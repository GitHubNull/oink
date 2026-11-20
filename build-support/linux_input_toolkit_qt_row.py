#!/usr/bin/env python3
"""Qt native-callback delivery row: keys (modifiers/AltGr/media/repeat),
exact relative motion, and four-way wheel through the owned master."""
import json
import os
import signal
import subprocess
import threading
import time

from linux_app_x11 import X11
from linux_input_toolkit_scenarios import (
    SCROLL_BUTTONS, XK_A, XK_AUDIO_MUTE, XK_B, XK_LEVEL3, XK_SHIFT_L, Failure,
    Router, check, row, wait_window,
)


class QtFixture:
    """Line-json Qt widget fixture with a background stdout reader."""

    def __init__(self, binary, environment):
        self.process = subprocess.Popen([str(binary)], env=environment, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, bufsize=1)
        self.lines = []
        self.lock = threading.Lock()
        threading.Thread(target=self._reader, daemon=True).start()
        self.ready = self.wait_for(lambda r: r.get('type') == 'fixture_ready', 'fixture_ready', 20)

    def _reader(self):
        try:
            for line in self.process.stdout:
                with self.lock:
                    self.lines.append(json.loads(line))
        except (OSError, ValueError):
            pass

    def events(self, kind):
        with self.lock:
            return [line for line in self.lines if line.get('type') == kind]

    def wait_for(self, predicate, label, seconds=10):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            with self.lock:
                for line in self.lines:
                    if predicate(line):
                        return line
            if self.process.poll() is not None:
                raise Failure(f'Qt fixture exited {self.process.returncode}: '
                              f'{self.process.stderr.read()}')
            time.sleep(0.03)
        raise Failure(f'Qt fixture did not report {label}')

    def close(self):
        self.process.send_signal(signal.SIGTERM)
        check(self.process.wait(timeout=10) == 0, f'Qt fixture exit {self.process.returncode}')
        check(not self.process.stderr.read(), 'Qt fixture stderr')


def qt_delivery_row(fixture_binary, route_binary, environment):
    x11 = X11(environment)
    fixture = QtFixture(fixture_binary, environment)
    router = None
    try:
        window = wait_window(x11, fixture.ready['xid'])
        router = Router(route_binary, window['xid'], environment)
        center = (window['x'] + window['width'] // 2, window['y'] + window['height'] // 2)
        router.motion('abs', *center)
        before = router.pos()
        router.motion('rel', 13, 7)
        after = router.pos()
        check(abs(after['root_x'] - before['root_x'] - 13) < 0.01
              and abs(after['root_y'] - before['root_y'] - 7) < 0.01,
              f'relative motion delta: {before} -> {after}')

        code = router.map(XK_A)['keycode']
        check(code, 'XK_a must be reachable for the Qt row')
        router.key(XK_A, True)
        router.key(XK_A, False)
        fixture.wait_for(lambda r: r.get('type') == 'key' and r.get('press')
                         and r.get('key') == 0x41 and r.get('native_scan') == code,
                         'Qt key delivery', 6)
        router.key(XK_SHIFT_L, True)
        router.key(XK_B, True)
        shifted = fixture.wait_for(lambda r: r.get('type') == 'key' and r.get('press')
                                   and r.get('key') == 0x42 and r.get('mods') & 0x02000000,
                                   'Qt shifted key delivery', 6)
        router.key(XK_B, False)
        router.key(XK_SHIFT_L, False)
        facts = {'shifted_key': shifted['key']}

        if router.map(XK_LEVEL3)['keycode']:
            router.key(XK_LEVEL3, True)
            alt = router.key(XK_A, True)
            check(alt['mods'] != 0, f'AltGr/Level3 not active: {alt}')
            fixture.wait_for(lambda r: r.get('type') == 'key' and r.get('press')
                             and r.get('native_scan') == code, 'AltGr chord delivery', 6)
            router.key(XK_A, False)
            router.key(XK_LEVEL3, False)
            facts['altgr'] = 'PASS'
        else:
            facts['altgr'] = 'NOT_REACHABLE_ACTIVE_GROUP'
        if router.map(XK_AUDIO_MUTE)['keycode']:
            media_code = router.map(XK_AUDIO_MUTE)['keycode']
            router.key(XK_AUDIO_MUTE, True)
            router.key(XK_AUDIO_MUTE, False)
            fixture.wait_for(lambda r: r.get('type') == 'key' and r.get('press')
                             and r.get('native_scan') == media_code, 'media key delivery', 6)
            facts['media_key'] = 'PASS'
        else:
            facts['media_key'] = 'NOT_REACHABLE_ACTIVE_GROUP'

        key_before = len(fixture.events('key'))
        router.key(XK_A, True)
        time.sleep(0.9)
        router.key(XK_A, False)
        presses, releases = [], []
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            keys = fixture.events('key')[key_before:]
            presses = [k for k in keys if k.get('press') and k.get('key') == 0x41]
            releases = [k for k in keys if not k.get('press') and k.get('key') == 0x41]
            if len(presses) >= 2 and len(fixture.events('key')[key_before:])                     and not fixture.events('key')[key_before:][-1].get('press'):
                break
            time.sleep(0.05)
        check(len(presses) >= 2, f'Qt server repeat missing: presses={len(presses)}')
        check(len(releases) == len(presses),
              f'Qt repeat pairs: presses={len(presses)} releases={len(releases)}')
        facts['server_repeat'] = (f'presses={len(presses)} releases={len(releases)} '
                                  'press_release_pairs (Qt xcb did not enable detectable '
                                  'autorepeat; zenity row shows the detectable model)')

        seen = {}
        for button, expected in SCROLL_BUTTONS.items():
            router.btn(button, True)
            time.sleep(0.12)
            router.btn(button, False)
            wanted = 'dy' if expected in ('up', 'down') else 'dx'
            sign = 1 if expected in ('up', 'left') else -1
            deadline = time.monotonic() + 4
            while expected not in seen and time.monotonic() < deadline:
                for wheel in fixture.events('wheel'):
                    if sign * wheel[wanted] > 0:
                        seen[expected] = wheel
                time.sleep(0.05)
        missing = [d for d in SCROLL_BUTTONS.values() if d not in seen]
        check(not missing, f'Qt scroll directions missing: {missing}')
        facts['scroll'] = {d: {k: v for k, v in w.items() if k in ('dx', 'dy')}
                           for d, w in seen.items()}
        row('qt-native-callback-delivery', 'PASS', **facts)
    finally:
        # Close the application before the router removes its XI master:
        # toolkit XI2 bookkeeping may still reference the routed devices.
        fixture.close()
        if router:
            router.close()
        x11.close()


