/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::{
    transport::PacketResult,
    units::{DataRate, TimeDelta, Timestamp},
};

#[derive(Clone, Debug)]
pub struct LossBasedControlConfig {
    pub enabled: bool,                             // Enabled
    pub min_increase_factor: f64,                  // min_incr
    pub max_increase_factor: f64,                  // max_incr
    pub increase_low_rtt: TimeDelta,               // incr_low_rtt
    pub increase_high_rtt: TimeDelta,              // incr_high_rtt
    pub decrease_factor: f64,                      // decr
    pub loss_window: TimeDelta,                    // loss_win
    pub loss_max_window: TimeDelta,                // loss_max_win
    pub acknowledged_rate_max_window: TimeDelta,   // ackrate_max_win
    pub increase_offset: DataRate,                 // incr_offset
    pub loss_bandwidth_balance_increase: DataRate, // balance_incr
    pub loss_bandwidth_balance_decrease: DataRate, // balance_decr
    pub loss_bandwidth_balance_reset: DataRate,    // balance_reset
    pub loss_bandwidth_balance_exponent: f64,      // exponent
    pub allow_resets: bool,                        // resets
    pub decrease_interval: TimeDelta,              // decr_intvl
    pub loss_report_timeout: TimeDelta,            // timeout
}

impl Default for LossBasedControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_increase_factor: 1.02,
            max_increase_factor: 1.08,
            increase_low_rtt: TimeDelta::from_millis(200),
            increase_high_rtt: TimeDelta::from_millis(800),
            decrease_factor: 0.99,
            loss_window: TimeDelta::from_millis(800),
            loss_max_window: TimeDelta::from_millis(800),
            acknowledged_rate_max_window: TimeDelta::from_millis(800),
            increase_offset: DataRate::from_bits_per_sec(1000),
            loss_bandwidth_balance_increase: DataRate::from_kilobits_per_sec_float(0.5),
            loss_bandwidth_balance_decrease: DataRate::from_kilobits_per_sec(4),
            loss_bandwidth_balance_reset: DataRate::from_kilobits_per_sec_float(0.1),
            loss_bandwidth_balance_exponent: 0.5,
            allow_resets: false,
            decrease_interval: TimeDelta::from_millis(300),
            loss_report_timeout: TimeDelta::from_millis(6000),
        }
    }
}

// Estimates an upper BWE limit based on loss.
// It requires knowledge about lost packets and acknowledged bitrate.
// Ie, this class require transport feedback.
pub struct LossBasedBandwidthEstimation {
    config: LossBasedControlConfig,
    average_loss: f64,
    average_loss_max: f64,
    loss_based_bitrate: DataRate,
    acknowledged_bitrate_max: DataRate,
    acknowledged_bitrate_last_update: Timestamp,
    time_last_decrease: Timestamp,
    has_decreased_since_last_loss_report: bool,
    last_loss_packet_report: Timestamp,
    last_loss_ratio: f64,
}

impl LossBasedBandwidthEstimation {
    // Expecting RTCP feedback to be sent with roughly 1s intervals, a 5s gap
    // indicates a channel outage.
    const MAX_RTCP_FEEDBACK_INTERVAL: TimeDelta = TimeDelta::from_millis(5000);

    pub fn new(config: LossBasedControlConfig) -> Self {
        Self {
            config,
            average_loss: 0.0,
            average_loss_max: 0.0,
            loss_based_bitrate: DataRate::zero(),
            acknowledged_bitrate_max: DataRate::zero(),
            acknowledged_bitrate_last_update: Timestamp::minus_infinity(),
            time_last_decrease: Timestamp::minus_infinity(),
            has_decreased_since_last_loss_report: false,
            last_loss_packet_report: Timestamp::minus_infinity(),
            last_loss_ratio: 0.0,
        }
    }

