#!/usr/bin/env python3
"""Offline fakes only: subprocess, libpulse and signalling are blocked throughout."""
from contextlib import ExitStack
import ctypes
from pathlib import Path
import os
import stat
import subprocess
from types import SimpleNamespace as NS
import unittest
from unittest.mock import Mock, patch


def block_native():
    stack = ExitStack()
    for name in ('Popen', 'run', 'call', 'check_call', 'check_output', 'getoutput', 'getstatusoutput'):
        stack.enter_context(patch.object(subprocess, name,
                            side_effect=AssertionError('subprocess forbidden')))
    for name in ('CDLL', 'PyDLL'):
        stack.enter_context(patch.object(ctypes, name, side_effect=AssertionError('native forbidden')))
    for name in ('kill', 'killpg', 'system', 'popen', 'fork', 'posix_spawn', 'posix_spawnp'):
        stack.enter_context(patch.object(os, name, side_effect=AssertionError('native forbidden')))
    return stack


with block_native():  # Imports themselves must be inert too.
    import linux_audio_probe as base
    import linux_audio_restart_probe as probe
    import linux_audio_route_policy as policy
    import linux_audio_route_runtime as runtime


class Process:
    def __init__(self, pid=8123):
        self.pid, self.returncode = pid, None

    def poll(self):
        return self.returncode


def stop(process):
    process.returncode = 0


def evidence(server='test:1'):
    return frozenset([(policy.Identity(server, 4, '88'), policy.NodeIdentity(server, 7, '88'))])


def node(number, serial, name, kind, pid=None):
    props = {'object.serial': str(serial), 'node.name': name, 'media.class': kind}
    if pid is not None:
        props['application.process.id'] = str(pid)
    return {'id': number, 'type': 'PipeWire:Interface:Node',
            'info': {'props': props, 'state': 'running'}}


def observation():
    source = node(2, 22, 'oink_test', 'Audio/Sink')
    consumer = node(7, 88, CONSUMER, 'Stream/Input/Audio', 8123)
    links = [{'id': i, 'type': 'PipeWire:Interface:Link',
              'info': {'output-node-id': 2, 'input-node-id': 7, 'state': 'active'}}
             for i in (31, 32)]
    rows = [{'index': 4, 'source': 10, 'corked': False,
             'properties': {'object.serial': '88', 'application.process.id': '8123',
                            'media.name': CONSUMER}}]
    return [source, consumer, *links], rows, policy.node_identity(source, 'test:1')


CONSUMER = probe.CONSUMER


class OfflineTest(unittest.TestCase):
    def setUp(self):
        self.stack = block_native()
        self.addCleanup(self.stack.close)
        for module in (base, runtime):
            for name in ('command', 'pulse', 'graph', 'route', 'spawn', 'stop', 'isolation_guard'):
                if hasattr(module, name):
                    self.replace(module, name, side_effect=AssertionError('native helper forbidden'))
        self.replace(base, 'emit')
        self.replace(probe.signal, 'signal', side_effect=AssertionError('signal handler forbidden'))
        self.replace(Path, 'mkdir', side_effect=AssertionError('filesystem creation forbidden'))
        self.replace(runtime.threading.Thread, 'start', side_effect=AssertionError('thread forbidden'))

    def replace(self, obj, name, **kwargs):
        return self.stack.enter_context(patch.object(obj, name, **kwargs))

    def permit(self):
        self.replace(base, 'stop', side_effect=stop)
        self.sender = Process(8124)
        self.start = Mock(return_value=self.sender)
        permit = probe.Permit(self.start)
        permit.bind('test:1')
        return permit


