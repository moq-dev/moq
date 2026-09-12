//! Shared catalog-publishing logic for the video codec importers.
//!
//! Every video importer resolves its [`VideoConfig`](hang::catalog::VideoConfig) lazily from the
//! bitstream and re-publishes it whenever the stream reveals a change. [`Catalog`] owns the part of
//! that which is identical across codecs: overlay the caller's [`VideoHint`], skip a publish
//! that matches the last one, and drive the shared [`hang::catalog::stalled::Detector`] detector so a
//! lagging rendition is marked in the catalog.

use std::time::{Duration, Instant};

use hang::catalog::stalled::{Detector, Sample};

use crate::catalog::hang::CatalogExt;
use crate::catalog::{VideoHint, VideoTrack};

/// The catalog-publishing state a video importer overlays onto every config it resolves.
///
/// Holds the caller's hint, the last config published (to dedupe re-publishes), and the
/// stalled detector. Not generic over the extension: none of that depends on it.
pub(crate) struct Catalog {
	/// Overlaid onto every config, so a hinted field counts as supplied and is never overwritten by
	/// the rendition's detector.
	hint: VideoHint,
	/// The last config published, so an unchanged re-resolve doesn't re-mirror the rendition.
	last: Option<hang::catalog::VideoConfig>,
	stalled: Detector,
	last_source: Option<Instant>,
	lag: Duration,
}

impl Catalog {
	/// Hold `hint` for every publish.
	pub(crate) fn new(hint: VideoHint) -> Self {
		Self {
			hint,
			last: None,
			stalled: Detector::new(),
			last_source: None,
			lag: Duration::ZERO,
		}
	}

	/// The config the hint alone resolves to, for importers that publish the catalog before parsing
	/// the stream (a hint carrying a codec). `None` if the hint lacks a codec. See [`VideoHint::to_config`].
	pub(crate) fn initial_config(&self) -> Option<hang::catalog::VideoConfig> {
		self.hint.to_config()
	}

	/// Whether a config has been published yet, so an importer can tell a still-unconfigured stream
	/// (an undecodable keyframe, a mid-join leftover) from a resolved one.
	pub(crate) fn configured(&self) -> bool {
		self.last.is_some()
	}

	/// Overlay the hint onto `config` and publish it to `rendition`, unless it matches the last
	/// publish. A changed config just re-mirrors the rendition; there are no fixed tracks to
	/// reject a reconfiguration. The detector's current flag is applied so a bitstream republish
	/// cannot clear a stall.
	pub(crate) fn publish(
		&mut self,
		rendition: &mut VideoTrack<impl CatalogExt>,
		mut config: hang::catalog::VideoConfig,
	) -> crate::Result<()> {
		self.hint.apply(&mut config);
		config.stalled = self.stalled.flag();
		if self.last.as_ref() == Some(&config) {
			return Ok(());
		}
		tracing::debug!(name = ?rendition.name(), ?config, "starting track");
		rendition.set(config.clone())?;
		self.last = Some(config);
		Ok(())
	}

	/// A frame was handed to the transport, completing one recovery observation.
	pub(crate) fn on_frame(&mut self, rendition: &mut VideoTrack<impl CatalogExt>, demand: bool) -> crate::Result<()> {
		self.last_source = Some(Instant::now());
		self.publish_stalled(rendition, demand, true, self.lag)
	}

	/// Re-evaluate stall from silence: the source has not delivered since the last frame.
	pub(crate) fn tick(&mut self, rendition: &mut VideoTrack<impl CatalogExt>, demand: bool) -> crate::Result<()> {
		self.publish_stalled(rendition, demand, false, Duration::ZERO)
	}

	/// Record extra delay (a slow encode) without treating it as a new source frame.
	pub(crate) fn observe_lag(
		&mut self,
		rendition: &mut VideoTrack<impl CatalogExt>,
		demand: bool,
		lag: Duration,
	) -> crate::Result<()> {
		self.lag = lag;
		self.publish_stalled(rendition, demand, false, lag)
	}

	/// The source is gone (camera released). Never stalled.
	pub(crate) fn idle(&mut self, rendition: &mut VideoTrack<impl CatalogExt>) -> crate::Result<()> {
		self.last_source = None;
		self.lag = Duration::ZERO;
		self.publish_stalled(rendition, false, false, Duration::ZERO)
	}

	fn publish_stalled(
		&mut self,
		rendition: &mut VideoTrack<impl CatalogExt>,
		demand: bool,
		frame: bool,
		extra_lag: Duration,
	) -> crate::Result<()> {
		if demand {
			self.last_source.get_or_insert_with(Instant::now);
		} else {
			self.last_source = None;
		}
		let media_lag = extra_lag;
		let quiet = self
			.last_source
			.map(|at| Instant::now().saturating_duration_since(at))
			.unwrap_or(Duration::ZERO);
		let interval = hang::catalog::stalled::interval_from_fps(self.last.as_ref().and_then(|c| c.framerate));
		if !self.stalled.observe(Sample {
			frame,
			media_lag,
			quiet,
			interval,
			demand,
			idle: !demand,
		}) {
			return Ok(());
		}
		let flag = self.stalled.flag();
		if let Some(last) = self.last.as_mut() {
			last.stalled = flag;
		}
		rendition.update(|config| config.stalled = flag)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn quiet_startup_tracks_demand() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let reserved = catalog.reserve();
		let mut rendition = reserved.video("video").unwrap();
		let mut state = Catalog::new(VideoHint::default());
		state
			.publish(
				&mut rendition,
				hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8),
			)
			.unwrap();

		state.tick(&mut rendition, true).unwrap();
		let start = state
			.last_source
			.expect("demand starts the quiet clock before the first frame");
		state.last_source = Some(start - Duration::from_millis(200));
		state.tick(&mut rendition, true).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["video"].stalled, Some(true));

		state.tick(&mut rendition, false).unwrap();
		assert!(state.last_source.is_none());
		assert_eq!(catalog.snapshot().video.renditions["video"].stalled, None);
		state.tick(&mut rendition, true).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["video"].stalled, None);

		state
			.observe_lag(&mut rendition, true, Duration::from_millis(200))
			.unwrap();
		state.on_frame(&mut rendition, true).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["video"].stalled, Some(true));
		state.observe_lag(&mut rendition, true, Duration::ZERO).unwrap();
		for _ in 0..2 {
			state.on_frame(&mut rendition, true).unwrap();
			for _ in 0..10 {
				state.tick(&mut rendition, true).unwrap();
			}
			assert_eq!(catalog.snapshot().video.renditions["video"].stalled, Some(true));
		}
		state.on_frame(&mut rendition, true).unwrap();
		assert_eq!(catalog.snapshot().video.renditions["video"].stalled, None);
	}
}
