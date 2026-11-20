#!/usr/bin/env python3
"""Build the Qt input toolkit fixture with the pinned project-local Qt6 kit."""
import argparse
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from linux_dnd_qt_build import flags  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]


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
    partial = output.with_suffix('.partial')
    subprocess.run(['c++', '-std=c++17', '-O2', '-Wall', '-Wextra', '-Werror',
                    '-DQT_NO_KEYWORDS', str(ROOT / 'build-support/linux_input_toolkit_qt.cpp'),
                    *flags(['Qt6Widgets'], kit), f'-Wl,-rpath,{kit / "lib"}',
                    '-o', str(partial)], check=True, timeout=120)
    partial.replace(output)
    print(f'input_toolkit_qt_build=PASS qt={version} output={output} '
          f'mtime_ns={output.stat().st_mtime_ns}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kit', type=Path, default=ROOT / '.tools/Qt/6.12.0/gcc_64')
    parser.add_argument('--output', type=Path,
                        default=ROOT / '.release-tools/linux-probes/input-toolkit-qt')
    args = parser.parse_args()
    build(args.kit, args.output.resolve())


if __name__ == '__main__':
    main()
