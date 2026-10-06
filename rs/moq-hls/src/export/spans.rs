//! One rendition's own timeline, which resolves the broadcast's segments to that rendition's frames.
//!
//! Segment boundaries come from the reference rendition's records (see [`super::segments`]).
//! Every other rendition maps each boundary onto its own timeline:
//!
//! * a video rendition snaps each boundary to its nearest record starting on a keyframe within
//!   [`TOLERANCE`]; a segment with no start in range is a gap (HLS `EXT-X-GAP`), so a player
//!   switching renditions lands on the next real segment;
//! * an audio rendition takes every frame whose timestamp falls in the segment's span, possibly
//!   from more than one record.
//!
//! A segment resolves only once the rendition's timeline has reached past its end (or ended), so
//! every edge and every reload agree on its content. A failed timeline ends the rendition's
//! playlist at its last record instead of listing the rest as gaps.

use std::collections::VecDeque;
use std::ops::Range;
use std::task::Poll;
use std::time::Duration;

use hang::timeline::Position;
use moq_mux::timeline::Entry;

/// How far a video boundary may move to land on a keyframe.
pub(crate) const TOLERANCE: Duration = Duration::from_secs(1);

/// What one segment holds on one rendition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Content {
	/// The rendition's timeline has not reached the segment's end yet.
	Pending,
	/// The rendition has no content for the segment.
	Gap,
	/// The rendition's timeline failed before reaching the segment's end, so neither it nor any
	/// later segment will resolve.
	Failed,
	/// The frames to fetch, as position ranges, keeping only those whose timestamps fall in
	/// `filter` when set.
	Frames {
		ranges: Vec<Range<Position>>,
		filter: Option<Range<Duration>>,
	},
}

struct State {
	/// Retained records, oldest first.
	records: VecDeque<Entry>,
	/// No more records will arrive, so nothing is pending.
	ended: bool,
	/// The timeline failed: no more records will arrive, and a segment past the last one fails
	/// rather than resolving as a gap.
	failed: bool,
}

impl State {
	fn resolve(&self, is_video: bool, span: Range<Duration>) -> Content {
		if self.failed && self.records.back().is_none_or(|last| last.end_time() < span.end) {
			return Content::Failed;
		}
		let records = self.records.iter().collect();
		let ended = self.ended || self.failed;
		match is_video {
			true => video(records, ended, span),
			false => audio(records, ended, span),
		}
	}
}

/// A rendition's own record window, fed by its timeline watcher.
pub(crate) struct Spans {
	state: kio::Producer<State>,
}

impl Spans {
	pub fn new() -> Self {
		Self {
			state: kio::Producer::new(State {
				records: VecDeque::new(),
				ended: false,
				failed: false,
			}),
		}
	}

	/// Append a record. Only [`trim`](Self::trim) and source pops remove records.
	pub fn push(&self, entry: Entry) {
		let Ok(mut state) = self.state.write() else {
			return;
		};
		if state
			.records
			.back()
			.is_some_and(|back| entry.sequence <= back.sequence || entry.pts < back.pts)
		{
			// The publisher restarted its timeline; the old records describe other media.
			state.records.clear();
		}
		state.records.push_back(entry);
	}

	/// Evict records that end before `oldest`, the start of the oldest listed segment, less the
	/// snapping tolerance. Tied to the listed segments rather than this timeline's own newest
	/// content, so a rendition running ahead of a stalled reference never drops a record a listed
	/// segment still resolves to.
	pub fn trim(&self, oldest: Duration) {
		let Ok(mut state) = self.state.write() else {
			return;
		};
		while state.records.len() >= 2 && state.records[0].end_time() + 2 * TOLERANCE < oldest {
			state.records.pop_front();
		}
	}

	/// Remove the records with these sequences.
	pub fn pop(&self, sequences: Range<u64>) {
		if let Ok(mut state) = self.state.write() {
			state.records.retain(|record| !sequences.contains(&record.sequence));
		}
	}

	/// Forget every record after the source skipped some.
	pub fn clear(&self) {
		if let Ok(mut state) = self.state.write() {
			state.records.clear();
		}
	}

	/// No more records will arrive.
	pub fn end(&self) {
		if let Ok(mut state) = self.state.write() {
			state.ended = true;
		}
	}

	/// The timeline failed: segments its records cover still resolve, and later ones are
	/// [`Content::Failed`].
	pub fn fail(&self) {
		if let Ok(mut state) = self.state.write() {
			state.failed = true;
		}
	}

	/// Resolve the segment spanning `span`, snapping to keyframes when `video`.
	pub fn resolve(&self, video: bool, span: Range<Duration>) -> Content {
		self.state.read().resolve(video, span)
	}

