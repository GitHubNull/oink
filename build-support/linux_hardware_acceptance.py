#!/usr/bin/env python3
"""User-operated hardware acceptance entry points (F wave).

Default mode prints instructions and the state of the agreed evidence logs;
it never touches hardware. Modes that reach the user's session require the
explicit OINK_HARDWARE_USER=1 token, refuse to start inside the private
probe scope (OINK_NATIVE_TEST_ROOT set), collect only aggregate counts from
the collector's own focused window (no global listener, no key content, no
injection/grab, stop on focus loss), and the microphone bridge records only
RMS/byte counts and start/stop state — never audio samples.
"""
import argparse
import ctypes as C
import ctypes.util
import json
import os
from pathlib import Path
import sys
import time

import linux_hardware_mic as mic

LOG_DIR = Path(__file__).resolve().parent.parent / '.release-tools' / 'linux-probes'
INPUT_LOG = LOG_DIR / 'hardware-input-user.log'
MIC_LOG = LOG_DIR / 'hardware-mic-user.log'
FUSE_LOG = LOG_DIR / 'fuse-user.log'

KeyPress, KeyRelease = 2, 3
ButtonPress, ButtonRelease = 4, 5
MotionNotify = 6
FocusOut = 10
MapNotify = 19
ClientMessage = 33

# Event masks are protocol bit positions, not event type numbers.
KeyPressMask = 1 << 0
KeyReleaseMask = 1 << 1
ButtonPressMask = 1 << 2
ButtonReleaseMask = 1 << 3
PointerMotionMask = 1 << 6
StructureNotifyMask = 1 << 17

EVENT_NAMES = {KeyPress: 'key_press', KeyRelease: 'key_release',
               ButtonPress: 'button_press', ButtonRelease: 'button_release',
               MotionNotify: 'motion'}


def probe_scope_active(env):
    return 'OINK_NATIVE_TEST_ROOT' in env


def deadline_expired(started_monotonic, seconds, now=None):
    """Collector deadline on the monotonic clock only; wall clock just stamps logs."""
    if now is None:
        now = time.monotonic()
    return now - started_monotonic >= seconds


def focus_is_self(focus, window):
    """True only when the server reports the collector's own window as focus."""
    return bool(window) and focus == window


def focus_lost_is_final(held_seconds, minimum=1.0):
    """WM startup corrections bounce focus; only a held-then-lost focus is a stop."""
    return held_seconds >= minimum


XErrorHandler = C.CFUNCTYPE(C.c_int, C.c_void_p, C.c_void_p)


@XErrorHandler
def _ignore_x_error(display, event):
    """Xlib's default handler exits the process on BadMatch; focus is best-effort."""
    return 0


def require_user_session(env, enabled):
    if probe_scope_active(env):
        raise SystemExit('refused: running inside the private probe scope')
    if not enabled:
        raise SystemExit('refused: user token OINK_HARDWARE_USER=1 and explicit flag required')


class Collector:
    """Aggregate event counts for the collector's own window only."""

    def __init__(self):
        self.counts = dict.fromkeys(EVENT_NAMES.values(), 0)
        self.focus_out = 0
        self.stopped = False
        self.stop_reason = ''

    def feed(self, event_type):
        if self.stopped:
            return None
        if event_type == FocusOut:
            self.focus_out += 1
            self.stopped = True
            self.stop_reason = 'focus_lost'
            return self.snapshot()
        if event_type == ClientMessage:
            # WM_DELETE_WINDOW: a bare window gets XKillClient instead, which
            # severs the whole X connection before any stop line is written.
            self.stopped = True
            self.stop_reason = 'window_closed'
            return self.snapshot()
        name = EVENT_NAMES.get(event_type)
        if name:
            self.counts[name] += 1
        return None

    def snapshot(self):
        return {'counts': dict(self.counts), 'focus_out': self.focus_out,
                'stopped': self.stopped, 'stop_reason': self.stop_reason}


def rms_db(samples):
    """RMS level in dBFS of a 16-bit PCM chunk; -inf for silence."""
    if not samples:
        return float('-inf')
    total = 0
    for sample in samples:
        total += sample * sample
    mean = total / len(samples)
    if mean == 0:
        return float('-inf')
    return 20.0 * math.log10(math.sqrt(mean) / 32768.0)


