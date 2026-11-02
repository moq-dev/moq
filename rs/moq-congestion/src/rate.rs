//! Delivery-rate sampling, shared by the controllers that estimate bandwidth.
//!
//! Follows draft-ietf-ccwg-bbr section 4.1 and Linux `tcp_rate.c`: each sent packet snapshots the
//! connection's delivery counters, and each ACK turns its most recently sent packet's snapshot
//! into a rate sample.
//! <https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-4.1>

use kio::clock::Instant;
use std::time::Duration;

use crate::{Ack, Sent};

/// Controller state for one sent packet, which the transport stores and hands back.
///
/// Equivalent to the draft's per-packet `P` fields.
/// <https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-4.1.2.1.2>
#[derive(Debug, Clone, Copy)]
pub struct Packet {
    /// equivalent to P.delivered: C.delivered when the packet was sent.
    pub(crate) delivered: u64,
    /// equivalent to P.delivered_time: C.delivered_time when the packet was sent.
    pub(crate) delivered_time: Instant,
    /// equivalent to P.first_send_time: C.first_send_time when the packet was sent.
    pub(crate) first_send_time: Instant,
    /// equivalent to P.lost: C.lost when the packet was sent.
    pub(crate) lost: u64,
    /// equivalent to P.tx_in_flight: C.inflight immediately after the packet was sent, saturating.
    /// As in s2n-quic, 32 bits keep the transport's per-packet storage small, and no path holds
    /// 4 GiB in flight.
    pub(crate) tx_in_flight: u32,
    /// equivalent to P.is_app_limited: whether C.app_limited was set when the packet was sent.
    pub(crate) is_app_limited: bool,
}

/// An empty state, for a controller that keeps none.
impl Default for Packet {
    fn default() -> Self {
        Self {
            delivered: 0,
            delivered_time: Instant::ZERO,
            first_send_time: Instant::ZERO,
            lost: 0,
            tx_in_flight: 0,
            is_app_limited: false,
        }
    }
}

/// A rate sample from one ACK, equivalent to the draft's `RS`.
/// <https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-2.3>
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sample {
    /// equivalent to RS.delivery_rate: bytes per second, or zero if the interval was too short to
    /// trust.
    pub delivery_rate: f64,
    /// equivalent to RS.is_app_limited: P.is_app_limited of the most recently sent packet acked.
    pub is_app_limited: bool,
    /// equivalent to RS.delivered: bytes delivered over the interval.
    pub delivered: u64,
    /// equivalent to RS.prior_delivered: P.delivered of the most recently sent packet acked.
    pub prior_delivered: u64,
    /// equivalent to RS.rtt: the RTT of the most recently sent packet acked.
    pub rtt: Duration,
    /// equivalent to RS.tx_in_flight: C.inflight when the most recently sent packet acked was
    /// sent.
    pub tx_in_flight: u64,
    /// equivalent to RS.lost: bytes declared lost between that packet's send and its ACK.
    pub lost: u64,
}

impl Sample {
    /// A sample for a lost packet: no delivery to measure, only its own flight and labels.
    pub fn lost(packet: Packet, lost: u64) -> Self {
        Self {
            delivery_rate: 0.0,
            is_app_limited: packet.is_app_limited,
            delivered: 0,
            prior_delivered: packet.delivered,
            rtt: Duration::ZERO,
            tx_in_flight: packet.tx_in_flight.into(),
            lost,
        }
    }
}

/// The connection's delivery counters, equivalent to the draft's `C` rate-sampling fields.
#[derive(Debug, Clone, Default)]
pub(crate) struct Sampler {
    /// equivalent to C.delivered: bytes delivered over the connection's lifetime.
    pub delivered: u64,
    /// equivalent to C.delivered_time: when C.delivered was last updated.
    pub delivered_time: Option<Instant>,
    /// equivalent to C.first_send_time: the send time of the packet most recently marked
    /// delivered, or of the most recent send after an idle period.
    pub first_send_time: Option<Instant>,
    /// equivalent to C.app_limited: a byte index into the delivery stream marking the
    /// application-limited phase, or 0 if not application-limited.
    pub app_limited: u64,
    /// equivalent to C.lost: bytes lost over the connection's lifetime.
    pub lost: u64,
}

