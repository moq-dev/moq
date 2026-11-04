//! One rendition: playlists from its media groups on the broadcast's grid, and segments fetched on
//! demand.

use kio::clock::Wall;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AudioConfig, Clock, Reference, VideoConfig};
use moq_mux::container::fmp4::Muxer;

use super::driver::Owner;
use super::playlist::{Segment, Snapshot};
use super::{grid, mpd, renditions, segments};
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

/// The fewest groups one segment request may fetch: see [`Rendition::max_groups`].
const MIN_SEGMENT_GROUPS: u64 = 1024;

/// The most groups one segment request ever fetches, whatever interval the catalog declares: see
/// [`Rendition::max_groups`]. 20 ms audio groups fill it in about 22 minutes.
const MAX_SEGMENT_GROUPS: u64 = 1 << 16;

/// DASH timelines are in milliseconds.
const TIMESCALE: u32 = 1000;

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

/// How far behind the live edge the media watcher and a cursor's feed still ask for a group. The
/// watcher starts its window that far back, and a segment is listed only once a later one starts,
/// so a cursor keeping up fetches it at the live edge; the budget only has to cover a burst (a
/// publisher catching up) and a cursor that fell behind. The default zero would deliver only the
/// newest group of a burst.
const FEED_MAX_DELAY: Duration = Duration::from_secs(30);

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
		let hash = rind::digest::digest(&rind::digest::SHA256, &bytes).as_ref()[..8]
			.iter()
			.map(|b| format!("{b:02x}"))
			.collect();
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
#[allow(
	clippy::large_enum_variant,
	reason = "one per rendition, so the variant sizes never add up to an allocation's worth; only large when extra features grow the video config"
)]
enum Config {
	Video(VideoConfig),
	Audio(AudioConfig),
}

/// The broadcast serving this rendition's media. A sibling that ends ends the rendition: it is
/// never rebound onto whatever serves the path next.
struct Media {
	handle: Mutex<Handle>,
	track: String,
	/// The rendition reads another broadcast than the catalog's.
	sibling: bool,
}

struct Handle {
	binding: Arc<moq_mux::Binding>,
	/// The query holding the track instance alive without subscribing to its media.
	track: Option<moq_net::track::Resolving>,
	/// The bound sibling ended, so only its retained rows drain.
	ended: bool,
}

impl Handle {
	fn capture(&mut self, broadcast: &moq_net::broadcast::Consumer, name: &str) {
		if self.track.is_none()
			&& let Ok(track) = broadcast.track(name)
		{
			// The lookup is lazy: the query installs the instance, and reads pin to it through
			// the query, since End may refuse any lookup made after this one.
			self.track = Some(track.info().into_inner());
		}
	}
}

impl Media {
	fn bind(source: &moq_mux::Source, reference: &impl hang::catalog::Reference, track: &str) -> moq_mux::Result<Self> {
		let rel = reference.broadcast();
		let mut handle = Handle {
			binding: Arc::new(source.bind(rel, reference.instance())?),
			track: None,
			ended: false,
		};
		if let Poll::Ready(Ok(broadcast)) = handle.binding.poll_broadcast(&kio::Waiter::noop()) {
			handle.capture(&broadcast, track);
		}
		Ok(Self {
			handle: Mutex::new(handle),
			track: track.to_string(),
			// `bind` already rejected an escaping reference, so both sides name a path.
			sibling: source.resolve_reference(rel) != source.resolve_reference(None),
		})
	}

	/// Whether incoming rows should be listed: not once the bound sibling ended.
	fn admits(&self, window: &segments::Producer) -> bool {
		self.sync(window);
		!self.handle.lock().expect("media lock poisoned").ended
	}

	/// End the window when a bound sibling has ended; retain its rows and media for draining.
	fn sync(&self, window: &segments::Producer) -> Arc<moq_mux::Binding> {
		self.poll_sync(&kio::Waiter::noop(), window)
	}

	/// Register the bound broadcast's close and retain its track before new lookups are refused.
	fn poll_sync(&self, waiter: &kio::Waiter, window: &segments::Producer) -> Arc<moq_mux::Binding> {
		let mut handle = self.handle.lock().expect("media lock poisoned");
		if let Poll::Ready(Ok(broadcast)) = handle.binding.poll_broadcast(waiter) {
			handle.capture(&broadcast, &self.track);
			if self.sibling && !handle.ended && broadcast.poll_closed(waiter).is_ready() {
				// End and Restart both drain only the pinned instance. Looking its path up again
				// could serve a replacement under the old track's rows.
				handle.ended = true;
				window.end();
			}
		}
		handle.binding.clone()
	}

	/// The settled authorization refusal of this rendition's referenced broadcast.
	fn refusal(&self) -> Option<crate::Error> {
		let handle = self.handle.lock().expect("media lock poisoned");
		match handle.binding.poll_broadcast(&kio::Waiter::noop()) {
			Poll::Ready(Err(err @ moq_mux::Error::Moq(moq_net::Error::Unauthorized))) => Some(err.into()),
			_ => None,
		}
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
	) -> Result<Option<(moq_net::broadcast::Consumer, moq_net::track::Consumer)>> {
		let track = kio::wait(|waiter| self.poll_track(binding, waiter)).await;
		// The bind can be refused while this read waits. Preserve that exact instance's
		// authorization error; other failed or disconnected references stay missing.
		if track.is_none()
			&& let Poll::Ready(Err(err @ moq_mux::Error::Moq(moq_net::Error::Unauthorized))) =
				binding.poll_broadcast(&kio::Waiter::noop())
		{
			return Err(err.into());
		}
		Ok(track)
	}

	fn poll_track(
		&self,
		binding: &moq_mux::Binding,
		waiter: &kio::Waiter,
	) -> Poll<Option<(moq_net::broadcast::Consumer, moq_net::track::Consumer)>> {
		let Ok(broadcast) = std::task::ready!(binding.poll_broadcast(waiter)) else {
			return Poll::Ready(None);
		};
		let mut handle = self.handle.lock().expect("media lock poisoned");
		handle.capture(&broadcast, &self.track);
		Poll::Ready(handle.track.as_ref().map(|query| (broadcast, query.track())))
	}
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
	config.delay = None;
	config.label = None;
	config.enabled = true;
	config.preference = 0;
	// Publishers estimate it and republish once it settles. Nothing muxed reads it.
	config.framerate = None;
	config
}

/// The audio counterpart of [`normalize_video`].
fn normalize_audio(config: &AudioConfig) -> AudioConfig {
	let mut config = config.clone();
	config.bitrate = None;
	config.jitter = None;
	config.delay = None;
	config.label = None;
	config.enabled = true;
	config
}

/// `EXT-X-TARGETDURATION` for segments `length` long: twice that, in whole seconds rounded up, and
/// at least one. That is twice the interval when a segment is one interval (maintainer,
/// 2026-11-03), and 2 s for 1 s segments of 20 ms audio groups.
///
/// A segment starts at the group containing its line, which can start up to one interval, at most
/// one length, before it, and ends where the next segment starts, at or before the next line, so a
/// segment of groups within the interval stays under twice the length, whatever shifts the GOP
/// phase: a join, a cut, a resize, or a live interval change. A gap runs from its line to the next
/// segment's start, which a resuming group past its own line puts under two lengths later. Only an
/// overrun, a group longer than the interval, can exceed it; one whose rounded `EXTINF` does is
/// listed anyway (see [`Rendition::overrun`]).
fn target_duration(length: Duration) -> u64 {
	let twice = length.saturating_mul(2);
	(twice.as_secs() + u64::from(twice.subsec_nanos() > 0)).max(1)
}

/// The segment [tag](segments::tag) of `track` numbered on a grid of `interval` and muxed under
/// the normalized `config`, hashing its catalog JSON, which every edge encodes alike.
fn tag<T: serde::Serialize>(track: &str, interval: Duration, config: &T) -> Arc<str> {
	// It was parsed from catalog JSON, so it encodes back.
	let config = serde_json::to_vec(config).expect("a catalog config encodes as JSON");
	let track = format!("{track}@{}", interval.as_nanos());
	Arc::from(segments::tag(&track, &config))
}

/// Where a retired rendition stops listing its segments, for its replacement to take over.
pub(crate) struct Handoff {
	/// The first segment it will not list. Every track of the broadcast numbers its segments on
	/// the same grid, so the replacement's segments continue the numbering.
	next: u64,
	/// Its `EXT-X-TARGETDURATION`, which a replacement serving the same playlist URI keeps.
	target_duration: u64,
	/// The retired instance, while it lists or still waits to list a segment: the replacement
	/// keeps listing those (see [`Rendition::after`]).
	previous: Option<Arc<Rendition>>,
}

/// Which segments a rendition lists, shared by its media watcher and [`Rendition::retire`] so a
/// replacement starts exactly where a retired instance stops.
#[derive(Default)]
struct Claim {
	/// Segments numbered below this are a predecessor's (see [`Rendition::after`]).
	start: u64,
	/// The first segment this instance will not list: the one after its open segment, else the
	/// one after the newest it listed. Final once a retired instance's window closes.
	next: u64,
	/// Set by [`Rendition::retire`]: only the groups it had already read are listed.
	retired: bool,
	/// A segment waits on a later one to start before it is listed.
	pending: bool,
	/// The newest group this instance read from its track.
	seen: Option<u64>,
	/// Set by [`Rendition::retire`] to [`seen`](Self::seen): the groups this instance still lists
	/// are those up to it, and a replacement on the same track lists only groups past it.
	boundary: Option<u64>,
}

