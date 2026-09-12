//! One rendition: playlists from its view of the broadcast timeline, segments fetched on
//! demand.

use std::sync::{Arc, Mutex, RwLock};
use std::task::Poll;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use hang::catalog::{AudioConfig, MOQ_EPOCH_UNIX_MILLIS, Timeline, VideoConfig};
use moq_mux::container::fmp4::Muxer;
use moq_mux::timeline::Entry;

use super::playlist::{Segment, Snapshot};
use super::upstream::Upstream;
use super::{mpd, segments};
use crate::Result;

/// Fallback advertised bitrates when the catalog doesn't carry one.
const DEFAULT_VIDEO_BITRATE: u64 = 2_000_000;
const DEFAULT_AUDIO_BITRATE: u64 = 128_000;

/// The fallback advertised bitrate for a rendition of this kind.
fn default_bandwidth(kind: Kind) -> u64 {
	match kind {
		Kind::Video => DEFAULT_VIDEO_BITRATE,
		Kind::Audio => DEFAULT_AUDIO_BITRATE,
	}
}

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
enum Config {
	Video(VideoConfig),
	Audio(AudioConfig),
}

/// The broadcast serving this rendition's media, rebound when a sibling publisher is replaced.
struct Media {
	handle: Mutex<Handle>,
	/// When set, a Dropped sibling is rebound through this source rather than keeping the
	/// replaced publisher's rows listed.
	sibling: Option<(moq_mux::Source, moq_net::PathRelativeOwned)>,
}

struct Handle {
	binding: Arc<moq_mux::Binding>,
	/// Skip incoming timeline rows until the current bind resolves (set when a sibling is rebound).
	waiting: bool,
}

impl Media {
	fn bind(upstream: &Upstream, rel: Option<&moq_net::PathRelativeOwned>) -> moq_mux::Result<Self> {
		Ok(Self {
			handle: Mutex::new(Handle {
				binding: Arc::new(upstream.bind(rel)?),
				waiting: false,
			}),
			sibling: sibling(upstream, rel),
		})
	}

	/// Drop listed rows and rebind when a sibling publisher has been replaced; return whether
	/// incoming timeline rows should be listed.
	fn admits(&self, window: &segments::Producer) -> bool {
		self.sync(window);
		!self.handle.lock().expect("media lock poisoned").waiting
	}

	/// Drop listed rows and rebind when a sibling publisher has been replaced; return the
	/// binding those rows belong to. Fetch from this handle, not a later `sync`: a replacement
	/// that lands between a range lookup and `track` would otherwise serve the old groups.
	fn sync(&self, window: &segments::Producer) -> Arc<moq_mux::Binding> {
		let Some((source, rel)) = &self.sibling else {
			return self.handle.lock().expect("media lock poisoned").binding.clone();
		};
		let mut handle = self.handle.lock().expect("media lock poisoned");
		match handle.binding.poll_broadcast(&kio::Waiter::noop()) {
			Poll::Ready(Ok(broadcast)) if broadcast.is_closed() => {
				// The bound sibling ended (a different first hop replaces the publisher with
				// Dropped). Origin finish()es that spliced front, so is_finished() cannot
				// tell a rival publisher from a clean VOD end. Rows listed for it must not
				// be served from the replacement.
				window.clear();
				if let Ok(next) = source.bind(Some(rel)) {
					handle.binding = Arc::new(next);
				}
				handle.waiting = true;
			}
			Poll::Ready(Ok(_)) => handle.waiting = false,
			Poll::Ready(Err(_)) => {
				// Binding never retries a failed request. Re-issue so a publisher that
				// appears after this Unroutable can still recover the rendition. Do not
				// set waiting: an initial Unroutable (announce still in flight) must
				// still list the catalog's rows.
				if let Ok(next) = source.bind(Some(rel)) {
					handle.binding = Arc::new(next);
				}
			}
			Poll::Pending => {}
		}
		handle.binding.clone()
	}
}

/// A catalog `broadcast` reference that names a different path than the catalog itself.
fn sibling(
	upstream: &Upstream,
	rel: Option<&moq_net::PathRelativeOwned>,
) -> Option<(moq_mux::Source, moq_net::PathRelativeOwned)> {
	let target = upstream.source.resolve_reference(rel)?;
	if upstream.source.resolve_reference(None).as_ref() == Some(&target) {
		return None;
	}
	Some((
		upstream.source.clone(),
		rel.cloned().unwrap_or_else(moq_net::PathRelative::empty),
	))
}

