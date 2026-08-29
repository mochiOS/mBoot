use core::arch::{asm, global_asm};
use core::ptr::write_bytes;

use crate::arch::x86_64::{
    descriptor, read_cr0, read_cr3, read_cr4, read_msr, write_cr0, write_cr4, write_msr,
};
use crate::{
    arch::x86_64::cpu, BackendKind, CpuidResult, Error, GuestBootMode, GuestConfig, VmExit,
    VmExitReason,
};

const IA32_FEATURE_CONTROL: u32 = 0x3a;
const IA32_VMX_BASIC: u32 = 0x480;
const IA32_VMX_PROCBASED_CTLS: u32 = 0x482;
const IA32_VMX_EXIT_CTLS: u32 = 0x483;
const IA32_VMX_ENTRY_CTLS: u32 = 0x484;
const IA32_VMX_CR0_FIXED0: u32 = 0x486;
const IA32_VMX_CR0_FIXED1: u32 = 0x487;
const IA32_VMX_CR4_FIXED0: u32 = 0x488;
const IA32_VMX_CR4_FIXED1: u32 = 0x489;
const IA32_VMX_PROCBASED_CTLS2: u32 = 0x48b;
const IA32_VMX_EPT_VPID_CAP: u32 = 0x48c;
const IA32_VMX_TRUE_PINBASED_CTLS: u32 = 0x48d;
const IA32_VMX_TRUE_PROCBASED_CTLS: u32 = 0x48e;
const IA32_VMX_TRUE_EXIT_CTLS: u32 = 0x48f;
const IA32_VMX_TRUE_ENTRY_CTLS: u32 = 0x490;
const IA32_FS_BASE: u32 = 0xc000_0100;
const IA32_GS_BASE: u32 = 0xc000_0101;
const IA32_SYSENTER_CS: u32 = 0x174;
const IA32_SYSENTER_ESP: u32 = 0x175;
const IA32_SYSENTER_EIP: u32 = 0x176;
const IA32_EFER: u32 = 0xc000_0080;

const FEATURE_CONTROL_LOCK: u64 = 1 << 0;
const FEATURE_CONTROL_VMX_OUTSIDE_SMX: u64 = 1 << 2;
const CR4_VMXE: u64 = 1 << 13;
const CR0_PAGING: u64 = 1 << 31;
const EFER_SCE: u64 = 1 << 0;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;
const EFER_NXE: u64 = 1 << 11;

const PIN_BASED_CONTROLS: u64 = 0x4000;
const PRIMARY_CONTROLS: u64 = 0x4002;
const EXCEPTION_BITMAP: u64 = 0x4004;
const PAGE_FAULT_ERROR_MASK: u64 = 0x4006;
const PAGE_FAULT_ERROR_MATCH: u64 = 0x4008;
const CR3_TARGET_COUNT: u64 = 0x400a;
const EXIT_CONTROLS: u64 = 0x400c;
const EXIT_MSR_STORE_COUNT: u64 = 0x400e;
const EXIT_MSR_LOAD_COUNT: u64 = 0x4010;
const ENTRY_CONTROLS: u64 = 0x4012;
const ENTRY_MSR_LOAD_COUNT: u64 = 0x4014;
const ENTRY_INTERRUPTION_INFO: u64 = 0x4016;
const ENTRY_EXCEPTION_ERROR_CODE: u64 = 0x4018;
const SECONDARY_CONTROLS: u64 = 0x401e;
const CR0_GUEST_HOST_MASK: u64 = 0x6000;
const CR4_GUEST_HOST_MASK: u64 = 0x6002;
const CR0_READ_SHADOW: u64 = 0x6004;
const CR4_READ_SHADOW: u64 = 0x6006;
const MSR_BITMAP: u64 = 0x2004;
const EXIT_MSR_STORE_ADDRESS: u64 = 0x2006;
const EXIT_MSR_LOAD_ADDRESS: u64 = 0x2008;
const ENTRY_MSR_LOAD_ADDRESS: u64 = 0x200a;
const EPT_POINTER: u64 = 0x201a;
const GUEST_PHYSICAL_ADDRESS: u64 = 0x2400;
const VMCS_LINK_POINTER: u64 = 0x2800;
const GUEST_DEBUGCTL: u64 = 0x2802;
const GUEST_PAT: u64 = 0x2804;
const GUEST_EFER: u64 = 0x2806;
const HOST_PAT: u64 = 0x2c00;
const HOST_EFER: u64 = 0x2c02;
const GUEST_ES_SELECTOR: u64 = 0x0800;
const GUEST_CS_SELECTOR: u64 = 0x0802;
const GUEST_SS_SELECTOR: u64 = 0x0804;
const GUEST_DS_SELECTOR: u64 = 0x0806;
const GUEST_FS_SELECTOR: u64 = 0x0808;
const GUEST_GS_SELECTOR: u64 = 0x080a;
const GUEST_LDTR_SELECTOR: u64 = 0x080c;
const GUEST_TR_SELECTOR: u64 = 0x080e;
const HOST_ES_SELECTOR: u64 = 0x0c00;
const HOST_CS_SELECTOR: u64 = 0x0c02;
const HOST_SS_SELECTOR: u64 = 0x0c04;
const HOST_DS_SELECTOR: u64 = 0x0c06;
const HOST_FS_SELECTOR: u64 = 0x0c08;
const HOST_GS_SELECTOR: u64 = 0x0c0a;
const HOST_TR_SELECTOR: u64 = 0x0c0c;
const GUEST_ES_LIMIT: u64 = 0x4800;
const GUEST_CS_LIMIT: u64 = 0x4802;
const GUEST_SS_LIMIT: u64 = 0x4804;
const GUEST_DS_LIMIT: u64 = 0x4806;
const GUEST_FS_LIMIT: u64 = 0x4808;
const GUEST_GS_LIMIT: u64 = 0x480a;
const GUEST_LDTR_LIMIT: u64 = 0x480c;
const GUEST_TR_LIMIT: u64 = 0x480e;
const GUEST_GDTR_LIMIT: u64 = 0x4810;
const GUEST_IDTR_LIMIT: u64 = 0x4812;
const GUEST_ES_AR: u64 = 0x4814;
const GUEST_CS_AR: u64 = 0x4816;
const GUEST_SS_AR: u64 = 0x4818;
const GUEST_DS_AR: u64 = 0x481a;
const GUEST_FS_AR: u64 = 0x481c;
const GUEST_GS_AR: u64 = 0x481e;
const GUEST_LDTR_AR: u64 = 0x4820;
const GUEST_TR_AR: u64 = 0x4822;
const GUEST_INTERRUPTIBILITY: u64 = 0x4824;
const GUEST_ACTIVITY_STATE: u64 = 0x4826;
const GUEST_SYSENTER_CS: u64 = 0x482a;
const HOST_SYSENTER_CS: u64 = 0x4c00;
const GUEST_CR0: u64 = 0x6800;
const GUEST_CR3: u64 = 0x6802;
const GUEST_CR4: u64 = 0x6804;
const GUEST_ES_BASE: u64 = 0x6806;
const GUEST_CS_BASE: u64 = 0x6808;
const GUEST_SS_BASE: u64 = 0x680a;
const GUEST_DS_BASE: u64 = 0x680c;
const GUEST_FS_BASE: u64 = 0x680e;
const GUEST_GS_BASE: u64 = 0x6810;
const GUEST_LDTR_BASE: u64 = 0x6812;
const GUEST_TR_BASE: u64 = 0x6814;
const GUEST_GDTR_BASE: u64 = 0x6816;
const GUEST_IDTR_BASE: u64 = 0x6818;
const GUEST_DR7: u64 = 0x681a;
const GUEST_RSP: u64 = 0x681c;
const GUEST_RIP: u64 = 0x681e;
const GUEST_RFLAGS: u64 = 0x6820;
const GUEST_SYSENTER_ESP: u64 = 0x6824;
const GUEST_SYSENTER_EIP: u64 = 0x6826;
const HOST_CR0: u64 = 0x6c00;
const HOST_CR3: u64 = 0x6c02;
const HOST_CR4: u64 = 0x6c04;
const HOST_FS_BASE: u64 = 0x6c06;
const HOST_GS_BASE: u64 = 0x6c08;
const HOST_TR_BASE: u64 = 0x6c0a;
const HOST_GDTR_BASE: u64 = 0x6c0c;
const HOST_IDTR_BASE: u64 = 0x6c0e;
const HOST_SYSENTER_ESP: u64 = 0x6c10;
const HOST_SYSENTER_EIP: u64 = 0x6c12;
const EXIT_REASON: u64 = 0x4402;
const EXIT_QUALIFICATION: u64 = 0x6400;
const VM_INSTRUCTION_ERROR: u64 = 0x4400;
const EXIT_INSTRUCTION_LENGTH: u64 = 0x440c;
const EXIT_INTERRUPTION_INFO: u64 = 0x4404;
const EXTERNAL_INTERRUPT_EXIT_REASON: u64 = 1;
const INTERRUPT_WINDOW_EXIT_REASON: u64 = 7;
const CPUID_EXIT_REASON: u64 = 10;
const HLT_EXIT_REASON: u64 = 12;
const VMCALL_EXIT_REASON: u64 = 18;
const RDMSR_EXIT_REASON: u64 = 31;
const WRMSR_EXIT_REASON: u64 = 32;
const EPT_VIOLATION_EXIT_REASON: u64 = 48;
const IA32_PAT: u32 = 0x277;
const GUEST_MSR_LIST: [u32; 6] = [
    0xc000_0081,
    0xc000_0082,
    0xc000_0083,
    0xc000_0084,
    0xc000_0102,
    0xc000_0103,
];

