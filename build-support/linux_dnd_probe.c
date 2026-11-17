#define _POSIX_C_SOURCE 200809L
#include <gtk/gtk.h>
#include <gdk/gdkx.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Only linux-native-test.py may launch this fixture. No clipboard or user data. */
static const char payload[] = "oink-private-gtk-dnd-fixture";
static GtkTargetEntry targets[] = {{"UTF8_STRING", 0, 1}};
static GtkWidget *source, *destination, *windows[2];
static Display *inject, *observer;
static int opcode, pointer_id, keyboard_id, pointer_xtest, keyboard_xtest;
static int round_id, state, sx, sy, dx, dy;
static gint64 since;
static bool button_held;
static GdkAtom target;
struct Counts {
    int begin, end, failed, cancelled, get, received, drop, deleted;
    int source_press, down, up, raw_down, raw_up, key_down, key_up;
    int motions, leaves, visits, held_checks, returned, free_checks;
    bool inside;
    GdkDragContext *context;
    GdkWindow *source_window;
};
static struct Counts rounds[2];
#define C (&rounds[round_id])

static void require(bool condition, const char *message) {
    if (condition) return;
    fprintf(stderr, "Probe failed: %s (branch=%d state=%d)\n", message, round_id, state);
    printf("result=FAIL begin=%d end=%d drop=%d received=%d failed=%d "
           "visits=%d leaves=%d raw_down=%d raw_up=%d\n", C->begin, C->end,
           C->drop, C->received, C->failed, C->visits, C->leaves, C->raw_down, C->raw_up);
    exit(1);
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

static void next(int value) {
    state = value;
    since = g_get_monotonic_time();
}

static void motion(int x, int y) {
    require(XTestFakeMotionEvent(inject, DefaultScreen(inject), x, y, CurrentTime), "XTEST motion");
    XSync(inject, False);
}

static void button(bool down) {
    require(button_held != down, "no duplicate synthetic button transition");
    require(XTestFakeButtonEvent(inject, 1, down, CurrentTime), "XTEST button");
    button_held = down;
    if (down) C->down++; else C->up++;
    XSync(inject, False);
}

static void raw_events(void) {
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
            if (type == XI_RawButtonPress) C->raw_down++; else C->raw_up++;
        } else if (type == XI_RawKeyPress || type == XI_RawKeyRelease) {
            require(raw->sourceid == keyboard_xtest
                    && raw->detail == XKeysymToKeycode(inject, XK_Escape), "only private XTEST Escape");
            if (type == XI_RawKeyPress) C->key_down++; else C->key_up++;
        }
        XFreeEventData(observer, &event.xcookie);
    }
}

/* A second connection must NOT take the source grab. Ungrab only our own successes. */
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

static void pointer_at(int x, int y, bool held) {
    Window root, child;
    int rx, ry, wx, wy;
    unsigned int mask;
    require(XQueryPointer(inject, DefaultRootWindow(inject), &root, &child,
                          &rx, &ry, &wx, &wy, &mask), "query private pointer");
    require(rx == x && ry == y && !!(mask & Button1Mask) == held,
            "pointer position and physical button state");
}

static void held_context(void) {
    require(C->begin == 1 && C->end == 0 && C->failed == 0 && C->drop == 0
            && C->get == 0 && C->received == 0 && C->down == 1 && C->up == 0
            && C->raw_down == 1 && C->raw_up == 0, "one uninterrupted drag, no early transfer");
    require(C->context && gtk_drag_get_source_widget(C->context) == source
            && gdk_drag_context_get_source_window(C->context) == C->source_window,
            "original live source context and window");
    GdkDevice *device = gdk_drag_context_get_device(C->context);
    require(gdk_x11_device_get_id(device) == pointer_id
            && gdk_display_device_is_grabbed(gdk_display_get_default(), device), "GTK source owns core grab");
    require(try_grab(pointer_id, false) == AlreadyGrabbed, "native source grab cannot be stolen");
    C->held_checks++;
}

static void no_grab(void) {
    require(!gdk_display_device_is_grabbed(gdk_display_get_default(),
                gdk_drag_context_get_device(C->context)), "GTK released source grab");
    require(try_grab(pointer_id, false) == GrabSuccess
            && try_grab(keyboard_id, true) == GrabSuccess, "no ghost pointer or keyboard grab");
    C->free_checks++;
}

