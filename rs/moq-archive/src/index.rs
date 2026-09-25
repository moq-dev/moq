use std::collections::{BTreeMap, HashMap};
use std::ops::{Bound, Range, RangeInclusive};

use hang::timeline::Record;

use crate::path::check_range;

/// One track's stored object for one record: its filename bounds and the exact runs it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Span {
	/// Inclusive first-to-last group sequence, the object's filename bounds.
	pub bounds: RangeInclusive<u64>,
	/// The advertised runs, ascending and nonoverlapping; groups between them never existed.
	pub runs: Vec<RangeInclusive<u64>>,
	/// The advertising record's start, in timeline units.
	pub pts: u64,
}

impl Span {
	/// Build from a record's ranges, refusing empty, reversed, out-of-range, or unordered runs.
	fn new(ranges: &[hang::timeline::Range], pts: u64) -> Option<Self> {
		let mut runs: Vec<RangeInclusive<u64>> = Vec::with_capacity(ranges.len());
		for range in ranges {
			let run = range.start..=range.end;
			check_range(&run).ok()?;
			if let Some(prev) = runs.last()
				&& run.start() <= prev.end()
			{
				return None;
			}
			runs.push(run);
		}
		let bounds = *runs.first()?.start()..=*runs.last()?.end();
		Some(Self { bounds, runs, pts })
	}

	fn contains(&self, group: u64) -> bool {
		self.runs.iter().any(|run| run.contains(&group))
	}
}

/// The committed group ranges advertised by the replayed timeline window.
#[derive(Default)]
pub(crate) struct Index {
	/// Per track, each span keyed by its smallest group sequence.
	tracks: HashMap<String, BTreeMap<u64, Span>>,
	/// Per window index, the `(track, smallest)` spans that record added, so a pop can evict them.
	records: BTreeMap<u64, Vec<(String, u64)>>,
}

impl Index {
	/// Add a record's spans at window `index`.
	///
	/// A track whose ranges are malformed or overlap another record's span is left out, so its
	/// groups behave like ones the source never delivered; the record's other tracks stay usable.
	pub fn push(&mut self, index: u64, record: &Record) {
		let mut added = Vec::new();
		for (track, ranges) in &record.tracks {
			let Some(span) = Span::new(ranges, record.pts) else {
				tracing::warn!(track, segment = record.segment, "ignoring malformed archive ranges");
				continue;
			};

			let spans = self.tracks.entry(track.clone()).or_default();
			let smallest = *span.bounds.start();
			let before = spans.range(..=span.bounds.end()).next_back();
			if before.is_some_and(|(_, prev)| prev.bounds.end() >= span.bounds.start()) {
				tracing::warn!(track, segment = record.segment, "ignoring overlapping archive ranges");
				continue;
			}

			spans.insert(smallest, span);
			added.push((track.clone(), smallest));
		}
		self.records.insert(index, added);
	}

	/// Remove the records at these window indices, returning the evicted spans by track.
	pub fn pop(&mut self, indices: Range<u64>) -> Vec<(String, Span)> {
		let popped: Vec<u64> = self.records.range(indices).map(|(index, _)| *index).collect();
		let mut evicted = Vec::new();
		for index in popped {
			for (track, smallest) in self.records.remove(&index).unwrap_or_default() {
				let span = self.tracks.get_mut(&track).and_then(|spans| spans.remove(&smallest));
				if let Some(span) = span {
					evicted.push((track, span));
				}
			}
		}
		evicted
	}

	/// Whether any replayed record named this track.
	pub fn has_track(&self, track: &str) -> bool {
		self.tracks.contains_key(track)
	}

	/// The first group at or after `group` that the retained window commits on `track`.
	pub fn next(&self, track: &str, group: u64) -> Option<u64> {
		let spans = self.tracks.get(track)?;
		let within = spans.range(..=group).next_back().and_then(|(_, span)| {
			let run = span.runs.iter().find(|run| *run.end() >= group)?;
			Some(group.max(*run.start()))
		});
		within.or_else(|| {
			let (smallest, _) = spans.range((Bound::Excluded(group), Bound::Unbounded)).next()?;
			Some(*smallest)
		})
	}

	/// The first group of the latest `track` span whose record starts at or before `pts`, or of
	/// the earliest span when every record starts later.
	pub fn seek(&self, track: &str, pts: u64) -> Option<u64> {
		let spans = self.tracks.get(track)?;
		let earliest = spans.values().next()?;
		let span = spans
			.values()
			.take_while(|span| span.pts <= pts)
			.last()
			.unwrap_or(earliest);
		Some(*span.bounds.start())
	}

	/// The span advertising `group` on `track`, if the retained window commits it.
	pub fn get(&self, track: &str, group: u64) -> Option<Span> {
		let (_, span) = self.tracks.get(track)?.range(..=group).next_back()?;
		span.contains(group).then(|| span.clone())
	}
}

