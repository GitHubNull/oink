"""Device-free safety contracts for the Linux native test runner."""
import argparse
from contextlib import contextmanager, ExitStack
import importlib.util
import io
import json
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


RUNNER = Path(__file__).with_name('linux-native-test.py')
if sys.platform == 'linux':
    SPEC = importlib.util.spec_from_file_location('linux_native_test', RUNNER)
    runner = importlib.util.module_from_spec(SPEC)
    SPEC.loader.exec_module(runner)
HOST = {'HOME': '/host/home', 'DISPLAY': ':42.0', 'PATH': '/usr/bin:/bin',
        'DBUS_SESSION_BUS_ADDRESS': 'unix:path=/private-session-bus'}


@unittest.skipUnless(sys.platform == 'linux', 'Linux-only native test runner')
class SafeCase(unittest.TestCase):
    def setUp(self):
        # An accidentally unmocked service launch or host mutation must fail closed.
        self.stack = self.enterContext(ExitStack())
        for target in ('subprocess.Popen', 'subprocess.run', 'subprocess.call',
                       'os.killpg', 'signal.signal', 'resource.setrlimit'):
            self.stack.enter_context(mock.patch.object(
                getattr(runner, target.split('.')[0]), target.split('.')[1],
                side_effect=AssertionError('Unmocked host operation: ' + target)))
        self.root = self.new_root()

    def new_root(self):
        return Path(self.enterContext(tempfile.TemporaryDirectory()))

    def session(self):
        root = self.new_root()
        return runner.Session(root, runner.private_environment(root, dict(HOST)))


class EnvironmentTests(SafeCase):
    def test_whitelist_replaces_host_display_audio_and_oink_data(self):
        host = dict(HOST, XAUTHORITY='/host/auth', PULSE_SERVER='unix:/host/pulse',
                    PULSE_CLIENTCONFIG='/host/pulse.conf', PULSE_COOKIE='/host/cookie',
                    PIPEWIRE_REMOTE='host-core', PIPEWIRE_RUNTIME_DIR='/host/runtime',
                    PIPEWIRE_CONFIG_DIR='/host/pipewire', WAYLAND_DISPLAY='wayland-0',
                    WAYLAND_SOCKET='17', OINK_HOME='/host/oink', OINK_SECRET='secret',
                    OINK_NATIVE_TEST_ROOT='/host/test', DBUS_SYSTEM_BUS_ADDRESS='host',
                    XDG_RUNTIME_DIR='/host/run', XDG_CONFIG_HOME='/host/config',
                    XDG_DATA_HOME='/host/data', XDG_CACHE_HOME='/host/cache',
                    XDG_STATE_HOME='/host/state', XDG_SESSION_TYPE='wayland',
                    TMPDIR='/host/tmp', GDK_BACKEND='wayland', QT_QPA_PLATFORM='wayland',
                    LD_PRELOAD='/host/inject.so', PYTHONPATH='/host/python',
                    UNRELATED_SECRET='secret', CARGO_HOME='/host/cargo',
                    RUSTUP_HOME='/host/rustup')
        before = host.copy()
        env = runner.private_environment(self.root, host)
        allowed = {'PATH', 'CARGO_HOME', 'RUSTUP_HOME'}
        for key in host:
            with self.subTest(variable=key):
                if key in allowed:
                    self.assertEqual(env[key], host[key])
                else:
                    self.assertNotEqual(env.get(key), host[key])
        for key in ('WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'PULSE_COOKIE', 'OINK_SECRET',
                    'LD_PRELOAD', 'PYTHONPATH', 'UNRELATED_SECRET'):
            self.assertNotIn(key, env)
        self.assertEqual(host, before)
        self.assertEqual(env['DISPLAY'], ':invalid')
        self.assertEqual(env['XDG_SESSION_TYPE'], 'x11')

    def test_private_paths_and_permissions_and_blocked_system_bus(self):
        env = runner.private_environment(self.root, dict(HOST))
        for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME',
                    'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'OINK_HOME', 'TMPDIR'):
            path = Path(env[key])
            self.assertIn(self.root, path.parents)
            self.assertTrue(path.is_dir())
        for directory in (self.root, *self.root.iterdir()):
            self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o700)
        address = env['DBUS_SYSTEM_BUS_ADDRESS']
        self.assertTrue(address.startswith('unix:path='))
        blocked = Path(address.removeprefix('unix:path='))
        self.assertIn(self.root, blocked.parents)
        self.assertFalse(blocked.exists())
        self.assertIn(str(self.root), env['DBUS_SESSION_BUS_ADDRESS'])
        self.assertNotEqual(env['DBUS_SESSION_BUS_ADDRESS'], HOST['DBUS_SESSION_BUS_ADDRESS'])
        self.assertIn(str(self.root), env['PULSE_SERVER'])
        self.assertEqual(env['PIPEWIRE_RUNTIME_DIR'], env['XDG_RUNTIME_DIR'])

    def test_minimal_environment_has_toolchain_defaults(self):
        env = runner.private_environment(self.root, {k: HOST[k] for k in
                                         ('HOME', 'DBUS_SESSION_BUS_ADDRESS')})
        self.assertEqual(env['PATH'], '/usr/bin:/bin')
        self.assertEqual(env['CARGO_HOME'], '/host/home/.cargo')
        self.assertEqual(env['RUSTUP_HOME'], '/host/home/.rustup')


