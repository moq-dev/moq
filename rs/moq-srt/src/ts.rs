//! The seam between an SRT byte stream and the MoQ origin.
//!
//! SRT carries MPEG-TS, so ingest is the same three steps every time: create a
//! broadcast, publish it into the origin so downstream subscribers can find it,
//! and feed the incoming bytes through a [`moq_mux`] TS importer that demuxes
//! them into MoQ tracks. [`Publisher`] packages that up.

use bytes::Bytes;
use moq_mux::container::ts;
use moq_net::{BroadcastInfo, OriginProducer, OriginPublish};

use crate::Result;

/// Publishes an MPEG-TS source into the origin as a single broadcast.
///
/// Each chunk is handed straight to the TS importer, which consumes whole
/// transport packets and retains any partial trailing packet internally for the
/// next call (the same pattern `moq-cli publish ts` uses against stdin).
/// Dropping the publisher ends the broadcast: the held [`OriginPublish`] guard
/// unannounces it from the origin.
pub struct Publisher {
	/// Held to keep the broadcast announced for the publisher's lifetime; the
	/// importer owns its own clone of the broadcast for writing frames.
	_publish: OriginPublish,
	importer: ts::Import,
}

impl Publisher {
	/// Create the broadcast, wire up the TS importer + catalog, and announce it
	/// into `origin` at `path`.
	pub fn new(origin: &OriginProducer, path: &str) -> Result<Self> {
		let mut broadcast = BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let importer = ts::Import::new(broadcast.clone(), catalog);

		let publish = origin.publish_broadcast(path, broadcast.consume())?;
		tracing::info!(%path, "publishing ingest broadcast");

		Ok(Self {
			_publish: publish,
			importer,
		})
	}

	/// Feed a chunk of MPEG-TS bytes (one SRT payload) into the importer.
	///
	/// `decode` drains `data` fully, buffering any partial trailing packet in
	/// its own internal scratch, so there's nothing to retain here.
	pub fn feed(&mut self, data: Bytes) -> Result<()> {
		Ok(self.importer.decode(&data)?)
	}

	/// Flush any buffered media and close out the broadcast's open groups.
	pub fn finish(&mut self) -> Result<()> {
		Ok(self.importer.finish()?)
	}
}
