//! The load chain (#32): which file holds the state (`current.json`, an
//! interrupted save in `save.tmp`, a generation, the baseline) and what the
//! seed must keep (`Store::live_state`).

use super::*;

impl Store {
    /// The live state the seed must keep, if any, named as the load chain
    /// would pick it: `current.json` (or `save.tmp` when it supersedes it),
    /// else `save.tmp`, else the newest generation. `save` writes the new
    /// state to `save.tmp` (synced), then renames `current.json` to a
    /// generation and `save.tmp` to `current.json`: a crash before the
    /// second rename leaves the newest state only in `save.tmp`, which the
    /// load chain then reads when it is complete, so `import
    /// --seed-if-absent` must neither seed over it nor touch it (iemmixer#9
    /// 2026-09-28, #32). Any of these files counts whenever it exists,
    /// complete or not: the seed never writes over what the engine may load.
    /// Ignores `baseline.json` (a seed is not live state) and `baseline.tmp`.
    /// An I/O error while looking is an error, never "no state" (#32 D5: the
    /// seed would write over state it could not see).
    pub fn live_state(&self) -> io::Result<Option<Source>> {
        let current = self.dir.join(CURRENT);
        let tmp = self.dir.join(TMP);
        Ok(match (current.try_exists()?, tmp.try_exists()?) {
            (true, true) if tmp_supersedes(&current, &tmp)? => Some(Source::Interrupted),
            (true, _) => Some(Source::Current),
            (false, true) => Some(Source::Interrupted),
            (false, false) => self
                .generations()?
                .last()
                .map(|&(seq, _)| Source::Generation(seq)),
        })
    }

    /// The load chain. A file must pass `decode` (format, schema, the
    /// payload's SHA-256, then the payload's parse) to be used; one that
    /// exists and does not is `rejected`. `save.tmp` holds a save a crash cut
    /// off before its renames (#32): it is used when `current.json` is
    /// missing, or when it is complete and strictly newer than a valid
    /// `current.json` by revision ([`supersedes`]); a `save.tmp` it does not
    /// use beside a valid `current.json` is `rejected` with the reason.
    pub fn load(&self, topo: &Topology) -> Loaded {
        let mut rejected = Vec::new();
        let tmp = self.dir.join(TMP);
        let first = match read_state(&self.dir.join(CURRENT), &mut rejected) {
            Read::Found(current) => Some(match read_state(&tmp, &mut rejected) {
                Read::Found(newer) if supersedes(&newer, &current) => (newer, Source::Interrupted),
                Read::Found(older) => {
                    rejected.push((
                        tmp,
                        format!(
                            "revision {} is not newer than current.json's {}",
                            older.rev, current.rev
                        ),
                    ));
                    (current, Source::Current)
                }
                Read::Missing | Read::Rejected => (current, Source::Current),
            }),
            Read::Missing => match read_state(&tmp, &mut rejected) {
                Read::Found(interrupted) => Some((interrupted, Source::Interrupted)),
                Read::Missing | Read::Rejected => None,
            },
            // A damaged current.json gives save.tmp no revision to beat.
            Read::Rejected => None,
        };
        if let Some((persisted, source)) = first {
            return settle(topo, persisted, source, rejected);
        }
        let mut rest: Vec<(PathBuf, Source)> = self
            .generations()
            .unwrap_or_default()
            .into_iter()
            .rev()
            .map(|(seq, path)| (path, Source::Generation(seq)))
            .collect();
        rest.push((self.dir.join(BASELINE), Source::Baseline));
        for (path, source) in rest {
            if let Read::Found(persisted) = read_state(&path, &mut rejected) {
                return settle(topo, persisted, source, rejected);
            }
        }
        Loaded {
            persisted: Persisted {
                topology_hash: topo.hash.clone(),
                state: defaults_muted(topo),
                ..Persisted::default()
            },
            source: Source::Defaults,
            rejected,
            dropped: Vec::new(),
        }
    }
}

/// One state file, read and decoded.
enum Read {
    Missing,
    /// Present but unreadable or not a valid state file (in `rejected`).
    Rejected,
    Found(Persisted),
}

