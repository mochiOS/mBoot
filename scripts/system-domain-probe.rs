//! Default mode stops at the real System ELF's Ready HVC. Explicit boot mode
//! supplies the selected native boot module and continues actual mnu/EL0
//! execution. A diagnostic budget expiry is a failure, never OS boot success.
use super::*;
use image::ImageMemory;
#[cfg(not(paired_domains))]
use mnu_abi::hypervisor::HypercallNumber;
use mnu_abi::hypervisor::{self, DomainBootInfo};

const RAM_BYTES: usize = if cfg!(hardware_boot_disk) {
    512
} else if cfg!(system_domain_boot) {
    128
} else {
    16
} * 1024
    * 1024;
const fn grant_bytes() -> u64 {
    if cfg!(paired_domains) {
        16 * 1024 * 1024
    } else {
        0x40000
    }
}
const BOOT_INFO: u64 = 0x3000;
#[cfg(not(paired_domains))]
const UPPER: u64 = 0xffff_8000_0000_0000;
static ELF: &[u8] = include_bytes!(env!("MBOOT_SYSTEM_ELF"));
#[cfg(system_domain_boot)]
static MODULE: &[u8] = include_bytes!(env!("MBOOT_SYSTEM_MODULE"));

fn u64_at(bytes: &[u8], index: usize) -> u64 {
    u64::from_le_bytes(bytes[index..index + 8].try_into().expect("ELF u64"))
}
fn u32_at(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index..index + 4].try_into().expect("ELF u32"))
}
fn u16_at(bytes: &[u8], index: usize) -> usize {
    u16::from_le_bytes(bytes[index..index + 2].try_into().expect("ELF u16")) as usize
}

pub(super) struct Prepared<'pool> {
    pub ram: pages::Pages<'pool>,
    pub tables: native_memory::Tables<'pool, 512>,
    pub info: DomainBootInfo,
    pub entry: u64,
    pub loaded_end: u64,
    #[cfg(hardware_boot_disk)]
    pub realtime: Option<qemu_rtc::Realtime>,
    #[cfg(firmware_ramfb)]
    pub framebuffer: qemu_ramfb::Framebuffer<'pool>,
}

