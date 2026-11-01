/*
 *  Copyright (c) 2016 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use std::collections::HashMap;

use crate::api::{
    transport::{PacedPacketInfo, PacketResult},
    units::{DataRate, DataSize, TimeDelta, Timestamp},
};

struct AggregatedCluster {
    pub num_probes: i64,
    pub first_send: Timestamp,
    pub last_send: Timestamp,
    pub first_receive: Timestamp,
    pub last_receive: Timestamp,
    pub size_last_send: DataSize,
    pub size_first_receive: DataSize,
    pub size_total: DataSize,
}

impl Default for AggregatedCluster {
    fn default() -> Self {
        Self {
            num_probes: 0,
            first_send: Timestamp::plus_infinity(),
            last_send: Timestamp::minus_infinity(),
            first_receive: Timestamp::plus_infinity(),
            last_receive: Timestamp::minus_infinity(),
            size_last_send: DataSize::zero(),
            size_first_receive: DataSize::zero(),
            size_total: DataSize::zero(),
        }
    }
}

#[derive(Default)]
pub struct ProbeBitrateEstimator {
    clusters: HashMap<i32, AggregatedCluster>,
    estimated_data_rate: Option<DataRate>,
}

impl ProbeBitrateEstimator {
    // The minumum number of probes we need to receive feedback about in percent
    // in order to have a valid estimate.
    const MIN_RECEIVED_PROBES_RATIO: f64 = 0.80;

    // The minumum number of bytes we need to receive feedback about in percent
    // in order to have a valid estimate.
    const MIN_RECEIVED_BYTES_RATIO: f64 = 0.80;

    // The maximum |receive rate| / |send rate| ratio for a valid estimate.
    const MAX_VALID_RATIO: f64 = 2.0;

    // The minimum |receive rate| / |send rate| ratio assuming that the link is
    // not saturated, i.e. we assume that we will receive at least
    // MinRatioForUnsaturatedLink * |send rate| if |send rate| is less than the
    // link capacity.
    const MIN_RATIO_FOR_UNSATURATED_LINK: f32 = 0.9;

    // The target utilization of the link. If we know true link capacity
    // we'd like to send at 95% of that rate.
    const TARGET_UTILIZATION_FRACTION: f32 = 0.95;

    // The maximum time period over which the cluster history is retained.
    // This is also the maximum time period beyond which a probing burst is not
    // expected to last.
    const MAX_CLUSTER_HISTORY: TimeDelta = TimeDelta::from_seconds(1);

    // The maximum time interval between first and the last probe on a cluster
    // on the sender side as well as the receive side.
    const MAX_PROBE_INTERVAL: TimeDelta = TimeDelta::from_seconds(1);

    // Should be called for every probe packet we receive feedback about.
    // Returns the estimated bitrate if the probe completes a valid cluster.
    pub fn handle_probe_and_estimate_bitrate(
        &mut self,
        packet_feedback: &PacketResult,
    ) -> Option<DataRate> {
        let cluster_id: i32 = packet_feedback.sent_packet.pacing_info.probe_cluster_id;
        assert_ne!(cluster_id, PacedPacketInfo::NOT_APROBE);

        self.erase_old_clusters(packet_feedback.receive_time);

        let cluster: &mut AggregatedCluster = self.clusters.entry(cluster_id).or_default();

        if packet_feedback.sent_packet.send_time < cluster.first_send {
            cluster.first_send = packet_feedback.sent_packet.send_time;
        }
        if packet_feedback.sent_packet.send_time > cluster.last_send {
            cluster.last_send = packet_feedback.sent_packet.send_time;
            cluster.size_last_send = packet_feedback.sent_packet.size;
        }
        if packet_feedback.receive_time < cluster.first_receive {
            cluster.first_receive = packet_feedback.receive_time;
            cluster.size_first_receive = packet_feedback.sent_packet.size;
        }
        if packet_feedback.receive_time > cluster.last_receive {
            cluster.last_receive = packet_feedback.receive_time;
        }
        cluster.size_total += packet_feedback.sent_packet.size;
        cluster.num_probes += 1;

        assert!(
            packet_feedback
                .sent_packet
                .pacing_info
                .probe_cluster_min_probes
                > 0
        );
        assert!(
            packet_feedback
                .sent_packet
                .pacing_info
                .probe_cluster_min_bytes
                > 0
        );

        let min_probes: i64 = (packet_feedback
            .sent_packet
            .pacing_info
            .probe_cluster_min_probes as f64
            * Self::MIN_RECEIVED_PROBES_RATIO) as i64;
        let min_size: DataSize = DataSize::from_bytes(
            packet_feedback
                .sent_packet
                .pacing_info
                .probe_cluster_min_bytes as _,
        ) * Self::MIN_RECEIVED_BYTES_RATIO;
        if cluster.num_probes < min_probes || cluster.size_total < min_size {
            return None;
        }

        let send_interval: TimeDelta = cluster.last_send - cluster.first_send;
        let receive_interval: TimeDelta = cluster.last_receive - cluster.first_receive;

        if send_interval <= TimeDelta::zero()
            || send_interval > Self::MAX_PROBE_INTERVAL
            || receive_interval <= TimeDelta::zero()
            || receive_interval > Self::MAX_PROBE_INTERVAL
        {
            tracing::debug!("Probing unsuccessful, invalid send/receive interval [cluster id: {}] [send interval: {:?}] [receive interval: {:?}]",
                      cluster_id, send_interval, receive_interval);
            return None;
        }
        // Since the `send_interval` does not include the time it takes to actually
        // send the last packet the size of the last sent packet should not be
        // included when calculating the send bitrate.
        assert!(cluster.size_total > cluster.size_last_send);
        let send_size: DataSize = cluster.size_total - cluster.size_last_send;
        let send_rate: DataRate = send_size / send_interval;

        // Since the `receive_interval` does not include the time it takes to
        // actually receive the first packet the size of the first received packet
        // should not be included when calculating the receive bitrate.
        assert!(cluster.size_total > cluster.size_first_receive);
        let receive_size: DataSize = cluster.size_total - cluster.size_first_receive;
        let receive_rate: DataRate = receive_size / receive_interval;

        let ratio: f64 = receive_rate / send_rate;
        if ratio > Self::MAX_VALID_RATIO {
            tracing::debug!("Probing unsuccessful, receive/send ratio too high [cluster id: {}] [send: {:?}/{:?} = {:?}] [receive: {:?}/{:?} = {:?}] [ratio: {:?} > {:?}]",
                      cluster_id, send_size, send_interval, send_rate, receive_size, receive_interval, receive_rate, ratio, Self::MAX_VALID_RATIO);
            return None;
        }

        tracing::debug!("Probing successful [cluster id: {}] [send: {:?}/{:?} = {:?}] [receive: {:?}/{:?} = {:?}]",
                    cluster_id, send_size, send_interval, send_rate, receive_size, receive_interval, receive_rate);

        let mut res: DataRate = std::cmp::min(send_rate, receive_rate);
        // If we're receiving at significantly lower bitrate than we were sending at,
        // it suggests that we've found the true capacity of the link. In this case,
        // set the target bitrate slightly lower to not immediately overuse.
        if receive_rate < Self::MIN_RATIO_FOR_UNSATURATED_LINK * send_rate {
            assert!(send_rate > receive_rate);
            res = Self::TARGET_UTILIZATION_FRACTION * receive_rate;
        }
        self.estimated_data_rate = Some(res);
        self.estimated_data_rate
    }

    pub fn fetch_and_reset_last_estimated_bitrate(&mut self) -> Option<DataRate> {
        let estimated_data_rate: Option<DataRate> = self.estimated_data_rate;
        self.estimated_data_rate.take();
        estimated_data_rate
    }

    // Erases old cluster data that was seen before `timestamp`.
    fn erase_old_clusters(&mut self, timestamp: Timestamp) {
        self.clusters
            .retain(|_, cluster| cluster.last_receive + Self::MAX_CLUSTER_HISTORY >= timestamp);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use approx::assert_relative_eq;

    const DEFAULT_MIN_PROBES: i64 = 5;
    const DEFAULT_MIN_BYTES: i64 = 5000;
    const TARGET_UTILIZATION_FRACTION: f64 = 0.95;

    #[derive(Default)]
    struct TestProbeBitrateEstimator {
        measured_data_rate: Option<DataRate>,
        probe_bitrate_estimator: ProbeBitrateEstimator,
    }

    impl TestProbeBitrateEstimator {
        // TODO(philipel): Use PacedPacketInfo when ProbeBitrateEstimator is rewritten
        //                 to use that information.
        fn add_packet_feedback(
            &mut self,
            probe_cluster_id: i32,
            size_bytes: usize,
            send_time_ms: i64,
            arrival_time_ms: i64,
            min_probes: i64,
            min_bytes: i64,
        ) {
            const REFERENCE_TIME: Timestamp = Timestamp::from_seconds(1000);
            let mut feedback: PacketResult = PacketResult::default();
            feedback.sent_packet.send_time = REFERENCE_TIME + TimeDelta::from_millis(send_time_ms);
            feedback.sent_packet.size = DataSize::from_bytes(size_bytes as _);
            feedback.sent_packet.pacing_info =
                PacedPacketInfo::new(probe_cluster_id, min_probes, min_bytes);
            feedback.receive_time = REFERENCE_TIME + TimeDelta::from_millis(arrival_time_ms);
            self.measured_data_rate = self
                .probe_bitrate_estimator
                .handle_probe_and_estimate_bitrate(&feedback);
        }
    }

    #[test]
    fn one_cluster() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 20, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 30, 40, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);

        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            800000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn one_cluster_too_few_probes() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 2000, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 2000, 10, 20, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 2000, 20, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);

        assert!(test.measured_data_rate.is_none());
    }

    #[test]
    fn one_cluster_too_few_bytes() {
        let mut test = TestProbeBitrateEstimator::default();
        const MIN_BYTES: i64 = 6000;
        test.add_packet_feedback(0, 800, 0, 10, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 800, 10, 20, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 800, 20, 30, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 800, 30, 40, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 800, 40, 50, DEFAULT_MIN_PROBES, MIN_BYTES);

        assert!(test.measured_data_rate.is_none());
    }

    #[test]
    fn small_cluster() {
        let mut test = TestProbeBitrateEstimator::default();
        const MIN_BYTES: i64 = 1000;
        test.add_packet_feedback(0, 150, 0, 10, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 150, 10, 20, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 150, 20, 30, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 150, 30, 40, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 150, 40, 50, DEFAULT_MIN_PROBES, MIN_BYTES);
        test.add_packet_feedback(0, 150, 50, 60, DEFAULT_MIN_PROBES, MIN_BYTES);
        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            120000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn large_cluster() {
        let mut test = TestProbeBitrateEstimator::default();
        const MIN_PROBES: i64 = 30;
        const MIN_BYTES: i64 = 312500;

        let mut receive_time: i64 = 5;
        for send_time in 0..25 {
            test.add_packet_feedback(0, 12500, send_time, receive_time, MIN_PROBES, MIN_BYTES);
            receive_time += 1;
        }
        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            100000000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn fast_receive() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 15, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 35, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 30, 40, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);

        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            800000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn too_fast_receive() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 19, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 22, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 25, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 40, 27, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);

        assert!(test.measured_data_rate.is_none());
    }

    #[test]
    fn slow_receive() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 40, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 70, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 30, 85, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        // Expected send rate = 800 kbps, expected receive rate = 320 kbps.

        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            TARGET_UTILIZATION_FRACTION * 320000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn burst_receive() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 50, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 50, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 50, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 40, 50, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);

        assert!(test.measured_data_rate.is_none());
    }

    #[test]
    fn multiple_clusters() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 20, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 40, 60, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        // Expected send rate = 600 kbps, expected receive rate = 480 kbps.
        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            TARGET_UTILIZATION_FRACTION * 480000.0,
            epsilon = 10.0
        );

        test.add_packet_feedback(0, 1000, 50, 60, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        // Expected send rate = 640 kbps, expected receive rate = 640 kbps.
        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            640000.0,
            epsilon = 10.0
        );

        test.add_packet_feedback(1, 1000, 60, 70, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(1, 1000, 65, 77, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(1, 1000, 70, 84, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(1, 1000, 75, 90, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        // Expected send rate = 1600 kbps, expected receive rate = 1200 kbps.

        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            TARGET_UTILIZATION_FRACTION * 1200000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn ignore_old_clusters() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 20, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);

        test.add_packet_feedback(1, 1000, 60, 70, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(1, 1000, 65, 77, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(1, 1000, 70, 84, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(1, 1000, 75, 90, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        // Expected send rate = 1600 kbps, expected receive rate = 1200 kbps.

        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            TARGET_UTILIZATION_FRACTION * 1200000.0,
            epsilon = 10.0
        );

        // Coming in 6s later
        test.add_packet_feedback(
            0,
            1000,
            40 + 6000,
            60 + 6000,
            DEFAULT_MIN_PROBES,
            DEFAULT_MIN_BYTES,
        );

        assert!(test.measured_data_rate.is_none());
    }

    #[test]
    fn ignore_size_last_send_packet() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 20, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 30, 40, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1500, 40, 50, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        // Expected send rate = 800 kbps, expected receive rate = 900 kbps.

        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            800000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn ignore_size_first_receive_packet() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1500, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 20, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 30, 40, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        // Expected send rate = 933 kbps, expected receive rate = 800 kbps.

        assert_relative_eq!(
            test.measured_data_rate.unwrap().bps_float(),
            TARGET_UTILIZATION_FRACTION * 800000.0,
            epsilon = 10.0
        );
    }

    #[test]
    fn no_last_estimated_bitrate_bps() {
        let mut test = TestProbeBitrateEstimator::default();
        assert!(test
            .probe_bitrate_estimator
            .fetch_and_reset_last_estimated_bitrate()
            .is_none());
    }

    #[test]
    fn fetch_last_estimated_bitrate_bps() {
        let mut test = TestProbeBitrateEstimator::default();
        test.add_packet_feedback(0, 1000, 0, 10, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 10, 20, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 20, 30, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);
        test.add_packet_feedback(0, 1000, 30, 40, DEFAULT_MIN_PROBES, DEFAULT_MIN_BYTES);

        let estimated_bitrate = test
            .probe_bitrate_estimator
            .fetch_and_reset_last_estimated_bitrate();
        assert!(estimated_bitrate.is_some());
        assert_relative_eq!(
            estimated_bitrate.unwrap().bps_float(),
            800000.0,
            epsilon = 10.0
        );
        assert!(test
            .probe_bitrate_estimator
            .fetch_and_reset_last_estimated_bitrate()
            .is_none());
    }
} // namespace webrtc
