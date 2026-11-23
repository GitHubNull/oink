#!/usr/bin/env python3
"""Exercise the Linux device-notification subscription against real module
load/unload events in the private audio scope; run exclusively via
linux-native-test.py."""
import json
import os
from pathlib import Path
import subprocess as S
import sys


def emit(**fields):
    print(' '.join(f'{key}={value}' for key, value in fields.items()), flush=True)


def load_sink(name):
    result = S.run(['pactl', 'load-module', 'module-null-sink', f'sink_name={name}'],
                   capture_output=True, text=True, timeout=15)
    if result.returncode != 0:
        raise RuntimeError(f'load {name} failed: {result.stderr[-300:]}')
    return result.stdout.strip()


def unload(module):
    result = S.run(['pactl', 'unload-module', module], capture_output=True, text=True,
                   timeout=15)
    if result.returncode != 0:
        raise RuntimeError(f'unload {module} failed: {result.stderr[-300:]}')


def main():
    if 'PULSE_SERVER' not in os.environ:
        raise RuntimeError('must run inside linux-native-test.py scope')
    binary = sys.argv[1]
    if not Path(binary).is_file():
        raise RuntimeError(f'example binary missing: {binary}')
    process = S.Popen([binary, '--seconds', '12'], stdout=S.PIPE, stderr=S.PIPE,
                      text=True, bufsize=1)
    failures = []
    try:
        initial = json.loads(process.stdout.readline())
        if initial.get('stage') != 'initial':
            raise RuntimeError(f'unexpected first record: {initial}')
        emit(stage='initial', generation=initial.get('generation'),
             service_generation=initial.get('service_generation'))
        initial_generation = initial.get('generation')
        service_generation = initial.get('service_generation')
        loaded_module = None
        ticks = []
        for line in process.stdout:
            record = json.loads(line)
            if record.get('stage') != 'tick':
                continue
            ticks.append(record)
            if record['tick'] == 2 and loaded_module is None:
                loaded_module = load_sink('probe_notify_out')
                emit(stage='loaded', module=loaded_module)
            if record['tick'] == 6 and loaded_module is not None:
                unload(loaded_module)
                emit(stage='unloaded', module=loaded_module)
                loaded_module = None
        process.wait(timeout=30)
        if process.returncode != 0:
            raise RuntimeError(f'example exited {process.returncode}: '
                               f'{process.stderr.read()[-300:]}')

        def check(name, ok, detail=''):
            emit(stage=name, result='pass' if ok else 'fail', detail=detail)
            if not ok:
                failures.append(name)

        generations = [tick.get('generation') for tick in ticks]
        max_generation = max(generations) if generations else None
        total_wakes = ticks[-1].get('wakes') if ticks else 0
        check('initial_generation_present',
              isinstance(initial_generation, int) and initial_generation >= 1,
              f'generation={initial_generation}')
        check('service_generation_present',
              isinstance(service_generation, int) and service_generation >= 1,
              f'service_generation={service_generation}')
        check('generation_advanced_on_load_unload',
              max_generation is not None and max_generation > initial_generation,
              f'initial={initial_generation} max={max_generation}')
        check('wake_callback_fired', isinstance(total_wakes, int) and total_wakes > 0,
              f'wakes={total_wakes}')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
    emit(stage='summary', failures=len(failures), exit=1 if failures else 0)
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
