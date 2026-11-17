import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "prune_cold_signals", Path(__file__).with_name("prune_cold_signals.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
NATIVE = os.environ.get("CTA_COLD_TEST_BINARY")


class PruneGuards(unittest.TestCase):
    def fixture(self, root):
        hot, cold = root / "hot", root / "cold"
        hot.mkdir()
        cold.mkdir()
        (hot / "IDENTITY").write_text("expected-id\n")
        (cold / "COLD_ARCHIVE.json").write_text(json.dumps("expected-id"))
        config = root / "manager.toml"
        config.write_text(f'[kline]\nrocksdb_path = "{hot}"\n'
                          f'[archive.cold]\nroot = "{cold}"\n')
        return argparse.Namespace(config=config, binary=Path("/unused/cta_position_archive"),
                                  expected_source_identity="expected-id", max_segments=32,
                                  max_seconds=45, execute=False, discard_without_backup=False)

    def test_foreign_identity_and_exec_overlap_fail_before_helper(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            args = self.fixture(root)
            with mock.patch.object(module, "invoke") as call:
                args.expected_source_identity = "other-id"
                with self.assertRaisesRegex(RuntimeError, "identity"):
                    module.prune(args)
                args.expected_source_identity = "expected-id"
                with args.config.open("a") as stream:
                    stream.write(f'\n[[sources]]\nrocksdb_path = "{root / "cold"}"\n')
                with self.assertRaisesRegex(RuntimeError, "Exec"):
                    module.prune(args)
                call.assert_not_called()

    def test_execution_requires_explicit_discard_opt_in(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = self.fixture(Path(temporary))
            args.execute = True
            with mock.patch.object(module, "invoke") as call:
                with self.assertRaisesRegex(RuntimeError, "both"):
                    module.prune(args)
                call.assert_not_called()

    def test_cold_symlink_and_hot_overlap_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            args = self.fixture(root)
            link = root / "cold-link"
            link.symlink_to(root / "cold", target_is_directory=True)
            text = args.config.read_text()
            args.config.write_text(text.replace(str(root / "cold"), str(link)))
            with self.assertRaisesRegex(RuntimeError, "absolute"):
                module.environment(args)
            args.config.write_text(text.replace(str(root / "cold"), str(root / "hot")))
            with self.assertRaisesRegex(RuntimeError, "overlaps"):
                module.environment(args)

    def test_recent_signal_cannot_be_discarded(self):
        now = 100 * 86_400_000_000
        with mock.patch.object(module.time, "time_ns", return_value=now * 1000):
            segment = dict(id="segment", source_identity="expected-id",
                           first=[now - 86_400_000_000, 0], last=[now, 0])
            with self.assertRaisesRegex(RuntimeError, "retained"):
                module.validate_segment(segment, "expected-id")


@unittest.skipUnless(NATIVE, "set CTA_COLD_TEST_BINARY for native RocksDB integration")
class NativePrune(unittest.TestCase):
    def fixture(self, parent):
        root = parent / "fixture"
        result = subprocess.run([NATIVE, "rehearsal-cold", "--root", str(root)],
                                capture_output=True, text=True, check=True)
        proof = json.loads(result.stdout)
        config = parent / "manager.toml"
        config.write_text(f'[kline]\nrocksdb_path = "{root / "hot"}"\n'
                          f'[archive.cold]\nroot = "{root / "cold"}"\n')
        args = argparse.Namespace(config=config, binary=Path(NATIVE),
                                  expected_source_identity=proof["source_identity"],
                                  max_segments=32, max_seconds=45,
                                  execute=False, discard_without_backup=False)
        return root, args, proof

    def hot_hashes(self, root):
        return {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                for p in (root / "hot").iterdir() if p.is_file()}

    def test_preview_and_real_discard_preserve_hot_and_boundary(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("CTA_COLD_TEST_ROOT")) as temporary:
            root, args, proof = self.fixture(Path(temporary))
            before = self.hot_hashes(root)
            segment = proof["segment"]
            with contextlib.redirect_stdout(io.StringIO()):
                module.prune(args)
            self.assertTrue((root / "cold" / segment["id"]).is_dir())
            args.execute = args.discard_without_backup = True
            with contextlib.redirect_stdout(io.StringIO()):
                module.prune(args)
                module.prune(args)
            self.assertFalse((root / "cold" / segment["id"]).exists())
            self.assertEqual(json.loads((root / "cold/RETAINED_BOUNDARY.json").read_text()), segment)
            self.assertFalse((root / "cold/.discard-pending.json").exists())
            self.assertEqual(before, self.hot_hashes(root))

    def test_lost_delete_acknowledgement_is_recovered_on_next_run(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("CTA_COLD_TEST_ROOT")) as temporary:
            root, args, proof = self.fixture(Path(temporary))
            args.execute = args.discard_without_backup = True
            before = self.hot_hashes(root)
            original = module.invoke

            def lose_ack(arguments, cold, action, *values, **options):
                result = original(arguments, cold, action, *values, **options)
                if action == "cold-delete":
                    raise RuntimeError("simulated lost acknowledgement after durable deletion")
                return result

            with contextlib.redirect_stdout(io.StringIO()):
                with mock.patch.object(module, "invoke", side_effect=lose_ack):
                    with self.assertRaisesRegex(RuntimeError, "lost acknowledgement"):
                        module.prune(args)
                self.assertTrue((root / "cold/.discard-pending.json").is_file())
                self.assertFalse((root / "cold" / proof["segment"]["id"]).exists())
                module.prune(args)
            self.assertFalse((root / "cold/.discard-pending.json").exists())
            self.assertEqual(before, self.hot_hashes(root))


if __name__ == "__main__":
    unittest.main()
