#!/usr/bin/env python3
"""Dedicated-master input delivery to real toolkit applications (E wave).

Binds an exclusive Oink-Toolkit XI2 master (input-toolkit-route, logic
extracted from linux_input_matrix.c) to a real application window via
XISetClientPointer + XISetFocus and injects through the master's own XTEST
slaves. Primary evidence is NATIVE application behavior: Qt widget callbacks
(exact native scan codes), zenity entry text (exact typed text incl. shift
chords and server auto-repeat), browser trusted DOM events, and nemo view
navigation (keys + scroll, pixel/title evidence). Physical HID, touchpad,
gestures and lock screen remain F.

Platform note (2026-11-20, Xephyr+metacity): a minimal custom GTK fixture
deflected every XTEST event (XSendEvent did reach it) while real GTK3
applications (nemo, zenity) receive XTEST normally; GTK evidence here uses
real applications, the stronger witness.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from linux_app_x11 import X11
import linux_app_harness as harness
from linux_dnd_app_scenarios import PageServer, row
from linux_dnd_protocol import Failure, check, isolated_environment

XK_A, XK_B = 0x61, 0x62
XK_RETURN, XK_SHIFT_L, XK_CONTROL_L = 0xFF0D, 0xFFE1, 0xFFE3
XK_LEVEL3 = 0xFE03
XK_AUDIO_MUTE = 0x1008FF11
SCROLL_BUTTONS = {4: 'up', 5: 'down', 6: 'left', 7: 'right'}


class Router:
    """Exclusive-master router process; one JSON line per query on stdout."""

    def __init__(self, binary, xid, environment):
        self.process = subprocess.Popen([str(binary), str(xid)], env=environment,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, bufsize=1)
        self.bound = self.readline('bound', 15)

    def readline(self, want, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise Failure(f'router exited {self.process.returncode}: '
                              f'{self.process.stderr.read()}')
            line = self.process.stdout.readline()
            if not line:
                time.sleep(0.02)
                continue
            report = json.loads(line)
            if report.get('type') == want:
                return report
        raise Failure(f'router did not report {want}')

    def request(self, line):
        self.process.stdin.write(line + '\n')
        self.process.stdin.flush()

    def query(self, line, expect):
        self.request(line)
        return self.readline(expect, 10)

    def map(self, keysym):
        return self.query(f'map {keysym}', 'map')

    def key(self, keysym, down):
        return self.query(f'key {keysym} {1 if down else 0}', 'key')

    def btn(self, button, down):
        self.request(f'btn {button} {1 if down else 0}')

    def motion(self, op, x, y):
        self.request(f'{op} {x} {y}')

    def pos(self):
        return self.query('pos', 'pos')

    def close(self):
        try:
            self.process.stdin.close()
            self.readline('route_exit', 5)
        finally:
            if self.process.poll() is None:
                self.process.kill()
        check(self.process.wait(timeout=5) == 0, 'router clean exit')
        check(not self.process.stderr.read(), 'router stderr')


def wait_window(x11, xid, timeout=15):
    """Viewable plus geometry-stable: metacity repositions a fresh managed
    window after the first MapNotify; binding (XISetFocus) before that settle
    leaves device focus on a window the server later moves."""
    deadline = time.monotonic() + timeout
    stable, stable_hits = None, 0
    while time.monotonic() < deadline:
        for window in x11.top_windows(None):
            if window['xid'] != xid:
                continue
            shape = (window['x'], window['y'], window['width'], window['height'])
            if shape == stable:
                stable_hits += 1
                if stable_hits >= 3:
                    return window
            else:
                stable, stable_hits = shape, 0
        time.sleep(0.1)
    raise Failure(f'window {xid} never became viewable and geometry-stable')


def find_window_by_pid(x11, pid, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for window in x11.top_windows(None):
            try:
                if x11.xres_pid(window['xid']) == pid:
                    return window
            except Failure:
                continue
        time.sleep(0.2)
    raise Failure(f'no viewable window owned by pid {pid}')


def find_app_window(x11, session, wm_class, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        tree = session.tree()
        for window in x11.top_windows(wm_class):
            try:
                if x11.xres_pid(window['xid']) in tree:
                    return window
            except Failure:
                continue
        time.sleep(0.2)
    raise Failure(f'no {wm_class} window owned by the launched application')


def tap(router, keysym, gap=0.06):
    router.key(keysym, True)
    time.sleep(gap)
    router.key(keysym, False)


def type_text(router, text):
    """Type through the owned master; uppercase letters use a shift chord."""
    for char in text:
        keysym = ord(char)
        if router.map(keysym)['keycode']:
            tap(router, keysym)
        elif char.isupper():
            router.key(XK_SHIFT_L, True)
            tap(router, ord(char.lower()))
            router.key(XK_SHIFT_L, False)
        else:
            raise Failure(f'character {char!r} not directly reachable in the active group')


def zenity_setup(route_binary, environment):
    """Launch zenity, bind the router, click once to focus the entry."""
    x11 = X11(environment)
    process = subprocess.Popen(
        ['zenity', '--entry', '--title', 'oink-probe', '--text', 'type', '--width', '400'],
        env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        start_new_session=True)
    window = find_window_by_pid(x11, process.pid)
    router = Router(route_binary, window['xid'], environment)
    router.motion('abs', window['x'] + window['width'] // 2, window['y'] + window['height'] // 2)
    time.sleep(0.3)
    router.btn(1, True)
    time.sleep(0.15)
    router.btn(1, False)
    time.sleep(0.6)
    return x11, process, router


def zenity_finish(x11, process, router):
    if process.poll() is None:
        process.kill()
        process.wait(timeout=5)
    router.close()
    x11.close()


def zenity_typed_row(route_binary, environment):
    x11, process, router = zenity_setup(route_binary, environment)
    try:
        text = 'Ab3-z'
        type_text(router, text)
        tap(router, XK_RETURN)
        out, err = process.communicate(timeout=10)
        check(process.returncode == 0, f'zenity exit {process.returncode}: {err}')
        check(out.strip() == text, f'GTK native text mismatch: {out.strip()!r} != {text!r}')
        row('zenity-entry-text', 'PASS', typed=text, gtk_native_stdout='exact match')
    finally:
        zenity_finish(x11, process, router)


def zenity_repeat_row(route_binary, environment):
    x11, process, router = zenity_setup(route_binary, environment)
    try:
        router.key(XK_A, True)
        time.sleep(0.9)
        router.key(XK_A, False)
        tap(router, XK_RETURN)
        out, err = process.communicate(timeout=10)
        check(process.returncode == 0, f'zenity exit {process.returncode}: {err}')
        check(len(out.strip()) >= 2 and set(out.strip()) == {'a'},
              f'server auto-repeat must deliver >=2 presses: {out.strip()!r}')
        row('zenity-server-repeat', 'PASS', presses=len(out.strip()))
    finally:
        zenity_finish(x11, process, router)


def normal_input_row(route_binary, environment):
    x11, process, router = zenity_setup(route_binary, environment)
    try:
        # Default-master input (python's own XTEST connection) must still reach
        # the app alongside the owned master: no swallowing.
        x_code = router.map(ord('x'))['keycode']
        x11.key(x_code, True)
        time.sleep(0.1)
        x11.key(x_code, False)
        type_text(router, 'y')
        tap(router, XK_RETURN)
        out, err = process.communicate(timeout=10)
        check(process.returncode == 0, f'zenity exit {process.returncode}: {err}')
        check(out.strip() == 'xy', f'normal input comparison: {out.strip()!r} != "xy"')
        row('normal-input-comparison', 'PASS', typed='xy',
            default_master='x', owned_master='y', no_swallow='PASS')
    finally:
        zenity_finish(x11, process, router)


def browser_row(kind, wm_class, environment, route_binary):
    server = PageServer(Path(__file__).with_name('linux_input_app_page.html'))
    app = harness.launch(kind, server.url, environment)
    x11 = X11(environment)
    router = None
    try:
        server.wait_report('probe-ready', 60)
        window = find_app_window(x11, app, wm_class)
        router = Router(route_binary, window['xid'], environment)
        router.motion('abs', window['x'] + window['width'] // 2,
                      window['y'] + window['height'] // 2)
        router.key(XK_A, True)
        router.key(XK_A, False)
        down = server.wait_report('probe-key', 10)
        check(down['trusted'] is True and down['key'] == 'a', f'trusted key event: {down}')
        server.wait_report('probe-keyup', 10)
        router.btn(4, True)
        time.sleep(0.12)
        router.btn(4, False)
        wheel = server.wait_report('probe-wheel', 10)
        check(wheel['trusted'] is True and wheel['delta_y'] != 0, f'trusted wheel: {wheel}')
        row(f'{kind}-trusted-input', 'PASS', key=down['key'], wheel_delta_y=wheel['delta_y'])
    finally:
        if router:
            router.close()
        x11.close()
        app.close()
        server.close()


def screenshot(environment, name):
    path = Path(environment['OINK_NATIVE_TEST_ROOT']) / f'{name}.xwd'
    result = subprocess.run(['xwd', '-silent', '-root'], env=environment,
                            capture_output=True, timeout=10)
    check(result.returncode == 0 and result.stdout, f'xwd screenshot: {result.stderr}')
    path.write_bytes(result.stdout)
    return path.read_bytes()


def nemo_scroll_row(environment, route_binary):
    root = Path(environment['OINK_NATIVE_TEST_ROOT'])
    folder = root / 'nemo-scroll'
    folder.mkdir(mode=0o700)
    for index in range(300):
        (folder / f'entry_{index:04d}.txt').write_text(f'{index}\n')
    app = harness.launch('nemo', str(folder), environment)
    x11 = X11(environment)
    router = None
    try:
        window = find_app_window(x11, app, 'Nemo')
        router = Router(route_binary, window['xid'], environment)
        router.motion('abs', window['x'] + window['width'] // 2,
                      window['y'] + window['height'] // 2)
        before = screenshot(environment, 'nemo-before')
        for _ in range(3):
            router.btn(5, True)
            time.sleep(0.12)
            router.btn(5, False)
            time.sleep(0.3)
        scrolled = screenshot(environment, 'nemo-scrolled')
        for _ in range(3):
            router.btn(4, True)
            time.sleep(0.12)
            router.btn(4, False)
            time.sleep(0.3)
        back = screenshot(environment, 'nemo-back')
        check(before != scrolled, 'scroll down must change the nemo view pixels')
        check(scrolled != back, 'scroll up must change the nemo view pixels')
        check(app.process.poll() is None, 'nemo must stay alive throughout')
        row('nemo-navigation-scroll', 'PASS', direction='down+up',
            pixel_evidence='xwd before/scrolled/back all distinct')
    finally:
        if router:
            router.close()
        x11.close()
        app.close()


def nemo_key_row(environment, route_binary):
    app = harness.launch('nemo', str(Path(environment['OINK_NATIVE_TEST_ROOT'])), environment)
    x11 = X11(environment)
    router = None
    try:
        window = find_app_window(x11, app, 'Nemo')
        router = Router(route_binary, window['xid'], environment)
        router.motion('abs', window['x'] + window['width'] // 2,
                      window['y'] + window['height'] // 2)
        before = screenshot(environment, 'nemo-key-before')
        router.key(XK_CONTROL_L, True)
        time.sleep(0.12)
        router.key(XK_CONTROL_L, False)
        time.sleep(0.8)
        bar = screenshot(environment, 'nemo-key-bar')
        type_text(router, '/tmp')
        time.sleep(0.4)
        typed = screenshot(environment, 'nemo-key-typed')
        tap(router, XK_RETURN)
        time.sleep(1.5)
        title = subprocess.run(['xdotool', 'getwindowname', str(window['xid'])],
                               env=environment, capture_output=True, text=True, timeout=5)
        check(before != bar, 'Ctrl+L must open the nemo location bar (pixels)')
        check(bar != typed, 'typed path must appear in the location bar (pixels)')
        check(title.stdout.strip() == 'tmp', f'nemo must navigate to /tmp: {title.stdout!r}')
        check(app.process.poll() is None, 'nemo must stay alive')
        row('nemo-key-navigation', 'PASS', keys='ctrl+l, type /tmp, return',
            window_title=title.stdout.strip())
    finally:
        if router:
            router.close()
        x11.close()
        app.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--route', type=Path, required=True)
    parser.add_argument('--qt-fixture', type=Path, required=True)
    args = parser.parse_args(argv)
    for binary in (args.route, args.qt_fixture):
        check(binary.is_absolute() and binary.is_file() and os.access(binary, os.X_OK),
              f'{binary} must be an absolute compiled executable')
    environment = isolated_environment(dict(os.environ))
    import linux_input_toolkit_qt_row as qt_row
    rows = [
        ('zenity-entry-text', lambda: zenity_typed_row(args.route, environment)),
        ('zenity-server-repeat', lambda: zenity_repeat_row(args.route, environment)),
        ('qt-native-callback-delivery', lambda: qt_row.qt_delivery_row(
            args.qt_fixture, args.route, environment)),
        ('normal-input-comparison', lambda: normal_input_row(args.route, environment)),
        ('firefox-trusted-input', lambda: browser_row(
            'firefox', 'firefox', environment, args.route)),
        ('chrome-trusted-input', lambda: browser_row(
            'chrome', 'Google-chrome', environment, args.route)),
        ('nemo-navigation-scroll', lambda: nemo_scroll_row(environment, args.route)),
        ('nemo-key-navigation', lambda: nemo_key_row(environment, args.route)),
    ]
    failures = 0
    for name, run in rows:
        try:
            run()
        except (Failure, OSError, ValueError, subprocess.SubprocessError) as error:
            failures += 1
            row(name, 'FAIL', reason=str(error))
    result = 'INPUT_TOOLKIT_DONE' if not failures else 'INPUT_TOOLKIT_FAILED'
    print(json.dumps({'result': result, 'rows': len(rows), 'failures': failures},
                     sort_keys=True), flush=True)
    return 0 if not failures else 1


if __name__ == '__main__':
    sys.exit(main())
