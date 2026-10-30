//! Receive timestamps, as draft-ietf-quic-receive-ts-03 reports them

use std::collections::VecDeque;

use bytes::BufMut;

use crate::{Duration, Instant, frame, range_set::ArrayRangeSet};

/// When received packets arrived, kept until an ACK reports them to a peer that asked
///
/// Each arrival is reported once. An ACK-only packet is never acknowledged, so repeating
/// arrivals until the peer confirms them would resend every one in every ACK, and the sender
/// only reads the times of packets an ACK newly acknowledges. Losing an ACK loses its times,
/// which the draft leaves to best effort.
pub(super) struct Log {
	/// Packet numbers and arrivals not yet reported, oldest first, arrivals never decreasing
	received: VecDeque<(u64, Instant)>,
	/// The latest arrival recorded, which no later one goes before
	latest: Option<Instant>,
	/// What every reported arrival counts from, set just before the first
	basis: Option<Instant>,
	/// The most timestamps the peer takes in one ACK
	max: usize,
	/// Each reported unit is 2^exponent microseconds
	exponent: u32,
}

impl Log {
	/// A log for a peer that takes `max` timestamps per ACK, in units of 2^`exponent` µs
	pub(super) fn new(max: u64, exponent: u64) -> Self {
		Self {
			received: VecDeque::new(),
			latest: None,
			basis: None,
			max: usize::try_from(max).unwrap_or(usize::MAX),
			exponent: exponent as u32,
		}
	}

	/// Records that `packet` arrived at `at`, keeping at most `limit` unreported arrivals
	///
	/// An arrival before the previous one, as from a clock that stepped back, counts as the
	/// previous one, since reported arrivals never go back in time.
	pub(super) fn received(&mut self, packet: u64, at: Instant, limit: usize) {
		let at = self.latest.map_or(at, |latest| latest.max(at));
		self.latest = Some(at);
		// The basis is strictly before every reported arrival
		self.basis.get_or_insert_with(|| at.checked_sub(Duration::from_micros(1)).unwrap_or(at));
		self.received.push_back((packet, at));
		while self.received.len() > limit.min(self.max) {
			self.received.pop_front();
		}
	}

	/// Whether an ACK of `ranges` has any arrival to report
	pub(super) fn any(&self, ranges: &ArrayRangeSet) -> bool {
		self.received.iter().any(|&(packet, _)| ranges.contains(packet))
	}

	/// Writes the Receive Timestamps field for an ACK of `ranges` in at most `space` bytes,
	/// newest arrival first, and forgets the arrivals it reported
	pub(super) fn encode<W: BufMut>(&mut self, ranges: &ArrayRangeSet, space: usize, buf: &mut W) {
		let largest = ranges.max().expect("an ACK acknowledges a packet");
		let basis = self.basis.expect("an arrival sets the basis");
		let received = self
			.received
			.iter()
			.rev()
			.filter(|&&(packet, _)| ranges.contains(packet))
			.take(self.max)
			.map(|&(packet, at)| (packet, ((at - basis).as_micros() as u64) >> self.exponent));
		let mut reported = frame::ReceiveTimestamps::encode(largest, received, space, buf);

		// The reported arrivals are the newest the ACK covers
		let mut i = self.received.len();
		while reported > 0 {
			i -= 1;
			if ranges.contains(self.received[i].0) {
				self.received.remove(i);
				reported -= 1;
			}
		}
	}
}
