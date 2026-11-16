// SPDX-License-Identifier: Apache-2.0

#include "syscall.h"

struct kernel_timespec {
    long seconds;
    long nanoseconds;
};

static void write_message(long descriptor, const char *message, unsigned long length)
{
    syscall5(SYS_WRITE, descriptor, (long)message, (long)length, 0, 0);
}

static void mount_filesystem(const char *source, const char *target, const char *type)
{
    syscall5(SYS_MOUNT, (long)source, (long)target, (long)type, 0, 0);
}

__attribute__((noreturn)) static void supervise_gpu(void)
{
    static char *const argv[] = { "/usr/bin/mdriver-gpu", 0 };
    static char *const envp[] = {
        "EGL_PLATFORM=gbm",
        "LIBGL_DRIVERS_PATH=/usr/lib/dri",
        "MDRIVER_GPU_TIMING=1",
        0,
    };
    static const struct kernel_timespec retry_delay = {
        .seconds = 2,
        .nanoseconds = 0,
    };

    for (;;) {
        long child = fork_process();
        if (child == 0) {
            syscall5(SYS_EXECVE, (long)argv[0], (long)argv, (long)envp, 0, 0);
            syscall5(SYS_EXIT, 127, 0, 0, 0, 0);
            for (;;)
                ;
        }
        if (child > 0)
            syscall5(SYS_WAIT4, child, 0, 0, 0, 0);
        syscall5(SYS_NANOSLEEP, (long)&retry_delay, 0, 0, 0, 0);
    }
}

__attribute__((noreturn)) void _start(void)
{
    static const char starting[] = "mDriver starting\n";
    static const char ready[] = "mDriver OK\n";
    long log;

    mount_filesystem("devtmpfs", "/dev", "devtmpfs");
    log = open_output("/dev/kmsg");
    if (log < 0)
        log = open_output("/dev/console");
    if (log < 0)
        log = 1;

    write_message(log, starting, sizeof(starting) - 1);
    /* Renderer and relay diagnostics must enter the same retained kernel log. */
    duplicate_output(log, 1);
    duplicate_output(log, 2);
    mount_filesystem("proc", "/proc", "proc");
    mount_filesystem("sysfs", "/sys", "sysfs");
    if (fork_process() == 0) {
        static char *const argv[] = { "/usr/bin/mdriver-log", 0 };
        static char *const envp[] = { 0 };
        syscall5(SYS_EXECVE, (long)argv[0], (long)argv, (long)envp, 0, 0);
        syscall5(SYS_EXIT, 127, 0, 0, 0, 0);
        for (;;)
            ;
    }
    if (fork_process() == 0)
        supervise_gpu();
    write_message(log, ready, sizeof(ready) - 1);

    for (;;)
        syscall5(SYS_PAUSE, 0, 0, 0, 0, 0);
}
