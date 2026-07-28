//! One rendition's view of the broadcast timeline, as a `Producer`/[`Consumer`] pair.
//!
//! The broadcast has a single timeline track; the catalog watcher reads it and fans each
//! record out to every rendition as a row: the segment's number, timing, and this
//! rendition's group ranges (empty when the record carries no content for it, a gap). Records
//! are self-contained (the segmenter only publishes a segment once its content is final on
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
use std::sync::{Arc, OnceLock};
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
	/// The broadcast serving the media track, resolved once (it may be a sibling broadcast
	/// when the catalog rendition carries a `broadcast` reference).
	pub broadcast: OnceLock<moq_net::broadcast::Consumer>,
}

struct State {
	/// Rows within the window, oldest first. Every row is a complete segment.
	rows: VecDeque<Row>,
	/// The timeline track ended: the broadcast is over (`EXT-X-ENDLIST`).
	ended: bool,
}

/// One playlist segment: its aligned number, timing, and this rendition's group ranges.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
	/// The aligned segment number (its URI: `seg/{segment}.m4s`), shared across renditions.
	pub segment: u64,
	/// This rendition's group ranges within the segment. Empty means the rendition has no
	/// content for the span (`EXT-X-GAP`).
	pub ranges: Vec<Range>,
	/// Presentation duration in seconds.
	pub duration: f64,
	/// The segment's starting presentation timestamp.
	pub pts: moq_net::Timestamp,
	/// The segment's ending presentation timestamp (`pts + duration`), for window eviction
	/// and discontinuity detection.
	pub end: Duration,
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
	/// A segment is ready to fetch, along with the segment number that follows it in the
	/// window (`None` if it's the newest row). The successor lets a cursor notice a gap: if
	/// the next segment it emits isn't this successor, rows were evicted unseen in between.
	Ready { row: Row, successor: Option<u64> },
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
			sequence: self.rows.front().map(|s| s.segment).unwrap_or(0),
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
		let mut iter = self.rows.iter().enumerate().filter(|(_, r)| match after {
			Some(after) => r.segment > after,
			None => true,
		});
		let Some((index, row)) = iter.next() else {
			return if self.ended { Next::Ended } else { Next::Pending };
		};
		Next::Ready {
			row: row.clone(),
			successor: self.rows.get(index + 1).map(|next| next.segment),
		}
	}
}

impl Producer {
	pub fn new() -> Self {
		Self {
			state: kio::Producer::new(State {
				rows: VecDeque::new(),
				ended: false,
			}),
			broadcast: OnceLock::new(),
		}
	}

	/// Append a row, evicting the front of the window past `window`.
	pub fn push(&self, row: Row, window: Duration) {
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

		state.rows.push_back(row);

		// Evict from the front while the remaining rows still cover the window.
		while state.rows.len() >= 2 {
			let span = state.rows.back().unwrap().end.saturating_sub(state.rows[1].pts.into());
			if span < window {
				break;
			}
			state.rows.pop_front();
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
			expected: None,
			gap: false,
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
	/// Presentation duration in seconds.
	pub duration: f64,
	/// Wall-clock start time, when the timeline advertises an anchor.
	pub program_date_time: Option<SystemTime>,
	/// The media timeline is broken before this segment: one or more segments were skipped
	/// since the previous one (evicted from the window before they could be fetched, or gaps
	/// with no content for this rendition). A recorder marks an `EXT-X-DISCONTINUITY` here.
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
	/// The successor segment recorded when the last one was emitted. If the next segment
	/// isn't it, rows were evicted unseen in between (a gap).
	expected: Option<u64>,
	/// A gap opened since the last emitted segment (a skip); set on the next one's discontinuity.
	gap: bool,
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
	/// groups already left the relay cache (or that is a gap for this rendition) is skipped
	/// (this resumes at the next one, flagging [`Segment::discontinuity`]) rather than
	/// surfaced as an error; a real fetch/transmux failure is returned, leaving the cursor to
	/// retry it on the next call.
	pub async fn next(&mut self) -> Result<Option<Segment>> {
		loop {
			let Some((row, successor)) = kio::wait(|waiter| self.poll_next(waiter)).await else {
				return Ok(None);
			};
			// A gap opened if the segment we're about to emit isn't the successor the previous
			// one recorded (rows between them evicted unseen).
			let gap = self.gap || self.expected.is_some_and(|expected| expected != row.segment);

			match self.rendition.segment(row.segment).await? {
				Some(media) => {
					self.after = Some(row.segment);
					self.expected = successor;
					self.gap = false;
					return Ok(Some(Segment {
						segment: row.segment,
						media,
						duration: row.duration,
						program_date_time: self.rendition.wall_clock(row.pts),
						discontinuity: gap,
					}));
				}
				// A gap for this rendition, or its groups aged out of the cache before we
				// fetched them; skip to the next and carry the gap onto whichever segment we
				// emit next.
				None => {
					self.after = Some(row.segment);
					self.expected = successor;
					self.gap = true;
				}
			}
		}
	}

	fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Option<(Row, Option<u64>)>> {
		let poll = self.state.poll(waiter, |state| match state.next_after(self.after) {
			Next::Ready { row, successor } => Poll::Ready(Some((row, successor))),
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
			segment,
			ranges: vec![Range::new(group, group)],
			duration: duration_ms as f64 / 1000.0,
			pts,
			end: Duration::from(pts) + Duration::from_millis(duration_ms),
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
		assert_eq!(window.segments[0].duration, 2.0);

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
		let span: f64 = snapshot.segments.iter().map(|s| s.duration).sum();
		assert!(span >= 4.0);
		assert_eq!(snapshot.segments.first().unwrap().segment, snapshot.sequence);
	}

	#[test]
	fn segment_ranges_and_gaps() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(row(0, 0, 0, 1_000), window);
		// Segment 1 is a gap for this rendition: no ranges.
		live.push(
			Row {
				segment: 1,
				ranges: Vec::new(),
				duration: 1.0,
				pts: moq_net::Timestamp::from_millis(1_000).unwrap(),
				end: Duration::from_millis(2_000),
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

		let first = match live.state.read().next_after(None) {
			Next::Ready { row, successor } => {
				assert_eq!(successor, Some(1));
				row
			}
			_ => panic!("expected a segment"),
		};
		assert_eq!(first.segment, 0);
		let second = match live.state.read().next_after(Some(0)) {
			Next::Ready { row, successor } => {
				assert_eq!(successor, None);
				row
			}
			_ => panic!("expected a segment"),
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