#[derive(Clone, Copy)]
#[repr(C)]
struct VmEntryMsr {
    index: u32,
    reserved: u32,
    value: u64,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct VmxRunContext {
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
    resume: u64,
    rcx: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
}

global_asm!(
    ".global mboot_vmx_launch",
    "mboot_vmx_launch:",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "push rdi",
    "mov rax, rsp",
    "mov rcx, 0x6c14",
    "vmwrite rcx, rax",
    "lea rax, [rip + mboot_vmx_exit]",
    "mov rcx, 0x6c16",
    "vmwrite rcx, rax",
    "mov rax, [rsp]",
    "cmp qword ptr [rax + 80], 0",
    "jne mboot_vmx_resume_guest",
    "mov rbx, [rax + 32]",
    "mov rbp, [rax + 40]",
    "mov r12, [rax + 48]",
    "mov r13, [rax + 56]",
    "mov r14, [rax + 64]",
    "mov r15, [rax + 72]",
    "mov rdi, [rax + 8]",
    "mov rsi, [rax + 16]",
    "mov rdx, [rax + 24]",
    "mov rcx, [rax + 88]",
    "mov r8, [rax + 96]",
    "mov r9, [rax + 104]",
    "mov r10, [rax + 112]",
    "mov r11, [rax + 120]",
    "mov rax, [rax]",
    "sti",
    "vmlaunch",
    "cli",
    "jmp mboot_vmx_entry_failed",
    "mboot_vmx_resume_guest:",
    "mov rbx, [rax + 32]",
    "mov rbp, [rax + 40]",
    "mov r12, [rax + 48]",
    "mov r13, [rax + 56]",
    "mov r14, [rax + 64]",
    "mov r15, [rax + 72]",
    "mov rdi, [rax + 8]",
    "mov rsi, [rax + 16]",
    "mov rdx, [rax + 24]",
    "mov rcx, [rax + 88]",
    "mov r8, [rax + 96]",
    "mov r9, [rax + 104]",
    "mov r10, [rax + 112]",
    "mov r11, [rax + 120]",
    "mov rax, [rax]",
    "sti",
    "vmresume",
    "cli",
    "mboot_vmx_entry_failed:",
    "mov eax, 1",
    "jmp mboot_vmx_return",
    "mboot_vmx_exit:",
    "cli",
    "push rax",
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
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "mov rcx, [rsp + 120]",
    "mov rax, [rsp + 112]",
    "mov [rcx], rax",
    "mov rax, [rsp + 56]",
    "mov [rcx + 8], rax",
    "mov rax, [rsp + 48]",
    "mov [rcx + 16], rax",
    "mov rax, [rsp + 40]",
    "mov [rcx + 24], rax",
    "mov rax, [rsp + 104]",
    "mov [rcx + 32], rax",
    "mov rax, [rsp + 96]",
    "mov [rcx + 40], rax",
    "mov rax, [rsp + 88]",
    "mov [rcx + 48], rax",
    "mov rax, [rsp + 80]",
    "mov [rcx + 56], rax",
    "mov rax, [rsp + 72]",
    "mov [rcx + 64], rax",
    "mov rax, [rsp + 64]",
    "mov [rcx + 72], rax",
    "mov rax, [rsp + 32]",
    "mov [rcx + 88], rax",
    "mov rax, [rsp + 24]",
    "mov [rcx + 96], rax",
    "mov rax, [rsp + 16]",
    "mov [rcx + 104], rax",
    "mov rax, [rsp + 8]",
    "mov [rcx + 112], rax",
    "mov rax, [rsp]",
    "mov [rcx + 120], rax",
    "add rsp, 120",
    "xor eax, eax",
    "mboot_vmx_return:",
    "add rsp, 8",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
);

unsafe extern "sysv64" {
    fn mboot_vmx_launch(context: *mut VmxRunContext) -> u32;
}

pub struct Vmx {
    revision_id: u32,
    vmcs_phys: u64,
    active: bool,
    owns_vmx_operation: bool,
    run_context: VmxRunContext,
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
        if !control_region_address_valid(basic, vmxon_phys)
            || !control_region_address_valid(basic, vmcs_phys)
        {
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
            owns_vmx_operation: true,
            run_context: VmxRunContext::default(),
        })
    }

    /// Creates another vCPU while VMX operation is already active.
    ///
    /// # Safety
    /// `vmcs_phys` must be a writable, page-aligned physical page exclusively
    /// owned by mBoot. `self` must belong to the current logical CPU.
    pub unsafe fn create_vcpu(
        &self,
        vmcs_phys: u64,
        _address_space_id: u32,
    ) -> Result<Self, Error> {
        validate_page(vmcs_phys)?;
        if !self.active {
            return Err(Error::InvalidState);
        }
        // SAFETY: VMX operation being active makes IA32_VMX_BASIC available.
        let basic = unsafe { read_msr(IA32_VMX_BASIC) };
        if !control_region_address_valid(basic, vmcs_phys) {
            return Err(Error::InvalidPage);
        }
        // SAFETY: VMX operation is active and this page has exclusive ownership.
        unsafe {
            initialize_control_region(vmcs_phys, self.revision_id);
            vmclear(vmcs_phys).map_err(|()| Error::ControlInstructionFailed)?;
        }
        Ok(Self {
            revision_id: self.revision_id,
            vmcs_phys,
            active: true,
            owns_vmx_operation: false,
            run_context: VmxRunContext::default(),
        })
    }

    pub const fn revision_id(&self) -> u32 {
        self.revision_id
    }

    pub const fn vmcs_phys(&self) -> u64 {
        self.vmcs_phys
    }

    pub unsafe fn reset(&mut self) -> Result<(), Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmclear(self.vmcs_phys).map_err(|()| Error::ControlInstructionFailed)? };
        self.run_context = VmxRunContext::default();
        Ok(())
    }

    /// Runs a 64-bit guest until its first intercepted exit.
    ///
    /// # Safety
    /// `ept_pointer` must name a live EPT owned by mBoot. The current VMCS and
    /// descriptor tables must remain installed on the CPU that called `enable`.
    pub unsafe fn run(&mut self, config: GuestConfig) -> Result<VmExit, Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        // SAFETY: This vCPU owns the VMCS and VMX operation is active.
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        // SAFETY: VMX is active and all VMCS host/guest values are supplied here.
        unsafe { initialize_vmcs(config)? };
        self.run_context = VmxRunContext {
            rax: 0,
            rdi: if config.boot_mode == GuestBootMode::Long64 {
                config.boot_info
            } else {
                0
            },
            rbx: if config.boot_mode == GuestBootMode::LinuxPvh32 {
                config.boot_info
            } else {
                0
            },
            ..VmxRunContext::default()
        };
        super::timer::prepare_entry();
        unsafe { synchronize_guest_long_mode()? };
        // SAFETY: The VMCS host RIP/RSP target the assembly return trampoline.
        if unsafe { mboot_vmx_launch(&raw mut self.run_context) } != 0 {
            // SAFETY: VMfailValid leaves the current VMCS readable. A zero value
            // is retained if the processor reported VMfailInvalid instead.
            return Err(Error::GuestEntryFailed(unsafe {
                vmread(VM_INSTRUCTION_ERROR)
            }));
        }
        // SAFETY: VMLAUNCH returned only through a VM exit.
        unsafe { self.decode_exit() }
    }

    /// Resumes the vCPU after a VMCALL exit.
    ///
    /// # Safety
    /// The previous exit must have been the VMCALL returned by `run`/`resume`.
    pub unsafe fn resume(&mut self, result: u64) -> Result<VmExit, Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        // SAFETY: This reloads the stopped VMCS after another vCPU may have run.
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        // SAFETY: VMEXIT left a current, stopped VMCS with readable exit fields.
        let (rip, instruction_len) =
            unsafe { (vmread(GUEST_RIP), vmread(EXIT_INSTRUCTION_LENGTH)) };
        // SAFETY: The stopped VMCS accepts the next guest RIP.
        unsafe { vmwrite(GUEST_RIP, rip + instruction_len)? };
        self.run_context.rax = result;
        self.run_context.resume = 1;
        super::timer::prepare_entry();
        unsafe { synchronize_guest_long_mode()? };
        // SAFETY: The VMCS and captured register state belong to this stopped vCPU.
        if unsafe { mboot_vmx_launch(&raw mut self.run_context) } != 0 {
            // SAFETY: VMfailValid leaves the current VMCS readable. A zero value
            // is retained if the processor reported VMfailInvalid instead.
            return Err(Error::GuestEntryFailed(unsafe {
                vmread(VM_INSTRUCTION_ERROR)
            }));
        }
        // SAFETY: VMRESUME returned only through a VM exit.
        unsafe { self.decode_exit() }
    }

    pub unsafe fn resume_preempted(&mut self) -> Result<VmExit, Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        self.run_context.resume = 1;
        super::timer::prepare_entry();
        unsafe { synchronize_guest_long_mode()? };
        if unsafe { mboot_vmx_launch(&raw mut self.run_context) } != 0 {
            return Err(Error::GuestEntryFailed(unsafe {
                vmread(VM_INSTRUCTION_ERROR)
            }));
        }
        unsafe { self.decode_exit() }
    }

    pub unsafe fn resume_halted(&mut self) -> Result<VmExit, Error> {
        unsafe { self.resume_instruction() }
    }

    pub unsafe fn resume_msr_read(&mut self, value: u64) -> Result<VmExit, Error> {
        self.run_context.rax = u64::from(value as u32);
        self.run_context.rdx = u64::from((value >> 32) as u32);
        unsafe { self.resume_instruction() }
    }

    pub unsafe fn resume_msr_write(&mut self) -> Result<VmExit, Error> {
        unsafe { self.resume_instruction() }
    }

    pub unsafe fn resume_cpuid(&mut self, result: CpuidResult) -> Result<VmExit, Error> {
        self.run_context.rax = u64::from(result.eax);
        self.run_context.rbx = u64::from(result.ebx);
        self.run_context.rcx = u64::from(result.ecx);
        self.run_context.rdx = u64::from(result.edx);
        unsafe { self.resume_instruction() }
    }

    unsafe fn resume_instruction(&mut self) -> Result<VmExit, Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        let (rip, instruction_len) =
            unsafe { (vmread(GUEST_RIP), vmread(EXIT_INSTRUCTION_LENGTH)) };
        unsafe { vmwrite(GUEST_RIP, rip + instruction_len)? };
        self.run_context.resume = 1;
        super::timer::prepare_entry();
        unsafe { synchronize_guest_long_mode()? };
        if unsafe { mboot_vmx_launch(&raw mut self.run_context) } != 0 {
            return Err(Error::GuestEntryFailed(unsafe {
                vmread(VM_INSTRUCTION_ERROR)
            }));
        }
        unsafe { self.decode_exit() }
    }

    pub unsafe fn inject_interrupt(&mut self, vector: u8) -> Result<(), Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe {
            vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)?;
            vmwrite(ENTRY_INTERRUPTION_INFO, (1 << 31) | u64::from(vector))
        }
    }

    pub unsafe fn inject_general_protection(&mut self) -> Result<(), Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe {
            vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)?;
            vmwrite(ENTRY_EXCEPTION_ERROR_CODE, 0)?;
            vmwrite(
                ENTRY_INTERRUPTION_INFO,
                (1 << 31) | (1 << 11) | (3 << 8) | 13,
            )
        }
    }

    pub unsafe fn can_inject_interrupt(&mut self) -> Result<bool, Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        let flags = unsafe { vmread(GUEST_RFLAGS) };
        let interruptibility = unsafe { vmread(GUEST_INTERRUPTIBILITY) };
        Ok(flags & (1 << 9) != 0 && interruptibility & 0b11 == 0)
    }

    pub unsafe fn set_interrupt_window(&mut self, enabled: bool) -> Result<(), Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        let mut controls = unsafe { vmread(PRIMARY_CONTROLS) };
        if enabled {
            controls |= 1 << 2;
        } else {
            controls &= !(1 << 2);
        }
        unsafe { vmwrite(PRIMARY_CONTROLS, controls) }
    }

    /// Invalidates cached translations for one EPT hierarchy.
    ///
    /// # Safety
    /// The caller must run on the VMX-enabled CPU while no vCPU using this EPT
    /// is executing.
    pub unsafe fn flush_nested(&self, ept_pointer: u64) -> Result<(), Error> {
        #[repr(C)]
        struct InveptDescriptor {
            ept_pointer: u64,
            reserved: u64,
        }
        let descriptor = InveptDescriptor {
            ept_pointer,
            reserved: 0,
        };
        let kind = 1_u64;
        let mut failed: u8;
        unsafe {
            asm!(
                "invept {kind}, [{descriptor}]",
                "setna {failed}",
                kind = in(reg) kind,
                descriptor = in(reg) &descriptor,
                failed = lateout(reg_byte) failed,
                options(nostack)
            )
        };
        if failed == 0 {
            Ok(())
        } else {
            Err(Error::ControlInstructionFailed)
        }
    }

    /// Reads the instruction pointer from a stopped guest.
    ///
    /// # Safety
    /// The vCPU must have entered at least once, be stopped, and remain owned
    /// by the current VMX-enabled CPU.
    pub unsafe fn guest_instruction_pointer(&self) -> Result<u64, Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        Ok(unsafe { vmread(GUEST_RIP) })
    }

    /// Reads the stack pointer from a stopped guest for crash reporting.
    ///
    /// # Safety
    /// The vCPU must have entered at least once, be stopped, and remain owned
    /// by the current VMX-enabled CPU.
    pub unsafe fn guest_stack_pointer(&self) -> Result<u64, Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        Ok(unsafe { vmread(GUEST_RSP) })
    }

    /// Reads CR0 and CR3 from a stopped guest for early paging diagnostics.
    ///
    /// # Safety
    /// The vCPU must have entered at least once, be stopped, and remain owned
    /// by the current VMX-enabled CPU.
    pub unsafe fn guest_paging_state(&self) -> Result<(u64, u64), Error> {
        if !self.active {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        Ok(unsafe { (vmread(GUEST_CR0), vmread(GUEST_CR3)) })
    }

    /// Emulates an intercepted architectural MSR read from stopped guest state.
    ///
    /// # Safety
    /// The vCPU must be stopped and remain owned by the current VMX-enabled CPU.
    pub unsafe fn read_guest_msr(&self, msr: u32) -> Result<u64, Error> {
        if !self.active || msr != IA32_EFER {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        Ok(unsafe { vmread(GUEST_EFER) })
    }

    /// Emulates an intercepted architectural MSR write into stopped guest state.
    ///
    /// # Safety
    /// The vCPU must be stopped and remain owned by the current VMX-enabled CPU.
    pub unsafe fn write_guest_msr(&mut self, msr: u32, value: u64) -> Result<(), Error> {
        if !self.active || msr != IA32_EFER {
            return Err(Error::InvalidState);
        }
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::VmcsLoadFailed)? };
        let current = unsafe { vmread(GUEST_EFER) };
        let cr0 = unsafe { vmread(GUEST_CR0) };
        let updated = updated_guest_efer(current, cr0, value).ok_or(Error::InvalidState)?;
        unsafe { vmwrite(GUEST_EFER, updated) }
    }

    unsafe fn decode_exit(&self) -> Result<VmExit, Error> {
        // SAFETY: A VM exit returned through the configured host trampoline.
        let reason = unsafe { vmread(EXIT_REASON) } & 0xffff;
        match reason {
            INTERRUPT_WINDOW_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::InterruptWindow,
                raw_reason: reason,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
                msr: 0,
                msr_value: 0,
                cpuid_leaf: 0,
                cpuid_subleaf: 0,
                fault_address: 0,
                fault_info: 0,
            }),
            EXTERNAL_INTERRUPT_EXIT_REASON => {
                let info = unsafe { vmread(EXIT_INTERRUPTION_INFO) };
                if info & (1 << 31) != 0 && info & (7 << 8) == 0 {
                    Ok(VmExit {
                        reason: VmExitReason::Preempted,
                        raw_reason: reason,
                        hypercall_number: 0,
                        arg0: 0,
                        arg1: 0,
                        arg2: 0,
                        msr: 0,
                        msr_value: 0,
                        cpuid_leaf: 0,
                        cpuid_subleaf: 0,
                        fault_address: 0,
                        fault_info: info,
                    })
                } else {
                    Err(Error::UnexpectedVmExit(reason))
                }
            }
            HLT_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::Halt,
                raw_reason: reason,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
                msr: 0,
                msr_value: 0,
                cpuid_leaf: 0,
                cpuid_subleaf: 0,
                fault_address: 0,
                fault_info: 0,
            }),
            VMCALL_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::Hypercall,
                raw_reason: reason,
                hypercall_number: self.run_context.rax,
                arg0: self.run_context.rdi,
                arg1: self.run_context.rsi,
                arg2: self.run_context.rdx,
                msr: 0,
                msr_value: 0,
                cpuid_leaf: 0,
                cpuid_subleaf: 0,
                fault_address: 0,
                fault_info: 0,
            }),
            RDMSR_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::MsrRead,
                raw_reason: reason,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
                msr: self.run_context.rcx as u32,
                msr_value: 0,
                cpuid_leaf: 0,
                cpuid_subleaf: 0,
                fault_address: 0,
                fault_info: 0,
            }),
            WRMSR_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::MsrWrite,
                raw_reason: reason,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
                msr: self.run_context.rcx as u32,
                msr_value: u64::from(self.run_context.rax as u32)
                    | (u64::from(self.run_context.rdx as u32) << 32),
                cpuid_leaf: 0,
                cpuid_subleaf: 0,
                fault_address: 0,
                fault_info: 0,
            }),
            CPUID_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::Cpuid,
                raw_reason: reason,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
                msr: 0,
                msr_value: 0,
                cpuid_leaf: self.run_context.rax as u32,
                cpuid_subleaf: self.run_context.rcx as u32,
                fault_address: 0,
                fault_info: 0,
            }),
            EPT_VIOLATION_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::NestedPageFault,
                raw_reason: reason,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
                msr: 0,
                msr_value: 0,
                cpuid_leaf: 0,
                cpuid_subleaf: 0,
                fault_address: unsafe { vmread(GUEST_PHYSICAL_ADDRESS) },
                fault_info: unsafe { vmread(EXIT_QUALIFICATION) },
            }),
            _ => Err(Error::UnexpectedVmExit(reason)),
        }
    }

    /// Leaves VMX operation on the current logical CPU.
    ///
    /// # Safety
    /// Must run on the same logical CPU that called `enable`, with no vCPU
    /// currently executing and no code relying on the current VMCS.
    pub unsafe fn disable(&mut self) {
        if !self.active {
            return;
        }
        if self.owns_vmx_operation {
            // SAFETY: The caller guarantees the enabling CPU and no active vCPU.
            unsafe {
                vmxoff();
                write_cr4(read_cr4() & !CR4_VMXE);
            }
        }
        self.active = false;
    }
}

