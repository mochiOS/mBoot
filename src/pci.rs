use core::arch::{asm, x86_64::__cpuid};
use core::hint::spin_loop;
use core::ptr::write_volatile;
use core::sync::atomic::{AtomicU64, Ordering};

use mnu_abi::hypervisor::{
    PciDeviceResource, PCI_DEVICE_INTERRUPT_CONFIG, PCI_DEVICE_INTERRUPT_COUNT,
    PCI_DEVICE_INTERRUPT_QUEUE, PCI_RESOURCE_FLAG_READABLE, PCI_RESOURCE_FLAG_WRITABLE,
    PCI_RESOURCE_KIND_MMIO,
};

use crate::arch::x86_64::{read_msr, timer};

const CONFIG_ADDRESS: u16 = 0x0cf8;
const CONFIG_DATA: u16 = 0x0cfc;
const COMMAND_BUS_MASTER: u16 = 1 << 2;
const COMMAND_IO: u16 = 1;
const COMMAND_MEMORY: u16 = 1 << 1;
const COMMAND_INTERRUPT_DISABLE: u16 = 1 << 10;
const STATUS_CAPABILITIES: u16 = 1 << 4;
const CAPABILITY_MSI: u8 = 0x05;
const CAPABILITY_PCIE: u8 = 0x10;
const CAPABILITY_MSIX: u8 = 0x11;
const MSI_ENABLE: u16 = 1;
const MSI_64_BIT: u16 = 1 << 7;
const MSI_PER_VECTOR_MASK: u16 = 1 << 8;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_TABLE_BIR: u32 = 0x7;
const MSIX_TABLE_OFFSET: u32 = 0xffff_fff8;
const MSIX_ENTRY_MASKED: u32 = 1;
const PCIE_DEVICE_CAP_FLR: u32 = 1 << 28;
const PCIE_DEVICE_CONTROL_FLR: u16 = 1 << 15;
const INTEL_VENDOR_ID: u16 = 0x8086;
const INTEL_GRAPHICS_REQUESTER: u16 = 0x0010;
const INTEL_BDSM: u8 = 0xb0;
const INTEL_BGSM: u8 = 0xb4;
const INTEL_TOLUD: u8 = 0xbc;
const INTEL_MEMORY_BASE_MASK: u32 = 0xfff0_0000;
const MAX_INTEL_STOLEN_MEMORY: u64 = 1024 * 1024 * 1024;
pub const MAX_DEVICE_BARS: usize = 6;
const MAX_BARS: usize = MAX_DEVICE_BARS;
const MAX_ASSIGNMENTS: usize = 32;
pub const DEVICE_VECTOR_FIRST: u8 = 0x50;
pub const DEVICE_VECTOR_LAST: u8 =
    DEVICE_VECTOR_FIRST + (MAX_ASSIGNMENTS * PCI_DEVICE_INTERRUPT_COUNT) as u8 - 1;
const X2APIC_ENABLE: u64 = 1 << 10;
const APIC_ENABLE: u64 = 1 << 11;
const IA32_APIC_BASE: u32 = 0x1b;
const X2APIC_ISR_BASE: u32 = 0x810;
const APIC_ISR_BASE: usize = 0x100;
const MAX_ACTIVE_REQUESTERS: usize = 32;
const MAX_INVENTORY_FUNCTIONS: usize = 256;

