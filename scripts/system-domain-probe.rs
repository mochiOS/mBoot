//! Execute the real System Domain ELF only through its initial Ready HVC.
//! No success response is supplied, no boot module is consumed and no services
//! are started. This verifies bootstrap translation, not a completed OS boot.
use super::*;
use image::ImageMemory;
use mnu_abi::hypervisor::{self, DomainBootInfo, HypercallNumber};

const RAM_BYTES: usize = 16 * 1024 * 1024;
const BOOT_INFO: u64 = 0x3000;
const UPPER: u64 = 0xffff_8000_0000_0000;
static ELF: &[u8] = include_bytes!(env!("MBOOT_SYSTEM_ELF"));

fn u64_at(bytes: &[u8], index: usize) -> u64 {
    u64::from_le_bytes(bytes[index..index + 8].try_into().expect("ELF u64"))
}
fn u32_at(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index..index + 4].try_into().expect("ELF u32"))
}
fn u16_at(bytes: &[u8], index: usize) -> usize {
    u16::from_le_bytes(bytes[index..index + 2].try_into().expect("ELF u16")) as usize
}

pub(super) fn run(config: Config, pool: &pages::PagePool) {
    emit("System Domain real ELF bootstrap entered\n");
    let ram = pool
        .allocate(RAM_BYTES as u64 / PAGE_SIZE, PAGE_SIZE)
        .expect("System RAM");
    let mut tables = native_memory::Tables::<32>::new(pool, config).expect("System tables");
    // SAFETY: Only this stopped guest's private RAM is writable by the loader.
    let loaded = unsafe { image::load_elf(ELF, &ram) }.expect("native System ELF");
    check(
        loaded.entry() == 0x10000 && loaded.loaded_end() < RAM_BYTES as u64 - 0x41000,
        "System image layout does not fit fixture RAM",
    );
    let mut info = DomainBootInfo::new(
        1,
        0,
        hypervisor::HYPERVISOR_BACKEND_ARM_EL2,
        hypervisor::DOMAIN_ROLE_SYSTEM,
        RAM_BYTES as u64,
        RAM_BYTES as u64 - 0x41000,
        4096,
        RAM_BYTES as u64 - 0x40000,
        0x40000,
        0x2000_0000,
        0x10000,
        0,
        0,
    );
    info.feature_flags = hypervisor::DOMAIN_FEATURE_SHUTDOWN | hypervisor::DOMAIN_FEATURE_READY;
    check(info.validate().is_ok(), "native bootstrap metadata");
    // The declared module range is intentionally not a runnable boot module;
    // this probe stops at Ready, before mnu can try to consume module bytes.
    unsafe {
        (ram.guest_host_address(BOOT_INFO, core::mem::size_of::<DomainBootInfo>() as u64)
            .expect("boot metadata RAM") as *mut DomainBootInfo)
            .write(info);
    }
    let root = tables.root();
    // SAFETY: Separate root/pool, new VMID, no device assignment or DMA.
    let mut mapper = unsafe { Mapper::new(&mut tables, root, config, 3).expect("System root") };
    select(&mapper);
    for address in (0..RAM_BYTES as u64).step_by(4096) {
        let physical = ram
            .physical_address(address, 4096)
            .expect("System RAM page");
        unsafe {
            mapper
                .map_page(address, physical, READ_WRITE)
                .expect("System RAM map")
        };
    }
    // Inspect program headers independently after the actual loader admitted
    // the image. The test grants execute only to its real RX load segments.
    let headers = u64_at(ELF, 32) as usize;
    let stride = u16_at(ELF, 54);
    for index in 0..u16_at(ELF, 56) {
        let header = headers + index * stride;
        if u32_at(ELF, header) != 1 {
            continue;
        }
        let flags = u32_at(ELF, header + 4);
        check(flags & 3 != 3, "W+X System ELF segment");
        let start = u64_at(ELF, header + 24);
        let end = start + u64_at(ELF, header + 40);
        let access = if flags & 1 != 0 {
            READ_EXECUTE
        } else if flags & 2 != 0 {
            READ_WRITE
        } else {
            READ_ONLY
        };
        for page in ((start & !4095)..((end + 4095) & !4095)).step_by(4096) {
            unsafe {
                mapper
                    .protect_page(page, access)
                    .expect("System ELF permissions")
            };
        }
    }
    // EL2's fixture has caching off; finish image publication and instruction
    // visibility before this memory is ever executed by EL1.
    unsafe { asm!("dsb sy", "ic iallu", "dsb sy", "isb", options(nostack)) };
    match vcpu::Vcpu::new(&mapper, loaded.entry(), BOOT_INFO, 0) {
        Ok(mut guest) => {
            let mut reached_ready = false;
            for _ in 0..16 {
                let exit = unsafe { guest.run(&mapper).expect("real System vCPU") };
                if exit.kind == vcpu::ExitKind::Synchronous && exit.esr >> 26 == 0x16 {
                    check(
                        guest.register(0) == Some(HypercallNumber::Ready as u64),
                        "System bootstrap reached an unexpected HVC",
                    );
                    reached_ready = true;
                    break;
                }
                // Fixture policy only: the baseline entry needs its real 4K
                // granule/PARange features. No fabricated mitigation response,
                // unknown sysreg or unsupported bank is silently accepted.
                check(
                    guest.system_register_read()
                        == Some(vcpu::SystemRegister {
                            op0: 3,
                            op1: 0,
                            crn: 0,
                            crm: 7,
                            op2: 0,
                        }),
                    "unexpected System bootstrap register trap",
                );
                let features: u64;
                unsafe {
                    asm!("mrs {}, id_aa64mmfr0_el1", out(reg) features, options(nomem, nostack));
                }
                guest
                    .complete_system_register_read(features)
                    .expect("MMFR0 read");
                check(
                    guest.complete_system_register_read(features).is_err(),
                    "MRS completed twice",
                );
            }
            check(reached_ready, "System vCPU did not reach actual Ready");
            emit("Real System ELF / native vCPU resume / Ready PASS\n");
        }
        Err(vcpu::Error::UnsupportedCpu) => {
            // Still exercise bootstrap on CPUs whose extended bank the new
            // continuation does not support yet, using the original one-shot.
            unsafe { enter_guest(loaded.entry(), 0, BOOT_INFO) };
            let exit = unsafe { read_volatile(addr_of!(LAST_EXIT)) };
            check(
                exit.esr >> 26 == 0x16 && exit.value == HypercallNumber::Ready as u64,
                "System bootstrap did not reach its actual Ready HVC",
            );
        }
        Err(_) => check(false, "System vCPU admission failed"),
    }
    let pc: u64;
    let stack: u64;
    let control: u64;
    let upper_root: u64;
    unsafe {
        asm!("mrs {}, elr_el2", out(reg) pc, options(nomem, nostack));
        asm!("mrs {}, sp_el1", out(reg) stack, options(nomem, nostack));
        asm!("mrs {}, sctlr_el1", out(reg) control, options(nomem, nostack));
        asm!("mrs {}, ttbr1_el1", out(reg) upper_root, options(nomem, nostack));
    }
    check(
        pc >= UPPER + 0x200000 && pc < UPPER + loaded.loaded_end(),
        "System PC is not retained in TTBR1",
    );
    check(
        stack >= UPPER && stack < UPPER + loaded.loaded_end() && stack & 15 == 0,
        "System stack is not retained/aligned",
    );
    check(
        control & ((1 << 19) | 1) == (1 << 19) | 1,
        "System MMU/WXN is disabled",
    );
    check(
        upper_root & !4095 < loaded.loaded_end(),
        "System upper root is outside owned RAM",
    );
    emit("Real System ELF / Stage-2 / upper EL1 Ready PASS\n");
}
