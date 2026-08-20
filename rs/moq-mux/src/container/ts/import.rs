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
//! private-section path, and the 90 kHz -> microsecond PTS conversion. The service
//! layer is captured too: the identity parsed from the PAT as a
//! [`Program`](catalog::Program) record in the `mpegts` catalog section, and the
//! standalone SI PIDs as opaque sections on per-`(PID, table_id)` snapshot tracks
//! (see [`si`](super::si)), so both survive the round-trip.

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
/// PID, when the catalog `E` carries the [`mpegts`](super::Mpegts) section: PES-framed
/// streams ride the normal PES reassembly, section-framed streams (SCTE-35, marked
/// by a program-level 'CUEI' registration descriptor, and other private sections)
/// are intercepted before the reader and reassembled. With a base `Catalog<()>`
/// they're logged and dropped instead.
pub struct Import<E: catalog::Catalog = ()> {
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer<E>,

	/// Held while the first PMT is parsed so the catalog is withheld from the broadcast until every
	/// stream in the initial program has reserved its rendition. Dropped once the PMT is fully
	/// processed, so a one-shot muxer (fMP4, TS re-export) sees the complete track list in the first
	/// snapshot rather than a half-converged one. See [`Reserved`](crate::catalog::Reserved).
	initial_reservation: Option<crate::catalog::Reserved<E>>,

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
	/// Per elementary-stream-PID TS continuity state.
	continuity: HashMap<Pid, Continuity>,
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
	/// Reassemblers for the standalone SI PIDs ([`catalog::SI_PIDS`]), keyed by PID.
	/// Intercepted before the reader (like SCTE-35 sections) so the service layer
	/// survives the round-trip. Only populated with `mpegts` catalog support.
	si_sections: HashMap<u16, SectionReassembler>,
	/// The SI store: buffers each sub-table to a complete generation, publishes the
	/// per-`(PID, table_id)` snapshot tracks, and advertises them in the catalog.
	si: super::si::Capture<E>,
	/// Whether the service identity (TSID/service_id/PMT PID) has been captured from
	/// the PAT into the catalog service record yet (set once; the PAT is stable).
	identity_recorded: bool,
	/// Latest video PTS: the media clock used to timestamp private sections, which
	/// carry no PES PTS of their own. Unwrapped independently of the video stream.
	/// SPTS scope: one clock for the whole input. Under MPTS every program's video
	/// advances it, so a cue could be stamped with another program's PTS.
	last_pts: Option<Timestamp>,
	media_unwrap: PtsUnwrap,
}

