#!/usr/bin/env python3
"""Authorized-root file staging acceptance, only inside linux-native-test.py.

Build externally (no auto-install or auto-compilation); pass --roles /absolute/binary.
The only body path stays selection-based: source FILE_READ ingests a real file
below the private authorized root (sha256 + size-stability double read), the
bytes travel source -> catcher via a real XDND drag (INCR when above
INCR_THRESHOLD), catcher FETCH_STREAM -> controller bridge -> proxy
LOAD_STREAM, and the proxy publishes the verified body under published/ with
one atomic rename after a staging re-hash. Catcher/target FILE_VERIFY then
really open the published path: offset pread + sha256 for regular files,
deterministic sorted walk (symlinks listed, never followed) for directories.
Refusal cases (escape/dot/backslash paths, directories at source, missing
paths, oversized files, existing publish targets, out-of-root symlinks) must
reject without success; hard-fenced ones kill the offending role. No payload
bytes are logged, only hashes/counts.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import struct
import subprocess
import sys
import time

from linux_dnd_protocol import (
    ARM, ESCAPE, FETCH, FETCH_STREAM, FILE_READ, FILE_STAGE, FILE_VERIFY, LOAD,
    MIME_OCTET, PAYLOAD_MAX, READ_SELECTION, STREAM_CHUNK_MAX, STREAM_HEADER,
    STREAM_MAGIC, STREAM_TERMINATE, UP, Blocked, Failure, bridge_payload, check,
    isolated_environment, peer_environment, stream_feed,
)
from linux_dnd_scenarios import (
    Group, clipboard, cleanup_source, enter, evidence, exact, return_reenter,
    same_payload, start, wait,
)

TEXT = ('Oink 文件拖拽验收：源端按授权根实读，正文唯一通路为 XDND selection。\n'
        'Unicode 内容 café · 北京 · ファイル · \U0001F411\n').encode()
INCR_THRESHOLD = 256 * 1024
BLOB_SIZE = INCR_THRESHOLD + 65536 + 7
BLOB = bytes(((i * 31 + 7) & 0xff) for i in range(BLOB_SIZE))
LEFT = b'left tree payload\n'
RIGHT = bytes(range(256)) * 3
EMPTY_DIGEST_WORDS = None  # filled in prepare()


def sha_words(data):
    digest = hashlib.sha256(data).digest()
    return tuple(int.from_bytes(digest[i * 4:i * 4 + 4], 'big') for i in range(8))


def report_words(report):
    return tuple(report[f'digest{i}'] for i in range(8))


def place(root, rel, data):
    target = root / rel
    target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(target.parent, 0o700)
    fd = os.open(target, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
    return target


def prepare_tree(root):
    global EMPTY_DIGEST_WORDS
    EMPTY_DIGEST_WORDS = sha_words(b'')
    root.mkdir(mode=0o700, exist_ok=True)
    os.chmod(root, 0o700)
    place(root, 'docs/note.txt', TEXT)
    place(root, 'payloads/blob.bin', BLOB)
    place(root, 'empty/zero.bin', b'')
    place(root, 'big/huge.bin', b'\x5a' * (PAYLOAD_MAX + 1))
    place(root, 'adir/inner.txt', b'inner\n')
    published = root / 'published'
    (published / 'inbox').mkdir(mode=0o700, parents=True, exist_ok=True)
    (published / 'tree' / 'left').mkdir(mode=0o700, parents=True, exist_ok=True)
    (published / 'tree' / 'right').mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(published, 0o700)
    place(root, 'published/taken.bin', b'taken\n')
    link = published / 'tree' / 'link'
    if not link.exists() and not link.is_symlink():
        link.symlink_to('/etc/hostname')


def prepare(env, peer):
    prepare_tree(Path(env['OINK_NATIVE_TEST_ROOT']) / 'files')
    if peer:
        prepare_tree(Path(peer['OINK_NATIVE_TEST_ROOT']) / 'files')


def file_read(role, rel):
    return role.command(FILE_READ, *rel.encode())


def stream_body(role, timeout=15):
    """Read a catcher body back over the FD channel (FETCH_STREAM sender path)."""
    check(role.name == 'catcher', 'stream body reads catcher roles only')
    seq = role.send_wire(FETCH_STREAM)
    deadline = time.monotonic() + timeout
    buf, body = bytearray(), bytearray()
    offset, terminated = 0, False
    while not terminated:
        left = deadline - time.monotonic()
        check(left > 0, 'stream body deadline')
        readable, _, _ = select.select([role.stream_sock], [], [], left)
        check(readable, 'stream body deadline')
        chunk = role.stream_sock.recv(STREAM_CHUNK_MAX + 4096)
        check(chunk, 'catcher stream closed mid-transfer')
        buf.extend(chunk)
        while len(buf) >= 44:
            magic, ep, gen, transfer, mime, flags, off, total, clen = \
                struct.unpack_from(STREAM_HEADER, buf)
            if len(buf) < 44 + clen:
                break
            check(magic == STREAM_MAGIC and mime <= MIME_OCTET, 'stream frame magic/mime')
            check(ep == role.last['epoch'] and gen == role.generation,
                  'stale stream frame epoch/generation')
            check(clen and clen <= STREAM_CHUNK_MAX and off == offset and total,
                  'stream frame offset/size mismatch')
            body.extend(buf[44:44 + clen])
            del buf[:44 + clen]
            offset += clen
            if flags & STREAM_TERMINATE:
                check(offset == total, 'terminate before declared total')
                terminated = True
    status, report, _ = role.receive(seq, deadline)
    check(status == 0, f'FETCH_STREAM status {status}')
    return bytes(body)


def check_source_read(report, data, requests):
    exact(report, length=len(data), file_requests=requests)
    check(report_words(report) == sha_words(data), f'digest mismatch: {report}')


def file_text_drag_same_server(binary, env):
    name = 'file-text-drag-same-server'
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source, catcher = group.add('source', env), group.add('catcher', env)
        target = group.add('target', env)
        clipboard(sentinel, source, catcher, target)
        check_source_read(file_read(source, 'docs/note.txt'), TEXT, 1)
        xid = start(source)
        catcher.command(ARM, xid)
        target.command(ARM, xid)
        enter(source, catcher)
        exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
        result = wait(catcher, lambda r: r['received'] == 1, 'catcher file selection read')
        exact(result, drop=0, stale=0)
        same_payload(result, source.last)
        body = stream_body(catcher)
        check(body == TEXT, f'catcher file body mismatch: {len(body)} bytes')
        return_reenter(source, catcher, gets=1)
        enter(source, target, gets=1)
        source.command(UP)
        dropped = wait(target, lambda r: r['received'] == 1, 'target file drop read')
        exact(dropped, drop=1, stale=0)
        same_payload(dropped, source.last)
        # Catcher already consumed the selection once; the target drop is the
        # second real SelectionRequest against the source.
        cleanup_source(source, False, 2)
        clipboard(sentinel, source, catcher, target)
        root = Path(env['OINK_NATIVE_TEST_ROOT']) / 'files'
        check((root / 'docs/note.txt').read_bytes() == TEXT, 'source file must not be modified')
        evidence(name, source=source, catcher=catcher, target=target, sentinel=sentinel)


def stage(proxy, rel, data, nth):
    loaded = stream_feed(proxy, data)
    exact(loaded, length=len(data))
    exact(proxy.command(FILE_STAGE, *rel.encode()), staged=nth, published=nth)


def file_stage_dual_server(binary, env, peer):
    name = 'file-stage-dual-server'
    with Group(binary) as group:
        sentinel_a, sentinel_b = group.add('sentinel', env), group.add('sentinel', peer)
        source, catcher = group.add('source', env), group.add('catcher', env)
        proxy, target = group.add('proxy', peer), group.add('target', peer)
        catcher_b = group.add('catcher', peer)
        clipboard(sentinel_a, source, catcher)
        clipboard(sentinel_b, proxy, target)
        check_source_read(file_read(source, 'payloads/blob.bin'), BLOB, 1)
        catcher.command(ARM, start(source))
        enter(source, catcher)
        exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
        received = wait(catcher, lambda r: r['received'] == 1, 'catcher INCR file read',
                        seconds=15)
        exact(received, drop=0, stale=0)
        check(received['incr_bytes'] == BLOB_SIZE, f'INCR file bytes: {received}')
        same_payload(received, source.last)
        source.command(ESCAPE)
        exact(wait(source, lambda r: r['end'] == 1, 'source drag end'), get=1)
        loaded = bridge_payload(catcher, proxy, timeout=15)
        check(loaded['length'] == BLOB_SIZE and loaded['bridge_bytes'] == BLOB_SIZE
              and not loaded['bridge_bad'], 'bridge ingest')
        same_payload(loaded, received)
        cleanup_source(source, True, 1, returned=0)
        # The bridge already loaded the blob body into the proxy; staging
        # consumes it, after which the proxy may ingest the next file.
        exact(proxy.command(FILE_STAGE, *b'inbox/blob.bin'), staged=1, published=1)
        verified = target.command(FILE_VERIFY, *b'published/inbox/blob.bin')
        exact(verified, length=BLOB_SIZE, file_reads=1, file_bytes=BLOB_SIZE)
        same_payload(verified, loaded)
        peer_root = Path(peer['OINK_NATIVE_TEST_ROOT']) / 'files'
        published_blob = peer_root / 'published/inbox/blob.bin'
        check(published_blob.read_bytes() == BLOB, 'published bytes differ from source file')
        stage(proxy, 'tree/left/a.txt', LEFT, 2)
        stage(proxy, 'tree/right/b.bin', RIGHT, 3)
        catcher_b.command(ARM, 1)
        catcher_b.command(FILE_VERIFY, *b'published/tree')
        listed = stream_body(catcher_b)
        expected = listing_body(LEFT, RIGHT)
        check(listed == expected, f'listing bytes: {listed!r}')
        before_listing = target.last
        listing = target.command(FILE_VERIFY, *b'published/tree')
        walked = tree_file_bytes(peer_root / 'published/tree')
        check(listing['file_entries'] == 5
              and listing['file_bytes'] - before_listing['file_bytes'] == walked,
              f'directory listing counts: {listing}, walked={walked}')
        check(report_words(listing) == sha_words(listed),
              'target listing digest diverges from catcher listing bytes')
        check(report_words(catcher_b.last) == report_words(listing),
              'target/catcher directory listings diverge')
        clipboard(sentinel_a, source, catcher)
        clipboard(sentinel_b, proxy, target, catcher_b)
        root = Path(env['OINK_NATIVE_TEST_ROOT']) / 'files'
        check((root / 'payloads/blob.bin').read_bytes() == BLOB, 'source file must survive')
        evidence(name, source=source, catcher=catcher, proxy=proxy, target=target,
                 catcher_b=catcher_b, sentinel_a=sentinel_a, sentinel_b=sentinel_b)


def tree_file_bytes(root):
    total = 0
    for dirpath, dirnames, filenames in os.walk(root):
        for name in filenames:
            path = os.path.join(dirpath, name)
            if not os.path.islink(path):
                total += os.path.getsize(path)
    return total


def listing_body(left, right):
    # The C walk prints "<kind> <relpath> <size>" for every entry; non-regular
    # entries carry size 0.
    lines = [f'D published/tree/left 0', f'F published/tree/left/a.txt {len(left)}',
             f'L published/tree/link 0', f'D published/tree/right 0',
             f'F published/tree/right/b.bin {len(right)}']
    return ('\n'.join(lines) + '\n').encode()


def file_empty_roundtrip(binary, env):
    name = 'file-empty-roundtrip'
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source, proxy = group.add('source', env), group.add('proxy', env)
        target = group.add('target', env)
        clipboard(sentinel, source, proxy, target)
        report = file_read(source, 'empty/zero.bin')
        exact(report, length=0, file_requests=1)
        check(report_words(report) == EMPTY_DIGEST_WORDS, f'empty digest: {report}')
        exact(proxy.command(FILE_STAGE, *b'inbox/zero.bin'), staged=1, published=1)
        verified = target.command(FILE_VERIFY, *b'published/inbox/zero.bin')
        exact(verified, length=0, file_reads=1, file_bytes=0)
        check(report_words(verified) == EMPTY_DIGEST_WORDS, f'empty verify digest: {verified}')
        published = Path(env['OINK_NATIVE_TEST_ROOT']) / 'files/published/inbox/zero.bin'
        check(published.read_bytes() == b'', 'empty publish must stay empty')
        check((Path(env['OINK_NATIVE_TEST_ROOT']) / 'files/empty/zero.bin').read_bytes() == b'',
              'empty source file must not be modified')
        clipboard(sentinel, source, proxy, target)
        evidence(name, source=source, proxy=proxy, target=target, sentinel=sentinel)


def wait_exit(role, label):
    deadline = time.monotonic() + 5
    while role.process.poll() is None and time.monotonic() < deadline:
        time.sleep(.02)
    check(role.process.poll() is not None,
          f'{label}: hard refusal must kill the offending role')


def expect_death(binary, env, role_name, opcode, rel, label):
    with Group(binary) as group:
        role = group.add(role_name, env)
        try:
            role.command(opcode, *rel.encode())
        except Failure:
            wait_exit(role, label)
            print(json.dumps({'result': 'PASS', 'kind': 'file_refusal',
                              'case': label, 'path': rel, 'mechanism': 'role_exit'}),
                  flush=True)
            return
        raise Failure(f'{label}: forbidden path accepted: {rel}')


def expect_status2(binary, env, role_name, opcode, rel, label):
    with Group(binary) as group:
        role = group.add(role_name, env)
        report = role.command(opcode, *rel.encode(), expected=2)
        check(role.process.poll() is None, f'{label}: soft refusal must keep role alive')
        print(json.dumps({'result': 'PASS', 'kind': 'file_refusal',
                          'case': label, 'path': rel, 'mechanism': 'status_2',
                          'file_bad': report['file_bad']}), flush=True)


def file_refusals(binary, env, peer):
    expect_death(binary, env, 'source', FILE_READ, '../escape.txt', 'read-parent-escape')
    expect_death(binary, env, 'source', FILE_READ, 'docs/../note.txt', 'read-dotdot-component')
    expect_death(binary, env, 'source', FILE_READ, 'docs/./note.txt', 'read-dot-component')
    expect_death(binary, env, 'source', FILE_READ, 'docs\\note.txt', 'read-backslash')
    expect_death(binary, env, 'source', FILE_READ, 'missing.bin', 'read-missing')
    expect_death(binary, env, 'target', FILE_VERIFY, '../escape.txt', 'verify-parent-escape')
    if peer:
        expect_death(binary, peer, 'target', FILE_VERIFY, 'published/tree/link',
                     'verify-out-of-root-symlink')
    else:
        print(json.dumps({'result': 'BLOCKED', 'kind': 'file_refusal',
                          'case': 'verify-out-of-root-symlink',
                          'reason': 'no peer environment'}), flush=True)
    expect_status2(binary, env, 'source', FILE_READ, 'adir', 'read-directory')
    expect_status2(binary, env, 'source', FILE_READ, 'big/huge.bin', 'read-oversize')
    expect_status2(binary, env, 'target', FILE_READ, 'docs/note.txt', 'read-wrong-role')
    with Group(binary) as group:
        proxy = group.add('proxy', env)
        exact(proxy.command(LOAD, 3, 65, 66, 67), length=3)
        try:
            proxy.command(FILE_STAGE, *b'taken.bin')
        except Failure:
            wait_exit(proxy, 'stage-existing')
            print(json.dumps({'result': 'PASS', 'kind': 'file_refusal',
                              'case': 'stage-existing-target', 'path': 'published/taken.bin',
                              'mechanism': 'role_exit'}), flush=True)
        else:
            raise Failure('stage-existing-target accepted')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--roles', required=True, type=Path, help='absolute compiled role binary')
    parser.add_argument('--suite', choices=('all', 'staging', 'refusals'), default='all')
    args = parser.parse_args(argv)
    try:
        check(args.roles.is_absolute() and args.roles.is_file() and os.access(args.roles, os.X_OK),
              '--roles must name an existing absolute executable')
        env = isolated_environment(dict(os.environ))
        try:
            peer = peer_environment(env, os.environ.get('OINK_NATIVE_TEST_PEER_ENV'))
        except Blocked:
            peer = None
        if args.suite in ('all', 'staging'):
            check(peer, 'staging suite requires runner --peer-x11')
        prepare(env, peer)
        if args.suite in ('all', 'staging'):
            file_text_drag_same_server(args.roles, env)
            file_stage_dual_server(args.roles, env, peer)
            file_empty_roundtrip(args.roles, env)
        if args.suite in ('all', 'refusals'):
            file_refusals(args.roles, env, peer)
        print(json.dumps({'result': 'FILE_SCENARIOS_PASS', 'scope': args.suite,
                          'restriction': 'empty files never travel the XDND body path '
                                         '(zero-length payload cannot begin a drag); '
                                         'staging/verify are exercised directly'}), flush=True)
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
