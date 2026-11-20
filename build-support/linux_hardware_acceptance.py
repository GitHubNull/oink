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
import math
import os
from pathlib import Path
import select
import subprocess as S
import sys
import time

LOG_DIR = Path(__file__).resolve().parent.parent / '.release-tools' / 'linux-probes'
INPUT_LOG = LOG_DIR / 'hardware-input-user.log'
MIC_LOG = LOG_DIR / 'hardware-mic-user.log'
FUSE_LOG = LOG_DIR / 'fuse-user.log'

KeyPress, KeyRelease = 2, 3
ButtonPress, ButtonRelease = 4, 5
MotionNotify = 6
FocusOut = 10
ClientMessage = 33

EVENT_NAMES = {KeyPress: 'key_press', KeyRelease: 'key_release',
               ButtonPress: 'button_press', ButtonRelease: 'button_release',
               MotionNotify: 'motion'}


def probe_scope_active(env):
    return 'OINK_NATIVE_TEST_ROOT' in env


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
    lib.XDestroyWindow.argtypes = [C.c_void_p, C.c_ulong]
    lib.XCloseDisplay.argtypes = [C.c_void_p]
    return lib


def run_input_collector(log_path, seconds, title):
    """Map one owned window, count own-window events until focus loss or timeout."""
    lib = _x11()
    display = lib.XOpenDisplay(None)
    if not display:
        raise SystemExit('refused: cannot connect DISPLAY; run from your own session')
    screen = lib.XDefaultScreen(display)
    root = lib.XRootWindow(display, screen)
    mask = (1 << KeyPress) | (1 << KeyRelease) | (1 << ButtonPress) | (1 << ButtonRelease) \
        | (1 << MotionNotify) | (1 << FocusOut)
    window = lib.XCreateSimpleWindow(display, root, 40, 40, 360, 160, 1, 0, 0xE0E0E0)
    lib.XSelectInput(display, window, mask)
    delete = C.c_ulong(lib.XInternAtom(display, b'WM_DELETE_WINDOW', False))
    lib.XSetWMProtocols(display, window, C.byref(delete), 1)
    lib.XMapWindow(display, window)
    collector = Collector()
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started = time.time()
    with log_path.open('a', encoding='utf-8') as log:
        log.write(json.dumps({'type': 'input_start', 'xid': window, 'display': os.environ.get('DISPLAY'),
                              'title': title, 'started_epoch': started}) + '\n')
        event = C.create_string_buffer(192)
        while time.monotonic() - started < seconds and not collector.stopped:
            while lib.XPending(display) > 0:
                lib.XNextEvent(display, event)
                done = collector.feed(C.c_int.from_buffer(event).value)
                if done:
                    log.write(json.dumps({'type': 'input_stop', 'reason': done['stop_reason'],
                                          **done['counts'], 'focus_out': done['focus_out']}) + '\n')
            time.sleep(0.01)
        if not collector.stopped:
            snap = collector.snapshot()
            log.write(json.dumps({'type': 'input_stop', 'reason': 'timeout', **snap['counts'],
                                  'focus_out': snap['focus_out']}) + '\n')
        lib.XDestroyWindow(display, window)
        lib.XCloseDisplay(display)
    return 0


def host_capture_env(env):
    """parec must reach the host device, not the private session socket."""
    clean = dict(env)
    for key in ('PULSE_SERVER', 'PIPEWIRE_RUNTIME_DIR', 'PIPEWIRE_REMOTE'):
        clean.pop(key, None)
    return clean


def core_env(pulse_server):
    """Map a private PULSE_SERVER unix path onto the core oink-test socket env.

    The private audio stack has no session manager: pulse streams never link
    (foreign or in-session alike), so delivery goes through the core pipewire
    socket, which module-access marks unrestricted for this socket name.
    """
    runtime = pulse_server.removeprefix('unix:').removesuffix('/pulse-native')
    if not runtime.startswith('/tmp/oink-native-'):
        raise SystemExit('refused: not a private runner pulse socket')
    env = dict(os.environ)
    env['PIPEWIRE_RUNTIME_DIR'] = runtime
    env['PIPEWIRE_REMOTE'] = 'oink-test'
    env['PIPEWIRE_CONFIG_NAME'] = 'client.conf'
    return env