static gboolean pressed(GtkWidget *widget, GdkEventButton *event, gpointer data) {
    (void)widget; (void)data;
    if (event->button == 1 && event->type == GDK_BUTTON_PRESS) C->source_press++;
    return FALSE;
}

static void begun(GtkWidget *widget, GdkDragContext *context, gpointer data) {
    (void)data;
    require(widget == source && ++C->begin == 1 && C->down == 1 && C->up == 0,
            "GTK source-set started exactly once from held button");
    require(!round_id || context != rounds[0].context, "cancellation uses a new real drag");
    C->context = g_object_ref(context);
    C->source_window = gdk_drag_context_get_source_window(context);
    require(gdk_drag_context_get_protocol(context) == GDK_DRAG_PROTO_XDND, "native X11 GTK drag");
}

static void ended(GtkWidget *widget, GdkDragContext *context, gpointer data) {
    (void)widget; (void)data;
    require(context == C->context && ++C->end == 1, "one drag-end on original source context");
}

static gboolean failed(GtkWidget *widget, GdkDragContext *context, GtkDragResult result, gpointer data) {
    (void)widget; (void)data;
    require(context == C->context && round_id == 1, "unexpected drag failure");
    C->failed++;
    if (result == GTK_DRAG_RESULT_USER_CANCELLED) C->cancelled++;
    /* FALSE preserves GTK's native cancellation cleanup/animation. */
    return FALSE;
}

static void data_get(GtkWidget *widget, GdkDragContext *context, GtkSelectionData *selection,
                     guint info, guint time, gpointer data) {
    (void)widget; (void)time; (void)data;
    require(context == C->context && round_id == 0 && C->drop == 1 && info == 1
            && gtk_selection_data_get_target(selection) == target, "real drop requested fixture target");
    require(gtk_selection_data_get_selection(selection) == gdk_atom_intern_static_string("XdndSelection"),
            "DnD selection only, never CLIPBOARD");
    C->get++;
    gtk_selection_data_set(selection, target, 8, (const guchar *)payload, sizeof(payload) - 1);
}

static void deleted(GtkWidget *widget, GdkDragContext *context, gpointer data) {
    (void)widget; (void)context; (void)data;
    C->deleted++;
    require(false, "COPY must never request source deletion");
}

static gboolean dragged(GtkWidget *widget, GdkDragContext *context, gint x, gint y,
                        guint time, gpointer data) {
    (void)widget; (void)x; (void)y; (void)data;
    require(C->begin == 1 && C->end == 0 && gtk_drag_get_source_widget(context) == source,
            "destination motion belongs to live original source");
    C->motions++;
    if (!C->inside) { C->inside = true; C->visits++; }
    gdk_drag_status(context, GDK_ACTION_COPY, time);
    return TRUE;
}

static void left(GtkWidget *widget, GdkDragContext *context, guint time, gpointer data) {
    (void)widget; (void)context; (void)time; (void)data;
    C->inside = false;
    C->leaves++;
}

static gboolean dropped(GtkWidget *widget, GdkDragContext *context, gint x, gint y,
                        guint time, gpointer data) {
    (void)x; (void)y; (void)data;
    require(round_id == 0 && state == 7 && !button_held && ++C->drop == 1
            && C->returned == 1 && C->visits == 2, "drop only after return, reentry and release");
    gtk_drag_get_data(widget, context, target, time);
    return TRUE;
}

static void received(GtkWidget *widget, GdkDragContext *context, gint x, gint y,
                     GtkSelectionData *selection, guint info, guint time, gpointer data) {
    (void)widget; (void)x; (void)y; (void)data;
    require(round_id == 0 && C->drop == 1 && C->get == 1 && info == 1
            && gtk_drag_get_source_widget(context) == source
            && gtk_selection_data_get_data_type(selection) == target
            && gtk_selection_data_get_format(selection) == 8
            && gtk_selection_data_get_length(selection) == (gint)sizeof(payload) - 1
            && memcmp(gtk_selection_data_get_data(selection), payload, sizeof(payload) - 1) == 0,
            "real data-received with exact constant payload");
    require(++C->received == 1, "single data-received");
    gtk_drag_finish(context, TRUE, FALSE, time);
}

