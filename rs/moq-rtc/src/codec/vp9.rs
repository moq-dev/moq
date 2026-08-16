//! VP9 bridge.
//!
//! str0m hands us complete VP9 frames, which is exactly the raw shape that
//! [`moq_mux::codec::vp9::Import`] consumes. The shared importer parses keyframes
//! so the catalog carries the encoded dimensions and stays in sync if they change.

use crate::{Result, codec};

/// Bridges str0m VP9 frames into a MoQ VP9 track.
pub struct Bridge {
	import: codec::DeferredVideo<moq_mux::codec::vp9::Import>,
}

impl Bridge {
	/// Publish a `.vp9` track on `broadcast`, adding the catalog rendition once config is known.
	pub fn new(broadcast: moq_net::broadcast::Producer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let import = codec::DeferredVideo::new(broadcast, catalog, ".vp9")?;
		Ok(Self { import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		self.import.decode(frame.payload, pts)
	}

	fn abort(self: Box<Self>, err: moq_net::Error) {
		self.import.abort(err);
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use crate::codec::{self, Bridge as _};

	#[test]
	fn keyframe_publishes_catalog_dimensions() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let mut bridge = super::Bridge::new(broadcast, catalog.clone()).unwrap();

		assert!(catalog.snapshot().video.renditions.is_empty());

		// VP9 profile 0 keyframe header for 320x240.
		bridge
			.push(codec::Frame {
				timestamp_us: 0,
				payload: Bytes::from_static(&[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00]),
			})
			.unwrap();

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.values().next().unwrap();
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));
	}
}
