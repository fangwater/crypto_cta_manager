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


if __name__ == "__main__":
    unittest.main()
