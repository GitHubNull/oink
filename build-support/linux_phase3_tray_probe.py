#!/usr/bin/env python3
"""Phase 3 W5 gate: real Linux tray registration and panel lifecycle.

Run through build-support/linux-native-test.py so DISPLAY, HOME, XAUTHORITY and
OINK_HOME belong to a disposable Xephyr session. Phase A starts without a
StatusNotifierWatcher and checks the documented silent degradation. Phase B
starts build-support/linux_sni_watcher.py first, drives the product's real
libayatana-appindicator dbusmenu and checks the panel open/close/reopen chain.

The UI gate binary must embed frontend assets (cargo build -p oink-ui
--features custom-protocol --bin oink); a devUrl build has no page to execute.
"""
import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

import dbus

PANEL_LABELS = ('Open Panel', '打开面板', 'パネルを開く', '패널 열기')
SHOW_LABELS = ('Show main window', '显示主界面', 'メインウインドウを表示', '주 창 표시')
WATCHER = 'org.kde.StatusNotifierWatcher'


def run(command, timeout=10, env=None):
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout, env=env)


def visible_windows(name='Oink'):
    try:
        return run(['xdotool', 'search', '--onlyvisible', '--name', f'^{name}$']).stdout.split()
    except subprocess.TimeoutExpired:
        return []


def window_geometry(window):
    geometry = {}
    info = run(['xwininfo', '-id', window]).stdout
    for line in info.splitlines():
        key, _, value = line.partition(':')
        if key.strip() in ('Width', 'Height', 'Map State', 'Absolute upper-left X',
                           'Absolute upper-left Y'):
            geometry[key.strip()] = value.strip()
    return geometry


def size_of(geometry):
    try:
        return int(geometry.get('Width', 0)), int(geometry.get('Height', 0))
    except ValueError:
        return 0, 0


def main_windows():
    found = []
    for window in visible_windows():
        geometry = window_geometry(window)
        width, height = size_of(geometry)
        if geometry.get('Map State') == 'IsViewable' and width >= 900 and height >= 600:
            found.append(window)
    return found


def panel_windows():
    found = []
    for window in visible_windows():
        geometry = window_geometry(window)
        width, height = size_of(geometry)
        if geometry.get('Map State') == 'IsViewable' and 350 <= width <= 370 and 100 <= height <= 1000:
            found.append((window, geometry))
    return found


