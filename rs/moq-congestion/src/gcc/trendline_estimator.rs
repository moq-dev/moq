/*
 *  Copyright (c) 2016 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use std::collections::VecDeque;

use crate::{api::transport::BandwidthUsage, DelayIncreaseDetectorInterface};

// WebRTC-Bwe-TrendlineEstimatorSettings
#[derive(Debug, Clone)]
pub struct TrendlineEstimatorSettings {
    // Sort the packets in the window. Should be redundant,
    // but then almost no cost.
    pub enable_sort: bool,

    // Cap the trendline slope based on the minimum delay seen
    // in the beginning_packets and end_packets respectively.
    pub enable_cap: bool,
    pub beginning_packets: usize,
    pub end_packets: usize,
    pub cap_uncertainty: f64,

    // Size (in packets) of the window.
    pub window_size: usize,
}

impl TrendlineEstimatorSettings {
    const DEFAULT_TRENDLINE_WINDOW_SIZE: usize = 20;

    pub fn validate(&mut self) {
        if self.window_size < 10 || 200 < self.window_size {
            tracing::warn!("Window size must be between 10 and 200 packets");
            self.window_size = Self::DEFAULT_TRENDLINE_WINDOW_SIZE;
        }
        if self.enable_cap {
            if self.beginning_packets < 1
                || self.end_packets < 1
                || self.beginning_packets > self.window_size
                || self.end_packets > self.window_size
            {
                tracing::warn!(
                    "Size of beginning and end must be between 1 and {}",
                    self.window_size
                );
                self.enable_cap = false;
                self.beginning_packets = 0;
                self.end_packets = 0;
                self.cap_uncertainty = 0.0;
            }
            if self.beginning_packets + self.end_packets > self.window_size {
                tracing::warn!("Size of beginning plus end can't exceed the window size");
                self.enable_cap = false;
                self.beginning_packets = 0;
                self.end_packets = 0;
                self.cap_uncertainty = 0.0;
            }
            if self.cap_uncertainty < 0.0 || 0.025 < self.cap_uncertainty {
                tracing::warn!("Cap uncertainty must be between 0 and 0.025");
                self.cap_uncertainty = 0.0;
            }
        }
    }
}

impl Default for TrendlineEstimatorSettings {
    fn default() -> Self {
        Self {
            enable_sort: false,
            enable_cap: false,
            beginning_packets: 7,
            end_packets: 7,
            cap_uncertainty: 0.0,
            window_size: 20,
        }
    }
}

pub struct TrendlineEstimator {
    // Parameters.
    settings: TrendlineEstimatorSettings,
    smoothing_coef: f64,
    threshold_gain: f64,
    // Used by the existing threshold.
    num_of_deltas: i64,
    // Keep the arrival times small by using the change from the first packet.
    first_arrival_time_ms: i64,
    // Exponential backoff filtering.
    accumulated_delay: f64,
    smoothed_delay: f64,
    // Linear least squares regression.
    delay_hist: VecDeque<PacketTiming>,

    k_up: f64,
    k_down: f64,
    overusing_time_threshold: f64,
    threshold: f64,
    prev_modified_trend: f64,
    last_update_ms: i64,
    prev_trend: f64,
    time_over_using: f64,
    overuse_counter: i64,
    hypothesis: BandwidthUsage,
}

struct PacketTiming {
    pub arrival_time_ms: f64,
    pub smoothed_delay_ms: f64,
    pub raw_delay_ms: f64,
}

impl Default for TrendlineEstimator {
    fn default() -> Self {
        Self::new(TrendlineEstimatorSettings::default())
    }
}

impl DelayIncreaseDetectorInterface for TrendlineEstimator {
    fn update(
        &mut self,
        recv_delta_ms: f64,
        send_delta_ms: f64,
        send_time_ms: i64,
        arrival_time_ms: i64,
        packet_size: usize,
        calculated_deltas: bool,
    ) {
        if calculated_deltas {
            self.update_trendline(
                recv_delta_ms,
                send_delta_ms,
                send_time_ms,
                arrival_time_ms,
                packet_size,
            );
        }
    }
    fn state(&self) -> BandwidthUsage {
        self.hypothesis
    }
}

fn linear_fit_slope(packets: &VecDeque<PacketTiming>) -> Option<f64> {
    assert!(packets.len() >= 2);
    // Compute the "center of mass".
    let mut sum_x: f64 = 0.0;
    let mut sum_y: f64 = 0.0;
    for packet in packets {
        sum_x += packet.arrival_time_ms;
        sum_y += packet.smoothed_delay_ms;
    }
    let x_avg: f64 = sum_x / packets.len() as f64;
    let y_avg: f64 = sum_y / packets.len() as f64;
    // Compute the slope k = \sum (x_i-x_avg)(y_i-y_avg) / \sum (x_i-x_avg)^2
    let mut numerator: f64 = 0.0;
    let mut denominator: f64 = 0.0;
    for packet in packets.iter() {
        let x: f64 = packet.arrival_time_ms;
        let y: f64 = packet.smoothed_delay_ms;
        numerator += (x - x_avg) * (y - y_avg);
        denominator += (x - x_avg) * (x - x_avg);
    }
    if denominator == 0.0 {
        return None;
    }
    Some(numerator / denominator)
}

fn compute_slope_cap(
    packets: &VecDeque<PacketTiming>,
    settings: &TrendlineEstimatorSettings,
) -> Option<f64> {
    assert!(1 <= settings.beginning_packets && settings.beginning_packets < packets.len());
    assert!(1 <= settings.end_packets && settings.end_packets < packets.len());
    assert!(settings.beginning_packets + settings.end_packets <= packets.len());
    let mut early = &packets[0];
    for packet in packets.iter().take(settings.beginning_packets).skip(1) {
        if packet.raw_delay_ms < early.raw_delay_ms {
            early = packet;
        }
    }
    let late_start: usize = packets.len() - settings.end_packets;
    let mut late = &packets[late_start];
    for packet in packets.iter().skip(late_start + 1) {
        if packet.raw_delay_ms < late.raw_delay_ms {
            late = packet;
        }
    }
    if late.arrival_time_ms - early.arrival_time_ms < 1.0 {
        return None;
    }
    Some(
        (late.raw_delay_ms - early.raw_delay_ms) / (late.arrival_time_ms - early.arrival_time_ms)
            + settings.cap_uncertainty,
    )
}

impl TrendlineEstimator {
    const DEFAULT_TRENDLINE_SMOOTHING_COEFF: f64 = 0.9;
    const DEFAULT_TRENDLINE_THRESHOLD_GAIN: f64 = 4.0;
    const MAX_ADAPT_OFFSET_MS: f64 = 15.0;
    const OVER_USING_TIME_THRESHOLD: f64 = 10.0;
    const MIN_NUM_DELTAS: i64 = 60;
    const DELTA_COUNTER_MAX: i64 = 1000;

    pub fn new(mut settings: TrendlineEstimatorSettings) -> Self {
        settings.validate();

        tracing::debug!("Using Trendline filter for delay change estimation with settings {:?} and no network state predictor", settings);
        Self {
            settings,
            smoothing_coef: Self::DEFAULT_TRENDLINE_SMOOTHING_COEFF,
            threshold_gain: Self::DEFAULT_TRENDLINE_THRESHOLD_GAIN,
            num_of_deltas: 0,
            first_arrival_time_ms: -1,
            accumulated_delay: 0.0,
            smoothed_delay: 0.0,
            delay_hist: VecDeque::new(),
            k_up: 0.0087,
            k_down: 0.039,
            overusing_time_threshold: Self::OVER_USING_TIME_THRESHOLD,
            threshold: 12.5,
            prev_modified_trend: f64::NAN,
            last_update_ms: -1,
            prev_trend: 0.0,
            time_over_using: -1.0,
            overuse_counter: 0,
            hypothesis: BandwidthUsage::Normal,
        }
    }

    fn update_trendline(
        &mut self,
        recv_delta_ms: f64,
        send_delta_ms: f64,
        _send_time_ms: i64,
        arrival_time_ms: i64,
        _packet_size: usize,
    ) {
        let delta_ms: f64 = recv_delta_ms - send_delta_ms;
        self.num_of_deltas += 1;
        self.num_of_deltas = self.num_of_deltas.min(Self::DELTA_COUNTER_MAX);
        if self.first_arrival_time_ms == -1 {
            self.first_arrival_time_ms = arrival_time_ms;
        }

        // Exponential backoff filter.
        self.accumulated_delay += delta_ms;
        self.smoothed_delay = self.smoothing_coef * self.smoothed_delay
            + (1.0 - self.smoothing_coef) * self.accumulated_delay;

        // Maintain packet window
        self.delay_hist.push_back(PacketTiming {
            arrival_time_ms: (arrival_time_ms - self.first_arrival_time_ms) as f64,
            smoothed_delay_ms: self.smoothed_delay,
            raw_delay_ms: self.accumulated_delay,
        });
        if self.settings.enable_sort {
            let mut i = self.delay_hist.len() - 1;
            while i > 0
                && self.delay_hist[i].arrival_time_ms < self.delay_hist[i - 1].arrival_time_ms
            {
                self.delay_hist.swap(i, i - 1);
                i -= 1;
            }
        }
        if self.delay_hist.len() > self.settings.window_size {
            self.delay_hist.pop_front();
        }

        // Simple linear regression.
        let mut trend: f64 = self.prev_trend;
        if self.delay_hist.len() == self.settings.window_size {
            // Update self.trend if it is possible to fit a line to the data. The delay
            // trend can be seen as an estimate of (send_rate - capacity)/capacity.
            // 0 < trend < 1   .  the delay increases, queues are filling up
            //   trend == 0    .  the delay does not change
            //   trend < 0     .  the delay decreases, queues are being emptied
            trend = linear_fit_slope(&self.delay_hist).unwrap_or(trend);
            if self.settings.enable_cap {
                let cap = compute_slope_cap(&self.delay_hist, &self.settings);
                // We only use the cap to filter out overuse detections, not
                // to detect additional underuses.
                if let Some(cap) = cap {
                    if trend >= 0.0 && trend > cap {
                        trend = cap;
                    }
                }
            }
        }
        self.detect(trend, send_delta_ms, arrival_time_ms);
    }

    fn detect(&mut self /* TrendlineEstimator */, trend: f64, ts_delta: f64, now_ms: i64) {
        if self.num_of_deltas < 2 {
            self.hypothesis = BandwidthUsage::Normal;
            return;
        }
        let modified_trend: f64 =
            self.num_of_deltas.min(Self::MIN_NUM_DELTAS) as f64 * trend * self.threshold_gain;
        self.prev_modified_trend = modified_trend;
        if modified_trend > self.threshold {
            if self.time_over_using == -1.0 {
                // Initialize the timer. Assume that we've been
                // over-using half of the time since the previous
                // sample.
                self.time_over_using = ts_delta / 2.0;
            } else {
                // Increment timer
                self.time_over_using += ts_delta;
            }
            self.overuse_counter += 1;
            if self.time_over_using > self.overusing_time_threshold
                && self.overuse_counter > 1
                && trend >= self.prev_trend
            {
                self.time_over_using = 0.0;
                self.overuse_counter = 0;
                self.hypothesis = BandwidthUsage::Overusing;
            }
        } else if modified_trend < -self.threshold {
            self.time_over_using = -1.0;
            self.overuse_counter = 0;
            self.hypothesis = BandwidthUsage::Underusing;
        } else {
            self.time_over_using = -1.0;
            self.overuse_counter = 0;
            self.hypothesis = BandwidthUsage::Normal;
        }
        self.prev_trend = trend;
        self.update_threshold(modified_trend, now_ms);
    }

    fn update_threshold(&mut self /* TrendlineEstimator */, modified_trend: f64, now_ms: i64) {
        if self.last_update_ms == -1 {
            self.last_update_ms = now_ms;
        }

        if modified_trend.abs() > self.threshold + Self::MAX_ADAPT_OFFSET_MS {
            // Avoid adapting the threshold to big latency spikes, caused e.g.,
            // by a sudden capacity drop.
            self.last_update_ms = now_ms;
            return;
        }

        let k: f64 = match modified_trend.abs() < self.threshold {
            true => self.k_down,
            false => self.k_up,
        };
        const MAX_TIME_DELTA_MS: i64 = 100;
        let time_delta_ms: i64 = std::cmp::min(now_ms - self.last_update_ms, MAX_TIME_DELTA_MS);
        self.threshold += k * (modified_trend.abs() - self.threshold) * time_delta_ms as f64;
        self.threshold = self.threshold.clamp(6.0, 600.0);
        self.last_update_ms = now_ms;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    struct PacketTimeGenerator {
        initial_clock: i64,
        time_between_packets: f64,
        packets: usize,
    }

    impl PacketTimeGenerator {
        pub fn new(initial_clock: i64, time_between_packets: f64) -> Self {
            Self {
                initial_clock,
                time_between_packets,
                packets: 0,
            }
        }
    }

    impl Iterator for PacketTimeGenerator {
        type Item = i64;

        fn next(&mut self) -> Option<Self::Item> {
            let value =
                self.initial_clock + (self.time_between_packets * self.packets as f64) as i64;
            self.packets += 1;
            Some(value)
        }
    }

    struct TrendlineEstimatorTest {
        send_times: Vec<i64>,
        recv_times: Vec<i64>,
        packet_sizes: Vec<usize>,
        estimator: TrendlineEstimator,
        count: usize,
    }

    impl TrendlineEstimatorTest {
        const PACKET_COUNT: usize = 25;
        const PACKET_SIZE_BYTES: usize = 1200;

        pub fn new(send_times: PacketTimeGenerator, recv_times: PacketTimeGenerator) -> Self {
            Self {
                send_times: send_times.take(Self::PACKET_COUNT).collect(),
                recv_times: recv_times.take(Self::PACKET_COUNT).collect(),
                packet_sizes: vec![Self::PACKET_SIZE_BYTES; Self::PACKET_COUNT],
                estimator: TrendlineEstimator::default(),
                count: 1,
            }
        }

        fn run_test_until_state_change(&mut self) {
            assert_eq!(self.send_times.len(), Self::PACKET_COUNT);
            assert_eq!(self.recv_times.len(), Self::PACKET_COUNT);
            assert_eq!(self.packet_sizes.len(), Self::PACKET_COUNT);
            assert!(self.count >= 1);
            assert!(self.count < Self::PACKET_COUNT);

            let initial_state = self.estimator.state();
            while self.count < Self::PACKET_COUNT {
                let recv_delta: f64 =
                    (self.recv_times[self.count] - self.recv_times[self.count - 1]) as f64;
                let send_delta: f64 =
                    (self.send_times[self.count] - self.send_times[self.count - 1]) as f64;
                self.estimator.update(
                    recv_delta,
                    send_delta,
                    self.send_times[self.count],
                    self.recv_times[self.count],
                    self.packet_sizes[self.count],
                    true,
                );
                if self.estimator.state() != initial_state {
                    return;
                }

                self.count += 1;
            }
        }
    }

    #[test]
    fn normal() {
        let send_time_generator = PacketTimeGenerator::new(
            123456789, /*initial clock*/
            20.0,      /*20 ms between sent packets*/
        );
        let recv_time_generator = PacketTimeGenerator::new(
            987654321, /*initial clock*/
            20.0,      /*delivered at the same pace*/
        );

        let mut test = TrendlineEstimatorTest::new(send_time_generator, recv_time_generator);

        assert_eq!(test.estimator.state(), BandwidthUsage::Normal);
        test.run_test_until_state_change();
        assert_eq!(test.estimator.state(), BandwidthUsage::Normal);
        assert_eq!(test.count, TrendlineEstimatorTest::PACKET_COUNT); // All packets processed
    }

    #[test]
    fn overusing() {
        let send_time_generator = PacketTimeGenerator::new(
            123456789, /*initial clock*/
            20.0,      /*20 ms between sent packets*/
        );
        let recv_time_generator = PacketTimeGenerator::new(
            987654321,  /*initial clock*/
            1.1 * 20.0, /*10% slower delivery*/
        );
        let mut test = TrendlineEstimatorTest::new(send_time_generator, recv_time_generator);

        assert_eq!(test.estimator.state(), BandwidthUsage::Normal);
        test.run_test_until_state_change();
        assert_eq!(test.estimator.state(), BandwidthUsage::Overusing);
        test.run_test_until_state_change();
        assert_eq!(test.estimator.state(), BandwidthUsage::Overusing);
        assert_eq!(test.count, TrendlineEstimatorTest::PACKET_COUNT); // All packets processed
    }

    #[test]
    fn underusing() {
        let send_time_generator = PacketTimeGenerator::new(
            123456789, /*initial clock*/
            20.0,      /*20 ms between sent packets*/
        );
        let recv_time_generator = PacketTimeGenerator::new(
            987654321,   /*initial clock*/
            0.85 * 20.0, /*15% faster delivery*/
        );
        let mut test = TrendlineEstimatorTest::new(send_time_generator, recv_time_generator);

        assert_eq!(test.estimator.state(), BandwidthUsage::Normal);
        test.run_test_until_state_change();
        assert_eq!(test.estimator.state(), BandwidthUsage::Underusing);
        test.run_test_until_state_change();
        assert_eq!(test.estimator.state(), BandwidthUsage::Underusing);
        assert_eq!(test.count, TrendlineEstimatorTest::PACKET_COUNT); // All packets processed
    }

    #[test]
    fn includes_small_packets_by_default() {
        let send_time_generator = PacketTimeGenerator::new(
            123456789, /*initial clock*/
            20.0,      /*20 ms between sent packets*/
        );
        let recv_time_generator = PacketTimeGenerator::new(
            987654321,  /*initial clock*/
            1.1 * 20.0, /*10% slower delivery*/
        );
        let mut test = TrendlineEstimatorTest::new(send_time_generator, recv_time_generator);
        test.packet_sizes = vec![100; TrendlineEstimatorTest::PACKET_COUNT];

        assert_eq!(test.estimator.state(), BandwidthUsage::Normal);
        test.run_test_until_state_change();
        assert_eq!(test.estimator.state(), BandwidthUsage::Overusing);
        test.run_test_until_state_change();
        assert_eq!(test.estimator.state(), BandwidthUsage::Overusing);
        assert_eq!(test.count, TrendlineEstimatorTest::PACKET_COUNT); // All packets processed
    }
} // namespace webrtc
