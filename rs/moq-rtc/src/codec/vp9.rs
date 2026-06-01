//! VP9 bridge.
//!
//! Keyframes are detected from the frame_type bit (RFC 8741 §3 / VP9 spec §6.2:
//! the second bit of the uncompressed header).

use crate::{Result, codec};

pub struct Bridge {
	catalog: moq_mux::catalog::hang::Producer,
	track: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
	announced: bool,
}

impl Bridge {
	pub fn new(mut broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::hang::Producer) -> Result<Self> {
		let track = broadcast.create_track(
			moq_net::Track::new(broadcast.unique_name(".vp9")).with_timescale(hang::container::TIMESCALE),
		)?;
		let producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		Ok(Self {
			catalog,
			track: producer,
			announced: false,
		})
	}

	fn announce(&mut self) {
		if self.announced {
			return;
		}
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VP9::default());
		config.container = hang::catalog::Container::Legacy;
		self.catalog
			.lock()
			.video
			.renditions
			.insert(self.track.track().name.clone(), config);
		self.announced = true;
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		self.announce();
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		// VP9 uncompressed header: bit 2 is frame_type (0 = keyframe).
		let keyframe = frame.payload.first().map(|b| (b & 0b0000_0100) == 0).unwrap_or(false);
		self.track
			.write(moq_mux::container::Frame {
				timestamp: pts,
				payload: frame.payload,
				keyframe,
				duration: None,
			})
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("vp9 track write failed: {err}")))?;
		Ok(())
	}
}

impl Drop for Bridge {
	fn drop(&mut self) {
		self.catalog.lock().video.renditions.remove(&self.track.track().name);
	}
}
