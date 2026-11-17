#!/usr/bin/env python3
"""Build the Qt6 role with a project-local kit; run inside a bounded resource scope."""
import argparse
import os
from pathlib import Path
import shlex
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def flags(modules, kit=None):
    env = dict(os.environ)
    command = ['pkg-config']
    if kit:
        env['PKG_CONFIG_PATH'] = str(kit / 'lib/pkgconfig')
        command.append(f'--define-variable=prefix={kit}')
    command += ['--cflags', '--libs', *modules]
    return shlex.split(subprocess.check_output(command, env=env, text=True, timeout=10))


def build(kit, output):
    kit = kit.resolve(strict=True)
    for relative in ('bin/qmake', 'lib/pkgconfig/Qt6Widgets.pc', 'plugins/platforms/libqxcb.so'):
        if not (kit / relative).is_file():
            raise RuntimeError(f'incomplete local Qt6 kit: {relative}')
    version = subprocess.check_output([str(kit / 'bin/qmake'), '-query', 'QT_VERSION'],
                                      text=True, timeout=10).strip()
    if version != '6.12.0':
        raise RuntimeError(f'expected pinned Qt 6.12.0, got {version}')
    if not output.parent.is_dir():
        raise RuntimeError('output directory must already exist')
    source = ROOT / 'build-support'
    object_file = output.with_suffix('.native.o')
    partial = output.with_suffix('.partial')
    native_flags = flags(['x11', 'xi', 'xtst'])
    subprocess.run(['cc', '-std=c11', '-D_POSIX_C_SOURCE=200809L', '-O2', '-Wall', '-Wextra',
                    '-Werror', '-c', str(source / 'linux_dnd_native.c'), '-o', str(object_file),
                    *native_flags], check=True, timeout=60)
    subprocess.run(['c++', '-std=c++17', '-O2', '-Wall', '-Wextra', '-Werror', '-fPIC',
                    '-DQT_NO_KEYWORDS', str(source / 'linux_dnd_qt_probe.cpp'),
                    str(source / 'linux_dnd_qt_control.cpp'), str(object_file),
                    *flags(['Qt6Widgets'], kit), *native_flags,
                    f'-Wl,-rpath,{kit / "lib"}', '-o', str(partial)], check=True, timeout=120)
    partial.replace(output)
    print(f'qt_role_build=PASS qt={version} output={output} mtime_ns={output.stat().st_mtime_ns}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kit', type=Path, default=ROOT / '.tools/Qt/6.12.0/gcc_64')
    parser.add_argument('--output', type=Path, default=ROOT / '.release-tools/linux-probes/dnd-qt')
    args = parser.parse_args()
    build(args.kit, args.output.resolve())


if __name__ == '__main__':
    main()
