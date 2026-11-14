//! One rendition: playlists from its media groups on the broadcast's grid, and segments fetched on
//! demand.

use kio::clock::Wall;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::task::{Poll, ready};
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
	lazy: bool,
	handle: Mutex<Handle>,
	track: String,
	/// The rendition reads another broadcast than the catalog's.
	sibling: bool,
}

struct Handle {
	binding: Arc<moq_mux::Binding>,
	/// The query pinning the track instance. Even an info query creates publisher demand,
	/// so a lazy rendition holds it only while serving media.
	track: Option<moq_net::track::Resolving>,
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
	fn bind(
		source: &moq_mux::Source,
		reference: &impl hang::catalog::Reference,
		track: &str,
		lazy: bool,
	) -> moq_mux::Result<Self> {
		let rel = reference.broadcast();
		let mut handle = Handle {
			binding: Arc::new(source.bind(rel, reference.instance())?),
			track: None,
		};
		if !lazy && let Poll::Ready(Ok(broadcast)) = handle.binding.poll_broadcast(&kio::Waiter::noop()) {
			handle.capture(&broadcast, track);
		}
		Ok(Self {
			lazy,
			handle: Mutex::new(handle),
			track: track.to_string(),
			// `bind` already rejected an escaping reference, so both sides name a path.
			sibling: source.resolve_reference(rel) != source.resolve_reference(None),
		})
	}

	/// Whether incoming rows should be listed: not once the window stopped listing, as it does
	/// when the bound sibling ends.
	fn admits(&self, window: &segments::Producer) -> bool {
		self.sync(window);
		window.lists()
	}

	/// End the window when a bound sibling has ended; retain its rows and media for draining.
	fn sync(&self, window: &segments::Producer) -> Arc<moq_mux::Binding> {
		self.poll_sync(&kio::Waiter::noop(), window)
	}

