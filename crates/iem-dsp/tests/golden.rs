//! S1b golden parity (program spec §3.5): every ReaEQ vector (matrix, edges,
//! site EQs) ≤ 1e-9, the response ≤ 0.01 dB over 20 Hz–20 kHz, the measured pan
//! table and the pan-dependent renders, and the mono downmix law. Each test
//! prints `golden <category>: max …` lines for the CI parity report.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use iem_dsp::eq::{Band, BandKind, EqParams, Equalizer, response_db};
use iem_dsp::pan::{gains, mono_downmix, send_gains};
use iem_rpp::golden::{Goldens, s1b_dir};
use serde_json::Value;

fn goldens() -> Goldens {
    Goldens::open(&s1b_dir()).unwrap()
}

fn band(v: &Value) -> Band {
    let kind = match v["kind"].as_str().unwrap() {
        "high_pass" => BandKind::HighPass,
        "low_shelf" => BandKind::LowShelf,
        "band" => BandKind::Peak,
        "high_shelf" => BandKind::HighShelf,
        other => panic!("unknown band kind {other}"),
    };
    Band {
        kind,
        enabled: v["enabled"].as_bool().unwrap(),
        freq_hz: v["freq_hz"].as_f64().unwrap(),
        gain_lin: v["gain_lin"].as_f64().unwrap(),
        bw_oct: v["bw_oct"].as_f64().unwrap(),
    }
}

/// The EQ a golden case rendered: one band in its standard slot (the matrix)
/// or the whole five-band chunk (edges, site EQs).
fn eq_of(meta: &Value) -> (EqParams, &'static str) {
    if let Some(b) = meta["params"].get("band") {
        let b = band(b);
        let mut p = EqParams::standard_flat();
        let (slot, name) = match b.kind {
            BandKind::HighPass => (0, "high_pass"),
            BandKind::LowShelf => (1, "low_shelf"),
            BandKind::Peak => (2, "peak"),
            BandKind::HighShelf => (4, "high_shelf"),
        };
        p.bands[slot] = b;
        return (p, name);
    }
    let (eq, name) = match meta["params"].get("eq") {
        Some(eq) => (eq, "edge"),
        None => (&meta["eq"], "site"),
    };
    let bands: Vec<Band> = eq["bands"].as_array().unwrap().iter().map(band).collect();
    let p = EqParams {
        bands: bands.try_into().unwrap(),
        global_gain: eq["global_gain"].as_f64().unwrap(),
    };
    (p, name)
}

fn rate(file: &str) -> f64 {
    file.rsplit('-').next().unwrap().parse().unwrap()
}

fn impulse_response(p: &EqParams, fs: f64, n: usize) -> Vec<f64> {
    let mut x = vec![0.0; n];
    x[0] = 1.0;
    Equalizer::<1>::new(p, fs).process([x.as_mut_slice()]);
    x
}

const EQ_FILES: [&str; 4] = ["eq-44100", "eq-48000", "eq-96000", "site-eq-96000"];

#[test]
fn every_eq_golden_matches_within_1e_9() {
    let g = goldens();
    let mut worst: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for file in EQ_FILES {
        let fs = rate(file);
        for c in g.cases(file).unwrap() {
            let (p, name) = eq_of(&c.meta);
            let y = impulse_response(&p, fs, c.data.len());
            let err = y
                .iter()
                .zip(&c.data)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            assert!(err <= 1e-9, "{file}/{}: {err:e}", c.name);
            let w = worst.entry(format!("eq {file} {name}")).or_insert((0.0, 0));
            *w = (w.0.max(err), w.1 + 1);
        }
    }
    assert_eq!(
        worst.values().map(|w| w.1).sum::<usize>(),
        426 + 426 + 427 + 15
    );
    for (cat, (err, n)) in worst {
        println!("golden {cat}: max {err:.3e} over {n} cases (≤ 1e-9)");
    }
}

fn dtft_db(h: &[f64], fs: f64, f: f64) -> f64 {
    let w = 2.0 * PI * f / fs;
    let (re, im) = h.iter().enumerate().fold((0.0, 0.0), |(re, im), (n, x)| {
        let ph = w * n as f64;
        (re + x * ph.cos(), im - x * ph.sin())
    });
    10.0 * (re * re + im * im).log10()
}

fn log_freqs() -> Vec<f64> {
    (0..=30)
        .map(|i| 20.0 * 1000f64.powf(f64::from(i) / 30.0))
        .collect()
}

#[test]
fn eq_response_is_within_0_01_db_from_20_hz_to_20_khz() {
    // REAPER's own response: the DTFT of every golden impulse response that has
    // decayed inside its 256 taps (tail ≤ 1e-9 of the peak), against response_db.
    let g = goldens();
    let (mut worst, mut cases) = (0.0f64, 0);
    for file in ["eq-44100", "eq-48000", "eq-96000"] {
        let fs = rate(file);
        for c in g.cases(file).unwrap() {
            let peak = c.data.iter().fold(0.0f64, |m, x| m.max(x.abs()));
            let tail = c.data[224..].iter().fold(0.0f64, |m, x| m.max(x.abs()));
            if tail > 1e-9 * peak {
                continue;
            }
            cases += 1;
            let (p, _) = eq_of(&c.meta);
            for f in log_freqs().into_iter().filter(|f| *f < 0.49 * fs) {
                let err = (dtft_db(&c.data, fs, f) - response_db(&p, fs, f)).abs();
                assert!(err <= 0.01, "{file}/{} at {f} Hz: {err} dB", c.name);
                worst = worst.max(err);
            }
        }
    }
    assert!(cases > 400, "only {cases} decayed cases");
    println!(
        "golden eq response (REAPER IR DTFT): max {worst:.3e} dB over {cases} cases (≤ 0.01 dB)"
    );
    // The site EQs: response_db against a long impulse response of the port.
    let mut site = 0.0f64;
    for c in g.cases("site-eq-96000").unwrap() {
        let (p, _) = eq_of(&c.meta);
        let h = impulse_response(&p, 96_000.0, 1 << 16);
        for f in log_freqs() {
            site = site.max((dtft_db(&h, 96_000.0, f) - response_db(&p, 96_000.0, f)).abs());
        }
    }
    assert!(site < 1e-6, "{site}");
    println!("golden eq response (site EQs, long IR): max {site:.3e} dB (≤ 0.01 dB)");
}

