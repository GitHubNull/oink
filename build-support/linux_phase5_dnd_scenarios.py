#!/usr/bin/env python3
"""Phase 5 XDND gate: the product backend as catcher and as source against the
native GTK role binary.

Run only inside build-support/linux-native-test.py so DISPLAY, HOME and every
selection belong to a disposable Xephyr session. Build the probe first:

  cargo build --locked -p oink-clipboard --example phase5_dnd_probe

Payloads stay synthetic; assertions use lengths, SHA-256 and FNV-1a words only.
The GTK target role fences payload reads behind the _OINK_TEST_DND_GENERATION
property; the probe (the drag source peer) sets it exactly like a test bridge.
"""
import argparse
import hashlib
import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time

from linux_dnd_protocol import (
    ARM,
    DOWN,
    ESCAPE,
    FOCUS,
    FREE,
    HELD,
    MOVE,
    STATUS,
    UP,
    Failure,
    check,
    isolated_environment,
)
from linux_dnd_scenarios import Group, clipboard, evidence, exact, wait

# Product catcher parks on empty desktop; product source starts over the
# target role window (900,220 + 230x220, centered).
CATCHER_POINT = (640, 500)
RESUME_POINT = (700, 520)
TARGET_POINT = (1015, 330)
TEXT = b"oink-phase5-dnd-text"
INCR_SIZE = 150_000
PREPARATION_NS = 3_000_000_000

PROBE = None
ROLES = None
CLIENTS = []


def fnv1a(data):
    value = 2166136261
    for byte in data:
        value = ((value ^ byte) * 16777619) & 0xFFFFFFFF
    return value


def digest_words(sha256_hex):
    """Split a SHA-256 hex string into the eight big-endian u32 role words."""
    raw = bytes.fromhex(sha256_hex)
    check(len(raw) == 32, f'sha256 hex must be 32 bytes: {sha256_hex}')
    return [int.from_bytes(raw[offset:offset + 4], 'big') for offset in range(0, 32, 4)]


def summarize_probe_events(events):
    """Fold probe JSON lines into per-event counts and per-operation outcomes."""
    summary = {'counts': {}, 'operations': {}}
    for event in events:
        name = event.get('event')
        if not isinstance(name, str):
            raise Failure(f'malformed probe event: {event}')
        summary['counts'][name] = summary['counts'].get(name, 0) + 1
        operation = event.get('operation')
        if operation is not None:
            outcomes = summary['operations'].setdefault(str(operation), {})
            for key in ('accepted', 'dropped'):
                if key in event:
                    outcomes[key] = event[key]
    return summary


class Probe:
    """Product probe process: JSON events on stdout, JSON commands on stdin."""

    def __init__(self, tag, *args):
        self.tag = tag
        self.process = subprocess.Popen([PROBE, *args], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, bufsize=1, start_new_session=True)
        self.lines = queue.Queue()
        self.errors = []
        self.stash = {}
        CLIENTS.append(self)
        threading.Thread(target=self.pump, daemon=True).start()
        threading.Thread(target=self.pump_errors, daemon=True).start()

    def pump_errors(self):
        for line in self.process.stderr:
            self.errors.append(line)

    def pump(self):
        for line in self.process.stdout:
            line = line.strip()
            if line.startswith('{'):
                value = json.loads(line)
                print(f'{self.tag} {json.dumps(value, sort_keys=True)}', flush=True)
                self.lines.put(value)
        self.lines.put(None)

    def event(self, name, timeout=15):
        # Backend replies and asynchronous events can arrive in either order
        # (a reject reports source-ended before the finish command acks), so
        # unmatched events are stashed by name instead of dropped.
        deadline = time.monotonic() + timeout
        while True:
            if self.stash.get(name):
                return self.stash[name].pop(0)
            left = deadline - time.monotonic()
            if left <= 0:
                raise Failure(f'{self.tag}: timeout waiting for {name}')
            try:
                value = self.lines.get(timeout=left)
            except queue.Empty:
                raise Failure(f'{self.tag}: timeout waiting for {name}') from None
            if value is None:
                raise Failure(f'{self.tag}: exited waiting for {name}: {self.stderr()}')
            if value.get('event') == 'error':
                raise Failure(f'{self.tag}: {value}')
            if value.get('event') == name:
                return value
            self.stash.setdefault(value.get('event'), []).append(value)

    def quiet(self, seconds=.4):
        if self.stash:
            raise Failure(f'{self.tag}: unexpected late stashed event {self.stash}')
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                value = self.lines.get(timeout=max(0.0, deadline - time.monotonic()))
            except queue.Empty:
                return
            if value is not None:
                raise Failure(f'{self.tag}: unexpected late event {value}')

    def send(self, command):
        self.process.stdin.write(json.dumps(command) + '\n')
        self.process.stdin.flush()

    def finish(self, timeout=10):
        try:
            code = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            raise Failure(f'{self.tag}: did not exit: {self.stderr()}') from None
        check(code == 0, f'{self.tag}: exit {code}: {self.stderr()}')

    def stop(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)

    def stderr(self):
        return ''.join(self.errors).strip()


