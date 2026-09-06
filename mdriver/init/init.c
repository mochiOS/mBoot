// SPDX-License-Identifier: Apache-2.0

#define SYS_WRITE 1
#define SYS_OPEN 2
#define SYS_NANOSLEEP 35
#define SYS_PAUSE 34
#define SYS_FORK 57
#define SYS_EXECVE 59
#define SYS_EXIT 60
#define SYS_WAIT4 61
#define SYS_MOUNT 165

struct kernel_timespec {
    long seconds;
    long nanoseconds;
};

static long syscall5(long number, long arg0, long arg1, long arg2, long arg3, long arg4)
{
    register long argument3 __asm__("r10") = arg3;
    register long argument4 __asm__("r8") = arg4;
    long result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(number), "D"(arg0), "S"(arg1), "d"(arg2),
                       "r"(argument3), "r"(argument4)
                     : "rcx", "r11", "memory");
    return result;
}

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
        "MDRIVER_GPU_DIAG_PRE_KMS=1",
        0,
    };
    static const struct kernel_timespec retry_delay = {
        .seconds = 2,
        .nanoseconds = 0,
    };

    for (;;) {
        long child = syscall5(SYS_FORK, 0, 0, 0, 0, 0);
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
    log = syscall5(SYS_OPEN, (long)"/dev/kmsg", 1, 0, 0, 0);
    if (log < 0)
        log = syscall5(SYS_OPEN, (long)"/dev/console", 1, 0, 0, 0);
    if (log < 0)
        log = 1;

    write_message(log, starting, sizeof(starting) - 1);
    mount_filesystem("proc", "/proc", "proc");
    mount_filesystem("sysfs", "/sys", "sysfs");
    if (syscall5(SYS_FORK, 0, 0, 0, 0, 0) == 0)
        supervise_gpu();
    write_message(log, ready, sizeof(ready) - 1);

    for (;;)
        syscall5(SYS_PAUSE, 0, 0, 0, 0, 0);
}
