pub const MIN_DEVICE_VECTOR: u8 = 0x20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptError {
    InvalidVector,
    NotInService,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualLocalApic {
    pending: [u64; 4],
    in_service: [u64; 4],
    masked: [u64; 4],
    task_priority: u8,
}

impl VirtualLocalApic {
    pub const fn new() -> Self {
        Self {
            pending: [0; 4],
            in_service: [0; 4],
            masked: [0; 4],
            task_priority: 0,
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
        let processor_priority = self
            .highest_in_service()
            .map_or(self.task_priority, |vector| self.task_priority.max(vector))
            & 0xf0;
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

    pub fn is_in_service(&self, vector: u8) -> bool {
        contains(&self.in_service, vector)
    }

    fn highest_in_service(&self) -> Option<u8> {
        (MIN_DEVICE_VECTOR..=u8::MAX)
            .rev()
            .find(|&vector| contains(&self.in_service, vector))
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
}
