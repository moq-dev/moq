//! H.264 bridge.
//!
//! str0m hands us reassembled Annex-B frames (start-code prefixed NALs with
//! inline SPS/PPS), which is exactly what
//! [`moq_mux::codec::h264::Import`] in Avc3 mode wants. We just convert the
//! timestamp and stream NALs in.

use crate::{Result, codec};

/// Feeds str0m's Annex-B H.264 access units into a moq-mux avc3 importer.
pub struct Bridge {
	import: moq_mux::codec::h264::Import<moq_mux::catalog::hang::Extra>,
}

impl Bridge {
	/// Publish an `.avc3` track on `broadcast`, registering it in `catalog`.
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		// Pin avc3 (Annex-B, inline SPS/PPS) up front: str0m always hands us that wire shape.
		let import =
			moq_mux::codec::h264::Import::new(broadcast, catalog).with_mode(moq_mux::codec::h264::Mode::Avc3)?;
		Ok(Self { import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_mux::container::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		// str0m hands over one whole access unit per frame.
		let mut payload = frame.payload;
		self.import.decode_frame(&mut payload, Some(pts))?;
		Ok(())
	}
}
