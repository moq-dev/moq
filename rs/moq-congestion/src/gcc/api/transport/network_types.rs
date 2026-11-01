use std::cmp::Ordering;

use crate::api::units::{DataRate, DataSize, TimeDelta, Timestamp};

use super::EcnMarking;

/// Represents constraints and rates related to the currently enabled streams.
/// This is used as input to the congestion controller via the StreamsConfig
/// struct.
#[derive(Default, Clone, Copy, Debug)]
pub struct BitrateAllocationLimits {
    // The total minimum send bitrate required by all sending streams.
    pub min_allocatable_rate: DataRate,
    // The total maximum allocatable bitrate for all currently available streams.
    pub max_allocatable_rate: DataRate,
    // The max bitrate to use for padding. The sum of the per-stream max padding
    // rate.
    pub max_padding_rate: DataRate,
}

/// Use StreamsConfig for information about streams that is required for specific
/// adjustments to the algorithms in network controllers. Especially useful
/// for experiments.
#[derive(Debug, Clone, Copy)]
pub struct StreamsConfig {
    pub at_time: Timestamp,
    pub requests_alr_probing: Option<bool>,
    /// If `enable_repeated_initial_probing` is set to true, Probes are sent
    /// periodically every 1s during the first 5s after the network becomes
    /// available. The probes ignores max_total_allocated_bitrate.
    pub enable_repeated_initial_probing: Option<bool>,
    pub pacing_factor: Option<f64>,

    // TODO(srte): Use BitrateAllocationLimits here.
    pub min_total_allocated_bitrate: Option<DataRate>,
    pub max_padding_rate: Option<DataRate>,
    pub max_total_allocated_bitrate: Option<DataRate>,
}

impl Default for StreamsConfig {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            requests_alr_probing: None,
            enable_repeated_initial_probing: None,
            pacing_factor: None,
            min_total_allocated_bitrate: None,
            max_padding_rate: None,
            max_total_allocated_bitrate: None,
        }
    }
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

#[derive(Debug, Clone, Copy)]
pub struct NetworkRouteChange {
    pub at_time: Timestamp,
    /// The TargetRateConstraints are set here so they can be changed synchronously
    /// when network route changes.
    pub constraints: TargetRateConstraints,
}

impl Default for NetworkRouteChange {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            constraints: TargetRateConstraints::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PacedPacketInfo {
    pub send_bitrate: DataRate,
    pub probe_cluster_id: i32,
    pub probe_cluster_min_probes: i64,
    pub probe_cluster_min_bytes: i64,
    pub probe_cluster_bytes_sent: i64,
}

impl PacedPacketInfo {
    pub const NOT_APROBE: i32 = -1;

