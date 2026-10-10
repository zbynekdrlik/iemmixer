"""PC bootstrap and the S1a hand-over of iempc.py (S6 design note §6; split
out of iempc.py, #36): `bootstrap` runs one IemPc.psm1 function over ssh
from a fetched, verified bundle (dev time only, read-only functions too; the
module staged admin-only and checked by its sha256 before the import;
`Register-IemRunner`'s token on stdin only), and `handover-s1a` closes an
open S1a window whose card is free, after which "ide event" is the guard's.

The names a test patches are read as `core.<name>` (iempc_core.py)."""
from __future__ import annotations

import os
import re
from pathlib import Path

import iempc_core as core
from iempc_bundle import extract_member, latest_record_sha, need_record
from iempc_core import (BOOTSTRAP_S, OPS_REPO, STATUS_S, Ctx, EventNow, Refused, StepError, check_sha, emit, pc_join,
                        pc_mkdir, ps_quote, remote, run_module, spike_module)

FUNCTION = re.compile(r"(Get|Test|Set|Add|Register|Grant|Remove)-Iem[A-Za-z0-9]+")
READ_ONLY_VERBS = ("Get", "Test")
PARAM_NAME = re.compile(r"-[A-Za-z][A-Za-z0-9]*")
RUNNER_FUNCTION = "Register-IemRunner"
RUNNER_TOKEN = re.compile(r"[A-Za-z0-9]{20,200}")


def read_only_function(name: str) -> bool:
    return FUNCTION.fullmatch(name) is not None and name.split("-", 1)[0] in READ_ONLY_VERBS


def ps_params(params: list[str]) -> str:
    """`-Name value` pairs and `-Switch` flags for an IemPc function."""
    out = []
    for p in params:
        if p.startswith("--"):
            raise Refused(f"{p}: iempc options go before the function name")
        if PARAM_NAME.fullmatch(p):
            out.append(p)
        elif '"' in p or any(ord(c) < 32 for c in p):
            raise Refused(f"parameter value not allowed on the PC: {p!r}")
        else:
            out.append(ps_quote(p))
    return "".join(" " + p for p in out)


def cmd_bootstrap(ctx: Ctx) -> int:
    """An IemPc.psm1 function over ssh, from the fetched and verified bundle
    (the module's sha256 is checked on the PC before it is imported). Dev
    time only, read-only functions too (design §6): each run makes a folder,
    copies the module and runs it elevated on the PC. A new flag abandons a
    read-only function and lets a changing one finish."""
    env, fn = ctx.env, ctx.args.step
    if not FUNCTION.fullmatch(fn):
        raise Refused(f"not an IemPc function: {fn!r}")
    params = ps_params(ctx.args.params)
    sha = check_sha(ctx.args.sha) if ctx.args.sha else latest_record_sha()
    rec = need_record(sha)
    local, hexd = extract_member(sha, rec, "IemPc.psm1")
    pre = fin = ""
    if fn == RUNNER_FUNCTION:
        # The one-time registration token reaches the PC on stdin only, never a command line.
        token = core.gh(["api", "-X", "POST", f"repos/{OPS_REPO}/actions/runners/registration-token", "--jq", ".token"]).strip()
        if not RUNNER_TOKEN.fullmatch(token):
            raise StepError("the runner registration token has an unexpected form")
        pre = f"$env:ACTIONS_RUNNER_INPUT_TOKEN = {ps_quote(token)} ; "
        fin = "Remove-Item -Path 'Env:\\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue"
    mode = ctx.watch(abandon=read_only_function(fn))
    rel = f"bootstrap/{sha}"
    pc_mkdir(ctx, rel, mode)
    core.scp(str(local), remote(env, f"{rel}/IemPc.psm1"), mode)
    r = run_module(env, fn + params, BOOTSTRAP_S, mode, module=pc_join(env["PC_ROOT"], f"{rel}/IemPc.psm1"),
                   module_hex=hexd, pre=pre, fin=fin)
    emit({"bootstrap": fn, "sha": sha, "result": r})
    return 0


def handover_problems(r: dict, original: int) -> list[str]:
    problems = []
    if r.get("pref") != original:
        problems.append(f"the driver's preferred buffer reads {r.get('pref')}, the original is {original}")
    if r.get("holders"):
        problems.append(f"the card is held: {r.get('holders')}")
    if r.get("spike"):
        problems.append("a spike runs")
    if r.get("task"):
        problems.append("the spike task runs")
    return problems


def cmd_handover_s1a(ctx: Ctx) -> int:
    """Closes an open S1a window whose card is free: no driver-module holder,
    no spike or spike task running, the preference reads the original. From
    then on "ide event" is the guard's (`iempc event`)."""
    sw = spike_module()
    try:
        spike_env = sw.load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
        state = sw.load_state()
        if state.get("closed"):
            raise Refused(f"S1a window {state.get('id')} is already closed")
        if state.get("card") != "free":
            raise Refused(f"S1a window {state.get('id')}: the card is '{state.get('card')}', not free; close it with "
                          "spike_window.py to-event")
        if sw.intent_live(state.get("in_flight")):
            raise Refused(f"S1a window {state.get('id')}: a PC step is in flight ({state['in_flight'].get('step')}): "
                          "wait for it")
        body = (f"$p = Get-SpikeBufferPref -Key {ps_quote(spike_env['PC_BUFFER_KEY'])} -Name {ps_quote(spike_env['PC_BUFFER_NAME'])} ; "
                f"$h = Get-GoldenAsioHolders -Module {ps_quote(spike_env['PC_ASIO_MODULE'])} ; "
                f"$t = Get-ScheduledTask {sw.TASK} -ErrorAction SilentlyContinue ; "
                "[pscustomobject]@{ pref = $p.value; holders = @($h); "
                "spike = @(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count; "
                "task = [bool]($t -and $t.State -eq 'Running') }")
        r = sw.ps(spike_env, body, timeout=STATUS_S, event=ctx.watch(abandon=True))
    except sw.EventNow:
        raise EventNow() from None
    except sw.StepError as e:
        raise StepError(str(e)) from None
    problems = handover_problems(r, int(state["pref_original"]))
    if problems:
        raise StepError("S1a window stays open: " + "; ".join(problems))

    def hand_over(st: dict) -> None:
        # The state as saved now, under the window lock (F2 round 3, m5): another
        # window process may have changed it during the checks.
        if st.get("id") != state.get("id") or st.get("closed"):
            raise Refused(f"S1a window {state.get('id')} was closed meanwhile (a preempt or to-event): nothing handed over")
        if st.get("card") != "free":
            raise StepError(f"S1a window {state.get('id')}: the card is '{st.get('card')}' now, not free: the window stays open")
        if sw.intent_live(st.get("in_flight")):
            raise StepError(f"S1a window {state.get('id')}: a PC step is in flight now ({st['in_flight'].get('step')}): "
                            "the window stays open")
        st["closed"] = True
        st["handed_over"] = {"to": "iemmixer guard (S6)", "at": core.now_iso(), "checks": r}

    try:
        sw.update_state(change=hand_over)
    except sw.StepError as e:   # the window lock was not free within its bound (an owner alarm was printed)
        raise StepError(str(e)) from None
    emit({"handover-s1a": state.get("id"), "closed": True, "checks": r})
    return 0
