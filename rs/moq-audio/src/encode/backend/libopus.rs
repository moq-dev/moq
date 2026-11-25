//! Opus through libopus 1.3.1, via [`moq_opus`].

use std::collections::VecDeque;

use bytes::Bytes;
use moq_opus::{
	OPUS_APPLICATION_AUDIO, OPUS_APPLICATION_VOIP, OPUS_GET_BITRATE_REQUEST, OPUS_GET_IN_DTX_REQUEST,
	OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_RESET_STATE, OPUS_SET_BITRATE_REQUEST, OPUS_SET_DTX_REQUEST,
	OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_MUSIC, OPUS_SIGNAL_VOICE, OpusEncoder, opus_encode_float, opus_encoder_create,
	opus_encoder_ctl_impl, opus_encoder_destroy, varargs,
};

use super::Backend;
use crate::encode::{Config, Encoded, Kind};
use crate::{Error, opus};

pub(super) const NAME: &str = "libopus";

/// libopus packet size ceiling per RFC 6716 §3.4.
const MAX_PACKET_BYTES: usize = 4_000;

pub(super) struct Libopus {
	inner: *mut OpusEncoder,
	scratch: Vec<u8>,
	/// Packets submitted but not yet received, which reuse their slots across calls.
	ready: VecDeque<Encoded>,
	sample_rate: u32,
	channels: u32,
	frame_size: usize,
	bitrate: u64,
	lookahead: usize,
	/// Whether discontinuous transmission is on, which only voice turns on.
	dtx: bool,
}

// SAFETY: OpusEncoder is heap-allocated state owned exclusively by this
// struct; libopus encoder methods take a single &mut, so a unique owner is
// allowed to move it across threads.
unsafe impl Send for Libopus {}

impl Libopus {
	/// Opens at the config's rate and layout, which the front end has already
	/// checked against what Opus codes.
	pub(super) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(config)?))
	}

	fn new(config: &Config) -> Result<Self, Error> {
		let sample_rate = config.sample_rate;
		let channels = config.layout.channels();
		let frame_size = opus::frame_size(sample_rate, config.frame_duration)?;
		// The same application and signal WebCodecs gets from `@moq/publish` for each kind. libopus
		// has no default application, so an unknown kind takes the general-purpose one.
		let (application, signal) = match config.kind {
			Some(Kind::Voice) => (OPUS_APPLICATION_VOIP, Some(OPUS_SIGNAL_VOICE)),
			Some(Kind::Music) => (OPUS_APPLICATION_AUDIO, Some(OPUS_SIGNAL_MUSIC)),
			None => (OPUS_APPLICATION_AUDIO, None),
		};
		let dtx = config.kind == Some(Kind::Voice);

		let mut err = 0i32;
		// SAFETY: out-pointer `err` is valid; inner is checked for null below.
		let inner = unsafe {
			opus_encoder_create(
				sample_rate as i32,
				opus::validate_channels(channels)?,
				application,
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
			ready: VecDeque::new(),
			sample_rate,
			channels,
			frame_size,
			bitrate: 0,
			lookahead: 0,
			dtx,
		};

		if let Some(bitrate) = config.bitrate {
			backend.set_rate(bitrate.as_bps())?;
		}
		if let Some(signal) = signal {
			backend.set_ctl(OPUS_SET_SIGNAL_REQUEST, signal, "OPUS_SET_SIGNAL")?;
		}
		backend.set_ctl(OPUS_SET_DTX_REQUEST, i32::from(dtx), "OPUS_SET_DTX")?;

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
	fn encode(&mut self, pcm: &[f32]) -> Result<(), Error> {
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
		// Asked after the frame, so it describes this packet: its keep-alives code a frame while
		// the encoder stays in discontinuous transmission.
		let in_dtx = self.dtx && self.get_ctl(OPUS_GET_IN_DTX_REQUEST, "OPUS_GET_IN_DTX")? != 0;
		self.ready.push_back(Encoded {
			payload,
			activity,
			in_dtx,
		});
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.ready.pop_front())
	}

	/// libopus readies each frame's packet as soon as it is submitted.
	fn flush(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn reset(&mut self) {
		// SAFETY: `inner` owns a live encoder and OPUS_RESET_STATE takes no arguments.
		let rc = unsafe { opus_encoder_ctl_impl(self.inner, OPUS_RESET_STATE, varargs![]) };
		debug_assert_eq!(rc, OPUS_OK, "OPUS_RESET_STATE failed with {rc}");
		self.ready.clear();
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
	use moq_opus::{OPUS_AUTO, OPUS_GET_APPLICATION_REQUEST, OPUS_GET_DTX_REQUEST, OPUS_GET_SIGNAL_REQUEST};

	use super::*;

	#[test]
	fn runtime_bitrate_reaches_libopus() {
		let mut backend = Libopus::new(&Config {
			bitrate: Some(moq_net::bandwidth::Rate::from_bps(64_000)),
			..Config::default()
		})
		.unwrap();

		backend.set_bitrate(32_000).unwrap();
		assert_eq!(backend.bitrate(), 32_000);
		assert_eq!(
			backend.get_ctl(OPUS_GET_BITRATE_REQUEST, "OPUS_GET_BITRATE").unwrap(),
			32_000
		);
	}

	/// Each kind reaches libopus as the application, signal, and transmission `@moq/publish` asks
	/// WebCodecs for.
	#[test]
	fn kind_tunes_libopus() {
		let cases = [
			(Some(Kind::Voice), OPUS_APPLICATION_VOIP, OPUS_SIGNAL_VOICE, 1),
			(Some(Kind::Music), OPUS_APPLICATION_AUDIO, OPUS_SIGNAL_MUSIC, 0),
			(None, OPUS_APPLICATION_AUDIO, OPUS_AUTO, 0),
		];
		for (kind, application, signal, dtx) in cases {
			let backend = Libopus::new(&Config {
				kind,
				..Config::default()
			})
			.unwrap();

			let get = |request, name| backend.get_ctl(request, name).unwrap();
			assert_eq!(
				get(OPUS_GET_APPLICATION_REQUEST, "OPUS_GET_APPLICATION"),
				application,
				"{kind:?}"
			);
			assert_eq!(get(OPUS_GET_SIGNAL_REQUEST, "OPUS_GET_SIGNAL"), signal, "{kind:?}");
			assert_eq!(get(OPUS_GET_DTX_REQUEST, "OPUS_GET_DTX"), dtx, "{kind:?}");
		}
	}
}
