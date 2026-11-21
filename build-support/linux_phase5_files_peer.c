/* Phase 5 X11 file-list peer: an independent ICCCM owner and requestor for
 * text/uri-list, GNOME copied-files and the KDE password marker. Prints one
 * JSON line per structured result; bytes leave only as lengths/booleans. */
#define _POSIX_C_SOURCE 200809L
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <time.h>
#include <unistd.h>

#define MAX_PATHS 64
#define MAX_BYTES 262144u

static const char TEXT[] = "oink-phase5-text";
static const char SECRET[] = "secret";

typedef struct {
    Atom clipboard, targets, timestamp, uri_list, gnome, marker, utf8, integer;
} Atoms;
static Display *dpy;
static Window win;
static Atoms a;
static Atom scratch;
static unsigned long ownership_time;

static void emit(const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vfprintf(stdout, fmt, args);
    va_end(args);
    putchar('\n'), fflush(stdout);
}
static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}
static Atom intern(const char *name) { return XInternAtom(dpy, name, False); }
static void load_atoms(void) {
    struct { Atom *slot; const char *name; } table[] = {
        {&a.clipboard, "CLIPBOARD"}, {&a.targets, "TARGETS"}, {&a.timestamp, "TIMESTAMP"},
        {&a.uri_list, "text/uri-list"}, {&a.gnome, "x-special/gnome-copied-files"},
        {&a.marker, "x-kde-passwordManagerHint"}, {&a.utf8, "UTF8_STRING"},
    };
    for (size_t i = 0; i < sizeof(table) / sizeof(table[0]); i++) *table[i].slot = intern(table[i].name);
    a.integer = intern("INTEGER");
}
static int wait_event(XEvent *event, long deadline) {
    for (;;) {
        if (XPending(dpy)) {
            XNextEvent(dpy, event);
            return 1;
        }
        long left = deadline - now_ms();
        if (left <= 0) return 0;
        int fd = ConnectionNumber(dpy);
        fd_set set;
        FD_ZERO(&set);
        FD_SET(fd, &set);
        struct timeval timeout = {left / 1000, (left % 1000) * 1000};
        if (select(fd + 1, &set, NULL, NULL, &timeout) < 0) return 0;
    }
}
static int server_time(unsigned long *out) {
    Atom ts = intern("P5_TIMESTAMP");
    XChangeProperty(dpy, win, ts, a.integer, 8, PropModeReplace, (const unsigned char *)"", 0);
    XFlush(dpy);
    long deadline = now_ms() + 2000;
    XEvent event;
    while (wait_event(&event, deadline)) {
        if (event.type == PropertyNotify && event.xproperty.window == win && event.xproperty.atom == ts &&
            event.xproperty.state == PropertyNewValue) {
            XDeleteProperty(dpy, win, ts);
            *out = (unsigned long)event.xproperty.time;
            return 1;
        }
    }
    return 0;
}
static void notify_requestor(XSelectionRequestEvent *request, Atom property) {
    XEvent event;
    memset(&event, 0, sizeof(event));
    event.xselection.type = SelectionNotify;
    event.xselection.display = dpy;
    event.xselection.requestor = request->requestor;
    event.xselection.selection = request->selection;
    event.xselection.target = request->target;
    event.xselection.property = property;
    event.xselection.time = request->time;
    XSendEvent(dpy, request->requestor, False, NoEventMask, &event);
}
static void direct(XSelectionRequestEvent *request, Atom property, Atom type, const void *data, size_t len) {
    XChangeProperty(dpy, request->requestor, property, type, 8, PropModeReplace, data, (int)len);
    notify_requestor(request, property);
}
static int separator(unsigned char ch) {
    return (ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') || (ch >= '0' && ch <= '9') ||
           ch == '-' || ch == '.' || ch == '_' || ch == '~' || ch == '/';
}
static size_t append_uri(unsigned char *out, size_t used, const char *path) {
    const char *prefix = "file://";
    while (*prefix) out[used++] = (unsigned char)*prefix++;
    for (const unsigned char *p = (const unsigned char *)path; *p; p++) {
        if (separator(*p)) {
            out[used++] = *p;
        } else {
            static const char hex[] = "0123456789ABCDEF";
            out[used++] = '%';
            out[used++] = (unsigned char)hex[*p >> 4];
            out[used++] = (unsigned char)hex[*p & 15];
        }
    }
    return used;
}
static size_t render_uris(unsigned char *out, char paths[][1024], int count, int gnome) {
    size_t used = 0;
    if (gnome) {
        memcpy(out, "copy\n", 5);
        used = 5;
    }
    for (int i = 0; i < count; i++) {
        used = append_uri(out, used, paths[i]);
        out[used++] = gnome ? '\n' : '\r';
        if (!gnome) out[used++] = '\n';
    }
    return used;
}
static int split_paths(char *value, char paths[][1024]) {
    int count = 0;
    char *start = value;
    while (start && *start && count < MAX_PATHS) {
        char *end = strchr(start, ';');
        if (end) *end = '\0';
        if (*start) {
            size_t n = strlen(start);
            if (n > 1023) n = 1023;
            memcpy(paths[count], start, n);
            paths[count][n] = '\0';
            count++;
        }
        start = end ? end + 1 : NULL;
    }
    return count;
}
static int serve_target(XSelectionRequestEvent *request, int secret, char paths[][1024], int count) {
    Atom property = request->property == None ? request->target : request->property;
    unsigned char *buffer = malloc(MAX_BYTES);
    if (!buffer) return 0;
    int served = 1;
    if (request->target == a.targets) {
        unsigned long list[6] = {a.targets, a.timestamp};
        int n = 2;
        if (secret) {
            list[n++] = a.marker;
            list[n++] = a.utf8;
        } else {
            list[n++] = a.uri_list;
            list[n++] = a.gnome;
        }
        XChangeProperty(dpy, request->requestor, property, XA_ATOM, 32, PropModeReplace,
                        (const unsigned char *)list, n);
        notify_requestor(request, property);
    } else if (request->target == a.timestamp) {
        XChangeProperty(dpy, request->requestor, property, a.integer, 32, PropModeReplace,
                        (const unsigned char *)&ownership_time, 1);
        notify_requestor(request, property);
    } else if (secret && request->target == a.marker) {
        direct(request, property, request->target, SECRET, sizeof(SECRET) - 1);
        emit("{\"event\":\"peer-served\",\"target\":\"marker\",\"length\":%zu}", sizeof(SECRET) - 1);
    } else if (secret && request->target == a.utf8) {
        direct(request, property, request->target, TEXT, sizeof(TEXT) - 1);
        emit("{\"event\":\"peer-served\",\"target\":\"text\",\"length\":%zu}", sizeof(TEXT) - 1);
    } else if (request->target == a.uri_list) {
        size_t len = render_uris(buffer, paths, count, 0);
        direct(request, property, request->target, buffer, len);
        emit("{\"event\":\"peer-served\",\"target\":\"text/uri-list\",\"length\":%zu}", len);
    } else if (request->target == a.gnome) {
        size_t len = render_uris(buffer, paths, count, 1);
        direct(request, property, request->target, buffer, len);
        emit("{\"event\":\"peer-served\",\"target\":\"x-special/gnome-copied-files\",\"length\":%zu}", len);
    } else {
        notify_requestor(request, None);
        served = 0;
    }
    free(buffer);
    return served;
}
static int owner_mode(int secret, char paths[][1024], int count, long hold_ms) {
    load_atoms();
    win = XCreateSimpleWindow(dpy, DefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
    XSelectInput(dpy, win, PropertyChangeMask);
    unsigned long ts = 0;
    if (!server_time(&ts)) { fprintf(stderr, "peer: no timestamp\n"); return 4; }
    ownership_time = ts;
    XSetSelectionOwner(dpy, a.clipboard, win, ts);
    XSync(dpy, False);
    if (XGetSelectionOwner(dpy, a.clipboard) != win) {
        fprintf(stderr, "peer: ownership lost\n");
        return 4;
    }
    emit("{\"event\":\"peer-ready\",\"owner\":%lu,\"timestamp\":%lu,\"mode\":\"%s\",\"paths\":%d}", win, ts,
         secret ? "secret" : "files", count);
    long deadline = now_ms() + hold_ms;
    XEvent event;
    while (wait_event(&event, deadline)) {
        if (event.type == SelectionRequest) {
            serve_target(&event.xselectionrequest, secret, paths, count);
        } else if (event.type == SelectionClear) {
            emit("{\"event\":\"peer-clear\"}");
            break;
        }
        XFlush(dpy);
    }
    emit("{\"event\":\"peer-exit\"}");
    return 0;
}
static int wait_notify(Atom target, Atom *out_property) {
    long deadline = now_ms() + 5000;
    XEvent event;
    while (wait_event(&event, deadline)) {
        if (event.type == SelectionNotify && event.xselection.requestor == win &&
            event.xselection.selection == a.clipboard && event.xselection.target == target) {
            *out_property = event.xselection.property;
            return 1;
        }
    }
    return 0;
}
static int read_target(Atom target, unsigned char **out, size_t *len) {
    XConvertSelection(dpy, a.clipboard, target, scratch, win, CurrentTime);
    XFlush(dpy);
    Atom property = None;
    if (!wait_notify(target, &property) || property == None) return 0;
    Atom type = None;
    int format = 0;
    unsigned long items = 0, after = 0;
    unsigned char *value = NULL;
    if (XGetWindowProperty(dpy, win, property, 0, (long)(MAX_BYTES / 4), True, AnyPropertyType, &type, &format,
                           &items, &after, &value) != Success)
        return 0;
    if (type == None || format != 8) {
        if (value) XFree(value);
        return 0;
    }
    *out = value;
    *len = (size_t)items;
    return 1;
}
static int request_mode(const char *what, char paths[][1024], int count) {
    load_atoms();
    win = XCreateSimpleWindow(dpy, DefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
    XSelectInput(dpy, win, PropertyChangeMask);
    scratch = intern("P5_FILES");
    unsigned char expected[MAX_BYTES];
    size_t expected_len = 0;
    Atom target;
    if (strcmp(what, "request-uris") == 0) {
        target = a.uri_list;
        expected_len = render_uris(expected, paths, count, 0);
    } else if (strcmp(what, "request-gnome") == 0) {
        target = a.gnome;
        expected_len = render_uris(expected, paths, count, 1);
    } else if (strcmp(what, "request-marker") == 0) {
        target = a.marker;
        memcpy(expected, SECRET, sizeof(SECRET) - 1);
        expected_len = sizeof(SECRET) - 1;
    } else {
        return 2;
    }
    unsigned char *bytes = NULL;
    size_t len = 0;
    if (!read_target(target, &bytes, &len)) {
        emit("{\"event\":\"peer-read\",\"target\":\"%s\",\"ok\":false}", what);
        return 5;
    }
    int match = len == expected_len && memcmp(bytes, expected, len) == 0;
    int header = len >= 5 && memcmp(bytes, "copy\n", 5) == 0;
    emit("{\"event\":\"peer-read\",\"target\":\"%s\",\"ok\":true,\"length\":%zu,\"match\":%s,\"header\":%s}",
         what, len, match ? "true" : "false", header ? "true" : "false");
    XFree(bytes);
    return match ? 0 : 3;
}
int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s owner-files|owner-secret|request-uris|request-gnome|request-marker "
                        "[--paths A;B] [--hold-ms N]\n", argv[0]);
        return 2;
    }
    const char *mode = argv[1];
    static char raw_paths[8192] = "";
    static char paths[MAX_PATHS][1024];
    long hold_ms = 3000;
    for (int i = 2; i + 1 < argc; i += 2) {
        if (strcmp(argv[i], "--paths") == 0) snprintf(raw_paths, sizeof(raw_paths), "%s", argv[i + 1]);
        else if (strcmp(argv[i], "--hold-ms") == 0) hold_ms = strtol(argv[i + 1], NULL, 10);
        else {
            fprintf(stderr, "peer: unknown argument %s\n", argv[i]);
            return 2;
        }
    }
    int count = split_paths(raw_paths, paths);
    dpy = XOpenDisplay(NULL);
    if (dpy == NULL) {
        fprintf(stderr, "peer: cannot open display\n");
        return 2;
    }
    int status;
    if (strcmp(mode, "owner-files") == 0) {
        status = count > 0 ? owner_mode(0, paths, count, hold_ms) : 2;
    } else if (strcmp(mode, "owner-secret") == 0) {
        status = owner_mode(1, paths, 0, hold_ms);
    } else {
        status = count > 0 || strcmp(mode, "request-marker") == 0 ? request_mode(mode, paths, count) : 2;
    }
    XCloseDisplay(dpy);
    return status;
}
