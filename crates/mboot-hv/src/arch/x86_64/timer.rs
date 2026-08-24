use core::arch::{asm, x86_64::__cpuid, x86_64::_rdtsc};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{read_msr, write_msr};

pub const VECTOR: u8 = 0xf0;

const IA32_APIC_BASE: u32 = 0x1b;
const IA32_TSC_DEADLINE: u32 = 0x6e0;
const APIC_ENABLE: u64 = 1 << 11;
const X2APIC_ENABLE: u64 = 1 << 10;
const X2APIC_EOI: u32 = 0x80b;
const X2APIC_TPR: u32 = 0x808;
const X2APIC_SIVR: u32 = 0x80f;
const X2APIC_LVT_TIMER: u32 = 0x832;
const X2APIC_LVT_LINT0: u32 = 0x835;
const X2APIC_LVT_LINT1: u32 = 0x836;
const X2APIC_INITIAL_COUNT: u32 = 0x838;
const X2APIC_DIVIDE: u32 = 0x83e;
const SIVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const LVT_TSC_DEADLINE: u32 = 1 << 18;
const DIVIDE_BY_16: u32 = 0x3;
const INITIAL_COUNT: u32 = 1_000_000;

static TSC_DEADLINE_INTERVAL: AtomicU64 = AtomicU64::new(0);

/// Starts a Local APIC timer for vCPU time slicing.
///
/// A calibrated TSC deadline supplies 10 ms slices when the CPU reports enough
/// frequency information. Older CPUs use a conservative periodic APIC count.
pub unsafe fn initialize() -> bool {
    let apic_base = unsafe { read_msr(IA32_APIC_BASE) };
    if apic_base & APIC_ENABLE == 0 {
        return false;
    }
    if apic_base & X2APIC_ENABLE != 0 {
        unsafe {
            write_msr(X2APIC_SIVR, u64::from(SIVR_ENABLE | 0xff));
            write_msr(X2APIC_TPR, 0);
            write_msr(X2APIC_LVT_LINT0, u64::from(LVT_MASKED));
            write_msr(X2APIC_LVT_LINT1, u64::from(LVT_MASKED));
        }
        if initialize_tsc_deadline(true) {
            return true;
        }
        unsafe {
            write_msr(X2APIC_DIVIDE, u64::from(DIVIDE_BY_16));
            write_msr(
                X2APIC_LVT_TIMER,
                u64::from(LVT_PERIODIC | u32::from(VECTOR)),
            );
            write_msr(X2APIC_INITIAL_COUNT, u64::from(INITIAL_COUNT));
        }
        return true;
    }

    if __cpuid(1).edx & (1 << 9) == 0 {
        return false;
    }
    let base = (apic_base & 0xffff_f000) as *mut u32;
    unsafe {
        write_mmio(base, 0x0f0, SIVR_ENABLE | 0xff);
        write_mmio(base, 0x080, 0);
        write_mmio(base, 0x350, LVT_MASKED);
        write_mmio(base, 0x360, LVT_MASKED);
    }
    if initialize_tsc_deadline(false) {
        return true;
    }
    unsafe {
        write_mmio(base, 0x3e0, DIVIDE_BY_16);
        write_mmio(base, 0x320, LVT_PERIODIC | u32::from(VECTOR));
        write_mmio(base, 0x380, INITIAL_COUNT);
    }
    true
}

pub fn acknowledge() {
    let apic_base = unsafe { read_msr(IA32_APIC_BASE) };
    if apic_base & X2APIC_ENABLE != 0 {
        unsafe { write_msr(X2APIC_EOI, 0) };
    } else {
        let base = (apic_base & 0xffff_f000) as *mut u32;
        unsafe { write_mmio(base, 0x0b0, 0) };
    }
}

fn initialize_tsc_deadline(x2apic: bool) -> bool {
    let features = __cpuid(1);
    if features.ecx & (1 << 24) == 0 {
        return false;
    }
    let Some(frequency) = tsc_frequency_hz() else {
        return false;
    };
    let interval = frequency / 100;
    if interval == 0 {
        return false;
    }
    if x2apic {
        unsafe {
            write_msr(
                X2APIC_LVT_TIMER,
                u64::from(LVT_TSC_DEADLINE | u32::from(VECTOR)),
            )
        };
    } else {
        let apic_base = unsafe { read_msr(IA32_APIC_BASE) };
        let base = (apic_base & 0xffff_f000) as *mut u32;
        unsafe { write_mmio(base, 0x320, LVT_TSC_DEADLINE | u32::from(VECTOR)) };
    }
    TSC_DEADLINE_INTERVAL.store(interval, Ordering::Release);
    true
}

/// Arms a fresh deadline immediately before entering a guest. This prevents a
/// slow host-side scheduling pass from consuming the next guest's whole slice.
pub(crate) fn prepare_entry() {
    let interval = TSC_DEADLINE_INTERVAL.load(Ordering::Acquire);
    if interval != 0 {
        unsafe { write_msr(IA32_TSC_DEADLINE, _rdtsc().wrapping_add(interval)) };
    }
}

/// Returns the monotonic counter used by virtual Local APIC timers.
pub fn now() -> u64 {
    unsafe { _rdtsc() }
}

pub fn tsc_frequency_hz() -> Option<u64> {
    let maximum_leaf = __cpuid(0).eax;
    if maximum_leaf >= 0x15 {
        let ratio = __cpuid(0x15);
        if ratio.eax != 0 && ratio.ebx != 0 && ratio.ecx != 0 {
            return u64::from(ratio.ecx)
                .checked_mul(u64::from(ratio.ebx))?
                .checked_div(u64::from(ratio.eax));
        }
    }
    if maximum_leaf >= 0x16 {
        let frequency = __cpuid(0x16).eax & 0xffff;
        if frequency != 0 {
            return Some(u64::from(frequency) * 1_000_000);
        }
    }
    None
}

unsafe fn write_mmio(base: *mut u32, offset: usize, value: u32) {
    unsafe {
        base.byte_add(offset).write_volatile(value);
        asm!("mfence", options(nostack, preserves_flags));
    }
}