class MountTests(SafeCase):
    @staticmethod
    def mountinfo(paths):
        def encode(path):
            return str(path).replace('\\', r'\134').replace(' ', r'\040').replace(
                '\t', r'\011').replace('\n', r'\012')
        return '\n'.join(f'1 0 0:1 / {encode(p)} rw - tmpfs tmpfs rw' for p in paths)

    def test_escaped_names_root_and_component_boundaries(self):
        root = Path('/tmp/native root\t\n\\040')
        inside = [root, root / 'mount one', root / 'nested' / 'tab\tline\nslash\\']
        outside = [root.parent, Path(str(root) + '-other') / 'mount', Path('/elsewhere')]
        self.assertEqual(runner.mounts_below(root, self.mountinfo(inside + outside)), inside)

    def test_reads_mountinfo_and_empty_input_is_safe(self):
        with mock.patch.object(Path, 'read_text', return_value=self.mountinfo([self.root])) as read:
            self.assertEqual(runner.mounts_below(self.root), [self.root])
            read.assert_called_once_with()
        self.assertEqual(runner.mounts_below(self.root, ''), [])


class SessionTests(SafeCase):
    def test_start_tracks_private_process_group_and_log(self):
        session = self.session()
        process = mock.Mock(pid=123, returncode=0)
        with mock.patch.object(runner.subprocess, 'Popen', return_value=process) as popen:
            self.assertIs(session.start('service', ['fake'], pass_fds=(71,)), process)
        kwargs = popen.call_args.kwargs
        self.assertIs(kwargs['env'], session.env)
        self.assertTrue(kwargs['start_new_session'])
        self.assertEqual(kwargs['pass_fds'], (71,))
        self.assertEqual(kwargs['stdin'], subprocess.DEVNULL)
        self.assertIs(kwargs['stdout'], kwargs['stderr'])
        self.assertIn(('service', process), session.processes)
        with mock.patch.object(runner.os, 'killpg'):
            session.close()
        self.assertTrue(kwargs['stdout'].closed)

    def test_dbus_uses_private_session_address_and_never_system_bus(self):
        session = self.session()
        with mock.patch.object(session, 'start') as start, mock.patch.object(session, 'run') as run:
            session.start_dbus()
        command = start.call_args.args[1]
        self.assertEqual(command[0], 'dbus-daemon')
        self.assertIn('--session', command)
        self.assertIn('--nofork', command)
        self.assertIn('--address=' + session.env['DBUS_SESSION_BUS_ADDRESS'], command)
        self.assertNotIn('--system', command)
        self.assertTrue(any('--session' in c.args[0] for c in run.call_args_list))
        env = start.call_args.kwargs['environment']
        self.assertEqual(env['XDG_DATA_DIRS'], str(session.root / 'data'))
        self.assertNotIn('XDG_DATA_DIRS', session.env)

    def test_dbus_activation_exposes_only_gvfs_core(self):
        session = self.session()
        with mock.patch.object(session, 'start'), mock.patch.object(session, 'run'):
            session.start_dbus()
        services = (session.root / 'data' / 'dbus-1' / 'services').iterdir()
        names = {p.name for p in services}
        self.assertTrue(names <= {'org.gtk.vfs.Daemon.service', 'org.gtk.vfs.Metadata.service'})
        self.assertNotIn('org.freedesktop.portal.Desktop.service', names)
        for path in (session.root / 'data' / 'dbus-1' / 'services').iterdir():
            if path.name.startswith('org.gtk.vfs.'):
                self.assertTrue(path.is_symlink())

    def test_run_is_checked_bounded_and_private(self):
        session = self.session()
        with mock.patch.object(runner.subprocess, 'run') as run:
            session.run(['fake'], timeout=3, input='data')
        self.assertEqual(run.call_args.args[0], ['fake'])
        self.assertIs(run.call_args.kwargs['env'], session.env)
        self.assertTrue(run.call_args.kwargs['check'])
        self.assertEqual(run.call_args.kwargs['timeout'], 3)
        self.assertEqual(run.call_args.kwargs['input'], 'data')

    def test_wait_ready_retries_transient_failures_but_is_bounded(self):
        session = self.session()
        probe = mock.Mock(side_effect=[OSError('not ready'), 'ready'])
        with mock.patch.object(runner.time, 'monotonic', side_effect=[0, 1, 2]), \
                mock.patch.object(runner.time, 'sleep'):
            self.assertEqual(session.wait_ready(probe, seconds=3), 'ready')
        with mock.patch.object(runner.time, 'monotonic', side_effect=[0, 1, 4]), \
                mock.patch.object(runner.time, 'sleep'):
            with self.assertRaises(RuntimeError):
                session.wait_ready(mock.Mock(side_effect=subprocess.CalledProcessError(1, 'fake')), 3)

    def test_dead_service_aborts_readiness_before_probe(self):
        session = self.session()
        session.processes = [('service', mock.Mock(poll=mock.Mock(return_value=9), returncode=9))]
        probe = mock.Mock()
        with self.assertRaises(RuntimeError):
            session.wait_ready(probe)
        probe.assert_not_called()

    def test_execute_tracks_probe_on_exit_and_timeout(self):
        for outcome in (7, subprocess.TimeoutExpired('fake', 3)):
            with self.subTest(outcome=outcome):
                session = self.session()
                process = mock.Mock(pid=123)
                process.wait.side_effect = [outcome]
                with mock.patch.object(runner.subprocess, 'Popen', return_value=process) as popen:
                    if isinstance(outcome, Exception):
                        with self.assertRaises(RuntimeError):
                            session.execute(['fake'], 3)
                    else:
                        self.assertEqual(session.execute(['fake'], 3), outcome)
                self.assertIn(('probe', process), session.processes)
                process.wait.assert_called_once_with(timeout=3)
                self.assertIs(popen.call_args.kwargs['env'], session.env)
                self.assertTrue(popen.call_args.kwargs['start_new_session'])

    def test_close_terminates_then_kills_groups_even_if_leader_exited(self):
        session = self.session()
        exited, stuck = mock.Mock(pid=123), mock.Mock(pid=456)
        stuck.wait.side_effect = [subprocess.TimeoutExpired('fake', 2), 0]
        session.processes = [('exited', exited), ('stuck', stuck)]
        log = io.BytesIO()
        session.logs = [log]
        with mock.patch.object(runner.os, 'killpg') as kill:
            session.close()
        for process in (exited, stuck):
            kill.assert_any_call(process.pid, signal.SIGTERM)
            kill.assert_any_call(process.pid, signal.SIGKILL)
            self.assertTrue(process.wait.called)
        self.assertTrue(log.closed)

    def test_close_tolerates_already_absent_process_groups(self):
        session = self.session()
        session.processes = [('gone', mock.Mock(pid=123))]
        with mock.patch.object(runner.os, 'killpg', side_effect=ProcessLookupError):
            session.close()

    def test_cleanup_still_reaps_other_groups_and_closes_logs_after_wait_failure(self):
        session = self.session()
        other, stuck = mock.Mock(pid=123), mock.Mock(pid=456)
        stuck.wait.side_effect = subprocess.TimeoutExpired('fake', 2)
        session.processes = [('other', other), ('stuck', stuck)]
        session.logs = [io.BytesIO()]
        with mock.patch.object(runner.os, 'killpg') as kill:
            try:
                session.close()
            except subprocess.SubprocessError:
                pass  # Raising is allowed, abandoning the other resources is not.
        self.assertTrue(other.wait.called, 'Cleanup abandoned another process')
        kill.assert_any_call(other.pid, signal.SIGKILL)
        self.assertTrue(session.logs[0].closed, 'Cleanup abandoned an open log')


