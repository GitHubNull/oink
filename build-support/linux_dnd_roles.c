#include "linux_dnd_roles.h"
#include <glib-unix.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>

struct Report r = {.gen = 1};
GtkWidget *window, *box;
GdkDragContext *source_context, *incoming_context;
bool is_source, is_proxy, is_catcher, is_sentinel;
static char output[16384];
static size_t output_used;
static GtkTargetEntry targets[] = {{"UTF8_STRING", 0, 1}};

void require(bool condition, const char *message) {
    if (condition) return;
    fprintf(stderr, "role FAIL: %s (generation=%lu)\n", message, r.gen);
    exit(1);
}

static void flush_output(void) {
    if (!output_used) return;
    ssize_t written = write(STDOUT_FILENO, output, output_used);
    if (written < 0) {
        require(errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR, "control output disconnected");
        return;
    }
    memmove(output, output + written, output_used - (size_t)written);
    output_used -= (size_t)written;
}

void emit(const char *format, ...) {
    va_list args;
    va_start(args, format);
    int n = vsnprintf(output + output_used, sizeof(output) - output_used, format, args);
    va_end(args);
    require(n >= 0 && (size_t)n < sizeof(output) - output_used, "bounded output backpressure");
    output_used += (size_t)n;
}

static void geometry(void) {
    if (!box || !gtk_widget_get_window(box)
            || !gdk_window_is_viewable(gtk_widget_get_window(box))) return;
    int x, y;
    gdk_window_get_origin(gtk_widget_get_window(box), &x, &y);
    x += gtk_widget_get_allocated_width(box) / 2;
    y += gtk_widget_get_allocated_height(box) / 2;
    require(x >= 0 && y >= 0, "viewable in-screen role window");
    r.x = (unsigned long)x; r.y = (unsigned long)y;
    r.xid = GDK_WINDOW_XID(gtk_widget_get_window(window));
}

void report(unsigned long seq, unsigned long status) {
    geometry();
    native_poll();
    emit("R %lu %lu", seq, status);
#define PRINT(name) emit(" %lu", r.name);
    FIELDS(PRINT)
#undef PRINT
    emit("\n");
    flush_output();
}

static gboolean pressed(GtkWidget *widget, GdkEventButton *event, gpointer data) {
    (void)widget; (void)data;
    if (event->button == 1 && event->type == GDK_BUTTON_PRESS) r.press++;
    return FALSE;
}
static void begun(GtkWidget *widget, GdkDragContext *context, gpointer data) {
    (void)data;
    require(widget == box && ++r.begin == 1 && r.down == 1 && r.up == 0 && body_length,
            "exactly one begin from real held button with available source data");
    require(gdk_drag_context_get_protocol(context) == GDK_DRAG_PROTO_XDND, "native XDND source");
    source_context = g_object_ref(context);
    r.source_xid = GDK_WINDOW_XID(gdk_drag_context_get_source_window(context));
    selection_publish_generation();
}
static void ended(GtkWidget *widget, GdkDragContext *context, gpointer data) {
    (void)widget; (void)data;
    require(context == source_context && ++r.end == 1, "one end on original context");
}
static gboolean failed(GtkWidget *widget, GdkDragContext *context, GtkDragResult result, gpointer data) {
    (void)widget; (void)data;
    require(context == source_context, "failure on original context");
    r.failed++;
    if (result == GTK_DRAG_RESULT_USER_CANCELLED) r.cancelled++;
    return FALSE;
}
static void deleted(GtkWidget *widget, GdkDragContext *context, gpointer data) {
    (void)widget; (void)context; (void)data;
    r.deleted++;
    require(false, "COPY must never delete source data");
}
static gboolean dragged(GtkWidget *widget, GdkDragContext *context, gint x, gint y,
                        guint time, gpointer data) {
    (void)widget; (void)x; (void)y; (void)data;
    if (!selection_identity(context)) {
        gdk_drag_status(context, 0, time);
        return TRUE;
    }
    if (incoming_context != context) {
        g_clear_object(&incoming_context);
        incoming_context = g_object_ref(context);
    }
    r.source_xid = GDK_WINDOW_XID(gdk_drag_context_get_source_window(context));
    r.motions++;
    if (!r.inside) { r.inside = 1; r.visits++; }
    gdk_drag_status(context, GDK_ACTION_COPY, time);
    return TRUE;
}
static void left(GtkWidget *widget, GdkDragContext *context, guint time, gpointer data) {
    (void)widget; (void)time; (void)data;
    if (context == incoming_context) { r.inside = 0; r.leaves++; }
}
static void request_selection(GdkDragContext *context, guint time) {
    require(!r.pending && context && selection_identity(context), "one valid asynchronous selection read");
    r.pending = 1; pending_epoch = r.epoch; pending_gen = r.gen;
    pending_context = g_object_ref(context);
    gtk_drag_get_data(box, context, data_target, time);
}
static gboolean dropped(GtkWidget *widget, GdkDragContext *context, gint x, gint y,
                        guint time, gpointer data) {
    (void)widget; (void)x; (void)y; (void)data;
    require(!is_catcher && context == incoming_context && selection_identity(context)
            && ++r.drop == 1 && gdk_drag_context_get_selected_action(context) == GDK_ACTION_COPY,
            "target COPY drop on armed source XID/selection owner/generation");
    request_selection(context, time);
    return TRUE;
}

