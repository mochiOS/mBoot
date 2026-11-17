//! Execute the actual ARM64 Linux Hardware Domain and its real PID1. This
//! bounded bring-up run does not stand in for System storage or desktop boot.
use super::*;
use image::ImageMemory;
#[cfg(not(paired_domains))]
use mnu_abi::hypervisor::{self, HypercallNumber};

pub(super) const RAM: u64 = if cfg!(hardware_boot_disk) { 768 } else { 128 } * 1024 * 1024;
const DTB: u64 = 0x10000;
const INITRD: u64 = 32 * 1024 * 1024;
pub(super) const GICD: u64 = 0x3000_0000;
const GICV: u64 = 0x3001_0000;
pub(super) const TIMER: u16 = 27;
static IMAGE: &[u8] = include_bytes!(env!("MBOOT_HARDWARE_IMAGE"));
static INIT: &[u8] = include_bytes!(env!("MBOOT_HARDWARE_INITRD"));

fn description(bytes: &mut [u8], seed: [u8; 32]) -> usize {
    use fdt::writer::Writer;
    let mut tree = Writer::new(bytes).unwrap();
    let cells = |values: &[u32]| {
        let mut bytes = [0; 64];
        for (index, value) in values.iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        bytes
    };
    macro_rules! prop {
        ($name:expr, $($value:expr),+) => {{
            let values: &[u32] = &[$($value),+];
            tree.property($name, &cells(values)[..values.len() * 4]).unwrap();
        }};
    }
    tree.begin("").unwrap();
    prop!("#address-cells", 2);
    prop!("#size-cells", 2);
    prop!("interrupt-parent", 1);
    tree.property("compatible", b"mochios,hardware-domain\0")
        .unwrap();
    tree.begin("chosen").unwrap();
    tree.property(
        "bootargs",
        if cfg!(hardware_boot_disk) { b"mboot.text_console=1 loglevel=8 keep_bootcon panic=-1 nosmp rdinit=/init mboot.boot_ramdisk=1 ramdisk_size=262144\0" } else { b"mboot.text_console=1 loglevel=8 keep_bootcon panic=-1 nosmp rdinit=/init\0" },
    )
    .unwrap();
    tree.property("rng-seed", &seed).unwrap();
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"mBoot guest KASLR seed v1\0");
    hash.update(seed);
    let kaslr = hash.finalize();
    tree.property("kaslr-seed", &kaslr[..8]).unwrap();
    prop!("linux,initrd-start", 0, INITRD as u32);
    prop!("linux,initrd-end", 0, (INITRD + INIT.len() as u64) as u32);
    tree.end().unwrap();
    tree.begin("memory@0").unwrap();
    tree.property("device_type", b"memory\0").unwrap();
    prop!("reg", 0, 0, 0, (RAM - 16 * 1024 * 1024) as u32);
    tree.end().unwrap();
    tree.begin("cpus").unwrap();
    prop!("#address-cells", 1);
    prop!("#size-cells", 0);
    tree.begin("cpu@0").unwrap();
    tree.property("device_type", b"cpu\0").unwrap();
    tree.property("compatible", b"arm,cortex-a53\0").unwrap();
    prop!("reg", 0);
    tree.end().unwrap();
    tree.end().unwrap();
    tree.begin("interrupt-controller@30000000").unwrap();
    tree.property("compatible", b"arm,gic-400\0").unwrap();
    tree.property("interrupt-controller", &[]).unwrap();
    prop!("#interrupt-cells", 3);
    prop!("phandle", 1);
    prop!("reg", 0, GICD as u32, 0, 4096, 0, GICV as u32, 0, 8192);
    tree.end().unwrap();
    tree.begin("timer").unwrap();
    tree.property("compatible", b"arm,armv8-timer\0").unwrap();
    prop!("interrupts", 1, 14, 4, 1, 11, 4, 1, 10, 4);
    tree.property("interrupt-names", b"phys\0virt\0hyp-phys\0")
        .unwrap();
    tree.property("always-on", &[]).unwrap();
    tree.end().unwrap();
    tree.begin("psci").unwrap();
    tree.property("compatible", b"arm,psci-0.2\0").unwrap();
    tree.property("method", b"hvc\0").unwrap();
    tree.end().unwrap();
    tree.begin("hypervisor").unwrap();
    tree.property("compatible", b"mochios,mboot\0").unwrap();
    prop!("mochios,abi-version", 1);
    prop!(
        "mochios,grant-window",
        0,
        (RAM - 16 * 1024 * 1024) as u32,
        0,
        16 * 1024 * 1024
    );
    tree.end().unwrap();
    tree.end().unwrap();
    tree.finish().unwrap().len()
}