fn validate_page(phys: u64) -> Result<(), Error> {
    if phys == 0 || phys & 0xfff != 0 {
        Err(Error::InvalidPage)
    } else {
        Ok(())
    }
}

fn control_region_address_valid(vmx_basic: u64, phys: u64) -> bool {
    let restricted_to_32_bits = vmx_basic & (1 << 48) != 0;
    !restricted_to_32_bits || phys <= u32::MAX as u64
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
    let invept = ept & (1 << 20) != 0;
    let single_context_invept = ept & (1 << 25) != 0;
    secondary_controls
        && ept_control
        && four_level_walk
        && write_back
        && invept
        && single_context_invept
}

unsafe fn initialize_vmcs(config: GuestConfig) -> Result<(), Error> {
    // SAFETY: All MSRs are architectural VMX capability and host-state MSRs.
    let basic = unsafe { read_msr(IA32_VMX_BASIC) };
    let true_controls = basic & (1 << 55) != 0;
    let pin_msr = if true_controls {
        IA32_VMX_TRUE_PINBASED_CTLS
    } else {
        0x481
    };
    let primary_msr = if true_controls {
        IA32_VMX_TRUE_PROCBASED_CTLS
    } else {
        IA32_VMX_PROCBASED_CTLS
    };
    let exit_msr = if true_controls {
        IA32_VMX_TRUE_EXIT_CTLS
    } else {
        IA32_VMX_EXIT_CTLS
    };
    let entry_msr = if true_controls {
        IA32_VMX_TRUE_ENTRY_CTLS
    } else {
        IA32_VMX_ENTRY_CTLS
    };
    if unsafe { read_msr(primary_msr) } >> 32 & (1 << 2) == 0 {
        return Err(Error::InterruptVirtualizationUnavailable);
    }
    // SAFETY: Capability MSRs are available after VMX CPUID detection.
    let pvh = config.boot_mode == GuestBootMode::LinuxPvh32;
    let (pin, primary, secondary, exit, entry) = unsafe {
        (
            adjusted_vm_control(1, read_msr(pin_msr)),
            adjusted_vm_control(
                (1 << 7) | (1 << 28) | (1 << 31),
                read_msr(primary_msr),
            ),
            adjusted_vm_control(
                (1 << 1) | if pvh { 1 << 7 } else { 0 },
                read_msr(IA32_VMX_PROCBASED_CTLS2),
            ),
            adjusted_vm_control(
                (1 << 9)
                    | (1 << 15)
                    | (1 << 20)
                    | (1 << 21)
                    | if pvh { (1 << 18) | (1 << 19) } else { 0 },
                read_msr(exit_msr),
            ),
            adjusted_vm_control(
                (if pvh { 1 << 14 } else { 1 << 9 }) | (1 << 15),
                read_msr(entry_msr),
            ),
        )
    };
    if secondary & (1 << 1) == 0 {
        return Err(Error::NestedPagingUnavailable);
    }
    if primary & (1 << 28) == 0 {
        return Err(Error::UnsupportedCpu);
    }
    if pvh && secondary & (1 << 7) == 0 {
        return Err(Error::UnsupportedCpu);
    }
    if pin & 1 == 0 || exit & (1 << 15) == 0 {
        return Err(Error::InterruptVirtualizationUnavailable);
    }

    for (field, value) in [
        (PIN_BASED_CONTROLS, pin),
        (PRIMARY_CONTROLS, primary),
        (SECONDARY_CONTROLS, secondary),
        (EXIT_CONTROLS, exit),
        (ENTRY_CONTROLS, entry),
        (EXCEPTION_BITMAP, 0),
        (PAGE_FAULT_ERROR_MASK, 0),
        (PAGE_FAULT_ERROR_MATCH, 0),
        (CR3_TARGET_COUNT, 0),
        (EXIT_MSR_STORE_COUNT, GUEST_MSR_LIST.len() as u64),
        (EXIT_MSR_LOAD_COUNT, GUEST_MSR_LIST.len() as u64),
        (ENTRY_MSR_LOAD_COUNT, GUEST_MSR_LIST.len() as u64),
        (ENTRY_INTERRUPTION_INFO, 0),
    ] {
        // SAFETY: Each field is a writable control field of the current VMCS.
        unsafe { vmwrite(field, value)? };
    }
    if primary & (1 << 28) != 0 {
        validate_page(config.msr_permission_map)?;
        unsafe { vmwrite(MSR_BITMAP, config.msr_permission_map)? };
    }
    unsafe { initialize_guest_msr_lists(config.msr_state_page)? };
    let host_list =
        config.msr_state_page + (GUEST_MSR_LIST.len() * core::mem::size_of::<VmEntryMsr>()) as u64;
    unsafe {
        vmwrite(EXIT_MSR_STORE_ADDRESS, config.msr_state_page)?;
        vmwrite(ENTRY_MSR_LOAD_ADDRESS, config.msr_state_page)?;
        vmwrite(EXIT_MSR_LOAD_ADDRESS, host_list)?;
    }
    // SAFETY: `ept_pointer` was constructed from validated EPT capabilities.
    unsafe {
        vmwrite(EPT_POINTER, config.nested_root)?;
        initialize_guest_state(config)?;
        initialize_host_state()?;
    }
    Ok(())
}

