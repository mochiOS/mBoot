//! CPU-local GICv2 virtualization control. Only software-originated virtual
//! interrupts are admitted here; physical IRQ assignment/acknowledgement and
//! retained event queues belong to the hypervisor's resource owner.
use core::arch::asm;
use core::marker::PhantomData;
use core::ptr::{read_volatile, write_volatile};

const HCR: usize = 0;
const VTR: usize = 4;
const VMCR: usize = 8;
const APR: usize = 0xf0;
const LR: usize = 0x100;
const PENDING: u32 = 1 << 28;
const ACTIVE: u32 = 1 << 29;
const STATE: u32 = PENDING | ACTIVE;
const ID: u32 = 1023;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidRegion,
    WrongCpu,
    InvalidContext,
    UnsupportedController,
    Busy,
    Unconfigured,
    ForeignController,
    InvalidInterrupt,
    QueueFull,
    ConflictingSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptStatus {
    pub pending: bool,
    pub active: bool,
}

/// Private to its enclosing vCPU. No caller can edit raw LRs to acquire a
/// hardware interrupt's deactivation authority (the HW bit always stays zero).
pub(super) struct State {
    binding: Option<(usize, u64, u32)>,
    hcr: u32,
    vmcr: u32,
    apr: u32,
    list: [u32; 64],
}

impl State {
    pub(super) const fn new() -> Self {
        Self {
            binding: None,
            hcr: 1,
            vmcr: 0,
            apr: 0,
            list: [0; 64],
        }
    }

    pub(super) fn configured(&self) -> bool {
        self.binding.is_some()
    }

    pub(super) fn inject_edge(
        &mut self,
        id: u16,
        priority: u8,
        source: Option<u8>,
    ) -> Result<(), Error> {
        let (_, _, vtr) = self.binding.ok_or(Error::Unconfigured)?;
        if id >= 1020
            || priority & 7 != 0
            || if id < 16 {
                !matches!(source, Some(0..=7))
            } else {
                source.is_some()
            }
        {
            return Err(Error::InvalidInterrupt);
        }
        let source = u32::from(source.unwrap_or(0)) << 10;
        let count = (vtr & 63) as usize + 1;
        if let Some(lr) = self.list[..count]
            .iter_mut()
            .find(|lr| **lr & STATE != 0 && **lr & ID == u32::from(id))
        {
            if *lr & (7 << 10) != source {
                return Err(Error::ConflictingSource);
            }
            // Coalesce an edge already pending. An active interrupt may retain
            // a subsequent pending edge, without losing its active priority.
            *lr |= PENDING;
            return Ok(());
        }
        let slot = self.list[..count]
            .iter_mut()
            .find(|lr| **lr & STATE == 0)
            .ok_or(Error::QueueFull)?;
        *slot = PENDING | (u32::from(priority >> 3) << 23) | source | u32::from(id);
        Ok(())
    }

    pub(super) fn status(&self, id: u16) -> Option<InterruptStatus> {
        let (_, _, vtr) = self.binding?;
        self.list[..((vtr & 63) as usize + 1)]
            .iter()
            .find(|lr| **lr & STATE != 0 && **lr & ID == u32::from(id))
            .map(|lr| InterruptStatus {
                pending: lr & PENDING != 0,
                active: lr & ACTIVE != 0,
            })
    }
}

/// Exclusive ownership of an EL2-only, mapped GICH bank on one physical CPU.
/// Discovery and MMIO ownership must precede construction; no board address or
/// guest-visible mapping is inferred by this register mechanism.
pub struct Controller {
    base: usize,
    owner: u64,
    vtr: u32,
    _local: PhantomData<*mut ()>,
}

