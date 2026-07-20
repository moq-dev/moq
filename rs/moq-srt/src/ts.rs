//! The seam between an SRT byte stream and the MoQ origin.
//!
//! SRT carries MPEG-TS, so ingest is the same three steps every time: create a
//! broadcast, publish it into the origin so downstream subscribers can find it,
//! and feed the incoming bytes through a [`moq_mux`] TS importer that demuxes
//! them into MoQ tracks. [`Publisher`] packages that up. [`Subscriber`] is the
//! mirror image for egress: it consumes a broadcast from the origin and re-muxes
//! it back to MPEG-TS for an SRT caller (VLC, ffmpeg) to play.

use std::time::Duration;

use bytes::Bytes;
use moq_mux::container::{Frame, ts};
use moq_net::{broadcast, origin};

use crate::Result;

/// Publishes an MPEG-TS source into the origin as a single broadcast.
///
/// Each chunk is handed straight to the TS importer, which consumes whole
/// transport packets and retains any partial trailing packet internally for the
/// next call (the same pattern `moq-cli import ... stdin ts` uses against stdin).
/// Either [`Self::finish`] or dropping the publisher ends the broadcast and
/// unannounces the path, the former without the dropped-without-finish warning.
pub struct Publisher {
	importer: ts::Import,
	// A clone of the importer's producer, so a deliberate end can finish() the
	// broadcast (prompt unannounce) even though the importer owns it.
	broadcast: moq_net::broadcast::Producer,
}

impl Publisher {
	/// Create the broadcast on `origin` at `path` and wire up the TS importer +
	/// catalog.
	pub fn new(origin: &origin::Producer, path: &str) -> Result<Self> {
		let mut broadcast = origin.create_broadcast(path, broadcast::Route::new().with_announce(true))?;
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let handle = broadcast.clone();
		let importer = ts::Import::new(broadcast, catalog.reserve());
		tracing::info!(%path, "publishing ingest broadcast");

		Ok(Self {
			importer,
			broadcast: handle,
		})
	}

	/// Feed a chunk of MPEG-TS bytes (one SRT payload) into the importer.
	///
	/// `decode` drains `data` fully, buffering any partial trailing packet in
	/// its own internal scratch, so there's nothing to retain here.
	pub fn feed(&mut self, data: Bytes) -> Result<()> {
		Ok(self.importer.decode(&data)?)
	}

	/// Flush any buffered media, close out the broadcast's open groups, and end
	/// the broadcast so the origin unannounces it immediately.
	pub fn finish(&mut self) -> Result<()> {
		self.importer.finish()?;
		self.broadcast.clone().finish();
		Ok(())
	}

	/// Abort the published tracks with `err` so subscribers see the real cause
	/// (the SRT caller dropped, a demux error) rather than a generic `Error::Dropped`.
	pub fn abort(&mut self, err: moq_net::Error) {
		self.importer.abort(err);
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
	/// `latency` bounds how long the muxer waits for a stalled group before it
	/// skips ahead to a newer one. We reuse the locally configured SRT receive
	/// latency for it: SRT paces egress on the media clock, so the skip threshold
	/// shares the same latency budget. It's the configured value, not the
	/// handshake result (srt-tokio doesn't expose the negotiated latency), so a
	/// peer that requests a higher receive latency gets a larger actual buffer
	/// than this skip threshold.
	///
	/// Returns `Ok(None)` if the broadcast can never be served (path outside the
	/// consumer's scope, or the origin closed). Otherwise waits for the broadcast
	/// to be announced, so a caller may connect before the publisher does.
	pub async fn new(origin: &origin::Consumer, path: &str, latency: Duration) -> Result<Option<Self>> {
		// Confirm the broadcast is in scope and wait for it to be announced (out-of-scope /
		// origin-closed -> `None`). The export re-resolves it (and any referenced sibling
		// broadcast, via the catalog `broadcast` field) through the origin.
		if origin.announced_broadcast(path).await.is_none() {
			return Ok(None);
		}

		let source = moq_mux::Source::new(origin.consume(), path);
		let export = ts::Export::new(source).await?.with_latency(latency);
		Ok(Some(Self { export }))
	}

	/// Pull the next muxed frame (TS bytes + media timestamp), or `None` once the
	/// broadcast ends.
	pub async fn next(&mut self) -> Result<Option<Frame>> {
		Ok(self.export.next().await?)
	}
}