/// Clear the video fields that don't change how the rendition decodes or is muxed, so a config
/// the publisher only re-measured or re-labelled compares equal to the one already being served.
///
/// A denylist rather than an allowlist on purpose: a field the catalog gains later counts as
/// decode-relevant until someone decides otherwise, so the failure mode is a needless rebuild
/// rather than serving an init segment that no longer describes the media.
fn normalize_video(config: &VideoConfig) -> VideoConfig {
	let mut config = config.clone();
	config.bitrate = None;
	config.jitter = None;
	config.label = None;
	config.stalled = None;
	// The muxer ignores a non-finite framerate, and NaN never equals itself, so a catalog
	// carrying one would otherwise look different from itself on every republish.
	config.framerate = config.framerate.filter(|fps| fps.is_finite());
	config
}

/// The audio counterpart of [`normalize_video`].
fn normalize_audio(config: &AudioConfig) -> AudioConfig {
	let mut config = config.clone();
	config.bitrate = None;
	config.jitter = None;
	config.label = None;
	config
}

/// A single HLS rendition: master-playlist metadata, its view of the broadcast timeline (which
/// renders its media playlist), and on-demand segment fetching.
pub struct Rendition {
	/// Rendition name (the catalog track name; also its URL path component).
	pub name: String,
	/// Whether this rendition is video or audio.
	pub kind: Kind,
	/// Advertised bitrate for the master playlist `BANDWIDTH` attribute; read through
	/// [`bandwidth`](Self::bandwidth), refreshed in place by [`refresh`](Self::refresh).
	bitrate: RwLock<Option<u64>>,
	/// Coded width, for the master playlist `RESOLUTION` (video only).
	pub width: Option<u32>,
	/// Coded height, for the master playlist `RESOLUTION` (video only).
	pub height: Option<u32>,
	/// RFC 6381 codec string for the master playlist `CODECS` attribute.
	pub codec: String,

	config: Config,
	/// The catalog's root archive timeline: the timescale and wall anchor timings decode with.
	section: Timeline,
	/// This rendition's window over the broadcast timeline, fed by the catalog watcher's
	/// fan-out.
	live: Arc<segments::Producer>,
	/// The broadcast serving this rendition's media. A sibling is bound when the rendition is
	/// created and rebound if that publisher is replaced (see [`Upstream::bind`]).
	media: Media,
	/// The init segment, built on first request.
	init: tokio::sync::Mutex<Option<Bytes>>,
}

impl Rendition {
	/// Whether `config` describes the same media this rendition is already serving, i.e. whether
	/// it decodes and muxes identically. Fields the publisher revises without touching the media
	/// (its bitrate and jitter estimates, a label) are ignored here and picked up by
	/// [`refresh`](Self::refresh) instead.
	pub(crate) fn matches_video(&self, config: &VideoConfig) -> bool {
		let Config::Video(current) = &self.config else {
			return false;
		};
		normalize_video(current) == normalize_video(config)
	}

	/// The audio counterpart of [`matches_video`](Self::matches_video).
	pub(crate) fn matches_audio(&self, config: &AudioConfig) -> bool {
		let Config::Audio(current) = &self.config else {
			return false;
		};
		normalize_audio(current) == normalize_audio(config)
	}

	/// The advertised bitrate in bits per second, for the master playlist `BANDWIDTH` attribute
	/// and the DASH `bandwidth`. Falls back to a per-kind default when the catalog carries none.
	pub fn bandwidth(&self) -> u64 {
		self.bitrate().unwrap_or(default_bandwidth(self.kind))
	}

	/// Take the advertised bitrate from a catalog update that
	/// [`matches`](Self::matches_video) this rendition.
	///
	/// The publisher's estimator republishes the catalog whenever its measured bitrate moves, so
	/// this is the common update by far: rebuilding the rendition for it would reset the playlist
	/// window, the cached init segment, and `EXT-X-MEDIA-SEQUENCE` several times a minute.
	pub(crate) fn refresh(&self, bitrate: Option<u64>) {
		*self.bitrate.write().expect("bitrate lock poisoned") = bitrate;
	}

