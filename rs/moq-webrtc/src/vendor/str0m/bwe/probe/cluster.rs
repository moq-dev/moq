//! Probe cluster data structures for bandwidth estimation.
//!
//! Probe clusters are short bursts of packets sent at specific bitrates to test
//! network capacity.

use std::cmp;
use std::time::{Duration, Instant};

use crate::rtp_::{Bitrate, DataSize, TwccClusterId};
use crate::util::{already_happened, not_happening};

const MAX_PADDING_PACKET_SIZE: DataSize = DataSize::bytes(240);

/// Configuration for a probe cluster (the plan).
///
/// This represents the immutable blueprint for a bandwidth probe: what bitrate
/// to test, for how long, and with what constraints.
#[derive(Debug, Clone, Copy)]
pub struct ProbeClusterConfig {
    /// Unique identifier for this probe cluster
    cluster: TwccClusterId,

    /// Target bitrate to probe at (e.g., 3 Mbps)
    target_bitrate: Bitrate,

    /// How long to sustain the target bitrate (e.g., 15ms)
    target_duration: Duration,

    /// Minimum number of packets to send (e.g., 5)
    /// This ensures statistical validity even for short bursts.
    min_packet_count: usize,

    /// Delta time between sent bursts of packets during probe.
    ///
    /// Mirrors WebRTC's `ProbeClusterConfig.min_probe_delta`. The pacer/probe sender must not
    /// schedule probe packets more frequently than this.
    min_probe_delta: Duration,

    /// The kind of probe this is.
    kind: ProbeKind,
}

/// Kind of probe
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProbeKind {
    Initial,
    Exponential,
    Allocation { in_alr: bool },
    PeriodicAlr,
    LargeDrop,
    Stagnant,
}

impl ProbeKind {
    pub(crate) fn is_alr(&self) -> bool {
        matches!(
            self,
            ProbeKind::PeriodicAlr | ProbeKind::Allocation { in_alr: true }
        )
    }
}

impl ProbeClusterConfig {
    /// Create a new probe cluster configuration with standard defaults:
    /// - 15ms duration (enough to get meaningful feedback without excessive delay)
    /// - 5 minimum packets (statistical significance for BWE analysis)
    pub fn new(cluster: TwccClusterId, target_bitrate: Bitrate, kind: ProbeKind) -> Self {
        Self {
            cluster,
            target_bitrate,
            // WebRTC defaults
            target_duration: Duration::from_millis(15),
            min_packet_count: 5,
            // WebRTC default for general probing (not initial/network-state): 2ms.
            min_probe_delta: Duration::from_millis(2),
            kind,
        }
    }

    /// Set a custom target duration for this probe.
    /// WebRTC uses 100ms for initial probes to allow time for media to start.
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.target_duration = duration;
        self
    }

    /// Set a custom minimum packet count for this probe.
    pub fn with_min_packet_count(mut self, min_packet_count: usize) -> Self {
        self.min_packet_count = min_packet_count;
        self
    }

    /// Set a custom minimum probe delta (spacing constraint) for this probe.
    pub fn with_min_probe_delta(mut self, min_probe_delta: Duration) -> Self {
        self.min_probe_delta = min_probe_delta;
        self
    }

    /// Check if this probe was created during ALR.
    pub fn is_alr_probe(&self) -> bool {
        self.kind.is_alr()
    }

    /// Get the probe cluster ID.
    pub fn cluster(&self) -> TwccClusterId {
        self.cluster
    }

    /// Get the target bitrate.
    pub fn target_bitrate(&self) -> Bitrate {
        self.target_bitrate
    }

    /// Get the minimum packet count required for a valid probe.
    pub fn min_packet_count(&self) -> usize {
        self.min_packet_count
    }

    /// Get the minimum probe delta (spacing constraint).
    pub fn min_probe_delta(&self) -> Duration {
        self.min_probe_delta
    }

    /// Calculate the target bytes for this probe.
    /// This is how much data we expect to send at target_bitrate for target_duration.
    pub fn target_bytes(&self) -> DataSize {
        self.target_bitrate * self.target_duration
    }
}

