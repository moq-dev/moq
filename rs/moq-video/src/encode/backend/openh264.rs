//! Software H.264 backend via Cisco's openh264 (vendored, statically linked).
//!
//! The fallback when no hardware encoder is available. Emits Annex-B with
//! in-band SPS/PPS, ready for `moq_mux::codec::h264::Import` in avc3 mode.

use bytes::Bytes;
use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::YUVSlices;
use openh264_sys2::{ENCODER_OPTION_BITRATE, SBitrateInfo, SPATIAL_LAYER_ALL};

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::Surface;

pub(crate) const NAME: &str = "openh264";

pub(crate) struct Openh264 {
	encoder: Encoder,
	/// openh264 builds the underlying encoder lazily on the first frame and
	/// rejects `SetOption` with `cmInitExpected` until it exists, so a rate set
	/// before then waits here and is applied once there's something to set it on.
	pending: Option<u64>,
	/// Whether a frame has gone through, i.e. whether the encoder exists yet.
	started: bool,
}

impl Openh264 {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(config)?))
	}

	fn new(config: &Config) -> Result<Self, Error> {
		let cfg = EncoderConfig::new()
			.bitrate(BitRate::from_bps(config.resolved_bitrate().min(u32::MAX as u64) as u32))
			.max_frame_rate(FrameRate::from_hz(config.framerate as f32))
			.rate_control_mode(RateControlMode::Bitrate)
			// Real-time camera: prioritize latency over compression.
			.usage_type(UsageType::CameraVideoRealTime)
			.intra_frame_period(IntraFramePeriod::from_num_frames(config.gop));

		let encoder = Encoder::with_api_config(OpenH264API::from_source(), cfg)
			.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 init: {e}")))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Self {
			encoder,
			pending: None,
			started: false,
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
			return Err(Error::Codec(anyhow::anyhow!(
				"openh264 set bitrate to {bitrate}: status {status}"
			)));
		}
		Ok(())
	}
}

impl Backend for Openh264 {
	fn encode(&mut self, frame: &Surface, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		// A rate deferred from before the encoder existed lands here, ahead of the
		// frame rather than after it, so a rejected rate can't cost us a frame's
		// packets on the way out.
		if self.started
			&& let Some(bitrate) = self.pending.take()
		{
			self.apply_bitrate(bitrate)?;
		}

		if keyframe {
			self.encoder.force_intra_frame();
		}

		// Software path: needs CPU I420, downloading a GPU surface if necessary.
		let i420 = frame.to_i420()?;
		let (w, h) = (i420.width as usize, i420.height as usize);
		let yuv = YUVSlices::new((i420.y(), i420.u(), i420.v()), (w, h), (w, w / 2, w / 2));

		let bitstream = self
			.encoder
			.encode(&yuv)
			.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 encode: {e}")))?;

		// One Annex-B access unit per frame (low-delay, no B-frames). A skipped
		// frame yields an empty bitstream.
		let bytes = bitstream.to_vec();

		// The encode above built the underlying encoder, so any pending rate can
		// be set from the next frame on.
		self.started = true;
		Ok(if bytes.is_empty() {
			Vec::new()
		} else {
			vec![Bytes::from(bytes)]
		})
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		// Low-delay: nothing is buffered, so there's nothing to flush.
		Ok(Vec::new())
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

	fn name(&self) -> &str {
		NAME
	}
}

#[cfg(test)]
mod tests {
	use super::super::super::encoder::Kind;
	use super::*;
	use crate::frame::I420;

	fn config() -> Config {
		Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		}
	}

	fn gray() -> Surface {
		Surface::I420(I420 {
			width: 320,
			height: 240,
			data: vec![0x80u8; I420::len(320, 240)],
		})
	}

	/// The rate reaches the encoder, verified by reading it back rather than by
	/// trusting our own bookkeeping.
	#[test]
	fn set_bitrate_reaches_the_encoder() {
		let mut enc = Openh264::new(&config()).unwrap();
		enc.encode(&gray(), true).unwrap();

		let lower = config().resolved_bitrate() / 2;
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
		enc.encode(&gray(), true).unwrap();

		let higher = config().resolved_bitrate() * 4;
		assert!(enc.set_bitrate(higher).is_err());
	}

	/// The policy's ceiling is exactly the opening rate, so full recovery sets
	/// that value back. It sits one step from the rate openh264 rejects above,
	/// so pin that the boundary itself is allowed.
	#[test]
	fn set_bitrate_at_the_opening_rate_is_accepted() {
		let mut enc = Openh264::new(&config()).unwrap();
		enc.encode(&gray(), true).unwrap();
		let opened = config().resolved_bitrate();

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
		let opened = config().resolved_bitrate();

		// Deferred: the encoder doesn't exist yet.
		enc.set_bitrate(opened / 2).unwrap();
		enc.encode(&gray(), true).unwrap();

		// Live: this is the rate the caller last asked for.
		enc.set_bitrate(opened / 4).unwrap();
		enc.encode(&gray(), false).unwrap();

		assert_eq!(enc.read_bitrate(), (opened / 4) as i64, "the deferred rate resurrected");
	}
}
