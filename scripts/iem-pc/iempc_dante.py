"""`iempc dante-labels --roster PATH [--site PATH] [--before PATH] [--apply]`
(#58 plan Task 15, design §10): the IEM card's Dante channel labels for the
roster's people. Outcome A of the plan's Step 1 spike (#62): the card sits
on a routed subnet, where mDNS does not reach the dev box but unicast does,
so dante_card.py (the one module that imports netaudio) reads and writes it
by its IP, run with the netaudio venv's python. Nothing here talks to the
PC.

The labels that name a person, and only those (the engineer's and the
translator's outputs, HIL's spare outputs and the inputs nobody owns keep
theirs), in the predecessor's convention (number last on TX, first on RX,
so Dante Controller's matrix lines up):

- each set someone wears: its two TX channels `NAME inear L NN` and `NAME
  inear R NN`. NAME is the main lineup's wearer; when the main lineup
  leaves the set empty, the one person who wears it in the other lineups
  (the row's `lineup` says which); nobody, or more than one person: no row;
- each input someone owns, by the same rule: its RX channel `NN NAME type`,
  a stereo input's two `NN NAME type L|R`; `type` is the last word of that
  lineup's own label for the input, folded like NAME and lower-cased, when
  that is then a short word (`gtr` in `MEMBER4 gtr`, `vocals` in `MEMBER2
  Vocals`), else `mic`.

NAME is the person's name in ASCII capitals: diacritics dropped, other
letters outside ASCII and Dante's `= . @` left out, spaces collapsed. NN is
the card channel the site's `[engine]` gives the set or the input, at least
two digits. A label holds at most LIMIT (31) characters: a longer NAME is
cut and the row says `shortened`. The labels follow the roster, never the
lineup that plays (design §10, decision 4): a youth member on a borrowed
set or mic is iemmixer's business alone, so a lineup switch never changes a
label.

Dante subscribes a receive channel to a transmit channel's NAME, so a
renamed TX channel drops the stagebox's subscription to it until the owner
remakes it by hand in Dante Controller (sessions never change a
subscription); an RX rename keeps its own.

Without `--apply` it prints the sheet, one JSON object `{"dante_labels":
{tx, rx, resubscribe, compared}}`: `resubscribe` lists the TX rows whose
label differs from what the card holds, read now when the private env
names the card (`compared: "card"`; a read that fails is a warning), else
from the `--before` roster (`"before"`, the offline fallback), else every
TX row (null). With `--apply` (the env's DANTE_IEM_HOST, a unicast IPv4
address, DANTE_IEM_NAME and NETAUDIO_PYTHON, an executable file: all three)
it reads the card, writes only the labels that differ from what it holds
(the flag read again right before, and by the helper before each label),
reads them back, and prints `{"dante_apply": {changed, unchanged,
confirmed, written, unsent, mismatches, resubscribe, error}}`: what the
card took and the TX labels it changed, from the read back. When the write
run fails, outlives its bound or answers no report, the planned labels come
out unconfirmed (`confirmed: false`) and it fails; exit 1 on anything but
every label taken. The device at the IP must name itself DANTE_IEM_NAME
before anything is written (dante_card.py). The helper runs here, on the
dev box, never ended by force: a run past CARD_S goes on by itself, and no
other run starts while its recorded pid lives. Both hold people's names and
the site's channels: printed on this box only, never committed, never
posted (P6).

Dev time and locked (iempc's Spec): refused while the EVENT-NOW flag exists,
since renames happen only in dev time with the owner ready to remake the
listed subscriptions.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tomllib
import unicodedata
from dataclasses import dataclass
from pathlib import Path

import dante_card   # its checks only (unicast_ipv4, MAX_CHANNEL); it imports netaudio only when it runs a card

LIMIT = 31                 # characters in a Dante channel label
# The private env's keys of the card (none of their values is in this repo, P6).
CARD_KEYS = ("DANTE_IEM_HOST", "DANTE_IEM_NAME", "NETAUDIO_PYTHON")
HELPER = Path(__file__).resolve().parent / "dante_card.py"
CARD_S = 120.0             # one run of the helper: a read, or a write with its read before and after
# In the dev box's state dir: the last run of the helper (its pid), its stdout and its stderr.
RECORD, OUT, ERR = "dante-helper.json", "dante-helper.out", "dante-helper.err"
# Runs that outlived CARD_S: held, never ended by force, so they end by themselves.
OUTLIVED: list[subprocess.Popen] = []
NO_REPORT = "dante_card.py write answered no report of its writes"
FORBIDDEN = frozenset("=.@")
DEFAULT_TYPE = "mic"
TYPE = re.compile(r"[a-z]{1,8}")
SIDES = ("L", "R")


class SheetError(ValueError):
    """A roster or site the labels cannot come from (ids only, never a name)."""


@dataclass(frozen=True)
class Label:
    kind: str          # "tx" (a set's output) or "rx" (an input)
    channel: int       # the card channel, from the site's [engine]
    label: str
    of: str            # the set (a mix id) or the input id
    person: str        # the person it names (an id)
    shortened: bool = False
    lineup: str | None = None   # the other lineup that named it (None: the main lineup)


def plain(text: str) -> str:
    """`text` in what a Dante label holds: NFKD splits a letter from its
    diacritics, which the ASCII filter then drops with every other letter
    outside ASCII, a control character and Dante's `= . @`."""
    return "".join(c for c in unicodedata.normalize("NFKD", text)
                   if c.isascii() and c.isprintable() and c not in FORBIDDEN)


