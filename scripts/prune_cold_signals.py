#!/usr/bin/env python3
"""Discard explicitly unwanted, expired Manager cold signals through the Rust helper.

Preview is the default. Execution needs both --execute and
--discard-without-backup. Never opens the hot RocksDB or manipulates SST files.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import time
import tomllib


def emit(event, **fields):
    print(json.dumps(dict(event=event, **fields)), flush=True)


def overlap(a, b):
    return a.is_relative_to(b) or b.is_relative_to(a)


def environment(args):
    with args.config.open("rb") as stream:
        config = tomllib.load(stream)
    hot = Path(config["kline"]["rocksdb_path"])
    cold = Path(config["archive"]["cold"]["root"])
    if not hot.is_absolute() or not cold.is_absolute() or cold.is_symlink():
        raise RuntimeError("use an existing absolute Manager hot/cold path")
    hot = hot.resolve(strict=True)
    cold = cold.resolve(strict=True)
    if overlap(hot, cold):
        raise RuntimeError("cold root overlaps the hot Manager database")
    for source in config.get("sources", []):
        source = Path(source["rocksdb_path"]).resolve()
        if overlap(cold, source) or overlap(hot, source):
            raise RuntimeError("refuse an Exec database path")
    identity = (hot / "IDENTITY").read_text().strip()
    marker = cold / "COLD_ARCHIVE.json"
    if marker.is_symlink() or not marker.is_file():
        raise RuntimeError("cold root has no regular identity marker")
    if identity != args.expected_source_identity or json.loads(marker.read_text()) != identity:
        raise RuntimeError("Manager/cold source identity does not match the pinned identity")
    return hot, cold


def cutoff(now_us):
    # Same 33-day boundary and ten-minute guard as Rust retention::cutoff.
    day = 86_400_000_000
    return (max(1, (now_us - 33 * day) // day * day - 600_000_000), 0)


def validate_segment(segment, identity):
    if segment.get("source_identity") != identity:
        raise RuntimeError("foreign cold segment")
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,160}", segment.get("id", "")):
        raise RuntimeError("invalid cold segment identity")
    first, last = segment["first"], segment["last"]
    if any(len(key) != 2 or not all(type(n) is int and n >= 0 for n in key)
           for key in (first, last)):
        raise RuntimeError("invalid cold segment range")
    if tuple(first) > tuple(last) or tuple(last) >= cutoff(time.time_ns() // 1000):
        raise RuntimeError("refuse retained or unordered signals")


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def save_pending(path, segment):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent,
                                     prefix=".discard-pending-", delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(segment, stream, separators=(",", ":"))
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    try:
        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def invoke(args, cold, action, remaining, receipt=None, limit=None):
    command = [str(args.binary), action, "--root", str(cold)]
    if limit is not None:
        command.extend(["--limit", str(limit)])
    if receipt is not None:
        command.extend(["--receipt", "-"])
    result = subprocess.run(command, input=json.dumps(receipt) if receipt is not None else None,
                            capture_output=True, text=True, timeout=max(0.1, remaining))
    if result.returncode:
        raise RuntimeError(f"{action} failed (exit={result.returncode}): "
                           + result.stderr.strip()[:2048])
    if len(result.stdout) > 64 * 1024 * 1024:
        raise RuntimeError("cold inventory exceeds metadata limit")
    return json.loads(result.stdout)


def prune(args):
    if args.execute != args.discard_without_backup:
        raise RuntimeError("execution requires both --execute and --discard-without-backup")
    if not args.config.is_absolute() or not args.binary.is_absolute():
        raise RuntimeError("config and Rust helper paths must be absolute")
    if not 1 <= args.max_segments <= 100 or not 1 <= args.max_seconds <= 300:
        raise RuntimeError("invalid bounded segment/time limit")
    hot, cold = environment(args)
    pending = cold / ".discard-pending.json"
    lock_fd = os.open(cold / ".discard-worker.lock",
                      os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(lock_fd).st_mode):
            raise RuntimeError("discard job lock is not a regular file")
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            emit("cold_discard_deferred", reason="another discard job is running")
            return
        deadline = time.monotonic() + args.max_seconds
        removed = records = 0

        def remaining():
            return deadline - time.monotonic()

        def remove(segment, resumed=False):
            nonlocal removed, records
            validate_segment(segment, args.expected_source_identity)
            if environment(args) != (hot, cold):
                raise RuntimeError("configured storage changed during cleanup")
            # Persist before deletion, so timeout/lost acknowledgement can be
            # retried through Rust's exact durable retirement receipt.
            save_pending(pending, segment)
            result = invoke(args, cold, "cold-delete", remaining(), receipt=segment)
            if result != {"removed": True}:
                raise RuntimeError("Rust helper did not confirm exact segment retirement")
            pending.unlink()
            sync_dir(cold)
            removed += 1
            records += segment["count"]
            emit("cold_segment_discarded", segment=segment["id"], records=segment["count"],
                 resumed_pending=resumed, backup_required=False)

        prior = None
        if pending.exists() or pending.is_symlink():
            if pending.is_symlink() or not pending.is_file() or pending.stat().st_size > 64 * 1024 * 1024:
                raise RuntimeError("unsafe pending retirement receipt")
            prior = json.loads(pending.read_text())
            validate_segment(prior, args.expected_source_identity)
            if args.execute:
                remove(prior, resumed=True)
        segments = []
        if removed < args.max_segments and remaining() > 0:
            segments = invoke(args, cold, "cold-list", remaining(),
                              limit=args.max_segments - removed)
            previous = None
            for segment in segments:
                validate_segment(segment, args.expected_source_identity)
                if previous is not None and tuple(segment["first"]) <= previous:
                    raise RuntimeError("cold inventory overlaps or is not oldest-first")
                previous = tuple(segment["last"])
        if not args.execute:
            emit("cold_discard_preview", source_identity=args.expected_source_identity,
                 planned_segments=len(segments), planned_records=sum(s["count"] for s in segments),
                 pending_retirement=prior is not None, would_discard_without_backup=True)
            return
        for segment in segments:
            if remaining() <= 0:
                break
            remove(segment)
        emit("cold_discard_complete", source_identity=args.expected_source_identity,
             removed_segments=removed, removed_records=records, backup_required=False,
             next_run_continues_oldest_first=True)
    finally:
        os.close(lock_fd)


def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--expected-source-identity", required=True)
    parser.add_argument("--max-segments", type=int, default=32)
    parser.add_argument("--max-seconds", type=float, default=45)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--discard-without-backup", action="store_true")
    args = parser.parse_args()
    try:
        prune(args)
    except Exception as error:
        emit("cold_discard_failed", error_type=type(error).__name__,
             error=str(error) if isinstance(error, RuntimeError) else "operation failed; pending receipt retained")
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
