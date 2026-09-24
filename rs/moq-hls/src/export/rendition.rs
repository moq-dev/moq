//! One rendition: playlists from its view of the broadcast timeline, segments fetched on
//! demand.

use std::sync::{Arc, Mutex, RwLock};
use std::task::Poll;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use hang::catalog::{Archive, AudioConfig, Clock, VideoConfig};
use moq_mux::container::fmp4::Muxer;
use moq_mux::timeline::Entry;
use sha2::{Digest, Sha256};

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
	/// Parse a rendition URL path component.
	pub fn parse(value: &str) -> Option<Self> {
		match value {
			"video" => Some(Self::Video),
			"audio" => Some(Self::Audio),
			_ => None,
		}
	}

	/// The URL path component for this kind (`"video"` / `"audio"`).
	pub fn as_str(self) -> &'static str {
		match self {
			Kind::Video => "video",
			Kind::Audio => "audio",
		}
	}
}

/// A built init segment, with the content hash that versions its URL (`init.{hash}.mp4`).
///
/// Hashing the bytes rather than a list of catalog fields covers every input that shapes the
/// init, and lets two edges serving the same rendition agree on the URL without coordinating.
struct Init {
	bytes: Bytes,
	hash: String,
}

impl Init {
	fn new(bytes: Bytes) -> Self {
		// 64 bits: the URL only has to tell apart the inits one rendition name ever carries.
		let hash = Sha256::digest(&bytes)[..8].iter().map(|b| format!("{b:02x}")).collect();
		Self { bytes, hash }
	}
}

/// The publisher run a rendition lists: the label its segment URLs carry and the init built
/// from its media. Read and replaced together, so a render never pairs one run's label with
/// another run's init.
#[derive(Clone, Default)]
struct Run {
	generation: Option<Arc<str>>,
	init: Option<Arc<Init>>,
	/// Bumped by every restart, so an init build that straddles one isn't cached for the new run.
	epoch: u64,
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
	sibling: Option<(moq_mux::Source, moq_net::path::RelativeOwned)>,
}

struct Handle {
	binding: Arc<moq_mux::Binding>,
	/// Skip incoming timeline rows until the current bind resolves (set when a sibling is rebound).
	waiting: bool,
}

