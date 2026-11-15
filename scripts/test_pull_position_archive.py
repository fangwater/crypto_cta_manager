import contextlib
import copy
import datetime
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import pull_position_archive as pull


class ManualArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "backup"
        self.root.mkdir()
        self.before = "2000-01-01"
        self.boundary = [pull.date_boundary(self.before), 0]
        self.host = {"name": "test", "ssh": "host", "socket": "/private/socket",
                     "binary": "/bin/client", "db": "/manager/db"}
        self.receipt = {"identity": "db", "from": [1, 0], "to": self.boundary,
                        "sha256": "a" * 64, "count": 300, "raw_bytes": 1000000, "previous": None}
        self.status = {"identity": "db", "floor": [1, 0], "eligible_before": self.boundary,
                       "last_receipt": None}
        self.calls = []
        self.fail_transfer = False
        self.fail_verify = False
        self.lose_commit_reply = False
        self.fail_compact = False

    def remote(self, host, action, before=None, data=None, output=subprocess.PIPE):
        self.calls.append(action)
        if action == "export":
            self.assertEqual(before, self.before)
            output.write(b"compressed full date range")
            return subprocess.CompletedProcess([], 1 if self.fail_transfer else 0)
        if action == "commit":
            self.status["floor"] = copy.deepcopy(data["to"])
            self.status["last_receipt"] = copy.deepcopy(data)
            if self.lose_commit_reply:
                return subprocess.CompletedProcess([], 1)
        if action == "compact" and self.fail_compact:
            return subprocess.CompletedProcess([], 1)
        if action == "inspect":
            return subprocess.CompletedProcess([], 0, json.dumps(self.status).encode())
        return subprocess.CompletedProcess([], 0, json.dumps({"ok": True, "status": copy.deepcopy(self.status)}).encode())

    def verify(self, *args):
        self.calls.append("verify")
        if self.fail_verify:
            raise RuntimeError("bad backup")
        return copy.deepcopy(self.receipt)

    def run_phase(self, phase):
        with patch.object(pull, "remote", self.remote), patch.object(pull, "verify", self.verify), contextlib.redirect_stdout(io.StringIO()):
            return phase(self.host, self.root, self.before, Path("/verifier"))

    def test_backup_and_reclaim_are_separate_full_range_operations(self):
        self.run_phase(pull.backup)
        self.assertEqual(self.calls, ["status", "export", "verify"])
        self.assertEqual(self.status["floor"], [1, 0])
        self.assertTrue(pull.backup_path(self.root / "test", self.before).is_file())
        self.calls.clear()
        self.run_phase(pull.reclaim)
        self.assertEqual(self.calls, ["verify", "status", "commit", "compact"])
        self.assertEqual(self.status["floor"], self.boundary)

    def test_missing_or_failed_backup_never_deletes(self):
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.reclaim)
        self.assertEqual(self.calls, [])
        for flag in ["fail_transfer", "fail_verify"]:
            setattr(self, flag, True)
            self.calls.clear()
            with self.assertRaises(RuntimeError):
                self.run_phase(pull.backup)
            self.assertNotIn("commit", self.calls)
            self.assertNotIn("compact", self.calls)
            self.assertFalse(pull.backup_path(self.root / "test", self.before).exists())
            setattr(self, flag, False)

    def test_corrupt_saved_backup_blocks_reclamation(self):
        self.run_phase(pull.backup)
        self.calls.clear()
        self.fail_verify = True
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.reclaim)
        self.assertEqual(self.calls, ["verify"])

    def test_lost_commit_reply_retries_verification_and_compaction_without_second_delete(self):
        self.run_phase(pull.backup)
        self.lose_commit_reply = True
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.reclaim)
        self.assertEqual(self.status["floor"], self.boundary)
        self.calls.clear()
        self.lose_commit_reply = False
        self.run_phase(pull.reclaim)
        self.assertEqual(self.calls, ["verify", "status", "compact"])

    def test_failed_compaction_remains_retriable_after_range_was_deleted(self):
        self.run_phase(pull.backup)
        self.fail_compact = True
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.reclaim)
        self.assertFalse((self.root / "test" / f"before-{self.before}.reclaimed.json").exists())
        self.calls.clear()
        self.fail_compact = False
        self.run_phase(pull.reclaim)
        self.assertEqual(self.calls, ["verify", "status", "compact"])
        self.assertTrue((self.root / "test" / f"before-{self.before}.reclaimed.json").exists())

    def test_changed_identity_or_prefix_stops_before_commit(self):
        self.run_phase(pull.backup)
        for status in [{**self.status, "identity": "replacement"},
                       {**self.status, "floor": [2, 0]}]:
            saved = self.status
            self.status = status
            self.calls.clear()
            with self.assertRaises(RuntimeError):
                self.run_phase(pull.reclaim)
            self.assertEqual(self.calls, ["verify", "status"])
            self.status = saved

    def test_repeating_backup_reuses_verified_file_without_export(self):
        self.run_phase(pull.backup)
        self.calls.clear()
        self.run_phase(pull.backup)
        self.assertEqual(self.calls, ["status", "verify"])

    def test_wrong_date_manifest_or_export_boundary_is_rejected(self):
        original = copy.deepcopy(self.receipt)
        self.receipt["to"] = [self.boundary[0] + 1, 0]
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.backup)
        self.receipt = original
        self.run_phase(pull.backup)
        path = pull.backup_path(self.root / "test", self.before)
        value = json.loads(path.read_text())
        value["before"] = "2000-01-02"
        path.write_text(json.dumps(value))
        self.calls.clear()
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.reclaim)
        self.assertEqual(self.calls, [])

    def test_too_recent_date_and_new_identity_after_pruning_are_rejected(self):
        self.status["eligible_before"] = [self.boundary[0] - 1, 0]
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.backup)
        self.assertEqual(self.calls, ["status"])
        self.status["eligible_before"] = self.boundary
        self.status["floor"] = [2, 0]
        self.calls.clear()
        with self.assertRaises(RuntimeError):
            self.run_phase(pull.backup)
        self.assertNotIn("export", self.calls)

    def test_default_cli_only_plans_and_creates_no_backup_directories(self):
        new_root = self.root / "absent"
        config = Path(self.temporary.name) / "manual.toml"
        config.write_text(f'backup_root = "{new_root}"\nverifier = "/verifier"\n[[hosts]]\n'
                          'name = "test"\nssh = "host"\ndb = "/manager/db"\n'
                          'socket = "/private/socket"\nbinary = "/bin/client"\n')
        with patch("sys.argv", ["pull", "--config", str(config), "--host", "test", "--before", self.before]), \
                patch.object(pull, "remote", self.remote), contextlib.redirect_stdout(io.StringIO()):
            pull.main()
        self.assertEqual(self.calls, ["inspect"])
        self.assertFalse(new_root.exists())

    def test_date_is_exclusive_shanghai_midnight_and_canonical(self):
        expected = datetime.datetime(2026, 9, 30, 16, tzinfo=datetime.timezone.utc)
        self.assertEqual(pull.date_boundary("2026-10-01"), int(expected.timestamp()) * 1000000)
        for date in ["2026-1-1", "2026-02-30", "2026-10-01T00:00:00", "1969-12-31"]:
            with self.assertRaises(ValueError):
                pull.date_boundary(date)


if __name__ == "__main__":
    unittest.main()
