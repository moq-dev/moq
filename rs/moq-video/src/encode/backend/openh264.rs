//! Software H.264 backend via Cisco's openh264 (vendored, statically linked).
//!
//! The fallback when no hardware encoder is available. Emits Annex-B with
//! in-band SPS/PPS, ready for `moq_mux::codec::h264::Import` in avc3 mode.
//!
//! openh264 has no constant-quality mode: `RC_QUALITY_MODE` targets the
//! bitrate the same way `RC_BITRATE_MODE` does, and coded the test clips within
//! 1% of it. Its rate control does honor a quantizer floor, so the target is the
//! cap and the floor ([`MIN_QP`] unless [`Config::qp_floor`] sets another) is the
//! quality: a scene that needs more bits than the cap gets a coarser quantizer,
//! and one the floor already codes cleanly spends less.

use std::collections::VecDeque;

use bytes::Bytes;
use openh264::OpenH264API;
use openh264::encoder::{
	BitRate, ColorPrimaries, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
	MatrixCoefficients, QpRange, RateControlMode, TransferCharacteristics, UsageType, VuiConfig,
};
use openh264::formats::YUVSlices;
use openh264_sys2::{ENCODER_OPTION_BITRATE, SBitrateInfo, SPATIAL_LAYER_ALL};
#[cfg(test)]
use openh264_sys2::{
	ENCODER_OPTION_COMPLEXITY, ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, LOW_COMPLEXITY, MEDIUM_COMPLEXITY, SEncParamExt,
};

use super::super::encoder::{Applied, Config, Preset};
use super::{Backend, Encoded};
use crate::color::{Matrix, Primaries, Range, Transfer};
use crate::{Error, Frame};

pub(crate) const NAME: &str = "openh264";

/// The finest quantizer the rate control may pick, which is what caps quality
/// and lets a calm scene fall below the bitrate: the coarsest that kept a
/// 720p30 talking head at VMAF 95, the usual mark for transparent. At a 4 Mbit/s
/// cap it scored 95.1 at 2.5 Mbit/s, against 95.7 at the full 4 Mbit/s with no
/// floor; 23 spent 2.0 Mbit/s for 94.7. The default for [`Config::qp_floor`].
const MIN_QP: u8 = 22;

/// The finest quantizer openh264 codes at whatever it is asked for: it raises
/// a lower floor to this (`GOM_MIN_QP_MODE`).
const CODEC_MIN_QP: u8 = 12;

pub(crate) struct Openh264 {
	encoder: Encoder,
	applied: Applied,
	/// openh264 builds the underlying encoder lazily on the first frame and
	/// rejects `SetOption` with `cmInitExpected` until it exists, so a rate set
	/// before then waits here and is applied once there's something to set it on.
	pending: Option<u64>,
	/// Whether a frame has gone through, i.e. whether the encoder exists yet.
	started: bool,
	ready: VecDeque<Encoded>,
}

