#!/usr/bin/env python3
"""Offline native-only fixtures/contracts, NOT evidence from a real pw-cat run."""
from contextlib import redirect_stdout
from copy import deepcopy
import io
import os
import subprocess
import unittest
from unittest.mock import Mock, patch

import linux_audio_probe as base
import linux_audio_route_policy as policy
import linux_audio_route_probe as probe
import linux_audio_route_runtime as runtime
from test_linux_audio_route_probe import OfflineTest


class DemandTests(OfflineTest):
    def row(self, index, *, corked=False, source=9, serial=None):
        return dict(index=index, source=source, corked=corked,
                    properties={'object.serial': str(serial or index + 100),
                                'media.name': runtime.FEED})

    def test_only_internal_feed_cannot_enable(self):
        row = self.row(1)
        key = policy.identity(row, 'epoch')
        result = policy.consumers([row], 9, 'epoch', {key}, {'101'})
        self.assertEqual(result, frozenset())

    def test_consumer_named_like_internal_is_not_excluded(self):
        rows = [self.row(1), self.row(2)]
        key = policy.identity(rows[0], 'epoch')
        result = policy.consumers(rows, 9, 'epoch', {key}, {'101', '102'})
        self.assertEqual(result, frozenset([policy.identity(rows[1], 'epoch')]))

    def test_cork_partial_exit_and_last_exit(self):
        rows = [self.row(1), self.row(2)]
        count = lambda current: len(policy.consumers(current, 9, 'epoch', set(), {'101', '102'}))
        self.assertEqual(count(rows), 2)
        rows[1]['corked'] = True
        self.assertEqual(count(rows), 1)
        self.assertEqual(count(rows[1:]), 0)
        self.assertEqual(count([]), 0)

    def test_unlinked_and_other_sources_not_demand(self):
        rows = [self.row(1), self.row(2, source=8)]
        self.assertEqual(policy.consumers(rows, 9, 'epoch', set(), {'102'}), frozenset())

    def test_unknown_schema_identity_or_native_link_fails_closed(self):
        for row in (self.row(1, corked='false'), dict(index=1, source=9, corked=False)):
            self.assertIsNone(policy.consumers([row], 9, 'epoch', set(), {'101'}))
        self.assertIsNone(policy.consumers([self.row(1)], 9, 'epoch', set(), {'101'}, True))

    def test_changed_server_and_unknown_source_fail_closed(self):
        row = self.row(1)
        key = policy.identity(row, 'old-server')
        self.assertIsNone(policy.consumers([row], 9, 'new-server', {key}, {'101'}))
        row.pop('source')
        self.assertIsNone(policy.consumers([row], 9, 'epoch', set(), {'101'}))

    def test_permit_truth_table(self):
        for enabled in (False, True):
            for active in (False, True):
                lease = policy.DemandLease()
                lease.publish({'consumer'} if active else set(), 10)
                start, stop = Mock(return_value=object()), Mock()
                gate = policy.CaptureGate(start, stop)
                gate.update(enabled, lease, 10)
                self.assertEqual(gate.stream is not None, enabled and active)
                gate.close()

    def test_unknown_revoked_expired_and_disabled_stop_callback(self):
        for loss in ('unknown', 'revoked', 'expired', 'disabled'):
            with self.subTest(loss=loss):
                lease = policy.DemandLease()
                lease.publish({'a'}, 10, ttl=2)
                stream, stop = object(), Mock()
                gate = policy.CaptureGate(Mock(return_value=stream), stop)
                gate.update(True, lease, 10)
                if loss == 'unknown':
                    lease.publish(None, 11)
                elif loss == 'revoked':
                    lease.revoke()
                gate.update(loss != 'disabled', lease, 12 if loss == 'expired' else 11)
                stop.assert_called_once_with(stream)
                self.assertIsNone(gate.stream)

    def test_partial_exit_retains_stream_then_last_exit_stops(self):
        lease = policy.DemandLease()
        start, stop = Mock(side_effect=[object(), object()]), Mock()
        gate = policy.CaptureGate(start, stop)
        lease.publish({'a', 'b'}, 1)
        original = gate.update(True, lease, 1)
        lease.publish({'b'}, 2)
        self.assertIs(gate.update(True, lease, 2), original)
        lease.publish(set(), 3)
        gate.update(True, lease, 3)
        lease.publish({'c'}, 4)
        self.assertIsNot(gate.update(True, lease, 4), original)
        self.assertEqual((gate.starts, gate.stops), (2, 1))

    def test_deadline_boundary_and_nonfinite_lease(self):
        for ttl in (0, -1, float('inf'), float('nan')):
            lease = policy.DemandLease()
            lease.publish({'a'}, 10, ttl=ttl)
            self.assertFalse(lease.permits(10))
        lease.publish({'a'}, 10, ttl=1)
        self.assertTrue(lease.permits(10.99))
        self.assertFalse(lease.permits(11))

    def test_stop_failure_keeps_handle_for_cleanup(self):
        stop = Mock(side_effect=RuntimeError('stop failed'))
        gate = policy.CaptureGate(Mock(return_value=object()), stop)
        lease = policy.DemandLease()
        lease.publish({'a'}, 1)
        stream = gate.update(True, lease, 1)
        with self.assertRaises(RuntimeError):
            gate.update(False, lease, 1)
        self.assertIs(gate.stream, stream)
        stop.side_effect = None
        gate.close()
        self.assertIsNone(gate.stream)


