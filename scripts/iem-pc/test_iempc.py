"""Tests for scripts/iem-pc/iempc.py. A fake ssh runner stands in for the PC
and a fake gh for GitHub; every value is synthetic, and the private env file
is never read (a temp file takes its place)."""
from __future__ import annotations

import contextlib
import fcntl
import hashlib
import io
import json
import os
import re
import shutil
import sys
import tempfile
import time
import unittest
import warnings
import zipfile
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc as ip  # noqa: E402

REAL_GH = ip.gh  # Base puts a FakeGh in its place for every test
SHA ="1234567890abcdef1234567890abcdef12345678"
SHA2 = "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
RUN = 987654
ENV = {"PC_SSH": "tester@pc.test", "PC_ROOT": "X:\\root", "PC_ROOT_SCP": "/X:/root", "PC_BIN": "X:\\root\\bin"}
OK = json.dumps({"ok": True, "alarms": []})


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_zip(path: Path, *, sha: str = SHA, branch: str = "dev", run: int = RUN, drop: tuple[str, ...] = (),
             tamper: str | None = None, unlisted: str | None = None, rename: dict | None = None,
             manifest: dict | None = None, sums_extra: str = "", manifest_raw: bytes | None = None,
             sums_raw: bytes | None = None) -> Path:
    """A bundle zip shaped like the CI `bundle` job's (plan Task 12)."""
    files = {n: f"synthetic {n}".encode() for n in ip.BUNDLE_REQUIRED if n != "manifest.json"}
    files["tuning/state.ps1"] = b"synthetic tuning"
    doc = manifest if manifest is not None else {"sha": sha, "branch": branch, "version": "2.0.0-dev.9", "run": run}
    files["manifest.json"] = json.dumps(doc).encode() if manifest_raw is None else manifest_raw
    for name in drop:
        files.pop(name)
    sums = "".join(f"{sha256(b)}  {n}\n" for n, b in sorted(files.items())) + sums_extra
    if tamper:
        files[tamper] = b"changed after the sums"
    if unlisted:
        files[unlisted] = b"not in the sums"
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("tuning/", b"")  # a directory entry, as Compress-Archive writes one
        for name, data in files.items():
            z.writestr((rename or {}).get(name, name), data)
        z.writestr("SHA256SUMS", sums if sums_raw is None else sums_raw)
    return path


def unquote(text: str) -> str:
    return text[1:-1].replace("''", "'")


class FakePc:
    """Stands in for `ssh_ps` and `scp`: records each native call (program,
    arguments, flag mode) and module call, answers with scripted replies, and
    ends a watched call on the flag the way `guarded` does."""

    NATIVE = re.compile(r"\$x = ('(?:[^']|'')*') ; \$a = @\((.*?)\) ; \$r = ")

    def __init__(self) -> None:
        self.calls: list[tuple[str, list[str], str]] = []
        self.timeouts: list[float] = []
        self.native_scripts: list[str] = []
        self.modules: list[tuple[str, str]] = []
        self.scps: list[tuple[str, str, str]] = []
        self.replies: dict = {}  # args -> (exit, stdout[, stderr]) or a callable returning one
        self.module_result = "ok"

    def ssh_ps(self, env, script, timeout, event):
        m = self.NATIVE.search(script)
        if m:
            exe = unquote(m.group(1))
            args = [unquote(a) for a in re.findall(r"'(?:[^']|'')*'", m.group(2))]
            self.calls.append((exe.rsplit("\\", 1)[-1], args, event))
            self.timeouts.append(timeout)
            self.native_scripts.append(script)
            reply = self.replies.get(tuple(args), (0, OK))
            code, out, err = (*(reply() if callable(reply) else reply), "")[:3]
            doc = {"exit": code, "out": out, "err": err}
        else:
            self.modules.append((script, event))
            r = self.module_result
            doc = {"ok": True, "r": r() if callable(r) else r}
        if event != "ignore" and ip.event_now():
            raise ip.EventNow()
        return "PowerShell noise\n" + json.dumps(doc) + "\n"

    def scp(self, src, dst, event):
        self.scps.append((src, dst, event))


class FakeGh:
    def __init__(self, artifact: Path) -> None:
        self.calls: list[list[str]] = []
        self.heads = {"dev": SHA, "main": SHA2}
        self.runs = [{"databaseId": RUN, "headSha": SHA, "event": "push", "headBranch": "dev", "conclusion": "success"}]
        self.jobs = {RUN: [{"name": "bundle", "conclusion": "success"}, {"name": "attest", "conclusion": "success"}]}
        self.artifact = artifact
        self.attest_ok = True
        self.runner_reply = "A" * 29
        self.on_list = None
        self.on_download = None

    def __call__(self, args, timeout=ip.GH_S):
        args = list(args)
        self.calls.append(args)
        if args[0] == "api" and args[1].startswith(f"repos/{ip.REPO}/git/ref/heads/"):
            return self.heads[args[1].rsplit("/", 1)[1]] + "\n"
        if args[:2] == ["run", "list"]:
            if self.on_list:
                self.on_list()
            sha = args[args.index("--commit") + 1]
            return json.dumps([r for r in self.runs if r["headSha"] == sha])
        if args[:2] == ["run", "view"]:
            return json.dumps({"jobs": self.jobs.get(int(args[2]), [])})
        if args[:2] == ["run", "download"]:
            dest = Path(args[args.index("-D") + 1])
            dest.mkdir(parents=True, exist_ok=True)
            shutil.copy(self.artifact, dest / self.artifact.name)
            if self.on_download:
                self.on_download()
            return ""
        if args[:2] == ["attestation", "verify"]:
            if not self.attest_ok:
                raise ip.StepError("gh attestation verify failed (exit 1): no attestation matched")
            return ""
        if args[:2] == ["workflow", "run"]:
            return ""
        if args[:3] == ["api", "-X", "POST"]:
            return self.runner_reply + "\n"
        raise AssertionError(f"unexpected gh call {args}")

    def named(self, *prefix: str) -> list[list[str]]:
        return [c for c in self.calls if c[:len(prefix)] == list(prefix)]


