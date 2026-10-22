"""Tests for scripts/iem-pc/iempc_roster.py: `iempc roster-install` (#58 plan
Task 14, design §9). FakePc stands in for ssh (its `replies` answer the
server's `roster check`, its `texts` the install and the status reads); the
roster is the synthetic config/test-roster.toml and every other value is
synthetic (P6)."""
from __future__ import annotations

import hashlib
import json
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_roster as roster  # noqa: E402
from iempc_test_support import SHA, Base, ip  # noqa: E402
from test_iempc_live import RunningGuardCases  # noqa: E402

TEXT = (Path(__file__).resolve().parents[2] / "config" / "test-roster.toml").read_bytes()
FILE_SHA = hashlib.sha256(TEXT).hexdigest()
OTHER_SHA = "0f" * 32
CONFIG = "X:\\site\\iemmixer.toml"
UPLOAD = f"X:\\root\\incoming\\roster-{FILE_SHA[:12]}.toml"
CHECK = ("roster", "check", UPLOAD)
# The guard's `iemmode status` with bundle SHA active.
STATUS = {"ok": True, "mode": "live", "switching": None, "detail": f"mode live; bundle {SHA}", "alarms": []}


def status(sha: str = FILE_SHA, *, applied: bool = True, ok: bool | None = None, problems=(), added=(),
           removed=(), at: int = 1790000000) -> str:
    """roster.status.json as the server writes it (roster_watch::Status)."""
    return json.dumps({"sha256": sha, "ok": applied if ok is None else ok, "applied": applied, "at": at,
                       "problems": list(problems), "added": list(added), "removed": list(removed)}, indent=2)


def checked(ok: bool = True, problems=(), sha: str = FILE_SHA) -> tuple[int, str]:
    """`iem-server roster check`'s exit code and its one JSON line."""
    return (0 if ok else 1), json.dumps({"ok": ok, "problems": list(problems), "sha256": sha})


