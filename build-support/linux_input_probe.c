#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <poll.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int last_error;
static int xi_opcode;

static int record_error(Display *display, XErrorEvent *event) {
    (void)display;
    last_error = event->error_code;
    return 0;
}

static void require(bool condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "Probe failed: %s\n", message);
        exit(1);
    }
}

static void require_isolated(void) {
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

static long milliseconds(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

struct Counts {
    int core_keys;
    int xi_keys;
    int raw_keys;
    int source;
};

static struct Counts collect(Display *display, int duration) {
    struct Counts counts = {0};
    long deadline = milliseconds() + duration;
    for (;;) {
        while (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == KeyPress || event.type == KeyRelease)
                counts.core_keys++;
            if (event.type != GenericEvent || event.xcookie.extension != xi_opcode
                    || !XGetEventData(display, &event.xcookie))
                continue;
            int type = event.xcookie.evtype;
            if (type == XI_KeyPress || type == XI_KeyRelease) {
                XIDeviceEvent *key = event.xcookie.data;
                counts.xi_keys++;
                counts.source = key->sourceid;
            }
            if (type == XI_RawKeyPress || type == XI_RawKeyRelease) {
                XIRawEvent *key = event.xcookie.data;
                counts.raw_keys++;
                counts.source = key->sourceid;
            }
            XFreeEventData(display, &event.xcookie);
        }
        int remaining = (int)(deadline - milliseconds());
        if (remaining <= 0)
            return counts;
        struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
        require(poll(&fd, 1, remaining) >= 0, "X11 event poll");
    }
}

static XIEventMask keyboard_mask(int deviceid, unsigned char *bits) {
    memset(bits, 0, XIMaskLen(XI_LASTEVENT));
    XISetMask(bits, XI_KeyPress);
    XISetMask(bits, XI_KeyRelease);
    return (XIEventMask){deviceid, XIMaskLen(XI_LASTEVENT), bits};
}

static void key(Display *display, unsigned int code, bool pressed) {
    require(XTestFakeKeyEvent(display, code, pressed, CurrentTime), "XTEST request");
    XSync(display, False);
    require(!last_error, "XTEST server response");
}

static int find_device(Display *display, const char *name) {
    int count;
    int found = 0;
    XIDeviceInfo *devices = XIQueryDevice(display, XIAllDevices, &count);
    for (int index = 0; index < count; index++) {
        if (strcmp(devices[index].name, name) == 0)
            found = devices[index].deviceid;
    }
    XIFreeDeviceInfo(devices);
    return found;
}

static Window prepare_target(Display *target, Window root) {
    /* Metacity may reclaim focus from an unmanaged window after initial input. */
    Window window = XCreateSimpleWindow(target, root, 100, 100, 400, 300, 0, 0, 0);
    XSelectInput(target, window, KeyPressMask | KeyReleaseMask | ButtonPressMask
                 | ButtonReleaseMask | StructureNotifyMask);
    XMapWindow(target, window);
    XFlush(target);
    long deadline = milliseconds() + 2000;
    for (;;) {
        XWindowAttributes attributes;
        require(XGetWindowAttributes(target, window, &attributes), "query managed target mapping");
        if (attributes.map_state == IsViewable) return window;
        int remaining = (int)(deadline - milliseconds());
        require(remaining > 0, "managed target mapping deadline");
        if (XPending(target)) {
            XEvent event;
            XNextEvent(target, &event);
            continue;
        }
        struct pollfd fd = {ConnectionNumber(target), POLLIN, 0};
        require(poll(&fd, 1, remaining) >= 0, "managed target mapping poll");
    }
}

static void require_focus_ready(Display *inject, Window window) {
    /* Metacity applies its own focus shortly after mapping; wait until that
       settles, then take explicit focus and require the server to confirm it. */
    Window previous = None;
    long settled_at = -1;
    long deadline = milliseconds() + 2000;
    for (;;) {
        Window current;
        int revert;
        require(XGetInputFocus(inject, &current, &revert), "query window manager focus");
        if (current != None && current == previous) {
            if (settled_at < 0)
                settled_at = milliseconds();
            if (milliseconds() - settled_at >= 20)
                break;
        } else {
            settled_at = -1;
        }
        previous = current;
        require(milliseconds() < deadline, "window manager focus settling deadline");
        if (XPending(inject)) {
            XEvent event;
            XNextEvent(inject, &event);
            continue;
        }
        struct pollfd fd = {ConnectionNumber(inject), POLLIN, 0};
        require(poll(&fd, 1, 20) >= 0, "window manager focus settling poll");
    }
    XSetInputFocus(inject, window, RevertToParent, CurrentTime);
    XFlush(inject);
    long focus_deadline = milliseconds() + 2000;
    for (;;) {
        Window focus;
        int revert;
        require(XGetInputFocus(inject, &focus, &revert), "confirm managed focus");
        if (focus == window)
            return;
        require(milliseconds() < focus_deadline, "managed focus readiness deadline");
        if (XPending(inject)) {
            XEvent event;
            XNextEvent(inject, &event);
            continue;
        }
        struct pollfd fd = {ConnectionNumber(inject), POLLIN, 0};
        require(poll(&fd, 1, 20) >= 0, "managed focus readiness poll");
    }
}

int main(void) {
    require_isolated();
    XSetErrorHandler(record_error);
    Display *capture = XOpenDisplay(NULL);
    Display *target = XOpenDisplay(NULL);
    Display *inject = XOpenDisplay(NULL);
    require(capture && target && inject, "connect only to private X server");
    int event_base, error_base;
    require(XQueryExtension(capture, "XInputExtension", &xi_opcode, &event_base, &error_base), "XI2");
    int major = 2, minor = 2;
    require(XIQueryVersion(capture, &major, &minor) == Success, "XI2 version");
    Window root = DefaultRootWindow(capture);
    Window window = prepare_target(target, root);
    require_focus_ready(inject, window);
    XTestFakeMotionEvent(inject, DefaultScreen(inject), 200, 200, CurrentTime);
    XSync(inject, False);
    int pointer;
    require(XIGetClientPointer(inject, None, &pointer), "core client pointer");
    int count;
    XIDeviceInfo *device = XIQueryDevice(inject, pointer, &count);
    require(device && count == 1, "core master pointer");
    int keyboard = device->attachment;
    XIFreeDeviceInfo(device);
    unsigned char raw[XIMaskLen(XI_LASTEVENT)] = {0};
    XISetMask(raw, XI_RawKeyPress);
    XISetMask(raw, XI_RawKeyRelease);
    XIEventMask raw_mask = {XIAllMasterDevices, sizeof(raw), raw};
    XISelectEvents(capture, root, &raw_mask, 1);
    XSync(capture, False);
    unsigned int code = XKeysymToKeycode(inject, XK_F12);
    require(code != 0, "fixture key exists");
    key(inject, code, true);
    key(inject, code, false);
    struct Counts baseline = collect(target, 100);
    struct Counts baseline_capture = collect(capture, 100);
    require(baseline.core_keys == 2 && baseline_capture.raw_keys == 2, "native baseline delivery");
    printf("baseline: core_keys=%d raw_keys=%d source=%d\n",
           baseline.core_keys, baseline_capture.raw_keys, baseline_capture.source);

    unsigned char bits[XIMaskLen(XI_LASTEVENT)];
    XIEventMask mask = keyboard_mask(keyboard, bits);
    XIGrabModifiers modifier = {XIAnyModifier, 0};
    XIGrabKeycode(capture, keyboard, code, root, XIGrabModeSync, XIGrabModeAsync,
                 False, &mask, 1, &modifier);
    XSync(capture, False);
    require(!last_error && modifier.status == GrabSuccess, "synchronous passive key grab");
    key(inject, code, true);
    struct Counts held = collect(capture, 100);
    struct Counts before = collect(target, 100);
    require(held.xi_keys == 1 && before.core_keys == 0, "synchronous interception blocks native delivery");
    XIAllowEvents(capture, keyboard, XIReplayDevice, CurrentTime);
    XSync(capture, False);
    key(inject, code, false);
    struct Counts replayed = collect(target, 100);
    require(replayed.core_keys == 2, "replay preserves the original press/release");
    printf("passive_replay: intercepted=%d before=%d after=%d\n",
           held.xi_keys, before.core_keys, replayed.core_keys);
    XIUngrabKeycode(capture, keyboard, code, root, 1, &modifier);
    XSync(capture, False);
    collect(capture, 20);

    int grab = XIGrabDevice(capture, keyboard, root, CurrentTime, None,
                           XIGrabModeAsync, XIGrabModeAsync, False, &mask);
    require(grab == GrabSuccess, "active remote keyboard grab");
    key(inject, code, true);
    key(inject, code, false);
    struct Counts swallowed = collect(capture, 100);
    struct Counts local = collect(target, 100);
    require(swallowed.xi_keys == 2 && local.core_keys == 0, "active grab truly swallows local delivery");
    XIUngrabDevice(capture, keyboard, CurrentTime);
    XSync(capture, False);
    key(inject, code, true);
    key(inject, code, false);
    struct Counts restored = collect(target, 100);
    require(restored.core_keys == 2, "ungrab restores native delivery");
    printf("remote_swallow: captured=%d local=%d restored=%d\n",
           swallowed.xi_keys, local.core_keys, restored.core_keys);
    collect(capture, 20);

    int native_grab = XGrabPointer(target, window, False, ButtonPressMask | ButtonReleaseMask,
                                  GrabModeAsync, GrabModeAsync, None, None, CurrentTime);
    require(native_grab == GrabSuccess, "fixture native DnD pointer grab");
    unsigned char pointer_bits[XIMaskLen(XI_LASTEVENT)] = {0};
    XISetMask(pointer_bits, XI_Motion);
    XISetMask(pointer_bits, XI_ButtonPress);
    XISetMask(pointer_bits, XI_ButtonRelease);
    XIEventMask pointer_mask = {pointer, sizeof(pointer_bits), pointer_bits};
    int during_drag = XIGrabDevice(capture, pointer, root, CurrentTime, None,
                                  XIGrabModeAsync, XIGrabModeAsync, False, &pointer_mask);
    printf("native_drag: concurrent_remote_grab_status=%d (AlreadyGrabbed=%d)\n",
           during_drag, AlreadyGrabbed);
    require(during_drag == AlreadyGrabbed, "native drag grab remains owned by its source");
    XUngrabPointer(target, CurrentTime);
    XSync(target, False);

    XIAnyHierarchyChangeInfo add = {.add = {XIAddMaster, "Oink-Probe", True, True}};
    XIChangeHierarchy(capture, &add, 1);
    XSync(capture, False);
    require(!last_error, "create private injection master");
    int own_pointer = find_device(capture, "Oink-Probe pointer");
    int own_keyboard = find_device(capture, "Oink-Probe keyboard");
    int own_xtest = find_device(capture, "Oink-Probe XTEST keyboard");
    require(own_pointer && own_keyboard && own_xtest, "private injection devices exist");
    XISetClientPointer(inject, None, own_pointer);
    XISetFocus(inject, own_keyboard, window, CurrentTime);
    XSync(inject, False);
    collect(capture, 20);
    key(inject, code, true);
    key(inject, code, false);
    struct Counts own_core = collect(target, 100);
    struct Counts own_raw = collect(capture, 100);
    printf("dedicated_master: core_keys=%d raw_keys=%d source=%d expected_source=%d\n",
           own_core.core_keys, own_raw.raw_keys, own_raw.source, own_xtest);
    require(own_core.core_keys == 2 && own_raw.raw_keys == 2,
            "core target receives both dedicated-master key transitions");
    require(own_raw.source == own_xtest && own_raw.source != baseline_capture.source,
            "dedicated master identifies its own injection source");

    XSelectInput(target, window, 0);
    mask = keyboard_mask(XIAllMasterDevices, bits);
    XISelectEvents(target, window, &mask, 1);
    XSync(target, False);
    key(inject, code, true);
    key(inject, code, false);
    struct Counts own_xi = collect(target, 100);
    printf("dedicated_master_xi2: keys=%d source=%d\n", own_xi.xi_keys, own_xi.source);
    require(own_xi.xi_keys == 2, "XI2 target receives private-master injection");

    XIAnyHierarchyChangeInfo attach = {.attach = {XIAttachSlave, own_xtest, keyboard}};
    XIChangeHierarchy(capture, &attach, 1);
    XSync(capture, False);
    printf("reattach_private_xtest: x_error=%d\n", last_error);
    last_error = 0;
    XISetClientPointer(inject, None, pointer);
    XIAnyHierarchyChangeInfo remove = {.remove = {XIRemoveMaster, own_pointer,
                                                  XIAttachToMaster, pointer, keyboard}};
    XIChangeHierarchy(capture, &remove, 1);
    XSync(capture, False);
    require(!last_error, "remove only owned private master");
    XDestroyWindow(target, window);
    XCloseDisplay(inject);
    XCloseDisplay(target);
    XCloseDisplay(capture);
    printf("Mechanism probe complete; native toolkit drag and physical HID tests remain required.\n");
    return 0;
}
