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

use std::collections::VecDeque;

use crate::remote_bitrate_estimator::CONGESTION_CONTROLLER_MIN_BITRATE;
use crate::{
    api::{
        transport::{BandwidthUsage, SentPacket, TransportPacketsFeedback},
        units::{DataRate, TimeDelta, Timestamp},
    },
    experiments::FieldTrials,
    LossBasedBandwidthEstimation, LossBasedBweV2, LossBasedState,
};

#[derive(Clone, Debug)]
pub struct BweLossExperiment {
    pub enabled: bool,
    pub low_loss_threshold: f32,
    pub high_loss_threshold: f32,
    pub bitrate_threshold_kbps: f64,
}

impl Default for BweLossExperiment {
    fn default() -> Self {
        Self {
            enabled: false,
            low_loss_threshold: 0.02,
            high_loss_threshold: 0.1,
            bitrate_threshold_kbps: 0.0,
        }
    }
}

pub struct LinkCapacityTracker {
    capacity_estimate_bps: f64,
    last_link_capacity_update: Timestamp, // = Timestamp::minus_infinity();
    last_delay_based_estimate: DataRate,  // = DataRate::plus_infinity();
}

impl Default for LinkCapacityTracker {
    fn default() -> Self {
        Self {
            capacity_estimate_bps: 0.0,
            last_link_capacity_update: Timestamp::minus_infinity(),
            last_delay_based_estimate: DataRate::plus_infinity(),
        }
    }
}

impl LinkCapacityTracker {
    // Call when a new delay-based estimate is available.
    pub fn update_delay_based_estimate(
        &mut self,
        at_time: Timestamp,
        delay_based_bitrate: DataRate,
    ) {
        if delay_based_bitrate < self.last_delay_based_estimate {
            self.capacity_estimate_bps = self
                .capacity_estimate_bps
                .min(delay_based_bitrate.bps_float());
            self.last_link_capacity_update = at_time;
        }
        self.last_delay_based_estimate = delay_based_bitrate;
    }
    pub fn on_starting_rate(&mut self, start_rate: DataRate) {
        if self.last_link_capacity_update.is_infinite() {
            self.capacity_estimate_bps = start_rate.bps_float();
        }
    }
    pub fn on_rate_update(
        &mut self,
        acknowledged: Option<DataRate>,
        target: DataRate,
        at_time: Timestamp,
    ) {
        let acknowledged = match acknowledged {
            Some(ack) => ack,
            None => return,
        };
        let acknowledged_target: DataRate = std::cmp::min(acknowledged, target);
        if acknowledged_target.bps_float() > self.capacity_estimate_bps {
            let delta: TimeDelta = at_time - self.last_link_capacity_update;
            let alpha: f64 = if delta.is_finite() {
                (-(delta / TimeDelta::from_seconds(10))).exp()
            } else {
                0.0
            };
            self.capacity_estimate_bps = alpha * self.capacity_estimate_bps
                + (1.0 - alpha) * acknowledged_target.bps_float();
        }
        self.last_link_capacity_update = at_time;
    }
    pub fn on_rtt_backoff(&mut self, backoff_rate: DataRate, at_time: Timestamp) {
        self.capacity_estimate_bps = self.capacity_estimate_bps.min(backoff_rate.bps_float());
        self.last_link_capacity_update = at_time;
    }
    pub fn estimate(&self) -> DataRate {
        DataRate::from_bits_per_sec_float(self.capacity_estimate_bps)
    }
}

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

pub struct SendSideBandwidthEstimation {
    field_trials: FieldTrials,
    rtt_backoff: RttBasedBackoff,
    link_capacity: LinkCapacityTracker,

    min_bitrate_history: VecDeque<(Timestamp, DataRate)>,

    // incoming filters
    lost_packets_since_last_loss_update: i64,
    expected_packets_since_last_loss_update: i64,

    acknowledged_rate: Option<DataRate>,
    current_target: DataRate,
    min_bitrate_configured: DataRate,
    max_bitrate_configured: DataRate,
    last_low_bitrate_log: Timestamp,

