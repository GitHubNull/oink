#!/usr/bin/env python3
"""Synthetic-only audio mechanism probe; run exclusively via linux-native-test.py."""
import array
import ctypes as C
import json
import math
import os
from pathlib import Path
import select
import shutil
import stat
import subprocess as S
import sys
import threading
import time


def isolation_guard():
    root = Path(os.environ.get('OINK_NATIVE_TEST_ROOT', '/invalid'))
    display = os.environ.get('DISPLAY', '')
    parent = Path(f'/proc/{os.getppid()}')
    host_display = next((item.split(b'=', 1)[1] for item in
                         (parent / 'environ').read_bytes().split(b'\0')
                         if item.startswith(b'DISPLAY=')), b'')
    argv = (parent / 'cmdline').read_bytes().split(b'\0')
    runner = str(Path(__file__).resolve().with_name('linux-native-test.py')).encode()
    valid = (root.is_absolute() and root.parent == Path('/tmp')
             and root.name.startswith('oink-native-') and root.resolve() == root
             and root.stat().st_uid == os.getuid() and not root.stat().st_mode & 0o077
             and runner in argv and b'--inside-scope' in argv
             and display.startswith(':') and display[1:].isdecimal()
             and bool(host_display)
             and display != host_display.decode().split('.')[0]
             and os.environ.get('XAUTHORITY') == str(root / 'Xauthority')
             and (root / 'Xauthority').stat().st_size > 0
             and os.environ.get('PULSE_SERVER') == f'unix:{root}/runtime/pulse-native'
             and os.environ.get('PIPEWIRE_RUNTIME_DIR') == str(root / 'runtime')
             and os.environ.get('XDG_RUNTIME_DIR') == str(root / 'runtime')
             and os.environ.get('PIPEWIRE_REMOTE') == 'oink-test'
             and stat.S_ISSOCK((root / 'runtime/pulse-native').stat().st_mode))
    if not valid:
        raise RuntimeError('isolation_refused')


def emit(stage, **values):
    print(stage, ' '.join(f'{key}={value}' for key, value in values.items()), flush=True)


failures = 0
processes = []
modules = []


def check(stage, condition, **values):
    global failures
    failures += not condition
    emit(stage, status='pass' if condition else 'fail', **values)
    return condition


def command(*args):
    result = S.run(args, stdin=S.DEVNULL, capture_output=True, timeout=5)
    if result.returncode:
        emit('command', tool=args[0], status='failed', code=result.returncode)
        raise RuntimeError('command_failed')
    return result.stdout


def pulse(kind):
    return json.loads(command('pactl', '-f', 'json', 'list', kind))


def graph():
    return json.loads(command('pw-dump'))


def props(obj):
    return obj.get('info', {}).get('props', {})


def nodes(name):
    return [obj for obj in graph() if obj['type'].endswith(':Node')
            and props(obj).get('node.name') == name]


def wait(predicate, seconds=4, pump=None):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if pump:
            pump()
        value = predicate()
        if value:
            return value
        select.select([], [], [], 0.025)
    return None


def spawn(*args, **kwargs):
    kwargs.setdefault('stderr', S.DEVNULL)
    process = S.Popen(args, **kwargs)
    processes.append(process)
    return process


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=2)
        except S.TimeoutExpired:
            process.kill()
            process.wait(timeout=2)
    for pipe in (process.stdout, process.stdin):
        if pipe:
            try:
                pipe.close()
            except BrokenPipeError:
                pass


