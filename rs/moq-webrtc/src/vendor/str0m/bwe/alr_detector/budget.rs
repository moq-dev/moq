//! Interval budget tracker for ALR detection.
//!
//! Port of libWebRTC's IntervalBudget class from
//! `webrtc/modules/pacing/interval_budget.{h,cc}`

use std::time::Duration;

use crate::rtp_::{Bitrate, DataSize};

/// Leaky bucket budget tracker over a fixed time window.
///
/// Used by ALR detector to handle bursty traffic (encoders, etc.)
/// over a 500ms window. Tracks how much "budget" we have left
/// based on target rate vs. actual send rate.
///
/// This implements the same logic as libWebRTC's IntervalBudget class
/// to ensure ALR detection behaves identically when handling bursty
/// encoder traffic (keyframes, scene changes, temporal layers, etc.)
#[derive(Debug, Clone)]
pub struct IntervalBudget {
    /// Target bitrate for budget calculation
    target_rate: Bitrate,
    /// Maximum bytes that can accumulate in the budget
    max_bytes_in_budget: DataSize,
    /// Current bytes remaining in budget (can be negative = debt)
    bytes_remaining: DataSize,
    /// Whether underuse can build up credit across intervals
    can_build_up_underuse: bool,
}

impl IntervalBudget {
    /// Window size for budget tracking
    const WINDOW: Duration = Duration::from_millis(500);

    /// Create a new IntervalBudget with the specified target rate.
    pub fn new(target_rate: Bitrate, can_build_up_underuse: bool) -> Self {
        let max_bytes_in_budget = Self::calculate_max_bytes(target_rate);

        Self {
            target_rate,
            max_bytes_in_budget,
            bytes_remaining: DataSize::ZERO,
            can_build_up_underuse,
        }
    }

    /// Update the target bitrate.
    ///
    /// Called when the BWE estimate changes. Clamps existing bytes_remaining
    /// to the new max_bytes_in_budget range.
    pub fn set_target_rate(&mut self, target_rate: Bitrate) {
        self.target_rate = target_rate;
        self.max_bytes_in_budget = Self::calculate_max_bytes(target_rate);

        // Clamp bytes_remaining to new budget limits
        let max = self.max_bytes_in_budget;
        let neg_max = max * -1i64;
        self.bytes_remaining = DataSize::bytes(
            self.bytes_remaining
                .as_bytes_i64()
                .clamp(neg_max, max.as_bytes_i64()),
        );
    }

    /// Add budget based on elapsed time.
    ///
    /// Increases the budget by `target_rate * delta_time`.
    /// Behavior depends on `can_build_up_underuse`:
    /// - If true: underuse accumulates (for ALR detection)
    /// - If false: budget resets each interval (for pacing)
    pub fn increase_budget(&mut self, delta_time: Duration) {
        let bytes = self.target_rate * delta_time;

        let max = self.max_bytes_in_budget;
        if self.bytes_remaining.as_bytes_i64() < 0 || self.can_build_up_underuse {
            // We overused last interval, compensate this interval
            // OR we allow building up underuse credit
            self.bytes_remaining = DataSize::bytes(
                (self.bytes_remaining + bytes)
                    .as_bytes_i64()
                    .min(max.as_bytes_i64()),
            );
        } else {
            // If we underused last interval we can't use it this interval
            self.bytes_remaining = DataSize::bytes(bytes.as_bytes_i64().min(max.as_bytes_i64()));
        }
    }

    /// Consume budget by the specified number of bytes.
    ///
    /// This represents sending data. The budget can go negative (debt).
    pub fn use_budget(&mut self, bytes: DataSize) {
        let max = self.max_bytes_in_budget;
        let neg_max = max * -1i64;
        self.bytes_remaining =
            DataSize::bytes((self.bytes_remaining - bytes).as_bytes_i64().max(neg_max));
    }

    /// Get the current budget ratio.
    ///
    /// Returns a value in the range [-1.0, 1.0]:
    /// - 1.0 = full budget (maximum underuse)
    /// - 0.0 = neutral (sending exactly at target rate)
    /// - -1.0 = maximum debt (maximum overuse)
    ///
    /// Used by ALR detector for hysteresis thresholds:
    /// - Enter ALR when ratio > 0.80 (sustained low usage)
    /// - Exit ALR when ratio < 0.50 (increased usage)
    pub fn budget_ratio(&self) -> f64 {
        let max = self.max_bytes_in_budget.as_bytes_i64();
        if max == 0 {
            return 0.0;
        }
        self.bytes_remaining.as_bytes_i64() as f64 / max as f64
    }

    /// Calculate max_bytes_in_budget from target rate
    fn calculate_max_bytes(target_rate: Bitrate) -> DataSize {
        target_rate * Self::WINDOW
    }

    #[cfg(test)]
    pub fn target_rate(&self) -> Bitrate {
        self.target_rate
    }

