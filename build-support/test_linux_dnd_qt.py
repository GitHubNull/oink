"""Offline Qt matrix/build contract tests; no X connection or native input."""
import contextlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import linux_dnd_protocol as protocol
import linux_dnd_qt_build as build
import linux_dnd_qt_scenarios as scenarios


class QtKitTests(unittest.TestCase):
    def test_pkgconfig_overrides_unrelocated_official_prefix_only_for_qt(self):
        kit = Path('/private/kit with spaces')
        with mock.patch.object(build.subprocess, 'check_output', return_value='-lQt6Widgets') as run:
            self.assertEqual(build.flags(['Qt6Widgets'], kit), ['-lQt6Widgets'])
        command = run.call_args.args[0]
        self.assertIn('--define-variable=prefix=/private/kit with spaces', command)
        self.assertEqual(run.call_args.kwargs['env']['PKG_CONFIG_PATH'], str(kit / 'lib/pkgconfig'))
        with mock.patch.object(build.subprocess, 'check_output', return_value='-lX11') as run:
            build.flags(['x11'])
        self.assertFalse(any(arg.startswith('--define-variable') for arg in run.call_args.args[0]))

    def test_missing_kit_never_falls_back_to_system_compiler(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(build.subprocess, 'run') as run:
            with self.assertRaises(RuntimeError):
                build.build(Path(directory), Path(directory) / 'output')
            run.assert_not_called()

    def test_wrong_qt_version_cannot_produce_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            kit = Path(directory)
            for relative in ('bin/qmake', 'lib/pkgconfig/Qt6Widgets.pc', 'plugins/platforms/libqxcb.so'):
                path = kit / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            with mock.patch.object(build.subprocess, 'check_output', return_value='6.4.2\n'), \
                    mock.patch.object(build.subprocess, 'run') as run, self.assertRaises(RuntimeError):
                build.build(kit, kit / 'output')
            run.assert_not_called()


class QtEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.role = mock.Mock()
        self.role.process.pid = 123
        self.kit = Path('/project/.tools/Qt/6.12.0/gcc_64')

    def test_local_qt_and_xcb_library_mapping_is_required(self):
        maps = f'0-1 r-xp 0 0 1 {self.kit}/lib/libQt6Core.so.6\n'
        plugin = f'0-1 r-xp 0 0 1 {self.kit}/plugins/platforms/libqxcb.so\n'
        with mock.patch.object(Path, 'read_text', return_value=maps + plugin):
            scenarios.verify_qt_maps(self.role, self.kit)
        for invalid in (maps, plugin, maps + plugin + '0-1 r-xp 0 0 1 /usr/lib/libQt6Gui.so.6\n'):
            with self.subTest(invalid=invalid), \
                    mock.patch.object(Path, 'read_text', return_value=invalid), \
                    self.assertRaises(protocol.Failure):
                scenarios.verify_qt_maps(self.role, self.kit)

    def test_directory_prefix_collision_is_not_a_local_library(self):
        maps = (f'0-1 r-xp 0 0 1 {self.kit}-foreign/lib/libQt6Core.so.6\n'
                f'0-1 r-xp 0 0 1 {self.kit}/plugins/platforms/libqxcb.so\n')
        with mock.patch.object(Path, 'read_text', return_value=maps), self.assertRaises(protocol.Failure):
            scenarios.verify_qt_maps(self.role, self.kit)

    def test_foreign_xcb_plugin_prefix_collision_is_rejected(self):
        maps = (f'0-1 r-xp 0 0 1 {self.kit}/lib/libQt6Core.so.6\n'
                f'0-1 r-xp 0 0 1 {self.kit}-foreign/plugins/platforms/libqxcb.so\n')
        with mock.patch.object(Path, 'read_text', return_value=maps), self.assertRaises(protocol.Failure):
            scenarios.verify_qt_maps(self.role, self.kit)

    def test_exit_errors_and_warnings_prevent_clean_evidence(self):
        for code, errors in ((1, b''), (0, b'Qt warning\n')):
            with self.subTest(code=code, errors=errors):
                role = mock.Mock(errors=bytearray())
                role.process.wait.return_value = code
                role.process.stderr.read.return_value = errors
                group = scenarios.ToolkitGroup(Path('/binary'))
                group.roles = [role]
                with self.assertRaises(protocol.Failure):
                    group.finish()
                role.command.assert_called_once_with(protocol.QUIT)

    def test_matrix_includes_both_directions_cancel_and_negative_identity(self):
        with mock.patch.object(Path, 'is_file', return_value=True), \
                mock.patch.object(scenarios.os, 'access', return_value=True), \
                mock.patch.object(scenarios, 'isolated_environment', return_value={}), \
                mock.patch.object(scenarios, 'scenario') as scenario, \
                mock.patch.object(scenarios, 'rejected_identity') as rejected, \
                mock.patch.object(scenarios, 'rejected_frames') as frames, \
                contextlib.redirect_stdout(io.StringIO()):
            result = scenarios.main(['--gtk', '/gtk', '--qt', '/qt', '--kit', '/kit'])
        self.assertEqual(result, 0)
        self.assertEqual([call.args[-3:] for call in scenario.call_args_list],
                         [(source, target, cancel) for source, target in
                          [('qt', 'qt'), ('qt', 'gtk'), ('gtk', 'qt')] for cancel in (False, True)])
        self.assertEqual(rejected.call_count, 2)
        self.assertTrue(rejected.call_args.kwargs['wrong_owner'])
        frames.assert_called_once_with(Path('/qt'), {})

    def test_missing_binary_is_failure_not_silent_skip(self):
        with mock.patch.object(Path, 'is_file', return_value=False), \
                mock.patch.object(scenarios, 'scenario') as scenario, \
                contextlib.redirect_stdout(io.StringIO()):
            result = scenarios.main(['--gtk', '/gtk', '--qt', '/qt', '--kit', '/kit'])
        self.assertEqual(result, 1)
        scenario.assert_not_called()

    def test_qt_sources_and_shared_headers_stay_below_limit(self):
        names = ('linux_dnd_qt.h', 'linux_dnd_qt_probe.cpp', 'linux_dnd_qt_control.cpp',
                 'linux_dnd_wire.h', 'linux_dnd_native.h', 'linux_dnd_qt_build.py',
                 'linux_dnd_qt_scenarios.py', 'test_linux_dnd_qt.py')
        for name in names:
            with self.subTest(name=name):
                lines = Path(__file__).with_name(name).read_text().splitlines()
                self.assertLessEqual(len(lines), 500)
                self.assertTrue(all(line == line.rstrip() for line in lines))


if __name__ == '__main__':
    unittest.main()
