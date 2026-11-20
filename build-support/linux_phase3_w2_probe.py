#!/usr/bin/env python3
"""Phase 3 W2 gate: real backend lifecycle, UI attach, duplicate instance, TERM.

Run through build-support/linux-native-test.py so DISPLAY, HOME, XAUTHORITY and
OINK_HOME belong to a disposable Xephyr session. Evidence (logs, screenshots,
window identities, JSON summary) is written to --out.
"""
import argparse
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path


def run(command, timeout=10, env=None):
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout, env=env)


def visible_windows(name):
    try:
        return run(['xdotool', 'search', '--onlyvisible', '--name', name]).stdout.split()
    except subprocess.TimeoutExpired:
        return []


def proc_alive(process):
    return process.poll() is None


def wait_exit(process, timeout):
    try:
        return process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        return None


def wait_window(name, process, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not proc_alive(process):
            return []
        windows = visible_windows(name)
        if windows:
            return windows
        time.sleep(0.5)
    return []


def call_status(cli, out, tag):
    """Poll the authenticated IPC read-only method until the backend answers."""
    deadline = time.monotonic() + 45
    last = ''
    while time.monotonic() < deadline:
        try:
            result = run([cli, 'call', 'status'], timeout=8)
        except subprocess.TimeoutExpired:
            last = 'timeout'
            time.sleep(1)
            continue
        last = (result.stdout + result.stderr).strip()
        if result.returncode == 0:
            (out / f'{tag}-status.json').write_text(result.stdout)
            return True, last
        time.sleep(1)
    return False, last


def stop_backend(process, timeout=25):
    if not proc_alive(process):
        return process.poll()
    process.send_signal(signal.SIGTERM)
    return wait_exit(process, timeout)


def live_processes(paths):
    wanted = {str(p.resolve()) for p in paths}
    found = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            exe = os.readlink(entry / 'exe')
        except OSError:
            continue
        if exe in wanted:
            found.append({'pid': int(entry.name), 'exe': exe})
    return found


def capture_window_evidence(out, windows):
    tree = run(['xwininfo', '-root', '-tree']).stdout
    (out / 'window-tree.txt').write_text(tree)
    identities = {}
    for window in windows:
        props = run(['xprop', '-id', window, 'WM_CLASS', 'WM_NAME', '_NET_WM_NAME']).stdout
        geometry = run(['xwininfo', '-id', window]).stdout
        identities[window] = {'xprop': props, 'xwininfo': geometry}
    (out / 'windows.json').write_text(json.dumps(identities, indent=2))
    shot = run(['xwd', '-root', '-silent', '-out', str(out / 'root.xwd')])
    if shot.returncode == 0:
        run(['ffmpeg', '-y', '-loglevel', 'error', '-i', str(out / 'root.xwd'), str(out / 'root.png')], timeout=60)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--backend', required=True)
    parser.add_argument('--ui', required=True)
    parser.add_argument('--cli', required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('--window-name', default='Oink')
    args = parser.parse_args()

    backend = Path(args.backend).resolve()
    ui = Path(args.ui).resolve()
    cli = Path(args.cli).resolve()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    evidence = {}

    # Phase A: start the backend directly; IPC must answer, a duplicate must exit 3,
    # and SIGTERM must stop it with code 0.
    direct_log = open(out / 'backend-direct.log', 'wb')
    direct = subprocess.Popen([str(backend)], stdout=direct_log, stderr=subprocess.STDOUT, cwd=str(backend.parent))
    status_ok, status_raw = call_status(cli, out, 'direct')
    evidence['direct_status_ok'] = status_ok
    evidence['direct_status'] = status_raw[:400]

    try:
        duplicate = run([str(backend)], timeout=25)
        duplicate_exit = duplicate.returncode
        duplicate_output = duplicate.stdout + duplicate.stderr
    except subprocess.TimeoutExpired as error:
        duplicate_exit = 'timeout'
        duplicate_output = (error.stdout or b'').decode(errors='replace') if isinstance(error.stdout, bytes) else (error.stdout or '')
    evidence['duplicate_exit'] = duplicate_exit
    (out / 'duplicate.log').write_text(duplicate_output)

    direct_exit = stop_backend(direct)
    direct_log.close()
    evidence['direct_exit_after_sigterm'] = direct_exit
    time.sleep(1)
    residual = live_processes([backend, ui])
    evidence['residual_after_direct'] = residual

    # Phase B: the UI must start its sibling backend, connect, and show its window.
    ui_log = open(out / 'ui.log', 'wb')
    ui_process = subprocess.Popen([str(ui)], stdout=ui_log, stderr=subprocess.STDOUT, cwd=str(ui.parent))
    ui_status_ok, ui_status_raw = call_status(cli, out, 'ui')
    windows = wait_window(args.window_name, ui_process, 40)
    time.sleep(4)
    if windows and proc_alive(ui_process):
        capture_window_evidence(out, windows)
    evidence['ui_spawned_status_ok'] = ui_status_ok
    evidence['ui_status'] = ui_status_raw[:400]
    evidence['ui_windows'] = windows
    evidence['ui_alive_before_term'] = proc_alive(ui_process)

    # The backend is a sibling process the UI started. TERM must be graceful.
    siblings = live_processes([backend])
    evidence['ui_spawned_backend'] = siblings
    backend_exit = None
    if siblings:
        os.kill(siblings[0]['pid'], signal.SIGTERM)
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline and live_processes([backend]):
            time.sleep(0.5)
        backend_exit = 'exited' if not live_processes([backend]) else 'still-running'
    evidence['ui_spawned_backend_exit'] = backend_exit

    if proc_alive(ui_process):
        ui_process.send_signal(signal.SIGTERM)
    ui_exit = wait_exit(ui_process, 20)
    if ui_exit is None:
        ui_process.kill()
        ui_exit = ui_process.wait()
    ui_log.close()
    evidence['ui_exit'] = ui_exit
    evidence['residual_after_all'] = live_processes([backend, ui])

    # Phase C: unsupported sessions must be refused with a visible reason.
    for tag, overrides, drop in [
        ('wayland', {'XDG_SESSION_TYPE': 'wayland', 'WAYLAND_DISPLAY': 'wayland-0'}, ()),
        ('no-display', {'XDG_SESSION_TYPE': 'x11'}, ('DISPLAY', 'WAYLAND_DISPLAY')),
    ]:
        env = dict(os.environ)
        env.update(overrides)
        for key in drop:
            env.pop(key, None)
        try:
            refused = run([str(backend)], timeout=25, env=env)
            refused_exit = refused.returncode
            refused_output = refused.stdout + refused.stderr
        except subprocess.TimeoutExpired as error:
            refused_exit = 'timeout'
            refused_output = (error.stdout or b'').decode(errors='replace') if isinstance(error.stdout, bytes) else (error.stdout or '')
        (out / f'refused-{tag}.log').write_text(refused_output)
        evidence[f'refused_{tag}_exit'] = refused_exit
        evidence[f'refused_{tag}_message'] = refused_output.strip()[:300]

    checks = {
        'direct_status_ok': evidence['direct_status_ok'],
        'duplicate_exit_is_3': evidence['duplicate_exit'] == 3,
        'direct_sigterm_exit_is_0': evidence['direct_exit_after_sigterm'] == 0,
        'no_residual_after_direct': not evidence['residual_after_direct'],
        'ui_spawned_status_ok': evidence['ui_spawned_status_ok'],
        'ui_window_present': bool(evidence['ui_windows']),
        'ui_backend_exited': evidence['ui_spawned_backend_exit'] == 'exited',
        'no_residual_after_all': not evidence['residual_after_all'],
        'wayland_refused': evidence['refused_wayland_exit'] == 1,
        'no_display_refused': evidence['refused_no-display_exit'] == 1,
    }
    evidence['checks'] = checks
    (out / 'summary.json').write_text(json.dumps(evidence, indent=2))
    print('phase3_w2_probe=' + json.dumps(evidence, sort_keys=True))
    if not all(checks.values()):
        print('FAIL: ' + ', '.join(name for name, ok in checks.items() if not ok), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
