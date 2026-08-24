use crate::CpuidResult;

pub const MAX_BASIC_LEAF: u32 = 0x0b;
pub const MAX_EXTENDED_LEAF: u32 = 0x8000_0008;
pub const MAX_HYPERVISOR_LEAF: u32 = 0x4000_0001;

const CPU_VENDOR: [u8; 12] = *b"MochiOS CPU ";
const HYPERVISOR_VENDOR: [u8; 12] = *b"MochiOSmBoot";
const CPU_BRAND: [u8; 48] = *b"mochiOS Virtual CPU                             ";

const LEAF1_ECX_X2APIC: u32 = 1 << 21;
const LEAF1_ECX_HYPERVISOR: u32 = 1 << 31;
const LEAF1_EDX_BASELINE: u32 = (1 << 0)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 8)
    | (1 << 9)
    | (1 << 15)
    | (1 << 23)
    | (1 << 24)
    | (1 << 25)
    | (1 << 26);
const EXTENDED_EDX_NX: u32 = 1 << 20;
const EXTENDED_EDX_LONG_MODE: u32 = 1 << 29;

/// Returns the stable CPU model exposed to a mochiOS domain.
///
/// `apic_id` and `vcpu_count` describe the domain, not the physical host.
pub fn query(leaf: u32, subleaf: u32, apic_id: u32, vcpu_count: u32) -> CpuidResult {
    match leaf {
        0 => vendor_leaf(MAX_BASIC_LEAF, CPU_VENDOR, false),
        1 => CpuidResult {
            eax: 0x0000_06a0,
            ebx: ((apic_id & 0xff) << 24) | (vcpu_count.clamp(1, 0xff) << 16),
            ecx: LEAF1_ECX_X2APIC | LEAF1_ECX_HYPERVISOR,
            edx: LEAF1_EDX_BASELINE,
        },
        0x0b => topology_leaf(subleaf, apic_id, vcpu_count),
        0x4000_0000 => vendor_leaf(MAX_HYPERVISOR_LEAF, HYPERVISOR_VENDOR, true),
        0x4000_0001 => CpuidResult {
            eax: 1,
            ..CpuidResult::default()
        },
        0x8000_0000 => CpuidResult {
            eax: MAX_EXTENDED_LEAF,
            ..CpuidResult::default()
        },
        0x8000_0001 => CpuidResult {
            edx: EXTENDED_EDX_NX | EXTENDED_EDX_LONG_MODE,
            ..CpuidResult::default()
        },
        0x8000_0002..=0x8000_0004 => brand_leaf(leaf),
        0x8000_0008 => CpuidResult {
            // The initial model permits 64 GiB of guest physical addresses and
            // uses the conventional four-level, 48-bit linear address space.
            eax: 36 | (48 << 8),
            ..CpuidResult::default()
        },
        _ => CpuidResult::default(),
    }
}

fn vendor_leaf(max_leaf: u32, vendor: [u8; 12], hypervisor_order: bool) -> CpuidResult {
    let first = u32::from_le_bytes(vendor[0..4].try_into().unwrap());
    let second = u32::from_le_bytes(vendor[4..8].try_into().unwrap());
    let third = u32::from_le_bytes(vendor[8..12].try_into().unwrap());
    if hypervisor_order {
        CpuidResult {
            eax: max_leaf,
            ebx: first,
            ecx: second,
            edx: third,
        }
    } else {
        CpuidResult {
            eax: max_leaf,
            ebx: first,
            ecx: third,
            edx: second,
        }
    }
}

fn topology_leaf(subleaf: u32, apic_id: u32, vcpu_count: u32) -> CpuidResult {
    let logical_processors = vcpu_count.clamp(1, u16::MAX.into());
    match subleaf {
        0 => CpuidResult {
            eax: 0,
            ebx: 1,
            ecx: 1 << 8,
            edx: apic_id,
        },
        1 => CpuidResult {
            eax: logical_processors.next_power_of_two().trailing_zeros(),
            ebx: logical_processors,
            ecx: (2 << 8) | 1,
            edx: apic_id,
        },
        _ => CpuidResult {
            edx: apic_id,
            ..CpuidResult::default()
        },
    }
}

fn brand_leaf(leaf: u32) -> CpuidResult {
    let offset = ((leaf - 0x8000_0002) * 16) as usize;
    let word = |index: usize| {
        u32::from_le_bytes(
            CPU_BRAND[offset + index..offset + index + 4]
                .try_into()
                .unwrap(),
        )
    };
    CpuidResult {
        eax: word(0),
        ebx: word(4),
        ecx: word(8),
        edx: word(12),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_nested_virtualization_and_identifies_mboot() {
        let features = query(1, 0, 0, 1);
        assert_eq!(features.ecx & (1 << 5), 0);
        assert_ne!(features.ecx & LEAF1_ECX_HYPERVISOR, 0);
        assert_ne!(features.ecx & LEAF1_ECX_X2APIC, 0);
        assert_eq!(query(0x8000_0001, 0, 0, 1).ecx & (1 << 2), 0);

        let vendor = query(0x4000_0000, 0, 0, 1);
        let bytes = [vendor.ebx, vendor.ecx, vendor.edx]
            .map(u32::to_le_bytes)
            .concat();
        assert_eq!(bytes, HYPERVISOR_VENDOR);
    }

    #[test]
    fn reports_domain_topology_instead_of_host_topology() {
        let leaf = query(0x0b, 1, 3, 4);
        assert_eq!(leaf.eax, 2);
        assert_eq!(leaf.ebx, 4);
        assert_eq!(leaf.edx, 3);
    }

    #[test]
    fn unsupported_leaves_are_empty() {
        assert_eq!(query(0x1234_5678, 0, 0, 1), CpuidResult::default());
        assert_eq!(query(7, 0, 0, 1), CpuidResult::default());
    }
}