impl<E: catalog::Catalog> Import<E> {
	pub fn new(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		let feed = Feed::default();
		// A long-lived producer handle for catalog edits (mpegts sections, later PMTs); the passed
		// reservation gates the initial publish and is dropped once the first PMT is parsed.
		let catalog = reserved.producer();
		// Sample the real catalog once at construction, not E::default(): an extension
		// may carry the section by value, and a snapshot clones under the mutex (no publish).
		let mut snapshot = catalog.snapshot();
		let supports_mpegts = snapshot.mpegts_mut().is_some();
		let si = super::si::Capture::new(broadcast.clone(), catalog.clone());
		Self {
			broadcast,
			catalog,
			initial_reservation: Some(reserved),
			reader: TsPacketReader::new(feed.clone()),
			feed,
			pmt_pids: HashSet::new(),
			streams: HashMap::new(),
			pending: HashMap::new(),
			continuity: HashMap::new(),
			initialized: false,
			audio_burst: None,
			scratch: Vec::new(),
			synced: false,
			sections: HashMap::new(),
			supports_mpegts,
			es_descriptors: HashMap::new(),
			recorded_media: HashSet::new(),
			program_recorded: false,
			si_sections: HashMap::new(),
			si,
			identity_recorded: false,
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
			// Intercept the standalone SI PIDs before the routing gate below drops them:
			// they carry the service layer, not media, and are not fed to the reader
			// (which only routes the PAT/PMT/ES PIDs it learns).
			if self.supports_mpegts && catalog::SI_PIDS.contains(&pid) {
				self.si_section(pid, &pkt)?;
				continue;
			}
			// An elementary stream's PES is as vulnerable to a break as a section is. A
			// looping publisher wraps with its last PES still open and short of its declared
			// length, so the next loop's leading packets would otherwise complete it and hand
			// the codec one buffer straddling the cut.
			if let Ok(pid) = Pid::new(pid)
				&& self.streams.contains_key(&pid)
			{
				match self.continuity.entry(pid).or_default().observe(&pkt) {
					Continuation::Duplicate => continue,
					// Flagged corrupt, so the packet joins the partial rather than opening a
					// new PES out of bytes the demodulator already disowned.
					Continuation::Corrupt => {
						self.pending.remove(&pid);
						if let Some(stream) = self.streams.get_mut(&pid) {
							stream.desync();
						}
						continue;
					}
					Continuation::Broken => {
						// Salvage the truncated PES only where its bytes stand on their own, then
						// drop whatever is left mid-unit and require the next frame to prove its
						// boundary.
						if self.streams.get(&pid).is_some_and(Stream::salvages_partial_pes) {
							self.flush(pid)?;
						} else {
							self.pending.remove(&pid);
						}
						if let Some(stream) = self.streams.get_mut(&pid) {
							stream.desync();
						}
						// This packet still routes normally: a PUSI opens a fresh PES, while a
						// continuation finds no pending entry and is dropped, so the stream
						// resumes at the next PES start rather than mid-frame.
					}
					Continuation::Contiguous => {}
				}
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
		// Cut the snapshot groups for whatever SI committed in this batch. Batching per
		// decode call (plus the store's own host-clock debounce) coalesces a junction's
		// burst of sub-table commits into few groups instead of one per commit.
		self.si.flush(self.last_pts.unwrap_or(Timestamp::ZERO), false)?;
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

				// Every stream in the initial program is registered now; release the reservation
				// so the catalog publishes once each rendition's config resolves, not before.
				self.initial_reservation = None;
			}
			Some(TsPayload::PesStart(pes)) => self.handle_pes_start(pid, pes)?,
			Some(TsPayload::PesContinuation(bytes)) => self.handle_pes_continuation(pid, &bytes)?,
			// Learn the PMT PIDs so the routing gate in `decode` lets them through.
			Some(TsPayload::Pat(pat)) => {
				self.pmt_pids
					.extend(pat.table.iter().map(|entry| entry.program_map_pid));
				self.record_program_identity(&pat);
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
				let track = self.broadcast.unique_track(".avc3", self.catalog.track_info())?;
				Stream::H264 {
					split: h264::Split::new(),
					import: Box::new(h264::Import::new(track, self.catalog.reserve(), Default::default())?),
					unwrap: PtsUnwrap::default(),
				}
			}
			StreamType::H265 => {
				let track = self.broadcast.unique_track(".hev1", self.catalog.track_info())?;
				Stream::H265 {
					split: h265::Split::new(),
					import: Box::new(h265::Import::new(track, self.catalog.reserve(), Default::default())?),
					unwrap: PtsUnwrap::default(),
				}
			}
			// Only ADTS-framed AAC (0x0F). 0x11 is LATM/LOAS, which uses a different
			// framing and syncword, so it falls through to the ignored arm below.
			StreamType::AdtsAac => Stream::Aac(Box::new(AacStream {
				import: None,
				broadcast: self.broadcast.clone(),
				reserved: Some(self.catalog.reserve()),
				unwrap: PtsUnwrap::default(),
				jitter: None,
				tail: Vec::new(),
				tail_pts: None,
				resync: Resync::default(),
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
				let track = self.broadcast.unique_track(".opus", self.catalog.track_info())?;
				let config = opus::Config::new(48_000, channel_count);
				Stream::Opus(Box::new(OpusStream {
					import: opus::Import::new(track, self.catalog.reserve(), config.into())?,
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
		self.continuity.entry(pid).or_default();
		Ok(())
	}

	fn legacy_stream(&self, descriptor: &'static legacy::Descriptor) -> Stream<E> {
		Stream::Legacy(Box::new(LegacyStream {
			descriptor,
			import: None,
			broadcast: self.broadcast.clone(),
			reserved: Some(self.catalog.reserve()),
			unwrap: PtsUnwrap::default(),
			tail: Vec::new(),
			tail_pts: None,
			resync: Resync::default(),
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
		self.continuity.remove(&pid);
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

	/// Capture the transport/service identity (TSID, service number, PMT PID) from the
	/// PAT into the catalog service record, once. No-op without `mpegts` support.
	fn record_program_identity(&mut self, pat: &mpeg2ts::ts::payload::Pat) {
		if !self.supports_mpegts || self.identity_recorded {
			return;
		}
		// program_number 0 is the network PID association, not a service; skip it.
		let Some(entry) = pat.table.iter().find(|entry| entry.program_num != 0) else {
			return;
		};
		if let Some(mpegts) = self.catalog.lock().mpegts_mut() {
			let program = mpegts.program.get_or_insert_with(Default::default);
			program.transport_stream_id = pat.transport_stream_id;
			program.program_number = entry.program_num;
			program.pmt_pid = entry.program_map_pid.as_u16();
		}
		self.identity_recorded = true;
	}

	/// Feed one TS packet on a standalone SI PID to its reassembler, folding each
	/// completed section into the SI store.
	///
	/// Every section is captured, whatever its `table_id`: the SDT PID also carries
	/// the BAT, the EIT PID carries now/next and the schedule, and a table we don't
	/// recognize is exactly as worth preserving as one we do. The store buffers each
	/// sub-table to a complete generation and commits it atomically, so a plain
	/// repetition (SI repeats every couple of seconds) publishes nothing and a torn
	/// multi-section transition is never visible.
	fn si_section(&mut self, pid: u16, pkt: &[u8]) -> anyhow::Result<()> {
		let mut sections = Vec::new();
		self.si_sections.entry(pid).or_default().push(pkt, &mut sections);
		for section in sections {
			self.si.section(pid, section)?;
		}
		Ok(())
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
		self.si.finish(self.last_pts.unwrap_or(Timestamp::ZERO))?;
		Ok(())
	}

	/// Abort every track with `err` instead of finishing, so subscribers see the
	/// real cause rather than [`moq_net::Error::Dropped`]. Buffered PES is discarded.
	/// Consumes the importer.
	pub fn abort(mut self, err: moq_net::Error) {
		for stream in std::mem::take(&mut self.streams).into_values() {
			stream.abort(err.clone());
		}
		for section in std::mem::take(&mut self.sections).into_values() {
			section.abort(err.clone());
		}
		let si = std::mem::replace(
			&mut self.si,
			super::si::Capture::new(self.broadcast.clone(), self.catalog.clone()),
		);
		si.abort(err);
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

impl Pending {
	/// A PES carrying nothing, used to drain a stream's carried tail at end of stream.
	fn empty() -> Self {
		Self {
			pts: None,
			dts: None,
			stream_id: 0,
			data: Vec::new(),
			data_len: None,
		}
	}
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
	broadcast: &mut moq_net::broadcast::Producer,
	catalog: &mut crate::catalog::Producer<E>,
	pid: u16,
	stream_type: u8,
	framing: catalog::Framing,
	descriptors: Vec<catalog::Descriptor>,
) -> anyhow::Result<crate::container::Producer<crate::catalog::hang::Container>> {
	// Verbatim payloads ride the legacy container, which normalizes the per-frame
	// timestamp to microseconds on the wire (see `hang::container::Frame::encode`),
	// so the track declares that timescale to match.
	let track = broadcast.unique_track(".ts", catalog.track_info())?;
	let name = track.name().to_string();

	// Build the media producer before advertising the track. It is fallible (its
	// timeline track can collide), and the `VerbatimEntry` that removes this catalog
	// entry on drop only exists once this function returns successfully, so an entry
	// published first would be stranded.
	let media = catalog.media_producer(track, crate::catalog::hang::Container::Legacy)?;

	let mut guard = catalog.lock();
	let Some(mpegts) = guard.mpegts_mut() else {
		// supports_mpegts was true when sampled at construction; None here means the
		// catalog dropped the section since.
		anyhow::bail!("catalog extension no longer carries an mpegts section");
	};
	mpegts.tracks.insert(
		name,
		catalog::Track {
			pid,
			descriptors,
			verbatim: Some(catalog::Verbatim::new(stream_type, framing)),
		},
	);
	drop(guard);

	Ok(media)
}

/// Remove a verbatim track's entry from the `mpegts` catalog section on drop.
fn unregister_verbatim<E: catalog::Catalog>(catalog: &mut crate::catalog::Producer<E>, name: &str) {
	if let Some(mpegts) = catalog.lock().mpegts_mut() {
		mpegts.tracks.remove(name);
	}
}

/// Owns a verbatim track's `mpegts` catalog entry, removing it however the stream ends.
struct VerbatimEntry<E: catalog::Catalog> {
	catalog: crate::catalog::Producer<E>,
	name: String,
}

impl<E: catalog::Catalog> Drop for VerbatimEntry<E> {
	fn drop(&mut self) {
		unregister_verbatim(&mut self.catalog, &self.name);
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
	/// Held for its `Drop`, which clears this track's catalog entry.
	_entry: VerbatimEntry<E>,
	reassembler: SectionReassembler,
}

impl<E: catalog::Catalog> SectionStream<E> {
	fn new(
		mut broadcast: moq_net::broadcast::Producer,
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
		let entry = VerbatimEntry {
			name: track.name().to_string(),
			catalog,
		};
		Ok(Self {
			track,
			_entry: entry,
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
		self.track.cut(None)?;
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

	fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
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
	entry: VerbatimEntry<E>,
	unwrap: PtsUnwrap,
	/// Whether the PES stream_id has been recorded into the catalog yet (once).
	stream_id_recorded: bool,
}

impl<E: catalog::Catalog> VerbatimStream<E> {
	fn new(
		mut broadcast: moq_net::broadcast::Producer,
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
		let entry = VerbatimEntry {
			name: track.name().to_string(),
			catalog,
		};
		Ok(Self {
			track,
			entry,
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
			if let Some(mpegts) = self.entry.catalog.lock().mpegts_mut()
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
		self.track.cut(None)?;
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

	fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}
}

/// Whether one PID's TS packets are still an unbroken chain, for whoever is accumulating
/// bytes out of them.
///
/// Both reassemblers on this PID need the same answer: a section and a PES are equally
/// meaningless when spliced onto bytes that didn't follow them. Keeping one implementation
/// keeps a subtle rule (which packets advance the counter, which retransmissions to ignore)
/// from drifting into two.
#[derive(Default)]
struct Continuity {
	/// Last continuity_counter seen on a packet with payload, to spot gaps.
	last_cc: Option<u8>,
	/// Last payload packet, to identify ISO 13818-1 retransmissions.
	last_pkt: Option<[u8; 188]>,
}

/// What one packet says about the bytes already accumulated for its PID.
enum Continuation {
	/// A retransmission, which may refresh PCR/OPCR. Ignore it entirely to avoid duplicate bytes.
	Duplicate,
	/// Contiguous with the previous payload packet.
	Contiguous,
	/// A counter gap or a declared discontinuity. Whatever was accumulating for this PID is
	/// lost, but this packet's own payload is intact and still worth routing.
	Broken,
	/// The demodulator flagged this packet corrupt. The partial is lost like [`Broken`], and
	/// so is the packet: nothing in it can be trusted.
	Corrupt,
}

/// Whether two packets differ only in the clock fields a retransmission may refresh.
fn is_duplicate(last: &[u8; 188], pkt: &[u8; 188]) -> bool {
	if last == pkt {
		return true;
	}

	// A refreshed clock requires the same adaptation-field shape in both packets. Comparing
	// through the flags byte also pins the PID, payload-unit start, counter, length, and every
	// other header bit before any bytes are ignored.
	let afc = (last[3] >> 4) & 0x3;
	if afc & 0x2 == 0 || last[..6] != pkt[..6] {
		return false;
	}

	let af_len = last[4] as usize;
	let flags = last[5];
	let clock_len = usize::from(flags & 0x10 != 0) * 6 + usize::from(flags & 0x08 != 0) * 6;
	let clock_end = 6 + clock_len;
	let adaptation_end = 5 + af_len;
	if clock_len == 0 || clock_end > adaptation_end || adaptation_end > last.len() {
		return false;
	}

	last[clock_end..] == pkt[clock_end..]
}

impl Continuity {
	/// Classify one 188-byte packet, recording what the next call needs.
	fn observe(&mut self, pkt: &[u8; 188]) -> Continuation {
		// transport_error_indicator: the demodulator flagged this packet as corrupt, so its
		// payload can't be trusted (and we don't validate CRC-32). Forgetting the counter
		// keeps the next clean packet from also looking like a gap.
		if pkt[1] & 0x80 != 0 {
			self.last_cc = None;
			self.last_pkt = None;
			return Continuation::Corrupt;
		}

		let afc = (pkt[3] >> 4) & 0x3;
		let has_payload = afc & 0x1 != 0;
		// Read the adaptation field before the no-payload case: a discontinuity can ride on
		// an adaptation-only packet, and it counts just the same.
		let discontinuity = if afc & 0x2 != 0 {
			let af_len = pkt[4] as usize;
			af_len > 0 && pkt[5] & 0x80 != 0
		} else {
			false
		};

		if !has_payload {
			if discontinuity {
				self.last_cc = None;
				self.last_pkt = None;
				return Continuation::Broken;
			}
			return Continuation::Contiguous;
		}

		// A retransmission repeats the counter, and so does a loss of exactly 15 packets.
		// Only the bytes tell them apart, which is why the counter alone can't decide: taking
		// every repeat for a duplicate would carry a partial straight across that loss and
		// join it to unrelated bytes.
		if self.last_pkt.as_ref().is_some_and(|last| is_duplicate(last, pkt)) {
			return Continuation::Duplicate;
		}
		self.last_pkt = Some(*pkt);

		// Only payload packets advance the counter, so this is the one place it moves.
		let cc = pkt[3] & 0x0f;
		let cc_gap = matches!(self.last_cc, Some(last) if cc != (last + 1) & 0x0f);
		self.last_cc = Some(cc);
		if discontinuity || cc_gap {
			Continuation::Broken
		} else {
			Continuation::Contiguous
		}
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
	/// Shared with the PES path: a broken chain drops the partial either way.
	continuity: Continuity,
}

impl SectionReassembler {
	/// Consume one 188-byte TS packet, appending every completed section to `out`.
	fn push(&mut self, pkt: &[u8], out: &mut Vec<Vec<u8>>) {
		let pkt: &[u8; 188] = pkt.try_into().expect("section packet must be 188 bytes");
		match self.continuity.observe(pkt) {
			Continuation::Duplicate => return,
			// A packet flagged corrupt takes its own payload down with the partial: this is
			// the one case where the packet itself must not be processed.
			Continuation::Corrupt => {
				self.acc.clear();
				return;
			}
			Continuation::Broken => self.acc.clear(),
			Continuation::Contiguous => {}
		}

		let pusi = pkt[1] & 0x40 != 0;
		let afc = (pkt[3] >> 4) & 0x3;
		let has_payload = afc & 0x1 != 0;

		let mut off = 4;
		if afc & 0x2 != 0 {
			let af_len = pkt[4] as usize;
			off = 5 + af_len;
		}

		if !has_payload {
			return;
		}

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

	/// Whether a PES cut short by a break is still worth publishing.
	///
	/// True where one PES carries many independently decodable units, so the ones ahead of
	/// the cut are whole and correct on their own. False where it carries exactly one: half
	/// an access unit is a picture with missing slices, and half a keyframe stays wrong for
	/// every picture that references it. Verbatim payloads are all-or-nothing the same way.
	fn salvages_partial_pes(&self) -> bool {
		match self {
			Stream::Aac(_) | Stream::Legacy(_) => true,
			// Opus carries many packets per PES like the audio above, but its framing is
			// declared rather than self-describing: a trailing packet cut short of the length
			// its control header promises is a parse error, and that error would travel up out
			// of `decode` and end the session. Losing the PES beats losing the broadcast.
			Stream::Opus(_) => false,
			Stream::H264 { .. } | Stream::H265 { .. } | Stream::Verbatim(_) => false,
			Stream::Clock | Stream::Ignored => false,
		}
	}

	/// Sync was lost: drop whatever partial unit is held and stop vouching for the next
	/// boundary. This is [`seek`](Self::seek) without the group-sequence side effect, for a
	/// break the stream recovers from in place.
	fn desync(&mut self) {
		match self {
			Stream::H264 { split, .. } => split.reset(),
			Stream::H265 { split, .. } => split.reset(),
			Stream::Aac(stream) => stream.desync(),
			Stream::Legacy(stream) => stream.desync(),
			Stream::Opus(_) | Stream::Verbatim(_) | Stream::Clock | Stream::Ignored => {}
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

	fn abort(self, err: moq_net::Error) {
		match self {
			Stream::H264 { import, .. } => import.abort(err),
			Stream::H265 { import, .. } => import.abort(err),
			Stream::Aac(stream) => stream.abort(err),
			Stream::Opus(stream) => stream.abort(err),
			Stream::Legacy(stream) => stream.abort(err),
			Stream::Verbatim(stream) => stream.abort(err),
			Stream::Clock | Stream::Ignored => {}
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

/// Tracks how far a self-describing audio stream has scanned without finding a frame.
///
/// A frame header that doesn't parse means sync was lost: a damaged byte, a dropped PES,
/// or a splice (a looping file wraps mid-frame). The stream scans to the next sync-word
/// candidate and resumes, so a few lost milliseconds of audio stay a gap in one track
/// rather than an error that takes the whole session down.
///
/// A scanned candidate is not trusted on its own. Sync words are short enough to occur by
/// chance in compressed payload (a valid-looking MP2 header turns up about every 25 KiB of
/// random bytes, an ADTS one every 5 KiB), so a candidate is only accepted once a second
/// header parses exactly where the frame it declares ends. Without that the scan would
/// publish payload bytes as audio, and worse, each false positive would reset the budget
/// below and keep a stream that never really parses scanning forever.
///
/// A frame joined out of a carried tail is confirmed the same way, even though the previous
/// frame vouched for where that tail begins. What it vouched for is the boundary, not the
/// bytes the next PES supplies, and a splice joins two unrelated halves whose seam a header
/// alone can't see. See [`needs_confirmation`](Self::needs_confirmation).
///
/// The budget is what keeps that from failing silently. A PID whose frames never parse
/// (a PMT declaring a stream type the payload doesn't match) would otherwise scan
/// forever, publishing nothing and holding its catalog reservation open, which withholds
/// the catalog for every other track. Past the budget the parse error propagates, so a
/// stream that is simply the wrong codec still fails the way it always has.
struct Resync {
	/// Bytes discarded since the last frame was emitted.
	discarded: usize,
	/// Whether the next frame comes from a scan rather than the previous frame's end, and
	/// so has to be confirmed before it can be published.
	unconfirmed: bool,
	/// End of stream: nothing more can arrive to confirm anything, so publish what parses
	/// rather than drop a frame that is whole.
	draining: bool,
}

impl Default for Resync {
	fn default() -> Self {
		Self {
			discarded: 0,
			// A stream starts unconfirmed for the same reason a scan does: nothing has
			// vouched for the boundary yet. A capture joins mid-stream, so the first PES
			// routed to a PID can open in the middle of a frame, and a chance header there
			// would publish payload as audio and take the track's sample rate and channel
			// count from it for the life of the broadcast.
			unconfirmed: true,
			draining: false,
		}
	}
}

impl Resync {
	/// Roughly a second of audio at the highest legacy bitrate: orders of magnitude more
	/// than the one or two frames a damaged header costs, and short enough that a stream
	/// that never parses fails while its capture is still on screen.
	const BUDGET: usize = 64 * 1024;

	/// Where to resume after the header at `offset` failed to parse, discarding what is
	/// skipped.
	///
	/// The offset it picks is only a candidate: the caller confirms it by parsing a header
	/// there and comes back here if that fails too. That mirrors how the TS layer
	/// reacquires packet alignment, where a lone sync byte is a guess until the next
	/// packet confirms it.
	///
	/// `offset` must leave room for a header, which is the loop condition at both call
	/// sites; the scan starts one byte past it, since a header just failed to parse there.
	fn recover(&mut self, data: &[u8], offset: usize, codec: &SyncWord) -> Recover {
		self.unconfirmed = true;
		if let Some(rel) = memchr::memchr(codec.sync_byte, &data[offset + 1..]) {
			let found = offset + 1 + rel;
			self.discarded += found - offset;
			return Recover::At(found);
		}
		// No candidate left, so only a partial sync word can remain: keep that much for
		// the next PES (the word can straddle the boundary) and discard the rest.
		let keep = data.len().saturating_sub(codec.min_header_len - 1).max(offset);
		self.discarded += keep - offset;
		Recover::Carry(keep)
	}

	/// Give up once a stream has scanned further than [`BUDGET`](Self::BUDGET) without
	/// emitting a frame.
	fn exhausted(&self) -> bool {
		self.discarded > Self::BUDGET
	}

	/// Whether the offset itself is one nothing has vouched for, which is true from a scan
	/// (or the start of a stream, or a seek) until the frame it found is published.
	fn unconfirmed(&self) -> bool {
		self.unconfirmed
	}

	/// Whether the frame at the current offset has to be confirmed by a header where it ends
	/// before it can be published. Beyond an unconfirmed offset, that covers a frame
	/// beginning in a carried tail: the previous frame vouched for where the tail starts, but
	/// nothing vouches for the bytes joined onto it, and at a splice the two halves are
	/// unrelated. Confirming only these keeps the cost off the common path, since a frame
	/// that begins inside a PES is whole by the time it is parsed.
	fn needs_confirmation(&self, in_tail: bool) -> bool {
		!self.draining && (self.unconfirmed || in_tail)
	}

	/// A frame was published, so the stream is back in sync: the next frame starts where
	/// this one ended and needs no confirmation of its own.
	fn recovered(&mut self) {
		self.discarded = 0;
		self.unconfirmed = false;
	}

	/// Undo the scanning charged since `discarded`. Those bytes turned out to be retained
	/// rather than discarded, and charging for bytes we keep would fail a large frame that
	/// legitimately arrives over many small PES.
	fn refund(&mut self, discarded: usize) {
		self.discarded = discarded;
	}

	/// A discontinuity: whatever vouched for the next boundary no longer applies, so the
	/// frame after it has to be confirmed again.
	///
	/// The budget resets too. It measures how long *this* run of the stream has gone without
	/// a frame, and a seek ends that run: carrying the count over would fail a perfectly
	/// good stream on its first parse failure after seeking away from the damage.
	fn desynced(&mut self) {
		self.unconfirmed = true;
		self.discarded = 0;
	}

	/// End of stream. Nothing more can arrive to confirm the carried tail, so take it as-is
	/// rather than drop a frame that is whole and parses.
	fn drain(&mut self) {
		self.unconfirmed = false;
		self.draining = true;
	}
}

/// Where a stream picks up after losing frame sync. See [`Resync::recover`].
enum Recover {
	/// Try to parse a frame header at this offset.
	At(usize),
	/// Nothing else in this buffer can start a frame; carry from this offset into the
	/// next PES.
	Carry(usize),
}

/// A candidate passed over for declaring a frame longer than the buffer holds, kept in case
/// nothing later in the buffer confirms.
///
/// Restoring it has to undo the scan that followed: its bytes end up retained rather than
/// discarded, and it still belongs to the tail its timestamp was derived from.
struct Fallback {
	offset: usize,
	discarded: usize,
	pts: Option<Timestamp>,
	in_tail: bool,
}

/// What a resync needs to know about the codec it is scanning for.
struct SyncWord {
	/// Bytes needed to attempt a header parse.
	min_header_len: usize,
	/// First byte of the frame sync word.
	sync_byte: u8,
}

impl SyncWord {
	/// ADTS: an 11-bit sync of all ones, then at least 7 bytes of header.
	const ADTS: Self = Self {
		min_header_len: adts::MIN_HEADER_LEN,
		sync_byte: 0xFF,
	};
}

impl From<&legacy::Descriptor> for SyncWord {
	fn from(descriptor: &legacy::Descriptor) -> Self {
		Self {
			min_header_len: descriptor.min_header_len,
			sync_byte: descriptor.sync_byte,
		}
	}
}

/// AAC needs the first ADTS header before it can build a [`aac::Import`]
/// (the sample rate and channel layout aren't in the PMT), so creation is
/// deferred until the first frame arrives.
struct AacStream<E: CatalogExt = ()> {
	import: Option<aac::Import<E>>,
	broadcast: moq_net::broadcast::Producer,
	/// Reservation held from the PMT until the first frame builds the importer, so the catalog stays
	/// withheld until this deferred rendition resolves (config comes from the first ADTS header).
	/// Consumed when `import` is built.
	reserved: Option<crate::catalog::Reserved<E>>,
	unwrap: PtsUnwrap,
	/// Largest audio burst span seen, published as the catalog jitter.
	jitter: Option<Timestamp>,
	/// Partial frame left at the end of the previous PES. ISO 13818-1 doesn't require
	/// audio frames to align with PES boundaries, so a legitimate mux can split one.
	tail: Vec<u8>,
	/// PTS for the frame the tail begins, computed when it was cut. The PES PTS only
	/// covers frames that begin in that PES.
	tail_pts: Option<Timestamp>,
	resync: Resync,
}

impl<E: CatalogExt> AacStream<E> {
	fn write(&mut self, pending: Pending, run_start: Option<u64>) -> anyhow::Result<()> {
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

		// PTS for the next frame to emit. The tail frame keeps the PTS computed at its cut;
		// the first frame that BEGINS in this PES takes the PES PTS (per ISO 13818-1).
		let mut pts = if carried > 0 { self.tail_pts.take() } else { pes_base };
		let mut in_tail = carried > 0;

		// A single PES can carry several ADTS frames; split and feed each raw frame.
		let mut offset = 0;
		let mut index = 0u64;
		let mut sample_rate = None;
		// Earliest candidate passed over for declaring a frame longer than the buffer holds,
		// carried only if nothing later in the buffer confirms.
		let mut fallback = None;
		while offset + adts::MIN_HEADER_LEN <= data.len() {
			if in_tail && offset >= carried {
				pts = pes_base;
				in_tail = false;
			}

			// Parse the frame here, and unless the previous frame vouched for this offset and
			// for the bytes past it, require a header where the frame it declares ends before
			// believing it. See `Resync`. `Err(None)` means nothing parsed badly, the
			// candidate just isn't usable.
			let confirm = self.resync.needs_confirmation(in_tail);
			let parsed: Result<_, Option<anyhow::Error>> = match adts::Header::parse(&data[offset..]) {
				Ok(header) => {
					let end = offset + header.frame_len;
					if end > data.len() {
						if !self.resync.unconfirmed() {
							// A boundary the previous frame vouched for: the frame continues in
							// the next PES, so finish it there. A joined tail waits here too, since
							// nothing can confirm a frame the buffer doesn't hold yet.
							break;
						}
						// Unconfirmed, and the length it declares outruns the buffer, so a split
						// frame and a false sync claiming a length the stream never delivers look
						// identical. Remember it, but prefer any later candidate that does fit
						// and confirm: waiting here would swallow the real frames inside the
						// range this one claims. A byte of a real ADTS header is 0xFF often
						// enough for this to matter.
						fallback.get_or_insert(Fallback {
							offset,
							discarded: self.resync.discarded,
							pts,
							in_tail,
						});
						Err(None)
					} else if !confirm {
						Ok((header, end))
					} else if end + adts::MIN_HEADER_LEN > data.len() {
						// Whole, but too few bytes left to confirm it. Carry it and retry once
						// the next PES extends the buffer, rather than trusting it now.
						break;
					} else {
						adts::Header::parse(&data[end..]).map(|_| (header, end)).map_err(Some)
					}
				}
				Err(err) => Err(Some(err)),
			};

			let (header, end) = match parsed {
				Ok(found) => found,
				Err(err) => {
					// Sync is lost; scan to the next candidate. See `Resync`.
					if self.resync.exhausted() {
						let context = format!("AAC stream never regained sync after {} bytes", self.resync.discarded);
						return Err(match err {
							Some(err) => err.context(context),
							None => anyhow::Error::msg(context),
						});
					}
					// Re-anchor only where the tail is actually abandoned: restoring a fallback
					// keeps the candidate, so it keeps the timestamp it was found with.
					match self.resync.recover(data, offset, &SyncWord::ADTS) {
						Recover::At(next) => {
							if in_tail {
								pts = pes_base;
								in_tail = false;
							}
							offset = next;
							continue;
						}
						// Nothing in the buffer confirmed, so fall back to the earliest
						// candidate that was merely too long; it may complete next PES.
						Recover::Carry(next) => {
							match fallback {
								Some(found) => {
									self.resync.refund(found.discarded);
									offset = found.offset;
									pts = found.pts;
									in_tail = found.in_tail;
								}
								None => {
									if in_tail {
										pts = pes_base;
										in_tail = false;
									}
									offset = next;
								}
							}
							break;
						}
					}
				}
			};
			sample_rate = Some(header.sample_rate);

			let import = match &mut self.import {
				Some(import) => import,
				None => {
					let config = aac::Config {
						profile: header.object_type,
						sample_rate: header.sample_rate,
						channel_count: header.channel_count,
					};
					// Consume the reservation held since the PMT: this resolves the gated rendition,
					// and carries the catalog's declared media retention onto the track.
					// The importer synthesizes the AudioSpecificConfig `description` from the config so
					// out-of-band consumers (fMP4/MKV export, WebCodecs) can configure the decoder.
					let reserved = self.reserved.take().expect("aac reservation already consumed");
					let track = self.broadcast.unique_track(".aac", reserved.track_info())?;
					let aac = aac::Import::new(track, reserved, config.into())?;
					self.import.insert(aac)
				}
			};

			import.decode(&data[offset + header.header_len..end], pts)?;
			// The importer accumulates; cut each ADTS frame into its own group (one QUIC stream)
			// so the relay forwards it without waiting for the next.
			import.cut(None)?;
			self.resync.recovered();
			// Offsets behind the published frame are spent; carrying one would republish it.
			fallback = None;

			// Every ADTS frame is 1024 samples.
			pts = advance_pts(pts, 1024, header.sample_rate)?;
			offset = end;
			index += 1;
		}

		// Keep any partial frame (cut mid-frame, or even mid-header) for the next PES,
		// with the PTS it should carry.
		if offset < data.len() {
			if in_tail && offset >= carried {
				pts = pes_base;
			}
			self.tail = data[offset..].to_vec();
			self.tail_pts = pts;
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
		// A seek is a discontinuity like any other.
		self.desync();
		if let Some(import) = &mut self.import {
			import.seek(sequence)?;
		}
		Ok(())
	}

	/// The partial frame will never see its end, and whatever vouched for the next frame
	/// boundary no longer applies.
	fn desync(&mut self) {
		self.tail.clear();
		self.tail_pts = None;
		self.resync.desynced();
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		// Drain a frame held only for want of a successor to confirm it: at end of stream
		// that successor is never coming. Only once this stream has published a frame,
		// though. Before that nothing has vouched for any boundary, so accepting one here
		// would hand a capture that joined mid-frame and ended immediately the same false
		// frame that starting unconfirmed exists to reject, and build the track's config out
		// of it.
		if !self.tail.is_empty() && self.import.is_some() {
			self.resync.drain();
			self.write(Pending::empty(), None)?;
		}
		// A partial frame at end of stream isn't emissible; drop it, but leave a trace for
		// diagnosing truncated captures.
		if !self.tail.is_empty() {
			tracing::debug!(bytes = self.tail.len(), "dropping partial ADTS frame at end of stream");
		}
		// Nothing was ever published here, so this rendition will never resolve. Release its
		// reservation or it gates the initial catalog publish for every other track: `finish`
		// takes streams by reference, and callers keep the importer alive past it.
		if self.import.is_none() {
			self.reserved.take();
		}
		if let Some(import) = &mut self.import {
			import.finish()?;
		}
		Ok(())
	}

	fn abort(mut self, err: moq_net::Error) {
		if let Some(import) = self.import.take() {
			import.abort(err);
		}
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
					Some(base.checked_add(advance.convert(base.scale())?)?)
				}
				other => other,
			};
			self.import.decode(packet, pts)?;
			// The importer accumulates; cut each Opus packet into its own group (one QUIC stream)
			// so the relay forwards it without waiting for the next.
			self.import.cut(None)?;

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

	fn abort(self, err: moq_net::Error) {
		self.import.abort(err);
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
	broadcast: moq_net::broadcast::Producer,
	/// Reservation held from the PMT until the first frame builds the importer, so the catalog stays
	/// withheld until this deferred rendition resolves (config comes from the first frame header).
	/// Consumed when `import` is built.
	reserved: Option<crate::catalog::Reserved<E>>,
	unwrap: PtsUnwrap,
	/// Partial frame left at the end of the previous PES. ISO 13818-1 doesn't
	/// require audio frames to align with PES boundaries, so a legitimate mux can
	/// split one; it's reassembled here. A lost or spliced PES makes the join
	/// garbage, which [`Resync`] then scans past.
	tail: Vec<u8>,
	/// PTS for the frame the tail begins, computed when it was cut. The PES PTS
	/// only covers frames that begin in that PES.
	tail_pts: Option<Timestamp>,
	resync: Resync,
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
		// Earliest candidate passed over for declaring a frame longer than the buffer holds,
		// carried only if nothing later in the buffer confirms.
		let mut fallback = None;
		while offset + self.descriptor.min_header_len <= data.len() {
			if in_tail && offset >= carried {
				pts = pes_base;
				in_tail = false;
			}

			// Parse the frame here, and unless the previous frame vouched for this offset and
			// for the bytes past it, require a header where the frame it declares ends before
			// believing it. See `Resync`. `Err(None)` means nothing parsed badly, the
			// candidate just isn't usable.
			let confirm = self.resync.needs_confirmation(in_tail);
			let parsed: Result<_, Option<legacy::Error>> = match (self.descriptor.parse)(&data[offset..]) {
				Ok(header) => {
					let end = offset + header.len;
					if end > data.len() {
						if !self.resync.unconfirmed() {
							// A boundary the previous frame vouched for: the frame continues in
							// the next PES, so finish it there. A joined tail waits here too, since
							// nothing can confirm a frame the buffer doesn't hold yet.
							break;
						}
						// Unconfirmed, and the length it declares outruns the buffer, so a split
						// frame and a false sync claiming a length the stream never delivers look
						// identical. Remember it, but prefer any later candidate that does fit
						// and confirm: waiting here would swallow the real frames inside the
						// range this one claims.
						fallback.get_or_insert(Fallback {
							offset,
							discarded: self.resync.discarded,
							pts,
							in_tail,
						});
						Err(None)
					} else if !confirm {
						Ok((header, end))
					} else if end + self.descriptor.min_header_len > data.len() {
						// Whole, but too few bytes left to confirm it. Carry it and retry once
						// the next PES extends the buffer, rather than trusting it now.
						break;
					} else {
						(self.descriptor.parse)(&data[end..])
							.map(|_| (header, end))
							.map_err(Some)
					}
				}
				Err(err) => Err(Some(err)),
			};

			let (header, end) = match parsed {
				Ok(found) => found,
				Err(err) => {
					// Sync is lost. Scan past this byte to the next candidate and let the
					// next iteration confirm it by parsing there.
					if self.resync.exhausted() {
						// Nothing in this PID parses, so it isn't the codec its PMT declares.
						// That's a mux or config error rather than damage: fail loudly instead
						// of scanning forever behind an unresolved catalog reservation.
						let context = format!(
							"{} stream never regained sync after {} bytes",
							self.descriptor.track_suffix, self.resync.discarded
						);
						return Err(match err {
							Some(err) => anyhow::Error::new(err).context(context),
							None => anyhow::Error::msg(context),
						});
					}
					// The tail's frame boundary is what we just lost, so its PTS no longer
					// describes anything. Re-anchor on the PES, whose PTS covers the first
					// frame starting in it: wrong by less than one PES, where a stale tail
					// (a loop wrap carries the PTS from before it) can be wrong by hours.
					// Only where the tail is actually abandoned, though: restoring a fallback
					// keeps the candidate, so it keeps the timestamp it was found with.
					match self.resync.recover(data, offset, &self.descriptor.into()) {
						Recover::At(next) => {
							if in_tail {
								pts = pes_base;
								in_tail = false;
							}
							offset = next;
							continue;
						}
						// Nothing in the buffer confirmed, so fall back to the earliest
						// candidate that was merely too long; it may complete next PES.
						Recover::Carry(next) => {
							match fallback {
								Some(found) => {
									self.resync.refund(found.discarded);
									offset = found.offset;
									pts = found.pts;
									in_tail = found.in_tail;
								}
								None => {
									if in_tail {
										pts = pes_base;
										in_tail = false;
									}
									offset = next;
								}
							}
							break;
						}
					}
				}
			};

			let import = match &mut self.import {
				Some(import) => import,
				None => {
					let config = legacy::Config {
						sample_rate: header.sample_rate,
						channel_count: header.channel_count,
					};
					// Consume the reservation held since the PMT: this resolves the gated rendition,
					// and carries the catalog's declared media retention onto the track.
					let reserved = self.reserved.take().expect("legacy reservation already consumed");
					let track = self
						.broadcast
						.unique_track(self.descriptor.track_suffix, reserved.track_info())?;
					let legacy = legacy::Import::new(self.descriptor, track, reserved, config)?;
					self.import.insert(legacy)
				}
			};

			import.decode(&data[offset..end], pts)?;
			// The importer accumulates; cut each frame into its own group (one QUIC stream)
			// so the relay forwards it without waiting for the next.
			import.cut(None)?;
			self.resync.recovered();
			// Offsets behind the published frame are spent; carrying one would republish it.
			fallback = None;

			pts = advance_pts(pts, header.samples, header.sample_rate)?;
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
		// A seek is a discontinuity like any other.
		self.desync();
		if let Some(import) = &mut self.import {
			import.seek(sequence)?;
		}
		Ok(())
	}

	/// The partial frame will never see its end, and whatever vouched for the next frame
	/// boundary no longer applies.
	fn desync(&mut self) {
		self.tail.clear();
		self.tail_pts = None;
		self.resync.desynced();
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		// Drain a frame held only for want of a successor to confirm it: at end of stream
		// that successor is never coming. Only once this stream has published a frame,
		// though. Before that nothing has vouched for any boundary, so accepting one here
		// would hand a capture that joined mid-frame and ended immediately the same false
		// frame that starting unconfirmed exists to reject, and build the track's config out
		// of it.
		if !self.tail.is_empty() && self.import.is_some() {
			self.resync.drain();
			self.write(Pending::empty())?;
		}
		// A partial frame at end of stream isn't emissible verbatim; drop it, but
		// leave a trace for diagnosing truncated captures.
		if !self.tail.is_empty() {
			tracing::debug!(
				suffix = self.descriptor.track_suffix,
				bytes = self.tail.len(),
				"dropping partial frame at end of stream"
			);
		}
		// Nothing was ever published here, so this rendition will never resolve. Release its
		// reservation or it gates the initial catalog publish for every other track: `finish`
		// takes streams by reference, and callers keep the importer alive past it.
		if self.import.is_none() {
			self.reserved.take();
		}
		if let Some(import) = &mut self.import {
			import.finish()?;
		}
		Ok(())
	}

	fn abort(mut self, err: moq_net::Error) {
		if let Some(import) = self.import.take() {
			import.abort(err);
		}
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

/// Swallow a [`MissingKeyframe`](crate::container::MissingKeyframe) from a video
/// decode: a TS capture can join mid-GOP, so the deltas before the first keyframe
/// have no group to anchor and are simply dropped rather than aborting the demux.
fn skip_missing_keyframe(result: crate::Result<()>) -> anyhow::Result<()> {
	match result {
		Ok(()) | Err(crate::Error::MissingKeyframe(_)) => Ok(()),
		Err(e) => Err(e.into()),
	}
}

/// Advance a PES-derived PTS past one frame of `samples` at `sample_rate`.
///
/// Per frame rather than `index * constant`: E-AC-3 varies the samples per frame, and a
/// resync means the frames in a PES are no longer a contiguous run to index into.
fn advance_pts(pts: Option<Timestamp>, samples: u64, sample_rate: u32) -> anyhow::Result<Option<Timestamp>> {
	let Some(pts) = pts else {
		return Ok(None);
	};
	// `pts` is a 90 kHz PES PTS; rescale the sample-rate advance to match before adding
	// (the scale-aware Timestamp rejects mixed scales).
	let advance = Timestamp::from_scale(samples, sample_rate as u64)?;
	Ok(Some(pts.checked_add(advance.convert(pts.scale())?)?))
}

/// Convert a raw 90 kHz PTS to a microsecond [`Timestamp`], unwrapping the
/// 33-bit field. Returns `None` when the PES carried no PTS.
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

	/// A drift budget no test timeline comes close to, so the reader sees every group.
	///
	/// The media track's full retention window, so a reader started after importing can
	/// still read every retained group. These tests import a whole file first, which the default
	/// [`Duration::ZERO`] budget collapses to the live
	/// edge: completeness has to be asked for.
	const RECORDING_MAX_AGE: std::time::Duration = Duration::from_secs(30);
	use mpeg2ts::es::StreamType;

	use super::{Continuation, Continuity, SectionReassembler};
	use moq_net::Timestamp;
	use std::time::Duration;

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

	/// A continuation packet carrying PCR and/or OPCR in its adaptation field.
	fn clock_packet(cc: u8, pcr: Option<[u8; 6]>, opcr: Option<[u8; 6]>, body: &[u8]) -> Vec<u8> {
		let flags = if pcr.is_some() { 0x10 } else { 0 } | if opcr.is_some() { 0x08 } else { 0 };
		let length = 1 + usize::from(pcr.is_some()) * 6 + usize::from(opcr.is_some()) * 6;
		let mut p = vec![0x47, 0x00, 0x21, 0x30 | (cc & 0x0f), length as u8, flags];
		if let Some(pcr) = pcr {
			p.extend_from_slice(&pcr);
		}
		if let Some(opcr) = opcr {
			p.extend_from_slice(&opcr);
		}
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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(&[(StreamType::Dts8ChannelLosslessAudio, 0x21)], true));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		import.decode(&bytes).unwrap(); // must not abort on the private section

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
		import.finish().unwrap();
		// SCTE detection takes no lock here (video/audio would still publish later): the old
		// discarding ScteStream took the lock and republished an empty catalog on this path.
		assert!(
			tokio::time::timeout(Duration::from_millis(10), updates.next())
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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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

		assert!(
			!import.streams.contains_key(&pid),
			"upgrade drops the stale Ignored route"
		);
		assert_eq!(
			catalog.snapshot().mpegts.tracks.len(),
			1,
			"upgrade advertises the cue track"
		);

		// The importer clears its verbatim entries from the catalog when it drops, so read the
		// track name while it is still registered.
		let name = catalog.snapshot().mpegts.tracks.keys().next().unwrap().clone();
		import.finish().unwrap();
		let track = consumer
			.track(&name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_age(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy);
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read())
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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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

	/// Open a bounded audio PES whose declared payload is longer than this packet carries.
	fn audio_pes_open(pid: u16, cc: u8, pts: u64, declared: usize, payload: &[u8]) -> Vec<u8> {
		assert!(payload.len() < declared, "the test PES must remain open");
		let pts_field = [
			0x21 | (((pts >> 30) & 0x07) << 1) as u8,
			((pts >> 22) & 0xff) as u8,
			0x01 | (((pts >> 15) & 0x7f) << 1) as u8,
			((pts >> 7) & 0xff) as u8,
			0x01 | ((pts & 0x7f) << 1) as u8,
		];
		let mut pes = vec![0x00, 0x00, 0x01, 0xc0];
		let pes_len = 3 + 5 + declared;
		pes.push((pes_len >> 8) as u8);
		pes.push((pes_len & 0xff) as u8);
		pes.extend_from_slice(&[0x80, 0x80, 0x05]);
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
			p.push(0x00);
			p.extend(std::iter::repeat_n(0xff, af_len - 1));
		}
		p.extend_from_slice(&pes);
		assert_eq!(p.len(), 188, "open audio PES packet must fill exactly one TS packet");
		p
	}

	/// Build a non-PUSI TS payload packet with adaptation-field stuffing.
	fn ts_continuation(pid: u16, cc: u8, payload: &[u8]) -> Vec<u8> {
		let af_len = 184 - 1 - payload.len();
		let mut p = vec![0x47, ((pid >> 8) as u8 & 0x1f), (pid & 0xff) as u8, 0x30 | (cc & 0x0f)];
		p.push(af_len as u8);
		if af_len > 0 {
			p.push(0x00);
			p.extend(std::iter::repeat_n(0xff, af_len - 1));
		}
		p.extend_from_slice(payload);
		assert_eq!(p.len(), 188, "continuation packet must fill exactly one TS packet");
		p
	}

	// MP2/AC-3 flush like any audio PES but don't consume the jitter hint; if one
	// anchored the audio run, an AAC PID in the same TS would publish a jitter
	// inflated by the inter-PID PTS offset.
	#[test]
	fn verbatim_audio_does_not_anchor_aac_jitter() {
		const AAC_PID: u16 = 0x0060;
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[(StreamType::AdtsAac, AAC_PID), (StreamType::Mpeg1Audio, MP2_PID)],
			false,
		));
		// A whole MP2 frame (MPEG-1 Layer II, 32 kbps, 48 kHz, stereo = 96 bytes),
		// 2 s ahead of the AAC PES that follows in the same audio run.
		// Two frames, in their own PES: a frame is confirmed by the one after it, so a lone
		// frame on a PID that never establishes sync is not published at all.
		let mut mp2 = vec![0xFF, 0xFD, 0x14, 0x00];
		mp2.resize(96, 0xAA);
		bytes.extend_from_slice(&audio_pes_packet(MP2_PID, 0, 90_000, &mp2));
		bytes.extend_from_slice(&audio_pes_packet(MP2_PID, 1, 92_160, &mp2));

		// Two ADTS frames: a frame is confirmed by the one after it, so a lone frame is only
		// published at end of stream, after the jitter accounting for its PES has run.
		let mut aac = Vec::new();
		for _ in 0..2 {
			aac.extend_from_slice(&super::adts::write_header(2, 48_000, 2, 8).unwrap());
			aac.extend_from_slice(&[0u8; 8]);
		}
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
			jitter <= Duration::from_millis(100),
			"AAC jitter anchored on a foreign PID: {jitter:?}"
		);
	}

	/// Read every retained frame of the single audio rendition in `catalog`.
	async fn read_audio_frames(
		consumer: &moq_net::broadcast::Consumer,
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
		let track = consumer
			.track(&name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_age(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy);
		let mut frames = Vec::new();
		while let Ok(Ok(Some(frame))) = tokio::time::timeout(Duration::from_millis(50), reader.read()).await {
			frames.push(frame);
		}
		frames
	}

	// The production shape from #2729, on a real capture: a looping publisher plays part of
	// a file and wraps to the top, so the audio PES open at the cut leaves a tail that
	// splices onto the file's first frames. Every wrap used to end the session with
	// "missing AC-3 sync word", which is what accumulated 216 restarts on the reporter's
	// feed. AC-3 also exercises a different sync word and parser than the synthetic MP2
	// and ADTS tests above.
	#[tokio::test(start_paused = true)]
	async fn legacy_survives_a_looping_file_wrap() {
		let data = include_bytes!("test_data/ac3.ts");

		// Wrap mid-file on a packet boundary, so the TS layer stays aligned and the
		// discontinuity lands squarely on the audio frame the last PES left open.
		let cut = (data.len() / 2) / 188 * 188;
		let mut looped = data[..cut].to_vec();
		looped.extend_from_slice(data);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&bytes::BytesMut::from(&looped[..]))
			.expect("a loop wrap must not end the session");
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		// Every frame published is still a whole AC-3 frame: resync lands on sync words,
		// never mid-frame.
		assert!(
			frames.iter().all(|f| f.payload[0] == 0x0B && f.payload[1] == 0x77),
			"resync published a frame that doesn't start at an AC-3 sync word"
		);
		// The wrap costs at most the frame it interrupted, not the rest of the stream.
		let pristine = {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve());
			import.decode(&bytes::BytesMut::from(&data[..])).unwrap();
			import.finish().unwrap();
			read_audio_frames(&consumer, &catalog).await.len()
		};
		// The wrap costs nothing after it: the whole second copy is published, on top of
		// the frames the truncated first pass already delivered.
		assert!(
			frames.len() > pristine,
			"the wrap swallowed frames: {} looped vs {pristine} pristine",
			frames.len()
		);
	}

	/// Read every retained frame of the single video rendition in `catalog`.
	async fn read_video_frames(
		consumer: &moq_net::broadcast::Consumer,
		catalog: &crate::catalog::Producer,
	) -> Vec<crate::container::Frame> {
		let name = catalog
			.snapshot()
			.video
			.renditions
			.keys()
			.next()
			.expect("a video track")
			.clone();
		let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
		let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy);
		let mut frames = Vec::new();
		while let Ok(Ok(Some(frame))) = tokio::time::timeout(Duration::from_millis(50), reader.read()).await {
			frames.push(frame);
		}
		frames
	}

	/// Annex-B bytes for one access unit: SPS + PPS + IDR for a keyframe, else a delta slice.
	fn annexb_au(keyframe: bool) -> Vec<u8> {
		use crate::container::test_util::{IDR, PPS, SPS};
		let nals: &[&[u8]] = if keyframe {
			&[SPS, PPS, IDR]
		} else {
			&[&[0x41, 0x9a, 0x00, 0x01]]
		};
		let mut out = Vec::new();
		for nal in nals {
			out.extend_from_slice(&[0, 0, 0, 1]);
			out.extend_from_slice(nal);
		}
		out
	}

	// A break mid-picture is not the same as a break mid-audio. One video PES is exactly one
	// access unit, so there is no whole unit ahead of the cut to salvage: publishing what
	// arrived would hand the decoder a picture with missing slices, and a keyframe missing
	// slices stays wrong for every picture that references it. Drop it and wait for the next.
	#[tokio::test(start_paused = true)]
	async fn video_drops_a_partial_access_unit_across_a_continuity_break() {
		const VIDEO_PID: u16 = 0x0050;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::H264, VIDEO_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// A keyframe cut in half by the wrap: the PES declares more than this packet carries.
		let whole = annexb_au(true);
		import
			.decode(audio_pes_open(VIDEO_PID, 0, 90_000, whole.len() + 32, &whole[..28]).as_slice())
			.unwrap();
		// The wrap completes the open PES with unrelated bytes, and continuity gives it away.
		import
			.decode(ts_continuation(VIDEO_PID, 12, &whole[28..]).as_slice())
			.unwrap();
		// A clean keyframe after it, which is what the track should carry.
		import
			.decode(audio_pes_packet(VIDEO_PID, 13, 270_000, &whole).as_slice())
			.unwrap();
		import
			.decode(audio_pes_packet(VIDEO_PID, 14, 450_000, &annexb_au(false)).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_video_frames(&consumer, &catalog).await;
		assert!(
			frames.iter().all(|f| f.payload.len() >= whole.len() || !f.keyframe),
			"a keyframe with missing slices reached the track: {:?}",
			frames.iter().map(|f| (f.keyframe, f.payload.len())).collect::<Vec<_>>()
		);
		assert_eq!(
			frames.first().map(|f| f.payload.to_vec()),
			Some(whole.clone()),
			"the first published picture is not the whole keyframe"
		);
	}

	/// One whole ADTS frame carrying `raw_len` bytes of `fill`. `fill` must not be 0xFF,
	/// which a resync would mistake for a frame sync.
	fn adts_frame(raw_len: usize, fill: u8) -> Vec<u8> {
		let mut f = super::adts::write_header(2, 48_000, 2, raw_len).unwrap().to_vec();
		f.resize(raw_len + 7, fill);
		f
	}

	// The AAC mirror of `legacy_drops_a_pes_completed_across_a_continuity_break`.
	#[tokio::test(start_paused = true)]
	async fn aac_drops_a_pes_completed_across_a_continuity_break() {
		const AAC_PID: u16 = 0x0060;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		let mut opening = adts_frame(40, 0xAA);
		opening.extend_from_slice(&adts_frame(40, 0xBB)[..25]);
		import
			.decode(audio_pes_open(AAC_PID, 0, 90_000, 94, &opening).as_slice())
			.unwrap();
		import
			.decode(ts_continuation(AAC_PID, 12, &adts_frame(40, 0xCC)[..32]).as_slice())
			.unwrap();
		let mut normal = adts_frame(40, 0xDD);
		normal.extend_from_slice(&adts_frame(40, 0xEE));
		import
			.decode(audio_pes_packet(AAC_PID, 13, 270_000, &normal).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.to_vec()).collect::<Vec<_>>(),
			[0xAA, 0xDD, 0xEE]
				.map(|fill| adts_frame(40, fill)[7..].to_vec())
				.to_vec(),
			"the wrap was spliced onto the frame the cut left open"
		);
	}

	// ISO 13818-1 doesn't require AAC frames to align with PES boundaries any more than it
	// does the legacy codecs, but only the legacy path reassembled a split frame: AAC
	// rejected the PES outright, so a mux that split one killed the broadcast on well-formed
	// input, with no corruption or discontinuity involved.
	#[tokio::test(start_paused = true)]
	async fn aac_frame_split_across_pes_reassembles() {
		const AAC_PID: u16 = 0x0060;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		let frame = adts_frame(40, 0x5A);
		import
			.decode(audio_pes_packet(AAC_PID, 0, 90_000, &frame[..25]).as_slice())
			.expect("a split ADTS frame is not fatal");
		// The rest of the split frame, plus the frame that confirms its boundary.
		let mut rest = frame[25..].to_vec();
		rest.extend_from_slice(&adts_frame(40, 0x77));
		import
			.decode(audio_pes_packet(AAC_PID, 1, 270_000, &rest).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(frames.len(), 2, "the split frame is reassembled, not dropped");
		// The ADTS header is stripped: the track carries raw AAC.
		assert_eq!(frames[0].payload.as_ref(), &frame[7..], "reassembled byte-exact");
		// It began in PES 1 (90000 ticks = 1 s), so PES 2's PTS must not apply to it.
		assert_eq!(frames[0].timestamp, Timestamp::from_micros(1_000_000).unwrap());
	}

	// AAC resyncs past a damaged header for the same reason the legacy codecs do.
	#[tokio::test(start_paused = true)]
	async fn aac_resyncs_past_damaged_header() {
		const AAC_PID: u16 = 0x0060;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// Two good frames to lock onto (a frame is confirmed by the one after it), then a
		// frame whose syncword lost a bit, then two more good ones.
		let mut payload = adts_frame(40, 0xAA);
		payload.extend_from_slice(&adts_frame(40, 0xBB));
		import
			.decode(audio_pes_packet(AAC_PID, 0, 90_000, &payload).as_slice())
			.unwrap();

		let mut damaged = adts_frame(40, 0x11);
		damaged[0] = 0xFE;
		let mut hit = damaged.clone();
		hit.extend_from_slice(&adts_frame(40, 0xCC));
		import
			.decode(audio_pes_packet(AAC_PID, 1, 270_000, &hit).as_slice())
			.expect("a damaged ADTS header is not fatal");
		import
			.decode(audio_pes_packet(AAC_PID, 2, 450_000, &adts_frame(40, 0xDD)).as_slice())
			.unwrap();
		import.finish().unwrap();

		// The track carries raw AAC, so each published frame is its ADTS body.
		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.to_vec()).collect::<Vec<_>>(),
			[0xAA, 0xBB, 0xCC, 0xDD]
				.map(|fill| adts_frame(40, fill)[7..].to_vec())
				.to_vec(),
			"the undamaged frames either side survive, and only the damaged one is dropped"
		);
	}

	// AAC reassembles a split frame the same way the legacy codecs do, so it splices the same
	// way at a wrap and confirms the join for the same reason. See
	// `legacy_confirms_a_frame_joined_out_of_a_carried_tail`.
	#[tokio::test(start_paused = true)]
	async fn aac_confirms_a_frame_joined_out_of_a_carried_tail() {
		const AAC_PID: u16 = 0x0060;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// A whole frame, then one cut mid-frame by the wrap.
		let mut payload = adts_frame(40, 0xAA);
		payload.extend_from_slice(&adts_frame(40, 0xBB)[..25]);
		import
			.decode(audio_pes_packet(AAC_PID, 0, 90_000, &payload).as_slice())
			.unwrap();
		// The top of the file again: the tail splices onto its first frame.
		let mut wrapped = adts_frame(40, 0xCC);
		wrapped.extend_from_slice(&adts_frame(40, 0xDD));
		import
			.decode(audio_pes_packet(AAC_PID, 1, 270_000, &wrapped).as_slice())
			.expect("a splice is not fatal");
		import
			.decode(audio_pes_packet(AAC_PID, 2, 450_000, &adts_frame(40, 0xEE)).as_slice())
			.unwrap();
		import.finish().unwrap();

		// The track carries raw AAC, so each published frame is its ADTS body.
		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.to_vec()).collect::<Vec<_>>(),
			[0xAA, 0xCC, 0xDD, 0xEE]
				.map(|fill| adts_frame(40, fill)[7..].to_vec())
				.to_vec(),
			"the splice cost more than the frame it interrupted"
		);
		assert_eq!(
			frames[1].timestamp,
			Timestamp::from_micros(3_000_000).unwrap(),
			"not re-anchored on the new PES"
		);
	}

	// The end-of-stream drain, for AAC. See `legacy_drains_a_joined_frame_at_end_of_stream`.
	#[tokio::test(start_paused = true)]
	async fn aac_drains_a_joined_frame_at_end_of_stream() {
		const AAC_PID: u16 = 0x0060;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		let split = adts_frame(40, 0xBB);
		let mut payload = adts_frame(40, 0xAA);
		payload.extend_from_slice(&split[..25]);
		import
			.decode(audio_pes_packet(AAC_PID, 0, 90_000, &payload).as_slice())
			.unwrap();
		// The rest of the split frame and nothing else, so the stream ends with it joined,
		// whole, and unconfirmed.
		import
			.decode(audio_pes_packet(AAC_PID, 1, 270_000, &split[25..]).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.to_vec()).collect::<Vec<_>>(),
			[0xAA, 0xBB].map(|fill| adts_frame(40, fill)[7..].to_vec()).to_vec(),
			"the last frame was held for a confirmation that could never arrive"
		);
	}

	/// One whole MPEG-2 Layer II frame (8 kbps, 16 kHz, mono = 72 bytes), filled with
	/// `fill` so frames are told apart on the wire. Small enough that two fit in the
	/// single TS packet [`audio_pes_packet`] builds. `fill` must not be 0xFF, which a
	/// resync would mistake for a frame sync.
	fn mp2_frame(fill: u8) -> Vec<u8> {
		let mut f = vec![0xFF, 0xF5, 0x18, 0xC0];
		f.resize(72, fill);
		f
	}

	// The shape a looping mux actually produces, which is not a carried tail: the last PES
	// before the cut is truncated, so it stays open, and the next loop's leading continuation
	// packets complete it. The foreign bytes land in the SAME PES, so the codec sees one
	// buffer with nothing carried, and the confirmation rule above never gets a say. The
	// continuity counter is what gives the wrap away, so the truncated PES is flushed for the
	// whole frames it did deliver and the stream resumes at the next PES start.
	#[tokio::test(start_paused = true)]
	async fn legacy_drops_a_pes_completed_across_a_continuity_break() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		let mut opening = mp2_frame(0xAA);
		opening.extend_from_slice(&mp2_frame(0xBB)[..40]);
		import
			.decode(audio_pes_open(MP2_PID, 0, 90_000, 144, &opening).as_slice())
			.unwrap();
		import
			.decode(ts_continuation(MP2_PID, 12, &mp2_frame(0xCC)[..32]).as_slice())
			.unwrap();
		let mut normal = mp2_frame(0xDD);
		normal.extend_from_slice(&mp2_frame(0xEE));
		import
			.decode(audio_pes_packet(MP2_PID, 13, 270_000, &normal).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xAA), mp2_frame(0xDD), mp2_frame(0xEE)],
			"the wrap was spliced onto the frame the cut left open"
		);
	}

	// A damaged frame header must not take the session down with it: the demuxer scans to
	// the next sync word and keeps publishing, the way the TS and video layers already do.
	// One bit flipped in a sync word used to abort the whole broadcast (#2729).
	#[tokio::test(start_paused = true)]
	async fn legacy_resyncs_past_damaged_header() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// Two good frames to lock onto (a frame is confirmed by the one after it, and two
		// 72-byte frames is all one TS packet holds), then a frame whose sync word lost a
		// bit, then two more good ones.
		let mut payload = mp2_frame(0xAA);
		payload.extend_from_slice(&mp2_frame(0xBB));
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &payload).as_slice())
			.unwrap();

		let mut damaged = mp2_frame(0x11);
		damaged[0] = 0xFE;
		let mut hit = damaged.clone();
		hit.extend_from_slice(&mp2_frame(0xCC));
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &hit).as_slice())
			.expect("a damaged frame header is not fatal");
		import
			.decode(audio_pes_packet(MP2_PID, 2, 450_000, &mp2_frame(0xDD)).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xAA), mp2_frame(0xBB), mp2_frame(0xCC), mp2_frame(0xDD)],
			"the undamaged frames either side survive, and only the damaged one is dropped"
		);
	}

	// The reported production shape: a looping publisher wraps mid-frame, so the carried
	// tail is spliced onto unrelated bytes and the join can't parse. The stale tail must be
	// scanned past rather than ending the session, and the recovered frame takes the new
	// PES's PTS instead of the pre-wrap one it would have inherited.
	#[tokio::test(start_paused = true)]
	async fn legacy_resyncs_past_stale_tail_at_a_splice() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// PES 1 ends mid-frame, leaving a tail the wrap will never complete.
		let mut cut = mp2_frame(0xAA);
		cut.truncate(40);
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &cut).as_slice())
			.unwrap();
		// PES 2 is the top of the file again: the tail splices onto its first frame.
		let mut wrapped = mp2_frame(0xCC);
		wrapped.extend_from_slice(&mp2_frame(0xDD));
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &wrapped).as_slice())
			.expect("a splice is not fatal");
		// One more PES to show the wrap costs nothing after it.
		import
			.decode(audio_pes_packet(MP2_PID, 2, 450_000, &mp2_frame(0xEE)).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		// The stale tail still carries an intact header claiming 72 bytes, so what gives the
		// splice away is the confirmation: the frame it declares ends inside the wrapped
		// frame rather than at a header. The tail is scanned past, not published.
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xCC), mp2_frame(0xDD), mp2_frame(0xEE)],
			"the splice was published as audio instead of scanned past"
		);
		// Re-anchored on PES 2 (270000 ticks = 3 s) rather than inheriting the stale tail's
		// 1 s. At a real loop wrap that inherited error is the whole file's duration, not a
		// frame.
		assert_eq!(
			frames[0].timestamp,
			Timestamp::from_micros(3_000_000).unwrap(),
			"not re-anchored on the new PES"
		);
	}

	// The same splice one frame later, which is the shape a real wrap takes: the stream has
	// already published a frame, so the tail sits at a boundary the previous frame vouched
	// for. What it vouched for is where the tail begins, not the unrelated bytes the next PES
	// joins onto it, so the join still has to be confirmed. Without that this published one
	// frame of pre-wrap and post-wrap bytes spliced together, and swallowed the real frame
	// underneath it (#2802).
	#[tokio::test(start_paused = true)]
	async fn legacy_confirms_a_frame_joined_out_of_a_carried_tail() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// A whole frame, then one cut mid-frame by the wrap. The first vouches for where the
		// tail begins, which is what made the join look trustworthy.
		let mut payload = mp2_frame(0xAA);
		payload.extend_from_slice(&mp2_frame(0xBB)[..40]);
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &payload).as_slice())
			.unwrap();
		// The top of the file again: the tail splices onto its first frame.
		let mut wrapped = mp2_frame(0xCC);
		wrapped.extend_from_slice(&mp2_frame(0xDD));
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &wrapped).as_slice())
			.expect("a splice is not fatal");
		import
			.decode(audio_pes_packet(MP2_PID, 2, 450_000, &mp2_frame(0xEE)).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xAA), mp2_frame(0xCC), mp2_frame(0xDD), mp2_frame(0xEE)],
			"the splice cost more than the frame it interrupted"
		);
		// The wrapped frame keeps the new PES's PTS, not the pre-wrap tail's.
		assert_eq!(
			frames[1].timestamp,
			Timestamp::from_micros(3_000_000).unwrap(),
			"not re-anchored on the new PES"
		);
	}

	// Confirming a joined frame means holding it when the buffer ends too soon after it to
	// hold a header. At end of stream that header is never coming, so the drain publishes it
	// anyway rather than dropping a frame that is whole and parses.
	#[tokio::test(start_paused = true)]
	async fn legacy_drains_a_joined_frame_at_end_of_stream() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		let mut payload = mp2_frame(0xAA);
		payload.extend_from_slice(&mp2_frame(0xBB)[..40]);
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &payload).as_slice())
			.unwrap();
		// The rest of the split frame and nothing else, so the stream ends with it joined,
		// whole, and unconfirmed.
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &mp2_frame(0xBB)[40..]).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xAA), mp2_frame(0xBB)],
			"the last frame was held for a confirmation that could never arrive"
		);
	}

	// A sync word is short enough to turn up by chance in compressed payload (a valid-looking
	// MP2 header lands about every 25 KiB of random bytes). Scanning onto one and trusting it
	// would publish payload as audio, so a scanned candidate is only accepted once a header
	// parses where the frame it declares ends. Here the planted header's frame would end in
	// the middle of the next real frame, which is what gives it away.
	#[tokio::test(start_paused = true)]
	async fn legacy_rejects_an_unconfirmed_sync_candidate() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// Two good frames to lock onto, then a damaged one with an intact header planted 8
		// bytes into its payload for the scan to find, then two more good ones.
		let mut payload = mp2_frame(0xAA);
		payload.extend_from_slice(&mp2_frame(0xBB));
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &payload).as_slice())
			.unwrap();

		let mut damaged = mp2_frame(0x11);
		damaged[0] = 0xFE;
		damaged[8..12].copy_from_slice(&mp2_frame(0)[..4]);
		let mut hit = damaged.clone();
		hit.extend_from_slice(&mp2_frame(0xCC));
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &hit).as_slice())
			.unwrap();
		import
			.decode(audio_pes_packet(MP2_PID, 2, 450_000, &mp2_frame(0xDD)).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		// Without confirmation the planted header is published as a frame of payload bytes,
		// and it eats the front of the next real frame on the way past.
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xAA), mp2_frame(0xBB), mp2_frame(0xCC), mp2_frame(0xDD)],
			"a false sync inside the payload was published as audio"
		);
	}

	// A capture joins mid-stream, so the first PES routed to a PID can open in the middle of
	// a frame whose start was never seen. Nothing vouches for that boundary, so the first
	// frame is confirmed like a scanned one: otherwise a chance header there is published as
	// audio and, worse, the track takes its sample rate and channel count from it for the
	// life of the broadcast.
	#[tokio::test(start_paused = true)]
	async fn legacy_confirms_the_first_frame_of_a_stream() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// Join mid-frame: a fragment that happens to open with a valid MP2 header. It declares
		// 72 bytes but only 40 arrive before the real stream, so the frame it claims ends
		// inside a real one. (Confirmation catches a false sync whose declared length misses
		// a real boundary; one that happens to land on it is indistinguishable.)
		let mut joined = mp2_frame(0)[..4].to_vec();
		joined.resize(40, 0x11);
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &joined).as_slice())
			.unwrap();
		// The real stream, which does chain.
		let mut real = mp2_frame(0xCC);
		real.extend_from_slice(&mp2_frame(0xDD));
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &real).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert!(
			!frames.iter().any(|f| f.payload.ends_with(&[0x11; 8])),
			"published the unvouched-for bytes the capture joined in the middle of"
		);
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xCC), mp2_frame(0xDD)],
			"the real stream is picked up once two frames chain"
		);
	}

	// A seek is a discontinuity, so whatever vouched for the next boundary no longer holds
	// and the frame after it is confirmed like any other unvouched-for one.
	#[tokio::test(start_paused = true)]
	async fn legacy_confirms_the_first_frame_after_a_seek() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		let mut opening = mp2_frame(0xAA);
		opening.extend_from_slice(&mp2_frame(0xBB));
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &opening).as_slice())
			.unwrap();

		import.seek(10).unwrap();

		// Landing mid-frame after the seek, on a fragment that parses as a header but whose
		// declared frame ends inside a real one.
		let mut landed = mp2_frame(0)[..4].to_vec();
		landed.resize(40, 0x11);
		import
			.decode(audio_pes_packet(MP2_PID, 1, 270_000, &landed).as_slice())
			.unwrap();
		let mut real = mp2_frame(0xCC);
		real.extend_from_slice(&mp2_frame(0xDD));
		import
			.decode(audio_pes_packet(MP2_PID, 2, 450_000, &real).as_slice())
			.unwrap();
		import.finish().unwrap();

		// Only the four real frames: the fragment the seek landed inside is dropped rather
		// than spliced onto the front of the first real frame after it.
		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xAA), mp2_frame(0xBB), mp2_frame(0xCC), mp2_frame(0xDD)],
			"published the unvouched-for bytes the seek landed in the middle of"
		);
	}

	// A frame that legitimately arrives over many small PES must not trip the resync budget.
	// The candidate is rescanned each time the buffer grows, and charging those bytes on
	// every pass would bill a single 8 KiB frame for over a hundred KiB of "discarded" data
	// and fail the stream before it ever completes. Bytes that end up retained are refunded.
	#[tokio::test(start_paused = true)]
	async fn legacy_does_not_bill_a_frame_that_arrives_slowly() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// The largest frame MPEG-1 Layer II can declare (384 kbps at 32 kHz), dribbled in 4
		// bytes at a time so it is rescanned 432 times. Billing each pass would charge ~364
		// KiB against a 64 KiB budget.
		let mut frame = vec![0xFF, 0xFD, 0xE8, 0x00];
		frame.resize(1728, 0xAA);
		for (i, chunk) in frame.chunks(4).enumerate() {
			import
				.decode(audio_pes_packet(MP2_PID, (i % 16) as u8, 90_000, chunk).as_slice())
				.expect("a slowly arriving frame must not exhaust the resync budget");
		}
		// Enough of the next frame to confirm the boundary.
		import
			.decode(audio_pes_packet(MP2_PID, 0, 270_000, &frame[..4]).as_slice())
			.unwrap();
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![frame.clone()],
			"the reassembled frame keeps its bytes"
		);
		// It began in the first PES, so it keeps that PTS rather than the one it completed under.
		assert_eq!(frames[0].timestamp, Timestamp::from_micros(1_000_000).unwrap());
	}

	// A PID that never publishes must not hold the catalog shut for the rest of the
	// broadcast. Its rendition is reserved from the PMT and only consumed when the importer
	// is built, and `finish` takes streams by reference (moq-srt finishes the importer and
	// keeps it), so a reservation left alive there gates the initial catalog publish
	// indefinitely. A lone frame that can never be confirmed is exactly such a PID.
	#[tokio::test(start_paused = true)]
	async fn a_pid_that_never_publishes_releases_the_catalog() {
		const MP2_PID: u16 = 0x0061;
		const AAC_PID: u16 = 0x0060;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(
			&[(StreamType::AdtsAac, AAC_PID), (StreamType::Mpeg1Audio, MP2_PID)],
			false,
		);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// MP2 carries a single frame, which nothing can ever confirm, so it is dropped and
		// the importer for that PID is never built.
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &mp2_frame(0xAA)).as_slice())
			.unwrap();
		// AAC resolves normally.
		let mut aac = adts_frame(40, 0xCC);
		aac.extend_from_slice(&adts_frame(40, 0xDD));
		import
			.decode(audio_pes_packet(AAC_PID, 0, 90_000, &aac).as_slice())
			.unwrap();
		import.finish().unwrap();

		// The importer is deliberately still alive here, as moq-srt leaves it after finish.
		let track = consumer
			.track(hang::catalog::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_age(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy);
		let published = tokio::time::timeout(Duration::from_millis(50), reader.read()).await;
		assert!(
			matches!(published, Ok(Ok(Some(_)))),
			"a PID that never published held the catalog shut: {published:?}"
		);
		drop(import);
	}

	// Resync is bounded: a PID carrying something other than the codec its PMT declares is a
	// config error, not damage, so it must still fail rather than scan forever behind an
	// unresolved catalog reservation (which would withhold the catalog for every track).
	//
	// The junk is seeded with header-shaped bytes because that is what defeats a naive
	// budget: every false positive that gets published resets it, so a stream that never
	// really parses would scan forever. Only confirmed frames may reset it.
	#[test]
	fn legacy_gives_up_when_nothing_ever_parses() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// A valid MP2 header every 16 bytes, none of which is ever a real frame boundary:
		// the frames they declare are 72 bytes long, which never lands on another header.
		let header = mp2_frame(0);
		let junk: Vec<u8> = (0..150)
			.map(|i: usize| match i % 16 {
				n @ 0..=3 => header[n],
				_ => 0xBB,
			})
			.collect();

		let err = (0..1000)
			.map(|i| import.decode(audio_pes_packet(MP2_PID, (i % 16) as u8, 90_000, &junk).as_slice()))
			.find_map(Result::err)
			.expect("an unparseable stream must still fail");
		assert!(
			err.to_string().contains("never regained sync"),
			"gave up with the wrong error: {err}"
		);
	}

	// A seek is a discontinuity, so the budget spent failing to find a frame before it says
	// nothing about the stream after it. Carrying it over means a stream that scanned close
	// to the budget, then seeked somewhere clean, dies on its first parse failure with
	// "never regained sync" despite being perfectly in sync.
	#[tokio::test(start_paused = true)]
	async fn legacy_seek_resets_the_resync_budget() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// Burn the budget down to just under the give-up threshold: these discard 147 bytes
		// and then 150 apiece (a 3-byte partial sync is carried), so 436 of them leaves
		// 65397 against a 65536 budget.
		let junk = vec![0x00u8; 150];
		for i in 0..436 {
			import
				.decode(audio_pes_packet(MP2_PID, (i % 16) as u8, 90_000, &junk).as_slice())
				.expect("still under the budget");
		}

		// Seek away from the damage. Landing mid-frame costs a couple more failures, which
		// is what tips a carried-over budget past the threshold.
		import.seek(10).unwrap();
		for i in 0..2 {
			import
				.decode(audio_pes_packet(MP2_PID, i, 270_000, &junk).as_slice())
				.expect("a seek must not inherit the pre-seek budget");
		}

		// Then a perfectly good stream.
		let mut good = mp2_frame(0xAA);
		good.extend_from_slice(&mp2_frame(0xBB));
		import
			.decode(audio_pes_packet(MP2_PID, 2, 450_000, &good).as_slice())
			.expect("a clean stream after a seek must not inherit the pre-seek budget");
		import.finish().unwrap();

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames.iter().map(|f| f.payload.clone()).collect::<Vec<_>>(),
			vec![mp2_frame(0xAA), mp2_frame(0xBB)],
			"the post-seek stream should publish normally"
		);
	}

	// The AAC counterpart of the above. Worth its own test rather than trusting the shared
	// `Resync`: this path scans for the ADTS sync byte rather than a descriptor's, and
	// builds its give-up error by adding context to the `anyhow` one `adts::Header::parse`
	// returns, where the legacy path wraps a `thiserror` value.
	#[test]
	fn aac_gives_up_when_nothing_ever_parses() {
		const AAC_PID: u16 = 0x0060;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// A valid ADTS header every 16 bytes, none of which is ever a real frame boundary:
		// the 47-byte frames they declare never end on another header.
		let header = adts_frame(40, 0);
		let junk: Vec<u8> = (0..150)
			.map(|i: usize| match i % 16 {
				n if n < super::adts::MIN_HEADER_LEN => header[n],
				_ => 0xBB,
			})
			.collect();

		let err = (0..1000)
			.map(|i| import.decode(audio_pes_packet(AAC_PID, (i % 16) as u8, 90_000, &junk).as_slice()))
			.find_map(Result::err)
			.expect("an unparseable stream must still fail");
		assert!(
			err.to_string().contains("never regained sync"),
			"gave up with the wrong error: {err}"
		);
	}

	// ISO 13818-1 doesn't require audio frames to align with PES boundaries: a
	// frame split across two PES must be reassembled byte-exact, stamped with the
	// PTS of the PES it began in, and the next whole frame takes the new PES's PTS.
	#[tokio::test(start_paused = true)]
	async fn legacy_frame_split_across_pes_reassembles() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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
		let track = consumer
			.track(&name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_age(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy);
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read())
			.await
			.expect("cue read timed out")
			.unwrap()
			.expect("a published cue frame");

		assert_eq!(&frame.payload[..], &CUE[..], "verbatim splice_info_section");
		assert_ne!(frame.timestamp, Timestamp::ZERO, "cue must not stamp zero");
		// The legacy container normalizes the wire timestamp to microseconds, so compare the
		// instant (not the raw scale) against the 90 kHz media clock the cue was stamped with.
		assert_eq!(
			Duration::from(frame.timestamp),
			Duration::from(clock),
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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		// catalog::Ext (not the base catalog) makes a wrong ensure_scte() observable: it
		// would create a rendition, which the base catalog silently drops.
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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
	fn tei_pusi_section_is_dropped() {
		// A PUSI flagged TEI must not start a section out of payload the demodulator has
		// already disowned. Resetting the counter is not enough: the packet itself is junk.
		let mut corrupt = packet(true, 0, 0, &CUE);
		corrupt[1] |= 0x80; // transport_error_indicator
		let clean = packet(true, 1, 0, &CUE);
		assert_eq!(
			run(&[corrupt, clean]),
			vec![CUE.to_vec()],
			"a section was emitted from a packet flagged corrupt"
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
	fn duplicate_with_refreshed_clock_is_skipped() {
		let old = [0x00, 0x00, 0x00, 0x00, 0x7e, 0x00];
		let refreshed = [0x00, 0x00, 0x00, 0x01, 0x7e, 0x00];
		for (name, pcr, opcr) in [("PCR", Some(old), None), ("OPCR", None, Some(old))] {
			let section = fake_section(0xfc, 390);
			let p1 = packet(true, 0, 0, &section[..183]);
			let p2 = clock_packet(1, pcr, opcr, &section[183..359]);
			let duplicate = clock_packet(1, pcr.map(|_| refreshed), opcr.map(|_| refreshed), &section[183..359]);
			let p3 = packet(false, 2, 0, &section[359..]);
			assert_eq!(run(&[p1, p2, duplicate, p3]), vec![section], "refreshed {name}");
		}
	}

	#[test]
	fn repeated_counter_after_fifteen_lost_packets_is_broken() {
		let mut continuity = Continuity::default();
		let previous: [u8; 188] = packet(false, 5, 0, &[0x11; 184]).try_into().unwrap();
		let after_loss: [u8; 188] = packet(false, 5, 0, &[0x22; 184]).try_into().unwrap();

		assert!(matches!(continuity.observe(&previous), Continuation::Contiguous));
		assert!(matches!(continuity.observe(&after_loss), Continuation::Broken));
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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, Catalog::<Ext>::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

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

		let track = consumer
			.track(name.as_str())
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_age(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy);
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read())
			.await
			.expect("verbatim read timed out")
			.unwrap()
			.expect("a published verbatim frame");
		assert_eq!(&frame.payload[..], &payload[..], "verbatim PES payload round-trips");
	}
}
