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
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us).map_err(moq_mux::Error::from)?;
		self.import.decode(frame.payload, pts)
	}

	fn finish(&mut self) -> Result<()> {
		self.import.finish()
	}

	fn abort(self: Box<Self>, err: moq_net::Error) {
		self.import.abort(err);
	}
}