def caps(name: str, person: str) -> str:
    """NAME: the person's name in ASCII capitals, spaces collapsed."""
    out = " ".join(plain(name).upper().split())
    if not out:
        raise SheetError(f"person '{person}': the name holds no letter a Dante label can hold")
    return out


def fit(head: str, name: str, tail: str) -> tuple[str, bool]:
    """`head + name + tail` in at most LIMIT characters, the name cut if it
    must be; whether it was."""
    room = LIMIT - len(head) - len(tail)
    cut = name[:room].rstrip()
    return head + cut + tail, cut != name


def type_word(label: str) -> str:
    """The input's type: its label's last word folded like NAME and lower-cased,
    when the label has two words or more and the word is then a short one."""
    words = label.split()
    last = plain(words[-1]).lower() if words else ""
    return last if len(words) > 1 and TYPE.fullmatch(last) else DEFAULT_TYPE


def _table(doc, key: str, what: str) -> dict:
    value = doc.get(key) if isinstance(doc, dict) else None
    if not isinstance(value, dict):
        raise SheetError(f"{what}: no [{key}] table")
    return value


def _rows(doc, key: str, what: str) -> list[dict]:
    value = doc.get(key) if isinstance(doc, dict) else None
    if not isinstance(value, list) or not all(isinstance(v, dict) for v in value):
        raise SheetError(f"{what}: no array of tables '{key}'")
    return value


def _channels(value, what: str) -> list[int]:
    """One or two card channels, as Dante numbers them (1 to 255); whether
    the card has them is the card's count to say (`apply`)."""
    ok = (isinstance(value, list) and 1 <= len(value) <= 2
          and all(type(c) is int and 1 <= c <= dante_card.MAX_CHANNEL for c in value))
    if not ok:
        raise SheetError(f"{what}: its channels are not one or two card channels")
    return value


def main_lineup(roster: dict) -> dict:
    mains = [lineup for lineup in _rows(roster, "lineups", "roster") if lineup.get("main") is True]
    if len(mains) != 1:
        raise SheetError(f"roster: exactly one main lineup is needed, it has {len(mains)}")
    return mains[0]


def people(roster: dict) -> dict[str, str]:
    """Person id → name."""
    out = {}
    for p in _rows(roster, "people", "roster"):
        if not isinstance(p.get("id"), str) or not isinstance(p.get("name"), str):
            raise SheetError("roster: a person without a text id and name")
        out[p["id"]] = p["name"]
    return out


def topology(site: dict) -> tuple[dict, dict]:
    """The site's [engine]: mix id → its TX, input id → its RX (unchecked)."""
    engine = _table(site, "engine", "site")
    mixes = {m.get("id"): m.get("tx") for m in _rows(engine, "mixes", "site [engine]")}
    inputs = {i.get("id"): i.get("rx") for i in _rows(engine, "inputs", "site [engine]")}
    return mixes, inputs


