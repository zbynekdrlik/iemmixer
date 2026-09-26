//! Reader for the committed S1b goldens (`goldens/s1b`, S2 design note §5):
//! `index.json` maps file → case → `{offset, shape, …metadata}`, where the
//! offset counts f64 values inside `<file>.f64` (float64 little-endian), and
//! `laws.json` holds the measured laws. The S2 DSP and limiter tests read it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GoldenError {
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {source}", .path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("golden {0}")]
    Invalid(String),
}

/// One golden vector with its `index.json` entry (`params`, `eq`, …).
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub name: String,
    pub shape: Vec<usize>,
    pub meta: Value,
    pub data: Vec<f64>,
}

#[derive(Debug)]
pub struct Goldens {
    dir: PathBuf,
    index: Value,
    laws: Value,
}

/// `goldens/s1b` of this checkout.
pub fn s1b_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../goldens/s1b")
}

fn read(path: &Path) -> Result<Vec<u8>, GoldenError> {
    fs::read(path).map_err(|source| GoldenError::Io {
        path: path.to_owned(),
        source,
    })
}

fn json(path: &Path) -> Result<Value, GoldenError> {
    serde_json::from_slice(&read(path)?).map_err(|source| GoldenError::Json {
        path: path.to_owned(),
        source,
    })
}

fn invalid(file: &str, what: &str) -> GoldenError {
    GoldenError::Invalid(format!("{file}: {what}"))
}

impl Goldens {
    pub fn open(dir: &Path) -> Result<Self, GoldenError> {
        Ok(Self {
            dir: dir.to_owned(),
            index: json(&dir.join("index.json"))?,
            laws: json(&dir.join("laws.json"))?,
        })
    }

    /// Vector file names in `index.json` (e.g. `eq-96000`).
    pub fn files(&self) -> Vec<String> {
        self.index
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Every case stored in `<file>.f64`.
    pub fn cases(&self, file: &str) -> Result<Vec<Case>, GoldenError> {
        let entries = self
            .index
            .get(file)
            .and_then(Value::as_object)
            .ok_or_else(|| invalid(file, "not in index.json"))?;
        let raw = read(&self.dir.join(format!("{file}.f64")))?;
        if raw.len() % 8 != 0 {
            return Err(invalid(file, "length is not a whole number of f64"));
        }
        let values: Vec<f64> = raw
            .chunks_exact(8)
            .map(|b| {
                let mut a = [0u8; 8];
                a.copy_from_slice(b);
                f64::from_le_bytes(a)
            })
            .collect();
        entries
            .iter()
            .map(|(name, meta)| {
                let bad = |what: &str| invalid(file, &format!("{name}: {what}"));
                let start = meta
                    .get("offset")
                    .and_then(Value::as_u64)
                    .and_then(|o| usize::try_from(o).ok())
                    .ok_or_else(|| bad("no offset"))?;
                let shape = meta
                    .get("shape")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_u64)
                            .filter_map(|n| usize::try_from(n).ok())
                            .collect::<Vec<_>>()
                    })
                    .ok_or_else(|| bad("no shape"))?;
                let len: usize = shape.iter().product();
                let data = start
                    .checked_add(len)
                    .and_then(|end| values.get(start..end))
                    .ok_or_else(|| bad("runs past the end of the file"))?
                    .to_vec();
                Ok(Case {
                    name: name.clone(),
                    shape,
                    meta: meta.clone(),
                    data,
                })
            })
            .collect()
    }

    /// A measured law (`laws.<name>` in `laws.json`).
    pub fn law(&self, name: &str) -> Option<&Value> {
        self.laws.get("laws")?.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(index: &str, values: &[f64]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("index.json"), index).unwrap();
        fs::write(
            d.path().join("laws.json"),
            r#"{"laws": {"x": {"verdict": "ok"}}}"#,
        )
        .unwrap();
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        fs::write(d.path().join("v.f64"), bytes).unwrap();
        d
    }

    #[test]
    fn reads_every_case_at_its_offset() {
        let d = tree(
            r#"{"v": {"a": {"offset": 1, "shape": [2], "params": {"k": 3}}, "b": {"offset": 3, "shape": [1, 2]}}}"#,
            &[9.0, 1.5, -2.0, 4.0, 0.25],
        );
        let g = Goldens::open(d.path()).unwrap();
        assert_eq!(g.files(), vec!["v".to_owned()]);
        let cases = g.cases("v").unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "a");
        assert_eq!(cases[0].data, vec![1.5, -2.0]);
        assert_eq!(cases[0].shape, vec![2]);
        assert_eq!(cases[0].meta["params"]["k"], 3);
        assert_eq!(cases[1].data, vec![4.0, 0.25]);
        assert_eq!(cases[1].shape, vec![1, 2]);
        assert_eq!(g.law("x").unwrap()["verdict"], "ok");
        assert!(g.law("y").is_none());
    }

    #[test]
    fn refuses_short_files_bad_lengths_and_unknown_names() {
        let d = tree(
            r#"{"v": {"a": {"offset": 2, "shape": [2]}}}"#,
            &[1.0, 2.0, 3.0],
        );
        let g = Goldens::open(d.path()).unwrap();
        assert!(matches!(g.cases("v"), Err(GoldenError::Invalid(_))));
        assert!(matches!(g.cases("w"), Err(GoldenError::Invalid(_))));
        fs::write(d.path().join("v.f64"), [0u8; 7]).unwrap();
        assert!(matches!(g.cases("v"), Err(GoldenError::Invalid(_))));
        let exact = tree(
            r#"{"v": {"a": {"offset": 2, "shape": [2]}}}"#,
            &[1.0, 2.0, 3.0, 4.0],
        );
        let g = Goldens::open(exact.path()).unwrap();
        assert_eq!(g.cases("v").unwrap()[0].data, vec![3.0, 4.0]);
        let e = tempfile::tempdir().unwrap();
        assert!(matches!(
            Goldens::open(e.path()),
            Err(GoldenError::Io { .. })
        ));
        fs::write(e.path().join("index.json"), "{").unwrap();
        assert!(matches!(
            Goldens::open(e.path()),
            Err(GoldenError::Json { .. })
        ));
    }

    #[test]
    fn committed_s1b_vectors_are_complete_and_non_blank() {
        let g = Goldens::open(&s1b_dir()).unwrap();
        let files = g.files();
        assert_eq!(
            files,
            [
                "eq-44100",
                "eq-48000",
                "eq-96000",
                "lim-44100",
                "lim-48000",
                "lim-96000",
                "site-eq-96000"
            ]
        );
        let mut total = 0;
        for f in &files {
            for c in g.cases(f).unwrap() {
                // S1b wrote all but the last vector of each file as zeros (fixed in S2).
                assert!(c.data.iter().any(|x| *x != 0.0), "{f}/{} is blank", c.name);
                total += 1;
            }
        }
        assert_eq!(total, 426 + 426 + 427 + 1 + 1 + 3 + 15);
        let eq = g.cases("eq-96000").unwrap();
        let peak = eq.iter().find(|c| c.name == "cal64-peak").unwrap();
        assert_eq!(peak.data.len(), 256);
        assert_eq!(peak.data[0], 1.016041922628664);
        let lim = g.cases("lim-96000").unwrap();
        assert!(lim.iter().all(|c| c.shape == [96_000, 2]));
        assert_eq!(
            g.law("pan_law").unwrap()["detail"]
                .as_array()
                .unwrap()
                .len(),
            43
        );
    }
}
