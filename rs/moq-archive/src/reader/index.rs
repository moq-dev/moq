use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use hang::timeline::{Position, Record};

use crate::path::check_id;

/// One committed record: the object at `segments/<sequence>` and the frames it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
	/// The record's sequence, which names its object.
	pub sequence: u64,
	/// The first frame, inclusive.
	pub start: Position,
	/// The end, exclusive.
	pub end: Position,
}

impl Span {
	/// Build from a record, refusing an empty span or IDs past the recording range.
	fn new(record: &Record) -> Option<Self> {
		check_id(record.sequence).ok()?;
		check_id(record.start.group).ok()?;
		check_id(record.end.group).ok()?;
		(record.start < record.end).then_some(Self {
			sequence: record.sequence,
			start: record.start,
			end: record.end,
		})
	}
}

/// One track's committed spans.
#[derive(Default)]
struct Spans {
	/// Each span keyed by its start.
	spans: BTreeMap<Position, Span>,
	/// Each span's start keyed by its sequence, so a pop can find it.
	sequences: BTreeMap<u64, Position>,
}

/// The spans each track's replayed timeline window commits.
#[derive(Default)]
pub(super) struct Index {
	tracks: HashMap<String, Spans>,
}

impl Index {
	/// Add `track`'s record.
	///
	/// A malformed record or one overlapping another is left out, so its frames behave like ones
	/// the source never delivered; the track's other records stay usable.
	pub fn push(&mut self, track: &str, record: &Record) {
		let Some(span) = Span::new(record) else {
			tracing::warn!(track, sequence = record.sequence, "ignoring a malformed archive record");
			return;
		};

		let spans = self.tracks.entry(track.to_string()).or_default();
		let before = spans.spans.range(..span.end).next_back();
		if before.is_some_and(|(_, prev)| prev.end > span.start) || spans.sequences.contains_key(&span.sequence) {
			tracing::warn!(track, sequence = record.sequence, "ignoring an overlapping archive record");
			return;
		}
		spans.spans.insert(span.start, span);
		spans.sequences.insert(span.sequence, span.start);
	}

	/// Remove `track`'s records with these sequences, returning the evicted spans.
	pub fn pop(&mut self, track: &str, sequences: Range<u64>) -> Vec<Span> {
		let Some(spans) = self.tracks.get_mut(track) else {
			return Vec::new();
		};
		let popped: Vec<u64> = spans.sequences.range(sequences).map(|(sequence, _)| *sequence).collect();
		popped
			.into_iter()
			.filter_map(|sequence| {
				let start = spans.sequences.remove(&sequence)?;
				spans.spans.remove(&start)
			})
			.collect()
	}

	/// The spans holding frames of `group` on `track`, in order.
	pub fn group(&self, track: &str, group: u64) -> Vec<Span> {
		let Some(spans) = self.tracks.get(track) else {
			return Vec::new();
		};
		let head = Position::group(group);
		let mut found: Vec<Span> = spans
			.spans
			.range(..Position::group(group.saturating_add(1)))
			.rev()
			.map(|(_, span)| *span)
			.take_while(|span| span.end > head)
			.collect();
		found.reverse();
		found
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ID_MAX;

	fn record(sequence: u64, start: Position, end: Position) -> Record {
		Record::new(sequence, sequence * 1000, 1000, start, end)
	}

	fn at(group: u64) -> Position {
		Position::group(group)
	}

	#[test]
	fn lookup_finds_every_span_of_a_group() {
		let mut index = Index::default();
		index.push("audio", &record(0, at(0), at(3)));
		index.push("audio", &record(1, at(5), at(6)));
		// Group 7 split across two records, then whole groups again.
		index.push("video", &record(0, at(7), Position::new(7, 3)));
		index.push("video", &record(1, Position::new(7, 3), at(9)));

		let sequences = |spans: Vec<Span>| spans.iter().map(|span| span.sequence).collect::<Vec<_>>();
		assert_eq!(sequences(index.group("audio", 1)), vec![0]);
		assert!(index.group("audio", 3).is_empty(), "the gap between records");
		assert_eq!(sequences(index.group("audio", 5)), vec![1]);
		assert!(index.group("audio", 6).is_empty());
		assert_eq!(sequences(index.group("video", 7)), vec![0, 1]);
		assert_eq!(sequences(index.group("video", 8)), vec![1]);
		assert!(index.group("chat", 0).is_empty());
	}

	#[test]
	fn pop_evicts_only_the_popped_records() {
		let mut index = Index::default();
		index.push("video", &record(0, at(0), at(2)));
		index.push("video", &record(1, at(2), at(4)));

		let evicted = index.pop("video", 0..1);
		assert_eq!(evicted.len(), 1);
		assert_eq!(evicted[0].start, at(0));
		assert!(index.group("video", 0).is_empty());
		assert_eq!(index.group("video", 2).len(), 1);
		assert!(index.pop("video", 0..1).is_empty(), "already popped");
	}

	#[test]
	fn malformed_and_overlapping_records_are_ignored() {
		let mut index = Index::default();
		index.push("video", &record(0, at(4), at(7)));
		index.push("video", &record(1, at(6), at(9)));
		assert_eq!(index.group("video", 8), Vec::new(), "overlapping the earlier span");
		index.push("video", &record(2, at(3), at(5)));
		assert_eq!(index.group("video", 3), Vec::new(), "overlapping the later span");

		index.push("audio", &record(0, at(3), at(3)));
		assert!(index.group("audio", 3).is_empty(), "an empty span");
		index.push("data", &record(0, at(ID_MAX + 1), at(ID_MAX + 2)));
		assert!(index.group("data", ID_MAX + 1).is_empty(), "past the recording range");
	}
}