def sets_of(lineup: dict) -> dict:
    return _table(lineup, "sets", f"lineup '{lineup.get('id')}'")


def inputs_of(lineup: dict) -> dict:
    owned = lineup.get("inputs", {})
    if not isinstance(owned, dict):
        raise SheetError(f"lineup '{lineup.get('id')}': its inputs are no table")
    return owned


def owner(spec) -> str | None:
    return spec.get("owner") if isinstance(spec, dict) else None


def single(found: list[tuple[str, str, object]]) -> tuple[str, str, object] | None:
    """(person, lineup id, entry) when exactly one person appears in `found`
    (the first lineup naming that person), else None."""
    people_found = {person for person, _, _ in found}
    return found[0] if len(people_found) == 1 else None


def labels(roster: dict, site: dict) -> list[Label]:
    """Every label the roster's people give the card (pure): TX rows by
    channel, then RX rows by channel. A set names its main-lineup wearer;
    when the main lineup leaves it empty, the one person who wears it in the
    other lineups (`lineup` says which), and no row when nobody or more than
    one person does. An input the same way, by its owner."""
    main = main_lineup(roster)
    others = [lineup for lineup in _rows(roster, "lineups", "roster") if lineup is not main]
    names = people(roster)
    mixes, inputs = topology(site)
    caps_of = {}

    def name_of(person, what: str) -> str:
        if person not in names:
            raise SheetError(f"{what}: person '{person}' is no person of the roster")
        if person not in caps_of:
            caps_of[person] = caps(names[person], person)
        return caps_of[person]

    main_sets = sets_of(main)
    worn = {mix: (person, None, None) for mix, person in main_sets.items()}
    for mix in dict.fromkeys(m for lineup in others for m in sets_of(lineup)):
        if mix not in worn:
            found = single([(sets_of(lineup)[mix], lineup.get("id"), None) for lineup in others
                            if mix in sets_of(lineup)])
            if found is not None:
                worn[mix] = found
    tx = []
    for mix, (person, from_lineup, _) in worn.items():
        name = name_of(person, f"set '{mix}'")
        if mix not in mixes:
            raise SheetError(f"set '{mix}': no mix of the site")
        channels = _channels(mixes[mix], f"set '{mix}'")
        if len(channels) != 2:
            raise SheetError(f"set '{mix}': not a stereo mix of the site")
        for side, channel in zip(SIDES, channels):
            text, cut = fit("", name, f" inear {side} {channel:02d}")
            tx.append(Label("tx", channel, text, mix, person, cut, from_lineup))
    main_inputs = inputs_of(main)
    owned = {i: (owner(spec), None, spec) for i, spec in main_inputs.items() if owner(spec) is not None}
    for input_id in dict.fromkeys(i for lineup in others for i in inputs_of(lineup)):
        if input_id not in owned:
            found = single([(owner(inputs_of(lineup)[input_id]), lineup.get("id"), inputs_of(lineup)[input_id])
                            for lineup in others if owner(inputs_of(lineup).get(input_id)) is not None])
            if found is not None:
                owned[input_id] = found
    rx = []
    for input_id, (person, from_lineup, spec) in owned.items():
        name = name_of(person, f"input '{input_id}'")
        if input_id not in inputs:
            raise SheetError(f"input '{input_id}': no input of the site")
        channels = _channels(inputs[input_id], f"input '{input_id}'")
        kind = type_word(spec.get("name") if isinstance(spec.get("name"), str) else "")
        for side, channel in zip(SIDES if len(channels) == 2 else ("",), channels):
            text, cut = fit(f"{channel:02d} ", name, f" {kind}" + (f" {side}" if side else ""))
            rx.append(Label("rx", channel, text, input_id, person, cut, from_lineup))
    return sorted(tx, key=lambda r: r.channel) + sorted(rx, key=lambda r: r.channel)


def row(r: Label) -> dict:
    out = {"channel": r.channel, "label": r.label, "set" if r.kind == "tx" else "input": r.of, "person": r.person}
    if r.shortened:
        out["shortened"] = True
    if r.lineup is not None:
        out["lineup"] = r.lineup
    return out


