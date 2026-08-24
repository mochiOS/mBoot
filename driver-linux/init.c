#define SYS_WRITE 1
#define SYS_PAUSE 34

static long syscall1(long number, long arg0)
{
    long result;
    __asm__ volatile("syscall" : "=a"(result) : "a"(number), "D"(arg0) : "rcx", "r11", "memory");
    return result;
}

static long syscall3(long number, long arg0, long arg1, long arg2)
{
    long result;
    __asm__ volatile("syscall" : "=a"(result) : "a"(number), "D"(arg0), "S"(arg1), "d"(arg2) : "rcx", "r11", "memory");
    return result;
}

void _start(void)
{
    static const char message[] = "Driver Linux OK\n";
    syscall3(SYS_WRITE, 1, (long)message, sizeof(message) - 1);
    for (;;)
        syscall1(SYS_PAUSE, 0);
}
