#!/usr/bin/env python3
"""Phase 4 W6 mixed load: real UI/backend plus overlay driver inside a private
Xephyr session. Run through build-support/linux-native-test.py. Evidence stays
structural (counts, resource samples, exit codes), never actual input contents.
"""
import argparse
import csv
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

TICK = 10
TICK_SECONDS = os.sysconf('SC_CLK_TCK')


def run(command, timeout=20):
    try:
        return subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(command, 1, '', 'timeout')


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


def metrics(pid):
    result = {'rss_kb': 0, 'fds': 0, 'threads': 0, 'ticks': 0}
    try:
        for line in Path(f'/proc/{pid}/status').read_text().splitlines():
            if line.startswith('VmRSS:'):
                result['rss_kb'] = int(line.split()[1])
            elif line.startswith('Threads:'):
                result['threads'] = int(line.split()[1])
        result['fds'] = len(list(Path(f'/proc/{pid}/fd').iterdir()))
        stat = Path(f'/proc/{pid}/stat').read_text()
        rest = stat[stat.rindex(')') + 2:].split()
        result['ticks'] = int(rest[11]) + int(rest[12])
    except (OSError, ValueError, IndexError):
        pass
    return result


def role_sample(paths, previous, dt):
    total = {'rss_kb': 0, 'fds': 0, 'threads': 0, 'ticks': 0, 'cpu': 0.0}
    for process in live_processes(paths):
        now = metrics(process['pid'])
        before = previous.get(process['pid'])
        if before is not None and dt > 0:
            total['cpu'] += (now['ticks'] - before['ticks']) * 100.0 / (TICK_SECONDS * dt)
        previous[process['pid']] = now
        for key in ('rss_kb', 'fds', 'threads', 'ticks'):
            total[key] += now[key]
    return total


def root_windows():
    output = run(['xwininfo', '-root', '-children']).stdout
    return len([line for line in output.splitlines() if re.match(r'\s+0x[0-9a-f]+ ', line)])


def xrandr_signature():
    return ' '.join(run(['xrandr', '--query']).stdout.split())


def window_geometry(window):
    output = run(['xdotool', 'getwindowgeometry', '--shell', window]).stdout
    values = {}
    for line in output.splitlines():
        if '=' in line:
            name, value = line.split('=', 1)
            values[name] = value
    try:
        return tuple(int(values[name]) for name in ('X', 'Y', 'WIDTH', 'HEIGHT'))
    except (KeyError, ValueError):
        return None


def keyboard_state():
    listing = run(['xinput', 'list', '--short']).stdout
    ids = re.findall(r'id=(\d+)\s+\[slave\s+keyboard', listing)
    if not ids:
        return None
    down = keys = seen = 0
    for device in ids:
        output = run(['xinput', 'query-state', device]).stdout
        states = re.findall(r'key\[\d+\]=(down|up)', output)
        if states:
            seen += 1
            down += states.count('down')
            keys += len(states)
    if not seen:
        return None
    return {'devices': seen, 'down': down, 'keys': keys}


