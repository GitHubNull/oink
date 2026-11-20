"""Bounded test-only role transport and private-session validation; no GUI on import."""
import json
import os
from pathlib import Path
import re
import select
import selectors
import socket
import stat
import struct
import subprocess
import time

MAX_LINE = 2048
MAX_BODY = 128
FIELDS = ("gen xid x y source_xid owner epoch armed begin end failed cancelled get "
          "received drop deleted press down up raw_down raw_up key_down key_up "
          "motions visits leaves inside held_checks returned free_checks selected "
          "button length hash pending stale slow_requests slow_sent slow_abandoned "
          "clip_owner clip_checks clip_bad digest0 digest1 digest2 digest3 "
          "digest4 digest5 digest6 digest7 toolkit_grabbed app "
          "incr_handshake incr_chunks incr_bytes incr_done "
          "incr_sent incr_sent_bytes incr_abandoned "
          "bridge_chunks bridge_bytes bridge_bad "
          "file_requests file_reads file_entries file_bytes file_bad "
          "staged published").split()
(STATUS, ARM, FOCUS, MOVE, DOWN, UP, ESCAPE, HELD, FREE, READ_SELECTION, LOAD,
 CLIP_CHECK, DELAY, DISARM, QUIT, RETURNED, FETCH, ORPHAN_UP, ARM_APP,
 FETCH_STREAM, LOAD_STREAM, FILE_READ, FILE_STAGE, FILE_VERIFY,
 GENERATION) = range(25)
PAYLOAD_MAX = 8 * 1024 * 1024
STREAM_CHUNK_MAX = 64 * 1024
# Binary payload channel header: magic u32, epoch u32, generation u32,
# transfer u32, mime u32, flags u32, offset u64, total u64, chunk_len u32
# (all little-endian), then chunk_len body bytes.
STREAM_HEADER = '<IIIIIIQQI'
STREAM_MAGIC = 0x4F4E4B31  # 'ONK1' per-chunk framing sanity
STREAM_TERMINATE = 1
STREAM_FLAG_ERROR = 2
MIME_UTF8 = 0
MIME_TEXT = 1
MIME_URI_LIST = 2
MIME_OCTET = 3


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
                PIPEWIRE_REMOTE='oink-test', NO_AT_BRIDGE='1')


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
        self.stream_sock, role_sock = socket.socketpair()
        environment = isolated_environment(environment)
        environment['OINK_STREAM_FD'] = str(role_sock.fileno())
        self.process = subprocess.Popen([str(binary), name], env=environment,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, bufsize=0, close_fds=True,
                                        pass_fds=(role_sock.fileno(),))
        role_sock.close()
        os.set_blocking(self.stream_sock.fileno(), False)
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

    def send_wire(self, opcode, *arguments, generation=None):
        """Write a command without waiting for its (possibly deferred) response."""
        self.seq += 1
        gen = self.generation if generation is None else generation
        values = (self.seq, opcode, gen, *arguments)
        check(all(isinstance(v, int) and 0 <= v <= 0xffffffff for v in values), 'invalid command integers')
        wire = (' '.join(map(str, values)) + '\n').encode('ascii')
        check(len(wire) < MAX_LINE, 'oversized command')
        deadline = time.monotonic() + 5
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
        return self.seq

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
        if opcode in (ARM, ARM_APP, GENERATION) and status == 0:
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
        self.stream_sock.close()
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()