class X11Tests(SafeCase):
    @contextmanager
    def fake_x11(self, session, payload=b'73\n', ready=True, start_error=None):
        real_close = runner.os.close
        with ExitStack() as stack:
            start = stack.enter_context(mock.patch.object(session, 'start', side_effect=start_error))
            run = stack.enter_context(mock.patch.object(session, 'run'))
            stack.enter_context(mock.patch.object(runner.os, 'pipe', return_value=(71, 72)))
            close = stack.enter_context(mock.patch.object(runner.os, 'close',
                side_effect=lambda fd: None if fd in (71, 72) else real_close(fd)))
            stack.enter_context(mock.patch.object(runner.os, 'read', return_value=payload))
            select = stack.enter_context(mock.patch.object(
                runner.select, 'select', return_value=([71] if ready else [], [], [])))
            yield start, run, close, select

    def test_nested_x11_requires_auth_no_host_grab_and_no_tcp(self):
        session = self.session()
        with self.fake_x11(session) as (start, run, close, select):
            session.start_x11(dict(HOST))
        command = start.call_args_list[0].args[1]
        self.assertEqual(command[0], 'Xephyr')
        self.assertIn('-no-host-grab', command)
        self.assertEqual(command[command.index('-nolisten') + 1], 'tcp')
        self.assertEqual(command[command.index('-auth') + 1], session.env['XAUTHORITY'])
        self.assertNotIn('-ac', command)
        self.assertEqual(start.call_args_list[0].args[2]['DISPLAY'], HOST['DISPLAY'])
        self.assertIn(int(command[command.index('-displayfd') + 1]), start.call_args_list[0].args[3])
        self.assertEqual(session.env['DISPLAY'], ':73')
        self.assertEqual(stat.S_IMODE(Path(session.env['XAUTHORITY']).stat().st_mode), 0o600)
        auth_inputs = [c.kwargs['input'] for c in run.call_args_list if c.args[0][0] == 'xauth']
        self.assertTrue(all('MIT-MAGIC-COOKIE-1' in text for text in auth_inputs))
        self.assertIn('add :73 ', auth_inputs[-1])
        self.assertTrue(any(c.args[0][0] == 'xdpyinfo' for c in run.call_args_list))
        self.assertGreater(select.call_args.args[3], 0)
        close.assert_has_calls([mock.call(71), mock.call(72)], any_order=True)

    def test_displayfd_invalid_eof_non_ascii_and_host_display_are_rejected(self):
        for payload in (b'', b'not-a-display\n', b'-1\n', b'7\n8\n', b'\xff', b'42\n'):
            with self.subTest(payload=payload):
                session = self.session()
                with self.fake_x11(session, payload) as (start, _, close, _):
                    with self.assertRaises((RuntimeError, UnicodeError)):
                        session.start_x11(dict(HOST))
                self.assertEqual(session.env['DISPLAY'], ':invalid')
                self.assertEqual(start.call_count, 1)
                close.assert_has_calls([mock.call(71), mock.call(72)], any_order=True)

    def test_displayfd_timeout_and_launch_error_close_both_pipe_ends(self):
        for ready, error in ((False, None), (True, OSError('launch failed'))):
            with self.subTest(ready=ready, error=error):
                session = self.session()
                with self.fake_x11(session, ready=ready, start_error=error) as (_, run, close, _):
                    with self.assertRaises((RuntimeError, OSError)):
                        session.start_x11(dict(HOST))
                self.assertEqual(session.env['DISPLAY'], ':invalid')
                self.assertFalse(any(c.args[0][0] == 'xdpyinfo' for c in run.call_args_list))
                close.assert_has_calls([mock.call(71), mock.call(72)], any_order=True)


