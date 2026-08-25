// SPDX-License-Identifier: Apache-2.0

#define SYS_WRITE 1
#define SYS_OPEN 2
#define SYS_CLOSE 3
#define SYS_PREAD64 17
#define SYS_PWRITE64 18
#define SYS_PAUSE 34
#define SYS_MOUNT 165
#define O_RDWR 2
#define O_DIRECT 040000

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

static void write_message(long descriptor, const char *message, unsigned long length)
{
    syscall5(SYS_WRITE, descriptor, (long)message, (long)length, 0, 0);
}

static void mount_filesystem(const char *source, const char *target, const char *type)
{
    syscall5(SYS_MOUNT, (long)source, (long)target, (long)type, 0, 0);
}

static int verify_block_device(long log)
{
    static unsigned char write_buffer[4096] __attribute__((aligned(4096)));
    static unsigned char read_buffer[4096] __attribute__((aligned(4096)));
    static const char success[] = "mDriver block IRQ OK\n";
    long block;
    unsigned int pass;
    unsigned int index;

    block = syscall5(SYS_OPEN, (long)"/dev/vda", O_RDWR | O_DIRECT, 0, 0, 0);
    if (block < 0)
        return -1;
    for (pass = 0; pass < 2; pass++) {
        for (index = 0; index < sizeof(write_buffer); index++) {
            write_buffer[index] = (unsigned char)(index + pass + 1);
            read_buffer[index] = 0;
        }
        if (syscall5(SYS_PWRITE64, block, (long)write_buffer,
                     sizeof(write_buffer), pass * sizeof(write_buffer), 0) !=
            sizeof(write_buffer))
            goto fail;
        if (syscall5(SYS_PREAD64, block, (long)read_buffer,
                     sizeof(read_buffer), pass * sizeof(read_buffer), 0) !=
            sizeof(read_buffer))
            goto fail;
        for (index = 0; index < sizeof(write_buffer); index++)
            if (read_buffer[index] != write_buffer[index])
                goto fail;
    }
    syscall1(SYS_CLOSE, block);
    write_message(log, success, sizeof(success) - 1);
    return 0;

fail:
    syscall1(SYS_CLOSE, block);
    return -1;
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
    verify_block_device(log);
    write_message(log, ready, sizeof(ready) - 1);

    for (;;)
        syscall1(SYS_PAUSE, 0);
}
