use std::time::{Duration, Instant};

use super::budget::IntervalBudget;
use crate::rtp_::{Bitrate, DataSize};

/// Application Limited Region detector.
///
/// Detects when we're sending significantly less than network capacity,
/// using IntervalBudget to handle bursty encoder traffic over 500ms windows.
///
/// This is critical for real-world usage where encoders produce bursty traffic
/// (keyframes, scene changes, temporal layers, etc.) The IntervalBudget smooths
/// these bursts over a 500ms window to detect sustained application-limited state.
///
/// ## ALR State Transitions
///
/// - **Enter ALR**: When budget ratio > 0.80 (sustained low usage for 500ms)
/// - **Exit ALR**: When budget ratio < 0.50 (increased usage)
/// - The 30% hysteresis gap prevents rapid state flapping
#[derive(Debug)]
pub struct AlrDetector {
    /// Budget tracker with 500ms window
    budget: IntervalBudget,
    /// Current ALR state
    state: AlrState,
    /// Last time we sent bytes
    last_send_time: Option<Instant>,

    // Configuration matching libWebRTC defaults
    /// Target send rate as fraction of estimate (0.65 = 65%)
    bandwidth_usage_ratio: f64,
    /// Budget ratio threshold to enter ALR (0.80 = 80%)
    start_budget_level_ratio: f64,
    /// Budget ratio threshold to exit ALR (0.50 = 50%)
    stop_budget_level_ratio: f64,
}

/// ALR state for the detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlrState {
    /// Not currently in Application Limited Region
    NotInAlr,
    /// In Application Limited Region since the given instant
    InAlr(Instant),
}

impl AlrState {
    fn alr_start_time(&self) -> Option<Instant> {
        match self {
            AlrState::InAlr(start) => Some(*start),
            AlrState::NotInAlr => None,
        }
    }
}

impl AlrDetector {
    /// Create a new ALR detector with libWebRTC default configuration.
    pub fn new() -> Self {
        Self {
            // Start with zero bitrate, will be updated via set_estimated_bitrate()
            // Use can_build_up_underuse=true for ALR detection
            budget: IntervalBudget::new(Bitrate::ZERO, true),
            state: AlrState::NotInAlr,
            last_send_time: None,

            // libWebRTC defaults from alr_detector.cc:30-49
            bandwidth_usage_ratio: 0.65,
            start_budget_level_ratio: 0.80,
            stop_budget_level_ratio: 0.50,
        }
    }

    /// Advance the budget through complete media silence. Keep WebRTC's
    /// packet-driven updates while media is flowing: frequent zero-byte updates
    /// would round away fractional byte credit and change ALR transitions.
    pub fn handle_timeout(&mut self, now: Instant) {
        const IDLE_INTERVAL: Duration = Duration::from_millis(500);
        if self
            .last_send_time
            .is_none_or(|last| now.saturating_duration_since(last) >= IDLE_INTERVAL)
        {
            self.on_bytes_sent(DataSize::ZERO, now);
        }
    }

    /// Update with bytes sent.
    ///
    /// Should be called for every media packet sent (excluding padding and probes).
    /// This updates the IntervalBudget and checks for ALR state transitions.
    pub fn on_bytes_sent(&mut self, bytes: DataSize, now: Instant) {
        // First call - just record time
        let Some(last) = self.last_send_time else {
            self.last_send_time = Some(now);
            return;
        };

        // saturating_duration_since protects against time going backwards
        let delta = now.saturating_duration_since(last);
        self.last_send_time = Some(now);

        // Update budget based on time and usage
        self.budget.use_budget(bytes);
        self.budget.increase_budget(delta);

        // Check for state transitions with hysteresis
        let ratio = self.budget.budget_ratio();

        match self.state {
            AlrState::NotInAlr => {
                if ratio > self.start_budget_level_ratio {
                    // Enter ALR: budget accumulated because we're not sending much
                    self.state = AlrState::InAlr(now);
                    trace!(
                        "ALR: Entered ALR state (ratio={:.3} > threshold={:.3})",
                        ratio, self.start_budget_level_ratio
                    );
                }
            }
            AlrState::InAlr(_) => {
                if ratio < self.stop_budget_level_ratio {
                    // Exit ALR: budget depleted because we're sending more
                    self.state = AlrState::NotInAlr;
                    trace!(
                        "ALR: Exited ALR state (ratio={:.3} < threshold={:.3})",
                        ratio, self.stop_budget_level_ratio
                    );
                }
            }
        }
    }