def tx_labels(rows: list[Label]) -> dict[int, str]:
    """The TX labels of `rows` by channel (what `--before`'s roster gave the card)."""
    return {r.channel: r.label for r in rows if r.kind == "tx"}


def resubscribe(after: list[Label], known: dict[int, str] | None) -> list[dict]:
    """The TX channels whose label changes from `known` (the card's TX labels
    by channel; None: not known, every TX row): their subscriptions drop (pure)."""
    out = []
    for r in after:
        if r.kind != "tx":
            continue
        was = None if known is None else known.get(r.channel)
        if known is None or was != r.label:
            out.append({"channel": r.channel, "label": r.label, "before": was})
    return out


def sheet(after: list[Label], known: dict[int, str] | None, against: str | None) -> dict:
    """The printed sheet (pure): `against` says what `known` came from, the
    card read now or the `--before` roster (None: nothing known)."""
    return {"tx": [row(r) for r in after if r.kind == "tx"], "rx": [row(r) for r in after if r.kind == "rx"],
            "resubscribe": resubscribe(after, known), "compared": against}


@dataclass(frozen=True)
class Card:
    """The IEM card from the private env: its IP, its expected device name and
    the netaudio venv's python that runs dante_card.py."""
    host: str
    name: str
    python: str


def card_env(ip, env: dict[str, str], need: bool) -> Card | None:
    """The card's keys of the private env; None when none is set and `need`
    is false (the sheet then compares with `--before`). A key missing, or one
    that is not what it names, refuses (its value is never printed)."""
    if not need and not any(env.get(k) for k in CARD_KEYS):
        return None
    missing = [k for k in CARD_KEYS if not env.get(k)]
    if missing:
        raise ip.Refused(f"dante-labels: {', '.join(missing)} missing in the private env (DANTE_IEM_HOST: the card's "
                         "IP, DANTE_IEM_NAME: its expected device name, NETAUDIO_PYTHON: the netaudio venv's python)")
    host, name, python = (env[k] for k in CARD_KEYS)
    if not dante_card.unicast_ipv4(host):
        raise ip.Refused("dante-labels: DANTE_IEM_HOST is not a unicast IPv4 address")
    if not name.isprintable():
        raise ip.Refused("dante-labels: DANTE_IEM_NAME is not printable")
    if not (Path(python).is_absolute() and Path(python).is_file() and os.access(python, os.X_OK)):
        raise ip.Refused("dante-labels: NETAUDIO_PYTHON is not the full path of an executable file")
    return Card(host, name, python)


def running(pid: int) -> bool:
    """Whether `pid` is a run of dante_card.py on this box (a pid reused by
    another program, or one that has ended, is not)."""
    try:
        args = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
    except (FileNotFoundError, ProcessLookupError):
        return False
    return str(HELPER).encode() in args


def helper(ip, cmd: list[str], stdin: str) -> str:
    """One run of dante_card.py, here on the dev box, in its own process
    (tests replace it): its stdout. Its stdout and stderr go to files in the
    state dir, so a run that outlives CARD_S goes on and ends by itself,
    never ended by force; its pid is recorded, and no other run starts
    while it lives. The flag is the caller's to read before a write, and
    the helper's before each label."""
    state = ip.state_dir()
    pid = ip.read_json(state / RECORD, {}).get("pid")
    if type(pid) is int and running(pid):
        raise ip.Refused(f"dante-labels: dante_card.py of an earlier run still runs on this box (pid {pid}); wait "
                         f"for it to end by itself (never force-end it); its answer goes to {state / OUT}")
    verb = cmd[3]
    with open(state / OUT, "w", encoding="utf-8") as out, open(state / ERR, "w", encoding="utf-8") as err:
        try:
            proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=out, stderr=err, text=True, encoding="utf-8",
                                    errors="replace")
        except OSError as e:   # the path stays out of the words (it is a value of the private env)
            raise ip.StepError(f"dante-labels: the netaudio python could not be started "
                               f"({e.strerror or type(e).__name__})") from None
        ip.write_json(state / RECORD, {"pid": proc.pid, "verb": verb})
        try:
            proc.communicate(stdin, timeout=CARD_S)
        except subprocess.TimeoutExpired:
            OUTLIVED.append(proc)
            raise ip.StillRunning(f"dante_card.py {verb} still running after {CARD_S:g} s here on this box (pid "
                                  f"{proc.pid}): left to end by itself, never force-ended; its answer goes to "
                                  f"{state / OUT}, and no other run of it starts while it runs") from None
    if proc.returncode != 0:
        tail = (state / ERR).read_text(encoding="utf-8", errors="replace").strip()[-1500:]
        raise ip.StepError(f"dante_card.py {verb} failed (exit {proc.returncode}): {tail}")
    return (state / OUT).read_text(encoding="utf-8", errors="replace")


