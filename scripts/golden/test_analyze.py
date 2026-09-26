"""Tests for scripts/golden/analyze.py (synthetic renders from known filters)."""
from __future__ import annotations

import json
import math
import struct
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze as an  # noqa: E402


def wav(path: Path, rate: int, data: np.ndarray, bits: int = 64, tag: int = 3) -> None:
    data = np.atleast_2d(data.T).T
    frames, ch = data.shape
    raw = data.astype("<f8" if bits == 64 else "<f4").tobytes() if tag == 3 else (data * 32767).astype("<i2").tobytes()
    fmt = struct.pack("<HHIIHH", tag, ch, rate, rate * ch * bits // 8, ch * bits // 8, bits)
    body = b"WAVE" + b"fmt " + struct.pack("<I", len(fmt)) + fmt + b"data" + struct.pack("<I", len(raw)) + raw
    path.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)


def direct_form_ir(coef: np.ndarray, n: int) -> np.ndarray:
    """Independent of an.ir: a direct-form-I filter fed a unit impulse."""
    b0, b1, b2, a1, a2 = coef
    x = np.zeros(n)
    x[0] = 1.0
    y = np.zeros(n)
    for i in range(n):
        y[i] = b0 * x[i] + (b1 * x[i - 1] if i > 0 else 0) + (b2 * x[i - 2] if i > 1 else 0) - (a1 * y[i - 1] if i > 0 else 0) - (a2 * y[i - 2] if i > 1 else 0)
    return y


class WavTests(unittest.TestCase):
    def test_read_wav_round_trips_float_and_refuses_pcm(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "a.wav"
            wav(p, 96000, np.array([[0.5, 0.25], [-1.0, 0.0]]))
            rate, y, bits = an.read_wav(p)
            self.assertEqual((rate, bits), (96000, 64))
            self.assertTrue(np.array_equal(y, np.array([[0.5, 0.25], [-1.0, 0.0]])))
            wav(p, 48000, np.array([[0.5], [0.5]]), bits=16, tag=1)
            with self.assertRaises(an.Fail):
                an.read_wav(p)


class EqTests(unittest.TestCase):
    def test_ir_matches_an_independent_direct_form(self) -> None:
        coef = an.rbj("band", 96000, 1000.0, 10 ** (6 / 20), 1.0)
        self.assertLess(np.max(np.abs(an.ir(coef, 256) - direct_form_ir(coef, 256))), 1e-15)

    def test_recover_biquad_is_exact_for_an_rbj_peak(self) -> None:
        coef = an.rbj("band", 96000, 1000.0, 10 ** (6 / 20), 1.0)
        got = an.recover_biquad(direct_form_ir(coef, 256))
        self.assertLess(np.max(np.abs(got - coef)), 1e-12)

    def test_shelf_alpha_classifies_candidates(self) -> None:
        for fs, f0, g, bw in ((96000, 307.0, 10 ** (-9 / 20), 1.5), (96000, 20.0, 10 ** (12 / 20), 0.4), (44100, 8000.0, 10 ** (-3 / 20), 2.0)):
            for cand in ("B", "A"):
                alpha = an.alpha_bw(fs, f0, bw) if cand == "B" else an.alpha_slope(fs, f0, g, bw)
                h = direct_form_ir(an.rbj("low_shelf", fs, f0, g, bw, alpha=alpha), 256)
                self.assertEqual(an.classify_shelf("low_shelf", fs, f0, g, bw, h), cand, (fs, f0, cand))
        h = direct_form_ir(an.rbj("low_shelf", 96000, 307.0, 0.5, 1.5, alpha=0.001), 256)
        self.assertEqual(an.classify_shelf("low_shelf", 96000, 307.0, 0.5, 1.5, h), "neither")

    def test_hp_gain_scale_is_measured(self) -> None:
        g = 2.0
        coef = an.rbj("high_pass", 96000, 100.0, 1.0, 2.0) * np.array([g, g, g, 1, 1])
        self.assertAlmostEqual(an.hp_gain_scale(96000, 100.0, 2.0, direct_form_ir(coef, 256)), g, places=12)


class LinearTests(unittest.TestCase):
    def test_linear_oracle_detects_a_wrong_gain(self) -> None:
        y = np.zeros((2000, 2))
        y[960] = [0.25, 0.5]
        expect = [{"at": 960, "l": 0.25, "r": 0.5}]
        self.assertEqual(an.oracle_error(y, expect), 0.0)
        y[960, 0] = 0.26
        self.assertAlmostEqual(an.oracle_error(y, expect), 0.01, places=12)
        y[961, 1] = 1e-3
        self.assertGreaterEqual(an.oracle_error(y, expect), 1e-3)


class SizeTests(unittest.TestCase):
    def test_size_gate(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "x.f64").write_bytes(b"\0" * 1024)
            an.check_size(Path(d), limit=2048)
            with self.assertRaises(an.Fail):
                an.check_size(Path(d), limit=512)


class EndToEndTests(unittest.TestCase):
    def test_a_pan_case_and_an_eq_case_become_laws_and_vectors(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "renders" / "pan-96000").mkdir(parents=True)
            (root / "renders" / "eq-96000").mkdir(parents=True)
            (root / "stimuli").mkdir()
            y = np.zeros((48000, 2))
            y[960] = [0.25, 0.5]
            wav(root / "renders" / "pan-96000" / "pan-send-dm-30.wav", 96000, y)
            coef = an.rbj("band", 96000, 1000.0, 2.0, 1.0)
            h = direct_form_ir(coef, 48000 - 960) * 0.5
            e = np.zeros((48000, 2))
            e[960:, 0] = h
            e[960:, 1] = h
            wav(root / "renders" / "eq-96000" / "eq-pk-f2-g3-w2.wav", 96000, e)
            bundle = {"generator": "test", "projects": [
                {"id": "pan-96000", "rate": 96000, "bits": 64, "tracks": [{"track": "pan-send-dm-30", "family": "pan", "stimulus": "imp-dm-96000.wav", "position": 0,
                  "params": {"what": "send_pan", "source": "dm", "pan": 0.5}, "expect": [{"at": 960, "l": 0.25, "r": 0.5}]}]},
                {"id": "eq-96000", "rate": 96000, "bits": 64, "tracks": [{"track": "eq-pk-f2-g3-w2", "family": "eq", "stimulus": "imp-dm-96000.wav", "position": 0,
                  "params": {"band": {"kind": "band", "enabled": True, "freq_hz": 1000.0, "gain_lin": 2.0, "bw_oct": 1.0}, "global_gain": 1.0}, "expect": None}]}]}
            (root / "bundle.json").write_text(json.dumps(bundle), encoding="utf-8")
            out = root / "goldens"
            self.assertEqual(an.main(["--bundle-json", str(root / "bundle.json"), "--renders", str(root / "renders"), "--stimuli", str(root / "stimuli"), "--out", str(out)]), 0)
            laws = json.loads((out / "laws.json").read_text(encoding="utf-8"))["laws"]
            self.assertEqual(laws["send_pan"]["verdict"], "confirmed")
            self.assertEqual(laws["peak_bw"]["verdict"], "confirmed")
            self.assertTrue((out / "eq-96000.f64").is_file())
            self.assertTrue(math.isclose(laws["peak_bw"]["max_residual"], 0.0, abs_tol=1e-9))


if __name__ == "__main__":
    unittest.main()