class IsolationTests(OfflineTest):
    def test_refusal_precedes_loader_root_queries_and_start(self):
        guard = self.replace(base, 'isolation_guard', side_effect=RuntimeError('isolation_refused'))
        loader = self.replace(probe, 'load_runner')
        prepare = self.replace(probe, 'prepare_environment')
        session = self.replace(probe, 'Experiment')
        self.assertEqual(probe.main(), 1)
        guard.assert_called_once_with()
        loader.assert_not_called()
        prepare.assert_not_called()
        session.assert_not_called()
        base.graph.assert_not_called()

    def private_fixture(self):
        outer = {'OINK_NATIVE_TEST_ROOT': '/tmp/oink-native-offline', 'HOME': '/outer/home',
                 'DISPLAY': ':89', 'XAUTHORITY': '/tmp/oink-native-offline/Xauthority',
                 'PULSE_SERVER': 'unix:/host/pulse', 'PIPEWIRE_REMOTE': 'host',
                 'PULSE_SOURCE': 'microphone', 'PIPEWIRE_CONFIG_NAME': 'host.conf'}
        root = Path(outer['OINK_NATIVE_TEST_ROOT']) / 'restart'
        self.replace(Path, 'mkdir')
        self.replace(Path, 'resolve', autospec=True, side_effect=lambda path: path)
        self.replace(Path, 'stat', return_value=NS(st_mode=stat.S_IFDIR | 0o700, st_uid=os.getuid()))
        return root, outer, probe.load_runner()

    def test_private_environment_has_no_host_audio_or_bus_fallback(self):
        root, outer, runner = self.private_fixture()
        actual, env = probe.prepare_environment(runner, outer)
        self.assertEqual(actual, root)
        self.assertEqual(env['DISPLAY'], outer['DISPLAY'])
        self.assertEqual(env['XAUTHORITY'], outer['XAUTHORITY'])
        self.assertNotIn('PULSE_SOURCE', env)
        self.assertNotIn('PIPEWIRE_CONFIG_NAME', env)
        self.assertEqual(env['PULSE_SERVER'], f'unix:{root}/runtime/pulse-native')
        self.assertEqual(env['PIPEWIRE_RUNTIME_DIR'], str(root / 'runtime'))
        self.assertIn(str(root), env['DBUS_SESSION_BUS_ADDRESS'])
        with patch.dict(os.environ, outer, clear=True):
            before = dict(os.environ)
            with self.assertRaisesRegex(RuntimeError, 'restore'):
                with probe.environment(env):
                    self.assertEqual(dict(os.environ), env)
                    raise RuntimeError('restore')
            self.assertEqual(dict(os.environ), before)

    def test_environment_tampering_rejected_before_session_construction(self):
        root, outer, runner = self.private_fixture()
        _, env = probe.prepare_environment(runner, outer)
        factory = self.replace(runner, 'Session')
        for key, value in [('PULSE_SERVER', 'unix:/host/pulse'), ('PIPEWIRE_REMOTE', 'host'),
                           ('PIPEWIRE_CONFIG_NAME', 'host.conf'), ('HOME', '/host'),
                           ('DBUS_SESSION_BUS_ADDRESS', 'unix:path=/host/bus')]:
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                probe.OwnedSession(runner, root, outer, dict(env, **{key: value}))
        factory.assert_not_called()

    def test_existing_restart_root_not_adopted(self):
        runner = Mock()
        self.replace(Path, 'mkdir', side_effect=FileExistsError())
        with self.assertRaises(FileExistsError):
            probe.prepare_environment(runner, {'OINK_NATIVE_TEST_ROOT': '/tmp/oink-native-test'})
        runner.private_environment.assert_not_called()

    def test_symlink_or_nonprivate_subroot_rejected(self):
        root, outer, runner = self.private_fixture()
        _, env = probe.prepare_environment(runner, outer)
        for mode, uid in ((stat.S_IFDIR | 0o755, os.getuid()),
                          (stat.S_IFDIR | 0o700, os.getuid() + 1)):
            self.replace(Path, 'stat', return_value=NS(st_mode=mode, st_uid=uid))
            with self.assertRaisesRegex(RuntimeError, 'not_private'):
                probe.validate_environment(root, outer, env)
        self.replace(Path, 'resolve', return_value=Path('/host'))
        with self.assertRaisesRegex(RuntimeError, 'not_owned'):
            probe.validate_environment(root, outer, env)

    def test_foreign_parent_group_or_session_rejected(self):
        process = Process()
        for parent, group, session in [(1, process.pid, process.pid),
                                       (os.getpid(), 1, process.pid),
                                       (os.getpid(), process.pid, 1)]:
            fields = ['S', str(parent), str(group), str(session)] + ['0'] * 15 + ['1234']
            self.replace(Path, 'read_text', return_value=f'{process.pid} (service) ' + ' '.join(fields))
            with self.assertRaisesRegex(RuntimeError, 'foreign_service'):
                probe.process_token(process)


