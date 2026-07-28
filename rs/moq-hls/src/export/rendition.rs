//! One rendition: playlists from its view of the broadcast timeline, segments fetched on
//! demand.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use hang::catalog::{AudioConfig, MOQ_EPOCH_UNIX_MILLIS, Timeline, VideoConfig};
use moq_mux::container::fmp4::Muxer;
use moq_mux::timeline::Entry;

use super::playlist::{Segment, Snapshot};
use super::segments;
use crate::Result;

/// Fallback advertised bitrates when the catalog doesn't carry one.
const DEFAULT_VIDEO_BITRATE: u64 = 2_000_000;
const DEFAULT_AUDIO_BITRATE: u64 = 128_000;

/// Upper bound on the groups fetched for one segment, so a corrupt timeline can't turn one
/// HTTP request into an endless fetch loop.
const MAX_SEGMENT_GROUPS: u64 = 1024;

/// Whether a rendition carries video or audio.
///
/// Also the first URL path component of a rendition (`/{broadcast}/{kind}/{name}/...`),
/// so video and audio renditions that share a name don't collide.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Kind {
	/// A video rendition.
	Video,
	/// An audio rendition.
	Audio,
}

impl Kind {
	/// The URL path component for this kind (`"video"` / `"audio"`).
	pub fn as_str(self) -> &'static str {
		match self {
			Kind::Video => "video",
			Kind::Audio => "audio",
		}
	}
}

impl std::str::FromStr for Kind {
	type Err = ();

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"video" => Ok(Kind::Video),
			"audio" => Ok(Kind::Audio),
			_ => Err(()),
		}
	}
}

/// The rendition's catalog config, kept whole so a [`Muxer`] can be built per request.
#[derive(PartialEq)]
enum Config {
	Video(VideoConfig),
	Audio(AudioConfig),
}

/// A single HLS rendition: master-playlist metadata, its view of the broadcast timeline (which
/// renders its media playlist), and on-demand segment fetching.
pub struct Rendition {
	/// Rendition name (the catalog track name; also its URL path component).
	pub name: String,
	/// Whether this rendition is video or audio.
	pub kind: Kind,
	/// Advertised bitrate for the master playlist `BANDWIDTH` attribute.
	pub bandwidth: u64,
	/// Coded width, for the master playlist `RESOLUTION` (video only).
	pub width: Option<u32>,
	/// Coded height, for the master playlist `RESOLUTION` (video only).
	pub height: Option<u32>,
	/// RFC 6381 codec string for the master playlist `CODECS` attribute.
	pub codec: String,

	config: Config,
	/// The catalog's root timeline section: the timescale and wall anchor timings decode with.
	section: Timeline,
	/// This rendition's window over the broadcast timeline, fed by the catalog watcher's
	/// fan-out.
	live: Arc<segments::Producer>,
	/// The source and (possibly sibling) broadcast reference, resolved lazily for FETCH.
	source: moq_mux::Source,
	sibling: Option<moq_net::PathRelativeOwned>,
	/// The init segment, built on first request.
	init: tokio::sync::Mutex<Option<Bytes>>,
}

impl Rendition {
	pub(crate) fn matches_video(&self, config: &VideoConfig) -> bool {
		self.config == Config::Video(config.clone())
	}

	pub(crate) fn matches_audio(&self, config: &AudioConfig) -> bool {
		self.config == Config::Audio(config.clone())
	}

