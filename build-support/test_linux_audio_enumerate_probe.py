#!/usr/bin/env python3
"""Offline tests for linux_audio_enumerate_probe.py's comparison logic."""
import importlib.util
from pathlib import Path
import unittest


def load_probe():
    path = Path(__file__).resolve().with_name('linux_audio_enumerate_probe.py')
    spec = importlib.util.spec_from_file_location('enumerate_probe', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


probe = load_probe()


def output(name, is_default=False):
    return {'stage': 'output', 'id': f'pulse:sink:{name}', 'name': name,
            'is_default': is_default}


class CompareTests(unittest.TestCase):
    def compare(self, records, sinks, default_sink):
        return probe.compare(records, sinks, default_sink)

    def test_matching_sets_default_and_rate_pass(self):
        records = [output('line_out', True), output('hdmi'),
                   {'stage': 'default_sink_rate', 'rate': 48000}]
        sinks = [{'name': 'line_out'}, {'name': 'hdmi'}, {'name': 'oink_test'}]
        self.assertEqual(self.compare(records, sinks, 'line_out'), [])

    def test_private_sinks_are_excluded_from_both_sides(self):
        records = [output('line_out', True), {'stage': 'default_sink_rate', 'rate': 48000}]
        sinks = [{'name': 'line_out'}, {'name': 'oink_mic'}, {'name': 'oink_test'}]
        self.assertEqual(self.compare(records, sinks, 'line_out'), [])

    def test_sink_set_mismatch_fails(self):
        records = [output('line_out', True)]
        sinks = [{'name': 'line_out'}, {'name': 'hdmi'}]
        self.assertIn('sink_sets_match', self.compare(records, sinks, 'line_out'))

    def test_unprefixed_id_fails(self):
        records = [output('line_out', True), {'stage': 'output', 'id': 'raw-name'}]
        sinks = [{'name': 'line_out'}]
        self.assertIn('all_ids_prefixed', self.compare(records, sinks, 'line_out'))

    def test_private_default_sink_marks_nothing_default(self):
        records = [output('line_out'), {'stage': 'default_sink_rate', 'rate': 48000}]
        sinks = [{'name': 'line_out'}, {'name': 'oink_test'}]
        self.assertEqual(self.compare(records, sinks, 'oink_test'), [])

    def test_symbolic_default_fails_closed(self):
        records = [output('line_out'),
                   {'stage': 'default_sink_rate_error',
                    'error': 'pulse server reports no concrete default sink'}]
        sinks = [{'name': 'line_out'}, {'name': 'oink_test'}]
        self.assertEqual(self.compare(records, sinks, '@DEFAULT_SINK@'), [])

    def test_symbolic_default_with_reported_rate_fails(self):
        records = [output('line_out'), {'stage': 'default_sink_rate', 'rate': 44100}]
        sinks = [{'name': 'line_out'}]
        failures = self.compare(records, sinks, '@DEFAULT_SINK@')
        self.assertIn('symbolic_default_fails_closed', failures)

    def test_concrete_default_without_rate_fails(self):
        records = [output('line_out', True)]
        sinks = [{'name': 'line_out'}]
        self.assertIn('default_sink_rate_reported',
                      self.compare(records, sinks, 'line_out'))

    def test_enumerate_error_record_fails(self):
        records = [output('line_out', True), {'stage': 'default_sink_rate', 'rate': 1},
                   {'stage': 'outputs_error', 'error': 'boom'}]
        sinks = [{'name': 'line_out'}]
        self.assertIn('no_enumerate_errors', self.compare(records, sinks, 'line_out'))

    def test_public_sink_names_filter(self):
        sinks = [{'name': 'a'}, {'name': 'oink_x'}, {'name': None}, {'name': 'b'}]
        self.assertEqual(probe.public_sink_names(sinks), ['a', 'b'])


if __name__ == '__main__':
    unittest.main()
