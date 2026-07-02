//! MPEG-TS demuxer.
//!
//! [`Import`] reads a TS byte stream, reassembles PES packets per PID, and
//! routes their payloads to the codec importers (H.264/H.265/AAC, plus the
//! legacy MP2/AC-3/E-AC-3 verbatim path), which own their broadcast tracks and
//! catalog entries. Elementary streams we don't decode are carried verbatim, one
//! MoQ track per PID, described in the `mpegts` catalog section: PES-framed streams
//! ride the normal PES reassembly, while section-framed streams (SCTE-35 and
//! other private sections, which are not PES) are intercepted before the mpeg2ts
//! reader and reassembled. TS adds PAT/PMT discovery, PES reassembly, the
//! private-section path, and the 90 kHz -> microsecond PTS conversion.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use bytes::BytesMut;
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::PesHeader;
use mpeg2ts::ts::payload::Pes;
use mpeg2ts::ts::{Pid, ReadTsPacket, TsPacket, TsPacketReader, TsPayload};

use super::adts;
use super::catalog;
use crate::catalog::hang::CatalogExt;
use crate::codec::{aac, ac3, eac3, h264, h265, legacy, mp2, opus};
use moq_net::Timestamp;

/// Demuxes an MPEG-TS byte stream into a MoQ broadcast.
///
/// Supports H.264 (stream type 0x1B), H.265 (0x24), ADTS AAC (0x0F), MP2
/// (0x03/0x04), AC-3 (0x81), and E-AC-3 (0x87). LATM/LOAS AAC (0x11) is not
/// ADTS-framed and is dropped. Each codec stream is fed to its importer, which
/// manages the track, catalog config, and keyframe-based group boundaries.
///
/// Elementary streams we don't decode are carried verbatim, one MoQ track per
/// PID, when the catalog `E` carries the [`mpegts`](catalog) section: PES-framed
/// streams ride the normal PES reassembly, section-framed streams (SCTE-35, marked
/// by a program-level 'CUEI' registration descriptor, and other private sections)
/// are intercepted before the reader and reassembled. With a base `Catalog<()>`
/// they're logged and dropped instead.
pub struct Import<E: catalog::Catalog = ()> {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer<E>,

	/// Shared, refillable byte source the persistent reader pulls whole packets
	/// from. Kept beside the reader so [`decode`](Self::decode) can append bytes
	/// between reads without recreating the reader (which holds PAT/PMT state).
	feed: Feed,
	reader: TsPacketReader<Feed>,

	/// PMT PIDs announced by the PAT. With `streams` (the ES PIDs a PMT registers)
	/// these are the only PIDs the reader can route; see [`Self::decode`].
	pmt_pids: HashSet<Pid>,
	/// Per elementary-stream-PID codec routing.
	streams: HashMap<Pid, Stream<E>>,
	/// In-progress PES reassembly, keyed by elementary PID.
	pending: HashMap<Pid, Pending>,
	/// True once a PMT with at least one supported stream has been parsed.
	initialized: bool,
	/// Raw 90 kHz PTS of the first audio frame in the current consecutive run.
	/// TS muxes audio in clumps separated by video, so the span of one run sizes
	/// the audio catalog jitter (see [`AacStream::write`]). Reset on a video frame.
	audio_burst: Option<u64>,

	/// Whole-packet accumulator. Bytes are routed one TS packet at a time
	/// (section-framed verbatim PIDs diverted, the rest fed to the reader); a
	/// trailing partial packet is kept here for the next call.
	scratch: Vec<u8>,
	/// Sync lock. 0x47 is the packet sync byte but also occurs freely in payload (TS
	/// has no byte stuffing), so a lone 0x47 isn't a boundary. False until a candidate
	/// is confirmed by the next packet's sync byte; once true we stride 188 at a time
	/// and trust the per-packet check. Persists across `decode` calls so a candidate
	/// pending confirmation at a buffer tail is re-confirmed, not trusted blindly.
	synced: bool,
	/// Section-framed verbatim PIDs, intercepted before the reader. Private sections
	/// (SCTE-35 table_id 0xFC and others) are not PES, so the reader would
	/// `Pes::read_from` and abort. Keyed by PID. SCTE-35 is detected via the PMT
	/// 'CUEI' registration descriptor.
	sections: HashMap<u16, SectionStream<E>>,
	/// Whether the catalog can carry the `mpegts` section, sampled once at construction.
	/// A base `Catalog<()>` can't, so its undecoded PIDs route to `Stream::Ignored`.
	supports_mpegts: bool,
	/// PMT ES-level descriptors per PID, stashed when a PMT is parsed so a decoded
	/// media track can record them (language, registration, ...) once its track exists.
	es_descriptors: HashMap<u16, Vec<catalog::Descriptor>>,
	/// Decoded media PIDs already recorded into `mpegts.tracks`, so the reconcile in
	/// [`Self::flush`] runs once per track rather than on every frame.
	recorded_media: HashSet<Pid>,
	/// Whether the PMT program-level descriptors have been recorded yet (set once;
	/// PMT `program_info` is stable for the program's life).
	program_recorded: bool,
	/// Latest video PTS: the media clock used to timestamp private sections, which
	/// carry no PES PTS of their own. Unwrapped independently of the video stream.
	/// SPTS scope: one clock for the whole input. Under MPTS every program's video
	/// advances it, so a cue could be stamped with another program's PTS.
	last_pts: Option<Timestamp>,
	media_unwrap: PtsUnwrap,
}

