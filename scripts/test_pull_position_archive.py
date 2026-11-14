import contextlib
import copy
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import pull_position_archive as pull


class OffloadFailureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.host = {"name": "test", "ssh": "host", "socket": "/private/socket", "binary": "/bin/client"}
        self.receipt = {"identity": "db", "from": [1, 0], "to": [100, 0],
                        "sha256": "digest", "count": 3, "raw_bytes": 1000, "previous": None}
        self.status = {"identity": "db", "floor": [1, 0], "eligible_before": [100, 0], "last_receipt": None}
        self.calls = []
        self.fail_commit = False
        self.fail_transfer = False
        self.fail_restore = False

    def remote(self, host, action, data=None, output=subprocess.PIPE):
        self.calls.append(action)
        if action == "export":
            if self.fail_transfer:
                return subprocess.CompletedProcess([], 1)
            if self.status["floor"] == [100, 0]:
                return subprocess.CompletedProcess([], 3)
            output.write(b"compressed original records")
            return subprocess.CompletedProcess([], 0)
        if action == "commit":
            if self.fail_commit:
                return subprocess.CompletedProcess([], 1)
            self.status["floor"] = [100, 0]
            self.status["last_receipt"] = data
        return subprocess.CompletedProcess([], 0, json.dumps({"ok": True, "status": copy.deepcopy(self.status)}).encode())

    def verify(self, argv, **kwargs):
        self.calls.append("verify")
        if self.fail_restore:
            raise subprocess.CalledProcessError(1, argv)
        return subprocess.CompletedProcess(argv, 0, json.dumps(self.receipt).encode())

    def run_pull(self, apply):
        with patch.object(pull, "remote", self.remote), patch.object(pull.subprocess, "run", self.verify), contextlib.redirect_stdout(io.StringIO()):
            pull.pull_host(self.host, self.root, Path("/verifier"), apply, 2, False)

    def test_backup_only_never_prunes_and_retains_durable_receipt(self):
        self.run_pull(False)
        self.assertNotIn("commit", self.calls)
        self.assertTrue((self.root / "test/pending.json").is_file())
        self.assertTrue((self.root / "test/chunks/digest.signals.gz").is_file())

    def test_failed_transfer_or_restore_never_commits(self):
        for fail in ["fail_transfer", "fail_restore"]:
            setattr(self, fail, True)
            with self.assertRaises(Exception):
                self.run_pull(True)
            self.assertNotIn("commit", self.calls)
            self.assertFalse((self.root / "test/pending.json").exists())
            setattr(self, fail, False)

    def test_lost_commit_reply_resumes_only_after_backup_reverification(self):
        self.fail_commit = True
        with self.assertRaises(RuntimeError):
            self.run_pull(True)
        self.assertTrue((self.root / "test/pending.json").exists())
        self.calls.clear()
        self.fail_commit = False
        self.run_pull(True)
        self.assertEqual(self.calls[:3], ["status", "verify", "commit"])
        self.assertFalse((self.root / "test/pending.json").exists())
        self.assertEqual(self.status["last_receipt"], self.receipt)

    def test_store_identity_change_stops_before_export_and_commit(self):
        self.run_pull(False)
        self.status["identity"] = "replacement-db"
        self.calls.clear()
        with self.assertRaises(RuntimeError):
            self.run_pull(True)
        self.assertEqual(self.calls, ["status"])


if __name__ == "__main__":
    unittest.main()