/// Runtime state of an active probe cluster (the execution).
///
/// This tracks what's actually happening as we send probe packets: how much
/// we've sent, how many packets, and when we started.
#[derive(Debug)]
pub struct ProbeClusterState {
    /// The immutable plan
    config: ProbeClusterConfig,

    /// Total bytes sent so far in this probe
    bytes_sent: DataSize,

    /// Total packets sent so far in this probe
    packets_sent: usize,

    /// When the first packet was sent
    /// None = probe hasn't started yet (still queued)
    started_at: Option<Instant>,

    /// When the last packet was sent
    /// Used to calculate actual send duration (first to last packet)
    last_packet_at: Option<Instant>,

    /// When we last created a padding request
    /// Used to prevent creating multiple padding packets for the same instant
    last_padding_at: Instant,
}

impl ProbeClusterState {
    /// Create a new probe cluster state from a config.
    pub fn new(config: ProbeClusterConfig) -> Self {
        Self {
            config,
            bytes_sent: DataSize::ZERO,
            packets_sent: 0,
            started_at: None,
            last_packet_at: None,
            last_padding_at: already_happened(),
        }
    }

    /// Get the probe configuration.
    pub fn config(&self) -> &ProbeClusterConfig {
        &self.config
    }

    /// Calculates the next probe time based on total bytes sent and target bitrate.
    /// This naturally handles variable packet sizes (media packets can be much larger
    /// than padding packets). Next packet can be sent when:
    /// `now >= start_time + (bytes_sent / target_bitrate)`
    pub fn next_probe_time(&self) -> Instant {
        let Some(started_at) = self.started_at else {
            return already_happened();
        };

        if self.config.target_bitrate == Bitrate::ZERO {
            return not_happening();
        }

        // Packet-level pacing: schedule based on bytes sent at the target bitrate.
        // next_time = start_time + (bytes_sent / target_bitrate)
        let send_duration = self.bytes_sent / self.config.target_bitrate;
        let probe_time = started_at + send_duration;

        let min_burst_time = self.last_padding_at + self.config.min_probe_delta();
        probe_time.max(min_burst_time)
    }

    /// Check if the probe cluster is complete.
    ///
    /// A probe is complete when BOTH conditions are met (matching WebRTC):
    /// 1. Sent bytes >= target_bytes (target_bitrate * target_duration)
    /// 2. Sent packets >= min_packet_count
    ///
    /// Note: WebRTC does NOT check duration for completion. Duration is only used
    /// to calculate the target bytes threshold. This ensures probes complete even
    /// when all packets are sent at the same instant (e.g., in tests or when the
    /// pacer generates bursts faster than wall-clock time advances).
    pub fn is_complete(&self, _now: Instant) -> bool {
        if self.started_at.is_none() {
            return false; // Not started yet
        }

        // Match WebRTC's BitrateProber::ProbeSent() logic:
        // if (sent_bytes >= probe_cluster_min_bytes && sent_probes >= probe_cluster_min_probes)
        let bytes_met = self.bytes_sent >= self.config.target_bytes();
        let packets_met = self.packets_sent >= self.config.min_packet_count;

        bytes_met && packets_met
    }

    /// Check if it's time to send the next probe packet.
    ///
    /// Returns `true` if `now >= next_probe_time()`, meaning we should send a packet now.
    pub fn should_send_now(&self, now: Instant) -> bool {
        let next_time = self.next_probe_time();
        let min_burst_time = self.last_padding_at + self.config.min_probe_delta();
        now >= next_time && now >= min_burst_time
    }

