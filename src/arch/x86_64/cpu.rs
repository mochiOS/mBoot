use core::arch::x86_64::{__cpuid, __cpuid_count};

use crate::BackendKind;

const INTEL_VENDOR: [u8; 12] = *b"GenuineIntel";
const AMD_VENDOR: [u8; 12] = *b"AuthenticAMD";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuFeatures {
    pub vendor: [u8; 12],
    pub backend: Option<BackendKind>,
    pub nested_paging: Option<bool>,
    pub address_space_ids: u32,
}

pub fn detect() -> CpuFeatures {
    let leaf0 = __cpuid(0);
    let vendor = vendor_bytes(leaf0.ebx, leaf0.edx, leaf0.ecx);
    let mut features = CpuFeatures {
        vendor,
        backend: None,
        nested_paging: None,
        address_space_ids: 0,
    };

    if vendor == INTEL_VENDOR {
        let leaf1 = __cpuid(1);
        if leaf1.ecx & (1 << 5) != 0 {
            features.backend = Some(BackendKind::IntelVmx);
        }
    } else if vendor == AMD_VENDOR {
        let extended = __cpuid(0x8000_0000).eax;
        if extended >= 0x8000_0001 && __cpuid(0x8000_0001).ecx & (1 << 2) != 0 {
            features.backend = Some(BackendKind::AmdSvm);
        }
        if extended >= 0x8000_000a {
            let svm = __cpuid_count(0x8000_000a, 0);
            features.address_space_ids = svm.ebx;
            features.nested_paging = Some(svm.edx & 1 != 0);
        }
    }

    features
}

const fn vendor_bytes(ebx: u32, edx: u32, ecx: u32) -> [u8; 12] {
    let b = ebx.to_le_bytes();
    let d = edx.to_le_bytes();
    let c = ecx.to_le_bytes();
    [
        b[0], b[1], b[2], b[3], d[0], d[1], d[2], d[3], c[0], c[1], c[2], c[3],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_layout_matches_cpuid_order() {
        assert_eq!(
            vendor_bytes(0x756e_6547, 0x4965_6e69, 0x6c65_746e),
            INTEL_VENDOR
        );
        assert_eq!(
            vendor_bytes(0x6874_7541, 0x6974_6e65, 0x444d_4163),
            AMD_VENDOR
        );
    }

    #[test]
    fn host_detection_never_invents_a_vendor() {
        let detected = detect();
        if detected.backend == Some(BackendKind::IntelVmx) {
            assert_eq!(detected.vendor, INTEL_VENDOR);
        }
        if detected.backend == Some(BackendKind::AmdSvm) {
            assert_eq!(detected.vendor, AMD_VENDOR);
        }
    }
}
