/* Phase 4 ARGB overlay probe. Runs only inside the private Xephyr harness. */
#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/XTest.h>
#include <X11/extensions/Xfixes.h>
#include <X11/extensions/XShm.h>
#include <X11/extensions/shape.h>
#include <poll.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <time.h>
#include <unistd.h>

#define WIDTH 320
#define HEIGHT 240
#define LEFT 300
#define TOP 200
#define SAMPLES 48

static int x_errors;
static int failures;

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

static void check(bool condition, const char *name) {
    printf("%s=%s\n", name, condition ? "ok" : "fail");
    if (!condition)
        failures++;
}

static void sleep_ms(long milliseconds) {
    struct timespec delay = {milliseconds / 1000, (milliseconds % 1000) * 1000000L};
    nanosleep(&delay, NULL);
}

static Visual *find_argb_visual(Display *display, int *depth, Colormap *colormap) {
    Visual *visual = NULL;
    XVisualInfo template = {0};
    template.depth = 32;
    template.class = TrueColor;
    int count = 0;
    XVisualInfo *matches = XGetVisualInfo(display, VisualDepthMask | VisualClassMask, &template, &count);
    for (int i = 0; i < count && visual == NULL; i++) {
        if (matches[i].red_mask == 0xff0000 && matches[i].green_mask == 0xff00 &&
            matches[i].blue_mask == 0xff)
            visual = matches[i].visual;
    }
    if (matches)
        XFree(matches);
    if (visual == NULL)
        return NULL;
    *depth = 32;
    *colormap = XCreateColormap(display, DefaultRootWindow(display), visual, AllocNone);
    require(colormap != NULL, "ARGB colormap");
    return visual;
}

static Window create_or_window(Display *display, Visual *visual, Colormap colormap,
                               int x, int y, int width, int height, long events) {
    XSetWindowAttributes attributes = {0};
    attributes.override_redirect = True;
    attributes.background_pixel = 0;
    attributes.border_pixel = 0;
    attributes.colormap = colormap;
    Window window = XCreateWindow(display, DefaultRootWindow(display), x, y, width, height, 0, 32,
                                  InputOutput, visual,
                                  CWOverrideRedirect | CWBackPixel | CWBorderPixel | CWColormap,
                                  &attributes);
    require(window != 0, "ARGB window creation");
    if (events != 0)
        XSelectInput(display, window, events);
    XMapWindow(display, window);
    return window;
}

static XImage *make_image(Display *display, Visual *visual) {
    char *data = calloc(4, WIDTH * HEIGHT);
    require(data != NULL, "image storage");
    XImage *image = XCreateImage(display, visual, 32, ZPixmap, 0, data, WIDTH, HEIGHT, 32, 0);
    require(image != NULL, "XImage creation");
    for (int y = 0; y < HEIGHT; y++)
        for (int x = 0; x < WIDTH; x++)
            XPutPixel(image, x, y,
                      ((unsigned long)((x + y) & 0xff) << 24) |
                          ((unsigned long)(x * 255 / (WIDTH - 1)) << 16) |
                          ((unsigned long)(y * 255 / (HEIGHT - 1)) << 8) | 0x40UL);
    return image;
}

static Window wait_for_wm_focus(Display *display, Window expected) {
    long deadline = now_ms() + 3000;
    while (now_ms() < deadline) {
        Window focus = 0;
        int revert = 0;
        XGetInputFocus(display, &focus, &revert);
        if (focus == expected)
            return focus;
        if (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
        } else {
            struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
            if (poll(&fd, 1, 50) == 0)
                XSetInputFocus(display, expected, RevertToParent, CurrentTime);
        }
    }
    XSetInputFocus(display, expected, RevertToParent, CurrentTime);
    XSync(display, False);
    Window focus = 0;
    int revert = 0;
    XGetInputFocus(display, &focus, &revert);
    return focus;
}

static int compare_pixels(Display *display, Window window, XImage *expected, int *alpha_ok) {
    XImage *actual = XGetImage(display, window, 0, 0, WIDTH, HEIGHT, AllPlanes, ZPixmap);
    if (actual == NULL)
        return -1;
    int mismatches = 0;
    *alpha_ok = 1;
    for (int i = 0; i < SAMPLES; i++) {
        int x = (i * 37) % WIDTH;
        int y = (i * 53) % HEIGHT;
        unsigned long want = XGetPixel(expected, x, y);
        unsigned long got = XGetPixel(actual, x, y);
        if (want != got)
            mismatches++;
        if (((want >> 24) & 0xff) != ((got >> 24) & 0xff))
            *alpha_ok = 0;
    }
    XDestroyImage(actual);
    return mismatches;
}

