/*
 *  Copyright (c) 2012 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 *
 *  FEC and NACK added bitrate is handled outside class
 */

//! QUIC carries no RTCP, so the receiver-report loss path (`UpdatePacketsLost`, the classic
//! 2%/10% loss controller and its min-bitrate history) and the REMB receiver limit are gone. The
//! start phase upstream ends two seconds after the first receiver report, so here it never does:
//! the delay-based estimate is trusted until the loss-based estimator is ready.

use crate::gcc::remote_bitrate_estimator::CONGESTION_CONTROLLER_MIN_BITRATE;
use crate::gcc::{
    LossBasedBweV2, LossBasedState,
    api::{
        transport::{BandwidthUsage, SentPacket, TransportPacketsFeedback},
        units::{DataRate, TimeDelta, Timestamp},
    },
    experiments::FieldTrials,
};

// WebRTC-Bwe-MaxRttLimit
#[derive(Clone, Debug)]
pub struct RttBasedBackoffConfig {
    disabled: bool,              // Disabled
    configured_limit: TimeDelta, // limit
    drop_fraction: f64,          // fraction
    drop_interval: TimeDelta,    // interval,
    bandwidth_floor: DataRate,   // floor
}

impl Default for RttBasedBackoffConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            configured_limit: TimeDelta::from_seconds(3),
            drop_fraction: 0.8,
            drop_interval: TimeDelta::from_seconds(1),
            bandwidth_floor: DataRate::from_kilobits_per_sec(5),
        }
    }
}

#[derive(Clone)]
pub struct RttBasedBackoff {
    pub drop_fraction: f64,
    pub drop_interval: TimeDelta,
    pub bandwidth_floor: DataRate,
    pub rtt_limit: TimeDelta,
    pub last_propagation_rtt_update: Timestamp,
    pub last_propagation_rtt: TimeDelta,
    pub last_packet_sent: Timestamp,
}

impl RttBasedBackoff {
    pub fn new(config: &RttBasedBackoffConfig) -> Self {
        Self {
            drop_fraction: config.drop_fraction,
            drop_interval: config.drop_interval,
            bandwidth_floor: config.bandwidth_floor,
            rtt_limit: if !config.disabled {
                config.configured_limit
            } else {
                TimeDelta::plus_infinity()
            },
            last_propagation_rtt_update: Timestamp::plus_infinity(),
            last_propagation_rtt: TimeDelta::zero(),
            last_packet_sent: Timestamp::minus_infinity(),
        }
    }

    pub fn update_propagation_rtt(&mut self, at_time: Timestamp, propagation_rtt: TimeDelta) {
        self.last_propagation_rtt_update = at_time;
        self.last_propagation_rtt = propagation_rtt;
    }
    pub fn is_rtt_above_limit(&self) -> bool {
        self.corrected_rtt() > self.rtt_limit
    }

    fn corrected_rtt(&self) -> TimeDelta {
        // Avoid timeout when no packets are being sent.
        let timeout_correction: TimeDelta = std::cmp::max(
            self.last_packet_sent - self.last_propagation_rtt_update,
            TimeDelta::zero(),
        );
        timeout_correction + self.last_propagation_rtt
    }
}

#[derive(Clone)]
pub struct SendSideBandwidthEstimation {
    rtt_backoff: RttBasedBackoff,

    current_target: DataRate,
    min_bitrate_configured: DataRate,
    max_bitrate_configured: DataRate,
    last_low_bitrate_log: Timestamp,

    last_round_trip_time: TimeDelta,

    delay_based_limit: DataRate,
    time_last_decrease: Timestamp,
    loss_based_bandwidth_estimator_v2: LossBasedBweV2,
    loss_based_state: LossBasedState,
}

impl Default for SendSideBandwidthEstimation {
    fn default() -> Self {
        Self::new(&FieldTrials::default())
    }
}

impl SendSideBandwidthEstimation {
    const DEFAULT_MAX_BITRATE: DataRate = DataRate::from_bits_per_sec(1000000000);
    const LOW_BITRATE_LOG_PERIOD: TimeDelta = TimeDelta::from_millis(10000);

