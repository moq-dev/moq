//! VP8 bridge.
//!
//! VP8 carries no out-of-band config record. str0m hands us complete frames
//! and we forward them to a `.vp8` track with the matching catalog entry.
//! Keyframes are detected from the first byte (P-frame bit, RFC 6386 §9.1).

use crate::{Result, codec};

/// Forwards str0m's VP8 frames to a `.vp8` track, detecting keyframes inline.
pub struct Bridge {
	catalog: moq_mux::catalog::Producer,
	track: moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
	announced: bool,
}

impl Bridge {
	/// Publish a `.vp8` track on `broadcast`; the catalog rendition is added on the first frame.
	pub fn new(mut broadcast: moq_net::broadcast::Producer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let track = broadcast.create_track(broadcast.unique_name(".vp8"), hang::container::track_info())?;
		let producer = catalog.media_producer(track, moq_mux::catalog::hang::Container::Legacy);
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
		let name = self.track.track().name().to_string();
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.container = hang::catalog::Container::Legacy;
		config.timeline = Some(self.catalog.timeline(&name).section());
		self.catalog.lock().video.renditions.insert(name, config);
		self.announced = true;
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		self.announce();
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		// VP8: first byte bit 0 == 0 means keyframe (RFC 6386 §9.1).
		let keyframe = frame.payload.first().map(|b| b & 0x01 == 0).unwrap_or(false);
		self.track
			.write(moq_mux::container::Frame {
				timestamp: pts,
				payload: frame.payload,
				keyframe,
				duration: None,
			})
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("vp8 track write failed: {err}")))?;
		Ok(())
	}

	fn abort(&mut self, err: moq_net::Error) {
		self.track.abort(err);
	}
}

impl Drop for Bridge {
	fn drop(&mut self) {
		self.catalog.lock().video.renditions.remove(self.track.track().name());
	}
}
