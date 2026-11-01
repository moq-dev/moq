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

/// Which programs of a multi-program MPEG-TS an ingest publishes.
///
/// Without one, an ingest whose PAT lists more than one program fails with
/// [`ts::MultipleProgramsError`] rather than merging unrelated clocks onto one broadcast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
	/// Only the program with this PAT program number, on the ingest's path.
	One(u16),
	/// Every program the first PAT lists, each as its own broadcast under the ingest's path:
	/// `live/cam0` publishes `live/cam0/1`, `live/cam0/2`, ... (`event.hang` publishes
	/// `event/1.hang`, keeping the catalog suffix last).
	All,
}

/// Publishes an MPEG-TS source into the origin: one broadcast, or one per program.
///
/// Each chunk is handed straight to the TS importer, which consumes whole
/// transport packets and retains any partial trailing packet internally for the
/// next call (the same pattern `moq-cli import ... stdin ts` uses against stdin).
/// [`Self::finish`] keeps the finished broadcasts up for a linger; dropping the
/// publisher ends them and unannounces their paths.
///
/// A step of the source's clock onto another timeline ([`ts::Restart`]), such as a
/// changeover between two encoders whose clocks were never locked, finishes the broadcast
/// and republishes the rest of the input on the same path under a new epoch, on the same
/// connection. [`Program::All`] still ends the ingest there.
pub struct Publisher {
	importer: Importer,
	// The importer's per-stream counters, logged as `moq import ts` logs them, under a
	// span naming the path, since one server carries many ingests.
	log: ts::stats::Log,
	sampled: moq_net::time::Instant,
	span: tracing::Span,
}

enum Importer {
	One(Box<One>),
	/// Each program's broadcast is created once the first PAT names it, and announced once its PMT
	/// declares its tracks.
	All(Box<ts::Programs>),
}

/// One program's broadcast, and what the next instance publishes with when the source's
/// clock steps.
struct One {
	instance: Instance,
	origin: origin::Producer,
	path: String,
	config: moq_mux::catalog::Config,
	program: Option<u16>,
}

/// One program's broadcast, under one epoch.
struct Instance {
	// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
	// verbatim, so the importer uses the `mpegts` catalog extension rather than the
	// media-only `()`, which would route those PIDs to `Stream::Ignored` and drop them.
	import: ts::Import<ts::Ext>,
	// Finished at a clean end, so the finished broadcast keeps serving the catalog.
	catalog: moq_mux::catalog::Producer<ts::Ext>,
	// Controls publication while the importer fills it, including finish or close at EOF.
	broadcast: moq_net::origin::Publication,
}

impl Instance {
	/// Publish `path` under a new epoch, importing `program` of the input, or its only one.
	///
	/// Each instance is its own publisher, so an encoder reconnecting under the same stream id,
	/// or a source whose clock stepped, replaces the stale broadcast instead of resuming into it.
	fn publish(
		origin: &origin::Producer,
		path: &str,
		config: moq_mux::catalog::Config,
		program: Option<u16>,
	) -> Result<(Self, moq_net::Epoch)> {
		let epoch = moq_net::Epoch::mint();
		let broadcast = origin.publish(path, moq_net::origin::Route::default().with_epoch(epoch.clone()))?;
		let config = config.with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config)?;
		let mut import = ts::Import::new(broadcast.broadcast().clone(), catalog.reserve());
		if let Some(program) = program {
			import = import.with_program(program);
		}
		let instance = Self {
			import,
			catalog,
			broadcast,
		};
		Ok((instance, epoch))
	}

	/// Flush the buffered media and finish every track, the catalog included.
	fn finish(&mut self) -> Result<()> {
		self.import.finish().map_err(moq_mux::Error::from)?;
		self.catalog.finish()?;
		Ok(())
	}
}