static PENDING_DEVICE_INTERRUPTS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PciBar {
    pub index: u8,
    pub physical_address: u64,
    pub length: u64,
    pub guest_address: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PciDescriptor {
    pub requester: u16,
    pub original_command: u16,
    pub bars: [PciBar; MAX_BARS],
    pub bar_count: usize,
    msi_capability: u8,
    msix_capability: u8,
    pcie_capability: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Assignment {
    valid: bool,
    active: bool,
    domain_id: u32,
    guest_vectors: [u8; PCI_DEVICE_INTERRUPT_COUNT],
    interrupt_count: u8,
    descriptor: PciDescriptor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PciInterruptMode {
    Msi,
    MsixShared,
    MsixSplit,
}

impl PciInterruptMode {
    const fn interrupt_count(self) -> usize {
        match self {
            Self::Msi | Self::MsixShared => 1,
            Self::MsixSplit => PCI_DEVICE_INTERRUPT_COUNT,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciActivation {
    pub physical_vectors: [u8; PCI_DEVICE_INTERRUPT_COUNT],
    pub mode: PciInterruptMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PciError {
    InvalidRequester,
    InvalidState,
    UnsupportedHeader,
    UnsupportedBar,
    InvalidBar,
    DeviceWindowExhausted,
    InterruptUnavailable,
    RegisterWriteFailed,
}

pub struct AssignmentTable {
    assignments: [Assignment; MAX_ASSIGNMENTS],
}

impl Default for AssignmentTable {
    fn default() -> Self {
        Self::new()
    }
}

impl AssignmentTable {
    pub const fn new() -> Self {
        Self {
            assignments: [Assignment {
                valid: false,
                active: false,
                domain_id: 0,
                guest_vectors: [0; PCI_DEVICE_INTERRUPT_COUNT],
                interrupt_count: 0,
                descriptor: PciDescriptor {
                    requester: 0,
                    original_command: 0,
                    bars: [PciBar {
                        index: 0,
                        physical_address: 0,
                        length: 0,
                        guest_address: 0,
                    }; MAX_BARS],
                    bar_count: 0,
                    msi_capability: 0,
                    msix_capability: 0,
                    pcie_capability: 0,
                },
            }; MAX_ASSIGNMENTS],
        }
    }

    pub fn insert(
        &mut self,
        domain_id: u32,
        mut descriptor: PciDescriptor,
        window_start: u64,
        window_size: u64,
    ) -> Result<&[PciBar], PciError> {
        if domain_id == 0
            || descriptor.requester == 0
            || descriptor.bar_count == 0
            || window_start & 0xfff != 0
            || window_size == 0
            || window_size & 0xfff != 0
            || window_start.checked_add(window_size).is_none()
            || self.assignments.iter().any(|assignment| {
                assignment.valid && assignment.descriptor.requester == descriptor.requester
            })
        {
            return Err(PciError::InvalidState);
        }
        let slot = self
            .assignments
            .iter()
            .position(|assignment| !assignment.valid)
            .ok_or(PciError::DeviceWindowExhausted)?;
        let window_end = window_start
            .checked_add(window_size)
            .ok_or(PciError::DeviceWindowExhausted)?;
        for bar in &mut descriptor.bars[..descriptor.bar_count] {
            let end = bar
                .physical_address
                .checked_add(bar.length)
                .ok_or(PciError::DeviceWindowExhausted)?;
            if bar.physical_address < window_start || end > window_end {
                return Err(PciError::DeviceWindowExhausted);
            }
            // The guest sees the firmware-assigned BAR address, but nested page
            // tables still expose only this exact range. Keeping the address
            // avoids truncating 32-bit BARs and naturally preserves the
            // alignment required by large GPU apertures.
            bar.guest_address = bar.physical_address;
        }
        self.assignments[slot] = Assignment {
            valid: true,
            active: false,
            domain_id,
            guest_vectors: [0; PCI_DEVICE_INTERRUPT_COUNT],
            interrupt_count: 0,
            descriptor,
        };
        Ok(&self.assignments[slot].descriptor.bars[..descriptor.bar_count])
    }

    pub fn resource(
        &self,
        domain_id: u32,
        requester: u16,
        index: usize,
    ) -> Option<PciDeviceResource> {
        let assignment = self.assignment(domain_id, requester)?;
        let bar = assignment
            .descriptor
            .bars
            .get(index)
            .filter(|_| index < assignment.descriptor.bar_count)?;
        let resource = PciDeviceResource {
            requester,
            bar_index: bar.index,
            kind: PCI_RESOURCE_KIND_MMIO,
            flags: PCI_RESOURCE_FLAG_READABLE | PCI_RESOURCE_FLAG_WRITABLE,
            guest_address: bar.guest_address,
            length: bar.length,
            _reserved0: 0,
        };
        resource.validate().then_some(resource)
    }

    pub unsafe fn config_read(
        &self,
        domain_id: u32,
        requester: u16,
        offset: u16,
    ) -> Result<u32, PciError> {
        self.assignment(domain_id, requester)
            .ok_or(PciError::InvalidState)?;
        if offset > 0xfc || offset & 3 != 0 {
            return Err(PciError::InvalidState);
        }
        let (bus, device, function) = requester_parts(requester)?;
        Ok(unsafe { read_u32(bus, device, function, offset as u8) })
    }

    pub unsafe fn activate(
        &mut self,
        domain_id: u32,
        requester: u16,
        guest_vectors: [u8; PCI_DEVICE_INTERRUPT_COUNT],
    ) -> Result<PciActivation, PciError> {
        let slot = self
            .assignments
            .iter()
            .position(|assignment| {
                assignment.valid
                    && !assignment.active
                    && assignment.domain_id == domain_id
                    && assignment.descriptor.requester == requester
            })
            .ok_or(PciError::InvalidState)?;
        let mode = unsafe { interrupt_mode(&self.assignments[slot].descriptor)? };
        let interrupt_count = mode.interrupt_count();
        if !valid_guest_vectors(&guest_vectors, interrupt_count) {
            return Err(PciError::InvalidState);
        }
        let physical_vectors = [
            physical_vector(slot, PCI_DEVICE_INTERRUPT_CONFIG),
            physical_vector(slot, PCI_DEVICE_INTERRUPT_QUEUE),
        ];
        let pending_mask = 0b11_u64 << (slot * PCI_DEVICE_INTERRUPT_COUNT);
        PENDING_DEVICE_INTERRUPTS.fetch_and(!pending_mask, Ordering::AcqRel);
        unsafe { activate_descriptor(&self.assignments[slot].descriptor, physical_vectors, mode)? };
        self.assignments[slot].active = true;
        self.assignments[slot].guest_vectors = guest_vectors;
        self.assignments[slot].interrupt_count = interrupt_count as u8;
        Ok(PciActivation {
            physical_vectors,
            mode,
        })
    }

    pub unsafe fn deactivate(
        &mut self,
        domain_id: u32,
        requester: u16,
        reset: bool,
    ) -> Result<[PciBar; MAX_BARS], PciError> {
        let slot = self
            .assignments
            .iter()
            .position(|assignment| {
                assignment.valid
                    && assignment.domain_id == domain_id
                    && assignment.descriptor.requester == requester
            })
            .ok_or(PciError::InvalidState)?;
        let assignment = self.assignments[slot];
        unsafe { disable_descriptor(&assignment.descriptor, reset)? };
        let pending_mask = 0b11_u64 << (slot * PCI_DEVICE_INTERRUPT_COUNT);
        PENDING_DEVICE_INTERRUPTS.fetch_and(!pending_mask, Ordering::AcqRel);
        self.assignments[slot].active = false;
        self.assignments[slot].guest_vectors = [0; PCI_DEVICE_INTERRUPT_COUNT];
        self.assignments[slot].interrupt_count = 0;
        Ok(assignment.descriptor.bars)
    }

    pub fn remove(&mut self, domain_id: u32, requester: u16) -> Result<(), PciError> {
        let slot = self
            .assignments
            .iter()
            .position(|assignment| {
                assignment.valid
                    && !assignment.active
                    && assignment.domain_id == domain_id
                    && assignment.descriptor.requester == requester
            })
            .ok_or(PciError::InvalidState)?;
        self.assignments[slot] = Assignment::default();
        Ok(())
    }

    pub fn route_pending(&self, pending: u64) -> impl Iterator<Item = (u32, u8)> + '_ {
        self.assignments
            .iter()
            .enumerate()
            .flat_map(move |(slot, assignment)| {
                assignment
                    .guest_vectors
                    .iter()
                    .copied()
                    .take(usize::from(assignment.interrupt_count))
                    .enumerate()
                    .filter_map(move |(index, guest_vector)| {
                        let pending_index = slot * PCI_DEVICE_INTERRUPT_COUNT + index;
                        (pending & (1_u64 << pending_index) != 0
                            && assignment.valid
                            && assignment.active)
                            .then_some((assignment.domain_id, guest_vector))
                    })
            })
    }

    pub fn has_active(&self) -> bool {
        self.assignments
            .iter()
            .any(|assignment| assignment.valid && assignment.active)
    }

    pub fn has_active_for_domain(&self, domain_id: u32) -> bool {
        self.assignments.iter().any(|assignment| {
            assignment.valid && assignment.active && assignment.domain_id == domain_id
        })
    }

    fn assignment(&self, domain_id: u32, requester: u16) -> Option<&Assignment> {
        self.assignments.iter().find(|assignment| {
            assignment.valid
                && assignment.domain_id == domain_id
                && assignment.descriptor.requester == requester
        })
    }
}

fn physical_vector(slot: usize, index: usize) -> u8 {
    DEVICE_VECTOR_FIRST + (slot * PCI_DEVICE_INTERRUPT_COUNT + index) as u8
}

fn valid_guest_vectors(vectors: &[u8; PCI_DEVICE_INTERRUPT_COUNT], count: usize) -> bool {
    count > 0
        && count <= vectors.len()
        && vectors[..count]
            .iter()
            .all(|vector| (0x20..=0xef).contains(vector) && !matches!(vector, 0x40 | 0x41))
        && (count == 1 || vectors[0] != vectors[1])
        && vectors[count..].iter().all(|vector| *vector == 0)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PciFunction {
    pub requester: u16,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuarantineReport {
    pub functions: u32,
    pub bus_masters_disabled: u32,
    pub bus_masters_active: u32,
    pub first_active_requester: Option<u16>,
    pub display_requester: Option<u16>,
    active_requesters: [u16; MAX_ACTIVE_REQUESTERS],
    active_requester_count: usize,
    inventory: [PciFunction; MAX_INVENTORY_FUNCTIONS],
    inventory_count: usize,
}

/// Returns the Intel integrated graphics memory reserved below TOLUD.
///
/// The range contains both the GTT stolen area and the graphics data stolen
/// area. It is read from the Intel host bridge rather than guessed from the
/// framebuffer address. Only the conventional integrated display requester is
/// accepted so these chipset-specific registers are never interpreted for a
/// discrete GPU.
///
/// # Safety
/// The caller must exclusively own PCI configuration-space access.
pub unsafe fn intel_graphics_stolen_range(requester: u16) -> Option<(u64, u64)> {
    if requester != INTEL_GRAPHICS_REQUESTER
        || unsafe { read_u16(0, 0, 0, 0) } != INTEL_VENDOR_ID
        || unsafe { read_u8(0, 0, 0, 0x0b) } != 0x06
        || unsafe { read_u16(0, 2, 0, 0) } != INTEL_VENDOR_ID
        || unsafe { read_u8(0, 2, 0, 0x0b) } != 0x03
    {
        return None;
    }
    let bdsm = unsafe { read_u32(0, 0, 0, INTEL_BDSM) };
    let bgsm = unsafe { read_u32(0, 0, 0, INTEL_BGSM) };
    let tolud = unsafe { read_u32(0, 0, 0, INTEL_TOLUD) };
    intel_graphics_stolen_range_from_registers(bdsm, bgsm, tolud)
}

fn intel_graphics_stolen_range_from_registers(
    bdsm: u32,
    bgsm: u32,
    tolud: u32,
) -> Option<(u64, u64)> {
    let data_base = u64::from(bdsm & INTEL_MEMORY_BASE_MASK);
    let gtt_base = u64::from(bgsm & INTEL_MEMORY_BASE_MASK);
    let top = u64::from(tolud & INTEL_MEMORY_BASE_MASK);
    let base = data_base.min(gtt_base);
    let size = top.checked_sub(base)?;
    if base == 0
        || data_base >= top
        || gtt_base >= top
        || size == 0
        || size > MAX_INTEL_STOLEN_MEMORY
    {
        return None;
    }
    Some((base, top - 1))
}

impl Default for QuarantineReport {
    fn default() -> Self {
        Self {
            functions: 0,
            bus_masters_disabled: 0,
            bus_masters_active: 0,
            first_active_requester: None,
            display_requester: None,
            active_requesters: [0; MAX_ACTIVE_REQUESTERS],
            active_requester_count: 0,
            inventory: [PciFunction::default(); MAX_INVENTORY_FUNCTIONS],
            inventory_count: 0,
        }
    }
}

impl QuarantineReport {
    pub fn active_requesters(&self) -> &[u16] {
        &self.active_requesters[..self.active_requester_count]
    }

    pub const fn recorded_every_active_requester(&self) -> bool {
        self.active_requester_count as u32 == self.bus_masters_active
    }

    pub fn inventory(&self) -> &[PciFunction] {
        &self.inventory[..self.inventory_count]
    }
}

/// Reads and validates a Type-0 endpoint's assigned memory BARs while all
/// decoding and bus mastering are disabled.
///
/// # Safety
/// PCI configuration mechanism 1 must be exclusively owned by mBoot, and the
/// requester must remain stopped for the duration of the probe.
pub unsafe fn probe_descriptor(requester: u16) -> Result<PciDescriptor, PciError> {
    let (bus, device, function) = requester_parts(requester)?;
    if unsafe { read_u16(bus, device, function, 0) } == 0xffff {
        return Err(PciError::InvalidRequester);
    }
    if unsafe { read_u8(bus, device, function, 0x0e) } & 0x7f != 0 {
        return Err(PciError::UnsupportedHeader);
    }
    let original_command = unsafe { read_u16(bus, device, function, 4) };
    unsafe {
        write_u16(
            bus,
            device,
            function,
            4,
            original_command & !(COMMAND_IO | COMMAND_MEMORY | COMMAND_BUS_MASTER),
        )
    };
    let result = (|| {
        let mut descriptor = PciDescriptor {
            requester,
            original_command,
            ..PciDescriptor::default()
        };
        descriptor.msi_capability = unsafe { find_capability(requester, CAPABILITY_MSI)? };
        descriptor.msix_capability = unsafe { find_capability(requester, CAPABILITY_MSIX)? };
        descriptor.pcie_capability = unsafe { find_capability(requester, CAPABILITY_PCIE)? };
        unsafe { disable_interrupt(&descriptor) };
        let mut index = 0_u8;
        while usize::from(index) < MAX_BARS {
            let offset = 0x10 + index * 4;
            let low = unsafe { read_u32(bus, device, function, offset) };
            if low == 0 || low == u32::MAX {
                index += 1;
                continue;
            }
            if low & 1 != 0 {
                index += 1;
                continue;
            }
            let kind = low >> 1 & 3;
            let is_64 = kind == 2;
            if kind == 1 || (is_64 && usize::from(index) + 1 >= MAX_BARS) {
                return Err(PciError::UnsupportedBar);
            }
            let high = if is_64 {
                unsafe { read_u32(bus, device, function, offset + 4) }
            } else {
                0
            };
            unsafe {
                write_u32(bus, device, function, offset, u32::MAX);
                if is_64 {
                    write_u32(bus, device, function, offset + 4, u32::MAX);
                }
            }
            let mask_low = unsafe { read_u32(bus, device, function, offset) };
            let mask_high = if is_64 {
                unsafe { read_u32(bus, device, function, offset + 4) }
            } else {
                0
            };
            unsafe {
                if is_64 {
                    write_u32(bus, device, function, offset + 4, high);
                }
                write_u32(bus, device, function, offset, low);
            }
            let address = (u64::from(high) << 32) | u64::from(low & !0xf);
            let mask = if is_64 {
                (u64::from(mask_high) << 32) | u64::from(mask_low & !0xf)
            } else {
                u64::from(mask_low & !0xf) | 0xffff_ffff_0000_0000
            };
            let length = (!mask).wrapping_add(1);
            if address == 0
                || address & 0xfff != 0
                || length < 4096
                || length & 0xfff != 0
                || !length.is_power_of_two()
                || address.checked_add(length).is_none()
                || address >> 52 != 0
            {
                return Err(PciError::InvalidBar);
            }
            descriptor.bars[descriptor.bar_count] = PciBar {
                index,
                physical_address: address,
                length,
                guest_address: 0,
            };
            descriptor.bar_count += 1;
            index += if is_64 { 2 } else { 1 };
        }
        if descriptor.bar_count == 0
            || (descriptor.msi_capability == 0 && descriptor.msix_capability == 0)
        {
            return Err(PciError::InterruptUnavailable);
        }
        Ok(descriptor)
    })();
    unsafe {
        write_u16(
            bus,
            device,
            function,
            4,
            original_command & !COMMAND_BUS_MASTER | COMMAND_INTERRUPT_DISABLE,
        )
    };
    if unsafe { read_u16(bus, device, function, 4) } & COMMAND_BUS_MASTER != 0 {
        return Err(PciError::RegisterWriteFailed);
    }
    result
}

/// Records one physical device MSI and acknowledges it at the local APIC.
/// Called by the common device interrupt entry installed for vectors 0x50..0x8f.
pub fn acknowledge_device_interrupt() {
    if let Some(vector) = active_device_vector() {
        record_device_interrupt(vector);
    }
    timer::acknowledge();
}

/// Records a vector supplied by VMX's exit-interruption information and EOIs
/// the physical interrupt that caused the VM exit.
pub fn acknowledge_vmexit_interrupt(vector: u8) {
    if (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST).contains(&vector) {
        record_device_interrupt(vector);
    }
    timer::acknowledge();
}

fn record_device_interrupt(vector: u8) {
    PENDING_DEVICE_INTERRUPTS.fetch_or(1_u64 << (vector - DEVICE_VECTOR_FIRST), Ordering::Release);
}

pub fn take_pending_device_interrupts() -> u64 {
    PENDING_DEVICE_INTERRUPTS.swap(0, Ordering::AcqRel)
}

fn active_device_vector() -> Option<u8> {
    let apic_base = unsafe { read_msr(IA32_APIC_BASE) };
    if apic_base & APIC_ENABLE == 0 {
        return None;
    }
    for register in (0..8_u32).rev() {
        let bits = if apic_base & X2APIC_ENABLE != 0 {
            (unsafe { read_msr(X2APIC_ISR_BASE + register) }) as u32
        } else {
            let base = (apic_base & 0xffff_f000) as *const u8;
            unsafe {
                base.add(APIC_ISR_BASE + register as usize * 0x10)
                    .cast::<u32>()
                    .read_volatile()
            }
        };
        for bit in (0..32_u8).rev() {
            if bits & (1_u32 << bit) != 0 {
                let vector = register as u8 * 32 + bit;
                if (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST).contains(&vector) {
                    return Some(vector);
                }
            }
        }
    }
    None
}

unsafe fn activate_descriptor(
    descriptor: &PciDescriptor,
    physical_vectors: [u8; PCI_DEVICE_INTERRUPT_COUNT],
    mode: PciInterruptMode,
) -> Result<(), PciError> {
    let (bus, device, function) = requester_parts(descriptor.requester)?;
    let apic_base = unsafe { read_msr(IA32_APIC_BASE) };
    if apic_base & APIC_ENABLE == 0 {
        return Err(PciError::InterruptUnavailable);
    }
    let apic_id = if apic_base & X2APIC_ENABLE != 0 {
        u8::try_from(unsafe { read_msr(0x802) }).map_err(|_| PciError::InterruptUnavailable)?
    } else {
        (__cpuid(1).ebx >> 24) as u8
    };
    let message_address = 0xfee0_0000_u64 | (u64::from(apic_id) << 12);
    let memory_command = (descriptor.original_command | COMMAND_MEMORY | COMMAND_INTERRUPT_DISABLE)
        & !COMMAND_BUS_MASTER;
    unsafe { write_u16(bus, device, function, 4, memory_command) };
    match mode {
        PciInterruptMode::Msi => unsafe {
            enable_msi(
                descriptor,
                message_address,
                physical_vectors[PCI_DEVICE_INTERRUPT_CONFIG],
            )?
        },
        PciInterruptMode::MsixShared | PciInterruptMode::MsixSplit => unsafe {
            enable_msix(
                descriptor,
                message_address,
                physical_vectors,
                mode.interrupt_count(),
            )?
        },
    }
    asm_fence();
    unsafe {
        write_u16(
            bus,
            device,
            function,
            4,
            memory_command | COMMAND_BUS_MASTER,
        )
    };
    if unsafe { read_u16(bus, device, function, 4) } & COMMAND_BUS_MASTER == 0 {
        unsafe { disable_interrupt(descriptor) };
        return Err(PciError::RegisterWriteFailed);
    }
    Ok(())
}

unsafe fn interrupt_mode(descriptor: &PciDescriptor) -> Result<PciInterruptMode, PciError> {
    if descriptor.msix_capability != 0 {
        let (bus, device, function) = requester_parts(descriptor.requester)?;
        let control = unsafe { read_u16(bus, device, function, descriptor.msix_capability + 2) };
        return select_interrupt_mode(
            usize::from(control & 0x07ff) + 1,
            descriptor.msi_capability != 0,
        )
        .ok_or(PciError::InterruptUnavailable);
    }
    select_interrupt_mode(0, descriptor.msi_capability != 0).ok_or(PciError::InterruptUnavailable)
}

const fn select_interrupt_mode(
    msix_table_entries: usize,
    has_msi: bool,
) -> Option<PciInterruptMode> {
    if msix_table_entries >= PCI_DEVICE_INTERRUPT_COUNT {
        Some(PciInterruptMode::MsixSplit)
    } else if msix_table_entries == 1 {
        Some(PciInterruptMode::MsixShared)
    } else if has_msi {
        Some(PciInterruptMode::Msi)
    } else {
        None
    }
}

unsafe fn disable_descriptor(descriptor: &PciDescriptor, reset: bool) -> Result<(), PciError> {
    let (bus, device, function) = requester_parts(descriptor.requester)?;
    let command = unsafe { read_u16(bus, device, function, 4) };
    unsafe { write_u16(bus, device, function, 4, command & !COMMAND_BUS_MASTER) };
    if unsafe { read_u16(bus, device, function, 4) } & COMMAND_BUS_MASTER != 0 {
        return Err(PciError::RegisterWriteFailed);
    }
    unsafe { disable_interrupt(descriptor) };
    if reset {
        unsafe { function_level_reset(descriptor)? };
    }
    unsafe {
        write_u16(
            bus,
            device,
            function,
            4,
            descriptor.original_command & !COMMAND_BUS_MASTER | COMMAND_INTERRUPT_DISABLE,
        )
    };
    Ok(())
}

unsafe fn enable_msix(
    descriptor: &PciDescriptor,
    address: u64,
    vectors: [u8; PCI_DEVICE_INTERRUPT_COUNT],
    vector_count: usize,
) -> Result<(), PciError> {
    let (bus, device, function) = requester_parts(descriptor.requester)?;
    let capability = descriptor.msix_capability;
    let mut control = unsafe { read_u16(bus, device, function, capability + 2) };
    let table_entries = usize::from(control & 0x07ff) + 1;
    if vector_count == 0
        || vector_count > PCI_DEVICE_INTERRUPT_COUNT
        || table_entries < vector_count
    {
        return Err(PciError::InterruptUnavailable);
    }
    unsafe {
        write_u16(
            bus,
            device,
            function,
            capability + 2,
            (control | MSIX_FUNCTION_MASK) & !MSIX_ENABLE,
        )
    };
    let table = unsafe { read_u32(bus, device, function, capability + 4) };
    let bar_index = (table & MSIX_TABLE_BIR) as u8;
    let table_offset = u64::from(table & MSIX_TABLE_OFFSET);
    let bar = descriptor.bars[..descriptor.bar_count]
        .iter()
        .find(|bar| bar.index == bar_index)
        .ok_or(PciError::InvalidBar)?;
    if table_offset
        .checked_add((vector_count * 16) as u64)
        .is_none_or(|end| end > bar.length)
    {
        return Err(PciError::InvalidBar);
    }
    let table = (bar.physical_address + table_offset) as *mut u32;
    for (index, vector) in vectors.iter().copied().take(vector_count).enumerate() {
        let entry = unsafe { table.add(index * 4) };
        unsafe {
            write_volatile(entry.add(3), MSIX_ENTRY_MASKED);
            write_volatile(entry, address as u32);
            write_volatile(entry.add(1), (address >> 32) as u32);
            write_volatile(entry.add(2), u32::from(vector));
        }
    }
    asm_fence();
    for index in 0..vector_count {
        unsafe { write_volatile(table.add(index * 4 + 3), 0) };
    }
    control = (control | MSIX_ENABLE) & !MSIX_FUNCTION_MASK;
    unsafe { write_u16(bus, device, function, capability + 2, control) };
    let installed = unsafe { read_u16(bus, device, function, capability + 2) };
    if installed & (MSIX_ENABLE | MSIX_FUNCTION_MASK) != MSIX_ENABLE {
        return Err(PciError::RegisterWriteFailed);
    }
    Ok(())
}

unsafe fn enable_msi(descriptor: &PciDescriptor, address: u64, vector: u8) -> Result<(), PciError> {
    let (bus, device, function) = requester_parts(descriptor.requester)?;
    let capability = descriptor.msi_capability;
    if capability == 0 {
        return Err(PciError::InterruptUnavailable);
    }
    let mut control = unsafe { read_u16(bus, device, function, capability + 2) };
    control &= !(0b111 << 4 | MSI_ENABLE);
    let mask_offset = if control & MSI_64_BIT != 0 {
        0x10
    } else {
        0x0c
    };
    unsafe { write_u16(bus, device, function, capability + 2, control) };
    if control & MSI_PER_VECTOR_MASK != 0 {
        unsafe { write_u32(bus, device, function, capability + mask_offset, u32::MAX) };
    }
    unsafe {
        write_u32(bus, device, function, capability + 4, address as u32);
        if control & MSI_64_BIT != 0 {
            write_u32(
                bus,
                device,
                function,
                capability + 8,
                (address >> 32) as u32,
            );
            write_u16(bus, device, function, capability + 0x0c, u16::from(vector));
        } else {
            write_u16(bus, device, function, capability + 8, u16::from(vector));
        }
        asm_fence();
        write_u16(bus, device, function, capability + 2, control | MSI_ENABLE);
        if control & MSI_PER_VECTOR_MASK != 0 {
            write_u32(bus, device, function, capability + mask_offset, !1);
        }
    }
    let installed = unsafe { read_u16(bus, device, function, capability + 2) };
    if installed & (MSI_ENABLE | 0b111 << 4) != MSI_ENABLE {
        return Err(PciError::RegisterWriteFailed);
    }
    Ok(())
}

unsafe fn disable_interrupt(descriptor: &PciDescriptor) {
    let Ok((bus, device, function)) = requester_parts(descriptor.requester) else {
        return;
    };
    if descriptor.msix_capability != 0 {
        let control = unsafe { read_u16(bus, device, function, descriptor.msix_capability + 2) };
        unsafe {
            write_u16(
                bus,
                device,
                function,
                descriptor.msix_capability + 2,
                (control | MSIX_FUNCTION_MASK) & !MSIX_ENABLE,
            )
        };
    }
    if descriptor.msi_capability != 0 {
        let control = unsafe { read_u16(bus, device, function, descriptor.msi_capability + 2) };
        unsafe {
            write_u16(
                bus,
                device,
                function,
                descriptor.msi_capability + 2,
                control & !MSI_ENABLE,
            )
        };
    }
}

unsafe fn function_level_reset(descriptor: &PciDescriptor) -> Result<(), PciError> {
    if descriptor.pcie_capability == 0 {
        return Ok(());
    }
    let (bus, device, function) = requester_parts(descriptor.requester)?;
    let capabilities = unsafe { read_u32(bus, device, function, descriptor.pcie_capability + 4) };
    if capabilities & PCIE_DEVICE_CAP_FLR == 0 {
        return Ok(());
    }
    let control = unsafe { read_u16(bus, device, function, descriptor.pcie_capability + 8) };
    unsafe {
        write_u16(
            bus,
            device,
            function,
            descriptor.pcie_capability + 8,
            control | PCIE_DEVICE_CONTROL_FLR,
        )
    };
    wait_after_reset();
    Ok(())
}

fn wait_after_reset() {
    let start = timer::now();
    let ticks = tsc_frequency_hz().map_or(1_000_000_000, |frequency| frequency / 10);
    while timer::now().wrapping_sub(start) < ticks {
        spin_loop();
    }
}

fn tsc_frequency_hz() -> Option<u64> {
    let maximum_leaf = __cpuid(0).eax;
    if maximum_leaf >= 0x15 {
        let ratio = __cpuid(0x15);
        if ratio.eax != 0 && ratio.ebx != 0 && ratio.ecx != 0 {
            return u64::from(ratio.ecx)
                .checked_mul(u64::from(ratio.ebx))?
                .checked_div(u64::from(ratio.eax));
        }
    }
    if maximum_leaf >= 0x16 {
        let mhz = __cpuid(0x16).eax & 0xffff;
        if mhz != 0 {
            return Some(u64::from(mhz) * 1_000_000);
        }
    }
    None
}

unsafe fn find_capability(requester: u16, wanted: u8) -> Result<u8, PciError> {
    let (bus, device, function) = requester_parts(requester)?;
    if unsafe { read_u16(bus, device, function, 6) } & STATUS_CAPABILITIES == 0 {
        return Ok(0);
    }
    let mut offset = unsafe { read_u8(bus, device, function, 0x34) } & !3;
    let mut visited = 0_u64;
    while offset >= 0x40 {
        let bit = 1_u64 << (offset / 4);
        if visited & bit != 0 {
            return Err(PciError::InvalidState);
        }
        visited |= bit;
        if unsafe { read_u8(bus, device, function, offset) } == wanted {
            let required = match wanted {
                CAPABILITY_MSI => 0x14,
                CAPABILITY_MSIX => 0x08,
                CAPABILITY_PCIE => 0x0a,
                _ => 0x02,
            };
            if u16::from(offset) + required > 0x100 {
                return Err(PciError::InvalidState);
            }
            return Ok(offset);
        }
        offset = unsafe { read_u8(bus, device, function, offset + 1) } & !3;
    }
    Ok(0)
}

fn requester_parts(requester: u16) -> Result<(u8, u8, u8), PciError> {
    if requester == 0 {
        return Err(PciError::InvalidRequester);
    }
    Ok((
        (requester >> 8) as u8,
        (requester >> 3 & 0x1f) as u8,
        (requester & 7) as u8,
    ))
}

fn asm_fence() {
    unsafe { asm!("mfence", options(nostack, preserves_flags)) };
}

/// Stops PCI functions in segment zero from initiating new DMA transactions.
///
/// # Safety
/// The caller must execute at CPL0 after firmware device I/O has finished. No
/// other agent may access PCI configuration mechanism 1 concurrently.
pub unsafe fn quarantine_segment_zero() -> QuarantineReport {
    let mut report = QuarantineReport::default();
    let mut queue = [0_u8; 256];
    let mut queued = [false; 256];
    let mut head = 0;
    let mut tail = 1;
    queued[0] = true;
    while head < tail {
        let bus = queue[head];
        head += 1;
        // SAFETY: The public function contract gives this loop exclusive access
        // to PCI configuration mechanism 1.
        unsafe { quarantine_bus(bus, &mut report, &mut queue, &mut queued, &mut tail) };
    }
    report
}

unsafe fn quarantine_bus(
    bus: u8,
    report: &mut QuarantineReport,
    queue: &mut [u8; 256],
    queued: &mut [bool; 256],
    tail: &mut usize,
) {
    for device in 0_u8..32 {
        // SAFETY: quarantine_segment_zero serialized configuration-space access.
        if unsafe { read_u16(bus, device, 0, 0) } == 0xffff {
            continue;
        }
        // SAFETY: Function zero exists and configuration-space access is serialized.
        let header_type = unsafe { read_u8(bus, device, 0, 0x0e) };
        let functions = if header_type & 0x80 != 0 { 8 } else { 1 };
        for function in 0_u8..functions {
            // SAFETY: Configuration-space access remains serialized.
            if unsafe { read_u16(bus, device, function, 0) } == 0xffff {
                continue;
            }
            // SAFETY: The vendor read proved that this function exists.
            if let Some(next_bus) = unsafe { quarantine_function(bus, device, function, report) } {
                enqueue_bus(next_bus, queue, queued, tail);
            }
        }
    }
}

unsafe fn quarantine_function(
    bus: u8,
    device: u8,
    function: u8,
    report: &mut QuarantineReport,
) -> Option<u8> {
    report.functions = report.functions.saturating_add(1);
    // SAFETY: The caller established exclusive configuration-space access and
    // verified this function exists.
    let class = unsafe { read_u8(bus, device, function, 0x0b) };
    // SAFETY: The same serialized, existing function is being read.
    let subclass = unsafe { read_u8(bus, device, function, 0x0a) };
    if report.inventory_count < report.inventory.len() {
        report.inventory[report.inventory_count] = PciFunction {
            requester: requester_id(bus, device, function),
            vendor: unsafe { read_u16(bus, device, function, 0) },
            device: unsafe { read_u16(bus, device, function, 2) },
            class,
            subclass,
        };
        report.inventory_count += 1;
    }
    let next_bus = if class == 0x06 && subclass == 0x04 {
        // SAFETY: PCI-to-PCI bridge headers define byte 0x19 as Secondary Bus.
        Some(unsafe { read_u8(bus, device, function, 0x19) })
    } else if bus == 0 && class == 0x06 && subclass == 0x00 && function != 0 {
        Some(function)
    } else {
        None
    };
    if class == 0x03 && report.display_requester.is_none() {
        report.display_requester = Some(requester_id(bus, device, function));
    }
    // PCI bridge-class functions forward transactions for downstream requester
    // IDs; they do not originate payload DMA under the bridge function's own
    // requester ID. Intel host, ISA/LPC, and PCIe root bridges may also expose a
    // hardwired Command.BusMaster bit. Keep the forwarding path intact and
    // quarantine every discovered endpoint behind it instead.
    if !function_has_bus_master_control(class, subclass) {
        return next_bus.filter(|bus| *bus != 0);
    }
    // SAFETY: The command register exists for every PCI function.
    let command = unsafe { read_u16(bus, device, function, 4) };
    let quarantined = without_bus_master(command);
    if quarantined != command {
        // SAFETY: This writes only the command halfword and preserves all other bits.
        unsafe { write_u16(bus, device, function, 4, quarantined) };
        // SAFETY: Read-back verifies that the device accepted the quarantine.
        if unsafe { read_u16(bus, device, function, 4) } & COMMAND_BUS_MASTER == 0 {
            report.bus_masters_disabled = report.bus_masters_disabled.saturating_add(1);
        } else {
            report.bus_masters_active = report.bus_masters_active.saturating_add(1);
            if report.first_active_requester.is_none() {
                report.first_active_requester = Some(requester_id(bus, device, function));
            }
            if report.active_requester_count < report.active_requesters.len() {
                report.active_requesters[report.active_requester_count] =
                    requester_id(bus, device, function);
                report.active_requester_count += 1;
            }
        }
    }
    next_bus.filter(|bus| *bus != 0)
}

const fn function_has_bus_master_control(class: u8, _subclass: u8) -> bool {
    class != 0x06
}

const fn requester_id(bus: u8, device: u8, function: u8) -> u16 {
    ((bus as u16) << 8) | ((device as u16) << 3) | function as u16
}

const fn without_bus_master(command: u16) -> u16 {
    command & !COMMAND_BUS_MASTER
}

fn enqueue_bus(bus: u8, queue: &mut [u8; 256], queued: &mut [bool; 256], tail: &mut usize) {
    if !queued[bus as usize] {
        queued[bus as usize] = true;
        queue[*tail] = bus;
        *tail += 1;
    }
}

const fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xfc)
}

unsafe fn select(bus: u8, device: u8, function: u8, offset: u8) {
    let address = config_address(bus, device, function, offset);
    // SAFETY: The caller owns PCI configuration ports and executes at CPL0.
    unsafe {
        asm!("out dx, eax", in("dx") CONFIG_ADDRESS, in("eax") address, options(nomem, nostack));
    }
}

unsafe fn read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    // SAFETY: Forwarded from this function's serialized configuration access.
    unsafe { select(bus, device, function, offset) };
    let value: u8;
    let port = CONFIG_DATA + u16::from(offset & 3);
    // SAFETY: The selected PCI configuration byte is readable through this port.
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack)) };
    value
}

unsafe fn read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    // SAFETY: Forwarded from this function's serialized configuration access.
    unsafe { select(bus, device, function, offset) };
    let value: u16;
    let port = CONFIG_DATA + u16::from(offset & 2);
    // SAFETY: The selected PCI configuration halfword is readable through this port.
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack)) };
    value
}

