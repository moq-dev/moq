//! H.264 bridge.
//!
//! str0m hands us reassembled Annex-B frames (start-code prefixed NALs with
//! inline SPS/PPS), which is exactly what
//! [`moq_mux::codec::h264::Import`] in Avc3 mode wants. We just convert the
//! timestamp and stream NALs in.

use bytes::BytesMut;

use crate::{Result, codec};

pub struct Bridge {
	split: moq_mux::codec::h264::Split,
	import: moq_mux::publish::Published<moq_mux::codec::h264::Import>,
}

impl Bridge {
	pub fn new(mut broadcast: moq_net::BroadcastProducer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let track = moq_mux::publish::unique_track(&mut broadcast, ".avc3")?;
		let import = moq_mux::codec::h264::Import::from_track(track);
		let import = moq_mux::publish::Published::new(catalog, import);
		let split = moq_mux::codec::h264::Split::new().with_mode(moq_mux::codec::h264::Mode::Avc3);
		Ok(Self { split, import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		let mut buf = BytesMut::from(frame.payload.as_ref());
		let frames = self.split.decode_frame(&mut buf, Some(pts))?;
		self.import.decode(frames)?;
		Ok(())
	}
}
