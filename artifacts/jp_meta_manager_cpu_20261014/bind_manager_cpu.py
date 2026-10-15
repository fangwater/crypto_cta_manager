"""Bind only the running Manager to idle CPU 24 and persist without restarting."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import tomllib

ROOT = Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT / 'deploy-staging/manager-cpu24-20261014'
UNIT = 'crypto-cta-manager-web.service'
UNIT_PATH = Path.home() / '.config/systemd/user' / UNIT
CPU = 24


def manager_pid():
    return int(subprocess.check_output([
        'systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'
    ], text=True))


def identity(path):
    return (path / 'stat').read_text().rsplit(')', 1)[1].split()[19]


def capture():
    protected = {}
    pid = manager_pid()
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) in {pid, os.getpid()}:
            continue
        try:
            comm = (proc / 'comm').read_text().strip()
            cmd = (proc / 'cmdline').read_bytes()
            relevant = any(s in comm.lower() for s in [
                'trade', 'exec', 'viz', 'nginx', 'cta_monitor', 'signal',
                'factor', 'spread', 'account_mon', 'persist_manager'
            ]) or (comm.startswith('python') and any(s in cmd for s in [
                b'exec_config_server', b'viz_server'
            ]))
            if not relevant:
                continue
            threads = {}
            for thread in (proc / 'task').iterdir():
                try:
                    threads[thread.name] = {
                        'start': identity(thread),
                        'affinity': sorted(os.sched_getaffinity(int(thread.name)))
                    }
                except (FileNotFoundError, ProcessLookupError):
                    pass
            protected[proc.name] = {'comm': comm, 'start': identity(proc), 'threads': threads}
        except (OSError, PermissionError):
            continue
    cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
    paths = [ROOT / 'config/cta-manager.toml',
             Path('/etc/nginx/snippets/crypto_cta_manager.conf'),
             Path('/etc/nginx/sites-available/crypto_proxy_4191.conf'),
             Path('/home/ubuntu/nginx_locations.txt')]
    paths += [Path(p) for p in cfg['kline']['trade_engine_configs']]
    for source in cfg['sources']:
        env_path = Path(source.get('env_path', str(Path(source['rocksdb_path']).parent.parent / 'env.sh')))
        paths.append(env_path)
    return {
        'captured_at_us': time.time_ns() // 1000,
        'manager_pid': pid,
        'manager_start': identity(Path('/proc') / str(pid)),
        'manager_threads': {p.name: sorted(os.sched_getaffinity(int(p.name)))
                            for p in Path(f'/proc/{pid}/task').iterdir()},
        'manager_sha256': hashlib.sha256((ROOT / 'bin/cta_web').read_bytes()).hexdigest(),
        'unit_sha256': hashlib.sha256(UNIT_PATH.read_bytes()).hexdigest(),
        'protected': protected,
        'config_sha256': {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                          for p in set(paths) if p.is_file()},
        'webroot': str((ROOT / 'webroot').resolve()),
        'frontend_sha256': hashlib.sha256((ROOT / 'webroot/manager/index.html').read_bytes()).hexdigest()
    }


def verify_protected(before, after):
    assert before['config_sha256'] == after['config_sha256']
    for name in ['manager_pid', 'manager_start', 'manager_sha256', 'webroot', 'frontend_sha256']:
        assert before[name] == after[name], name
    for pid, old in before['protected'].items():
        new = after['protected'].get(pid)
        assert new and new['start'] == old['start'], ('protected process changed', pid)
        for tid, thread in old['threads'].items():
            if tid in new['threads'] and new['threads'][tid]['start'] == thread['start']:
                assert new['threads'][tid]['affinity'] == thread['affinity'], ('protected affinity changed', tid)


def main():
    assert CPU in os.sched_getaffinity(os.getpid()) or Path(f'/sys/devices/system/cpu/cpu{CPU}/online').read_text().strip() == '1'
    # Ensure no application has separately allocated this core.
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit():
            continue
        try:
            if not (proc / 'cmdline').read_bytes():
                continue
            for thread in (proc / 'task').iterdir():
                affinity = os.sched_getaffinity(int(thread.name))
                assert CPU not in affinity, ('CPU already allocated', proc.name, thread.name)
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            pass
    STAGE.mkdir(parents=True, exist_ok=False)
    before = capture()
    (STAGE / 'before.json').write_text(json.dumps(before, indent=2))
    original = UNIT_PATH.read_text()
    (STAGE / 'manager.service.previous').write_text(original)
    lines = [line for line in original.splitlines() if not line.startswith('CPUAffinity=')]
    assert lines.count('[Service]') == 1
    lines.insert(lines.index('[Service]') + 1, 'CPUAffinity=24')
    candidate = STAGE / UNIT
    candidate.write_text('\n'.join(lines) + '\n')
    result = subprocess.run(['systemd-analyze', '--user', 'verify', str(candidate)], capture_output=True, text=True)
    assert result.returncode == 0, 'systemd unit verification failed'
    pid = before['manager_pid']
    previous_affinity = before['manager_threads']
    try:
        for _ in range(5):
            assert manager_pid() == pid
            for thread in Path(f'/proc/{pid}/task').iterdir():
                try:
                    os.sched_setaffinity(int(thread.name), {CPU})
                except ProcessLookupError:
                    pass
            if all(os.sched_getaffinity(int(t.name)) == {CPU}
                   for t in Path(f'/proc/{pid}/task').iterdir()):
                break
        else:
            raise RuntimeError('Manager thread affinity did not stabilize')
        assert UNIT_PATH.read_text() == original, 'concurrent unit edit'
        next_path = UNIT_PATH.with_name(UNIT + '.next.cpu24')
        next_path.write_text(candidate.read_text())
        next_path.chmod(UNIT_PATH.stat().st_mode)
        os.replace(next_path, UNIT_PATH)
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
        effective = subprocess.check_output([
            'systemctl', '--user', 'show', UNIT, '-p', 'CPUAffinity', '--value'
        ], text=True).strip()
        assert effective == '24', effective
        after = capture()
        verify_protected(before, after)
        assert all(value == [CPU] for value in after['manager_threads'].values())
        (STAGE / 'after.json').write_text(json.dumps(after, indent=2))
    except Exception:
        UNIT_PATH.write_text(original)
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
        for thread in Path(f'/proc/{pid}/task').iterdir():
            try:
                os.sched_setaffinity(int(thread.name), set(previous_affinity.get(thread.name, list(range(6)))))
            except ProcessLookupError:
                pass
        raise
    print(json.dumps({'manager_pid': pid, 'cpu': CPU, 'threads': len(after['manager_threads']),
                      'protected_processes': len(before['protected']), 'restarted': False,
                      'persisted_cpu_affinity': effective, 'stage': str(STAGE)}), flush=True)


if __name__ == '__main__':
    main()
