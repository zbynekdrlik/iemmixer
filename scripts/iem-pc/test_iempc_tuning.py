"""Tests for scripts/iem-pc/iempc_tuning.py: `iempc tuning-install` and the
refresh of S1c's tuning modules after `iempc activate` (#15). They reuse
iempc_test_support's fakes (FakePc stands in for ssh and scp, FakeGh for GitHub);
every value is synthetic and the private profile is a temp file."""
from __future__ import annotations

import json
import re
import shutil
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_tuning as it  # noqa: E402
from iempc_test_support import SHA, Base, ip, make_zip, sha256  # noqa: E402

MODULES = {"tuning/IemTuning.psm1": b"synthetic IemTuning.psm1", "tuning/IemMeasure.psm1": b"synthetic IemMeasure.psm1",
           "tuning/IemTuningStore.psm1": b"synthetic IemTuningStore.psm1"}
# A profile shaped like the private one (tuning_rules.load_profile accepts it): synthetic values only.
PROFILE = {"version": 1, "journal": "X:\\tuning\\journal.json", "registry_root": "",
           "layout": {"housekeeping": [0], "nic": [1], "card": [2], "audio": [3]},
           "plan": {"guid": "00000000-0000-0000-0000-00000000000a", "source": "00000000-0000-0000-0000-00000000000b"},
           "governor": "gov", "placement": [], "services_disable": [], "services_mode": [],
           "updates": {"services": [], "tasks": []}, "maintenance": {"off": True, "tasks": []},
           "defender": {"paths": [], "processes": []}, "devices": [],
           "nic": {"adapter": "a", "properties": {}, "rss": {"base": 1, "max": 1}, "pnp_capabilities": 24},
           "fingerprint": {"files": [], "keys": []}}
# The installed profile's hash, as the PC answers it for -KeepProfile.
KEPT = sha256(b"the installed profile")
INSTALL = re.compile(r"Install-IemTuning -SourceDir '(?P<source>[^']*)' -TuningSha256 '(?P<tuning>[0-9a-f]{64})' "
                     r"-MeasureSha256 '(?P<measure>[0-9a-f]{64})' -StoreSha256 '(?P<store>[0-9a-f]{64})'"
                     r"(?: -ProfileSha256 '(?P<profile>[0-9a-f]{64})')?(?P<keep> -KeepProfile)?")
GUARD_COUNT = "Get-Process -Name 'iemmixer-guard'"
MODULE = f"X:\\root\\bootstrap\\{SHA}\\IemPc.psm1"
SOURCE = f"X:\\root\\bootstrap\\{SHA}\\tuning"


def hashes(m: re.Match, profile: str | None = None) -> dict[str, str]:
    """What Install-IemTuning reads back for the call `m`: the module hashes it
    was given, `profile` or the one it was given (the installed one's with
    -KeepProfile)."""
    return {"tuning": m["tuning"], "measure": m["measure"], "store": m["store"], "profile": profile or m["profile"] or KEPT}


class TuningBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.gh.artifact = make_zip(self.tmp / "artifact-tuning" / f"iemmixer-{SHA}.zip", extra=MODULES)
        self.profile = self.tmp / "pc-tuning.json"
        self.profile.write_text(json.dumps(PROFILE), encoding="utf-8")
        saved = it.PROFILE
        self.addCleanup(setattr, it, "PROFILE", saved)
        it.PROFILE = self.profile
        self.on_install = None   # a callable(match) answering Install-IemTuning, else its hashes
        self.guard_counts = [0]
        self.pc.module_result = self.answer

    def answer(self):
        """What the fake PC answers a module script: Install-IemTuning's read-back
        (the hashes it was given; the installed profile's for -KeepProfile), the
        guard process count, or 'ok' (pc_mkdir)."""
        script = self.pc.modules[-1][0]
        m = INSTALL.search(script)
        if m:
            if self.on_install:
                return self.on_install(m)
            return hashes(m)
        if GUARD_COUNT in script:
            return self.guard_counts.pop(0) if len(self.guard_counts) > 1 else self.guard_counts[0]
        return "ok"

    def installs(self) -> list[re.Match]:
        return [m for m in (INSTALL.search(s) for s, _ in self.pc.modules) if m]

    def profile_checks(self) -> list[str]:
        return [event for script, event in self.pc.modules if "profile.json" in script]

    def staged(self, name: str) -> str:
        return str(ip.bundle_dir(SHA) / "tuning" / name)


