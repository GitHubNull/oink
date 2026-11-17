"""Bounded test-only role transport and private-session validation; no GUI on import."""
import json
import os
from pathlib import Path
import re
import selectors
import stat
import subprocess
import time

MAX_LINE = 2048
MAX_BODY = 128
FIELDS = ("gen xid x y source_xid owner epoch armed begin end failed cancelled get "
          "received drop deleted press down up raw_down raw_up key_down key_up "
          "motions visits leaves inside held_checks returned free_checks selected "
          "button length hash pending stale slow_requests slow_sent slow_abandoned "
          "clip_owner clip_checks clip_bad digest0 digest1 digest2 digest3 "
          "digest4 digest5 digest6 digest7 toolkit_grabbed").split()
(STATUS, ARM, FOCUS, MOVE, DOWN, UP, ESCAPE, HELD, FREE, READ_SELECTION, LOAD,
 CLIP_CHECK, DELAY, DISARM, QUIT, RETURNED, FETCH, ORPHAN_UP) = range(18)


class Failure(RuntimeError):
    pass


class Blocked(RuntimeError):
    pass


def check(condition, message):
    if not condition:
        raise Failure(message)


def private_node(path, directory=False):
    info = Path(path).lstat()
    check((stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))
          and info.st_uid == os.getuid() and not info.st_mode & 0o077,
          f"not an owned private {'directory' if directory else 'file'}: {path}")


def isolated_environment(environment):
    """Whitelist a complete runner environment, never merge with host os.environ."""
    check(isinstance(environment, dict) and all(isinstance(k, str) and isinstance(v, str)
          and '\0' not in k + v for k, v in environment.items()), "invalid environment mapping")
    root = environment.get('OINK_NATIVE_TEST_ROOT', '')
    check(re.fullmatch(r'/tmp/oink-native-[A-Za-z0-9_-]+', root), "not a private runner root")
    private_node(root, directory=True)
    display = environment.get('DISPLAY', '')
    check(re.fullmatch(r':[1-9][0-9]*(?:\.0)?', display), "refusing host/remote DISPLAY")
    expected = {
        'HOME': f'{root}/home', 'XAUTHORITY': f'{root}/Xauthority',
        'XDG_CONFIG_HOME': f'{root}/config', 'XDG_DATA_HOME': f'{root}/data',
        'XDG_CACHE_HOME': f'{root}/cache', 'XDG_STATE_HOME': f'{root}/state',
        'XDG_RUNTIME_DIR': f'{root}/runtime', 'TMPDIR': f'{root}/tmp',
        'DBUS_SESSION_BUS_ADDRESS': f'unix:path={root}/runtime/session-bus',
        'DBUS_SYSTEM_BUS_ADDRESS': f'unix:path={root}/runtime/no-system-bus',
        'PULSE_SERVER': f'unix:{root}/runtime/pulse-native',
        'PULSE_CLIENTCONFIG': f'{root}/config/pulse-client.conf',
        'PIPEWIRE_RUNTIME_DIR': f'{root}/runtime',
        'PIPEWIRE_CONFIG_DIR': f'{root}/config/pipewire',
    }
    for key, value in expected.items():
        check(environment.get(key) == value, f"non-private or missing {key}")
    private_node(expected['XAUTHORITY'])
    return dict(expected, DISPLAY=display, OINK_NATIVE_TEST_ROOT=root,
                PATH='/usr/bin:/bin', LANG='C.UTF-8', LC_ALL='C.UTF-8',
                GDK_BACKEND='x11', QT_QPA_PLATFORM='xcb', XDG_SESSION_TYPE='x11',
                PIPEWIRE_REMOTE='oink-test')


def peer_environment(primary, metadata):
    if not metadata:
        raise Blocked('dual-server requires runner --peer-x11 / OINK_NATIVE_TEST_PEER_ENV')
    path = Path(metadata)
    check(path.is_absolute() and path.parent == Path(primary['OINK_NATIVE_TEST_ROOT']),
          "peer metadata must be directly inside the primary private root")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        check(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
              and stat.S_IMODE(info.st_mode) == 0o600 and info.st_size <= 65536,
              "peer metadata must be an owned 0600 bounded regular file")
        raw = os.read(fd, 65537)
        check(len(raw) <= 65536, 'peer metadata too large')
    finally:
        os.close(fd)
    peer = isolated_environment(json.loads(raw))
    check(peer['OINK_NATIVE_TEST_ROOT'] != primary['OINK_NATIVE_TEST_ROOT']
          and peer['DISPLAY'].split('.')[0] != primary['DISPLAY'].split('.')[0]
          and peer['XAUTHORITY'] != primary['XAUTHORITY'], "peer must be an independent private session")
    return peer