impl<E: catalog::Catalog> Import<E> {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer<E>) -> Self {
		let feed = Feed::default();
		// Sample the real catalog once at construction, not E::default(): an extension
		// may carry the section by value, and a snapshot clones under the mutex (no publish).
		let mut snapshot = catalog.snapshot();
		let supports_mpegts = snapshot.mpegts_mut().is_some();
		Self {
			broadcast,
			catalog,
			reader: TsPacketReader::new(feed.clone()),
			feed,
			pmt_pids: HashSet::new(),
			streams: HashMap::new(),
			pending: HashMap::new(),
			initialized: false,
			audio_burst: None,
			scratch: Vec::new(),
			synced: false,
			sections: HashMap::new(),
			supports_mpegts,
			es_descriptors: HashMap::new(),
			recorded_media: HashSet::new(),
			program_recorded: false,
			last_pts: None,
			media_unwrap: PtsUnwrap::default(),
		}
	}

	/// Append `buf` to the internal scratch and demux every whole TS packet it
	/// now completes. The buffer is fully consumed; a trailing partial packet
	/// (< 188 bytes) is retained for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		self.scratch.extend_from_slice(data);

		// Route one whole packet at a time. Section-framed verbatim PIDs are
		// intercepted here (the reader would PES-parse their sections and abort);
		// every other packet is fed to the reader. Per-packet so a PMT is parsed
		// (and any section PID registered) before the packets that follow it in the
		// same chunk route.
		let mut off = 0;
		while off + TsPacket::SIZE <= self.scratch.len() {
			// A TS packet starts with the 0x47 sync byte. Once synced we trust it and stride
			// 188 at a time; until then (or after a miss) we must (re)acquire the lock.
			if !self.synced || self.scratch[off] != 0x47 {
				self.synced = false;
				// 0x47 also occurs freely in payload (TS has no byte stuffing), so a lone one
				// isn't a boundary. Scan (SIMD via memchr) for a candidate whose next packet
				// also begins with 0x47, confirming the 188 stride before locking onto it.
				// Striding past a false candidate would route one bogus packet; jumping a flat
				// 188 instead would only re-align on exact multiples and could desync forever.
				loop {
					let Some(rel) = memchr::memchr(0x47, &self.scratch[off..]) else {
						// No sync byte left: the buffer is junk, drop it.
						off = self.scratch.len();
						break;
					};
					off += rel;
					match self.scratch.get(off + TsPacket::SIZE) {
						// Next packet also starts with a sync byte: lock onto this candidate.
						Some(&0x47) => {
							self.synced = true;
							break;
						}
						// The byte 188 ahead isn't a sync byte: this 0x47 was payload, keep scanning.
						Some(_) => off += 1,
						// Can't confirm yet (candidate is near the buffer tail). Stay unsynced so
						// it's re-confirmed next call (with the trailing bytes) instead of trusted.
						None => break,
					}
				}
				// Unsynced means the buffer had no confirmable sync byte, or the candidate is
				// pending confirmation; either way there's nothing to route until more arrives.
				if !self.synced {
					break;
				}
				continue;
			}
			let pkt: [u8; TsPacket::SIZE] = self.scratch[off..off + TsPacket::SIZE].try_into().unwrap();
			off += TsPacket::SIZE;
			let pid = (((pkt[1] & 0x1f) as u16) << 8) | pkt[2] as u16;
			let pts = self.last_pts.unwrap_or(Timestamp::ZERO);
			if let Some(section) = self.sections.get_mut(&pid) {
				section.packet(&pkt, pts)?;
				continue;
			}
			// PIDs we don't decode and don't carry (`Stream::Ignored`: a base catalog's
			// undecoded streams, or an ambiguous 0x86 PID without CUEI) are dropped here,
			// not fed to the PES reader, which aborts on private sections (spec section 7:
			// never fatal).
			if let Ok(p) = Pid::new(pid)
				&& matches!(self.streams.get(&p), Some(Stream::Ignored))
			{
				continue;
			}
			// Feed the reader only PIDs it can route: the PAT, the PMT PIDs it
			// announces, and the ES PIDs a PMT registers. A live capture joins
			// mid-stream, so PES arrive before their PSI; feeding those would make
			// the reader abort on an unknown PID. Drop them until the layout is
			// learned (PSI repeats), then normal demux resumes.
			if pid != Pid::PAT
				&& !Pid::new(pid).is_ok_and(|p| self.pmt_pids.contains(&p) || self.streams.contains_key(&p))
			{
				continue;
			}
			{
				let mut state = self.feed.lock();
				state.data.clear();
				state.data.extend_from_slice(&pkt);
				state.pos = 0;
			}
			while let Some(packet) = self.reader.read_ts_packet()? {
				self.handle_packet(packet)?;
			}
		}

		self.scratch.drain(..off);
		Ok(())
	}

	fn handle_packet(&mut self, packet: TsPacket) -> anyhow::Result<()> {
		let pid = packet.header.pid;
		match packet.payload {
			Some(TsPayload::Pmt(pmt)) => {
				// SCTE-35 is announced by a program-level registration descriptor with
				// format_identifier 'CUEI' (ITU-T J.181). The stream itself uses
				// stream_type 0x86, which mpeg2ts maps to a DTS audio variant, so
				// detection keys off the CUEI descriptor, not the stream type alone.
				let cuei = pmt
					.program_info
					.iter()
					.any(|d| d.tag == 0x05 && d.data.len() >= 4 && &d.data[0..4] == b"CUEI");

				// Record the program-level descriptors once (PMT program_info is stable);
				// export re-emits them verbatim, including the original CUEI.
				if self.supports_mpegts && !self.program_recorded && !pmt.program_info.is_empty() {
					let program = to_descriptors(&pmt.program_info);
					if let Some(mpegts) = self.catalog.lock().mpegts_mut() {
						mpegts.program_descriptors = program;
					}
					self.program_recorded = true;
				}

				for es in &pmt.es_info {
					let stream_type = es.stream_type as u8;
					// Stash ES descriptors so a decoded media track can record them once its
					// (lazily created) track exists; verbatim streams record their own.
					if self.supports_mpegts {
						self.es_descriptors
							.insert(es.elementary_pid.as_u16(), to_descriptors(&es.descriptors));
					}
					// Section-framed private data is intercepted before the reader (which
					// aborts on private sections): private sections (0x05) and CUEI-marked
					// SCTE-35 (0x86). Everything else routes through ensure_stream (a decoded
					// codec, PES-framed verbatim, or dropped).
					if stream_type == 0x05 || (cuei && matches!(es.stream_type, StreamType::Dts8ChannelLosslessAudio)) {
						self.ensure_section(es.elementary_pid, stream_type, &es.descriptors)?;
					} else {
						self.ensure_stream(es.elementary_pid, es.stream_type, &es.descriptors)?;
					}
				}
			}
			Some(TsPayload::PesStart(pes)) => self.handle_pes_start(pid, pes)?,
			Some(TsPayload::PesContinuation(bytes)) => self.handle_pes_continuation(pid, &bytes)?,
			// Learn the PMT PIDs so the routing gate in `decode` lets them through.
			Some(TsPayload::Pat(pat)) => {
				self.pmt_pids
					.extend(pat.table.iter().map(|entry| entry.program_map_pid));
			}
			_ => {}
		}
		Ok(())
	}

	fn ensure_stream(
		&mut self,
		pid: Pid,
		stream_type: StreamType,
		descriptors: &[mpeg2ts::ts::Descriptor],
	) -> anyhow::Result<()> {
		// A later PMT can remap a PID that was section-framed (intercepted in
		// `decode`) to a PES codec/verbatim stream. Drop the stale section route first,
		// or it would keep intercepting the PID and the new stream would never get data.
		// This only fires on a genuine remap: section PIDs otherwise route to
		// `ensure_section`, never here.
		if let Some(mut section) = self.sections.remove(&pid.as_u16()) {
			section.finish()?;
			self.pending.remove(&pid);
		}
		if self.streams.contains_key(&pid) {
			return Ok(());
		}

		let stream = match stream_type {
			StreamType::H264 => {
				let track = crate::import::unique_track(&mut self.broadcast, ".avc3")?;
				Stream::H264 {
					split: h264::Split::new(),
					import: Box::new(h264::Import::new(track, self.catalog.clone())),
					unwrap: PtsUnwrap::default(),
				}
			}
			StreamType::H265 => {
				let track = crate::import::unique_track(&mut self.broadcast, ".hev1")?;
				Stream::H265 {
					split: h265::Split::new(),
					import: Box::new(h265::Import::new(track, self.catalog.clone())),
					unwrap: PtsUnwrap::default(),
				}
			}
			// Only ADTS-framed AAC (0x0F). 0x11 is LATM/LOAS, which uses a different
			// framing and syncword, so it falls through to the ignored arm below.
			StreamType::AdtsAac => Stream::Aac(Box::new(AacStream {
				import: None,
				broadcast: self.broadcast.clone(),
				catalog: self.catalog.clone(),
				unwrap: PtsUnwrap::default(),
				jitter: None,
			})),
			// Legacy broadcast audio, carried verbatim. Both MP2 stream types
			// (0x03 MPEG-1, 0x04 MPEG-2 half rate) share one parser; sample rate and
			// channels always come from the frame header, not the PMT.
			StreamType::Mpeg1Audio | StreamType::Mpeg2HalvedSampleRateAudio => self.legacy_stream(&mp2::DESCRIPTOR),
			StreamType::DolbyDigitalUpToSixChannelAudio => self.legacy_stream(&ac3::DESCRIPTOR),
			StreamType::DolbyDigitalPlusUpTo16ChannelAudioForAtsc => self.legacy_stream(&eac3::DESCRIPTOR),
			// Opus rides private-data PES (0x06), distinguished from other private streams
			// by an 'Opus' registration descriptor. Channels and the (always 48 kHz) rate
			// come from the descriptors, so the importer is built up front.
			StreamType::Mpeg2PacketizedData if registration_format(descriptors) == Some(*b"Opus") => {
				let channel_count = opus_channel_count(descriptors).unwrap_or(2);
				let track = crate::import::unique_track(&mut self.broadcast, ".opus")?;
				let config = opus::Config {
					sample_rate: 48_000,
					channel_count,
				};
				Stream::Opus(Box::new(OpusStream {
					import: opus::Import::new(track, self.catalog.clone(), config)?,
					unwrap: PtsUnwrap::default(),
				}))
			}
			StreamType::Mpeg1Video | StreamType::Mpeg2Video => Stream::Clock,
			// A codec we don't decode. Carry it verbatim as PES when the catalog supports
			// the `mpegts` section. 0x86 is excluded: it's ambiguous (DTS audio, or a
			// non-conformant SCTE-35 mux without CUEI, which is sections the PES reader
			// would abort on), so drop it rather than risk feeding sections to the reader.
			other => {
				if self.supports_mpegts && !matches!(other, StreamType::Dts8ChannelLosslessAudio) {
					let descriptors = to_descriptors(descriptors);
					match VerbatimStream::new(
						self.broadcast.clone(),
						self.catalog.clone(),
						pid.as_u16(),
						stream_type as u8,
						descriptors,
					) {
						Ok(stream) => Stream::Verbatim(Box::new(stream)),
						Err(err) => {
							tracing::warn!(?err, pid = pid.as_u16(), "failed to create verbatim stream, dropping");
							Stream::Ignored
						}
					}
				} else {
					tracing::warn!(?other, pid = pid.as_u16(), "unsupported TS stream type, dropping");
					Stream::Ignored
				}
			}
		};

		// Clock is not a decodable track, so it doesn't initialize the importer.
		if !matches!(stream, Stream::Ignored | Stream::Clock) {
			self.initialized = true;
		}
		self.streams.insert(pid, stream);
		Ok(())
	}

	fn legacy_stream(&self, descriptor: &'static legacy::Descriptor) -> Stream<E> {
		Stream::Legacy(Box::new(LegacyStream {
			descriptor,
			import: None,
			broadcast: self.broadcast.clone(),
			catalog: self.catalog.clone(),
			unwrap: PtsUnwrap::default(),
			tail: Vec::new(),
			tail_pts: None,
		}))
	}

	/// Register a section-framed verbatim PID (SCTE-35 or other private sections):
	/// intercepted (see [`Self::decode`]) with a verbatim track when the catalog
	/// carries the `mpegts` section, dropped as `Ignored` when it can't.
	fn ensure_section(
		&mut self,
		pid: Pid,
		stream_type: u8,
		descriptors: &[mpeg2ts::ts::Descriptor],
	) -> anyhow::Result<()> {
		if self.sections.contains_key(&pid.as_u16()) {
			return Ok(());
		}
		// This PID is becoming section-framed; drop any partial PES a prior codec left pending.
		self.pending.remove(&pid);
		if !self.supports_mpegts {
			// Always route to Ignored, replacing any prior codec on this PID (a later PMT
			// can reassign it), so a private section never reaches the PES reader. Warn once.
			if !matches!(self.streams.insert(pid, Stream::Ignored), Some(Stream::Ignored)) {
				tracing::warn!(
					pid = pid.as_u16(),
					"private section stream detected without `mpegts` catalog support; dropping"
				);
			}
			return Ok(());
		}
		// A prior PMT may have routed this PID to Ignored; drop it so the PID has one route.
		self.streams.remove(&pid);
		let descriptors = to_descriptors(descriptors);
		let stream = SectionStream::new(
			self.broadcast.clone(),
			self.catalog.clone(),
			pid.as_u16(),
			stream_type,
			descriptors,
		)?;
		self.sections.insert(pid.as_u16(), stream);
		self.initialized = true;
		tracing::debug!(
			pid = pid.as_u16(),
			stream_type,
			"private section stream detected; intercepting before the reader"
		);
		Ok(())
	}

	fn handle_pes_start(&mut self, pid: Pid, pes: Pes) -> anyhow::Result<()> {
		// A new PES start means the previous one for this PID is complete.
		if self.pending.contains_key(&pid) {
			self.flush(pid)?;
		}

		let Some(stream) = self.streams.get(&pid) else {
			// PES before its PMT entry; ignore until the layout is known.
			return Ok(());
		};

		// A video PES arriving marks the end of any preceding audio run; audio is
		// muxed into the gaps between video frames. Resetting here (on delivery)
		// rather than only on the video flush avoids over-counting the startup run,
		// since unbounded video PES don't flush until the next one starts.
		let is_video = matches!(stream, Stream::H264 { .. } | Stream::H265 { .. } | Stream::Clock);
		let is_clock = matches!(stream, Stream::Clock);
		if is_video {
			self.audio_burst = None;
			// Advance the media clock here, not at flush: unbounded video only
			// flushes on the next PES, so a SCTE-35 section arriving during this
			// frame must be timestamped with this frame's PTS ("now"), not the
			// previous one's.
			if pes.header.pts.is_some() {
				self.last_pts = unwrap_pts(&mut self.media_unwrap, pes.header.pts.map(|t| t.as_u64()))?;
			}
		}

		if is_clock {
			return Ok(());
		}

		let data_len = pes_data_len(&pes.header, pes.pes_packet_len);
		let mut pending = Pending {
			pts: pes.header.pts.map(|t| t.as_u64()),
			dts: pes.header.dts.map(|t| t.as_u64()),
			stream_id: pes.header.stream_id.as_u8(),
			data: Vec::with_capacity(pes.data.len()),
			data_len,
		};
		pending.data.extend_from_slice(&pes.data);
		let complete = matches!(data_len, Some(len) if pending.data.len() >= len);
		self.pending.insert(pid, pending);

		if complete {
			self.flush(pid)?;
		}
		Ok(())
	}

	fn handle_pes_continuation(&mut self, pid: Pid, data: &[u8]) -> anyhow::Result<()> {
		let Some(pending) = self.pending.get_mut(&pid) else {
			return Ok(());
		};
		pending.data.extend_from_slice(data);
		if matches!(pending.data_len, Some(len) if pending.data.len() >= len) {
			self.flush(pid)?;
		}
		Ok(())
	}

	fn flush(&mut self, pid: Pid) -> anyhow::Result<()> {
		let Some(pending) = self.pending.remove(&pid) else {
			return Ok(());
		};

		// Track the start of the current consecutive audio run (audio PTS since the
		// last video frame), so the audio stream can size its jitter to the burst.
		// Only AAC consumes the jitter hint, so only AAC anchors the run: a legacy
		// audio stream opening it would re-anchor AAC's span on a foreign PTS,
		// inflating the published jitter by the inter-PID PTS offset.
		let is_video = matches!(self.streams.get(&pid), Some(Stream::H264 { .. } | Stream::H265 { .. }));
		let run_start = if is_video {
			self.audio_burst = None;
			None
		} else if matches!(self.streams.get(&pid), Some(Stream::Aac(_))) {
			pending.pts.map(|audio| *self.audio_burst.get_or_insert(audio))
		} else {
			None
		};

		let Some(stream) = self.streams.get_mut(&pid) else {
			return Ok(());
		};
		stream.write(pending, run_start)?;

		// Record the decoded media track's PID + PMT descriptors (language, ...) once
		// its lazily created track exists, so export can preserve them.
		self.record_media_track(pid);
		Ok(())
	}

	/// Record a decoded media stream's PID and ES descriptors into `mpegts.tracks`,
	/// once per track. No-op without the `mpegts` section, before the track exists,
	/// or for verbatim streams (which self-register).
	fn record_media_track(&mut self, pid: Pid) {
		if !self.supports_mpegts || self.recorded_media.contains(&pid) {
			return;
		}
		let (name, descriptors) = {
			let Some(name) = self.streams.get(&pid).and_then(|s| s.media_track_name()) else {
				return;
			};
			(
				name,
				self.es_descriptors.get(&pid.as_u16()).cloned().unwrap_or_default(),
			)
		};
		if let Some(mpegts) = self.catalog.lock().mpegts_mut() {
			let entry = mpegts
				.tracks
				.entry(name)
				.or_insert_with(|| catalog::Track::new(pid.as_u16()));
			entry.pid = pid.as_u16();
			entry.descriptors = descriptors;
		}
		self.recorded_media.insert(pid);
	}

	/// Close the current group on every track and reopen at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		for stream in self.streams.values_mut() {
			stream.seek(sequence)?;
		}
		for section in self.sections.values_mut() {
			section.seek(sequence)?;
		}
		Ok(())
	}

	/// Flush any buffered PES and finish every track.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		let pids: Vec<Pid> = self.pending.keys().copied().collect();
		for pid in pids {
			self.flush(pid)?;
		}
		for stream in self.streams.values_mut() {
			stream.finish()?;
		}
		for section in self.sections.values_mut() {
			section.finish()?;
		}
		Ok(())
	}
}

