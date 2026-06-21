//! Multi-tier index: which segment, in which tier, holds each group, and which segments to promote.
//!
//! This is the storage-agnostic orchestration the disk and remote tiers run on top of the
//! [`segment`](super::segment) format. It records, per group sequence, a [`Location`] (tier +
//! segment + byte range), so a fetch is "look up the location, ranged-read that segment." It also
//! drives **promotion**: when the disk tier grows past its bound, [`Index::promotion`] picks the
//! oldest disk segments to compact, and after the caller rolls them into one remote object
//! ([`segment::rollup`](super::segment::rollup)) [`Index::apply_promotion`] repoints those
//! sequences at the remote tier and drops the disk segments.
//!
//! The index holds only metadata (offsets, sizes, timestamps), never group bytes, so it is the
//! piece that stays in memory while the bytes live on disk or in remote storage. The actual I/O
//! (object_store `put` / `get_range` / `delete`) is a thin layer that calls these methods for its
//! decisions; nothing here blocks or allocates per byte.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use super::segment::Segment;
use super::{Bounds, Limit};

/// Identifier for a stored segment, assigned in creation order (so a lower id is older).
pub type SegmentId = u64;

/// Which durable tier a segment lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
	/// Local disk: the staging tier that batches flushed RAM bands.
	Disk,
	/// Remote object storage: the long-term tier disk segments roll up into.
	Remote,
}

/// Where a group's bytes live: which tier and segment, and the byte range within that segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
	/// The tier holding the segment.
	pub tier: Tier,
	/// The segment within that tier.
	pub segment: SegmentId,
	/// Byte offset of the group blob within the segment object.
	pub offset: u64,
	/// Byte length of the group blob.
	pub length: u64,
}

/// Per-segment bookkeeping for tier accounting and promotion.
struct Meta {
	tier: Tier,
	bytes: u64,
	/// Timestamp extent, as durations (a common scale), so cross-timescale segments compare.
	ts_min: Option<Duration>,
	ts_max: Option<Duration>,
}

/// A map from group sequence to its [`Location`], plus per-segment metadata for promotion.
#[derive(Default)]
pub struct Index {
	groups: BTreeMap<u64, Location>,
	segments: BTreeMap<SegmentId, Meta>,
	next_id: SegmentId,
}

impl Index {
	/// An empty index.
	pub fn new() -> Self {
		Self::default()
	}

	/// Record a freshly written `segment` on `tier`, returning its new id. Each group in the
	/// segment becomes locatable; an already-present sequence is repointed to this segment (this
	/// is how [`apply_promotion`](Self::apply_promotion) moves sequences to the remote tier).
	pub fn add(&mut self, tier: Tier, segment: &Segment) -> SegmentId {
		let id = self.next_id;
		self.next_id += 1;

		let mut ts_min: Option<Duration> = None;
		let mut ts_max: Option<Duration> = None;

		for entry in segment.entries() {
			self.groups.insert(
				entry.sequence,
				Location {
					tier,
					segment: id,
					offset: entry.offset,
					length: entry.length,
				},
			);
			if let Some(t) = entry.ts_first {
				let d = Duration::from(t);
				ts_min = Some(ts_min.map_or(d, |m| m.min(d)));
			}
			if let Some(t) = entry.ts_last {
				let d = Duration::from(t);
				ts_max = Some(ts_max.map_or(d, |m| m.max(d)));
			}
		}

		self.segments.insert(
			id,
			Meta {
				tier,
				bytes: segment.byte_len() as u64,
				ts_min,
				ts_max,
			},
		);
		id
	}

	/// Where the group with this sequence lives, or `None` if it is not in any tier.
	pub fn locate(&self, sequence: u64) -> Option<Location> {
		self.groups.get(&sequence).copied()
	}

	/// Total bytes stored in `tier`.
	pub fn bytes(&self, tier: Tier) -> u64 {
		self.segments.values().filter(|m| m.tier == tier).map(|m| m.bytes).sum()
	}

	/// Number of segments in `tier`.
	pub fn segment_count(&self, tier: Tier) -> usize {
		self.segments.values().filter(|m| m.tier == tier).count()
	}

