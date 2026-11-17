"""Deterministic teardown ordering tests; no toolkit, display or input operations."""
import itertools
import unittest
from unittest import mock

import linux_dnd_protocol as protocol
import linux_dnd_scenarios as scenarios


class Source:
    name = 'source'

    def __init__(self, grabbed, cancel=False, native_failure=False):
        self.grabbed = iter(grabbed)
        self.calls = []
        self.native_failure = native_failure
        self.last = dict.fromkeys(protocol.FIELDS, 0)
        self.last.update(begin=1, end=1, failed=int(cancel), cancelled=int(cancel),
                         get=int(not cancel), press=1, down=1, raw_down=1, returned=1,
                         up=int(not cancel), raw_up=int(not cancel), button=int(cancel),
                         key_down=int(cancel), key_up=int(cancel), toolkit_grabbed=1)

    def command(self, opcode, *args, **kwargs):
        self.calls.append(opcode)
        if opcode == protocol.STATUS:
            self.last['toolkit_grabbed'] = next(self.grabbed, self.last['toolkit_grabbed'])
        elif opcode == protocol.FREE:
            if self.last['toolkit_grabbed']:
                raise protocol.Failure('GTK released grab')
            if self.native_failure:
                raise protocol.Failure('no ghost pointer or keyboard grab')
        elif opcode == protocol.UP:
            self.last.update(up=1, raw_up=1, button=0)
        else:
            raise AssertionError(f'unexpected command {opcode}')
        return dict(self.last)


class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(mock.patch.object(scenarios.time, 'sleep'))
        self.enterContext(mock.patch.object(scenarios, 'quiet'))

    def test_drag_end_does_not_mean_toolkit_ungrab_has_been_dispatched(self):
        for cancel in (False, True):
            with self.subTest(cancel=cancel):
                source = Source([1, 1, 0], cancel)
                scenarios.cleanup_source(source, cancel, int(not cancel))
                self.assertEqual(source.calls[:4], [protocol.STATUS] * 3 + [protocol.FREE])
                self.assertEqual(source.calls.count(protocol.FREE), 2)
                self.assertEqual(source.calls.count(protocol.UP), int(cancel))
                if cancel:
                    self.assertLess(source.calls.index(protocol.FREE), source.calls.index(protocol.UP))

    def test_stuck_toolkit_grab_times_out_without_button_release_or_free(self):
        source = Source(itertools.repeat(1), cancel=True)
        clock = itertools.count(0, .1)
        with mock.patch.object(scenarios.time, 'monotonic', side_effect=lambda: next(clock)), \
                self.assertRaisesRegex(protocol.Failure, 'timeout waiting for toolkit grab release'):
            scenarios.cleanup_source(source, True, 0)
        self.assertNotIn(protocol.FREE, source.calls)
        self.assertNotIn(protocol.UP, source.calls)
        self.assertLess(next(clock), 2)

    def test_toolkit_release_never_replaces_native_grab_assertion(self):
        source = Source([0], cancel=True, native_failure=True)
        with self.assertRaisesRegex(protocol.Failure, 'no ghost pointer or keyboard grab'):
            scenarios.cleanup_source(source, True, 0)
        self.assertIn(protocol.FREE, source.calls)
        self.assertNotIn(protocol.UP, source.calls)

    def test_missing_toolkit_evidence_fails_closed(self):
        source = mock.Mock()
        source.command.return_value = {'end': 1}
        with self.assertRaises(KeyError):
            scenarios.cleanup_source(source, False, 1)
        self.assertTrue(all(call.args[0] == protocol.STATUS for call in source.command.call_args_list))


if __name__ == '__main__':
    unittest.main()