unsafe fn initialize_guest_state(config: GuestConfig) -> Result<(), Error> {
    let pvh = config.boot_mode == GuestBootMode::LinuxPvh32;
    for (field, selector) in [
        (GUEST_ES_SELECTOR, 0x10),
        (GUEST_CS_SELECTOR, 0x08),
        (GUEST_SS_SELECTOR, 0x10),
        (GUEST_DS_SELECTOR, 0x10),
        (GUEST_FS_SELECTOR, 0x10),
        (GUEST_GS_SELECTOR, 0x10),
        (GUEST_LDTR_SELECTOR, 0),
        (GUEST_TR_SELECTOR, 0x18),
    ] {
        // SAFETY: The selectors match the cached guest segment state below.
        unsafe { vmwrite(field, selector)? };
    }
    for field in [
        GUEST_ES_LIMIT,
        GUEST_CS_LIMIT,
        GUEST_SS_LIMIT,
        GUEST_DS_LIMIT,
        GUEST_FS_LIMIT,
        GUEST_GS_LIMIT,
        GUEST_LDTR_LIMIT,
    ] {
        // SAFETY: The long-mode cached segments use flat 32-bit limits.
        unsafe { vmwrite(field, 0xffff_ffff)? };
    }
    for field in [
        GUEST_ES_AR,
        GUEST_SS_AR,
        GUEST_DS_AR,
        GUEST_FS_AR,
        GUEST_GS_AR,
    ] {
        // SAFETY: 0xc093 is a present flat writable data segment.
        unsafe { vmwrite(field, 0xc093)? };
    }
    // SAFETY: These are architectural long-mode segment encodings.
    unsafe {
        vmwrite(GUEST_CS_AR, if pvh { 0xc09b } else { 0xa09b })?;
        vmwrite(GUEST_LDTR_AR, 0x1_0000)?;
        vmwrite(GUEST_TR_AR, 0x8b)?;
        vmwrite(GUEST_TR_LIMIT, 0x67)?;
        vmwrite(GUEST_GDTR_LIMIT, 0)?;
        vmwrite(GUEST_IDTR_LIMIT, 0)?;
    }
    for field in [
        GUEST_ES_BASE,
        GUEST_CS_BASE,
        GUEST_SS_BASE,
        GUEST_DS_BASE,
        GUEST_FS_BASE,
        GUEST_GS_BASE,
        GUEST_LDTR_BASE,
        GUEST_TR_BASE,
        GUEST_GDTR_BASE,
        GUEST_IDTR_BASE,
        GUEST_SYSENTER_ESP,
        GUEST_SYSENTER_EIP,
    ] {
        // SAFETY: The Domain starts with flat segment bases.
        unsafe { vmwrite(field, 0)? };
    }
    // SAFETY: All remaining values satisfy IA-32e guest VM-entry checks.
    unsafe {
        let requested_cr0 = if pvh { 0x11 } else { 0x8001_0033 };
        let requested_cr4 = if pvh { 0 } else { 1 << 5 };
        let cr0_fixed0 = read_msr(IA32_VMX_CR0_FIXED0);
        let cr0_fixed1 = read_msr(IA32_VMX_CR0_FIXED1);
        let cr4_fixed0 = read_msr(IA32_VMX_CR4_FIXED0);
        let cr4_fixed1 = read_msr(IA32_VMX_CR4_FIXED1);
        let mut guest_cr0 = adjusted_control_register(
            requested_cr0,
            cr0_fixed0,
            cr0_fixed1,
        );
        if pvh {
            guest_cr0 &= !(1 << 31);
            guest_cr0 |= 1;
        }
        let guest_cr4 = adjusted_control_register(
            requested_cr4,
            cr4_fixed0,
            cr4_fixed1,
        );
        vmwrite(GUEST_CR0, guest_cr0)?;
        vmwrite(GUEST_CR3, if pvh { 0 } else { config.page_table_root })?;
        vmwrite(GUEST_CR4, guest_cr4)?;
        // VMX may require CR0.NE and CR4.VMXE even when ordinary guest
        // software is allowed to clear them. Keep those host-required bits in
        // the VMCS while the read shadows expose the architectural guest view.
        // PE and PG remain guest-owned under unrestricted-guest execution so a
        // PVH kernel can perform its own protected/long-mode transition.
        vmwrite(
            CR0_GUEST_HOST_MASK,
            fixed_mask_for_guest(cr0_fixed0, (1 << 0) | CR0_PAGING),
        )?;
        vmwrite(CR4_GUEST_HOST_MASK, fixed_mask_for_guest(cr4_fixed0, 0))?;
        vmwrite(CR0_READ_SHADOW, requested_cr0)?;
        vmwrite(CR4_READ_SHADOW, requested_cr4)?;
        vmwrite(GUEST_DR7, 0x400)?;
        vmwrite(GUEST_RSP, config.stack)?;
        vmwrite(GUEST_RIP, config.entry)?;
        vmwrite(GUEST_RFLAGS, 2)?;
        vmwrite(GUEST_DEBUGCTL, 0)?;
        vmwrite(
            GUEST_EFER,
            if pvh {
                0
            } else {
                (1 << 8) | (1 << 10) | (1 << 11)
            },
        )?;
        if pvh {
            vmwrite(GUEST_PAT, read_msr(IA32_PAT))?;
        }
        vmwrite(VMCS_LINK_POINTER, u64::MAX)?;
        vmwrite(GUEST_INTERRUPTIBILITY, 0)?;
        vmwrite(GUEST_ACTIVITY_STATE, 0)?;
        vmwrite(GUEST_SYSENTER_CS, 0)?;
    }
    Ok(())
}