    has_decreased_since_last_fraction_loss: bool,
    last_loss_feedback: Timestamp,
    last_loss_packet_report: Timestamp,
    last_fraction_loss: u8,
    last_logged_fraction_loss: u8,
    last_round_trip_time: TimeDelta,

    // The max bitrate as set by the receiver in the call. This is typically
    // signalled using the REMB RTCP message and is used when we don't have any
    // send side delay based estimate.
    receiver_limit: DataRate,
    delay_based_limit: DataRate,
    time_last_decrease: Timestamp,
    first_report_time: Timestamp,
    initially_lost_packets: i64,
    bitrate_at_2_seconds: DataRate,
    last_rtc_event_log: Timestamp,
    low_loss_threshold: f32,
    high_loss_threshold: f32,
    bitrate_threshold: DataRate,
    loss_based_bandwidth_estimator_v1: LossBasedBandwidthEstimation,
    loss_based_bandwidth_estimator_v2: LossBasedBweV2,
    loss_based_state: LossBasedState,
    disable_receiver_limit_caps_only: bool,
}

impl Default for SendSideBandwidthEstimation {
    fn default() -> Self {
        Self::new(FieldTrials::default())
    }
}

impl SendSideBandwidthEstimation {
    const BWE_INCREASE_INTERVAL: TimeDelta = TimeDelta::from_millis(1000);
    const BWE_DECREASE_INTERVAL: TimeDelta = TimeDelta::from_millis(300);
    const START_PHASE: TimeDelta = TimeDelta::from_millis(2000);
    const LIMIT_NUM_PACKETS: i64 = 20;
    const DEFAULT_MAX_BITRATE: DataRate = DataRate::from_bits_per_sec(1000000000);
    const LOW_BITRATE_LOG_PERIOD: TimeDelta = TimeDelta::from_millis(10000);
    // Expecting that RTCP feedback is sent uniformly within [0.5, 1.5]s intervals.
    const MAX_RTCP_FEEDBACK_INTERVAL: TimeDelta = TimeDelta::from_millis(5000);

    pub fn new(field_trials: FieldTrials) -> Self {
        let mut low_loss_threshold = 0.02;
        let mut high_loss_threshold = 0.1;
        let mut bitrate_threshold_kbps = 0.0;

        if field_trials.loss_experiment.enabled {
            low_loss_threshold = field_trials.loss_experiment.low_loss_threshold;
            high_loss_threshold = field_trials.loss_experiment.high_loss_threshold;
            bitrate_threshold_kbps = field_trials.loss_experiment.bitrate_threshold_kbps;
            assert!(low_loss_threshold > 0.0);
            assert!(low_loss_threshold <= 1.0);
            assert!(high_loss_threshold > 0.0);
            assert!(high_loss_threshold <= 1.0);
            assert!(low_loss_threshold <= high_loss_threshold);
            assert!(bitrate_threshold_kbps >= 0.0);
            tracing::debug!(
                "Enabled BweLossExperiment with parameters {:?}, {:?}, {:?}",
                low_loss_threshold,
                high_loss_threshold,
                bitrate_threshold_kbps
            );
        }
        let bitrate_threshold = DataRate::from_kilobits_per_sec_float(bitrate_threshold_kbps);

        let mut loss_based_bandwidth_estimator_v2 =
            LossBasedBweV2::new(field_trials.loss_based_bwe_v2.clone());
        if field_trials.loss_based_bwe_v2.enabled {
            loss_based_bandwidth_estimator_v2
                .set_min_max_bitrate(CONGESTION_CONTROLLER_MIN_BITRATE, Self::DEFAULT_MAX_BITRATE);
        }

        Self {
            rtt_backoff: RttBasedBackoff::new(&field_trials.max_rtt_limit),
            link_capacity: LinkCapacityTracker::default(),
            min_bitrate_history: VecDeque::new(),
            lost_packets_since_last_loss_update: 0,
            expected_packets_since_last_loss_update: 0,
            current_target: DataRate::zero(),
            min_bitrate_configured: CONGESTION_CONTROLLER_MIN_BITRATE,
            max_bitrate_configured: Self::DEFAULT_MAX_BITRATE,
            last_low_bitrate_log: Timestamp::minus_infinity(),
            has_decreased_since_last_fraction_loss: false,
            last_loss_feedback: Timestamp::minus_infinity(),
            last_loss_packet_report: Timestamp::minus_infinity(),
            last_fraction_loss: 0,
            last_logged_fraction_loss: 0,
            last_round_trip_time: TimeDelta::zero(),
            receiver_limit: DataRate::plus_infinity(),
            delay_based_limit: DataRate::plus_infinity(),
            time_last_decrease: Timestamp::minus_infinity(),
            first_report_time: Timestamp::minus_infinity(),
            initially_lost_packets: 0,
            bitrate_at_2_seconds: DataRate::zero(),
            last_rtc_event_log: Timestamp::minus_infinity(),
            low_loss_threshold,
            high_loss_threshold,
            bitrate_threshold,
            loss_based_bandwidth_estimator_v1: LossBasedBandwidthEstimation::new(
                field_trials.loss_based_control.clone(),
            ),
            loss_based_bandwidth_estimator_v2,
            loss_based_state: LossBasedState::DelayBasedEstimate,
            disable_receiver_limit_caps_only: field_trials.receiver_limit_caps_only,
            acknowledged_rate: None,
            field_trials,
        }
    }

