//! Per-rendition segment/part ring buffer, aligned across renditions.
//!
//! Consumes [`moq_mux::container::fmp4::Fragment`]s from one rendition's exporter
//! and groups them into HLS segments and LL-HLS parts, keeping a bounded sliding
//! window. A [`tokio::sync::watch`] channel notifies playlist readers (blocking
//! reload) whenever a new part or segment lands.
//!
//! ## Cross-rendition alignment
//!
//! A multivariant playlist fronts one media playlist per rendition. For seeking
//! and rendition-switching to work, segment N must span ~the same time in every
//! rendition and carry the same sequence number. We get that with a shared
//! [`SegmentClock`]: the primary video store ([`Role::Leader`]) rolls on its GOP
//! keyframes (as before) and publishes each `(sequence, start)` boundary; the
//! audio (and any secondary video) stores ([`Role::Follower`]) roll and number
//! off that clock instead of an independent timer. Boundaries need not be
//! sample-exact (an audio segment opens at the first audio fragment at/after the
//! video boundary), but the numbering lines up. An audio-only broadcast has no
//! leader, so its audio store falls back to rolling on `audio_segment_target`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::{Bytes, BytesMut};
use moq_mux::container::fmp4::Fragment;
use tokio::sync::watch;

/// How many recent boundaries the clock keeps. Followers only read near the live
/// edge, so a bounded history is plenty and keeps the clock from growing.
const CLOCK_HISTORY: usize = 1024;

/// When a leader exists but has stalled (video frozen while audio keeps coming),
/// roll the follower anyway once its segment reaches this multiple of
/// `audio_segment_target`, to bound the segment. Large enough not to fire in
/// normal operation (a boundary always arrives within a GOP first).
const STALL_FACTOR: f64 = 4.0;

/// A store's role in cross-rendition alignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
	/// Primary video: rolls on keyframes and publishes boundaries to the clock.
	Leader,
	/// Audio or secondary video: rolls + numbers from the clock (audio falls back
	/// to its duration target when no leader has published).
	Follower,
}

/// One boundary on the shared timeline: segment `sequence` opened at presentation
/// time `start` (seconds).
#[derive(Clone, Copy)]
struct Boundary {
	sequence: u64,
	start: f64,
}

struct ClockInner {
	/// Ascending by `start`; pruned to the last [`CLOCK_HISTORY`] entries.
	boundaries: VecDeque<Boundary>,
	/// Set once the leader has ever published, so followers know to wait for
	/// boundaries rather than roll on their own cap.
	has_leader: bool,
	/// Set once the leader's track ends.
	finished: bool,
}

/// Shared segment-boundary timeline for one broadcast (see module docs). One
/// instance is created per [`Broadcaster`](super::Broadcaster) and shared by all
/// its renditions' stores.
pub struct SegmentClock {
	inner: Mutex<ClockInner>,
}

impl SegmentClock {
	pub fn new() -> Arc<Self> {
		Arc::new(Self {
			inner: Mutex::new(ClockInner {
				boundaries: VecDeque::new(),
				has_leader: false,
				finished: false,
			}),
		})
	}

	/// Leader: record that segment `sequence` opened at presentation time `start`.
	fn publish(&self, sequence: u64, start: f64) {
		let mut inner = self.inner.lock().unwrap();
		inner.has_leader = true;
		inner.boundaries.push_back(Boundary { sequence, start });
		while inner.boundaries.len() > CLOCK_HISTORY {
			inner.boundaries.pop_front();
		}
	}

	/// Leader: no more boundaries are coming.
	fn finish(&self) {
		self.inner.lock().unwrap().finished = true;
	}

	fn has_leader(&self) -> bool {
		self.inner.lock().unwrap().has_leader
	}

	fn is_finished(&self) -> bool {
		self.inner.lock().unwrap().finished
	}

	/// The sequence whose window contains `ts` (the largest boundary `start <= ts`),
	/// clamped to the first boundary when `ts` precedes it (so audio leading video
	/// lands in the first segment). `None` only before any boundary is published.
	fn sequence_at(&self, ts: f64) -> Option<u64> {
		let inner = self.inner.lock().unwrap();
		let mut chosen = None;
		for b in &inner.boundaries {
			if b.start <= ts {
				chosen = Some(b.sequence);
			} else {
				break;
			}
		}
		chosen.or_else(|| inner.boundaries.front().map(|b| b.sequence))
	}

