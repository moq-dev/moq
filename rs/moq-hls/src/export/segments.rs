//! One rendition's view of the broadcast timeline, as a `Producer`/[`Consumer`] pair.
//!
//! The broadcast has a single timeline track; the catalog watcher reads it and fans each
//! record out to every rendition as a row: the segment's number, timing, and this
//! rendition's group ranges (empty when the record carries no content for it, a gap). Records
//! are self-contained (the timeline only publishes a segment once its content is final on
//! every track), so every row is immediately listable and fetchable. Two things read the
//! window:
//!
//! * the HTTP serve path, synchronously, to render a media playlist and look up a segment's
//!   group ranges (nothing here touches media bytes on that path); and
//! * a [`Consumer`] cursor, for a recorder that wants every segment *with its media*, in
//!   order, exactly once. `next()` waits for the next row, FETCHes and transmuxes its groups
//!   (via [`Rendition`]), and yields the CMAF bytes.
//!
//! Rows carry the broadcast's aligned segment numbers, so a segment is addressed by that
//! number everywhere (the `seg/{segment}.m4s` URI, `EXT-X-MEDIA-SEQUENCE`, the recorder
//! cursor), and the same number names the same span of content time on every rendition.

use std::collections::VecDeque;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use hang::timeline::Range;

use super::Rendition;
use crate::Result;

/// The producing side of a rendition's timeline window.
///
/// The catalog watcher appends rows via [`push`](Self::push) and marks the stream
/// [`end`](Self::end)ed. Cheap to share behind an `Arc`; the window state lives in a
/// [`kio::Producer`] so a [`Consumer`] can await changes without a separate signal.
pub(crate) struct Producer {
	state: kio::Producer<State>,
}

struct State {
	/// Rows within the window, oldest first. Every row is a complete segment.
	rows: VecDeque<Row>,
	/// The first listed segment, or the segment that would follow an empty trimmed window.
	sequence: u64,
	/// The timeline track ended: the broadcast is over (`EXT-X-ENDLIST`).
	ended: bool,
	/// The discontinuity sequence stamped onto the next row, bumped whenever the content
	/// timeline breaks.
	discontinuity: u64,
	/// The end of the last pushed row, to detect a jump even after its row left the window.
	last_end: Option<Duration>,
	/// The rows were cleared, so the next row can't continue the previous timeline.
	broken: bool,
}

/// One playlist segment: its aligned number, timing, and this rendition's group ranges.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
	/// The source timeline record index, used to mirror exact window trims.
	pub index: u64,
	/// The aligned segment number (its URI: `seg/{segment}.m4s`), shared across renditions.
	pub segment: u64,
	/// This rendition's group ranges within the segment. Empty means the rendition has no
	/// content for the span (`EXT-X-GAP`).
	pub ranges: Vec<Range>,
	/// Presentation duration.
	pub duration: Duration,
	/// The segment's starting presentation timestamp.
	pub pts: moq_net::Timestamp,
	/// The segment's ending presentation timestamp (`pts + duration`), for window eviction
	/// and discontinuity detection.
	pub end: Duration,
	/// The discontinuity sequence, assigned by [`Producer::push`]. It changes wherever the
	/// content timeline breaks, identically on every rendition fed the same records, so
	/// renditions mark the same breaks however many segments each one skipped.
	pub discontinuity: u64,
}

/// A consistent read of the window, for rendering one playlist (the serve path only).
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) struct Window {
	/// The `EXT-X-MEDIA-SEQUENCE` of the first listed segment: its aligned segment number, so
	/// sequence numbers line up across renditions.
	pub sequence: u64,
	/// Listed segments, oldest first.
	pub segments: Vec<Row>,
	/// Whether the timeline (and so the playlist) has ended.
	pub ended: bool,
}

/// The next segment a [`Consumer`] should emit, resolved from the window.
enum Next {
	/// A segment is ready to fetch.
	Ready(Row),
	/// No further segment will ever appear (the timeline ended).
	Ended,
	/// Nothing new yet; wait for the next window change.
	Pending,
}