    pub fn on_route_change(&mut self) {
        self.lost_packets_since_last_loss_update = 0;
        self.expected_packets_since_last_loss_update = 0;
        self.current_target = DataRate::zero();
        self.min_bitrate_configured = CONGESTION_CONTROLLER_MIN_BITRATE;
        self.max_bitrate_configured = Self::DEFAULT_MAX_BITRATE;
        self.last_low_bitrate_log = Timestamp::minus_infinity();
        self.has_decreased_since_last_fraction_loss = false;
        self.last_loss_feedback = Timestamp::minus_infinity();
        self.last_loss_packet_report = Timestamp::minus_infinity();
        self.last_fraction_loss = 0;
        self.last_logged_fraction_loss = 0;
        self.last_round_trip_time = TimeDelta::zero();
        self.receiver_limit = DataRate::plus_infinity();
        self.delay_based_limit = DataRate::plus_infinity();
        self.time_last_decrease = Timestamp::minus_infinity();
        self.first_report_time = Timestamp::minus_infinity();
        self.initially_lost_packets = 0;
        self.bitrate_at_2_seconds = DataRate::zero();
        self.last_rtc_event_log = Timestamp::minus_infinity();
        if self.loss_based_bandwidth_estimator_v2_enabled()
            && self.loss_based_bandwidth_estimator_v2.use_in_start_phase()
        {
            self.loss_based_bandwidth_estimator_v2 =
                LossBasedBweV2::new(self.field_trials.loss_based_bwe_v2.clone());
        }
    }

    pub fn target_rate(&self) -> DataRate {
        let mut target: DataRate = self.current_target;
        if !self.disable_receiver_limit_caps_only {
            target = std::cmp::min(target, self.receiver_limit);
        }
        std::cmp::max(self.min_bitrate_configured, target)
    }
    pub fn loss_based_state(&self) -> LossBasedState {
        self.loss_based_state
    }
    // Return whether the current rtt is higher than the rtt limited configured in
    // RttBasedBackoff.
    pub fn is_rtt_above_limit(&self) -> bool {
        self.rtt_backoff.is_rtt_above_limit()
    }

    pub fn fraction_loss(&self) -> u8 {
        self.last_fraction_loss
    }

    pub fn round_trip_time(&self) -> TimeDelta {
        self.last_round_trip_time
    }