/// Reads and decodes `path`; an unreadable or invalid file goes to
/// `rejected` with the reason.
fn read_state(path: &Path, rejected: &mut Vec<(PathBuf, String)>) -> Read {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Read::Missing,
        Err(e) => {
            rejected.push((path.to_path_buf(), e.to_string()));
            return Read::Rejected;
        }
    };
    match decode(&bytes) {
        Ok(persisted) => Read::Found(persisted),
        Err(why) => {
            rejected.push((path.to_path_buf(), why));
            Read::Rejected
        }
    }
}

/// Whether an interrupted save (`save.tmp`) supersedes `current.json`: only
/// when its revision is strictly higher. `rev` is the core's own monotonic
/// revision (one per changing request, carried across restarts), so a
/// repeated save of the same state, an import's fresh count (0) and a
/// leftover baseline of an older engine never roll the saved state back.
fn supersedes(interrupted: &Persisted, current: &Persisted) -> bool {
    interrupted.rev > current.rev
}

/// [`supersedes`] on the files, for `Store::live_state`: an unreadable file
/// is an error, an invalid one supersedes nothing and is superseded by none.
fn tmp_supersedes(current: &Path, tmp: &Path) -> io::Result<bool> {
    let (Ok(current), Ok(tmp)) = (decode(&fs::read(current)?), decode(&fs::read(tmp)?)) else {
        return Ok(false);
    };
    Ok(supersedes(&tmp, &current))
}

