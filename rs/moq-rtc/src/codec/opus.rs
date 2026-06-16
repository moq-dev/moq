//! Opus bridge.
//!
//! str0m hands us one Opus packet per frame, which is exactly the
//! raw shape that [`moq_mux::codec::opus::Import`] consumes.

use crate::{Result, codec};

pub struct Bridge {
	import: moq_mux::publish::Published<moq_mux::codec::opus::Import>,
}

impl Bridge {
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
		let track = moq_mux::publish::unique_track(&mut broadcast, ".opus")?;
		let import = moq_mux::codec::opus::Import::from_track(track, config)?;
		let import = moq_mux::publish::Published::new(catalog, import);
		Ok(Self { import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		let mut payload = frame.payload;
		self.import.decode_buf(&mut payload, Some(pts))?;
		Ok(())
	}
}