def _pw_run(env, args):
    return S.run(args, capture_output=True, text=True, env=env, timeout=30,
                 stdin=S.DEVNULL)


def _stderr_tail(process, limit=200):
    """Captured stderr tail; empty when the stream was routed to devnull."""
    if process.stderr is None:
        return ''
    return process.stderr.read().decode(errors='replace')[-limit:]


def _stop_process(process):
    """Stop our own child; an operator-initiated stop is an orderly exit."""
    if process.poll() is not None:
        return process.returncode
    process.terminate()
    try:
        process.wait(timeout=5)
    except S.TimeoutExpired:
        process.kill()
        process.wait()
    return 0


def _pw_objects(env, kind):
    out = _pw_run(env, ['pw-dump'])
    if out.returncode != 0 or not out.stdout.strip():
        raise SystemExit('refused: private session core socket unreachable')
    return [obj for obj in json.loads(out.stdout) if obj['type'].endswith(':' + kind)]


def _node_id(env, name):
    matches = [obj for obj in _pw_objects(env, 'Node')
               if obj.get('info', {}).get('props', {}).get('node.name') == name]
    return matches[0]['id'] if matches else 0


def _ports(env, node, direction):
    return [obj for obj in _pw_objects(env, 'Port')
            if str(obj['info']['props'].get('node.id')) == str(node)
            and obj['info']['props'].get('port.direction') == direction]


def link_nodes(env, first, second, first_direction, second_direction):
    """PortConfig both ends, then pw-link matching channels (suite recipe)."""
    for node, direction in ((first, first_direction), (second, second_direction)):
        fmt = ('{ direction = %s mode = dsp monitor = true format = { mediaType = audio '
               'mediaSubtype = raw format = F32P rate = 48000 channels = 1 '
               'position = [ MONO ] } }' % direction)
        _pw_run(env, ['pw-cli', 'set-param', str(node), 'PortConfig', fmt])
    time.sleep(0.5)
    if first_direction == 'Output':
        outs, ins = _ports(env, first, 'out'), _ports(env, second, 'in')
    else:
        outs, ins = _ports(env, second, 'out'), _ports(env, first, 'in')
    linked = 0
    for out_port in outs:
        for in_port in ins:
            if out_port['info']['props'].get('audio.channel') == \
                    in_port['info']['props'].get('audio.channel'):
                result = _pw_run(env, ['pw-link', str(out_port['id']), str(in_port['id'])])
                linked += result.returncode == 0
    return linked


