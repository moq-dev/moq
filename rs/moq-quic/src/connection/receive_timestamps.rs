//! Receive timestamps, as draft-ietf-quic-receive-ts-03 reports them

use std::collections::VecDeque;

use bytes::BufMut;

use crate::{Duration, Instant, frame, range_set::ArrayRangeSet};

/// When received packets arrived, kept for a peer that asked to hear them in its ACKs
pub(super) struct Log {
	/// Packet numbers and arrivals, oldest arrival first, arrivals never decreasing
	received: VecDeque<(u64, Instant)>,
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
			basis: None,
			max: usize::try_from(max).unwrap_or(usize::MAX),
			exponent: exponent as u32,
		}
	}

	/// Records that `packet` arrived at `at`, keeping at most `limit` arrivals
	///
	/// An arrival before the previous one, as from a clock that stepped back, counts as the
	/// previous one, since reported arrivals never go back in time.
	pub(super) fn received(&mut self, packet: u64, at: Instant, limit: usize) {
		let at = self.received.back().map_or(at, |&(_, last)| last.max(at));
		// The basis is strictly before every reported arrival
		self.basis.get_or_insert_with(|| at.checked_sub(Duration::from_micros(1)).unwrap_or(at));
		self.received.push_back((packet, at));
		while self.received.len() > limit.min(self.max) {
			self.received.pop_front();
		}
	}

	/// Forgets packets numbered at or below `max`, as pending ACK ranges do once the peer
	/// acknowledged an ACK covering them
	pub(super) fn subtract_below(&mut self, max: u64) {
		self.received.retain(|&(packet, _)| packet > max);
	}

	/// Whether an ACK of `ranges` has any arrival to report
	pub(super) fn any(&self, ranges: &ArrayRangeSet) -> bool {
		self.received.iter().any(|&(packet, _)| ranges.contains(packet))
	}

	/// Writes the Receive Timestamps field for an ACK of `ranges` in at most `space` bytes,
	/// newest arrival first, returning how many it reported
	pub(super) fn encode<W: BufMut>(&self, ranges: &ArrayRangeSet, space: usize, buf: &mut W) -> usize {
		let largest = ranges.max().expect("an ACK acknowledges a packet");
		let basis = self.basis.expect("an arrival sets the basis");
		let received = self
			.received
			.iter()
			.rev()
			.filter(|&&(packet, _)| ranges.contains(packet))
			.take(self.max)
			.map(|&(packet, at)| (packet, ((at - basis).as_micros() as u64) >> self.exponent));
		frame::ReceiveTimestamps::encode(largest, received, space, buf)
	}
}
