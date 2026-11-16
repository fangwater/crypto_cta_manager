#!/usr/bin/env python3
"""Apply the approved el01 journal limits after archiving closed journals.

Preview is the default. Run --execute through an authenticated sudo session.
Only systemd-journald is restarted; application services are never restarted.
"""

import argparse
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tarfile
import tempfile


MACHINE_ID = "0d5f31f57aa742f3a86c2a7382d6d0a8"
ROOT = Path("/home/el01/crypto_cta_manager")
JOURNALS = Path("/var/log/journal") / MACHINE_ID
CONFIG = Path("/etc/systemd/journald.conf.d/90-crypto-cta-log-limits.conf")
LIMITS = b"[Journal]\nSystemMaxUse=1G\nSystemKeepFree=10G\nMaxRetentionSec=7day\nSystemMaxFileSize=64M\n"
PROTECTED_NAMES = (
    "cta_web", "cta_monitor", "trade_engine", "exec-pre-trade", "persist_manager",
    "viz_server", "config_server", "account_monitor", "redis-server", "postgres", "nginx",
)


def run(*args, data=None):
    # Capture diagnostics: journal text can contain private application data.
    result = subprocess.run(args, input=data, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed (exit {result.returncode}); diagnostics suppressed")
    return result.stdout.decode()


def digest(stream):
    value = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        value.update(chunk)
    return value.hexdigest()


def save(path, value):
    with path.open("x") as output:
        json.dump(value, output, indent=2)
        output.flush()
        os.fsync(output.fileno())
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def verify_limits(effective):
    section, values = None, {}
    for line in effective.splitlines():
        line = line.strip()
        if not line or line.startswith(("#", ";")):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line
        elif section == "[Journal]" and "=" in line:
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip()
    expected = {"SystemMaxUse": "1G", "SystemKeepFree": "10G",
                "MaxRetentionSec": "7day", "SystemMaxFileSize": "64M"}
    if any(values.get(key) != value for key, value in expected.items()):
        raise RuntimeError("another journald setting overrides the approved limits")


def protected():
    processes = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            executable = os.readlink(entry / "exe")
            if not any(name in Path(executable).name for name in PROTECTED_NAMES):
                continue
            fields = (entry / "stat").read_text().rsplit(") ", 1)[1].split()
            processes.append([int(entry.name), executable, fields[19]])
        except (OSError, IndexError):
            continue
    with (ROOT / "config/cta-manager.toml").open("rb") as source:
        config_digest = digest(source)
    return {
        "processes": sorted(processes),
        "manager_config_sha256": config_digest,
        "manager_identity": (ROOT / "db/IDENTITY").read_text().strip(),
    }


def archived_files():
    result = []
    for path in JOURNALS.iterdir():
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError("unexpected non-regular entry in journal directory")
        if ("@" in path.name and path.name.endswith(".journal")) or path.name.endswith(".journal~"):
            result.append(path)
    return sorted(result)


def archive(paths, destination):
    if not paths:
        return None
    expected = {}
    for path in paths:
        with path.open("rb") as source:
            expected[path.name] = digest(source)
    run("tar", "--create", "--use-compress-program=gzip -1", "--file", str(destination),
        "--directory", str(JOURNALS), "--null", "--files-from", "-",
        data=b"\0".join(os.fsencode(path.name) for path in paths) + b"\0")
    run("gzip", "--test", str(destination))
    with tarfile.open(destination, "r:gz") as saved:
        members = saved.getmembers()
        if len(members) != len(expected) or {m.name for m in members} != set(expected):
            raise RuntimeError("journal archive member mismatch")
        for member in members:
            if not member.isfile():
                raise RuntimeError("unexpected archive member type")
            with saved.extractfile(member) as source:
                if digest(source) != expected[member.name]:
                    raise RuntimeError("journal archive content mismatch")
    with destination.open("rb") as source:
        os.fsync(source.fileno())
        checksum = digest(source)
    return {"path": str(destination), "sha256": checksum, "members": expected}


def replace_config(payload, mode=0o644):
    with tempfile.NamedTemporaryFile(dir=CONFIG.parent, delete=False) as output:
        temporary = Path(output.name)
        output.write(payload)
        output.flush()
        os.fsync(output.fileno())
    try:
        temporary.chmod(mode)
        os.replace(temporary, CONFIG)
        directory = os.open(CONFIG.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true")
    args = parser.parse_args()
    if Path("/etc/machine-id").read_text().strip() != MACHINE_ID:
        parser.error("this maintenance is approved only for el01")
    if JOURNALS.is_symlink() or not JOURNALS.is_dir():
        raise RuntimeError("unexpected journal directory")
    if not args.execute:
        print(json.dumps({"execute": False, "requires_root": True,
            "destination": str(CONFIG), "config": LIMITS.decode(),
            "journal_directory": str(JOURNALS), "restart": "systemd-journald.service"}))
        return
    if os.geteuid() != 0:
        parser.error("--execute requires an authenticated sudo session")
    os.umask(0o077)
    os.nice(19)
    run("ionice", "-c", "3", "-p", str(os.getpid()))
    lock = os.open("/run/lock/crypto-cta-manager-journald.lock",
                   os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        apply()
    finally:
        os.close(lock)


def apply():
    before = protected()
    old = None
    old_mode = 0o644
    if CONFIG.exists() or CONFIG.is_symlink():
        info = CONFIG.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_nlink != 1:
            raise RuntimeError("unexpected existing journald drop-in")
        old, old_mode = CONFIG.read_bytes(), stat.S_IMODE(info.st_mode)
    usage_before = run("journalctl", "--directory", str(JOURNALS), "--disk-usage").strip()
    fs = os.statvfs(JOURNALS)
    available = fs.f_bavail * fs.f_frsize
    maximum_backup = sum(p.stat().st_size for p in JOURNALS.iterdir())
    if available < 10 * 1024**3 + 2 * maximum_backup:
        raise RuntimeError("insufficient journal backup headroom above 10 GiB reserve")
    base = Path("/var/backups/crypto-cta-manager-journald")
    base.mkdir(mode=0o700, exist_ok=True)
    info = base.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        raise RuntimeError("journal backup parent must be private and root-owned")
    backup = base / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    backup.mkdir(mode=0o700)
    save(backup / "before.json", {"protected": before, "drop_in_existed": old is not None,
         "drop_in_mode": old_mode, "disk_usage": usage_before, "available_bytes": available})
    if old is not None:
        with (backup / CONFIG.name).open("xb") as output:
            output.write(old)
            output.flush()
            os.fsync(output.fileno())
    # First preserve already-closed files: rotation can itself trigger retention.
    paths = archived_files()
    backups = [archive(paths, backup / "closed-before.tar.gz")]
    backed = {p.name for p in paths}
    run("journalctl", "--sync")
    run("journalctl", "--rotate")
    for attempt in range(3):
        missing = [p for p in archived_files() if p.name not in backed]
        if not missing:
            break
        backups.append(archive(missing, backup / f"closed-after-{attempt}.tar.gz"))
        backed.update(p.name for p in missing)
    if any(p.name not in backed for p in archived_files()):
        raise RuntimeError("journals rotate too quickly; limits were not installed")
    save(backup / "archives-verified.json", [b for b in backups if b])
    if protected() != before:
        raise RuntimeError("protected application state changed before installation")
    CONFIG.parent.mkdir(mode=0o755, exist_ok=True)
    try:
        replace_config(LIMITS)
        effective = run("systemd-analyze", "cat-config", "systemd/journald.conf")
        verify_limits(effective)
        save(backup / "effective-config.json", {"text": effective})
        # A single restart preserves systemd's logging stream descriptors.
        run("systemctl", "restart", "systemd-journald.service")
        run("systemctl", "is-active", "--quiet", "systemd-journald.service")
        run("journalctl", "--directory", str(JOURNALS), "--vacuum-size=1G", "--vacuum-time=7days")
        run("journalctl", "--sync")
        after = protected()
        result = {"applied": True, "backup": str(backup), "disk_usage_before": usage_before,
            "disk_usage_after": run("journalctl", "--directory", str(JOURNALS), "--disk-usage").strip(),
            "protected_processes": len(before["processes"]), "protected_state_unchanged": before == after}
        save(backup / "result.json", result)
        print(json.dumps(result), flush=True)
        if before != after:
            raise RuntimeError("protected application state changed; inspect saved result")
    except Exception:
        if old is None:
            CONFIG.unlink(missing_ok=True)
        else:
            replace_config(old, old_mode)
        run("systemctl", "restart", "systemd-journald.service")
        raise


if __name__ == "__main__":
    main()