    /// Calculate how much padding should be generated for the next probe packet.
    ///
    /// WebRTC probing uses **bursts** of packets, with a minimum delta between bursts
    /// (`min_probe_delta`). In str0m, a `PaddingRequest` is expressed as a number of bytes
    /// to queue (which may result in multiple RTP padding packets), so we request a burst
    /// sized approximately as:
    ///
    /// `burst_bytes = target_bitrate * min_probe_delta`
    ///
    /// This avoids unintentionally capping probe throughput at `MAX_PADDING_PACKET_SIZE /
    /// min_probe_delta` (e.g. 240 bytes / 2ms = 960 kbit/s), which would be far below the
    /// intended probe bitrate.
    ///
    /// Returns `None` if it's not time to send yet, or if we've already created
    /// a padding packet for this instant.
    pub fn next_packet(&mut self, now: Instant) -> Option<DataSize> {
        if self.started_at.is_none() {
            self.started_at = Some(now);
        }

        // Enforce min burst spacing (`min_probe_delta`) between successive padding requests.
        // (The very first burst is not gated because `last_padding_at` starts at already_happened()).
        if now < self.last_padding_at + self.config.min_probe_delta() {
            return None;
        }

        if now < self.next_probe_time() {
            return None;
        }

        // Check if we've already created a padding packet for this exact instant
        // This prevents creating multiple padding packets when handle_timeout() is
        // called multiple times with the same `now` value
        if self.last_padding_at >= now {
            return None;
        }

        // Mark that we've created padding for this instant
        self.last_padding_at = now;

        // It's time to send a new probe burst.
        // Calculate recommended probe size as target_bitrate * min_probe_delta.
        let min_delta = self.config.min_probe_delta();
        let recommended_probe_size = self.config.target_bitrate * min_delta;

        // Ensure we request at least one padding packet worth of bytes even if min_delta is 0.
        let recommended_probe_size = DataSize::bytes(cmp::max(
            recommended_probe_size.as_bytes_i64(),
            MAX_PADDING_PACKET_SIZE.as_bytes_i64(),
        ));

        // Calculate remaining bytes needed to complete the probe cluster.
        let bytes_remaining = self.config.target_bytes().saturating_sub(self.bytes_sent);

        // Like BitrateProber::ProbeSent, completion requires both the byte and
        // packet thresholds. At low rates the byte target can fit in one packet;
        // keep sending bounded padding until the minimum packet count is met.
        let request_bytes = if bytes_remaining == DataSize::ZERO
            && self.packets_sent < self.config.min_packet_count
        {
            MAX_PADDING_PACKET_SIZE
        } else {
            cmp::min(bytes_remaining, recommended_probe_size)
        };

        if request_bytes == DataSize::ZERO {
            None
        } else {
            Some(request_bytes)
        }
    }

    /// Record a packet that was sent as part of this probe (media or padding).
    ///
    /// This updates the probe's tracking state so that `next_probe_time()` correctly
    /// calculates when the next packet should be sent based on the target bitrate.
    ///
    /// Should be called by the pacer whenever ANY packet is sent during an active probe,
    /// not just padding packets generated by `next_packet()`.
    pub fn record_packet(&mut self, now: Instant, size: DataSize) {
        // If a probe is active and we observe a media packet before any probe-generated padding,
        // treat that as the start of the probe. This keeps timing semantics consistent and ensures
        // `next_probe_time()` enforces `min_probe_delta` from the first packet onwards.
        if self.started_at.is_none() {
            self.started_at = Some(now);
        }

        self.bytes_sent += size;
        self.packets_sent += 1;
        self.last_packet_at = Some(now);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn low_rate_padding_reaches_minimum_packet_count() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(0.into(), Bitrate::kbps(10), ProbeKind::Initial);
        let mut state = ProbeClusterState::new(config);
        for i in 0..config.min_packet_count() {
            let at = if i == 0 { now } else { state.next_probe_time() };
            let size = state.next_packet(at).expect("minimum packets still needed");
            assert!(size <= MAX_PADDING_PACKET_SIZE);
            state.record_packet(at, size);
        }
        assert!(state.is_complete(state.next_probe_time()));
    }

    // Test helper to directly set state for testing
    impl ProbeClusterState {
        fn test_set_state(&mut self, bytes_sent: DataSize, packets_sent: usize, now: Instant) {
            self.bytes_sent = bytes_sent;
            self.packets_sent = packets_sent;
            if self.started_at.is_none() {
                self.started_at = Some(now);
            }
            self.last_packet_at = Some(now);
        }
    }

