#!/usr/bin/env python3
"""Linux XDND D wave: generation, peer and controller recovery.

The scenario script is the recovery supervisor: it assigns generations, owns a
fault permit registry (exact PID + /proc starttime), observes real control-pipe
EOF before restarting anything, and never restarts into a stacked fault
environment. Old-generation commands are soft-refused by the roles (status 1),
so every restart is proven against a live stale-identity attack.
"""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))

from linux_dnd_app_scenarios import row, wait_task_headroom
from linux_dnd_protocol import (
    ARM, Blocked, ESCAPE, Failure, FETCH, GENERATION, READ_SELECTION, STATUS, UP,
    Role,
    bridge_payload, check, isolated_environment, peer_environment,
)
from linux_dnd_scenarios import (
    Group, clipboard, cleanup_source, enter, exact, evidence, return_reenter,
    start, wait,
)

SCENARIOS = {}


def scenario(fn):
    SCENARIOS[fn.__name__] = fn
    return fn


def process_token(process):
    """Owned-child identity: parent plus /proc starttime, not a reusable PID."""
    try:
        fields = Path(f'/proc/{process.pid}/stat').read_text().rsplit(')', 1)[1].split()
    except FileNotFoundError:
        check(process.poll() is not None, 'fault target unobservable while alive')
        return None
    check(int(fields[1]) == os.getpid(), 'fault target is not an owned child')
    return fields[19]


def group_gone(pid):
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return True
    return False


class Faults:
    """Supervisor-side fault permit registry: kill only registered identities."""

    def __init__(self):
        self.allowed = {}

    def register(self, label, process):
        token = process_token(process)
        check(token is not None and label not in self.allowed, 'duplicate fault registration')
        self.allowed[label] = (process.pid, token)

    def kill(self, label, process):
        check(label in self.allowed, f'fault refused: {label} not registered')
        pid, token = self.allowed[label]
        check(process.pid == pid and process_token(process) == token,
              f'fault refused: {label} identity mismatch (PID reuse?)')
        check(process.poll() is None, f'fault refused: {label} already exited')
        self.allowed.pop(label)
        process.kill()
        process.wait(timeout=5)
        check(process.returncode == -signal.SIGKILL, 'deliberate fault must be SIGKILL')
        check(group_gone(process.pid), f'{label} process group survived')

    def refuse(self, label, process):
        """Negative control: an unregistered or foreign target must never fault."""
        check(all(process.pid != pid for pid, _ in self.allowed.values()),
              f'{label} unexpectedly holds a fault permit')
        token = process_token(process)
        check(token is not None, f'{label} unobservable')
        raise Failure(f'fault permit correctly refused for unregistered {label}')


@contextmanager
def x11_session(session_env):
    """X11 helper on an arbitrary private session (it opens via process env)."""
    from linux_app_x11 import X11
    previous = {key: os.environ.get(key) for key in ('DISPLAY', 'XAUTHORITY')}
    os.environ['DISPLAY'] = session_env['DISPLAY']
    os.environ['XAUTHORITY'] = session_env['XAUTHORITY']
    try:
        x11 = X11(session_env)
        try:
            yield x11
        finally:
            x11.close()
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def expect_eof(role, code=-signal.SIGKILL):
    """The supervisor must observe control EOF and reap before any restart."""
    deadline = time.monotonic() + 5
    eof = False
    while time.monotonic() < deadline and not eof:
        try:
            chunk = os.read(role.process.stdout.fileno(), 4096)
            eof = chunk == b''
        except (BlockingIOError, OSError):
            time.sleep(0.02)
    check(eof, f'{role.name}: supervisor never saw control EOF (masked pipe?)')
    check(role.process.wait(timeout=2) == code, f'{role.name}: exit {code} expected')
    check(not role.frames, f'{role.name}: unprocessed frames would mask EOF')
    role.expected_exit = code
    role.close()  # Release the supervisor side: stream FD, selector, pipes.


def stale_refused(role, label):
    """Old-generation commands are soft-refused (status 1), role stays alive."""
    report = role.command(STATUS, generation=role.generation - 1, expected=1)
    check(report['gen'] == role.generation, f'{label}: stale read changed generation')


def catch_reads(catcher, label):
    exact(catcher.command(READ_SELECTION, timeout=.5), pending=1, received=0)
    received = wait(catcher, lambda r: r['received'] >= 1, f'{label} real selection read')
    check(received['drop'] == 0 and received['stale'] == 0, f'{label}: {received}')
    body = catcher.command(FETCH)
    check(len(body) == received['length'] and received['length'] > 0, f'{label}: empty read')
    return body


