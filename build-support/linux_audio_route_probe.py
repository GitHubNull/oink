#!/usr/bin/env python3
"""Synthetic route/permit experiments; launch DIRECTLY through linux-native-test.py.

No hardware, audio files, default-device writes, session manager, or service kills.
The reference tone and metering/local-feed fixtures stay alive while the *sending*
parec stream is governed by StartPermit. This is a mechanism probe, not a backend.
"""
import shutil
import signal
import sys
import time

import linux_audio_probe as base
from linux_audio_probe import isolation_guard, command, graph, pulse, route, stop
from linux_audio_route_policy import CaptureGate, DemandLease, identity, node_identity, tone
from linux_audio_route_runtime import (CAPTURE, FEED, NATIVE, PLAY, SEND, SOURCE, SPEAKER,
                                       Resources, Rig, defaults, epoch, named, native_status,
                                       no_feedback, observe, require, source_identity, stream_rows)

ONE = 5792.56
TWO = 2896.28


class CheckFailed(RuntimeError):
    pass


class Report:
    def __init__(self):
        self.failures = 0

    def check(self, stage, condition, **values):
        base.emit(stage, status='pass' if condition else 'fail', **values)
        if not condition:
            self.failures += 1
            raise CheckFailed(stage)


class Experiment:
    def __init__(self, rig, report):
        self.rig, self.report = rig, report
        self.lease = DemandLease()
        self.enabled = False
        self.gate = CaptureGate(rig.start_capture, rig.stop_capture)
        rig.resources.add('sending-capture', self.gate.close)
        self.reference = self.speaker = self.consumer = None
        self.source_index = self.source_key = None
        self.internal = frozenset()

    def tick(self):
        return self.gate.update(self.enabled, self.lease, time.monotonic())

    def demand(self, stage, expected):
        result = None

        def ready():
            nonlocal result
            result = observe(self.source_index, self.internal, self.source_key)
            return result is not None and len(result) == expected

        self.report.check(stage, bool(base.wait(ready, pump=self.rig.pump)),
                          active='unknown' if result is None else len(result),
                          excluded_internal=len(self.internal))
        return result

    def reconcile(self, stage, enabled, expected):
        self.enabled = enabled
        self.lease.publish(self.demand(stage + '_demand', expected), time.monotonic())
        self.tick()
        self.stream_state(stage, enabled and expected > 0)

    def stream_state(self, stage, running):
        process = self.gate.stream
        rows = stream_rows('source-outputs', SEND)
        nodes = named([obj for obj in graph() if obj['type'].endswith(':Node')], SEND)
        valid = (process is not None and process.poll() is None and len(rows) == len(nodes) == 1
                 if running else process is None and not rows and not nodes)
        self.report.check(stage, valid, capture_running=int(running),
                          pid=process.pid if process else 'none', native_nodes=len(nodes),
                          source_outputs=len(rows), starts=self.gate.starts, stops=self.gate.stops)

    def measure(self, stage, speaker_one=ONE, speaker_two=0):
        readers = [self.reference, self.speaker]
        labels = ['capture_reference', 'local_speaker']
        expected = [(ONE, 0), (speaker_one, speaker_two)]
        if self.gate.stream:
            readers.append(self.gate.stream)
            labels.append('sending_capture')
            expected.append((ONE, 0))
        if self.consumer and self.consumer.poll() is None:
            readers.append(self.consumer)
            labels.append('external_consumer')
            expected.append((ONE, 0))
        for label, meter, (one, two) in zip(labels, self.rig.measure(readers, self.tick), expected):
            self.report.check(stage + '_' + label, meter.matches(one, two), **meter.values())
        capture = [row for row in pulse('sinks') if row['name'] == CAPTURE]
        self.report.check(stage + '_capture_sink_unmuted',
                          len(capture) == 1 and capture[0]['mute'] is False)
        self.report.check(stage + '_no_feedback_path', no_feedback())

    def setup(self):
        self.rig.topology()
        self.source_index = next(row['index'] for row in pulse('sources') if row['name'] == SOURCE)
        self.rig.tone('oink-route-reference-tone', CAPTURE, tone())
        self.rig.branch()
        rows = base.wait(lambda: stream_rows('source-outputs', FEED))
        require(rows and len(rows) == 1, 'internal_feed_identity_missing')
        dump = graph()
        server = epoch(dump)
        self.source_key = source_identity(dump, server)
        serial = identity(rows[0], server).serial
        feeds = [obj for obj in dump if obj['type'].endswith(':Node')
                 and str(base.props(obj).get('object.serial')) == serial]
        require(len(feeds) == 1, 'internal_feed_registry_identity_missing')
        self.internal = frozenset([node_identity(feeds[0], server)])
        self.reference = self.rig.reader(CAPTURE + '.monitor', CAPTURE, 'oink-route-reference')
        self.speaker = self.rig.reader(SPEAKER + '.monitor', SPEAKER, 'oink-route-speaker-meter')
        self.reconcile('internal_feed_is_not_demand', True, 0)
        self.measure('baseline_fixture_only')

    def multiple_consumers(self):
        self.consumer = self.rig.reader(SOURCE, SOURCE, 'oink-route-consumer-a')
        self.reconcile('disabled_with_real_consumer', False, 1)
        self.measure('disabled_fixture_not_sender')
        self.reconcile('enabled_with_real_consumer', True, 1)
        self.measure('single_consumer')
        first = self.gate.stream
        client = self.rig.new_cork_client()
        self.reconcile('second_consumer_initially_corked', True, 1)
        meter = client.measure(0.2)
        self.report.check('initial_cork_has_no_samples', meter.samples == 0, **meter.values())
        client.cork(0)
        self.reconcile('two_real_consumers', True, 2)
        client.measure(0.3)
        meter = client.measure()
        self.report.check('libpulse_consumer_real_pcm', meter.samples > 10000
                          and abs(meter.values()['rms'] / ONE - 1) < 0.04, **meter.values())
        self.measure('two_consumers_signal')
        self.rig.stop_reader(self.consumer, 'oink-route-consumer-a')
        self.consumer = None
        self.reconcile('partial_normal_exit', True, 1)
        self.report.check('partial_exit_keeps_same_capture', self.gate.stream is first)
        self.measure('remaining_consumer_signal')
        client.cork(1)
        client.measure(0.3)
        self.reconcile('last_consumer_corked', True, 0)
        meter = client.measure(0.2)
        self.report.check('corked_has_no_samples', meter.samples == 0, **meter.values())
        client.cork(0)
        self.reconcile('last_consumer_resumed', True, 1)
        self.report.check('uncork_reopens_real_capture', self.gate.stream is not first
                          and first.poll() is not None)
        self.measure('resumed_signal')
        return client

    def routes(self):
        # Mute ONLY the independent local-playback sink-input, never CAPTURE.
        mute = self.rig.mute_lease(PLAY)
        self.measure('local_branch_muted', speaker_one=0)
        self.report.check('local_mute_restore', mute.close() == 'restored')
        self.measure('local_branch_restored')

        # A later writer wins, even when it changes the mute back to the old value.
        conflict = self.rig.mute_lease(PLAY)
        command('pactl', 'set-sink-input-mute', str(conflict.before.identity.index), '0')
        require(base.wait(lambda: conflict.read().value is False, pump=self.rig.pump),
                'external_mute_change_not_observed')
        self.report.check('lease_preserves_later_value', conflict.close() == 'value_changed')
        self.measure('later_unmute_preserved')

        receive, feed = self.rig.tone('oink-route-receive', SPEAKER, tone(2000, 4096))
        for index in range(2):
            self.measure('receive_mix_' + str(index), speaker_two=TWO)
        mute = self.rig.mute_lease(PLAY)
        self.measure('receive_survives_local_branch_mute', speaker_one=0, speaker_two=TWO)
        self.stop_player(receive, feed, 'oink-route-receive')
        self.measure('receive_stopped_no_feedback_tail', speaker_one=0)
        self.report.check('receive_phase_restore', mute.close() == 'restored')
        self.measure('receive_stopped_local_restored')

        # Same display name, different actual sink-input identity: do not restore.
        old, old_feed = self.rig.tone('oink-route-receive', SPEAKER, tone(2000, 4096))
        old_lease = self.rig.mute_lease('oink-route-receive')
        self.stop_player(old, old_feed, 'oink-route-receive')
        replacement, new_feed = self.rig.tone('oink-route-receive', SPEAKER, tone(2000, 4096))
        current = old_lease.read()
        self.report.check('replacement_has_new_identity', current is not None
                          and current.identity != old_lease.before.identity)
        self.report.check('lease_preserves_replacement', old_lease.close() == 'identity_changed'
                          and old_lease.read().value is False)
        self.measure('replacement_not_mutated', speaker_two=TWO)
        self.stop_player(replacement, new_feed, 'oink-route-receive')
        self.measure('replacement_removed_no_feedback_tail')

    def native_consumer(self):
        self.reconcile('before_native_consumer', True, 0)
        self.consumer, key = self.rig.native_reader(self.source_key)
        # With no session manager registration alone must not grant permission.
        self.reconcile('native_registered_without_link', True, 0)
        route(SOURCE, NATIVE, pump=self.rig.pump)
        self.reconcile('native_consumer_consent_disabled', False, 1)
        self.measure('native_disabled_real_pcm')
        self.report.check('native_disabled_consumer_alive', self.consumer.poll() is None)
        status = native_status(self.source_key, key, self.source_index)
        self.report.check('native_registry_and_pulse_visibility',
                          status['state'] == 'running' and status['active_links'] == 2
                          and status['target_active_links'] == 2
                          and status['pulse_same_serial'] == status['pulse_target']
                          and status['pulse_same_serial'] in (0, 1), **status)
        self.report.check('native_serial_counted_exactly_once',
                          self.demand('native_identity_demand', 1) == frozenset([key]))
        self.reconcile('native_consumer_capture_enabled', True, 1)
        self.measure('native_enabled_real_pcm')
        self.report.check('native_enabled_consumer_alive', self.consumer.poll() is None)
        capture, consumer = self.gate.stream, self.consumer
        self.rig.stop_native_reader(consumer, key)
        self.consumer = None
        self.report.check('native_consumer_process_reaped', consumer.poll() is not None)
        self.reconcile('last_native_consumer_exit', True, 0)
        self.report.check('native_exit_real_capture_process_reaped', capture.poll() is not None)
        self.measure('native_exit_internal_feed_is_not_demand')

    def stop_player(self, player, feeder, name):
        feeder.close()
        stop(player)
        require(base.wait(lambda: not stream_rows('sink-inputs', name), pump=self.rig.pump),
                'playback_stream_survived_stop')

    def permit_losses(self, client):
        self.reconcile('local_consent_revoked', False, 1)
        self.measure('disabled_sender_absent')
        self.reconcile('local_consent_restored', True, 1)
        self.measure('enabled_sender_reopened')
        # Inject an unavailable observation, NOT a private-server restart.
        self.lease.publish(None, time.monotonic())
        self.tick()
        self.stream_state('unknown_observation_stops_real_capture', False)
        self.measure('unknown_sender_absent')
        self.reconcile('known_demand_returns', True, 1)
        self.measure('known_sender_reopened')
        self.lease.revoke()
        self.tick()
        self.stream_state('receiver_lease_revocation_stops_real_capture', False)
        self.reconcile('receiver_lease_renewed', True, 1)
        self.measure('renewed_sender_reopened')
        expired_process = self.gate.stream
        self.lease.publish(self.demand('expiry_still_has_consumer', 1), time.monotonic(), ttl=0.12)
        # Real monotonic time, while polling actual PCM. No clock mock/native fiction.
        self.rig.collect([self.reference, self.speaker], duration=0.4, tick=self.tick)
        self.stream_state('expired_lease_stops_real_capture', False)
        self.report.check('expired_capture_process_reaped', expired_process.poll() is not None)
        self.measure('expired_sender_absent')
        self.reconcile('fresh_lease_after_expiry', True, 1)
        self.measure('fresh_sender_reopened')
        client.close()
        self.rig.pump_client = None
        self.reconcile('last_consumer_normal_exit', True, 0)
        self.consumer = self.rig.reader(SOURCE, SOURCE, 'oink-route-consumer-a')
        self.reconcile('consumer_before_abnormal_exit', True, 1)
        self.measure('consumer_before_kill_signal')
        victim = self.consumer
        victim.kill()  # Only our own synthetic consumer, never a runner service.
        victim.wait(timeout=2)
        self.rig.stop_reader(victim, 'oink-route-consumer-a')
        self.consumer = None
        self.report.check('consumer_was_signalled', victim.returncode == -signal.SIGKILL)
        self.reconcile('last_consumer_abnormal_exit', True, 0)
        self.measure('internal_feed_cannot_restart_sender')


