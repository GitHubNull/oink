#!/usr/bin/env python3
"""Real-application XDND text acceptance inside the private native runner.

Chrome/Firefox/Nemo run as brand-new verified processes with fresh profiles on
the private display. Text rows use the independent XRes+epoch+generation app
identity branch; no fixture-only checks are relaxed. The page is served only
from 127.0.0.1 and reports event trust plus payload hashes, never content.
"""
import argparse
import json
import os
from pathlib import Path
import random
import time
import traceback

import linux_app_harness as harness
from linux_app_page import PageServer, begin_app_drag, digest_words, page_matches
from linux_app_x11 import X11
from linux_dnd_protocol import (
    ARM_APP, DISARM, DOWN, FOCUS, FREE, MOVE, STATUS, UP, Failure, Role, check,
    isolated_environment,
)
from linux_dnd_scenarios import clipboard, evidence, exact, quiet, wait

SESSION_EPOCH_ATOM = '_OINK_TEST_SESSION_EPOCH'
APP_GENERATION_ATOM = '_OINK_TEST_APP_GENERATION'
ROWS = []


def row(name, result, **facts):
    ROWS.append({'row': name, 'result': result, **facts})
    print(json.dumps({'matrix_row': name, 'result': result, **facts}, sort_keys=True), flush=True)


def session_setup(environment):
    env = isolated_environment(environment)
    x11 = X11(env)
    epoch = random.SystemRandom().randrange(1, 2**31)
    x11.set_cardinal(x11.root, SESSION_EPOCH_ATOM, epoch)
    server = PageServer(Path(__file__).with_name('linux_dnd_app_page.html'))
    return env, x11, epoch, server


def scope_tasks():
    """Count threads in this cgroup v2 scope via /proc (pids.current is not delegated)."""
    mine = next(line[3:] for line in Path('/proc/self/cgroup').read_text().splitlines()
                if line.startswith('0::'))
    total = 0
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            cg = next(line[3:] for line in (entry / 'cgroup').read_text().splitlines()
                      if line.startswith('0::'))
            if cg != mine:
                continue
            for line in (entry / 'status').read_text().splitlines():
                if line.startswith('Threads:'):
                    total += int(line.split()[1])
                    break
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    return total


TASKS_MAX = 1024


def wait_task_headroom(minimum=32, timeout=120):
    """Roles need GLib threads; wait out app startup spikes inside the bounded scope."""
    deadline = time.monotonic() + timeout
    while True:
        total = scope_tasks()
        if total <= TASKS_MAX - minimum:
            return total
        check(time.monotonic() < deadline, f'cgroup tasks {total}/{TASKS_MAX}: headroom deadline')
        time.sleep(0.5)


def settled_coords(server):
    previous = None
    for _ in range(50):
        locate = server.latest('locate')
        current = (locate['source']['x'], locate['source']['y'],
                   locate['target']['x'], locate['target']['y'])
        if current == previous:
            return locate
        previous = current
        time.sleep(0.2)
    raise Failure('page coordinates never settled (window still moving?)')


def calibrate(server, x11, locate):
    """window.screenX/Y is unreliable under the private WM; learn the real
    content offset from the page's own trusted mouse coordinates."""
    offset = [0, 0]
    for _ in range(4):
        seen = len(server.snapshot())
        sx = locate['source']['x'] + offset[0]
        sy = locate['source']['y'] + offset[1]
        x11.motion(sx, sy)
        x11.button(True)
        time.sleep(0.15)
        x11.button(False)
        deadline = time.monotonic() + 3
        while len(server.snapshot()) <= seen and time.monotonic() < deadline:
            time.sleep(0.05)
        probe = server.latest('probe-mousedown')
        offset = [sx - probe['x'], sy - probe['y']]
        if (probe['x'], probe['y']) == (locate['source']['x'], locate['source']['y']):
            return offset
    raise Failure(f'pointer calibration did not converge; offset={offset}')


def shifted(locate, offset, key):
    return (locate[key]['x'] + offset[0], locate[key]['y'] + offset[1])