unsafe fn initialize_host_state() -> Result<(), Error> {
    // SAFETY: SGDT/SIDT only read the active descriptor-table registers.
    let (gdt_base, idt_base) = unsafe { descriptor_bases() };
    for (field, selector) in [
        (HOST_ES_SELECTOR, read_selector(SegmentRegister::Es)),
        (HOST_CS_SELECTOR, read_selector(SegmentRegister::Cs)),
        (HOST_SS_SELECTOR, read_selector(SegmentRegister::Ss)),
        (HOST_DS_SELECTOR, read_selector(SegmentRegister::Ds)),
        (HOST_FS_SELECTOR, read_selector(SegmentRegister::Fs)),
        (HOST_GS_SELECTOR, read_selector(SegmentRegister::Gs)),
        (HOST_TR_SELECTOR, read_selector(SegmentRegister::Tr)),
    ] {
        // SAFETY: Host selectors are current CPL0 selectors with RPL/TI removed.
        unsafe { vmwrite(field, u64::from(selector & !7))? };
    }
    // SAFETY: These values describe the currently executing 64-bit mBoot host.
    unsafe {
        vmwrite(HOST_CR0, read_cr0())?;
        vmwrite(HOST_CR3, read_cr3())?;
        vmwrite(HOST_CR4, read_cr4())?;
        vmwrite(HOST_FS_BASE, read_msr(IA32_FS_BASE))?;
        vmwrite(HOST_GS_BASE, read_msr(IA32_GS_BASE))?;
        vmwrite(HOST_TR_BASE, descriptor::tss_base())?;
        vmwrite(HOST_GDTR_BASE, gdt_base)?;
        vmwrite(HOST_IDTR_BASE, idt_base)?;
        vmwrite(HOST_SYSENTER_CS, read_msr(IA32_SYSENTER_CS) & 0xffff)?;
        vmwrite(HOST_SYSENTER_ESP, read_msr(IA32_SYSENTER_ESP))?;
        vmwrite(HOST_SYSENTER_EIP, read_msr(IA32_SYSENTER_EIP))?;
        vmwrite(HOST_EFER, read_msr(IA32_EFER))?;
        vmwrite(HOST_PAT, read_msr(IA32_PAT))?;
    }
    Ok(())
}

