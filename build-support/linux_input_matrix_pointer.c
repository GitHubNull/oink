#include "linux_input_matrix_support.h"

static void position(double *x, double *y, int button, bool down) {
    Window root, child;
    double wx, wy;
    XIButtonState buttons = {0};
    XIModifierState mods;
    XIGroupState group;
    matrix_require(XIQueryPointer(matrix.inject, matrix.route.pointer, matrix.route.window,
        &root, &child, x, y, &wx, &wy, &buttons, &mods, &group), "query_owned_pointer");
    bool held = button >= 0 && button < buttons.mask_len * 8
        && XIMaskIsSet(buttons.mask, button);
    if (buttons.mask) XFree(buttons.mask);
    matrix_require(root == matrix.route.root && isfinite(*x) && isfinite(*y)
                   && wx > 20 && wx < 580 && wy > 20 && wy < 380, "owned_pointer_inside_target");
    matrix_require(!mods.effective, "pointer_modifiers_neutral");
    if (button > 0) matrix_require(held == down, "owned_pointer_button_state");
    matrix_sync(matrix.inject);
}
static void relative_motion(struct Case *test) {
    int dx = test->argument > 0 ? 13 : -9, dy = test->argument > 0 ? 7 : -5;
    double x, y, after_x, after_y;
    position(&x, &y, 0, false);
    struct Trace trace = {0};
    matrix_require(XTestFakeRelativeMotionEvent(matrix.inject, dx, dy, CurrentTime),
                   "xtest_relative_motion_request");
    matrix_sync(matrix.inject);
    matrix_collect(&trace, 100, 0);
    position(&after_x, &after_y, 0, false);
    matrix_trace_report(&trace, "xtest_relative_motion");
    printf("{\"type\":\"relative_observation\",\"case\":\"%s\",\"mode\":\"%s\","
           "\"requested_delta\":[%d,%d],\"before\":[%.6f,%.6f],\"after\":[%.6f,%.6f],"
           "\"observed_delta\":[%.6f,%.6f],\"physical_acceleration_proven\":false}\n",
           test->name, matrix.xi_mode ? "xi2" : "core", dx, dy, x, y, after_x, after_y,
           after_x - x, after_y - y);
    matrix_require(matrix_route_ok(&trace, &matrix.route, matrix.xi_mode)
                   && matrix_motion_ok(&trace, x, y, dx, dy),
                   "one_target_one_raw_relative_event_sources_and_exact_numeric_deltas");
    matrix_require(fabs(trace.target[0].x - after_x) < 0.001
                   && fabs(trace.target[0].y - after_y) < 0.001, "motion_event_matches_queried_pointer");
}
static int physical_wheel(int logical) {
    int result = 0;
    for (int i = 0; i < matrix.mapping_count && i < matrix.button_count && i < 255; i++) {
        if (matrix.button_map[i] != logical) continue;
        if (result) return 0; /* Ambiguous mapping must not manufacture evidence. */
        result = i + 1;
    }
    return result;
}
static void wheel(struct Case *test, int physical) {
    int logical = test->argument;
    double x, y;
    struct Trace trace = {0};
    position(&x, &y, logical, false);
    matrix_button((unsigned)physical, true);
    position(&x, &y, logical, true);
    matrix_button((unsigned)physical, false);
    matrix_collect(&trace, 100, 0);
    matrix_trace_report(&trace, "xtest_legacy_wheel_button");
    /* Core state includes buttons 1..5; XI2 keeps buttons separate from mods. */
    unsigned release_state = !matrix.xi_mode && logical <= 5 ? 1u << (logical + 7) : 0;
    struct Expected sequence[] = {
        {BUTTON_DOWN, logical, physical, 0, false},
        {BUTTON_UP, logical, physical, release_state, false}
    };
    matrix_require(matrix_route_ok(&trace, &matrix.route, matrix.xi_mode)
                   && matrix_sequence_ok(&trace, sequence, 2, matrix.xi_mode),
                   "exact_wheel_sources_press_release_counts_and_logical_raw_details");
    matrix_require(!trace.raw[0].axes && !trace.raw[1].axes, "legacy_wheel_is_not_scroll_valuator");
    position(&x, &y, logical, false);
    printf("{\"type\":\"wheel_observation\",\"case\":\"%s\",\"mode\":\"%s\","
           "\"physical_button\":%d,\"logical_button\":%d,\"classification\":\"%s\","
           "\"legacy_steps\":1,\"presses\":1,\"releases\":1,\"fine_scroll_proven\":false}\n",
           test->name, matrix.xi_mode ? "xi2" : "core", physical, logical, matrix_wheel_name(logical));
}
void matrix_pointer(struct Case *test) {
    int physical = 0;
    if (test->kind == RELATIVE && matrix.relative_axes != 3) {
        matrix_skip(test, "xtest_relative_xy_valuators_unavailable");
        return;
    }
    if (test->kind == WHEEL) {
        physical = physical_wheel(test->argument);
        if (!physical) {
            matrix_skip(test, "legacy_wheel_button_mapping_unavailable");
            return;
        }
    }
    for (unsigned mode = 0; mode < 2; mode++) {
        matrix_mode(mode != 0);
        if (test->kind == RELATIVE) relative_motion(test);
        else wheel(test, physical);
        test->modes |= 1u << mode;
    }
}
