use std::collections::VecDeque;
use std::ops::RangeBounds;

use super::slots::Slots;
use super::spaces::SentPacket;

/// ACK-only packets kept before the oldest is forgotten.
///
/// RFC 9000 lets us forget them: they carry nothing to retransmit, and the peer need never ACK
/// them. Forgetting one costs the RTT sample when the peer's largest ACK names it, so this covers
/// our ACK rate over the RTT plus the peer's ACK delay: at one ACK per two 1200-byte packets and
/// 25 ms of delay, about 40 Mbps of receive at 100 ms RTT. Its gap also reads as an acknowledged
/// packet to persistent congestion detection, which can then go undeclared, as a skipped packet
/// number's gap already can.
const MAX_ACK_ONLY: usize = 256;

/// The packets sent in one space that are not yet acknowledged or declared lost.
///
/// Ack-eliciting packets are bounded by the congestion window, plus loss probes that back off.
/// ACK-only packets skip the congestion window, so the oldest is forgotten past [`MAX_ACK_ONLY`].
#[derive(Default)]
pub(super) struct SentPackets {
	packets: Slots<SentPacket>,
	/// Present entries with `size != 0`, for O(1) `has_in_flight`.
	in_flight: usize,
	/// The last [`MAX_ACK_ONLY`] ACK-only packet numbers sent, oldest first, including any
	/// already acknowledged or lost.
	ack_only: VecDeque<u64>,
}

impl SentPackets {
	/// Track `packet` at `pn`, which must exceed every previously inserted packet number.
	///
	/// Returns the ACK-only packet forgotten to make room, if any.
	pub(super) fn insert(&mut self, pn: u64, packet: SentPacket) -> Option<SentPacket> {
		let mut forgotten = None;
		if !packet.ack_eliciting {
			if self.ack_only.len() == MAX_ACK_ONLY {
				let oldest = self.ack_only.pop_front().expect("full");
				forgotten = self.remove(oldest);
			}
			self.ack_only.push_back(pn);
		}
		self.in_flight += usize::from(packet.size != 0);
		self.packets.insert(pn, packet);
		forgotten
	}

	/// Remove and return the entry for `pn`.
	pub(super) fn remove(&mut self, pn: u64) -> Option<SentPacket> {
		let packet = self.packets.remove(pn)?;
		self.in_flight -= usize::from(packet.size != 0);
		Some(packet)
	}

	/// Return the entry for `pn`.
	pub(super) fn get(&self, pn: u64) -> Option<&SentPacket> {
		self.packets.get(pn)
	}

	/// The highest packet number present.
	pub(super) fn last(&self) -> Option<u64> {
		self.packets.last()
	}

	/// Whether any present entry has `size != 0`.
	pub(super) fn has_in_flight(&self) -> bool {
		self.in_flight != 0
	}

	/// Iterate present entries in `range`, in increasing packet-number order.
	pub(super) fn range(&self, range: impl RangeBounds<u64>) -> impl Iterator<Item = (u64, &SentPacket)> + '_ {
		self.packets.range(range)
	}

	/// Mutably iterate present entries in increasing packet-number order.
	pub(super) fn values_mut(&mut self) -> impl Iterator<Item = &mut SentPacket> + '_ {
		self.packets.values_mut()
	}

	/// Consume the map, yielding present entries in increasing packet-number order.
	pub(super) fn into_values(self) -> impl Iterator<Item = SentPacket> {
		self.packets.into_values()
	}
}

/// `SentPackets` over packets of only a size and an ACK-eliciting flag, for the benches.
#[cfg(feature = "fuzz")]
pub struct Bench {
	packets: SentPackets,
	time_sent: crate::Instant,
}

#[cfg(feature = "fuzz")]
impl Default for Bench {
	fn default() -> Self {
		Self {
			packets: SentPackets::default(),
			time_sent: crate::Instant::now(),
		}
	}
}

