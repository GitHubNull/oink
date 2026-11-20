#include "linux_dnd_file.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <sys/types.h>

static char file_root[PATH_MAX];
static char real_root[PATH_MAX];
static const char *staging_name = "staging";
static const char *published_name = "published";

static void sha256_file(int fd, unsigned char digest[32], uint64_t *total) {
    GChecksum *checksum = g_checksum_new(G_CHECKSUM_SHA256);
    unsigned char chunk[STREAM_CHUNK_MAX];
    uint64_t offset = 0;
    for (;;) {
        ssize_t got = pread(fd, chunk, sizeof(chunk), (off_t)offset);
        require(got >= 0, "pread published file at offset");
        if (!got) break;
        g_checksum_update(checksum, chunk, (gssize)got);
        offset += (uint64_t)got;
    }
    gsize length = 32;
    g_checksum_get_digest(checksum, digest, &length);
    g_checksum_free(checksum);
    require(length == 32, "sha256 digest size");
    *total = offset;
}

static bool digest_matches(const unsigned char digest[32]) {
    const unsigned long *words[] = {&r.digest0, &r.digest1, &r.digest2, &r.digest3,
                                   &r.digest4, &r.digest5, &r.digest6, &r.digest7};
    for (size_t i = 0; i < G_N_ELEMENTS(words); i++) {
        unsigned long word = 0;
        for (size_t j = 0; j < 4; j++) word = (word << 8) | digest[i * 4 + j];
        if (*words[i] != word) return false;
    }
    return true;
}

/* Relative path validation: components separated by '/', no empty, '.', '..',
 * no control bytes, no backslash; portable staging names only. */
static size_t check_rel(const unsigned long *bytes, size_t count, char *out, size_t cap) {
    require(count && count < cap - 1, "bounded relative path");
    size_t used = 0;
    bool component_start = true;
    size_t component_len = 0;
    for (size_t i = 0; i < count; i++) {
        unsigned long b = bytes[i];
        require(b > 31 && b < 127 && b != '\\', "portable relative path byte");
        if (b == '/') {
            require(!component_start && component_len, "no empty or dot path components");
            component_start = true; component_len = 0;
        } else {
            require(b != '.' || component_len, "no dot path components");
            component_start = false; component_len++;
        }
        out[used++] = (char)b;
    }
    require(!component_start && component_len, "no trailing path component");
    out[used] = '\0';
    return used;
}

static void ensure_dir(const char *path) {
    if (mkdir(path, 0700) == 0) return;
    struct stat st;
    require(errno == EEXIST && lstat(path, &st) == 0 && S_ISDIR(st.st_mode)
            && st.st_uid == getuid() && (st.st_mode & 077) == 0, "private staging directory");
}

static void join3(char *out, size_t cap, const char *a, const char *b, const char *c) {
    require(snprintf(out, cap, "%s/%s/%s", a, b, c) < (int)cap, "bounded file path");
}

static void resolve_existing(const char *rel, char *out, size_t cap, struct stat *st) {
    char candidate[PATH_MAX];
    (void)cap;
    require(snprintf(candidate, sizeof(candidate), "%s/%s", file_root, rel) < (int)sizeof(candidate),
            "bounded file path");
    require(realpath(candidate, out) && strncmp(out, real_root, strlen(real_root)) == 0
            && out[strlen(real_root)] == '/',
            "path must stay inside the authorized private root");
    require(lstat(out, st) == 0, "resolved path exists");
}

/* Final component must not exist; its parent must resolve inside the root. */
static void resolve_new(const char *rel, char *out, size_t cap) {
    char copy[PATH_MAX];
    require(strlen(rel) < sizeof(copy), "bounded relative path");
    strcpy(copy, rel);
    char *slash = strrchr(copy, '/');
    const char *leaf = slash ? slash + 1 : copy;
    if (slash) {
        *slash = '\0';
        char parent[PATH_MAX];
        resolve_existing(copy, parent, sizeof(parent), &(struct stat){0});
        require(snprintf(out, cap, "%s/%s", parent, leaf) < (int)cap, "bounded file path");
    } else {
        require(snprintf(out, cap, "%s/%s", real_root, leaf) < (int)cap, "bounded file path");
    }
    struct stat st;
    require(lstat(out, &st) < 0 && errno == ENOENT, "publish target must not exist");
}