class RegistryFixture(OfflineTest):
    def setUp(self):
        super().setUp()
        self.stack.enter_context(patch.dict(os.environ, PIPEWIRE_REMOTE='oink-test'))
        self.dump = [dict(id=0, type='PW:Core', info={'cookie': 42}),
                     self.node(1, 100, runtime.SOURCE, 'Audio/Source'),
                     self.node(2, 101, runtime.NATIVE),
                     self.link(3), self.link(4)]
        self.rows = []
        self.replace(runtime, 'graph', side_effect=lambda: self.dump)
        self.replace(runtime, 'pulse', side_effect=lambda kind: self.rows)
        self.source = policy.NodeIdentity('oink-test:42', 1, '100')
        self.key = policy.NodeIdentity('oink-test:42', 2, '101')

    @staticmethod
    def node(index, serial, name, media_class='Stream/Input/Audio'):
        return dict(id=index, type='PW:Node', info={'state': 'running', 'props': {
            'object.serial': serial, 'node.name': name, 'media.class': media_class,
            'application.process.id': 55}})

    @staticmethod
    def link(index, source=1, target=2):
        return dict(id=index, type='PW:Link', info={'output-node-id': source,
                    'input-node-id': target, 'state': 'active'})

    @staticmethod
    def row():
        return dict(index=7, source=9, corked=False, properties={'object.serial': '101'})

    def observe(self, internal=frozenset()):
        return runtime.observe(9, internal, self.source)


