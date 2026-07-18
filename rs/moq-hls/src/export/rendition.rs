//! One rendition: playlists from its timeline track, segments fetched on demand.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use hang::catalog::{AudioConfig, MOQ_EPOCH_UNIX_MILLIS, Timeline, VideoConfig};
use moq_mux::container::fmp4::Muxer;

use super::playlist::{Segment, Snapshot};
use super::segments;
use crate::Result;

/// Fallback advertised bitrates when the catalog doesn't carry one.
const DEFAULT_VIDEO_BITRATE: u64 = 2_000_000;
const DEFAULT_AUDIO_BITRATE: u64 = 128_000;

/// Upper bound on the groups fetched for one segment, so a corrupt timeline (or an unbounded
/// final segment) can't turn one HTTP request into an endless fetch loop.
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

/// A single HLS rendition: master-playlist metadata, the live timeline window that renders
/// its media playlist, and on-demand segment fetching.
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
	/// The catalog timeline section: the timeline track's name, timescale, and wall anchor.
	section: Timeline,
	/// The timeline window shared with the watcher task.
	live: Arc<segments::Producer>,
	/// The largest `EXT-X-TARGETDURATION` advertised so far, in seconds. Latched monotonically:
	/// HLS forbids a live playlist's target duration from changing, so it never shrinks even
	/// after a long segment evicts from the window. Read only by the serve path's `playlist`.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	target_duration: std::sync::atomic::AtomicU64,
	/// The init segment, built on first request.
	init: tokio::sync::Mutex<Option<Bytes>>,
	/// Aborts the timeline watcher when the rendition is dropped.
	watcher: tokio::task::JoinHandle<()>,
}

impl Rendition {
	pub(crate) fn matches_video(&self, config: &VideoConfig) -> bool {
		self.config == Config::Video(config.clone())
	}

	pub(crate) fn matches_audio(&self, config: &AudioConfig) -> bool {
		self.config == Config::Audio(config.clone())
	}

