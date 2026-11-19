//! Retain and run the actual System and Hardware images together. This
//! reference bring-up driver reuses production Grant/Event/scheduling tables.
//! Its run limit is a failure, never a substitute for actual OS startup.
use super::*;
use mboot::domain::DomainId;
use mboot::event::EventChannelTable;
use mboot::grant::{GrantRef, GrantTable};
use mboot::scheduler::CooperativeScheduler;
use mnu_abi::hypervisor::*;

const WINDOW: u64 = 16 * 1024 * 1024;

fn in_window(ram: &pages::Pages<'_>, address: u64, count: u32) -> bool {
    address & 4095 == 0
        && count != 0
        && address >= ram.size() - WINDOW
        && address
            .checked_add(u64::from(count) * 4096)
            .is_some_and(|end| end <= ram.size())
}

pub(super) fn run(
    config: Config,
    pool: &pages::PagePool,
    tree: &fdt::Fdt<'_>,
    binding: fdt::gic::GicV2,
) {
    let system = system_domain_probe::prepare(config, pool, tree, binding);
    let virtualized = binding.virtualization.unwrap();
    let mut gic = unsafe {
        vgic::Controller::new(virtualized.control.start(), virtualized.control.size()).unwrap()
    };
    let hardware = hardware_domain_probe::prepare(config, pool, tree, binding, &gic);
    let system_domain_probe::Prepared {
        ram: system_ram,
        tables: system_tables,
        info,
        entry,
        loaded_end,
        #[cfg(hardware_boot_disk)]
        realtime,
        #[cfg(firmware_ramfb)]
        mut framebuffer,
    } = system;
    check(entry < loaded_end, "System entry outside loaded image");
    let hardware_domain_probe::Prepared {
        ram: hardware_ram,
        tables: hardware_tables,
        guest: hardware_guest,
        mut distributor,
    } = hardware;
    let ram = [system_ram, hardware_ram];
    let mut tables = [system_tables, hardware_tables];
    let roots = [tables[0].root(), tables[1].root()];
    let system_guest = {
        let mapper = unsafe { Mapper::new(&mut tables[0], roots[0], config, 3).unwrap() };
        select(&mapper);
        let mut guest = vcpu::Vcpu::new(&mapper, entry, 0x3000, 0).unwrap();
        guest.configure_interrupts(&gic).unwrap();
        guest
    };
    let mut guests = [system_guest, hardware_guest];
    let timers = fdt::timer::Timers::from_fdt(tree, &binding).unwrap();
    let mut timer = unsafe { irq::Timer::new(1000, timers.counter_frequency).unwrap() };
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
    timer.rearm().unwrap();
    let mut events = EventChannelTable::new();
    for port in 1..=4 {
        events
            .connect(DomainId::new(1), port, DomainId::new(2), port)
            .unwrap();
    }
    let mut grants = GrantTable::new();
    let mut scheduler = CooperativeScheduler::new();
    let mut event_wait = [false; 2];
    let mut runnable = [true; 2];
    let mut ready = [false; 2];
    let mut event_irq = [false; 2];
    let mut shared_maps = 0;
    emit("Actual System / Hardware Domains retained; entering shared dispatcher\n");
    // The real filesystem and authenticated built-in images require more work
    // than the control-only fixture. Both profiles remain explicitly bounded.
    let dispatch_budget = if cfg!(hardware_boot_disk) {
        10_000_000
    } else {
        1_000_000
    };
    for _ in 0..dispatch_budget {
        for index in 0..2 {
            if event_wait[index] {
                if let Some(port) = events.receive(DomainId::new(index as u32 + 1)) {
                    guests[index].complete_hypercall(u64::from(port)).unwrap();
                    event_wait[index] = false;
                    runnable[index] = true;
                }
            }
        }
        let now: u64;
        let frequency: u64;
        unsafe {
            asm!("mrs {}, cntpct_el0", "mrs {}, cntfrq_el0", out(reg) now, out(reg) frequency);
        }
        let Some(index) = scheduler.next_in_slice(&runnable, now, frequency / 1000) else {
            // IRQs are masked at EL2; WFI still wakes for a pending owned timer.
            // No physical exception handler or foreign interrupt is bypassed.
            unsafe {
                asm!("wfi", options(nomem, nostack));
            }
            if let Some(pending) = physical.acknowledge().unwrap() {
                timer.rearm().unwrap();
                pending.complete().unwrap();
            }
            continue;
        };
        let mut mapper = unsafe {
            Mapper::new(&mut tables[index], roots[index], config, 3 + index as u8).unwrap()
        };
        select(&mapper);
        if index == 0 {
            guests[index]
                .synchronize_virtual_timer(info.arm64_interrupts.timer_interrupt as u16)
                .unwrap();
            if event_irq[0] {
                guests[0]
                    .set_interrupt_level(
                        u16::from(EVENT_CHANNEL_VECTOR),
                        0x40,
                        events.has_pending(DomainId::new(1)),
                    )
                    .unwrap();
            }
        } else {
            distributor
                .synchronize(&mut guests[1], hardware_domain_probe::TIMER)
                .unwrap();
        }
        let exit = unsafe { guests[index].run_with_gic(&mapper, &mut gic).unwrap() };
        if exit.kind == vcpu::ExitKind::Irq {
            if let Some(pending) = physical.acknowledge().unwrap() {
                timer.rearm().unwrap();
                pending.complete().unwrap();
            }
            continue;
        }
        if exit.kind == vcpu::ExitKind::Synchronous && exit.esr >> 26 == 1 {
            // Architectural spurious wakeup is permitted. Retain the saved
            // continuation and surrender this slice instead of restarting it.
            guests[index].complete_wait().unwrap();
            continue;
        }
        if hardware_domain_probe::registers(&mut guests[index]) {
            scheduler.resume_after_emulation(index);
            continue;
        }
        if index == 1 {
            if let Some(access) = guests[1].mmio_access() {
                let base = hardware_domain_probe::GICD;
                if (base..base + 4096).contains(&access.address) {
                    let result = if access.write {
                        distributor
                            .write(
                                access.address - base,
                                access.size,
                                access.value,
                                &mut guests[1],
                            )
                            .unwrap();
                        0
                    } else {
                        distributor
                            .read(access.address - base, access.size, &guests[1])
                            .unwrap()
                    };
                    guests[1].complete_mmio(result).unwrap();
                    scheduler.resume_after_emulation(index);
                    continue;
                }
            }
        }
        if exit.kind != vcpu::ExitKind::Synchronous || exit.esr >> 26 != 0x16 {
            report(format_args!(
                "Domain {} unhandled exit {:?}\n",
                index + 1,
                exit
            ));
            finish(1);
        }
        // A Domain's authority belongs to its EL1 kernel, never to EL0 code.
        if guests[index].pstate() & 0x1f != 5 {
            guests[index]
                .complete_hypercall(HYPERCALL_UNSUPPORTED)
                .unwrap();
            continue;
        }
        let call = guests[index].register(0).unwrap();
        let a = guests[index].register(1).unwrap();
        let b = guests[index].register(2).unwrap();
        let c = guests[index].register(3).unwrap();
        let caller = DomainId::new(index as u32 + 1);
        let no_args = a == 0 && b == 0 && c == 0;
        let result = match call {
            #[cfg(hardware_boot_disk)]
            x if x == HypercallNumber::FirmwareRealtimeSeconds as u64 && index == 0 && no_args => {
                realtime.as_ref().map_or(HYPERCALL_UNSUPPORTED, qemu_rtc::Realtime::seconds)
            }
            #[cfg(firmware_ramfb)]
            x if x == HypercallNumber::FirmwareFramebufferPresent as u64 && index == 0 => {
                // SAFETY: The only CPU's System vCPU is suspended at this
                // EL1 HVC. Scanout and source own disjoint retained allocations.
                if unsafe { framebuffer.present(&ram[0], a, b, c) } {
                    HYPERCALL_SUCCESS
                } else {
                    HYPERCALL_INVALID_ARGUMENT
                }
            }
            x if x == HypercallNumber::ConsoleWrite as u64 => {
                if let Some(host) = ram[index]
                    .host_address(a, b)
                    .filter(|_| b <= 4096 && c <= u16::MAX as u64)
                {
                    let bytes =
                        unsafe { core::slice::from_raw_parts(host as *const u8, b as usize) };
                    if let Ok(text) = core::str::from_utf8(bytes) {
                        emit(text);
                        HYPERCALL_SUCCESS
                    } else {
                        HYPERCALL_INVALID_ARGUMENT
                    }
                } else {
                    HYPERCALL_INVALID_ARGUMENT
                }
            }
            x if x == HypercallNumber::Ready as u64 && no_args && !ready[index] => {
                ready[index] = true;
                report(format_args!("Domain {} actual Ready\n", caller.get()));
                HYPERCALL_SUCCESS
            }
            x if x == HypercallNumber::Yield as u64 && no_args => HYPERCALL_SUCCESS,
            x if x == HypercallNumber::Wait as u64 && no_args => HYPERCALL_SUCCESS,
            x if x == HypercallNumber::EventSend as u64 && b == 0 && c == 0 => u32::try_from(a)
                .ok()
                .and_then(|port| events.send(caller, port).ok())
                .map_or(HYPERCALL_INVALID_ARGUMENT, |_| HYPERCALL_SUCCESS),
            x if x == HypercallNumber::EventPoll as u64 && no_args => {
                scheduler.resume_after_emulation(index);
                events.receive(caller).map_or(0, u64::from)
            }
            x if x == HypercallNumber::EventWait as u64 && no_args => {
                if let Some(port) = events.receive(caller) {
                    u64::from(port)
                } else {
                    event_wait[index] = true;
                    runnable[index] = false;
                    continue;
                }
            }
            x if x == HypercallNumber::EventIrqEnable as u64 && no_args && !event_irq[index] => {
                event_irq[index] = true;
                HYPERCALL_SUCCESS
            }
            x if x == HypercallNumber::GrantCreate as u64
                || x == HypercallNumber::GrantCreateRange as u64 =>
            {
                let count = if x == HypercallNumber::GrantCreate as u64 {
                    1
                } else {
                    u32::try_from(c >> GRANT_RANGE_PAGE_COUNT_SHIFT).unwrap_or(0)
                };
                let flags = if x == HypercallNumber::GrantCreate as u64 {
                    c
                } else {
                    c & GRANT_RANGE_FLAGS_MASK
                };
                if b != (2 - index) as u64
                    || !in_window(&ram[index], a, count)
                    || flags & !GRANT_FLAG_WRITABLE != 0
                    || grants.target_range_is_mapped(caller, a, count)
                {
                    HYPERCALL_INVALID_ARGUMENT
                } else {
                    let physical = ram[index]
                        .physical_address(a, u64::from(count) * 4096)
                        .unwrap();
                    grants
                        .create_range(
                            caller,
                            DomainId::new(b as u32),
                            physical,
                            count,
                            flags & GRANT_FLAG_WRITABLE != 0,
                        )
                        .map_or(HYPERCALL_INVALID_ARGUMENT, |reference| {
                            u64::from(reference.get())
                        })
                }
            }
            x if x == HypercallNumber::GrantQuery as u64 && b == 0 && c == 0 => usize::try_from(a)
                .ok()
                .and_then(|ordinal| grants.query_target(caller, ordinal))
                .map_or(HYPERCALL_INVALID_ARGUMENT, |reference| {
                    u64::from(reference.get())
                }),
            x if x == HypercallNumber::GrantMap as u64 && c == 0 => {
                let reference = u32::try_from(a).ok().and_then(GrantRef::new);
                let mapping = reference
                    .filter(|&reference| {
                        grants
                            .mapping_page_count(caller, reference)
                            .is_some_and(|count| in_window(&ram[index], b, count))
                    })
                    .and_then(|reference| grants.map(caller, reference, b).ok());
                if let Some(mapping) = mapping {
                    for offset in (0..u64::from(mapping.page_count) * 4096).step_by(4096) {
                        // SAFETY: GrantTable admitted only pinned owner RAM;
                        // target range is its reserved window, never EL2/MMIO.
                        // Both Domains are stopped throughout replacement.
                        unsafe {
                            mapper
                                .remap_page(
                                    b + offset,
                                    mapping.host_page + offset,
                                    Access {
                                        writable: mapping.writable,
                                        executable: false,
                                        memory: MemoryType::Normal,
                                    },
                                )
                                .unwrap();
                        }
                    }
                    shared_maps += 1;
                    report(format_args!(
                        "Grant {} mapped into Domain {} ({} pages)\n",
                        a,
                        caller.get(),
                        mapping.page_count
                    ));
                    HYPERCALL_SUCCESS
                } else {
                    HYPERCALL_INVALID_ARGUMENT
                }
            }
            x if x == HypercallNumber::GrantUnmap as u64 && b == 0 && c == 0 => {
                if let Some(mapping) = u32::try_from(a)
                    .ok()
                    .and_then(GrantRef::new)
                    .and_then(|reference| grants.unmap(caller, reference).ok())
                {
                    for offset in (0..u64::from(mapping.page_count) * 4096).step_by(4096) {
                        unsafe {
                            mapper
                                .remap_page(
                                    mapping.target_page + offset,
                                    ram[index]
                                        .physical_address(mapping.target_page + offset, 4096)
                                        .unwrap(),
                                    READ_WRITE,
                                )
                                .unwrap();
                        }
                    }
                    HYPERCALL_SUCCESS
                } else {
                    HYPERCALL_INVALID_ARGUMENT
                }
            }
            x if x == HypercallNumber::GrantRevoke as u64 && b == 0 && c == 0 => u32::try_from(a)
                .ok()
                .and_then(GrantRef::new)
                .filter(|&reference| grants.revoke(caller, reference).is_ok())
                .map_or(HYPERCALL_INVALID_ARGUMENT, |_| HYPERCALL_SUCCESS),
            // No physical requester has been admitted on this reference VM.
            x if x == HypercallNumber::DeviceQuery as u64 && index == 1 => {
                HYPERCALL_INVALID_ARGUMENT
            }
            x if x == HypercallNumber::Shutdown as u64 => {
                report(format_args!(
                    "Domain {} shutdown reason {}\n",
                    caller.get(),
                    a
                ));
                finish(1);
            }
            0x8400_0000 if index == 1 => 2,
            0x8000_0000 if index == 1 => 0x10001,
            0x8000_0001 if index == 1 => u64::MAX,
            _ => HYPERCALL_UNSUPPORTED,
        };
        guests[index].complete_hypercall(result).unwrap();
    }
    report(format_args!(
        "Paired run limit: Ready={:?}, shared mappings={}\n",
        ready, shared_maps
    ));
    check(false, "OS startup incomplete within paired diagnostic run");
}