	/// The latest catalog bitrate, preserving `None` for muxer-specific fallback behavior.
	fn bitrate(&self) -> Option<u64> {
		*self.bitrate.read().expect("bitrate lock poisoned")
	}

	/// Build a video rendition over the broadcast's timeline `section`.
	///
	/// Fails when the config's `broadcast` reference escapes above the origin root: it names no
	/// broadcast, so there would be no media to serve.
	pub(crate) fn video(
		name: String,
		config: &VideoConfig,
		upstream: &Upstream,
		section: Timeline,
	) -> moq_mux::Result<Self> {
		Ok(Self {
			name,
			kind: Kind::Video,
			bitrate: RwLock::new(config.bitrate),
			width: config.coded_width,
			height: config.coded_height,
			codec: config.codec.to_string(),
			config: Config::Video(config.clone()),
			section,
			live: Arc::new(segments::Producer::new()),
			media: Media::bind(upstream, config.broadcast.as_ref())?,
			init: tokio::sync::Mutex::new(None),
		})
	}

	/// Build an audio rendition over the broadcast's timeline `section`, failing like
	/// [`video`](Self::video).
	pub(crate) fn audio(
		name: String,
		config: &AudioConfig,
		upstream: &Upstream,
		section: Timeline,
	) -> moq_mux::Result<Self> {
		Ok(Self {
			name,
			kind: Kind::Audio,
			bitrate: RwLock::new(config.bitrate),
			width: None,
			height: None,
			codec: config.codec.to_string(),
			config: Config::Audio(config.clone()),
			section,
			live: Arc::new(segments::Producer::new()),
			media: Media::bind(upstream, config.broadcast.as_ref())?,
			init: tokio::sync::Mutex::new(None),
		})
	}

	/// Feed one timeline record into this rendition's window: its own ranges (empty when the
	/// record carries none for it, a gap), timed by the record.
	pub(crate) fn push(&self, index: u64, entry: &Entry, window: Duration) {
		if !self.media.admits(&self.live) {
			return;
		}
		let row = segments::Row {
			index,
			segment: entry.segment,
			ranges: entry.tracks.get(&self.name).cloned().unwrap_or_default(),
			duration: entry.duration.as_secs_f64(),
			pts: entry.pts,
			end: Duration::from(entry.pts) + entry.duration,
		};
		self.live.push(row, window);
	}

	/// Remove source timeline records in `range` from this rendition's window.
	pub(crate) fn pop(&self, range: std::ops::Range<u64>) {
		self.live.pop(range);
	}

	/// Clear rows that can no longer be followed by a consecutive source timeline record.
	pub(crate) fn clear(&self) {
		self.live.clear();
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
		self.media.sync(&self.live);
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
		self.media.sync(&self.live);
		let window = self.live.window();

		// EXT-X-TARGETDURATION starts from the catalog's declared bound when the publisher
		// offered one: it knows its segment duration up front, so the first playlist already
		// carries the real value instead of a guess grown from observation. Rounded up, since
		// HLS wants whole seconds and the advertised value must never understate the bound.
		let declared = self
			.section
			.duration_max
			.map(|max| max.div_ceil((self.section.timescale as u64).max(1)))
			.unwrap_or(0);

		// Most publishers can't promise a bound (a real-time GOP can be minutes long), and even
		// a declared one is only a promise about content the publisher controls. HLS requires
		// EXT-X-TARGETDURATION to be at least every EXTINF, so cover the window either way.
		let observed = window
			.segments
			.iter()
			.map(|s| s.duration.ceil().max(0.0) as u64)
			.max()
			.unwrap_or(0);
		let target_duration = declared.max(observed).max(1);

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
		self.media.sync(&self.live);
		self.live.is_playable()
	}

