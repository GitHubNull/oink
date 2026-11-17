#include "linux_input_matrix_support.h"
#include <errno.h>
#include <poll.h>
#include <time.h>

long matrix_ms(void) {
    struct timespec now;
    matrix_require(clock_gettime(CLOCK_MONOTONIC, &now) == 0, "monotonic_clock");
    return now.tv_sec * 1000 + now.tv_nsec / 1000000;
}
static bool core_kind(int type, enum Kind *kind) {
    switch (type) {
    case KeyPress: *kind = KEY_DOWN; break;
    case KeyRelease: *kind = KEY_UP; break;
    case ButtonPress: *kind = BUTTON_DOWN; break;
    case ButtonRelease: *kind = BUTTON_UP; break;
    case MotionNotify: *kind = MOTION; break;
    default: return false;
    }
    return true;
}
static bool xi_kind(int type, enum Kind *kind, bool *raw) {
    *raw = false;
    switch (type) {
    case XI_KeyPress: *kind = KEY_DOWN; break;
    case XI_KeyRelease: *kind = KEY_UP; break;
    case XI_ButtonPress: *kind = BUTTON_DOWN; break;
    case XI_ButtonRelease: *kind = BUTTON_UP; break;
    case XI_Motion: *kind = MOTION; break;
    case XI_RawKeyPress: *kind = KEY_DOWN; *raw = true; break;
    case XI_RawKeyRelease: *kind = KEY_UP; *raw = true; break;
    case XI_RawButtonPress: *kind = BUTTON_DOWN; *raw = true; break;
    case XI_RawButtonRelease: *kind = BUTTON_UP; *raw = true; break;
    case XI_RawMotion: *kind = MOTION; *raw = true; break;
    default: return false;
    }
    return true;
}
static void valuators(struct Event *out, const XIValuatorState *v, const double *raw) {
    unsigned offset = 0;
    matrix_require(v->mask_len >= 0 && v->mask_len <= 32, "bounded_valuator_mask");
    for (int axis = 0; axis < v->mask_len * 8; axis++) {
        if (!XIMaskIsSet(v->mask, axis)) continue;
        matrix_require(isfinite(v->values[offset]) && (!raw || isfinite(raw[offset])), "finite_valuator");
        if (axis < 2) {
            out->axes |= 1u << axis;
            out->cooked[axis] = v->values[offset];
            if (raw) out->raw[axis] = raw[offset];
        } else out->axes |= 4; /* Unexpected axes are not silently ignored. */
        offset++;
    }
}
static void append(struct Trace *t, const struct Event *event, bool raw) {
    unsigned *n = raw ? &t->nraw : &t->ntarget;
    struct Event *events = raw ? t->raw : t->target;
    matrix_require(*n < MATRIX_MAX_EVENTS, "event_buffer_overflow");
    events[(*n)++] = *event;
}
static void receive(Display *display, struct Trace *trace, bool observer) {
    while (XPending(display)) {
        XEvent event;
        XNextEvent(display, &event);
        struct Event out = {0};
        if (core_kind(event.type, &out.kind)) {
            matrix_require(!observer && !matrix.xi_mode, "unexpected_core_event_channel");
            out.sent = event.xany.send_event;
            if (matrix_is_key(out.kind)) {
                XKeyEvent *e = &event.xkey;
                out.window = e->window; out.root = e->root; out.time = e->time;
                out.detail = (int)e->keycode; out.mods = e->state;
                out.x = e->x_root; out.y = e->y_root;
                matrix_require(e->same_screen, "core_key_screen");
            } else if (out.kind == MOTION) {
                XMotionEvent *e = &event.xmotion;
                out.window = e->window; out.root = e->root; out.time = e->time;
                out.mods = e->state; out.x = e->x_root; out.y = e->y_root;
                matrix_require(e->same_screen, "core_motion_screen");
            } else {
                XButtonEvent *e = &event.xbutton;
                out.window = e->window; out.root = e->root; out.time = e->time;
                out.detail = (int)e->button; out.mods = e->state;
                out.x = e->x_root; out.y = e->y_root;
                matrix_require(e->same_screen, "core_button_screen");
            }
            append(trace, &out, false);
            continue;
        }
        if (event.type != GenericEvent || event.xcookie.extension != matrix.opcode) continue;
        matrix_require(XGetEventData(display, &event.xcookie), "xi_cookie_data");
        bool raw;
        if (xi_kind(event.xcookie.evtype, &out.kind, &raw)) {
            matrix_require(raw == observer && (raw || matrix.xi_mode), "unexpected_xi_event_channel");
            if (raw) {
                XIRawEvent *e = event.xcookie.data;
                out.device = e->deviceid; out.source = e->sourceid; out.detail = e->detail;
                out.flags = e->flags; out.time = e->time; out.sent = e->send_event;
                valuators(&out, &e->valuators, e->raw_values);
            } else {
                XIDeviceEvent *e = event.xcookie.data;
                out.device = e->deviceid; out.source = e->sourceid; out.detail = e->detail;
                out.flags = e->flags; out.time = e->time; out.sent = e->send_event;
                out.window = e->event; out.root = e->root; out.mods = e->mods.effective;
                out.x = e->root_x; out.y = e->root_y;
                matrix_require(isfinite(out.x) && isfinite(out.y), "finite_xi_position");
                valuators(&out, &e->valuators, NULL);
            }
            append(trace, &out, raw);
        }
        XFreeEventData(display, &event.xcookie);
    }
}
void matrix_collect(struct Trace *trace, int duration, unsigned stop_target_presses) {
    long deadline = matrix_ms() + duration;
    for (;;) {
        matrix_require(!matrix_interrupted, "interrupted_during_poll");
        receive(matrix.target, trace, false);
        receive(matrix.observe, trace, true);
        if (stop_target_presses) {
            unsigned presses = 0;
            for (unsigned i = 0; i < trace->ntarget; i++) presses += trace->target[i].kind == KEY_DOWN;
            if (presses >= stop_target_presses) return;
        }
        int left = (int)(deadline - matrix_ms());
        if (left <= 0) return;
        struct pollfd fds[] = {{ConnectionNumber(matrix.target), POLLIN, 0},
                              {ConnectionNumber(matrix.observe), POLLIN, 0}};
        int status = poll(fds, 2, left);
        if (status < 0 && errno == EINTR) continue;
        matrix_require(status >= 0, "event_poll_failed");
        for (unsigned i = 0; i < 2; i++)
            matrix_require(!(fds[i].revents & (POLLERR | POLLHUP | POLLNVAL)), "x_socket_closed");
    }
}
void matrix_mode(bool xi) {
    struct Trace leftover = {0};
    matrix_collect(&leftover, 30, 0);
    matrix_require(!leftover.ntarget && !leftover.nraw, "events_leaked_between_cases");
    XSelectInput(matrix.target, matrix.route.window, 0);
    unsigned char bits[XIMaskLen(XI_LASTEVENT)] = {0};
    XIEventMask mask = {XIAllMasterDevices, sizeof(bits), bits};
    if (xi) {
        int types[] = {XI_KeyPress, XI_KeyRelease, XI_ButtonPress, XI_ButtonRelease, XI_Motion};
        for (unsigned i = 0; i < sizeof(types) / sizeof(types[0]); i++) XISetMask(bits, types[i]);
    }
    XISelectEvents(matrix.target, matrix.route.window, &mask, 1);
    if (!xi) XSelectInput(matrix.target, matrix.route.window,
        KeyPressMask | KeyReleaseMask | ButtonPressMask | ButtonReleaseMask | PointerMotionMask);
    matrix.xi_mode = xi;
    matrix_sync(matrix.target);
    Window focus = None;
    matrix_require(XIGetFocus(matrix.inject, matrix.route.keyboard, &focus) == Success
                   && focus == matrix.route.window, "dedicated_keyboard_focus");
}
void matrix_key(unsigned code, bool down) {
    matrix_require(code >= 8 && code < 256, "keycode_range");
    matrix_require(XTestFakeKeyEvent(matrix.inject, code, down, CurrentTime), "xtest_key_request");
    matrix.held_keys[code] = down;
    matrix_sync(matrix.inject);
}
void matrix_button(unsigned code, bool down) {
    matrix_require(code > 0 && code < 256, "button_range");
    matrix_require(XTestFakeButtonEvent(matrix.inject, code, down, CurrentTime), "xtest_button_request");
    matrix.held_buttons[code] = down;
    matrix_sync(matrix.inject);
}
void matrix_trace_report(const struct Trace *trace, const char *origin) {
    const char *name = matrix_cases[matrix.active].name;
    printf("{\"type\":\"trace\",\"case\":\"%s\",\"mode\":\"%s\",\"origin\":\"%s\","
           "\"target_count\":%u,\"raw_count\":%u,\"target\":[", name,
           matrix.xi_mode ? "xi2" : "core", origin, trace->ntarget, trace->nraw);
    const char *kinds[] = {"key_down", "key_up", "button_down", "button_up", "motion"};
    for (unsigned channel = 0; channel < 2; channel++) {
        unsigned n = channel ? trace->nraw : trace->ntarget;
        const struct Event *events = channel ? trace->raw : trace->target;
        if (channel) printf("],\"raw\":[");
        for (unsigned i = 0; i < n; i++) {
            const struct Event *e = &events[i];
            printf("%s{\"kind\":\"%s\",\"detail\":%d,\"device\":%d,\"source\":%d,"
                   "\"time\":%lu,\"flags\":%d,\"mods\":%u,\"xy\":[%.6f,%.6f],"
                   "\"axes\":%u,\"cooked\":[%.6f,%.6f],\"raw\":[%.6f,%.6f]}",
                   i ? "," : "", kinds[e->kind], e->detail, e->device, e->source, e->time,
                   e->flags, e->mods, e->x, e->y, e->axes, e->cooked[0], e->cooked[1], e->raw[0], e->raw[1]);
        }
    }
    puts("]}");
}