def wait_for(condition, process, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            return condition()
        value = condition()
        if value:
            return value
        time.sleep(0.3)
    return condition()


def call_status(cli, out, tag):
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


def capture(out, tag, windows):
    (out / f'window-tree-{tag}.txt').write_text(run(['xwininfo', '-root', '-tree']).stdout)
    identities = {}
    for window in windows:
        identities[window] = {
            'geometry': window_geometry(window),
            'xprop': run(['xprop', '-id', window, 'WM_CLASS', 'WM_NAME', '_NET_WM_NAME']).stdout,
            'xwininfo': run(['xwininfo', '-id', window]).stdout,
        }
    (out / f'windows-{tag}.json').write_text(json.dumps(identities, indent=2))
    shot = run(['xwd', '-root', '-silent', '-out', str(out / f'{tag}.xwd')])
    if shot.returncode == 0:
        run(['ffmpeg', '-y', '-loglevel', 'error', '-i', str(out / f'{tag}.xwd'),
             str(out / f'{tag}.png')], timeout=60)


def terminate(process, timeout=20):
    if process is None or process.poll() is not None:
        return process.returncode if process is not None else None
    process.send_signal(signal.SIGTERM)
    try:
        return process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        return process.wait()


def terminate_orphans(paths, timeout=20):
    for entry in live_processes(paths):
        try:
            os.kill(entry['pid'], signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and live_processes(paths):
        time.sleep(0.3)
    return not live_processes(paths)


def log_text(paths):
    text = ''
    for path in paths:
        try:
            text += path.read_text(errors='replace')
        except OSError:
            pass
    return text


def watcher_state(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def plain(value):
    if isinstance(value, dict):
        return {str(key): plain(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [plain(item) for item in value]
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    return str(value)


def introspected(bus, name, path):
    obj = bus.get_object(name, path)
    xml = dbus.Interface(obj, 'org.freedesktop.DBus.Introspectable').Introspect()
    root = ET.fromstring(xml)
    base = path.rstrip('/')
    return [f'{base}/{node.get("name")}' for node in root.findall('node') if node.get('name')]


def endpoint(state):
    for entry in reversed(state.get('items', [])):
        service, sender = entry.get('service', ''), entry.get('sender', '')
        if service.startswith('/'):
            return [sender] if sender else [], service
        if service:
            return [service] + ([sender] if sender and sender != service else []), None
    return [], None


def item_menu(bus, state):
    names, hint = endpoint(state)
    candidates = [hint] if hint else []
    for name in names:
        roots = ['/StatusNotifierItem', '/org/ayatana/NotificationItem']
        candidates.extend(roots)
        for root in roots:
            try:
                candidates.extend(introspected(bus, name, root))
            except dbus.DBusException:
                pass
        for path in dict.fromkeys(candidates):
            props = None
            try:
                obj = bus.get_object(name, path)
                props = dbus.Interface(obj, 'org.freedesktop.DBus.Properties')
                menu = str(props.Get('org.kde.StatusNotifierItem', 'Menu'))
                if menu and menu != '/':
                    return name, path, menu
            except dbus.DBusException:
                pass
            try:
                fallback = dbus.Interface(bus.get_object(name, path), 'com.canonical.dbusmenu')
                fallback.GetLayout(dbus.UInt32(0), 1, dbus.Array([], signature='s'))
                return name, path, path
            except dbus.DBusException:
                continue
    return None, None, None


def menu_items(bus, name, path):
    menu = dbus.Interface(bus.get_object(name, path), 'com.canonical.dbusmenu')
    try:
        menu.AboutToShow(dbus.Int32(0))
    except dbus.DBusException:
        pass
    revision, layout = menu.GetLayout(dbus.UInt32(0), -1, dbus.Array([], signature='s'))
    found = []

    def walk(node):
        item_id, props, children = node
        found.append((int(item_id), {str(key): str(value) for key, value in props.items()}))
        for child in children:
            walk(child)

    walk(layout)
    return menu, int(revision), found, plain(layout)


def find_item(items, labels):
    wanted = {label.replace('_', '') for label in labels}
    for item_id, props in items:
        if props.get('label', '').replace('_', '') in wanted:
            return item_id
    return None


def click(menu, item_id):
    menu.Event(dbus.Int32(item_id), 'clicked', dbus.Int32(0), dbus.UInt32(0))


def wait_menu(bus, state, labels, timeout=45):
    deadline = time.monotonic() + timeout
    last = 'no item endpoint'
    while time.monotonic() < deadline:
        name, path, menu_path = item_menu(bus, state)
        if name:
            try:
                menu, revision, items, layout = menu_items(bus, name, menu_path)
                item_id = find_item(items, labels)
                if item_id is not None:
                    return menu, item_id, revision, items, layout
                last = 'labels=' + json.dumps([props.get('label') for _, props in items])
            except dbus.DBusException as error:
                last = str(error)
        time.sleep(0.5)
    return None, None, None, [], {'unavailable': last}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--backend', required=True)
    parser.add_argument('--ui', required=True)
    parser.add_argument('--cli', required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('--window-name', default='Oink')
    parser.add_argument('--lang', help='UI locale override, e.g. zh_CN.UTF-8')
    args = parser.parse_args()

    backend = Path(args.backend).resolve()
    ui = Path(args.ui).resolve()
    cli = Path(args.cli).resolve()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    ui_env = dict(os.environ, LANG=args.lang, LC_ALL=args.lang) if args.lang else None
    evidence = {}
    first_ui = None
    second_ui = None
    watcher = None
    watcher_log = None

    def finish_checks():
        checks = {}
        checks.update(evidence.get('phase_a_checks', {}))
        checks.update(evidence.get('phase_b_checks', {}))
        evidence['checks'] = checks
        (out / 'summary.json').write_text(json.dumps(evidence, indent=2))
        print('phase3_tray_probe=' + json.dumps(evidence, sort_keys=True))
        failed = [name for name, ok in checks.items() if not ok]
        if failed:
            print('FAIL: ' + ', '.join(failed), file=sys.stderr)
            return 1
        return 0

    try:
        # Phase A: no StatusNotifierWatcher -> the tray is skipped with a warning.
        ui_log = open(out / 'ui-phase-a.log', 'wb')
        first_ui = subprocess.Popen([str(ui)], stdout=ui_log, stderr=subprocess.STDOUT,
                                    cwd=str(ui.parent), env=ui_env)
        time.sleep(4)
        phase_a_main = wait_for(main_windows, first_ui, 45)
        status_a_ok, status_a = call_status(cli, out, 'phase-a')
        time.sleep(2)
        first_ui.poll()
        phase_a_log = log_text([out / 'ui-phase-a.log'])
        ui_log.close()
        first_ui_pid = first_ui.pid
        ui_alive_a = first_ui.poll() is None
        evidence['phase_a'] = {
            'main_windows': phase_a_main,
            'main_geometry': window_geometry(phase_a_main[0]) if phase_a_main else {},
            'status_ok': status_a_ok,
            'status': status_a[:400],
            'ui_alive': ui_alive_a,
            'watcher_warning': 'StatusNotifierWatcher absent' in phase_a_log
            or 'Linux tray disabled' in phase_a_log,
            'panic': 'panicked at' in phase_a_log,
            'ui_pid': first_ui_pid,
        }
        if phase_a_main and ui_alive_a:
            capture(out, 'phase-a', phase_a_main)
        evidence['phase_a_checks'] = {
            'phase_a_ui_window_present': bool(phase_a_main),
            'phase_a_status_ok': status_a_ok,
            'phase_a_ui_stayed_alive': ui_alive_a,
            'phase_a_watcher_warning_logged': evidence['phase_a']['watcher_warning'],
            'phase_a_no_panic': not evidence['phase_a']['panic'],
        }
        terminate(first_ui)

        # Phase B: a watcher exists, so libayatana must register the tray item.
        # The watcher runs with its own cwd, so its state path must be absolute.
        watcher_state_path = (out / 'watcher-state.json').resolve()
        watcher_log = open(out / 'sni-watcher.log', 'wb')
        watcher = subprocess.Popen(
            [sys.executable, str(Path(__file__).with_name('linux_sni_watcher.py')),
             '--state', str(watcher_state_path)],
            stdout=watcher_log, stderr=subprocess.STDOUT, cwd=str(Path(__file__).parent))
        watcher_ready = wait_for(lambda: bool(watcher_state(watcher_state_path).get('ready')),
                                 watcher, 20)
        watcher_owned = 'true' in run(['gdbus', 'call', '--session', '--dest',
                                       'org.freedesktop.DBus', '--object-path', '/org/freedesktop/DBus',
                                       '--method', 'org.freedesktop.DBus.NameHasOwner',
                                       WATCHER]).stdout
        ui_b_log = open(out / 'ui-phase-b.log', 'wb')
        second_ui = subprocess.Popen([str(ui)], stdout=ui_b_log, stderr=subprocess.STDOUT,
                                     cwd=str(ui.parent), env=ui_env)
        phase_b_main = wait_for(main_windows, second_ui, 45)
        state = {}
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            state = watcher_state(watcher_state_path)
            if state.get('items'):
                break
            time.sleep(0.5)
        status_b_ok, status_b = call_status(cli, out, 'phase-b')
        bus = dbus.SessionBus()
        panel_menu, panel_id, revision, items, layout = wait_menu(bus, state, PANEL_LABELS)
        (out / 'menu-layout.json').write_text(json.dumps(layout, indent=2))
        evidence['phase_b'] = {
            'watcher_ready': bool(watcher_ready),
            'watcher_name_owned': bool(watcher_owned),
            'main_windows': phase_b_main,
            'status_ok': status_b_ok,
            'status': status_b[:400],
            'watcher_state': state,
            'menu_revision': revision,
            'menu_items': items,
            'panel_item_id': panel_id,
            'panel_open_window': None,
            'panel_open_geometry': {},
            'panel_closed_after_second_click': False,
            'panel_reopened_after_third_click': False,
            'main_reshown_from_menu': False,
        }
        opened = False
        closed = False
        reopened = False
        reshown = False
        if panel_id is not None:
            click(panel_menu, panel_id)
            found = wait_for(panel_windows, second_ui, 30)
            if found:
                opened = True
                evidence['phase_b']['panel_open_window'] = found[0][0]
                evidence['phase_b']['panel_open_geometry'] = found[0][1]
                capture(out, 'panel-open', [window for window, _ in found] + phase_b_main)
            click(panel_menu, panel_id)
            closed = bool(wait_for(lambda: not panel_windows(), second_ui, 30))
            evidence['phase_b']['panel_closed_after_second_click'] = closed
            capture(out, 'panel-closed', main_windows())
            click(panel_menu, panel_id)
            found = wait_for(panel_windows, second_ui, 30)
            reopened = bool(found)
            evidence['phase_b']['panel_reopened_after_third_click'] = reopened
            if reopened:
                evidence['phase_b']['panel_reopen_window'] = found[0][0]
                evidence['phase_b']['panel_reopen_geometry'] = found[0][1]
                capture(out, 'panel-reopened', [window for window, _ in found] + main_windows())
            if reopened and phase_b_main:
                run(['wmctrl', '-ic', hex(int(phase_b_main[0]))])
                hidden = wait_for(lambda: not main_windows(), second_ui, 20)
                show_menu, show_id, show_revision, show_items, show_layout = wait_menu(
                    bus, watcher_state(watcher_state_path), SHOW_LABELS)
                evidence['phase_b']['show_menu_revision'] = show_revision
                evidence['phase_b']['show_item_id'] = show_id
                if show_id is not None:
                    click(show_menu, show_id)
                    reshown = bool(wait_for(main_windows, second_ui, 20))
                evidence['phase_b']['main_hidden_before_show_click'] = bool(hidden)
                evidence['phase_b']['main_reshown_from_menu'] = reshown
                if reshown:
                    capture(out, 'main-reshown', main_windows())
        status_end_ok, status_end = call_status(cli, out, 'phase-b-end')
        phase_b_log = log_text([out / 'ui-phase-b.log'])
        evidence['phase_b']['status_end_ok'] = status_end_ok
        evidence['phase_b']['status_end'] = status_end[:400]
        evidence['phase_b']['panic'] = 'panicked at' in phase_b_log
        panel_geometry = evidence['phase_b']['panel_open_geometry']
        panel_width, panel_height = size_of(panel_geometry)
        evidence['phase_b_checks'] = {
            'tray_watcher_ready': bool(watcher_ready),
            'tray_main_window_present': bool(phase_b_main),
            'tray_status_ok': status_b_ok,
            'tray_item_registered': bool(state.get('items')),
            'tray_menu_contains_panel': panel_id is not None,
            'tray_panel_open_viewable_360_wide': opened and panel_width == 360
            and 100 <= panel_height <= 1000,
            'tray_panel_closed_on_second_click': closed,
            'tray_panel_reopened_on_third_click': reopened,
            'tray_main_reshown_from_menu': reshown,
            'tray_status_ok_after_actions': status_end_ok,
            'tray_no_panic': not evidence['phase_b']['panic'],
            'tray_no_zombies': not evidence.get('zombies', [1]),
            'tray_no_residual_after_all': not evidence.get('residual_after_all', [1]),
        }
    finally:
        terminate(second_ui)
        terminate(first_ui)
        terminate_orphans([ui])
        terminate_orphans([backend])
        terminate(watcher)
        if watcher_log:
            watcher_log.close()
        try:
            ui_b_log.close()
        except (NameError, OSError):
            pass

    time.sleep(1)
    evidence['residual_after_all'] = live_processes([backend, ui])
    evidence['zombies'] = zombies([backend, ui])
    if evidence.get('phase_b_checks'):
        evidence['phase_b_checks']['tray_no_zombies'] = not evidence['zombies']
        evidence['phase_b_checks']['tray_no_residual_after_all'] = not evidence['residual_after_all']
    return finish_checks()


if __name__ == '__main__':
    sys.exit(main())
