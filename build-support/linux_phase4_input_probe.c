/* Phase 4 input semantics probe. Runs only inside the private Xephyr harness. */
#define _POSIX_C_SOURCE 200809L
#include <X11/XKBlib.h>
#include <X11/Xlib.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <X11/extensions/Xfixes.h>
#include <X11/keysym.h>
#include <poll.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int xi_opcode;
static int x_errors;

static int on_x_error(Display *display, XErrorEvent *event) {
    (void)display;
    (void)event;
    x_errors++;
    return 0;
}

static long now_ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static void require(bool condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "FAIL %s\n", message);
        exit(2);
    }
}

static Display *open_conn(void) {
    Display *display = XOpenDisplay(NULL);
    require(display != NULL, "open DISPLAY");
    int event, error, major = 2, minor = 2;
    require(XQueryExtension(display, "XInputExtension", &xi_opcode, &event, &error), "XInput2");
    require(XIQueryVersion(display, &major, &minor) == Success, "XI2 version");
    return display;
}

static void xi_select(Display *display, Window window, int device, const int *types, int count) {
    XIEventMask mask = {0};
    unsigned char bits[XIMaskLen(XI_LASTEVENT)] = {0};
    for (int i = 0; i < count; i++)
        XISetMask(bits, types[i]);
    mask.deviceid = device;
    mask.mask_len = sizeof(bits);
    mask.mask = bits;
    XISelectEvents(display, window, &mask, 1);
    XSync(display, False);
}

static Window prepare_target(Display *target, Window root) {
    Window window = XCreateSimpleWindow(target, root, 100, 100, 400, 300, 0, 0, 0);
    XSelectInput(target, window,
                 KeyPressMask | KeyReleaseMask | ButtonPressMask | ButtonReleaseMask | ExposureMask);
    XMapWindow(target, window);
    XFlush(target);
    long deadline = now_ms() + 2000;
    for (;;) {
        XWindowAttributes attributes;
        require(XGetWindowAttributes(target, window, &attributes), "query target mapping");
        if (attributes.map_state == IsViewable)
            return window;
        require(now_ms() < deadline, "target mapping deadline");
        if (XPending(target)) {
            XEvent event;
            XNextEvent(target, &event);
        } else {
            struct pollfd fd = {ConnectionNumber(target), POLLIN, 0};
            require(poll(&fd, 1, 20) >= 0, "target mapping poll");
        }
    }
}

static void require_focus(Display *display, Window window) {
    Window previous = None;
    long settled = -1;
    long deadline = now_ms() + 2000;
    for (;;) {
        Window current;
        int revert;
        require(XGetInputFocus(display, &current, &revert), "query focus");
        if (current != None && current == previous) {
            if (settled < 0)
                settled = now_ms();
            if (now_ms() - settled >= 20)
                break;
        } else {
            settled = -1;
        }
        previous = current;
        require(now_ms() < deadline, "focus settling deadline");
        if (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
        } else {
            struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
            require(poll(&fd, 1, 20) >= 0, "focus settling poll");
        }
    }
    XSetInputFocus(display, window, RevertToParent, CurrentTime);
    XFlush(display);
    deadline = now_ms() + 2000;
    for (;;) {
        Window focus;
        int revert;
        require(XGetInputFocus(display, &focus, &revert), "confirm focus");
        if (focus == window)
            return;
        require(now_ms() < deadline, "focus readiness deadline");
        if (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
        } else {
            struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
            require(poll(&fd, 1, 20) >= 0, "focus readiness poll");
        }
    }
}

static unsigned find_device(Display *display, const char *name, int use) {
    int count = 0;
    XIDeviceInfo *devices = XIQueryDevice(display, XIAllDevices, &count);
    unsigned found = 0;
    for (int i = 0; i < count; i++) {
        if (strcmp(devices[i].name, name) == 0 && (use < 0 || devices[i].use == use)) {
            found = devices[i].deviceid;
            break;
        }
    }
    XIFreeDeviceInfo(devices);
    return found;
}

typedef struct {
    int xi_press, xi_release, raw_press, raw_release, raw_button, hierarchy;
    unsigned last_source;
    int last_detail;
} Counts;

