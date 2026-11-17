#!/usr/bin/env python3
"""Runner-direct, synthetic-only restart of a SECOND private audio service pair.

Never starts X11/DBus, loads extra modules, changes defaults, or records hardware.
Only numeric PCM measurements are emitted; samples remain bounded in memory.
"""
from contextlib import contextmanager
import importlib.util
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time

import linux_audio_probe as base
import linux_audio_route_policy as policy
import linux_audio_route_runtime as runtime

ONE = 5792.56
CONSUMER = 'oink-restart-consumer'
TONE = 'oink-restart-tone'
require = runtime.require


def load_runner():
    path = Path(__file__).resolve().with_name('linux-native-test.py')
    spec = importlib.util.spec_from_file_location('oink_restart_runner', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def validate_environment(root, outer, env):
    require(root == Path(outer['OINK_NATIVE_TEST_ROOT']) / 'restart'
            and root.is_absolute() and root.resolve() == root, 'restart_root_not_owned')
    info = root.stat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid()
            and not info.st_mode & 0o077, 'restart_root_not_private')
    expected = {
        'HOME': str(root / 'home'), 'XDG_CONFIG_HOME': str(root / 'config'),
        'OINK_NATIVE_TEST_ROOT': str(root), 'XDG_RUNTIME_DIR': str(root / 'runtime'),
        'PIPEWIRE_RUNTIME_DIR': str(root / 'runtime'), 'PIPEWIRE_REMOTE': 'oink-test',
        'PIPEWIRE_CONFIG_DIR': str(root / 'config/pipewire'),
        'PULSE_SERVER': f'unix:{root}/runtime/pulse-native',
        'PULSE_CLIENTCONFIG': str(root / 'config/pulse-client.conf'),
        'DBUS_SESSION_BUS_ADDRESS': f'unix:path={root}/runtime/session-bus',
        'DBUS_SYSTEM_BUS_ADDRESS': f'unix:path={root}/runtime/no-system-bus',
        'DISPLAY': outer['DISPLAY'], 'XAUTHORITY': outer['XAUTHORITY'],
    }
    require(all(env.get(key) == value for key, value in expected.items())
            and not any(key.startswith(('PULSE_', 'PIPEWIRE_')) and key not in expected
                        for key in env), 'private_audio_environment_required')


def prepare_environment(runner, outer):
    root = Path(outer['OINK_NATIVE_TEST_ROOT']) / 'restart'
    root.mkdir(mode=0o700)  # Exclusive: never adopt a previous root or symlink.
    env = runner.private_environment(root, outer)
    env.update({key: outer[key] for key in ('DISPLAY', 'XAUTHORITY')})
    validate_environment(root, outer, env)
    return root, env


@contextmanager
def environment(env):
    previous = dict(os.environ)
    os.environ.clear()
    os.environ.update(env)  # No inherited Pulse/PipeWire overrides or fallback.
    try:
        yield
    finally:
        os.environ.clear()
        os.environ.update(previous)


def process_token(process):
    """Check the actual parent/session/group before Session.close may signal it."""
    path = Path(f'/proc/{process.pid}/stat')
    try:
        fields = path.read_text().rsplit(')', 1)[1].split()
    except FileNotFoundError:
        require(process.poll() is not None, 'owned_process_unobservable')
        return None
    require(int(fields[1]) == os.getpid()
            and int(fields[2]) == int(fields[3]) == process.pid, 'foreign_service_process')
    return fields[19]  # starttime, not merely a reusable PID


def group_gone(pid):
    try:
        os.killpg(pid, 0)  # Existence check only; actual termination belongs to Session.
    except ProcessLookupError:
        return True
    return False


