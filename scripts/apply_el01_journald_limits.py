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
import signal
import stat
import subprocess
import tarfile
import tempfile
import time


MACHINE_ID = "0d5f31f57aa742f3a86c2a7382d6d0a8"
ROOT = Path("/home/el01/crypto_cta_manager")
JOURNALS = Path("/var/log/journal") / MACHINE_ID
CONFIG = Path("/etc/systemd/journald.conf.d/90-crypto-cta-log-limits.conf")
BACKUP_ROOT = Path("/var/backups/crypto-cta-manager-journald")
LIMITS = b"[Journal]\nSystemMaxUse=1G\nSystemKeepFree=10G\nMaxRetentionSec=7day\nSystemMaxFileSize=64M\n"
PROTECTED_NAMES = (
    "cta_web", "cta_monitor", "trade_engine", "exec-pre-trade", "persist_manager",
    "viz_server", "config_server", "account_monitor", "redis-server", "postgres", "nginx",
)
MULTIPROCESS_SERVICES = {"postgres", "nginx", "redis-server"}


def progress(stage, **fields):
    print(json.dumps({"stage": stage, **fields}), flush=True)


def run(*args, data=None, timeout=300):
    # Capture diagnostics: journal text can contain private application data.
    # Report long-running work without exposing captured command output.
    started = time.monotonic()
    with subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, start_new_session=True) as process:
        pending = data
        try:
            while True:
                try:
                    output, _ = process.communicate(input=pending, timeout=min(10, timeout))
                    break
                except subprocess.TimeoutExpired:
                    pending = None
                    elapsed = time.monotonic() - started
                    progress("command_running", command=args[0], elapsed_secs=round(elapsed))
                    if elapsed >= timeout:
                        raise RuntimeError(f"{args[0]} exceeded {timeout}s; no further steps run")
        except BaseException:
            # Stop tar and its compression child together on timeout/Ctrl+C.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.communicate()
            raise
        if process.returncode:
            raise RuntimeError(f"{args[0]} failed (exit {process.returncode}); diagnostics suppressed")
        return output.decode()


def digest(stream):
    value = hashlib.sha256()
    count, last_report = 0, time.monotonic()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        value.update(chunk)
        count += len(chunk)
        if time.monotonic() - last_report >= 10:
            progress("hashing", processed_bytes=count)
            last_report = time.monotonic()
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


def split_processes(rows):
    """Protect service roots; record replaceable service children separately.

    PostgreSQL starts/exits connection and worker processes during normal use.
    Nginx/Redis likewise manage child workers. Their master identities remain
    strict, as do every matching Manager/Exec process and the Manager data.
    """
    by_pid = {row[0]: row for row in rows}
    stable, auxiliary = [], []
    for pid, executable, started, parent in rows:
        parent_row = by_pid.get(parent)
        name = Path(executable.removesuffix(" (deleted)")).name
        if name in MULTIPROCESS_SERVICES and parent_row and parent_row[1] == executable:
            auxiliary.append([pid, executable, started, parent])
        else:
            stable.append([pid, executable, started])
    return sorted(stable), sorted(auxiliary)


def protected():
    rows = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            executable = os.readlink(entry / "exe")
            if not any(name in Path(executable).name for name in PROTECTED_NAMES):
                continue
            fields = (entry / "stat").read_text().rsplit(") ", 1)[1].split()
            rows.append([int(entry.name), executable, fields[19], int(fields[1])])
        except (OSError, IndexError):
            continue
    with (ROOT / "config/cta-manager.toml").open("rb") as source:
        config_digest = digest(source)
    processes, auxiliary = split_processes(rows)
    return {
        "processes": processes,
        "auxiliary_processes": auxiliary,
        "manager_config_sha256": config_digest,
        "manager_identity": (ROOT / "db/IDENTITY").read_text().strip(),
    }


def protected_diff(before, after):
    removed = [row for row in before["processes"] if row not in after["processes"]]
    added = [row for row in after["processes"] if row not in before["processes"]]
    changed = [key for key in ("manager_config_sha256", "manager_identity")
               if before[key] != after[key]]
    old_aux, new_aux = before.get("auxiliary_processes", []), after.get("auxiliary_processes", [])
    return {"unchanged": not (removed or added or changed),
            "processes_removed": removed, "processes_added": added, "data_changed": changed,
            "auxiliary_removed": [row for row in old_aux if row not in new_aux],
            "auxiliary_added": [row for row in new_aux if row not in old_aux]}


def check_protected(before, backup, phase):
    after = protected()
    diff = protected_diff(before, after)
    save(backup / f"protected-{phase}.json", {"after": after, "diff": diff})
    progress("check_application_services", phase=phase,
             unchanged=diff["unchanged"], service_processes=len(before["processes"]),
             auxiliary_added=len(diff["auxiliary_added"]),
             auxiliary_removed=len(diff["auxiliary_removed"]))
    if not diff["unchanged"]:
        # These rows contain only PID/executable/start time, never command lines.
        progress("application_state_changed", phase=phase,
                 processes_removed=diff["processes_removed"],
                 processes_added=diff["processes_added"], data_changed=diff["data_changed"])
        raise RuntimeError(f"protected service or Manager data changed ({phase}); see saved diff")
    return after


