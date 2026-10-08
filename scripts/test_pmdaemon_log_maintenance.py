#!/usr/bin/env python3
from __future__ import annotations

import os
from pathlib import Path
import tempfile
import unittest

from maintain_pmdaemon_logs import MIB, reclaim_log


class PmdaemonLogMaintenanceTests(unittest.TestCase):
    def test_reclaims_space_preserving_tail_inode_and_nonappend_writer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "exec-error.log"
            tail = b"recent diagnostic line\n" * 60_000
            with path.open("wb") as writer:
                writer.write(b"old diagnostic line\n" * 180_000)
                writer.write(tail)
                writer.flush()
                original_size = writer.tell()
                original_inode = path.stat().st_ino

                result = reclaim_log(root, path.name, 4 * MIB, 2 * MIB, True)
                self.assertEqual(result["status"], "reclaimed")
                self.assertGreater(result["reclaimed_bytes"], 2 * MIB)
                self.assertEqual(path.stat().st_ino, original_inode)
                self.assertEqual(path.stat().st_size, original_size)
                with path.open("rb") as reader:
                    reader.seek(original_size - len(tail))
                    self.assertEqual(reader.read(), tail)

                writer.write(b"writer continues normally\n")
                writer.flush()
                with path.open("rb") as reader:
                    reader.seek(original_size)
                    self.assertEqual(reader.read(), b"writer continues normally\n")

                # Repeated runs below the actual allocation budget keep data.
                self.assertEqual(
                    reclaim_log(root, path.name, 4 * MIB, 2 * MIB, True)["status"],
                    "below_threshold",
                )

    def test_dry_run_does_not_change_data(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "exec-error.log"
            data = b"diagnostic\n" * 300_000
            path.write_bytes(data)
            result = reclaim_log(root, path.name, 2 * MIB, MIB, False)
            self.assertEqual(result["status"], "would_reclaim")
            self.assertEqual(path.read_bytes(), data)

    def test_sparse_file_uses_actual_disk_blocks_for_threshold(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "exec-error.log"
            with path.open("wb") as writer:
                writer.seek(25 * 1024 * MIB)
                writer.write(b"latest diagnostic\n")
            result = reclaim_log(root, path.name, 2 * MIB, MIB, True)
            self.assertEqual(result["status"], "below_threshold")
            with path.open("rb") as reader:
                reader.seek(25 * 1024 * MIB)
                self.assertEqual(reader.read(), b"latest diagnostic\n")

    def test_rejects_symlinks_hardlinks_and_paths_outside_log_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            valuable = root / "records"
            data = b"immutable history\n" * 200_000
            valuable.write_bytes(data)
            link = root / "linked.log"
            link.symlink_to(valuable)
            with self.assertRaises(OSError):
                reclaim_log(root, link.name, 2 * MIB, MIB, True)
            link.unlink()
            os.link(valuable, link)
            with self.assertRaises(ValueError):
                reclaim_log(root, link.name, 2 * MIB, MIB, True)
            with self.assertRaises(ValueError):
                reclaim_log(root, "../linked.log", 2 * MIB, MIB, True)
            self.assertEqual(valuable.read_bytes(), data)


if __name__ == "__main__":
    unittest.main()