pub(super) fn registers(guest: &mut vcpu::Vcpu) -> bool {
    if let Some(register) = guest.system_register_read() {
        let value: u64;
        unsafe {
            match (
                register.op0,
                register.op1,
                register.crn,
                register.crm,
                register.op2,
            ) {
                (3, 0, 0, 1, 0) => asm!("mrs {}, s3_0_c0_c1_0", out(reg) value),
                (3, 0, 0, 1, 1) => asm!("mrs {}, s3_0_c0_c1_1", out(reg) value),
                (3, 0, 0, 1, 2) => asm!("mrs {}, s3_0_c0_c1_2", out(reg) value),
                (3, 0, 0, 1, 4) => asm!("mrs {}, s3_0_c0_c1_4", out(reg) value),
                (3, 0, 0, 1, 5) => asm!("mrs {}, s3_0_c0_c1_5", out(reg) value),
                (3, 0, 0, 1, 6) => asm!("mrs {}, s3_0_c0_c1_6", out(reg) value),
                (3, 0, 0, 1, 7) => asm!("mrs {}, s3_0_c0_c1_7", out(reg) value),
                (3, 0, 0, 2, 0) => asm!("mrs {}, s3_0_c0_c2_0", out(reg) value),
                (3, 0, 0, 2, 1) => asm!("mrs {}, s3_0_c0_c2_1", out(reg) value),
                (3, 0, 0, 2, 2) => asm!("mrs {}, s3_0_c0_c2_2", out(reg) value),
                (3, 0, 0, 2, 3) => asm!("mrs {}, s3_0_c0_c2_3", out(reg) value),
                (3, 0, 0, 2, 4) => asm!("mrs {}, s3_0_c0_c2_4", out(reg) value),
                (3, 0, 0, 2, 5) => asm!("mrs {}, s3_0_c0_c2_5", out(reg) value),
                (3, 0, 0, 2, 6) => asm!("mrs {}, s3_0_c0_c2_6", out(reg) value),
                (3, 0, 0, 2, 7) => asm!("mrs {}, s3_0_c0_c2_7", out(reg) value),
                (3, 0, 0, 3, 0) => asm!("mrs {}, s3_0_c0_c3_0", out(reg) value),
                (3, 0, 0, 3, 1) => asm!("mrs {}, s3_0_c0_c3_1", out(reg) value),
                (3, 0, 0, 3, 2) => asm!("mrs {}, s3_0_c0_c3_2", out(reg) value),
                (3, 0, 0, 3, 4) => asm!("mrs {}, s3_0_c0_c3_4", out(reg) value),
                (3, 0, 0, 3, 5) => asm!("mrs {}, s3_0_c0_c3_5", out(reg) value),
                (3, 0, 0, 3, 6) => asm!("mrs {}, s3_0_c0_c3_6", out(reg) value),
                (3, 0, 0, 4, 7) => asm!("mrs {}, s3_0_c0_c4_7", out(reg) value),
                (3, 0, 0, 0, 6) => asm!("mrs {}, revidr_el1", out(reg) value),
                (3, 0, 0, 7, 0) => asm!("mrs {}, id_aa64mmfr0_el1", out(reg) value),
                (3, 0, 0, 7, 1) => asm!("mrs {}, id_aa64mmfr1_el1", out(reg) value),
                (3, 0, 0, 7, 2) => asm!("mrs {}, id_aa64mmfr2_el1", out(reg) value),
                (3, 0, 0, 7, 3) => asm!("mrs {}, s3_0_c0_c7_3", out(reg) value),
                (3, 0, 0, 7, 4) => asm!("mrs {}, s3_0_c0_c7_4", out(reg) value),
                (3, 0, 0, 4, 0) => asm!("mrs {}, id_aa64pfr0_el1", out(reg) value),
                (3, 0, 0, 4, 1) => asm!("mrs {}, id_aa64pfr1_el1", out(reg) value),
                (3, 0, 0, 4, 2) => asm!("mrs {}, s3_0_c0_c4_2", out(reg) value),
                (3, 0, 0, 4, 4) => asm!("mrs {}, s3_0_c0_c4_4", out(reg) value),
                (3, 0, 0, 4, 5) => asm!("mrs {}, s3_0_c0_c4_5", out(reg) value),
                (3, 0, 0, 6, 0) => asm!("mrs {}, id_aa64isar0_el1", out(reg) value),
                (3, 0, 0, 6, 1) => asm!("mrs {}, id_aa64isar1_el1", out(reg) value),
                (3, 0, 0, 6, 2) => asm!("mrs {}, id_aa64isar2_el1", out(reg) value),
                (3, 0, 0, 6, 3) => asm!("mrs {}, s3_0_c0_c6_3", out(reg) value),
                (3, 0, 0, 5, 0) => asm!("mrs {}, id_aa64dfr0_el1", out(reg) value),
                (3, 0, 0, 5, 1) => asm!("mrs {}, id_aa64dfr1_el1", out(reg) value),
                (3, 0, 0, 5, 2) => asm!("mrs {}, s3_0_c0_c5_2", out(reg) value),
                (3, 1, 0, 0, 1) => asm!("mrs {}, clidr_el1", out(reg) value),
                (3, 3, 0, 0, 1) => asm!("mrs {}, ctr_el0", out(reg) value),
                _ => {
                    report(format_args!("Unsupported Linux MRS {:?}\n", register));
                    finish(1);
                }
            }
        }
        guest.complete_system_register_read(value).unwrap();
        return true;
    }
    if let Some((register, value)) = guest.system_register_write() {
        // All debug/PMU access stays trapped in this baseline. These exact
        // initialization writes only disable access already denied by EL2.
        // Enabling controls or unknown cache/debug operations is rejected.
        match (
            register.op0,
            register.op1,
            register.crn,
            register.crm,
            register.op2,
            value,
        ) {
            (2, 0, 0, 2, 2, 0 | 0x1000) | (3, 3, 9, 14, 0, 0) | (2, 0, 1, 0 | 3, 4, 0) => {
                guest.complete_system_register_write().unwrap();
                return true;
            }
            _ => {
                report(format_args!(
                    "Unsupported Linux MSR {:?} value={:#x}\n",
                    register, value
                ));
                finish(1);
            }
        }
    }
    false
}

