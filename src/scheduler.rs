#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WaitState {
    #[default]
    Running,
    Interrupt,
    EventChannel,
}

impl WaitState {
    /// Device IRQs remain queued while an EventWait awaits a port result.
    pub fn wake_for_interrupt(&mut self) -> bool {
        if *self == Self::EventChannel {
            return false;
        }
        *self = Self::Running;
        true
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CooperativeScheduler {
    cursor: usize,
    resume: Option<usize>,
    slice_start: u64,
}

impl CooperativeScheduler {
    pub const fn new() -> Self {
        Self {
            cursor: 0,
            resume: None,
            slice_start: 0,
        }
    }

    pub fn resume_after_emulation(&mut self, index: usize) {
        self.resume = Some(index);
    }

    /// Emulation continues the current slice; it never renews its time budget.
    pub fn next_in_slice(&mut self, runnable: &[bool], now: u64, budget: u64) -> Option<usize> {
        if let Some(index) = self.resume.take() {
            if runnable.get(index) == Some(&true) && now.wrapping_sub(self.slice_start) < budget {
                return Some(index);
            }
        }
        self.slice_start = now;
        self.next(runnable)
    }

    pub fn next(&mut self, runnable: &[bool]) -> Option<usize> {
        if runnable.is_empty() {
            return None;
        }
        for offset in 0..runnable.len() {
            let index = (self.cursor + offset) % runnable.len();
            if runnable[index] {
                self.cursor = (index + 1) % runnable.len();
                return Some(index);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulation_preserves_but_does_not_extend_slice() {
        let mut scheduler = CooperativeScheduler::new();
        let runnable = [true, true];
        assert_eq!(scheduler.next_in_slice(&runnable, 100, 10), Some(0));
        scheduler.resume_after_emulation(0);
        assert_eq!(scheduler.next_in_slice(&runnable, 109, 10), Some(0));
        scheduler.resume_after_emulation(0);
        assert_eq!(scheduler.next_in_slice(&runnable, 110, 10), Some(1));
        assert_eq!(scheduler.next_in_slice(&runnable, 111, 10), Some(0));
        scheduler.resume_after_emulation(0);
        assert_eq!(scheduler.next_in_slice(&[false, true], 112, 10), Some(1));
    }

    #[test]
    fn unknown_clock_disables_slice_continuation() {
        let mut scheduler = CooperativeScheduler::new();
        assert_eq!(scheduler.next_in_slice(&[true, true], 0, 0), Some(0));
        scheduler.resume_after_emulation(0);
        assert_eq!(scheduler.next_in_slice(&[true, true], 0, 0), Some(1));
    }

    #[test]
    fn repeated_nonblocking_returns_cannot_starve_peer() {
        let mut scheduler = CooperativeScheduler::new();
        let runnable = [true, true];
        assert_eq!(scheduler.next_in_slice(&runnable, 0, 10), Some(0));
        for now in 1..10 {
            scheduler.resume_after_emulation(0);
            assert_eq!(scheduler.next_in_slice(&runnable, now, 10), Some(0));
        }
        scheduler.resume_after_emulation(0);
        assert_eq!(scheduler.next_in_slice(&runnable, 10, 10), Some(1));
        // An explicit yield does not request slice continuation.
        assert_eq!(scheduler.next_in_slice(&runnable, 11, 10), Some(0));
    }

    #[test]
    fn device_irq_does_not_complete_event_wait() {
        let mut state = WaitState::EventChannel;
        assert!(!state.wake_for_interrupt());
        assert_eq!(state, WaitState::EventChannel);
        state = WaitState::Interrupt;
        assert!(state.wake_for_interrupt());
        assert_eq!(state, WaitState::Running);
        assert!(state.wake_for_interrupt());
    }

    #[test]
    fn rotates_over_runnable_vcpus() {
        let mut scheduler = CooperativeScheduler::new();
        let runnable = [true, true, false];
        assert_eq!(scheduler.next(&runnable), Some(0));
        assert_eq!(scheduler.next(&runnable), Some(1));
        assert_eq!(scheduler.next(&runnable), Some(0));
    }

    #[test]
    fn returns_none_when_all_vcpus_are_stopped() {
        let mut scheduler = CooperativeScheduler::new();
        assert_eq!(scheduler.next(&[false, false]), None);
    }
}