def browser_source_text(roles, kind, wm_class, environment):
    name = f'{kind}-source-text-drop'
    env, x11, epoch, server = session_setup(environment)
    app = None
    try:
        app = harness.launch(kind, server.url, environment)
        window = x11.wait_window(wm_class)[0]
        ready = server.wait_report('ready')
        check(ready['trusted'] is True and ready['payload_length'] > 32, 'page ready evidence')
        locate = settled_coords(server)
        wait_task_headroom()  # Firefox startup spikes past 240 threads; page POSTs need threads too.
        offset = calibrate(server, x11, locate)
        pid = None
        roles_used = []
        sentinel = Role(roles, 'sentinel', env); roles_used.append(sentinel); sentinel.ready()
        target = Role(roles, 'target', env); roles_used.append(target); target.ready()
        clipboard(sentinel, target)
        source_xid = begin_app_drag(x11, app, server, window, shifted(locate, offset, 'source'))
        server.wait_report('dragstart')
        pid = x11.xres_pid(window['xid'])
        exe = app.verify_window_pid(pid)
        generation = 2
        x11.set_cardinal(source_xid, APP_GENERATION_ATOM, generation)
        exact(target.command(ARM_APP, source_xid, pid, epoch, generation=generation),
              gen=generation, armed=1, app=1)
        target.command(STATUS)  # Refresh geometry after the private WM settles.
        # Sample the path repeatedly: the app may need several motion passes
        # before it forwards XdndEnter to the armed target.
        tx, ty = target.last['x'], target.last['y']
        sx, sy = shifted(locate, offset, 'source')
        # The drag source raises itself to take the press; the target role must
        # be restacked above the fullscreen app or the source's XdndEnter never
        # reaches it (metacity stacking race, observed as visits staying 0).
        x11.raise_window(target.last['xid'])
        time.sleep(0.2)
        accepted = None
        deadline = time.monotonic() + 15
        steps = 4
        while time.monotonic() < deadline:
            x11.drag_path((sx, sy), (tx, ty), steps=min(steps, 24), press=True)
            # Settle motions: Xephyr can drop the XdndPosition burst after a
            # fast path, so the app never forwards the drag to the target.
            for wiggle in range(4):
                x11.motion(tx + (10 if wiggle % 2 else -10), ty)
                time.sleep(0.1)
            x11.motion(tx, ty)
            report = target.command(STATUS)
            if report['visits'] >= 1 and report['inside'] == 1:
                accepted = report
                break
            steps += 4
        check(accepted is not None,
              f'app source accepted by identity; last={target.last}')
        x11.release()  # the app sends XdndDrop on button release; the read follows
        received = wait(target, lambda r: r['received'] == 1, 'target read real app selection')
        exact(received, drop=1, stale=0, app=1)
        page_matches(received, ready)
        ended = server.wait_report('dragend')
        check(ended['trusted'] is True and ended['dropEffect'] == 'copy', 'browser saw real COPY finish')
        target.command(FREE)
        clipboard(sentinel, target)
        evidence(name, sentinel=sentinel, target=target)
        row(name, 'PASS', app=kind, pid=pid, exe=exe, epoch=epoch,
            payload_length=ready['payload_length'], payload_sha256=ready['payload_sha256'])
        return True
    except Exception as error:
        row(name, 'FAIL', reason=str(error))
        raise
    finally:
        for role in locals().get('roles_used', []):
            role.close()
        if app:
            app.close()
        server.close()
        x11.close()


