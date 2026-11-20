#!/usr/bin/env python3
"""Phase 3 W3 gate: single instance, close-to-hide, reopen chain, graceful exit.

Run through build-support/linux-native-test.py so DISPLAY, HOME, XAUTHORITY and
OINK_HOME belong to a disposable Xephyr session. Evidence (logs, screenshots,
window identities, JSON summary) is written to --out.

The UI gate binary must embed frontend assets (cargo build -p oink-ui
--features custom-protocol --bin oink); a devUrl build has no page to execute.
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


def wait_windows(name, present, process, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            return []
        windows = visible_windows(name)
        if (bool(windows) == present):
            return windows
        time.sleep(0.5)
    return visible_windows(name)


def wait_exit(process, timeout):
    try:
        return process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        return None


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
            state = '?'
            try:
                for line in (entry / 'status').read_text().splitlines():
                    if line.startswith('State:'):
                        state = line.split()[1]
                        break
            except OSError:
                pass
            found.append({'pid': int(entry.name), 'exe': exe, 'state': state})
    return found


def zombies(paths):
    return [process for process in live_processes(paths) if process['state'] == 'Z']


def map_state(window):
    info = run(['xwininfo', '-id', window]).stdout
    for line in info.splitlines():
        if 'Map State:' in line:
            return line.split(':', 1)[1].strip()
    return 'unknown'


def capture(out, tag, windows):
    (out / f'window-tree-{tag}.txt').write_text(run(['xwininfo', '-root', '-tree']).stdout)
    identities = {}
    for window in windows:
        identities[window] = {
            'map_state': map_state(window),
            'xprop': run(['xprop', '-id', window, 'WM_CLASS', 'WM_NAME', '_NET_WM_NAME']).stdout,
            'xwininfo': run(['xwininfo', '-id', window]).stdout,
        }
    (out / f'windows-{tag}.json').write_text(json.dumps(identities, indent=2))
    shot = run(['xwd', '-root', '-silent', '-out', str(out / f'{tag}.xwd')])
    if shot.returncode == 0:
        run(['ffmpeg', '-y', '-loglevel', 'error', '-i', str(out / f'{tag}.xwd'), str(out / f'{tag}.png')], timeout=60)


def terminate(process, timeout=20):
    if process is None or process.poll() is not None:
        return process.returncode if process is not None else None
    process.send_signal(signal.SIGTERM)
    code = wait_exit(process, timeout)
    if code is None:
        process.kill()
        code = process.wait()
    return code


def terminate_orphans(kind, paths, timeout=20):
    """Detached UI/backend processes are not this probe's children; TERM and poll."""
    pids = [entry['pid'] for entry in live_processes(paths)]
    for pid in pids:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and live_processes(paths):
        time.sleep(0.3)
    return not live_processes(paths)


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
    first_ui = None

    def finish_checks():
        checks = {
            'ui_window_present': bool(evidence.get('first_windows')),
            'window_mapped_before_close': evidence.get('first_map_state') == 'IsViewable',
            'close_hid_window_and_kept_ui_alive': evidence.get('window_hidden')
            and evidence.get('hidden_map_state') == 'IsUnMapped'
            and evidence.get('ui_alive_after_close'),
            'backend_survived_window_close': evidence.get('status_after_close_ok', False),
            'second_launch_exits_zero': evidence.get('second_launch_exit') == 0,
            'second_launch_reshows_same_window': evidence.get('same_window_after_second_launch', False),
            'reopen_exits_zero': evidence.get('reopen_exit') == 0,
            'reopen_old_ui_exit_zero': evidence.get('old_ui_exit') == 0,
            'reopen_old_backend_gone': evidence.get('old_backend_gone', False),
            'reopen_new_ui_present': bool(evidence.get('new_ui')),
            'reopen_new_window_present': bool(evidence.get('reopen_windows')),
            'reopen_new_backend_serving': evidence.get('status_after_reopen_ok', False),
            'no_zombies': not evidence.get('zombies', [1]),
            'no_residual_after_all': not evidence.get('residual_after_all', [1]),
        }
        evidence['checks'] = checks
        (out / 'summary.json').write_text(json.dumps(evidence, indent=2))
        print('phase3_w3_probe=' + json.dumps(evidence, sort_keys=True))
        if not all(checks.values()):
            print('FAIL: ' + ', '.join(name for name, ok in checks.items() if not ok), file=sys.stderr)
            return 1
        return 0

    # Phase A: UI starts its sibling backend and reveals its window.
    ui_log = open(out / 'ui-first.log', 'wb')
    first_ui = subprocess.Popen([str(ui)], stdout=ui_log, stderr=subprocess.STDOUT, cwd=str(ui.parent))
    status_ok, status_raw = call_status(cli, out, 'first')
    first_windows = wait_windows(args.window_name, True, first_ui, 45)
    time.sleep(3)
    first_windows = visible_windows(args.window_name)
    if first_windows and first_ui.poll() is None:
        capture(out, 'connected', first_windows)
    evidence['first_status_ok'] = status_ok
    evidence['first_status'] = status_raw[:400]
    evidence['first_windows'] = first_windows
    evidence['first_map_state'] = map_state(first_windows[0]) if first_windows else 'missing'
    try:
        # Phase B: closing the product window hides it; the backend and UI survive.
        hidden = False
        if first_windows:
            # xdotool windowclose destroys the X window directly; the EWMH close
            # request reaches the app as the same WM_DELETE a window manager sends.
            run(['wmctrl', '-ic', hex(int(first_windows[0]))])
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline and visible_windows(args.window_name):
                time.sleep(0.3)
            hidden = not visible_windows(args.window_name)
            capture(out, 'hidden', [])
        evidence['window_hidden'] = hidden
        evidence['hidden_map_state'] = map_state(first_windows[0]) if first_windows else 'missing'
        evidence['ui_alive_after_close'] = first_ui.poll() is None
        status_after_close_ok, status_after_close = call_status(cli, out, 'after-close')
        evidence['status_after_close_ok'] = status_after_close_ok
        evidence['status_after_close'] = status_after_close[:400]

        # Phase C: a second launch hands off to the hidden first instance.
        try:
            second = run([str(ui)], timeout=60)
            evidence['second_launch_exit'] = second.returncode
            evidence['second_launch_output'] = (second.stdout + second.stderr)[:400]
        except subprocess.TimeoutExpired as error:
            evidence['second_launch_exit'] = 'timeout'
            evidence['second_launch_output'] = str(error)[:400]
        reshown = wait_windows(args.window_name, True, first_ui, 30)
        time.sleep(3)
        reshown = visible_windows(args.window_name)
        if reshown and first_ui.poll() is None:
            capture(out, 'reshown', reshown)
        evidence['reshown_windows'] = reshown
        evidence['same_window_after_second_launch'] = bool(set(reshown) & set(first_windows))
        evidence['ui_alive_after_second_launch'] = first_ui.poll() is None

        # Phase D: oink-agent reopen stops backend then UI, and starts a fresh UI.
        old_backend_pids = {entry['pid'] for entry in live_processes([backend])}
        evidence['old_backend_pids'] = sorted(old_backend_pids)
        try:
            reopened = run([str(cli), 'reopen'], timeout=60)
            evidence['reopen_exit'] = reopened.returncode
            evidence['reopen_output'] = (reopened.stdout + reopened.stderr)[:400]
        except subprocess.TimeoutExpired as error:
            evidence['reopen_exit'] = 'timeout'
            evidence['reopen_output'] = str(error)[:400]
        evidence['old_ui_exit'] = wait_exit(first_ui, 20)
        if evidence['old_ui_exit'] is None:
            first_ui.kill()
            first_ui.wait()
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and (
                {entry['pid'] for entry in live_processes([backend])} & old_backend_pids):
            time.sleep(0.3)
        evidence['old_backend_gone'] = not (
            {entry['pid'] for entry in live_processes([backend])} & old_backend_pids)

        deadline = time.monotonic() + 45
        new_ui, reopen_windows = [], []
        while time.monotonic() < deadline:
            candidates = [entry for entry in live_processes([ui]) if entry['pid'] != first_ui.pid]
            windows = visible_windows(args.window_name)
            if candidates and windows:
                new_ui, reopen_windows = candidates, windows
                break
            time.sleep(0.5)
        evidence['new_ui'] = new_ui
        evidence['reopen_windows'] = reopen_windows
        if reopen_windows and new_ui:
            capture(out, 'reopened', reopen_windows)
        status_after_reopen_ok, status_after_reopen = call_status(cli, out, 'after-reopen')
        evidence['status_after_reopen_ok'] = status_after_reopen_ok
        evidence['status_after_reopen'] = status_after_reopen[:400]
    finally:
        # The reopened UI is detached (setsid); always terminate it explicitly.
        terminate(first_ui)
        terminate_orphans(ui, [ui])
        terminate_orphans(backend, [backend])
        ui_log.close()

    time.sleep(1)
    evidence['residual_after_all'] = live_processes([backend, ui])
    evidence['zombies'] = zombies([backend, ui])
    return finish_checks()


if __name__ == '__main__':
    sys.exit(main())
