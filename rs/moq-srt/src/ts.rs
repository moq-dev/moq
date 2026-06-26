//! The seam between an SRT byte stream and the MoQ origin.
//!
//! SRT carries MPEG-TS, so ingest is the same three steps every time: create a
//! broadcast, publish it into the origin so downstream subscribers can find it,
//! and feed the incoming bytes through a [`moq_mux`] TS importer that demuxes
//! them into MoQ tracks. [`Publisher`] packages that up. [`Subscriber`] is the
//! mirror image for egress: it consumes a broadcast from the origin and re-muxes
//! it back to MPEG-TS for an SRT caller (VLC, ffmpeg) to play.

use bytes::Bytes;
use moq_mux::catalog::hang::Extra;
use moq_mux::container::{Frame, ts};
use moq_net::{Broadcast, OriginConsumer, OriginProducer};

use crate::Result;

/// Publishes an MPEG-TS source into the origin as a single broadcast.
///
/// Each chunk is handed straight to the TS importer, which consumes whole
/// transport packets and retains any partial trailing packet internally for the
/// next call (the same pattern `moq-cli publish ts` uses against stdin).
/// Dropping the publisher ends the broadcast: the importer's producer clone
/// closes, which unannounces it from the origin.
pub struct Publisher {
	/// Owns a clone of the broadcast producer, so the broadcast stays announced
	/// (and writable) for the publisher's lifetime.
	importer: ts::Import<Extra>,
}

impl Publisher {
	/// Create the broadcast, wire up the TS importer + catalog, and announce it
	/// into `origin` at `path`.
	pub fn new(origin: &OriginProducer, path: &str) -> Result<Self> {
		let mut broadcast = Broadcast::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let importer = ts::Import::new(broadcast.clone(), catalog);

		// The origin unannounces the path automatically when the broadcast closes,
		// i.e. when this importer (the last producer clone) is dropped.
		if !origin.publish_broadcast(path, broadcast.consume()) {
			return Err(crate::Error::from(anyhow::anyhow!(
				"not allowed to publish broadcast at {path}"
			)));
		}
		tracing::info!(%path, "publishing ingest broadcast");

		Ok(Self { importer })
	}

	/// Feed a chunk of MPEG-TS bytes (one SRT payload) into the importer.
	///
	/// `decode` drains `data` fully, buffering any partial trailing packet in
	/// its own internal scratch, so there's nothing to retain here.
	pub fn feed(&mut self, mut data: Bytes) -> Result<()> {
		Ok(self.importer.decode(&mut data)?)
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

		let export = ts::Export::new(broadcast)?;
		Ok(Some(Self { export }))
	}

	/// Pull the next muxed frame (TS bytes + media timestamp), or `None` once the
	/// broadcast ends.
	pub async fn next(&mut self) -> Result<Option<Frame>> {
		Ok(self.export.next().await?)
	}
}
