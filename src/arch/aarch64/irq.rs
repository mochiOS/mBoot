//! CPU-local ownership of a cold GICv2 and the EL2 physical timer. Only the
//! admitted timer PPI is enabled. No guest acquires physical EOI authority.
use core::{
    arch::asm,
    marker::PhantomData,
    ptr::{read_volatile, write_volatile},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Context(super::vgic::Error),
    InvalidRegion,
    Unsupported,
    Busy,
    WrongCpu,
    InvalidInterrupt,
}

pub struct Controller {
    distributor: usize,
    cpu: usize,
    owner: u64,
    timer: u16,
    _local: PhantomData<*mut ()>,
}

impl Controller {
    /// # Safety
    /// The boot owner has exclusive authority over this cold, single-CPU,
    /// non-security-extension GICv2 and its timer PPI. Both complete ranges
    /// are pinned Device/RW mappings, inaccessible to guests or DMA. No other
    /// controller aliases them. Retain these mappings until IRQs are stopped.
    pub unsafe fn new(
        dist: u64,
        dist_size: u64,
        cpu: u64,
        cpu_size: u64,
        timer: u16,
    ) -> Result<Self, Error> {
        let owner = context()?;
        if [dist, cpu]
            .iter()
            .any(|base| *base == 0 || base & 4095 != 0)
            || dist_size < 4096
            || cpu_size < 4096
            || dist.checked_add(dist_size).is_none()
            || cpu.checked_add(cpu_size).is_none()
            || dist < cpu + cpu_size && cpu < dist + dist_size
        {
            return Err(Error::InvalidRegion);
        }
        if !(16..32).contains(&timer) {
            return Err(Error::InvalidInterrupt);
        }
        let controller = Self {
            distributor: dist as usize,
            cpu: cpu as usize,
            owner,
            timer,
            _local: PhantomData,
        };
        let kind = controller.read_dist(4);
        if kind & ((1 << 10) | (7 << 5)) != 0 {
            return Err(Error::Unsupported);
        }
        if controller.read_dist(0) != 0 || controller.read_cpu(0) & 0x3ff != 0 {
            return Err(Error::Busy);
        }
        let banks = (kind & 31) as usize + 1;
        for bank in 0..banks {
            let enabled = controller.read_dist(0x100 + bank * 4);
            let active = controller.read_dist(0x300 + bank * 4);
            if enabled & if bank == 0 { 0xffff_0000 } else { u32::MAX } != 0 || active != 0 {
                return Err(Error::Busy);
            }
        }
        // All other PPIs remain disabled, including the guest CNTV output.
        // Set only the owned PPI's level configuration and priority.
        let shift = u32::from(timer - 16) * 2;
        let config = controller.read_dist(0xc04) & !(3 << shift);
        controller.write_dist(0xc04, config);
        if controller.read_dist(0xc04) & (2 << shift) != 0 {
            return Err(Error::Unsupported);
        }
        let offset = 0x400 + usize::from(timer / 4) * 4;
        let shift = u32::from(timer % 4) * 8;
        let priority = (controller.read_dist(offset) & !(255 << shift)) | (0x20 << shift);
        controller.write_dist(offset, priority);
        controller.write_cpu(4, 255);
        controller.write_cpu(8, 0);
        controller.write_cpu(0, 1); // EOImode=0, combined priority drop/deactivation.
        controller.write_dist(0, 1);
        controller.write_dist(0x100, 1 << timer);
        synchronize();
        Ok(controller)
    }
    fn check(&self) -> Result<(), Error> {
        if context()? != self.owner {
            return Err(Error::WrongCpu);
        }
        Ok(())
    }
    fn read_dist(&self, offset: usize) -> u32 {
        // SAFETY: Constructor retains the complete exclusive Distributor map.
        unsafe { read_volatile((self.distributor + offset) as *const u32) }
    }
    fn write_dist(&self, offset: usize, value: u32) {
        // SAFETY: Same admitted bank, only architectural in-range offsets.
        unsafe { write_volatile((self.distributor + offset) as *mut u32, value) };
    }
    fn read_cpu(&self, offset: usize) -> u32 {
        // SAFETY: Constructor retains this CPU's complete private interface.
        unsafe { read_volatile((self.cpu + offset) as *const u32) }
    }
    fn write_cpu(&self, offset: usize, value: u32) {
        // SAFETY: Same CPU-local bank; check prevents cross-CPU EOI.
        unsafe { write_volatile((self.cpu + offset) as *mut u32, value) };
    }
    pub fn acknowledge(&mut self) -> Result<Option<Pending<'_>>, Error> {
        self.check()?;
        let token = self.read_cpu(12);
        let id = token & 1023;
        if id >= 1020 {
            return Ok(None);
        }
        if id != u32::from(self.timer) {
            return Err(Error::InvalidInterrupt);
        }
        Ok(Some(Pending {
            controller: self,
            token,
        }))
    }
}

/// Only an actual IAR read can create an acknowledgement. Completion uses the
/// whole retained token; callers cannot forge a physical deactivation ID.
#[must_use]
pub struct Pending<'a> {
    controller: &'a mut Controller,
    token: u32,
}
impl Pending<'_> {
    /// Clear/rearm the owned timer source before completing this token.
    pub fn complete(self) -> Result<(), Error> {
        self.controller.check()?;
        synchronize();
        self.controller.write_cpu(16, self.token);
        synchronize();
        Ok(())
    }
}

pub struct Timer {
    owner: u64,
    period: u64,
    _local: PhantomData<*mut ()>,
}
impl Timer {
    /// # Safety
    /// Exclusively retain this CPU's EL2 physical timer. No firmware, host
    /// scheduler or other owner may program its control/deadline concurrently.
    pub unsafe fn new(rate: u64) -> Result<Self, Error> {
        let owner = context()?;
        let frequency: u64;
        let control: u64;
        // SAFETY: Confirmed EL2 context, read-only admission of this bank.
        unsafe {
            asm!("mrs {}, cntfrq_el0", "mrs {}, cnthp_ctl_el2", out(reg) frequency, out(reg) control, options(nomem, nostack));
        }
        if control & 1 != 0 {
            return Err(Error::Busy);
        }
        if rate == 0 || frequency < rate || frequency > u64::from(u32::MAX) {
            return Err(Error::Unsupported);
        }
        Ok(Self {
            owner,
            period: frequency / rate,
            _local: PhantomData,
        })
    }
    pub fn rearm(&mut self) -> Result<(), Error> {
        if context()? != self.owner {
            return Err(Error::WrongCpu);
        }
        let deadline = now().wrapping_add(self.period);
        // SAFETY: Exclusive CPU-local bank, protected by context and ownership.
        unsafe {
            asm!("msr cnthp_cval_el2, {}", "msr cnthp_ctl_el2, {}", "isb",
            in(reg) deadline, in(reg) 1u64, options(nostack));
        }
        Ok(())
    }
}

pub(super) fn now() -> u64 {
    let value;
    // SAFETY: Architected physical counter; no board register or secret bank.
    unsafe {
        asm!("isb", "mrs {}, cntpct_el0", out(reg) value, options(nomem, nostack));
    }
    value
}
fn synchronize() {
    // SAFETY: Privileged controller owner completes MMIO before deactivation.
    unsafe {
        asm!("dsb sy", "isb", options(nostack));
    }
}
fn context() -> Result<u64, Error> {
    super::vgic::context().map_err(Error::Context)
}