/// A single HLS rendition: master-playlist metadata, its window over its media track's groups
/// numbered on the broadcast's grid (which renders its media playlist), and on-demand segment
/// fetching.
pub struct Rendition {
	/// The rendition's catalog ID, also its URL path component.
	pub name: String,
	/// Whether this rendition is video or audio.
	pub kind: Kind,
	/// Advertised bitrate for the master playlist `BANDWIDTH` attribute; read through
	/// [`bandwidth`](Self::bandwidth), refreshed in place by [`refresh`](Self::refresh).
	bitrate: RwLock<Option<u64>>,
	/// The latest catalog framerate (video only), refreshed in place by [`refresh`](Self::refresh):
	/// the DASH `frameRate`.
	framerate: RwLock<Option<f64>>,
	/// Whether the catalog currently enables the rendition, refreshed in place by
	/// [`refresh`](Self::refresh). A disabled rendition keeps its window and media playlist but
	/// is left out of the master playlist and the DASH manifest.
	enabled: AtomicBool,
	/// Coded width, for the master playlist `RESOLUTION` (video only).
	pub width: Option<u32>,
	/// Coded height, for the master playlist `RESOLUTION` (video only).
	pub height: Option<u32>,
	/// RFC 6381 codec string for the master playlist `CODECS` attribute.
	pub codec: String,

	config: Config,
	/// The container its media track carries, to decode a group's first frame.
	container: Arc<moq_mux::catalog::hang::Container>,
	/// The catalog's `media.join.interval`, which fixes [`segment`](Self::segment).
	interval: Duration,
	/// The segment length the grid numbers this rendition's segments on (see
	/// [`grid::segment_length`]).
	segment: Duration,
	/// The [tag](segments::tag) of the media track numbered on [`interval`](Self::interval)'s
	/// grid and muxed under this config, in every segment URL.
	tag: Arc<str>,
	/// `EXT-X-TARGETDURATION`, in whole seconds: twice the interval rounded up (see
	/// [`target_duration`]), or a predecessor's (see [`after`](Self::after)), until
	/// [`pin`](Self::pin) sets the one its playlist URI keeps while it stays in the catalog.
	target_duration: u64,
	/// Segments listed past the target duration, counted to rate-limit their warning.
	overruns: AtomicU64,
	/// The catalog's `media.clock`: the wall anchor segment times map through. Absent when the
	/// publisher exposes none.
	clock: Option<Clock>,
	/// This rendition's window over its segments.
	live: Arc<segments::Producer>,
	/// What every rendition of the broadcast shares: discontinuity counts and the run.
	fanout: renditions::Fanout,
	/// Follows the media track's groups into [`live`](Self::live); ends when the rendition drops.
	watcher: Mutex<Option<Owner>>,
	/// Set once the catalog drops or replaces the rendition (see [`retire`](Self::retire)).
	retiring: tokio::sync::watch::Sender<bool>,
	/// Which segments this instance lists.
	claim: Mutex<Claim>,
	/// The retired instance this one replaced under the same playlist URI, whose segments its playlist
	/// still lists ahead of its own until the window slides past them (see [`after`](Self::after)).
	/// Released once none is listed, or by a restart.
	previous: Mutex<Option<Arc<Rendition>>>,
	/// The broadcast serving this rendition's media. A sibling is bound when the rendition is
	/// created, and ends the rendition when it ends. Shared with a background init build.
	media: Arc<Media>,
	/// The current publisher run: the embedder's label every segment URL carries (see
	/// [`Broadcaster::set_generation`](super::Broadcaster::set_generation)) and the init, built
	/// on first request. A catalog reconfigure rebuilds the whole rendition, while a new
	/// generation drops the init, since a restarted inline-codec publisher may carry new
	/// parameter sets under the same catalog. Shared state so a render can wait for the init.
	run: kio::Producer<Run>,
	/// Serializes init builds, so concurrent first requests fetch the keyframe group once. Shared
	/// so a background build (see [`start_init`](Self::start_init)) holds it for its whole run.
	building: Arc<tokio::sync::Mutex<()>>,
	/// The background init build; ends when the rendition drops.
	build: Mutex<Option<Owner>>,
}

impl Rendition {
	/// Whether `config` describes the same media this rendition is already serving, i.e. whether
	/// it decodes and muxes identically from the same track. Fields the publisher revises
	/// without touching the media (its bitrate, jitter, and framerate estimates, a label) are
	/// ignored here, and the bitrate and framerate are picked up by [`refresh`](Self::refresh)
	/// instead.
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

	/// Whether this rendition follows `previous` onto the same media track: the same track name in
	/// the same broadcast and instance, whose group sequences continue across the two.
	fn continues(&self, previous: &Rendition) -> bool {
		use hang::catalog::Reference;
		let source = |rendition: &Rendition| match &rendition.config {
			Config::Video(config) => (config.broadcast().cloned(), config.instance().cloned()),
			Config::Audio(config) => (config.broadcast().cloned(), config.instance().cloned()),
		};
		self.track() == previous.track() && source(self) == source(previous)
	}

	/// The media track this rendition lists and serves.
	pub fn track(&self) -> &str {
		&self.media.track
	}

	/// The catalog's `media.join.interval`, which fixes the [segment length](Self::segment_length).
	pub fn interval(&self) -> Duration {
		self.interval
	}

	/// How long a segment is: the smallest whole multiple of the interval that is at least a
	/// second, the grid every rendition numbers its segments on.
	pub fn segment_length(&self) -> Duration {
		self.segment
	}

	/// The most groups one segment request fetches: one per millisecond of the segment length,
	/// between [`MIN_SEGMENT_GROUPS`] and [`MAX_SEGMENT_GROUPS`]. This bounds a request's work by
	/// the media it serves, while neither a publisher grouping far finer than its interval nor one
	/// declaring a huge interval can turn one request into an endless fetch loop.
	pub(crate) fn max_groups(&self) -> u64 {
		u64::try_from(self.segment.as_millis())
			.unwrap_or(u64::MAX)
			.clamp(MIN_SEGMENT_GROUPS, MAX_SEGMENT_GROUPS)
	}

	/// The advertised bitrate in bits per second, for the master playlist `BANDWIDTH` attribute
	/// and the DASH `bandwidth`. Falls back to a per-kind default when the catalog carries none.
	pub fn bandwidth(&self) -> u64 {
		self.bitrate().unwrap_or(default_bandwidth(self.kind))
	}

	/// Update mutable catalog metadata without resetting the rendition.
	///
	/// The publisher's estimator republishes the catalog whenever its measured bitrate moves, so
	/// this is the common update by far: rebuilding the rendition for it would reset the playlist
	/// window, the cached init segment, and `EXT-X-MEDIA-SEQUENCE` several times a minute. A
	/// disable keeps all three too, so a rendition that re-enables resumes where it was.
	pub(crate) fn refresh(&self, bitrate: Option<u64>, framerate: Option<f64>, enabled: bool) {
		*self.bitrate.write().expect("bitrate lock poisoned") = bitrate;
		*self.framerate.write().expect("framerate lock poisoned") = framerate;
		self.enabled.store(enabled, Ordering::Relaxed);
	}

	/// Whether the catalog enables this rendition, so the master playlist and DASH manifest
	/// list it. The media playlist serves either way.
	pub(crate) fn is_enabled(&self) -> bool {
		self.enabled.load(Ordering::Relaxed)
	}

	/// The latest catalog bitrate, preserving `None` for muxer-specific fallback behavior.
	fn bitrate(&self) -> Option<u64> {
		*self.bitrate.read().expect("bitrate lock poisoned")
	}

	/// The latest catalog framerate.
	fn framerate(&self) -> Option<f64> {
		*self.framerate.read().expect("framerate lock poisoned")
	}

	/// Build a video rendition numbering its media track's groups on a grid of `interval`, timed
	/// by the catalog's `media.clock`.
	///
	/// Fails when the config's `broadcast` reference escapes above the origin root, so it names no
	/// broadcast, or its container is unknown or malformed, so its media can't be read: either way
	/// there would be no media to serve.
	pub(crate) fn video(
		name: String,
		config: &VideoConfig,
		source: &moq_mux::Source,
		interval: Duration,
		clock: Option<Clock>,
		fanout: renditions::Fanout,
	) -> moq_mux::Result<Self> {
		let track = config.track(&name);
		let container = Arc::new(moq_mux::catalog::hang::Container::try_from(config)?);
		let media = Media::bind(source, config, track)?;
		Ok(Self {
			tag: tag(track, interval, &normalize_video(config)),
			container,
			name,
			kind: Kind::Video,
			bitrate: RwLock::new(config.bitrate),
			framerate: RwLock::new(config.framerate),
			enabled: AtomicBool::new(config.enabled),
			width: config.coded_width,
			height: config.coded_height,
			codec: config.codec.to_string(),
			config: Config::Video(config.clone()),
			target_duration: target_duration(grid::segment_length(interval)),
			overruns: AtomicU64::new(0),
			interval,
			segment: grid::segment_length(interval),
			clock,
			live: Arc::new(segments::Producer::new(0)),
			fanout,
			watcher: Mutex::new(None),
			retiring: tokio::sync::watch::Sender::new(false),
			claim: Mutex::default(),
			previous: Mutex::new(None),
			media: Arc::new(media),
			run: kio::Producer::default(),
			building: Arc::new(tokio::sync::Mutex::new(())),
			build: Mutex::new(None),
		})
	}

	/// Build an audio rendition numbering its groups on a grid of `interval`, failing like
	/// [`video`](Self::video).
	pub(crate) fn audio(
		name: String,
		config: &AudioConfig,
		source: &moq_mux::Source,
		interval: Duration,
		clock: Option<Clock>,
		fanout: renditions::Fanout,
	) -> moq_mux::Result<Self> {
		let track = config.track(&name);
		let container = Arc::new(moq_mux::catalog::hang::Container::try_from(config)?);
		let media = Media::bind(source, config, track)?;
		Ok(Self {
			tag: tag(track, interval, &normalize_audio(config)),
			container,
			name,
			kind: Kind::Audio,
			bitrate: RwLock::new(config.bitrate),
			framerate: RwLock::new(None),
			enabled: AtomicBool::new(config.enabled),
			width: None,
			height: None,
			codec: config.codec.to_string(),
			config: Config::Audio(config.clone()),
			target_duration: target_duration(grid::segment_length(interval)),
			overruns: AtomicU64::new(0),
			interval,
			segment: grid::segment_length(interval),
			clock,
			live: Arc::new(segments::Producer::new(0)),
			fanout,
			watcher: Mutex::new(None),
			retiring: tokio::sync::watch::Sender::new(false),
			claim: Mutex::default(),
			previous: Mutex::new(None),
			media: Arc::new(media),
			run: kio::Producer::default(),
			building: Arc::new(tokio::sync::Mutex::new(())),
			build: Mutex::new(None),
		})
	}

