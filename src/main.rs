#![no_std]
#![no_main]

extern crate alloc;

mod display;
mod panic;
mod serial;

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::mem::size_of;
use core::ptr::{copy_nonoverlapping, write_bytes};
use mboot::arch::x86_64::{cpu, descriptor, timer};
use mboot::bundle::NetworkBundle;
use mboot::device::{DeviceError, DeviceTable};
use mboot::domain::{Domain, DomainId, DomainRole, DomainState};
use mboot::event::EventChannelTable;
use mboot::grant::{GrantRef, GrantTable};
use mboot::interrupt::VirtualLocalApic;
use mboot::iommu::{self, IntelTransitionStage, IommuKind};
use mboot::manifest::{
    LaunchManifest, ManifestDeviceKind, ManifestDomainRole, ManifestImageFormat,
    ManifestRestartPolicy, AUTO_REQUESTER,
};
use mboot::memory::{NestedPageResources, NestedPageTable};
use mboot::pci;
use mboot::scheduler::CooperativeScheduler;
use mboot::{
    cpuid, image, BackendKind, CpuidResult, GuestBootMode, GuestConfig, Virtualization,
    VirtualizationResources, VmExitReason,
};
use mnu_abi::hypervisor::{
    DomainBootInfo, DomainCrashInfo, HypercallNumber, PciDeviceInfo, PciDeviceResource,
    DOMAIN_CAPABILITY_DEVICE_CLAIM, DOMAIN_CAPABILITY_DEVICE_QUERY, DOMAIN_CRASH_STATUS_CRASHED,
    DOMAIN_CRASH_STATUS_RESTARTED, DOMAIN_MANAGEMENT_VECTOR, DOMAIN_ROLE_APPLICATION,
    DOMAIN_ROLE_HARDWARE, DOMAIN_ROLE_SYSTEM, EVENT_CHANNEL_NO_EVENT, EVENT_CHANNEL_VECTOR,
    GRANT_FLAG_WRITABLE, HYPERCALL_INVALID_ARGUMENT, HYPERCALL_SUCCESS, HYPERCALL_UNSUPPORTED,
    HYPERVISOR_BACKEND_AMD_SVM, HYPERVISOR_BACKEND_INTEL_VMX,
};
use sha2::{Digest, Sha256};
use uefi::fs::Error as FsError;
use uefi::prelude::*;
#[cfg(feature = "uefi-net")]
use uefi::proto::loaded_image::LoadedImage;
#[cfg(feature = "uefi-net")]
use uefi::proto::network::pxe::{BaseCode, DhcpV4Packet, Mode};
#[cfg(feature = "uefi-net")]
use uefi::proto::network::IpAddress;
use uefi::proto::rng::Rng;
#[cfg(feature = "uefi-net")]
use uefi::table::boot::ScopedProtocol;
use uefi::table::boot::{AllocateType, BootServices, MemoryType};
use uefi::table::cfg::{ACPI_GUID, ACPI2_GUID};
#[cfg(feature = "uefi-net")]
use uefi::CStr8;
use uefi::CString16;

const MAX_MEMORY_REGIONS: usize = 256;
const MAX_GUEST_MEMORY_PAGES: usize = 65_536;
const GRANT_WINDOW_PAGES: usize = 16;
const DEVICE_WINDOW_PAGES: usize = 64;
const DOMAIN_STACK_BYTES: u64 = 1024 * 1024;
const DEVICE_WINDOW_START: u64 = 0x1000_0000;
const DEVICE_WINDOW_LIMIT: u64 = mnu_abi::hypervisor::DOMAIN_DEVICE_ADDRESS_LIMIT;
const SPARSE_LEVEL2_PAGES: usize = 512;
const SPARSE_LEVEL3_PAGES: usize = 2;
const SPARSE_LEVEL1_PAGES: usize = 256;
const DOMAIN_BOOT_INFO_GPA: u64 = 0x3000;
const MAX_CONSOLE_WRITE: u64 = 4096;
const LAUNCH_MANIFEST_PATH: &str = "\\EFI\\MBOOT\\LAUNCH.MF";
#[cfg(feature = "uefi-net")]
const NETWORK_BUNDLE_NAME: &[u8] = b"mboot.bundle";
#[cfg(feature = "uefi-net")]
const MAX_NETWORK_BUNDLE_SIZE: usize = 256 * 1024 * 1024;

include!(concat!(env!("OUT_DIR"), "/launch_manifest_digest.rs"));

macro_rules! log {
    ($($arg:tt)*) => {
        crate::serial::print(format_args!("[mBoot] {}\n", format_args!($($arg)*)))
    };
}

#[derive(Clone, Copy)]
struct MemoryRegion {
    start: u64,
    len: u64,
    usable: bool,
    mmio: bool,
}

impl MemoryRegion {
    const EMPTY: Self = Self {
        start: 0,
        len: 0,
        usable: false,
        mmio: false,
    };
}

struct BootMemoryMap {
    regions: [MemoryRegion; MAX_MEMORY_REGIONS],
    len: usize,
    overflowed: bool,
}

struct PreparedDomain {
    id: u32,
    role: DomainRole,
    capabilities: u64,
    restart_policy: ManifestRestartPolicy,
    max_restarts: u8,
    image: Vec<u8>,
    image_format: ManifestImageFormat,
    initramfs: Option<Vec<u8>>,
    command_line: String,
    nested_pages: NestedPageResources,
    vcpu_control_page: u64,
    msr_permission_map: u64,
    msr_state_page: u64,
    entropy_root: [u8; 32],
}

#[derive(Clone, Copy)]
enum ResumeKind {
    Hypercall,
    WithoutAdvance,
    Halted,
    MsrRead(u64),
    MsrWrite,
    Cpuid(CpuidResult),
    ControlRegisterWrite { register: u8, value: u64 },
    GeneralProtection,
}

struct RuntimeDomain {
    domain: Domain,
    virtualization: Virtualization,
    guest: GuestConfig,
    started: bool,
    pending_result: u64,
    yield_count: u64,
    preemption_count: u64,
    ready: bool,
    waiting: bool,
    resume_kind: ResumeKind,
    event_irq_enabled: bool,
    interrupts: VirtualLocalApic,
    restart_policy: ManifestRestartPolicy,
    max_restarts: u8,
    restart_count: u32,
    crash_info: Option<DomainCrashInfo>,
    image: Vec<u8>,
    image_format: ManifestImageFormat,
    initramfs: Option<Vec<u8>>,
    command_line: String,
    entropy_root: [u8; 32],
}

impl BootMemoryMap {
    const fn new() -> Self {
        Self {
            regions: [MemoryRegion::EMPTY; MAX_MEMORY_REGIONS],
            len: 0,
            overflowed: false,
        }
    }

    fn push(&mut self, region: MemoryRegion) {
        if self.len < self.regions.len() {
            self.regions[self.len] = region;
            self.len += 1;
        } else {
            self.overflowed = true;
        }
    }

    fn usable_bytes(&self) -> u64 {
        self.regions[..self.len]
            .iter()
            .filter(|region| region.usable)
            .map(|region| region.len)
            .sum()
    }

    fn lowest_address(&self) -> u64 {
        self.regions[..self.len]
            .iter()
            .map(|region| region.start)
            .min()
            .unwrap_or(0)
    }

    fn allows_device_mmio(&self, start: u64, len: u64) -> bool {
        let Some(end) = start.checked_add(len) else {
            return false;
        };
        !self.overflowed
            && start != 0
            && len != 0
            && start & 0xfff == 0
            && len & 0xfff == 0
            && self.regions[..self.len].iter().all(|region| {
                let region_end = region.start.saturating_add(region.len);
                end <= region.start || start >= region_end || region.mmio
            })
    }
}