impl Media {
	fn bind(upstream: &Upstream, rel: Option<&moq_net::path::RelativeOwned>) -> moq_mux::Result<Self> {
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
	rel: Option<&moq_net::path::RelativeOwned>,
) -> Option<(moq_mux::Source, moq_net::path::RelativeOwned)> {
	let target = upstream.source.resolve_reference(rel)?;
	if upstream.source.resolve_reference(None).as_ref() == Some(&target) {
		return None;
	}
	Some((
		upstream.source.clone(),
		rel.cloned().unwrap_or_else(moq_net::path::Relative::empty),
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
	/// The catalog's root archive timeline: the timescale timings decode with.
	section: Archive,
	/// The catalog's root broadcast clock: the wall anchor timings map through, after
	/// timescale conversion. Absent when the publisher exposes none.
	clock: Option<Clock>,
	/// This rendition's window over the broadcast timeline, fed by the catalog watcher's
	/// fan-out.
	live: Arc<segments::Producer>,
	/// The broadcast serving this rendition's media. A sibling is bound when the rendition is
	/// created and rebound if that publisher is replaced (see [`Upstream::bind`]).
	media: Media,
	/// The current publisher run: the embedder's label every segment URL carries (see
	/// [`Broadcaster::set_generation`](super::Broadcaster::set_generation)) and the init, built
	/// on first request. A catalog reconfigure rebuilds the whole rendition, while a new
	/// generation drops the init, since a restarted inline-codec publisher may carry new
	/// parameter sets under the same catalog.
	run: Mutex<Run>,
	/// Serializes init builds, so concurrent first requests fetch the keyframe group once.
	building: tokio::sync::Mutex<()>,
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

	/// Build a video rendition over the broadcast's timeline `section`, timed by the
	/// catalog root `clock`.
	///
	/// Fails when the config's `broadcast` reference escapes above the origin root: it names no
	/// broadcast, so there would be no media to serve.
	pub(crate) fn video(
		name: String,
		config: &VideoConfig,
		upstream: &Upstream,
		section: Archive,
		clock: Option<Clock>,
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
			clock,
			live: Arc::new(segments::Producer::new()),
			media: Media::bind(upstream, config.broadcast.as_ref())?,
			run: Mutex::default(),
			building: tokio::sync::Mutex::new(()),
		})
	}

	/// Build an audio rendition over the broadcast's timeline `section`, timed by the catalog
	/// root `clock`, failing like [`video`](Self::video).
	pub(crate) fn audio(
		name: String,
		config: &AudioConfig,
		upstream: &Upstream,
		section: Archive,
		clock: Option<Clock>,
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
			clock,
			live: Arc::new(segments::Producer::new()),
			media: Media::bind(upstream, config.broadcast.as_ref())?,
			run: Mutex::default(),
			building: tokio::sync::Mutex::new(()),
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
			duration: entry.duration,
			pts: entry.pts,
			end: Duration::from(entry.pts) + entry.duration,
			discontinuity: 0,
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

	/// List every segment URL under `generation` from now on, keeping the run's rows and init.
	pub(crate) fn label(&self, generation: Option<Arc<str>>) {
		self.run.lock().expect("run lock poisoned").generation = generation;
	}

	/// Start a new publisher run labeled `generation`, dropping the previous run's rows and init.
	///
	/// The rows go first: renders read the run before the rows, so one that sees the new label
	/// never lists the old run's segments under it.
	pub(crate) fn restart(&self, generation: Option<Arc<str>>) {
		self.live.clear();
		let mut run = self.run.lock().expect("run lock poisoned");
		*run = Run {
			generation,
			init: None,
			epoch: run.epoch + 1,
		};
	}

	fn run(&self) -> Run {
		self.run.lock().expect("run lock poisoned").clone()
	}

	/// The generation every segment URL currently carries.
	pub(crate) fn generation(&self) -> Option<Arc<str>> {
		self.run().generation
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
		kio::wait(|waiter| self.poll_playable(waiter)).await;
	}

	/// Poll until this rendition has a renderable media playlist.
	pub(crate) fn poll_playable(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.media.sync(&self.live);
		self.live.poll_playable(waiter)
	}

	/// Render this rendition's media playlist from the current timeline window, or `None` when
	/// there is nothing to serve yet (no segment, and the broadcast has not ended) -- a
	/// playlist with no segments confuses players, so a server should treat `None` as "not
	/// ready" rather than serve it.
	///
	/// The playlist names its init segment by a hash of the bytes, so it is also `None` until
	/// the init is known. An inline-parameter-set codec (no catalog `description`) only learns
	/// it from media: await [`init`](Self::init) first.
	///
	/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
	/// appended to every child URL (the init map and each segment), so a stock player that does
	/// not replay request headers still carries a credential on its follow-up requests. It is an
	/// argument rather than a [`Config`](super::Config) field because one broadcaster fans out
	/// to viewers holding different tokens.
	pub fn media_playlist(&self, query: Option<&str>) -> Option<String> {
		// Read the run before the rows (see `restart`).
		let run = self.run();
		let init = self.built_init(&run)?;
		self.is_playable()
			.then(|| super::render_media(&self.snapshot(run.generation), &init.hash, query))
	}

	/// Snapshot the media playlist from the current timeline window.
	#[cfg(test)]
	pub(crate) fn playlist(&self) -> Snapshot {
		self.snapshot(self.generation())
	}

	/// Snapshot the media playlist from the current timeline window, labeled `generation`.
	fn snapshot(&self, generation: Option<Arc<str>>) -> Snapshot {
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
			.map(|s| s.duration.as_secs_f64().ceil() as u64)
			.max()
			.unwrap_or(0);
		let target_duration = declared.max(observed).max(1);

		let program_date_time = window.segments.first().and_then(|first| self.wall_clock(first.pts));

		let mut previous: Option<u64> = None;
		let segments = window
			.segments
			.into_iter()
			.map(|s| {
				let discontinuity = previous.replace(s.discontinuity).is_some_and(|p| p != s.discontinuity);
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
			generation,
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
	/// verbatim, so `$Time$` addressing resolves exactly). `None` until the init is known, as
	/// for [`media_playlist`](Self::media_playlist).
	pub(crate) fn representation(&self) -> Option<mpd::Representation> {
		// Read the run before the rows (see `restart`).
		let run = self.run();
		let init = self.built_init(&run)?;
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
		Some(mpd::Representation {
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
			init: init.hash.clone(),
			generation: run.generation,
		})
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

	/// The wall-clock time of a media timestamp, when the catalog advertises its broadcast
	/// clock. Converts the timeline-timescale timestamp into the clock's timescale and applies
	/// the root mapping; an unrepresentable result maps to no time rather than a truncated one.
	pub(crate) fn wall_clock(&self, pts: moq_net::Timestamp) -> Option<SystemTime> {
		let clock = self.clock.as_ref()?;
		clock.wall_clock(pts).ok()
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
		Ok(self.load_init().await?.map(|init| init.bytes.clone()))
	}

	/// The init segment served at `init.{hash}.mp4`: `None` when `hash` names any other bytes,
	/// so a URL never serves an init it does not describe.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub(crate) async fn init_versioned(&self, hash: &str) -> Result<Option<Bytes>> {
		let init = self.load_init().await?;
		Ok(init.filter(|init| init.hash == hash).map(|init| init.bytes.clone()))
	}

	/// `run`'s init if it is already built, or can be without media: an out-of-band codec
	/// builds its init straight from the catalog.
	fn built_init(&self, run: &Run) -> Option<Arc<Init>> {
		if let Some(init) = &run.init {
			return Some(init.clone());
		}
		// A muxer error surfaces from `init()`, which the serve path awaits before rendering.
		let bytes = self.muxer().ok()?.init().ok()??;
		Some(self.cache_init(run.epoch, bytes))
	}

	/// Cache `bytes` as the init of the run numbered `epoch`, unless a restart has since
	/// replaced that run.
	fn cache_init(&self, epoch: u64, bytes: Bytes) -> Arc<Init> {
		let mut run = self.run.lock().expect("run lock poisoned");
		if run.epoch != epoch {
			return Arc::new(Init::new(bytes));
		}
		run.init.get_or_insert_with(|| Arc::new(Init::new(bytes))).clone()
	}

	async fn load_init(&self) -> Result<Option<Arc<Init>>> {
		let binding = self.media.sync(&self.live);
		let _building = self.building.lock().await;
		let run = self.run();
		if let Some(init) = run.init {
			return Ok(Some(init));
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
		Ok(Some(self.cache_init(run.epoch, bytes)))
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
/// variant, not a collapsed integer: session unauthorized and stream delivery-timeout are
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
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
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