class OwnedSessionTests(OfflineTest):
    def owner(self):
        self.replace(probe, 'validate_environment')
        self.replace(probe, 'process_token', side_effect=lambda p: 'birth' if p.poll() is None else None)
        self.replace(probe, 'group_gone', return_value=True)
        self.session = Mock(processes=[], logs=[])
        self.session.start_audio.side_effect = lambda: self.session.processes.extend(
            [('pipewire', Process(8001)), ('pulse', Process(8002))])
        self.session.close.side_effect = lambda: [stop(p) for _, p in self.session.processes]
        owner = probe.OwnedSession(NS(Session=Mock(return_value=self.session)), Path('/unused'), {}, {})
        return owner

    def test_only_audio_start_and_owned_close_once(self):
        owner = self.owner()
        owner.start()
        owner.alive()
        owner.close()
        owner.close()
        self.session.start_audio.assert_called_once_with()
        self.session.start_x11.assert_not_called()
        self.session.start_dbus.assert_not_called()
        self.session.close.assert_called_once_with()
        self.assertTrue(owner.closed)
        with self.assertRaisesRegex(RuntimeError, 'generation_reused'):
            owner.start()

    def test_unowned_entry_and_reused_pid_never_signalled(self):
        for invalid_name in (True, False):
            owner = self.owner()
            owner.start()
            if invalid_name:
                self.session.processes.append(('outer-xephyr', Process(9999)))
            else:
                self.replace(probe, 'process_token', return_value='different-birth')
            with self.assertRaises(RuntimeError):
                owner.close()
            self.session.close.assert_not_called()

    def test_unexpected_death_still_cleans_but_cannot_pass(self):
        owner = self.owner()
        owner.start()
        self.session.require_alive.side_effect = RuntimeError('service exited')
        with self.assertRaisesRegex(RuntimeError, 'service exited'):
            owner.close()
        self.session.close.assert_called_once_with()
        self.assertTrue(owner.closed)

    def test_partial_start_failure_cleans_tracked_services(self):
        owner = self.owner()
        def fail():
            self.session.processes.append(('pipewire', Process(8001)))
            raise RuntimeError('restart failed')
        self.session.start_audio.side_effect = fail
        with self.assertRaisesRegex(RuntimeError, 'restart failed'):
            owner.start()
        owner.close()
        self.assertTrue(owner.closed)

    def test_lingering_process_group_or_close_error_not_success(self):
        for failure in ('group', 'close'):
            owner = self.owner()
            owner.start()
            if failure == 'group':
                self.replace(probe, 'group_gone', return_value=False)
            else:
                self.session.close.side_effect = RuntimeError('close failed')
            with self.assertRaises(RuntimeError):
                owner.close()
            self.assertFalse(owner.closed)


