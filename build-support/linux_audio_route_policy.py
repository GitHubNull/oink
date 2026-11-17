"""Pure policy/signal helpers; importing this module performs no native operations."""
import array
from dataclasses import dataclass
import math
import sys

from linux_audio_probe import Meter


@dataclass(frozen=True)
class Identity:
    server: str
    index: int
    serial: str


def identity(row, server):
    serial = row.get('properties', {}).get('object.serial')
    index = row.get('index')
    if (not server or serial is None or not str(serial).isascii() or not str(serial).isdecimal()
            or type(index) is not int or index < 0):
        raise ValueError('unobservable_identity')
    return Identity(str(server), index, str(serial))


@dataclass(frozen=True)
class NodeIdentity:
    """Registry identity, deliberately distinct from a Pulse stream index."""
    server: str
    id: int
    serial: str


def node_identity(node, server):
    info = node['info']
    key = identity({'index': node['id'], 'properties': info['props']}, server)
    if not node['type'].endswith(':Node') or key.index < 0:
        raise ValueError('unobservable_node_identity')
    return NodeIdentity(key.server, key.index, key.serial)


@dataclass(frozen=True)
class Value:
    identity: Identity
    value: bool


class RestoreLease:
    """One-shot identity AND value comparison, never a name-only restore.

    The read/write boundary is not a server-side atomic CAS. This probe owns the
    private objects; it tests intervening changes, not arbitrary concurrent CAS.
    """
    def __init__(self, read, write, desired):
        self.read, self.write = read, write
        self.before = read()
        if self.before is None:
            raise RuntimeError('lease_object_missing')
        self.applied = Value(self.before.identity, desired)
        self.armed = False

    def apply(self):
        if self.armed or self.read() != self.before:
            raise RuntimeError('lease_acquire_conflict')
        # Arm before the command, so a command/ACK failure still attempts cleanup.
        self.armed = True
        self.write(self.applied.identity, self.applied.value)
        if self.read() != self.applied:
            raise RuntimeError('lease_apply_not_observed')
        return self

    def close(self):
        if not self.armed:
            return 'closed'
        current = self.read()
        if current is None or current.identity != self.applied.identity:
            self.armed = False
            return 'identity_changed'
        if current.value != self.applied.value:
            self.armed = False
            return 'value_changed'
        self.write(self.before.identity, self.before.value)
        if self.read() != self.before:
            raise RuntimeError('lease_restore_not_observed')
        self.armed = False
        return 'restored'


class DemandLease:
    def __init__(self):
        self.consumers = None
        self.deadline = 0.0

    def publish(self, consumers, now, ttl=60.0):
        self.consumers = None if consumers is None else frozenset(consumers)
        self.deadline = now + ttl

    def revoke(self):
        self.consumers = frozenset()
        self.deadline = 0.0

    def permits(self, now):
        return bool(self.consumers) and math.isfinite(self.deadline) and now < self.deadline


class CaptureGate:
    """Mirrors StartPermit: local consent AND a current, observable demand lease.

    Callbacks own a real stream. Dropping permission synchronously destroys it;
    changing one consumer while another remains must not reopen capture.
    """
    def __init__(self, start, stop):
        self.start, self.stop = start, stop
        self.stream = None
        self.starts = self.stops = 0

    def update(self, user_enabled, lease, now):
        permitted = user_enabled is True and lease.permits(now)
        if not permitted:
            self.close()
        elif self.stream is None:
            self.stream = self.start()
            if self.stream is None:
                raise RuntimeError('capture_start_returned_no_stream')
            self.starts += 1
        return self.stream

    def close(self):
        if self.stream is not None:
            # Keep the handle on failure so finally cleanup can try again.
            self.stop(self.stream)
            self.stream = None
            self.stops += 1


def consumers(rows, source, server, internal, linked, unknown_link=False):
    """Pulse-only observer. Unknown identities/schema/native links fail closed.

    linked contains serials with active explicit graph links. Internal streams
    are excluded by full identity, NOT caller-controlled display names.
    """
    if unknown_link or not server or any(key.server != str(server) for key in internal):
        return None
    active = set()
    try:
        for row in rows:
            if type(row.get('source')) is not int:
                return None
            if row['source'] != source:
                continue
            key = identity(row, server)
            if type(row.get('corked')) is not bool:
                return None
            if key not in internal and not row['corked'] and key.serial in linked:
                active.add(key)
    except (ValueError, TypeError, AttributeError):
        return None
    return frozenset(active)


