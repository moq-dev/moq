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
use moq_net::origin;

use crate::Result;

/// Publishes an MPEG-TS source into the origin as a single broadcast.
///
/// Each chunk is handed straight to the TS importer, which consumes whole
/// transport packets and retains any partial trailing packet internally for the
/// next call (the same pattern `moq-cli import ... stdin ts` uses against stdin).
/// Either [`Self::finish`] or dropping the publisher ends the broadcast and
/// unannounces the path.
pub struct Publisher {
	// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
	// verbatim, so the importer uses the `mpegts` catalog extension rather than the
	// media-only `()`, which would route those PIDs to `Stream::Ignored` and drop them.
	importer: ts::Import<ts::Ext>,
	// A clone of the importer's producer, so an end can close the broadcast
	// (prompt unannounce) even though the importer owns it.
	broadcast: moq_net::broadcast::Producer,
	// The importer's per-stream counters, logged as `moq import ts` logs them, under a
	// span naming the path, since one server carries many ingests.
	log: ts::StatsLog,
	sampled: tokio::time::Instant,
	span: tracing::Span,
}

impl Publisher {
	/// Create the broadcast on `origin` at `path` and wire up the TS importer +
	/// catalog.
	///
	/// `config` is the catalog the importer publishes into: retention
	/// (`with_max_age`) and the connection allocator passthrough tracks claim on
	/// (`with_bandwidth`).
	pub fn new(origin: &origin::Producer, path: &str, config: moq_mux::catalog::Config) -> Result<Self> {
		let mut broadcast = origin.publish(path, moq_net::origin::Route::default())?;
		let config = config.with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;
		let handle = broadcast.clone();
		let importer = ts::Import::new(broadcast, catalog.reserve());
		tracing::info!(%path, "publishing ingest broadcast");

		Ok(Self {
			importer,
			broadcast: handle,
			log: ts::StatsLog::default(),
			sampled: tokio::time::Instant::now(),
			span: tracing::info_span!("srt", %path),
		})
	}

	/// Feed a chunk of MPEG-TS bytes (one SRT payload) into the importer.
	///
	/// `decode` drains `data` fully, buffering any partial trailing packet in
	/// its own internal scratch, so there's nothing to retain here. Once every
	/// [`ts::StatsLog::INTERVAL`] it also logs what moved in the importer's
	/// per-stream counters.
	pub fn feed(&mut self, data: Bytes) -> Result<()> {
		self.importer.decode(&data).map_err(moq_mux::Error::from)?;
		if self.sampled.elapsed() >= ts::StatsLog::INTERVAL {
			self.sampled = tokio::time::Instant::now();
			let _span = self.span.enter();
			self.log.sample(self.importer.stats());
		}
		Ok(())
	}

	/// Flush any buffered media, close out the broadcast's open groups, and end
	/// the broadcast so the origin unannounces it immediately.
	pub fn finish(&mut self) -> Result<()> {
		self.importer.finish().map_err(moq_mux::Error::from)?;
		// The drain at end of input can publish a frame nothing vouched for.
		self.span.in_scope(|| self.log.finish(&self.importer.stats()));
		self.broadcast.close();
		Ok(())
	}

	/// Abort the published tracks with `err` so subscribers see the real cause
	/// (the SRT caller dropped, a demux error) rather than a generic `Error::Dropped`.
	///
	/// Consumes the publisher and closes the broadcast.
	pub fn abort(self, err: moq_net::Error) {
		self.importer.abort(err);
		self.broadcast.close();
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
		if origin.routed(path).await.is_none() {
			return Ok(None);
		}

		let source = moq_mux::Source::new(origin.consume(), path);
		let export = ts::Export::with_ts(source, moq_mux::catalog::CatalogFormat::Hang)
			.await?
			.with_max_age(latency);
		Ok(Some(Self { export }))
	}

	/// Pull the next muxed frame (TS bytes + media timestamp), or `None` once the
	/// broadcast ends.
	pub async fn next(&mut self) -> Result<Option<Frame>> {
		Ok(self.export.next().await?)
	}

	/// The muxer's generation counter for the frame [`next`](Self::next) just
	/// returned, which increments each time the publisher rewinds its timeline.
	///
	/// The muxer restarts its program clock across that boundary, so a caller
	/// pacing on the media timestamps has to drop its own anchor with it: sample
	/// this after every frame and re-anchor when it changes, or the rewound span
	/// maps into the past and the whole new generation collapses onto one instant.
	pub fn discontinuity(&self) -> u64 {
		self.export.discontinuity()
	}
}

