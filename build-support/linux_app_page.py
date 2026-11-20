"""Loopback page fixture and verified app-drag helper for real-app suites.

The page is served only from 127.0.0.1; reports carry event trust plus payload
hashes, never content.
"""
import json
from pathlib import Path
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from linux_dnd_protocol import Failure, check


class PageServer:
    def __init__(self, page):
        self.page = Path(page).read_bytes()
        self.reports = []
        self.lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                if self.path.split('?')[0] == '/':
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/html; charset=utf-8')
                    self.send_header('Content-Length', str(len(outer.page)))
                    self.end_headers()
                    self.wfile.write(outer.page)
                elif self.path.split('?')[0] == '/state':
                    body = json.dumps(outer.snapshot()).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    self.send_error(404)

            def do_POST(self):
                if self.path.split('?')[0] != '/report':
                    self.send_error(404)
                    return
                length = int(self.headers.get('Content-Length', '0'))
                check(0 < length <= 4096, 'bounded page report')
                report = json.loads(self.rfile.read(length).decode())
                with outer.lock:
                    outer.reports.append(report)
                body = b'ok'
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.httpd = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        self.url = f'http://127.0.0.1:{self.httpd.server_address[1]}/'

    def snapshot(self):
        with self.lock:
            return list(self.reports)

    def wait_report(self, kind, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for report in self.snapshot():
                if report.get('kind') == kind:
                    return report
            time.sleep(0.05)
        raise Failure(f'page did not report {kind}; reports={self.snapshot()}')

    def latest(self, kind, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for report in reversed(self.snapshot()):
                if report.get('kind') == kind:
                    return report
            time.sleep(0.05)
        raise Failure(f'page did not report {kind}')

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=5)


def digest_words(sha256_hex):
    check(len(sha256_hex) == 64, 'page sha256 shape')
    return [int(sha256_hex[i * 8:(i + 1) * 8], 16) for i in range(8)]


def page_matches(report, ready):
    words = digest_words(ready['payload_sha256'])
    check(report['length'] == ready['payload_length'], 'payload length mismatch with page')
    check(all(report[f'digest{i}'] == words[i] for i in range(8)), 'payload SHA-256 mismatch with page')


def begin_app_drag(x11, app, server, window, coords):
    """Drag from the app source; return the real XdndSelection owner window. The
    owner may be an override-redirect proxy window outside the app tree, so the
    owner is verified by XRes server-side client PID inside the app process tree.
    A fresh page probe-mousedown proves the press reached the page, not a role
    window stacked above it."""
    pressed = False
    for attempt in range(3):
        x11.raise_window(window['xid'])
        x11.focus(window['xid'])
        x11.motion(*coords)
        seen = len(server.snapshot())
        x11.button(True)
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            reports = server.snapshot()
            fresh = reports[seen:]
            seen = len(reports)
            if any(report['kind'] == 'probe-mousedown' for report in fresh):
                pressed = True
                break
            time.sleep(0.05)
        if pressed:
            break
        x11.button(False)
        time.sleep(0.3)
    if not pressed:
        raise Failure('app page never received the synthetic press')
    time.sleep(0.2)  # let the app process the press before the drag motion
    for step in range(1, 7):
        x11.motion(coords[0] + 2 * step, coords[1] + step)
        time.sleep(0.02)
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        owner = x11.selection_owner()
        if owner:
            pid = x11.xres_pid(owner)
            if pid and pid in app.tree():
                return owner
        time.sleep(0.02)
    raise Failure(f'app never became XdndSelection owner; owner={x11.selection_owner()}')
