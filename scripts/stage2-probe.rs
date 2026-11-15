//! EL1 accesses and retained vCPU/IRQ state through production EL2 mechanisms.
//! Fixed QEMU board setup is a fixture, not a replacement VMM or OS boot.
#![no_std]
#![no_main]

#[cfg(not(feature = "arm-diagnostics"))]
#[path = "../src/arch/aarch64/stage2.rs"]
mod stage2;
#[cfg(not(feature = "arm-diagnostics"))]
#[path = "../src/arch/aarch64/vcpu.rs"]
mod vcpu;
#[cfg(not(feature = "arm-diagnostics"))]
#[path = "../src/arch/aarch64/vgic.rs"]
mod vgic;
#[cfg(feature = "arm-diagnostics")]
use mboot::arch::aarch64::{firmware, stage2, vcpu, vgic};
#[cfg(not(feature = "arm-diagnostics"))]
#[path = "../src/arch/aarch64/firmware.rs"]
mod firmware;
#[path = "vcpu-probe.rs"]
mod vcpu_probe;
#[path = "vgic-probe.rs"]
mod vgic_probe;

#[cfg(all(system_domain_probe, not(feature = "arm-diagnostics")))]
#[path = "../src/error.rs"]
mod native_error;
#[cfg(all(system_domain_probe, not(feature = "arm-diagnostics")))]
pub use native_error::Error;
#[cfg(all(system_domain_probe, not(feature = "arm-diagnostics")))]
#[path = "../src/image.rs"]
mod image;
#[cfg(all(system_domain_probe, feature = "arm-diagnostics"))]
use mboot::image;
#[cfg(system_domain_probe)]
#[path = "system-domain-probe.rs"]
mod system_domain_probe;

use core::arch::{asm, global_asm};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use stage2::{Access, Config, Error as Stage2Error, Mapper, MemoryType, PageTable, TableMemory, PAGE_SIZE};

const CODE: u64 = 0x10000;
const STACK: u64 = 0x20000;
const DATA: u64 = 0x30000;
const READ_ONLY: Access = Access {
    writable: false,
    executable: false,
    memory: MemoryType::Normal,
};
const READ_WRITE: Access = Access {
    writable: true,
    ..READ_ONLY
};
const READ_EXECUTE: Access = Access {
    executable: true,
    ..READ_ONLY
};

