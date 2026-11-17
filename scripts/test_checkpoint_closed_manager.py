import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

spec = importlib.util.spec_from_file_location("checkpoint_closed_manager", Path(__file__).with_name("checkpoint_closed_manager.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ClosedCheckpointTests(unittest.TestCase):
    def fixture(self, root):
        db = root / "db"
        db.mkdir()
        for name, data in {"IDENTITY":b"fixture-id", "CURRENT":b"MANIFEST-000001\n", "MANIFEST-000001":b"manifest", "OPTIONS-000002":b"options", "LOCK":b"", "000003.sst":b"immutable", "LOG":b"diagnostic"}.items():
            (db/name).write_bytes(data)
        for i in range(4, 8):
            p=db/f"{i:06}.log";p.write_bytes(f"wal-{i}".encode());os.utime(p,(time.time()-60,time.time()-60))
        return db

    def test_mutable_files_are_independent_and_changed_precopy_is_recopied(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);db=self.fixture(root);backup=root/"backup"
            module.precopy(db,backup)
            (db/"000004.log").write_bytes(b"new-4")
            module.capture(db,backup,"fixture-id")
            module.verify(backup)
            saved=backup/"db"
            self.assertEqual((saved/"000004.log").read_bytes(),b"new-4")
            for name in ("000004.log","000006.log","MANIFEST-000001","CURRENT","LOCK"):
                self.assertNotEqual((saved/name).stat().st_ino,(db/name).stat().st_ino)
            self.assertEqual((saved/"000003.sst").stat().st_ino,(db/"000003.sst").stat().st_ino)
            (db/"MANIFEST-000001").write_bytes(b"later manifest")
            (db/"000007.log").write_bytes(b"later wal")
            self.assertEqual((saved/"MANIFEST-000001").read_bytes(),b"manifest")
            self.assertEqual((saved/"000007.log").read_bytes(),b"wal-7")
            self.assertFalse((saved/"LOG").exists())
            module.verify(backup)

    def test_active_owner_lock_refuses_capture(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);db=self.fixture(root);backup=root/"backup"
            module.precopy(db,backup)
            code="import fcntl,sys; f=open(sys.argv[1],'r+'); fcntl.lockf(f,fcntl.LOCK_EX); print('locked',flush=True); sys.stdin.read()"
            p=subprocess.Popen([sys.executable,"-c",code,str(db/"LOCK")],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
            try:
                self.assertEqual(p.stdout.readline().strip(),"locked")
                with self.assertRaises(BlockingIOError):module.capture(db,backup,"fixture-id")
                self.assertFalse((backup/"CLOSED_CHECKPOINT.json").exists())
            finally:
                p.communicate(timeout=5)

    def test_identity_change_and_corrupt_copy_refuse_success(self):
        with tempfile.TemporaryDirectory() as raw:
            root=Path(raw);db=self.fixture(root);backup=root/"backup"
            module.precopy(db,backup)
            with self.assertRaises(RuntimeError):module.capture(db,backup,"wrong-id")
            module.capture(db,backup,"fixture-id")
            (backup/"db/000004.log").write_bytes(b"bad-4")
            with self.assertRaises(RuntimeError):module.verify(backup)


if __name__=="__main__":unittest.main()
