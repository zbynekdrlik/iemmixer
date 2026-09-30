#!/usr/bin/env python3
"""S1c tuning window on the dev box (design note §7, §8): the PC tuning
modules (IemTuning.psm1, IemMeasure.psm1 from the verified spike bundle) and
the measurement set, on top of spike_window.py's window, state, event guard
and unwind. A window opens only with spike_window's `new --signal`; every
command here checks the "ide event" flag and pre-empts like spike_window.
Site values come only from the private env ($SPIKE_ENV) and profile
($TUNING_PROFILE). Nothing is ever ended by force; a reboot happens only on
the owner's quoted approval, and it is an immediate restart that an app may
veto (never a delayed one: Windows forces those, I8)."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "asio-spike"))
sys.path.insert(0, str(HERE))
import latency_report as lr  # noqa: E402
import spike_window as sw  # noqa: E402
from golden_window import StepError, ps_quote  # noqa: E402

PROFILE = Path(os.environ.get("TUNING_PROFILE", str(Path.home() / ".config/iemmixer/pc-tuning.json")))
ADK_URL = "https://go.microsoft.com/fwlink/?linkid=2289980"  # ADK 10.1.26100.9457 (September 2026), design note [7]
PROFILE_KEYS = ("version", "journal", "registry_root", "layout", "plan", "governor", "placement", "services_disable",
                "services_mode", "updates", "maintenance", "defender", "devices", "nic", "fingerprint")
LAYOUT_ROLES = ("housekeeping", "card", "nic", "audio")
MODE_LEVERS = ("plan", "governor", "placement", "services")
MAX_CUTS = 5
LABEL = re.compile(r"[a-z0-9][a-z0-9-]{0,39}")
APPROVAL = re.compile(r".*\d{1,2}:\d{2}.*\S.*")


def parse_lps(text: str) -> list[int]:
    out: list[int] = []
    for part in (p.strip() for p in text.split(",") if p.strip()):
        lo, _, hi = part.partition("-")
        try:
            a, b = int(lo), int(hi or lo)
        except ValueError:
            raise StepError(f"bad processor list {text!r}") from None
        if not (0 <= a <= b <= 63):
            raise StepError(f"bad range {part!r}: processors are 0..63, ascending")
        for lp in range(a, b + 1):
            if lp in out:
                raise StepError(f"{text!r} names processor {lp} twice")
            out.append(lp)
    return sorted(out)


def load_profile(path: Path) -> dict:
    if not path.is_file():
        raise StepError(f"{path}: missing (private profile, plan Task 12)")
    p = json.loads(path.read_text(encoding="utf-8"))
    missing = [k for k in PROFILE_KEYS if k not in p]
    if missing:
        raise StepError(f"{path}: missing {', '.join(missing)}")
    roles: dict[int, str] = {}
    for role in LAYOUT_ROLES:
        for lp in p["layout"].get(role, []):
            if not 0 <= int(lp) <= 63:
                raise StepError(f"{path}: layout {role} processor {lp} outside 0..63")
            if lp in roles:
                raise StepError(f"{path}: processor {lp} has two roles ({roles[lp]}, {role})")
            roles[lp] = role
    return p


def watch_lps(profile: dict, audio_cpus: str) -> list[int]:
    """The CPUs whose DPC/ISR budget is watched: the card's and the audio one
    (the spike's --audio-cpus, else the profile's)."""
    audio = parse_lps(audio_cpus) if audio_cpus else list(profile["layout"]["audio"])
    return sorted(set(profile["layout"]["card"]) | set(audio))


def mode_only(text: str) -> list[str]:
    levers = [x.strip() for x in text.split(",") if x.strip()]
    bad = [x for x in levers if x not in MODE_LEVERS]
    if bad or not levers:
        raise StepError(f"--only takes {', '.join(MODE_LEVERS)}")
    return levers


def label_ok(text: str) -> bool:
    return bool(LABEL.fullmatch(text))


def check_approval(text: str) -> None:
    """The owner's approval of the reboot, quoted with its time (HH:MM)."""
    if not APPROVAL.fullmatch(text.strip()) or len(text.strip()) < 12:
        raise StepError("quote the owner's approval with its time, e.g. 'owner, 14:05: áno, reštartuj'")


def should_cut(progress: dict | None, seen: int, cuts: int, circular: bool) -> tuple[bool, int]:
    """A new missed period, overrun or position gap cuts a circular soak
    trace (at most MAX_CUTS times); returns (cut, glitches seen now)."""
    if not progress:
        return False, seen
    total = sum(int(progress.get(k, 0)) for k in ("missed", "overruns", "position_gaps"))
    return (circular and total > seen and cuts < MAX_CUTS), total


