"""Private native plumbing. Only the guarded route probe may call these helpers."""
import os
import select
import subprocess as S
import threading
import time

import linux_audio_probe as base
from linux_audio_probe import PulseRecorder, command, graph, pulse, route, spawn, stop
from linux_audio_route_policy import (RestoreLease, Spectrum, Value, graph_consumers,
                                      identity, node_identity, reachable)

CAPTURE = 'oink_test'
SOURCE = 'oink_probe_source'  # PulseRecorder's existing explicit-route contract.
SPEAKER = 'oink_route_speaker'
SEND = 'oink-route-send'
FEED = 'oink-route-internal-feed'
PLAY = 'oink-route-local-play'
NATIVE = 'oink-route-native-consumer'


def require(value, reason):
    if not value:
        raise RuntimeError(reason)
    return value


def named(objects, name):
    return [obj for obj in objects if name in
            (base.props(obj).get('node.name'), base.props(obj).get('media.name'))]


def stream_rows(kind, name):
    return [row for row in pulse(kind) if name in
            (row.get('properties', {}).get('media.name'),
             row.get('properties', {}).get('node.name'))]


def epoch(dump=None):
    cores = [obj for obj in (graph() if dump is None else dump) if obj['type'].endswith(':Core')]
    require(len(cores) == 1, 'missing_or_ambiguous_server_core')
    cookie = cores[0].get('info', {}).get('cookie')
    require(type(cookie) is int and cookie >= 0, 'missing_server_cookie')
    return f'{os.environ["PIPEWIRE_REMOTE"]}:{cookie}'


def defaults():
    return tuple(command('pactl', 'get-default-' + kind).decode().strip()
                 for kind in ('sink', 'source'))


class Resources:
    def __init__(self):
        self.items = []

    def add(self, label, close):
        self.items.append((label, close))

    def close(self):
        errors = []
        while self.items:
            label, close = self.items.pop()
            try:
                close()
            except Exception as error:
                errors.append((label, type(error).__name__))
                base.emit('cleanup_error', resource=label, error=type(error).__name__)
        return errors


class Pump:
    """Bounded in-memory tone/PCM feed, interruptible even when the sink stalls."""
    def __init__(self, player, payload=None, reader=None):
        self.player, self.payload, self.reader = player, payload, reader
        self.stopping = threading.Event()
        self.error = None
        self.thread = threading.Thread(target=self.run, name='oink-route-pump', daemon=True)

    def start(self):
        os.set_blocking(self.player.stdin.fileno(), False)
        self.thread.start()
        return self

    def run(self):
        try:
            pending = b''
            while not self.stopping.is_set():
                if self.player.poll() is not None:
                    raise RuntimeError('playback_exited')
                if not pending:
                    if self.reader is not None:
                        if not select.select([self.reader.stdout], [], [], 0.025)[0]:
                            continue
                        pending = os.read(self.reader.stdout.fileno(), 65536)
                        if not pending:
                            raise RuntimeError('feedthrough_eof')
                    else:
                        pending = self.payload
                if select.select([], [self.player.stdin], [], 0.025)[1]:
                    try:
                        pending = pending[os.write(self.player.stdin.fileno(), pending):]
                    except BlockingIOError:
                        pass
        except Exception as error:
            if not self.stopping.is_set():
                self.error = error

    def close(self):
        self.stopping.set()
        if self.thread.ident is not None:
            self.thread.join(timeout=2)
        require(not self.thread.is_alive(), 'feed_thread_join_failed')
        if self.error:
            raise RuntimeError('feed_thread_failed') from self.error


class SafePulseRecorder(PulseRecorder):
    """Reuse libpulse/cork implementation, including cleanup of failed construction."""
    def __init__(self, source):
        self.loop = self.context = self.stream = None
        try:
            super().__init__(source)
        except BaseException:
            self.close()
            raise