	/// The first boundary after `after_start` whose `start <= ts`: the next segment
	/// a follower currently at `after_start` should roll into for a fragment at `ts`.
	fn next_boundary(&self, after_start: f64, ts: f64) -> Option<Boundary> {
		let inner = self.inner.lock().unwrap();
		inner
			.boundaries
			.iter()
			.find(|b| b.start > after_start && b.start <= ts)
			.copied()
	}
}

/// One LL-HLS partial segment: a single CMAF moof+mdat fragment.
#[derive(Clone)]
struct Part {
	data: Bytes,
	duration: f64,
	independent: bool,
}

/// One HLS media segment, made of one or more [`Part`]s.
struct Segment {
	sequence: u64,
	/// Presentation start (seconds) used as the reference for the next clock
	/// boundary; not surfaced in the playlist.
	start: f64,
	parts: Vec<Part>,
	/// Total presentation duration so far (sum of part durations).
	duration: f64,
	/// Set once the following segment opens, so EXTINF is final.
	complete: bool,
}

/// Lightweight per-part metadata for rendering a playlist (no bytes).
pub struct PartMeta {
	pub duration: f64,
	pub independent: bool,
}

/// Lightweight per-segment metadata for rendering a playlist (no bytes).
pub struct SegmentMeta {
	pub sequence: u64,
	pub parts: Vec<PartMeta>,
	pub duration: f64,
	pub complete: bool,
}

/// A point-in-time view of the store, used to render a media playlist without
/// holding the lock during formatting.
pub struct Snapshot {
	pub init_ready: bool,
	pub part_target: f64,
	pub media_sequence: u64,
	pub next_sequence: u64,
	pub segments: Vec<SegmentMeta>,
	pub finished: bool,
}

/// Watch value: enough for a blocking-reload waiter to decide if its
/// `(_HLS_msn, _HLS_part)` target has been reached, without locking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Version {
	pub last_sequence: u64,
	pub last_parts: usize,
	pub media_sequence: u64,
	pub finished: bool,
}

struct Inner {
	init: Option<Bytes>,
	segments: VecDeque<Segment>,
	next_sequence: u64,
	finished: bool,
}

/// Bounded per-rendition store of CMAF segments and LL-HLS parts.
pub struct SegmentStore {
	inner: Mutex<Inner>,
	notify: watch::Sender<Version>,
	role: Role,
	is_video: bool,
	/// The shared timeline: published to as a leader, read from as a follower.
	clock: Arc<SegmentClock>,
	/// LL-HLS PART-TARGET, in seconds.
	part_target: f64,
	/// Audio roll target: the segment duration for an audio-only broadcast, and
	/// the base for the leader-stall safety cap (`* STALL_FACTOR`).
	audio_segment_target: f64,
	/// Minimum duration (seconds) of media kept in the sliding window. The oldest
	/// segment is evicted only while the remaining ones still cover this span.
	window: f64,
}

impl SegmentStore {
	pub fn new(
		role: Role,
		is_video: bool,
		clock: Arc<SegmentClock>,
		part_target: f64,
		audio_segment_target: f64,
		window: f64,
	) -> Self {
		let (notify, _) = watch::channel(Version::default());
		Self {
			inner: Mutex::new(Inner {
				init: None,
				segments: VecDeque::new(),
				next_sequence: 0,
				finished: false,
			}),
			notify,
			role,
			is_video,
			clock,
			part_target,
			audio_segment_target,
			window,
		}
	}

