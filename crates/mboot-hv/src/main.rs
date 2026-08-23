#![no_std]
#![no_main]

extern crate alloc;

mod panic;
mod serial;

use alloc::vec::Vec;
use core::arch::asm;
use core::mem::size_of;
use core::ptr::copy_nonoverlapping;
use mboot_hv::arch::x86_64::{cpu, descriptor};
use mboot_hv::domain::{Domain, DomainId};
use mboot_hv::image;
use mboot_hv::memory::{NestedPageResources, NestedPageTable};
use mboot_hv::{BackendKind, GuestConfig, Virtualization, VirtualizationResources, VmExitReason};
use mnu_abi::hypervisor::{
    DomainBootInfo, HypercallNumber, HYPERCALL_INVALID_ARGUMENT, HYPERCALL_SUCCESS,
    HYPERCALL_UNSUPPORTED, HYPERVISOR_BACKEND_AMD_SVM, HYPERVISOR_BACKEND_INTEL_VMX,
};
use uefi::fs::Error as FsError;
use uefi::prelude::*;
use uefi::table::boot::{AllocateType, MemoryType};
use uefi::CString16;

const MAX_MEMORY_REGIONS: usize = 256;
const GUEST_MEMORY_PAGES: usize = 512;
const DOMAIN_BOOT_INFO_GPA: u64 = 0x3000;
const DOMAIN_STACK_TOP: u64 = GUEST_MEMORY_PAGES as u64 * 4096 - 16;
const MAX_CONSOLE_WRITE: u64 = 4096;

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

    let features = cpu::detect();
    let vendor = core::str::from_utf8(&features.vendor).unwrap_or("unknown");
    let Some(backend) = features.backend else {
        log!("CPU {} has neither VMX nor SVM", vendor);
        return Status::UNSUPPORTED;
    };
    log!(
        "CPU {} backend={:?} nested-paging={:?} asids={}",
        vendor,
        backend,
        features.nested_paging,
        features.address_space_ids
    );

    let boot_services = system_table.boot_services();
    let guest_elf = match load_guest_elf(boot_services, image_handle) {
        Ok(image) => image,
        Err(status) => {
            log!("failed to load mnu Domain image: {:?}", status);
            return status;
        }
    };
    let host_control_page = match allocate_page(boot_services) {
        Ok(page) => page,
        Err(status) => return status,
    };
    let vcpu_control_page = match allocate_page(boot_services) {
        Ok(page) => page,
        Err(status) => return status,
    };
    let nested_pages = match allocate_nested_pages(boot_services) {
        Ok(pages) => pages,
        Err(status) => return status,
    };

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

    // SAFETY: Every page was allocated from UEFI for exclusive mBoot use and is
    // still identity-mapped at this stage.
    let nested = match unsafe { NestedPageTable::initialize(backend, nested_pages) } {
        Ok(table) => table,
        Err(error) => halt_with_error("nested page table", error),
    };
    // SAFETY: The Domain is stopped and all guest pages belong to this table.
    let guest_cr3 = match unsafe { nested.initialize_guest_page_tables() } {
        Ok(root) => root,
        Err(error) => halt_with_error("guest page tables", error),
    };
    // SAFETY: The Domain is stopped and its RAM is exclusively owned by mBoot.
    let guest_image = match unsafe { image::load_elf(&guest_elf, &nested) } {
        Ok(image) => image,
        Err(error) => halt_with_error("mnu image", error),
    };

    // SAFETY: The control pages are exclusively owned, execution is pinned to
    // the BSP, interrupts are disabled, and the code is running at CPL0.
    let mut virtualization = match unsafe {
        Virtualization::enable(VirtualizationResources {
            host_control_page,
            vcpu_control_page,
        })
    } {
        Ok(virtualization) => virtualization,
        Err(error) => halt_with_error("virtualization", error),
    };

    let mut domain = Domain::new(DomainId::new(1), virtualization.kind(), nested);
    if let Err(error) = domain.mark_ready() {
        halt_with_error("domain", error);
    }
    log!(
        "Domain {} ready: backend={:?} nested-root={:#x} guest-memory={:#x}+{} KiB",
        domain.id().get(),
        domain.backend(),
        domain.nested_pages().hardware_root(),
        domain.nested_pages().guest_base(),
        domain.nested_pages().guest_memory_size() / 1024
    );
    let backend = match domain.backend() {
        BackendKind::IntelVmx => HYPERVISOR_BACKEND_INTEL_VMX,
        BackendKind::AmdSvm => HYPERVISOR_BACKEND_AMD_SVM,
    };
    let boot_info = DomainBootInfo::new(
        domain.id().get(),
        0,
        backend,
        domain.nested_pages().guest_memory_size(),
    );
    let Some(boot_info_host) = domain
        .nested_pages()
        .guest_host_address(DOMAIN_BOOT_INFO_GPA, size_of::<DomainBootInfo>() as u64)
    else {
        halt_with_error("Domain boot info", mboot_hv::Error::InvalidPage)
    };
    // SAFETY: The destination is an aligned, in-bounds part of stopped guest RAM.
    unsafe {
        copy_nonoverlapping(
            &boot_info as *const DomainBootInfo,
            boot_info_host as *mut DomainBootInfo,
            1,
        )
    };
    if let Err(error) = domain.start() {
        halt_with_error("domain start", error);
    }
    // SAFETY: The image, stack, and page tables are in live Domain RAM, and this
    // is still the pinned BSP with interrupts disabled.
    let mut vm_exit = match unsafe {
        virtualization.run(GuestConfig {
            nested_root: domain.nested_pages().hardware_root(),
            page_table_root: guest_cr3,
            entry: guest_image.entry(),
            stack: DOMAIN_STACK_TOP,
            boot_info: DOMAIN_BOOT_INFO_GPA,
        })
    } {
        Ok(vm_exit) => vm_exit,
        Err(error) => {
            let _ = domain.mark_crashed();
            halt_with_error("guest entry", error)
        }
    };
    loop {
        if vm_exit.reason != VmExitReason::Hypercall {
            let _ = domain.mark_crashed();
            halt_with_error(
                "mnu Hypercall",
                mboot_hv::Error::UnexpectedVmExit(vm_exit.raw_reason),
            );
        }
        if vm_exit.hypercall_number == HypercallNumber::Shutdown as u64 {
            break;
        }
        let result = match vm_exit.hypercall_number {
            number if number == HypercallNumber::ConsoleWrite as u64 => {
                handle_console_write(domain.nested_pages(), vm_exit.arg0, vm_exit.arg1)
            }
            number if number == HypercallNumber::Yield as u64 => HYPERCALL_SUCCESS,
            _ => HYPERCALL_UNSUPPORTED,
        };
        // SAFETY: The previous exit was a Hypercall from this stopped vCPU.
        vm_exit = match unsafe { virtualization.resume(result) } {
            Ok(exit) => exit,
            Err(error) => {
                let _ = domain.mark_crashed();
                halt_with_error("mnu Hypercall resume", error)
            }
        };
    }
    if let Err(error) = domain.stop() {
        halt_with_error("domain stop", error);
    }
    log!(
        "Domain {} exited: reason={:?} raw={:#x}",
        domain.id().get(),
        vm_exit.reason,
        vm_exit.raw_reason
    );
    log!("mnu requested Domain shutdown: reason={}", vm_exit.arg0);
    log!("bootstrap complete; mnu Domain entry and Hypercall verified");

    let _keep_virtualization_active = virtualization;
    halt()
}