global_asm!(
    r#"
    .section .text.entry, "ax"
    .global _start
_start:
    msr daifset, #15
    ldr x0, =__stack_top
    mov sp, x0
    ldr x1, =__bss_start
    ldr x2, =__bss_end
1:  cmp x1, x2
    b.hs 2f
    str xzr, [x1], #8
    b 1b
2:  mrs x0, CurrentEL
    cmp x0, #8
    b.ne unexpected
    adr x0, vectors
    msr vbar_el2, x0
    mov x0, #(1 << 31)
    msr hcr_el2, x0
    // Baseline RES1, SVE trapped, FP available. SME changes bit 12 from
    // RES1 to TSM: clear it only when the actual CPU implements SME.
    mov x0, #0x33ff
    mrs x1, S3_0_C0_C4_1
    ubfx x1, x1, #24, #4
    cbz x1, 3f
    bic x0, x0, #0x1000
3:  msr cptr_el2, x0
    isb
    bl probe_main
    b .

    .balign 2048
vectors:
    .rept 8
    b unexpected
    .balign 128
    .endr
    b guest_exit
    .balign 128
    .rept 7
    b unexpected
    .balign 128
    .endr

    // Only a single EL1 instruction sequence is run. No guest resumes after
    // an exit, so there is no duplicated vCPU save/restore implementation.
guest_exit:
    adrp x9, LAST_EXIT
    add x9, x9, :lo12:LAST_EXIT
    str x0, [x9, #24]
    mrs x10, esr_el2
    str x10, [x9]
    mrs x10, far_el2
    str x10, [x9, #8]
    mrs x10, hpfar_el2
    str x10, [x9, #16]
    ldp x19, x20, [sp]
    ldp x21, x22, [sp, #16]
    ldp x23, x24, [sp, #32]
    ldp x25, x26, [sp, #48]
    ldp x27, x28, [sp, #64]
    ldp x29, x30, [sp, #80]
    ldr x9, [sp, #608]
    msr fpcr, x9
    ldr x9, [sp, #616]
    msr fpsr, x9
    ldp q0, q1, [sp, #96]
    ldp q2, q3, [sp, #128]
    ldp q4, q5, [sp, #160]
    ldp q6, q7, [sp, #192]
    ldp q8, q9, [sp, #224]
    ldp q10, q11, [sp, #256]
    ldp q12, q13, [sp, #288]
    ldp q14, q15, [sp, #320]
    ldp q16, q17, [sp, #352]
    ldp q18, q19, [sp, #384]
    ldp q20, q21, [sp, #416]
    ldp q22, q23, [sp, #448]
    ldp q24, q25, [sp, #480]
    ldp q26, q27, [sp, #512]
    ldp q28, q29, [sp, #544]
    ldp q30, q31, [sp, #576]
    add sp, sp, #624
    ret

    .global enter_guest
enter_guest:
    sub sp, sp, #624
    stp x19, x20, [sp]
    stp x21, x22, [sp, #16]
    stp x23, x24, [sp, #32]
    stp x25, x26, [sp, #48]
    stp x27, x28, [sp, #64]
    stp x29, x30, [sp, #80]
    stp q0, q1, [sp, #96]
    stp q2, q3, [sp, #128]
    stp q4, q5, [sp, #160]
    stp q6, q7, [sp, #192]
    stp q8, q9, [sp, #224]
    stp q10, q11, [sp, #256]
    stp q12, q13, [sp, #288]
    stp q14, q15, [sp, #320]
    stp q16, q17, [sp, #352]
    stp q18, q19, [sp, #384]
    stp q20, q21, [sp, #416]
    stp q22, q23, [sp, #448]
    stp q24, q25, [sp, #480]
    stp q26, q27, [sp, #512]
    stp q28, q29, [sp, #544]
    stp q30, q31, [sp, #576]
    mrs x9, fpcr
    str x9, [sp, #608]
    mrs x9, fpsr
    str x9, [sp, #616]
    movi v0.16b, #0
    movi v1.16b, #0
    movi v2.16b, #0
    movi v3.16b, #0
    movi v4.16b, #0
    movi v5.16b, #0
    movi v6.16b, #0
    movi v7.16b, #0
    movi v8.16b, #0
    movi v9.16b, #0
    movi v10.16b, #0
    movi v11.16b, #0
    movi v12.16b, #0
    movi v13.16b, #0
    movi v14.16b, #0
    movi v15.16b, #0
    movi v16.16b, #0
    movi v17.16b, #0
    movi v18.16b, #0
    movi v19.16b, #0
    movi v20.16b, #0
    movi v21.16b, #0
    movi v22.16b, #0
    movi v23.16b, #0
    movi v24.16b, #0
    movi v25.16b, #0
    movi v26.16b, #0
    movi v27.16b, #0
    movi v28.16b, #0
    movi v29.16b, #0
    movi v30.16b, #0
    movi v31.16b, #0
    msr fpcr, xzr
    msr fpsr, xzr
    msr elr_el2, x0
    msr sp_el1, x1
    mov x0, #0x3c5
    msr spsr_el2, x0
    ldr x0, =0x30d00800
    msr sctlr_el1, x0
    // Start with FP disabled and zeroed. A native bootstrap can enable
    // its own base FP/SIMD without reading or corrupting EL2 host FP state.
    msr cpacr_el1, xzr
    msr ttbr0_el1, xzr
    msr ttbr1_el1, xzr
    msr tpidr_el0, xzr
    msr tpidrro_el0, xzr
    msr tpidr_el1, xzr
    msr vbar_el1, xzr
    mov x0, x2
    mov x1, xzr
    mov x2, xzr
    mov x3, xzr
    mov x4, xzr
    mov x5, xzr
    mov x6, xzr
    mov x7, xzr
    mov x8, xzr
    mov x9, xzr
    mov x10, xzr
    mov x11, xzr
    mov x12, xzr
    mov x13, xzr
    mov x14, xzr
    mov x15, xzr
    mov x16, xzr
    mov x17, xzr
    mov x18, xzr
    mov x19, xzr
    mov x20, xzr
    mov x21, xzr
    mov x22, xzr
    mov x23, xzr
    mov x24, xzr
    mov x25, xzr
    mov x26, xzr
    mov x27, xzr
    mov x28, xzr
    mov x29, xzr
    mov x30, xzr
    isb
    eret

    .section .guest.text, "ax"
    .global guest_read
guest_read:
    ldr x0, [x0]
    hvc #0
    .global guest_write
guest_write:
    mov x1, #0x1234
    str x1, [x0]
    hvc #0
    .global guest_execute
guest_execute:
    mov x0, #0x5678
    hvc #0
    .section .text
"#
);

#[repr(C, align(65536))]
struct Root([PageTable; 16]);
static mut ROOT_A: Root = Root([const { PageTable::zeroed() }; 16]);
static mut ROOT_B: Root = Root([const { PageTable::zeroed() }; 16]);
static mut TABLES: [PageTable; 32] = [const { PageTable::zeroed() }; 32];
#[repr(C, align(4096))]
struct Page([u64; 512]);
static mut PAGE_A: Page = Page([0; 512]);
static mut PAGE_B: Page = Page([0; 512]);
static mut GUEST_STACK: Page = Page([0; 512]);

#[derive(Clone, Copy)]
#[repr(C)]
struct Exit {
    esr: u64,
    far: u64,
    hpfar: u64,
    value: u64,
}
#[unsafe(no_mangle)]
static mut LAST_EXIT: Exit = Exit {
    esr: 0,
    far: 0,
    hpfar: 0,
    value: 0,
};

struct Pool {
    base: u64,
    roots: [u64; 2],
    used: [bool; 32],
    budget: usize,
}
impl Pool {
    fn live(&self) -> usize {
        self.used.iter().filter(|&&v| v).count()
    }
}
// SAFETY: Private statically pinned page-aligned storage; live bits exclusively
// track unique allocations. Roots and pool are disjoint and never guest-mapped.
unsafe impl TableMemory for Pool {
    fn allocate_table(&mut self) -> Result<u64, Stage2Error> {
        if self.budget == 0 {
            return Err(Stage2Error::OutOfMemory);
        }
        let index = self
            .used
            .iter()
            .position(|&v| !v)
            .ok_or(Stage2Error::OutOfMemory)?;
        self.budget -= 1;
        self.used[index] = true;
        let ptr = (self.base as *mut PageTable).wrapping_add(index);
        // SAFETY: This slot is exclusively owned and has no published parent.
        unsafe {
            ptr.write(PageTable::zeroed());
        }
        Ok(ptr as u64)
    }
    unsafe fn free_table(&mut self, frame: u64) {
        let index = ((frame - self.base) / PAGE_SIZE) as usize;
        check(index < 32 && self.used[index], "invalid table free");
        self.used[index] = false;
    }
    unsafe fn table_ptr(&self, frame: u64) -> *mut PageTable {
        let in_root = self.roots.iter()
            .any(|&base| (base..base + 65536).contains(&frame));
        let base = self.base;
        let in_pool = (base..base + 32 * PAGE_SIZE).contains(&frame)
            && self.used[((frame - base) / PAGE_SIZE) as usize];
        check(
            frame % PAGE_SIZE == 0 && (in_root || in_pool),
            "unowned table",
        );
        frame as *mut PageTable
    }
}

unsafe extern "C" {
    fn enter_guest(entry: u64, stack: u64, argument: u64);
    fn guest_read();
    fn guest_write();
    fn guest_execute();
    static __guest_start: u8;
    static __guest_end: u8;
}

fn emit(message: &str) {
    for byte in message.bytes() {
        // SAFETY: Fixed QEMU fixture PL011, EL2-only and absent from the guest.
        unsafe {
            while read_volatile(0x0900_0018 as *const u32) & 32 != 0 {
                core::hint::spin_loop();
            }
            write_volatile(0x0900_0000 as *mut u32, u32::from(byte));
        }
    }
}
fn finish(code: u64) -> ! {
    let args = [0x20026u64, code];
    // SAFETY: Runner enables semihosting for this CPU fixture only.
    unsafe {
        asm!("hlt #0xf000", in("x0") 0x20u64, in("x1") args.as_ptr(), options(nostack));
    }
    loop {
        core::hint::spin_loop();
    }
}
fn check(ok: bool, message: &str) {
    if !ok {
        emit("STAGE2 FAIL: ");
        emit(message);
        emit("\n");
        finish(1);
    }
}
#[unsafe(no_mangle)]
extern "C" fn unexpected() -> ! {
    emit("STAGE2 FAIL: unexpected EL2 exception\n");
    finish(1)
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unexpected()
}

fn select(mapper: &Mapper<'_, Pool>) {
    // SAFETY: Single CPU, no guest running; stop Stage-1 speculative walks
    // with valid 4K controls before the production VMID switch.
    unsafe {
        let tcr = (2u64 << 30)
            | (3 << 28)
            | (1 << 26)
            | (1 << 24)
            | (1 << 23)
            | (16 << 16)
            | (3 << 12)
            | (1 << 10)
            | (1 << 8)
            | (1 << 7)
            | 16;
        asm!("msr tcr_el1, {tcr}", "isb", "msr sctlr_el1, {sctlr}", "isb",
            tcr = in(reg) tcr, sctlr = in(reg) 0x30d00801u64, options(nostack));
        mapper.activate().expect("activate");
    }
}
fn run(entry: u64, arg: u64) -> Exit {
    // SAFETY: Dedicated mapped guest code/stack; one exit returns to EL2 and
    // discards guest registers. No untrusted code or device passthrough.
    unsafe {
        let offset = entry - addr_of!(__guest_start) as u64;
        enter_guest(CODE + offset, STACK + PAGE_SIZE, arg);
        read_volatile(addr_of!(LAST_EXIT))
    }
}
fn read(arg: u64, expected: u64) {
    let result = run(guest_read as *const () as u64, arg);
    check(
        result.esr >> 26 == 0x16 && result.value == expected,
        "guest read/HVC",
    );
}
fn abort(exit: Exit, ipa: u64, execute: bool, write: bool, permission: bool) {
    check(
        exit.esr >> 26 == if execute { 0x20 } else { 0x24 },
        "wrong abort class",
    );
    check(
        exit.esr & 0x3c == if permission { 0x0c } else { 0x04 },
        "wrong fault status",
    );
    check(exit.far == ipa, "wrong fault address");
    let fault_ipa = ((exit.hpfar & 0x0000_00ff_ffff_fff0) << 8) | (exit.far & 4095);
    check(fault_ipa == ipa, "wrong Stage-2 IPA");
    if !execute {
        check((exit.esr & (1 << 6) != 0) == write, "wrong abort direction");
    }
}
fn map_basics(mapper: &mut Mapper<'_, Pool>, data: u64) {
    // SAFETY: Only dedicated guest pages; hypervisor pages remain unmapped.
    unsafe {
        map_fixture_code(mapper);
        mapper
            .map_page(STACK, addr_of!(GUEST_STACK) as u64, READ_WRITE)
            .expect("stack map");
        mapper.map_page(DATA, data, READ_WRITE).expect("data map");
    }
}

fn map_fixture_code(mapper: &mut Mapper<'_, Pool>) {
    let start = addr_of!(__guest_start) as u64;
    for offset in (0..addr_of!(__guest_end) as u64 - start).step_by(4096) {
        // SAFETY: Explicitly shared, immutable code-only fixture pages. The
        // linker separates this region from all EL2 text, rodata and state.
        unsafe { mapper.map_page(CODE + offset, start + offset, READ_EXECUTE).expect("fixture code"); }
    }
}

#[unsafe(no_mangle)]
extern "C" fn probe_main() -> ! {
    emit("STAGE2 EL2 started\n");
    #[cfg(feature = "arm-diagnostics")]
    emit("Linked production mBoot library\n");
    // SAFETY: This fixed virt-8.2 virtualization fixture provides the standard
    // SMC PSCI conduit. This is not product platform/FDT admission code.
    let firmware = unsafe { firmware::Firmware::from_smc_conduit().expect("SMC PSCI conduit") };
    check(firmware.psci_version() == 0x0001_0001, "real PSCI version");
    for workaround in [firmware::Workaround::BranchPredictor, firmware::Workaround::BranchHistory] {
        check(firmware.architectural_features(workaround) == Ok(-1),
            "unsupported firmware workaround was fabricated");
        check(firmware.apply(workaround) == Err(firmware::Error::UnsupportedWorkaround),
            "unavailable firmware workaround accepted");
    }
    emit("Real PSCI / unavailable SMCCC workaround propagation PASS\n");
    let native = Config::detect().expect("CPU features");
    // Exercise concatenation even when this CPU exposes a wider PA range.
    let features: u64;
    unsafe {
        asm!("mrs {}, id_aa64mmfr0_el1", out(reg) features, options(nomem, nostack));
    }
    let config = Config::from_features(features, Some(40)).expect("40-bit IPA");
    check(
        native.physical_bits() >= 40 && config.ipa_bits() == 40 && config.root_pages() == 2,
        "Cortex-A57 concatenated root geometry",
    );
    check(
        (addr_of!(__guest_end) as u64 - addr_of!(__guest_start) as u64) < STACK - CODE,
        "guest code overlaps stack",
    );
    // SAFETY: Fixture-private storage; executable data is initialized before
    // any guest entry and instruction-cache coherence is completed explicitly.
    unsafe {
        write_volatile(addr_of_mut!(PAGE_A).cast::<u64>(), 0xa11);
        write_volatile(addr_of_mut!(PAGE_B).cast::<u64>(), 0xb22);
        asm!("dsb ish", "ic iallu", "dsb ish", "isb", options(nostack));
    }
    let mut pool = Pool {
        base: addr_of!(TABLES) as u64,
        roots: [addr_of!(ROOT_A) as u64, addr_of!(ROOT_B) as u64],
        used: [false; 32],
        budget: usize::MAX,
    };
    let root_a = addr_of!(ROOT_A) as u64;
    let root_b = addr_of!(ROOT_B) as u64;
    // SAFETY: Two disjoint private root blocks and exclusive VMID reservations.
    let mut a = unsafe { Mapper::new(&mut pool, root_a, config, 1).expect("root A") };
    // SAFETY: Failed edits must leave inactive roots unchanged.
    check(
        unsafe { a.map_page(DATA, addr_of!(PAGE_A) as u64, READ_WRITE) }
            == Err(Stage2Error::InactiveAddressSpace),
        "inactive edit accepted",
    );
    select(&a);
    map_basics(&mut a, addr_of!(PAGE_A) as u64);
    read(DATA, 0xa11);
    // SAFETY: Same quiescent, selected and exclusively owned address space.
    unsafe {
        a.map_page(1 << 39, addr_of!(PAGE_B) as u64, READ_ONLY)
            .expect("high IPA");
    }
    read(1 << 39, 0xb22);
    check(
        run(guest_write as *const () as u64, DATA).esr >> 26 == 0x16,
        "writable store",
    );
    read(DATA, 0x1234);
    unsafe {
        a.protect_page(DATA, READ_ONLY).expect("protect");
    }
    abort(
        run(guest_write as *const () as u64, DATA),
        DATA,
        false,
        true,
        true,
    );
    read(DATA, 0x1234);
    // Execute from mapped normal data; must fault on Stage-2 XN before decode.
    unsafe {
        enter_guest(DATA, STACK + PAGE_SIZE, 0);
    }
    abort(
        unsafe { read_volatile(addr_of!(LAST_EXIT)) },
        DATA,
        true,
        false,
        true,
    );
    abort(
        run(guest_read as *const () as u64, root_a),
        root_a,
        false,
        false,
        false,
    );
    check(
        unsafe { a.unmap_page(DATA) } == Ok(addr_of!(PAGE_A) as u64),
        "unmap result",
    );
    abort(
        run(guest_read as *const () as u64, DATA),
        DATA,
        false,
        false,
        false,
    );
    unsafe {
        a.map_page(DATA, addr_of!(PAGE_A) as u64, READ_ONLY)
            .expect("remap");
    }
    read(DATA, 0x1234);
    check(
        run(guest_execute as *const () as u64, 0).value == 0x5678,
        "executable code",
    );
    emit("Stage-2 permissions, unmap and concatenated root PASS\n");
    drop(a);

    let before = pool.live();
    pool.budget = 1;
    let mut a = unsafe { Mapper::new(&mut pool, root_a, config, 1).unwrap() };
    check(
        unsafe { a.map_page(1 << 30, addr_of!(PAGE_A) as u64, READ_ONLY) }
            == Err(Stage2Error::OutOfMemory),
        "partial allocation accepted",
    );
    read(DATA, 0x1234);
    drop(a);
    check(pool.live() == before, "unpublished child leaked");
    pool.budget = usize::MAX;
    let mut a = unsafe { Mapper::new(&mut pool, root_a, config, 1).unwrap() };
    unsafe {
        a.map_page(1 << 30, addr_of!(PAGE_A) as u64, READ_ONLY)
            .expect("retry allocation");
    }
    read(1 << 30, 0x1234);
    check(
        unsafe { a.map_page(1 << 40, addr_of!(PAGE_A) as u64, READ_ONLY) }
            == Err(Stage2Error::InvalidAddress),
        "IPA overflow",
    );
    check(
        unsafe { a.map_page(DATA + PAGE_SIZE, 1 << config.physical_bits(), READ_ONLY) }
            == Err(Stage2Error::InvalidAddress),
        "PA overflow",
    );
    let device_execute = Access {
        memory: MemoryType::Device,
        ..READ_EXECUTE
    };
    check(
        unsafe { a.map_page(DATA + PAGE_SIZE, addr_of!(PAGE_A) as u64, device_execute) }
            == Err(Stage2Error::InvalidPermissions),
        "executable device",
    );
    drop(a);
    check(pool.live() == before + 2, "invalid input allocated tables");
    emit("Stage-2 allocation rollback and invalid input PASS\n");

    let mut b = unsafe { Mapper::new(&mut pool, root_b, config, 2).unwrap() };
    select(&b);
    map_basics(&mut b, addr_of!(PAGE_B) as u64);
    read(DATA, 0xb22);
    drop(b);
    let a = unsafe { Mapper::new(&mut pool, root_a, config, 1).unwrap() };
    select(&a);
    read(DATA, 0x1234);
    drop(a);
    // VMID 1's old owner is quiescent. Reuse it with a distinct existing root.
    let b = unsafe { Mapper::new(&mut pool, root_b, config, 1).unwrap() };
    select(&b);
    read(DATA, 0xb22);
    emit("Stage-2 VMID switching and reuse PASS\n");
    drop(b);
    for (bits, pages) in [(32, 4), (44, 1)] {
        if bits > config.physical_bits() {
            check(
                Config::from_features(features, Some(bits)) == Err(Stage2Error::InvalidAddress),
                "unsupported IPA geometry admitted",
            );
            emit("Stage-2 geometry beyond actual PA width rejected\n");
            continue;
        }
        let geometry = Config::from_features(features, Some(bits)).expect("root geometry");
        check(geometry.root_pages() == pages, "wrong root size");
        // SAFETY: All old guests are stopped. Retain all old child frames;
        // clear the owned root only, then activate flushes the reused VMID.
        unsafe {
            addr_of_mut!(ROOT_A).write(Root([const { PageTable::zeroed() }; 16]));
        }
        let mut mapper = unsafe { Mapper::new(&mut pool, root_a, geometry, 1).unwrap() };
        select(&mapper);
        map_basics(&mut mapper, addr_of!(PAGE_A) as u64);
        let high = 1u64 << (bits - 1);
        unsafe {
            mapper
                .map_page(high, addr_of!(PAGE_B) as u64, READ_ONLY)
                .expect("upper IPA");
        }
        read(high, 0xb22);
    }
    emit("Stage-2 supported starting levels PASS\n");
    vcpu_probe::run(config);
    vgic_probe::run(config);
    #[cfg(system_domain_probe)]
    system_domain_probe::run(config);
    emit("STAGE2 PASS aarch64\n");
    finish(0)
}