class RosterBase(Base):
    def setUp(self) -> None:
        super().setUp()
        envfile = ip.env_path()
        envfile.write_text(envfile.read_text(encoding="utf-8") + f"PC_SERVER_CONFIG={CONFIG}\n", encoding="utf-8")
        self.file = self.tmp / "ops" / "roster.toml"
        self.file.parent.mkdir()
        self.file.write_bytes(TEXT)
        self.fetched()
        self.gh.calls.clear()
        self.pc.replies[("status",)] = (0, json.dumps(STATUS))
        self.pc.replies[CHECK] = checked()
        self.before: str | None = status(OTHER_SHA)
        self.statuses: list[str | None] = [status(added=["member10"], removed=["member2"])]
        self.reads = 0
        self.read_at: list[float] = []
        self.pc.texts[roster.NEW] = lambda: {"installed": True, "before": self.before}
        self.pc.texts[roster.STATUS] = self.next_status

    def next_status(self) -> str | None:
        """The status file at each read: the list in order, its last one from there on."""
        self.reads += 1
        self.read_at.append(time.monotonic())
        return self.statuses[min(self.reads, len(self.statuses)) - 1]

    def install(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main("roster-install", "--file", str(self.file), *extra)

    def scripts(self, text: str) -> list[tuple[str, str]]:
        """The module scripts holding `text`, with their flag mode."""
        return [(s, e) for s, e in self.pc.modules if text in s]

    def installs(self) -> list[tuple[str, str]]:
        return self.scripts(roster.NEW)

    def status_reads(self) -> list[tuple[str, str]]:
        return [(s, e) for s, e in self.scripts(roster.STATUS) if roster.NEW not in s]


class RosterInstallTests(RosterBase):
    def test_refused_while_event_now(self) -> None:
        spec = ip.COMMANDS["roster-install"]
        self.assertEqual((spec.fn, spec.pc, spec.dev_time, spec.locked), (ip.cmd_roster_install, True, True, True))
        for argv in ((), ("--dry-run",)):
            self.flag()
            self.pc.modules.clear()
            code, docs, err = self.install(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("runs only in dev time", err)
            self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps), ([], [], []), argv)

    def test_a_check_with_problems_installs_nothing(self) -> None:
        why = "lineup 'main': set 'member9' is no stereo mix of the site"
        self.pc.replies[CHECK] = checked(ok=False, problems=[why])
        code, docs, err = self.install()
        self.assertEqual(code, 1, err)
        self.assertEqual(docs, [{"roster_install": {"sha256": FILE_SHA, "ok": False, "problems": [why],
                                                    "applied": False, "added": [], "removed": []}}])
        self.assertEqual(self.installs(), [])
        self.assertEqual(self.status_reads(), [])
        self.assertEqual(self.pc.calls[-1], ("iem-server.exe", list(CHECK), "abandon"))

    def test_dry_run_checks_only(self) -> None:
        code, docs, err = self.install("--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"roster_install": {"sha256": FILE_SHA, "ok": True, "problems": [], "applied": False,
                                                    "added": [], "removed": [], "dry_run": True}}])
        self.assertEqual([c[:2] for c in self.pc.calls], [("iemmode.exe", ["status"]), ("iem-server.exe", list(CHECK))])
        self.assertEqual([(src, dst) for src, dst, _ in self.pc.scps],
                         [(str(self.file), f"tester@pc.test:/X:/root/incoming/roster-{FILE_SHA[:12]}.toml")])
        self.assertEqual((self.installs(), self.status_reads()), ([], []))
        # A dry run with problems exits 1 as well, and installs nothing either.
        self.pc.replies[CHECK] = checked(ok=False, problems=["exactly one lineup must be main"])
        code, docs, _ = self.install("--dry-run")
        self.assertEqual((code, docs[-1]["roster_install"]["ok"]), (1, False))
        self.assertEqual(self.installs(), [])

    def test_the_check_runs_the_active_bundles_server_from_the_stage(self) -> None:
        code, _, err = self.install("--dry-run")
        self.assertEqual(code, 0, err)
        script = self.pc.native_scripts[-1]
        rec = ip.need_record(SHA)
        # The upload's hash first, then the bundle's server read once, checked by
        # this box's fetch record and run from the admin-only stage (#15), then
        # the server config: nothing runs from the user's root.
        upload_check = script.index(ip.hash_check(UPLOAD, FILE_SHA))
        stage = script.index(f"[IO.File]::ReadAllBytes('X:\\root\\bundles\\{SHA}\\iem-server.exe')")
        config = script.index(f"$env:IEMMIXER_CONFIG = '{CONFIG}'")
        self.assertLess(upload_check, stage)
        self.assertLess(stage, config)
        self.assertIn(rec["sums"]["iem-server.exe"], script)
        self.assertIn("$x = $iemMod ; ", script)

    def test_install_waits_for_the_status_sha(self) -> None:
        self.statuses = [status(OTHER_SHA), None, "{ cut sh", status(added=["member10"], removed=["member2"])]
        code, docs, err = self.install()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"roster_install": {"sha256": FILE_SHA, "ok": True, "problems": [], "applied": True,
                                                    "added": ["member10"], "removed": ["member2"]}}])
        self.assertEqual(self.reads, 4)
        gaps = [b - a for a, b in zip(self.read_at, self.read_at[1:])]
        self.assertTrue(all(g >= ip.POLL_S for g in gaps), gaps)   # at least one poll interval between two reads
        self.assertNotIn("Member", json.dumps(docs))   # ids and counts only, never a person's name
        # The install: a change (a new flag lets it finish); the reads are abandoned.
        (script, event), = self.installs()
        self.assertEqual(event, "finish")
        new, dst = "'X:\\site\\roster.toml.new'", "'X:\\site\\roster.toml'"
        order = [script.index(ip.hash_check(UPLOAD, FILE_SHA)),
                 script.index(f"Copy-Item -LiteralPath '{UPLOAD}' -Destination {new} -Force"),
                 script.index(ip.hash_check("X:\\site\\roster.toml.new", FILE_SHA)),
                 script.index(f"Move-Item -LiteralPath {new} -Destination {dst} -Force"),
                 script.index(ip.hash_check("X:\\site\\roster.toml", FILE_SHA))]
        self.assertEqual(order, sorted(order))
        self.assertEqual({e for _, e in self.status_reads()}, {"abandon"})
        self.assertEqual(self.pc.calls[-1][2], "abandon")   # the check only reads

    def test_a_status_that_never_matches_fails_naming_the_wait(self) -> None:
        self.statuses = [status(OTHER_SHA)]
        with mock.patch.object(roster, "WAIT_S", 0.3):
            code, docs, err = self.install()
        self.assertEqual(code, 1, err)
        out = docs[-1]["roster_install"]
        self.assertEqual((out["ok"], out["applied"], out["added"], out["removed"]), (False, False, [], []))
        why, = out["problems"]
        self.assertIn("not applied within 0.3 s", why)
        self.assertIn(f"names {OTHER_SHA[:12]}", why)
        self.assertIn("defers", why)
        self.assertGreater(self.reads, 1)
        self.assertEqual(len(self.installs()), 1)

    def test_a_refused_or_fallback_status_is_not_applied(self) -> None:
        lines = ["lineup 'youth': leader 'member3' is placed in no set", "in use instead: the roster applied last"]
        self.statuses = [status(applied=False, problems=lines)]
        code, docs, err = self.install()
        self.assertEqual(code, 1, err)
        self.assertEqual(docs[-1], {"roster_install": {"sha256": FILE_SHA, "ok": False, "problems": lines,
                                                       "applied": False, "added": [], "removed": []}})

    def test_applied_with_problems_counts_as_applied(self) -> None:
        why = "person 'member2' was not archived: PermissionDenied"
        self.statuses = [status(ok=False, problems=[why], removed=["member2"])]
        code, docs, err = self.install()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["roster_install"], {"sha256": FILE_SHA, "ok": False, "problems": [why],
                                                      "applied": True, "added": [], "removed": ["member2"]})

    def test_an_earlier_refusal_of_the_same_file_waits_for_the_servers_own_read(self) -> None:
        stale = status(applied=False, problems=["the roster has no lineup 'youth', which plays"], at=1789000000)
        self.before = stale
        self.statuses = [stale, stale, status(added=["member10"])]
        code, docs, err = self.install()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.reads, 3)
        self.assertEqual(docs[-1]["roster_install"]["added"], ["member10"])

    def test_the_roster_in_use_installed_again_is_unchanged(self) -> None:
        # The server writes no status for the roster it already uses.
        self.before = status(added=["member10"], at=1789000000)
        self.statuses = [self.before]
        code, docs, err = self.install()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.reads, 1)
        self.assertEqual(docs[-1]["roster_install"], {"sha256": FILE_SHA, "ok": True, "problems": [], "applied": True,
                                                      "added": [], "removed": [], "unchanged": True})

    def test_odd_status_fields_never_reach_the_output(self) -> None:
        self.statuses = [json.dumps({"sha256": FILE_SHA, "ok": True, "applied": True, "at": 1,
                                     "problems": [], "added": ["member10", "Zyxqw Name", 7], "removed": "member2"})]
        code, docs, err = self.install()
        self.assertEqual(code, 0, err)
        out = docs[-1]["roster_install"]
        self.assertEqual((out["added"], out["removed"]), (["member10", "?", "?"], []))

    def test_no_server_config_or_no_file_is_refused_before_any_call(self) -> None:
        envfile = ip.env_path()
        text = envfile.read_text(encoding="utf-8")
        for bad in ("", "iemmixer.toml", "X:\\site\\", "\\\\host\\share\\iemmixer.toml", "X:\\si\"te\\iemmixer.toml"):
            envfile.write_text(text.replace(f"PC_SERVER_CONFIG={CONFIG}", f"PC_SERVER_CONFIG={bad}"), encoding="utf-8")
            code, docs, err = self.install()
            self.assertEqual((code, docs), (1, []), bad)
            self.assertIn("PC_SERVER_CONFIG", err, bad)
        envfile.write_text(text, encoding="utf-8")
        code, docs, err = self.run_main("roster-install", "--file", str(self.tmp / "missing.toml"))
        self.assertEqual((code, docs), (1, []))
        self.assertIn("missing.toml", err)
        self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps, self.gh.calls), ([], [], [], []))

    def test_no_active_bundle_or_an_unfetched_one_is_refused_before_the_upload(self) -> None:
        self.pc.replies[("status",)] = (0, json.dumps({**STATUS, "detail": "mode event; no bundle"}))
        code, docs, err = self.install()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("no active bundle", err)
        other = "ab" * 20
        self.pc.replies[("status",)] = (0, json.dumps({**STATUS, "detail": f"mode live; bundle {other}"}))
        code, docs, err = self.install()
        self.assertEqual((code, docs), (1, []))
        self.assertIn(f"iempc fetch-bundle --sha {other}", err)
        self.pc.replies[("status",)] = (4, "")
        code, docs, err = self.install()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("iemmode status failed (exit 4)", err)
        self.assertEqual((self.pc.scps, self.installs()), ([], []))

    def test_a_server_without_roster_check_or_an_odd_answer_fails(self) -> None:
        for reply, words in (((2, ""), "does not know 'roster check'"),
                             ((0, "not json"), "printed no JSON object"),
                             (checked(problems=["x"]), "does not match its exit"),
                             ((1, json.dumps({"ok": True, "problems": [], "sha256": FILE_SHA})), "does not match its exit"),
                             (checked(sha=OTHER_SHA), "checked another file")):
            self.pc.replies[CHECK] = reply
            code, docs, err = self.install()
            self.assertEqual((code, docs), (1, []), words)
            self.assertIn(words, err)
            self.assertEqual(self.installs(), [], words)

    def test_an_install_the_pc_does_not_confirm_fails(self) -> None:
        self.pc.texts[roster.NEW] = lambda: {"installed": False, "before": None}
        code, docs, err = self.install()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("did not confirm the install", err)
        self.assertEqual(self.status_reads(), [])

    def test_a_flag_during_the_wait_runs_the_event_path(self) -> None:
        self.statuses = [status(OTHER_SHA)]
        self.pc.texts[roster.STATUS] = lambda: (self.flag(), status(OTHER_SHA))[1]
        code, docs, err = self.install()
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(len(self.installs()), 1)
        self.assertEqual(self.pc.calls[-1][:2], ("iemmode.exe", ["event", "--signal"]))

    def test_a_flag_during_the_check_installs_nothing(self) -> None:
        self.pc.replies[CHECK] = lambda: (self.flag(), checked())[1]
        code, _, err = self.install()
        self.assertEqual(code, ip.PREEMPTED, err)
        self.assertEqual(self.installs(), [])


class RosterInstallGuardTests(RunningGuardCases, RosterBase):
    """An install (not a dry run) refuses, before any call, while a live run
    or a soak of this dev entry may still run: the reload closes every
    person's mixer socket (4003), the soak's and the live run's too."""
    ARGV = ("roster-install", "--file", "<set in setUp>")
    HEAD = "no roster install"
    NOTHING = "(nothing was installed)"

    def setUp(self) -> None:
        super().setUp()
        self.ARGV = ("roster-install", "--file", str(self.file))

    def passes(self) -> None:
        code, docs, err = self.run_main(*self.ARGV)
        self.assertEqual(code, 0, err)
        self.assertTrue(docs[-1]["roster_install"]["applied"])

    def test_a_dry_run_is_not_refused(self) -> None:
        self.write_soaks([{"sha": SHA, "branch": "dev", "run": 1, "hours": 8, "entry": 0, "at": ip.now_iso()}])
        code, docs, err = self.run_main(*self.ARGV, "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertTrue(docs[-1]["roster_install"]["dry_run"])


if __name__ == "__main__":
    unittest.main()
