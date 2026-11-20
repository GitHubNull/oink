import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).with_name('linux_input_probe.c').resolve()
HARNESS = r'''
#define main probe_main
#define clock_gettime fixture_clock_gettime
#define poll fixture_poll
#include "INPUT_SOURCE"
#undef main

static long now_ms;
static int map_calls, dispatches, managed = 1, mode;
static int focus_mode, focus_calls;
static Window server_focus = 17;
static long selected;
int fixture_clock_gettime(clockid_t clock, struct timespec *value) {
    (void)clock;
    value->tv_sec = now_ms / 1000;
    value->tv_nsec = (now_ms % 1000) * 1000000;
    return 0;
}
int fixture_poll(struct pollfd *fds, nfds_t count, int timeout) {
    (void)fds; (void)count;
    now_ms += timeout;
    return 0;
}
Window XCreateWindow(Display *display, Window root, int x, int y,
                     unsigned int width, unsigned int height, unsigned int border,
                     int depth, unsigned int class, Visual *visual,
                     unsigned long mask, XSetWindowAttributes *attributes) {
    (void)display; (void)root; (void)x; (void)y; (void)width; (void)height;
    (void)border; (void)depth; (void)class; (void)visual;
    managed = !(mask & CWOverrideRedirect) || !attributes->override_redirect;
    return 17;
}
Window XCreateSimpleWindow(Display *display, Window root, int x, int y,
                           unsigned int width, unsigned int height,
                           unsigned int border_width, unsigned long border,
                           unsigned long background) {
    (void)display; (void)root; (void)x; (void)y; (void)width; (void)height;
    (void)border_width; (void)border; (void)background;
    managed = 1;
    return 17;
}
int XSelectInput(Display *display, Window window, long mask) {
    (void)display; (void)window;
    selected = mask;
    return 1;
}
int XMapWindow(Display *display, Window window) {
    (void)display; (void)window;
    map_calls++;
    return 1;
}
int XSync(Display *display, Bool discard) { (void)display; (void)discard; return 1; }
int XFlush(Display *display) { (void)display; return 1; }
Status XGetWindowAttributes(Display *display, Window window, XWindowAttributes *attributes) {
    (void)display; (void)window;
    attributes->map_state = (mode == 0 || (mode == 1 && dispatches)) ? IsViewable : IsUnmapped;
    return mode != 3;
}
int XPending(Display *display) { (void)display; return mode == 1 && !dispatches; }
int XNextEvent(Display *display, XEvent *event) {
    (void)display;
    memset(event, 0, sizeof(*event));
    event->type = MapNotify;
    dispatches++;
    now_ms++;
    return 0;
}
Status XGetInputFocus(Display *display, Window *focus_return, int *revert_to_return) {
    (void)display;
    if (focus_mode == 4)
        return 0;
    focus_calls++;
    if (focus_mode == 5 && focus_calls <= 3)
        *focus_return = 100 + focus_calls;
    else
        *focus_return = server_focus;
    *revert_to_return = RevertToNone;
    return 1;
}
int XSetInputFocus(Display *display, Window window, int revert_to, Time time) {
    (void)display; (void)revert_to; (void)time;
    if (focus_mode != 6)
        server_focus = window;
    return 1;
}
int main(int argc, char **argv) {
    mode = argc >= 2 ? atoi(argv[1]) : 0;
    focus_mode = argc >= 3 ? atoi(argv[2]) : 0;
    if (focus_mode == 6)
        server_focus = 2097162;
    _XPrivDisplay connection = calloc(1, sizeof(*connection));
    connection->fd = 3;
    Window result = prepare_target((Display *)connection, 1);
    require_focus_ready((Display *)connection, result);
    printf("managed=%d selected_structure=%d map_calls=%d dispatched=%d focus_calls=%d window=%lu\n",
           managed, !!(selected & StructureNotifyMask), map_calls, dispatches, focus_calls, result);
    free(connection);
    return 0;
}
'''


class InputWindowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        root = Path(cls.directory.name)
        harness = root / 'window.c'
        harness.write_text(HARNESS.replace('INPUT_SOURCE', str(SOURCE)))
        cls.binary = root / 'window'
        subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror',
                        '-ffunction-sections', '-fdata-sections', '-Wl,--gc-sections',
                        str(harness), '-o', str(cls.binary), '-lX11', '-lXi', '-lXtst'],
                       check=True, capture_output=True, timeout=30)

    def run_case(self, mode, focus=0):
        environment = dict(os.environ)
        environment.pop('DISPLAY', None)
        return subprocess.run([str(self.binary), str(mode), str(focus)], env=environment,
                              capture_output=True, text=True, timeout=3)

    def test_window_is_managed_before_input(self):
        result = self.run_case(0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('managed=1 selected_structure=1 map_calls=1', result.stdout)

    def test_waits_for_native_map_event(self):
        result = self.run_case(1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('dispatched=1', result.stdout)

    def test_unmapped_window_fails_closed(self):
        result = self.run_case(2)
        self.assertEqual(result.returncode, 1)
        self.assertIn('managed target mapping deadline', result.stderr)

    def test_window_query_failure_is_not_readiness(self):
        result = self.run_case(3)
        self.assertEqual(result.returncode, 1)
        self.assertIn('query managed target mapping', result.stderr)

    def test_waits_for_window_manager_focus_to_settle(self):
        result = self.run_case(0, 5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('focus_calls=7', result.stdout)
        self.assertIn('window=17', result.stdout)

    def test_focus_that_never_confirms_fails_closed(self):
        result = self.run_case(0, 6)
        self.assertEqual(result.returncode, 1)
        self.assertIn('managed focus readiness deadline', result.stderr)

    def test_focus_query_failure_is_not_readiness(self):
        result = self.run_case(0, 4)
        self.assertEqual(result.returncode, 1)
        self.assertIn('query window manager focus', result.stderr)
