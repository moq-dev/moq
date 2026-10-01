//! Opus through libopus 1.3.1, via [`unsafe_libopus`].

use bytes::Bytes;
use unsafe_libopus::{
	OPUS_APPLICATION_AUDIO, OPUS_GET_BITRATE_REQUEST, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_RESET_STATE,
	OPUS_SET_BITRATE_REQUEST, OPUS_SET_DTX_REQUEST, OpusEncoder, opus_encode_float, opus_encoder_create,
	opus_encoder_ctl_impl, opus_encoder_destroy, varargs,
};

use super::Backend;
use crate::encode::{Encoded, Settings};
use crate::{Error, opus};

pub(super) const NAME: &str = "libopus";

/// libopus packet size ceiling per RFC 6716 §3.4.
const MAX_PACKET_BYTES: usize = 4_000;

pub(super) struct Libopus {
	inner: *mut OpusEncoder,
	scratch: Vec<u8>,
	sample_rate: u32,
	channels: u32,
	frame_size: usize,
	bitrate: u64,
	lookahead: usize,
}

// SAFETY: OpusEncoder is heap-allocated state owned exclusively by this
// struct; libopus encoder methods take a single &mut, so a unique owner is
// allowed to move it across threads.
unsafe impl Send for Libopus {}

impl Libopus {
	/// Opens at the settings' rate and layout, which the front end has already
	/// checked against what Opus codes.
	pub(super) fn open(settings: &Settings) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(settings)?))
	}

	fn new(settings: &Settings) -> Result<Self, Error> {
		let sample_rate = settings.sample_rate;
		let channels = settings.layout.channels();
		let frame_size = opus::frame_size(sample_rate, settings.frame_duration)?;

		let mut err = 0i32;
		// SAFETY: out-pointer `err` is valid; inner is checked for null below.
		let inner = unsafe {
			opus_encoder_create(
				sample_rate as i32,
				opus::validate_channels(channels)?,
				OPUS_APPLICATION_AUDIO,
				&mut err,
			)
		};
		if err != OPUS_OK || inner.is_null() {
			return Err(opus::error(err, "opus_encoder_create"));
		}

		// Owned from here, so an early return below destroys it.
		let mut backend = Self {
			inner,
			scratch: vec![0u8; MAX_PACKET_BYTES],
			sample_rate,
			channels,
			frame_size,
			bitrate: 0,
			lookahead: 0,
		};

		if let Some(bitrate) = settings.bitrate {
			backend.set_rate(bitrate.as_bps())?;
		}
		backend.set_ctl(OPUS_SET_DTX_REQUEST, i32::from(settings.dtx), "OPUS_SET_DTX")?;

		let bitrate = backend.get_ctl(OPUS_GET_BITRATE_REQUEST, "OPUS_GET_BITRATE")?;
		backend.bitrate = u64::try_from(bitrate)
			.map_err(|_| Error::Unsupported(format!("Opus reported negative bitrate {bitrate}")))?;
		let lookahead = backend.get_ctl(OPUS_GET_LOOKAHEAD_REQUEST, "OPUS_GET_LOOKAHEAD")?;
		backend.lookahead = usize::try_from(lookahead)
			.map_err(|_| Error::Unsupported(format!("Opus reported negative lookahead {lookahead}")))?;

		Ok(backend)
	}

	/// Refuse rates libopus would silently clamp, then apply the rest.
	fn set_rate(&mut self, bitrate: u64) -> Result<(), Error> {
		let (channels, frame_size) = (self.channels, self.frame_size);
		let max = 300_000 * channels as u64;
		let min = opus::bitrate_floor(self.sample_rate, frame_size).max(500);
		if !(min..=max).contains(&bitrate) {
			return Err(Error::Unsupported(format!(
				"Opus bitrate must be between {min} and {max} bits per second for {channels} channel(s) at {frame_size} samples, got {bitrate}"
			)));
		}
		self.set_ctl(OPUS_SET_BITRATE_REQUEST, bitrate as i32, "OPUS_SET_BITRATE")
	}

	fn set_ctl(&mut self, request: i32, value: i32, name: &'static str) -> Result<(), Error> {
		// SAFETY: `inner` owns a live encoder and each request here expects one i32.
		let rc = unsafe { opus_encoder_ctl_impl(self.inner, request, varargs![value]) };
		if rc != OPUS_OK {
			return Err(opus::error(rc, name));
		}
		Ok(())
	}

	fn get_ctl(&self, request: i32, name: &'static str) -> Result<i32, Error> {
		let mut value = 0;
		// SAFETY: `inner` owns a live encoder and each request here expects one
		// valid mutable i32 output.
		let rc = unsafe { opus_encoder_ctl_impl(self.inner, request, varargs![&mut value]) };
		if rc != OPUS_OK {
			return Err(opus::error(rc, name));
		}
		Ok(value)
	}
}

impl Backend for Libopus {
	fn encode(&mut self, pcm: &[f32]) -> Result<Encoded, Error> {
		// SAFETY: `inner` owns a live OpusEncoder; pcm and scratch slices are
		// bounded by the lengths we pass, and the front end sized `pcm` to one frame.
		let n = unsafe {
			opus_encode_float(
				self.inner,
				pcm.as_ptr(),
				self.frame_size as i32,
				self.scratch.as_mut_ptr(),
				self.scratch.len() as i32,
			)
		};
		if n < 0 {
			return Err(opus::error(n, "opus_encode_float"));
		}
		let payload = Bytes::copy_from_slice(&self.scratch[..n as usize]);
		let activity = opus::activity(&payload, false);
		Ok(Encoded { payload, activity })
	}

	fn reset(&mut self) {
		// SAFETY: `inner` owns a live encoder and OPUS_RESET_STATE takes no arguments.
		let rc = unsafe { opus_encoder_ctl_impl(self.inner, OPUS_RESET_STATE, varargs![]) };
		debug_assert_eq!(rc, OPUS_OK, "OPUS_RESET_STATE failed with {rc}");
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		if bitrate != self.bitrate {
			self.set_rate(bitrate)?;
			self.bitrate = bitrate;
		}
		Ok(())
	}

	fn bitrate(&self) -> u64 {
		self.bitrate
	}

	fn delay(&self) -> usize {
		self.lookahead
	}

	fn name(&self) -> &str {
		NAME
	}
}

impl Drop for Libopus {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusEncoder that nothing else aliases.
		unsafe { opus_encoder_destroy(self.inner) };
	}
}

#[cfg(test)]
mod tests {
	use unsafe_libopus::OPUS_GET_DTX_REQUEST;

	use super::*;

	#[test]
	fn runtime_bitrate_reaches_libopus() {
		let mut backend = Libopus::new(&Settings {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(64_000)),
			..Settings::default()
		})
		.unwrap();

		backend.set_bitrate(32_000).unwrap();
		assert_eq!(backend.bitrate(), 32_000);
		assert_eq!(
			backend.get_ctl(OPUS_GET_BITRATE_REQUEST, "OPUS_GET_BITRATE").unwrap(),
			32_000
		);
	}

	#[test]
	fn applies_dtx_control() {
		let backend = Libopus::new(&Settings {
			dtx: true,
			..Settings::default()
		})
		.unwrap();

		assert_eq!(backend.get_ctl(OPUS_GET_DTX_REQUEST, "OPUS_GET_DTX").unwrap(), 1);
	}
}