impl Publisher {
	/// Wire up the TS importer and catalog for `path` on `origin`, announcing the broadcast
	/// now, or each program's once its PMT declares its tracks for [`Program::All`].
	///
	/// `config` is the catalog the importer publishes into: retention
	/// (`with_max_age`) and the connection allocator passthrough tracks claim on
	/// (`with_bandwidth`).
	pub fn new(
		origin: &origin::Producer,
		path: &str,
		config: moq_mux::catalog::Config,
		program: Option<Program>,
	) -> Result<Self> {
		let importer = match program {
			// `ts::Programs` mints an epoch per program.
			Some(Program::All) => {
				tracing::info!(%path, ?program, "publishing ingest broadcasts");
				Importer::All(Box::new(ts::Programs::new(
					origin.clone(),
					moq_net::Path::new(path).map_err(moq_net::Error::from)?,
					config,
				)))
			}
			Some(Program::One(_)) | None => {
				let program = match program {
					Some(Program::One(program)) => Some(program),
					_ => None,
				};
				let (instance, epoch) = Instance::publish(origin, path, config.clone(), program)?;
				tracing::info!(%path, ?program, %epoch, "publishing ingest broadcast");
				Importer::One(Box::new(One {
					instance,
					origin: origin.clone(),
					path: path.to_string(),
					config,
					program,
				}))
			}
		};

		Ok(Self {
			importer,
			log: ts::stats::Log::default(),
			sampled: moq_net::time::Instant::now(),
			span: tracing::info_span!("srt", %path),
		})
	}

	/// Feed a chunk of MPEG-TS bytes (one SRT payload) into the importer.
	///
	/// `decode` drains `data` fully, buffering any partial trailing packet in
	/// its own internal scratch, so there's nothing to retain here. Once every
	/// [`ts::stats::Log::INTERVAL`] it also logs what moved in the importer's
	/// per-stream counters.
	pub fn feed(&mut self, data: Bytes) -> Result<()> {
		match &mut self.importer {
			Importer::One(one) => {
				let One {
					instance,
					origin,
					path,
					config,
					program,
				} = &mut **one;
				let mut data = data;
				while let Err(err) = instance.import.decode(&data) {
					let restart = err.downcast::<ts::Restart>().map_err(moq_mux::Error::from)?;
					// The instance before the step finishes; the rest of the input republishes
					// the path under a new epoch.
					instance.finish()?;
					self.span.in_scope(|| self.log.finish(&instance.import.stats()));
					let (next, epoch) = Instance::publish(origin, path, config.clone(), *program)?;
					tracing::warn!(%path, step = %restart.step, %epoch, "the source's clock stepped; republishing");
					// The finished broadcast ends with its publication.
					*instance = next;
					self.log = ts::stats::Log::default();
					data = restart.remainder;
				}
			}
			Importer::All(programs) => programs.decode(&data).map_err(moq_mux::Error::from)?,
		}
		if self.sampled.elapsed() >= ts::stats::Log::INTERVAL {
			self.sampled = moq_net::time::Instant::now();
			let _span = self.span.enter();
			self.log.sample(self.stats());
		}
		Ok(())
	}

	fn stats(&self) -> ts::stats::Snapshot {
		match &self.importer {
			Importer::One(one) => one.instance.import.stats(),
			Importer::All(programs) => programs.stats(),
		}
	}

	/// Flush any buffered media and finish every track, the catalogs included, keeping each
	/// finished broadcast up for `linger`.
	pub fn finish(mut self, linger: Duration) -> Result<moq_mux::Linger<Self>> {
		let finished = match &mut self.importer {
			Importer::One(one) => {
				one.instance.finish()?;
				vec![one.instance.broadcast.broadcast().clone()]
			}
			Importer::All(programs) => programs.finish().map_err(moq_mux::Error::from)?,
		};
		// The drain at end of input can publish a frame nothing vouched for.
		self.span.in_scope(|| self.log.finish(&self.stats()));
		Ok(moq_mux::Linger::new(finished, self, linger))
	}

	/// Abort the published tracks with `err` so subscribers see the real cause
	/// (the SRT caller dropped, a demux error) rather than a generic `Error::Dropped`.
	///
	/// Consumes the publisher and closes the broadcasts.
	pub fn abort(self, err: moq_net::Error) {
		match self.importer {
			Importer::One(one) => {
				let Instance { import, broadcast, .. } = one.instance;
				import.abort(err);
				broadcast.close();
			}
			Importer::All(programs) => programs.abort(err),
		}
	}
}

/// The least delay an egress muxes at, `moq export ts`'s default: the send-ahead that fills a
/// receiver's buffers comes out of the delay, and a broadcast-sized CPB needs a second of it.
const MIN_DELAY: Duration = Duration::from_secs(1);

/// How an egress treats its broadcast ending or being replaced.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Options {
	/// The muxer's delay, at least [`MIN_DELAY`] ([`Subscriber::new`]).
	pub latency: Duration,
	/// How long to wait for the broadcast to come back once it ends.
	pub follow: Duration,
	/// Follow another instance replacing the broadcast, as a full program switch.
	pub stitch: bool,
}