unsafe fn synchronize_guest_long_mode() -> Result<(), Error> {
    let efer = unsafe { vmread(GUEST_EFER) };
    let mut controls = unsafe { vmread(ENTRY_CONTROLS) };
    if efer & EFER_LMA != 0 {
        controls |= 1 << 9;
    } else {
        controls &= !(1 << 9);
    }
    unsafe { vmwrite(ENTRY_CONTROLS, controls) }
}

fn updated_guest_efer(current: u64, cr0: u64, requested: u64) -> Option<u64> {
    let writable = EFER_SCE | EFER_LME | EFER_NXE;
    if requested & !writable != current & !writable {
        return None;
    }
    if cr0 & CR0_PAGING != 0 && (requested ^ current) & EFER_LME != 0 {
        return None;
    }
    Some((current & !writable) | (requested & writable))
}

const fn fixed_mask_for_guest(fixed0: u64, guest_owned: u64) -> u64 {
    fixed0 & !guest_owned
}

unsafe fn initialize_guest_msr_lists(page: u64) -> Result<(), Error> {
    validate_page(page)?;
    unsafe { write_bytes(page as *mut u8, 0, 4096) };
    let guest = page as *mut VmEntryMsr;
    let host = unsafe { guest.add(GUEST_MSR_LIST.len()) };
    for (index, msr) in GUEST_MSR_LIST.iter().copied().enumerate() {
        unsafe {
            guest.add(index).write(VmEntryMsr {
                index: msr,
                reserved: 0,
                value: 0,
            });
            host.add(index).write(VmEntryMsr {
                index: msr,
                reserved: 0,
                value: read_msr(msr),
            });
        }
    }
    Ok(())
}