/// A reassembled PES packet awaiting routing to its codec importer.
struct Pending {
	/// Raw 90 kHz PTS, before wrap-unwrapping.
	pts: Option<u64>,
	/// Raw 90 kHz DTS, before wrap-unwrapping. Present on reordered (B-frame) video; its
	/// distance below the PTS is the reorder delay published as the catalog jitter.
	dts: Option<u64>,
	/// PES stream_id, preserved for verbatim PES carriage.
	stream_id: u8,
	data: Vec<u8>,
	/// Expected payload length for bounded PES, else `None` (unbounded video).
	data_len: Option<usize>,
}

/// Convert mpeg2ts PMT descriptors to the catalog's verbatim form.
fn to_descriptors(descriptors: &[mpeg2ts::ts::Descriptor]) -> Vec<catalog::Descriptor> {
	descriptors
		.iter()
		.map(|d| catalog::Descriptor {
			tag: d.tag,
			data: bytes::Bytes::copy_from_slice(&d.data),
		})
		.collect()
}

/// Create a verbatim track and record it in the `mpegts` catalog section as a
/// [`Track`](catalog::Track) with a `verbatim` carriage record. Shared by the
/// section- and PES-framed paths.
fn register_verbatim<E: catalog::Catalog>(
	broadcast: &mut moq_net::BroadcastProducer,
	catalog: &mut crate::catalog::Producer<E>,
	pid: u16,
	stream_type: u8,
	framing: catalog::Framing,
	descriptors: Vec<catalog::Descriptor>,
) -> anyhow::Result<crate::container::Producer<crate::catalog::hang::Container>> {
	// Verbatim payloads ride the legacy container, which normalizes the per-frame
	// timestamp to microseconds on the wire (see `hang::container::Frame::encode`),
	// so the track declares that timescale to match.
	let track = broadcast.unique_track(
		".ts",
		moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
	)?;

	let mut guard = catalog.lock();
	let Some(mpegts) = guard.mpegts_mut() else {
		// supports_mpegts was true when sampled at construction; None here means the
		// catalog dropped the section since.
		anyhow::bail!("catalog extension no longer carries an mpegts section");
	};
	mpegts.tracks.insert(
		track.name().to_string(),
		catalog::Track {
			pid,
			descriptors,
			verbatim: Some(catalog::Verbatim::new(stream_type, framing)),
		},
	);
	drop(guard);

	Ok(crate::container::Producer::new(
		track,
		crate::catalog::hang::Container::Legacy,
	))
}

/// Remove a verbatim track's entry from the `mpegts` catalog section on drop.
fn unregister_verbatim<E: catalog::Catalog>(catalog: &mut crate::catalog::Producer<E>, name: &str) {
	if let Some(mpegts) = catalog.lock().mpegts_mut() {
		mpegts.tracks.remove(name);
	}
}

/// Publishes reassembled private sections (SCTE-35 and others) as verbatim frames
/// on a track described in the `mpegts` catalog section.
///
/// Private sections (e.g. SCTE-35 table_id 0xFC) are not PES, so this PID is
/// intercepted before the mpeg2ts reader (which would PES-parse it and abort).
/// The byte-level reassembly lives in [`SectionReassembler`]; this type owns the
/// track and catalog entry and stamps each section with the media clock.
struct SectionStream<E: catalog::Catalog> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	catalog: crate::catalog::Producer<E>,
	reassembler: SectionReassembler,
}

impl<E: catalog::Catalog> SectionStream<E> {
	fn new(
		mut broadcast: moq_net::BroadcastProducer,
		mut catalog: crate::catalog::Producer<E>,
		pid: u16,
		stream_type: u8,
		descriptors: Vec<catalog::Descriptor>,
	) -> anyhow::Result<Self> {
		let track = register_verbatim(
			&mut broadcast,
			&mut catalog,
			pid,
			stream_type,
			catalog::Framing::Section,
			descriptors,
		)?;
		Ok(Self {
			track,
			catalog,
			reassembler: SectionReassembler::default(),
		})
	}

	/// Consume one 188-byte TS packet, publishing each completed section. `pts` is
	/// the current media clock used to timestamp a section (its arrival on the
	/// timeline; the splice time itself is inside the section bytes).
	fn packet(&mut self, pkt: &[u8], pts: Timestamp) -> anyhow::Result<()> {
		let mut sections = Vec::new();
		self.reassembler.push(pkt, &mut sections);
		for section in sections {
			self.emit(section, pts)?;
		}
		Ok(())
	}

	/// Publish one complete section as a frame in its own group.
	fn emit(&mut self, section: Vec<u8>, pts: Timestamp) -> anyhow::Result<()> {
		let frame = crate::container::Frame {
			timestamp: pts,
			duration: None,
			payload: bytes::Bytes::from(section),
			keyframe: true,
		};
		self.track.write(frame)?;
		self.track.finish_group()?;
		Ok(())
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		self.track.finish()?;
		Ok(())
	}
}

impl<E: catalog::Catalog> Drop for SectionStream<E> {
	fn drop(&mut self) {
		let name = self.track.name().to_string();
		unregister_verbatim(&mut self.catalog, &name);
	}
}

/// Publishes whole reassembled PES payloads verbatim as frames on a track
/// described in the `mpegts` catalog section, for elementary streams we don't decode
/// (DTS audio, private PES, teletext, ...).
///
/// Unlike [`SectionStream`], these ride the normal PES reassembly path, so this
/// type only stamps each PES payload with its (unwrapped) PTS and writes it.
struct VerbatimStream<E: catalog::Catalog> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	catalog: crate::catalog::Producer<E>,
	unwrap: PtsUnwrap,
	/// Whether the PES stream_id has been recorded into the catalog yet (once).
	stream_id_recorded: bool,
}

impl<E: catalog::Catalog> VerbatimStream<E> {
	fn new(
		mut broadcast: moq_net::BroadcastProducer,
		mut catalog: crate::catalog::Producer<E>,
		pid: u16,
		stream_type: u8,
		descriptors: Vec<catalog::Descriptor>,
	) -> anyhow::Result<Self> {
		let track = register_verbatim(
			&mut broadcast,
			&mut catalog,
			pid,
			stream_type,
			catalog::Framing::Pes,
			descriptors,
		)?;
		Ok(Self {
			track,
			catalog,
			unwrap: PtsUnwrap::default(),
			stream_id_recorded: false,
		})
	}

	/// Publish one reassembled PES payload verbatim, in its own group, stamped with
	/// its PTS (or zero when the PES carried none).
	fn write(&mut self, pending: Pending) -> anyhow::Result<()> {
		// Record the original PES stream_id once, from the first PES, so export
		// re-emits the stream under its real id (e.g. 0xBD for teletext/DVB AC-3).
		if !self.stream_id_recorded {
			let name = self.track.name().to_string();
			if let Some(mpegts) = self.catalog.lock().mpegts_mut()
				&& let Some(verbatim) = mpegts.tracks.get_mut(&name).and_then(|t| t.verbatim.as_mut())
			{
				verbatim.stream_id = Some(pending.stream_id);
			}
			self.stream_id_recorded = true;
		}

		let pts = unwrap_pts(&mut self.unwrap, pending.pts)?.unwrap_or(Timestamp::ZERO);
		let frame = crate::container::Frame {
			timestamp: pts,
			duration: None,
			payload: bytes::Bytes::from(pending.data),
			keyframe: true,
		};
		self.track.write(frame)?;
		self.track.finish_group()?;
		Ok(())
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		self.track.finish()?;
		Ok(())
	}
}

impl<E: catalog::Catalog> Drop for VerbatimStream<E> {
	fn drop(&mut self) {
		let name = self.track.name().to_string();
		unregister_verbatim(&mut self.catalog, &name);
	}
}

/// Byte-level reassembler for MPEG-TS private sections on one PID.
///
/// Private sections (SCTE-35 table_id 0xFC and others) are not PES. This handles
/// pointer_field alignment, sections split across packets (including a 3-byte
/// header split, where section_length is not yet known), continuity-counter
/// gaps, and adaptation-field discontinuities. Deliberately private and minimal:
/// just enough to recover whole sections verbatim.
#[derive(Default)]
struct SectionReassembler {
	/// Bytes of the section currently being reassembled. Its 3-byte header (and
	/// thus section_length) may not all be present yet, so completeness is
	/// re-checked as bytes arrive; empty means no section in progress.
	acc: Vec<u8>,
	/// Last continuity_counter seen on a packet with payload, to spot gaps.
	last_cc: Option<u8>,
	/// Last payload packet, to skip ISO 13818-1 duplicates (same cc, identical bytes).
	last_pkt: Option<[u8; 188]>,
}

