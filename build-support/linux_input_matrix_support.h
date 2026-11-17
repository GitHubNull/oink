#ifndef OINK_LINUX_INPUT_MATRIX_H
#define OINK_LINUX_INPUT_MATRIX_H
#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/XKBlib.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <signal.h>

extern volatile sig_atomic_t matrix_interrupted;

/* Synthetic X11 only. No grabs/replay, HID, helper, toolkit or product transport.
 * The runner MUST own and tear down the entire private server on every exit,
 * including SIGKILL/disconnect; process-local cleanup cannot cover those exits.
 * JSON Lines on stdout; exit 0=all requested cases passed, 77=incomplete,
 * 1=assertion/runtime/cleanup failure, 2=refused isolation. Scope exclusions do
 * not change exit 0; missing mappings/capabilities DO change it to 77.
 */
#define MATRIX_MAX_EVENTS 128
#define MATRIX_REPEAT_MASK (XkbRepeatKeysMask | XkbControlsEnabledMask | XkbPerKeyRepeatMask)
enum Kind { KEY_DOWN, KEY_UP, BUTTON_DOWN, BUTTON_UP, MOTION };
enum MatrixStatus { PENDING, PASSED, NOT_TESTED, FAILED };
enum TestKind { MODIFIER, KEY, MANUAL_REPEAT, SERVER_REPEAT, RELATIVE, WHEEL };
struct Case {
    const char *name;
    enum TestKind kind;
    KeySym symbol;
    int argument;
    enum MatrixStatus status;
    unsigned modes;
    const char *reason;
};
struct Event {
    enum Kind kind;
    int detail, device, source, flags;
    unsigned mods, axes;
    bool sent;
    Window window, root;
    Time time;
    double x, y, cooked[2], raw[2];
};
struct Trace {
    struct Event target[MATRIX_MAX_EVENTS], raw[MATRIX_MAX_EVENTS];
    unsigned ntarget, nraw;
};
struct Expected { enum Kind kind; int detail, raw_detail; unsigned mods; bool repeat; };
struct Route { int pointer, keyboard, pointer_source, key_source; Window window, root; };
struct Matrix {
    Display *target, *observe, *inject;
    struct Route route;
    int opcode, original_pointer, xerror, xi_major, xi_minor, active;
    bool xi_mode, detectable, disconnected, cleaning;
    bool held_keys[256], held_buttons[256];
    XkbDescPtr map, saved[2];
    bool changed[2];
    unsigned group;
    int button_count, mapping_count, relative_axes;
    unsigned char button_map[256];
};
extern struct Matrix matrix;
extern struct Case matrix_cases[];
extern const unsigned matrix_case_count;

static inline bool matrix_is_key(enum Kind kind) { return kind == KEY_DOWN || kind == KEY_UP; }
/* Core has no sourceid. Attribution is a paired, dedicated-master raw trace,
 * not a claim that a core event itself contains a device identity. */
