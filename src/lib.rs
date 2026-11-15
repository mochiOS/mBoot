#![no_std]

pub mod arch;
pub mod boot_log;
pub mod bundle;
#[cfg(target_arch = "x86_64")]
pub mod cpuid;
#[cfg(target_arch = "x86_64")]
pub mod device;
pub mod domain;
pub mod event;
pub mod grant;
pub mod image;
#[cfg(target_arch = "x86_64")]
pub mod interrupt;
#[cfg(target_arch = "x86_64")]
pub mod iommu;
pub mod manifest;
#[cfg(target_arch = "x86_64")]
pub mod memory;
#[cfg(target_arch = "x86_64")]
pub mod pci;
pub mod scheduler;

mod error;
pub use error::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    IntelVmx,
    AmdSvm,
    #[cfg(target_arch = "aarch64")]
    ArmEl2,
}

// Preserve the public x86 API while keeping CR3, MSR and VMX/SVM controls
// with their CPU implementation. Native EL2 state has a separate typed API.
#[cfg(target_arch = "x86_64")]
pub use arch::x86_64::backend::{
    CpuidResult, GuestBootMode, GuestConfig, Virtualization, VirtualizationResources, VmExit, VmExitReason,
};
