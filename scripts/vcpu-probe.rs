//! Execute the production world switch, including repeated exits/resumes and
//! two distinct Stage-2 contexts. This is not the production Domain dispatcher.
use super::*;
use vcpu::{Error as VcpuError, ExitKind, Vcpu};

static mut ROOT_VCPU_A: Root = Root([const { PageTable::zeroed() }; 16]);
static mut ROOT_VCPU_B: Root = Root([const { PageTable::zeroed() }; 16]);
static mut VCPU_TABLES: [PageTable; 32] = [const { PageTable::zeroed() }; 32];
static mut STACK_A: Page = Page([0; 512]);
static mut STACK_B: Page = Page([0; 512]);

// Each guest establishes distinct values, then makes three consecutive HVCs.
// No reload from guest memory can hide lost live register state on re-entry.
global_asm!(
    r#"
    .section .guest.text, "ax"
    .global guest_vcpu
guest_vcpu:
    mov x1, #(3 << 20)
    msr cpacr_el1, x1
    isb
    lsl x1, x0, #14
    msr fpcr, x1
    mov x1, #8
    msr fpsr, x1
    lsl x1, x0, #20
    msr nzcv, x1
    add x1, x0, #0x40
    msr tpidr_el0, x1
    add x1, x0, #0x41
    msr tpidrro_el0, x1
    add x1, x0, #0x42
    msr tpidr_el1, x1
    lsl x1, x0, #4
    msr sp_el0, x1
    msr par_el1, x1
    lsl x1, x0, #12
    msr ttbr0_el1, x1
    lsl x1, x0, #16
    msr ttbr1_el1, x1
    mov x1, #0xff
    msr mair_el1, x1
    ldr x1, =0xb5103510
    msr tcr_el1, x1
    ldr x1, =0x21000
    mov sp, x1
    msr contextidr_el1, x0
    mov x1, #0x80
    msr spsr_el1, x1
    lsl x1, x0, #8
    msr elr_el1, x1
    msr cntv_cval_el0, x1
    mov x1, #2
    msr cntv_ctl_el0, x1
    isb
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    dup v\n\().2d, x0
    .endr
    .irp n, 1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30
    add x\n, x0, #\n
    .endr
    hvc #0
    hvc #0
    mrs x0, tpidr_el0
    mrs x1, tpidrro_el0
    mrs x2, tpidr_el1
    mrs x3, sp_el0
    mov x4, sp
    mrs x5, ttbr0_el1
    mrs x6, ttbr1_el1
    mrs x7, mair_el1
    mrs x8, tcr_el1
    mrs x9, contextidr_el1
    mrs x10, elr_el1
    mrs x11, spsr_el1
    mrs x12, cntv_cval_el0
    mrs x13, cntv_ctl_el0
    mrs x14, mpidr_el1
    mrs x15, par_el1
    mrs x16, fpcr
    mrs x17, fpsr
    hvc #0
    b .
    .global guest_vcpu_wfi
guest_vcpu_wfi:
    wfi
    hvc #0
    .global guest_vcpu_smc
guest_vcpu_smc:
    smc #0
    hvc #0
    .global guest_vcpu_debug
guest_vcpu_debug:
    mrs x0, mdscr_el1
    hvc #0
    .global guest_vcpu_id
guest_vcpu_id:
    mrs x0, id_aa64pfr0_el1
    hvc #0
    .section .text
"#
);
unsafe extern "C" {
    fn guest_vcpu();
    fn guest_vcpu_wfi();
    fn guest_vcpu_smc();
    fn guest_vcpu_debug();
    fn guest_vcpu_id();
}

fn guest_address(entry: unsafe extern "C" fn()) -> u64 {
    CODE + entry as *const () as u64 - addr_of!(__guest_start) as u64
}