    pub fn get_estimated_link_capacity(&self) -> DataRate {
        self.link_capacity.estimate()
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
                self.link_capacity.on_rtt_backoff(new_bitrate, at_time);
                self.update_target_bitrate(new_bitrate, at_time);
                return;
            }
            // TODO(srte): This is likely redundant in most cases.
            self.apply_target_limits(at_time);
            return;
        }

        // We trust the REMB and/or delay-based estimate during the first 2 seconds if
        // we haven't had any packet loss reported, to allow startup bitrate probing.
        if self.last_fraction_loss == 0
            && self.is_in_start_phase(at_time)
            && !self
                .loss_based_bandwidth_estimator_v2
                .ready_to_use_in_start_phase()
        {
            let mut new_bitrate: DataRate = self.current_target;
            // TODO(srte): We should not allow the new_bitrate to be larger than the
            // receiver limit here.
            if self.receiver_limit.is_finite() {
                new_bitrate = std::cmp::max(self.receiver_limit, new_bitrate);
            }
            if self.delay_based_limit.is_finite() {
                new_bitrate = std::cmp::max(self.delay_based_limit, new_bitrate);
            }
            if self.loss_based_bandwidth_estimator_v1_enabled() {
                self.loss_based_bandwidth_estimator_v1
                    .initialize(new_bitrate);
            }

            if new_bitrate != self.current_target {
                self.min_bitrate_history.clear();
                if self.loss_based_bandwidth_estimator_v1_enabled() {
                    self.min_bitrate_history.push_back((at_time, new_bitrate));
                } else {
                    self.min_bitrate_history
                        .push_back((at_time, self.current_target));
                }
                self.update_target_bitrate(new_bitrate, at_time);
                return;
            }
        }
        self.update_min_history(at_time);
        if self.last_loss_packet_report.is_infinite() {
            // No feedback received.
            // TODO(srte): This is likely redundant in most cases.
            self.apply_target_limits(at_time);
            return;
        }

        if self.loss_based_bandwidth_estimator_v1_ready_for_use() {
            let new_bitrate: DataRate = self.loss_based_bandwidth_estimator_v1.update(
                at_time,
                self.min_bitrate_history.front().unwrap().1,
                self.delay_based_limit,
                self.last_round_trip_time,
            );
            self.update_target_bitrate(new_bitrate, at_time);
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

        let time_since_loss_packet_report: TimeDelta = at_time - self.last_loss_packet_report;
        if time_since_loss_packet_report < 1.2 * Self::MAX_RTCP_FEEDBACK_INTERVAL {
            // We only care about loss above a given bitrate threshold.
            let loss: f32 = self.last_fraction_loss as f32 / 256.0;
            // We only make decisions based on loss when the bitrate is above a
            // threshold. This is a crude way of handling loss which is uncorrelated
            // to congestion.
            if self.current_target < self.bitrate_threshold || loss <= self.low_loss_threshold {
                // Loss < 2%: Increase rate by 8% of the min bitrate in the last
                // BweIncreaseInterval.
                // Note that by remembering the bitrate over the last second one can
                // rampup up one second faster than if only allowed to start ramping
                // at 8% per second rate now. E.g.:
                //   If sending a constant 100kbps it can rampup immediately to 108kbps
                //   whenever a receiver report is received with lower packet loss.
                //   If instead one would do: self.current_bitrate *= 1.08^(delta time),
                //   it would take over one second since the lower packet loss to achieve
                //   108kbps.
                let mut new_bitrate: DataRate = DataRate::from_bits_per_sec_float(
                    self.min_bitrate_history.front().unwrap().1.bps_float() * 1.08 + 0.5,
                );

                // Add 1 kbps extra, just to make sure that we do not get stuck
                // (gives a little extra increase at low rates, negligible at higher
                // rates).
                new_bitrate += DataRate::from_bits_per_sec(1000);
                self.update_target_bitrate(new_bitrate, at_time);
                return;
            } else if self.current_target > self.bitrate_threshold {
                if loss <= self.high_loss_threshold {
                    // Loss between 2% - 10%: Do nothing.
                } else {
                    // Loss > 10%: Limit the rate decreases to once a BweDecreaseInterval
                    // + rtt.
                    if !self.has_decreased_since_last_fraction_loss
                        && (at_time - self.time_last_decrease)
                            >= (Self::BWE_DECREASE_INTERVAL + self.last_round_trip_time)
                    {
                        self.time_last_decrease = at_time;

                        // Reduce rate:
                        //   newRate = rate * (1 - 0.5*lossRate);
                        //   where packetLoss = 256*lossRate;
                        let new_bitrate: DataRate = DataRate::from_bits_per_sec_float(
                            (self.current_target.bps_float()
                                * (512.0 - self.last_fraction_loss as f64))
                                / 512.0,
                        );
                        self.has_decreased_since_last_fraction_loss = true;
                        self.update_target_bitrate(new_bitrate, at_time);
                        return;
                    }
                }
            }
        }
        // TODO(srte): This is likely redundant in most cases.
        self.apply_target_limits(at_time);
    }
    pub fn on_sent_packet(&mut self, sent_packet: SentPacket) {
        // Only feedback-triggering packets will be reported here.
        self.rtt_backoff.last_packet_sent = sent_packet.send_time;
    }
    pub fn update_propagation_rtt(&mut self, at_time: Timestamp, propagation_rtt: TimeDelta) {
        self.rtt_backoff
            .update_propagation_rtt(at_time, propagation_rtt);
    }
    // Call when we receive a RTCP message with TMMBR or REMB.
    pub fn update_receiver_estimate(&mut self, at_time: Timestamp, bandwidth: DataRate) {
        // TODO(srte): Ensure caller passes plus_infinity, not zero, to represent no
        // limitation.
        self.receiver_limit = if bandwidth.is_zero() {
            DataRate::plus_infinity()
        } else {
            bandwidth
        };
        self.apply_target_limits(at_time);
    }
    // Call when a new delay-based estimate is available.
    pub fn update_delay_based_estimate(&mut self, at_time: Timestamp, bitrate: DataRate) {
        self.link_capacity
            .update_delay_based_estimate(at_time, bitrate);
        // TODO(srte): Ensure caller passes plus_infinity, not zero, to represent no
        // limitation.
        self.delay_based_limit = if bitrate.is_zero() {
            DataRate::plus_infinity()
        } else {
            bitrate
        };
        self.apply_target_limits(at_time);
    }
    // Call when we receive a RTCP message with a ReceiveBlock.
    pub fn update_packets_lost(
        &mut self,
        packets_lost: i64,
        number_of_packets: i64,
        at_time: Timestamp,
    ) {
        self.last_loss_feedback = at_time;
        if self.first_report_time.is_infinite() {
            self.first_report_time = at_time;
        }

        // Check sequence number diff and weight loss report
        if number_of_packets > 0 {
            let expected: i64 = self.expected_packets_since_last_loss_update + number_of_packets;

            // Don't generate a loss rate until it can be based on enough packets.
            if expected < Self::LIMIT_NUM_PACKETS {
                // Accumulate reports.
                self.expected_packets_since_last_loss_update = expected;
                self.lost_packets_since_last_loss_update += packets_lost;
                return;
            }

            self.has_decreased_since_last_fraction_loss = false;
            let lost_q8: i64 =
                std::cmp::max(self.lost_packets_since_last_loss_update + packets_lost, 0) << 8;
            self.last_fraction_loss = (lost_q8 / expected).min(255) as u8;

            // Reset accumulators.
            self.lost_packets_since_last_loss_update = 0;
            self.expected_packets_since_last_loss_update = 0;
            self.last_loss_packet_report = at_time;
            self.update_estimate(at_time);
        }
    }
    // Call when we receive a RTCP message with a ReceiveBlock.
    pub fn update_rtt(&mut self, rtt: TimeDelta, _at_time: Timestamp) {
        // Update RTT if we were able to compute an RTT based on this RTCP.
        // FlexFEC doesn't send RTCP SR, which means we won't be able to compute RTT.
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
            self.link_capacity.on_starting_rate(send_bitrate);
            self.set_send_bitrate(send_bitrate, at_time);
        }
    }
    pub fn set_send_bitrate(&mut self, bitrate: DataRate, at_time: Timestamp) {
        assert!(bitrate > DataRate::zero());
        // Reset to avoid being capped by the estimate.
        self.delay_based_limit = DataRate::plus_infinity();
        self.update_target_bitrate(bitrate, at_time);
        // Clear last sent bitrate history so the new value can be used directly
        // and not capped.
        self.min_bitrate_history.clear();
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

    pub fn set_acknowledged_rate(
        &mut self,
        acknowledged_rate: Option<DataRate>,
        at_time: Timestamp,
    ) {
        self.acknowledged_rate = acknowledged_rate;
        let acknowledged_rate = match acknowledged_rate {
            Some(rate) => rate,
            None => return,
        };
        if self.loss_based_bandwidth_estimator_v1_enabled() {
            self.loss_based_bandwidth_estimator_v1
                .update_acknowledged_bitrate(acknowledged_rate, at_time);
        }
        if self.loss_based_bandwidth_estimator_v2_enabled() {
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
        if self.loss_based_bandwidth_estimator_v1_enabled() {
            self.loss_based_bandwidth_estimator_v1
                .update_loss_statistics(&report.packet_feedbacks, report.feedback_time);
        }
        if self.loss_based_bandwidth_estimator_v2_enabled() {
            self.loss_based_bandwidth_estimator_v2
                .update_bandwidth_estimate(
                    &report.packet_feedbacks,
                    self.delay_based_limit,
                    in_alr,
                );
            self.update_estimate(report.feedback_time);
        }
    }
    pub fn pace_at_loss_based_estimate(&self) -> bool {
        self.loss_based_bandwidth_estimator_v2_ready_for_use()
            && self
                .loss_based_bandwidth_estimator_v2
                .pace_at_loss_based_estimate()
    }

    fn is_in_start_phase(&self, at_time: Timestamp) -> bool {
        self.first_report_time.is_infinite() || at_time - self.first_report_time < Self::START_PHASE
    }

    // Updates history of min bitrates.
    // After this method returns self.min_bitrate_history.front().second contains the
    // min bitrate used during last BweIncreaseIntervalMs.
    fn update_min_history(&mut self, at_time: Timestamp) {
        // Remove old data points from history.
        // Since history precision is in ms, add one so it is able to increase
        // bitrate if it is off by as little as 0.5ms.
        while !self.min_bitrate_history.is_empty()
            && at_time - self.min_bitrate_history.front().unwrap().0 + TimeDelta::from_millis(1)
                > Self::BWE_INCREASE_INTERVAL
        {
            self.min_bitrate_history.pop_front();
        }

        // Typical minimum sliding-window algorithm: Pop values higher than current
        // bitrate before pushing it.
        while !self.min_bitrate_history.is_empty()
            && self.current_target <= self.min_bitrate_history.back().unwrap().1
        {
            self.min_bitrate_history.pop_back();
        }

        self.min_bitrate_history
            .push_back((at_time, self.current_target));
    }

    // Gets the upper limit for the target bitrate. This is the minimum of the
    // delay based limit, the receiver limit and the loss based controller limit.
    fn get_upper_limit(&self) -> DataRate {
        let mut upper_limit: DataRate = self.delay_based_limit;
        if self.disable_receiver_limit_caps_only {
            upper_limit = std::cmp::min(upper_limit, self.receiver_limit);
        }
        std::cmp::min(upper_limit, self.max_bitrate_configured)
    }
    // Prints a warning if `bitrate` if sufficiently long time has past since last
    // warning.
    fn maybe_log_low_bitrate_warning(&mut self, bitrate: DataRate, at_time: Timestamp) {
        if at_time - self.last_low_bitrate_log > Self::LOW_BITRATE_LOG_PERIOD {
            tracing::warn!(
                "Estimated available bandwidth {:?} is below configured min bitrate {:?}.",
                bitrate,
                self.min_bitrate_configured
            );
            self.last_low_bitrate_log = at_time;
        }
    }

    // Cap `bitrate` to [self.min_bitrate_configured, max_bitrate_configured_] and
    // set `current_bitrate_` to the capped value and updates the event log.
    fn update_target_bitrate(&mut self, mut new_bitrate: DataRate, at_time: Timestamp) {
        new_bitrate = std::cmp::min(new_bitrate, self.get_upper_limit());
        if new_bitrate < self.min_bitrate_configured {
            self.maybe_log_low_bitrate_warning(new_bitrate, at_time);
            new_bitrate = self.min_bitrate_configured;
        }
        self.current_target = new_bitrate;
        self.link_capacity
            .on_rate_update(self.acknowledged_rate, self.current_target, at_time);
    }
    // Applies lower and upper bounds to the current target rate.
    // TODO(srte): This seems to be called even when limits haven't changed, that
    // should be cleaned up.
    fn apply_target_limits(&mut self, at_time: Timestamp) {
        self.update_target_bitrate(self.current_target, at_time);
    }

    fn loss_based_bandwidth_estimator_v1_enabled(&self) -> bool {
        self.field_trials.loss_based_control.enabled && !self.field_trials.loss_based_bwe_v2.enabled
    }
    fn loss_based_bandwidth_estimator_v2_enabled(&self) -> bool {
        self.field_trials.loss_based_bwe_v2.enabled
    }

    fn loss_based_bandwidth_estimator_v1_ready_for_use(&self) -> bool {
        self.loss_based_bandwidth_estimator_v1_enabled()
            && self.loss_based_bandwidth_estimator_v1.in_use()
    }
    fn loss_based_bandwidth_estimator_v2_ready_for_use(&self) -> bool {
        self.loss_based_bandwidth_estimator_v2.is_ready()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn test_probing(use_delay_based: bool) {
        let mut bwe = SendSideBandwidthEstimation::new(Default::default());
        let mut now_ms: i64 = 0;
        bwe.set_min_max_bitrate(
            DataRate::from_bits_per_sec(100000),
            DataRate::from_bits_per_sec(1500000),
        );
        bwe.set_send_bitrate(
            DataRate::from_bits_per_sec(200000),
            Timestamp::from_millis(now_ms),
        );

        const REMB_BPS: i64 = 1000000;
        const SECOND_REMB_BPS: i64 = REMB_BPS + 500000;

        bwe.update_packets_lost(
            /*packets_lost=*/ 0,
            /*number_of_packets=*/ 1,
            Timestamp::from_millis(now_ms),
        );
        bwe.update_rtt(TimeDelta::from_millis(50), Timestamp::from_millis(now_ms));

        // Initial REMB applies immediately.
        if use_delay_based {
            bwe.update_delay_based_estimate(
                Timestamp::from_millis(now_ms),
                DataRate::from_bits_per_sec(REMB_BPS),
            );
        } else {
            bwe.update_receiver_estimate(
                Timestamp::from_millis(now_ms),
                DataRate::from_bits_per_sec(REMB_BPS),
            );
        }
        bwe.update_estimate(Timestamp::from_millis(now_ms));
        assert_eq!(REMB_BPS, bwe.target_rate().bps());

        // Second REMB doesn't apply immediately.
        now_ms += 2001;
        if use_delay_based {
            bwe.update_delay_based_estimate(
                Timestamp::from_millis(now_ms),
                DataRate::from_bits_per_sec(SECOND_REMB_BPS),
            );
        } else {
            bwe.update_receiver_estimate(
                Timestamp::from_millis(now_ms),
                DataRate::from_bits_per_sec(SECOND_REMB_BPS),
            );
        }
        bwe.update_estimate(Timestamp::from_millis(now_ms));
        assert_eq!(REMB_BPS, bwe.target_rate().bps());
    }

    #[test]
    fn initial_remb_with_probing() {
        test_probing(false);
    }

    #[test]
    fn initial_delay_based_bwe_with_probing() {
        test_probing(true);
    }

    #[test]
    fn doesnt_reapply_bitrate_decrease_without_following_remb() {
        let mut bwe = SendSideBandwidthEstimation::new(Default::default());
        const MIN_BITRATE_BPS: i64 = 100000;
        const INITIAL_BITRATE_BPS: i64 = 1000000;
        let mut now_ms: i64 = 1000;
        bwe.set_min_max_bitrate(
            DataRate::from_bits_per_sec(MIN_BITRATE_BPS),
            DataRate::from_bits_per_sec(1500000),
        );
        bwe.set_send_bitrate(
            DataRate::from_bits_per_sec(INITIAL_BITRATE_BPS),
            Timestamp::from_millis(now_ms),
        );

        const FRACTION_LOSS: u8 = 128;
        const RTT_MS: i64 = 50;
        now_ms += 10000;

        assert_eq!(INITIAL_BITRATE_BPS, bwe.target_rate().bps());
        assert_eq!(0, bwe.fraction_loss());
        assert_eq!(0, bwe.round_trip_time().ms());

        // Signal heavy loss to go down in bitrate.
        bwe.update_packets_lost(
            /*packets_lost=*/ 50,
            /*number_of_packets=*/ 100,
            Timestamp::from_millis(now_ms),
        );
        bwe.update_rtt(
            TimeDelta::from_millis(RTT_MS),
            Timestamp::from_millis(now_ms),
        );

        // Trigger an update 2 seconds later to not be rate limited.
        now_ms += 1000;
        bwe.update_estimate(Timestamp::from_millis(now_ms));
        assert!(bwe.target_rate().bps() < INITIAL_BITRATE_BPS);
        // Verify that the obtained bitrate isn't hitting the min bitrate, or this
        // test doesn't make sense. If this ever happens, update the thresholds or
        // loss rates so that it doesn't hit min bitrate after one bitrate update.
        assert!(bwe.target_rate().bps() > MIN_BITRATE_BPS);
        assert_eq!(FRACTION_LOSS, bwe.fraction_loss());
        assert_eq!(RTT_MS, bwe.round_trip_time().ms());

        // Triggering an update shouldn't apply further downgrade nor upgrade since
        // there's no intermediate receiver block received indicating whether this is
        // currently good or not.
        let last_bitrate_bps: i64 = bwe.target_rate().bps();
        // Trigger an update 2 seconds later to not be rate limited (but it still
        // shouldn't update).
        now_ms += 1000;
        bwe.update_estimate(Timestamp::from_millis(now_ms));

        assert_eq!(last_bitrate_bps, bwe.target_rate().bps());
        // The old loss rate should still be applied though.
        assert_eq!(FRACTION_LOSS, bwe.fraction_loss());
        assert_eq!(RTT_MS, bwe.round_trip_time().ms());
    }

    #[test]
    fn setting_send_bitrate_overrides_delay_based_estimate() {
        let mut bwe = SendSideBandwidthEstimation::new(Default::default());
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
    fn fraction_loss_is_not_overflowed() {
        let mut bwe = SendSideBandwidthEstimation::new(Default::default());
        const MIN_BITRATE_BPS: i64 = 100000;
        const INITIAL_BITRATE_BPS: i64 = 1000000;
        let mut now_ms: i64 = 1000;
        bwe.set_min_max_bitrate(
            DataRate::from_bits_per_sec(MIN_BITRATE_BPS),
            DataRate::from_bits_per_sec(1500000),
        );
        bwe.set_send_bitrate(
            DataRate::from_bits_per_sec(INITIAL_BITRATE_BPS),
            Timestamp::from_millis(now_ms),
        );

        now_ms += 10000;

        assert_eq!(INITIAL_BITRATE_BPS, bwe.target_rate().bps());
        assert_eq!(0, bwe.fraction_loss());

        // Signal negative loss.
        bwe.update_packets_lost(
            /*packets_lost=*/ -1,
            /*number_of_packets=*/ 100,
            Timestamp::from_millis(now_ms),
        );
        assert_eq!(0, bwe.fraction_loss());
    }

    #[test]
    fn rtt_is_above_limit_if_rtt_greater_than_limit() {
        let mut bwe = SendSideBandwidthEstimation::new(Default::default());
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
        let mut bwe = SendSideBandwidthEstimation::new(Default::default());
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