class Rig:
    def __init__(self, resources):
        self.resources = resources
        self.pumps = []
        self.children = []
        self.pump_client = None

    def process(self, *args, **kwargs):
        process = spawn(*args, **kwargs)
        self.children.append(process)
        self.resources.add('process:' + str(process.pid), lambda: stop(process))
        return process

    def reader(self, source, node, name):
        process = self.process('parec', '--raw', '--format=s16le', '--rate=48000',
                               '--channels=2', '--latency-msec=40', '--device=' + source,
                               '--stream-name=' + name, stdin=S.DEVNULL, stdout=S.PIPE)
        route(node, name, pump=self.pump)
        return process

    def native_reader(self, source_key):
        require(source_identity(graph(), epoch()) == source_key, 'native_source_changed')
        process = self.process(
            'pw-cat', '--record', '--format=s16', '--rate=48000', '--channels=2',
            '--latency=40ms', '--target=' + SOURCE,
            '--properties={ node.name = ' + NATIVE + ' }', '-',
            stdin=S.DEVNULL, stdout=S.PIPE,
            env=dict(os.environ, PIPEWIRE_CONFIG_NAME='client.conf'))
        def registered():
            dump = graph()
            nodes = named([obj for obj in dump if obj['type'].endswith(':Node')], NATIVE)
            return (dump, nodes) if nodes else None

        observation = base.wait(registered, pump=self.pump)
        require(observation is not None and process.poll() is None,
                'native_consumer_registration_failed')
        dump, found = observation
        require(len(found) == 1, 'native_consumer_registration_ambiguous')
        # Native streams expose their process identity on the owning Client.
        client_id = base.props(found[0]).get('client.id')
        clients = [obj for obj in dump if obj['type'].endswith(':Client')
                   and obj['id'] == client_id]
        require(type(client_id) is int and len(clients) == 1
                and str(base.props(clients[0]).get('application.process.id')) == str(process.pid),
                'native_consumer_process_identity_mismatch')
        key = node_identity(found[0], epoch(dump))
        require(key.server == source_key.server, 'native_server_changed')
        return process, key

    def stop_native_reader(self, process, key):
        stop(process)

        def gone():
            dump = graph()
            require(epoch(dump) == key.server, 'native_server_changed')
            return (not any(node_identity(obj, key.server) == key for obj in dump
                            if obj['type'].endswith(':Node'))
                    and not any(str(row.get('properties', {}).get('object.serial')) == key.serial
                                for row in pulse('source-outputs')))

        require(base.wait(gone, pump=self.pump), 'native_consumer_survived_stop')
        require(process.poll() is not None, 'native_consumer_not_reaped')

    def player(self, sink, name):
        return self.process('pacat', '--playback', '--raw', '--format=s16le', '--rate=48000',
                            '--channels=2', '--latency-msec=40', '--device=' + sink,
                            '--stream-name=' + name, stdin=S.PIPE, stdout=S.DEVNULL)

    def feed(self, player, payload=None, reader=None):
        feeder = Pump(player, payload, reader)
        self.pumps.append(feeder)
        self.resources.add('feed-thread', feeder.close)
        feeder.start()
        return feeder

    def tone(self, name, sink, payload):
        player = self.player(sink, name)
        feeder = self.feed(player, payload=payload)
        route(name, sink, pump=self.pump)
        return player, feeder

    def load(self, module, *args):
        def current_ids():
            rows = [line.split('\t', 2) for line in
                    command('pactl', 'list', 'short', 'modules').decode().splitlines()]
            require(all(len(row) >= 2 and row[0].isascii() and row[0].isdecimal()
                        for row in rows), 'invalid_module_listing')
            return {row[0] for row in rows if row[1] == module}

        before = current_ids()
        loaded = []

        def unload():
            # Recover our new modules if the load command times out after creation.
            numbers = loaded or sorted(current_ids() - before)
            errors = []
            for number in numbers:
                try:
                    command('pactl', 'unload-module', number)
                except Exception as error:
                    errors.append(error)
            if errors:
                raise RuntimeError('module_unload_failed') from errors[0]

        self.resources.add('module:' + module, unload)
        number = command('pactl', 'load-module', module, *args).decode().strip()
        require(number.isdecimal(), 'invalid_module_id')
        loaded.append(number)
        return number

    def pump(self):
        if self.pump_client and self.pump_client.loop:
            self.pump_client.pump()
        for feeder in self.pumps:
            if feeder.error:
                raise RuntimeError('feed_thread_failed') from feeder.error

    def collect(self, readers, duration=0.65, tick=None):
        meters = [Spectrum() for _ in readers]
        end = time.monotonic() + duration
        while time.monotonic() < end:
            self.pump()
            if tick:
                tick()
            ready = select.select([p.stdout for p in readers], [], [], 0.015)[0]
            for process, meter in zip(readers, meters):
                if process.stdout in ready:
                    data = os.read(process.stdout.fileno(), 65536)
                    require(data, 'capture_eof')
                    meter.feed(data)
        return meters

    def measure(self, readers, tick=None):
        self.collect(readers, 0.4, tick=tick)  # drain stale queues and route/mute ramps
        return self.collect(readers, tick=tick)

    def stop_reader(self, process, name):
        stop(process)
        require(base.wait(lambda: not named([obj for obj in graph()
                                            if obj['type'].endswith(':Node')], name)
                          and not stream_rows('source-outputs', name), pump=self.pump),
                'record_stream_survived_stop')

    def new_cork_client(self):
        client = SafePulseRecorder(SOURCE)
        self.resources.add('libpulse-cork-client', client.close)
        self.pump_client = client
        return client

    def start_capture(self):
        return self.reader(CAPTURE + '.monitor', CAPTURE, SEND)

    def stop_capture(self, process):
        self.stop_reader(process, SEND)

    def mute_lease(self, name):
        def read():
            rows = stream_rows('sink-inputs', name)
            if not rows:
                return None
            require(len(rows) == 1 and type(rows[0].get('mute')) is bool,
                    'ambiguous_mute_object')
            return Value(identity(rows[0], epoch()), rows[0]['mute'])

        def write(key, value):
            current = read()
            require(current is not None and current.identity == key, 'mute_identity_changed')
            command('pactl', 'set-sink-input-mute', str(key.index), '1' if value else '0')
            require(base.wait(lambda: read() == Value(key, value), pump=self.pump),
                    'mute_ack_timeout')

        lease = RestoreLease(read, write, True)
        self.resources.add('mute-restore:' + name, lease.close)
        lease.apply()
        return lease

    def topology(self):
        self.load('module-null-sink', 'sink_name=' + SPEAKER,
                  'channels=2', 'rate=48000',
                  'sink_properties=priority.session=-10000')
        require(base.wait(lambda: any(row['name'] == SPEAKER for row in pulse('sinks'))),
                'speaker_sink_missing')
        self.load('module-remap-source', 'master=' + CAPTURE + '.monitor',
                  'source_name=' + SOURCE, 'channels=2', 'remix=no',
                  'source_properties=priority.session=-10000')
        require(base.wait(lambda: any(row['name'] == SOURCE for row in pulse('sources'))),
                'virtual_source_missing')
        route(CAPTURE, 'input.' + SOURCE)

    def branch(self):
        reader = self.reader(SOURCE, SOURCE, FEED)
        player = self.player(SPEAKER, PLAY)
        feeder = self.feed(player, reader=reader)
        route(PLAY, SPEAKER)
        return reader, player, feeder


