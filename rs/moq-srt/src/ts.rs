//! The seam between an SRT byte stream and the MoQ origin.
//!
//! SRT carries MPEG-TS, so ingest is the same three steps every time: create a
//! broadcast, publish it into the origin so downstream subscribers can find it,
//! and feed the incoming bytes through a [`moq_mux`] TS importer that demuxes
//! them into MoQ tracks. [`Publisher`] packages that up. [`Subscriber`] is the
//! mirror image for egress: it consumes a broadcast from the origin and re-muxes
//! it back to MPEG-TS for an SRT caller (VLC, ffmpeg) to play.

use bytes::Bytes;
use moq_mux::container::{Frame, ts};
use moq_net::{BroadcastInfo, OriginConsumer, OriginProducer, OriginPublish};

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

/// Muxes a single MoQ broadcast back into an MPEG-TS byte stream for egress.
///
/// The mirror of [`Publisher`]: where that demuxes SRT-carried TS into the
/// origin, this consumes a broadcast from the origin and re-muxes it to TS so an
/// SRT caller can play it. Pull frames with [`next`](Self::next); each carries
/// the TS bytes plus the media timestamp used to pace delivery.
pub struct Subscriber {
	export: ts::Export,
}

impl Subscriber {
	/// Resolve the broadcast at `path` in the origin and prepare to mux it to TS.
	///
	/// Returns `Ok(None)` if the broadcast can never be served (path outside the
	/// consumer's scope, or the origin closed). Otherwise waits for the broadcast
	/// to be announced, so a caller may connect before the publisher does.
	pub async fn new(origin: &OriginConsumer, path: &str) -> Result<Option<Self>> {
		let Some(broadcast) = origin.announced_broadcast(path).await else {
			return Ok(None);
		};

		let export = ts::Export::new(broadcast).await?;
		Ok(Some(Self { export }))
	}

	/// Pull the next muxed frame (TS bytes + media timestamp), or `None` once the
	/// broadcast ends.
	pub async fn next(&mut self) -> Result<Option<Frame>> {
		Ok(self.export.next().await?)
	}
}