#[cfg(test)]
mod tests {
	use moq_mux::catalog::hang::Container;
	use moq_mux::catalog::{CatalogFormat, Stream};
	use tokio::time::timeout;

	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use super::*;

	/// Real 5s H.264 + AAC capture with SCTE-35 time_signal cues on a
	/// CUEI-registered section PID (0x21, stream_type 0x86), the same fixture
	/// moq-mux's export tests replay.
	const BBB5S: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/scte35/bbb5s.ts");

	/// The PCR a packet's adaptation field carries, in 27 MHz ticks.
	fn pcr(pkt: &[u8]) -> Option<u64> {
		if pkt[3] & 0x20 == 0 || pkt[4] < 7 || pkt[5] & 0x10 == 0 {
			return None;
		}
		let base = (u64::from(pkt[6]) << 25)
			| (u64::from(pkt[7]) << 17)
			| (u64::from(pkt[8]) << 9)
			| (u64::from(pkt[9]) << 1)
			| (u64::from(pkt[10]) >> 7);
		Some(base * 300 + ((u64::from(pkt[10] & 0x01) << 8) | u64::from(pkt[11])))
	}

	/// Each packet of `ts` with the program clock it arrives at, counted from the first PCR.
	fn timed(ts: &[u8]) -> impl Iterator<Item = (Duration, &[u8; 188])> {
		let mut first = None;
		let mut now = Duration::ZERO;
		ts.as_chunks::<188>().0.iter().map(move |pkt| {
			if let Some(pcr) = pcr(pkt) {
				let first = *first.get_or_insert(pcr);
				now = Duration::from_nanos((pcr - first) * 1_000 / 27);
			}
			(now, pkt)
		})
	}