	/// Register the bound broadcast's close and retain its track before new lookups are refused.
	fn poll_sync(&self, waiter: &kio::Waiter, window: &segments::Producer) -> Arc<moq_mux::Binding> {
		let mut handle = self.handle.lock().expect("media lock poisoned");
		if let Poll::Ready(Ok(broadcast)) = handle.binding.poll_broadcast(waiter) {
			if !self.lazy {
				handle.capture(&broadcast, &self.track);
			}
			if self.sibling && broadcast.poll_closed(waiter).is_ready() {
				// End and Restart both drain only the pinned instance. Looking its path up again
				// could serve a replacement under the old track's rows.
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
	fn poll_track_or_refusal(
		&self,
		binding: &moq_mux::Binding,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<(moq_net::broadcast::Consumer, moq_net::track::Consumer)>>> {
		let track = ready!(self.poll_track(binding, waiter));
		// The bind can be refused while this read waits. Preserve that exact instance's
		// authorization error; other failed or disconnected references stay missing.
		if track.is_none()
			&& let Poll::Ready(Err(err @ moq_mux::Error::Moq(moq_net::Error::Unauthorized))) =
				binding.poll_broadcast(&kio::Waiter::noop())
		{
			return Poll::Ready(Err(err.into()));
		}
		Poll::Ready(Ok(track))
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
	/// Set by [`Rendition::retire`] to [`seen`](Self::seen), or to the group before a
	/// [`handover`](Rendition::handover): the groups this instance still lists are those up to it,
	/// and a replacement on the same track lists only groups past it.
	boundary: Option<u64>,
	/// Retired, where the replacement's media starts: the first group past the boundary.
	theirs_at: Option<moq_net::Timestamp>,
	/// The oldest group no completed segment holds, and the open segment if one is: what a retire
	/// can still hand over.
	unplaced: Option<(u64, Option<u64>)>,
	/// The picture size the newest group declared in band.
	size: Option<(u32, u32)>,
	/// The newest group declaring a picture size other than the one before it: every later group
	/// read shares its size.
	resized: Option<Resize>,
}

/// A group whose keyframe changed the picture size in band.
#[derive(Clone, Copy)]
struct Resize {
	group: u64,
	pts: moq_net::Timestamp,
	size: (u32, u32),
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
	watcher: Mutex<Watcher>,
	/// Which segments this instance lists. A channel so the media watcher wakes once a
	/// [`retire`](Self::retire) leaves no segment pending.
	claim: kio::Producer<Claim>,
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
	/// Serializes init builds, so concurrent first requests fetch the keyframe group once. A
	/// background build (see [`start_init`](Self::start_init)) holds it for its whole run.
	building: Building,
	/// The background init build; ends when the rendition drops.
	build: Mutex<Option<Owner>>,
}

#[derive(Default)]
struct Watcher {
	media: Option<Owner>,
	idle: Option<Owner>,
	#[cfg(feature = "server")]
	requests: usize,
	stopped: bool,
}

/// Admits one init build at a time; the others wait for it.
#[derive(Clone, Default)]
struct Building(kio::Shared<bool>);

/// A held [`Building`], released on drop.
struct Built(kio::Shared<bool>);

impl Building {
	/// Hold it now, unless a build already does.
	fn try_hold(&self) -> Option<Built> {
		let mut held = self.0.lock();
		if *held {
			return None;
		}
		*held = true;
		Some(Built(self.0.clone()))
	}

	/// Hold it once no build does.
	fn poll_hold(&self, waiter: &kio::Waiter) -> Poll<Built> {
		let mut held = ready!(self.0.poll(waiter, |held| match **held {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}));
		*held = true;
		Poll::Ready(Built(self.0.clone()))
	}

	fn is_held(&self) -> bool {
		*self.0.read()
	}
}

impl Drop for Built {
	fn drop(&mut self) {
		*self.0.lock() = false;
	}
}

/// Keeps lazy media demanded for the whole response, including its first encode.
#[cfg(feature = "server")]
pub(crate) struct Demand(Option<Arc<Rendition>>);

#[cfg(feature = "server")]
impl Drop for Demand {
	fn drop(&mut self) {
		let Some(rendition) = self.0.take() else { return };
		let mut watcher = rendition.watcher.lock().expect("watcher lock poisoned");
		watcher.requests -= 1;
		if watcher.requests != 0 || watcher.stopped {
			return;
		}
		// RFC 8216 6.3.4 permits a reload after one target duration (half when unchanged).
		// Three target durations allow that reload and two more intervals of request latency.
		// Release also forgets the listed rows, so never before a live window would have evicted
		// every row the last response listed: the playlist window plus one target duration, since
		// eviction happens only as a later segment is listed. Start at completion, so an encode or
		// slow fetch never expires mid-request.
		let target = Duration::from_secs(rendition.target_duration());
		let duration = target
			.saturating_mul(3)
			.max(rendition.fanout.window().saturating_add(target));
		watcher.idle = Some(rendition.fanout.spawn(Idle {
			rendition: Arc::downgrade(&rendition),
			duration,
			sleep: None,
		}));
	}
}

/// Releases a lazy rendition's media once it went `duration` without a request.
#[cfg(feature = "server")]
struct Idle {
	rendition: Weak<Rendition>,
	duration: Duration,
	/// Started on the first poll.
	sleep: Option<kio::clock::Sleep>,
}

#[cfg(feature = "server")]
impl kio::Task for Idle {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let duration = self.duration;
		let sleep = self.sleep.get_or_insert_with(|| kio::clock::sleep(duration));
		std::task::ready!(waiter.poll_future(std::pin::Pin::new(sleep)));
		let Some(rendition) = self.rendition.upgrade() else {
			return Poll::Ready(());
		};
		let mut claim = rendition.claim();
		let mut watcher = rendition.watcher.lock().expect("watcher lock poisoned");
		// A finished window keeps listing what it listed, inherited predecessor rows included,
		// and keeps its track pin: reads of its retained groups go through it, since a closed
		// sibling refuses any later lookup. Its finished watcher stays too, so no request follows
		// the ended track again.
		if watcher.requests != 0 || rendition.live.finished() {
			return Poll::Ready(());
		}
		rendition.media.handle.lock().expect("media lock poisoned").track.take();
		watcher.media.take();
		// A resumed watcher never rewrites already-addressed segments or lists a partial segment
		// it had open when the last player left.
		claim.start = claim.next;
		claim.pending = false;
		if claim.retired {
			// A replacement waits for this window to close before reading its own track.
			// Stopping the watcher must finish that handover too.
			rendition.live.cut();
		} else {
			rendition.live.clear();
			rendition.previous.lock().expect("previous lock poisoned").take();
		}
		Poll::Ready(())
	}
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
	pub(super) fn interval(&self) -> Duration {
		self.interval
	}

	/// How long a segment is: the smallest whole multiple of the interval that is at least a
	/// second, the grid every rendition numbers its segments on.
	#[cfg(test)]
	pub(super) fn segment_length(&self) -> Duration {
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
		let media = Media::bind(source, config, track, fanout.lazy)?;
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
			watcher: Mutex::default(),
			claim: kio::Producer::default(),
			previous: Mutex::new(None),
			media: Arc::new(media),
			run: kio::Producer::default(),
			building: Building::default(),
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
		let media = Media::bind(source, config, track, fanout.lazy)?;
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
			watcher: Mutex::default(),
			claim: kio::Producer::default(),
			previous: Mutex::new(None),
			media: Arc::new(media),
			run: kio::Producer::default(),
			building: Building::default(),
			build: Mutex::new(None),
		})
	}

	/// Start following this rendition's media track onto the grid.
	pub(crate) fn watch(self: &Arc<Self>) {
		let watcher = self.fanout.spawn(WatchMedia::new(self));
		self.watcher.lock().expect("watcher lock poisoned").media = Some(watcher);
	}

	#[cfg(feature = "server")]
	pub(crate) fn is_lazy(&self) -> bool {
		self.media.lazy
	}

	#[cfg(feature = "server")]
	pub(crate) fn demand(self: &Arc<Self>) -> Demand {
		let mut watcher = self.watcher.lock().expect("watcher lock poisoned");
		if !self.media.lazy {
			return Demand(None);
		}
		watcher.idle.take();
		watcher.requests += 1;
		if watcher.media.is_none() && !watcher.stopped {
			watcher.media = Some(self.fanout.spawn(WatchMedia::new(self)));
		}
		Demand(Some(self.clone()))
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
		self.claim = kio::Producer::new(Claim {
			start,
			next: start,
			..Claim::default()
		});
		self.live = Arc::new(segments::Producer::new(start));
		self.previous = Mutex::new(handoff.previous.clone());
		self
	}

	/// End the window once its open segment is listed, because the catalog dropped or replaced the
	/// rendition with `successor`'s config. Returns where a replacement takes over.
	///
	/// Its track may go on (a replacement may read it too), so its end can't end the window.
	/// Ending it at once would lose the last segment of every recording, since it is listed only
	/// once a later one starts or the track ends.
	///
	/// It keeps the groups it read before the catalog update, except a [`handover`](Self::handover):
	/// the media and the catalog are separate tracks, so the successor's first group can arrive first.
	pub(crate) fn retire(self: &Arc<Self>, successor: Option<&VideoConfig>) -> Handoff {
		let (next, pending) = {
			let mut claim = self.claim();
			claim.retired = true;
			claim.boundary = claim.seen;
			if let Some(resized) = self.handover(&claim, successor) {
				claim.boundary = resized.group.checked_sub(1);
				claim.theirs_at = Some(resized.pts);
				// No group of the open segment is this instance's, so it lists nothing more.
				if let Some((first, open)) = claim.unplaced
					&& first == resized.group
				{
					claim.next = open.unwrap_or(claim.next);
					claim.pending = false;
				}
			}
			(claim.next, claim.pending)
		};
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

	/// The group `successor`'s first on this track, when this instance read it but has not listed it:
	/// the newest in-band resize, to the picture size `successor` declares in place of this config's.
	///
	/// A track keeps its decoder identity, so the picture size is the only decoder change it carries,
	/// and only its keyframes say where that change starts (see the hang draft's Track Identity).
	fn handover(&self, claim: &Claim, successor: Option<&VideoConfig>) -> Option<Resize> {
		let (Config::Video(current), Some(successor)) = (&self.config, successor) else {
			return None;
		};
		let size = |config: &VideoConfig| config.coded_width.zip(config.coded_height);
		let same_track = successor.track(&self.name) == self.track()
			&& successor.broadcast() == current.broadcast()
			&& successor.instance() == current.instance();
		let resized = claim.resized?;
		let (unplaced, _) = claim.unplaced?;
		let resizes = size(successor).is_some_and(|size| size == resized.size) && size(successor) != size(current);
		(same_track && resizes && resized.group >= unplaced).then_some(resized)
	}

	fn claim(&self) -> kio::Mut<'_, Claim> {
		self.claim
			.write()
			.unwrap_or_else(|_| unreachable!("the rendition holds its claim"))
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
				Some((_, second)) => rows
					.back()
					.expect("two rows")
					.1
					.end
					.saturating_sub(super::span(second.pts)),
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
		let mut watcher = self.watcher.lock().expect("watcher lock poisoned");
		watcher.stopped = true;
		watcher.idle.take();
		watcher.media.take();
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

	/// Ready once the playlist is renderable or failed, i.e. once
	/// [`media_playlist`](Self::media_playlist) would return `Some` or an error: it lists a
	/// segment, or its media track ended, its media is denied, or its window failed. Bounding the
	/// wait is the caller's policy (the serve path wraps this in its own timeout).
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_playable(&self, waiter: &kio::Waiter) -> Poll<()> {
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

	/// Ready once the DASH [representation](Self::representation) lists a segment or this
	/// rendition fails, as [`poll_playable`](Self::poll_playable) does for the media playlist:
	/// this instance lists a segment of its own (or its media track ended), its media is denied,
	/// or its window failed. A predecessor's segments don't count: the DASH manifest lists none of
	/// them.
	#[cfg_attr(test, kio::wrapper)]
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
		let mut playlist = Playlist::Start;
		kio::wait(|waiter| playlist.poll(self, waiter, query)).await
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

	/// Whether a track query pins this rendition's media instance.
	#[cfg(all(test, feature = "server"))]
	pub(crate) fn pinned(&self) -> bool {
		self.media.handle.lock().expect("media lock poisoned").track.is_some()
	}

	/// Poll until the broadcast serving this rendition's media settles: `true` once it resolved,
	/// `false` if it failed, however. A listing advertises only a resolved rendition, so never
	/// one whose authorization is pending or whose media can't be read.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) fn poll_resolved(&self, waiter: &kio::Waiter) -> Poll<bool> {
		self.poll_media(waiter)
			.poll_broadcast(waiter)
			.map(|result| result.is_ok())
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
				let t = super::span(row.pts).as_millis() as u64;
				let d = row.end.saturating_sub(super::span(row.pts)).as_millis() as u64;
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
	fn fetch_at(&self, tag: &str, at: segments::At) -> Result<Option<Fetch>> {
		self.authorize()?;
		let sequence = match at {
			segments::At::Sequence(sequence) => sequence,
			segments::At::Pts(pts) => match self.live.segment_number_at(tag, pts) {
				Some(sequence) => sequence,
				None => return Ok(None),
			},
		};
		self.fetch_numbered(tag, sequence)
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
		let mut load = LoadInit::default();
		let init = kio::wait(|waiter| load.poll(self, waiter)).await?;
		Ok(init.filter(|init| init.hash == hash).map(|init| init.bytes.clone()))
	}

	/// The current CMAF init segment, whatever its hash, built on first request and cached.
	/// `load` holds the progress across polls, starting from [`LoadInit::default`].
	pub(crate) fn poll_latest_init(&self, waiter: &kio::Waiter, load: &mut LoadInit) -> Poll<Result<Option<Bytes>>> {
		let init = ready!(load.poll(self, waiter))?;
		Poll::Ready(Ok(init.map(|init| init.bytes.clone())))
	}

	/// `run`'s init if it is already built, or can be without media: an out-of-band codec
	/// builds its init straight from the catalog.
	fn built_init(&self, run: &Run) -> Option<Arc<Init>> {
		if let Some(init) = &run.init {
			return Some(init.clone());
		}
		// A muxer error surfaces from a `LoadInit`, which builds from media.
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
		let Some(building) = self.building.try_hold() else {
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
		let build = self.fanout.spawn(InitBuild {
			_building: building,
			read,
			media: self.media.clone(),
			binding,
			rendition: Arc::downgrade(self),
			name: self.name.clone(),
			epoch: run.epoch,
		});
		*self.build.lock().expect("build lock poisoned") = Some(build);
	}

	/// Whether the init is missing while a build of it is running, so a later render may list
	/// this rendition.
	pub(crate) fn init_pending(&self) -> bool {
		self.run().init.is_none() && self.building.is_held()
	}

	/// The listed groups an inline-codec init is read from, or `None` while none is listed.
	fn init_read(&self, muxer: Muxer) -> Option<InitRead> {
		let groups = self.live.init_groups();
		(!groups.is_empty()).then(|| InitRead::new(muxer, groups))
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
		let owner = self
			.lineage()
			.into_iter()
			.find(|rendition| *rendition.tag == *id.tag && rendition.live.holds(&id.tag, id.at));
		let owner = owner.as_deref().unwrap_or(self);
		let Some(mut fetch) = owner.fetch_own(id)? else {
			return Ok(None);
		};
		let segment = kio::wait(|waiter| fetch.poll(owner, waiter)).await?;
		// A generation change while fetching may have swapped the rows under the lookup.
		Ok(segment.filter(|_| owner.generation().as_deref() == id.generation.as_deref()))
	}

	/// The fetch of the segment `id` names among this instance's own rows; see
	/// [`segment`](Self::segment).
	fn fetch_own(&self, id: &segments::Id) -> Result<Option<Fetch>> {
		self.authorize()?;
		if self.generation().as_deref() != id.generation.as_deref() {
			return Ok(None);
		}
		self.fetch_at(&id.tag, id.at)
	}

	/// The fetch of the segment numbered `segment` on the track tagged `tag`, whatever the run;
	/// see [`segment`](Self::segment).
	fn fetch_numbered(&self, tag: &str, segment: u64) -> Result<Option<Fetch>> {
		self.authorize()?;
		Ok(self.live.row(tag, segment).map(Fetch::new))
	}
}

/// Fetches and transmuxes a row's groups on a rendition; see [`Rendition::segment`].
pub(crate) struct Fetch {
	row: segments::Row,
	state: Fetching,
	/// Set when only sizing the row: each frame is counted and dropped as it is read, never
	/// decoded or kept for a fragment.
	sizing: Option<Size>,
}

/// What a sizing [`Fetch`] counted so far, of the frames as its groups carry them.
#[derive(Default)]
struct Size {
	frames: usize,
	bytes: usize,
}

#[allow(
	clippy::large_enum_variant,
	reason = "one per segment request, and the reading variant is the working state"
)]
enum Fetching {
	Start,
	Track {
		binding: Arc<moq_mux::Binding>,
		epoch: u64,
	},
	Groups {
		_binding: Arc<moq_mux::Binding>,
		epoch: u64,
		broadcast: moq_net::broadcast::Consumer,
		track: moq_net::track::Consumer,
		muxer: Muxer,
		/// Every group's frames, accumulated into ONE fragment, so duration inference sees each
		/// frame's true successor. A per-group fragment would mis-time the trailing sample of
		/// every group (its real successor lives in the next group), audible on audio segments
		/// that span many groups.
		frames: Vec<moq_mux::container::Frame>,
		/// The group being read, and the ones after it, by start.
		groups: std::vec::IntoIter<u64>,
		group: Option<GroupRead>,
	},
	Done,
}

impl Fetch {
	pub(crate) fn new(row: segments::Row) -> Self {
		Self {
			row,
			state: Fetching::Start,
			sizing: None,
		}
	}

	/// A fetch that only sizes the row; see [`poll_size`](Self::poll_size).
	pub(crate) fn sizing(row: segments::Row) -> Self {
		Self {
			sizing: Some(Size::default()),
			..Self::new(row)
		}
	}

	/// Whether this fetch only sizes the row.
	pub(crate) fn sizes(&self) -> bool {
		self.sizing.is_some()
	}

	/// The row fetched.
	pub(crate) fn row(&self) -> &segments::Row {
		&self.row
	}

	/// The row's CMAF fragment, or `None` when it is a stop or its media is gone.
	pub(crate) fn poll(&mut self, rendition: &Rendition, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		let result = ready!(self.poll_fragment(rendition, waiter));
		self.state = Fetching::Done;
		Poll::Ready(result)
	}

	/// The bytes a [`sizing`](Self::sizing) fetch's groups carry, without decoding them, or
	/// `None` when it is a stop or its media is gone. Keeps no segment-wide accumulation.
	pub(crate) fn poll_size(&mut self, rendition: &Rendition, waiter: &kio::Waiter) -> Poll<Result<Option<usize>>> {
		let result = ready!(self.poll_groups(rendition, waiter)).map(|read| {
			let size = self.sizing.as_ref().expect("a sizing fetch");
			(read && size.frames > 0).then_some(size.bytes)
		});
		self.state = Fetching::Done;
		Poll::Ready(result)
	}

	fn poll_fragment(&mut self, rendition: &Rendition, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		debug_assert!(!self.sizes(), "a sizing fetch keeps no frames");
		if !ready!(self.poll_groups(rendition, waiter))? {
			return Poll::Ready(Ok(None));
		}
		let row = &self.row;
		let Fetching::Groups {
			epoch, muxer, frames, ..
		} = &mut self.state
		else {
			unreachable!("read every group above");
		};
		if frames.is_empty() {
			return Poll::Ready(Ok(None));
		}
		// The last presented sample has no successor to time it, so the fragment ends where the
		// next group starts, unless its codec or a duration marker times it.
		let end = row
			.content
			.and_then(|content| moq_net::Timestamp::from_micros(content.as_micros() as u64).ok());
		let fragment = muxer.fragment(row.segment as u32, frames, end)?;
		// Inline codec metadata was already decoded from these completed groups.
		// Reuse it instead of fetching a newer, possibly unfinished group.
		if let Some(bytes) = muxer.init()? {
			rendition.cache_init(*epoch, bytes);
		}
		Poll::Ready(Ok(Some(fragment)))
	}

	/// Read every group of the row, keeping or counting its frames; `false` when it is a stop or
	/// its media is gone.
	fn poll_groups(&mut self, rendition: &Rendition, waiter: &kio::Waiter) -> Poll<Result<bool>> {
		let row = &self.row;
		loop {
			match &mut self.state {
				Fetching::Start => {
					let binding = rendition.authorize()?;
					// Read the run before resolving the row (see `restart`), so an init decoded from
					// its groups is never cached for a run that started while they were being fetched.
					let epoch = rendition.run().epoch;
					if row.stop {
						return Poll::Ready(Ok(false));
					}
					// A segment must be served whole. If its group span exceeds the safety cap (a
					// publisher grouping far finer than its interval), 404 rather than silently
					// truncating it to a segment shorter than its advertised duration. The live window
					// lists such a segment as a gap instead, so only a row from elsewhere reaches this.
					if row.groups.len() as u64 > rendition.max_groups() {
						tracing::warn!(
							groups = ?row.groups,
							"segment spans more groups than one request fetches; refusing to truncate"
						);
						return Poll::Ready(Ok(false));
					}
					self.state = Fetching::Track { binding, epoch };
				}
				Fetching::Track { binding, epoch } => {
					let Some((broadcast, track)) = ready!(rendition.media.poll_track_or_refusal(binding, waiter))?
					else {
						return Poll::Ready(Ok(false));
					};
					self.state = Fetching::Groups {
						_binding: binding.clone(),
						epoch: *epoch,
						broadcast,
						track,
						muxer: rendition.muxer()?,
						frames: Vec::new(),
						groups: row.groups.starts(),
						group: None,
					};
				}
				Fetching::Groups {
					broadcast,
					track,
					muxer,
					frames,
					groups,
					group,
					..
				} => {
					let read = match group {
						Some(read) => read,
						None => match groups.next() {
							Some(sequence) => group.insert(GroupRead::new(track, sequence)),
							None => break,
						},
					};
					let sequence = read.sequence;
					let read = match &mut self.sizing {
						Some(size) => ready!(read.poll_size(waiter, broadcast, size))?,
						None => ready!(read.poll(waiter, muxer, broadcast))?.map(|mut read| frames.append(&mut read)),
					};
					let miss = match read {
						Ok(()) => {
							*group = None;
							continue;
						}
						Err(miss) => miss,
					};
					tracing::warn!(
						segment = row.segment,
						group = sequence,
						err = %miss,
						"abandoning an advertised segment: a group is unavailable"
					);
					return Poll::Ready(Ok(false));
				}
				Fetching::Done => panic!("polled a finished segment fetch"),
			}
		}
		Poll::Ready(Ok(true))
	}
}

/// Loads the current run's init, building and caching it from media if needed.
#[derive(Default)]
pub(crate) struct LoadInit(Loading);

#[derive(Default)]
#[allow(
	clippy::large_enum_variant,
	reason = "one per init load, and the reading variant is the working state"
)]
enum Loading {
	#[default]
	Start,
	/// Waiting for any other build to finish.
	Holding(Arc<moq_mux::Binding>),
	/// Reading media for the init, holding the build.
	Reading {
		binding: Arc<moq_mux::Binding>,
		_building: Built,
		epoch: u64,
		read: InitRead,
	},
	Done,
}

impl LoadInit {
	fn poll(&mut self, rendition: &Rendition, waiter: &kio::Waiter) -> Poll<Result<Option<Arc<Init>>>> {
		let result = ready!(self.0.poll(rendition, waiter));
		// Releases the build as the load returns.
		self.0 = Loading::Done;
		Poll::Ready(result)
	}
}

impl Loading {
	fn poll(&mut self, rendition: &Rendition, waiter: &kio::Waiter) -> Poll<Result<Option<Arc<Init>>>> {
		loop {
			match self {
				Self::Start => {
					let binding = rendition.authorize()?;
					// A transmuxed segment can cache the init while another bootstrap holds
					// `building`, blocked on an unfinished group.
					if let Some(init) = rendition.run().init {
						return Poll::Ready(Ok(Some(init)));
					}
					*self = Self::Holding(binding);
				}
				Self::Holding(binding) => {
					let building = ready!(rendition.building.poll_hold(waiter));
					let run = rendition.run();
					if let Some(init) = run.init {
						return Poll::Ready(Ok(Some(init)));
					}
					// An out-of-band codec builds its init straight from the catalog; an inline one
					// returns `None` until a keyframe group is read.
					let muxer = rendition.muxer()?;
					if let Some(bytes) = muxer.init()? {
						return Poll::Ready(Ok(Some(rendition.cache_init(run.epoch, bytes))));
					}
					let Some(read) = rendition.init_read(muxer) else {
						return Poll::Ready(Ok(None));
					};
					*self = Self::Reading {
						binding: binding.clone(),
						_building: building,
						epoch: run.epoch,
						read,
					};
				}
				Self::Reading {
					binding, epoch, read, ..
				} => {
					let Some(bytes) = ready!(read.poll(waiter, &rendition.media, binding))? else {
						return Poll::Ready(Ok(None));
					};
					return Poll::Ready(Ok(Some(rendition.cache_init(*epoch, bytes))));
				}
				Self::Done => panic!("polled a finished init load"),
			}
		}
	}
}

/// Renders a media playlist once it is servable; see [`Rendition::playlist`].
enum Playlist {
	Start,
	Playable,
	/// Building the init of each inherited instance in turn, then this instance's own.
	Inits {
		inherited: Vec<Arc<Rendition>>,
		/// This instance's own init is mapped only once it lists a segment (see `media_playlist`).
		mapped: bool,
		load: LoadInit,
	},
	/// Building this instance's own init, which it mapped since the render was checked.
	Again(LoadInit),
	Done,
}

impl Playlist {
	fn poll(
		&mut self,
		rendition: &Rendition,
		waiter: &kio::Waiter,
		query: Option<&str>,
	) -> Poll<Result<Option<String>>> {
		let result = ready!(self.poll_state(rendition, waiter, query));
		*self = Self::Done;
		Poll::Ready(result)
	}

