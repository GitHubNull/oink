#!/usr/bin/env python3
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import sys
import tempfile

from linux_deps_archive import ArchiveError, SafeExtractor

ROOT = Path(__file__).resolve().parents[1]
SOURCES = Path(__file__).with_name('linux_deps_sources.json')
MANIFEST = '.oink-dependency.json'
TARGETS = {'qt': Path('Qt/6.12.0/gcc_64'), 'sysroot': Path('sysroot')}


class DependencyError(RuntimeError):
    pass


def require(condition, reason):
    if not condition:
        raise DependencyError(reason)


def file_sha256(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def verify_archive(path, source):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), 'archive must be a regular file')
    require(path.stat().st_size == source['size'], 'archive size mismatch')
    require(file_sha256(path) == source['sha256'], 'archive SHA256 mismatch')


def project_path(path):
    path = Path(os.path.abspath(path))
    require(path.is_relative_to(ROOT) and path != ROOT, 'destination must be inside the project')
    for part in [path, *path.parents]:
        require(not part.is_symlink(), 'symlink in destination path')
        if part == ROOT:
            break
    require(not path.exists() or path.is_dir(), 'destination is not a directory')
    return path


def fingerprint(sources):
    return hashlib.sha256(json.dumps(sources, sort_keys=True).encode()).hexdigest()


def tree_manifest(root):
    files = {}
    for directory, folders, names in os.walk(root, followlinks=False):
        for name in sorted(folders + names):
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if relative == MANIFEST:
                continue
            info = path.lstat()
            entry = {'mode': stat.S_IMODE(info.st_mode)}
            if stat.S_ISLNK(info.st_mode):
                require(path.resolve().is_relative_to(root) and path.exists(), 'unsafe installed link')
                entry.update(kind='symlink', target=os.readlink(path))
            elif stat.S_ISDIR(info.st_mode):
                entry.update(kind='directory')
            else:
                require(stat.S_ISREG(info.st_mode), 'non-regular installed file')
                entry.update(kind='file', size=info.st_size, sha256=file_sha256(path))
            files[relative] = entry
    return files


def check_component(target, sources):
    require(target.is_dir() and not target.is_symlink(), 'missing component directory')
    marker = target / MANIFEST
    require(marker.is_file() and not marker.is_symlink() and marker.stat().st_size <= 4 * 1024 * 1024,
            'existing component has no bounded installation manifest; use a new destination')
    try:
        installed = json.loads(marker.read_text())
    except (ValueError, UnicodeError) as error:
        raise DependencyError('invalid component installation manifest') from error
    require(isinstance(installed, dict), 'component installation manifest must be an object')
    require(installed.get('schema') == 1 and installed.get('sources') == fingerprint(sources),
            'component source manifest mismatch')
    require(installed.get('files') == tree_manifest(target), 'installed component contents changed')


