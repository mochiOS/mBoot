#![no_std]
#![no_main]

mod panic;
mod serial;

use core::arch::asm;
use core::ptr::write_volatile;
use mboot_hv::arch::x86_64::{cpu, descriptor};
use mboot_hv::domain::{Domain, DomainId};
use mboot_hv::memory::{NestedPageResources, NestedPageTable};
use mboot_hv::{Virtualization, VirtualizationResources};
use uefi::prelude::*;
use uefi::table::boot::{AllocateType, MemoryType};

const MAX_MEMORY_REGIONS: usize = 256;

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
unsafe fn main(_image: Handle, system_table: SystemTable<Boot>) -> Status {
    serial::init();
    log!("starting independent hypervisor");

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
    // SAFETY: This is the exclusively owned and mapped guest page.
    unsafe { write_volatile(nested.guest_page() as *mut u8, 0xf4) };

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
        "Domain {} ready: backend={:?} nested-root={:#x} guest-page={:#x}",
        domain.id().get(),
        domain.backend(),
        domain.nested_pages().hardware_root(),
        domain.nested_pages().guest_page()
    );
    if let Err(error) = domain.start() {
        halt_with_error("domain start", error);
    }
    // SAFETY: The guest page contains HLT, the nested tables are live, and this
    // is still the pinned BSP with interrupts disabled.
    let vm_exit = match unsafe { virtualization.run(domain.nested_pages().hardware_root()) } {
        Ok(vm_exit) => vm_exit,
        Err(error) => {
            let _ = domain.mark_crashed();
            halt_with_error("guest entry", error)
        }
    };
    if let Err(error) = domain.stop() {
        halt_with_error("domain stop", error);
    }
    log!(
        "Domain {} exited: reason={:?} raw={:#x}",
        domain.id().get(),
        vm_exit.reason,
        vm_exit.raw_reason
    );
    log!("bootstrap complete; guest entry and VM exit verified");

    let _keep_virtualization_active = virtualization;
    halt()
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
        guest_page: allocate_page(boot_services)?,
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
