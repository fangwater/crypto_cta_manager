import importlib.util
import os
from pathlib import Path
import shutil
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "maintainer", Path(__file__).with_name("maintain_pmdaemon_logs.py")
)
maintainer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(maintainer)


class LogReclamationTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.addCleanup(self.root.cleanup)
        self.directory = Path(self.root.name)
        self.path = self.directory / "exec_pt_test-error.log"
        self.payload = b"a" * (3 * 1024 * 1024) + b"recent diagnostic tail\n" * 4096
        self.path.write_bytes(self.payload)

    def test_preview_preserves_all_bytes(self):
        result = maintainer.reclaim_log(self.directory, self.path.name, 1024 * 1024, 65536, False)
        self.assertEqual(result["status"], "would_reclaim")
        self.assertEqual(self.path.read_bytes(), self.payload)

    @unittest.skipUnless(shutil.which("fallocate"), "requires Linux fallocate")
    def test_live_writer_offset_inode_and_tail_survive_reclamation(self):
        before = self.path.stat()
        with self.path.open("r+b", buffering=0) as writer:
            writer.seek(0, os.SEEK_END)
            offset = writer.tell()
            result = maintainer.reclaim_log(self.directory, self.path.name, 1024 * 1024, 65536, True)
            self.assertEqual(result["status"], "reclaimed")
            after = self.path.stat()
            self.assertEqual((before.st_ino, before.st_size), (after.st_ino, after.st_size))
            self.assertLess(after.st_blocks, before.st_blocks)
            self.assertEqual(writer.tell(), offset)
            with self.path.open("rb") as reader:
                reader.seek(-65536, os.SEEK_END)
                self.assertEqual(reader.read(), self.payload[-65536:])
            writer.write(b"next live message\n")
        with self.path.open("rb") as reader:
            reader.seek(offset)
            self.assertEqual(reader.read(), b"next live message\n")

    def test_numbered_wal_and_path_escape_are_refused(self):
        for name in ["000123.log", "LOG.old.123", "../exec_pt_test-error.log"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                maintainer.reclaim_log(self.directory, name, 1024 * 1024, 65536, True)

    def test_symlinks_and_hard_links_are_refused(self):
        link = self.directory / "link-error.log"
        link.symlink_to(self.path)
        with self.assertRaises(OSError):
            maintainer.reclaim_log(self.directory, link.name, 1024 * 1024, 65536, True)
        link.unlink()
        os.link(self.path, link)
        with self.assertRaises(ValueError):
            maintainer.reclaim_log(self.directory, self.path.name, 1024 * 1024, 65536, True)
        self.assertEqual(self.path.read_bytes(), self.payload)

    def test_small_file_and_invalid_limits(self):
        result = maintainer.reclaim_log(self.directory, self.path.name, 8 * 1024 * 1024, 65536, True)
        self.assertEqual(result["status"], "below_threshold")
        with self.assertRaises(ValueError):
            maintainer.reclaim_log(self.directory, self.path.name, 65536, 65536, True)


if __name__ == "__main__":
    unittest.main()