	/// `ts` with the PES on `pid` suppressed from its first PES start at or after `from` to
	/// its first at or after `to`, as an encoder whose one input died behind a running mux
	/// emits it: the PCR kept in adaptation-only packets, everything else null stuffing, and
	/// the counters after the gap renumbered so continuity stays legal. The stimulus the
	/// moq-mux TS import tests check is legal.
	fn suppress(ts: &[u8], pid: u16, from: Duration, to: Duration) -> Vec<u8> {
		let mut null = [0xff; 188];
		null[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);

		let mut out = Vec::with_capacity(ts.len());
		let (mut active, mut done) = (false, false);
		let (mut last_cc, mut dropped) = (0, 0u8);
		for (now, pkt) in timed(ts) {
			let mut pkt = *pkt;
			if (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]) == pid {
				if pkt[1] & 0x40 != 0 {
					if !active && !done && now >= from {
						active = true;
					} else if active && now >= to {
						(active, done) = (false, true);
					}
				}
				let payload = pkt[3] & 0x10 != 0;
				if active {
					dropped = (dropped + u8::from(payload)) & 0x0f;
					pkt = match pcr(&pkt) {
						Some(_) => {
							let mut clock = [0xff; 188];
							clock[..6].copy_from_slice(&[0x47, pkt[1] & 0x1f, pkt[2], 0x20 | last_cc, 183, 0x10]);
							clock[6..12].copy_from_slice(&pkt[6..12]);
							clock
						}
						None => null,
					};
				} else {
					pkt[3] = (pkt[3] & 0xf0) | (pkt[3].wrapping_sub(dropped) & 0x0f);
					if payload {
						last_cc = pkt[3] & 0x0f;
					}
				}
			}
			out.extend_from_slice(&pkt);
		}
		assert!(done, "the fixture must resume the PID before it ends");
		out
	}

	/// Publish `ts` on `path` one SRT payload (7 packets) at a time, each delivered when the
	/// program clock says it is due.
	async fn ingest(origin: &moq_net::origin::Producer, path: &str, ts: &[u8]) {
		let mut publisher = Publisher::new(origin, path, Default::default()).unwrap();
		let mut clock = Duration::ZERO;
		for payload in timed(ts).collect::<Vec<_>>().chunks(7) {
			let (due, _) = payload[0];
			tokio::time::advance(due.saturating_sub(clock)).await;
			clock = clock.max(due);
			let bytes: Vec<u8> = payload.iter().flat_map(|(_, pkt)| pkt.iter().copied()).collect();
			publisher.feed(bytes.into()).unwrap();
		}
		publisher.finish().unwrap();
	}

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
		let mut publisher = Publisher::new(
			&origin,
			"live/cam0",
			moq_mux::catalog::Config::default().with_max_age(Duration::from_secs(3)),
		)
		.unwrap();

		let mut ts = psi_packet(0x0000, &pat(0x0100));
		ts.extend_from_slice(&psi_packet(0x0100, &pmt(0x0101)));
		publisher.feed(Bytes::from(ts)).unwrap();

		let consumer = origin.consume();
		consumer.routed("live/cam0").await.unwrap();
		let broadcast = consumer.request_broadcast("live/cam0").await.unwrap();
		let info = broadcast.track("0.avc3").unwrap().query().await.unwrap();
		assert_eq!(info.max_age, Duration::from_secs(3));
	}

	/// A video PID that goes silent behind a running mux is logged against its ingest path,
	/// and neither the same feed intact nor the audio that kept delivering is.
	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn publisher_reports_a_silent_pid() {
		const VIDEO: u16 = 0x100;
		let origin = produce_origin();
		ingest(&origin, "intact", BBB5S).await;
		let stimulus = suppress(BBB5S, VIDEO, Duration::from_millis(1500), Duration::from_millis(3500));
		ingest(&origin, "silent", &stimulus).await;

		logs_assert(|lines: &[&str]| {
			let stopped: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("stopped delivering access units"))
				.collect();
			let reported = |path: &str, pid: u16| {
				stopped
					.iter()
					.any(|line| line.contains(&format!("path={path}")) && line.contains(&format!("pid={pid} ")))
			};
			if !reported("silent", VIDEO) {
				return Err(format!("the silent video PID was not reported: {stopped:?}"));
			}
			if reported("intact", VIDEO) {
				return Err(format!("the intact feed's video was reported: {stopped:?}"));
			}
			if reported("silent", 0x101) || reported("intact", 0x101) {
				return Err(format!("the audio kept delivering: {stopped:?}"));
			}
			Ok(())
		});
	}

	/// SRT is a contribution protocol, so SCTE-35 cues survive ingest and egress.
	#[tokio::test(start_paused = true)]
	async fn publisher_preserves_scte35_cues() {
		let origin = produce_origin();
		let mut publisher = Publisher::new(&origin, "ingest", Default::default()).unwrap();

		let consumer = origin.consume();
		timeout(Duration::from_secs(5), consumer.routed("ingest"))
			.await
			.expect("announce timed out")
			.expect("the ingest broadcast is announced");
		let broadcast = consumer.request_broadcast("ingest").await.unwrap();

		publisher.feed(bytes::Bytes::from_static(BBB5S)).unwrap();

		let mut catalog = moq_mux::catalog::Consumer::<ts::Ext>::new(&broadcast, CatalogFormat::Hang)
			.await
			.unwrap();
		let name = loop {
			let snapshot = timeout(Duration::from_secs(5), catalog.next())
				.await
				.expect("no catalog snapshot carried the cue track")
				.unwrap()
				.expect("the catalog ended without the cue track");
			if let Some((name, track)) = snapshot.ext.mpegts.tracks.iter().find(|(_, track)| {
				track
					.verbatim
					.as_ref()
					.is_some_and(|verbatim| verbatim.stream_type == 0x86)
			}) {
				assert_eq!(track.pid, 0x21, "the cue PID is preserved");
				assert_eq!(
					track.verbatim.as_ref().unwrap().framing,
					ts::Framing::Section,
					"SCTE-35 is section-framed"
				);
				break name.clone();
			}
		};

		let track = broadcast.track(name.as_str()).unwrap().subscribe(None).await.unwrap();
		let mut reader = moq_mux::container::Consumer::new(track, Container::Legacy(moq_mux::container::Kind::Data));
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
		let roundtrip_catalog = moq_mux::catalog::Producer::new(
			&mut roundtrip,
			moq_mux::catalog::Config::default().with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default()),
		)
		.unwrap();
		let mut roundtrip_import = ts::Import::new(roundtrip, roundtrip_catalog.reserve());
		roundtrip_import.decode(&output).unwrap();
		roundtrip_import.finish().unwrap();

		let snapshot = roundtrip_catalog.snapshot();
		let (name, _) = snapshot
			.ext
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
		let mut reader = moq_mux::container::Consumer::new(track, Container::Legacy(moq_mux::container::Kind::Data));
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
