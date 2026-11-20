#!/usr/bin/env python3
"""File and directory XDND rows of the real-application matrix.

Extends the text matrix: the GTK fixture roles speak text/uri-list alongside
UTF8_STRING/text/plain. Role-source rows serve a real file:// URI list (read
through the authorized-root FILE_READ path) and nemo must really copy the
file/directory into its window; browser rows must report the uri-list through
the page drop probe. The nemo-source row drags a real file icon onto the
fixture catcher, which reads the uri-list selection and then really opens the
file through FILE_VERIFY. Source rows that cannot be driven synthetically are
registered BLOCKED with the existing tripwire evidence, never upgraded for free.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time

from linux_dnd_app_scenarios import (
    APP_GENERATION_ATOM, calibrate, check, clipboard, evidence, harness,
    row, session_setup, settled_coords, shifted, wait, wait_task_headroom,
)
from linux_dnd_protocol import (
    ARM_APP, FETCH, FILE_READ, FILE_VERIFY, FREE, MOVE, READ_SELECTION, STATUS,
    UP, Failure, Role,
)
from linux_dnd_scenarios import exact, start

PAYLOAD = 'nemo incoming payload 文件\n'.encode()
SOURCE_FILE = 'drag-me.txt'
SOURCE_BODY = 'nemo real file source 内容\n'.encode()


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def place(root, rel, data):
    target = root / rel
    target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(target.parent, 0o700)
    try:
        fd = os.open(target, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:  # matrix rows share one probe root; fixtures may be re-placed
        os.unlink(target)
        fd = os.open(target, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
    return target


def uri_fixture(root, name, path):
    uri = b'file://' + str(path).encode() + b'\r\n'
    return uri, place(root, f'uris/{name}.uri', uri)


def drive_role_drag(source, tx, ty):
    """Path a begun role drag to app coordinates with settle motion pairs."""
    sx, sy = source.last['x'], source.last['y']
    for step in range(1, 8):
        source.command(MOVE, sx + (tx - sx) * step // 8, sy + (ty - sy) * step // 8)
    for wiggle in range(6):
        source.command(MOVE, tx + (10 if wiggle % 2 else -10), ty)
        time.sleep(0.15)
    source.command(MOVE, tx, ty)
    time.sleep(0.2)


def files_root(env):
    root = Path(env['OINK_NATIVE_TEST_ROOT']) / 'files'
    root.mkdir(mode=0o700, exist_ok=True)
    os.chmod(root, 0o700)
    return root


def role_file_to_nemo(roles, environment, directory=False):
    label = 'directory' if directory else 'file'
    name = f'nemo-target-{label}-drop'
    env, x11, epoch, server = session_setup(environment)
    app = None
    roles_used = []
    try:
        root = files_root(env)
        drop_dir = root / 'nemo-drop'
        drop_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        if directory:
            place(root, 'incoming/folder/dirfile.txt', PAYLOAD)
            incoming = root / 'incoming/folder'
        else:
            incoming = place(root, 'incoming/payload.txt', PAYLOAD)
        uri, _ = uri_fixture(root, label, incoming)
        app = harness.launch('nemo', str(drop_dir), environment)
        window = x11.wait_window('Nemo')[0]
        pid = x11.xres_pid(window['xid'])
        exe = app.verify_window_pid(pid)
        wait_task_headroom()
        sentinel = Role(roles, 'sentinel', env); roles_used.append(sentinel); sentinel.ready()
        source = Role(roles, 'source', env); roles_used.append(source); source.ready()
        clipboard(sentinel, source)
        report = source.command(FILE_READ, *(f'uris/{label}.uri'.encode()))
        check(report['length'] == len(uri) and report['file_requests'] == 1,
              f'uri-list ingest: {report}')
        start(source)
        tx = window['x'] + window['width'] // 2
        ty = window['y'] + window['height'] // 2
        drive_role_drag(source, tx, ty)
        source.command(UP)
        wait(source, lambda r: r['end'] == 1, f'drag end over nemo {label} drop')
        wait(source, lambda r: r['toolkit_grabbed'] == 0, 'toolkit grab release', seconds=.5)
        report = source.command(STATUS)
        check(report['get'] >= 1, f'nemo requested the {label} selection: {report}')
        copied = drop_dir / incoming.name
        if directory:
            copied = copied / 'dirfile.txt'
        deadline = time.monotonic() + 5
        while not copied.exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        check(copied.is_file() and copied.read_bytes() == PAYLOAD,
              f'nemo did not deliver the {label}: {copied}')
        source.command(FREE)
        clipboard(sentinel, source)
        evidence(name, sentinel=sentinel, source=source)
        row(name, 'PASS', app='nemo', pid=pid, exe=exe, kind=label,
            selection_requests=report['get'], uri_sha256=sha(uri.decode()))
        return True
    except Exception as error:
        row(name, 'FAIL', reason=str(error))
        raise
    finally:
        for role in roles_used:
            role.close()
        if app:
            app.close()
        server.close()
        x11.close()


def role_file_to_browser(roles, kind, wm_class, environment):
    name = f'{kind}-target-file-drop'
    env, x11, epoch, server = session_setup(environment)
    app = None
    roles_used = []
    try:
        root = files_root(env)
        payload = place(root, 'incoming/browser-payload.txt', PAYLOAD)
        uri, _ = uri_fixture(root, 'browser', payload)
        app = harness.launch(kind, server.url, environment)
        window = x11.wait_window(wm_class)[0]
        ready = server.wait_report('ready')
        check(ready['trusted'] is True, 'page ready evidence')
        locate = settled_coords(server)
        wait_task_headroom()
        offset = calibrate(server, x11, locate)
        sentinel = Role(roles, 'sentinel', env); roles_used.append(sentinel); sentinel.ready()
        source = Role(roles, 'source', env); roles_used.append(source); source.ready()
        clipboard(sentinel, source)
        report = source.command(FILE_READ, *b'uris/browser.uri')
        check(report['length'] == len(uri) and report['file_requests'] == 1,
              f'uri-list ingest: {report}')
        start(source)
        tx, ty = shifted(locate, offset, 'target')
        drive_role_drag(source, tx, ty)
        source.command(UP)
        dropped = server.wait_report('drop')
        expected = sha(uri.decode())
        files = dropped.get('files') or []
        digests = dropped.get('file_digests') or []
        # GTK hands Firefox a file:// uri-list; Firefox converts it into a real
        # File object, so the page may see Files instead of a uri-list string.
        via_uri = (dropped['trusted'] is True and 'text/uri-list' in dropped['types']
                   and dropped['uri_length'] > 0 and dropped['uri_sha256'] == expected)
        via_file = (dropped['trusted'] is True and len(files) == 1 and len(digests) == 1
                    and files[0]['name'] == 'browser-payload.txt'
                    and files[0]['size'] == len(PAYLOAD)
                    and digests[0]['sha256'] == sha(PAYLOAD.decode()))
        check(via_uri or via_file, f'page file drop evidence: {dropped}')
        channel = 'uri-list' if via_uri else 'File'
        wait(source, lambda r: r['end'] == 1, 'source drag end after app drop')
        wait(source, lambda r: r['toolkit_grabbed'] == 0, 'toolkit grab release', seconds=.5)
        report = source.command(STATUS)
        check(1 <= report['get'] <= 4, f'bounded selection service, got {report["get"]}')
        source.command(FREE)
        clipboard(sentinel, source)
        evidence(name, sentinel=sentinel, source=source)
        row(name, 'PASS', app=kind, pid=app.pid, types=dropped['types'],
            channel=channel, uri_length=dropped['uri_length'],
            uri_sha256=dropped['uri_sha256'],
            file_sha256=digests[0]['sha256'] if via_file else '')
        return True
    except Exception as error:
        row(name, 'FAIL', reason=str(error))
        raise
    finally:
        for role in roles_used:
            role.close()
        if app:
            app.close()
        server.close()
        x11.close()


def nemo_file_source(roles, environment):
    name = 'nemo-source-file-drop'
    env, x11, epoch, server = session_setup(environment)
    app = None
    roles_used = []
    try:
        root = files_root(env)
        src_dir = root / 'nemo-src'
        src_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        real = place(src_dir, SOURCE_FILE, SOURCE_BODY)
        app = harness.launch('nemo', str(src_dir), environment)
        window = x11.wait_window('Nemo')[0]
        pid = x11.xres_pid(window['xid'])
        exe = app.verify_window_pid(pid)
        wait_task_headroom()
        sentinel = Role(roles, 'sentinel', env); roles_used.append(sentinel); sentinel.ready()
        catcher = Role(roles, 'catcher', env); roles_used.append(catcher); catcher.ready()
        clipboard(sentinel, catcher)
        generation = 2
        icon = (window['x'] + 236, window['y'] + 128)
        owner = None
        for attempt in range(3):
            x11.raise_window(window['xid'])
            x11.focus(window['xid'])
            x11.motion(*icon)
            x11.button(True)
            time.sleep(0.25)
            for step in range(1, 8):
                x11.motion(icon[0] + 4 * step, icon[1])
                time.sleep(0.03)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                owner = x11.selection_owner()
                if owner:
                    break
                time.sleep(0.05)
            if owner:
                break
            x11.button(False)
            time.sleep(0.4)
        check(owner, f'nemo never initiated a file drag from {icon}; window={window}')
        x11.set_cardinal(owner, APP_GENERATION_ATOM, generation)
        exact(catcher.command(ARM_APP, owner, pid, epoch, generation=generation),
              gen=generation, armed=1, app=1)
        catcher.command(STATUS)
        # nemo's window covers the catcher's slot; the source window raised for
        # the press keeps stacking above the catcher on metacity, so XdndEnter
        # never arrives. Park the catcher in screen space nemo cannot cover.
        x11.move_window(catcher.last['xid'], 1090, 600)
        time.sleep(0.3)
        catcher.command(STATUS)
        cx, cy = catcher.last['x'], catcher.last['y']
        x11.raise_window(catcher.last['xid'])
        time.sleep(0.2)
        accepted = None
        deadline = time.monotonic() + 15
        steps = 6
        while time.monotonic() < deadline:
            x11.drag_path(icon, (cx, cy), steps=min(steps, 24))
            for wiggle in range(4):
                x11.motion(cx + (10 if wiggle % 2 else -10), cy)
                time.sleep(0.1)
            x11.motion(cx, cy)
            report = catcher.command(STATUS)
            if report['visits'] >= 1 and report['inside'] == 1:
                accepted = report
                break
            steps += 4
        check(accepted is not None, f'nemo file drag never reached catcher; last={catcher.last}')
        exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
        received = wait(catcher, lambda r: r['received'] == 1, 'catcher reads nemo file selection')
        exact(received, drop=0, stale=0, app=1)
        body = catcher.command(FETCH)
        # nemo answers the UTF8_STRING read with the bare absolute path(s), not
        # a file:// URI; that plain-path text is its real native source format.
        check(str(real).encode() in body,
              f'nemo selection does not name the real file: {body!r}')
        exact(catcher.command(FILE_VERIFY, *str(real.relative_to(root)).encode()),
              file_reads=1, file_bad=0)
        words = tuple(int(hashlib.sha256(SOURCE_BODY).hexdigest()[i * 8:(i + 1) * 8], 16)
                      for i in range(8))
        check(tuple(catcher.last[f'digest{i}'] for i in range(8)) == words,
              f'FILE_VERIFY digest mismatch: {catcher.last}')
        x11.drag_path((cx, cy), (60, 640), steps=12)
        x11.release()
        clipboard(sentinel, catcher)
        evidence(name, sentinel=sentinel, catcher=catcher)
        row(name, 'PASS', app='nemo', pid=pid, exe=exe,
            uri_length=len(body), uri_sha256=sha(body.decode()))
        return True
    except Exception as error:
        row(name, 'FAIL', reason=str(error))
        raise
    finally:
        for role in roles_used:
            role.close()
        if app:
            app.close()
        server.close()
        x11.close()


def blocked_source_rows():
    row('chrome-source-file-drop', 'BLOCKED', app='chrome',
        reason='Chromium ignores XTEST drag initiation on X11 (source tripwire '
               'browser_source_synthetic_blocked); file row shares the same '
               'limitation; physical-mouse verification is wave F scope')
    row('firefox-source-file-drop', 'BLOCKED', app='firefox',
        reason='page fixture drags a text/plain payload only; a text/uri-list '
               'drag source from real Firefox is not implemented in this wave')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--roles', required=True, type=Path)
    parser.add_argument('--suite', choices=('nemo', 'browsers', 'all'), default='all')
    args = parser.parse_args(argv)
    check(args.roles.is_absolute() and args.roles.is_file(), '--roles must be an absolute binary')
    env = dict(os.environ)
    blocked_source_rows()
    try:
        if args.suite in ('nemo', 'all'):
            role_file_to_nemo(args.roles, env)
            role_file_to_nemo(args.roles, env, directory=True)
            nemo_file_source(args.roles, env)
        if args.suite in ('browsers', 'all'):
            role_file_to_browser(args.roles, 'firefox', 'firefox', env)
            role_file_to_browser(args.roles, 'chrome', 'Google-chrome', env)
        print(json.dumps({'result': 'APP_FILE_SCENARIOS_DONE'}), flush=True)
        return 0
    except Exception as error:
        print(json.dumps({'result': 'FAIL', 'reason': str(error)}), flush=True)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
