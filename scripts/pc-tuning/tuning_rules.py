"""The S1c tuning window's pure rules (no state, no PC): the profile and its
processor rules, the argument checks, the cut rule and the post-boot verdict.
tuning_window re-exports every name; the PC steps stay there."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "golden"))
from golden_window import StepError  # noqa: E402

PROFILE_KEYS = ("version", "journal", "registry_root", "layout", "plan", "governor", "placement", "services_disable",
                "services_mode", "updates", "maintenance", "defender", "devices", "nic", "fingerprint")
LAYOUT_ROLES = ("housekeeping", "card", "nic", "audio")
MODE_LEVERS = ("plan", "governor", "placement", "services")
MAX_CUTS = 5
# The spike outcomes of a completed measure run: it ran to its end, or the stop
# file ended it. refused, fault-caught, rate-changed and stop-hung end it without
# a measurement (outcome "error" already fails in cmd_run). No input level ends a
# run (#38): the stage's levels are only listed in the report.
MEASURED = ("done", "stopped")
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
    check_layout(p["layout"], path)
    check_devices(p["devices"], path)
    check_rss(p["nic"], path)
    return p


# The profile's processor rules (#32 MINOR-6, MAJOR-2 and its review) are the
# same as IemTuning's ConvertTo-IemLpNumber / ConvertTo-IemLpList /
# Assert-IemLayout / Get-IemDeviceLps; both self-tests run the shared cases of
# profile_cases.json. load_profile refuses every processor SHAPE the PC refuses
# (numbers, lists, layout, device lps, rss bounds), so the state step's device
# mask and RSS read never throw on a copied profile. Relations (a card's lps
# against layout.card, a device on a card or audio processor, processors that
# are not present) are checked only on the PC, before any write.

def lp_number(value) -> bool:
    """An integer 0..63. type(), not isinstance(): a bool is an int subclass;
    a float (4.0 too), a string, None or a list is no processor number."""
    return type(value) is int and 0 <= value <= 63


def lp_list(value, what: str, path: Path) -> list[int]:
    """A JSON list of processor numbers; one entry that is none is refused,
    never dropped, truncated or read as a number."""
    if not isinstance(value, list):
        raise StepError(f"{path}: {what}: not a list of processor numbers")
    for i, lp in enumerate(value):
        if not lp_number(lp):
            raise StepError(f"{path}: {what}: entry {i}: not a processor number 0..63 (integers only)")
    return value


def check_layout(layout, path: Path) -> None:
    """layout is an object; a role is absent (no processors) or a list of
    processor numbers; the roles are disjoint."""
    if not isinstance(layout, dict):
        raise StepError(f"{path}: layout: not an object")
    roles: dict[int, str] = {}
    for role in LAYOUT_ROLES:
        if role not in layout:
            continue
        for lp in lp_list(layout[role], f"layout {role}", path):
            if lp in roles:
                raise StepError(f"{path}: layout: processor {lp} has two roles ({roles[lp]}, {role})")
            roles[lp] = role


def check_devices(devices, path: Path) -> None:
    """Every device names a non-empty list of processor numbers (lps)."""
    if not isinstance(devices, list):
        raise StepError(f"{path}: devices: not a list")
    for d in devices:
        if not isinstance(d, dict):
            raise StepError(f"{path}: devices: an entry is not an object")
        lps = d.get("lps")
        if lps is None or lps == []:
            raise StepError(f"{path}: device {d.get('id')}: no processors (lps)")
        lp_list(lps, f"device {d.get('id')} lps", path)


def check_rss(nic, path: Path) -> None:
    """nic.rss base and max are processor numbers, base <= max (where the range
    lies against the layout and the present processors, the PC checks)."""
    rss = nic.get("rss") if isinstance(nic, dict) else None
    if not isinstance(rss, dict):
        raise StepError(f"{path}: nic.rss: missing (base and max)")
    for k in ("base", "max"):
        if not lp_number(rss.get(k)):
            raise StepError(f"{path}: nic.rss.{k}: not a processor number 0..63 (integers only)")
    if rss["base"] > rss["max"]:
        raise StepError(f"{path}: nic.rss: base {rss['base']} is above max {rss['max']}")


def layout_lps(profile: dict, role: str) -> list[int]:
    """A layout role's processors; an absent role is none (the layout rule)."""
    return list(profile["layout"].get(role, []))


def watch_lps(profile: dict, audio_cpus: str) -> list[int]:
    """The CPUs whose DPC/ISR budget is watched: the card's and the audio one
    (the spike's --audio-cpus, else the profile's)."""
    audio = parse_lps(audio_cpus) if audio_cpus else layout_lps(profile, "audio")
    return sorted(set(layout_lps(profile, "card")) | set(audio))


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


def boot_changed(prepared: str | None, now: str | None) -> tuple[bool, str | None]:
    """Whether the PC booted since reboot-prepare, told by the boot token
    (IemTuning's volatile boot key; equal tokens are one boot, Test-IemSameBoot),
    never by the boot time, which a clock change moves (F2 round 3, decision 5).
    Returns (rebooted, why it cannot be told)."""
    if not prepared:
        return False, "reboot-prepare recorded no boot token"
    if not now:
        return False, "the boot token cannot be read now"
    return prepared != now, None


def post_boot_verdict(c: dict) -> list[str]:
    problems = []
    if not c["booted_after_request"]:
        problems.append(f"whether the PC rebooted cannot be told ({c['boot_unknown']})" if c.get("boot_unknown")
                        else "the PC did not reboot after the request")
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
    if c.get("boot_problem"):
        # Without a boot token nothing reads as pending, so an empty "pending"
        # proves nothing (#32 MINOR-4).
        problems.append(f"the boot identity is unknown ({c['boot_problem']}): what is still pending cannot be told")
    return problems
