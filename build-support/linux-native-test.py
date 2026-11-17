#!/usr/bin/env python3
"""Run native probes against private X11, D-Bus and hardware-free audio services."""
import argparse
import json
import os
from pathlib import Path
import resource
import re
import secrets
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time

PIPEWIRE_CONFIG = '''context.properties = {
    core.daemon = true
    core.name = oink-test
    support.dbus = false
}
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-spa-node-factory }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-access
      args = { access.socket = { oink-test = "unrestricted" } } }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-link-factory }
]
context.objects = [
    { factory = spa-node-factory args = {
        factory.name = support.node.driver
        node.name = Dummy-Driver
        node.group = pipewire.dummy
        priority.driver = 20000
    } }
]
'''
CLIENT_CONFIG = '''context.properties = { support.dbus = false }
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-client-device }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-session-manager }
]
'''
PULSE_CONFIG = '''context.properties = { support.dbus = false }
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-protocol-pulse }
]
pulse.properties = { server.address = [ "unix:SOCKET_PATH" ] }
pulse.cmd = [
    { cmd = "load-module" args = "module-null-sink sink_name=oink_test" }
]
'''


def private_environment(root, original):
    home = root / 'home'
    config = root / 'config'
    runtime = root / 'runtime'
    for directory in (home, config, runtime, root / 'data', root / 'cache',
                      root / 'state', root / 'oink', root / 'mounts', root / 'tmp'):
        directory.mkdir(mode=0o700)
    return {
        'PATH': original.get('PATH', '/usr/bin:/bin'),
        'HOME': str(home), 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8',
        'XDG_CONFIG_HOME': str(config), 'XDG_DATA_HOME': str(root / 'data'),
        'XDG_CACHE_HOME': str(root / 'cache'), 'XDG_STATE_HOME': str(root / 'state'),
        'XDG_RUNTIME_DIR': str(runtime), 'XDG_SESSION_TYPE': 'x11',
        'XAUTHORITY': str(root / 'Xauthority'), 'DISPLAY': ':invalid',
        'OINK_HOME': str(root / 'oink'), 'OINK_NATIVE_TEST_ROOT': str(root),
        'TMPDIR': str(root / 'tmp'), 'GDK_BACKEND': 'x11', 'QT_QPA_PLATFORM': 'xcb',
        'DBUS_SESSION_BUS_ADDRESS': f'unix:path={runtime}/session-bus',
        'DBUS_SYSTEM_BUS_ADDRESS': f'unix:path={runtime}/no-system-bus',
        'PIPEWIRE_RUNTIME_DIR': str(runtime), 'PIPEWIRE_REMOTE': 'oink-test',
        'PIPEWIRE_CONFIG_DIR': str(config / 'pipewire'),
        'PULSE_SERVER': f'unix:{runtime}/pulse-native',
        'PULSE_CLIENTCONFIG': str(config / 'pulse-client.conf'),
        'CARGO_HOME': original.get('CARGO_HOME', str(Path(original['HOME']) / '.cargo')),
        'RUSTUP_HOME': original.get('RUSTUP_HOME', str(Path(original['HOME']) / '.rustup')),
        'CARGO_BUILD_JOBS': '2', 'RUST_TEST_THREADS': '2',
    }


def mounts_below(root, mountinfo=None):
    if mountinfo is None:
        mountinfo = Path('/proc/self/mountinfo').read_text()
    found = []
    for line in mountinfo.splitlines():
        value = line.split()[4]
        for code, character in [('040', ' '), ('011', '\t'), ('012', '\n'), ('134', '\\')]:
            value = value.replace('\\' + code, character)
        path = Path(value)
        if path == root or root in path.parents:
            found.append(path)
    return found