	fn poll_state(
		&mut self,
		rendition: &Rendition,
		waiter: &kio::Waiter,
		query: Option<&str>,
	) -> Poll<Result<Option<String>>> {
		loop {
			match self {
				Self::Start => {
					// A denied reference is refused before anything else, so no other error masks it.
					rendition.authorize()?;
					*self = Self::Playable;
				}
				Self::Playable => {
					ready!(rendition.poll_playable(waiter));
					// A failed media track may never supply the keyframe an inline init waits for.
					rendition.check()?;
					// Build the inits the listed segments map. An inline-codec init needs a keyframe
					// group fetched first. Building it caches, so the player's follow-up GET of the
					// init is free.
					let own = rendition.live.window().segments;
					let mut inherited = rendition.inherited(&own);
					inherited.dedup_by(|(a, _), (b, _)| Arc::ptr_eq(a, b));
					let mapped = !own.is_empty() || inherited.is_empty();
					// Loaded in reverse, popping the oldest first.
					let inherited = inherited.into_iter().rev().map(|(rendition, _)| rendition).collect();
					*self = Self::Inits {
						inherited,
						mapped,
						load: LoadInit::default(),
					};
				}
				Self::Inits {
					inherited,
					mapped,
					load,
				} => {
					let loading = match inherited.last() {
						Some(previous) => &**previous,
						None if *mapped => rendition,
						None => {
							let rendered = rendition.media_playlist(query)?;
							if rendered.is_some() {
								return Poll::Ready(Ok(rendered));
							}
							// It listed its first segment meanwhile.
							*self = Self::Again(LoadInit::default());
							continue;
						}
					};
					if ready!(load.poll(loading, waiter))?.is_none() {
						return Poll::Ready(Ok(None));
					}
					*load = LoadInit::default();
					if inherited.pop().is_none() {
						// This instance's own init, which it maps.
						return Poll::Ready(rendition.media_playlist(query));
					}
				}
				Self::Again(load) => {
					if ready!(load.poll(rendition, waiter))?.is_none() {
						return Poll::Ready(Ok(None));
					}
					return Poll::Ready(rendition.media_playlist(query));
				}
				Self::Done => panic!("polled a finished playlist render"),
			}
		}
	}
}

/// Builds an init in the background, holding the build until it ends; see
/// [`Rendition::start_init`].
struct InitBuild {
	_building: Built,
	read: InitRead,
	media: Arc<Media>,
	binding: Arc<moq_mux::Binding>,
	rendition: Weak<Rendition>,
	name: String,
	epoch: u64,
}

impl kio::Task for InitBuild {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		match ready!(self.read.poll(waiter, &self.media, &self.binding)) {
			Ok(Some(bytes)) => {
				if let Some(rendition) = self.rendition.upgrade() {
					rendition.cache_init(self.epoch, bytes);
				}
			}
			Ok(None) => {}
			Err(err) => tracing::warn!(rendition = %self.name, %err, "failed to build init segment"),
		}
		Poll::Ready(())
	}
}

