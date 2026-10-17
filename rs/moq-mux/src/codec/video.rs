//! Shared logic for the video codecs.
//!
//! Every video importer resolves its [`VideoConfig`](hang::catalog::VideoConfig) lazily from the
//! bitstream and re-publishes it whenever the stream reveals a change. [`Catalog`] owns the part of
//! that which is identical across codecs: overlay the caller's [`VideoHint`], skip a publish
//! that matches the last one. [`Reorder`] is what a sequence header declares about frame
//! reordering, read by the exporters that author a decode clock.

use crate::catalog::VideoHint;

/// The predefined sample aspects shared by H.264 and H.265 (their VUI Table E-1).
pub(super) fn sample_aspect(idc: u8) -> Option<(u32, u32)> {
	Some(match idc {
		1 => (1, 1),
		2 => (12, 11),
		3 => (10, 11),
		4 => (16, 11),
		5 => (40, 33),
		6 => (24, 11),
		7 => (20, 11),
		8 => (32, 11),
		9 => (80, 33),
		10 => (18, 11),
		11 => (15, 11),
		12 => (64, 33),
		13 => (160, 99),
		14 => (4, 3),
		15 => (3, 2),
		16 => (2, 1),
		_ => return None,
	})
}

/// Resolve non-square samples to the whole picture's reduced display aspect.
pub(super) fn display_aspect(width: u32, height: u32, sample: (u32, u32)) -> Option<(u32, u32)> {
	let (sample_width, sample_height) = sample;
	if width == 0 || height == 0 || sample_width == 0 || sample_height == 0 || sample_width == sample_height {
		return None;
	}
	let shown_width = u64::from(width) * u64::from(sample_width);
	let shown_height = u64::from(height) * u64::from(sample_height);
	let (mut divisor, mut remainder) = (shown_width, shown_height);
	while remainder != 0 {
		(divisor, remainder) = (remainder, divisor % remainder);
	}
	Some((
		u32::try_from(shown_width / divisor).ok()?,
		u32::try_from(shown_height / divisor).ok()?,
	))
}

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

impl Reorder {
	/// The minimum decode delay and lookahead in 90 kHz ticks.
	pub(crate) fn timing(self, framerate: Option<f64>) -> (u64, u64) {
		let period = match self.period {
			Some((units, scale)) => units.saturating_mul(90_000).div_ceil(scale),
			None => framerate
				.filter(|fps| fps.is_finite() && *fps > 0.0)
				.map_or(0, |fps| (90_000.0 / fps).ceil() as u64),
		};
		let delay = u64::from(self.depth).saturating_mul(period);
		let lookahead = 1u64.checked_shl(self.depth).unwrap_or(u64::MAX).saturating_mul(period);
		(delay, lookahead)
	}
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