class Base(unittest.TestCase):
    PATCHED = ("EVENT_NOW", "STATE_DIR", "SPIKE_STATE", "SPIKE", "POLL_S", "ssh_ps", "scp", "gh", "env_path",
               "EVENT_BUDGET_S", "SPIKE_SHARE_S", "SWITCH_MIN_S")

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        saved = {name: getattr(ip, name) for name in self.PATCHED}
        self.addCleanup(self.restore, saved)
        ip.EVENT_NOW = self.tmp / "config" / "EVENT-NOW"
        ip.STATE_DIR = self.tmp / "state"
        ip.SPIKE_STATE = self.tmp / "spike-window.json"
        ip.POLL_S = 0.05
        self.spike_log = self.tmp / "spike.log"
        self.spike_done = self.tmp / "spike.done"
        ip.SPIKE = self.write_spike(0)
        envfile = self.tmp / "iem-pc.env"
        envfile.write_text("".join(f"{k}={v}\n" for k, v in ENV.items()), encoding="utf-8")
        ip.env_path = lambda: envfile
        self.pc = FakePc()
        ip.ssh_ps, ip.scp = self.pc.ssh_ps, self.pc.scp
        self.artifact = make_zip(self.tmp / "artifact" / f"iemmixer-{SHA}.zip")
        self.gh = FakeGh(self.artifact)
        ip.gh = self.gh

    @staticmethod
    def restore(saved: dict) -> None:
        for name, value in saved.items():
            setattr(ip, name, value)

    def write_spike(self, code: int, delay: float = 0.0) -> Path:
        """A stand-in spike_window.py: logs its arguments at start, takes
        `delay` seconds, marks its end in spike_done, exits with `code`."""
        p = self.tmp / "spike_window.py"
        p.write_text(f"import sys, time\nopen({str(self.spike_log)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
                     f"time.sleep({delay})\nopen({str(self.spike_done)!r}, 'a').write('ended\\n')\n"
                     f"print('preempted')\nsys.exit({code})\n", encoding="utf-8")
        return p

    def wait_for_spike_end(self) -> None:
        """A spike stand-in left running ends by itself (never force-ended)."""
        deadline = time.monotonic() + 15
        while not self.spike_done.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue(self.spike_done.exists(), "the spike stand-in never ended")

    def route_to_real_gh(self, *prefix: str) -> None:
        """gh calls starting with `prefix` go through the real wrapper (and a
        stand-in gh program on PATH); every other call stays with FakeGh."""
        fake = self.gh

        def mixed(args, timeout=ip.GH_S):
            if list(args[:len(prefix)]) == list(prefix):
                return REAL_GH(args, timeout)
            return fake(args, timeout)

        ip.gh = mixed

    def gh_program(self, body: str) -> None:
        """A stand-in `gh` program first on PATH, running `body` (sys, time imported)."""
        d = self.tmp / "bin"
        d.mkdir(exist_ok=True)
        p = d / "gh"
        p.write_text(f"#!{sys.executable}\nimport sys, time\n{body}\n", encoding="utf-8")
        p.chmod(0o755)
        self.set_path(f"{d}{os.pathsep}{os.environ.get('PATH', '')}")

    def set_path(self, path: str) -> None:
        patcher = mock.patch.dict(os.environ, {"PATH": path})
        patcher.start()
        self.addCleanup(patcher.stop)

    def flag(self) -> None:
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
        ip.EVENT_NOW.write_text("2026-09-27T20:00:00+02:00\n", encoding="utf-8")

    def open_window(self, **kw) -> None:
        state = {"id": "w1", "card": "free", "pref_original": 64, "pref_current": None, "closed": False}
        state.update(kw)
        ip.SPIKE_STATE.write_text(json.dumps(state), encoding="utf-8")

    def run_main(self, *argv: str) -> tuple[int, list[dict], str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ip.main(list(argv))
        return code, [json.loads(line) for line in out.getvalue().splitlines() if line.strip()], err.getvalue()

    def fetched(self) -> dict:
        code, docs, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual(code, 0, err)
        return docs[-1]


class EnvTests(Base):
    def write(self, text: str) -> Path:
        p = self.tmp / "other.env"
        p.write_text(text, encoding="utf-8")
        return p

    def test_a_complete_env_loads_and_the_bin_defaults_to_the_root(self) -> None:
        env = ip.load_env(self.write('# private\nPC_SSH="tester@pc.test"\nPC_ROOT=X:\\root\\\nPC_ROOT_SCP=/X:/root\n'))
        self.assertEqual((env["PC_SSH"], env["PC_BIN"]), ("tester@pc.test", "X:\\root\\bin"))
        env = ip.load_env(self.write("PC_SSH=t@h\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\nPC_BIN=Y:\\b\n"))
        self.assertEqual(env["PC_BIN"], "Y:\\b")

    def test_missing_keys_and_files_are_named(self) -> None:
        with self.assertRaisesRegex(ip.StepError, "missing PC_ROOT_SCP"):
            ip.load_env(self.write("PC_SSH=t@h\nPC_ROOT=X:\\root\n"))
        with self.assertRaisesRegex(ip.StepError, "not KEY=VALUE"):
            ip.load_env(self.write("PC_SSH\n"))
        with self.assertRaisesRegex(ip.StepError, "missing"):
            ip.load_env(self.tmp / "absent.env")

    def test_an_ssh_target_that_looks_like_an_option_is_refused(self) -> None:
        with self.assertRaisesRegex(ip.StepError, "not an option"):
            ip.load_env(self.write("PC_SSH=-oProxyCommand=x\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\n"))

    real_env_path = staticmethod(ip.env_path)  # Base replaces ip.env_path with a temp file

    def test_the_env_file_is_pc_env_or_the_private_default(self) -> None:
        with mock.patch.dict(os.environ, {"PC_ENV": "/elsewhere/pc.env"}):
            self.assertEqual(self.real_env_path(), Path("/elsewhere/pc.env"))
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("PC_ENV", None)
            self.assertEqual(self.real_env_path(), Path.home() / ".config/iemmixer/iem-pc.env")

    def test_no_site_values_in_the_module(self) -> None:
        text = Path(ip.__file__).read_text(encoding="utf-8")
        self.assertIsNone(re.search(r"\b\d{1,3}(?:\.\d{1,3}){3}\b", text), "an IPv4 address")
        self.assertIsNone(re.search(r"\b[A-Za-z]:\\", text), "a Windows drive path")
        self.assertIsNone(re.search(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", text), "an ssh destination or email")
        self.assertIsNone(re.search(r"\b[a-z0-9-]+\.(?:lan|home|internal|corp)\b", text), "a site host name")
        self.assertEqual(set(re.findall(r"zbynekdrlik/[\w-]+", text)), {ip.REPO, ip.OPS_REPO})


class GuardedTests(Base):
    """guarded() with local commands standing in for ssh."""

    def py(self, code: str) -> list[str]:
        return [sys.executable, "-c", code]

    def test_output_and_stdin_pass_through(self) -> None:
        out = ip.guarded(self.py("import sys, time; time.sleep(0.2); print(sys.stdin.read().upper())"), "hello\n", 10, "finish")
        self.assertEqual(out.strip(), "HELLO")

    def test_a_failing_command_raises(self) -> None:
        with self.assertRaisesRegex(ip.StepError, "exit 3"):
            ip.guarded(self.py("import sys; sys.exit(3)"), "", 10, "finish")

    def test_abandon_returns_within_a_poll(self) -> None:
        self.flag()
        t = time.monotonic()
        with self.assertRaises(ip.EventNow):
            ip.guarded(self.py("import time; time.sleep(5)"), "", 10, "abandon")
        self.assertLess(time.monotonic() - t, 2.0)

    def test_finish_completes_the_call_first(self) -> None:
        self.flag()
        t = time.monotonic()
        with self.assertRaises(ip.EventNow):
            ip.guarded(self.py("import time; time.sleep(0.5)"), "", 10, "finish")
        self.assertGreaterEqual(time.monotonic() - t, 0.5)

    def test_ignore_is_for_the_preemption_itself(self) -> None:
        self.flag()
        self.assertEqual(ip.guarded(self.py("print('ok')"), "", 10, "ignore").strip(), "ok")

    def test_a_call_past_its_bound_is_reported_never_force_ended(self) -> None:
        done = self.tmp / "bounded.done"
        with self.assertRaisesRegex(ip.StillRunning, "still running after 0.3 s .*never force-end"):
            ip.guarded(self.py(f"import time; time.sleep(1); open({str(done)!r}, 'w').close()"), "", 0.3, "finish")
        self.assertFalse(done.exists())
        deadline = time.monotonic() + 10
        while not done.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue(done.exists(), "the call was left to end by itself")

    def test_a_flag_seen_during_a_finish_call_counts_even_when_gone_at_the_end(self) -> None:
        flag = str(ip.EVENT_NOW)
        code = (f"import pathlib, time; p = pathlib.Path({flag!r}); p.parent.mkdir(parents=True, exist_ok=True); "
                "p.write_text('x'); time.sleep(0.5); p.unlink(); time.sleep(0.3)")
        with self.assertRaises(ip.EventNow):
            ip.guarded(self.py(code), "", 10, "finish")
        self.assertFalse(ip.event_now())

    def test_only_a_flag_that_appears_after_the_start_preempts(self) -> None:
        args = type("A", (), {})()
        self.assertEqual([ip.Ctx(ENV, args, False).watch(True), ip.Ctx(ENV, args, False).watch(False)], ["abandon", "finish"])
        self.assertEqual([ip.Ctx(ENV, args, True).watch(True), ip.Ctx(ENV, args, True).watch(False)], ["ignore", "ignore"])


class ScriptTests(Base):
    def test_the_program_and_each_argument_are_quoted(self) -> None:
        s = ip.native_script("X:\\root\\bin\\iemmode.exe", ["install", "X:\\it's\\a.zip"], ("CHECK",))
        self.assertEqual(s.splitlines()[0], "$ErrorActionPreference = 'Continue'")
        self.assertIn("try { CHECK ; $x = 'X:\\root\\bin\\iemmode.exe' ; $a = @('install', 'X:\\it''s\\a.zip') ; ", s)
        self.assertTrue(s.endswith("ConvertTo-Json -InputObject $o -Compress"))
        self.assertIn("$a = @() ;", ip.native_script("x.exe", []))

    def test_every_powershell_single_quote_is_doubled(self) -> None:
        self.assertEqual(ip.ps_quote("a'b\u2019c\u2018d\u201ae\u201bf"), "'a''b\u2019\u2019c\u2018\u2018d\u201a\u201ae\u201b\u201bf'")

    def test_double_quotes_and_control_characters_never_reach_the_pc(self) -> None:
        for bad in ('a"b', "a\nb", "a\tb"):
            with self.assertRaises(ip.StepError, msg=bad):
                ip.ps_args([bad])

    def test_the_hash_check_throws_on_a_mismatch(self) -> None:
        check = ip.hash_check("X:\\z.zip", "ab" * 32)
        self.assertEqual(check, "if ((Get-FileHash -LiteralPath 'X:\\z.zip' -Algorithm SHA256 -ErrorAction Stop).Hash."
                                "ToLowerInvariant() -ne '" + "ab" * 32 + "') { throw ('sha256 mismatch: ' + 'X:\\z.zip') }")
        for bad in ("AB" * 32, "ab" * 31, "zz" * 32):
            with self.assertRaises(ip.StepError, msg=bad):
                ip.hash_check("X:\\z.zip", bad)

    def test_a_module_is_imported_only_after_its_hash_check(self) -> None:
        s = ip.module_script("Get-IemBootstrapState", module="X:\\m.psm1", module_hex="cd" * 32)
        line = s.splitlines()[2]
        self.assertLess(line.index("Get-FileHash -LiteralPath 'X:\\m.psm1'"), line.index("Import-Module 'X:\\m.psm1' -Force"))
        self.assertLess(line.index("Import-Module"), line.index("$r = & { Get-IemBootstrapState }"))
        self.assertNotIn("finally", s)
        self.assertIn("} finally { F } ; ConvertTo-Json", ip.module_script("B", pre="P ; ", fin="F"))
        self.assertIn("try { P ; $r = & { B }", ip.module_script("B", pre="P ; ", fin="F"))

    def test_the_last_line_is_the_envelope(self) -> None:
        self.assertEqual(ip.last_json("noise\n{\"exit\": 0}\n\n"), {"exit": 0})
        self.assertEqual(ip.last_json("\ufeff{\"a\": 1}"), {"a": 1})
        for bad, words in (("", "no output"), ("noise", "not JSON"), ("[1]", "not a JSON object")):
            with self.assertRaisesRegex(ip.StepError, words):
                ip.last_json(bad)

    def test_a_program_that_did_not_start_is_an_error(self) -> None:
        ip.ssh_ps = lambda env, script, timeout, event: json.dumps({"exit": None, "out": None, "err": "not recognized"})
        with self.assertRaisesRegex(ip.StepError, "iemmode.exe did not run on the PC: not recognized"):
            ip.iemmode(ENV, ["status"], 10, "abandon")
        ip.ssh_ps = lambda env, script, timeout, event: json.dumps({"exit": "zero", "out": "", "err": ""})
        with self.assertRaisesRegex(ip.StepError, "non-numeric"):
            ip.iemmode(ENV, ["status"], 10, "abandon")

    def test_a_module_error_is_raised(self) -> None:
        ip.ssh_ps = lambda env, script, timeout, event: json.dumps({"ok": False, "error": "access denied"})
        with self.assertRaisesRegex(ip.StepError, "PC step failed: access denied"):
            ip.run_module(ENV, "Get-IemBootstrapState", 10, "abandon")


class ReplyTests(Base):
    def test_iemmode_json_is_parsed_in_any_layout(self) -> None:
        doc = {"mode": "dev", "alarms": []}
        for text in (json.dumps(doc), json.dumps(doc, indent=2) + "\n", "\ufeff" + json.dumps(doc), "a log line\n" + json.dumps(doc)):
            self.assertEqual(ip.parse_reply(text), doc, text)

    def test_empty_or_non_object_replies_are_refused(self) -> None:
        for bad, words in (("  \n", "printed nothing"), ("[1, 2]", "not a JSON object"), ("mode: dev", "not JSON")):
            with self.assertRaisesRegex(ip.StepError, words):
                ip.parse_reply(bad)

    def test_owner_questions_are_the_unacknowledged_owner_alarms(self) -> None:
        # The guard's Alarm serializes `acked` (crates/iem-guard/src/alarms.rs).
        reply = {"alarms": [{"id": 1, "owner_question": True}, {"id": 2, "owner_question": True, "acked": True},
                            {"id": 3, "owner_question": False}, "text", {"id": 4}, {"id": 5, "owner_question": "yes"},
                            {"id": 6, "owner_question": True, "acked": False}]}
        self.assertEqual(ip.owner_alarms(reply), [{"id": 1, "owner_question": True},
                                                  {"id": 6, "owner_question": True, "acked": False}])
        self.assertEqual((ip.owner_alarms(None), ip.owner_alarms({"alarms": "x"}), ip.owner_alarms({})), ([], [], []))

    def test_an_owner_question_is_printed_for_the_agent(self) -> None:
        self.pc.replies[("status",)] = (0, json.dumps({"mode": "dev", "alarms": [{"id": 7, "owner_question": True}]}))
        code, docs, err = self.run_main("status")
        self.assertEqual(code, 0)
        self.assertIn('OWNER QUESTION (send the prepared question from the ops runbook): {"id": 7', err)

    def test_a_non_json_reply_is_kept_on_failure_and_refused_on_success(self) -> None:
        self.pc.replies[("status",)] = (1, "usage: iemmode ...")
        code, docs, _ = self.run_main("status")
        self.assertEqual((code, docs[-1]["reply"], docs[-1]["output"]), (1, None, "usage: iemmode ..."))
        self.pc.replies[("status",)] = (0, "usage: iemmode ...")
        code, docs, err = self.run_main("status")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("not JSON", err)

    def test_the_pcs_stderr_is_reported_only_when_there_is_some(self) -> None:
        self.pc.replies[("status",)] = (0, OK, "a warning from the PC")
        code, docs, _ = self.run_main("status")
        self.assertEqual((code, docs[-1]["stderr"]), (0, "a warning from the PC"))
        self.pc.replies[("status",)] = (0, OK, "")
        self.assertNotIn("stderr", self.run_main("status")[1][-1])


class StatusTests(Base):
    def test_status_reports_the_guard_and_this_box(self) -> None:
        self.open_window()
        code, docs, _ = self.run_main("status")
        self.assertEqual(code, 0)
        self.assertEqual({k: docs[-1][k] for k in ("iemmode", "exit", "reply", "event_now", "spike_window_open", "dev_entry")},
                         {"iemmode": ["status"], "exit": 0, "reply": {"ok": True, "alarms": []}, "event_now": False,
                          "spike_window_open": True, "dev_entry": 0})
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])

    def test_during_an_event_status_reports_this_box_only(self) -> None:
        self.flag()
        code, docs, _ = self.run_main("status")
        self.assertEqual((code, self.pc.calls, self.pc.modules), (0, [], []))
        self.assertEqual({k: docs[-1][k] for k in ("iemmode", "event_now", "spike_window_open", "dev_entry")},
                         {"iemmode": None, "event_now": True, "spike_window_open": False, "dev_entry": 0})
        self.assertIn("EVENT-NOW exists: no PC step during an event", docs[-1]["skipped"])

    def test_status_pc_asks_the_guard_during_an_event_and_passes_the_exit_through(self) -> None:
        self.flag()
        self.pc.replies[("status",)] = (4, OK)
        code, docs, _ = self.run_main("status", "--pc")
        self.assertEqual((code, docs[-1]["event_now"], "skipped" in docs[-1]), (4, True, False))
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "ignore")])

    def test_the_spike_window_state(self) -> None:
        self.assertFalse(ip.spike_window_open())
        self.open_window()
        self.assertTrue(ip.spike_window_open())
        self.open_window(closed=True)
        self.assertFalse(ip.spike_window_open())
        ip.SPIKE_STATE.write_text("{broken", encoding="utf-8")
        self.assertTrue(ip.spike_window_open())