impl Controller {
    /// # Safety
    /// The region is a pinned, writable Device mapping of this CPU's GICv2
    /// GICH bank, with exclusive lifetime ownership by this object. It is not
    /// accessible to a guest or DMA. No other controller may alias it. GICv3
    /// system-register CPU interfaces are disabled for the baseline v2 path.
    pub unsafe fn new(mapped_base: u64, mapped_size: u64) -> Result<Self, Error> {
        let owner = context()?;
        if mapped_base == 0
            || mapped_base & 4095 != 0
            || mapped_size < 4096
            || mapped_base.checked_add(mapped_size).is_none()
        {
            return Err(Error::InvalidRegion);
        }
        let mut controller = Self {
            base: mapped_base as usize,
            owner,
            vtr: 0,
            _local: PhantomData,
        };
        let vtr = controller.read(VTR);
        // GICv2 has five priority/preemption bits, up to 64 list registers.
        if vtr & !0xfc00_003f != 0 || vtr >> 29 != 4 || (vtr >> 26) & 7 != 4 {
            return Err(Error::UnsupportedController);
        }
        if controller.read(HCR) & 1 != 0 {
            return Err(Error::Busy);
        }
        controller.vtr = vtr;
        controller.clear();
        Ok(controller)
    }

    fn read(&self, offset: usize) -> u32 {
        // SAFETY: Construction owns a 4K Device mapping; all offsets <= 0x1fc.
        unsafe { read_volatile((self.base + offset) as *const u32) }
    }
    fn write(&self, offset: usize, value: u32) {
        // SAFETY: Same exclusive GICH mapping; no guest is executing here.
        unsafe { write_volatile((self.base + offset) as *mut u32, value) }
    }
    fn synchronize(&self) {
        // SAFETY: The caller owns the controller at privileged EL2.
        unsafe {
            asm!("dsb sy", "isb", options(nostack, preserves_flags));
        }
    }
    fn validate(&self) -> Result<(), Error> {
        if context()? != self.owner {
            return Err(Error::WrongCpu);
        }
        Ok(())
    }
    fn binding(&self) -> (usize, u64, u32) {
        (self.base, self.owner, self.vtr)
    }

    pub(super) fn configure(&self, state: &mut State) -> Result<(), Error> {
        self.validate()?;
        if let Some(binding) = state.binding {
            if binding != self.binding() {
                return Err(Error::ForeignController);
            }
        } else {
            state.binding = Some(self.binding());
        }
        Ok(())
    }

    pub(super) fn load(&mut self, state: &State) -> Result<(), Error> {
        self.validate()?;
        if state.binding != Some(self.binding()) {
            return Err(Error::ForeignController);
        }
        if self.read(HCR) & 1 != 0 {
            return Err(Error::Busy);
        }
        self.write(HCR, 0);
        self.synchronize();
        self.write(VMCR, state.vmcr);
        self.write(APR, state.apr);
        for i in 0..((self.vtr & 63) as usize + 1) {
            self.write(LR + 4 * i, state.list[i]);
        }
        self.synchronize();
        self.write(HCR, state.hcr | 1);
        self.synchronize();
        Ok(())
    }

    pub(super) fn save(&mut self, state: &mut State) -> Result<(), Error> {
        self.validate()?;
        if state.binding != Some(self.binding()) {
            return Err(Error::ForeignController);
        }
        state.hcr = self.read(HCR);
        self.write(HCR, state.hcr & !1);
        self.synchronize();
        state.vmcr = self.read(VMCR);
        state.apr = self.read(APR);
        for i in 0..((self.vtr & 63) as usize + 1) {
            state.list[i] = self.read(LR + 4 * i);
        }
        self.clear();
        Ok(())
    }

    fn clear(&self) {
        self.write(HCR, 0);
        self.synchronize();
        self.write(VMCR, 0);
        self.write(APR, 0);
        for i in 0..((self.vtr & 63) as usize + 1) {
            self.write(LR + 4 * i, 0);
        }
        self.synchronize();
    }
}

fn context() -> Result<u64, Error> {
    let level: u64;
    let daif: u64;
    // SAFETY: Read-only context registers, prior to accessing an EL2-only bank.
    unsafe {
        asm!("mrs {}, CurrentEL", "mrs {}, daif", out(reg) level, out(reg) daif,
        options(nomem, nostack, preserves_flags));
    }
    if level != 8 || daif & 0x3c0 != 0x3c0 {
        return Err(Error::InvalidContext);
    }
    let owner;
    // SAFETY: EL2 confirmed; physical affinity, not the guest VMPIDR alias.
    unsafe {
        asm!("mrs {}, mpidr_el1", out(reg) owner, options(nomem, nostack, preserves_flags));
    }
    Ok(owner)
}
