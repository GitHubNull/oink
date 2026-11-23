#!/usr/bin/env python3
"""Exercise the product Linux capture path (monitor -> encoder) with a real
tone in the private audio scope; run exclusively via linux-native-test.py.

The private stack has no session manager: pulse streams never auto-link, so
the probe wires pipewire ports explicitly (PortConfig + pw-link) for both the
capture stream node and the tone player node — exactly what a real session
manager does on the host.

Known private-stack limitation (registered in plan.md): pipewire-pulse 1.0.5
without a session manager randomly destroys stream nodes server-side (2.5s-18s
after start, client connection stays open). The probe therefore runs several
short capture phases and accumulates evidence across them: an early node death
still leaves usable pre-tone/tone evidence in that attempt, and later
attempts cover post-tone silence. Only levels/RMS/packet counts are recorded,
never audio content."""
import json
import math
import os
from pathlib import Path
import subprocess as S
import sys
import threading
import time


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


def input_link_count(node):
    return sum(1 for obj in graph() if obj['type'].endswith(':Link')
               and str(props(obj).get('input.nodeId')) == str(node['id']))


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


def tone_payload(seconds=3, rate=48000, hz=440, amplitude=12000):
    frames = rate * seconds
    data = bytearray()
    for index in range(frames):
        sample = int(amplitude * math.sin(2.0 * math.pi * hz * index / rate))
        data += sample.to_bytes(2, 'little', signed=True) * 2
    return bytes(data)


def run_capture_phase(binary, attempt):
    """One short capture phase; returns an evidence dict.

    Raises RuntimeError on harness-level failures (missing nodes, pacat
    errors). A server-side node death is NOT an error here: the ticks
    collected so far still carry evidence, and `stalled` marks it."""
    seconds = 8
    process = S.Popen([binary, '--seconds', str(seconds)], stdout=S.PIPE,
                      stderr=S.PIPE, text=True, bufsize=1)
    player = None
    writer_error = []
    try:
        upstream, downstream, linked = route('oink_test', 'oink-capture')
        emit(stage='capture_wired', attempt=attempt, upstream=upstream,
             downstream=downstream, linked=linked)
        started = json.loads(process.stdout.readline())
        if started.get('stage') != 'started':
            raise RuntimeError(f'capture did not start: {started}')
        emit(stage='started', attempt=attempt, backend=started.get('backend'),
             sample_rate=started.get('sample_rate'))
        payload = tone_payload()

        def feed_tone():
            try:
                route('oink-capture-tone', 'oink_test')
                # Registered pipewire-pulse 1.0.5 limitation: a playback
                # stream disconnect destroys the other pulse stream nodes
                # server-side. Keep pacat connected (stdin open, silence)
                # until the attempt ends so the capture stream survives.
                time.sleep(1.5)
                player.stdin.write(payload)
                player.stdin.flush()
            except Exception as error:  # surfaced after the loop
                writer_error.append(repr(error))

        player = S.Popen(
            ['pacat', '--playback', '--raw', '--format=s16le',
             '--rate=48000', '--channels=2', '--latency-msec=40',
             '--device=oink_test', '--stream-name=oink-capture-tone'],
            stdin=S.PIPE, stdout=S.DEVNULL, stderr=S.PIPE,
            env=dict(os.environ, PULSE_PROP_stream_connect='false'))
        threading.Thread(target=feed_tone, daemon=True).start()
        ticks = []
        finished = {}
        last_calls = None
        stalled = 0
        for line in process.stdout:
            record = json.loads(line)
            if record.get('stage') == 'finished':
                finished = record
                continue
            if record.get('stage') != 'tick':
                continue
            calls = record.get('cb', {}).get('calls')
            if calls is not None and calls == last_calls:
                stalled += 1
            else:
                stalled = 0
            last_calls = calls
            emit(stage='tick', attempt=attempt, tick=record['tick'],
                 calls=calls, frames=record.get('frames'))
            ticks.append(record)
            if stalled >= 2 and record['tick'] < seconds:
                emit(stage='node_dropped', attempt=attempt,
                     tick=record['tick'])
                break
        if writer_error:
            player.wait(timeout=10)
            raise RuntimeError(
                f'tone feeder failed: {writer_error[0]} '
                f'pacat rc={player.returncode}: '
                f'{player.stderr.read().decode()[-300:]}')
        if process.poll() is None and not finished:
            # Node death: the example is still alive waiting for stop.
            process.terminate()
            try:
                process.wait(timeout=10)
            except S.TimeoutExpired:
                process.kill()
                process.wait()
        if player is not None:
            player.terminate()
            try:
                player.wait(timeout=10)
            except S.TimeoutExpired:
                player.kill()
                player.wait()
        return {
            'started': started,
            'ticks': ticks,
            'finished': finished,
            'stalled': stalled > 0,
            'process_returncode': process.returncode,
            'stderr_tail': process.stderr.read()[-200:],
        }
    finally:
        if player is not None and player.poll() is None:
            player.kill()
            player.wait()
        if process.poll() is None:
            process.kill()
            process.wait()


