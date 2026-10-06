"""The S1a window's pure rules (no state, no PC): the private env, the run
request and its limits, the undo plan, the request hashtable, the bundle's
sums and run, the run verdict. spike_window re-exports every name; the state,
the lock, the PC access and the commands stay there."""
from __future__ import annotations

import re
from pathlib import Path

from golden_window import StepError, ps_quote  # scripts/golden, put on sys.path by spike_window

REQUIRED = (
    "PC_SSH", "PC_ROOT", "PC_ROOT_SCP", "PC_ASIO_MODULE", "PC_ASIO_DRIVER",
    "PC_BUFFER_KEY", "PC_BUFFER_NAME", "PC_BUFFER_ORIGINAL",
    "PC_REAPER_HTTP", "PC_MAIN_PROJECT", "PC_REAPER_START_TASK_PATH", "PC_REAPER_START_TASK",
    "PC_NTRACK", "PC_METER_BRIDGE", "PC_METER_HEARTBEAT", "PC_METER_ACTION",
    "PC_APP_PROCESS", "PC_APP_HTTP", "PC_ACTIVITY_CHANNELS", "RAW_DIR",
)
# The inputs the spike's band guard listens to: the site's stage inputs as card
# numbers from 1 ("101-110,121-124"), or "all" only when asked (program inputs may
# carry signal while the band is silent).
ACTIVITY_CHANNELS = re.compile(r"all|[0-9]+(-[0-9]+)?(,[0-9]+(-[0-9]+)?)*")
CPU_LIST = re.compile(r"[0-9]+(-[0-9]+)?(,[0-9]+(-[0-9]+)?)*")
MAX_INPUT = 1024
FRAMES = (32, 48, 64)
MAX_SECONDS = 36_000     # an 8 h soak with margin (S1c design note §8 W4)