/// Muxes a single MoQ broadcast back into an MPEG-TS byte stream for egress.
///
/// The mirror of [`Publisher`]: where that demuxes SRT-carried TS into the
/// origin, this consumes a broadcast from the origin and re-muxes it to TS so an
/// SRT caller can play it. Pull frames with [`next`](Self::next); each carries
/// the TS bytes plus the media timestamp used to pace delivery.
///
/// The path's announcements drive it through [`ts::Follower`], as they drive a player: the same
/// publisher instance returning within the follow window continues the stream, and a replacement either
/// ends it with [`moq_mux::Error::Replaced`] or, with `stitch`, switches the program.
pub struct Subscriber {
	follower: ts::Follower<ts::Ext>,
}

impl Subscriber {
	/// Resolve the broadcast at `path` in the origin and prepare to mux it to TS.
	///
	/// `options.latency` is the muxer's delay, but no less than [`MIN_DELAY`]: the output runs
	/// that far behind the source, a frame arriving later is dropped, and a stalled group is
	/// skipped after half of it. We reuse the locally configured SRT receive latency for
	/// it, the same budget an SRT hop gives a packet. It's the configured value,
	/// not the handshake result (srt-tokio doesn't expose the negotiated latency),
	/// so a peer that requests a higher receive latency gets a larger actual
	/// buffer than this delay.
	///
	/// Returns `Ok(None)` if the broadcast can never be served (path outside the
	/// consumer's scope, or the origin closed). Otherwise waits for the broadcast
	/// to be announced, so a caller may connect before the publisher does.
	pub(crate) async fn new(origin: &origin::Consumer, path: &str, options: Options) -> Result<Option<Self>> {
		let Ok(mut following) = origin.follow(path, None) else {
			return Ok(None);
		};

		// Join the broadcast once announced. The export resolves any referenced sibling
		// broadcast (the catalog `broadcast` field) through the same origin.
		let broadcast = match following.available().await {
			Ok(broadcast) => broadcast,
			Err(moq_net::Error::Closed) => return Ok(None),
			Err(err) => return Err(err.into()),
		};
		let source = moq_mux::Source::new(origin.clone(), broadcast);
		let catalog = source.catalog::<ts::Ext>(moq_mux::catalog::CatalogFormat::Hang).await?;
		let export = ts::Export::new(source, catalog).with_delay(options.latency.max(MIN_DELAY));
		let follower = ts::Follower::new(export)?
			.with_follow(options.follow)
			.with_stitch(options.stitch);
		Ok(Some(Self { follower }))
	}

	/// Pull the next muxed frame (TS bytes + media timestamp), or `None` once the
	/// broadcast ends with nothing to follow. Dropping the future before it resolves loses
	/// nothing.
	pub async fn next(&mut self) -> Result<Option<Frame>> {
		Ok(self.follower.next().await?)
	}

	/// The muxer's generation counter for the frame [`next`](Self::next) just
	/// returned, which increments each time the program clock restarts: a marker that
	/// breaks the publisher's timeline, or a switch to another instance.
	///
	/// A caller pacing on the media timestamps has to drop its own anchor with it: sample
	/// this after every frame and re-anchor when it changes, or the new clock maps
	/// through the old anchor and the whole new generation collapses onto one instant.
	pub fn discontinuity(&self) -> u64 {
		self.follower.export().discontinuity()
	}
}

#[cfg(test)]
mod tests {
	use moq_clock::timeout;
	use moq_mux::catalog::hang::Container;
	use moq_mux::catalog::{CatalogFormat, Stream};

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

	/// Publish each of `passes` on `path` through one connection, one SRT payload (7 packets) at
	/// a time, each delivered when its pass's program clock says it is due. A pass starts as the
	/// one before ends, whatever its clock reads.
	async fn ingest(origin: &moq_net::origin::Producer, path: &str, passes: &[&[u8]]) {
		let mut publisher = Publisher::new(origin, path, Default::default(), None).unwrap();
		let (mut clock, mut start) = (Duration::ZERO, Duration::ZERO);
		for ts in passes {
			for payload in timed(ts).collect::<Vec<_>>().chunks(7) {
				let due = start + payload[0].0;
				tokio::time::advance(due.saturating_sub(clock)).await;
				clock = clock.max(due);
				let bytes: Vec<u8> = payload.iter().flat_map(|(_, pkt)| pkt.iter().copied()).collect();
				publisher.feed(bytes.into()).unwrap();
			}
			start = clock + Duration::from_millis(40);
		}
		drop(publisher.finish(Duration::ZERO).unwrap());
	}

