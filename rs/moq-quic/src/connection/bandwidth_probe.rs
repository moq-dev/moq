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

use std::{ops::Bound, time::Duration};

use super::{Connection, SentPacket, StreamsState, sent_packets::SentPackets};
use crate::{
	Instant,
	frame::{self, FrameStruct},
	packet::SpaceId,
	transport,
};

/// The probe's state on a connection
pub(super) struct BandwidthProbe {
	/// The rate to probe up to, in bytes per second; zero turns probing off
	target: u64,
	/// Holds everything sent while probing to `target`, which the controller's probing gain
	/// alone may reach past
	pacing: Bucket,
	/// The packet number to resume taking in-flight data from, so the duplicates spread over the
	/// whole flight, oldest first
	cursor: u64,
	/// Every packet below this holds nothing left to resend
	///
	/// A stream's reset, its stop, and an acknowledgment are all final, so a packet found with
	/// nothing to resend never has any again, and each packet is searched past once.
	floor: u64,
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
	Send,
}

impl BandwidthProbe {
	pub(super) fn new(config: &transport::Config, now: Instant) -> Self {
		let mtu = u64::from(config.get_initial_mtu());
		Self {
			target: 0,
			// One packet of credit, resized to the target on first use
			pacing: Bucket {
				capacity: mtu,
				tokens: mtu,
				prev: now,
			},
			cursor: 0,
			floor: 0,
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
		for (pn, packet) in sources(packets, self.floor, self.cursor) {
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
	/// Whether a packet of `bytes` resending in-flight data may go now
	pub(super) fn bandwidth_probe_ready(&mut self, now: Instant, bytes: u64) -> Ready {
		let target = self.bandwidth_probe.target;
		if target == 0
			|| !self.state.is_established()
			|| self.spaces[SpaceId::Data].crypto.is_none()
			|| self.path.challenge.is_some()
		{
			return Ready::Idle;
		}
		let metrics = self.path.congestion.metrics();
		let Some(inflight) = metrics.probe_inflight else {
			return Ready::Idle;
		};
		if metrics.bandwidth_estimate.is_some_and(|estimate| estimate >= target) {
			return Ready::Idle;
		}
		if !self.find_bandwidth_probe_source() {
			return Ready::Idle;
		}
		if self.path.in_flight.bytes + bytes > inflight {
			return Ready::Blocked(None);
		}
		let mtu = self.path.current_mtu();
		match self.bandwidth_probe.pacing.delay(target, bytes, mtu, now) {
			Some(at) => Ready::Blocked(Some(at)),
			None => Ready::Send,
		}
	}

	/// Move the cursor to the next in-flight packet holding STREAM data to resend, returning
	/// whether there is one
	fn find_bandwidth_probe_source(&mut self) -> bool {
		let space = &self.spaces[SpaceId::Data];
		let probe = &mut self.bandwidth_probe;
		let found = sources(&space.sent_packets, probe.floor, probe.cursor).find(|(_, packet)| {
			packet
				.stream_frames
				.iter()
				.any(|frame| self.streams.can_duplicate(frame))
		});
		match found {
			Some((pn, _)) => probe.cursor = pn,
			None => {
				// Not past the next packet number: a packet being built has one but isn't
				// tracked yet
				if let Some(last) = space.sent_packets.last() {
					probe.floor = last + 1;
				}
				probe.cursor = probe.floor;
			}
		}
		found.is_some()
	}

	/// When each probe packet still in flight was sent, and the streams it resends data of
	#[cfg(test)]
	pub(crate) fn bandwidth_probes_in_flight(&self) -> impl Iterator<Item = (Instant, crate::StreamId)> + '_ {
		self.spaces[SpaceId::Data]
			.sent_packets
			.range(..)
			.filter(|(_, packet)| packet.bandwidth_probe)
			.flat_map(|(_, packet)| packet.stream_frames.iter().map(|frame| (packet.time_sent, frame.id)))
	}
}

/// A token bucket refilled at the probe's target
///
/// Mirrors the rate path of `moq-congestion`'s pacer, which a controller owns and so cannot pace
/// to a target of the transport's.
struct Bucket {
	/// The most credit held, sized to the rate so bursts stay short
	capacity: u64,
	/// Bytes that may be sent now
	tokens: u64,
	/// When `tokens` was last refilled
	prev: Instant,
}

impl Bucket {
	fn on_transmit(&mut self, size: u16) {
		self.tokens = self.tokens.saturating_sub(size.into());
	}

	/// When `bytes` may be sent at `rate` bytes per second: `None` for now
	fn delay(&mut self, rate: u64, bytes: u64, mtu: u16, now: Instant) -> Option<Instant> {
		let rate = rate.max(1);
		let capacity = burst_capacity(rate, mtu);
		if capacity != self.capacity {
			self.capacity = capacity;
			self.tokens = self.tokens.min(capacity);
		}

		let elapsed = now.saturating_duration_since(self.prev);
		let earned = u64::try_from(u128::from(rate) * elapsed.as_nanos() / 1_000_000_000).unwrap_or(u64::MAX);
		// Time too short to earn a whole byte carries over, so a slow rate polled often still
		// accumulates credit
		if earned > 0 {
			self.tokens = self.tokens.saturating_add(earned).min(self.capacity);
			self.prev = now;
		}

		// A packet larger than the whole bucket still leaves once the bucket is full
		let needed = Ord::min(bytes, self.capacity);
		if self.tokens >= needed {
			return None;
		}
		// Rounded up, so the wait earns the whole shortfall rather than re-arming at `now`
		let nanos = (u128::from(needed - self.tokens) * 1_000_000_000).div_ceil(u128::from(rate));
		Some(now + Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX)))
	}
}

/// The credit a bucket refilled at `rate` holds: 2ms of it, kept between 10 and 256 packets
/// while that is no more than 10ms of it, and never under one packet
fn burst_capacity(rate: u64, mtu: u16) -> u64 {
	let mtu = u64::from(mtu);
	let bytes_in = |ms: u128| (u128::from(rate) * ms / 1000) as u64;
	Ord::min(Ord::max(bytes_in(10), mtu), bytes_in(2).clamp(10 * mtu, 256 * mtu))
}

/// In-flight packets from `floor` on holding STREAM data sent once, from `cursor` on and then
/// from the oldest
fn sources(packets: &SentPackets, floor: u64, cursor: u64) -> impl Iterator<Item = (u64, &SentPacket)> {
	packets
		.range((Bound::Included(cursor), Bound::Unbounded))
		.chain(packets.range((Bound::Included(floor), Bound::Excluded(cursor))))
		.filter(|(_, packet)| !packet.bandwidth_probe && packet.size != 0 && !packet.stream_frames.is_empty())
}
