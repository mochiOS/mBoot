use core::arch::global_asm;
use core::ptr::write_bytes;

use crate::arch::x86_64::{cpu, read_msr, write_msr};
use crate::{BackendKind, Error, GuestConfig, VmExit, VmExitReason};

const EFER: u32 = 0xc000_0080;
const VM_CR: u32 = 0xc001_0114;
const VM_HSAVE_PA: u32 = 0xc001_0117;
const EFER_SVME: u64 = 1 << 12;
const VM_CR_SVMDIS: u64 = 1 << 4;

const VMCB_INTERCEPT_MISC1: usize = 0x00c;
const VMCB_INTERCEPT_MISC2: usize = 0x010;
const VMCB_MSRPM_BASE_PA: usize = 0x048;
const VMCB_GUEST_ASID: usize = 0x058;
const VMCB_TLB_CONTROL: usize = 0x05c;
const VMCB_INTERRUPT_CONTROL: usize = 0x060;
const VMCB_INTERRUPT_VECTOR: usize = 0x064;
const VMCB_EXIT_CODE: usize = 0x070;
const VMCB_EXIT_INFO1: usize = 0x078;
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
const INTERCEPT_MSR_PROT: u32 = 1 << 28;
const INTERCEPT_VMRUN: u32 = 1;
const INTERCEPT_VMMCALL: u32 = 1 << 1;
const TLB_CONTROL_FLUSH_ALL: u8 = 1;
const SVM_EXIT_HLT: u64 = 0x78;
const SVM_EXIT_VMMCALL: u64 = 0x81;
const SVM_EXIT_MSR: u64 = 0x7c;
const SVM_EXIT_INTR: u64 = 0x60;
const INTERCEPT_INTR: u32 = 1;
const V_INTR_MASKING: u64 = 1 << 24;
const V_IRQ: u64 = 1 << 8;
const V_INTR_PRIORITY: u64 = 0x4 << 16;
const V_IGNORE_TPR: u64 = 1 << 20;
const SEGMENT_CODE_LONG_MODE: u16 = 0x0a9b;
const SEGMENT_DATA_LONG_MODE: u16 = 0x0c93;

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct SvmRunContext {
    rax: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rcx: u64,
}

global_asm!(
    ".global mboot_svm_enter",
    "mboot_svm_enter:",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "push rdi",
    "push rsi",
    "mov rax, [rsp]",
    "mov rbx, [rax + 32]",
    "mov rbp, [rax + 40]",
    "mov r12, [rax + 48]",
    "mov r13, [rax + 56]",
    "mov r14, [rax + 64]",
    "mov r15, [rax + 72]",
    "mov rdi, [rax + 8]",
    "mov rsi, [rax + 16]",
    "mov rdx, [rax + 24]",
    "mov rcx, [rax + 80]",
    "mov rax, [rsp + 8]",
    "sti",
    "vmrun rax",
    "cli",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "push rdi",
    "push rsi",
    "push rdx",
    "push rcx",
    "mov rax, [rsp + 80]",
    "mov rcx, [rsp + 72]",
    "mov [rax + 32], rcx",
    "mov rcx, [rsp + 64]",
    "mov [rax + 40], rcx",
    "mov rcx, [rsp + 56]",
    "mov [rax + 48], rcx",
    "mov rcx, [rsp + 48]",
    "mov [rax + 56], rcx",
    "mov rcx, [rsp + 40]",
    "mov [rax + 64], rcx",
    "mov rcx, [rsp + 32]",
    "mov [rax + 72], rcx",
    "mov rcx, [rsp + 24]",
    "mov [rax + 8], rcx",
    "mov rcx, [rsp + 16]",
    "mov [rax + 16], rcx",
    "mov rcx, [rsp + 8]",
    "mov [rax + 24], rcx",
    "mov rcx, [rsp]",
    "mov [rax + 80], rcx",
    "add rsp, 80",
    "add rsp, 16",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
);

unsafe extern "sysv64" {
    fn mboot_svm_enter(vmcb: u64, context: *mut SvmRunContext);
}

