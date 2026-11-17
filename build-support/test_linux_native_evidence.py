"""Evidence and peer-session tests; native processes are always mocked."""
import argparse
from contextlib import contextmanager
import io
import json
from pathlib import Path
import stat
import sys
from unittest import mock
import unittest

import test_linux_native_test as support

if sys.platform == 'linux':
    runner = support.runner


class EvidenceTests(support.SafeCase):
    @contextmanager
    def fixture(self, peer=False, probe_exit=0):
        roots = [self.new_root(), self.new_root()]
        sessions = []
        environments = []

        def make_session(root, environment):
            index = len(sessions)
            session = mock.Mock(processes=[], root=root, env=environment)
            session.execute.return_value = probe_exit
            session.start_x11.side_effect = lambda _host: environment.update(DISPLAY=f':{80 + index}')
            sessions.append(session)
            environments.append(environment)
            return session

        output = io.StringIO()
        args = argparse.Namespace(command=['fixture'], timeout=10, keep=False, peer_x11=peer)
        with mock.patch.dict(runner.os.environ, dict(support.HOST, UNRELATED_SECRET='excluded'), clear=True), \
                mock.patch.object(runner.tempfile, 'mkdtemp', side_effect=map(str, roots)), \
                mock.patch.object(runner, 'Session', side_effect=make_session) as factory, \
                mock.patch.object(runner.resource, 'setrlimit'), \
                mock.patch.object(runner.signal, 'signal'), \
                mock.patch.object(runner, 'mounts_below', return_value=[]) as mounts, \
                mock.patch.object(runner.shutil, 'rmtree') as remove, \
                mock.patch('sys.stdout', output), mock.patch('sys.stderr', io.StringIO()):
            yield args, roots, sessions, environments, output, factory, mounts, remove

    def summary(self, output):
        lines = [line.split('=', 1)[1] for line in output.getvalue().splitlines()
                 if line.startswith('native_test_summary=')]
        self.assertEqual(len(lines), 1)
        return json.loads(lines[0])

    def test_only_private_forwarding_devices_are_disabled_and_verified(self):
        session = self.session()
        names = {'Xephyr virtual keyboard': '6', 'Xephyr virtual mouse': '7'}

        def run(command):
            if command[1:3] == ['list', '--id-only']:
                value = names[command[-1]]
            elif command[1:3] == ['list', '--name-only']:
                value = next(name for name, device in names.items() if device == command[-1])
            else:
                value = 'Device Enabled (123): 0\n'
            return mock.Mock(stdout=value)

        with mock.patch.object(session, 'run', side_effect=run) as calls:
            session.block_host_input()
        disabled = [call.args[0] for call in calls.call_args_list if call.args[0][1] == 'disable']
        self.assertEqual(disabled, [['xinput', 'disable', '6'], ['xinput', 'disable', '7']])

    def test_ambiguous_or_non_xephyr_device_never_gets_disabled(self):
        for outputs in (['6\n7'], ['bad'], ['5', 'Virtual core XTEST keyboard']):
            with self.subTest(outputs=outputs):
                session = self.session()
                with mock.patch.object(session, 'run', side_effect=[mock.Mock(stdout=s) for s in outputs]) as calls:
                    with self.assertRaises(RuntimeError):
                        session.block_host_input()
                    self.assertFalse(any(call.args[0][1] == 'disable' for call in calls.call_args_list))

    def test_forwarding_must_be_observed_disabled(self):
        session = self.session()
        values = ['6', 'Xephyr virtual keyboard', '', 'Device Enabled (123): 1\n']
        with mock.patch.object(session, 'run', side_effect=[mock.Mock(stdout=s) for s in values]):
            with self.assertRaises(RuntimeError):
                session.block_host_input()

    def test_summary_preserves_probe_exit_and_cleanup_status(self):
        for code in (0, 7, -11):
            with self.subTest(code=code), self.fixture(probe_exit=code) as data:
                args, roots, sessions, _, output, _, _, remove = data
                self.assertEqual(runner.isolated_run(args), code)
                self.assertEqual(self.summary(output), {
                    'probe_exit': code, 'services_ok': True, 'cleanup_ok': True,
                    'mount_check_ok': True, 'remaining_mounts': 0, 'sessions': 1, 'result': code,
                })
                sessions[0].close.assert_called_once()
                if code:
                    remove.assert_not_called()
                else:
                    remove.assert_called_once_with(roots[0])

    def test_peer_has_distinct_private_environment_and_private_manifest(self):
        with self.fixture(peer=True) as data:
            args, roots, sessions, envs, output, _, mounts, remove = data
            self.assertEqual(runner.isolated_run(args), 0)
            manifest = Path(envs[0]['OINK_NATIVE_TEST_PEER_ENV'])
            self.assertEqual(manifest.parent, roots[0])
            self.assertEqual(stat.S_IMODE(manifest.stat().st_mode), 0o600)
            self.assertEqual(json.loads(manifest.read_text()), envs[1])
            self.assertNotIn('OINK_NATIVE_TEST_PEER_ENV', envs[1])
            for key in ('DISPLAY', 'XAUTHORITY', 'HOME', 'OINK_HOME', 'XDG_RUNTIME_DIR',
                        'DBUS_SESSION_BUS_ADDRESS', 'PULSE_SERVER', 'PIPEWIRE_RUNTIME_DIR'):
                self.assertNotEqual(envs[0][key], envs[1][key], key)
            for env in envs:
                self.assertNotIn('UNRELATED_SECRET', env)
                self.assertNotEqual(env['DISPLAY'], support.HOST['DISPLAY'])
            sessions[0].execute.assert_called_once_with(args.command, args.timeout)
            sessions[1].execute.assert_not_called()
            for session in sessions:
                session.close.assert_called_once()
            self.assertEqual(mounts.call_count, 2)
            self.assertEqual(remove.call_count, 2)
            self.assertEqual(self.summary(output)['sessions'], 2)

    def test_peer_start_failure_closes_both_and_retains_both_roots(self):
        with self.fixture(peer=True) as data:
            args, _, sessions, _, output, factory, _, remove = data
            original = factory.side_effect

            def make(root, env):
                session = original(root, env)
                if len(sessions) == 2:
                    session.start_audio.side_effect = OSError('peer audio startup failed')
                return session

            factory.side_effect = make
            self.assertEqual(runner.isolated_run(args), 1)
            for session in sessions:
                session.close.assert_called_once()
                session.execute.assert_not_called()
            remove.assert_not_called()
            evidence = self.summary(output)
            self.assertIsNone(evidence['probe_exit'])
            self.assertFalse(evidence['services_ok'])

    def test_peer_cannot_reuse_primary_display(self):
        with self.fixture(peer=True) as data:
            args, _, sessions, _, output, factory, _, remove = data
            original = factory.side_effect

            def make(root, env):
                session = original(root, env)
                session.start_x11.side_effect = lambda _host: env.update(DISPLAY=':80')
                return session

            factory.side_effect = make
            self.assertEqual(runner.isolated_run(args), 1)
            sessions[0].execute.assert_not_called()
            sessions[1].start_dbus.assert_not_called()
            remove.assert_not_called()
            self.assertIsNone(self.summary(output)['probe_exit'])

    def test_peer_service_exit_cannot_pass_with_successful_probe(self):
        with self.fixture(peer=True) as data:
            args, _, sessions, _, output, factory, _, remove = data
            original = factory.side_effect

            def make(root, env):
                session = original(root, env)
                if len(sessions) == 2:
                    session.processes = [('pipewire', mock.Mock(poll=mock.Mock(return_value=1)))]
                return session

            factory.side_effect = make
            self.assertEqual(runner.isolated_run(args), 1)
            evidence = self.summary(output)
            self.assertEqual(evidence['probe_exit'], 0)
            self.assertFalse(evidence['services_ok'])
            remove.assert_not_called()

    def test_mount_check_failure_is_not_zero_mount_success(self):
        with self.fixture() as data:
            args, _, _, _, output, _, mounts, remove = data
            mounts.side_effect = OSError('mountinfo unavailable')
            self.assertEqual(runner.isolated_run(args), 1)
            self.assertFalse(self.summary(output)['mount_check_ok'])
            remove.assert_not_called()

    def test_peer_mount_retains_both_roots(self):
        with self.fixture(peer=True) as data:
            args, roots, _, _, output, _, mounts, remove = data
            mounts.side_effect = [[], [roots[1] / 'mounts' / 'fixture']]
            self.assertEqual(runner.isolated_run(args), 1)
            evidence = self.summary(output)
            self.assertEqual(evidence['remaining_mounts'], 1)
            self.assertFalse(evidence['cleanup_ok'])
            remove.assert_not_called()

    def test_artifact_cleanup_error_still_emits_failure(self):
        with self.fixture() as data:
            args, _, _, _, output, _, _, remove = data
            remove.side_effect = OSError('busy')
            self.assertEqual(runner.isolated_run(args), 1)
            self.assertFalse(self.summary(output)['cleanup_ok'])

    def test_session_cleanup_error_is_not_overwritten_by_zero_probe_exit(self):
        with self.fixture() as data:
            args, _, _, _, output, factory, _, remove = data
            original = factory.side_effect

            def make(root, env):
                session = original(root, env)
                session.close.side_effect = OSError('cleanup failed')
                return session

            factory.side_effect = make
            self.assertEqual(runner.isolated_run(args), 1)
            evidence = self.summary(output)
            self.assertEqual(evidence['probe_exit'], 0)
            self.assertFalse(evidence['cleanup_ok'])
            remove.assert_not_called()


if __name__ == '__main__':
    unittest.main()
