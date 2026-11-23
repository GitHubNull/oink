#!/usr/bin/env python3
"""Compare oink-audio Linux enumeration against pactl inside the private audio
scope; run exclusively via linux-native-test.py."""
import json
import os
from pathlib import Path
import subprocess as S
import sys


def emit(**fields):
    print(' '.join(f'{key}={value}' for key, value in fields.items()), flush=True)


def run_example(binary):
    result = S.run([binary], capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise RuntimeError(f'example exited {result.returncode}: {result.stderr[-400:]}')
    records = []
    for line in result.stdout.splitlines():
        line = line.strip()
        if line:
            records.append(json.loads(line))
    return records


def pactl_json(*args):
    result = S.run(['pactl', '-f', 'json', *args], capture_output=True, text=True, timeout=15)
    if result.returncode != 0:
        raise RuntimeError(f'pactl {" ".join(args)} exited {result.returncode}: {result.stderr[-400:]}')
    return json.loads(result.stdout)


def pactl_default_sink():
    # pactl info on pipewire-pulse can report the literal '@DEFAULT_SINK'
    # placeholder; get-default-sink resolves the real name.
    result = S.run(['pactl', 'get-default-sink'], capture_output=True, text=True,
                   timeout=15)
    if result.returncode != 0:
        raise RuntimeError(
            f'pactl get-default-sink exited {result.returncode}: {result.stderr[-400:]}')
    name = result.stdout.strip()
    if not name:
        raise RuntimeError('pactl get-default-sink returned empty')
    return name


def public_sink_names(sinks):
    return sorted(
        sink['name'] for sink in sinks
        if sink.get('name') and not sink['name'].startswith('oink_'))


def compare(records, sinks, default_sink):
    outputs = [record for record in records if record.get('stage') == 'output']
    failures = []

    def check(name, ok, detail=''):
        emit(stage=name, result='pass' if ok else 'fail', detail=detail)
        if not ok:
            failures.append(name)

    expected_names = public_sink_names(sinks)
    actual_names = sorted(
        record['id'].removeprefix('pulse:sink:')
        for record in outputs
        if record.get('id', '').startswith('pulse:sink:'))
    check('sink_sets_match', actual_names == expected_names,
          f'probe={actual_names} pactl={expected_names}')
    check('all_ids_prefixed',
          all(record.get('id', '').startswith('pulse:sink:') for record in outputs))
    check('private_sinks_excluded',
          not any('oink_' in record.get('id', '') for record in outputs))
    default_ids = [record['id'] for record in outputs if record.get('is_default')]
    # A private default sink (the virtual microphone render node) is excluded
    # from enumeration, so no output may claim the default flag then. The
    # symbolic '@DEFAULT_SINK@' placeholder (minimal pipewire-pulse without
    # session-manager policy) means "no concrete default reported": the probe
    # must see no default flags and an explicit no-concrete-default error,
    # never a guessed name.
    symbolic_default = default_sink.startswith('@')
    expected_default = (
        [] if symbolic_default or default_sink.startswith('oink_')
        else [f'pulse:sink:{default_sink}'])
    check('default_matches_pactl', default_ids == expected_default,
          f'probe={default_ids} pactl={default_sink}')
    rate = next((record.get('rate') for record in records
                 if record.get('stage') == 'default_sink_rate'), None)
    rate_errors = [record for record in records
                   if record.get('stage') == 'default_sink_rate_error']
    if symbolic_default:
        check('symbolic_default_fails_closed', rate is None and len(rate_errors) == 1
              and 'no concrete default' in rate_errors[0].get('error', ''),
              f'rate={rate} errors={len(rate_errors)}')
    else:
        check('default_sink_rate_reported', isinstance(rate, int) and rate > 0,
              f'rate={rate}')
    errors = [record for record in records
              if record.get('stage', '').endswith('_error') and record not in rate_errors]
    check('no_enumerate_errors', not errors, f'errors={len(errors)}')
    return failures


def main():
    if 'PULSE_SERVER' not in os.environ:
        raise RuntimeError('must run inside linux-native-test.py scope')
    binary = sys.argv[1]
    if not Path(binary).is_file():
        raise RuntimeError(f'example binary missing: {binary}')
    extra_sink = S.run(['pactl', 'load-module', 'module-null-sink',
                        'sink_name=probe_line_out'], capture_output=True, text=True,
                       timeout=15)
    if extra_sink.returncode != 0:
        raise RuntimeError(f'load probe_line_out failed: {extra_sink.stderr[-400:]}')
    failures = []
    cleanup_failures = 0
    try:
        records = run_example(binary)
        sinks = pactl_json('list', 'sinks')
        default_sink = pactl_default_sink()
        failures = compare(records, sinks, default_sink)
    finally:
        unload = S.run(['pactl', 'unload-module', extra_sink.stdout.strip()],
                       capture_output=True, text=True, timeout=15)
        if unload.returncode != 0:
            cleanup_failures += 1
            emit(stage='cleanup_warning', detail=unload.stderr.strip()[-200:])
    emit(stage='summary', failures=len(failures),
         cleanup_failures=cleanup_failures, exit=1 if failures else 0)
    sys.exit(1 if failures or cleanup_failures else 0)


if __name__ == '__main__':
    main()