#[cfg(test)]
impl Rendition {
	/// The current CMAF init segment, whatever its hash, built on first request and cached.
	pub(crate) async fn latest_init(&self) -> Result<Option<Bytes>> {
		let mut load = LoadInit::default();
		kio::wait(|waiter| self.poll_latest_init(waiter, &mut load)).await
	}

	/// Fetch and transmux the segment numbered `segment` on the track tagged `tag`, whatever the
	/// run.
	pub(crate) async fn numbered(&self, tag: &str, segment: u64) -> Result<Option<Bytes>> {
		let Some(mut fetch) = self.fetch_numbered(tag, segment)? else {
			return Ok(None);
		};
		kio::wait(|waiter| fetch.poll(self, waiter)).await
	}

	/// Fetch and transmux `row`'s groups.
	pub(crate) async fn fetch(&self, row: &segments::Row) -> Result<Option<Bytes>> {
		let mut fetch = Fetch::new(row.clone());
		kio::wait(|waiter| fetch.poll(self, waiter)).await
	}

	/// Fetch the listed segment numbered `segment`.
	pub(crate) async fn listed_segment(&self, segment: u64) -> Result<Option<Bytes>> {
		self.numbered(&self.tag.clone(), segment).await
	}

	/// Fetch the listed segment starting at `time` milliseconds.
	pub(crate) async fn listed_segment_at(&self, time: u64) -> Result<Option<Bytes>> {
		let Some(mut fetch) = self.fetch_at(&self.tag.clone(), segments::At::Pts(time))? else {
			return Ok(None);
		};
		kio::wait(|waiter| fetch.poll(self, waiter)).await
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

/// Ready once the rendition is retired with no segment left waiting to list, or is gone.
fn poll_retired(claim: &kio::Consumer<Claim>, waiter: &kio::Waiter) -> Poll<()> {
	// A closed channel means the rendition is gone, which ends the watch all the same.
	match claim.poll(waiter, |claim| match claim.retired && !claim.pending {
		true => Poll::Ready(()),
		false => Poll::Pending,
	}) {
		Poll::Ready(_) => Poll::Ready(()),
		Poll::Pending => Poll::Pending,
	}
}

/// Follows a rendition's media track into its window, numbering its groups on the grid, until the
/// track ends or fails.
///
/// A segment is listed once a later one starts, which bounds its groups and its `EXTINF`, or once
/// the track ends. A watcher joining mid-segment can't know the groups before its first, so it
/// lists nothing before the first segment it sees start (see [`grid::Grid::starts`]): another edge
/// lists that segment whole, and the same URL must name the same bytes everywhere.
struct WatchMedia {
	rendition: Weak<Rendition>,
	claim: kio::Consumer<Claim>,
	/// Taken from the rendition on the first poll.
	follow: Option<Follow>,
	/// The rendition's media and the binding it is read through, held while the watch runs.
	held: Option<(Arc<Media>, Arc<moq_mux::Binding>)>,
	groups: Option<moq_net::track::Ordered>,
	step: Step,
}

/// Where a [`WatchMedia`] is.
#[allow(
	clippy::large_enum_variant,
	reason = "one per watched rendition, and the subscribing variant is short-lived"
)]
enum Step {
	Start,
	/// Waiting for the predecessor to list all it will.
	Previous(Arc<Rendition>),
	Bind,
	/// Waiting for the bound broadcast to serve the track.
	Track,
	Subscribing {
		_track: moq_net::track::Consumer,
		subscribing: moq_net::track::Subscribing,
	},
	/// Waiting for the next group.
	Next,
	/// Reading a group's first frame, which places it.
	First(moq_net::group::Consumer),
	/// Reading the last group to its end once the track ended, for where its content ends.
	End(moq_net::group::Consumer, Option<Reach>),
}