def _x11():
    found = ctypes.util.find_library('X11')
    if not found:
        raise SystemExit('refused: libX11 runtime library missing')
    lib = C.CDLL(found)
    lib.XOpenDisplay.argtypes = [C.c_char_p]
    lib.XOpenDisplay.restype = C.c_void_p
    lib.XDefaultScreen.argtypes = [C.c_void_p]
    lib.XDefaultScreen.restype = C.c_int
    lib.XRootWindow.argtypes = [C.c_void_p, C.c_int]
    lib.XRootWindow.restype = C.c_ulong
    lib.XCreateSimpleWindow.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_int,
                                        C.c_uint, C.c_uint, C.c_uint, C.c_ulong, C.c_ulong]
    lib.XCreateSimpleWindow.restype = C.c_ulong
    lib.XSelectInput.argtypes = [C.c_void_p, C.c_ulong, C.c_long]
    lib.XMapWindow.argtypes = [C.c_void_p, C.c_ulong]
    lib.XPending.argtypes = [C.c_void_p]
    lib.XNextEvent.argtypes = [C.c_void_p, C.c_void_p]
    lib.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_bool]
    lib.XInternAtom.restype = C.c_ulong
    lib.XSetWMProtocols.argtypes = [C.c_void_p, C.c_ulong, C.c_void_p, C.c_int]
    lib.XStoreName.argtypes = [C.c_void_p, C.c_ulong, C.c_char_p]
    lib.XSetInputFocus.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_ulong]
    lib.XGetInputFocus.argtypes = [C.c_void_p, C.POINTER(C.c_ulong), C.POINTER(C.c_int)]
    lib.XSync.argtypes = [C.c_void_p, C.c_bool]
    lib.XCheckTypedWindowEvent.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_void_p]
    lib.XCheckTypedWindowEvent.restype = C.c_bool
    lib.XSetErrorHandler.argtypes = [XErrorHandler]
    lib.XSetErrorHandler.restype = C.c_void_p
    lib.XDestroyWindow.argtypes = [C.c_void_p, C.c_ulong]
    lib.XCloseDisplay.argtypes = [C.c_void_p]
    return lib


def run_input_collector(log_path, seconds, title):
    """Map one owned window, count own-window events until focus loss or timeout."""
    lib = _x11()
    display = lib.XOpenDisplay(None)
    if not display:
        raise SystemExit('refused: cannot connect DISPLAY; run from your own session')
    lib.XSetErrorHandler(_ignore_x_error)
    screen = lib.XDefaultScreen(display)
    root = lib.XRootWindow(display, screen)
    mask = KeyPressMask | KeyReleaseMask | ButtonPressMask | ButtonReleaseMask \
        | PointerMotionMask | StructureNotifyMask
    window = lib.XCreateSimpleWindow(display, root, 40, 40, 360, 160, 1, 0, 0xE0E0E0)
    lib.XSelectInput(display, window, mask)
    delete = C.c_ulong(lib.XInternAtom(display, b'WM_DELETE_WINDOW', False))
    lib.XSetWMProtocols(display, window, C.byref(delete), 1)
    lib.XStoreName(display, window, title.encode())
    lib.XMapWindow(display, window)
    event = C.create_string_buffer(192)
    mapped = False
    wait_until = time.monotonic() + 2.0
    while time.monotonic() < wait_until:
        if lib.XCheckTypedWindowEvent(display, window, MapNotify, event):
            mapped = True
            break
        time.sleep(0.01)
    focus = C.c_ulong(0)
    revert = C.c_int(0)
    for _ in range(20):
        if mapped:
            lib.XSetInputFocus(display, window, 2, 0)  # RevertToParent, CurrentTime
            lib.XSync(display, False)
        lib.XGetInputFocus(display, C.byref(focus), C.byref(revert))
        if focus_is_self(focus.value, window):
            break
        time.sleep(0.05)
    collector = Collector()
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started_epoch = time.time()
    started_monotonic = time.monotonic()
    held_focus = focus_is_self(focus.value, window)
    focus_since = started_monotonic if held_focus else 0.0
    next_focus_check = 0.0
    next_focus_try = 0.0
    next_title = started_monotonic
    next_progress = started_monotonic + 5.0
    with log_path.open('a', encoding='utf-8') as log:
        def emit(payload):
            log.write(json.dumps(payload) + '\n')
            log.flush()

        def live_title():
            totals = collector.snapshot()['counts']
            keys = totals['key_press'] + totals['key_release']
            buttons = totals['button_press'] + totals['button_release']
            state = 'yes' if held_focus else 'NO - click window'
            lib.XStoreName(display, window,
                           f'{title} | keys:{keys} buttons:{buttons} focus:{state}'.encode())

        emit({'type': 'input_start', 'xid': window, 'display': os.environ.get('DISPLAY'),
              'title': title, 'mapped': mapped,
              'focus_self': held_focus, 'started_epoch': started_epoch})
        print(json.dumps({'type': 'input_start', 'xid': window, 'mapped': mapped,
                          'focus_self': held_focus,
                          'hint': 'click the window, then type; the title bar shows live counts'}),
              flush=True)
        while not deadline_expired(started_monotonic, seconds) and not collector.stopped:
            while lib.XPending(display) > 0:
                lib.XNextEvent(display, event)
                done = collector.feed(C.c_int.from_buffer(event).value)
                if done:
                    emit({'type': 'input_stop', 'reason': done['stop_reason'],
                          **done['counts'], 'focus_out': done['focus_out']})
            now = time.monotonic()
            if now >= next_focus_check:
                lib.XGetInputFocus(display, C.byref(focus), C.byref(revert))
                now_self = focus_is_self(focus.value, window)
                if now_self and not held_focus:
                    held_focus, focus_since = True, now
                    emit({'type': 'focus', 'event': 'acquired', 'at': round(now - started_monotonic, 2)})
                elif held_focus and not now_self:
                    held = now - focus_since
                    if focus_lost_is_final(held):
                        collector.stopped = True
                        collector.stop_reason = 'focus_lost'
                        collector.focus_out += 1
                        emit({'type': 'input_stop', 'reason': 'focus_lost',
                              **collector.snapshot()['counts'],
                              'focus_out': collector.focus_out})
                    else:
                        held_focus = False
                        emit({'type': 'focus', 'event': 'bounced', 'held': round(held, 2)})
                if not held_focus and now >= next_focus_try:
                    lib.XSetInputFocus(display, window, 2, 0)
                    lib.XSync(display, False)
                    next_focus_try = now + 0.5
                next_focus_check = now + 0.2
            if now >= next_title:
                live_title()
                next_title = now + 1.0
            if now >= next_progress:
                snapshot = collector.snapshot()
                emit({'type': 'progress', 'focus_self': held_focus,
                      **snapshot['counts'], 'focus_out': snapshot['focus_out']})
                print(json.dumps({'type': 'progress', 'focus_self': held_focus,
                                  **snapshot['counts']}), flush=True)
                next_progress = now + 5.0
            time.sleep(0.01)
        if not collector.stopped:
            snapshot = collector.snapshot()
            emit({'type': 'input_stop', 'reason': 'timeout', **snapshot['counts'],
                  'focus_out': snapshot['focus_out']})
        lib.XDestroyWindow(display, window)
        lib.XCloseDisplay(display)
    return 0