impl SectionReassembler {
	/// Consume one 188-byte TS packet, appending every completed section to `out`.
	fn push(&mut self, pkt: &[u8], out: &mut Vec<Vec<u8>>) {
		// transport_error_indicator: the demodulator flagged this packet as corrupt,
		// so its payload can't be trusted (and we don't validate CRC-32). Drop it and
		// any partial; resync at the next clean PUSI.
		if pkt[1] & 0x80 != 0 {
			self.acc.clear();
			self.last_cc = None;
			self.last_pkt = None;
			return;
		}

		let pusi = pkt[1] & 0x40 != 0;
		let afc = (pkt[3] >> 4) & 0x3;
		let cc = pkt[3] & 0x0f;
		let has_payload = afc & 0x1 != 0;

		// Parse the adaptation field before the no-payload early return: a
		// discontinuity can ride on an adaptation-only packet and must still reset
		// reassembly.
		let mut off = 4;
		let mut discontinuity = false;
		if afc & 0x2 != 0 {
			let af_len = pkt[4] as usize;
			discontinuity = af_len > 0 && pkt[5] & 0x80 != 0;
			off = 5 + af_len;
		}

		if !has_payload {
			// An adaptation-only discontinuity still drops the partial; forgetting the
			// counter keeps the next payload packet from looking like a gap.
			if discontinuity {
				self.acc.clear();
				self.last_cc = None;
				self.last_pkt = None;
			}
			return;
		}

		// ISO 13818-1 permits one identical retransmission of a payload packet (same
		// cc, same bytes); processing it would reset a healthy partial or re-emit a
		// completed section. Skip it, recording this packet to catch the next.
		if self.last_pkt.as_ref().is_some_and(|last| last[..] == pkt[..]) {
			return;
		}
		self.last_pkt = pkt.try_into().ok();

		// A continuity-counter gap (only payload packets advance it) or a declared
		// discontinuity both mean the in-progress section is lost.
		let cc_gap = matches!(self.last_cc, Some(last) if cc != (last + 1) & 0x0f);
		let reset = discontinuity || cc_gap;
		if reset {
			self.acc.clear();
		}
		self.last_cc = Some(cc);

		if off >= pkt.len() {
			return;
		}
		let payload = &pkt[off..];

		if pusi {
			// pointer_field: payload[1..1+ptr] is the tail of the section already in
			// progress; a fresh section starts at 1+ptr.
			let ptr = payload[0] as usize;
			if 1 + ptr > payload.len() {
				// pointer_field points past the payload: malformed packet. Drop the
				// partial and resync at the next PUSI rather than slicing out of bounds
				// or treating the bytes as a valid continuation.
				self.acc.clear();
				return;
			}
			if !self.acc.is_empty() {
				// Complete the section in progress with its tail. With nothing in
				// progress (stream just joined, or a reset dropped the partial) these
				// bytes are an orphaned fragment, so skip straight to the pointer_field.
				self.acc.extend_from_slice(&payload[1..1 + ptr]);
				self.drain(out);
			}
			// The pointer_field is a hard section boundary: drop any leftover partial
			// and start the section it points to.
			self.acc.clear();
			self.acc.extend_from_slice(&payload[1 + ptr..]);
			self.drain(out);
		} else if !self.acc.is_empty() {
			// Continuation of the section in progress. A non-PUSI packet with nothing
			// in progress is unaligned (no pointer_field to resync on), so it is
			// ignored until the next PUSI. This keeps us desynced after a gap,
			// discontinuity, or corrupt pointer dropped the partial, rather than
			// resuming on stray bytes that merely look like a section.
			self.acc.extend_from_slice(payload);
			self.drain(out);
		}
	}

	/// Move every complete section out of `acc` into `out`, stopping at the first
	/// partial. The 3-byte header (which holds section_length) can itself be split
	/// across TS packets, so a short buffer waits for more bytes rather than being
	/// dropped. Every complete section is carried verbatim (SCTE-35 and any other
	/// private-section table on the PID); only 0xff stuffing is dropped.
	fn drain(&mut self, out: &mut Vec<Vec<u8>>) {
		loop {
			match self.acc.first() {
				None => return,
				// table_id 0xff is stuffing: the rest of the section area is padding.
				Some(&0xff) => {
					self.acc.clear();
					return;
				}
				_ => {}
			}
			if self.acc.len() < 3 {
				return;
			}
			let section_length = (((self.acc[1] & 0x0f) as usize) << 8) | self.acc[2] as usize;
			// section_length tops out at 4093 per spec (12-bit field, top 2 bits zero). A
			// larger value means we are misparsing garbage, so drop and resync at the next
			// pointer_field rather than buffering up to ~4 KB of junk.
			if section_length > 4093 {
				self.acc.clear();
				return;
			}
			let full = 3 + section_length;
			if self.acc.len() < full {
				return;
			}
			out.push(self.acc.drain(..full).collect());
		}
	}
}

/// One elementary stream's codec importer plus PTS-unwrap state.
enum Stream<E: catalog::Catalog = ()> {
	H264 {
		split: h264::Split,
		import: Box<h264::Import<E>>,
		unwrap: PtsUnwrap,
	},
	H265 {
		split: h265::Split,
		import: Box<h265::Import<E>>,
		unwrap: PtsUnwrap,
	},
	Aac(Box<AacStream<E>>),
	Opus(Box<OpusStream<E>>),
	Legacy(Box<LegacyStream<E>>),
	/// A codec we don't decode, carried verbatim as PES (DTS audio, private PES, ...).
	Verbatim(Box<VerbatimStream<E>>),
	/// MPEG-1/2 video we don't decode, kept only to advance the media clock.
	/// `is_video` counts it, so never reuse this variant for audio or data.
	Clock,
	Ignored,
}

impl<E: catalog::Catalog> Stream<E> {
	fn write(&mut self, pending: Pending, burst: Option<u64>) -> anyhow::Result<()> {
		match self {
			Stream::H264 { split, import, unwrap } => {
				let reorder = reorder_delay(pending.pts, pending.dts);
				let pts = unwrap_pts(unwrap, pending.pts)?;
				skip_missing_keyframe((|| {
					// Each PES is one access unit, so flush to emit it immediately.
					let mut frames = split.decode(&pending.data, pts)?;
					frames.extend(split.flush(pts)?);
					import.decode(frames)
				})())?;
				// After decode, so the track (and its catalog rendition) exists.
				if let Some(reorder) = reorder {
					import.observe_reorder(reorder);
				}
				Ok(())
			}
			Stream::H265 { split, import, unwrap } => {
				let reorder = reorder_delay(pending.pts, pending.dts);
				let pts = unwrap_pts(unwrap, pending.pts)?;
				skip_missing_keyframe((|| {
					// Each PES is one access unit, so flush to emit it immediately.
					let mut frames = split.decode(&pending.data, pts)?;
					frames.extend(split.flush(pts)?);
					import.decode(frames)
				})())?;
				if let Some(reorder) = reorder {
					import.observe_reorder(reorder);
				}
				Ok(())
			}
			Stream::Aac(stream) => stream.write(pending, burst),
			Stream::Opus(stream) => stream.write(pending),
			Stream::Legacy(stream) => stream.write(pending),
			Stream::Verbatim(stream) => stream.write(pending),
			Stream::Clock | Stream::Ignored => Ok(()),
		}
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		match self {
			Stream::H264 { split, import, .. } => {
				split.reset();
				Ok(import.seek(sequence)?)
			}
			Stream::H265 { split, import, .. } => {
				split.reset();
				Ok(import.seek(sequence)?)
			}
			Stream::Aac(stream) => stream.seek(sequence),
			Stream::Opus(stream) => stream.seek(sequence),
			Stream::Legacy(stream) => stream.seek(sequence),
			Stream::Verbatim(stream) => stream.seek(sequence),
			Stream::Clock | Stream::Ignored => Ok(()),
		}
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		match self {
			Stream::H264 { import, .. } => Ok(import.finish()?),
			Stream::H265 { import, .. } => Ok(import.finish()?),
			Stream::Aac(stream) => stream.finish(),
			Stream::Opus(stream) => stream.finish(),
			Stream::Legacy(stream) => stream.finish(),
			Stream::Verbatim(stream) => stream.finish(),
			Stream::Clock | Stream::Ignored => Ok(()),
		}
	}

	/// The MoQ track name of a decoded media stream, once its (lazily created) track
	/// exists. `None` for verbatim/clock/ignored streams (verbatim self-registers).
	fn media_track_name(&self) -> Option<String> {
		match self {
			Stream::H264 { import, .. } => Some(import.name().to_string()),
			Stream::H265 { import, .. } => Some(import.name().to_string()),
			Stream::Aac(stream) => stream.import.as_ref().map(|i| i.name().to_string()),
			Stream::Opus(stream) => Some(stream.import.name().to_string()),
			Stream::Legacy(stream) => stream.import.as_ref().map(|i| i.name().to_string()),
			Stream::Verbatim(_) | Stream::Clock | Stream::Ignored => None,
		}
	}
}

/// AAC needs the first ADTS header before it can build a [`aac::Import`]
/// (the sample rate and channel layout aren't in the PMT), so creation is
/// deferred until the first frame arrives.
struct AacStream<E: CatalogExt = ()> {
	import: Option<aac::Import<E>>,
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer<E>,
	unwrap: PtsUnwrap,
	/// Largest audio burst span seen, published as the catalog jitter.
	jitter: Option<Timestamp>,
}

impl<E: CatalogExt> AacStream<E> {
	fn write(&mut self, pending: Pending, run_start: Option<u64>) -> anyhow::Result<()> {
		let base = unwrap_pts(&mut self.unwrap, pending.pts)?;

		// A single PES can carry several ADTS frames; split and feed each raw frame.
		let data = &pending.data;
		let mut offset = 0;
		let mut index = 0u64;
		let mut sample_rate = None;
		while offset + 7 <= data.len() {
			let header = adts::Header::parse(&data[offset..])?;
			let end = offset + header.frame_len;
			anyhow::ensure!(end <= data.len(), "ADTS frame exceeds PES payload");
			sample_rate = Some(header.sample_rate);

			let import = match &mut self.import {
				Some(import) => import,
				None => {
					let config = aac::Config {
						profile: header.object_type,
						sample_rate: header.sample_rate,
						channel_count: header.channel_count,
					};
					// Synthesize the AudioSpecificConfig from the first ADTS header so
					// downstream consumers that need out-of-band config (fMP4/MKV export,
					// WebCodecs) can configure the decoder. TS itself carries it inline.
					let description = config.encode();
					let track = crate::import::unique_track(&mut self.broadcast, ".aac")?;
					let mut aac = aac::Import::new(track, self.catalog.clone(), config)?;
					aac.update_rendition(|rendition| rendition.description = Some(description));
					self.import.insert(aac)
				}
			};

			// Each frame after the first in this PES advances by 1024 samples.
			let pts = match base {
				Some(base) if index > 0 => {
					let advance = Timestamp::from_scale(index * 1024, header.sample_rate as u64)?;
					// `base` is a 90 kHz PTS; rescale the sample-rate advance to match
					// before adding (the scale-aware Timestamp rejects mixed scales).
					Some(base + advance.convert(base.scale())?)
				}
				other => other,
			};

			import.decode(&data[offset + header.header_len..end], pts)?;

			offset = end;
			index += 1;
		}

		self.update_jitter(run_start, pending.pts, index, sample_rate)
	}