/// A group read, and its first pts unless it is a marker.
type Read = (u64, Option<moq_net::Timestamp>);

/// What a [`WatchMedia`] numbers its groups with, and how far it got.
struct Follow {
	track: String,
	interval: Duration,
	container: Arc<moq_mux::catalog::hang::Container>,
	/// Reads each group's picture size.
	probe: Option<Muxer>,
	/// The next segment listed starts a new discontinuity.
	broken: bool,
	/// The last group a predecessor on the same track read: only later ones are this instance's.
	after: Option<u64>,
	grid: grid::Grid,
	/// The newest group, its first pts, and what its first frame reached, read again for its
	/// content's end if the track ends after it, and the gap from the group before it.
	latest: Option<(moq_net::group::Consumer, moq_net::Timestamp, Reach)>,
	spacing: Option<Duration>,
	/// Nothing is listed before the first segment this watcher saw start.
	whole: bool,
}

impl Follow {
	fn new(rendition: &Rendition) -> Self {
		Self {
			track: rendition.track().to_string(),
			interval: rendition.interval,
			container: rendition.container.clone(),
			probe: rendition.muxer().ok(),
			broken: false,
			after: None,
			grid: grid::Grid::new(rendition.segment, segments::MAX_SEGMENTS),
			latest: None,
			spacing: None,
			whole: false,
		}
	}

