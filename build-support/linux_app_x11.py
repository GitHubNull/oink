"""Minimal ctypes X11/XRes/XTest helpers for real-application scenarios.

Only used inside the private linux-native-test.py session. Loads system
libX11/libXtst/libXRes at connect time; callers must pass an environment whose
DISPLAY/XAUTHORITY belong to the private runner root, so the host session is
never reachable.
"""
import ctypes as C
import ctypes.util

from linux_dnd_protocol import Failure, check


class XClassHint(C.Structure):
    _fields_ = [('res_name', C.c_void_p), ('res_class', C.c_void_p)]


class XWindowAttributes(C.Structure):
    _fields_ = [('x', C.c_int), ('y', C.c_int), ('width', C.c_int), ('height', C.c_int),
                ('border_width', C.c_int), ('depth', C.c_int), ('visual', C.c_void_p),
                ('root', C.c_ulong), ('class', C.c_int), ('bit_gravity', C.c_int),
                ('win_gravity', C.c_int), ('backing_store', C.c_int), ('backing_planes', C.c_ulong),
                ('backing_pixel', C.c_ulong), ('save_under', C.c_bool), ('colormap', C.c_ulong),
                ('map_installed', C.c_bool), ('map_state', C.c_int), ('all_event_masks', C.c_long),
                ('your_event_mask', C.c_long), ('do_not_propagate_mask', C.c_long),
                ('override_redirect', C.c_bool), ('screen', C.c_void_p)]


class XResClientIdSpec(C.Structure):
    _fields_ = [('client', C.c_ulong), ('mask', C.c_uint)]


class XResClientIdValue(C.Structure):
    _fields_ = [('spec', XResClientIdSpec), ('length', C.c_long), ('value', C.c_void_p)]


XRES_CLIENT_ID_PID = 1
XRES_CLIENT_ID_PID_MASK = 1 << XRES_CLIENT_ID_PID