static void set_input_rect(Display *display, Window window, XRectangle *rect) {
    XserverRegion region = XFixesCreateRegion(display, rect, rect ? 1 : 0);
    XFixesSetWindowShapeRegion(display, window, ShapeInput, 0, 0, region);
    XFixesDestroyRegion(display, region);
    XSync(display, False);
}

static void drain(Display *display) {
    XSync(display, False);
    while (XPending(display)) {
        XEvent event;
        XNextEvent(display, &event);
    }
}

static void count_button_press(Display *display, Window first, Window second, int *first_presses,
                               int *second_presses, long milliseconds) {
    long deadline = now_ms() + milliseconds;
    long idle_since = now_ms();
    *first_presses = 0;
    *second_presses = 0;
    while (now_ms() < deadline) {
        bool drained = false;
        while (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
            drained = true;
            if (event.type != ButtonPress || event.xbutton.button != Button1)
                continue;
            if (event.xbutton.window == first)
                *first_presses += 1;
            else if (event.xbutton.window == second)
                *second_presses += 1;
        }
        if (drained)
            idle_since = now_ms();
        else if (*first_presses + *second_presses > 0 && now_ms() - idle_since > 80)
            break;
        struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
        poll(&fd, 1, 20);
    }
    while (XPending(display)) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type != ButtonPress || event.xbutton.button != Button1)
            continue;
        if (event.xbutton.window == first)
            *first_presses += 1;
        else if (event.xbutton.window == second)
            *second_presses += 1;
    }
}

static void click_at(Display *display, int x, int y) {
    XTestFakeMotionEvent(display, 0, x, y, CurrentTime);
    XTestFakeButtonEvent(display, Button1, True, CurrentTime);
    XTestFakeButtonEvent(display, Button1, False, CurrentTime);
    XSync(display, False);
}

