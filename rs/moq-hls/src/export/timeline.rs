//! The windowed view of one rendition's timeline track.
//!
//! A background task appends decoded [`moq_mux::timeline::Entry`]s as the publisher indexes
//! new groups; playlist rendering and segment lookup read a bounded window of them. Nothing
//! here touches media bytes.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use moq_mux::timeline::Entry;
use tokio::sync::watch;

/// Duration assumed for the live-edge segment of an ended timeline before any complete
/// segment has revealed the real cadence.
const DEFAULT_SEGMENT: Duration = Duration::from_secs(4);

/// A rendition's live state, shared between its [`Rendition`](super::Rendition) handle and
/// the background task feeding it timeline records.
pub(crate) struct Live {
	state: Mutex<State>,
	/// Bumped on every timeline change so playlist requests can wait for the next record.
	updated: watch::Sender<u64>,
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
pub(crate) struct Segment {
	/// The first group of the segment (its URI: `seg/{group}.m4s`). The segment covers every
	/// group up to (excluding) the next segment's `group`.
	pub group: u64,
	/// Presentation duration in seconds (the gap to the next timeline record).
	pub duration: f64,
	/// The segment's starting presentation timestamp.
	pub pts: moq_net::Timestamp,
}

/// A consistent read of the window, for rendering one playlist.
pub(crate) struct Window {
	/// The `EXT-X-MEDIA-SEQUENCE` of the first listed segment.
	pub sequence: u64,
	/// Complete segments, oldest first.
	pub segments: Vec<Segment>,
	/// Whether the timeline (and so the playlist) has ended.
	pub ended: bool,
}

impl Live {
	pub fn new() -> Self {
		Self {
			state: Mutex::new(State {
				entries: VecDeque::new(),
				dropped: 0,
				ended: false,
			}),
			updated: watch::channel(0).0,
			broadcast: OnceLock::new(),
		}
	}

	/// Append a timeline record, evicting the front of the window past `window`.
	pub fn push(&self, entry: Entry, window: Duration) {
		let mut state = self.state.lock().unwrap();

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

		drop(state);
		self.bump();
	}

	/// Mark the timeline ended (the broadcast is over).
	pub fn end(&self) {
		self.state.lock().unwrap().ended = true;
		self.bump();
	}

	fn bump(&self) {
		self.updated.send_modify(|v| *v += 1);
	}

	/// Subscribe to window changes (any push or end bumps the value).
	pub fn subscribe(&self) -> watch::Receiver<u64> {
		self.updated.subscribe()
	}

	/// Snapshot the current window.
	///
	/// A segment needs the next record for its duration, so the live-edge record isn't
	/// listed; once the timeline ends it becomes the final segment, whose duration is
	/// estimated as the largest gap seen in the window (falling back to
	/// [`DEFAULT_SEGMENT`] before any complete segment exists).
	pub fn window(&self) -> Window {
		let state = self.state.lock().unwrap();
		let mut segments = Vec::with_capacity(state.entries.len());

		for (entry, next) in state.entries.iter().zip(state.entries.iter().skip(1)) {
			segments.push(segment(entry, Some(next)));
		}

		// The final (open-ended) segment has no successor record to time it; guess from the
		// largest complete-segment gap seen so far.
		if state.ended
			&& let Some(last) = state.entries.back()
		{
			let fallback = segments
				.iter()
				.map(|s| s.duration)
				.fold(0.0f64, f64::max)
				.max(DEFAULT_SEGMENT.as_secs_f64());
			segments.push(Segment {
				group: last.group,
				duration: fallback,
				pts: last.pts,
			});
		}

		Window {
			sequence: state.dropped,
			segments,
			ended: state.ended,
		}
	}

	/// The groups covered by the segment starting at `group`, or `None` if it isn't in the
	/// window. An open end means "until the track ends" (the final segment of an ended
	/// timeline).
	pub fn segment_groups(&self, group: u64) -> Option<(u64, Option<u64>)> {
		let state = self.state.lock().unwrap();
		let index = state.entries.iter().position(|e| e.group == group)?;
		match state.entries.get(index + 1) {
			Some(next) => Some((group, Some(next.group))),
			// The live-edge record is only a segment once the timeline ended.
			None if state.ended => Some((group, None)),
			None => None,
		}
	}

	/// The newest complete segment's starting group, used to bootstrap an init segment for
	/// inline-parameter-set codecs.
	pub fn latest_group(&self) -> Option<u64> {
		let state = self.state.lock().unwrap();
		if state.ended {
			return state.entries.back().map(|e| e.group);
		}
		// The back entry is the live edge; the one before it starts a complete segment.
		state.entries.iter().rev().nth(1).map(|e| e.group)
	}
}

fn segment(entry: &Entry, next: Option<&Entry>) -> Segment {
	let duration = next
		.map(|next| Duration::from(next.pts).saturating_sub(Duration::from(entry.pts)))
		.unwrap_or(DEFAULT_SEGMENT);
	Segment {
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
		let live = Live::new();
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
		let live = Live::new();
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
		let live = Live::new();
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
		let live = Live::new();
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
		let live = Live::new();
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
}