class AudioTests(SafeCase):
    def start_audio(self, session, sources, sinks):
        def run(command, **_kwargs):
            names = sources if command[-1] == 'sources' else sinks
            return subprocess.CompletedProcess(command, 0, json.dumps([{'name': n} for n in names]))
        with mock.patch.object(session, 'start') as start, \
                mock.patch.object(session, 'run', side_effect=run):
            session.start_audio()
        return start

    def test_private_audio_configs_disable_dbus_and_hardware_discovery(self):
        session = self.session()
        start = self.start_audio(session, ['oink_test.monitor'], ['oink_test'])
        config = Path(session.env['PIPEWIRE_CONFIG_DIR'])
        self.assertEqual(stat.S_IMODE(config.stat().st_mode), 0o700)
        texts = [path.read_text() for path in config.glob('*.conf')]
        self.assertGreaterEqual(len(texts), 2)
        for text in texts:
            self.assertRegex(text, r'support\.dbus\s*=\s*false')
            self.assertNotRegex(text.lower(), r'alsa|bluez|v4l2|udev|jack|wireplumber')
        pulse = (config / 'pipewire-pulse.conf').read_text()
        self.assertIn(str(session.root / 'runtime' / 'pulse-native'), pulse)
        self.assertNotIn('SOCKET_PATH', pulse)
        self.assertIn('module-null-sink sink_name=oink_test', pulse)
        self.assertIn('autospawn = no', Path(session.env['PULSE_CLIENTCONFIG']).read_text())
        self.assertEqual({c.args[1][0] for c in start.call_args_list}, {'pipewire', 'pipewire-pulse'})

    def test_audio_rejects_missing_extra_duplicate_or_physical_endpoints(self):
        for sources, sinks in (([], ['oink_test']), (['oink_test.monitor'], []),
                               (['microphone'], ['oink_test']),
                               (['oink_test.monitor', 'microphone'], ['oink_test']),
                               (['oink_test.monitor'], ['oink_test', 'speakers']),
                               (['oink_test.monitor'] * 2, ['oink_test'])):
            with self.subTest(sources=sources, sinks=sinks):
                with self.assertRaises(RuntimeError):
                    self.start_audio(self.session(), sources, sinks)