static Counts collect(Display *display, int duration_ms) {
    Counts counts = {0};
    long deadline = now_ms() + duration_ms;
    for (;;) {
        while (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type != GenericEvent || event.xcookie.extension != xi_opcode
                || !XGetEventData(display, &event.xcookie))
                continue;
            int type = event.xcookie.evtype;
            if (type == XI_KeyPress || type == XI_KeyRelease || type == XI_ButtonPress
                || type == XI_ButtonRelease) {
                XIDeviceEvent *device = event.xcookie.data;
                if (type == XI_KeyPress)
                    counts.xi_press++;
                else if (type == XI_KeyRelease)
                    counts.xi_release++;
                counts.last_source = device->sourceid;
                counts.last_detail = device->detail;
            } else if (type == XI_RawKeyPress || type == XI_RawKeyRelease
                       || type == XI_RawButtonPress || type == XI_RawButtonRelease) {
                XIRawEvent *raw = event.xcookie.data;
                if (type == XI_RawKeyPress)
                    counts.raw_press++;
                else if (type == XI_RawKeyRelease)
                    counts.raw_release++;
                else if (type == XI_RawButtonPress)
                    counts.raw_button++;
                counts.last_source = raw->sourceid;
                counts.last_detail = raw->detail;
            } else if (type == XI_HierarchyChanged) {
                counts.hierarchy++;
            }
            XFreeEventData(display, &event.xcookie);
        }
        int remaining = (int)(deadline - now_ms());
        if (remaining <= 0)
            return counts;
        struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
        require(poll(&fd, 1, remaining) >= 0, "XI poll");
    }
}

typedef struct { int press, release, button; } CoreCounts;

static CoreCounts collect_core(Display *display, int duration_ms) {
    CoreCounts counts = {0};
    long deadline = now_ms() + duration_ms;
    for (;;) {
        while (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == KeyPress)
                counts.press++;
            else if (event.type == KeyRelease)
                counts.release++;
            else if (event.type == ButtonPress)
                counts.button++;
        }
        int remaining = (int)(deadline - now_ms());
        if (remaining <= 0)
            return counts;
        struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
        require(poll(&fd, 1, remaining) >= 0, "core poll");
    }
}

static int wait_hierarchy(Display *display, int timeout_ms) {
    long deadline = now_ms() + timeout_ms;
    for (;;) {
        while (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type != GenericEvent || event.xcookie.extension != xi_opcode
                || !XGetEventData(display, &event.xcookie))
                continue;
            int type = event.xcookie.evtype;
            XFreeEventData(display, &event.xcookie);
            if (type == XI_HierarchyChanged)
                return 1;
        }
        int remaining = (int)(deadline - now_ms());
        if (remaining <= 0)
            return 0;
        struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
        require(poll(&fd, 1, remaining) >= 0, "hierarchy poll");
    }
}

static void key(Display *display, unsigned code, int down) {
    require(XTestFakeKeyEvent(display, code, down, CurrentTime), "XTEST key request");
    XSync(display, False);
    require(!x_errors, "XTEST key response");
}

