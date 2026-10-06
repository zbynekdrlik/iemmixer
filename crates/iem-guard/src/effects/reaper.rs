//! REAPER's web control (S6 design note §5.2): its replies are
//! tab-separated lines that start with a verb (`NTRACK`, `TRACK`,
//! `EXTSTATE`). A port of the S1a spike's `ConvertFrom-SpikeReaperLine`,
//! plus the stage tracks' meters.

/// A `TRACK` line carries its meters only with at least this many fields,
/// the verb included; its 7th field is then the last meter peak in dB × 10
/// (REAPER's web-interface description, as the predecessor's poller reads
/// it). Without them the 7th field is something else.
pub const METER_FIELDS: usize = 14;

/// The URL of one web-control command (`NTRACK`, `TRACK`, an action id,
/// `GET/EXTSTATE/<section>/<key>`).
pub fn url(base: &str, command: &str) -> String {
    format!("{}/_/{command}", base.trim_end_matches('/'))
}

/// The command that reads one extended state value.
pub fn extstate_command(section_key: &str) -> String {
    format!("GET/EXTSTATE/{section_key}")
}

/// The fields after `verb` on the first line that starts with it; `None`
/// when no line does.
pub fn fields<'a>(text: &'a str, verb: &str) -> Option<Vec<&'a str>> {
    text.lines().find_map(|line| {
        let mut parts = line.trim_end_matches('\r').split('\t');
        if parts.next() == Some(verb) {
            Some(parts.collect())
        } else {
            None
        }
    })
}

/// `NTRACK`'s count.
pub fn ntrack(text: &str) -> Option<u32> {
    fields(text, "NTRACK")?.first()?.trim().parse().ok()
}

/// An extended state's value (`EXTSTATE`, section, key, value); empty when
/// the reply has none.
pub fn extstate(text: &str) -> String {
    fields(text, "EXTSTATE")
        .and_then(|f| f.get(2).map(|v| (*v).to_owned()))
        .unwrap_or_default()
}

/// Every `TRACK` line's last meter peak in dBFS, by track number.
pub fn track_peaks(text: &str) -> Vec<(u32, f64)> {
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
            if parts.len() < METER_FIELDS || parts.first() != Some(&"TRACK") {
                return None;
            }
            let track = parts.get(1)?.parse().ok()?;
            let db10: f64 = parts.get(6)?.parse().ok()?;
            Some((track, db10 / 10.0))
        })
        .collect()
}

/// The stage tracks' peaks from one `TRACK` reply, in the order of `stage`;
/// `None` for a track the reply has no meters for.
pub fn stage_peaks(text: &str, stage: &[u32]) -> Vec<Option<f64>> {
    let all = track_peaks(text);
    stage
        .iter()
        .map(|s| all.iter().find(|(t, _)| t == s).map(|(_, db)| *db))
        .collect()
}