	/// Start following this rendition's media track onto the grid.
	pub(crate) fn watch(self: &Arc<Self>) {
		let watcher = self
			.fanout
			.spawn(watch_media(Arc::downgrade(self), self.retiring.subscribe()));
		*self.watcher.lock().expect("watcher lock poisoned") = Some(watcher);
	}

	/// Take over from a retired predecessor (see [`retire`](Self::retire)): keep its target
	/// duration and list only the segments it won't. Every track numbers its segments on the
	/// broadcast's grid, so the replacement continues the numbering even on another track.
	///
	/// The playlist URI keeps listing the predecessor's segments, under the
	/// init they were muxed for, until the window slides past them: a server never removes a
	/// segment from the middle or end of a live playlist (RFC 8216 6.2.2). This instance lists its
	/// own behind a discontinuity, once the predecessor has listed all it will, so the two never
	/// interleave.
	pub(crate) fn after(mut self, handoff: Option<&Handoff>) -> Self {
		let Some(handoff) = handoff else {
			return self;
		};
		// The playlist URI stays the same, so it keeps its target duration (RFC 8216 6.2.1).
		self.target_duration = handoff.target_duration;
		let start = handoff.next;
		self.claim = Mutex::new(Claim {
			start,
			next: start,
			..Claim::default()
		});
		self.live = Arc::new(segments::Producer::new(start));
		self.previous = Mutex::new(handoff.previous.clone());
		self
	}

	/// End the window once its open segment is listed, because the catalog dropped or replaced the
	/// rendition. Returns where a replacement takes over.
	///
	/// Its track may go on (a replacement may read it too), so its end can't end the window.
	/// Ending it at once would lose the last segment of every recording, since it is listed only
	/// once a later one starts or the track ends.
	pub(crate) fn retire(self: &Arc<Self>) -> Handoff {
		let (next, pending) = {
			let mut claim = self.claim.lock().expect("claim lock poisoned");
			claim.retired = true;
			claim.boundary = claim.seen;
			(claim.next, claim.pending)
		};
		self.retiring.send_replace(true);
		// Once retired, one that neither lists nor waits to list a segment never will, so the
		// replacement keeps only what this one was keeping: churn can't stack empty instances.
		let lists = pending || !self.live.window().segments.is_empty();
		Handoff {
			next,
			target_duration: self.target_duration,
			previous: match lists {
				true => Some(self.clone()),
				false => self.previous(),
			},
		}
	}

	fn previous(&self) -> Option<Arc<Rendition>> {
		self.previous.lock().expect("previous lock poisoned").clone()
	}

	/// The retired instances whose segments this one's playlist still lists, oldest first.
	fn lineage(&self) -> Vec<Arc<Rendition>> {
		let mut lineage = Vec::new();
		let mut next = self.previous();
		while let Some(rendition) = next {
			next = rendition.previous();
			lineage.push(rendition);
		}
		lineage.reverse();
		lineage
	}

	/// The predecessors' rows listed ahead of `own`, oldest first, each with its instance.
	///
	/// They are trimmed as one window with `own`, as if one instance had listed them all. Rows
	/// before a jump in the numbering are dropped, as a skip clears one instance's window. A
	/// predecessor none of whose rows is left is released, along with every older one, once none
	/// of them can list another.
	fn inherited(&self, own: &[segments::Row]) -> Vec<(Arc<Rendition>, segments::Row)> {
		let lineage = self.lineage();
		if lineage.is_empty() {
			return Vec::new();
		}
		// Each read whole, so its rows are all it will ever list once it reads closed.
		let windows: Vec<_> = lineage.iter().map(|rendition| rendition.live.window()).collect();
		let closed = windows.iter().all(|window| window.closed);
		let mut rows: std::collections::VecDeque<_> = lineage
			.iter()
			.zip(windows)
			.flat_map(|(rendition, window)| {
				window
					.segments
					.into_iter()
					.map(move |row| (Some(rendition.clone()), row))
			})
			.collect();
		let inherited = rows.len();
		rows.extend(own.iter().map(|row| (None, row.clone())));
		// Records skipped across the hand-over leave a jump the inherited rows can't bridge.
		let numbers: Vec<_> = rows.iter().map(|(_, row)| row.segment).collect();
		let mut trimmed = broken(&numbers, inherited);
		rows.drain(..trimmed);
		let window = self.fanout.window();
		while trimmed < inherited {
			let rest = match rows.get(1) {
				Some((_, second)) => rows.back().expect("two rows").1.end.saturating_sub(second.pts.into()),
				None => Duration::ZERO,
			};
			if !segments::evicts(window, rows.len(), rest) {
				break;
			}
			rows.pop_front();
			trimmed += 1;
		}
		let rows: Vec<_> = rows
			.into_iter()
			.take(inherited - trimmed)
			.map(|(rendition, row)| (rendition.expect("an inherited row"), row))
			.collect();
		// Nothing lists anything older than the oldest instance left, whose rows followed every
		// older one's last.
		match rows.first() {
			Some((oldest, _)) => *oldest.previous.lock().expect("previous lock poisoned") = None,
			None if closed => *self.previous.lock().expect("previous lock poisoned") = None,
			None => {}
		}
		rows
	}

	/// Advertise `target_duration` seconds as `EXT-X-TARGETDURATION`: the one this rendition's
	/// playlist URI keeps while it stays in the catalog.
	pub(crate) fn pin(mut self, target_duration: u64) -> Self {
		self.target_duration = target_duration;
		self
	}

	/// The `EXT-X-TARGETDURATION` of this rendition's media playlist, in whole seconds.
	pub fn target_duration(&self) -> u64 {
		self.target_duration
	}

	/// How many segments were listed past the target duration.
	#[cfg(test)]
	pub(crate) fn overruns(&self) -> u64 {
		self.overruns.load(Ordering::Relaxed)
	}

	/// Warn that segment `segment`, `duration` long, is listed past the target duration. Only on
	/// the 1st, 2nd, 4th, 8th... overrun, so a publisher whose every GOP overruns can't flood the
	/// log.
	fn overrun(&self, segment: u64, duration: Duration) {
		let overruns = self.overruns.fetch_add(1, Ordering::Relaxed) + 1;
		if overruns.is_power_of_two() {
			tracing::warn!(
				rendition = %self.name,
				segment,
				?duration,
				target_duration = self.target_duration,
				overruns,
				"a group overruns the declared join interval; listing its segment past EXT-X-TARGETDURATION"
			);
		}
	}

	/// This rendition's identity in its broadcast.
	fn key(&self) -> renditions::Key {
		(self.kind, self.name.clone())
	}

	/// The [tag](segments::tag) every segment URL carries.
	#[cfg(test)]
	pub(crate) fn tag(&self) -> &str {
		&self.tag
	}

	/// List one segment, unless the bound sibling ended.
	fn push(&self, row: segments::Row) {
		if self.media.admits(&self.live) {
			self.live.push(row, self.fanout.window());
			// Release the predecessors this push slid out, even with nobody rendering.
			if self.previous().is_some() {
				self.inherited(&self.live.window().segments);
			}
		}
	}

	/// List every segment URL under `generation` from now on, keeping the run's rows and init.
	pub(crate) fn label(&self, generation: Option<Arc<str>>) {
		self.run_mut().generation = generation;
	}

	/// Start a new publisher run labeled `generation`, dropping the previous run's rows and init.
	///
	/// The rows go first: renders read the run before the rows, so one that sees the new label
	/// never lists the old run's segments under it.
	pub(crate) fn restart(&self, generation: Option<Arc<str>>) {
		// A build parked on the old run's open group would hold `building`, so the new run could
		// never start its own.
		self.build.lock().expect("build lock poisoned").take();
		self.live.clear();
		// Its predecessors' rows are the previous run's too.
		*self.previous.lock().expect("previous lock poisoned") = None;
		let mut run = self.run_mut();
		*run = Run {
			generation,
			init: None,
			epoch: run.epoch + 1,
		};
	}

	fn run(&self) -> Run {
		self.run.read().clone()
	}

