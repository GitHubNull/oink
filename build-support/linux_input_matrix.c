#include "linux_input_matrix_support.h"
#include <X11/Xauth.h>
#include <X11/keysym.h>
#include <X11/XF86keysym.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

struct Matrix matrix = {.active = -1};
volatile sig_atomic_t matrix_interrupted;
#define CASE(n, k, s, a) {n, k, s, a, PENDING, 0, NULL}
struct Case matrix_cases[] = {
    CASE("shift_left", MODIFIER, XK_Shift_L, ShiftMask),
    CASE("shift_right", MODIFIER, XK_Shift_R, ShiftMask),
    CASE("ctrl_left", MODIFIER, XK_Control_L, ControlMask),
    CASE("ctrl_right", MODIFIER, XK_Control_R, ControlMask),
    CASE("alt_left", MODIFIER, XK_Alt_L, 0),
    CASE("alt_right", MODIFIER, XK_Alt_R, 0),
    CASE("super_left", MODIFIER, XK_Super_L, 0),
    CASE("super_right", MODIFIER, XK_Super_R, 0),
    CASE("altgr_iso_level3", MODIFIER, XK_ISO_Level3_Shift, 0),
    CASE("media_mute", KEY, XF86XK_AudioMute, 0),
    CASE("media_volume_down", KEY, XF86XK_AudioLowerVolume, 0),
    CASE("media_volume_up", KEY, XF86XK_AudioRaiseVolume, 0),
    CASE("media_play", KEY, XF86XK_AudioPlay, 0),
    CASE("media_previous", KEY, XF86XK_AudioPrev, 0),
    CASE("media_next", KEY, XF86XK_AudioNext, 0),
    CASE("ordinary_a", KEY, XK_a, 0),
    CASE("ordinary_return", KEY, XK_Return, 0),
    CASE("ordinary_f12", KEY, XK_F12, 0),
    CASE("manual_duplicate_down", MANUAL_REPEAT, XK_a, 0),
    CASE("server_auto_repeat", SERVER_REPEAT, XK_a, 0),
    CASE("relative_positive", RELATIVE, NoSymbol, 1),
    CASE("relative_negative", RELATIVE, NoSymbol, -1),
    CASE("wheel_vertical_up", WHEEL, NoSymbol, 4),
    CASE("wheel_vertical_down", WHEEL, NoSymbol, 5),
    CASE("wheel_horizontal_left", WHEEL, NoSymbol, 6),
    CASE("wheel_horizontal_right", WHEEL, NoSymbol, 7),
};
const unsigned matrix_case_count = sizeof(matrix_cases) / sizeof(matrix_cases[0]);

static int record_error(Display *display, XErrorEvent *event) {
    (void)display;
    matrix.xerror = event->error_code;
    return 0;
}
static int disconnected(Display *display) {
    (void)display;
    matrix.disconnected = true;
    matrix_finish(1, "x_connection_lost_runner_must_destroy_private_server");
}
static void interrupted(int number) { matrix_interrupted = number; }

void matrix_sync(Display *display) {
    XSync(display, False);
    matrix_require(!matrix.xerror, "x_protocol_error");
    matrix_require(!matrix_interrupted, "interrupted_runner_must_destroy_private_server");
}
void matrix_require(bool condition, const char *reason) {
    if (!condition) matrix_finish(1, reason);
}
static void isolated(bool condition, const char *reason) {
    if (!condition) matrix_finish(2, reason);
}
/* Same mandatory private-root/audio guard as linux_input_probe.c, plus a
 * numeric display and matching private authority entry before XOpenDisplay.
 * Never discover a display, fall back to :0, copy host auth, or use a helper. */