def graph_consumers(dump, rows, source, source_key, internal):
    """Join real registry identities and Pulse serials; never infer demand by name.

    Pulse mirrors veto corked streams rather than adding another consumer. A
    native-only stream needs the same running-node/active-target-link evidence.
    Unknown or contradictory schemas return no permission, including for feeds.
    The caller must verify the server epoch on both sides of the Pulse snapshot.
    """
    try:
        if (type(source) is not int or not isinstance(source_key, NodeIdentity)
                or any(not isinstance(key, NodeIdentity) or key.server != source_key.server
                       for key in internal)):
            return None
        nodes, serials = {}, {}
        for obj in dump:
            if obj['type'].endswith(':Node'):
                key = node_identity(obj, source_key.server)
                if key.id in nodes or key.serial in serials:
                    return None
                nodes[key.id], serials[key.serial] = (key, obj), key
        if nodes.get(source_key.id, (None,))[0] != source_key:
            return None
        mirrors, relevant, linked = {}, set(), set()
        for row in rows:
            if type(row.get('source')) is not int or type(row.get('corked')) is not bool:
                return None
            key = identity(row, source_key.server)
            if key.serial in mirrors:
                return None
            mirrors[key.serial] = row
            if row['source'] == source:
                if key.serial not in serials:
                    return None
                relevant.add(serials[key.serial])
        for obj in dump:
            if not obj['type'].endswith(':Link'):
                continue
            info = obj['info']
            if type(info.get('output-node-id')) is not int:
                return None
            if info['output-node-id'] != source_key.id:
                continue
            if type(info.get('input-node-id')) is not int:
                return None
            key, _node = nodes[info['input-node-id']]
            state = info.get('state')
            if state not in ('error', 'unlinked', 'init', 'negotiating', 'allocating',
                             'paused', 'active'):
                return None
            relevant.add(key)
            if state == 'active':
                linked.add(key)
        active = set()
        for key in relevant:
            info = nodes[key.id][1]['info']
            if (info['props'].get('media.class') != 'Stream/Input/Audio'
                    or info.get('state') not in ('error', 'creating', 'suspended', 'idle', 'running')):
                return None
            mirror = mirrors.get(key.serial)
            if mirror is not None and mirror['source'] != source:
                return None  # A link and Pulse disagree about this stream's source.
            if (key not in internal and key in linked and info['state'] == 'running'
                    and (mirror is None or mirror['corked'] is False)):
                active.add(key)
        return frozenset(active)
    except (KeyError, ValueError, TypeError, AttributeError):
        return None


def tone(hz=1000, amplitude=8192, frames=4800):
    data = array.array('h', (round(amplitude * math.sin(2 * math.pi * hz * i / 48000))
                             for i in range(frames) for _channel in range(2)))
    if sys.byteorder != 'little':
        data.byteswap()
    return data.tobytes()


class Spectrum(Meter):
    """Streaming stereo RMS and phase-independent 1/2 kHz projections.

    480-frame windows are exact integer periods at 48 kHz. Both channels are
    checked independently (anti-phase stereo cannot cancel); no audio is saved.
    """
    size = 480
    tables = {hz: tuple((math.cos(2 * math.pi * hz * i / 48000),
                        math.sin(2 * math.pi * hz * i / 48000)) for i in range(480))
              for hz in (1000, 2000)}

    def __init__(self):
        super().__init__()
        self.block = b''
        self.windows = 0
        self.energy = {hz: [0.0, 0.0] for hz in self.tables}

    def feed(self, data):
        super().feed(data)
        self.block += data
        width = self.size * 4
        while len(self.block) >= width:
            values = array.array('h', self.block[:width])
            self.block = self.block[width:]
            if sys.byteorder != 'little':
                values.byteswap()
            for hz, table in self.tables.items():
                for channel in (0, 1):
                    signal = values[channel::2]
                    real = sum(x * trig[0] for x, trig in zip(signal, table))
                    imag = sum(x * trig[1] for x, trig in zip(signal, table))
                    self.energy[hz][channel] += 2 * (real * real + imag * imag) / self.size ** 2
            self.windows += 1

    def bands(self):
        return {hz: tuple(math.sqrt(x / max(1, self.windows)) for x in energy)
                for hz, energy in self.energy.items()}

    def matches(self, one=0, two=0):
        bands = self.bands()
        expected_rms = math.hypot(one, two)
        return (self.samples > 10000 and self.windows >= 10 and not self.holes
                and abs(self.values()['rms'] - expected_rms) < max(2, expected_rms * 0.04)
                and all(abs(value - expected) < max(20, expected * 0.04)
                        for hz, expected in ((1000, one), (2000, two))
                        for value in bands[hz]))

    def values(self):
        return dict(super().values(), windows=self.windows,
                    hz1000=tuple(round(x, 2) for x in self.bands()[1000]),
                    hz2000=tuple(round(x, 2) for x in self.bands()[2000]))


def reachable(edges, start, end):
    pending, seen = [start], set()
    while pending:
        node = pending.pop()
        if node == end:
            return True
        if node not in seen:
            seen.add(node)
            pending.extend(target for source, target in edges if source == node)
    return False