class NativeRegistryTests(RegistryFixture):
    def test_native_only_running_audio_target_links_grant_once(self):
        self.assertEqual(self.observe(), frozenset([self.key]))
        self.assertIsInstance(next(iter(self.observe())), policy.NodeIdentity)

    def test_pulse_mirror_deduplicates_by_serial_not_index_or_name(self):
        self.rows = [self.row()]
        self.assertEqual(self.observe(), frozenset([self.key]))
        self.rows[0]['index'] = 999
        self.rows[0]['properties']['media.name'] = runtime.FEED
        self.assertEqual(self.observe(), frozenset([self.key]))
        self.rows[0]['corked'] = True
        self.assertEqual(self.observe(), frozenset())  # No native fallback around cork.

    def test_registration_source_running_or_wrong_direction_never_grants(self):
        self.dump = self.dump[:3]
        self.assertEqual(self.observe(), frozenset())
        self.dump.append(self.link(3, source=2, target=1))
        self.assertEqual(self.observe(), frozenset())
        self.dump[-1] = self.link(3, source=99)
        self.assertEqual(self.observe(), frozenset())

    def test_known_inactive_node_and_link_states_never_grant(self):
        for state in ('error', 'creating', 'suspended', 'idle'):
            self.dump[2]['info']['state'] = state
            self.assertEqual(self.observe(), frozenset(), state)
        self.dump[2]['info']['state'] = 'running'
        for state in ('error', 'unlinked', 'init', 'negotiating', 'allocating', 'paused'):
            for link in self.dump[3:]:
                link['info']['state'] = state
            self.assertEqual(self.observe(), frozenset(), state)

    def test_unknown_states_and_non_audio_input_fail_closed(self):
        for state in (None, 'RUNNING', 'future', False, {}):
            self.dump[2]['info']['state'] = state
            self.assertIsNone(self.observe())
        self.dump[2]['info']['state'] = 'running'
        for state in (None, 'ACTIVE', 'future', False, {}):
            self.dump[3]['info']['state'] = state
            self.assertIsNone(self.observe())
        self.dump[3]['info']['state'] = 'active'
        for media_class in (None, 'Audio/Source', 'Stream/Output/Audio', 'Stream/Input/Video'):
            self.dump[2]['info']['props']['media.class'] = media_class
            self.assertIsNone(self.observe())

    def test_invalid_node_identity_fails_closed(self):
        original = deepcopy(self.dump)
        for index in (None, '2', True, -1):
            self.dump = deepcopy(original)
            self.dump[2]['id'] = index
            self.assertIsNone(self.observe())
        for serial in (None, 'unknown', True, -1, '１０１', {}):
            self.dump = deepcopy(original)
            self.dump[2]['info']['props']['object.serial'] = serial
            self.assertIsNone(self.observe())

    def test_missing_or_ambiguous_registry_objects_fail_closed(self):
        original = deepcopy(self.dump)
        for field in ('id', 'info', 'type'):
            self.dump = deepcopy(original)
            self.dump[2].pop(field)
            self.assertIsNone(self.observe())
        for node in (self.node(2, 102, 'other'), self.node(5, 101, 'other')):
            self.dump = deepcopy(original) + [node]
            self.assertIsNone(self.observe())
        self.dump = deepcopy(original)
        self.dump.pop(2)  # Dangling target link.
        self.assertIsNone(self.observe())

    def test_link_endpoint_schema_fails_closed(self):
        for field in ('output-node-id', 'input-node-id'):
            for value in (None, '2', True, []):
                original = self.dump[3]['info'][field]
                self.dump[3]['info'][field] = value
                self.assertIsNone(self.observe())
                self.dump[3]['info'][field] = original

    def test_malformed_pulse_mirror_does_not_become_native_only(self):
        for field, value in (('source', None), ('source', '9'), ('corked', 'false'),
                             ('corked', None), ('index', True), ('index', -1),
                             ('properties', {}), ('properties', None)):
            self.rows = [self.row()]
            self.rows[0][field] = value
            self.assertIsNone(self.observe())
        self.rows = [self.row(), self.row()]
        self.assertIsNone(self.observe())
        self.rows = [self.row()]
        self.rows[0]['source'] = 8  # Contradiction with actual SOURCE links.
        self.assertIsNone(self.observe())
        self.rows[0]['source'] = 9
        self.rows[0]['properties']['object.serial'] = 'unknown'
        self.assertIsNone(self.observe())

    def test_internal_exclusion_is_full_registry_identity_only(self):
        self.assertEqual(self.observe({self.key}), frozenset())
        self.rows = [self.row()]
        self.assertEqual(self.observe({self.key}), frozenset())
        self.dump[2]['info']['props']['node.name'] = runtime.FEED
        self.assertEqual(self.observe(), frozenset([self.key]))
        self.assertIsNone(self.observe({policy.identity(self.rows[0], self.key.server)}))
        self.rows = []
        for old in (policy.NodeIdentity(self.key.server, 8, self.key.serial),
                    policy.NodeIdentity(self.key.server, self.key.id, '99')):
            self.assertEqual(self.observe({old}), frozenset([self.key]))
        self.assertIsNone(self.observe({policy.NodeIdentity('old-server', 2, '101')}))

    def test_internal_feed_plus_same_named_native_only_consumer(self):
        self.dump[2]['info']['props']['node.name'] = runtime.FEED
        self.dump.extend([self.node(5, 105, runtime.FEED), self.link(6, target=5)])
        second = policy.NodeIdentity(self.key.server, 5, '105')
        for mirrored in (False, True):
            self.rows = [self.row()] if mirrored else []
            self.assertEqual(self.observe({self.key}), frozenset([second]))
        self.dump = self.dump[:5]
        self.assertEqual(self.observe({self.key}), frozenset())

    def test_unknown_linked_schema_blocks_other_known_consumers(self):
        self.dump.extend([self.node(5, 105, 'unknown'), self.link(6, target=5)])
        self.dump[5]['info']['props'].pop('media.class')
        self.assertIsNone(self.observe())
        self.assertIsNone(self.observe({self.key}))
        self.rows = [self.row()]
        self.assertIsNone(self.observe())

    def test_unknown_observation_revokes_existing_native_capture(self):
        lease = policy.DemandLease()
        stop = Mock()
        gate = policy.CaptureGate(Mock(return_value=object()), stop)
        lease.publish(self.observe(), 1)
        capture = gate.update(True, lease, 1)
        self.dump[2]['info']['state'] = 'unknown-schema'
        self.assertIsNone(self.observe())
        lease.publish(self.observe(), 2)
        self.assertIsNone(gate.update(True, lease, 2))
        stop.assert_called_once_with(capture)

    def test_source_identity_and_epoch_replacement_fail_closed(self):
        self.dump[1]['info']['props']['object.serial'] = 999
        self.assertIsNone(self.observe())
        self.dump[1]['info']['props']['object.serial'] = 100
        self.dump[0]['info']['cookie'] = 43
        self.assertIsNone(self.observe())

    def test_epoch_change_across_pulse_query_fails_closed(self):
        changed = deepcopy(self.dump)
        changed[0]['info']['cookie'] = 43
        self.replace(runtime, 'graph', side_effect=[self.dump, changed])
        self.assertIsNone(self.observe())

    def test_unknown_core_or_unavailable_service_fails_closed(self):
        for cookie in (None, '42', False, -1, {}):
            self.dump[0]['info']['cookie'] = cookie
            self.assertIsNone(self.observe())
        self.dump.pop(0)
        self.assertIsNone(self.observe())
        for error in (RuntimeError('offline'), OSError('offline'),
                      subprocess.TimeoutExpired('pw-dump', 5)):
            self.replace(runtime, 'graph', side_effect=error)
            self.assertIsNone(self.observe())

    def test_native_partial_and_last_exit_drive_capture_callbacks(self):
        self.dump.extend([self.node(5, 105, 'second'), self.link(6, target=5)])
        second = policy.NodeIdentity(self.key.server, 5, '105')
        self.assertEqual(self.observe(), frozenset([self.key, second]))
        lease = policy.DemandLease()
        start, stop = Mock(return_value=object()), Mock()
        gate = policy.CaptureGate(start, stop)
        lease.publish(self.observe(), 1)
        self.assertIsNone(gate.update(False, lease, 1))
        handle = gate.update(True, lease, 1)
        self.dump = self.dump[:2] + self.dump[5:]
        self.assertEqual(self.observe(), frozenset([second]))
        lease.publish(self.observe(), 2)
        self.assertIs(gate.update(True, lease, 2), handle)
        self.dump = self.dump[:2]
        self.assertEqual(self.observe(), frozenset())
        lease.publish(self.observe(), 3)
        self.assertIsNone(gate.update(True, lease, 3))
        start.assert_called_once()
        stop.assert_called_once_with(handle)


