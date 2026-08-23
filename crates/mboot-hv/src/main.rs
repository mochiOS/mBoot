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
use mboot_hv::arch::x86_64::{cpu, descriptor};
use mboot_hv::domain::{Domain, DomainId, DomainRole};
use mboot_hv::manifest::{LaunchManifest, ManifestDomainRole};
use mboot_hv::memory::{NestedPageResources, NestedPageTable};
use mboot_hv::scheduler::CooperativeScheduler;
use mboot_hv::{
    image, BackendKind, GuestConfig, Virtualization, VirtualizationResources, VmExitReason,
};
use mnu_abi::hypervisor::{
    DomainBootInfo, HypercallNumber, HYPERCALL_INVALID_ARGUMENT, HYPERCALL_SUCCESS,
    HYPERCALL_UNSUPPORTED, HYPERVISOR_BACKEND_AMD_SVM, HYPERVISOR_BACKEND_INTEL_VMX,
};
use uefi::fs::Error as FsError;
use uefi::prelude::*;
use uefi::table::boot::{AllocateType, MemoryType};
use uefi::CString16;

const MAX_MEMORY_REGIONS: usize = 256;
const MAX_GUEST_MEMORY_PAGES: usize = 512;
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
    memory_size: u64,
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
            || !(16..=MAX_GUEST_MEMORY_PAGES).contains(&guest_pages)
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
            memory_size: config.memory_size,
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
            domain.nested_pages().guest_memory_size(),
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
                stack: prepared.memory_size - 16,
                boot_info: DOMAIN_BOOT_INFO_GPA,
            },
            domain,
            virtualization,
            started: false,
            pending_result: HYPERCALL_SUCCESS,
            yield_count: 0,
            _image: prepared.image,
        });
    }

    let mut scheduler = CooperativeScheduler::new();
    while let Some(index) = scheduler.next(&runnable) {
        let runtime = &mut runtime_domains[index];
        // SAFETY: The selected vCPU is stopped and owns all guest and control state.
        let vm_exit = if runtime.started {
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
        if vm_exit.reason != VmExitReason::Hypercall {
            let _ = runtime.domain.mark_crashed();
            halt_with_error(
                "Domain Hypercall",
                mboot_hv::Error::UnexpectedVmExit(vm_exit.raw_reason),
            );
        }
        if vm_exit.hypercall_number == HypercallNumber::Shutdown as u64 {
            if let Err(error) = runtime.domain.stop() {
                halt_with_error("Domain stop", error);
            }
            runnable[index] = false;
            log!(
                "Domain {} stopped: reason={} yields={} raw={:#x}",
                runtime.domain.id().get(),
                vm_exit.arg0,
                runtime.yield_count,
                vm_exit.raw_reason
            );
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
            _ => HYPERCALL_UNSUPPORTED,
        };
    }
    log!(
        "bootstrap complete; {} Domains entered and stopped cleanly",
        runtime_domains.len()
    );
    display::success();

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
    crate::serial::print(format_args!("[mnu Domain {}] {}", domain_id.get(), message));
    HYPERCALL_SUCCESS
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
    display::failure(10);
    halt()
}

fn halt() -> ! {
    loop {
        // SAFETY: `halt` is only used after interrupts have been disabled.
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
}
