#ifndef OINK_LINUX_DND_INCR_H
#define OINK_LINUX_DND_INCR_H
#include "linux_dnd_roles.h"

/* INCR sender state machine (source roles serving bodies above
 * INCR_THRESHOLD) and passive INCR receive instrumentation. The sender drives
 * the property-delete handshake from the GDK event loop via a window filter;
 * the receiver side is handled by GTK and only observed, never contested. */
void incr_init(void);
void incr_poll(void);
bool incr_sending(void);
#endif