static int name_cmp(const void *a, const void *b) {
    return strcmp(*(const char *const *)a, *(const char *const *)b);
}

/* Deterministic recursive walk: regular files as "F <relpath> <size>\n",
 * directories recursed, symlinks listed as "L <relpath>\n" (never followed). */
static void walk_dir(const char *abs, const char *rel, size_t *used, unsigned *depth) {
    require(*depth < 32, "bounded directory depth");
    int fd = open(abs, O_RDONLY | O_DIRECTORY);
    require(fd >= 0, "open published directory");
    DIR *dir = fdopendir(fd);
    require(dir, "read published directory");
    GPtrArray *names = g_ptr_array_new_with_free_func(g_free);
    struct dirent *entry;
    while ((entry = readdir(dir))) {
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, "..")) continue;
        g_ptr_array_add(names, g_strdup(entry->d_name));
    }
    closedir(dir);
    g_ptr_array_sort(names, name_cmp);
    for (unsigned i = 0; i < names->len; i++) {
        const char *name = g_ptr_array_index(names, i);
        char child_abs[PATH_MAX], child_rel[PATH_MAX];
        require(snprintf(child_abs, sizeof(child_abs), "%s/%s", abs, name) < (int)sizeof(child_abs)
                && snprintf(child_rel, sizeof(child_rel), "%s/%s", rel, name) < (int)sizeof(child_rel),
                "bounded walk path");
        struct stat st;
        require(lstat(child_abs, &st) == 0, "walk entry lstat");
        int line = snprintf((char *)body + *used, body_length - *used, "%c %s %lu\n",
                            S_ISLNK(st.st_mode) ? 'L' : S_ISDIR(st.st_mode) ? 'D' : 'F',
                            child_rel, S_ISREG(st.st_mode) ? (unsigned long)st.st_size : 0UL);
        require(line > 0 && (size_t)line < body_length - *used, "bounded listing buffer");
        *used += (size_t)line;
        r.file_entries++;
        if (S_ISREG(st.st_mode)) r.file_bytes += (unsigned long)st.st_size;
        if (S_ISDIR(st.st_mode)) {
            unsigned next = *depth + 1;
            walk_dir(child_abs, child_rel, used, &next);
        }
    }
    g_ptr_array_free(names, TRUE);
}

unsigned long file_read(unsigned long seq, const unsigned long *bytes, size_t count) {
    (void)seq;
    if (!is_source || r.begin || !count) { r.file_bad++; return 2; }
    char rel[PATH_MAX];
    check_rel(bytes, count, rel, sizeof(rel));
    char path[PATH_MAX];
    struct stat st;
    resolve_existing(rel, path, sizeof(path), &st);
    if (!S_ISREG(st.st_mode) || (uint64_t)st.st_size > PAYLOAD_MAX) { r.file_bad++; return 2; }
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    require(fd >= 0, "open authorized source file");
    uint64_t total;
    unsigned char digest[32], reread[32];
    sha256_file(fd, digest, &total);
    require((uint64_t)st.st_size == total, "source file size stable during read");
    require(pread(fd, body, (size_t)total, 0) == (ssize_t)total, "read authorized source file");
    close(fd);
    /* The digest report fields still describe the previous payload here, so
     * detect a mid-read change by hashing the body that was actually read. */
    GChecksum *verify = g_checksum_new(G_CHECKSUM_SHA256);
    gsize reread_len = sizeof(reread);
    g_checksum_update(verify, body, (gssize)total);
    g_checksum_get_digest(verify, reread, &reread_len);
    g_checksum_free(verify);
    require(reread_len == sizeof(reread) && !memcmp(digest, reread, sizeof(reread)),
            "source file changed while reading");
    body_length = (size_t)total;
    body_mime = MIME_OCTET;
    payload_report();
    r.file_requests++;
    return 0;
}

