// SPDX-License-Identifier: Apache-2.0

#define SYS_WRITE 1
#define SYS_PAUSE 34
#define SYS_MOUNT 165

static long syscall1(long number, long arg0)
{
    long result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(number), "D"(arg0)
                     : "rcx", "r11", "memory");
    return result;
}

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

static void write_message(const char *message, unsigned long length)
{
    syscall5(SYS_WRITE, 1, (long)message, (long)length, 0, 0);
}

static void mount_filesystem(const char *source, const char *target, const char *type)
{
    syscall5(SYS_MOUNT, (long)source, (long)target, (long)type, 0, 0);
}

__attribute__((noreturn)) void _start(void)
{
    static const char starting[] = "mDriver starting\n";
    static const char thanks[] = "Linuxを作り、育ててきた皆さんに感謝します。\n";
    static const char ready[] = "mDriver OK\n";

    write_message(starting, sizeof(starting) - 1);
    write_message(thanks, sizeof(thanks) - 1);
    mount_filesystem("devtmpfs", "/dev", "devtmpfs");
    mount_filesystem("proc", "/proc", "proc");
    mount_filesystem("sysfs", "/sys", "sysfs");
    write_message(ready, sizeof(ready) - 1);

    for (;;)
        syscall1(SYS_PAUSE, 0);
}