@scenario
def source_restart_two_generations(binary, env, peer):
    """Kill an idle source, reap EOF, refuse stale gen, restart at gen 2."""
    name = 'source-restart-two-generations'
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source = group.add('source', env)
        catcher = group.add('catcher', env)
        target = group.add('target', env)
        clipboard(sentinel, source, catcher, target)
        faults = Faults()
        faults.register('source-gen1', source.process)
        # Generation 1: full real delivery through catcher and target.
        catcher.command(ARM, start(source))
        target.command(ARM, source.last['source_xid'])
        enter(source, catcher)
        return_reenter(source, catcher)
        enter(source, target)
        source.command(UP)
        wait(target, lambda r: r['received'] == 1, 'gen1 target real selection read')
        cleanup_source(source, False, 1)
        # Fault: source dies idle; supervisor sees EOF, reaps, releases FDs.
        faults.kill('source-gen1', source.process)
        expect_eof(source)
        clipboard(sentinel, catcher, target)  # session alive, sentinel unchanged
        # Negative control: catcher holds no fault permit.
        try:
            faults.refuse('catcher', catcher.process)
        except Failure:
            pass
        # Generation 2: fresh source process, old generation soft-refused.
        source2 = group.add('source', env)
        faults.register('source-gen2', source2.process)
        source2.command(GENERATION, generation=2)  # before begin: publishes gen 2
        catcher.command(ARM, start(source2), generation=2)
        target.command(ARM, source2.last['source_xid'], generation=2)
        stale_refused(catcher, 'catcher gen1 attack')
        stale_refused(target, 'target gen1 attack')
        enter(source2, catcher, visits=3)
        enter(source2, target, visits=2)
        source2.command(UP)
        wait(target, lambda r: r['received'] == 2, 'gen2 target real selection read')
        cleanup_source(source2, False, 1, returned=0)
        clipboard(sentinel, source2, catcher, target)
        evidence(name, sentinel=sentinel, source=source2, catcher=catcher, target=target)
        row(name, 'PASS', generations=2, source_restarts=1,
            stale_attacks=2, source_pid_gen2=source2.process.pid)


@scenario
def peer_reconnect_two_generations(binary, env, peer):
    """Dual server: reap source+catcher, restart both, bridge at gen 2."""
    name = 'peer-reconnect-two-generations'
    with Group(binary) as group:
        sentinel_a = group.add('sentinel', env)
        sentinel_b = group.add('sentinel', peer)
        source, catcher = group.add('source', env), group.add('catcher', env)
        proxy, target = group.add('proxy', peer), group.add('target', peer)
        clipboard(sentinel_a, source, catcher)
        clipboard(sentinel_b, proxy, target)
        faults = Faults()
        faults.register('source-gen1', source.process)
        faults.register('catcher-gen1', catcher.process)
        # Generation 1: source -> catcher (main) -> bridge -> proxy -> target (peer).
        catcher.command(ARM, start(source))
        enter(source, catcher)
        body = catch_reads(catcher, 'gen1 catcher')
        source.command(ESCAPE)
        wait(source, lambda r: r['end'] == 1, 'gen1 drag end')
        # XTEST button state is server-global: release before the fault, or the
        # SIGKILLed holder leaves button 1 stuck down for every later press.
        source.command(UP)
        wait(source, lambda r: r['raw_up'] == 1, 'gen1 button release')
        loaded = bridge_payload(catcher, proxy)
        check(loaded['length'] == len(body), 'gen1 bridge length')
        target.command(ARM, start(proxy))
        enter(proxy, target)
        proxy.command(UP)
        wait(target, lambda r: r['received'] == 1, 'gen1 peer target read')
        cleanup_source(proxy, False, 1, returned=0)
        # Full old-generation reap before any restart: EOF + FD release, both legs.
        faults.kill('source-gen1', source.process)
        expect_eof(source)
        faults.kill('catcher-gen1', catcher.process)
        expect_eof(catcher)
        clipboard(sentinel_a)  # main session survived the double fault
        clipboard(sentinel_b, proxy, target)  # peer session survived
        try:
            faults.refuse('proxy', proxy.process)
        except Failure:
            pass
        # Generation 2: both restarted processes, stale gen refused on the survivor.
        with x11_session(peer) as x11:  # old proxy keeps the proxy slot on peer
            x11.move_window(proxy.last['xid'], 60, 620)
        time.sleep(0.3)
        source2 = group.add('source', env)
        catcher2 = group.add('catcher', env)
        proxy2 = group.add('proxy', peer)
        source2.command(GENERATION, generation=2)
        proxy2.command(GENERATION, generation=2)
        catcher2.command(ARM, start(source2), generation=2)
        enter(source2, catcher2)
        body2 = catch_reads(catcher2, 'gen2 catcher')
        source2.command(ESCAPE)
        wait(source2, lambda r: r['end'] == 1, 'gen2 drag end')
        source2.command(UP)
        wait(source2, lambda r: r['raw_up'] == 1, 'gen2 button release')
        bridge_payload(catcher2, proxy2)
        target.command(ARM, start(proxy2), generation=2)
        stale_refused(target, 'target gen1 attack')
        enter(proxy2, target, visits=2)
        proxy2.command(UP)
        wait(target, lambda r: r['received'] == 2, 'gen2 peer target read')
        cleanup_source(proxy2, False, 1, returned=0)
        evidence(name, source=source2, catcher=catcher2, proxy=proxy2, target=target,
                 sentinel_a=sentinel_a, sentinel_b=sentinel_b)
        row(name, 'PASS', generations=2, peer_restarts=2, stale_attacks=1,
            servers=('main', 'peer'))


