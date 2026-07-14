//! One rendition's read-side metadata plus the store its media lands in.
//!
//! A `Rendition` carries only what a playlist renderer / HTTP handler needs: the
//! master-playlist attributes and the [`SegmentStore`] fed by the owning
//! [`Broadcaster`](super::Broadcaster)'s poll loop. The per-track
//! [`Export`](moq_mux::container::fmp4::Export) that fills the store lives on the
//! driver side (see `mod.rs`), so nothing here spawns or owns a subscription.

use std::sync::Arc;

use hang::catalog::{AudioConfig, VideoConfig};

use super::Config;
use super::store::SegmentStore;

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
/// segment/part store the driver fills. Cheap to hold behind an `Arc`; the driver
/// and every reader (HTTP handlers, the VOD uploader) share the same store.
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
	/// The segment/part store the driver's exporter fills.
	pub store: Arc<SegmentStore>,
}

impl Rendition {
	/// Build a video rendition (metadata + an empty store). The driver pairs this
	/// with an exporter that fills the store.
	pub(super) fn video(name: String, config: &VideoConfig, cfg: &Config) -> Self {
		Self {
			name,
			kind: Kind::Video,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_VIDEO_BITRATE),
			width: config.coded_width,
			height: config.coded_height,
			codec: config.codec.to_string(),
			store: Arc::new(SegmentStore::new(Kind::Video, cfg)),
		}
	}

	/// Build an audio rendition (metadata + an empty store).
	pub(super) fn audio(name: String, config: &AudioConfig, cfg: &Config) -> Self {
		Self {
			name,
			kind: Kind::Audio,
			bandwidth: config.bitrate.unwrap_or(DEFAULT_AUDIO_BITRATE),
			width: None,
			height: None,
			codec: config.codec.to_string(),
			store: Arc::new(SegmentStore::new(Kind::Audio, cfg)),
		}
	}
}