    #[cfg(test)]
    pub fn bytes_remaining(&self) -> i64 {
        self.bytes_remaining.as_bytes_i64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_budget_is_zero() {
        let budget = IntervalBudget::new(Bitrate::kbps(300), true);
        assert_eq!(budget.bytes_remaining(), 0);
        assert_eq!(budget.budget_ratio(), 0.0);
    }

    #[test]
    fn test_increase_budget_accumulates() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);

        // Add 100ms worth of budget: 300 kbps * 100ms = 3750 bytes
        budget.increase_budget(Duration::from_millis(100));
        assert_eq!(budget.bytes_remaining(), 3750);

        // Budget ratio should be positive (underuse)
        assert!(budget.budget_ratio() > 0.0);
    }

    #[test]
    fn test_use_budget_creates_debt() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);

        // Use 5000 bytes
        budget.use_budget(DataSize::bytes(5000));
        assert_eq!(budget.bytes_remaining(), -5000);

        // Budget ratio should be negative (overuse)
        assert!(budget.budget_ratio() < 0.0);
    }

    #[test]
    fn test_budget_ratio_clamped() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);
        let max_bytes = (Bitrate::kbps(300) * Duration::from_millis(500)).as_bytes_i64();

        // Fill to max
        budget.increase_budget(Duration::from_millis(500));
        assert_eq!(budget.budget_ratio(), 1.0);

        // Try to add more - should stay at 1.0
        budget.increase_budget(Duration::from_millis(100));
        assert_eq!(budget.budget_ratio(), 1.0);

        // Use all budget plus create max debt
        budget.use_budget(DataSize::bytes(max_bytes * 2));
        assert_eq!(budget.budget_ratio(), -1.0);
    }

    #[test]
    fn test_can_build_up_underuse_false() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), false);

        // Add budget
        budget.increase_budget(Duration::from_millis(100));
        let after_first = budget.bytes_remaining();
        assert!(after_first > 0);

        // Add more budget - should reset, not accumulate
        budget.increase_budget(Duration::from_millis(100));
        assert_eq!(budget.bytes_remaining(), 3750); // Only the latest 100ms

        // Not the accumulated 7500
        assert_ne!(budget.bytes_remaining(), after_first + 3750);
    }

    #[test]
    fn test_can_build_up_underuse_true() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);

        // Add budget
        budget.increase_budget(Duration::from_millis(100));
        let after_first = budget.bytes_remaining();

        // Add more budget - should accumulate
        budget.increase_budget(Duration::from_millis(100));
        assert_eq!(budget.bytes_remaining(), after_first + 3750);
    }

    #[test]
    fn test_debt_recovery() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);

        // Create debt
        budget.use_budget(DataSize::bytes(5000));
        assert_eq!(budget.bytes_remaining(), -5000);

        // Add budget to recover from debt
        budget.increase_budget(Duration::from_millis(200));
        let expected_recovery = 3750 * 2; // 200ms worth
        assert_eq!(budget.bytes_remaining(), -5000 + expected_recovery);
    }

    #[test]
    fn test_set_target_rate_clamps_budget() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);

        // Build up budget at 300 kbps
        budget.increase_budget(Duration::from_millis(500));
        let old_max = (Bitrate::kbps(300) * Duration::from_millis(500)).as_bytes_i64();
        assert_eq!(budget.bytes_remaining(), old_max);

        // Reduce target rate - budget should be clamped to new max
        budget.set_target_rate(Bitrate::kbps(150));
        let new_max = (Bitrate::kbps(150) * Duration::from_millis(500)).as_bytes_i64();
        assert_eq!(budget.bytes_remaining(), new_max);
        assert!(new_max < old_max);
    }

    #[test]
    fn test_alr_hysteresis_thresholds() {
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);

        // Simulate ALR entry threshold (80% budget ratio)
        let target_bytes =
            (0.8 * (Bitrate::kbps(300) * Duration::from_millis(500)).as_bytes_i64() as f64) as i64;

        // Accumulate budget to 80% threshold
        while budget.bytes_remaining() < target_bytes {
            budget.increase_budget(Duration::from_millis(50));
        }

        assert!(budget.budget_ratio() >= 0.80);

        // Simulate ALR exit threshold (50% budget ratio)
        // Use budget to bring ratio down
        let use_amount =
            (0.30 * (Bitrate::kbps(300) * Duration::from_millis(500)).as_bytes_i64() as f64) as i64;
        budget.use_budget(DataSize::bytes(use_amount));

        assert!(budget.budget_ratio() < 0.80);
        assert!(budget.budget_ratio() > 0.40); // Should be around 50%
    }

    #[test]
    fn test_target_rate_getter() {
        let budget = IntervalBudget::new(Bitrate::kbps(500), true);
        assert_eq!(budget.target_rate(), Bitrate::kbps(500));

        // After modifying target rate
        let mut budget = IntervalBudget::new(Bitrate::kbps(300), true);
        budget.set_target_rate(Bitrate::kbps(600));
        assert_eq!(budget.target_rate(), Bitrate::kbps(600));
    }
}
