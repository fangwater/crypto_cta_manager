#!/usr/bin/env python3
"""Pull sealed Manager signals to powerleader; prune only after durable restore proof.

Default is backup-only. --apply explicitly commits verified receipts. Each host
must be explicitly enabled in the host-only TOML. No credentials are needed.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
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


def remote(host, action, data=None, output=subprocess.PIPE):
    argv = [host["binary"], action, "--socket", host["socket"]]
    if action == "commit":
        argv += ["--receipt", "-"]
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
         host["ssh"], shlex.join(argv)],
        input=None if data is None else json.dumps(data).encode(),
        stdout=output, check=False, timeout=7200,
    )


def checked_remote(host, action, data=None):
    result = remote(host, action, data)
    if result.returncode:
        raise RuntimeError(f"{host['name']}: remote {action} failed ({result.returncode})")
    response = json.loads(result.stdout)
    if not response.get("ok"):
        raise RuntimeError(f"{host['name']}: maintenance rejected")
    return response.get("status")


def apply_pending(host, directory, status, verifier):
    pending = directory / "pending.json"
    if not pending.exists():
        return status
    receipt = json.loads(pending.read_text())
    chunk = directory / "chunks" / (receipt["sha256"] + ".signals.gz")
    if not chunk.is_file():
        raise RuntimeError("pending receipt has no durable compressed backup")
    if status["last_receipt"] != receipt:
        proof = subprocess.run([str(verifier), "verify", "--file", str(chunk),
                                "--restore", str(directory / "restored")],
                               check=True, capture_output=True, timeout=7200)
        if json.loads(proof.stdout) != receipt:
            raise RuntimeError("pending backup failed restore verification")
        if status["identity"] != receipt["identity"] or status["floor"] != receipt["from"]:
            raise RuntimeError("remote prefix changed; manual archive reconciliation required")
        status = checked_remote(host, "commit", receipt)
    if status["last_receipt"] != receipt or status["floor"] != receipt["to"]:
        raise RuntimeError("remote commit did not confirm the verified receipt")
    os.unlink(pending)
    sync_dir(directory)
    print(f"{host['name']}: committed {receipt['count']} records / {receipt['raw_bytes']} raw bytes", flush=True)
    return status


def pull_host(host, root, verifier, apply, max_chunks, compact):
    if not re.fullmatch(r"[A-Za-z0-9_-]+", host["name"]):
        raise ValueError("invalid host label")
    directory = root / host["name"]
    directory.mkdir(mode=0o700, exist_ok=True)
    sync_dir(root)
    chunks = directory / "chunks"
    chunks.mkdir(mode=0o700, exist_ok=True)
    sync_dir(directory)
    status = checked_remote(host, "status")
    identity_file = directory / "identity.json"
    if identity_file.exists():
        if json.loads(identity_file.read_text())["identity"] != status["identity"]:
            raise RuntimeError("Manager database identity changed; refusing automatic pruning")
    else:
        if status["floor"] != [1, 0]:
            raise RuntimeError("source was already pruned; restore the existing backup/identity chain first")
        durable_json(identity_file, {"identity": status["identity"], "ssh": host["ssh"]})
    print(f"{host['name']}: identity={status['identity']} floor={status['floor']} eligible_before={status['eligible_before']}", flush=True)
    pending = directory / "pending.json"
    if pending.exists():
        if not apply:
            print(f"{host['name']}: verified backup pending; --apply required to advance", flush=True)
            return
        status = apply_pending(host, directory, status, verifier)
    changed = False
    for _ in range(max_chunks):
        fd, temporary = tempfile.mkstemp(prefix=".receiving-", dir=chunks)
        try:
            with os.fdopen(fd, "wb") as out:
                result = remote(host, "export", output=out)
                out.flush()
                os.fsync(out.fileno())
            if result.returncode == 3:
                break
            if result.returncode:
                raise RuntimeError(f"remote export failed ({result.returncode}); originals retained")
            proof = subprocess.run([str(verifier), "verify", "--file", temporary,
                                    "--restore", str(directory / "restored")],
                                   check=True, capture_output=True, timeout=7200)
            receipt = json.loads(proof.stdout)
            if receipt["identity"] != status["identity"] or receipt["from"] != status["floor"]:
                raise RuntimeError("export identity/prefix mismatch")
            final = chunks / (receipt["sha256"] + ".signals.gz")
            if final.exists():
                # Existing content must also pass restore verification.
                existing = subprocess.run([str(verifier), "verify", "--file", str(final),
                                           "--restore", str(directory / "restored")],
                                          check=True, capture_output=True, timeout=7200)
                if json.loads(existing.stdout) != receipt:
                    raise RuntimeError("existing archive chunk conflicts")
                os.unlink(temporary)
            else:
                os.rename(temporary, final)
            sync_dir(chunks)
            durable_json(chunks / (receipt["sha256"] + ".json"), receipt)
            durable_json(pending, receipt)
            print(f"{host['name']}: restored and verified {receipt['count']} records -> {final}", flush=True)
            if not apply:
                print(f"{host['name']}: backup-only; no live records removed", flush=True)
                break
            status = apply_pending(host, directory, status, verifier)
            changed = True
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    if compact and apply and changed:
        checked_remote(host, "compact")
        print(f"{host['name']}: Manager position_updates compaction finished", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--compact", action="store_true")
    parser.add_argument("--max-chunks", type=int, default=16)
    args = parser.parse_args()
    if args.max_chunks < 1:
        parser.error("--max-chunks must be positive")
    settings = tomllib.loads(args.config.read_text())
    root = Path(settings["backup_root"])
    if not root.is_absolute():
        parser.error("backup_root must be absolute")
    os.umask(0o077)
    root.mkdir(parents=True, mode=0o700, exist_ok=True)
    sync_dir(root.parent)
    with (root / ".pull.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        failed = False
        for host in settings["hosts"]:
            if not host.get("enabled", False):
                continue
            try:
                pull_host(host, root, Path(settings["verifier"]), args.apply,
                          args.max_chunks, args.compact)
            except Exception as error:
                # Keep the other host independent. Never print credentials/config.
                print(f"{host['name']}: {error}", file=sys.stderr, flush=True)
                failed = True
        if failed:
            sys.exit(1)


if __name__ == "__main__":
    main()