	/// Size the catalog jitter to the TS audio burst. MPEG-TS delivers audio in
	/// clumps (several ADTS frames per PES, and runs of audio PES between video)
	/// rather than one frame at a time, so without a matching jitter hint the
	/// player under-buffers audio and stutters between bursts. The burst is the
	/// PTS span from the start of the current audio run to this PES's last frame.
	fn update_jitter(
		&mut self,
		run_start: Option<u64>,
		pes_pts: Option<u64>,
		frames: u64,
		sample_rate: Option<u32>,
	) -> anyhow::Result<()> {
		let (Some(start), Some(pts), Some(rate)) = (run_start, pes_pts, sample_rate) else {
			return Ok(());
		};
		if frames == 0 {
			return Ok(());
		}

		let frame = 1024 * 90_000 / rate as u64;
		// Span from the run start through this PES's frames, plus one frame slack.
		let span = pts.saturating_sub(start) + frames * frame;
		// Ignore implausible spans (e.g. across a 33-bit PTS wrap).
		if span > 90_000 * 4 {
			return Ok(());
		}

		let jitter = Timestamp::from_scale(span, 90_000)?;
		if jitter <= self.jitter.unwrap_or(Timestamp::ZERO) {
			return Ok(());
		}
		self.jitter = Some(jitter);

		if let Some(import) = &mut self.import {
			import.update_rendition(|rendition| rendition.jitter = Some(jitter.into()));
		}
		Ok(())
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		if let Some(import) = &mut self.import {
			import.seek(sequence)?;
		}
		Ok(())
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		if let Some(import) = &mut self.import {
			import.finish()?;
		}
		Ok(())
	}
}

/// One Opus elementary stream. The channels come from the PMT descriptors and the rate
/// is always 48 kHz, so (unlike AAC) the importer is built up front. A PES carries one or
/// more Opus packets, each prefixed by the Opus-in-TS control header.
struct OpusStream<E: CatalogExt = ()> {
	import: opus::Import<E>,
	unwrap: PtsUnwrap,
}

impl<E: CatalogExt> OpusStream<E> {
	fn write(&mut self, pending: Pending) -> anyhow::Result<()> {
		let base = unwrap_pts(&mut self.unwrap, pending.pts)?;

		let data = &pending.data;
		let mut offset = 0;
		// 48 kHz samples elapsed since this PES's PTS, advancing each packet after the first.
		let mut elapsed: u64 = 0;
		while offset < data.len() {
			let (header_len, size) = parse_opus_control_header(&data[offset..])?;
			let start = offset + header_len;
			let end = start + size;
			anyhow::ensure!(end <= data.len(), "Opus access unit exceeds PES payload");
			let packet = &data[start..end];

			let pts = match base {
				Some(base) if elapsed > 0 => {
					let advance = Timestamp::from_scale(elapsed, 48_000)?;
					// `base` is a 90 kHz PTS; rescale the sample advance to match before
					// adding (the scale-aware Timestamp rejects mixed scales).
					Some(base + advance.convert(base.scale())?)
				}
				other => other,
			};
			self.import.decode(packet, pts)?;

			// Default to 20 ms (960 samples) if the TOC can't be read, so a malformed packet
			// doesn't stall the timeline for the rest of the PES.
			elapsed += opus::packet_samples(packet).unwrap_or(960) as u64;
			offset = end;
		}
		Ok(())
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		Ok(self.import.seek(sequence)?)
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		Ok(self.import.finish()?)
	}
}

/// The 4-byte registration `format_identifier` from a PMT registration descriptor
/// (tag 0x05), if present. Identifies the codec of a private-data (0x06) stream.
fn registration_format(descriptors: &[mpeg2ts::ts::Descriptor]) -> Option<[u8; 4]> {
	descriptors
		.iter()
		.find(|d| d.tag == 0x05)
		.and_then(|d| d.data.get(..4))
		.and_then(|s| s.try_into().ok())
}

/// The Opus channel count from the DVB extension descriptor (tag 0x7f, ext tag 0x80).
///
/// `channel_config_code` follows the Opus-in-TS mapping (and ffmpeg's demuxer): 0 is dual
/// mono (decoded as stereo), 1..=8 is the channel count directly. Higher codes (0x81
/// explicitly-coded layouts, reserved values) aren't supported, so they fall back to the
/// caller's default rather than being read as a raw 129..=255 count.
fn opus_channel_count(descriptors: &[mpeg2ts::ts::Descriptor]) -> Option<u32> {
	descriptors
		.iter()
		.find(|d| d.tag == 0x7f && d.data.first() == Some(&0x80))
		.and_then(|d| d.data.get(1))
		.and_then(|&cc| match cc {
			0 => Some(2),
			1..=8 => Some(cc as u32),
			_ => None,
		})
}

/// Parse one Opus-in-TS access-unit control header, returning `(header_len, payload_size)`.
fn parse_opus_control_header(data: &[u8]) -> anyhow::Result<(usize, usize)> {
	anyhow::ensure!(data.len() >= 2, "Opus control header truncated");
	// 11-bit 0x3FF sync: byte 0 == 0x7F and the top 3 bits of byte 1 == 0b111.
	anyhow::ensure!(
		data[0] == 0x7f && (data[1] & 0xe0) == 0xe0,
		"invalid Opus control header sync (0x{:02x}{:02x})",
		data[0],
		data[1]
	);
	let start_trim = (data[1] & 0x10) != 0;
	let end_trim = (data[1] & 0x08) != 0;
	let control_ext = (data[1] & 0x04) != 0;

	let mut pos = 2;
	// au_size: sum a run of 0xFF bytes plus the final byte < 0xFF.
	let mut size = 0usize;
	loop {
		let b = *data.get(pos).context("Opus au_size truncated")?;
		pos += 1;
		size += b as usize;
		if b != 0xff {
			break;
		}
	}
	// Each trim field is 16 bits; the control extension is a length byte plus that many bytes.
	if start_trim {
		pos += 2;
	}
	if end_trim {
		pos += 2;
	}
	if control_ext {
		let len = *data.get(pos).context("Opus control extension truncated")? as usize;
		pos += 1 + len;
	}
	anyhow::ensure!(pos <= data.len(), "Opus control header exceeds payload");
	Ok((pos, size))
}

/// One stream of legacy broadcast audio (MP2, AC-3, E-AC-3), carried verbatim:
/// whole self-describing frames, split out of the PES by the codec's header
/// parser. Like AAC, import creation is deferred until the first frame header
/// (the config isn't in the PMT). No jitter hint: it only matters to browser
/// players, which cannot decode these codecs.
struct LegacyStream<E: CatalogExt = ()> {
	descriptor: &'static legacy::Descriptor,
	import: Option<legacy::Import<E>>,
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer<E>,
	unwrap: PtsUnwrap,
	/// Partial frame left at the end of the previous PES. ISO 13818-1 doesn't
	/// require audio frames to align with PES boundaries, so a legitimate mux can
	/// split one; it's reassembled here. A lost PES between the cut and its
	/// continuation still fails the next header parse (no frame-level resync).
	tail: Vec<u8>,
	/// PTS for the frame the tail begins, computed when it was cut. The PES PTS
	/// only covers frames that begin in that PES.
	tail_pts: Option<Timestamp>,
}

impl<E: CatalogExt> LegacyStream<E> {
	fn write(&mut self, pending: Pending) -> anyhow::Result<()> {
		let pes_base = unwrap_pts(&mut self.unwrap, pending.pts)?;

		// Prepend the partial frame left by the previous PES, if any.
		let carried = self.tail.len();
		let joined;
		let data: &[u8] = if carried == 0 {
			&pending.data
		} else {
			let mut j = std::mem::take(&mut self.tail);
			j.extend_from_slice(&pending.data);
			joined = j;
			&joined
		};

		// PTS for the next frame to emit. The tail frame keeps the PTS computed at
		// its cut; the first frame that BEGINS in this PES takes the PES PTS (per
		// ISO 13818-1, a PES PTS refers to the first access unit starting in it).
		// After each frame it advances by that frame's duration (per frame, not
		// `index * constant`: E-AC-3 varies the samples per frame).
		let mut pts = if carried > 0 { self.tail_pts.take() } else { pes_base };
		let mut in_tail = carried > 0;

		let mut offset = 0;
		while offset + self.descriptor.min_header_len <= data.len() {
			if in_tail && offset >= carried {
				pts = pes_base;
				in_tail = false;
			}

			let header = (self.descriptor.parse)(&data[offset..])?;
			let end = offset + header.len;
			if end > data.len() {
				// The frame continues in the next PES; finish it there.
				break;
			}

			let import = match &mut self.import {
				Some(import) => import,
				None => {
					let config = legacy::Config {
						sample_rate: header.sample_rate,
						channel_count: header.channel_count,
					};
					let track = crate::import::unique_track(&mut self.broadcast, self.descriptor.track_suffix)?;
					let legacy = legacy::Import::new(self.descriptor, track, self.catalog.clone(), config);
					self.import.insert(legacy)
				}
			};

			import.decode(&data[offset..end], pts)?;

			pts = match pts {
				// `pts` is a 90 kHz PES PTS; rescale the sample-rate advance to match
				// before adding (the scale-aware Timestamp rejects mixed scales).
				Some(pts) => {
					let advance = Timestamp::from_scale(header.samples, header.sample_rate as u64)?;
					Some(pts + advance.convert(pts.scale())?)
				}
				None => None,
			};
			offset = end;
		}

		// Keep any partial frame (cut mid-frame, or even mid-header) for the next
		// PES, with the PTS it should carry.
		if offset < data.len() {
			if in_tail && offset >= carried {
				pts = pes_base;
			}
			self.tail = data[offset..].to_vec();
			self.tail_pts = pts;
		}

		Ok(())
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		// A seek is a discontinuity; the partial frame will never see its end.
		self.tail.clear();
		self.tail_pts = None;
		if let Some(import) = &mut self.import {
			import.seek(sequence)?;
		}
		Ok(())
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		// A partial frame at end of stream isn't emissible verbatim; drop it, but
		// leave a trace for diagnosing truncated captures.
		if !self.tail.is_empty() {
			tracing::debug!(
				suffix = self.descriptor.track_suffix,
				bytes = self.tail.len(),
				"dropping partial frame at end of stream"
			);
		}
		if let Some(import) = &mut self.import {
			import.finish()?;
		}
		Ok(())
	}
}

/// Replicates [`PesHeader::optional_header_len`] (which is crate-private) to
/// compute the declared PES payload length for bounded packets.
fn pes_data_len(header: &PesHeader, pes_packet_len: u16) -> Option<usize> {
	if pes_packet_len == 0 {
		// Unbounded (the usual case for video); flush on the next PES start.
		return None;
	}
	let optional = 3 + header.pts.map_or(0, |_| 5) + header.dts.map_or(0, |_| 5) + header.escr.map_or(0, |_| 6);
	pes_packet_len.checked_sub(optional).map(|n| n as usize)
}

