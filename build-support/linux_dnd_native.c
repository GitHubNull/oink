#define _POSIX_C_SOURCE 200809L
#include "linux_dnd_native.h"
#include <sys/stat.h>

Display *inject, *observer;
static int opcode, pointer_id, keyboard_id, pointer_xtest, keyboard_xtest;
static bool button_held;

/* Kept equivalent to linux_dnd_probe.c; called before GTK and ANY X connection. */
void require_isolated(void) {
    const char *root = getenv("OINK_NATIVE_TEST_ROOT");
    const char *authority = getenv("XAUTHORITY");
    const char *display = getenv("DISPLAY");
    char expected[4096];
    struct stat st;
    require(root && strncmp(root, "/tmp/oink-native-", 17) == 0,
            "use linux-native-test.py, never the host session");
    require(strlen(root) + 32 < sizeof(expected), "invalid private root");
    require(lstat(root, &st) == 0 && S_ISDIR(st.st_mode) && st.st_uid == getuid()
            && (st.st_mode & 077) == 0, "private root must be owned and inaccessible to others");
    snprintf(expected, sizeof(expected), "%s/Xauthority", root);
    require(authority && strcmp(authority, expected) == 0,
            "Xauthority must belong to the isolated environment");
    require(lstat(authority, &st) == 0 && S_ISREG(st.st_mode) && st.st_uid == getuid()
            && (st.st_mode & 077) == 0, "private authority must be an owned private file");
    require(display && display[0] == ':' && strcmp(display, ":0") != 0
            && strcmp(display, ":0.0") != 0, "refusing host or remote DISPLAY");
    snprintf(expected, sizeof(expected), "unix:%s/runtime/pulse-native", root);
    require(getenv("PULSE_SERVER") && strcmp(getenv("PULSE_SERVER"), expected) == 0,
            "missing private audio environment");
}

void motion(int x, int y) {
    require(is_source && x >= 0 && y >= 0 && x < DisplayWidth(inject, DefaultScreen(inject))
            && y < DisplayHeight(inject, DefaultScreen(inject)), "private in-screen motion");
    require(XTestFakeMotionEvent(inject, DefaultScreen(inject), x, y, CurrentTime), "XTEST motion");
    XSync(inject, False);
}

void button(bool down) {
    require(button_held != down, "no duplicate synthetic button transition");
    require(XTestFakeButtonEvent(inject, 1, down, CurrentTime), "XTEST button");
    button_held = down;
    if (down) r.down++; else r.up++;
    XSync(inject, False);
}

void escape_key(void) {
    unsigned int code = XKeysymToKeycode(inject, XK_Escape);
    require(code && XTestFakeKeyEvent(inject, code, True, CurrentTime)
            && XTestFakeKeyEvent(inject, code, False, CurrentTime), "private XTEST Escape");
    XSync(inject, False);
}

static int try_grab(int device, bool keyboard) {
    unsigned char bits[XIMaskLen(XI_LASTEVENT)] = {0};
    XISetMask(bits, keyboard ? XI_KeyPress : XI_ButtonPress);
    XIEventMask mask = {device, sizeof(bits), bits};
    int result = XIGrabDevice(observer, device, DefaultRootWindow(observer), CurrentTime,
                             None, XIGrabModeAsync, XIGrabModeAsync, False, &mask);
    if (result == GrabSuccess) XIUngrabDevice(observer, device, CurrentTime);
    XSync(observer, False);
    return result;
}

void native_poll(void) {
    while (XPending(observer)) {
        XEvent event;
        XNextEvent(observer, &event);
        if (event.type != GenericEvent || event.xcookie.extension != opcode
                || !XGetEventData(observer, &event.xcookie)) continue;
        XIRawEvent *raw = event.xcookie.data;
        int type = event.xcookie.evtype;
        if (type == XI_RawButtonPress || type == XI_RawButtonRelease) {
            require(raw->sourceid == pointer_xtest && raw->detail == 1,
                    "only private core XTEST button input is allowed");
            if (type == XI_RawButtonPress) r.raw_down++; else r.raw_up++;
        } else if (type == XI_RawKeyPress || type == XI_RawKeyRelease) {
            require(raw->sourceid == keyboard_xtest
                    && raw->detail == XKeysymToKeycode(inject, XK_Escape), "only private XTEST Escape");
            if (type == XI_RawKeyPress) r.key_down++; else r.key_up++;
        }
        XFreeEventData(observer, &event.xcookie);
    }
    Window root, child;
    int rx, ry, wx, wy;
    unsigned int mask;
    require(XQueryPointer(inject, DefaultRootWindow(inject), &root, &child,
                          &rx, &ry, &wx, &wy, &mask), "query private pointer");
    r.button = !!(mask & Button1Mask);
    r.owner = XGetSelectionOwner(inject, selection_atom);
    toolkit_poll();
}

