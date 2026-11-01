/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

//! The messages GoogCC exchanges with its transport, trimmed to what a QUIC sender has: each
//! packet's send time, size and probe cluster, and its receive time or loss. RTP's sequence
//! numbers, ECN feedback, audio flag and untracked data, REMB, RTCP loss reports, route changes and
//! the network state estimate have no source here, and the pacer has no padding to send.

use crate::gcc::api::units::{DataRate, DataSize, TimeDelta, Timestamp};

/// Use StreamsConfig for information about streams that is required for specific
/// adjustments to the algorithms in network controllers. Especially useful
/// for experiments.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamsConfig {
    pub requests_alr_probing: Option<bool>,
    /// If `enable_repeated_initial_probing` is set to true, Probes are sent
    /// periodically every 1s during the first 5s after the network becomes
    /// available. The probes ignores max_total_allocated_bitrate.
    pub enable_repeated_initial_probing: bool,
    pub pacing_factor: Option<f64>,

    // TODO(srte): Use BitrateAllocationLimits here.
    pub min_total_allocated_bitrate: Option<DataRate>,
    pub max_total_allocated_bitrate: Option<DataRate>,
}

#[derive(Debug, Clone, Copy)]
pub struct TargetRateConstraints {
    pub at_time: Timestamp,
    pub min_data_rate: Option<DataRate>,
    pub max_data_rate: Option<DataRate>,
    /// The initial bandwidth estimate to base target rate on. This should be used
    /// as the basis for initial OnTargetTransferRate and OnPacerConfig callbacks.
    pub starting_rate: Option<DataRate>,
}

impl Default for TargetRateConstraints {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            min_data_rate: None,
            max_data_rate: None,
            starting_rate: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NetworkAvailability {
    pub at_time: Timestamp,
    pub network_available: bool,
}

impl Default for NetworkAvailability {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            network_available: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PacedPacketInfo {
    pub probe_cluster_id: i32,
    pub probe_cluster_min_probes: i64,
    pub probe_cluster_min_bytes: i64,
}

impl PacedPacketInfo {
    pub const NOT_APROBE: i32 = -1;