def ask_card(ip, card: Card, verb: str, stdin: str = "", extra: tuple[str, ...] = ()) -> dict:
    """dante_card.py `verb` on the configured card: its one JSON object.
    A refusal of its own (another device, no answer) is its exit 1, which
    `helper` raises as a StepError naming its words."""
    out = helper(ip, [card.python, "-I", str(HELPER), verb, card.host, card.name, *extra], stdin)
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1]) if lines else None
    except ValueError:
        doc = None
    if not isinstance(doc, dict):
        raise ip.StepError(f"dante_card.py {verb} printed no JSON object")
    return doc


def held(ip, doc: dict, counted: bool = False) -> dict[str, dict[int, str]]:
    """The labels in one of dante_card.py's answers: {"tx": {n: label}, "rx": ...}.
    `counted` (a read): each kind's count is the number of its labels (the
    helper refuses a read of none)."""
    out = {}
    for kind in ("tx", "rx"):
        part = doc.get(kind)
        ok = isinstance(part, dict) and all(isinstance(k, str) and k.isdigit() and isinstance(v, str)
                                            for k, v in part.items())
        if ok and counted:
            count = doc.get(f"{kind}_count")
            ok = type(count) is int and count == len(part)
        if not ok:
            raise ip.StepError(f"dante_card.py gave no {kind} labels")
        out[kind] = {int(k): v for k, v in part.items()}
    return out


def took(ip, doc: dict, asked: list[dict]) -> tuple[int, str | None, dict | None]:
    """The write's answer (dante_card.py's report of its writes): how many of
    `asked` it touched (each it sent, and the one whose command raised), its
    error, and the labels read back after (None: not read back). A StepError
    when it is not such a report."""
    sent, failed, error, back = (doc.get(k) for k in ("sent", "failed", "error", "read_back"))
    ok = (isinstance(sent, list) and sent == asked[:len(sent)]
          and (failed is None or (len(sent) < len(asked) and failed == asked[len(sent)]))
          and (error is None or isinstance(error, str)) and type(back) is bool
          and isinstance(doc.get("mismatches"), list))
    if not ok:
        raise ip.StepError(NO_REPORT)
    try:
        labels = held(ip, doc) if back else None
    except ip.StepError:
        raise ip.StepError(NO_REPORT) from None
    return len(sent) + (failed is not None), error, labels