	fn run_mut(&self) -> kio::Mut<'_, Run> {
		self.run
			.write()
			.unwrap_or_else(|_| unreachable!("the run is never closed"))
	}

	/// Poll until the current run's init is built.
	pub(crate) fn poll_init(&self, waiter: &kio::Waiter) -> Poll<()> {
		match self.run.poll_ref(waiter, |run| match run.init {
			Some(_) => Poll::Ready(()),
			None => Poll::Pending,
		}) {
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// The generation every segment URL currently carries.
	pub(crate) fn generation(&self) -> Option<Arc<str>> {
		self.run().generation
	}

	/// Mark this rendition's window ended (its media track finished, or it left the catalog).
	pub(crate) fn end(&self) {
		self.live.end();
	}

	/// End the window at the segments listed now, because another publisher run replaced this
	/// one, and stop following its media: what the replaced run lists later is never listed.
	pub(crate) fn cut(&self) {
		let watcher = self.watcher.lock().expect("watcher lock poisoned").take();
		drop(watcher);
		self.live.cut();
	}

	/// Fail this rendition's window with `err`.
	pub(crate) fn fail(&self, err: crate::Error) {
		self.live.fail(err);
	}

	/// A cursor over this rendition's segments, with their media, in order.
	/// For a recorder mirroring the broadcast; see [`segments::Consumer`].
	pub fn segments(self: &Arc<Self>) -> segments::Consumer {
		self.live.subscribe(self.clone())
	}

	/// Close the window after recording its end or failure, so segment cursors drain it.
	/// An unmarked close fails the recording instead of reporting completion.
	pub(crate) fn close(&self) {
		self.live.close();
	}

	/// Resolve once the playlist is renderable or failed, i.e. once
	/// [`media_playlist`](Self::media_playlist) would return `Some` or an error. Bounding the wait
	/// is the caller's policy (the serve path wraps this in its own timeout).
	pub async fn playable(&self) {
		kio::wait(|waiter| self.poll_playable(waiter)).await;
	}

	/// Poll until this rendition is playable (it lists a segment, or its media track ended), its
	/// media is denied, or its window failed.
	pub(crate) fn poll_playable(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.poll_media(waiter);
		if self.media.refusal().is_some() {
			return Poll::Ready(());
		}
		// A replacement's playlist lists its predecessors' segments until it has its own.
		if self.live.poll_listed(waiter).is_ready()
			|| self
				.lineage()
				.iter()
				.any(|rendition| rendition.live.poll_rows(waiter).is_ready())
		{
			return Poll::Ready(());
		}
		Poll::Pending
	}

	/// Resolve once the DASH [representation](Self::representation) lists a segment or this
	/// rendition fails, as [`playable`](Self::playable) does for the media playlist.
	pub(crate) async fn represented(&self) {
		kio::wait(|waiter| self.poll_represented(waiter)).await;
	}

	/// Poll until this instance lists a segment of its own (or its media track ended), its media is
	/// denied, or its window failed. Unlike [`poll_playable`](Self::poll_playable), a
	/// predecessor's segments don't count: the DASH manifest lists none of them.
	pub(crate) fn poll_represented(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.poll_media(waiter);
		if self.media.refusal().is_some() {
			return Poll::Ready(());
		}
		self.live.poll_listed(waiter)
	}

	/// End the window if the bound sibling ended, registering `waiter` to wake when it does, and
	/// return the binding this rendition's media is fetched through now.
	pub(crate) fn poll_media(&self, waiter: &kio::Waiter) -> Arc<moq_mux::Binding> {
		self.media.poll_sync(waiter, &self.live)
	}

	/// Render this rendition's media playlist from its current window, or `None` when there is
	/// nothing to serve yet (no segment, and the media track has not ended) -- a playlist with no
	/// segments confuses players, so a server should treat `None` as "not ready" rather than
	/// serve it.
	///
	/// Fails with [`MediaFailed`](crate::Error::MediaFailed) once this rendition's media track
	/// failed, or [`CatalogFailed`](crate::Error::CatalogFailed) once the catalog did, rather
	/// than ending a truncated playlist with `EXT-X-ENDLIST`. A server should answer that with an
	/// uncacheable error.
	///
	/// A denied referenced broadcast propagates the underlying authorization error; it is
	/// not missing media. The HTTP server answers it with 403 and no-store.
	///
	/// The playlist names each init segment by a hash of the bytes, so it is also `None` until
	/// the init of every listed segment is known. An inline-parameter-set codec (no catalog `description`) only learns
	/// it from media: await [`init`](Self::init) first.
	///
	/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
	/// appended to every child URL (the init map and each segment), so a stock player that does
	/// not replay request headers still carries a credential on its follow-up requests. It is an
	/// argument rather than a [`Config`](super::Config) field because one broadcaster fans out
	/// to viewers holding different tokens.
	pub fn media_playlist(&self, query: Option<&str>) -> Result<Option<String>> {
		// Read the run before the rows (see `restart`).
		let run = self.run();
		let snapshot = self.snapshot_as(run.generation.clone())?;
		if snapshot.segments.is_empty() && !snapshot.finished {
			return Ok(None);
		}
		// A predecessor's segment maps its own init, `""` while that is not built.
		if snapshot.segments.iter().any(|s| s.map.as_deref() == Some("")) {
			return Ok(None);
		}
		// Nothing maps this instance's init until it lists a segment of its own, and an inline
		// codec builds it only from that segment.
		let mapped = snapshot.segments.is_empty() || snapshot.segments.iter().any(|s| s.map.is_none());
		let init = match mapped {
			true => match self.built_init(&run) {
				Some(init) => init.hash.clone(),
				None => return Ok(None),
			},
			false => String::new(),
		};
		Ok(Some(super::render_media(&snapshot, &init, query)))
	}

	/// Wait until the media playlist is servable, then render it: at least one segment is
	/// listed and the `EXT-X-MAP` init segment it names is built, so a player never loads a
	/// map that 404s. `None` when the init cannot be built yet.
	///
	/// Waits without bound for the first segment; bounding it is the caller's policy. `query`
	/// propagates exactly as in [`media_playlist`](Self::media_playlist).
	///
	/// A failed rendition or catalog fails it as in [`media_playlist`](Self::media_playlist).
	pub async fn playlist(&self, query: Option<&str>) -> Result<Option<String>> {
		// A denied reference is refused before anything else, so no other error masks it.
		self.authorize()?;
		self.playable().await;
		// A failed media track may never supply the keyframe an inline init waits for.
		self.check()?;
		// Build the inits the listed segments map. An inline-codec init needs a keyframe group
		// fetched first. Building it caches, so the player's follow-up GET of the init is free.
		let own = self.live.window().segments;
		let mut inherited = self.inherited(&own);
		inherited.dedup_by(|(a, _), (b, _)| Arc::ptr_eq(a, b));
		for (rendition, _) in &inherited {
			if rendition.load_init().await?.is_none() {
				return Ok(None);
			}
		}
		// This instance's own init is mapped only once it lists a segment (see `media_playlist`).
		let mapped = !own.is_empty() || inherited.is_empty();
		if mapped && self.load_init().await?.is_none() {
			return Ok(None);
		}
		let rendered = self.media_playlist(query)?;
		if rendered.is_some() || mapped {
			return Ok(rendered);
		}
		// It listed its first segment meanwhile.
		if self.load_init().await?.is_none() {
			return Ok(None);
		}
		self.media_playlist(query)
	}

	/// Snapshot the media playlist from the current window.
	#[cfg(test)]
	pub(crate) fn snapshot(&self) -> Snapshot {
		self.try_snapshot().expect("the rendition failed")
	}

	/// Snapshot the media playlist, or the error its rendition failed with.
	#[cfg(test)]
	pub(crate) fn try_snapshot(&self) -> Result<Snapshot> {
		self.snapshot_as(self.generation())
	}

	/// Snapshot the media playlist from the current window, labeled `generation`.
	fn snapshot_as(&self, generation: Option<Arc<str>>) -> Result<Snapshot> {
		self.media.sync(&self.live);
		let window = self.live.window();
		if let Some(err) = self.failure(&window) {
			return Err(err);
		}
		let finished = window.ended;
		let inherited = self.inherited(&window.segments);
		Ok(self.snapshot_from_window(inherited, window, finished, generation))
	}

	/// Why listings refuse: the referenced media is denied, or the window failed.
	fn failure(&self, window: &segments::Window) -> Option<crate::Error> {
		self.media.refusal().or_else(|| window.failed.clone())
	}

	/// Return bound media or its authorization refusal, leaving the window alone.
	pub(crate) fn authorize(&self) -> Result<Arc<moq_mux::Binding>> {
		let binding = self.media.sync(&self.live);
		match self.media.refusal() {
			Some(err) => Err(err),
			None => Ok(binding),
		}
	}

	/// Refuse denied media or a failed window (see [`failure`](Self::failure)).
	pub(crate) fn check(&self) -> Result<()> {
		self.authorize()?;
		match self.live.failure() {
			Some(err) => Err(err),
			None => Ok(()),
		}
	}

	/// Whether this rendition failed while the catalog did not, so listings leave it out while
	/// its siblings stay available.
	pub(crate) fn failed_alone(&self) -> bool {
		self.fanout.failure().is_none() && self.check().is_err()
	}

	/// Poll until listings of this rendition refuse (see [`check`](Self::check)), waking when its
	/// media is denied or its media track fails.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) fn poll_refused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.poll_media(waiter);
		let _ = self.live.poll_failed(waiter);
		match self.check() {
			Err(_) => Poll::Ready(()),
			Ok(()) => Poll::Pending,
		}
	}

	/// The snapshot listing `inherited` (see [`inherited`](Self::inherited)), then `window`.
	///
	/// An inherited segment maps its instance's init, or `""` while that is not built, which
	/// [`media_playlist`](Self::media_playlist) refuses to render.
	fn snapshot_from_window(
		&self,
		inherited: Vec<(Arc<Rendition>, segments::Row)>,
		window: segments::Window,
		finished: bool,
		generation: Option<Arc<str>>,
	) -> Snapshot {
		let media_sequence = inherited.first().map_or(window.sequence, |(_, row)| row.segment);
		let discontinuity_sequence = inherited
			.first()
			.map_or(window.discontinuity, |(_, row)| row.discontinuity);
		let rows = inherited
			.iter()
			.map(|(rendition, row)| {
				// Cached once built, so this is a read for every row after the first.
				let map = rendition.built_init(&rendition.run());
				(Some(map.map_or_else(String::new, |init| init.hash.clone())), row)
			})
			.chain(window.segments.iter().map(|row| (None, row)));
		let mut previous = None;
		let segments = rows
			.map(|(map, s)| {
				let discontinuity = previous.replace(s.discontinuity).is_some_and(|p| p != s.discontinuity);
				Segment {
					segment: s.segment,
					tag: s.tag.to_string(),
					map,
					duration: s.duration,
					gap: s.stop,
					discontinuity,
					program_date_time: self.wall_clock(s.pts),
				}
			})
			.collect();
		Snapshot {
			target_duration: self.target_duration,
			media_sequence,
			discontinuity_sequence,
			segments,
			finished,
			generation,
		}
	}

	/// Whether the multivariant playlist advertises this rendition, i.e. a player can start it.
	///
	/// Every segment starts at a group, and every group at a keyframe, so a rendition can start
	/// as soon as it lists anything.
	pub(crate) fn is_advertised(&self) -> bool {
		self.poll_advertised(&kio::Waiter::noop()).is_ready()
	}

	/// Poll until [`is_advertised`](Self::is_advertised), for the master route's long-poll.
	pub(crate) fn poll_advertised(&self, waiter: &kio::Waiter) -> Poll<()> {
		// Its playlist answers an error, so new players start on a sibling instead.
		if self.failed_alone() {
			return Poll::Pending;
		}
		self.poll_playable(waiter)
	}

	/// This rendition's DASH representation: its master-level metadata plus its segments as
	/// `(t, d)` pairs in milliseconds (the row values verbatim, so `$Time$` addressing resolves
	/// exactly). Empty until the init is known, as for [`media_playlist`](Self::media_playlist),
	/// and failing like it.
	pub(crate) fn representation(&self) -> Result<Option<mpd::Representation>> {
		// Read the run before the rows (see `restart`).
		let run = self.run();
		self.media.sync(&self.live);
		let window = self.live.window();
		if let Some(err) = self.failure(&window) {
			return Err(err);
		}
		let Some(init) = self.built_init(&run) else {
			return Ok(None);
		};
		let ended = window.ended;
		let segments = window
			.segments
			.iter()
			.filter(|row| !row.stop)
			.map(|row| {
				let t = row.pts.as_millis() as u64;
				let d = row.end.saturating_sub(row.pts.into()).as_millis() as u64;
				(t, d)
			})
			// A zero-duration segment cannot be requested by DASH timing.
			.filter(|(_, d)| *d > 0)
			.collect();
		let (framerate, sample_rate, channel_count) = match &self.config {
			Config::Video(_) => (self.framerate(), None, None),
			Config::Audio(config) => (None, Some(config.sample_rate), Some(config.channel_count)),
		};
		Ok(Some(mpd::Representation {
			name: self.name.clone(),
			kind: self.kind,
			bandwidth: self.bandwidth(),
			codec: self.codec.clone(),
			width: self.width,
			height: self.height,
			framerate,
			sample_rate,
			channel_count,
			timescale: TIMESCALE,
			segments,
			ended,
			tag: self.tag.to_string(),
			init: init.hash.clone(),
			generation: run.generation.clone(),
		}))
	}

	/// Fetch and transmux the segment of the track tagged `tag` at `at`: its number, or
	/// its start in milliseconds (DASH `$Time$`).
	async fn segment_at(&self, tag: &str, at: segments::At) -> Result<Option<Bytes>> {
		self.authorize()?;
		let sequence = match at {
			segments::At::Sequence(sequence) => sequence,
			segments::At::Pts(pts) => match self.live.segment_number_at(tag, pts) {
				Some(sequence) => sequence,
				None => return Ok(None),
			},
		};
		self.numbered(tag, sequence).await
	}

	/// The wall-clock time of a media timestamp, when the catalog advertises its broadcast
	/// clock; an unrepresentable result maps to no time rather than a truncated one.
	pub(crate) fn wall_clock(&self, pts: moq_net::Timestamp) -> Option<Wall> {
		let clock = self.clock.as_ref()?;
		clock.wall_clock(pts).ok()
	}

	/// A muxer at the timescale this rendition's config chose, with the latest bitrate.
	///
	/// Each segment times its last sample from its own row, so no catalog estimate changes
	/// the media bytes.
	pub(crate) fn muxer(&self) -> Result<Muxer> {
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

	/// Subscribe to the media track on `binding` once it resolves: `None` when it never will.
	pub(crate) fn poll_subscribe(
		&self,
		binding: &moq_mux::Binding,
		waiter: &kio::Waiter,
	) -> Poll<Option<moq_net::track::Subscribing>> {
		let track = std::task::ready!(self.media.poll_track(binding, waiter));
		let subscription = moq_net::track::Subscription::default()
			.with_delay(moq_net::track::Delay::default().with_max(FEED_MAX_DELAY));
		Poll::Ready(track.map(|(_, track)| track.subscribe(subscription).into_inner()))
	}

	/// The CMAF init segment served at `init.{hash}.mp4`, built on first request and cached.
	///
	/// `None` when it cannot be built yet, or `hash` (a [`Resource::Init`](super::Resource::Init)
	/// hash) names any other bytes, so a URL never serves an init it does not describe.
	///
	/// For inline-parameter-set codecs (no catalog `description`), the parameter sets are
	/// resolved by fetching the newest listed segment's first group. A bound track's retained
	/// media remains usable after its broadcast ends or is replaced; replacement media is never
	/// read.
	pub async fn init(&self, hash: &str) -> Result<Option<Bytes>> {
		// A predecessor's segments still listed map its init, which was built before any playlist
		// listed them (see `media_playlist`), so it is served without building this instance's.
		// Only the instance supplying the bytes is authorized: a denied one leaves them to this
		// instance, which may have the same config.
		for rendition in self.lineage() {
			if let Some(init) = rendition.built_init(&rendition.run()).filter(|init| init.hash == hash)
				&& rendition.authorize().is_ok()
			{
				return Ok(Some(init.bytes.clone()));
			}
		}
		let init = self.load_init().await?;
		Ok(init.filter(|init| init.hash == hash).map(|init| init.bytes.clone()))
	}

	/// The current CMAF init segment, whatever its hash, built on first request and cached.
	pub(crate) async fn latest_init(&self) -> Result<Option<Bytes>> {
		Ok(self.load_init().await?.map(|init| init.bytes.clone()))
	}

	/// `run`'s init if it is already built, or can be without media: an out-of-band codec
	/// builds its init straight from the catalog.
	fn built_init(&self, run: &Run) -> Option<Arc<Init>> {
		if let Some(init) = &run.init {
			return Some(init.clone());
		}
		// A muxer error surfaces from `bootstrap`, which builds from media.
		let bytes = self.muxer().ok()?.init().ok()??;
		Some(self.cache_init(run.epoch, bytes))
	}

	/// Cache `bytes` as the init of the run numbered `epoch`, unless a restart has since
	/// replaced that run.
	fn cache_init(&self, epoch: u64, bytes: Bytes) -> Arc<Init> {
		let mut run = self.run_mut();
		if run.epoch != epoch {
			return Arc::new(Init::new(bytes));
		}
		run.init.get_or_insert_with(|| Arc::new(Init::new(bytes))).clone()
	}

	/// Build the init in the background unless it is built or a build is already running, so a
	/// later [`representation`](Self::representation) lists this rendition without any render
	/// waiting on its media.
	///
	/// The build never holds the rendition itself, and ends when the rendition drops, so
	/// one stuck on an open group cannot keep a torn-down export's state alive.
	pub(crate) fn start_init(self: &Arc<Self>) {
		let Ok(binding) = self.authorize() else {
			return;
		};
		let run = self.run();
		if self.built_init(&run).is_some() {
			return;
		}
		// Taken before spawning, so renders never queue tasks behind a build stuck on an open group.
		let Ok(building) = self.building.clone().try_lock_owned() else {
			return;
		};
		let read = match self.muxer().map(|muxer| self.init_read(muxer)) {
			Ok(Some(read)) => read,
			Ok(None) => return,
			Err(err) => {
				tracing::warn!(rendition = %self.name, %err, "failed to build init segment");
				return;
			}
		};
		let media = self.media.clone();
		let rendition = Arc::downgrade(self);
		let name = self.name.clone();
		let build = self.fanout.spawn(async move {
			let _building = building;
			match read.read(&media, &binding).await {
				Ok(Some(bytes)) => {
					if let Some(rendition) = rendition.upgrade() {
						rendition.cache_init(run.epoch, bytes);
					}
				}
				Ok(None) => {}
				Err(err) => tracing::warn!(rendition = %name, %err, "failed to build init segment"),
			}
		});
		*self.build.lock().expect("build lock poisoned") = Some(build);
	}

	/// Whether the init is missing while a build of it is running, so a later render may list
	/// this rendition.
	pub(crate) fn init_pending(&self) -> bool {
		self.run().init.is_none() && self.building.try_lock().is_err()
	}

	async fn load_init(&self) -> Result<Option<Arc<Init>>> {
		let binding = self.authorize()?;
		// A transmuxed segment can cache the init while another bootstrap holds `building`,
		// blocked on an unfinished group.
		if let Some(init) = self.run().init {
			return Ok(Some(init));
		}
		let _building = self.building.lock().await;
		self.bootstrap(&binding).await
	}

	/// Build and cache the init, holding `building`.
	async fn bootstrap(&self, binding: &moq_mux::Binding) -> Result<Option<Arc<Init>>> {
		let run = self.run();
		if let Some(init) = run.init {
			return Ok(Some(init));
		}

		// An out-of-band codec builds its init straight from the catalog; an inline one returns
		// `None` until a keyframe group is read.
		let muxer = self.muxer()?;
		if let Some(bytes) = muxer.init()? {
			return Ok(Some(self.cache_init(run.epoch, bytes)));
		}
		let Some(read) = self.init_read(muxer) else {
			return Ok(None);
		};
		let Some(bytes) = read.read(&self.media, binding).await? else {
			return Ok(None);
		};
		Ok(Some(self.cache_init(run.epoch, bytes)))
	}

	/// The listed groups an inline-codec init is read from, or `None` while none is listed.
	fn init_read(&self, muxer: Muxer) -> Option<InitRead> {
		let groups = self.live.init_groups();
		(!groups.is_empty()).then_some(InitRead { muxer, groups })
	}

	/// Fetch and transmux the segment a [`Resource::Segment`](super::Resource::Segment) URL names.
	///
	/// Fetches every group the segment holds on this rendition (an audio segment packs many
	/// short groups) and encodes them as a single CMAF fragment. `None` when the segment isn't in
	/// the playlist window, is a gap, or its groups already
	/// left the relay cache. After End or Restart, only the bound instance's retained media
	/// drains; a replacement's media is never read under these rows. A denied referenced
	/// broadcast returns its authorization error even when the requested row is absent.
	///
	/// Only the current run's URLs are served: `None` when `id` names another
	/// [generation](super::Broadcaster::set_generation), since a restart reuses segment numbers
	/// and another run's URL would name different bytes.
	pub async fn segment(&self, id: &segments::Id) -> Result<Option<Bytes>> {
		// A predecessor's segments still listed are muxed under its config. A config that returns
		// gets its tag back, but the instances' segment numbers never overlap.
		if let Some(rendition) = self
			.lineage()
			.into_iter()
			.find(|rendition| *rendition.tag == *id.tag && rendition.live.holds(&id.tag, id.at))
		{
			return rendition.own_segment(id).await;
		}
		self.own_segment(id).await
	}

	/// Fetch and transmux the segment `id` names among this instance's own rows; see
	/// [`segment`](Self::segment).
	async fn own_segment(&self, id: &segments::Id) -> Result<Option<Bytes>> {
		self.authorize()?;
		let current = || self.generation().as_deref() == id.generation.as_deref();
		if !current() {
			return Ok(None);
		}
		let segment = self.segment_at(&id.tag, id.at).await?;
		// A generation change while fetching may have swapped the rows under the lookup.
		Ok(segment.filter(|_| current()))
	}

	/// Fetch and transmux the segment numbered `segment` on the track tagged `tag`,
	/// whatever the run; see [`segment`](Self::segment).
	pub(crate) async fn numbered(&self, tag: &str, segment: u64) -> Result<Option<Bytes>> {
		self.authorize()?;
		let Some(row) = self.live.row(tag, segment) else {
			return Ok(None);
		};
		self.fetch(&row).await
	}

	/// Fetch and transmux `row`'s groups on this rendition; see [`segment`](Self::segment).
	pub(crate) async fn fetch(&self, row: &segments::Row) -> Result<Option<Bytes>> {
		let binding = self.authorize()?;
		// Read the run before resolving the row (see `restart`), so an init decoded from its
		// groups is never cached for a run that started while they were being fetched.
		let run = self.run();
		if row.stop {
			return Ok(None);
		}
		let groups = row.groups.clone();

		// A segment must be served whole. If its group span exceeds the safety cap (a publisher
		// grouping far finer than its interval), 404 rather than silently truncating it to a
		// segment shorter than its advertised duration. The live window lists such a segment as a
		// gap instead, so only a row from elsewhere reaches this.
		if groups.end.saturating_sub(groups.start) > self.max_groups() {
			tracing::warn!(
				?groups,
				"segment spans more groups than one request fetches; refusing to truncate"
			);
			return Ok(None);
		}

		let Some((broadcast, track)) = self.media.track(&binding).await? else {
			return Ok(None);
		};

		let mut muxer = self.muxer()?;
		// Accumulate every group's frames into ONE fragment, so duration inference sees each
		// frame's true successor. A per-group fragment would mis-time the trailing sample of every
		// group (its real successor lives in the next group), audible on audio segments that span
		// many groups.
		let mut frames = Vec::new();
		for sequence in groups {
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
				segment = row.segment,
				group = sequence,
				err = %miss,
				"abandoning an advertised segment: a group is unavailable"
			);
			return Ok(None);
		}
		if frames.is_empty() {
			return Ok(None);
		}
		// The last presented sample has no successor to time it, so the fragment ends where the
		// next group starts, unless its codec or a duration marker times it.
		let end = row
			.content
			.and_then(|content| moq_net::Timestamp::from_micros(content.as_micros() as u64).ok());
		let fragment = muxer.fragment(row.segment as u32, &frames, end)?;
		// Inline codec metadata was already decoded from these completed groups.
		// Reuse it instead of fetching a newer, possibly unfinished group.
		if let Some(bytes) = muxer.init()? {
			self.cache_init(run.epoch, bytes);
		}
		Ok(Some(fragment))
	}
}

