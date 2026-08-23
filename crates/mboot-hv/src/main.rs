#![no_std]
#![no_main]

extern crate alloc;

mod display;
mod panic;
mod serial;

use alloc::vec::Vec;
use core::arch::asm;
use core::mem::size_of;
use core::ptr::copy_nonoverlapping;
use mboot_hv::arch::x86_64::{cpu, descriptor, timer};
use mboot_hv::domain::{Domain, DomainId, DomainRole, DomainState};
use mboot_hv::event::EventChannelTable;
use mboot_hv::grant::{GrantRef, GrantTable};
use mboot_hv::manifest::{LaunchManifest, ManifestDomainRole};
use mboot_hv::memory::{NestedPageResources, NestedPageTable};
use mboot_hv::scheduler::CooperativeScheduler;
use mboot_hv::{
    image, BackendKind, GuestConfig, Virtualization, VirtualizationResources, VmExitReason,
};
use mnu_abi::hypervisor::{
    DomainBootInfo, HypercallNumber, DOMAIN_ROLE_APPLICATION, DOMAIN_ROLE_HARDWARE,
    DOMAIN_ROLE_SYSTEM, EVENT_CHANNEL_VECTOR, GRANT_FLAG_WRITABLE, HYPERCALL_INVALID_ARGUMENT,
    HYPERCALL_SUCCESS, HYPERCALL_UNSUPPORTED, HYPERVISOR_BACKEND_AMD_SVM,
    HYPERVISOR_BACKEND_INTEL_VMX,
};
use uefi::fs::Error as FsError;
use uefi::prelude::*;
use uefi::table::boot::{AllocateType, MemoryType};
use uefi::CString16;

const MAX_MEMORY_REGIONS: usize = 256;
const MAX_GUEST_MEMORY_PAGES: usize = 512;
const GRANT_WINDOW_PAGES: usize = 16;
const DOMAIN_BOOT_INFO_GPA: u64 = 0x3000;
const MAX_CONSOLE_WRITE: u64 = 4096;
const LAUNCH_MANIFEST_PATH: &str = "\\EFI\\MBOOT\\LAUNCH.MF";

include!(concat!(env!("OUT_DIR"), "/launch_manifest_digest.rs"));

macro_rules! log {
    ($($arg:tt)*) => {
        crate::serial::print(format_args!("[mBoot-HV] {}\n", format_args!($($arg)*)))
    };
}

#[derive(Clone, Copy)]
struct MemoryRegion {
    start: u64,
    len: u64,
    usable: bool,
}

impl MemoryRegion {
    const EMPTY: Self = Self {
        start: 0,
        len: 0,
        usable: false,
    };
}

struct BootMemoryMap {
    regions: [MemoryRegion; MAX_MEMORY_REGIONS],
    len: usize,
}

struct PreparedDomain {
    id: u32,
    role: DomainRole,
    capabilities: u64,
    image: Vec<u8>,
    nested_pages: NestedPageResources,
    vcpu_control_page: u64,
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
    resume_preempted: bool,
    event_irq_enabled: bool,
    event_irq_pending: bool,
    _image: Vec<u8>,
}

impl BootMemoryMap {
    const fn new() -> Self {
        Self {
            regions: [MemoryRegion::EMPTY; MAX_MEMORY_REGIONS],
            len: 0,
        }
    }