static inline bool matrix_route_ok(const struct Trace *t, const struct Route *r, bool xi) {
    if (t->ntarget > MATRIX_MAX_EVENTS || t->nraw > MATRIX_MAX_EVENTS) return false;
    for (unsigned channel = 0; channel < 2; channel++) {
        unsigned n = channel ? t->nraw : t->ntarget;
        const struct Event *events = channel ? t->raw : t->target;
        for (unsigned i = 0; i < n; i++) {
            const struct Event *e = &events[i];
            if (e->sent) return false;
            if (!channel && (e->window != r->window || e->root != r->root)) return false;
            if (channel || xi) {
                int master = matrix_is_key(e->kind) ? r->keyboard : r->pointer;
                int source = matrix_is_key(e->kind) ? r->key_source : r->pointer_source;
                if (e->device != master || e->source != source) return false;
            }
        }
    }
    return true;
}
static inline bool matrix_sequence_ok(const struct Trace *t, const struct Expected *s,
                                      unsigned n, bool xi) {
    if (n > MATRIX_MAX_EVENTS || t->ntarget != n || t->nraw != n) return false;
    for (unsigned i = 0; i < n; i++) {
        const struct Event *a = &t->target[i], *b = &t->raw[i];
        if (a->kind != s[i].kind || b->kind != s[i].kind || a->detail != s[i].detail
                || b->detail != s[i].raw_detail || a->mods != s[i].mods || a->time != b->time) return false;
        if (matrix_is_key(a->kind)) {
            if (((b->flags & XIKeyRepeat) != 0) != s[i].repeat) return false;
            if (xi && (((a->flags & XIKeyRepeat) != 0) != s[i].repeat)) return false;
        }
    }
    return true;
}
/* Xorg's AccessX software timer bypasses raw generation and creates a master
 * sourced XI2 repeat. A duplicate XTEST Down instead reaches raw but is filtered
 * by XKB at the target. Neither path may be inferred from XIKeyRepeat alone. */
static inline bool matrix_repeat_ok(const struct Trace *t, const struct Route *r,
                                    bool xi, bool server, int code) {
    if (t->nraw != (server ? 2u : 3u)) return false;
    if (server ? (t->ntarget < 4 || t->ntarget > 24) : t->ntarget != 2) return false;
    if (t->target[0].time != t->raw[0].time
            || t->target[t->ntarget - 1].time != t->raw[t->nraw - 1].time) return false;
    for (unsigned channel = 0; channel < 2; channel++) {
        unsigned n = channel ? t->nraw : t->ntarget;
        const struct Event *events = channel ? t->raw : t->target;
        for (unsigned i = 0; i < n; i++) {
            const struct Event *e = &events[i];
            bool repeat = !channel && server && i > 0 && i + 1 < n;
            if (e->kind != (i + 1 == n ? KEY_UP : KEY_DOWN) || e->detail != code
                    || e->sent || e->mods) return false;
            if (!channel && (e->window != r->window || e->root != r->root)) return false;
            if (channel || xi) {
                int source = repeat ? r->keyboard : r->key_source;
                if (e->device != r->keyboard || e->source != source
                        || (((e->flags & XIKeyRepeat) != 0) != repeat)) return false;
            }
        }
    }
    return true;
}
static inline bool matrix_motion_ok(const struct Trace *t, double x, double y, int dx, int dy) {
    if (t->ntarget != 1 || t->nraw != 1) return false;
    const struct Event *a = &t->target[0], *b = &t->raw[0];
    return a->kind == MOTION && b->kind == MOTION && b->axes == 3 && a->time == b->time
        && isfinite(a->x) && isfinite(a->y) && isfinite(b->raw[0]) && isfinite(b->raw[1])
        && fabs(b->raw[0] - dx) < 0.001 && fabs(b->raw[1] - dy) < 0.001
        && fabs(a->x - x - dx) < 0.001 && fabs(a->y - y - dy) < 0.001;
}
static inline const char *matrix_wheel_name(int button) {
    switch (button) {
    case 4: return "vertical_up";
    case 5: return "vertical_down";
    case 6: return "horizontal_left";
    case 7: return "horizontal_right";
    default: return NULL;
    }
}
void matrix_require(bool condition, const char *reason);
_Noreturn void matrix_finish(int result, const char *reason);
void matrix_sync(Display *display);
long matrix_ms(void);
void matrix_collect(struct Trace *trace, int duration, unsigned stop_target_presses);
void matrix_mode(bool xi);
void matrix_key(unsigned code, bool down);
void matrix_button(unsigned code, bool down);
void matrix_trace_report(const struct Trace *trace, const char *origin);
void matrix_skip(struct Case *test, const char *reason);
void matrix_keyboard(struct Case *test);
void matrix_pointer(struct Case *test);
void matrix_repeat_restore(void);
#endif