#[cfg(test)]
impl Rendition {
	/// Fetch the listed segment numbered `segment`.
	pub(crate) async fn listed_segment(&self, segment: u64) -> Result<Option<Bytes>> {
		self.numbered(&self.tag.clone(), segment).await
	}

	/// Fetch the listed segment starting at `time` milliseconds.
	pub(crate) async fn listed_segment_at(&self, time: u64) -> Result<Option<Bytes>> {
		self.segment_at(&self.tag.clone(), segments::At::Pts(time)).await
	}
}

/// How many leading rows of `segments` a jump in their numbering cuts off, looking only at the
/// first `inherited` rows and the one after them.
///
/// A media playlist numbers its rows from the first one's segment, so rows listed before a jump
/// would shift every later row's media sequence number once they slide out.
fn broken(segments: &[u64], inherited: usize) -> usize {
	(1..segments.len().min(inherited + 1))
		.rev()
		.find(|&i| segments[i - 1].checked_add(1) != Some(segments[i]))
		.unwrap_or(0)
}

/// Follow a rendition's media track into its window, numbering its groups on the grid, until the
/// track ends or fails.
///
/// A segment is listed once a later one starts, which bounds its groups and its `EXTINF`, or once
/// the track ends. A watcher joining mid-segment can't know the groups before its first, so it
/// lists nothing before the first segment it sees start (see [`grid::Grid::starts`]): another edge
/// lists that segment whole, and the same URL must name the same bytes everywhere.
async fn watch_media(rendition: Weak<Rendition>, mut retiring: tokio::sync::watch::Receiver<bool>) {
	let Some((track, interval, segment, container)) = rendition
		.upgrade()
		.map(|r| (r.track().to_string(), r.interval, r.segment, r.container.clone()))
	else {
		return;
	};
	let result: Result<()> = async {
		// A replacement lists nothing until its predecessor has listed all it will, so their
		// segments never interleave and its own start a new discontinuity: they decode under
		// another init. It then starts where the predecessor finally stopped, and on the same
		// track past the last group the predecessor read, which another config may have muxed.
		let previous = rendition.upgrade().and_then(|rendition| rendition.previous());
		let mut broken = previous.is_some();
		let mut after: Option<u64> = None;
		if let Some(previous) = previous {
			tokio::select! {
				_ = previous.live.closed() => {}
				_ = retiring.wait_for(|retiring| *retiring) => return Ok(()),
			}
			let (next, boundary) = {
				let claim = previous.claim.lock().expect("claim lock poisoned");
				(claim.next, claim.boundary)
			};
			// Another track, or the same name in another broadcast, numbers its groups afresh.
			let continues = rendition
				.upgrade()
				.is_some_and(|rendition| rendition.continues(&previous));
			after = boundary.filter(|_| continues);
			if let Some(rendition) = rendition.upgrade() {
				let mut claim = rendition.claim.lock().expect("claim lock poisoned");
				claim.start = claim.start.max(next);
				claim.next = claim.next.max(next);
			}
		}
		let (media, binding) = match rendition.upgrade() {
			Some(rendition) => (rendition.media.clone(), rendition.authorize()?),
			None => return Ok(()),
		};
		let subscribe = async {
			let Some((_, track)) = media.track(&binding).await? else {
				return Ok(None);
			};
			let subscription = moq_net::track::Subscription::default()
				.with_delay(moq_net::track::Delay::default().with_max(FEED_MAX_DELAY));
			Result::Ok(Some(track.subscribe(subscription).await?.ordered()))
		};
		let mut groups = tokio::select! {
			groups = subscribe => match groups? {
				Some(groups) => groups,
				// The bound broadcast never serves the track: nothing to list.
				None => return Ok(()),
			},
			_ = retiring.wait_for(|retiring| *retiring) => return Ok(()),
		};

		let mut grid = grid::Grid::new(segment, segments::MAX_SEGMENTS);
		// The newest group, its first pts, and what its first frame reached, read again for its
		// content's end if the track ends after it, and the gap from the group before it.
		let mut latest: Option<(moq_net::group::Consumer, moq_net::Timestamp, Reach)> = None;
		let mut spacing: Option<std::time::Duration> = None;
		// Nothing is listed before the first segment this watcher saw start.
		let mut whole = false;
		// Retired, where the first group past the last it had read starts: the replacement's media.
		let mut theirs_at: Option<moq_net::Timestamp> = None;
		loop {
			let pending = rendition
				.upgrade()
				.is_some_and(|rendition| rendition.claim.lock().expect("claim lock poisoned").pending);
			let next = tokio::select! {
				next = groups.next_group() => next?,
				// Nothing waits on a later segment, so a retiring rendition has listed all it will.
				_ = retiring.wait_for(|retiring| *retiring), if !pending => return Ok(()),
			};
			let ended = next.is_none();
			// The group read and, unless it is a marker, its first pts.
			let mut read: Option<(u64, Option<moq_net::Timestamp>)> = None;
			let slots = match next {
				Some(mut group) => {
					// A group's place is its first frame's pts: the transport's stamp, or the
					// container's own where the session carries none.
					let first = tokio::select! {
						first = group.read_frame() => first?,
						_ = retiring.wait_for(|retiring| *retiring), if !pending => return Ok(()),
					};
					let Some(first) = first.and_then(|frame| first_frame(&container, frame)) else {
						tracing::warn!(%track, group = group.sequence, "skipping a group without a timed first frame");
						continue;
					};
					match first {
						// A discontinuity marker carries no media: the content before it ends there,
						// and the grid numbers what resumes by its own timestamps, as every edge does.
						First::Marker(at) => {
							read = Some((group.sequence, None));
							latest = None;
							grid.stop(at)
						}
						First::Media(pts, reach) => {
							read = Some((group.sequence, Some(pts)));
							if let Some((_, previous, _)) = &latest {
								spacing = pts.checked_sub(*previous).ok().map(Duration::from).or(spacing);
							}
							latest = Some((group, pts, reach));
							grid.group(read.expect("set above").0, pts)
						}
					}
				}
				None => {
					let (reach, end) = match latest.take() {
						Some((group, _, reach)) => content_end(group, &container, reach, spacing, interval).await?,
						None => (None, None),
					};
					grid.finish(reach, end)
				}
			};
			let Some(rendition) = rendition.upgrade() else {
				return Ok(());
			};
			// Held while listing, so a retire sees exactly what this instance lists.
			let mut claim = rendition.claim.lock().expect("claim lock poisoned");
			// Retired, a group past the last it had read is the replacement's: this instance lists
			// the segments and gaps before the first holding one, and its own part of that one, then
			// stops there.
			let boundary = claim.boundary;
			let theirs = |groups: &std::ops::Range<u64>| boundary.is_none_or(|boundary| groups.end > boundary + 1);
			if claim.retired
				&& let Some((sequence, Some(pts))) = read
				&& theirs(&(sequence..sequence + 1))
			{
				theirs_at = theirs_at.or(Some(pts));
			}
			for slot in slots {
				let Some(segment) = slot.segment() else {
					tracing::warn!(%track, "media timestamps went back; resetting the playlist window");
					rendition.live.clear();
					broken = true;
					whole = false;
					continue;
				};
				if segment < claim.start {
					continue;
				}
				let (slot, last) = match slot {
					grid::Slot::Media {
						segment, groups, pts, ..
					} if claim.retired && theirs(&groups) => match (boundary, theirs_at) {
						// The replacement can't list this segment's number again, so this instance
						// lists the groups it had read, ending where the replacement's media starts.
						(Some(boundary), Some(at)) if groups.start <= boundary => {
							let slot = grid::Slot::Media {
								segment,
								groups: groups.start..boundary + 1,
								pts,
								end: at,
								content: Some(at),
							};
							(slot, true)
						}
						_ => return Ok(()),
					},
					slot => (slot, false),
				};
				if !whole {
					// The first segment listed holds only this instance's groups, and starts whole.
					let own = match &slot {
						grid::Slot::Media { groups, .. } => after.is_none_or(|after| groups.start > after),
						_ => false,
					};
					if !own || !grid.starts(&slot) {
						if last {
							return Ok(());
						}
						continue;
					}
					whole = true;
				}
				claim.next = segment + 1;
				rendition.push(row(&rendition, slot, broken));
				broken = false;
				if last {
					return Ok(());
				}
			}
			if claim.retired
				&& grid
					.open_groups()
					.is_some_and(|groups| theirs(&(groups.start..groups.start + 1)))
			{
				return Ok(());
			}
			if let Some((sequence, _)) = read {
				claim.seen = Some(sequence);
			}
			if !claim.retired
				&& let Some(open) = grid.open()
			{
				claim.next = claim.next.max(open + 1);
			}
			claim.pending = grid.waiting();
			if ended {
				return Ok(());
			}
		}
	}
	.await;
	let Some(rendition) = rendition.upgrade() else {
		return;
	};
	match result {
		Ok(()) => rendition.end(),
		// A bound sibling that ended takes its track down with it: that ends the rendition, whose
		// retained rows drain, rather than failing it.
		Err(_) if !rendition.media.admits(&rendition.live) => rendition.end(),
		// Never retried (the origin already rides out transient source failures), and not
		// papered over with gaps: the rendition's playlist answers the error.
		Err(err) => {
			tracing::error!(%track, %err, "media track failed; failing the rendition's playlist");
			rendition.fail(crate::Error::MediaFailed {
				track,
				source: Box::new(err),
			});
		}
	}
	rendition.close();
}
/// What a group's frames reach: the timestamps of its samples, where one ends explicitly, and a
/// discontinuity marker, read as they are decoded.
#[derive(Default)]
struct Reach {
	/// Each sample's pts, in nanoseconds.
	stamps: Vec<u128>,
	/// The latest end a sample states with its own duration, in nanoseconds.
	ends: Option<u128>,
	/// A frame with no payload: where the publisher says the content stops.
	marker: Option<moq_net::Timestamp>,
}