static void require_isolated(void) {
    const char *root = getenv("OINK_NATIVE_TEST_ROOT");
    const char *authority = getenv("XAUTHORITY"), *display = getenv("DISPLAY");
    struct stat st;
    char expected[4096];
    isolated(geteuid() == getuid() && getuid() != 0, "ordinary_user_required");
    isolated(root && strncmp(root, "/tmp/oink-native-", 17) == 0,
             "use_linux_native_test_runner_never_host_session");
    isolated(strlen(root) + 32 < sizeof(expected) && root[17] && !strchr(root + 5, '/'),
             "invalid_private_root");
    isolated(lstat(root, &st) == 0 && S_ISDIR(st.st_mode) && st.st_uid == getuid()
             && (st.st_mode & 077) == 0, "root_must_be_owned_private_directory");
    snprintf(expected, sizeof(expected), "%s/Xauthority", root);
    isolated(authority && strcmp(authority, expected) == 0, "authority_outside_private_root");
    isolated(lstat(authority, &st) == 0 && S_ISREG(st.st_mode) && st.st_uid == getuid()
             && (st.st_mode & 077) == 0, "authority_must_be_owned_private_file");
    isolated(display && display[0] == ':' && display[1] >= '1' && display[1] <= '9',
             "refuse_host_zero_remote_or_non_numeric_display");
    char *end;
    long number = strtol(display + 1, &end, 10);
    isolated(number > 0 && number < 65536 && (!*end || strcmp(end, ".0") == 0),
             "invalid_private_display_number");
    int fd = open(authority, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    isolated(fd >= 0, "cannot_open_private_authority");
    FILE *file = fdopen(fd, "rb");
    isolated(file != NULL, "cannot_read_private_authority");
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
    isolated(matching, "display_not_registered_in_private_authority");
    snprintf(expected, sizeof(expected), "unix:%s/runtime/pulse-native", root);
    isolated(getenv("PULSE_SERVER") && strcmp(getenv("PULSE_SERVER"), expected) == 0,
             "missing_private_audio_environment");
}

static bool cleanup(void) {
    if (matrix.disconnected || matrix.cleaning) return false;
    matrix.cleaning = true;
    matrix.xerror = 0;
    if (matrix.inject && matrix.route.pointer) {
        for (unsigned i = 1; i < 256; i++) {
            if (matrix.held_keys[i]) XTestFakeKeyEvent(matrix.inject, i, False, CurrentTime);
            if (matrix.held_buttons[i]) XTestFakeButtonEvent(matrix.inject, i, False, CurrentTime);
        }
        XSync(matrix.inject, False);
        matrix_repeat_restore();
        XISetClientPointer(matrix.inject, None, matrix.original_pointer);
        XIAnyHierarchyChangeInfo change = {.remove = {XIRemoveMaster,
            matrix.route.pointer, XIFloating, 0, 0}};
        XIChangeHierarchy(matrix.inject, &change, 1);
        XSync(matrix.inject, False);
    }
    if (matrix.map) XkbFreeKeyboard(matrix.map, XkbAllComponentsMask, True);
    if (matrix.target && matrix.route.window) XDestroyWindow(matrix.target, matrix.route.window);
    if (matrix.inject) XCloseDisplay(matrix.inject);
    if (matrix.observe) XCloseDisplay(matrix.observe);
    if (matrix.target) XCloseDisplay(matrix.target);
    return !matrix.xerror;
}
_Noreturn void matrix_finish(int result, const char *reason) {
    if (matrix.active >= 0 && matrix_cases[matrix.active].status == PENDING)
        matrix_cases[matrix.active].status = FAILED;
    bool cleaned = cleanup();
    if (!cleaned && result != 2) result = 1;
    unsigned passed = 0, skipped = 0, failed = 0;
    for (unsigned i = 0; i < matrix_case_count; i++) {
        struct Case *c = &matrix_cases[i];
        if (c->status == PASSED) passed++;
        else if (c->status == FAILED) failed++;
        else skipped++;
    }
    if (!result && skipped) result = 77;
    printf("{\"type\":\"summary\",\"schema\":1,\"scope\":\"synthetic_x11_only\","
           "\"exit\":%d,\"reason\":\"%s\",\"passed\":%u,\"failed\":%u,\"incomplete\":%u,"
           "\"cleanup_ok\":%s,\"runner_server_teardown_required\":true,"
           "\"core_sourceid_available\":false,\"not_tested\":[", result, reason, passed,
           failed, skipped, cleaned ? "true" : "false");
    const char *excluded[] = {"physical_hid", "physical_pointer_acceleration", "fine_scroll",
        "touch_contacts_and_complete_gestures", "qt_delivery", "nemo_delivery", "browser_delivery",
        "product_transport_and_contacts_frame_0x020b", "media_key_system_actions",
        "layout_remapping_or_text_composition"};
    bool comma = false;
    for (unsigned i = 0; i < sizeof(excluded) / sizeof(excluded[0]); i++) {
        printf("%s{\"item\":\"%s\",\"reason\":\"outside_synthetic_x11_scope\"}",
               comma ? "," : "", excluded[i]);
        comma = true;
    }
    for (unsigned i = 0; i < matrix_case_count; i++) {
        struct Case *c = &matrix_cases[i];
        if (c->status == NOT_TESTED || c->status == PENDING)
            printf(",{\"item\":\"%s\",\"reason\":\"%s\"}", c->name,
                   c->reason ? c->reason : "not_reached");
    }
    printf("],\"cases\":[");
    const char *states[] = {"not_tested", "pass", "not_tested", "fail"};
    for (unsigned i = 0; i < matrix_case_count; i++)
        printf("%s{\"name\":\"%s\",\"status\":\"%s\",\"completed_modes\":%u}",
               i ? "," : "", matrix_cases[i].name, states[matrix_cases[i].status], matrix_cases[i].modes);
    puts("]}");
    fflush(stdout);
    exit(result);
}
void matrix_skip(struct Case *test, const char *reason) {
    test->status = NOT_TESTED;
    test->reason = reason;
}

static int named_device(const char *name, int use, int attachment) {
    int count, found = 0;
    XIDeviceInfo *devices = XIQueryDevice(matrix.inject, XIAllDevices, &count);
    matrix_require(devices != NULL, "query_private_devices");
    for (int i = 0; i < count; i++) {
        if (strcmp(name, devices[i].name) != 0) continue;
        matrix_require(!found && devices[i].use == use && devices[i].enabled
                       && (!attachment || devices[i].attachment == attachment), "owned_device_identity");
        found = devices[i].deviceid;
    }
    XIFreeDeviceInfo(devices);
    return found;
}
static void capabilities(int device, const char *role) {
    int count;
    XIDeviceInfo *info = XIQueryDevice(matrix.inject, device, &count);
    matrix_require(info && count == 1, "query_owned_capabilities");
    int keys = 0, buttons = 0, axes = 0, relative = 0, scroll = 0, touch = 0, gesture = 0;
    for (int i = 0; i < info->num_classes; i++) {
        XIAnyClassInfo *c = info->classes[i];
        switch (c->type) {
        case XIKeyClass: keys = ((XIKeyClassInfo *)c)->num_keycodes; break;
        case XIButtonClass: buttons = ((XIButtonClassInfo *)c)->num_buttons; break;
        case XIValuatorClass: {
            XIValuatorClassInfo *v = (XIValuatorClassInfo *)c;
            axes++;
            if (v->number >= 0 && v->number < 2 && v->mode == XIModeRelative)
                relative |= 1 << v->number;
            break;
        }
        case XIScrollClass: scroll++; break;
        case XITouchClass: touch++; break;
#ifdef XIGestureClass
        case XIGestureClass: gesture++; break;
#endif
        }
    }
    if (device == matrix.route.pointer_source) {
        matrix.button_count = buttons;
        matrix.relative_axes = relative;
    }
    printf("{\"type\":\"device_capabilities\",\"role\":\"%s\",\"id\":%d,"
           "\"keys\":%d,\"buttons\":%d,\"valuators\":%d,\"relative_xy_mask\":%d,"
           "\"scroll_classes\":%d,\"touch_classes\":%d,\"gesture_classes\":%d}\n",
           role, device, keys, buttons, axes, relative, scroll, touch, gesture);
    XIFreeDeviceInfo(info);
}
static void setup(void) {
    XSetErrorHandler(record_error);
    XSetIOErrorHandler(disconnected);
    matrix.target = XOpenDisplay(NULL);
    matrix.observe = XOpenDisplay(NULL);
    matrix.inject = XOpenDisplay(NULL);
    matrix_require(matrix.target && matrix.observe && matrix.inject, "connect_private_display");
    int event, error;
    if (!XQueryExtension(matrix.observe, "XInputExtension", &matrix.opcode, &event, &error))
        matrix_finish(77, "xi_extension_unavailable");
    Display *clients[] = {matrix.observe, matrix.target, matrix.inject};
    for (unsigned i = 0; i < 3; i++) {
        int major = 2, minor = 4;
        int status = XIQueryVersion(clients[i], &major, &minor);
        printf("{\"type\":\"xi_version\",\"client\":%u,\"major\":%d,\"minor\":%d,\"status\":%d}\n",
               i, major, minor, status);
        if (status != Success || major < 2 || (major == 2 && minor < 1))
            matrix_finish(77, "xi_2_1_required_for_reliable_raw_sourceid");
        matrix.xi_major = major;
        matrix.xi_minor = minor;
    }
    int major = XkbMajorVersion, minor = XkbMinorVersion, opcode;
    if (!XkbQueryExtension(matrix.inject, &opcode, &event, &error, &major, &minor))
        matrix_finish(77, "xkb_unavailable");
    printf("{\"type\":\"xkb_version\",\"major\":%d,\"minor\":%d}\n", major, minor);
    if (!XTestQueryExtension(matrix.inject, &event, &error, &major, &minor))
        matrix_finish(77, "xtest_unavailable");
    printf("{\"type\":\"xtest_version\",\"major\":%d,\"minor\":%d}\n", major, minor);
    if (major < 2 || (major == 2 && minor < 1)) matrix_finish(77, "xtest_relative_motion_unavailable");
    Window pointer_root, pointer_child;
    int root_x, root_y, window_x, window_y;
    unsigned pointer_mask;
    /* Xorg assigns the initial client pointer lazily on its first pointer query. */
    matrix_require(XQueryPointer(matrix.inject, DefaultRootWindow(matrix.inject),
        &pointer_root, &pointer_child, &root_x, &root_y, &window_x, &window_y, &pointer_mask),
        "initialize_client_pointer");
    matrix_require(XIGetClientPointer(matrix.inject, None, &matrix.original_pointer), "original_client_pointer");
    char prefix[64], name[96];
    snprintf(prefix, sizeof(prefix), "Oink-Matrix-%ld", (long)getpid());
    snprintf(name, sizeof(name), "%s pointer", prefix);
    matrix_require(!named_device(name, XIMasterPointer, 0), "refuse_existing_master_name");
    XIAnyHierarchyChangeInfo add = {.add = {XIAddMaster, prefix, True, True}};
    XIChangeHierarchy(matrix.inject, &add, 1);
    matrix_sync(matrix.inject);
    matrix.route.pointer = named_device(name, XIMasterPointer, 0);
    matrix_require(matrix.route.pointer != 0, "created_master_pointer");
    snprintf(name, sizeof(name), "%s keyboard", prefix);
    matrix.route.keyboard = named_device(name, XIMasterKeyboard, matrix.route.pointer);
    snprintf(name, sizeof(name), "%s XTEST keyboard", prefix);
    matrix.route.key_source = named_device(name, XISlaveKeyboard, matrix.route.keyboard);
    snprintf(name, sizeof(name), "%s XTEST pointer", prefix);
    matrix.route.pointer_source = named_device(name, XISlavePointer, matrix.route.pointer);
    matrix_require(matrix.route.keyboard && matrix.route.key_source && matrix.route.pointer_source,
                   "dedicated_master_and_xtest_slaves");
    capabilities(matrix.route.key_source, "xtest_keyboard");
    capabilities(matrix.route.pointer_source, "xtest_pointer");
    matrix.route.root = DefaultRootWindow(matrix.target);
    XSetWindowAttributes attributes = {.override_redirect = True};
    matrix.route.window = XCreateWindow(matrix.target, matrix.route.root, 20, 20, 600, 400, 0,
        CopyFromParent, InputOutput, CopyFromParent, CWOverrideRedirect, &attributes);
    XMapRaised(matrix.target, matrix.route.window);
    matrix_sync(matrix.target);
    XISetClientPointer(matrix.inject, None, matrix.route.pointer);
    XISetClientPointer(matrix.target, None, matrix.route.pointer);
    XISetFocus(matrix.inject, matrix.route.keyboard, matrix.route.window, CurrentTime);
    matrix_require(XTestFakeMotionEvent(matrix.inject, DefaultScreen(matrix.inject), 300, 200, CurrentTime),
                   "position_owned_pointer_inside_fixture");
    matrix_sync(matrix.inject);
    Bool supported = False;
    matrix.detectable = XkbSetDetectableAutoRepeat(matrix.target, True, &supported) && supported;
    matrix.map = XkbGetMap(matrix.inject, XkbAllClientInfoMask, matrix.route.keyboard);
    matrix_require(matrix.map != NULL, "query_owned_xkb_map");
    XkbStateRec state;
    matrix_require(XkbGetState(matrix.inject, matrix.route.keyboard, &state) == Success, "initial_xkb_state");
    matrix_require(!state.mods && !state.latched_mods && !state.locked_mods, "owned_keyboard_initially_neutral");
    matrix.group = state.group;
    printf("{\"type\":\"fixture\",\"master_pointer\":%d,\"master_keyboard\":%d,"
           "\"xtest_pointer\":%d,\"xtest_keyboard\":%d,\"window\":%lu,\"root\":%lu,"
           "\"detectable_repeat\":%s,\"active_group\":%u}\n", matrix.route.pointer,
           matrix.route.keyboard, matrix.route.pointer_source, matrix.route.key_source,
           matrix.route.window, matrix.route.root, matrix.detectable ? "true" : "false", matrix.group);
    matrix.mapping_count = XGetPointerMapping(matrix.inject, matrix.button_map, sizeof(matrix.button_map));
    matrix_require(matrix.mapping_count <= 256, "bounded_pointer_mapping");
    unsigned char bits[XIMaskLen(XI_LASTEVENT)] = {0};
    int types[] = {XI_RawKeyPress, XI_RawKeyRelease, XI_RawButtonPress, XI_RawButtonRelease, XI_RawMotion};
    for (unsigned i = 0; i < sizeof(types) / sizeof(types[0]); i++) XISetMask(bits, types[i]);
    XIEventMask mask = {XIAllMasterDevices, sizeof(bits), bits};
    XISelectEvents(matrix.observe, matrix.route.root, &mask, 1);
    matrix_sync(matrix.observe);
}
int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    require_isolated();
    struct sigaction action = {.sa_handler = interrupted};
    sigemptyset(&action.sa_mask);
    sigaction(SIGTERM, &action, NULL);
    sigaction(SIGINT, &action, NULL);
    setup();
    for (unsigned i = 0; i < matrix_case_count; i++) {
        matrix.active = (int)i;
        struct Case *test = &matrix_cases[i];
        if (test->kind == RELATIVE || test->kind == WHEEL) matrix_pointer(test);
        else matrix_keyboard(test);
        if (test->status == PENDING) {
            matrix_require(test->modes == 3, "both_core_and_xi_target_modes_required");
            test->status = PASSED;
        }
        matrix.active = -1;
    }
    /* Contacts ARE on the product wire (v1 registry 0x020b); no network path
     * is exercised here. Do not infer physical input or toolkit delivery. */
    matrix_finish(0, "matrix_completed");
}
