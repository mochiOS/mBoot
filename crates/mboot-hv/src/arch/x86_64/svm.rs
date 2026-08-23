use core::ptr::write_bytes;

use crate::arch::x86_64::{cpu, read_msr, write_msr};
use crate::{BackendKind, Error, VmExit, VmExitReason};

const EFER: u32 = 0xc000_0080;
const VM_CR: u32 = 0xc001_0114;
const VM_HSAVE_PA: u32 = 0xc001_0117;
const EFER_SVME: u64 = 1 << 12;
const VM_CR_SVMDIS: u64 = 1 << 4;

const VMCB_INTERCEPT_MISC1: usize = 0x00c;
const VMCB_INTERCEPT_MISC2: usize = 0x010;
const VMCB_GUEST_ASID: usize = 0x058;
const VMCB_EXIT_CODE: usize = 0x070;
const VMCB_NP_ENABLE: usize = 0x090;
const VMCB_NCR3: usize = 0x0b0;
const VMCB_ES: usize = 0x400;
const VMCB_CS: usize = 0x410;
const VMCB_SS: usize = 0x420;
const VMCB_DS: usize = 0x430;
const VMCB_FS: usize = 0x440;
const VMCB_GS: usize = 0x450;
const VMCB_GDTR: usize = 0x460;
const VMCB_LDTR: usize = 0x470;
const VMCB_IDTR: usize = 0x480;
const VMCB_TR: usize = 0x490;
const VMCB_EFER: usize = 0x4d0;
const VMCB_CR4: usize = 0x548;
const VMCB_CR3: usize = 0x550;
const VMCB_CR0: usize = 0x558;
const VMCB_DR7: usize = 0x560;
const VMCB_DR6: usize = 0x568;
const VMCB_RFLAGS: usize = 0x570;
const VMCB_RIP: usize = 0x578;
const VMCB_RSP: usize = 0x5d8;
const VMCB_RAX: usize = 0x5f8;

const INTERCEPT_HLT: u32 = 1 << 24;
const INTERCEPT_VMRUN: u32 = 1;
const SVM_EXIT_HLT: u64 = 0x78;
const SEGMENT_CODE_REAL_MODE: u16 = 0x009b;
const SEGMENT_DATA_REAL_MODE: u16 = 0x0093;

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

    /// Runs a real-mode guest whose first byte is expected to be `HLT`.
    ///
    /// # Safety
    /// `nested_root` and the VMCB must remain exclusively owned and physically
    /// accessible. The caller must execute on the CPU that called `enable`.
    pub unsafe fn run(&mut self, nested_root: u64) -> Result<VmExit, Error> {
        validate_page(nested_root)?;
        if !self.active {
            return Err(Error::InvalidState);
        }

        // SAFETY: `enable` established exclusive ownership of this mapped VMCB.
        unsafe { initialize_guest(self.vmcb_phys, nested_root) };

        // SAFETY: EFER.SVME and VM_HSAVE_PA are configured, the VMCB is valid,
        // and interrupts are disabled. The guest executes only a HLT instruction.
        unsafe {
            core::arch::asm!(
                "vmrun rax",
                inlateout("rax") self.vmcb_phys => _,
                options(nostack)
            );
        }

        // SAFETY: VMEXIT completed and the processor wrote the control area.
        let exit_code = unsafe { read_u64(self.vmcb_phys, VMCB_EXIT_CODE) };
        if exit_code != SVM_EXIT_HLT {
            return Err(Error::UnexpectedVmExit(exit_code));
        }
        Ok(VmExit {
            reason: VmExitReason::Halt,
            raw_reason: exit_code,
        })
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

unsafe fn initialize_guest(vmcb: u64, nested_root: u64) {
    // SAFETY: The caller owns the complete VMCB page.
    unsafe {
        write_bytes(vmcb as *mut u8, 0, 4096);
        write_u32(vmcb, VMCB_INTERCEPT_MISC1, INTERCEPT_HLT);
        // VMRUN must never recurse into a guest-provided VMCB. AMD defines this
        // as a mandatory intercept for a valid first-level guest.
        write_u32(vmcb, VMCB_INTERCEPT_MISC2, INTERCEPT_VMRUN);
        write_u32(vmcb, VMCB_GUEST_ASID, 1);
        write_u64(vmcb, VMCB_NP_ENABLE, 1);
        write_u64(vmcb, VMCB_NCR3, nested_root);

        for offset in [VMCB_ES, VMCB_SS, VMCB_DS, VMCB_FS, VMCB_GS] {
            write_segment(vmcb, offset, 0, SEGMENT_DATA_REAL_MODE, 0xffff, 0);
        }
        write_segment(vmcb, VMCB_CS, 0, SEGMENT_CODE_REAL_MODE, 0xffff, 0);
        write_segment(vmcb, VMCB_GDTR, 0, 0, 0xffff, 0);
        write_segment(vmcb, VMCB_IDTR, 0, 0, 0xffff, 0);
        write_segment(vmcb, VMCB_LDTR, 0, 0x0082, 0xffff, 0);
        write_segment(vmcb, VMCB_TR, 0, 0x008b, 0xffff, 0);

        // SVME remains set in guest EFER while SVM is active, even though this
        // guest starts in real mode and does not use long mode.
        write_u64(vmcb, VMCB_EFER, EFER_SVME);
        write_u64(vmcb, VMCB_CR4, 0);
        write_u64(vmcb, VMCB_CR3, 0);
        write_u64(vmcb, VMCB_CR0, 0x10);
        write_u64(vmcb, VMCB_DR7, 0x400);
        write_u64(vmcb, VMCB_DR6, 0xffff_0ff0);
        write_u64(vmcb, VMCB_RFLAGS, 2);
        write_u64(vmcb, VMCB_RIP, 0);
        write_u64(vmcb, VMCB_RSP, 0x800);
        write_u64(vmcb, VMCB_RAX, 0);
    }
}

unsafe fn write_segment(
    vmcb: u64,
    offset: usize,
    selector: u16,
    attributes: u16,
    limit: u32,
    base: u64,
) {
    // SAFETY: All fields are within the state-save area of the owned VMCB.
    unsafe {
        write_u16(vmcb, offset, selector);
        write_u16(vmcb, offset + 2, attributes);
        write_u32(vmcb, offset + 4, limit);
        write_u64(vmcb, offset + 8, base);
    }
}

unsafe fn write_u16(base: u64, offset: usize, value: u16) {
    // SAFETY: The caller provides an in-bounds, suitably aligned VMCB field.
    unsafe { ((base as *mut u8).add(offset).cast::<u16>()).write_volatile(value) };
}

unsafe fn write_u32(base: u64, offset: usize, value: u32) {
    // SAFETY: The caller provides an in-bounds, suitably aligned VMCB field.
    unsafe { ((base as *mut u8).add(offset).cast::<u32>()).write_volatile(value) };
}

unsafe fn write_u64(base: u64, offset: usize, value: u64) {
    // SAFETY: The caller provides an in-bounds, suitably aligned VMCB field.
    unsafe { ((base as *mut u8).add(offset).cast::<u64>()).write_volatile(value) };
}

unsafe fn read_u64(base: u64, offset: usize) -> u64 {
    // SAFETY: The caller provides an in-bounds, suitably aligned VMCB field.
    unsafe { ((base as *const u8).add(offset).cast::<u64>()).read_volatile() }
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