class FlagTests(Base):
    def test_the_flag_refuses_dev_and_touches_nothing(self) -> None:
        self.flag()
        code, docs, err = self.run_main("dev", "--build", SHA)
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules, self.gh.calls), (1, [], [], [], []))
        self.assertIn("runs only in dev time", err)

    def test_every_dev_time_command_is_refused_with_the_flag(self) -> None:
        self.fetched()
        self.flag()
        before = len(self.gh.calls)
        for argv in (["install", "--sha", SHA], ["install", "--sha", SHA, "--first"], ["dispatch-hil", "--sha", SHA],
                     ["rehearse-teardown"], ["probe-task"], ["handover-s1a"], ["bootstrap", "Register-IemTasks"],
                     ["bootstrap", "Get-IemBootstrapState"], ["bootstrap", "Get-IemTunnelOrigin"],
                     ["bootstrap", "Test-IemServiceRight"], ["dev", "--dry-run"]):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("runs only in dev time", err, argv)
        self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps, len(self.gh.calls)), ([], [], [], before))

    def test_event_goes_through_with_the_flag(self) -> None:
        self.flag()
        code, docs, _ = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event"], "ignore")])
        self.assertNotIn("flag", docs[0])

    def test_event_writes_the_flag_before_anything_else(self) -> None:
        seen = []
        self.pc.replies[("event",)] = lambda: (seen.append(ip.event_now()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, docs[0]), (0, [True], {"flag": str(ip.EVENT_NOW), "written": True}))
        self.assertRegex(ip.EVENT_NOW.read_text(encoding="utf-8"), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d[+-]\d\d:\d\d\n$")

    def test_an_existing_flag_is_kept(self) -> None:
        self.flag()
        self.assertFalse(ip.ensure_flag())
        self.assertEqual(ip.EVENT_NOW.read_text(encoding="utf-8"), "2026-09-27T20:00:00+02:00\n")

    def test_a_flag_that_cannot_be_written_never_stops_the_event_path(self) -> None:
        blocker = self.tmp / "not-a-folder"
        blocker.write_text("a file where the flag's folder should be", encoding="utf-8")
        ip.EVENT_NOW = blocker / "EVENT-NOW"
        self.open_window()
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual({k: docs[0][k] for k in ("flag", "written")}, {"flag": str(ip.EVENT_NOW), "written": False})
        self.assertIn("File exists", docs[0]["error"])
        self.assertEqual(self.spike_log.read_text(encoding="utf-8"), "preempt\n")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event"], "ignore")])
        self.assertIn("was not written", err)
        self.assertNotIn("alarm the owner", err)


class EventTests(Base):
    def test_an_open_spike_window_is_preempted_before_iemmode(self) -> None:
        self.open_window()
        seen = []
        self.pc.replies[("event",)] = lambda: (seen.append(self.spike_log.is_file()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, self.spike_log.read_text(encoding="utf-8")), (0, [True], "preempt\n"))
        self.assertEqual(docs[1], {"spike_preempt": {"ok": True, "output": "preempted\n"}})

    def test_a_closed_or_absent_window_is_left_alone(self) -> None:
        for state in (None, {"closed": True}):
            if state is not None:
                self.open_window(**state)
            self.assertEqual(self.run_main("event")[0], 0)
        self.assertFalse(self.spike_log.exists())
        self.assertEqual(len(self.pc.calls), 2)

    def test_a_failed_spike_preempt_still_runs_iemmode_event(self) -> None:
        self.open_window()
        ip.SPIKE = self.write_spike(3)
        code, docs, err = self.run_main("event")
        self.assertEqual((code, docs[1]["spike_preempt"]["ok"], "running" in docs[1]["spike_preempt"]), (0, False, False))
        self.assertIn("exit 3", docs[1]["spike_preempt"]["error"])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event"], "ignore")])
        self.assertIn("spike preempt", err)

    def test_the_event_path_has_one_budget_that_fits_a_bash_call(self) -> None:
        self.assertLessEqual(ip.EVENT_BUDGET_S, 540)  # a Bash call ends at 10 min; the plan's waits stay within 9
        self.assertLessEqual(ip.SPIKE_SHARE_S + ip.SWITCH_MIN_S, ip.EVENT_BUDGET_S)
        self.pc.replies[("event",)] = (4, OK)
        self.assertEqual(self.run_main("event")[0], 0)
        first, direct = self.pc.timeouts
        self.assertLessEqual(first, ip.EVENT_BUDGET_S)
        self.assertGreater(first, ip.EVENT_BUDGET_S - 10)
        self.assertLess(direct, first)

    def test_iemmode_event_gets_what_the_spike_preempt_left(self) -> None:
        self.open_window()
        ip.SPIKE = self.write_spike(0, delay=0.4)
        self.assertEqual(self.run_main("event")[0], 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event"], "ignore")])
        self.assertLessEqual(self.pc.timeouts[0], ip.EVENT_BUDGET_S - 0.4)
        self.assertGreater(self.pc.timeouts[0], ip.EVENT_BUDGET_S - 10)

    def test_no_iemmode_call_while_the_spike_preempt_outlives_its_share(self) -> None:
        self.open_window()
        ip.SPIKE = self.write_spike(0, delay=3)
        ip.SPIKE_SHARE_S = 0.3
        t = time.monotonic()
        code, docs, err = self.run_main("event")
        spike_still_runs = not self.spike_done.exists()
        self.assertTrue(spike_still_runs)
        self.assertLess(time.monotonic() - t, 2.0)
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertEqual((docs[1]["spike_preempt"]["ok"], docs[1]["spike_preempt"]["running"]), (False, True))
        self.assertIn("preempt still runs after its 0.3 s share", err)
        self.assertIn("alarm the owner now", err)
        self.wait_for_spike_end()
        self.assertEqual(self.pc.calls, [])

    def test_an_iemmode_call_never_starts_with_less_than_its_minimum(self) -> None:
        ip.EVENT_BUDGET_S, ip.SWITCH_MIN_S = 1.0, 0.5
        self.pc.replies[("event",)] = lambda: (time.sleep(0.6), (4, OK))[1]
        code, _, err = self.run_main("event")
        self.assertEqual((code, [c[1] for c in self.pc.calls]), (1, [["event"]]))
        self.assertIn("less than the 0.5 s an iemmode call gets: run 'iempc event' again", err)
        self.assertIn("alarm the owner now", err)
        self.pc.calls.clear()
        self.pc.replies[("event",)] = lambda: (time.sleep(0.1), (4, OK))[1]
        self.assertEqual(self.run_main("event")[0], 0)
        self.assertEqual([c[1] for c in self.pc.calls], [["event"], ["event", "--direct"]])

    def test_an_unreachable_guard_falls_back_to_direct(self) -> None:
        self.pc.replies[("event",)] = (4, json.dumps({"error": "guard unreachable"}))
        code, docs, _ = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event"], "ignore"), ("iemmode.exe", ["event", "--direct"], "ignore")])
        self.assertEqual([d.get("iemmode") for d in docs[1:]], [["event"], ["event", "--direct"]])

    def test_only_exit_4_falls_back(self) -> None:
        for exit_code in (1, 2, 3, 5, 70):
            self.pc.calls.clear()
            self.pc.replies[("event",)] = (exit_code, OK)
            code, _, err = self.run_main("event")
            self.assertEqual((code, self.pc.calls), (exit_code, [("iemmode.exe", ["event"], "ignore")]), exit_code)
            self.assertIn("the event path did not complete", err)

    def test_a_failed_direct_event_is_an_owner_alarm(self) -> None:
        self.pc.replies[("event",)] = (4, OK)
        self.pc.replies[("event", "--direct")] = (1, json.dumps({"error": "a guard runs; use the pipe"}))
        code, _, err = self.run_main("event")
        self.assertEqual((code, len(self.pc.calls)), (1, 2))
        self.assertIn("alarm the owner now", err)

    def test_an_unreachable_pc_is_an_owner_alarm(self) -> None:
        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        self.pc.replies[("event",)] = down
        code, _, err = self.run_main("event")
        self.assertEqual(code, 1)
        self.assertIn("no route", err)
        self.assertIn("alarm the owner now", err)

    def test_a_dry_run_writes_no_flag_and_preempts_nothing(self) -> None:
        self.open_window()
        self.pc.replies[("event", "--dry-run")] = (4, OK)
        code, docs, err = self.run_main("event", "--dry-run")
        self.assertEqual((code, ip.event_now(), self.spike_log.exists()), (0, False, False))
        self.assertEqual(docs[0], {"spike_window": "open", "plan": "spike_window.py preempt"})
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--dry-run"], "ignore"),
                                         ("iemmode.exe", ["event", "--dry-run", "--direct"], "ignore")])
        self.assertNotIn("alarm the owner", err)

    def test_a_failed_dry_run_is_no_owner_alarm(self) -> None:
        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        self.pc.replies[("event", "--dry-run")] = down
        code, _, err = self.run_main("event", "--dry-run")
        self.assertEqual((code, ip.event_now()), (1, False))
        self.assertIn("no route", err)
        self.assertNotIn("alarm the owner", err)