	/// `ts` with every PCR, PTS and DTS moved by `by` 90 kHz ticks, modulo the 33-bit wrap,
	/// and no `discontinuity_indicator`: the same programme from another encoder's clock.
	fn shift(ts: &[u8], by: i64) -> Vec<u8> {
		const WRAP: i64 = 1 << 33;
		let mut out = ts.to_vec();
		for pkt in out.as_chunks_mut::<188>().0 {
			if let Some(pcr) = pcr(pkt) {
				let base = (pcr as i64 / 300 + by).rem_euclid(WRAP);
				pkt[6] = (base >> 25) as u8;
				pkt[7] = (base >> 17) as u8;
				pkt[8] = (base >> 9) as u8;
				pkt[9] = (base >> 1) as u8;
				pkt[10] = (pkt[10] & 0x7f) | ((base as u8 & 1) << 7);
			}
			// A PES start's PTS, and DTS after it, when its header fits the packet.
			if pkt[1] & 0x40 == 0 || pkt[3] & 0x10 == 0 {
				continue;
			}
			let start = if pkt[3] & 0x20 != 0 { 5 + usize::from(pkt[4]) } else { 4 };
			if pkt.len() < start + 19 || pkt[start..start + 3] != [0, 0, 1] {
				continue;
			}
			let fields = match pkt[start + 7] >> 6 {
				0b10 => 1,
				0b11 => 2,
				_ => 0,
			};
			for field in 0..fields {
				let f = &mut pkt[start + 9 + 5 * field..start + 14 + 5 * field];
				let t = (i64::from(f[0] >> 1 & 0x07) << 30)
					| (i64::from(f[1]) << 22)
					| (i64::from(f[2] >> 1) << 15)
					| (i64::from(f[3]) << 7)
					| i64::from(f[4] >> 1);
				let t = (t + by).rem_euclid(WRAP);
				f[0] = (f[0] & 0xf1) | ((t >> 29) as u8 & 0x0e);
				f[1] = (t >> 22) as u8;
				f[2] = ((t >> 14) as u8 & 0xfe) | 1;
				f[3] = (t >> 7) as u8;
				f[4] = ((t << 1) as u8 & 0xfe) | 1;
			}
		}
		out
	}

