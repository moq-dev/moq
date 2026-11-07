//! Statistics events.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::Bitrate;
use crate::rtp::SeqNo;
use crate::rtp_::{Mid, Rid};
use crate::{io::Protocol, rtp_::MidRid};

pub(crate) struct Stats {
    last_now: Option<Instant>,
    events: VecDeque<StatsEvent>,
    interval: Duration,
}

pub(crate) struct StatsSnapshot {
    pub peer_tx: u64,
    pub peer_rx: u64,
    pub tx: u64,
    pub rx: u64,
    pub egress_loss_fraction: Option<f32>,
    pub ingress_loss_fraction: Option<f32>,
    pub rtt: Option<Duration>,
    pub ingress: HashMap<MidRid, MediaIngressStats>,
    pub egress: HashMap<MidRid, MediaEgressStats>,
    pub bwe_tx: Option<Bitrate>,
    pub selected_candidate_pair: Option<CandidatePairStats>,
    timestamp: Instant,
}

impl StatsSnapshot {
    pub(crate) fn new(timestamp: Instant) -> StatsSnapshot {
        StatsSnapshot {
            peer_rx: 0,
            peer_tx: 0,
            tx: 0,
            rx: 0,
            egress_loss_fraction: None,
            ingress_loss_fraction: None,
            ingress: HashMap::new(),
            egress: HashMap::new(),
            rtt: None,
            bwe_tx: None,
            selected_candidate_pair: None,
            timestamp,
        }
    }
}

// Output events

#[derive(Debug, Clone)]
pub(crate) enum StatsEvent {
    Peer(PeerStats),
    MediaEgress(MediaEgressStats),
    MediaIngress(MediaIngressStats),
}

/// Peer statistics in [`Event::PeerStats`][crate::Event::PeerStats].
///
/// This event is generated roughly every second
#[derive(Debug, Clone)]
pub struct PeerStats {
    /// Total bytes transmitted.
    pub peer_bytes_rx: u64,
    /// Total bytes received.
    pub peer_bytes_tx: u64,
    /// Total bytes transmitted, only counting media traffic (rtp payload).
    pub bytes_rx: u64,
    /// Total bytes received, only counting media traffic (rtp payload).
    pub bytes_tx: u64,
    /// Timestamp when this event was generated.
    pub timestamp: Instant,
    /// The last egress bandwidth estimate from the BWE subsystem, if enabled.
    pub bwe_tx: Option<Bitrate>,
    /// The egress loss over the last second.
    pub egress_loss_fraction: Option<f32>,
    /// The ingress loss since the last stats event.
    pub ingress_loss_fraction: Option<f32>,
    /// The most recent RTT since the last stats event.
    pub rtt: Option<Duration>,
    /// The selected ICE candidate pair, if any.
    pub selected_candidate_pair: Option<CandidatePairStats>,
}

#[derive(Debug, Clone)]
/// ICE candidate pair statistics.
pub struct CandidatePairStats {
    /// The selected protocol.
    pub protocol: Protocol,
    /// The local candidate.
    pub local: CandidateStats,
    /// The remote candidate.
    pub remote: CandidateStats,
    /// Round-trip time of the most recent STUN binding transaction on the
    /// nominated pair.
    ///
    /// Spec equivalent of [`RTCIceCandidatePairStats.currentRoundTripTime`][1].
    ///
    /// This is sourced from the ICE keepalive/consent traffic and is
    /// available even on receive-only endpoints where RTP-based RTT
    /// estimates aren't.
    ///
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcicecandidatepairstats-currentroundtriptime
    pub current_round_trip_time: Option<Duration>,
    /// Sum of all RTTs measured from successful STUN binding transactions
    /// on the nominated pair over its lifetime. Divide by
    /// [`responses_received`][Self::responses_received] for the average.
    ///
    /// Spec equivalent of [`RTCIceCandidatePairStats.totalRoundTripTime`][1].
    ///
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcicecandidatepairstats-totalroundtriptime
    pub total_round_trip_time: Duration,
    /// Total number of STUN binding responses received on the nominated
    /// pair over its lifetime.
    ///
    /// Spec equivalent of [`RTCIceCandidatePairStats.responsesReceived`][1].
    ///
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcicecandidatepairstats-responsesreceived
    pub responses_received: u64,
}

