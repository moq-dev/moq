/*
 *  Copyright (c) 2017 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::{
    api::{
        transport::PacketResult,
        units::{DataRate, Timestamp},
    },
    experiments::FieldTrials,
    AcknowledgedBitrateEstimatorInterface, BitrateEstimator, RobustThroughputEstimator,
};

pub struct AcknowledgedBitrateEstimator {
    alr_ended_time: Option<Timestamp>,
    in_alr: bool,
    bitrate_estimator: BitrateEstimator,
}

impl AcknowledgedBitrateEstimatorInterface for AcknowledgedBitrateEstimator {
    fn incoming_packet_feedback(&mut self, packet_feedback: &[PacketResult]) {
        //assert!(packet_feedback.is_sorted_by_key(|x| x.receive_time));

        for packet in packet_feedback.iter() {
            if let Some(alr_ended_time) = self.alr_ended_time {
                if packet.sent_packet.send_time > alr_ended_time {
                    self.bitrate_estimator.expect_fast_rate_change();
                    self.alr_ended_time = None;
                }
            }
            let mut acknowledged_estimate = packet.sent_packet.size;
            acknowledged_estimate += packet.sent_packet.prior_unacked_data;
            self.bitrate_estimator
                .update(packet.receive_time, acknowledged_estimate, self.in_alr);
        }
    }

    fn bitrate(&self) -> Option<DataRate> {
        self.bitrate_estimator.bitrate()
    }

    fn peek_rate(&self) -> Option<DataRate> {
        self.bitrate_estimator.peek_rate()
    }

    fn set_alr_ended_time(&mut self, alr_ended_time: Timestamp) {
        self.alr_ended_time.replace(alr_ended_time);
    }

    fn set_alr(&mut self, in_alr: bool) {
        self.in_alr = in_alr;
    }
}

impl AcknowledgedBitrateEstimator {
    pub fn new(bitrate_estimator: BitrateEstimator) -> Self {
        Self {
            alr_ended_time: None,
            in_alr: false,
            bitrate_estimator,
        }
    }

    pub fn create(field_trials: &FieldTrials) -> Box<dyn AcknowledgedBitrateEstimatorInterface> {
        let simplified_estimator_settings = &field_trials.robust_throughput_estimator_settings;
        if simplified_estimator_settings.enabled {
            Box::new(RobustThroughputEstimator::new(
                simplified_estimator_settings.clone(),
            ))
        } else {
            Box::new(AcknowledgedBitrateEstimator::new(BitrateEstimator::new(
                field_trials.bwe_throughput_window_config.clone(),
            )))
        }
    }
}
