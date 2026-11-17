#!/usr/bin/env python3
"""Cross-process Qt/GTK XDND text matrix inside the private native runner."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

from linux_dnd_protocol import (
    ARM, DISARM, DOWN, ESCAPE, LOAD, MOVE, QUIT, STATUS, UP,
    Failure, Role, check, isolated_environment,
)
from linux_dnd_scenarios import (
    Group, cleanup_source, clipboard, enter, evidence, exact, quiet,
    responsive_status, return_reenter, same_payload, start, wait,
)


class ToolkitGroup(Group):
    def __exit__(self, error_type, error, traceback):
        if error_type:
            for role in self.roles:
                chunk = role.process.stderr.read(8192) or b''
                print(json.dumps({'kind': 'failed_toolkit_snapshot', 'role': role.name,
                                  'pid': role.process.pid, 'exit_code': role.process.poll(),
                                  'last_report': role.last,
                                  'stderr': (bytes(role.errors) + chunk).decode(errors='replace')}), flush=True)
        super().__exit__(error_type, error, traceback)

    def add_toolkit(self, binary, name, env):
        role = Role(binary, name, env)
        self.roles.append(role)
        role.ready()
        return role

    def finish(self):
        for role in reversed(self.roles):
            role.command(QUIT)
            check(role.process.wait(timeout=3) == 0, f'{role.name}: unclean toolkit exit')
            role.expected_exit = 0
            role.errors.extend(role.process.stderr.read() or b'')
            check(not role.errors, f'{role.name}: toolkit stderr: {role.errors.decode(errors="replace")}')
        print(json.dumps({'kind': 'toolkit_process_cleanup', 'result': 'PASS',
                          'exit_codes': [role.process.returncode for role in self.roles]}), flush=True)


def verify_qt_maps(role, kit):
    maps = Path(f'/proc/{role.process.pid}/maps').read_text()
    libraries = [line.split()[-1] for line in maps.splitlines() if '/libQt6' in line]
    check(libraries and all(Path(path).is_relative_to(kit) for path in libraries),
          'Qt role must load only the pinned project kit')
    plugins = [line.split()[-1] for line in maps.splitlines() if '/libqxcb.so' in line]
    check(plugins and all(Path(path).is_relative_to(kit) for path in plugins),
          'Qt role must load the local xcb platform plugin')


def scenario(gtk, qt, kit, env, source_toolkit, target_toolkit, cancel=False):
    name = f'{source_toolkit}-to-{target_toolkit}-return-reenter-' + ('escape' if cancel else 'drop')
    with ToolkitGroup(gtk) as group:
        sentinel = group.add('sentinel', env)
        source = group.add_toolkit(qt if source_toolkit == 'qt' else gtk, 'source', env)
        catcher = group.add('catcher', env)
        target = group.add_toolkit(qt if target_toolkit == 'qt' else gtk, 'target', env)
        for toolkit, role in ((source_toolkit, source), (target_toolkit, target)):
            if toolkit == 'qt':
                verify_qt_maps(role, kit)
        clipboard(sentinel, source, catcher, target)
        xid = start(source)
        source.command(DOWN, expected=2)
        catcher.command(ARM, xid); target.command(ARM, xid)
        enter(source, catcher)
        return_reenter(source, catcher)
        enter(source, target)
        return_reenter(source, target)
        responsive_status(source, begin=1, end=0, down=1, up=0, get=0)
        responsive_status(target, drop=0, received=0)
        if cancel:
            source.command(ESCAPE)
        else:
            source.command(UP)
            received = wait(target, lambda r: r['received'] == 1, 'real toolkit selection read')
            exact(received, drop=1, stale=0)
            same_payload(received, source.last)
        cleanup_source(source, cancel, int(not cancel), returned=2)
        quiet(target, lambda r: r['drop'] == int(not cancel) and r['received'] == int(not cancel))
        exact(target.command(STATUS), visits=2, stale=0)
        exact(catcher.command(STATUS), visits=2, drop=0, received=0)
        clipboard(sentinel, source, catcher, target)
        evidence(name, source=source, catcher=catcher, target=target, sentinel=sentinel)
        group.finish()


def rejected_identity(gtk, qt, env, wrong_owner=False):
    with ToolkitGroup(gtk) as group:
        sentinel = group.add('sentinel', env)
        source, target = group.add('source', env), group.add_toolkit(qt, 'target', env)
        xid = start(source)
        target.command(ARM, sentinel.last['xid'] if wrong_owner else xid,
                       generation=1 if wrong_owner else 2)
        source.command(MOVE, target.last['x'], target.last['y'])
        quiet(target, lambda r: not r['inside'] and not r['received'] and not r['drop'])
        target.command(LOAD, 1, 65, expected=2)
        if not wrong_owner:
            exact(target.command(DISARM, generation=1, expected=1), gen=2, armed=1)
        source.command(MOVE, source.last['x'], source.last['y'])
        source.command(ESCAPE)
        cleanup_source(source, True, 0, returned=0)
        clipboard(sentinel, source, target)
        evidence('qt-reject-' + ('foreign-owner' if wrong_owner else 'stale-generation'),
                 source=source, target=target, sentinel=sentinel)
        group.finish()


def rejected_frames(qt, env):
    frames = (b'1 0 1 \n', b'1 0 0\n', b'0 0 1\n', b'1 -1 1\n',
              b'4294967296 0 1\n', b'1 0 1\x00\n', b'1' * 2048)
    for frame in frames:
        role = Role(qt, 'target', env)
        try:
            role.ready()
            check(os.write(role.process.stdin.fileno(), frame) == len(frame), 'complete test control frame')
            check(role.process.wait(timeout=3) == 1, 'malformed Qt control must fail closed')
            errors = role.process.stderr.read(8192) or b''
            check(b'Qt role FAIL:' in errors, 'explicit Qt protocol refusal')
        finally:
            role.close()
    print(json.dumps({'kind': 'qt_protocol_rejection', 'result': 'PASS', 'cases': len(frames)}), flush=True)


def capture_fixture(gtk, qt, env):
    with ToolkitGroup(gtk) as group:
        source = group.add_toolkit(qt, 'source', env)
        group.add_toolkit(qt, 'target', env)
        quiet(source, lambda r: r['begin'] == 0, .2)
        path = Path(env['OINK_NATIVE_TEST_ROOT']) / 'qt-fixture.xwd'
        subprocess.run(['xwd', '-silent', '-root', '-out', str(path)], env=env,
                       check=True, timeout=5, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        print(json.dumps({'kind': 'private_fixture_screenshot', 'path': str(path)}), flush=True)
        group.finish()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gtk', type=Path, required=True)
    parser.add_argument('--qt', type=Path, required=True)
    parser.add_argument('--kit', type=Path, required=True)
    parser.add_argument('--capture', action='store_true', help='save private fixture XWD inside runner root')
    args = parser.parse_args(argv)
    try:
        for binary in (args.gtk, args.qt):
            check(binary.is_absolute() and binary.is_file() and os.access(binary, os.X_OK),
                  'roles must be absolute compiled executables')
        env = isolated_environment(dict(os.environ))
        if args.capture:
            capture_fixture(args.gtk, args.qt, env)
        for source, target in (('qt', 'qt'), ('qt', 'gtk'), ('gtk', 'qt')):
            for cancel in (False, True):
                scenario(args.gtk, args.qt, args.kit.resolve(), env, source, target, cancel)
        rejected_identity(args.gtk, args.qt, env)
        rejected_identity(args.gtk, args.qt, env, wrong_owner=True)
        rejected_frames(args.qt, env)
        print(json.dumps({'result': 'IMPLEMENTED_SCENARIOS_PASS', 'scenarios': 8,
                          'scope': 'private-core-XTEST-cross-process-Qt-GTK-text',
                          'not_covered': ['files/INCR', 'real applications', 'dedicated-master toolkit input',
                                          'product transport', 'hardware']}), flush=True)
        return 0
    except (Failure, OSError, ValueError, subprocess.SubprocessError) as error:
        print(json.dumps({'result': 'FAIL', 'reason': str(error)}), flush=True)
        return 1


if __name__ == '__main__':
    sys.exit(main())