	/// Build a video rendition over the broadcast's timeline `section`.
	pub(crate) fn video(name: String, config: &VideoConfig, source: &moq_mux::Source, section: Timeline) -> Self {
		Self {
			name,
			kind: Kind::Video,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_VIDEO_BITRATE),
			width: config.coded_width,
			height: config.coded_height,
			codec: config.codec.to_string(),
			config: Config::Video(config.clone()),
			section,
			live: Arc::new(segments::Producer::new()),
			source: source.clone(),
			sibling: config.broadcast.clone(),
			init: tokio::sync::Mutex::new(None),
		}
	}

	/// Build an audio rendition over the broadcast's timeline `section`.
	pub(crate) fn audio(name: String, config: &AudioConfig, source: &moq_mux::Source, section: Timeline) -> Self {
		Self {
			name,
			kind: Kind::Audio,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_AUDIO_BITRATE),
			width: None,
			height: None,
			codec: config.codec.to_string(),
			config: Config::Audio(config.clone()),
			section,
			live: Arc::new(segments::Producer::new()),
			source: source.clone(),
			sibling: config.broadcast.clone(),
			init: tokio::sync::Mutex::new(None),
		}
	}

	/// Feed one timeline record into this rendition's window: its own ranges (empty when the
	/// record carries none for it, a gap), timed by the record.
	pub(crate) fn push(&self, entry: &Entry, window: Duration) {
		let row = segments::Row {
			segment: entry.segment,
			ranges: entry.tracks.get(&self.name).cloned().unwrap_or_default(),
			duration: entry.duration.as_secs_f64(),
			pts: entry.pts,
			end: Duration::from(entry.pts) + entry.duration,
		};
		self.live.push(row, window);
	}

	/// Mark this rendition's window ended (the timeline finished cleanly).
	pub(crate) fn end(&self) {
		self.live.end();
	}

	/// A cursor over this rendition's segments, with their media, in timeline order.
	/// For a recorder mirroring the broadcast; see [`segments::Consumer`].
	pub fn segments(self: &Arc<Self>) -> segments::Consumer {
		self.live.subscribe(self.clone())
	}

	/// Retire this rendition: close its window so every [`segments::Consumer`] over it drains
	/// what's left and then ends.
	///
	/// Called when the catalog drops or replaces the rendition; a cursor's `Arc<Self>` would
	/// otherwise park forever on a window nothing feeds anymore.
	pub(crate) fn close(&self) {
		self.live.close();
	}

	/// Resolve once the playlist is renderable, i.e. once [`media_playlist`](Self::media_playlist)
	/// would return `Some`. Bounding the wait is the caller's policy (the serve path wraps this
	/// in its own timeout).
	pub async fn playable(&self) {
		kio::wait(|waiter| self.live.poll_playable(waiter)).await;
	}

	/// Render this rendition's media playlist from the current timeline window, or `None` when
	/// there is nothing to serve yet (no segment, and the broadcast has not ended) -- a
	/// playlist with no segments confuses players, so a server should treat `None` as "not
	/// ready" rather than serve it.
	///
	/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
	/// appended to every child URL (the init map and each segment), so a stock player that does
	/// not replay request headers still carries a credential on its follow-up requests. It is an
	/// argument rather than a [`Config`](super::Config) field because one broadcaster fans out
	/// to viewers holding different tokens.
	pub fn media_playlist(&self, query: Option<&str>) -> Option<String> {
		self.is_playable().then(|| super::render_media(&self.playlist(), query))
	}

	/// Render the media playlist from the current timeline window.
	pub(crate) fn playlist(&self) -> Snapshot {
		let window = self.live.window();

		// EXT-X-TARGETDURATION comes straight from the catalog's declared bound: the encoder
		// (or cutter) knows and enforces its segment duration, so the exporter never has to
		// guess from observed durations. Rounded up, since HLS wants whole seconds and the
		// advertised value must never understate the bound.
		let target_duration = self
			.section
			.duration_max
			.div_ceil((self.section.timescale as u64).max(1))
			.max(1);

		let program_date_time = window.segments.first().and_then(|first| self.wall_clock(first.pts));

		// A jump in content time between consecutive rows is a discontinuity: the next segment
		// doesn't continue the previous one's timeline. Tolerate sub-millisecond drift from
		// timescale rounding.
		let mut previous_end: Option<Duration> = None;
		let segments = window
			.segments
			.into_iter()
			.map(|s| {
				let discontinuity = previous_end.is_some_and(|end| {
					let start = Duration::from(s.pts);
					start.saturating_sub(end).max(end.saturating_sub(start)) > Duration::from_millis(1)
				});
				previous_end = Some(s.end);
				Segment {
					segment: s.segment,
					duration: s.duration,
					gap: s.ranges.is_empty(),
					discontinuity,
				}
			})
			.collect();

		Snapshot {
			target_duration,
			media_sequence: window.sequence,
			segments,
			finished: window.ended,
			program_date_time,
		}
	}

	/// Whether the playlist has anything to serve yet (at least one segment, or the broadcast
	/// already ended).
	pub(crate) fn is_playable(&self) -> bool {
		self.live.is_playable()
	}

	/// The wall-clock time of a media timestamp, when the timeline advertises an anchor.
	pub(crate) fn wall_clock(&self, pts: moq_net::Timestamp) -> Option<SystemTime> {
		let wall = self.section.wall?;
		let timescale = moq_net::Timescale::new(self.section.timescale as u64).ok()?;
		let units = wall as u128 + pts.as_scale(timescale);
		let unix_ms = MOQ_EPOCH_UNIX_MILLIS as u128 + units * 1000 / timescale.as_u64() as u128;
		Some(SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(unix_ms).ok()?))
	}

	fn muxer(&self) -> Result<Muxer> {
		Ok(match &self.config {
			Config::Video(config) => Muxer::video(config)?,
			Config::Audio(config) => Muxer::audio(config)?,
		})
	}

	/// The media track handle on its (possibly sibling) broadcast, resolved lazily on the
	/// first fetch.
	async fn track(&self) -> Option<moq_net::track::Consumer> {
		if self.live.broadcast.get().is_none() {
			let broadcast = self.source.resolve(self.sibling.as_ref()).await.ok()?;
			let _ = self.live.broadcast.set(broadcast);
		}
		let broadcast = self.live.broadcast.get()?;
		broadcast.track(&self.name).ok()
	}

	/// The rendition's CMAF init segment, built on first request and cached.
	///
	/// For inline-parameter-set codecs (no catalog `description`), the parameter sets are
	/// resolved by fetching the newest keyframe group first.
	pub(crate) async fn init(&self) -> Result<Option<Bytes>> {
		let mut cache = self.init.lock().await;
		if let Some(bytes) = cache.as_ref() {
			return Ok(Some(bytes.clone()));
		}

		let mut muxer = self.muxer()?;

		// An out-of-band codec builds its init straight from the catalog; an inline one returns
		// `None` until a keyframe group is read, so bootstrap it from the newest one indexed.
		let bytes = match muxer.init()? {
			Some(bytes) => bytes,
			None => {
				let Some(sequence) = self.live.latest_keyframe_group() else {
					return Ok(None);
				};
				let Some(track) = self.track().await else {
					return Ok(None);
				};
				let Some(mut group) = fetch(&track, sequence).await? else {
					return Ok(None);
				};
				// A cache eviction mid-read leaves the init unbuildable for now, not an error.
				if read_group(&mut muxer, &mut group).await?.is_none() {
					return Ok(None);
				}
				let Some(bytes) = muxer.init()? else {
					return Ok(None);
				};
				bytes
			}
		};
		*cache = Some(bytes.clone());
		Ok(Some(bytes))
	}

	/// Fetch and transmux the segment numbered `segment`.
	///
	/// Fetches every group the segment's ranges cover (an audio segment packs many short
	/// groups) and encodes them as a single CMAF fragment. `None` when the segment isn't in
	/// the playlist window, is a gap for this rendition, or its groups already left the relay
	/// cache.
	pub async fn segment(&self, segment: u64) -> Result<Option<Bytes>> {
		let Some(ranges) = self.live.segment_ranges(segment) else {
			return Ok(None);
		};
		if ranges.is_empty() {
			// A gap: the record says this rendition has no content for the span.
			return Ok(None);
		}

		// A segment must be served whole. If its group span exceeds the safety cap (a
		// pathologically coarse timeline), 404 rather than silently truncating it to a segment
		// shorter than its advertised duration.
		let total: u64 = ranges
			.iter()
			.map(|r| r.end.saturating_sub(r.start).saturating_add(1))
			.sum();
		if total > MAX_SEGMENT_GROUPS {
			tracing::warn!(
				total,
				"segment spans more than MAX_SEGMENT_GROUPS; refusing to truncate"
			);
			return Ok(None);
		}

		let Some(track) = self.track().await else {
			return Ok(None);
		};

		let mut muxer = self.muxer()?;
		// Accumulate every group's frames into ONE fragment, so duration inference sees each
		// frame's true successor. A per-group fragment would mis-time the trailing sample of every
		// group (its real successor lives in the next group), audible on audio segments that span
		// many groups.
		let mut frames = Vec::new();
		for range in &ranges {
			for sequence in range.start..=range.end {
				let Some(mut group) = fetch(&track, sequence).await? else {
					// A missing group: the segment left the relay cache; serve nothing rather
					// than a truncated segment.
					return Ok(None);
				};
				let Some(mut group_frames) = read_group(&mut muxer, &mut group).await? else {
					// The group aged out of the cache mid-read.
					return Ok(None);
				};
				frames.append(&mut group_frames);
			}
		}

		if frames.is_empty() {
			return Ok(None);
		}
		Ok(Some(muxer.fragment(segment as u32, &frames)?))
	}
}

