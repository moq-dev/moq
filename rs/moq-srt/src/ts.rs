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
	///
	/// `max_age` is the retention declared on the media tracks the importer mints: how
	/// long relays keep a non-latest group fetchable, or `None` for hang's own default.
	pub fn new(origin: &origin::Producer, path: &str, max_age: Option<Duration>) -> Result<Self> {
		let mut broadcast = origin.create_broadcast(path, broadcast::Route::new().with_announce(true))?;
		let config = moq_mux::catalog::Config::default().with_max_age(max_age);
		let catalog = moq_mux::catalog::Producer::with_config(&mut broadcast, config)?;
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
		let export = ts::Export::new(source).await?.with_max_age(latency);
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
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::Origin::random().into());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(driver);
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use super::*;

	/// One payload-only TS packet carrying a complete PSI section (PUSI + pointer_field
	/// 0), padded to 188 with stuffing.
	fn psi_packet(pid: u16, section: &[u8]) -> Vec<u8> {
		let mut p = vec![0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10, 0x00];
		p.extend_from_slice(section);
		p.resize(188, 0xff);
		p
	}

	/// Append the CRC-32/MPEG-2 the PSI parser checks (poly 0x04c11db7, init all-ones,
	/// unreflected, no final xor) over everything written so far.
	fn seal(mut section: Vec<u8>) -> Vec<u8> {
		let mut crc = 0xffff_ffffu32;
		for byte in &section {
			crc ^= u32::from(*byte) << 24;
			for _ in 0..8 {
				crc = if crc & 0x8000_0000 != 0 {
					(crc << 1) ^ 0x04c1_1db7
				} else {
					crc << 1
				};
			}
		}
		section.extend_from_slice(&crc.to_be_bytes());
		section
	}

	/// A PAT with one program (number 1) whose PMT lives on `pmt_pid`.
	fn pat(pmt_pid: u16) -> Vec<u8> {
		let mut s = vec![0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00];
		s.extend_from_slice(&[0x00, 0x01]);
		s.extend_from_slice(&[0xe0 | (pmt_pid >> 8) as u8, pmt_pid as u8]);
		seal(s)
	}

	/// A PMT for program 1 declaring a single H.264 elementary stream on `es_pid`.
	fn pmt(es_pid: u16) -> Vec<u8> {
		let mut s = vec![0x02, 0xb0, 0x12, 0x00, 0x01, 0xc1, 0x00, 0x00];
		s.extend_from_slice(&[0xe0 | (es_pid >> 8) as u8, es_pid as u8]);
		s.extend_from_slice(&[0xf0, 0x00]);
		s.extend_from_slice(&[0x1b, 0xe0 | (es_pid >> 8) as u8, es_pid as u8, 0xf0, 0x00]);
		seal(s)
	}

	/// The retention the caller configured has to reach the media tracks the TS importer
	/// mints off the PMT, not stop at the catalog producer it was set on.
	#[tokio::test]
	async fn publisher_declares_the_configured_retention() {
		let origin = produce_origin();
		let mut publisher = Publisher::new(&origin, "live/cam0", Some(Duration::from_secs(3))).unwrap();

		let mut ts = psi_packet(0x0000, &pat(0x0100));
		ts.extend_from_slice(&psi_packet(0x0100, &pmt(0x0101)));
		publisher.feed(Bytes::from(ts)).unwrap();

		let broadcast = origin.consume().announced_broadcast("live/cam0").await.unwrap();
		let info = broadcast.track("0.avc3").unwrap().info().await.unwrap();
		assert_eq!(info.max_age, Duration::from_secs(3));
	}
}
