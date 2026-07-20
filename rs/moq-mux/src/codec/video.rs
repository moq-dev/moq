//! Shared catalog-publishing logic for the video codec importers.
//!
//! Every video importer resolves its [`VideoConfig`](hang::catalog::VideoConfig) lazily from the
//! bitstream and re-publishes it whenever the stream reveals a change. [`Catalog`] owns the part of
//! that which is identical across codecs: overlay the caller's [`VideoHint`], advertise the
//! rendition's timeline, and skip a publish that matches the last one. The rendition itself stays a
//! separate field on the importer, since each drives its frame recording (`record_*`) directly.

use crate::catalog::hang::CatalogExt;
use crate::catalog::{Reserved, VideoHint, VideoTrack};

/// The catalog-publishing state a video importer overlays onto every config it resolves.
///
/// Holds the caller's hint, the rendition's advertised timeline section, and the last config
/// published (to dedupe re-publishes). Not generic over the extension: none of these depend on it.
pub(crate) struct Catalog {
	/// Overlaid onto every config, so a hinted field counts as supplied and is never overwritten by
	/// the rendition's detector.
	hint: VideoHint,
	/// The rendition's timeline section, advertised on every config (the generic `set()` no longer
	/// does). Snapshotted at construction; the timeline track is 1:1 by name.
	timeline: hang::catalog::Timeline,
	/// The last config published, so an unchanged re-resolve doesn't re-mirror the rendition.
	last: Option<hang::catalog::VideoConfig>,
}

impl Catalog {
	/// Snapshot the timeline for the rendition named `name`, and hold `hint` for every publish.
	pub(crate) fn new(reserved: &Reserved<impl CatalogExt>, name: &str, hint: VideoHint) -> Self {
		Self {
			timeline: reserved.producer().timeline(name).section(),
			hint,
			last: None,
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

	/// Overlay the hint and timeline onto `config` and publish it to `rendition`, unless it matches
	/// the last publish. A changed config just re-mirrors the rendition; there are no fixed tracks to
	/// reject a reconfiguration.
	pub(crate) fn publish(
		&mut self,
		rendition: &mut VideoTrack<impl CatalogExt>,
		mut config: hang::catalog::VideoConfig,
	) {
		self.hint.apply(&mut config);
		config.timeline = Some(self.timeline.clone());
		if self.last.as_ref() == Some(&config) {
			return;
		}
		tracing::debug!(name = ?rendition.name(), ?config, "starting track");
		rendition.set(config.clone());
		self.last = Some(config);
	}
}
