#!/usr/bin/env python3
"""Phase 5 clipboard gate: product X11 backend against a raw Xlib peer.

Run only inside build-support/linux-native-test.py so DISPLAY, HOME and the
clipboard belong to a disposable Xephyr session. Build both binaries first:

  cargo build --locked -p oink-clipboard --example phase5_clipboard_probe
  cc -O2 -Wall -Wextra -Werror -std=c11 build-support/linux_phase5_selection_peer.c \
     -o <out> $(pkg-config --cflags --libs x11)

Payloads stay synthetic; assertions use lengths and SHA-256 only.
"""
import argparse
import hashlib
import json
import queue
import signal
import subprocess
import sys
import threading
import time

TEXT = b"oink-phase5-text"
HTML = b"<p>oink-phase5-html</p>"
RTF = b"{\\rtf1 oink-phase5}"
PNG = bytes([
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 0x0A,
    0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0, 0, 0, 0, 2, 0, 1, 0x0D, 0x0A, 0x2D, 0xB4,
    0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
])

PROBE = None
PEER = None
CLIENTS = []


class Failure(Exception):
    pass


def check(condition, message):
    if not condition:
        raise Failure(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def pattern(length):
    return bytes(97 + index % 26 for index in range(length))


def evidence(tag, value):
    print(f'{tag} {json.dumps(value, sort_keys=True)}', flush=True)


class Client:
    def __init__(self, tag, command):
        self.tag = tag
        self.process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, bufsize=1, start_new_session=True)
        self.lines = queue.Queue()
        self.errors = []
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
                evidence(self.tag, value)
                self.lines.put(value)
        self.lines.put(None)

    def event(self, name, timeout=15):
        deadline = time.monotonic() + timeout
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                raise Failure(f'{self.tag}: timeout waiting for {name}')
            try:
                value = self.lines.get(timeout=left)
            except queue.Empty:
                raise Failure(f'{self.tag}: timeout waiting for {name}') from None
            if value is None:
                raise Failure(f'{self.tag}: exited while waiting for {name}: {self.stderr()}')
            if value.get('event') == 'error':
                raise Failure(f'{self.tag}: {value}')
            if value.get('event') == name:
                return value

    def finish(self, timeout=30):
        try:
            code = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            raise Failure(f'{self.tag}: did not exit: {self.stderr()}') from None
        check(code == 0, f'{self.tag}: exit {code}: {self.stderr()}')
        return self

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


