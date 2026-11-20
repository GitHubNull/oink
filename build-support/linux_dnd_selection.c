#include "linux_dnd_roles.h"
#include <X11/Xatom.h>
#include <X11/extensions/XRes.h>

GdkAtom data_target;
static GdkAtom uri_target;
Atom selection_atom, generation_atom, app_generation_atom, session_epoch_atom;
unsigned char body[PAYLOAD_MAX];
size_t body_length;
unsigned int body_mime = MIME_UTF8;
unsigned int slow_ms;
Window expected_source;
unsigned long pending_epoch, pending_gen;
GdkDragContext *pending_context;
bool app_armed;
unsigned long app_pid, app_epoch;
static const char sentinel[] = "oink-private-clipboard-sentinel";
static Window clipboard_expected;
static Atom clipboard_atom;
static bool clipboard_pending;

uint32_t payload_hash(const unsigned char *bytes, size_t length) {
    uint32_t value = 2166136261u;
    for (size_t i = 0; i < length; i++) value = (value ^ bytes[i]) * 16777619u;
    return value;
}

void payload_report(void) {
    unsigned long *words[] = {&r.digest0, &r.digest1, &r.digest2, &r.digest3,
                             &r.digest4, &r.digest5, &r.digest6, &r.digest7};
    unsigned char digest[32];
    GChecksum *checksum = g_checksum_new(G_CHECKSUM_SHA256);
    gsize length = sizeof(digest);
    g_checksum_update(checksum, body, body_length);
    g_checksum_get_digest(checksum, digest, &length);
    g_checksum_free(checksum);
    require(length == sizeof(digest), "sha256 digest size");
    r.length = body_length;
    r.hash = body_length ? payload_hash(body, body_length) : 0;
    for (size_t i = 0; i < G_N_ELEMENTS(words); i++) {
        *words[i] = 0;
        for (size_t j = 0; j < 4; j++) *words[i] = (*words[i] << 8) | digest[i * 4 + j];
    }
}

void payload_clear(void) {
    memset(body, 0, sizeof(body)); body_length = 0; body_mime = MIME_UTF8; payload_report();
}

void selection_publish_generation(void) {
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    unsigned long generation = r.gen;
    XChangeProperty(display, r.source_xid, generation_atom, XA_CARDINAL, 32,
                    PropModeReplace, (unsigned char *)&generation, 1);
    XSync(display, False);
}

bool selection_identity(GdkDragContext *context) {
    GdkWindow *source = gdk_drag_context_get_source_window(context);
    if (!r.armed || app_armed || !source || GDK_WINDOW_XID(source) != expected_source
            || gdk_drag_context_get_protocol(context) != GDK_DRAG_PROTO_XDND
            || XGetSelectionOwner(inject, selection_atom) != expected_source) return false;
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    Atom type;
    int format;
    unsigned long count, remaining;
    unsigned char *data = NULL;
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    int status = XGetWindowProperty(display, expected_source, generation_atom, 0, 1,
                                   False, XA_CARDINAL, &type, &format, &count, &remaining, &data);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    bool valid = !error && status == Success && type == XA_CARDINAL && format == 32
                 && count == 1 && remaining == 0 && data && *(unsigned long *)data == r.gen;
    if (data) XFree(data);
    return valid;
}

static bool cardinal_property(Window window, Atom atom, unsigned long expect) {
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    Atom type;
    int format;
    unsigned long count, remaining;
    unsigned char *data = NULL;
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    int status = XGetWindowProperty(display, window, atom, 0, 1, False, XA_CARDINAL,
                                    &type, &format, &count, &remaining, &data);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    bool valid = !error && status == Success && type == XA_CARDINAL && format == 32
                 && count == 1 && remaining == 0 && data && *(unsigned long *)data == expect;
    if (data) XFree(data);
    return valid;
}

/* Real application sources cannot run our fixture code. This independent branch
 * binds the drag to the XRes server-side owner PID, the session epoch atom on
 * the root window, and the controller-written generation atom on the source
 * window. It never weakens the private fixture checks above. */