class PreemptionTests(Base):
    """"ide event" while another command waits: the flag file appears."""

    def test_a_new_flag_during_dev_abandons_the_client_and_runs_the_event_path(self) -> None:
        """The guard owns the switch and pre-empts it itself: the client is
        abandoned (a wait sees the flag within a poll, GuardedTests)."""
        self.pc.replies[("dev", "--build", SHA)] = lambda: (self.flag(), (0, OK))[1]
        code, docs, _ = self.run_main("dev", "--build", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["dev", "--build", SHA], "abandon"), ("iemmode.exe", ["event"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual(ip.current_entry(), 0)

    def test_a_failed_step_after_a_new_flag_runs_the_event_path(self) -> None:
        def fail():
            self.flag()
            raise ip.StepError("ssh: connection reset")

        self.pc.replies[("rehearse-teardown",)] = fail
        code, docs, _ = self.run_main("rehearse-teardown")
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(docs[0]["event"], "ide event (flag file) after a failed step")
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))

    def test_a_failed_step_without_the_flag_does_not(self) -> None:
        def fail():
            raise ip.StepError("ssh: connection reset")

        self.pc.replies[("rehearse-teardown",)] = fail
        code, docs, _ = self.run_main("rehearse-teardown")
        self.assertEqual((code, docs, len(self.pc.calls)), (1, [], 1))

    def test_a_read_only_call_started_during_an_event_is_not_preempted_again(self) -> None:
        self.flag()

        def fail():
            raise ip.StepError("ssh: timeout")

        self.pc.replies[("status",)] = fail
        code, docs, _ = self.run_main("status", "--pc")
        self.assertEqual((code, docs, len(self.pc.calls)), (1, [], 1))

    def test_a_dev_box_command_never_starts_the_event_path(self) -> None:
        self.gh.runs = []
        self.gh.on_list = self.flag
        code, _, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertIn("no green push run", err)

    def test_a_failed_event_after_a_preemption_is_reported(self) -> None:
        self.pc.replies[("probe-task",)] = lambda: (self.flag(), (0, OK))[1]
        self.pc.replies[("event",)] = (1, OK)
        code, _, err = self.run_main("probe-task")
        self.assertEqual(code, 1)
        self.assertIn("alarm the owner now", err)