    fn push(&mut self, region: MemoryRegion) {
        if self.len < self.regions.len() {
            self.regions[self.len] = region;
            self.len += 1;
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

    let boot_services = system_table.boot_services();
    let manifest_bytes = match load_file(boot_services, image_handle, LAUNCH_MANIFEST_PATH) {
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
            || !config.is_required()
            || config.vcpu_count != 1
            || !(GRANT_WINDOW_PAGES + 16..=MAX_GUEST_MEMORY_PAGES).contains(&guest_pages)
        {
            log!("unsupported configuration for Domain {}", config.id);
            display::failure(6);
            return Status::UNSUPPORTED;
        }
        let image = match load_file(boot_services, image_handle, config.image_path) {
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
        let vcpu_control_page = match allocate_page(boot_services) {
            Ok(page) => page,
            Err(status) => {
                display::failure(12);
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
            image,
            nested_pages,
            vcpu_control_page,
        });
        runnable.push(true);
    }
    if system_domains != 1 {
        log!("Launch Manifest must contain exactly one System Domain");
        display::failure(9);
        return Status::UNSUPPORTED;
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
        });
    }

    // SAFETY: UEFI entered at CPL0; boot services are gone and mBoot now owns
    // interrupt and descriptor-table policy.
    unsafe {
        asm!("cli", options(nomem, nostack));
        descriptor::install();
    }
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
        let guest_cr3 = match unsafe { nested.initialize_guest_page_tables() } {
            Ok(root) => root,
            Err(error) => halt_with_error("guest page tables", error),
        };
        // SAFETY: The Domain is stopped and its RAM is exclusively owned by mBoot.
        let guest_image = match unsafe { image::load_elf(&prepared.image, &nested) } {
            Ok(image) => image,
            Err(error) => halt_with_error("Domain image", error),
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
        let boot_info = DomainBootInfo::new(
            domain.id().get(),
            0,
            backend_id,
            abi_domain_role(domain.role()),
            domain.nested_pages().guest_memory_size(),
            grant_window_start(domain.nested_pages()),
            GRANT_WINDOW_PAGES as u64 * 4096,
        );
        let Some(boot_info_host) = domain
            .nested_pages()
            .guest_host_address(DOMAIN_BOOT_INFO_GPA, size_of::<DomainBootInfo>() as u64)
        else {
            halt_with_error("Domain boot info", mboot_hv::Error::InvalidPage)
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
                nested_root: domain.nested_pages().hardware_root(),
                page_table_root: guest_cr3,
                entry: guest_image.entry(),
                stack: grant_window_start(domain.nested_pages()) - 16,
                boot_info: DOMAIN_BOOT_INFO_GPA,
            },
            domain,
            virtualization,
            started: false,
            pending_result: HYPERCALL_SUCCESS,
            yield_count: 0,
            preemption_count: 0,
            ready: false,
            waiting: false,
            resume_preempted: false,
            event_irq_enabled: false,
            event_irq_pending: false,
            _image: prepared.image,
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
            halt_with_error("Event Channel manifest", mboot_hv::Error::InvalidManifest)
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
    while let Some(index) = scheduler.next(&runnable) {
        let runtime = &mut runtime_domains[index];
        if runtime.event_irq_pending {
            if let Err(error) = unsafe {
                runtime
                    .virtualization
                    .inject_interrupt(EVENT_CHANNEL_VECTOR)
            } {
                halt_with_error("Event IRQ injection", error)
            }
            runtime.event_irq_pending = false;
        }
        // SAFETY: The selected vCPU is stopped and owns all guest and control state.
        let vm_exit = if runtime.resume_preempted {
            runtime.resume_preempted = false;
            // SAFETY: The timer stopped this vCPU without completing a guest instruction.
            unsafe { runtime.virtualization.resume_preempted() }
        } else if runtime.started {
            // SAFETY: This vCPU stopped at the previous VM exit and remains selected.
            unsafe { runtime.virtualization.resume(runtime.pending_result) }
        } else {
            runtime.started = true;
            // SAFETY: This is the first entry into the stopped, fully prepared vCPU.
            unsafe { runtime.virtualization.run(runtime.guest) }
        };
        let vm_exit = match vm_exit {
            Ok(exit) => exit,
            Err(error) => {
                let _ = runtime.domain.mark_crashed();
                halt_with_error("vCPU entry", error)
            }
        };
        if vm_exit.reason == VmExitReason::Preempted {
            runtime.preemption_count += 1;
            if runtime.preemption_count <= 3 {
                log!(
                    "Domain {} preempted: count={}",
                    runtime.domain.id().get(),
                    runtime.preemption_count
                );
            }
            runtime.resume_preempted = true;
            continue;
        }
        if vm_exit.reason != VmExitReason::Hypercall {
            let _ = runtime.domain.mark_crashed();
            halt_with_error(
                "Domain Hypercall",
                mboot_hv::Error::UnexpectedVmExit(vm_exit.raw_reason),
            );
        }
        if vm_exit.hypercall_number == HypercallNumber::Shutdown as u64 {
            let stopping_domain = runtime_domains[index].domain.id();
            for mapping in grants.cleanup_domain(stopping_domain).into_iter().flatten() {
                let Some(target_index) = runtime_domains
                    .iter()
                    .position(|runtime| runtime.domain.id() == mapping.target)
                else {
                    halt_with_error("Grant cleanup", mboot_hv::Error::InvalidState)
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
                log!(
                    "Event Channel {}:{} -> {}:{}",
                    sender.get(),
                    vm_exit.arg0,
                    delivery.domain.get(),
                    delivery.port
                );
                if let Some(target_index) = target_index {
                    if runtime_domains[target_index].waiting {
                        let port = event_channels
                            .receive(delivery.domain)
                            .unwrap_or(delivery.port);
                        runtime_domains[target_index].pending_result = u64::from(port);
                        runtime_domains[target_index].waiting = false;
                        runnable[target_index] = true;
                    } else if runtime_domains[target_index].event_irq_enabled {
                        runtime_domains[target_index].event_irq_pending = true;
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
                runtime_domains[index].event_irq_pending = event_channels.has_pending(receiver);
                runtime_domains[index].pending_result = HYPERCALL_SUCCESS;
                log!(
                    "Domain {} enabled Event Channel IRQ vector {:#x}",
                    receiver.get(),
                    EVENT_CHANNEL_VECTOR
                );
            }
            continue;
        }
        if vm_exit.hypercall_number == HypercallNumber::EventWait as u64 {
            if vm_exit.arg0 != 0 || vm_exit.arg1 != 0 || vm_exit.arg2 != 0 {
                runtime_domains[index].pending_result = HYPERCALL_INVALID_ARGUMENT;
                continue;
            }
            let receiver = runtime_domains[index].domain.id();
            if let Some(port) = event_channels.receive(receiver) {
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
                runtime.domain.nested_pages(),
                vm_exit.arg0,
                vm_exit.arg1,
            ),
            number if number == HypercallNumber::Yield as u64 => {
                runtime.yield_count += 1;
                HYPERCALL_SUCCESS
            }
            number if number == HypercallNumber::Ready as u64 => {
                if runtime.domain.role() != DomainRole::System || runtime.ready {
                    HYPERCALL_INVALID_ARGUMENT
                } else {
                    runtime.ready = true;
                    log!("mochiOS System Domain {} ready", runtime.domain.id().get());
                    display::mochios_ready();
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
    if waiting_domains == 0 {
        log!(
            "bootstrap complete; {} Domains entered and stopped cleanly",
            runtime_domains.len()
        );
        display::bootstrap_success();
    } else {
        log!("{} resident Domain(s) waiting", waiting_domains);
    }

    let _keep_domains_alive = runtime_domains;
    let _keep_prepared_storage = prepared_domains;
    let _keep_manifest_alive = manifest_bytes;
    halt()
}

fn handle_console_write(
    domain_id: DomainId,
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
    HYPERCALL_SUCCESS
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

fn grant_window_contains(memory: &NestedPageTable, guest_page: u64) -> bool {
    guest_page & 0xfff == 0
        && guest_page >= grant_window_start(memory)
        && guest_page
            .checked_add(4096)
            .is_some_and(|end| end <= memory.guest_memory_size())
}

fn allocate_page(boot_services: &BootServices) -> Result<u64, Status> {
    boot_services
        .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
        .map_err(|error| error.status())
}

fn allocate_nested_pages(
    boot_services: &BootServices,
    guest_pages: usize,
) -> Result<NestedPageResources, Status> {
    Ok(NestedPageResources {
        root: allocate_page(boot_services)?,
        level3: allocate_page(boot_services)?,
        level2: allocate_page(boot_services)?,
        level1: allocate_page(boot_services)?,
        guest_base: boot_services
            .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, guest_pages)
            .map_err(|error| error.status())?,
        guest_pages,
    })
}

fn load_file(boot_services: &BootServices, image: Handle, path: &str) -> Result<Vec<u8>, Status> {
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

fn halt_with_error(stage: &str, error: mboot_hv::Error) -> ! {
    log!("{} initialization failed: {:?}", stage, error);
    match error {
        mboot_hv::Error::VmcsWriteFailed(field) => display::vmcs_failure(field),
        mboot_hv::Error::GuestEntryFailed(instruction_error) => {
            display::vm_entry_failure(instruction_error)
        }
        _ => display::failure(halt_error_code(stage, error)),
    }
    halt()
}

fn halt_error_code(stage: &str, error: mboot_hv::Error) -> u8 {
    match stage {
        "nested page table" => 20,
        "guest page tables" => 21,
        "Domain image" => 22,
        "virtualization" => match error {
            mboot_hv::Error::VirtualizationDisabled => 31,
            mboot_hv::Error::NestedPagingUnavailable => 32,
            mboot_hv::Error::InvalidPage => 33,
            mboot_hv::Error::ControlInstructionFailed => 34,
            mboot_hv::Error::ControlRegionTooLarge => 35,
            _ => 30,
        },
        "vCPU creation" => 36,
        "Domain" => 40,
        "Domain boot info" => 41,
        "Domain start" => 42,
        "vCPU entry" => match error {
            mboot_hv::Error::GuestEntryFailed(_) => 51,
            mboot_hv::Error::VmcsLoadFailed => 52,
            _ => 50,
        },
        "Domain Hypercall" => 53,
        "Domain stop" => 54,
        "Event Channel manifest" => 55,
        "Grant translation flush" => 56,
        "Grant unmap" => 57,
        "Grant cleanup" => 58,
        _ => 10,
    }
}

fn halt() -> ! {
    loop {
        // SAFETY: `halt` is only used after interrupts have been disabled.
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
}
