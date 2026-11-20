#define _POSIX_C_SOURCE 200809L
/* Dedicated-master input router for real-application toolkit delivery tests.
 *
 * Extracted from linux_input_matrix.c: creates an exclusive Oink-Toolkit-<pid>
 * master pair with its XTEST slaves, binds the caller-chosen application
 * window via XISetClientPointer + XISetFocus, then executes line-based
 * injection commands from stdin (keys resolved against the OWNED master
 * keyboard map, never a made-up mapping). Cleanup on EOF/exit releases held
 * keys/buttons, restores the client pointer and removes only our master.
 *
 * Synthetic private X11 only: refuses the host session, non-numeric displays,
 * missing private root/auth/audio, and pre-existing master names. Never
 * discovers a display, never falls back to :0, never uses a helper.
 */
#include <X11/Xlib.h>
#include <X11/XKBlib.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <X11/Xauth.h>
#include <fcntl.h>
#include <poll.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int xerror_code;
static int original_pointer, master_pointer, master_keyboard, xtest_pointer, xtest_keyboard;
static Display *inject, *verify;
static Window target_window;
static bool held_keys[256], held_buttons[256], cleaning;
static XkbDescPtr keyboard_map;
static unsigned active_group;

static int record_error(Display *display, XErrorEvent *event) {
    (void)display;
    xerror_code = event->error_code;
    return 0;
}
static void fail(const char *reason) {
    fprintf(stderr, "route: %s (xerror=%d)\n", reason, xerror_code);
    exit(1);
}
static void refuse(const char *reason) {
    fprintf(stderr, "route: refused: %s\n", reason);
    exit(2);
}
static void sync_require(const char *reason) {
    XSync(inject, False);
    if (xerror_code) fail(reason);
}
static void require_isolated(void) {
    const char *root = getenv("OINK_NATIVE_TEST_ROOT");
    const char *authority = getenv("XAUTHORITY"), *display = getenv("DISPLAY");
    struct stat st;
    char expected[4096];
    if (!(geteuid() == getuid() && getuid() != 0)) refuse("ordinary_user_required");
    if (!(root && strncmp(root, "/tmp/oink-native-", 17) == 0 && root[17]))
        refuse("use_linux_native_test_runner_never_host_session");
    if (strchr(root + 5, '/')) refuse("invalid_private_root");
    if (!(lstat(root, &st) == 0 && S_ISDIR(st.st_mode) && st.st_uid == getuid()
            && (st.st_mode & 077) == 0)) refuse("root_must_be_owned_private_directory");
    snprintf(expected, sizeof(expected), "%s/Xauthority", root);
    if (!(authority && strcmp(authority, expected) == 0)) refuse("authority_outside_private_root");
    if (!(lstat(authority, &st) == 0 && S_ISREG(st.st_mode) && st.st_uid == getuid()
            && (st.st_mode & 077) == 0)) refuse("authority_must_be_owned_private_file");
    if (!(display && display[0] == ':' && display[1] >= '1' && display[1] <= '9'))
        refuse("refuse_host_zero_remote_or_non_numeric_display");
    char *end;
    long number = strtol(display + 1, &end, 10);
    if (!(number > 0 && number < 65536 && (!*end || strcmp(end, ".0") == 0)))
        refuse("invalid_private_display_number");
    int fd = open(authority, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) refuse("cannot_open_private_authority");
    FILE *file = fdopen(fd, "rb");
    if (!file) refuse("cannot_read_private_authority");
    bool matching = false;
    Xauth *auth;
    while ((auth = XauReadAuth(file)) != NULL) {
        if (auth->family == FamilyLocal && auth->number_length == (unsigned)(end - display - 1)
                && memcmp(auth->number, display + 1, auth->number_length) == 0
                && auth->name_length == 18 && memcmp(auth->name, "MIT-MAGIC-COOKIE-1", 18) == 0
                && auth->data_length == 16) matching = true;
        XauDisposeAuth(auth);
    }
    fclose(file);
    if (!matching) refuse("display_not_registered_in_private_authority");
    snprintf(expected, sizeof(expected), "unix:%s/runtime/pulse-native", root);
    if (!(getenv("PULSE_SERVER") && strcmp(getenv("PULSE_SERVER"), expected) == 0))
        refuse("missing_private_audio_environment");
}