	/// Place a group by its first frame and list what that places; `Some` once the watch is done.
	fn first(
		&mut self,
		rendition: &Weak<Rendition>,
		group: moq_net::group::Consumer,
		first: Option<moq_net::frame::Frame>,
	) -> Option<Result<()>> {
		// A group's place is its first frame's pts: the transport's stamp, or the container's own
		// where the session carries none.
		let Some(first) = first.and_then(|frame| first_frame(&self.container, frame, self.probe.as_mut())) else {
			tracing::warn!(track = %self.track, group = group.start, "skipping a group without a timed first frame");
			return None;
		};
		let (slots, read, size) = match first {
			// A discontinuity marker carries no media: the content before it ends there, and the
			// grid numbers what resumes by its own timestamps, as every edge does.
			First::Marker(at) => {
				self.latest = None;
				(self.grid.stop(at), (group.start, None), None)
			}
			First::Media(pts, reach, declared) => {
				let sequence = group.start;
				if let Some((_, previous, _)) = &self.latest {
					self.spacing = pts.checked_sub(*previous).ok().map(super::span).or(self.spacing);
				}
				self.latest = Some((group, pts, reach));
				(self.grid.group(sequence, pts), (sequence, Some(pts)), declared)
			}
		};
		self.list(rendition, slots, Some(read), size, false)
	}

	/// List what `slots` place, after reading the group `read` whose keyframe declares `size`;
	/// `Some` once the watch is done.
	fn list(
		&mut self,
		rendition: &Weak<Rendition>,
		slots: Vec<grid::Slot>,
		read: Option<Read>,
		size: Option<(u32, u32)>,
		ended: bool,
	) -> Option<Result<()>> {
		let Self {
			track,
			grid,
			broken,
			whole,
			after,
			..
		} = self;
		let after = *after;
		let Some(rendition) = rendition.upgrade() else {
			return Some(Ok(()));
		};
		// Held while listing, so a retire sees exactly what this instance lists.
		let mut claim = rendition.claim();
		// Retired, a group past the last it had read is the replacement's: this instance lists
		// the segments and gaps before the first holding one, and its own part of that one, then
		// stops there.
		let boundary = claim.boundary;
		let theirs = |last: Option<u64>| boundary.is_none_or(|boundary| last.is_some_and(|last| last > boundary));
		if claim.retired
			&& let Some((sequence, Some(pts))) = read
			&& theirs(Some(sequence))
		{
			claim.theirs_at = claim.theirs_at.or(Some(pts));
		}
		let theirs_at = claim.theirs_at;
		for slot in slots {
			let Some(segment) = slot.segment() else {
				tracing::warn!(%track, "media timestamps went back; resetting the playlist window");
				rendition.live.clear();
				*broken = true;
				*whole = false;
				continue;
			};
			if segment < claim.start {
				continue;
			}
			let (slot, last) = match slot {
				grid::Slot::Media {
					segment,
					groups,
					pts,
					start,
					..
				} if claim.retired && theirs(groups.last()) => match (boundary, theirs_at) {
					// The replacement can't list this segment's number again, so this instance
					// lists the groups it had read, ending where the replacement's media starts.
					(Some(boundary), Some(at)) if groups.first().is_some_and(|first| first <= boundary) => {
						let slot = grid::Slot::Media {
							segment,
							groups: groups.through(boundary),
							pts,
							start,
							end: at,
							content: Some(at),
						};
						(slot, true)
					}
					_ => return Some(Ok(())),
				},
				slot => (slot, false),
			};
			if !*whole {
				// The first segment listed holds only this instance's groups, and starts whole.
				let own = match &slot {
					grid::Slot::Media { groups, .. } => {
						after.is_none_or(|after| groups.first().is_some_and(|first| first > after))
					}
					_ => false,
				};
				if !own || !grid.starts(&slot) {
					if last {
						return Some(Ok(()));
					}
					continue;
				}
				*whole = true;
			}
			claim.next = segment + 1;
			rendition.push(row(&rendition, slot, *broken));
			*broken = false;
			if last {
				return Some(Ok(()));
			}
		}
		if claim.retired && grid.open_groups().is_some_and(|groups| theirs(groups.first())) {
			return Some(Ok(()));
		}
		if let Some((sequence, _)) = read {
			claim.seen = Some(sequence);
		}
		if let (Some((group, Some(pts))), Some(size)) = (read, size)
			&& claim.size != Some(size)
		{
			claim.size = Some(size);
			claim.resized = Some(Resize { group, pts, size });
		}
		claim.unplaced = grid.unplaced().map(|first| (first, grid.open()));
		if !claim.retired
			&& let Some(open) = grid.open()
		{
			claim.next = claim.next.max(open + 1);
		}
		claim.pending = grid.waiting();
		if ended {
			return Some(Ok(()));
		}
		None
	}
}

impl WatchMedia {
	fn new(rendition: &Arc<Rendition>) -> Self {
		Self {
			rendition: Arc::downgrade(rendition),
			claim: rendition.claim.consume(),
			follow: None,
			held: None,
			groups: None,
			step: Step::Start,
		}
	}