	/// Apply one exported fragment. The init fragment sets the init segment;
	/// media fragments append a part (rolling a new segment per the policy).
	pub fn push(&self, fragment: Fragment) {
		if fragment.init {
			self.inner.lock().unwrap().init = Some(fragment.data);
			self.bump();
			return;
		}

		let ts = fragment.timestamp;
		{
			let mut inner = self.inner.lock().unwrap();

			// Decide whether to open a new segment, and with which (sequence, start).
			let open = self.next_segment(&inner, &fragment, ts);

			if let Some((sequence, start)) = open {
				if let Some(cur) = inner.segments.back_mut() {
					cur.complete = true;
				}
				inner.segments.push_back(Segment {
					sequence,
					start,
					parts: Vec::new(),
					duration: 0.0,
					complete: false,
				});
				inner.next_sequence = sequence + 1;
				// The leader publishes the boundary it just opened so followers can
				// align. Safe to lock the (leaf) clock while holding `inner`.
				if self.role == Role::Leader {
					self.clock.publish(sequence, start);
				}
			}

			let cur = inner.segments.back_mut().expect("segment present after open");
			cur.duration += fragment.duration;
			cur.parts.push(Part {
				data: fragment.data,
				duration: fragment.duration,
				independent: fragment.independent,
			});

			// Evict from the front while the newer segments still cover the window.
			// Always keep the in-progress segment, so never drop below one.
			while inner.segments.len() > 1 {
				let total: f64 = inner.segments.iter().map(|s| s.duration).sum();
				let oldest = inner.segments.front().expect("segments non-empty").duration;
				if total - oldest >= self.window {
					inner.segments.pop_front();
				} else {
					break;
				}
			}
		}

		self.bump();
	}

	/// Decide whether this fragment opens a new segment, and with which
	/// `(sequence, start)`. See the module docs for the alignment rules.
	fn next_segment(&self, inner: &Inner, fragment: &Fragment, ts: f64) -> Option<(u64, f64)> {
		match self.role {
			// Leader (primary video): roll on each GOP keyframe, like classic
			// per-GOP segmentation. Sequence is this store's own counter.
			Role::Leader => {
				let new = inner.segments.back().map(|_| fragment.independent).unwrap_or(true);
				new.then(|| (inner.next_sequence, ts))
			}
			Role::Follower => match inner.segments.back() {
				// First segment: adopt the clock's sequence for this time if a
				// leader exists, else start at 0 (audio-only / leader not seen yet).
				None => Some((self.clock.sequence_at(ts).unwrap_or(0), ts)),
				Some(cur) => {
					if self.is_video {
						// Secondary video: keep starting segments on its own keyframe
						// (HLS needs that), but number from the clock so an aligned
						// ABR ladder stays in lockstep.
						if fragment.independent {
							let seq = self.clock.sequence_at(ts).unwrap_or(cur.sequence + 1);
							(seq > cur.sequence).then_some((seq, ts))
						} else {
							None
						}
					} else if let Some(b) = self.clock.next_boundary(cur.start, ts) {
						// Crossed into the next leader segment: roll + adopt its number.
						Some((b.sequence, b.start))
					} else if !self.clock.has_leader() {
						// Audio-only: roll on the duration target, numbered sequentially.
						(cur.duration >= self.audio_segment_target).then_some((cur.sequence + 1, ts))
					} else if !self.clock.is_finished() && cur.duration >= self.audio_segment_target * STALL_FACTOR {
						// Safety net: leader stalled but audio keeps coming. Bound the
						// segment rather than grow it forever (numbers may skip).
						Some((cur.sequence + 1, ts))
					} else {
						None
					}
				}
			},
		}
	}

	/// Signal end-of-track. The playlist gains `#EXT-X-ENDLIST`.
	pub fn finish(&self) {
		{
			let mut inner = self.inner.lock().unwrap();
			if let Some(cur) = inner.segments.back_mut() {
				cur.complete = true;
			}
			inner.finished = true;
		}
		// Let followers stop waiting on a leader that's done.
		if self.role == Role::Leader {
			self.clock.finish();
		}
		self.bump();
	}

	fn bump(&self) {
		let version = self.version();
		// Ignore send errors: no receivers just means nobody is waiting yet.
		let _ = self.notify.send(version);
	}

