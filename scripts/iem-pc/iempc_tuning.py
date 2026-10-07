"""`iempc tuning-install` and the refresh after `iempc activate` (#15, the
re-plan of 2026-10-07, approach 1 step 1, with the decision on the trust model).

S1c's tuning modules (IemTuning.psm1, IemMeasure.psm1) and the private profile
reach the PC's elevated tuning folder (%ProgramData%\\iemmixer\\tuning,
Administrators and SYSTEM change it, the user reads) only through the admin
ssh path from this box, as `iempc bootstrap` runs IemPc.psm1: the modules come
out of the fetched, attested bundle (`extract_member`, its SHA256SUMS checked
again), the profile from the private ~/.config/iemmixer/pc-tuning.json
($TUNING_PROFILE, or `--profile PATH`), its shapes checked here first
(tuning_rules.load_profile). They go by scp into the bootstrap run folder
(`bootstrap/<sha>/tuning` under PC_ROOT) next to the bundle's IemPc.psm1, whose
Install-IemTuning (elevated; the module's sha256 checked on the PC before its
import) checks each file's sha256, the profile with IemTuning's own loader,
then writes them fresh and reads them back. Its answer, the three hashes, must
equal what this box sent. The guard's elevated tuning task then finds them
(`Invoke-IemTuningVerb`) instead of answering `absent`.

`activate --sha` refreshes the two modules from the NEW bundle once its
hand-over is verified, online and offline, when the elevated tuning folder
holds a profile (one read-only check): the installed profile stays as it is
(`-KeepProfile`), so the module never drifts from the running bundle. A failed
refresh is reported (stderr and a `tuning_refresh: failed` line); the
activation still counts as done. The exclude task is left as it is: it would
take the modules from the bundle in the user's root, which the user may change.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "pc-tuning"))
import tuning_rules as tr  # noqa: E402

PROFILE = Path(os.environ.get("TUNING_PROFILE", str(Path.home() / ".config/iemmixer/pc-tuning.json")))
# The bundle's tuning modules (`tuning/<name>`) by the key of their hash in Install-IemTuning's answer.
MODULES = {"tuning": "IemTuning.psm1", "measure": "IemMeasure.psm1"}
# The elevated tuning folder as Register-IemTasks resolves the elevated root: the
# ProgramData known folder, never an environment variable.
TUNING_DIR_PS = "(Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'iemmixer\\tuning')"
PROFILE_PRESENT = f"Test-Path -LiteralPath (Join-Path {TUNING_DIR_PS} 'profile.json') -PathType Leaf"
HASH_KEYS = ("tuning", "measure", "profile")


def checked_profile(ip, path: Path) -> Path:
    """The private profile, its shapes checked as the PC checks them (tuning_rules)."""
    try:
        tr.load_profile(path)
    except (tr.StepError, ValueError, OSError) as e:
        raise ip.Refused(f"the tuning profile is refused: {e}") from None
    return path


def check_hashes(ip, r, want: dict[str, str]) -> dict:
    """Install-IemTuning's answer: the three hashes it read back, each the one
    this box sent (the installed profile's is any sha256 with -KeepProfile)."""
    if not isinstance(r, dict) or sorted(r) != sorted(HASH_KEYS):
        raise ip.StepError(f"Install-IemTuning answered {r!r}, not the three hashes")
    for key in HASH_KEYS:
        got = r[key]
        if not isinstance(got, str) or not ip.HEX64.fullmatch(got):
            raise ip.StepError(f"Install-IemTuning answered {key} {got!r}, not a sha256")
        if key in want and got != want[key]:
            raise ip.StepError(f"the PC read back {key} {got}, this box sent {want[key]}")
    return r


def run_install(ctx, ip, sha: str, profile: Path | None) -> dict:
    """Uploads the bundle's IemPc.psm1 and two tuning modules (and `profile`,
    unless None: the installed one stays) and runs Install-IemTuning elevated.
    Every step changes the PC: a new flag lets each finish (then the event path)."""
    env = ctx.env
    rec = ip.need_record(sha)
    pc_module, pc_module_hex = ip.extract_member(sha, rec, "IemPc.psm1")
    uploads = []
    want: dict[str, str] = {}
    for key, name in MODULES.items():
        local, want[key] = ip.extract_member(sha, rec, f"tuning/{name}", nested=True)
        uploads.append((local, name))
    args = f" -TuningSha256 {ip.ps_quote(want['tuning'])} -MeasureSha256 {ip.ps_quote(want['measure'])}"
    if profile is None:
        args += " -KeepProfile"
    else:
        want["profile"] = ip.sha256_file(profile)
        uploads.append((profile, "profile.json"))
        args += f" -ProfileSha256 {ip.ps_quote(want['profile'])}"
    mode = ctx.watch(abandon=False)
    rel = f"bootstrap/{sha}"
    src = f"{rel}/tuning"
    ip.pc_mkdir(ctx, src, mode)
    ip.scp(str(pc_module), ip.remote(env, f"{rel}/IemPc.psm1"), mode)
    for local, name in uploads:
        ip.scp(str(local), ip.remote(env, f"{src}/{name}"), mode)
    body = f"Install-IemTuning -SourceDir {ip.ps_quote(ip.pc_join(env['PC_ROOT'], src))}{args}"
    r = ip.run_module(env, body, ip.BOOTSTRAP_S, mode, module=ip.pc_join(env["PC_ROOT"], f"{rel}/IemPc.psm1"),
                      module_hex=pc_module_hex)
    return check_hashes(ip, r, want)


def install(ctx, ip) -> int:
    """`iempc tuning-install --sha SHA [--profile PATH]` (dev time)."""
    sha = ip.check_sha(ctx.args.sha)
    profile = checked_profile(ip, Path(ctx.args.profile) if ctx.args.profile else PROFILE)
    ip.emit({"tuning_install": sha, "hashes": run_install(ctx, ip, sha, profile)})
    return 0


def refresh_after_activate(ctx, ip, sha: str) -> None:
    """After `activate --sha`'s verified hand-over: the new bundle's two modules
    when the elevated tuning folder holds a profile, which stays. A failure is
    reported and never raised, so the activation counts; a failure after a new
    flag goes on to main, which runs the event path; a new flag itself
    (EventNow) always does."""
    try:
        present = ip.run_module(ctx.env, PROFILE_PRESENT, ip.STATUS_S, ctx.watch(abandon=True))
        if present is False:
            print("iempc: no tuning profile in the PC's elevated tuning folder: nothing to refresh ('iempc "
                  "tuning-install' installs one)", file=sys.stderr, flush=True)
            return
        if present is not True:
            raise ip.StepError(f"the profile check reads {present!r}")
        ip.emit({"tuning_refresh": sha, "hashes": run_install(ctx, ip, sha, None)})
    except ip.StepError as e:
        if ip.event_now() and not ctx.flag_at_start:
            raise
        print(f"iempc: WARNING: the tuning modules were not refreshed from {sha} ({e}); the activation is done; the "
              f"tuning task keeps the modules it had: run 'iempc tuning-install --sha {sha}'", file=sys.stderr, flush=True)
        ip.emit({"tuning_refresh": "failed", "sha": sha, "error": str(e)[-800:]})