impl Openh264 {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(config)?))
	}

	fn new(config: &Config) -> Result<Self, Error> {
		let interval = config.keyframe_interval();
		let color = config.resolved_color();
		// State the color space in the SPS so a decoder doesn't fall back to
		// guessing it from the frame height. The SDR curve goes out as BT.709 (1)
		// whatever the matrix, as every backend emits it; see
		// [`Color::code_points`].
		let vui = VuiConfig::new()
			.color_primaries(match color.primaries {
				Primaries::Smpte170m => ColorPrimaries::Smpte170M,
				Primaries::Bt470bg => ColorPrimaries::Bt470BG,
				Primaries::Bt709 => ColorPrimaries::Bt709,
				Primaries::Bt2020 => ColorPrimaries::Bt2020,
			})
			.transfer_characteristics(match color.transfer {
				Transfer::Bt709 => TransferCharacteristics::Bt709,
				Transfer::Pq => TransferCharacteristics::Smpte2084,
				Transfer::Hlg => TransferCharacteristics::Hlg,
			})
			.matrix_coefficients(match color.matrix {
				Matrix::Bt601 => MatrixCoefficients::Smpte170M,
				Matrix::Bt709 => MatrixCoefficients::Bt709,
				Matrix::Bt2020 => MatrixCoefficients::Bt2020Ncl,
			})
			.full_range(color.range == Range::Full);

		// Clamped here rather than left to openh264, which reads a floor of 0 as
		// unset and swaps in its default range, lowering the ceiling to 42 as well.
		let floor = config.qp_floor.unwrap_or(MIN_QP).clamp(CODEC_MIN_QP, 51);

		// Measured at 720p and 1080p (see `examples/encode-presets.rs`): Low saves
		// about 1.5 ms and 15% CPU per frame over Medium for under 0.1 dB, and High
		// codes the same stream as Medium, only slower. So Quality gets Medium and
		// reports itself as Balanced.
		let (complexity, applied) = match config.preset {
			Preset::LowLatency => (
				Complexity::Low,
				Applied::new(Preset::LowLatency, format!("low complexity, QP {floor} floor")),
			),
			Preset::Balanced | Preset::Quality => (
				Complexity::Medium,
				Applied::new(Preset::Balanced, format!("medium complexity, QP {floor} floor")),
			),
		};
		let cfg = EncoderConfig::new()
			.bitrate(BitRate::from_bps(
				config.resolved_bitrate().as_bps().min(u32::MAX as u64) as u32,
			))
			.max_frame_rate(FrameRate::from_hz(config.framerate.as_f64() as f32))
			.rate_control_mode(RateControlMode::Bitrate)
			.qp(QpRange::new(floor, 51))
			// Real-time camera: prioritize latency over compression.
			.usage_type(UsageType::CameraVideoRealTime)
			.intra_frame_period(IntraFramePeriod::from_num_frames(interval))
			.complexity(complexity)
			.vui(vui);

		let encoder = Encoder::with_api_config(OpenH264API::from_source(), cfg)
			.map_err(|e| Error::codec(anyhow::anyhow!("openh264 init: {e}")))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Self {
			encoder,
			applied,
			pending: None,
			started: false,
			ready: VecDeque::new(),
		})
	}

	/// Read the rate back off the live encoder, so a test can tell what the
	/// encoder is actually doing rather than what we think we told it.
	#[cfg(test)]
	fn read_bitrate(&mut self) -> i64 {
		let mut info = SBitrateInfo {
			iLayer: SPATIAL_LAYER_ALL,
			iBitrate: 0,
		};
		let status = unsafe {
			let api = self.encoder.raw_api();
			api.get_option(ENCODER_OPTION_BITRATE, std::ptr::from_mut(&mut info).cast())
		};
		assert_eq!(status, 0, "openh264 get bitrate failed");
		info.iBitrate as i64
	}

	/// Read the complexity mode back off the live encoder, like `read_bitrate`.
	#[cfg(test)]
	fn read_complexity(&mut self) -> i32 {
		let mut complexity = -1i32;
		let status = unsafe {
			let api = self.encoder.raw_api();
			api.get_option(ENCODER_OPTION_COMPLEXITY, std::ptr::from_mut(&mut complexity).cast())
		};
		assert_eq!(status, 0, "openh264 get complexity failed");
		complexity
	}

	/// Read the quantizer range back off the live encoder, like `read_bitrate`.
	#[cfg(test)]
	fn read_qp(&mut self) -> (i32, i32) {
		// SAFETY: a plain C struct of integers, bools, and nested structs of the same.
		let mut params: SEncParamExt = unsafe { std::mem::zeroed() };
		let status = unsafe {
			let api = self.encoder.raw_api();
			api.get_option(
				ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
				std::ptr::from_mut(&mut params).cast(),
			)
		};
		assert_eq!(status, 0, "openh264 get params failed");
		(params.iMinQp, params.iMaxQp)
	}

	/// Set the rate on the live encoder. Only valid once it exists; see `pending`.
	fn apply_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		// The safe wrapper only takes a bitrate at construction, so go through the
		// raw API. Safe to do here: the wrapper re-applies its own cached
		// SEncParamExt (which would clobber this) only when the frame dimensions
		// change, and ours are fixed for the encoder's lifetime.
		let mut info = SBitrateInfo {
			iLayer: SPATIAL_LAYER_ALL,
			iBitrate: bitrate.min(i32::MAX as u64) as i32,
		};

		let status = unsafe {
			let api = self.encoder.raw_api();
			api.set_option(ENCODER_OPTION_BITRATE, std::ptr::from_mut(&mut info).cast())
		};
		if status != 0 {
			return Err(Error::codec(anyhow::anyhow!(
				"openh264 set bitrate to {bitrate}: status {status}"
			)));
		}
		Ok(())
	}
}

