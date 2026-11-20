#include "linux_dnd_stream.h"
#include <glib-unix.h>
#include <errno.h>
#include <fcntl.h>

#define STREAM_MAGIC 0x4F4E4B31u
#define STREAM_TERMINATE 1u
#define STREAM_HEADER_LEN 44u
#define STREAM_FRAME_MAX (STREAM_HEADER_LEN + STREAM_CHUNK_MAX)

struct Header {
    uint32_t magic, epoch, generation, transfer, mime, flags;
    uint64_t offset, total;
    uint32_t chunk_len;
};

static int fd = -1;
static guint out_watch, in_watch;

struct SendState { bool active; size_t offset; uint32_t transfer; };
struct RecvState {
    bool active; size_t offset; uint32_t transfer;
    uint64_t total; uint32_t epoch, generation, mime; unsigned long seq;
};

static struct SendState send_state;
static struct RecvState recv_state;
static unsigned char out_frame[STREAM_FRAME_MAX];
static size_t out_len, out_done;
static unsigned char in_header[STREAM_HEADER_LEN];
static size_t in_want, in_done;
static bool in_header_phase;
static uint32_t in_flags;

static void header_pack(unsigned char *out, const struct Header *h) {
    uint32_t words[6] = {h->magic, h->epoch, h->generation, h->transfer, h->mime, h->flags};
    for (int i = 0; i < 6; i++)
        for (int b = 0; b < 4; b++) out[i * 4 + b] = (unsigned char)(words[i] >> (8 * b));
    for (int b = 0; b < 8; b++) out[24 + b] = (unsigned char)(h->offset >> (8 * b));
    for (int b = 0; b < 8; b++) out[32 + b] = (unsigned char)(h->total >> (8 * b));
    for (int b = 0; b < 4; b++) out[40 + b] = (unsigned char)(h->chunk_len >> (8 * b));
}

static bool header_unpack(struct Header *h, const unsigned char *in) {
    uint32_t words[6];
    for (int i = 0; i < 6; i++) {
        words[i] = 0;
        for (int b = 0; b < 4; b++) words[i] |= (uint32_t)in[i * 4 + b] << (8 * b);
    }
    uint64_t offset = 0, total = 0;
    for (int b = 0; b < 8; b++) offset |= (uint64_t)in[24 + b] << (8 * b);
    for (int b = 0; b < 8; b++) total |= (uint64_t)in[32 + b] << (8 * b);
    uint32_t chunk_len = 0;
    for (int b = 0; b < 4; b++) chunk_len |= (uint32_t)in[40 + b] << (8 * b);
    h->magic = words[0]; h->epoch = words[1]; h->generation = words[2];
    h->transfer = words[3]; h->mime = words[4]; h->flags = words[5];
    h->offset = offset; h->total = total; h->chunk_len = chunk_len;
    return h->magic == STREAM_MAGIC && h->mime <= MIME_OCTET;
}

static gboolean stream_writable(gint source_fd, GIOCondition condition, gpointer data) {
    (void)condition; (void)data;
    for (;;) {
        if (out_done == out_len) {
            if (!send_state.active) { out_watch = 0; return G_SOURCE_REMOVE; }
            size_t remaining = body_length - send_state.offset;
            size_t chunk = remaining > STREAM_CHUNK_MAX ? STREAM_CHUNK_MAX : remaining;
            struct Header h = {.magic = STREAM_MAGIC, .epoch = (uint32_t)r.epoch,
                               .generation = (uint32_t)r.gen, .transfer = send_state.transfer,
                               .mime = body_mime, .flags = chunk == remaining ? STREAM_TERMINATE : 0,
                               .offset = send_state.offset, .total = body_length,
                               .chunk_len = (uint32_t)chunk};
            header_pack(out_frame, &h);
            memcpy(out_frame + STREAM_HEADER_LEN, body + send_state.offset, chunk);
            out_len = STREAM_HEADER_LEN + chunk;
            out_done = 0;
        }
        ssize_t written = write(source_fd, out_frame + out_done, out_len - out_done);
        if (written < 0) {
            require(errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR, "stream channel write");
            return G_SOURCE_CONTINUE;
        }
        out_done += (size_t)written;
        if (out_done == out_len) {
            r.bridge_chunks++;
            r.bridge_bytes += out_len - STREAM_HEADER_LEN;
            send_state.offset += out_len - STREAM_HEADER_LEN;
            out_len = out_done = 0;
            if (send_state.offset == body_length) send_state.active = false;
        }
    }
}

