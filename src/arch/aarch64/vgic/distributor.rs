//! One-vCPU, 96-line virtual GICv2 Distributor. Guest register writes affect
//! only this vCPU's software LRs; physical Distributor/EOI rights stay in EL2.
use super::super::vcpu::Vcpu;
use super::{Error, ACTIVE, ID, PENDING, STATE};

const INTERRUPTS: usize = 96;

pub struct Distributor {
    control: u32,
    enabled: [u32; 3],
    pending: [u32; 3],
    injected: [u32; 3],
    priority: [u8; INTERRUPTS],
    config: [u32; 6],
}

impl Default for Distributor {
    fn default() -> Self {
        Self::new()
    }
}
impl Distributor {
    pub const fn new() -> Self {
        Self {
            control: 0,
            enabled: [0xffff, 0, 0],
            pending: [0; 3],
            injected: [0; 3],
            priority: [0xa0; INTERRUPTS],
            config: [0xaaaa_aaaa, 0, 0, 0, 0, 0],
        }
    }
    fn enabled(&self, id: usize) -> bool {
        self.control & 1 != 0 && self.enabled[id / 32] & (1 << (id % 32)) != 0
    }
    fn bitmap(&self, first: usize, guest: &Vcpu, active: bool) -> u32 {
        let mut value = if active { 0 } else { self.pending[first / 32] };
        for id in first..first + 32 {
            if guest.interrupt_status(id as u16).is_some_and(|status| {
                if active {
                    status.active
                } else {
                    status.pending
                }
            }) {
                value |= 1 << (id % 32);
            }
        }
        value
    }
    fn access(offset: u64, size: u8) -> Result<usize, Error> {
        if !matches!(size, 1 | 4) || offset >= 4096 || offset & (u64::from(size) - 1) != 0 {
            return Err(Error::InvalidRegion);
        }
        Ok(offset as usize)
    }
    pub fn read(&self, offset: u64, size: u8, guest: &Vcpu) -> Result<u64, Error> {
        let offset = Self::access(offset, size)?;
        if (0x400..0x400 + INTERRUPTS).contains(&offset) {
            return Ok(self.read_bytes(offset - 0x400, size, false));
        }
        if (0x800..0x800 + INTERRUPTS).contains(&offset) {
            return Ok(self.read_bytes(offset - 0x800, size, true));
        }
        if size != 4 {
            return Err(Error::InvalidRegion);
        }
        let value = match offset {
            0 => self.control,
            4 => 2,           // 3 interrupt banks, one CPU, no Security Extensions.
            8 => 0,           // No claim to a physical vendor/implementation identity.
            0x80..=0x88 => 0, // No Group 1 in this virtual security profile.
            0x100..=0x108 => self.enabled[(offset - 0x100) / 4],
            0x180..=0x188 => self.enabled[(offset - 0x180) / 4],
            0x200..=0x208 => self.bitmap((offset - 0x200) * 8, guest, false),
            0x280..=0x288 => self.bitmap((offset - 0x280) * 8, guest, false),
            0x300..=0x308 => self.bitmap((offset - 0x300) * 8, guest, true),
            0x380..=0x388 => self.bitmap((offset - 0x380) * 8, guest, true),
            0xc00..=0xc14 => self.config[(offset - 0xc00) / 4],
            0xf00 => 0, // SGIR is write-only.
            0xf10..=0xf1c | 0xf20..=0xf2c => {
                let first = (offset & 15) as u16;
                let mut pending = 0;
                for byte in 0..4 {
                    let id = first + byte;
                    if self.pending[0] & (1 << id) != 0
                        || guest.interrupt_status(id).is_some_and(|s| s.pending)
                    {
                        pending |= 1 << (byte * 8);
                    }
                }
                pending
            }
            0xfe8 => 0x20, // Architectural GICv2 identification.
            0xfd0..=0xfe4 | 0xfec => 0,
            0xff0 => 0x0d,
            0xff4 => 0xf0,
            0xff8 => 0x05,
            0xffc => 0xb1,
            _ => return Err(Error::UnsupportedController),
        };
        Ok(u64::from(value))
    }
    fn read_bytes(&self, first: usize, size: u8, targets: bool) -> u64 {
        let mut value = 0;
        for byte in 0..usize::from(size) {
            value |= u64::from(if targets {
                1
            } else {
                self.priority[first + byte]
            }) << (byte * 8);
        }
        value
    }
    pub fn write(
        &mut self,
        offset: u64,
        size: u8,
        value: u64,
        guest: &mut Vcpu,
    ) -> Result<(), Error> {
        let offset = Self::access(offset, size)?;
        if (0x400..0x400 + INTERRUPTS).contains(&offset) {
            for byte in 0..usize::from(size) {
                self.priority[offset - 0x400 + byte] = ((value >> (byte * 8)) as u8) & 0xf8;
            }
            return Ok(());
        }
        if (0x800..0x800 + INTERRUPTS).contains(&offset) {
            // The only CPU is CPU0; unwired target bits cannot address another
            // vCPU or Domain. Banked SGI/PPI target registers are read-only.
            return if value & !0x0101_0101 == 0 {
                Ok(())
            } else {
                Err(Error::InvalidInterrupt)
            };
        }
        if size != 4 {
            return Err(Error::InvalidRegion);
        }
        let value = value as u32;
        match offset {
            0 if value & !1 == 0 => self.control = value,
            0x80..=0x88 => {} // Group registers are RAZ/WI without Security Extensions.
            0x100..=0x108 => self.enabled[(offset - 0x100) / 4] |= value,
            0x180..=0x188 => {
                let bank = (offset - 0x180) / 4;
                self.enabled[bank] &= !value;
                self.enabled[0] |= 0xffff; // SGIs are always enabled.
            }
            0x200..=0x208 => self.pending[(offset - 0x200) / 4] |= value,
            0x280..=0x288 => {
                let bank = (offset - 0x280) / 4;
                self.pending[bank] &= !value;
                self.injected[bank] &= !value;
                for lr in &mut guest.interrupt_state().list {
                    let id = (*lr & ID) as usize;
                    if id / 32 == bank && value & (1 << (id % 32)) != 0 {
                        *lr &= !PENDING;
                    }
                }
            }
            0x380..=0x388 => {
                let bank = (offset - 0x380) / 4;
                for lr in &mut guest.interrupt_state().list {
                    let id = (*lr & ID) as usize;
                    if id / 32 == bank && value & (1 << (id % 32)) != 0 {
                        *lr &= !ACTIVE;
                    }
                }
            }
            0xc00 | 0xc04 => {} // Fixed SGI edge / PPI level wiring.
            0xc08..=0xc14 => self.config[(offset - 0xc00) / 4] = value & 0xaaaa_aaaa,
            0xf00 if value & !0x03ff_000f == 0 => {
                let filter = (value >> 24) & 3;
                if filter == 3 {
                    return Err(Error::InvalidInterrupt);
                }
                if filter == 2 || filter == 0 && value & (1 << 16) != 0 {
                    self.pending[0] |= 1 << (value & 15);
                }
            }
            0xf10..=0xf1c | 0xf20..=0xf2c => {
                let first = offset & 15;
                for byte in 0..4 {
                    if value & (1 << (byte * 8)) == 0 {
                        continue;
                    }
                    let id = first + byte;
                    if offset >= 0xf20 {
                        self.pending[0] |= 1 << id;
                    } else {
                        self.pending[0] &= !(1 << id);
                        self.injected[0] &= !(1 << id);
                        for lr in &mut guest.interrupt_state().list {
                            if *lr & ID == id as u32 {
                                *lr &= !PENDING;
                            }
                        }
                    }
                }
            }
            _ => return Err(Error::UnsupportedController),
        }
        Ok(())
    }
    /// Reconcile only saved software state before the next world switch.
    /// Queue pressure retains pending bits; IRQ assignment never becomes a
    /// physical Distributor mapping or a guest-controlled HW LR descriptor.
    pub fn synchronize(
        &mut self,
        guest: &mut Vcpu,
        timer: u16,
    ) -> Result<(), super::super::vcpu::Error> {
        if !(16..32).contains(&timer) {
            return Err(super::super::vcpu::Error::InvalidState);
        }
        for id in 0..INTERRUPTS {
            let bank = id / 32;
            let bit = 1 << (id % 32);
            let status = guest.interrupt_status(id as u16);
            if self.injected[bank] & bit != 0 && status.is_none_or(|s| s.active || !s.pending) {
                self.pending[bank] &= !bit;
                self.injected[bank] &= !bit;
            }
            if !self.enabled(id) {
                for lr in &mut guest.interrupt_state().list {
                    if *lr & ID == id as u32 && *lr & PENDING != 0 {
                        if id != usize::from(timer) {
                            self.pending[bank] |= bit;
                        }
                        *lr &= !PENDING;
                        self.injected[bank] &= !bit;
                    }
                }
                continue;
            }
            if id == usize::from(timer) {
                let asserted = guest.virtual_timer_level()? || self.pending[bank] & bit != 0;
                match guest.set_interrupt_level(timer, self.priority[id], asserted) {
                    Ok(()) => {}
                    Err(super::super::vcpu::Error::Interrupts(Error::QueueFull)) => continue,
                    Err(error) => return Err(error),
                }
                if self.pending[bank] & bit != 0 {
                    self.injected[bank] |= bit;
                }
            } else if self.pending[bank] & bit != 0 && status.is_none() {
                match guest.inject_edge(id as u16, self.priority[id], (id < 16).then_some(0)) {
                    Ok(()) => self.injected[bank] |= bit,
                    Err(super::super::vcpu::Error::Interrupts(Error::QueueFull)) => {}
                    Err(error) => return Err(error),
                }
            }
            for lr in &mut guest.interrupt_state().list {
                if *lr & ID == id as u32 && *lr & STATE == PENDING {
                    *lr = (*lr & !(31 << 23)) | (u32::from(self.priority[id] >> 3) << 23);
                }
            }
        }
        Ok(())
    }
}