@scenario
def controller_eof_mid_drag(binary, env, peer):
    """Kill a target mid-drag: EOF observed, grabs reclaimed, both legs restart."""
    name = 'controller-eof-mid-drag'
    with Group(binary) as group:
        sentinel = group.add('sentinel', env)
        source = group.add('source', env)
        catcher = group.add('catcher', env)
        target = group.add('target', env)
        clipboard(sentinel, source, catcher, target)
        faults = Faults()
        xid = start(source)
        catcher.command(ARM, xid)
        target.command(ARM, xid)
        enter(source, catcher)
        faults.register('target-gen1', target.process)
        # Controller death while the source drag (and its grabs) is live.
        faults.kill('target-gen1', target.process)
        expect_eof(target)
        report = source.command(STATUS)
        check(report['begin'] == 1 and report['end'] == 0, 'source drag died with target')
        # Cancel the orphaned drag: Escape must free grabs before ButtonUp.
        source.command(ESCAPE)
        wait(source, lambda r: r['end'] == 1, 'orphaned drag cancel')
        cleanup_source(source, True, 0, returned=0)
        # GTK leaves the cancelled drag's override-redirect icon window behind;
        # retiring the process is the only clean teardown that reclaims it.
        source.close()
        time.sleep(0.4)
        # Restart only after EOF/exit observed: gen 2, stale identity refused.
        source2 = group.add('source', env)
        target2 = group.add('target', env)
        faults.register('source-gen2', source2.process)
        source2.command(GENERATION, generation=2)
        target2.command(ARM, start(source2), generation=2)
        catcher.command(ARM, source2.last['source_xid'], generation=2)
        stale_refused(catcher, 'catcher gen1 attack')
        stale_refused(target2, 'target gen1 attack')
        enter(source2, catcher, visits=2)
        enter(source2, target2)
        source2.command(UP)
        wait(target2, lambda r: r['received'] == 1, 'gen2 target real selection read')
        cleanup_source(source2, False, 1, returned=0)
        clipboard(sentinel, source2, catcher, target2)
        evidence(name, sentinel=sentinel, source=source2, catcher=catcher, target=target2)
        row(name, 'PASS', generations=2, mid_drag_faults=1,
            eof_observed=True, orphan_cancelled=True)