	/// Segment ids in `tier`, oldest first.
	fn tier_segments(&self, tier: Tier) -> Vec<SegmentId> {
		// BTreeMap iterates by id, which is creation order, i.e. oldest first.
		self.segments
			.iter()
			.filter(|(_, m)| m.tier == tier)
			.map(|(id, _)| *id)
			.collect()
	}

	/// Total bytes and timestamp span across a set of segments.
	fn stats(&self, ids: &[SegmentId]) -> (u64, Duration) {
		let mut bytes = 0;
		let mut lo: Option<Duration> = None;
		let mut hi: Option<Duration> = None;
		for id in ids {
			let Some(m) = self.segments.get(id) else { continue };
			bytes += m.bytes;
			if let Some(d) = m.ts_min {
				lo = Some(lo.map_or(d, |x| x.min(d)));
			}
			if let Some(d) = m.ts_max {
				hi = Some(hi.map_or(d, |x| x.max(d)));
			}
		}
		let span = match (lo, hi) {
			(Some(a), Some(b)) => b.saturating_sub(a),
			_ => Duration::ZERO,
		};
		(bytes, span)
	}

	/// Whether `(bytes, span)` trips a high watermark. An all-unset limit is unbounded.
	fn over_max(stats: (u64, Duration), max: Limit) -> bool {
		!max.is_unset() && Self::over(stats, max)
	}

	/// Whether `(bytes, span)` is still above a low watermark. An all-unset limit is a floor of
	/// zero, so any non-empty content is above it.
	fn above_min(stats: (u64, Duration), min: Limit) -> bool {
		if min.is_unset() {
			return stats.0 > 0;
		}
		Self::over(stats, min)
	}

	fn over((bytes, span): (u64, Duration), limit: Limit) -> bool {
		limit.bytes.is_some_and(|b| bytes > b) || limit.duration.is_some_and(|d| span > d)
	}

	/// The oldest disk segments to promote so the disk tier returns within `bounds`. Empty unless
	/// the disk tier is over its high watermark; otherwise the oldest segments are selected until
	/// what remains is within the low watermark, oldest first (the order to roll them up in).
	pub fn promotion(&self, bounds: Bounds) -> Vec<SegmentId> {
		let disk = self.tier_segments(Tier::Disk);
		if !Self::over_max(self.stats(&disk), bounds.max) {
			return Vec::new();
		}

		let mut promote = Vec::new();
		let mut remaining = disk;
		while !remaining.is_empty() && Self::above_min(self.stats(&remaining), bounds.min) {
			// Promote the oldest; recompute against what is left.
			promote.push(remaining.remove(0));
		}
		promote
	}

	/// Register `remote` (the rollup of `promoted`) on the remote tier, repoint its sequences, and
	/// drop the promoted disk segments. Returns the new remote segment id. `remote` must contain
	/// exactly the groups of `promoted`; any sequence missing from it is dropped from the index.
	pub fn apply_promotion(&mut self, promoted: &[SegmentId], remote: &Segment) -> SegmentId {
		let new_id = self.add(Tier::Remote, remote);

		let promoted: HashSet<SegmentId> = promoted.iter().copied().collect();
		// `add` already repointed every sequence in `remote` to `new_id`; anything still pointing
		// at a promoted segment was not in the rollup, so drop it.
		self.groups.retain(|_, loc| !promoted.contains(&loc.segment));
		self.segments.retain(|id, _| !promoted.contains(id));
		new_id
	}

	/// Drop a set of segments and the group locations pointing at them. Used to evict from the
	/// disk tier when there is no remote tier to promote into.
	pub fn evict(&mut self, segments: &[SegmentId]) {
		let drop: HashSet<SegmentId> = segments.iter().copied().collect();
		self.groups.retain(|_, loc| !drop.contains(&loc.segment));
		self.segments.retain(|id, _| !drop.contains(id));
	}
}

#[cfg(test)]
mod tests {
	use super::super::segment;
	use super::super::{Frame, Group};
	use super::*;
	use crate::Timestamp;
	use bytes::Bytes;
	use std::collections::HashMap;

