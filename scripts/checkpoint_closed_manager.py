#!/usr/bin/env python3
"""Precopy immutable WALs, then snapshot an exclusively locked, closed Manager DB.

No RocksDB owner is opened. SST/blob files are linked only after Manager stops;
WAL/manifest/options files are independent copies. Expensive hashing is optional
and never part of the publication outage. Does not operate on Exec databases.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time
import tomllib


def emit(stage, **fields):
    print(json.dumps(dict(stage=stage, **fields)), flush=True)


def durable_json(path, obj):
    temporary = path.with_name(path.name + ".next")
    with temporary.open("x") as stream:
        json.dump(obj, stream, sort_keys=True, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    sync_dir(path.parent)


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def stamp(metadata):
    return [metadata.st_dev, metadata.st_ino, metadata.st_size,
            metadata.st_mtime_ns, metadata.st_ctime_ns]


def identity(db):
    return (db / "IDENTITY").read_text().strip()


def files(db):
    result = []
    for path in db.iterdir():
        if path.name == "LOCK" or path.name == "LOG" or path.name.startswith("LOG.old."):
            continue
        if path.name.endswith(".dbtmp"):
            continue
        mode = path.lstat().st_mode
        if not stat.S_ISREG(mode):
            raise RuntimeError("unsupported database entry: " + path.name)
        result.append(path)
    return sorted(result)


def copy_checked(source, destination):
    digest = hashlib.sha256()
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        with os.fdopen(fd, "rb", closefd=False) as incoming:
            before = stamp(os.fstat(fd))
            with destination.open("xb") as outgoing:
                remaining = before[2]
                while remaining:
                    data = incoming.read(min(4 * 1024 * 1024, remaining))
                    if not data:
                        raise RuntimeError("source truncated during copy")
                    outgoing.write(data)
                    digest.update(data)
                    remaining -= len(data)
                outgoing.flush()
                os.fsync(outgoing.fileno())
            after = stamp(os.fstat(fd))
    finally:
        os.close(fd)
    return dict(source_stamp=before, unchanged=before == after,
                bytes=before[2], sha256=digest.hexdigest())


def precopy(db, backup):
    backup.mkdir(mode=0o700)
    cache = backup / "wal-precopy"
    cache.mkdir(mode=0o700)
    inventory = dict(identity=identity(db), source=str(db), files={})
    wals = sorted(p for p in files(db) if re.fullmatch(r"\d+\.log", p.name))
    copied = 0
    # Exclude the newest two WALs, and anything recently modified. They are
    # copied in full after shutdown, never reused on an append-only assumption.
    for path in wals[:-2]:
        if time.time() - path.stat().st_mtime < 30:
            continue
        info = copy_checked(path, cache / path.name)
        inventory["files"][path.name] = info
        copied += info["bytes"]
        if len(inventory["files"]) % 8 == 0:
            emit("precopy", files=len(inventory["files"]), bytes=copied)
    durable_json(backup / "PRECOPY.json", inventory)
    emit("precopy_complete", files=len(inventory["files"]), bytes=copied)


def capture(db, backup, expected_identity):
    inventory = json.loads((backup / "PRECOPY.json").read_text())
    if identity(db) != expected_identity or inventory["identity"] != expected_identity:
        raise RuntimeError("Manager DB identity changed")
    if inventory["source"] != str(db):
        raise RuntimeError("precopy source mismatch")
    # RocksDB's POSIX lock uses fcntl. Never remove or replace its LOCK file.
    fd = os.open(db / "LOCK", os.O_RDWR | os.O_NOFOLLOW)
    try:
        fcntl.lockf(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        started = time.monotonic()
        pending = backup / "db.incomplete"
        pending.mkdir(mode=0o700)
        manifest = dict(identity=expected_identity, source=str(db), files={})
        copied = linked = reused = 0
        for path in files(db):
            destination = pending / path.name
            original = stamp(path.stat())
            if path.suffix in (".sst", ".blob"):
                os.link(path, destination, follow_symlinks=False)
                info = dict(kind="immutable_link", bytes=original[2], source_stamp=original)
                linked += 1
            else:
                cached = inventory["files"].get(path.name)
                cached_path = backup / "wal-precopy" / path.name
                if cached and cached["unchanged"] and cached["source_stamp"] == original:
                    if cached_path.stat().st_size != original[2]:
                        raise RuntimeError("precopy size changed")
                    os.replace(cached_path, destination)
                    info = dict(cached, kind="independent_precopy")
                    reused += 1
                else:
                    if original[2] > 256 * 1024 * 1024:
                        raise RuntimeError("unexpected large mutable database file")
                    info = dict(copy_checked(path, destination), kind="independent_copy")
                    if not info["unchanged"]:
                        raise RuntimeError("closed database changed during capture")
                    copied += original[2]
            if stamp(path.stat())[:5] != original and info["kind"] != "immutable_link":
                raise RuntimeError("closed source file changed")
            manifest["files"][path.name] = info
        (pending / "LOCK").touch(mode=0o600)
        sync_dir(pending)
        if identity(db) != expected_identity:
            raise RuntimeError("source identity changed during capture")
        os.rename(pending, backup / "db")
        sync_dir(backup)
        manifest.update(capture_seconds=time.monotonic() - started,
                        immutable_files=linked, reused_wals=reused,
                        stop_time_copied_bytes=copied)
        durable_json(backup / "CLOSED_CHECKPOINT.json", manifest)
        emit("closed_checkpoint_complete", **{k:v for k,v in manifest.items() if k != "files"})
    finally:
        os.close(fd)


def verify(backup):
    manifest = json.loads((backup / "CLOSED_CHECKPOINT.json").read_text())
    db = backup / "db"
    if identity(db) != manifest["identity"]:
        raise RuntimeError("checkpoint identity mismatch")
    actual = {p.name for p in files(db)}
    if actual != set(manifest["files"]):
        raise RuntimeError("checkpoint file catalog changed")
    for name, info in manifest["files"].items():
        path = db / name
        if path.stat().st_size != info["bytes"]:
            raise RuntimeError("checkpoint file size changed: " + name)
        if "sha256" in info:
            with path.open("rb") as stream:
                if hashlib.file_digest(stream, "sha256").hexdigest() != info["sha256"]:
                    raise RuntimeError("checkpoint copied-file checksum mismatch: " + name)
    emit("checkpoint_files_verified", identity=manifest["identity"], files=len(actual))


def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("action", choices=("precopy", "capture", "verify"))
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--expected-identity", required=True)
    args = parser.parse_args()
    cfg = tomllib.loads(args.config.read_text())
    db = Path(cfg["kline"]["rocksdb_path"]).resolve(strict=True)
    backup = args.backup.absolute()
    parent = backup.parent.resolve(strict=True)
    backup = parent / backup.name
    if backup.is_symlink() or backup.is_relative_to(db) or db.is_relative_to(backup):
        raise RuntimeError("checkpoint/source overlap")
    for source in cfg.get("sources", []):
        live = Path(source["rocksdb_path"]).resolve()
        if db.is_relative_to(live) or live.is_relative_to(db) or backup.is_relative_to(live) or live.is_relative_to(backup):
            raise RuntimeError("refuse Exec store")
    if identity(db) != args.expected_identity:
        raise RuntimeError("unexpected Manager DB identity")
    if args.action == "precopy":
        precopy(db, backup)
    elif args.action == "capture":
        state = subprocess.check_output(["systemctl", "--user", "show",
            "crypto-cta-manager-web.service", "--property=MainPID", "--value"], text=True).strip()
        if state != "0":
            raise RuntimeError("stop only Manager before capture")
        capture(db, backup, args.expected_identity)
    else:
        verify(backup)


if __name__ == "__main__":
    main()
