use core::arch::asm;
use core::ptr::write_bytes;

use crate::arch::x86_64::{read_cr0, read_cr4, read_msr, write_cr0, write_cr4, write_msr};
use crate::{arch::x86_64::cpu, BackendKind, Error};

const IA32_FEATURE_CONTROL: u32 = 0x3a;
const IA32_VMX_BASIC: u32 = 0x480;
const IA32_VMX_PROCBASED_CTLS: u32 = 0x482;
const IA32_VMX_CR0_FIXED0: u32 = 0x486;
const IA32_VMX_CR0_FIXED1: u32 = 0x487;
const IA32_VMX_CR4_FIXED0: u32 = 0x488;
const IA32_VMX_CR4_FIXED1: u32 = 0x489;
const IA32_VMX_PROCBASED_CTLS2: u32 = 0x48b;
const IA32_VMX_EPT_VPID_CAP: u32 = 0x48c;

const FEATURE_CONTROL_LOCK: u64 = 1 << 0;
const FEATURE_CONTROL_VMX_OUTSIDE_SMX: u64 = 1 << 2;
const CR4_VMXE: u64 = 1 << 13;

pub struct Vmx {
    revision_id: u32,
    vmcs_phys: u64,
    active: bool,
}

impl Vmx {
    /// Enables VMX operation and selects an empty VMCS for the current CPU.
    ///
    /// # Safety
    /// Both pages must be writable, page-aligned physical addresses exclusively
    /// owned by mBoot. This function must run at CPL0 on a pinned logical CPU.
    pub unsafe fn enable(vmxon_phys: u64, vmcs_phys: u64) -> Result<Self, Error> {
        validate_page(vmxon_phys)?;
        validate_page(vmcs_phys)?;

        let features = cpu::detect();
        if features.backend != Some(BackendKind::IntelVmx) {
            return Err(Error::UnsupportedCpu);
        }
        // SAFETY: VMX CPUID support and CPL0 are established by the contract.
        if !unsafe { ept_available() } {
            return Err(Error::NestedPagingUnavailable);
        }

        // SAFETY: VMX CPUID support and CPL0 are established by the contract.
        let mut feature_control = unsafe { read_msr(IA32_FEATURE_CONTROL) };
        if feature_control & FEATURE_CONTROL_LOCK == 0 {
            feature_control |= FEATURE_CONTROL_LOCK | FEATURE_CONTROL_VMX_OUTSIDE_SMX;
            // SAFETY: The MSR is unlocked and only architectural VMX bits are set.
            unsafe { write_msr(IA32_FEATURE_CONTROL, feature_control) };
        } else if feature_control & FEATURE_CONTROL_VMX_OUTSIDE_SMX == 0 {
            return Err(Error::VirtualizationDisabled);
        }

        // SAFETY: VMX CPUID support makes IA32_VMX_BASIC available at CPL0.
        let basic = unsafe { read_msr(IA32_VMX_BASIC) };
        let region_size = ((basic >> 32) & 0x1fff) as usize;
        if region_size == 0 || region_size > 4096 {
            return Err(Error::ControlRegionTooLarge);
        }
        let supports_64_bit_phys = basic & (1 << 48) != 0;
        if !supports_64_bit_phys && (vmxon_phys > u32::MAX as u64 || vmcs_phys > u32::MAX as u64) {
            return Err(Error::InvalidPage);
        }
        let revision_id = basic as u32 & 0x7fff_ffff;

        // SAFETY: All registers are architectural VMX capabilities read at CPL0.
        let (cr0, cr4) = unsafe {
            (
                adjusted_control_register(
                    read_cr0(),
                    read_msr(IA32_VMX_CR0_FIXED0),
                    read_msr(IA32_VMX_CR0_FIXED1),
                ),
                adjusted_control_register(
                    read_cr4() | CR4_VMXE,
                    read_msr(IA32_VMX_CR4_FIXED0),
                    read_msr(IA32_VMX_CR4_FIXED1),
                ),
            )
        };
        // SAFETY: Fixed-bit masks produced valid control values and both pages
        // were validated as exclusively owned, writable VMX control regions.
        unsafe {
            write_cr0(cr0);
            write_cr4(cr4);
            initialize_control_region(vmxon_phys, revision_id);
            initialize_control_region(vmcs_phys, revision_id);
        }

        // SAFETY: The control registers and VMXON revision region are initialized.
        if unsafe { vmxon(vmxon_phys) }.is_err() {
            // SAFETY: VMXON failed, so VMXE can be cleared immediately.
            unsafe { write_cr4(read_cr4() & !CR4_VMXE) };
            return Err(Error::ControlInstructionFailed);
        }
        // SAFETY: VMX operation is active and the VMCS page has the right revision.
        let vmcs_loaded = unsafe { vmclear(vmcs_phys).and_then(|()| vmptrld(vmcs_phys)) };
        if vmcs_loaded.is_err() {
            // SAFETY: VMX operation is active and no guest has been launched.
            unsafe {
                vmxoff();
                write_cr4(read_cr4() & !CR4_VMXE);
            }
            return Err(Error::ControlInstructionFailed);
        }

        Ok(Self {
            revision_id,
            vmcs_phys,
            active: true,
        })
    }