fn handle_console_write(memory: &NestedPageTable, address: u64, len: u64) -> u64 {
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
    crate::serial::print(format_args!("[mnu] {}", message));
    HYPERCALL_SUCCESS
}

fn allocate_page(boot_services: &BootServices) -> Result<u64, Status> {
    boot_services
        .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
        .map_err(|error| error.status())
}

fn allocate_nested_pages(boot_services: &BootServices) -> Result<NestedPageResources, Status> {
    Ok(NestedPageResources {
        root: allocate_page(boot_services)?,
        level3: allocate_page(boot_services)?,
        level2: allocate_page(boot_services)?,
        level1: allocate_page(boot_services)?,
        guest_base: boot_services
            .allocate_pages(
                AllocateType::AnyPages,
                MemoryType::LOADER_DATA,
                GUEST_MEMORY_PAGES,
            )
            .map_err(|error| error.status())?,
        guest_pages: GUEST_MEMORY_PAGES,
    })
}

fn load_guest_elf(boot_services: &BootServices, image: Handle) -> Result<Vec<u8>, Status> {
    let filesystem = boot_services
        .get_image_file_system(image)
        .map_err(|error| error.status())?;
    let mut filesystem = uefi::fs::FileSystem::new(filesystem);
    let path =
        CString16::try_from("\\EFI\\MBOOT\\MNU.ELF").map_err(|_| Status::INVALID_PARAMETER)?;
    filesystem.read(path.as_ref()).map_err(|error| match error {
        FsError::Io(io) => io.uefi_error.status(),
        FsError::Path(_) | FsError::Utf8Encoding(_) => Status::LOAD_ERROR,
    })
}

fn halt_with_error(stage: &str, error: mboot_hv::Error) -> ! {
    log!("{} initialization failed: {:?}", stage, error);
    halt()
}

fn halt() -> ! {
    loop {
        // SAFETY: `halt` is only used after interrupts have been disabled.
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
}
