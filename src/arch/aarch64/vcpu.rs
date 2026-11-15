//! Non-VHE EL2 continuation for the baseline AArch64 EL1 register bank.
//!
//! This is the execution mechanism, not Domain scheduling, an SMCCC server or
//! a virtual interrupt controller. Unsupported extended register banks fail
//! admission; they must be trapped or switched before admitting newer CPUs.
use core::arch::{asm, global_asm};
use core::marker::PhantomData;

use super::stage2::{self, Mapper, TableMemory};
use super::vgic;

const WALK_DISABLED: u64 = (1 << 7) | (1 << 23);
const RESET_SCTLR: u64 = 0x30d0_0800;
const RESET_TCR: u64 = 0xb510_3510 | WALK_DISABLED;
// AArch64 EL1, Stage-2, physical exceptions to EL2, trapping SMC/WFx,
// implementation-defined controls, set/way cache maintenance and ID registers.
// PAC key/instruction access stays trapped (APK/API=0).
const GUEST_HCR: u64 = (1 << 35)
    | (1 << 31)
    | (1 << 22)
    | (1 << 21)
    | (1 << 20)
    | (1 << 19)
    | (1 << 18)
    | (1 << 17)
    | (1 << 16)
    | (1 << 14)
    | (1 << 13)
    | (1 << 5)
    | (1 << 4)
    | (1 << 3)
    | (1 << 2)
    | 1;
// Non-VHE RES1, trap SVE/trace/AMU. This admitted baseline has no SME, so
// bit 12 is RES1 (it becomes TSM=0 when SME state is supported in future).
// Base FP is switched eagerly.
const GUEST_CPTR: u64 =
    (1 << 30) | (1 << 20) | (1 << 13) | (1 << 12) | (1 << 9) | (1 << 8) | 0xff;
