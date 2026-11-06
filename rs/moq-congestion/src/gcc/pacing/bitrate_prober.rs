/*
 *  Copyright (c) 2014 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

//! WebRTC's pacer sends each probe cluster GoogCC asks for, padding with old media when it has no
//! new. Here the cluster paces the controller instead: its packets leave at the cluster's rate,
//! and while it runs the transport may fill it with duplicates of data in flight (QUIC's native
//! probing). The `WebRTC-Bwe-ProbingBehavior` settings keep their defaults.

use std::collections::VecDeque;

use crate::gcc::api::{
    transport::{PacedPacketInfo, ProbeClusterConfig},
    units::{DataRate, DataSize, TimeDelta, Timestamp},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbingState {
    // Probing will not be triggered in this state at all times.
    Disabled,
    // Probing is enabled and ready to trigger on the first packet arrival if
    // there is a probe cluster.
    Inactive,
    // Probe cluster is filled with the set of data rates to be probed and
    // probes are being sent.
    Active,
}

#[derive(Debug, Clone)]
struct ProbeCluster {
    pace_info: PacedPacketInfo,
    send_bitrate: DataRate,

    sent_probes: i64,
    sent_bytes: i64,
    min_probe_delta: TimeDelta,
    requested_at: Timestamp,
    started_at: Timestamp,
}

/// Paces the probe clusters GoogCC requests, as WebRTC's `BitrateProber`.
#[derive(Debug, Clone)]
pub struct BitrateProber {
    probing_state: ProbingState,

    // Probe bitrate per packet. These are used to compute the delta relative to
    // the previous probe packet based on the size and time when that packet was
    // sent.
    clusters: VecDeque<ProbeCluster>,

    // Time the next probe should be sent when in kActive state.
    next_probe_time: Timestamp,

    max_probe_delay: TimeDelta,
    min_packet_size: DataSize,

    /// Clusters restarted so far, numbering their new ids
    restarts: i32,
}

impl Default for BitrateProber {
    fn default() -> Self {
        Self {
            probing_state: ProbingState::Inactive,
            clusters: VecDeque::new(),
            next_probe_time: Timestamp::plus_infinity(),
            max_probe_delay: TimeDelta::from_millis(10),
            min_packet_size: DataSize::from_bytes(200),
            restarts: 0,
        }
    }
}

impl BitrateProber {
    const PROBE_CLUSTER_TIMEOUT: TimeDelta = TimeDelta::from_seconds(5);
    const MAX_PENDING_PROBE_CLUSTERS: usize = 5;
    /// Restarted clusters take ids from here up, above any GoogCC's ProbeController counts to
    pub const RESTART_ID_BASE: i32 = 1 << 30;

    // Returns true if the prober is in a probing session, i.e., it currently
    // wants packets to be sent out according to the time returned by
    // TimeUntilNextProbe().
    #[cfg(test)]
    pub fn is_probing(&self) -> bool {
        self.probing_state == ProbingState::Active
    }

    // Initializes a new probing session if the prober is allowed to probe. Does
    // not initialize the prober unless the packet size is large enough to probe
    // with.
    pub fn on_incoming_packet(&mut self, packet_size: DataSize) {
        self.maybe_set_active_state(packet_size);
    }

    pub fn create_probe_cluster(&mut self, cluster_config: &ProbeClusterConfig) {
        debug_assert!(self.probing_state != ProbingState::Disabled);
        debug_assert!(cluster_config.min_probe_delta > TimeDelta::zero());

        while let Some(front) = self.clusters.front() {
            if cluster_config.at_time - front.requested_at > Self::PROBE_CLUSTER_TIMEOUT
                || self.clusters.len() > Self::MAX_PENDING_PROBE_CLUSTERS
            {
                self.clusters.pop_front();
            } else {
                break;
            }
        }

        let cluster = ProbeCluster {
            pace_info: PacedPacketInfo::new(
                cluster_config.id,
                cluster_config.target_probe_count as i64,
                (cluster_config.target_data_rate * cluster_config.target_duration).bytes(),
            ),
            send_bitrate: cluster_config.target_data_rate,
            sent_probes: 0,
            sent_bytes: 0,
            min_probe_delta: cluster_config.min_probe_delta,
            requested_at: cluster_config.at_time,
            started_at: Timestamp::plus_infinity(),
        };
        tracing::debug!(
            bitrate = ?cluster.send_bitrate,
            min_bytes = cluster.pace_info.probe_cluster_min_bytes,
            min_probes = cluster.pace_info.probe_cluster_min_probes,
            "probe cluster"
        );
        self.clusters.push_back(cluster);

        self.maybe_set_active_state(DataSize::zero());
    }

    // Returns the time at which the next probe should be sent to get accurate
    // probing. If probing is not desired at this time, Timestamp::PlusInfinity()
    // will be returned.
    pub fn next_probe_time(&self) -> Timestamp {
        // Probing is not active or probing is already complete.
        if self.probing_state != ProbingState::Active || self.clusters.is_empty() {
            return Timestamp::plus_infinity();
        }
        self.next_probe_time
    }

    // Information about the current probing cluster.
    pub fn current_cluster(&mut self, now: Timestamp) -> Option<PacedPacketInfo> {
        if self.clusters.is_empty() || self.probing_state != ProbingState::Active {
            return None;
        }

        if self.next_probe_time.is_finite() && now - self.next_probe_time > self.max_probe_delay {
            tracing::debug!(
                next = ?self.next_probe_time,
                ?now,
                "probe delay too high, discarding probe cluster"
            );
            self.clusters.pop_front();
            if self.clusters.is_empty() {
                self.probing_state = ProbingState::Inactive;
                return None;
            }
        }

        self.clusters.front().map(|cluster| cluster.pace_info)
    }

    /// The bytes and packets the next cluster still needs, at its rate, whether or not a packet
    /// has started it.
    pub fn remaining(&self) -> Option<(DataSize, i64, DataRate)> {
        if self.probing_state == ProbingState::Disabled {
            return None;
        }
        let cluster = self.clusters.front()?;
        Some((
            DataSize::from_bytes(
                (cluster.pace_info.probe_cluster_min_bytes - cluster.sent_bytes).max(0),
            ),
            (cluster.pace_info.probe_cluster_min_probes - cluster.sent_probes).max(0),
            cluster.send_bitrate,
        ))
    }

    // Returns the minimum number of bytes that the prober recommends for
    // the next probe, or zero if not probing. A probe can consist of multiple
    // packets that are sent back to back.
    pub fn recommended_min_probe_size(&self) -> DataSize {
        match self.clusters.front() {
            Some(cluster) => cluster.send_bitrate * cluster.min_probe_delta,
            None => DataSize::zero(),
        }
    }

    // Called to report to the prober that a probe has been sent. In case of
    // multiple packets per probe, this call would be made at the end of sending
    // the last packet in probe. `size` is the total size of all packets in probe.
    pub fn probe_sent(&mut self, now: Timestamp, size: DataSize) {
        debug_assert!(self.probing_state == ProbingState::Active);
        debug_assert!(!size.is_zero());

        let Some(cluster) = self.clusters.front_mut() else {
            return;
        };
        if cluster.sent_probes == 0 {
            debug_assert!(cluster.started_at.is_infinite());
            cluster.started_at = now;
        }
        cluster.sent_bytes += size.bytes();
        cluster.sent_probes += 1;
        // Compute the time delta from the cluster start to ensure probe bitrate stays
        // close to the target bitrate.
        self.next_probe_time =
            cluster.started_at + DataSize::from_bytes(cluster.sent_bytes) / cluster.send_bitrate;
        if cluster.sent_bytes >= cluster.pace_info.probe_cluster_min_bytes
            && cluster.sent_probes >= cluster.pace_info.probe_cluster_min_probes
        {
            self.clusters.pop_front();
        }
        if self.clusters.is_empty() {
            self.probing_state = ProbingState::Inactive;
        }
    }

    /// The sender ran out of data to send.
    ///
    /// WebRTC's pacer pads a probe cluster when media runs out, so a gap means the pacer fell
    /// behind and the cluster is dropped. Here only the transport's duplicates can pad it, so a
    /// sender with nothing to send restarts the cluster with its next packet instead, under a new
    /// id: the packets it already sent span the gap, and estimating from them would read the gap
    /// as the path's rate.
    pub fn on_app_limited(&mut self) {
        if self.probing_state != ProbingState::Active {
            return;
        }
        let Some(cluster) = self.clusters.front_mut() else {
            return;
        };
        if cluster.sent_probes == 0 {
            return;
        }
        cluster.sent_probes = 0;
        cluster.sent_bytes = 0;
        cluster.started_at = Timestamp::plus_infinity();
        self.restarts = (self.restarts + 1) % Self::RESTART_ID_BASE;
        cluster.pace_info.probe_cluster_id = Self::RESTART_ID_BASE + self.restarts;
        self.probing_state = ProbingState::Inactive;
    }

    fn maybe_set_active_state(&mut self, packet_size: DataSize) {
        if self.ready_to_set_active_state(packet_size) {
            self.next_probe_time = Timestamp::minus_infinity();
            self.probing_state = ProbingState::Active;
        }
    }

    fn ready_to_set_active_state(&self, packet_size: DataSize) -> bool {
        if self.clusters.is_empty() {
            return false;
        }
        match self.probing_state {
            ProbingState::Disabled | ProbingState::Active => false,
            // A "large enough" packet must be sent first, before a probe can be
            // generated and sent.
            ProbingState::Inactive => {
                packet_size
                    >= std::cmp::min(self.recommended_min_probe_size(), self.min_packet_size)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn cluster(at_time: Timestamp, rate: DataRate, id: i32) -> ProbeClusterConfig {
        ProbeClusterConfig {
            at_time,
            target_data_rate: rate,
            target_duration: TimeDelta::from_millis(15),
            min_probe_delta: TimeDelta::from_millis(2),
            target_probe_count: 5,
            id,
        }
    }

    // A cluster starts on a large enough packet, paces at its rate, and ends once it sent both
    // its packets and its bytes.
    #[test]
    fn verify_states_and_time_between_probes() {
        let mut prober = BitrateProber::default();
        assert!(!prober.is_probing());

        let mut now = Timestamp::zero();
        let start_time = now;
        assert_eq!(prober.next_probe_time(), Timestamp::plus_infinity());

        const TEST_BITRATE_1: DataRate = DataRate::from_kilobits_per_sec(900);
        const TEST_BITRATE_2: DataRate = DataRate::from_kilobits_per_sec(1800);
        const CLUSTER_SIZE: i64 = 5;
        const PROBE_SIZE: DataSize = DataSize::from_bytes(1000);
        const MIN_PROBE_DURATION: TimeDelta = TimeDelta::from_millis(15);

        prober.create_probe_cluster(&cluster(now, TEST_BITRATE_1, 0));
        prober.create_probe_cluster(&cluster(now, TEST_BITRATE_2, 1));
        assert!(!prober.is_probing());

        prober.on_incoming_packet(PROBE_SIZE);
        assert!(prober.is_probing());
        assert_eq!(0, prober.current_cluster(now).unwrap().probe_cluster_id);

        // First packet should probe as soon as possible.
        assert_eq!(Timestamp::minus_infinity(), prober.next_probe_time());

        for _ in 0..CLUSTER_SIZE {
            now = std::cmp::max(now, prober.next_probe_time());
            assert_eq!(now, std::cmp::max(now, prober.next_probe_time()));
            assert_eq!(0, prober.current_cluster(now).unwrap().probe_cluster_id);
            prober.probe_sent(now, PROBE_SIZE);
        }

        assert!(now - start_time >= MIN_PROBE_DURATION);
        // Verify that the actual bitrate is withing 10% of the target.
        let bitrate = PROBE_SIZE * (CLUSTER_SIZE - 1) / (now - start_time);
        assert!(bitrate > TEST_BITRATE_1 * 0.9);
        assert!(bitrate < TEST_BITRATE_1 * 1.1);

        now = std::cmp::max(now, prober.next_probe_time());
        let probe2_started = now;

        for _ in 0..CLUSTER_SIZE {
            now = std::cmp::max(now, prober.next_probe_time());
            assert_eq!(1, prober.current_cluster(now).unwrap().probe_cluster_id);
            prober.probe_sent(now, PROBE_SIZE);
        }

        // Verify that the actual bitrate is withing 10% of the target.
        let duration = now - probe2_started;
        assert!(duration >= MIN_PROBE_DURATION);
        let bitrate = (PROBE_SIZE * (CLUSTER_SIZE - 1)) / duration;
        assert!(bitrate > TEST_BITRATE_2 * 0.9);
        assert!(bitrate < TEST_BITRATE_2 * 1.1);

        assert_eq!(prober.next_probe_time(), Timestamp::plus_infinity());
        assert!(!prober.is_probing());
    }

    // A sender that falls more than the maximum probe delay behind abandons the cluster rather
    // than pacing at its rate forever.
    #[test]
    fn discards_delayed_probes() {
        let mut prober = BitrateProber::default();
        let mut now = Timestamp::zero();
        prober.create_probe_cluster(&cluster(now, DataRate::from_kilobits_per_sec(900), 0));
        prober.on_incoming_packet(DataSize::from_bytes(1000));
        assert!(prober.is_probing());
        prober.probe_sent(now, DataSize::from_bytes(1000));

        now = prober.next_probe_time() + TimeDelta::from_millis(11);
        assert!(prober.current_cluster(now).is_none());
        assert!(!prober.is_probing());
    }

    // Clusters requested more than five seconds before a new one are dropped unsent.
    #[test]
    fn discards_stale_clusters() {
        let mut prober = BitrateProber::default();
        let rate = DataRate::from_kilobits_per_sec(900);
        prober.create_probe_cluster(&cluster(Timestamp::zero(), rate, 0));
        prober.create_probe_cluster(&cluster(Timestamp::from_seconds(6), rate, 1));
        prober.on_incoming_packet(DataSize::from_bytes(1000));
        assert_eq!(
            prober
                .current_cluster(Timestamp::from_seconds(6))
                .unwrap()
                .probe_cluster_id,
            1
        );
    }

    // Probing waits for a packet large enough to probe with.
    #[test]
    fn does_not_initialize_probing_for_small_packets() {
        let mut prober = BitrateProber::default();
        prober.create_probe_cluster(&cluster(
            Timestamp::zero(),
            DataRate::from_kilobits_per_sec(900),
            0,
        ));
        prober.on_incoming_packet(DataSize::from_bytes(100));
        assert!(!prober.is_probing());
        prober.on_incoming_packet(DataSize::from_bytes(1000));
        assert!(prober.is_probing());
    }
}