static void center(GtkWidget *widget, int *x, int *y) {
    require(gdk_window_is_viewable(gtk_widget_get_window(widget)), "fixture is viewable");
    gdk_window_get_origin(gtk_widget_get_window(widget), x, y);
    *x += gtk_widget_get_allocated_width(widget) / 2;
    *y += gtk_widget_get_allocated_height(widget) / 2;
}

static void report(void) {
    printf("%s: begin=%d end=%d failed=%d user_cancelled=%d data_get=%d data_received=%d "
           "drop=%d delete=%d source_button_down=%d injected_down/up=%d/%d raw_down/up=%d/%d "
           "escape_down/up=%d/%d destination_visits=%d motion=%d leave=%d "
           "same_context_return=%d native_grab_held=%d ghost_grab_free=%d\n",
           round_id ? "escape" : "drop", C->begin, C->end, C->failed, C->cancelled,
           C->get, C->received, C->drop, C->deleted, C->source_press, C->down, C->up,
           C->raw_down, C->raw_up, C->key_down, C->key_up, C->visits, C->motions,
           C->leaves, C->returned, C->held_checks, C->free_checks);
}

static gboolean tick(gpointer data) {
    (void)data;
    raw_events();
    gint64 elapsed = (g_get_monotonic_time() - since) / 1000;
    require(elapsed < 2500, "state signal deadline");
    switch (state) {
    case 0:
        if (elapsed < 400) break;
        center(source, &sx, &sy); center(destination, &dx, &dy);
        XSetInputFocus(inject, GDK_WINDOW_XID(gtk_widget_get_window(windows[0])),
                       RevertToParent, CurrentTime);
        motion(sx, sy); next(1); break;
    case 1:
        if (elapsed < 100) break;
        pointer_at(sx, sy, false); button(true); next(2); break;
    case 2:
        if (C->source_press != 1 || elapsed < 100) break;
        motion(sx + 60, sy); next(3); break;
    case 3:
        if (C->begin != 1 || elapsed < 150) break;
        held_context(); motion(dx, dy); next(4); break;
    case 4:
        if (C->visits != 1 || elapsed < 150) break;
        pointer_at(dx, dy, true); held_context(); motion(sx, sy); next(5); break;
    case 5:
        if (C->leaves < 1 || elapsed < 250) break;
        pointer_at(sx, sy, true); held_context(); C->returned++;
        motion(dx, dy); next(6); break;
    case 6:
        if (C->visits != 2 || elapsed < 200
                || gdk_drag_context_get_selected_action(C->context) != GDK_ACTION_COPY) break;
        pointer_at(dx, dy, true); held_context(); next(7);
        if (!round_id) button(false);
        else {
            unsigned int code = XKeysymToKeycode(inject, XK_Escape);
            require(code && XTestFakeKeyEvent(inject, code, True, CurrentTime)
                    && XTestFakeKeyEvent(inject, code, False, CurrentTime), "XTEST Escape");
            XSync(inject, False);
        }
        break;
    case 7:
        if (C->end != 1 || elapsed < 150) break;
        no_grab();
        if (round_id) {
            require(C->failed == 1 && C->cancelled == 1 && C->drop == 0
                    && C->get == 0 && C->received == 0, "Escape cancels without transfer");
            pointer_at(dx, dy, true); /* Cancellation must release grab before ButtonUp. */
        } else require(C->received == 1 && C->drop == 1 && C->failed == 0, "successful real drop");
        motion(sx, sy); next(8); break;
    case 8:
        if (elapsed < 200) break;
        if (round_id) { no_grab(); button(false); }
        motion(dx, dy); next(9); break;
    case 9:
        if (elapsed < 600) break;
        pointer_at(dx, dy, false); no_grab();
        require(C->begin == 1 && C->end == 1 && C->source_press == 1
                && C->down == 1 && C->up == 1 && C->raw_down == 1 && C->raw_up == 1
                && C->key_down == round_id && C->key_up == round_id
                && C->drop == 1 - round_id && C->get == 1 - round_id
                && C->received == 1 - round_id && C->failed == round_id
                && C->cancelled == round_id && C->deleted == 0 && C->returned == 1,
                "final exact counts, including quiet period after release");
        report();
        if (!round_id) { round_id = 1; next(0); }
        else { gtk_main_quit(); return G_SOURCE_REMOVE; }
        break;
    }
    return G_SOURCE_CONTINUE;
}