	/// This rendition's DASH representation: its master-level metadata plus its slice of the
	/// shared timeline as `(t, d)` pairs in the timeline's own timescale (the record values
	/// verbatim, so `$Time$` addressing resolves exactly).
	pub(crate) fn representation(&self) -> mpd::Representation {
		self.media.sync(&self.live);
		let window = self.live.window();
		let timescale = self.timescale();
		let segments = window
			.segments
			.iter()
			.filter(|row| !row.ranges.is_empty())
			.filter_map(|row| {
				let t = row.pts.as_scale(timescale) as u64;
				let span = row.end.saturating_sub(row.pts.into());
				let d = (span.as_nanos() * timescale.as_u64() as u128 / 1_000_000_000) as u64;
				// A zero-duration record (the terminal segment when the publisher couldn't
				// bound its final sample) would sit exactly at the presentation end, so a
				// conforming player would never request it; advertising it in a
				// SegmentTimeline is contradictory timing, so leave it out. (HLS still lists
				// it as EXTINF:0, which players fetch by URL rather than by timing.)
				(d > 0).then_some((t, d))
			})
			.collect();
		let (framerate, sample_rate, channel_count) = match &self.config {
			Config::Video(config) => (config.framerate, None, None),
			Config::Audio(config) => (None, Some(config.sample_rate), Some(config.channel_count)),
		};
		mpd::Representation {
			name: self.name.clone(),
			kind: self.kind,
			bandwidth: self.bandwidth(),
			codec: self.codec.clone(),
			width: self.width,
			height: self.height,
			framerate,
			sample_rate,
			channel_count,
			timescale: self.section.timescale.max(1),
			segments,
			ended: window.ended,
		}
	}

	/// Fetch and transmux the segment whose timeline `pts` is `time`, in the timeline's
	/// timescale (DASH `$Time$` addressing of the same bytes [`segment`](Self::segment) serves
	/// by aligned number).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub(crate) async fn segment_at(&self, time: u64) -> Result<Option<Bytes>> {
		self.media.sync(&self.live);
		let Some(segment) = self.live.segment_number_at(time, self.timescale()) else {
			return Ok(None);
		};
		self.segment(segment).await
	}

	/// The timeline's timescale; a declared 0 is clamped rather than trusted.
	fn timescale(&self) -> moq_net::Timescale {
		moq_net::Timescale::new((self.section.timescale as u64).max(1)).expect("clamped nonzero timescale")
	}

	/// The wall-clock time of a media timestamp, when the timeline advertises an anchor.
	pub(crate) fn wall_clock(&self, pts: moq_net::Timestamp) -> Option<SystemTime> {
		let wall = self.section.wall?;
		let timescale = moq_net::Timescale::new(u64::from(self.section.timescale)).ok()?;
		let units = u128::from(wall) + pts.as_scale(timescale);
		let unix_ms = u128::from(MOQ_EPOCH_UNIX_MILLIS) + units * 1000 / u128::from(timescale.as_u64());
		Some(SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(unix_ms).ok()?))
	}

	fn muxer(&self) -> Result<Muxer> {
		let bitrate = self.bitrate();
		Ok(match &self.config {
			Config::Video(config) => {
				let mut config = config.clone();
				config.bitrate = bitrate;
				Muxer::video(&config)?
			}
			Config::Audio(config) => {
				let mut config = config.clone();
				config.bitrate = bitrate;
				Muxer::audio(&config)?
			}
		})
	}

	/// The media track handle on `binding`, with that bound broadcast so a fetch can tell a
	/// gone publisher from a live failure.
	///
	/// `binding` is the handle captured with the rows being served. Collecting the answer
	/// never looks the path up ad hoc: a same-path republish installs a new broadcast whose
	/// group numbering restarts, and those bytes must not be served under rows produced for
	/// the publisher it replaced.
	async fn track(
		&self,
		binding: &moq_mux::Binding,
	) -> Option<(moq_net::broadcast::Consumer, moq_net::track::Consumer)> {
		let broadcast = binding.broadcast().await.ok()?;
		let track = broadcast.track(&self.name).ok()?;
		Some((broadcast, track))
	}

	/// The rendition's CMAF init segment, built on first request and cached.
	///
	/// For inline-parameter-set codecs (no catalog `description`), the parameter sets are
	/// resolved by fetching the newest keyframe group first.
	pub async fn init(&self) -> Result<Option<Bytes>> {
		let binding = self.media.sync(&self.live);
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
				let Some((broadcast, track)) = self.track(&binding).await else {
					return Ok(None);
				};
				let Ok(mut group) = fetch(&track, sequence, &broadcast).await? else {
					return Ok(None);
				};
				// A cache eviction mid-read leaves the init unbuildable for now, not an error.
				if read_group(&mut muxer, &mut group, &broadcast).await?.is_err() {
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
		let binding = self.media.sync(&self.live);
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

		let Some((broadcast, track)) = self.track(&binding).await else {
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
				let miss = match fetch(&track, sequence, &broadcast).await? {
					Ok(mut group) => match read_group(&mut muxer, &mut group, &broadcast).await? {
						Ok(mut group_frames) => {
							frames.append(&mut group_frames);
							continue;
						}
						Err(err) => err,
					},
					Err(err) => err,
				};
				tracing::warn!(
					segment,
					group = sequence,
					err = %miss,
					"abandoning an advertised segment: a group is unavailable"
				);
				return Ok(None);
			}
		}

		if frames.is_empty() {
			return Ok(None);
		}
		Ok(Some(muxer.fragment(segment as u32, &frames)?))
	}
}