#[derive(Debug, Clone)]
/// ICE candidate statistics.
pub struct CandidateStats {
    /// The address of the candidate.
    pub addr: SocketAddr,
}

/// Outgoing media statistics in [`Event::MediaEgressStats`][crate::Event::MediaEgressStats].
///
/// note: when simulcast is disabled, `rid` is `None`
#[derive(Debug, Clone)]
pub struct MediaEgressStats {
    /// The identifier of the media these stats are for.
    pub mid: Mid,
    /// The Rid identifier in case of simulcast.
    pub rid: Option<Rid>,
    /// Total bytes sent, including retransmissions.
    ///
    /// Spec equivalent to [`RTCSentRtpStreamStats.bytesSent`][1].
    ///
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcsentrtpstreamstats-bytessent
    pub bytes: u64,
    /// Total number of rtp packets sent, including retransmissions
    ///
    /// Spec equivalent of [`RTCSentRtpStreamStats.packetsSent`][1].
    ///
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcsentrtpstreamstats-packetssent
    pub packets: u64,
    /// Number of firs received.
    pub firs: u64,
    /// Number of plis received.
    pub plis: u64,
    /// Number of nacks received.
    pub nacks: u64,
    /// Round-trip-time extracted from the last RTCP receiver report.
    pub rtt: Option<Duration>,
    /// Fraction of packets lost averaged from the RTCP receiver reports received.
    /// `None` if no reports have been received since the last event
    pub loss: Option<f32>,
    /// Timestamp when this event was generated
    pub timestamp: Instant,
    /// Stats provided by the remote peer via ReceiverReports
    pub remote: Option<RemoteIngressStats>,
}

/// Stats as reported by the remote side (via RTCP ReceiverReports).
#[derive(Debug, Clone)]
pub struct RemoteIngressStats {
    /// The remotely calculated jitter.
    pub jitter: u32,
    /// The maximum extended sequence number received.
    pub maximum_sequence_number: SeqNo,
    /// The cumulative number of packets lost.
    pub packets_lost: u64,
}

/// Incoming media statistics in [`Event::MediaIngressStats`][crate::Event::MediaIngressStats].
///
/// note: when simulcast is disabled, `rid` is `None`
#[derive(Debug, Clone)]
pub struct MediaIngressStats {
    /// The identifier of the media these stats are for.
    pub mid: Mid,
    /// The Rid identifier in case of simulcast.
    pub rid: Option<Rid>,
    /// Total bytes received, including retransmissions.
    pub bytes: u64,
    /// Total number of rtp packets received, including retransmissions.
    pub packets: u64,
    /// Number of firs sent.
    pub firs: u64,
    /// Number of plis sent.
    pub plis: u64,
    /// Number of nacks sent.
    pub nacks: u64,
    /// Interarrival jitter, in RTP timestamp units, as reported in the last RTCP
    /// receiver report we sent for this stream.
    pub jitter: u32,
    /// Round-trip-time extracted from the last RTCP XR DLRR report block.
    pub rtt: Option<Duration>,
    /// Fraction of packets lost extracted from the last RTCP receiver report.
    pub loss: Option<f32>,
    /// Timestamp when this event was generated.
    pub timestamp: Instant,
    /// Stats provided by the remote peer via SenderReports
    pub remote: Option<RemoteEgressStats>,
}