static gboolean deadline(gpointer data) {
    (void)data;
    require(false, "18-second overall deadline");
    return G_SOURCE_REMOVE;
}

int main(void) {
    require_isolated(); /* Must precede GTK initialization and all X11 connections. */
    alarm(19);
    setvbuf(stdout, NULL, _IOLBF, 0);
    require(gtk_init_check(NULL, NULL), "private GTK X11 initialization");
    require(GDK_IS_X11_DISPLAY(gdk_display_get_default()), "X11 backend only");
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
    /* A fresh X client may not have a ClientPointer until its first core request. */
    XISetClientPointer(inject, None, pointer_id);
    int selected;
    require(XIGetClientPointer(inject, None, &selected) && selected == pointer_id, "core test pointer");
    unsigned char bits[XIMaskLen(XI_LASTEVENT)] = {0};
    XISetMask(bits, XI_RawButtonPress); XISetMask(bits, XI_RawButtonRelease);
    XISetMask(bits, XI_RawKeyPress); XISetMask(bits, XI_RawKeyRelease);
    XIEventMask mask = {XIAllMasterDevices, sizeof(bits), bits};
    XISelectEvents(observer, DefaultRootWindow(observer), &mask, 1);
    XSync(observer, False);
    GtkWidget *boxes[2];
    for (int i = 0; i < 2; i++) {
        windows[i] = gtk_window_new(GTK_WINDOW_TOPLEVEL);
        gtk_window_set_decorated(GTK_WINDOW(windows[i]), FALSE);
        gtk_window_set_default_size(GTK_WINDOW(windows[i]), 260, 220);
        gtk_window_move(GTK_WINDOW(windows[i]), 100 + i * 550, 200);
        boxes[i] = gtk_event_box_new();
        gtk_container_add(GTK_CONTAINER(windows[i]), boxes[i]);
        gtk_container_add(GTK_CONTAINER(boxes[i]), gtk_label_new(i ? "Private catcher / destination" : "Private source"));
    }
    source = boxes[0]; destination = boxes[1];
    target = gdk_atom_intern_static_string("UTF8_STRING");
    gtk_widget_add_events(source, GDK_BUTTON_PRESS_MASK | GDK_BUTTON_RELEASE_MASK | GDK_POINTER_MOTION_MASK);
    g_signal_connect(source, "button-press-event", G_CALLBACK(pressed), NULL);
    gtk_drag_source_set(source, GDK_BUTTON1_MASK, targets, 1, GDK_ACTION_COPY);
    gtk_drag_dest_set(destination, 0, targets, 1, GDK_ACTION_COPY);
    g_signal_connect(source, "drag-begin", G_CALLBACK(begun), NULL);
    g_signal_connect(source, "drag-end", G_CALLBACK(ended), NULL);
    g_signal_connect(source, "drag-failed", G_CALLBACK(failed), NULL);
    g_signal_connect(source, "drag-data-get", G_CALLBACK(data_get), NULL);
    g_signal_connect(source, "drag-data-delete", G_CALLBACK(deleted), NULL);
    g_signal_connect(destination, "drag-motion", G_CALLBACK(dragged), NULL);
    g_signal_connect(destination, "drag-leave", G_CALLBACK(left), NULL);
    g_signal_connect(destination, "drag-drop", G_CALLBACK(dropped), NULL);
    g_signal_connect(destination, "drag-data-received", G_CALLBACK(received), NULL);
    gtk_widget_show_all(windows[0]); gtk_widget_show_all(windows[1]);
    next(0);
    g_timeout_add(25, tick, NULL);
    g_timeout_add(18000, deadline, NULL);
    gtk_main();
    for (int i = 0; i < 2; i++) { g_object_unref(rounds[i].context); gtk_widget_destroy(windows[i]); }
    XCloseDisplay(observer); XCloseDisplay(inject);
    printf("result=PASS GTK-local native source/grab survived return without second ButtonDown; "
           "real drop and Escape cleanup verified; not product XDND/network/cross-screen, Qt/Nemo/browser untested.\n");
    return 0;
}