static void recv_fail(unsigned long seq, unsigned long status) {
    r.bridge_bad++;
    memset(&recv_state, 0, sizeof(recv_state));
    in_want = in_done = 0; in_header_phase = true;
    in_watch = 0;
    report(seq, status);
}

static gboolean stream_readable(gint source_fd, GIOCondition condition, gpointer data) {
    (void)condition; (void)data;
    for (;;) {
        if (!recv_state.active) { in_watch = 0; return G_SOURCE_REMOVE; }
        if (!in_want) { in_want = STREAM_HEADER_LEN; in_header_phase = true; }
        unsigned char *target = in_header_phase ? in_header : body + recv_state.offset;
        ssize_t got = read(source_fd, target + in_done, in_want - in_done);
        if (got < 0) {
            require(errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR, "stream channel read");
            return G_SOURCE_CONTINUE;
        }
        require(got > 0, "stream payload channel closed mid-transfer");
        in_done += (size_t)got;
        if (in_done != in_want) continue;
        if (in_header_phase) {
            struct Header h;
            if (!header_unpack(&h, in_header) || !h.chunk_len
                    || h.epoch != recv_state.epoch || h.generation != recv_state.generation
                    || h.transfer != recv_state.transfer || h.offset != recv_state.offset
                    || h.total != recv_state.total || h.chunk_len > STREAM_CHUNK_MAX
                    || h.chunk_len > h.total - recv_state.offset) {
                recv_fail(recv_state.seq, 2);
                return G_SOURCE_REMOVE;
            }
            in_flags = h.flags;
            recv_state.mime = h.mime;
            in_header_phase = false;
            in_want = h.chunk_len;
            in_done = 0;
            continue;
        }
        r.bridge_chunks++;
        r.bridge_bytes += in_want;
        recv_state.offset += in_want;
        bool last = (in_flags & STREAM_TERMINATE) != 0;
        in_want = in_done = 0; in_header_phase = true;
        if (last != (recv_state.offset == recv_state.total)) {
            recv_fail(recv_state.seq, 2);
            return G_SOURCE_REMOVE;
        }
        if (last) {
            body_length = recv_state.total;
            body_mime = recv_state.mime;
            payload_report();
            unsigned long seq = recv_state.seq;
            memset(&recv_state, 0, sizeof(recv_state));
            report(seq, 0);
        }
    }
}

unsigned long stream_fetch(unsigned long seq) {
    if (!is_catcher || !r.armed || !body_length || r.pending || send_state.active) return 2;
    send_state.active = true;
    send_state.offset = 0;
    send_state.transfer++;
    (void)seq;
    if (!out_watch) out_watch = g_unix_fd_add(fd, G_IO_OUT, stream_writable, NULL);
    return 0;
}

unsigned long stream_load(unsigned long seq, unsigned long total,
                          unsigned long epoch, unsigned long generation) {
    /* The plain source role carries an auto-generated small body; a streamed
     * payload may replace it before the drag begins. Proxies start empty and
     * must not overwrite a loaded body. */
    if (!is_source || r.begin || recv_state.active
            || !total || total > PAYLOAD_MAX || !generation) return 2;
    if (body_length && is_proxy) return 2;
    recv_state.active = true;
    recv_state.offset = 0;
    recv_state.transfer++;
    recv_state.total = total;
    recv_state.epoch = (uint32_t)epoch;
    recv_state.generation = (uint32_t)generation;
    recv_state.seq = seq;
    in_want = in_done = 0; in_header_phase = true;
    if (!in_watch) in_watch = g_unix_fd_add(fd, G_IO_IN | G_IO_HUP | G_IO_ERR, stream_readable, NULL);
    return STREAM_DEFERRED;
}

void stream_init(void) {
    const char *env = getenv("OINK_STREAM_FD");
    fd = env ? atoi(env) : STREAM_FD_FALLBACK;
    require(fcntl(fd, F_SETFL, O_NONBLOCK) == 0, "nonblocking stream payload channel");
}
