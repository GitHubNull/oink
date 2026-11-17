#define _POSIX_C_SOURCE 200809L
#define FUSE_USE_VERSION 31
#include <fuse3/fuse.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static const char payload[] = "oink-native-probe\n";
static const long fuse_magic = 0x65735546;

static int attributes(const char *path, struct stat *st, struct fuse_file_info *info) {
    (void)info;
    memset(st, 0, sizeof(*st));
    st->st_uid = getuid();
    st->st_gid = getgid();
    st->st_nlink = 1;
    if (strcmp(path, "/") == 0) {
        st->st_mode = S_IFDIR | 0555;
        return 0;
    }
    if (strcmp(path, "/probe.txt") == 0) {
        st->st_mode = S_IFREG | 0444;
        st->st_size = sizeof(payload) - 1;
        return 0;
    }
    return -ENOENT;
}

static int directory(const char *path, void *buffer, fuse_fill_dir_t fill, off_t offset,
                     struct fuse_file_info *info, enum fuse_readdir_flags flags) {
    (void)offset;
    (void)info;
    (void)flags;
    if (strcmp(path, "/") != 0)
        return -ENOENT;
    fill(buffer, ".", NULL, 0, 0);
    fill(buffer, "..", NULL, 0, 0);
    fill(buffer, "probe.txt", NULL, 0, 0);
    return 0;
}

static int open_file(const char *path, struct fuse_file_info *info) {
    if (strcmp(path, "/probe.txt") != 0)
        return -ENOENT;
    return (info->flags & O_ACCMODE) == O_RDONLY ? 0 : -EROFS;
}

static int read_file(const char *path, char *buffer, size_t size, off_t offset,
                     struct fuse_file_info *info) {
    (void)info;
    if (strcmp(path, "/probe.txt") != 0)
        return -ENOENT;
    if (offset < 0)
        return -EINVAL;
    if ((size_t)offset >= sizeof(payload) - 1)
        return 0;
    size_t available = sizeof(payload) - 1 - (size_t)offset;
    if (size > available)
        size = available;
    memcpy(buffer, payload + offset, size);
    return (int)size;
}

static bool mounted(const char *path) {
    struct statfs st;
    return statfs(path, &st) == 0 && st.f_type == fuse_magic;
}

int main(void) {
    const char *root = getenv("OINK_NATIVE_TEST_ROOT");
    const char *home = getenv("OINK_HOME");
    char mountpoint[4096], file[4096], expected_home[4096];
    struct stat st;
    if (!root || strncmp(root, "/tmp/oink-native-", 17) || strlen(root) > 3900
            || lstat(root, &st) != 0 || !S_ISDIR(st.st_mode)
            || st.st_uid != getuid() || (st.st_mode & 077) != 0 || getuid() == 0) {
        fprintf(stderr, "Run only through linux-native-test.py as an ordinary user\n");
        return 2;
    }
    snprintf(expected_home, sizeof(expected_home), "%s/oink", root);
    if (!home || strcmp(home, expected_home)) {
        fprintf(stderr, "Missing private application environment\n");
        return 2;
    }
    if (prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1) {
        fprintf(stderr, "FUSE gate blocked: NoNewPrivs=1 disables the installed setuid fusermount3.\n"
                        "Run this same isolated probe from an ordinary user terminal; "
                        "do not change device permissions or run Oink as root.\n");
        return 1;
    }
    snprintf(mountpoint, sizeof(mountpoint), "%s/mounts/read-only", root);
    snprintf(file, sizeof(file), "%s/mounts/read-only/probe.txt", root);
    if (mkdir(mountpoint, 0700) != 0) {
        perror("create fresh private mountpoint");
        return 1;
    }
    pid_t server = fork();
    if (server == -1) {
        perror("fork");
        return 1;
    }
    if (server == 0) {
        char *args[] = {"oink-fuse-probe", "-f", "-s", "-o",
                       "ro,nodev,nosuid,noexec,fsname=oink-native-probe,subtype=oink-probe",
                       mountpoint, NULL};
        const struct fuse_operations operations = {
            .getattr = attributes, .readdir = directory, .open = open_file, .read = read_file,
        };
        _exit(fuse_main(6, args, &operations, NULL));
    }
    int status = 0;
    bool exited = false;
    struct timespec interval = {.tv_sec = 0, .tv_nsec = 20000000};
    for (int i = 0; i < 150 && !mounted(mountpoint); i++) {
        if (waitpid(server, &status, WNOHANG) == server) {
            exited = true;
            break;
        }
        nanosleep(&interval, NULL);
    }
    if (!mounted(mountpoint)) {
        if (!exited) {
            kill(server, SIGTERM);
            waitpid(server, &status, 0);
        }
        fprintf(stderr, "Private FUSE mount unavailable; server_status=%d\n", status);
        return 1;
    }
    int fd = open(file, O_RDONLY);
    char buffer[sizeof(payload)] = {0};
    ssize_t count = fd >= 0 ? pread(fd, buffer, sizeof(buffer), 0) : -1;
    bool valid = count == (ssize_t)sizeof(payload) - 1
                 && memcmp(buffer, payload, sizeof(payload) - 1) == 0;
    if (fd >= 0) {
        char slice[5];
        valid = valid && pread(fd, slice, sizeof(slice), 5) == (ssize_t)sizeof(slice)
                && memcmp(slice, payload + 5, sizeof(slice)) == 0;
        close(fd);
    }
    errno = 0;
    fd = open(file, O_WRONLY);
    int write_error = errno;
    if (fd >= 0)
        close(fd);
    valid = valid && fd == -1 && write_error == EROFS;
    printf("private_fuse: content_and_offset=%s write_errno=%d expected=%d\n",
           valid ? "ok" : "failed", write_error, EROFS);
    pid_t unmount = fork();
    if (unmount == 0) {
        execlp("fusermount3", "fusermount3", "-u", mountpoint, (char *)NULL);
        _exit(127);
    }
    if (unmount < 0 || waitpid(unmount, &status, 0) != unmount
            || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "Private mount cleanup failed; preserve fixture directory\n");
        kill(server, SIGTERM);
        waitpid(server, &status, 0);
        return 1;
    }
    waitpid(server, &status, 0);
    if (mounted(mountpoint)) {
        fprintf(stderr, "Private FUSE mount is still present\n");
        return 1;
    }
    return valid ? 0 : 1;
}
