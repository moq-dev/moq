//! H.264 bridge.
//!
//! str0m hands us reassembled Annex-B frames (start-code prefixed NALs with
//! inline SPS/PPS), which is exactly what
//! [`moq_mux::codec::h264::Import`] in Avc3 mode wants. We just convert the
//! timestamp and stream NALs in.

use bytes::BytesMut;

use crate::{Result, codec};

pub struct Bridge {
	import: moq_mux::codec::h264::Import,
}

impl Bridge {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::hang::Producer) -> Result<Self> {
		let import = moq_mux::codec::h264::Import::new(broadcast, catalog)
			.with_mode(moq_mux::codec::h264::Mode::Avc3)
			.map_err(crate::Error::Other)?;
		Ok(Self { import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_mux::container::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		let mut buf = BytesMut::from(frame.payload.as_ref());
		self.import
			.decode_frame(&mut buf, Some(pts))
			.map_err(crate::Error::Other)?;
		Ok(())
	}
}