static unsigned long dispatch(unsigned long *v, size_t n) {
    unsigned long op = v[1], generation = v[2];
    size_t argc = n - 3;
    unsigned long *a = v + 3;
    if (op == ARM && !is_source && argc == 1 && a[0] && generation >= r.gen) {
        r.gen = generation; expected_source = a[0]; r.armed = 1; r.epoch++;
        r.inside = 0; payload_clear();
        return 0;
    }
    if (generation != r.gen) return 1;
    switch (op) {
    case STATUS: return argc ? 2 : 0;
    case FOCUS:
        if (!is_source || argc || r.down) return 2;
        XSetInputFocus(inject, r.xid, RevertToParent, CurrentTime);
        motion((int)r.x, (int)r.y); return 0;
    case MOVE:
        if (!is_source || argc != 2 || a[0] > 32767 || a[1] > 32767) return 2;
        motion((int)a[0], (int)a[1]); return 0;
    case DOWN:
        if (!is_source || argc || r.down || !body_length) return 2;
        button(true); return 0;
    case UP:
        if (!is_source || argc || r.down != 1 || r.up) return 2;
        button(false); return 0;
    case ESCAPE:
        if (!is_source || argc || !source_context || r.end || r.key_down) return 2;
        escape_key(); return 0;
    case HELD:
    case RETURNED:
        if (!is_source || argc != 1) return 2;
        held_context(a[0]);
        if (op == RETURNED) { pointer_at_source(); r.returned++; }
        return 0;
    case FREE:
        if (argc) return 2;
        no_grab(); return 0;
    case READ_SELECTION:
        if (!is_catcher || argc || !r.armed || !r.inside || r.pending || !incoming_context) return 2;
        request_selection(incoming_context, GDK_CURRENT_TIME); return 0;
    case LOAD:
        if (!is_proxy || r.begin || body_length || argc < 2 || a[0] > PAYLOAD_MAX
                || a[0] != argc - 1 || !a[0]) return 2;
        for (size_t i = 1; i < argc; i++) if (a[i] > 255) return 2;
        body_length = a[0];
        for (size_t i = 0; i < body_length; i++) body[i] = (unsigned char)a[i + 1];
        payload_report(); return 0;
    case CLIP_CHECK:
        if (argc != 1 || !a[0]) return 2;
        selection_check_clipboard(a[0]); return 0;
    case DELAY:
        if (!is_source || argc != 1 || a[0] > 3000 || r.get) return 2;
        slow_ms = (unsigned int)a[0]; return 0;
    case DISARM:
        if (is_source || argc) return 2;
        r.armed = 0; r.epoch++; r.inside = 0; payload_clear();
        return 0;
    case FETCH:
        if (!is_catcher || argc || !r.armed || !body_length || r.pending) return 2;
        emit("D %lu %lu %zu", v[0], r.gen, body_length);
        for (size_t i = 0; i < body_length; i++) emit(" %u", body[i]);
        emit("\n"); return 0;
    case ORPHAN_UP:
        if (is_source || argc || !expected_source || XGetSelectionOwner(inject, selection_atom) != None)
            return 2;
        native_poll();
        if (!r.button) return 2;
        /* Controller has reaped the dead source; FREE must succeed before cleanup. */
        no_grab();
        require(XTestFakeButtonEvent(inject, 1, False, CurrentTime), "private orphan button cleanup");
        XSync(inject, False); r.up++; return 0;
    case QUIT:
        if (argc) return 2;
        gtk_main_quit(); return 0;
    default: return 2;
    }
}

static void command(char *line) {
    unsigned long values[PAYLOAD_MAX + 4];
    size_t n = 0;
    char *p = line;
    while (*p) {
        require(n < G_N_ELEMENTS(values) && *p >= '0' && *p <= '9', "numeric bounded control tokens");
        errno = 0;
        char *end;
        unsigned long value = strtoul(p, &end, 10);
        require(!errno && value <= UINT32_MAX && (*end == ' ' || *end == '\0'), "uint32 control token");
        values[n++] = value;
        p = *end ? end + 1 : end;
        require(!*end || *p, "no trailing control whitespace");
    }
    require(n >= 3 && values[0] && values[2], "sequence/opcode/generation required");
    report(values[0], dispatch(values, n));
}

