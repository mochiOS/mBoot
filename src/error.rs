//! Hypervisor errors shared by native mechanisms and image validation.
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

