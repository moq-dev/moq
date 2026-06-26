//! Opus bridge.
//!
//! str0m hands us one Opus packet per frame, which is exactly the
//! raw shape that [`moq_mux::codec::opus::Import`] consumes.

use crate::{Result, codec};

/// Feeds str0m's Opus packets into a moq-mux Opus importer.
pub struct Bridge {
	import: moq_mux::codec::opus::Import,
}

impl Bridge {
	/// Publish an `.opus` track on `broadcast` at the negotiated sample rate / channel count.
	pub fn new(
		mut broadcast: moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::Producer,
		sample_rate: u32,
		channel_count: u32,
	) -> Result<Self> {
		let config = moq_mux::codec::opus::Config {
			sample_rate,
			channel_count,
		};
		let track = broadcast.unique_track(".opus")?;
		let import = moq_mux::codec::opus::Import::new_with_track(track, catalog, config)?;
		Ok(Self { import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_mux::container::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		let mut payload = frame.payload;
		self.import.decode(&mut payload, Some(pts))?;
		Ok(())
	}
}