/// Fetch one group, mapping "no longer (or not yet) servable" to the error that says so
/// rather than a bare `None`: a caller abandoning a segment can only explain itself with it.
async fn fetch(
	track: &moq_net::track::Consumer,
	sequence: u64,
	broadcast: &moq_net::broadcast::Consumer,
) -> Result<std::result::Result<moq_net::group::Consumer, moq_net::Error>> {
	match track.fetch_group(sequence, None).await {
		Ok(group) => Ok(Ok(group)),
		Err(err) if is_unservable(&err, broadcast) => Ok(Err(err)),
		Err(err) => Err(err.into()),
	}
}

/// Read a fetched group's frames, mapping a mid-read cache eviction (the group aged out of the
/// relay while we were reading it) to `None` rather than an error. Without this the eviction would
/// surface as a 500; it's really the same "group gone" 404 as a fetch-time miss.
async fn read_group(
	muxer: &mut Muxer,
	group: &mut moq_net::group::Consumer,
	broadcast: &moq_net::broadcast::Consumer,
) -> Result<std::result::Result<Vec<moq_mux::container::Frame>, moq_net::Error>> {
	match muxer.read(group).await {
		Ok(frames) => Ok(Ok(frames)),
		Err(err) => {
			let miss = net_errors(&err).find(|err| is_unservable(err, broadcast)).cloned();
			match miss {
				Some(miss) => Ok(Err(miss)),
				None => Err(err.into()),
			}
		}
	}
}

/// True if a FETCH failure means the segment can never be served: a 404, not a 500.
fn is_unservable(err: &moq_net::Error, broadcast: &moq_net::broadcast::Consumer) -> bool {
	is_cache_miss(err) || is_gone_publisher(err, broadcast)
}

/// True if a moq-net error means the group left (or hasn't reached) the relay cache: a 404, not
/// a 500.
///
/// Matches the named local variants and the stream-scoped codes a moq-lite reset
/// decodes to after [`StreamError::from_code`](moq_net::StreamError::from_code). IETF has
/// no stream-reset value for a miss (`ietf::error::to_stream_code` sends INTERNAL_ERROR),
/// so one arrives as `Error::Stream(StreamError::Internal)` and answers 500. Inspect the
/// variant, not `Error::to_code`: session unauthorized and stream delivery-timeout are
/// also 2 in their own registries.
fn is_cache_miss(err: &moq_net::Error) -> bool {
	matches!(
		err,
		moq_net::Error::NotFound
			| moq_net::Error::Old
			| moq_net::Error::Evicted
			| moq_net::Error::Stream(
				moq_net::StreamError::NotFound | moq_net::StreamError::Old | moq_net::StreamError::Evicted
			)
	)
}

/// True if the bound publisher is gone, so a listed segment can never be served.
///
/// A disconnect that crossed a session arrives as `Error::Session(SessionError::Cancel)`;
/// locally it is [`moq_net::Error::Dropped`]. The bound broadcast being closed is the
/// condition: matching a session cancel alone would 404 a clean close while the publisher
/// is still live.
fn is_gone_publisher(err: &moq_net::Error, broadcast: &moq_net::broadcast::Consumer) -> bool {
	broadcast.is_closed() || matches!(err, moq_net::Error::Dropped | moq_net::Error::Cancel)
}