	/// A one-frame group of `bytes` bytes at `secs` seconds, so segments carry a timestamp span.
	fn group(sequence: u64, bytes: usize, secs: u64) -> Group {
		Group {
			sequence,
			frames: vec![Frame {
				timestamp: Some(Timestamp::from_secs(secs).unwrap()),
				payload: Bytes::from(vec![7u8; bytes]),
			}],
		}
	}

	fn encoded(groups: &[Group]) -> Bytes {
		segment::encode(groups).unwrap()
	}

	/// A tiny stand-in for the eventual object_store: segment id -> bytes. Mirrors what the I/O
	/// layer will do (put on add, get_range on locate), so the index logic is exercised end to end.
	#[derive(Default)]
	struct Store {
		objects: HashMap<SegmentId, Bytes>,
	}

	impl Store {
		/// Read a group as the real tier will: ranged-read `[offset, offset+length)`, decode the
		/// blob with `group_from_blob`. No footer, no full-segment parse.
		fn read(&self, sequence: u64, loc: Location) -> Group {
			let bytes = &self.objects[&loc.segment];
			let blob = bytes.slice(loc.offset as usize..(loc.offset + loc.length) as usize);
			segment::group_from_blob(sequence, blob).unwrap()
		}
	}

	#[test]
	fn add_and_locate_disk() {
		let mut index = Index::new();
		let seg = Segment::open(encoded(&[group(0, 10, 0), group(1, 10, 1)])).unwrap();
		let id = index.add(Tier::Disk, &seg);

		let loc0 = index.locate(0).unwrap();
		assert_eq!(loc0.tier, Tier::Disk);
		assert_eq!(loc0.segment, id);
		assert!(index.locate(2).is_none());
		// The footer entry and the index agree on the byte range.
		assert_eq!(
			(loc0.offset, loc0.length),
			(seg.entries()[0].offset, seg.entries()[0].length)
		);
	}

	#[test]
	fn tier_byte_accounting() {
		let mut index = Index::new();
		let a = Segment::open(encoded(&[group(0, 100, 0)])).unwrap();
		let b = Segment::open(encoded(&[group(1, 50, 1)])).unwrap();
		index.add(Tier::Disk, &a);
		index.add(Tier::Disk, &b);
		assert_eq!(index.bytes(Tier::Disk), a.byte_len() as u64 + b.byte_len() as u64);
		assert_eq!(index.segment_count(Tier::Disk), 2);
		assert_eq!(index.bytes(Tier::Remote), 0);
	}

	#[test]
	fn promotion_empty_within_bounds() {
		let mut index = Index::new();
		index.add(Tier::Disk, &Segment::open(encoded(&[group(0, 10, 0)])).unwrap());
		// A high watermark well above the single small segment: nothing to promote.
		let bounds = Bounds::new(Limit::bytes(0), Limit::bytes(1_000_000));
		assert!(index.promotion(bounds).is_empty());
	}

	#[test]
	fn promotion_selects_oldest_over_high_watermark() {
		let mut index = Index::new();
		let mut ids = Vec::new();
		for seq in 0..5u64 {
			let seg = Segment::open(encoded(&[group(seq, 100, seq)])).unwrap();
			ids.push(index.add(Tier::Disk, &seg));
		}
		// Each segment is >100 bytes; keep ~150 bytes, flush over ~350.
		let bounds = Bounds::new(Limit::bytes(150), Limit::bytes(350));
		let promote = index.promotion(bounds);

		// Oldest-first, leaving the remainder within the low watermark.
		assert_eq!(&promote[..], &ids[..promote.len()]);
		assert!(!promote.is_empty());
		let remaining: Vec<SegmentId> = ids[promote.len()..].to_vec();
		assert!(index.bytes(Tier::Disk) > 0);
		// What remains must be within the low watermark (<= 150 bytes worth of segments).
		let remaining_bytes: u64 = remaining.iter().map(|id| index.segments[id].bytes).sum();
		assert!(remaining_bytes <= 150, "remaining {remaining_bytes} over low watermark");
	}