    pub fn new(field_trials: &FieldTrials) -> Self {
        let mut loss_based_bandwidth_estimator_v2 =
            LossBasedBweV2::new(field_trials.loss_based_bwe_v2.clone());
        if loss_based_bandwidth_estimator_v2.is_enabled() {
            loss_based_bandwidth_estimator_v2
                .set_min_max_bitrate(CONGESTION_CONTROLLER_MIN_BITRATE, Self::DEFAULT_MAX_BITRATE);
        }

        Self {
            rtt_backoff: RttBasedBackoff::new(&field_trials.max_rtt_limit),
            current_target: DataRate::zero(),
            min_bitrate_configured: CONGESTION_CONTROLLER_MIN_BITRATE,
            max_bitrate_configured: Self::DEFAULT_MAX_BITRATE,
            last_low_bitrate_log: Timestamp::minus_infinity(),
            last_round_trip_time: TimeDelta::zero(),
            delay_based_limit: DataRate::plus_infinity(),
            time_last_decrease: Timestamp::minus_infinity(),
            loss_based_bandwidth_estimator_v2,
            loss_based_state: LossBasedState::DelayBasedEstimate,
        }
    }

    pub fn target_rate(&self) -> DataRate {
        std::cmp::max(self.min_bitrate_configured, self.current_target)
    }
    pub fn loss_based_state(&self) -> LossBasedState {
        self.loss_based_state
    }
    // Return whether the current rtt is higher than the rtt limited configured in
    // RttBasedBackoff.
    pub fn is_rtt_above_limit(&self) -> bool {
        self.rtt_backoff.is_rtt_above_limit()
    }

    pub fn round_trip_time(&self) -> TimeDelta {
        self.last_round_trip_time
    }

    // Call periodically to update estimate.
    pub fn update_estimate(&mut self, at_time: Timestamp) {
        if self.rtt_backoff.is_rtt_above_limit() {
            if at_time - self.time_last_decrease >= self.rtt_backoff.drop_interval
                && self.current_target > self.rtt_backoff.bandwidth_floor
            {
                self.time_last_decrease = at_time;
                let new_bitrate: DataRate = std::cmp::max(
                    self.current_target * self.rtt_backoff.drop_fraction,
                    self.rtt_backoff.bandwidth_floor,
                );
                self.update_target_bitrate(new_bitrate, at_time);
                return;
            }
            // TODO(srte): This is likely redundant in most cases.
            self.apply_target_limits(at_time);
            return;
        }

        // We trust the delay-based estimate until the loss-based estimator is ready, to allow
        // startup bitrate probing.
        if !self
            .loss_based_bandwidth_estimator_v2
            .ready_to_use_in_start_phase()
            && self.delay_based_limit.is_finite()
            && self.delay_based_limit > self.current_target
        {
            self.update_target_bitrate(self.delay_based_limit, at_time);
            return;
        }

        if self.loss_based_bandwidth_estimator_v2_ready_for_use() {
            let result = self
                .loss_based_bandwidth_estimator_v2
                .get_loss_based_result();
            self.loss_based_state = result.state;
            self.update_target_bitrate(result.bandwidth_estimate, at_time);
            return;
        }

        // TODO(srte): This is likely redundant in most cases.
        self.apply_target_limits(at_time);
    }
    pub fn on_sent_packet(&mut self, sent_packet: &SentPacket) {
        // Only feedback-triggering packets will be reported here.
        self.rtt_backoff.last_packet_sent = sent_packet.send_time;
    }
    pub fn update_propagation_rtt(&mut self, at_time: Timestamp, propagation_rtt: TimeDelta) {
        self.rtt_backoff
            .update_propagation_rtt(at_time, propagation_rtt);
    }
    // Call when a new delay-based estimate is available.
    pub fn update_delay_based_estimate(&mut self, at_time: Timestamp, bitrate: DataRate) {
        // TODO(srte): Ensure caller passes plus_infinity, not zero, to represent no
        // limitation.
        self.delay_based_limit = if bitrate.is_zero() {
            DataRate::plus_infinity()
        } else {
            bitrate
        };
        self.apply_target_limits(at_time);
    }
    pub fn update_rtt(&mut self, rtt: TimeDelta) {
        if rtt > TimeDelta::zero() {
            self.last_round_trip_time = rtt;
        }
    }
    pub fn set_bitrates(
        &mut self,
        send_bitrate: Option<DataRate>,
        min_bitrate: DataRate,
        max_bitrate: DataRate,
        at_time: Timestamp,
    ) {
        self.set_min_max_bitrate(min_bitrate, max_bitrate);
        if let Some(send_bitrate) = send_bitrate {
            self.set_send_bitrate(send_bitrate, at_time);
        }
    }
    pub fn set_send_bitrate(&mut self, bitrate: DataRate, at_time: Timestamp) {
        debug_assert!(bitrate > DataRate::zero());
        // Reset to avoid being capped by the estimate.
        self.delay_based_limit = DataRate::plus_infinity();
        self.update_target_bitrate(bitrate, at_time);
    }
    pub fn set_min_max_bitrate(&mut self, min_bitrate: DataRate, max_bitrate: DataRate) {
        self.min_bitrate_configured = std::cmp::max(min_bitrate, CONGESTION_CONTROLLER_MIN_BITRATE);
        if max_bitrate > DataRate::zero() && max_bitrate.is_finite() {
            self.max_bitrate_configured = std::cmp::max(self.min_bitrate_configured, max_bitrate);
        } else {
            self.max_bitrate_configured = Self::DEFAULT_MAX_BITRATE;
        }
        self.loss_based_bandwidth_estimator_v2
            .set_min_max_bitrate(self.min_bitrate_configured, self.max_bitrate_configured);
    }

