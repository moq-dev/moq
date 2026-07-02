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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	/// Video: a segment is a GOP, rolling on each independent fragment.
	Video,
	/// Audio: segments roll on accumulated duration (no keyframes).
	Audio,
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
}

impl Rendition {
	/// Build a video rendition and spawn its exporter pump.
	pub fn video(
		name: String,
		config: &VideoConfig,
		broadcast: moq_net::BroadcastConsumer,
		cfg: &Config,
		paused: watch::Receiver<bool>,
	) -> Self {
		let store = Arc::new(SegmentStore::new(Kind::Video, cfg));
		spawn_pump(broadcast, name.clone(), Kind::Video, store.clone(), cfg.clone(), paused);
		Self {
			name,
			kind: Kind::Video,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_VIDEO_BITRATE),
			width: config.coded_width,
			height: config.coded_height,
			codec: config.codec.to_string(),
			store,
		}
	}

	/// Build an audio rendition and spawn its exporter pump.
	pub fn audio(
		name: String,
		config: &AudioConfig,
		broadcast: moq_net::BroadcastConsumer,
		cfg: &Config,
		paused: watch::Receiver<bool>,
	) -> Self {
		let store = Arc::new(SegmentStore::new(Kind::Audio, cfg));
		spawn_pump(broadcast, name.clone(), Kind::Audio, store.clone(), cfg.clone(), paused);
		Self {
			name,
			kind: Kind::Audio,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_AUDIO_BITRATE),
			width: None,
			height: None,
			codec: config.codec.to_string(),
			store,
		}
	}
}

fn spawn_pump(
	broadcast: moq_net::BroadcastConsumer,
	name: String,
	kind: Kind,
	store: Arc<SegmentStore>,
	cfg: Config,
	paused: watch::Receiver<bool>,
) {
	tokio::spawn(async move {
		if let Err(err) = run_pump(broadcast, &name, kind, &store, &cfg, paused).await {
			tracing::warn!(%name, ?kind, %err, "hls rendition pump ended with error");
		}
		// Whatever happened, mark the playlist closed so blocking readers wake.
		store.finish();
	});
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
