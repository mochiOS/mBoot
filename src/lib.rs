#![no_std]

pub mod arch;
pub mod bundle;
pub mod cpuid;
pub mod device;
pub mod domain;
pub mod event;
pub mod grant;
pub mod image;
pub mod interrupt;
pub mod iommu;
pub mod manifest;
pub mod memory;
pub mod pci;
pub mod scheduler;

use arch::x86_64::{cpu, svm, vmx};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    UnsupportedCpu,
    VirtualizationDisabled,
    NestedPagingUnavailable,
    InvalidPage,
    InvalidState,
    ControlInstructionFailed,
    VmcsLoadFailed,
    VmcsWriteFailed(u64),
    ControlRegionTooLarge,
    GuestEntryFailed(u64),
    UnexpectedVmExit(u64),
    InvalidImage,
    ImageTooLarge,
    InvalidManifest,
    InvalidBundle,
    ManifestDigestMismatch,
    ImageDigestMismatch,
    UnsupportedDomainConfig,
    AddressSpaceIdUnavailable,
    InterruptVirtualizationUnavailable,
    DeviceQuarantineFailed,
    UnsupportedIommu,
    IommuInitializationFailed,
    IommuCommandTimeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    IntelVmx,
    AmdSvm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualizationResources {
    pub host_control_page: u64,
    pub vcpu_control_page: u64,
}

