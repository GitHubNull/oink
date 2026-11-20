"""Offline tests for the real-application wave helpers; no X server or browsers."""
import json
import os
from pathlib import Path
import tempfile
import unittest

import linux_app_harness as harness
from linux_app_x11 import X11
from linux_dnd_app_scenarios import PageServer, digest_words, page_matches
import linux_dnd_app_scenarios as scenarios
from linux_dnd_protocol import Failure


def write_stat(root, pid, ppid, starttime, comm='app'):
    (root / str(pid)).mkdir(exist_ok=True)
    # pid (comm) state ppid ... starttime is field 22 overall; after comm: state=field3
    fields = [str(pid), f'({comm})', 'S'] + ['0'] * 19
    fields[3] = str(ppid)          # ppid is the 4th field
    fields[21] = str(starttime)    # starttime is the 22nd field
    (root / str(pid) / 'stat').write_text(' '.join(fields) + '\n')


class ProcTreeTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.proc = Path(tmp.name)

    def test_tree_collects_descendants_and_rejects_outsiders(self):
        for pid, ppid in ((100, 0), (101, 100), (102, 101), (103, 100), (200, 0)):
            write_stat(self.proc, pid, ppid, 5000 + pid)
        tree = harness.process_tree(100, self.proc)
        self.assertEqual(tree, {100, 101, 102, 103})
        self.assertNotIn(200, tree)
        self.assertIsNone(harness._stat(999, self.proc))

    def test_comm_with_spaces_and_parens_parses(self):
        write_stat(self.proc, 300, 0, 42, comm='chrome --type=rend (x)')
        stat = harness._stat(300, self.proc)
        self.assertEqual(stat['ppid'], 0)
        self.assertEqual(stat['starttime'], 42)

    def test_verify_window_pid_accepts_tree_and_rejects_foreign(self):
        for pid, ppid in ((400, 0), (401, 400)):
            write_stat(self.proc, pid, ppid, 7000 + pid)
        for pid in (400, 401):
            (self.proc / str(pid) / 'exe').symlink_to('/usr/bin/true')
        class Proc:
            pid = 400
            returncode = None
            def poll(self):
                return None
        session = harness.AppSession('fake', Proc(), '/usr/bin/true', 7400, self.proc)
        self.assertEqual(session.verify_window_pid(401), '/usr/bin/true')
        with self.assertRaises(Failure):
            session.verify_window_pid(999)


class X11GuardTests(unittest.TestCase):
    def test_host_display_and_missing_root_are_rejected(self):
        with self.assertRaises(Failure):
            X11({'OINK_NATIVE_TEST_ROOT': '/tmp/oink-native-x', 'DISPLAY': ':0'})
        with self.assertRaises(Failure):
            X11({'OINK_NATIVE_TEST_ROOT': '/tmp/not-private', 'DISPLAY': ':1'})


class PageServerTests(unittest.TestCase):
    def test_serves_page_collects_reports_and_bounds_posts(self):
        page = Path(__file__).with_name('linux_dnd_app_page.html').read_bytes()
        server = PageServer(Path(__file__).with_name('linux_dnd_app_page.html'))
        self.addCleanup(server.close)
        from urllib.request import urlopen, Request
        body = urlopen(server.url, timeout=5).read()
        self.assertEqual(body, page)
        report = {'kind': 'ready', 'payload_sha256': 'a' * 64, 'payload_length': 64,
                  'source': {'x': 1, 'y': 2}, 'target': {'x': 3, 'y': 4}, 'trusted': True}
        request = Request(server.url + 'report', data=json.dumps(report).encode(),
                          headers={'Content-Type': 'application/json'})
        self.assertEqual(urlopen(request, timeout=5).read(), b'ok')
        self.assertEqual(server.wait_report('ready'), report)
        state = json.loads(urlopen(server.url + 'state', timeout=5).read())
        self.assertEqual(state[0]['kind'], 'ready')
        with self.assertRaises(Exception):
            urlopen(Request(server.url + 'report', data=b'x' * 5000), timeout=5)


class PayloadMatchTests(unittest.TestCase):
    def test_digest_words_and_page_match(self):
        hexed = '0123456789abcdef' * 4
        words = digest_words(hexed)
        report = {'length': 64}
        for i, word in enumerate(words):
            report[f'digest{i}'] = word
        page_matches(report, {'payload_sha256': hexed, 'payload_length': 64})
        report['digest3'] += 1
        with self.assertRaises(Failure):
            page_matches(report, {'payload_sha256': hexed, 'payload_length': 64})
        with self.assertRaises(Failure):
            digest_words('abcd')


class TaskHeadroomTests(unittest.TestCase):
    def test_waits_until_headroom_and_fails_closed_on_deadline(self):
        readings = iter([scenarios.TASKS_MAX - 6, scenarios.TASKS_MAX - 16, scenarios.TASKS_MAX - 40])
        original = scenarios.scope_tasks
        scenarios.scope_tasks = lambda: next(readings)
        try:
            self.assertEqual(scenarios.wait_task_headroom(minimum=32, timeout=5),
                             scenarios.TASKS_MAX - 40)
        finally:
            scenarios.scope_tasks = original
        scenarios.scope_tasks = lambda: scenarios.TASKS_MAX
        try:
            with self.assertRaises(Failure):
                scenarios.wait_task_headroom(minimum=32, timeout=0.2)
        finally:
            scenarios.scope_tasks = original


if __name__ == '__main__':
    unittest.main()