class Session:
    def __init__(self, root, environment):
        self.root = root
        self.env = environment
        self.processes = []
        self.logs = []

    def start(self, name, command, environment=None, pass_fds=()):
        log = (self.root / f'{name}.log').open('wb')
        self.logs.append(log)
        process = subprocess.Popen(command, env=environment or self.env,
                                   stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                   start_new_session=True, pass_fds=pass_fds)
        self.processes.append((name, process))
        return process

    def require_alive(self):
        for name, process in self.processes:
            if process.poll() is not None:
                raise RuntimeError(f'{name} exited with {process.returncode}; see {self.root}/{name}.log')

    def run(self, command, timeout=10, **kwargs):
        return subprocess.run(command, env=self.env, timeout=timeout, check=True,
                              capture_output=True, text=True, **kwargs)

    def wait_ready(self, probe, seconds=10):
        deadline = time.monotonic() + seconds
        last = None
        while time.monotonic() < deadline:
            self.require_alive()
            try:
                return probe()
            except (OSError, subprocess.SubprocessError) as error:
                last = error
                time.sleep(0.05)
        detail = getattr(last, 'stderr', '') or str(last)
        raise RuntimeError(f'Private service did not become ready: {detail}')

    def start_x11(self, host):
        authority = Path(self.env['XAUTHORITY'])
        authority.touch(mode=0o600)
        cookie = secrets.token_hex(16)
        self.run(['xauth', '-f', str(authority)],
                 input=f'add :999 MIT-MAGIC-COOKIE-1 {cookie}\n')
        read_fd, write_fd = os.pipe()
        try:
            self.start('xephyr', ['Xephyr', '-displayfd', str(write_fd),
                       '-auth', str(authority), '-nolisten', 'tcp', '-no-host-grab',
                       '-screen', '1280x800', '-noreset'], host, (write_fd,))
            os.close(write_fd)
            write_fd = None
            if not select.select([read_fd], [], [], 10)[0]:
                raise RuntimeError('Xephyr did not assign a display within 10 seconds')
            number = os.read(read_fd, 64).decode('ascii').strip()
            if not number.isdecimal():
                raise RuntimeError('Xephyr returned an invalid display number')
        finally:
            os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)
        display = f':{number}'
        if display == host['DISPLAY'].split('.')[0]:
            raise RuntimeError('Refusing to use the host display for native probes')
        self.env['DISPLAY'] = display
        self.run(['xauth', '-f', str(authority)],
                 input=f'add {display} MIT-MAGIC-COOKIE-1 {cookie}\nremove :999\n')
        self.wait_ready(lambda: self.run(['xdpyinfo'], timeout=1))

    def block_host_input(self):
        # Xephyr otherwise forwards real host input into synthetic-only probes.
        for name in ('Xephyr virtual keyboard', 'Xephyr virtual mouse'):
            device = self.run(['xinput', 'list', '--id-only', name]).stdout.strip()
            if not device.isascii() or not device.isdecimal():
                raise RuntimeError('Cannot uniquely identify private Xephyr input device')
            actual = self.run(['xinput', 'list', '--name-only', device]).stdout.strip()
            if actual != name:
                raise RuntimeError('Refusing to disable an unexpected private input device')
            self.run(['xinput', 'disable', device])
            properties = self.run(['xinput', 'list-props', device]).stdout
            if not re.search(r'^\s*Device Enabled \(\d+\):\s*0\s*$', properties, re.MULTILINE):
                raise RuntimeError('Private Xephyr host-input forwarding is still enabled')

    def start_dbus(self):
        self.start('dbus', ['dbus-daemon', '--session', '--nofork',
                   '--address=' + self.env['DBUS_SESSION_BUS_ADDRESS']])
        self.wait_ready(lambda: self.run(['dbus-send', '--session', '--print-reply',
                        '--dest=org.freedesktop.DBus', '/org/freedesktop/DBus',
                        'org.freedesktop.DBus.ListNames'], timeout=1))

    def start_audio(self):
        config = Path(self.env['PIPEWIRE_CONFIG_DIR'])
        config.mkdir(mode=0o700, exist_ok=True)
        (config / 'pipewire.conf').write_text(PIPEWIRE_CONFIG)
        (config / 'client.conf').write_text(CLIENT_CONFIG)
        socket = self.root / 'runtime' / 'pulse-native'
        (config / 'pipewire-pulse.conf').write_text(
            PULSE_CONFIG.replace('SOCKET_PATH', str(socket)))
        Path(self.env['PULSE_CLIENTCONFIG']).write_text('autospawn = no\n')
        self.start('pipewire', ['pipewire', '-c', str(config / 'pipewire.conf')])
        self.wait_ready(lambda: self.run(['pw-cli', 'info', '0'], timeout=1))
        self.start('pulse', ['pipewire-pulse', '-c', str(config / 'pipewire-pulse.conf')])
        self.wait_ready(lambda: self.run(['pactl', 'info'], timeout=1))
        sources = json.loads(self.run(['pactl', '-f', 'json', 'list', 'sources']).stdout)
        sinks = json.loads(self.run(['pactl', '-f', 'json', 'list', 'sinks']).stdout)
        if ([sink['name'] for sink in sinks] != ['oink_test']
                or [source['name'] for source in sources] != ['oink_test.monitor']):
            raise RuntimeError('Private audio service did not expose only the test sink/monitor')

    def execute(self, command, timeout):
        process = subprocess.Popen(command, env=self.env, start_new_session=True)
        self.processes.append(('probe', process))
        try:
            return process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            raise RuntimeError(f'Native probe exceeded {timeout} seconds') from None

    def close(self):
        errors = []
        for _, process in reversed(self.processes):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            except OSError as error:
                errors.append(error)
        for _, process in reversed(self.processes):
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
            except OSError as error:
                errors.append(error)
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError as error:
                errors.append(error)
            try:
                process.wait(timeout=2)
            except (OSError, subprocess.SubprocessError) as error:
                errors.append(error)
        for log in self.logs:
            try:
                log.close()
            except OSError as error:
                errors.append(error)
        if errors:
            raise errors[0]