def env():
    return isolated_environment(dict(os.environ))


def text_item(events):
    items = [item for item in events.get('items', []) if item.get('kind') == 'text']
    check(items, f'no text item in detected event: {events}')
    return items[0]


def cleanup_source(source):
    wait(source, lambda r: r['end'] == 1, 'native drag end')
    wait(source, lambda r: r['toolkit_grabbed'] == 0, 'toolkit grab release', seconds=.5)
    source.command(FREE)
    # The product finish already replayed the button release (raw_up advanced
    # once); the explicit UP here is the user's physical release on top.
    before = source.last['raw_up']
    source.command(UP)
    wait(source, lambda r: r['raw_up'] == before + 1, 'raw button release')


def start_source_drag(source):
    """Mirror linux_dnd_scenarios.start: focus, press, begin a real GTK drag."""
    source.command(FOCUS)
    source.command(MOVE, source.last['x'], source.last['y'])
    source.command(DOWN)
    wait(source, lambda r: r['press'] == 1, 'real button press')
    source.command(MOVE, source.last['x'] + 60, source.last['y'])
    report = wait(source, lambda r: r['begin'] == 1, 'native drag begin')
    source.command(HELD, 0)
    return report['source_xid']


def enter_probe_catcher(source):
    source.command(STATUS)  # window cache refresh after the probe catcher maps
    started = time.monotonic_ns()
    source.command(MOVE, *CATCHER_POINT)
    return started


def scenario_catcher_text_drop():
    with Group(ROLES) as group:
        sentinel = group.add('sentinel', env())
        source = group.add('source', env())
        clipboard(sentinel, source)
        probe = Probe('probe', 'catcher', '--x', str(CATCHER_POINT[0]), '--y', str(CATCHER_POINT[1]))
        probe.event('ready')
        start_source_drag(source)
        started = enter_probe_catcher(source)
        detected = probe.event('detected')
        elapsed = time.monotonic_ns() - started
        check(0 < elapsed < PREPARATION_NS, f'detected over 3s preparation budget: {elapsed} ns')
        print(json.dumps({'result': 'PASS', 'kind': 'preparation_latency',
                          'elapsed_ns': elapsed, 'preparation_budget_ns': PREPARATION_NS,
                          'scope': 'product_eager_capture'}), flush=True)
        wait(source, lambda r: r['get'] == 3, 'source served UTF8_STRING, text/plain and uri-list')
        item = text_item(detected)
        check(item['length'] == source.last['length'], f"length: {item} vs {source.last['length']}")
        expected_words = [source.last[f'digest{i}'] for i in range(8)]
        check(digest_words(item['sha256']) == expected_words,
              f"payload digest mismatch: {item['sha256']} vs {expected_words}")
        probe.send({'cmd': 'hold'})
        held = probe.event('held')
        check(held['operation'] == detected['operation'] and held['surface'] > 0, held)
        source.command(HELD, 3)
        probe.send({'cmd': 'finish', 'accept': True})
        ack = probe.event('finish-ack')
        check(ack['ok'] and ack['accept'], ack)
        ended = probe.event('source-ended')
        check(ended['operation'] == detected['operation'] and ended['accepted'], ended)
        cleanup_source(source)
        check(source.last['failed'] == 0, f"COPY drop failed: {source.last}")
        clipboard(sentinel, source)
        probe.send({'cmd': 'quit'})
        probe.finish()
        evidence('probe-catcher-text-drop', source=source, sentinel=sentinel)


def scenario_catcher_reject():
    with Group(ROLES) as group:
        sentinel = group.add('sentinel', env())
        source = group.add('source', env())
        clipboard(sentinel, source)
        probe = Probe('probe', 'catcher', '--x', str(CATCHER_POINT[0]), '--y', str(CATCHER_POINT[1]))
        probe.event('ready')
        start_source_drag(source)
        enter_probe_catcher(source)
        detected = probe.event('detected')
        probe.send({'cmd': 'hold'})
        probe.event('held')
        source.command(HELD, 3)
        probe.send({'cmd': 'finish', 'accept': False})
        ack = probe.event('finish-ack')
        check(ack['ok'] and not ack['accept'], ack)
        ended = probe.event('source-ended')
        check(ended['operation'] == detected['operation'] and not ended['accepted'], ended)
        cleanup_source(source)
        check(source.last['failed'] == 1, f'reject must fail the drag: {source.last}')
        clipboard(sentinel, source)
        probe.send({'cmd': 'quit'})
        probe.finish()
        evidence('probe-catcher-reject', source=source, sentinel=sentinel)