pub(super) fn prepare<'pool>(
    config: Config,
    pool: &'pool pages::PagePool,
    tree: &fdt::Fdt<'_>,
    binding: fdt::gic::GicV2,
) -> Prepared<'pool> {
    emit("System Domain real ELF bootstrap entered\n");
    let ram = pool
        .allocate(RAM_BYTES as u64 / PAGE_SIZE, PAGE_SIZE)
        .expect("System RAM");
    let mut tables = native_memory::Tables::<512>::new(pool, config).expect("System tables");
    // SAFETY: Only this stopped guest's private RAM is writable by the loader.
    let loaded = unsafe { image::load_elf(ELF, &ram) }.expect("native System ELF");
    check(
        loaded.entry() == 0x10000 && loaded.loaded_end() < RAM_BYTES as u64 - grant_bytes() - 4096,
        "System image layout does not fit fixture RAM",
    );
    let mut info = DomainBootInfo::new(
        1,
        0,
        hypervisor::HYPERVISOR_BACKEND_ARM_EL2,
        hypervisor::DOMAIN_ROLE_SYSTEM,
        RAM_BYTES as u64,
        RAM_BYTES as u64 - grant_bytes() - 4096,
        4096,
        RAM_BYTES as u64 - grant_bytes(),
        grant_bytes(),
        0x2000_0000,
        0x10000,
        0,
        0,
    );
    info.feature_flags = hypervisor::DOMAIN_FEATURE_SHUTDOWN | hypervisor::DOMAIN_FEATURE_READY;
    if let Some(seed) = tree.rng_seed().expect("fresh firmware seed") {
        info.entropy_seed = seed;
        info.entropy_seed_valid = 1;
    }
    #[cfg(system_domain_boot)]
    {
        mnu_abi::boot_module::decode(MODULE).expect("real selected boot module");
        // SAFETY: The same retained, stopped RAM and disjoint immutable source.
        let (start, size) = unsafe {
            image::load_boot_module(MODULE, loaded.loaded_end(), info.grant_window_start, &ram)
        }
        .expect("selected boot module RAM");
        info.boot_module_start = start;
        info.boot_module_size = size;
        info.arm64_interrupts = mnu_abi::arm64_interrupts::Resources::gic_v2(
            info.device_window_start,
            u32::from(
                fdt::timer::Timers::from_fdt(tree, &binding)
                    .expect("firmware timer wiring")
                    .virtual_timer
                    .id,
            ),
            u32::from(hypervisor::EVENT_CHANNEL_VECTOR),
            u32::from(hypervisor::DOMAIN_MANAGEMENT_VECTOR),
            1,
            2,
        );
        info.feature_flags |= hypervisor::DOMAIN_FEATURE_CONSOLE_WRITE
            | hypervisor::DOMAIN_FEATURE_YIELD
            | hypervisor::DOMAIN_FEATURE_EVENT_CHANNEL
            | hypervisor::DOMAIN_FEATURE_EVENT_IRQ
            | hypervisor::DOMAIN_FEATURE_EVENT_POLL
            | hypervisor::DOMAIN_FEATURE_VIRTUAL_GIC;
    }
    #[cfg(paired_domains)]
    {
        info.feature_flags |= hypervisor::DOMAIN_FEATURE_WAIT
            | hypervisor::DOMAIN_FEATURE_GRANT_TABLE
            | hypervisor::DOMAIN_FEATURE_SHARED_RING
            | hypervisor::DOMAIN_FEATURE_GRANT_QUERY;
    }
    #[cfg(firmware_ramfb)]
    let framebuffer = {
        let framebuffer = qemu_ramfb::Framebuffer::prepare(pool, tree);
        info.device_window_size = 16 * 1024 * 1024;
        info = info.with_framebuffer(
            info.device_window_start + 0x10000,
            qemu_ramfb::BYTES,
            qemu_ramfb::WIDTH,
            qemu_ramfb::HEIGHT,
            qemu_ramfb::WIDTH,
            hypervisor::FRAMEBUFFER_FORMAT_BGR,
        );
        framebuffer
    };
    #[cfg(hardware_boot_disk)]
    let realtime = qemu_rtc::Realtime::prepare(tree);
    #[cfg(hardware_boot_disk)]
    if realtime.is_some() { info.feature_flags |= hypervisor::DOMAIN_FEATURE_REALTIME; }
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
    #[cfg(system_domain_boot)]
    {
        let cpu = binding.virtualization.expect("GIC virtualization").cpu;
        check(
            cpu.size() >= info.arm64_interrupts.cpu_interface_size,
            "short GICV bank",
        );
        for offset in [0, 4096] {
            // SAFETY: Only the assigned CPU-local virtual interface is exposed.
            // Distributor and GICH remain private; no physical IRQ delegation.
            unsafe {
                mapper
                    .map_page(
                        info.device_window_start + offset,
                        cpu.start() + offset,
                        Access {
                            writable: true,
                            executable: false,
                            memory: MemoryType::Device,
                        },
                    )
                    .unwrap()
            };
        }
    }
    #[cfg(firmware_ramfb)]
    for offset in (0..qemu_ramfb::BYTES).step_by(4096) {
        // SAFETY: Retained scanout RAM, disjoint from both Domains and EL2
        // tables/firmware control. Only the System receives this device range.
        unsafe {
            mapper
                .map_page(
                    info.framebuffer_addr + offset,
                    framebuffer.pixels.physical_address(offset, 4096).unwrap(),
                    Access {
                        writable: true,
                        executable: false,
                        memory: MemoryType::Device,
                    },
                )
                .unwrap();
        }
    }
    for address in (0..RAM_BYTES as u64).step_by(4096) {
        let physical = ram
            .physical_address(address, 4096)
            .expect("System RAM page");
        // Stage-2 admits the System Domain's private RAM. Its retained mnu
        // Stage-1 tables enforce process W^X, including newly allocated ELF
        // frames; denying execute on all allocator RAM prevents any EL0 code.
        // EL2/shared/device pages are never included in this RAM assignment.
        let access = Access {
            executable: cfg!(system_domain_boot),
            ..READ_WRITE
        };
        unsafe {
            mapper
                .map_page(address, physical, access)
                .expect("System RAM map")
        };
    }
    #[cfg(system_domain_boot)]
    for page in (info.boot_module_start
        ..(info.boot_module_start + info.boot_module_size + 4095) & !4095)
        .step_by(4096)
    {
        // SAFETY: Retained boot bytes are immutable and never executable.
        unsafe { mapper.protect_page(page, READ_ONLY).unwrap() };
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
    drop(mapper);
    Prepared {
        ram,
        tables,
        info,
        entry: loaded.entry(),
        loaded_end: loaded.loaded_end(),
        #[cfg(hardware_boot_disk)]
        realtime,
        #[cfg(firmware_ramfb)]
        framebuffer,
    }
}