static int named_device(const char *name, int use, int attachment) {
    int count, found = 0;
    XIDeviceInfo *devices = XIQueryDevice(inject, XIAllDevices, &count);
    if (!devices) fail("query_private_devices");
    for (int i = 0; i < count; i++) {
        if (strcmp(name, devices[i].name) != 0) continue;
        if (found || devices[i].use != use || !devices[i].enabled
                || (attachment && devices[i].attachment != attachment)) {
            XIFreeDeviceInfo(devices);
            fail("owned_device_identity");
        }
        found = devices[i].deviceid;
    }
    XIFreeDeviceInfo(devices);
    return found;
}

/* Only directly reachable base-level symbols in the actual active group are
 * injectable without extra modifiers (same rule as the input matrix). */
static unsigned mapped_code(unsigned long symbol, unsigned *mask_out) {
    for (unsigned code = keyboard_map->min_key_code; code <= keyboard_map->max_key_code; code++) {
        if (active_group < (unsigned)XkbKeyNumGroups(keyboard_map, code)
                && XkbKeySymEntry(keyboard_map, code, 0, active_group) == symbol) {
            if (mask_out) *mask_out = keyboard_map->map->modmap[code];
            return code;
        }
    }
    if (mask_out) *mask_out = 0;
    return 0;
}

static void setup(Window window) {
    XSetErrorHandler(record_error);
    inject = XOpenDisplay(NULL);
    verify = XOpenDisplay(NULL);
    if (!inject || !verify) fail("connect_private_display");
    int event, error;
    if (!XQueryExtension(inject, "XInputExtension", &(int){0}, &event, &error))
        fail("xi_extension_unavailable");
    int major = 2, minor = 4;
    if (XIQueryVersion(inject, &major, &minor) != Success || major < 2 || (major == 2 && minor < 1))
        fail("xi_2_1_required");
    if (!XTestQueryExtension(inject, &event, &error, &major, &minor))
        fail("xtest_unavailable");
    if (major < 2 || (major == 2 && minor < 1)) fail("xtest_relative_motion_unavailable");
    /* Xorg assigns the initial client pointer lazily on its first pointer query. */
    Window root_return, child;
    int root_x, root_y, win_x, win_y;
    unsigned mask;
    if (!XQueryPointer(inject, DefaultRootWindow(inject), &root_return, &child,
                       &root_x, &root_y, &win_x, &win_y, &mask))
        fail("initialize_client_pointer");
    if (!XIGetClientPointer(inject, None, &original_pointer)) fail("original_client_pointer");
    XWindowAttributes attributes;
    if (!XGetWindowAttributes(verify, window, &attributes)) fail("query_target_window");
    if (attributes.map_state != IsViewable) fail("target_window_not_viewable");
    target_window = window;
    char prefix[64], name[96];
    snprintf(prefix, sizeof(prefix), "Oink-Toolkit-%ld", (long)getpid());
    snprintf(name, sizeof(name), "%s pointer", prefix);
    if (named_device(name, XIMasterPointer, 0)) fail("refuse_existing_master_name");
    XIAnyHierarchyChangeInfo add = {.add = {XIAddMaster, prefix, True, True}};
    XIChangeHierarchy(inject, &add, 1);
    sync_require("create_private_master");
    master_pointer = named_device(name, XIMasterPointer, 0);
    if (!master_pointer) fail("created_master_pointer");
    snprintf(name, sizeof(name), "%s keyboard", prefix);
    master_keyboard = named_device(name, XIMasterKeyboard, master_pointer);
    snprintf(name, sizeof(name), "%s XTEST keyboard", prefix);
    xtest_keyboard = named_device(name, XISlaveKeyboard, master_keyboard);
    snprintf(name, sizeof(name), "%s XTEST pointer", prefix);
    xtest_pointer = named_device(name, XISlavePointer, master_pointer);
    if (!master_keyboard || !xtest_keyboard || !xtest_pointer) fail("dedicated_master_and_xtest_slaves");
    XISetClientPointer(inject, None, master_pointer);
    sync_require("bind_client_pointer");
    int bound_pointer = 0;
    if (!XIGetClientPointer(inject, None, &bound_pointer) || bound_pointer != master_pointer)
        fail("confirm_client_pointer_binding");
    /* Metacity settles its own focus shortly after a map; let that pass, then
     * take device focus for our own master. Device focus is per-master, so
     * the window manager cannot reclaim it for this keyboard. */
    Window previous = None, current;
    int revert, settles = 0;
    long deadline_ms = 2000;
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (;;) {
        if (!XGetInputFocus(inject, &current, &revert)) fail("query_window_manager_focus");
        if (current == previous) {
            if (++settles >= 2) break;
        } else {
            settles = 0;
            previous = current;
        }
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        if ((now.tv_sec - start.tv_sec) * 1000L + (now.tv_nsec - start.tv_nsec) / 1000000L > deadline_ms)
            break; /* Settle is best-effort; delivery itself is asserted by the scenario. */
        struct pollfd fd = {ConnectionNumber(inject), POLLIN, 0};
        poll(&fd, 1, 20);
    }
    XISetFocus(inject, master_keyboard, target_window, CurrentTime);
    sync_require("set_owned_device_focus");
    Bool supported = False;
    bool detectable = XkbSetDetectableAutoRepeat(inject, True, &supported) && supported;
    keyboard_map = XkbGetMap(inject, XkbAllClientInfoMask, master_keyboard);
    if (!keyboard_map) fail("query_owned_xkb_map");
    XkbStateRec state;
    if (XkbGetState(inject, master_keyboard, &state) != Success) fail("initial_xkb_state");
    if (state.mods || state.latched_mods || state.locked_mods) fail("owned_keyboard_initially_neutral");
    active_group = state.group;
    printf("{\"type\":\"bound\",\"window\":%lu,\"master_pointer\":%d,\"master_keyboard\":%d,"
           "\"xtest_pointer\":%d,\"xtest_keyboard\":%d,\"geometry\":[%d,%d,%d,%d],"
           "\"detectable_repeat\":%s,\"group\":%u}\n",
           target_window, master_pointer, master_keyboard, xtest_pointer, xtest_keyboard,
           attributes.x, attributes.y, attributes.width, attributes.height,
           detectable ? "true" : "false", active_group);
    fflush(stdout);
}

