//! Writes a golden bundle: projects, stimuli and a hash manifest.

use std::fs;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::cases::{CaseMeta, Catalogue};
use crate::fx::ALLOWED_FX_HEADS;
use crate::wav::float_wav;

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("output folder is not empty: {0}")]
    NotEmpty(String),
    #[error("plug-in not on the allowlist: {0}")]
    Plugin(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Rpp(#[from] crate::rpp::RppError),
    #[error(transparent)]
    Wav(#[from] crate::wav::WavError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ProjectEntry {
    pub file: String,
    pub id: String,
    pub rate: u32,
    pub bits: u16,
    pub tracks: Vec<CaseMeta>,
}

#[derive(Debug, Serialize)]
pub struct StimulusEntry {
    pub file: String,
    pub rate: u32,
    pub channels: usize,
    pub frames: usize,
}

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub schema: u32,
    pub generator: String,
    pub projects: Vec<ProjectEntry>,
    pub stimuli: Vec<StimulusEntry>,
    pub files: Vec<FileEntry>,
}

const FX_WORDS: [&str; 9] = [
    "VST",
    "VST3",
    "JS",
    "CLAP",
    "AU",
    "AUi",
    "DX",
    "LV2",
    "VIDEO_EFFECT",
];

/// Every plug-in chunk must be one of the allowed heads (P5).
pub fn check_allowlist(rpp: &str) -> Result<(), BundleError> {
    for line in rpp.lines() {
        let t = line.trim();
        let Some(head) = t.strip_prefix('<') else {
            continue;
        };
        let word = head.split_whitespace().next().unwrap_or_default();
        if FX_WORDS.contains(&word) && !ALLOWED_FX_HEADS.contains(&head) {
            return Err(BundleError::Plugin(t.to_owned()));
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write(root: &Path, rel: &str, bytes: &[u8]) -> Result<FileEntry, BundleError> {
    fs::write(root.join(rel), bytes)?;
    Ok(FileEntry {
        path: rel.to_owned(),
        sha256: hex(&Sha256::digest(bytes)),
        bytes: bytes.len() as u64,
    })
}

pub fn write_bundle(cat: &Catalogue, out: &Path) -> Result<Manifest, BundleError> {
    if out.exists() && fs::read_dir(out)?.next().is_some() {
        return Err(BundleError::NotEmpty(out.display().to_string()));
    }
    fs::create_dir_all(out.join("projects"))?;
    fs::create_dir_all(out.join("stimuli"))?;
    let mut files = Vec::new();
    let mut stimuli = Vec::new();
    for s in &cat.stimuli {
        files.push(write(
            out,
            &format!("stimuli/{}", s.file),
            &float_wav(s.rate, 64, &s.channels)?,
        )?);
        stimuli.push(StimulusEntry {
            file: s.file.clone(),
            rate: s.rate,
            channels: s.channels.len(),
            frames: s.channels.first().map_or(0, Vec::len),
        });
    }
    let mut projects = Vec::new();
    for case in &cat.projects {
        let text = case.project.to_rpp()?;
        check_allowlist(&text)?;
        let rel = format!("projects/{}.rpp", case.project.id);
        files.push(write(out, &rel, text.as_bytes())?);
        projects.push(ProjectEntry {
            file: rel,
            id: case.project.id.clone(),
            rate: case.project.rate,
            bits: case.project.format.bits(),
            tracks: case.meta.clone(),
        });
    }
    let manifest = Manifest {
        schema: 1,
        generator: format!("iem-rpp {}", env!("CARGO_PKG_VERSION")),
        projects,
        stimuli,
        files,
    };
    fs::write(
        out.join("bundle.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::catalogue;

    #[test]
    fn rejects_a_foreign_plugin_and_accepts_the_allowed_ones() {
        assert!(
            check_allowlist("  <VST \"VST3: Other (Vendor)\" other.vst3 0 \"\" 1{x} \"\"\n")
                .is_err()
        );
        assert!(check_allowlist("<JS utility/tonegenerator \"\"\n").is_err());
        assert!(check_allowlist("  <JS utility/volume_pan \"\"\n  <SOURCE WAVE\n  AUXRECV 0 3 1 0 0 0 0 0 0 -1:U 0 -1 ''\n").is_ok());
    }

    #[test]
    fn bundles_are_byte_identical_across_runs_and_hashes_match_the_files() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let cat = catalogue(&["cal".into(), "sum".into()]).unwrap();
        let ma = write_bundle(&cat, a.path()).unwrap();
        write_bundle(&cat, b.path()).unwrap();
        assert_eq!(
            fs::read(a.path().join("bundle.json")).unwrap(),
            fs::read(b.path().join("bundle.json")).unwrap()
        );
        for f in &ma.files {
            let bytes = fs::read(a.path().join(&f.path)).unwrap();
            assert_eq!(hex(&Sha256::digest(&bytes)), f.sha256);
            assert_eq!(bytes, fs::read(b.path().join(&f.path)).unwrap());
        }
    }

    #[test]
    fn refuses_a_used_folder() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("x"), b"x").unwrap();
        assert!(matches!(
            write_bundle(&catalogue(&["cal".into()]).unwrap(), d.path()),
            Err(BundleError::NotEmpty(_))
        ));
    }
}
