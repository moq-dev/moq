/*
 *  Copyright (c) 2019 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use std::collections::VecDeque;

use crate::{
    api::{
        transport::PacketResult,
        units::{DataRate, DataSize, TimeDelta, Timestamp},
    },
    AcknowledgedBitrateEstimatorInterface,
};

// WebRTC-Bwe-RobustThroughputEstimatorSettings
#[derive(Debug, Clone)]
pub struct RobustThroughputEstimatorSettings {
    // Set `enabled` to true to use the RobustThroughputEstimator, false to use
    // the AcknowledgedBitrateEstimator.
    pub enabled: bool,

    // The estimator keeps the smallest window containing at least
    // `window_packets` and at least the packets received during the last
    // `min_window_duration` milliseconds.
    // (This means that it may store more than `window_packets` at high bitrates,
    // and a longer duration than `min_window_duration` at low bitrates.)
    // However, if will never store more than MaxPackets (for performance
    // reasons), and never longer than max_window_duration (to avoid very old
    // packets influencing the estimate for example when sending is paused).
    pub window_packets: usize,
    pub max_window_packets: usize,
    pub window_duration: TimeDelta,
    pub max_window_duration: TimeDelta,

    // The estimator window requires at least `required_packets` packets
    // to produce an estimate.
    pub required_packets: usize,

    // If audio packets aren't included in allocation (i.e. the
    // estimated available bandwidth is divided only among the video
    // streams), then `unacked_weight` should be set to 0.
    // If audio packets are included in allocation, but not in bandwidth
    // estimation (i.e. they don't have transport-wide sequence numbers,
    // but we nevertheless divide the estimated available bandwidth among
    // both audio and video streams), then `unacked_weight` should be set to 1.
    // If all packets have transport-wide sequence numbers, then the value
    // of `unacked_weight` doesn't matter.
    pub unacked_weight: f64,
}

impl Default for RobustThroughputEstimatorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            window_packets: 20,
            max_window_packets: 500,
            window_duration: TimeDelta::from_seconds(1),
            max_window_duration: TimeDelta::from_seconds(5),
            required_packets: 10,
            unacked_weight: 1.0,
        }
    }
}

impl RobustThroughputEstimatorSettings {
    pub fn validate(&mut self) {
        assert!(self.enabled);

        if self.window_packets < 10 || 1000 < self.window_packets {
            tracing::warn!("Window size must be between 10 and 1000 packets");
            self.window_packets = 20;
        }
        if self.max_window_packets < 10 || 1000 < self.max_window_packets {
            tracing::warn!("Max window size must be between 10 and 1000 packets");
            self.max_window_packets = 500;
        }
        self.max_window_packets = self.max_window_packets.max(self.window_packets);

        if self.required_packets < 10 || 1000 < self.required_packets {
            tracing::warn!(
                "Required number of initial packets must be between 10 and 1000 packets"
            );
            self.required_packets = 10;
        }
        self.required_packets = self.required_packets.min(self.window_packets);

        if self.window_duration < TimeDelta::from_millis(100)
            || TimeDelta::from_millis(3000) < self.window_duration
        {
            tracing::warn!("Window duration must be between 100 and 3000 ms");
            self.window_duration = TimeDelta::from_millis(750);
        }
        if self.max_window_duration < TimeDelta::from_seconds(1)
            || TimeDelta::from_seconds(15) < self.max_window_duration
        {
            tracing::warn!("Max window duration must be between 1 and 15 s");
            self.max_window_duration = TimeDelta::from_seconds(5);
        }
        self.window_duration = self.window_duration.min(self.max_window_duration);

        if self.unacked_weight < 0.0 || 1.0 < self.unacked_weight {
            tracing::warn!("Weight for prior unacked size must be between 0 and 1.");
            self.unacked_weight = 1.0;
        }
    }
}

pub struct RobustThroughputEstimator {
    settings: RobustThroughputEstimatorSettings,
    window: VecDeque<PacketResult>,
    latest_discarded_send_time: Timestamp,
}

impl Default for RobustThroughputEstimator {
    fn default() -> Self {
        Self::new(RobustThroughputEstimatorSettings::default())
    }
}

impl RobustThroughputEstimator {
    pub fn new(mut settings: RobustThroughputEstimatorSettings) -> Self {
        settings.validate();

        Self {
            settings,
            window: VecDeque::new(),
            latest_discarded_send_time: Timestamp::minus_infinity(),
        }
    }

    fn first_packet_outside_window(&self) -> bool {
        if self.window.is_empty() {
            return false;
        }
        if self.window.len() > self.settings.max_window_packets {
            return true;
        }
        let current_window_duration: TimeDelta =
            self.window.back().unwrap().receive_time - self.window.front().unwrap().receive_time;
        if current_window_duration > self.settings.max_window_duration {
            return true;
        }
        if self.window.len() > self.settings.window_packets
            && current_window_duration > self.settings.window_duration
        {
            return true;
        }
        false
    }
}

impl AcknowledgedBitrateEstimatorInterface for RobustThroughputEstimator {
    fn incoming_packet_feedback(&mut self, packet_feedback: &[PacketResult]) {
        //assert!(packet_feedback.is_sorted_by_key(|x| x.receive_time));
        for packet in packet_feedback {
            // Ignore packets without valid send or receive times.
            // (This should not happen in production since lost packets are filtered
            // out before passing the feedback vector to the throughput estimator.
            // However, explicitly handling this case makes the estimator more robust
            // and avoids a hard-to-detect bad state.)
            if packet.receive_time.is_infinite() || packet.sent_packet.send_time.is_infinite() {
                continue;
            }

            // Insert the new packet.
            self.window.push_back(*packet);
            self.window
                .back_mut()
                .unwrap()
                .sent_packet
                .prior_unacked_data *= self.settings.unacked_weight;
            // In most cases, receive timestamps should already be in order, but in the
            // rare case where feedback packets have been reordered, we do some swaps to
            // ensure that the window is sorted.
            for i in (1..self.window.len()).rev() {
                if self.window[i].receive_time < self.window[i - 1].receive_time {
                    self.window.swap(i, i - 1);
                }
            }

            const MAX_REORDERING_TIME: TimeDelta = TimeDelta::from_seconds(1);
            let receive_delta: TimeDelta =
                self.window.back().unwrap().receive_time - packet.receive_time;
            if receive_delta > MAX_REORDERING_TIME {
                tracing::warn!(
                    "Severe packet re-ordering or timestamps offset changed: {:?}",
                    receive_delta
                );
                self.window.clear();
                self.latest_discarded_send_time = Timestamp::minus_infinity();
            }
        }

        // Remove old packets.
        while self.first_packet_outside_window() {
            self.latest_discarded_send_time = std::cmp::max(
                self.latest_discarded_send_time,
                self.window.front().unwrap().sent_packet.send_time,
            );
            self.window.pop_front();
        }
    }

    fn bitrate(&self) -> Option<DataRate> {
        if self.window.is_empty() || self.window.len() < self.settings.required_packets {
            return None;
        }

        let mut largest_recv_gap = TimeDelta::zero();
        let mut second_largest_recv_gap = TimeDelta::zero();
        for i in 1..self.window.len() {
            // Find receive time gaps.
            let gap: TimeDelta = self.window[i].receive_time - self.window[i - 1].receive_time;
            if gap > largest_recv_gap {
                second_largest_recv_gap = largest_recv_gap;
                largest_recv_gap = gap;
            } else if gap > second_largest_recv_gap {
                second_largest_recv_gap = gap;
            }
        }

        let mut first_send_time: Timestamp = Timestamp::plus_infinity();
        let mut last_send_time: Timestamp = Timestamp::minus_infinity();
        let mut first_recv_time: Timestamp = Timestamp::plus_infinity();
        let mut last_recv_time: Timestamp = Timestamp::minus_infinity();
        let mut recv_size: DataSize = DataSize::from_bytes(0);
        let mut send_size: DataSize = DataSize::from_bytes(0);
        let mut first_recv_size: DataSize = DataSize::from_bytes(0);
        let mut last_send_size: DataSize = DataSize::from_bytes(0);
        let mut num_sent_packets_in_window: usize = 0;
        for packet in &self.window {
            if packet.receive_time < first_recv_time {
                first_recv_time = packet.receive_time;
                first_recv_size = packet.sent_packet.size + packet.sent_packet.prior_unacked_data;
            }
            last_recv_time = std::cmp::max(last_recv_time, packet.receive_time);
            recv_size += packet.sent_packet.size;
            recv_size += packet.sent_packet.prior_unacked_data;

            if packet.sent_packet.send_time < self.latest_discarded_send_time {
                // If we have dropped packets from the window that were sent after
                // this packet, then this packet was reordered. Ignore it from
                // the send rate computation (since the send time may be very far
                // in the past, leading to underestimation of the send rate.)
                // However, ignoring packets creates a risk that we end up without
                // any packets left to compute a send rate.
                continue;
            }
            if packet.sent_packet.send_time > last_send_time {
                last_send_time = packet.sent_packet.send_time;
                last_send_size = packet.sent_packet.size + packet.sent_packet.prior_unacked_data;
            }
            first_send_time = std::cmp::min(first_send_time, packet.sent_packet.send_time);

            send_size += packet.sent_packet.size;
            send_size += packet.sent_packet.prior_unacked_data;
            num_sent_packets_in_window += 1;
        }

        // Suppose a packet of size S is sent every T milliseconds.
        // A window of N packets would contain N*S bytes, but the time difference
        // between the first and the last packet would only be (N-1)*T. Thus, we
        // need to remove the size of one packet to get the correct rate of S/T.
        // Which packet to remove (if the packets have varying sizes),
        // depends on the network model.
        // Suppose that 2 packets with sizes s1 and s2, are received at times t1
        // and t2, respectively. If the packets were transmitted back to back over
        // a bottleneck with rate capacity r, then we'd expect t2 = t1 + r * s2.
        // Thus, r = (t2-t1) / s2, so the size of the first packet doesn't affect
        // the difference between t1 and t2.
        // Analoguously, if the first packet is sent at time t1 and the sender
        // paces the packets at rate r, then the second packet can be sent at time
        // t2 = t1 + r * s1. Thus, the send rate estimate r = (t2-t1) / s1 doesn't
        // depend on the size of the last packet.
        recv_size -= first_recv_size;
        send_size -= last_send_size;

        // Remove the largest gap by replacing it by the second largest gap.
        // This is to ensure that spurious "delay spikes" (i.e. when the
        // network stops transmitting packets for a short period, followed
        // by a burst of delayed packets), don't cause the estimate to drop.
        // This could cause an overestimation, which we guard against by
        // never returning an estimate above the send rate.
        assert!(first_recv_time.is_finite());
        assert!(last_recv_time.is_finite());
        let mut recv_duration: TimeDelta =
            (last_recv_time - first_recv_time) - largest_recv_gap + second_largest_recv_gap;
        recv_duration = std::cmp::max(recv_duration, TimeDelta::from_millis(1));

        if num_sent_packets_in_window < self.settings.required_packets {
            // Too few send times to calculate a reliable send rate.
            return Some(recv_size / recv_duration);
        }

        assert!(first_send_time.is_finite());
        assert!(last_send_time.is_finite());
        let mut send_duration: TimeDelta = last_send_time - first_send_time;
        send_duration = std::cmp::max(send_duration, TimeDelta::from_millis(1));

        Some(std::cmp::min(
            send_size / send_duration,
            recv_size / recv_duration,
        ))
    }

    fn peek_rate(&self) -> Option<DataRate> {
        self.bitrate()
    }

    fn set_alr(&mut self, _in_alr: bool) {
        // noop
    }

    fn set_alr_ended_time(&mut self, _alr_ended_time: crate::api::units::Timestamp) {
        // noop
    }
}

#[cfg(test)]
mod test {
    use crate::api::transport::SentPacket;
    use approx::assert_relative_eq;

    use super::*;

    struct FeedbackGenerator {
        send_clock: Timestamp,
        recv_clock: Timestamp,
        sequence_number: i64,
    }

    impl Default for FeedbackGenerator {
        fn default() -> Self {
            Self {
                send_clock: Timestamp::from_millis(100000),
                recv_clock: Timestamp::from_millis(10000),
                sequence_number: 100,
            }
        }
    }

    impl FeedbackGenerator {
        fn create_feedback(
            &mut self,
            number_of_packets: usize,
            packet_size: DataSize,
            send_rate: DataRate,
            recv_rate: DataRate,
        ) -> Vec<PacketResult> {
            let mut packet_feedback = Vec::with_capacity(number_of_packets);

            for _ in 0..number_of_packets {
                self.recv_clock += packet_size / recv_rate;
                packet_feedback.push(PacketResult {
                    sent_packet: SentPacket {
                        send_time: self.send_clock,
                        sequence_number: self.sequence_number,
                        size: packet_size,
                        ..Default::default()
                    },
                    receive_time: self.recv_clock,
                    ..Default::default()
                });
                self.send_clock += packet_size / send_rate;
                self.sequence_number += 1;
            }

            packet_feedback
        }

        fn current_receive_clock(&self) -> Timestamp {
            self.recv_clock
        }

        fn advance_receive_clock(&mut self, delta: TimeDelta) {
            self.recv_clock += delta;
        }

        fn advance_send_clock(&mut self, delta: TimeDelta) {
            self.send_clock += delta;
        }
    }

    #[test]
    fn initial_estimate() {
        let mut feedback_generator = FeedbackGenerator::default();

        let mut throughput_estimator = RobustThroughputEstimator::default();
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        // No estimate until the estimator has enough data.
        let packet_feedback: Vec<PacketResult> =
            feedback_generator.create_feedback(9, DataSize::from_bytes(1000), send_rate, recv_rate);
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        assert!(throughput_estimator.bitrate().is_none());

        // Estimate once `required_packets` packets have been received.
        let packet_feedback =
            feedback_generator.create_feedback(1, DataSize::from_bytes(1000), send_rate, recv_rate);
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert_eq!(throughput.unwrap(), send_rate);

        // Estimate remains stable when send and receive rates are stable.
        let packet_feedback = feedback_generator.create_feedback(
            15,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert_eq!(throughput.unwrap(), send_rate);
    }

    #[test]
    fn estimate_adapts() {
        let mut feedback_generator = FeedbackGenerator::default();
        let mut throughput_estimator = RobustThroughputEstimator::default();

        // 1 second, 800kbps, estimate is stable.
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        for _ in 0..10 {
            let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
                10,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert_eq!(throughput.unwrap(), send_rate);
        }

        // 1 second, 1600kbps, estimate increases
        let send_rate = DataRate::from_bytes_per_sec(200000);
        let recv_rate = DataRate::from_bytes_per_sec(200000);
        for _ in 0..20 {
            let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
                10,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert!(throughput.is_some());
            assert!(throughput.unwrap() >= DataRate::from_bytes_per_sec(100000));
            assert!(throughput.unwrap() <= send_rate);
        }

        // 1 second, 1600kbps, estimate is stable
        for _ in 0..20 {
            let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
                10,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert_eq!(throughput.unwrap(), send_rate);
        }

        // 1 second, 400kbps, estimate decreases
        let send_rate = DataRate::from_bytes_per_sec(50000);
        let recv_rate = DataRate::from_bytes_per_sec(50000);
        for _ in 0..5 {
            let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
                10,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert!(throughput.is_some());
            assert!(throughput.unwrap() <= DataRate::from_bytes_per_sec(200000));
            assert!(throughput.unwrap() >= send_rate);
        }

        // 1 second, 400kbps, estimate is stable
        let send_rate = DataRate::from_bytes_per_sec(50000);
        let recv_rate = DataRate::from_bytes_per_sec(50000);
        for _ in 0..5 {
            let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
                10,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert_eq!(throughput.unwrap(), send_rate);
        }
    }

    #[test]
    fn capped_by_receive_rate() {
        let mut feedback_generator = FeedbackGenerator::default();
        let mut throughput_estimator = RobustThroughputEstimator::default();
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(25000);

        let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert!(throughput.is_some());
        assert_relative_eq!(
            throughput.unwrap().bytes_per_sec_float(),
            recv_rate.bytes_per_sec_float(),
            epsilon = 0.05 * recv_rate.bytes_per_sec_float()
        ); // Allow 5% error
    }

    #[test]
    fn capped_by_send_rate() {
        let mut feedback_generator = FeedbackGenerator::default();
        let mut throughput_estimator = RobustThroughputEstimator::default();
        let send_rate: DataRate = DataRate::from_bytes_per_sec(50000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert!(throughput.is_some());
        assert_relative_eq!(
            throughput.unwrap().bytes_per_sec_float(),
            send_rate.bytes_per_sec_float(),
            epsilon = 0.05 * send_rate.bytes_per_sec_float()
        ); // Allow 5% error
    }

    #[test]
    fn delay_spike() {
        let mut feedback_generator = FeedbackGenerator::default();
        // This test uses a 500ms window to amplify the effect
        // of a delay spike.
        let settings = RobustThroughputEstimatorSettings {
            enabled: true,
            window_duration: TimeDelta::from_millis(500),
            ..Default::default()
        };
        let mut throughput_estimator = RobustThroughputEstimator::new(settings);
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert_eq!(throughput.unwrap(), send_rate);

        // Delay spike. 25 packets sent, but none received.
        feedback_generator.advance_receive_clock(TimeDelta::from_millis(250));

        // Deliver all of the packets during the next 50 ms. (During this time,
        // we'll have sent an additional 5 packets, so we need to receive 30
        // packets at 1000 bytes each in 50 ms, i.e. 600000 bytes per second).
        let recv_rate = DataRate::from_bytes_per_sec(600000);
        // Estimate should not drop.
        for _ in 0..30 {
            let packet_feedback = feedback_generator.create_feedback(
                1,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert!(throughput.is_some());
            assert_relative_eq!(
                throughput.unwrap().bytes_per_sec_float(),
                send_rate.bytes_per_sec_float(),
                epsilon = 0.05 * send_rate.bytes_per_sec_float()
            ); // Allow 5% error
        }

        // Delivery at normal rate. When the packets received before the gap
        // has left the estimator's window, the receive rate will be high, but the
        // estimate should be capped by the send rate.
        let recv_rate = DataRate::from_bytes_per_sec(100000);
        for _ in 0..20 {
            let packet_feedback = feedback_generator.create_feedback(
                5,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert!(throughput.is_some());
            assert_relative_eq!(
                throughput.unwrap().bytes_per_sec_float(),
                send_rate.bytes_per_sec_float(),
                epsilon = 0.05 * send_rate.bytes_per_sec_float()
            ); // Allow 5% error
        }
    }

    #[test]
    fn high_loss() {
        let mut feedback_generator = FeedbackGenerator::default();
        let mut throughput_estimator = RobustThroughputEstimator::default();
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        let mut packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );

        // 50% loss
        for packet in packet_feedback.iter_mut().skip(1).step_by(2) {
            packet.receive_time = Timestamp::plus_infinity();
        }

        packet_feedback.sort_by_key(|x| x.receive_time);
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert!(throughput.is_some());
        assert_relative_eq!(
            throughput.unwrap().bytes_per_sec_float(),
            send_rate.bytes_per_sec_float() / 2.0,
            epsilon = 0.05 * send_rate.bytes_per_sec_float() / 2.0
        ); // Allow 5% error
    }

    #[test]
    fn reordered_feedback() {
        let mut feedback_generator = FeedbackGenerator::default();
        let mut throughput_estimator = RobustThroughputEstimator::default();
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert_eq!(throughput.unwrap(), send_rate);

        let delayed_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            10,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        let packet_feedback = feedback_generator.create_feedback(
            10,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );

        // Since we're missing some feedback, it's expected that the
        // estimate will drop.
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert!(throughput.is_some());
        assert!(throughput.unwrap() < send_rate);

        // But it should completely recover as soon as we get the feedback.
        throughput_estimator.incoming_packet_feedback(&delayed_feedback);
        let throughput = throughput_estimator.bitrate();
        assert_eq!(throughput.unwrap(), send_rate);

        // It should then remain stable (as if the feedbacks weren't reordered.)
        for _ in 0..10 {
            let packet_feedback = feedback_generator.create_feedback(
                15,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert_eq!(throughput.unwrap(), send_rate);
        }
    }

    #[test]
    fn deep_reordering() {
        let mut feedback_generator = FeedbackGenerator::default();
        // This test uses a 500ms window to amplify the
        // effect of reordering.
        let settings = RobustThroughputEstimatorSettings {
            enabled: true,
            window_duration: TimeDelta::from_millis(500),
            ..Default::default()
        };
        let mut throughput_estimator = RobustThroughputEstimator::new(settings);
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        let mut delayed_packets: Vec<PacketResult> =
            feedback_generator.create_feedback(1, DataSize::from_bytes(1000), send_rate, recv_rate);

        for _ in 0..10 {
            let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
                10,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert_eq!(throughput.unwrap(), send_rate);
        }

        // Delayed packet arrives ~1 second after it should have.
        // Since the window is 500 ms, the delayed packet was sent ~500
        // ms before the second oldest packet. However, the send rate
        // should not drop.
        delayed_packets.first_mut().unwrap().receive_time =
            feedback_generator.current_receive_clock();
        throughput_estimator.incoming_packet_feedback(&delayed_packets);
        let throughput = throughput_estimator.bitrate();
        assert!(throughput.is_some());
        assert_relative_eq!(
            throughput.unwrap().bytes_per_sec_float(),
            send_rate.bytes_per_sec_float(),
            epsilon = 0.05 * send_rate.bytes_per_sec_float()
        ); // Allow 5% error

        // Thoughput should stay stable.
        for _ in 0..10 {
            let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
                10,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert!(throughput.is_some());
            assert_relative_eq!(
                throughput.unwrap().bytes_per_sec_float(),
                send_rate.bytes_per_sec_float(),
                epsilon = 0.05 * send_rate.bytes_per_sec_float()
            ); // Allow 5% error
        }
    }

    #[test]
    fn resets_if_receive_clock_change_backwards() {
        let mut feedback_generator = FeedbackGenerator::default();
        let mut throughput_estimator = RobustThroughputEstimator::default();
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        assert_eq!(throughput_estimator.bitrate().unwrap(), send_rate);

        feedback_generator.advance_receive_clock(TimeDelta::from_seconds(-2));
        let send_rate = DataRate::from_bytes_per_sec(200000);
        let recv_rate = DataRate::from_bytes_per_sec(200000);
        let packet_feedback = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        assert_eq!(throughput_estimator.bitrate().unwrap(), send_rate);
    }

    #[test]
    fn stream_paused_and_resumed() {
        let mut feedback_generator = FeedbackGenerator::default();
        let mut throughput_estimator = RobustThroughputEstimator::default();
        let send_rate: DataRate = DataRate::from_bytes_per_sec(100000);
        let recv_rate: DataRate = DataRate::from_bytes_per_sec(100000);

        let packet_feedback: Vec<PacketResult> = feedback_generator.create_feedback(
            20,
            DataSize::from_bytes(1000),
            send_rate,
            recv_rate,
        );
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert!(throughput.is_some());
        let expected_bytes_per_sec: f64 = 100.0 * 1000.0;
        assert_relative_eq!(
            throughput.unwrap().bytes_per_sec_float(),
            expected_bytes_per_sec,
            epsilon = 0.05 * expected_bytes_per_sec
        ); // Allow 5% error

        // No packets sent or feedback received for 60s.
        feedback_generator.advance_send_clock(TimeDelta::from_seconds(60));
        feedback_generator.advance_receive_clock(TimeDelta::from_seconds(60));

        // Resume sending packets at the same rate as before. The estimate
        // will initially be invalid, due to lack of recent data.
        let packet_feedback =
            feedback_generator.create_feedback(5, DataSize::from_bytes(1000), send_rate, recv_rate);
        throughput_estimator.incoming_packet_feedback(&packet_feedback);
        let throughput = throughput_estimator.bitrate();
        assert!(throughput.is_none());

        // But be back to the normal level once we have enough data.
        for _ in 0..4 {
            let packet_feedback = feedback_generator.create_feedback(
                5,
                DataSize::from_bytes(1000),
                send_rate,
                recv_rate,
            );
            throughput_estimator.incoming_packet_feedback(&packet_feedback);
            let throughput = throughput_estimator.bitrate();
            assert_eq!(throughput.unwrap(), send_rate);
        }
    }
} // namespace webrtc