static gboolean input_ready(gint fd, GIOCondition condition, gpointer data) {
    (void)condition; (void)data;
    static char line[LINE_MAXIMUM];
    static size_t used;
    char bytes[1024];
    ssize_t count = read(fd, bytes, sizeof(bytes));
    if (count == 0) { gtk_main_quit(); return G_SOURCE_REMOVE; }
    if (count < 0) {
        require(errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR, "control read");
        return G_SOURCE_CONTINUE;
    }
    for (ssize_t i = 0; i < count; i++) {
        if (bytes[i] == '\n') { line[used] = '\0'; command(line); used = 0; }
        else {
            require(used + 1 < sizeof(line) && (bytes[i] == ' ' || (bytes[i] >= '0' && bytes[i] <= '9')),
                    "bounded ASCII control frame");
            line[used++] = bytes[i];
        }
    }
    return G_SOURCE_CONTINUE;
}

static gboolean tick(gpointer data) {
    (void)data;
    static bool ready;
    native_poll(); flush_output(); geometry();
    if (!ready && r.xid) { ready = true; report(0, 0); }
    return G_SOURCE_CONTINUE;
}

int main(int argc, char **argv) {
    require_isolated();
    require(argc == 2 && (!strcmp(argv[1], "source") || !strcmp(argv[1], "catcher")
            || !strcmp(argv[1], "proxy") || !strcmp(argv[1], "target")
            || !strcmp(argv[1], "sentinel")), "role argument");
    is_proxy = !strcmp(argv[1], "proxy");
    is_source = is_proxy || !strcmp(argv[1], "source");
    is_catcher = !strcmp(argv[1], "catcher");
    is_sentinel = !strcmp(argv[1], "sentinel");
    alarm(90); signal(SIGPIPE, SIG_IGN);
    require(fcntl(STDIN_FILENO, F_SETFL, O_NONBLOCK) == 0
            && fcntl(STDOUT_FILENO, F_SETFL, O_NONBLOCK) == 0, "nonblocking control pipes");
    require(gtk_init_check(NULL, NULL), "private GTK X11 initialization");
    require(GDK_IS_X11_DISPLAY(gdk_display_get_default()), "X11 backend only");
    native_init(); selection_init();
    window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_decorated(GTK_WINDOW(window), FALSE);
    gtk_window_set_default_size(GTK_WINDOW(window), 230, 220);
    gtk_window_move(GTK_WINDOW(window), is_source ? 100 : is_catcher ? 500 : 900, 220);
    if (is_sentinel) {
        gtk_window_set_default_size(GTK_WINDOW(window), 90, 60);
        gtk_window_move(GTK_WINDOW(window), 20, 20);
    }
    box = gtk_event_box_new();
    gtk_container_add(GTK_CONTAINER(window), box);
    gtk_container_add(GTK_CONTAINER(box), gtk_label_new(argv[1]));
    if (is_source) {
        gtk_widget_add_events(box, GDK_BUTTON_PRESS_MASK | GDK_BUTTON_RELEASE_MASK | GDK_POINTER_MOTION_MASK);
        gtk_drag_source_set(box, GDK_BUTTON1_MASK, targets, 1, GDK_ACTION_COPY);
        g_signal_connect(box, "button-press-event", G_CALLBACK(pressed), NULL);
        g_signal_connect(box, "drag-begin", G_CALLBACK(begun), NULL);
        g_signal_connect(box, "drag-end", G_CALLBACK(ended), NULL);
        g_signal_connect(box, "drag-failed", G_CALLBACK(failed), NULL);
        g_signal_connect(box, "drag-data-get", G_CALLBACK(selection_get), NULL);
        g_signal_connect(box, "drag-data-delete", G_CALLBACK(deleted), NULL);
    } else if (!is_sentinel) {
        gtk_drag_dest_set(box, 0, targets, 1, GDK_ACTION_COPY);
        g_signal_connect(box, "drag-motion", G_CALLBACK(dragged), NULL);
        g_signal_connect(box, "drag-leave", G_CALLBACK(left), NULL);
        g_signal_connect(box, "drag-drop", G_CALLBACK(dropped), NULL);
        g_signal_connect(box, "drag-data-received", G_CALLBACK(selection_received), NULL);
    }
    gtk_widget_show_all(window);
    g_unix_fd_add(STDIN_FILENO, G_IO_IN | G_IO_HUP | G_IO_ERR, input_ready, NULL);
    g_timeout_add(10, tick, NULL);
    gtk_main();
    flush_output();
    return 0;
}