impl Sampler {
    /// equivalent to OnPacketSent: snapshots the counters for this packet.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-4.1.2.2>
    pub fn on_sent(&mut self, sent: Sent) -> Packet {
        if sent.in_flight <= u64::from(sent.bytes) {
            self.first_send_time = Some(sent.now);
            self.delivered_time = Some(sent.now);
        }
        Packet {
            delivered: self.delivered,
            delivered_time: self.delivered_time.unwrap_or(sent.now),
            first_send_time: self.first_send_time.unwrap_or(sent.now),
            lost: self.lost,
            tx_in_flight: sent.in_flight.try_into().unwrap_or(u32::MAX),
            is_app_limited: self.app_limited != 0,
        }
    }

    /// equivalent to UpdateRateSample and GenerateRateSample, for one ACK.
    ///
    /// Samples the most recently sent packet acked, if any. A sample whose interval is shorter
    /// than `min_rtt` (or this sample's own RTT, if lower) carries no rate, as the draft discards
    /// it, but still describes the ACK.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-4.1.2.3>
    pub fn on_ack(&mut self, ack: &Ack<'_>, min_rtt: Duration) -> Option<Sample> {
        for acked in ack.packets {
            self.delivered += u64::from(acked.bytes);
            self.delivered_time = Some(ack.now);
        }
        // The packets come in send order, and `max_by_key` keeps the last of equal keys, so a
        // tie on send time goes to the packet sent later.
        let newest = ack.packets.iter().max_by_key(|acked| acked.sent);

        if self.app_limited != 0 && self.delivered > self.app_limited {
            self.app_limited = 0;
        } else if ack.app_limited {
            self.mark_app_limited(ack.in_flight);
        }

        let newest = newest?;
        let p = newest.packet;
        self.first_send_time = Some(newest.sent);
        let send_elapsed = newest.sent.saturating_duration_since(p.first_send_time);
        let ack_elapsed = ack.now.saturating_duration_since(p.delivered_time);
        let rtt = ack.now.saturating_duration_since(newest.sent);
        let interval = Ord::max(send_elapsed, ack_elapsed);
        let delivered = self.delivered.saturating_sub(p.delivered);
        let mut delivery_rate = 0.0;
        if interval >= Ord::min(min_rtt, rtt) && interval != Duration::ZERO {
            delivery_rate = delivered as f64 / interval.as_secs_f64();
        }
        Some(Sample {
            delivery_rate,
            is_app_limited: p.is_app_limited,
            delivered,
            prior_delivered: p.delivered,
            rtt,
            tx_in_flight: p.tx_in_flight.into(),
            lost: self.lost_since(p),
        })
    }

    /// Counts `bytes` as lost.
    pub fn on_lost(&mut self, bytes: u16) {
        self.lost += u64::from(bytes);
    }

    /// Bytes lost since `packet` was sent.
    pub fn lost_since(&self, packet: Packet) -> u64 {
        self.lost.saturating_sub(packet.lost)
    }

    /// equivalent to marking C.app_limited: packets sent until `in_flight` more bytes are
    /// delivered are application-limited.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-4.1.2.4>
    pub fn mark_app_limited(&mut self, in_flight: u64) {
        self.app_limited = Ord::max(self.delivered + in_flight, 1);
    }

    /// Whether the connection is in an application-limited phase.
    pub fn is_app_limited(&self) -> bool {
        self.app_limited != 0
    }

    /// equivalent to C.delivered.
    pub fn delivered(&self) -> u64 {
        self.delivered
    }
}
