//! Opus through libopus 1.3.1, via [`moq_opus`].

use std::collections::VecDeque;

use bytes::Bytes;
use moq_opus::{Application, Signal};

use super::Backend;
use crate::encode::{Config, Encoded, Kind};
use crate::{Error, opus};

pub(super) const NAME: &str = "libopus";

/// libopus packet size ceiling per RFC 6716 §3.4.
const MAX_PACKET_BYTES: usize = 4_000;

pub(super) struct Libopus {
	inner: moq_opus::Encoder,
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
		let (application, signal, dtx) = tuning(config.kind);

		let inner = moq_opus::Encoder::new(sample_rate, opus::validate_channels(channels)?, application)
			.map_err(|err| opus::error(err, "create"))?;
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
		let encoder = &mut backend.inner;
		encoder
			.set_signal(signal)
			.map_err(|err| opus::error(err, "set signal"))?;
		encoder.set_dtx(dtx).map_err(|err| opus::error(err, "set DTX"))?;
		backend.bitrate = encoder.bitrate().map_err(|err| opus::error(err, "get bitrate"))?.into();
		backend.lookahead = encoder.lookahead().map_err(|err| opus::error(err, "get lookahead"))?;

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
		// The range above fits a u32.
		self.inner
			.set_bitrate(bitrate as u32)
			.map_err(|err| opus::error(err, "set bitrate"))
	}
}

/// The application, signal, and discontinuous transmission WebCodecs gets from `@moq/publish`
/// for `kind`. libopus has no default application, so an unknown kind takes the general-purpose
/// one, and only voice turns on discontinuous transmission.
fn tuning(kind: Option<Kind>) -> (Application, Signal, bool) {
	match kind {
		Some(Kind::Voice) => (Application::Voip, Signal::Voice, true),
		Some(Kind::Music) => (Application::Audio, Signal::Music, false),
		None => (Application::Audio, Signal::Auto, false),
	}
}

impl Backend for Libopus {
	fn encode(&mut self, pcm: &[f32]) -> Result<(), Error> {
		// The front end sized `pcm` to one frame.
		let n = self
			.inner
			.encode_float(pcm, &mut self.scratch)
			.map_err(|err| opus::error(err, "encode"))?;
		let payload = Bytes::copy_from_slice(&self.scratch[..n]);
		let activity = opus::activity(&payload, false);
		// Asked after the frame, so it describes this packet: its keep-alives code a frame while
		// the encoder stays in discontinuous transmission.
		let in_dtx = self.dtx && self.inner.in_dtx().map_err(|err| opus::error(err, "get DTX state"))?;
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
		let reset = self.inner.reset();
		debug_assert_eq!(reset, Ok(()), "reset failed");
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

#[cfg(test)]
mod tests {
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
		assert_eq!(backend.inner.bitrate().unwrap(), 32_000);
	}

	/// Each kind opens with the application, signal, and transmission `@moq/publish` asks
	/// WebCodecs for.
	#[test]
	fn kind_tunes_libopus() {
		for kind in [Some(Kind::Voice), Some(Kind::Music), None] {
			Libopus::new(&Config {
				kind,
				..Config::default()
			})
			.unwrap();
		}
		assert_eq!(tuning(Some(Kind::Voice)), (Application::Voip, Signal::Voice, true));
		assert_eq!(tuning(Some(Kind::Music)), (Application::Audio, Signal::Music, false));
		assert_eq!(tuning(None), (Application::Audio, Signal::Auto, false));
	}
}
