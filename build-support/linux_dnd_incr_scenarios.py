#!/usr/bin/env python3
"""INCR and binary-stream payload acceptance, only inside linux-native-test.py.

Build externally (no auto-install or auto-compilation); pass --roles /absolute/binary.
Bodies above INCR_THRESHOLD are dragged source -> catcher (INCR receive evidence:
handshake/chunks/bytes/done), relayed catcher -> controller -> proxy over the
inherited-FD stream channel, then dragged proxy -> target (INCR sender evidence:
get/incr_sent/incr_sent_bytes). Importing is GUI-free; no payload bytes are logged.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from linux_dnd_protocol import (
    ARM, ESCAPE, LOAD_STREAM, PAYLOAD_MAX, READ_SELECTION, UP, Blocked, Failure,
    bridge_payload, check, isolated_environment, stream_feed,
)
from linux_dnd_scenarios import (
    Group, clipboard, cleanup_source, enter, evidence, exact, responsive_status,
    same_payload, start, wait,
)

INCR_THRESHOLD = 256 * 1024


def patterned(size):
    return bytes(((i * 31 + 7) & 0xff) for i in range(size))


def incr_evidence(report, size):
    exact(report, length=size, incr_handshake=1, incr_done=1, incr_bytes=size)
    check(report['incr_chunks'] >= 2, f'INCR chunking absent: {report}')


def incr_source_to_catcher(binary, env, size):
    name = f'incr-source-catcher-{size}'
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source, catcher = group.add('source', env), group.add('catcher', env)
        clipboard(sentinel, source, catcher)
        stream_feed(source, patterned(size))
        catcher.command(ARM, start(source))
        enter(source, catcher)
        exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
        result = wait(catcher, lambda r: r['received'] == 1, 'catcher INCR read', seconds=15)
        exact(result, drop=0, stale=0, received=1)
        incr_evidence(result, size)
        same_payload(result, source.last)
        source.command(ESCAPE)
        report = wait(source, lambda r: r['end'] == 1, 'source drag end')
        exact(report, get=1, incr_sent_bytes=size, incr_abandoned=0)
        check(report['incr_sent'] >= 2, f'INCR sender chunking absent: {report}')
        cleanup_source(source, True, 1, returned=0)
        responsive_status(source)
        clipboard(sentinel, source, catcher)
        evidence(name, source=source, catcher=catcher, sentinel=sentinel)


def incr_source_catcher_bridge(binary, env, size):
    name = f'incr-source-catcher-bridge-{size}'
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source, catcher = group.add('source', env), group.add('catcher', env)
        clipboard(sentinel, source, catcher)
        stream_feed(source, patterned(size))
        catcher.command(ARM, start(source))
        enter(source, catcher)
        exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
        received = wait(catcher, lambda r: r['received'] == 1, 'catcher INCR read', seconds=15)
        exact(received, drop=0, stale=0)
        incr_evidence(received, size)
        same_payload(received, source.last)
        source.command(ESCAPE)
        report = wait(source, lambda r: r['end'] == 1, 'source drag end')
        exact(report, get=1, incr_sent_bytes=size)
        cleanup_source(source, True, 1, returned=0)
        # The proxy window shares the source slot; mapping it only after the
        # source drag avoids an unreliable stacking raise (the bridge itself
        # needs no window interaction).
        proxy = group.add('proxy', env)
        loaded = bridge_payload(catcher, proxy, timeout=15)
        check(loaded['length'] == size and loaded['bridge_bytes'] == size
              and not loaded['bridge_bad'], 'bridge ingest')
        same_payload(loaded, received)
        responsive_status(proxy)
        clipboard(sentinel, source, catcher, proxy)
        evidence(name, source=source, catcher=catcher, proxy=proxy, sentinel=sentinel)


def incr_proxy_to_target(binary, env, size):
    name = f'incr-proxy-target-{size}'
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        proxy, target = group.add('proxy', env), group.add('target', env)
        clipboard(sentinel, proxy, target)
        stream_feed(proxy, patterned(size))
        target.command(ARM, start(proxy))
        enter(proxy, target)
        proxy.command(UP)
        result = wait(target, lambda r: r['received'] == 1, 'target INCR read', seconds=15)
        exact(result, drop=1, stale=0)
        incr_evidence(result, size)
        same_payload(result, proxy.last)
        report = wait(proxy, lambda r: r['end'] == 1, 'proxy drag end')
        exact(report, get=1, incr_sent_bytes=size, incr_abandoned=0)
        cleanup_source(proxy, False, 1, returned=0)
        responsive_status(proxy)
        clipboard(sentinel, proxy, target)
        evidence(name, proxy=proxy, target=target, sentinel=sentinel)


def stream_rejects_oversize_and_stale(binary, env):
    with Group(binary) as group:
        source = group.add('source', env)
        deadline = time.monotonic() + 5
        seq = source.send_wire(LOAD_STREAM, PAYLOAD_MAX + 1,
                               source.last['epoch'], source.generation)
        status, report, body = source.receive(seq, deadline)
        check(status == 2 and body is None, f'oversize LOAD_STREAM status {status}')
        check(report['length'] == 48 and not report['bridge_bad'], 'oversize disturbed role')
        seq = source.send_wire(LOAD_STREAM, 64, source.last['epoch'], source.generation,
                               generation=source.generation + 5)
        status, _, _ = source.receive(seq, deadline)
        check(status == 1, f'stale-generation LOAD_STREAM status {status}')
        evidence('stream-load-rejects-oversize-and-stale', source=source)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--roles', required=True, type=Path, help='absolute compiled role binary')
    args = parser.parse_args(argv)
    try:
        check(args.roles.is_absolute() and args.roles.is_file() and os.access(args.roles, os.X_OK),
              '--roles must name an existing absolute executable')
        env = isolated_environment(dict(os.environ))
        incr_source_to_catcher(args.roles, env, 1024 * 1024 + 123)
        incr_source_catcher_bridge(args.roles, env, INCR_THRESHOLD + 65536 + 7)
        incr_proxy_to_target(args.roles, env, 1024 * 1024 + 123)
        stream_rejects_oversize_and_stale(args.roles, env)
        print(json.dumps({'result': 'INCR_SCENARIOS_PASS'}), flush=True)
        return 0
    except Blocked as error:
        print(json.dumps({'result': 'BLOCKED', 'reason': str(error)}), flush=True)
        return 2
    except (Failure, OSError, ValueError, subprocess.SubprocessError) as error:
        import traceback
        print(json.dumps({'result': 'FAIL', 'reason': str(error),
                          'traceback': traceback.format_exc()}), flush=True)
        return 1


if __name__ == '__main__':
    sys.exit(main())
