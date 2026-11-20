"""Launch real applications inside the private runner session and verify identity.

Every launch gets a fresh profile/HOME subtree and is verified as a brand-new
process (start time, real executable, descendant tree, private display/auth).
A host instance can never satisfy these checks, so reuse is rejected rather
than merely discouraged.
"""
import os
from pathlib import Path
import signal
import subprocess
import time

from linux_dnd_protocol import Failure, check, isolated_environment

APPS = {
    'chrome': {'exe': 'google-chrome', 'wm_class': 'Google-chrome'},
    'firefox': {'exe': 'firefox', 'wm_class': 'firefox'},
    'nemo': {'exe': 'nemo', 'wm_class': 'Nemo'},
}


def _stat(pid, proc_root=Path('/proc')):
    try:
        fields = (proc_root / str(pid) / 'stat').read_text().split()
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return None
    # comm may itself contain spaces or parens; fields after its final ')' are stable.
    tail = fields[max(i for i, token in enumerate(fields) if token.endswith(')')) + 1:]
    return {'ppid': int(tail[1]), 'starttime': int(tail[19])}


def process_tree(root_pid, proc_root=Path('/proc')):
    root_stat = _stat(root_pid, proc_root)
    check(root_stat is not None, f'process {root_pid} missing')
    tree = {root_pid}
    queue = [root_pid]
    while queue:
        parent = queue.pop()
        for entry in proc_root.iterdir():
            if not entry.name.isdigit():
                continue
            stat = _stat(int(entry.name), proc_root)
            if stat and stat['ppid'] == parent and int(entry.name) not in tree:
                tree.add(int(entry.name))
                queue.append(int(entry.name))
    return tree


class AppSession:
    def __init__(self, kind, process, exe_path, starttime, proc_root=Path('/proc')):
        self.kind = kind
        self.process = process
        self.exe_path = exe_path
        self.starttime = starttime
        self.proc_root = proc_root

    @property
    def pid(self):
        return self.process.pid

    def tree(self):
        check(self.process.poll() is None, f'{self.kind} exited early ({self.process.returncode})')
        return process_tree(self.pid, self.proc_root)

    def exe_of(self, pid):
        try:
            return os.path.realpath(self.proc_root / str(pid) / 'exe')
        except (FileNotFoundError, PermissionError):
            return None

    def verify_window_pid(self, pid):
        check(pid in self.tree(), f'window owner pid {pid} outside {self.kind} process tree')
        exe = self.exe_of(pid)
        check(exe is not None and Path(exe).is_file(), f'no real executable for pid {pid}')
        return exe

    def close(self, timeout=10):
        try:
            os.killpg(os.getpgid(self.pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(self.pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            self.process.wait(timeout=5)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if _stat(self.pid, self.proc_root) is None:
                return
            time.sleep(0.05)
        raise Failure(f'{self.kind} root process still alive after close')


def _argv(kind, profile, target):
    if kind == 'chrome':
        return ['google-chrome', f'--user-data-dir={profile}', '--no-first-run',
                '--no-default-browser-check', '--disable-background-networking',
                '--disable-component-update', '--disable-sync', '--no-pings',
                '--ozone-platform=x11', '--window-size=960,720', target]
    if kind == 'firefox':
        return ['firefox', '-profile', str(profile), '-no-remote', '-new-instance', target]
    if kind == 'nemo':
        return ['nemo', '--no-desktop', str(target)]
    raise Failure(f'unknown app {kind}')


def launch(kind, target, environment, proc_root=Path('/proc'), start_new_session=True):
    check(kind in APPS, f'unknown app {kind}')
    env = isolated_environment(environment)
    root = Path(env['OINK_NATIVE_TEST_ROOT'])
    profile = root / 'app-profiles' / f'{kind}-{time.time_ns()}'
    profile.mkdir(mode=0o700, parents=True, exist_ok=False)
    if kind == 'firefox':
        (profile / 'user.js').write_text('\n'.join([
            'user_pref("browser.shell.checkDefaultBrowser", false);',
            'user_pref("app.update.disabledForTesting", true);',
            'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
            'user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);',
            'user_pref("browser.sessionstore.resume_from_crash", false);',
            'user_pref("dom.ipc.processCount", 1);',
            'user_pref("dom.ipc.processPrelaunchEnabled", false);',
            'user_pref("dom.ipc.processPrelaunch.fission.number", 0);',
            'user_pref("dom.ipc.processPrelaunch.lowmem", 0);',
        ]) + '\n')
    exe = APPS[kind]['exe']
    argv = _argv(kind, profile, target)
    before = time.time()
    process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               start_new_session=start_new_session)
    stat = _stat(process.pid, proc_root)
    check(stat is not None, f'{kind} process vanished')
    check(_exe_real(process.pid, proc_root) is not None, f'{kind} has no executable')
    after = time.time()
    uptime_start = _boot_time(proc_root)
    started_at = uptime_start + stat['starttime'] / os.sysconf('SC_CLK_TCK')
    check(uptime_start > 0 and before - 5 <= started_at <= after + 5,
          f'{kind} process predates this launch (host instance reuse?)')
    exe_path = _exe_real(process.pid, proc_root)
    check(exe_path and Path(exe_path).is_file() and not Path(exe_path).is_symlink(),
          f'{kind} executable must be a real file')
    check(env['DISPLAY'] == environment['DISPLAY']
          and env['XAUTHORITY'] == environment['XAUTHORITY'], 'private display/auth binding')
    return AppSession(kind, process, exe_path, stat['starttime'], proc_root)


def _exe_real(pid, proc_root):
    try:
        return os.path.realpath(proc_root / str(pid) / 'exe')
    except (FileNotFoundError, PermissionError):
        return None


def _boot_time(proc_root):
    for line in (proc_root / 'stat').read_text().splitlines():
        if line.startswith('btime '):
            return int(line.split()[1])
    raise Failure('no btime in /proc/stat')