def browser_source_synthetic_blocked(roles, kind, wm_class, environment, modes):
    """Chromium never initiates an X11 drag from XTEST input: trusted press and
    motion reach the page but no dragstart/XdndSelection owner appears. Verify
    that limitation with a bounded probe (tripwire: a real owner fails loudly so
    the row must be upgraded to a full source test), then record BLOCKED; the
    physical-mouse counterpart is wave F user verification."""
    for mode in modes:
        name = f'{kind}-source-' + ('text-drop' if mode == 'text' else f'reject-{mode}')
        env, x11, epoch, server = session_setup(environment)
        app = None
        try:
            app = harness.launch(kind, server.url, environment)
            window = x11.wait_window(wm_class)[0]
            ready = server.wait_report('ready')
            check(ready['trusted'] is True and ready['payload_length'] > 32, 'page ready evidence')
            locate = settled_coords(server)
            wait_task_headroom()
            offset = calibrate(server, x11, locate)
            sx, sy = shifted(locate, offset, 'source')
            x11.raise_window(window['xid'])
            x11.focus(window['xid'])
            x11.motion(sx, sy)
            x11.button(True)
            time.sleep(0.2)
            for step in range(1, 21):
                x11.motion(sx + 3 * step, sy + step)
                time.sleep(0.02)
            deadline = time.monotonic() + 5
            owner = 0
            while time.monotonic() < deadline:
                owner = x11.selection_owner()
                if owner:
                    break
                time.sleep(0.1)
            x11.button(False)
            kinds = {r['kind'] for r in server.snapshot()}
            check(owner == 0 and 'dragstart' not in kinds,
                  f'{kind} initiated a drag from synthetic input (owner={owner}); upgrade this row to a real source test')
            row(name, 'BLOCKED', app=kind, reason='Chromium ignores XTEST drag initiation on X11; '
                'trusted press+motion reached the page without dragstart or XdndSelection owner; '
                'physical-mouse verification is wave F scope', pid=app.pid)
        finally:
            if app:
                app.close()
            server.close()
            x11.close()