def scenario_catcher_cancel():
    with Group(ROLES) as group:
        sentinel = group.add('sentinel', env())
        source = group.add('source', env())
        clipboard(sentinel, source)
        probe = Probe('probe', 'catcher', '--x', str(CATCHER_POINT[0]), '--y', str(CATCHER_POINT[1]))
        probe.event('ready')
        start_source_drag(source)
        enter_probe_catcher(source)
        detected = probe.event('detected')
        probe.send({'cmd': 'hold'})
        probe.event('held')
        source.command(HELD, 3)
        source.command(ESCAPE)
        lost = probe.event('catcher-lost')
        check(lost['operation'] == detected['operation'], lost)
        wait(source, lambda r: r['end'] == 1, 'native drag end after Escape')
        check(source.last['cancelled'] == 1, f'Escape must cancel: {source.last}')
        source.command(FREE)
        source.command(UP)
        wait(source, lambda r: r['raw_up'] == 1, 'raw button release')
        clipboard(sentinel, source)
        probe.send({'cmd': 'quit'})
        probe.finish()
        evidence('probe-catcher-cancel', source=source, sentinel=sentinel)


def scenario_catcher_hold_resume():
    with Group(ROLES) as group:
        sentinel = group.add('sentinel', env())
        source = group.add('source', env())
        clipboard(sentinel, source)
        probe = Probe('probe', 'catcher', '--x', str(CATCHER_POINT[0]), '--y', str(CATCHER_POINT[1]))
        probe.event('ready')
        start_source_drag(source)
        enter_probe_catcher(source)
        detected = probe.event('detected')
        probe.send({'cmd': 'hold'})
        probe.event('held')
        source.command(HELD, 3)
        source.command(MOVE, *RESUME_POINT)
        probe.quiet(.4)  # fullscreen catcher keeps GDK's hit test on us
        probe.send({'cmd': 'resume', 'x': RESUME_POINT[0], 'y': RESUME_POINT[1]})
        resumed = probe.event('resumed')
        check(resumed['ok'], resumed)
        source.command(MOVE, *RESUME_POINT)
        probe.send({'cmd': 'finish', 'accept': True})
        probe.event('finish-ack')
        ended = probe.event('source-ended')
        check(ended['operation'] == detected['operation'] and ended['accepted'], ended)
        cleanup_source(source)
        clipboard(sentinel, source)
        probe.send({'cmd': 'quit'})
        probe.finish()
        evidence('probe-catcher-hold-resume', source=source, sentinel=sentinel)


def scenario_catcher_generation():
    with Group(ROLES) as group:
        sentinel = group.add('sentinel', env())
        clipboard(sentinel)
        probe = Probe('probe', 'catcher', '--x', str(CATCHER_POINT[0]), '--y', str(CATCHER_POINT[1]))
        probe.event('ready')
        first_digest = second_digest = None
        for accept in (False, True):
            source = group.add('source', env())
            clipboard(sentinel, source)
            start_source_drag(source)
            enter_probe_catcher(source)
            detected = probe.event('detected')
            item = text_item(detected)
            if first_digest is None:
                first_digest = item['sha256']
            else:
                second_digest = item['sha256']
            probe.send({'cmd': 'hold'})
            probe.event('held')
            source.command(HELD, 3)
            probe.send({'cmd': 'finish', 'accept': accept})
            probe.event('finish-ack')
            ended = probe.event('source-ended')
            check(ended['operation'] == detected['operation'], ended)
            check(ended['accepted'] == accept, ended)
            cleanup_source(source)
            clipboard(sentinel, source)
        check(first_digest and second_digest and first_digest != second_digest,
              'two sessions must carry distinct payloads')
        probe.send({'cmd': 'quit'})
        probe.finish()
        print(json.dumps({'result': 'PASS', 'kind': 'generation_regression',
                          'sessions': 2, 'distinct_payloads': first_digest != second_digest,
                          'scope': 'product_session_isolation'}), flush=True)
        evidence('probe-catcher-generation', sentinel=sentinel)