    pub fn get_min_bitrate(&self) -> i64 {
        self.min_bitrate_configured.bps()
    }

    pub fn set_acknowledged_rate(&mut self, acknowledged_rate: Option<DataRate>) {
        let Some(acknowledged_rate) = acknowledged_rate else {
            return;
        };
        if self.loss_based_bandwidth_estimator_v2.is_enabled() {
            self.loss_based_bandwidth_estimator_v2
                .set_acknowledged_bitrate(acknowledged_rate);
        }
    }
    pub fn update_loss_based_estimator(
        &mut self,
        report: &TransportPacketsFeedback,
        _delay_detector_state: BandwidthUsage,
        _probe_bitrate: Option<DataRate>,
        in_alr: bool,
    ) {
        if self.loss_based_bandwidth_estimator_v2.is_enabled() {
            self.loss_based_bandwidth_estimator_v2
                .update_bandwidth_estimate(
                    &report.packet_feedbacks,
                    self.delay_based_limit,
                    in_alr,
                );
            self.update_estimate(report.feedback_time);
        }
    }

    // Gets the upper limit for the target bitrate. This is the minimum of the
    // delay based limit and the configured max.
    fn get_upper_limit(&self) -> DataRate {
        std::cmp::min(self.delay_based_limit, self.max_bitrate_configured)
    }
    // Prints a warning if `bitrate` if sufficiently long time has past since last
    // warning.
    fn maybe_log_low_bitrate_warning(&mut self, bitrate: DataRate, at_time: Timestamp) {
        if at_time - self.last_low_bitrate_log > Self::LOW_BITRATE_LOG_PERIOD {
            tracing::debug!(
                "Estimated available bandwidth {:?} is below configured min bitrate {:?}.",
                bitrate,
                self.min_bitrate_configured
            );
            self.last_low_bitrate_log = at_time;
        }
    }

    // Cap `bitrate` to [self.min_bitrate_configured, max_bitrate_configured_] and
    // set `current_bitrate_` to the capped value.
    fn update_target_bitrate(&mut self, mut new_bitrate: DataRate, at_time: Timestamp) {
        new_bitrate = std::cmp::min(new_bitrate, self.get_upper_limit());
        if new_bitrate < self.min_bitrate_configured {
            self.maybe_log_low_bitrate_warning(new_bitrate, at_time);
            new_bitrate = self.min_bitrate_configured;
        }
        self.current_target = new_bitrate;
    }
    // Applies lower and upper bounds to the current target rate.
    // TODO(srte): This seems to be called even when limits haven't changed, that
    // should be cleaned up.
    fn apply_target_limits(&mut self, at_time: Timestamp) {
        self.update_target_bitrate(self.current_target, at_time);
    }