	pub fn version(&self) -> Version {
		let inner = self.inner.lock().unwrap();
		let media_sequence = inner
			.segments
			.front()
			.map(|s| s.sequence)
			.unwrap_or(inner.next_sequence);
		match inner.segments.back() {
			Some(last) => Version {
				last_sequence: last.sequence,
				last_parts: last.parts.len(),
				media_sequence,
				finished: inner.finished,
			},
			None => Version {
				last_sequence: inner.next_sequence,
				last_parts: 0,
				media_sequence,
				finished: inner.finished,
			},
		}
	}

	pub fn subscribe(&self) -> watch::Receiver<Version> {
		self.notify.subscribe()
	}

	pub fn init(&self) -> Option<Bytes> {
		self.inner.lock().unwrap().init.clone()
	}

	/// The bytes of one part (`part/<sequence>/<index>.m4s`).
	pub fn part(&self, sequence: u64, index: usize) -> Option<Bytes> {
		let inner = self.inner.lock().unwrap();
		let segment = inner.segments.iter().find(|s| s.sequence == sequence)?;
		segment.parts.get(index).map(|p| p.data.clone())
	}

	/// The bytes of a full segment (`seg/<sequence>.m4s`): its parts concatenated.
	pub fn segment(&self, sequence: u64) -> Option<Bytes> {
		let inner = self.inner.lock().unwrap();
		let segment = inner.segments.iter().find(|s| s.sequence == sequence)?;
		let mut buf = BytesMut::new();
		for part in &segment.parts {
			buf.extend_from_slice(&part.data);
		}
		Some(buf.freeze())
	}

	/// True once the store holds the `(sequence, part)` the caller asked for, the
	/// window has already advanced past it, or the track has ended. Used to decide
	/// whether a blocking-reload request can be answered now.
	pub fn satisfies(&self, sequence: u64, part: usize) -> bool {
		let inner = self.inner.lock().unwrap();
		if inner.finished {
			return true;
		}
		let media_sequence = inner.segments.front().map(|s| s.sequence).unwrap_or(0);
		if sequence < media_sequence {
			return true; // already rolled past; the playlist no longer carries it
		}
		match inner.segments.iter().find(|s| s.sequence == sequence) {
			Some(segment) => segment.parts.len() > part || segment.complete,
			None => false,
		}
	}