impl State {
	/// Snapshot the current window. Every row is a complete segment, so all are listed.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	fn window(&self) -> Window {
		Window {
			sequence: self.sequence,
			segments: self.rows.iter().cloned().collect(),
			ended: self.ended,
		}
	}

	/// The first segment numbered past `after`, for a cursor.
	///
	/// Rows are complete the moment they arrive. Segments evicted from the front of the
	/// window before the cursor reached them are skipped: the cursor resumes at the oldest
	/// row still in the window.
	fn next_after(&self, after: Option<u64>) -> Next {
		let next = self.rows.iter().find(|r| match after {
			Some(after) => r.segment > after,
			None => true,
		});
		match next {
			Some(row) => Next::Ready(row.clone()),
			None if self.ended => Next::Ended,
			None => Next::Pending,
		}
	}
}

impl Producer {
	/// An empty window.
	pub fn new() -> Self {
		Self {
			state: kio::Producer::new(State {
				rows: VecDeque::new(),
				sequence: 0,
				ended: false,
				discontinuity: 0,
				last_end: None,
				broken: false,
			}),
		}
	}

	/// Append a row, evicting the front of the window past `window`.
	pub fn push(&self, mut row: Row, window: Duration) {
		let Ok(mut state) = self.state.write() else {
			return;
		};

		if let Some(back) = state.rows.back() {
			// A backward jump in pts or segment number means the publisher restarted its timeline;
			// the old window can't be stitched onto the new one, so start over.
			if Duration::from(row.pts) < Duration::from(back.pts) || row.segment <= back.segment {
				tracing::warn!("timeline jumped backwards; resetting the playlist window");
				state.rows.clear();
			}
		}
		if state.rows.is_empty() {
			state.sequence = row.segment;
		}

		// Tolerate sub-millisecond drift from timescale rounding.
		let start = Duration::from(row.pts);
		let jumped = state
			.last_end
			.is_some_and(|end| start.saturating_sub(end).max(end.saturating_sub(start)) > Duration::from_millis(1));
		if state.broken || jumped {
			state.discontinuity += 1;
		}
		state.broken = false;
		state.last_end = Some(row.end);
		row.discontinuity = state.discontinuity;

		state.rows.push_back(row);

		// Evict from the front while the remaining rows still cover the window.
		while state.rows.len() >= 2 {
			let span = state.rows.back().unwrap().end.saturating_sub(state.rows[1].pts.into());
			if span < window {
				break;
			}
			state.rows.pop_front();
		}
		state.sequence = state.rows.front().unwrap().segment;
	}

	/// Remove rows whose source timeline indices fall within `range`.
	pub fn pop(&self, range: std::ops::Range<u64>) {
		if let Ok(mut state) = self.state.write() {
			let after = state
				.rows
				.iter()
				.filter(|row| range.contains(&row.index))
				.map(|row| row.segment.saturating_add(1))
				.max();
			state.rows.retain(|row| !range.contains(&row.index));
			state.sequence = state
				.rows
				.front()
				.map(|row| row.segment)
				.or(after)
				.unwrap_or(state.sequence);
		}
	}

	/// Clear every retained row after an unrecoverable gap in the source timeline.
	pub fn clear(&self) {
		if let Ok(mut state) = self.state.write() {
			state.rows.clear();
			state.broken = true;
		}
	}

	/// Mark the timeline ended (the broadcast finished cleanly): the playlist gets
	/// `EXT-X-ENDLIST` and cursors end once drained.
	pub fn end(&self) {
		if let Ok(mut state) = self.state.write() {
			state.ended = true;
		}
	}

	/// Close the channel: no more rows will arrive. A [`Consumer`] drains the segments it
	/// can still see and then ends; the serve path keeps reading the frozen window. Call after
	/// [`end`](Self::end) on a clean finish, or on its own when the source is lost mid-stream.
	pub fn close(&self) {
		let _ = self.state.close();
	}