    pub const fn new(
        probe_cluster_id: i32,
        probe_cluster_min_probes: i64,
        probe_cluster_min_bytes: i64,
    ) -> Self {
        Self {
            send_bitrate: DataRate::from_bits_per_sec(0),
            probe_cluster_id,
            probe_cluster_min_probes,
            probe_cluster_min_bytes,
            probe_cluster_bytes_sent: 0,
        }
    }
}

impl Default for PacedPacketInfo {
    fn default() -> Self {
        Self {
            send_bitrate: DataRate::from_bits_per_sec(0),
            probe_cluster_id: Self::NOT_APROBE,
            probe_cluster_min_probes: -1,
            probe_cluster_min_bytes: -1,
            probe_cluster_bytes_sent: 0,
        }
    }
}

impl PartialEq for PacedPacketInfo {
    fn eq(&self, other: &Self) -> bool {
        self.send_bitrate == other.send_bitrate
            && self.probe_cluster_id == other.probe_cluster_id
            && self.probe_cluster_min_probes == other.probe_cluster_min_probes
            && self.probe_cluster_min_bytes == other.probe_cluster_min_bytes
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SentPacket {
    pub send_time: Timestamp,
    /// Size of packet with overhead up to IP layer.
    pub size: DataSize,
    /// Size of preceeding packets that are not part of feedback.
    pub prior_unacked_data: DataSize,
    /// Probe cluster id and parameters including bitrate, number of packets and
    /// number of bytes.
    pub pacing_info: PacedPacketInfo,
    /// True if the packet is an audio packet, false for video, padding, RTX etc.
    pub audio: bool,
    /// Transport independent sequence number, any tracked packet should have a
    /// sequence number that is unique over the whole call and increasing by 1 for
    /// each packet.
    pub sequence_number: i64,
    /// Tracked data in flight when the packet was sent, excluding unacked data.
    pub data_in_flight: DataSize,
}

impl Default for SentPacket {
    fn default() -> Self {
        Self {
            send_time: Timestamp::plus_infinity(),
            size: DataSize::zero(),
            prior_unacked_data: DataSize::zero(),
            pacing_info: PacedPacketInfo::default(),
            audio: false,
            sequence_number: 0,
            data_in_flight: DataSize::zero(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReceivedPacket {
    pub send_time: Timestamp,
    pub receive_time: Timestamp,
    pub size: DataSize,
}

impl Default for ReceivedPacket {
    fn default() -> Self {
        Self {
            send_time: Timestamp::minus_infinity(),
            receive_time: Timestamp::plus_infinity(),
            size: DataSize::zero(),
        }
    }
}

// Transport level feedback

#[derive(Debug, Clone, Copy)]
pub struct RemoteBitrateReport {
    pub receive_time: Timestamp,
    pub bandwidth: DataRate,
}

impl Default for RemoteBitrateReport {
    fn default() -> Self {
        Self {
            receive_time: Timestamp::plus_infinity(),
            bandwidth: DataRate::infinity(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RoundTripTimeUpdate {
    pub receive_time: Timestamp,
    pub round_trip_time: TimeDelta,
    pub smoothed: bool,
}

impl Default for RoundTripTimeUpdate {
    fn default() -> Self {
        Self {
            receive_time: Timestamp::plus_infinity(),
            round_trip_time: TimeDelta::plus_infinity(),
            smoothed: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransportLossReport {
    pub receive_time: Timestamp,
    pub start_time: Timestamp,
    pub end_time: Timestamp,
    pub packets_lost_delta: u64,
    pub packets_received_delta: u64,
}

impl Default for TransportLossReport {
    fn default() -> Self {
        Self {
            receive_time: Timestamp::plus_infinity(),
            start_time: Timestamp::plus_infinity(),
            end_time: Timestamp::plus_infinity(),
            packets_lost_delta: 0,
            packets_received_delta: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PacketResult {
    pub sent_packet: SentPacket,
    pub receive_time: Timestamp,
    pub ecn: EcnMarking,
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
            ecn: EcnMarking::NotEct,
        }
    }
}

impl PartialOrd for PacketResult {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.receive_time != other.receive_time {
            return Some(self.receive_time.cmp(&other.receive_time));
        }

        if self.sent_packet.send_time != other.sent_packet.send_time {
            return Some(self.sent_packet.send_time.cmp(&other.sent_packet.send_time));
        }

        Some(
            self.sent_packet
                .sequence_number
                .cmp(&other.sent_packet.sequence_number),
        )
    }
}

impl PartialEq for PacketResult {
    fn eq(&self, other: &Self) -> bool {
        self.receive_time == other.receive_time
            && self.sent_packet.send_time == other.sent_packet.send_time
            && self.sent_packet.sequence_number == other.sent_packet.sequence_number
    }
}

#[derive(Debug, Clone)]
pub struct TransportPacketsFeedback {
    pub feedback_time: Timestamp,
    pub data_in_flight: DataSize,
    pub packet_feedbacks: Vec<PacketResult>,

    /// Arrival times for messages without send time information.
    pub sendless_arrival_times: Vec<Timestamp>,
}

impl Default for TransportPacketsFeedback {
    fn default() -> Self {
        Self {
            feedback_time: Timestamp::plus_infinity(),
            data_in_flight: DataSize::zero(),
            packet_feedbacks: Vec::new(),
            sendless_arrival_times: Vec::new(),
        }
    }
}

impl TransportPacketsFeedback {
    /// NOTE: These returned Vec copies in the original C++ code. Use collect() if you want that behavior.
    pub fn received_with_send_info(&self) -> impl Iterator<Item = &PacketResult> {
        self.packet_feedbacks.iter().filter(|fb| fb.is_received())
    }

    pub fn lost_with_send_info(&self) -> impl Iterator<Item = &PacketResult> {
        self.packet_feedbacks.iter().filter(|fb| !fb.is_received())
    }

    pub fn packets_with_feedback(&self) -> &Vec<PacketResult> {
        &self.packet_feedbacks
    }

    pub fn sorted_by_receive_time(&self) -> Vec<PacketResult> {
        let mut res: Vec<PacketResult> = self.received_with_send_info().cloned().collect();
        res.sort_by(|a, b| a.receive_time.cmp(&b.receive_time));
        res
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NetworkEstimate {
    pub at_time: Timestamp,
    /// Deprecated, use TargetTransferRate::target_rate instead.
    pub bandwidth: DataRate,
    pub round_trip_time: TimeDelta,
    pub bwe_period: TimeDelta,

    pub loss_rate_ratio: f32,
}

impl Default for NetworkEstimate {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            bandwidth: DataRate::infinity(),
            round_trip_time: TimeDelta::plus_infinity(),
            bwe_period: TimeDelta::plus_infinity(),
            loss_rate_ratio: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PacerConfig {
    pub at_time: Timestamp,
    /// Pacer should send at most data_window data over time_window duration.
    pub data_window: DataSize,
    pub time_window: TimeDelta,
    /// Pacer should send at least pad_window data over time_window duration.
    pub pad_window: DataSize,
}

impl Default for PacerConfig {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            data_window: DataSize::infinity(),
            time_window: TimeDelta::plus_infinity(),
            pad_window: DataSize::zero(),
        }
    }
}

impl PacerConfig {
    pub fn data_rate(&self) -> DataRate {
        self.data_window / self.time_window
    }
    pub fn pad_rate(&self) -> DataRate {
        self.pad_window / self.time_window
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
    pub at_time: Timestamp,
    /// The estimate on which the target rate is based on.
    pub network_estimate: NetworkEstimate,
    pub target_rate: DataRate,
    pub stable_target_rate: DataRate,
    pub cwnd_reduce_ratio: f64,
}

impl Default for TargetTransferRate {
    fn default() -> Self {
        Self {
            at_time: Timestamp::plus_infinity(),
            network_estimate: NetworkEstimate::default(),
            target_rate: DataRate::zero(),
            stable_target_rate: DataRate::zero(),
            cwnd_reduce_ratio: 0.0,
        }
    }
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

impl NetworkControlUpdate {
    pub fn has_updates(&self) -> bool {
        self.congestion_window.is_some()
            || self.pacer_config.is_some()
            || !self.probe_cluster_configs.is_empty()
            || self.target_rate.is_some()
    }
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

/// Under development, subject to change without notice.
#[derive(Debug, Clone, Copy)]
pub struct NetworkStateEstimate {
    pub confidence: f64,

    // The time the estimate was received/calculated.
    pub update_time: Timestamp,
    pub last_receive_time: Timestamp,
    pub last_send_time: Timestamp,

    // Total estimated link capacity.
    pub link_capacity: DataRate,
    // Used as a safe measure of available capacity.
    pub link_capacity_lower: DataRate,
    // Used as limit for increasing bitrate.
    pub link_capacity_upper: DataRate,

    pub pre_link_buffer_delay: TimeDelta,
    pub post_link_buffer_delay: TimeDelta,
    pub propagation_delay: TimeDelta,

    // Only for debugging
    pub time_delta: TimeDelta,
    pub last_feed_time: Timestamp,
    pub cross_delay_rate: f64,
    pub spike_delay_rate: f64,
    pub link_capacity_std_dev: DataRate,
    pub link_capacity_min: DataRate,
    pub cross_traffic_ratio: f64,
}

impl Default for NetworkStateEstimate {
    fn default() -> Self {
        Self {
            confidence: f64::NAN,
            update_time: Timestamp::minus_infinity(),
            last_receive_time: Timestamp::minus_infinity(),
            last_send_time: Timestamp::minus_infinity(),
            link_capacity: DataRate::minus_infinity(),
            link_capacity_lower: DataRate::minus_infinity(),
            link_capacity_upper: DataRate::minus_infinity(),
            pre_link_buffer_delay: TimeDelta::minus_infinity(),
            post_link_buffer_delay: TimeDelta::minus_infinity(),
            propagation_delay: TimeDelta::minus_infinity(),
            time_delta: TimeDelta::minus_infinity(),
            last_feed_time: Timestamp::minus_infinity(),
            cross_delay_rate: f64::NAN,
            spike_delay_rate: f64::NAN,
            link_capacity_std_dev: DataRate::minus_infinity(),
            link_capacity_min: DataRate::minus_infinity(),
            cross_traffic_ratio: f64::NAN,
        }
    }
}