int main(void) {
    XSetErrorHandler(on_x_error);
    Display *target = open_conn();
    Display *inject = open_conn();
    Display *observer = open_conn();
    Window root = DefaultRootWindow(target);
    Window app = prepare_target(target, root);
    require_focus(inject, app);
    unsigned master_pointer = find_device(observer, "Virtual core pointer", XIMasterPointer);
    unsigned master_keyboard = find_device(observer, "Virtual core keyboard", XIMasterKeyboard);
    unsigned xtest_keyboard = find_device(observer, "Virtual core XTEST keyboard", -1);
    require(master_pointer && master_keyboard && xtest_keyboard, "identify core devices");
    unsigned key_a = XKeysymToKeycode(inject, XK_a);
    unsigned key_c = XKeysymToKeycode(inject, XK_c);
    unsigned key_f9 = XKeysymToKeycode(inject, XK_F12);
    unsigned key_f10 = XKeysymToKeycode(inject, XK_F10);
    require(key_a && key_c && key_f9 && key_f10, "fixture keycodes");
    int failures = 0;

    const int raw_types[] = {XI_RawKeyPress, XI_RawKeyRelease, XI_RawButtonPress,
                             XI_RawButtonRelease, XI_KeyPress, XI_KeyRelease};
    xi_select(observer, root, XIAllMasterDevices, raw_types, 6);
    const int key_types[] = {XI_KeyPress, XI_KeyRelease};
    XIEventMask key_mask = {0};
    unsigned char key_bits[XIMaskLen(XI_LASTEVENT)] = {0};
    XISetMask(key_bits, XI_KeyPress);
    XISetMask(key_bits, XI_KeyRelease);
    key_mask.deviceid = (int)master_keyboard;
    key_mask.mask_len = sizeof(key_bits);
    key_mask.mask = key_bits;

    key(inject, key_a, 1);
    key(inject, key_a, 0);
    CoreCounts app_keys = collect_core(target, 200);
    Counts observed = collect(observer, 200);
    int observation_ok = app_keys.press > 0 && app_keys.release > 0 && observed.raw_press > 0
                         && observed.last_source == xtest_keyboard
                         && observed.last_detail == (int)key_a;
    printf("passive_observation=%s app_press=%d app_release=%d raw_press=%d sourceid=%u "
           "expected=%u detail=%d\n", observation_ok ? "ok" : "fail", app_keys.press,
           app_keys.release, observed.raw_press, observed.last_source, xtest_keyboard,
           observed.last_detail);
    failures += !observation_ok;

    Display *grab = open_conn();
    xi_select(grab, root, (int)master_keyboard, key_types, 2);
    XIGrabModifiers modifiers = {.modifiers = XIAnyModifier, .status = 0};
    Status passive = XIGrabKeycode(grab, (int)master_keyboard, (int)key_f9, root,
                                   XIGrabModeSync, XIGrabModeAsync, False, &key_mask, 1,
                                   &modifiers);
    XSync(grab, False);
    require(!x_errors && modifiers.status == GrabSuccess, "passive key grab");
    key(inject, key_f9, 1);
    Counts held = collect(grab, 120);
    CoreCounts blocked = collect_core(target, 120);
    Status replay_status = XIAllowEvents(grab, (int)master_keyboard, XIReplayDevice, CurrentTime);
    XSync(grab, False);
    key(inject, key_f9, 0);
    CoreCounts replayed = collect_core(target, 250);
    Counts replayed_observer = collect(observer, 100);
    Window focus_after = None;
    int revert_after = 0;
    XGetInputFocus(inject, &focus_after, &revert_after);
    int replay_ok = passive == GrabSuccess && held.xi_press > 0 && blocked.press == 0
                    && replayed.press > 0 && replayed.release > 0;
    printf("passive_grab_replay=%s status=%d allow=%d held=%d blocked=%d replayed_press=%d "
           "replayed_release=%d observer_xi=%d focus=%lu app=%lu\n", replay_ok ? "ok" : "fail",
           (int)passive, (int)replay_status, held.xi_press, blocked.press, replayed.press,
           replayed.release, replayed_observer.xi_press, (unsigned long)focus_after,
           (unsigned long)app);
    failures += !replay_ok;

    XIGrabModifiers ungrab = {.modifiers = XIAnyModifier, .status = 0};
    XIUngrabKeycode(grab, (int)master_keyboard, (int)key_f9, root, 1, &ungrab);
    XSync(grab, False);
    XIGrabModifiers swallow_mods = {.modifiers = XIAnyModifier, .status = 0};
    passive = XIGrabKeycode(grab, (int)master_keyboard, (int)key_f9, root, XIGrabModeSync,
                            XIGrabModeAsync, False, &key_mask, 1, &swallow_mods);
    XSync(grab, False);
    key(inject, key_f9, 1);
    Counts swallow_held = collect(grab, 120);
    XIAllowEvents(grab, (int)master_keyboard, XIAsyncDevice, CurrentTime);
    XSync(grab, False);
    key(inject, key_f9, 0);
    Counts swallow_release = collect(grab, 120);
    XIAllowEvents(grab, (int)master_keyboard, XIAsyncDevice, CurrentTime);
    XSync(grab, False);
    CoreCounts swallowed = collect_core(target, 200);
    int swallow_ok = passive == GrabSuccess && swallow_held.xi_press > 0
                     && swallow_release.xi_release > 0 && swallowed.press == 0;
    printf("passive_grab_swallow=%s held=%d released=%d local=%d\n",
           swallow_ok ? "ok" : "fail", swallow_held.xi_press, swallow_release.xi_release,
           swallowed.press);
    failures += !swallow_ok;
    XIUngrabKeycode(grab, (int)master_keyboard, (int)key_f9, root, 1, &ungrab);
    XSync(grab, False);
    require_focus(inject, app);

    require(!x_errors, "clean X state before active grab");
    Status active = XIGrabDevice(grab, (int)master_keyboard, root, CurrentTime, None,
                                 XIGrabModeAsync, XIGrabModeAsync, False, &key_mask);
    XSync(grab, False);
    Display *competitor = open_conn();
    Status competing = XIGrabDevice(competitor, (int)master_keyboard, root, CurrentTime, None,
                                    XIGrabModeAsync, XIGrabModeAsync, False, &key_mask);
    int competing_core = XGrabKeyboard(competitor, root, False, GrabModeAsync, GrabModeAsync,
                                       CurrentTime);
    key(inject, key_f10, 1);
    key(inject, key_f10, 0);
    Counts active_capture = collect(grab, 200);
    CoreCounts active_local = collect_core(target, 200);
    XIUngrabDevice(grab, (int)master_keyboard, CurrentTime);
    XSync(grab, False);
    require_focus(inject, app);
    key(inject, key_f10, 1);
    key(inject, key_f10, 0);
    CoreCounts restored = collect_core(target, 250);
    int active_ok = active == GrabSuccess && active_capture.xi_press > 0
                    && active_capture.xi_release > 0 && active_local.press == 0
                    && competing == AlreadyGrabbed && competing_core == AlreadyGrabbed
                    && restored.press > 0 && restored.release > 0;
    printf("active_grab=%s status=%d press=%d release=%d local=%d competing=%d "
           "competing_core=%d restored=%d\n", active_ok ? "ok" : "fail", (int)active,
           active_capture.xi_press, active_capture.xi_release, active_local.press,
           (int)competing, competing_core, restored.press);
    failures += !active_ok;

    Bool detectable = False;
    Bool repeat_supported = XkbSetDetectableAutoRepeat(observer, True, &detectable);
    XSync(observer, False);
    key(inject, key_c, 1);
    Counts repeated = collect(observer, 1200);
    key(inject, key_c, 0);
    Counts repeat_end = collect(observer, 300);
    int repeat_ok = repeat_supported && detectable
                    && (repeated.xi_press + repeated.raw_press > 1)
                    && repeat_end.xi_release + repeat_end.raw_release > 0;
    printf("xkb_repeat=%s supported=%d detectable=%d presses=%d raw_presses=%d "
           "releases_after_up=%d\n", repeat_ok ? "ok" : "fail", repeat_supported,
           detectable, repeated.xi_press, repeated.raw_press,
           repeat_end.xi_release + repeat_end.raw_release);
    failures += !repeat_ok;

    XTestFakeButtonEvent(inject, 4, True, CurrentTime);
    XTestFakeButtonEvent(inject, 4, False, CurrentTime);
    XSync(inject, False);
    Counts wheel = collect(observer, 300);
    int scroll_classes = 0;
    int device_count = 0;
    XIDeviceInfo *pointer = XIQueryDevice(observer, (int)master_pointer, &device_count);
    for (int i = 0; i < device_count; i++)
        for (int c = 0; c < pointer[i].num_classes; c++)
            if (pointer[i].classes[c]->type == XIScrollClass)
                scroll_classes++;
    XIFreeDeviceInfo(pointer);
    int wheel_ok = wheel.raw_button > 0 && wheel.last_detail == 4;
    printf("scroll_valuators=%d wheel=%s detail=%d\n", scroll_classes,
           wheel_ok ? "ok" : "fail", wheel.last_detail);
    failures += !wheel_ok;

    int hierarchy_types[] = {XI_HierarchyChanged};
    xi_select(observer, root, XIAllDevices, hierarchy_types, 1);
    char master_name[64];
    snprintf(master_name, sizeof(master_name), "Oink-Probe-%ld", (long)getpid());
    XIAnyHierarchyChangeInfo add = {.add = {XIAddMaster, master_name, True, True}};
    XIChangeHierarchy(grab, &add, 1);
    XSync(grab, False);
    int added = wait_hierarchy(observer, 2000);
    char pointer_name[96];
    snprintf(pointer_name, sizeof(pointer_name), "%s pointer", master_name);
    char keyboard_name[96];
    snprintf(keyboard_name, sizeof(keyboard_name), "%s keyboard", master_name);
    unsigned own_pointer = find_device(observer, pointer_name, -1);
    unsigned own_keyboard = find_device(observer, keyboard_name, -1);
    int hotplug_ok = !x_errors && added && own_pointer && own_keyboard;
    if (own_pointer && own_keyboard) {
        XIAnyHierarchyChangeInfo remove = {
            .remove = {XIRemoveMaster, own_pointer, XIAttachToMaster, (int)master_pointer,
                       (int)master_keyboard}};
        XIChangeHierarchy(grab, &remove, 1);
        XSync(grab, False);
    }
    printf("hotplug=%s event=%d pointer=%u keyboard=%u\n", hotplug_ok ? "ok" : "fail", added,
           own_pointer, own_keyboard);
    failures += !hotplug_ok;

    int fixes_event = 0, fixes_error = 0;
    int have_fixes = XFixesQueryExtension(target, &fixes_event, &fixes_error);
    int errors_before = x_errors;
    Window root_return, child_return;
    int root_x = 0, root_y = 0, win_x = 0, win_y = 0;
    unsigned mask = 0;
    XGrabServer(target);
    XFixesHideCursor(target, root);
    XWarpPointer(target, None, root, 0, 0, 0, 0, 321, 222);
    XSync(target, False);
    XQueryPointer(target, root, &root_return, &child_return, &root_x, &root_y, &win_x, &win_y,
                  &mask);
    XFixesShowCursor(target, root);
    XUngrabServer(target);
    XSync(target, False);
    int cursor_ok = have_fixes && x_errors == errors_before && root_x == 321 && root_y == 222;
    printf("cursor_hide_restore=%s fixes=%d warp=%d,%d xerrors=%d\n",
           cursor_ok ? "ok" : "fail", have_fixes, root_x, root_y, x_errors - errors_before);
    failures += !cursor_ok;

    printf("phase4_input_probe result=%s failures=%d\n", failures ? "fail" : "pass", failures);
    return failures ? 1 : 0;
}