	fn poll_watch(&mut self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		let follow = self.follow.as_mut().expect("taken on the first poll");
		loop {
			let retired = || poll_retired(&self.claim, waiter).is_ready();
			match &mut self.step {
				Step::Start => {
					// A replacement lists nothing until its predecessor has listed all it will, so
					// their segments never interleave and its own start a new discontinuity: they
					// decode under another init. It then starts where the predecessor finally stopped,
					// and on the same track past the last group the predecessor read, which another
					// config may have muxed.
					let previous = self.rendition.upgrade().and_then(|rendition| rendition.previous());
					follow.broken = previous.is_some();
					self.step = match previous {
						Some(previous) => Step::Previous(previous),
						None => Step::Bind,
					};
				}
				Step::Previous(previous) => {
					if retired() {
						return Poll::Ready(Ok(()));
					}
					ready!(previous.live.poll_closed(waiter));
					let (next, boundary) = {
						let claim = previous.claim();
						(claim.next, claim.boundary)
					};
					// Another track, or the same name in another broadcast, numbers its groups afresh.
					let continues = self
						.rendition
						.upgrade()
						.is_some_and(|rendition| rendition.continues(previous));
					follow.after = boundary.filter(|_| continues);
					if let Some(rendition) = self.rendition.upgrade() {
						let mut claim = rendition.claim();
						claim.start = claim.start.max(next);
						claim.next = claim.next.max(next);
					}
					self.step = Step::Bind;
				}
				Step::Bind => {
					let Some(rendition) = self.rendition.upgrade() else {
						return Poll::Ready(Ok(()));
					};
					self.held = Some((rendition.media.clone(), rendition.authorize()?));
					self.step = Step::Track;
				}
				Step::Track => {
					if retired() {
						return Poll::Ready(Ok(()));
					}
					let (media, binding) = self.held.as_ref().expect("bound above");
					// The bound broadcast never serves the track: nothing to list.
					let Some((_, track)) = ready!(media.poll_track_or_refusal(binding, waiter))? else {
						return Poll::Ready(Ok(()));
					};
					let subscription = moq_net::track::Subscription::default()
						.with_delay(moq_net::track::Delay::default().with_max(FEED_MAX_DELAY));
					// The index is the export's own bookkeeping, not delivery to a client, so a
					// metered source counts only the segments and inits clients fetch.
					let track = track.unmetered();
					let subscribing = track.subscribe(subscription).into_inner();
					self.step = Step::Subscribing {
						_track: track,
						subscribing,
					};
				}
				Step::Subscribing { subscribing, .. } => {
					if retired() {
						return Poll::Ready(Ok(()));
					}
					let subscriber = ready!(subscribing.poll_ok(waiter))?;
					self.groups = Some(subscriber.ordered());
					self.step = Step::Next;
				}
				Step::Next => {
					if retired() {
						return Poll::Ready(Ok(()));
					}
					let groups = self.groups.as_mut().expect("subscribed above");
					match ready!(groups.poll_next_group(waiter))? {
						Some(group) => self.step = Step::First(group),
						None => match follow.latest.take() {
							Some((group, _, reach)) => self.step = Step::End(group, Some(reach)),
							None => {
								let slots = follow.grid.finish(None, None);
								let done = follow.list(&self.rendition, slots, None, None, true);
								return Poll::Ready(done.expect("an ended track ends the watch"));
							}
						},
					}
				}
				Step::First(group) => {
					if retired() {
						return Poll::Ready(Ok(()));
					}
					let first = ready!(group.poll_read_frame(waiter))?;
					let Step::First(group) = std::mem::replace(&mut self.step, Step::Next) else {
						unreachable!("reading the first frame");
					};
					if let Some(done) = follow.first(&self.rendition, group, first) {
						return Poll::Ready(done);
					}
				}
				Step::End(group, reach) => {
					// Read once the track has ended so every frame is in. Fails when the group does:
					// its last segment would otherwise be listed complete but truncated.
					let reach_mut = reach.as_mut().expect("read to its end once");
					while let Some(frame) = ready!(group.poll_read_frame(waiter))? {
						// The container says what a frame holds: a marker's payload is empty only once decoded.
						if let Ok(decoded) = moq_mux::container::Container::decode(&*follow.container, frame) {
							reach_mut.add(decoded);
						}
					}
					let (reach, end) = reach
						.take()
						.expect("read to its end once")
						.finish(follow.spacing, follow.interval);
					let slots = follow.grid.finish(reach, end);
					let done = follow.list(&self.rendition, slots, None, None, true);
					return Poll::Ready(done.expect("an ended track ends the watch"));
				}
			}
		}
	}
}

impl kio::Task for WatchMedia {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.follow.is_none() {
			let Some(follow) = self.rendition.upgrade().map(|rendition| Follow::new(&rendition)) else {
				return Poll::Ready(());
			};
			self.follow = Some(follow);
		}
		let result = ready!(self.poll_watch(waiter));
		let Some(rendition) = self.rendition.upgrade() else {
			return Poll::Ready(());
		};
		let track = self.follow.take().expect("taken on the first poll").track;
		match result {
			Ok(()) => rendition.end(),
			// A bound sibling that ended takes its track down with it: that ended the rendition,
			// whose retained rows drain, rather than failing it. A window cut or failed already
			// keeps how it stopped.
			Err(_) if !rendition.media.admits(&rendition.live) => {}
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
		Poll::Ready(())
	}
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
			// A sample with no time can't reach anywhere on the grid.
			let Ok(pts) = frame.timestamp.as_nanos() else {
				continue;
			};
			if frame.payload.is_empty() {
				self.marker = Some(frame.timestamp);
				continue;
			}
			self.stamps.push(pts);
			if let Some(Ok(duration)) = frame.duration.map(moq_net::Timestamp::as_nanos) {
				self.ends = self.ends.max(Some(pts + duration));
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
	/// Media: its pts, the transport's stamp or else its first sample's, with what its samples reach
	/// and the picture size its keyframe declares.
	Media(moq_net::Timestamp, Reach, Option<(u32, u32)>),
	/// A discontinuity marker, a frame with no payload: the content stops at its timestamp.
	Marker(moq_net::Timestamp),
}

/// The first frame of a group, or `None` when it can't be read. `probe` reads its picture size; one
/// it can't parse declares none, leaving the hand-over to arrival order.
fn first_frame(
	container: &moq_mux::catalog::hang::Container,
	frame: moq_net::frame::Frame,
	probe: Option<&mut Muxer>,
) -> Option<First> {
	use moq_mux::container::Container as _;
	let stamp = frame.timestamp;
	let decoded = container.decode(frame).ok()?;
	let first = decoded.first()?;
	// A scale-less session carries the group key, not a time, so the container's own is used.
	let pts = if stamp.scale().is_some() {
		stamp
	} else {
		first.timestamp
	};
	// The grid places only times.
	pts.scale()?;
	if first.payload.is_empty() {
		return Some(First::Marker(pts));
	}
	let size = probe.and_then(|probe| probe.picture_size(first).ok().flatten());
	let mut reach = Reach::default();
	reach.add(decoded);
	Some(First::Media(pts, reach, size))
}

/// The row listing `slot`, starting a new discontinuity when `broken`.
fn row(rendition: &Rendition, slot: grid::Slot, broken: bool) -> segments::Row {
	let (segment, groups, pts, end, content, stop) = match slot {
		// Listed as a gap rather than as a segment every request for would refuse.
		grid::Slot::Media {
			segment,
			groups,
			start,
			end,
			..
		} if groups.len() as u64 > rendition.max_groups() => {
			tracing::warn!(
				rendition = rendition.name,
				segment,
				?groups,
				"a segment holds more groups than one request fetches; listing it as a gap"
			);
			(segment, Default::default(), start, end, None, true)
		}
		// Timed from where its EXTINF starts, which a group resuming past its line puts at the line.
		grid::Slot::Media {
			segment,
			groups,
			start,
			end,
			content,
			..
		} => (segment, groups, start, end, content, false),
		grid::Slot::Gap { segment, pts, end } => (segment, Default::default(), pts, end, None, true),
		grid::Slot::Reset => unreachable!("a reset lists no row"),
	};
	let start = super::span(pts);
	let end = super::span(end).max(start);
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
		content: content.map(super::span),
	}
}