fn adjusted_vm_control(desired: u64, capability: u64) -> u64 {
    (desired | (capability & 0xffff_ffff)) & (capability >> 32)
}

#[derive(Clone, Copy)]
enum SegmentRegister {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
    Tr,
}

unsafe fn read_selector(register: SegmentRegister) -> u16 {
    let value: u16;
    // SAFETY: Reading segment selectors is valid at CPL0.
    unsafe {
        match register {
            SegmentRegister::Es => {
                asm!("mov {0:x}, es", out(reg) value, options(nomem, nostack, preserves_flags))
            }
            SegmentRegister::Cs => {
                asm!("mov {0:x}, cs", out(reg) value, options(nomem, nostack, preserves_flags))
            }
            SegmentRegister::Ss => {
                asm!("mov {0:x}, ss", out(reg) value, options(nomem, nostack, preserves_flags))
            }
            SegmentRegister::Ds => {
                asm!("mov {0:x}, ds", out(reg) value, options(nomem, nostack, preserves_flags))
            }
            SegmentRegister::Fs => {
                asm!("mov {0:x}, fs", out(reg) value, options(nomem, nostack, preserves_flags))
            }
            SegmentRegister::Gs => {
                asm!("mov {0:x}, gs", out(reg) value, options(nomem, nostack, preserves_flags))
            }
            SegmentRegister::Tr => {
                asm!("str {0:x}", out(reg) value, options(nomem, nostack, preserves_flags))
            }
        }
    }
    value
}

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

