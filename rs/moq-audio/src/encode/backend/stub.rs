//! A stand-in AAC encoder, so the AAC front end is testable on a host with no
//! platform encoder. Selectable only by name, and only in tests.

use bytes::Bytes;

use super::Backend;
use crate::Error;
use crate::encode::{Encoded, Settings};

pub(crate) const NAME: &str = "stub";

/// The AudioToolbox AAC-LC encoder delay, which is what a real backend reports.
pub(crate) const DELAY: usize = 2112;

/// Emits each frame's index as its payload, and cannot retune.
pub(crate) struct Stub {
	bitrate: u64,
	frames: u64,
}

impl Stub {
	pub(super) fn open(settings: &Settings) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self {
			bitrate: settings.bitrate.map_or(128_000, |rate| rate.as_bps()),
			frames: 0,
		}))
	}
}

impl Backend for Stub {
	fn encode(&mut self, _pcm: &[f32]) -> Result<Encoded, Error> {
		let payload = Bytes::copy_from_slice(&self.frames.to_be_bytes());
		self.frames += 1;
		Ok(Encoded::new(payload))
	}

	fn reset(&mut self) {
		self.frames = 0;
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		match bitrate == self.bitrate {
			true => Ok(()),
			false => Err(Error::Unsupported("the stub cannot change rate mid-stream".into())),
		}
	}

	fn bitrate(&self) -> u64 {
		self.bitrate
	}

	fn delay(&self) -> usize {
		DELAY
	}

	fn name(&self) -> &str {
		NAME
	}
}
