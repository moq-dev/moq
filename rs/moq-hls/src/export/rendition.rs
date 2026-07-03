//! One rendition: a per-track exporter pumping CMAF fragments into a store.

use std::sync::Arc;

use hang::catalog::{AudioConfig, VideoConfig};
use moq_mux::catalog::{self, CatalogFormat, Stream};
use moq_mux::container::fmp4::Export;
use moq_mux::select;
use tokio::sync::watch;

use super::Config;
use super::store::SegmentStore;
use crate::Result;

/// Fallback advertised bitrates when the catalog doesn't carry one.
const DEFAULT_VIDEO_BITRATE: u64 = 2_000_000;
const DEFAULT_AUDIO_BITRATE: u64 = 128_000;

/// Whether a rendition carries video or audio (drives the store's segmenting policy).
///
/// Also the first URL path component of a rendition (`/{broadcast}/{kind}/{name}/...`),
/// so video and audio renditions that share a name don't collide.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Kind {
	/// Video: a segment is a GOP, rolling on each independent fragment.
	Video,
	/// Audio: segments roll on accumulated duration (no keyframes).
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

/// Shared context passed to every rendition constructor: the broadcast to pull
/// from, the export tuning, and the pause signal. Grouped so the constructors
/// don't take a long list of easily-transposed positional arguments.
pub(crate) struct Context<'a> {
	/// The broadcast whose track this rendition exports.
	pub broadcast: moq_net::BroadcastConsumer,
	/// Export tuning shared across renditions.
	pub cfg: &'a Config,
	/// Pause signal shared with every rendition pump.
	pub paused: watch::Receiver<bool>,
}

/// A single HLS rendition: its display metadata for the master playlist plus the
/// segment/part store fed by a background exporter task.
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
	/// The segment/part store fed by this rendition's exporter task.
	pub store: Arc<SegmentStore>,
	/// Aborts the background exporter pump when the rendition is dropped, so a
	/// pump doesn't outlive the [`Broadcaster`](super::Broadcaster) (and server)
	/// that owns it.
	pump: tokio::task::JoinHandle<()>,
}

impl Rendition {
	/// Build a video rendition and spawn its exporter pump.
	pub(crate) fn video(name: String, config: &VideoConfig, ctx: Context<'_>) -> Self {
		let store = Arc::new(SegmentStore::new(Kind::Video, ctx.cfg));
		let pump = spawn_pump(name.clone(), Kind::Video, store.clone(), &ctx);
		Self {
			name,
			kind: Kind::Video,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_VIDEO_BITRATE),
			width: config.coded_width,
			height: config.coded_height,
			codec: config.codec.to_string(),
			store,
			pump,
		}
	}

	/// Build an audio rendition and spawn its exporter pump.
	pub(crate) fn audio(name: String, config: &AudioConfig, ctx: Context<'_>) -> Self {
		let store = Arc::new(SegmentStore::new(Kind::Audio, ctx.cfg));
		let pump = spawn_pump(name.clone(), Kind::Audio, store.clone(), &ctx);
		Self {
			name,
			kind: Kind::Audio,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_AUDIO_BITRATE),
			width: None,
			height: None,
			codec: config.codec.to_string(),
			store,
			pump,
		}
	}
}

impl Drop for Rendition {
	fn drop(&mut self) {
		self.pump.abort();
	}
}

fn spawn_pump(name: String, kind: Kind, store: Arc<SegmentStore>, ctx: &Context<'_>) -> tokio::task::JoinHandle<()> {
	let broadcast = ctx.broadcast.clone();
	let cfg = ctx.cfg.clone();
	let paused = ctx.paused.clone();
	tokio::spawn(async move {
		if let Err(err) = run_pump(broadcast, &name, kind, &store, &cfg, paused).await {
			tracing::warn!(%name, ?kind, %err, "hls rendition pump ended with error");
		}
		// Whatever happened, mark the playlist closed so blocking readers wake.
		store.finish();
	})
}

async fn run_pump(
	broadcast: moq_net::BroadcastConsumer,
	name: &str,
	kind: Kind,
	store: &SegmentStore,
	cfg: &Config,
	mut paused: watch::Receiver<bool>,
) -> Result<()> {
	let consumer = catalog::Consumer::<()>::new(&broadcast, CatalogFormat::Hang).await?;

	// Select this rendition's name on its own axis so the exporter sees exactly one track.
	let selection = match kind {
		Kind::Video => select::Broadcast::default().video(select::Video::default().name(name)),
		Kind::Audio => select::Broadcast::default().audio(select::Audio::default().name(name)),
	};
	let filtered = consumer.select(selection);

	// A handle for noticing the broadcast close even while paused; the `Export`
	// below takes its own clone for pulling fragments.
	let closed = broadcast.clone();

	let mut export = Export::new(broadcast, filtered)
		.with_fragment_duration(cfg.part_target)
		.with_latency(cfg.latency);

	// Whether we just resumed, so the first post-resume fragment opens a new
	// continuity region (`#EXT-X-DISCONTINUITY`).
	let mut resumed = false;

	loop {
		// While paused, stop reading the track entirely: the relay stops sending, so
		// nothing is buffered here and the publisher isn't kept ingesting for a
		// receiver that isn't recording.
		while *paused.borrow_and_update() {
			resumed = true;
			tokio::select! {
				// Resume request, or the Broadcaster (and its sender) being dropped.
				res = paused.changed() => {
					if res.is_err() {
						return Ok(()); // Broadcaster gone: stop pumping.
					}
				}
				// The broadcast ending while paused still finalizes the track.
				_ = kio::wait(|w| closed.poll_closed(w)) => return Ok(()),
			}
		}

		if resumed {
			// The media dropped while paused is a real gap, so tag the seam. The export
			// recovers on its own: the group it was mid-read on aged out of the relay
			// cache while we weren't reading, and reading an evicted (or now-missing)
			// group errors instead of blocking (moq-net aborts it with `Error::Old`), so
			// the consumer skips the evicted span and resumes from the NEXT group still in
			// the cache (`recv_group`), reading forward -- not jumping to live.
			store.mark_discontinuity();
			resumed = false;
		}

		// Pull one fragment uninterrupted (next_fragment isn't cancel-safe), then
		// re-check the pause flag at the top of the loop -- so entering a pause costs at
		// most one extra fragment (~part_target), recording right up to the pause point.
		match export.next_fragment().await? {
			Some(fragment) => store.push(fragment),
			None => break,
		}
	}

	Ok(())
}