	/// A source switched between two encoders whose clocks started hours apart, without a
	/// `discontinuity_indicator`, either way: the ingest republishes under a new epoch on the
	/// same connection, and an egress that stitches replacements carries on into it.
	#[tokio::test(start_paused = true)]
	async fn a_clock_step_republishes_under_a_new_epoch() {
		// The issue's step: two encoders started about 4 h 40 m apart.
		const SWITCH: i64 = 16_844 * 90_000 + 45_000;
		for step in [-SWITCH, SWITCH] {
			let origin = produce_origin();
			let options = Options {
				latency: Duration::from_secs(1),
				follow: Duration::from_secs(30),
				stitch: true,
			};
			let consumer = origin.consume();
			let egress = tokio::spawn(async move {
				let mut subscriber = Subscriber::new(&consumer, "ingest", options).await.unwrap().unwrap();
				// The frames each program clock generation carried.
				let mut generations = std::collections::BTreeMap::<u64, usize>::new();
				while let Some(frame) = subscriber.next().await.expect("the egress follows the restart") {
					if !frame.payload.is_empty() {
						*generations.entry(subscriber.discontinuity()).or_default() += 1;
					}
				}
				generations
			});
			let second = shift(BBB5S, step);
			ingest(&origin, "ingest", &[BBB5S, &second]).await;
			let generations = egress.await.unwrap();
			assert!(
				generations.len() >= 2 && generations.values().all(|&frames| frames > 0),
				"{step}: the egress switched programs: {generations:?}"
			);
		}
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

	/// A PAT listing each `(program_number, pmt_pid)`.
	fn pat(programs: &[(u16, u16)]) -> Vec<u8> {
		let len = 9 + 4 * programs.len();
		let mut s = vec![0x00, 0xb0, len as u8, 0x00, 0x01, 0xc1, 0x00, 0x00];
		for &(program, pmt_pid) in programs {
			s.extend_from_slice(&program.to_be_bytes());
			s.extend_from_slice(&[0xe0 | (pmt_pid >> 8) as u8, pmt_pid as u8]);
		}
		seal(s)
	}

	/// A PMT for `program` declaring a single elementary stream of `stream_type` on `es_pid`.
	fn pmt(program: u16, stream_type: u8, es_pid: u16) -> Vec<u8> {
		let mut s = vec![0x02, 0xb0, 0x12];
		s.extend_from_slice(&program.to_be_bytes());
		s.extend_from_slice(&[0xc1, 0x00, 0x00]);
		s.extend_from_slice(&[0xe0 | (es_pid >> 8) as u8, es_pid as u8]);
		s.extend_from_slice(&[0xf0, 0x00]);
		s.extend_from_slice(&[stream_type, 0xe0 | (es_pid >> 8) as u8, es_pid as u8, 0xf0, 0x00]);
		seal(s)
	}

	/// One TS packet carrying a whole private PES with a PTS, stuffed in its adaptation field
	/// so the PES ends exactly at its declared length.
	fn pes_packet(pid: u16, pts: u64) -> Vec<u8> {
		let mut pes = vec![0x00, 0x00, 0x01, 0xbd, 0x00, 10, 0x80, 0x80, 0x05];
		pes.extend_from_slice(&[
			0x21 | (((pts >> 30) & 0x07) << 1) as u8,
			(pts >> 22) as u8,
			0x01 | (((pts >> 15) & 0x7f) << 1) as u8,
			(pts >> 7) as u8,
			0x01 | ((pts & 0x7f) << 1) as u8,
		]);
		pes.extend_from_slice(&[0xde, 0xad]);
		let stuffing = 188 - 4 - pes.len();
		let mut p = vec![
			0x47,
			0x40 | (pid >> 8) as u8,
			pid as u8,
			0x30,
			(stuffing - 1) as u8,
			0x00,
		];
		p.resize(4 + stuffing, 0xff);
		p.extend_from_slice(&pes);
		p
	}

	/// A two-program multiplex, each program carrying one private PES stream (carried
	/// verbatim) and its first PES plus a complete PCR measurement window:
	/// program 1's on PID 0x101, program 2's on 0x201.
	fn two_programs() -> Bytes {
		let mut ts = psi_packet(0x0000, &pat(&[(1, 0x0100), (2, 0x0200)]));
		ts.extend_from_slice(&psi_packet(0x0100, &pmt(1, 0x06, 0x0101)));
		ts.extend_from_slice(&psi_packet(0x0200, &pmt(2, 0x06, 0x0201)));
		ts.extend_from_slice(&pes_packet(0x0101, 90_000));
		ts.extend_from_slice(&pes_packet(0x0201, 90_000));
		// Complete the startup rate window before expecting a live catalog.
		for index in 0..=8u64 {
			let base = index * 22_500;
			for pid in [0x0101u16, 0x0201] {
				let mut packet = vec![0x47, (pid >> 8) as u8, pid as u8, 0x20, 183, 0x10];
				packet.extend_from_slice(&[
					(base >> 25) as u8,
					(base >> 17) as u8,
					(base >> 9) as u8,
					(base >> 1) as u8,
					((base & 1) as u8) << 7 | 0x7e,
					0,
				]);
				packet.resize(188, 0xff);
				ts.extend_from_slice(&packet);
			}
		}
		ts.into()
	}

	/// The PAT program number and the stream PIDs the catalog at `path` records, once it is
	/// announced.
	async fn program(origin: &moq_net::origin::Producer, path: &str) -> (u16, Vec<u16>) {
		let consumer = origin.consume();
		let broadcast = consumer.follow(path, None).unwrap().available().await.unwrap();
		let mut catalog = moq_mux::catalog::Consumer::<ts::Ext>::new(&broadcast, CatalogFormat::Hang)
			.await
			.unwrap();
		let snapshot = timeout(Duration::from_secs(5), catalog.next())
			.await
			.expect("catalog timed out")
			.unwrap()
			.expect("a catalog");
		let program = snapshot.ext.mpegts.program.as_ref().expect("the PAT identity");
		let pids = snapshot.ext.mpegts.tracks.values().map(|track| track.pid).collect();
		(program.program_number, pids)
	}

	/// Without a selection, a multiplex is refused rather than merged onto one broadcast.
	#[tokio::test(start_paused = true)]
	async fn publisher_refuses_a_multiplex() {
		let origin = produce_origin();
		let mut publisher = Publisher::new(&origin, "ingest", Default::default(), None).unwrap();
		let err = publisher.feed(two_programs()).unwrap_err();
		let crate::Error::Mux(moq_mux::Error::Other(inner)) = &err else {
			panic!("a demux error: {err}");
		};
		let refused = inner.downcast_ref::<ts::MultipleProgramsError>();
		assert_eq!(refused.map(|refused| refused.programs.as_slice()), Some(&[1, 2][..]));
	}

	/// A caller reconnecting under the same stream id while its stale connection is
	/// still open restarts the broadcast at once: each connection is its own epoch, so
	/// a fresh request reaches the reconnect, while the stale viewer stays on the old
	/// one until it leaves.
	#[tokio::test(start_paused = true)]
	async fn a_reconnect_replaces_the_stale_connection() {
		let origin = produce_origin();
		let consumer = origin.consume();
		let _stale = Publisher::new(&origin, "ingest", Default::default(), None).unwrap();
		let stale = consumer.request("ingest", None).await.unwrap();
		let mut catalog = stale
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();

		let _fresh = Publisher::new(&origin, "ingest", Default::default(), None).unwrap();
		let stale_viewer = moq_clock::timeout(Duration::from_secs(1), async {
			loop {
				match catalog.recv_group().await {
					Ok(Some(_)) => continue,
					other => return other.map(|_| ()),
				}
			}
		})
		.await;
		assert!(stale_viewer.is_err(), "the stale viewer ended: {stale_viewer:?}");
		let fresh = consumer.request("ingest", None).await.unwrap();
		assert!(!fresh.is_clone(&stale), "viewers reach the reconnected caller");
	}

	/// `Program::One` publishes the chosen program alone on the ingest's path.
	#[tokio::test(start_paused = true)]
	async fn publisher_imports_one_program() {
		let origin = produce_origin();
		let mut publisher = Publisher::new(&origin, "ingest", Default::default(), Some(Program::One(2))).unwrap();
		publisher.feed(two_programs()).unwrap();
		assert_eq!(program(&origin, "ingest").await, (2, vec![0x0201]));
	}

	/// `Program::All` publishes each program as its own broadcast under the ingest's path.
	#[tokio::test(start_paused = true)]
	async fn publisher_imports_every_program() {
		let origin = produce_origin();
		let mut publisher = Publisher::new(&origin, "ingest", Default::default(), Some(Program::All)).unwrap();
		publisher.feed(two_programs()).unwrap();
		assert_eq!(program(&origin, "ingest/1").await, (1, vec![0x0101]));
		assert_eq!(program(&origin, "ingest/2").await, (2, vec![0x0201]));
		assert!(
			matches!(
				origin.consume().request("ingest", None).await,
				Err(moq_net::Error::Unroutable)
			),
			"nothing is published on the bare path"
		);
		drop(publisher.finish(Duration::ZERO).unwrap());
	}

	/// A finished ingest stays servable, its catalog included, for the linger, so a
	/// subscriber arriving after the end still learns what the tracks were.
	#[tokio::test(start_paused = true)]
	async fn a_finished_ingest_serves_its_catalog_for_the_linger() {
		let origin = produce_origin();
		let linger = Duration::from_secs(3);
		let mut publisher = Publisher::new(&origin, "live/cam0", Default::default(), None).unwrap();
		publisher.feed(Bytes::from_static(BBB5S)).unwrap();

		let consumer = origin.consume();
		let broadcast = consumer.follow("live/cam0", None).unwrap().available().await.unwrap();
		let start = moq_clock::now();
		let wait = tokio::spawn(publisher.finish(linger).unwrap().wait());
		moq_clock::sleep(linger / 2).await;
		let mut catalog = broadcast.track("catalog.json").unwrap().subscribe(None).await.unwrap();
		assert!(
			catalog.recv_group().await.unwrap().is_some(),
			"the late subscriber reads the catalog"
		);

		wait.await.unwrap();
		assert_eq!(start.elapsed(), linger);
		assert!(broadcast.is_closed());
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
			None,
		)
		.unwrap();

		let mut ts = psi_packet(0x0000, &pat(&[(1, 0x0100)]));
		ts.extend_from_slice(&psi_packet(0x0100, &pmt(1, 0x1b, 0x0101)));
		publisher.feed(Bytes::from(ts)).unwrap();

		let consumer = origin.consume();
		let broadcast = consumer.follow("live/cam0", None).unwrap().available().await.unwrap();
		let info = broadcast.track("0.avc3").unwrap().info().await.unwrap();
		assert_eq!(info.max_age, Some(Duration::from_secs(3)));
	}

