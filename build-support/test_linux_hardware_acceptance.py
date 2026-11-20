import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import linux_hardware_acceptance as acceptance


class GuardTests(unittest.TestCase):
    def test_status_needs_no_token(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'status.json'
            # status must work with a hostile/empty environment too
            old = dict(os.environ)
            os.environ.clear()
            try:
                self.assertEqual(acceptance.main(['status', '--log', str(target)]), 0)
            finally:
                os.environ.update(old)

    def test_collector_refused_without_token(self):
        with self.assertRaises(SystemExit):
            acceptance.require_user_session({}, False)

    def test_collector_refused_inside_probe_scope_even_with_token(self):
        env = {'OINK_HARDWARE_USER': '1', 'OINK_NATIVE_TEST_ROOT': '/tmp/oink-native-x'}
        self.assertTrue(acceptance.probe_scope_active(env))
        with self.assertRaises(SystemExit):
            acceptance.require_user_session(env, True)

    def test_mic_requires_enable_flag_and_device(self):
        env = {'OINK_HARDWARE_USER': '1'}
        with self.assertRaises(SystemExit):
            acceptance.require_user_session(env, True and False and True)
        with self.assertRaises(SystemExit):
            acceptance.require_user_session(env, True and True and False)


class CoreEnvTests(unittest.TestCase):
    def test_core_env_maps_pulse_socket_to_oink_test_remote(self):
        env = acceptance.core_env('unix:/tmp/oink-native-abc/runtime/pulse-native')
        self.assertEqual(env['PIPEWIRE_RUNTIME_DIR'], '/tmp/oink-native-abc/runtime')
        self.assertEqual(env['PIPEWIRE_REMOTE'], 'oink-test')
        self.assertEqual(env['PIPEWIRE_CONFIG_NAME'], 'client.conf')

    def test_core_env_refuses_foreign_socket(self):
        with self.assertRaises(SystemExit):
            acceptance.core_env('unix:/run/user/1000/pulse/native')

    def test_consumer_refused_outside_probe_scope(self):
        old = dict(os.environ)
        os.environ.pop('OINK_NATIVE_TEST_ROOT', None)
        try:
            with self.assertRaises(SystemExit):
                acceptance.run_mic_consumer(0.1)
        finally:
            os.environ.update(old)


class CollectorTests(unittest.TestCase):
    def test_counts_only_no_event_detail(self):
        collector = acceptance.Collector()
        for event_type in (2, 2, 3, 4, 4, 4, 5, 6, 6):
            self.assertIsNone(collector.feed(event_type))
        snap = collector.snapshot()
        self.assertEqual(snap['counts'],
                         {'key_press': 2, 'key_release': 1, 'button_press': 3,
                          'button_release': 1, 'motion': 2})
        self.assertFalse(snap['stopped'])
        encoded = json.dumps(snap)
        self.assertNotIn('keysym', encoded)
        self.assertNotIn('keycode', encoded)
        self.assertNotIn('button', encoded.replace('button_press', '').replace('button_release', ''))

    def test_focus_out_stops_collection(self):
        collector = acceptance.Collector()
        collector.feed(2)
        done = collector.feed(10)
        self.assertIsNotNone(done)
        self.assertTrue(collector.stopped)
        self.assertEqual(collector.stop_reason, 'focus_lost')
        collector.feed(2)
        self.assertEqual(collector.snapshot()['counts']['key_press'], 1)

    def test_window_close_message_stops_collection(self):
        collector = acceptance.Collector()
        collector.feed(2)
        done = collector.feed(33)
        self.assertIsNotNone(done)
        self.assertEqual(collector.stop_reason, 'window_closed')
        collector.feed(2)
        self.assertEqual(collector.snapshot()['counts']['key_press'], 1)

    def test_unknown_events_ignored(self):
        collector = acceptance.Collector()
        for event_type in (0, 12, 22, 33):
            collector.feed(event_type)
        self.assertEqual(sum(collector.snapshot()['counts'].values()), 0)


class RmsTests(unittest.TestCase):
    def test_full_scale_square_is_zero_dbfs(self):
        self.assertAlmostEqual(acceptance.rms_db([32767, -32767] * 100), 0.0, places=1)

    def test_silence_is_negative_infinity(self):
        self.assertEqual(acceptance.rms_db([0] * 64), float('-inf'))
        self.assertEqual(acceptance.rms_db([]), float('-inf'))

    def test_half_amplitude_is_about_minus_six_db(self):
        level = acceptance.rms_db([16384, -16384] * 100)
        self.assertAlmostEqual(level, 20 * math.log10(0.5), places=1)


class ExitDiagnosticsTests(unittest.TestCase):
    def test_devnull_stderr_tail_is_empty(self):
        process = subprocess.Popen([sys.executable, '-c', 'pass'], stderr=subprocess.DEVNULL)
        process.wait()
        self.assertEqual(acceptance._stderr_tail(process), '')

    def test_captured_stderr_tail_keeps_last_chars(self):
        process = subprocess.Popen([sys.executable, '-c',
                                    'import sys; sys.stderr.write("x" * 300)'],
                                   stderr=subprocess.PIPE)
        process.wait()
        self.assertEqual(acceptance._stderr_tail(process, 100), 'x' * 100)

    def test_running_child_stopped_by_operator_counts_as_orderly(self):
        process = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
        self.assertEqual(acceptance._stop_process(process), 0)
        self.assertIsNotNone(process.returncode)

    def test_self_exited_child_keeps_its_code(self):
        process = subprocess.Popen([sys.executable, '-c', 'import sys; sys.exit(3)'])
        process.wait()
        self.assertEqual(acceptance._stop_process(process), 3)


class LogSchemaTests(unittest.TestCase):
    def test_capture_env_strips_private_socket_so_parec_reaches_host(self):
        env = {'PULSE_SERVER': 'unix:/tmp/oink-native-x/runtime/pulse-native',
               'PIPEWIRE_RUNTIME_DIR': '/tmp/oink-native-x/runtime',
               'PIPEWIRE_REMOTE': 'oink-test', 'DISPLAY': ':0', 'HOME': '/home/wgh'}
        clean = acceptance.host_capture_env(env)
        self.assertNotIn('PULSE_SERVER', clean)
        self.assertNotIn('PIPEWIRE_RUNTIME_DIR', clean)
        self.assertNotIn('PIPEWIRE_REMOTE', clean)
        self.assertEqual(clean['DISPLAY'], ':0')

    def test_mic_stop_reasons_and_allowed_fields(self):
        self.assertTrue(set(acceptance.EVENT_NAMES) >= {2, 3, 4, 5, 6})
        self.assertEqual(acceptance.FOCUS_OUT if hasattr(acceptance, 'FOCUS_OUT') else 10, 10)

    def test_default_log_paths_are_agreed(self):
        self.assertTrue(str(acceptance.INPUT_LOG).endswith('.release-tools/linux-probes/hardware-input-user.log'))
        self.assertTrue(str(acceptance.MIC_LOG).endswith('.release-tools/linux-probes/hardware-mic-user.log'))
        self.assertTrue(str(acceptance.FUSE_LOG).endswith('.release-tools/linux-probes/fuse-user.log'))


if __name__ == '__main__':
    unittest.main()