/// A loaded state, reconciled against the topology.
fn settle(
    topo: &Topology,
    mut persisted: Persisted,
    source: Source,
    rejected: Vec<(PathBuf, String)>,
) -> Loaded {
    let (r, dropped) = reconcile(topo, &persisted.state);
    persisted.state = to_state(topo, &r);
    Loaded {
        persisted,
        source,
        rejected,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::tests::{sample, store};
    use crate::test_support::test_site;
    use iem_engine_proto::{InputId, InputState};

    fn corrupt(path: &Path) {
        let mut text = fs::read_to_string(path).unwrap();
        let at = text.find("\"rev\":").unwrap() + 6;
        text.insert(at, '9');
        fs::write(path, text).unwrap();
    }

    #[test]
    fn live_state_sees_current_and_generations_independently() {
        let (_d, s) = store();
        // A fresh store holds no live state.
        assert_eq!(s.live_state().unwrap(), None);
        // `current.json` alone is live state.
        std::fs::write(s.dir().join(CURRENT), b"{}").unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Current));
        // A generation ALONE, with no `current.json`, is also live state: a
        // crash between `save`'s two renames leaves the previous state only as
        // a generation (the newest waits in save.tmp: #32 D6).
        std::fs::remove_file(s.dir().join(CURRENT)).unwrap();
        std::fs::write(s.dir().join("gen-0000000001.json"), b"{}").unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Generation(1)));
    }

    #[test]
    fn an_interrupted_save_is_live_state() {
        // #32 D6: `save` writes the new state to save.tmp before its renames,
        // so a crash between them leaves the newest state only there.
        let (_d, s) = store();
        fs::write(s.dir().join(TMP), b"NEWEST").unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
    }

    #[test]
    fn live_state_follows_the_load_chains_order() {
        // current.json, then save.tmp, then the newest generation.
        let (_d, s) = store();
        fs::write(s.dir().join("gen-0000000002.json"), b"{}").unwrap();
        fs::write(s.dir().join("gen-0000000003.json"), b"{}").unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Generation(3)));
        fs::write(s.dir().join(TMP), b"NEWEST").unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
        fs::write(s.dir().join(CURRENT), b"{}").unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Current));
    }

    #[test]
    fn live_state_fails_when_the_state_dir_cannot_be_read() {
        // #32 D5: an I/O error is not "no state" — the seed must never write
        // over state it could not look at.
        let (_d, s) = store();
        fs::remove_dir(s.dir()).unwrap();
        fs::write(s.dir(), b"not a directory").unwrap();
        assert!(s.live_state().is_err());
    }

    #[test]
    fn a_bad_checksum_falls_back_to_the_newest_good_generation() {
        let (_d, s) = store();
        for rev in 1..=3 {
            s.save(&sample(rev)).unwrap();
        }
        corrupt(&s.dir().join(CURRENT));
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Generation(2));
        assert_eq!(loaded.persisted.rev, 2);
        assert_eq!(loaded.rejected.len(), 1);
        assert!(loaded.rejected[0].0.ends_with(CURRENT));
        assert_eq!(loaded.rejected[0].1, "checksum mismatch");
    }

    #[test]
    fn truncated_or_foreign_files_are_skipped() {
        let (_d, s) = store();
        for rev in 1..=3 {
            s.save(&sample(rev)).unwrap();
        }
        let current = s.dir().join(CURRENT);
        let bytes = fs::read(&current).unwrap();
        fs::write(&current, &bytes[..bytes.len() / 2]).unwrap();
        let gens = s.generations().unwrap();
        let newest = &gens.last().unwrap().1;
        let text = fs::read_to_string(newest)
            .unwrap()
            .replace("iemmixer-state", "not-ours");
        fs::write(newest, text).unwrap();
        // A stray temp file never counts.
        fs::write(s.dir().join(TMP), b"garbage").unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Generation(1));
        assert_eq!(loaded.persisted.rev, 1);
        assert_eq!(loaded.rejected.len(), 2);
    }

    #[test]
    fn a_crash_between_the_renames_boots_on_the_interrupted_save() {
        // #32: `save` writes save.tmp, renames current.json to a generation,
        // then save.tmp to current.json. A crash between the renames leaves
        // the newest state only in save.tmp: the load chain must take it, not
        // the older generation (whose boot would lose it at the next save).
        let (_d, s) = store();
        let g = test_site();
        s.save(&sample(1)).unwrap();
        fs::write(s.dir().join(TMP), encode(&sample(2)).unwrap()).unwrap();
        fs::rename(s.dir().join(CURRENT), s.dir().join("gen-0000000001.json")).unwrap();
        let loaded = s.load(&g);
        assert_eq!(loaded.persisted.rev, 2);
        assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
        // A save cut off while writing save.tmp is no state: the generation
        // stands, and the cut-off file is reported.
        let partial = encode(&sample(3)).unwrap();
        fs::write(s.dir().join(TMP), &partial[..partial.len() / 2]).unwrap();
        let loaded = s.load(&g);
        assert_eq!(loaded.source, Source::Generation(1));
        assert_eq!(loaded.persisted.rev, 1);
        assert_eq!(loaded.rejected.len(), 1, "{:?}", loaded.rejected);
        assert!(loaded.rejected[0].0.ends_with(TMP));
    }

    #[test]
    fn a_boot_on_save_tmp_is_named_interrupted() {
        // The first save, cut off before its rename: save.tmp is the only state.
        let (_d, s) = store();
        fs::write(s.dir().join(TMP), encode(&sample(4)).unwrap()).unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Interrupted);
        assert_eq!(loaded.persisted.rev, 4);
    }

    #[test]
    fn a_newer_save_tmp_beside_current_json_wins() {
        // #32: a crash after save.tmp was written but before current.json
        // became a generation. save.tmp (revision N + 1) is the newest state.
        let (_d, s) = store();
        s.save(&sample(5)).unwrap();
        fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Interrupted);
        assert_eq!(loaded.persisted.rev, 6);
        assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
    }

    #[test]
    fn a_save_tmp_not_newer_than_current_json_is_skipped_and_named() {
        // Equal or older by revision (a repeated save, a leftover baseline of
        // an older engine) or cut off while writing: current.json stands, and
        // the skipped save.tmp is reported.
        let (_d, s) = store();
        s.save(&sample(5)).unwrap();
        let cut = encode(&sample(6)).unwrap();
        for (tmp, why) in [
            (
                encode(&sample(5)).unwrap(),
                "revision 5 is not newer than current.json's 5",
            ),
            (
                encode(&sample(4)).unwrap(),
                "revision 4 is not newer than current.json's 5",
            ),
            (cut[..cut.len() / 2].to_vec(), "not a state file"),
        ] {
            fs::write(s.dir().join(TMP), &tmp).unwrap();
            let loaded = s.load(&test_site());
            assert_eq!(loaded.source, Source::Current, "{why}");
            assert_eq!(loaded.persisted.rev, 5, "{why}");
            assert_eq!(loaded.rejected.len(), 1, "{why}: {:?}", loaded.rejected);
            assert!(loaded.rejected[0].0.ends_with(TMP));
            assert!(
                loaded.rejected[0].1.starts_with(why),
                "{:?}",
                loaded.rejected
            );
        }
    }

    #[test]
    fn live_state_names_the_file_the_engine_would_load() {
        // Beside current.json, save.tmp is the live state only when it is a
        // complete state strictly newer by revision, as in the load chain.
        let (_d, s) = store();
        s.save(&sample(5)).unwrap();
        fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
        fs::write(s.dir().join(TMP), encode(&sample(5)).unwrap()).unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Current));
        fs::write(s.dir().join(TMP), b"cut off").unwrap();
        assert_eq!(s.live_state().unwrap(), Some(Source::Current));
    }

    #[test]
    fn an_older_save_tmp_beside_current_json_is_not_used() {
        let (_d, s) = store();
        s.save(&sample(2)).unwrap();
        fs::write(s.dir().join(TMP), encode(&sample(1)).unwrap()).unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Current);
        assert_eq!(loaded.persisted.rev, 2);
    }

    #[test]
    fn baseline_is_the_last_resort_before_defaults() {
        let (_d, s) = store();
        s.save_baseline(&sample(9)).unwrap();
        fs::write(s.dir().join(CURRENT), b"{}").unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Baseline);
        assert_eq!(loaded.persisted.rev, 9);
        assert_eq!(loaded.rejected.len(), 1);
    }

    #[test]
    fn an_unreadable_file_is_rejected_not_skipped_as_missing() {
        let (_d, s) = store();
        s.save_baseline(&sample(4)).unwrap();
        // A directory where current.json belongs: reading it fails, not NotFound.
        fs::create_dir(s.dir().join(CURRENT)).unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Baseline);
        assert_eq!(loaded.persisted.rev, 4);
        assert_eq!(loaded.rejected.len(), 1);
        assert!(loaded.rejected[0].0.ends_with(CURRENT));
    }

    #[test]
    fn nothing_loadable_gives_muted_defaults() {
        let (_d, s) = store();
        let g = test_site();
        let loaded = s.load(&g);
        assert_eq!(loaded.source, Source::Defaults);
        assert!(loaded.rejected.is_empty());
        assert_eq!(loaded.persisted.rev, 0);
        assert_eq!(loaded.persisted.topology_hash, g.hash);
        for n in &g.mixes {
            assert!(loaded.persisted.state.mixes[&n.id].out.muted, "{}", n.id);
        }
    }

    #[test]
    fn newer_fields_are_ignored_and_missing_default() {
        let (_d, s) = store();
        let payload = r#"{"rev":7,"state":{"inputs":{"mic1":{"trim_db":-2.0,"colour":"red"}}},"future":{"x":1}}"#;
        let file = format!(
            r#"{{"format":"iemmixer-state","schema":9,"sha256":"{}","payload":{payload},"later":true}}"#,
            digest(payload)
        );
        fs::write(s.dir().join(CURRENT), file).unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Current);
        assert_eq!(loaded.persisted.rev, 7);
        assert!(loaded.persisted.counters.is_empty());
        let mic1 = loaded.persisted.state.inputs[&InputId::new("mic1")];
        assert_eq!(mic1.trim_db, -2.0);
        assert!(mic1.processing);
        assert_eq!(loaded.persisted.state.mixes.len(), 11);
    }

    #[test]
    fn unknown_ids_are_dropped_on_load() {
        let (_d, s) = store();
        let mut p = sample(2);
        p.state
            .inputs
            .insert(InputId::new("ghost"), InputState::default());
        s.save(&p).unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.dropped, vec!["input ghost"]);
        assert!(
            !loaded
                .persisted
                .state
                .inputs
                .contains_key(&InputId::new("ghost"))
        );
        assert_eq!(loaded.persisted.state.inputs.len(), 24);
    }
}