pub struct Svm {
    hsave_phys: u64,
    vmcb_phys: u64,
    guest_asid: u32,
    active: bool,
    owns_svm_operation: bool,
    started: bool,
    run_context: SvmRunContext,
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
        if !valid_guest_asid(1, features.address_space_ids) {
            return Err(Error::AddressSpaceIdUnavailable);
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
            guest_asid: 1,
            active: true,
            owns_svm_operation: true,
            started: false,
            run_context: SvmRunContext::default(),
        })
    }

    /// Creates another vCPU while SVM is already active on this CPU.
    ///
    /// # Safety
    /// `vmcb_phys` must be a writable, page-aligned physical page exclusively
    /// owned by mBoot. `guest_asid` must be unused by every other live Domain on
    /// this CPU. `self` must belong to the current logical CPU.
    pub unsafe fn create_vcpu(&self, vmcb_phys: u64, guest_asid: u32) -> Result<Self, Error> {
        validate_page(vmcb_phys)?;
        if !self.active {
            return Err(Error::InvalidState);
        }
        if !valid_guest_asid(guest_asid, cpu::detect().address_space_ids) {
            return Err(Error::AddressSpaceIdUnavailable);
        }
        // SAFETY: The new VMCB page is exclusively owned and writable.
        unsafe { write_bytes(vmcb_phys as *mut u8, 0, 4096) };
        Ok(Self {
            hsave_phys: self.hsave_phys,
            vmcb_phys,
            guest_asid,
            active: true,
            owns_svm_operation: false,
            started: false,
            run_context: SvmRunContext::default(),
        })
    }

    pub const fn hsave_phys(&self) -> u64 {
        self.hsave_phys
    }

    pub const fn vmcb_phys(&self) -> u64 {
        self.vmcb_phys
    }

    /// Runs a 64-bit guest until its first intercepted exit.
    ///
    /// # Safety
    /// `nested_root` and the VMCB must remain exclusively owned and physically
    /// accessible. The caller must execute on the CPU that called `enable`.
    pub unsafe fn run(&mut self, config: GuestConfig) -> Result<VmExit, Error> {
        validate_page(config.nested_root)?;
        validate_page(config.msr_permission_map)?;
        if !self.active {
            return Err(Error::InvalidState);
        }

        // SAFETY: `enable` established exclusive ownership of this mapped VMCB.
        unsafe { initialize_guest(self.vmcb_phys, self.guest_asid, config) };

        self.run_context = SvmRunContext {
            rdi: config.boot_info,
            ..SvmRunContext::default()
        };
        self.started = true;
        // SAFETY: The VMCB and initial register state were just initialized.
        unsafe { self.enter() }
    }

    /// Resumes the vCPU after a VMMCALL exit.
    ///
    /// # Safety
    /// The previous exit must have been the VMMCALL returned by `run`/`resume`.
    pub unsafe fn resume(&mut self, result: u64) -> Result<VmExit, Error> {
        if !self.active || !self.started {
            return Err(Error::InvalidState);
        }
        // SAFETY: The stopped vCPU owns the VMCB state-save area.
        let rip = unsafe { read_u64(self.vmcb_phys, VMCB_RIP) };
        // SAFETY: VMMCALL is three bytes and the next RIP remains in guest code.
        unsafe { write_u64(self.vmcb_phys, VMCB_RIP, rip + 3) };
        self.run_context.rax = result;
        // SAFETY: The VMCB still describes the stopped vCPU.
        unsafe { self.enter() }
    }

    pub unsafe fn resume_preempted(&mut self) -> Result<VmExit, Error> {
        if !self.active || !self.started {
            return Err(Error::InvalidState);
        }
        unsafe { self.enter() }
    }

    pub unsafe fn resume_msr_read(&mut self, value: u64) -> Result<VmExit, Error> {
        self.run_context.rax = u64::from(value as u32);
        self.run_context.rdx = u64::from((value >> 32) as u32);
        unsafe { self.resume_msr() }
    }

    pub unsafe fn resume_msr_write(&mut self) -> Result<VmExit, Error> {
        unsafe { self.resume_msr() }
    }

    unsafe fn resume_msr(&mut self) -> Result<VmExit, Error> {
        if !self.active || !self.started {
            return Err(Error::InvalidState);
        }
        let rip = unsafe { read_u64(self.vmcb_phys, VMCB_RIP) };
        // RDMSR and WRMSR are both two-byte instructions.
        unsafe { write_u64(self.vmcb_phys, VMCB_RIP, rip + 2) };
        unsafe { self.enter() }
    }

    pub unsafe fn inject_interrupt(&mut self, vector: u8) -> Result<(), Error> {
        if !self.active || !self.started {
            return Err(Error::InvalidState);
        }
        unsafe {
            write_u64(
                self.vmcb_phys,
                VMCB_INTERRUPT_CONTROL,
                V_INTR_MASKING | V_IRQ | V_INTR_PRIORITY | V_IGNORE_TPR,
            );
            write_u8(self.vmcb_phys, VMCB_INTERRUPT_VECTOR, vector);
        }
        Ok(())
    }

    pub unsafe fn can_inject_interrupt(&mut self) -> Result<bool, Error> {
        if !self.active || !self.started {
            return Err(Error::InvalidState);
        }
        // V_IRQ remains pending in the VMCB until IF, GIF and the interrupt
        // shadow permit delivery, so SVM can accept it immediately.
        Ok(true)
    }

    /// Requests an ASID translation flush before the next VMRUN.
    ///
    /// # Safety
    /// The associated vCPU must be stopped and the VMCB must remain writable.
    pub unsafe fn flush_nested(&mut self) -> Result<(), Error> {
        if !self.active || !self.started {
            return Err(Error::InvalidState);
        }
        unsafe { write_u8(self.vmcb_phys, VMCB_TLB_CONTROL, TLB_CONTROL_FLUSH_ALL) };
        Ok(())
    }

    unsafe fn enter(&mut self) -> Result<VmExit, Error> {
        // SAFETY: The stopped guest owns its VMCB RAX field.
        unsafe { write_u64(self.vmcb_phys, VMCB_RAX, self.run_context.rax) };
        super::timer::prepare_entry();
        // SAFETY: EFER.SVME and VM_HSAVE_PA are configured. The assembly bridge
        // preserves host callee-saved registers and captures guest registers.
        unsafe { mboot_svm_enter(self.vmcb_phys, &raw mut self.run_context) };

        // SAFETY: VMEXIT completed and the processor wrote the control area.
        let exit_code = unsafe { read_u64(self.vmcb_phys, VMCB_EXIT_CODE) };
        // SAFETY: The initial flush has completed; later mapping changes must
        // request another flush explicitly before entering this vCPU.
        unsafe { write_u8(self.vmcb_phys, VMCB_TLB_CONTROL, 0) };
        // SAFETY: VMEXIT saved guest RAX in the VMCB state area.
        self.run_context.rax = unsafe { read_u64(self.vmcb_phys, VMCB_RAX) };
        match exit_code {
            SVM_EXIT_INTR => {
                super::timer::acknowledge();
                Ok(VmExit {
                    reason: VmExitReason::Preempted,
                    raw_reason: exit_code,
                    hypercall_number: 0,
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    msr: 0,
                    msr_value: 0,
                })
            }
            SVM_EXIT_HLT => Ok(VmExit {
                reason: VmExitReason::Halt,
                raw_reason: exit_code,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
                msr: 0,
                msr_value: 0,
            }),
            SVM_EXIT_VMMCALL => Ok(VmExit {
                reason: VmExitReason::Hypercall,
                raw_reason: exit_code,
                hypercall_number: self.run_context.rax,
                arg0: self.run_context.rdi,
                arg1: self.run_context.rsi,
                arg2: self.run_context.rdx,
                msr: 0,
                msr_value: 0,
            }),
            SVM_EXIT_MSR => {
                let write = unsafe { read_u64(self.vmcb_phys, VMCB_EXIT_INFO1) } & 1 != 0;
                Ok(VmExit {
                    reason: if write {
                        VmExitReason::MsrWrite
                    } else {
                        VmExitReason::MsrRead
                    },
                    raw_reason: exit_code,
                    hypercall_number: 0,
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    msr: self.run_context.rcx as u32,
                    msr_value: u64::from(self.run_context.rax as u32)
                        | (u64::from(self.run_context.rdx as u32) << 32),
                })
            }
            _ => Err(Error::UnexpectedVmExit(exit_code)),
        }
    }

    /// Disables SVM on the current logical CPU.
    ///
    /// # Safety
    /// Must run on the same logical CPU that called `enable`, with no guest
    /// executing and no code relying on the configured host-save area.
    pub unsafe fn disable(&mut self) {
        if !self.active {
            return;
        }
        if self.owns_svm_operation {
            // SAFETY: The caller guarantees the enabling CPU and no active guest.
            unsafe { write_msr(EFER, read_msr(EFER) & !EFER_SVME) };
        }
        self.active = false;
    }
}

