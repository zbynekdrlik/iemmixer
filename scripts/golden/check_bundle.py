#!/usr/bin/env python3
"""Golden bundle gate (P5): every file listed with its sha256, nothing
unlisted, every plug-in on the allowlist, every media and render path
inside the job tokens. Runs in CI (golden-bundle) and on the dev box
before a bundle is uploaded to the PC. Mirrors GoldenPc.psm1 Test-GoldenRpp."""
from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

ALLOWED = {
    'VST "VST: ReaEQ (Cockos)" reaeq.dll 0 "" 1919247729<56535472656571726561657100000000> ""',
    'JS utility/volume_pan ""',
    'JS loser/MGA_JSLimiterST ""',
}
FX_HEAD = re.compile(r"^<(VST3?|JS|CLAP|AUi?|DX|LV2|VIDEO_EFFECT)\b")
FILE_LINE = re.compile(r'^FILE "@@JOB@@\\stimuli\\[A-Za-z0-9._-]+\.wav"$')
RENDER_LINE = re.compile(r'^RENDER_FILE "@@OUT@@\\[A-Za-z0-9._-]+"$')


def rpp_problems(rel: str, text: str) -> list[str]:
    out: list[str] = []
    for n, line in enumerate(text.splitlines(), start=1):
        t = line.strip()
        if FX_HEAD.match(t) and t[1:] not in ALLOWED:
            out.append(f"{rel}:{n}: plug-in not on the allowlist")
        if t.startswith("FILE ") and not FILE_LINE.match(t):
            out.append(f"{rel}:{n}: media path outside the job")
        if t.startswith("RENDER_FILE ") and not RENDER_LINE.match(t):
            out.append(f"{rel}:{n}: render path outside the job")
    return out


def problems(root: Path) -> list[str]:
    manifest = json.loads((root / "bundle.json").read_text(encoding="utf-8"))
    listed = {f["path"]: f for f in manifest.get("files", [])}
    on_disk = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()} - {"bundle.json"}
    out = [f"{p}: not listed in bundle.json" for p in sorted(on_disk - listed.keys())]
    out += [f"{p}: listed but missing" for p in sorted(listed.keys() - on_disk)]
    for rel, entry in sorted(listed.items()):
        path = root / rel
        if not path.is_file():
            continue
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != entry["sha256"]:
            out.append(f"{rel}: sha256 mismatch")
        if rel.endswith(".rpp"):
            out += rpp_problems(rel, data.decode("utf-8"))
    if not manifest.get("projects"):
        out.append("bundle.json: no projects")
    return out


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print("usage: check_bundle.py BUNDLE_DIR", file=sys.stderr)
        return 2
    found = problems(Path(argv[0]))
    for item in found:
        print(item)
    if found:
        return 1
    print("bundle: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