class DevTests(Base):
    def test_every_successful_dev_opens_a_new_entry(self) -> None:
        code, docs, _ = self.run_main("dev", "--build", SHA)
        self.assertEqual((code, docs[-1]["dev_entry"], ip.current_entry()), (0, 1, 1))
        self.assertEqual(self.run_main("dev")[1][-1]["dev_entry"], 2)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["dev"], "abandon"))
        self.assertEqual(self.pc.timeouts, [ip.SWITCH_S, ip.SWITCH_S])

    def test_an_open_spike_window_refuses_dev_and_the_rehearsal(self) -> None:
        self.open_window()
        for argv in (["dev", "--build", SHA], ["dev", "--dry-run"], ["rehearse-teardown"]):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("'iempc handover-s1a' has handed the card over", err, argv)
        self.assertEqual(self.pc.calls, [])
        self.open_window(closed=True)
        self.assertEqual((self.run_main("dev", "--build", SHA)[0], self.run_main("rehearse-teardown")[0]), (0, 0))
        self.assertEqual([c[1][0] for c in self.pc.calls], ["dev", "rehearse-teardown"])

    def test_a_refused_dev_or_a_dry_run_opens_none(self) -> None:
        self.pc.replies[("dev", "--build", SHA)] = (1, json.dumps({"error": "band activity"}))
        code, docs, _ = self.run_main("dev", "--build", SHA)
        self.assertEqual((code, "dev_entry" in docs[-1], ip.current_entry()), (1, False, 0))
        code, docs, _ = self.run_main("dev", "--build", SHA, "--dry-run")
        self.assertEqual((code, "dev_entry" in docs[-1], ip.current_entry()), (0, False, 0))
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["dev", "--build", SHA, "--dry-run"], "abandon"))
        self.assertEqual(self.pc.timeouts[-1], ip.STATUS_S)

    def test_a_bad_build_is_refused_before_the_pc(self) -> None:
        for bad in ("1234", SHA.upper(), SHA + "0"):
            self.assertEqual(self.run_main("dev", "--build", bad)[0], 1, bad)
        self.assertEqual(self.pc.calls, [])

    def test_rehearse_teardown_and_probe_task_are_plain_guard_requests(self) -> None:
        self.assertEqual(self.run_main("rehearse-teardown")[0], 0)
        self.pc.replies[("probe-task",)] = (1, json.dumps({"error": "refused"}))
        self.assertEqual(self.run_main("probe-task")[0], 1)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["rehearse-teardown"], "abandon"), ("iemmode.exe", ["probe-task"], "finish")])


class BundleTests(Base):
    def zip(self, **kw) -> Path:
        return make_zip(self.tmp / "z" / "b.zip", **kw)

    def test_a_complete_bundle_verifies_with_its_tuning_folder(self) -> None:
        sums = ip.verify_zip(self.zip(), SHA, "dev", RUN)
        self.assertEqual(sorted(sums), sorted([*ip.BUNDLE_REQUIRED, "tuning/state.ps1"]))
        self.assertEqual(sums["iemmode.exe"], sha256(b"synthetic iemmode.exe"))

    def test_backslash_entry_names_are_read_as_folders(self) -> None:
        self.assertIn("tuning/state.ps1", ip.verify_zip(self.zip(rename={"tuning/state.ps1": "tuning\\state.ps1"}), SHA, "dev", RUN))

    def test_a_changed_unlisted_or_absent_file_is_refused(self) -> None:
        for kw, words in (({"tamper": "iem-engine.exe"}, "iem-engine.exe does not match"),
                          ({"unlisted": "extra.exe"}, "present but unlisted \\['extra.exe'\\]"),
                          ({"sums_extra": "ab" * 32 + "  ghost.exe\n"}, "listed but absent \\['ghost.exe'\\]"),
                          ({"drop": ("iemmode.exe",)}, "required files missing: \\['iemmode.exe'\\]")):
            with self.assertRaisesRegex(ip.StepError, words, msg=str(kw)):
                ip.verify_zip(self.zip(**kw), SHA, "dev", RUN)

    def test_the_manifest_must_name_this_sha_branch_and_run(self) -> None:
        good = {"sha": SHA, "branch": "dev", "run": RUN}
        self.assertEqual(len(ip.verify_zip(self.zip(manifest=dict(good, run=str(RUN))), SHA, "dev", RUN)), 10)
        for change in ({"sha": SHA2}, {"branch": "main"}, {"run": RUN + 1}):
            with self.assertRaisesRegex(ip.StepError, "manifest.json names", msg=str(change)):
                ip.verify_zip(self.zip(manifest=dict(good, **change)), SHA, "dev", RUN)

    def test_names_that_leave_the_bundle_are_refused(self) -> None:
        for bad in ("../x.exe", "/abs.exe", "a/../b", "a/b/c.exe", "C:x.exe", "./a", "a\\b", ""):
            with self.assertRaises(ip.StepError, msg=bad):
                ip.check_member(bad)
        with self.assertRaisesRegex(ip.StepError, "bundle entry refused"):
            ip.verify_zip(self.zip(rename={"hil-v1.ps1": "../hil-v1.ps1"}), SHA, "dev", RUN)

    def test_sums_lines(self) -> None:
        self.assertEqual(ip.parse_sums("\n" + "a" * 64 + "  tuning/x.ps1\n"), {"tuning/x.ps1": "a" * 64})
        for bad in ("a" * 64 + " one-space.exe", "xyz  a.exe", "a" * 64 + "  ../x.exe", ("a" * 64 + "  x\n") * 2):
            with self.assertRaises(ip.StepError, msg=bad):
                ip.parse_sums(bad)

    def test_an_unreadable_member_is_named(self) -> None:
        for kw, member in (({"manifest_raw": b"{not json"}, "manifest.json"),
                           ({"manifest_raw": b"\xff\xfe not utf-8"}, "manifest.json"),
                           ({"sums_raw": b"\xff" * 70}, "SHA256SUMS")):
            with self.assertRaisesRegex(ip.StepError, f"b.zip: {member} is unreadable", msg=str(kw)):
                ip.verify_zip(self.zip(**kw), SHA, "dev", RUN)

    def test_a_member_with_a_bad_crc_is_named(self) -> None:
        p = self.zip()
        with zipfile.ZipFile(p) as z:
            info = z.getinfo("iemmode.exe")
        data = bytearray(p.read_bytes())
        data[info.header_offset + 30 + len(info.filename.encode()) + len(info.extra)] ^= 0xFF
        p.write_bytes(bytes(data))
        with self.assertRaisesRegex(ip.StepError, "b.zip: iemmode.exe is unreadable \\(BadZipFile: Bad CRC-32"):
            ip.verify_zip(p, SHA, "dev", RUN)

    def test_a_file_that_is_no_zip_is_refused(self) -> None:
        p = self.tmp / "no.zip"
        p.write_bytes(b"not a zip")
        with self.assertRaisesRegex(ip.StepError, "not a zip"):
            ip.verify_zip(p, SHA, "dev", RUN)

    def test_only_a_green_push_run_of_that_sha_on_dev_or_main(self) -> None:
        runs = [
            {"databaseId": 1, "headSha": SHA, "event": "pull_request", "headBranch": "dev", "conclusion": "success"},
            {"databaseId": 2, "headSha": SHA, "event": "push", "headBranch": "dev", "conclusion": "failure"},
            {"databaseId": 3, "headSha": SHA2, "event": "push", "headBranch": "dev", "conclusion": "success"},
            {"databaseId": 4, "headSha": SHA, "event": "push", "headBranch": "feature", "conclusion": "success"},
            {"databaseId": 5, "headSha": SHA, "event": "push", "headBranch": "main", "conclusion": "success"},
            {"databaseId": 6, "headSha": SHA, "event": "push", "headBranch": "dev", "conclusion": "success"},
        ]
        self.assertEqual([r["databaseId"] for r in ip.pick_runs(runs, SHA, ip.BRANCHES)], [5, 6])
        self.assertEqual([r["databaseId"] for r in ip.pick_runs(runs, SHA, ("dev",))], [6])

    def test_the_bundle_and_attest_jobs_must_have_succeeded(self) -> None:
        self.assertTrue(ip.job_ok([{"name": "attest", "conclusion": "success"}], "attest"))
        for jobs in ([{"name": "attest", "conclusion": "skipped"}], [{"name": "bundle", "conclusion": "success"}], []):
            self.assertFalse(ip.job_ok(jobs, "attest"), jobs)


