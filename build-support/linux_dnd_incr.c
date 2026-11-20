#include "linux_dnd_incr.h"
#include <X11/Xatom.h>

#define INCR_TRANSFER_MAX_MS 30000L /* drag-lifetime bounded */

static Atom incr_atom;

struct IncrSend {
    bool active;
    bool final_sent;    /* all data chunks written; awaiting the requestor's
                           final PropertyDelete before the zero-length chunk */
    Window requestor;
    Atom property, target;
    Time time;
    gint64 started;
    size_t total, offset, next_chunk;
};

static struct IncrSend send;

static void send_zero_length(struct IncrSend *t, Display *display) {
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    XChangeProperty(display, t->requestor, t->property, t->target, 8,
                    PropModeReplace, (unsigned char *)"", 0);
    XSync(display, False);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    (void)error;
}

/* A transfer always ends with a zero-length chunk once the handshake was
 * announced, so a GTK receiver never hangs; abandoned transfers short-change
 * the receiver and are counted, never silently retried. */
static void transfer_finish(struct IncrSend *t, Display *display, bool terminated, bool abandoned) {
    if (!t->active) return;
    if (!terminated) send_zero_length(t, display);
    if (abandoned) r.incr_abandoned++;
    memset(t, 0, sizeof(*t));
}

static void transfer_send_chunk(struct IncrSend *t, Display *display) {
    size_t chunk = t->offset + t->next_chunk >= t->total ? t->total - t->offset : t->next_chunk;
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    XChangeProperty(display, t->requestor, t->property, t->target, 8,
                    PropModeReplace, body + t->offset, (int)chunk);
    XSync(display, False);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    if (error) { transfer_finish(t, display, false, true); return; }
    r.incr_sent++; r.incr_sent_bytes += chunk;
    t->offset += chunk;
    if (t->next_chunk < INCR_CHUNK_MAX) t->next_chunk *= 2;
    /* Do NOT send the zero-length chunk here: it would overwrite the final
     * data chunk before the requestor reads it. Wait for the requestor's
     * PropertyDelete of the final chunk, then send the terminator. */
    if (t->offset == t->total) t->final_sent = true;
}

static bool transfer_abandoned(const struct IncrSend *t) {
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    return r.end || !source_context
            || XGetSelectionOwner(display, selection_atom) != r.source_xid
            || body_length != t->total
            || g_get_monotonic_time() / 1000 - t->started > INCR_TRANSFER_MAX_MS;
}

static void transfer_start(struct IncrSend *t, XSelectionRequestEvent *e, Display *display) {
    memset(t, 0, sizeof(*t));
    t->active = true;
    t->requestor = e->requestor;
    t->property = e->property == None ? e->target : e->property;
    t->target = e->target;
    t->time = e->time;
    t->started = g_get_monotonic_time() / 1000;
    t->total = body_length; t->next_chunk = 4096;
    unsigned long size = (unsigned long)t->total;
    XEvent reply = {0};
    reply.xselection.type = SelectionNotify;
    reply.xselection.display = display;
    reply.xselection.requestor = e->requestor;
    reply.xselection.selection = e->selection;
    reply.xselection.target = e->target;
    reply.xselection.property = t->property;
    reply.xselection.time = e->time;
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    /* PropertyNotify on the foreign requestor window tells us when GTK has
     * consumed a chunk; per-client event mask, it disturbs no other client. */
    XSelectInput(display, e->requestor, PropertyChangeMask);
    XChangeProperty(display, e->requestor, t->property, incr_atom, 32,
                    PropModeReplace, (unsigned char *)&size, 1);
    XSendEvent(display, e->requestor, False, 0, &reply);
    XSync(display, False);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    if (error) memset(t, 0, sizeof(*t));
}