class X11:
    def __init__(self, environment):
        root = environment.get('OINK_NATIVE_TEST_ROOT', '')
        check(root.startswith('/tmp/oink-native-'), 'X11 helpers require the private runner root')
        display = environment.get('DISPLAY', '')
        check(display.startswith(':') and display not in (':0', ':0.0'), 'refusing host/remote DISPLAY')
        found = {name: ctypes.util.find_library(name) for name in ('X11', 'Xtst', 'XRes')}
        check(all(found.values()), f'missing X11 runtime library: {found}')
        self.x11 = C.CDLL(found['X11'])
        self.xtst = C.CDLL(found['Xtst'])
        self.xres = C.CDLL(found['XRes'])
        self._signatures()
        self.display = self.x11.XOpenDisplay(None)
        check(self.display, 'connect private X server')
        self.screen = self.x11.XDefaultScreen(self.display)
        self.root = self.x11.XRootWindow(self.display, self.screen)
        event = error = major = minor = C.c_int()
        check(self.xtst.XTestQueryExtension(self.display, C.byref(event), C.byref(error),
                                            C.byref(major), C.byref(minor)), 'XTEST extension')
        width = self.x11.XDisplayWidth(self.display, self.screen)
        height = self.x11.XDisplayHeight(self.display, self.screen)
        check(width > 0 and height > 0, 'private screen geometry')
        res_major, res_minor = C.c_int(), C.c_int()
        check(self.xres.XResQueryVersion(self.display, C.byref(res_major), C.byref(res_minor)),
              'XRes extension version negotiation')

    def _signatures(self):
        x11 = self.x11
        x11.XOpenDisplay.restype = C.c_void_p
        x11.XOpenDisplay.argtypes = [C.c_char_p]
        x11.XDefaultScreen.restype = C.c_int
        x11.XDefaultScreen.argtypes = [C.c_void_p]
        x11.XRootWindow.restype = C.c_ulong
        x11.XRootWindow.argtypes = [C.c_void_p, C.c_int]
        x11.XDisplayWidth.restype = C.c_int
        x11.XDisplayWidth.argtypes = [C.c_void_p, C.c_int]
        x11.XDisplayHeight.restype = C.c_int
        x11.XDisplayHeight.argtypes = [C.c_void_p, C.c_int]
        x11.XInternAtom.restype = C.c_ulong
        x11.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_bool]
        x11.XGetSelectionOwner.restype = C.c_ulong
        x11.XGetSelectionOwner.argtypes = [C.c_void_p, C.c_ulong]
        x11.XChangeProperty.restype = C.c_int
        x11.XChangeProperty.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong, C.c_ulong, C.c_int,
                                        C.c_int, C.POINTER(C.c_ulong), C.c_int]
        x11.XDeleteProperty.restype = C.c_int
        x11.XDeleteProperty.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong]
        x11.XGetWindowProperty.restype = C.c_int
        x11.XGetWindowProperty.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong, C.c_long, C.c_long,
                                           C.c_bool, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_int),
                                           C.POINTER(C.c_ulong), C.POINTER(C.c_ulong),
                                           C.POINTER(C.POINTER(C.c_ulong))]
        x11.XFree.argtypes = [C.c_void_p]
        x11.XSync.restype = C.c_int
        x11.XSync.argtypes = [C.c_void_p, C.c_bool]
        x11.XCloseDisplay.argtypes = [C.c_void_p]
        x11.XQueryTree.restype = C.c_bool
        x11.XQueryTree.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong),
                                   C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)]
        x11.XGetWindowAttributes.restype = C.c_bool
        x11.XGetWindowAttributes.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(XWindowAttributes)]
        x11.XGetClassHint.restype = C.c_bool
        x11.XGetClassHint.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(XClassHint)]
        x11.XSetInputFocus.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_ulong]
        x11.XRaiseWindow.argtypes = [C.c_void_p, C.c_ulong]
        x11.XMoveWindow.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_int]
        x11.XTranslateCoordinates.restype = C.c_bool
        x11.XTranslateCoordinates.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong, C.c_int, C.c_int,
                                              C.POINTER(C.c_int), C.POINTER(C.c_int), C.POINTER(C.c_ulong)]
        x11.XMapRaised.restype = C.c_int
        x11.XMapRaised.argtypes = [C.c_void_p, C.c_ulong]
        xt = self.xtst
        xt.XTestQueryExtension.restype = C.c_bool
        xt.XTestQueryExtension.argtypes = [C.c_void_p, C.POINTER(C.c_int), C.POINTER(C.c_int),
                                           C.POINTER(C.c_int), C.POINTER(C.c_int)]
        xt.XTestFakeMotionEvent.restype = C.c_bool
        xt.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]
        xt.XTestFakeButtonEvent.restype = C.c_bool
        xt.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_bool, C.c_ulong]
        xt.XTestFakeKeyEvent.restype = C.c_bool
        xt.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_bool, C.c_ulong]
        xr = self.xres
        xr.XResQueryVersion.restype = C.c_int
        xr.XResQueryVersion.argtypes = [C.c_void_p, C.POINTER(C.c_int), C.POINTER(C.c_int)]
        xr.XResQueryClientIds.restype = C.c_int
        xr.XResQueryClientIds.argtypes = [C.c_void_p, C.c_long, C.POINTER(XResClientIdSpec),
                                          C.POINTER(C.c_long), C.POINTER(C.c_void_p)]
        xr.XResGetClientIdType.restype = C.c_int
        xr.XResGetClientIdType.argtypes = [C.c_void_p]
        xr.XResGetClientPid.restype = C.c_int
        xr.XResGetClientPid.argtypes = [C.c_void_p]
        xr.XResClientIdsDestroy.argtypes = [C.c_long, C.c_void_p]

    def atom(self, name):
        return self.x11.XInternAtom(self.display, name.encode(), False)

    def selection_owner(self, name='XdndSelection'):
        return self.x11.XGetSelectionOwner(self.display, self.atom(name))

    def set_cardinal(self, window, atom_name, value):
        atom = self.atom(atom_name) if isinstance(atom_name, str) else atom_name
        data = C.c_ulong(value)
        check(self.x11.XChangeProperty(self.display, window, atom, self.atom('CARDINAL'), 32,
                                       0, C.byref(data), 1) == 1, f'set {atom_name}')  # PropModeReplace=0
        self.x11.XSync(self.display, False)

    def get_cardinal(self, window, atom_name):
        atom = self.atom(atom_name) if isinstance(atom_name, str) else atom_name
        actual = C.c_ulong()
        fmt = C.c_int()
        count = C.c_ulong()
        remaining = C.c_ulong()
        data = C.POINTER(C.c_ulong)()
        status = self.x11.XGetWindowProperty(self.display, window, atom, 0, 1, False,
                                             self.atom('CARDINAL'), C.byref(actual), C.byref(fmt),
                                             C.byref(count), C.byref(remaining), C.byref(data))
        check(status == 0, f'query {atom_name}')
        if not count.value:
            return None
        check(count.value == 1 and fmt.value == 32 and remaining.value == 0, f'shape of {atom_name}')
        value = data.contents.value
        self.x11.XFree(data)
        return value

    def xres_pid(self, window):
        spec = XResClientIdSpec(window, XRES_CLIENT_ID_PID_MASK)
        count = C.c_long()
        ids = C.c_void_p()
        # Xephyr fills valid results but reports Status 0, so accept populated
        # output only; an unanswered query leaves count 0 and ids NULL.
        self.xres.XResQueryClientIds(self.display, 1, C.byref(spec), C.byref(count),
                                     C.byref(ids))
        pid = None
        if ids.value and count.value > 0:
            entries = C.cast(ids, C.POINTER(XResClientIdValue))
            for index in range(count.value):
                if self.xres.XResGetClientIdType(C.byref(entries[index])) == XRES_CLIENT_ID_PID:
                    pid = self.xres.XResGetClientPid(C.byref(entries[index]))
            self.xres.XResClientIdsDestroy(count, ids)
        check(pid is not None and pid > 0, f'no XRes PID for window {window}')
        return pid

    def top_windows(self, res_class):
        found = []
        stack = [self.root]
        while stack:
            window = stack.pop()
            attributes = XWindowAttributes()
            if not self.x11.XGetWindowAttributes(self.display, window, C.byref(attributes)):
                continue
            if attributes.map_state != 2:  # IsViewable
                continue
            hint = XClassHint()
            matched = False
            if self.x11.XGetClassHint(self.display, window, C.byref(hint)):
                klass = C.cast(hint.res_class, C.c_char_p).value if hint.res_class else None
                matched = res_class is None or klass == res_class.encode()
                if hint.res_name:
                    self.x11.XFree(hint.res_name)
                if hint.res_class:
                    self.x11.XFree(hint.res_class)
            if matched:
                root_x = C.c_int()
                root_y = C.c_int()
                child = C.c_ulong()
                check(self.x11.XTranslateCoordinates(self.display, window, self.root, 0, 0,
                                                     C.byref(root_x), C.byref(root_y), C.byref(child)),
                      'translate app window origin')
                found.append({'xid': window, 'x': root_x.value, 'y': root_y.value,
                              'width': attributes.width, 'height': attributes.height})
            root_return = C.c_ulong()
            parent = C.c_ulong()
            children = C.POINTER(C.c_ulong)()
            count = C.c_uint()
            if self.x11.XQueryTree(self.display, window, C.byref(root_return), C.byref(parent),
                                   C.byref(children), C.byref(count)):
                for index in range(count.value):
                    stack.append(children[index])
                if children:
                    self.x11.XFree(children)
        return found

    def wait_window(self, res_class, timeout=30):
        import time
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            windows = self.top_windows(res_class)
            if windows:
                return windows
            time.sleep(0.1)
        raise Failure(f'no viewable {res_class} window on the private display')

    def focus(self, xid):
        self.x11.XSetInputFocus(self.display, xid, 1, 0)  # 1 = RevertToPointerRoot
        self.x11.XSync(self.display, False)

    def raise_window(self, xid):
        self.x11.XRaiseWindow(self.display, xid)
        self.x11.XSync(self.display, False)

    def move_window(self, xid, x, y):
        self.x11.XMoveWindow(self.display, xid, x, y)
        self.x11.XSync(self.display, False)

    def motion(self, x, y):
        check(0 <= x < 32768 and 0 <= y < 32768, 'in-screen motion')
        check(self.xtst.XTestFakeMotionEvent(self.display, self.screen, x, y, 0), 'XTEST motion')
        self.x11.XSync(self.display, False)

    def button(self, down):
        check(self.xtst.XTestFakeButtonEvent(self.display, 1, down, 0), 'XTEST button')
        self.x11.XSync(self.display, False)

    def key(self, keycode, down):
        check(0 < keycode < 256, 'keycode range')
        check(self.xtst.XTestFakeKeyEvent(self.display, keycode, down, 0), 'XTEST key')
        self.x11.XSync(self.display, False)

    def drag_path(self, start, end, steps=25, press=False):
        """Move along a path; the caller normally holds the button across the
        whole drag. press=True re-asserts the press for apps whose drag
        tracking loses the initial synthetic press across a focus change."""
        self.motion(*start)
        if press:
            self.button(True)
        for index in range(1, steps + 1):
            x = start[0] + (end[0] - start[0]) * index // steps
            y = start[1] + (end[1] - start[1]) * index // steps
            self.motion(x, y)
            import time
            time.sleep(0.01)

    def release(self):
        self.button(False)

    def close(self):
        if getattr(self, 'display', None):
            self.x11.XCloseDisplay(self.display)
            self.display = None
