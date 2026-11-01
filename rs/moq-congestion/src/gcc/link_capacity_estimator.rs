/*
 *  Copyright 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::units::DataRate;

pub struct LinkCapacityEstimator {
    estimate_kbps: Option<f64>,
    deviation_kbps: f64,
}

impl Default for LinkCapacityEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkCapacityEstimator {
    pub fn new() -> Self {
        Self {
            estimate_kbps: None,
            deviation_kbps: 0.4,
        }
    }
    pub fn upper_bound(&self) -> DataRate {
        if let Some(estimate_kbps) = self.estimate_kbps {
            DataRate::from_kilobits_per_sec_float(
                estimate_kbps + 3.0 * self.deviation_estimate_kbps(estimate_kbps),
            )
        } else {
            DataRate::infinity()
        }
    }

    pub fn lower_bound(&self) -> DataRate {
        if let Some(estimate_kbps) = self.estimate_kbps {
            DataRate::from_kilobits_per_sec_float(
                (estimate_kbps - 3.0 * self.deviation_estimate_kbps(estimate_kbps)).max(0.0),
            )
        } else {
            DataRate::zero()
        }
    }

    pub fn reset(&mut self) {
        self.estimate_kbps.take();
    }

    pub fn on_overuse_detected(&mut self, acknowledged_rate: DataRate) {
        self.update(acknowledged_rate, 0.05);
    }

    /* unused
    pub fn on_probe_rate(&mut self, probe_rate: DataRate) {
        self.update(probe_rate, 0.5);
    }
    */

    pub fn has_estimate(&self) -> bool {
        self.estimate_kbps.is_some()
    }

    pub fn estimate(&self) -> DataRate {
        DataRate::from_kilobits_per_sec_float(self.estimate_kbps.unwrap_or_default())
    }

    fn update(&mut self, capacity_sample: DataRate, alpha: f64) {
        let sample_kbps: f64 = capacity_sample.kbps_float();
        let estimate_kbps = if let Some(estimate_kbps) = self.estimate_kbps {
            (1.0 - alpha) * estimate_kbps + alpha * sample_kbps
        } else {
            sample_kbps
        };

        // Estimate the variance of the link capacity estimate and normalize the
        // variance with the link capacity estimate.
        let norm: f64 = estimate_kbps.max(1.0);
        let error_kbps: f64 = estimate_kbps - sample_kbps;
        self.deviation_kbps =
            (1.0 - alpha) * self.deviation_kbps + alpha * error_kbps * error_kbps / norm;
        // 0.4 ~= 14 kbit/s at 500 kbit/s
        // 2.5f ~= 35 kbit/s at 500 kbit/s
        self.deviation_kbps = self.deviation_kbps.clamp(0.4, 2.5);
        self.estimate_kbps = Some(estimate_kbps);
    }

    fn deviation_estimate_kbps(&self, estimate_kbps: f64) -> f64 {
        // Calculate the max bit rate std dev given the normalized
        // variance and the current throughput bitrate. The standard deviation will
        // only be used if self.estimate_kbps has a value.
        (self.deviation_kbps * estimate_kbps).sqrt()
    }
}
