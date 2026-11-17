#include "linux_input_matrix_support.h"
#include <X11/keysym.h>

/* Only directly reachable base-level symbols in the actual active group are
 * injectable without extra modifiers. Never equate Alt_R with ISO_Level3_Shift,
 * and never install a made-up mapping just to manufacture a passing result. */
static unsigned mapped_code(KeySym symbol) {
    for (unsigned code = matrix.map->min_key_code; code <= matrix.map->max_key_code; code++) {
        if (matrix.group < (unsigned)XkbKeyNumGroups(matrix.map, code)
                && XkbKeySymEntry(matrix.map, code, 0, matrix.group) == symbol) return code;
    }
    return 0;
}
static bool symbol_anywhere(KeySym symbol) {
    for (unsigned code = matrix.map->min_key_code; code <= matrix.map->max_key_code; code++)
        for (int i = 0; i < XkbKeyNumSyms(matrix.map, code); i++)
            if (XkbKeySymsPtr(matrix.map, code)[i] == symbol) return true;
    return false;
}
static void key_state(unsigned code, bool down) {
    char bits[32];
    XQueryKeymap(matrix.inject, bits);
    matrix_require(((bits[code / 8] & (1u << (code % 8))) != 0) == down,
                   "owned_master_key_down_state");
    matrix_sync(matrix.inject);
}
static void modifiers(unsigned expected) {
    XkbStateRec state;
    matrix_require(XkbGetState(matrix.inject, matrix.route.keyboard, &state) == Success,
                   "read_owned_keyboard_state");
    matrix_require(state.mods == expected && state.base_mods == expected
                   && !state.latched_mods && !state.locked_mods && state.group == matrix.group,
                   "owned_xkb_modifiers_and_group");
}
static bool repeat_save(void) {
    if (matrix.saved[0] && matrix.saved[1]) return true;
    int devices[] = {matrix.route.key_source, matrix.route.keyboard};
    for (unsigned i = 0; i < 2; i++) {
        matrix.saved[i] = XkbAllocKeyboard();
        matrix_require(matrix.saved[i] != NULL, "allocate_saved_repeat_controls");
        matrix.saved[i]->dpy = matrix.inject;
        matrix.saved[i]->device_spec = (unsigned short)devices[i];
        if (XkbGetControls(matrix.inject, XkbAllControlsMask, matrix.saved[i]) != Success) return false;
    }
    return true;
}
static void repeat_timing(unsigned code, unsigned delay, unsigned interval) {
    for (unsigned i = 0; i < 2; i++) {
        XkbDescRec edit = *matrix.saved[i];
        XkbControlsRec controls = *edit.ctrls;
        edit.ctrls = &controls;
        controls.repeat_delay = (unsigned short)delay;
        controls.repeat_interval = (unsigned short)interval;
        controls.enabled_ctrls |= XkbRepeatKeysMask;
        controls.per_key_repeat[code / 8] |= (unsigned char)(1u << (code % 8));
        matrix.changed[i] = true; /* Also restore after a rejected/partial request. */
        matrix_require(XkbSetControls(matrix.inject, MATRIX_REPEAT_MASK, &edit), "set_owned_repeat_timing");
        matrix_sync(matrix.inject);
        matrix_require(XkbGetControls(matrix.inject, XkbAllControlsMask, &edit) == Success,
                       "read_back_owned_repeat_controls");
        matrix_require(controls.repeat_delay == delay && controls.repeat_interval == interval
                       && (controls.enabled_ctrls & XkbRepeatKeysMask)
                       && (controls.per_key_repeat[code / 8] & (1u << (code % 8))), "repeat_timing_applied");
    }
}
void matrix_repeat_restore(void) {
    /* Restore master before slave, so master propagation cannot overwrite the
     * slave's separately saved controls after they have been checked. */
    for (int i = 1; i >= 0; i--) {
        if (!matrix.saved[i]) continue;
        if (matrix.changed[i]) {
            if (!XkbSetControls(matrix.inject, MATRIX_REPEAT_MASK, matrix.saved[i])) matrix.xerror = 1;
            XSync(matrix.inject, False);
            XkbDescRec check = *matrix.saved[i];
            XkbControlsRec controls = *check.ctrls;
            check.ctrls = &controls;
            if (XkbGetControls(matrix.inject, XkbAllControlsMask, &check) != Success
                    || controls.repeat_delay != matrix.saved[i]->ctrls->repeat_delay
                    || controls.repeat_interval != matrix.saved[i]->ctrls->repeat_interval
                    || controls.enabled_ctrls != matrix.saved[i]->ctrls->enabled_ctrls
                    || memcmp(controls.per_key_repeat, matrix.saved[i]->ctrls->per_key_repeat, 32))
                matrix.xerror = 1;
        }
        XkbFreeKeyboard(matrix.saved[i], XkbAllComponentsMask, True);
        matrix.saved[i] = NULL;
        matrix.changed[i] = false;
    }
}
static void transitions(struct Case *test, unsigned code, unsigned anchor, unsigned mask) {
    struct Trace trace = {0};
    struct Expected sequence[4];
    modifiers(0);
    key_state(code, false);
    matrix_key(code, true);
    key_state(code, true);
    unsigned n;
    if (test->kind == MODIFIER) {
        modifiers(mask);
        matrix_key(anchor, true);
        matrix_key(anchor, false);
        sequence[0] = (struct Expected){KEY_DOWN, (int)code, (int)code, 0, false};
        sequence[1] = (struct Expected){KEY_DOWN, (int)anchor, (int)anchor, mask, false};
        sequence[2] = (struct Expected){KEY_UP, (int)anchor, (int)anchor, mask, false};
        sequence[3] = (struct Expected){KEY_UP, (int)code, (int)code, mask, false};
        n = 4;
    } else {
        sequence[0] = (struct Expected){KEY_DOWN, (int)code, (int)code, 0, false};
        sequence[1] = (struct Expected){KEY_UP, (int)code, (int)code, 0, false};
        n = 2;
    }
    matrix_key(code, false);
    matrix_collect(&trace, 100, 0);
    matrix_trace_report(&trace, "xtest_press_release");
    matrix_require(matrix_route_ok(&trace, &matrix.route, matrix.xi_mode)
                   && matrix_sequence_ok(&trace, sequence, n, matrix.xi_mode),
                   "exact_key_sources_sequence_counts_details_modifiers_and_no_repeat");
    key_state(code, false);
    if (test->kind == MODIFIER) key_state(anchor, false);
    modifiers(0);
}
static void repeat(struct Case *test, unsigned code) {
    const bool server = test->kind == SERVER_REPEAT;
    const unsigned delay = server ? 200 : 5000, interval = server ? 100 : 1000;
    repeat_timing(code, delay, interval);
    modifiers(0);
    key_state(code, false);
    struct Trace trace = {0};
    long start = matrix_ms();
    matrix_key(code, true);
    if (server) {
        /* Exactly ONE injected Down. Further target presses are generated by
         * Xorg's master software timer, without additional raw events.
         * Stop after >=3 target presses; require exactly 2 raw transitions and
         * one target release, but do not assume an exact number of timer ticks. */
        matrix_collect(&trace, 1800, 3);
    } else {
        matrix_collect(&trace, 30, 0);
        matrix_key(code, true); /* Deliberate duplicate Down, NOT an Up/Down pair. */
        matrix_collect(&trace, 30, 0);
    }
    key_state(code, true);
    matrix_key(code, false);
    long held_ms = matrix_ms() - start;
    matrix_collect(&trace, server ? 350 : 100, 0);
    matrix_trace_report(&trace, server ? "single_down_server_timer" : "two_explicit_down_requests");
    unsigned n = trace.ntarget;
    matrix_require(server || held_ms < (long)delay, "manual_downs_completed_before_timer_delay");
    matrix_require(matrix_repeat_ok(&trace, &matrix.route, matrix.xi_mode, server, (int)code),
                   "xorg_repeat_exact_sources_target_raw_counts_flags_and_release");
    if (server) {
        uint32_t first_delay = (uint32_t)(trace.target[1].time - trace.target[0].time);
        matrix_require(first_delay >= delay, "server_repeat_not_before_configured_delay");
        for (unsigned i = 2; i + 1 < n; i++)
            matrix_require((uint32_t)(trace.target[i].time - trace.target[i - 1].time) >= interval,
                           "server_repeat_interval");
    }
    key_state(code, false);
    modifiers(0);
    struct Trace quiet = {0};
    matrix_collect(&quiet, server ? 220 : 30, 0);
    matrix_require(!quiet.ntarget && !quiet.nraw, "no_repeat_after_release");
    printf("{\"type\":\"repeat_observation\",\"case\":\"%s\",\"mode\":\"%s\","
           "\"injected_downs\":%d,\"injected_ups\":1,\"observed_presses\":%u,"
           "\"observed_releases\":1,\"delay_ms\":%u,\"interval_ms\":%u,\"held_ms\":%ld,"
           "\"raw_presses\":%u,\"raw_releases\":1,\"xorg_software_repeat_model\":true,"
           "\"origin_from_schedule_not_repeat_flag_alone\":true}\n", test->name,
           matrix.xi_mode ? "xi2" : "core", server ? 1 : 2, n - 1, delay, interval, held_ms, trace.nraw - 1);
    matrix_repeat_restore();
    matrix_require(!matrix.xerror, "owned_repeat_controls_restored");
}
void matrix_keyboard(struct Case *test) {
    unsigned code = mapped_code(test->symbol);
    if (!code) {
        matrix_skip(test, symbol_anywhere(test->symbol) ? "symbol_not_directly_reachable_in_active_group"
                                                     : "keysym_mapping_missing");
        return;
    }
    unsigned anchor = mapped_code(XK_a), mask = matrix.map->map->modmap[code];
    if (test->kind == MODIFIER && (!mask || !anchor || code == anchor)) {
        matrix_skip(test, "modifier_mask_or_chord_anchor_unavailable");
        return;
    }
    if (test->kind == MODIFIER && test->argument)
        matrix_require(mask == (unsigned)test->argument, "shift_control_semantic_modifier_mask");
    bool repeating = test->kind == MANUAL_REPEAT || test->kind == SERVER_REPEAT;
    if (repeating && !matrix.detectable) {
        matrix_skip(test, "detectable_core_repeat_unavailable");
        return;
    }
    printf("{\"type\":\"key_mapping\",\"case\":\"%s\",\"keysym\":%lu,\"keycode\":%u,"
           "\"group\":%u,\"level\":0,\"modifier_mask\":%u,\"alt_r_keycode\":%u,"
           "\"iso_level3_keycode\":%u}\n", test->name, test->symbol, code, matrix.group, mask,
           mapped_code(XK_Alt_R), mapped_code(XK_ISO_Level3_Shift));
    for (unsigned mode = 0; mode < 2; mode++) {
        matrix_mode(mode != 0);
        if (repeating) {
            if (!repeat_save()) {
                matrix_repeat_restore();
                matrix_skip(test, "owned_repeat_controls_unavailable");
                return;
            }
            repeat(test, code);
        } else transitions(test, code, anchor, mask);
        test->modes |= 1u << mode;
    }
}