	/// A video PID that goes silent behind a running mux is logged against its ingest path,
	/// and neither the same feed intact nor the audio that kept delivering is.
	#[tokio::test(start_paused = true)]
	async fn publisher_reports_a_silent_pid() {
		const VIDEO: u16 = 0x100;
		// `moq-mux` logs the report, and `traced_test` keeps only this crate's lines. The
		// current-thread runtime runs the ingest on this thread, where this subscriber is.
		#[derive(Clone, Default)]
		struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
		impl std::io::Write for Logs {
			fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
				self.0.lock().unwrap().write(buf)
			}
			fn flush(&mut self) -> std::io::Result<()> {
				Ok(())
			}
		}
		let logs = Logs::default();
		let writer = logs.clone();
		let subscriber = tracing_subscriber::fmt()
			.with_max_level(tracing::Level::INFO)
			.with_ansi(false)
			.with_writer(move || writer.clone())
			.finish();
		let _guard = tracing::subscriber::set_default(subscriber);
		// With one dispatcher registered, tracing caches each callsite's interest from the
		// first thread to hit it, so a parallel test without a subscriber could disable the
		// ingest span for good. A second one leaves every thread to ask its own subscriber.
		let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());

		let origin = produce_origin();
		ingest(&origin, "intact", &[BBB5S]).await;
		let stimulus = suppress(BBB5S, VIDEO, Duration::from_millis(1500), Duration::from_millis(3500));
		ingest(&origin, "silent", &[&stimulus]).await;

		let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
		let stopped: Vec<_> = logs
			.lines()
			.filter(|line| line.contains("stopped delivering access units"))
			.collect();
		let reported = |path: &str, pid: u16| {
			stopped
				.iter()
				.any(|line| line.contains(&format!("path={path}")) && line.contains(&format!("pid={pid} ")))
		};
		assert!(
			reported("silent", VIDEO),
			"the silent video PID was not reported: {stopped:?}"
		);
		assert!(
			!reported("intact", VIDEO),
			"the intact feed's video was reported: {stopped:?}"
		);
		assert!(
			!reported("silent", 0x101) && !reported("intact", 0x101),
			"the audio kept delivering: {stopped:?}"
		);
	}

	/// SRT is a contribution protocol, so SCTE-35 cues survive ingest and egress.
	#[tokio::test(start_paused = true)]
	async fn publisher_preserves_scte35_cues() {
		let origin = produce_origin();
		let mut publisher = Publisher::new(&origin, "ingest", Default::default(), None).unwrap();

		let consumer = origin.consume();
		let broadcast = consumer.follow("ingest", None).unwrap().available().await.unwrap();

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
		let cue = timeout(Duration::from_secs(5), next_frame(&mut reader))
			.await
			.expect("cue read timed out")
			.expect("a published cue section");
		let expected_cue = cue.payload;
		assert_eq!(expected_cue[0], 0xFC, "a verbatim splice_info_section (table_id 0xFC)");

		// The egress joins the clip's last group, read at once and two seconds long. Under the
		// default delay its keyframe is stale, and the egress waits on the next one.
		let options = Options {
			latency: Duration::from_secs(3),
			..Default::default()
		};
		let mut subscriber = Subscriber::new(&origin.consume(), "ingest", options)
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
		drop(publisher.finish(Duration::ZERO).unwrap());

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
		let cue = timeout(Duration::from_secs(5), next_frame(&mut reader))
			.await
			.expect("round-trip cue read timed out")
			.expect("SRT egress preserves a cue section");
		assert_eq!(cue.payload[0], 0xFC, "the round-trip cue is a splice_info_section");
		assert_eq!(
			cue.payload, expected_cue,
			"SRT egress preserves the complete cue section"
		);
	}

	/// The next frame, passing over gaps and ends.
	async fn next_frame(reader: &mut moq_mux::container::Consumer<Container>) -> Option<moq_mux::container::Frame> {
		loop {
			match reader.read().await.unwrap()? {
				moq_mux::container::Read::Frame { frame, .. } => return Some(frame),
				moq_mux::container::Read::Gap { .. } | moq_mux::container::Read::End { .. } => continue,
			}
		}
	}
}
