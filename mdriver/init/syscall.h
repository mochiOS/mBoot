/* SPDX-License-Identifier: Apache-2.0 */
#ifndef MDRIVER_INIT_SYSCALL_H
#define MDRIVER_INIT_SYSCALL_H

#if defined(__x86_64__)
#define SYS_WRITE 1
#define SYS_READ 0
#define SYS_CLOSE 3
#define SYS_FSYNC 74
#define SYS_OPEN 2
#define SYS_DUP2 33
#define SYS_NANOSLEEP 35
#define SYS_PAUSE 34
#define SYS_FORK 57
#define SYS_EXECVE 59
#define SYS_EXIT 60
#define SYS_WAIT4 61
#define SYS_MOUNT 165

static inline long syscall5(long number, long arg0, long arg1, long arg2,
                            long arg3, long arg4)
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
static inline long open_output(const char *path)
{ return syscall5(SYS_OPEN, (long)path, 1, 0, 0, 0); }
static inline long open_input(const char *path)
{ return syscall5(SYS_OPEN, (long)path, 0, 0, 0, 0); }
static inline long fork_process(void)
{ return syscall5(SYS_FORK, 0, 0, 0, 0, 0); }
static inline long duplicate_output(long descriptor, long target)
{ return syscall5(SYS_DUP2, descriptor, target, 0, 0, 0); }

#elif defined(__aarch64__)
#define SYS_WRITE 64
#define SYS_READ 63
#define SYS_CLOSE 57
#define SYS_FSYNC 82
#define SYS_NANOSLEEP 101
#define SYS_PAUSE 73 /* ppoll(NULL, 0, NULL, NULL, 0), no timeout/mask. */
#define SYS_EXECVE 221
#define SYS_EXIT 93
#define SYS_WAIT4 260
#define SYS_MOUNT 40

static inline long syscall5(long number, long arg0, long arg1, long arg2,
                            long arg3, long arg4)
{
    register long result __asm__("x0") = arg0;
    register long argument1 __asm__("x1") = arg1;
    register long argument2 __asm__("x2") = arg2;
    register long argument3 __asm__("x3") = arg3;
    register long argument4 __asm__("x4") = arg4;
    register long selector __asm__("x8") = number;
    __asm__ volatile("svc #0" : "+r"(result)
                     : "r"(argument1), "r"(argument2), "r"(argument3),
                       "r"(argument4), "r"(selector) : "memory", "cc");
    return result;
}
static inline long open_output(const char *path)
{ return syscall5(56, -100, (long)path, 1, 0, 0); } /* openat(AT_FDCWD). */
static inline long open_input(const char *path)
{ return syscall5(56, -100, (long)path, 0, 0, 0); }
static inline long fork_process(void)
{ return syscall5(220, 17, 0, 0, 0, 0); } /* clone(SIGCHLD), private address space. */
static inline long duplicate_output(long descriptor, long target)
{ return descriptor == target ? target : syscall5(24, descriptor, target, 0, 0, 0); }
#else
#error Unsupported Linux Hardware Domain architecture
#endif
#endif
