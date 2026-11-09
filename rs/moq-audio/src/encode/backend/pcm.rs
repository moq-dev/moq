//! Uncompressed little-endian `f32` PCM, which needs no codec at all.

use std::collections::VecDeque;

use super::Backend;
use crate::encode::{Config, Encoded};
use crate::{Error, Layout, pcm};

pub(super) const NAME: &str = "pcm";

pub(super) struct Pcm {
	bitrate: u64,
	/// Packets submitted but not yet received, which reuse their slots across calls.
	ready: VecDeque<Encoded>,
}

impl Pcm {
	/// The catalog carries only a channel count, so a named layout that count
	/// would decode as another one is refused rather than relabeled.
	pub(super) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let layout = config.layout;
		if !matches!(layout, Layout::Discrete(_)) && Layout::from_channels(layout.channels())? != layout {
			return Err(Error::Unsupported(format!(
				"pcm carries only a channel count, which decodes as {:?}, not {layout:?}",
				Layout::from_channels(layout.channels())?
			)));
		}
		let bitrate = pcm::bitrate(config.sample_rate, config.layout.channels())?;
		Ok(Box::new(Self {
			bitrate,
			ready: VecDeque::new(),
		}))
	}
}

impl Backend for Pcm {
	fn encode(&mut self, pcm: &[f32]) -> Result<(), Error> {
		let mut payload = Vec::with_capacity(std::mem::size_of_val(pcm));
		for sample in pcm {
			payload.extend_from_slice(&sample.to_le_bytes());
		}
		self.ready.push_back(Encoded::new(payload.into()));
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn reset(&mut self) {
		self.ready.clear();
	}

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