def bridge_payload(catcher, proxy, timeout=15):
    """Relay a catcher FETCH_STREAM body into proxy LOAD_STREAM, validating every
    frame header (magic, epoch/generation, offset continuity, bounded chunk,
    terminate flag only on the final chunk covering the declared total)."""
    check(catcher.name == 'catcher' and proxy.name == 'proxy', 'bridge relays catcher to proxy only')
    total = catcher.last['length']
    epoch = catcher.last['epoch']
    check(0 < total <= PAYLOAD_MAX and catcher.last['hash'], 'catcher payload ready')
    check(catcher.generation == proxy.generation, 'bridge requires a shared generation')
    proxy_seq = proxy.send_wire(LOAD_STREAM, total, epoch, catcher.generation)
    catcher.command(FETCH_STREAM)
    deadline = time.monotonic() + timeout
    buf, out = bytearray(), bytearray()
    forwarded, terminated = 0, False
    with selectors.DefaultSelector() as sel:
        sel.register(catcher.stream_sock, selectors.EVENT_READ, 'catch')
        sel.register(proxy.stream_sock, selectors.EVENT_WRITE, 'send')
        while True:
            left = deadline - time.monotonic()
            check(left > 0, 'stream bridge deadline')
            events = sel.select(left)
            check(events, 'stream bridge deadline')
            for key, mask in events:
                if key.data == 'send':
                    if mask & selectors.EVENT_WRITE and out:
                        try:
                            count = proxy.stream_sock.send(out)
                            del out[:count]
                        except BlockingIOError:
                            pass
                    continue
                if terminated:
                    continue
                chunk = catcher.stream_sock.recv(STREAM_CHUNK_MAX + 4096)
                check(chunk, 'catcher stream closed mid-transfer')
                buf.extend(chunk)
                while len(buf) >= 44:
                    magic, ep, gen, transfer, mime, flags, offset, size, clen = \
                        struct.unpack_from(STREAM_HEADER, buf)
                    if len(buf) < 44 + clen:
                        break
                    check(magic == STREAM_MAGIC and mime <= MIME_OCTET, 'stream frame magic/mime')
                    check(ep == epoch and gen == catcher.generation, 'stale stream frame epoch/generation')
                    check(clen and clen <= STREAM_CHUNK_MAX and size == total
                          and offset == forwarded, 'stream frame offset/size mismatch')
                    check(flags & ~STREAM_TERMINATE == 0, 'unknown stream flags')
                    frame = bytes(buf[44:44 + clen])
                    del buf[:44 + clen]
                    out.extend(struct.pack(STREAM_HEADER, magic, ep, gen, transfer, mime,
                                           flags, offset, size, clen))
                    out.extend(frame)
                    forwarded += clen
                    if flags & STREAM_TERMINATE:
                        check(forwarded == total, 'terminate before declared total')
                        terminated = True
            if terminated and not out:
                break
    check(terminated, 'catcher stream ended without terminate flag')
    status, report, body = proxy.receive(proxy_seq, deadline)
    check(status == 0 and body is None, f'proxy LOAD_STREAM status {status}')
    check(report['bridge_bytes'] == total and not report['bridge_bad'],
          'proxy did not ingest a clean full payload')
    return report


def stream_feed(role, payload, timeout=15, mime=MIME_UTF8):
    """Push a binary payload into a source role (source or proxy) via LOAD_STREAM."""
    check(role.name in ('source', 'proxy'), 'feed targets source roles only')
    total = len(payload)
    check(role.last and role.last['xid'] and 0 < total <= PAYLOAD_MAX, 'feed size/role state')
    check(mime <= MIME_OCTET, 'stream feed mime')
    seq = role.send_wire(LOAD_STREAM, total, role.last['epoch'], role.generation)
    deadline = time.monotonic() + timeout
    offset, transfer = 0, 1
    while offset < total:
        chunk = payload[offset:offset + STREAM_CHUNK_MAX]
        flags = STREAM_TERMINATE if offset + len(chunk) == total else 0
        frame = struct.pack(STREAM_HEADER, STREAM_MAGIC, role.last['epoch'],
                            role.generation, transfer, mime, flags,
                            offset, total, len(chunk)) + chunk
        while frame:
            left = deadline - time.monotonic()
            check(left > 0, 'stream feed deadline')
            _, writable, _ = select.select([], [role.stream_sock], [], left)
            check(writable, 'stream feed deadline')
            try:
                count = role.stream_sock.send(frame)
            except BlockingIOError:
                continue
            frame = frame[count:]
        offset += len(chunk)
    status, report, body = role.receive(seq, deadline)
    check(status == 0 and body is None, f'{role.name} LOAD_STREAM status {status}')
    check(report['length'] == total and report['hash'], 'feed payload not ingested')
    return report
