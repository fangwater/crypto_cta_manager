#!/usr/bin/env python3
"""Reclaim old blocks from explicitly selected pmdaemon logs, preserving the tail."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import stat
import subprocess


MIB = 1024 * 1024


def reclaim_log(
    log_dir: Path, filename: str, max_bytes: int, keep_bytes: int, execute: bool
) -> dict:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+\.log", filename):
        raise ValueError("--file must be a single .log filename")
    if not log_dir.is_absolute():
        raise ValueError("--log-dir must be absolute")
    if not 0 < keep_bytes < max_bytes:
        raise ValueError("require 0 < keep bytes < maximum bytes")

    directory = os.open(log_dir, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        access = os.O_RDWR if execute else os.O_RDONLY
        try:
            fd = os.open(
                filename, access | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory
            )
        except FileNotFoundError:
            return {"file": filename, "status": "missing"}
    finally:
        os.close(directory)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid():
            raise ValueError("log must be a regular file owned by the current user")
        if before.st_nlink != 1:
            raise ValueError("refusing a log with multiple hard links")

        result = {
            "file": filename,
            "logical_bytes": before.st_size,
            "allocated_bytes_before": before.st_blocks * 512,
        }
        # Non-append writers retain their offset after manual truncation. Their
        # sparse files can appear huge while occupying very few actual blocks.
        if result["allocated_bytes_before"] <= max_bytes:
            return {**result, "status": "below_threshold"}

        block = os.fstatvfs(fd).f_frsize or 4096
        prefix = max(0, before.st_size - keep_bytes) // block * block
        if prefix == 0:
            return {**result, "status": "no_old_prefix"}
        result["retained_tail_bytes"] = before.st_size - prefix
        if not execute:
            return {**result, "status": "would_reclaim"}

        # Punch only old blocks. KEEP_SIZE preserves both append and non-append
        # writers' existing offsets; the recent tail stays in this same inode.
        # Pass the opened inode, rather than reopening a pathname that can race
        # a supervisor replacement. Unsupported filesystems fail without any
        # fallback to deleting/recreating/truncating the live file.
        subprocess.run(
            [
                "fallocate",
                "--punch-hole",
                "--keep-size",
                "--offset",
                "0",
                "--length",
                str(prefix),
                f"/proc/self/fd/{fd}",
            ],
            pass_fds=(fd,),
            capture_output=True,
            check=True,
        )
        after = os.fstat(fd)
        result["allocated_bytes_after"] = after.st_blocks * 512
        result["reclaimed_bytes"] = max(
            0, result["allocated_bytes_before"] - result["allocated_bytes_after"]
        )
        return {**result, "status": "reclaimed"}
    finally:
        os.close(fd)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log-dir", type=Path, default=Path.home() / ".pmdaemon/logs")
    parser.add_argument("--file", action="append", required=True, help="repeat per log")
    parser.add_argument("--max-mib", type=int, default=256)
    parser.add_argument("--keep-mib", type=int, default=16)
    parser.add_argument("--execute", action="store_true", help="otherwise only inspect")
    args = parser.parse_args()
    if not 0 < args.keep_mib < args.max_mib:
        parser.error("require 0 < --keep-mib < --max-mib")
    if not args.log_dir.is_absolute():
        parser.error("--log-dir must be absolute")

    failed = False
    for filename in dict.fromkeys(args.file):
        try:
            result = reclaim_log(
                args.log_dir,
                filename,
                args.max_mib * MIB,
                args.keep_mib * MIB,
                args.execute,
            )
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            failed = True
            result = {"file": filename, "status": "error", "error": str(error)}
        print(json.dumps(result, ensure_ascii=False))
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
