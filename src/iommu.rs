use core::slice;
use core::{
    arch::asm,
    hint::spin_loop,
    ptr::{read_volatile, write_bytes, write_volatile},
};

const ACPI_HEADER_SIZE: usize = 36;
const IOMMU_TABLE_HEADER_SIZE: usize = 48;
const MAX_ACPI_TABLE_SIZE: usize = 1024 * 1024;
const MAX_IOMMU_UNITS: usize = 16;
const MAX_UNIT_SCOPES: usize = 8;
const MAX_RESERVED_MAPPINGS: usize = 32;
const INTEL_TABLE_PAGES: usize = 96;
const AMD_DEVICE_TABLE_PAGES: usize = 512;
const DOMAIN_TABLE_PAGES: usize = 96;
const AMD_COMMAND_BUFFER_PAGES: usize = 2;
const MAX_DOMAIN_MAPPINGS: usize = 8;
const INTEL_VERSION: u64 = 0x00;
const INTEL_CAPABILITY: u64 = 0x08;
const INTEL_EXTENDED_CAPABILITY: u64 = 0x10;
const INTEL_GLOBAL_COMMAND: u64 = 0x18;
const INTEL_GLOBAL_STATUS: u64 = 0x1c;
const INTEL_ROOT_TABLE_ADDRESS: u64 = 0x20;
const INTEL_CONTEXT_COMMAND: u64 = 0x28;
const INTEL_FAULT_STATUS: u64 = 0x34;
const INTEL_PROTECTED_MEMORY_ENABLE: u64 = 0x64;
const INTEL_TRANSLATION_ENABLE: u32 = 1 << 31;
const INTEL_SET_ROOT_POINTER: u32 = 1 << 30;
const INTEL_WRITE_BUFFER_FLUSH: u32 = 1 << 27;
const INTEL_QUEUED_INVALIDATION_ENABLE: u32 = 1 << 26;
const INTEL_INTERRUPT_REMAP_ENABLE: u32 = 1 << 25;
const INTEL_PROTECTED_MEMORY_ENABLED: u32 = 1 << 31;
const INTEL_PROTECTED_MEMORY_STATUS: u32 = 1;
const INTEL_CAPABILITY_PROTECTED_MEMORY: u64 = (1 << 5) | (1 << 6);
const INTEL_CAPABILITY_WRITE_BUFFER_FLUSH: u64 = 1 << 4;
const INTEL_INVALIDATE_CONTEXT: u64 = 1 << 63;
const INTEL_CONTEXT_GLOBAL: u64 = 1 << 61;
const INTEL_INVALIDATE_IOTLB: u64 = 1 << 63;
const INTEL_IOTLB_GLOBAL: u64 = 1 << 60;
const AMD_DEVICE_TABLE_BASE: u64 = 0x00;
const AMD_COMMAND_BUFFER_BASE: u64 = 0x08;
const AMD_CONTROL: u64 = 0x18;
const AMD_COMMAND_BUFFER_HEAD: u64 = 0x2000;
const AMD_COMMAND_BUFFER_TAIL: u64 = 0x2008;
const AMD_IOMMU_ENABLE: u64 = 1;
const AMD_COMMAND_BUFFER_ENABLE: u64 = 1 << 12;
const AMD_DEVICE_TABLE_SEGMENT_ENABLE: u64 = 0b111 << 34;
const AMD_DEVICE_TABLE_SIZE: u64 = AMD_DEVICE_TABLE_PAGES as u64 - 1;
const AMD_COMMAND_BUFFER_SIZE: usize = AMD_COMMAND_BUFFER_PAGES * 4096;
const AMD_COMMAND_BUFFER_ENCODING: u64 = 0x9 << 56;
const AMD_DTE_VALID: u64 = 1;
const AMD_DTE_TRANSLATION_VALID: u64 = 1 << 1;
const AMD_DTE_MODE_4_LEVEL: u64 = 4 << 9;
const AMD_DTE_READ_WRITE: u64 = (1 << 61) | (1 << 62);
const AMD_PTE_PRESENT: u64 = 1;
const AMD_PTE_READ_WRITE: u64 = (1 << 61) | (1 << 62);
const AMD_COMMAND_COMPLETION_WAIT: u32 = 1;
const AMD_COMMAND_INVALIDATE_DTE: u32 = 2;
const AMD_COMMAND_INVALIDATE_PAGES: u32 = 3;
const AMD_INVALIDATE_ALL_PAGES: u64 = 0x7fff_ffff_ffff_f000 | 3;
const REGISTER_WAIT_LIMIT: usize = 1_000_000;
const INTEL_ENABLE_WAIT_LIMIT: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IommuKind {
    IntelVtd,
    AmdVi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IommuUnit {
    pub segment: u16,
    pub register_base: u64,
    pub include_all: bool,
    scope_requesters: [u16; MAX_UNIT_SCOPES],
    scope_count: usize,
}

impl IommuUnit {
    pub fn covers_requester(&self, requester: u16) -> bool {
        self.scope_requesters[..self.scope_count].contains(&requester)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReservedMapping {
    pub segment: u16,
    pub requester: u16,
    pub base: u64,
    pub limit: u64,
}

const EMPTY_RESERVED_MAPPING: ReservedMapping = ReservedMapping {
    segment: 0,
    requester: 0,
    base: 0,
    limit: 0,
};

const EMPTY_UNIT: IommuUnit = IommuUnit {
    segment: 0,
    register_base: 0,
    include_all: false,
    scope_requesters: [0; MAX_UNIT_SCOPES],
    scope_count: 0,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IommuTopology {
    kind: IommuKind,
    units: [IommuUnit; MAX_IOMMU_UNITS],
    unit_count: usize,
    reserved_mappings: [ReservedMapping; MAX_RESERVED_MAPPINGS],
    reserved_mapping_count: usize,
}

impl IommuTopology {
    const fn new(kind: IommuKind) -> Self {
        Self {
            kind,
            units: [EMPTY_UNIT; MAX_IOMMU_UNITS],
            unit_count: 0,
            reserved_mappings: [EMPTY_RESERVED_MAPPING; MAX_RESERVED_MAPPINGS],
            reserved_mapping_count: 0,
        }
    }

    pub const fn kind(&self) -> IommuKind {
        self.kind
    }

    pub const fn unit_count(&self) -> usize {
        self.unit_count
    }

    pub fn units(&self) -> &[IommuUnit] {
        &self.units[..self.unit_count]
    }

    pub fn reserved_mappings(&self) -> &[ReservedMapping] {
        &self.reserved_mappings[..self.reserved_mapping_count]
    }

    /// Returns whether one remapping unit is authoritative for a requester.
    /// Explicit VT-d scopes take precedence over an include-all unit.
    fn unit_handles_requester(&self, index: usize, segment: u16, requester: u16) -> bool {
        let unit = self.units[index];
        if unit.segment != segment {
            return false;
        }
        if self.kind == IommuKind::AmdVi {
            return true;
        }
        let has_explicit = self.units().iter().any(|candidate| {
            candidate.segment == segment && candidate.covers_requester(requester)
        });
        if has_explicit {
            unit.covers_requester(requester)
        } else {
            unit.include_all
        }
    }

    pub fn covers_requester(&self, segment: u16, requester: u16) -> bool {
        (0..self.unit_count)
            .any(|index| self.unit_handles_requester(index, segment, requester))
    }

    fn push(&mut self, unit: IommuUnit) -> Result<(), Error> {
        if self.units[..self.unit_count]
            .iter()
            .any(|known| known.segment == unit.segment && known.register_base == unit.register_base)
        {
            return Ok(());
        }
        if self.unit_count == self.units.len() {
            return Err(Error::TooManyUnits);
        }
        if unit.register_base == 0 || unit.register_base & 0xfff != 0 {
            return Err(Error::InvalidTable);
        }
        self.units[self.unit_count] = unit;
        self.unit_count += 1;
        Ok(())
    }
    fn push_reserved_mapping(&mut self, mapping: ReservedMapping) -> Result<(), Error> {
        if self.reserved_mapping_count == self.reserved_mappings.len() {
            return Err(Error::TooManyReservedMappings);
        }
        self.reserved_mappings[self.reserved_mapping_count] = mapping;
        self.reserved_mapping_count += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidRsdp,
    InvalidTable,
    TooManyUnits,
    TooManyReservedMappings,
    InvalidResources,
    UnsupportedHardware,
    RegisterWriteFailed,
    CommandTimeout,
    DeviceNotCovered,
    ReservedDevice,
    DomainIdUnavailable,
    TableExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntelTransitionStage {
    Tables,
    Disable,
    WriteBuffer,
    Root,
    Context,
    Iotlb,
    Enable {
        global_status: u32,
        fault_status: u32,
        root_table: u64,
    },
    ProtectedMemory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IommuResources {
    pub remapping_table: u64,
    pub domain_tables: u64,
    pub command_buffer: u64,
    pub completion: u64,
}

impl IommuResources {
    pub const fn required_pages(kind: IommuKind) -> (usize, usize, usize, usize) {
        match kind {
            IommuKind::IntelVtd => (INTEL_TABLE_PAGES, 0, 0, 0),
            IommuKind::AmdVi => (
                AMD_DEVICE_TABLE_PAGES,
                DOMAIN_TABLE_PAGES,
                AMD_COMMAND_BUFFER_PAGES,
                1,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DomainMapping {
    domain_id: u16,
    root: u64,
    host_base: u64,
    size: u64,
}

const EMPTY_DOMAIN_MAPPING: DomainMapping = DomainMapping {
    domain_id: 0,
    root: 0,
    host_base: 0,
    size: 0,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnitRuntime {
    resources: IommuResources,
    enabled: bool,
    next_domain_page: usize,
    command_tail: usize,
    completion_value: u64,
    domains: [DomainMapping; MAX_DOMAIN_MAPPINGS],
}

const EMPTY_RESOURCES: IommuResources = IommuResources {
    remapping_table: 0,
    domain_tables: 0,
    command_buffer: 0,
    completion: 0,
};

const EMPTY_UNIT_RUNTIME: UnitRuntime = UnitRuntime {
    resources: EMPTY_RESOURCES,
    enabled: false,
    next_domain_page: 0,
    command_tail: 0,
    completion_value: 0,
    domains: [EMPTY_DOMAIN_MAPPING; MAX_DOMAIN_MAPPINGS],
};

pub struct DmaRemapper {
    topology: IommuTopology,
    units: [UnitRuntime; MAX_IOMMU_UNITS],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntelRegisterSnapshot {
    pub global_status: u32,
    pub fault_status: u32,
    pub root_table: u64,
}

pub const fn deny_all_table_pages(kind: IommuKind) -> usize {
    match kind {
        IommuKind::IntelVtd => INTEL_TABLE_PAGES,
        IommuKind::AmdVi => AMD_DEVICE_TABLE_PAGES,
    }
}

impl DmaRemapper {
    /// Captures the small VT-d register set needed to diagnose a failed
    /// requester transition. The values are read only and do not acknowledge
    /// or clear faults.
    ///
    /// # Safety
    /// IOMMU MMIO must remain mapped and exclusively owned by mBoot.
    pub unsafe fn intel_register_snapshot(
        &self,
        segment: u16,
        requester: u16,
    ) -> Option<IntelRegisterSnapshot> {
        if self.topology.kind != IommuKind::IntelVtd {
            return None;
        }
        let index = (0..self.topology.unit_count)
            .find(|index| self.unit_handles(*index, segment, requester))?;
        let base = self.topology.units[index].register_base;
        Some(IntelRegisterSnapshot {
            global_status: unsafe { mmio_read_u32(base, INTEL_GLOBAL_STATUS) },
            fault_status: unsafe { mmio_read_u32(base, INTEL_FAULT_STATUS) },
            root_table: unsafe { mmio_read_u64(base, INTEL_ROOT_TABLE_ADDRESS) },
        })
    }

    /// Installs deny-by-default tables and takes ownership of every described
    /// remapping unit except a firmware display unit that was explicitly deferred.
    ///
    /// # Safety
    /// Every resource range must contain the zeroed, contiguous pages reported by
    /// [`IommuResources::required_pages`]. IOMMU MMIO must remain identity-mapped,
    /// and PCI bus mastering must already be disabled.
    pub unsafe fn initialize(
        topology: IommuTopology,
        resources: &[IommuResources],
        deferred_requester: Option<u16>,
    ) -> Result<Self, Error> {
        if resources.len() != topology.unit_count {
            return Err(Error::InvalidResources);
        }
        let mut remapper = Self {
            topology,
            units: [EMPTY_UNIT_RUNTIME; MAX_IOMMU_UNITS],
        };
        for (index, (unit, resource)) in topology
            .units()
            .iter()
            .copied()
            .zip(resources.iter().copied())
            .enumerate()
        {
            validate_resources(topology.kind, resource)?;
            let deferred = topology.kind == IommuKind::IntelVtd
                && deferred_requester.is_some_and(|requester| {
                    topology.unit_handles_requester(index, 0, requester)
                });
            if deferred {
                remapper.units[index].resources = resource;
                continue;
            }
            let next_domain_page = match topology.kind {
                IommuKind::IntelVtd => unsafe {
                    enable_intel_protection(
                        &unit,
                        resource.remapping_table,
                        topology.reserved_mappings(),
                    )?
                },
                IommuKind::AmdVi => {
                    unsafe { enable_amd_protection(&unit, resource)? };
                    0
                }
            };
            remapper.units[index] = UnitRuntime {
                resources: resource,
                enabled: true,
                next_domain_page,
                command_tail: 0,
                completion_value: 0,
                domains: [EMPTY_DOMAIN_MAPPING; MAX_DOMAIN_MAPPINGS],
            };
        }
        Ok(remapper)
    }

    /// Connects one requester to a Domain IOVA space while PCI bus mastering
    /// remains disabled. IOVA zero maps to the first byte of Domain RAM.
    ///
    /// # Safety
    /// Domain RAM must remain exclusively owned at `host_base..host_base+size`,
    /// and this remapper must retain exclusive access to its tables and MMIO.
    pub unsafe fn assign(
        &mut self,
        segment: u16,
        requester: u16,
        domain_id: u32,
        host_base: u64,
        size: u64,
    ) -> Result<(), Error> {
        let domain_id = u16::try_from(domain_id)
            .ok()
            .filter(|domain_id| *domain_id != 0)
            .ok_or(Error::DomainIdUnavailable)?;
        if requester == 0
            || host_base == 0
            || host_base & 0xfff != 0
            || size == 0
            || size & 0xfff != 0
            || host_base.checked_add(size).is_none()
        {
            return Err(Error::InvalidResources);
        }
        let mut matched = false;
        for index in 0..self.topology.unit_count {
            if !self.unit_handles(index, segment, requester) {
                continue;
            }
            matched = true;
            let root = match ensure_domain_mapping(
                self.topology.kind,
                &mut self.units[index],
                domain_id,
                host_base,
                size,
            ) {
                Ok(root) => root,
                Err(error) => {
                    // A requester can be covered by more than one AMD unit.
                    // Undo any earlier attachment before reporting failure.
                    let _ = unsafe { self.detach(segment, requester, u32::from(domain_id)) };
                    return Err(error);
                }
            };
            // Intel firmware can reserve a small identity-mapped DMA range for
            // an integrated display controller. Preserve only the ranges that
            // DMAR associates with this exact requester. They must not overlap
            // the Domain's normal IOVA space, which starts at zero.
            if self.topology.kind == IommuKind::IntelVtd {
                let mut has_reserved_mapping = false;
                for mapping in self.topology.reserved_mappings().iter().copied() {
                    if mapping.segment != segment || mapping.requester != requester {
                        continue;
                    }
                    has_reserved_mapping = true;
                    if mapping.base < size {
                        let _ = unsafe {
                            self.detach(segment, requester, u32::from(domain_id))
                        };
                        return Err(Error::ReservedDevice);
                    }
                    if let Err(error) = unsafe {
                        map_domain_reserved_identity(
                            &mut self.units[index],
                            root,
                            mapping.base,
                            mapping.limit,
                        )
                    } {
                        let _ = unsafe {
                            self.detach(segment, requester, u32::from(domain_id))
                        };
                        return Err(error);
                    }
                }
                if has_reserved_mapping {
                    // The deny-by-default table initially keeps this requester
                    // in reserved Domain 1 so firmware DMA remains valid. Bus
                    // mastering is disabled here, so it is safe to remove that
                    // temporary context before installing the Hardware Domain
                    // context below.
                    if let Err(error) = unsafe {
                        detach_intel_requester(
                            &self.topology.units[index],
                            &mut self.units[index],
                            requester,
                            1,
                        )
                    } {
                        return Err(error);
                    }
                }
            }
            let result = unsafe {
                match self.topology.kind {
                    IommuKind::IntelVtd => attach_intel_requester(
                        &self.topology.units[index],
                        &mut self.units[index],
                        requester,
                        domain_id,
                        root,
                    ),
                    IommuKind::AmdVi => attach_amd_requester(
                        &self.topology.units[index],
                        &mut self.units[index],
                        requester,
                        domain_id,
                        root,
                    ),
                }
            };
            if let Err(error) = result {
                let _ = unsafe { self.detach(segment, requester, u32::from(domain_id)) };
                return Err(error);
            }
        }
        if matched {
            Ok(())
        } else {
            Err(Error::DeviceNotCovered)
        }
    }

    /// Replaces the firmware VT-d state kept for the boot display with mBoot's
    /// deny-by-default table. The display function must have memory decoding and
    /// bus mastering disabled before this transition.
    ///
    /// # Safety
    /// PCI configuration and IOMMU MMIO must be exclusively owned by mBoot. No
    /// code may access the firmware framebuffer after this call begins.
    pub unsafe fn take_over_deferred_display(
        &mut self,
        segment: u16,
        requester: u16,
        mut progress: impl FnMut(IntelTransitionStage),
    ) -> Result<bool, Error> {
        if self.topology.kind != IommuKind::IntelVtd || requester == 0 {
            return Ok(false);
        }
        let mut matched = false;
        let mut changed = false;
        for index in 0..self.topology.unit_count {
            let unit = self.topology.units[index];
            if !self
                .topology
                .unit_handles_requester(index, segment, requester)
            {
                continue;
            }
            matched = true;
            if self.units[index].enabled {
                continue;
            }
            let resources = self.units[index].resources;
            let status = unsafe { mmio_read_u32(unit.register_base, INTEL_GLOBAL_STATUS) };
            let next_domain_page = if status & INTEL_TRANSLATION_ENABLE != 0 {
                unsafe {
                    replace_intel_protection(
                        &unit,
                        resources.remapping_table,
                        self.topology.reserved_mappings(),
                        &mut progress,
                    )?
                }
            } else {
                unsafe {
                    enable_intel_protection_with_progress(
                        &unit,
                        resources.remapping_table,
                        self.topology.reserved_mappings(),
                        &mut progress,
                    )?
                }
            };
            self.units[index] = UnitRuntime {
                resources,
                enabled: true,
                next_domain_page,
                command_tail: 0,
                completion_value: 0,
                domains: [EMPTY_DOMAIN_MAPPING; MAX_DOMAIN_MAPPINGS],
            };
            changed = true;
        }
        if matched {
            Ok(changed)
        } else {
            Err(Error::DeviceNotCovered)
        }
    }

    /// Restores a requester to deny-all and invalidates cached translations.
    ///
    /// # Safety
    /// PCI bus mastering must be disabled before this function is called.
    pub unsafe fn detach(
        &mut self,
        segment: u16,
        requester: u16,
        domain_id: u32,
    ) -> Result<(), Error> {
        let domain_id = u16::try_from(domain_id)
            .ok()
            .filter(|domain_id| *domain_id != 0)
            .ok_or(Error::DomainIdUnavailable)?;
        let mut matched = false;
        for index in 0..self.topology.unit_count {
            if !self.unit_handles(index, segment, requester) {
                continue;
            }
            matched = true;
            unsafe {
                match self.topology.kind {
                    IommuKind::IntelVtd => detach_intel_requester(
                        &self.topology.units[index],
                        &mut self.units[index],
                        requester,
                        domain_id,
                    )?,
                    IommuKind::AmdVi => detach_amd_requester(
                        &self.topology.units[index],
                        &mut self.units[index],
                        requester,
                        domain_id,
                    )?,
                }
            }
        }
        if matched {
            Ok(())
        } else {
            Err(Error::DeviceNotCovered)
        }
    }

    fn unit_handles(&self, index: usize, segment: u16, requester: u16) -> bool {
        if !self.units[index].enabled {
            return false;
        }
        self.topology
            .unit_handles_requester(index, segment, requester)
    }
}

unsafe fn map_domain_reserved_identity(
    runtime: &mut UnitRuntime,
    root: u64,
    base: u64,
    limit: u64,
) -> Result<(), Error> {
    if base & 0xfff != 0 || limit & 0xfff != 0xfff || limit < base {
        return Err(Error::InvalidResources);
    }
    let end = limit.checked_add(1).ok_or(Error::InvalidResources)?;
    let mut arena = TableArena {
        base: runtime.resources.remapping_table,
        next_page: runtime.next_domain_page,
        page_count: INTEL_TABLE_PAGES,
    };
    unsafe { identity_map_intel(root, base, end, &mut arena)? };
    runtime.next_domain_page = arena.next_page;
    Ok(())
}

fn validate_resources(kind: IommuKind, resources: IommuResources) -> Result<(), Error> {
    let aligned = |address: u64| address != 0 && address & 0xfff == 0;
    if !aligned(resources.remapping_table) {
        return Err(Error::InvalidResources);
    }
    match kind {
        IommuKind::IntelVtd
            if resources.domain_tables == 0
                && resources.command_buffer == 0
                && resources.completion == 0 =>
        {
            Ok(())
        }
        IommuKind::AmdVi
            if aligned(resources.domain_tables)
                && aligned(resources.command_buffer)
                && aligned(resources.completion) =>
        {
            Ok(())
        }
        _ => Err(Error::InvalidResources),
    }
}

/// Enables DMA remapping with no valid device mappings.
///
/// # Safety
/// Each table address must name the zeroed, contiguous number of pages returned
/// by [`deny_all_table_pages`]. IOMMU MMIO ranges must remain identity-mapped,
/// PCI bus mastering must be disabled except for requesters covered by a parsed
/// reserved mapping, and no other agent may program the units.
pub unsafe fn enable_deny_all(
    topology: &IommuTopology,
    table_addresses: &[u64],
    deferred_requester: Option<u16>,
) -> Result<(), Error> {
    if table_addresses.len() != topology.unit_count {
        return Err(Error::InvalidResources);
    }
    for (index, (unit, table_address)) in topology
        .units()
        .iter()
        .zip(table_addresses)
        .enumerate()
    {
        if topology.kind == IommuKind::IntelVtd
            && deferred_requester
                .is_some_and(|requester| topology.unit_handles_requester(index, 0, requester))
        {
            continue;
        }
        if unit.segment != 0 || *table_address == 0 || *table_address & 0xfff != 0 {
            return Err(Error::InvalidResources);
        }
        // SAFETY: The public function contract grants exclusive access to the
        // matching remapping unit and its correctly sized zeroed table.
        unsafe {
            match topology.kind {
                IommuKind::IntelVtd => {
                    let _ = enable_intel_protection(
                        unit,
                        *table_address,
                        topology.reserved_mappings(),
                    )?;
                }
                IommuKind::AmdVi => enable_amd_deny_all(unit, *table_address)?,
            }
        }
    }
    Ok(())
}

unsafe fn enable_intel_protection(
    unit: &IommuUnit,
    root_table: u64,
    mappings: &[ReservedMapping],
) -> Result<usize, Error> {
    unsafe { enable_intel_protection_with_progress(unit, root_table, mappings, &mut |_| {}) }
}

unsafe fn enable_intel_protection_with_progress(
    unit: &IommuUnit,
    root_table: u64,
    mappings: &[ReservedMapping],
    progress: &mut impl FnMut(IntelTransitionStage),
) -> Result<usize, Error> {
    if root_table >> 52 != 0 {
        return Err(Error::InvalidResources);
    }
    // SAFETY: The caller guarantees that this unit's MMIO range is mapped.
    let version = unsafe { mmio_read_u32(unit.register_base, INTEL_VERSION) };
    // Legacy root/context translation requires at least one supported adjusted
    // guest-address width in CAP.SAGAW.
    // SAFETY: The capability register belongs to the same mapped unit.
    let capability = unsafe { mmio_read_u64(unit.register_base, INTEL_CAPABILITY) };
    if version == 0 || capability >> 8 & 0x04 == 0 {
        return Err(Error::UnsupportedHardware);
    }

    // SAFETY: The caller supplied INTEL_TABLE_PAGES zeroed contiguous pages.
    progress(IntelTransitionStage::Tables);
    let next_page = unsafe { build_intel_tables(root_table, mappings)? };

    // Start from a known state. Requesters without an RMRR are unable to issue
    // new DMA; firmware-reserved requesters are immediately restored below.
    // SAFETY: GCMD is the command register of this exclusively owned unit.
    progress(IntelTransitionStage::Disable);
    unsafe { mmio_write_u32(unit.register_base, INTEL_GLOBAL_COMMAND, 0) };
    // SAFETY: GSTS is readable while the unit processes the disable command.
    unsafe {
        wait_intel_status(unit.register_base, INTEL_TRANSLATION_ENABLE, false)?;
        flush_intel_write_buffer(unit, capability, 0, progress)?;
        progress(IntelTransitionStage::Root);
        mmio_write_u64(unit.register_base, INTEL_ROOT_TABLE_ADDRESS, root_table);
        if mmio_read_u64(unit.register_base, INTEL_ROOT_TABLE_ADDRESS) & !0xfff != root_table {
            return Err(Error::RegisterWriteFailed);
        }
        dma_table_fence();
        mmio_write_u32(
            unit.register_base,
            INTEL_GLOBAL_COMMAND,
            INTEL_SET_ROOT_POINTER,
        );
        wait_intel_status(unit.register_base, INTEL_SET_ROOT_POINTER, true)?;
        invalidate_intel_caches_with_progress(unit, progress)?;
        progress(IntelTransitionStage::Enable {
            global_status: mmio_read_u32(unit.register_base, INTEL_GLOBAL_STATUS),
            fault_status: mmio_read_u32(unit.register_base, INTEL_FAULT_STATUS),
            root_table: mmio_read_u64(unit.register_base, INTEL_ROOT_TABLE_ADDRESS),
        });
        mmio_write_u32(
            unit.register_base,
            INTEL_GLOBAL_COMMAND,
            INTEL_TRANSLATION_ENABLE,
        );
        wait_intel_status_with_limit(
            unit.register_base,
            INTEL_TRANSLATION_ENABLE,
            true,
            INTEL_ENABLE_WAIT_LIMIT,
        )?;
        disable_intel_protected_memory(unit, capability, progress)?;
    }
    Ok(next_page)
}

unsafe fn replace_intel_protection(
    unit: &IommuUnit,
    root_table: u64,
    mappings: &[ReservedMapping],
    progress: &mut impl FnMut(IntelTransitionStage),
) -> Result<usize, Error> {
    if root_table >> 52 != 0 {
        return Err(Error::InvalidResources);
    }
    let version = unsafe { mmio_read_u32(unit.register_base, INTEL_VERSION) };
    let capability = unsafe { mmio_read_u64(unit.register_base, INTEL_CAPABILITY) };
    if version == 0 || capability >> 8 & 0x04 == 0 {
        return Err(Error::UnsupportedHardware);
    }

    progress(IntelTransitionStage::Tables);
    let next_page = unsafe { build_intel_tables(root_table, mappings)? };

    // Firmware may leave translation, queued invalidation, and interrupt
    // remapping active on the display unit. Keep translation enabled throughout
    // the handoff, but stop the two optional engines before using register-based
    // invalidation. Non-reserved PCI bus masters are disabled, and the new root
    // reproduces every firmware-reserved requester mapping before the switch.
    progress(IntelTransitionStage::Disable);
    unsafe {
        mmio_write_u32(
            unit.register_base,
            INTEL_GLOBAL_COMMAND,
            INTEL_TRANSLATION_ENABLE,
        );
        wait_intel_status(
            unit.register_base,
            INTEL_QUEUED_INVALIDATION_ENABLE,
            false,
        )?;
        wait_intel_status(
            unit.register_base,
            INTEL_INTERRUPT_REMAP_ENABLE,
            false,
        )?;
        flush_intel_write_buffer(
            unit,
            capability,
            INTEL_TRANSLATION_ENABLE,
            progress,
        )?;

        progress(IntelTransitionStage::Root);
        mmio_write_u64(unit.register_base, INTEL_ROOT_TABLE_ADDRESS, root_table);
        if mmio_read_u64(unit.register_base, INTEL_ROOT_TABLE_ADDRESS) & !0xfff != root_table {
            return Err(Error::RegisterWriteFailed);
        }
        dma_table_fence();
        mmio_write_u32(
            unit.register_base,
            INTEL_GLOBAL_COMMAND,
            INTEL_TRANSLATION_ENABLE | INTEL_SET_ROOT_POINTER,
        );
        wait_intel_status(unit.register_base, INTEL_SET_ROOT_POINTER, true)?;
        invalidate_intel_caches_with_progress(unit, progress)?;
        disable_intel_protected_memory(unit, capability, progress)?;
    }
    Ok(next_page)
}

unsafe fn flush_intel_write_buffer(
    unit: &IommuUnit,
    capability: u64,
    active_commands: u32,
    progress: &mut impl FnMut(IntelTransitionStage),
) -> Result<(), Error> {
    if capability & INTEL_CAPABILITY_WRITE_BUFFER_FLUSH == 0 {
        return Ok(());
    }
    progress(IntelTransitionStage::WriteBuffer);
    unsafe {
        mmio_write_u32(
            unit.register_base,
            INTEL_GLOBAL_COMMAND,
            active_commands | INTEL_WRITE_BUFFER_FLUSH,
        );
        wait_intel_status(unit.register_base, INTEL_WRITE_BUFFER_FLUSH, false)
    }
}

unsafe fn disable_intel_protected_memory(
    unit: &IommuUnit,
    capability: u64,
    progress: &mut impl FnMut(IntelTransitionStage),
) -> Result<(), Error> {
    if capability & INTEL_CAPABILITY_PROTECTED_MEMORY == 0 {
        return Ok(());
    }
    progress(IntelTransitionStage::ProtectedMemory);
    let value = unsafe { mmio_read_u32(unit.register_base, INTEL_PROTECTED_MEMORY_ENABLE) };
    unsafe {
        mmio_write_u32(
            unit.register_base,
            INTEL_PROTECTED_MEMORY_ENABLE,
            value & !INTEL_PROTECTED_MEMORY_ENABLED,
        );
    }
    for _ in 0..REGISTER_WAIT_LIMIT {
        if unsafe { mmio_read_u32(unit.register_base, INTEL_PROTECTED_MEMORY_ENABLE) }
            & INTEL_PROTECTED_MEMORY_STATUS
            == 0
        {
            return Ok(());
        }
        spin_loop();
    }
    Err(Error::CommandTimeout)
}

unsafe fn invalidate_intel_caches(unit: &IommuUnit) -> Result<(), Error> {
    unsafe { invalidate_intel_caches_with_progress(unit, &mut |_| {}) }
}

unsafe fn invalidate_intel_caches_with_progress(
    unit: &IommuUnit,
    progress: &mut impl FnMut(IntelTransitionStage),
) -> Result<(), Error> {
    unsafe {
        progress(IntelTransitionStage::Context);
        mmio_write_u64(
            unit.register_base,
            INTEL_CONTEXT_COMMAND,
            INTEL_INVALIDATE_CONTEXT | INTEL_CONTEXT_GLOBAL,
        );
        wait_u64_clear(
            unit.register_base,
            INTEL_CONTEXT_COMMAND,
            INTEL_INVALIDATE_CONTEXT,
        )?;
        let extended = mmio_read_u64(unit.register_base, INTEL_EXTENDED_CAPABILITY);
        let iotlb_offset = (extended >> 8 & 0x3ff) * 16;
        if iotlb_offset == 0 {
            return Err(Error::UnsupportedHardware);
        }
        progress(IntelTransitionStage::Iotlb);
        let command = iotlb_offset + 8;
        mmio_write_u64(
            unit.register_base,
            command,
            INTEL_INVALIDATE_IOTLB | INTEL_IOTLB_GLOBAL,
        );
        wait_u64_clear(unit.register_base, command, INTEL_INVALIDATE_IOTLB)?;
    }
    Ok(())
}

struct TableArena {
    base: u64,
    next_page: usize,
    page_count: usize,
}

impl TableArena {
    unsafe fn allocate(&mut self) -> Result<u64, Error> {
        if self.next_page == self.page_count {
            return Err(Error::TableExhausted);
        }
        let address = self.base + self.next_page as u64 * 4096;
        self.next_page += 1;
        unsafe { write_bytes(address as *mut u8, 0, 4096) };
        Ok(address)
    }
}

unsafe fn build_intel_tables(
    root_table: u64,
    mappings: &[ReservedMapping],
) -> Result<usize, Error> {
    let mut arena = TableArena {
        base: root_table,
        next_page: 1,
        page_count: INTEL_TABLE_PAGES,
    };
    for mapping in mappings.iter().filter(|mapping| mapping.segment == 0) {
        if mapping.base & 0xfff != 0
            || mapping.limit & 0xfff != 0xfff
            || mapping.limit < mapping.base
            || mapping.limit >> 52 != 0
        {
            return Err(Error::InvalidTable);
        }
        let bus = usize::from(mapping.requester >> 8);
        let device_function = usize::from(mapping.requester & 0xff);
        // SAFETY: Root and context entries are inside the caller-owned arena.
        let context_table = unsafe { ensure_root_entry(root_table, bus, &mut arena)? };
        let context_low = (context_table + device_function as u64 * 16) as *mut u64;
        // SAFETY: A PCI requester indexes one of 256 16-byte context entries.
        let mut second_level = unsafe { read_volatile(context_low) } & !0xfff;
        if second_level == 0 {
            // SAFETY: The arena returns a fresh zeroed page.
            second_level = unsafe { arena.allocate()? };
            // Present, multi-level translation. The upper word selects a
            // 48-bit adjusted guest-address width and Domain ID 1.
            unsafe {
                write_volatile(context_low.add(1), (1_u64 << 8) | 2);
                write_volatile(context_low, second_level | 1);
            }
        }
        // SAFETY: The second-level root and all children belong to this arena.
        unsafe { identity_map_intel(second_level, mapping.base, mapping.limit + 1, &mut arena)? };
    }
    Ok(arena.next_page)
}

unsafe fn ensure_root_entry(
    root_table: u64,
    bus: usize,
    arena: &mut TableArena,
) -> Result<u64, Error> {
    let root_low = (root_table + bus as u64 * 16) as *mut u64;
    // SAFETY: A PCI bus indexes one of 256 16-byte root entries.
    let mut context_table = unsafe { read_volatile(root_low) } & !0xfff;
    if context_table == 0 {
        // SAFETY: The arena returns a fresh zeroed page.
        context_table = unsafe { arena.allocate()? };
        // SAFETY: The root entry is within the first arena page.
        unsafe { write_volatile(root_low, context_table | 1) };
    }
    Ok(context_table)
}

unsafe fn identity_map_intel(
    level4: u64,
    mut address: u64,
    end: u64,
    arena: &mut TableArena,
) -> Result<(), Error> {
    while address < end {
        let level3 = unsafe { ensure_page_entry(level4, (address >> 39) & 0x1ff, arena)? };
        let level2 = unsafe { ensure_page_entry(level3, (address >> 30) & 0x1ff, arena)? };
        let level2_index = (address >> 21) & 0x1ff;
        let level2_entry = (level2 + level2_index * 8) as *mut u64;
        let remaining = end - address;
        // Use a 2 MiB second-level leaf for aligned interiors. RMRR edges that
        // are only 4 KiB aligned use a final page table.
        if address & 0x1f_ffff == 0 && remaining >= 0x20_0000 {
            // SAFETY: The entry is in the mapped level-two table.
            let current = unsafe { read_volatile(level2_entry) };
            if current == 0 || current & (1 << 7) != 0 {
                unsafe { write_volatile(level2_entry, address | 0x83) };
                address += 0x20_0000;
                continue;
            }
        }
        // SAFETY: The level-two entry either is empty or points to our page table.
        let level1 = unsafe { ensure_page_entry(level2, level2_index, arena)? };
        let level1_entry = (level1 + ((address >> 12) & 0x1ff) * 8) as *mut u64;
        // SAFETY: The leaf entry is inside the level-one page table.
        unsafe { write_volatile(level1_entry, address | 3) };
        address += 4096;
    }
    Ok(())
}

unsafe fn ensure_page_entry(table: u64, index: u64, arena: &mut TableArena) -> Result<u64, Error> {
    let entry = (table + index * 8) as *mut u64;
    // SAFETY: Every x86 page-table index is in 0..512.
    let mut child = unsafe { read_volatile(entry) } & 0x000f_ffff_ffff_f000;
    if child == 0 {
        // SAFETY: The arena returns a fresh zeroed page.
        child = unsafe { arena.allocate()? };
        // Read and write permissions are required at every second-level table.
        unsafe { write_volatile(entry, child | 3) };
    }
    Ok(child)
}

fn ensure_domain_mapping(
    kind: IommuKind,
    runtime: &mut UnitRuntime,
    domain_id: u16,
    host_base: u64,
    size: u64,
) -> Result<u64, Error> {
    if let Some(mapping) = runtime
        .domains
        .iter()
        .find(|mapping| mapping.domain_id == domain_id)
    {
        return if mapping.host_base == host_base && mapping.size == size {
            Ok(mapping.root)
        } else {
            Err(Error::InvalidResources)
        };
    }
    let slot = runtime
        .domains
        .iter()
        .position(|mapping| mapping.domain_id == 0)
        .ok_or(Error::TableExhausted)?;
    let (base, page_count) = match kind {
        IommuKind::IntelVtd => (runtime.resources.remapping_table, INTEL_TABLE_PAGES),
        IommuKind::AmdVi => (runtime.resources.domain_tables, DOMAIN_TABLE_PAGES),
    };
    let mut arena = TableArena {
        base,
        next_page: runtime.next_domain_page,
        page_count,
    };
    let root = unsafe { arena.allocate()? };
    unsafe { map_domain_memory(kind, root, host_base, size, &mut arena)? };
    runtime.next_domain_page = arena.next_page;
    runtime.domains[slot] = DomainMapping {
        domain_id,
        root,
        host_base,
        size,
    };
    Ok(root)
}

unsafe fn map_domain_memory(
    kind: IommuKind,
    level4: u64,
    host_base: u64,
    size: u64,
    arena: &mut TableArena,
) -> Result<(), Error> {
    let mut iova = 0_u64;
    while iova < size {
        let level3 =
            unsafe { ensure_dma_page_entry(kind, level4, (iova >> 39) & 0x1ff, 3, arena)? };
        let level2 =
            unsafe { ensure_dma_page_entry(kind, level3, (iova >> 30) & 0x1ff, 2, arena)? };
        let level1 =
            unsafe { ensure_dma_page_entry(kind, level2, (iova >> 21) & 0x1ff, 1, arena)? };
        let entry = (level1 + ((iova >> 12) & 0x1ff) * 8) as *mut u64;
        let host = host_base.checked_add(iova).ok_or(Error::InvalidResources)?;
        let flags = match kind {
            IommuKind::IntelVtd => 3,
            IommuKind::AmdVi => AMD_PTE_PRESENT | AMD_PTE_READ_WRITE,
        };
        unsafe { write_volatile(entry, host | flags) };
        iova += 4096;
    }
    Ok(())
}

unsafe fn ensure_dma_page_entry(
    kind: IommuKind,
    table: u64,
    index: u64,
    child_level: u64,
    arena: &mut TableArena,
) -> Result<u64, Error> {
    let entry = (table + index * 8) as *mut u64;
    let mut child = unsafe { read_volatile(entry) } & 0x000f_ffff_ffff_f000;
    if child == 0 {
        child = unsafe { arena.allocate()? };
        let flags = match kind {
            IommuKind::IntelVtd => 3,
            IommuKind::AmdVi => AMD_PTE_PRESENT | AMD_PTE_READ_WRITE | (child_level << 9),
        };
        unsafe { write_volatile(entry, child | flags) };
    }
    Ok(child)
}

unsafe fn attach_intel_requester(
    unit: &IommuUnit,
    runtime: &mut UnitRuntime,
    requester: u16,
    domain_id: u16,
    root: u64,
) -> Result<(), Error> {
    let mut arena = TableArena {
        base: runtime.resources.remapping_table,
        next_page: runtime.next_domain_page,
        page_count: INTEL_TABLE_PAGES,
    };
    let context_table = unsafe {
        ensure_root_entry(
            runtime.resources.remapping_table,
            usize::from(requester >> 8),
            &mut arena,
        )?
    };
    let context_low = (context_table + u64::from(requester & 0xff) * 16) as *mut u64;
    if unsafe { read_volatile(context_low) } & 1 != 0 {
        return Err(Error::InvalidResources);
    }
    runtime.next_domain_page = arena.next_page;
    unsafe {
        write_volatile(context_low.add(1), (u64::from(domain_id) << 8) | 2);
        dma_table_fence();
        write_volatile(context_low, root | 1);
        dma_table_fence();
        invalidate_intel_caches(unit)?;
    }
    Ok(())
}

unsafe fn detach_intel_requester(
    unit: &IommuUnit,
    runtime: &mut UnitRuntime,
    requester: u16,
    domain_id: u16,
) -> Result<(), Error> {
    let root_entry =
        (runtime.resources.remapping_table + u64::from(requester >> 8) * 16) as *const u64;
    let context_table = unsafe { read_volatile(root_entry) } & !0xfff;
    if context_table == 0 {
        return Ok(());
    }
    let context_low = (context_table + u64::from(requester & 0xff) * 16) as *mut u64;
    let low = unsafe { read_volatile(context_low) };
    if low & 1 == 0 {
        return Ok(());
    }
    let high = unsafe { read_volatile(context_low.add(1)) };
    if (high >> 8) as u16 != domain_id {
        return Err(Error::InvalidResources);
    }
    unsafe {
        write_volatile(context_low, 0);
        dma_table_fence();
        write_volatile(context_low.add(1), 0);
        dma_table_fence();
        invalidate_intel_caches(unit)?;
    }
    Ok(())
}

unsafe fn attach_amd_requester(
    unit: &IommuUnit,
    runtime: &mut UnitRuntime,
    requester: u16,
    domain_id: u16,
    root: u64,
) -> Result<(), Error> {
    let dte = (runtime.resources.remapping_table + u64::from(requester) * 32) as *mut u64;
    if unsafe { read_volatile(dte) } != 0 {
        return Err(Error::InvalidResources);
    }
    let entry = amd_dte(root, domain_id);
    unsafe {
        write_volatile(dte.add(3), entry[3]);
        write_volatile(dte.add(2), entry[2]);
        write_volatile(dte.add(1), entry[1]);
        dma_table_fence();
        write_volatile(dte, entry[0]);
        dma_table_fence();
        invalidate_amd_requester(unit, runtime, requester, domain_id)?;
    }
    Ok(())
}

fn amd_dte(root: u64, domain_id: u16) -> [u64; 4] {
    [
        root | AMD_DTE_VALID
            | AMD_DTE_TRANSLATION_VALID
            | AMD_DTE_MODE_4_LEVEL
            | AMD_DTE_READ_WRITE,
        u64::from(domain_id),
        0,
        0,
    ]
}

unsafe fn detach_amd_requester(
    unit: &IommuUnit,
    runtime: &mut UnitRuntime,
    requester: u16,
    domain_id: u16,
) -> Result<(), Error> {
    let dte = (runtime.resources.remapping_table + u64::from(requester) * 32) as *mut u64;
    let low = unsafe { read_volatile(dte) };
    if low & AMD_DTE_VALID == 0 {
        return Ok(());
    }
    if unsafe { read_volatile(dte.add(1)) } as u16 != domain_id {
        return Err(Error::InvalidResources);
    }
    unsafe {
        write_volatile(dte, 0);
        dma_table_fence();
        write_volatile(dte.add(1), 0);
        write_volatile(dte.add(2), 0);
        write_volatile(dte.add(3), 0);
        dma_table_fence();
        invalidate_amd_requester(unit, runtime, requester, domain_id)?;
    }
    Ok(())
}

unsafe fn enable_amd_deny_all(unit: &IommuUnit, device_table: u64) -> Result<(), Error> {
    if device_table >> 52 != 0 {
        return Err(Error::InvalidResources);
    }
    // Disable translation and table segmentation before replacing firmware
    // state with one invalid entry for every possible DeviceID.
    // SAFETY: The caller exclusively owns this mapped AMD-Vi MMIO range.
    unsafe { mmio_write_u64(unit.register_base, AMD_CONTROL, 0) };
    // SAFETY: Read-back detects locked or non-writable control registers.
    let disabled = unsafe { mmio_read_u64(unit.register_base, AMD_CONTROL) };
    if disabled & (AMD_IOMMU_ENABLE | AMD_DEVICE_TABLE_SEGMENT_ENABLE) != 0 {
        return Err(Error::RegisterWriteFailed);
    }
    let table_register = device_table | AMD_DEVICE_TABLE_SIZE;
    // SAFETY: The 2 MiB zeroed Device Table is contiguous and exclusively owned.
    unsafe {
        mmio_write_u64(unit.register_base, AMD_DEVICE_TABLE_BASE, table_register);
        dma_table_fence();
    }
    // SAFETY: Read-back verifies both the base and the 4 KiB size count.
    let installed = unsafe { mmio_read_u64(unit.register_base, AMD_DEVICE_TABLE_BASE) };
    if installed & 0x000f_ffff_ffff_f1ff != table_register {
        return Err(Error::RegisterWriteFailed);
    }
    // SAFETY: Bit zero enables translation with the installed invalid DTE table.
    unsafe { mmio_write_u64(unit.register_base, AMD_CONTROL, AMD_IOMMU_ENABLE) };
    // SAFETY: Read-back verifies that translation was accepted.
    if unsafe { mmio_read_u64(unit.register_base, AMD_CONTROL) } & AMD_IOMMU_ENABLE == 0 {
        return Err(Error::RegisterWriteFailed);
    }
    Ok(())
}

unsafe fn enable_amd_protection(unit: &IommuUnit, resources: IommuResources) -> Result<(), Error> {
    if resources.remapping_table >> 52 != 0
        || resources.domain_tables >> 52 != 0
        || resources.command_buffer >> 52 != 0
        || resources.completion >> 52 != 0
    {
        return Err(Error::InvalidResources);
    }
    unsafe { mmio_write_u64(unit.register_base, AMD_CONTROL, 0) };
    let disabled = unsafe { mmio_read_u64(unit.register_base, AMD_CONTROL) };
    if disabled & (AMD_IOMMU_ENABLE | AMD_DEVICE_TABLE_SEGMENT_ENABLE) != 0 {
        return Err(Error::RegisterWriteFailed);
    }
    let device_table = resources.remapping_table | AMD_DEVICE_TABLE_SIZE;
    let command_buffer = resources.command_buffer | AMD_COMMAND_BUFFER_ENCODING;
    unsafe {
        mmio_write_u64(unit.register_base, AMD_DEVICE_TABLE_BASE, device_table);
        mmio_write_u64(unit.register_base, AMD_COMMAND_BUFFER_BASE, command_buffer);
        dma_table_fence();
    }
    if unsafe { mmio_read_u64(unit.register_base, AMD_DEVICE_TABLE_BASE) } & 0x000f_ffff_ffff_f1ff
        != device_table
        || unsafe { mmio_read_u64(unit.register_base, AMD_COMMAND_BUFFER_BASE) }
            & 0x0fff_ffff_ffff_f000
            != command_buffer
    {
        return Err(Error::RegisterWriteFailed);
    }
    unsafe {
        mmio_write_u64(
            unit.register_base,
            AMD_CONTROL,
            AMD_IOMMU_ENABLE | AMD_COMMAND_BUFFER_ENABLE,
        )
    };
    let enabled = unsafe { mmio_read_u64(unit.register_base, AMD_CONTROL) };
    if enabled & (AMD_IOMMU_ENABLE | AMD_COMMAND_BUFFER_ENABLE)
        != AMD_IOMMU_ENABLE | AMD_COMMAND_BUFFER_ENABLE
    {
        return Err(Error::RegisterWriteFailed);
    }
    Ok(())
}

unsafe fn invalidate_amd_requester(
    unit: &IommuUnit,
    runtime: &mut UnitRuntime,
    requester: u16,
    domain_id: u16,
) -> Result<(), Error> {
    runtime.completion_value = runtime.completion_value.wrapping_add(1).max(1);
    let completion = runtime.completion_value;
    unsafe { write_volatile(runtime.resources.completion as *mut u64, 0) };
    let commands = amd_invalidation_commands(
        requester,
        domain_id,
        runtime.resources.completion,
        completion,
    );
    unsafe {
        for command in commands {
            queue_amd_command(unit, runtime, command)?;
        }
    }
    for _ in 0..REGISTER_WAIT_LIMIT {
        if unsafe { read_volatile(runtime.resources.completion as *const u64) } == completion {
            return Ok(());
        }
        spin_loop();
    }
    Err(Error::CommandTimeout)
}

fn amd_invalidation_commands(
    requester: u16,
    domain_id: u16,
    completion_address: u64,
    completion_value: u64,
) -> [[u32; 4]; 3] {
    [
        [u32::from(requester), AMD_COMMAND_INVALIDATE_DTE << 28, 0, 0],
        [
            0,
            (AMD_COMMAND_INVALIDATE_PAGES << 28) | u32::from(domain_id),
            AMD_INVALIDATE_ALL_PAGES as u32,
            (AMD_INVALIDATE_ALL_PAGES >> 32) as u32,
        ],
        [
            completion_address as u32 | 1,
            ((completion_address >> 32) as u32) | (AMD_COMMAND_COMPLETION_WAIT << 28),
            completion_value as u32,
            (completion_value >> 32) as u32,
        ],
    ]
}

unsafe fn queue_amd_command(
    unit: &IommuUnit,
    runtime: &mut UnitRuntime,
    command: [u32; 4],
) -> Result<(), Error> {
    let next = (runtime.command_tail + 16) % AMD_COMMAND_BUFFER_SIZE;
    for _ in 0..REGISTER_WAIT_LIMIT {
        let head = unsafe { mmio_read_u64(unit.register_base, AMD_COMMAND_BUFFER_HEAD) } as usize
            & (AMD_COMMAND_BUFFER_SIZE - 1);
        if next != head {
            let destination =
                (runtime.resources.command_buffer + runtime.command_tail as u64) as *mut u32;
            unsafe {
                for (index, value) in command.into_iter().enumerate() {
                    write_volatile(destination.add(index), value);
                }
                dma_table_fence();
                mmio_write_u64(unit.register_base, AMD_COMMAND_BUFFER_TAIL, next as u64);
            }
            runtime.command_tail = next;
            return Ok(());
        }
        spin_loop();
    }
    Err(Error::CommandTimeout)
}

unsafe fn wait_intel_status(base: u64, mask: u32, set: bool) -> Result<(), Error> {
    unsafe { wait_intel_status_with_limit(base, mask, set, REGISTER_WAIT_LIMIT) }
}

unsafe fn wait_intel_status_with_limit(
    base: u64,
    mask: u32,
    set: bool,
    limit: usize,
) -> Result<(), Error> {
    for _ in 0..limit {
        // SAFETY: The caller guarantees the Intel VT-d register range is mapped.
        let status = unsafe { mmio_read_u32(base, INTEL_GLOBAL_STATUS) };
        if (status & mask != 0) == set {
            return Ok(());
        }
        spin_loop();
    }
    Err(Error::CommandTimeout)
}

unsafe fn wait_u64_clear(base: u64, offset: u64, mask: u64) -> Result<(), Error> {
    for _ in 0..REGISTER_WAIT_LIMIT {
        if unsafe { mmio_read_u64(base, offset) } & mask == 0 {
            return Ok(());
        }
        spin_loop();
    }
    Err(Error::CommandTimeout)
}

unsafe fn mmio_read_u32(base: u64, offset: u64) -> u32 {
    // SAFETY: The caller provides a mapped register base and valid aligned offset.
    unsafe { read_volatile((base + offset) as *const u32) }
}

unsafe fn mmio_read_u64(base: u64, offset: u64) -> u64 {
    // SAFETY: The caller provides a mapped register base and valid aligned offset.
    unsafe { read_volatile((base + offset) as *const u64) }
}

unsafe fn mmio_write_u32(base: u64, offset: u64, value: u32) {
    // SAFETY: The caller provides a mapped register base and writable aligned offset.
    unsafe { write_volatile((base + offset) as *mut u32, value) };
}

unsafe fn mmio_write_u64(base: u64, offset: u64, value: u64) {
    // SAFETY: The caller provides a mapped register base and writable aligned offset.
    unsafe { write_volatile((base + offset) as *mut u64, value) };
}

unsafe fn dma_table_fence() {
    // SAFETY: MFENCE is available in x86_64 mode and serializes prior table writes
    // before the following MMIO command.
    unsafe { asm!("mfence", options(nostack, preserves_flags)) };
}

/// Finds and copies the IOMMU description from ACPI-owned memory.
///
/// # Safety
/// `rsdp_address` and every ACPI pointer reachable from it must remain
/// identity-mapped and readable for the duration of this call.
pub unsafe fn discover(rsdp_address: u64) -> Result<Option<IommuTopology>, Error> {
    // ACPI 1.0 defines a 20-byte RSDP. Read no further until its revision says
    // the extended ACPI 2.0 fields are present.
    let rsdp = unsafe { table_bytes(rsdp_address, 20)? };
    if &rsdp[..8] != b"RSD PTR " || !checksum_is_zero(rsdp) {
        return Err(Error::InvalidRsdp);
    }

    let (root_address, root_signature, entry_size) = if rsdp[15] >= 2 {
        let rsdp = unsafe { table_bytes(rsdp_address, 36)? };
        let rsdp_length = read_u32(rsdp, 20)? as usize;
        if !(36..=MAX_ACPI_TABLE_SIZE).contains(&rsdp_length) {
            return Err(Error::InvalidRsdp);
        }
        let rsdp = unsafe { table_bytes(rsdp_address, rsdp_length)? };
        if !checksum_is_zero(rsdp) {
            return Err(Error::InvalidRsdp);
        }
        let xsdt_address = read_u64(rsdp, 24)?;
        if xsdt_address != 0 {
            (xsdt_address, b"XSDT" as &[u8], 8)
        } else {
            (u64::from(read_u32(rsdp, 16)?), b"RSDT" as &[u8], 4)
        }
    } else {
        (u64::from(read_u32(rsdp, 16)?), b"RSDT" as &[u8], 4)
    };
    if root_address == 0 {
        return Err(Error::InvalidRsdp);
    }
    let root = unsafe { acpi_table(root_address)? };
    if &root[..4] != root_signature
        || !(root.len() - ACPI_HEADER_SIZE).is_multiple_of(entry_size)
    {
        return Err(Error::InvalidTable);
    }
    for entry in root[ACPI_HEADER_SIZE..].chunks_exact(entry_size) {
        let address = if entry_size == 8 {
            u64::from_le_bytes(entry.try_into().map_err(|_| Error::InvalidTable)?)
        } else {
            u64::from(u32::from_le_bytes(
                entry.try_into().map_err(|_| Error::InvalidTable)?,
            ))
        };
        // SAFETY: XSDT entries are firmware-provided physical pointers covered by
        // the discover contract.
        let header = unsafe { table_bytes(address, ACPI_HEADER_SIZE)? };
        if &header[..4] == b"DMAR" || &header[..4] == b"IVRS" {
            // SAFETY: The fixed header above validated that this pointer is readable.
            let table = unsafe { acpi_table(address)? };
            return if &table[..4] == b"DMAR" {
                parse_dmar(table).map(Some)
            } else {
                parse_ivrs(table).map(Some)
            };
        }
    }
    Ok(None)
}

pub fn parse_dmar(table: &[u8]) -> Result<IommuTopology, Error> {
    validate_acpi_table(table, b"DMAR", IOMMU_TABLE_HEADER_SIZE)?;
    let mut topology = IommuTopology::new(IommuKind::IntelVtd);
    walk_structures(
        &table[IOMMU_TABLE_HEADER_SIZE..],
        |kind, structure| match kind {
            0 => {
                if structure.len() < 16 {
                    return Err(Error::InvalidTable);
                }
                topology.push(IommuUnit {
                    segment: read_u16(structure, 6)?,
                    register_base: read_u64(structure, 8)?,
                    include_all: structure[4] & 1 != 0,
                    scope_requesters: parse_direct_scopes(&structure[16..])?,
                    scope_count: count_direct_scopes(&structure[16..])?,
                })
            }
            1 => parse_rmrr(structure, &mut topology),
            _ => Ok(()),
        },
    )?;
    if topology.unit_count == 0 {
        return Err(Error::InvalidTable);
    }
    Ok(topology)
}

fn parse_direct_scopes(scopes: &[u8]) -> Result<[u16; MAX_UNIT_SCOPES], Error> {
    let mut requesters = [0_u16; MAX_UNIT_SCOPES];
    let mut count = 0;
    walk_device_scopes(scopes, |requester| {
        if count == requesters.len() {
            return Err(Error::TooManyUnits);
        }
        requesters[count] = requester;
        count += 1;
        Ok(())
    })?;
    Ok(requesters)
}

fn count_direct_scopes(scopes: &[u8]) -> Result<usize, Error> {
    let mut count = 0;
    walk_device_scopes(scopes, |_| {
        count += 1;
        Ok(())
    })?;
    Ok(count)
}

fn walk_device_scopes(
    mut scopes: &[u8],
    mut visit: impl FnMut(u16) -> Result<(), Error>,
) -> Result<(), Error> {
    while !scopes.is_empty() {
        if scopes.len() < 6 {
            return Err(Error::InvalidTable);
        }
        let length = usize::from(scopes[1]);
        if length < 8 || length > scopes.len() || !(length - 6).is_multiple_of(2) {
            return Err(Error::InvalidTable);
        }
        if matches!(scopes[0], 1 | 2) && length == 8 {
            let bus = u16::from(scopes[5]);
            let device = u16::from(scopes[6]);
            let function = u16::from(scopes[7]);
            if device >= 32 || function >= 8 {
                return Err(Error::InvalidTable);
            }
            visit(bus << 8 | device << 3 | function)?;
        }
        scopes = &scopes[length..];
    }
    Ok(())
}

fn parse_rmrr(structure: &[u8], topology: &mut IommuTopology) -> Result<(), Error> {
    if structure.len() < 24 {
        return Err(Error::InvalidTable);
    }
    let segment = read_u16(structure, 6)?;
    let base = read_u64(structure, 8)?;
    let limit = read_u64(structure, 16)?;
    if base & 0xfff != 0 || limit & 0xfff != 0xfff || limit < base {
        return Err(Error::InvalidTable);
    }
    let mut scopes = &structure[24..];
    while !scopes.is_empty() {
        if scopes.len() < 6 {
            return Err(Error::InvalidTable);
        }
        let length = usize::from(scopes[1]);
        if length < 8 || length > scopes.len() || !(length - 6).is_multiple_of(2) {
            return Err(Error::InvalidTable);
        }
        // A direct endpoint scope is sufficient for integrated graphics and
        // other devices attached to the root bus. Multi-hop paths remain denied
        // until PCI bridge resolution is available.
        if scopes[0] == 1 && length == 8 {
            let bus = u16::from(scopes[5]);
            let device = u16::from(scopes[6]);
            let function = u16::from(scopes[7]);
            if device >= 32 || function >= 8 {
                return Err(Error::InvalidTable);
            }
            topology.push_reserved_mapping(ReservedMapping {
                segment,
                requester: bus << 8 | device << 3 | function,
                base,
                limit,
            })?;
        }
        scopes = &scopes[length..];
    }
    Ok(())
}

pub fn parse_ivrs(table: &[u8]) -> Result<IommuTopology, Error> {
    validate_acpi_table(table, b"IVRS", IOMMU_TABLE_HEADER_SIZE)?;
    let mut topology = IommuTopology::new(IommuKind::AmdVi);
    walk_structures(&table[IOMMU_TABLE_HEADER_SIZE..], |kind, structure| {
        if !matches!(kind as u8, 0x10 | 0x11 | 0x40) {
            return Ok(());
        }
        if structure.len() < 24 {
            return Err(Error::InvalidTable);
        }
        topology.push(IommuUnit {
            segment: read_u16(structure, 16)?,
            register_base: read_u64(structure, 8)?,
            include_all: true,
            scope_requesters: [0; MAX_UNIT_SCOPES],
            scope_count: 0,
        })
    })?;
    if topology.unit_count == 0 {
        return Err(Error::InvalidTable);
    }
    Ok(topology)
}

fn walk_structures(
    mut bytes: &[u8],
    mut visit: impl FnMut(u16, &[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    while !bytes.is_empty() {
        if bytes.len() < 4 {
            return Err(Error::InvalidTable);
        }
        let kind = read_u16(bytes, 0)?;
        let length = read_u16(bytes, 2)? as usize;
        if length < 4 || length > bytes.len() {
            return Err(Error::InvalidTable);
        }
        visit(kind, &bytes[..length])?;
        bytes = &bytes[length..];
    }
    Ok(())
}

fn validate_acpi_table(
    table: &[u8],
    signature: &[u8; 4],
    minimum_size: usize,
) -> Result<(), Error> {
    if table.len() < minimum_size
        || &table[..4] != signature
        || read_u32(table, 4)? as usize != table.len()
        || !checksum_is_zero(table)
    {
        return Err(Error::InvalidTable);
    }
    Ok(())
}

unsafe fn acpi_table(address: u64) -> Result<&'static [u8], Error> {
    // SAFETY: The caller guarantees that this ACPI table pointer remains readable.
    let header = unsafe { table_bytes(address, ACPI_HEADER_SIZE)? };
    let length = read_u32(header, 4)? as usize;
    if !(ACPI_HEADER_SIZE..=MAX_ACPI_TABLE_SIZE).contains(&length) {
        return Err(Error::InvalidTable);
    }
    // SAFETY: The length came from the readable header and passed the size limit.
    let table = unsafe { table_bytes(address, length)? };
    if !checksum_is_zero(table) {
        return Err(Error::InvalidTable);
    }
    Ok(table)
}

unsafe fn table_bytes(address: u64, length: usize) -> Result<&'static [u8], Error> {
    if address == 0 || length == 0 || address.checked_add(length as u64).is_none() {
        return Err(Error::InvalidTable);
    }
    // SAFETY: The caller guarantees this identity-mapped range is readable, and
    // the arithmetic above rejected null and wrapping ranges.
    Ok(unsafe { slice::from_raw_parts(address as *const u8, length) })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let value = bytes.get(offset..offset + 2).ok_or(Error::InvalidTable)?;
    Ok(u16::from_le_bytes(
        value.try_into().map_err(|_| Error::InvalidTable)?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let value = bytes.get(offset..offset + 4).ok_or(Error::InvalidTable)?;
    Ok(u32::from_le_bytes(
        value.try_into().map_err(|_| Error::InvalidTable)?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let value = bytes.get(offset..offset + 8).ok_or(Error::InvalidTable)?;
    Ok(u64::from_le_bytes(
        value.try_into().map_err(|_| Error::InvalidTable)?,
    ))
}

fn checksum_is_zero(bytes: &[u8]) -> bool {
    bytes.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish_table(table: &mut [u8], signature: &[u8; 4]) {
        table[..4].copy_from_slice(signature);
        let length = table.len() as u32;
        table[4..8].copy_from_slice(&length.to_le_bytes());
        table[9] = 0;
        table[9] = 0_u8.wrapping_sub(table.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte)));
    }

    #[test]
    fn parses_intel_dmar_units() {
        let mut table = [0_u8; 64];
        table[48..50].copy_from_slice(&0_u16.to_le_bytes());
        table[50..52].copy_from_slice(&16_u16.to_le_bytes());
        table[52] = 1;
        table[54..56].copy_from_slice(&2_u16.to_le_bytes());
        table[56..64].copy_from_slice(&0xfed9_0000_u64.to_le_bytes());
        finish_table(&mut table, b"DMAR");
        let topology = parse_dmar(&table).unwrap();
        assert_eq!(topology.kind(), IommuKind::IntelVtd);
        assert_eq!(topology.units()[0].segment, 2);
        assert!(topology.units()[0].include_all);
        assert!(topology.covers_requester(2, 0x00f8));
        assert!(!topology.covers_requester(0, 0x00f8));
    }

    #[test]
    fn parses_a_direct_requester_scope_for_an_intel_unit() {
        let mut table = [0_u8; 72];
        table[48..50].copy_from_slice(&0_u16.to_le_bytes());
        table[50..52].copy_from_slice(&24_u16.to_le_bytes());
        table[56..64].copy_from_slice(&0xfed9_0000_u64.to_le_bytes());
        table[64] = 1;
        table[65] = 8;
        table[69] = 0;
        table[70] = 2;
        table[71] = 0;
        finish_table(&mut table, b"DMAR");
        let topology = parse_dmar(&table).unwrap();
        assert!(topology.units()[0].covers_requester(0x0010));
        assert!(!topology.units()[0].covers_requester(0x0018));
    }

    #[test]
    fn explicit_intel_scope_precedes_the_include_all_unit() {
        let mut topology = IommuTopology::new(IommuKind::IntelVtd);
        topology
            .push(IommuUnit {
                segment: 0,
                register_base: 0xfed9_0000,
                include_all: true,
                scope_requesters: [0; MAX_UNIT_SCOPES],
                scope_count: 0,
            })
            .unwrap();
        let mut scoped = [0; MAX_UNIT_SCOPES];
        scoped[0] = 0x0010;
        topology
            .push(IommuUnit {
                segment: 0,
                register_base: 0xfed9_1000,
                include_all: false,
                scope_requesters: scoped,
                scope_count: 1,
            })
            .unwrap();
        assert!(!topology.unit_handles_requester(0, 0, 0x0010));
        assert!(topology.unit_handles_requester(1, 0, 0x0010));
        assert!(topology.unit_handles_requester(0, 0, 0x0018));
        assert!(topology.covers_requester(0, 0x0018));
    }

    #[test]
    fn parses_intel_rmrr_for_a_direct_pci_endpoint() {
        let mut table = [0_u8; 96];
        table[48..50].copy_from_slice(&0_u16.to_le_bytes());
        table[50..52].copy_from_slice(&16_u16.to_le_bytes());
        table[52] = 1;
        table[56..64].copy_from_slice(&0xfed9_0000_u64.to_le_bytes());
        table[64..66].copy_from_slice(&1_u16.to_le_bytes());
        table[66..68].copy_from_slice(&32_u16.to_le_bytes());
        table[72..80].copy_from_slice(&0x7a00_0000_u64.to_le_bytes());
        table[80..88].copy_from_slice(&0x7bff_ffff_u64.to_le_bytes());
        table[88] = 1;
        table[89] = 8;
        table[93] = 0;
        table[94] = 2;
        table[95] = 0;
        finish_table(&mut table, b"DMAR");
        let topology = parse_dmar(&table).unwrap();
        assert_eq!(
            topology.reserved_mappings(),
            &[ReservedMapping {
                segment: 0,
                requester: 0x0010,
                base: 0x7a00_0000,
                limit: 0x7bff_ffff,
            }]
        );
    }

    #[test]
    fn parses_amd_ivrs_units() {
        let mut table = [0_u8; 72];
        table[48..50].copy_from_slice(&0x10_u16.to_le_bytes());
        table[50..52].copy_from_slice(&24_u16.to_le_bytes());
        table[56..64].copy_from_slice(&0xfeb8_0000_u64.to_le_bytes());
        table[64..66].copy_from_slice(&1_u16.to_le_bytes());
        finish_table(&mut table, b"IVRS");
        let topology = parse_ivrs(&table).unwrap();
        assert_eq!(topology.kind(), IommuKind::AmdVi);
        assert_eq!(topology.units()[0].segment, 1);
    }

    #[test]
    fn rejects_a_bad_checksum() {
        let mut table = [0_u8; 64];
        finish_table(&mut table, b"DMAR");
        table[20] = 1;
        assert_eq!(parse_dmar(&table), Err(Error::InvalidTable));
    }

    #[test]
    fn rejects_zero_length_remapping_structures() {
        let mut table = [0_u8; 52];
        finish_table(&mut table, b"DMAR");
        assert_eq!(parse_dmar(&table), Err(Error::InvalidTable));
    }

    #[test]
    fn deny_all_tables_cover_each_architectures_requester_space() {
        assert_eq!(deny_all_table_pages(IommuKind::IntelVtd), INTEL_TABLE_PAGES);
        assert_eq!(deny_all_table_pages(IommuKind::AmdVi), 512);
        assert_eq!(AMD_DEVICE_TABLE_SIZE, 0x1ff);
        assert_eq!(
            IommuResources::required_pages(IommuKind::IntelVtd),
            (INTEL_TABLE_PAGES, 0, 0, 0)
        );
        assert_eq!(
            IommuResources::required_pages(IommuKind::AmdVi),
            (512, DOMAIN_TABLE_PAGES, 2, 1)
        );
    }

    #[test]
    fn intel_tables_map_only_the_reserved_requester_range() {
        #[repr(align(4096))]
        struct Tables([u8; INTEL_TABLE_PAGES * 4096]);
        let mut tables = Tables([0; INTEL_TABLE_PAGES * 4096]);
        let base = tables.0.as_mut_ptr() as u64;
        let mapping = ReservedMapping {
            segment: 0,
            requester: 0x0010,
            base: 0x2000_0000,
            limit: 0x203f_ffff,
        };
        unsafe { build_intel_tables(base, &[mapping]).unwrap() };
        let root = unsafe { read_volatile(base as *const u64) };
        assert_ne!(root & 1, 0);
        let context = root & !0xfff;
        let display_context = unsafe { read_volatile((context + 0x10 * 16) as *const u64) };
        let unrelated_context = unsafe { read_volatile((context + 0x18 * 16) as *const u64) };
        assert_ne!(display_context & 1, 0);
        assert_eq!(unrelated_context, 0);
    }

    #[test]
    fn intel_domain_tables_translate_iova_to_domain_ram() {
        #[repr(align(4096))]
        struct Tables([u8; DOMAIN_TABLE_PAGES * 4096]);
        let mut tables = Tables([0; DOMAIN_TABLE_PAGES * 4096]);
        let base = tables.0.as_mut_ptr() as u64;
        let mut arena = TableArena {
            base,
            next_page: 0,
            page_count: DOMAIN_TABLE_PAGES,
        };
        let root = unsafe { arena.allocate().unwrap() };
        unsafe {
            map_domain_memory(IommuKind::IntelVtd, root, 0x2000_0000, 8192, &mut arena).unwrap()
        };
        let level3 = unsafe { read_volatile(root as *const u64) } & 0x000f_ffff_ffff_f000;
        let level2 = unsafe { read_volatile(level3 as *const u64) } & 0x000f_ffff_ffff_f000;
        let level1 = unsafe { read_volatile(level2 as *const u64) } & 0x000f_ffff_ffff_f000;
        assert_eq!(unsafe { read_volatile(level1 as *const u64) }, 0x2000_0003);
        assert_eq!(
            unsafe { read_volatile((level1 + 8) as *const u64) },
            0x2000_1003
        );
    }

    #[test]
    fn intel_domain_tables_keep_the_assigned_requesters_reserved_dma_range() {
        #[repr(align(4096))]
        struct Tables([u8; INTEL_TABLE_PAGES * 4096]);
        let mut tables = Tables([0; INTEL_TABLE_PAGES * 4096]);
        let base = tables.0.as_mut_ptr() as u64;
        let mut runtime = UnitRuntime {
            resources: IommuResources {
                remapping_table: base,
                ..EMPTY_RESOURCES
            },
            next_domain_page: 1,
            ..EMPTY_UNIT_RUNTIME
        };
        let root = base;

        unsafe {
            map_domain_reserved_identity(
                &mut runtime,
                root,
                0x2000_0000,
                0x201f_ffff,
            )
            .unwrap()
        };

        let level3 = unsafe { read_volatile(root as *const u64) } & 0x000f_ffff_ffff_f000;
        let level2 = unsafe { read_volatile(level3 as *const u64) } & 0x000f_ffff_ffff_f000;
        let entry = unsafe { read_volatile((level2 + 0x100 * 8) as *const u64) };
        assert_eq!(entry, 0x2000_0083);
    }

    #[test]
    fn amd_domain_tables_encode_levels_and_permissions() {
        #[repr(align(4096))]
        struct Tables([u8; DOMAIN_TABLE_PAGES * 4096]);
        let mut tables = Tables([0; DOMAIN_TABLE_PAGES * 4096]);
        let base = tables.0.as_mut_ptr() as u64;
        let mut arena = TableArena {
            base,
            next_page: 0,
            page_count: DOMAIN_TABLE_PAGES,
        };
        let root = unsafe { arena.allocate().unwrap() };
        unsafe {
            map_domain_memory(IommuKind::AmdVi, root, 0x3000_0000, 8192, &mut arena).unwrap()
        };
        let root_entry = unsafe { read_volatile(root as *const u64) };
        assert_eq!(root_entry >> 9 & 7, 3);
        let level3 = root_entry & 0x000f_ffff_ffff_f000;
        let level3_entry = unsafe { read_volatile(level3 as *const u64) };
        assert_eq!(level3_entry >> 9 & 7, 2);
        let level2 = level3_entry & 0x000f_ffff_ffff_f000;
        let level2_entry = unsafe { read_volatile(level2 as *const u64) };
        assert_eq!(level2_entry >> 9 & 7, 1);
        let level1 = level2_entry & 0x000f_ffff_ffff_f000;
        assert_eq!(
            unsafe { read_volatile(level1 as *const u64) },
            0x3000_0000 | AMD_PTE_PRESENT | AMD_PTE_READ_WRITE
        );
    }

    #[test]
    fn amd_dte_encodes_domain_root_and_permissions() {
        let entry = amd_dte(0x1234_5000, 7);
        assert_eq!(entry[0] & 0x000f_ffff_ffff_f000, 0x1234_5000);
        assert_eq!(entry[0] >> 9 & 7, 4);
        assert_ne!(entry[0] & AMD_DTE_VALID, 0);
        assert_ne!(entry[0] & AMD_DTE_TRANSLATION_VALID, 0);
        assert_eq!(entry[0] & AMD_DTE_READ_WRITE, AMD_DTE_READ_WRITE);
        assert_eq!(entry[1], 7);
        assert_eq!(entry[2..], [0, 0]);
    }

    #[test]
    fn amd_invalidation_sequence_covers_device_domain_and_completion() {
        let commands = amd_invalidation_commands(0x1234, 7, 0x1_2345_6000, 9);
        assert_eq!(
            commands[0],
            [0x1234, AMD_COMMAND_INVALIDATE_DTE << 28, 0, 0]
        );
        assert_eq!(commands[1][1], (AMD_COMMAND_INVALIDATE_PAGES << 28) | 7);
        assert_eq!(
            u64::from(commands[1][2]) | (u64::from(commands[1][3]) << 32),
            AMD_INVALIDATE_ALL_PAGES
        );
        assert_eq!(commands[2][0], 0x2345_6001);
        assert_eq!(commands[2][1], (AMD_COMMAND_COMPLETION_WAIT << 28) | 1);
        assert_eq!(commands[2][2], 9);
        assert_eq!(commands[2][3], 0);
    }
}
