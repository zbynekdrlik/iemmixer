"""Tests for scripts/iem-pc/iempc_bundle.py: fetch-bundle, install, activate
(online and offline), dispatch-hil and a bundle's members (split out of
test_iempc.py, #36). They reuse iempc_test_support's fakes (FakePc stands in
for ssh and scp, FakeGh for GitHub); every value is synthetic."""
from __future__ import annotations

import json
import os
import shutil
import sys
import time
import unittest
import warnings
import zipfile
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import RUN, SHA, SHA2, Base, ip, make_zip, sha256  # noqa: E402


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
        # #15 (review): read once, checked, staged admin-only and run from the stage.
        run, at = self.pc.native_scripts[0], 0
        for step in (f"$iemB = [IO.File]::ReadAllBytes({ip.ps_quote(guard)})",
                     f"$iemH -cne '{sha256(b'synthetic iemmixer-guard.exe')}'",
                     "$iemMod = Join-Path $iemStage 'iemmixer-guard.exe'", "& $iemOnly $iemMod",
                     "$x = $iemMod ; $r = @(& $x @a 2>&1)"):
            at = run.index(step, at)
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


class ActivateTests(Base):
    """`activate` (#9 2026-09-28): `iemmode activate`, then the hand-over,
    verified by the `guard_build` of the guard that answers `iemmode status`."""

    ACTIVATED = (0, json.dumps({"ok": True, "mode": "event", "alarms": [],
                                "detail": f"activated {SHA}; the guard hands over to its new exe"}))
    UNREACHABLE = (4, json.dumps({"ok": False, "detail": "the guard is unreachable: no pipe"}))

    def setUp(self) -> None:
        super().setUp()
        self.patch(HANDOVER_S=10.0, HANDOVER_POLL_S=0.01)
        self.pc.replies[("activate", SHA)] = self.ACTIVATED

    @staticmethod
    def status(build: str | None) -> tuple[int, str]:
        doc = {"ok": True, "mode": "event", "alarms": [], "detail": "mode event; bundle " + SHA}
        if build is not None:
            doc["guard_build"] = build
        return 0, json.dumps(doc)

    def statuses(self, *answers) -> None:
        """`iemmode status` gives these in turn, then the last one again; a
        callable is called (it may raise or write the flag)."""
        queue = list(answers)

        def answer():
            a = queue.pop(0) if len(queue) > 1 else queue[0]
            return a() if callable(a) else a

        self.pc.replies[("status",)] = answer

    def test_activate_waits_until_the_guard_that_answers_names_the_sha(self) -> None:
        # The old guard (no guard_build: older than the field, or SHA2's), the gap, the new one.
        self.statuses(self.status(None), self.status(SHA2), self.UNREACHABLE, self.status(SHA))
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["activate", SHA], "finish")]
                         + [("iemmode.exe", ["status"], "abandon")] * 4)
        self.assertEqual(self.pc.timeouts, [ip.SWITCH_S] + [ip.STATUS_S] * 4)
        self.assertEqual({k: docs[0][k] for k in ("iemmode", "exit")}, {"iemmode": ["activate", SHA], "exit": 0})
        self.assertEqual(docs[0]["reply"]["detail"], f"activated {SHA}; the guard hands over to its new exe")
        self.assertEqual(docs[1]["elevated_bin"], "failed")   # not fetched here; before the hand-over (#15)
        self.assertEqual(docs[2], {"handover": {"guard_build": SHA, "reads": 4, "mode": "event",
                                                "detail": "mode event; bundle " + SHA}})

    def test_a_guard_already_on_the_sha_is_read_once(self) -> None:
        self.statuses(self.status(SHA))
        code, docs, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual((code, docs[2]["handover"]["reads"]), (0, 1))
        self.assertEqual(len(self.pc.calls), 2)

    def test_a_status_read_that_fails_is_read_again(self) -> None:
        def ssh_cut():
            raise ip.StepError("ssh: connection reset")

        self.statuses(ssh_cut, self.status(SHA))
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[2]["handover"]["reads"], 2)

    def test_a_hand_over_that_never_names_the_sha_fails_within_its_bound(self) -> None:
        self.patch(HANDOVER_S=0.2)
        self.statuses(self.status(SHA2))
        start = time.monotonic()
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertLess(time.monotonic() - start, 5)
        self.assertEqual((code, [next(iter(d)) for d in docs]), (1, ["iemmode", "elevated_bin"]))
        self.assertIn(f"the guard did not name build {SHA} within 0.2 s", err)
        self.assertIn(f"the last: exit 0, guard_build '{SHA2}'", err)
        self.assertIn("never force-end", err)
        self.assertGreater(len(self.pc.calls), 2)
        self.assertEqual({c[1][0] for c in self.pc.calls[1:]}, {"status"})

    def test_a_refused_activation_waits_for_no_hand_over(self) -> None:
        self.pc.replies[("activate", SHA)] = (1, json.dumps({
            "ok": False, "mode": "event", "alarms": [],
            "detail": "activate in event needs no iemmixer process; running: engine"}))
        code, docs, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual((code, len(docs), docs[0]["exit"]), (1, 1, 1))
        self.assertIn("running: engine", docs[0]["reply"]["detail"])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["activate", SHA], "finish")])

    def test_activate_is_dev_time_and_one_at_a_time(self) -> None:
        self.assertEqual((ip.COMMANDS["activate"].pc, ip.COMMANDS["activate"].dev_time,
                          ip.COMMANDS["activate"].locked), (True, True, True))
        self.flag()
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules), (1, [], [], []))
        self.assertIn("runs only in dev time", err)

    def test_an_open_spike_window_refuses_activate(self) -> None:
        self.open_window()
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual((code, docs, self.pc.calls), (1, [], []))
        self.assertIn("'activate' waits until 'iempc handover-s1a' has handed the card over", err)

    def test_a_bad_sha_is_refused_before_the_pc(self) -> None:
        for bad in ("1234", SHA.upper(), SHA + "0"):
            code, _, err = self.run_main("activate", "--sha", bad)
            self.assertEqual(code, 1, bad)
            self.assertIn("not a full commit SHA", err, bad)
        self.assertEqual(self.pc.calls, [])

    def test_a_new_flag_during_the_activation_lets_it_finish_then_runs_the_event_path(self) -> None:
        self.pc.replies[("activate", SHA)] = lambda: (self.flag(), self.ACTIVATED)[1]
        code, docs, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["activate", SHA], "finish"), ("iemmode.exe", ["event"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})

    def test_a_new_flag_during_a_status_read_abandons_it_and_runs_the_event_path(self) -> None:
        self.statuses(lambda: (self.flag(), self.status(SHA2))[1])
        code, _, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([(c[1][0], c[2]) for c in self.pc.calls],
                         [("activate", "finish"), ("status", "abandon"), ("event", "ignore")])

    def test_a_new_flag_between_two_reads_runs_the_event_path(self) -> None:
        self.patch(HANDOVER_POLL_S=1.0)
        self.statuses(self.status(SHA2))
        with mock.patch.object(time, "sleep", side_effect=lambda _s: self.flag()):
            code, _, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["activate", "status", "event"])