impl Reach {
	/// Count every sample `frames` decodes to.
	fn add(&mut self, frames: Vec<moq_mux::container::Frame>) {
		for frame in frames {
			if frame.payload.is_empty() {
				self.marker = Some(frame.timestamp);
				continue;
			}
			let pts = frame.timestamp.as_nanos();
			self.stamps.push(pts);
			if let Some(duration) = frame.duration {
				self.ends = self.ends.max(Some(pts + duration.as_nanos()));
			}
		}
	}

	/// Where the last sample starts, which places the group on the grid, and where the content
	/// ends, which times its segment.
	///
	/// A duration marker is both. Otherwise the end is the latest a sample states, else its newest
	/// sample plus the shortest spacing between them, else `spacing`, the gap from the group
	/// before, capped at `interval` so a lone frame after a long group never claims its length.
	fn finish(
		mut self,
		spacing: Option<std::time::Duration>,
		interval: std::time::Duration,
	) -> (Option<moq_net::Timestamp>, Option<moq_net::Timestamp>) {
		if self.marker.is_some() {
			return (self.marker, self.marker);
		}
		self.stamps.sort_unstable();
		self.stamps.dedup();
		let timestamp = |nanos: u128| moq_net::Timestamp::from_nanos(u64::try_from(nanos).ok()?).ok();
		let Some(&newest) = self.stamps.last() else {
			return (None, None);
		};
		let step = self
			.stamps
			.windows(2)
			.map(|pair| pair[1] - pair[0])
			.min()
			.or_else(|| spacing.map(|spacing| spacing.min(interval).as_nanos()));
		let end = self
			.ends
			.filter(|end| *end > newest)
			.or_else(|| step.map(|step| newest + step));
		(timestamp(newest), end.and_then(timestamp))
	}
}