@scenario
def fault_nested_xephyr(binary, env, peer):
    """Fault permit gates a nested Xephyr; the main private session survives."""
    name = 'fault-nested-xephyr'
    from linux_app_x11 import X11
    root = Path(env['OINK_NATIVE_TEST_ROOT']) / 'fault-xephyr'
    root.mkdir(mode=0o700)
    authority = root / 'Xauthority'
    authority.touch(mode=0o600)
    cookie = os.urandom(16).hex()
    subprocess.run(['xauth', '-f', str(authority), 'add', ':999',
                    'MIT-MAGIC-COOKIE-1', cookie], env=env, check=True, timeout=5)
    read_fd, write_fd = os.pipe()
    log = (root / 'nested.log').open('wb')
    nested = None
    try:
        nested = subprocess.Popen(
            ['Xephyr', '-displayfd', str(write_fd), '-auth', str(authority),
             '-nolisten', 'tcp', '-no-host-grab', '-screen', '640x480', '-noreset'],
            env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            start_new_session=True, pass_fds=(write_fd,))
        os.close(write_fd)
        write_fd = None
        check(select.select([read_fd], [], [], 10)[0], 'nested Xephyr display assign')
        number = os.read(read_fd, 64).decode('ascii').strip()
        check(number.isdecimal() and f':{number}' != env['DISPLAY'].split('.')[0],
              'nested display invalid or host collision')
        subprocess.run(['xauth', '-f', str(authority), 'add', f':{number}',
                        'MIT-MAGIC-COOKIE-1', cookie], env=env, check=True, timeout=5)
        subprocess.run(['xauth', '-f', str(authority), 'remove', ':999'],
                       env=env, check=True, timeout=5)
        nested_env = dict(env)
        nested_env['DISPLAY'] = f':{number}'
        nested_env['XAUTHORITY'] = str(authority)
        previous = os.environ.get('DISPLAY'), os.environ.get('XAUTHORITY')
        os.environ['DISPLAY'] = nested_env['DISPLAY']
        os.environ['XAUTHORITY'] = nested_env['XAUTHORITY']
        try:
            probe = X11(nested_env)  # XOpenDisplay(None) reads the process env.
            check(probe.display, 'nested Xephyr not speaking X11')
            probe.close()
        finally:
            if previous[0] is None:
                os.environ.pop('DISPLAY', None)
            else:
                os.environ['DISPLAY'] = previous[0]
            if previous[1] is None:
                os.environ.pop('XAUTHORITY', None)
            else:
                os.environ['XAUTHORITY'] = previous[1]
        # Negative control: a foreign sleep process holds no fault permit.
        stray = subprocess.Popen(['sleep', '60'], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        faults = Faults()
        try:
            faults.refuse('stray-sleep', stray)
        except Failure:
            pass
        stray.terminate()
        stray.wait(timeout=2)
        # Registered fault: exact PID + starttime, then reap and verify survival.
        faults.register('nested-xephyr', nested)
        with Group(binary) as group:
            sentinel = group.add('sentinel', env)
            catcher = group.add('catcher', env)
            clipboard(sentinel, catcher)
            faults.kill('nested-xephyr', nested)
            check(nested.returncode == -signal.SIGKILL, 'nested Xephyr SIGKILL')
            main_x = X11(env)
            check(main_x.display and main_x.root, 'main private X server died')
            main_x.close()
            clipboard(sentinel, catcher)  # roles on the main server unaffected
            evidence(name, sentinel=sentinel, catcher=catcher)
            row(name, 'PASS', nested_display=f':{number}',
                nested_pid=nested.pid, main_session_survived=True)
    finally:
        os.close(read_fd)
        if write_fd is not None:
            os.close(write_fd)
        log.close()
        if nested is not None and nested.poll() is None:
            nested.kill()
            nested.wait(timeout=5)


@scenario
def new_generation_start_failure(binary, env, peer):
    """A restart that cannot start must fail closed with no adopted resources."""
    name = 'new-generation-start-failure'
    root = Path(env['OINK_NATIVE_TEST_ROOT'])
    missing = root / 'no-such-role-binary'
    check(not missing.exists(), 'missing binary path unexpectedly exists')
    role = None
    try:
        role = Role(missing, 'source', env)
        role.ready()
    except (OSError, Failure) as error:
        if role is not None:
            role.close()
        row(f'{name}-missing-binary', 'PASS', refused=type(error).__name__)
    else:
        raise Failure('missing binary was adopted')
    # A real binary that is not a role exits without readiness: no adoption.
    impostor = Role('/bin/true', 'source', env)
    try:
        impostor.ready()
    except Failure:
        check(impostor.process.wait(timeout=2) == 0, 'impostor still running')
        impostor.expected_exit = 0
        impostor.close()
        row(f'{name}-impostor-binary', 'PASS', refused='EOF', exit_code=0)
    else:
        raise Failure('impostor binary was adopted')
    # The session stays healthy for a real restart afterwards.
    with Group(binary) as group:
        first = group.add('sentinel', env)
        second = group.add('catcher', env)
        clipboard(first, second)
        evidence(name, sentinel=first, catcher=second)
        row(name, 'PASS', failed_starts=2, session_healthy=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--roles', required=True, type=Path)
    parser.add_argument('--suite', choices=('all', *sorted(SCENARIOS)), default='all')
    args = parser.parse_args(argv)
    check(args.roles.is_absolute() and args.roles.is_file(), '--roles must be an absolute binary')
    env = isolated_environment(dict(os.environ))
    try:
        peer = peer_environment(env, os.environ.get('OINK_NATIVE_TEST_PEER_ENV'))
    except Blocked:
        peer = None
    selected = sorted(SCENARIOS) if args.suite == 'all' else (args.suite,)
    failures = 0
    for name in selected:
        if name == 'peer_reconnect_two_generations' and peer is None:
            row(name, 'BLOCKED', reason='dual-server requires runner --peer-x11')
            continue
        wait_task_headroom()
        try:
            SCENARIOS[name](args.roles, env, peer)
        except Exception as error:
            failures += 1
            import traceback
            row(name, 'FAIL', reason=str(error), trace=traceback.format_exc())
        # XTEST button state is server-global and survives client death; a
        # fault-killed mid-drag role must not leave button 1 stuck for the next
        # scenario sharing this private display.
        with x11_session(env) as x11:
            x11.release()
    print(json.dumps({'result': 'RECOVERY_DONE' if not failures else 'FAIL',
                      'rows': len(selected), 'failures': failures}), flush=True)
    return 1 if failures else 0


if __name__ == '__main__':
    raise SystemExit(main())
