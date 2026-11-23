#!/usr/bin/env python3
"""Exercise the product Linux microphone capture, virtual source and demand
monitor against the private test source; run exclusively via linux-native-test.py.

No audio content is involved: only counts, flags, rates and channel numbers
cross the wire. The foreign demand recorder is a host-side parec process the
probe spawns and kills; its only observable is the DemandMonitor sample.
"""
import json
import os
from pathlib import Path
import subprocess as S
import sys
import threading
import time


def emit(**fields):
    print(' '.join(f'{key}={value}' for key, value in fields.items()), flush=True)


def read_line(process, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if line:
            return json.loads(line)
        time.sleep(0.05)
    raise RuntimeError('timed out waiting for example output')


def command(*args):
    result = S.run(list(args), capture_output=True, timeout=20)
    if result.returncode != 0:
        raise RuntimeError(f"command_failed: {' '.join(args)}: {result.stderr.decode()[:200]}")
    return result.stdout.decode()


def graph():
    text = command('pw-dump')
    decoder = json.JSONDecoder()
    objects = []
    index = 0
    while index < len(text):
        while index < len(text) and text[index].isspace():
            index += 1
        if index >= len(text):
            break
        obj, end = decoder.raw_decode(text, index)
        objects.append(obj)
        index = end
    # pw-dump normally returns one array; tolerate concatenated objects.
    flat = []
    for obj in objects:
        if isinstance(obj, list):
            flat.extend(obj)
        else:
            flat.append(obj)
    return flat


def props(obj):
    return obj.get('info', {}).get('props', {}) if isinstance(obj, dict) else {}


def is_type(obj, suffix):
    return isinstance(obj, dict) and str(obj.get('type', '')).endswith(suffix)


def locate_node(name):
    return [obj for obj in graph() if is_type(obj, ':Node') and
            name in (props(obj).get('node.name'), props(obj).get('media.name'))]


def find_node(name, timeout=20):
    deadline = time.monotonic() + timeout
    last_nodes = []
    while time.monotonic() < deadline:
        dump = graph()
        last_nodes = [props(obj).get('node.name', '?')
                      for obj in dump if is_type(obj, ':Node')]
        found = [obj for obj in dump if is_type(obj, ':Node') and
                 name in (props(obj).get('node.name'), props(obj).get('media.name'))]
        if found:
            return found[0]
        time.sleep(0.2)
    raise RuntimeError(f'node_missing: {name} nodes={last_nodes}')


def force_ports(node, direction):
    if props(node).get('media.class') == 'Audio/Sink':
        direction = 'Input'
    if not node['info']['n-' + direction.lower() + '-ports']:
        command('pw-cli', 'set-param', str(node['id']), 'PortConfig',
                '{ direction = ' + direction + ' mode = dsp monitor = true '
                'format = { mediaType = audio mediaSubtype = raw format = F32P '
                'rate = 48000 channels = 2 position = [ FL FR ] } }')


def ports_of(node, direction):
    return [obj for obj in graph() if is_type(obj, ':Port')
            and str(props(obj).get('node.id')) == str(node.get('id'))
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
            if result.returncode != 0 and b'File exists' not in result.stderr:
                raise RuntimeError(f"pw-link failed: {result.stderr.decode()[:200]}")
            linked += 1
    return len(upstream), len(downstream), linked


def route(source_name, target_name):
    source = find_node(source_name)
    target = find_node(target_name)
    force_ports(source, 'Output')
    force_ports(target, 'Input')
    target = find_node(target_name)
    return link_ports(source, target)


def main():
    emit(stage='probe_main_start')
    if 'PULSE_SERVER' not in os.environ:
        raise RuntimeError('must run inside linux-native-test.py scope')
    binary = sys.argv[1]
    if not Path(binary).is_file():
        raise RuntimeError(f'example binary missing: {binary}')
    process = S.Popen([binary, '--source', 'pulse:source:oink_test_source', '--seconds', '8'],
                      stdout=S.PIPE, stderr=S.PIPE, text=True, bufsize=1)
    failures = []

    def check(name, ok, detail=''):
        emit(stage=name, result='pass' if ok else 'fail', detail=detail)
        if not ok:
            failures.append(name)

    # Wire the microphone capture node to the test source in the background:
    # the stream connects corked and only reaches READY once it has a route
    # (private-stack behaviour), so the wiring must race the example's start.
    wiring_error = []

    def wire_all():
        # The foreign demand recorder needs a route on the session-manager-less
        # stack; wire it first (the example only waits ~5 s for demand) so the
        # DemandMonitor observes it as active demand.
        for _ in range(30):
            try:
                upstream, downstream, linked = route('oink_input', 'oink-foreign-rec')
                emit(stage='foreign_wired', upstream=upstream, downstream=downstream, linked=linked)
                break
            except Exception:
                time.sleep(0.3)
        # Give the remap-source an input route from the test monitor so it
        # starts producing; without this the source is idle and the product
        # record stream never leaves Creating on the session-manager-less stack.
        for _ in range(30):
            try:
                route('oink_test_mic', 'oink_test_source')
                emit(stage='remap_source_wired')
                break
            except Exception:
                time.sleep(0.3)
        try:
            emit(stage='mic_wiring_start')
            # The product record stream's node is named after the pulse client
            # name (oink-microphone), not the source; poll until it appears and
            # wire the test source's output into it.
            for attempt in range(10):
                try:
                    upstream, downstream, linked = route('oink_test_source', 'oink-microphone')
                    emit(stage='mic_wired', attempt=attempt, upstream=upstream,
                         downstream=downstream, linked=linked)
                    break
                except Exception:
                    time.sleep(0.5)
            else:
                raise RuntimeError('product record node oink-microphone never appeared')
            # The render playback stream (client name oink-playback) feeds the
            # virtual sink oink_mic; without a route it never reaches READY.
            for attempt in range(20):
                try:
                    upstream, downstream, linked = route('oink-playback', 'oink_mic')
                    emit(stage='render_wired', attempt=attempt, upstream=upstream,
                         downstream=downstream, linked=linked)
                    break
                except Exception:
                    time.sleep(0.5)
            else:
                raise RuntimeError('render playback node oink-playback never appeared')
        except Exception as error:
            wiring_error.append(repr(error))
            emit(stage='mic_wiring_error', error=repr(error))

    threading.Thread(target=wire_all, daemon=True).start()
    try:
        permission = read_line(process)
        check('permission_system_controlled',
              permission.get('stage') == 'permission'
              and permission.get('permission') == 'SystemControlled',
              f"stage={permission.get('stage')} permission={permission.get('permission')}")
        enumeration = read_line(process)
        check('enumeration_reports_test_source',
              enumeration.get('stage') == 'enumeration'
              and enumeration.get('inputs', 0) >= 1
              and enumeration.get('virtual_output_ready') is True,
              f"inputs={enumeration.get('inputs')} candidates={enumeration.get('candidates')} "
              f"virtual={enumeration.get('virtual_output_ready')}")
        demand_before = read_line(process)
        check('foreign_demand_detected',
              demand_before.get('stage') == 'demand_before_capture'
              and demand_before.get('active') is True,
              f"active={demand_before.get('active')} epoch={demand_before.get('epoch')}")
        pre_capture = read_line(process)
        check('reached_pre_capture', pre_capture.get('stage') == 'pre_capture',
              f"stage={pre_capture.get('stage')}")

        def dump_detail():
            nodes = []
            for obj in graph():
                if not is_type(obj, ':Node'):
                    continue
                p = props(obj)
                ports = ports_of(obj, 'Output') + ports_of(obj, 'Input')
                nodes.append({
                    'id': obj.get('id'), 'name': p.get('node.name'),
                    'media_name': p.get('media.name'), 'class': p.get('media.class'),
                    'in_ports': len(ports_of(obj, 'Input')),
                    'out_ports': len(ports_of(obj, 'Output')),
                    'ports': [{'id': port.get('id'),
                               'dir': props(port).get('port.direction'),
                               'ch': props(port).get('audio.channel'),
                               'links': props(port).get('link.ids')}
                              for port in ports],
                })
            links = [{'id': obj.get('id'),
                      'out': props(obj).get('link.output.port'),
                      'in': props(obj).get('link.input.port'),
                      'active': props(obj).get('link.active')}
                     for obj in graph() if is_type(obj, ':Link')]
            return nodes, links

        nodes, links = dump_detail()
        emit(stage='nodes_after_wiring', nodes=nodes, links=links)
        try:
            capture_started = read_line(process, timeout=30)
            emit(stage='capture_line', line=capture_started)
        except RuntimeError:
            stderr = process.stderr.read() if process.stderr else ''
            emit(stage='capture_timeout', stderr=stderr[:500])
            raise
        check('capture_started',
              capture_started.get('stage') == 'capture_started'
              and capture_started.get('device') == 'pulse:source:oink_test_source'
              and capture_started.get('backend') == 'pulse_record'
              and capture_started.get('hardware_sample_rate', 0) > 0
              and capture_started.get('hardware_channels', 0) > 0,
              f"device={capture_started.get('device')} backend={capture_started.get('backend')} "
              f"rate={capture_started.get('hardware_sample_rate')} channels={capture_started.get('hardware_channels')}")
        # Ticks while both capture and virtual render sink are running.
        tick_ok = 0
        for _ in range(8):
            tick = read_line(process)
            if tick.get('stage') != 'tick':
                emit(stage='unexpected_line', line=tick)
            if (tick.get('stage') == 'tick'
                    and tick.get('capture_error') is None
                    and tick.get('sink_error') is None
                    and tick.get('capture_invalidated') is False):
                tick_ok += 1
        check('ticks_stable', tick_ok == 8, f'stable={tick_ok}/8')
        demand_after = read_line(process)
        check('demand_remains_active',
              demand_after.get('stage') == 'demand_during_capture'
              and demand_after.get('active') is True,
              f"active={demand_after.get('active')} epoch={demand_after.get('epoch')}")
        quiet = read_line(process)
        check('demand_quiet_after_foreign_exit',
              quiet.get('stage') == 'demand_after_foreign_exit'
              and quiet.get('quiet') is True,
              f"quiet={quiet.get('quiet')}")
        finished = read_line(process)
        check('clean_finish', finished.get('stage') == 'finished',
              f"stage={finished.get('stage')}")
        process.wait(timeout=15)
        check('exit_zero', process.returncode == 0,
              f'rc={process.returncode}')
        check('mic_wiring_error_empty', not wiring_error, f'errors={wiring_error}')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        stderr = process.stderr.read() if process.stderr else ''
        if stderr:
            emit(stage='example_stderr', stderr=stderr[:500])
    emit(stage='summary', failures=len(failures), exit=1 if failures else 0)
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