	/// Snapshot the current window (serve path).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub fn window(&self) -> Window {
		self.state.read().window()
	}

	/// The group ranges segment `segment` covers for this rendition, or `None` if it isn't in
	/// the window. An empty vec means the segment is a gap for this rendition.
	pub fn segment_ranges(&self, segment: u64) -> Option<Vec<Range>> {
		let state = self.state.read();
		let row = state.rows.iter().find(|r| r.segment == segment)?;
		Some(row.ranges.clone())
	}

	/// The number of the segment whose `pts` is exactly `time` in the timeline's timescale
	/// (DASH `$Time$` addressing), or `None` when no listed row starts there. Exact match is
	/// safe because the rendered `S@t` and this lookup convert the same [`Row::pts`] the same
	/// way.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub fn segment_number_at(&self, time: u64, timescale: moq_net::Timescale) -> Option<u64> {
		let state = self.state.read();
		state
			.rows
			.iter()
			.find(|row| row.pts.as_scale(timescale) == time as u128)
			.map(|row| row.segment)
	}

	/// The newest group known to start with a keyframe, used to bootstrap an init segment for
	/// inline-parameter-set codecs.
	pub fn latest_keyframe_group(&self) -> Option<u64> {
		let state = self.state.read();
		state
			.rows
			.iter()
			.rev()
			.flat_map(|row| row.ranges.iter().rev())
			.find(|range| range.keyframe)
			.map(|range| range.start)
	}

	/// Whether the playlist has anything to serve yet (at least one segment, or the broadcast
	/// already ended).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub fn is_playable(&self) -> bool {
		let state = self.state.read();
		state.ended || !state.rows.is_empty()
	}

	/// Poll until [`is_playable`](Self::is_playable), for the serve path's long-poll.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub fn poll_playable(&self, waiter: &kio::Waiter) -> Poll<()> {
		let poll = self.state.poll_ref(waiter, |state| {
			if state.ended || !state.rows.is_empty() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		});
		match poll {
			// Ready, or the channel closed (no more rows will arrive): stop waiting either way.
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// A cursor over segments, starting from the oldest still in the window.
	pub fn subscribe(&self, rendition: Arc<Rendition>) -> Consumer {
		Consumer {
			state: self.state.consume(),
			rendition,
			after: None,
			emitted: None,
		}
	}
}

/// A segment with its transmuxed media, yielded by a [`Consumer`].
pub struct Segment {
	/// The aligned segment number (also its `seg/{segment}.m4s` URI stem), shared across the
	/// broadcast's renditions.
	pub segment: u64,
	/// The transmuxed CMAF fragment (`moof`+`mdat`), fetched on demand by [`Consumer::next`].
	pub media: Bytes,
	/// Presentation duration.
	pub duration: Duration,
	/// Wall-clock start time, when the timeline advertises an anchor.
	pub program_date_time: Option<SystemTime>,
	/// The content timeline breaks before this segment (the source skipped or restarted), so a
	/// recorder marks an `EXT-X-DISCONTINUITY` here. Every rendition marks the same breaks, as
	/// HLS requires. Segments this cursor skipped (evicted, uncached, or gaps with no content
	/// for this rendition) leave a hole on a continuous timeline, not a discontinuity.
	pub discontinuity: bool,
}

/// A cursor over one rendition's segments, in timeline order.
///
/// Obtained from [`Rendition::segments`](super::Rendition::segments). Drives the same
/// fetch-on-demand path the HTTP serve path uses, so it adds no standing traffic: each
/// [`next`](Self::next) awaits the next segment, then FETCHes and transmuxes it.
pub struct Consumer {
	state: kio::Consumer<State>,
	rendition: Arc<Rendition>,
	/// The number of the last segment returned; the next call resumes strictly after it. Only
	/// advanced once a segment is fetched or skipped, so a transient fetch error re-tries the
	/// same segment on the next call instead of losing it.
	after: Option<u64>,
	/// The discontinuity sequence of the last segment returned.
	emitted: Option<u64>,
}

impl Consumer {
	/// The rendition's CMAF init segment, built once and cached; `None` until it can be built
	/// (an inline-parameter-set codec needs the first segment first).
	pub async fn init(&self) -> Result<Option<Bytes>> {
		self.rendition.init().await
	}

