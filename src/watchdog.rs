//! Hardware watchdogs retained by mBoot.
//!
//! A watchdog owned by a guest cannot detect a stalled hypervisor. mBoot
//! therefore programs the device itself and reloads it only after a heartbeat
//! from the System Domain.

use core::ptr::write_volatile;

use crate::pci::{self, PciFunction};

const INTEL_VENDOR_ID: u16 = 0x8086;
const INTEL_6300ESB_WATCHDOG_ID: u16 = 0x25ab;
const PCI_COMMAND_MEMORY: u32 = 1 << 1;
const PCI_BAR_MEMORY_MASK: u32 = !0x0f;
const ESB_CONFIG_REGISTER: u16 = 0x60;
const ESB_LOCK_REGISTER: u16 = 0x68;
const ESB_TIMER1_OFFSET: usize = 0x00;
const ESB_TIMER2_OFFSET: usize = 0x04;
const ESB_RELOAD_OFFSET: usize = 0x0c;
const ESB_WATCHDOG_ENABLE: u8 = 1 << 1;
const ESB_WATCHDOG_LOCK: u8 = 1;
const ESB_RELOAD: u16 = 1 << 8;
const ESB_TIMED_OUT: u16 = 1 << 9;
const ESB_UNLOCK1: u16 = 0x80;
const ESB_UNLOCK2: u16 = 0x86;
const ESB_MIN_TIMEOUT_SECONDS: u16 = 1;
const ESB_MAX_TIMEOUT_SECONDS: u16 = 2046;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidTimeout,
    InvalidBar,
    PciAccess,
    RegisterWriteFailed,
}

pub struct HardwareWatchdog {
    backend: Backend,
}

enum Backend {
    Intel6300Esb(Intel6300Esb),
}

struct Intel6300Esb {
    registers: usize,
}

impl HardwareWatchdog {
    /// Finds, initializes, and starts a watchdog owned by mBoot.
    ///
    /// `None` means that no supported device was advertised. An advertised but
    /// invalid device returns an error so a broken watchdog is never reported
    /// as active.
    ///
    /// # Safety
    /// PCI configuration mechanism 1 and the discovered MMIO range must be
    /// exclusively owned by mBoot. The firmware identity mapping must remain
    /// active for the MMIO range.
    pub unsafe fn start(
        functions: &[PciFunction],
        timeout_seconds: u16,
    ) -> Result<Option<Self>, Error> {
        let Some(function) = functions.iter().find(|function| {
            function.vendor == INTEL_VENDOR_ID && function.device == INTEL_6300ESB_WATCHDOG_ID
        }) else {
            return Ok(None);
        };
        let watchdog = unsafe { Intel6300Esb::start(function.requester, timeout_seconds)? };
        Ok(Some(Self {
            backend: Backend::Intel6300Esb(watchdog),
        }))
    }

    /// Reloads the watchdog countdown.
    ///
    /// # Safety
    /// The watchdog MMIO mapping must still be owned by mBoot.
    pub unsafe fn heartbeat(&mut self) -> Result<(), Error> {
        match &mut self.backend {
            Backend::Intel6300Esb(watchdog) => unsafe { watchdog.heartbeat() },
        }
    }

    pub const fn name(&self) -> &'static str {
        match &self.backend {
            Backend::Intel6300Esb(_) => "Intel 6300ESB",
        }
    }
}

impl Intel6300Esb {
    unsafe fn start(requester: u16, timeout_seconds: u16) -> Result<Self, Error> {
        if !(ESB_MIN_TIMEOUT_SECONDS..=ESB_MAX_TIMEOUT_SECONDS).contains(&timeout_seconds) {
            return Err(Error::InvalidTimeout);
        }
        let bar = unsafe { pci::config_read(requester, 0x10) }.map_err(|_| Error::PciAccess)?;
        if bar & 1 != 0 || bar & PCI_BAR_MEMORY_MASK == 0 {
            return Err(Error::InvalidBar);
        }
        let registers = (bar & PCI_BAR_MEMORY_MASK) as usize;

        let command = unsafe { pci::config_read(requester, 0x04) }.map_err(|_| Error::PciAccess)?;
        unsafe { pci::config_write(requester, 0x04, command | PCI_COMMAND_MEMORY) }
            .map_err(|_| Error::PciAccess)?;

        // Timer 1 must not generate an interrupt. Timer 2 drives reset output.
        unsafe { pci::config_write_u16(requester, ESB_CONFIG_REGISTER, 0x0003) }
            .map_err(|_| Error::PciAccess)?;

        let lock = unsafe { pci::config_read_u8(requester, ESB_LOCK_REGISTER) }
            .map_err(|_| Error::PciAccess)?;
        if lock & ESB_WATCHDOG_LOCK != 0 {
            return Err(Error::RegisterWriteFailed);
        }
        unsafe { pci::config_write_u8(requester, ESB_LOCK_REGISTER, 0) }
            .map_err(|_| Error::PciAccess)?;

        let watchdog = Self { registers };
        let timer_value = u32::from(timeout_seconds) << 9;
        unsafe {
            watchdog.unlock();
            watchdog.write_u16(ESB_RELOAD_OFFSET, ESB_TIMED_OUT | ESB_RELOAD);
            watchdog.unlock();
            watchdog.write_u32(ESB_TIMER1_OFFSET, timer_value);
            watchdog.unlock();
            watchdog.write_u32(ESB_TIMER2_OFFSET, timer_value);
            watchdog.unlock();
            watchdog.write_u16(ESB_RELOAD_OFFSET, ESB_RELOAD);
            pci::config_write_u8(requester, ESB_LOCK_REGISTER, ESB_WATCHDOG_ENABLE)
        }
        .map_err(|_| Error::PciAccess)?;

        let installed = unsafe { pci::config_read_u8(requester, ESB_LOCK_REGISTER) }
            .map_err(|_| Error::PciAccess)?;
        if installed & ESB_WATCHDOG_ENABLE == 0 {
            return Err(Error::RegisterWriteFailed);
        }
        Ok(watchdog)
    }

    unsafe fn heartbeat(&mut self) -> Result<(), Error> {
        unsafe {
            self.unlock();
            self.write_u16(ESB_RELOAD_OFFSET, ESB_RELOAD);
        }
        Ok(())
    }

    unsafe fn unlock(&self) {
        unsafe {
            self.write_u16(ESB_RELOAD_OFFSET, ESB_UNLOCK1);
            self.write_u16(ESB_RELOAD_OFFSET, ESB_UNLOCK2);
        }
    }

    unsafe fn write_u16(&self, offset: usize, value: u16) {
        unsafe { write_volatile((self.registers + offset) as *mut u16, value) };
    }

    unsafe fn write_u32(&self, offset: usize, value: u32) {
        unsafe { write_volatile((self.registers + offset) as *mut u32, value) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_range_matches_the_two_stage_counter() {
        assert_eq!(ESB_MIN_TIMEOUT_SECONDS, 1);
        assert_eq!(ESB_MAX_TIMEOUT_SECONDS, 2046);
        assert_eq!(u32::from(30_u16) << 9, 15_360);
    }
}