	/// Capture a lock-free view for rendering a media playlist.
	pub fn snapshot(&self) -> Snapshot {
		let inner = self.inner.lock().unwrap();
		let media_sequence = inner
			.segments
			.front()
			.map(|s| s.sequence)
			.unwrap_or(inner.next_sequence);
		let segments = inner
			.segments
			.iter()
			.map(|s| SegmentMeta {
				sequence: s.sequence,
				parts: s
					.parts
					.iter()
					.map(|p| PartMeta {
						duration: p.duration,
						independent: p.independent,
					})
					.collect(),
				duration: s.duration,
				complete: s.complete,
			})
			.collect();
		Snapshot {
			init_ready: inner.init.is_some(),
			part_target: self.part_target,
			media_sequence,
			next_sequence: inner.next_sequence,
			segments,
			finished: inner.finished,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn media(ts: f64, duration: f64, independent: bool) -> Fragment {
		Fragment {
			data: Bytes::from_static(b"x"),
			init: false,
			independent,
			duration,
			timestamp: ts,
		}
	}

	fn seqs(store: &SegmentStore) -> Vec<u64> {
		store.snapshot().segments.iter().map(|s| s.sequence).collect()
	}

	/// Leader rolls a new segment on each keyframe, numbered 0,1,2,...
	#[test]
	fn leader_rolls_on_keyframe() {
		let clock = SegmentClock::new();
		let video = SegmentStore::new(Role::Leader, true, clock, 0.5, 2.0, 60.0);
		// GOP 0: keyframe + delta. GOP 1: keyframe + delta. GOP 2: keyframe.
		video.push(media(0.0, 1.0, true));
		video.push(media(1.0, 1.0, false));
		video.push(media(2.0, 1.0, true));
		video.push(media(3.0, 1.0, false));
		video.push(media(4.0, 1.0, true));
		assert_eq!(seqs(&video), vec![0, 1, 2]);
	}

	/// The core requirement: audio follows the video boundaries and shares the
	/// same segment numbers, even though audio fragments are smaller/more frequent.
	#[test]
	fn audio_follows_video_and_shares_numbers() {
		let clock = SegmentClock::new();
		let video = SegmentStore::new(Role::Leader, true, clock.clone(), 0.5, 2.0, 60.0);
		let audio = SegmentStore::new(Role::Follower, false, clock, 0.5, 2.0, 60.0);

		// Video GOPs of 2s: keyframes at 0 and 2 (publishing boundaries 0 and 2s).
		video.push(media(0.0, 2.0, true)); // opens seg 0 @ 0.0
		// Audio fragments inside GOP 0 (ts < 2.0) -> seg 0.
		audio.push(media(0.0, 0.5, true));
		audio.push(media(0.5, 0.5, true));
		audio.push(media(1.0, 0.5, true));
		audio.push(media(1.5, 0.5, true));
		// Video rolls at 2.0 (publishes boundary seq 1 @ 2.0).
		video.push(media(2.0, 2.0, true)); // opens seg 1 @ 2.0
		// Audio crossing 2.0 rolls into seg 1, adopting the leader's number.
		audio.push(media(2.0, 0.5, true));
		audio.push(media(2.5, 0.5, true));

		assert_eq!(seqs(&video), vec![0, 1], "video numbered by GOP");
		assert_eq!(seqs(&audio), vec![0, 1], "audio shares the video numbering");

		// Audio seg 0 holds the four pre-boundary fragments; seg 1 the two after.
		let snap = audio.snapshot();
		assert_eq!(snap.segments[0].parts.len(), 4);
		assert_eq!(snap.segments[1].parts.len(), 2);
	}

	/// Audio that briefly outruns the leader stays in the current segment, then
	/// rolls once the boundary is published (no perfect timestamp alignment, but
	/// the numbers still line up).
	#[test]
	fn audio_ahead_of_leader_waits_for_boundary() {
		let clock = SegmentClock::new();
		let video = SegmentStore::new(Role::Leader, true, clock.clone(), 0.5, 2.0, 60.0);
		let audio = SegmentStore::new(Role::Follower, false, clock, 0.5, 2.0, 60.0);

		video.push(media(0.0, 2.0, true)); // boundary 0 @ 0.0
		audio.push(media(0.0, 0.5, true)); // seg 0
		// Audio reaches 2.0 before the leader has rolled its second GOP: stays in 0.
		audio.push(media(2.0, 0.5, true));
		assert_eq!(seqs(&audio), vec![0]);
		// Leader rolls; the next audio fragment crosses into seg 1.
		video.push(media(2.0, 2.0, true)); // boundary 1 @ 2.0
		audio.push(media(2.5, 0.5, true));
		assert_eq!(seqs(&audio), vec![0, 1]);
	}

	/// With no video (audio-only), the audio store has no leader and falls back to
	/// rolling on `audio_segment_target`.
	#[test]
	fn audio_only_rolls_on_target() {
		let clock = SegmentClock::new();
		let audio = SegmentStore::new(Role::Follower, false, clock, 0.5, 2.0, 60.0);
		// 0.5s parts; a new segment every >= 2.0s of accumulated duration.
		for i in 0..8 {
			audio.push(media(i as f64 * 0.5, 0.5, true));
		}
		// 4s of audio in 0.5s parts -> segments 0 and 1 (then 2 opening).
		assert!(seqs(&audio).len() >= 2, "audio-only must still segment");
		assert_eq!(seqs(&audio)[0], 0);
	}

	/// Old segments are evicted once the newer ones still cover the window.
	#[test]
	fn evicts_past_window() {
		let clock = SegmentClock::new();
		let video = SegmentStore::new(Role::Leader, true, clock, 0.5, 2.0, 4.0);
		// Ten 1s GOPs; window is 4s, so only a handful of recent segments remain.
		for i in 0..10 {
			video.push(media(i as f64, 1.0, true));
		}
		let snap = video.snapshot();
		assert!(snap.segments.len() <= 6, "window should bound retained segments");
		// media_sequence advanced past the evicted front.
		assert!(snap.media_sequence > 0);
	}
}
