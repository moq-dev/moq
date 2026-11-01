/*
 *  Copyright (c) 2013 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::units::latest_timestamp;

#[derive(Clone, Copy, Debug)]
struct TimestampGroup {
    pub size: usize,
    pub first_timestamp: u32,
    pub timestamp: u32,
    pub first_arrival_ms: i64,
    pub complete_time_ms: i64,
    pub last_system_time_ms: i64,
}
impl TimestampGroup {
    pub fn is_first_packet(&self) -> bool {
        self.complete_time_ms == -1
    }
}

impl Default for TimestampGroup {
    fn default() -> Self {
        Self {
            size: 0,
            first_timestamp: 0,
            timestamp: 0,
            first_arrival_ms: -1,
            complete_time_ms: -1,
            last_system_time_ms: -1,
        }
    }
}

// Helper class to compute the inter-arrival time delta and the size delta
// between two timestamp groups. A timestamp is a 32 bit unsigned number with
// a client defined rate.
pub struct InterArrival {
    timestamp_group_length_ticks: u32,
    current_timestamp_group: TimestampGroup,
    prev_timestamp_group: TimestampGroup,
    timestamp_to_ms_coeff: f64,
    num_consecutive_reordered_packets: usize,
}

impl InterArrival {
    // After this many packet groups received out of order InterArrival will
    // reset, assuming that clocks have made a jump.
    pub const REORDERED_RESET_THRESHOLD: usize = 3;
    pub const ARRIVAL_TIME_OFFSET_THRESHOLD_MS: i64 = 3000;

    const BURST_DELTA_THRESHOLD_MS: i64 = 5;
    const MAX_BURST_DURATION_MS: i64 = 100;

    // A timestamp group is defined as all packets with a timestamp which are at
    // most timestamp_group_length_ticks older than the first timestamp in that
    // group.
    pub fn new(timestamp_group_length_ticks: u32, timestamp_to_ms_coeff: f64) -> Self {
        Self {
            timestamp_group_length_ticks,
            current_timestamp_group: TimestampGroup::default(),
            prev_timestamp_group: TimestampGroup::default(),
            timestamp_to_ms_coeff,
            num_consecutive_reordered_packets: 0,
        }
    }

    // This function returns true if a delta was computed, or false if the current
    // group is still incomplete or if only one group has been completed.
    // `timestamp` is the timestamp.
    // `arrival_time_ms` is the local time at which the packet arrived.
    // `packet_size` is the size of the packet.
    // `timestamp_delta` (output) is the computed timestamp delta.
    // `arrival_time_delta_ms` (output) is the computed arrival-time delta.
    // `packet_size_delta` (output) is the computed size delta.
    #[allow(clippy::too_many_arguments)]
    pub fn compute_deltas(
        &mut self,
        timestamp: u32,
        arrival_time_ms: i64,
        system_time_ms: i64,
        packet_size: usize,
        timestamp_delta: &mut u32,
        arrival_time_delta_ms: &mut i64,
        packet_size_delta: &mut i64,
    ) -> bool {
        let mut calculated_deltas = false;
        if self.current_timestamp_group.is_first_packet() {
            // We don't have enough data to update the filter, so we store it until we
            // have two frames of data to process.
            self.current_timestamp_group.timestamp = timestamp;
            self.current_timestamp_group.first_timestamp = timestamp;
            self.current_timestamp_group.first_arrival_ms = arrival_time_ms;
        } else if !self.packet_in_order(timestamp) {
            return false;
        } else if self.new_timestamp_group(arrival_time_ms, timestamp) {
            // First packet of a later frame, the previous frame sample is ready.
            if self.prev_timestamp_group.complete_time_ms >= 0 {
                *timestamp_delta = self
                    .current_timestamp_group
                    .timestamp
                    .wrapping_sub(self.prev_timestamp_group.timestamp);
                *arrival_time_delta_ms = self.current_timestamp_group.complete_time_ms
                    - self.prev_timestamp_group.complete_time_ms;
                // Check system time differences to see if we have an unproportional jump
                // in arrival time. In that case reset the inter-arrival computations.
                let system_time_delta_ms: i64 = self.current_timestamp_group.last_system_time_ms
                    - self.prev_timestamp_group.last_system_time_ms;
                if *arrival_time_delta_ms - system_time_delta_ms
                    >= Self::ARRIVAL_TIME_OFFSET_THRESHOLD_MS
                {
                    tracing::warn!(
                        "The arrival time clock offset has changed (diff = {} ms), resetting.",
                        *arrival_time_delta_ms - system_time_delta_ms
                    );
                    self.reset();
                    return false;
                }
                if *arrival_time_delta_ms < 0 {
                    // The group of packets has been reordered since receiving its local
                    // arrival timestamp.
                    self.num_consecutive_reordered_packets += 1;
                    if self.num_consecutive_reordered_packets >= Self::REORDERED_RESET_THRESHOLD {
                        tracing::warn!(
                            "Packets are being reordered on the path from the
                 socket to the bandwidth estimator. Ignoring this
                 packet for bandwidth estimation, resetting."
                        );
                        self.reset();
                    }
                    return false;
                } else {
                    self.num_consecutive_reordered_packets = 0;
                }
                assert!(*arrival_time_delta_ms >= 0);
                *packet_size_delta = self.current_timestamp_group.size as i64
                    - self.prev_timestamp_group.size as i64;
                calculated_deltas = true;
            }
            self.prev_timestamp_group = self.current_timestamp_group;
            // The new timestamp is now the current frame.
            self.current_timestamp_group.first_timestamp = timestamp;
            self.current_timestamp_group.timestamp = timestamp;
            self.current_timestamp_group.first_arrival_ms = arrival_time_ms;
            self.current_timestamp_group.size = 0;
        } else {
            self.current_timestamp_group.timestamp =
                latest_timestamp(self.current_timestamp_group.timestamp, timestamp);
        }
        // Accumulate the frame size.
        self.current_timestamp_group.size += packet_size;
        self.current_timestamp_group.complete_time_ms = arrival_time_ms;
        self.current_timestamp_group.last_system_time_ms = system_time_ms;

        calculated_deltas
    }

    // Returns true if the packet with timestamp `timestamp` arrived in order.
    fn packet_in_order(&self, timestamp: u32) -> bool {
        if self.current_timestamp_group.is_first_packet() {
            true
        } else {
            // Assume that a diff which is bigger than half the timestamp interval
            // (32 bits) must be due to reordering. This code is almost identical to
            // that in IsNewerTimestamp() in module_common_types.h.
            let timestamp_diff: u32 =
                timestamp.wrapping_sub(self.current_timestamp_group.first_timestamp);
            timestamp_diff < 0x80000000
        }
    }

    // Returns true if the last packet was the end of the current batch and the
    // packet with `timestamp` is the first of a new batch.
    fn new_timestamp_group(&self, arrival_time_ms: i64, timestamp: u32) -> bool {
        if self.current_timestamp_group.is_first_packet() {
            false
        } else if self.belongs_to_burst(arrival_time_ms, timestamp) {
            return false;
        } else {
            let timestamp_diff: u32 =
                timestamp.wrapping_sub(self.current_timestamp_group.first_timestamp);
            return timestamp_diff > self.timestamp_group_length_ticks;
        }
    }

    fn belongs_to_burst(&self, arrival_time_ms: i64, timestamp: u32) -> bool {
        assert!(self.current_timestamp_group.complete_time_ms >= 0);
        let arrival_time_delta_ms: i64 =
            arrival_time_ms - self.current_timestamp_group.complete_time_ms;
        let timestamp_diff: u32 = timestamp.wrapping_sub(self.current_timestamp_group.timestamp);
        let ts_delta_ms: i64 = (self.timestamp_to_ms_coeff * (timestamp_diff as f64) + 0.5) as i64;
        if ts_delta_ms == 0 {
            return true;
        }
        let propagation_delta_ms: i64 = arrival_time_delta_ms - ts_delta_ms;
        if propagation_delta_ms < 0
            && arrival_time_delta_ms <= Self::BURST_DELTA_THRESHOLD_MS
            && arrival_time_ms - self.current_timestamp_group.first_arrival_ms
                < Self::MAX_BURST_DURATION_MS
        {
            return true;
        }
        false
    }

    fn reset(&mut self) {
        self.num_consecutive_reordered_packets = 0;
        self.current_timestamp_group = TimestampGroup::default();
        self.prev_timestamp_group = TimestampGroup::default();
    }
}

#[cfg(test)]
mod test {
    use approx::assert_relative_eq;

    use super::*;

    const TIMESTAMP_GROUP_LENGTH_US: i64 = 5000;
    const MIN_STEP: i64 = 20;
    const TRIGGER_NEW_GROUP_US: i64 = TIMESTAMP_GROUP_LENGTH_US + MIN_STEP;
    const BURST_THRESHOLD_MS: i64 = 5;
    const ABS_SEND_TIME_FRACTION: i64 = 18;
    const ABS_SEND_TIME_INTER_ARRIVAL_UPSHIFT: i64 = 8;
    const INTER_ARRIVAL_SHIFT: i64 = ABS_SEND_TIME_FRACTION + ABS_SEND_TIME_INTER_ARRIVAL_UPSHIFT;

    const RTP_TIMESTAMP_TO_MS: f64 = 1.0 / 90.0;
    const AST_TO_MS: f64 = 1000.0 / (1 << INTER_ARRIVAL_SHIFT) as f64;

    struct InterArrivalTest {
        pub inter_arrival: InterArrival,
        inter_arrival_rtp: InterArrival,
        inter_arrival_ast: InterArrival,
    }

    impl InterArrivalTest {
        pub fn new() -> Self {
            Self {
                inter_arrival: InterArrival::new((TIMESTAMP_GROUP_LENGTH_US / 1000) as u32, 1.0),
                inter_arrival_rtp: InterArrival::new(
                    Self::make_rtp_timestamp(TIMESTAMP_GROUP_LENGTH_US),
                    RTP_TIMESTAMP_TO_MS,
                ),
                inter_arrival_ast: InterArrival::new(
                    Self::make_abs_send_time(TIMESTAMP_GROUP_LENGTH_US),
                    AST_TO_MS,
                ),
            }
        }
        // Test that neither inter_arrival instance complete the timestamp group from
        // the given data.
        pub fn expect_false(
            &mut self,
            timestamp_us: i64,
            arrival_time_ms: i64,
            packet_size: usize,
        ) {
            Self::internal_expect_false(
                &mut self.inter_arrival_rtp,
                Self::make_rtp_timestamp(timestamp_us),
                arrival_time_ms,
                packet_size,
            );
            Self::internal_expect_false(
                &mut self.inter_arrival_ast,
                Self::make_abs_send_time(timestamp_us),
                arrival_time_ms,
                packet_size,
            );
        }

        // Test that both inter_arrival instances complete the timestamp group from
        // the given data and that all returned deltas are as expected (except
        // delta: Timestamp, which is rounded from us to different ranges and must
        // match within an interval, given in |timestamp_near].
        #[allow(clippy::too_many_arguments)]
        pub fn expect_true(
            &mut self,
            timestamp_us: i64,
            arrival_time_ms: i64,
            packet_size: usize,
            expected_timestamp_delta_us: i64,
            expected_arrival_time_delta_ms: i64,
            expected_packet_size_delta: i64,
            timestamp_near: u32,
        ) {
            Self::internal_expect_true(
                &mut self.inter_arrival_rtp,
                Self::make_rtp_timestamp(timestamp_us),
                arrival_time_ms,
                packet_size,
                Self::make_rtp_timestamp(expected_timestamp_delta_us),
                expected_arrival_time_delta_ms,
                expected_packet_size_delta,
                timestamp_near,
            );
            Self::internal_expect_true(
                &mut self.inter_arrival_ast,
                Self::make_abs_send_time(timestamp_us),
                arrival_time_ms,
                packet_size,
                Self::make_abs_send_time(expected_timestamp_delta_us),
                expected_arrival_time_delta_ms,
                expected_packet_size_delta,
                timestamp_near << 8,
            );
        }

        pub fn wrap_test_helper(
            &mut self,
            wrap_start_us: i64,
            timestamp_near: u32,
            unorderly_within_group: bool,
        ) {
            // Step through the range of a 32 bit int, 1/4 at a time to not cause
            // packets close to wraparound to be judged as out of order.

            // G1
            let mut arrival_time: i64 = 17;
            self.expect_false(0, arrival_time, 1);

            // G2
            arrival_time += BURST_THRESHOLD_MS + 1;
            self.expect_false(wrap_start_us / 4, arrival_time, 1);

            // G3
            arrival_time += BURST_THRESHOLD_MS + 1;
            self.expect_true(
                wrap_start_us / 2,
                arrival_time,
                1,
                wrap_start_us / 4,
                6,
                0, // Delta G2-G1
                0,
            );

            // G4
            arrival_time += BURST_THRESHOLD_MS + 1;
            let g4_arrival_time: i64 = arrival_time;
            self.expect_true(
                wrap_start_us / 2 + wrap_start_us / 4,
                arrival_time,
                1,
                wrap_start_us / 4,
                6,
                0, // Delta G3-G2
                timestamp_near,
            );

            // G5
            arrival_time += BURST_THRESHOLD_MS + 1;
            self.expect_true(
                wrap_start_us,
                arrival_time,
                2,
                wrap_start_us / 4,
                6,
                0, // Delta G4-G3
                timestamp_near,
            );
            for i in 0..10 {
                // Slowly step across the wrap point.
                arrival_time += BURST_THRESHOLD_MS + 1;
                if unorderly_within_group {
                    // These packets arrive with timestamps in decreasing order but are
                    // nevertheless accumulated to group because their timestamps are higher
                    // than the initial timestamp of the group.
                    self.expect_false(wrap_start_us + MIN_STEP * (9 - i), arrival_time, 1);
                } else {
                    self.expect_false(wrap_start_us + MIN_STEP * i, arrival_time, 1);
                }
            }
            let g5_arrival_time: i64 = arrival_time;

            // This packet is out of order and should be dropped.
            arrival_time += BURST_THRESHOLD_MS + 1;
            self.expect_false(wrap_start_us - 100, arrival_time, 100);

            // G6
            arrival_time += BURST_THRESHOLD_MS + 1;
            let g6_arrival_time: i64 = arrival_time;

            self.expect_true(
                wrap_start_us + TRIGGER_NEW_GROUP_US,
                arrival_time,
                10,
                wrap_start_us / 4 + 9 * MIN_STEP,
                g5_arrival_time - g4_arrival_time,
                (2 + 10) - 1, // Delta G5-G4
                timestamp_near,
            );

            // This packet is out of order and should be dropped.
            arrival_time += BURST_THRESHOLD_MS + 1;
            self.expect_false(wrap_start_us + TIMESTAMP_GROUP_LENGTH_US, arrival_time, 100);

            // G7
            arrival_time += BURST_THRESHOLD_MS + 1;
            self.expect_true(
                wrap_start_us + 2 * TRIGGER_NEW_GROUP_US,
                arrival_time,
                100,
                // Delta G6-G5
                TRIGGER_NEW_GROUP_US - 9 * MIN_STEP,
                g6_arrival_time - g5_arrival_time,
                10 - (2 + 10),
                timestamp_near,
            );
        }

        fn make_rtp_timestamp(us: i64) -> u32 {
            ((us * 90 + 500) as u64 / 1000) as u32
        }

        fn make_abs_send_time(us: i64) -> u32 {
            let absolute_send_time: u32 =
                ((((us as u64) << 18) + 500000) / 1000000) as u32 & 0x00FFFFFF;
            absolute_send_time << 8
        }

        fn internal_expect_false(
            inter_arrival: &mut InterArrival,
            timestamp: u32,
            arrival_time_ms: i64,
            packet_size: usize,
        ) {
            let mut dummy_timestamp: u32 = 101;
            let mut dummy_arrival_time_ms: i64 = 303;
            let mut dummy_packet_size: i64 = 909;
            let computed: bool = inter_arrival.compute_deltas(
                timestamp,
                arrival_time_ms,
                arrival_time_ms,
                packet_size,
                &mut dummy_timestamp,
                &mut dummy_arrival_time_ms,
                &mut dummy_packet_size,
            );
            assert!(!computed);
            assert_eq!(101, dummy_timestamp);
            assert_eq!(303, dummy_arrival_time_ms);
            assert_eq!(909, dummy_packet_size);
        }

        #[allow(clippy::too_many_arguments)]
        fn internal_expect_true(
            inter_arrival: &mut InterArrival,
            timestamp: u32,
            arrival_time_ms: i64,
            packet_size: usize,
            expected_timestamp_delta: u32,
            expected_arrival_time_delta_ms: i64,
            expected_packet_size_delta: i64,
            timestamp_near: u32,
        ) {
            let mut delta_timestamp: u32 = 101;
            let mut delta_arrival_time_ms: i64 = 303;
            let mut delta_packet_size: i64 = 909;
            let computed: bool = inter_arrival.compute_deltas(
                timestamp,
                arrival_time_ms,
                arrival_time_ms,
                packet_size,
                &mut delta_timestamp,
                &mut delta_arrival_time_ms,
                &mut delta_packet_size,
            );
            assert!(computed);
            assert_relative_eq!(
                expected_timestamp_delta as f64,
                delta_timestamp as f64,
                epsilon = timestamp_near as f64
            );
            assert_eq!(expected_arrival_time_delta_ms, delta_arrival_time_ms);
            assert_eq!(expected_packet_size_delta, delta_packet_size);
        }
    }

    #[test]
    fn first_packet() {
        let mut test = InterArrivalTest::new();
        test.expect_false(0, 17, 1);
    }

    #[test]
    fn first_group() {
        let mut test = InterArrivalTest::new();
        // G1
        let mut arrival_time: i64 = 17;
        let g1_arrival_time: i64 = arrival_time;
        test.expect_false(0, arrival_time, 1);

        // G2
        arrival_time += BURST_THRESHOLD_MS + 1;
        let g2_arrival_time: i64 = arrival_time;
        test.expect_false(TRIGGER_NEW_GROUP_US, arrival_time, 2);

        // G3
        // Only once the first packet of the third group arrives, do we see the deltas
        // between the first two.
        arrival_time += BURST_THRESHOLD_MS + 1;
        test.expect_true(
            2 * TRIGGER_NEW_GROUP_US,
            arrival_time,
            1,
            // Delta G2-G1
            TRIGGER_NEW_GROUP_US,
            g2_arrival_time - g1_arrival_time,
            1,
            0,
        );
    }

    #[test]
    fn second_group() {
        let mut test = InterArrivalTest::new();
        // G1
        let mut arrival_time: i64 = 17;
        let g1_arrival_time: i64 = arrival_time;
        test.expect_false(0, arrival_time, 1);

        // G2
        arrival_time += BURST_THRESHOLD_MS + 1;
        let g2_arrival_time: i64 = arrival_time;
        test.expect_false(TRIGGER_NEW_GROUP_US, arrival_time, 2);

        // G3
        arrival_time += BURST_THRESHOLD_MS + 1;
        let g3_arrival_time: i64 = arrival_time;
        test.expect_true(
            2 * TRIGGER_NEW_GROUP_US,
            arrival_time,
            1,
            // Delta G2-G1
            TRIGGER_NEW_GROUP_US,
            g2_arrival_time - g1_arrival_time,
            1,
            0,
        );

        // G4
        // First packet of 4th group yields deltas between group 2 and 3.
        arrival_time += BURST_THRESHOLD_MS + 1;
        test.expect_true(
            3 * TRIGGER_NEW_GROUP_US,
            arrival_time,
            2,
            // Delta G3-G2
            TRIGGER_NEW_GROUP_US,
            g3_arrival_time - g2_arrival_time,
            -1,
            0,
        );
    }

    #[test]
    fn accumulated_group() {
        let mut test = InterArrivalTest::new();
        // G1
        let mut arrival_time: i64 = 17;
        let g1_arrival_time: i64 = arrival_time;
        test.expect_false(0, arrival_time, 1);

        // G2
        arrival_time += BURST_THRESHOLD_MS + 1;
        test.expect_false(TRIGGER_NEW_GROUP_US, 28, 2);
        let mut timestamp: i64 = TRIGGER_NEW_GROUP_US;
        for _ in 0..10 {
            // A bunch of packets arriving within the same group.
            arrival_time += BURST_THRESHOLD_MS + 1;
            timestamp += MIN_STEP;
            test.expect_false(timestamp, arrival_time, 1);
        }
        let g2_arrival_time: i64 = arrival_time;
        let g2_timestamp: i64 = timestamp;

        // G3
        arrival_time = 500;
        test.expect_true(
            2 * TRIGGER_NEW_GROUP_US,
            arrival_time,
            100,
            g2_timestamp,
            g2_arrival_time - g1_arrival_time,
            (2 + 10) - 1, // Delta G2-G1
            0,
        );
    }

    #[test]
    fn out_of_order_packet() {
        let mut test = InterArrivalTest::new();
        // G1
        let mut arrival_time: i64 = 17;
        let mut timestamp: i64 = 0;
        test.expect_false(timestamp, arrival_time, 1);
        let g1_timestamp: i64 = timestamp;
        let g1_arrival_time: i64 = arrival_time;

        // G2
        arrival_time += 11;
        timestamp += TRIGGER_NEW_GROUP_US;
        test.expect_false(timestamp, 28, 2);
        for _ in 0..10 {
            arrival_time += BURST_THRESHOLD_MS + 1;
            timestamp += MIN_STEP;
            test.expect_false(timestamp, arrival_time, 1);
        }
        let g2_timestamp: i64 = timestamp;
        let g2_arrival_time: i64 = arrival_time;

        // This packet is out of order and should be dropped.
        arrival_time = 281;
        test.expect_false(g1_timestamp, arrival_time, 100);

        // G3
        arrival_time = 500;
        timestamp = 2 * TRIGGER_NEW_GROUP_US;
        test.expect_true(
            timestamp,
            arrival_time,
            100,
            // Delta G2-G1
            g2_timestamp - g1_timestamp,
            g2_arrival_time - g1_arrival_time,
            (2 + 10) - 1,
            0,
        );
    }

    #[test]
    fn out_of_order_within_group() {
        let mut test = InterArrivalTest::new();
        // G1
        let mut arrival_time: i64 = 17;
        let mut timestamp: i64 = 0;
        test.expect_false(timestamp, arrival_time, 1);
        let g1_timestamp: i64 = timestamp;
        let g1_arrival_time: i64 = arrival_time;

        // G2
        timestamp += TRIGGER_NEW_GROUP_US;
        arrival_time += 11;
        test.expect_false(TRIGGER_NEW_GROUP_US, 28, 2);
        timestamp += 10 * MIN_STEP;
        let g2_timestamp: i64 = timestamp;
        for _ in 0..10 {
            // These packets arrive with timestamps in decreasing order but are
            // nevertheless accumulated to group because their timestamps are higher
            // than the initial timestamp of the group.
            arrival_time += BURST_THRESHOLD_MS + 1;
            test.expect_false(timestamp, arrival_time, 1);
            timestamp -= MIN_STEP;
        }
        let g2_arrival_time: i64 = arrival_time;

        // However, this packet is deemed out of order and should be dropped.
        arrival_time = 281;
        timestamp = g1_timestamp;
        test.expect_false(timestamp, arrival_time, 100);

        // G3
        timestamp = 2 * TRIGGER_NEW_GROUP_US;
        arrival_time = 500;
        test.expect_true(
            timestamp,
            arrival_time,
            100,
            g2_timestamp - g1_timestamp,
            g2_arrival_time - g1_arrival_time,
            (2 + 10) - 1,
            0,
        );
    }

    #[test]
    fn two_bursts() {
        let mut test = InterArrivalTest::new();
        // G1
        let g1_arrival_time: i64 = 17;
        test.expect_false(0, g1_arrival_time, 1);

        // G2
        let mut timestamp: i64 = TRIGGER_NEW_GROUP_US;
        let mut arrival_time: i64 = 100; // Simulate no packets arriving for 100 ms.
        for _ in 0..10 {
            // A bunch of packets arriving in one burst (within 5 ms apart).
            timestamp += 30000;
            arrival_time += BURST_THRESHOLD_MS;
            test.expect_false(timestamp, arrival_time, 1);
        }
        let g2_arrival_time: i64 = arrival_time;
        let g2_timestamp: i64 = timestamp;

        // G3
        timestamp += 30000;
        arrival_time += BURST_THRESHOLD_MS + 1;
        test.expect_true(
            timestamp,
            arrival_time,
            100,
            g2_timestamp,
            g2_arrival_time - g1_arrival_time,
            10 - 1, // Delta G2-G1
            0,
        );
    }

    #[test]
    fn no_bursts() {
        let mut test = InterArrivalTest::new();
        // G1
        test.expect_false(0, 17, 1);

        // G2
        let timestamp: i64 = TRIGGER_NEW_GROUP_US;
        let arrival_time: i64 = 28;
        test.expect_false(timestamp, arrival_time, 2);

        // G3
        test.expect_true(
            TRIGGER_NEW_GROUP_US + 30000,
            arrival_time + BURST_THRESHOLD_MS + 1,
            100,
            timestamp,
            arrival_time - 17,
            2 - 1, // Delta G2-G1
            0,
        );
    }

    // Yields 0xfffffffe when converted to internal representation in
    // self.inter_arrival_rtp and self.inter_arrival_ast respectively.
    const START_RTP_TIMESTAMP_WRAP_US: i64 = 47721858827;
    const START_ABS_SEND_TIME_WRAP_US: i64 = 63999995;

    #[test]
    fn rtp_timestamp_wrap() {
        let mut test = InterArrivalTest::new();
        test.wrap_test_helper(START_RTP_TIMESTAMP_WRAP_US, 1, false);
    }

    #[test]
    fn abs_send_time_wrap() {
        let mut test = InterArrivalTest::new();
        test.wrap_test_helper(START_ABS_SEND_TIME_WRAP_US, 1, false);
    }

    #[test]
    fn rtp_timestamp_wrap_out_of_order_within_group() {
        let mut test = InterArrivalTest::new();
        test.wrap_test_helper(START_RTP_TIMESTAMP_WRAP_US, 1, true);
    }

    #[test]
    fn abs_send_time_wrap_out_of_order_within_group() {
        let mut test = InterArrivalTest::new();
        test.wrap_test_helper(START_ABS_SEND_TIME_WRAP_US, 1, true);
    }

    #[test]
    fn positive_arrival_time_jump() {
        let mut test = InterArrivalTest::new();
        const PACKET_SIZE: usize = 1000;
        let mut send_time_ms: u32 = 10000;
        let mut arrival_time_ms: i64 = 20000;
        let mut system_time_ms: i64 = 30000;

        let mut send_delta: u32 = 0;
        let mut arrival_delta: i64 = 0;
        let mut size_delta: i64 = 0;
        assert!(!test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));

        const TIME_DELTA_MS: i64 = 30;
        send_time_ms += TIME_DELTA_MS as u32;
        arrival_time_ms += TIME_DELTA_MS;
        system_time_ms += TIME_DELTA_MS;
        assert!(!test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));

        send_time_ms += TIME_DELTA_MS as u32;
        arrival_time_ms += TIME_DELTA_MS + InterArrival::ARRIVAL_TIME_OFFSET_THRESHOLD_MS;
        system_time_ms += TIME_DELTA_MS;
        assert!(test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));
        assert_eq!(TIME_DELTA_MS, send_delta as i64);
        assert_eq!(TIME_DELTA_MS, arrival_delta);
        assert_eq!(size_delta, 0);

        send_time_ms += TIME_DELTA_MS as u32;
        arrival_time_ms += TIME_DELTA_MS;
        system_time_ms += TIME_DELTA_MS;
        // The previous arrival time jump should now be detected and cause a reset.
        assert!(!test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));

        // The two next packets will not give a valid delta since we're in the initial
        // state.
        for _ in 0..2 {
            send_time_ms += TIME_DELTA_MS as u32;
            arrival_time_ms += TIME_DELTA_MS;
            system_time_ms += TIME_DELTA_MS;
            assert!(!test.inter_arrival.compute_deltas(
                send_time_ms,
                arrival_time_ms,
                system_time_ms,
                PACKET_SIZE,
                &mut send_delta,
                &mut arrival_delta,
                &mut size_delta
            ));
        }

        send_time_ms += TIME_DELTA_MS as u32;
        arrival_time_ms += TIME_DELTA_MS;
        system_time_ms += TIME_DELTA_MS;
        assert!(test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));
        assert_eq!(TIME_DELTA_MS, send_delta as _);
        assert_eq!(TIME_DELTA_MS, arrival_delta);
        assert_eq!(size_delta, 0);
    }

    #[test]
    fn negative_arrival_time_jump() {
        let mut test = InterArrivalTest::new();
        const PACKET_SIZE: usize = 1000;
        let mut send_time_ms: u32 = 10000;
        let mut arrival_time_ms: i64 = 20000;
        let mut system_time_ms: i64 = 30000;

        let mut send_delta: u32 = 0;
        let mut arrival_delta: i64 = 0;
        let mut size_delta: i64 = 0;
        assert!(!test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));

        const TIME_DELTA_MS: i64 = 30;
        send_time_ms += TIME_DELTA_MS as u32;
        arrival_time_ms += TIME_DELTA_MS;
        system_time_ms += TIME_DELTA_MS;
        assert!(!test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));

        send_time_ms += TIME_DELTA_MS as u32;
        arrival_time_ms += TIME_DELTA_MS;
        system_time_ms += TIME_DELTA_MS;
        assert!(test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));
        assert_eq!(TIME_DELTA_MS, send_delta as _);
        assert_eq!(TIME_DELTA_MS, arrival_delta);
        assert_eq!(size_delta, 0);

        // Three out of order will fail, after that we will be reset and two more will
        // fail before we get our first valid delta after the reset.
        arrival_time_ms -= 1000;
        for _ in 0..InterArrival::REORDERED_RESET_THRESHOLD + 3 {
            send_time_ms += TIME_DELTA_MS as u32;
            arrival_time_ms += TIME_DELTA_MS;
            system_time_ms += TIME_DELTA_MS;
            // The previous arrival time jump should now be detected and cause a reset.
            assert!(!test.inter_arrival.compute_deltas(
                send_time_ms,
                arrival_time_ms,
                system_time_ms,
                PACKET_SIZE,
                &mut send_delta,
                &mut arrival_delta,
                &mut size_delta
            ));
        }

        send_time_ms += TIME_DELTA_MS as u32;
        arrival_time_ms += TIME_DELTA_MS;
        system_time_ms += TIME_DELTA_MS;
        assert!(test.inter_arrival.compute_deltas(
            send_time_ms,
            arrival_time_ms,
            system_time_ms,
            PACKET_SIZE,
            &mut send_delta,
            &mut arrival_delta,
            &mut size_delta
        ));
        assert_eq!(TIME_DELTA_MS, send_delta as _);
        assert_eq!(TIME_DELTA_MS, arrival_delta);
        assert_eq!(size_delta, 0);
    }
}