fn pan_rows(g: &Goldens) -> Vec<(f64, f64, f64)> {
    g.law("pan_law").unwrap()["detail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["pan"].as_f64().unwrap(),
                r["gain_l"].as_f64().unwrap(),
                r["gain_r"].as_f64().unwrap(),
            )
        })
        .collect()
}

fn on_grid(p: f64) -> bool {
    ((p * 20.0).round() - p * 20.0).abs() < 1e-9
}

/// 0.04 and 0.86 lie between table nodes: interpolation error, ≤ 1e-5.
fn tolerance(p: f64) -> f64 {
    if on_grid(p) { 1e-15 } else { 1e-5 }
}

#[test]
fn pan_law_matches_the_measured_table() {
    let rows = pan_rows(&goldens());
    assert_eq!(rows.len(), 43);
    let (mut grid, mut off) = (0.0f64, 0.0f64);
    for (p, gl, gr) in rows {
        let (l, r) = gains(p);
        let err = (l - gl).abs().max((r - gr).abs());
        assert!(err <= tolerance(p), "p = {p}: {err:e}");
        let db = (20.0 * (l.hypot(r) / gl.hypot(gr)).log10()).abs();
        assert!(db < 1e-4, "p = {p}: {db} dB");
        if on_grid(p) {
            grid = grid.max(err);
        } else {
            off = off.max(err);
        }
    }
    println!("golden pan table: max {grid:.3e} on the 0.05 grid, {off:.3e} between nodes");
}

fn measured(row: &Value) -> Vec<(f64, f64)> {
    row["measured"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t[0].as_f64().unwrap(), t[1].as_f64().unwrap()))
        .collect()
}

#[test]
fn pan_dependent_renders_match_the_law() {
    // Taps of the S1b renders: stimuli are 0.5 impulses; a stereo source has L
    // at k0 and R at k2, a dual-mono or mono source both at k0.
    let g = goldens();
    let mut worst = 0.0f64;
    let mut n = 0;
    for law in [
        "send_pan",
        "track_pan",
        "mono_media",
        "post_fader",
        "send_vol",
    ] {
        for row in g.law(law).unwrap()["detail"].as_array().unwrap() {
            let p = &row["params"];
            let pan = p["pan"].as_f64().unwrap_or(0.0);
            let want = match law {
                "post_fader" => {
                    let (tl, tr) = gains(p["track_pan"].as_f64().unwrap());
                    let (sl, sr) = gains(pan);
                    let v = 0.5 * p["track_vol"].as_f64().unwrap();
                    vec![(v * tl * sl, 0.0), (0.0, v * tr * sr)]
                }
                "send_vol" => {
                    let (l, r) = send_gains(p["vol"].as_f64().unwrap(), false, 0.0);
                    vec![(0.5 * l, 0.5 * r)]
                }
                _ => {
                    let (l, r) = gains(pan);
                    if p["source"] == "st" {
                        vec![(0.5 * l, 0.0), (0.0, 0.5 * r)]
                    } else {
                        vec![(0.5 * l, 0.5 * r)]
                    }
                }
            };
            let got = measured(row);
            assert_eq!(got.len(), want.len(), "{law} {p}");
            for ((gl, gr), (wl, wr)) in got.iter().zip(&want) {
                let err = (gl - wl).abs().max((gr - wr).abs());
                let tol = if on_grid(pan) { 1e-15 } else { 1e-5 };
                assert!(err <= tol, "{law} {p}: {err:e}");
                if on_grid(pan) {
                    worst = worst.max(err);
                }
            }
            n += 1;
        }
    }
    assert_eq!(n, 88 + 44 + 3 + 6 + 6);
    println!(
        "golden pan renders (send, track, mono media, post-fader, send volume): max {worst:.3e} on the grid over {n} cases"
    );
}

#[test]
fn mono_downmix_matches_the_measured_law() {
    let g = goldens();
    let rows = g.law("mono_downmix").unwrap()["detail"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    let mut worst = 0.0f64;
    for row in rows {
        let p = &row["params"];
        let (gl, gr) = send_gains(
            p["vol"].as_f64().unwrap(),
            false,
            p["pan"].as_f64().unwrap(),
        );
        // Stereo source: L impulse at k0, R impulse at k2; dual mono: both at k0.
        let (k0, k2) = if p["source"] == "st" {
            (
                mono_downmix(0.5, 0.0, gl, gr),
                mono_downmix(0.0, 0.5, gl, gr),
            )
        } else {
            (mono_downmix(0.5, 0.5, gl, gr), 0.0)
        };
        for (tap, want) in [("L_at_k0", k0), ("R_at_k2", k2)] {
            let got = &row[tap];
            let err = (got[0].as_f64().unwrap() - want)
                .abs()
                .max(got[1].as_f64().unwrap().abs());
            assert!(err <= 1e-15, "{p} {tap}: {err:e}");
            worst = worst.max(err);
        }
    }
    println!("golden mono downmix: max {worst:.3e} over 5 cases");
}
