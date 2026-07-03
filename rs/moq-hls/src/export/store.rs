//! Per-rendition segment/part ring buffer.
//!
//! Consumes [`moq_mux::container::fmp4::Fragment`]s from one rendition's exporter
//! and groups them into HLS segments and LL-HLS parts, keeping a bounded sliding
//! window. A [`tokio::sync::watch`] channel notifies playlist readers (blocking
//! reload) whenever a new part or segment lands.

use std::collections::VecDeque;
use std::sync::Mutex;

use bytes::{Bytes, BytesMut};
use moq_mux::container::fmp4::Fragment;
use tokio::sync::watch;

use super::{Config, Kind};

/// One LL-HLS partial segment: a single CMAF moof+mdat fragment.
#[derive(Clone)]
struct Part {
	data: Bytes,
	duration: f64,
	independent: bool,
}

/// One HLS media segment, made of one or more [`Part`]s. For video a segment is
/// a GOP (rolls on an independent fragment); for audio it accumulates parts up to
/// a target duration.
struct Segment {
	sequence: u64,
	parts: Vec<Part>,
	/// Total presentation duration so far (sum of part durations).
	duration: f64,
	/// Set once the following segment opens, so EXTINF is final.
	complete: bool,
	/// First segment after a pause/resume: the media timeline jumps here, so the
	/// playlist precedes it with `#EXT-X-DISCONTINUITY`.
	discontinuity: bool,
}

/// Lightweight per-part metadata for rendering a playlist (no bytes).
pub struct PartMeta {
	/// Part presentation duration, in seconds.
	pub duration: f64,
	/// Whether the part can be decoded independently (starts a keyframe).
	pub independent: bool,
}

/// Lightweight per-segment metadata for rendering a playlist (no bytes).
pub struct SegmentMeta {
	/// HLS media sequence number of this segment.
	pub sequence: u64,
	/// Parts making up this segment, in order.
	pub parts: Vec<PartMeta>,
	/// Total presentation duration so far (sum of part durations), in seconds.
	pub duration: f64,
	/// Whether the segment is finalized (a later segment has opened).
	pub complete: bool,
	/// True if this segment opens a new continuity region (post pause/resume); the
	/// renderer emits `#EXT-X-DISCONTINUITY` before it.
	pub discontinuity: bool,
}

/// A point-in-time view of the store, used to render a media playlist without
/// holding the lock during formatting.
pub struct Snapshot {
	/// Whether the init segment (`init.mp4`) is available.
	pub init_ready: bool,
	/// LL-HLS PART-TARGET, in seconds.
	pub part_target: f64,
	/// `EXT-X-MEDIA-SEQUENCE`: sequence of the first segment in the window.
	pub media_sequence: u64,
	/// Sequence the next segment to roll will be assigned.
	pub next_sequence: u64,
	/// Segments currently in the sliding window, oldest first.
	pub segments: Vec<SegmentMeta>,
	/// Whether the track has ended (the playlist gains `#EXT-X-ENDLIST`).
	pub finished: bool,
}

/// Watch value: enough for a blocking-reload waiter to decide if its
/// `(_HLS_msn, _HLS_part)` target has been reached, without locking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Version {
	/// Sequence of the newest segment.
	pub last_sequence: u64,
	/// Number of parts in the newest segment.
	pub last_parts: usize,
	/// Sequence of the oldest segment still in the window.
	pub media_sequence: u64,
	/// Whether the track has ended.
	pub finished: bool,
}

struct Inner {
	init: Option<Bytes>,
	segments: VecDeque<Segment>,
	next_sequence: u64,
	finished: bool,
	/// Set by [`SegmentStore::mark_discontinuity`] after a resume; the next segment
	/// to roll is forced open and tagged discontinuous, then this clears.
	discontinuity_pending: bool,
}

/// Bounded per-rendition store of CMAF segments and LL-HLS parts.
pub struct SegmentStore {
	inner: Mutex<Inner>,
	notify: watch::Sender<Version>,
	/// Video rolls a segment per GOP; audio rolls on duration.
	kind: Kind,
	/// LL-HLS PART-TARGET, in seconds.
	part_target: f64,
	/// Target segment duration for audio (video rolls on GOP boundaries instead).
	audio_segment_target: f64,
	/// Minimum duration (seconds) of media kept in the sliding window. The oldest
	/// segment is evicted only while the remaining ones still cover this span.
	window: f64,
}

impl SegmentStore {
	/// Create an empty store for a rendition of `kind`, taking part/segment/window
	/// timing from `config`.
	pub fn new(kind: Kind, config: &Config) -> Self {
		let (notify, _) = watch::channel(Version::default());
		Self {
			inner: Mutex::new(Inner {
				init: None,
				segments: VecDeque::new(),
				next_sequence: 0,
				finished: false,
				discontinuity_pending: false,
			}),
			notify,
			kind,
			part_target: config.part_target.as_secs_f64(),
			audio_segment_target: config.audio_segment_target.as_secs_f64(),
			window: config.window.as_secs_f64(),
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

		{
			let mut inner = self.inner.lock().unwrap();

			// A pending discontinuity forces a fresh segment so the resumed media never
			// shares a segment with pre-pause media (matters for audio, which otherwise
			// rolls on duration, not keyframes).
			let discontinuity = inner.discontinuity_pending;
			let need_new = discontinuity
				|| match inner.segments.back() {
					None => true,
					Some(cur) => {
						if self.kind == Kind::Video {
							// A new GOP (independent fragment) starts a new segment.
							fragment.independent
						} else {
							// Audio has no keyframes: roll once the segment is long enough.
							cur.duration >= self.audio_segment_target
						}
					}
				};

			if need_new {
				if let Some(cur) = inner.segments.back_mut() {
					cur.complete = true;
				}
				let sequence = inner.next_sequence;
				inner.next_sequence += 1;
				inner.segments.push_back(Segment {
					sequence,
					parts: Vec::new(),
					duration: 0.0,
					complete: false,
					discontinuity,
				});
				inner.discontinuity_pending = false;
			}

			let cur = inner.segments.back_mut().expect("segment present after need_new");
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

	/// Mark that the next segment to roll begins a new continuity region. Called
	/// once on each pause->resume transition: the media timeline jumps across the
	/// dropped span, so the renderer emits `#EXT-X-DISCONTINUITY` before it. The
	/// next [`push`](Self::push) forces a fresh segment and tags it.
	pub fn mark_discontinuity(&self) {
		self.inner.lock().unwrap().discontinuity_pending = true;
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
		self.bump();
	}

	fn bump(&self) {
		let version = self.version();
		// Ignore send errors: no receivers just means nobody is waiting yet.
		let _ = self.notify.send(version);
	}

	/// Current [`Version`] watermark (newest sequence/part counts and window edge).
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

	/// Subscribe to [`Version`] updates (one tick per new part/segment/finish).
	pub fn subscribe(&self) -> watch::Receiver<Version> {
		self.notify.subscribe()
	}

	/// The init segment (`init.mp4`) bytes, once available.
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
				discontinuity: s.discontinuity,
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