    #[test]
    fn probe_cluster_not_complete_before_start() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(1.into(), Bitrate::mbps(3), ProbeKind::Initial);
        let state = ProbeClusterState::new(config);

        assert!(!state.is_complete(now));
    }

    #[test]
    fn probe_cluster_not_complete_after_packets_but_not_bytes() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(1.into(), Bitrate::mbps(3), ProbeKind::Initial);
        // target_bytes = 3 Mbps * 15ms = 45,000 bits = 5,625 bytes
        let mut state = ProbeClusterState::new(config);

        // Send 5 packets (meets packet count) but only small packets (doesn't meet bytes)
        // 5 * 100 = 500 bytes, which is < 5,625 bytes
        state.test_set_state(DataSize::bytes(500), 5, now);

        // Not complete: packets met but bytes not met
        assert!(!state.is_complete(now));
    }

    #[test]
    fn probe_cluster_not_complete_after_bytes_but_not_packets() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(1.into(), Bitrate::mbps(3), ProbeKind::Initial);
        // target_bytes = 3 Mbps * 15ms = 45,000 bits = 5,625 bytes
        let mut state = ProbeClusterState::new(config);

        // Send 2 large packets (meets bytes) but not enough packets
        // 2 * 3000 = 6000 bytes > 5,625 bytes, but only 2 packets < 5 packets
        state.test_set_state(DataSize::bytes(6000), 2, now);

        // Not complete: bytes met but packets not met
        assert!(!state.is_complete(now));
    }

    #[test]
    fn probe_cluster_complete_when_both_criteria_met() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(1.into(), Bitrate::mbps(3), ProbeKind::Initial);
        // target_bytes = 3 Mbps * 15ms = 45,000 bits = 5,625 bytes
        let mut state = ProbeClusterState::new(config);

        // Send 5 packets of 1200 bytes each = 6000 bytes
        // Meets both: 6000 >= 5625 bytes AND 5 >= 5 packets
        state.test_set_state(DataSize::bytes(6000), 5, now);

        // Complete: both bytes and packets met, even at same instant
        assert!(state.is_complete(now));
    }

    #[test]
    fn probe_cluster_complete_even_with_zero_duration() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(1.into(), Bitrate::mbps(3), ProbeKind::Initial);
        let mut state = ProbeClusterState::new(config);

        // Send all packets at exactly the same instant (duration = 0)
        state.test_set_state(DataSize::bytes(6000), 5, now);

        // Should still complete (this is the bug fix - no duration requirement)
        assert!(state.is_complete(now));
    }

    #[test]
    fn probe_cluster_tracks_bytes_and_packets() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(1.into(), Bitrate::mbps(3), ProbeKind::Initial);
        let mut state = ProbeClusterState::new(config);

        state.test_set_state(DataSize::bytes(1200), 1, now);
        assert_eq!(state.bytes_sent, DataSize::bytes(1200));
        assert_eq!(state.packets_sent, 1);

        state.test_set_state(DataSize::bytes(2200), 2, now + Duration::from_millis(1));
        assert_eq!(state.bytes_sent, DataSize::bytes(2200));
        assert_eq!(state.packets_sent, 2);
    }

    #[test]
    fn min_probe_delta_is_enforced_in_next_probe_time() {
        let now = Instant::now();
        let config = ProbeClusterConfig::new(1.into(), Bitrate::mbps(3), ProbeKind::Initial)
            .with_min_probe_delta(Duration::from_millis(20));
        let mut state = ProbeClusterState::new(config);
        assert_eq!(state.config().min_probe_delta(), Duration::from_millis(20));

        // Start a burst at `now`, then verify that a second burst is blocked until >= now+20ms.
        assert!(state.next_packet(now).is_some());
        assert!(state.next_packet(now + Duration::from_millis(19)).is_none());
        assert!(state.next_packet(now + Duration::from_millis(20)).is_some());
        let next = state.last_padding_at;
        assert!(
            next >= now + Duration::from_millis(20),
            "next_probe_time {next:?} must be >= now + min_probe_delta"
        );
    }
}