unsafe fn descriptor_bases() -> (u64, u64) {
    let mut gdt = DescriptorTablePointer { limit: 0, base: 0 };
    let mut idt = DescriptorTablePointer { limit: 0, base: 0 };
    // SAFETY: SGDT and SIDT store into valid local descriptors.
    unsafe {
        asm!("sgdt [{}]", in(reg) &mut gdt, options(nostack, preserves_flags));
        asm!("sidt [{}]", in(reg) &mut idt, options(nostack, preserves_flags));
    }
    (gdt.base, idt.base)
}

unsafe fn vmwrite(field: u64, value: u64) -> Result<(), Error> {
    let failed: u8;
    // SAFETY: VMX is active and a current VMCS was loaded by `enable`. Intel
    // syntax places the VMCS field register before the value operand.
    unsafe {
        asm!("vmwrite {field}, {value}", "setna {failed}", value = in(reg) value,
            field = in(reg) field, failed = lateout(reg_byte) failed, options(nostack));
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(Error::VmcsWriteFailed(field))
    }
}

unsafe fn vmread(field: u64) -> u64 {
    let value: u64;
    // SAFETY: VMX is active and the field is readable from the current VMCS.
    unsafe { asm!("vmread rax, rcx", in("rcx") field, lateout("rax") value, options(nostack)) };
    value
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
    fn vmx_only_hides_host_required_control_bits() {
        let fixed0 = CR0_PAGING | (1 << 5) | 1;
        assert_eq!(
            fixed_mask_for_guest(fixed0, CR0_PAGING | 1),
            1 << 5
        );
        assert_eq!(fixed_mask_for_guest(1 << 13, 0), 1 << 13);
    }

    #[test]
    fn vm_controls_include_required_bits_and_remove_unsupported_bits() {
        let capability = 0b0010 | (0b0111u64 << 32);
        assert_eq!(adjusted_vm_control(0b1101, capability), 0b0111);
    }

    #[test]
    fn primary_controls_request_msr_bitmap_support() {
        let desired = (1 << 7) | (1 << 28) | (1 << 31);
        let capability = desired << 32;
        assert_eq!(adjusted_vm_control(desired, capability), desired);
        assert_ne!(adjusted_vm_control(desired, capability) & (1 << 28), 0);
    }

    #[test]
    fn control_pages_must_be_aligned() {
        assert_eq!(validate_page(0), Err(Error::InvalidPage));
        assert_eq!(validate_page(0x1001), Err(Error::InvalidPage));
        assert_eq!(validate_page(0x2000), Ok(()));
    }

    #[test]
    fn assembly_context_offsets_include_all_guest_scratch_registers() {
        assert_eq!(core::mem::offset_of!(VmxRunContext, resume), 80);
        assert_eq!(core::mem::offset_of!(VmxRunContext, rcx), 88);
        assert_eq!(core::mem::offset_of!(VmxRunContext, r8), 96);
        assert_eq!(core::mem::offset_of!(VmxRunContext, r11), 120);
        assert_eq!(core::mem::size_of::<VmxRunContext>(), 128);
    }

    #[test]
    fn vmx_basic_bit_48_means_32_bit_address_restriction() {
        let above_4_gib = 0x1_0000_0000;
        assert!(control_region_address_valid(0, above_4_gib));
        assert!(!control_region_address_valid(1 << 48, above_4_gib));
        assert!(control_region_address_valid(1 << 48, u32::MAX as u64));
    }

    #[test]
    fn ept_requires_secondary_control_four_levels_and_write_back() {
        let primary = (1u64 << 31) << 32;
        let secondary = (1u64 << 1) << 32;
        let capabilities = (1 << 6) | (1 << 14) | (1 << 20) | (1 << 25);
        assert!(supports_ept(primary, secondary, capabilities));
        assert!(!supports_ept(primary, secondary, capabilities & !(1 << 6)));
        assert!(!supports_ept(primary, secondary, capabilities & !(1 << 14)));
        assert!(!supports_ept(primary, secondary, capabilities & !(1 << 20)));
        assert!(!supports_ept(primary, secondary, capabilities & !(1 << 25)));
        assert!(!supports_ept(0, secondary, capabilities));
        assert!(!supports_ept(primary, 0, capabilities));
    }

    #[test]
    fn efer_lme_can_change_only_while_paging_is_disabled() {
        assert_eq!(updated_guest_efer(0, 0, EFER_LME), Some(EFER_LME));
        assert_eq!(updated_guest_efer(0, CR0_PAGING, EFER_LME), None);
        assert_eq!(
            updated_guest_efer(EFER_LME | EFER_LMA, CR0_PAGING, EFER_LME | EFER_LMA),
            Some(EFER_LME | EFER_LMA)
        );
        assert_eq!(updated_guest_efer(0, 0, EFER_LMA), None);
    }
}
