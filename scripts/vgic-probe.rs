//! Real GICH/GICV execution with active IRQs retained across vCPU switches.
//! Board discovery is fixed here only. No physical Distributor is guest-mapped.
use super::*;

const GICV: u64 = 0x40000;
const GICH_HOST: u64 = 0x0803_0000;
const GICV_HOST: u64 = 0x0804_0000;
static mut ROOT_IRQ_A: Root = Root([const { PageTable::zeroed() }; 16]);
static mut ROOT_IRQ_B: Root = Root([const { PageTable::zeroed() }; 16]);
static mut IRQ_TABLES: [PageTable; 32] = [const { PageTable::zeroed() }; 32];
static mut STACK_IRQ_A: Page = Page([0; 512]);
static mut STACK_IRQ_B: Page = Page([0; 512]);

global_asm!(
    r#"
    .section .guest.text, "ax"
    .balign 2048
guest_irq_vectors:
    .rept 5
    b guest_irq_bad
    .balign 128
    .endr
    b guest_irq_received
    .balign 128
    .rept 10
    b guest_irq_bad
    .balign 128
    .endr
    .global guest_irq_entry
guest_irq_entry:
    ldr x1, =0x21000
    mov sp, x1
    adr x1, guest_irq_vectors
    msr vbar_el1, x1
    mov x19, #0x40000
    mov w1, #1
    str w1, [x19]
    mov w1, #0xff
    str w1, [x19, #4]
    str wzr, [x19, #8]
    dsb sy
    msr daifclr, #2
1:  b 1b
guest_irq_received:
    ldr w20, [x19, #12]
    mov x0, x20
    // Leave this virtual IRQ active while the host runs another Domain.
    hvc #0
    str w20, [x19, #16]
    dsb sy
    mov x0, #0xeeee
    hvc #0
    eret
guest_irq_bad:
    mov x0, #0xbad
    hvc #0
    b .
    .section .text
"#
);
unsafe extern "C" {
    fn guest_irq_entry();
}

fn guest_address(entry: unsafe extern "C" fn()) -> u64 {
    CODE + entry as *const () as u64 - addr_of!(__guest_start) as u64
}

fn active(guest: &vcpu::Vcpu, id: u16) {
    check(
        guest.interrupt_status(id)
            == Some(vgic::InterruptStatus {
                pending: false,
                active: true,
            }),
        "virtual active interrupt lost",
    );
}

pub fn run(config: Config) {
    emit("Native virtual GIC entered\n");
    let roots = [addr_of!(ROOT_IRQ_A) as u64, addr_of!(ROOT_IRQ_B) as u64];
    let stacks = [addr_of!(STACK_IRQ_A) as u64, addr_of!(STACK_IRQ_B) as u64];
    let mut pool = Pool {
        base: addr_of!(IRQ_TABLES) as u64,
        roots,
        used: [false; 32],
        budget: usize::MAX,
    };
    let mut guests = [None, None];
    // Admission is checked before touching the fixture's physical GICH bank.
    for i in 0..2 {
        let mut mapper = unsafe { Mapper::new(&mut pool, roots[i], config, 6 + i as u8).unwrap() };
        select(&mapper);
        let candidate = vcpu::Vcpu::new(&mapper, guest_address(guest_irq_entry), 0, i as u64);
        if matches!(candidate, Err(vcpu::Error::UnsupportedCpu)) {
            emit("Native virtual GIC execution requires a supported vCPU register bank\n");
            return;
        }
        let guest = candidate.unwrap();
        map_fixture_code(&mut mapper);
        unsafe {
            mapper.map_page(STACK, stacks[i], READ_WRITE).unwrap();
            for offset in [0, 4096] {
                mapper
                    .map_page(
                        GICV + offset,
                        GICV_HOST + offset,
                        Access {
                            writable: true,
                            executable: false,
                            memory: MemoryType::Device,
                        },
                    )
                    .unwrap();
            }
        }
        guests[i] = Some(guest);
    }
    // SAFETY: Fixed fixture owns this CPU's GICH; EL2-only and Device memory.
    let mut controller = unsafe { vgic::Controller::new(GICH_HOST, 4096).expect("GICH admission") };
    // BPR/ABPR have architecturally required implementation minima; writing
    // zero does not imply that these two fields read back as zero.
    let reset_vmcr = unsafe { read_volatile((GICH_HOST + 8) as *const u32) };
    for i in 0..2 {
        let guest = guests[i].as_mut().unwrap();
        check(
            guest.inject_edge(48, 0x40, None).is_err(),
            "unconfigured virtual IRQ accepted",
        );
        guest.configure_interrupts(&controller).unwrap();
        check(
            guest.inject_edge(1020, 0x40, None).is_err(),
            "spurious virtual ID accepted",
        );
        check(
            guest.inject_edge(48, 0x41, None).is_err(),
            "invalid priority accepted",
        );
        check(
            guest.inject_edge(48, 0x40, Some(1)).is_err(),
            "non-SGI source accepted",
        );
        check(
            guest.inject_edge(3, 0x40, None).is_err(),
            "missing SGI source accepted",
        );
        guest
            .inject_edge(48 + i as u16, (i as u8 + 1) * 0x40, None)
            .unwrap();
        guest
            .inject_edge(48 + i as u16, (i as u8 + 1) * 0x40, None)
            .unwrap(); // Coalesced edge.
    }
    for phase in 0..2 {
        for i in 0..2 {
            let mapper = unsafe { Mapper::new(&mut pool, roots[i], config, 6 + i as u8).unwrap() };
            select(&mapper);
            let guest = guests[i].as_mut().unwrap();
            check(
                matches!(
                    unsafe { guest.run(&mapper) },
                    Err(vcpu::Error::InvalidState)
                ),
                "configured vCPU ran without GIC state",
            );
            if phase == 1 {
                guest.complete_hypercall(0).unwrap();
            }
            // SAFETY: Both controllers, all pinned pages and Stage-2 contexts
            // are exclusively owned by this one CPU. No physical IRQs or DMA.
            let exit = unsafe {
                guest
                    .run_with_gic(&mapper, &mut controller)
                    .expect("virtual IRQ entry")
            };
            check(
                exit.kind == vcpu::ExitKind::Synchronous && exit.esr >> 26 == 0x16,
                "virtual IRQ did not reach guest handler",
            );
            check(
                guest.register(0) == Some(if phase == 0 { 48 + i as u64 } else { 0xeeee }),
                "wrong virtual IRQ or EOI continuation",
            );
            if phase == 0 {
                active(guest, 48 + i as u16);
            } else {
                check(
                    guest.interrupt_status(48 + i as u16).is_none(),
                    "EOI failed to deactivate virtual IRQ",
                );
            }
            // Saved state must leave no active/pending/priority residue in the
            // physical GICH bank before another Domain is selected.
            unsafe {
                check(
                    read_volatile(GICH_HOST as *const u32) == 0,
                    "GICH left enabled",
                );
                check(
                    read_volatile((GICH_HOST + 0xf0) as *const u32) == 0,
                    "active priority leaked",
                );
                check(
                    read_volatile((GICH_HOST + 8) as *const u32) == reset_vmcr,
                    "VMCR leaked",
                );
                let count = (read_volatile((GICH_HOST + 4) as *const u32) & 63) + 1;
                for slot in 0..count {
                    check(
                        read_volatile((GICH_HOST + 0x100 + u64::from(slot) * 4) as *const u32) == 0,
                        "virtual list register leaked",
                    );
                }
            }
        }
    }
    let mapper = unsafe { Mapper::new(&mut pool, roots[0], config, 6).unwrap() };
    select(&mapper);
    let guest = guests[0].as_mut().unwrap();
    // Pending SGI source is retained in the complete IAR/EOIR token.
    guest.inject_edge(3, 0x40, Some(5)).unwrap();
    check(
        guest.inject_edge(3, 0x40, Some(4))
            == Err(vcpu::Error::Interrupts(vgic::Error::ConflictingSource)),
        "conflicting SGI source silently discarded",
    );
    guest.complete_hypercall(0).unwrap();
    unsafe {
        guest.run_with_gic(&mapper, &mut controller).unwrap();
    }
    check(
        guest.register(0) == Some(3 | (5 << 10)),
        "SGI source token lost",
    );
    active(guest, 3);
    guest.complete_hypercall(0).unwrap();
    unsafe {
        guest.run_with_gic(&mapper, &mut controller).unwrap();
    }
    check(
        guest.register(0) == Some(0xeeee) && guest.interrupt_status(3).is_none(),
        "SGI EOI token lost",
    );
    // The guest's map must not expose the physical Distributor or GICH.
    for private in [0x0800_0000, GICH_HOST] {
        let mut denied = vcpu::Vcpu::new(&mapper, guest_address(guest_read), private, 0).unwrap();
        let exit = unsafe { denied.run(&mapper).unwrap() };
        abort(
            Exit {
                esr: exit.esr,
                far: exit.far,
                hpfar: exit.hpfar,
                value: denied.register(0).unwrap(),
            },
            private,
            false,
            false,
            false,
        );
    }
    let mut full = vcpu::Vcpu::new(&mapper, guest_address(guest_irq_entry), 0, 0).unwrap();
    full.configure_interrupts(&controller).unwrap();
    let count = unsafe { read_volatile((GICH_HOST + 4) as *const u32) & 63 } as u16 + 1;
    for id in 64..64 + count {
        full.inject_edge(id, 0x40, None).unwrap();
    }
    check(
        full.inject_edge(64 + count, 0x40, None)
            == Err(vcpu::Error::Interrupts(vgic::Error::QueueFull)),
        "full IRQ queue silently discarded an event",
    );
    unsafe {
        full.run_with_gic(&mapper, &mut controller).unwrap();
    }
    check(
        matches!(full.register(0), Some(id) if id >= 64 && id < u64::from(64+count)),
        "filled virtual IRQ queue did not deliver",
    );
    check(
        full.interrupt_status(64 + count).is_none(),
        "failed injection mutated the queue",
    );
    emit("Native virtual GIC active/pending/priority/SGI isolation and EOI PASS\n");
}
