#!/usr/bin/env python3
"""S1b analysis (design note §3): renders fetched from the IEM PC plus the
bundle's case metadata → goldens/s1b/ (laws.json, index.json, *.f64
float64 LE, README.md). Needs numpy. Never claims a live-only behaviour."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
import sys
from pathlib import Path

import numpy as np

LIMIT_BYTES = 20 * 1024 * 1024
TOL_LINEAR = 1e-12
TOL_COEF = 1e-9
EQ_TAPS = 256
SITE_TAPS = 2048
RESIDUALS = [
    "live input duplication of mono inputs (RECMON) — Method B",
    "hardware-output mono downmix (TRANSLATOR) — the send-to-mono law stands in; Method B/C",
    "FX processing on muted tracks (norunmute) — Method C",
    "live plugin delay compensation — Method B/C",
    "REAPER volume/pan/mute ramps — not reproduced (X15)",
]


class Fail(Exception):
    pass


def read_wav(path: Path) -> tuple[int, np.ndarray, int]:
    data = path.read_bytes()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise Fail(f"{path.name}: not RIFF/WAVE")
    pos, fmt, frames = 12, None, None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        size = struct.unpack_from("<I", data, pos + 4)[0]
        body = data[pos + 8:pos + 8 + size]
        if cid == b"fmt ":
            tag, ch, rate, _, _, bits = struct.unpack_from("<HHIIHH", body)
            if tag == 0xFFFE and len(body) >= 26:
                tag = struct.unpack_from("<H", body, 24)[0]
            fmt = (tag, ch, rate, bits)
        elif cid == b"data":
            frames = body
        pos += 8 + size + (size & 1)
    if fmt is None or frames is None:
        raise Fail(f"{path.name}: no fmt/data chunk")
    tag, ch, rate, bits = fmt
    if tag != 3 or bits not in (32, 64):
        raise Fail(f"{path.name}: not IEEE float 32/64 (tag {tag}, {bits} bit)")
    y = np.frombuffer(frames, dtype="<f8" if bits == 64 else "<f4").astype(np.float64)
    return rate, y.reshape(-1, ch), bits


# ---- linear cases ----

def oracle_error(y: np.ndarray, expect: list[dict]) -> float:
    ref = np.zeros_like(y)
    for tap in expect:
        if tap["at"] < len(y):
            ref[tap["at"], 0] += tap["l"]
            ref[tap["at"], 1 % y.shape[1]] += tap["r"]
    return float(np.max(np.abs(y - ref))) if y.size else 0.0


def taps(y: np.ndarray, at: int) -> tuple[float, float]:
    return float(y[at, 0]), float(y[at, 1 % y.shape[1]])


# ---- EQ ----

def alpha_bw(fs: float, f0: float, bw: float) -> float:
    w0 = 2 * math.pi * f0 / fs
    return math.sin(w0) * math.sinh(math.log(2) / 2 * bw * w0 / math.sin(w0))


def alpha_slope(fs: float, f0: float, g: float, bw: float) -> float:
    w0 = 2 * math.pi * f0 / fs
    a = math.sqrt(g)
    s = min(max(1.0 / max(bw, 0.01), 0.01), 1.0)
    return math.sin(w0) / 2 * math.sqrt(max((a + 1 / a) * (1 / s - 1) + 2, 0.0))


def rbj(kind: str, fs: float, f0: float, g: float, bw: float, alpha: float | None = None) -> np.ndarray:
    w0 = 2 * math.pi * f0 / fs
    c = math.cos(w0)
    a = math.sqrt(g)
    al = alpha_bw(fs, f0, bw) if alpha is None else alpha
    if kind == "band":
        b = [1 + al * a, -2 * c, 1 - al * a]
        d = [1 + al / a, -2 * c, 1 - al / a]
    elif kind == "high_pass":
        b = [(1 + c) / 2, -(1 + c), (1 + c) / 2]
        d = [1 + al, -2 * c, 1 - al]
    elif kind == "low_shelf":
        r = 2 * math.sqrt(a) * al
        b = [a * ((a + 1) - (a - 1) * c + r), 2 * a * ((a - 1) - (a + 1) * c), a * ((a + 1) - (a - 1) * c - r)]
        d = [(a + 1) + (a - 1) * c + r, -2 * ((a - 1) + (a + 1) * c), (a + 1) + (a - 1) * c - r]
    elif kind == "high_shelf":
        r = 2 * math.sqrt(a) * al
        b = [a * ((a + 1) + (a - 1) * c + r), -2 * a * ((a - 1) + (a + 1) * c), a * ((a + 1) + (a - 1) * c - r)]
        d = [(a + 1) - (a - 1) * c + r, 2 * ((a - 1) - (a + 1) * c), (a + 1) - (a - 1) * c - r]
    else:
        raise Fail(f"unknown band kind {kind}")
    return np.array([b[0] / d[0], b[1] / d[0], b[2] / d[0], d[1] / d[0], d[2] / d[0]])


def recover_biquad(h: np.ndarray, n: int = 64) -> np.ndarray:
    m = np.column_stack([-h[2:n - 1], -h[1:n - 2]])
    (a1, a2), *_ = np.linalg.lstsq(m, h[3:n], rcond=None)
    b0 = h[0]
    b1 = h[1] + a1 * h[0]
    b2 = h[2] + a1 * h[1] + a2 * h[0]
    return np.array([b0, b1, b2, a1, a2])


def ir(coef: np.ndarray, n: int) -> np.ndarray:
    """Impulse response of a normalised biquad [b0, b1, b2, a1, a2]."""
    b0, b1, b2, a1, a2 = (float(c) for c in coef)
    y = [0.0] * n
    for i in range(n):
        x = b0 if i == 0 else b1 if i == 1 else b2 if i == 2 else 0.0
        y[i] = x - (a1 * y[i - 1] if i > 0 else 0.0) - (a2 * y[i - 2] if i > 1 else 0.0)
    return np.array(y)


def ir_residual(h: np.ndarray, coef: np.ndarray) -> float:
    """Max deviation of a measured IR from a candidate, relative to its peak.
    Classification compares responses, never inverted coefficients: the
    inversion is ill-conditioned for low f0/fs."""
    return float(np.max(np.abs(ir(coef, len(h)) - h)) / max(float(np.max(np.abs(h))), 1e-300))


def shelf_alpha(kind: str, fs: float, f0: float, g: float, coef: np.ndarray) -> float:
    """Informational: alpha implied by recovered coefficients (RBJ shelf form)."""
    c = math.cos(2 * math.pi * f0 / fs)
    a = math.sqrt(g)
    a2n = coef[4]
    base = (a + 1) + (a - 1) * c if kind == "low_shelf" else (a + 1) - (a - 1) * c
    a0 = 2 * base / (1 + a2n)
    return a0 * (1 - a2n) / (4 * math.sqrt(a))


def classify_shelf(kind: str, fs: float, f0: float, g: float, bw: float, h: np.ndarray) -> str:
    for name, alpha in (("B", alpha_bw(fs, f0, bw)), ("A", alpha_slope(fs, f0, g, bw))):
        if ir_residual(h[:EQ_TAPS], rbj(kind, fs, f0, g, bw, alpha=alpha)) <= TOL_COEF:
            return name
    return "neither"


def hp_gain_scale(fs: float, f0: float, bw: float, h: np.ndarray) -> float:
    """h[0] = b0 exactly, so the gain applied to an HPF band is h[0] / b0(G=1)."""
    return float(h[0] / rbj("high_pass", fs, f0, 1.0, bw)[0])


def fftconv(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    n = len(a) + len(b) - 1
    size = 1 << (n - 1).bit_length()
    return np.fft.irfft(np.fft.rfft(a, size) * np.fft.rfft(b, size), size)[:n]


# ---- output ----

def check_size(out: Path, limit: int = LIMIT_BYTES) -> int:
    total = sum(p.stat().st_size for p in out.rglob("*") if p.is_file())
    if total > limit:
        raise Fail(f"goldens are {total} bytes, over the {limit}-byte budget")
    return total


class Vectors:
    """Appends float64 vectors to <name>.f64 and records offsets in index.json."""

    def __init__(self, out: Path) -> None:
        self.out, self.index, self.handles = out, {}, {}

    def add(self, name: str, case: str, data: np.ndarray, meta: dict) -> None:
        fh = self.handles.setdefault(name, (self.out / f"{name}.f64").open("wb"))
        offset = fh.tell() // 8
        arr = np.ascontiguousarray(data, dtype="<f8")
        fh.write(arr.tobytes())
        self.index.setdefault(name, {})[case] = {"offset": offset, "shape": list(arr.shape), **meta}

    def close(self) -> None:
        for fh in self.handles.values():
            fh.close()
        (self.out / "index.json").write_text(json.dumps(self.index, indent=1, sort_keys=True), encoding="utf-8")


def law(laws: dict, name: str, case: str, residual: float, tol: float = TOL_LINEAR, detail: dict | None = None) -> None:
    entry = laws.setdefault(name, {"verdict": "confirmed", "max_residual": 0.0, "cases": [], "detail": []})
    entry["cases"].append(case)
    entry["max_residual"] = max(entry["max_residual"], residual)
    if residual > tol:
        entry["verdict"] = "mismatch"
    if detail:
        entry["detail"].append(detail)


def analyse(bundle: dict, renders: Path, stimuli: Path, out: Path, families: set[str] | None) -> dict:
    laws: dict = {}
    vec = Vectors(out)
    for proj in bundle["projects"]:
        for case in proj["tracks"]:
            fam = case["family"]
            if families and fam not in families:
                continue
            path = renders / proj["id"] / f"{case['track']}.wav"
            if not path.is_file():
                raise Fail(f"missing render {proj['id']}/{case['track']}.wav")
            rate, y, bits = read_wav(path)
            if rate != proj["rate"]:
                raise Fail(f"{path.name}: rate {rate}, expected {proj['rate']}")
            p, name = case["params"], case["track"]
            law(laws, f"render_bits_{proj['bits']}", name, 0.0 if bits == proj["bits"] else 1.0, tol=0.5, detail={"bits": bits})
            if bits != 64:
                continue   # 1e-9 comparisons need 64-bit renders (Task 13 fixes the config)
            k0 = rate // 100
            if case["expect"] is not None:
                fam_law = {"pan": p.get("what", "pan"), "mute": "mute", "sum": "summing", "mono": "mono_media", "cal": "cal_" + str(p.get("case"))}.get(fam, fam)
                law(laws, fam_law, name, oracle_error(y, case["expect"]), detail={"params": p, "measured": [taps(y, t["at"]) for t in case["expect"]]})
            elif fam == "cal" and p["case"] == "trim6":
                g = y[k0, 0] / 0.5
                law(laws, "trim_db", name, abs(g - 10 ** (6 / 20)), detail={"gain": g})
            elif fam in ("cal", "eq", "eq-edge", "bypass") and ("band" in p or "eq" in p or p.get("case") == "peak" or fam == "bypass"):
                h = y[k0:, 0] / 0.5
                if fam == "bypass":
                    want = {"identity": 1.0, "trim_only": 10 ** (p.get("trim_db", 0) / 20)}.get(p["expect"])
                    if want is not None:
                        err = float(np.max(np.abs(h[1:EQ_TAPS]))) + abs(h[0] - want)
                        law(laws, f"bypass_{p['what']}", name, err, detail={"h0": float(h[0])})
                    continue
                vec.add(f"eq-{rate}", name, h[:EQ_TAPS], {"params": p})
                band = p.get("band")
                if band is None:
                    law(laws, "eq_edge", name, 0.0, tol=math.inf, detail={"params": p, "h0": float(h[0]), "sum_abs": float(np.sum(np.abs(h[:EQ_TAPS])))})
                    laws["eq_edge"]["verdict"] = "measured"
                    continue
                coef = recover_biquad(h)
                hn = h[:EQ_TAPS]
                kind, f0, g, bw = band["kind"], band["freq_hz"], band["gain_lin"], band["bw_oct"]
                if kind == "band":
                    law(laws, "peak_bw", name, ir_residual(hn, rbj("band", rate, f0, g, bw)), tol=TOL_COEF, detail={"coef": coef.tolist()})
                elif kind == "high_pass":
                    scale = hp_gain_scale(rate, f0, bw, hn)
                    law(laws, "hp_bw", name, ir_residual(hn / scale, rbj("high_pass", rate, f0, 1.0, bw)), tol=TOL_COEF)
                    laws.setdefault("hp_gain", {"verdict": "measured", "max_residual": 0.0, "cases": [], "detail": []})
                    laws["hp_gain"]["cases"].append(name)
                    laws["hp_gain"]["detail"].append({"gain_lin": g, "scale": scale})
                else:
                    verdict = classify_shelf(kind, rate, f0, g, bw, hn)
                    laws.setdefault("shelf_bw", {"verdict": "measured", "max_residual": 0.0, "cases": [], "detail": []})
                    laws["shelf_bw"]["cases"].append(name)
                    laws["shelf_bw"]["detail"].append({"kind": kind, "fs": rate, "f0": f0, "gain_lin": g, "bw": bw, "alpha_implied": shelf_alpha(kind, rate, f0, g, coef), "candidate": verdict, "coef": coef.tolist()})
            elif fam == "downmix":
                k2 = rate // 50
                law(laws, "mono_downmix", name, 0.0, tol=math.inf, detail={"params": p, "L_at_k0": taps(y, k0), "R_at_k2": taps(y, k2)})
                laws["mono_downmix"]["verdict"] = "measured"
            elif fam == "site-eq":
                if p["stimulus"] == "impulse":
                    vec.add("site-eq-96000", p["id"], y[k0:k0 + SITE_TAPS, 0] / 0.5, {"eq": p["eq"]})
                else:
                    imp = renders / proj["id"] / f"{name.replace('-sweep', '-imp')}.wav"
                    _, yi, _ = read_wav(imp)
                    _, stim, _ = read_wav(stimuli / "sweep-96000.wav")
                    h = yi[k0:, 0] / 0.5
                    ref = fftconv(stim[:, 0], h)[: len(y)]
                    err = float(np.max(np.abs(y[: len(ref), 0] - ref)) / max(np.max(np.abs(y[:, 0])), 1e-300))
                    law(laws, "site_eq_linearity", name, err, tol=1e-6, detail={"id": p["id"], "residual_db": 20 * math.log10(max(err, 1e-300))})
            elif fam == "lim":
                ceiling = 10 ** (p["limit_db"] / 20)
                n = rate
                peak = float(np.max(np.abs(y[:n])))
                if peak > ceiling * (1 + 1e-12):
                    raise Fail(f"{name}: limiter output {peak} above ceiling {ceiling}")
                vec.add(f"lim-{rate}", name, y[:n], {"params": p})
                law(laws, "limiter_ceiling", name, 0.0, detail={"peak": peak, "ceiling": ceiling})
    vec.close()
    for key in ("shelf_bw",):
        entry = laws.get(key)
        if entry:
            cands = {d["candidate"] for d in entry["detail"]}
            entry["verdict"] = f"candidate {cands.pop()}" if len(cands) == 1 and "neither" not in cands else "table"
    for key in ("hp_gain",):
        entry = laws.get(key)
        if entry:
            scales = entry["detail"]
            if all(abs(d["scale"] - 1) <= TOL_COEF for d in scales):
                entry["verdict"] = "ignored"
            elif all(abs(d["scale"] - d["gain_lin"]) <= TOL_COEF * d["gain_lin"] for d in scales):
                entry["verdict"] = "linear gain"
            else:
                entry["verdict"] = "table"
    return laws


def readme(laws: dict) -> str:
    rows = ["| Law | Verdict | Max residual | Cases |", "|---|---|---|---|"]
    for name in sorted(laws):
        e = laws[name]
        rows.append(f"| `{name}` | {e['verdict']} | {e['max_residual']:.3g} | {len(e['cases'])} |")
    return "\n".join(["# S1b goldens", "", "Measured on the IEM PC's REAPER 7.65 (offline renders, D7). Generated by `scripts/golden/analyze.py`; vectors are float64 little-endian, offsets in `index.json`.", "", *rows, "", "## Not covered by offline renders", "", *[f"- {r}" for r in RESIDUALS], ""])


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bundle-json", required=True)
    ap.add_argument("--renders", required=True)
    ap.add_argument("--stimuli", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--families")
    args = ap.parse_args(argv)
    bundle = json.loads(Path(args.bundle_json).read_text(encoding="utf-8"))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    try:
        laws = analyse(bundle, Path(args.renders), Path(args.stimuli), out, set(args.families.split(",")) if args.families else None)
        renders = sorted(p for p in Path(args.renders).rglob("*.wav"))
        digest = hashlib.sha256("".join(f"{p.relative_to(args.renders).as_posix()}:{hashlib.sha256(p.read_bytes()).hexdigest()}\n" for p in renders).encode()).hexdigest()
        doc = {"schema": 1, "generator": bundle.get("generator"), "renders": {"count": len(renders), "sha256_of_list": digest}, "laws": laws, "residuals_not_covered": RESIDUALS}
        (out / "laws.json").write_text(json.dumps(doc, indent=1, sort_keys=True), encoding="utf-8")
        (out / "README.md").write_text(readme(laws), encoding="utf-8")
        total = check_size(out)
    except Fail as e:
        print(f"analyze: {e}", file=sys.stderr)
        return 1
    print(f"analyze: {len(laws)} laws, {total} bytes in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
