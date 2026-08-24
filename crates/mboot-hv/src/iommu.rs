use core::slice;
use core::{
    arch::asm,
    hint::spin_loop,
    ptr::{read_volatile, write_volatile},
};

const ACPI_HEADER_SIZE: usize = 36;
const IOMMU_TABLE_HEADER_SIZE: usize = 48;
const MAX_ACPI_TABLE_SIZE: usize = 1024 * 1024;
const MAX_IOMMU_UNITS: usize = 16;
const MAX_UNIT_SCOPES: usize = 8;
const MAX_RESERVED_MAPPINGS: usize = 32;
const INTEL_TABLE_PAGES: usize = 64;
const AMD_DEVICE_TABLE_PAGES: usize = 512;
const INTEL_VERSION: u64 = 0x00;
const INTEL_CAPABILITY: u64 = 0x08;
const INTEL_GLOBAL_COMMAND: u64 = 0x18;
const INTEL_GLOBAL_STATUS: u64 = 0x1c;
const INTEL_ROOT_TABLE_ADDRESS: u64 = 0x20;
const INTEL_TRANSLATION_ENABLE: u32 = 1 << 31;
const INTEL_SET_ROOT_POINTER: u32 = 1 << 30;
const AMD_DEVICE_TABLE_BASE: u64 = 0x00;
const AMD_CONTROL: u64 = 0x18;
const AMD_IOMMU_ENABLE: u64 = 1;
const AMD_DEVICE_TABLE_SEGMENT_ENABLE: u64 = 0b111 << 34;
const AMD_DEVICE_TABLE_SIZE: u64 = AMD_DEVICE_TABLE_PAGES as u64 - 1;
const REGISTER_WAIT_LIMIT: usize = 1_000_000;

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
}

pub const fn deny_all_table_pages(kind: IommuKind) -> usize {
    match kind {
        IommuKind::IntelVtd => INTEL_TABLE_PAGES,
        IommuKind::AmdVi => AMD_DEVICE_TABLE_PAGES,
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
    for (unit, table_address) in topology.units().iter().zip(table_addresses) {
        if topology.kind == IommuKind::IntelVtd
            && deferred_requester.is_some_and(|requester| unit.covers_requester(requester))
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
                    enable_intel_protection(unit, *table_address, topology.reserved_mappings())?
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
) -> Result<(), Error> {
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
    unsafe { build_intel_tables(root_table, mappings)? };

    // Start from a known state. Requesters without an RMRR are unable to issue
    // new DMA; firmware-reserved requesters are immediately restored below.
    // SAFETY: GCMD is the command register of this exclusively owned unit.
    unsafe { mmio_write_u32(unit.register_base, INTEL_GLOBAL_COMMAND, 0) };
    // SAFETY: GSTS is readable while the unit processes the disable command.
    unsafe {
        wait_intel_status(unit.register_base, INTEL_TRANSLATION_ENABLE, false)?;
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
        mmio_write_u32(
            unit.register_base,
            INTEL_GLOBAL_COMMAND,
            INTEL_TRANSLATION_ENABLE,
        );
        wait_intel_status(unit.register_base, INTEL_TRANSLATION_ENABLE, true)?;
    }
    Ok(())
}

struct IntelTableArena {
    base: u64,
    next_page: usize,
}

impl IntelTableArena {
    unsafe fn allocate(&mut self) -> Result<u64, Error> {
        if self.next_page == INTEL_TABLE_PAGES {
            return Err(Error::InvalidResources);
        }
        let address = self.base + self.next_page as u64 * 4096;
        self.next_page += 1;
        Ok(address)
    }
}

unsafe fn build_intel_tables(root_table: u64, mappings: &[ReservedMapping]) -> Result<(), Error> {
    let mut arena = IntelTableArena {
        base: root_table,
        next_page: 1,
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
    Ok(())
}

unsafe fn ensure_root_entry(
    root_table: u64,
    bus: usize,
    arena: &mut IntelTableArena,
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
    arena: &mut IntelTableArena,
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

unsafe fn ensure_page_entry(
    table: u64,
    index: u64,
    arena: &mut IntelTableArena,
) -> Result<u64, Error> {
    let entry = (table + index * 8) as *mut u64;
    // SAFETY: Every x86 page-table index is in 0..512.
    let mut child = unsafe { read_volatile(entry) } & !0xfff;
    if child == 0 {
        // SAFETY: The arena returns a fresh zeroed page.
        child = unsafe { arena.allocate()? };
        // Read and write permissions are required at every second-level table.
        unsafe { write_volatile(entry, child | 3) };
    }
    Ok(child)
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

unsafe fn wait_intel_status(base: u64, mask: u32, set: bool) -> Result<(), Error> {
    for _ in 0..REGISTER_WAIT_LIMIT {
        // SAFETY: The caller guarantees the Intel VT-d register range is mapped.
        let status = unsafe { mmio_read_u32(base, INTEL_GLOBAL_STATUS) };
        if (status & mask != 0) == set {
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
    // SAFETY: The caller guarantees the RSDP mapping, initially for its fixed header.
    let rsdp = unsafe { table_bytes(rsdp_address, 36)? };
    if &rsdp[..8] != b"RSD PTR " || !checksum_is_zero(&rsdp[..20]) || rsdp[15] < 2 {
        return Err(Error::InvalidRsdp);
    }
    let rsdp_length = read_u32(rsdp, 20)? as usize;
    if !(36..=MAX_ACPI_TABLE_SIZE).contains(&rsdp_length) {
        return Err(Error::InvalidRsdp);
    }
    // SAFETY: The validated RSDP length remains within the caller-owned ACPI mapping.
    let rsdp = unsafe { table_bytes(rsdp_address, rsdp_length)? };
    if !checksum_is_zero(rsdp) {
        return Err(Error::InvalidRsdp);
    }
    let xsdt_address = read_u64(rsdp, 24)?;
    // SAFETY: The caller's ACPI mapping guarantee covers the XSDT pointer from the RSDP.
    let xsdt = unsafe { acpi_table(xsdt_address)? };
    if &xsdt[..4] != b"XSDT" || !(xsdt.len() - ACPI_HEADER_SIZE).is_multiple_of(8) {
        return Err(Error::InvalidTable);
    }
    for entry in xsdt[ACPI_HEADER_SIZE..].chunks_exact(8) {
        let address = u64::from_le_bytes(entry.try_into().map_err(|_| Error::InvalidTable)?);
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
        assert_eq!(deny_all_table_pages(IommuKind::IntelVtd), 64);
        assert_eq!(deny_all_table_pages(IommuKind::AmdVi), 512);
        assert_eq!(AMD_DEVICE_TABLE_SIZE, 0x1ff);
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
}
