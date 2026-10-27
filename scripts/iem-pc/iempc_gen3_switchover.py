"""The switch between iemmixer 2.x (the IEM PC and its card) and 3.x (the Gen 3
box), on the owner's signal (#72 phase E Task 7; plan
docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md; the takeover
proven live on #72, 2026-10-27).

`iempc gen3-switch to-3|to-2 [--dry-run]` (locked; `to-3` refuses the "ide
event" flag, `to-2` runs during an event too: the way back):

to-3, each step with its undo (a later failure runs the undos newest first):
 1. the box's stack stopped; the band's files moved from the PC and read
    back (iempc_gen3_data.send_band) and the PC's pepper (pepper);
 2. our device's takeover config (the card's name and TX labels, read live
    by IP; iempc_gen3_switch.takeover_config) installed on the box;
 3. the card renamed to its spare name, read back (undo: its name back);
 4. the box's engine and server started (our device under the card's name),
    health healthy and our device answering with the card's name;
 5. our RX table made the card's (the mirror), the receivers given TX_S to
    ask our device for their flows (at least one live TX flow);
 6. the band's public URL moved to the box's connector (iempc_gen3_tunnel);
 7. the box's engine, server and connector enabled at boot (a power loss
    brings 3.x back);
 8. the side recorded (this box's state: `gen3-side.json`).
to-2: the URL back to the PC's connector, the box's units disabled at boot
and stopped (our device's goodbyes: the receivers lose the name), the card's
name back (read back), the box's usual device config installed again, the
side recorded. Each step reads back; a step that fails stops the way back
and says what is left, never undoes the steps before (2.x is the safe side).

stdout: one JSON object of the steps done and fixed words; names and
addresses only from the private env, never printed."""
from __future__ import annotations

import json
import subprocess
import tempfile
import time
from pathlib import Path

import dante_card
import gen3_arc
import iempc_dante
import iempc_gen3_data as data
import iempc_gen3_switch as switch
import iempc_gen3_tunnel as tunnel

SIDE = "gen3-side.json"
STAGED = ".cache/iem-gen3-deploy/dante-device.toml"
INSTALL_CONFIG = f"sudo -n install -D -m 0644 {STAGED} /etc/iemmixer/dante-device.toml"
STOP = "sudo -n systemctl stop iem-server iem-engine iem-dante"
START = "sudo -n systemctl start iem-engine iem-server"
ENABLE = "sudo -n systemctl enable iem-engine iem-server iem-tunnel"
DISABLE = "sudo -n systemctl disable iem-dante iem-engine iem-server"
DISABLE_TUNNEL = "sudo -n systemctl disable iem-tunnel"
HEALTH = "/opt/iemmixer/bin/iem-dante-dev health"
STATUS = "/opt/iemmixer/bin/iem-dante-dev ctl status"
KEYS = ("GEN3_HOST", "GEN3_DANTE_IP", "GEN3_DANTE_NAME", "GEN3_CARD_SPARE", "GEN3_DEVICE_CONFIG", "GEN3_PUBLIC_URL",
        "DANTE_IEM_HOST", "DANTE_IEM_NAME", "NETAUDIO_PYTHON")
HEALTH_S = 60
TX_S = 30
POLL_S = 2.0


class Failed(Exception):
    """A step that did not read back as wanted."""


def copy(host: str, local: str, remote: str) -> int:
    """scp of one file to the box (tests replace it)."""
    return subprocess.run(["scp", "-q", "-o", "BatchMode=yes", local, f"{host}:{remote}"],
                          capture_output=True, timeout=60).returncode


def on_box(host: str, command: str) -> tuple[int, str]:
    code, out, _ = data.box(host, command, b"", data.BOX_S)
    return code, out.decode(errors="replace")