def load_env(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise StepError(f"{path}: missing (private env, plan Task 10)")
    env: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise StepError(f"{path}: not KEY=VALUE: {key}")
        env[key.strip()] = value.strip().strip('"')
    missing = [k for k in REQUIRED if not env.get(k)]
    if missing:
        raise StepError(f"{path}: missing {', '.join(missing)}")
    for k in ("PC_BUFFER_ORIGINAL", "PC_NTRACK"):
        if not env[k].isdigit():
            raise StepError(f"{path}: {k} must be a whole number")
    check_channels(env["PC_ACTIVITY_CHANNELS"], path)
    return env


def check_channels(text: str, path: Path) -> None:
    """The spike refuses the same lists (telemetry.rs `Watched::parse`)."""
    ok = ACTIVITY_CHANNELS.fullmatch(text) is not None
    if ok and text != "all":
        for part in text.split(","):
            first, _, last = part.partition("-")
            lo, hi = int(first), int(last or first)
            ok = ok and 1 <= lo <= hi <= MAX_INPUT
    if not ok:
        raise StepError(f"{path}: PC_ACTIVITY_CHANNELS must be 'all' or card inputs from 1 like 101-110,121-124 (got {text!r})")


def check_request(mode: str, frames: int | None, seconds: int, burn_us: int, stress: int, cycles: int,
                  cpu: int | None = None, threshold_us: int = 10, audio_cpus: str = "", stress_cpus: str = "") -> None:
    if mode not in ("probe", "duplex", "reopen", "hwlat"):
        raise StepError(f"unknown mode {mode}")
    if mode in ("duplex", "reopen") and frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if mode == "hwlat" and not (cpu is not None and 0 <= cpu <= 63 and 1 <= threshold_us <= 1000):
        raise StepError("hwlat needs --cpu 0..63 and --threshold-us 1..1000")
    if not (1 <= seconds <= MAX_SECONDS and 0 <= burn_us <= 300 and 0 <= stress <= 8 and 1 <= cycles <= 20):
        raise StepError(f"limits: seconds 1..{MAX_SECONDS}, burn-us 0..300, stress 0..8, cycles 1..20")
    for text in (audio_cpus, stress_cpus):
        if text and not CPU_LIST.fullmatch(text):
            raise StepError("CPU lists look like 14 or 0,1,6-13")
    # The spike's own rules: busy threads next to a reserved audio CPU need their
    # own CPUs, and those never include an audio CPU (with or without threads).
    if stress > 0 and audio_cpus and not stress_cpus:
        raise StepError("--stress with --audio-cpus needs --stress-cpus (the busy threads' own CPUs)")
    overlap = sorted(cpu_set(stress_cpus) & cpu_set(audio_cpus))
    if overlap:
        raise StepError(f"--stress-cpus and --audio-cpus overlap on processors {overlap} "
                        "(a busy thread would run next to the audio callback)")


def cpu_set(text: str) -> set[int]:
    """The processors of a CPU list like 0,1,6-13 (checked by CPU_LIST)."""
    out: set[int] = set()
    for part in (p for p in text.split(",") if p):
        first, _, last = part.partition("-")
        out.update(range(int(first), int(last or first) + 1))
    return out


def run_fields(env: dict[str, str], args) -> dict:
    """The request the PC task hands to the spike."""
    return {"mode": args.mode, "driver": env["PC_ASIO_DRIVER"], "module": env["PC_ASIO_MODULE"], "frames": args.frames or 0,
            "seconds": args.seconds, "burn_us": args.burn_us, "stress": args.stress, "panic_at": args.panic_at,
            "cycles": args.cycles,
            "cpu": -1 if getattr(args, "cpu", None) is None else args.cpu, "threshold_us": getattr(args, "threshold_us", 10),
            "audio_cpus": getattr(args, "audio_cpus", "") or "", "stress_cpus": getattr(args, "stress_cpus", "") or "",
            "activity_channels": env["PC_ACTIVITY_CHANNELS"],
            "timeout": run_timeout(args.mode, args.seconds, args.cycles)}


def run_timeout(mode: str, seconds: int, cycles: int) -> int:
    """Seconds after which the PC task writes the stop file itself."""
    return {"probe": 60, "duplex": seconds + 60, "reopen": 30 * cycles + 60, "hwlat": seconds + 60}[mode]


def buffer_touched(state: dict) -> bool:
    """A set-buffer was recorded (before its write, which may have failed
    half-way): the registry value is unknown, so it is written back and read
    back whatever the recorded value says."""
    return state.get("pref_current") is not None


def undo_plan(state: dict, spike_running: bool) -> list[str]:
    """What leaving the window (or "ide event") must do, in order. While the
    card is free a spike may be starting (the task has not launched it yet),
    so the graceful stop always runs; it is harmless when none runs. A kernel
    trace stops and the S1c mode levers revert before the buffer and REAPER
    (S1c design note §5.2); the fingerprint is read after REAPER is back.
    `rebooting` (tuning_window reboot-prepare) is card-away too: REAPER was
    quit and comes back here unless the reboot already brought it. Its clean
    unwind restored the buffer with read-back, and after the reboot REAPER may
    hold the driver, so a verified restore is not written again there (the
    bring-back reads it and refuses unless it is the original)."""
    plan: list[str] = []
    card = state.get("card")
    card_away = card in ("switching", "free", "rebooting")
    if spike_running or card_away:
        plan.append("stop-spike")
    if state.get("trace"):
        plan.append("trace-stop")
    if state.get("tuning_mode"):
        plan.append("tuning-exit")
    if buffer_touched(state) and not (card == "rebooting" and state.get("pref_restored")):
        plan.append("restore-buffer")
    if card_away:
        plan.append("bring-back")
        if state.get("fingerprint"):
            plan.append("fingerprint")
    return plan


def buffer_args(state: dict) -> str:
    """-Original (and the original text of a String value, from preflight)."""
    args = f"-Original {state['pref_original']}"
    pre = state.get("preflight") or {}
    if pre.get("kind") == "String" and pre.get("raw"):
        args += f" -Raw {ps_quote(str(pre['raw']))}"
    return args


def ps_hashtable(fields: dict) -> str:
    parts = []
    for k, v in fields.items():
        if not re.fullmatch(r"[a-z_]+", k):
            raise StepError(f"bad request field {k!r}")
        parts.append(f"{k} = {v}" if isinstance(v, int) and not isinstance(v, bool) else f"{k} = {ps_quote(str(v))}")
    return "@{ " + "; ".join(parts) + " }"


def parse_sums(text: str) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        m = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not m:
            raise StepError(f"malformed SHA256SUMS line: {line!r}")
        sums[m.group(2)] = m.group(1)
    return sums


def pick_run(runs: list[dict], sha: str) -> int:
    """The successful push run on dev for exactly `sha` (P5)."""
    for r in runs:
        if (r.get("headSha"), r.get("event"), r.get("headBranch"), r.get("conclusion")) == (sha, "push", "dev", "success"):
            return int(r["databaseId"])
    raise StepError(f"no successful push run on dev for {sha}")


def verdict(report: dict) -> dict:
    """Stable = ended as planned with no missed period, no overrun, no
    position gap and no driver reset, overload or buffer-size message."""
    tel = [s.get("telemetry") or {} for s in report.get("segments", [])]
    total = {k: sum(t.get(k, 0) for t in tel) for k in ("callbacks", "late", "missed", "overruns", "position_gaps")}
    messages = {k: sum((t.get("messages") or {}).get(k, 0) for t in tel) for k in ("resets", "overloads", "buffer_size_changes")}
    worst = max(((t.get("interval_us") or {}).get("p999", 0.0) for t in tel), default=0.0)
    stable = report.get("outcome") == "done" and bool(tel) and all(v == 0 for v in messages.values()) and all(
        total[k] == 0 for k in ("missed", "overruns", "position_gaps"))
    return {"outcome": report.get("outcome"), "stable": stable, **messages, "interval_p999_us": worst, **total,
            "activity_channels": report.get("activity_channels"), "loudest_inputs": report.get("loudest_inputs", [])}