/// [`is_cache_miss`] for a group read, which reaches us wrapped in whichever container the track
/// uses: plain transport for a raw read, `Hang` for the legacy container (what the JS and native
/// encoders publish), `Cmaf` for fMP4.
///
/// Walks the source chain rather than matching those wrappings, since missing one turns that
/// container's evictions back into server errors while the rest 404, and `moq_mux::Error` is
/// `#[non_exhaustive]`: a container added later would silently reintroduce the bug.
fn net_errors(err: &moq_mux::Error) -> impl Iterator<Item = &moq_net::Error> {
	std::iter::successors(Some(err as &(dyn std::error::Error + 'static)), |err| err.source())
		.filter_map(|err| err.downcast_ref::<moq_net::Error>())
}
#[cfg(test)]
mod tests {
	use super::*;

	/// Decode the code `local` would send on a moq-lite stream reset.
	fn over_lite(local: &moq_net::Error) -> moq_net::Error {
		moq_net::Error::from(moq_net::StreamError::from_code(
			moq_net::StreamError::from(local).to_code(),
		))
	}

	/// Decode INTERNAL_ERROR, which is what `ietf::error::to_stream_code` sends for a miss.
	fn over_ietf_miss() -> moq_net::Error {
		moq_net::Error::from(moq_net::StreamError::from_code(0))
	}

	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::Hop::random().into());
		std::mem::forget(driver);
		producer
	}

	#[test]
	fn a_cache_miss_that_crossed_a_session_is_still_a_cache_miss() {
		for local in [moq_net::Error::NotFound, moq_net::Error::Old, moq_net::Error::Evicted] {
			assert!(is_cache_miss(&local), "{local:?} locally");
			let lite = over_lite(&local);
			assert!(is_cache_miss(&lite), "{local:?} over lite ({lite:?})");
			// IETF has no stream-reset value for a miss, so one stays 500.
			assert!(!is_cache_miss(&over_ietf_miss()), "{local:?} over ietf must not 404");
		}
	}

	#[test]
	fn a_real_failure_is_not_a_cache_miss() {
		for err in [
			moq_net::Error::Unauthorized,
			moq_net::Error::ProtocolViolation,
			moq_net::Error::Timeout,
			moq_net::Error::from(moq_net::SessionError::Unauthorized),
			moq_net::Error::from(moq_net::StreamError::DeliveryTimeout),
		] {
			assert!(!is_cache_miss(&err), "{err:?} locally");
			assert!(!is_cache_miss(&over_lite(&err)), "{err:?} over lite");
		}
	}

	#[test]
	fn a_cache_miss_is_recognised_through_the_mux_error_layers() {
		let lite = over_lite(&moq_net::Error::NotFound);
		assert!(net_errors(&moq_mux::Error::Moq(lite.clone())).any(is_cache_miss));
		assert!(net_errors(&moq_mux::Error::Hang(hang::Error::Moq(lite.clone()))).any(is_cache_miss));
		assert!(net_errors(&moq_mux::Error::Cmaf(moq_mux::container::fmp4::Error::Moq(lite))).any(is_cache_miss));

		let denied = moq_net::Error::Unauthorized;
		assert!(!net_errors(&moq_mux::Error::Moq(denied.clone())).any(is_cache_miss));
		assert!(!net_errors(&moq_mux::Error::Hang(hang::Error::Moq(denied))).any(is_cache_miss));
		assert!(!net_errors(&moq_mux::Error::UnknownFormat("nope".to_string())).any(is_cache_miss));
	}

	#[test]
	fn a_gone_publisher_is_unservable_and_an_ietf_miss_is_not() {
		let origin = produce_origin();
		let producer = origin.create_broadcast("live").expect("publish allowed");
		let live = producer.consume();
		let ietf_miss = over_ietf_miss();
		assert!(
			!is_gone_publisher(&ietf_miss, &live),
			"an IETF stream-reset miss on a live publisher must stay 500"
		);
		assert!(!is_unservable(&ietf_miss, &live));

		drop(producer);
		let gone = live;
		assert!(is_gone_publisher(&ietf_miss, &gone));
		assert!(is_unservable(&ietf_miss, &gone));
		assert!(is_gone_publisher(&moq_net::Error::Dropped, &gone));
		assert!(is_gone_publisher(&moq_net::Error::Cancel, &gone));
		assert!(!is_cache_miss(&moq_net::Error::Dropped));
		assert!(!is_cache_miss(&moq_net::Error::Remote(0)));
	}
}