bool app_identity(GdkDragContext *context) {
    GdkWindow *source = gdk_drag_context_get_source_window(context);
    if (!r.armed || !app_armed || !source || GDK_WINDOW_XID(source) != expected_source
            || gdk_drag_context_get_protocol(context) != GDK_DRAG_PROTO_XDND
            || XGetSelectionOwner(inject, selection_atom) != expected_source) return false;
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    if (!cardinal_property(DefaultRootWindow(display), session_epoch_atom, app_epoch)
            || !cardinal_property(expected_source, app_generation_atom, r.gen)) return false;
    XResClientIdSpec spec = {expected_source, XRES_CLIENT_ID_PID_MASK};
    long count = 0;
    XResClientIdValue *ids = NULL;
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    /* Xephyr returns Status 0 with fully populated results, so treat populated
     * output as success; an unanswered query leaves count 0 and ids NULL. */
    Status ok = XResQueryClientIds(display, 1, &spec, &count, &ids);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    pid_t pid = -1;
    if (!error && ids && count > 0) {
        for (long i = 0; i < count; i++)
            if (XResGetClientIdType(&ids[i]) == XRES_CLIENT_ID_PID)
                pid = XResGetClientPid(&ids[i]);
        XResClientIdsDestroy(count, ids);
    }
    (void)ok;
    return pid != -1 && (unsigned long)pid == app_pid;
}

bool identity_valid(GdkDragContext *context) {
    return selection_identity(context) || app_identity(context);
}

void selection_get(GtkWidget *widget, GdkDragContext *context, GtkSelectionData *selection,
                   guint info, guint time, gpointer data) {
    (void)widget; (void)time; (void)data;
    require(is_source && context == source_context && r.end == 0
            && (info == 1 || info == 2 || info == 3) && body_length
            && body_length <= INCR_THRESHOLD,
            "small selection request on live source; INCR filter owns large bodies");
    GdkAtom requested = gtk_selection_data_get_target(selection);
    require(gtk_selection_data_get_selection(selection) == gdk_atom_intern_static_string("XdndSelection")
            && ((info != 3 && (requested == data_target
                || requested == gdk_atom_intern_static_string("text/plain")))
                || (info == 3 && requested == uri_target)),
            "XdndSelection UTF8_STRING/text/plain/uri-list only");
    r.get++;
    gtk_selection_data_set(selection, requested, 8, body, (gint)body_length);
}

void selection_received(GtkWidget *widget, GdkDragContext *context, gint x, gint y,
                        GtkSelectionData *selection, guint info, guint time, gpointer data) {
    (void)widget; (void)x; (void)y; (void)data;
    require(r.pending == 1 && pending_context == context, "one tracked asynchronous selection request");
    bool current = pending_gen == r.gen && pending_epoch == r.epoch && identity_valid(context);
    r.pending = 0;
    g_clear_object(&pending_context);
    if (!current) {
        r.stale++;
        if (!is_catcher) gtk_drag_finish(context, FALSE, FALSE, time);
        return;
    }
    int length = gtk_selection_data_get_length(selection);
    bool utf8 = info == 1 && gtk_selection_data_get_data_type(selection) == data_target;
    bool uri = info == 3 && gtk_selection_data_get_data_type(selection) == uri_target;
    require((utf8 || uri) && gtk_selection_data_get_selection(selection)
            == gdk_atom_intern_static_string("XdndSelection")
            && gtk_selection_data_get_format(selection) == 8 && length > 0 && length <= PAYLOAD_MAX,
            "real bounded native selection data");
    require(is_catcher || (r.drop == 1
            && gdk_drag_context_get_selected_action(context) == GDK_ACTION_COPY), "COPY drop before target read");
    body_length = (size_t)length;
    memcpy(body, gtk_selection_data_get_data(selection), body_length);
    payload_report(); r.received++;
    if (!is_catcher) gtk_drag_finish(context, TRUE, FALSE, time);
}

struct SlowRequest { XSelectionRequestEvent event; unsigned long generation; };
static gboolean send_slow(gpointer data) {
    struct SlowRequest *request = data;
    XSelectionRequestEvent *e = &request->event;
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    bool live = request->generation == r.gen && !r.end
                && XGetSelectionOwner(display, selection_atom) == r.source_xid;
    XEvent reply = {0};
    reply.xselection.type = SelectionNotify;
    reply.xselection.display = display;
    reply.xselection.requestor = e->requestor;
    reply.xselection.selection = e->selection;
    reply.xselection.target = e->target;
    reply.xselection.time = e->time;
    Atom property = e->property == None ? e->target : e->property;
    reply.xselection.property = live ? property : None;
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    if (live) XChangeProperty(display, e->requestor, property, e->target, 8,
                              PropModeReplace, body, (int)body_length);
    XSendEvent(display, e->requestor, False, 0, &reply);
    XSync(display, False);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    if (live && !error) r.slow_sent++; else r.slow_abandoned++;
    g_free(request);
    return G_SOURCE_REMOVE;
}