/// Convert a raw 90 kHz PTS to a microsecond [`Timestamp`], unwrapping the
/// 33-bit field. Returns `None` when the PES carried no PTS (the codec layer
/// then falls back to a wall-clock timestamp).
/// Swallow a [`MissingKeyframe`](crate::container::MissingKeyframe) from a video
/// decode: a TS capture can join mid-GOP, so the deltas before the first keyframe
/// have no group to anchor and are simply dropped rather than aborting the demux.
fn skip_missing_keyframe(result: crate::Result<()>) -> anyhow::Result<()> {
	match result {
		Ok(()) | Err(crate::Error::MissingKeyframe(_)) => Ok(()),
		Err(e) => Err(e.into()),
	}
}

fn unwrap_pts(unwrap: &mut PtsUnwrap, pts: Option<u64>) -> anyhow::Result<Option<Timestamp>> {
	let Some(raw) = pts else {
		return Ok(None);
	};
	let extended = unwrap.unwrap(raw);
	Ok(Some(Timestamp::from_scale(extended, 90_000)?))
}

/// The reorder delay `PTS - DTS` for one PES, as a microsecond [`Timestamp`]. `None` unless
/// both stamps are present and the gap is a plausible reorder (a few seconds); a larger or
/// negative gap is a discontinuity or bad DTS, ignored so it can't inflate the jitter. Both
/// are raw 90 kHz, so the subtraction is done modulo the 33-bit field to stay correct across
/// the wrap.
fn reorder_delay(pts: Option<u64>, dts: Option<u64>) -> Option<Timestamp> {
	const FIELD: u64 = 1 << 33;
	const MAX_REORDER_TICKS: u64 = 90_000 * 2; // 2 s; broadcast reorder is well under this.
	let (pts, dts) = (pts?, dts?);
	let delay = pts.wrapping_sub(dts) & (FIELD - 1);
	if delay == 0 || delay > MAX_REORDER_TICKS {
		return None;
	}
	Timestamp::from_scale(delay, 90_000).ok()
}

/// Tracks the wrap-around of the 33-bit, 90 kHz PTS field so timestamps stay
/// monotonic across the ~26.5 hour wrap period.
#[derive(Default)]
struct PtsUnwrap {
	last: Option<u64>,
	offset: u64,
}

impl PtsUnwrap {
	fn unwrap(&mut self, raw: u64) -> u64 {
		const WRAP: u64 = 1 << 33;
		const HALF: i64 = (WRAP / 2) as i64;
		if let Some(last) = self.last {
			let diff = raw as i64 - last as i64;
			if diff < -HALF {
				self.offset += WRAP;
			} else if diff > HALF && self.offset >= WRAP {
				self.offset -= WRAP;
			}
		}
		self.last = Some(raw);
		self.offset + raw
	}
}

/// A cloneable, refillable [`Read`] source backed by a shared buffer.
///
/// [`Import`] loads one whole TS packet at a time; the reader consumes it and
/// reaches end-of-stream before the next refill.
#[derive(Clone, Default)]
struct Feed(Arc<Mutex<FeedState>>);

#[derive(Default)]
struct FeedState {
	data: BytesMut,
	pos: usize,
}

impl Feed {
	fn lock(&self) -> std::sync::MutexGuard<'_, FeedState> {
		self.0.lock().unwrap()
	}
}

impl Read for Feed {
	fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
		let mut state = self.lock();
		let n = out.len().min(state.data.len() - state.pos);
		if n == 0 {
			return Ok(0);
		}
		out[..n].copy_from_slice(&state.data[state.pos..state.pos + n]);
		state.pos += n;
		Ok(n)
	}
}

#[cfg(test)]
mod test {
	use mpeg2ts::es::StreamType;

	use super::SectionReassembler;
	use moq_net::Timestamp;

	// libklvanc public-sample cue: table_id 0xFC, section_length 0x1b (27), 30 bytes total.
	const CUE: [u8; 30] = [
		0xfc, 0x30, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf0, 0x0a, 0x05, 0x00, 0x00, 0x2b, 0xb4,
		0x7f, 0xdf, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xad, 0x25, 0xe8, 0x39,
	];

	/// Build a payload-only TS packet (PID 0x0021, afc 0b01). `body` is the bytes
	/// after the pointer_field (when `pusi`) or after the 4-byte header, padded to
	/// 188 with 0xff stuffing. A packet carrying a section that continues into the
	/// next packet must fill `body` exactly (so no stuffing lands mid-section).
	fn packet(pusi: bool, cc: u8, pointer: u8, body: &[u8]) -> Vec<u8> {
		let mut p = vec![0x47, 0x00, 0x21, 0x10 | (cc & 0x0f)];
		if pusi {
			p[1] |= 0x40;
			p.push(pointer);
		}
		p.extend_from_slice(body);
		assert!(p.len() <= 188, "test packet body overflows 188 bytes");
		p.resize(188, 0xff);
		p
	}

	/// A continuation packet (no PUSI) whose adaptation field sets
	/// discontinuity_indicator, followed by `body`.
	fn discontinuity_packet(cc: u8, body: &[u8]) -> Vec<u8> {
		// afc 0b11 (adaptation + payload); adaptation_field_length 1, flags 0x80.
		let mut p = vec![0x47, 0x00, 0x21, 0x30 | (cc & 0x0f), 0x01, 0x80];
		p.extend_from_slice(body);
		assert!(p.len() <= 188, "test packet body overflows 188 bytes");
		p.resize(188, 0xff);
		p
	}

	/// A synthetic section: `table_id`, a 12-bit length, then `body_len` zero bytes.
	fn fake_section(table_id: u8, body_len: usize) -> Vec<u8> {
		let mut s = vec![table_id, ((body_len >> 8) & 0x0f) as u8, (body_len & 0xff) as u8];
		s.resize(3 + body_len, 0x00);
		s
	}

	fn run(pkts: &[Vec<u8>]) -> Vec<Vec<u8>> {
		let mut r = SectionReassembler::default();
		let mut out = Vec::new();
		for p in pkts {
			r.push(p, &mut out);
		}
		out
	}

	#[test]
	fn single_section() {
		assert_eq!(run(&[packet(true, 0, 0, &CUE)]), vec![CUE.to_vec()]);
	}

	#[test]
	fn carries_all_sections_verbatim() {
		// A non-SCTE table_id 0x00 section ahead of the cue: both are carried verbatim
		// (we no longer filter by table_id), and back-to-back sections parse cleanly.
		let other = fake_section(0x00, 5);
		let mut body = other.clone();
		body.extend_from_slice(&CUE);
		assert_eq!(run(&[packet(true, 0, 0, &body)]), vec![other, CUE.to_vec()]);
	}

	#[test]
	fn stuffing_only() {
		// A PUSI payload that is all 0xff stuffing emits nothing.
		assert!(run(&[packet(true, 0, 0, &[])]).is_empty());
	}

	#[test]
	fn payload_split_across_packets() {
		// A 250-byte section spans two packets with intact continuity; it reassembles.
		let section = fake_section(0xfc, 247);
		let p1 = packet(true, 0, 0, &section[..183]);
		let p2 = packet(false, 1, 0, &section[183..]);
		assert_eq!(run(&[p1, p2]), vec![section]);
	}

	#[test]
	fn header_split_across_packets() {
		// Section A fills packet 1 except for the cue's first two header bytes (`fc 30`);
		// the third (`1b`, which carries section_length) arrives in packet 2. The
		// reassembler must wait for it instead of dropping the start.
		let a = fake_section(0xfc, 178);
		let mut body = a.clone();
		body.extend_from_slice(&CUE[..2]);
		let p1 = packet(true, 0, 0, &body);
		let p2 = packet(false, 1, 0, &CUE[2..]);
		assert_eq!(run(&[p1, p2]), vec![a, CUE.to_vec()]);
	}

	#[test]
	fn continuity_gap_drops_partial() {
		// Same split, but packet 2's continuity_counter jumps (1 -> 3): the partial
		// section is dropped rather than completed from the wrong bytes.
		let section = fake_section(0xfc, 247);
		let p1 = packet(true, 0, 0, &section[..183]);
		let p2 = packet(false, 3, 0, &section[183..]);
		assert!(run(&[p1, p2]).is_empty());
	}

	#[test]
	fn discontinuity_drops_partial() {
		// Same split, but packet 2 carries an adaptation-field discontinuity, which
		// drops the partial even though the continuity_counter would line up.
		let section = fake_section(0xfc, 247);
		let p1 = packet(true, 0, 0, &section[..183]);
		let p2 = discontinuity_packet(1, &section[183..]);
		assert!(run(&[p1, p2]).is_empty());
	}

	#[test]
	fn gap_then_unaligned_payload_is_not_emitted() {
		// After a gap on a non-PUSI packet, the payload is unaligned continuation of
		// the dropped section. Even if it happens to start with 0xFC, it must not be
		// mistaken for a new section (there is no pointer_field to realign on).
		let section = fake_section(0xfc, 247);
		let p1 = packet(true, 0, 0, &section[..183]);
		let p2 = packet(false, 2, 0, &CUE); // cc gap (expected 1), payload looks like a cue
		assert!(run(&[p1, p2]).is_empty());
	}

	#[test]
	fn corrupt_pointer_field_is_dropped() {
		// A pointer_field that points past the payload marks a malformed packet; the
		// reassembler drops it instead of slicing out of bounds or fabricating a
		// section from the bytes that follow.
		assert!(run(&[packet(true, 0, 200, &CUE)]).is_empty());
	}

	#[test]
	fn stays_desynced_until_next_pusi() {
		// After a gap drops the partial, EVERY following non-PUSI packet is unaligned
		// continuation and must be ignored, not just the one carrying the gap, until a
		// PUSI re-establishes a section boundary. p3 looks like a cue but arrives with
		// no PUSI since the drop, so only p4's cue is emitted.
		let section = fake_section(0xfc, 247);
		let p1 = packet(true, 0, 0, &section[..183]);
		let p2 = packet(false, 2, 0, &section[183..]); // cc gap (expected 1) -> drop
		let p3 = packet(false, 3, 0, &CUE); // continuous cc, but unaligned bytes
		let p4 = packet(true, 4, 0, &CUE); // a real PUSI -> resync and emit
		assert_eq!(run(&[p1, p2, p3, p4]), vec![CUE.to_vec()]);
	}

	#[test]
	fn orphan_tail_before_section_is_skipped() {
		// A PUSI packet whose pointer_field skips a leading fragment (the tail of a
		// section we never saw the start of): the fragment is discarded even though it
		// looks like a cue, and only the section the pointer points to is emitted.
		let mut body = CUE.to_vec(); // orphan fragment ahead of the pointer
		body.extend_from_slice(&CUE); // the section the pointer points to
		let pkt = packet(true, 0, CUE.len() as u8, &body);
		assert_eq!(run(&[pkt]), vec![CUE.to_vec()]);
	}