impl Backend for Openh264 {
	fn encode(&mut self, frame: &Frame, cut: bool) -> Result<(), Error> {
		// A rate deferred from before the encoder existed lands here, ahead of the
		// frame rather than after it, so a rejected rate can't cost us a frame's
		// packets on the way out.
		if self.started
			&& let Some(bitrate) = self.pending.take()
		{
			self.apply_bitrate(bitrate)?;
		}

		if cut {
			self.encoder.force_intra_frame();
		}

		// Software path: needs CPU I420, downloading a GPU surface if necessary.
		let i420 = frame.surface.to_i420()?;
		let (w, h) = (i420.width as usize, i420.height as usize);
		let yuv = YUVSlices::new((i420.y(), i420.u(), i420.v()), (w, h), (w, w / 2, w / 2));

		let bitstream = self
			.encoder
			.encode(&yuv)
			.map_err(|e| Error::codec(anyhow::anyhow!("openh264 encode: {e}")))?;

		// One Annex-B access unit per frame (low-delay, no B-frames). A skipped
		// frame yields an empty bitstream.
		let bytes = bitstream.to_vec();
		let keyframe = bitstream.frame_type() == FrameType::IDR;

		// The encode above built the underlying encoder, so any pending rate can
		// be set from the next frame on.
		self.started = true;
		// One access unit out per frame in, so it carries that frame's timestamp.
		if !bytes.is_empty() {
			self.ready
				.push_back(Encoded::new(Bytes::from(bytes), frame.timestamp, keyframe));
		}
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		// Low-delay: nothing is buffered, so there's nothing to flush.
		Ok(())
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		// Nothing to set it on yet: defer to the first frame. The contract is only
		// that the rate takes effect from roughly the next frame, and it does.
		if !self.started {
			self.pending = Some(bitrate);
			return Ok(());
		}
		// Drop anything still deferred: it is older than this rate, and would
		// otherwise resurrect on the next encode and clobber it.
		self.pending = None;
		self.apply_bitrate(bitrate)
	}

	fn can_cut(&self) -> bool {
		true
	}

	fn name(&self) -> &'static str {
		NAME
	}

	fn applied(&self) -> Applied {
		self.applied.clone()
	}
}

#[cfg(test)]
mod tests {
	use super::super::super::encoder::Kind;
	use super::*;
	use crate::frame::{I420, Surface};