class FetchTests(Base):
    def test_a_green_attested_bundle_is_fetched_and_recorded(self) -> None:
        doc = self.fetched()
        digest = "sha256:" + sha256(self.artifact.read_bytes())
        self.assertEqual({k: doc[k] for k in ("sha", "branch", "run", "digest", "fetched")},
                         {"sha": SHA, "branch": "dev", "run": RUN, "digest": digest, "fetched": True})
        partial_zip = str(ip.STATE_DIR / "bundles" / f"{SHA}.partial" / f"iemmixer-{SHA}.zip")
        self.assertEqual(self.gh.named("attestation"), [[
            "attestation", "verify", partial_zip, "-R", ip.REPO, "--signer-workflow", f"{ip.REPO}/.github/workflows/ci.yml",
            "--source-ref", "refs/heads/dev", "--deny-self-hosted-runners"]])
        self.assertEqual(self.gh.named("run", "download"), [[
            "run", "download", str(RUN), "-R", ip.REPO, "-n", f"iemmixer-bundle-{SHA}", "-D",
            str(ip.STATE_DIR / "bundles" / f"{SHA}.partial")]])
        self.assertEqual(ip.load_record(SHA)["digest"], digest)
        self.assertEqual(ip.zip_path(SHA).read_bytes(), self.artifact.read_bytes())
        self.assertFalse((ip.STATE_DIR / "bundles" / f"{SHA}.partial").exists())
        self.assertEqual(os.stat(ip.STATE_DIR).st_mode & 0o777, 0o700)

    def test_a_fetched_bundle_is_reused_only_while_its_digest_holds(self) -> None:
        self.fetched()
        code, docs, _ = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, docs[-1]["fetched"], len(self.gh.named("run", "download"))), (0, False, 1))
        with open(ip.zip_path(SHA), "ab") as f:
            f.write(b"x")
        code, docs, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, docs), (1, []))
        self.assertIn("differs from the fetched", err)

    def test_a_run_whose_attest_job_did_not_succeed_is_refused(self) -> None:
        self.gh.jobs[RUN][1]["conclusion"] = "skipped"
        code, _, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, self.gh.named("run", "download")), (1, []))
        self.assertIn("'attest' jobs succeeded (P5)", err)

    def test_a_failed_attestation_keeps_nothing(self) -> None:
        self.gh.attest_ok = False
        code, _, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("no attestation matched", err)
        self.assertEqual(sorted(p.name for p in (ip.STATE_DIR / "bundles").iterdir()), [])

    def test_a_tampered_artifact_is_refused_before_the_attestation(self) -> None:
        make_zip(self.artifact, tamper="iemmode.exe")
        code, _, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, self.gh.named("attestation")), (1, []))
        self.assertIn("iemmode.exe does not match SHA256SUMS", err)
        self.assertIsNone(ip.load_record(SHA))

    def test_an_artifact_without_the_zip_is_refused(self) -> None:
        self.gh.artifact = make_zip(self.tmp / "other" / "wrong-name.zip")
        code, _, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn(f"has no iemmixer-{SHA}.zip", err)

    def test_a_malformed_manifest_is_a_message_and_keeps_nothing(self) -> None:
        make_zip(self.artifact, manifest_raw=b"{not json")
        code, docs, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, docs, self.gh.named("attestation")), (1, [], []))
        self.assertIn(f"iemmixer-{SHA}.zip: manifest.json is unreadable (JSONDecodeError", err)
        self.assertEqual(list((ip.STATE_DIR / "bundles").iterdir()), [])

    def test_a_bundle_folder_without_a_fetch_record_is_refused(self) -> None:
        ip.bundle_dir(SHA).mkdir(parents=True)
        code, _, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, self.gh.named("run", "download")), (1, []))
        self.assertIn("exists without a fetch record", err)

    def test_a_stale_partial_download_is_replaced(self) -> None:
        stale = ip.STATE_DIR / "bundles" / f"{SHA}.partial"
        stale.mkdir(parents=True)
        (stale / "left-over.zip").write_bytes(b"from a cut download")
        self.fetched()
        self.assertFalse(stale.exists())
        self.assertEqual(sorted(p.name for p in ip.bundle_dir(SHA).iterdir()), ["fetch.json", f"iemmixer-{SHA}.zip"])

    def test_a_fetched_bundle_is_reused_only_for_its_own_branch(self) -> None:
        self.fetched()
        with self.assertRaisesRegex(ip.StepError, "was fetched from dev, not main"):
            ip.fetch_bundle(SHA, "main")
        self.assertEqual(ip.fetch_bundle(SHA, "dev")[1], False)
        self.assertEqual(len(self.gh.named("run", "download")), 1)


class InstallTests(Base):
    def test_install_needs_a_fetched_bundle_with_its_digest(self) -> None:
        code, _, err = self.run_main("install", "--sha", SHA)
        self.assertEqual((code, self.pc.scps, self.pc.calls), (1, [], []))
        self.assertIn("is not fetched", err)
        self.fetched()
        with open(ip.zip_path(SHA), "ab") as f:
            f.write(b"x")
        code, _, err = self.run_main("install", "--sha", SHA)
        self.assertEqual((code, self.pc.scps, self.pc.calls), (1, [], []))
        self.assertIn("differs from the fetched", err)

    def test_install_copies_the_zip_and_iemmode_installs_it_after_a_pc_side_hash_check(self) -> None:
        digest = self.fetched()["digest"]
        code, docs, _ = self.run_main("install", "--sha", SHA)
        pc_zip = f"X:\\root\\incoming\\iemmixer-{SHA}.zip"
        self.assertEqual(code, 0)
        self.assertIn("New-Item -ItemType Directory -Force -Path 'X:\\root\\incoming'", self.pc.modules[0][0])
        self.assertEqual(self.pc.scps, [(str(ip.zip_path(SHA)), f"tester@pc.test:/X:/root/incoming/iemmixer-{SHA}.zip", "finish")])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["install", pc_zip], "finish")])
        self.assertIn(ip.hash_check(pc_zip, digest.split(":")[1]) + " ; $x = 'X:\\root\\bin\\iemmode.exe'", self.pc.native_scripts[0])
        self.assertEqual((docs[-1]["install"], docs[-1]["via"]), ([SHA], "iemmode"))

    def test_the_first_bundle_is_installed_by_its_own_guard(self) -> None:
        self.fetched()
        self.pc.replies[("install", f"X:\\root\\incoming\\iemmixer-{SHA}.zip")] = (0, "installed")
        code, docs, _ = self.run_main("install", "--sha", SHA, "--first")
        guard = f"X:\\root\\incoming\\iemmixer-guard-{SHA}.exe"
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.scps[1], (str(ip.bundle_dir(SHA) / "iemmixer-guard.exe"),
                                           f"tester@pc.test:/X:/root/incoming/iemmixer-guard-{SHA}.exe", "finish"))
        self.assertEqual(self.pc.calls[0][0], f"iemmixer-guard-{SHA}.exe")
        self.assertIn(ip.hash_check(guard, sha256(b"synthetic iemmixer-guard.exe")), self.pc.native_scripts[0])
        self.assertEqual((docs[-1]["via"], docs[-1]["output"]), ("iemmixer-guard (first bundle)", "installed"))

    def test_an_open_spike_window_refuses_install_but_not_the_first_bundle(self) -> None:
        """`iemmode install` may start the guard; the first bundle's own guard
        only installs files (no guard run, no card)."""
        self.fetched()
        self.open_window()
        code, docs, err = self.run_main("install", "--sha", SHA)
        self.assertEqual((code, docs, self.pc.scps, self.pc.calls, self.pc.modules), (1, [], [], [], []))
        self.assertIn("'install' waits until 'iempc handover-s1a' has handed the card over", err)
        self.pc.replies[("install", f"X:\\root\\incoming\\iemmixer-{SHA}.zip")] = (0, "installed")
        self.assertEqual(self.run_main("install", "--sha", SHA, "--first")[0], 0)
        self.assertEqual(self.pc.calls[0][0], f"iemmixer-guard-{SHA}.exe")


