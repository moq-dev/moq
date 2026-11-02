/*
 *  Copyright (c) 2016 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::gcc::{
    api::units::{DataRate, DataSize, Timestamp},
    pacing::IntervalBudget,
};

#[derive(Clone, Debug)]
pub struct AlrDetectorConfig {
    // Sent traffic ratio as a function of network capacity used to determine
    // application-limited region. ALR region start when bandwidth usage drops
    // below AlrStartUsageRatio and ends when it raises above
    // AlrEndUsageRatio. NOTE: This is intentionally conservative at the moment
    // until BW adjustments of application limited region is fine tuned.
    pub bandwidth_usage_ratio: f64,
    pub start_budget_level_ratio: f64,
    pub stop_budget_level_ratio: f64,
}

impl Default for AlrDetectorConfig {
    fn default() -> Self {
        // WebRTC-ProbingScreenshareBwe is default-on with the fixed settings
        // "1.0,2875,80,40,-60,3", which override the detector's own defaults.
        Self {
            bandwidth_usage_ratio: 0.80,
            start_budget_level_ratio: 0.40,
            stop_budget_level_ratio: -0.60,
        }
    }
}

// Application limited region detector is a class that utilizes signals of
// elapsed time and bytes sent to estimate whether network traffic is
// currently limited by the application's ability to generate traffic.
//
// AlrDetector provides a signal that can be utilized to adjust
// estimate bandwidth.
// Note: This class is not thread-safe.
#[derive(Clone)]
pub struct AlrDetector {
    conf: AlrDetectorConfig,

    last_send_time: Option<Timestamp>,

    alr_budget: IntervalBudget,
    alr_started_time: Option<Timestamp>,
}

impl Default for AlrDetector {
    fn default() -> Self {
        Self::new(AlrDetectorConfig::default())
    }
}

impl AlrDetector {
    pub fn new(conf: AlrDetectorConfig) -> Self {
        Self {
            conf,
            last_send_time: None,
            alr_budget: IntervalBudget::new(0, true),
            alr_started_time: None,
        }
    }

    // Upstream stamps the region's start with the clock when the send is processed; the send
    // time is that instant here, and keeps the detector on the controller's mocked time.
    pub fn on_bytes_sent(&mut self, bytes_sent: DataSize, send_time: Timestamp) {
        let Some(last_send_time) = self.last_send_time else {
            // Since the duration for sending the bytes is unknwon, return without
            // updating alr state.
            self.last_send_time = Some(send_time);
            return;
        };
        // A deliberate departure from upstream, which rounds each interval to whole milliseconds
        // (`(send_time - last_send_time).ms()`), so sends under half a millisecond apart never
        // refill its budget and sends 0.5-1ms apart refill it for a whole millisecond. Rounding
        // each time instead, as upstream did before its send times became `Timestamp`s, keeps the
        // intervals adding up to the time between the sends.
        let delta_time_ms = send_time.ms() - last_send_time.ms();
        self.last_send_time = Some(send_time);

        self.alr_budget.use_budget(bytes_sent.bytes() as usize);
        self.alr_budget.increase_budget(delta_time_ms);
        if self.alr_budget.budget_ratio() > self.conf.start_budget_level_ratio
            && self.alr_started_time.is_none()
        {
            self.alr_started_time = Some(send_time);
        } else if self.alr_budget.budget_ratio() < self.conf.stop_budget_level_ratio
            && self.alr_started_time.is_some()
        {
            self.alr_started_time = None;
        }
    }

    // Set current estimated bandwidth.
    pub fn set_estimated_bitrate(&mut self, bitrate: DataRate) {
        debug_assert!(bitrate > DataRate::zero());
        self.alr_budget
            .set_target_rate_kbps((bitrate * self.conf.bandwidth_usage_ratio).kbps());
    }

    // Returns time when the current application-limited region started or empty
    // result if the sender is currently not application-limited.
    pub fn get_application_limited_region_start_time(&self) -> Option<Timestamp> {
        self.alr_started_time
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::gcc::api::units::TimeDelta;

    const ESTIMATED_BITRATE_BPS: i64 = 300000;

    pub struct SimulateOutgoingTrafficIn<'a> {
        alr_detector: &'a mut AlrDetector,
        timestamp_ms: &'a mut i64,
        interval_ms: Option<i64>,
        usage_percentage: Option<i64>,
    }

    impl<'a> SimulateOutgoingTrafficIn<'a> {
        pub fn new(alr_detector: &'a mut AlrDetector, timestamp_ms: &'a mut i64) -> Self {
            Self {
                alr_detector,
                timestamp_ms,
                interval_ms: None,
                usage_percentage: None,
            }
        }

        pub fn for_time_ms(mut self, time_ms: i64) -> Self {
            self.interval_ms.replace(time_ms);
            self
        }

        pub fn at_percent_of_estimated_bitrate(mut self, usage_percentage: i64) {
            self.usage_percentage.replace(usage_percentage);
            self.produce_traffic();
        }

        fn produce_traffic(&mut self) {
            let interval_ms = self.interval_ms.unwrap();
            let usage_percentage = self.usage_percentage.unwrap();
            const TIME_STEP_MS: i64 = 10;
            let mut t: i64 = 0;

            while t < interval_ms {
                *self.timestamp_ms += TIME_STEP_MS;
                self.alr_detector.on_bytes_sent(
                    DataSize::from_bytes(
                        ESTIMATED_BITRATE_BPS * usage_percentage * TIME_STEP_MS / (8 * 100 * 1000),
                    ),
                    Timestamp::from_millis(*self.timestamp_ms),
                );
                t += TIME_STEP_MS;
            }
            let remainder_ms: i64 = interval_ms % TIME_STEP_MS;
            if remainder_ms > 0 {
                *self.timestamp_ms += TIME_STEP_MS;
                self.alr_detector.on_bytes_sent(
                    DataSize::from_bytes(
                        ESTIMATED_BITRATE_BPS * usage_percentage * remainder_ms / (8 * 100 * 1000),
                    ),
                    Timestamp::from_millis(*self.timestamp_ms),
                );
            }
        }
    }

    #[test]
    fn alr_detection() {
        let mut timestamp_ms: i64 = 1000;
        let mut alr_detector = AlrDetector::default();
        alr_detector.set_estimated_bitrate(DataRate::from_bits_per_sec(ESTIMATED_BITRATE_BPS));

        // Start in non-ALR state.
        assert!(
            alr_detector
                .get_application_limited_region_start_time()
                .is_none()
        );

        // Stay in non-ALR state when usage is close to 100%.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(1000)
            .at_percent_of_estimated_bitrate(90);
        assert!(
            alr_detector
                .get_application_limited_region_start_time()
                .is_none()
        );

        // Verify that we ALR starts when bitrate drops below 20%.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(1500)
            .at_percent_of_estimated_bitrate(20);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_some())
        );

        // Verify that ALR ends when usage is above 65%.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(4000)
            .at_percent_of_estimated_bitrate(100);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_none())
        );
    }

    #[test]
    fn short_spike() {
        let mut timestamp_ms: i64 = 1000;
        let mut alr_detector = AlrDetector::default();
        alr_detector.set_estimated_bitrate(DataRate::from_bits_per_sec(ESTIMATED_BITRATE_BPS));
        // Start in non-ALR state.
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_none())
        );

        // Verify that we ALR starts when bitrate drops below 20%.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(1000)
            .at_percent_of_estimated_bitrate(20);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_some())
        );

        // Verify that we stay in ALR region even after a short bitrate spike.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(100)
            .at_percent_of_estimated_bitrate(150);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_some())
        );

        // ALR ends when usage is above 65%.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(3000)
            .at_percent_of_estimated_bitrate(100);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_none())
        );
    }

    #[test]
    fn bandwidth_estimate_changes() {
        let mut timestamp_ms: i64 = 1000;
        let mut alr_detector = AlrDetector::default();
        alr_detector.set_estimated_bitrate(DataRate::from_bits_per_sec(ESTIMATED_BITRATE_BPS));

        // Start in non-ALR state.
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_none())
        );

        // ALR starts when bitrate drops below 20%.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(1000)
            .at_percent_of_estimated_bitrate(20);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_some())
        );

        // When bandwidth estimate drops the detector should stay in ALR mode and quit
        // it shortly afterwards as the sender continues sending the same amount of
        // traffic. This is necessary to ensure that ProbeController can still react
        // to the BWE drop by initiating a new probe.
        alr_detector.set_estimated_bitrate(DataRate::from_bits_per_sec(ESTIMATED_BITRATE_BPS / 5));
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_some())
        );
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(1000)
            .at_percent_of_estimated_bitrate(50);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_none())
        );
    }

    #[test]
    fn parse_alr_specific_field_trial() {
        let config = AlrDetectorConfig {
            bandwidth_usage_ratio: 0.90,
            start_budget_level_ratio: 0.0,
            stop_budget_level_ratio: -0.10,
        };
        let mut alr_detector = AlrDetector::new(config);
        let mut timestamp_ms: i64 = 1000;
        alr_detector.set_estimated_bitrate(DataRate::from_bits_per_sec(ESTIMATED_BITRATE_BPS));

        // Start in non-ALR state.
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_none())
        );

        // ALR does not start at 100% utilization.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(1000)
            .at_percent_of_estimated_bitrate(100);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_none())
        );

        // ALR does start at 85% utilization.
        // Overused 10% above so it should take about 2s to reach a budget level of
        // 0%.
        SimulateOutgoingTrafficIn::new(&mut alr_detector, &mut timestamp_ms)
            .for_time_ms(2100)
            .at_percent_of_estimated_bitrate(85);
        assert!(
            (alr_detector
                .get_application_limited_region_start_time()
                .is_some())
        );
    }

    /// Packets under a millisecond apart refill the budget for the time between them: 1200 bytes
    /// every 400us is 24 Mbit/s, under 80% of a 40 Mbit/s estimate, so the sender is
    /// application-limited
    #[test]
    fn sub_millisecond_sends_refill_the_budget() {
        let mut alr_detector = AlrDetector::default();
        alr_detector.set_estimated_bitrate(DataRate::from_kilobits_per_sec(40_000));
        let mut send_time = Timestamp::from_millis(1000);
        for _ in 0..10_000 {
            alr_detector.on_bytes_sent(DataSize::from_bytes(1200), send_time);
            send_time += TimeDelta::from_micros(400);
        }
        assert!(
            alr_detector
                .get_application_limited_region_start_time()
                .is_some()
        );
    }
}
