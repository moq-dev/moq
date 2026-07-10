//! AV1 bridge.
//!
//! str0m hands us complete AV1 temporal units (OBU-framed, with inline sequence
//! headers). This feeds the moq-mux AV1 splitter/importer so catalog config and
//! keyframe detection stay shared with the other ingest paths.

use crate::{Result, codec};

/// Bridges str0m AV1 temporal units into a MoQ AV1 track.
pub struct Bridge {
	split: moq_mux::codec::av1::Split,
	import: moq_mux::codec::av1::Import,
}

impl Bridge {
	/// Publish an `.av1` track on `broadcast`, adding the catalog rendition once config is known.
	pub fn new(mut broadcast: moq_net::broadcast::Producer, catalog: moq_mux::catalog::Producer) -> Result<Self> {
		let track = moq_mux::import::unique_track(&mut broadcast, ".av1")?;
		let import = moq_mux::codec::av1::Import::new(track, catalog.reserve(), Default::default());
		let split = moq_mux::codec::av1::Split::new();
		Ok(Self { split, import })
	}
}

impl codec::Bridge for Bridge {
	fn push(&mut self, frame: codec::Frame) -> Result<()> {
		let pts = moq_net::Timestamp::from_micros(frame.timestamp_us)
			.map_err(|err| crate::Error::Other(anyhow::anyhow!("invalid timestamp: {err}")))?;
		// str0m hands over one whole temporal unit per frame, so flush to emit it.
		let mut frames = self.split.decode(&frame.payload, Some(pts))?;
		frames.extend(self.split.flush(Some(pts))?);
		self.import.decode(frames)?;
		Ok(())
	}

	fn abort(&mut self, err: moq_net::Error) {
		self.import.abort(err);
	}
}