def evaluate(evidence):
    """Merge per-attempt evidence into gate checks."""
    failures = []

    def check(name, ok, detail=''):
        emit(stage=name, result='pass' if ok else 'fail', detail=detail)
        if not ok:
            failures.append(name)

    def rms_of(tick):
        value = tick.get('rms_dbfs')
        return value if value is not None else -120.0

    started_ok = any(e['started'].get('backend') == 'pulse'
                     and e['started'].get('sample_rate') == 48000
                     for e in evidence)
    check('capture_started', started_ok,
          f"attempts={len(evidence)}")
    check('pcm_flowing', any(e['ticks'] and e['ticks'][-1].get('frames', 0) > 0
                             for e in evidence),
          'frames=' + ','.join(str(e['ticks'][-1].get('frames', 0))
                               if e['ticks'] else '-' for e in evidence))
    # Tone starts ~1.5s into each attempt and plays ~3s (signal lands on
    # ticks 2..6); pacat then stays connected in silence, so ticks 7..8 are
    # reliably post-tone.
    check('silent_before_tone',
          any(e['ticks'] and all(rms_of(t) < -60.0 for t in e['ticks']
                                 if t['tick'] <= 1) for e in evidence),
          'attempts=' + ','.join(str(len(e['ticks'])) for e in evidence))
    tone_max = [max((rms_of(t) for t in e['ticks'] if 2 <= t['tick'] <= 5),
                    default=None) for e in evidence]
    check('signal_during_tone',
          any(value is not None and value > -60.0 for value in tone_max),
          f'max_rms={tone_max}')
    plausible = [value for value in tone_max
                 if value is not None and -40.0 < value < -6.0]
    check('tone_rms_plausible', bool(plausible), f'plausible={plausible}')
    check('silent_after_tone_stops',
          any(e['ticks'] and all(rms_of(t) < -60.0 for t in e['ticks']
                                 if t['tick'] >= 7) for e in evidence),
          'after=' + ','.join(str(sum(1 for t in e['ticks'] if t['tick'] >= 7))
                              for e in evidence))
    errors = [t.get('error') for e in evidence for t in e['ticks'] if t.get('error')]
    check('no_capture_error', not errors, f'errors={len(errors)}')
    check('clean_finish', any(e['finished'].get('stage') == 'finished'
                              and e['finished'].get('error') is None
                              for e in evidence),
          'finished=' + ','.join(str(e['finished'].get('stage'))
                                 for e in evidence))
    dropped = sum(1 for e in evidence if e['stalled'])
    emit(stage='node_drop_stat', dropped=dropped, attempts=len(evidence))
    return failures


def main():
    if 'PULSE_SERVER' not in os.environ:
        raise RuntimeError('must run inside linux-native-test.py scope')
    binary = sys.argv[1]
    if not Path(binary).is_file():
        raise RuntimeError(f'example binary missing: {binary}')
    # Phase 1: the private stack has no session-manager policy (symbolic
    # default, set-default-sink unsupported), so capture without an explicit
    # source must fail closed, never guess.
    env = dict(os.environ)
    env.pop('OINK_CAPTURE_SOURCE', None)
    probe = S.run([binary, '--seconds', '2'], capture_output=True, text=True,
                  env=env, timeout=60)
    first = json.loads(probe.stdout.splitlines()[0])
    fail_closed = (probe.returncode == 2 and first.get('stage') == 'start_error'
                   and 'no concrete default' in first.get('error', ''))
    emit(stage='no_default_fails_closed', result='pass' if fail_closed else 'fail',
         detail=first.get('error', '')[:120])
    failures = [] if fail_closed else ['no_default_fails_closed']

    # Phase 2: short attempts accumulate evidence despite the private
    # server's random stream-node destruction (registered limitation).
    os.environ['OINK_CAPTURE_SOURCE'] = 'oink_test.monitor'
    evidence = []
    for attempt in range(1, 5):
        try:
            evidence.append(run_capture_phase(binary, attempt))
        except RuntimeError as error:
            emit(stage='attempt_failed', attempt=attempt, detail=str(error)[:160])
        if len(evidence) >= 3:
            break
    if not evidence:
        raise RuntimeError('no capture attempt produced evidence')
    failures.extend(evaluate(evidence))
    emit(stage='summary', failures=len(failures), exit=1 if failures else 0)
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