/// Keeps the loudest reading of each stage track; start from
/// `f64::NEG_INFINITY` (no reading yet).
pub fn keep_max(loudest: &mut [f64], now: &[Option<f64>]) {
    for (l, n) in loudest.iter_mut().zip(now) {
        if let Some(v) = n {
            *l = l.max(*v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TRACK line with meters: number, name, flags, volume, pan, peak and
    /// position (dB × 10), then the rest.
    fn track(n: u32, peak_db10: i32) -> String {
        format!("TRACK\t{n}\tIn {n}\t64\t1.0\t0.0\t{peak_db10}\t{peak_db10}\t1.0\t0\t0\t0\t1\t0")
    }

    #[test]
    fn urls_join_the_base_and_the_command() {
        assert_eq!(
            url("http://127.0.0.1:8080", "NTRACK"),
            "http://127.0.0.1:8080/_/NTRACK"
        );
        assert_eq!(
            url("http://127.0.0.1:8080/", "40026"),
            "http://127.0.0.1:8080/_/40026"
        );
        assert_eq!(url("http://h//", "TRACK"), "http://h/_/TRACK");
        assert_eq!(
            extstate_command("bridge/state"),
            "GET/EXTSTATE/bridge/state"
        );
    }

    #[test]
    fn fields_follow_the_verb_of_the_first_matching_line() {
        assert_eq!(fields("NTRACK\t45\n", "NTRACK"), Some(vec!["45"]));
        assert_eq!(
            fields("TRACK\t1\n\r\nEXTSTATE\tsec\tkey\t1\r\n", "EXTSTATE"),
            Some(vec!["sec", "key", "1"])
        );
        assert_eq!(fields("", "NTRACK"), None);
        assert_eq!(fields("NTRACKS\t1\n", "NTRACK"), None);
        assert_eq!(fields("X\tNTRACK\t1", "NTRACK"), None);
        assert_eq!(fields("NTRACK", "NTRACK"), Some(vec![]));
        assert_eq!(fields("A\t1\nA\t2", "A"), Some(vec!["1"]));
        // A bare CR at the end of the last line is trimmed too.
        assert_eq!(fields("A\t1\r", "A"), Some(vec!["1"]));
    }

    #[test]
    fn ntrack_and_extstate_read_their_values() {
        assert_eq!(ntrack("NTRACK\t45\n"), Some(45));
        assert_eq!(ntrack("NTRACK\t 7 \r\n"), Some(7));
        assert_eq!(ntrack("NTRACK\t\n"), None);
        assert_eq!(ntrack("NTRACK\n"), None);
        assert_eq!(ntrack("NTRACK\tx\n"), None);
        assert_eq!(ntrack(""), None);
        assert_eq!(extstate("EXTSTATE\tbridge\tstate\t1\n"), "1");
        assert_eq!(extstate("EXTSTATE\tbridge\tstate\t\n"), "");
        assert_eq!(extstate("EXTSTATE\tbridge\tstate\n"), "");
        assert_eq!(extstate("NTRACK\t1\n"), "");
        assert_eq!(extstate("EXTSTATE\ts\tk\t12345\tmore\n"), "12345");
    }

    #[test]
    fn track_peaks_need_the_meter_fields() {
        let text = format!(
            "NTRACK\t3\n{}\n{}\nTRACK\t3\tIn 3\t64\t1.0\t0.0\t-200\t-200\t1.0\t0\t0\t0\n",
            track(1, -123),
            track(2, -1500)
        );
        assert_eq!(track_peaks(&text), [(1, -12.3), (2, -150.0)]);
        // Exactly the meter fields: 14 with the verb.
        let exact = track(4, -35);
        assert_eq!(exact.split('\t').count(), METER_FIELDS);
        assert_eq!(track_peaks(&exact), [(4, -3.5)]);
        let short: Vec<&str> = exact.split('\t').take(METER_FIELDS - 1).collect();
        assert!(track_peaks(&short.join("\t")).is_empty());
        // Another verb with as many fields, or unreadable numbers: nothing.
        assert!(track_peaks(&exact.replacen("TRACK", "SEND", 1)).is_empty());
        assert!(track_peaks(&exact.replacen("\t4\t", "\tx\t", 1)).is_empty());
        assert!(track_peaks(&exact.replacen("\t-35\t", "\tloud\t", 1)).is_empty());
        assert_eq!(track_peaks(&format!("{exact}\r\n")), [(4, -3.5)]);
    }

    #[test]
    fn stage_peaks_follow_the_stage_order() {
        let text = format!(
            "{}\n{}\n{}\n",
            track(1, -100),
            track(2, -200),
            track(3, -300)
        );
        assert_eq!(
            stage_peaks(&text, &[3, 1, 9]),
            [Some(-30.0), Some(-10.0), None]
        );
        assert!(stage_peaks(&text, &[]).is_empty());
    }

    #[test]
    fn the_loudest_reading_of_each_track_is_kept() {
        let mut loudest = vec![f64::NEG_INFINITY, -20.0, -30.0];
        keep_max(&mut loudest, &[Some(-40.0), Some(-25.0), None]);
        assert_eq!(loudest, [-40.0, -20.0, -30.0]);
        keep_max(&mut loudest, &[Some(-50.0), Some(-10.0), Some(-29.9)]);
        assert_eq!(loudest, [-40.0, -10.0, -29.9]);
        keep_max(&mut loudest, &[]);
        assert_eq!(loudest, [-40.0, -10.0, -29.9]);
    }
}