def interrupted(_signal, _frame):
    raise InterruptedError('route_probe_interrupted')


def run():
    report, resources = Report(), Resources()
    initial_defaults = initial_server = None
    rig = None
    cleanup_errors = []
    try:
        # Must remain the first operation touching native services. The parent
        # must be linux-native-test.py --inside-scope, not another probe/shell.
        isolation_guard()
        base.emit('isolation', status='pass')
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, interrupted)
        for tool in ('pactl', 'pacat', 'parec', 'pw-cat', 'pw-link', 'pw-cli', 'pw-dump'):
            require(shutil.which(tool), 'missing_' + tool)
        initial_server, initial_defaults = epoch(), defaults()
        # The shared harness also loads the microphone wave's test source
        # (oink_test_mic + oink_test_source); the route wave only drives CAPTURE.
        report.check('private_initial_devices',
                     sorted(row['name'] for row in pulse('sinks')) == sorted([CAPTURE, 'oink_test_mic'])
                     and sorted(row['name'] for row in pulse('sources'))
                     == sorted([CAPTURE + '.monitor', 'oink_test_mic.monitor', 'oink_test_source']))
        report.check('capture_initially_unmuted', pulse('sinks')[0]['mute'] is False)
        base.emit('scope', audio='synthetic_only_in_memory', consumer_observer='registry_plus_pulse',
                  unknown='fail_closed', service_restart='not_exercised',
                  physical_capture='not_exercised', backend='not_implemented',
                  restore='identity_and_value_non_atomic_compare_restore',
                  permit_poll='mainloop_15ms_plus_synchronous_commands')
        rig = Rig(resources)
        experiment = Experiment(rig, report)
        experiment.setup()
        client = experiment.multiple_consumers()
        experiment.routes()
        experiment.permit_losses(client)
        experiment.native_consumer()
    except (Exception, KeyboardInterrupt) as error:
        if not isinstance(error, CheckFailed):
            report.failures += 1
        base.emit('probe_error', error=type(error).__name__, reason=str(error))
    finally:
        # Every callback runs even if a previous close/unload/restore failed.
        cleanup_errors = resources.close()
        if initial_defaults is not None:
            try:
                report.check('default_devices_unchanged', defaults() == initial_defaults,
                             before=initial_defaults, after=defaults())
                report.check('private_services_not_restarted', epoch() == initial_server)
                report.check('private_devices_restored', bool(base.wait(
                    lambda: sorted(row['name'] for row in pulse('sinks')) == sorted([CAPTURE, 'oink_test_mic'])
                    and sorted(row['name'] for row in pulse('sources'))
                    == sorted([CAPTURE + '.monitor', 'oink_test_mic.monitor', 'oink_test_source']))))
                report.check('no_owned_children_alive', rig is None or
                             all(child.poll() is not None for child in rig.children))
                report.check('no_owned_threads_alive', rig is None or
                             all(not pump.thread.is_alive() for pump in rig.pumps))
                # Module-owned record streams are permanent harness fixtures
                # (the oink_test_source remap source); leaks are client-owned.
                report.check('no_streams_leaked', not [
                    row for row in pulse('source-outputs') if row.get('owner_module') is None
                ] and not pulse('sink-inputs'))
            except Exception as error:
                cleanup_errors.append(('post_cleanup', type(error).__name__))
                base.emit('cleanup_error', resource='post_cleanup', error=type(error).__name__)
        result = int(bool(report.failures or cleanup_errors))
        base.emit('summary', failures=report.failures, cleanup_failures=len(cleanup_errors), exit=result)
    return result


if __name__ == '__main__':
    sys.exit(run())
