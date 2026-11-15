// Private non-VHE world switch. Register offsets are asserted in vcpu.rs.
.section .text.mboot_aarch64_run, "ax"
.balign 16
.global mboot_aarch64_run
mboot_aarch64_run:
    // 96 GPR + 512 SIMD + FP controls + host vector + context + exit scratch.
    sub sp, sp, #672
    stp x19, x20, [sp, #0]
    stp x21, x22, [sp, #16]
    stp x23, x24, [sp, #32]
    stp x25, x26, [sp, #48]
    stp x27, x28, [sp, #64]
    stp x29, x30, [sp, #80]
    stp q0, q1, [sp, #96]
    stp q2, q3, [sp, #128]
    stp q4, q5, [sp, #160]
    stp q6, q7, [sp, #192]
    stp q8, q9, [sp, #224]
    stp q10, q11, [sp, #256]
    stp q12, q13, [sp, #288]
    stp q14, q15, [sp, #320]
    stp q16, q17, [sp, #352]
    stp q18, q19, [sp, #384]
    stp q20, q21, [sp, #416]
    stp q22, q23, [sp, #448]
    stp q24, q25, [sp, #480]
    stp q26, q27, [sp, #512]
    stp q28, q29, [sp, #544]
    stp q30, q31, [sp, #576]
    mrs x9, fpcr
    str x9, [sp, #608]
    mrs x9, fpsr
    str x9, [sp, #616]
    mrs x9, vbar_el2
    str x9, [sp, #624]
    str x0, [sp, #632]
    adr x9, mboot_aarch64_vcpu_vectors
    msr vbar_el2, x9
    isb
    ldr x9, [x0, #248]
    msr elr_el2, x9
    ldr x9, [x0, #256]
    msr spsr_el2, x9
    ldr x9, [x0, #264]
    msr fpcr, x9
    ldr x9, [x0, #272]
    msr fpsr, x9
    ldp q0, q1, [x0, #288]
    ldp q2, q3, [x0, #320]
    ldp q4, q5, [x0, #352]
    ldp q6, q7, [x0, #384]
    ldp q8, q9, [x0, #416]
    ldp q10, q11, [x0, #448]
    ldp q12, q13, [x0, #480]
    ldp q14, q15, [x0, #512]
    ldp q16, q17, [x0, #544]
    ldp q18, q19, [x0, #576]
    ldp q20, q21, [x0, #608]
    ldp q22, q23, [x0, #640]
    ldp q24, q25, [x0, #672]
    ldp q26, q27, [x0, #704]
    ldp q28, q29, [x0, #736]
    ldp q30, q31, [x0, #768]
    ldp x2, x3, [x0, #16]
    ldp x4, x5, [x0, #32]
    ldp x6, x7, [x0, #48]
    ldp x8, x9, [x0, #64]
    ldp x10, x11, [x0, #80]
    ldp x12, x13, [x0, #96]
    ldp x14, x15, [x0, #112]
    ldp x16, x17, [x0, #128]
    ldp x18, x19, [x0, #144]
    ldp x20, x21, [x0, #160]
    ldp x22, x23, [x0, #176]
    ldp x24, x25, [x0, #192]
    ldp x26, x27, [x0, #208]
    ldp x28, x29, [x0, #224]
    ldr x30, [x0, #240]
    ldp x0, x1, [x0]
    isb
    eret

.balign 2048
mboot_aarch64_vcpu_vectors:
    .rept 8
    b .Lhost_fault
    .balign 128
    .endr
    b .Lguest_0
    .balign 128
    b .Lguest_1
    .balign 128
    b .Lguest_2
    .balign 128
    b .Lguest_3
    .balign 128
    b .Lguest_0
    .balign 128
    b .Lguest_1
    .balign 128
    b .Lguest_2
    .balign 128
    b .Lguest_3
    .balign 128
.Lguest_0:
    msr daifset, #15
    str x0, [sp, #640]
    str x1, [sp, #648]
    mov x0, #0
    str x0, [sp, #656]
    b .Lsave_guest
.Lguest_1:
    msr daifset, #15
    str x0, [sp, #640]
    str x1, [sp, #648]
    mov x0, #1
    str x0, [sp, #656]
    b .Lsave_guest
.Lguest_2:
    msr daifset, #15
    str x0, [sp, #640]
    str x1, [sp, #648]
    mov x0, #2
    str x0, [sp, #656]
    b .Lsave_guest
.Lguest_3:
    msr daifset, #15
    str x0, [sp, #640]
    str x1, [sp, #648]
    mov x0, #3
    str x0, [sp, #656]
    b .Lsave_guest
.Lsave_guest:
    ldr x0, [sp, #632]
    stp x2, x3, [x0, #16]
    stp x4, x5, [x0, #32]
    stp x6, x7, [x0, #48]
    stp x8, x9, [x0, #64]
    stp x10, x11, [x0, #80]
    stp x12, x13, [x0, #96]
    stp x14, x15, [x0, #112]
    stp x16, x17, [x0, #128]
    stp x18, x19, [x0, #144]
    stp x20, x21, [x0, #160]
    stp x22, x23, [x0, #176]
    stp x24, x25, [x0, #192]
    stp x26, x27, [x0, #208]
    stp x28, x29, [x0, #224]
    str x30, [x0, #240]
    ldr x1, [sp, #640]
    str x1, [x0]
    ldr x1, [sp, #648]
    str x1, [x0, #8]
    mrs x1, elr_el2
    str x1, [x0, #248]
    mrs x1, spsr_el2
    str x1, [x0, #256]
    mrs x1, fpcr
    str x1, [x0, #264]
    mrs x1, fpsr
    str x1, [x0, #272]
    mrs x1, esr_el2
    str x1, [x0, #800]
    mrs x1, far_el2
    str x1, [x0, #808]
    mrs x1, hpfar_el2
    str x1, [x0, #816]
    ldr x1, [sp, #656]
    str x1, [x0, #824]
    stp q0, q1, [x0, #288]
    stp q2, q3, [x0, #320]
    stp q4, q5, [x0, #352]
    stp q6, q7, [x0, #384]
    stp q8, q9, [x0, #416]
    stp q10, q11, [x0, #448]
    stp q12, q13, [x0, #480]
    stp q14, q15, [x0, #512]
    stp q16, q17, [x0, #544]
    stp q18, q19, [x0, #576]
    stp q20, q21, [x0, #608]
    stp q22, q23, [x0, #640]
    stp q24, q25, [x0, #672]
    stp q26, q27, [x0, #704]
    stp q28, q29, [x0, #736]
    stp q30, q31, [x0, #768]
    // No Rust runs until the host FP/SIMD bank has been restored.
    ldr x9, [sp, #608]
    msr fpcr, x9
    ldr x9, [sp, #616]
    msr fpsr, x9
    ldp q0, q1, [sp, #96]
    ldp q2, q3, [sp, #128]
    ldp q4, q5, [sp, #160]
    ldp q6, q7, [sp, #192]
    ldp q8, q9, [sp, #224]
    ldp q10, q11, [sp, #256]
    ldp q12, q13, [sp, #288]
    ldp q14, q15, [sp, #320]
    ldp q16, q17, [sp, #352]
    ldp q18, q19, [sp, #384]
    ldp q20, q21, [sp, #416]
    ldp q22, q23, [sp, #448]
    ldp q24, q25, [sp, #480]
    ldp q26, q27, [sp, #512]
    ldp q28, q29, [sp, #544]
    ldp q30, q31, [sp, #576]
    ldr x9, [sp, #624]
    msr vbar_el2, x9
    isb
    ldp x19, x20, [sp, #0]
    ldp x21, x22, [sp, #16]
    ldp x23, x24, [sp, #32]
    ldp x25, x26, [sp, #48]
    ldp x27, x28, [sp, #64]
    ldp x29, x30, [sp, #80]
    add sp, sp, #672
    ret

.Lhost_fault:
    // EL2 faults are host failures, never fabricated guest exits.
    msr daifset, #15
1:  wfi
    b 1b