    /// Update the BWE estimate.
    ///
    /// This adjusts the IntervalBudget's target rate to 65% of the estimate.
    /// Should be called whenever the bandwidth estimate changes.
    pub fn set_estimated_bitrate(&mut self, estimate: Bitrate) {
        let target = estimate * self.bandwidth_usage_ratio;
        self.budget.set_target_rate(target);
    }

    /// Get the time when ALR started, if currently in ALR.
    ///
    /// Returns `Some(Instant)` if in ALR, `None` otherwise.
    /// Used by ProbeControl to determine if periodic ALR probes should be sent.
    pub fn alr_start_time(&self) -> Option<Instant> {
        self.state.alr_start_time()
    }

    #[cfg(test)]
    pub fn budget_ratio(&self) -> f64 {
        self.budget.budget_ratio()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_detects_complete_silence() {
        let now = Instant::now();
        let mut detector = AlrDetector::new();
        detector.set_estimated_bitrate(Bitrate::mbps(1));
        detector.handle_timeout(now);
        detector.handle_timeout(now + Duration::from_millis(500));
        assert!(detector.alr_start_time().is_some());
    }

    #[test]
    fn frequent_timeouts_preserve_media_budget() {
        let now = Instant::now();
        let mut with_timeouts = AlrDetector::new();
        let mut packet_driven = AlrDetector::new();
        for detector in [&mut with_timeouts, &mut packet_driven] {
            detector.set_estimated_bitrate(Bitrate::kbps(250));
            detector.on_bytes_sent(DataSize::ZERO, now);
        }
        for tick in 1..10_000 {
            let now = now + Duration::from_micros(tick * 100);
            with_timeouts.handle_timeout(now);
            if tick % 50 == 0 {
                for detector in [&mut with_timeouts, &mut packet_driven] {
                    detector.on_bytes_sent(DataSize::bytes(100), now);
                }
                assert_eq!(with_timeouts.budget_ratio(), packet_driven.budget_ratio());
                assert_eq!(
                    with_timeouts.alr_start_time(),
                    packet_driven.alr_start_time()
                );
            }
        }
    }

    #[test]
    fn test_alr_not_detected_initially() {
        let alr = AlrDetector::new();
        assert!(alr.alr_start_time().is_none());
    }

    #[test]
    fn test_alr_enter_with_low_usage() {
        let mut alr = AlrDetector::new();
        let now = Instant::now();

        // Set estimate to 1 Mbps (target will be 650 kbps)
        alr.set_estimated_bitrate(Bitrate::mbps(1));

        // Send very little data (way below target)
        // Over 500ms, we should accumulate budget
        for i in 0..50 {
            let t = now + Duration::from_millis(i * 10);
            // Send 100 bytes every 10ms = 80 kbps (way below 650 kbps target)
            alr.on_bytes_sent(DataSize::bytes(100), t);
        }

        // Should enter ALR
        assert!(alr.alr_start_time().is_some());
        assert!(alr.budget_ratio() > 0.80);
    }

    #[test]
    fn test_alr_exit_with_high_usage() {
        let mut alr = AlrDetector::new();
        let now = Instant::now();

        alr.set_estimated_bitrate(Bitrate::mbps(1));

        // Enter ALR with low usage
        for i in 0..50 {
            let t = now + Duration::from_millis(i * 10);
            alr.on_bytes_sent(DataSize::bytes(100), t);
        }
        assert!(alr.alr_start_time().is_some());

        // Now send at high rate to exit ALR
        let start_exit = now + Duration::from_millis(500);
        for i in 0..50 {
            let t = start_exit + Duration::from_millis(i * 10);
            // Send 10,000 bytes every 10ms = 8 Mbps (way above 650 kbps target)
            alr.on_bytes_sent(DataSize::bytes(10_000), t);
        }

        // Should exit ALR
        assert!(alr.alr_start_time().is_none());
        assert!(alr.budget_ratio() < 0.50);
    }

    #[test]
    fn test_alr_hysteresis() {
        let mut alr = AlrDetector::new();
        let now = Instant::now();

        alr.set_estimated_bitrate(Bitrate::mbps(1));

        // Send at exactly 65% (the target) - should not enter ALR
        for i in 0..100 {
            let t = now + Duration::from_millis(i * 10);
            // 65% of 1 Mbps = 650 kbps = 812.5 bytes per 10ms
            alr.on_bytes_sent(DataSize::bytes(812), t);
        }

        // Budget ratio should be near 0, not trigger ALR entry (needs > 0.80)
        assert!(alr.alr_start_time().is_none());
        let ratio = alr.budget_ratio();
        assert!(ratio < 0.80, "ratio={}", ratio);
        assert!(ratio > -0.20, "ratio={}", ratio); // Some tolerance
    }

    #[test]
    fn test_alr_handles_bursts() {
        let mut alr = AlrDetector::new();
        let now = Instant::now();

        alr.set_estimated_bitrate(Bitrate::mbps(1));

        // Simulate bursty encoder: alternate between large and small frames
        let mut time = now;
        for i in 0..100 {
            time += Duration::from_millis(33); // ~30fps

            if i % 10 == 0 {
                // Keyframe: 10x normal size
                alr.on_bytes_sent(DataSize::bytes(50_000), time);
            } else {
                // Normal frame: small
                alr.on_bytes_sent(DataSize::bytes(5_000), time);
            }
        }

        // Average is about 9.5 KB per frame = 285 KB/s = 2.28 Mbps
        // This is above the 650 kbps target, so should NOT be in ALR
        // The IntervalBudget's 500ms window should smooth out the bursts
        assert!(alr.alr_start_time().is_none());
    }

    #[test]
    fn test_alr_estimate_change() {
        let mut alr = AlrDetector::new();
        let now = Instant::now();

        // Start with 1 Mbps estimate (target = 650 kbps)
        alr.set_estimated_bitrate(Bitrate::mbps(1));

        // Send at 200 kbps (way below 650 kbps target) to accumulate budget quickly
        for i in 0..100 {
            let t = now + Duration::from_millis(i * 10);
            alr.on_bytes_sent(DataSize::bytes(250), t); // 200 kbps
        }

        // Should enter ALR after sending well below target
        assert!(alr.alr_start_time().is_some());

        // Now BWE drops to 300 kbps (target becomes 195 kbps)
        alr.set_estimated_bitrate(Bitrate::kbps(300));

        // Continue sending at 250 kbps - now above target (195 kbps)
        let continue_time = now + Duration::from_millis(1000);
        for i in 0..100 {
            let t = continue_time + Duration::from_millis(i * 10);
            alr.on_bytes_sent(DataSize::bytes(312), t); // 250 kbps
        }

        // Should exit ALR (now sending above target)
        assert!(alr.alr_start_time().is_none());
    }

    #[test]
    fn test_alr_should_not_trigger_when_sending_at_target_rate() {
        // This test verifies that ALR doesn't incorrectly trigger when sending
        // at the target rate, even with small time advances between packets.
        let mut alr = AlrDetector::new();
        let now = Instant::now();

        // Set estimate to 8.25 Mbps (target will be 5.36 Mbps at 65%)
        let estimate = Bitrate::kbps(8250);
        alr.set_estimated_bitrate(estimate);

        // Send at exactly 8.25 Mbps with realistic packet timing
        // Packet: 1150 bytes
        // Interval: 1150 bytes × 8 bits / 8,250,000 bps = 1.115ms
        let packet_size = DataSize::bytes(1150);
        let packet_interval = Duration::from_micros(1115); // 1.115ms

        // Send 100 packets over ~111ms
        for i in 0..100 {
            let t = now + packet_interval * i;
            alr.on_bytes_sent(packet_size, t);
        }

        // Should NOT be in ALR - we're sending at 8.25 Mbps, which is above
        // the target threshold of 5.36 Mbps (65% of estimate)
        assert!(
            alr.alr_start_time().is_none(),
            "ALR should not trigger when sending at target rate. Budget ratio: {:.3}",
            alr.budget_ratio()
        );
    }

    #[test]
    fn test_alr_handles_spurious_small_time_advances() {
        // This test verifies that ALR works correctly even when time advances
        // in very small increments (like the test harness's forced 0.2ms advances).
        let mut alr = AlrDetector::new();
        let now = Instant::now();

        // Set estimate to 8.25 Mbps
        let estimate = Bitrate::kbps(8250);
        alr.set_estimated_bitrate(estimate);

        // Simulate sending 1150 byte packets at 8.25 Mbps
        // But with spurious 0.2ms time advances between on_bytes_sent calls
        let packet_size = DataSize::bytes(1150);
        let spurious_advance = Duration::from_micros(200); // 0.2ms

        let mut time = now;
        // Send packets - but accumulate proper time even if called frequently
        for _ in 0..100 {
            alr.on_bytes_sent(packet_size, time);
            time += spurious_advance; // Small spurious advance
        }

        // With 100 packets over 20ms (100 × 0.2ms), we've sent:
        // 115,000 bytes in 20ms = 46 Mbps
        // This is WAY above target, so ALR should NOT trigger
        assert!(
            alr.alr_start_time().is_none(),
            "ALR should not trigger with spurious small advances when sending fast. Budget ratio: {:.3}",
            alr.budget_ratio()
        );
    }
}