/// What a group's first frame holds.
enum First {
	/// Media: its pts, the transport's stamp or else its first sample's, with what its samples reach.
	Media(moq_net::Timestamp, Reach),
	/// A discontinuity marker, a frame with no payload: the content stops at its timestamp.
	Marker(moq_net::Timestamp),
}

/// The first frame of a group, or `None` when it can't be read.
fn first_frame(container: &moq_mux::catalog::hang::Container, frame: moq_net::frame::Frame) -> Option<First> {
	use moq_mux::container::Container as _;
	let stamp = frame.timestamp;
	let decoded = container.decode(frame).ok()?;
	let first = decoded.first()?;
	let pts = stamp.unwrap_or(first.timestamp);
	if first.payload.is_empty() {
		return Some(First::Marker(pts));
	}
	let mut reach = Reach::default();
	reach.add(decoded);
	Some(First::Media(pts, reach))
}

/// What a group's content reaches, read once the track has ended so every frame is in, starting
/// from what its first frame reached (see [`Reach::finish`]).
///
/// Fails when the group does: its last segment would otherwise be listed complete but truncated.
async fn content_end(
	mut group: moq_net::group::Consumer,
	container: &moq_mux::catalog::hang::Container,
	mut reach: Reach,
	spacing: Option<std::time::Duration>,
	interval: std::time::Duration,
) -> Result<(Option<moq_net::Timestamp>, Option<moq_net::Timestamp>)> {
	use moq_mux::container::Container as _;
	while let Some(frame) = group.read_frame().await? {
		// The container says what a frame holds: a marker's payload is empty only once decoded.
		if let Ok(decoded) = container.decode(frame) {
			reach.add(decoded);
		}
	}
	Ok(reach.finish(spacing, interval))
}

/// The row listing `slot`, starting a new discontinuity when `broken`.
fn row(rendition: &Rendition, slot: grid::Slot, broken: bool) -> segments::Row {
	let (segment, groups, pts, end, content, stop) = match slot {
		// Listed as a gap rather than as a segment every request for would refuse.
		grid::Slot::Media {
			segment,
			groups,
			pts,
			end,
			..
		} if groups.end.saturating_sub(groups.start) > rendition.max_groups() => {
			tracing::warn!(
				rendition = rendition.name,
				segment,
				?groups,
				"a segment holds more groups than one request fetches; listing it as a gap"
			);
			(segment, 0..0, pts, end, None, true)
		}
		grid::Slot::Media {
			segment,
			groups,
			pts,
			end,
			content,
		} => (segment, groups, pts, end, content, false),
		grid::Slot::Gap { segment, pts, end } => (segment, 0..0, pts, end, None, true),
		grid::Slot::Reset => unreachable!("a reset lists no row"),
	};
	let start = Duration::from(pts);
	let end = Duration::from(end).max(start);
	let duration = end - start;
	// RFC 8216 4.3.3.1 requires every EXTINF, rounded to the nearest second, to fit the target
	// duration. A group overrunning the interval is listed anyway: refusing it loses content, and
	// splitting or skipping it renumbers the segments after it (RFC 8216 6.2.2). It is listed at its
	// first line, so its EXTINF understates its media and the gaps after it overstate theirs.
	if !stop && duration.as_secs_f64().round() as u64 > rendition.target_duration {
		rendition.overrun(segment, duration);
	}
	segments::Row {
		index: segment,
		segment,
		tag: rendition.tag.clone(),
		groups,
		duration,
		pts,
		end,
		discontinuity: rendition.fanout.stamp(&rendition.key(), broken, end),
		stop,
		// Listed once a later segment started or the track ended, so its groups are all complete.
		complete: !stop,
		content: content.map(Duration::from),
	}
}

/// An inline-codec init still to be read from media: owned, so a background build waits on it
/// without holding its rendition.
struct InitRead {
	muxer: Muxer,
	/// Groups to try in order (see [`segments::Producer::init_groups`]): a complete group first,
	/// since reading an open one waits for the publisher to finish it.
	groups: Vec<u64>,
}

impl InitRead {
	/// Read the groups until one yields the init.
	async fn read(mut self, media: &Media, binding: &moq_mux::Binding) -> Result<Option<Bytes>> {
		let Some((broadcast, track)) = media.track(binding).await? else {
			return Ok(None);
		};
		for sequence in self.groups {
			// A group gone from the cache, before or mid-read, leaves the next candidate to try.
			let Ok(mut group) = fetch(&track, sequence, &broadcast).await? else {
				continue;
			};
			if read_group(&mut self.muxer, &mut group, &broadcast).await?.is_err() {
				continue;
			}
			if let Some(bytes) = self.muxer.init()? {
				return Ok(Some(bytes));
			}
		}
		Ok(None)
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
			| moq_net::Error::NotFetchable
			| moq_net::Error::Old
			| moq_net::Error::Evicted
			| moq_net::Error::Stream(
				moq_net::StreamError::NotFound
					| moq_net::StreamError::NotFetchable
					| moq_net::StreamError::Old
					| moq_net::StreamError::Evicted
			)
	)
}

