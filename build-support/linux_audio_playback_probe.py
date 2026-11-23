#!/usr/bin/env python3
"""Exercise the product Linux playback path (encoder -> sink decoder ->
pulse playback) with a real tone in the private audio scope; run exclusively
via linux-native-test.py.

The private stack has no session manager: pulse streams never auto-link, so
the probe wires pipewire ports explicitly (PortConfig + pw-link) for both the
product playback node and the monitor recorder node — exactly what a real
session manager does on the host.

Registered private-stack limitation (plan.md): a playback-stream disconnect
destroys the other pulse stream nodes server-side. The recorder therefore
only needs to survive while the example runs (tone ends at ~5.5s of 10s);
its own node is expected to die at example teardown, after all evidence is
collected. Only levels/RMS/nonzero counts are recorded, never audio content.
"""
import json
import math
import os
from pathlib import Path
import select
import subprocess as S
import sys
import threading
import time

WINDOW_BYTES = 48000 * 2 * 2  # 1s of s16le stereo at 48 kHz


def emit(**fields):
    print(' '.join(f'{key}={value}' for key, value in fields.items()), flush=True)


def command(*args):
    result = S.run(list(args), capture_output=True, timeout=20)
    if result.returncode != 0:
        raise RuntimeError(f"command_failed: {' '.join(args)}: {result.stderr.decode()[:200]}")
    return result.stdout


def graph():
    return json.loads(command('pw-dump').decode())


def props(obj):
    return obj.get('info', {}).get('props', {})


def wait(find, timeout=20.0, interval=0.1):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        found = find()
        if found:
            return found
        time.sleep(interval)
    return None


def locate_node(name):
    return [obj for obj in graph() if obj['type'].endswith(':Node') and
            name in (props(obj).get('node.name'), props(obj).get('media.name'))]


def find_node(name, timeout=10):
    found = wait(lambda: locate_node(name), timeout=timeout)
    if not found:
        raise RuntimeError(f'node_missing: {name}')
    return found[0]


def force_ports(node, direction):
    if props(node).get('media.class') == 'Audio/Sink':
        direction = 'Input'
    if not node['info']['n-' + direction.lower() + '-ports']:
        command('pw-cli', 'set-param', str(node['id']), 'PortConfig',
                '{ direction = ' + direction + ' mode = dsp monitor = true '
                'format = { mediaType = audio mediaSubtype = raw format = F32P '
                'rate = 48000 channels = 2 position = [ FL FR ] } }')


def ports_of(node, direction):
    return [obj for obj in graph() if obj['type'].endswith(':Port')
            and str(props(obj).get('node.id')) == str(node['id'])
            and props(obj).get('port.direction') == direction]


def link_ports(source, target):
    upstream = ports_of(source, 'out')
    downstream = ports_of(target, 'in')
    linked = 0
    for dest in downstream:
        match = [port for port in upstream if props(port).get('audio.channel') ==
                 props(dest).get('audio.channel')]
        if match:
            result = S.run(['pw-link', str(match[0]['id']), str(dest['id'])],
                           capture_output=True, timeout=20)
            # pipewire-pulse may have linked the pair already; that is success.
            if result.returncode != 0 and b'File exists' not in result.stderr:
                raise RuntimeError(f"pw-link failed: {result.stderr.decode()[:200]}")
            linked += 1
    return len(upstream), len(downstream), linked


def route(source_name, target_name):
    """Feed source into target, forcing ports with PortConfig when absent."""
    source = find_node(source_name)
    target = find_node(target_name)
    force_ports(source, 'Output')
    force_ports(target, 'Input')
    target = find_node(target_name)
    return link_ports(source, target)


def rms_dbfs(data):
    if not data:
        return None
    count = len(data) // 2
    if count == 0:
        return None
    total = 0
    nonzero = 0
    for index in range(count):
        sample = int.from_bytes(data[index * 2:index * 2 + 2], 'little', signed=True)
        if sample:
            nonzero += 1
        total += sample * sample
    rms = math.sqrt(total / count)
    return {'rms_dbfs': 20.0 * math.log10(max(rms, 1e-9) / 32768.0),
            'nonzero': nonzero, 'samples': count}


def read_windows(stream, windows, stop):
    """Accumulate 1s RMS windows from the recorder stdout until stopped."""
    pending = b''
    while not stop.is_set():
        ready, _, _ = select.select([stream], [], [], 0.5)
        if not ready:
            continue
        chunk = os.read(stream.fileno(), WINDOW_BYTES * 2)
        if not chunk:
            break
        pending += chunk
        while len(pending) >= WINDOW_BYTES:
            windows.append(rms_dbfs(pending[:WINDOW_BYTES]))
            pending = pending[WINDOW_BYTES:]


