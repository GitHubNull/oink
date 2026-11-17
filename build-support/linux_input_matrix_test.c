/* Pure validator unit tests: no connection, XTEST, native server, GUI or helper.
 * Compile separately from linux_input_matrix.c. Running is left to the caller.
 */
#include "linux_input_matrix_support.h"
#include <assert.h>

static const struct Route route = {10, 11, 12, 13, 100, 1};
static const struct Expected pair[] = {
    {KEY_DOWN, 38, 38, 0, false}, {KEY_UP, 38, 38, 0, false}
};
static struct Trace fixture(bool xi) {
    struct Trace t = {.ntarget = 2, .nraw = 2};
    for (unsigned i = 0; i < 2; i++) {
        t.target[i] = (struct Event){.kind = i ? KEY_UP : KEY_DOWN, .detail = 38,
            .device = xi ? 11 : 0, .source = xi ? 13 : 0, .window = 100, .root = 1};
        t.raw[i] = (struct Event){.kind = i ? KEY_UP : KEY_DOWN, .detail = 38,
            .device = 11, .source = 13};
    }
    return t;
}
static void identity_tests(void) {
    for (unsigned xi = 0; xi < 2; xi++) {
        struct Trace t = fixture(xi != 0);
        assert(matrix_route_ok(&t, &route, xi != 0));
        assert(matrix_sequence_ok(&t, pair, 2, xi != 0));
        t.raw[0].source = 99; /* A correct last source must not hide an earlier wrong source. */
        assert(!matrix_route_ok(&t, &route, xi != 0));
        t.raw[0].source = 13;
        t.raw[0].device = 99;
        assert(!matrix_route_ok(&t, &route, xi != 0));
        t.raw[0].device = 11;
        t.target[0].window = 101;
        assert(!matrix_route_ok(&t, &route, xi != 0));
        t.target[0].window = 100;
        t.target[0].root = 2;
        assert(!matrix_route_ok(&t, &route, xi != 0));
        t.target[0].root = 1;
        t.target[1].sent = true;
        assert(!matrix_route_ok(&t, &route, xi != 0));
        t = fixture(xi != 0);
        if (xi) {
            t.target[0].source = 99;
            assert(!matrix_route_ok(&t, &route, true));
        }
    }
}
static void counts_and_release_tests(void) {
    struct Trace t = fixture(true);
    t.ntarget = 1;
    assert(!matrix_sequence_ok(&t, pair, 2, true));
    t = fixture(true);
    t.nraw = 1;
    assert(!matrix_sequence_ok(&t, pair, 2, true));
    t = fixture(true);
    t.nraw = 3;
    assert(!matrix_sequence_ok(&t, pair, 2, true));
    t = fixture(true);
    t.raw[1].kind = KEY_DOWN;
    assert(!matrix_sequence_ok(&t, pair, 2, true));
    t = fixture(true);
    t.raw[0].detail++;
    assert(!matrix_sequence_ok(&t, pair, 2, true));
    t = fixture(true);
    t.target[0].mods = ShiftMask;
    assert(!matrix_sequence_ok(&t, pair, 2, true));
    t = fixture(true);
    t.raw[0].flags = XIKeyRepeat;
    assert(!matrix_sequence_ok(&t, pair, 2, true));
    t.ntarget = MATRIX_MAX_EVENTS + 1;
    assert(!matrix_route_ok(&t, &route, true));
    assert(!matrix_sequence_ok(&t, pair, MATRIX_MAX_EVENTS + 1, true));
}
static void repeat_tests(void) {
    for (unsigned xi = 0; xi < 2; xi++) {
        struct Trace t = fixture(xi != 0);
        /* Duplicate Down: raw Down/Down/Up, target Down/Up (XKB filtering). */
        t.raw[2] = t.raw[1]; t.raw[1] = t.raw[0]; t.nraw = 3;
        assert(matrix_repeat_ok(&t, &route, xi != 0, false, 38));
        t.raw[1].flags = XIKeyRepeat;
        assert(!matrix_repeat_ok(&t, &route, xi != 0, false, 38));
        t.raw[1].flags = 0;
        t.raw[0].source = 99;
        assert(!matrix_repeat_ok(&t, &route, xi != 0, false, 38));
        t = fixture(xi != 0);
        /* Auto-repeat: raw has ONLY Down/Up; extra target presses originate
         * at the master timer, not the XTEST slave. */
        t.target[3] = t.target[1];
        t.target[1] = t.target[2] = t.target[0];
        for (unsigned i = 1; i < 3; i++) {
            t.target[i].source = xi ? route.keyboard : 0;
            t.target[i].flags = xi ? XIKeyRepeat : 0;
        }
        t.ntarget = 4;
        assert(matrix_repeat_ok(&t, &route, xi != 0, true, 38));
        assert(!matrix_repeat_ok(&t, &route, xi != 0, false, 38));
        t.nraw = 3;
        assert(!matrix_repeat_ok(&t, &route, xi != 0, true, 38));
        t.nraw = 2;
        t.target[3].kind = KEY_DOWN;
        assert(!matrix_repeat_ok(&t, &route, xi != 0, true, 38));
        t.target[3].kind = KEY_UP;
        if (xi) {
            t.target[1].source = route.key_source;
            assert(!matrix_repeat_ok(&t, &route, true, true, 38));
            t.target[1].source = route.keyboard;
            t.target[1].flags = 0;
            assert(!matrix_repeat_ok(&t, &route, true, true, 38));
        }
    }
}
static void motion_and_wheel_tests(void) {
    struct Trace t = {.ntarget = 1, .nraw = 1};
    t.target[0] = (struct Event){.kind = MOTION, .window = 100, .root = 1, .x = 313, .y = 207};
    t.raw[0] = (struct Event){.kind = MOTION, .device = 10, .source = 12, .axes = 3, .raw = {13, 7}};
    assert(matrix_route_ok(&t, &route, false));
    assert(matrix_motion_ok(&t, 300, 200, 13, 7));
    t.raw[0].axes = 1;
    assert(!matrix_motion_ok(&t, 300, 200, 13, 7));
    t.raw[0].axes = 7;
    assert(!matrix_motion_ok(&t, 300, 200, 13, 7));
    t.raw[0].axes = 3;
    t.raw[0].raw[0] = 12;
    assert(!matrix_motion_ok(&t, 300, 200, 13, 7));
    t.raw[0].raw[0] = NAN;
    assert(!matrix_motion_ok(&t, 300, 200, 13, 7));
    t.raw[0].raw[0] = 13;
    t.target[0].x = 312;
    assert(!matrix_motion_ok(&t, 300, 200, 13, 7));
    assert(!strcmp(matrix_wheel_name(4), "vertical_up"));
    assert(!strcmp(matrix_wheel_name(5), "vertical_down"));
    assert(!strcmp(matrix_wheel_name(6), "horizontal_left"));
    assert(!strcmp(matrix_wheel_name(7), "horizontal_right"));
    assert(matrix_wheel_name(3) == NULL && matrix_wheel_name(8) == NULL);
    struct Expected wheel[] = {{BUTTON_DOWN, 6, 7, 0, false}, {BUTTON_UP, 6, 7, 0, false}};
    t = fixture(true);
    for (unsigned i = 0; i < 2; i++) {
        t.target[i].kind = t.raw[i].kind = i ? BUTTON_UP : BUTTON_DOWN;
        t.target[i].detail = 6; t.raw[i].detail = 7;
        t.target[i].device = t.raw[i].device = 10;
        t.target[i].source = t.raw[i].source = 12;
    }
    assert(matrix_route_ok(&t, &route, true));
    assert(matrix_sequence_ok(&t, wheel, 2, true));
    t.raw[1].detail = 6;
    assert(!matrix_sequence_ok(&t, wheel, 2, true));
}
int main(void) {
    identity_tests();
    counts_and_release_tests();
    repeat_tests();
    motion_and_wheel_tests();
    puts("{\"type\":\"unit_summary\",\"exit\":0,\"native_executed\":false}");
    return 0;
}
