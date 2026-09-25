//! Uncompressed little-endian `f32` PCM, which needs no codec at all.

use super::Backend;
use crate::encode::{Encoded, Settings};
use crate::{Error, pcm};

pub(super) const NAME: &str = "pcm";

pub(super) struct Pcm {
	bitrate: u64,
}

impl Pcm {
	pub(super) fn open(settings: &Settings) -> Result<Box<dyn Backend>, Error> {
		let bitrate = pcm::bitrate(settings.sample_rate, settings.layout.channels())?;
		Ok(Box::new(Self { bitrate }))
	}
}

impl Backend for Pcm {
	fn encode(&mut self, pcm: &[f32]) -> Result<Encoded, Error> {
		let mut payload = Vec::with_capacity(std::mem::size_of_val(pcm));
		for sample in pcm {
			payload.extend_from_slice(&sample.to_le_bytes());
		}
		Ok(Encoded::new(payload.into()))
	}

	fn reset(&mut self) {}

	fn set_bitrate(&mut self, _bitrate: u64) -> Result<(), Error> {
		Err(Error::Unsupported("pcm bitrate is fixed".into()))
	}

	fn bitrate(&self) -> u64 {
		self.bitrate
	}

	fn delay(&self) -> usize {
		0
	}

	fn name(&self) -> &str {
		NAME
	}
}