#[cfg(not(paired_domains))]
pub(super) fn run(
    config: Config,
    pool: &pages::PagePool,
    tree: &fdt::Fdt<'_>,
    binding: fdt::gic::GicV2,
) {
    let Prepared {
        ram,
        mut tables,
        info,
        entry,
        loaded_end,
        #[cfg(hardware_boot_disk)]
        realtime: _,
        #[cfg(firmware_ramfb)]
            framebuffer: _,
    } = prepare(config, pool, tree, binding);
    #[cfg(not(system_domain_boot))]
    let _ = info;
    let root = tables.root();
    let mapper = unsafe { Mapper::new(&mut tables, root, config, 3).unwrap() };
    select(&mapper);
    match vcpu::Vcpu::new(&mapper, entry, BOOT_INFO, 0) {
        Ok(mut guest) => {
            #[cfg(system_domain_boot)]
            let mut runtime = {
                // SAFETY: This fixed reference boot owns the cold single-CPU
                // GIC and EL2 timer. Every other guest/DMA/CPU is stopped.
                let runtime = unsafe {
                    BootRuntime::new(
                        binding,
                        fdt::timer::Timers::from_fdt(tree, &binding).unwrap(),
                    )
                };
                guest.configure_interrupts(&runtime.gic).unwrap();
                runtime
            };
            let mut reached_ready = false;
            for _ in 0..100_000 {
                #[cfg(not(system_domain_boot))]
                let exit = unsafe { guest.run(&mapper).expect("real System vCPU") };
                #[cfg(system_domain_boot)]
                let exit = {
                    guest
                        .synchronize_virtual_timer(info.arm64_interrupts.timer_interrupt as u16)
                        .unwrap();
                    // SAFETY: Same retained one-CPU ownership as the loader;
                    // the physical timer is owned by runtime, not the guest.
                    unsafe {
                        guest
                            .run_with_gic(&mapper, &mut runtime.gic)
                            .expect("System interrupt entry")
                    }
                };
                #[cfg(system_domain_boot)]
                if runtime.boot_exit(&mut guest, exit, &ram) {
                    continue;
                }
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
            #[cfg(system_domain_boot)]
            {
                let control: u64;
                let deadline: u64;
                let now: u64;
                unsafe {
                    asm!("mrs {}, cnthp_ctl_el2", "mrs {}, cnthp_cval_el2", "mrs {}, cntpct_el0",
                    out(reg) control, out(reg) deadline, out(reg) now, options(nomem, nostack));
                }
                report(format_args!("Boot run limit: physical IRQs={} timer ctl={:#x} deadline={} now={} guest pstate={:#x}\n",
                    runtime.physical_irqs, control, deadline, now, guest.pstate()));
                unsafe {
                    report(format_args!(
                        "GIC enabled={:#x} pending={:#x} highest={:#x} control={:#x}\n",
                        read_volatile((binding.distributor.start() + 0x100) as *const u32),
                        read_volatile((binding.distributor.start() + 0x200) as *const u32),
                        read_volatile((binding.cpu.start() + 0x18) as *const u32),
                        read_volatile(binding.cpu.start() as *const u32)
                    ));
                }
                check(
                    false,
                    "native OS startup did not finish within the diagnostic run",
                );
            }
            check(reached_ready, "System vCPU did not reach actual Ready");
            emit("Real System ELF / native vCPU resume / Ready PASS\n");
        }
        Err(vcpu::Error::UnsupportedCpu) => {
            // Still exercise bootstrap on CPUs whose extended bank the new
            // continuation does not support yet, using the original one-shot.
            unsafe { enter_guest(entry, 0, BOOT_INFO) };
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
        pc >= UPPER + 0x200000 && pc < UPPER + loaded_end,
        "System PC is not retained in TTBR1",
    );
    check(
        stack >= UPPER && stack < UPPER + loaded_end && stack & 15 == 0,
        "System stack is not retained/aligned",
    );
    check(
        control & ((1 << 19) | 1) == (1 << 19) | 1,
        "System MMU/WXN is disabled",
    );
    check(
        upper_root & !4095 < loaded_end,
        "System upper root is outside owned RAM",
    );
    emit("Real System ELF / Stage-2 / upper EL1 Ready PASS\n");
}

#[cfg(all(system_domain_boot, not(paired_domains)))]
struct BootRuntime {
    gic: vgic::Controller,
    physical: irq::Controller,
    timer: irq::Timer,
    events: mboot::event::EventChannelTable,
    event_irq_enabled: bool,
    ready: bool,
    physical_irqs: u64,
}

#[cfg(all(system_domain_boot, not(paired_domains)))]
impl BootRuntime {
    unsafe fn new(gic: fdt::gic::GicV2, timers: fdt::timer::Timers) -> Self {
        for ppi in [timers.virtual_timer, timers.hypervisor] {
            check(
                ppi.cpu_mask == 0 || ppi.cpu_mask & 1 != 0,
                "timer not wired to admitted CPU",
            );
        }
        let mut timer = unsafe {
            irq::Timer::new(1000, timers.counter_frequency).expect("EL2 timer owner")
        };
        // SAFETY: Caller exclusively owns these Device mappings and CPU banks.
        let physical = unsafe {
            irq::Controller::new(
                gic.distributor.start(),
                gic.distributor.size(),
                gic.cpu.start(),
                gic.cpu.size(),
                timers.hypervisor.id,
            )
            .expect("physical GIC timer owner")
        };
        let virtualized = gic.virtualization.unwrap();
        let gic = unsafe {
            vgic::Controller::new(virtualized.control.start(), virtualized.control.size()).unwrap()
        };
        timer.rearm().unwrap();
        Self {
            gic,
            physical,
            timer,
            events: mboot::event::EventChannelTable::new(),
            event_irq_enabled: false,
            ready: false,
            physical_irqs: 0,
        }
    }

    fn boot_exit(
        &mut self,
        guest: &mut vcpu::Vcpu,
        exit: vcpu::Exit,
        ram: &pages::Pages<'_>,
    ) -> bool {
        if exit.kind == vcpu::ExitKind::Irq {
            if let Some(pending) = self.physical.acknowledge().expect("owned physical IRQ") {
                self.timer
                    .rearm()
                    .expect("rearm physical source before EOI");
                pending.complete().expect("physical IRQ token EOI");
                self.physical_irqs += 1;
                if self.physical_irqs == 1 {
                    emit("EL2 physical timer / owned GIC acknowledgement active\n");
                }
            }
            return true;
        }
        if exit.kind == vcpu::ExitKind::Synchronous && exit.esr >> 26 == 1 {
            guest
                .complete_wait()
                .expect("architectural wait completion");
            return true;
        }
        if let Some(register) = guest.system_register_read() {
            let value: u64;
            // SAFETY: Read-only physical ID registers on this admitted CPU. The
            // trusted frontend exposes only this baseline profile; unknown banks
            // are rejected, never synthesized as zero/success.
            unsafe {
                match (
                    register.op0,
                    register.op1,
                    register.crn,
                    register.crm,
                    register.op2,
                ) {
                    (3, 0, 0, 7, 0) => asm!("mrs {}, id_aa64mmfr0_el1", out(reg) value),
                    (3, 0, 0, 7, 1) => asm!("mrs {}, id_aa64mmfr1_el1", out(reg) value),
                    (3, 0, 0, 4, 0) => asm!("mrs {}, id_aa64pfr0_el1", out(reg) value),
                    (3, 0, 0, 6, 0) => asm!("mrs {}, id_aa64isar0_el1", out(reg) value),
                    _ => {
                        report(format_args!("Unsupported System MRS: {:?}\n", register));
                        finish(1);
                    }
                }
            }
            guest.complete_system_register_read(value).unwrap();
            return true;
        }
        if exit.kind == vcpu::ExitKind::Synchronous && exit.esr >> 26 == 0x16 {
            let call = guest.register(0).unwrap();
            let no_arguments = (1..=3).all(|i| guest.register(i) == Some(0));
            let result = if call == HypercallNumber::Ready as u64 {
                if self.ready || !no_arguments {
                    hypervisor::HYPERCALL_INVALID_ARGUMENT
                } else {
                    self.ready = true;
                    emit("System Domain ready; entering mnu initialization\n");
                    hypervisor::HYPERCALL_SUCCESS
                }
            } else if call == HypercallNumber::EventIrqEnable as u64 {
                if !self.ready || self.event_irq_enabled || !no_arguments {
                    hypervisor::HYPERCALL_INVALID_ARGUMENT
                } else {
                    self.event_irq_enabled = true;
                    hypervisor::HYPERCALL_SUCCESS
                }
            } else if call == HypercallNumber::EventPoll as u64 {
                if !no_arguments {
                    hypervisor::HYPERCALL_INVALID_ARGUMENT
                } else {
                    u64::from(
                        self.events
                            .receive(mboot::domain::DomainId::new(1))
                            .unwrap_or(0),
                    )
                }
            } else if call == HypercallNumber::EventSend as u64 {
                if guest.register(2) != Some(0) || guest.register(3) != Some(0) {
                    hypervisor::HYPERCALL_INVALID_ARGUMENT
                } else {
                    match u32::try_from(guest.register(1).unwrap())
                        .ok()
                        .and_then(|port| {
                            self.events.send(mboot::domain::DomainId::new(1), port).ok()
                        }) {
                        Some(_) => hypervisor::HYPERCALL_SUCCESS,
                        None => hypervisor::HYPERCALL_INVALID_ARGUMENT,
                    }
                }
            } else if call == HypercallNumber::Yield as u64 {
                if no_arguments {
                    hypervisor::HYPERCALL_SUCCESS
                } else {
                    hypervisor::HYPERCALL_INVALID_ARGUMENT
                }
            } else if call == HypercallNumber::Shutdown as u64 {
                report(format_args!(
                    "System Domain requested shutdown reason={}\n",
                    guest.register(1).unwrap()
                ));
                finish(1)
            } else if call == HypercallNumber::ConsoleWrite as u64 {
                let offset = guest.register(1).unwrap();
                let size = guest.register(2).unwrap();
                if let Some(host) = ram
                    .host_address(offset, size)
                    .filter(|_| size <= 4096 && guest.register(3) == Some(0))
                {
                    // SAFETY: Owned stopped RAM with complete checked bounds.
                    let bytes =
                        unsafe { core::slice::from_raw_parts(host as *const u8, size as usize) };
                    if let Ok(message) = core::str::from_utf8(bytes) {
                        emit(message);
                        hypervisor::HYPERCALL_SUCCESS
                    } else {
                        hypervisor::HYPERCALL_INVALID_ARGUMENT
                    }
                } else {
                    hypervisor::HYPERCALL_INVALID_ARGUMENT
                }
            } else {
                report(format_args!("Unsupported System HVC {}\n", call));
                hypervisor::HYPERCALL_UNSUPPORTED
            };
            guest.complete_hypercall(result).unwrap();
            return true;
        }
        report(format_args!("Unhandled System exit {:?}\n", exit));
        finish(1)
    }
}