class Meter:
    def __init__(self):
        self.samples = self.squares = self.nonzero = self.holes = 0
        self.pending = b''

    def feed(self, data):
        data = self.pending + data
        self.pending = data[len(data) // 2 * 2:]
        samples = array.array('h', data[:len(data) // 2 * 2])
        if sys.byteorder != 'little':
            samples.byteswap()
        self.samples += len(samples)
        self.squares += sum(value * value for value in samples)
        self.nonzero += sum(value != 0 for value in samples)

    def values(self):
        return dict(samples=self.samples, frames=self.samples // 2,
                    rms=round(math.sqrt(self.squares / max(1, self.samples)), 3),
                    nonzero=self.nonzero, holes=self.holes)


def capture(process, duration=0.7):
    meter = Meter()
    end = time.monotonic() + duration
    while time.monotonic() < end:
        if select.select([process.stdout], [], [], max(0, min(0.02, end - time.monotonic())))[0]:
            data = os.read(process.stdout.fileno(), 65536)
            if not data:
                break
            meter.feed(data)
    return meter


def record(source, name):
    return spawn('parec', '--raw', '--format=s16le', '--rate=48000', '--channels=2',
                 '--latency-msec=40', f'--device={source}', f'--stream-name={name}',
                 stdin=S.DEVNULL, stdout=S.PIPE)


def outputs(source):
    return [row for row in pulse('source-outputs') if row['source'] == source]


def snapshot(stage, source):
    rows = outputs(source)
    sources = [row for row in pulse('sources') if row['index'] == source]
    active = sum(not row['corked'] for row in rows)
    emit(stage, outputs=len(rows), active=active,
         corked=sum(bool(row['corked']) for row in rows),
         state=sources[0]['state'] if sources else 'absent')
    return len(rows), active


class PulseRecorder:
    """Real libpulse record stream, polled on one thread; explicit cork ACKs."""
    def __init__(self, source):
        self.lib = C.CDLL('libpulse.so.0')
        self.loop = self.context = self.stream = None
        V, I, U = C.c_void_p, C.c_int, C.c_uint
        signatures = {
            'mainloop_new': (V, []), 'mainloop_get_api': (V, [V]),
            'mainloop_iterate': (I, [V, I, V]), 'mainloop_free': (None, [V]),
            'context_new': (V, [V, C.c_char_p]),
            'context_connect': (I, [V, C.c_char_p, I, V]),
            'context_get_state': (I, [V]), 'context_disconnect': (None, [V]),
            'context_unref': (None, [V]), 'stream_new': (V, [V, C.c_char_p, V, V]),
            'stream_connect_record': (I, [V, C.c_char_p, V, I]),
            'stream_get_state': (I, [V]), 'stream_get_index': (U, [V]),
            'stream_readable_size': (C.c_size_t, [V]),
            'stream_peek': (I, [V, C.POINTER(V), C.POINTER(C.c_size_t)]),
            'stream_drop': (I, [V]), 'stream_cork': (V, [V, I, V, V]),
            'operation_get_state': (I, [V]), 'operation_unref': (None, [V]),
            'stream_disconnect': (I, [V]), 'stream_unref': (None, [V]),
        }
        for name, (result, args) in signatures.items():
            function = getattr(self.lib, 'pa_' + name)
            function.restype, function.argtypes = result, args
            setattr(self, name, function)
        self.loop = self.mainloop_new()
        self.context = self.context_new(self.mainloop_get_api(self.loop), b'oink-probe-cork')
        if self.context_connect(self.context, os.environ['PULSE_SERVER'].encode(), 1, None) < 0:
            raise RuntimeError('pulse_connect_failed')
        if not wait(lambda: self.context_get_state(self.context) == 4, pump=self.pump):
            raise RuntimeError('pulse_context_timeout')
        class SampleSpec(C.Structure):
            _fields_ = [('format', I), ('rate', U), ('channels', C.c_ubyte)]
        class BufferAttr(C.Structure):
            _fields_ = [(name, U) for name in ('maxlength', 'tlength', 'prebuf', 'minreq', 'fragsize')]
        self.stream = self.stream_new(self.context, b'oink-probe-cork',
                                      C.byref(SampleSpec(3, 48000, 2)), None)
        attr = BufferAttr(0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 7680)
        if self.stream_connect_record(self.stream, source.encode(), C.byref(attr), 1) < 0:
            raise RuntimeError('pulse_record_failed')
        route('oink_probe_source', 'oink-probe-cork', pump=self.pump)
        if not wait(lambda: self.stream_get_state(self.stream) == 2, pump=self.pump):
            raise RuntimeError('pulse_stream_timeout')

    def pump(self):
        self.mainloop_iterate(self.loop, 0, None)

    def measure(self, duration=0.7):
        meter = Meter()
        end = time.monotonic() + duration
        while time.monotonic() < end:
            self.pump()
            size = self.stream_readable_size(self.stream)
            if size == C.c_size_t(-1).value:
                raise RuntimeError('pulse_read_failed')
            if size:
                data, length = C.c_void_p(), C.c_size_t()
                if self.stream_peek(self.stream, C.byref(data), C.byref(length)) < 0:
                    raise RuntimeError('pulse_peek_failed')
                if data.value:
                    meter.feed(C.string_at(data, length.value))
                else:
                    meter.holes += length.value // 2
                self.stream_drop(self.stream)
            else:
                select.select([], [], [], 0.005)
        return meter

    def cork(self, value):
        ack = []
        callback = C.CFUNCTYPE(None, C.c_void_p, C.c_int, C.c_void_p)(
            lambda _stream, success, _data: ack.append(success))
        operation = self.stream_cork(self.stream, value, callback, None)
        if not operation:
            raise RuntimeError('cork_operation_failed')
        try:
            if not wait(lambda: ack, pump=self.pump) or ack != [1]:
                raise RuntimeError('cork_ack_failed')
        finally:
            self.operation_unref(operation)

    def close(self):
        if self.stream:
            self.stream_disconnect(self.stream)
            self.stream_unref(self.stream)
            self.stream = None
        if self.context:
            self.context_disconnect(self.context)
            self.context_unref(self.context)
            self.context = None
        if self.loop:
            self.mainloop_free(self.loop)
            self.loop = None


def route(source_name, target_name, pump=None):
    def locate(name):
        return [obj for obj in graph() if obj['type'].endswith(':Node') and
                name in (props(obj).get('node.name'), props(obj).get('media.name'))]
    source = wait(lambda: locate(source_name), pump=pump)
    target = wait(lambda: locate(target_name), pump=pump)
    if not source or not target:
        raise RuntimeError('route_node_missing')
    source, target = source[0], target[0]
    for node, direction in ((source, 'Output'), (target, 'Input')):
        if props(node).get('media.class') == 'Audio/Sink':
            direction = 'Input'
        if not node['info']['n-' + direction.lower() + '-ports']:
            command('pw-cli', 'set-param', str(node['id']), 'PortConfig',
                    '{ direction = ' + direction + ' mode = dsp monitor = true '
                    'format = { mediaType = audio mediaSubtype = raw format = F32P '
                    'rate = 48000 channels = 2 position = [ FL FR ] } }')
    ports = [obj for obj in graph() if obj['type'].endswith(':Port')]
    upstream = [obj for obj in ports if str(props(obj).get('node.id')) == str(source['id'])
                and props(obj).get('port.direction') == 'out']
    downstream = [obj for obj in ports if str(props(obj).get('node.id')) == str(target['id'])
                  and props(obj).get('port.direction') == 'in']
    emit('explicit_route', upstream=len(upstream), downstream=len(downstream))
    for dest in downstream:
        match = [port for port in upstream if props(port).get('audio.channel') ==
                 props(dest).get('audio.channel')]
        if match:
            command('pw-link', str(match[0]['id']), str(dest['id']))


def native_probe(source_index):
    native = spawn('pw-cat', '--record', '--format=s16', '--rate=48000', '--channels=2',
                   '--latency=40ms', '--target=oink_probe_source',
                   '--properties={ node.name = oink_probe_native }', '-',
                   stdin=S.DEVNULL, stdout=S.PIPE, stderr=S.PIPE,
                   env=dict(os.environ, PIPEWIRE_CONFIG_NAME='client.conf'))
    found = wait(lambda: nodes('oink_probe_native'))
    if not check('native_registered', bool(found), count=len(found or [])):
        stop(native)
        error = native.stderr.read()
        native.stderr.close()
        emit('native_start_error', code=native.returncode, bytes=len(error),
             missing_config=int(b'config' in error), missing_file=int(b'No such file' in error),
             context_failed=int(b'pw_context_new' in error),
             invalid_option=int(b'option' in error))
        return
    node = found[0]
    emit('native_before_manual_link', **capture(native, 0.3).values())
    route('oink_probe_source', 'oink_probe_native')
    capture(native, 0.3)
    meter = capture(native)
    dump = graph()
    links = [obj for obj in dump if obj['type'].endswith(':Link')
             and obj.get('info', {}).get('input-node-id') == node['id']]
    running = sum(obj.get('info', {}).get('state') == 'active' for obj in links)
    rows = pulse('source-outputs')
    serial = str(props(node).get('object.serial'))
    matches = [row for row in rows if str(row.get('properties', {}).get('object.serial')) == serial
               or row.get('properties', {}).get('node.name') == 'oink_probe_native']
    target_matches = [row for row in matches if row['source'] == source_index]
    emit('native_visibility', registered=1, links=len(links), active_links=running,
         pulse_total=len(rows), pulse_target=len(outputs(source_index)),
         pulse_identity_matches=len(matches), pulse_target_matches=len(target_matches),
         pulse_active_matches=sum(not row['corked'] for row in target_matches),
         supported=int(len(target_matches) == 1), **meter.values())
    check('native_real_capture', running == 2 and meter.samples > 10000
          and abs(meter.values()['rms'] / 5792.56 - 1) < 0.03)
    snapshot('native_running_demand', source_index)
    stop(native)
    native.stderr.close()
    check('native_exit', bool(wait(lambda: not nodes('oink_probe_native'))))
    wait(lambda: not outputs(source_index))
    check('native_exit_no_demand', snapshot('native_exit_demand', source_index) == (0, 0))


def main():
    isolation_guard()
    emit('isolation', status='pass')
    for tool in ('pactl', 'pacat', 'parec', 'pw-cat', 'pw-link', 'pw-dump'):
        if not shutil.which(tool):
            raise RuntimeError('missing_' + tool)
    core = next(obj for obj in graph() if obj['type'].endswith(':Core'))
    version = core['info']['version'].split('.')
    emit('pipewire_version', major=int(version[0]), minor=int(version[1]), micro=int(version[2]))
    sinks, sources = pulse('sinks'), pulse('sources')
    if not check('private_devices', [x['name'] for x in sinks] == ['oink_test'] and
                 [x['name'] for x in sources] == ['oink_test.monitor'],
                 sinks=len(sinks), sources=len(sources)):
        return 1
    tone = array.array('h', (round(8192 * math.sin(2 * math.pi * i / 48))
                           for i in range(4800) for _channel in range(2)))
    if sys.byteorder != 'little':
        tone.byteswap()
    payload = tone.tobytes()
    emit('synthetic', rate=48000, channels=2, hz=1000, amplitude=8192)
    player = spawn('pacat', '--playback', '--raw', '--format=s16le', '--rate=48000',
                   '--channels=2', '--latency-msec=40', '--device=oink_test',
                   '--stream-name=oink-probe-tone', stdin=S.PIPE, stdout=S.DEVNULL)
    def feed():
        try:
            while player.poll() is None:
                player.stdin.write(payload)
                player.stdin.flush()
        except (BrokenPipeError, ValueError, OSError):
            pass
    threading.Thread(target=feed, daemon=True).start()
    monitor = record('oink_test.monitor', 'oink-probe-monitor')
    emit('monitor_automatic', **capture(monitor, 0.5).values())
    route('oink-probe-tone', 'oink_test')
    route('oink_test', 'oink-probe-monitor')
    capture(monitor, 0.4)
    normal = capture(monitor)
    emit('monitor_unmuted', **normal.values())
    check('monitor_capture', normal.samples > 10000
          and abs(normal.values()['rms'] / 5792.56 - 1) < 0.03)
    command('pactl', 'set-sink-mute', 'oink_test', '1')
    capture(monitor, 0.4)
    muted = capture(monitor)
    emit('monitor_muted', **muted.values())
    check('mute_observed', pulse('sinks')[0]['mute'] and muted.samples > 10000)
    emit('mute_comparison', ratio=round(muted.values()['rms'] / max(1, normal.values()['rms']), 4),
         monitor_silenced=int(muted.samples > 10000 and muted.values()['rms'] < 1))
    command('pactl', 'set-sink-mute', 'oink_test', '0')
    capture(monitor, 0.4)
    restored = capture(monitor)
    check('monitor_restored', not pulse('sinks')[0]['mute'] and restored.samples > 10000
          and abs(restored.values()['rms'] / 5792.56 - 1) < 0.03, **restored.values())
    stop(monitor)
    module = command('pactl', 'load-module', 'module-remap-source',
                     'master=oink_test.monitor', 'source_name=oink_probe_source',
                     'channels=2', 'remix=no').decode().strip()
    modules.append(module)
    source = wait(lambda: [row for row in pulse('sources') if row['name'] == 'oink_probe_source'])
    if not source:
        raise RuntimeError('virtual_source_timeout')
    index = source[0]['index']
    route('oink_test', 'input.oink_probe_source')
    check('virtual_no_demand', snapshot('virtual_baseline', index) == (0, 0))
    consumer = record('oink_probe_source', 'oink-probe-parec')
    route('oink_probe_source', 'oink-probe-parec')
    wait(lambda: outputs(index))
    capture(consumer, 0.4)
    meter = capture(consumer)
    count, active = snapshot('parec_started', index)
    check('parec_real_capture', count == active == 1 and meter.samples > 10000
          and meter.values()['rms'] > 1000, **meter.values())
    stop(consumer)
    wait(lambda: not outputs(index))
    check('parec_exit_no_demand', snapshot('parec_exited', index) == (0, 0))
    client = PulseRecorder('oink_probe_source')
    try:
        meter = client.measure()
        check('cork_initial_no_demand', snapshot('cork_initial', index) == (1, 0)
              and meter.samples == 0, **meter.values())
        client.cork(0)
        client.measure(0.4)
        meter = client.measure()
        check('uncork_demand', snapshot('uncorked', index) == (1, 1)
              and meter.samples > 10000 and meter.values()['rms'] > 1000, **meter.values())
        client.cork(1)
        client.measure(0.4)
        meter = client.measure()
        check('cork_no_demand', snapshot('corked', index) == (1, 0)
              and meter.samples == 0, **meter.values())
        client.cork(0)
        client.measure(0.4)
        meter = client.measure()
        check('resume_demand', snapshot('resumed', index) == (1, 1)
              and meter.samples > 10000, **meter.values())
    finally:
        client.close()
    wait(lambda: not outputs(index))
    check('pulse_disconnect_no_demand', snapshot('pulse_disconnected', index) == (0, 0))
    native_probe(index)
    return int(bool(failures))


if __name__ == '__main__':
    result = 1
    try:
        result = main()
    except Exception as error:
        emit('probe_error', status=type(error).__name__, reason=str(error) if
             isinstance(error, RuntimeError) else 'unexpected')
    finally:
        for process in reversed(processes):
            stop(process)
        for module in reversed(modules):
            try:
                command('pactl', 'unload-module', module)
            except Exception:
                result = 1
        emit('summary', failures=failures, exit=result)
    sys.exit(result)
