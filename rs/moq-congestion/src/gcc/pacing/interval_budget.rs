pub struct IntervalBudget {
    target_rate_kbps: i64,
    max_bytes_in_budget: i64,
    bytes_remaining: i64,
    can_build_up_underuse: bool,
}

impl IntervalBudget {
    const WINDOW_MS: i64 = 500;

    pub fn new(initial_target_rate_kbps: i64, can_build_up_underuse: bool) -> Self {
        let mut this = Self {
            bytes_remaining: 0,
            can_build_up_underuse,
            target_rate_kbps: 0,
            max_bytes_in_budget: 0,
        };
        this.set_target_rate_kbps(initial_target_rate_kbps);
        this
    }

    pub fn set_target_rate_kbps(&mut self, target_rate_kbps: i64) {
        self.target_rate_kbps = target_rate_kbps;
        self.max_bytes_in_budget = (Self::WINDOW_MS * self.target_rate_kbps) / 8;
        self.bytes_remaining = std::cmp::min(
            std::cmp::max(-self.max_bytes_in_budget, self.bytes_remaining),
            self.max_bytes_in_budget,
        );
    }

    pub fn increase_budget(&mut self, delta_time_ms: i64) {
        let bytes: i64 = self.target_rate_kbps * delta_time_ms / 8;
        if self.bytes_remaining < 0 || self.can_build_up_underuse {
            // We overused last interval, compensate this interval.
            self.bytes_remaining =
                std::cmp::min(self.bytes_remaining + bytes, self.max_bytes_in_budget);
        } else {
            // If we underused last interval we can't use it this interval.
            self.bytes_remaining = std::cmp::min(bytes, self.max_bytes_in_budget);
        }
    }

    pub fn use_budget(&mut self, bytes: usize) {
        self.bytes_remaining = std::cmp::max(
            self.bytes_remaining - bytes as i64,
            -self.max_bytes_in_budget,
        );
    }

    /* unused
    pub fn bytes_remaining(&self) -> usize {
        self.bytes_remaining.try_into().unwrap_or(0)
    }
    */

    pub fn budget_ratio(&self) -> f64 {
        if self.max_bytes_in_budget == 0 {
            return 0.0;
        }
        self.bytes_remaining as f64 / self.max_bytes_in_budget as f64
    }

    /* unused
    pub fn target_rate_kbps(&self) -> i64 {
        self.target_rate_kbps
    }
    */
}
