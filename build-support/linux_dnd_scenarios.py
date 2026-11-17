#!/usr/bin/env python3
"""Native GTK multi-process acceptance, only inside linux-native-test.py.

Build externally (no auto-install or auto-compilation); pass --roles /absolute/binary.
Each C source is separate: linux_dnd_roles.c linux_dnd_native.c linux_dnd_selection.c.
The dual-server path is a TEST BRIDGE, not Oink networking: source XdndSelection
read -> catcher FETCH -> controller -> proxy LOAD -> proxy XdndSelection -> target.
The target never receives body bytes over its control pipe. Importing is GUI-free.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from linux_dnd_protocol import (
    ARM, CLIP_CHECK, DELAY, DISARM, DOWN, ESCAPE, FETCH, FOCUS, FREE, HELD, LOAD,
    MOVE, ORPHAN_UP, READ_SELECTION, RETURNED, STATUS, UP,
    Blocked, Failure, Role, check, isolated_environment, peer_environment,
)

COPY = 2  # GDK_ACTION_COPY
CONTROL_ACK_NS = 500_000_000
PREPARATION_NS = 3_000_000_000
ORDINARY_INPUT_EXPIRY_NS = 5_000_000_000
NOT_IMPLEMENTED = [
    'product network transport / actual monitor-edge detection',
    'Qt, file managers, browsers, Wayland, INCR and large selections',
    'recovery/reconnect after peer-server death or controller death',
    'new source drag generation after restart (destination late-generation fencing is tested)',
]


def exact(report, **counts):
    for key, value in counts.items():
        check(report[key] == value, f'{key}: got {report[key]}, expected {value}; report={report}')


def responsive_status(role, **counts):
    started = time.monotonic_ns()
    report = role.command(STATUS, timeout=CONTROL_ACK_NS / 1_000_000_000)
    elapsed = time.monotonic_ns() - started
    check(0 <= elapsed < CONTROL_ACK_NS < PREPARATION_NS < ORDINARY_INPUT_EXPIRY_NS,
          f'{role.name}: control ACK over budget ({elapsed} ns)')
    exact(report, **counts)
    print(json.dumps({'result': 'PASS', 'kind': 'control_latency', 'role': role.name,
                      'elapsed_ns': elapsed, 'control_ack_budget_ns': CONTROL_ACK_NS,
                      'preparation_budget_ns': PREPARATION_NS,
                      'ordinary_input_expiry_ns': ORDINARY_INPUT_EXPIRY_NS,
                      'scope': 'test_role_control_not_product_fence'}), flush=True)


def same_payload(actual, expected):
    check(expected['length'] > 0, 'source must have a real nonempty payload')
    exact(actual, **{key: expected[key] for key in ('length', 'hash', *(f'digest{i}' for i in range(8)))})


def wait(role, predicate, label, seconds=4):
    deadline = time.monotonic() + seconds
    while True:
        left = deadline - time.monotonic()
        check(left > 0, f'{role.name}: timeout waiting for {label}; last={role.last}')
        report = role.command(STATUS, timeout=min(1, left))
        if predicate(report):
            return report
        time.sleep(min(.02, max(0, deadline - time.monotonic())))


def quiet(role, predicate, seconds=.4):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        check(predicate(role.command(STATUS)), f'{role.name}: late event during quiet period')
        time.sleep(.02)


class Group:
    def __init__(self, binary):
        self.binary, self.roles = binary, []

    def __enter__(self):
        return self

    def add(self, name, environment):
        role = Role(self.binary, name, environment)
        self.roles.append(role)
        role.ready()
        return role

    def kill(self, role):
        check(role.process.poll() is None, 'exit scenario must kill a live native role')
        role.process.kill()
        role.process.wait(timeout=2)
        check(role.process.returncode == -signal.SIGKILL, 'expected deliberate native role SIGKILL')
        role.expected_exit = -signal.SIGKILL

    def __exit__(self, *_):
        for role in reversed(self.roles):
            role.close()


def clipboard(sentinel, *roles):
    owner = sentinel.last['clip_owner']
    check(owner, 'sentinel must have a baseline X11 clipboard owner')
    for role in (sentinel, *roles):
        previous = role.last['clip_checks']
        role.command(CLIP_CHECK, owner)
        report = wait(role, lambda r: r['clip_checks'] == previous + 1, 'clipboard sentinel read')
        exact(report, clip_bad=0)
    exact(sentinel.last, clip_owner=owner, clip_bad=0)


def start(source):
    source.command(FOCUS)
    quiet(source, lambda r: r['begin'] == 0, .1)
    source.command(MOVE, source.last['x'], source.last['y'])
    source.command(DOWN)
    wait(source, lambda r: r['press'] == 1, 'real button press')
    source.command(MOVE, source.last['x'] + 60, source.last['y'])
    report = wait(source, lambda r: r['begin'] == 1, 'native drag begin')
    source.command(HELD, 0)
    return report['source_xid']


def enter(source, destination, visits=1, gets=0):
    destination.command(STATUS)  # Refresh geometry after the private WM maps/reparents.
    source.command(MOVE, destination.last['x'], destination.last['y'])
    wait(destination, lambda r: r['visits'] == visits and r['inside'] == 1, 'valid source enters')
    wait(source, lambda r: r['selected'] == COPY, 'native COPY status')
    source.command(HELD, gets)
    exact(destination.last, source_xid=source.last['source_xid'], owner=source.last['source_xid'])


def return_reenter(source, catcher, gets=0):
    leaves, visits = catcher.last['leaves'], catcher.last['visits']
    source.command(MOVE, source.last['x'], source.last['y'])
    wait(catcher, lambda r: r['leaves'] > leaves and not r['inside'], 'boundary leave')
    source.command(RETURNED, gets)
    enter(source, catcher, visits + 1, gets)


def cleanup_source(source, cancel, gets, returned=1):
    wait(source, lambda r: r['end'] == 1, 'native drag end')
    # GTK's asynchronous ungrab bookkeeping can outlive drag-end and XSync.
    wait(source, lambda r: r['toolkit_grabbed'] == 0, 'toolkit grab release', seconds=.5)
    report = source.command(FREE)
    if cancel:
        exact(report, failed=1, cancelled=1, button=1, up=0)
        source.command(UP)  # Escape must free grabs BEFORE physical ButtonUp.
    wait(source, lambda r: r['raw_up'] == 1, 'raw button release')
    quiet(source, lambda r: r['begin'] == 1 and r['end'] == 1 and r['get'] == gets)
    report = source.command(FREE)
    exact(report, begin=1, end=1, failed=int(cancel), cancelled=int(cancel),
          get=gets, deleted=0, press=1, down=1, up=1, raw_down=1, raw_up=1,
          key_down=int(cancel), key_up=int(cancel), button=0, returned=returned)


def evidence(name, **roles):
    # Full numeric reports remain available for audit; no payload bytes are logged.
    reports = {}
    for label, role in roles.items():
        code = role.process.poll()
        check(code == role.expected_exit, f'{label}: unexpected role exit {code}')
        reports[label] = {'pid': role.process.pid, 'exit_code': code,
                          'snapshot_before_deliberate_exit': code is not None, 'report': role.last}
    print(json.dumps({'scenario': name, 'result': 'PASS', 'evidence': reports}, sort_keys=True), flush=True)


def same_server(binary, env, cancel=False, slow=False):
    name = 'same-server-return-reenter-' + ('escape' if cancel else 'slow-drop' if slow else 'drop')
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source, catcher, target = (group.add(name, env) for name in ('source', 'catcher', 'target'))
        clipboard(sentinel, source, catcher, target)
        if slow:
            source.command(DELAY, 1500)
        xid = start(source)
        catcher.command(ARM, xid); target.command(ARM, xid)
        enter(source, catcher)
        return_reenter(source, catcher)
        enter(source, target)
        if cancel:
            source.command(ESCAPE)
        else:
            source.command(UP)
            if slow:
                wait(source, lambda r: r['slow_requests'] == 1, 'real delayed SelectionRequest')
                # Both GTK main loops must ACK while the real selection is outstanding.
                responsive_status(source, slow_sent=0, end=0)
                responsive_status(target, pending=1, received=0)
            result = wait(target, lambda r: r['received'] == 1, 'target reads real selection')
            exact(result, drop=1, stale=0)
            same_payload(result, source.last)
        cleanup_source(source, cancel, int(not cancel))
        quiet(target, lambda r: r['drop'] == int(not cancel) and r['received'] == int(not cancel))
        exact(catcher.command(STATUS), visits=2, drop=0, received=0)
        if slow:
            exact(source.last, slow_requests=1, slow_sent=1, slow_abandoned=0)
        clipboard(sentinel, source, catcher, target)
        evidence(name, source=source, catcher=catcher, target=target, sentinel=sentinel)


def late_generation(binary, env):
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source, catcher = (group.add(name, env) for name in ('source', 'catcher'))
        clipboard(sentinel, source, catcher)
        source.command(DELAY, 1800)
        catcher.command(ARM, start(source))
        enter(source, catcher)
        exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
        wait(source, lambda r: r['slow_requests'] == 1, 'real slow selection pending')
        responsive_status(source, slow_sent=0, end=0)
        source.command(HELD, 1)
        source.command(MOVE, source.last['x'], source.last['y'])
        wait(catcher, lambda r: r['leaves'] == 1, 'leave during slow selection')
        source.command(RETURNED, 1)
        catcher.command(DISARM)
        catcher.command(ARM, source.last['source_xid'], generation=2)
        # Late command is rejected without changing destination state.
        exact(catcher.command(READ_SELECTION, generation=1, expected=1), gen=2, pending=1)
        report = wait(catcher, lambda r: r['stale'] == 1 and r['pending'] == 0, 'late native reply fenced')
        exact(report, received=0, drop=0, length=0, hash=0)
        catcher.command(FETCH, expected=2)
        exact(source.command(STATUS), slow_sent=1)
        source.command(HELD, 1)
        source.command(ESCAPE)
        cleanup_source(source, True, 1)
        clipboard(sentinel, source, catcher)
        evidence('same-server-slow-selection-late-generation', source=source, catcher=catcher, sentinel=sentinel)


def process_exit(binary, env, which):
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source, catcher, target = (group.add(name, env) for name in ('source', 'catcher', 'target'))
        clipboard(sentinel, source, catcher, target)
        xid = start(source)
        catcher.command(ARM, xid); target.command(ARM, xid)
        enter(source, catcher)
        return_reenter(source, catcher)
        enter(source, target)
        if which == 'target':
            group.kill(target)
            source.command(MOVE, source.last['x'], source.last['y'])
            source.command(HELD, 0)
            source.command(ESCAPE)
            cleanup_source(source, True, 0)
            clipboard(sentinel, source, catcher)
        else:
            exact(source.command(STATUS), button=1, begin=1, end=0, get=0, up=0)
            group.kill(source)
            wait(target, lambda r: r['owner'] == 0, 'dead source loses XdndSelection')
            exact(target.command(FREE), button=1, drop=0, received=0)
            target.command(ORPHAN_UP)
            wait(target, lambda r: r['raw_up'] == 1 and r['button'] == 0, 'orphan input cleanup')
            target.command(FREE)
            quiet(target, lambda r: r['drop'] == 0 and r['received'] == 0)
            clipboard(sentinel, catcher, target)
        evidence(f'same-server-{which}-exit-held', source=source, catcher=catcher,
                 target=target, sentinel=sentinel)


def dual_server(binary, env, peer, cancel=False):
    with Group(binary) as group:
        sentinel_a, sentinel_b = group.add('sentinel', env), group.add('sentinel', peer)
        source, catcher = group.add('source', env), group.add('catcher', env)
        proxy, target = group.add('proxy', peer), group.add('target', peer)
        clipboard(sentinel_a, source, catcher); clipboard(sentinel_b, proxy, target)
        source.command(DELAY, 1000)
        catcher.command(ARM, start(source))
        enter(source, catcher)
        return_reenter(source, catcher)
        exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
        wait(source, lambda r: r['slow_requests'] == 1, 'source server real selection request')
        responsive_status(source, slow_sent=0, end=0)
        source.command(HELD, 1)
        received = wait(catcher, lambda r: r['received'] == 1, 'catcher real selection read')
        exact(received, drop=0)
        same_payload(received, source.last)
        payload = catcher.command(FETCH)
        # Sole body bridge: FETCH is selection-derived, and LOAD is proxy-only.
        loaded = proxy.command(LOAD, len(payload), *payload)
        same_payload(loaded, received)
        target.command(ARM, start(proxy))
        enter(proxy, target)
        source.command(HELD, 1)  # Both servers now have simultaneously live native grabs.
        return_reenter(source, catcher, gets=1)
        proxy.command(HELD, 0)
        if cancel:
            proxy.command(ESCAPE)
        else:
            proxy.command(UP)
            result = wait(target, lambda r: r['received'] == 1, 'peer target real proxy selection read')
            exact(result, drop=1)
            same_payload(result, received)
        cleanup_source(proxy, cancel, int(not cancel), returned=0)
        source.command(HELD, 1)  # Proxy completion must not release/restart original source.
        exact(catcher.command(STATUS), visits=3, received=1, drop=0, stale=0)
        source.command(ESCAPE)
        cleanup_source(source, True, 1, returned=2)
        quiet(target, lambda r: r['drop'] == int(not cancel) and r['received'] == int(not cancel))
        clipboard(sentinel_a, source, catcher); clipboard(sentinel_b, proxy, target)
        evidence('dual-server-test-bridge-' + ('escape' if cancel else 'copy-drop'),
                 source=source, catcher=catcher, proxy=proxy, target=target,
                 sentinel_a=sentinel_a, sentinel_b=sentinel_b)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--roles', required=True, type=Path, help='absolute compiled role binary')
    parser.add_argument('--suite', choices=('same-server', 'dual-server', 'all'), default='same-server')
    args = parser.parse_args(argv)
    try:
        check(args.roles.is_absolute() and args.roles.is_file() and os.access(args.roles, os.X_OK),
              '--roles must name an existing absolute executable')
        env = isolated_environment(dict(os.environ))
        if args.suite in ('same-server', 'all'):
            same_server(args.roles, env)
            same_server(args.roles, env, cancel=True)
            same_server(args.roles, env, slow=True)
            late_generation(args.roles, env)
            process_exit(args.roles, env, 'target')
            process_exit(args.roles, env, 'source')
        if args.suite in ('dual-server', 'all'):
            peer = peer_environment(env, os.environ.get('OINK_NATIVE_TEST_PEER_ENV'))
            dual_server(args.roles, env, peer)
            dual_server(args.roles, env, peer, cancel=True)
        print(json.dumps({'result': 'IMPLEMENTED_SCENARIOS_PASS', 'scope': args.suite,
                          'blocked_unimplemented': NOT_IMPLEMENTED}), flush=True)
        return 0
    except Blocked as error:
        print(json.dumps({'result': 'BLOCKED', 'reason': str(error)}), flush=True)
        return 2
    except (Failure, OSError, ValueError, subprocess.SubprocessError) as error:
        print(json.dumps({'result': 'FAIL', 'reason': str(error)}), flush=True)
        return 1


if __name__ == '__main__':
    sys.exit(main())
