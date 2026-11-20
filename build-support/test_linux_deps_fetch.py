"""Offline lifecycle tests for the fixed-source dependency installer."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import linux_deps_fetch as fetch
from test_linux_deps_archive import deb_bytes, file, link, tar_bytes


class DownloadTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.cache = Path(tmp.name)
        self.source = {'id': 'sample', 'format': '7z', 'size': 4,
                       'sha256': hashlib.sha256(b'good').hexdigest(),
                       'url': 'https://download.qt.io/test-fixture'}

    def test_bad_sha_is_rejected_even_when_size_matches(self):
        path = self.cache / 'archive'
        path.write_bytes(b'evil')
        with self.assertRaises(fetch.DependencyError):
            fetch.verify_archive(path, self.source)

    def test_bad_size_and_symlinks_are_rejected(self):
        path = self.cache / 'archive'
        path.write_bytes(b'good!')
        with self.assertRaises(fetch.DependencyError):
            fetch.verify_archive(path, self.source)
        path.write_bytes(b'good')
        alias = self.cache / 'alias'
        alias.symlink_to(path)
        with self.assertRaises(fetch.DependencyError):
            fetch.verify_archive(alias, self.source)

    def test_matching_cache_is_read_only_and_bad_cache_is_not_replaced(self):
        path = self.cache / 'sample.7z'
        path.write_bytes(b'good')
        before = path.stat().st_mtime_ns
        with patch.object(fetch.subprocess, 'run') as run:
            self.assertEqual(fetch.download(self.source, self.cache), path)
            self.assertEqual(path.stat().st_mtime_ns, before)
            path.write_bytes(b'evil')
            with self.assertRaises(fetch.DependencyError):
                fetch.download(self.source, self.cache)
            run.assert_not_called()
        self.assertEqual(path.read_bytes(), b'evil')

    def fake_curl(self, body=b'good', code=0, error=None):
        def run(command, **kwargs):
            self.assertEqual(command[0:2], ['curl', '--disable'])
            for flag in ('--proto', '--proto-redir'):
                self.assertEqual(command[command.index(flag) + 1], '=https')
            self.assertEqual(command[command.index('--max-filesize') + 1], '4')
            self.assertLessEqual(kwargs['timeout'], 310)
            self.assertNotIn('CURL_CA_BUNDLE', kwargs['env'])
            Path(command[command.index('--output') + 1]).write_bytes(body)
            if error:
                raise error
            return subprocess.CompletedProcess(command, code, b'', b'private diagnostic')
        return run

    def test_download_verifies_before_publishing_and_discards_only_own_partial(self):
        kept = self.cache / '.old-partial'
        kept.write_bytes(b'keep')
        with patch.object(fetch.subprocess, 'run', side_effect=self.fake_curl()):
            path = fetch.download(self.source, self.cache)
        self.assertEqual(path.read_bytes(), b'good')
        self.assertEqual(set(p.name for p in self.cache.iterdir()), {'.old-partial', 'sample.7z'})

    def test_failed_corrupt_and_interrupted_downloads_never_publish(self):
        cases = [(b'evil', 0, None), (b'big!!', 0, None), (b'good', 22, None),
                 (b'part', 0, subprocess.TimeoutExpired('curl', 310)),
                 (b'part', 0, KeyboardInterrupt())]
        for body, code, error in cases:
            with self.subTest(code=code, error=type(error).__name__, body=body):
                with patch.object(fetch.subprocess, 'run', side_effect=self.fake_curl(body, code, error)):
                    with self.assertRaises((fetch.DependencyError, subprocess.TimeoutExpired,
                                            KeyboardInterrupt)):
                        fetch.download(self.source, self.cache)
                self.assertEqual(list(self.cache.iterdir()), [])


class InstallationTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.destination = self.root / 'tools'
        self.destination.mkdir()
        self.cache = self.destination / '.downloads'
        self.cache.mkdir()
        root_patch = patch.object(fetch, 'ROOT', self.root)
        root_patch.start()
        self.addCleanup(root_patch.stop)
        members = [file('usr/include/X11/extensions/XRes.h'),
                   file('usr/lib/x86_64-linux-gnu/pkgconfig/xres.pc'),
                   link('usr/lib/x86_64-linux-gnu/libXRes.so', 'libXRes.so.1'),
                   file('usr/lib/x86_64-linux-gnu/libXRes.so.1')]
        body = deb_bytes([('debian-binary', b'2.0\n'), ('data.tar', tar_bytes(members))])
        self.source = {'id': 'xres', 'format': 'deb', 'component': 'sysroot', 'subdir': '.',
                       'url': 'https://archive.ubuntu.com/test-fixture', 'size': len(body),
                       'sha256': hashlib.sha256(body).hexdigest()}
        (self.cache / 'xres.deb').write_bytes(body)
        self.target = self.destination / 'sysroot'

    def install(self):
        return fetch.install_component('sysroot', self.destination, [self.source], self.cache)

    def test_publish_has_complete_manifest_and_internal_runtime_link(self):
        self.assertEqual(self.install(), self.target)
        fetch.check_component(self.target, [self.source])
        runtime = self.target / 'usr/lib/x86_64-linux-gnu/libXRes.so'
        self.assertTrue(runtime.resolve().is_relative_to(self.target))
        self.assertEqual(runtime.read_bytes(), b'content')
        self.assertFalse(list(self.destination.glob('.sysroot-*')))

    def test_rerun_rechecks_without_download_extract_or_write(self):
        self.install()
        marker = self.target / fetch.MANIFEST
        before = marker.stat().st_mtime_ns
        with patch.object(fetch, 'download') as download, patch.object(fetch, 'SafeExtractor') as extract:
            self.assertEqual(self.install(), self.target)
            download.assert_not_called()
            extract.assert_not_called()
        self.assertEqual(marker.stat().st_mtime_ns, before)

    def test_incomplete_or_foreign_directory_is_preserved(self):
        self.target.mkdir()
        kept = self.target / 'keep'
        kept.write_bytes(b'original')
        with patch.object(fetch, 'download') as download:
            with self.assertRaises(fetch.DependencyError):
                self.install()
            download.assert_not_called()
        self.assertEqual(kept.read_bytes(), b'original')

    def test_manifest_content_mode_source_and_extra_file_tampering_fail(self):
        self.install()
        marker = self.target / fetch.MANIFEST
        original = marker.read_bytes()
        for body in (b'not-json', b'[]', b'null', b'{}'):
            with self.subTest(body=body):
                marker.write_bytes(body)
                with self.assertRaises(fetch.DependencyError):
                    self.install()
        marker.write_bytes(original)
        changed_source = dict(self.source, sha256='0' * 64)
        with self.assertRaises(fetch.DependencyError):
            fetch.check_component(self.target, [changed_source])
        header = self.target / 'usr/include/X11/extensions/XRes.h'
        header.write_bytes(b'changed')
        with self.assertRaises(fetch.DependencyError):
            self.install()
        header.write_bytes(b'content')
        header.chmod(0o600)
        with self.assertRaises(fetch.DependencyError):
            self.install()
        header.chmod(0o644)
        extra = self.target / 'extra'
        extra.write_bytes(b'extra')
        with self.assertRaises(fetch.DependencyError):
            self.install()

    def test_failure_or_interrupt_before_publish_cleans_own_stage_and_allows_retry(self):
        foreign = self.destination / '.sysroot-foreign'
        foreign.mkdir()
        kept = foreign / 'keep'
        kept.write_bytes(b'untouched')
        for error in (fetch.DependencyError('layout'), KeyboardInterrupt()):
            with self.subTest(error=type(error).__name__):
                with patch.object(fetch, 'validate_layout', side_effect=error):
                    with self.assertRaises(type(error)):
                        self.install()
                self.assertFalse(self.target.exists())
                self.assertEqual(list(self.destination.glob('.sysroot-*')), [foreign])
        self.install()
        self.assertEqual(kept.read_bytes(), b'untouched')

    def test_new_conflicting_target_is_not_replaced(self):
        original = fetch.validate_layout
        def conflict(component, stage):
            original(component, stage)
            self.target.mkdir()
            (self.target / 'keep').write_bytes(b'concurrent')
        with patch.object(fetch, 'validate_layout', side_effect=conflict):
            with self.assertRaises(fetch.DependencyError):
                self.install()
        self.assertEqual((self.target / 'keep').read_bytes(), b'concurrent')
        self.assertFalse(list(self.destination.glob('.sysroot-*')))

    def test_check_only_does_not_create_or_modify_anything(self):
        manifest = self.root / 'sources.json'
        manifest.write_text(json.dumps({'schema': 1, 'platform': 'linux-x86_64',
                                        'archives': [self.source]}))
        with patch.object(fetch, 'SOURCES', manifest), patch.object(fetch, 'TARGETS', {'sysroot': Path('sysroot')}):
            absent = self.root / 'absent'
            with self.assertRaises(fetch.DependencyError):
                fetch.prepare(absent, check_only=True)
            self.assertFalse(absent.exists())
            self.install()
            before = {str(p): p.stat().st_mtime_ns for p in self.destination.rglob('*')}
            with patch.object(fetch, 'download') as download, patch('sys.stdout', new=io.StringIO()):
                fetch.prepare(self.destination, check_only=True)
                download.assert_not_called()
            self.assertEqual(before, {str(p): p.stat().st_mtime_ns for p in self.destination.rglob('*')})
            self.assertFalse((self.destination / '.oink-fetch.lock').exists())

    def test_project_boundary_and_symlink_parents_are_rejected(self):
        alias = self.root / 'alias'
        alias.symlink_to(self.destination, target_is_directory=True)
        for path in (self.root, self.root.parent / 'outside', alias / 'child'):
            with self.subTest(path=path), self.assertRaises(fetch.DependencyError):
                fetch.project_path(path)
        self.target.symlink_to(self.cache, target_is_directory=True)
        with self.assertRaises(fetch.DependencyError):
            self.install()

    def test_lock_excludes_another_process_and_can_be_reacquired(self):
        script = ('import sys; from pathlib import Path; import linux_deps_fetch as f; '
                  'ctx=f.install_lock(Path(sys.argv[1])); ctx.__enter__()')
        env = dict(os.environ, PYTHONPATH=str(Path(fetch.__file__).parent))
        with fetch.install_lock(self.destination):
            result = subprocess.run([os.sys.executable, '-B', '-c', script, str(self.destination)],
                                    env=env, capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'another dependency installation is active', result.stderr)
        with fetch.install_lock(self.destination):
            pass

    def test_lock_does_not_follow_symlink(self):
        kept = self.root / 'kept'
        kept.write_bytes(b'keep')
        (self.destination / '.oink-fetch.lock').symlink_to(kept)
        with self.assertRaises(OSError):
            with fetch.install_lock(self.destination):
                self.fail('followed a lock symlink')
        self.assertEqual(kept.read_bytes(), b'keep')


if __name__ == '__main__':
    unittest.main()