	/// Build a video rendition and spawn its timeline watcher. `None` if the catalog doesn't
	/// advertise a timeline (fetch-on-demand needs one).
	pub(crate) fn video(
		name: String,
		config: &VideoConfig,
		source: &moq_mux::Source,
		window: Duration,
	) -> Option<Self> {
		let section = config.timeline.clone()?;
		let live = Arc::new(segments::Producer::new());
		let watcher = spawn_watcher(
			source.clone(),
			config.broadcast.clone(),
			section.clone(),
			live.clone(),
			window,
		);
		Some(Self {
			name,
			kind: Kind::Video,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_VIDEO_BITRATE),
			width: config.coded_width,
			height: config.coded_height,
			codec: config.codec.to_string(),
			config: Config::Video(config.clone()),
			section,
			live,
			target_duration: std::sync::atomic::AtomicU64::new(0),
			init: tokio::sync::Mutex::new(None),
			watcher,
		})
	}

	/// Build an audio rendition and spawn its timeline watcher. `None` if the catalog doesn't
	/// advertise a timeline.
	pub(crate) fn audio(
		name: String,
		config: &AudioConfig,
		source: &moq_mux::Source,
		window: Duration,
	) -> Option<Self> {
		let section = config.timeline.clone()?;
		let live = Arc::new(segments::Producer::new());
		let watcher = spawn_watcher(
			source.clone(),
			config.broadcast.clone(),
			section.clone(),
			live.clone(),
			window,
		);
		Some(Self {
			name,
			kind: Kind::Audio,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_AUDIO_BITRATE),
			width: None,
			height: None,
			codec: config.codec.to_string(),
			config: Config::Audio(config.clone()),
			section,
			live,
			target_duration: std::sync::atomic::AtomicU64::new(0),
			init: tokio::sync::Mutex::new(None),
			watcher,
		})
	}

	/// A cursor over this rendition's finalized segments, with their media, in timeline order.
	/// For a recorder mirroring the broadcast; see [`segments::Consumer`].
	pub fn segments(self: &Arc<Self>) -> segments::Consumer {
		self.live.subscribe(self.clone())
	}

	/// Retire this rendition: close its segment timeline so every [`segments::Consumer`] over it
	/// drains what's left and then ends, and stop the timeline watcher (releasing its standing
	/// source subscription).
	///
	/// Called when the catalog drops or replaces the rendition. The media track may continue
	/// through a replacement, so the old watcher has no clean end to await; a cursor's
	/// `Arc<Self>` would otherwise keep it subscribed and parked forever.
	pub(crate) fn close(&self) {
		self.live.close();
		self.watcher.abort();
	}

	/// Resolve once the playlist is renderable ([`is_playable`](Self::is_playable)). Bounding
	/// the wait is the caller's policy (the serve path wraps this in its own timeout).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub(crate) async fn playable(&self) {
		kio::wait(|waiter| self.live.poll_playable(waiter)).await;
	}

	/// Render the media playlist from the current timeline window.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub(crate) fn playlist(&self) -> Snapshot {
		let window = self.live.window();

		// EXT-X-TARGETDURATION must be >= every segment's duration and, per the HLS spec, must not
		// change over a playlist's lifetime. The timeline is our only cadence signal, so take the
		// longest gap seen so far and latch it monotonically (fetch_max) so a long segment evicting
		// from the window can never shrink the advertised value.
		let longest = window.segments.iter().map(|s| s.duration).fold(0.0f64, f64::max);
		let want = (longest.ceil() as u64).max(1);
		let target_duration = self
			.target_duration
			.fetch_max(want, std::sync::atomic::Ordering::Relaxed)
			.max(want);

		let program_date_time = window.segments.first().and_then(|first| self.wall_clock(first.pts));

		Snapshot {
			target_duration,
			media_sequence: window.sequence,
			segments: window
				.segments
				.into_iter()
				.map(|s| Segment {
					group: s.group,
					duration: s.duration,
				})
				.collect(),
			finished: window.ended,
			program_date_time,
		}
	}

	/// Whether the playlist has anything to serve yet (at least one complete segment, or the
	/// broadcast already ended).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
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

	/// The media track handle on its (possibly sibling) broadcast; `None` until the watcher
	/// has resolved the broadcast.
	fn track(&self) -> Option<moq_net::track::Consumer> {
		let broadcast = self.live.broadcast.get()?;
		broadcast.track(&self.name).ok()
	}

	/// The rendition's CMAF init segment, built on first request and cached.
	///
	/// For inline-parameter-set codecs (no catalog `description`), the parameter sets are
	/// resolved by fetching the newest complete segment's group first.
	pub(crate) async fn init(&self) -> Result<Option<Bytes>> {
		let mut cache = self.init.lock().await;
		if let Some(bytes) = cache.as_ref() {
			return Ok(Some(bytes.clone()));
		}

		let mut muxer = self.muxer()?;

		// An out-of-band codec builds its init straight from the catalog; an inline one returns
		// `None` until a keyframe group is read, so bootstrap it from the newest complete segment.
		let bytes = match muxer.init()? {
			Some(bytes) => bytes,
			None => {
				let Some(sequence) = self.live.latest_group() else {
					return Ok(None);
				};
				let Some(track) = self.track() else {
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

	/// Fetch and transmux the segment starting at group `sequence`.
	///
	/// Fetches every group the segment covers (audio timelines skip groups, so a segment may span
	/// several) and encodes them as a single CMAF fragment. `None` when the segment isn't in the
	/// playlist window or its groups already left the relay cache.
	pub(crate) async fn segment(&self, sequence: u64) -> Result<Option<Bytes>> {
		let Some((start, end)) = self.live.segment_groups(sequence) else {
			return Ok(None);
		};
		let Some(track) = self.track() else {
			return Ok(None);
		};

		let bounded = end.is_some();
		// A bounded segment must be served whole. If its real group span exceeds the safety cap
		// (a pathologically coarse timeline), 404 rather than silently truncating it to a segment
		// shorter than its advertised duration.
		if let Some(end) = end
			&& end.saturating_sub(start) > MAX_SEGMENT_GROUPS
		{
			tracing::warn!(
				start,
				end,
				"segment spans more than MAX_SEGMENT_GROUPS; refusing to truncate"
			);
			return Ok(None);
		}
		let end = end.unwrap_or(u64::MAX).min(start.saturating_add(MAX_SEGMENT_GROUPS));

		let mut muxer = self.muxer()?;
		// Accumulate every group's frames into ONE fragment, so duration inference sees each
		// frame's true successor. A per-group fragment would mis-time the trailing sample of every
		// group (its real successor lives in the next group), audible on audio segments that span
		// many groups.
		let mut frames = Vec::new();
		for sequence in start..end {
			let Some(mut group) = fetch(&track, sequence).await? else {
				// A missing group: a bounded segment left the relay cache (serve nothing); the
				// open-ended final segment simply ends at the last present group.
				if bounded {
					return Ok(None);
				}
				break;
			};
			let Some(mut group_frames) = read_group(&mut muxer, &mut group).await? else {
				// The group aged out of the cache mid-read.
				if bounded {
					return Ok(None);
				}
				break;
			};
			frames.append(&mut group_frames);
		}

		if frames.is_empty() {
			return Ok(None);
		}
		Ok(Some(muxer.fragment(start as u32, &frames)?))
	}
}

impl Drop for Rendition {
	fn drop(&mut self) {
		self.watcher.abort();
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

/// Spawn the per-rendition watcher: resolve the (possibly sibling) broadcast, subscribe to
/// the timeline track, and feed records into the shared window.
fn spawn_watcher(
	source: moq_mux::Source,
	broadcast: Option<moq_net::PathRelativeOwned>,
	section: Timeline,
	live: Arc<segments::Producer>,
	window: Duration,
) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		match watch(source, broadcast, &section, &live, window).await {
			// The timeline finished cleanly: the publisher is done, so its media groups are
			// finalized and the last record can become a servable (ENDLIST) segment.
			Ok(()) => live.end(),
			// A transient error (subscription reset, relay hiccup): don't mark the window ended,
			// which would turn the last record into an open-ended segment whose FETCH could park
			// on a still-open group. The serve path keeps serving the frozen window.
			Err(err) => {
				tracing::warn!(track = %section.track, %err, "timeline watcher error; leaving the playlist live")
			}
		}
		// The timeline stream is over either way: close so a recording cursor terminates instead
		// of parking forever (the serve path still reads the last window).
		live.close();
	})
}

async fn watch(
	source: moq_mux::Source,
	broadcast: Option<moq_net::PathRelativeOwned>,
	section: &Timeline,
	live: &segments::Producer,
	window: Duration,
) -> Result<()> {
	let broadcast = source.resolve(broadcast.as_ref()).await?;
	// Publish the resolved broadcast so segment requests can FETCH from it.
	let _ = live.broadcast.set(broadcast.clone());

	let mut timeline = moq_mux::timeline::Consumer::<()>::subscribe(&broadcast, section).await?;
	while let Some(entry) = timeline.next().await.map_err(moq_mux::Error::from)? {
		live.push(entry, window);
	}
	Ok(())
}