	/// The next segment, with its media; `None` once the rendition ends.
	///
	/// Waits for the next segment, then FETCHes and transmuxes its groups. A segment whose
	/// groups already left the relay cache (or that is a gap for this rendition) is skipped,
	/// resuming at the next one, rather than surfaced as an error; a real fetch/transmux
	/// failure is returned, leaving the cursor to retry it on the next call.
	pub async fn next(&mut self) -> Result<Option<Segment>> {
		loop {
			let Some(row) = kio::wait(|waiter| self.poll_next(waiter)).await else {
				return Ok(None);
			};
			match self.rendition.segment(row.segment).await? {
				Some(media) => {
					return Ok(Some(Segment {
						segment: row.segment,
						media,
						duration: row.duration,
						program_date_time: self.rendition.wall_clock(row.pts),
						discontinuity: self.emit(&row),
					}));
				}
				None => self.after = Some(row.segment),
			}
		}
	}

	/// Advance past `row` as returned, reporting whether it starts a new discontinuity.
	fn emit(&mut self, row: &Row) -> bool {
		self.after = Some(row.segment);
		let previous = self.emitted.replace(row.discontinuity);
		previous.is_some_and(|previous| previous != row.discontinuity)
	}

	fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Option<Row>> {
		let poll = self.state.poll(waiter, |state| match state.next_after(self.after) {
			Next::Ready(row) => Poll::Ready(Some(row)),
			Next::Ended => Poll::Ready(None),
			Next::Pending => Poll::Pending,
		});
		match poll {
			Poll::Ready(Ok(found)) => Poll::Ready(found),
			// The producer closed without a clean end (broadcast dropped): no more segments.
			Poll::Ready(Err(_)) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn row(segment: u64, group: u64, pts_ms: u64, duration_ms: u64) -> Row {
		let pts = moq_net::Timestamp::from_millis(pts_ms).unwrap();
		Row {
			index: segment,
			segment,
			ranges: vec![Range::new(group, group)],
			duration: Duration::from_millis(duration_ms),
			pts,
			end: Duration::from(pts) + Duration::from_millis(duration_ms),
			discontinuity: 0,
		}
	}

	#[test]
	fn every_row_is_listed() {
		let live = Producer::new();
		live.push(row(0, 0, 0, 2_000), Duration::from_secs(30));
		live.push(row(1, 1, 2_000, 2_000), Duration::from_secs(30));

		let window = live.window();
		assert_eq!(window.sequence, 0);
		assert!(!window.ended);
		assert_eq!(window.segments.len(), 2, "rows are complete segments; all are listed");
		assert_eq!(window.segments[0].segment, 0);
		assert_eq!(window.segments[0].duration, Duration::from_secs(2));

		live.end();
		assert!(live.window().ended);
	}

	#[test]
	fn window_evicts_and_advances_sequence() {
		let live = Producer::new();
		let window = Duration::from_secs(4);
		for i in 0..6u64 {
			live.push(row(i, i, i * 2_000, 2_000), window);
		}

		let snapshot = live.window();
		// Segments still cover >= 4s after eviction, and the sequence is the first listed
		// segment's aligned number.
		assert!(snapshot.sequence > 0);
		let span: Duration = snapshot.segments.iter().map(|s| s.duration).sum();
		assert!(span >= Duration::from_secs(4));
		assert_eq!(snapshot.segments.first().unwrap().segment, snapshot.sequence);
	}

	#[test]
	fn source_window_pop_removes_playlist_rows() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		for i in 0..4u64 {
			let mut row = row(i, i, i * 2_000, 2_000);
			row.index = i + 10;
			live.push(row, window);
		}

		live.pop(10..12);
		let snapshot = live.window();
		assert_eq!(snapshot.sequence, 2);
		assert_eq!(
			snapshot.segments.iter().map(|row| row.segment).collect::<Vec<_>>(),
			vec![2, 3]
		);

		live.pop(12..14);
		let snapshot = live.window();
		assert_eq!(snapshot.sequence, 4);
		assert!(snapshot.segments.is_empty());
	}

	#[test]
	fn a_skipped_source_range_clears_rows_before_the_next_segment() {
		let live = Producer::new();
		live.push(row(4, 4, 8_000, 2_000), Duration::from_secs(10));
		live.clear();
		live.push(row(10, 10, 20_000, 2_000), Duration::from_secs(10));

		let snapshot = live.window();
		assert_eq!(snapshot.sequence, 10);
		assert_eq!(
			snapshot.segments.iter().map(|row| row.segment).collect::<Vec<_>>(),
			vec![10]
		);
		assert_eq!(
			snapshot.segments[0].discontinuity, 1,
			"the row after a skipped source range starts a new discontinuity"
		);
	}

	#[test]
	fn a_skipped_row_on_a_continuous_timeline_is_not_a_discontinuity() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		for i in 0..3u64 {
			live.push(row(i, i, i * 2_000, 2_000), window);
		}

		// A cursor that emits segment 0 and skips segment 1 (uncached, or a gap for its
		// rendition) must not mark segment 2: a sibling that fetched segment 1 wouldn't, and
		// players require renditions to agree on discontinuities.
		let snapshot = live.window();
		assert_eq!(snapshot.segments[0].discontinuity, snapshot.segments[2].discontinuity);
	}