#[entry]
unsafe fn main(image_handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    serial::init();
    log!("starting independent hypervisor");
    if let Err(error) = uefi::helpers::init(&mut system_table) {
        log!("UEFI helper initialization failed: {:?}", error.status());
        return error.status();
    }
    let has_display = display::initialize(system_table.boot_services());
    log!("boot display available={}", has_display);

    let rsdp_address = system_table
        .config_table()
        .iter()
        .find(|entry| entry.guid == ACPI2_GUID)
        .or_else(|| {
            system_table
                .config_table()
                .iter()
                .find(|entry| entry.guid == ACPI_GUID)
        })
        .map(|entry| entry.address as usize as u64);

    let features = cpu::detect();
    let vendor = core::str::from_utf8(&features.vendor).unwrap_or("unknown");
    let Some(backend) = features.backend else {
        log!("CPU {} has neither VMX nor SVM", vendor);
        display::failure(1);
        return Status::UNSUPPORTED;
    };
    display::backend(backend == BackendKind::IntelVmx);
    log!(
        "CPU {} backend={:?} nested-paging={:?} asids={}",
        vendor,
        backend,
        features.nested_paging,
        features.address_space_ids
    );

    let iommu_topology = match rsdp_address {
        // SAFETY: UEFI supplied this ACPI 2.0 RSDP pointer and Boot Services are
        // still active, so the firmware tables remain identity-mapped here.
        Some(address) => match unsafe { iommu::discover(address) } {
            Ok(topology) => topology,
            Err(error) => {
                log!("invalid ACPI IOMMU description: {:?}", error);
                display::failure(15);
                return Status::SECURITY_VIOLATION;
            }
        },
        None => None,
    };
    if let Some(topology) = iommu_topology {
        let expected = match backend {
            BackendKind::IntelVmx => IommuKind::IntelVtd,
            BackendKind::AmdSvm => IommuKind::AmdVi,
        };
        if topology.kind() != expected {
            log!(
                "IOMMU description {:?} does not match CPU backend {:?}",
                topology.kind(),
                backend
            );
            display::failure(16);
            return Status::SECURITY_VIOLATION;
        }
        log!(
            "IOMMU description {:?}: {} remapping unit(s), {} reserved mapping(s)",
            topology.kind(),
            topology.unit_count(),
            topology.reserved_mappings().len()
        );
        for unit in topology.units() {
            log!(
                "IOMMU unit: segment={} registers={:#x} include-all={}",
                unit.segment,
                unit.register_base,
                unit.include_all
            );
        }
    } else {
        log!("IOMMU description unavailable; device assignment remains disabled");
    }

    let boot_services = system_table.boot_services();
    let Some(boot_entropy) = collect_boot_entropy(boot_services) else {
        log!("no secure boot entropy source is available");
        display::failure(61);
        return Status::SECURITY_VIOLATION;
    };
    log!("secure boot entropy collected");
    let iommu_resources = match iommu_topology {
        Some(topology) => match allocate_iommu_resources(boot_services, topology) {
            Ok(tables) => tables,
            Err(status) => {
                log!("failed to allocate deny-all IOMMU tables: {:?}", status);
                display::failure(18);
                return status;
            }
        },
        None => Vec::new(),
    };
    #[cfg(feature = "uefi-net")]
    let network_bundle = match load_network_bundle(boot_services, image_handle) {
        Ok(bytes) => {
            log!("downloaded network bundle: {} bytes", bytes.len());
            Some(bytes)
        }
        Err(status) => {
            log!("failed to download network bundle: {:?}", status);
            display::failure(60);
            return status;
        }
    };
    #[cfg(not(feature = "uefi-net"))]
    let network_bundle: Option<Vec<u8>> = None;

    let manifest_bytes = match load_file(
        boot_services,
        image_handle,
        network_bundle.as_deref(),
        LAUNCH_MANIFEST_PATH,
    ) {
        Ok(bytes) => bytes,
        Err(status) => {
            log!("failed to load Launch Manifest: {:?}", status);
            display::failure(2);
            return status;
        }
    };
    let manifest = match LaunchManifest::parse(&manifest_bytes, EMBEDDED_LAUNCH_MANIFEST_SHA256) {
        Ok(manifest) => manifest,
        Err(error) => {
            log!("Launch Manifest verification failed: {:?}", error);
            display::failure(3);
            return Status::SECURITY_VIOLATION;
        }
    };
    if backend == BackendKind::AmdSvm
        && manifest.domain_count() >= features.address_space_ids as usize
    {
        log!(
            "{} Domains exceed the {} usable AMD ASID slots",
            manifest.domain_count(),
            features.address_space_ids.saturating_sub(1)
        );
        display::failure(4);
        return Status::UNSUPPORTED;
    }
    let host_control_page = match allocate_page(boot_services) {
        Ok(page) => page,
        Err(status) => {
            display::failure(11);
            return status;
        }
    };
    let mut prepared_domains = Vec::with_capacity(manifest.domain_count());
    let mut runtime_domains: Vec<RuntimeDomain> = Vec::with_capacity(manifest.domain_count());
    let mut runnable = Vec::with_capacity(manifest.domain_count());
    let mut system_domains = 0;
    for index in 0..manifest.domain_count() {
        let config = match manifest.domain(index) {
            Ok(config) => config,
            Err(error) => {
                log!("invalid Domain entry {}: {:?}", index, error);
                display::failure(5);
                return Status::LOAD_ERROR;
            }
        };
        let role = match config.role {
            ManifestDomainRole::System => {
                system_domains += 1;
                DomainRole::System
            }
            ManifestDomainRole::Hardware => DomainRole::Hardware,
            ManifestDomainRole::Application => DomainRole::Application,
        };
        let guest_pages = (config.memory_size / 4096) as usize;
        if !config.auto_starts()
            || config.vcpu_count != 1
            || !(GRANT_WINDOW_PAGES + DEVICE_WINDOW_PAGES + 16..=MAX_GUEST_MEMORY_PAGES)
                .contains(&guest_pages)
        {
            log!("unsupported configuration for Domain {}", config.id);
            display::failure(6);
            return Status::UNSUPPORTED;
        }
        let image = match load_file(
            boot_services,
            image_handle,
            network_bundle.as_deref(),
            config.image_path,
        ) {
            Ok(image) => image,
            Err(status) => {
                log!(
                    "failed to load Domain {} image {}: {:?}",
                    config.id,
                    config.image_path,
                    status
                );
                display::failure(7);
                return status;
            }
        };
        if let Err(error) = config.verify_image(&image) {
            log!(
                "Domain {} image verification failed: {:?}",
                config.id,
                error
            );
            display::failure(8);
            return Status::SECURITY_VIOLATION;
        }
        let initramfs = match config.initramfs_path {
            Some(path) => {
                let image =
                    match load_file(boot_services, image_handle, network_bundle.as_deref(), path) {
                        Ok(image) => image,
                        Err(status) => {
                            log!(
                                "failed to load Domain {} initramfs {}: {:?}",
                                config.id,
                                path,
                                status
                            );
                            display::failure(7);
                            return status;
                        }
                    };
                if let Err(error) = config.verify_initramfs(&image) {
                    log!(
                        "Domain {} initramfs verification failed: {:?}",
                        config.id,
                        error
                    );
                    display::failure(8);
                    return Status::SECURITY_VIOLATION;
                }
                Some(image)
            }
            None => None,
        };
        let vcpu_control_page = match allocate_page(boot_services) {
            Ok(page) => page,
            Err(status) => {
                display::failure(12);
                return status;
            }
        };
        let msr_permission_map =
            match allocate_msr_permission_map(boot_services, backend, config.image_format) {
                Ok(map) => map,
                Err(status) => {
                    display::failure(14);
                    return status;
                }
            };
        let msr_state_page = match allocate_page(boot_services) {
            Ok(page) => page,
            Err(status) => {
                display::failure(14);
                return status;
            }
        };
        let nested_pages = match allocate_nested_pages(boot_services, guest_pages) {
            Ok(pages) => pages,
            Err(status) => {
                display::failure(13);
                return status;
            }
        };
        prepared_domains.push(PreparedDomain {
            id: config.id,
            role,
            capabilities: config.capabilities,
            restart_policy: config.restart_policy,
            max_restarts: config.max_restarts,
            image,
            image_format: config.image_format,
            initramfs,
            command_line: String::from(config.command_line),
            nested_pages,
            vcpu_control_page,
            msr_permission_map,
            msr_state_page,
            entropy_root: boot_entropy,
        });
        runnable.push(true);
    }
    if system_domains != 1 {
        log!("Launch Manifest must contain exactly one System Domain");
        display::failure(9);
        return Status::UNSUPPORTED;
    }
    let mut device_policies = Vec::with_capacity(manifest.device_count());
    for index in 0..manifest.device_count() {
        match manifest.device(index) {
            Ok(device) => device_policies.push(device),
            Err(error) => {
                log!("invalid device policy {}: {:?}", index, error);
                display::failure(5);
                return Status::LOAD_ERROR;
            }
        }
    }
    let firmware_framebuffer = display::framebuffer_info();
    let firmware_framebuffer_fallback = iommu_topology.is_none() && firmware_framebuffer.is_some();
    if !device_policies.is_empty() && iommu_topology.is_none() {
        let automatic_fallback_is_safe = firmware_framebuffer_fallback
            && device_policies.iter().all(|policy| {
                policy.requester == AUTO_REQUESTER
                    && (!policy.is_required() || policy.kind == ManifestDeviceKind::Display)
            });
        if automatic_fallback_is_safe {
            log!(
                "IOMMU unavailable; disabling {} automatic PCI assignment policy entry(s) and retaining the firmware framebuffer",
                device_policies.len()
            );
            device_policies.clear();
        } else {
            log!("device assignment requires an ACPI IOMMU description");
            display::failure(17);
            return Status::UNSUPPORTED;
        }
    }

    // SAFETY: All required firmware allocations are complete and no boot service
    // is used after this call.
    let (_runtime, firmware_map) =
        unsafe { system_table.exit_boot_services(MemoryType::LOADER_DATA) };
    let mut memory_map = BootMemoryMap::new();
    for descriptor in firmware_map.entries() {
        memory_map.push(MemoryRegion {
            start: descriptor.phys_start,
            len: descriptor.page_count * 4096,
            usable: descriptor.ty == MemoryType::CONVENTIONAL,
            mmio: descriptor.ty == MemoryType::MMIO,
        });
    }

    // SAFETY: UEFI entered at CPL0; boot services are gone and mBoot now owns
    // interrupt and descriptor-table policy.
    unsafe {
        asm!("cli", options(nomem, nostack));
        descriptor::install();
    }
    // SAFETY: Firmware I/O has ended, interrupts are disabled, and mBoot is the
    // sole PCI configuration-space owner from this point onward.
    let quarantine = unsafe { pci::quarantine_segment_zero() };
    log!(
        "PCI DMA quarantine: {} function(s), {} bus master(s) disabled, {} still active",
        quarantine.functions,
        quarantine.bus_masters_disabled,
        quarantine.bus_masters_active
    );
    if let Some(requester) = quarantine.first_active_requester {
        log!(
            "PCI DMA quarantine could not disable {:02x}:{:02x}.{}",
            requester >> 8,
            requester >> 3 & 0x1f,
            requester & 7
        );
    }
    if quarantine.bus_masters_active != 0 {
        let every_active_requester_is_reserved = quarantine.recorded_every_active_requester()
            && iommu_topology.is_some_and(|topology| {
                quarantine.active_requesters().iter().all(|requester| {
                    topology
                        .reserved_mappings()
                        .iter()
                        .any(|mapping| mapping.segment == 0 && mapping.requester == *requester)
                })
            });
        if !every_active_requester_is_reserved {
            if let Some(requester) = quarantine.first_active_requester {
                display::pci_dma_failure(requester);
                log!(
                    "PCI DMA quarantine initialization failed: {:?}",
                    mboot::Error::DeviceQuarantineFailed
                );
                halt()
            }
            halt_with_error("PCI DMA quarantine", mboot::Error::DeviceQuarantineFailed)
        }
        for requester in quarantine.active_requesters() {
            log!(
                "PCI {:02x}:{:02x}.{} remains active only for its reserved DMA mapping",
                requester >> 8,
                requester >> 3 & 0x1f,
                requester & 7
            );
        }
    }
    let deferred_display = quarantine.display_requester.filter(|requester| {
        !quarantine.active_requesters().contains(requester)
            && iommu_topology.is_some_and(|topology| topology.covers_requester(0, *requester))
    });
    if let Some(requester) = deferred_display {
        log!(
            "PCI display {:02x}:{:02x}.{} keeps its firmware VT-d unit until the Hardware Domain takes ownership",
            requester >> 8,
            requester >> 3 & 0x1f,
            requester & 7
        );
    }
    let mut devices =
        match DeviceTable::from_pci(quarantine.inventory(), deferred_display, &device_policies) {
            Ok(devices) => devices,
            Err(DeviceError::DeviceUnavailable) => {
                log!("PCI ownership policy failed: required device unavailable");
                let mut storage = quarantine
                    .inventory()
                    .iter()
                    .copied()
                    .filter(|function| function.class == 0x01);
                display::storage_candidates(
                    storage.next().map(pci_identity),
                    storage.next().map(pci_identity),
                );
                halt()
            }
            Err(DeviceError::AmbiguousDevice(first, second)) => {
                log!("PCI ownership policy failed: automatic device selection is ambiguous");
                display::nvme_candidates(
                    (first.requester, first.vendor, first.device),
                    (second.requester, second.vendor, second.device),
                );
                halt()
            }
            Err(error) => {
                log!("PCI ownership policy failed: {:?}", error);
                halt_with_error("PCI ownership policy", mboot::Error::InvalidManifest)
            }
        };
    let mut pci_assignments = pci::AssignmentTable::new();
    log!("PCI ownership table: {} device(s)", devices.len());
    let mut dma_remapper = if let Some(topology) = iommu_topology {
        // SAFETY: Tables were allocated and zeroed before ExitBootServices, PCI
        // bus mastering is disabled, and mBoot now exclusively owns IOMMU MMIO.
        let remapper = match unsafe {
            iommu::DmaRemapper::initialize(topology, &iommu_resources, deferred_display)
        } {
            Ok(remapper) => remapper,
            Err(error) => halt_with_error("IOMMU protection", iommu_error(error)),
        };
        log!(
            "IOMMU DMA protection enabled: {:?} {}",
            topology.kind(),
            if deferred_display.is_some() {
                "non-display protected; firmware display deferred"
            } else if topology.reserved_mappings().is_empty() {
                "deny-all"
            } else {
                "firmware-reserved-only"
            }
        );
        Some(remapper)
    } else {
        None
    };
    let preemption_timer = unsafe { timer::initialize() };
    log!(
        "vCPU preemption timer {}",
        if preemption_timer {
            "enabled"
        } else {
            "unavailable"
        }
    );
    log!(
        "owned memory map: regions={} usable={} MiB lowest={:#x}",
        memory_map.len,
        memory_map.usable_bytes() / (1024 * 1024),
        memory_map.lowest_address()
    );

    for (index, prepared) in prepared_domains.drain(..).enumerate() {
        // SAFETY: Every page was allocated from UEFI for exclusive mBoot use and
        // remains identity-mapped.
        let nested = match unsafe { NestedPageTable::initialize(backend, prepared.nested_pages) } {
            Ok(table) => table,
            Err(error) => halt_with_error("nested page table", error),
        };
        // SAFETY: This stopped Domain exclusively owns its guest pages.
        let guest_cr3 = if prepared.image_format == ManifestImageFormat::NativeElf {
            match unsafe { nested.initialize_guest_page_tables() } {
                Ok(root) => root,
                Err(error) => halt_with_error("guest page tables", error),
            }
        } else {
            0
        };
        let domain_framebuffer = if firmware_framebuffer_fallback
            && prepared.role == DomainRole::System
            && prepared.image_format == ManifestImageFormat::NativeElf
        {
            let framebuffer = firmware_framebuffer.expect("fallback requires a framebuffer");
            let host_page = framebuffer.address & !0xfff;
            let offset = framebuffer.address - host_page;
            let Some(mapped_len) = framebuffer
                .size
                .checked_add(offset)
                .and_then(|length| length.checked_add(0xfff))
                .map(|length| length & !0xfff)
            else {
                halt_with_error("firmware framebuffer", mboot::Error::InvalidPage)
            };
            let guest_page = align_up_4k(nested.guest_memory_size().max(DEVICE_WINDOW_START));
            let Some(guest_address) = guest_page.checked_add(offset) else {
                halt_with_error("firmware framebuffer", mboot::Error::InvalidPage)
            };
            // SAFETY: GOP supplied the physical framebuffer range. The System
            // Domain is stopped, and no PCI function is assigned in fallback mode.
            if unsafe { nested.map_device_range(guest_page, host_page, mapped_len) }
                .and_then(|()| unsafe {
                    nested.map_guest_identity_device_range(guest_page, mapped_len)
                })
                .is_err()
            {
                halt_with_error("firmware framebuffer", mboot::Error::InvalidPage)
            }
            log!(
                "firmware framebuffer mapped into System Domain: guest={:#x} host={:#x} size={} {}x{} stride={}",
                guest_address,
                framebuffer.address,
                framebuffer.size,
                framebuffer.width,
                framebuffer.height,
                framebuffer.stride
            );
            Some((guest_address, framebuffer))
        } else {
            None
        };
        // SAFETY: The Domain is stopped and its RAM is exclusively owned by mBoot.
        let guest_image = match unsafe {
            match prepared.image_format {
                ManifestImageFormat::NativeElf => image::load_elf(&prepared.image, &nested),
                ManifestImageFormat::LinuxPvh => image::load_linux_pvh(
                    &prepared.image,
                    prepared.initramfs.as_deref(),
                    &prepared.command_line,
                    0,
                    device_window_start(&nested),
                    &nested,
                ),
            }
        } {
            Ok(image) => image,
            Err(error) => halt_with_error("Domain image", error),
        };
        let (boot_module_start, boot_module_size) =
            if prepared.image_format == ManifestImageFormat::NativeElf {
                match prepared.initramfs.as_deref() {
                    Some(module) => match unsafe {
                        image::load_boot_module(
                            module,
                            guest_image.loaded_end(),
                            domain_stack_bottom(&nested),
                            &nested,
                        )
                    } {
                        Ok(module) => module,
                        Err(error) => halt_with_error("Domain boot module", error),
                    },
                    None => (0, 0),
                }
            } else {
                (0, 0)
            };
        // SAFETY: Control pages are exclusive, execution is pinned to the BSP,
        // interrupts are disabled, and this code runs at CPL0.
        let virtualization = if index == 0 {
            // SAFETY: The preconditions above apply to the BSP's first vCPU.
            match unsafe {
                Virtualization::enable(VirtualizationResources {
                    host_control_page,
                    vcpu_control_page: prepared.vcpu_control_page,
                })
            } {
                Ok(virtualization) => virtualization,
                Err(error) => halt_with_error("virtualization", error),
            }
        } else {
            // SAFETY: The first backend is active on this BSP and the new control
            // page and address-space identifier are exclusive to this vCPU.
            match unsafe {
                runtime_domains[0]
                    .virtualization
                    .create_vcpu(prepared.vcpu_control_page, index as u32 + 1)
            } {
                Ok(virtualization) => virtualization,
                Err(error) => halt_with_error("vCPU creation", error),
            }
        };
        let mut domain = Domain::new(
            DomainId::new(prepared.id),
            prepared.role,
            prepared.capabilities,
            virtualization.kind(),
            nested,
        );
        if let Err(error) = domain.mark_ready() {
            halt_with_error("Domain", error);
        }
        let backend_id = match domain.backend() {
            BackendKind::IntelVmx => HYPERVISOR_BACKEND_INTEL_VMX,
            BackendKind::AmdSvm => HYPERVISOR_BACKEND_AMD_SVM,
        };
        let mut boot_info = DomainBootInfo::new(
            domain.id().get(),
            0,
            backend_id,
            abi_domain_role(domain.role()),
            domain.nested_pages().guest_memory_size(),
            boot_module_start,
            boot_module_size,
            grant_window_start(domain.nested_pages()),
            GRANT_WINDOW_PAGES as u64 * 4096,
            DEVICE_WINDOW_START,
            DEVICE_WINDOW_LIMIT - DEVICE_WINDOW_START,
            0,
            domain.capabilities(),
        )
        .with_entropy_seed(derive_domain_entropy(
            &prepared.entropy_root,
            prepared.id,
            0,
        ));
        if let Some((guest_address, framebuffer)) = domain_framebuffer {
            boot_info = boot_info.with_framebuffer(
                guest_address,
                framebuffer.size,
                framebuffer.width,
                framebuffer.height,
                framebuffer.stride,
                framebuffer.format,
            );
        }
        let Some(boot_info_host) = domain
            .nested_pages()
            .guest_host_address(DOMAIN_BOOT_INFO_GPA, size_of::<DomainBootInfo>() as u64)
        else {
            halt_with_error("Domain boot info", mboot::Error::InvalidPage)
        };
        // SAFETY: The destination is aligned, in bounds, and the Domain is stopped.
        unsafe {
            copy_nonoverlapping(
                &boot_info as *const DomainBootInfo,
                boot_info_host as *mut DomainBootInfo,
                1,
            )
        };
        if let Err(error) = domain.start() {
            halt_with_error("Domain start", error);
        }
        log!(
            "Domain {} ready: role={:?} capabilities={:#x} backend={:?} nested-root={:#x} guest-memory={:#x}+{} KiB",
            domain.id().get(),
            domain.role(),
            domain.capabilities(),
            domain.backend(),
            domain.nested_pages().hardware_root(),
            domain.nested_pages().guest_base(),
            domain.nested_pages().guest_memory_size() / 1024
        );
        runtime_domains.push(RuntimeDomain {
            guest: GuestConfig {
                boot_mode: match prepared.image_format {
                    ManifestImageFormat::NativeElf => GuestBootMode::Long64,
                    ManifestImageFormat::LinuxPvh => GuestBootMode::LinuxPvh32,
                },
                nested_root: domain.nested_pages().hardware_root(),
                page_table_root: guest_cr3,
                entry: guest_image.entry(),
                stack: domain_stack_pointer(domain.nested_pages()),
                boot_info: if prepared.image_format == ManifestImageFormat::NativeElf {
                    DOMAIN_BOOT_INFO_GPA
                } else {
                    guest_image.boot_info()
                },
                msr_permission_map: prepared.msr_permission_map,
                msr_state_page: prepared.msr_state_page,
            },
            domain,
            virtualization,
            started: false,
            pending_result: HYPERCALL_SUCCESS,
            yield_count: 0,
            preemption_count: 0,
            ready: false,
            waiting: false,
            resume_kind: ResumeKind::Hypercall,
            event_irq_enabled: false,
            interrupts: if prepared.image_format == ManifestImageFormat::LinuxPvh {
                VirtualLocalApic::new_x2apic()
            } else {
                VirtualLocalApic::new()
            },
            restart_policy: prepared.restart_policy,
            max_restarts: prepared.max_restarts,
            restart_count: 0,
            crash_info: None,
            image: prepared.image,
            image_format: prepared.image_format,
            initramfs: prepared.initramfs,
            command_line: prepared.command_line,
            entropy_root: prepared.entropy_root,
        });
    }

    let mut event_channels = EventChannelTable::new();
    let mut grants = GrantTable::new();
    for index in 0..manifest.event_channel_count() {
        let channel = match manifest.event_channel(index) {
            Ok(channel) => channel,
            Err(error) => halt_with_error("Event Channel manifest", error),
        };
        if event_channels
            .connect(
                DomainId::new(channel.domain_a),
                channel.port_a,
                DomainId::new(channel.domain_b),
                channel.port_b,
            )
            .is_err()
        {
            halt_with_error("Event Channel manifest", mboot::Error::InvalidManifest)
        }
        log!(
            "Event Channel connected: {}:{} <-> {}:{}",
            channel.domain_a,
            channel.port_a,
            channel.domain_b,
            channel.port_b
        );
    }

    let mut scheduler = CooperativeScheduler::new();
    loop {
        let pending_devices = pci::take_pending_device_interrupts();
        for (domain_id, vector) in pci_assignments.route_pending(pending_devices) {
            let Some(target) = runtime_domains
                .iter()
                .position(|runtime| runtime.domain.id().get() == domain_id)
            else {
                halt_with_error("PCI IRQ routing", mboot::Error::InvalidState)
            };
            if runtime_domains[target].interrupts.raise(vector).is_err() {
                halt_with_error("PCI IRQ routing", mboot::Error::InvalidState)
            }
            runtime_domains[target].waiting = false;
            runnable[target] = true;
        }
        let Some(index) = scheduler.next(&runnable) else {
            if pci_assignments.has_active() {
                // SAFETY: The mBoot IDT owns every enabled device vector. STI is
                // immediately followed by HLT, and the handler returns with IF clear.
                unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
                continue;
            }
            break;
        };
        let runtime = &mut runtime_domains[index];
        if runtime.interrupts.update_timer(timer::now()).is_err() {
            halt_with_error("Virtual APIC timer", mboot::Error::InvalidState)
        }
        if !matches!(runtime.resume_kind, ResumeKind::GeneralProtection) {
            if let Some(vector) = runtime.interrupts.next_pending() {
                let can_inject = match unsafe { runtime.virtualization.can_inject_interrupt() } {
                    Ok(can_inject) => can_inject,
                    Err(error) => halt_with_error("Event IRQ readiness", error),
                };
                if can_inject {
                    if let Err(error) =
                        unsafe { runtime.virtualization.set_interrupt_window(false) }
                    {
                        halt_with_error("Interrupt window disable", error)
                    }
                    if let Err(error) = unsafe { runtime.virtualization.inject_interrupt(vector) } {
                        halt_with_error("Event IRQ injection", error)
                    }
                    if runtime.interrupts.accept(vector).is_err() {
                        halt_with_error("Virtual APIC accept", mboot::Error::InvalidState)
                    }
                } else if let Err(error) =
                    unsafe { runtime.virtualization.set_interrupt_window(true) }
                {
                    halt_with_error("Interrupt window enable", error)
                }
            }
        }
        // SAFETY: The selected vCPU is stopped and owns all guest and control state.
        let vm_exit = if runtime.started {
            match runtime.resume_kind {
                ResumeKind::Hypercall => {
                    // SAFETY: This vCPU stopped at a Hypercall and remains selected.
                    unsafe { runtime.virtualization.resume(runtime.pending_result) }
                }
                ResumeKind::WithoutAdvance => {
                    // SAFETY: No guest instruction completed at the previous exit.
                    unsafe { runtime.virtualization.resume_preempted() }
                }
                ResumeKind::Halted => {
                    // SAFETY: This vCPU stopped on an intercepted HLT.
                    unsafe { runtime.virtualization.resume_halted() }
                }
                ResumeKind::MsrRead(value) => {
                    // SAFETY: The value completes the preceding intercepted RDMSR.
                    unsafe { runtime.virtualization.resume_msr_read(value) }
                }
                ResumeKind::MsrWrite => {
                    // SAFETY: The preceding intercepted WRMSR was emulated.
                    unsafe { runtime.virtualization.resume_msr_write() }
                }
                ResumeKind::Cpuid(result) => {
                    // SAFETY: These values complete the preceding intercepted CPUID.
                    unsafe { runtime.virtualization.resume_cpuid(result) }
                }
                ResumeKind::ControlRegisterWrite { register, value } => unsafe {
                    runtime
                        .virtualization
                        .resume_control_register_write(register, value)
                },
                ResumeKind::GeneralProtection => {
                    // SAFETY: The vCPU is stopped at the rejected instruction.
                    if let Err(error) =
                        unsafe { runtime.virtualization.inject_general_protection() }
                    {
                        halt_with_error("Domain exception injection", error)
                    }
                    // SAFETY: Fault delivery must preserve the faulting guest RIP.
                    unsafe { runtime.virtualization.resume_preempted() }
                }
            }
        } else {
            runtime.started = true;
            // SAFETY: This is the first entry into the stopped, fully prepared vCPU.
            unsafe { runtime.virtualization.run(runtime.guest) }
        };
        let vm_exit = match vm_exit {
            Ok(exit) => exit,
            Err(mboot::Error::UnexpectedVmExit(raw_reason)) => {
                let instruction_pointer = unsafe {
                    runtime_domains[index]
                        .virtualization
                        .guest_instruction_pointer()
                }
                .unwrap_or(0);
                let (cr0, cr3) = unsafe {
                    runtime_domains[index]
                        .virtualization
                        .guest_paging_state()
                }
                .unwrap_or((0, 0));
                let paging_state = (cr0 << 32) | (cr3 & u64::from(u32::MAX));
                isolate_crashed_domain(
                    index,
                    &mut runtime_domains,
                    &mut runnable,
                    &mut grants,
                    &mut event_channels,
                    &mut devices,
                    &mut pci_assignments,
                    &mut dma_remapper,
                    manifest,
                    raw_reason,
                    instruction_pointer,
                    paging_state,
                );
                continue;
            }
            Err(error) => halt_with_error("vCPU entry", error),
        };
        if vm_exit.reason == VmExitReason::Preempted {
            // VMX acknowledges the interrupt during VM exit and reports its
            // vector here. SVM dispatches the pending interrupt through the
            // host IDT before returning from its backend.
            if vm_exit.fault_info & (1 << 31) != 0 {
                pci::acknowledge_vmexit_interrupt(vm_exit.fault_info as u8);
            }
            runtime.preemption_count += 1;
            if runtime.preemption_count <= 3 {
                log!(
                    "Domain {} preempted: count={}",
                    runtime.domain.id().get(),
                    runtime.preemption_count
                );
            }
            runtime.resume_kind = ResumeKind::WithoutAdvance;
            continue;
        }
        if vm_exit.reason == VmExitReason::InterruptWindow {
            runtime.resume_kind = ResumeKind::WithoutAdvance;
            continue;
        }
        if vm_exit.reason == VmExitReason::Halt {
            runtime.resume_kind = ResumeKind::Halted;
            continue;
        }
        if vm_exit.reason == VmExitReason::MsrRead {
            if runtime.interrupts.update_timer(timer::now()).is_err() {
                halt_with_error("Virtual APIC timer", mboot::Error::InvalidState)
            }
            let value = runtime
                .interrupts
                .read_msr(vm_exit.msr)
                .ok()
                .or_else(|| unsafe {
                    runtime
                        .virtualization
                        .read_guest_msr(vm_exit.msr)
                        .ok()
                });
            runtime.resume_kind =
                value.map_or(ResumeKind::GeneralProtection, ResumeKind::MsrRead);
            continue;
        }
        if vm_exit.reason == VmExitReason::MsrWrite {
            if runtime.interrupts.update_timer(timer::now()).is_err() {
                halt_with_error("Virtual APIC timer", mboot::Error::InvalidState)
            }
            let written = runtime
                .interrupts
                .write_msr(vm_exit.msr, vm_exit.msr_value)
                .is_ok()
                || unsafe {
                    runtime
                        .virtualization
                        .write_guest_msr(vm_exit.msr, vm_exit.msr_value)
                        .is_ok()
                };
            runtime.resume_kind = if written {
                ResumeKind::MsrWrite
            } else {
                ResumeKind::GeneralProtection
            };
            continue;
        }
        if vm_exit.reason == VmExitReason::Cpuid {
            let result = cpuid::query(
                vm_exit.cpuid_leaf,
                vm_exit.cpuid_subleaf,
                0,
                1,
                timer::tsc_frequency_hz()
                    .and_then(|frequency| u32::try_from(frequency / 1_000).ok())
                    .unwrap_or(1_000_000),
                match runtime.domain.backend() {
                    BackendKind::IntelVmx => HYPERVISOR_BACKEND_INTEL_VMX,
                    BackendKind::AmdSvm => HYPERVISOR_BACKEND_AMD_SVM,
                },
                grant_window_start(runtime.domain.nested_pages()),
                GRANT_WINDOW_PAGES as u64 * 4096,
            );
            runtime.resume_kind = ResumeKind::Cpuid(result);
            continue;
        }
        if vm_exit.reason == VmExitReason::ControlRegisterWrite {
            runtime.resume_kind = ResumeKind::ControlRegisterWrite {
                register: (vm_exit.fault_info & 0xf) as u8,
                value: vm_exit.fault_address,
            };
            continue;
        }
        if vm_exit.reason == VmExitReason::NestedPageFault {
            isolate_crashed_domain(
                index,
                &mut runtime_domains,
                &mut runnable,
                &mut grants,
                &mut event_channels,
                &mut devices,
                &mut pci_assignments,
                &mut dma_remapper,
                manifest,
                vm_exit.raw_reason,
                vm_exit.fault_address,
                vm_exit.fault_info,
            );
            continue;
        }
        if vm_exit.reason != VmExitReason::Hypercall {
            let instruction_pointer = unsafe {
                runtime_domains[index]
                    .virtualization
                    .guest_instruction_pointer()
            }
            .unwrap_or(0);
            isolate_crashed_domain(
                index,
                &mut runtime_domains,
                &mut runnable,
                &mut grants,
                &mut event_channels,
                &mut devices,
                &mut pci_assignments,
                &mut dma_remapper,
                manifest,
                vm_exit.raw_reason,
                instruction_pointer,
                0,
            );
            continue;
        }
        runtime.resume_kind = ResumeKind::Hypercall;
        if vm_exit.hypercall_number == HypercallNumber::DeviceQuery as u64 {
            let domain_id = runtime_domains[index].domain.id().get();
            let authorized = runtime_domains[index].domain.role() == DomainRole::Hardware
                && runtime_domains[index].domain.capabilities() & DOMAIN_CAPABILITY_DEVICE_QUERY
                    != 0;
            let device_index = usize::try_from(vm_exit.arg0).ok();
            let info = device_index.and_then(|device_index| devices.query(domain_id, device_index));
            if authorized {
                display::mdriver_query(
                    u16::try_from(vm_exit.arg0).unwrap_or(u16::MAX),
                    info.map(|info| info.requester),
                );
            }
            let destination = runtime_domains[index]
                .domain
                .nested_pages()
                .guest_host_address(vm_exit.arg1, size_of::<PciDeviceInfo>() as u64);
            runtime_domains[index].pending_result =
                if authorized && vm_exit.arg2 >= size_of::<PciDeviceInfo>() as u64 {
                    match (info, destination) {
                        (Some(info), Some(destination)) => {
                            unsafe {
                                copy_nonoverlapping(
                                    &info as *const PciDeviceInfo,
                                    destination as *mut PciDeviceInfo,
                                    1,
                                )
                            };
                            HYPERCALL_SUCCESS
                        }
                        _ => HYPERCALL_INVALID_ARGUMENT,
                    }
                } else {
                    HYPERCALL_INVALID_ARGUMENT
                };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::DeviceClaim as u64 {
            let domain_id = runtime_domains[index].domain.id().get();
            let authorized = runtime_domains[index].domain.role() == DomainRole::Hardware
                && runtime_domains[index].domain.capabilities() & DOMAIN_CAPABILITY_DEVICE_CLAIM
                    != 0;
            let requester = u16::try_from(vm_exit.arg0).ok().filter(|value| *value != 0);
            if authorized {
                if let Some(requester) = requester {
                    display::mdriver_claim(requester);
                }
            }
            let claimed = authorized
                && vm_exit.arg1 == 0
                && vm_exit.arg2 == 0
                && requester.is_some_and(|requester| {
                    claim_pci_device(
                        index,
                        &mut runtime_domains,
                        &mut devices,
                        &mut pci_assignments,
                        &mut dma_remapper,
                        &memory_map,
                        requester,
                    )
                });
            runtime_domains[index].pending_result = if claimed {
                log!(
                    "PCI requester {:04x} mapped for DMA and claimed-disabled by Hardware Domain {}",
                    requester.unwrap_or(0),
                    domain_id
                );
                HYPERCALL_SUCCESS
            } else {
                HYPERCALL_INVALID_ARGUMENT
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::DeviceConfigRead as u64 {
            let domain_id = runtime_domains[index].domain.id().get();
            let requester = u16::try_from(vm_exit.arg0).ok();
            let offset = u16::try_from(vm_exit.arg1).ok();
            runtime_domains[index].pending_result = match (requester, offset) {
                (Some(requester), Some(offset))
                    if runtime_domains[index].domain.role() == DomainRole::Hardware =>
                {
                    unsafe { pci_assignments.config_read(domain_id, requester, offset) }
                        .map_or(HYPERCALL_INVALID_ARGUMENT, u64::from)
                }
                _ => HYPERCALL_INVALID_ARGUMENT,
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::DeviceResourceQuery as u64 {
            let domain_id = runtime_domains[index].domain.id().get();
            let requester = u16::try_from(vm_exit.arg0).ok();
            let resource_index = usize::try_from(vm_exit.arg1).ok();
            let destination = runtime_domains[index]
                .domain
                .nested_pages()
                .guest_host_address(vm_exit.arg2, size_of::<PciDeviceResource>() as u64);
            let resource = requester
                .zip(resource_index)
                .and_then(|(requester, resource_index)| {
                    pci_assignments.resource(domain_id, requester, resource_index)
                });
            runtime_domains[index].pending_result = match (resource, destination) {
                (Some(resource), Some(destination))
                    if runtime_domains[index].domain.role() == DomainRole::Hardware =>
                {
                    unsafe {
                        copy_nonoverlapping(
                            &resource as *const PciDeviceResource,
                            destination as *mut PciDeviceResource,
                            1,
                        )
                    };
                    HYPERCALL_SUCCESS
                }
                _ => HYPERCALL_INVALID_ARGUMENT,
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::DeviceActivate as u64 {
            let domain_id = runtime_domains[index].domain.id().get();
            let requester = u16::try_from(vm_exit.arg0).ok();
            let config_vector = u8::try_from(vm_exit.arg1).ok();
            let queue_vector = u8::try_from(vm_exit.arg2).ok();
            let activated = requester
                .zip(config_vector.zip(queue_vector))
                .is_some_and(|(requester, (config_vector, queue_vector))| {
                    if runtime_domains[index].domain.role() != DomainRole::Hardware
                        || devices.can_activate(domain_id, requester).is_err()
                    {
                        return false;
                    }
                    match unsafe {
                        pci_assignments.activate(
                            domain_id,
                            requester,
                            [config_vector, queue_vector],
                        )
                    } {
                        Ok(activation) => {
                            if devices.activate(domain_id, requester).is_err() {
                                halt_with_error(
                                    "PCI activation state",
                                    mboot::Error::InvalidState,
                                )
                            }
                            match activation.mode {
                                pci::PciInterruptMode::MsixSplit => log!(
                                    "PCI requester {:04x} active: config IRQ {:#x} -> Domain {} vector {:#x}, queue IRQ {:#x} -> vector {:#x}",
                                    requester,
                                    activation.physical_vectors[0],
                                    domain_id,
                                    config_vector,
                                    activation.physical_vectors[1],
                                    queue_vector
                                ),
                                pci::PciInterruptMode::MsixShared => log!(
                                    "PCI requester {:04x} active: shared MSI-X IRQ {:#x} -> Domain {} vector {:#x}",
                                    requester,
                                    activation.physical_vectors[0],
                                    domain_id,
                                    config_vector
                                ),
                                pci::PciInterruptMode::Msi => log!(
                                    "PCI requester {:04x} active: MSI IRQ {:#x} -> Domain {} vector {:#x}",
                                    requester,
                                    activation.physical_vectors[0],
                                    domain_id,
                                    config_vector
                                ),
                            }
                            true
                        }
                        Err(_) => false,
                    }
                });
            runtime_domains[index].pending_result = if activated {
                HYPERCALL_SUCCESS
            } else {
                HYPERCALL_INVALID_ARGUMENT
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::DeviceRelease as u64 {
            let domain_id = runtime_domains[index].domain.id().get();
            let authorized = runtime_domains[index].domain.role() == DomainRole::Hardware
                && runtime_domains[index].domain.capabilities() & DOMAIN_CAPABILITY_DEVICE_CLAIM
                    != 0;
            let requester = u16::try_from(vm_exit.arg0).ok().filter(|value| *value != 0);
            let released = authorized
                && vm_exit.arg1 == 0
                && vm_exit.arg2 == 0
                && requester.is_some_and(|requester| {
                    release_pci_device(
                        index,
                        &mut runtime_domains,
                        &mut devices,
                        &mut pci_assignments,
                        &mut dma_remapper,
                        requester,
                        true,
                    )
                });
            runtime_domains[index].pending_result = if released {
                log!(
                    "PCI requester {:04x} returned to IOMMU deny-all by Hardware Domain {}",
                    requester.unwrap_or(0),
                    domain_id
                );
                HYPERCALL_SUCCESS
            } else {
                HYPERCALL_INVALID_ARGUMENT
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::DomainCrashQuery as u64 {
            let requester_is_system = runtime_domains[index].domain.role() == DomainRole::System;
            let crash_info = u32::try_from(vm_exit.arg0).ok().and_then(|domain_id| {
                runtime_domains
                    .iter()
                    .find(|runtime| runtime.domain.id().get() == domain_id)
                    .and_then(|runtime| runtime.crash_info)
            });
            let destination = runtime_domains[index]
                .domain
                .nested_pages()
                .guest_host_address(vm_exit.arg1, size_of::<DomainCrashInfo>() as u64);
            runtime_domains[index].pending_result =
                if requester_is_system && vm_exit.arg2 >= size_of::<DomainCrashInfo>() as u64 {
                    match (crash_info, destination) {
                        (Some(info), Some(destination)) => {
                            unsafe {
                                copy_nonoverlapping(
                                    &info as *const DomainCrashInfo,
                                    destination as *mut DomainCrashInfo,
                                    1,
                                )
                            };
                            HYPERCALL_SUCCESS
                        }
                        _ => HYPERCALL_INVALID_ARGUMENT,
                    }
                } else {
                    HYPERCALL_INVALID_ARGUMENT
                };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::Shutdown as u64 {
            cleanup_domain_resources(
                index,
                &mut runtime_domains,
                &mut grants,
                &mut event_channels,
                &mut devices,
                &mut pci_assignments,
                &mut dma_remapper,
            );
            let runtime = &mut runtime_domains[index];
            if let Err(error) = runtime.domain.stop() {
                halt_with_error("Domain stop", error);
            }
            runnable[index] = false;
            log!(
                "Domain {} stopped: reason={} yields={} preemptions={} raw={:#x}",
                runtime.domain.id().get(),
                vm_exit.arg0,
                runtime.yield_count,
                runtime.preemption_count,
                vm_exit.raw_reason
            );
            let should_restart = runtime.domain.role() == DomainRole::Application
                && runtime.restart_policy == ManifestRestartPolicy::Always
                && runtime.restart_count < u32::from(runtime.max_restarts);
            if should_restart {
                restart_domain(index, &mut runtime_domains, &mut runnable);
                reconnect_domain_channels(index, &runtime_domains, &mut event_channels, manifest);
            }
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::GrantCreate as u64 {
            let owner = runtime_domains[index].domain.id();
            let target = u32::try_from(vm_exit.arg1).ok().map(DomainId::new);
            let target_is_running = target.is_some_and(|target| {
                runtime_domains.iter().any(|runtime| {
                    runtime.domain.id() == target && runtime.domain.state() == DomainState::Running
                })
            });
            let source_page = vm_exit.arg0;
            let writable = vm_exit.arg2 & GRANT_FLAG_WRITABLE != 0;
            let valid_flags = vm_exit.arg2 & !GRANT_FLAG_WRITABLE == 0;
            let host_page =
                if grant_window_contains(runtime_domains[index].domain.nested_pages(), source_page)
                    && !grants.target_page_is_mapped(owner, source_page)
                {
                    runtime_domains[index]
                        .domain
                        .nested_pages()
                        .owned_page_host_address(source_page)
                } else {
                    None
                };
            runtime_domains[index].pending_result = match (target, host_page) {
                (Some(target), Some(host_page)) if target_is_running && valid_flags => grants
                    .create(owner, target, host_page, writable)
                    .map_or(HYPERCALL_INVALID_ARGUMENT, |reference| {
                        log!(
                            "Grant {} created: {}:{:#x} -> {} writable={}",
                            reference.get(),
                            owner.get(),
                            source_page,
                            target.get(),
                            writable
                        );
                        u64::from(reference.get())
                    }),
                _ => HYPERCALL_INVALID_ARGUMENT,
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::GrantQuery as u64 {
            let target = runtime_domains[index].domain.id();
            runtime_domains[index].pending_result = if vm_exit.arg1 == 0 && vm_exit.arg2 == 0 {
                usize::try_from(vm_exit.arg0)
                    .ok()
                    .and_then(|ordinal| grants.query_target(target, ordinal))
                    .map_or(HYPERCALL_INVALID_ARGUMENT, |reference| {
                        u64::from(reference.get())
                    })
            } else {
                HYPERCALL_INVALID_ARGUMENT
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::GrantMap as u64 {
            let target = runtime_domains[index].domain.id();
            let reference = u32::try_from(vm_exit.arg0).ok().and_then(GrantRef::new);
            let target_page = vm_exit.arg1;
            let mapping = if vm_exit.arg2 == 0
                && grant_window_contains(runtime_domains[index].domain.nested_pages(), target_page)
            {
                reference.and_then(|reference| grants.map(target, reference, target_page).ok())
            } else {
                None
            };
            if let (Some(reference), Some(mapping)) = (reference, mapping) {
                let runtime = &mut runtime_domains[index];
                let nested_root = runtime.domain.nested_pages().hardware_root();
                if unsafe {
                    runtime.domain.nested_pages().map_shared_page(
                        mapping.target_page,
                        mapping.host_page,
                        mapping.writable,
                    )
                }
                .is_err()
                {
                    let _ = grants.unmap(target, reference);
                    runtime.pending_result = HYPERCALL_INVALID_ARGUMENT;
                    continue;
                }
                if let Err(error) = unsafe { runtime.virtualization.flush_nested(nested_root) } {
                    halt_with_error("Grant translation flush", error)
                }
                log!(
                    "Grant {} mapped: Domain {} GPA {:#x}",
                    reference.get(),
                    target.get(),
                    target_page
                );
                runtime.pending_result = HYPERCALL_SUCCESS;
            } else {
                runtime_domains[index].pending_result = HYPERCALL_INVALID_ARGUMENT;
            }
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::GrantUnmap as u64 {
            let target = runtime_domains[index].domain.id();
            let reference = u32::try_from(vm_exit.arg0).ok().and_then(GrantRef::new);
            let mapping = if vm_exit.arg1 == 0 && vm_exit.arg2 == 0 {
                reference.and_then(|reference| grants.unmap(target, reference).ok())
            } else {
                None
            };
            if let (Some(reference), Some(mapping)) = (reference, mapping) {
                let runtime = &mut runtime_domains[index];
                let nested_root = runtime.domain.nested_pages().hardware_root();
                if let Err(error) = unsafe {
                    runtime
                        .domain
                        .nested_pages()
                        .restore_owned_page(mapping.target_page)
                } {
                    halt_with_error("Grant unmap", error)
                }
                if let Err(error) = unsafe { runtime.virtualization.flush_nested(nested_root) } {
                    halt_with_error("Grant translation flush", error)
                }
                log!(
                    "Grant {} unmapped from Domain {}",
                    reference.get(),
                    target.get()
                );
                runtime.pending_result = HYPERCALL_SUCCESS;
            } else {
                runtime_domains[index].pending_result = HYPERCALL_INVALID_ARGUMENT;
            }
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::GrantRevoke as u64 {
            let owner = runtime_domains[index].domain.id();
            let reference = u32::try_from(vm_exit.arg0).ok().and_then(GrantRef::new);
            runtime_domains[index].pending_result = if vm_exit.arg1 == 0
                && vm_exit.arg2 == 0
                && reference.is_some_and(|reference| grants.revoke(owner, reference).is_ok())
            {
                log!("Grant revoked by Domain {}", owner.get());
                HYPERCALL_SUCCESS
            } else {
                HYPERCALL_INVALID_ARGUMENT
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::EventSend as u64 {
            let sender = runtime_domains[index].domain.id();
            let mut delivery = if vm_exit.arg1 == 0 && vm_exit.arg2 == 0 {
                u32::try_from(vm_exit.arg0)
                    .ok()
                    .and_then(|port| event_channels.send(sender, port).ok())
            } else {
                None
            };
            let target_index = delivery.and_then(|event| {
                runtime_domains
                    .iter()
                    .position(|domain| domain.domain.id() == event.domain)
            });
            if target_index
                .is_none_or(|target| runtime_domains[target].domain.state() != DomainState::Running)
            {
                if let Some(event) = delivery {
                    let _ = event_channels.receive(event.domain);
                }
                delivery = None;
            }
            runtime_domains[index].pending_result = if delivery.is_some() {
                HYPERCALL_SUCCESS
            } else {
                HYPERCALL_INVALID_ARGUMENT
            };
            if let Some(delivery) = delivery {
                if delivery.first_delivery {
                    log!(
                        "Event Channel {}:{} -> {}:{}",
                        sender.get(),
                        vm_exit.arg0,
                        delivery.domain.get(),
                        delivery.port
                    );
                }
                if let Some(target_index) = target_index {
                    if runtime_domains[target_index].waiting {
                        let port = event_channels
                            .receive(delivery.domain)
                            .unwrap_or(delivery.port);
                        runtime_domains[target_index].pending_result = u64::from(port);
                        runtime_domains[target_index].waiting = false;
                        runnable[target_index] = true;
                    } else if runtime_domains[target_index].event_irq_enabled {
                        if runtime_domains[target_index]
                            .interrupts
                            .raise(EVENT_CHANNEL_VECTOR)
                            .is_err()
                        {
                            halt_with_error("Virtual APIC raise", mboot::Error::InvalidState)
                        }
                    }
                }
            }
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::EventIrqEnable as u64 {
            let receiver = runtime_domains[index].domain.id();
            if vm_exit.arg0 != 0
                || vm_exit.arg1 != 0
                || vm_exit.arg2 != 0
                || runtime_domains[index].event_irq_enabled
            {
                runtime_domains[index].pending_result = HYPERCALL_INVALID_ARGUMENT;
            } else {
                runtime_domains[index].event_irq_enabled = true;
                if event_channels.has_pending(receiver)
                    && runtime_domains[index]
                        .interrupts
                        .raise(EVENT_CHANNEL_VECTOR)
                        .is_err()
                {
                    halt_with_error("Virtual APIC raise", mboot::Error::InvalidState)
                }
                runtime_domains[index].pending_result = HYPERCALL_SUCCESS;
                log!(
                    "Domain {} enabled Event Channel IRQ vector {:#x}",
                    receiver.get(),
                    EVENT_CHANNEL_VECTOR
                );
            }
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::IrqEoi as u64 {
            let receiver = runtime_domains[index].domain.id();
            let valid = vm_exit.arg0 == 0
                && vm_exit.arg1 == 0
                && vm_exit.arg2 == 0
                && (runtime_domains[index].event_irq_enabled
                    || pci_assignments.has_active_for_domain(receiver.get()));
            let completed = if valid {
                runtime_domains[index].interrupts.eoi().ok()
            } else {
                None
            };
            runtime_domains[index].pending_result = if let Some(vector) = completed {
                if vector == EVENT_CHANNEL_VECTOR && event_channels.has_pending(receiver) {
                    if runtime_domains[index].interrupts.raise(vector).is_err() {
                        halt_with_error("Virtual APIC raise", mboot::Error::InvalidState)
                    }
                }
                HYPERCALL_SUCCESS
            } else {
                HYPERCALL_INVALID_ARGUMENT
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::IrqMask as u64 {
            let vector = u8::try_from(vm_exit.arg0).ok();
            let masked = match vm_exit.arg1 {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            };
            runtime_domains[index].pending_result = match (vector, masked) {
                (Some(vector), Some(masked))
                    if vm_exit.arg2 == 0
                        && runtime_domains[index]
                            .interrupts
                            .set_masked(vector, masked)
                            .is_ok() =>
                {
                    HYPERCALL_SUCCESS
                }
                _ => HYPERCALL_INVALID_ARGUMENT,
            };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::IrqSetTpr as u64 {
            if vm_exit.arg0 <= u64::from(u8::MAX) && vm_exit.arg1 == 0 && vm_exit.arg2 == 0 {
                runtime_domains[index]
                    .interrupts
                    .set_task_priority(vm_exit.arg0 as u8);
                runtime_domains[index].pending_result = HYPERCALL_SUCCESS;
            } else {
                runtime_domains[index].pending_result = HYPERCALL_INVALID_ARGUMENT;
            }
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::EventPoll as u64 {
            let receiver = runtime_domains[index].domain.id();
            runtime_domains[index].pending_result =
                if vm_exit.arg0 == 0 && vm_exit.arg1 == 0 && vm_exit.arg2 == 0 {
                    event_channels
                        .receive(receiver)
                        .map_or(EVENT_CHANNEL_NO_EVENT, |port| u64::from(port))
                } else {
                    HYPERCALL_INVALID_ARGUMENT
                };
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::EventWait as u64 {
            if vm_exit.arg0 != 0 || vm_exit.arg1 != 0 || vm_exit.arg2 != 0 {
                runtime_domains[index].pending_result = HYPERCALL_INVALID_ARGUMENT;
                continue;
            }
            let receiver = runtime_domains[index].domain.id();
            if let Some(port) = event_channels.receive(receiver) {
                let _ = runtime_domains[index]
                    .interrupts
                    .cancel_pending(EVENT_CHANNEL_VECTOR);
                runtime_domains[index].pending_result = u64::from(port);
            } else {
                runtime_domains[index].waiting = true;
                runnable[index] = false;
            }
            continue;
        }
        runtime.pending_result = match vm_exit.hypercall_number {
            number if number == HypercallNumber::ConsoleWrite as u64 => handle_console_write(
                runtime.domain.id(),
                matches!(
                    runtime.domain.role(),
                    DomainRole::System | DomainRole::Hardware
                ),
                runtime.domain.nested_pages(),
                vm_exit.arg0,
                vm_exit.arg1,
            ),
            number if number == HypercallNumber::Yield as u64 => {
                runtime.yield_count += 1;
                HYPERCALL_SUCCESS
            }
            number if number == HypercallNumber::Ready as u64 => {
                if !matches!(
                    runtime.domain.role(),
                    DomainRole::System | DomainRole::Hardware
                ) || runtime.ready
                    || vm_exit.arg0 != 0
                    || vm_exit.arg1 != 0
                    || vm_exit.arg2 != 0
                {
                    HYPERCALL_INVALID_ARGUMENT
                } else {
                    runtime.ready = true;
                    if runtime.domain.role() == DomainRole::System {
                        log!("mochiOS System Domain {} ready", runtime.domain.id().get());
                        display::mochios_ready();
                        if firmware_framebuffer_fallback {
                            display::handoff();
                            log!("firmware framebuffer ownership transferred to mochiOS");
                        }
                    } else {
                        log!("Hardware Domain {} ready", runtime.domain.id().get());
                        display::hardware_ready();
                        if deferred_display.is_some() {
                            // Keep the firmware framebuffer available while
                            // mDriver enumerates and probes the transferred GPU.
                            // Its Ready notification is the first point where a
                            // replacement display backend is known to exist.
                            display::handoff();
                            log!("boot display diagnostics handed off to the Hardware Domain");
                        }
                    }
                    HYPERCALL_SUCCESS
                }
            }
            number if number == HypercallNumber::Wait as u64 => {
                if runtime.domain.role() == DomainRole::System && !runtime.ready {
                    HYPERCALL_INVALID_ARGUMENT
                } else {
                    runtime.waiting = true;
                    runnable[index] = false;
                    HYPERCALL_SUCCESS
                }
            }
            _ => HYPERCALL_UNSUPPORTED,
        };
    }
    let waiting_domains = runtime_domains
        .iter()
        .filter(|domain| domain.waiting)
        .count();
    let crashed_domains = runtime_domains
        .iter()
        .filter(|domain| domain.domain.state() == DomainState::Crashed)
        .count();
    let recovered_crashes = runtime_domains
        .iter()
        .filter(|domain| {
            domain.crash_info.is_some_and(|info| {
                info.status == DOMAIN_CRASH_STATUS_RESTARTED
                    && domain.domain.state() != DomainState::Crashed
            })
        })
        .count();
    let stopped_domains = runtime_domains
        .iter()
        .filter(|domain| domain.domain.state() == DomainState::Stopped)
        .count();
    if waiting_domains == 0 && crashed_domains == 0 && recovered_crashes == 0 {
        log!(
            "bootstrap complete; {} Domains entered and stopped cleanly",
            stopped_domains
        );
        display::bootstrap_success();
    } else if waiting_domains == 0 && crashed_domains == 0 {
        log!(
            "bootstrap complete; {} Domain(s) stopped cleanly; {} crash(es) recovered",
            stopped_domains,
            recovered_crashes
        );
        display::isolation_success();
    } else if waiting_domains == 0 {
        log!(
            "bootstrap complete; {} Domain(s) stopped cleanly; {} crash(es) isolated",
            stopped_domains,
            crashed_domains
        );
        // `isolate_crashed_domain` already showed the failure, but another
        // Domain may have updated the framebuffer before the scheduler became
        // idle.  A crashed required Domain is not a successful isolation boot:
        // keep its diagnostic visible instead of replacing it with a green
        // `ISOLATION OK` screen.
        if let Some(crash) = runtime_domains.iter().find_map(|runtime| {
            (runtime.domain.state() == DomainState::Crashed)
                .then_some(runtime.crash_info)
                .flatten()
        }) {
            display::domain_crash(
                crash.domain_id,
                crash.raw_reason,
                crash.fault_address,
                crash.fault_info,
            );
        } else {
            display::failure(53);
        }
    } else {
        log!(
            "{} resident Domain(s) waiting; {} crash(es) isolated",
            waiting_domains,
            crashed_domains
        );
    }

    let _keep_domains_alive = runtime_domains;
    let _keep_prepared_storage = prepared_domains;
    let _keep_manifest_alive = manifest_bytes;
    halt()
}

fn pci_identity(function: pci::PciFunction) -> (u16, u16, u16, u8) {
    (
        function.requester,
        function.vendor,
        function.device,
        function.subclass,
    )
}

fn isolate_crashed_domain(
    index: usize,
    runtime_domains: &mut [RuntimeDomain],
    runnable: &mut [bool],
    grants: &mut GrantTable,
    event_channels: &mut EventChannelTable,
    devices: &mut DeviceTable,
    pci_assignments: &mut pci::AssignmentTable,
    dma_remapper: &mut Option<iommu::DmaRemapper>,
    manifest: LaunchManifest<'_>,
    raw_reason: u64,
    fault_address: u64,
    fault_info: u64,
) {
    let domain_id = runtime_domains[index].domain.id();
    if let Err(error) = runtime_domains[index].domain.mark_crashed() {
        halt_with_error("Domain crash transition", error)
    }
    runtime_domains[index].waiting = false;
    runnable[index] = false;
    let next_restart_count = runtime_domains[index].restart_count.saturating_add(1);
    runtime_domains[index].crash_info = Some(DomainCrashInfo::new(
        domain_id.get(),
        raw_reason,
        fault_address,
        fault_info,
        next_restart_count,
        DOMAIN_CRASH_STATUS_CRASHED,
    ));
    display::domain_crash(domain_id.get(), raw_reason, fault_address, fault_info);
    cleanup_domain_resources(
        index,
        runtime_domains,
        grants,
        event_channels,
        devices,
        pci_assignments,
        dma_remapper,
    );
    log!(
        "Domain {} crashed and was isolated: exit={:#x} address={:#x} info={:#x}",
        domain_id.get(),
        raw_reason,
        fault_address,
        fault_info
    );
    let should_restart = runtime_domains[index].domain.role() == DomainRole::Application
        && matches!(
            runtime_domains[index].restart_policy,
            ManifestRestartPolicy::OnFailure | ManifestRestartPolicy::Always
        )
        && runtime_domains[index].restart_count < u32::from(runtime_domains[index].max_restarts);
    if should_restart {
        restart_domain(index, runtime_domains, runnable);
        reconnect_domain_channels(index, runtime_domains, event_channels, manifest);
    }
    notify_system_domain(runtime_domains);
}

fn cleanup_domain_resources(
    index: usize,
    runtime_domains: &mut [RuntimeDomain],
    grants: &mut GrantTable,
    event_channels: &mut EventChannelTable,
    devices: &mut DeviceTable,
    pci_assignments: &mut pci::AssignmentTable,
    dma_remapper: &mut Option<iommu::DmaRemapper>,
) {
    let domain_id = runtime_domains[index].domain.id();
    let crashed = runtime_domains[index].domain.state() == DomainState::Crashed;
    let mut released_devices = 0;
    loop {
        let requester = {
            let mut claimed = devices.claimed_requesters(domain_id.get());
            claimed.next()
        };
        let Some(requester) = requester else {
            break;
        };
        if !release_pci_device(
            index,
            runtime_domains,
            devices,
            pci_assignments,
            dma_remapper,
            requester,
            true,
        ) {
            halt_with_error("PCI device cleanup", mboot::Error::InvalidState)
        }
        released_devices += 1;
    }
    if released_devices != 0 {
        log!(
            "Domain {} returned {} PCI device(s)",
            domain_id.get(),
            released_devices
        );
    }
    let mappings = if crashed {
        grants.cleanup_crashed_domain(domain_id)
    } else {
        grants.cleanup_domain(domain_id)
    };
    for mapping in mappings.into_iter().flatten() {
        let Some(target_index) = runtime_domains
            .iter()
            .position(|runtime| runtime.domain.id() == mapping.target)
        else {
            halt_with_error("Grant cleanup", mboot::Error::InvalidState)
        };
        let target = &mut runtime_domains[target_index];
        let nested_root = target.domain.nested_pages().hardware_root();
        if let Err(error) = unsafe {
            target
                .domain
                .nested_pages()
                .restore_owned_page(mapping.target_page)
        } {
            halt_with_error("Grant cleanup", error)
        }
        if let Err(error) = unsafe { target.virtualization.flush_nested(nested_root) } {
            halt_with_error("Grant translation flush", error)
        }
        log!(
            "Grant mapping at Domain {} GPA {:#x} cleaned up",
            mapping.target.get(),
            mapping.target_page
        );
    }
    let disconnected = event_channels.disconnect_domain(domain_id);
    if disconnected != 0 {
        log!(
            "Domain {} released {} Event Channel(s)",
            domain_id.get(),
            disconnected
        );
    }
}

fn restart_domain(index: usize, runtime_domains: &mut [RuntimeDomain], runnable: &mut [bool]) {
    let runtime = &mut runtime_domains[index];
    if let Err(error) = unsafe { runtime.virtualization.reset_vcpu() } {
        halt_with_error("vCPU reset", error)
    }
    unsafe { runtime.domain.nested_pages().clear_guest_memory() };
    let page_table_root = if runtime.image_format == ManifestImageFormat::NativeElf {
        match unsafe { runtime.domain.nested_pages().initialize_guest_page_tables() } {
            Ok(root) => root,
            Err(error) => halt_with_error("guest page table restart", error),
        }
    } else {
        0
    };
    let guest_image = match unsafe {
        match runtime.image_format {
            ManifestImageFormat::NativeElf => {
                image::load_elf(&runtime.image, runtime.domain.nested_pages())
            }
            ManifestImageFormat::LinuxPvh => image::load_linux_pvh(
                &runtime.image,
                runtime.initramfs.as_deref(),
                &runtime.command_line,
                0,
                device_window_start(runtime.domain.nested_pages()),
                runtime.domain.nested_pages(),
            ),
        }
    } {
        Ok(image) => image,
        Err(error) => halt_with_error("Domain image restart", error),
    };
    let (boot_module_start, boot_module_size) =
        if runtime.image_format == ManifestImageFormat::NativeElf {
            match runtime.initramfs.as_deref() {
                Some(module) => match unsafe {
                    image::load_boot_module(
                        module,
                        guest_image.loaded_end(),
                        domain_stack_bottom(runtime.domain.nested_pages()),
                        runtime.domain.nested_pages(),
                    )
                } {
                    Ok(module) => module,
                    Err(error) => halt_with_error("Domain boot module restart", error),
                },
                None => (0, 0),
            }
        } else {
            (0, 0)
        };
    runtime.restart_count = runtime.restart_count.saturating_add(1);
    let backend = match runtime.domain.backend() {
        BackendKind::IntelVmx => HYPERVISOR_BACKEND_INTEL_VMX,
        BackendKind::AmdSvm => HYPERVISOR_BACKEND_AMD_SVM,
    };
    let boot_info = DomainBootInfo::new(
        runtime.domain.id().get(),
        0,
        backend,
        abi_domain_role(runtime.domain.role()),
        runtime.domain.nested_pages().guest_memory_size(),
        boot_module_start,
        boot_module_size,
        grant_window_start(runtime.domain.nested_pages()),
        GRANT_WINDOW_PAGES as u64 * 4096,
        DEVICE_WINDOW_START,
        DEVICE_WINDOW_LIMIT - DEVICE_WINDOW_START,
        runtime.restart_count,
        runtime.domain.capabilities(),
    )
    .with_entropy_seed(derive_domain_entropy(
        &runtime.entropy_root,
        runtime.domain.id().get(),
        runtime.restart_count,
    ));
    let Some(boot_info_host) = runtime
        .domain
        .nested_pages()
        .guest_host_address(DOMAIN_BOOT_INFO_GPA, size_of::<DomainBootInfo>() as u64)
    else {
        halt_with_error("Domain restart boot info", mboot::Error::InvalidPage)
    };
    unsafe {
        copy_nonoverlapping(
            &boot_info as *const DomainBootInfo,
            boot_info_host as *mut DomainBootInfo,
            1,
        )
    };
    if let Err(error) = runtime.domain.prepare_restart() {
        halt_with_error("Domain restart transition", error)
    }
    if let Err(error) = runtime.domain.start() {
        halt_with_error("Domain restart", error)
    }
    runtime.guest.page_table_root = page_table_root;
    runtime.guest.entry = guest_image.entry();
    runtime.guest.stack = domain_stack_pointer(runtime.domain.nested_pages());
    runtime.guest.boot_info = if runtime.image_format == ManifestImageFormat::NativeElf {
        DOMAIN_BOOT_INFO_GPA
    } else {
        guest_image.boot_info()
    };
    runtime.started = false;
    runtime.pending_result = HYPERCALL_SUCCESS;
    runtime.yield_count = 0;
    runtime.preemption_count = 0;
    runtime.ready = false;
    runtime.waiting = false;
    runtime.resume_kind = ResumeKind::Hypercall;
    runtime.event_irq_enabled = false;
    runtime.interrupts = if runtime.image_format == ManifestImageFormat::LinuxPvh {
        VirtualLocalApic::new_x2apic()
    } else {
        VirtualLocalApic::new()
    };
    if let Some(info) = &mut runtime.crash_info {
        info.restart_count = runtime.restart_count;
        info.status = DOMAIN_CRASH_STATUS_RESTARTED;
    }
    runnable[index] = true;
    log!(
        "Domain {} restarted: attempt={}",
        runtime.domain.id().get(),
        runtime.restart_count
    );
}

fn reconnect_domain_channels(
    index: usize,
    runtime_domains: &[RuntimeDomain],
    event_channels: &mut EventChannelTable,
    manifest: LaunchManifest<'_>,
) {
    let domain_id = runtime_domains[index].domain.id().get();
    for channel_index in 0..manifest.event_channel_count() {
        let channel = match manifest.event_channel(channel_index) {
            Ok(channel) => channel,
            Err(error) => halt_with_error("Event Channel restart manifest", error),
        };
        if channel.domain_a != domain_id && channel.domain_b != domain_id {
            continue;
        }
        let peer = if channel.domain_a == domain_id {
            channel.domain_b
        } else {
            channel.domain_a
        };
        if !runtime_domains.iter().any(|runtime| {
            runtime.domain.id().get() == peer && runtime.domain.state() == DomainState::Running
        }) {
            continue;
        }
        if event_channels
            .connect(
                DomainId::new(channel.domain_a),
                channel.port_a,
                DomainId::new(channel.domain_b),
                channel.port_b,
            )
            .is_err()
        {
            halt_with_error("Event Channel reconnect", mboot::Error::InvalidState)
        }
        log!(
            "Event Channel reconnected: {}:{} <-> {}:{}",
            channel.domain_a,
            channel.port_a,
            channel.domain_b,
            channel.port_b
        );
    }
}

fn notify_system_domain(runtime_domains: &mut [RuntimeDomain]) {
    let Some(system) = runtime_domains.iter_mut().find(|runtime| {
        runtime.domain.role() == DomainRole::System
            && runtime.domain.state() == DomainState::Running
    }) else {
        return;
    };
    if system.interrupts.raise(DOMAIN_MANAGEMENT_VECTOR).is_err() {
        halt_with_error("Domain management notification", mboot::Error::InvalidState)
    }
}

fn handle_console_write(
    domain_id: DomainId,
    allow_display: bool,
    memory: &NestedPageTable,
    address: u64,
    len: u64,
) -> u64 {
    if len > MAX_CONSOLE_WRITE {
        return HYPERCALL_INVALID_ARGUMENT;
    }
    let Some(host_address) = memory.guest_host_address(address, len) else {
        return HYPERCALL_INVALID_ARGUMENT;
    };
    // SAFETY: `guest_host_address` checked the complete immutable guest range and
    // the vCPU is stopped for the duration of this read.
    let bytes = unsafe { core::slice::from_raw_parts(host_address as *const u8, len as usize) };
    let Ok(message) = core::str::from_utf8(bytes) else {
        return HYPERCALL_INVALID_ARGUMENT;
    };
    crate::serial::print(format_args!("[Domain {}] {}", domain_id.get(), message));
    if allow_display {
        if let Some(report) = bytes.strip_prefix(b"DISPLAY\n") {
            let _ = crate::display::console_page(report);
        }
    }
    HYPERCALL_SUCCESS
}

fn claim_pci_device(
    index: usize,
    runtime_domains: &mut [RuntimeDomain],
    devices: &mut DeviceTable,
    assignments: &mut pci::AssignmentTable,
    dma_remapper: &mut Option<iommu::DmaRemapper>,
    memory_map: &BootMemoryMap,
    requester: u16,
) -> bool {
    let domain_id = runtime_domains[index].domain.id().get();
    if devices.can_claim(domain_id, requester).is_err() {
        display::mdriver_claim_failure(requester, b"POLICY ERROR");
        return false;
    }
    let descriptor = match unsafe { pci::probe_descriptor(requester) } {
        Ok(descriptor) => descriptor,
        Err(error) => {
            log!(
                "PCI requester {:04x} resource probe failed: {:?}",
                requester,
                error
            );
            let stage = match error {
                pci::PciError::InvalidRequester => b"BDF ERROR" as &[u8],
                pci::PciError::InvalidState => b"STATE ERROR",
                pci::PciError::UnsupportedHeader => b"HEADER ERROR",
                pci::PciError::UnsupportedBar => b"BAR TYPE ERROR",
                pci::PciError::InvalidBar => b"BAR VALUE ERROR",
                pci::PciError::DeviceWindowExhausted => b"WINDOW ERROR",
                pci::PciError::InterruptUnavailable => b"IRQ ERROR",
                pci::PciError::RegisterWriteFailed => b"PCI WRITE ERROR",
            };
            display::mdriver_claim_failure(requester, stage);
            return false;
        }
    };
    if descriptor.bars[..descriptor.bar_count]
        .iter()
        .any(|bar| !memory_map.allows_device_mmio(bar.physical_address, bar.length))
    {
        log!("PCI requester {:04x} exposed an unsafe MMIO BAR", requester);
        display::mdriver_claim_failure(requester, b"MMIO ERROR");
        return false;
    }
    let window_start = DEVICE_WINDOW_START;
    let bars = match assignments.insert(
        domain_id,
        descriptor,
        window_start,
        DEVICE_WINDOW_LIMIT - DEVICE_WINDOW_START,
    ) {
        Ok(bars) => {
            let mut copied = [pci::PciBar::default(); pci::MAX_DEVICE_BARS];
            copied[..bars.len()].copy_from_slice(bars);
            copied
        }
        Err(error) => {
            log!(
                "PCI requester {:04x} device window allocation failed: {:?}",
                requester,
                error
            );
            for bar in descriptor.bars[..descriptor.bar_count]
                .iter()
                .filter(|bar| bar.length != 0)
            {
                log!(
                    "PCI requester {:04x} BAR{} range={:#x}..{:#x}",
                    requester,
                    bar.index,
                    bar.physical_address,
                    bar.physical_address.saturating_add(bar.length)
                );
            }
            display::mdriver_claim_failure(requester, b"WINDOW ERROR");
            return false;
        }
    };
    let nested_root = runtime_domains[index].domain.nested_pages().hardware_root();
    let mut mapped_bars = [pci::PciBar::default(); pci::MAX_DEVICE_BARS];
    let mut mapped_count = 0;
    for bar in bars.iter().filter(|bar| bar.length != 0) {
        if unsafe {
            runtime_domains[index]
                .domain
                .nested_pages()
                .map_device_range(bar.guest_address, bar.physical_address, bar.length)
        }
        .is_err()
        {
            if !rollback_pci_mapping(
                index,
                runtime_domains,
                assignments,
                requester,
                mapped_bars,
            ) {
                halt_with_error("PCI mapping rollback", mboot::Error::InvalidState)
            }
            display::mdriver_claim_failure(requester, b"EPT ERROR");
            return false;
        }
        mapped_bars[mapped_count] = *bar;
        mapped_count += 1;
    }
    if unsafe {
        runtime_domains[index]
            .virtualization
            .flush_nested(nested_root)
    }
    .is_err()
    {
        if !rollback_pci_mapping(index, runtime_domains, assignments, requester, bars) {
            halt_with_error("PCI mapping rollback", mboot::Error::InvalidState)
        }
        display::mdriver_claim_failure(requester, b"EPT FLUSH ERROR");
        return false;
    }
    let Some(remapper) = dma_remapper.as_mut() else {
        if !rollback_pci_mapping(index, runtime_domains, assignments, requester, bars) {
            halt_with_error("PCI mapping rollback", mboot::Error::InvalidState)
        }
        display::mdriver_claim_failure(requester, b"IOMMU ERROR");
        return false;
    };
    if devices.is_firmware_deferred(requester) {
        display::gpu_dma_transition(requester, b"GPU IOMMU");
        if unsafe {
            remapper.take_over_deferred_display(0, requester, |stage| {
                let label: &[u8] = match stage {
                    IntelTransitionStage::Tables => b"IOMMU TABLES",
                    IntelTransitionStage::Disable => b"IOMMU DISABLE",
                    IntelTransitionStage::WriteBuffer => b"IOMMU WRITE BUFFER",
                    IntelTransitionStage::Root => b"IOMMU ROOT",
                    IntelTransitionStage::Context => b"IOMMU CONTEXT",
                    IntelTransitionStage::Iotlb => b"IOMMU IOTLB",
                    IntelTransitionStage::Enable => b"IOMMU ENABLE",
                    IntelTransitionStage::ProtectedMemory => b"IOMMU PROTECTED",
                };
                display::gpu_dma_transition(requester, label);
            })
        }
        .is_err()
        {
            if !rollback_pci_mapping(index, runtime_domains, assignments, requester, bars) {
                halt_with_error("PCI mapping rollback", mboot::Error::InvalidState)
            }
            display::mdriver_claim_failure(requester, b"IOMMU HANDOFF ERROR");
            return false;
        }
        display::gpu_dma_transition(requester, b"IOMMU DMA MAP");
    }
    let guest_base = runtime_domains[index].domain.nested_pages().guest_base();
    let guest_size = runtime_domains[index]
        .domain
        .nested_pages()
        .guest_memory_size();
    if unsafe { remapper.assign(0, requester, domain_id, guest_base, guest_size) }.is_err() {
        if !rollback_pci_mapping(index, runtime_domains, assignments, requester, bars) {
            halt_with_error("PCI mapping rollback", mboot::Error::InvalidState)
        }
        display::mdriver_claim_failure(requester, b"DMA MAP ERROR");
        return false;
    }
    let firmware_display = devices.is_firmware_deferred(requester);
    if devices.claim(domain_id, requester).is_err() {
        if unsafe { remapper.detach(0, requester, domain_id) }.is_err() {
            halt_with_error("PCI DMA rollback", mboot::Error::InvalidState)
        }
        if !rollback_pci_mapping(index, runtime_domains, assignments, requester, bars) {
            halt_with_error("PCI mapping rollback", mboot::Error::InvalidState)
        }
        display::mdriver_claim_failure(requester, b"STATE ERROR");
        return false;
    }
    if firmware_display {
        log!(
            "firmware display {:04x} DMA ownership transferred to the Hardware Domain",
            requester
        );
    }
    true
}

fn release_pci_device(
    index: usize,
    runtime_domains: &mut [RuntimeDomain],
    devices: &mut DeviceTable,
    assignments: &mut pci::AssignmentTable,
    dma_remapper: &mut Option<iommu::DmaRemapper>,
    requester: u16,
    reset: bool,
) -> bool {
    let domain_id = runtime_domains[index].domain.id().get();
    if devices.can_release(domain_id, requester).is_err() {
        return false;
    }
    let bars = match unsafe { assignments.deactivate(domain_id, requester, reset) } {
        Ok(bars) => bars,
        Err(error) => {
            log!(
                "PCI requester {:04x} shutdown failed: {:?}",
                requester,
                error
            );
            return false;
        }
    };
    let Some(remapper) = dma_remapper.as_mut() else {
        return false;
    };
    if unsafe { remapper.detach(0, requester, domain_id) }.is_err() {
        return false;
    }
    if !restore_pci_mapping(index, runtime_domains, bars) {
        halt_with_error("PCI mapping restore", mboot::Error::InvalidState)
    }
    if assignments.remove(domain_id, requester).is_err()
        || devices.release(domain_id, requester).is_err()
    {
        halt_with_error("PCI release state", mboot::Error::InvalidState)
    }
    true
}

fn rollback_pci_mapping(
    index: usize,
    runtime_domains: &mut [RuntimeDomain],
    assignments: &mut pci::AssignmentTable,
    requester: u16,
    bars: [pci::PciBar; pci::MAX_DEVICE_BARS],
) -> bool {
    let domain_id = runtime_domains[index].domain.id().get();
    let deactivated = unsafe { assignments.deactivate(domain_id, requester, false) }.is_ok();
    let restored = restore_pci_mapping(index, runtime_domains, bars);
    let removed = assignments.remove(domain_id, requester).is_ok();
    deactivated && restored && removed
}

fn restore_pci_mapping(
    index: usize,
    runtime_domains: &mut [RuntimeDomain],
    bars: [pci::PciBar; pci::MAX_DEVICE_BARS],
) -> bool {
    let nested_root = runtime_domains[index].domain.nested_pages().hardware_root();
    for bar in bars.iter().filter(|bar| bar.length != 0) {
        if unsafe {
            runtime_domains[index]
                .domain
                .nested_pages()
                .unmap_device_range(bar.guest_address, bar.length)
        }
        .is_err()
        {
            return false;
        }
    }
    unsafe {
        runtime_domains[index]
            .virtualization
            .flush_nested(nested_root)
    }
    .is_ok()
}

fn abi_domain_role(role: DomainRole) -> u32 {
    match role {
        DomainRole::System => DOMAIN_ROLE_SYSTEM,
        DomainRole::Hardware => DOMAIN_ROLE_HARDWARE,
        DomainRole::Application => DOMAIN_ROLE_APPLICATION,
    }
}

fn grant_window_start(memory: &NestedPageTable) -> u64 {
    memory.guest_memory_size() - GRANT_WINDOW_PAGES as u64 * 4096
}

fn device_window_start(memory: &NestedPageTable) -> u64 {
    grant_window_start(memory) - DEVICE_WINDOW_PAGES as u64 * 4096
}

const fn align_up_4k(value: u64) -> u64 {
    value.saturating_add(0xfff) & !0xfff
}

fn domain_stack_bottom(memory: &NestedPageTable) -> u64 {
    device_window_start(memory) - DOMAIN_STACK_BYTES
}

fn domain_stack_pointer(memory: &NestedPageTable) -> u64 {
    device_window_start(memory) - 16
}

fn grant_window_contains(memory: &NestedPageTable, guest_page: u64) -> bool {
    guest_page & 0xfff == 0
        && guest_page >= grant_window_start(memory)
        && guest_page
            .checked_add(4096)
            .is_some_and(|end| end <= memory.guest_memory_size())
}

fn allocate_iommu_resources(
    boot_services: &BootServices,
    topology: iommu::IommuTopology,
) -> Result<Vec<iommu::IommuResources>, Status> {
    let (remapping_pages, domain_pages, command_pages, completion_pages) =
        iommu::IommuResources::required_pages(topology.kind());
    let mut resources = Vec::with_capacity(topology.unit_count());
    for _ in topology.units() {
        let remapping_table = allocate_zeroed_pages(boot_services, remapping_pages)?;
        let domain_tables = allocate_optional_zeroed_pages(boot_services, domain_pages)?;
        let command_buffer = allocate_optional_zeroed_pages(boot_services, command_pages)?;
        let completion = allocate_optional_zeroed_pages(boot_services, completion_pages)?;
        resources.push(iommu::IommuResources {
            remapping_table,
            domain_tables,
            command_buffer,
            completion,
        });
    }
    Ok(resources)
}

fn allocate_optional_zeroed_pages(
    boot_services: &BootServices,
    pages: usize,
) -> Result<u64, Status> {
    if pages == 0 {
        Ok(0)
    } else {
        allocate_zeroed_pages(boot_services, pages)
    }
}

fn allocate_zeroed_pages(boot_services: &BootServices, pages: usize) -> Result<u64, Status> {
    let address = boot_services
        .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
        .map_err(|error| error.status())?;
    unsafe { write_bytes(address as *mut u8, 0, pages * 4096) };
    Ok(address)
}

fn iommu_error(error: iommu::Error) -> mboot::Error {
    match error {
        iommu::Error::UnsupportedHardware => mboot::Error::UnsupportedIommu,
        iommu::Error::CommandTimeout => mboot::Error::IommuCommandTimeout,
        _ => mboot::Error::IommuInitializationFailed,
    }
}

fn allocate_page(boot_services: &BootServices) -> Result<u64, Status> {
    boot_services
        .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
        .map_err(|error| error.status())
}

fn collect_boot_entropy(boot_services: &BootServices) -> Option<[u8; 32]> {
    let mut material = [0u8; 64];
    let firmware_available = boot_services
        .get_handle_for_protocol::<Rng>()
        .ok()
        .and_then(|handle| boot_services.open_protocol_exclusive::<Rng>(handle).ok())
        .is_some_and(|mut rng| rng.get_rng(None, &mut material[..32]).is_ok());
    let hardware_available = cpu::fill_hardware_random(&mut material[32..]);
    if !firmware_available && !hardware_available {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"mBoot Domain entropy root v1");
    hasher.update([u8::from(firmware_available), u8::from(hardware_available)]);
    hasher.update(material);
    material.fill(0);
    Some(hasher.finalize().into())
}

fn derive_domain_entropy(root: &[u8; 32], domain_id: u32, restart_count: u32) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mBoot Domain entropy v1");
    hasher.update(root);
    hasher.update(domain_id.to_le_bytes());
    hasher.update(restart_count.to_le_bytes());
    hasher.finalize().into()
}

fn allocate_msr_permission_map(
    boot_services: &BootServices,
    backend: BackendKind,
    image_format: ManifestImageFormat,
) -> Result<u64, Status> {
    let address = boot_services
        .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 2)
        .map_err(|error| error.status())?;
    // AMD's MSRPM occupies two contiguous pages. Setting every bit intercepts
    // every covered RDMSR and WRMSR until mBoot explicitly emulates it.
    unsafe { write_bytes(address as *mut u8, 0xff, 8192) };
    if matches!(
        image_format,
        ManifestImageFormat::LinuxPvh | ManifestImageFormat::NativeElf
    ) {
        for msr in [
            0x174,
            0x175,
            0x176,
            0x277,
            0xc000_0080,
            0xc000_0081,
            0xc000_0082,
            0xc000_0083,
            0xc000_0084,
            0xc000_0100,
            0xc000_0101,
            0xc000_0102,
            0xc000_0103,
        ] {
            unsafe { allow_guest_msr(address, backend, msr) };
        }
    }
    Ok(address)
}

unsafe fn allow_guest_msr(bitmap: u64, backend: BackendKind, msr: u32) {
    match backend {
        BackendKind::IntelVmx => {
            let (index, read_base, write_base) = if msr <= 0x1fff {
                (msr as usize, 0usize, 2048usize)
            } else if (0xc000_0000..=0xc000_1fff).contains(&msr) {
                ((msr - 0xc000_0000) as usize, 1024usize, 3072usize)
            } else {
                return;
            };
            let mask = !(1 << (index & 7));
            unsafe {
                let read = (bitmap as *mut u8).add(read_base + index / 8);
                read.write(read.read() & mask);
                let write = (bitmap as *mut u8).add(write_base + index / 8);
                write.write(write.read() & mask);
            }
        }
        BackendKind::AmdSvm => {
            let index = if msr <= 0x1fff {
                msr as usize
            } else if (0xc000_0000..=0xc000_1fff).contains(&msr) {
                8192 + (msr - 0xc000_0000) as usize
            } else if (0xc001_0000..=0xc001_1fff).contains(&msr) {
                16_384 + (msr - 0xc001_0000) as usize
            } else {
                return;
            };
            let bit = index * 2;
            let byte = unsafe { (bitmap as *mut u8).add(bit / 8) };
            let mask = !(0b11 << (bit & 7));
            unsafe { byte.write(byte.read() & mask) };
        }
    }
}

fn allocate_nested_pages(
    boot_services: &BootServices,
    guest_pages: usize,
) -> Result<NestedPageResources, Status> {
    let level1_pages = guest_pages.div_ceil(512);
    Ok(NestedPageResources {
        root: allocate_page(boot_services)?,
        level3: boot_services
            .allocate_pages(
                AllocateType::AnyPages,
                MemoryType::LOADER_DATA,
                SPARSE_LEVEL3_PAGES,
            )
            .map_err(|error| error.status())?,
        level3_pages: SPARSE_LEVEL3_PAGES,
        level2: boot_services
            .allocate_pages(
                AllocateType::AnyPages,
                MemoryType::LOADER_DATA,
                SPARSE_LEVEL2_PAGES * SPARSE_LEVEL3_PAGES,
            )
            .map_err(|error| error.status())?,
        level2_pages: SPARSE_LEVEL2_PAGES * SPARSE_LEVEL3_PAGES,
        level1: boot_services
            .allocate_pages(
                AllocateType::AnyPages,
                MemoryType::LOADER_DATA,
                level1_pages,
            )
            .map_err(|error| error.status())?,
        level1_pages,
        device_level1: boot_services
            .allocate_pages(
                AllocateType::AnyPages,
                MemoryType::LOADER_DATA,
                SPARSE_LEVEL1_PAGES,
            )
            .map_err(|error| error.status())?,
        device_level1_pages: SPARSE_LEVEL1_PAGES,
        device_state: allocate_page(boot_services)?,
        guest_base: boot_services
            .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, guest_pages)
            .map_err(|error| error.status())?,
        guest_pages,
    })
}

fn load_file(
    boot_services: &BootServices,
    image: Handle,
    network_bundle: Option<&[u8]>,
    path: &str,
) -> Result<Vec<u8>, Status> {
    if let Some(bytes) = network_bundle {
        return NetworkBundle::parse(bytes)
            .and_then(|bundle| bundle.file(path))
            .map(Vec::from)
            .map_err(|_| Status::LOAD_ERROR);
    }
    let filesystem = boot_services
        .get_image_file_system(image)
        .map_err(|error| error.status())?;
    let mut filesystem = uefi::fs::FileSystem::new(filesystem);
    let path = CString16::try_from(path).map_err(|_| Status::INVALID_PARAMETER)?;
    filesystem.read(path.as_ref()).map_err(|error| match error {
        FsError::Io(io) => io.uefi_error.status(),
        FsError::Path(_) | FsError::Utf8Encoding(_) => Status::LOAD_ERROR,
    })
}

#[cfg(feature = "uefi-net")]
fn load_network_bundle(
    boot_services: &BootServices,
    image_handle: Handle,
) -> Result<Vec<u8>, Status> {
    let device = {
        let loaded_image = boot_services
            .open_protocol_exclusive::<LoadedImage>(image_handle)
            .map_err(|error| error.status())?;
        loaded_image.device().ok_or(Status::UNSUPPORTED)?
    };
    let mut pxe = open_boot_pxe(boot_services, device)?;
    if !pxe.mode().started {
        pxe.start(false).map_err(|error| error.status())?;
    }
    if !pxe.mode().dhcp_ack_received {
        pxe.dhcp(true).map_err(|error| error.status())?;
    }
    if pxe.mode().using_ipv6 {
        return Err(Status::UNSUPPORTED);
    }

    let (server, boot_file) = pxe_boot_source(pxe.mode()).ok_or(Status::NOT_FOUND)?;
    let filename_bytes = sibling_tftp_path(boot_file, NETWORK_BUNDLE_NAME)?;
    let filename =
        CStr8::from_bytes_with_nul(&filename_bytes).map_err(|_| Status::INVALID_PARAMETER)?;
    let size = pxe
        .tftp_get_file_size(&server, filename)
        .map_err(|error| error.status())?;
    let size = usize::try_from(size).map_err(|_| Status::BAD_BUFFER_SIZE)?;
    if !(32..=MAX_NETWORK_BUNDLE_SIZE).contains(&size) {
        return Err(Status::BAD_BUFFER_SIZE);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    bytes.resize(size, 0);
    let received = pxe
        .tftp_read_file(&server, filename, Some(&mut bytes))
        .map_err(|error| error.status())?;
    let received = usize::try_from(received).map_err(|_| Status::BAD_BUFFER_SIZE)?;
    if received > bytes.len() {
        return Err(Status::BAD_BUFFER_SIZE);
    }
    bytes.truncate(received);
    NetworkBundle::parse(&bytes).map_err(|_| Status::COMPROMISED_DATA)?;
    Ok(bytes)
}

#[cfg(feature = "uefi-net")]
fn open_boot_pxe<'a>(
    boot_services: &'a BootServices,
    boot_device: Handle,
) -> Result<ScopedProtocol<'a, BaseCode>, Status> {
    if let Ok(protocol) = boot_services.open_protocol_exclusive::<BaseCode>(boot_device) {
        return Ok(protocol);
    }

    let handles = boot_services
        .find_handles::<BaseCode>()
        .map_err(|error| error.status())?;
    let mut fallback = None;
    for handle in handles {
        let Ok(protocol) = boot_services.open_protocol_exclusive::<BaseCode>(handle) else {
            continue;
        };
        if protocol.mode().started {
            return Ok(protocol);
        }
        if fallback.is_none() {
            fallback = Some(protocol);
        }
    }
    fallback.ok_or(Status::NOT_FOUND)
}

#[cfg(feature = "uefi-net")]
fn pxe_boot_source(mode: &Mode) -> Option<(IpAddress, &[u8])> {
    let packets = [
        (mode.pxe_reply_received, &mode.pxe_reply),
        (mode.proxy_offer_received, &mode.proxy_offer),
        (mode.dhcp_ack_received, &mode.dhcp_ack),
    ];
    for (valid, packet) in packets {
        if !valid {
            continue;
        }
        let packet: &DhcpV4Packet = packet.as_ref();
        if packet.bootp_si_addr != [0; 4] {
            return Some((
                IpAddress::new_v4(packet.bootp_si_addr),
                &packet.bootp_boot_file,
            ));
        }
    }
    None
}

#[cfg(feature = "uefi-net")]
fn sibling_tftp_path(boot_file: &[u8], sibling: &[u8]) -> Result<Vec<u8>, Status> {
    let end = boot_file
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(boot_file.len());
    let boot_file = &boot_file[..end];
    if boot_file.iter().any(|byte| !byte.is_ascii() || *byte == 0) {
        return Err(Status::INVALID_PARAMETER);
    }
    let directory_end = boot_file
        .iter()
        .rposition(|byte| *byte == b'/' || *byte == b'\\')
        .map_or(0, |index| index + 1);
    let mut path = Vec::new();
    path.try_reserve_exact(directory_end + sibling.len() + 1)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    path.extend_from_slice(&boot_file[..directory_end]);
    path.extend_from_slice(sibling);
    path.push(0);
    Ok(path)
}

fn halt_with_error(stage: &str, error: mboot::Error) -> ! {
    log!("{} initialization failed: {:?}", stage, error);
    match error {
        mboot::Error::VmcsWriteFailed(field) => display::vmcs_failure(field),
        mboot::Error::GuestEntryFailed(instruction_error) => {
            display::vm_entry_failure(instruction_error)
        }
        _ => display::failure(halt_error_code(stage, error)),
    }
    halt()
}

fn halt_error_code(stage: &str, error: mboot::Error) -> u8 {
    match stage {
        "nested page table" => 20,
        "guest page tables" => 21,
        "Domain image" => 22,
        "PCI ownership policy" => 23,
        "virtualization" => match error {
            mboot::Error::VirtualizationDisabled => 31,
            mboot::Error::NestedPagingUnavailable => 32,
            mboot::Error::InvalidPage => 33,
            mboot::Error::ControlInstructionFailed => 34,
            mboot::Error::ControlRegionTooLarge => 35,
            _ => 30,
        },
        "vCPU creation" => 36,
        "Domain" => 40,
        "Domain boot info" => 41,
        "Domain start" => 42,
        "vCPU entry" => match error {
            mboot::Error::GuestEntryFailed(_) => 51,
            mboot::Error::VmcsLoadFailed => 52,
            _ => 50,
        },
        "Domain Hypercall" => 53,
        "Domain stop" => 54,
        "Event Channel manifest" => 55,
        "Grant translation flush" => 56,
        "Grant unmap" => 57,
        "Grant cleanup" => 58,
        "PCI DMA quarantine" => 17,
        "IOMMU protection" => 19,
        _ => 10,
    }
}

fn halt() -> ! {
    loop {
        // SAFETY: `halt` is only used after interrupts have been disabled.
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
}
