/*
 *  Copyright (c) 2014 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::{
    api::{
        transport::{BandwidthUsage, NetworkStateEstimate},
        units::{DataRate, DataSize, TimeDelta, Timestamp},
    },
    experiments::FieldTrials,
    LinkCapacityEstimator,
    remote_bitrate_estimator::BITRATE_WINDOW,
};

use super::{RateControlInput, CONGESTION_CONTROLLER_MIN_BITRATE};

// WebRTC-BweBackOffFactor
#[derive(Debug, Clone)]
pub struct BweBackOffFactor {
    pub backoff_factor: f64, // Enabled-*
}

impl Default for BweBackOffFactor {
    fn default() -> Self {
        Self {
            backoff_factor: Self::DEFAULT_BACKOFF_FACTOR,
        }
    }
}

impl BweBackOffFactor {
    const DEFAULT_BACKOFF_FACTOR: f64 = 0.85;

    pub fn validate(&mut self) {
        if self.backoff_factor >= 1.0 {
            tracing::warn!("Back-off factor must be less than 1.");
        } else if self.backoff_factor <= 0.0 {
            tracing::warn!("Back-off factor must be greater than 0.");
        } else {
            return;
        }

        tracing::warn!("Failed to parse parameters for AimdRateControl experiment from field trial string. Using default.");
        self.backoff_factor = Self::DEFAULT_BACKOFF_FACTOR
    }
}

// WebRTC-Bwe-EstimateBoundedIncrease
#[derive(Debug, Clone)]
pub struct EstimateBoundedIncrease {
    pub disable_estimate_bounded_increase: bool,
    pub use_current_estimate_as_min_upper_bound: bool,
}

impl Default for EstimateBoundedIncrease {
    fn default() -> Self {
        Self {
            disable_estimate_bounded_increase: false,
            use_current_estimate_as_min_upper_bound: true,
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum RateControlState {
    Hold,
    Increase,
    Decrease,
}

// A rate control implementation based on additive increases of
// bitrate when no over-use is detected and multiplicative decreases when
// over-uses are detected. When we think the available bandwidth has changes or
// is unknown, we will switch to a "slow-start mode" where we increase
// multiplicatively.
pub struct AimdRateControl {
    min_configured_bitrate: DataRate,
    current_bitrate: DataRate,
    latest_estimated_throughput: DataRate,
    link_capacity: LinkCapacityEstimator,
    network_estimate: Option<NetworkStateEstimate>,
    rate_control_state: RateControlState,
    time_last_bitrate_change: Timestamp,
    time_last_bitrate_decrease: Timestamp,
    time_first_throughput_estimate: Timestamp,
    bitrate_is_initialized: bool,
    beta: f64,
    in_alr: bool,
    rtt: TimeDelta,
    send_side: bool,
    // Allow the delay based estimate to only increase as long as application
    // limited region (alr) is not detected.
    no_bitrate_increase_in_alr: bool,
    last_decrease: Option<DataRate>,

    // Field trials
    disable_estimate_bounded_increase: bool,
    use_current_estimate_as_min_upper_bound: bool,
}

impl Default for AimdRateControl {
    fn default() -> Self {
        let max_configured_bitrate = DataRate::from_kilobits_per_sec(30000);
        Self {
            min_configured_bitrate: CONGESTION_CONTROLLER_MIN_BITRATE,
            current_bitrate: max_configured_bitrate,
            latest_estimated_throughput: max_configured_bitrate,
            link_capacity: LinkCapacityEstimator::new(),
            rate_control_state: RateControlState::Hold,
            time_last_bitrate_change: Timestamp::minus_infinity(),
            time_last_bitrate_decrease: Timestamp::minus_infinity(),
            time_first_throughput_estimate: Timestamp::minus_infinity(),
            bitrate_is_initialized: false,
            beta: Self::DEFAULT_BACKOFF_FACTOR,
            in_alr: false,
            rtt: Self::DEFAULT_RTT,
            send_side: false,
            no_bitrate_increase_in_alr: false,
            network_estimate: None,
            last_decrease: None,
            disable_estimate_bounded_increase: false,
            use_current_estimate_as_min_upper_bound: true,
        }
    }
}

impl AimdRateControl {
    const DEFAULT_RTT: TimeDelta = TimeDelta::from_millis(200);
    const DEFAULT_BACKOFF_FACTOR: f64 = 0.85;

    pub fn new(field_trials: &FieldTrials, send_side: bool) -> Self {
        tracing::debug!(
            "Using aimd rate control with back off factor {}",
            field_trials.bwe_back_off_factor.backoff_factor
        );

        Self {
            send_side,
            beta: field_trials.bwe_back_off_factor.backoff_factor,
            no_bitrate_increase_in_alr: field_trials.no_bitrate_increase_in_alr,
            use_current_estimate_as_min_upper_bound: field_trials
                .estimate_bounded_increase
                .use_current_estimate_as_min_upper_bound,
            disable_estimate_bounded_increase: field_trials
                .estimate_bounded_increase
                .disable_estimate_bounded_increase,
            ..Default::default()
        }
    }

    // Returns true if the target bitrate has been initialized. This happens
    // either if it has been explicitly set via SetStartBitrate/SetEstimate, or if
    // we have measured a throughput.
    pub fn valid_estimate(&self) -> bool {
        self.bitrate_is_initialized
    }

    pub fn set_start_bitrate(&mut self, start_bitrate: DataRate) {
        self.current_bitrate = start_bitrate;
        self.latest_estimated_throughput = self.current_bitrate;
        self.bitrate_is_initialized = true;
    }

    pub fn set_min_bitrate(&mut self, min_bitrate: DataRate) {
        self.min_configured_bitrate = min_bitrate;
        self.current_bitrate = self.current_bitrate.max(min_bitrate);
    }

    /* unused
    pub fn get_feedback_interval(&self) -> TimeDelta {
        // Estimate how often we can send RTCP if we allocate up to 5% of bandwidth
        // to feedback.
        const RTCP_SIZE: DataSize = DataSize::from_bytes(80);
        let rtcp_bitrate: DataRate = self.current_bitrate * 0.05;
        let interval: TimeDelta = RTCP_SIZE / rtcp_bitrate;
        const MIN_FEEDBACK_INTERVAL: TimeDelta = TimeDelta::from_millis(200);
        const MAX_FEEDBACK_INTERVAL: TimeDelta = TimeDelta::from_millis(1000);
        interval.clamp(MIN_FEEDBACK_INTERVAL, MAX_FEEDBACK_INTERVAL)
    }
    */

    // Returns true if the bitrate estimate hasn't been changed for more than
    // an RTT, or if the estimated_throughput is less than half of the current
    // estimate. Should be used to decide if we should reduce the rate further
    // when over-using.
    pub fn time_to_reduce_further(
        &mut self,
        at_time: Timestamp,
        estimated_throughput: DataRate,
    ) -> bool {
        let bitrate_reduction_interval: TimeDelta = self
            .rtt
            .clamp(TimeDelta::from_millis(10), TimeDelta::from_millis(200));
        if at_time - self.time_last_bitrate_change >= bitrate_reduction_interval {
            return true;
        }
        if self.valid_estimate() {
            // TODO(terelius/holmer): Investigate consequences of increasing
            // the threshold to 0.95 * LatestEstimate().
            let threshold: DataRate = 0.5 * self.latest_estimate();
            return estimated_throughput < threshold;
        }
        false
    }
    // As above. To be used if overusing before we have measured a throughput.
    pub fn initial_time_to_reduce_further(&mut self, at_time: Timestamp) -> bool {
        self.valid_estimate()
            && self.time_to_reduce_further(
                at_time,
                self.latest_estimate() / 2 - DataRate::from_bits_per_sec(1),
            )
    }

    pub fn latest_estimate(&self) -> DataRate {
        self.current_bitrate
    }
    pub fn set_rtt(&mut self, rtt: TimeDelta) {
        self.rtt = rtt;
    }
    pub fn update(&mut self, input: RateControlInput, at_time: Timestamp) -> DataRate {
        // Set the initial bit rate value to what we're receiving the first half
        // second.
        // TODO(bugs.webrtc.org/9379): The comment above doesn't match to the code.
        if !self.bitrate_is_initialized {
            const INITIALIZATION_TIME: TimeDelta = TimeDelta::from_seconds(5);
            assert!(BITRATE_WINDOW <= INITIALIZATION_TIME);

            if let Some(estimated_throughput) = input.estimated_throughput {
                if self.time_first_throughput_estimate.is_infinite() {
                    self.time_first_throughput_estimate = at_time;
                } else if at_time - self.time_first_throughput_estimate > INITIALIZATION_TIME {
                    self.current_bitrate = estimated_throughput;
                    self.bitrate_is_initialized = true;
                }
            }
        }

        self.change_bitrate(input, at_time);
        self.current_bitrate
    }

    pub fn set_in_application_limited_region(&mut self, in_alr: bool) {
        self.in_alr = in_alr;
    }

    pub fn set_estimate(&mut self, bitrate: DataRate, at_time: Timestamp) {
        self.bitrate_is_initialized = true;
        let prev_bitrate: DataRate = self.current_bitrate;
        self.current_bitrate = self.clamp_bitrate(bitrate);
        self.time_last_bitrate_change = at_time;
        if self.current_bitrate < prev_bitrate {
            self.time_last_bitrate_decrease = at_time;
        }
    }

    #[cfg(test)]
    pub fn set_network_state_estimate(&mut self, estimate: Option<NetworkStateEstimate>) {
        self.network_estimate = estimate;
    }

    // Returns the increase rate when used bandwidth is near the link capacity.
    pub fn get_near_max_increase_rate_bps_per_second(&self) -> f64 {
        assert!(!self.current_bitrate.is_zero());
        let frame_interval: TimeDelta = TimeDelta::from_seconds_float(1.0 / 30.0);
        let frame_size: DataSize = self.current_bitrate * frame_interval;
        const PACKET_SIZE: DataSize = DataSize::from_bytes(1200);
        let packets_per_frame: f64 = (frame_size / PACKET_SIZE).ceil();
        let avg_packet_size: DataSize = frame_size / packets_per_frame;

        // Approximate the over-use estimator delay to 100 ms.
        let mut response_time: TimeDelta = self.rtt + TimeDelta::from_millis(100);

        response_time *= 2;
        let increase_rate_bps_per_second: f64 = (avg_packet_size / response_time).bps_float();
        const MIN_INCREASE_RATE_BPS_PER_SECOND: f64 = 4000.0;
        increase_rate_bps_per_second.max(MIN_INCREASE_RATE_BPS_PER_SECOND)
    }
    // Returns the expected time between overuse signals (assuming steady state).
    pub fn get_expected_bandwidth_period(&self) -> TimeDelta {
        const MIN_PERIOD: TimeDelta = TimeDelta::from_seconds(2);
        const DEFAULT_PERIOD: TimeDelta = TimeDelta::from_seconds(3);
        const MAX_PERIOD: TimeDelta = TimeDelta::from_seconds(50);

        let increase_rate_bps_per_second: f64 = self.get_near_max_increase_rate_bps_per_second();
        if let Some(last_decrease) = self.last_decrease {
            let time_to_recover_decrease_seconds: f64 =
                last_decrease.bps_float() / increase_rate_bps_per_second;
            let period: TimeDelta = TimeDelta::from_seconds_float(time_to_recover_decrease_seconds);
            period.clamp(MIN_PERIOD, MAX_PERIOD)
        } else {
            DEFAULT_PERIOD
        }
    }

    // Update the target bitrate based on, among other things, the current rate
    // control state, the current target bitrate and the estimated throughput.
    // When in the "increase" state the bitrate will be increased either
    // additively or multiplicatively depending on the rate control region. When
    // in the "decrease" state the bitrate will be decreased to slightly below the
    // current throughput. When in the "hold" state the bitrate will be kept
    // constant to allow built up queues to drain.
    fn change_bitrate(&mut self, input: RateControlInput, at_time: Timestamp) {
        let mut new_bitrate: Option<DataRate> = None;
        let estimated_throughput: DataRate = input
            .estimated_throughput
            .unwrap_or(self.latest_estimated_throughput);
        if let Some(estimated_throughput) = input.estimated_throughput {
            self.latest_estimated_throughput = estimated_throughput;
        }

        // An over-use should always trigger us to reduce the bitrate, even though
        // we have not yet established our first estimate. By acting on the over-use,
        // we will end up with a valid estimate.
        if !self.bitrate_is_initialized && input.bw_state != BandwidthUsage::Overusing {
            return;
        }

        self.change_state(input, at_time);

        match self.rate_control_state {
            RateControlState::Hold => (),
            RateControlState::Increase => {
                if estimated_throughput > self.link_capacity.upper_bound() {
                    self.link_capacity.reset();
                }

                // We limit the new bitrate based on the troughput to avoid unlimited
                // bitrate increases. We allow a bit more lag at very low rates to not too
                // easily get stuck if the encoder produces uneven outputs.
                let mut increase_limit: DataRate =
                    1.5 * estimated_throughput + DataRate::from_kilobits_per_sec(10);
                if self.send_side && self.in_alr && self.no_bitrate_increase_in_alr {
                    // Do not increase the delay based estimate in alr since the estimator
                    // will not be able to get transport feedback necessary to detect if
                    // the new estimate is correct.
                    // If we have previously increased above the limit (for instance due to
                    // probing), we don't allow further changes.
                    increase_limit = self.current_bitrate;
                }

                if self.current_bitrate < increase_limit {
                    let increased_bitrate: DataRate = if self.link_capacity.has_estimate() {
                        // The link_capacity estimate is reset if the measured throughput
                        // is too far from the estimate. We can therefore assume that our
                        // target rate is reasonably close to link capacity and use additive
                        // increase.
                        let additive_increase: DataRate =
                            self.additive_rate_increase(at_time, self.time_last_bitrate_change);
                        self.current_bitrate + additive_increase
                    } else {
                        // If we don't have an estimate of the link capacity, use faster ramp
                        // up to discover the capacity.
                        let multiplicative_increase: DataRate = self.multiplicative_rate_increase(
                            at_time,
                            self.time_last_bitrate_change,
                            self.current_bitrate,
                        );
                        self.current_bitrate + multiplicative_increase
                    };
                    new_bitrate = Some(increased_bitrate.min(increase_limit));
                }
                self.time_last_bitrate_change = at_time;
            }
            RateControlState::Decrease => {
                // Set bit rate to something slightly lower than the measured throughput
                // to get rid of any self-induced delay.
                let mut decreased_bitrate: DataRate = estimated_throughput * self.beta;
                if decreased_bitrate > DataRate::from_kilobits_per_sec(5) {
                    decreased_bitrate -= DataRate::from_kilobits_per_sec(5);
                }

                if decreased_bitrate > self.current_bitrate {
                    // TODO(terelius): The link_capacity estimate may be based on old
                    // throughput measurements. Relying on them may lead to unnecessary
                    // BWE drops.
                    if self.link_capacity.has_estimate() {
                        decreased_bitrate = self.beta * self.link_capacity.estimate();
                    }
                }
                // Avoid increasing the rate when over-using.
                if decreased_bitrate < self.current_bitrate {
                    new_bitrate = Some(decreased_bitrate);
                }

                if self.bitrate_is_initialized && estimated_throughput < self.current_bitrate {
                    if let Some(new_bitrate) = new_bitrate {
                        self.last_decrease = Some(self.current_bitrate - new_bitrate);
                    } else {
                        self.last_decrease = Some(DataRate::zero());
                    }
                }
                if estimated_throughput < self.link_capacity.lower_bound() {
                    // The current throughput is far from the estimated link capacity. Clear
                    // the estimate to allow an immediate update in OnOveruseDetected.
                    self.link_capacity.reset();
                }

                self.bitrate_is_initialized = true;
                self.link_capacity.on_overuse_detected(estimated_throughput);
                // Stay on hold until the pipes are cleared.
                self.rate_control_state = RateControlState::Hold;
                self.time_last_bitrate_change = at_time;
                self.time_last_bitrate_decrease = at_time;
            }
        };

        self.current_bitrate = self.clamp_bitrate(new_bitrate.unwrap_or(self.current_bitrate));
    }

    fn clamp_bitrate(&self, mut new_bitrate: DataRate) -> DataRate {
        if let Some(network_estimate) = self.network_estimate.as_ref() {
            if !self.disable_estimate_bounded_increase
                && network_estimate.link_capacity_upper.is_finite()
            {
                let upper_bound: DataRate = if self.use_current_estimate_as_min_upper_bound {
                    network_estimate
                        .link_capacity_upper
                        .max(self.current_bitrate)
                } else {
                    network_estimate.link_capacity_upper
                };
                new_bitrate = upper_bound.min(new_bitrate);
            }
            if network_estimate.link_capacity_lower.is_finite()
                && new_bitrate < self.current_bitrate
            {
                new_bitrate = self
                    .current_bitrate
                    .min(new_bitrate.max(network_estimate.link_capacity_lower * self.beta));
            }
        }
        new_bitrate = new_bitrate.max(self.min_configured_bitrate);
        new_bitrate
    }

    fn multiplicative_rate_increase(
        &self,
        at_time: Timestamp,
        last_time: Timestamp,
        current_bitrate: DataRate,
    ) -> DataRate {
        let mut alpha: f64 = 1.08;
        if last_time.is_finite() {
            let time_since_last_update = at_time - last_time;
            alpha = alpha.powf(time_since_last_update.seconds_float().min(1.0));
        }
        let multiplicative_increase: DataRate =
            (current_bitrate * (alpha - 1.0)).max(DataRate::from_bits_per_sec(1000));
        multiplicative_increase
    }
    fn additive_rate_increase(&self, at_time: Timestamp, last_time: Timestamp) -> DataRate {
        let time_period_seconds: f64 = (at_time - last_time).seconds_float();
        let data_rate_increase_bps: f64 =
            self.get_near_max_increase_rate_bps_per_second() * time_period_seconds;
        DataRate::from_bits_per_sec_float(data_rate_increase_bps)
    }
    fn change_state(&mut self, input: RateControlInput, at_time: Timestamp) {
        match input.bw_state {
            BandwidthUsage::Normal => {
                if self.rate_control_state == RateControlState::Hold {
                    self.time_last_bitrate_change = at_time;
                    self.rate_control_state = RateControlState::Increase;
                }
            }
            BandwidthUsage::Overusing => {
                if self.rate_control_state != RateControlState::Decrease {
                    self.rate_control_state = RateControlState::Decrease;
                }
            }
            BandwidthUsage::Underusing => {
                self.rate_control_state = RateControlState::Hold;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const INITIAL_TIME: Timestamp = Timestamp::from_millis(123_456);

    const MIN_BWE_PERIOD: TimeDelta = TimeDelta::from_seconds(2);
    const DEFAULT_PERIOD: TimeDelta = TimeDelta::from_seconds(3);
    const MAX_BWE_PERIOD: TimeDelta = TimeDelta::from_seconds(50);

    // After an overuse, we back off to 85% to the received bitrate.
    const FRACTION_AFTER_OVERUSE: f64 = 0.85;

    #[test]
    fn min_near_max_increase_rate_on_low_bandwith() {
        let mut aimd_rate_control = AimdRateControl::default();
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(30_000), INITIAL_TIME);
        assert_eq!(
            aimd_rate_control.get_near_max_increase_rate_bps_per_second(),
            4_000.0
        );
    }

    #[test]
    fn near_max_increase_rate_is_5kbps_on_90kbps_and_200ms_rtt() {
        let mut aimd_rate_control = AimdRateControl::default();
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(90_000), INITIAL_TIME);
        assert_eq!(
            aimd_rate_control.get_near_max_increase_rate_bps_per_second(),
            5_000.0
        );
    }

    #[test]
    fn near_max_increase_rate_is_5kbps_on_60kbps_and_100ms_rtt() {
        let mut aimd_rate_control = AimdRateControl::default();
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(60_000), INITIAL_TIME);
        aimd_rate_control.set_rtt(TimeDelta::from_millis(100));
        assert_eq!(
            aimd_rate_control.get_near_max_increase_rate_bps_per_second(),
            5_000.0
        );
    }

    #[test]
    fn get_increase_rate_and_bandwidth_period() {
        let mut aimd_rate_control = AimdRateControl::default();
        const BITRATE: DataRate = DataRate::from_bits_per_sec(300_000);
        aimd_rate_control.set_estimate(BITRATE, INITIAL_TIME);
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Overusing, Some(BITRATE)),
            INITIAL_TIME,
        );
        assert!(
            (aimd_rate_control.get_near_max_increase_rate_bps_per_second() - 14_000.0).abs()
                <= 1_000.0,
        );
        assert_eq!(
            aimd_rate_control.get_expected_bandwidth_period(),
            DEFAULT_PERIOD
        );
    }

    #[test]
    fn bwe_limited_by_acked_bitrate() {
        let mut aimd_rate_control = AimdRateControl::default();
        const ACKED_BITRATE: DataRate = DataRate::from_bits_per_sec(10_000);
        let mut now: Timestamp = INITIAL_TIME;
        aimd_rate_control.set_estimate(ACKED_BITRATE, now);
        while now - INITIAL_TIME < TimeDelta::from_seconds(20) {
            aimd_rate_control.update(
                RateControlInput::new(BandwidthUsage::Normal, Some(ACKED_BITRATE)),
                now,
            );
            now += TimeDelta::from_millis(100);
        }
        assert!(aimd_rate_control.valid_estimate());
        assert_eq!(
            aimd_rate_control.latest_estimate(),
            1.5 * ACKED_BITRATE + DataRate::from_bits_per_sec(10_000)
        );
    }

    #[test]
    fn bwe_not_limited_by_decreasing_acked_bitrate() {
        let mut aimd_rate_control = AimdRateControl::default();
        const ACKED_BITRATE: DataRate = DataRate::from_bits_per_sec(10_000);
        let mut now: Timestamp = INITIAL_TIME;
        aimd_rate_control.set_estimate(ACKED_BITRATE, now);
        while now - INITIAL_TIME < TimeDelta::from_seconds(20) {
            aimd_rate_control.update(
                RateControlInput::new(BandwidthUsage::Normal, Some(ACKED_BITRATE)),
                now,
            );
            now += TimeDelta::from_millis(100);
        }
        assert!(aimd_rate_control.valid_estimate());
        // If the acked bitrate decreases the BWE shouldn't be reduced to 1.5x
        // what's being acked, but also shouldn't get to increase more.
        let prev_estimate: DataRate = aimd_rate_control.latest_estimate();
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Normal, Some(ACKED_BITRATE / 2)),
            now,
        );
        let new_estimate: DataRate = aimd_rate_control.latest_estimate();
        assert_eq!(new_estimate, prev_estimate);
        assert!(
            (new_estimate.bps()
                - (1.5f64 * ACKED_BITRATE + DataRate::from_bits_per_sec(10_000)).bps())
            .abs()
                < 2_000
        );
    }

    #[test]
    fn default_period_until_first_overuse() {
        let mut aimd_rate_control = AimdRateControl::default();
        aimd_rate_control.set_start_bitrate(DataRate::from_kilobits_per_sec(300));
        assert_eq!(
            aimd_rate_control.get_expected_bandwidth_period(),
            DEFAULT_PERIOD
        );
        aimd_rate_control.update(
            RateControlInput::new(
                BandwidthUsage::Overusing,
                Some(DataRate::from_kilobits_per_sec(280)),
            ),
            INITIAL_TIME,
        );
        assert_ne!(
            aimd_rate_control.get_expected_bandwidth_period(),
            DEFAULT_PERIOD
        );
    }

    #[test]
    fn expected_period_after_typical_drop() {
        let mut aimd_rate_control = AimdRateControl::default();
        // The rate increase at 216 kbps should be 12 kbps. If we drop from
        // 216 + 4*12 = 264 kbps, it should take 4 seconds to recover. Since we
        // back off to 0.85*acked_rate-5kbps, the acked bitrate needs to be 260
        // kbps to end up at 216 kbps.
        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(264_000);
        const UPDATED_BITRATE: DataRate = DataRate::from_bits_per_sec(216_000);
        let acked_bitrate: DataRate =
            (UPDATED_BITRATE + DataRate::from_bits_per_sec(5_000)) / FRACTION_AFTER_OVERUSE;
        let mut now: Timestamp = INITIAL_TIME;
        aimd_rate_control.set_estimate(INITIAL_BITRATE, now);
        now += TimeDelta::from_millis(100);
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Overusing, Some(acked_bitrate)),
            now,
        );
        assert_eq!(aimd_rate_control.latest_estimate(), UPDATED_BITRATE);
        assert_eq!(
            aimd_rate_control.get_near_max_increase_rate_bps_per_second(),
            12_000.0
        );
        assert_eq!(
            aimd_rate_control.get_expected_bandwidth_period(),
            TimeDelta::from_seconds(4)
        );
    }

    #[test]
    fn bandwidth_period_is_not_below_min() {
        let mut aimd_rate_control = AimdRateControl::default();
        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(10_000);
        let mut now: Timestamp = INITIAL_TIME;
        aimd_rate_control.set_estimate(INITIAL_BITRATE, now);
        now += TimeDelta::from_millis(100);
        // Make a small (1.5 kbps) bitrate drop to 8.5 kbps.
        aimd_rate_control.update(
            RateControlInput::new(
                BandwidthUsage::Overusing,
                Some(INITIAL_BITRATE - DataRate::from_bits_per_sec(1)),
            ),
            now,
        );
        assert_eq!(
            aimd_rate_control.get_expected_bandwidth_period(),
            MIN_BWE_PERIOD
        );
    }

    #[test]
    fn bandwidth_period_is_not_above_max_no_smoothing_exp() {
        let mut aimd_rate_control = AimdRateControl::default();
        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(10_010_000);
        let mut now: Timestamp = INITIAL_TIME;
        aimd_rate_control.set_estimate(INITIAL_BITRATE, now);
        now += TimeDelta::from_millis(100);
        // Make a large (10 Mbps) bitrate drop to 10 kbps.
        let acked_bitrate: DataRate = DataRate::from_bits_per_sec(10_000) / FRACTION_AFTER_OVERUSE;
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Overusing, Some(acked_bitrate)),
            now,
        );
        assert_eq!(
            aimd_rate_control.get_expected_bandwidth_period(),
            MAX_BWE_PERIOD
        );
    }

    #[test]
    fn sending_rate_bounded_when_throughput_not_estimated() {
        let mut aimd_rate_control = AimdRateControl::default();
        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(123_000);
        let mut now: Timestamp = INITIAL_TIME;
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Normal, Some(INITIAL_BITRATE)),
            now,
        );
        // AimdRateControl sets the initial bit rate to what it receives after
        // five seconds has passed.
        // TODO(bugs.webrtc.org/9379): The comment in the AimdRateControl does not
        // match the constant.
        const INITIALIZATION_TIME: TimeDelta = TimeDelta::from_seconds(5);
        now += INITIALIZATION_TIME + TimeDelta::from_millis(1);
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Normal, Some(INITIAL_BITRATE)),
            now,
        );
        for _ in 0..100 {
            aimd_rate_control.update(RateControlInput::new(BandwidthUsage::Normal, None), now);
            now += TimeDelta::from_millis(100);
        }
        assert!(
            aimd_rate_control.latest_estimate()
                <= INITIAL_BITRATE * 1.5 + DataRate::from_bits_per_sec(10_000)
        );
    }

    #[test]
    fn estimate_does_not_increase_in_alr() {
        // When alr is detected, the delay based estimator is not allowed to increase
        // bwe since there will be no feedback from the network if the new estimate
        // is correct.
        // WebRTC-DontIncreaseDelayBasedBweInAlr/Enabled/
        let field_trials = FieldTrials {
            no_bitrate_increase_in_alr: true,
            ..Default::default()
        };
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);
        let mut now: Timestamp = INITIAL_TIME;
        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(123_000);
        aimd_rate_control.set_estimate(INITIAL_BITRATE, now);
        aimd_rate_control.set_in_application_limited_region(true);
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Normal, Some(INITIAL_BITRATE)),
            now,
        );
        assert_eq!(aimd_rate_control.latest_estimate(), INITIAL_BITRATE);

        for _ in 0..100 {
            aimd_rate_control.update(RateControlInput::new(BandwidthUsage::Normal, None), now);
            now += TimeDelta::from_millis(100);
        }
        assert_eq!(aimd_rate_control.latest_estimate(), INITIAL_BITRATE);
    }

    #[test]
    fn set_estimate_increase_bwe_in_alr() {
        let field_trials = FieldTrials {
            no_bitrate_increase_in_alr: true,
            ..Default::default()
        };
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);

        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(123_000);
        aimd_rate_control.set_estimate(INITIAL_BITRATE, INITIAL_TIME);
        aimd_rate_control.set_in_application_limited_region(true);
        assert_eq!(aimd_rate_control.latest_estimate(), INITIAL_BITRATE);
        aimd_rate_control.set_estimate(2 * INITIAL_BITRATE, INITIAL_TIME);
        assert_eq!(aimd_rate_control.latest_estimate(), 2 * INITIAL_BITRATE);
    }

    #[test]
    fn set_estimate_upper_limited_by_network_estimate() {
        let field_trials = Default::default();
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(300_000), INITIAL_TIME);
        let network_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_bits_per_sec(400_000),
            ..Default::default()
        };
        aimd_rate_control.set_network_state_estimate(Some(network_estimate));
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(500_000), INITIAL_TIME);
        assert_eq!(
            aimd_rate_control.latest_estimate(),
            network_estimate.link_capacity_upper
        );
    }

    #[test]
    fn set_estimate_default_upper_limited_by_current_bitrate_if_network_estimate_is_low() {
        let field_trials = Default::default();
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(500_000), INITIAL_TIME);
        assert_eq!(
            aimd_rate_control.latest_estimate(),
            DataRate::from_bits_per_sec(500_000)
        );

        let network_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_bits_per_sec(300_000),
            ..Default::default()
        };

        aimd_rate_control.set_network_state_estimate(Some(network_estimate));
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(700_000), INITIAL_TIME);
        assert_eq!(
            aimd_rate_control.latest_estimate(),
            DataRate::from_bits_per_sec(500_000)
        );
    }

    #[test]
    fn set_estimate_not_upper_limited_by_current_bitrate_if_network_estimate_is_low_if() {
        // WebRTC-Bwe-EstimateBoundedIncrease/c_upper:false/
        let field_trials = FieldTrials {
            estimate_bounded_increase: EstimateBoundedIncrease {
                use_current_estimate_as_min_upper_bound: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);

        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(500_000), INITIAL_TIME);
        assert_eq!(
            aimd_rate_control.latest_estimate(),
            DataRate::from_bits_per_sec(500_000)
        );

        let network_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_bits_per_sec(300_000),
            ..Default::default()
        };
        aimd_rate_control.set_network_state_estimate(Some(network_estimate));
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(700_000), INITIAL_TIME);
        assert_eq!(
            aimd_rate_control.latest_estimate(),
            DataRate::from_bits_per_sec(300_000)
        );
    }

    #[test]
    fn set_estimate_lower_limited_by_network_estimate() {
        let field_trials = Default::default();
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);
        let network_estimate = NetworkStateEstimate {
            link_capacity_lower: DataRate::from_bits_per_sec(400_000),
            ..Default::default()
        };
        aimd_rate_control.set_network_state_estimate(Some(network_estimate));
        aimd_rate_control.set_estimate(DataRate::from_bits_per_sec(100_000), INITIAL_TIME);
        // 0.85 is default backoff factor. (`beta_`)
        assert_eq!(
            aimd_rate_control.latest_estimate(),
            network_estimate.link_capacity_lower * 0.85
        );
    }

    #[test]
    fn set_estimate_ignored_if_lower_than_network_estimate_and_current() {
        let field_trials = Default::default();
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);
        aimd_rate_control.set_estimate(DataRate::from_kilobits_per_sec(200), INITIAL_TIME);
        assert_eq!(aimd_rate_control.latest_estimate().kbps(), 200);
        let network_estimate = NetworkStateEstimate {
            link_capacity_lower: DataRate::from_kilobits_per_sec(400),
            ..Default::default()
        };
        aimd_rate_control.set_network_state_estimate(Some(network_estimate));
        // Ignore the next SetEstimate, since the estimate is lower than 85% of
        // the network estimate.
        aimd_rate_control.set_estimate(DataRate::from_kilobits_per_sec(100), INITIAL_TIME);
        assert_eq!(aimd_rate_control.latest_estimate().kbps(), 200);
    }

    #[test]
    fn estimate_increase_while_not_in_alr() {
        // Allow the estimate to increase as long as alr is not detected to ensure
        // tha BWE can not get stuck at a certain bitrate.
        // WebRTC-DontIncreaseDelayBasedBweInAlr/Enabled/
        let field_trials = FieldTrials {
            no_bitrate_increase_in_alr: true,
            ..Default::default()
        };
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);
        aimd_rate_control.no_bitrate_increase_in_alr = true;
        let mut now: Timestamp = INITIAL_TIME;
        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(123_000);
        aimd_rate_control.set_estimate(INITIAL_BITRATE, now);
        aimd_rate_control.set_in_application_limited_region(false);
        aimd_rate_control.update(
            RateControlInput::new(BandwidthUsage::Normal, Some(INITIAL_BITRATE)),
            now,
        );
        for _ in 0..100 {
            aimd_rate_control.update(RateControlInput::new(BandwidthUsage::Normal, None), now);
            now += TimeDelta::from_millis(100);
        }
        assert!(aimd_rate_control.latest_estimate() > INITIAL_BITRATE);
    }

    #[test]
    fn estimate_not_limited_by_network_estimate_if_disabled() {
        // WebRTC-Bwe-EstimateBoundedIncrease/Disabled/
        let field_trials = FieldTrials {
            estimate_bounded_increase: EstimateBoundedIncrease {
                disable_estimate_bounded_increase: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut aimd_rate_control = AimdRateControl::new(&field_trials, true);
        aimd_rate_control.disable_estimate_bounded_increase = true;

        let mut now: Timestamp = INITIAL_TIME;
        const INITIAL_BITRATE: DataRate = DataRate::from_bits_per_sec(123_000);
        aimd_rate_control.set_estimate(INITIAL_BITRATE, now);
        aimd_rate_control.set_in_application_limited_region(false);
        let network_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_bits_per_sec(150_000),
            ..Default::default()
        };
        aimd_rate_control.set_network_state_estimate(Some(network_estimate));

        for _ in 0..100 {
            aimd_rate_control.update(RateControlInput::new(BandwidthUsage::Normal, None), now);
            now += TimeDelta::from_millis(100);
        }
        assert!(aimd_rate_control.latest_estimate() > network_estimate.link_capacity_upper);
    }
}