static void capabilities(void) {
    int count, keys = 0, buttons = 0, axes = 0, relative = 0, scroll = 0, touch = 0, gesture = 0;
    XIDeviceInfo *info = XIQueryDevice(inject, xtest_pointer, &count);
    if (!info || count != 1) fail("query_pointer_capabilities");
    for (int i = 0; i < info->num_classes; i++) {
        XIAnyClassInfo *c = info->classes[i];
        switch (c->type) {
        case XIKeyClass: keys = ((XIKeyClassInfo *)c)->num_keycodes; break;
        case XIButtonClass: buttons = ((XIButtonClassInfo *)c)->num_buttons; break;
        case XIValuatorClass: {
            XIValuatorClassInfo *v = (XIValuatorClassInfo *)c;
            axes++;
            if (v->number >= 0 && v->number < 2 && v->mode == XIModeRelative) relative |= 1 << v->number;
            break;
        }
        case XIScrollClass: scroll++; break;
        case XITouchClass: touch++; break;
#ifdef XIGestureClass
        case XIGestureClass: gesture++; break;
#endif
        }
    }
    printf("{\"type\":\"capabilities\",\"keys\":%d,\"buttons\":%d,\"valuators\":%d,"
           "\"relative_xy_mask\":%d,\"scroll_classes\":%d,\"touch_classes\":%d,"
           "\"gesture_classes\":%d}\n", keys, buttons, axes, relative, scroll, touch, gesture);
    XIFreeDeviceInfo(info);
    XSync(inject, False);
    if (xerror_code) fail("capabilities_query");
}