static GdkFilterReturn slow_filter(GdkXEvent *event, GdkEvent *translated, gpointer data) {
    (void)translated; (void)data;
    XEvent *x = event;
    if (!is_source || !slow_ms || x->type != SelectionRequest) return GDK_FILTER_CONTINUE;
    XSelectionRequestEvent *e = &x->xselectionrequest;
    if (e->selection != selection_atom || e->target != gdk_x11_atom_to_xatom(data_target))
        return GDK_FILTER_CONTINUE;
    require(e->owner == r.source_xid && source_context && !r.end && body_length,
            "slow request belongs to the live XdndSelection source");
    require(r.slow_requests < 8, "bounded outstanding slow requests");
    struct SlowRequest *request = g_new0(struct SlowRequest, 1);
    request->event = *e; request->generation = r.gen;
    r.get++; r.slow_requests++;
    /* Do not sleep inside GTK's synchronous drag-data-get signal. Hold the real
     * X SelectionRequest and reply from a GLib timer; control ACK stays live. */
    g_timeout_add(slow_ms, send_slow, request);
    return GDK_FILTER_REMOVE;
}

static void clip_get(GtkClipboard *clipboard, GtkSelectionData *selection, guint info, gpointer data) {
    (void)clipboard; (void)info; (void)data;
    gtk_selection_data_set_text(selection, sentinel, -1);
}
static void clip_clear(GtkClipboard *clipboard, gpointer data) {
    (void)clipboard; (void)data;
    r.clip_bad++;
}
static void clip_received(GtkClipboard *clipboard, const gchar *text, gpointer data) {
    (void)clipboard; (void)data;
    Window owner = XGetSelectionOwner(inject, clipboard_atom);
    if (!text || strcmp(text, sentinel) || owner != clipboard_expected) r.clip_bad++;
    r.clip_checks++; clipboard_pending = false;
}
void selection_check_clipboard(Window owner) {
    require(!clipboard_pending, "one asynchronous clipboard sentinel check");
    clipboard_expected = owner; clipboard_pending = true;
    if (XGetSelectionOwner(inject, clipboard_atom) != owner) r.clip_bad++;
    gtk_clipboard_request_text(gtk_clipboard_get(GDK_SELECTION_CLIPBOARD), clip_received, NULL);
}

void selection_init(void) {
    int xres_major = 0, xres_minor = 0;
    require(XResQueryVersion(inject, &xres_major, &xres_minor)
            && xres_major >= 1 && xres_minor >= 2,
            "XRes 1.2 client-id query support (run linux-native-test.py)");
    data_target = gdk_atom_intern_static_string("UTF8_STRING");
    uri_target = gdk_atom_intern_static_string("text/uri-list");
    selection_atom = XInternAtom(inject, "XdndSelection", False);
    clipboard_atom = XInternAtom(inject, "CLIPBOARD", False);
    generation_atom = XInternAtom(inject, "_OINK_TEST_DND_GENERATION", False);
    app_generation_atom = XInternAtom(inject, "_OINK_TEST_APP_GENERATION", False);
    session_epoch_atom = XInternAtom(inject, "_OINK_TEST_SESSION_EPOCH", False);
    if (is_sentinel) {
        GtkTargetEntry entries[] = {{"UTF8_STRING", 0, 1}};
        require(gtk_clipboard_set_with_data(gtk_clipboard_get(GDK_SELECTION_CLIPBOARD),
                entries, 1, clip_get, clip_clear, NULL), "private clipboard sentinel ownership");
        gdk_display_sync(gdk_display_get_default());
        r.clip_owner = XGetSelectionOwner(inject, clipboard_atom);
        require(r.clip_owner != None, "sentinel has native owner");
    }
    if (is_source) {
        if (!is_proxy) {
            body_length = 48;
            for (size_t i = 0; i < body_length; i++) body[i] = (unsigned char)g_random_int_range(33, 127);
            payload_report();
        }
        gdk_window_add_filter(NULL, slow_filter, NULL);
    }
}
