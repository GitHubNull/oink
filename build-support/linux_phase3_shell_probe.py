#!/usr/bin/env python3
"""Launch the oink UI in the private Xephyr session and record its first window.

Run through build-support/linux-native-test.py so DISPLAY, HOME and XAUTHORITY
belong to a disposable environment. Evidence (stdout log, window tree, xprop
identities, root screenshot) is written to --out.
"""
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path


def run(command, timeout=10):
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout)


def visible_windows(name):
    try:
        found = run(['xdotool', 'search', '--onlyvisible', '--name', name]).stdout.split()
    except subprocess.TimeoutExpired:
        return []
    return found


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--exe', required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('--window-name', default='Oink')
    parser.add_argument('--timeout', type=float, default=40)
    parser.add_argument('--settle', type=float, default=6)
    args = parser.parse_args()

    exe = Path(args.exe).resolve()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    log_path = out / 'oink-ui.log'
    with open(log_path, 'wb') as log:
        process = subprocess.Popen(
            [str(exe)], stdout=log, stderr=subprocess.STDOUT, cwd=str(exe.parent)
        )
        deadline = time.monotonic() + args.timeout
        windows = []
        while time.monotonic() < deadline:
            if process.poll() is not None:
                break
            windows = visible_windows(args.window_name)
            if windows:
                break
            time.sleep(0.5)
        time.sleep(args.settle)

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
            run(
                ['ffmpeg', '-y', '-loglevel', 'error', '-i', str(out / 'root.xwd'), str(out / 'root.png')],
                timeout=60,
            )

        alive = process.poll() is None
        exit_code = None
        if alive:
            process.terminate()
        try:
            exit_code = process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            exit_code = process.wait()

    summary = {
        'exe': str(exe),
        'visible_windows': windows,
        'alive_before_signal': alive,
        'exit_after_sigterm': exit_code,
        'log_bytes': log_path.stat().st_size,
    }
    print('phase3_shell_probe=' + json.dumps(summary, sort_keys=True))
    if not windows or not alive:
        print('FAIL: oink UI window did not appear', file=sys.stderr)
        tail = log_path.read_text(errors='replace').splitlines()[-20:]
        print('\n'.join(tail), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
