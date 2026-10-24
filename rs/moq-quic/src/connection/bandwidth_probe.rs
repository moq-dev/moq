//! Resending in-flight STREAM data so an application-limited sender can probe for bandwidth.
//!
//! Congestion controllers stop learning once the application sends less than the path carries:
//! BBR3 only raises its estimate from an application-limited sample above the current one. So a
//! session that wants more than it sends, to resume a stopped rendition, would never find out
//! the path has room. While the controller probes, this fills the pacing headroom it opens with
//! duplicates of STREAM data already in flight, up to the session's target, so the probe's
//! samples measure the path. A duplicate needs no wire change, and repairs a loss of its
//! original. It is never retransmitted, and a reset stream's data is never sent again, so a
//! probe never sends past a group's expiry.

use std::ops::Bound;

use super::{Connection, SentPacket, StreamsState, pacing::Pacer, sent_packets::SentPackets};
use crate::{
    Instant, TransportConfig,
    congestion::ControllerMetrics,
    frame::{self, FrameStruct},
    packet::SpaceId,
};

/// The probe's state on a connection
pub(super) struct BandwidthProbe {
    /// The rate to probe up to, in bytes per second; zero turns probing off
    target: u64,
    /// Holds everything sent while probing to `target`, which the controller's probing gain
    /// alone may reach past
    pacing: Pacer,
    /// The packet number to resume taking in-flight data from, so the duplicates spread over the
    /// whole flight, oldest first
    cursor: u64,
    /// The next packet number when a search last found nothing in flight to resend
    ///
    /// Only a packet sent since can hold data to resend, so the search is not repeated until then.
    dry: Option<u64>,
}

/// Whether a probe packet may be sent now
pub(super) enum Ready {
    /// Not probing, or nothing in flight to resend: the application is all that limits the
    /// sender
    Idle,
    /// Held back by the target or by the inflight the controller allows a probe, which limit
    /// the sender as pacing does; a held-back pace ends at the instant given
    Blocked(Option<Instant>),
    /// Send one
    Ready,
}

impl BandwidthProbe {
    pub(super) fn new(config: &TransportConfig, now: Instant) -> Self {
        let mtu = config.get_initial_mtu();
        Self {
            target: 0,
            // Paced at an explicit rate, which resizes the bucket on first use
            pacing: Pacer::new(config.initial_rtt, mtu.into(), mtu, None, now),
            cursor: 0,
            dry: None,
        }
    }

    pub(super) fn set_target(&mut self, bytes_per_second: u64) {
        self.target = bytes_per_second;
    }

    /// Count a packet of `size` bytes in flight against the target
    pub(super) fn on_transmit(&mut self, size: u16) {
        self.pacing.on_transmit(size);
    }

    /// Fill the packet in `buf` up to `max_size` with STREAM frames resending the data in flight
    /// in `packets`, taking whole packets' frames from the cursor on and wrapping around once
    pub(super) fn write(
        &mut self,
        packets: &SentPackets,
        streams: &StreamsState,
        buf: &mut Vec<u8>,
        max_size: usize,
    ) -> frame::StreamMetaVec {
        let mut written = frame::StreamMetaVec::new();
        for (pn, packet) in sources(packets, self.cursor) {
            if buf.len() + frame::Stream::SIZE_BOUND >= max_size {
                break;
            }
            streams.write_duplicates(buf, max_size, &packet.stream_frames, &mut written);
            self.cursor = pn + 1;
        }
        written
    }
}

impl Connection {
    /// Whether a packet of `bytes` resending in-flight data may go now, given the controller's
    /// `metrics` snapshot
    pub(super) fn bandwidth_probe_ready(
        &mut self,
        now: Instant,
        metrics: &ControllerMetrics,
        bytes: u64,
    ) -> Ready {
        let target = self.bandwidth_probe.target;
        if target == 0
            || !self.state.is_established()
            || self.spaces[SpaceId::Data].crypto.is_none()
            || self.path.challenge.is_some()
        {
            return Ready::Idle;
        }
        let Some(inflight) = metrics.probe_inflight else {
            return Ready::Idle;
        };
        if metrics
            .bandwidth_estimate
            .is_some_and(|estimate| estimate >= target)
        {
            return Ready::Idle;
        }
        if !self.find_bandwidth_probe_source() {
            return Ready::Idle;
        }
        if self.path.in_flight.bytes + bytes > inflight {
            return Ready::Blocked(None);
        }
        let mtu = self.path.current_mtu();
        match self
            .bandwidth_probe
            .pacing
            .delay_at_rate(target, bytes, mtu, now)
        {
            Some(at) => Ready::Blocked(Some(at)),
            None => Ready::Ready,
        }
    }

    /// Move the cursor to the next in-flight packet holding STREAM data to resend, returning
    /// whether there is one
    fn find_bandwidth_probe_source(&mut self) -> bool {
        let space = &self.spaces[SpaceId::Data];
        let next = space.next_packet_number;
        let probe = &mut self.bandwidth_probe;
        if probe.dry == Some(next) {
            return false;
        }
        let found = sources(&space.sent_packets, probe.cursor).find(|(_, packet)| {
            packet
                .stream_frames
                .iter()
                .any(|frame| self.streams.can_duplicate(frame))
        });
        match found {
            Some((pn, _)) => {
                probe.cursor = pn;
                probe.dry = None;
                true
            }
            None => {
                probe.dry = Some(next);
                false
            }
        }
    }

    /// When each probe packet still in flight was sent, and the streams it resends data of
    #[cfg(test)]
    pub(crate) fn bandwidth_probes_in_flight(
        &self,
    ) -> impl Iterator<Item = (Instant, crate::StreamId)> + '_ {
        self.spaces[SpaceId::Data]
            .sent_packets
            .range(..)
            .filter(|(_, packet)| packet.bandwidth_probe)
            .flat_map(|(_, packet)| {
                packet
                    .stream_frames
                    .iter()
                    .map(|frame| (packet.time_sent, frame.id))
            })
    }
}

/// In-flight packets holding STREAM data sent once, from `cursor` on and then from the oldest
fn sources(packets: &SentPackets, cursor: u64) -> impl Iterator<Item = (u64, &SentPacket)> {
    packets
        .range((Bound::Included(cursor), Bound::Unbounded))
        .chain(packets.range((Bound::Unbounded, Bound::Excluded(cursor))))
        .filter(|(_, packet)| {
            !packet.bandwidth_probe && packet.size != 0 && !packet.stream_frames.is_empty()
        })
}
