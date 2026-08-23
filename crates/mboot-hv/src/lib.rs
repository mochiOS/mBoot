#![no_std]

pub mod arch;
pub mod domain;
pub mod memory;

use arch::x86_64::{cpu, svm, vmx};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    UnsupportedCpu,
    VirtualizationDisabled,
    NestedPagingUnavailable,
    InvalidPage,
    InvalidState,
    ControlInstructionFailed,
    ControlRegionTooLarge,
    UnrestrictedGuestUnavailable,
    GuestEntryFailed,
    UnexpectedVmExit(u64),
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmExit {
    pub reason: VmExitReason,
    pub raw_reason: u64,
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

    /// Enters a one-vCPU guest and returns after its first intercepted exit.
    ///
    /// # Safety
    /// `nested_root` must describe a live EPT/NPT owned by mBoot. Guest physical
    /// address zero must contain executable guest code. This must run on the CPU
    /// that enabled this backend, with interrupts disabled.
    pub unsafe fn run(&mut self, nested_root: u64) -> Result<VmExit, Error> {
        match self {
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Intel(vmx) => unsafe { vmx.run(nested_root) },
            // SAFETY: The public function contract is forwarded unchanged.
            Self::Amd(svm) => unsafe { svm.run(nested_root) },
        }
    }
}
