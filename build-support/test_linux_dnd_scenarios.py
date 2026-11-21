"""Pure unit tests: fake pipes/processes only; never initialize GTK, X11 or XTEST."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import linux_dnd_protocol as protocol
import linux_dnd_scenarios as scenarios


def environment(root, display=':123'):
    return {
        'OINK_NATIVE_TEST_ROOT': str(root), 'DISPLAY': display,
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


def report_wire(sequence=0, status=0, **overrides):
    report = dict.fromkeys(protocol.FIELDS, 0)
    report.update(gen=1, xid=123, x=100, y=200)
    report.update(overrides)
    return ('R ' + ' '.join(map(str, (sequence, status, *(report[k] for k in protocol.FIELDS)))) + '\n').encode()


class EnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.roots = []
        self.primary = self.make_environment(':123')

    def tearDown(self):
        for root in self.roots:
            root.cleanup()

    def make_environment(self, display):
        root = tempfile.TemporaryDirectory(prefix='oink-native-', dir='/tmp')
        self.roots.append(root)
        fd = os.open(Path(root.name) / 'Xauthority', os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(fd)
        return environment(root.name, display)

    def metadata(self, peer, mode=0o600):
        path = Path(self.primary['OINK_NATIVE_TEST_ROOT']) / 'peer.json'
        fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, mode)
        with os.fdopen(fd, 'w') as stream:
            json.dump(peer, stream)
        return str(path)

    def test_whitelist_never_inherits_login_environment(self):
        supplied = dict(self.primary, LD_PRELOAD='/host/inject.so', SSH_AUTH_SOCK='/host/socket',
                        WAYLAND_DISPLAY='wayland-0', BROWSER='login-browser', PATH='/host/bin')
        clean = protocol.isolated_environment(supplied)
        self.assertFalse({'LD_PRELOAD', 'SSH_AUTH_SOCK', 'WAYLAND_DISPLAY', 'BROWSER'} & clean.keys())
        self.assertEqual(clean['PATH'], '/usr/bin:/bin')
        self.assertEqual(clean['GDK_BACKEND'], 'x11')

    def test_host_remote_and_screen_alias_displays_refused(self):
        for display in (':0', ':0.0', 'localhost:123', ':invalid', ':123.1', ':01'):
            with self.subTest(display=display), self.assertRaises(protocol.Failure):
                protocol.isolated_environment(dict(self.primary, DISPLAY=display))

    def test_nonprivate_service_and_root_refused(self):
        for key, value in [('PULSE_SERVER', 'unix:/run/user/1000/pulse/native'),
                           ('DBUS_SESSION_BUS_ADDRESS', 'unix:path=/host/bus'),
                           ('HOME', '/home/login'), ('XAUTHORITY', '/home/login/.Xauthority'),
                           ('OINK_NATIVE_TEST_ROOT', '/tmp/oink-native-foo/../host')]:
            with self.subTest(key=key), self.assertRaises(protocol.Failure):
                protocol.isolated_environment(dict(self.primary, **{key: value}))

    def test_peer_is_complete_and_independent(self):
        peer = self.make_environment(':124')
        clean = protocol.peer_environment(self.primary, self.metadata(peer))
        self.assertEqual(clean['DISPLAY'], ':124')
        self.assertNotEqual(clean['HOME'], self.primary['HOME'])

    def test_peer_same_display_alias_refused(self):
        peer = self.make_environment(':123.0')
        with self.assertRaises(protocol.Failure):
            protocol.peer_environment(self.primary, self.metadata(peer))

    def test_peer_same_root_refused(self):
        with self.assertRaises(protocol.Failure):
            protocol.peer_environment(self.primary, self.metadata(dict(self.primary, DISPLAY=':124')))

    def test_peer_requires_exact_0600(self):
        peer = self.make_environment(':124')
        with self.assertRaises(protocol.Failure):
            protocol.peer_environment(self.primary, self.metadata(peer, 0o400))

    def test_peer_symlink_refused(self):
        path = Path(self.primary['OINK_NATIVE_TEST_ROOT']) / 'link.json'
        path.symlink_to(self.metadata(self.make_environment(':124')))
        with self.assertRaises(OSError):
            protocol.peer_environment(self.primary, str(path))

    def test_no_peer_is_blocked_not_pass(self):
        with self.assertRaises(protocol.Blocked):
            protocol.peer_environment(self.primary, None)


class FramingTests(unittest.TestCase):
    def test_split_and_coalesced_frames(self):
        parser = protocol.Framer()
        data = report_wire()
        self.assertEqual(parser.feed(data[:15]), [])
        self.assertEqual(parser.feed(data[15:] + data), [data[:-1], data[:-1]])

    def test_line_limits_and_non_ascii(self):
        for data in (b'1' * protocol.MAX_LINE, b'\x00', b'\xff', b'\n'):
            with self.subTest(data=data[:10]), self.assertRaises(protocol.Failure):
                protocol.Framer().feed(data)

    def test_malformed_reports(self):
        for frame in (b'R 0 0', b'R -1 0', b'R 4294967296 0', b'X 0 0', b'R  0',
                      report_wire(status=3)[:-1]):
            with self.subTest(frame=frame), self.assertRaises(protocol.Failure):
                protocol.decode(frame)

    def test_bounded_numeric_bridge_body(self):
        self.assertEqual(protocol.decode(b'D 5 1 3 1 2 255'), ('D', 5, (1, b'\x01\x02\xff')))
        for frame in (b'D 5 1 0', b'D 5 1 3 1 2', b'D 5 1 1 256', b'D 5 1 129'):
            with self.subTest(frame=frame), self.assertRaises(protocol.Failure):
                protocol.decode(frame)

    def test_c_and_python_report_and_opcode_order_match(self):
        header = Path(__file__).with_name('linux_dnd_wire.h').read_text()
        field_block = header.split('#define FIELDS(X)', 1)[1].split('#define DECLARE', 1)[0]
        self.assertEqual(re.findall(r'X\((\w+)\)', field_block), protocol.FIELDS)
        commands = header.split('enum Command {', 1)[1].split('};', 1)[0]
        names = re.findall(r'\b[A-Z_]+\b', commands)
        self.assertEqual([getattr(protocol, name) for name in names], list(range(len(names))))

    def test_new_source_files_stay_within_500_lines(self):
        names = ('linux_dnd_roles.c', 'linux_dnd_roles.h', 'linux_dnd_native.c',
                 'linux_dnd_selection.c', 'linux_dnd_incr.c', 'linux_dnd_incr.h',
                 'linux_dnd_stream.c', 'linux_dnd_stream.h', 'linux_dnd_protocol.py',
                 'linux_dnd_scenarios.py', 'linux_dnd_incr_scenarios.py',
                 'linux_dnd_file.c', 'linux_dnd_file.h', 'linux_dnd_file_scenarios.py',
                 'linux_dnd_app_file_scenarios.py', 'linux_dnd_recovery.py',
                 'test_linux_dnd_scenarios.py', 'test_linux_dnd_recovery.py',
                 'linux_input_toolkit_route.c', 'linux_input_toolkit_qt.cpp',
                 'linux_input_toolkit_qt_row.py', 'linux_input_toolkit_scenarios.py',
                 'linux_input_toolkit_qt_build.py', 'test_linux_input_toolkit.py',
                 'linux_hardware_acceptance.py', 'linux_hardware_mic.py',
                 'test_linux_hardware_acceptance.py',
                 'linux_app_harness.py', 'linux_app_page.py', 'linux_app_x11.py',
                 'linux_dnd_app_scenarios.py', 'linux_dnd_app_page.html',
                 'linux_deps_archive.py', 'linux_deps_fetch.py',
                 'linux_input_app_page.html', 'test_linux_app_wave.py',
                 'test_linux_deps_archive.py', 'test_linux_deps_fetch.py',
                 'test_linux_input_probe.py',
                 'linux_phase3_shell_probe.py', 'linux_phase3_w2_probe.py',
                 'linux_phase3_w3_probe.py', 'linux_phase3_tray_probe.py',
                 'linux_phase4_input_probe.c', 'linux_phase4_overlay_probe.c',
                 'linux_phase4_wallpaper_probe.py', 'linux_phase4_load.py',
                 'linux_sni_watcher.py', 'fake_logind_service.py',
                 'linux_phase5_selection_peer.c',
                 'linux_phase5_clipboard_scenarios.py',
                 'crates/oink-clipboard/examples/phase5_clipboard_probe.rs')
        root = Path(__file__).resolve().parent.parent
        for name in names:
            path = root / name if '/' in name else Path(__file__).with_name(name)
            lines = path.read_text().splitlines()
            with self.subTest(name=name):
                self.assertLessEqual(len(lines), 500)
                self.assertTrue(all(line == line.rstrip() for line in lines))

    def test_sha256_difference_cannot_pass_on_length_and_short_hash_alone(self):
        payload = dict(length=3, hash=123, **{f'digest{i}': i for i in range(8)})
        with self.assertRaises(protocol.Failure):
            scenarios.same_payload(dict(payload, digest7=99), payload)
        scenarios.same_payload(payload, payload)

    def test_exact_checks_do_not_default_missing_or_wrong_counts_to_pass(self):
        with self.assertRaises(protocol.Failure):
            scenarios.exact({'drop': 0}, drop=1)
        with self.assertRaises(KeyError):
            scenarios.exact({}, received=1)


class ControlTimingTests(unittest.TestCase):
    def test_actual_elapsed_is_recorded_with_test_and_product_budgets(self):
        role = mock.Mock()
        role.name = 'source'
        role.command.return_value = dict(slow_sent=0, end=0)
        with mock.patch.object(scenarios.time, 'monotonic_ns', side_effect=[1_000_000_000, 1_012_345_000]), \
                mock.patch('builtins.print') as output:
            scenarios.responsive_status(role, slow_sent=0, end=0)
        role.command.assert_called_once_with(protocol.STATUS, timeout=.5)
        record = json.loads(output.call_args.args[0])
        self.assertEqual(record['elapsed_ns'], 12_345_000)
        self.assertEqual(record['control_ack_budget_ns'], 500_000_000)
        self.assertEqual(record['preparation_budget_ns'], 3_000_000_000)
        self.assertEqual(record['ordinary_input_expiry_ns'], 5_000_000_000)
        self.assertEqual(record['scope'], 'test_role_control_not_product_fence')

    def test_over_budget_return_is_rejected_even_if_role_claims_success(self):
        role = mock.Mock()
        for elapsed in (500_000_000, 500_000_001, 3_000_000_000, 5_000_000_000, -1):
            with self.subTest(elapsed=elapsed), \
                    mock.patch.object(scenarios.time, 'monotonic_ns', side_effect=[1, 1 + elapsed]), \
                    mock.patch('builtins.print') as output, self.assertRaises(protocol.Failure):
                scenarios.responsive_status(role)
            output.assert_not_called()

    def test_slow_reply_already_finished_cannot_pass_responsiveness_check(self):
        role = mock.Mock()
        role.command.return_value = dict(slow_sent=1, end=1)
        with mock.patch.object(scenarios.time, 'monotonic_ns', side_effect=[1, 2]), \
                self.assertRaises(protocol.Failure):
            scenarios.responsive_status(role, slow_sent=0, end=0)

    def test_comparison_budgets_match_current_product_contracts(self):
        root = Path(__file__).resolve().parents[1]
        target = (root / 'crates/oink-routing/src/router/dnd/target.rs').read_text()
        inject = (root / 'crates/oink-agent/src/threads/inject.rs').read_text()
        self.assertIn('preparation_deadline: std::time::Instant::now() + std::time::Duration::from_secs(3)', target)
        self.assertIn('self.at.elapsed() > std::time::Duration::from_secs(5)', inject)
        self.assertLess(scenarios.CONTROL_ACK_NS, scenarios.PREPARATION_NS)
        self.assertLess(scenarios.PREPARATION_NS, scenarios.ORDINARY_INPUT_EXPIRY_NS)


class TransportTests(unittest.TestCase):
    def fake_role(self, script, name='source'):
        real_popen = subprocess.Popen
        captured = []

        def spawn(command, **kwargs):
            captured.append(kwargs['env'])
            return real_popen([sys.executable, '-u', '-c', script], **kwargs)

        with mock.patch.object(protocol, 'isolated_environment', return_value={'PATH': '/usr/bin:/bin'}), \
                mock.patch.object(protocol.subprocess, 'Popen', side_effect=spawn):
            role = protocol.Role('/not-a-native-binary', name, {})
        self.addCleanup(role.close)
        self.assertEqual([{k: v for k, v in env.items() if k != 'OINK_STREAM_FD'}
                          for env in captured], [{'PATH': '/usr/bin:/bin'}])
        return role

    def test_partial_line_cannot_block_past_deadline(self):
        role = self.fake_role("import os,time; os.write(1,b'R 0 0'); time.sleep(2)")
        start = time.monotonic()
        with self.assertRaises(protocol.Failure):
            role.receive(0, start + .15)
        self.assertLess(time.monotonic() - start, .8)

    def test_eof_is_failure(self):
        role = self.fake_role('pass')
        with self.assertRaises(protocol.Failure):
            role.ready()

    def test_nonresponsive_command_has_bounded_read_timeout(self):
        script = f'import os,time; os.write(1,{report_wire()!r}); os.read(0,2048); time.sleep(2)'
        role = self.fake_role(script)
        role.ready()
        with self.assertRaises(protocol.Failure):
            role.command(protocol.STATUS, timeout=.15)

    def test_full_stdin_pipe_cannot_block_command_write(self):
        role = self.fake_role('import time; time.sleep(2)')
        for _ in range(1024):
            try:
                os.write(role.process.stdin.fileno(), b'0' * 2048)
            except BlockingIOError:
                break
        else:
            self.fail('fake process pipe did not apply backpressure')
        started = time.monotonic()
        with self.assertRaises(protocol.Failure):
            role.command(protocol.STATUS, timeout=.15)
        self.assertLess(time.monotonic() - started, .8)

    def test_unexpected_native_exit_cannot_be_reported_pass(self):
        role = mock.Mock(expected_exit=None)
        role.process.poll.return_value = 1
        with self.assertRaises(protocol.Failure):
            scenarios.evidence('unexpected-exit', source=role)

    def test_wrong_sequence_is_failure(self):
        role = self.fake_role(f'import os; os.write(1,{report_wire(sequence=9)!r})')
        with self.assertRaises(protocol.Failure):
            role.ready()

    def test_ready_then_real_pipe_command_ack(self):
        script = (f'import os; os.write(1,{report_wire()!r}); request=os.read(0,2048); '
                  f'os.write(1,{report_wire(sequence=1)!r}); os.read(0,2048)')
        role = self.fake_role(script)
        self.assertEqual(role.ready()['xid'], 123)
        self.assertEqual(role.command(protocol.STATUS)['gen'], 1)

    def test_unsolicited_body_never_reaches_target(self):
        script = "import os; os.write(1,b'D 0 1 1 65\\n')"
        role = self.fake_role(script, 'target')
        with self.assertRaises(protocol.Failure):
            role.ready()

    def test_stale_body_rejected(self):
        role = self.fake_role("import time; time.sleep(2)", 'catcher')
        role.frames = [('D', 1, (2, b'A'))]
        with self.assertRaises(protocol.Failure):
            role.receive(1, time.monotonic() + .1, allow_data=True)

    def test_ack_with_no_fetch_body_is_failure(self):
        script = (f'import os; os.write(1,{report_wire()!r}); os.read(0,2048); '
                  f'os.write(1,{report_wire(sequence=1)!r}); os.read(0,2048)')
        role = self.fake_role(script, 'catcher')
        role.ready()
        with self.assertRaises(protocol.Failure):
            role.command(protocol.FETCH)

    def test_bridge_fetch_requires_matching_generation_and_bounded_bytes(self):
        data = b'D 1 1 3 65 66 67\n' + report_wire(sequence=1)
        script = (f'import os; os.write(1,{report_wire()!r}); os.read(0,2048); '
                  f'os.write(1,{data!r}); os.read(0,2048)')
        role = self.fake_role(script, 'catcher')
        role.ready()
        self.assertEqual(role.command(protocol.FETCH), b'ABC')

    def test_delayed_generation_ack_cannot_silently_replace_live_generation(self):
        role = self.fake_role("import time; time.sleep(2)")
        role.frames = [('R', 1, (0, dict(zip(protocol.FIELDS, [2] + [0] * (len(protocol.FIELDS) - 1)))))]
        with self.assertRaises(protocol.Failure):
            role.command(protocol.STATUS)


if __name__ == '__main__':
    unittest.main()
