/*
 *  Copyright (c) 2017 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::units::{DataRate, DataSize, TimeDelta, Timestamp};

// WebRTC-BweThroughputWindowConfig
#[derive(Debug, Clone)]
pub struct BitrateEstimatorConfig {
    pub initial_window_ms: i64,
    pub window_ms: i64,
    pub scale: f64,
    pub scale_alr: f64,
    pub scale_small: f64,
    pub small_thresh: DataSize,
    pub symmetry_cap: DataRate,
    pub floor: DataRate,
}

impl BitrateEstimatorConfig {
    const INITIAL_RATE_WINDOW_MS: i64 = 500;
    const RATE_WINDOW_MS: i64 = 150;
    const MIN_RATE_WINDOW_MS: i64 = 150;
    const MAX_RATE_WINDOW_MS: i64 = 1000;
}

impl Default for BitrateEstimatorConfig {
    fn default() -> Self {
        Self {
            initial_window_ms: Self::INITIAL_RATE_WINDOW_MS,
            window_ms: Self::RATE_WINDOW_MS,
            scale: 10.0,
            scale_alr: 10.0,   // scale
            scale_small: 10.0, // scale
            small_thresh: DataSize::zero(),
            symmetry_cap: DataRate::zero(),
            floor: DataRate::zero(),
        }
    }
}

impl BitrateEstimatorConfig {
    pub fn validate(&mut self) {
        self.initial_window_ms = self
            .initial_window_ms
            .clamp(Self::MAX_RATE_WINDOW_MS, Self::MAX_RATE_WINDOW_MS);
        self.window_ms = self
            .window_ms
            .clamp(Self::MIN_RATE_WINDOW_MS, Self::MAX_RATE_WINDOW_MS);
    }
}

// Computes a bayesian estimate of the throughput given acks containing
// the arrival time and payload size. Samples which are far from the current
// estimate or are based on few packets are given a smaller weight, as they
// are considered to be more likely to have been caused by, e.g., delay spikes
// unrelated to congestion.
pub struct BitrateEstimator {
    sum: i64,
    initial_window_ms: i64,
    noninitial_window_ms: i64,
    uncertainty_scale: f64,
    uncertainty_scale_in_alr: f64,
    small_sample_uncertainty_scale: f64,
    small_sample_threshold: DataSize,
    uncertainty_symmetry_cap: DataRate,
    estimate_floor: DataRate,
    current_window_ms: i64,
    prev_time_ms: i64,
    bitrate_estimate_kbps: f64,
    bitrate_estimate_var: f64,
}

impl Default for BitrateEstimator {
    fn default() -> Self {
        Self::new(BitrateEstimatorConfig::default())
    }
}

impl BitrateEstimator {
    pub fn new(mut config: BitrateEstimatorConfig) -> Self {
        config.validate();
        Self {
            sum: 0,
            initial_window_ms: config.initial_window_ms,
            noninitial_window_ms: config.window_ms,
            uncertainty_scale: config.scale,
            uncertainty_scale_in_alr: config.scale_alr,
            small_sample_uncertainty_scale: config.scale_small,
            small_sample_threshold: config.small_thresh,
            uncertainty_symmetry_cap: config.symmetry_cap,
            estimate_floor: config.floor,
            current_window_ms: 0,
            prev_time_ms: -1,
            bitrate_estimate_kbps: -1.0,
            bitrate_estimate_var: 50.0,
        }
    }

    pub fn update(&mut self, at_time: Timestamp, amount: DataSize, in_alr: bool) {
        let mut rate_window_ms: i64 = self.noninitial_window_ms;
        // We use a larger window at the beginning to get a more stable sample that
        // we can use to initialize the estimate.
        if self.bitrate_estimate_kbps < 0.0 {
            rate_window_ms = self.initial_window_ms;
        }
        let mut is_small_sample: bool = false;
        let bitrate_sample_kbps: f64 = self.update_window(
            at_time.ms(),
            amount.bytes(),
            rate_window_ms,
            &mut is_small_sample,
        );
        if bitrate_sample_kbps < 0.0 {
            return;
        }
        if self.bitrate_estimate_kbps < 0.0 {
            // This is the very first sample we get. Use it to initialize the estimate.
            self.bitrate_estimate_kbps = bitrate_sample_kbps;
            return;
        }
        // Optionally use higher uncertainty for very small samples to avoid dropping
        // estimate and for samples obtained in ALR.
        let mut scale: f64 = self.uncertainty_scale;
        if is_small_sample && bitrate_sample_kbps < self.bitrate_estimate_kbps {
            scale = self.small_sample_uncertainty_scale;
        } else if in_alr && bitrate_sample_kbps < self.bitrate_estimate_kbps {
            // Optionally use higher uncertainty for samples obtained during ALR.
            scale = self.uncertainty_scale_in_alr;
        }
        // Define the sample uncertainty as a function of how far away it is from the
        // current estimate. With low values of self.uncertainty_symmetry_cap we add more
        // uncertainty to increases than to decreases. For higher values we approach
        // symmetry.
        let sample_uncertainty: f64 = scale
            * (self.bitrate_estimate_kbps - bitrate_sample_kbps).abs()
            / (self.bitrate_estimate_kbps
                + bitrate_sample_kbps.max(self.uncertainty_symmetry_cap.kbps_float()));

        let sample_var: f64 = sample_uncertainty * sample_uncertainty;
        // Update a bayesian estimate of the rate, weighting it lower if the sample
        // uncertainty is large.
        // The bitrate estimate uncertainty is increased with each update to model
        // that the bitrate changes over time.
        let pred_bitrate_estimate_var: f64 = self.bitrate_estimate_var + 5.0;
        self.bitrate_estimate_kbps = (sample_var * self.bitrate_estimate_kbps
            + pred_bitrate_estimate_var * bitrate_sample_kbps)
            / (sample_var + pred_bitrate_estimate_var);
        self.bitrate_estimate_kbps = self
            .bitrate_estimate_kbps
            .max(self.estimate_floor.kbps_float());
        self.bitrate_estimate_var =
            sample_var * pred_bitrate_estimate_var / (sample_var + pred_bitrate_estimate_var);
    }

    pub fn bitrate(&self) -> Option<DataRate> {
        if self.bitrate_estimate_kbps < 0.0 {
            return None;
        }
        Some(DataRate::from_kilobits_per_sec_float(
            self.bitrate_estimate_kbps,
        ))
    }

    pub fn peek_rate(&self) -> Option<DataRate> {
        if self.current_window_ms > 0 {
            return Some(
                DataSize::from_bytes(self.sum) / TimeDelta::from_millis(self.current_window_ms),
            );
        }
        None
    }

    pub fn expect_fast_rate_change(&mut self) {
        // By setting the bitrate-estimate variance to a higher value we allow the
        // bitrate to change fast for the next few samples.
        self.bitrate_estimate_var += 200.0;
    }

    fn update_window(
        &mut self,
        now_ms: i64,
        bytes: i64,
        rate_window_ms: i64,
        is_small_sample: &mut bool,
    ) -> f64 {
        // Reset if time moves backwards.
        if now_ms < self.prev_time_ms {
            self.prev_time_ms = -1;
            self.sum = 0;
            self.current_window_ms = 0;
        }
        if self.prev_time_ms >= 0 {
            self.current_window_ms += now_ms - self.prev_time_ms;
            // Reset if nothing has been received for more than a full window.
            if now_ms - self.prev_time_ms > rate_window_ms {
                self.sum = 0;
                self.current_window_ms %= rate_window_ms;
            }
        }
        self.prev_time_ms = now_ms;
        let mut bitrate_sample: f64 = -1.0;
        if self.current_window_ms >= rate_window_ms {
            *is_small_sample = self.sum < self.small_sample_threshold.bytes();
            bitrate_sample = 8.0 * self.sum as f64 / (rate_window_ms) as f64;
            self.current_window_ms -= rate_window_ms;
            self.sum = 0;
        }
        self.sum += bytes;
        bitrate_sample
    }
}