pub(super) struct Prepared<'pool> {
    pub ram: pages::Pages<'pool>,
    pub tables: native_memory::Tables<'pool, 512>,
    pub guest: vcpu::Vcpu,
    pub distributor: vgic::distributor::Distributor,
}
pub(super) fn prepare<'pool>(
    config: Config,
    pool: &'pool pages::PagePool,
    tree: &fdt::Fdt<'_>,
    binding: fdt::gic::GicV2,
    gic: &vgic::Controller,
) -> Prepared<'pool> {
    emit("Hardware Domain actual Linux Image entered\n");
    let ram = pool.allocate(RAM / PAGE_SIZE, PAGE_SIZE).unwrap();
    let mut tables = native_memory::Tables::<512>::new(pool, config).unwrap();
    // SAFETY: Disjoint immutable Image, stopped exclusively retained guest RAM.
    let image = unsafe { image::load_arm64_linux(IMAGE, 0x200000, &ram) }.unwrap();
    check(
        image.loaded_end() <= INITRD && INITRD + (INIT.len() as u64) < RAM - 16 * 1024 * 1024,
        "Linux payload overlap",
    );
    let mut blob = [0u8; 4096];
    // Domain-separated entropy from the trusted fresh firmware handoff. A
    // guest description or cached input alone never establishes entropy.
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"mBoot Hardware Domain seed v1\0");
    hash.update(tree.rng_seed().unwrap().expect("fresh boot entropy"));
    let size = description(&mut blob, hash.finalize().into());
    unsafe {
        core::ptr::copy_nonoverlapping(
            blob.as_ptr(),
            ram.guest_host_address(DTB, size as u64).unwrap() as *mut u8,
            size,
        );
        core::ptr::copy_nonoverlapping(
            INIT.as_ptr(),
            ram.guest_host_address(INITRD, INIT.len() as u64).unwrap() as *mut u8,
            INIT.len(),
        );
    }
    let root = tables.root();
    let mut mapper = unsafe { Mapper::new(&mut tables, root, config, 4).unwrap() };
    select(&mapper);
    for page in (0..RAM).step_by(4096) {
        unsafe {
            mapper
                .map_page(
                    page,
                    ram.physical_address(page, 4096).unwrap(),
                    Access {
                        executable: true,
                        ..READ_WRITE
                    },
                )
                .unwrap();
        }
    }
    let virtualized = binding.virtualization.unwrap();
    for offset in [0, 4096] {
        unsafe {
            mapper
                .map_page(
                    GICV + offset,
                    virtualized.cpu.start() + offset,
                    Access {
                        writable: true,
                        executable: false,
                        memory: MemoryType::Device,
                    },
                )
                .unwrap();
        }
    }
    let mut guest = vcpu::Vcpu::new(&mapper, image.entry(), DTB, 0).unwrap();
    guest.configure_interrupts(gic).unwrap();
    drop(mapper);
    Prepared {
        ram,
        tables,
        guest,
        distributor: vgic::distributor::Distributor::new(),
    }
}