def run_status():
    def read_log(path):
        if not path.is_file():
            return {'present': False}
        lines = path.read_text(encoding='utf-8', errors='replace').splitlines()
        events = []
        for line in lines[-50:]:
            try:
                events.append(json.loads(line))
            except ValueError:
                events.append({'raw': line[:120]})
        return {'present': True, 'events': len(lines), 'tail': events[-5:]}

    print(json.dumps({'type': 'hardware_status',
                      'instructions': {
                          'input': f'OINK_HARDWARE_USER=1 python3 {Path(__file__).name} '
                                   f'input-collector  # focus the window, press/type, then switch away',
                          'microphone': f'OINK_HARDWARE_USER=1 python3 {Path(__file__).name} '
                                        f'mic-bridge --enable-mic --device <pactl source name>',
                          'fuse': 'run the handover §7.5 command in a normal terminal'},
                      'logs': {'input': read_log(INPUT_LOG), 'mic': read_log(MIC_LOG),
                               'fuse': read_log(FUSE_LOG)}},
                     indent=1))
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', nargs='?', default='status',
                        choices=('status', 'input-collector', 'mic-bridge', 'mic-consumer'))
    parser.add_argument('--log', type=Path, default=None)
    parser.add_argument('--seconds', type=float, default=60)
    parser.add_argument('--device', default='')
    parser.add_argument('--rate', type=int, default=48000)
    parser.add_argument('--enable-mic', action='store_true')
    parser.add_argument('--title', default='oink hardware input collector')
    args = parser.parse_args(argv)
    env = os.environ
    if args.mode == 'status':
        return run_status()
    if args.mode == 'mic-consumer':
        return mic.run_mic_consumer(args.seconds)
    token = env.get('OINK_HARDWARE_USER') == '1'
    if args.mode == 'input-collector':
        require_user_session(env, token)
        return run_input_collector(args.log or INPUT_LOG, args.seconds, args.title)
    require_user_session(env, token and args.enable_mic and bool(args.device))
    return mic.run_mic_bridge(args.log or MIC_LOG, args.device, args.seconds, args.rate)


if __name__ == '__main__':
    sys.exit(main())