def post_boot_verdict(c: dict) -> list[str]:
    problems = []
    if not c["booted_after_request"]:
        problems.append("the PC did not reboot after the request")
    if not c["reaper"]:
        problems.append("REAPER did not start by itself within 5 min")
    if "error" in (c.get("handover") or {}):
        problems.append(f"handover checks failed: {c['handover']['error']}")
    if c["fingerprint"]:
        problems.append("REAPER mode differs: " + ", ".join(d["key"] for d in c["fingerprint"]))
    if c["pending"]:
        problems.append("still pending after the reboot: " + ", ".join(c["pending"]))
    if c["failed_items"]:
        problems.append("items not as applied: " + ", ".join(c["failed_items"]))
    return problems


# ---- PC access (the PC is the external dependency; no unit tests below) ----

def tps(env: dict[str, str], body: str, **kw):
    return sw.ps(env, sw.tuning_body(env, body), **kw)


def as_list(value) -> list:
    """PowerShell returns one row as an object and none as null."""
    if value is None:
        return []
    return value if isinstance(value, list) else [value]


def baseline_path(env: dict[str, str]) -> Path:
    """The REAPER-mode fingerprint baseline: one file across windows."""
    return Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / "fingerprint-baseline.json"


def xperf(env: dict[str, str]) -> str:
    return ps_quote(env["PC_XPERF"])


def need_free(state: dict) -> None:
    if state["card"] != "free":
        raise StepError("the card is not free (run spike_window to-dev first)")


