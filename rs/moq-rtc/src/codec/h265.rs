//! H.265 bridge.
//!
//! str0m hands us reassembled Annex-B frames (start-code prefixed NALs with
//! inline VPS/SPS/PPS), which is the `hev1` shape
//! [`moq_mux::codec::h265::Import`] wants. We convert timestamps and stream
//! NALs through the shared splitter/importer.

use crate::{Result, codec};

/// Bridges str0m H.265 Annex-B access units into a MoQ H.265 track.
pub struct Bridge {
	split: moq_mux::codec::h265::Split,
	import: moq_mux::codec::h265::Import,
}

impl Bridge {
	/// Publish a `.hev1` track on `broadcast`, adding the catalog rendition once config is known.
	pub fn new(mut broadcast: moq_net::broadcast::Producer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let track = moq_mux::import::unique_track(&mut broadcast, ".hev1")?;
		let import = moq_mux::codec::h265::Import::new(track, catalog.reserve(), Default::default());
		let split = moq_mux::codec::h265::Split::new();
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

	fn abort(&mut self, err: moq_net::Error) {
		self.import.abort(err);
	}
}