class OwnedSession:
    def __init__(self, runner, root, outer, env):
        validate_environment(root, outer, env)
        self.session = runner.Session(root, env)
        self.tokens = {}
        self.attempted = self.ready = self.closed = False

    def ownership(self):
        names, pids = set(), set()
        for name, process in self.session.processes:
            require(name in ('pipewire', 'pulse') and name not in names
                    and process.pid not in pids, 'unexpected_owned_service')
            names.add(name)
            pids.add(process.pid)
            token = process_token(process)
            if token is not None:
                require(self.tokens.setdefault(process.pid, token) == token,
                        'service_pid_reused')

    def start(self):
        require(not self.attempted, 'session_generation_reused')
        self.attempted = True
        self.session.start_audio()  # Deliberately NOT start_x11/start_dbus.
        self.ownership()
        require(len(self.session.processes) == 2, 'missing_owned_services')
        self.session.require_alive()
        self.ready = True

    def alive(self):
        require(self.ready and not self.closed, 'service_not_running')
        self.ownership()
        self.session.require_alive()

    def close(self):
        if self.closed:
            return
        self.ownership()  # Also protects partial start failures; never adopt outer PIDs.
        early_error = None
        if self.ready:
            try:
                self.session.require_alive()
            except Exception as error:
                early_error = error
        self.session.close()
        require(all(p.poll() is not None and group_gone(p.pid)
                    for _, p in self.session.processes), 'owned_services_survived_close')
        require(all(log.closed for log in self.session.logs), 'service_logs_not_closed')
        self.closed = True
        if early_error:
            raise early_error


class Permit:
    def __init__(self, start):
        self.server = None
        self.lease = policy.DemandLease()
        # Disconnection makes graph-based rig.stop_capture unsafe. Reap directly.
        self.gate = policy.CaptureGate(start, base.stop)

    def unknown(self):
        self.lease.publish(None, time.monotonic())
        self.gate.update(True, self.lease, time.monotonic())

    def bind(self, server):
        self.unknown()
        require(server and server != self.server, 'server_epoch_not_changed')
        self.server = server

    def publish(self, evidence):
        try:
            valid = (bool(evidence) and all(
                isinstance(a, policy.Identity) and isinstance(b, policy.NodeIdentity)
                and a.server == b.server == self.server and a.serial == b.serial
                for a, b in evidence))
        except (TypeError, ValueError):
            valid = False
        self.lease.publish(evidence if valid else None, time.monotonic())
        self.gate.update(True, self.lease, time.monotonic())
        return bool(valid)


def stream_evidence(dump, rows, server, source_index, source_key, process, name):
    """Demand needs the owned consumer PID AND full Pulse/registry/link evidence."""
    if source_key.server != server:
        return None
    matches = [row for row in rows if
               str(row.get('properties', {}).get('application.process.id')) == str(process.pid)]
    if len(matches) != 1:
        return None
    row = matches[0]
    if (row.get('source') != source_index or row.get('corked') is not False
            or row.get('properties', {}).get('media.name') != name):
        return None
    key = policy.identity(row, server)
    nodes = [obj for obj in dump if obj['type'].endswith(':Node')
             and str(base.props(obj).get('object.serial')) == key.serial]
    if len(nodes) != 1:
        return None
    node = nodes[0]
    node_key = policy.node_identity(node, server)
    if (base.props(node).get('media.class') != 'Stream/Input/Audio'
            or str(base.props(node).get('application.process.id')) != str(process.pid)
            or node['info'].get('state') != 'running'):
        return None
    linked = policy.graph_consumers(dump, rows, source_index, source_key, frozenset())
    links = [obj['info'] for obj in dump if obj['type'].endswith(':Link')
             and obj['info'].get('input-node-id') == node_key.id
             and obj['info'].get('output-node-id') == source_key.id
             and obj['info'].get('state') == 'active']
    if linked is None or node_key not in linked or len(links) != 2:
        return None
    return frozenset([(key, node_key)])