def apply(ip, after: list[Label], card: Card) -> int:
    """`--apply`: the card read, only the labels that differ from what it
    holds written (the flag read again right before, and by the helper
    before each label), then read back; one JSON object
    `{"dante_apply": {changed, unchanged, confirmed, written, unsent,
    mismatches, resubscribe, error}}`. `written` and `resubscribe` come from
    the labels read back: what the card took, and the TX labels it changed.
    A write run that fails, outlives its bound or answers no report prints
    the planned labels unconfirmed (`confirmed: false`), then fails. Exit 1
    on anything but every label taken."""
    answer = ask_card(ip, card, "read")
    now = held(ip, answer, counted=True)
    lacking = [r for r in after if r.channel > answer[f"{r.kind}_count"] or r.channel not in now[r.kind]]
    if lacking:
        raise ip.Refused(f"dante-labels: the card has no {lacking[0].kind} channel {lacking[0].channel} (nothing was "
                         "written)")
    changes = [r for r in after if now[r.kind][r.channel] != r.label]
    counts = {"changed": len(changes), "unchanged": len(after) - len(changes)}

    def entry(r: Label) -> dict:
        return {"type": r.kind, "channel": r.channel, "label": r.label, "before": now[r.kind][r.channel]}

    def subscription(r: Label, label: str) -> dict:
        out = {"channel": r.channel, "label": label, "before": now["tx"][r.channel]}
        return out if label == r.label else {**out, "mismatch": True}

    if not changes:
        ip.emit({"dante_apply": {**counts, "confirmed": True, "written": [], "unsent": [], "mismatches": [],
                                 "resubscribe": [], "error": None,
                                 "note": "the card holds every label already: nothing to change"}})
        return 0
    if ip.event_now():   # "ide event" since the start: no label is written during an event
        raise ip.Refused(f"{ip.EVENT_NOW} appeared: no label is written during an event (nothing was written)")
    asked = [{"type": r.kind, "channel": r.channel, "label": r.label} for r in changes]
    try:
        touched, error, labels = took(ip, ask_card(ip, card, "write", json.dumps(asked), (str(ip.EVENT_NOW),)),
                                      asked)
    except ip.StepError as e:   # failed, outlived CARD_S (StillRunning), or no report: what it took is not known
        ip.emit({"dante_apply": {**counts, "confirmed": False, "written": [entry(r) for r in changes],
                                 "unsent": None, "mismatches": None,
                                 "resubscribe": [subscription(r, r.label) for r in changes if r.kind == "tx"],
                                 "error": str(e)[-300:]}})
        raise
    tried, unsent = changes[:touched], [entry(r) for r in changes[touched:]]
    if labels is None:   # sent, not read back: unconfirmed
        report = {"confirmed": False, "written": [entry(r) for r in tried], "mismatches": None,
                  "resubscribe": [subscription(r, r.label) for r in tried if r.kind == "tx"]}
    else:
        got = {(r.kind, r.channel): labels[r.kind].get(r.channel) for r in tried}
        report = {"confirmed": True, "written": [entry(r) for r in tried if got[(r.kind, r.channel)] == r.label],
                  "mismatches": [{"type": r.kind, "channel": r.channel, "want": r.label, "got": got[(r.kind, r.channel)]}
                                 for r in tried if got[(r.kind, r.channel)] != r.label],
                  "resubscribe": [subscription(r, got[("tx", r.channel)]) for r in tried
                                  if r.kind == "tx" and got[("tx", r.channel)] != now["tx"][r.channel]]}
    ip.emit({"dante_apply": {**counts, **report, "unsent": unsent, "error": error}})
    return 0 if report["confirmed"] and not (report["mismatches"] or unsent or error) else 1


def load(path: Path) -> dict:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as e:
        raise SheetError(f"{path}: {e.strerror or type(e).__name__}") from None
    except UnicodeDecodeError:
        raise SheetError(f"{path}: not UTF-8 text") from None
    except tomllib.TOMLDecodeError as e:
        raise SheetError(f"{path}: not TOML ({e})") from None


def run(ctx, ip) -> int:
    """`iempc dante-labels` (dev time, locked; no PC call)."""
    args = ctx.args
    roster = Path(args.roster)
    site_path = Path(args.site) if args.site else roster.parent / "site.toml"
    try:
        site = load(site_path)
        after = labels(load(roster), site)
        try:
            before = labels(load(Path(args.before)), site) if args.before else None
        except SheetError as e:
            raise SheetError(f"--before: {e}") from None
    except SheetError as e:
        raise ip.Refused(f"dante-labels: {e} (no sheet)") from None
    card = card_env(ip, ctx.env, need=bool(args.apply))
    if args.apply:
        return apply(ip, after, card)
    known, against = None, None
    if card is not None:
        try:
            known, against = held(ip, ask_card(ip, card, "read"), counted=True)["tx"], "card"
        except ip.StepError as e:
            print(f"iempc: WARNING: the card's labels could not be read ({str(e)[-300:]}); "
                  f"{'compared with --before' if before is not None else 'every TX row is listed'}",
                  file=sys.stderr, flush=True)
    if known is None and before is not None:
        known, against = tx_labels(before), "before"
    ip.emit({"dante_labels": sheet(after, known, against)})
    return 0
