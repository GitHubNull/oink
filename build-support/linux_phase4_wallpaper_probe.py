#!/usr/bin/env python3
"""Phase 4 wallpaper capability probe. Runs only inside the private Xephyr harness.

Never touches the real session: GSettings uses the keyfile backend under the
harness' private XDG_CONFIG_HOME, so no dconf/Cinnamon wallpaper is modified.
"""
import argparse
import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

os.environ['GSETTINGS_BACKEND'] = 'keyfile'

import gi  # noqa: E402

gi.require_version('GdkPixbuf', '2.0')
from gi.repository import GdkPixbuf, Gio, GLib  # noqa: E402

SCHEMA_ID = 'org.cinnamon.desktop.background'
LONG_EDGE = 480
TARGET_BYTES = 40 * 1024
OPTIONS = {'none', 'wallpaper', 'centered', 'scaled', 'stretched', 'zoom', 'spanned'}
failures = 0
evidence = {}


def check(condition, name, detail=''):
    global failures
    state = 'ok' if condition else 'fail'
    print(f'{name}={state}{(" " + detail) if detail else ""}')
    evidence[name] = condition
    if not condition:
        failures += 1
    return condition


def patterned_pixbuf(width, height):
    rowstride = width * 3
    data = bytearray(rowstride * height)
    for y in range(height):
        base = y * rowstride
        for x in range(width):
            offset = base + x * 3
            data[offset] = (x * 255) // (width - 1)
            data[offset + 1] = (y * 255) // (height - 1)
            data[offset + 2] = 40 + (x * y) % 200
    return GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(bytes(data)),
                                           GdkPixbuf.Colorspace.RGB, False, 8,
                                           width, height, rowstride)


def jpeg_ladder(pixbuf, directory):
    chosen = None
    for quality in (85, 75, 65, 55, 45, 35, 25):
        path = directory / f'thumbnail-q{quality}.jpg'
        pixbuf.savev(str(path), 'jpeg', ['quality'], [str(quality)])
        size = path.stat().st_size
        print(f'wallpaper_jpeg_quality q={quality} bytes={size}')
        if size <= TARGET_BYTES:
            chosen = (quality, path, size)
            break
    for stale in directory.glob('thumbnail-q*.jpg'):
        if chosen is None or stale != chosen[1]:
            stale.unlink()
    return chosen


def settings_checks(directory):
    source = Gio.SettingsSchemaSource.get_default()
    schema = source.lookup(SCHEMA_ID, True)
    if not check(schema is not None, 'wallpaper_schema', f'id={SCHEMA_ID}'):
        return
    keys = {key: schema.get_key(key) for key in ('picture-uri', 'picture-options', 'primary-color')}
    check(all(value is not None for value in keys.values()), 'wallpaper_keys',
          'picture-uri picture-options primary-color')
    settings = Gio.Settings.new(SCHEMA_ID)
    options = settings.get_string('picture-options')
    check(options in OPTIONS, 'wallpaper_options_read', f'value={options}')
    check(settings.is_writable('picture-uri'), 'wallpaper_settings_writable', 'backend=keyfile')

    seen = []

    def on_changed(setting, key):
        seen.append((key, setting.get_string(key)))

    handler = settings.connect('changed::picture-uri', on_changed)
    uri = (directory / 'gsettings-probe.png').as_uri()
    settings.set_string('picture-uri', uri)
    deadline = time.monotonic() + 2.0
    while not seen and time.monotonic() < deadline:
        GLib.MainContext.default().iteration(False)
    check(bool(seen) and seen[0] == ('picture-uri', uri), 'wallpaper_changed_signal',
          f'events={len(seen)}')
    check(settings.get_string('picture-uri') == uri, 'wallpaper_uri_roundtrip')
    settings.disconnect(handler)
    settings.reset('picture-uri')

    settings.set_string('picture-options', 'none')
    check(settings.get_string('picture-options') == 'none', 'wallpaper_options_none')
    settings.reset('picture-options')
    evidence['wallpaper_options_default'] = options


def image_checks(directory):
    source = patterned_pixbuf(640, 480)
    png = directory / 'source.png'
    source.savev(str(png), 'png', [], [])
    check(png.stat().st_size > 0, 'wallpaper_png_encode', f'bytes={png.stat().st_size}')
    decoded = GdkPixbuf.Pixbuf.new_from_file(str(png))
    check((decoded.get_width(), decoded.get_height()) == (640, 480), 'wallpaper_png_decode',
          f'{decoded.get_width()}x{decoded.get_height()}')

    factor = LONG_EDGE / max(decoded.get_width(), decoded.get_height())
    width = max(1, round(decoded.get_width() * factor))
    height = max(1, round(decoded.get_height() * factor))
    scaled = decoded.scale_simple(width, height, GdkPixbuf.InterpType.BILINEAR)
    check((width, height) == (480, 360), 'wallpaper_thumbnail_size', f'{width}x{height}')
    scaled.savev(str(directory / 'thumbnail.png'), 'png', [], [])

    chosen = jpeg_ladder(scaled, directory)
    check(chosen is not None, 'wallpaper_jpeg_target',
          f'target={TARGET_BYTES} bytes={chosen[2]} quality={chosen[0]}' if chosen else '')
    if chosen is not None:
        evidence['jpeg_quality'] = chosen[0]
        evidence['jpeg_bytes'] = chosen[2]

    missing_ok = False
    try:
        GdkPixbuf.Pixbuf.new_from_file(str(directory / 'missing.png'))
    except GLib.Error:
        missing_ok = True
    check(missing_ok, 'wallpaper_missing_decode', 'error=treated_as_no_wallpaper')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', default='')
    arguments = parser.parse_args()
    os.makedirs(os.environ.get('XDG_CONFIG_HOME', '/tmp'), exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='oink-wallpaper-') as name:
        directory = Path(name)
        settings_checks(directory)
        image_checks(directory)
        if arguments.out:
            output = Path(arguments.out)
            output.mkdir(parents=True, exist_ok=True)
            for item in ('source.png', 'thumbnail.png'):
                shutil.copy2(directory / item, output / item)
            chosen = sorted(directory.glob('thumbnail-q*.jpg'))
            if chosen:
                shutil.copy2(chosen[0], output / chosen[0].name)
            (output / 'wallpaper_probe.json').write_text(
                json.dumps(evidence, indent=2, sort_keys=True) + '\n')
    print('phase4_wallpaper_probe=' + json.dumps(evidence, sort_keys=True))
    print(f'phase4_wallpaper_probe result={"pass" if failures == 0 else "fail"} failures={failures}')
    return 0 if failures == 0 else 1


if __name__ == '__main__':
    sys.exit(main())