class TuningInstallTests(TuningBase):
    def test_tuning_install_is_dev_time_and_one_at_a_time(self) -> None:
        spec = ip.COMMANDS["tuning-install"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))
        self.fetched()
        self.flag()
        code, docs, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules, self.pc.scps), (1, [], [], [], []))
        self.assertIn("runs only in dev time", err)

    def test_the_attested_modules_and_the_profile_go_up_and_the_pc_checks_their_hashes(self) -> None:
        self.fetched()
        code, docs, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, 0, err)
        dest = f"tester@pc.test:/X:/root/bootstrap/{SHA}"
        self.assertEqual(self.pc.scps, [(str(ip.bundle_dir(SHA) / "IemPc.psm1"), f"{dest}/IemPc.psm1", "finish"),
                                        (self.staged("IemTuning.psm1"), f"{dest}/tuning/IemTuning.psm1", "finish"),
                                        (self.staged("IemMeasure.psm1"), f"{dest}/tuning/IemMeasure.psm1", "finish"),
                                        # IemTuning's store (#34), from the same attested zip
                                        (self.staged("IemTuningStore.psm1"), f"{dest}/tuning/IemTuningStore.psm1", "finish"),
                                        (str(self.profile), f"{dest}/tuning/profile.json", "finish"),
                                        # then the admin-only bin's iemmode.exe (#15)
                                        (str(ip.bundle_dir(SHA) / "iemmode.exe"),
                                         f"tester@pc.test:/X:/root/incoming/iemmode-{SHA}.exe", "finish")])
        for name in ("IemTuning.psm1", "IemMeasure.psm1", "IemTuningStore.psm1"):
            self.assertEqual(Path(self.staged(name)).read_bytes(), MODULES[f"tuning/{name}"], name)
        script, mode = next(m for m in self.pc.modules if "Install-IemTuning" in m[0])
        self.assertEqual(mode, "finish")
        self.assertIn(f"$iemB = [IO.File]::ReadAllBytes('{MODULE}')", script)
        self.assertIn(f"$iemH -cne '{sha256(b'synthetic IemPc.psm1')}'", script)
        self.assertIn("Import-Module $iemMod -Force ; $r = & { Install-IemTuning ", script)
        self.assertNotIn(f"Import-Module '{MODULE}'", script)   # never from the run folder (#15)
        [m] = self.installs()
        sums = ip.load_record(SHA)["sums"]
        self.assertEqual(m.groups(), (SOURCE, sums["tuning/IemTuning.psm1"], sums["tuning/IemMeasure.psm1"],
                                      sums["tuning/IemTuningStore.psm1"], sha256(self.profile.read_bytes()), None))
        # Each module hash is the zip member's own (the store's too, #34).
        self.assertEqual((m["tuning"], m["measure"], m["store"]),
                         tuple(sha256(MODULES[f"tuning/{n}"]) for n in ("IemTuning.psm1", "IemMeasure.psm1", "IemTuningStore.psm1")))
        self.assertEqual(docs[-2], {"tuning_install": SHA, "hashes": hashes(m)})
        self.assertEqual(docs[-1]["elevated_bin"], SHA)

    def test_profile_names_another_private_file(self) -> None:
        self.fetched()
        other = self.tmp / "other.json"
        other.write_text(json.dumps({**PROFILE, "governor": "other"}), encoding="utf-8")
        code, _, err = self.run_main("tuning-install", "--sha", SHA, "--profile", str(other))
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.scps[-2][0], str(other))   # the last one is the admin-only bin's iemmode.exe (#15)
        self.assertEqual(self.installs()[0]["profile"], sha256(other.read_bytes()))

    def test_a_profile_load_profile_refuses_never_reaches_the_pc(self) -> None:
        self.fetched()
        bad = {**PROFILE, "layout": {"housekeeping": [0], "card": [2], "nic": [1], "audio": [2]}}
        self.profile.write_text(json.dumps(bad), encoding="utf-8")
        code, docs, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual((code, docs, self.pc.modules, self.pc.scps), (1, [], [], []))
        self.assertIn("processor 2 has two roles", err)
        self.profile.unlink()
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual((code, self.pc.modules, self.pc.scps), (1, [], []))
        self.assertIn("missing", err)

    def test_a_read_back_that_differs_from_what_was_sent_fails(self) -> None:
        self.fetched()
        self.on_install = lambda m: hashes(m, profile="0" * 64)
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("the PC read back profile 0000", err)
        self.on_install = lambda m: {**hashes(m), "store": "1" * 64}
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn(f"the PC read back store {'1' * 64}", err)
        # An answer without the store's hash (an IemPc from before #34) fails too.
        self.on_install = lambda m: {k: v for k, v in hashes(m).items() if k != "store"}
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("Install-IemTuning answered {", err)
        self.on_install = lambda m: "ok"
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("Install-IemTuning answered 'ok'", err)

    def test_a_refusal_on_the_pc_fails_the_command(self) -> None:
        self.fetched()

        def refused(_m):
            raise ip.StepError("PC step failed: layout: processor 2 is in both card and audio (roles overlap)")

        self.on_install = refused
        code, docs, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual((code, docs), (1, []))
        self.assertIn("roles overlap", err)

    def test_a_bundle_without_the_tuning_modules_is_refused_before_the_pc(self) -> None:
        self.gh.artifact = self.artifact   # iempc_test_support's bundle: tuning/state.ps1 only
        self.fetched()
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual((code, self.pc.modules, self.pc.scps), (1, [], []))
        self.assertIn("tuning/IemTuning.psm1 is not a listed tuning file", err)

    def test_a_bundle_from_before_the_store_is_refused_before_the_pc(self) -> None:
        # #34: IemTuning.psm1 loads IemTuningStore.psm1 from its own folder; a bundle
        # without the store would install an IemTuning that cannot load.
        self.gh.artifact = make_zip(self.tmp / "artifact-old" / f"iemmixer-{SHA}.zip",
                                    extra={k: v for k, v in MODULES.items() if k != "tuning/IemTuningStore.psm1"})
        self.fetched()
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual((code, self.pc.modules, self.pc.scps), (1, [], []))
        self.assertIn("tuning/IemTuningStore.psm1 is not a listed tuning file", err)

    def test_an_unfetched_bundle_or_a_bad_sha_is_refused_before_the_pc(self) -> None:
        code, _, err = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual((code, self.pc.modules, self.pc.scps), (1, [], []))
        self.assertIn("is not fetched", err)
        code, _, err = self.run_main("tuning-install", "--sha", SHA.upper())
        self.assertEqual(code, 1)
        self.assertIn("not a full commit SHA", err)

    def test_a_new_flag_during_the_install_lets_it_finish_then_runs_the_event_path(self) -> None:
        self.fetched()

        def install_then_flag(m):
            self.flag()
            return hashes(m)

        self.on_install = install_then_flag
        code, docs, _ = self.run_main("tuning-install", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.pc.modules[-1][1], "finish")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})