def archived_files():
    result = []
    for path in JOURNALS.iterdir():
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError("unexpected non-regular entry in journal directory")
        if ("@" in path.name and path.name.endswith(".journal")) or path.name.endswith(".journal~"):
            result.append(path)
    return sorted(result)


def fingerprints(paths):
    result = {}
    for path in paths:
        with path.open("rb") as source:
            result[path.name] = digest(source)
    return result


def archive(paths, destination, attempts=3):
    if not paths:
        return None
    for attempt in range(1, attempts + 1):
        progress("archive_hash_sources", file_count=len(paths), attempt=attempt)
        expected = fingerprints(paths)
        partial = destination.with_name(destination.name + ".partial")
        if partial.exists() or destination.exists():
            raise RuntimeError("refusing to overwrite an existing journal backup")
        progress("archive_compress", file_count=len(paths), attempt=attempt)
        run("tar", "--create", "--use-compress-program=gzip -1", "--file", str(partial),
            "--directory", str(JOURNALS), "--null", "--files-from", "-",
            data=b"\0".join(os.fsencode(path.name) for path in paths) + b"\0")
        progress("archive_verify", file_count=len(paths), attempt=attempt)
        run("gzip", "--test", str(partial))
        actual = {}
        # Stream once instead of scanning the compressed tar and seeking back.
        with tarfile.open(partial, "r|gz") as saved:
            for member in saved:
                if not member.isfile() or member.name in actual or member.name not in expected:
                    raise RuntimeError("unexpected or duplicate journal archive member")
                with saved.extractfile(member) as source:
                    actual[member.name] = digest(source)
        if set(actual) != set(expected):
            raise RuntimeError("journal archive member mismatch")
        after = fingerprints(paths)
        if actual == expected == after:
            with partial.open("rb") as source:
                os.fsync(source.fileno())
                checksum = digest(source)
            os.replace(partial, destination)
            progress("archive_verified", file_count=len(paths), attempt=attempt)
            return {"path": str(destination), "sha256": checksum, "members": expected}
        if expected == after:
            raise RuntimeError("journal backup differs from stable source; no limits installed")
        # Rotation can complete before journald finishes updating file headers.
        # Retry only source changes; never accept a mismatched backup.
        progress("archive_source_changed", changed_files=sum(
            expected[name] != after[name] for name in expected), attempt=attempt)
        if attempt == attempts:
            raise RuntimeError("journals remained unstable after bounded retries; no limits installed")
        partial.unlink()
        time.sleep(1)


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


def create_backup_directory():
    BACKUP_ROOT.mkdir(mode=0o700, exist_ok=True)
    info = BACKUP_ROOT.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        raise RuntimeError("journal backup parent must be private and root-owned")
    backup = BACKUP_ROOT / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    backup.mkdir(mode=0o700)
    return backup


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
    progress("preflight")
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
    backup = create_backup_directory()
    progress("backup_started", backup=str(backup))
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
    progress("journal_sync_and_rotate")
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
    check_protected(before, backup, "before-install")
    CONFIG.parent.mkdir(mode=0o755, exist_ok=True)
    try:
        progress("install_limits")
        replace_config(LIMITS)
        effective = run("systemd-analyze", "cat-config", "systemd/journald.conf")
        verify_limits(effective)
        save(backup / "effective-config.json", {"text": effective})
        # A single restart preserves systemd's logging stream descriptors.
        progress("restart_journald")
        run("systemctl", "restart", "systemd-journald.service")
        run("systemctl", "is-active", "--quiet", "systemd-journald.service")
        progress("vacuum_archived_journals")
        run("journalctl", "--directory", str(JOURNALS), "--vacuum-size=1G", "--vacuum-time=7days")
        run("journalctl", "--sync")
        after = check_protected(before, backup, "after-install")
        result = {"applied": True, "backup": str(backup), "disk_usage_before": usage_before,
            "disk_usage_after": run("journalctl", "--directory", str(JOURNALS), "--disk-usage").strip(),
            "protected_processes": len(before["processes"]), "protected_state_unchanged": True,
            "auxiliary_processes_observed": len(after["auxiliary_processes"])}
        save(backup / "result.json", result)
        print(json.dumps(result), flush=True)
    except BaseException:
        progress("restore_previous_limits")
        if old is None:
            CONFIG.unlink(missing_ok=True)
        else:
            replace_config(old, old_mode)
        run("systemctl", "restart", "systemd-journald.service")
        raise


if __name__ == "__main__":
    main()