/// Fetch one group, mapping "no longer (or not yet) servable" to `None`.
async fn fetch(track: &moq_net::track::Consumer, sequence: u64) -> Result<Option<moq_net::group::Consumer>> {
	match track.fetch_group(sequence, None).await {
		Ok(group) => Ok(Some(group)),
		Err(err) if is_cache_miss(&err) => Ok(None),
		Err(err) => Err(err.into()),
	}
}

/// Read a fetched group's frames, mapping a mid-read cache eviction (the group aged out of the
/// relay while we were reading it) to `None` rather than an error. Without this the eviction would
/// surface as a 500; it's really the same "group gone" 404 as a fetch-time miss.
async fn read_group(
	muxer: &mut Muxer,
	group: &mut moq_net::group::Consumer,
) -> Result<Option<Vec<moq_mux::container::Frame>>> {
	match muxer.read(group).await {
		Ok(frames) => Ok(Some(frames)),
		Err(err) if is_cache_miss_mux(&err) => Ok(None),
		Err(err) => Err(err.into()),
	}
}

/// True if a moq-net error means the group left (or hasn't reached) the relay cache: a 404, not
/// a 500.
fn is_cache_miss(err: &moq_net::Error) -> bool {
	matches!(
		err,
		moq_net::Error::NotFound | moq_net::Error::Old | moq_net::Error::Evicted
	)
}

/// [`is_cache_miss`] through the moq-mux error layers a group read surfaces (plain transport, or
/// wrapped in a CMAF decode error).
fn is_cache_miss_mux(err: &moq_mux::Error) -> bool {
	match err {
		moq_mux::Error::Moq(err) => is_cache_miss(err),
		moq_mux::Error::Cmaf(moq_mux::container::fmp4::Error::Moq(err)) => is_cache_miss(err),
		_ => false,
	}
}
