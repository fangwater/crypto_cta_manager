#!/usr/bin/env python3
"""Manual, date-bounded Manager signal backup and reclamation.

Default: read-only plan. --backup seals and verifies the complete chosen range
without deleting it. --reclaim requires that exact backup, then asks Manager to
prune once and compact once. Only Manager opens the live database for writing.
Dates are exclusive midnight boundaries in UTC+08:00. No scheduler is installed.
"""
import argparse
import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time
import tomllib


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_json(path, value):
    fd, temporary = tempfile.mkstemp(prefix=".receipt-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as out:
            json.dump(value, out, sort_keys=True)
            out.write("\n")
            out.flush()
            os.fsync(out.fileno())
        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def date_boundary(value):
    parsed = datetime.date.fromisoformat(value)
    if parsed.isoformat() != value:
        raise ValueError("before must be YYYY-MM-DD")
    local = datetime.datetime.combine(parsed, datetime.time(),
                                     datetime.timezone(datetime.timedelta(hours=8)))
    delta = local.astimezone(datetime.timezone.utc) - datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)
    us = (delta.days * 86400 + delta.seconds) * 1000000
    if us <= 1:
        raise ValueError("before must be after the Unix epoch")
    return us


def remote(host, action, before=None, data=None, output=subprocess.PIPE):
    argv = [host["binary"], action]
    if action == "inspect":
        argv += ["--db", host["db"]]
    else:
        argv += ["--socket", host["socket"]]
    if action in ("inspect", "export", "compact"):
        argv += ["--before", before]
    if action == "commit":
        argv += ["--receipt", "-"]
    transport = host.get("transport", "ssh")
    if transport == "local":
        command = argv
    elif transport == "ssh":
        command = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", host["ssh"], shlex.join(argv)]
    else:
        raise ValueError("transport must be ssh or local")
    return subprocess.run(
        command,
        input=None if data is None else json.dumps(data).encode(),
        stdout=output, check=False, timeout=7200,
    )


def checked_remote(host, action, before=None, data=None):
    result = remote(host, action, before, data)
    if result.returncode:
        raise RuntimeError(f"{host['name']}: remote {action} failed ({result.returncode})")
    response = json.loads(result.stdout)
    if action == "inspect":
        return response
    if not response.get("ok"):
        raise RuntimeError(f"{host['name']}: maintenance rejected: {response.get('error', 'unknown error')}")
    return response["status"]


def verify(verifier, chunk, restored):
    result = subprocess.run([str(verifier), "verify", "--file", str(chunk), "--restore", str(restored)],
                            check=True, capture_output=True, timeout=7200)
    return json.loads(result.stdout)


def backup_path(directory, before):
    return directory / f"before-{before}.json"


def verify_saved(directory, before, verifier):
    path = backup_path(directory, before)
    if not path.is_file():
        raise RuntimeError("no verified backup for this date; run --backup first")
    saved = json.loads(path.read_text())
    receipt = saved["receipt"]
    if saved["before"] != before or receipt["to"] != [date_boundary(before), 0]:
        raise RuntimeError("backup date/range mismatch")
    digest = receipt["sha256"]
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise RuntimeError("invalid backup digest")
    chunk = directory / "chunks" / f"{digest}.signals.gz"
    if not chunk.is_file() or verify(verifier, chunk, directory / "restored") != receipt:
        raise RuntimeError("saved backup failed restore/readback verification")
    identity = json.loads((directory / "identity.json").read_text())["identity"]
    if identity != receipt["identity"]:
        raise RuntimeError("backup identity mismatch")
    return receipt


def prepare_directory(host, root, status):
    directory = root / host["name"]
    directory.mkdir(mode=0o700, exist_ok=True)
    sync_dir(root)
    chunks = directory / "chunks"
    chunks.mkdir(mode=0o700, exist_ok=True)
    sync_dir(directory)
    identity = directory / "identity.json"
    if identity.exists():
        if json.loads(identity.read_text())["identity"] != status["identity"]:
            raise RuntimeError("Manager database identity changed; refusing maintenance")
    else:
        if status["floor"] != [1, 0]:
            raise RuntimeError("source already pruned; restore the existing local identity/receipt chain first")
        durable_json(identity, {"identity": status["identity"], "ssh": host.get("ssh", "local")})
    return directory


