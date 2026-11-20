#ifndef OINK_LINUX_DND_STREAM_H
#define OINK_LINUX_DND_STREAM_H
#include "linux_dnd_roles.h"

/* Binary payload channel on an inherited nonblocking FD (env OINK_STREAM_FD).
 * Chunks carry a fixed little-endian header binding epoch/generation/transfer,
 * MIME, offset and total; STREAM_CHUNK_MAX per chunk, PAYLOAD_MAX per
 * transfer, one transfer per direction at a time, event-loop driven. */
#define STREAM_FD_FALLBACK 3
#define STREAM_DEFERRED 0xffffffffUL

void stream_init(void);
unsigned long stream_fetch(unsigned long seq);
unsigned long stream_load(unsigned long seq, unsigned long total,
                           unsigned long epoch, unsigned long generation);
#endif