    // Returns the new estimate.
    pub fn update(
        &mut self,
        at_time: Timestamp,
        min_bitrate: DataRate,
        wanted_bitrate: DataRate,
        last_round_trip_time: TimeDelta,
    ) -> DataRate {
        if self.loss_based_bitrate.is_zero() {
            self.loss_based_bitrate = wanted_bitrate;
        }

        // Only increase if loss has been low for some time.
        let loss_estimate_for_increase: f64 = self.average_loss_max;
        // Avoid multiple decreases from averaging over one loss spike.
        let loss_estimate_for_decrease: f64 = self.average_loss.min(self.last_loss_ratio);
        let allow_decrease: bool = !self.has_decreased_since_last_loss_report
            && (at_time - self.time_last_decrease
                >= last_round_trip_time + self.config.decrease_interval);
        // If packet lost reports are too old, dont increase bitrate.
        let loss_report_valid: bool =
            at_time - self.last_loss_packet_report < 1.2 * Self::MAX_RTCP_FEEDBACK_INTERVAL;

        if loss_report_valid
            && self.config.allow_resets
            && loss_estimate_for_increase < self.loss_reset_threshold()
        {
            self.loss_based_bitrate = wanted_bitrate;
        } else if loss_report_valid && loss_estimate_for_increase < self.loss_increase_threshold() {
            // Increase bitrate by RTT-adaptive ratio.
            let new_increased_bitrate: DataRate = min_bitrate
                * get_increase_factor(&self.config, last_round_trip_time)
                + self.config.increase_offset;
            // The bitrate that would make the loss "just high enough".
            let new_increased_bitrate_cap: DataRate = bitrate_from_loss(
                loss_estimate_for_increase,
                self.config.loss_bandwidth_balance_increase,
                self.config.loss_bandwidth_balance_exponent,
            );
            let new_increased_bitrate =
                std::cmp::min(new_increased_bitrate, new_increased_bitrate_cap);
            self.loss_based_bitrate = std::cmp::max(new_increased_bitrate, self.loss_based_bitrate);
        } else if loss_estimate_for_decrease > self.loss_decrease_threshold() && allow_decrease {
            // The bitrate that would make the loss "just acceptable".
            let new_decreased_bitrate_floor: DataRate = bitrate_from_loss(
                loss_estimate_for_decrease,
                self.config.loss_bandwidth_balance_decrease,
                self.config.loss_bandwidth_balance_exponent,
            );
            let new_decreased_bitrate: DataRate =
                std::cmp::max(self.decreased_bitrate(), new_decreased_bitrate_floor);
            if new_decreased_bitrate < self.loss_based_bitrate {
                self.time_last_decrease = at_time;
                self.has_decreased_since_last_loss_report = true;
                self.loss_based_bitrate = new_decreased_bitrate;
            }
        }
        self.loss_based_bitrate
    }
    pub fn update_acknowledged_bitrate(
        &mut self,
        acknowledged_bitrate: DataRate,
        at_time: Timestamp,
    ) {
        let time_passed: TimeDelta = if self.acknowledged_bitrate_last_update.is_finite() {
            at_time - self.acknowledged_bitrate_last_update
        } else {
            TimeDelta::from_seconds(1)
        };
        self.acknowledged_bitrate_last_update = at_time;
        if acknowledged_bitrate > self.acknowledged_bitrate_max {
            self.acknowledged_bitrate_max = acknowledged_bitrate;
        } else {
            self.acknowledged_bitrate_max -=
                exponential_update(self.config.acknowledged_rate_max_window, time_passed)
                    * (self.acknowledged_bitrate_max - acknowledged_bitrate);
        }
    }
    pub fn initialize(&mut self, bitrate: DataRate) {
        self.loss_based_bitrate = bitrate;
        self.average_loss = 0.0;
        self.average_loss_max = 0.0;
    }
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }
    // Returns true if LossBasedBandwidthEstimation is enabled and have
    // received loss statistics. Ie, this class require transport feedback.
    pub fn in_use(&self) -> bool {
        self.enabled() && self.last_loss_packet_report.is_finite()
    }
    pub fn update_loss_statistics(&mut self, packet_results: &[PacketResult], at_time: Timestamp) {
        if packet_results.is_empty() {
            unreachable!();
        }
        let mut loss_count: i64 = 0;
        for pkt in packet_results {
            loss_count += if !pkt.is_received() { 1 } else { 0 };
        }
        self.last_loss_ratio = (loss_count) as f64 / packet_results.len() as f64;
        let time_passed: TimeDelta = if self.last_loss_packet_report.is_finite() {
            at_time - self.last_loss_packet_report
        } else {
            TimeDelta::from_seconds(1)
        };
        self.last_loss_packet_report = at_time;
        self.has_decreased_since_last_loss_report = false;

        self.average_loss += exponential_update(self.config.loss_window, time_passed)
            * (self.last_loss_ratio - self.average_loss);
        if self.average_loss > self.average_loss_max {
            self.average_loss_max = self.average_loss;
        } else {
            self.average_loss_max += exponential_update(self.config.loss_max_window, time_passed)
                * (self.average_loss - self.average_loss_max);
        }
    }

    /* unused
    pub fn get_estimate(&self) -> DataRate {
        self.loss_based_bitrate
    }
    */

    fn loss_increase_threshold(&self) -> f64 {
        loss_from_bitrate(
            self.loss_based_bitrate,
            self.config.loss_bandwidth_balance_increase,
            self.config.loss_bandwidth_balance_exponent,
        )
    }
    fn loss_decrease_threshold(&self) -> f64 {
        loss_from_bitrate(
            self.loss_based_bitrate,
            self.config.loss_bandwidth_balance_decrease,
            self.config.loss_bandwidth_balance_exponent,
        )
    }
    fn loss_reset_threshold(&self) -> f64 {
        loss_from_bitrate(
            self.loss_based_bitrate,
            self.config.loss_bandwidth_balance_reset,
            self.config.loss_bandwidth_balance_exponent,
        )
    }

    fn decreased_bitrate(&self) -> DataRate {
        self.config.decrease_factor * self.acknowledged_bitrate_max
    }
}