    pub const fn new(
        probe_cluster_id: i32,
        probe_cluster_min_probes: i64,
        probe_cluster_min_bytes: i64,
    ) -> Self {
        Self {
            probe_cluster_id,
            probe_cluster_min_probes,
            probe_cluster_min_bytes,
        }
    }
}

impl Default for PacedPacketInfo {
    fn default() -> Self {
        Self::new(Self::NOT_APROBE, -1, -1)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SentPacket {
    pub send_time: Timestamp,
    /// Size of packet with overhead up to IP layer.
    pub size: DataSize,
    /// Probe cluster id and parameters including bitrate, number of packets and
    /// number of bytes.
    pub pacing_info: PacedPacketInfo,
    /// Tracked data in flight when the packet was sent, excluding unacked data.
    pub data_in_flight: DataSize,
}

impl Default for SentPacket {
    fn default() -> Self {
        Self {
            send_time: Timestamp::plus_infinity(),
            size: DataSize::zero(),
            pacing_info: PacedPacketInfo::default(),
            data_in_flight: DataSize::zero(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RoundTripTimeUpdate {
    pub round_trip_time: TimeDelta,
}

/// Packet level feedback
#[derive(Debug, Clone, Copy)]
pub struct PacketResult {
    pub sent_packet: SentPacket,
    pub receive_time: Timestamp,
    /// Whether `receive_time` is only a stand-in, so the packet counts as received but not in
    /// delay or RTT calculations, as upstream marks a retransmission it cannot match
    pub ambiguous_receive_time: bool,
}

impl PacketResult {
    pub const fn is_received(&self) -> bool {
        !self.receive_time.is_plus_infinity()
    }
}

impl Default for PacketResult {
    fn default() -> Self {
        Self {
            sent_packet: SentPacket::default(),
            receive_time: Timestamp::plus_infinity(),
            ambiguous_receive_time: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransportPacketsFeedback {
    pub feedback_time: Timestamp,
    pub data_in_flight: DataSize,
    pub packet_feedbacks: Vec<PacketResult>,
}

impl Default for TransportPacketsFeedback {
    fn default() -> Self {
        Self {
            feedback_time: Timestamp::plus_infinity(),
            data_in_flight: DataSize::zero(),
            packet_feedbacks: Vec::new(),
        }
    }
}

impl TransportPacketsFeedback {
    /// Packets received at a known time; one received at an ambiguous time is left out, since
    /// every caller reads the time
    pub fn received_with_send_info(&self) -> impl Iterator<Item = &PacketResult> {
        self.packet_feedbacks
            .iter()
            .filter(|fb| fb.is_received() && !fb.ambiguous_receive_time)
    }

    pub fn sorted_by_receive_time(&self) -> Vec<PacketResult> {
        let mut res: Vec<PacketResult> = self.received_with_send_info().cloned().collect();
        // Upstream breaks a tie on send time, then sequence number; a stable sort keeps send
        // order, which QUIC acknowledges in.
        res.sort_by(|a, b| {
            a.receive_time
                .cmp(&b.receive_time)
                .then(a.sent_packet.send_time.cmp(&b.sent_packet.send_time))
        });
        res
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PacerConfig {
    /// Pacer should send at most data_window data over time_window duration.
    /// If `time_window` is zero, the pacer should pace every packet as accurately
    /// as possible and data_rate() is calculated over one second.
    pub data_window: DataSize,
    pub time_window: TimeDelta,
}

impl Default for PacerConfig {
    fn default() -> Self {
        Self {
            data_window: DataSize::infinity(),
            time_window: TimeDelta::plus_infinity(),
        }
    }
}

impl PacerConfig {
    pub const DEFAULT_TIME_INTERVAL: TimeDelta = TimeDelta::from_millis(40);

    pub fn create(send_rate: DataRate, time_window: TimeDelta) -> Self {
        let mut pacer_config = Self {
            time_window,
            ..Default::default()
        };
        // Note that precision is lost here, since DataSize store number of bytes.
        pacer_config.data_window = send_rate * pacer_config.rate_window();
        pacer_config
    }

    pub fn rate_window(&self) -> TimeDelta {
        if self.time_window.is_zero() {
            TimeDelta::from_seconds(1)
        } else {
            self.time_window
        }
    }

    pub fn data_rate(&self) -> DataRate {
        self.data_window / self.rate_window()
    }
}

#[derive(Debug, Clone)]
pub struct ProbeClusterConfig {
    pub at_time: Timestamp,
    pub target_data_rate: DataRate,
    /// Duration of a probe.
    pub target_duration: TimeDelta,
    /// Delta time between sent bursts of packets during probe.
    pub min_probe_delta: TimeDelta,
    pub target_probe_count: i32,
    pub id: i32,
}

impl Default for ProbeClusterConfig {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            target_data_rate: DataRate::zero(),
            target_duration: TimeDelta::zero(),
            min_probe_delta: TimeDelta::from_millis(2),
            target_probe_count: 0,
            id: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TargetTransferRate {
    pub target_rate: DataRate,
}

/// Contains updates of network controller comand state. Using optionals to
/// indicate whether a member has been updated. The array of probe clusters
/// should be used to send out probes if not empty.
#[derive(Default, Debug, Clone)]
pub struct NetworkControlUpdate {
    pub congestion_window: Option<DataSize>,
    pub pacer_config: Option<PacerConfig>,
    pub probe_cluster_configs: Vec<ProbeClusterConfig>,
    pub target_rate: Option<TargetTransferRate>,
}

// Process control
pub struct ProcessInterval {
    pub at_time: Timestamp,
    pub pacer_queue: Option<DataSize>,
}

impl Default for ProcessInterval {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            pacer_queue: None,
        }
    }
}
