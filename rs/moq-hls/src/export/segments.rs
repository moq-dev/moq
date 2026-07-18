//! One rendition's segment timeline, as a `Producer`/[`Consumer`] pair.
//!
//! The `Producer` is fed [`moq_mux::timeline::Entry`]s by a background task as the publisher
//! indexes new groups; it keeps a bounded window of them. Two things read that window:
//!
//! * the HTTP serve path, synchronously, to render a media playlist and look up a segment's
//!   group span (nothing here touches media bytes on that path); and
//! * a [`Consumer`] cursor, for a recorder that wants every finalized segment *with its media*,
//!   in order, exactly once. `next()` waits for the next segment to finalize, FETCHes and
//!   transmuxes its groups (via [`Rendition`]), and yields the CMAF bytes.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::task::Poll;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use moq_mux::timeline::Entry;

use super::Rendition;
use crate::Result;

/// Duration assumed for the live-edge segment of an ended timeline before any complete
/// segment has revealed the real cadence.
const DEFAULT_SEGMENT: Duration = Duration::from_secs(4);

/// The producing side of a rendition's timeline window.
///
/// A background task appends timeline records via [`push`](Self::push) and marks the stream
/// [`end`](Self::end)ed. Cheap to share behind an `Arc`; the window state lives in a
/// [`kio::Producer`] so a [`Consumer`] can await changes without a separate signal.
pub(crate) struct Producer {
	state: kio::Producer<State>,
	/// The broadcast serving the media track, resolved once by the background task (it may be
	/// a sibling broadcast when the catalog rendition carries a `broadcast` reference).
	pub broadcast: OnceLock<moq_net::broadcast::Consumer>,
}

struct State {
	/// Timeline records within the window, oldest first. Consecutive records bound a segment:
	/// record `i` starts it, record `i + 1` supplies its duration, so the last record is the
	/// live edge (its segment is still growing).
	entries: VecDeque<Entry>,
	/// Records evicted from the front of the window since subscribing; the playlist's
	/// `EXT-X-MEDIA-SEQUENCE` base, so segment positions stay stable across reloads.
	dropped: u64,
	/// The timeline track ended: the broadcast is over and the last record is a full segment.
	ended: bool,
}

/// One playlist segment: a starting group and the media it covers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
	/// The first group of the segment (its URI: `seg/{group}.m4s`). The segment covers every
	/// group up to (excluding) the next segment's `group`.
	pub group: u64,
	/// Presentation duration in seconds (the gap to the next timeline record).
	pub duration: f64,
	/// The segment's starting presentation timestamp.
	pub pts: moq_net::Timestamp,
}

/// A consistent read of the window, for rendering one playlist (the serve path only).
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) struct Window {
	/// The `EXT-X-MEDIA-SEQUENCE` of the first listed segment.
	pub sequence: u64,
	/// Complete segments, oldest first.
	pub segments: Vec<Row>,
	/// Whether the timeline (and so the playlist) has ended.
	pub ended: bool,
}

/// The next finalized segment a [`Consumer`] should emit, resolved from the window.
enum Next {
	/// A finalized segment is ready to fetch, along with the group that follows it in the
	/// window (`None` if it's the last / ended-final record). The successor lets a cursor
	/// notice a gap: if the next segment it emits isn't this successor, records were evicted
	/// unseen in between.
	Ready { row: Row, successor: Option<u64> },
	/// No further segment will ever appear (the timeline ended).
	Ended,
	/// Nothing new yet; wait for the next window change.
	Pending,
}

impl State {
	/// Snapshot the current window.
	///
	/// A segment needs the next record for its duration, so the live-edge record isn't
	/// listed; once the timeline ends it becomes the final segment, whose duration is
	/// estimated as the largest gap seen in the window (falling back to
	/// [`DEFAULT_SEGMENT`] before any complete segment exists).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	fn window(&self) -> Window {
		let mut segments = Vec::with_capacity(self.entries.len());

		for (entry, next) in self.entries.iter().zip(self.entries.iter().skip(1)) {
			segments.push(row(entry, Some(next)));
		}