unsafe fn initialize_guest(vmcb: u64, guest_asid: u32, config: GuestConfig) {
    // SAFETY: The caller owns the complete VMCB page.
    unsafe {
        write_bytes(vmcb as *mut u8, 0, 4096);
        write_u32(
            vmcb,
            VMCB_INTERCEPT_MISC1,
            INTERCEPT_INTR | INTERCEPT_HLT | INTERCEPT_MSR_PROT,
        );
        // VMRUN must never recurse into a guest-provided VMCB. AMD defines this
        // as a mandatory intercept for a valid first-level guest.
        write_u32(
            vmcb,
            VMCB_INTERCEPT_MISC2,
            INTERCEPT_VMRUN | INTERCEPT_VMMCALL,
        );
        write_u32(vmcb, VMCB_GUEST_ASID, guest_asid);
        write_u64(vmcb, VMCB_MSRPM_BASE_PA, config.msr_permission_map);
        write_u8(vmcb, VMCB_TLB_CONTROL, TLB_CONTROL_FLUSH_ALL);
        // Physical interrupts are governed by host RFLAGS.IF while the guest is
        // running. The guest starts with IF clear until it installs its own IDT.
        write_u64(vmcb, VMCB_INTERRUPT_CONTROL, V_INTR_MASKING);
        write_u64(vmcb, VMCB_NP_ENABLE, 1);
        write_u64(vmcb, VMCB_NCR3, config.nested_root);

        for offset in [VMCB_ES, VMCB_SS, VMCB_DS, VMCB_FS, VMCB_GS] {
            write_segment(vmcb, offset, 0x10, SEGMENT_DATA_LONG_MODE, 0xffff_ffff, 0);
        }
        write_segment(vmcb, VMCB_CS, 0x08, SEGMENT_CODE_LONG_MODE, 0xffff_ffff, 0);
        write_segment(vmcb, VMCB_GDTR, 0, 0, 0, 0);
        write_segment(vmcb, VMCB_IDTR, 0, 0, 0, 0);
        write_segment(vmcb, VMCB_LDTR, 0, 0, 0, 0);
        write_segment(vmcb, VMCB_TR, 0x18, 0x008b, 0x67, 0);

        // SVME remains set in guest EFER while SVM is active. LME and LMA start
        // the Domain directly in 64-bit mode.
        write_u64(
            vmcb,
            VMCB_EFER,
            EFER_SVME | (1 << 8) | (1 << 10) | (1 << 11),
        );
        write_u64(vmcb, VMCB_CR4, 1 << 5);
        write_u64(vmcb, VMCB_CR3, config.page_table_root);
        write_u64(vmcb, VMCB_CR0, 0x8001_0033);
        write_u64(vmcb, VMCB_DR7, 0x400);
        write_u64(vmcb, VMCB_DR6, 0xffff_0ff0);
        write_u64(vmcb, VMCB_RFLAGS, 2);
        write_u64(vmcb, VMCB_RIP, config.entry);
        write_u64(vmcb, VMCB_RSP, config.stack);
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

unsafe fn write_u8(base: u64, offset: usize, value: u8) {
    // SAFETY: The caller provides an in-bounds VMCB field.
    unsafe { ((base as *mut u8).add(offset)).write_volatile(value) };
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

const fn valid_guest_asid(asid: u32, address_space_ids: u32) -> bool {
    asid != 0 && asid < address_space_ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svm_control_pages_must_be_aligned() {
        assert_eq!(validate_page(0x1234), Err(Error::InvalidPage));
        assert_eq!(validate_page(0x4000), Ok(()));
    }

    #[test]
    fn assembly_context_offsets_include_guest_rcx() {
        assert_eq!(core::mem::offset_of!(SvmRunContext, rcx), 80);
        assert_eq!(core::mem::size_of::<SvmRunContext>(), 88);
    }

    #[test]
    fn svm_guest_asids_exclude_zero_and_the_reported_limit() {
        assert!(!valid_guest_asid(0, 16));
        assert!(valid_guest_asid(1, 16));
        assert!(valid_guest_asid(15, 16));
        assert!(!valid_guest_asid(16, 16));
    }
}