	/// Poll until the segment spanning `span` stops being [`Content::Pending`].
	pub fn poll_resolved(&self, waiter: &kio::Waiter, video: bool, span: Range<Duration>) -> Poll<()> {
		let poll = self
			.state
			.poll_ref(waiter, |state| match state.resolve(video, span.clone()) {
				Content::Pending => Poll::Pending,
				_ => Poll::Ready(()),
			});
		match poll {
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// The newest group known to start with a keyframe, used to bootstrap an init segment for
	/// inline-parameter-set codecs.
	pub fn latest_keyframe_group(&self) -> Option<u64> {
		let state = self.state.read();
		state
			.records
			.iter()
			.rev()
			.find(|record| record.keyframe && record.start.frame == 0)
			.map(|record| record.start.group)
	}
}

/// Snap `boundary` to the nearest keyframe record start within [`TOLERANCE`], preferring the
/// earlier one on a tie.
fn snap<'a>(candidates: &[&'a Entry], boundary: Duration) -> Option<&'a Entry> {
	candidates
		.iter()
		.copied()
		.filter(|record| Duration::from(record.pts).abs_diff(boundary) <= TOLERANCE)
		.min_by_key(|record| Duration::from(record.pts).abs_diff(boundary))
}

/// Every record between two positions, merged into contiguous ranges.
fn ranges(records: &[&Entry], frames: Range<Position>) -> Vec<Range<Position>> {
	let mut out: Vec<Range<Position>> = Vec::new();
	for record in records {
		let start = record.start.max(frames.start);
		let end = record.end.min(frames.end);
		if start >= end {
			continue;
		}
		match out.last_mut() {
			Some(last) if last.end == start => last.end = end,
			_ => out.push(start..end),
		}
	}
	out
}

fn video(records: Vec<&Entry>, ended: bool, span: Range<Duration>) -> Content {
	// A nearer keyframe can still arrive until the timeline passes the boundary plus the tolerance.
	let reached = |time: Duration| ended || records.last().is_some_and(|last| last.end_time() >= time + TOLERANCE);
	if !reached(span.end) {
		return Content::Pending;
	}

	let candidates: Vec<&Entry> = records
		.iter()
		.copied()
		.filter(|record| record.keyframe && record.start.frame == 0)
		.collect();
	let Some(first) = snap(&candidates, span.start) else {
		return Content::Gap;
	};
	// Without a keyframe near the end, run to the next one, so no frame falls between segments.
	let end = match snap(&candidates, span.end) {
		Some(record) => record.start,
		None => match candidates.iter().find(|record| Duration::from(record.pts) > span.end) {
			Some(record) => record.start,
			None if ended => records.last().expect("a candidate exists").end,
			None => return Content::Pending,
		},
	};
	if first.start >= end {
		return Content::Gap;
	}
	Content::Frames {
		ranges: ranges(&records, first.start..end),
		filter: None,
	}
}

