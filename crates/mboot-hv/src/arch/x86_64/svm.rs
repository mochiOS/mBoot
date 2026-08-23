use core::ptr::write_bytes;

use crate::arch::x86_64::{cpu, read_msr, write_msr};
use crate::{BackendKind, Error};

const EFER: u32 = 0xc000_0080;
const VM_CR: u32 = 0xc001_0114;
const VM_HSAVE_PA: u32 = 0xc001_0117;
const EFER_SVME: u64 = 1 << 12;
const VM_CR_SVMDIS: u64 = 1 << 4;

pub struct Svm {
    hsave_phys: u64,
    vmcb_phys: u64,
    active: bool,
}

impl Svm {
    /// Enables AMD SVM and assigns host-save and VMCB pages.
    ///
    /// # Safety
    /// Both pages must be writable, page-aligned physical addresses exclusively
    /// owned by mBoot. This function must run at CPL0 on a pinned logical CPU.
    pub unsafe fn enable(hsave_phys: u64, vmcb_phys: u64) -> Result<Self, Error> {
        validate_page(hsave_phys)?;
        validate_page(vmcb_phys)?;

        let features = cpu::detect();
        if features.backend != Some(BackendKind::AmdSvm) {
            return Err(Error::UnsupportedCpu);
        }
        if features.nested_paging != Some(true) {
            return Err(Error::NestedPagingUnavailable);
        }
        // SAFETY: CPUID confirmed AMD SVM and the caller guarantees CPL0.
        if unsafe { read_msr(VM_CR) } & VM_CR_SVMDIS != 0 {
            return Err(Error::VirtualizationDisabled);
        }

        // SAFETY: The page ownership and CPL0 requirements are in the contract.
        unsafe {
            write_bytes(hsave_phys as *mut u8, 0, 4096);
            write_bytes(vmcb_phys as *mut u8, 0, 4096);
            write_msr(VM_HSAVE_PA, hsave_phys);
            write_msr(EFER, read_msr(EFER) | EFER_SVME);
        }

        Ok(Self {
            hsave_phys,
            vmcb_phys,
            active: true,
        })
    }

    pub const fn hsave_phys(&self) -> u64 {
        self.hsave_phys
    }

    pub const fn vmcb_phys(&self) -> u64 {
        self.vmcb_phys
    }

    /// Disables SVM on the current logical CPU.
    ///
    /// # Safety
    /// Must run on the same logical CPU that called `enable`, with no guest
    /// executing and no code relying on the configured host-save area.
    pub unsafe fn disable(&mut self) {
        if self.active {
            // SAFETY: The caller guarantees the enabling CPU and no active guest.
            unsafe { write_msr(EFER, read_msr(EFER) & !EFER_SVME) };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svm_control_pages_must_be_aligned() {
        assert_eq!(validate_page(0x1234), Err(Error::InvalidPage));
        assert_eq!(validate_page(0x4000), Ok(()));
    }
}