/// True if the bound publisher is gone, so a listed segment can never be served.
///
/// An application dropping its producer, or a local cancellation, leaves the track
/// unservable. A session-scoped cancellation alone can leave its publisher live,
/// so it only counts once the bound broadcast closed.
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

	/// A one-second join interval.
	fn interval() -> Duration {
		Duration::from_secs(1)
	}

	// One request fetches a group per millisecond of the segment, within fixed bounds the catalog's
	// interval can't move.
	#[tokio::test]
	async fn a_segment_request_fetches_a_bounded_number_of_groups() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
		let renditions = renditions::Producer::spawned(Duration::from_secs(30));
		let config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		let max_groups = |interval| {
			Rendition::video("video".into(), &config, &source, interval, None, renditions.fanout())
				.unwrap()
				.max_groups()
		};
		assert_eq!(
			max_groups(Duration::from_millis(20)),
			MIN_SEGMENT_GROUPS,
			"a 1 s segment"
		);
		assert_eq!(max_groups(Duration::from_secs(4)), 4_000);
		assert_eq!(
			max_groups(Duration::from_secs(3_600)),
			MAX_SEGMENT_GROUPS,
			"an hour declared"
		);
	}

	// A replacement skips the groups its predecessor read only on the same track: the same name in
	// another broadcast numbers its groups afresh.
	#[tokio::test]
	async fn a_replacement_continues_only_the_same_track() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
		let renditions = renditions::Producer::spawned(Duration::from_secs(30));
		let build = |config: &VideoConfig| {
			Rendition::video("video".into(), config, &source, interval(), None, renditions.fanout()).unwrap()
		};
		let config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		let previous = build(&config);

		let mut reconfigured = config.clone();
		reconfigured.coded_width = Some(1280);
		assert!(build(&reconfigured).continues(&previous));

		let mut renamed = config.clone();
		renamed.track = Some("other".into());
		assert!(!build(&renamed).continues(&previous));

		let mut elsewhere = config.clone();
		elsewhere.broadcast = Some(moq_net::path::Relative::new("./sibling").unwrap());
		assert!(!build(&elsewhere).continues(&previous));
	}

	#[tokio::test]
	async fn pending_reads_preserve_a_later_authorization_refusal() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room/live").unwrap();
		let dynamic = origin.requests("room/secret").unwrap();
		let source = moq_mux::Source::new(origin.consume(), broadcast.consume());
		let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.broadcast = Some(moq_net::path::Relative::new("./secret").unwrap());
		let renditions = renditions::Producer::spawned(Duration::from_secs(30));
		let rendition = Arc::new(
			Rendition::video("video".into(), &config, &source, interval(), None, renditions.fanout()).unwrap(),
		);
		let request = dynamic.requested_broadcast().await.unwrap();
		let row = segments::Row {
			index: 0,
			segment: 0,
			tag: rendition.tag.clone(),
			groups: 0..1,
			duration: Duration::from_secs(1),
			pts: moq_net::Timestamp::ZERO,
			end: Duration::from_secs(1),
			discontinuity: 0,
			stop: false,
			complete: true,
			content: None,
		};
		let segment = rendition.fetch(&row);
		tokio::pin!(segment);
		// The cursor waits on a window with no rows, which the refusal neither fills nor ends.
		let mut cursor = rendition.segments();
		let next = cursor.next();
		tokio::pin!(next);
		tokio::select! {
			biased;
			_ = &mut segment => panic!("the reference request has not been answered"),
			_ = &mut next => panic!("the reference request has not been answered"),
			_ = std::future::ready(()) => {}
		}
		request.reject(moq_net::Error::Unauthorized);
		for read in [segment.await, next.await.map(|_| None)] {
			assert!(matches!(
				read,
				Err(crate::Error::Mux(moq_mux::Error::Moq(moq_net::Error::Unauthorized)))
			));
		}
	}

	// End may land while capture's query installs a dynamically served track. The handle must
	// stay on that instance rather than repeat a lookup End refuses, or its retained media is lost.
	#[test]
	fn capture_keeps_the_queried_track_when_end_lands_during_the_query() {
		struct Serve {
			broadcast: moq_net::broadcast::Producer,
			dynamic: std::sync::Mutex<moq_net::broadcast::Requests>,
			track: std::sync::Mutex<Option<moq_net::track::Producer>>,
			// Kept alive here, since a list only holds it weakly.
			waiter: std::sync::OnceLock<kio::Waiter>,
		}
		impl Serve {
			fn poll(&self) -> Poll<moq_net::Result<moq_net::track::Request>> {
				let waiter = self.waiter.get().expect("waiter set");
				self.dynamic.lock().unwrap().poll_requested_track(waiter)
			}
		}
		impl std::task::Wake for Serve {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				// The lookup before the query wakes us too, so poll until the request lands.
				let Poll::Ready(Ok(request)) = self.poll() else {
					return;
				};
				let mut track = request.accept(moq_net::track::Info::default());
				track.write_frame(None, Bytes::from_static(b"retained")).unwrap();
				self.broadcast.close();
				*self.track.lock().unwrap() = Some(track);
			}
		}

		let broadcast = moq_net::broadcast::Info::new().produce();
		let serve = Arc::new(Serve {
			dynamic: std::sync::Mutex::new(broadcast.requests()),
			broadcast,
			track: Default::default(),
			waiter: Default::default(),
		});
		let waiter = kio::Waiter::new(std::task::Waker::from(serve.clone()));
		assert!(serve.waiter.set(waiter).is_ok());
		assert!(serve.poll().is_pending());

		let consumer = serve.broadcast.consume();
		let mut handle = Handle {
			binding: Arc::new(moq_mux::Binding::new(consumer.clone())),
			track: None,
			ended: false,
		};
		handle.capture(&consumer, "video");
		assert!(serve.track.lock().unwrap().is_some(), "the query was served");
		assert!(consumer.track("video").is_err(), "End refuses new lookups");

		let query = handle.track.as_ref().expect("the queried track is kept");
		let mut fetch = query.track().fetch_group(0, None).into_inner();
		let Poll::Ready(group) = kio::Task::poll(&mut fetch, &kio::Waiter::noop()) else {
			panic!("the retained group is cached");
		};
		group.expect("the retained group drains after End");
	}

	#[test]
	fn a_cache_miss_that_crossed_a_session_is_still_a_cache_miss() {
		for local in [
			moq_net::Error::NotFound,
			moq_net::Error::NotFetchable,
			moq_net::Error::Old,
			moq_net::Error::Evicted,
		] {
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

	// A wait on a rendition's refusal wakes when its media track fails mid-wait, so the manifest's
	// first-video wait drops a video that can no longer be listed.
	#[tokio::test(start_paused = true)]
	async fn a_refusal_wait_wakes_on_a_media_failure() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("live").unwrap();
		let source = moq_mux::Source::new(origin.consume(), broadcast.consume());
		let config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		let renditions = renditions::Producer::spawned(Duration::from_secs(30));
		let rendition = Arc::new(
			Rendition::video("video".into(), &config, &source, interval(), None, renditions.fanout()).unwrap(),
		);
		// On its own task, so only a registered wake polls it again.
		let refused = tokio::spawn({
			let rendition = rendition.clone();
			async move { kio::wait(|waiter| rendition.poll_refused(waiter)).await }
		});
		kio::clock::sleep(Duration::from_millis(10)).await;
		assert!(!refused.is_finished(), "the media has not failed yet");
		rendition.fail(crate::Error::Moq(moq_net::Error::Transport("connection lost".into())));
		kio::clock::timeout(Duration::from_secs(1), refused)
			.await
			.expect("the failure wakes the wait")
			.unwrap();
	}

	fn sample(ms: u64, duration_ms: Option<u64>, payload: &'static [u8]) -> moq_mux::container::Frame {
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_millis(ms).unwrap(),
			payload: Bytes::from_static(payload),
			keyframe: ms == 0,
			duration: duration_ms.map(|ms| moq_net::Timestamp::from_millis(ms).unwrap()),
		}
	}

	fn millis(reach: (Option<moq_net::Timestamp>, Option<moq_net::Timestamp>)) -> (Option<u128>, Option<u128>) {
		(reach.0.map(|t| t.as_millis()), reach.1.map(|t| t.as_millis()))
	}

	// A fragment carrying all its samples in one frame reaches as far as its samples say, however
	// far apart the groups before it were.
	#[test]
	fn a_whole_fragment_in_one_frame_reaches_its_last_sample() {
		let mut reach = Reach::default();
		reach.add([0, 40, 80].map(|ms| sample(ms, Some(40), b"x")).to_vec());
		assert_eq!(
			millis(reach.finish(Some(Duration::from_secs(2)), Duration::from_secs(2))),
			(Some(80), Some(120))
		);

		// Without durations, the spacing between its samples times the last.
		let mut reach = Reach::default();
		reach.add([0, 40, 80].map(|ms| sample(ms, None, b"x")).to_vec());
		assert_eq!(
			millis(reach.finish(Some(Duration::from_secs(2)), Duration::from_secs(2))),
			(Some(80), Some(120))
		);

		// A lone frame borrows the group spacing, capped at the interval; a marker says where it ends.
		let mut reach = Reach::default();
		reach.add(vec![sample(0, None, b"x")]);
		assert_eq!(
			millis(reach.finish(Some(Duration::from_secs(7)), Duration::from_secs(2))),
			(Some(0), Some(2000))
		);
		let mut reach = Reach::default();
		reach.add(vec![sample(0, None, b"x"), sample(500, None, b"")]);
		assert_eq!(
			millis(reach.finish(None, Duration::from_secs(2))),
			(Some(500), Some(500))
		);
	}

	// A jump in the numbering, from segments skipped across a hand-over, drops the inherited rows
	// before it, but never the rendition's own.
	#[test]
	fn a_numbering_jump_cuts_the_inherited_rows() {
		assert_eq!(super::broken(&[0, 1, 2, 3], 2), 0);
		assert_eq!(super::broken(&[0, 1, 5, 6], 2), 2, "between inherited and own");
		assert_eq!(super::broken(&[0, 3, 4, 5], 2), 1, "between two predecessors");
		assert_eq!(super::broken(&[0, 3, 7], 2), 2, "the last jump counts");
		assert_eq!(super::broken(&[0, 1, 2, 9], 2), 0, "own rows are never cut");
		assert_eq!(super::broken(&[0, 1], 2), 0);
		assert_eq!(super::broken(&[], 0), 0);
	}
}