def browser_target_text(roles, kind, wm_class, environment):
    name = f'{kind}-target-text-drop'
    env, x11, epoch, server = session_setup(environment)
    app = None
    try:
        app = harness.launch(kind, server.url, environment)
        window = x11.wait_window(wm_class)[0]
        ready = server.wait_report('ready')
        locate = settled_coords(server)
        wait_task_headroom()  # Firefox startup spikes past 240 threads; page POSTs need threads too.
        offset = calibrate(server, x11, locate)
        roles_used = []
        sentinel = Role(roles, 'sentinel', env); roles_used.append(sentinel); sentinel.ready()
        source = Role(roles, 'source', env); roles_used.append(source); source.ready()
        clipboard(sentinel, source)
        source.command(FOCUS)
        quiet(source, lambda r: r['begin'] == 0, .1)
        source.command(MOVE, source.last['x'], source.last['y'])
        source.command(DOWN)
        wait(source, lambda r: r['press'] == 1, 'real button press')
        source.command(MOVE, source.last['x'] + 60, source.last['y'])
        wait(source, lambda r: r['begin'] == 1, 'native drag begin')
        # Drive the drag through the role's own synthetic input connection; a
        # second XTEST connection is invisible to the role's drag grab.
        tx, ty = shifted(locate, offset, 'target')
        sx, sy = source.last['x'] + 60, source.last['y']
        for step in range(1, 8):
            source.command(MOVE, sx + (tx - sx) * step // 8, sy + (ty - sy) * step // 8)
        # Settle with real motion pairs before release: Xephyr sometimes drops
        # the first XdndPosition burst after a fast teleporting path, and the
        # app then never processes the drop. A user path always has motion.
        for wiggle in range(6):
            source.command(MOVE, tx + (10 if wiggle % 2 else -10), ty)
            time.sleep(0.15)
        source.command(MOVE, tx, ty)
        time.sleep(0.2)
        source.command(UP)  # the app dispatches its drop event on button release
        dropped = server.wait_report('drop')
        expected_hex = ''.join(f'{source.last[f"digest{i}"]:08x}' for i in range(8))
        check(dropped['trusted'] is True and dropped['dropEffect'] == 'copy'
              and dropped['received_length'] == source.last['length']
              and dropped['received_sha256'] == expected_hex,
              'page received the role payload verbatim via real COPY drop')
        wait(source, lambda r: r['end'] == 1, 'source drag end after app drop')
        wait(source, lambda r: r['toolkit_grabbed'] == 0, 'toolkit grab release', seconds=.5)
        report = source.command(STATUS)
        # App targets choose their own selection request count (Chrome asks
        # twice); the integrity evidence is the verbatim page payload above.
        check(1 <= report['get'] <= 4, f'bounded selection service, got {report["get"]}')
        exact(report, begin=1, end=1, received=0, drop=0, deleted=0, press=1,
              down=1, up=1, raw_down=1, raw_up=1, cancelled=0)
        source.command(FREE)
        clipboard(sentinel, source)
        evidence(name, sentinel=sentinel, source=source)
        row(name, 'PASS', app=kind, pid=app.pid,
            payload_length=dropped['received_length'], payload_sha256=dropped['received_sha256'])
        return True
    except Exception as error:
        row(name, 'FAIL', reason=str(error))
        raise
    finally:
        for role in locals().get('roles_used', []):
            role.close()
        if app:
            app.close()
        server.close()
        x11.close()


def browser_source_rejected(roles, kind, wm_class, environment, mode):
    name = f'{kind}-source-reject-{mode}'
    env, x11, epoch, server = session_setup(environment)
    app = None
    try:
        app = harness.launch(kind, server.url, environment)
        window = x11.wait_window(wm_class)[0]
        ready = server.wait_report('ready')
        locate = settled_coords(server)
        wait_task_headroom()  # Firefox startup spikes past 240 threads; page POSTs need threads too.
        offset = calibrate(server, x11, locate)
        roles_used = []
        sentinel = Role(roles, 'sentinel', env); roles_used.append(sentinel); sentinel.ready()
        target = Role(roles, 'target', env); roles_used.append(target); target.ready()
        clipboard(sentinel, target)
        source_xid = begin_app_drag(x11, app, server, window, shifted(locate, offset, 'source'))
        real_pid = x11.xres_pid(window['xid'])
        app.verify_window_pid(real_pid)
        generation = 2
        forged_pid = real_pid + 1 if mode == 'forged-pid' else real_pid
        if mode == 'late-generation':
            x11.set_cardinal(source_xid, APP_GENERATION_ATOM, generation - 1)
        else:
            x11.set_cardinal(source_xid, APP_GENERATION_ATOM, generation)
        exact(target.command(ARM_APP, source_xid, forged_pid, epoch, generation=generation),
              gen=generation, armed=1, app=1)
        x11.drag_path(shifted(locate, offset, 'source'),
                      (target.last['x'], target.last['y']))
        quiet(target, lambda r: not r['visits'] and not r['received'], .5)
        x11.drag_path((target.last['x'], target.last['y']), (60, 640), steps=12)
        x11.release()
        ended = server.wait_report('dragend')
        check(ended['dropEffect'] == 'none', 'rejected drag must not report a drop effect')
        exact(target.command(STATUS), visits=0, received=0, drop=0, stale=0)
        target.command(DISARM)
        clipboard(sentinel, target)
        evidence(name, sentinel=sentinel, target=target)
        row(name, 'PASS', app=kind, mode=mode, real_pid=real_pid, armed_pid=forged_pid)
        return True
    except Exception as error:
        row(name, 'FAIL', reason=str(error))
        raise
    finally:
        for role in locals().get('roles_used', []):
            role.close()
        if app:
            app.close()
        server.close()
        x11.close()


def nemo_startup_and_text_contract(roles, environment):
    startup = 'nemo-startup-identity'
    contract = 'nemo-target-text-contract'
    env, x11, epoch, server = session_setup(environment)
    app = None
    roles_used = []
    try:
        tree_dir = Path(env['OINK_NATIVE_TEST_ROOT']) / 'nemo-tree'
        tree_dir.mkdir(mode=0o700)
        (tree_dir / 'fixture-a.txt').write_text('oink nemo fixture\n')
        (tree_dir / 'fixture-b.txt').write_text('second fixture\n')
        app = harness.launch('nemo', str(tree_dir), environment)
        window = x11.wait_window('Nemo')[0]
        pid = x11.xres_pid(window['xid'])
        exe = app.verify_window_pid(pid)
        row(startup, 'PASS', app='nemo', pid=pid, exe=exe, epoch=epoch)
        wait_task_headroom()
        sentinel = Role(roles, 'sentinel', env); roles_used.append(sentinel); sentinel.ready()
        source = Role(roles, 'source', env); roles_used.append(source); source.ready()
        clipboard(sentinel, source)
        source.command(FOCUS)
        quiet(source, lambda r: r['begin'] == 0, .1)
        source.command(MOVE, source.last['x'], source.last['y'])
        source.command(DOWN)
        wait(source, lambda r: r['press'] == 1, 'real button press')
        source.command(MOVE, source.last['x'] + 60, source.last['y'])
        wait(source, lambda r: r['begin'] == 1, 'native drag begin')
        before = {p.name for p in tree_dir.iterdir()}
        # Drive the drag through the role's own synthetic input connection; a
        # second XTEST connection is invisible to the role's drag grab.
        tx = window['x'] + window['width'] // 2
        ty = window['y'] + window['height'] // 2
        sx, sy = source.last['x'] + 60, source.last['y']
        for step in range(1, 8):
            source.command(MOVE, sx + (tx - sx) * step // 8, sy + (ty - sy) * step // 8)
        # Settle with real motion before release (same XdndPosition-burst loss
        # as the browser target row on Xephyr).
        for wiggle in range(6):
            source.command(MOVE, tx + (10 if wiggle % 2 else -10), ty)
            time.sleep(0.15)
        source.command(MOVE, tx, ty)
        time.sleep(0.2)
        source.command(UP)  # nemo dispatches its drop handling on button release
        wait(source, lambda r: r['end'] == 1, 'drag end over nemo')
        wait(source, lambda r: r['toolkit_grabbed'] == 0, 'toolkit grab release', seconds=.5)
        report = source.command(STATUS)
        after = {p.name for p in tree_dir.iterdir()}
        accepted = report['get'] > 0 or after != before
        row(contract, 'CONTRACT_UNSUPPORTED' if not accepted else 'PASS',
            app='nemo', selection_requests=report['get'], tree_changed=after != before,
            note='nemo list view does not accept text/plain drops' if not accepted else '')
        source.command(FREE)
        clipboard(sentinel, source)
        evidence('nemo-text-contract', sentinel=sentinel, source=source)
        return True
    except Exception as error:
        row(startup, 'FAIL', reason=str(error))
        raise
    finally:
        for role in roles_used:
            role.close()
        if app:
            app.close()
        server.close()
        x11.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--roles', required=True, type=Path)
    parser.add_argument('--suite', choices=('chrome', 'firefox', 'nemo', 'all'), default='all')
    args = parser.parse_args(argv)
    check(args.roles.is_absolute() and args.roles.is_file(), '--roles must be an absolute binary')
    env = dict(os.environ)
    try:
        if args.suite in ('chrome', 'all'):
            browser_target_text(args.roles, 'chrome', 'Google-chrome', env)
            browser_source_synthetic_blocked(args.roles, 'chrome', 'Google-chrome', env,
                                             ('text', 'forged-pid', 'late-generation'))
        if args.suite in ('firefox', 'all'):
            browser_source_text(args.roles, 'firefox', 'firefox', env)
            browser_target_text(args.roles, 'firefox', 'firefox', env)
            browser_source_rejected(args.roles, 'firefox', 'firefox', env, 'forged-pid')
        if args.suite in ('nemo', 'all'):
            nemo_startup_and_text_contract(args.roles, env)
        print(json.dumps({'result': 'APP_SCENARIOS_DONE', 'rows': ROWS}, sort_keys=True), flush=True)
        return 0
    except Exception as error:
        print(json.dumps({'result': 'FAIL', 'reason': str(error), 'trace': traceback.format_exc(), 'rows': ROWS}, sort_keys=True), flush=True)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
