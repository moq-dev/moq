//! VP8 bridge.
//!
//! str0m hands us complete VP8 frames, which is exactly the raw shape that
//! [`moq_mux::codec::vp8::Import`] consumes. The shared importer parses keyframes
//! so the catalog carries the encoded dimensions and stays in sync if they change.

use crate::{Result, codec};

/// Bridges str0m VP8 frames into a MoQ VP8 track.
pub struct Bridge {
	import: codec::DeferredVideo<moq_mux::codec::vp8::Import>,
}

impl Bridge {
	/// Publish a `.vp8` track on `broadcast`, adding the catalog rendition once config is known.
	pub fn new(broadcast: moq_net::broadcast::Producer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let import = codec::DeferredVideo::new(broadcast, catalog, ".vp8")?;
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

		// VP8 keyframe header for 320x240.
		bridge
			.push(codec::Frame {
				timestamp_us: 0,
				payload: Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]),
			})
			.unwrap();

		let snapshot = catalog.snapshot();
		let config = snapshot.video.renditions.values().next().unwrap();
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));
	}

	#[tokio::test]
	async fn importer_creation_failure_preserves_abort_error() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let _collision = broadcast.create_track(hang::timeline::DEFAULT_NAME, None).unwrap();
		let consumer = broadcast.consume();
		let mut bridge = super::Bridge::new(broadcast, catalog).unwrap();
		let mut track = consumer.track("0.vp8").unwrap().subscribe(None).await.unwrap();

		let result = bridge.push(codec::Frame {
			timestamp_us: 0,
			payload: Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00]),
		});
		assert!(result.is_err(), "timeline collision must fail importer creation");

		Box::new(bridge).abort(moq_net::Error::Transport("session failed".into()));
		let Err(error) = track.recv_group().await else {
			panic!("aborted track must fail");
		};
		assert!(matches!(
			error,
			moq_net::Error::Transport(message) if message == "session failed"
		));
	}
}