		// The final (open-ended) segment has no successor record to time it.
		if self.ended
			&& let Some(last) = self.entries.back()
		{
			segments.push(Row {
				group: last.group,
				duration: self.final_duration(),
				pts: last.pts,
			});
		}

		Window {
			sequence: self.dropped,
			segments,
			ended: self.ended,
		}
	}

	/// The duration to advertise for the final (open-ended) segment of an ended timeline, which
	/// has no successor record to time it: the largest gap seen in the window, floored at
	/// [`DEFAULT_SEGMENT`] before any complete segment has revealed the real cadence. Shared by
	/// the serve path and the recording cursor so both time that segment identically.
	fn final_duration(&self) -> f64 {
		self.entries
			.iter()
			.zip(self.entries.iter().skip(1))
			.map(|(entry, next)| {
				Duration::from(next.pts)
					.saturating_sub(Duration::from(entry.pts))
					.as_secs_f64()
			})
			.fold(0.0f64, f64::max)
			.max(DEFAULT_SEGMENT.as_secs_f64())
	}

	/// The first finalized segment starting past `after`, for a cursor.
	///
	/// A record is finalized once a later record bounds its duration, or the timeline has
	/// ended (making the live-edge record the final segment). Segments evicted from the front
	/// of the window before the cursor reached them are skipped: the cursor resumes at the
	/// oldest record still in the window.
	fn next_after(&self, after: Option<u64>) -> Next {
		let mut iter = self.entries.iter().enumerate().filter(|(_, e)| match after {
			Some(after) => e.group > after,
			None => true,
		});
		let Some((index, entry)) = iter.next() else {
			return if self.ended { Next::Ended } else { Next::Pending };
		};
		match self.entries.get(index + 1) {
			// Bounded by the next record: finalized, and that record is its successor.
			Some(next) => Next::Ready {
				row: row(entry, Some(next)),
				successor: Some(next.group),
			},
			// The live-edge record is only a segment once the timeline ended; nothing follows it,
			// so it takes the same estimated duration the serve path advertises for it.
			None if self.ended => Next::Ready {
				row: Row {
					group: entry.group,
					duration: self.final_duration(),
					pts: entry.pts,
				},
				successor: None,
			},
			None => Next::Pending,
		}
	}
}

impl Producer {
	pub fn new() -> Self {
		Self {
			state: kio::Producer::new(State {
				entries: VecDeque::new(),
				dropped: 0,
				ended: false,
			}),
			broadcast: OnceLock::new(),
		}
	}

	/// Append a timeline record, evicting the front of the window past `window`.
	pub fn push(&self, entry: Entry, window: Duration) {
		let Ok(mut state) = self.state.write() else {
			return;
		};

		if let Some(back_pts) = state.entries.back().map(|e| e.pts) {
			// A non-advancing record (a duplicate, or a stalled/mis-scaled source) would open a
			// zero-duration segment and never trigger eviction; drop it so the prior segment just
			// covers its groups too.
			if entry.pts == back_pts {
				return;
			}
			// A backward jump means the publisher restarted its timeline; the old window can't be
			// stitched onto the new one, so start over (keeping the sequence monotonic).
			if entry.pts < back_pts {
				tracing::warn!("timeline jumped backwards; resetting the playlist window");
				state.dropped += state.entries.len() as u64;
				state.entries.clear();
			}
		}

		state.entries.push_back(entry);

		// Evict from the front while the *listed* segments (which start at the second entry
		// once the first is dropped) still cover the window.
		while state.entries.len() >= 3 {
			let span =
				Duration::from(state.entries.back().unwrap().pts).saturating_sub(Duration::from(state.entries[1].pts));
			if span < window {
				break;
			}
			state.entries.pop_front();
			state.dropped += 1;
		}
	}

	/// Mark the timeline ended (the broadcast finished cleanly): the live-edge record becomes
	/// the final segment.
	pub fn end(&self) {
		if let Ok(mut state) = self.state.write() {
			state.ended = true;
		}
	}

	/// Close the channel: no more records will arrive. A [`Consumer`] drains the segments it
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

