#ifndef OINK_LINUX_DND_WIRE_H
#define OINK_LINUX_DND_WIRE_H

#define PAYLOAD_MAX 128
#define LINE_MAXIMUM 2048
#define FIELDS(X) \
 X(gen) X(xid) X(x) X(y) X(source_xid) X(owner) X(epoch) X(armed) \
 X(begin) X(end) X(failed) X(cancelled) X(get) X(received) X(drop) X(deleted) \
 X(press) X(down) X(up) X(raw_down) X(raw_up) X(key_down) X(key_up) \
 X(motions) X(visits) X(leaves) X(inside) X(held_checks) X(returned) X(free_checks) \
 X(selected) X(button) X(length) X(hash) X(pending) X(stale) X(slow_requests) \
 X(slow_sent) X(slow_abandoned) X(clip_owner) X(clip_checks) X(clip_bad) \
 X(digest0) X(digest1) X(digest2) X(digest3) X(digest4) X(digest5) X(digest6) X(digest7) \
 X(toolkit_grabbed)
#define DECLARE(name) unsigned long name;
struct Report { FIELDS(DECLARE) };
#undef DECLARE

enum Command {
    STATUS, ARM, FOCUS, MOVE, DOWN, UP, ESCAPE, HELD, FREE,
    READ_SELECTION, LOAD, CLIP_CHECK, DELAY, DISARM, QUIT, RETURNED,
    FETCH, ORPHAN_UP
};
#endif