pub fn run(config: Config) {
    emit("Native vCPU continuation entered\n");
    let roots = [addr_of!(ROOT_VCPU_A) as u64, addr_of!(ROOT_VCPU_B) as u64];
    let stacks = [addr_of!(STACK_A) as u64, addr_of!(STACK_B) as u64];
    let mut pool = Pool {
        base: addr_of!(VCPU_TABLES) as u64,
        roots,
        used: [false; 32],
        budget: usize::MAX,
    };
    let mut guests = [None, None];
    for i in 0..2 {
        // SAFETY: Distinct owned table roots, private stacks, unique VMIDs.
        let mut mapper = unsafe { Mapper::new(&mut pool, roots[i], config, 4 + i as u8).unwrap() };
        select(&mapper);
        let candidate = Vcpu::new(
            &mapper,
            guest_address(guest_vcpu),
            (i as u64 + 1) << 8,
            i as u64,
        );
        if matches!(candidate, Err(VcpuError::UnsupportedCpu)) {
            emit("Native vCPU extended register bank rejected (not yet supported)\n");
            return;
        }
        // SAFETY: Read-only executable fixture code is explicitly shared;
        // stacks are private and no EL2 table/code/data beyond that page maps.
        unsafe {
            map_fixture_code(&mut mapper);
            mapper.map_page(STACK, stacks[i], READ_WRITE).unwrap();
        }
        guests[i] = Some(candidate.expect("baseline CPU admission"));
    }
    for phase in 0..3 {
        for i in 0..2 {
            let seed = (i as u64 + 1) << 8;
            let guest = guests[i].as_mut().unwrap();
            // SAFETY: Reattach the retained root/VMID; no other owner or CPU.
            let mapper = unsafe { Mapper::new(&mut pool, roots[i], config, 4 + i as u8).unwrap() };
            select(&mapper);
            if phase == 1 {
                check(guest.complete_hypercall(0x8877).is_ok(), "HVC completion");
                check(
                    guest.complete_hypercall(0x99).is_err(),
                    "HVC completed twice",
                );
            } else if phase == 2 {
                guest.complete_hypercall(0x9988).unwrap();
            }
            // SAFETY: This fixture owns execution, all backing pages, and no
            // virtual interrupt engine or physical device is guest-accessible.
            let exit = unsafe { guest.run(&mapper).expect("resume native vCPU") };
            check(
                exit.kind == ExitKind::Synchronous && exit.esr >> 26 == 0x16,
                "native vCPU HVC exit",
            );
            check(guest.pstate() & 0xf000_0000 == seed << 20, "NZCV retention");
            for n in 0..32 {
                check(
                    guest.simd_register(n) == Some(u128::from(seed) << 64 | u128::from(seed)),
                    "SIMD continuation leaked or lost",
                );
            }
            if phase < 2 {
                for n in 0..31 {
                    let value = if n == 0 && phase == 1 {
                        0x8877
                    } else {
                        seed + n as u64
                    };
                    check(
                        guest.register(n) == Some(value),
                        "GPR continuation leaked or lost",
                    );
                }
            } else {
                let expected = [
                    seed + 0x40,
                    seed + 0x41,
                    seed + 0x42,
                    seed << 4,
                    STACK + PAGE_SIZE,
                    seed << 12,
                    seed << 16,
                    0xff,
                    0xb510_3510,
                    seed,
                    seed << 8,
                    0x80,
                    seed << 8,
                    2,
                    (1 << 31) | i as u64,
                    seed << 4,
                    seed << 14,
                    8,
                ];
                for (n, value) in expected.into_iter().enumerate() {
                    check(
                        guest.register(n) == Some(value),
                        "EL1/TLS/timer continuation lost",
                    );
                }
            }
        }
    }
    // Wrong or inactive roots cannot resume an otherwise valid continuation.
    let a = unsafe { Mapper::new(&mut pool, roots[0], config, 4).unwrap() };
    select(&a);
    check(
        matches!(
            unsafe { guests[1].as_mut().unwrap().run(&a) },
            Err(VcpuError::WrongAddressSpace)
        ),
        "foreign Stage-2 continuation accepted",
    );
    let b = unsafe { Mapper::new(&mut pool, roots[1], config, 5).unwrap() };
    check(
        matches!(
            unsafe { guests[1].as_mut().unwrap().run(&b) },
            Err(VcpuError::Stage2(Stage2Error::InactiveAddressSpace))
        ),
        "inactive continuation accepted",
    );
    let mapper = unsafe { Mapper::new(&mut pool, roots[0], config, 4).unwrap() };
    select(&mapper);
    let mut denied = Vcpu::new(&mapper, guest_address(guest_read), roots[0], 0).unwrap();
    for _ in 0..2 {
        let exit = unsafe { denied.run(&mapper).unwrap() };
        check(
            exit.kind == ExitKind::Synchronous,
            "native Stage-2 fault kind",
        );
        abort(
            Exit {
                esr: exit.esr,
                far: exit.far,
                hpfar: exit.hpfar,
                value: denied.register(0).unwrap(),
            },
            roots[0],
            false,
            false,
            false,
        );
        check(
            denied.complete_hypercall(0).is_err(),
            "Stage-2 fault completed as HVC",
        );
    }
    for (entry, ec) in [
        (guest_vcpu_wfi as unsafe extern "C" fn(), 1),
        (guest_vcpu_smc, 0x17),
        (guest_vcpu_debug, 0x18),
        (guest_vcpu_id, 0x18),
    ] {
        let mapper = unsafe { Mapper::new(&mut pool, roots[0], config, 4).unwrap() };
        select(&mapper);
        let mut guest = Vcpu::new(&mapper, guest_address(entry), 0, 0).unwrap();
        let exit = unsafe { guest.run(&mapper).unwrap() };
        check(
            exit.kind == ExitKind::Synchronous && exit.esr >> 26 == ec,
            "unhandled control escaped EL2 trap",
        );
        check(
            guest.complete_hypercall(0).is_err(),
            "non-HVC completion accepted",
        );
        if entry as *const () == guest_vcpu_id as *const () {
            check(
                guest.system_register_read()
                    == Some(vcpu::SystemRegister {
                        op0: 3,
                        op1: 0,
                        crn: 0,
                        crm: 4,
                        op2: 0,
                    }),
                "MRS operand decoding",
            );
            guest.complete_system_register_read(0x1234).unwrap();
            check(
                guest.complete_system_register_read(0).is_err(),
                "MRS completed twice",
            );
            let exit = unsafe { guest.run(&mapper).unwrap() };
            check(
                exit.esr >> 26 == 0x16 && guest.register(0) == Some(0x1234),
                "MRS resumed at wrong instruction",
            );
        }
    }
    emit("Native vCPU GPR/SIMD/TLS/EL1/timer isolation and resume PASS\n");
}
