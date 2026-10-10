//! Shared logic for the video codecs.
//!
//! Every video importer resolves its [`VideoConfig`](hang::catalog::VideoConfig) lazily from the
//! bitstream and re-publishes it whenever the stream reveals a change. [`Catalog`] owns the part of
//! that which is identical across codecs: overlay the caller's [`VideoHint`], skip a publish
//! that matches the last one. [`Reorder`] is what a sequence header declares about frame
//! reordering, read by the exporters that author a decode clock.

use crate::catalog::VideoHint;

type Track = crate::container::Producer<crate::catalog::hang::Container, hang::catalog::VideoConfig>;

/// The reordering a video sequence header declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reorder {
	/// The most pictures that can precede any picture in decode order and follow it in output
	/// order: H.264 `max_num_reorder_frames`, HEVC `sps_max_num_reorder_pics`. A count of
	/// pictures, not a time.
	pub depth: u32,
	/// One picture's duration as `(units, scale)`, i.e. `units / scale` seconds, when the header
	/// declares a fixed picture rate.
	pub period: Option<(u64, u64)>,
}

/// The hypothetical reference decoder a video sequence header declares: its NAL HRD's last
/// schedule, the one the transport stream's decoder buffers are sized from (ISO 13818-1
/// 2.14.3.1, 2.17.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hrd {
	/// The CPB's input rate in bits per second.
	pub bit_rate: u64,
	/// The CPB's size in bits.
	pub cpb_size: u64,
}

/// The catalog-publishing state a video importer overlays onto every config it resolves.
///
/// Holds the caller's hint and the last config published (to dedupe re-publishes). Not generic
/// over the extension: neither depends on it.
pub(crate) struct Catalog {
	/// Overlaid onto every config, so a hinted field counts as supplied and is never overwritten by
	/// the rendition's detector.
	hint: VideoHint,
	/// The last config published, so an unchanged re-resolve doesn't re-mirror the rendition.
	last: Option<hang::catalog::VideoConfig>,
}

impl Catalog {
	/// Hold `hint` for every publish.
	pub(crate) fn new(hint: VideoHint) -> Self {
		Self { hint, last: None }
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
	/// reject a reconfiguration.
	pub(crate) fn publish(&mut self, track: &mut Track, mut config: hang::catalog::VideoConfig) -> crate::Result<()> {
		self.hint.apply(&mut config);
		if self.last.as_ref() == Some(&config) {
			return Ok(());
		}
		tracing::debug!(name = ?track.name(), ?config, "starting track");
		track.set(config.clone())?;
		self.last = Some(config);
		Ok(())
	}
}