def raw(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


def stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def cmd_tuning_setup(env, args) -> None:
    state = sw.open_state()
    profile = load_profile(PROFILE)
    root = ps_quote(env["PC_TUNING_ROOT"])
    # sw.ps runs this under $ErrorActionPreference='Stop' and returns a PC error
    # as a StepError; a native exit code never throws by itself, so icacls' is
    # checked. Nothing is copied into a folder whose ACL was not set.
    sw.ps(env, f"New-Item -ItemType Directory -Force -Path {root}, (Join-Path {root} 'runs') | Out-Null ; "
               f"& icacls.exe {root} /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null ; "
               "if ($LASTEXITCODE -ne 0) { throw \"icacls exited $LASTEXITCODE\" } ; 'ok'", timeout=60)
    sw.scp(str(PROFILE), f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/profile.json")
    r = tps(env, f"(Read-IemProfile -Path {sw.tuning_profile(env)}).version", timeout=60)
    if baseline_path(env).is_file():
        state["fingerprint"] = str(baseline_path(env))   # to-event and preempt compare against it
        sw.save_state(state)
    print(json.dumps({"tuning-setup": {"profile_version": r, "local_version": profile["version"], "window": state["id"],
                                       "fingerprint": state.get("fingerprint")}}))


def cmd_inventory(env, args) -> None:
    state = sw.open_state()
    f = env["PC_TUNING_ROOT"] + f"\\inventory-{stamp()}.json"
    tps(env, f"$i = Get-IemInventory -ProfilePath {sw.tuning_profile(env)} ; [IO.File]::WriteAllText({ps_quote(f)}, ($i | ConvertTo-Json -Depth 8)) ; 'ok'",
        timeout=900, event="abandon")
    local = raw(env, state) / Path(f.replace("\\", "/")).name
    sw.scp(f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/{local.name}", str(local))
    print(json.dumps({"inventory": str(local), "bytes": local.stat().st_size}))


def cmd_fingerprint(env, args) -> None:
    state = sw.open_state()
    current = tps(env, f"Get-IemReaperFingerprint -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="abandon")
    if args.baseline:
        if state["card"] != "reaper":
            raise StepError("the baseline is read while REAPER runs (before to-dev)")
        path = baseline_path(env)
        path.parent.mkdir(parents=True, exist_ok=True)
        text = json.dumps(current, indent=1)
        (raw(env, state) / f"fingerprint-baseline-{stamp()}.json").write_text(text, encoding="utf-8")
        path.write_text(text, encoding="utf-8")
        state["fingerprint"] = str(path)
        sw.save_state(state)
        print(json.dumps({"fingerprint-baseline": str(path), "keys": len(current)}))
        return
    if not state.get("fingerprint"):
        raise StepError("no baseline in this window (fingerprint --baseline)")
    diff = sw.fingerprint_diff(json.loads(Path(state["fingerprint"]).read_text(encoding="utf-8")), current)
    print(json.dumps({"fingerprint-check": diff}))
    if diff:
        raise StepError("the fingerprint differs from the baseline: " + ", ".join(d["key"] for d in diff))


def cmd_wpt_install(env, args) -> None:
    state = sw.open_state()
    if state["card"] != "reaper":
        raise StepError("install WPT while REAPER still holds the card (before to-dev)")
    local = Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / "adksetup.exe"
    local.parent.mkdir(parents=True, exist_ok=True)
    if not local.is_file():
        subprocess.run(["curl", "-fsSL", "-o", str(local), ADK_URL], check=True, timeout=300)
    sw.scp(str(local), f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/adksetup.exe")
    r = tps(env, f"Install-IemWpt -Setup {ps_quote(env['PC_TUNING_ROOT'] + chr(92) + 'adksetup.exe')} -Xperf {xperf(env)}", timeout=1800)
    print(json.dumps({"wpt-install": r}))


def cmd_enter(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    only = mode_only(args.only)
    state["tuning_mode"] = True   # recorded before the action: preempt reverts even a half-done enter
    sw.save_state(state)
    rows = as_list(tps(env, f"Enter-IemTuningMode -ProfilePath {sw.tuning_profile(env)} -Only @({', '.join(ps_quote(x) for x in only)}) -Idle {ps_quote(args.idle)}", timeout=240))
    state.setdefault("tuning_steps", []).append({"enter": only, "idle": args.idle, "at": stamp()})
    sw.save_state(state)
    print(json.dumps({"enter": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} mode item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_exit(env, args) -> None:
    state = sw.open_state()
    rows = as_list(tps(env, f"Exit-IemTuningMode -ProfilePath {sw.tuning_profile(env)}", timeout=240))
    state["tuning_mode"] = False
    sw.save_state(state)
    print(json.dumps({"exit": rows}))


def only_arg(text: str) -> str:
    groups = [x.strip() for x in text.split(",") if x.strip()]
    if any(not re.fullmatch(r"[a-z]+(:[a-z0-9-]+)?", g) for g in groups):
        raise StepError("--only takes group names such as services,updates or irq:card")
    return "@(" + ", ".join(ps_quote(g) for g in groups) + ")"


def cmd_apply(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = as_list(tps(env, f"Invoke-IemTuningApply -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only_arg(args.only)}", timeout=600))
    state.setdefault("tuning_steps", []).append({"apply": args.tier, "only": args.only, "at": stamp()})
    sw.save_state(state)
    print(json.dumps({"apply": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_undo(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = as_list(tps(env, f"Undo-IemTuning -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only_arg(args.only)}", timeout=600))
    state.setdefault("tuning_steps", []).append({"undo": args.tier, "only": args.only, "at": stamp()})
    sw.save_state(state)
    print(json.dumps({"undo": rows}))
    # Fail loud on any un-reverted item, like cmd_apply (I2, script-failure-policy):
    # post_boot_verdict's failed_items cannot catch it (a failed revert stays
    # journaled but still matches its tuned value → counts ok), so a silent
    # exit 0 would hide a global lever left applied.
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} revert item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_state(env, args) -> None:
    print(json.dumps({"state": tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="abandon")}, indent=1))


def cmd_measure(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    profile = load_profile(PROFILE)
    if not label_ok(args.label):
        raise StepError("--label: lower-case letters, digits and dashes, at most 40")
    current = state["pref_current"] if state["pref_current"] is not None else state["pref_original"]
    if args.frames != current:
        raise StepError(f"the driver's preferred buffer is {current}: spike_window set-buffer --frames {args.frames} first")
    run_dir = env["PC_TUNING_ROOT"] + f"\\runs\\{args.label}-{stamp()}"
    since = tps(env, "Get-IemNow", timeout=60, event="abandon")
    tracing = args.trace != "none"
    if tracing:
        state["trace"] = run_dir   # recorded before the start: preempt stops it
        sw.save_state(state)
        opt = (" -CSwitch" if args.trace == "diag" else "") + (f" -CircularMB {args.circular_mb}" if args.circular_mb else "")
        tps(env, f"Start-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)}{opt}", timeout=120)
    polls: list[dict] = []
    cut = {"n": 0, "seen": 0}

    def on_poll(st: dict) -> None:
        status, progress = st.get("status") or {}, st.get("progress")
        pid = next((r.get("pid") for r in status.get("results") or [] if isinstance(r, dict) and r.get("pid")), 0)
        tid = (progress or {}).get("callback_thread", 0)
        polls.append(tps(env, f"Get-IemPollSample -ProfilePath {sw.tuning_profile(env)} -SpikePid {int(pid or 0)} -ThreadId {int(tid or 0)}",
                         timeout=60, event="abandon"))
        do_cut, cut["seen"] = should_cut(progress, cut["seen"], cut["n"], bool(tracing and args.circular_mb))
        if do_cut:
            cut["n"] += 1
            tps(env, f"Stop-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -Merge -Name 'cut-{cut['n']}.etl' ; "
                     f"Start-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -CircularMB {args.circular_mb}", timeout=300)

    run_args = argparse.Namespace(mode="duplex", frames=args.frames, seconds=args.seconds, burn_us=args.burn_us, stress=args.stress,
                                  panic_at=0, cycles=5, cpu=None, threshold_us=10, audio_cpus=args.audio_cpus, stress_cpus=args.stress_cpus)
    result = sw.cmd_run(env, run_args, on_poll=on_poll)
    state = sw.load_state()   # cmd_run saved its own changes (the run list): never overwrite them
    out = raw(env, state) / Path(run_dir.replace("\\", "/")).name
    out.mkdir(exist_ok=True)
    dpcisr_text = None
    if tracing:
        extra = " ; Export-IemNearGlitch -Xperf {x} -Dir {d}".format(x=xperf(env), d=ps_quote(run_dir)) if args.trace == "diag" else ""
        cuts = " ; ".join(f"Invoke-IemDpcIsr -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -Name 'cut-{i}.etl'" for i in range(1, cut["n"] + 1))
        tps(env, f"Stop-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -Merge ; Invoke-IemDpcIsr -Xperf {xperf(env)} -Dir {ps_quote(run_dir)}"
                 + (f" ; {cuts}" if cuts else "") + extra, timeout=1800)
        state["trace"] = None
        sw.save_state(state)
        scp_dir = env["PC_TUNING_ROOT_SCP"] + "/runs/" + out.name
        names = ["dpcisr.txt"] + [f"cut-{i}.dpcisr.txt" for i in range(1, cut["n"] + 1)] + (["near.txt"] if args.trace == "diag" else [])
        for name in names:
            sw.scp(f"{env['PC_SSH']}:{scp_dir}/{name}", str(out / name))
        dpcisr_text = (out / "dpcisr.txt").read_text(encoding="utf-8", errors="replace")
    events = as_list(tps(env, f"Get-IemSystemEvents -Since {ps_quote(since)}", timeout=120, event="abandon"))
    report = json.loads(Path(result["report"]).read_text(encoding="utf-8"))
    summary = lr.summarize(args.label, result["verdict"], report, dpcisr_text, polls, events, watch_lps(profile, args.audio_cpus))
    summary["cuts"] = [lr.budget_findings(lr.parse_dpcisr((out / f"cut-{i}.dpcisr.txt").read_text(encoding="utf-8", errors="replace")),
                                          watch_lps(profile, args.audio_cpus)) for i in range(1, cut["n"] + 1)]
    if args.trace == "diag":
        summary["near_glitch"] = lr.near_glitch((out / "near.txt").read_text(encoding="utf-8", errors="replace"), period_us=lr.PERIOD_US)
    (out / "summary.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
    state.setdefault("measurements", []).append({"label": args.label, "summary": str(out / "summary.json"), "stable": (result["verdict"] or {}).get("stable")})
    sw.save_state(state)
    print(json.dumps(summary))


def cmd_hwlat(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = []
    for lp in parse_lps(args.lps):
        ns = argparse.Namespace(mode="hwlat", frames=None, seconds=args.seconds, burn_us=0, stress=0, panic_at=0, cycles=1,
                                cpu=lp, threshold_us=args.threshold_us, audio_cpus="", stress_cpus="")
        r = sw.cmd_run(env, ns)
        rows.append(lr.hwlat_summary(json.loads(Path(r["report"]).read_text(encoding="utf-8"))))
    path = raw(env, state) / f"hwlat-{stamp()}.json"
    path.write_text(json.dumps(rows, indent=1), encoding="utf-8")
    print(json.dumps({"hwlat": rows, "file": str(path)}))


def unwind_failures(done: list[dict]) -> list[str]:
    """The unwind steps that did not complete: a spike not confirmed gone, or
    a step recorded with an error (unwind alarms and carries on past those)."""
    return [name for step in done for name, r in step.items()
            if (name == "stop-spike" and not r) or (isinstance(r, dict) and "error" in r)]


def cmd_reboot_prepare(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    running = sw.spike_running(env)
    # A reboot is prepared only over a cleanly preempted window (I1). unwind
    # raises (after an owner alarm) before the buffer write when the spike was
    # not confirmed gone; a trace that did not stop or a mode lever not reverted
    # would carry into the reboot and the event mode it comes back in. Refuse on
    # any of them: the card stays free and the window open (preempt or to-event
    # brings REAPER back); the spike is never force-ended (I8, guard.md).
    done = sw.unwind(env, state, running, bring_back_reaper=False)
    failed = unwind_failures(done)
    if failed:
        sw.alarm(f"the unwind before the reboot did not complete ({', '.join(failed)}): no reboot is prepared; "
                 "the card stays free and the window open (preempt or to-event brings REAPER back)")
        raise StepError(f"the unwind failed at {', '.join(failed)}: no reboot prepared")
    st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120)
    state["card"] = "rebooting"
    state["reboot"] = {"prepared_at": tps(env, "Get-IemNow", timeout=60)}
    sw.save_state(state)
    items = as_list(st["items"])
    print(json.dumps({"reboot-prepare": done, "pending": [i["key"] for i in items if i["pending"]],
                      "revert_pending": [i["key"] for i in items if i["revert_pending"]]}))


# An immediate, planned restart (reason: operating system reconfiguration) and
# never a forced one (I8): Microsoft documents that a timeout above 0 implies
# the force flag, so the timeout is 0. An app may then veto the restart;
# post-boot reports that as "the PC did not reboot after the request".
REBOOT_REQUEST = "& shutdown.exe /r /t 0 /d p:2:4 /c 'iemmixer S1c: owner-approved restart' ; $LASTEXITCODE"


def cmd_reboot(env, args) -> None:
    """Records the owner's quoted approval; without --by-owner it also asks
    Windows for an immediate, planned, graceful restart (REBOOT_REQUEST).
    Refused while the "ide event" flag exists (open_state; main pre-empts)."""
    state = sw.open_state()
    if state["card"] != "rebooting" or "reboot" not in state:
        raise StepError("run reboot-prepare first")
    check_approval(args.approval)
    state["reboot"]["approval"] = args.approval
    state["reboot"]["by"] = "owner" if args.by_owner else "agent"
    sw.save_state(state)
    if args.by_owner:
        print(json.dumps({"reboot": "the owner restarts the PC himself; run post-boot afterwards"}))
        return
    try:
        code = sw.ps(env, REBOOT_REQUEST, timeout=60, event="ignore")
    except StepError as e:
        # The restart begins at once: the session may end before the exit code comes back.
        raise StepError(f"no answer to the restart request ({e}): the PC may be restarting; "
                        "run post-boot, which tells whether it rebooted") from None
    if int(code) != 0:
        raise StepError(f"shutdown.exe /r exited {code}: nothing restarts; tell the owner")
    print(json.dumps({"reboot": "requested", "in_s": 0}))


def cmd_post_boot(env, args) -> None:
    state = sw.load_state()
    if "approval" not in state.get("reboot", {}):
        raise StepError("no approved reboot recorded in this window (reboot --approval ...)")
    deadline = time.monotonic() + 900
    while subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"], "exit"], capture_output=True, check=False).returncode != 0:
        if time.monotonic() > deadline:
            sw.alarm("the PC is not reachable 15 min after the approved reboot: tell the owner (power cycle is his)")
            raise StepError("PC unreachable after the reboot")
        time.sleep(15)
    boot = tps(env, "Get-IemBootTime", timeout=60, event="ignore")
    checks = {"booted_after_request": boot > state["reboot"]["prepared_at"], "reaper": False, "handover": None,
              "fingerprint": [], "pending": [], "failed_items": []}
    for _ in range(30):
        if int(sw.ps(env, "@(Get-Process reaper -ErrorAction SilentlyContinue).Count", timeout=60, event="ignore")) > 0:
            checks["reaper"] = True
            break
        time.sleep(10)
    # The window closes only with REAPER back (#32 B13): the bring-back starts it
    # through the start task when it did not start by itself (still a problem:
    # a reboot must come back in event mode) and runs the handover checks. A
    # failed bring-back keeps the window open (card "rebooting"): preempt or
    # to-event brings REAPER back later.
    try:
        checks["handover"] = sw.bring_back(env, state)
    except StepError as e:
        checks["handover"] = {"error": str(e)}
    back = "error" not in checks["handover"]
    if back:
        state["card"], state["closed"] = "reaper", True
    sw.save_state(state)
    current = tps(env, f"Get-IemReaperFingerprint -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    checks["fingerprint"] = sw.fingerprint_diff(json.loads(baseline_path(env).read_text(encoding="utf-8")), current)
    st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    items = as_list(st["items"])
    checks["pending"] = [i["key"] for i in items if i["pending"] or i["revert_pending"]]
    checks["failed_items"] = [i["key"] for i in items if i["journaled"] and not i["ok"]]
    a = tps(env, "Get-IemCpuSample", timeout=60, event="ignore")
    time.sleep(10)
    b = tps(env, "Get-IemCpuSample", timeout=60, event="ignore")
    checks["interrupts"] = lr.cpu_rates([a, b])
    problems = post_boot_verdict(checks)
    state["post_boot"] = {"checks": checks, "problems": problems}
    sw.save_state(state)
    print(json.dumps({"post-boot": checks, "problems": problems}))
    if problems:
        sw.alarm("after the approved reboot: " + "; ".join(problems) + ". Revert: tuning_window undo --tier 3 in a dev window, "
                 "then the pre-approved revert reboot."
                 + ("" if back else " REAPER is not back, so the window stays open: spike_window to-event brings it back."))
        raise StepError("post-boot checks failed")


STEPS = ("tuning-setup", "enter", "exit", "apply", "undo", "measure", "hwlat", "reboot-prepare")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("tuning-setup", "inventory", "wpt-install", "exit", "state", "reboot-prepare", "post-boot"):
        sub.add_parser(name)
    fp = sub.add_parser("fingerprint")
    g = fp.add_mutually_exclusive_group(required=True)
    g.add_argument("--baseline", action="store_true")
    g.add_argument("--check", action="store_true")
    en = sub.add_parser("enter")
    en.add_argument("--only", default="plan,governor,placement")
    en.add_argument("--idle", choices=("default", "c1", "disable"), default="default")
    for name in ("apply", "undo"):
        p = sub.add_parser(name)
        p.add_argument("--tier", type=int, choices=(2, 3), required=True)
        p.add_argument("--only", default="")
    m = sub.add_parser("measure")
    m.add_argument("--label", required=True)
    m.add_argument("--frames", type=int, default=32)
    m.add_argument("--seconds", type=int, default=600)
    m.add_argument("--burn-us", type=int, default=0)
    m.add_argument("--stress", type=int, default=0)
    m.add_argument("--audio-cpus", default="")
    m.add_argument("--stress-cpus", default="")
    m.add_argument("--trace", choices=("none", "dpc", "diag"), default="dpc")
    m.add_argument("--circular-mb", type=int, default=0)
    h = sub.add_parser("hwlat")
    h.add_argument("--lps", default="0-15")
    h.add_argument("--seconds", type=int, default=30)
    h.add_argument("--threshold-us", type=int, default=10)
    rb = sub.add_parser("reboot")
    rb.add_argument("--approval", required=True)
    rb.add_argument("--by-owner", action="store_true")
    args = ap.parse_args(argv)
    handlers = {"tuning-setup": cmd_tuning_setup, "inventory": cmd_inventory, "fingerprint": cmd_fingerprint, "wpt-install": cmd_wpt_install,
                "enter": cmd_enter, "exit": cmd_exit, "apply": cmd_apply, "undo": cmd_undo, "state": cmd_state, "measure": cmd_measure,
                "hwlat": cmd_hwlat, "reboot-prepare": cmd_reboot_prepare, "reboot": cmd_reboot, "post-boot": cmd_post_boot}
    try:
        env = sw.load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
    except StepError as e:
        print(f"tuning_window: {e}", file=sys.stderr)
        return 1
    try:
        handlers[args.cmd](env, args)
        return 0
    except sw.EventNow:
        print(json.dumps({"event": "ide event (flag file)", "action": "preempt"}), flush=True)
    except StepError as e:
        print(f"tuning_window: {e}", file=sys.stderr, flush=True)
        if args.cmd not in STEPS or not sw.event_now():
            return 1
        print(json.dumps({"event": "ide event (flag file) after a failed step", "action": "preempt"}), flush=True)
    try:
        sw.cmd_preempt(env)
    except StepError as e:
        print(f"tuning_window: preempt: {e}", file=sys.stderr)
        return 1
    return 10


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
