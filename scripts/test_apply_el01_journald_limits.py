import importlib.util
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


if __name__ == "__main__":
    unittest.main()