	#[test]
	fn a_content_time_jump_starts_a_new_discontinuity() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 2_000), window);
		live.push(row(1, 1, 2_000, 2_000), window);
		live.push(row(2, 2, 10_000, 2_000), window);
		live.push(row(3, 3, 12_000, 2_000), window);

		let sequences: Vec<_> = live.window().segments.iter().map(|row| row.discontinuity).collect();
		assert_eq!(sequences, vec![0, 0, 1, 1]);
	}

	#[test]
	fn segment_ranges_and_gaps() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 1_000), window);
		// Segment 1 is a gap for this rendition: no ranges.
		live.push(
			Row {
				index: 1,
				segment: 1,
				ranges: Vec::new(),
				duration: Duration::from_secs(1),
				pts: moq_net::Timestamp::from_millis(1_000).unwrap(),
				end: Duration::from_millis(2_000),
				discontinuity: 0,
			},
			window,
		);
		live.push(row(2, 100, 2_000, 1_000), window);

		assert_eq!(live.segment_ranges(0), Some(vec![Range::new(0, 0)]));
		assert_eq!(live.segment_ranges(1), Some(vec![]), "a gap is present but empty");
		assert_eq!(live.segment_ranges(7), None, "unknown segments miss");

		// The gap row carries no keyframe group; the bootstrap group comes from segment 2.
		assert_eq!(live.latest_keyframe_group(), Some(100));
	}

	#[test]
	fn backwards_jump_resets_the_window() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 10_000, 2_000), window);
		live.push(row(1, 1, 12_000, 2_000), window);
		live.push(row(2, 2, 1_000, 2_000), window); // restart: pts rewound

		let snapshot = live.window();
		assert_eq!(snapshot.segments.len(), 1, "the window restarted at the new row");
		assert_eq!(snapshot.segments[0].segment, 2);

		// A segment number that rewinds (a restarted publisher) resets the same way.
		live.push(row(0, 0, 2_000, 2_000), window);
		assert_eq!(live.window().segments.len(), 1);
	}

	#[test]
	fn next_after_walks_segments() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 2_000), window);
		live.push(row(1, 1, 2_000, 2_000), window);

		let Next::Ready(first) = live.state.read().next_after(None) else {
			panic!("expected a segment");
		};
		assert_eq!(first.segment, 0);
		let Next::Ready(second) = live.state.read().next_after(Some(0)) else {
			panic!("expected a segment");
		};
		assert_eq!(second.segment, 1);
		assert!(
			matches!(live.state.read().next_after(Some(1)), Next::Pending),
			"nothing further while live"
		);

		live.end();
		assert!(matches!(live.state.read().next_after(Some(1)), Next::Ended));
	}
}
