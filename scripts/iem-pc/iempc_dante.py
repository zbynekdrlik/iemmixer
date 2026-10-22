"""`iempc dante-labels --roster PATH [--site PATH] [--before PATH]` (#58
plan Task 15, design §10): the IEM card's Dante channel labels for the main
lineup, as a sheet the owner types into Dante Controller. Outcome B of the
plan's Step 1 spike: nothing here talks to the PC or to a Dante device.

The labels that name a person, and only those (the engineer's and the
translator's outputs, HIL's spare outputs and the inputs nobody owns keep
theirs), in the predecessor's convention (number last on TX, first on RX,
so Dante Controller's matrix lines up):

- each set the main lineup places a person on: its two TX channels
  `NAME inear L NN` and `NAME inear R NN`;
- each input the main lineup gives an owner: its RX channel `NN NAME type`,
  a stereo input's two `NN NAME type L|R`; `type` is the last word of the
  lineup's own label for the input when that is a short lower-case word
  (`gtr` in `MEMBER4 gtr`), else `mic`.

NAME is the person's name in ASCII capitals: diacritics dropped, other
letters outside ASCII and Dante's `= . @` left out, spaces collapsed. NN is
the card channel the site's `[engine]` gives the set or the input, at least
two digits. A label holds at most LIMIT (31) characters: a longer NAME is
cut and the row says `shortened`. Only the main lineup names the card
(design §10, decision 4): a youth member on a borrowed set or mic is
iemmixer's business alone, so a lineup switch never changes a label.

Dante subscribes a receive channel to a transmit channel's NAME, so a
renamed TX channel drops the stagebox's subscription to it until the owner
remakes it by hand (sessions never change a subscription); an RX rename
keeps its own. `resubscribe` lists the TX rows whose label differs from the
one the `--before` roster gives (the roster the card's labels follow now,
on the same site); without `--before` the card's labels are not known
(outcome B reads no device) and every TX row is listed.

It prints one JSON object `{"dante_labels": {tx, rx, resubscribe,
compared}}`. The sheet holds people's names and the site's channels: it is
printed on this box only, never committed, never posted (P6).

Dev time and locked (iempc's Spec): refused while the EVENT-NOW flag exists,
since the renames it is for happen only in dev time with the owner ready to
remake the listed subscriptions. `write_labels` is the seam a `--apply`
writer plugs into if the spike shows a tool can write the card (outcome A);
none is built.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import re
import tomllib
import unicodedata
from dataclasses import dataclass
from pathlib import Path

LIMIT = 31                 # characters in a Dante channel label
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
    person: str        # the main lineup's person (an id)
    shortened: bool = False


def write_labels(labels: list[Label]) -> None:
    """The seam of `dante-labels --apply` (outcome A of plan Task 15's Step 1
    spike): writes `labels` onto the IEM card's own channels, touching no
    other device and no subscription. Not built: the spike decides whether a
    tool can write the card at all."""
    raise NotImplementedError("dante-labels --apply is not built: plan Task 15's Step 1 spike decides it")


def caps(name: str, person: str) -> str:
    """NAME: the person's name in ASCII capitals (see the module docs). NFKD
    splits a letter from its diacritics, which the ASCII filter then drops."""
    plain = unicodedata.normalize("NFKD", name)
    kept = "".join(c for c in plain if c.isascii() and c.isprintable() and c not in FORBIDDEN)
    out = " ".join(kept.upper().split())
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
    words = label.split()
    return words[-1] if len(words) > 1 and TYPE.fullmatch(words[-1]) else DEFAULT_TYPE


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


def labels(roster: dict, site: dict) -> list[Label]:
    """Every label the main lineup's people give the card (pure): TX rows by
    channel, then RX rows by channel."""
    lineup = main_lineup(roster)
    names = people(roster)
    mixes, inputs = topology(site)
    caps_of = {}

    def name_of(person, what: str) -> str:
        if person not in names:
            raise SheetError(f"{what}: person '{person}' is no person of the roster")
        if person not in caps_of:
            caps_of[person] = caps(names[person], person)
        return caps_of[person]

    tx = []
    for mix, person in _table(lineup, "sets", "the main lineup").items():
        name = name_of(person, f"set '{mix}'")
        if mix not in mixes:
            raise SheetError(f"set '{mix}': no mix of the site")
        channels = _channels(mixes[mix], f"set '{mix}'")
        if len(channels) != 2:
            raise SheetError(f"set '{mix}': not a stereo mix of the site")
        for side, channel in zip(SIDES, channels):
            text, cut = fit("", name, f" inear {side} {channel:02d}")
            tx.append(Label("tx", channel, text, mix, person, cut))
    owned = lineup.get("inputs", {})
    if not isinstance(owned, dict):
        raise SheetError("the main lineup: its inputs are no table")
    rx = []
    for input_id, spec in owned.items():
        if not isinstance(spec, dict) or spec.get("owner") is None:
            continue
        name = name_of(spec["owner"], f"input '{input_id}'")
        if input_id not in inputs:
            raise SheetError(f"input '{input_id}': no input of the site")
        channels = _channels(inputs[input_id], f"input '{input_id}'")
        kind = type_word(spec.get("name") if isinstance(spec.get("name"), str) else "")
        for side, channel in zip(SIDES if len(channels) == 2 else ("",), channels):
            text, cut = fit(f"{channel:02d} ", name, f" {kind}" + (f" {side}" if side else ""))
            rx.append(Label("rx", channel, text, input_id, spec["owner"], cut))
    return sorted(tx, key=lambda r: r.channel) + sorted(rx, key=lambda r: r.channel)


def row(r: Label) -> dict:
    out = {"channel": r.channel, "label": r.label, "set" if r.kind == "tx" else "input": r.of, "person": r.person}
    if r.shortened:
        out["shortened"] = True
    return out


def resubscribe(after: list[Label], before: list[Label] | None) -> list[dict]:
    """The TX channels whose label changes: their subscriptions drop (pure)."""
    known = None if before is None else {r.channel: r.label for r in before if r.kind == "tx"}
    out = []
    for r in after:
        if r.kind != "tx":
            continue
        was = None if known is None else known.get(r.channel)
        if known is None or was != r.label:
            out.append({"channel": r.channel, "label": r.label, "before": was})
    return out


def sheet(after: list[Label], before: list[Label] | None) -> dict:
    """The printed sheet (pure)."""
    return {"tx": [row(r) for r in after if r.kind == "tx"], "rx": [row(r) for r in after if r.kind == "rx"],
            "resubscribe": resubscribe(after, before), "compared": before is not None}


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
    ip.emit({"dante_labels": sheet(after, before)})
    return 0