@contextmanager
def install_lock(destination):
    fd = os.open(destination / '.oink-fetch.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        require(stat.S_ISREG(os.fstat(fd).st_mode), 'invalid install lock')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise DependencyError('another dependency installation is active') from error
        yield
    finally:
        os.close(fd)


def download(source, cache):
    target = cache / f"{source['id']}.{source['format']}"
    if target.exists() or target.is_symlink():
        verify_archive(target, source)
        return target
    fd, partial = tempfile.mkstemp(prefix=f".{source['id']}-", dir=cache)
    os.close(fd)
    partial = Path(partial)
    try:
        env = {key: value for key, value in os.environ.items()
               if key in ('PATH', 'HOME', 'http_proxy', 'https_proxy', 'no_proxy',
                          'HTTP_PROXY', 'HTTPS_PROXY', 'NO_PROXY')}
        result = subprocess.run(['curl', '--disable', '--fail', '--silent', '--show-error',
                                 '--location', '--max-redirs', '5', '--proto', '=https',
                                 '--proto-redir', '=https', '--connect-timeout', '20',
                                 '--max-time', '300', '--max-filesize', str(source['size']),
                                 '--output', str(partial), source['url']], env=env,
                                capture_output=True, timeout=310)
        require(result.returncode == 0, f"download failed for {source['id']}: curl exit {result.returncode}")
        verify_archive(partial, source)
        require(not target.exists(), 'download destination appeared during installation')
        partial.rename(target)
        return target
    finally:
        partial.unlink(missing_ok=True)


def validate_layout(component, root):
    if component == 'qt':
        required = ('bin/qmake', 'lib/pkgconfig/Qt6Widgets.pc',
                    'lib/cmake/Qt6Widgets/Qt6WidgetsConfig.cmake', 'plugins/platforms/libqxcb.so')
        require(all((root / name).is_file() for name in required), 'incomplete pinned Qt kit')
        environment = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'}
        for key, expected in (('QT_VERSION', '6.12.0'), ('QT_INSTALL_PREFIX', str(root))):
            value = subprocess.check_output([str(root / 'bin/qmake'), '-query', key],
                                            env=environment, text=True, timeout=10).strip()
            require(value == expected, f'pinned Qt qmake {key} mismatch')
    else:
        required = ('usr/include/X11/extensions/XRes.h',
                    'usr/lib/x86_64-linux-gnu/pkgconfig/xres.pc',
                    'usr/lib/x86_64-linux-gnu/libXRes.so')
        require(all((root / name).is_file() for name in required), 'incomplete XRes sysroot')
        require((root / required[-1]).resolve().is_relative_to(root), 'XRes runtime escapes sysroot')


def install_component(component, destination, sources, cache):
    target = destination / TARGETS[component]
    project_path(target)
    if target.exists():
        check_component(target, sources)
        return target
    target.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=f'.{component}-', dir=target.parent))
    try:
        extractors = {}
        for source in sources:
            archive = download(source, cache)
            root = stage / source['subdir']
            root.mkdir(mode=0o700, parents=True, exist_ok=True)
            if root not in extractors:
                extractors[root] = SafeExtractor(root)
            extractor = extractors[root]
            if source['format'] == 'deb':
                extractor.extract_deb(archive, stage)
            else:
                extractor.extract(archive, source['format'])
        for extractor in extractors.values():
            extractor.finish()
        validate_layout(component, stage)
        manifest = {'schema': 1, 'sources': fingerprint(sources), 'files': tree_manifest(stage)}
        with (stage / MANIFEST).open('x') as output:
            json.dump(manifest, output, sort_keys=True)
            output.write('\n')
        require(not target.exists() and not target.is_symlink(), 'component destination appeared')
        stage.rename(target)
        check_component(target, sources)
        return target
    finally:
        if stage.exists():
            shutil.rmtree(stage)


def prepare(destination, check_only=False):
    destination = project_path(destination)
    document = json.loads(SOURCES.read_text())
    require(document['schema'] == 1 and document['platform'] == 'linux-x86_64', 'invalid source manifest')
    sources = document['archives']
    grouped = {component: [row for row in sources if row['component'] == component]
               for component in TARGETS}
    if check_only:
        for component, archives in grouped.items():
            target = project_path(destination / TARGETS[component])
            check_component(target, archives)
            print(f'dependency_check=PASS component={component} path={target}', flush=True)
        return
    destination.mkdir(parents=True, mode=0o700, exist_ok=True)
    with install_lock(destination):
        for component, archives in grouped.items():
            target = project_path(destination / TARGETS[component])
            if target.exists():
                check_component(target, archives)
        cache = project_path(destination / '.downloads')
        cache.mkdir(mode=0o700, exist_ok=True)
        for component, archives in grouped.items():
            target = install_component(component, destination, archives, cache)
            print(f'dependency_install=PASS component={component} path={target}', flush=True)


def main():
    parser = argparse.ArgumentParser(description='获取固定 Qt 6.12.0/XRes 探针依赖，不安装系统包；已有目录不覆盖。')
    parser.add_argument('--destination', type=Path, default=ROOT / '.tools',
                        help='项目内目标根；已有拷贝请改用新的空目录')
    parser.add_argument('--check-only', action='store_true', help='只复核清单和全部文件，不写入或下载')
    parser.add_argument('--inside-scope', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    require(sys.platform == 'linux' and platform.machine() == 'x86_64'
            and os.getuid() == os.geteuid() != 0, 'ordinary Linux x86_64 user required')
    if not args.inside_scope:
        return subprocess.call(['systemd-run', '--user', '--scope', '--quiet', '-p', 'MemoryMax=4G',
                                '-p', 'CPUQuota=200%', '-p', 'TasksMax=256', 'timeout', '900',
                                sys.executable, '-B', str(Path(__file__).resolve()), '--inside-scope',
                                *sys.argv[1:]])
    spec = importlib.util.spec_from_file_location('native_runner', Path(__file__).with_name('linux-native-test.py'))
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    runner.require_resource_limits()
    prepare(args.destination, args.check_only)
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (DependencyError, ArchiveError, OSError, subprocess.SubprocessError) as error:
        print(f'dependency_install=FAIL reason={error}', file=sys.stderr)
        sys.exit(1)
