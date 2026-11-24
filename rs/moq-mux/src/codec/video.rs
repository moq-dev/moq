//! Shared logic for the video codecs.
//!
//! Every video importer resolves its [`VideoConfig`](hang::catalog::VideoConfig) lazily from the
//! bitstream and re-publishes it whenever the stream reveals a change. [`Catalog`] owns the part of
//! that which is identical across codecs: overlay the caller's [`VideoHint`], skip a publish
//! that matches the last one. [`Reorder`] is what a sequence header declares about frame
//! reordering, read by the exporters that author a decode clock.

use crate::catalog::VideoHint;

/// Mark an H.264 or H.265 config in band when the first keyframe after its record carries any
/// parameter set.
///
/// The record's parameter sets seed every keyframe; any one in band (an SPS, a PPS, or an H.265
/// VPS) means a keyframe may also change or supply them, which only the `avc3`/`hev1` codec string
/// allows. `keyframe` is length-prefixed by the record's length size. A config without a record is
/// left alone.
pub(crate) fn detect_in_band(config: &mut hang::catalog::VideoConfig, keyframe: &[u8]) -> crate::Result<()> {
	let mut scan = InBand::new(config)?;
	scan.scan(keyframe);
	scan.apply(config);
	Ok(())
}

/// [`detect_in_band`] over a keyframe that arrives in pieces, holding only the current NAL unit's
/// length prefix and how much of it is left.
pub(crate) struct InBand {
	/// How the record frames the keyframe, or `None` for a config this leaves alone.
	framing: Option<Framing>,
	/// The length prefix read so far, and how many of its bytes.
	length: usize,
	read: usize,
	/// The bytes left in the current NAL unit, whose first is its header when `header` is set.
	remaining: usize,
	header: bool,
	found: bool,
}

/// A record's NALU length size and its codec's check for a parameter-set NAL header.
#[derive(Clone, Copy)]
struct Framing {
	length_size: usize,
	is_parameter_set: fn(u8) -> bool,
}

impl InBand {
	/// A scan for the record `config` carries.
	pub(crate) fn new(config: &hang::catalog::VideoConfig) -> crate::Result<Self> {
		let codec = match (&config.codec, config.description.as_deref()) {
			(hang::catalog::VideoCodec::H264(_), Some(record)) => Some(Framing {
				length_size: super::h264::Avcc::parse(record)?.length_size,
				is_parameter_set: super::h264::is_parameter_set,
			}),
			(hang::catalog::VideoCodec::H265(_), Some(record)) => Some(Framing {
				length_size: super::h265::Hvcc::parse(record)?.length_size,
				is_parameter_set: super::h265::is_parameter_set,
			}),
			_ => None,
		};
		Ok(Self {
			framing: codec,
			length: 0,
			read: 0,
			remaining: 0,
			header: false,
			found: false,
		})
	}

	/// Read the next bytes of the keyframe.
	pub(crate) fn scan(&mut self, mut bytes: &[u8]) {
		let Some(Framing {
			length_size,
			is_parameter_set,
		}) = self.framing
		else {
			return;
		};
		while !self.found && !bytes.is_empty() {
			if self.remaining > 0 {
				if std::mem::take(&mut self.header) {
					self.found = is_parameter_set(bytes[0]);
				}
				let skip = self.remaining.min(bytes.len());
				self.remaining -= skip;
				bytes = &bytes[skip..];
			} else {
				self.length = (self.length << 8) | usize::from(bytes[0]);
				self.read += 1;
				bytes = &bytes[1..];
				if self.read == length_size {
					(self.remaining, self.header) = (self.length, self.length > 0);
					(self.length, self.read) = (0, 0);
				}
			}
		}
	}

	/// Mark `config` in band if the bytes read so far carried a parameter set.
	pub(crate) fn apply(&self, config: &mut hang::catalog::VideoConfig) {
		match &mut config.codec {
			hang::catalog::VideoCodec::H264(codec) => codec.inline |= self.found,
			hang::catalog::VideoCodec::H265(codec) => codec.in_band |= self.found,
			_ => {}
		}
	}
}

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
	reduce_aspect(
		u128::from(width) * u128::from(sample_width),
		u128::from(height) * u128::from(sample_height),
	)
}

