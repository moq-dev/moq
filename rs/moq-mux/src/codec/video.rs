//! Shared catalog-publishing logic for the video codec importers.
//!
//! Every video importer resolves its [`VideoConfig`](hang::catalog::VideoConfig) lazily from the
//! bitstream and re-publishes it whenever the stream reveals a change. [`Catalog`] owns the part of
//! that which is identical across codecs: overlay the caller's [`VideoHint`], skip a publish
//! that matches the last one, and drive the shared [`hang::catalog::Stalled`] detector so a
//! lagging rendition is marked in the catalog.

use std::time::{Duration, Instant};

use hang::catalog::{Stalled, StalledSample};
use moq_net::Timestamp;

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
	stalled: Stalled,
	last_source: Option<Instant>,
	last_captured: Option<Timestamp>,
	last_accepted: Option<Timestamp>,
}

impl Catalog {
	/// Hold `hint` for every publish.
	pub(crate) fn new(hint: VideoHint) -> Self {
		Self {
			hint,
			last: None,
			stalled: Stalled::new(),
			last_source: None,
			last_captured: None,
			last_accepted: None,
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

	/// A frame of this rendition was captured and handed to the transport.
	///
	/// `lag` is extra delay on top of the timestamps (a slow encode). Passthrough
	/// imports pass [`Duration::ZERO`].
	pub(crate) fn on_frame(
		&mut self,
		rendition: &mut VideoTrack<impl CatalogExt>,
		timestamp: Timestamp,
		demand: bool,
		lag: Duration,
	) -> crate::Result<()> {
		self.last_captured = Some(max_ts(self.last_captured, timestamp));
		self.last_accepted = Some(max_ts(self.last_accepted, timestamp));
		self.last_source = Some(Instant::now());
		self.publish_stalled(rendition, demand, false, lag)
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
		self.publish_stalled(rendition, demand, false, lag)
	}

	/// The source is gone (camera released). Never stalled.
	pub(crate) fn idle(&mut self, rendition: &mut VideoTrack<impl CatalogExt>) -> crate::Result<()> {
		self.last_source = None;
		self.last_captured = None;
		self.last_accepted = None;
		self.publish_stalled(rendition, false, true, Duration::ZERO)
	}

	fn publish_stalled(
		&mut self,
		rendition: &mut VideoTrack<impl CatalogExt>,
		demand: bool,
		idle: bool,
		extra_lag: Duration,
	) -> crate::Result<()> {
		let media_lag = extra_lag.max(timestamp_lag(self.last_captured, self.last_accepted));
		let quiet = self
			.last_source
			.map(|at| Instant::now().saturating_duration_since(at))
			.unwrap_or(Duration::ZERO);
		let interval = hang::catalog::stalled_interval_from_fps(self.last.as_ref().and_then(|c| c.framerate));
		if !self.stalled.observe(StalledSample {
			media_lag,
			quiet,
			interval,
			demand,
			idle,
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

fn max_ts(current: Option<Timestamp>, next: Timestamp) -> Timestamp {
	match current {
		Some(prev) if prev > next => prev,
		_ => next,
	}
}

fn timestamp_lag(captured: Option<Timestamp>, accepted: Option<Timestamp>) -> Duration {
	match (captured, accepted) {
		(Some(captured), Some(accepted)) if captured > accepted => captured
			.checked_sub(accepted)
			.map(Duration::from)
			.unwrap_or(Duration::ZERO),
		_ => Duration::ZERO,
	}
}