	/// Serialize PAT + PMT for the given `(stream_type, pid)` elementary streams; with
	/// `cuei`, add the program-level CUEI descriptor so a `0x86` PID is detected as SCTE-35.
	fn synth_pmt(es: &[(StreamType, u16)], cuei: bool) -> Vec<u8> {
		use mpeg2ts::ts::payload::{Pat, Pmt};
		use mpeg2ts::ts::{
			ContinuityCounter, Descriptor, EsInfo, Pid, ProgramAssociation, TransportScramblingControl, TsHeader,
			TsPacket, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
		};

		const PMT_PID: u16 = 0x0100;
		let pat = Pat {
			transport_stream_id: 1,
			version_number: VersionNumber::default(),
			table: vec![ProgramAssociation {
				program_num: 1,
				program_map_pid: Pid::new(PMT_PID).unwrap(),
			}],
		};
		let pmt = Pmt {
			program_num: 1,
			pcr_pid: None,
			version_number: VersionNumber::default(),
			program_info: if cuei {
				vec![Descriptor {
					tag: 0x05,
					data: b"CUEI".to_vec(),
				}]
			} else {
				Vec::new()
			},
			es_info: es
				.iter()
				.map(|&(stream_type, pid)| EsInfo {
					stream_type,
					elementary_pid: Pid::new(pid).unwrap(),
					descriptors: Vec::new(),
				})
				.collect(),
		};

		let write = |out: &mut Vec<u8>, pid: u16, payload: TsPayload| {
			let packet = TsPacket {
				header: TsHeader {
					transport_error_indicator: false,
					transport_priority: false,
					pid: Pid::new(pid).unwrap(),
					transport_scrambling_control: TransportScramblingControl::NotScrambled,
					continuity_counter: ContinuityCounter::default(),
				},
				adaptation_field: None,
				payload: Some(payload),
			};
			TsPacketWriter::new(out).write_ts_packet(&packet).unwrap();
		};

		let mut out = Vec::new();
		write(&mut out, Pid::PAT, TsPayload::Pat(pat));
		write(&mut out, PMT_PID, TsPayload::Pmt(pmt));
		out
	}