/// Reduce positive display dimensions to the catalog's integer ratio without truncating.
pub(crate) fn reduce_aspect(width: u128, height: u128) -> Option<(u32, u32)> {
	if width == 0 || height == 0 {
		return None;
	}
	let (mut divisor, mut remainder) = (width, height);
	while remainder != 0 {
		(divisor, remainder) = (remainder, divisor % remainder);
	}
	Some((
		u32::try_from(width / divisor).ok()?,
		u32::try_from(height / divisor).ok()?,
	))
}

/// Fill a missing display aspect from container metadata, retaining and diagnosing SPS conflicts.
pub(crate) fn fill_display_aspect(config: &mut hang::catalog::VideoConfig, aspect: (u32, u32), source: &str) {
	if let (Some(width), Some(height)) = (config.display_aspect_width, config.display_aspect_height) {
		if u64::from(width) * u64::from(aspect.1) != u64::from(height) * u64::from(aspect.0) {
			tracing::warn!(kept = ?(width, height), other = ?aspect, source, "display aspect disagrees with the stream; keeping the stream's");
		}
	} else {
		config.display_aspect_width = Some(aspect.0);
		config.display_aspect_height = Some(aspect.1);
	}
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
	/// The deepest reordering a conforming sequence header declares: no decoded picture buffer
	/// holds more than 16 frames (H.264 A.3.1, HEVC A.4.2).
	pub(crate) const MAX_DEPTH: u32 = 16;

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

/// The deepest catalog `reorder` a rendition's codec and level can reach: what fits the level's
/// decoded picture buffer at the coded size (H.264 A.3.1 MaxDpbFrames, H.265 A.4.2 MaxDpbSize
/// less the current picture), or the codec's largest buffer when the level or size is unknown.
/// VP8, VP9 and AV1 present in decode order.
pub(crate) fn max_reorder(config: &hang::catalog::VideoConfig) -> u32 {
	use hang::catalog::VideoCodec;

	match &config.codec {
		VideoCodec::H264(h264) => {
			// level_idc 11 with constraint_set3_flag is level 1b for Baseline, Main and Extended.
			let level_1b = h264.level == 11 && h264.constraints & 0x10 != 0 && matches!(h264.profile, 66 | 77 | 88);
			let max_dpb_mbs: Option<u64> = match h264.level {
				_ if level_1b => Some(396),
				9 | 10 => Some(396),
				11 => Some(900),
				12 | 13 | 20 => Some(2_376),
				21 => Some(4_752),
				22 | 30 => Some(8_100),
				31 => Some(18_000),
				32 => Some(20_480),
				40 | 41 => Some(32_768),
				42 => Some(34_816),
				50 => Some(110_400),
				51 | 52 => Some(184_320),
				60..=62 => Some(696_320),
				_ => None,
			};
			let frame_mbs = config
				.coded_width
				.zip(config.coded_height)
				.map(|(width, height)| u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16)));
			match max_dpb_mbs.zip(frame_mbs.filter(|mbs| *mbs > 0)) {
				Some((dpb, frame)) => (dpb / frame).min(u64::from(Reorder::MAX_DEPTH)) as u32,
				None => Reorder::MAX_DEPTH,
			}
		}
		VideoCodec::H265(h265) => {
			let max_luma_ps: Option<u64> = match h265.level_idc {
				30 => Some(36_864),
				60 => Some(122_880),
				63 => Some(245_760),
				90 => Some(552_960),
				93 => Some(983_040),
				120 | 123 => Some(2_228_224),
				150 | 153 | 156 => Some(8_912_896),
				180 | 183 | 186 => Some(35_651_584),
				_ => None,
			};
			let samples = config
				.coded_width
				.zip(config.coded_height)
				.map(|(width, height)| u64::from(width) * u64::from(height));
			let max_dpb_size = match max_luma_ps.zip(samples) {
				Some((luma, size)) if size <= luma >> 2 => 16,
				Some((luma, size)) if size <= luma >> 1 => 12,
				Some((luma, size)) if size <= (3 * luma) >> 2 => 8,
				Some(_) => 6,
				None => 16,
			};
			max_dpb_size - 1
		}
		VideoCodec::VP8 | VideoCodec::VP9(_) | VideoCodec::AV1(_) => 0,
		_ => Reorder::MAX_DEPTH,
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
	/// The last config resolved, before the overlays, so new HDR metadata can republish it.
	resolved: Option<hang::catalog::VideoConfig>,
	/// The static HDR metadata the bitstream carried in band, overlaid onto every config.
	hdr: crate::codec::color::Hdr,
	/// Whether the publisher is producing this rendition, overlaid onto every config.
	enabled: bool,
}

