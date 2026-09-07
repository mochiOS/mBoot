// SPDX-License-Identifier: Apache-2.0
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#define HELLO "MDEV-LOG/1 HELLO\n"
#define READY "MDEV-LOG/1 READY\n"

static int send_all(int fd, const void *bytes, size_t size)
{
    const char *p = bytes;
    while (size) {
        ssize_t n = send(fd, p, size, MSG_NOSIGNAL);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            return -1;
        p += n;
        size -= (size_t)n;
    }
    return 0;
}

static int connect_receiver(const struct sockaddr_in *address)
{
    int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (fd < 0)
        return -1;
    if (connect(fd, (const struct sockaddr *)address, sizeof(*address))) {
        struct pollfd pfd = { .fd = fd, .events = POLLOUT };
        int error = 0;
        socklen_t length = sizeof(error);
        if (errno != EINPROGRESS || poll(&pfd, 1, 2000) <= 0 ||
            getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &length) || error)
            goto fail;
    }
    if (fcntl(fd, F_SETFL, 0))
        goto fail;
    struct timeval timeout = { .tv_sec = 5 };
    if (setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout)) ||
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout)) ||
        send_all(fd, HELLO, sizeof(HELLO) - 1))
        goto fail;
    char response[sizeof(READY) - 1];
    size_t used = 0;
    while (used < sizeof(response)) {
        ssize_t n = recv(fd, response + used, sizeof(response) - used, 0);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            goto fail;
        used += (size_t)n;
    }
    if (memcmp(response, READY, sizeof(response)))
        goto fail;
    return fd;
fail:
    close(fd);
    return -1;
}

static int send_reports(int fd, const char *pattern)
{
    glob_t paths = {0};
    int result = 0;
    if (glob(pattern, 0, NULL, &paths)) {
        globfree(&paths);
        return send_all(fd, "GPU sysfs reports unavailable\n", 30);
    }
    for (size_t i = 0; i < paths.gl_pathc && !result; i++) {
        int report = open(paths.gl_pathv[i], O_RDONLY | O_CLOEXEC);
        if (report < 0)
            continue;
        char buffer[4096];
        ssize_t n = read(report, buffer, sizeof(buffer));
        if (n > 0)
            result = send_all(fd, buffer, (size_t)n);
        close(report);
    }
    globfree(&paths);
    return result;
}

static int release_probe(const char *path)
{
    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    ssize_t n = write(fd, "1\n", 2);
    int saved = errno;
    close(fd);
    errno = saved;
    return n == 2 || (n < 0 && saved == EALREADY) ? 0 : -1;
}

static int cmdline_server(char *server, size_t size)
{
    FILE *file = fopen("/proc/cmdline", "r");
    char command[4096];
    if (!file)
        return -1;
    char *line = fgets(command, sizeof(command), file);
    fclose(file);
    if (!line)
        return -1;
    char *save = NULL;
    for (char *token = strtok_r(command, " \n", &save); token;
         token = strtok_r(NULL, " \n", &save)) {
        const char key[] = "mboot.log=";
        if (!strncmp(token, key, sizeof(key) - 1)) {
            if (strlen(token + sizeof(key) - 1) >= size)
                return -1;
            strcpy(server, token + sizeof(key) - 1);
            return 0;
        }
    }
    return -1;
}

int main(int argc, char **argv)
{
    char server[INET_ADDRSTRLEN] = {0};
    const char *log_path = "/dev/kmsg";
    const char *probe_path = "/sys/kernel/mboot_gpu_probe_start";
    const char *reports = "/sys/bus/pci/devices/*/gpu_probe";
    unsigned long port = 6666;
    int once = 0, released = 0;
    cmdline_server(server, sizeof(server));
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--once")) {
            once = 1;
            continue;
        }
        if (i + 1 == argc)
            return 2;
        const char *key = argv[i++], *value = argv[i];
        if (!strcmp(key, "--server")) {
            if (strlen(value) >= sizeof(server))
                return 2;
            strcpy(server, value);
        } else if (!strcmp(key, "--port")) {
            char *end;
            port = strtoul(value, &end, 10);
            if (*end || !port || port > 65535)
                return 2;
        } else if (!strcmp(key, "--log")) {
            log_path = value;
        } else if (!strcmp(key, "--probe-start")) {
            probe_path = value;
        } else if (!strcmp(key, "--reports")) {
            reports = value;
        } else {
            return 2;
        }
    }
    if (!*server)
        return 0; /* Other configurations retain the existing boot path. */
    struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(port) };
    if (inet_pton(AF_INET, server, &address.sin_addr) != 1)
        return 2;
    signal(SIGPIPE, SIG_IGN);
    dprintf(2, "mDriver log: userspace started; waiting for receiver %s:%lu\n", server, port);
    for (;;) {
        int log = open(log_path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
        int fd = log >= 0 ? connect_receiver(&address) : -1;
        if (fd < 0) {
            if (log >= 0)
                close(log);
            if (once)
                return 1;
            sleep(2);
            continue;
        }
        /* Each connection replays retained kernel records, including early boot. */
        if (send_reports(fd, reports))
            goto reconnect;
        if (!released) {
            if (release_probe(probe_path)) {
                dprintf(2, "mDriver log: cannot release GPU probe errno=%d\n", errno);
                goto reconnect;
            }
            released = 1;
        }
        dprintf(2, "mDriver log: receiver %s:%lu acknowledged\n", server, port);
        for (;;) {
            char buffer[8192];
            ssize_t n = read(log, buffer, sizeof(buffer));
            if (n > 0) {
                if (send_all(fd, buffer, (size_t)n))
                    break;
            } else if (n < 0 && errno == EPIPE) {
                /* /dev/kmsg reports overwritten records, then resumes at oldest. */
                const char lost[] = "mDriver log: kernel ring overrun; records lost\n";
                if (send_all(fd, lost, sizeof(lost) - 1))
                    break;
            } else if (n < 0 && errno == EINTR) {
                continue;
            } else if (!n || errno == EAGAIN) {
                if (once) {
                    close(fd);
                    close(log);
                    return 0;
                }
                struct pollfd pfd = { .fd = fd, .events = POLLIN };
                if (poll(&pfd, 1, 100) > 0)
                    break; /* EOF, hangup, or unexpected server data: reconnect. */
            } else {
                break;
            }
        }
reconnect:
        close(fd);
        close(log);
        if (once)
            return 1;
        sleep(2);
    }
}
