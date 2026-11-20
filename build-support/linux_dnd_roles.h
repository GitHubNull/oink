#ifndef OINK_LINUX_DND_ROLES_H
#define OINK_LINUX_DND_ROLES_H
#define _POSIX_C_SOURCE 200809L
#include <gtk/gtk.h>
#include <gdk/gdkx.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Test-only wire protocol: bounded ASCII unsigned decimal tokens, newline framed.
 * Request: sequence opcode generation [arguments]. Response: R sequence status
 * followed by FIELDS below. Status 0=ACK, 1=stale generation, 2=invalid state/input.
 * FETCH also emits D sequence generation length byte... (catcher -> test bridge
 * -> proxy LOAD only). Target has no payload input command. No product transport.
 * Every process has a 90-second hard lifetime and nonblocking stdin/stdout.
 */
#include "linux_dnd_native.h"
extern struct Report r;
extern GtkWidget *window, *box;
extern GdkDragContext *source_context, *incoming_context;
extern Display *inject, *observer;
extern GdkAtom data_target;
extern Atom selection_atom, generation_atom, app_generation_atom, session_epoch_atom;
extern bool app_armed;
extern unsigned long app_pid, app_epoch;
extern bool is_source, is_proxy, is_catcher, is_sentinel;
extern unsigned char body[PAYLOAD_MAX];
extern size_t body_length;
extern unsigned int body_mime;
extern unsigned int slow_ms;
extern Window expected_source;
extern unsigned long pending_epoch, pending_gen;
extern GdkDragContext *pending_context;

void require(bool condition, const char *message);
void require_isolated(void);
void emit(const char *format, ...) G_GNUC_PRINTF(1, 2);
void report(unsigned long seq, unsigned long status);
void native_init(void);
void native_poll(void);
void motion(int x, int y);
void button(bool down);
void escape_key(void);
void held_context(unsigned long allowed_get);
void no_grab(void);
void pointer_at_source(void);
void selection_init(void);
void selection_check_clipboard(Window owner);
void selection_publish_generation(void);
bool selection_identity(GdkDragContext *context);
bool app_identity(GdkDragContext *context);
bool identity_valid(GdkDragContext *context);
uint32_t payload_hash(const unsigned char *bytes, size_t length);
void payload_report(void);
void payload_clear(void);
void selection_get(GtkWidget *, GdkDragContext *, GtkSelectionData *, guint, guint, gpointer);
void selection_received(GtkWidget *, GdkDragContext *, gint, gint, GtkSelectionData *, guint, guint, gpointer);
#endif
