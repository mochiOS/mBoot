#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CooperativeScheduler {
    cursor: usize,
}

impl CooperativeScheduler {
    pub const fn new() -> Self {
        Self { cursor: 0 }
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
