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
	// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
	// verbatim, so the importer uses the `mpegts` catalog extension rather than the
	// media-only `()`, which would route those PIDs to `Stream::Ignored` and drop them.
	importer: ts::Import<ts::Ext>,
	// A clone of the importer's producer, so a deliberate end can finish() the
	// broadcast (prompt unannounce) even though the importer owns it.
	broadcast: moq_net::broadcast::Producer,
}

impl Publisher {
	/// Create the broadcast on `origin` at `path` and wire up the TS importer +
	/// catalog.
	pub fn new(origin: &origin::Producer, path: &str) -> Result<Self> {
		let mut broadcast = origin.create_broadcast(path, broadcast::Route::new().with_announce(true))?;
		let catalog = moq_mux::catalog::Producer::with_catalog(
			&mut broadcast,
			moq_mux::catalog::hang::Catalog::<ts::Ext>::default(),
		)?;
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
		self.broadcast.finish();
		Ok(())
	}

	/// Abort the published tracks with `err` so subscribers see the real cause
	/// (the SRT caller dropped, a demux error) rather than a generic `Error::Dropped`.
	///
	/// Consumes the publisher: the broadcast is done.
	pub fn abort(self, err: moq_net::Error) {
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
	export: ts::Export<ts::Ext>,
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
		let export = ts::Export::with_ts(source, moq_mux::catalog::CatalogFormat::Hang)
			.await?
			.with_latency(latency);
		Ok(Some(Self { export }))
	}

	/// Pull the next muxed frame (TS bytes + media timestamp), or `None` once the
	/// broadcast ends.
	pub async fn next(&mut self) -> Result<Option<Frame>> {
		Ok(self.export.next().await?)
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use moq_mux::catalog::hang::Container;
	use moq_mux::catalog::{CatalogFormat, Stream};
	use tokio::time::timeout;

	use super::*;

	/// Real 5s H.264 + AAC capture with SCTE-35 time_signal cues on a
	/// CUEI-registered section PID (0x21, stream_type 0x86), the same fixture
	/// moq-mux's export tests replay.
	const BBB5S: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/scte35/bbb5s.ts");

	/// SRT is a contribution protocol, so the cues riding the feed must survive
	/// ingest: [`Publisher`] builds a `ts::Import<Ext>`, which catalogs the
	/// SCTE-35 PID in the `mpegts` section and publishes the sections verbatim.
	/// With the media-only `Catalog<()>` the CUEI PID routes to `Stream::Ignored`
	/// and the cues silently vanish.
	#[tokio::test(start_paused = true)]
	async fn publisher_preserves_scte35_cues() {
		let origin = moq_net::Origin::random().produce();
		let mut publisher = Publisher::new(&origin, "ingest").unwrap();

		// Resolve the broadcast like any subscriber would (waits for the announce).
		let broadcast = timeout(Duration::from_secs(5), origin.consume().announced_broadcast("ingest"))
			.await
			.expect("announce timed out")
			.expect("the ingest broadcast is announced");

		publisher.feed(bytes::Bytes::from_static(BBB5S)).unwrap();

		// The catalog advertises the cue track in its `mpegts` section...
		let mut catalog = moq_mux::catalog::Consumer::<ts::Ext>::new(&broadcast, CatalogFormat::Hang)
			.await
			.unwrap();
		let name = loop {
			let snapshot = timeout(Duration::from_secs(5), catalog.next())
				.await
				.expect("no catalog snapshot carried the cue track")
				.unwrap()
				.expect("the catalog ended without the cue track");
			if let Some((name, track)) = snapshot
				.mpegts
				.tracks
				.iter()
				.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x86))
			{
				assert_eq!(track.pid, 0x21, "the cue PID is preserved");
				assert_eq!(
					track.verbatim.as_ref().unwrap().framing,
					ts::Framing::Section,
					"SCTE-35 is section-framed"
				);
				break name.clone();
			}
		};

		// ...and the splice_info_sections themselves are published verbatim.
		let track = broadcast.track(name.as_str()).unwrap().subscribe(None).await.unwrap();
		let mut reader = moq_mux::container::Consumer::new(track, Container::Legacy).with_latency(Duration::ZERO);
		let cue = timeout(Duration::from_secs(5), reader.read())
			.await
			.expect("cue read timed out")
			.unwrap()
			.expect("a published cue section");
		let expected_cue = cue.payload;
		assert_eq!(expected_cue[0], 0xFC, "a verbatim splice_info_section (table_id 0xFC)");

		let mut subscriber = Subscriber::new(&origin.consume(), "ingest", Duration::ZERO)
			.await
			.unwrap()
			.expect("the ingest broadcast is available for SRT egress");
		let mut output = Vec::new();
		loop {
			match timeout(Duration::from_secs(5), subscriber.next()).await {
				Ok(Ok(Some(frame))) => output.extend_from_slice(&frame.payload),
				Ok(Ok(None)) => break,
				Ok(Err(err)) => panic!("SRT egress failed: {err}"),
				Err(_) => break,
			}
		}
		publisher.finish().unwrap();

		let mut roundtrip = moq_net::broadcast::Info::new().produce();
		let roundtrip_consumer = roundtrip.consume();
		let roundtrip_catalog = moq_mux::catalog::Producer::with_catalog(
			&mut roundtrip,
			moq_mux::catalog::hang::Catalog::<ts::Ext>::default(),
		)
		.unwrap();
		let mut roundtrip_import = ts::Import::new(roundtrip, roundtrip_catalog.reserve());
		roundtrip_import.decode(&output).unwrap();
		roundtrip_import.finish().unwrap();

		let snapshot = roundtrip_catalog.snapshot();
		let (name, _) = snapshot
			.mpegts
			.tracks
			.iter()
			.find(|(_, track)| {
				track
					.verbatim
					.as_ref()
					.is_some_and(|verbatim| verbatim.stream_type == 0x86)
			})
			.expect("SRT egress preserves the SCTE-35 track");
		let track = roundtrip_consumer.track(name).unwrap().subscribe(None).await.unwrap();
		let mut reader = moq_mux::container::Consumer::new(track, Container::Legacy);
		let cue = timeout(Duration::from_secs(5), reader.read())
			.await
			.expect("round-trip cue read timed out")
			.unwrap()
			.expect("SRT egress preserves a cue section");
		assert_eq!(cue.payload[0], 0xFC, "the round-trip cue is a splice_info_section");
		assert_eq!(
			cue.payload, expected_cue,
			"SRT egress preserves the complete cue section"
		);
	}
}