	#[test]
	fn promotion_duration_watermark() {
		let mut index = Index::new();
		// Segments at 0s, 1s, 2s, 3s; keep 1s, flush over 2s of span.
		for seq in 0..4u64 {
			index.add(Tier::Disk, &Segment::open(encoded(&[group(seq, 10, seq)])).unwrap());
		}
		let bounds = Bounds::new(
			Limit::duration(Duration::from_secs(1)),
			Limit::duration(Duration::from_secs(2)),
		);
		let promote = index.promotion(bounds);
		assert!(!promote.is_empty(), "3s span should exceed the 2s high watermark");
	}

	#[test]
	fn apply_promotion_repoints_to_remote() {
		let mut index = Index::new();
		let g0 = group(0, 100, 0);
		let g1 = group(1, 100, 1);
		let g2 = group(2, 100, 2);
		let s0 = index.add(Tier::Disk, &Segment::open(encoded(std::slice::from_ref(&g0))).unwrap());
		let s1 = index.add(Tier::Disk, &Segment::open(encoded(std::slice::from_ref(&g1))).unwrap());
		index.add(Tier::Disk, &Segment::open(encoded(std::slice::from_ref(&g2))).unwrap());

		// Roll up the two oldest disk segments into one remote object.
		let promoted = [s0, s1];
		let rolled = segment::rollup(&[encoded(&[g0]), encoded(&[g1])]).unwrap();
		let remote = Segment::open(rolled).unwrap();
		let new_id = index.apply_promotion(&promoted, &remote);

		// Sequences 0 and 1 now live remotely in one segment; the disk segments are gone.
		assert_eq!(index.locate(0).unwrap().tier, Tier::Remote);
		assert_eq!(index.locate(1).unwrap().tier, Tier::Remote);
		assert_eq!(index.locate(0).unwrap().segment, new_id);
		assert_eq!(index.locate(1).unwrap().segment, new_id);
		// Sequence 2 is untouched on disk.
		assert_eq!(index.locate(2).unwrap().tier, Tier::Disk);
		// Disk dropped the two promoted segments; remote gained one.
		assert_eq!(index.segment_count(Tier::Disk), 1);
		assert_eq!(index.segment_count(Tier::Remote), 1);
	}

	#[test]
	fn evict_drops_segments_and_their_locations() {
		let mut index = Index::new();
		let a = index.add(Tier::Disk, &Segment::open(encoded(&[group(0, 10, 0)])).unwrap());
		index.add(Tier::Disk, &Segment::open(encoded(&[group(1, 10, 1)])).unwrap());

		index.evict(&[a]);
		assert!(index.locate(0).is_none(), "evicted segment's groups are gone");
		assert!(index.locate(1).is_some(), "other segment untouched");
		assert_eq!(index.segment_count(Tier::Disk), 1);
	}

	#[test]
	fn end_to_end_locate_then_read_through_promotion() {
		// Build disk segments, store their bytes, and verify a located group decodes correctly
		// both before and after promotion (the rollup repoints offsets, the read still matches).
		let mut index = Index::new();
		let mut store = Store::default();

		let groups = [group(0, 40, 0), group(1, 40, 1), group(2, 40, 2)];
		for g in &groups {
			let bytes = encoded(std::slice::from_ref(g));
			let id = index.add(Tier::Disk, &Segment::open(bytes.clone()).unwrap());
			store.objects.insert(id, bytes);
		}

		// Before promotion: each group reads back identically from its disk location.
		for g in &groups {
			let loc = index.locate(g.sequence).unwrap();
			assert_eq!(&store.read(g.sequence, loc), g);
		}

		// Promote sequences 0 and 1 into one remote object.
		let promoted = [index.locate(0).unwrap().segment, index.locate(1).unwrap().segment];
		let rolled = segment::rollup(&[
			encoded(std::slice::from_ref(&groups[0])),
			encoded(std::slice::from_ref(&groups[1])),
		])
		.unwrap();
		let remote_id = index.apply_promotion(&promoted, &Segment::open(rolled.clone()).unwrap());
		store.objects.insert(remote_id, rolled);

		// After promotion: every group still reads back identically, now via the remote segment.
		for g in &groups {
			let loc = index.locate(g.sequence).unwrap();
			assert_eq!(
				&store.read(g.sequence, loc),
				g,
				"sequence {} mismatched after promotion",
				g.sequence
			);
		}
	}
}
