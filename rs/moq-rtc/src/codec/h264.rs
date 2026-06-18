//! H.264 bridge.
//!
//! str0m hands us reassembled Annex-B frames (start-code prefixed NALs with
//! inline SPS/PPS), which is exactly what
//! [`moq_mux::codec::h264::Import`] in Avc3 mode wants. We just convert the
//! timestamp and stream NALs in.

use crate::{Result, codec};

pub struct Bridge {
	split: moq_mux::codec::h264::Split,
	import: moq_mux::codec::h264::Import,
}

impl Bridge {
	pub fn new(mut broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let track = moq_mux::import::unique_track(&mut broadcast, ".avc3")?;
		let import = moq_mux::codec::h264::Import::new(track, catalog);
		let split = moq_mux::codec::h264::Split::new();
		Ok(Self { split, import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		// str0m hands over one whole access unit per frame, so flush to emit it.
		let mut frames = self.split.decode(&frame.payload, Some(pts))?;
		frames.extend(self.split.flush(Some(pts))?);
		self.import.decode(frames)?;
		Ok(())
	}
}
