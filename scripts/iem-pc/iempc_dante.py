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
TX row (null). With `--apply` (the env's DANTE_IEM_HOST, DANTE_IEM_NAME and
NETAUDIO_PYTHON, all three) it reads the card, writes only the labels that
differ from what it holds (the flag read again right before), reads every
one back, and prints `{"dante_apply": {changed, unchanged, mismatches,
written, resubscribe}}`; exit 1 on a mismatch. The device at the IP must
name itself DANTE_IEM_NAME before anything is written (dante_card.py). Both
hold people's names and the site's channels: printed on this box only,
never committed, never posted (P6).

Dev time and locked (iempc's Spec): refused while the EVENT-NOW flag exists,
since renames happen only in dev time with the owner ready to remake the
listed subscriptions.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import ipaddress
import json
import re
import sys
import tomllib
import unicodedata
from dataclasses import dataclass
from pathlib import Path

LIMIT = 31                 # characters in a Dante channel label
# The private env's keys of the card (none of their values is in this repo, P6).
CARD_KEYS = ("DANTE_IEM_HOST", "DANTE_IEM_NAME", "NETAUDIO_PYTHON")
HELPER = Path(__file__).resolve().parent / "dante_card.py"
CARD_S = 120.0             # one run of the helper: a read, or a write with its read before and after
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
    """One or two card channels."""
    ok = (isinstance(value, list) and 1 <= len(value) <= 2
          and all(type(c) is int and 1 <= c <= 9999 for c in value))
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
    try:
        ipaddress.ip_address(host)
    except ValueError:
        raise ip.Refused("dante-labels: DANTE_IEM_HOST is not an IP address") from None
    if not name.isprintable():
        raise ip.Refused("dante-labels: DANTE_IEM_NAME is not printable")
    if not Path(python).is_absolute() or not Path(python).is_file():
        raise ip.Refused("dante-labels: NETAUDIO_PYTHON is not the full path of a file")
    return Card(host, name, python)


def helper(ip, cmd: list[str], stdin: str) -> str:
    """One run of dante_card.py (tests replace it): bounded by CARD_S, never
    ended by force; its stdout. The flag is the caller's to read."""
    return ip.guarded(cmd, stdin, CARD_S, "ignore")


def ask_card(ip, card: Card, verb: str, stdin: str = "") -> dict:
    """dante_card.py `verb` on the configured card: its one JSON object.
    A refusal of its own (another device, no answer) is its exit 1, which
    guarded raises as a StepError naming its words."""
    out = helper(ip, [card.python, "-I", str(HELPER), verb, card.host, card.name], stdin)
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1]) if lines else None
    except ValueError:
        doc = None
    if not isinstance(doc, dict):
        raise ip.StepError(f"dante_card.py {verb} printed no JSON object")
    return doc


def held(ip, doc: dict) -> dict[str, dict[int, str]]:
    """The labels in one of dante_card.py's answers: {"tx": {n: label}, "rx": ...}."""
    out = {}
    for kind in ("tx", "rx"):
        part = doc.get(kind)
        if not isinstance(part, dict) or not all(isinstance(k, str) and k.isdigit() and isinstance(v, str)
                                                 for k, v in part.items()):
            raise ip.StepError(f"dante_card.py gave no {kind} labels")
        out[kind] = {int(k): v for k, v in part.items()}
    return out


def apply(ip, after: list[Label], card: Card) -> int:
    """`--apply`: the card read, only the labels that differ from what it
    holds written (the flag read again right before), every one read back;
    one JSON object. Exit 1 on a mismatch."""
    now = held(ip, ask_card(ip, card, "read"))
    lacking = [r for r in after if r.channel not in now[r.kind]]
    if lacking:
        raise ip.Refused(f"dante-labels: the card has no {lacking[0].kind} channel {lacking[0].channel} (nothing was "
                         "written)")
    changes = [r for r in after if now[r.kind][r.channel] != r.label]
    report = {"changed": len(changes), "unchanged": len(after) - len(changes), "mismatches": [],
              "written": [{"type": r.kind, "channel": r.channel, "label": r.label, "before": now[r.kind][r.channel]}
                          for r in changes],
              "resubscribe": [{"channel": r.channel, "label": r.label, "before": now["tx"][r.channel]}
                              for r in changes if r.kind == "tx"]}
    if not changes:
        ip.emit({"dante_apply": {**report, "note": "the card holds every label already: nothing to change"}})
        return 0
    if ip.event_now():   # "ide event" since the start: no label is written during an event
        raise ip.Refused(f"{ip.EVENT_NOW} appeared: no label is written during an event (nothing was written)")
    doc = ask_card(ip, card, "write", json.dumps([{"type": r.kind, "channel": r.channel, "label": r.label}
                                                  for r in changes]))
    mismatches = doc.get("mismatches")
    if doc.get("written") != len(changes) or not isinstance(mismatches, list):
        raise ip.StepError(f"dante_card.py write answered no count of {len(changes)} written labels and mismatches")
    report["mismatches"] = mismatches
    ip.emit({"dante_apply": report})
    return 1 if mismatches else 0


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
            known, against = held(ip, ask_card(ip, card, "read"))["tx"], "card"
        except ip.StepError as e:
            print(f"iempc: WARNING: the card's labels could not be read ({str(e)[-300:]}); "
                  f"{'compared with --before' if before is not None else 'every TX row is listed'}",
                  file=sys.stderr, flush=True)
    if known is None and before is not None:
        known, against = tx_labels(before), "before"
    ip.emit({"dante_labels": sheet(after, known, against)})
    return 0
