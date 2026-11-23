#!/usr/bin/env python3
"""Exercise the product Linux volume endpoint (read/write/latch/restore)
against the private test sink; run exclusively via linux-native-test.py.

No audio content is involved: only percents and mute flags cross the wire.
External changes are applied with pactl (the host-side view), and the probe
cross-checks the endpoint readback against `pactl get-sink-volume/mute`.
"""
import json
import os
from pathlib import Path
import subprocess as S
import sys
import time


def emit(**fields):
    print(' '.join(f'{key}={value}' for key, value in fields.items()), flush=True)


def command(*args):
    result = S.run(list(args), capture_output=True, timeout=20)
    if result.returncode != 0:
        raise RuntimeError(f"command_failed: {' '.join(args)}: {result.stderr.decode()[:200]}")
    return result.stdout.decode()


def sink_volume_percent(name):
    # pactl prints e.g. "Volume: front-left: 26214 /  40% / -13.89 dB ..."
    out = command('pactl', 'get-sink-volume', name)
    for token in out.replace('/', ' ').split():
        if token.endswith('%'):
            return int(token[:-1])
    raise RuntimeError(f'no percent in: {out[:120]}')


def sink_muted(name):
    return command('pactl', 'get-sink-mute', name).strip().endswith('yes')


def read_line(process, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if line:
            return json.loads(line)
        time.sleep(0.05)
    raise RuntimeError('timed out waiting for example output')


def main():
    if 'PULSE_SERVER' not in os.environ:
        raise RuntimeError('must run inside linux-native-test.py scope')
    binary = sys.argv[1]
    if not Path(binary).is_file():
        raise RuntimeError(f'example binary missing: {binary}')
    process = S.Popen([binary, '--output', 'pulse:sink:oink_test'],
                      stdin=S.PIPE, stdout=S.PIPE, stderr=S.PIPE, text=True,
                      bufsize=1)
    failures = []

    def check(name, ok, detail=''):
        emit(stage=name, result='pass' if ok else 'fail', detail=detail)
        if not ok:
            failures.append(name)

    try:
        initial = read_line(process)
        check('endpoint_opened', initial.get('stage') == 'initial',
              f"stage={initial.get('stage')}")
        write_ack = read_line(process)
        check('write_applied', write_ack.get('stage') == 'write_ack'
              and write_ack.get('reading', {}).get('percent') == 40
              and write_ack.get('reading', {}).get('muted') is True,
              f"reading={write_ack.get('reading')}")
        host_percent = sink_volume_percent('oink_test')
        host_muted = sink_muted('oink_test')
        check('host_observes_write', host_percent in (39, 40, 41) and host_muted,
              f'percent={host_percent} muted={host_muted}')
        # External change the endpoint never wrote itself.
        command('pactl', 'set-sink-volume', 'oink_test', '55%')
        command('pactl', 'set-sink-mute', 'oink_test', '0')
        process.stdin.write('changed\n')
        process.stdin.flush()
        external = read_line(process)
        check('external_change_latched', external.get('latched') is True
              and external.get('wakes', 0) >= 1,
              f"latched={external.get('latched')} wakes={external.get('wakes')}")
        check('external_change_readback',
              external.get('reading', {}).get('percent') == 55
              and external.get('reading', {}).get('muted') is False,
              f"reading={external.get('reading')}")
        missing = read_line(process)
        check('missing_sink_fails_closed',
              missing.get('stage') == 'missing_sink'
              and missing.get('error') is not None,
              f"error={missing.get('error')}")
        restored = read_line(process)
        initial_reading = initial.get('reading', {})
        check('original_restored',
              restored.get('reading') == initial_reading,
              f"restored={restored.get('reading')} initial={initial_reading}")
        host_percent = sink_volume_percent('oink_test')
        host_muted = sink_muted('oink_test')
        check('host_observes_restore',
              host_percent == initial_reading.get('percent') and host_muted == initial_reading.get('muted'),
              f'percent={host_percent} muted={host_muted}')
        finished = read_line(process)
        check('clean_finish', finished.get('stage') == 'finished',
              f"stage={finished.get('stage')}")
        process.wait(timeout=15)
        check('exit_zero', process.returncode == 0,
              f'rc={process.returncode}')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
    emit(stage='summary', failures=len(failures), exit=1 if failures else 0)
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