void pointer_at_source(void) {
    Window root, child;
    int rx, ry, wx, wy;
    unsigned int mask;
    require(XQueryPointer(inject, DefaultRootWindow(inject), &root, &child,
                          &rx, &ry, &wx, &wy, &mask), "query private source position");
    require(rx == (int)r.x && ry == (int)r.y && (mask & Button1Mask), "returned with button held");
}

void held_context(unsigned long allowed_get) {
    native_poll();
    require(is_source && r.begin == 1 && r.end == 0 && r.failed == 0 && r.get == allowed_get
            && r.down == 1 && r.up == 0 && r.raw_down == 1 && r.raw_up == 0 && r.button,
            "one uninterrupted real source drag");
    toolkit_held(pointer_id);
    require(try_grab(pointer_id, false) == AlreadyGrabbed, "native grab cannot be stolen");
    require(r.owner == r.source_xid, "live XdndSelection owner");
    r.held_checks++;
}

void no_grab(void) {
    toolkit_released();
    require(try_grab(pointer_id, false) == GrabSuccess
            && try_grab(keyboard_id, true) == GrabSuccess, "no ghost pointer or keyboard grab");
    r.free_checks++;
}

void native_init(void) {
    inject = XOpenDisplay(NULL); observer = XOpenDisplay(NULL);
    require(inject && observer, "connect private X server");
    int event, error, major = 2, minor = 2;
    require(XTestQueryExtension(inject, &event, &error, &major, &minor), "XTEST extension");
    require(XQueryExtension(observer, "XInputExtension", &opcode, &event, &error), "XI2 extension");
    major = 2; minor = 2;
    require(XIQueryVersion(observer, &major, &minor) == Success
            && (major > 2 || (major == 2 && minor >= 2)), "XI2.2 raw events during grabs");
    int count;
    XIDeviceInfo *devices = XIQueryDevice(inject, XIAllDevices, &count);
    require(devices != NULL, "private device enumeration");
    for (int i = 0; i < count; i++) {
        if (!strcmp(devices[i].name, "Virtual core pointer") && devices[i].use == XIMasterPointer) {
            pointer_id = devices[i].deviceid; keyboard_id = devices[i].attachment;
        }
        if (!strcmp(devices[i].name, "Virtual core XTEST pointer")) pointer_xtest = devices[i].deviceid;
        if (!strcmp(devices[i].name, "Virtual core XTEST keyboard")) keyboard_xtest = devices[i].deviceid;
    }
    require(pointer_id && keyboard_id && pointer_xtest && keyboard_xtest, "private core XTEST devices");
    for (int i = 0; i < count; i++) {
        if (devices[i].deviceid == pointer_xtest)
            require(devices[i].attachment == pointer_id, "core XTEST pointer attachment");
        if (devices[i].deviceid == keyboard_xtest)
            require(devices[i].attachment == keyboard_id, "core XTEST keyboard attachment");
    }
    XIFreeDeviceInfo(devices);
    XISetClientPointer(inject, None, pointer_id);
    int selected;
    require(XIGetClientPointer(inject, None, &selected) && selected == pointer_id, "core test pointer");
    unsigned char bits[XIMaskLen(XI_LASTEVENT)] = {0};
    XISetMask(bits, XI_RawButtonPress); XISetMask(bits, XI_RawButtonRelease);
    XISetMask(bits, XI_RawKeyPress); XISetMask(bits, XI_RawKeyRelease);
    XIEventMask mask = {XIAllMasterDevices, sizeof(bits), bits};
    XISelectEvents(observer, DefaultRootWindow(observer), &mask, 1);
    XSync(observer, False);
}