/// An inline-codec init still to be read from media: owned, so a background build waits on it
/// without holding its rendition.
struct InitRead {
	muxer: Muxer,
	/// Groups to try in order (see [`segments::Producer::init_groups`]): a complete group first,
	/// since reading an open one waits for the publisher to finish it.
	groups: std::vec::IntoIter<u64>,
	track: Option<(moq_net::broadcast::Consumer, moq_net::track::Consumer)>,
	/// The group being read.
	group: Option<GroupRead>,
}

impl InitRead {
	fn new(muxer: Muxer, groups: Vec<u64>) -> Self {
		Self {
			muxer,
			groups: groups.into_iter(),
			track: None,
			group: None,
		}
	}

	/// Read the groups until one yields the init.
	fn poll(&mut self, waiter: &kio::Waiter, media: &Media, binding: &moq_mux::Binding) -> Poll<Result<Option<Bytes>>> {
		let (broadcast, track) = match &self.track {
			Some(track) => track,
			None => match ready!(media.poll_track_or_refusal(binding, waiter))? {
				Some(track) => self.track.insert(track),
				None => return Poll::Ready(Ok(None)),
			},
		};
		loop {
			let read = match &mut self.group {
				Some(read) => read,
				None => match self.groups.next() {
					Some(sequence) => self.group.insert(GroupRead::new(track, sequence)),
					None => return Poll::Ready(Ok(None)),
				},
			};
			let read = ready!(read.poll(waiter, &mut self.muxer, broadcast))?;
			self.group = None;
			// A group gone from the cache, before or mid-read, leaves the next candidate to try.
			if read.is_err() {
				continue;
			}
			if let Some(bytes) = self.muxer.init()? {
				return Poll::Ready(Ok(Some(bytes)));
			}
		}
	}
}

/// Fetches one group, then reads its frames through a muxer.
struct GroupRead {
	sequence: u64,
	state: GroupReading,
}

enum GroupReading {
	Fetching(moq_net::track::Fetching),
	Reading(moq_net::group::Consumer, Vec<moq_mux::container::Frame>),
}

impl GroupRead {
	fn new(track: &moq_net::track::Consumer, sequence: u64) -> Self {
		Self {
			sequence,
			state: GroupReading::Fetching(track.fetch_group(sequence, None).into_inner()),
		}
	}

	/// Count the group's frames and their bytes into `size` as they are read, keeping none; an
	/// unservable group is an error as in [`poll`](Self::poll).
	fn poll_size(
		&mut self,
		waiter: &kio::Waiter,
		broadcast: &moq_net::broadcast::Consumer,
		size: &mut Size,
	) -> Poll<Result<std::result::Result<(), moq_net::Error>>> {
		if let GroupReading::Fetching(fetching) = &mut self.state {
			match ready!(kio::Task::poll(fetching, waiter)) {
				Ok(group) => self.state = GroupReading::Reading(group, Vec::new()),
				Err(err) if is_unservable(&err, broadcast) => return Poll::Ready(Ok(Err(err))),
				Err(err) => return Poll::Ready(Err(err.into())),
			}
		}
		let GroupReading::Reading(group, _) = &mut self.state else {
			unreachable!("fetched above");
		};
		loop {
			match ready!(group.poll_read_frame(waiter)) {
				Ok(Some(frame)) => {
					size.frames += 1;
					size.bytes += frame.payload.len();
				}
				Ok(None) => return Poll::Ready(Ok(Ok(()))),
				Err(err) if is_unservable(&err, broadcast) => return Poll::Ready(Ok(Err(err))),
				Err(err) => return Poll::Ready(Err(err.into())),
			}
		}
	}

	/// The group's frames, or the error saying it is no longer (or not yet) servable rather than a
	/// bare `None`: a caller abandoning a segment can only explain itself with it.
	///
	/// A mid-read cache eviction (the group aged out of the relay while we were reading it) is
	/// such a miss too. Without this the eviction would surface as a 500; it's really the same
	/// "group gone" 404 as a fetch-time miss.
	fn poll(
		&mut self,
		waiter: &kio::Waiter,
		muxer: &mut Muxer,
		broadcast: &moq_net::broadcast::Consumer,
	) -> Poll<Result<std::result::Result<Vec<moq_mux::container::Frame>, moq_net::Error>>> {
		if let GroupReading::Fetching(fetching) = &mut self.state {
			match ready!(kio::Task::poll(fetching, waiter)) {
				Ok(group) => self.state = GroupReading::Reading(group, Vec::new()),
				Err(err) if is_unservable(&err, broadcast) => return Poll::Ready(Ok(Err(err))),
				Err(err) => return Poll::Ready(Err(err.into())),
			}
		}
		let GroupReading::Reading(group, frames) = &mut self.state else {
			unreachable!("fetched above");
		};
		Poll::Ready(match ready!(muxer.poll_read(waiter, group, frames)) {
			Ok(frames) => Ok(Ok(frames)),
			Err(err) => {
				let miss = net_errors(&err).find(|err| is_unservable(err, broadcast)).cloned();
				match miss {
					Some(miss) => Ok(Err(miss)),
					None => Err(err.into()),
				}
			}
		})
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
		let broadcast = origin
			.route("room/live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let dynamic = origin
			.claim("room/secret", moq_net::origin::RouteOptions::default())
			.unwrap();
		let source = moq_mux::Source::new(origin.consume(), broadcast.broadcast().consume());
		let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.broadcast = Some(moq_net::path::Relative::new("./secret").unwrap());
		let renditions = renditions::Producer::spawned(Duration::from_secs(30));
		let rendition = Arc::new(
			Rendition::video("video".into(), &config, &source, interval(), None, renditions.fanout()).unwrap(),
		);
		let request = dynamic.requested().await.unwrap();
		let row = segments::Row {
			index: 0,
			segment: 0,
			tag: rendition.tag.clone(),
			groups: grid::Groups::one(0),
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
				let track = request.accept(moq_net::track::Info::default());
				let mut group = track.append_group(0).unwrap();
				group
					.write_frame(moq_net::Timestamp::unscaled(0), Bytes::from_static(b"retained"))
					.unwrap();
				group.close().unwrap();
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
		let producer = origin
			.route("live", moq_net::origin::RouteOptions::hidden())
			.expect("publish allowed");
		let live = producer.broadcast().consume();
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
		let broadcast = origin.route("live", moq_net::origin::RouteOptions::hidden()).unwrap();
		let source = moq_mux::Source::new(origin.consume(), broadcast.broadcast().consume());
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
		(
			reach.0.map(|t| t.as_millis().unwrap()),
			reach.1.map(|t| t.as_millis().unwrap()),
		)
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