def run_mic_bridge(log_path, device, seconds, rate):
    """Forward the user-named device PCM into the private session; RMS counts only."""
    pulse_server = os.environ.get('PULSE_SERVER', '')
    if not pulse_server.startswith('unix:/tmp/oink-native-'):
        raise SystemExit('refused: PULSE_SERVER is not a private runner socket')
    env = core_env(pulse_server)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    capture = S.Popen(['parec', f'--device={device}', f'--rate={rate}', '--channels=1',
                       '--format=s16le'], stdout=S.PIPE, stderr=S.PIPE,
                      env=host_capture_env(os.environ))
    playback = S.Popen(['pw-cat', '-p', '--format=s16', f'--rate={rate}',
                        '--channels=1', '--latency=40ms',
                        '--properties={ node.name = oink-mic-bridge }', '-'],
                       stdin=S.PIPE, stderr=S.DEVNULL, env=env)
    started = time.monotonic()
    started_epoch = time.time()
    failures = 0
    with log_path.open('a', encoding='utf-8') as log:
        log.write(json.dumps({'type': 'mic_start', 'device': device, 'rate': rate,
                              'pulse_server': pulse_server,
                              'started_epoch': started_epoch}) + '\n')
        time.sleep(2)
        bridge_node = _node_id(env, 'oink-mic-bridge')
        sink_node = _node_id(env, 'oink_test')
        linked = link_nodes(env, bridge_node, sink_node, 'Output', 'Input') \
            if bridge_node and sink_node else 0
        log.write(json.dumps({'type': 'mic_route', 'bridge_node': bridge_node,
                              'sink_node': sink_node, 'links': linked}) + '\n')
        failures = not (bridge_node and sink_node and linked)
        chunk = rate // 2 * 2
        while not failures and time.monotonic() - started < seconds:
            data = capture.stdout.read(chunk)
            if not data:
                break
            values = C.cast(C.c_char_p(data), C.POINTER(C.c_int16))
            rms = rms_db([values[i] for i in range(len(data) // 2)])
            log.write(json.dumps({'type': 'mic_rms', 'rms_db': round(rms, 1),
                                  'bytes': len(data)}) + '\n')
            try:
                playback.stdin.write(data)
                playback.stdin.flush()
            except (BrokenPipeError, ValueError):
                failures = 1
                break
        capture_exit = _stop_process(capture)
        playback_exit = _stop_process(playback)
        capture_err = _stderr_tail(capture)
        playback_err = _stderr_tail(playback)
        failures = failures or capture_exit not in (0,) or playback_exit not in (0,)
        log.write(json.dumps({'type': 'mic_stop',
                              'reason': 'timeout' if not failures else 'device_error',
                              'capture_exit': capture_exit,
                              'playback_exit': playback_exit,
                              'error_tail': (capture_err + ' | ' + playback_err)[-400:]}) + '\n')
    return failures


def run_mic_consumer(seconds):
    """In-session consumer: pw-cat record node linked to the user-run bridge node.

    Agent-side verification counterpart of the user-run mic bridge; only
    starts inside the private probe scope and never touches host hardware.
    The private stack has no session manager, so the link is explicit
    (PortConfig + pw-link), the same recipe the audio suites use. The bridge
    may start at any time, so its node is polled for inside the read loop.
    """
    if not probe_scope_active(os.environ):
        raise SystemExit('refused: consumer only runs inside the private probe scope')
    env = dict(os.environ)
    env['PIPEWIRE_CONFIG_NAME'] = 'client.conf'
    recorder = S.Popen(['pw-cat', '-r', '--format=s16', '--rate=48000', '--channels=1',
                        '--latency=40ms', '--properties={ node.name = oink-mic-consumer }',
                        '-'], stdout=S.PIPE, stderr=S.DEVNULL, env=env)
    started = time.monotonic()
    print(json.dumps({'type': 'consumer_start',
                      'started_epoch': time.time()}), flush=True)
    linked = 0
    bridge = 0
    bridge_seen = 0.0
    consumer = 0
    while time.monotonic() - started < seconds:
        if not linked:
            consumer = consumer or _node_id(env, 'oink-mic-consumer')
            bridge = _node_id(env, 'oink-mic-bridge')
            if bridge and not bridge_seen:
                bridge_seen = time.monotonic()
            # grace period: let the bridge own its node's PortConfig first
            if bridge and consumer and time.monotonic() - bridge_seen > 3.0:
                linked = link_nodes(env, consumer, bridge, 'Input', 'Output')
                print(json.dumps({'type': 'consumer_route', 'bridge_node': bridge,
                                  'consumer_node': consumer, 'links': linked}), flush=True)
        ready, _, _ = select.select([recorder.stdout], [], [], 0.5)
        if not ready:
            continue
        data = os.read(recorder.stdout.fileno(), 48000)
        if not data:
            break
        values = C.cast(C.c_char_p(data), C.POINTER(C.c_int16))
        samples = [values[i] for i in range(len(data) // 2)]
        mean = sum(v * v for v in samples) / len(samples)
        level = float('-inf') if mean == 0 else round(
            20.0 * math.log10(math.sqrt(mean) / 32768.0), 1)
        print(json.dumps({'type': 'consumer_rms', 'rms_db': level,
                          'samples': len(samples),
                          'nonzero': sum(1 for v in samples if v)}), flush=True)
    print(json.dumps({'type': 'consumer_stop', 'exit': _stop_process(recorder),
                      'linked': linked}), flush=True)
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
        return run_mic_consumer(args.seconds)
    token = env.get('OINK_HARDWARE_USER') == '1'
    if args.mode == 'input-collector':
        require_user_session(env, token)
        return run_input_collector(args.log or INPUT_LOG, args.seconds, args.title)
    require_user_session(env, token and args.enable_mic and bool(args.device))
    return run_mic_bridge(args.log or MIC_LOG, args.device, args.seconds, args.rate)


if __name__ == '__main__':
    sys.exit(main())