unsafe fn read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    unsafe { select(bus, device, function, offset) };
    let value: u32;
    unsafe { asm!("in eax, dx", in("dx") CONFIG_DATA, out("eax") value, options(nomem, nostack)) };
    value
}

unsafe fn write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    // SAFETY: Forwarded from this function's serialized configuration access.
    unsafe { select(bus, device, function, offset) };
    let port = CONFIG_DATA + u16::from(offset & 2);
    // SAFETY: The selected PCI command halfword is writable through this port.
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack)) };
}

unsafe fn write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    unsafe { select(bus, device, function, offset) };
    unsafe { asm!("out dx, eax", in("dx") CONFIG_DATA, in("eax") value, options(nomem, nostack)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_address_keeps_bdf_and_aligned_offset() {
        assert_eq!(config_address(2, 3, 4, 0x0f), 0x8002_1c0c);
    }

    #[test]
    fn quarantine_changes_only_bus_master_enable() {
        assert_eq!(without_bus_master(0xffff), 0xfffb);
        assert_eq!(without_bus_master(0x0403), 0x0403);
    }

    #[test]
    fn requester_id_uses_the_pci_bdf_layout() {
        assert_eq!(requester_id(0x12, 0x1f, 7), 0x12ff);
    }

    #[test]
    fn bridge_functions_are_not_dma_requester_endpoints() {
        assert!(!function_has_bus_master_control(0x06, 0x00));
        assert!(!function_has_bus_master_control(0x06, 0x01));
        assert!(!function_has_bus_master_control(0x06, 0x04));
        assert!(function_has_bus_master_control(0x03, 0x00));
    }

    #[test]
    fn intel_graphics_range_covers_gtt_and_data_stolen_memory() {
        assert_eq!(
            intel_graphics_stolen_range_from_registers(0x7800_0001, 0x7780_0001, 0x8000_0001),
            Some((0x7780_0000, 0x7fff_ffff))
        );
    }

    #[test]
    fn invalid_intel_graphics_ranges_are_rejected() {
        assert_eq!(intel_graphics_stolen_range_from_registers(0, 0, 0), None);
        assert_eq!(
            intel_graphics_stolen_range_from_registers(0x8000_0000, 0x7f00_0000, 0x8000_0000),
            None
        );
        assert_eq!(
            intel_graphics_stolen_range_from_registers(0x5000_0000, 0x4000_0000, 0x9000_0000),
            None
        );
    }

    fn descriptor(requester: u16, bar_lengths: &[u64]) -> PciDescriptor {
        let mut descriptor = PciDescriptor {
            requester,
            ..PciDescriptor::default()
        };
        for (index, length) in bar_lengths.iter().copied().enumerate() {
            descriptor.bars[index] = PciBar {
                index: index as u8,
                physical_address: 0x8000_0000 + index as u64 * 0x10_0000,
                length,
                guest_address: 0,
            };
            descriptor.bar_count += 1;
        }
        descriptor
    }

    #[test]
    fn device_windows_preserve_firmware_bar_addresses() {
        let mut assignments = AssignmentTable::new();
        let first = assignments
            .insert(2, descriptor(0x10, &[0x2000]), 0x1000_0000, 0x8_0000_0000)
            .unwrap()[0];
        assert_eq!(first.guest_address, 0x8000_0000);
        let second = assignments
            .insert(2, descriptor(0x18, &[0x1000]), 0x1000_0000, 0x8_0000_0000)
            .unwrap()[0];
        assert_eq!(second.guest_address, 0x8000_0000);
        let other_domain = assignments
            .insert(4, descriptor(0x20, &[0x1000]), 0x1000_0000, 0x8_0000_0000)
            .unwrap()[0];
        assert_eq!(other_domain.guest_address, 0x8000_0000);
        assignments.remove(2, 0x10).unwrap();
        let reused = assignments
            .insert(2, descriptor(0x28, &[0x2000]), 0x1000_0000, 0x8_0000_0000)
            .unwrap()[0];
        assert_eq!(reused.guest_address, 0x8000_0000);
    }

    #[test]
    fn resource_query_exposes_only_the_assigned_bar_range() {
        let mut assignments = AssignmentTable::new();
        assignments
            .insert(2, descriptor(0x10, &[0x4000]), 0x1000_0000, 0x8_0000_0000)
            .unwrap();
        let resource = assignments.resource(2, 0x10, 0).unwrap();
        assert_eq!(resource.guest_address, 0x8000_0000);
        assert_eq!(resource.length, 0x4000);
        assert_eq!(assignments.resource(3, 0x10, 0), None);
    }

    #[test]
    fn bar_larger_than_device_window_is_rejected() {
        let mut assignments = AssignmentTable::new();
        assert_eq!(
            assignments.insert(2, descriptor(0x10, &[0x8_0000]), 0x1b_0000, 0x4_0000),
            Err(PciError::DeviceWindowExhausted)
        );
    }

    #[test]
    fn pending_vectors_route_only_to_active_assignments() {
        let mut assignments = AssignmentTable::new();
        assignments
            .insert(2, descriptor(0x10, &[0x1000]), 0x1000_0000, 0x8_0000_0000)
            .unwrap();
        assert!(!assignments.has_active_for_domain(2));
        assignments.assignments[0].active = true;
        assignments.assignments[0].guest_vectors = [0x42, 0x43];
        assignments.assignments[0].interrupt_count = 2;
        assert!(assignments.has_active_for_domain(2));
        assert!(!assignments.has_active_for_domain(3));
        let mut routed = assignments.route_pending(0b11);
        assert_eq!(routed.next(), Some((2, 0x42)));
        assert_eq!(routed.next(), Some((2, 0x43)));
        assert_eq!(routed.next(), None);
        assert!(assignments.route_pending(0b100).next().is_none());
    }

    #[test]
    fn shared_interrupt_routes_only_the_registered_guest_vector() {
        let mut assignments = AssignmentTable::new();
        assignments
            .insert(2, descriptor(0x10, &[0x1000]), 0x1000_0000, 0x8_0000_0000)
            .unwrap();
        assignments.assignments[0].active = true;
        assignments.assignments[0].guest_vectors = [0x42, 0];
        assignments.assignments[0].interrupt_count = 1;
        let mut routed = assignments.route_pending(0b01);
        assert_eq!(routed.next(), Some((2, 0x42)));
        assert_eq!(routed.next(), None);
    }

    #[test]
    fn interrupt_mode_prefers_msix_and_falls_back_to_msi() {
        assert_eq!(
            select_interrupt_mode(2, true),
            Some(PciInterruptMode::MsixSplit)
        );
        assert_eq!(
            select_interrupt_mode(1, true),
            Some(PciInterruptMode::MsixShared)
        );
        assert_eq!(select_interrupt_mode(0, true), Some(PciInterruptMode::Msi));
        assert_eq!(select_interrupt_mode(0, false), None);
    }

    #[test]
    fn shared_and_split_guest_vectors_have_distinct_contracts() {
        assert!(valid_guest_vectors(&[0x42, 0], 1));
        assert!(!valid_guest_vectors(&[0x42, 0x43], 1));
        assert!(valid_guest_vectors(&[0x42, 0x43], 2));
        assert!(!valid_guest_vectors(&[0x42, 0x42], 2));
    }

    #[test]
    fn physical_vectors_are_disjoint_between_assignments() {
        assert_eq!(physical_vector(0, 0), 0x50);
        assert_eq!(physical_vector(0, 1), 0x51);
        assert_eq!(physical_vector(1, 0), 0x52);
        assert_eq!(physical_vector(MAX_ASSIGNMENTS - 1, 1), DEVICE_VECTOR_LAST);
    }
}