static GdkFilterReturn send_filter(GdkXEvent *event, GdkEvent *translated, gpointer data) {
    (void)translated; (void)data;
    XEvent *x = event;
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    if (x->type == PropertyNotify && send.active && x->xproperty.state == PropertyDelete
            && x->xproperty.window == send.requestor && x->xproperty.atom == send.property) {
        if (send.final_sent) {
            transfer_finish(&send, display, false, false);
        } else {
            transfer_send_chunk(&send, display);
        }
        return GDK_FILTER_CONTINUE;
    }
    if (x->type != SelectionRequest || !is_source || !source_context || r.end
            || body_length <= INCR_THRESHOLD) return GDK_FILTER_CONTINUE;
    XSelectionRequestEvent *e = &x->xselectionrequest;
    if (e->selection != selection_atom || e->owner != r.source_xid || !body_length) return GDK_FILTER_CONTINUE;
    require(e->target == gdk_x11_atom_to_xatom(data_target)
            || e->target == XInternAtom(display, "text/plain", False),
            "INCR XdndSelection UTF8_STRING/text/plain only");
    require(!send.active, "bounded outstanding INCR transfers");
    r.get++;
    transfer_start(&send, e, display);
    return GDK_FILTER_REMOVE;
}

static Window recv_window;
static Atom recv_atom;
static bool recv_expect;    /* a Delete was seen; next NewValue is a new chunk */

static GdkFilterReturn recv_filter(GdkXEvent *event, GdkEvent *translated, gpointer data) {
    (void)translated; (void)data;
    XEvent *x = event;
    if (x->type != PropertyNotify) return GDK_FILTER_CONTINUE;
    if (!r.pending) {
        recv_window = None; recv_atom = None; recv_expect = false;
        return GDK_FILTER_CONTINUE;
    }
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    if (x->xproperty.state == PropertyDelete) {
        if (x->xproperty.window == recv_window && x->xproperty.atom == recv_atom)
            recv_expect = true;
        return GDK_FILTER_CONTINUE;
    }
    if (x->xproperty.state != PropertyNewValue) return GDK_FILTER_CONTINUE;
    Atom type;
    int format;
    unsigned long count, remaining;
    unsigned char *prop = NULL;
    gdk_x11_display_error_trap_push(gdk_display_get_default());
    int status = XGetWindowProperty(display, x->xproperty.window, x->xproperty.atom,
                                    0, 0, False, AnyPropertyType, &type, &format,
                                    &count, &remaining, &prop);
    int error = gdk_x11_display_error_trap_pop(gdk_display_get_default());
    if (prop) XFree(prop);
    if (error || status != Success || !format) return GDK_FILTER_CONTINUE;
    if (!recv_window) {
        /* GTK retrieves through a per-screen IPC widget window, not our box;
         * latch the first INCR announcement seen while a read is pending. */
        if (type != incr_atom) return GDK_FILTER_CONTINUE;
        recv_window = x->xproperty.window;
        recv_atom = x->xproperty.atom;
        r.incr_handshake++;
        return GDK_FILTER_CONTINUE;
    }
    if (x->xproperty.window != recv_window || x->xproperty.atom != recv_atom
            || type == incr_atom || !recv_expect)
        return GDK_FILTER_CONTINUE;    /* duplicate NewValue, not yet consumed */
    recv_expect = false;
    /* Queried with length 0: remaining is the full property size in bytes. */
    r.incr_chunks++;
    r.incr_bytes += remaining;
    if (remaining == 0) r.incr_done++;
    return GDK_FILTER_CONTINUE;
}

void incr_poll(void) {
    if (send.active && transfer_abandoned(&send)) {
        Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
        transfer_finish(&send, display, false, true);
    }
}

bool incr_sending(void) { return send.active; }

void incr_init(void) {
    Display *display = GDK_DISPLAY_XDISPLAY(gdk_display_get_default());
    incr_atom = XInternAtom(display, "INCR", False);
    gdk_window_add_filter(NULL, send_filter, NULL);
    gdk_window_add_filter(NULL, recv_filter, NULL);
}
