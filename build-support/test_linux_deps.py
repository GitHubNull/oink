"""Exercise linux-deps.sh with an isolated PATH; never install system packages."""
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().with_name('linux-deps.sh')
PACKAGES = '''build-essential pkg-config cmake curl ca-certificates xz-utils
python3 dpkg-dev libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev
librsvg2-dev libx11-dev libxi-dev libxtst-dev libxrandr-dev libxfixes-dev
libxrender-dev libxext-dev libxkbcommon-dev libxkbcommon-x11-dev libasound2-dev
libpulse-dev libfuse3-dev fuse3 libpango1.0-dev libfontconfig1-dev libcairo2-dev
fonts-noto-cjk'''.split()
NATIVE_PACKAGES = '''xserver-xephyr xauth x11-utils xinput metacity dbus-daemon
pipewire pipewire-bin pipewire-pulse pulseaudio-utils'''.split()
MODULES = '''gtk+-3.0 webkit2gtk-4.1 ayatana-appindicator3-0.1 librsvg-2.0 x11 xi
xtst xrandr xfixes xrender xext xkbcommon xkbcommon-x11 alsa libpulse fuse3
pangocairo fontconfig cairo'''.split()
COMMANDS = 'cc c++ make cmake curl xz python3 dpkg-deb fusermount3 fc-match'.split()
NATIVE_COMMANDS = '''Xephyr xauth xdpyinfo xinput metacity dbus-daemon dbus-send
pipewire pipewire-pulse pactl pacat parec pw-cli pw-cat pw-link pw-dump'''.split()
KIT_RUNTIME_PACKAGES = '''libxcb1 libx11-xcb1 libxcb-cursor0 libxcb-icccm4
libxcb-image0 libxcb-keysyms1 libxcb-randr0 libxcb-render0 libxcb-render-util0
libxcb-shape0 libxcb-shm0 libxcb-sync1 libxcb-util1 libxcb-xfixes0 libxcb-xkb1
libxkbcommon-x11-0 libgl1 libegl1 libglx0'''.split()
TOOLKIT_PACKAGES = KIT_RUNTIME_PACKAGES + ['qtbase5-dev', 'libxres-dev', 'libxres1', 'nemo']
TOOLKIT_MODULES = ['Qt5Widgets', 'xres']
TOOLKIT_COMMANDS = ['nemo', 'google-chrome', 'firefox']
FORBIDDEN = '''chmod chown chgrp setfacl setcap usermod groupmod groupadd gpasswd
adduser addgroup mknod mount umount xhost systemctl service udevadm apt dpkg'''.split()
FUSE_ERROR = '当前用户无法读写 /dev/fuse；请检查系统配置，本脚本不会放宽设备权限。'
SUCCESS = 'Linux 系统依赖检查通过；工具链版本、隔离会话与功能验收需另行检查。'
STUB = '''import json, os, sys
from pathlib import Path
name, args = Path(sys.argv[0]).name, sys.argv[1:]
config = json.loads(os.environ['STUB_CONFIG'])
with open(os.environ['STUB_LOG'], 'a', encoding='utf-8') as log:
    log.write(json.dumps([name, *args]) + '\\n')
if name == 'uname':
    print(config.get('system', 'Linux') if args == ['-s'] else config.get('arch', 'x86_64'))
elif name == 'dpkg-query':
    assert args[:2] == ['-W', '-f=${db:Status-Status}']
    status = config.get('statuses', {}).get(args[2], 'installed')
    if status == 'absent':
        sys.exit(1)
    print(status, end='')
elif name == 'pkg-config':
    assert args[0] == '--modversion'
    if args[1] in config.get('missing_modules', []):
        sys.exit(1)
    print('1.2.3')
elif name == 'sudo':
    if config.get('sudo_status'):
        sys.exit(config['sudo_status'])
    assert args[0] == 'apt-get', 'sudo may only dispatch to the local apt-get stub'
    target = str(Path(sys.argv[0]).with_name('apt-get'))
    os.execv(target, [target, *args[1:]])
elif name == 'apt-get':
    assert args[0] in ('update', 'install')
    code = config.get(args[0] + '_status', 0)
    if code:
        print('stub apt-get ' + args[0] + ' failed', file=sys.stderr)
    sys.exit(code)
else:
    sys.exit(99)  # Probed tools and forbidden mutators must never be executed.
'''