def clicks_at(x, y, wheel=False):
    run(['xdotool', 'mousemove', str(x), str(y)])
    if wheel:
        run(['xdotool', 'click', '4'])
        run(['xdotool', 'click', '5'])
    else:
        run(['xdotool', 'click', '1'])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--ui', required=True)
    parser.add_argument('--backend', required=True)
    parser.add_argument('--cli', required=True)
    parser.add_argument('--overlay', required=True)
    parser.add_argument('--duration', type=int, default=1800)
    parser.add_argument('--pause-every', type=int, default=300)
    parser.add_argument('--out', required=True)
    args = parser.parse_args()

    ui = Path(args.ui).resolve()
    backend = Path(args.backend).resolve()
    overlay = Path(args.overlay).resolve()
    cli = Path(args.cli).resolve()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    evidence = {'duration': args.duration, 'samples': []}
    processes = {'ui': None, 'overlay': None, 'xev': None}
    logs = {}
    started = time.monotonic()

    def elapsed():
        return round(time.monotonic() - started, 1)

    def record(role, process):
        processes[role] = process

    try:
        base_windows = root_windows()
        base_display = xrandr_signature()
        evidence['baseline_windows'] = base_windows
        logs['ui'] = open(out / 'ui.log', 'wb')
        record('ui', subprocess.Popen([str(ui)], stdout=logs['ui'], stderr=subprocess.STDOUT,
                                      cwd=str(ui.parent)))
        status_ok = False
        windows = []
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if processes['ui'].poll() is not None:
                break
            result = run([str(cli), 'call', 'status'], timeout=8)
            windows = run(['xdotool', 'search', '--onlyvisible', '--name', 'Oink']).stdout.split()
            if result.returncode == 0 and windows:
                status_ok = True
                break
            time.sleep(1)
        evidence['ui_boot_ok'] = status_ok
        evidence['ui_window'] = windows
        if not status_ok:
            raise RuntimeError('UI or backend did not become ready')
        time.sleep(2)
        geometry = window_geometry(windows[0]) if windows else None
        evidence['ui_geometry'] = geometry
        screen = (1280, 800)
        outside = [p for p in [(4, 4), (1276, 4), (4, 796), (1276, 796)]
                   if not geometry or not (geometry[0] <= p[0] < geometry[0] + geometry[2]
                                           and geometry[1] <= p[1] < geometry[1] + geometry[3])]
        click_point = outside[0] if outside else (4, 4)
        evidence['click_point'] = click_point

        logs['xev'] = open(out / 'xev.log', 'wb')
        record('xev', subprocess.Popen(['stdbuf', '-oL', 'xev', '-root', '-event', 'button'],
                                       stdout=logs['xev'], stderr=subprocess.STDOUT))
        logs['overlay'] = open(out / 'overlay.log', 'wb')
        record('overlay', subprocess.Popen(
            [str(overlay), '--seconds', str(max(60, args.duration - 60))],
            stdout=logs['overlay'], stderr=subprocess.STDOUT, cwd=str(overlay.parent)))

        csv_file = open(out / 'samples.csv', 'w', newline='')
        writer = csv.writer(csv_file)
        writer.writerow(['elapsed_s', 'ui_rss_kb', 'ui_cpu_pct', 'ui_fds', 'ui_threads',
                         'overlay_rss_kb', 'overlay_cpu_pct', 'overlay_fds', 'overlay_threads',
                         'windows', 'shift_down'])
        previous = {'ui': {}, 'overlay': {}}
        counters = {'status': 0, 'notify': 0, 'pauses': 0, 'clicks': 0}
        displays_stable = True
        next_status = next_pause = next_notify = next_input = time.monotonic()
        last_sample = [0.0]
        end = started + args.duration
        while time.monotonic() < end:
            now = time.monotonic()
            if now >= next_status:
                result = run([str(cli), 'call', 'status'], timeout=10)
                counters['status'] += int(result.returncode == 0)
                if xrandr_signature() != base_display:
                    displays_stable = False
                next_status = now + 60
            if now >= next_pause:
                request = f'load-{elapsed()}'
                paused = run([str(cli), 'call', 'set_local_paused',
                              json.dumps({'requestId': request, 'paused': True})], timeout=10)
                time.sleep(8)
                clicks_at(*click_point)
                counters['clicks'] += 1
                resumed = run([str(cli), 'call', 'set_local_paused',
                               json.dumps({'requestId': request + 'r', 'paused': False})], timeout=10)
                counters['pauses'] += int(paused.returncode == 0 and resumed.returncode == 0)
                if paused.returncode or resumed.returncode:
                    evidence.setdefault('cli_errors', []).append(
                        (paused.stderr or paused.stdout or resumed.stderr or resumed.stdout)[:200])
                next_pause = time.monotonic() + args.pause_every
            if now >= next_notify:
                payload = {'notification': {'level': 'info', 'title': '负载通知',
                                            'description': '仅界面压力测试'}}
                result = run([str(cli), 'call', 'notify', json.dumps(payload)], timeout=10)
                counters['notify'] += int(result.returncode == 0)
                next_notify = now + 120
            if now >= next_input:
                point = (120 + (elapsed() * 37) % 1000, 120 + (elapsed() * 19) % 560)
                run(['xdotool', 'mousemove', str(point[0]), str(point[1])])
                clicks_at(*click_point, wheel=True)
                run(['xdotool', 'keydown', 'Shift_L'])
                run(['xdotool', 'keyup', 'Shift_L'])
                counters['clicks'] += 1
                next_input = now + 12
            stamp = time.monotonic()
            dt = stamp - last_sample[0] if last_sample[0] else 0
            last_sample[0] = stamp
            ui_total = role_sample([ui, backend], previous['ui'], dt)
            overlay_total = role_sample([overlay], previous['overlay'], dt)
            state = keyboard_state()
            shift = state['down'] if state else -1
            windows = root_windows()
            writer.writerow([elapsed(), ui_total['rss_kb'], round(ui_total['cpu'], 1),
                             ui_total['fds'], ui_total['threads'], overlay_total['rss_kb'],
                             round(overlay_total['cpu'], 1), overlay_total['fds'],
                             overlay_total['threads'], windows, shift])
            csv_file.flush()
            evidence['samples'].append({
                'elapsed': elapsed(), 'ui_rss_kb': ui_total['rss_kb'],
                'overlay_rss_kb': overlay_total['rss_kb'], 'windows': windows,
                'shift_down': shift})
            time.sleep(max(0.5, TICK - (time.monotonic() - now)))

        csv_file.close()
        evidence['counters'] = counters
        evidence['displays_stable'] = displays_stable
        overlay_code = processes['overlay'].wait(timeout=120)
        evidence['overlay_exit'] = overlay_code
        for line in (out / 'overlay.log').read_text(errors='replace').splitlines():
            if line.startswith('phase4_overlay_load=ok'):
                evidence['overlay_ok'] = True
        evidence['overlay_ok'] = evidence.get('overlay_ok', False)
    except Exception as error:  # noqa: BLE001 - the summary must record any failure
        evidence['error'] = str(error)[:400]
    finally:
        for role in ('overlay', 'xev', 'ui'):
            process = processes.get(role)
            if process is not None and process.poll() is None:
                process.send_signal(signal.SIGTERM)
        for role in ('overlay', 'xev', 'ui'):
            process = processes.get(role)
            if process is not None and process.poll() is None:
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for process in live_processes([ui, backend, overlay]):
            try:
                os.kill(process['pid'], signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and live_processes([ui, backend]):
            time.sleep(0.3)
        for log in logs.values():
            log.close()
        time.sleep(2)
        evidence['residual'] = live_processes([ui, backend, overlay])
        evidence['zombies'] = [p for p in evidence['residual'] if p['state'] == 'Z']
        final_windows = root_windows()
        evidence['final_windows'] = final_windows
        evidence['windows_reclaimed'] = final_windows <= evidence.get('baseline_windows', 0)
        xev_log = out / 'xev.log'
        evidence['root_button_events'] = (xev_log.read_text(errors='replace').count('ButtonPress')
                                          if xev_log.exists() else 0)
        state = keyboard_state()
        evidence['final_shift_down'] = state['down'] if state else -1

        samples = evidence.get('samples', [])
        peak_ui = max((s['ui_rss_kb'] for s in samples), default=0)
        peak_overlay = max((s['overlay_rss_kb'] for s in samples), default=0)
        evidence['peak_rss_kb'] = {'ui_backend': peak_ui, 'overlay': peak_overlay}
        checks = {
            'ui_boot_ok': evidence.get('ui_boot_ok', False),
            'ui_window_present': bool(evidence.get('ui_window')),
            'overlay_driver_ok': evidence.get('overlay_exit') == 0 and evidence.get('overlay_ok'),
            'status_polls': evidence.get('counters', {}).get('status', 0)
            >= max(3, args.duration // 120),
            'pause_cycles': evidence.get('counters', {}).get('pauses', 0) >= 2,
            'notifications_accepted': evidence.get('counters', {}).get('notify', 0) >= 2,
            'root_button_events': evidence.get('root_button_events', 0) >= 4,
            'no_stuck_shift': evidence.get('final_shift_down') == 0,
            'displays_stable': evidence.get('displays_stable', False),
            'sample_rows': len(samples) >= max(10, args.duration // TICK - 5),
            'windows_reclaimed': evidence.get('windows_reclaimed', False),
            'no_residual': not evidence.get('residual'),
            'no_zombies': not evidence.get('zombies'),
            'bounded_memory': peak_ui + peak_overlay < 3 * 1024 * 1024,
        }
        evidence['checks'] = checks
        (out / 'summary.json').write_text(json.dumps(evidence, indent=2))
        print('phase4_load=' + json.dumps(checks, sort_keys=True))
        print('phase4_load_summary=' + json.dumps({
            'peak_rss_kb': evidence['peak_rss_kb'], 'counters': evidence.get('counters'),
            'root_button_events': evidence['root_button_events'],
            'final_windows': final_windows, 'baseline_windows': evidence.get('baseline_windows')}))
        if not all(checks.values()):
            print('FAIL: ' + ', '.join(name for name, ok in checks.items() if not ok),
                  file=sys.stderr)
            return 1
        return 0


if __name__ == '__main__':
    sys.exit(main())