impl MediaIngressStats {
    /// Merge `other` into `self`, mutating `self`.
    ///
    /// **Panics** if called with stats that don't have the same `(mid, rid)` pair.
    pub(crate) fn merge_by_mid_rid(&mut self, other: &Self) {
        assert!(
            self.mid == other.mid,
            "Cannot merge MediaIngressStats for different mids"
        );
        assert!(
            self.rid == other.rid,
            "Cannot merge MediaIngressStats for different rids"
        );
        let (jitter, rtt, loss) = if self.timestamp > other.timestamp {
            (self.jitter, self.rtt, self.loss)
        } else {
            (other.jitter, other.rtt, other.loss)
        };

        *self = Self {
            mid: self.mid,
            rid: self.rid,
            bytes: self.bytes + other.bytes,
            packets: self.packets + other.packets,
            firs: self.firs + other.firs,
            plis: self.plis + other.plis,
            nacks: self.nacks + other.nacks,
            jitter,
            rtt,
            loss,
            timestamp: self.timestamp.max(other.timestamp),
            remote: match (&self.remote, &other.remote) {
                (None, None) => None,
                (Some(remote), None) => Some(remote.clone()),
                (None, Some(other_remote)) => Some(other_remote.clone()),
                (Some(remote), Some(other_remote)) => Some(RemoteEgressStats {
                    bytes: remote.bytes + other_remote.bytes,
                    packets: remote.packets + other_remote.packets,
                }),
            },
        };
    }
}

/// Stats as reported by the remote side (via RTCP SenderReports).
#[derive(Debug, Clone)]
pub struct RemoteEgressStats {
    /// Total bytes sent, including retransmissions.
    pub bytes: u64,
    /// Total number of rtp packets sent, including retransmissions.
    pub packets: u64,
}

impl Stats {
    /// Create a new stats instance
    ///
    /// The internal state is market with the current `Instant::now()`.
    /// This allows us to emit stats right away at the first upcoming timeout
    pub fn new(interval: Duration) -> Stats {
        Stats {
            // by starting with the current time we can generate stats right on first timeout
            last_now: None,
            events: VecDeque::new(),
            interval,
        }
    }

    /// Returns true if we want to handle the timeout
    ///
    /// The caller can use this to compute the snapshot only if needed, before calling \
    /// [`Stats::do_handle_timeout`]
    pub fn wants_timeout(&mut self, now: Instant) -> bool {
        let Some(last_now) = self.last_now else {
            // Learn our first ever `now`
            self.last_now = Some(now);
            return false;
        };

        let min_step = last_now + self.interval;
        now >= min_step
    }

    /// Actually handles the timeout advancing the internal state and preparing the output
    pub fn do_handle_timeout(&mut self, snapshot: &mut StatsSnapshot) {
        // enqueue stats and timestamp them so they can be sent out

        let event = PeerStats {
            peer_bytes_rx: snapshot.peer_rx,
            peer_bytes_tx: snapshot.peer_tx,
            bytes_rx: snapshot.rx,
            bytes_tx: snapshot.tx,
            timestamp: snapshot.timestamp,
            bwe_tx: snapshot.bwe_tx,
            egress_loss_fraction: snapshot.egress_loss_fraction,
            ingress_loss_fraction: snapshot.ingress_loss_fraction,
            rtt: snapshot.rtt,
            selected_candidate_pair: snapshot.selected_candidate_pair.clone(),
        };

        self.events.push_back(StatsEvent::Peer(event));

        for (_, event) in snapshot.ingress.drain() {
            self.events.push_back(StatsEvent::MediaIngress(event));
        }

        for (_, event) in snapshot.egress.drain() {
            self.events.push_back(StatsEvent::MediaEgress(event));
        }

        self.last_now = Some(snapshot.timestamp);
    }

    /// Poll for the next time to call [`Stats::wants_timeout`] and [`Stats::do_handle_timeout`].
    ///
    /// NOTE: we only need Option<_> to conform to .soonest() (see caller)
    pub fn poll_timeout(&self) -> Option<Instant> {
        let last_now = self.last_now?;
        Some(last_now + self.interval)
    }

    /// Return any events ready for delivery
    pub fn poll_output(&mut self) -> Option<StatsEvent> {
        self.events.pop_front()
    }
}