class EvidencePermitTests(OfflineTest):
    def test_cookie_change_rejects_reused_index_serial_and_old_lease(self):
        permit = self.permit()
        self.assertTrue(permit.publish(evidence()))
        permit.bind('test:2')
        self.assertIsNone(permit.lease.consumers)
        self.assertIsNone(permit.gate.stream)
        self.assertIsNotNone(self.sender.poll())
        self.assertFalse(permit.publish(evidence()))
        self.assertEqual(permit.gate.starts, 1)
        self.sender = Process(8125)
        self.start.return_value = self.sender
        self.assertTrue(permit.publish(evidence('test:2')))
        self.assertEqual(permit.gate.starts, 2)

    def test_malformed_permission_fails_closed(self):
        for malformed in ([object()], [()], [('name', 'count')]):
            permit = self.permit()
            permit.publish(evidence())
            self.assertFalse(permit.publish(malformed))
            self.assertIsNone(permit.lease.consumers)
            self.assertIsNone(permit.gate.stream)

    def test_same_cookie_is_failure_and_revokes_before_raising(self):
        permit = self.permit()
        permit.publish(evidence())
        with self.assertRaisesRegex(RuntimeError, 'epoch_not_changed'):
            permit.bind('test:1')
        self.assertFalse(permit.lease.permits(0))
        self.assertIsNone(permit.gate.stream)

    def test_stop_failure_retains_handle_but_revokes_permission(self):
        permit = self.permit()
        permit.publish(evidence())
        permit.gate.stop = Mock(side_effect=RuntimeError('cannot reap'))
        with self.assertRaisesRegex(RuntimeError, 'cannot reap'):
            permit.unknown()
        self.assertIs(permit.gate.stream, self.sender)
        self.assertIsNone(permit.lease.consumers)

    def test_demand_requires_full_identity_cork_pid_serial_and_active_links(self):
        for broken in ('none', 'cork', 'pid', 'serial', 'link', 'source', 'name', 'state', 'duplicate'):
            dump, rows, source = observation()
            if broken == 'cork': rows[0]['corked'] = True
            if broken == 'pid': rows[0]['properties']['application.process.id'] = '666'
            if broken == 'serial': rows[0]['properties']['object.serial'] = '99'
            if broken == 'link': dump[-1]['info']['state'] = 'paused'
            if broken == 'source': rows[0]['source'] = 11
            if broken == 'name': rows[0]['properties']['media.name'] = 'other'
            if broken == 'state': dump[1]['info']['state'] = 'idle'
            if broken == 'duplicate': rows.append(dict(rows[0]))
            found = probe.stream_evidence(dump, rows, 'test:1', 10, source, Process(), CONSUMER)
            with self.subTest(broken=broken):
                self.assertEqual(found, evidence() if broken == 'none' else None)

    def test_synthetic_baseline_rejects_wrong_frequency_and_empty_pcm(self):
        good, wrong = policy.Spectrum(), policy.Spectrum()
        good.feed(policy.tone(frames=9600))
        wrong.feed(policy.tone(2000, frames=9600))
        self.assertTrue(good.matches(probe.ONE, 0))
        self.assertFalse(wrong.matches(probe.ONE, 0))
        self.assertFalse(policy.Spectrum().matches(probe.ONE, 0))