class IsolatedRunTests(SafeCase):
    @contextmanager
    def isolation(self, result=0, mounts=(), keep=False):
        root = self.new_root()
        session = mock.Mock(processes=[])
        session.execute.return_value = result
        args = argparse.Namespace(command=['fake', '--arg'], timeout=9, keep=keep, peer_x11=False)
        with ExitStack() as stack:
            stack.enter_context(mock.patch.dict(runner.os.environ, HOST, clear=True))
            stack.enter_context(mock.patch.object(runner.tempfile, 'mkdtemp', return_value=str(root)))
            stack.enter_context(mock.patch.object(runner, 'Session', return_value=session))
            limits = stack.enter_context(mock.patch.object(runner.resource, 'setrlimit'))
            signals = stack.enter_context(mock.patch.object(runner.signal, 'signal'))
            mounted = stack.enter_context(mock.patch.object(runner, 'mounts_below', return_value=list(mounts)))
            remove = stack.enter_context(mock.patch.object(runner.shutil, 'rmtree'))
            stack.enter_context(mock.patch('sys.stdout', new_callable=io.StringIO))
            stack.enter_context(mock.patch('sys.stderr', new_callable=io.StringIO))
            yield args, root, session, mounted, remove, limits, signals

    def test_success_cleans_only_after_shutdown_and_mount_check(self):
        with self.isolation() as (args, root, session, mounted, remove, limits, signals):
            events = mock.Mock()
            for name, method in [('close', session.close), ('mounts', mounted), ('remove', remove)]:
                events.attach_mock(method, name)
            self.assertEqual(runner.isolated_run(args), 0)
            self.assertEqual(events.mock_calls, [mock.call.close(), mock.call.mounts(root), mock.call.remove(root)])
            limits.assert_called_once_with(runner.resource.RLIMIT_CORE, (0, 0))
            for sig in (signal.SIGINT, signal.SIGTERM):
                signals.assert_any_call(sig, runner.interrupt)
            session.execute.assert_called_once_with(args.command, args.timeout)

    def test_keep_failure_and_remaining_mounts_never_delete_recursively(self):
        for result, keep, mounts, expected in ((0, True, [], 0), (7, False, [], 7),
                                               (0, False, [self.root / 'mount'], 1)):
            with self.subTest(result=result, keep=keep, mounts=mounts):
                with self.isolation(result, mounts, keep) as (args, _, session, _, remove, _, _):
                    self.assertEqual(runner.isolated_run(args), expected)
                    session.close.assert_called_once()
                    remove.assert_not_called()

    def test_startup_probe_timeout_and_interrupt_failures_cleanup_and_keep_artifacts(self):
        failures = [('start_x11', OSError('x11')), ('start_dbus', RuntimeError('dbus')),
                    ('start_audio', RuntimeError('audio')),
                    ('require_alive', RuntimeError('died')), ('execute', RuntimeError('timeout')),
                    ('execute', subprocess.TimeoutExpired('fake', 9)),
                    ('execute', InterruptedError('signal'))]
        for stage, error in failures:
            with self.subTest(stage=stage, error=error):
                with self.isolation() as (args, _, session, _, remove, _, _):
                    getattr(session, stage).side_effect = error
                    self.assertEqual(runner.isolated_run(args), 1)
                    session.close.assert_called_once()
                    remove.assert_not_called()

    def test_partial_environment_failure_retains_directory_without_session(self):
        with self.isolation() as (args, _, session, _, remove, _, _):
            with mock.patch.object(runner, 'private_environment', side_effect=OSError('mkdir')):
                self.assertEqual(runner.isolated_run(args), 1)
            session.close.assert_not_called()
            remove.assert_not_called()

    def test_cleanup_error_reports_failure_checks_mounts_and_retains_directory(self):
        with self.isolation() as (args, root, session, mounted, remove, _, _):
            session.close.side_effect = subprocess.TimeoutExpired('fake', 2)
            self.assertEqual(runner.isolated_run(args), 1)
            mounted.assert_called_once_with(root)
            remove.assert_not_called()

    def test_service_exit_during_successful_probe_is_failure(self):
        with self.isolation() as (args, _, session, _, remove, _, _):
            session.processes = [('audio', mock.Mock(poll=mock.Mock(return_value=4)))]
            self.assertEqual(runner.isolated_run(args), 1)
            remove.assert_not_called()
            session.close.assert_called_once()

    def test_interrupt_becomes_cleanup_compatible_exception(self):
        for sig in (signal.SIGTERM, signal.SIGINT):
            with self.assertRaises(InterruptedError):
                runner.interrupt(sig, None)