unsigned long file_stage(unsigned long seq, const unsigned long *bytes, size_t count) {
    if (!is_proxy || r.begin || !count) { r.file_bad++; return 2; }
    char rel[PATH_MAX];
    check_rel(bytes, count, rel, sizeof(rel));
    /* Staging publishes under the published/ subtree only. */
    char published_rel[PATH_MAX + 16];
    require(snprintf(published_rel, sizeof(published_rel), "%s/%s", published_name, rel)
            < (int)sizeof(published_rel), "bounded publish path");
    char target[PATH_MAX], tmp[PATH_MAX];
    resolve_new(published_rel, target, sizeof(target));
    require(snprintf(tmp, sizeof(tmp), "%s/%s/tmp-%lu-%lu-%lu", real_root, staging_name,
                     r.gen, r.epoch, seq) < (int)sizeof(tmp), "bounded staging path");
    struct stat st;
    require(lstat(tmp, &st) < 0 && errno == ENOENT, "staging name collision");
    /* O_RDWR: the staging file is re-read on fd for the publish-time re-hash. */
    int fd = open(tmp, O_RDWR | O_CREAT | O_EXCL, 0600);
    require(fd >= 0, "create private staging file");
    size_t done = 0;
    while (done < body_length) {
        ssize_t put = write(fd, body + done, body_length - done);
        require(put > 0, "write staging file");
        done += (size_t)put;
    }
    require(fsync(fd) == 0, "fsync staging file");
    lseek(fd, 0, SEEK_SET);
    uint64_t total;
    unsigned char digest[32];
    sha256_file(fd, digest, &total);
    require(close(fd) == 0, "close staging file");
    if (total != body_length || !digest_matches(digest)) {
        unlink(tmp);
        r.file_bad++;
        return 2;
    }
    r.staged++;
    if (rename(tmp, target) != 0) {
        unlink(tmp);
        r.file_bad++;
        return 2;
    }
    r.published++;
    payload_clear();    /* the staged payload is consumed; proxy may load the next one */
    return 0;
}

unsigned long file_verify(unsigned long seq, const unsigned long *bytes, size_t count) {
    (void)seq;
    if (is_source || !count) { r.file_bad++; return 2; }
    char rel[PATH_MAX];
    check_rel(bytes, count, rel, sizeof(rel));
    char path[PATH_MAX];
    struct stat st;
    resolve_existing(rel, path, sizeof(path), &st);
    r.file_reads++;
    if (S_ISREG(st.st_mode)) {
        require((uint64_t)st.st_size <= PAYLOAD_MAX, "bounded verify file");
        int fd = open(path, O_RDONLY | O_NOFOLLOW);
        require(fd >= 0, "open published file");
        uint64_t total;
        unsigned char digest[32];
        sha256_file(fd, digest, &total);
        require(pread(fd, body, (size_t)total, 0) == (ssize_t)total, "pread published file");
        require(close(fd) == 0, "close published file");
        require((uint64_t)st.st_size == total, "published file size stable");
        body_length = (size_t)total;
        body_mime = MIME_OCTET;
        payload_report();
        r.file_bytes += (unsigned long)total;
    } else if (S_ISDIR(st.st_mode)) {
        size_t used = 0;
        unsigned depth = 0;
        body_length = PAYLOAD_MAX;
        walk_dir(path, rel, &used, &depth);
        body_length = used;
        payload_report();
    } else {
        r.file_bad++;
        return 2;
    }
    return 0;
}

void file_init(void) {
    const char *override = getenv("OINK_FILE_ROOT");
    const char *root_env = getenv("OINK_NATIVE_TEST_ROOT");
    require((override && snprintf(file_root, sizeof(file_root), "%s", override) < (int)sizeof(file_root))
            || (root_env && snprintf(file_root, sizeof(file_root), "%s/files", root_env)
                            < (int)sizeof(file_root)),
            "file staging root configured");
    struct stat st;
    if (lstat(file_root, &st) != 0) {
        require(errno == ENOENT && mkdir(file_root, 0700) == 0, "create private file root");
        require(lstat(file_root, &st) == 0, "stat private file root");
    }
    require(S_ISDIR(st.st_mode) && st.st_uid == getuid() && (st.st_mode & 077) == 0,
            "private owned file root");
    require(realpath(file_root, real_root) == real_root, "resolve file root");
    char path[PATH_MAX];
    join3(path, sizeof(path), real_root, staging_name, "");
    path[strlen(path) - 1] = '\0';
    ensure_dir(path);
    join3(path, sizeof(path), real_root, published_name, "");
    path[strlen(path) - 1] = '\0';
    ensure_dir(path);
}
