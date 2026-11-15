#![no_std]

pub mod arch;
pub mod boot_log;
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

mod error;
pub use error::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    IntelVmx,
    AmdSvm,
}

// Preserve the public x86 API while keeping CR3, MSR and VMX/SVM controls
// with their CPU implementation. Native EL2 state has a separate typed API.
pub use arch::x86_64::backend::{
    CpuidResult, GuestBootMode, GuestConfig, Virtualization, VirtualizationResources, VmExit, VmExitReason,
};