class NativePlumbingTests(RegistryFixture):
    def setUp(self):
        super().setUp()
        self.dump[2]['info']['props'].pop('application.process.id')
        self.dump[2]['info']['props']['client.id'] = 8
        self.dump.append(dict(id=8, type='PW:Client', info={'props': {
            'object.serial': 108, 'application.process.id': 55}}))

    def test_pw_cat_rejects_missing_ambiguous_or_foreign_client(self):
        rig = runtime.Rig(runtime.Resources())
        process = Mock(pid=55)
        process.poll.return_value = None
        self.replace(rig, 'process', return_value=process)
        self.replace(base, 'wait', side_effect=lambda predicate, **kwargs: predicate())
        original = deepcopy(self.dump)
        for fault in ('missing', 'ambiguous', 'foreign', 'invalid_id'):
            self.dump = deepcopy(original)
            if fault == 'missing':
                self.dump.pop()
            elif fault == 'ambiguous':
                self.dump.append(deepcopy(self.dump[-1]))
            elif fault == 'foreign':
                self.dump[-1]['info']['props']['application.process.id'] = 99
            else:
                self.dump[2]['info']['props']['client.id'] = '8'
            with self.subTest(fault=fault), self.assertRaises(RuntimeError):
                rig.native_reader(self.source)

    def test_pw_cat_explicit_private_target_pcm_stdout_and_client_config(self):
        rig = runtime.Rig(runtime.Resources())
        process = Mock(pid=55)
        process.poll.return_value = None
        self.replace(rig, 'process', return_value=process)
        self.replace(base, 'wait', side_effect=lambda predicate, **kwargs: predicate())
        self.assertEqual(rig.native_reader(self.source), (process, self.key))
        args, kwargs = rig.process.call_args
        self.assertEqual(args, ('pw-cat', '--record', '--format=s16', '--rate=48000',
                               '--channels=2', '--latency=40ms', '--target=' + runtime.SOURCE,
                               '--properties={ node.name = ' + runtime.NATIVE + ' }', '-'))
        self.assertEqual(kwargs['stdin'], subprocess.DEVNULL)
        self.assertEqual(kwargs['stdout'], subprocess.PIPE)
        self.assertEqual(kwargs['env']['PIPEWIRE_CONFIG_NAME'], 'client.conf')
        self.assertEqual(kwargs['env']['PIPEWIRE_REMOTE'], 'oink-test')
        runtime.spawn.assert_not_called()

    def test_pw_cat_registration_requires_owned_process_identity(self):
        rig = runtime.Rig(runtime.Resources())
        process = Mock(pid=99)
        process.poll.return_value = None
        self.replace(rig, 'process', return_value=process)
        self.replace(base, 'wait', side_effect=lambda predicate, **kwargs: predicate())
        with self.assertRaisesRegex(RuntimeError, 'process_identity_mismatch'):
            rig.native_reader(self.source)

    def test_stop_native_checks_identity_not_name_and_waits_for_pulse(self):
        rig = runtime.Rig(runtime.Resources())
        process = Mock()
        process.poll.return_value = 0
        stopped = self.replace(runtime, 'stop')
        self.dump[2]['info']['props']['object.serial'] = 999  # Same name/id, new object.
        self.replace(base, 'wait', side_effect=lambda predicate, **kwargs: predicate())
        rig.stop_native_reader(process, self.key)
        stopped.assert_called_once_with(process)
        self.rows = [self.row()]
        with self.assertRaisesRegex(RuntimeError, 'survived_stop'):
            rig.stop_native_reader(process, self.key)
        self.rows = []
        process.poll.return_value = None
        with self.assertRaisesRegex(RuntimeError, 'not_reaped'):
            rig.stop_native_reader(process, self.key)

    def test_native_visibility_uses_real_serial_not_name(self):
        status = runtime.native_status(self.source, self.key, 9)
        self.assertEqual(status['native_only_evidence'], 'real_unmirrored')
        self.assertEqual((status['pulse_visible'], status['pulse_same_serial']), (0, 0))
        self.assertEqual((status['node_id'], status['serial']), (2, '101'))
        self.assertEqual((status['active_links'], status['target_active_links']), (2, 2))
        self.rows = [self.row()]
        self.rows[0]['properties']['node.name'] = 'different-name'
        status = runtime.native_status(self.source, self.key, 9)
        self.assertEqual(status['native_only_evidence'], 'not_exercised_pulse_mirrored')
        self.assertEqual((status['pulse_visible'], status['pulse_target']), (1, 1))

    def test_native_experiment_keeps_real_measure_and_final_reap_contract(self):
        rig = Mock()
        consumer, capture = Mock(), Mock()
        consumer.poll.side_effect = [None, None, 0]
        capture.poll.return_value = 0
        rig.native_reader.return_value = (consumer, self.key)
        experiment = probe.Experiment(rig, probe.Report())
        experiment.source_index, experiment.source_key = 9, self.source
        self.replace(experiment, 'measure')
        self.replace(experiment, 'demand', return_value=frozenset([self.key]))
        self.replace(probe, 'native_status', return_value=runtime.native_status(self.source,
                                                                              self.key, 9))
        routed = self.replace(probe, 'route')

        def reconcile(stage, enabled, expected):
            experiment.gate.stream = capture if stage == 'native_consumer_capture_enabled' else None

        reconciled = self.replace(experiment, 'reconcile', side_effect=reconcile)
        with redirect_stdout(io.StringIO()):
            experiment.native_consumer()
        self.assertEqual([call.args for call in reconciled.call_args_list], [
            ('before_native_consumer', True, 0), ('native_registered_without_link', True, 0),
            ('native_consumer_consent_disabled', False, 1),
            ('native_consumer_capture_enabled', True, 1), ('last_native_consumer_exit', True, 0)])
        self.assertEqual([call.args[0] for call in experiment.measure.call_args_list], [
            'native_disabled_real_pcm', 'native_enabled_real_pcm',
            'native_exit_internal_feed_is_not_demand'])
        routed.assert_called_once_with(runtime.SOURCE, runtime.NATIVE, pump=rig.pump)
        rig.stop_native_reader.assert_called_once_with(consumer, self.key)
        self.assertIsNone(experiment.consumer)
        capture.poll.assert_called_once()
        self.assertEqual(consumer.poll.call_count, 3)


if __name__ == '__main__':
    unittest.main()