// All debug, OS debug and PMU register accesses trap. No debug/PMU ABI yet.
const GUEST_MDCR: u64 =
    (1 << 12) | (1 << 11) | (1 << 10) | (1 << 9) | (1 << 8) | (1 << 6) | (1 << 5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidState,
    UnsupportedCpu,
    WrongCpu,
    WrongAddressSpace,
    Stage2(stage2::Error),
    Interrupts(vgic::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum ExitKind {
    Synchronous = 0,
    Irq = 1,
    Fiq = 2,
    SError = 3,
}

/// ESR/FAR/HPFAR are raw architectural exit information. The dispatcher must
/// check their validity bits before interpreting an address or advancing PC.
#[derive(Clone, Copy, Debug)]
pub struct Exit {
    pub kind: ExitKind,
    pub esr: u64,
    pub far: u64,
    pub hpfar: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemRegister {
    pub op0: u8,
    pub op1: u8,
    pub crn: u8,
    pub crm: u8,
    pub op2: u8,
}

#[repr(C, align(16))]
struct Registers {
    x: [u64; 31],
    pc: u64,
    pstate: u64,
    fpcr: u64,
    fpsr: u64,
    _pad: u64,
    q: [u128; 32],
    esr: u64,
    far: u64,
    hpfar: u64,
    kind: u64,
}

const _: () = {
    assert!(core::mem::offset_of!(Registers, pc) == 248);
    assert!(core::mem::offset_of!(Registers, pstate) == 256);
    assert!(core::mem::offset_of!(Registers, fpcr) == 264);
    assert!(core::mem::offset_of!(Registers, fpsr) == 272);
    assert!(core::mem::offset_of!(Registers, q) == 288);
    assert!(core::mem::offset_of!(Registers, esr) == 800);
    assert!(core::mem::size_of::<Registers>() == 832);
};

// Kept together so the switched EL1 bank and its initialization cannot drift
// into separate hand-maintained save and restore lists. SCTLR/TCR are ordered
// separately to suppress speculative walks while changing translation state.
macro_rules! register_bank {
    ($($name:ident),+ $(,)?) => {
        #[derive(Default)]
        struct SystemRegisters {
            $($name: u64,)+
            sctlr: u64,
            tcr: u64,
        }
        impl SystemRegisters {
            unsafe fn save(&mut self) {
                // SAFETY: Caller exclusively owns the non-VHE EL1 bank at EL2.
                unsafe {
                    $(asm!(concat!("mrs {}, ", stringify!($name)), out(reg) self.$name,
                        options(nomem, nostack, preserves_flags));)+
                    asm!("mrs {}, sctlr_el1", "mrs {}, tcr_el1",
                        out(reg) self.sctlr, out(reg) self.tcr,
                        options(nomem, nostack, preserves_flags));
                }
            }
            unsafe fn restore(&self) {
                // SAFETY: Stage-2 is already selected, EL1 walks stopped.
                // All roots remain guest IPAs and are still subject to Stage-2.
                unsafe {
                    $(asm!(concat!("msr ", stringify!($name), ", {}"), in(reg) self.$name,
                        options(nostack, preserves_flags));)+
                    asm!("dsb nsh", "msr sctlr_el1, {sctlr}", "isb",
                        "msr tcr_el1, {tcr}", "isb",
                        sctlr = in(reg) self.sctlr, tcr = in(reg) self.tcr,
                        options(nostack, preserves_flags));
                }
            }
        }
    }
}
register_bank!(
    sp_el0,
    sp_el1,
    elr_el1,
    spsr_el1,
    tpidr_el0,
    tpidrro_el0,
    tpidr_el1,
    ttbr0_el1,
    ttbr1_el1,
    mair_el1,
    amair_el1,
    vbar_el1,
    cpacr_el1,
    contextidr_el1,
    esr_el1,
    far_el1,
    afsr0_el1,
    afsr1_el1,
    par_el1,
    cntkctl_el1,
    csselr_el1
);

/// A continuation is bound to one retained Stage-2 address space and physical
/// CPU. Migration and extended CPU state require separate implementations.
/// The type cannot be cloned, sent or shared between execution owners.
pub struct Vcpu {
    registers: Registers,
    system: SystemRegisters,
    address_space: (u64, u64),
    owner: u64,
    mpidr: u64,
    timer_compare: u64,
    timer_control: u64,
    interrupts: vgic::State,
    valid: bool,
    _local: PhantomData<*mut ()>,
}

impl Vcpu {
    pub fn new<M: TableMemory>(
        mapper: &Mapper<'_, M>,
        entry: u64,
        argument: u64,
        mpidr: u64,
    ) -> Result<Self, Error> {
        require_host()?;
        admit_cpu()?;
        if entry == 0
            || entry & 3 != 0
            || entry >= 1u64 << 48
            || mpidr & !0x0000_00ff_00ff_ffff != 0
        {
            return Err(Error::InvalidState);
        }
        let mut registers = Registers {
            x: [0; 31],
            pc: entry,
            pstate: 0x3c5,
            fpcr: 0,
            fpsr: 0,
            _pad: 0,
            q: [0; 32],
            esr: 0,
            far: 0,
            hpfar: 0,
            kind: 0,
        };
        registers.x[0] = argument;
        Ok(Self {
            registers,
            system: SystemRegisters {
                sctlr: RESET_SCTLR,
                tcr: RESET_TCR,
                ..SystemRegisters::default()
            },
            address_space: mapper.identity(),
            owner: hardware_id(),
            mpidr,
            timer_compare: u64::MAX,
            timer_control: 0,
            interrupts: vgic::State::new(),
            valid: true,
            _local: PhantomData,
        })
    }

    pub fn register(&self, index: usize) -> Option<u64> {
        self.registers.x.get(index).copied()
    }

    pub fn simd_register(&self, index: usize) -> Option<u128> {
        self.registers.q.get(index).copied()
    }

    pub fn pstate(&self) -> u64 {
        self.registers.pstate
    }

    pub fn configure_interrupts(&mut self, controller: &vgic::Controller) -> Result<(), Error> {
        controller
            .configure(&mut self.interrupts)
            .map_err(Error::Interrupts)
    }

    /// Queue a software-originated, coalesced edge in the assigned virtual
    /// controller. The caller must retain authoritative event payloads and
    /// retry QueueFull; no event data is stored in this hardware queue.
    pub fn inject_edge(&mut self, id: u16, priority: u8, source: Option<u8>) -> Result<(), Error> {
        self.interrupts
            .inject_edge(id, priority, source)
            .map_err(Error::Interrupts)
    }

    pub fn interrupt_status(&self, id: u16) -> Option<vgic::InterruptStatus> {
        self.interrupts.status(id)
    }

    /// Complete an architectural HVC #0 without changing unrelated registers.
    /// ELR_EL2 already points after HVC; advancing it again would skip code.
    pub fn complete_hypercall(&mut self, value: u64) -> Result<(), Error> {
        if !self.valid
            || self.registers.kind != ExitKind::Synchronous as u64
            || self.registers.esr >> 26 != 0x16
            || self.registers.esr & 0xffff != 0
        {
            return Err(Error::InvalidState);
        }
        self.registers.x[0] = value;
        // A completion is single-use, even if the same value is supplied.
        self.registers.esr = 0;
        Ok(())
    }

    /// Decode a trapped AArch64 MRS, without implementing register policy.
    pub fn system_register_read(&self) -> Option<SystemRegister> {
        let esr = self.registers.esr;
        if !self.valid
            || self.registers.kind != ExitKind::Synchronous as u64
            || esr >> 26 != 0x18
            || esr & (1 << 25) == 0
            || esr & 1 == 0
        {
            return None;
        }
        Some(SystemRegister {
            op0: ((esr >> 20) & 3) as u8,
            op1: ((esr >> 14) & 7) as u8,
            crn: ((esr >> 10) & 15) as u8,
            crm: ((esr >> 1) & 15) as u8,
            op2: ((esr >> 17) & 7) as u8,
        })
    }

    /// The trusted dispatcher supplies a value only after applying its CPU
    /// feature policy. Unlike HVC, the trapped MRS itself must be skipped.
    pub fn complete_system_register_read(&mut self, value: u64) -> Result<(), Error> {
        self.system_register_read().ok_or(Error::InvalidState)?;
        let next = self
            .registers
            .pc
            .checked_add(4)
            .ok_or(Error::InvalidState)?;
        let target = ((self.registers.esr >> 5) & 31) as usize;
        if target != 31 {
            self.registers.x[target] = value;
        }
        self.registers.pc = next;
        self.registers.esr = 0;
        Ok(())
    }

    /// Enter or resume the retained guest; returns on any lower-EL exception.
    ///
    /// # Safety
    /// Call non-reentrantly at non-VHE EL2 with all DAIF masks set. The host
    /// uses base FP/SIMD only and does not execute at EL1. The caller owns the
    /// CPU, vCPU, Stage-2 reservation and pinned backing pages throughout the
    /// call; no guest or DMA may access this state or the host stack. Virtual
    /// GIC/other optional execution engines must be disabled until their state
    /// switch is integrated. No physical interrupt-controller passthrough.
    /// The caller must provide cross-Domain speculative-execution mitigation;
    /// this register mechanism alone does not establish that protection.
    /// On return, EL1 translation walks and its virtual timer are stopped,
    /// while the selected Stage-2 context remains unchanged.
    pub unsafe fn run<M: TableMemory>(&mut self, mapper: &Mapper<'_, M>) -> Result<Exit, Error> {
        // SAFETY: Inherits this public method's exclusive execution contract.
        unsafe { self.run_inner(mapper, None) }
    }

    /// Enter with the vCPU's own GICv2 state, saving active/pending priorities
    /// before returning to the caller. A configured vCPU requires this path.
    ///
    /// # Safety
    /// The same execution, ownership, speculation and DMA requirements as
    /// run(), with this exclusively owned controller replacing its disabled
    /// virtual-GIC requirement. Its GICV mapping belongs to this Domain only.
    pub unsafe fn run_with_gic<M: TableMemory>(
        &mut self,
        mapper: &Mapper<'_, M>,
        controller: &mut vgic::Controller,
    ) -> Result<Exit, Error> {
        // SAFETY: Public caller supplies the exclusive execution resources.
        unsafe { self.run_inner(mapper, Some(controller)) }
    }

    unsafe fn run_inner<M: TableMemory>(
        &mut self,
        mapper: &Mapper<'_, M>,
        mut controller: Option<&mut vgic::Controller>,
    ) -> Result<Exit, Error> {
        require_host()?;
        if !self.valid {
            return Err(Error::InvalidState);
        }
        if self.owner != hardware_id() {
            return Err(Error::WrongCpu);
        }
        if self.address_space != mapper.identity() {
            return Err(Error::WrongAddressSpace);
        }
        mapper.require_active().map_err(Error::Stage2)?;
        if controller.is_none() && self.interrupts.configured() {
            return Err(Error::InvalidState);
        }
        let pstate = self.registers.pstate;
        if !matches!(pstate & 0x1f, 0 | 5) || self.registers.pc & 3 != 0 {
            self.valid = false;
            return Err(Error::InvalidState);
        }
        let host = HostRegisters::save();
        if let Some(controller) = controller.as_deref_mut() {
            controller
                .load(&self.interrupts)
                .map_err(Error::Interrupts)?;
        }
        // SAFETY: Preconditions above plus the exclusive execution contract.
        unsafe {
            quiesce();
            asm!("msr hcr_el2, {hcr}", "msr cptr_el2, {cptr}",
                "msr mdcr_el2, {mdcr}", "msr cnthctl_el2, xzr",
                "msr cntvoff_el2, xzr", "msr mdscr_el1, xzr",
                "msr vmpidr_el2, {mpidr}", "msr vpidr_el2, {midr}", "isb",
                hcr = in(reg) GUEST_HCR, cptr = in(reg) GUEST_CPTR,
                mdcr = in(reg) GUEST_MDCR, mpidr = in(reg) self.mpidr | (1 << 31),
                midr = in(reg) host.midr, options(nostack, preserves_flags));
            self.system.restore();
            asm!("msr cntv_cval_el0, {compare}", "msr cntv_ctl_el0, {control}", "isb",
                compare = in(reg) self.timer_compare, control = in(reg) self.timer_control,
                options(nostack, preserves_flags));
            mboot_aarch64_run(&mut self.registers);
            // Rust may use host FP now; assembly has already captured all guest
            // FP state and restored the exact host FP bank before returning.
            self.system.save();
            asm!("mrs {}, cntv_cval_el0", "mrs {}, cntv_ctl_el0",
                out(reg) self.timer_compare, out(reg) self.timer_control,
                options(nomem, nostack, preserves_flags));
            quiesce();
            let saved_interrupts = match controller {
                Some(controller) => controller.save(&mut self.interrupts),
                None => Ok(()),
            };
            host.restore();
            saved_interrupts.map_err(Error::Interrupts)?;
        }
        if !matches!(self.registers.pstate & 0x1f, 0 | 5) {
            self.valid = false;
            return Err(Error::InvalidState);
        }
        let kind = match self.registers.kind {
            0 => ExitKind::Synchronous,
            1 => ExitKind::Irq,
            2 => ExitKind::Fiq,
            3 => ExitKind::SError,
            _ => {
                self.valid = false;
                return Err(Error::InvalidState);
            }
        };
        Ok(Exit {
            kind,
            esr: self.registers.esr,
            far: self.registers.far,
            hpfar: self.registers.hpfar,
        })
    }
}

fn hardware_id() -> u64 {
    let value;
    // SAFETY: Read-only MPIDR access after the privileged context check.
    unsafe {
        asm!("mrs {}, mpidr_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn require_host() -> Result<(), Error> {
    let level: u64;
    let daif: u64;
    // SAFETY: These registers are available before selecting an EL2 bank.
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, daif", out(reg) level, out(reg) daif,
        options(nomem, nostack, preserves_flags));
    }
    if level != 8 || daif & 0x3c0 != 0x3c0 {
        return Err(Error::InvalidState);
    }
    let hcr: u64;
    // SAFETY: CurrentEL confirmed non-aliased EL2 access.
    unsafe {
        asm!("mrs {}, hcr_el2", out(reg) hcr, options(nomem, nostack, preserves_flags));
    }
    if hcr & ((1 << 34) | (1 << 27)) != 0 || hcr & (1 << 31) == 0 {
        return Err(Error::InvalidState);
    }
    Ok(())
}

fn admit_cpu() -> Result<(), Error> {
    let pfr0: u64;
    let pfr1: u64;
    let mmfr3: u64;
    let mmfr1: u64;
    // SAFETY: Read-only baseline feature-register encodings at validated EL2.
    unsafe {
        asm!("mrs {}, id_aa64pfr0_el1", "mrs {}, S3_0_C0_C4_1",
        "mrs {}, S3_0_C0_C7_3", "mrs {}, id_aa64mmfr1_el1",
        out(reg) pfr0, out(reg) pfr1, out(reg) mmfr3, out(reg) mmfr1,
        options(nomem, nostack, preserves_flags));
    }
    // RAS/MTE/GCS/TCR2 and newer banked controls need additional switch/trap
    // code. Reject them rather than leak an unhandled bank across Domains.
    if pfr1 != 0
        || mmfr3 != 0
        || pfr0 & (15 << 28) != 0
        || mmfr1 & (15 << 40) != 0
        || (pfr0 >> 16) & 15 != 0
        || (pfr0 >> 20) & 15 != 0
    {
        return Err(Error::UnsupportedCpu);
    }
    Ok(())
}

unsafe fn quiesce() {
    // SAFETY: Owned non-VHE EL1 bank, all guest execution stopped at EL2.
    unsafe {
        let tcr: u64;
        let sctlr: u64;
        asm!("mrs {}, tcr_el1", "mrs {}, sctlr_el1", out(reg) tcr, out(reg) sctlr,
            options(nomem, nostack, preserves_flags));
        asm!("msr cntv_ctl_el0, xzr", "msr tcr_el1, {tcr}", "isb",
            "msr sctlr_el1, {sctlr}", "isb",
            tcr = in(reg) tcr | WALK_DISABLED, sctlr = in(reg) sctlr | 1,
            options(nostack, preserves_flags));
    }
}

macro_rules! host_registers {
    ($($name:ident),+ $(,)?) => {
        struct HostRegisters { $($name: u64,)+ midr: u64 }
        impl HostRegisters {
            fn save() -> Self {
                $(let $name;)+ let midr;
                // SAFETY: run() checked EL2 and excludes nested execution.
                unsafe {
                    $(asm!(concat!("mrs {}, ", stringify!($name)), out(reg) $name,
                        options(nomem, nostack, preserves_flags));)+
                    asm!("mrs {}, midr_el1", out(reg) midr, options(nomem, nostack, preserves_flags));
                }
                Self { $($name,)+ midr }
            }
            unsafe fn restore(&self) {
                // SAFETY: Guest stopped, walks/timer disabled. Host bank only.
                unsafe {
                    $(asm!(concat!("msr ", stringify!($name), ", {}"), in(reg) self.$name,
                        options(nostack, preserves_flags));)+
                    asm!("isb", options(nostack, preserves_flags));
                }
            }
        }
    }
}
host_registers!(
    hcr_el2,
    cptr_el2,
    mdcr_el2,
    cnthctl_el2,
    cntvoff_el2,
    vmpidr_el2,
    vpidr_el2,
    mdscr_el1,
    cntv_cval_el0,
    cntv_ctl_el0
);

global_asm!(include_str!("vcpu.S"));
unsafe extern "C" {
    fn mboot_aarch64_run(registers: *mut Registers);
}
