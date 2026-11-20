#ifndef OINK_LINUX_DND_WIRE_H
#define OINK_LINUX_DND_WIRE_H

#define PAYLOAD_MAX (8 * 1024 * 1024)
#define MIME_UTF8 0u
#define MIME_TEXT 1u
#define MIME_URI_LIST 2u
#define MIME_OCTET 3u
#define INCR_THRESHOLD (256 * 1024)
#define INCR_CHUNK_MAX (64 * 1024)
#define STREAM_CHUNK_MAX (64 * 1024)
#define STREAM_QUEUE_MAX 4
#define LINE_MAXIMUM 2048
#define FIELDS(X) \
 X(gen) X(xid) X(x) X(y) X(source_xid) X(owner) X(epoch) X(armed) \
 X(begin) X(end) X(failed) X(cancelled) X(get) X(received) X(drop) X(deleted) \
 X(press) X(down) X(up) X(raw_down) X(raw_up) X(key_down) X(key_up) \
 X(motions) X(visits) X(leaves) X(inside) X(held_checks) X(returned) X(free_checks) \
 X(selected) X(button) X(length) X(hash) X(pending) X(stale) X(slow_requests) \
 X(slow_sent) X(slow_abandoned) X(clip_owner) X(clip_checks) X(clip_bad) \
 X(digest0) X(digest1) X(digest2) X(digest3) X(digest4) X(digest5) X(digest6) X(digest7) \
 X(toolkit_grabbed) X(app) \
 X(incr_handshake) X(incr_chunks) X(incr_bytes) X(incr_done) \
 X(incr_sent) X(incr_sent_bytes) X(incr_abandoned) \
 X(bridge_chunks) X(bridge_bytes) X(bridge_bad) \
 X(file_requests) X(file_reads) X(file_entries) X(file_bytes) X(file_bad) \
 X(staged) X(published)
#define DECLARE(name) unsigned long name;
struct Report { FIELDS(DECLARE) };
#undef DECLARE

enum Command {
    STATUS, ARM, FOCUS, MOVE, DOWN, UP, ESCAPE, HELD, FREE,
    READ_SELECTION, LOAD, CLIP_CHECK, DELAY, DISARM, QUIT, RETURNED,
    FETCH, ORPHAN_UP, ARM_APP, FETCH_STREAM, LOAD_STREAM,
    FILE_READ, FILE_STAGE, FILE_VERIFY, GENERATION
};
#endif