def run_playback_phase(binary):
    """One playback phase; returns an evidence dict."""
    seconds = 10
    process = S.Popen([binary, '--seconds', str(seconds),
                       '--output', 'pulse:sink:oink_test'],
                      stdout=S.PIPE, stderr=S.PIPE, text=True, bufsize=1)
    recorder = None
    try:
        # The product stream connects corked and only reaches READY once it
        # has a route (private-stack behaviour): wire before awaiting start.
        upstream, downstream, linked = route('oink-playback', 'oink_test')
        emit(stage='playback_wired', upstream=upstream,
             downstream=downstream, linked=linked)
        recorder = S.Popen(
            ['parec', '--record', '--raw', '--format=s16le',
             '--rate=48000', '--channels=2', '--latency-msec=40',
             '--device=oink_test.monitor', '--stream-name=oink-playback-rec'],
            stdout=S.PIPE, stderr=S.PIPE,
            env=dict(os.environ, PULSE_PROP_stream_connect='false'))
        upstream, downstream, linked = route('oink_test', 'oink-playback-rec')
        emit(stage='recorder_wired', upstream=upstream,
             downstream=downstream, linked=linked)
        started = json.loads(process.stdout.readline())
        if started.get('stage') != 'started':
            raise RuntimeError(f'playback did not start: {started}')
        emit(stage='started', backend=started.get('backend'),
             sample_rate=started.get('sample_rate'),
             reported_output_latency_ms=started.get('reported_output_latency_ms'))
        windows = []
        stop = threading.Event()
        reader = threading.Thread(target=read_windows,
                                  args=(recorder.stdout, windows, stop),
                                  daemon=True)
        reader.start()
        ticks = []
        finished = {}
        for line in process.stdout:
            record = json.loads(line)
            if record.get('stage') == 'finished':
                finished = record
                continue
            if record.get('stage') != 'tick':
                emit(stage='example', detail=line.strip()[:200])
                continue
            emit(stage='tick', tick=record['tick'],
                 error=record.get('error'),
                 underrun_frames=record.get('underrun_frames'),
                 queued_frames=record.get('queued_frames'))
            ticks.append(record)
        process.wait(timeout=15)
        # The example exit severs the playback stream; the registered
        # limitation then tears down the recorder node server-side. Give the
        # reader a short grace period to drain windows already in flight.
        grace = time.monotonic() + 3.0
        while time.monotonic() < grace and reader.is_alive():
            time.sleep(0.1)
        stop.set()
        recorder.terminate()
        try:
            recorder.wait(timeout=10)
        except S.TimeoutExpired:
            recorder.kill()
            recorder.wait()
        return {
            'started': started,
            'ticks': ticks,
            'finished': finished,
            'windows': windows,
            'recorder_stderr_tail': recorder.stderr.read()[-200:],
            'process_returncode': process.returncode,
            'stderr_tail': process.stderr.read()[-200:],
        }
    finally:
        if recorder is not None and recorder.poll() is None:
            recorder.kill()
            recorder.wait()
        if process.poll() is None:
            process.kill()
            process.wait()


def evaluate(evidence):
    failures = []

    def check(name, ok, detail=''):
        emit(stage=name, result='pass' if ok else 'fail', detail=detail)
        if not ok:
            failures.append(name)

    check('playback_started',
          evidence['started'].get('backend') == 'pulse'
          and evidence['started'].get('sample_rate') == 48000,
          f"backend={evidence['started'].get('backend')}")
    check('no_playback_error',
          all(not t.get('error') for t in evidence['ticks'])
          and evidence['finished'].get('error') is None
          and evidence['process_returncode'] == 0,
          f"rc={evidence['process_returncode']}")
    check('device_not_invalidated',
          all(not t.get('device_invalidated') for t in evidence['ticks']),
          '')
    underruns = [t.get('underrun_frames', 0) for t in evidence['ticks']]
    check('no_underruns', all(value == 0 for value in underruns),
          f'underruns={underruns}')
    windows = evidence['windows']
    emit(stage='window_levels',
         levels=','.join(f"{w['rms_dbfs']:.1f}" for w in windows))
    check('recorder_flowing', len(windows) >= 6,
          f'windows={len(windows)}')
    silent = [index for index, w in enumerate(windows) if w['rms_dbfs'] < -60.0]
    loud = [index for index, w in enumerate(windows)
            if -40.0 < w['rms_dbfs'] < -6.0]
    # End-to-end ordering: a silent window, then the tone, then silence again.
    ordered = any(i < j < k for i in silent for j in loud for k in silent)
    check('silence_tone_silence_ordered', ordered,
          f'silent={silent} loud={loud}')
    check('tone_rms_plausible', bool(loud), f'loud={loud}')
    nonzero = [w['nonzero'] for w in windows if -40.0 < w['rms_dbfs'] < -6.0]
    check('tone_window_has_pcm', nonzero and min(nonzero) > 40000,
          f'nonzero={nonzero}')
    check('clean_finish', evidence['finished'].get('stage') == 'finished'
          and evidence['finished'].get('error') is None,
          f"finished={evidence['finished'].get('stage')}")
    return failures


def main():
    if 'PULSE_SERVER' not in os.environ:
        raise RuntimeError('must run inside linux-native-test.py scope')
    binary = sys.argv[1]
    if not Path(binary).is_file():
        raise RuntimeError(f'example binary missing: {binary}')
    evidence = run_playback_phase(binary)
    failures = evaluate(evidence)
    emit(stage='summary', failures=len(failures), exit=1 if failures else 0)
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
