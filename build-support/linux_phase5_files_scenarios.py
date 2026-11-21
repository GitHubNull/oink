#!/usr/bin/env python3
"""Phase 5 file/privacy/delayed gate: product X11 backend against raw peers.

Run only inside build-support/linux-native-test.py so DISPLAY, HOME and the
clipboard belong to a disposable Xephyr session. Build first:

  cargo build --locked -p oink-clipboard --example phase5_files_probe
  cc -O2 -Wall -Wextra -Werror -std=c11 build-support/linux_phase5_files_peer.c \
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
import tempfile
import threading
import time
from pathlib import Path

TEXT = b"oink-phase5-text"
SECRET = b"secret"
ONE = b"oink-phase5-file-one\n"
TWO = b"oink-phase5-file-two"

PROBE = None
PEER = None
FILES_PROBE = None
FILES_PEER = None
CLIENTS = []


class Failure(Exception):
    pass


def check(condition, message):
    if not condition:
        raise Failure(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def evidence(tag, value):
    print(f'{tag} {json.dumps(value, sort_keys=True)}', flush=True)


class Client:
    def __init__(self, tag, command):
        self.tag = tag
        self.process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, bufsize=1, start_new_session=True)
        self.lines = queue.Queue()
        self.history = []
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
                self.history.append(value)
                self.lines.put(value)
        self.lines.put(None)

    def event(self, name, timeout=20):
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

    def count(self, name):
        return sum(1 for value in self.history if value.get('event') == name)

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


def run(tag, command, timeout=40):
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    lines = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{')]
    for line in lines:
        evidence(tag, line)
    check(result.returncode == 0,
          f'{tag} {command[1:]} exited {result.returncode}: {result.stderr.strip()} {lines}')
    return lines


def probe(*args, timeout=40):
    return run('probe', [PROBE, *args], timeout)


def peer(*args, timeout=30):
    return run('peer', [PEER, *args], timeout)


def files_probe(*args, timeout=60):
    return run('fprobe', [FILES_PROBE, *args], timeout)


def files_peer(*args, timeout=30):
    return run('fpeer', [FILES_PEER, *args], timeout)


def owner(kind, hold_ms):
    client = Client('peer', [PEER, 'owner', '--kind', kind, '--hold-ms', str(hold_ms)])
    client.event('peer-ready')
    return client


def owner_files(paths, hold_ms):
    client = Client('fpeer', [FILES_PEER, 'owner-files', '--paths', ';'.join(str(p) for p in paths),
                             '--hold-ms', str(hold_ms)])
    client.event('peer-ready')
    return client


def owner_secret(hold_ms):
    client = Client('fpeer', [FILES_PEER, 'owner-secret', '--hold-ms', str(hold_ms)])
    client.event('peer-ready')
    return client


def fixture():
    root = Path(tempfile.mkdtemp(prefix='phase5-files-'))
    one = root / 'one.txt'
    one.write_bytes(ONE)
    folder = root / 'dir'
    folder.mkdir()
    (folder / 'two.bin').write_bytes(TWO)
    two = root / 'two.txt'
    two.write_bytes(TWO)
    return root, one, folder, two


def scenario_peer_files_capture():
    root, one, folder, _ = fixture()
    saved_dir = root / 'saved'
    saved_dir.mkdir()
    owner_client = owner_files([one, folder], 8000)
    probe_client = Client('fprobe', [FILES_PROBE, 'capture-files', '--save', str(saved_dir)])
    captured = probe_client.event('capture')
    check(captured['items'] == 1 and captured['snapshot_items'] >= 1, captured)
    kinds = [root_item['kind'] for root_item in captured['roots']]
    check(kinds == ['file', 'dir'], f'roots: {captured}')
    check(captured['file_items'] == 1, captured)
    check('linux:text/uri-list' in captured['file_reps']
          and 'linux:x-special/gnome-copied-files' in captured['file_reps'], captured)
    first = probe_client.event('namespace-read')
    check(first['role'] == 'root' and first['sha256'] == digest(ONE), first)
    listed = probe_client.event('namespace-list')
    check(listed['entries'] == 1 and len(listed['children']) == 1, listed)
    child = probe_client.event('namespace-read')
    check(child['role'] == 'child' and child['sha256'] == digest(TWO), child)
    saved = probe_client.event('save')
    check(saved['files'] == 2 and saved['digests'] == sorted([digest(ONE), digest(TWO)]), saved)
    probe_client.finish()
    owner_client.stop()


def scenario_file_serve():
    _, one, folder, _ = fixture()
    probe_client = Client('fprobe', [FILES_PROBE, 'write-files', '--path', str(one), '--path', str(folder),
                                     '--hold-ms', '8000'])
    written = probe_client.event('write')
    check(written['entries'] == 2, written)
    uris = files_peer('request-uris', '--paths', f'{one};{folder}')[-1]
    check(uris['ok'] and uris['match'] and not uris['header'], uris)
    gnome = files_peer('request-gnome', '--paths', f'{one};{folder}')[-1]
    check(gnome['ok'] and gnome['match'] and gnome['header'], gnome)
    probe_client.finish()


def scenario_privacy_capture():
    client = owner_secret(6000)
    lines = probe('capture', '--read')
    captured = next(line for line in lines if line['event'] == 'capture')
    check(captured['sensitivity'] == 'Concealed', captured)
    reads = [(line['length'], line['sha256']) for line in lines
             if line['event'] == 'read' and line['ok']]
    check((len(TEXT), digest(TEXT)) in reads, f'text read missing: {captured} {reads}')
    check((len(SECRET), digest(SECRET)) in reads, f'marker read missing: {captured} {reads}')
    client.stop()


def scenario_privacy_write():
    probe_client = Client('fprobe', [FILES_PROBE, 'write-sensitive', '--level', 'concealed',
                                     '--hold-ms', '8000'])
    written = probe_client.event('write')
    check(written['sensitivity'] == 'Concealed' and written['length'] == len(TEXT), written)
    marker = files_peer('request-marker')[-1]
    check(marker['ok'] and marker['match'] and marker['length'] == len(SECRET), marker)
    probe_client.finish()


def scenario_delayed_provider():
    owner_client = owner('text', 8000)
    probe_client = Client('probe', [PROBE, 'capture', '--delay-ms', '600', '--read'])
    captured = probe_client.event('capture')
    check(captured['items'] == 1, captured)
    check(owner_client.count('peer-served') == 0, 'capture fetched payload bytes')
    first = probe_client.event('read', timeout=20)
    check(first['ok'], first)
    check(owner_client.count('peer-served') >= 1, 'provider never served the lazy read')
    probe_client.finish()
    owner_client.stop()


def scenario_guarded_cancel():
    owner_client = owner('text', 8000)
    before = probe('key')[-1]['key']
    for mode in ('guard-fail', 'cancel-fail'):
        result = next(line for line in files_probe('write-guarded', '--mode', mode)
                      if line['event'] == 'result')
        check(not result['ok'] and 'Busy' in result['kind'], result)
        after = probe('key')[-1]['key']
        check(after == before, f'{mode} changed clipboard ownership: {before} {after}')
    probe_client = Client('fprobe', [FILES_PROBE, 'write-guarded', '--mode', 'pass', '--hold-ms', '8000'])
    result = probe_client.event('result')
    check(result['ok'] and result['published'], result)
    served = peer('request', '--target', 'UTF8_STRING', '--expect', 'text')[-1]
    check(served['match'] and served['length'] == len(TEXT), served)
    probe_client.finish()
    owner_client.stop()


def scenario_virtual_write():
    _, one, _, two = fixture()
    probe_client = Client('fprobe', [FILES_PROBE, 'write-virtual', '--path', str(one), '--path', str(two),
                                     '--hold-ms', '8000'])
    written = probe_client.event('write')
    check(written['entries'] == 2, written)
    uris = files_peer('request-uris', '--paths', f'{one};{two}')[-1]
    check(uris['ok'] and uris['match'], uris)
    probe_client.finish()


SCENARIOS = [
    ('peer-files-capture', scenario_peer_files_capture),
    ('file-serve', scenario_file_serve),
    ('privacy-capture', scenario_privacy_capture),
    ('privacy-write', scenario_privacy_write),
    ('delayed-provider', scenario_delayed_provider),
    ('guarded-cancel', scenario_guarded_cancel),
    ('virtual-write', scenario_virtual_write),
]


def main():
    global PROBE, PEER, FILES_PROBE, FILES_PEER
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', required=True, type=str)
    parser.add_argument('--peer', required=True, type=str)
    parser.add_argument('--files-probe', required=True, type=str)
    parser.add_argument('--files-peer', required=True, type=str)
    parser.add_argument('--only', action='append', default=None)
    args = parser.parse_args()
    PROBE, PEER = args.probe, args.peer
    FILES_PROBE, FILES_PEER = args.files_probe, args.files_peer
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
