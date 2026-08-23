use core::arch::asm;

#[inline]
pub(crate) unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: The caller guarantees CPL0 and a valid MSR number.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack));
    }
    (u64::from(high) << 32) | u64::from(low)
}

#[inline]
pub(crate) unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: The caller guarantees CPL0 and a value accepted by this MSR.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack)
        );
    }
}

#[inline]
pub(crate) unsafe fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: The caller guarantees CPL0.
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

#[inline]
pub(crate) unsafe fn write_cr0(value: u64) {
    // SAFETY: The caller guarantees CPL0 and a valid CR0 value.
    unsafe { asm!("mov cr0, {}", in(reg) value, options(nostack, preserves_flags)) };
}

#[inline]
pub(crate) unsafe fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: The caller guarantees CPL0.
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

#[inline]
pub(crate) unsafe fn write_cr4(value: u64) {
    // SAFETY: The caller guarantees CPL0 and a valid CR4 value.
    unsafe { asm!("mov cr4, {}", in(reg) value, options(nostack, preserves_flags)) };
}