class MainTests(SafeCase):
    @contextmanager
    def cli(self, argv, uid=1000, platform='linux', display=':42'):
        with mock.patch.object(runner.sys, 'argv', [str(RUNNER), *argv]), \
                mock.patch.object(runner.sys, 'platform', platform), \
                mock.patch.object(runner.os, 'geteuid', return_value=uid), \
                mock.patch.dict(runner.os.environ, {'DISPLAY': display}, clear=True), \
                mock.patch.object(runner.subprocess, 'call') as call, \
                mock.patch.object(runner, 'isolated_run') as isolated, \
                mock.patch.object(runner, 'require_resource_limits') as limits, \
                mock.patch('sys.stderr', new_callable=io.StringIO):
            yield call, isolated, limits

    def test_outer_main_requires_bounded_user_resource_scope(self):
        argv = ['--keep', '--timeout', '17', '--', 'fake', '--literal']
        with self.cli(argv) as (call, isolated, limits):
            call.return_value = 7
            self.assertEqual(runner.main(), 7)
            isolated.assert_not_called()
        command = call.call_args.args[0]
        self.assertEqual(command[0], 'systemd-run')
        for required in ('--user', '--scope', 'MemoryMax=4G', 'CPUQuota=200%', 'TasksMax=1024'):
            self.assertIn(required, command)
        self.assertIn('--inside-scope', command)
        self.assertIn(str(RUNNER.resolve()), command)
        self.assertEqual(command[-len(argv):], argv)

    def test_inside_scope_strips_separator_and_preserves_command_arguments(self):
        with self.cli(['--inside-scope', '--timeout', '17', '--keep', '--', 'fake', '--arg']) as (call, isolated, limits):
            isolated.return_value = 3
            self.assertEqual(runner.main(), 3)
            limits.assert_called_once_with()
            call.assert_not_called()
            args = isolated.call_args.args[0]
            self.assertEqual((args.command, args.timeout, args.keep), (['fake', '--arg'], 17, True))

    def test_inside_scope_rejects_failed_resource_verification(self):
        for error in (OSError('missing'), RuntimeError('unbounded'), ValueError('bad'), StopIteration()):
            with self.subTest(error=error), self.cli(['--inside-scope', '--', 'fake']) as (call, isolated, limits):
                limits.side_effect = error
                with self.assertRaises(SystemExit) as failure:
                    runner.main()
                self.assertEqual(failure.exception.code, 2)
                call.assert_not_called()
                isolated.assert_not_called()

    def test_effective_cgroup_limits_accept_bounds_and_reject_unbounded_values(self):
        defaults = {'cgroup': '0::/unit.scope\n', 'memory.max': str(4 * 1024 ** 3),
                    'cpu.max': '200000 100000', 'pids.max': '1024'}
        cases = [({}, False), ({'memory.max': 'max'}, True),
                 ({'memory.max': str(4 * 1024 ** 3 + 1)}, True),
                 ({'cpu.max': 'max 100000'}, True), ({'cpu.max': '200001 100000'}, True),
                 ({'pids.max': 'max'}, True), ({'pids.max': '1025'}, True)]
        for changes, rejected in cases:
            with self.subTest(changes=changes):
                values = dict(defaults, **changes)
                with mock.patch.object(Path, 'read_text', autospec=True,
                                       side_effect=lambda path: values[path.name]):
                    if rejected:
                        with self.assertRaises(RuntimeError):
                            runner.require_resource_limits()
                    else:
                        runner.require_resource_limits()

    def test_invalid_arguments_root_non_linux_and_missing_display_never_launch(self):
        cases = [([], {}), (['--'], {}), (['--timeout', '0', '--', 'fake'], {}),
                 (['--timeout', '-1', '--', 'fake'], {}), (['--timeout', 'bad'], {}),
                 (['--unknown'], {}), (['--', 'fake'], {'uid': 0}),
                 (['--inside-scope', '--', 'fake'], {'uid': 0}),
                 (['--', 'fake'], {'platform': 'darwin'}), (['--', 'fake'], {'display': ''})]
        for argv, kwargs in cases:
            with self.subTest(argv=argv, kwargs=kwargs), self.cli(argv, **kwargs) as (call, isolated, limits):
                with self.assertRaises(SystemExit) as error:
                    runner.main()
                self.assertEqual(error.exception.code, 2)
                call.assert_not_called()
                isolated.assert_not_called()


if __name__ == '__main__':
    unittest.main()