#[cfg(not(paired_domains))]
pub(super) fn run(
    config: Config,
    pool: &pages::PagePool,
    tree: &fdt::Fdt<'_>,
    binding: fdt::gic::GicV2,
) {
    let virtualized = binding.virtualization.unwrap();
    let timers = fdt::timer::Timers::from_fdt(tree, &binding).unwrap();
    let mut physical = unsafe {
        irq::Controller::new(
            binding.distributor.start(),
            binding.distributor.size(),
            binding.cpu.start(),
            binding.cpu.size(),
            timers.hypervisor.id,
        )
        .unwrap()
    };
    let mut timer = unsafe { irq::Timer::new(1000).unwrap() };
    timer.rearm().unwrap();
    let mut gic = unsafe {
        vgic::Controller::new(virtualized.control.start(), virtualized.control.size()).unwrap()
    };
    let Prepared {
        ram,
        mut tables,
        mut guest,
        mut distributor,
    } = prepare(config, pool, tree, binding, &gic);
    let root = tables.root();
    let mapper = unsafe { Mapper::new(&mut tables, root, config, 4).unwrap() };
    select(&mapper);
    unsafe {
        asm!("dsb sy", "ic iallu", "dsb sy", "isb", options(nostack));
    }
    for _ in 0..200_000 {
        distributor.synchronize(&mut guest, TIMER).unwrap();
        let exit = unsafe { guest.run_with_gic(&mapper, &mut gic).unwrap() };
        if exit.kind == vcpu::ExitKind::Irq {
            if let Some(pending) = physical.acknowledge().unwrap() {
                timer.rearm().unwrap();
                pending.complete().unwrap();
            }
            continue;
        }
        if exit.kind == vcpu::ExitKind::Synchronous && exit.esr >> 26 == 1 {
            guest.complete_wait().unwrap();
            continue;
        }
        if registers(&mut guest) {
            continue;
        }
        if let Some(access) = guest.mmio_access() {
            if (GICD..GICD + 4096).contains(&access.address) {
                let value = if access.write {
                    distributor
                        .write(access.address - GICD, access.size, access.value, &mut guest)
                        .expect("Linux Distributor write");
                    0
                } else {
                    distributor
                        .read(access.address - GICD, access.size, &guest)
                        .expect("Linux Distributor read")
                };
                guest.complete_mmio(value).unwrap();
                continue;
            }
        }
        if exit.kind == vcpu::ExitKind::Synchronous && exit.esr >> 26 == 0x16 {
            let call = guest.register(0).unwrap();
            let result = match call {
                0x8400_0000 => 2, // This single-vCPU virtual PSCI implements 0.2.
                0x8400_0008 | 0x8400_0009 => {
                    emit("Linux requested machine stop\n");
                    finish(1);
                }
                0x8000_0000 => 0x10001,  // SMCCC v1.1 discovery only.
                0x8000_0001 => u64::MAX, // No architectural workaround advertised.
                x if x == HypercallNumber::ConsoleWrite as u64 => {
                    let address = guest.register(1).unwrap();
                    let size = guest.register(2).unwrap();
                    if let Some(host) = ram.host_address(address, size).filter(|_| size <= 4096) {
                        let bytes = unsafe {
                            core::slice::from_raw_parts(host as *const u8, size as usize)
                        };
                        if let Ok(text) = core::str::from_utf8(bytes) {
                            emit(text);
                            hypervisor::HYPERCALL_SUCCESS
                        } else {
                            hypervisor::HYPERCALL_INVALID_ARGUMENT
                        }
                    } else {
                        hypervisor::HYPERCALL_INVALID_ARGUMENT
                    }
                }
                x if x == HypercallNumber::DeviceQuery as u64 => {
                    hypervisor::HYPERCALL_INVALID_ARGUMENT
                }
                _ => {
                    report(format_args!("Linux HVC {:#x}\n", call));
                    hypervisor::HYPERCALL_UNSUPPORTED
                }
            };
            guest.complete_hypercall(result).unwrap();
            continue;
        }
        let pc: u64;
        unsafe {
            asm!("mrs {}, elr_el2", out(reg) pc);
        }
        report(format_args!(
            "Unhandled Linux exit {:?} pc={:#x}\n",
            exit, pc
        ));
        finish(1);
    }
    check(
        false,
        "Linux bring-up budget exhausted; not OS boot completion",
    );
}