	/// The groups covered by the segment starting at `group`, or `None` if it isn't in the
	/// window. An open end means "until the track ends" (the final segment of an ended
	/// timeline).
	pub fn segment_groups(&self, group: u64) -> Option<(u64, Option<u64>)> {
		let state = self.state.read();
		let index = state.entries.iter().position(|e| e.group == group)?;
		match state.entries.get(index + 1) {
			Some(next) => Some((group, Some(next.group))),
			None if state.ended => Some((group, None)),
			None => None,
		}
	}

	/// The newest complete segment's starting group, used to bootstrap an init segment for
	/// inline-parameter-set codecs.
	pub fn latest_group(&self) -> Option<u64> {
		let state = self.state.read();
		if state.ended {
			return state.entries.back().map(|e| e.group);
		}
		// The back entry is the live edge; the one before it starts a complete segment.
		state.entries.iter().rev().nth(1).map(|e| e.group)
	}

	/// Whether the playlist has anything to serve yet (at least one complete segment, or the
	/// broadcast already ended).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub fn is_playable(&self) -> bool {
		let state = self.state.read();
		state.ended || state.entries.len() >= 2
	}

	/// Poll until [`is_playable`](Self::is_playable), for the serve path's long-poll.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub fn poll_playable(&self, waiter: &kio::Waiter) -> Poll<()> {
		let poll = self.state.poll_ref(waiter, |state| {
			if state.ended || state.entries.len() >= 2 {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		});
		match poll {
			// Ready, or the channel closed (no more records will arrive): stop waiting either way.
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// A cursor over finalized segments, starting from the oldest still in the window.
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

/// A finalized segment with its transmuxed media, yielded by a [`Consumer`].
pub struct Segment {
	/// The segment's starting group (also its `seg/{group}.m4s` URI stem).
	pub group: u64,
	/// The transmuxed CMAF fragment (`moof`+`mdat`), fetched on demand by [`Consumer::next`].
	pub media: Bytes,
	/// Presentation duration in seconds.
	pub duration: f64,
	/// Wall-clock start time, when the timeline advertises an anchor.
	pub program_date_time: Option<SystemTime>,
	/// The media timeline is broken before this segment: one or more finalized segments were
	/// skipped (evicted from the window before they could be fetched) since the previous one.
	/// A recorder marks an `EXT-X-DISCONTINUITY` here.
	pub discontinuity: bool,
}

/// A cursor over one rendition's finalized segments, in timeline order.
///
/// Obtained from [`Rendition::segments`](super::Rendition::segments). Drives the same
/// fetch-on-demand path the HTTP serve path uses, so it adds no standing traffic: each
/// [`next`](Self::next) awaits the next segment to finalize, then FETCHes and transmuxes it.
pub struct Consumer {
	state: kio::Consumer<State>,
	rendition: Arc<Rendition>,
	/// The group of the last segment returned; the next call resumes strictly after it. Only
	/// advanced once a segment is fetched or skipped, so a transient fetch error re-tries the
	/// same segment on the next call instead of losing it.
	after: Option<u64>,
	/// The successor group recorded when the last segment was emitted. If the next segment
	/// isn't it, records were evicted unseen in between (a gap).
	expected: Option<u64>,
	/// A gap opened since the last emitted segment (a skip); set on the next one's discontinuity.
	gap: bool,
}

impl Consumer {
	/// The rendition's CMAF init segment, built once and cached; `None` until it can be built
	/// (an inline-parameter-set codec needs the first complete segment first).
	pub async fn init(&self) -> Result<Option<Bytes>> {
		self.rendition.init().await
	}

	/// The next finalized segment, with its media; `None` once the rendition ends.
	///
	/// Waits for the next segment to finalize, then FETCHes and transmuxes its groups. A
	/// segment whose groups already left the relay cache is skipped (this resumes at the next
	/// one, flagging [`Segment::discontinuity`]) rather than surfaced as an error; a real
	/// fetch/transmux failure is returned, leaving the cursor to retry it on the next call.
	pub async fn next(&mut self) -> Result<Option<Segment>> {
		loop {
			let Some((row, successor)) = kio::wait(|waiter| self.poll_next(waiter)).await else {
				return Ok(None);
			};
			// A gap opened if the segment we're about to emit isn't the successor the previous
			// one recorded (records between them evicted unseen).
			let gap = self.gap || self.expected.is_some_and(|expected| expected != row.group);

			match self.rendition.segment(row.group).await? {
				Some(media) => {
					self.after = Some(row.group);
					self.expected = successor;
					self.gap = false;
					return Ok(Some(Segment {
						group: row.group,
						media,
						duration: row.duration,
						program_date_time: self.rendition.wall_clock(row.pts),
						discontinuity: gap,
					}));
				}
				// Its groups aged out of the cache before we fetched them; skip to the next and
				// carry the gap onto whichever segment we emit next.
				None => {
					self.after = Some(row.group);
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

fn row(entry: &Entry, next: Option<&Entry>) -> Row {
	let duration = next
		.map(|next| Duration::from(next.pts).saturating_sub(Duration::from(entry.pts)))
		.unwrap_or(DEFAULT_SEGMENT);
	Row {
		group: entry.group,
		duration: duration.as_secs_f64(),
		pts: entry.pts,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn entry(group: u64, pts_ms: u64) -> Entry {
		Entry {
			group,
			pts: moq_net::Timestamp::from_millis(pts_ms).unwrap(),
			ext: (),
		}
	}

	#[test]
	fn last_record_is_the_live_edge_until_ended() {
		let live = Producer::new();
		live.push(entry(0, 0), Duration::from_secs(30));
		live.push(entry(1, 2_000), Duration::from_secs(30));
		live.push(entry(2, 4_000), Duration::from_secs(30));

		let window = live.window();
		assert_eq!(window.sequence, 0);
		assert!(!window.ended);
		assert_eq!(window.segments.len(), 2, "the live-edge record is not listed");
		assert_eq!(window.segments[0].group, 0);
		assert_eq!(window.segments[0].duration, 2.0);
		assert_eq!(window.segments[1].group, 1);

		live.end();
		let window = live.window();
		assert!(window.ended);
		assert_eq!(window.segments.len(), 3, "ending lists the final record");
		assert_eq!(window.segments[2].duration, 4.0, "final duration falls back");
	}

	#[test]
	fn window_evicts_and_advances_sequence() {
		let live = Producer::new();
		let window = Duration::from_secs(4);
		for i in 0..6u64 {
			live.push(entry(i, i * 2_000), window);
		}

		let snapshot = live.window();
		// Segments still cover >= 4s after eviction, and the sequence counts the drops.
		assert!(snapshot.sequence > 0);
		let span: f64 = snapshot.segments.iter().map(|s| s.duration).sum();
		assert!(span >= 4.0);
		assert_eq!(snapshot.segments.first().unwrap().group, snapshot.sequence);
	}

	#[test]
	fn segment_groups_cover_record_gaps() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		// An audio-style timeline: records skip groups (granularity throttling).
		live.push(entry(0, 0), window);
		live.push(entry(50, 1_000), window);
		live.push(entry(100, 2_000), window);

		assert_eq!(live.segment_groups(0), Some((0, Some(50))));
		assert_eq!(live.segment_groups(50), Some((50, Some(100))));
		// The live edge isn't a segment yet, and unknown groups miss.
		assert_eq!(live.segment_groups(100), None);
		assert_eq!(live.segment_groups(7), None);

		live.end();
		assert_eq!(live.segment_groups(100), Some((100, None)));
	}

	#[test]
	fn backwards_jump_resets_the_window() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(entry(0, 10_000), window);
		live.push(entry(1, 12_000), window);
		live.push(entry(2, 1_000), window); // restart

		let snapshot = live.window();
		assert_eq!(snapshot.sequence, 2, "reset keeps the media sequence monotonic");
		assert!(snapshot.segments.is_empty(), "only the live edge remains");
	}

	#[test]
	fn non_advancing_record_is_skipped() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(entry(0, 0), window);
		live.push(entry(1, 2_000), window);
		// A record whose pts equals the live edge would open a zero-duration segment; it's dropped
		// so the prior segment just covers its groups.
		live.push(entry(2, 2_000), window);
		live.push(entry(3, 4_000), window);

		live.end();
		let window = live.window();
		let durations: Vec<f64> = window.segments.iter().map(|s| s.duration).collect();
		assert!(!durations.contains(&0.0), "no zero-duration segment: {durations:?}");
		// Group 2 was absorbed into group 1's segment (still bounded by group 3's record).
		assert_eq!(live.segment_groups(1), Some((1, Some(3))));
		assert_eq!(live.segment_groups(2), None, "the skipped record is not a boundary");
	}

	// `end()` is what promotes the live-edge record into the final segment, and it is a no-op
	// once the channel is closed. So anything retiring a rendition must NOT force-close a
	// timeline that is about to end on its own, or that last segment is silently lost -- which
	// is why `renditions::Producer::clear` drops its renditions rather than closing them.
	#[test]
	fn closing_before_end_loses_the_final_segment() {
		let window = Duration::from_secs(30);

		// The watcher's own order: end() then close() -- the live edge finalizes.
		let clean = Producer::new();
		clean.push(entry(0, 0), window);
		clean.push(entry(1, 2_000), window);
		clean.end();
		clean.close();
		assert!(
			matches!(clean.state.read().next_after(Some(0)), Next::Ready { .. }),
			"end() before close() finalizes the live edge"
		);

		// Reversed: a close() that races ahead of end() strands it forever.
		let raced = Producer::new();
		raced.push(entry(0, 0), window);
		raced.push(entry(1, 2_000), window);
		raced.close();
		raced.end();
		assert!(
			matches!(raced.state.read().next_after(Some(0)), Next::Pending),
			"end() is a no-op after close(), so the final segment never finalizes"
		);
	}

	// The cursor and the serve path must time the final (open-ended) segment identically:
	// estimated from the observed cadence, not a flat DEFAULT_SEGMENT.
	#[test]
	fn final_segment_duration_matches_the_serve_path() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		// A 6s cadence, so the flat 4s DEFAULT_SEGMENT would be wrong for the last segment.
		live.push(entry(0, 0), window);
		live.push(entry(1, 6_000), window);
		live.push(entry(2, 12_000), window);
		live.end();

		let served = live.window();
		let last_served = served.segments.last().expect("a final segment");
		assert_eq!(last_served.group, 2);
		assert_eq!(last_served.duration, 6.0, "serve path estimates from the cadence");

		let last_recorded = match live.state.read().next_after(Some(1)) {
			Next::Ready { row, .. } => row,
			_ => panic!("ending finalizes the live edge"),
		};
		assert_eq!(last_recorded.group, 2);
		assert_eq!(
			last_recorded.duration, last_served.duration,
			"the cursor times the final segment like the serve path"
		);
	}

	#[test]
	fn next_after_walks_finalized_segments() {
		let live = Producer::new();
		let window = Duration::from_secs(30);
		live.push(entry(0, 0), window);
		live.push(entry(1, 2_000), window);
		live.push(entry(2, 4_000), window);

		// Groups 0 and 1 are finalized; group 2 is the live edge.
		let first = match live.state.read().next_after(None) {
			Next::Ready { row, .. } => row,
			_ => panic!("expected a finalized segment"),
		};
		assert_eq!(first.group, 0);
		let second = match live.state.read().next_after(Some(0)) {
			Next::Ready { row, .. } => row,
			_ => panic!("expected a finalized segment"),
		};
		assert_eq!(second.group, 1);
		assert!(
			matches!(live.state.read().next_after(Some(1)), Next::Pending),
			"the live edge is not finalized while live"
		);

		live.end();
		let last = match live.state.read().next_after(Some(1)) {
			Next::Ready { row, .. } => row,
			_ => panic!("ending finalizes the live edge"),
		};
		assert_eq!(last.group, 2);
		assert!(matches!(live.state.read().next_after(Some(2)), Next::Ended));
	}
}
