import importlib.util
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "journal_limits", Path(__file__).with_name("apply_el01_journald_limits.py")
)
limits = importlib.util.module_from_spec(spec)
spec.loader.exec_module(limits)


class JournalLimitsTests(unittest.TestCase):
    def setUp(self):
        reporter = patch.object(limits, "progress")
        self.progress = reporter.start()
        self.addCleanup(reporter.stop)

    def test_archive_preserves_closed_binary_journal_exactly(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "journals"
            source.mkdir()
            payload = bytes(range(256)) * 8192
            (source / "system@old.journal").write_bytes(payload)
            (source / "system.journal").write_bytes(b"active: must not be archived")
            with patch.object(limits, "JOURNALS", source):
                paths = limits.archived_files()
                proof = limits.archive(paths, root / "archive.tar.gz")
            self.assertEqual(set(proof["members"]), {"system@old.journal"})
            self.assertEqual(proof["members"]["system@old.journal"],
                             limits.hashlib.sha256(payload).hexdigest())
            self.assertEqual((source / "system@old.journal").read_bytes(), payload)

    def test_archive_refuses_unexpected_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "system@old.journal").symlink_to("/etc/passwd")
            with patch.object(limits, "JOURNALS", root), self.assertRaises(RuntimeError):
                limits.archived_files()

    def test_later_config_override_is_rejected(self):
        config = limits.LIMITS.decode()
        limits.verify_limits(config)
        with self.assertRaises(RuntimeError):
            limits.verify_limits(config + "\n[Journal]\nSystemMaxUse=8G\n")
        with self.assertRaises(RuntimeError):
            limits.verify_limits(config.replace("[Journal]", "[Other]"))

    def test_failed_archive_cannot_produce_verified_result(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            journal = root / "system@old.journal"
            journal.write_bytes(b"binary journal")
            with patch.object(limits, "JOURNALS", root), \
                    patch.object(limits, "run", side_effect=RuntimeError("archive failed")), \
                    self.assertRaises(RuntimeError):
                limits.archive([journal], root / "missing.tar.gz")
            self.assertTrue(journal.exists())

    def test_rotation_header_change_retries_then_verifies_exact_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            journal = root / "system@old.journal"
            journal.write_bytes(b"before delayed journal header update")
            original_run = limits.run
            compressions = []

            def change_after_tar(*args, **kwargs):
                result = original_run(*args, **kwargs)
                if args[0] == "tar":
                    compressions.append(True)
                    if len(compressions) == 1:
                        journal.write_bytes(b"after delayed journal header update")
                return result

            with patch.object(limits, "JOURNALS", root), \
                    patch.object(limits, "run", side_effect=change_after_tar), \
                    patch.object(limits.time, "sleep"):
                proof = limits.archive([journal], root / "saved.tar.gz")
            self.assertEqual(len(compressions), 2)
            self.assertEqual(proof["members"][journal.name],
                             limits.hashlib.sha256(journal.read_bytes()).hexdigest())
            self.assertFalse((root / "saved.tar.gz.partial").exists())

    def test_continuous_source_change_stops_without_verified_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            journal = root / "system@old.journal"
            journal.write_bytes(b"journal")
            original_run = limits.run
            compressions = []

            def always_change_after_tar(*args, **kwargs):
                result = original_run(*args, **kwargs)
                if args[0] == "tar":
                    compressions.append(True)
                    with journal.open("ab") as output:
                        output.write(b"changing")
                return result

            with patch.object(limits, "JOURNALS", root), \
                    patch.object(limits, "run", side_effect=always_change_after_tar), \
                    patch.object(limits.time, "sleep"), \
                    self.assertRaisesRegex(RuntimeError, "bounded retries"):
                limits.archive([journal], root / "saved.tar.gz")
            self.assertEqual(len(compressions), 3)
            self.assertFalse((root / "saved.tar.gz").exists())

    def test_command_timeout_reaps_child_and_reports_progress(self):
        with self.assertRaisesRegex(RuntimeError, "exceeded"):
            limits.run("/usr/bin/python3", "-c", "import time; time.sleep(30)", timeout=0.05)
        self.progress.assert_called()

    def test_service_children_are_separate_and_every_exec_is_strict(self):
        rows = [
            [10, "/bin/postgres", "100", 1],
            [11, "/bin/postgres", "101", 10],
            [20, "/bin/nginx", "200", 1],
            [21, "/bin/nginx", "201", 20],
            [30, "/bin/redis-server", "300", 1],
            [31, "/bin/redis-server", "301", 30],
            [40, "/bin/trade_engine", "400", 1],
            [41, "/bin/trade_engine", "401", 40],
        ]
        stable, auxiliary = limits.split_processes(rows)
        self.assertEqual([row[0] for row in stable], [10, 20, 30, 40, 41])
        self.assertEqual([row[0] for row in auxiliary], [11, 21, 31])

    def snapshot(self, auxiliary_pid=11):
        return {"processes": [[10, "/bin/postgres", "100"],
                              [40, "/bin/cta_web", "400"],
                              [41, "/bin/trade_engine", "410"]],
                "auxiliary_processes": [[auxiliary_pid, "/bin/postgres", "500", 10]],
                "manager_identity": "manager-db-identity",
                "manager_config_sha256": "immutable-config-digest"}

    def test_backend_churn_passes_but_service_restart_and_data_changes_fail(self):
        before = self.snapshot()
        self.assertTrue(limits.protected_diff(before, self.snapshot(12))["unchanged"])
        for key in ["manager_identity", "manager_config_sha256"]:
            changed = copy.deepcopy(before)
            changed[key] = "changed"
            self.assertFalse(limits.protected_diff(before, changed)["unchanged"])
        for index in range(len(before["processes"])):
            changed = copy.deepcopy(before)
            changed["processes"][index][2] = "new-start-time-with-reused-pid"
            self.assertFalse(limits.protected_diff(before, changed)["unchanged"])

    def exercise_apply(self, snapshots, expected_error=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            journals = root / "journals"
            journals.mkdir()
            (journals / "system@closed.journal").write_bytes(b"immutable binary journal")
            backup = root / "backup"
            backup.mkdir()
            config = root / "drop-in" / "90-test.conf"
            original_run = limits.run
            commands = []

            def fake_system_commands(*args, **kwargs):
                commands.append(args)
                if args[0] in {"tar", "gzip"}:
                    return original_run(*args, **kwargs)
                if args[0] == "systemd-analyze":
                    return config.read_text()
                if "--disk-usage" in args:
                    return "Archived and active journals take up 1.0G in the file system."
                return ""

            with patch.object(limits, "JOURNALS", journals), \
                    patch.object(limits, "CONFIG", config), \
                    patch.object(limits, "create_backup_directory", return_value=backup), \
                    patch.object(limits, "protected", side_effect=snapshots), \
                    patch.object(limits, "run", side_effect=fake_system_commands), \
                    patch("builtins.print"):
                if expected_error:
                    with self.assertRaisesRegex(RuntimeError, expected_error):
                        limits.apply()
                    self.assertFalse(config.exists())
                    self.assertFalse((backup / "result.json").exists())
                else:
                    limits.apply()
                    self.assertEqual(config.read_bytes(), limits.LIMITS)
                    result = json.loads((backup / "result.json").read_text())
                    self.assertTrue(result["applied"])
                    self.assertTrue(result["protected_state_unchanged"])
            self.assertTrue((backup / "archives-verified.json").exists())
            self.assertTrue((backup / "protected-before-install.json").exists())
            return commands

    def test_entire_apply_accepts_connection_churn_at_both_guards(self):
        commands = self.exercise_apply([self.snapshot(11), self.snapshot(12), self.snapshot(13)])
        self.assertEqual(sum(args[:2] == ("systemctl", "restart") for args in commands), 1)

    def test_entire_apply_stops_before_installation_on_real_exec_restart(self):
        before = self.snapshot()
        changed = copy.deepcopy(before)
        changed["processes"][-1][2] = "restarted"
        commands = self.exercise_apply([before, changed], "protected service")
        self.assertFalse(any(args[:2] == ("systemctl", "restart") for args in commands))

    def test_entire_apply_rolls_back_if_manager_changes_after_installation(self):
        before = self.snapshot()
        changed = copy.deepcopy(before)
        changed["manager_identity"] = "unexpected-replacement-db"
        commands = self.exercise_apply([before, self.snapshot(12), changed], "protected service")
        self.assertEqual(sum(args[:2] == ("systemctl", "restart") for args in commands), 2)


if __name__ == "__main__":
    unittest.main()