def require_resource_limits():
    entry = next(line[3:] for line in Path('/proc/self/cgroup').read_text().splitlines()
                 if line.startswith('0::'))
    scope = Path('/sys/fs/cgroup') / entry.lstrip('/')
    memory = (scope / 'memory.max').read_text().strip()
    quota, period = (scope / 'cpu.max').read_text().split()
    tasks = (scope / 'pids.max').read_text().strip()
    if (memory == 'max' or int(memory) > 4 * 1024 ** 3
            or quota == 'max' or int(quota) > 2 * int(period)
            or tasks == 'max' or int(tasks) > 256):
        raise RuntimeError('Native tests require MemoryMax<=4G, CPUQuota<=200% and TasksMax<=256')


def interrupt(_signum, _frame):
    raise InterruptedError('Native probe interrupted')


def isolated_run(args):
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    root = Path(tempfile.mkdtemp(prefix='oink-native-', dir='/tmp'))
    roots, sessions, environments = [root], [], []
    host = os.environ.copy()
    result, probe_exit = 1, None
    services_ok, cleanup_ok, mount_check_ok = False, True, True
    remaining = []
    try:
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, interrupt)
        for index in range(2 if args.peer_x11 else 1):
            if index:
                roots.append(Path(tempfile.mkdtemp(prefix='oink-native-', dir='/tmp')))
            env = private_environment(roots[index], host)
            session = Session(roots[index], env)
            sessions.append(session)
            environments.append(env)
            session.start_x11(host)
            if any(previous['DISPLAY'] == env['DISPLAY'] for previous in environments[:-1]):
                raise RuntimeError('Peer X11 servers must have distinct displays')
            session.block_host_input()
            session.start_dbus()
            session.start('metacity', ['metacity', '--sm-disable'])
            session.start_audio()
            session.require_alive()
            print(f'Isolated native environment: DISPLAY={env["DISPLAY"]}, root={roots[index]}', flush=True)
        if args.peer_x11:
            peer = root / 'peer-environment.json'
            fd = os.open(peer, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as output:
                json.dump(environments[1], output)
            environments[0]['OINK_NATIVE_TEST_PEER_ENV'] = str(peer)
        probe_exit = sessions[0].execute(args.command, args.timeout)
        result = probe_exit
        for session in sessions:
            for name, process in session.processes:
                if name != 'probe' and process.poll() is not None:
                    raise RuntimeError(f'Private service {name} exited during the probe')
        services_ok = True
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Native test failed: {error}', file=sys.stderr)
        result = 1
    finally:
        for session in reversed(sessions):
            try:
                session.close()
            except (OSError, subprocess.SubprocessError) as error:
                print(f'Native test cleanup failed: {error}', file=sys.stderr)
                cleanup_ok = False
                result = 1
        for directory in roots:
            try:
                remaining.extend(mounts_below(directory))
            except OSError as error:
                print(f'Cannot verify remaining mounts: {error}', file=sys.stderr)
                mount_check_ok = False
                result = 1
        if remaining:
            print('Active mounts remain; refusing recursive deletion.', file=sys.stderr)
            cleanup_ok = False
            result = 1
        for directory in roots:
            if args.keep or result != 0:
                print(f'Private test artifacts retained: {directory}', file=sys.stderr)
            else:
                try:
                    shutil.rmtree(directory)
                except OSError as error:
                    print(f'Private artifact cleanup failed: {directory}: {error}', file=sys.stderr)
                    cleanup_ok = False
                    result = 1
        print('native_test_summary=' + json.dumps({
            'probe_exit': probe_exit, 'services_ok': services_ok,
            'cleanup_ok': cleanup_ok, 'mount_check_ok': mount_check_ok,
            'remaining_mounts': len(remaining), 'sessions': len(sessions),
            'result': result,
        }, sort_keys=True), flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--timeout', type=int, default=120)
    parser.add_argument('--keep', action='store_true', help='Keep private logs and fixtures')
    parser.add_argument('--peer-x11', action='store_true', help='Start a second isolated peer session')
    parser.add_argument('--inside-scope', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command[:1] == ['--']:
        args.command.pop(0)
    if not args.command or args.timeout < 1:
        parser.error('Supply a command after -- and a positive --timeout')
    if sys.platform != 'linux' or os.geteuid() == 0:
        parser.error('Run as an ordinary Linux user, never root')
    if not os.environ.get('DISPLAY'):
        parser.error('A host X display is required for Xephyr')
    if not args.inside_scope:
        command = ['systemd-run', '--user', '--scope', '--quiet', '-p', 'MemoryMax=4G',
                   '-p', 'CPUQuota=200%', '-p', 'TasksMax=256',
                   sys.executable, str(Path(__file__).resolve()), '--inside-scope',
                   *sys.argv[1:]]
        return subprocess.call(command)
    try:
        require_resource_limits()
    except (OSError, RuntimeError, ValueError, StopIteration) as error:
        parser.error(f'Missing bounded cgroup v2 scope: {error}')
    return isolated_run(args)


if __name__ == '__main__':
    sys.exit(main())