	fn config() -> Config {
		Config {
			kind: Kind::Software,
			..Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap())
		}
	}

	/// A mid-gray frame at an arbitrary time; these tests only exercise the rate
	/// controls, so the timestamp is never read back.
	fn gray() -> Frame {
		let size = crate::Size::new(320, 240);
		let i420 = I420::new(size, vec![0x80u8; I420::len(size).unwrap()]).unwrap();
		Frame::new(Surface::I420(i420), moq_net::Timestamp::from_micros(0).unwrap())
	}

	/// Each preset reaches the codec as its complexity mode, read back off the
	/// live encoder, and the report names the preset whose controls it got.
	#[test]
	fn a_preset_reaches_the_codec() {
		for (preset, complexity, applied) in [
			(Preset::LowLatency, LOW_COMPLEXITY, Preset::LowLatency),
			(Preset::Balanced, MEDIUM_COMPLEXITY, Preset::Balanced),
			// High complexity codes the same stream as Medium, so Quality reports what it got.
			(Preset::Quality, MEDIUM_COMPLEXITY, Preset::Balanced),
		] {
			let mut enc = Openh264::new(&Config { preset, ..config() }).unwrap();
			enc.encode_all(&gray(), true).unwrap();
			assert_eq!(enc.read_complexity(), complexity, "{preset:?}");
			assert_eq!(enc.applied().preset, Some(applied), "{preset:?}");
		}
	}

	/// The rate reaches the encoder, verified by reading it back rather than by
	/// trusting our own bookkeeping.
	#[test]
	fn set_bitrate_reaches_the_encoder() {
		let mut enc = Openh264::new(&config()).unwrap();
		enc.encode_all(&gray(), true).unwrap();

		let lower = config().resolved_bitrate().as_bps() / 2;
		enc.set_bitrate(lower).unwrap();
		assert_eq!(enc.read_bitrate(), lower as i64);
	}

	/// openh264 rejects a target above the rate it was opened with
	/// (`cmInitParaError`), which is why the rate control policy's ceiling is the
	/// encoder's own opening bitrate. Pinned here so a future policy change that
	/// lets the target climb past it fails loudly rather than at runtime.
	#[test]
	fn set_bitrate_above_the_opening_rate_is_rejected() {
		let mut enc = Openh264::new(&config()).unwrap();
		enc.encode_all(&gray(), true).unwrap();

		let higher = config().resolved_bitrate().as_bps() * 4;
		assert!(enc.set_bitrate(higher).is_err());
	}

	/// The policy's ceiling is exactly the opening rate, so full recovery sets
	/// that value back. It sits one step from the rate openh264 rejects above,
	/// so pin that the boundary itself is allowed.
	#[test]
	fn set_bitrate_at_the_opening_rate_is_accepted() {
		let mut enc = Openh264::new(&config()).unwrap();
		enc.encode_all(&gray(), true).unwrap();
		let opened = config().resolved_bitrate().as_bps();

		enc.set_bitrate(opened / 2).unwrap();
		enc.set_bitrate(opened).unwrap();
		assert_eq!(enc.read_bitrate(), opened as i64);
	}

	/// Regression: a rate set before the first frame is deferred, and a later
	/// live set must supersede it. Leaving the deferred value queued lets it
	/// resurrect on the next encode and silently clobber the newer rate, leaving
	/// the encoder at a bitrate nobody asked for while `Encoder::bitrate()`
	/// reports the newer one.
	#[test]
	fn a_live_set_supersedes_a_deferred_one() {
		let mut enc = Openh264::new(&config()).unwrap();
		let opened = config().resolved_bitrate().as_bps();

		// Deferred: the encoder doesn't exist yet.
		enc.set_bitrate(opened / 2).unwrap();
		enc.encode_all(&gray(), true).unwrap();

		// Live: this is the rate the caller last asked for.
		enc.set_bitrate(opened / 4).unwrap();
		enc.encode_all(&gray(), false).unwrap();

		assert_eq!(enc.read_bitrate(), (opened / 4) as i64, "the deferred rate resurrected");
	}

	/// A calm scene spends well under a generous cap, held there by the quantizer
	/// floor rather than the rate (without it, the same scene spent 1.05 Mbit/s);
	/// a busy one is held to a tight cap on average. Its single seconds overshoot
	/// by a fifth or more, the floor or not: openh264 holds a rate by skipping
	/// frames, in bursts. One opening keyframe, so the seconds measured are rate
	/// control's alone.
	#[test]
	fn quality_under_a_cap() {
		use super::super::test_util::{Scene, spend};

		let at = |bps: u64| Config {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(bps)),
			join: std::time::Duration::from_secs(10),
			..config()
		};
		let calm = spend(&at(4_000_000), Scene::Calm, None).unwrap();
		assert!(calm.mean < 800_000, "a calm scene filled the cap: {calm:?}");

		let busy = spend(&at(500_000), Scene::Busy, None).unwrap();
		assert!(
			(400_000..520_000).contains(&busy.mean),
			"a busy scene missed the cap: {busy:?}"
		);
	}

	/// The configured floor reaches the codec, read back off the live encoder:
	/// unset keeps 22, and a floor below the codec's own 12 lands on 12 without
	/// openh264's default range lowering the ceiling.
	#[test]
	fn the_qp_floor_reaches_the_codec() {
		for (floor, expected) in [(None, 22), (Some(0), 12), (Some(30), 30), (Some(60), 51)] {
			let mut enc = Openh264::new(&Config {
				qp_floor: floor,
				..config()
			})
			.unwrap();
			enc.encode_all(&gray(), true).unwrap();
			assert_eq!(enc.read_qp(), (expected, 51), "{floor:?}");
			assert!(
				enc.applied().controls.contains(&format!("QP {expected} floor")),
				"{floor:?}"
			);
		}
	}

	/// Without the floor, a calm scene buys a finer quantizer with more of a
	/// generous cap; with it, the scene spends less.
	#[test]
	fn lowering_the_qp_floor_spends_more_on_a_calm_scene() {
		use super::super::test_util::spend;

		let at = |qp_floor| Config {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(4_000_000)),
			join: std::time::Duration::from_secs(10),
			qp_floor,
			..config()
		};
		let floored = spend(&at(None), false, None).unwrap();
		let unfloored = spend(&at(Some(0)), false, None).unwrap();
		assert!(
			unfloored.mean > floored.mean * 3 / 2,
			"no floor {unfloored:?} against the default {floored:?}"
		);
	}

	/// The SPS states the color space, so a decoder never falls back to guessing
	/// it from the frame height, and it states the space the pixels were actually
	/// converted into. Read back out of the bitstream, not off our own config.
	#[test]
	fn the_sps_declares_the_color_space() {
		use super::super::test_util::{BT601_DESCRIBED, BT709_DESCRIBED, declared_color};
		use crate::{Color, Size};

		for (size, described) in [
			(Size::new(640, 480), BT601_DESCRIBED),
			(Size::new(1920, 1080), BT709_DESCRIBED),
		] {
			let config = Config {
				kind: Kind::Software,
				..Config::new(size.width, size.height, crate::FrameRate::new(30, 1).unwrap())
			};
			let mut enc = Openh264::new(&config).unwrap();

			// Saturated red: the color the two matrices disagree most about, so a
			// mislabeled stream is a visible bug rather than a rounding difference.
			let rgba = [255u8, 0, 0, 255].repeat(size.pixels() as usize);
			let surface = Surface::rgba(&rgba, size).unwrap();
			let frame = Frame::new(surface, moq_net::Timestamp::from_micros(0).unwrap());

			let encoded = enc.encode_all(&frame, true).unwrap();
			let encoded = encoded.first().expect("a keyframe");
			let annexb = &encoded.payload;

			assert_eq!(declared_color(annexb), Some(described), "{size} SPS color description");

			// The label has to match the pixels: same source of truth on both sides.
			let i420 = frame.surface.to_i420().unwrap();
			assert_eq!(i420.color(), Some(Color::infer(size)), "{size} converted pixels");
		}
	}

	/// Every keyframe is flagged, the GOP cadence's as well as a forced one, and
	/// the flag agrees with the bitstream.
	#[test]
	fn keyframes_are_flagged_on_the_cadence_and_on_a_cut() {
		let config = Config {
			join: std::time::Duration::from_secs(1),
			..config()
		};
		let mut enc = Openh264::new(&config).unwrap();

		let mut keyframes = Vec::new();
		for index in 0..50u64 {
			let frame = Frame {
				timestamp: moq_net::Timestamp::from_micros(index * 33_333).unwrap(),
				..gray()
			};
			for unit in enc.encode_all(&frame, index == 45).unwrap() {
				assert_eq!(
					unit.keyframe,
					super::super::keyframe_annexb(crate::encode::Codec::H264, &unit.payload),
					"frame {index}"
				);
				if unit.keyframe {
					keyframes.push(index);
				}
			}
		}
		assert_eq!(keyframes, vec![0, 30, 45], "the opening, cadence, and forced keyframes");
	}
}
