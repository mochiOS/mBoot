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
const X2APIC_ICR: u32 = 0x830;
const X2APIC_LVT_TIMER: u32 = 0x832;
const X2APIC_LVT_THERMAL: u32 = 0x833;
const X2APIC_LVT_ERROR: u32 = 0x837;
const X2APIC_INITIAL_COUNT: u32 = 0x838;
const X2APIC_CURRENT_COUNT: u32 = 0x839;
const X2APIC_DIVIDE_CONFIGURATION: u32 = 0x83e;
const X2APIC_SELF_IPI: u32 = 0x83f;
const APIC_BASE_ADDRESS: u64 = 0xfee0_0000;
const APIC_BASE_BSP: u64 = 1 << 8;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const LVT_TIMER_MODE: u32 = 3 << 17;
const ICR_DESTINATION_SHORTHAND: u64 = 3 << 18;
const ICR_SELF: u64 = 1 << 18;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualLocalApic {
    pending: [u64; 4],
    in_service: [u64; 4],
    masked: [u64; 4],
    task_priority: u8,
    apic_base: u64,
    spurious_vector: u16,
    icr: u64,
    lvt_timer: u32,
    lvt_local: [u32; 5],
    timer_initial_count: u32,
    timer_current_count: u32,
    timer_divide_configuration: u8,
    timer_last_tsc: u64,
    timer_remainder: u64,
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
            icr: 0,
            lvt_timer: LVT_MASKED,
            lvt_local: [LVT_MASKED; 5],
            timer_initial_count: 0,
            timer_current_count: 0,
            timer_divide_configuration: 0,
            timer_last_tsc: 0,
            timer_remainder: 0,
        }
    }

    pub const fn new_x2apic() -> Self {
        let mut apic = Self::new();
        apic.apic_base |= APIC_BASE_X2APIC;
        apic
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
            X2APIC_ICR => Ok(self.icr),
            X2APIC_LVT_TIMER => Ok(u64::from(self.lvt_timer)),
            X2APIC_LVT_THERMAL..=X2APIC_LVT_ERROR => Ok(u64::from(
                self.lvt_local[(msr - X2APIC_LVT_THERMAL) as usize],
            )),
            X2APIC_INITIAL_COUNT => Ok(u64::from(self.timer_initial_count)),
            X2APIC_CURRENT_COUNT => Ok(u64::from(self.timer_current_count)),
            X2APIC_DIVIDE_CONFIGURATION => Ok(u64::from(self.timer_divide_configuration)),
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
            X2APIC_SIVR if value & !0x3ff == 0 && value as u8 >= 0x10 => {
                self.spurious_vector = value as u16;
                Ok(ApicMsrEffect::None)
            }
            X2APIC_ESR if value == 0 => Ok(ApicMsrEffect::None),
            X2APIC_ICR => self.write_icr(value),
            X2APIC_LVT_TIMER if value <= u32::MAX.into() => {
                let value = value as u32;
                if value & !(u32::from(u8::MAX) | LVT_MASKED | LVT_TIMER_MODE) != 0
                    || !matches!(value & LVT_TIMER_MODE, 0 | LVT_PERIODIC)
                    || value & LVT_MASKED == 0 && (value as u8) < MIN_DEVICE_VECTOR
                {
                    return Err(ApicMsrError::InvalidValue);
                }
                self.lvt_timer = value;
                Ok(ApicMsrEffect::None)
            }
            X2APIC_LVT_THERMAL..=X2APIC_LVT_ERROR if value <= u32::MAX.into() => {
                self.lvt_local[(msr - X2APIC_LVT_THERMAL) as usize] = value as u32;
                Ok(ApicMsrEffect::None)
            }
            X2APIC_INITIAL_COUNT if value <= u32::MAX.into() => {
                self.timer_initial_count = value as u32;
                self.timer_current_count = value as u32;
                self.timer_remainder = 0;
                Ok(ApicMsrEffect::None)
            }
            X2APIC_DIVIDE_CONFIGURATION if valid_divide_configuration(value) => {
                self.timer_divide_configuration = value as u8;
                self.timer_remainder = 0;
                Ok(ApicMsrEffect::None)
            }
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

    /// Advances the virtual APIC timer using a monotonically increasing TSC.
    /// At most one pending interrupt is recorded; repeated expirations coalesce.
    pub fn update_timer(&mut self, now: u64) -> Result<Option<u8>, InterruptError> {
        if self.timer_last_tsc == 0 {
            self.timer_last_tsc = now;
            return Ok(None);
        }
        let elapsed = now.wrapping_sub(self.timer_last_tsc);
        self.timer_last_tsc = now;
        if self.timer_current_count == 0 {
            self.timer_remainder = 0;
            return Ok(None);
        }
        let divisor = u64::from(timer_divisor(self.timer_divide_configuration));
        let total = self.timer_remainder.saturating_add(elapsed);
        let decrement = total / divisor;
        self.timer_remainder = total % divisor;
        if decrement < u64::from(self.timer_current_count) {
            self.timer_current_count -= decrement as u32;
            return Ok(None);
        }

        if self.lvt_timer & LVT_TIMER_MODE == LVT_PERIODIC && self.timer_initial_count != 0 {
            let overshoot = decrement - u64::from(self.timer_current_count);
            let phase = overshoot % u64::from(self.timer_initial_count);
            self.timer_current_count = if phase == 0 {
                self.timer_initial_count
            } else {
                self.timer_initial_count - phase as u32
            };
        } else {
            self.timer_current_count = 0;
        }

        if self.lvt_timer & LVT_MASKED != 0 {
            return Ok(None);
        }
        let vector = self.lvt_timer as u8;
        self.raise(vector)?;
        Ok(Some(vector))
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

    fn write_icr(&mut self, value: u64) -> Result<ApicMsrEffect, ApicMsrError> {
        let shorthand = value & ICR_DESTINATION_SHORTHAND;
        let destination = value >> 32;
        let reserved = value & 0x0000_0000_fff3_3000;
        let fixed_physical_edge = value & ((7 << 8) | (1 << 11) | (1 << 15)) == 0;
        let targets_self = shorthand == ICR_SELF || shorthand == 0 && destination == 0;
        if reserved != 0 || !fixed_physical_edge || !targets_self {
            return Err(ApicMsrError::Unsupported);
        }
        let vector = value as u8;
        self.raise(vector).map_err(ApicMsrError::Interrupt)?;
        self.icr = value;
        Ok(ApicMsrEffect::SelfIpi(vector))
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

fn valid_divide_configuration(value: u64) -> bool {
    value <= 0xb && value & 0x4 == 0
}

fn timer_divisor(configuration: u8) -> u8 {
    match configuration & 0xb {
        0x0 => 2,
        0x1 => 4,
        0x2 => 8,
        0x3 => 16,
        0x8 => 32,
        0x9 => 64,
        0xa => 128,
        0xb => 1,
        _ => 2,
    }
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

    #[test]
    fn x2apic_local_vector_table_state_round_trips() {
        let mut apic = x2apic();
        for msr in X2APIC_LVT_THERMAL..=X2APIC_LVT_ERROR {
            assert_eq!(apic.read_msr(msr), Ok(u64::from(LVT_MASKED)));
            let value = u64::from(LVT_MASKED | 0xfe);
            assert_eq!(apic.write_msr(msr, value), Ok(ApicMsrEffect::None));
            assert_eq!(apic.read_msr(msr), Ok(value));
        }
    }

    #[test]
    fn self_ipi_and_icr_self_shorthand_raise_interrupts() {
        let mut apic = x2apic();
        assert_eq!(
            apic.write_msr(X2APIC_SELF_IPI, 0x51),
            Ok(ApicMsrEffect::SelfIpi(0x51))
        );
        assert_eq!(apic.next_pending(), Some(0x51));
        apic.accept(0x51).unwrap();
        apic.eoi().unwrap();
        assert_eq!(
            apic.write_msr(X2APIC_ICR, ICR_SELF | 0x52),
            Ok(ApicMsrEffect::SelfIpi(0x52))
        );
        assert_eq!(apic.read_msr(X2APIC_ICR), Ok(ICR_SELF | 0x52));
        assert_eq!(apic.next_pending(), Some(0x52));
        assert_eq!(
            apic.write_msr(X2APIC_ICR, (1_u64 << 32) | 0x53),
            Err(ApicMsrError::Unsupported)
        );
    }

    #[test]
    fn one_shot_timer_counts_down_and_raises_its_vector() {
        let mut apic = x2apic();
        apic.update_timer(100).unwrap();
        apic.write_msr(X2APIC_DIVIDE_CONFIGURATION, 0xb).unwrap();
        apic.write_msr(X2APIC_LVT_TIMER, 0x50).unwrap();
        apic.write_msr(X2APIC_INITIAL_COUNT, 10).unwrap();
        assert_eq!(apic.update_timer(105), Ok(None));
        assert_eq!(apic.read_msr(X2APIC_CURRENT_COUNT), Ok(5));
        assert_eq!(apic.update_timer(110), Ok(Some(0x50)));
        assert_eq!(apic.read_msr(X2APIC_CURRENT_COUNT), Ok(0));
        assert_eq!(apic.next_pending(), Some(0x50));
    }

    #[test]
    fn periodic_timer_reloads_after_each_expiration() {
        let mut apic = x2apic();
        apic.update_timer(100).unwrap();
        apic.write_msr(X2APIC_DIVIDE_CONFIGURATION, 0xb).unwrap();
        apic.write_msr(X2APIC_LVT_TIMER, u64::from(LVT_PERIODIC | 0x50))
            .unwrap();
        apic.write_msr(X2APIC_INITIAL_COUNT, 4).unwrap();
        assert_eq!(apic.update_timer(104), Ok(Some(0x50)));
        assert_eq!(apic.read_msr(X2APIC_CURRENT_COUNT), Ok(4));
        apic.accept(0x50).unwrap();
        apic.eoi().unwrap();
        assert_eq!(apic.update_timer(108), Ok(Some(0x50)));
    }

    #[test]
    fn masked_timer_expires_without_raising_an_interrupt() {
        let mut apic = x2apic();
        apic.update_timer(100).unwrap();
        apic.write_msr(X2APIC_DIVIDE_CONFIGURATION, 0xb).unwrap();
        apic.write_msr(X2APIC_LVT_TIMER, u64::from(LVT_MASKED))
            .unwrap();
        apic.write_msr(X2APIC_INITIAL_COUNT, 4).unwrap();
        assert_eq!(apic.update_timer(104), Ok(None));
        assert_eq!(apic.read_msr(X2APIC_CURRENT_COUNT), Ok(0));
        assert_eq!(apic.next_pending(), None);
    }

    fn x2apic() -> VirtualLocalApic {
        let mut apic = VirtualLocalApic::new();
        let base = apic.read_msr(IA32_APIC_BASE).unwrap();
        apic.write_msr(IA32_APIC_BASE, base | APIC_BASE_X2APIC)
            .unwrap();
        apic
    }
}