class Framer:
    def __init__(self):
        self.pending = bytearray()

    def feed(self, data):
        frames = []
        for byte in data:
            if byte == 10:
                check(self.pending, 'empty role frame')
                frames.append(bytes(self.pending))
                self.pending.clear()
                check(len(frames) <= 32, 'too many role frames')
            else:
                check(len(self.pending) < MAX_LINE - 1 and 32 <= byte < 127, 'invalid/oversized role frame')
                self.pending.append(byte)
        return frames


def decode(frame):
    words = frame.decode('ascii').split(' ')
    check(words[0] in ('R', 'D') and all(re.fullmatch(r'[0-9]{1,10}', s) for s in words[1:]),
          'malformed numeric role response')
    values = [int(s) for s in words[1:]]
    check(all(v <= 0xffffffff for v in values), 'role response integer overflow')
    if words[0] == 'R':
        check(len(values) == len(FIELDS) + 2 and values[1] <= 2, 'invalid report shape/status')
        return 'R', values[0], (values[1], dict(zip(FIELDS, values[2:])))
    check(len(values) >= 3 and 0 < values[2] <= MAX_BODY and len(values) == values[2] + 3
          and all(b <= 255 for b in values[3:]), 'invalid bridge body')
    return 'D', values[0], (values[1], bytes(values[3:]))


class Role:
    def __init__(self, binary, name, environment):
        self.name, self.seq, self.generation = name, 0, 1
        self.framer, self.frames, self.errors = Framer(), [], bytearray()
        self.selector = selectors.DefaultSelector()
        self.process = subprocess.Popen([str(binary), name], env=isolated_environment(environment),
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, bufsize=0, close_fds=True)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            os.set_blocking(stream.fileno(), False)
        self.selector.register(self.process.stdout, selectors.EVENT_READ, 'out')
        self.selector.register(self.process.stderr, selectors.EVENT_READ, 'err')
        self.last = None
        self.expected_exit = None

    def pump(self, deadline):
        left = deadline - time.monotonic()
        check(left > 0, f'{self.name}: control deadline (partial={len(self.framer.pending)})')
        events = self.selector.select(left)
        check(events, f'{self.name}: control deadline')
        for key, _ in events:
            chunk = os.read(key.fd, 4096)
            if not chunk:
                self.selector.unregister(key.fileobj)
                if key.data == 'out':
                    raise Failure(f'{self.name}: EOF/exit {self.process.poll()}; {self.errors.decode(errors="replace")}')
                continue
            if key.data == 'err':
                self.errors.extend(chunk)
                check(len(self.errors) <= 8192, f'{self.name}: excessive stderr')
            else:
                self.frames.extend(decode(frame) for frame in self.framer.feed(chunk))
                check(len(self.frames) <= 32, 'bounded role response queue')

    def receive(self, sequence, deadline, allow_data=False):
        body = None
        while True:
            if not self.frames:
                self.pump(deadline)
                continue
            kind, seq, value = self.frames.pop(0)
            check(seq == sequence, f'{self.name}: unexpected response sequence {seq}')
            if kind == 'D':
                check(allow_data and self.name == 'catcher' and body is None
                      and value[0] == self.generation, 'unsolicited/stale bridge payload')
                body = value[1]
            else:
                status, self.last = value
                return status, self.last, body

    def ready(self):
        status, report, body = self.receive(0, time.monotonic() + 5)
        check(status == 0 and report['xid'] and report['x'] and report['y'] and body is None,
              f'{self.name}: incomplete window readiness')
        return report

    def command(self, opcode, *arguments, generation=None, expected=0, timeout=2):
        self.seq += 1
        gen = self.generation if generation is None else generation
        values = (self.seq, opcode, gen, *arguments)
        check(all(isinstance(v, int) and 0 <= v <= 0xffffffff for v in values), 'invalid command integers')
        wire = (' '.join(map(str, values)) + '\n').encode('ascii')
        check(len(wire) < MAX_LINE, 'oversized command')
        deadline = time.monotonic() + timeout
        # Nonblocking write with a deadline even if a role never drains stdin.
        with selectors.DefaultSelector() as writer:
            writer.register(self.process.stdin, selectors.EVENT_WRITE)
            while wire:
                left = deadline - time.monotonic()
                check(left > 0 and writer.select(left), f'{self.name}: command write deadline')
                try:
                    count = os.write(self.process.stdin.fileno(), wire)
                except BlockingIOError:
                    continue
                wire = wire[count:]
        status, report, body = self.receive(self.seq, deadline, allow_data=opcode == FETCH)
        check(status == expected, f'{self.name}: opcode {opcode} status {status}, expected {expected}')
        if opcode == ARM and status == 0:
            self.generation = gen
        check(report['gen'] == self.generation, f'{self.name}: unexpected generation')
        if opcode == FETCH and status == 0:
            check(body is not None, 'missing catcher selection body')
            return body
        return report

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=1)
        self.selector.close()
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()