class OfflineActivateTests(Base):
    """`activate --offline` (#9 2026-09-28): a guard too old to activate in
    event is quit gracefully, the bundle's own guard exe activates while no
    guard runs, and the hand-over is verified as online."""

    EXE = f"X:\\root\\bundles\\{SHA}\\iemmixer-guard.exe"
    QUIT = (0, json.dumps({"ok": True, "mode": "event", "alarms": [],
                           "detail": "the guard stops; its children keep running"}))
    OFFLINE = (0, json.dumps({"ok": True, "mode": "event", "alarms": [], "guard_build": SHA,
                              "detail": f"activated {SHA} without a guard; the next iemmode call starts the guard "
                                        "from bin"}))

    def setUp(self) -> None:
        super().setUp()
        self.patch(HANDOVER_S=10.0, HANDOVER_POLL_S=0.01, QUIT_S=10.0)
        self.fetched()
        self.pc.replies[("quit",)] = self.QUIT
        self.pc.replies[("activate", SHA)] = self.OFFLINE
        self.pc.replies[("status",)] = ActivateTests.status(SHA)
        self.guards(1, 1, 0)

    def guards(self, *counts: int) -> None:
        """The guard processes the PC reads in turn, then the last again."""
        queue = list(counts)
        self.pc.module_result = lambda: queue.pop(0) if len(queue) > 1 else queue[0]

    def reads(self) -> list[str]:
        return [event for script, event in self.pc.modules if "Get-Process -Name 'iemmixer-guard'" in script]

    def test_the_old_guard_quits_the_bundle_s_own_guard_activates_and_the_new_one_answers(self) -> None:
        code, docs, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["quit"], "finish"),
                                         ("iemmixer-guard.exe", ["activate", SHA], "finish"),
                                         ("iemmode.exe", ["status"], "abandon")])
        self.assertEqual(self.reads(), ["abandon"] * 3)
        # #15: the guard is read once from bundles\<sha>, checked by the fetch record, staged
        # admin-only and run from the stage, never from the user's root.
        guard_run = self.pc.native_scripts[1]
        at = 0
        for step in (f"$iemB = [IO.File]::ReadAllBytes({ip.ps_quote(self.EXE)})",
                     f"$iemH -cne '{sha256(b'synthetic iemmixer-guard.exe')}'", "$iemStage = Join-Path $iemRoot 'bootstrap-stage'",
                     "$iemMod = Join-Path $iemStage 'iemmixer-guard.exe'", "& $iemOnly $iemMod", f"$x = {ip.ps_quote(self.EXE)}",
                     "$x = $iemMod ; $r = @(& $x @a 2>&1)"):
            at = guard_run.index(step, at)
        self.assertEqual(guard_run.count(ip.ps_quote(self.EXE)), 3)   # read once, named in the mismatch, the fallback text
        self.assertEqual(self.pc.timeouts, [ip.STATUS_S, ip.INSTALL_S, ip.STATUS_S])
        self.assertEqual([next(iter(d)) for d in docs], ["iemmode", "guard_stopped", "iemmixer-guard", "elevated_bin", "handover"])
        self.assertEqual(docs[1], {"guard_stopped": {"reads": 2}})
        self.assertEqual((docs[2]["iemmixer-guard"], docs[2]["exit"]), (["activate", SHA], 0))
        self.assertEqual(docs[4]["handover"]["guard_build"], SHA)

    def test_without_a_running_guard_nothing_is_quit(self) -> None:
        self.guards(0)
        code, docs, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, 0, err)
        self.assertEqual([c[0] for c in self.pc.calls], ["iemmixer-guard.exe", "iemmode.exe"])
        self.assertEqual(self.reads(), ["abandon"])

    def test_a_refused_offline_activation_starts_the_guard_again(self) -> None:
        self.pc.replies[("activate", SHA)] = (1, json.dumps({
            "ok": False, "mode": "event", "alarms": [],
            "detail": "activate in event needs no iemmixer process; running: engine"}))
        self.pc.replies[("status",)] = ActivateTests.status(SHA2)
        code, docs, _ = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, 1)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["quit", "activate", "status"])
        self.assertIn("running: engine", docs[2]["reply"]["detail"])
        self.assertEqual((docs[-1]["iemmode"], docs[-1]["reply"]["guard_build"]), (["status"], SHA2))
        self.assertNotIn("handover", docs[-1])

    def test_an_offline_guard_that_did_not_run_starts_the_guard_again(self) -> None:
        self.pc.replies[("activate", SHA)] = (None, "", "sha256 mismatch: " + self.EXE)
        code, docs, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, 1)
        self.assertIn("iemmixer-guard.exe did not run on the PC: sha256 mismatch", err)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["quit", "activate", "status"])
        self.assertEqual(docs[-1]["iemmode"], ["status"])

    def test_a_refused_quit_activates_nothing(self) -> None:
        self.pc.replies[("quit",)] = (1, json.dumps({"ok": False, "mode": "dev", "alarms": [], "detail": "switching"}))
        code, docs, _ = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual((code, len(docs)), (1, 1))
        self.assertEqual([c[1][0] for c in self.pc.calls], ["quit"])

    def test_a_guard_that_does_not_end_is_waited_for_within_a_bound_never_forced(self) -> None:
        self.patch(QUIT_S=0.2)
        self.guards(1)
        start = time.monotonic()
        code, _, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertLess(time.monotonic() - start, 5)
        self.assertEqual(code, 1)
        self.assertIn("iemmixer-guard process(es) still run 0.2 s after 'iemmode quit': nothing was activated", err)
        self.assertIn("never force-end", err)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["quit"])
        self.assertGreater(len(self.reads()), 2)

    def test_it_needs_this_box_s_fetch_record_before_any_pc_step(self) -> None:
        shutil.rmtree(ip.bundle_dir(SHA))
        code, docs, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules), (1, [], [], []))
        self.assertIn("is not fetched", err)

    def test_the_flag_or_an_open_window_refuses_it_before_the_pc(self) -> None:
        self.open_window()
        code, _, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, 1)
        self.assertIn("'activate' waits until 'iempc handover-s1a'", err)
        self.open_window(closed=True)
        self.flag()
        code, _, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, 1)
        self.assertIn("runs only in dev time", err)
        self.assertEqual((self.pc.calls, self.pc.modules), ([], []))

    def test_a_new_flag_during_the_quit_lets_it_finish_then_runs_the_event_path(self) -> None:
        self.pc.replies[("quit",)] = lambda: (self.flag(), self.QUIT)[1]
        code, _, _ = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["quit"], "finish"), ("iemmode.exe", ["event"], "ignore")])

    def test_a_new_flag_during_the_offline_step_lets_it_finish_then_runs_the_event_path(self) -> None:
        self.pc.replies[("activate", SHA)] = lambda: (self.flag(), self.OFFLINE)[1]
        code, _, _ = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([(c[0], c[1][0], c[2]) for c in self.pc.calls],
                         [("iemmode.exe", "quit", "finish"), ("iemmixer-guard.exe", "activate", "finish"),
                          ("iemmode.exe", "event", "ignore")])

    def test_a_new_flag_while_the_guard_ends_runs_the_event_path(self) -> None:
        def count():
            if len(self.reads()) == 2:  # the read after the quit
                self.flag()
            return 1

        self.pc.module_result = count
        code, _, _ = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["quit", "event"])


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
        self.patch(gh=self.gh)  # gh works again: the same SHA and entry is no repeat
        self.assertEqual(self.run_main("dispatch-hil", "--sha", SHA)[0], 0)
        self.assertEqual(len(ip.load_dispatches()), 1)


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


if __name__ == "__main__":
    unittest.main()