class Generation:
    def __init__(self, runner, root, outer, env, number):
        self.number = number
        self.resources = runtime.Resources()
        self.owned = OwnedSession(runner, root, outer, env)
        self.resources.add('owned-audio-session', self.owned.close)
        self.rig = runtime.Rig(self.resources)
        self.cleanup_errors = []
        self.finished = False
        self.server = self.source_key = self.source_index = self.before = None
        self.consumer = self.feeder = self.evidence = None
        self.query_failed = False

    def start(self):
        self.owned.start()
        self.endpoints = tuple(os.environ[key] for key in
                               ('XDG_RUNTIME_DIR', 'PIPEWIRE_RUNTIME_DIR',
                                'PIPEWIRE_REMOTE', 'PULSE_SERVER'))
        for path in (Path(self.endpoints[1]) / self.endpoints[2],
                     Path(self.endpoints[3].removeprefix('unix:'))):
            require(path.resolve() == path and stat.S_ISSOCK(path.stat().st_mode),
                    'private_audio_socket_missing')
        self.server, self.before = runtime.epoch(), runtime.defaults()
        sinks, sources = base.pulse('sinks'), base.pulse('sources')
        require([row['name'] for row in sinks] == ['oink_test']
                and [row['name'] for row in sources] == ['oink_test.monitor']
                and sinks[0]['mute'] is False, 'unexpected_private_devices')
        self.source_index = sources[0]['index']
        dump = base.graph()
        nodes = [obj for obj in dump if obj['type'].endswith(':Node')]
        sink = runtime.named(nodes, 'oink_test')
        require(len(sink) == 1, 'private_sink_identity_missing')
        self.source_key = policy.node_identity(sink[0], self.server)
        require(not base.pulse('source-outputs') and not base.pulse('sink-inputs')
                and not any(base.props(obj).get('media.class', '').startswith('Stream/')
                            for obj in nodes), 'old_stream_survived_restart')
        require(runtime.epoch() == self.server, 'startup_epoch_changed')

    def observe(self, process=None, name=CONSUMER):
        self.query_failed = False
        try:
            dump = base.graph()
            rows = base.pulse('source-outputs')
            after = base.graph()
        except (RuntimeError, OSError, subprocess.SubprocessError):
            self.query_failed = True
            return None
        if runtime.epoch(dump) != self.server or runtime.epoch(after) != self.server:
            return None
        return stream_evidence(dump, rows, self.server, self.source_index,
                               self.source_key, process or self.consumer, name)

    def healthy(self):
        self.owned.alive()
        self.rig.pump()
        require(all(p.poll() is None for p in self.rig.children), 'stream_exited_early')

    def live_observe(self, process=None, name=CONSUMER):
        result = self.observe(process, name)
        require(not self.query_failed, 'live_audio_query_failed')
        return result

    def capture(self, permit):
        _, self.feeder = self.rig.tone(TONE, 'oink_test', policy.tone())
        self.consumer = self.rig.reader('oink_test.monitor', 'oink_test', CONSUMER)
        self.evidence = base.wait(self.live_observe, pump=self.healthy)
        require(self.evidence and permit.publish(self.evidence), 'fresh_consumer_not_observed')
        sending = permit.gate.stream
        require(sending is not None and sending is not self.consumer, 'capture_not_independent')
        sent = base.wait(lambda: self.live_observe(sending, runtime.SEND), pump=self.healthy)
        require(sent and sent != self.evidence, 'sending_identity_not_observed')
        for label, meter in zip(('consumer', 'sending'),
                                self.rig.measure([self.consumer, sending], tick=self.healthy)):
            require(meter.matches(ONE, 0), 'synthetic_pcm_mismatch')
            base.emit('restart_pcm', generation=self.number, reader=label,
                      samples=meter.samples, rms=meter.values()['rms'])
        require(self.live_observe() == self.evidence, 'consumer_identity_changed')
        require(runtime.defaults() == self.before, 'inner_defaults_changed')
        self.healthy()

    def disconnect(self, permit):
        self.healthy()
        self.feeder.close()  # Join ONLY tone feeder; both real readers remain live.
        self.healthy()
        sending = permit.gate.stream
        require(sending is not None and sending.poll() is None
                and self.consumer.poll() is None, 'restart_requires_live_readers')
        self.owned.close()
        observed = self.observe()
        permit.unknown()  # Real query failure revokes lease and synchronously reaps capture.
        require(observed is None and self.query_failed, 'disconnect_not_observed_as_unknown')
        require(permit.lease.consumers is None and not permit.lease.permits(time.monotonic())
                and permit.gate.stream is None and sending.poll() is not None,
                'disconnect_capture_not_reaped')
        base.emit('restart_unknown', generation=self.number, lease_revoked=1, capture_reaped=1)

    def close(self):
        # Preserve unexpected death and every cleanup error, including across retries.
        if self.owned.ready and not self.owned.closed:
            try:
                self.owned.alive()
            except Exception as error:
                self.cleanup_errors.append(('service_alive', type(error).__name__))
        self.cleanup_errors.extend(self.resources.close())
        reaped = sum(p.poll() is not None for p in self.rig.children)
        joined = all(not feeder.thread.is_alive() for feeder in self.rig.pumps)
        require(not self.cleanup_errors and self.owned.closed
                and reaped == len(self.rig.children) and joined, 'generation_cleanup_failed')
        self.finished = True
        base.emit('restart_generation_closed', generation=self.number,
                  service_pids=len(self.owned.session.processes),
                  stream_pids=len(self.rig.children), streams_reaped=reaped, threads_joined=int(joined))