class LifecycleTests(OfflineTest):
    def generation(self):
        self.owned = Mock(ready=True, closed=False)
        self.replace(probe, 'OwnedSession', return_value=self.owned)
        gen = probe.Generation(None, None, None, None, 1)
        gen.server = 'test:1'
        gen.consumer = Process()
        gen.feeder = Mock()
        return gen

    def test_disconnect_order_keeps_both_readers_alive_until_service_close(self):
        permit, gen = self.permit(), self.generation()
        permit.publish(evidence())
        events = []
        gen.feeder.close.side_effect = lambda: events.append('feeder_joined')
        def close_service():
            self.assertEqual(events, ['feeder_joined'])
            self.assertIsNone(gen.consumer.poll())
            self.assertIsNone(permit.gate.stream.poll())
            events.append('service_closed')
        self.owned.close.side_effect = close_service
        def query():
            events.append('query_failed')
            raise RuntimeError('command_failed')
        self.replace(base, 'graph', side_effect=query)
        gen.disconnect(permit)
        self.assertEqual(events, ['feeder_joined', 'service_closed', 'query_failed'])
        base.stop.assert_called_once_with(self.sender)
        self.assertIsNone(permit.lease.consumers)
        self.assertIsNone(permit.gate.stream)

    def test_early_death_is_not_treated_as_controlled_disconnect(self):
        permit, gen = self.permit(), self.generation()
        permit.publish(evidence())
        self.owned.alive.side_effect = RuntimeError('unexpected death')
        with self.assertRaisesRegex(RuntimeError, 'unexpected death'):
            gen.disconnect(permit)
        self.owned.close.assert_not_called()
        gen.feeder.close.assert_not_called()

    def test_start_refuses_old_streams_before_any_capture(self):
        gen = self.generation()
        self.replace(Path, 'resolve', autospec=True, side_effect=lambda path: path)
        self.replace(Path, 'stat', return_value=NS(st_mode=stat.S_IFSOCK))
        self.replace(runtime, 'epoch', return_value='test:2')
        self.replace(runtime, 'defaults', return_value=('oink_test', 'oink_test.monitor'))
        dump, _, _ = observation()
        self.replace(base, 'graph', return_value=dump)
        self.replace(base, 'pulse', side_effect=lambda kind: {
            'sinks': [{'name': 'oink_test', 'mute': False}],
            'sources': [{'name': 'oink_test.monitor', 'index': 10}],
            'source-outputs': [], 'sink-inputs': []}[kind])
        env = {'XDG_RUNTIME_DIR': '/private/runtime', 'PIPEWIRE_RUNTIME_DIR': '/private/runtime',
               'PIPEWIRE_REMOTE': 'oink-test', 'PULSE_SERVER': 'unix:/private/runtime/pulse'}
        with probe.environment(env), self.assertRaisesRegex(RuntimeError, 'old_stream_survived'):
            gen.start()
        self.assertEqual(gen.rig.children, [])

    def test_query_error_during_live_generation_is_not_silently_retried(self):
        gen = self.generation()
        self.replace(base, 'graph', side_effect=RuntimeError('command_failed'))
        with self.assertRaisesRegex(RuntimeError, 'live_audio_query_failed'):
            gen.live_observe()
        self.assertTrue(gen.query_failed)

    def test_unjoined_thread_cannot_finish_generation(self):
        gen = self.generation()
        self.owned.close.side_effect = lambda: setattr(self.owned, 'closed', True)
        gen.rig.pumps.append(NS(thread=NS(is_alive=lambda: True)))
        with self.assertRaisesRegex(RuntimeError, 'generation_cleanup_failed'):
            gen.close()
        self.assertFalse(gen.finished)

    def test_non_query_unknown_is_not_restart_evidence(self):
        permit, gen = self.permit(), self.generation()
        permit.publish(evidence())
        gen.observe = Mock(return_value=None)
        with self.assertRaisesRegex(RuntimeError, 'not_observed_as_unknown'):
            gen.disconnect(permit)
        self.assertIsNone(permit.gate.stream)

    def test_cleanup_failure_is_sticky_and_other_resources_still_close(self):
        gen = self.generation()
        completed = Mock()
        gen.resources.add('complete', completed)
        gen.resources.add('broken', Mock(side_effect=RuntimeError('cleanup failed')))
        self.owned.close.side_effect = lambda: setattr(self.owned, 'closed', True)
        for _ in range(2):
            with self.assertRaisesRegex(RuntimeError, 'cleanup_failed'):
                gen.close()
        completed.assert_called_once_with()
        self.owned.close.assert_called_once_with()
        self.assertFalse(gen.finished)
        self.assertEqual(len(gen.cleanup_errors), 1)

    def experiment(self, same_cookie=False, restart_failure=False):
        self.replace(probe, 'validate_environment')
        self.replace(base, 'stop', side_effect=stop)
        gens, events = [], []
        def make(*args):
            number = len(gens) + 1
            gen = Mock(server='test:1' if same_cookie else f'test:{number}',
                       before=('oink_test', 'oink_test.monitor'), finished=False,
                       endpoints=('/private/runtime', '/private/runtime', 'oink-test', 'unix:/private/pulse'),
                       cleanup_errors=[], owned=NS(closed=False))
            gen.evidence = evidence(gen.server)
            gen.rig.start_capture.return_value = Process(8200 + number)
            gen.start.side_effect = (RuntimeError('restart_failed') if restart_failure and number == 2
                                     else lambda: events.append(('start', number)))
            def capture(permit):
                self.assertIsNone(permit.gate.stream)
                self.assertFalse(permit.lease.permits(0))
                self.assertTrue(permit.publish(gen.evidence))
            gen.capture.side_effect = capture
            def close():
                events.append(('close', number))
                gen.owned.closed = gen.finished = True
            gen.close.side_effect = close
            gen.disconnect.side_effect = lambda permit: (permit.unknown(), close())
            gens.append(gen)
            return gen
        self.replace(probe, 'Generation', side_effect=make)
        exp = probe.Experiment(None, Path('/private/restart'), {},
                               {'PIPEWIRE_RUNTIME_DIR': '/private/restart/runtime',
                                'PULSE_SERVER': 'unix:/private/restart/runtime/pulse-native',
                                'PIPEWIRE_REMOTE': 'oink-test'})
        return exp, gens, events

    def test_two_generations_reap_before_restart_and_need_fresh_demand(self):
        exp, gens, events = self.experiment()
        exp.run()
        self.assertLess(events.index(('close', 1)), events.index(('start', 2)))
        self.assertEqual(exp.permit.gate.starts, 2)
        self.assertEqual(exp.permit.gate.stops, 2)
        self.assertNotEqual(gens[0].evidence, gens[1].evidence)
        self.assertEqual(exp.close(), [])

    def test_restart_failure_or_unchanged_epoch_cannot_continue(self):
        for same, failed in ((True, False), (False, True)):
            exp, gens, _ = self.experiment(same, failed)
            with self.assertRaises(RuntimeError):
                exp.run()
            self.assertIsNone(exp.permit.gate.stream)
            gens[1].capture.assert_not_called()
            self.assertEqual(exp.close(), [])

    def test_unfinished_generation_blocks_new_session(self):
        exp, _, _ = self.experiment()
        exp.generations.append(NS(finished=False, owned=NS(closed=True), cleanup_errors=[]))
        with self.assertRaisesRegex(RuntimeError, 'previous_generation_not_closed'):
            exp.new_generation()
        probe.Generation.assert_not_called()

    def test_main_never_reports_pass_on_failure_including_outer_checks(self):
        for fault in ('none', 'death', 'restart', 'cleanup', 'outer_epoch', 'outer_defaults'):
            with self.subTest(fault=fault):
                self.replace(base, 'isolation_guard')
                self.replace(probe, 'load_runner', return_value=Mock())
                self.replace(probe, 'prepare_environment', return_value=(Path('/private'), {}))
                self.replace(probe.signal, 'signal', return_value=None)
                self.replace(runtime, 'epoch', side_effect=['outer', 'changed' if fault == 'outer_epoch' else 'outer'])
                self.replace(runtime, 'defaults', side_effect=[('sink', 'source'),
                             ('changed', 'source') if fault == 'outer_defaults' else ('sink', 'source')])
                exp = Mock(generations=[1, 2])
                if fault in ('death', 'restart'):
                    exp.run.side_effect = RuntimeError(fault)
                exp.close.return_value = [('resource', 'error')] if fault == 'cleanup' else []
                self.replace(probe, 'Experiment', return_value=exp)
                self.assertEqual(probe.main(), 0 if fault == 'none' else 1)
                exp.close.assert_called_once_with()


if __name__ == '__main__':
    unittest.main()