def settings(ip, env: dict[str, str]) -> dict[str, str]:
    missing = [k for k in KEYS if not env.get(k)]
    if missing:
        raise ip.Refused(f"gen3-switch: {', '.join(missing)} missing in the private env")
    for key in ("GEN3_DANTE_NAME", "GEN3_CARD_SPARE", "DANTE_IEM_NAME"):
        if not dante_card.DEVICE_NAME.fullmatch(env[key]):
            raise ip.Refused(f"gen3-switch: {key} is not a Dante device name")
    if env["GEN3_CARD_SPARE"] == env["DANTE_IEM_NAME"]:
        raise ip.Refused("gen3-switch: GEN3_CARD_SPARE must differ from DANTE_IEM_NAME")
    if not Path(env["GEN3_DEVICE_CONFIG"]).is_file():
        raise ip.Refused("gen3-switch: GEN3_DEVICE_CONFIG is not a file (the ops device config)")
    return {k: env[k] for k in KEYS}


def side(ip) -> int:
    doc = ip.read_json(ip.state_dir() / SIDE, {})
    return doc.get("side", 2) if doc.get("side") in (2, 3) else 2


def record(ip, value: int) -> None:
    ip.write_json(ip.state_dir() / SIDE, {"side": value, "at": ip.now_iso()})


def card(ip, s: dict[str, str], name: str) -> iempc_dante.Card:
    return iempc_dante.Card(s["DANTE_IEM_HOST"], name, s["NETAUDIO_PYTHON"])


def rename_card(ip, s: dict[str, str], old: str, new: str) -> None:
    doc = iempc_dante.ask_card(ip, card(ip, s, old), "rename", extra=(new,))
    if doc.get("renamed") is not True:
        raise Failed(f"the card did not take the name {'back' if new == s['DANTE_IEM_NAME'] else 'spare'} "
                     f"({doc.get('error')})")


def install_config(ip, s: dict[str, str], text: str) -> None:
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False, encoding="utf-8") as f:
        f.write(text)
        local = f.name
    try:
        if copy(s["GEN3_HOST"], local, STAGED) != 0 or on_box(s["GEN3_HOST"], INSTALL_CONFIG)[0] != 0:
            raise Failed("the device config could not be installed on the box")
    finally:
        Path(local).unlink(missing_ok=True)


def must(ip, s: dict[str, str], command: str, what: str) -> None:
    if on_box(s["GEN3_HOST"], command)[0] != 0:
        raise Failed(f"{what} failed on the box")


def wait(check, bound: float) -> bool:
    deadline = time.monotonic() + bound
    while True:
        if check():
            return True
        if time.monotonic() > deadline:
            return False
        time.sleep(POLL_S)


def answers_as(ip, host: str, name: str) -> bool:
    try:
        return gen3_arc.read_rx(host)["name"] == name
    except (OSError, ValueError):
        return False


def live_tx_flows(s: dict[str, str]) -> int:
    code, out = on_box(s["GEN3_HOST"], STATUS)
    try:
        return sum(1 for f in json.loads(out)["tx"] if f.get("live") == 1) if code == 0 else 0
    except (ValueError, KeyError, TypeError):
        return 0


