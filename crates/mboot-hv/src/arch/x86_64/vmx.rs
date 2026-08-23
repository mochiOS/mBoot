use core::arch::{asm, global_asm};
use core::ptr::write_bytes;

use crate::arch::x86_64::{
    descriptor, read_cr0, read_cr3, read_cr4, read_msr, write_cr0, write_cr4, write_msr,
};
use crate::{arch::x86_64::cpu, BackendKind, Error, GuestConfig, VmExit, VmExitReason};

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
const SECONDARY_CONTROLS: u64 = 0x401e;
const EPT_POINTER: u64 = 0x201a;
const VMCS_LINK_POINTER: u64 = 0x2800;
const GUEST_DEBUGCTL: u64 = 0x2802;
const GUEST_EFER: u64 = 0x2806;
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
const EXIT_INSTRUCTION_LENGTH: u64 = 0x440c;
const HLT_EXIT_REASON: u64 = 12;
const VMCALL_EXIT_REASON: u64 = 18;

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
    "vmwrite rax, rcx",
    "lea rax, [rip + mboot_vmx_exit]",
    "mov rcx, 0x6c16",
    "vmwrite rax, rcx",
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
    "mov rax, [rax]",
    "vmlaunch",
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
    "mov rax, [rax]",
    "vmresume",
    "mboot_vmx_entry_failed:",
    "mov eax, 1",
    "jmp mboot_vmx_return",
    "mboot_vmx_exit:",
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
    "mov rcx, [rsp + 80]",
    "mov rax, [rsp + 72]",
    "mov [rcx], rax",
    "mov rax, [rsp + 16]",
    "mov [rcx + 8], rax",
    "mov rax, [rsp + 8]",
    "mov [rcx + 16], rax",
    "mov rax, [rsp]",
    "mov [rcx + 24], rax",
    "mov rax, [rsp + 64]",
    "mov [rcx + 32], rax",
    "mov rax, [rsp + 56]",
    "mov [rcx + 40], rax",
    "mov rax, [rsp + 48]",
    "mov [rcx + 48], rax",
    "mov rax, [rsp + 40]",
    "mov [rcx + 56], rax",
    "mov rax, [rsp + 32]",
    "mov [rcx + 64], rax",
    "mov rax, [rsp + 24]",
    "mov [rcx + 72], rax",
    "add rsp, 80",
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
        let supports_64_bit_phys = unsafe { read_msr(IA32_VMX_BASIC) } & (1 << 48) != 0;
        if !supports_64_bit_phys && vmcs_phys > u32::MAX as u64 {
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
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::ControlInstructionFailed)? };
        // SAFETY: VMX is active and all VMCS host/guest values are supplied here.
        unsafe { initialize_vmcs(config)? };
        self.run_context = VmxRunContext {
            rax: 0,
            rdi: config.boot_info,
            ..VmxRunContext::default()
        };
        // SAFETY: The VMCS host RIP/RSP target the assembly return trampoline.
        if unsafe { mboot_vmx_launch(&raw mut self.run_context) } != 0 {
            return Err(Error::GuestEntryFailed);
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
        unsafe { vmptrld(self.vmcs_phys).map_err(|()| Error::ControlInstructionFailed)? };
        // SAFETY: VMEXIT left a current, stopped VMCS with readable exit fields.
        let (rip, instruction_len) =
            unsafe { (vmread(GUEST_RIP), vmread(EXIT_INSTRUCTION_LENGTH)) };
        // SAFETY: The stopped VMCS accepts the next guest RIP.
        unsafe { vmwrite(GUEST_RIP, rip + instruction_len)? };
        self.run_context.rax = result;
        self.run_context.resume = 1;
        // SAFETY: The VMCS and captured register state belong to this stopped vCPU.
        if unsafe { mboot_vmx_launch(&raw mut self.run_context) } != 0 {
            return Err(Error::GuestEntryFailed);
        }
        // SAFETY: VMRESUME returned only through a VM exit.
        unsafe { self.decode_exit() }
    }

    unsafe fn decode_exit(&self) -> Result<VmExit, Error> {
        // SAFETY: A VM exit returned through the configured host trampoline.
        let reason = unsafe { vmread(EXIT_REASON) } & 0xffff;
        match reason {
            HLT_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::Halt,
                raw_reason: reason,
                hypercall_number: 0,
                arg0: 0,
                arg1: 0,
                arg2: 0,
            }),
            VMCALL_EXIT_REASON => Ok(VmExit {
                reason: VmExitReason::Hypercall,
                raw_reason: reason,
                hypercall_number: self.run_context.rax,
                arg0: self.run_context.rdi,
                arg1: self.run_context.rsi,
                arg2: self.run_context.rdx,
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
    // SAFETY: Capability MSRs are available after VMX CPUID detection.
    let (pin, primary, secondary, exit, entry) = unsafe {
        (
            adjusted_vm_control(0, read_msr(pin_msr)),
            adjusted_vm_control((1 << 7) | (1 << 31), read_msr(primary_msr)),
            adjusted_vm_control(1 << 1, read_msr(IA32_VMX_PROCBASED_CTLS2)),
            adjusted_vm_control((1 << 9) | (1 << 21), read_msr(exit_msr)),
            adjusted_vm_control((1 << 9) | (1 << 15), read_msr(entry_msr)),
        )
    };
    if secondary & (1 << 1) == 0 {
        return Err(Error::NestedPagingUnavailable);
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
        (EXIT_MSR_STORE_COUNT, 0),
        (EXIT_MSR_LOAD_COUNT, 0),
        (ENTRY_MSR_LOAD_COUNT, 0),
        (ENTRY_INTERRUPTION_INFO, 0),
    ] {
        // SAFETY: Each field is a writable control field of the current VMCS.
        unsafe { vmwrite(field, value)? };
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
        vmwrite(GUEST_CS_AR, 0xa09b)?;
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
        let guest_cr0 = adjusted_control_register(
            0x8001_0033,
            read_msr(IA32_VMX_CR0_FIXED0),
            read_msr(IA32_VMX_CR0_FIXED1),
        );
        let guest_cr4 = adjusted_control_register(
            1 << 5,
            read_msr(IA32_VMX_CR4_FIXED0),
            read_msr(IA32_VMX_CR4_FIXED1),
        );
        vmwrite(GUEST_CR0, guest_cr0)?;
        vmwrite(GUEST_CR3, config.page_table_root)?;
        vmwrite(GUEST_CR4, guest_cr4)?;
        vmwrite(GUEST_DR7, 0x400)?;
        vmwrite(GUEST_RSP, config.stack)?;
        vmwrite(GUEST_RIP, config.entry)?;
        vmwrite(GUEST_RFLAGS, 2)?;
        vmwrite(GUEST_DEBUGCTL, 0)?;
        vmwrite(GUEST_EFER, (1 << 8) | (1 << 10))?;
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
    // SAFETY: VMX is active and a current VMCS was loaded by `enable`.
    unsafe {
        asm!("vmwrite {value}, {field}", "setna {failed}", value = in(reg) value,
            field = in(reg) field, failed = lateout(reg_byte) failed, options(nostack));
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(Error::ControlInstructionFailed)
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
    fn vm_controls_include_required_bits_and_remove_unsupported_bits() {
        let capability = 0b0010 | (0b0111u64 << 32);
        assert_eq!(adjusted_vm_control(0b1101, capability), 0b0111);
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