impl Catalog {
	/// Hold `hint` for every publish.
	pub(crate) fn new(hint: VideoHint) -> Self {
		Self {
			hint,
			last: None,
			resolved: None,
			hdr: Default::default(),
			enabled: true,
		}
	}

	/// Advertise whether the publisher is producing this rendition, republishing the last
	/// config if it changed. Overlaid onto every later config, so a resolution change
	/// while disabled stays disabled.
	pub(crate) fn set_enabled(&mut self, track: &mut Track, enabled: bool) -> crate::Result<()> {
		if self.enabled == enabled {
			return Ok(());
		}
		self.enabled = enabled;
		match self.resolved.clone() {
			Some(config) => self.publish(track, config),
			None => Ok(()),
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

	/// The out-of-band record the last published config carries, if any.
	pub(crate) fn description(&self) -> Option<&[u8]> {
		self.last.as_ref()?.description.as_deref()
	}

	/// Overlay the hint onto `config` and publish it to `rendition`, unless it matches the last
	/// publish. A decoder identity change finishes the previous track and mints a fresh name;
	/// anything else updates the rendition in place.
	///
	/// A declared `reorder` is fixed for the track's life, so a later sequence header declaring
	/// less or none keeps it, and one declaring more fails: frames reordered that deep would
	/// break the bound consumers already clock the track by. A minted track declares its own.
	pub(crate) fn publish(&mut self, track: &mut Track, mut config: hang::catalog::VideoConfig) -> crate::Result<()> {
		self.resolved = Some(config.clone());
		self.hdr.apply(&mut config.color);
		self.hint.apply(&mut config);
		config.enabled = self.enabled;
		// A new decoder identity mints a new track, whose bound starts over.
		let declared = self
			.last
			.as_ref()
			.filter(|last| last.same_identity(&config))
			.and_then(|last| last.reorder);
		if let Some(declared) = declared {
			match config.reorder {
				Some(reorder) if reorder > declared => {
					return Err(crate::container::ReorderError::Deepened { declared, reorder }.into());
				}
				_ => config.reorder = Some(declared),
			}
		}
		if self.last.as_ref() == Some(&config) {
			return Ok(());
		}
		tracing::debug!(name = ?track.name(), ?config, "starting track");
		track.reconfigure(config.clone())?;
		self.last = Some(config);
		Ok(())
	}

	/// Overlay the HDR metadata a keyframe carried in band, republishing the last config if it changed.
	///
	/// `sequence` marks a keyframe that starts a new coded sequence (a new SPS or sequence header):
	/// its metadata replaces the old rather than merging, so an HDR-to-SDR switch drops stale HDR10
	/// values, and it is left for the new config's publish.
	pub(crate) fn hdr(
		&mut self,
		track: &mut Track,
		hdr: crate::codec::color::Hdr,
		sequence: bool,
	) -> crate::Result<()> {
		if sequence {
			self.hdr = hdr;
			return Ok(());
		}
		let mut next = self.hdr;
		next.update(hdr);
		if next == self.hdr {
			return Ok(());
		}
		self.hdr = next;
		match self.resolved.clone() {
			Some(config) => self.publish(track, config),
			None => Ok(()),
		}
	}
}

#[cfg(test)]
mod tests {
	/// The in-band scan reads NAL headers only, never their bodies, and finds the same answer however
	/// the keyframe is split.
	#[test]
	fn in_band_scan_matches_across_any_split() {
		use crate::codec::h264::fixtures::{PPS, SPS_BT709};
		use bytes::Bytes;

		let avcc =
			crate::codec::h264::build_avcc(&[Bytes::from_static(SPS_BT709)], &[Bytes::from_static(PPS)]).unwrap();
		let config = crate::codec::h264::config(&avcc).unwrap();
		// An IDR whose body is full of bytes that would read as an SPS header.
		let idr: &[u8] = &[0x65, 0x67, 0x67, 0x68, 0x67];
		let au = |nals: &[&[u8]]| -> Vec<u8> {
			nals.iter()
				.flat_map(|nal| [&(nal.len() as u32).to_be_bytes()[..], nal].concat())
				.collect()
		};
		for (keyframe, expected) in [
			(au(&[idr]), false),
			(au(&[idr, idr]), false),
			(au(&[SPS_BT709, PPS, idr]), true),
			(au(&[idr, PPS]), true),
			(au(&[&[], idr]), false),
		] {
			let detected = |pieces: &[&[u8]]| {
				let mut scan = InBand::new(&config).unwrap();
				for piece in pieces {
					scan.scan(piece);
				}
				let mut config = config.clone();
				scan.apply(&mut config);
				config.codec.to_string().starts_with("avc3")
			};
			assert_eq!(detected(&[&keyframe]), expected, "{keyframe:?}");
			for at in 0..=keyframe.len() {
				let (head, tail) = keyframe.split_at(at);
				assert_eq!(detected(&[head, tail]), expected, "{keyframe:?} split at {at}");
			}
			let bytes: Vec<&[u8]> = keyframe.chunks(1).collect();
			assert_eq!(detected(&bytes), expected, "{keyframe:?} byte at a time");
		}
	}

	use super::*;
	use crate::container::Frame;

	#[test]
	fn decoder_changes_mint_fresh_names_but_size_and_estimates_stay_in_place() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::<()>::new(&mut broadcast, Default::default()).unwrap();
		let raw = broadcast
			.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut track = catalog
			.video(
				raw,
				crate::catalog::hang::Container::Legacy(crate::container::Kind::Video),
				None,
			)
			.unwrap();
		let mut publishing = Catalog::new(VideoHint::default());
		let mut config = hang::catalog::VideoConfig::new("avc1.42c01e".parse::<hang::catalog::VideoCodec>().unwrap());
		config.description = Some(bytes::Bytes::from_static(b"first"));
		publishing.publish(&mut track, config.clone()).unwrap();
		track
			.write(Frame {
				timestamp: moq_net::Timestamp::ZERO,
				payload: bytes::Bytes::from_static(b"old"),
				keyframe: true,
				duration: None,
			})
			.unwrap();
		let old = track.track().clone();
		config.coded_width = Some(640);
		config.coded_height = Some(360);
		config.bitrate = Some(500_000);
		config.framerate = Some(30.0);
		publishing.publish(&mut track, config.clone()).unwrap();
		assert_eq!(track.name(), "video");
		config.bitrate = Some(250_000);
		config.framerate = Some(15.0);
		publishing.publish(&mut track, config.clone()).unwrap();
		assert_eq!(track.name(), "video");
		config.description = Some(bytes::Bytes::from_static(b"second"));
		publishing.publish(&mut track, config.clone()).unwrap();
		let second = track.name().to_string();
		assert_ne!(second, "video");
		assert!(!catalog.snapshot().video.renditions.contains_key("video"));
		assert_eq!(catalog.snapshot().video.renditions[&second], config);
		assert_eq!(old.largest().map(|p| p.group), Some(0));
		assert!(
			old.poll_closed(&kio::Waiter::noop()).is_ready(),
			"the previous track ends"
		);
		track
			.write(Frame {
				timestamp: moq_net::Timestamp::ZERO,
				payload: bytes::Bytes::from_static(b"new"),
				keyframe: true,
				duration: None,
			})
			.unwrap();
		assert_eq!(track.track().largest().map(|p| p.group), Some(0));
		config.description = Some(bytes::Bytes::from_static(b"first"));
		publishing.publish(&mut track, config).unwrap();
		assert_ne!(track.name(), "video");
		assert_ne!(track.name(), second);
		let _finished = track.finish().unwrap();
	}
}