#[cfg(test)]
mod tests {
	use hang::timeline::Range;

	use super::*;
	use crate::ID_MAX;

	fn record(segment: u64, tracks: &[(&str, &[(u64, u64)])]) -> Record {
		let mut record = Record::new(segment, segment * 1000, 1000);
		for (name, ranges) in tracks {
			let ranges = ranges.iter().map(|&(start, end)| Range::new(start, end)).collect();
			record.tracks.insert(name.to_string(), ranges);
		}
		record
	}

	#[test]
	fn lookup_respects_bounds_and_internal_gaps() {
		let mut index = Index::default();
		index.push(0, &record(0, &[("audio", &[(0, 2), (5, 6)]), ("video", &[(0, 0)])]));
		index.push(1, &record(1, &[("audio", &[(7, 9)])]));

		assert_eq!(index.get("audio", 1).unwrap().bounds, 0..=6);
		assert_eq!(index.get("audio", 6).unwrap().runs, vec![0..=2, 5..=6]);
		assert!(index.get("audio", 3).is_none(), "internal gap");
		assert_eq!(index.get("audio", 8).unwrap().bounds, 7..=9);
		assert!(index.get("audio", 10).is_none());
		assert!(index.get("video", 1).is_none());
		assert!(index.get("chat", 0).is_none());
		assert!(index.has_track("video"));
		assert!(!index.has_track("chat"));
	}

	#[test]
	fn next_skips_gaps_and_popped_records() {
		let mut index = Index::default();
		index.push(0, &record(0, &[("audio", &[(0, 2), (5, 6)])]));
		index.push(1, &record(1, &[("audio", &[(9, 9)]), ("video", &[(3, 3)])]));

		assert_eq!(index.next("audio", 0), Some(0));
		assert_eq!(index.next("audio", 2), Some(2));
		assert_eq!(index.next("audio", 3), Some(5), "internal gap");
		assert_eq!(index.next("audio", 7), Some(9), "gap between records");
		assert_eq!(index.next("audio", 10), None, "past the newest record");
		assert_eq!(index.next("video", 0), Some(3));
		assert_eq!(index.next("chat", 0), None);

		index.pop(0..1);
		assert_eq!(
			index.next("audio", 1),
			Some(9),
			"expired groups skip to the retained window"
		);
	}

	#[test]
	fn seek_picks_the_segment_starting_at_or_before() {
		let mut index = Index::default();
		// Records start at segment * 1000.
		index.push(0, &record(0, &[("video", &[(0, 1)])]));
		index.push(1, &record(1, &[("audio", &[(0, 0)])]));
		index.push(2, &record(2, &[("video", &[(4, 5)])]));

		assert_eq!(index.seek("video", 0), Some(0));
		assert_eq!(index.seek("video", 1999), Some(0), "segment 1 has no video");
		assert_eq!(index.seek("video", 2000), Some(4));
		assert_eq!(index.seek("video", u64::MAX), Some(4));
		assert_eq!(index.seek("chat", 0), None);

		index.pop(0..1);
		assert_eq!(
			index.seek("video", 0),
			Some(4),
			"before the window starts at its oldest span"
		);
	}

	#[test]
	fn pop_evicts_only_the_popped_records() {
		let mut index = Index::default();
		index.push(0, &record(0, &[("video", &[(0, 1)])]));
		index.push(1, &record(1, &[("video", &[(2, 3)])]));

		let evicted = index.pop(0..1);
		assert_eq!(evicted.len(), 1);
		assert_eq!(evicted[0].1.bounds, 0..=1);
		assert!(index.get("video", 0).is_none());
		assert!(index.get("video", 2).is_some());
		assert!(index.pop(0..1).is_empty(), "already popped");
		assert!(index.has_track("video"), "a popped track stays known");
	}

	#[test]
	fn malformed_and_overlapping_tracks_are_ignored() {
		let mut index = Index::default();
		index.push(0, &record(0, &[("video", &[(4, 6)]), ("audio", &[(3, 1)])]));
		assert!(index.get("audio", 1).is_none(), "reversed run");

		// Overlapping the earlier video span drops only the video entry.
		index.push(1, &record(1, &[("video", &[(6, 8)]), ("audio", &[(10, 11)])]));
		assert_eq!(index.get("video", 6).unwrap().bounds, 4..=6);
		assert!(index.get("video", 7).is_none());
		assert!(index.get("audio", 10).is_some());

		// Unordered runs and IDs past the recording range.
		index.push(
			2,
			&record(2, &[("chat", &[(5, 6), (1, 2)]), ("data", &[(ID_MAX, ID_MAX + 1)])]),
		);
		assert!(index.get("chat", 5).is_none());
		assert!(index.get("data", ID_MAX).is_none());

		// Popping a record whose track was ignored leaves the earlier span alone.
		index.pop(1..2);
		assert!(index.get("video", 4).is_some());
	}
}