class RefreshTests(TuningBase):
    """After `activate --sha`'s verified hand-over, the new bundle's two modules
    replace the installed ones when the elevated tuning folder holds a profile."""

    ACTIVATED = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "detail": f"activated {SHA}"}))
    STATUS = (0, json.dumps({"ok": True, "mode": "dev", "alarms": [], "detail": "mode dev; bundle " + SHA, "guard_build": SHA}))

    def setUp(self) -> None:
        super().setUp()
        self.patch(HANDOVER_S=10.0, HANDOVER_POLL_S=0.01, QUIT_S=10.0)
        self.pc.replies[("activate", SHA)] = self.ACTIVATED
        self.pc.replies[("status",)] = self.STATUS
        self.fetched()

    def test_an_installed_profile_gets_the_new_bundle_s_modules_and_stays(self) -> None:
        self.pc.texts["profile.json"] = True
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual([next(iter(d)) for d in docs], ["iemmode", "elevated_bin", "handover", "tuning_refresh"])
        self.assertEqual(self.profile_checks(), ["abandon"])
        [m] = self.installs()
        sums = ip.load_record(SHA)["sums"]
        self.assertEqual(m.groups(), (SOURCE, sums["tuning/IemTuning.psm1"], sums["tuning/IemMeasure.psm1"],
                                      sums["tuning/IemTuningStore.psm1"], None, " -KeepProfile"))
        self.assertEqual([s[1].rsplit("/", 1)[1] for s in self.pc.scps],
                         [f"iemmode-{SHA}.exe", "IemPc.psm1", "IemTuning.psm1", "IemMeasure.psm1", "IemTuningStore.psm1"])
        self.assertEqual({s[2] for s in self.pc.scps}, {"finish"})
        install = next(s for s, _ in self.pc.modules if "Install-IemTuning" in s)
        self.assertIn("Import-Module $iemMod -Force ; $r = & { Install-IemTuning ", install)   # the staged copy (#15)
        self.assertNotIn(f"Import-Module '{MODULE}'", install)
        self.assertEqual(docs[-1], {"tuning_refresh": SHA, "hashes": hashes(m)})
        # The hand-over came first: the refresh runs after the guard named the SHA.
        self.assertEqual([c[1][0] for c in self.pc.calls], ["activate", "status"])

    def test_without_an_installed_profile_nothing_is_refreshed(self) -> None:
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.profile_checks(), ["abandon"])
        self.assertEqual((self.installs(), [s[1].rsplit("/", 1)[1] for s in self.pc.scps]), ([], [f"iemmode-{SHA}.exe"]))
        self.assertEqual([next(iter(d)) for d in docs], ["iemmode", "elevated_bin", "handover"])
        self.assertIn("no tuning profile in the PC's elevated tuning folder", err)

    def test_a_failed_refresh_is_reported_and_the_activation_counts(self) -> None:
        self.pc.texts["profile.json"] = True

        def refused(_m):
            raise ip.StepError("PC step failed: tuning file read-back: a rule for S-1-5-21-1-2-3-1001")

        self.on_install = refused
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["tuning_refresh"], "failed")
        self.assertEqual(docs[-1]["sha"], SHA)
        self.assertIn("tuning file read-back", docs[-1]["error"])
        self.assertIn("WARNING: the tuning modules were not refreshed", err)
        self.assertIn("iempc tuning-install --sha", err)

    def test_a_refresh_that_outlives_its_bound_is_named_as_still_running(self) -> None:
        self.pc.texts["profile.json"] = True

        def still_running(_m):
            raise ip.StillRunning("ssh still running after 540 s (bounded on the PC; check 'iempc status', never force-end)")

        self.on_install = still_running
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["tuning_refresh"], "still-running")
        self.assertIn("the refresh may still run on the PC", err)
        self.assertNotIn("were not refreshed", err)

    def test_a_refresh_still_running_after_a_new_flag_runs_the_event_path(self) -> None:
        self.pc.texts["profile.json"] = True

        def still_running(_m):
            self.flag()
            raise ip.StillRunning("ssh still running after 540 s (bounded on the PC; check 'iempc status', never force-end)")

        self.on_install = still_running
        code, _, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["activate", "status", "event"])

    def test_a_bundle_from_before_the_store_is_a_failed_refresh(self) -> None:
        # #34 (review): activating a bundle from before the store still counts, but
        # its modules never replace the installed ones: the refresh fails before
        # anything of it reaches the PC, naming the missing member.
        self.pc.texts["profile.json"] = True
        shutil.rmtree(ip.bundle_dir(SHA))
        self.gh.artifact = make_zip(self.tmp / "artifact-old" / f"iemmixer-{SHA}.zip",
                                    extra={k: v for k, v in MODULES.items() if k != "tuning/IemTuningStore.psm1"})
        self.fetched()
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["tuning_refresh"], "failed")
        self.assertIn("tuning/IemTuningStore.psm1 is not a listed tuning file", docs[-1]["error"])
        self.assertEqual((self.installs(), [s[1].rsplit("/", 1)[1] for s in self.pc.scps]), ([], [f"iemmode-{SHA}.exe"]))

    def test_an_unanswerable_profile_check_is_a_failed_refresh(self) -> None:
        self.pc.texts["profile.json"] = "ok"
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["tuning_refresh"], "failed")
        self.assertIn("the profile check reads 'ok'", docs[-1]["error"])
        self.assertEqual(self.installs(), [])

    def test_a_bundle_this_box_never_fetched_is_a_failed_refresh(self) -> None:
        self.pc.texts["profile.json"] = True
        shutil.rmtree(ip.bundle_dir(SHA))
        code, docs, err = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["tuning_refresh"], "failed")
        self.assertIn("is not fetched", docs[-1]["error"])
        self.assertEqual((self.installs(), self.pc.scps), ([], []))

    def test_an_offline_activation_is_refreshed_too(self) -> None:
        self.pc.texts["profile.json"] = True
        self.pc.replies[("quit",)] = (0, json.dumps({"ok": True, "mode": "event", "alarms": [], "detail": "the guard stops"}))
        self.guard_counts = [1, 0]
        code, docs, err = self.run_main("activate", "--sha", SHA, "--offline")
        self.assertEqual(code, 0, err)
        self.assertEqual([c[0] for c in self.pc.calls], ["iemmode.exe", "iemmixer-guard.exe", "iemmode.exe"])
        self.assertEqual([next(iter(d)) for d in docs][-3:], ["elevated_bin", "handover", "tuning_refresh"])
        self.assertEqual(docs[-1]["tuning_refresh"], SHA)
        self.assertEqual(self.installs()[0]["keep"], " -KeepProfile")

    def test_a_refused_activation_refreshes_nothing(self) -> None:
        self.pc.texts["profile.json"] = True
        self.pc.replies[("activate", SHA)] = (1, json.dumps({"ok": False, "mode": "event", "alarms": [], "detail": "busy"}))
        code, _, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertEqual((self.profile_checks(), self.installs(), self.pc.scps), ([], [], []))

    def test_a_new_flag_during_the_refresh_runs_the_event_path(self) -> None:
        self.pc.texts["profile.json"] = True

        def install_then_flag(m):
            self.flag()
            return hashes(m)

        self.on_install = install_then_flag
        code, _, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["activate", "status", "event"])

    def test_a_failed_profile_check_after_a_new_flag_runs_the_event_path(self) -> None:
        def ssh_cut():
            self.flag()
            raise ip.StepError("ssh: connection reset")

        self.pc.texts["profile.json"] = ssh_cut
        code, _, _ = self.run_main("activate", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([c[1][0] for c in self.pc.calls], ["activate", "status", "event"])
        self.assertEqual(self.installs(), [])


if __name__ == "__main__":
    unittest.main()
