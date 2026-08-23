pub const MIN_DEVICE_VECTOR: u8 = 0x20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptError {
    InvalidVector,
    NotInService,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApicMsrError {
    Unsupported,
    Disabled,
    InvalidValue,
    Interrupt(InterruptError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApicMsrEffect {
    None,
    Eoi(u8),
    SelfIpi(u8),
}

const IA32_APIC_BASE: u32 = 0x1b;
const X2APIC_ID: u32 = 0x802;
const X2APIC_VERSION: u32 = 0x803;
const X2APIC_TPR: u32 = 0x808;
const X2APIC_PPR: u32 = 0x80a;
const X2APIC_EOI: u32 = 0x80b;
const X2APIC_SIVR: u32 = 0x80f;
const X2APIC_ISR_BASE: u32 = 0x810;
const X2APIC_TMR_BASE: u32 = 0x818;
const X2APIC_IRR_BASE: u32 = 0x820;
const X2APIC_ESR: u32 = 0x828;
const X2APIC_SELF_IPI: u32 = 0x83f;
const APIC_BASE_ADDRESS: u64 = 0xfee0_0000;
const APIC_BASE_BSP: u64 = 1 << 8;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ENABLE: u64 = 1 << 11;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualLocalApic {
    pending: [u64; 4],
    in_service: [u64; 4],
    masked: [u64; 4],
    task_priority: u8,
    apic_base: u64,
    spurious_vector: u16,
}

impl VirtualLocalApic {
    pub const fn new() -> Self {
        Self {
            pending: [0; 4],
            in_service: [0; 4],
            masked: [0; 4],
            task_priority: 0,
            apic_base: APIC_BASE_ADDRESS | APIC_BASE_BSP | APIC_BASE_ENABLE,
            spurious_vector: 0xff,
        }
    }

    pub fn raise(&mut self, vector: u8) -> Result<(), InterruptError> {
        validate_vector(vector)?;
        set(&mut self.pending, vector);
        Ok(())
    }

    pub fn cancel_pending(&mut self, vector: u8) -> Result<(), InterruptError> {
        validate_vector(vector)?;
        clear(&mut self.pending, vector);
        Ok(())
    }

    pub fn next_pending(&self) -> Option<u8> {
        let processor_priority = self.processor_priority();
        (MIN_DEVICE_VECTOR..=u8::MAX).rev().find(|&vector| {
            contains(&self.pending, vector)
                && !contains(&self.masked, vector)
                && vector & 0xf0 > processor_priority
        })
    }

    pub fn accept(&mut self, vector: u8) -> Result<(), InterruptError> {
        if self.next_pending() != Some(vector) {
            return Err(InterruptError::InvalidVector);
        }
        clear(&mut self.pending, vector);
        set(&mut self.in_service, vector);
        Ok(())
    }

    /// Completes the highest-priority in-service interrupt, matching APIC EOI.
    pub fn eoi(&mut self) -> Result<u8, InterruptError> {
        let vector = self
            .highest_in_service()
            .ok_or(InterruptError::NotInService)?;
        clear(&mut self.in_service, vector);
        Ok(vector)
    }

    pub fn set_masked(&mut self, vector: u8, masked: bool) -> Result<(), InterruptError> {
        validate_vector(vector)?;
        if masked {
            set(&mut self.masked, vector);
        } else {
            clear(&mut self.masked, vector);
        }
        Ok(())
    }

    pub fn set_task_priority(&mut self, priority: u8) {
        self.task_priority = priority & 0xf0;
    }

    pub const fn task_priority(&self) -> u8 {
        self.task_priority
    }

    pub fn read_msr(&self, msr: u32) -> Result<u64, ApicMsrError> {
        if msr == IA32_APIC_BASE {
            return Ok(self.apic_base);
        }
        self.require_x2apic()?;
        match msr {
            X2APIC_ID => Ok(0),
            X2APIC_VERSION => Ok(0x0005_0014),
            X2APIC_TPR => Ok(u64::from(self.task_priority)),
            X2APIC_PPR => Ok(u64::from(self.processor_priority())),
            X2APIC_SIVR => Ok(u64::from(self.spurious_vector)),
            X2APIC_ISR_BASE..=0x817 => {
                Ok(self.register_word(&self.in_service, msr - X2APIC_ISR_BASE))
            }
            X2APIC_TMR_BASE..=0x81f => Ok(0),
            X2APIC_IRR_BASE..=0x827 => Ok(self.register_word(&self.pending, msr - X2APIC_IRR_BASE)),
            X2APIC_ESR => Ok(0),
            _ => Err(ApicMsrError::Unsupported),
        }
    }

    pub fn write_msr(&mut self, msr: u32, value: u64) -> Result<ApicMsrEffect, ApicMsrError> {
        if msr == IA32_APIC_BASE {
            let allowed = APIC_BASE_ADDRESS | APIC_BASE_BSP | APIC_BASE_X2APIC | APIC_BASE_ENABLE;
            if value & !allowed != 0
                || value & APIC_BASE_ADDRESS != APIC_BASE_ADDRESS
                || value & APIC_BASE_BSP == 0
                || value & APIC_BASE_X2APIC != 0 && value & APIC_BASE_ENABLE == 0
            {
                return Err(ApicMsrError::InvalidValue);
            }
            self.apic_base = value;
            return Ok(ApicMsrEffect::None);
        }
        self.require_x2apic()?;
        match msr {
            X2APIC_TPR if value <= u8::MAX.into() => {
                self.set_task_priority(value as u8);
                Ok(ApicMsrEffect::None)
            }
            X2APIC_EOI if value == 0 => match self.eoi() {
                Ok(vector) => Ok(ApicMsrEffect::Eoi(vector)),
                Err(InterruptError::NotInService) => Ok(ApicMsrEffect::None),
                Err(error) => Err(ApicMsrError::Interrupt(error)),
            },
            X2APIC_SIVR if value & !0x1ff == 0 && value as u8 >= 0x10 => {
                self.spurious_vector = value as u16;
                Ok(ApicMsrEffect::None)
            }
            X2APIC_ESR if value == 0 => Ok(ApicMsrEffect::None),
            X2APIC_SELF_IPI if value <= u8::MAX.into() => {
                let vector = value as u8;
                self.raise(vector).map_err(ApicMsrError::Interrupt)?;
                Ok(ApicMsrEffect::SelfIpi(vector))
            }
            _ => Err(ApicMsrError::Unsupported),
        }
    }

    pub fn is_in_service(&self, vector: u8) -> bool {
        contains(&self.in_service, vector)
    }

    fn highest_in_service(&self) -> Option<u8> {
        (MIN_DEVICE_VECTOR..=u8::MAX)
            .rev()
            .find(|&vector| contains(&self.in_service, vector))
    }

    fn processor_priority(&self) -> u8 {
        self.highest_in_service()
            .map_or(self.task_priority, |vector| self.task_priority.max(vector))
            & 0xf0
    }

    fn require_x2apic(&self) -> Result<(), ApicMsrError> {
        if self.apic_base & (APIC_BASE_ENABLE | APIC_BASE_X2APIC)
            == APIC_BASE_ENABLE | APIC_BASE_X2APIC
        {
            Ok(())
        } else {
            Err(ApicMsrError::Disabled)
        }
    }

    fn register_word(&self, bitmap: &[u64; 4], index: u32) -> u64 {
        let bit = index * 32;
        (bitmap[(bit / 64) as usize] >> (bit % 64)) & u64::from(u32::MAX)
    }
}

impl Default for VirtualLocalApic {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_vector(vector: u8) -> Result<(), InterruptError> {
    if vector < MIN_DEVICE_VECTOR {
        Err(InterruptError::InvalidVector)
    } else {
        Ok(())
    }
}

fn contains(bitmap: &[u64; 4], vector: u8) -> bool {
    bitmap[usize::from(vector / 64)] & (1_u64 << (vector % 64)) != 0
}

fn set(bitmap: &mut [u64; 4], vector: u8) {
    bitmap[usize::from(vector / 64)] |= 1_u64 << (vector % 64);
}

fn clear(bitmap: &mut [u64; 4], vector: u8) {
    bitmap[usize::from(vector / 64)] &= !(1_u64 << (vector % 64));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_highest_priority_pending_vector() {
        let mut apic = VirtualLocalApic::new();
        apic.raise(0x40).unwrap();
        apic.raise(0x71).unwrap();
        assert_eq!(apic.next_pending(), Some(0x71));
        apic.accept(0x71).unwrap();
        assert_eq!(apic.next_pending(), None);
        assert_eq!(apic.eoi(), Ok(0x71));
        assert_eq!(apic.next_pending(), Some(0x40));
    }

    #[test]
    fn mask_and_tpr_hold_interrupts_pending() {
        let mut apic = VirtualLocalApic::new();
        apic.raise(0x40).unwrap();
        apic.set_masked(0x40, true).unwrap();
        assert_eq!(apic.next_pending(), None);
        apic.set_masked(0x40, false).unwrap();
        apic.set_task_priority(0x40);
        assert_eq!(apic.next_pending(), None);
        apic.set_task_priority(0x30);
        assert_eq!(apic.next_pending(), Some(0x40));
    }

    #[test]
    fn repeated_interrupts_are_coalesced_until_eoi() {
        let mut apic = VirtualLocalApic::new();
        apic.raise(0x40).unwrap();
        apic.raise(0x40).unwrap();
        apic.accept(0x40).unwrap();
        assert!(apic.is_in_service(0x40));
        assert_eq!(apic.next_pending(), None);
        assert_eq!(apic.eoi(), Ok(0x40));
        assert_eq!(apic.eoi(), Err(InterruptError::NotInService));
    }

    #[test]
    fn consuming_a_source_can_cancel_an_uninjected_interrupt() {
        let mut apic = VirtualLocalApic::new();
        apic.raise(0x40).unwrap();
        apic.cancel_pending(0x40).unwrap();
        assert_eq!(apic.next_pending(), None);
    }

    #[test]
    fn exception_vectors_cannot_be_raised() {
        let mut apic = VirtualLocalApic::new();
        assert_eq!(apic.raise(0x1f), Err(InterruptError::InvalidVector));
    }

    #[test]
    fn x2apic_tpr_and_pending_registers_share_interrupt_state() {
        let mut apic = VirtualLocalApic::new();
        let base = apic.read_msr(IA32_APIC_BASE).unwrap();
        apic.write_msr(IA32_APIC_BASE, base | APIC_BASE_X2APIC)
            .unwrap();
        apic.write_msr(X2APIC_TPR, 0x40).unwrap();
        apic.raise(0x71).unwrap();
        assert_eq!(apic.read_msr(X2APIC_TPR), Ok(0x40));
        assert_eq!(apic.read_msr(X2APIC_IRR_BASE + 3), Ok(1 << 17));
        apic.accept(0x71).unwrap();
        assert_eq!(apic.read_msr(X2APIC_ISR_BASE + 3), Ok(1 << 17));
        assert_eq!(apic.write_msr(X2APIC_EOI, 0), Ok(ApicMsrEffect::Eoi(0x71)));
    }

    #[test]
    fn x2apic_registers_require_x2apic_mode() {
        let apic = VirtualLocalApic::new();
        assert_eq!(apic.read_msr(X2APIC_TPR), Err(ApicMsrError::Disabled));
    }
}