def probe(*args, timeout=40):
    result = subprocess.run([PROBE, *args], capture_output=True, text=True, timeout=timeout)
    lines = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{')]
    for line in lines:
        evidence('probe', line)
    check(result.returncode == 0,
          f'probe {args} exited {result.returncode}: {result.stderr.strip()} {lines}')
    return lines


def peer(*args, timeout=30):
    result = subprocess.run([PEER, *args], capture_output=True, text=True, timeout=timeout)
    lines = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{')]
    for line in lines:
        evidence('peer', line)
    if result.returncode != 0:
        raise Failure(f'peer {args} exited {result.returncode}: {result.stderr.strip()} {lines}')
    return lines


def request(target, expect='text', size=None):
    args = ['request', '--target', target, '--expect', expect]
    if size is not None:
        args += ['--size', str(size)]
    return peer(*args)[-1]


def owner(kind, hold_ms, size=None):
    args = ['owner', '--kind', kind, '--hold-ms', str(hold_ms)]
    if size is not None:
        args += ['--size', str(size)]
    client = Client('peer', [PEER, *args])
    client.event('peer-ready')
    return client


def capture_reads(*extra):
    lines = probe('capture', '--read', *extra)
    captured = next(line for line in lines if line['event'] == 'capture')
    reads = [(line['length'], line['sha256']) for line in lines
             if line['event'] == 'read' and line['ok']]
    check(all(line['event'] in ('capture', 'read', 'exit') for line in lines),
          f'unexpected capture events: {lines}')
    return captured, reads


def scenario_text_serve():
    client = Client('probe', [PROBE, 'serve', '--hold-ms', '8000'])
    written = client.event('write')
    check(written['length'] == len(TEXT) and written['sha256'] == digest(TEXT), written)
    check(written['count'] == 1 and written['change_count'] == 1, written)
    targets = request('TARGETS')
    check(targets['utf8'] and targets['string'] and targets['timestamp'] and targets['multiple'],
          f'TARGETS missing mandatory targets: {targets}')
    first = request('TIMESTAMP')
    check(first['equal'] and first['positive'], f'TIMESTAMP is not stable: {first}')
    text = request('UTF8_STRING', 'text')
    check(text['match'] and not text['incr'] and text['length'] == len(TEXT), text)
    latin = request('STRING', 'text')
    check(latin['match'] and not latin['incr'], latin)
    multiple = peer('multiple')[-1]
    check(multiple['text'] and multiple['string'], f'MULTIPLE pair properties: {multiple}')
    client.finish()


def scenario_multi_serve():
    client = Client('probe', [PROBE, 'write-multi', '--hold-ms', '8000'])
    written = client.event('write')
    check(written['text']['sha256'] == digest(TEXT), written)
    targets = request('TARGETS')
    check(targets['html'] and targets['rtf'] and targets['png'], f'rich TARGETS: {targets}')
    for target, expect in (('text/html', 'html'), ('application/rtf', 'rtf'), ('image/png', 'png')):
        result = request(target, expect)
        check(result['match'], f'{target}: {result}')
    client.finish()


def scenario_incr_write():
    size = 150_000
    client = Client('probe', [PROBE, 'serve', '--size', str(size), '--hold-ms', '15000'])
    client.event('write')
    result = request('UTF8_STRING', 'large', size)
    check(result['incr'] and result['match'] and result['length'] == size, result)
    client.finish()


def scenario_peer_text_capture():
    client = owner('text', 6000)
    captured, reads = capture_reads()
    check('Linux' in captured['platform'], f'platform: {captured["platform"]}')
    check(captured['items'] == 1 and captured['snapshot_items'] >= 1, captured)
    check((len(TEXT), digest(TEXT)) in reads, f'text read missing: {captured} {reads}')
    client.stop()


def scenario_peer_incr_capture():
    size = 200_000
    client = owner('large', 8000, size=size)
    captured, reads = capture_reads()
    check((size, digest(pattern(size))) in reads, f'INCR read missing: {captured} {reads}')
    client.stop()


def scenario_peer_multi_capture():
    client = owner('multi', 8000)
    captured, reads = capture_reads()
    expected = [(len(TEXT), digest(TEXT)), (len(HTML), digest(HTML)),
                (len(RTF), digest(RTF)), (len(PNG), digest(PNG))]
    for item in expected:
        check(item in reads, f'rich read missing {item}: {captured} {reads}')
    client.stop()


def scenario_owner_exit_watch():
    client = owner('text', 1500)
    watch = Client('probe', [PROBE, 'watch', '--seconds', '3'])
    watch.event('watch-ready')
    done = watch.event('watch-done', timeout=10)
    check(done['events'] >= 1 and done['count'] == done['events'], done)
    watch.finish()
    client.stop()


def scenario_primary_ignored():
    client = Client('peer', [PEER, 'primary', '--hold-ms', '4000'])
    client.event('peer-ready')
    watch = Client('probe', [PROBE, 'watch', '--seconds', '1'])
    watch.event('watch-ready')
    done = watch.event('watch-done', timeout=6)
    check(done['events'] == 0 and done['count'] == 0, f'PRIMARY leaked: {done}')
    watch.finish()
    captured, _ = capture_reads()
    check(captured['items'] == 0 and captured['snapshot_items'] == 0, captured)
    client.stop()


def scenario_self_write():
    watch = Client('probe', [PROBE, 'watch', '--seconds', '1', '--write-size', '32'])
    watch.event('watch-ready')
    written = watch.event('write')
    check(written['count'] == 1 and written['length'] == 32, written)
    done = watch.event('watch-done', timeout=6)
    check(done['events'] == 0 and done['count'] == 1, f'self write looped: {done}')
    watch.finish()


def scenario_key_tracks_ownership():
    before = probe('key')[-1]['key']
    client = owner('text', 4000)
    during = probe('key')[-1]['key']
    check(during != before, 'clipboard key ignored ownership')
    client.stop()
    time.sleep(0.3)
    after = probe('key')[-1]['key']
    check(after == before, f'clipboard key did not return to unowned: {before} {after}')


SCENARIOS = [
    ('text-serve', scenario_text_serve),
    ('multi-serve', scenario_multi_serve),
    ('incr-write', scenario_incr_write),
    ('peer-text-capture', scenario_peer_text_capture),
    ('peer-incr-capture', scenario_peer_incr_capture),
    ('peer-multi-capture', scenario_peer_multi_capture),
    ('owner-exit-watch', scenario_owner_exit_watch),
    ('primary-ignored', scenario_primary_ignored),
    ('self-write', scenario_self_write),
    ('key-tracks-ownership', scenario_key_tracks_ownership),
]


def main():
    global PROBE, PEER
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', required=True, type=str)
    parser.add_argument('--peer', required=True, type=str)
    parser.add_argument('--only', action='append', default=None)
    args = parser.parse_args()
    PROBE, PEER = args.probe, args.peer
    selected = [item for item in SCENARIOS if not args.only or item[0] in args.only]
    check(selected, f'no scenario matched {args.only}')
    failures = 0
    try:
        for name, function in selected:
            started = time.monotonic()
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
        for client in CLIENTS:
            client.stop()
    print(json.dumps({'result': 'FAIL' if failures else 'PASS', 'scenarios': len(selected),
                      'failures': failures}, sort_keys=True), flush=True)
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