fn audio(records: Vec<&Entry>, ended: bool, span: Range<Duration>) -> Content {
	if !ended && records.last().is_none_or(|last| last.end_time() < span.end) {
		return Content::Pending;
	}
	let overlapping: Vec<&Entry> = records
		.iter()
		.copied()
		.filter(|record| Duration::from(record.pts) < span.end && record.end_time() > span.start)
		.collect();
	let (Some(first), Some(last)) = (overlapping.first(), overlapping.last()) else {
		return Content::Gap;
	};
	Content::Frames {
		ranges: ranges(&overlapping, first.start..last.end),
		filter: Some(span),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn secs(s: f64) -> Duration {
		Duration::from_secs_f64(s)
	}

	fn record(sequence: u64, pts_ms: u64, duration_ms: u64, start: Position, end: Position) -> Entry {
		Entry {
			sequence,
			pts: moq_net::Timestamp::from_millis(pts_ms).unwrap(),
			duration: Duration::from_millis(duration_ms),
			start,
			end,
			keyframe: start.frame == 0,
			ext: (),
		}
	}

	/// One whole-group record per GOP, starting at `starts` (ms), the last one ending at `end`.
	fn gops(starts: &[u64], end: u64) -> Vec<Entry> {
		starts
			.iter()
			.enumerate()
			.map(|(i, &pts)| {
				let next = starts.get(i + 1).copied().unwrap_or(end);
				let group = i as u64;
				record(
					i as u64,
					pts,
					next - pts,
					Position::group(group),
					Position::group(group + 1),
				)
			})
			.collect()
	}

	fn frames(ranges: &[(Position, Position)], filter: Option<Range<Duration>>) -> Content {
		Content::Frames {
			ranges: ranges.iter().map(|&(start, end)| start..end).collect(),
			filter,
		}
	}

	#[test]
	fn video_snaps_each_boundary_to_the_nearest_gop() {
		// GOPs 300ms off the reference's 2s boundaries.
		let records = gops(&[300, 2_300, 4_300, 6_300], 8_300);
		let refs: Vec<&Entry> = records.iter().collect();
		assert_eq!(
			video(refs.clone(), false, secs(2.0)..secs(4.0)),
			frames(&[(Position::group(1), Position::group(2))], None)
		);
		// The end boundary is not yet a tolerance behind the newest record.
		assert_eq!(video(refs.clone(), false, secs(6.0)..secs(8.0)), Content::Pending);
		assert_eq!(
			video(refs, true, secs(6.0)..secs(8.0)),
			frames(&[(Position::group(3), Position::group(4))], None)
		);
	}

	#[test]
	fn video_without_a_nearby_start_is_a_gap() {
		// GOPs never coincide with the reference: 3.5s cadence against 2s boundaries.
		let records = gops(&[0, 3_500, 7_000], 10_500);
		let refs: Vec<&Entry> = records.iter().collect();
		assert_eq!(video(refs.clone(), true, secs(2.0)..secs(4.0)), Content::Gap);
		// The segment before the gap runs to the next GOP, so no frame is lost between them.
		assert_eq!(
			video(refs.clone(), true, secs(0.0)..secs(2.0)),
			frames(&[(Position::group(0), Position::group(1))], None)
		);
		assert_eq!(
			video(refs, true, secs(4.0)..secs(6.0)),
			frames(&[(Position::group(1), Position::group(2))], None)
		);
	}

	#[test]
	fn a_rendition_that_starts_later_is_a_gap_until_it_starts() {
		let records = gops(&[4_000, 6_000], 8_000);
		let refs: Vec<&Entry> = records.iter().collect();
		assert_eq!(video(refs.clone(), false, secs(0.0)..secs(2.0)), Content::Gap);
		assert_eq!(
			video(refs, false, secs(4.0)..secs(6.0)),
			frames(&[(Position::group(0), Position::group(1))], None)
		);
	}

	#[test]
	fn audio_takes_every_overlapping_record() {
		// 1s audio records against 2.5s segments.
		let records: Vec<Entry> = (0..6)
			.map(|i| record(i, i * 1_000, 1_000, Position::group(i * 2), Position::group(i * 2 + 2)))
			.collect();
		let refs: Vec<&Entry> = records.iter().collect();
		assert_eq!(
			audio(refs.clone(), false, secs(2.5)..secs(5.0)),
			frames(&[(Position::group(4), Position::group(10))], Some(secs(2.5)..secs(5.0)))
		);
		assert_eq!(audio(refs.clone(), false, secs(5.0)..secs(7.5)), Content::Pending);
		assert_eq!(audio(refs, true, secs(7.0)..secs(9.0)), Content::Gap);
	}

	#[test]
	fn a_frame_split_record_is_not_a_video_boundary() {
		let records = [
			record(0, 0, 10_000, Position::group(0), Position::new(0, 300)),
			record(1, 10_000, 2_000, Position::new(0, 300), Position::group(1)),
			record(2, 12_000, 2_000, Position::group(1), Position::group(2)),
		];
		let refs: Vec<&Entry> = records.iter().collect();
		assert_eq!(video(refs.clone(), true, secs(10.0)..secs(12.0)), Content::Gap);
		assert_eq!(
			video(refs, true, secs(0.0)..secs(12.0)),
			frames(&[(Position::group(0), Position::group(1))], None),
			"the split's halves merge back into one range"
		);
	}

	#[test]
	fn a_trim_keeps_records_a_snap_may_need() {
		let spans = Spans::new();
		for entry in gops(&[0, 2_000, 4_000, 6_000, 8_000, 10_000], 12_000) {
			spans.push(entry);
		}
		spans.trim(secs(8.0));
		let state = spans.state.read();
		assert_eq!(state.records.front().unwrap().sequence, 2);
	}

	#[test]
	fn a_failed_timeline_resolves_only_what_its_records_cover() {
		let spans = Spans::new();
		for entry in gops(&[0, 2_000, 4_000], 6_000) {
			spans.push(entry);
		}
		// Waiting on a keyframe that could still land near the boundary.
		assert_eq!(spans.resolve(true, secs(4.0)..secs(6.0)), Content::Pending);

		spans.fail();
		assert_eq!(
			spans.resolve(true, secs(4.0)..secs(6.0)),
			frames(&[(Position::group(2), Position::group(3))], None)
		);
		assert_eq!(spans.resolve(true, secs(6.0)..secs(8.0)), Content::Failed);
		assert_eq!(spans.resolve(false, secs(6.0)..secs(8.0)), Content::Failed);
		let resolved = spans.poll_resolved(&kio::Waiter::noop(), true, secs(6.0)..secs(8.0));
		assert!(resolved.is_ready(), "nothing waits on a failed timeline");
	}
}
