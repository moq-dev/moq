use std::collections::VecDeque;
use std::iter::Sum;
use std::ops::{AddAssign, SubAssign};
use std::time::{Duration, Instant};

/// Holds a history values of type T for up to a certain Duration, as well as the
/// cumulated (total) value.
#[derive(Debug)]
pub(crate) struct ValueHistory<T> {
    value: T,
    history: VecDeque<(Instant, T)>,
    max_time: Duration,
}

const DEFAULT_VALUE_HISTORY_DURATION: Duration = Duration::from_secs(1);

impl<T: Default> Default for ValueHistory<T> {
    fn default() -> Self {
        Self {
            value: Default::default(),
            history: Default::default(),
            max_time: DEFAULT_VALUE_HISTORY_DURATION,
        }
    }
}

impl<T> ValueHistory<T>
where
    T: Copy + AddAssign + SubAssign + Sum,
{
    /// Adds a timed value
    /// Note: time should always monotonically increase in subsequent calls to add()
    pub fn push(&mut self, t: Instant, v: T) {
        self.value += v;
        self.history.push_back((t, v));
    }

    /// Returns the sum of all values in the history up to max_time. Might
    /// return stale value unless [`ValueHistory::purge_old`] is called before.
    pub fn sum(&self) -> T {
        self.value
    }

    /// Recalculates sum purging values older than `now - max_time`.
    pub fn purge_old(&mut self, now: Instant) {
        while {
            let Some(front_t) = self.history.front().map(|v| v.0) else {
                return;
            };
            now.duration_since(front_t) > self.max_time
        } {
            if let Some((_, v)) = self.history.pop_front() {
                self.value -= v;
            }
        }
    }
}

#[allow(clippy::unchecked_time_subtraction)]
#[cfg(test)]
mod test {
    use std::time::{Duration, Instant};

    use super::ValueHistory;

    #[test]
    fn with_value_test() {
        let now = Instant::now();

        let mut h = ValueHistory {
            value: 11,
            max_time: Duration::from_secs(1),
            ..Default::default()
        };

        assert_eq!(h.sum(), 11);
        h.purge_old(now);
        assert_eq!(h.sum(), 11);
        h.push(now - Duration::from_millis(1500), 22);
        h.push(now - Duration::from_millis(500), 22);
        assert_eq!(h.sum(), 11 + 22 + 22);
        h.purge_old(now);
        assert_eq!(h.sum(), 11 + 22);
        h.push(now, 0);
        assert_eq!(h.sum(), 11 + 22);
    }

    #[test]
    fn test() {
        let now = Instant::now();
        let mut h = ValueHistory::default();

        assert_eq!(h.sum(), 0);
        h.push(now - Duration::from_millis(1500), 22);
        assert_eq!(h.sum(), 22);
        h.purge_old(now);
        assert_eq!(h.sum(), 0);
        h.push(now - Duration::from_millis(700), 22);
        h.push(now - Duration::from_millis(500), 33);
        assert_eq!(h.sum(), 22 + 33);
        h.purge_old(now);
        assert_eq!(h.sum(), 22 + 33);

        h.purge_old(now + Duration::from_millis(400));
        assert_eq!(h.sum(), 33);
        h.purge_old(now + Duration::from_millis(600));
        assert_eq!(h.sum(), 0);
    }
}