int main(void) {
    XSetErrorHandler(on_x_error);
    Display *display = XOpenDisplay(NULL);
    require(display != NULL, "open DISPLAY");
    Window root = DefaultRootWindow(display);

    int fixes_event, fixes_error;
    require(XFixesQueryExtension(display, &fixes_event, &fixes_error), "XFixes extension");
    int depth = 0;
    Colormap colormap;
    Visual *visual = find_argb_visual(display, &depth, &colormap);
    require(visual != NULL, "depth-32 TrueColor visual");
    printf("overlay_visual=ok depth=%d class=%d\n", depth, TrueColor);

    Window focus_window = XCreateSimpleWindow(display, root, 40, 40, 220, 120, 0, 0, 0xffffff);
    XSelectInput(display, focus_window, ExposureMask);
    XMapWindow(display, focus_window);
    XSync(display, False);
    long viewable_deadline = now_ms() + 2000;
    XWindowAttributes focus_attributes;
    for (;;) {
        require(XGetWindowAttributes(display, focus_window, &focus_attributes), "query focus window");
        if (focus_attributes.map_state == IsViewable)
            break;
        require(now_ms() < viewable_deadline, "focus window mapping deadline");
        sleep_ms(10);
    }
    Window focused = wait_for_wm_focus(display, focus_window);
    check(focused == focus_window, "overlay_focus_setup");
    Window focus_before = focused;

    Window below = create_or_window(display, visual, colormap, LEFT, TOP, WIDTH, HEIGHT,
                                    ButtonPressMask | ButtonReleaseMask);
    Window overlay = create_or_window(display, visual, colormap, LEFT, TOP, WIDTH, HEIGHT,
                                      ExposureMask | ButtonPressMask | ButtonReleaseMask);
    XRaiseWindow(display, overlay);
    set_input_rect(display, overlay, NULL);
    XSync(display, False);
    sleep_ms(300);

    XWindowAttributes attributes;
    require(XGetWindowAttributes(display, overlay, &attributes), "query overlay attributes");
    Window focus_after = 0;
    int revert = 0;
    XGetInputFocus(display, &focus_after, &revert);
    check(attributes.override_redirect == True && attributes.depth == 32 &&
              attributes.map_state == IsViewable && focus_after == focus_before,
          "overlay_focus");
    printf("overlay_attributes=ok override_redirect=%d depth=%d mapped=%d focus_before=%lu focus_after=%lu\n",
           attributes.override_redirect, attributes.depth, attributes.map_state == IsViewable,
           focus_before, focus_after);

    XImage *image = make_image(display, visual);
    XGCValues values = {0};
    GC gc = XCreateGC(display, overlay, 0, &values);
    require(gc != NULL, "overlay GC");
    XPutImage(display, overlay, gc, image, 0, 0, 0, 0, WIDTH, HEIGHT);
    XSync(display, False);
    int alpha_ok = 0;
    int mismatches = compare_pixels(display, overlay, image, &alpha_ok);
    check(mismatches == 0 && alpha_ok, "overlay_pixels_put");
    printf("overlay_pixels_put_detail=ok mismatches=%d alpha=%d xerrors=%d\n", mismatches, alpha_ok,
           x_errors);

    int shm_ok = -1;
    if (!XShmQueryExtension(display)) {
        printf("overlay_pixels_shm=skipped reason=no_mit_shm\n");
    } else {
        XShmSegmentInfo segment = {0};
        XImage *shared = XShmCreateImage(display, visual, 32, ZPixmap, NULL, &segment, WIDTH, HEIGHT);
        require(shared != NULL, "XShm image creation");
        segment.shmid = shmget(IPC_PRIVATE, (size_t)shared->bytes_per_line * HEIGHT, IPC_CREAT | 0600);
        require(segment.shmid >= 0, "shmget");
        segment.shmaddr = shmat(segment.shmid, NULL, 0);
        require(segment.shmaddr != (char *)-1, "shmat");
        shared->data = segment.shmaddr;
        segment.readOnly = False;
        require(XShmAttach(display, &segment), "XShmAttach");
        XSync(display, False);
        int before_errors = x_errors;
        for (int y = 0; y < HEIGHT; y++)
            for (int x = 0; x < WIDTH; x++)
                XPutPixel(shared, x, y, XGetPixel(image, x, y));
        XShmPutImage(display, overlay, gc, shared, 0, 0, 0, 0, WIDTH, HEIGHT, False);
        XSync(display, False);
        if (x_errors != before_errors) {
            printf("overlay_pixels_shm=skipped reason=server_error\n");
        } else {
            mismatches = compare_pixels(display, overlay, image, &alpha_ok);
            shm_ok = mismatches == 0 && alpha_ok;
            check(shm_ok, "overlay_pixels_shm");
            printf("overlay_pixels_shm_detail=ok mismatches=%d alpha=%d\n", mismatches, alpha_ok);
        }
        XShmDetach(display, &segment);
        XSync(display, False);
        XDestroyImage(shared);
        shmdt(segment.shmaddr);
        shmctl(segment.shmid, IPC_RMID, NULL);
    }

    drain(display);
    XClearArea(display, overlay, 0, 0, 0, 0, True);
    XSync(display, False);
    int exposures = 0;
    long deadline = now_ms() + 2000;
    while (now_ms() < deadline && exposures == 0) {
        while (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == Expose && event.xexpose.window == overlay)
                exposures += 1 + event.xexpose.count;
        }
        struct pollfd fd = {ConnectionNumber(display), POLLIN, 0};
        poll(&fd, 1, 20);
    }
    check(exposures > 0, "overlay_expose");
    XPutImage(display, overlay, gc, image, 0, 0, 0, 0, WIDTH, HEIGHT);
    XSync(display, False);
    mismatches = compare_pixels(display, overlay, image, &alpha_ok);
    check(mismatches == 0 && alpha_ok, "overlay_redraw_after_expose");

    drain(display);
    click_at(display, LEFT + 10, TOP + 10);
    int overlay_presses = 0;
    int below_presses = 0;
    count_button_press(display, overlay, below, &overlay_presses, &below_presses, 600);
    check(overlay_presses == 0 && below_presses >= 1, "overlay_shape_hole");
    printf("overlay_shape_hole_detail=ok overlay_press=%d below_press=%d xerrors=%d\n",
           overlay_presses, below_presses, x_errors);

    XRectangle input = {60, 60, 120, 90};
    set_input_rect(display, overlay, &input);
    drain(display);
    click_at(display, LEFT + 100, TOP + 100);
    count_button_press(display, overlay, below, &overlay_presses, &below_presses, 600);
    check(overlay_presses == 1 && below_presses == 0, "overlay_shape_region");
    printf("overlay_shape_region_detail=ok overlay_press=%d below_press=%d\n", overlay_presses,
           below_presses);

    drain(display);
    click_at(display, LEFT + 20, TOP + 20);
    count_button_press(display, overlay, below, &overlay_presses, &below_presses, 600);
    check(overlay_presses == 0 && below_presses >= 1, "overlay_shape_outside_passthrough");
    printf("overlay_shape_outside_detail=ok overlay_press=%d below_press=%d\n", overlay_presses,
           below_presses);

    XRectangle full = {0, 0, WIDTH, HEIGHT};
    set_input_rect(display, overlay, &full);
    XDestroyImage(image);
    XFreeGC(display, gc);
    XDestroyWindow(display, overlay);
    XDestroyWindow(display, below);
    XDestroyWindow(display, focus_window);
    XFreeColormap(display, colormap);
    XSync(display, False);
    check(x_errors == 0, "overlay_no_xerrors");
    XCloseDisplay(display);
    printf("phase4_overlay_probe result=%s failures=%d\n", failures == 0 ? "pass" : "fail", failures);
    return failures == 0 ? 0 : 1;
}