def to_3(ctx, ip, s: dict[str, str], dry_run: bool) -> dict:
    host, name, spare = s["GEN3_HOST"], s["DANTE_IEM_NAME"], s["GEN3_CARD_SPARE"]
    labels = {int(n): v for n, v in iempc_dante.ask_card(ip, card(ip, s, name), "read").get("tx", {}).items()}
    try:
        config = switch.takeover_config(Path(s["GEN3_DEVICE_CONFIG"]).read_text(encoding="utf-8"), labels, name)
    except switch.TakeoverError as e:
        raise ip.Refused(f"gen3-switch: {e}: nothing changed") from None
    tunnel.credential_ok(ip, host, make_owner_only=False)
    if dry_run:
        return {"dry_run": True, "steps": ["band", "config", "card", "start", "mirror", "url", "boot"]}
    done: list[str] = []
    undo: list = []
    try:
        must(ip, s, STOP, "stopping the stack")
        report = data.send_band(ip, ctx.env, host, ctx.watch(abandon=False))
        data.pepper(ip, ctx.env, host, ctx.watch(abandon=False))
        done.append("band")
        usual = Path(s["GEN3_DEVICE_CONFIG"]).read_text(encoding="utf-8")
        install_config(ip, s, config)
        undo.append(lambda: install_config(ip, s, usual))
        done.append("config")
        rename_card(ip, s, name, spare)
        undo.append(lambda: rename_card(ip, s, spare, name))
        done.append("card")
        must(ip, s, START, "starting the engine and the server")
        undo.append(lambda: must(ip, s, STOP, "stopping the stack"))
        if not wait(lambda: on_box(host, HEALTH)[0] == 0 and answers_as(ip, s["GEN3_DANTE_IP"], name), HEALTH_S):
            raise Failed(f"the box is not healthy under the card's name within {HEALTH_S} s")
        done.append("start")
        sent, why = switch.mirror(ip, switch.Device(s["DANTE_IEM_HOST"], spare),
                                  switch.Device(s["GEN3_DANTE_IP"], name), also=(name,))
        if why:
            raise Failed(f"the mirror: {why}")
        if not wait(lambda: live_tx_flows(s) > 0, TX_S):
            raise Failed(f"no receiver asked our device for a flow within {TX_S} s")
        done.append("mirror")
        tunnel.to_box(ip, ctx.env, host, s["GEN3_PUBLIC_URL"])
        undo.append(lambda: tunnel.to_pc(ip, ctx.env, host, s["GEN3_PUBLIC_URL"]))
        done.append("url")
        must(ip, s, ENABLE, "enabling the box's units at boot")
        done.append("boot")
    except (Failed, ip.StepError) as e:
        left = []
        for step in reversed(undo):
            try:
                step()
            except (Failed, ip.StepError) as u:
                left.append(str(u))
        raise ip.StepError(f"gen3-switch to-3 stopped after {done or 'nothing'}: {e}; "
                           + (f"undo failed: {'; '.join(left)}" if left else "2.x serves again")) from None
    record(ip, 3)
    return {"serves": "3.x", "steps": done, "files": report["members"], "mirrored": sent}


def to_2(ctx, ip, s: dict[str, str], dry_run: bool) -> dict:
    host, name, spare = s["GEN3_HOST"], s["DANTE_IEM_NAME"], s["GEN3_CARD_SPARE"]
    if dry_run:
        return {"dry_run": True, "steps": ["url", "boot", "stop", "card", "config"]}
    done: list[str] = []
    try:
        tunnel.to_pc(ip, ctx.env, host, s["GEN3_PUBLIC_URL"])
        done.append("url")
        must(ip, s, DISABLE, "disabling the stack at boot")
        must(ip, s, DISABLE_TUNNEL, "disabling the connector at boot")
        done.append("boot")
        must(ip, s, STOP, "stopping the stack")
        done.append("stop")
        if not answers_as(ip, s["DANTE_IEM_HOST"], name):   # a way back run again finds the card's name back
            rename_card(ip, s, spare, name)
        done.append("card")
        install_config(ip, s, Path(s["GEN3_DEVICE_CONFIG"]).read_text(encoding="utf-8"))
        done.append("config")
    except (Failed, ip.StepError) as e:
        raise ip.StepError(f"gen3-switch to-2 stopped after {done or 'nothing'}: {e} (run it again once the "
                           "cause is fixed)") from None
    record(ip, 2)
    return {"serves": "2.x", "steps": done}


def run(ctx, ip) -> int:
    """`iempc gen3-switch`."""
    s = settings(ip, ctx.env)
    target = ctx.args.target
    if target == "to-3":
        if ip.event_now():
            raise ip.Refused("gen3-switch to-3: the flag exists: an event is on; only to-2 runs then")
        if side(ip) == 3:
            raise ip.Refused("gen3-switch to-3: 3.x serves already (gen3-side.json)")
        out = to_3(ctx, ip, s, ctx.args.dry_run)
    else:
        out = to_2(ctx, ip, s, ctx.args.dry_run)
    ip.emit({"gen3_switch": out})
    return 0