def scenario_source_drop(size=None, name='probe-source-text-drop'):
    generation = 7
    with Group(ROLES) as group:
        sentinel = group.add('sentinel', env())
        target = group.add('target', env())
        clipboard(sentinel, target)
        args = ('source', '--x', str(TARGET_POINT[0]), '--y', str(TARGET_POINT[1]), '--gen', str(generation))
        if size is not None:
            args += ('--size', str(size))
        probe = Probe('probe', *args)
        ack = probe.event('begin-ack')
        check(ack['selection_owner'] and ack['generation'] == generation, ack)
        target.command(ARM, ack['selection_owner'], generation=generation)
        probe.send({'cmd': 'move', 'x': TARGET_POINT[0] + 1, 'y': TARGET_POINT[1]})
        report = wait(target, lambda r: r['visits'] == 1 and r['motions'] >= 1,
                      'armed target accepts the product source')
        probe.send({'cmd': 'release'})
        ended = probe.event('ended')
        check(ended['dropped'], ended)
        report = wait(target, lambda r: r['received'] == 1, 'target read the XdndSelection')
        exact(report, drop=1, stale=0)
        expected = TEXT if size is None else bytes(97 + index % 26 for index in range(size))
        sha256_hex = hashlib.sha256(expected).hexdigest()
        check(report['length'] == len(expected), f"target length {report['length']} != {len(expected)}")
        expected_words = digest_words(sha256_hex)
        check([report[f'digest{i}'] for i in range(8)] == expected_words,
              f'target payload digest mismatch: {report} vs {expected_words}')
        check(report['hash'] == fnv1a(expected), f"target FNV mismatch: {report['hash']}")
        target.command(FREE)
        clipboard(sentinel, target)
        probe.finish()
        evidence(name, target=target, sentinel=sentinel)


def scenario_source_cancel():
    generation = 9
    with Group(ROLES) as group:
        sentinel = group.add('sentinel', env())
        target = group.add('target', env())
        clipboard(sentinel, target)
        probe = Probe('probe', 'source', '--x', str(TARGET_POINT[0]), '--y', str(TARGET_POINT[1]),
                      '--gen', str(generation))
        ack = probe.event('begin-ack')
        target.command(ARM, ack['selection_owner'], generation=generation)
        probe.send({'cmd': 'move', 'x': TARGET_POINT[0] + 1, 'y': TARGET_POINT[1]})
        wait(target, lambda r: r['visits'] == 1, 'armed target accepts the product source')
        probe.send({'cmd': 'cancel'})
        cancel = probe.event('cancel-ack')
        check(cancel['ok'], cancel)
        ended = probe.event('ended')
        check(not ended['dropped'], ended)
        target.command(FREE)
        report = target.command(STATUS)
        exact(report, drop=0, received=0, stale=0)
        clipboard(sentinel, target)
        probe.finish()
        evidence('probe-source-cancel', target=target, sentinel=sentinel)


SCENARIOS = [
    ('catcher-text-drop', scenario_catcher_text_drop),
    ('catcher-reject', scenario_catcher_reject),
    ('catcher-cancel', scenario_catcher_cancel),
    ('catcher-hold-resume', scenario_catcher_hold_resume),
    ('catcher-generation', scenario_catcher_generation),
    ('source-text-drop', lambda: scenario_source_drop()),
    ('source-incr-drop', lambda: scenario_source_drop(size=INCR_SIZE, name='probe-source-incr-drop')),
    ('source-cancel', scenario_source_cancel),
]


def main():
    global PROBE, ROLES
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', required=True, type=str)
    parser.add_argument('--roles', required=True, type=str)
    parser.add_argument('--only', action='append', default=None)
    args = parser.parse_args()
    PROBE, ROLES = args.probe, args.roles
    selected = [item for item in SCENARIOS if not args.only or item[0] in args.only]
    check(selected, f'no scenario matched {args.only}')
    failures = 0
    try:
        for name, function in selected:
            started = time.monotonic()
            mark = len(CLIENTS)
            try:
                function()
            except (Failure, subprocess.SubprocessError, OSError, ValueError, KeyError) as error:
                failures += 1
                print(json.dumps({'result': 'FAIL', 'scenario': name, 'detail': str(error)},
                                 sort_keys=True), flush=True)
            else:
                print(json.dumps({'result': 'PASS', 'scenario': name,
                                  'seconds': round(time.monotonic() - started, 3)},
                                 sort_keys=True), flush=True)
            finally:
                # A failed scenario must not leave its probe backend (and its
                # mapped XdndAware catcher windows) on the shared display for
                # the next scenario's drag to find.
                for client in CLIENTS[mark:]:
                    client.stop()
                del CLIENTS[mark:]
    finally:
        for client in CLIENTS:
            client.stop()
    print(json.dumps({'result': 'FAIL' if failures else 'PASS', 'scenarios': len(selected),
                      'failures': failures}, sort_keys=True), flush=True)
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