@unittest.skipUnless(sys.platform.startswith('linux'), 'Linux dependency script')
class LinuxDepsTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.log = self.root / 'commands.jsonl'
        for name in ['uname', 'dpkg-query', 'pkg-config', 'apt-get', 'sudo',
                     *COMMANDS, *NATIVE_COMMANDS, *TOOLKIT_COMMANDS, *FORBIDDEN]:
            tool = self.bin / name
            tool.write_text(f'#!{sys.executable} -I\n' + STUB, encoding='utf-8')
            tool.chmod(0o700)  # Only temporary fixtures, never device permissions.
        self.calls = []
        self.fuse_ok = (Path('/dev/fuse').is_char_device()
                        and os.access('/dev/fuse', os.R_OK | os.W_OK, effective_ids=True))

    def run_script(self, *args, needs_distro=True, extra_env=None, **config):
        if needs_distro:
            try:
                release = platform.freedesktop_os_release()
            except OSError:
                self.skipTest('No readable host os-release; no system files are changed')
            ids = (release.get('ID', '') + ' ' + release.get('ID_LIKE', '')).split()
            if not {'debian', 'ubuntu', 'linuxmint'}.intersection(ids):
                self.skipTest('Requires a supported host distro; no os-release mocking')
        self.log.write_text('', encoding='utf-8')
        # Do not inherit PATH, BASH_ENV, shell functions, or user configuration.
        env = {'PATH': str(self.bin), 'HOME': str(self.root), 'LC_ALL': 'C.UTF-8',
               'STUB_LOG': str(self.log), 'STUB_CONFIG': json.dumps(config),
               'OINK_QT_KIT': str(self.root / 'missing-qt-kit'),
               'OINK_XRES_SYSROOT': str(self.root / 'missing-xres-sysroot'),
               'OINK_TEST_QMAKE_LOG': str(self.root / 'qmake.log')}
        env.update(extra_env or {})
        result = subprocess.run(['/bin/bash', str(SCRIPT), *args], cwd=self.root,
                                env=env, capture_output=True, text=True,
                                encoding='utf-8', timeout=30)
        self.calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertFalse([call for call in self.calls if call[0] in FORBIDDEN])
        self.assertTrue(all(call[0] in ('uname', 'dpkg-query', 'pkg-config',
                                       'apt-get', 'sudo') for call in self.calls), self.calls)
        if '--install' not in args:
            self.assert_no_install()
        return result

    def make_qt_kit(self, version='6.12.0'):
        kit = self.root / 'qt-kit'
        (kit / 'bin').mkdir(parents=True)
        (kit / 'lib/cmake/Qt6Widgets').mkdir(parents=True)
        (kit / 'plugins/platforms').mkdir(parents=True)
        (kit / 'lib/cmake/Qt6Widgets/Qt6WidgetsConfig.cmake').write_text('', encoding='utf-8')
        (kit / 'plugins/platforms/libqxcb.so').write_text('', encoding='utf-8')
        qmake = kit / 'bin/qmake'
        qmake.write_text(
            '#!/bin/sh\n'
            'printf "%s\\n" "$*" >> "${OINK_TEST_QMAKE_LOG:-/dev/null}"\n'
            f'printf "QT_VERSION:{version}\\n"\n', encoding='utf-8')
        qmake.chmod(0o700)
        return kit

    def make_xres_sysroot(self):
        sysroot = self.root / 'xres-sysroot'
        (sysroot / 'usr/include/X11/extensions').mkdir(parents=True)
        (sysroot / 'usr/lib/x86_64-linux-gnu/pkgconfig').mkdir(parents=True)
        (sysroot / 'usr/include/X11/extensions/XRes.h').write_text('', encoding='utf-8')
        (sysroot / 'usr/lib/x86_64-linux-gnu/pkgconfig/xres.pc').write_text(
            'Version: 1.2.1\n', encoding='utf-8')
        (sysroot / 'usr/lib/x86_64-linux-gnu/libXRes.so').write_text('', encoding='utf-8')
        return sysroot

    def calls_for(self, name):
        return [call[1:] for call in self.calls if call[0] == name]

    def assert_no_install(self):
        self.assertEqual(self.calls_for('apt-get'), [])
        self.assertEqual(self.calls_for('sudo'), [])

    def assert_packages(self, native=False, toolkit=False, local_qt=False, local_xres=False):
        expected = list(PACKAGES) + (NATIVE_PACKAGES if native else [])
        if toolkit:
            expected += KIT_RUNTIME_PACKAGES
            if not local_qt:
                expected.append('qtbase5-dev')
            if not local_xres:
                expected.append('libxres-dev')
            expected += ['libxres1', 'nemo']
        self.assertEqual(self.calls_for('dpkg-query'),
                         [['-W', '-f=${db:Status-Status}', name] for name in expected])

    def assert_missing(self, result, packages):
        self.assertEqual([line[2:] for line in result.stdout.splitlines()
                          if line.startswith('  ')], packages)
        self.assertIn('缺少系统包：\n', result.stdout)
        self.assertNotIn(SUCCESS, result.stdout)

    def assert_checked(self, result, errors=(), toolkit=False, local_qt=False, local_xres=False):
        expected_modules = list(MODULES)
        if toolkit:
            if not local_qt:
                expected_modules.append('Qt5Widgets')
            if not local_xres:
                expected_modules.append('xres')
        self.assertEqual(self.calls_for('pkg-config'),
                         [['--modversion', name] for name in expected_modules])
        expected = list(errors) + ([] if self.fuse_ok else [FUSE_ERROR])
        self.assertEqual(result.stderr.splitlines(), expected)
        self.assertEqual(result.returncode, int(bool(expected)), result)
        self.assertEqual(SUCCESS in result.stdout, not expected)

    def assert_install(self, packages, update_only=False):
        expected = [['update']]
        if not update_only:
            expected.append(['install', '--no-install-recommends', *packages])
        self.assertEqual(self.calls_for('apt-get'), expected)
        self.assertEqual(self.calls_for('sudo'),
                         [] if os.geteuid() == 0 else [['apt-get', *call] for call in expected])

    def test_help_does_not_probe_or_install(self):
        for flag in ('-h', '--help'):
            with self.subTest(flag=flag):
                result = self.run_script(flag, needs_distro=False)
                self.assertEqual(result.returncode, 0)
                self.assertIn('默认仅检查，不安装、不修改系统。', result.stdout)
                self.assertEqual(result.stderr, '')
                self.assertEqual(self.calls, [])

    def test_unknown_argument_rejected_before_install(self):
        for args in [('--unknown',), ('--install', '--unknown')]:
            with self.subTest(args=args):
                result = self.run_script(*args, needs_distro=False)
                self.assertEqual(result.returncode, 2)
                self.assertTrue(result.stderr.startswith('未知参数：--unknown\n用法：'))
                self.assertEqual(result.stdout, '')
                self.assertEqual(self.calls, [])

    def test_non_linux_rejected_before_install(self):
        result = self.run_script('--install', needs_distro=False, system='Darwin')
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr, '此脚本仅适用于 Linux。\n')
        self.assertEqual(self.calls, [['uname', '-s']])

    def test_unsupported_architecture_rejected_before_install(self):
        for arch in ('aarch64', 'i686'):
            with self.subTest(arch=arch):
                result = self.run_script('--install', arch=arch)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stderr, f'本轮 Linux 构建基线为 x86_64，当前架构：{arch}\n')
                self.assertTrue(self.calls)
                self.assertTrue(all(call[0] == 'uname' for call in self.calls))

    def test_default_and_explicit_check_report_exact_missing_packages(self):
        for args in [(), ('--check',)]:
            with self.subTest(args=args):
                result = self.run_script(*args, statuses={'cmake': 'unpacked',
                    'libxi-dev': 'absent', 'libpulse-dev': 'config-files'})
                self.assertEqual(result.returncode, 1)
                self.assert_missing(result, ['cmake', 'libxi-dev', 'libpulse-dev'])
                self.assert_packages()
                self.assertIn('请由用户执行：bash build-support/linux-deps.sh --install',
                              result.stdout + result.stderr)
                self.assertEqual(self.calls_for('pkg-config'), [])

    def test_satisfied_base_dependencies_do_not_require_native_tools_or_install(self):
        for name in NATIVE_COMMANDS:
            (self.bin / name).unlink()
        for args in [(), ('--check',), ('--install',)]:
            with self.subTest(args=args):
                result = self.run_script(*args, statuses=dict.fromkeys(NATIVE_PACKAGES, 'absent'))
                self.assert_packages()
                self.assert_checked(result)
                self.assert_no_install()

    def test_missing_modules_report_each_error_and_fail(self):
        missing = ['x11', 'xkbcommon-x11', 'fuse3']
        result = self.run_script(missing_modules=missing)
        self.assert_packages()
        self.assert_checked(result, [f'缺少开发模块：{name}' for name in missing])

    def test_missing_base_commands_fail(self):
        missing = ['cc', 'fusermount3', 'fc-match']
        for name in missing:
            (self.bin / name).unlink()
        result = self.run_script('--check')
        self.assert_checked(result, [f'缺少命令：{name}' for name in missing])

    def test_native_tests_require_extra_packages_and_preserve_install_hint(self):
        for args in [('--native-tests',), ('--check', '--native-tests')]:
            with self.subTest(args=args):
                result = self.run_script(*args, statuses=dict.fromkeys(NATIVE_PACKAGES, 'absent'))
                self.assertEqual(result.returncode, 1)
                self.assert_packages(native=True)
                self.assert_missing(result, NATIVE_PACKAGES)
                self.assertIn('--install --native-tests', result.stdout + result.stderr)
                self.assertEqual(self.calls_for('pkg-config'), [])

    def test_native_tests_require_extra_commands(self):
        for name in NATIVE_COMMANDS:
            (self.bin / name).unlink()
        result = self.run_script('--native-tests')
        self.assert_packages(native=True)
        self.assert_checked(result, [f'缺少命令：{name}' for name in NATIVE_COMMANDS])

    def test_satisfied_native_dependencies_only_allow_real_fuse_error(self):
        result = self.run_script('--check', '--native-tests')
        self.assert_packages(native=True)
        self.assert_checked(result)

    def test_install_only_missing_packages_then_checks_modules(self):
        for native in (False, True):
            with self.subTest(native=native):
                args = ['--install'] + (['--native-tests'] if native else [])
                missing = ['cmake', 'libx11-dev'] + (['xinput'] if native else [])
                result = self.run_script(*args, statuses={'cmake': 'unpacked',
                    'libx11-dev': 'absent', 'xinput': 'absent'})
                self.assert_packages(native)
                self.assert_install(missing)
                self.assertEqual([line[2:] for line in result.stdout.splitlines()
                                  if line.startswith('  ')], missing)
                self.assert_checked(result)

    def test_install_propagates_update_and_install_failures(self):
        for stage, code in [('update', 42), ('install', 43)]:
            with self.subTest(stage=stage):
                result = self.run_script('--install', statuses={'libxi-dev': 'absent'},
                                         **{stage + '_status': code})
                self.assertEqual(result.returncode, code)
                self.assertEqual(result.stderr, f'stub apt-get {stage} failed\n')
                self.assert_missing(result, ['libxi-dev'])
                self.assert_install(['libxi-dev'], update_only=stage == 'update')
                self.assertEqual(self.calls_for('pkg-config'), [])

    def test_toolkit_dependencies_are_opt_in(self):
        for name in TOOLKIT_COMMANDS:
            (self.bin / name).unlink()
        result = self.run_script('--native-tests', statuses=dict.fromkeys(TOOLKIT_PACKAGES, 'absent'),
                                 missing_modules=TOOLKIT_MODULES)
        self.assert_packages(native=True)
        self.assert_checked(result)

    def test_toolkit_missing_packages_preserve_both_flags(self):
        result = self.run_script('--native-tests', '--toolkit-tests',
                                 statuses={'qtbase5-dev': 'absent'})
        self.assertEqual(result.returncode, 1)
        self.assert_packages(native=True, toolkit=True)
        self.assert_missing(result, ['qtbase5-dev'])
        self.assertIn('--install --native-tests --toolkit-tests', result.stdout + result.stderr)

    def test_toolkit_missing_xres_package_fails_without_installing(self):
        result = self.run_script('--check', '--native-tests', '--toolkit-tests',
                                 statuses={'libxres-dev': 'absent'})
        self.assertEqual(result.returncode, 1)
        self.assert_packages(native=True, toolkit=True)
        self.assert_missing(result, ['libxres-dev'])
        self.assertIn('--install --native-tests --toolkit-tests', result.stdout + result.stderr)
        self.assertEqual(self.calls_for('pkg-config'), [])
        self.assert_no_install()

    def test_toolkit_install_only_missing_development_packages(self):
        for native in (False, True):
            with self.subTest(native=native):
                args = ['--install', '--toolkit-tests'] + (['--native-tests'] if native else [])
                missing = ['qtbase5-dev', 'libxres-dev']
                result = self.run_script(*args, statuses=dict.fromkeys(missing, 'absent'))
                self.assert_packages(native=native, toolkit=True)
                self.assert_install(missing)
                self.assert_checked(result, toolkit=True)

    def test_toolkit_missing_xres_module_fails_without_installing(self):
        result = self.run_script('--toolkit-tests', missing_modules=['xres'])
        self.assert_packages(toolkit=True)
        self.assert_checked(result, ['缺少开发模块：xres'], toolkit=True)
        self.assert_no_install()

    def test_local_qt_kit_and_xres_sysroot_replace_system_dev_packages(self):
        kit = self.make_qt_kit()
        sysroot = self.make_xres_sysroot()
        result = self.run_script('--check', '--toolkit-tests', extra_env={
            'OINK_QT_KIT': str(kit), 'OINK_XRES_SYSROOT': str(sysroot)})
        self.assert_packages(toolkit=True, local_qt=True, local_xres=True)
        self.assert_checked(result, toolkit=True, local_qt=True, local_xres=True)
        self.assertIn('qt-kit', result.stdout)
        self.assertIn('6.12.0', result.stdout)
        self.assertIn('xres-sysroot', result.stdout)
        self.assertIn('1.2.1', result.stdout)
        self.assertEqual((self.root / 'qmake.log').read_text(encoding='utf-8').splitlines(),
                         ['-query QT_VERSION'])
        self.assert_no_install()

    def test_local_qt_kit_still_requires_system_xres_dev(self):
        kit = self.make_qt_kit()
        result = self.run_script('--check', '--toolkit-tests',
                                 extra_env={'OINK_QT_KIT': str(kit)})
        self.assert_packages(toolkit=True, local_qt=True)
        self.assert_checked(result, toolkit=True, local_qt=True)
        self.assertIn('qt-kit', result.stdout)
        self.assertNotIn('xres-sysroot', result.stdout)
        self.assert_no_install()

    def test_local_xres_sysroot_still_requires_qt5_dev(self):
        sysroot = self.make_xres_sysroot()
        result = self.run_script('--check', '--toolkit-tests',
                                 extra_env={'OINK_XRES_SYSROOT': str(sysroot)})
        self.assert_packages(toolkit=True, local_xres=True)
        self.assert_checked(result, toolkit=True, local_xres=True)
        self.assertIn('xres-sysroot', result.stdout)
        self.assertNotIn('qt-kit', result.stdout)
        self.assert_no_install()

    def test_incomplete_local_qt_kit_fails_without_falling_back(self):
        kit = self.root / 'qt-kit'
        (kit / 'bin').mkdir(parents=True)
        qmake = kit / 'bin/qmake'
        qmake.write_text('#!/bin/sh\nprintf "QT_VERSION:6.12.0\\n"\n', encoding='utf-8')
        qmake.chmod(0o700)
        result = self.run_script('--check', '--toolkit-tests',
                                 extra_env={'OINK_QT_KIT': str(kit)})
        self.assertEqual(result.returncode, 1)
        self.assertIn('本地 Qt kit 不完整', result.stderr)
        self.assertNotIn('缺少开发模块：Qt5Widgets', result.stderr)
        self.assertNotIn(SUCCESS, result.stdout)
        self.assert_packages(toolkit=True, local_qt=True)
        self.assert_no_install()

    def test_incomplete_local_xres_sysroot_fails_without_falling_back(self):
        sysroot = self.root / 'xres-sysroot'
        (sysroot / 'usr/include/X11/extensions').mkdir(parents=True)
        (sysroot / 'usr/include/X11/extensions/XRes.h').write_text('', encoding='utf-8')
        result = self.run_script('--check', '--toolkit-tests',
                                 extra_env={'OINK_XRES_SYSROOT': str(sysroot)})
        self.assertEqual(result.returncode, 1)
        self.assertIn('本地 XRes sysroot 不完整', result.stderr)
        self.assertNotIn('缺少开发模块：xres', result.stderr)
        self.assertNotIn(SUCCESS, result.stdout)
        self.assert_packages(toolkit=True, local_xres=True)
        self.assert_no_install()

    def test_toolkit_requires_xres_runtime_package(self):
        result = self.run_script('--check', '--toolkit-tests',
                                 statuses={'libxres1': 'absent'})
        self.assert_missing(result, ['libxres1'])
        self.assert_packages(toolkit=True)
        self.assertEqual(self.calls_for('pkg-config'), [])

    def test_toolkit_checks_widgets_and_app_commands_without_launching(self):
        result = self.run_script('--toolkit-tests')
        self.assert_packages(toolkit=True)
        self.assert_checked(result, toolkit=True)
        self.assert_no_install()

    def test_toolkit_missing_module_and_apps_fail_without_installing_browser(self):
        for name in TOOLKIT_COMMANDS:
            (self.bin / name).unlink()
        result = self.run_script('--install', '--toolkit-tests', missing_modules=['Qt5Widgets'])
        self.assert_packages(toolkit=True)
        self.assert_checked(result, ['缺少开发模块：Qt5Widgets',
                                     *[f'缺少命令：{name}' for name in TOOLKIT_COMMANDS]], toolkit=True)
        self.assert_no_install()

    @unittest.skipIf(hasattr(os, 'geteuid') and os.geteuid() == 0, 'Root does not use sudo')
    def test_sudo_denial_propagates_without_running_apt(self):
        result = self.run_script('--install', statuses={'fuse3': 'absent'}, sudo_status=77)
        self.assertEqual(result.returncode, 77)
        self.assertEqual(self.calls_for('sudo'), [['apt-get', 'update']])
        self.assertEqual(self.calls_for('apt-get'), [])
        self.assertEqual(self.calls_for('pkg-config'), [])
        self.assertNotIn(SUCCESS, result.stdout)


if __name__ == '__main__':
    unittest.main()
