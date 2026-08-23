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
}