def source_identity(dump, server):
    virtual = named([obj for obj in dump if obj['type'].endswith(':Node')], SOURCE)
    require(len(virtual) == 1, 'source_identity_missing_or_ambiguous')
    return node_identity(virtual[0], server)


def observe(source_index, internal, source_key=None):
    """Reconcile an unfiltered Pulse snapshot and actual native registry objects.

    The experiment pins SOURCE and feed identities at setup. Bracketing the
    Pulse query catches service changes; this is not an atomic server snapshot.
    """
    try:
        dump = graph()
        server = epoch(dump)
        current_source = source_identity(dump, server)
        if source_key is not None and current_source != source_key:
            return None
        rows = pulse('source-outputs')
        if epoch(graph()) != server:
            return None
        return graph_consumers(dump, rows, source_index, current_source, internal)
    except (KeyError, ValueError, TypeError, AttributeError, RuntimeError, OSError, S.SubprocessError):
        return None


def native_status(source_key, key, source_index):
    """Unmodified live evidence: Pulse visibility is reported, never hidden."""
    dump = graph()
    server = epoch(dump)
    require(source_identity(dump, server) == source_key and key.server == server,
            'native_source_or_server_changed')
    nodes = [obj for obj in dump if obj['type'].endswith(':Node')
             and node_identity(obj, server) == key]
    require(len(nodes) == 1, 'native_identity_changed')
    require(base.props(nodes[0]).get('media.class') == 'Stream/Input/Audio',
            'native_not_audio_input')
    links = [obj['info'] for obj in dump if obj['type'].endswith(':Link')
             and obj['info'].get('input-node-id') == key.id]
    active = [info for info in links if info.get('state') == 'active']
    rows = [row for row in pulse('source-outputs')
            if str(row.get('properties', {}).get('object.serial')) == key.serial]
    require(epoch(graph()) == server, 'native_server_changed')
    return dict(server=server, node_id=key.id, serial=key.serial,
                target_node_id=source_key.id, target_serial=source_key.serial,
                state=nodes[0]['info'].get('state'), links=len(links), active_links=len(active),
                target_active_links=sum(info.get('output-node-id') == source_key.id
                                        for info in active),
                pulse_same_serial=len(rows), pulse_target=sum(row.get('source') == source_index
                                                              for row in rows),
                pulse_visible=int(bool(rows)),
                native_only_evidence='not_exercised_pulse_mirrored' if rows else 'real_unmirrored')


def no_feedback():
    dump = graph()
    nodes = [obj for obj in dump if obj['type'].endswith(':Node')]
    speaker, capture = named(nodes, SPEAKER), named(nodes, CAPTURE)
    require(len(speaker) == len(capture) == 1, 'feedback_nodes_missing')
    edges = [(obj['info']['output-node-id'], obj['info']['input-node-id'])
             for obj in dump if obj['type'].endswith(':Link')]
    # The in-process PCM feed joins two otherwise separate native stream nodes.
    feeds, plays = named(nodes, FEED), named(nodes, PLAY)
    require(len(feeds) == len(plays) == 1, 'feed_nodes_missing')
    edges.append((feeds[0]['id'], plays[0]['id']))
    return not reachable(edges, speaker[0]['id'], capture[0]['id'])