	// An extended catalog detects the CUEI PID, advertises a cue track, and the
	// section is published (a `Catalog<catalog::Ext>` carries the rendition).
	#[test]
	fn scte35_extension_catalogs_the_cue_track() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(&[(StreamType::Dts8ChannelLosslessAudio, 0x21)], true));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		assert_eq!(
			catalog.snapshot().mpegts.tracks.len(),
			1,
			"expected one scte35 rendition"
		);
	}

	// The base catalog (`Catalog<()>`) can't carry cues, so a detected CUEI PID routes to
	// Stream::Ignored: dropped before the reader (no abort), with no ScteStream created, so
	// the publishing lock is never taken and the catalog is never republished empty.
	#[tokio::test(start_paused = true)]
	async fn base_catalog_routes_cue_pid_to_ignored() {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(&[(StreamType::Dts8ChannelLosslessAudio, 0x21)], true));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		import.decode(&bytes).unwrap(); // must not abort on the private section
		import.finish().unwrap();

		assert!(
			import.sections.is_empty(),
			"no cue stream is created for a base catalog"
		);
		assert!(
			matches!(
				import.streams.get(&mpeg2ts::ts::Pid::new(0x21).unwrap()),
				Some(super::Stream::Ignored)
			),
			"the CUEI PID routes to Ignored"
		);
		// SCTE detection takes no lock here (video/audio would still publish later): the old
		// discarding ScteStream took the lock and republished an empty catalog on this path.
		assert!(
			tokio::time::timeout(std::time::Duration::from_millis(10), updates.next())
				.await
				.is_err(),
			"SCTE detection must not publish the base catalog"
		);
	}

	// A PMT without CUEI first routes the 0x86 PID to Ignored; a later PMT with CUEI upgrades
	// it to a cue track. ensure_scte drops the stale Ignored route and decode prefers `scte`,
	// so the cue publishes.
	#[tokio::test(start_paused = true)]
	async fn pmt_without_cuei_then_with_cuei_upgrades() {
		use crate::catalog::hang::{Catalog, Container};
		use crate::container::Consumer;
		use crate::container::ts::catalog::Ext;

		const SECTION_PID: u16 = 0x0021;
		let pid = mpeg2ts::ts::Pid::new(SECTION_PID).unwrap();

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		// First PMT lacks CUEI: the 0x86 PID is ambiguous and routes to Ignored.
		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[(StreamType::Dts8ChannelLosslessAudio, SECTION_PID)],
			false,
		));
		import.decode(&bytes).unwrap();
		assert!(
			matches!(import.streams.get(&pid), Some(super::Stream::Ignored)),
			"pre-CUEI PMT routes the PID to Ignored"
		);

		// Second PMT carries CUEI: upgrade to a cue track, then a section on the same PID.
		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(&[(StreamType::Dts8ChannelLosslessAudio, SECTION_PID)], true));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		assert!(
			!import.streams.contains_key(&pid),
			"upgrade drops the stale Ignored route"
		);
		assert_eq!(
			catalog.snapshot().mpegts.tracks.len(),
			1,
			"upgrade advertises the cue track"
		);

		let name = catalog.snapshot().mpegts.tracks.keys().next().unwrap().clone();
		let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy).with_latency(std::time::Duration::ZERO);
		let frame = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read())
			.await
			.expect("cue read timed out")
			.unwrap()
			.expect("a published cue frame");
		assert_eq!(
			&frame.payload[..],
			&CUE[..],
			"verbatim splice_info_section after upgrade"
		);
	}

	/// A PUSI TS packet on `pid` carrying a minimal PES with `pts` (90 kHz) and a
	/// 1-byte dummy payload, for streams we observe only for their PTS.
	fn pes_packet(pid: u16, pts: u64) -> Vec<u8> {
		let pts_field = [
			0x21 | (((pts >> 30) & 0x07) << 1) as u8,
			((pts >> 22) & 0xff) as u8,
			0x01 | (((pts >> 15) & 0x7f) << 1) as u8,
			((pts >> 7) & 0xff) as u8,
			0x01 | ((pts & 0x7f) << 1) as u8,
		];
		let mut pes = vec![0x00, 0x00, 0x01, 0xe0]; // PES start code + a video stream_id
		let pes_len = 3 + 5 + 1; // flags(2) + header_data_length(1) + PTS(5) + payload(1)
		pes.push((pes_len >> 8) as u8);
		pes.push((pes_len & 0xff) as u8);
		pes.push(0x80); // '10' marker bits
		pes.push(0x80); // PTS_DTS_flags = '10' (PTS only)
		pes.push(0x05); // PES_header_data_length
		pes.extend_from_slice(&pts_field);
		pes.push(0xff); // dummy payload

		let mut p = vec![0x47, 0x40 | ((pid >> 8) as u8 & 0x1f), (pid & 0xff) as u8, 0x10];
		p.extend_from_slice(&pes);
		assert!(p.len() <= 188, "PES packet overflows 188 bytes");
		p.resize(188, 0xff);
		p
	}

	// The SCTE-35 media clock follows the video PTS only: a private PES never sets it,
	// and a private PES arriving after the video must not overwrite it.
	#[test]
	fn media_clock_follows_video_not_private_pes() {
		const VIDEO_PID: u16 = 0x0050;
		const PRIVATE_PID: u16 = 0x0051;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog);

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, VIDEO_PID),
				(StreamType::Mpeg2PacketizedData, PRIVATE_PID),
			],
			true,
		));
		import.decode(&bytes).unwrap();

		// Private before video: no clock yet.
		import.decode(pes_packet(PRIVATE_PID, 1_000).as_slice()).unwrap();
		assert!(import.last_pts.is_none(), "a private PES must not start the clock");

		// Video sets the clock.
		import.decode(pes_packet(VIDEO_PID, 90_000).as_slice()).unwrap();
		let after_video = import.last_pts;
		assert!(after_video.is_some(), "MPEG-2 video PTS must set the clock");

		// Private after video: must NOT overwrite it.
		import.decode(pes_packet(PRIVATE_PID, 270_000).as_slice()).unwrap();
		assert_eq!(
			import.last_pts, after_video,
			"a later private PES must not overwrite the clock"
		);
	}

	/// A PUSI TS packet on `pid`: a bounded audio PES (stream_id 0xC0) carrying
	/// `payload` (whole codec frames or a fragment of one), sized exactly via
	/// adaptation-field stuffing so the PES completes (and flushes) on this packet.
	fn audio_pes_packet(pid: u16, cc: u8, pts: u64, payload: &[u8]) -> Vec<u8> {
		let pts_field = [
			0x21 | (((pts >> 30) & 0x07) << 1) as u8,
			((pts >> 22) & 0xff) as u8,
			0x01 | (((pts >> 15) & 0x7f) << 1) as u8,
			((pts >> 7) & 0xff) as u8,
			0x01 | ((pts & 0x7f) << 1) as u8,
		];
		let mut pes = vec![0x00, 0x00, 0x01, 0xc0];
		let pes_len = 3 + 5 + payload.len();
		pes.push((pes_len >> 8) as u8);
		pes.push((pes_len & 0xff) as u8);
		pes.extend_from_slice(&[0x80, 0x80, 0x05]); // marker bits, PTS only, header len
		pes.extend_from_slice(&pts_field);
		pes.extend_from_slice(payload);

		let af_len = 184 - 1 - pes.len();
		let mut p = vec![
			0x47,
			0x40 | ((pid >> 8) as u8 & 0x1f),
			(pid & 0xff) as u8,
			0x30 | (cc & 0x0f),
		];
		p.push(af_len as u8);
		if af_len > 0 {
			p.push(0x00); // no AF flags; the rest is stuffing
			p.extend(std::iter::repeat_n(0xff, af_len - 1));
		}
		p.extend_from_slice(&pes);
		assert_eq!(p.len(), 188, "audio PES packet must fill exactly one TS packet");
		p
	}

	// MP2/AC-3 flush like any audio PES but don't consume the jitter hint; if one
	// anchored the audio run, an AAC PID in the same TS would publish a jitter
	// inflated by the inter-PID PTS offset.
	#[test]
	fn verbatim_audio_does_not_anchor_aac_jitter() {
		const AAC_PID: u16 = 0x0060;
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[(StreamType::AdtsAac, AAC_PID), (StreamType::Mpeg1Audio, MP2_PID)],
			false,
		));
		// A whole MP2 frame (MPEG-1 Layer II, 32 kbps, 48 kHz, stereo = 96 bytes),
		// 2 s ahead of the AAC PES that follows in the same audio run.
		let mut mp2 = vec![0xFF, 0xFD, 0x14, 0x00];
		mp2.resize(96, 0xAA);
		bytes.extend_from_slice(&audio_pes_packet(MP2_PID, 0, 90_000, &mp2));

		let mut aac = super::adts::write_header(2, 48_000, 2, 8).unwrap().to_vec();
		aac.extend_from_slice(&[0u8; 8]);
		bytes.extend_from_slice(&audio_pes_packet(AAC_PID, 0, 270_000, &aac));

		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		let snap = catalog.snapshot();
		assert_eq!(snap.audio.renditions.len(), 2, "AAC and MP2 renditions");
		let aac_rendition = snap
			.audio
			.renditions
			.values()
			.find(|a| a.codec.to_string().starts_with("mp4a"))
			.expect("AAC rendition");
		let jitter = aac_rendition.jitter.expect("AAC publishes a jitter");
		// Anchored on its own PES: one 1024-sample frame at 48 kHz (~21 ms).
		// Anchored on the MP2 PES it would be ~2 s.
		assert!(
			jitter <= std::time::Duration::from_millis(100),
			"AAC jitter anchored on a foreign PID: {jitter:?}"
		);
	}

	/// Read every retained frame of the single audio rendition in `catalog`.
	async fn read_audio_frames(
		consumer: &moq_net::BroadcastConsumer,
		catalog: &crate::catalog::Producer,
	) -> Vec<crate::container::Frame> {
		let name = catalog
			.snapshot()
			.audio
			.renditions
			.keys()
			.next()
			.expect("an audio track")
			.clone();
		let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
		let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy);
		let mut frames = Vec::new();
		while let Ok(Ok(Some(frame))) = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read()).await
		{
			frames.push(frame);
		}
		frames
	}

	// ISO 13818-1 doesn't require audio frames to align with PES boundaries: a
	// frame split across two PES must be reassembled byte-exact, stamped with the
	// PTS of the PES it began in, and the next whole frame takes the new PES's PTS.
	#[tokio::test(start_paused = true)]
	async fn legacy_frame_split_across_pes_reassembles() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// Two 96-byte MP2 frames with distinct payloads; frame A is cut at byte 50.
		let mut frame_a = vec![0xFF, 0xFD, 0x14, 0x00];
		frame_a.extend((4..96).map(|i| i as u8));
		let mut frame_b = vec![0xFF, 0xFD, 0x14, 0x00];
		frame_b.extend((4..96).rev().map(|i| i as u8));

		let mut second = frame_a[50..].to_vec();
		second.extend_from_slice(&frame_b);
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &frame_a[..50]).as_slice())
			.unwrap();
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &second).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(frames.len(), 2, "both frames must survive the split");
		assert_eq!(
			frames[0].payload.as_ref(),
			&frame_a[..],
			"frame A reassembled byte-exact"
		);
		assert_eq!(frames[1].payload.as_ref(), &frame_b[..], "frame B intact");
		// Frame A began in PES 1 (PTS 90000 ticks = 1 s); frame B begins in PES 2
		// (270000 ticks = 3 s). The legacy container normalizes to microseconds on the wire.
		assert_eq!(frames[0].timestamp, Timestamp::from_micros(1_000_000).unwrap());
		assert_eq!(frames[1].timestamp, Timestamp::from_micros(3_000_000).unwrap());
	}

	// A cut inside the next frame's header (fewer bytes left than a parseable
	// header) must also reassemble, and the carried frame keeps the PTS derived
	// from the PES it began in, NOT the next PES's (whose PTS only covers frames
	// that begin in it).
	#[tokio::test(start_paused = true)]
	async fn legacy_header_split_keeps_origin_pts() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		let mut frame_a = vec![0xFF, 0xFD, 0x14, 0x00];
		frame_a.resize(96, 0x55);
		let mut frame_b = vec![0xFF, 0xFD, 0x14, 0x00];
		frame_b.resize(96, 0x66);

		// PES 1: frame A whole plus only 2 bytes of frame B (not even a header).
		let mut first = frame_a.clone();
		first.extend_from_slice(&frame_b[..2]);
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &first).as_slice())
			.unwrap();
		// PES 2: the rest of frame B, under a far-off PTS that must NOT apply to it.
		import
			.decode(audio_pes_packet(MP2_PID, 1, 900_000, &frame_b[2..]).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(frames.len(), 2, "both frames must survive the header split");
		assert_eq!(
			frames[1].payload.as_ref(),
			&frame_b[..],
			"frame B reassembled byte-exact"
		);
		// Frame B began in PES 1: its PTS is frame A's plus one frame duration
		// (1152 samples at 48 kHz = 24 ms), not PES 2's 10 s.
		assert_eq!(frames[1].timestamp, Timestamp::from_micros(1_024_000).unwrap());
	}

	// End-to-end: a real SCTE-35 PID is detected, and its section is published as a frame
	// stamped with the video PTS (the bug stamped every cue at zero).
	#[tokio::test(start_paused = true)]
	async fn scte35_cue_stamped_with_video_pts() {
		use crate::catalog::hang::{Catalog, Container};
		use crate::container::Consumer;
		use crate::container::ts::catalog::Ext;
		use moq_net::Timestamp;

		const VIDEO_PID: u16 = 0x0050;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, VIDEO_PID),
				(StreamType::Dts8ChannelLosslessAudio, 0x21),
			],
			true,
		));
		bytes.extend_from_slice(&pes_packet(VIDEO_PID, 90_000)); // video sets the clock
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE)); // then the SCTE-35 section
		import.decode(&bytes).unwrap();
		let clock = import.last_pts.expect("video set the media clock");
		import.finish().unwrap();

		let name = catalog.snapshot().mpegts.tracks.keys().next().unwrap().clone();
		let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy).with_latency(std::time::Duration::ZERO);
		let frame = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read())
			.await
			.expect("cue read timed out")
			.unwrap()
			.expect("a published cue frame");

		assert_eq!(&frame.payload[..], &CUE[..], "verbatim splice_info_section");
		assert_ne!(frame.timestamp, Timestamp::ZERO, "cue must not stamp zero");
		// The legacy container normalizes the wire timestamp to microseconds, so compare the
		// instant (not the raw scale) against the 90 kHz media clock the cue was stamped with.
		assert_eq!(
			std::time::Duration::from(frame.timestamp),
			std::time::Duration::from(clock),
			"cue stamped with the video media clock"
		);
	}

	// A 0x86 PID without CUEI is ambiguous (DTS audio or a non-conformant SCTE mux):
	// it's classified Ignored and dropped, NOT handed to the PES reader (which aborts
	// on private sections, spec section 7) and NOT cataloged. The rest keeps importing.
	#[test]
	fn section_pid_without_cuei_is_dropped_not_cataloged() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		const VIDEO_PID: u16 = 0x0050;
		const SECTION_PID: u16 = 0x0021;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		// catalog::Ext (not the base catalog) makes a wrong ensure_scte() observable: it
		// would create a rendition, which the base catalog silently drops.
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let mut bytes = bytes::BytesMut::new();
		// PMT WITHOUT CUEI: the 0x86 PID must not be recognized as SCTE-35.
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, VIDEO_PID),
				(StreamType::Dts8ChannelLosslessAudio, SECTION_PID),
			],
			false,
		));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE)); // a private section on 0x21
		bytes.extend_from_slice(&pes_packet(VIDEO_PID, 90_000)); // valid video after it
		import.decode(&bytes).unwrap(); // must NOT abort

		assert!(
			import.last_pts.is_some(),
			"video kept importing past the dropped section PID"
		);
		assert!(
			catalog.snapshot().mpegts.tracks.is_empty(),
			"a 0x86 PID without CUEI must not be cataloged"
		);
	}

	#[test]
	fn duplicate_mid_section_packet_is_skipped() {
		// A 3-packet section with the central continuation duplicated (same cc, same
		// bytes): the duplicate is skipped so the section still reassembles.
		let section = fake_section(0xfc, 400); // 403 bytes, spans 3 packets
		let p1 = packet(true, 0, 0, &section[..183]);
		let p2 = packet(false, 1, 0, &section[183..367]);
		let p3 = packet(false, 2, 0, &section[367..]);
		assert_eq!(run(&[p1, p2.clone(), p2, p3]), vec![section]);
	}

	#[test]
	fn duplicate_pusi_packet_emits_once() {
		// A complete cue in one PUSI packet sent twice (legal duplicate) emits once.
		let p = packet(true, 0, 0, &CUE);
		assert_eq!(run(&[p.clone(), p]), vec![CUE.to_vec()]);
	}

	#[test]
	fn tei_continuation_drops_partial_and_resyncs() {
		// A continuation flagged TEI corrupts the partial: drop it, ignore the
		// following unaligned bytes, and resync on the next clean PUSI.
		let section = fake_section(0xfc, 247);
		let p1 = packet(true, 0, 0, &section[..183]);
		let mut p2 = packet(false, 1, 0, &section[183..]);
		p2[1] |= 0x80; // transport_error_indicator
		let p3 = packet(false, 2, 0, &CUE); // unaligned after the drop: must not emit
		let p4 = packet(true, 3, 0, &CUE); // clean PUSI: resync and emit
		assert_eq!(run(&[p1, p2, p3, p4]), vec![CUE.to_vec()]);
	}

	// A PES-framed elementary stream we don't decode (private data, stream_type 0x06)
	// is carried verbatim: cataloged in the `mpegts` section with its PID and framing, and
	// its PES payload published byte-for-byte.
	#[tokio::test(start_paused = true)]
	async fn private_pes_carried_verbatim() {
		use crate::catalog::hang::{Catalog, Container};
		use crate::container::Consumer;
		use crate::container::ts::catalog::{Ext, Framing};

		const VIDEO_PID: u16 = 0x0050;
		const DATA_PID: u16 = 0x0052;

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.clone());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, VIDEO_PID),
				(StreamType::Mpeg2PacketizedData, DATA_PID),
			],
			false,
		));
		bytes.extend_from_slice(&pes_packet(VIDEO_PID, 90_000)); // video sets the media clock
		let payload = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];
		bytes.extend_from_slice(&audio_pes_packet(DATA_PID, 0, 90_000, &payload));
		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		let snap = catalog.snapshot();
		assert_eq!(snap.mpegts.tracks.len(), 1, "the private PES PID is carried verbatim");
		let (name, track) = snap.mpegts.tracks.iter().next().unwrap();
		let verbatim = track.verbatim.as_ref().expect("a verbatim carriage record");
		assert_eq!(verbatim.stream_type, 0x06, "recorded the PMT stream_type");
		assert_eq!(verbatim.framing, Framing::Pes, "private PES is PES-framed");
		// `audio_pes_packet` uses stream_id 0xC0; it must be captured for faithful re-emit.
		assert_eq!(verbatim.stream_id, Some(0xC0), "recorded the PES stream_id");
		assert_eq!(track.pid, DATA_PID, "recorded the original PID");

		let track = consumer.track(name.as_str()).unwrap().subscribe(None).await.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy).with_latency(std::time::Duration::ZERO);
		let frame = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read())
			.await
			.expect("verbatim read timed out")
			.unwrap()
			.expect("a published verbatim frame");
		assert_eq!(&frame.payload[..], &payload[..], "verbatim PES payload round-trips");
	}
}