#[cfg(feature = "fuzz")]
impl Bench {
	/// Track `pn`, returning whether an ACK-only packet was forgotten to make room.
	#[inline]
	pub fn insert(&mut self, pn: u64, size: u16, ack_eliciting: bool) -> bool {
		let packet = SentPacket {
			path_generation: 0,
			time_sent: self.time_sent,
			size,
			ack_eliciting,
			congestion: None,
			largest_acked: None,
			retransmits: Default::default(),
			stream_frames: Default::default(),
			bandwidth_probe: false,
		};
		self.packets.insert(pn, packet).is_some()
	}

	/// Stop tracking `pn`, returning whether it was tracked.
	#[inline]
	pub fn remove(&mut self, pn: u64) -> bool {
		self.packets.remove(pn).is_some()
	}

	/// The size of `pn`, if tracked.
	#[inline]
	pub fn get(&self, pn: u64) -> Option<u16> {
		self.packets.get(pn).map(|packet| packet.size)
	}

	/// Tracked packet numbers and sizes in `range`, in order.
	#[inline]
	pub fn range(&self, range: impl RangeBounds<u64>) -> impl Iterator<Item = (u64, u16)> + '_ {
		self.packets.range(range).map(|(pn, packet)| (pn, packet.size))
	}

	/// Whether a tracked packet counts toward congestion control.
	#[inline]
	pub fn has_in_flight(&self) -> bool {
		self.packets.has_in_flight()
	}

	/// Drop every packet, returning how many were tracked.
	#[inline]
	pub fn drain(self) -> usize {
		self.packets.into_values().map(std::hint::black_box).count()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Instant;
	use std::ops::Bound;

	/// A peer that never ACKs leaves the packets tracked bounded by the congestion window, loss
	/// probes, and [`MAX_ACK_ONLY`], and storage bounded by the packets tracked, while ACK-only
	/// packets, padded or not, keep going out.
	#[test]
	fn silent_peer_is_bounded() {
		const WINDOW: usize = 32;
		const PROBES: usize = 16;
		let mut m = SentPackets::default();
		let mut pn = 0;
		let mut in_flight = WINDOW + PROBES;
		for _ in 0..WINDOW {
			m.insert(pn, eliciting(1200));
			pn += 1;
		}
		// Each loss probe waits longer, so more ACK-only packets go out before the next.
		for probe in 0..PROBES {
			for i in 0..100 << probe.min(6) {
				let size = if i % 2 == 0 { 1200 } else { 0 };
				in_flight += usize::from(size != 0);
				if let Some(forgotten) = m.insert(pn, packet(size)) {
					assert!(!forgotten.ack_eliciting);
					in_flight -= usize::from(forgotten.size != 0);
				}
				pn += 1;
			}
			m.insert(pn, eliciting(1200));
			pn += 1;
		}

		let live = m.range(..).count();
		assert_eq!(live, WINDOW + PROBES + MAX_ACK_ONLY);
		assert_eq!(m.range(..).filter(|(_, p)| p.size != 0).count(), in_flight);
		// An emptied chunk is freed, so each chunk holds at least one packet.
		assert!(m.packets.slots() <= live * super::super::slots::CHUNK);
	}

	/// The oldest ACK-only packet is forgotten wherever it sits, and one already acknowledged
	/// leaves nothing to forget.
	#[test]
	fn forgets_oldest_ack_only() {
		let mut m = SentPackets::default();
		m.insert(0, packet(1200));
		m.insert(1, eliciting(1200));
		for pn in 2..=MAX_ACK_ONLY as u64 {
			assert!(m.insert(pn, packet(0)).is_none());
		}
		// The padded ACK-only packet before the ack-eliciting one goes first.
		let forgotten = m.insert(MAX_ACK_ONLY as u64 + 1, packet(0)).unwrap();
		assert_eq!(forgotten.size, 1200);
		assert!(m.get(0).is_none());
		assert!(m.get(1).is_some());

		// Acknowledged, so the next ACK-only packet takes its place.
		m.remove(2);
		assert!(m.insert(MAX_ACK_ONLY as u64 + 2, packet(0)).is_none());
		assert_eq!(m.range(..).filter(|(_, p)| !p.ack_eliciting).count(), MAX_ACK_ONLY);
		assert!(m.get(3).is_some());
		assert!(m.has_in_flight());
		m.remove(1);
		assert!(!m.has_in_flight());
	}

	#[test]
	fn insert_get_and_order() {
		let mut m = SentPackets::default();
		for pn in 3..=6 {
			m.insert(pn, eliciting(pn as u16 * 10));
		}
		assert_eq!(m.get(3).map(|p| p.size), Some(30));
		assert_eq!(m.get(6).map(|p| p.size), Some(60));
		assert!(m.get(2).is_none());
		assert!(m.get(7).is_none());
		assert_eq!(m.get(5).map(|p| p.size), Some(50));
		assert_eq!(m.last(), Some(6));
		assert_eq!(range_of(&m, ..), vec![(3, 30), (4, 40), (5, 50), (6, 60)]);
	}

	#[test]
	fn range_bounds_and_gaps() {
		let mut m = SentPackets::default();
		for pn in (10..300).filter(|pn| pn % 7 != 0) {
			m.insert(pn, eliciting(pn as u16));
		}
		let expect = |r: std::ops::Range<u64>| r.filter(|pn| pn % 7 != 0).map(|pn| (pn, pn as u16)).collect::<Vec<_>>();
		assert_eq!(range_of(&m, ..), expect(10..300));
		assert_eq!(range_of(&m, 12..15), expect(12..15));
		assert_eq!(range_of(&m, 70..=140), expect(70..141));
		assert_eq!(range_of(&m, (Bound::Excluded(200), Bound::Unbounded)), expect(201..300));
		// Out-of-window ranges are empty, not a panic.
		assert_eq!(range_of(&m, 0..5), vec![]);
		assert_eq!(range_of(&m, 400..500), vec![]);
		assert_eq!(m.last(), Some(299));
	}

	#[test]
	fn values_mut_and_into_values() {
		let mut m = SentPackets::default();
		for pn in 0..4 {
			m.insert(pn, eliciting(pn as u16));
		}
		m.remove(1);
		for v in m.values_mut() {
			v.size += 100;
		}
		assert_eq!(m.get(0).map(|p| p.size), Some(100));
		assert_eq!(m.get(2).map(|p| p.size), Some(102));
		assert_eq!(m.into_values().map(|p| p.size).collect::<Vec<_>>(), vec![100, 102, 103]);
	}

	#[test]
	fn tracks_in_flight() {
		let mut m = SentPackets::default();
		assert!(!m.has_in_flight());
		m.insert(0, packet(0)); // size 0: not in flight
		assert!(!m.has_in_flight());
		m.insert(1, eliciting(1200));
		m.insert(2, packet(1200)); // padded
		assert!(m.has_in_flight());
		m.remove(1);
		assert!(m.has_in_flight());
		m.remove(2);
		assert!(!m.has_in_flight());
	}

	/// An ACK-only `SentPacket` identified by its `size`.
	fn packet(size: u16) -> SentPacket {
		SentPacket {
			path_generation: 0,
			time_sent: Instant::now(),
			size,
			ack_eliciting: false,
			congestion: None,
			largest_acked: None,
			retransmits: Default::default(),
			stream_frames: Default::default(),
			bandwidth_probe: false,
		}
	}

	/// An ack-eliciting `SentPacket` identified by its `size`.
	fn eliciting(size: u16) -> SentPacket {
		SentPacket {
			ack_eliciting: true,
			..packet(size)
		}
	}

	/// `(pn, size)` pairs from `range`.
	fn range_of<R: RangeBounds<u64>>(m: &SentPackets, r: R) -> Vec<(u64, u16)> {
		m.range(r).map(|(pn, v)| (pn, v.size)).collect()
	}
}
