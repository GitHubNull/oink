"""Offline checks for the dedicated-master toolkit input delivery suite."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

DIRECTORY = Path(__file__).resolve().parent
ROUTE = DIRECTORY / 'linux_input_toolkit_route.c'
QT_FIXTURE = DIRECTORY / 'linux_input_toolkit_qt.cpp'
SCENARIOS = DIRECTORY / 'linux_input_toolkit_scenarios.py'
QT_ROW = DIRECTORY / 'linux_input_toolkit_qt_row.py'
PAGE = DIRECTORY / 'linux_input_app_page.html'
BUILD = DIRECTORY / 'linux_input_toolkit_qt_build.py'
NEW_FILES = (ROUTE, QT_FIXTURE, SCENARIOS, QT_ROW, BUILD, PAGE)


class SourceContractTests(unittest.TestCase):
    def setUp(self):
        self.sources = {p.name: p.read_text() for p in NEW_FILES}
        self.addCleanup(self.sources.clear)

    def test_private_runner_guards_present(self):
        for name, source in self.sources.items():
            if not name.endswith(('.c', '.cpp')):
                continue
            self.assertIn('OINK_NATIVE_TEST_ROOT', source, name)
            self.assertIn('/tmp/oink-native-', source, name)
            self.assertIn('XAUTHORITY', source, name)
            self.assertIn('refuse', source.lower(), name)

    def test_never_host_session_shortcuts(self):
        banned = ['XOpenDisplay(":0', "XOpenDisplay(':0", 'getenv("HOME")/.Xauthority',
                  'xhost', 'xauth add :0', 'Xvfb', 'DISPLAY", ":0"']
        for name, source in self.sources.items():
            for token in banned:
                self.assertNotIn(token, source, f'{name} contains {token}')

    def test_router_exclusive_master_and_cleanup(self):
        route = self.sources['linux_input_toolkit_route.c']
        self.assertIn('XIAddMaster', route)
        self.assertIn('XIRemoveMaster', route)
        self.assertIn('XISetFocus', route)
        self.assertIn('XTestFakeRelativeMotionEvent', route)
        self.assertIn('refuse_existing_master_name', route)
        self.assertGreaterEqual(route.count('XTestFakeKeyEvent'), 2)

    def test_qt_fixture_reports_native_codes(self):
        qt = self.sources['linux_input_toolkit_qt.cpp']
        self.assertIn('nativeScanCode', qt)
        self.assertIn('wheelEvent', qt)

    def test_scenario_rows_and_no_leftover_api(self):
        scenarios = self.sources['linux_input_toolkit_scenarios.py']
        self.assertNotIn('router.command(', scenarios)
        self.assertNotIn('.abs(', scenarios)
        for name in ('zenity-entry-text', 'zenity-server-repeat', 'normal-input-comparison',
                     'firefox-trusted-input', 'chrome-trusted-input',
                     'nemo-navigation-scroll', 'nemo-key-navigation'):
            self.assertIn(f"'{name}'", scenarios)
        qt_row = self.sources['linux_input_toolkit_qt_row.py']
        self.assertIn("'qt-native-callback-delivery'", qt_row)
        self.assertIn('altgr', qt_row)
        self.assertIn('media_key', qt_row)
        self.assertIn('server_repeat', qt_row)

    def test_page_reports_trusted_events(self):
        page = PAGE.read_text()
        for token in ('isTrusted', 'probe-key', 'probe-wheel', 'probe-ready'):
            self.assertIn(token, page)

    def test_all_new_files_within_500_lines(self):
        for path in (*NEW_FILES, Path(__file__).resolve()):
            lines = path.read_text().splitlines()
            self.assertLessEqual(len(lines), 500, f'{path.name} has {len(lines)} lines')


class BuildTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        cls.root = Path(cls.directory.name)

    def compile_ok(self, name, command):
        binary = self.root / name
        subprocess.run([*command, '-o', str(binary)], check=True,
                       capture_output=True, timeout=120)
        self.assertTrue(binary.is_file() and os.access(binary, os.X_OK))

    def test_router_builds_zero_warning(self):
        flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'x11', 'xi', 'xtst'],
                                        text=True).split()
        self.compile_ok('route', ['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror',
                                  str(ROUTE), *flags, '-lXau'])


if __name__ == '__main__':
    unittest.main()
