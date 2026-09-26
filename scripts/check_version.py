#!/usr/bin/env python3
"""Version gate: one version for the whole workspace; on PRs to main the head
version must be greater than main's (SemVer 2.0 precedence).

Single source: [workspace.package].version in Cargo.toml. Every crate uses
`version.workspace = true`, tauri.conf.json sets no version, and Cargo.lock
records that version for every workspace package.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ["iem-core", "iem-server", "iem-ui", "iem-tray", "iem-rpp", "iem-dsp"]
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?$")


def parse(version: str) -> tuple[tuple[int, int, int], list[str] | None]:
    match = SEMVER.match(version)
    if not match:
        raise ValueError(f"not a SemVer version: {version!r}")
    core = (int(match[1]), int(match[2]), int(match[3]))
    return core, (match[4].split(".") if match[4] else None)


def compare(a: str, b: str) -> int:
    (core_a, pre_a), (core_b, pre_b) = parse(a), parse(b)
    if core_a != core_b:
        return -1 if core_a < core_b else 1
    if pre_a == pre_b:
        return 0
    if pre_a is None:
        return 1
    if pre_b is None:
        return -1
    for x, y in zip(pre_a, pre_b):
        if x == y:
            continue
        x_num, y_num = x.isdigit(), y.isdigit()
        if x_num and y_num:
            return -1 if int(x) < int(y) else 1
        if x_num != y_num:
            return -1 if x_num else 1
        return -1 if x < y else 1
    return -1 if len(pre_a) < len(pre_b) else 1


def workspace_version(cargo_toml: str) -> str:
    return tomllib.loads(cargo_toml)["workspace"]["package"]["version"]


def consistency_errors(root: Path) -> list[str]:
    errors: list[str] = []
    version = workspace_version((root / "Cargo.toml").read_text(encoding="utf-8"))
    parse(version)
    for crate in CRATES:
        manifest = tomllib.loads((root / "crates" / crate / "Cargo.toml").read_text(encoding="utf-8"))
        if manifest["package"].get("version") != {"workspace": True}:
            errors.append(f"crates/{crate}/Cargo.toml must use version.workspace = true")
    tauri = json.loads((root / "crates" / "iem-tray" / "tauri.conf.json").read_text(encoding="utf-8"))
    if "version" in tauri:
        errors.append("crates/iem-tray/tauri.conf.json must not set a version (Cargo is the single source)")
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    locked = {package["name"]: package["version"] for package in lock["package"] if package["name"] in CRATES}
    for crate in CRATES:
        if locked.get(crate) != version:
            errors.append(f"Cargo.lock has {crate} {locked.get(crate)}, expected {version}")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Workspace version gate.")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--base-ref", help="e.g. origin/main: the head version must be greater")
    args = parser.parse_args(argv)
    errors = consistency_errors(args.root)
    head = workspace_version((args.root / "Cargo.toml").read_text(encoding="utf-8"))
    if args.base_ref:
        base_text = subprocess.run(["git", "-C", str(args.root), "show", f"{args.base_ref}:Cargo.toml"],
                                   check=True, capture_output=True, text=True).stdout
        base = workspace_version(base_text)
        if compare(head, base) <= 0:
            errors.append(f"version {head} must be greater than {args.base_ref} ({base}): bump [workspace.package].version first")
        else:
            print(f"version bump OK: {base} -> {head}")
    for error in errors:
        print(f"::error::{error}")
    if errors:
        return 1
    print(f"version {head}: consistent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
