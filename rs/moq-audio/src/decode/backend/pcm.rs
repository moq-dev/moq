//! Uncompressed little-endian `f32` PCM, described entirely by the catalog.

use std::collections::VecDeque;

use super::Backend;
use crate::decode::Decoded;
use crate::{Activity, Error, Layout, pcm};

pub(super) const NAME: &str = "pcm";

pub(super) struct Pcm {
	sample_rate: u32,
	layout: Layout,
	bytes_per_frame: usize,
	/// Outputs submitted but not yet received, which reuse their slots across calls.
	ready: VecDeque<Decoded>,
}

impl Pcm {
	/// Uses the catalog's rate and channel count, and requires an absent `description`.
	pub(super) fn open(catalog: &hang::catalog::AudioConfig) -> Result<Box<dyn Backend>, Error> {
		if catalog.sample_rate == 0 {
			return Err(Error::Unsupported("pcm sample rate must be greater than zero".into()));
		}
		if catalog.channel_count == 0 {
			return Err(Error::Unsupported("pcm channel count must be greater than zero".into()));
		}
		if catalog.description.is_some() {
			return Err(Error::Unsupported("pcm catalog description must be absent".into()));
		}
		let bitrate = pcm::bitrate(catalog.sample_rate, catalog.channel_count)?;
		if catalog.bitrate.is_some_and(|declared| declared != bitrate) {
			return Err(Error::Unsupported(format!(
				"pcm catalog bitrate must be {bitrate} bits per second"
			)));
		}

		Ok(Box::new(Self {
			sample_rate: catalog.sample_rate,
			layout: Layout::from_channels(catalog.channel_count)?,
			bytes_per_frame: pcm::frame_bytes(1, catalog.channel_count)?,
			ready: VecDeque::new(),
		}))
	}
}

impl Backend for Pcm {
	fn decode(&mut self, packet: &[u8]) -> Result<(), Error> {
		if packet.is_empty() || !packet.len().is_multiple_of(self.bytes_per_frame) {
			return Err(Error::Misaligned {
				got: packet.len(),
				expected: packet.len().max(1).next_multiple_of(self.bytes_per_frame),
			});
		}

		let samples = packet
			.as_chunks::<{ pcm::BYTES_PER_SAMPLE }>()
			.0
			.iter()
			.map(|sample| f32::from_le_bytes(*sample))
			.collect();
		self.ready.push_back(Decoded {
			samples,
			activity: Activity::Active,
		});
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Decoded>, Error> {
		Ok(self.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn reset(&mut self) -> Result<(), Error> {
		self.ready.clear();
		Ok(())
	}

	fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	fn layout(&self) -> Layout {
		self.layout
	}

	fn name(&self) -> &str {
		NAME
	}
}
