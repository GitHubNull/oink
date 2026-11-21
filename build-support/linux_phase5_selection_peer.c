/* Phase 5 X11 raw selection peer: an independent ICCCM owner and requestor.
 * Prints one JSON line per structured result; payload bytes never leave as
 * text, only lengths and exact-match booleans. */
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

#define DIRECT_LIMIT 65536u
#define CHUNK 65536u
#define MAX_READ (8u * 1024u * 1024u)

static const char TEXT[] = "oink-phase5-text";
static const char HTML[] = "<p>oink-phase5-html</p>";
static const char RTF[] = "{\\rtf1 oink-phase5}";
static const char URL[] = "https://example.invalid/phase5\r\n";
static const unsigned char PNG[] = {
    0x89, 'P', 'N', 'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 'I', 'H', 'D', 'R',
    0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0a,
    'I', 'D', 'A', 'T', 0x78, 0x9c, 0x63, 0, 0, 0, 0, 2, 0, 1, 0x0d, 0x0a, 0x2d, 0xb4,
    0, 0, 0, 0, 0x49, 'E', 'N', 'D', 0xae, 0x42, 0x60, 0x82,
};

typedef struct {
    Atom clipboard, primary, targets, timestamp, incr, multiple;
    Atom utf8, text, plain, plain_utf8, string, html, html_utf8, rtf, app_rtf;
    Atom png, uri_list, integer, atom;
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
static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}
static Atom intern(const char *name) { return XInternAtom(dpy, name, False); }
static void load_atoms(void) {
    struct { Atom *slot; const char *name; } table[] = {
        {&a.clipboard, "CLIPBOARD"}, {&a.primary, "PRIMARY"}, {&a.targets, "TARGETS"},
        {&a.timestamp, "TIMESTAMP"}, {&a.incr, "INCR"}, {&a.multiple, "MULTIPLE"},
        {&a.utf8, "UTF8_STRING"}, {&a.text, "TEXT"}, {&a.plain, "text/plain"},
        {&a.plain_utf8, "text/plain;charset=utf-8"}, {&a.string, "STRING"},
        {&a.html, "text/html"}, {&a.html_utf8, "text/html;charset=utf-8"},
        {&a.rtf, "text/rtf"}, {&a.app_rtf, "application/rtf"}, {&a.png, "image/png"},
        {&a.uri_list, "text/uri-list"}, {&a.integer, "INTEGER"},
    };
    for (size_t i = 0; i < sizeof(table) / sizeof(table[0]); i++) *table[i].slot = intern(table[i].name);
    a.atom = XA_ATOM;
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
static void direct(XSelectionRequestEvent *request, Atom property, Atom type, const void *data,
                   size_t len, int format) {
    int count = format == 32 ? (int)(len / sizeof(unsigned long)) : (int)len;
    XChangeProperty(dpy, request->requestor, property, type, format, PropModeReplace, data, count);
    notify_requestor(request, property);
}
typedef struct {
    int active;
    Window requestor;
    Atom property, target;
    size_t offset, len;
    const unsigned char *data;
} Transfer;
static Transfer transfer;
static void advance_incr(void) {
    size_t left = transfer.len - transfer.offset;
    if (left == 0) {
        XChangeProperty(dpy, transfer.requestor, transfer.property, transfer.target, 8, PropModeReplace,
                        (const unsigned char *)"", 0);
        transfer.active = 0;
        XSelectInput(dpy, transfer.requestor, NoEventMask);
        emit("{\"event\":\"peer-served-incr\",\"length\":%zu}", transfer.len);
        return;
    }
    size_t chunk = left < CHUNK ? left : CHUNK;
    XChangeProperty(dpy, transfer.requestor, transfer.property, transfer.target, 8, PropModeReplace,
                    transfer.data + transfer.offset, (int)chunk);
    transfer.offset += chunk;
}
static int start_incr(XSelectionRequestEvent *request, Atom property, Atom target, const unsigned char *data,
                      size_t len) {
    if (transfer.active) {
        notify_requestor(request, None);
        return 0;
    }
    unsigned long total = (unsigned long)len;
    XSelectInput(dpy, request->requestor, PropertyChangeMask);
    XChangeProperty(dpy, request->requestor, property, a.incr, 32, PropModeReplace,
                    (const unsigned char *)&total, 1);
    notify_requestor(request, property);
    transfer = (Transfer){1, request->requestor, property, target, 0, len, data};
    return 1;
}
static void fill_pattern(unsigned char *out, size_t len) { for (size_t i = 0; i < len; i++) out[i] = (unsigned char)('a' + i % 26); }
static const unsigned char *payload_for(const char *kind, unsigned char *large, size_t size, size_t *len) {
    if (strcmp(kind, "large") == 0) {
        fill_pattern(large, size);
        *len = size;
        return large;
    }
    struct { const char *kind; const char *data; size_t len; } table[] = {
        {"text", TEXT, sizeof(TEXT) - 1}, {"html", HTML, sizeof(HTML) - 1},
        {"rtf", RTF, sizeof(RTF) - 1}, {"url", URL, sizeof(URL) - 1},
        {"png", (const char *)PNG, sizeof(PNG)},
    };
    *len = 0;
    for (size_t i = 0; i < sizeof(table) / sizeof(table[0]); i++) {
        if (strcmp(kind, table[i].kind) == 0) {
            *len = table[i].len;
            return (const unsigned char *)table[i].data;
        }
    }
    return NULL;
}
static int serve_target(XSelectionRequestEvent *request, const char *kind, unsigned char *large, size_t size) {
    Atom property = request->property == None ? request->target : request->property;
    if (request->target == a.targets) {
        unsigned long list[16] = {a.targets, a.timestamp, a.utf8, a.text, a.plain, a.string};
        int n = 6;
        if (strcmp(kind, "multi") == 0) {
            list[n++] = a.html;
            list[n++] = a.rtf;
            list[n++] = a.png;
        }
        if (strcmp(kind, "html") == 0) list[n++] = a.html;
        if (strcmp(kind, "rtf") == 0) list[n++] = a.rtf;
        if (strcmp(kind, "png") == 0) list[n++] = a.png;
        if (strcmp(kind, "url") == 0) list[n++] = a.uri_list;
        direct(request, property, XA_ATOM, list, (size_t)n * sizeof(unsigned long), 32);
        return 1;
    }
    if (request->target == a.timestamp) {
        direct(request, property, a.integer, &ownership_time, sizeof(ownership_time), 32);
        return 1;
    }
    const char *source = strcmp(kind, "multi") == 0 ? "text" : kind;
    const unsigned char *data = NULL;
    size_t len = 0;
    if (request->target == a.utf8 || request->target == a.text || request->target == a.plain ||
        request->target == a.string) {
        data = payload_for(source, large, size, &len);
    } else if (request->target == a.html || request->target == a.html_utf8) {
        data = payload_for("html", large, size, &len);
    } else if (request->target == a.rtf || request->target == a.app_rtf) {
        data = payload_for("rtf", large, size, &len);
    } else if (request->target == a.png) {
        data = payload_for("png", large, size, &len);
    } else if (request->target == a.uri_list) {
        data = payload_for("url", large, size, &len);
    }
    if (data == NULL) {
        notify_requestor(request, None);
        return 1;
    }
    if (len > DIRECT_LIMIT) {
        start_incr(request, property, request->target, data, len);
    } else {
        direct(request, property, request->target, data, len, 8);
        emit("{\"event\":\"peer-served\",\"target\":\"%s\",\"length\":%zu,\"incr\":false}",
             XGetAtomName(dpy, request->target) ?: "?", len);
    }
    return 1;
}
static int owner_mode(const char *kind, size_t size, long hold_ms) {
    unsigned char *large = malloc(size ? size : 1);
    if (large == NULL) return 2;
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
    emit("{\"event\":\"peer-ready\",\"owner\":%lu,\"timestamp\":%lu,\"kind\":\"%s\",\"size\":%zu}",
         win, ts, kind, size);
    long deadline = now_ms() + hold_ms;
    XEvent event;
    while (wait_event(&event, deadline)) {
        if (event.type == SelectionRequest) {
            serve_target(&event.xselectionrequest, kind, large, size);
        } else if (event.type == SelectionClear) {
            emit("{\"event\":\"peer-clear\"}");
            break;
        } else if (event.type == PropertyNotify && transfer.active && event.xproperty.window == transfer.requestor &&
                   event.xproperty.atom == transfer.property && event.xproperty.state == PropertyDelete) {
            advance_incr();
        }
        XFlush(dpy);
    }
    free(large);
    emit("{\"event\":\"peer-exit\"}");
    return 0;
}
static int primary_mode(long hold_ms) {
    load_atoms();
    win = XCreateSimpleWindow(dpy, DefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
    XSelectInput(dpy, win, PropertyChangeMask);
    unsigned long ts = 0;
    if (!server_time(&ts)) { fprintf(stderr, "peer: no timestamp\n"); return 4; }
    XSetSelectionOwner(dpy, a.primary, win, ts);
    XSync(dpy, False);
    emit("{\"event\":\"peer-ready\",\"selection\":\"PRIMARY\",\"timestamp\":%lu}", ts);
    sleep_ms(hold_ms);
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
static int convert(Atom target, Atom property, Atom *result) {
    XConvertSelection(dpy, a.clipboard, target, property, win, CurrentTime);
    XFlush(dpy);
    return wait_notify(target, result);
}
static int read_property(Atom property, Atom *type, int *format, unsigned char **data, unsigned long *items,
                         unsigned long long_length) {
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long count = 0, after = 0;
    unsigned char *value = NULL;
    int status = XGetWindowProperty(dpy, win, property, 0, (long)long_length, True, AnyPropertyType, &actual_type,
                                    &actual_format, &count, &after, &value);
    if (status != Success) return 0;
    *type = actual_type;
    *format = actual_format;
    *data = value;
    *items = count;
    return actual_type != None;
}
static int read_bytes_target(Atom target, unsigned char **out, size_t *len, int *incr) {
    Atom property = None;
    if (!convert(target, scratch, &property) || property == None) return 0;
    Atom type = None;
    int format = 0;
    unsigned char *data = NULL;
    unsigned long items = 0;
    if (!read_property(property, &type, &format, &data, &items, MAX_READ / 4)) return 0;
    if (type != a.incr || format != 32) {
        if (format != 8 || type == None) {
            if (data) XFree(data);
            return 0;
        }
        size_t total = (size_t)items;
        unsigned char *bytes = malloc(total + 1);
        memcpy(bytes, data, total);
        XFree(data);
        *out = bytes;
        *len = total;
        *incr = 0;
        return 1;
    }
    size_t total = (size_t)((unsigned long *)data)[0];
    XFree(data);
    unsigned char *bytes = malloc(total + 1);
    size_t used = 0;
    long deadline = now_ms() + 30000;
    XEvent event;
    while (used < total + 1) {
        if (!wait_event(&event, deadline)) break;
        if (event.type != PropertyNotify || event.xproperty.window != win || event.xproperty.atom != property ||
            event.xproperty.state != PropertyNewValue)
            continue;
        if (!read_property(property, &type, &format, &data, &items, CHUNK / 4)) break;
        if (items > 0) {
            if (format != 8 || used + items > total + 1) {
                XFree(data);
                free(bytes);
                return 0;
            }
            memcpy(bytes + used, data, items);
            used += items;
        }
        if (data) XFree(data);
        if (items == 0) break;
    }
    if (used != total) {
        free(bytes);
        return 0;
    }
    *out = bytes;
    *len = used;
    *incr = 1;
    return 1;
}
static int target_names_mode(void) {
    Atom property = None;
    if (!convert(a.targets, scratch, &property) || property == None) return 5;
    Atom type = None;
    int format = 0;
    unsigned char *data = NULL;
    unsigned long items = 0;
    if (!read_property(property, &type, &format, &data, &items, 1024) || format != 32) return 4;
    unsigned long *atoms = (unsigned long *)data;
    Atom wanted[] = {a.utf8, a.string, a.html, a.app_rtf, a.png, a.uri_list, a.timestamp, a.multiple};
    int seen[8] = {0};
    for (unsigned long i = 0; i < items; i++)
        for (int j = 0; j < 8; j++) seen[j] |= atoms[i] == wanted[j];
    emit("{\"event\":\"peer-targets\",\"count\":%lu,\"utf8\":%s,\"string\":%s,\"html\":%s,"
         "\"rtf\":%s,\"png\":%s,\"url\":%s,\"timestamp\":%s,\"multiple\":%s}",
         items, seen[0] ? "true" : "false", seen[1] ? "true" : "false", seen[2] ? "true" : "false",
         seen[3] ? "true" : "false", seen[4] ? "true" : "false", seen[5] ? "true" : "false",
         seen[6] ? "true" : "false", seen[7] ? "true" : "false");
    XFree(data);
    return 0;
}
static int timestamp_mode(void) {
    unsigned long values[2] = {0, 0};
    for (int round = 0; round < 2; round++) {
        Atom property = None;
        if (!convert(a.timestamp, scratch, &property) || property == None) return 5;
        Atom type = None; int format = 0;
        unsigned char *data = NULL;
        unsigned long items = 0;
        if (!read_property(property, &type, &format, &data, &items, 1) || format != 32 || items != 1) return 4;
        values[round] = ((unsigned long *)data)[0];
        XFree(data);
        sleep_ms(50);
    }
    emit("{\"event\":\"peer-time\",\"first\":%lu,\"second\":%lu,\"equal\":%s,\"positive\":%s}", values[0],
         values[1], values[0] == values[1] ? "true" : "false", values[0] > 0 ? "true" : "false");
    return values[0] == values[1] && values[0] > 0 ? 0 : 3;
}
static int bytes_mode(const char *target_name, const char *expect, size_t size) {
    Atom target = intern(target_name);
    unsigned char *bytes = NULL;
    size_t len = 0;
    int incr = 0;
    if (!read_bytes_target(target, &bytes, &len, &incr)) {
        emit("{\"event\":\"peer-read\",\"target\":\"%s\",\"ok\":false}", target_name);
        return 5;
    }
    unsigned char *large = malloc(size ? size : 1);
    if (large == NULL) return 2;
    size_t expected_len = 0;
    const unsigned char *expected = payload_for(expect, large, size, &expected_len);
    int match = expected != NULL && len == expected_len && memcmp(bytes, expected, len) == 0;
    emit("{\"event\":\"peer-read\",\"target\":\"%s\",\"ok\":true,\"length\":%zu,\"incr\":%s,\"match\":%s}",
         target_name, len, incr ? "true" : "false", match ? "true" : "false");
    free(bytes);
    free(large);
    return match ? 0 : 3;
}
static int request_mode(const char *target_name, const char *expect, size_t size) {
    load_atoms();
    win = XCreateSimpleWindow(dpy, DefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
    XSelectInput(dpy, win, PropertyChangeMask);
    scratch = intern("P5_DATA");
    if (strcmp(target_name, "TARGETS") == 0) return target_names_mode();
    if (strcmp(target_name, "TIMESTAMP") == 0) return timestamp_mode();
    return bytes_mode(target_name, expect, size);
}
static int multiple_mode(void) {
    load_atoms();
    win = XCreateSimpleWindow(dpy, DefaultRootWindow(dpy), 0, 0, 1, 1, 0, 0, 0);
    XSelectInput(dpy, win, PropertyChangeMask);
    Atom list = intern("P5_MULTI_LIST");
    Atom text_prop = intern("P5_MULTI_TEXT");
    Atom string_prop = intern("P5_MULTI_STRING");
    unsigned long pairs[4] = {a.utf8, text_prop, a.string, string_prop};
    XChangeProperty(dpy, win, list, XA_ATOM, 32, PropModeReplace, (const unsigned char *)pairs, 4);
    Atom property = None;
    if (!convert(a.multiple, list, &property) || property == None) return 5;
    Atom type = None;
    int format = 0;
    unsigned char *data = NULL;
    unsigned long items = 0;
    Atom props[2] = {text_prop, string_prop};
    int ok[2] = {0, 0};
    for (int i = 0; i < 2; i++) {
        if (read_property(props[i], &type, &format, &data, &items, 4096) && format == 8)
            ok[i] = items == sizeof(TEXT) - 1 && memcmp(data, TEXT, items) == 0;
        if (data) XFree(data);
        data = NULL;
    }
    emit("{\"event\":\"peer-multiple\",\"notify\":\"MULTIPLE\",\"text\":%s,\"string\":%s}",
         ok[0] ? "true" : "false", ok[1] ? "true" : "false");
    return ok[0] && ok[1] ? 0 : 3;
}
int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s owner|primary|request|multiple [--kind K] [--size N] [--hold-ms N] "
                        "[--expect E] [--target T]\n",
                argv[0]);
        return 2;
    }
    const char *mode = argv[1];
    const char *kind = "text", *expect = "text", *target = "UTF8_STRING";
    size_t size = 200000;
    long hold_ms = 3000;
    for (int i = 2; i + 1 < argc; i += 2) {
        const char *value = argv[i + 1];
        if (strcmp(argv[i], "--kind") == 0) kind = value;
        else if (strcmp(argv[i], "--size") == 0) size = (size_t)strtoull(value, NULL, 10);
        else if (strcmp(argv[i], "--hold-ms") == 0) hold_ms = strtol(value, NULL, 10);
        else if (strcmp(argv[i], "--expect") == 0) expect = value;
        else if (strcmp(argv[i], "--target") == 0) target = value;
        else {
            fprintf(stderr, "peer: unknown argument %s\n", argv[i]);
            return 2;
        }
    }
    dpy = XOpenDisplay(NULL);
    if (dpy == NULL) {
        fprintf(stderr, "peer: cannot open display\n");
        return 2;
    }
    int status;
    if (strcmp(mode, "owner") == 0) {
        status = owner_mode(kind, size, hold_ms);
    } else if (strcmp(mode, "primary") == 0) {
        status = primary_mode(hold_ms);
    } else if (strcmp(mode, "request") == 0) {
        status = request_mode(target, expect, size);
    } else if (strcmp(mode, "multiple") == 0) {
        status = multiple_mode();
    } else {
        fprintf(stderr, "peer: unknown mode %s\n", mode);
        status = 2;
    }
    XCloseDisplay(dpy);
    return status;
}