pub enum Virtualization {
    Intel(vmx::Vmx),
    Amd(svm::Svm),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmExitReason {
    Halt,
    Hypercall,
    Preempted,
    InterruptWindow,
    MsrRead,
    MsrWrite,
    Cpuid,
    ControlRegisterWrite,
    NestedPageFault,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmExit {
    pub reason: VmExitReason,
    pub raw_reason: u64,
    pub hypercall_number: u64,
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub msr: u32,
    pub msr_value: u64,
    pub cpuid_leaf: u32,
    pub cpuid_subleaf: u32,
    pub fault_address: u64,
    pub fault_info: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestBootMode {
    Long64,
    LinuxPvh32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestConfig {
    pub boot_mode: GuestBootMode,
    pub nested_root: u64,
    pub page_table_root: u64,
    pub entry: u64,
    pub stack: u64,
    pub boot_info: u64,
    pub msr_permission_map: u64,
    pub msr_state_page: u64,
}

impl Virtualization {
    /// Enables the virtualization extension offered by the current CPU.
    ///
    /// # Safety
    /// The caller must run at CPL0, keep every supplied page exclusively owned
    /// by mBoot, and pin execution to the current logical CPU until disabled.
    pub unsafe fn enable(resources: VirtualizationResources) -> Result<Self, Error> {
        match cpu::detect().backend {
            Some(BackendKind::IntelVmx) => {
                // SAFETY: The public function contract is forwarded unchanged.
                unsafe {
                    vmx::Vmx::enable(resources.host_control_page, resources.vcpu_control_page)
                        .map(Self::Intel)
                }
            }
            Some(BackendKind::AmdSvm) => {
                // SAFETY: The public function contract is forwarded unchanged.
                unsafe {
                    svm::Svm::enable(resources.host_control_page, resources.vcpu_control_page)
                        .map(Self::Amd)
                }
            }
            None => Err(Error::UnsupportedCpu),
        }
    }

    pub const fn kind(&self) -> BackendKind {
        match self {
            Self::Intel(_) => BackendKind::IntelVmx,
            Self::Amd(_) => BackendKind::AmdSvm,
        }
    }

    /// Creates another vCPU under the virtualization state enabled by `self`.
    ///
    /// # Safety
    /// `vcpu_control_page` must be writable, page aligned, and exclusively owned
    /// by mBoot. `address_space_id` must be unique among live vCPUs on this CPU.
    /// The caller must remain on the CPU that enabled `self`.
    pub unsafe fn create_vcpu(
        &self,
        vcpu_control_page: u64,
        address_space_id: u32,
    ) -> Result<Self, Error> {
        match self {
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Intel(vmx) => unsafe {
                vmx.create_vcpu(vcpu_control_page, address_space_id)
                    .map(Self::Intel)
            },
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Amd(svm) => unsafe {
                svm.create_vcpu(vcpu_control_page, address_space_id)
                    .map(Self::Amd)
            },
        }
    }

    /// Enters a one-vCPU guest and returns after its first intercepted exit.
    ///
    /// # Safety
    /// `nested_root` must describe a live EPT/NPT owned by mBoot. Guest physical
    /// `entry`, `stack`, and `page_table_root` must point into mapped guest RAM.
    /// This must run on the CPU that enabled this backend, with interrupts disabled.
    pub unsafe fn run(&mut self, config: GuestConfig) -> Result<VmExit, Error> {
        match self {
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Intel(vmx) => unsafe { vmx.run(config) },
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Amd(svm) => unsafe { svm.run(config) },
        }
    }

    /// Returns a stopped vCPU to its pre-launch architectural state.
    ///
    /// # Safety
    /// The vCPU must be stopped on the CPU that owns its control structure.
    pub unsafe fn reset_vcpu(&mut self) -> Result<(), Error> {
        match self {
            Self::Intel(vmx) => unsafe { vmx.reset() },
            Self::Amd(svm) => unsafe { svm.reset() },
        }
    }

    /// Completes the intercepted Hypercall and resumes the same vCPU.
    ///
    /// # Safety
    /// The previous exit must be a Hypercall from this vCPU. Guest memory and
    /// control structures must still satisfy the `run` contract.
    pub unsafe fn resume(&mut self, result: u64) -> Result<VmExit, Error> {
        match self {
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Intel(vmx) => unsafe { vmx.resume(result) },
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Amd(svm) => unsafe { svm.resume(result) },
        }
    }

    /// Resumes a vCPU after a host timer preempted it.
    ///
    /// Unlike [`Self::resume`], this does not advance the guest instruction
    /// pointer or replace RAX because no guest instruction caused the exit.
    ///
    /// # Safety
    /// The previous exit must have been `VmExitReason::Preempted` from this vCPU.
    pub unsafe fn resume_preempted(&mut self) -> Result<VmExit, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.resume_preempted() },
            Virtualization::Amd(svm) => unsafe { svm.resume_preempted() },
        }
    }

    /// Completes an intercepted HLT and resumes after the instruction.
    ///
    /// # Safety
    /// The previous exit must have been `VmExitReason::Halt` from this vCPU.
    pub unsafe fn resume_halted(&mut self) -> Result<VmExit, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.resume_halted() },
            Virtualization::Amd(svm) => unsafe { svm.resume_halted() },
        }
    }

    /// Completes an intercepted RDMSR and resumes after the instruction.
    ///
    /// # Safety
    /// The previous exit must have been `VmExitReason::MsrRead` from this vCPU.
    pub unsafe fn resume_msr_read(&mut self, value: u64) -> Result<VmExit, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.resume_msr_read(value) },
            Virtualization::Amd(svm) => unsafe { svm.resume_msr_read(value) },
        }
    }

    /// Completes an intercepted WRMSR and resumes after the instruction.
    ///
    /// # Safety
    /// The previous exit must have been `VmExitReason::MsrWrite` from this vCPU.
    pub unsafe fn resume_msr_write(&mut self) -> Result<VmExit, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.resume_msr_write() },
            Virtualization::Amd(svm) => unsafe { svm.resume_msr_write() },
        }
    }

    /// Completes an intercepted CPUID and resumes after the instruction.
    ///
    /// # Safety
    /// The previous exit must have been `VmExitReason::Cpuid` from this vCPU.
    pub unsafe fn resume_cpuid(&mut self, result: CpuidResult) -> Result<VmExit, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.resume_cpuid(result) },
            Virtualization::Amd(svm) => unsafe { svm.resume_cpuid(result) },
        }
    }

    /// Completes an intercepted write to CR0 or CR4 and resumes the vCPU.
    ///
    /// # Safety
    /// The previous exit must be a control-register write from this vCPU.
    pub unsafe fn resume_control_register_write(
        &mut self,
        register: u8,
        value: u64,
    ) -> Result<VmExit, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe {
                vmx.resume_control_register_write(register, value)
            },
            Virtualization::Amd(_) => Err(Error::InvalidState),
        }
    }

    /// Queues an external interrupt for delivery on the next vCPU entry.
    ///
    /// # Safety
    /// The vCPU must be stopped and its guest IDT must accept `vector`.
    pub unsafe fn inject_interrupt(&mut self, vector: u8) -> Result<(), Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.inject_interrupt(vector) },
            Virtualization::Amd(svm) => unsafe { svm.inject_interrupt(vector) },
        }
    }

    /// Queues a general-protection fault with error code zero for VM entry.
    ///
    /// # Safety
    /// The vCPU must be stopped at the faulting instruction.
    pub unsafe fn inject_general_protection(&mut self) -> Result<(), Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.inject_general_protection() },
            Virtualization::Amd(svm) => unsafe { svm.inject_general_protection() },
        }
    }

    /// Reports whether the stopped guest can accept an external interrupt on
    /// its next entry.
    ///
    /// # Safety
    /// The vCPU must have entered at least once and be stopped on its owner CPU.
    pub unsafe fn can_inject_interrupt(&mut self) -> Result<bool, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.can_inject_interrupt() },
            Virtualization::Amd(svm) => unsafe { svm.can_inject_interrupt() },
        }
    }

    /// Enables or disables an exit when the guest next becomes interruptible.
    /// AMD SVM keeps a queued VINTR in hardware and therefore needs no exit.
    ///
    /// # Safety
    /// The vCPU must be stopped on its owner CPU.
    pub unsafe fn set_interrupt_window(&mut self, enabled: bool) -> Result<(), Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.set_interrupt_window(enabled) },
            Virtualization::Amd(_) => Ok(()),
        }
    }

    /// Invalidates nested translations after mBoot changes a stopped Domain map.
    ///
    /// # Safety
    /// The vCPU must be stopped on the CPU that owns this virtualization state.
    pub unsafe fn flush_nested(&mut self, nested_root: u64) -> Result<(), Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.flush_nested(nested_root) },
            Virtualization::Amd(svm) => unsafe { svm.flush_nested() },
        }
    }

    /// Reads the instruction pointer from a stopped vCPU for crash reporting.
    ///
    /// # Safety
    /// The vCPU must have entered at least once and be stopped on its owner CPU.
    pub unsafe fn guest_instruction_pointer(&self) -> Result<u64, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.guest_instruction_pointer() },
            Virtualization::Amd(svm) => unsafe { svm.guest_instruction_pointer() },
        }
    }

    /// Reads the stack pointer from a stopped vCPU for crash reporting.
    ///
    /// # Safety
    /// The vCPU must have entered at least once and be stopped on its owner CPU.
    pub unsafe fn guest_stack_pointer(&self) -> Result<u64, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.guest_stack_pointer() },
            Virtualization::Amd(svm) => unsafe { svm.guest_stack_pointer() },
        }
    }

    /// Reads CR0 and CR3 from a stopped vCPU for early paging diagnostics.
    ///
    /// # Safety
    /// The vCPU must have entered at least once and be stopped on its owner CPU.
    pub unsafe fn guest_paging_state(&self) -> Result<(u64, u64), Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.guest_paging_state() },
            Virtualization::Amd(svm) => unsafe { svm.guest_paging_state() },
        }
    }

    /// Reads an intercepted architectural MSR from stopped guest state.
    ///
    /// # Safety
    /// The vCPU must be stopped on its owner CPU.
    pub unsafe fn read_guest_msr(&self, msr: u32) -> Result<u64, Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.read_guest_msr(msr) },
            Virtualization::Amd(svm) => unsafe { svm.read_guest_msr(msr) },
        }
    }

    /// Writes an intercepted architectural MSR into stopped guest state.
    ///
    /// # Safety
    /// The vCPU must be stopped on its owner CPU.
    pub unsafe fn write_guest_msr(&mut self, msr: u32, value: u64) -> Result<(), Error> {
        match self {
            Virtualization::Intel(vmx) => unsafe { vmx.write_guest_msr(msr, value) },
            Virtualization::Amd(svm) => unsafe { svm.write_guest_msr(msr, value) },
        }
    }
}