class DispatchTests(Base):
    def test_a_sha_that_is_no_branch_head_is_refused(self) -> None:
        self.gh.heads = {"dev": SHA2, "main": SHA2}
        code, _, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual((code, self.gh.named("workflow")), (1, []))
        self.assertIn("is not the head of dev or main", err)

    def test_a_head_without_a_green_run_is_refused(self) -> None:
        self.gh.runs[0]["conclusion"] = "failure"
        code, _, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual((code, self.gh.named("workflow")), (1, []))
        self.assertIn("no green push run", err)

    def test_hil_is_dispatched_once_per_sha_per_dev_entry(self) -> None:
        code, docs, _ = self.run_main("dispatch-hil", "--sha", SHA)
        digest = "sha256:" + sha256(self.artifact.read_bytes())
        self.assertEqual(code, 0)
        self.assertEqual(self.gh.named("workflow"), [["workflow", "run", "hil.yml", "-R", ip.OPS_REPO, "-f", f"sha={SHA}",
                                                      "-f", "branch=dev", "-f", f"run={RUN}", "-f", f"digest={digest}"]])
        self.assertEqual({k: docs[-1]["dispatched"][k] for k in ("sha", "branch", "run", "digest", "entry")},
                         {"sha": SHA, "branch": "dev", "run": RUN, "digest": digest, "entry": 0})
        code, _, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual((code, len(self.gh.named("workflow"))), (1, 1))
        self.assertIn("already dispatched in dev entry 0", err)
        ip.next_entry(SHA)
        self.assertEqual(self.run_main("dispatch-hil", "--sha", SHA)[0], 0)
        self.assertEqual(len(self.gh.named("workflow")), 2)

    def test_the_default_sha_is_the_head_of_dev(self) -> None:
        code, docs, _ = self.run_main("dispatch-hil")
        self.assertEqual((code, docs[-1]["dispatched"]["sha"]), (0, SHA))

    def test_the_head_of_main_dispatches_with_branch_main(self) -> None:
        self.gh.heads = {"dev": SHA2, "main": SHA}
        self.gh.runs = [{"databaseId": RUN, "headSha": SHA, "event": "push", "headBranch": "main", "conclusion": "success"}]
        make_zip(self.artifact, branch="main")
        code, docs, _ = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual((code, docs[-1]["dispatched"]["branch"]), (0, "main"))
        self.assertIn("branch=main", self.gh.named("workflow")[0])
        self.assertEqual(self.gh.named("attestation")[0][-2], "refs/heads/main")

    def test_a_bundle_fetched_from_another_run_is_refused(self) -> None:
        self.fetched()
        self.gh.runs[0]["databaseId"] = RUN + 1
        self.gh.jobs[RUN + 1] = self.gh.jobs[RUN]
        code, _, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual((code, self.gh.named("workflow")), (1, []))
        self.assertIn(f"fetched from run {RUN}", err)

    def test_a_malformed_branch_head_is_refused(self) -> None:
        for bad in ("", "not a sha", SHA.upper(), SHA + "0"):
            self.gh.heads["dev"] = bad
            code, _, err = self.run_main("dispatch-hil")
            self.assertEqual(code, 1, bad)
            self.assertIn(f"the head of dev reads {bad!r}", err, bad)
        self.assertEqual(self.gh.named("workflow"), [])
        self.gh.heads["dev"] = SHA
        self.assertEqual(ip.branch_head("dev"), SHA)

    def test_a_flag_that_appears_during_the_gh_waits_stops_the_dispatch(self) -> None:
        self.gh.on_download = self.flag
        code, docs, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual((code, docs, self.gh.named("workflow"), self.pc.calls), (1, [], [], []))
        self.assertIn("no HIL dispatch during an event (nothing was dispatched)", err)
        self.assertEqual(ip.load_dispatches(), [])

    def test_a_failed_workflow_dispatch_through_the_real_gh_is_not_recorded(self) -> None:
        self.gh_program("sys.stderr.write('HTTP 422: Workflow does not have workflow_dispatch trigger'); sys.exit(1)")
        self.route_to_real_gh("workflow", "run")
        code, docs, err = self.run_main("dispatch-hil", "--sha", SHA)
        self.assertEqual((code, docs), (1, []))
        self.assertIn("gh workflow run failed (exit 1): HTTP 422", err)
        self.assertEqual(ip.load_dispatches(), [])
        ip.gh = self.gh  # gh works again: the same SHA and entry is no repeat
        self.assertEqual(self.run_main("dispatch-hil", "--sha", SHA)[0], 0)
        self.assertEqual(len(ip.load_dispatches()), 1)


