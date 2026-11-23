#!/usr/bin/env python3
"""Offline unit tests only. Native command/libpulse access is explicitly blocked.

Run with python3 -B; these tests are NOT evidence of native audio correctness.
"""
import array
from contextlib import ExitStack, redirect_stdout
import io
import math
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock, patch

import linux_audio_probe as base
import linux_audio_route_policy as policy
import linux_audio_route_probe as probe
import linux_audio_route_runtime as runtime


class OfflineTest(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        for module in (base, runtime, probe):
            for name in ('command', 'pulse', 'graph', 'route', 'spawn', 'stop', 'isolation_guard'):
                if hasattr(module, name):
                    self.stack.enter_context(patch.object(
                        module, name, side_effect=AssertionError('native operation forbidden')))
        self.stack.enter_context(patch.object(base.C, 'CDLL',
                                              side_effect=AssertionError('libpulse forbidden')))

    def replace(self, target, name, **kwargs):
        return self.stack.enter_context(patch.object(target, name, **kwargs))


class SignalTests(OfflineTest):
    def meter(self, data):
        meter = policy.Spectrum()
        # Exercise splits inside stereo frames and individual s16 samples.
        for start in range(0, len(data), 733):
            meter.feed(data[start:start + 733])
        return meter

    def test_one_khz_rms_and_both_channels(self):
        meter = self.meter(policy.tone(frames=9600))
        self.assertTrue(meter.matches(probe.ONE, 0), meter.values())
        self.assertAlmostEqual(meter.values()['rms'], probe.ONE, delta=0.1)
        self.assertEqual(meter.samples, 19200)
        self.assertEqual(len(meter.block), 0)

    def test_two_khz_is_not_sending_signal(self):
        meter = self.meter(policy.tone(2000, 4096, 9600))
        self.assertTrue(meter.matches(0, probe.TWO))
        self.assertFalse(meter.matches(probe.ONE, 0))

    def test_mix_exposes_receive_leak(self):
        left = array.array('h', policy.tone(frames=9600))
        right = array.array('h', policy.tone(2000, 4096, 9600))
        if sys.byteorder != 'little':
            left.byteswap()
            right.byteswap()
        mix = array.array('h', (a + b for a, b in zip(left, right)))
        if sys.byteorder != 'little':
            mix.byteswap()
        meter = self.meter(mix.tobytes())
        self.assertTrue(meter.matches(probe.ONE, probe.TWO))
        self.assertFalse(meter.matches(probe.ONE, 0))

    def test_silence_requires_actual_samples(self):
        self.assertFalse(policy.Spectrum().matches())
        self.assertTrue(self.meter(bytes(38400)).matches())
        self.assertFalse(self.meter(bytes(38400)).matches(probe.ONE, 0))
        self.assertFalse(self.meter(bytes(8)).matches())

    def test_equal_rms_wrong_frequency_rejected(self):
        meter = self.meter(policy.tone(3000, 8192, 9600))
        self.assertAlmostEqual(meter.values()['rms'], probe.ONE, delta=0.3)
        self.assertFalse(meter.matches(probe.ONE, 0))

    def test_phase_and_anti_phase_stereo_do_not_cancel(self):
        data = array.array('h', (round(8192 * math.sin(2 * math.pi * i / 48 + 0.71)) * sign
                                for i in range(9600) for sign in (1, -1)))
        if sys.byteorder != 'little':
            data.byteswap()
        self.assertTrue(self.meter(data.tobytes()).matches(probe.ONE, 0))

    def test_clipped_or_feedback_amplified_signal_rejected(self):
        self.assertFalse(self.meter(policy.tone(1000, 16384, 9600)).matches(probe.ONE, 0))
        meter = self.meter(policy.tone(frames=9600))
        meter.holes = 1
        self.assertFalse(meter.matches(probe.ONE, 0))

    def test_meter_buffers_bounded_to_partial_window(self):
        meter = self.meter(policy.tone(frames=9611) + b'\0')
        self.assertLess(len(meter.block), policy.Spectrum.size * 4)
        self.assertEqual(len(meter.pending), 1)


class RestoreTests(OfflineTest):
    def fixture(self, muted=False):
        self.key = policy.Identity('server-a', 7, '101')
        self.current = policy.Value(self.key, muted)
        self.writes = []

        def write(key, value):
            self.writes.append((key, value))
            self.current = policy.Value(key, value)

        return policy.RestoreLease(lambda: self.current, write, True)

    def test_apply_restore_once(self):
        lease = self.fixture().apply()
        self.assertTrue(self.current.value)
        self.assertEqual(lease.close(), 'restored')
        self.assertFalse(self.current.value)
        self.assertEqual(lease.close(), 'closed')
        self.assertEqual(len(self.writes), 2)

    def test_preexisting_mute_restored_to_true(self):
        lease = self.fixture(True).apply()
        self.assertEqual(lease.close(), 'restored')
        self.assertTrue(self.current.value)

    def test_later_value_wins(self):
        lease = self.fixture().apply()
        self.current = policy.Value(self.key, False)
        self.assertEqual(lease.close(), 'value_changed')
        self.assertEqual(len(self.writes), 1)

    def test_index_reuse_serial_change_and_server_change(self):
        for key in (policy.Identity('server-a', 7, '102'),
                    policy.Identity('server-b', 7, '101'),
                    policy.Identity('server-a', 8, '101')):
            with self.subTest(key=key):
                lease = self.fixture().apply()
                self.current = policy.Value(key, True)
                self.assertEqual(lease.close(), 'identity_changed')
                self.assertEqual(len(self.writes), 1)

    def test_disappeared_object_not_recreated(self):
        lease = self.fixture().apply()
        self.current = None
        self.assertEqual(lease.close(), 'identity_changed')
        self.assertEqual(len(self.writes), 1)

    def test_acquire_conflict_does_not_write(self):
        lease = self.fixture()
        self.current = policy.Value(self.key, True)
        with self.assertRaisesRegex(RuntimeError, 'acquire_conflict'):
            lease.apply()
        self.assertEqual(self.writes, [])

    def test_restore_failure_remains_armed(self):
        lease = self.fixture().apply()
        lease.write = Mock(side_effect=RuntimeError('write failed'))
        with self.assertRaises(RuntimeError):
            lease.close()
        self.assertTrue(lease.armed)

    def test_failed_apply_still_can_restore(self):
        lease = self.fixture()
        write = lease.write

        def fail_after_write(key, value):
            write(key, value)
            raise RuntimeError('ack failed')

        lease.write = fail_after_write
        with self.assertRaises(RuntimeError):
            lease.apply()
        lease.write = write
        self.assertEqual(lease.close(), 'restored')


class PlumbingTests(OfflineTest):
    def test_graph_reachability_including_cycle_and_feed_edge(self):
        edges = [('capture', 'source'), ('source', 'feed'), ('feed', 'play'), ('play', 'speaker')]
        self.assertFalse(policy.reachable(edges, 'speaker', 'capture'))
        self.assertTrue(policy.reachable(edges, 'capture', 'speaker'))
        self.assertTrue(policy.reachable(edges + [('speaker', 'capture')], 'speaker', 'capture'))
        self.assertFalse(policy.reachable(edges + [('speaker', 'source')], 'speaker', 'capture'))

    def test_cleanup_runs_all_callbacks_in_reverse_and_reports_errors(self):
        resources, calls = runtime.Resources(), []
        resources.add('first', lambda: calls.append('first'))

        def fail():
            calls.append('middle')
            raise RuntimeError('cleanup')

        resources.add('middle', fail)
        resources.add('last', lambda: calls.append('last'))
        with redirect_stdout(io.StringIO()):
            errors = resources.close()
        self.assertEqual(calls, ['last', 'middle', 'first'])
        self.assertEqual(errors, [('middle', 'RuntimeError')])
        self.assertEqual(resources.close(), [])

    def test_observer_requires_registry_identity_and_active_native_link(self):
        dump = [dict(id=1, type='PW:Node', info={'props': {
                    'node.name': runtime.SOURCE, 'object.serial': 100}}),
                dict(id=2, type='PW:Node', info={'state': 'running', 'props': {
                    'object.serial': 101, 'media.class': 'Stream/Input/Audio'}}),
                dict(id=3, type='PW:Link', info={'output-node-id': 1, 'input-node-id': 2,
                                               'state': 'active'})]
        row = dict(index=7, source=9, corked=False, properties={'object.serial': '101'})
        self.replace(runtime, 'graph', return_value=dump)
        self.replace(runtime, 'epoch', return_value='epoch')
        rows = self.replace(runtime, 'pulse', return_value=[row])
        key = policy.node_identity(dump[1], 'epoch')
        self.assertEqual(runtime.observe(9, set()), frozenset([key]))
        self.assertEqual(runtime.observe(9, {key}), frozenset())
        row['corked'] = True
        self.assertEqual(runtime.observe(9, set()), frozenset())
        row['corked'] = False
        dump[-1]['info']['state'] = 'paused'
        self.assertEqual(runtime.observe(9, set()), frozenset())
        rows.return_value = []  # Offline native-only schema: still requires an active link.
        self.assertEqual(runtime.observe(9, set()), frozenset())
        dump[-1]['info']['state'] = 'active'
        self.assertEqual(runtime.observe(9, set()), frozenset([key]))
        dump[1]['info']['props'].pop('media.class')
        self.assertIsNone(runtime.observe(9, set()))  # Unknown schema never grants.
        dump.clear()
        self.assertIsNone(runtime.observe(9, set()))

    def test_failed_module_load_still_registers_cleanup(self):
        before = b'1\tmodule-null-sink\tsink_name=oink_test\n'
        after = before + b'2\tmodule-null-sink\tsink_name=oink_route_speaker\n'
        command = self.replace(runtime, 'command', side_effect=[
            before, RuntimeError('load ACK lost'), after, b''])
        resources = runtime.Resources()
        rig = runtime.Rig(resources)
        with self.assertRaisesRegex(RuntimeError, 'ACK lost'):
            rig.load('module-null-sink', 'sink_name=oink_route_speaker')
        self.assertEqual(resources.close(), [])
        self.assertEqual(command.call_args.args, ('pactl', 'unload-module', '2'))
        runtime.pulse.assert_not_called()

    def test_known_module_cleanup_does_not_unload_preexisting_module(self):
        command = self.replace(runtime, 'command', side_effect=[
            b'1\tmodule-null-sink\tsink_name=oink_test\n', b'8\n', b''])
        resources = runtime.Resources()
        self.assertEqual(runtime.Rig(resources).load('module-null-sink'), '8')
        self.assertEqual(resources.close(), [])
        self.assertEqual(command.call_args.args, ('pactl', 'unload-module', '8'))
        runtime.pulse.assert_not_called()

    def test_module_json_without_index_is_not_used(self):
        self.replace(runtime, 'pulse', return_value=[{'name': 'module-null-sink'}])
        command = self.replace(runtime, 'command', side_effect=[
            b'536870912\tmodule-null-sink\tsink_name=oink_test\n', b'536870913\n', b''])
        resources = runtime.Resources()
        self.assertEqual(runtime.Rig(resources).load('module-null-sink'), '536870913')
        self.assertEqual(resources.close(), [])
        self.assertEqual(command.call_args_list[0].args, ('pactl', 'list', 'short', 'modules'))
        self.assertEqual(command.call_args.args, ('pactl', 'unload-module', '536870913'))
        runtime.pulse.assert_not_called()

    def test_invalid_module_short_listing_refuses_load(self):
        command = self.replace(runtime, 'command', return_value=b'unknown\tmodule-null-sink\n')
        with self.assertRaisesRegex(RuntimeError, 'module_listing'):
            runtime.Rig(runtime.Resources()).load('module-null-sink')
        command.assert_called_once_with('pactl', 'list', 'short', 'modules')

    def test_libpulse_constructor_failure_attempts_close(self):
        self.replace(runtime.PulseRecorder, '__init__', side_effect=RuntimeError('connect failed'))
        close = self.replace(runtime.SafePulseRecorder, 'close')
        with self.assertRaises(RuntimeError):
            runtime.SafePulseRecorder(runtime.SOURCE)
        close.assert_called_once()

    def test_pump_join_failure_is_cleanup_failure(self):
        pump = runtime.Pump(Mock(), payload=b'\0\0')
        pump.thread = Mock()
        pump.thread.is_alive.return_value = True
        with self.assertRaisesRegex(RuntimeError, 'join_failed'):
            pump.close()
        pump.thread.join.assert_called_once_with(timeout=2)
        self.assertTrue(pump.stopping.is_set())

    def test_guard_refusal_prevents_all_native_operations(self):
        guard = self.replace(probe, 'isolation_guard', side_effect=RuntimeError('isolation_refused'))
        with redirect_stdout(io.StringIO()) as output:
            result = probe.run()
        self.assertEqual(result, 1)
        guard.assert_called_once()
        probe.pulse.assert_not_called()
        probe.graph.assert_not_called()
        probe.command.assert_not_called()
        self.assertIn('exit=1', output.getvalue())

    def fake_run(self, *, stage_failure=False, cleanup_failure=False, defaults_changed=False):
        self.replace(probe, 'isolation_guard', return_value=None)
        self.replace(probe.signal, 'signal')
        self.replace(probe.shutil, 'which', return_value='/fake/tool')
        self.replace(probe, 'epoch', return_value='epoch')
        initial = ('oink_test', 'oink_test.monitor')
        self.replace(probe, 'defaults', side_effect=[initial, ('changed', 'changed'), initial]
                     if defaults_changed else None, return_value=initial)
        self.replace(probe, 'pulse', side_effect=lambda kind: {
            'sinks': [{'name': 'oink_test', 'mute': False}, {'name': 'oink_test_mic', 'mute': False}],
            'sources': [{'name': 'oink_test.monitor'},
                        {'name': 'oink_test_mic.monitor'},
                        {'name': 'oink_test_source'}],
        }.get(kind, []))
        self.replace(base, 'wait', side_effect=lambda predicate, **kwargs: predicate())

        def experiment(rig, report):
            if cleanup_failure:
                rig.resources.add('injected', Mock(side_effect=RuntimeError('cleanup')))
            fake = Mock()
            if stage_failure:
                fake.setup.side_effect = RuntimeError('stage failed')
            return fake

        self.replace(probe, 'Experiment', side_effect=experiment)
        with redirect_stdout(io.StringIO()) as output:
            result = probe.run()
        return result, output.getvalue()

    def test_summary_nonzero_on_stage_failure(self):
        result, output = self.fake_run(stage_failure=True)
        self.assertEqual(result, 1)
        self.assertIn('failures=1 cleanup_failures=0 exit=1', output)

    def test_summary_nonzero_on_cleanup_failure(self):
        result, output = self.fake_run(cleanup_failure=True)
        self.assertEqual(result, 1)
        self.assertIn('cleanup_failures=1 exit=1', output)

    def test_default_change_fails_post_cleanup_summary(self):
        result, output = self.fake_run(defaults_changed=True)
        self.assertEqual(result, 1)
        self.assertIn('default_devices_unchanged status=fail', output)
        self.assertIn('cleanup_failures=1 exit=1', output)

    def test_fake_success_summary_is_not_a_native_test(self):
        result, output = self.fake_run()
        self.assertEqual(result, 0)
        self.assertIn('failures=0 cleanup_failures=0 exit=0', output)

    def test_owned_source_files_remain_under_500_lines(self):
        directory = Path(__file__).resolve().parent
        paths = list(directory.glob('linux_audio_route_*.py')) + [
            Path(__file__), directory / 'test_linux_audio_native_consumer.py']
        for path in paths:
            self.assertLessEqual(len(path.read_text().splitlines()), 500, str(path))


if __name__ == '__main__':
    unittest.main()