class Experiment:
    def __init__(self, runner, root, outer, env):
        self.runner, self.root, self.outer, self.env = runner, root, outer, env
        self.generations = []
        self.resources = runtime.Resources()
        self.permit = Permit(lambda: self.generations[-1].rig.start_capture())

    def new_generation(self):
        require(all(gen.finished and gen.owned.closed and not gen.cleanup_errors
                    for gen in self.generations),
                'previous_generation_not_closed')
        validate_environment(self.root, self.outer, self.env)
        gen = Generation(self.runner, self.root, self.outer, self.env, len(self.generations) + 1)
        self.generations.append(gen)
        self.resources.add('generation', gen.close)
        gen.start()
        return gen

    def run(self):
        first = self.new_generation()
        self.permit.bind(first.server)
        first.capture(self.permit)
        first.disconnect(self.permit)
        first.close()  # All old processes/threads gone BEFORE reusing any socket/config.
        second = self.new_generation()
        self.permit.bind(second.server)
        require(second.before == first.before, 'restart_defaults_changed')
        require(second.endpoints == first.endpoints, 'restart_socket_path_changed')
        starts = self.permit.gate.starts
        require(not self.permit.publish(first.evidence) and self.permit.gate.stream is None
                and not self.permit.lease.permits(time.monotonic())
                and self.permit.gate.starts == starts, 'stale_demand_restarted_capture')
        base.emit('restart_epoch', generations=2, cookie_changed=int(first.server != second.server),
                  runtime_unchanged=int(first.endpoints[:2] == second.endpoints[:2]),
                  socket_unchanged=int(first.endpoints == second.endpoints), old_streams_absent=1,
                  stale_lease_rejected=1, runtime=self.env['PIPEWIRE_RUNTIME_DIR'],
                  pulse_socket=self.env['PULSE_SERVER'],
                  pipewire_socket=str(self.root / 'runtime' / self.env['PIPEWIRE_REMOTE']))
        second.capture(self.permit)
        require(second.evidence != first.evidence and self.permit.gate.starts == 2
                and self.permit.gate.stops == 1, 'fresh_generation_capture_failed')
        self.permit.unknown()
        second.close()
        require(self.permit.gate.stops == 2, 'final_capture_not_closed')

    def close(self):
        errors = []
        try:
            self.permit.unknown()
        except Exception as error:
            errors.append(('capture_gate', type(error).__name__))
        errors.extend(self.resources.close())
        return errors


def interrupted(_signal, _frame):
    raise InterruptedError('restart_probe_interrupted')


def main():
    result, outer_state, experiment = 1, None, None
    handlers = {}
    try:
        base.isolation_guard()  # MUST precede importlib, mkdir, queries, Session and native access.
        outer = dict(os.environ)
        outer_state = runtime.epoch(), runtime.defaults()
        runner = load_runner()
        root, env = prepare_environment(runner, outer)
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, interrupted)
        with environment(env):
            experiment = Experiment(runner, root, outer, env)
            try:
                experiment.run()
            finally:
                require(not experiment.close(), 'restart_cleanup_failed')
        result = 0
    except Exception as error:
        base.emit('restart_error', status='fail', error=type(error).__name__)
    finally:
        if outer_state is not None:
            try:
                require((runtime.epoch(), runtime.defaults()) == outer_state,
                        'outer_audio_state_changed')
                base.emit('restart_outer_unchanged', defaults=1, epoch=1)
            except Exception as error:
                base.emit('restart_outer_error', error=type(error).__name__)
                result = 1
        for number, handler in handlers.items():
            signal.signal(number, handler)
        base.emit('restart_summary', status='pass' if result == 0 else 'fail', exit=result,
                  generations=len(experiment.generations) if experiment else 0)
    return result


if __name__ == '__main__':
    sys.exit(main())