def backup(host, root, before, verifier):
    status = checked_remote(host, "status")
    requested = [date_boundary(before), 0]
    if requested > status["eligible_before"]:
        raise RuntimeError("date would remove retained monthly signals; choose an earlier date")
    directory = prepare_directory(host, root, status)
    if backup_path(directory, before).exists():
        receipt = verify_saved(directory, before, verifier)
        if receipt["identity"] != status["identity"]:
            raise RuntimeError("saved backup belongs to a different live database")
        print(f"{host['name']}: existing backup verified; {receipt['count']} records, no live deletion", flush=True)
        return receipt
    if status["floor"] >= requested:
        raise RuntimeError("date range already reclaimed; use the existing backup chain")
    chunks = directory / "chunks"
    fd, temporary = tempfile.mkstemp(prefix=".receiving-", dir=chunks)
    try:
        print(f"{host['name']}: backing up complete prefix before {before} 00:00 +08:00", flush=True)
        with os.fdopen(fd, "wb") as out:
            result = remote(host, "export", before, output=out)
            out.flush()
            os.fsync(out.fileno())
        if result.returncode:
            raise RuntimeError(f"export failed ({result.returncode}); live originals retained")
        receipt = verify(verifier, temporary, directory / "restored")
        previous = status["last_receipt"]["sha256"] if status["last_receipt"] else None
        if (receipt["identity"] != status["identity"] or receipt["from"] != status["floor"]
                or receipt["to"] != requested or receipt["previous"] != previous):
            raise RuntimeError("export identity, date or receipt chain mismatch")
        digest = receipt["sha256"]
        if not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise RuntimeError("invalid export digest")
        final = chunks / f"{digest}.signals.gz"
        if final.exists():
            if verify(verifier, final, directory / "restored") != receipt:
                raise RuntimeError("existing archive conflicts")
        else:
            os.rename(temporary, final)
        sync_dir(chunks)
        durable_json(chunks / f"{digest}.json", receipt)
        durable_json(backup_path(directory, before), {
            "before": before, "verified_at_us": time.time_ns() // 1000, "receipt": receipt,
        })
        print(f"{host['name']}: backed up and restored {receipt['count']} records / {receipt['raw_bytes']} raw bytes", flush=True)
        print(f"{host['name']}: backup={final}; live records retained; --reclaim is a separate step", flush=True)
        return receipt
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def reclaim(host, root, before, verifier):
    directory = root / host["name"]
    # Verify the complete durable backup AGAIN before asking the live owner to delete.
    receipt = verify_saved(directory, before, verifier)
    status = checked_remote(host, "status")
    if status["identity"] != receipt["identity"]:
        raise RuntimeError("live database identity changed")
    if status["floor"] == receipt["from"]:
        previous = status["last_receipt"]["sha256"] if status["last_receipt"] else None
        if previous != receipt["previous"]:
            raise RuntimeError("live receipt chain changed")
        print(f"{host['name']}: asking Manager to atomically reclaim verified range", flush=True)
        status = checked_remote(host, "commit", data=receipt)
        if status["floor"] != receipt["to"] or status["last_receipt"] != receipt:
            raise RuntimeError("Manager did not confirm the verified range")
    elif status["floor"] < receipt["to"]:
        raise RuntimeError("live prefix changed; manual reconciliation required")
    # Always compact, including a retry after a lost commit reply or failed compact.
    # Manager only accepts a boundary inside its already verified reclaimed prefix.
    print(f"{host['name']}: asking Manager to compact position_updates before {before}", flush=True)
    checked_remote(host, "compact", before)
    durable_json(directory / f"before-{before}.reclaimed.json", {
        "before": before, "completed_at_us": time.time_ns() // 1000, "receipt": receipt,
    })
    print(f"{host['name']}: verified reclamation and compaction completed", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--host", required=True, help="Exactly one host label from the TOML")
    parser.add_argument("--before", required=True, help="Exclusive YYYY-MM-DD, 00:00 UTC+08:00")
    phase = parser.add_mutually_exclusive_group()
    phase.add_argument("--backup", action="store_true", help="Backup/verify only; no live deletion")
    phase.add_argument("--reclaim", action="store_true", help="Require saved backup, then prune and compact through Manager")
    args = parser.parse_args()
    date_boundary(args.before)
    settings = tomllib.loads(args.config.read_text())
    hosts = [host for host in settings["hosts"] if host["name"] == args.host]
    if len(hosts) != 1 or not re.fullmatch(r"[A-Za-z0-9_-]+", args.host):
        parser.error("--host must select exactly one valid configured host")
    host = hosts[0]
    for key in ("db", "socket", "binary"):
        if not Path(host[key]).is_absolute():
            parser.error(f"host {key} must be absolute")
    root = Path(settings["backup_root"])
    if not root.is_absolute():
        parser.error("backup_root must be absolute")
    if not args.backup and not args.reclaim:
        plan = checked_remote(host, "inspect", args.before)
        print(json.dumps({"host": args.host, "before": args.before, "timezone": "UTC+08:00",
                          "read_only": True, "plan": plan}, ensure_ascii=False, indent=2))
        return
    os.umask(0o077)
    root.mkdir(parents=True, mode=0o700, exist_ok=True)
    sync_dir(root.parent)
    with (root / ".pull.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        verifier = Path(settings["verifier"])
        if args.backup:
            backup(host, root, args.before, verifier)
        else:
            reclaim(host, root, args.before, verifier)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"position archive: {error}", file=sys.stderr, flush=True)
        sys.exit(1)