// Increase slower when RTT is high.
fn get_increase_factor(config: &LossBasedControlConfig, mut rtt: TimeDelta) -> f64 {
    // Clamp the RTT
    if rtt < config.increase_low_rtt {
        rtt = config.increase_low_rtt;
    } else if rtt > config.increase_high_rtt {
        rtt = config.increase_high_rtt;
    }
    let rtt_range = config.increase_high_rtt - config.increase_low_rtt;
    if rtt_range <= TimeDelta::zero() {
        unreachable!(); // Only on misconfiguration.
    }
    let rtt_offset = rtt - config.increase_low_rtt;
    let relative_offset = (rtt_offset / rtt_range).clamp(0.0, 1.0);
    let factor_range = config.max_increase_factor - config.min_increase_factor;
    config.min_increase_factor + (1.0 - relative_offset) * factor_range
}

fn loss_from_bitrate(bitrate: DataRate, loss_bandwidth_balance: DataRate, exponent: f64) -> f64 {
    if loss_bandwidth_balance >= bitrate {
        return 1.0;
    }
    (loss_bandwidth_balance / bitrate).powf(exponent)
}

fn bitrate_from_loss(loss: f64, loss_bandwidth_balance: DataRate, exponent: f64) -> DataRate {
    if exponent <= 0.0 {
        unreachable!();
    }
    if loss < 1e-5 {
        return DataRate::infinity();
    }
    loss_bandwidth_balance * loss.powf(-1.0 / exponent)
}

fn exponential_update(window: TimeDelta, interval: TimeDelta) -> f64 {
    // Use the convention that exponential window length (which is really
    // infinite) is the time it takes to dampen to 1/e.
    if window <= TimeDelta::zero() {
        unreachable!();
    }

    1.0 - (interval / window * -1.0).exp()
}