    pub const fn revision_id(&self) -> u32 {
        self.revision_id
    }

    pub const fn vmcs_phys(&self) -> u64 {
        self.vmcs_phys
    }

    /// Leaves VMX operation on the current logical CPU.
    ///
    /// # Safety
    /// Must run on the same logical CPU that called `enable`, with no vCPU
    /// currently executing and no code relying on the current VMCS.
    pub unsafe fn disable(&mut self) {
        if self.active {
            // SAFETY: The caller guarantees the enabling CPU and no active vCPU.
            unsafe {
                vmxoff();
                write_cr4(read_cr4() & !CR4_VMXE);
            }
            self.active = false;
        }
    }
}

fn validate_page(phys: u64) -> Result<(), Error> {
    if phys == 0 || phys & 0xfff != 0 {
        Err(Error::InvalidPage)
    } else {
        Ok(())
    }
}

fn adjusted_control_register(current: u64, fixed_zero: u64, fixed_one: u64) -> u64 {
    (current | fixed_zero) & fixed_one
}

unsafe fn ept_available() -> bool {
    // SAFETY: The caller established VMX CPUID support and CPL0.
    let primary = unsafe { read_msr(IA32_VMX_PROCBASED_CTLS) };
    // SAFETY: The caller established VMX CPUID support and CPL0.
    let secondary = unsafe { read_msr(IA32_VMX_PROCBASED_CTLS2) };
    // SAFETY: The caller established VMX CPUID support and CPL0.
    let ept = unsafe { read_msr(IA32_VMX_EPT_VPID_CAP) };
    supports_ept(primary, secondary, ept)
}

fn supports_ept(primary: u64, secondary: u64, ept: u64) -> bool {
    let secondary_controls = primary >> 32 & (1 << 31) != 0;
    let ept_control = secondary >> 32 & (1 << 1) != 0;
    let four_level_walk = ept & (1 << 6) != 0;
    let write_back = ept & (1 << 14) != 0;
    secondary_controls && ept_control && four_level_walk && write_back
}

unsafe fn initialize_control_region(phys: u64, revision_id: u32) {
    // SAFETY: The caller validated exclusive writable ownership of this page.
    unsafe {
        write_bytes(phys as *mut u8, 0, 4096);
        (phys as *mut u32).write_volatile(revision_id);
    }
}

unsafe fn vmxon(phys: u64) -> Result<(), ()> {
    let failed: u8;
    // SAFETY: The caller prepared CR0/CR4 and the VMXON revision region.
    unsafe {
        asm!(
            "vmxon [{address}]",
            "setna {failed}",
            address = in(reg) &phys,
            failed = lateout(reg_byte) failed,
            options(nostack)
        );
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(())
    }
}

unsafe fn vmclear(phys: u64) -> Result<(), ()> {
    let failed: u8;
    // SAFETY: VMX is active and `phys` names an initialized VMCS page.
    unsafe {
        asm!(
            "vmclear [{address}]",
            "setna {failed}",
            address = in(reg) &phys,
            failed = lateout(reg_byte) failed,
            options(nostack)
        );
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(())
    }
}

unsafe fn vmptrld(phys: u64) -> Result<(), ()> {
    let failed: u8;
    // SAFETY: VMX is active and `phys` names a successfully cleared VMCS page.
    unsafe {
        asm!(
            "vmptrld [{address}]",
            "setna {failed}",
            address = in(reg) &phys,
            failed = lateout(reg_byte) failed,
            options(nostack)
        );
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(())
    }
}

unsafe fn vmxoff() {
    // SAFETY: The caller guarantees VMX operation is active on this CPU.
    unsafe { asm!("vmxoff", options(nostack)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_bits_are_applied_in_architectural_order() {
        assert_eq!(adjusted_control_register(0b0101, 0b0010, 0b0111), 0b0111);
    }

    #[test]
    fn control_pages_must_be_aligned() {
        assert_eq!(validate_page(0), Err(Error::InvalidPage));
        assert_eq!(validate_page(0x1001), Err(Error::InvalidPage));
        assert_eq!(validate_page(0x2000), Ok(()));
    }

    #[test]
    fn ept_requires_secondary_control_four_levels_and_write_back() {
        let primary = (1u64 << 31) << 32;
        let secondary = (1u64 << 1) << 32;
        let capabilities = (1 << 6) | (1 << 14);
        assert!(supports_ept(primary, secondary, capabilities));
        assert!(!supports_ept(primary, secondary, capabilities & !(1 << 6)));
        assert!(!supports_ept(primary, secondary, capabilities & !(1 << 14)));
        assert!(!supports_ept(0, secondary, capabilities));
        assert!(!supports_ept(primary, 0, capabilities));
    }
}