class BootstrapTests(Base):
    MODULE = f"X:\\root\\bootstrap\\{SHA}\\IemPc.psm1"

    def test_functions_and_their_parameters(self) -> None:
        self.assertEqual(ip.ps_params(["-Service", "svc name", "-WhatIf", "-20"]), " -Service 'svc name' -WhatIf '-20'")
        for bad in (["--sha", SHA], ['-Name', 'a"b']):
            with self.assertRaises(ip.Refused, msg=str(bad)):
                ip.ps_params(bad)
        self.assertEqual([ip.read_only_function(f) for f in ("Get-IemBootstrapState", "Test-IemServiceRight",
                                                               "Grant-IemServiceRight", "Register-IemTasks", "Get-Process")],
                         [True, True, False, False, False])

    def test_every_bootstrap_step_is_dev_time_and_locked(self) -> None:
        """Read-only functions too: each run makes a folder, copies the module
        and runs it elevated on the PC (design §6, P10)."""
        spec = ip.COMMANDS["bootstrap"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))

    def test_the_verified_module_is_shipped_and_imported_by_its_hash(self) -> None:
        self.fetched()
        self.pc.module_result = {"reaper": 1}
        code, docs, _ = self.run_main("bootstrap", "Grant-IemServiceRight", "-Service", "svc name")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.scps, [(str(ip.bundle_dir(SHA) / "IemPc.psm1"),
                                         f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemPc.psm1", "finish")])
        script, mode = self.pc.modules[-1]
        self.assertEqual(mode, "finish")
        self.assertIn(ip.hash_check(self.MODULE, sha256(b"synthetic IemPc.psm1")) + f" ; Import-Module '{self.MODULE}' -Force ; "
                      "$r = & { Grant-IemServiceRight -Service 'svc name' }", script)
        self.assertEqual(docs[-1], {"bootstrap": "Grant-IemServiceRight", "sha": SHA, "result": {"reaper": 1}})

    def test_a_read_only_step_is_refused_during_an_event(self) -> None:
        self.fetched()
        self.flag()
        for fn in ("Get-IemBootstrapState", "Get-IemPredecessorFacts", "Test-IemServiceRight"):
            code, docs, err = self.run_main("bootstrap", fn)
            self.assertEqual((code, docs), (1, []), fn)
            self.assertIn("runs only in dev time", err, fn)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_a_new_flag_abandons_a_read_only_step(self) -> None:
        self.fetched()
        self.assertEqual(self.run_main("bootstrap", "Get-IemBootstrapState")[0], 0)
        self.assertEqual(({m for _, m in self.pc.modules}, {s[2] for s in self.pc.scps}), ({"abandon"}, {"abandon"}))

    def test_the_runner_token_reaches_the_pc_on_stdin_only(self) -> None:
        self.fetched()
        code, _, _ = self.run_main("bootstrap", "Register-IemRunner")
        self.assertEqual(code, 0)
        self.assertEqual(self.gh.named("api", "-X"), [["api", "-X", "POST", f"repos/{ip.OPS_REPO}/actions/runners/registration-token",
                                                       "--jq", ".token"]])
        script = self.pc.modules[-1][0]
        self.assertIn(f"try {{ $env:ACTIONS_RUNNER_INPUT_TOKEN = '{self.gh.runner_reply}' ; ", script)
        self.assertIn("finally { Remove-Item -Path 'Env:\\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue }", script)
        self.assertFalse(any(self.gh.runner_reply in " ".join(s) for s in self.pc.scps))
        self.gh.runner_reply = "an unexpected form!"
        before = (len(self.pc.modules), len(self.pc.scps))
        self.assertEqual(self.run_main("bootstrap", "Register-IemRunner")[0], 1)
        self.assertEqual((len(self.pc.modules), len(self.pc.scps)), before)  # refused before the PC

    def test_unknown_functions_and_missing_bundles_are_refused(self) -> None:
        code, _, err = self.run_main("bootstrap", "Get-IemBootstrapState")
        self.assertEqual((code, self.pc.modules), (1, []))
        self.assertIn("no fetched bundle", err)
        self.fetched()
        for argv in (["bootstrap", "Invoke-Expression"], ["bootstrap", "Get-IemX", "--sha", SHA]):
            self.assertEqual(self.run_main(*argv)[0], 1, argv)
        self.assertEqual(self.pc.modules, [])

    def test_the_newest_fetched_bundle_is_the_default(self) -> None:
        self.fetched()
        self.gh.artifact = make_zip(self.tmp / "artifact2" / f"iemmixer-{SHA2}.zip", sha=SHA2, run=RUN + 1)
        self.gh.runs.append({"databaseId": RUN + 1, "headSha": SHA2, "event": "push", "headBranch": "dev", "conclusion": "success"})
        self.gh.jobs[RUN + 1] = self.gh.jobs[RUN]
        self.assertEqual(self.run_main("fetch-bundle", "--sha", SHA2)[0], 0)
        for older, newer in ((SHA, SHA2), (SHA2, SHA)):
            for sha, at in ((older, "2026-01-01T00:00:00+00:00"), (newer, "2026-09-27T12:00:00+00:00")):
                rec = ip.load_record(sha)
                rec["fetched_at"] = at
                ip.write_json(ip.bundle_dir(sha) / "fetch.json", rec)
            self.assertEqual(ip.latest_record_sha(), newer)


class ExtractTests(Base):
    """extract_member: one top-level file of the fetched zip, checked again."""

    def test_only_a_listed_top_level_file_is_extracted(self) -> None:
        self.fetched()
        rec = ip.load_record(SHA)
        for name in ("tuning/state.ps1", "absent.exe"):
            with self.assertRaisesRegex(ip.StepError, "is not a listed top-level file", msg=name):
                ip.extract_member(SHA, rec, name)
        path, hexd = ip.extract_member(SHA, rec, "IemPc.psm1")
        self.assertEqual((path, path.read_bytes(), hexd),
                         (ip.bundle_dir(SHA) / "IemPc.psm1", b"synthetic IemPc.psm1", sha256(b"synthetic IemPc.psm1")))

    def test_a_member_that_does_not_match_its_sum_is_refused(self) -> None:
        self.fetched()
        rec = ip.load_record(SHA)
        rec["sums"]["IemPc.psm1"] = "0" * 64
        with self.assertRaisesRegex(ip.StepError, f"IemPc.psm1 in iemmixer-{SHA}.zip does not match SHA256SUMS"):
            ip.extract_member(SHA, rec, "IemPc.psm1")
        self.assertFalse((ip.bundle_dir(SHA) / "IemPc.psm1").exists())

    def test_a_member_absent_or_there_twice_is_refused(self) -> None:
        want = sha256(b"synthetic iemmode.exe")
        for copies in (0, 2):
            z = ip.zip_path(SHA)
            z.parent.mkdir(parents=True, exist_ok=True)
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")  # zipfile warns about a duplicate name
                with zipfile.ZipFile(z, "w") as zf:
                    zf.writestr("other.exe", b"x")
                    for _ in range(copies):
                        zf.writestr("iemmode.exe", b"synthetic iemmode.exe")
            rec = {"digest": "sha256:" + sha256(z.read_bytes()), "sums": {"iemmode.exe": want}}
            with self.assertRaisesRegex(ip.StepError, f"iemmode.exe is not in iemmixer-{SHA}.zip exactly once", msg=copies):
                ip.extract_member(SHA, rec, "iemmode.exe")


class HandoverTests(Base):
    def setUp(self) -> None:
        super().setUp()
        self.sw = ip.spike_module()
        saved = {n: getattr(self.sw, n) for n in ("STATE", "load_env", "ps")}
        self.addCleanup(lambda: [setattr(self.sw, n, v) for n, v in saved.items()])
        self.sw.STATE = ip.SPIKE_STATE
        self.sw.load_env = lambda path: {"PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_ASIO_MODULE": "testcard.dll"}
        self.checks = {"pref": 64, "holders": [], "spike": 0, "task": False}
        self.bodies: list[tuple[str, str]] = []

        def fake_ps(env, body, timeout=300, event="finish"):
            self.bodies.append((body, event))
            return dict(self.checks)

        self.sw.ps = fake_ps

    def state(self) -> dict:
        return json.loads(ip.SPIKE_STATE.read_text(encoding="utf-8"))

    def test_a_free_quiet_card_closes_the_window(self) -> None:
        self.open_window(pref_current=64)
        code, docs, _ = self.run_main("handover-s1a")
        self.assertEqual((code, self.state()["closed"], self.state()["handed_over"]["checks"]), (0, True, self.checks))
        body, mode = self.bodies[0]
        self.assertEqual(mode, "abandon")
        self.assertIn("Get-SpikeBufferPref -Key 'K' -Name 'N'", body)
        self.assertIn("Get-GoldenAsioHolders -Module 'testcard.dll'", body)
        self.assertIn("Get-ScheduledTask -TaskPath '\\iemmixer\\' -TaskName 'iemmixer-asio-spike'", body)
        self.assertEqual(docs[-1], {"handover-s1a": "w1", "closed": True, "checks": self.checks})

    def test_each_problem_keeps_the_window_open(self) -> None:
        self.open_window()
        for change, words in (({"pref": 32}, "reads 32, the original is 64"), ({"holders": ["testcard-host.exe:9"]}, "held"),
                              ({"spike": 1}, "a spike runs"), ({"task": True}, "the spike task runs")):
            self.checks = dict({"pref": 64, "holders": [], "spike": 0, "task": False}, **change)
            code, _, err = self.run_main("handover-s1a")
            self.assertEqual((code, self.state()["closed"]), (1, False), change)
            self.assertIn(words, err, change)

    def test_the_problems_on_both_sides(self) -> None:
        good = {"pref": 64, "holders": [], "spike": 0, "task": False}
        self.assertEqual(ip.handover_problems(good, 64), [])
        self.assertEqual(len(ip.handover_problems(good, 32)), 1)
        self.assertEqual(len(ip.handover_problems({"pref": 32, "holders": ["x:1"], "spike": 2, "task": True}, 64)), 4)

    def test_a_window_with_reaper_on_the_card_or_a_closed_one_is_not_handed_over(self) -> None:
        for kw, words in (({"card": "reaper"}, "not free"), ({"card": "switching"}, "not free"), ({"closed": True}, "already closed")):
            self.open_window(**kw)
            code, _, err = self.run_main("handover-s1a")
            self.assertEqual(code, 1, kw)
            self.assertIn(words, err, kw)
        self.assertEqual(self.bodies, [])

    def test_a_new_flag_during_the_checks_preempts_the_window(self) -> None:
        self.open_window()

        def flag_appears(env, body, timeout=300, event="finish"):
            self.flag()
            raise self.sw.EventNow()

        self.sw.ps = flag_appears
        code, _, _ = self.run_main("handover-s1a")
        self.assertEqual((code, self.spike_log.read_text(encoding="utf-8")), (ip.PREEMPTED, "preempt\n"))
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event"], "ignore")])
        self.assertFalse(self.state()["closed"])


class LockTests(Base):
    def test_a_second_changing_command_is_refused_while_one_runs_but_event_never_waits(self) -> None:
        with open(ip.state_dir() / "iempc.lock", "a+", encoding="utf-8") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            for argv in (["probe-task"], ["bootstrap", "Get-IemBootstrapState"]):
                code, _, err = self.run_main(*argv)
                self.assertEqual((code, self.pc.calls, self.pc.modules), (1, [], []), argv)
                self.assertIn("another iempc command runs", err, argv)
            self.assertEqual(self.run_main("status")[0], 0)
            self.assertEqual(self.run_main("event")[0], 0)
        ip.EVENT_NOW.unlink()  # "event skončil"
        self.assertEqual(self.run_main("probe-task")[0], 0)
        self.assertEqual([c[1] for c in self.pc.calls], [["status"], ["event"], ["probe-task"]])


class GhTests(Base):
    """The real gh wrapper, with a stand-in `gh` program first on PATH: the P5
    gate rests on its exit-code check."""

    def test_a_successful_call_returns_its_output(self) -> None:
        self.gh_program("sys.stderr.write('unknown command in a warning'); sys.stdout.write('args: ' + ' '.join(sys.argv[1:]))")
        self.assertEqual(REAL_GH(["run", "list", "--limit", "1"]), "args: run list --limit 1")

    def test_a_failed_call_raises_with_its_exit_and_stderr(self) -> None:
        self.gh_program("sys.stderr.write('HTTP 404: Not Found\\n'); sys.exit(1)")
        with self.assertRaises(ip.StepError) as cm:
            REAL_GH(["workflow", "run", "hil.yml"])
        self.assertEqual(str(cm.exception), "gh workflow run failed (exit 1): HTTP 404: Not Found")

    def test_a_gh_without_attestation_gets_the_hint(self) -> None:
        self.gh_program("sys.stderr.write('unknown command \"attestation\" for \"gh\"'); sys.exit(1)")
        with self.assertRaises(ip.StepError) as cm:
            REAL_GH(["attestation", "verify", "x.zip"])
        self.assertEqual(str(cm.exception), "gh attestation verify failed (exit 1) (this gh has no 'attestation' command: "
                                            "install gh >= 2.49): unknown command \"attestation\" for \"gh\"")

    def test_a_missing_gh_is_named(self) -> None:
        empty = self.tmp / "empty-bin"
        empty.mkdir()
        self.set_path(str(empty))
        with self.assertRaisesRegex(ip.StepError, "^gh is not installed on this box$"):
            REAL_GH(["run", "list"])

    def test_a_gh_that_does_not_answer_is_bounded(self) -> None:
        self.gh_program("time.sleep(5)")
        t = time.monotonic()
        with self.assertRaisesRegex(ip.StepError, "^gh run download: no answer within 0.3 s$"):
            REAL_GH(["run", "download", "1"], timeout=0.3)
        self.assertLess(time.monotonic() - t, 3)

    def test_a_failed_attestation_through_the_real_gh_keeps_nothing(self) -> None:
        self.gh_program("sys.stderr.write('no attestation matched the bundle'); sys.exit(1)")
        self.route_to_real_gh("attestation", "verify")
        code, docs, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, docs), (1, []))
        self.assertIn("gh attestation verify failed (exit 1): no attestation matched the bundle", err)
        self.assertIsNone(ip.load_record(SHA))
        self.assertEqual(list((ip.STATE_DIR / "bundles").iterdir()), [])
        self.assertEqual(len(self.gh.named("run", "download")), 1)


if __name__ == "__main__":
    unittest.main()
