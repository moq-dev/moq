//! One rendition: a per-track exporter pumping CMAF fragments into a store.

use std::sync::Arc;

use hang::catalog::{AudioConfig, VideoConfig};
use moq_mux::catalog::{self, CatalogFormat, Filter, FilterAudio, FilterVideo};
use moq_mux::container::fmp4::Export;

use super::Config;
use super::store::SegmentStore;
use crate::Result;

/// Fallback advertised bitrates when the catalog doesn't carry one.
const DEFAULT_VIDEO_BITRATE: u64 = 2_000_000;
const DEFAULT_AUDIO_BITRATE: u64 = 128_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Video,
	Audio,
}

/// A single HLS rendition: its display metadata for the master playlist plus the
/// segment/part store fed by a background exporter task.
pub struct Rendition {
	pub name: String,
	pub kind: Kind,
	pub bandwidth: u64,
	pub width: Option<u32>,
	pub height: Option<u32>,
	/// RFC 6381 codec string for the master playlist `CODECS` attribute.
	pub codec: String,
	pub store: Arc<SegmentStore>,
}

impl Rendition {
	pub fn video(name: String, config: &VideoConfig, broadcast: moq_net::BroadcastConsumer, cfg: &Config) -> Self {
		let store = Arc::new(SegmentStore::new(
			true,
			cfg.part_target.as_secs_f64(),
			cfg.audio_segment_target.as_secs_f64(),
			cfg.window.as_secs_f64(),
		));
		spawn_pump(broadcast, name.clone(), Kind::Video, store.clone(), cfg.clone());
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

	pub fn audio(name: String, config: &AudioConfig, broadcast: moq_net::BroadcastConsumer, cfg: &Config) -> Self {
		let store = Arc::new(SegmentStore::new(
			false,
			cfg.part_target.as_secs_f64(),
			cfg.audio_segment_target.as_secs_f64(),
			cfg.window.as_secs_f64(),
		));
		spawn_pump(broadcast, name.clone(), Kind::Audio, store.clone(), cfg.clone());
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

fn spawn_pump(broadcast: moq_net::BroadcastConsumer, name: String, kind: Kind, store: Arc<SegmentStore>, cfg: Config) {
	tokio::spawn(async move {
		if let Err(err) = run_pump(broadcast, &name, kind, &store, &cfg).await {
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
) -> Result<()> {
	let consumer = catalog::Consumer::new(&broadcast, CatalogFormat::Hang).await?;
	let mut filter = Filter::new(consumer);

	// Narrow *both* axes to this rendition's name so the exporter sees exactly one
	// track: the opposite axis can't hold a rendition with this name, so it empties.
	filter.set_video(FilterVideo {
		name: Some(name.to_string()),
		..Default::default()
	});
	filter.set_audio(FilterAudio {
		name: Some(name.to_string()),
		..Default::default()
	});
	let _ = kind; // kind only drives the store policy; the exporter is codec-agnostic.

	let mut export = Export::new(broadcast, filter)
		.with_fragment_duration(cfg.part_target)
		.with_latency(cfg.latency);

	while let Some(fragment) = export.next_fragment().await? {
		store.push(fragment);
	}

	Ok(())
}