    fn loss_based_bandwidth_estimator_v2_ready_for_use(&self) -> bool {
        self.loss_based_bandwidth_estimator_v2.is_ready()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn initial_delay_based_bwe_with_probing() {
        let mut bwe = SendSideBandwidthEstimation::default();
        let now = Timestamp::from_millis(0);
        bwe.set_min_max_bitrate(
            DataRate::from_bits_per_sec(100000),
            DataRate::from_bits_per_sec(1500000),
        );
        bwe.set_send_bitrate(DataRate::from_bits_per_sec(200000), now);
        bwe.update_rtt(TimeDelta::from_millis(50));

        // The delay-based estimate applies at once while the loss-based estimator has no
        // observations.
        bwe.update_delay_based_estimate(now, DataRate::from_bits_per_sec(1000000));
        bwe.update_estimate(now);
        assert_eq!(1000000, bwe.target_rate().bps());
    }

    #[test]
    fn setting_send_bitrate_overrides_delay_based_estimate() {
        let mut bwe = SendSideBandwidthEstimation::default();
        const MIN_BITRATE_BPS: i64 = 10000;
        const MAX_BITRATE_BPS: i64 = 10000000;
        const INITIAL_BITRATE_BPS: i64 = 300000;
        const DELAY_BASED_BITRATE_BPS: i64 = 350000;
        const FORCED_HIGH_BITRATE: i64 = 2500000;

        let now_ms: i64 = 0;

        bwe.set_min_max_bitrate(
            DataRate::from_bits_per_sec(MIN_BITRATE_BPS),
            DataRate::from_bits_per_sec(MAX_BITRATE_BPS),
        );
        bwe.set_send_bitrate(
            DataRate::from_bits_per_sec(INITIAL_BITRATE_BPS),
            Timestamp::from_millis(now_ms),
        );

        bwe.update_delay_based_estimate(
            Timestamp::from_millis(now_ms),
            DataRate::from_bits_per_sec(DELAY_BASED_BITRATE_BPS),
        );
        bwe.update_estimate(Timestamp::from_millis(now_ms));
        assert!(bwe.target_rate().bps() >= INITIAL_BITRATE_BPS);
        assert!(bwe.target_rate().bps() <= DELAY_BASED_BITRATE_BPS);

        bwe.set_send_bitrate(
            DataRate::from_bits_per_sec(FORCED_HIGH_BITRATE),
            Timestamp::from_millis(now_ms),
        );
        assert_eq!(bwe.target_rate().bps(), FORCED_HIGH_BITRATE);
    }

    #[test]
    fn default_enabled() {
        let rtt_backoff = RttBasedBackoff::new(&RttBasedBackoffConfig::default());
        assert!(rtt_backoff.rtt_limit.is_finite());
    }

    #[test]
    fn can_be_disabled() {
        let config = RttBasedBackoffConfig {
            disabled: true,
            ..Default::default()
        };

        let rtt_backoff = RttBasedBackoff::new(&config);
        assert!(rtt_backoff.rtt_limit.is_plus_infinity());
    }

    #[test]
    fn rtt_is_above_limit_if_rtt_greater_than_limit() {
        let mut bwe = SendSideBandwidthEstimation::default();
        const MIN_BITRATE_BPS: i64 = 10000;
        const MAX_BITRATE_BPS: i64 = 10000000;
        const INITIAL_BITRATE_BPS: i64 = 300000;
        let now_ms: i64 = 0;
        bwe.set_min_max_bitrate(
            DataRate::from_bits_per_sec(MIN_BITRATE_BPS),
            DataRate::from_bits_per_sec(MAX_BITRATE_BPS),
        );
        bwe.set_send_bitrate(
            DataRate::from_bits_per_sec(INITIAL_BITRATE_BPS),
            Timestamp::from_millis(now_ms),
        );
        bwe.update_propagation_rtt(
            /*at_time=*/ Timestamp::from_millis(now_ms),
            /*propagation_rtt=*/ TimeDelta::from_millis(5000),
        );
        assert!(bwe.is_rtt_above_limit());
    }

    #[test]
    fn rtt_is_below_limit_if_rtt_less_than_limit() {
        let mut bwe = SendSideBandwidthEstimation::default();
        const MIN_BITRATE_BPS: i64 = 10000;
        const MAX_BITRATE_BPS: i64 = 10000000;
        const INITIAL_BITRATE_BPS: i64 = 300000;
        let now_ms: i64 = 0;
        bwe.set_min_max_bitrate(
            DataRate::from_bits_per_sec(MIN_BITRATE_BPS),
            DataRate::from_bits_per_sec(MAX_BITRATE_BPS),
        );
        bwe.set_send_bitrate(
            DataRate::from_bits_per_sec(INITIAL_BITRATE_BPS),
            Timestamp::from_millis(now_ms),
        );
        bwe.update_propagation_rtt(
            /*at_time=*/ Timestamp::from_millis(now_ms),
            /*propagation_rtt=*/ TimeDelta::from_millis(1000),
        );
        assert!(!bwe.is_rtt_above_limit());
    }
}