static void command_key(unsigned long symbol, bool down) {
    unsigned mask = 0;
    unsigned code = mapped_code(symbol, &mask);
    if (!code) fail("symbol_not_directly_reachable_in_active_group");
    if (!XTestFakeKeyEvent(inject, code, down, CurrentTime)) fail("xtest_key_request");
    held_keys[code] = down;
    sync_require("xtest_key_delivery");
    XkbStateRec state;
    if (XkbGetState(inject, master_keyboard, &state) != Success) fail("read_owned_keyboard_state");
    printf("{\"type\":\"key\",\"keysym\":%lu,\"keycode\":%u,\"down\":%s,\"mods\":%u,\"mask\":%u}\n",
           symbol, code, down ? "true" : "false", state.mods, mask);
    fflush(stdout);
}
static void command_map(unsigned long symbol) {
    unsigned mask = 0;
    unsigned code = mapped_code(symbol, &mask);
    printf("{\"type\":\"map\",\"keysym\":%lu,\"keycode\":%u,\"mask\":%u}\n", symbol, code, mask);
    fflush(stdout);
}
static void command_button(unsigned button, bool down) {
    if (!button || button >= 256) fail("invalid_button");
    if (!XTestFakeButtonEvent(inject, button, down, CurrentTime)) fail("xtest_button_request");
    held_buttons[button] = down;
    sync_require("xtest_button_delivery");
}
static void command_motion(bool absolute, int a, int b) {
    bool ok = absolute ? XTestFakeMotionEvent(inject, DefaultScreen(inject), a, b, CurrentTime)
                       : XTestFakeRelativeMotionEvent(inject, a, b, CurrentTime);
    if (!ok) fail("xtest_motion_request");
    sync_require("xtest_motion_delivery");
}
static void command_focus(unsigned long window) {
    XWindowAttributes attributes;
    if (!XGetWindowAttributes(verify, (Window)window, &attributes)) fail("query_focus_target");
    if (attributes.map_state != IsViewable) fail("focus_target_not_viewable");
    XISetFocus(inject, master_keyboard, (Window)window, CurrentTime);
    sync_require("set_owned_device_focus");
    printf("{\"type\":\"focus_ack\",\"window\":%lu}\n", window);
    fflush(stdout);
}
static void command_pos(void) {
    Window root, child;
    double x, y, wx, wy;
    XIButtonState buttons = {0};
    XIModifierState mods;
    XIGroupState group;
    if (!XIQueryPointer(inject, master_pointer, DefaultRootWindow(inject),
                        &root, &child, &x, &y, &wx, &wy, &buttons, &mods, &group))
        fail("query_owned_pointer");
    printf("{\"type\":\"pos\",\"root_x\":%.3f,\"root_y\":%.3f}\n", x, y);
    fflush(stdout);
    if (buttons.mask) XFree(buttons.mask);
    XSync(inject, False);
    if (xerror_code) fail("pos_query");
}

static void cleanup(void) {
    if (cleaning) return;
    cleaning = true;
    xerror_code = 0;
    for (unsigned i = 1; i < 256; i++) {
        if (held_keys[i]) XTestFakeKeyEvent(inject, i, False, CurrentTime);
        if (held_buttons[i]) XTestFakeButtonEvent(inject, i, False, CurrentTime);
    }
    XSync(inject, False);
    if (keyboard_map) XkbFreeKeyboard(keyboard_map, XkbAllComponentsMask, True);
    if (master_pointer) {
        XISetClientPointer(inject, None, original_pointer);
        XIAnyHierarchyChangeInfo change = {.remove = {XIRemoveMaster,
            master_pointer, XIFloating, 0, 0}};
        XIChangeHierarchy(inject, &change, 1);
        XSync(inject, False);
        if (xerror_code) {
            fprintf(stderr, "route: cleanup xerror=%d; runner must destroy the private server\n",
                    xerror_code);
            exit(1);
        }
    }
    if (inject) XCloseDisplay(inject);
    if (verify) XCloseDisplay(verify);
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    require_isolated();
    if (argc != 2) {
        fprintf(stderr, "usage: route <viewable-window-xid>\n");
        return 2;
    }
    char *end;
    unsigned long window = strtoul(argv[1], &end, 0);
    if (!*argv[1] || *end || !window) refuse("invalid_window_xid");
    setup((Window)window);
    char line[256];
    while (fgets(line, sizeof(line), stdin)) {
        char op[16];
        unsigned long a, b;
        int fields = sscanf(line, "%15s %lu %lu", op, &a, &b);
        if (fields >= 1 && !strcmp(op, "cap")) capabilities();
        else if (fields == 2 && !strcmp(op, "map")) command_map(a);
        else if (fields == 2 && !strcmp(op, "focus")) command_focus(a);
        else if (fields >= 1 && !strcmp(op, "pos")) command_pos();
        else if (fields == 3 && !strcmp(op, "key")) command_key(a, b != 0);
        else if (fields == 3 && !strcmp(op, "btn")) command_button((unsigned)a, b != 0);
        else if (fields == 3 && !strcmp(op, "rel")) command_motion(false, (int)a, (int)b);
        else if (fields == 3 && !strcmp(op, "abs")) command_motion(true, (int)a, (int)b);
        else fail("unknown_or_malformed_command");
        if (xerror_code) fail("x_protocol_error");
    }
    if (ferror(stdin)) fail("stdin_read_error");
    cleanup();
    printf("{\"type\":\"route_exit\",\"clean\":true}\n");
    return 0;
}
