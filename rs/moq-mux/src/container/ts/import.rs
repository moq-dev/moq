//! MPEG-TS demuxer.
//!
//! [`Import`] reads a TS byte stream, reassembles PES packets per PID, and
//! routes their payloads to the codec importers (H.264/H.265/AAC, plus the
//! legacy MP2/AC-3/E-AC-3 verbatim path), which own their broadcast tracks and
//! catalog entries. Elementary streams we don't decode are carried verbatim, one
//! MoQ track per PID, described in the `mpegts` catalog section: PES streams ride
//! the normal PES reassembly and travel as whole PES packets at their placement on
//! the program clock, while section-framed streams (SCTE-35 and other private
//! sections, which are not PES) are reassembled as sections, as the PAT and PMT
//! are. TS adds PAT/PMT discovery, PES reassembly, the
//! private-section path, and the 90 kHz PTS conversion across its 33-bit wrap. The service
//! layer is captured too: the identity parsed from the PAT as a
//! [`Program`](catalog::Program) record in the `mpegts` catalog section, and the
//! standalone SI PIDs as opaque sections on per-`(PID, table_id)` snapshot tracks
//! (see [`si`](super::si)), so both survive the round-trip.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Context;
use mpeg2ts::es::StreamType;
use mpeg2ts::ts::{Pid, TsPacket};

use super::adts;
use super::catalog;
use super::demux::{Demux, Packet};
use super::health::{Continuation, Continuity, Health};
use super::mux_rate::PCR_WRAP;
use super::passthrough::{PCR_HZ, PCR_OFFSET, Sample, locate};
use super::psi::{self, PesStart, Pmt};
use super::stats;
use crate::catalog::Offset;
use crate::catalog::hang::CatalogExt;
use crate::codec::{aac, ac3, eac3, h264, h265, legacy, mp2, opus};
use moq_net::Timestamp;

#[path = "video.rs"]
mod video;

/// Demuxes an MPEG-TS byte stream into a MoQ broadcast.
///
/// Decodes H.264 (stream type 0x1B), H.265 (0x24), ADTS AAC (0x0F), MP2
/// (0x03/0x04), AC-3 (0x81), and E-AC-3 (0x87). Each codec stream is fed to its importer, which
/// manages the track, catalog config, and keyframe-based group boundaries. MPEG-1/2 video
/// (0x01/0x02) is not decoded but still paces the program: its PTS anchors the timebase while
/// its PES ride whole as below.
///
/// Video access units can span PES packets, and one PES can contain several. Its PTS
/// stamps the AU whose required start-code prefix begins there, once. Later AUs need
/// anchored POC and proportional timing: AVC's TS timing descriptor plus fixed-rate
/// SPS VUI, or HEVC's `poc_proportional_to_timing_flag`. Missing timing ends the import;
/// it is not recoverable packet damage. Raw elementary-stream import keeps its
/// caller-supplied timestamp mode.
///
/// Partial header contents hold at most 264 bytes per PID. PES payloads, retained video
/// AUs, and sections awaiting a timestamp share half a MoQ group's byte budget;
/// buffered units also share half its frame budget. Exceeding either limit refuses the input instead of retaining more bytes.
///
/// Elementary streams we don't decode are carried verbatim, one MoQ track per
/// PID, when the catalog `E` carries the [`mpegts`](super::Mpegts) section: section-framed
/// streams (private sections 0x05, the ISO 13818-6 DSM-CC types 0x0A to 0x0D, and SCTE-35 0x86
/// marked by a program-level 'CUEI' registration descriptor) are reassembled as sections, and
/// any other stream travels as whole PES packets ([`PesPacket`](super::PesPacket)), LATM/LOAS
/// AAC (0x11) included. A 0x86 stream without 'CUEI', which may be DTS audio or SCTE-35, is not
/// carried. With a base `Catalog<()>` no undecoded stream is carried.
///
/// Every PID is accounted for. A listed stream that is not carried counts once in
/// [`stats::Stream::refused`] with a warning, and so does each TS-scrambled packet on a carried
/// one. Once the PMT is in, a packet on a PID no table claims (the null PID aside) counts in its
/// own row: see [`stats::Snapshot::streams`]. That holds when the PAT lists only the imported
/// program; a [`with_program`](Self::with_program) import of a multiplex cannot tell another
/// program's PIDs from unclaimed ones, so it leaves them to [`Programs`](super::Programs).
///
/// A whole PES or section is timestamped with its placement: the program clock at its first byte,
/// interpolated between the PCRs either side of it (ISO 13818-1 2.4.2.2) and shifted onto the
/// catalog clock as the media is. It waits for the PCR after it, and for the first media PTS
/// to anchor the timebase, or the first PCR when the PMT declares no media stream, within the
/// staging bound below; one that starts past the last PCR before a
/// clock break or the end of input is extrapolated from the last two. One before the first PCR,
/// or with no interval to extrapolate from, is dropped and counted in
/// [`stats::Stream::unplaced`]. A section's SCTE-35 splice times absorb the media's offset in
/// `pts_adjustment`; a whole PES's PTS and DTS move by that offset, and every other byte is kept.
/// A stream whose PES carry a clock only the PTS and DTS of are rebased (ESCR, a
/// `pack_header_field`, a TREF), PES-level scrambling, an unbounded non-video PES, or a header
/// export would refuse is refused rather than altered. A program with no PCR refuses both
/// whole PES and section streams: [`stats::Stream::refused`]. A PMT that moves the PCR PID ends the
/// placement timebase, as a clock break does; one that drops the PCR refuses those streams, and
/// one that names it again carries them.
///
/// The PAT and every PMT are reassembled the same way, so a table that spans packets or
/// sits behind a nonzero pointer_field is read whole. A PAT or PMT section whose CRC-32
/// fails is dropped and counted in [`stats::Snapshot::crc_error`]; the last good table stays
/// in force.
///
/// With the `mpegts` catalog extension, the initial catalog waits for a two-second PCR
/// measurement window. This hold applies to every subscriber of the broadcast.
/// A variable-rate window publishes without a rate, as does four seconds of media without a
/// complete PCR window; EOF releases a shorter input too.
/// Imports without that extension do not wait for a rate measurement.
///
/// The selected container applies only to decoded media renditions. Verbatim tracks in the
/// `mpegts` catalog section continue to use the legacy Hang container.
///
/// PTS values follow the program's timestamp history across their 33-bit wrap,
/// including the PCR history before a data-only program gains media.
pub struct Import<E: catalog::Catalog = ()> {
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer<E>,
	container: hang::catalog::Container,

	/// Held until the first media PTS or data-only PCR anchors the clock, withholding the catalog
	/// until every stream in the initial program has reserved its rendition and the root `clock` is
	/// final. A one-shot muxer (fMP4, TS re-export) sees the complete track list in the first
	/// snapshot rather than a half-converged one. See [`Reserved`](crate::catalog::Reserved).
	initial_reservation: Option<crate::catalog::Reserved<E>>,
	/// Hold the whole catalog until one PCR measurement window completes, or enough media time
	/// passes without one, independently of decoder readiness.
	mux_rate_reservation: Option<crate::catalog::Reserved<E>>,
	/// Media time seen while that hold is active, summed across time-base breaks so a source
	/// that keeps resetting its clock still reaches the bound.
	held: std::time::Duration,
	/// The newest 90 kHz PTS of the current time base counted into `held`.
	held_edge: Option<u64>,

	/// Whether the current PMT declares a media stream that can anchor the program clock.
	media_clock: bool,

	/// The program's timestamp base: the first media PTS, or a data-only PCR, anchors it; every PTS shifts by
	/// its offset onto the catalog clock.
	timebase: crate::catalog::Timebase<E>,

	/// Frames the input [`decode`](Self::decode) reads. [`Programs`](super::Programs) runs one
	/// parser for every program instead and hands each its packets.
	demux: Demux,
	/// PIDs whose route changed while handling the current packet, for the parser to restart.
	restarts: Vec<u16>,
	/// Whether a PAT moved the PMT, or a PMT was handled, since
	/// [`claims_changed`](Self::claims_changed) last said so.
	claims_changed: bool,
	/// The PMT PID the latest PAT names for the imported program, with its reassembler.
	/// Several programs' PMTs may share one.
	pmt: Option<(Pid, SectionReassembler)>,
	/// PMT sections dropped for a CRC mismatch, keyed by PID.
	crc_errors: BTreeMap<u16, u64>,
	/// Per elementary-stream-PID codec routing.
	streams: HashMap<Pid, Stream<E>>,
	/// Counters from routes a later PMT replaced, keyed by PID.
	retired_stats: BTreeMap<u16, stats::Stream>,
	/// Damaged packets, PES, or access units refused, cumulative across PID remaps.
	damaged: BTreeMap<u16, u64>,
	damage_log: super::damage::Log,
	/// Access units per elementary stream, timed on the program clock.
	liveness: Liveness,
	/// In-progress PES reassembly, keyed by elementary PID.
	pending: HashMap<Pid, Pending>,
	/// Partial clear PES headers, at most 9 + 255 bytes per elementary PID, each with where its
	/// start code sits on the program clock ([`Placement::span`]).
	headers: HashMap<Pid, (Option<Span>, Vec<u8>)>,
	/// The program clock at each input byte, which places whole PES packets and sections.
	placement: Placement,
	/// Whole PES and sections dropped for want of a clock to place them, keyed by PID.
	unplaced: BTreeMap<u16, u64>,
	/// Undecoded streams refused rather than carried altered, keyed by PID.
	refused: BTreeMap<u16, u64>,
	/// The `stream_type` of each undecoded stream refused only for want of a PCR, routed again
	/// once a PMT names one or gives the PID another type.
	unclocked: HashMap<Pid, u8>,
	/// One program-wide budget for PES payloads and sections awaiting their clock.
	staging: Arc<Staging>,
	/// The TR 101 290 checks timed on the program clock.
	health: Health,
	/// PID the PMT designates as carrying the program clock reference. An
	/// adaptation-field `discontinuity_indicator` means a *system time-base*
	/// discontinuity only here; on any other PID it says nothing but that the
	/// continuity counter jumped ([`Continuation::Broken`]).
	pcr_pid: Option<Pid>,
	/// The input offset from which a PCR may start the clock a PMT names, while none does.
	/// Exporters may lead their first PAT/PMT with PCR, which the parser keeps per PID.
	unclocked_since: Option<u64>,
	/// The multiplex rate measured off the PCR PID, recorded in the `mpegts` section
	/// while the source holds one. Only fed with `mpegts` catalog support.
	mux_rate: super::mux_rate::Meter,
	/// Whether any media or section has been published since the last timebase break, so
	/// consecutive markers (a repeated flag, a retransmitted clock packet) declare one
	/// break rather than one each.
	published: bool,
	/// True once a PMT with at least one supported stream has been parsed.
	initialized: bool,

	/// Section-framed verbatim PIDs. Private sections (SCTE-35 table_id 0xFC and others)
	/// are not PES, so they are reassembled as sections rather than PES-parsed. Keyed by
	/// PID. SCTE-35 is detected via the PMT 'CUEI' registration descriptor.
	sections: HashMap<u16, SectionStream<E>>,
	/// Whether the catalog can carry the `mpegts` section, sampled once at construction.
	/// A base `Catalog<()>` can't, so its undecoded PIDs route to `Stream::Ignored`.
	supports_mpegts: bool,
	/// PMT ES-level descriptors per PID, stashed when a PMT is parsed so a decoded
	/// media track can record them (language, registration, ...) once its track exists.
	es_descriptors: HashMap<u16, Vec<catalog::Descriptor>>,
	/// Decoded media PIDs already recorded into `mpegts.tracks`, by the track name recorded, so the
	/// reconcile in [`Self::flush`] runs once per track rather than on every frame, and moves the
	/// entry when a format change publishes the PID under a new name.
	recorded_media: HashMap<Pid, String>,
	/// The program-level descriptors the catalog advertises, rewritten in place as the PMT
	/// revises them.
	program_info: Vec<catalog::Descriptor>,
	/// What the PMT in force says of each PID it lists, so a revision that says otherwise ends
	/// the route rather than carrying the new stream under the old name.
	listed: HashMap<Pid, Listing>,
	/// The SI store: buffers each sub-table to a complete generation, publishes the
	/// per-`(PID, table_id)` snapshot tracks, and advertises them in the catalog.
	si: super::si::Capture<E>,
	/// The `(transport_stream_id, program_number)` the first PAT named, also captured into the
	/// catalog service record with the PMT PID. A PAT naming another refuses the import: the
	/// carried SI describes this one.
	identity: Option<(u16, u16)>,
	/// Latest video PTS, used to timestamp service-information snapshots.
	last_pts: Option<Timestamp>,
	/// The program's PTS history, which picks each value's 33-bit wrap.
	pts: PtsUnwrap,
	/// The program number chosen by [`with_program`](Self::with_program). `None` imports the
	/// multiplex's only program and refuses a PAT that lists more than one.
	program: Option<u16>,
	/// The packets on PIDs no table claims, counted once [`alone`](Self::alone) and
	/// [`described`](Self::described). Under [`Programs`](super::Programs) this importer reads only
	/// the PIDs it claims, so `Programs` counts them instead, once for the multiplex.
	unclaimed: super::unclaimed::Unclaimed,
	/// The last PAT lists only the imported program, so a PID it does not claim is no other's.
	alone: bool,
	/// A PMT has described the program, so a PID it does not claim is not one still to come.
	described: bool,
	/// Carried PIDs already warned about a TS-scrambled packet.
	scrambled: HashSet<u16>,
	/// Whether [`finish`](Self::finish) or [`abort`](Self::abort) ended the input, even if finishing
	/// failed part way.
	finished: bool,
}

impl<E: catalog::Catalog> Import<E> {
	/// Import a byte stream into the broadcast using its reserved catalog producer.
	pub fn new(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		let container = hang::catalog::Container::default();
		// A long-lived producer handle for catalog edits (mpegts sections, later PMTs); the passed
		// reservation gates the initial publish and is dropped once the first PES anchors.
		let catalog = reserved.producer();
		// Sample the real catalog once at construction, not E::default(): an extension
		// may carry the section by value, and a snapshot clones under the mutex (no publish).
		let mut snapshot = catalog.snapshot();
		let mpegts = snapshot.ext.mpegts_mut();
		let supports_mpegts = mpegts.is_some();
		// What the catalog already advertises, so the first PMT republishes only a difference.
		let program_info = mpegts
			.map(|mpegts| std::mem::take(&mut mpegts.program_descriptors))
			.unwrap_or_default();
		if supports_mpegts {
			catalog.count_ext_tracks(|ext: &mut E| ext.mpegts_mut().is_some_and(|mpegts| mpegts.has_track()));
		}
		let si = super::si::Capture::new(broadcast.clone(), catalog.clone());
		let mux_rate_reservation = supports_mpegts.then(|| reserved.clone());
		Self {
			broadcast,
			catalog,
			container,
			media_clock: false,
			timebase: reserved.timebase(),
			initial_reservation: Some(reserved),
			mux_rate_reservation,
			held: std::time::Duration::ZERO,
			held_edge: None,
			demux: Demux::new(supports_mpegts),
			restarts: Vec::new(),
			claims_changed: false,
			pmt: None,
			crc_errors: BTreeMap::new(),
			streams: HashMap::new(),
			retired_stats: BTreeMap::new(),
			damaged: BTreeMap::new(),
			damage_log: Default::default(),
			liveness: Liveness::default(),
			pending: HashMap::new(),
			headers: HashMap::new(),
			placement: Placement::default(),
			unplaced: BTreeMap::new(),
			refused: BTreeMap::new(),
			unclocked: HashMap::new(),
			staging: Arc::default(),
			health: Health::default(),
			pcr_pid: None,
			unclocked_since: Some(0),
			mux_rate: Default::default(),
			published: false,
			initialized: false,
			sections: HashMap::new(),
			supports_mpegts,
			es_descriptors: HashMap::new(),
			recorded_media: HashMap::new(),
			program_info,
			listed: HashMap::new(),
			si,
			identity: None,
			last_pts: None,
			pts: PtsUnwrap::default(),
			program: None,
			unclaimed: Default::default(),
			alone: false,
			described: false,
			scrambled: HashSet::new(),
			finished: false,
		}
	}

	/// Import only the program numbered `program` in the PAT, ignoring the rest of the multiplex.
	///
	/// Without this, a PAT that lists more than one program fails the import with
	/// [`MultipleProgramsError`] rather than merging them onto one clock. A PAT that does not
	/// list `program` fails it too. The SI describes only this program's service: other
	/// services' EIT actual is dropped and the SDT actual lists this service alone.
	pub fn with_program(mut self, program: u16) -> Self {
		self.program = Some(program);
		self.si.select(program);
		self
	}

	/// Select the container this importer wraps decoded media renditions in.
	///
	/// [`Legacy`](hang::catalog::Container::Legacy) unless selected. It applies to every rendition
	/// this input demuxes, since the tracks are discovered rather than named by the caller.
	pub fn with_container(mut self, container: hang::catalog::Container) -> Self {
		self.container = container;
		self
	}

	/// Carry on the PMT `sections` began on `pid`, for an importer that starts mid-stream on the
	/// packet after the ones `sections` read.
	pub(super) fn with_pmt(mut self, pid: Pid, sections: SectionReassembler) -> Self {
		self.pmt = Some((pid, sections));
		self
	}

	/// Whether this importer reassembles the PMT sections on `pid`, and counts their CRC errors.
	pub(super) fn reads_pmt(&self, pid: u16) -> bool {
		self.pmt.as_ref().is_some_and(|(pmt, _)| pmt.as_u16() == pid)
	}

	/// Whether a route of this importer delivers what arrives on `pid`.
	pub(super) fn carries(&self, pid: u16) -> bool {
		self.sections.contains_key(&pid) || Pid::new(pid).is_ok_and(|pid| self.streams.contains_key(&pid))
	}

	/// Whether this importer holds a PMT section begun on `pid`, which it counts if corrupt.
	pub(super) fn assembling_pmt(&self, pid: u16) -> bool {
		self.pmt
			.as_ref()
			.is_some_and(|(pmt, sections)| pmt.as_u16() == pid && sections.assembling())
	}

	/// A reservation on this program's timebase, so every stream shifts by its offset.
	fn reserve(&self) -> crate::catalog::Reserved<E> {
		self.timebase.reserve()
	}

	/// The video hint for a decoded rendition: this importer's container, nothing else.
	fn video_hint(&self) -> crate::catalog::VideoHint {
		crate::catalog::VideoHint {
			container: self.container.clone(),
			..Default::default()
		}
	}

	/// Append `buf` to the internal scratch and demux every whole TS packet it
	/// now completes. The buffer is fully consumed; a trailing partial packet
	/// (< 188 bytes) is retained for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		let mut demux = std::mem::take(&mut self.demux);
		let result = self.decode_with(&mut demux, data);
		self.demux = demux;
		result
	}

	fn decode_with(&mut self, demux: &mut Demux, data: &[u8]) -> anyhow::Result<()> {
		demux.push(data);
		// Route one whole packet at a time, so a PMT is parsed (and any PID it declares
		// registered) before the packets that follow it in the same chunk route.
		loop {
			for pid in self.restarts.drain(..) {
				demux.restart(pid);
			}
			let Some(packet) = demux.next() else { break };
			self.packet(&packet)?;
		}
		demux.drain();
		self.flush_batch()
	}

	/// Cut the snapshot groups for whatever SI committed since the last call, and flush the
	/// damage log. Batching per decode call (plus the store's own host-clock debounce)
	/// coalesces a junction's burst of sub-table commits into few groups instead of one per
	/// commit.
	pub(super) fn flush_batch(&mut self) -> anyhow::Result<()> {
		self.damage_log.flush(moq_clock::now());
		self.si.flush(self.last_pts.unwrap_or(Timestamp::ZERO), false)
	}

	/// The PIDs this program reads besides the PAT and the standalone SI PIDs: its PMTs, its
	/// elementary streams and its clock.
	pub(super) fn claims(&self) -> Vec<u16> {
		let mut pids: Vec<u16> = self
			.pmt
			.iter()
			.map(|(pid, _)| pid)
			.chain(self.streams.keys())
			.chain(self.pcr_pid.iter())
			.map(Pid::as_u16)
			.chain(self.sections.keys().copied())
			.collect();
		pids.sort_unstable();
		pids.dedup();
		pids
	}

	/// Whether this program reads `pid`: one it [`claims`](Self::claims), or one claimed whatever
	/// the PMT says.
	fn claims_pid(&self, pid: u16) -> bool {
		super::unclaimed::Unclaimed::fixed(pid, self.supports_mpegts)
			|| self.sections.contains_key(&pid)
			|| Pid::new(pid).is_ok_and(|pid| {
				self.reads_pmt(pid.as_u16()) || self.streams.contains_key(&pid) || self.pcr_pid == Some(pid)
			})
	}

	/// The program clock: PCR ticks since its first PCR, or `None` before one.
	pub(super) fn now(&self) -> Option<u64> {
		self.liveness.now()
	}

	/// Whether [`claims`](Self::claims) may have changed since the last call.
	pub(super) fn claims_changed(&mut self) -> bool {
		std::mem::take(&mut self.claims_changed)
	}

	/// Drain the PIDs whose route changed since the last call, for the parser to restart.
	pub(super) fn restarts(&mut self) -> std::vec::Drain<'_, u16> {
		self.restarts.drain(..)
	}

	/// Route one packet the parser framed. Packets on PIDs this program does not
	/// [`claim`](Self::claims) may be skipped, except the PAT's and the SI PIDs' whole sections.
	pub(super) fn packet(&mut self, packet: &Packet) -> anyhow::Result<()> {
		let Packet {
			pkt,
			pid,
			offset,
			continuation,
			..
		} = *packet;
		// A packet flagged corrupt may carry another PID's number.
		if self.alone && self.described && pkt[1] & 0x80 == 0 && !self.claims_pid(pid) {
			self.unclaimed.packet(pid);
		}
		// Every packet is graded, but only the media PIDs and the clock route on the
		// verdict: PSI and section PIDs keep their reassemblers' own.
		self.health.packet(pkt, continuation, self.liveness.now());
		let continuation = Pid::new(pid)
			.ok()
			.filter(|pid| self.streams.contains_key(pid) || self.pcr_pid == Some(*pid))
			.map(|_| continuation);
		// A retransmitted payload must not repeat the clock reset it carried.
		if matches!(continuation, Some(Continuation::Duplicate)) {
			return Ok(());
		}
		// A media or PCR packet whose adaptation field overruns itself is refused before
		// its clock bits can reach the program clock. Its counter already joined the
		// chain, so a gap in front of it still salvages what came before.
		if let Ok(pid) = Pid::new(pid)
			&& (self.streams.contains_key(&pid) || self.pcr_pid == Some(pid))
			&& pkt[1] & 0x80 == 0
			&& !adaptation_valid(pkt)
		{
			if matches!(continuation, Some(Continuation::Broken)) {
				self.salvage(pid)?;
			}
			return self.damage(pid, &anyhow::anyhow!("malformed TS adaptation field"));
		}
		// Read the clock's own flag before routing, so the break lands between the media
		// either side of it. The PCR PID can be a dedicated one nothing below routes, and
		// the flag rides an adaptation-only packet as readily as a payload one.
		// A packet the demodulator flagged corrupt (`transport_error_indicator`) is not
		// read: its adaptation field is as untrustworthy as its payload, and taking a bit
		// out of it would break every track in the program on line noise.
		if self.pcr_pid.is_some_and(|p| p.as_u16() == pid) && pkt[1] & 0x80 == 0 {
			if discontinuity_indicator(pkt) {
				// A PES this packet starts ends the one before it on its PID whole, on the old
				// clock; only the other PIDs' are cut short by the break. An adaptation-only
				// packet starts none whatever its PUSI says, as `pes_packet` treats it.
				if pkt[1] & 0x40 != 0
					&& matches!(payload(pkt), Payload::Bytes(_))
					&& let Ok(pid) = Pid::new(pid)
				{
					self.flush(pid)?;
				}
				self.timebase_break()?;
				// The new timebase starts with the PCR the flag rides, if it rides one.
				if let Some(pcr) = pcr(pkt) {
					self.place_pcr(pcr, offset)?;
					self.si.pcr(pcr, packet.count);
				}
			} else if let Some(pcr) = pcr(pkt) {
				self.place_pcr(pcr, offset)?;
				self.liveness.pcr(pcr);
				self.si.pcr(pcr, packet.count);
				if let Some(now) = self.liveness.now() {
					self.health.tick(now);
				}
				if self.supports_mpegts {
					let ready = self.mux_rate.ready();
					if self.mux_rate.pcr(pcr, packet.count) {
						self.record_mux_rate()?;
					}
					if self.mux_rate.ready() {
						self.mux_rate_reservation = None;
					} else if ready {
						// A backwards clock voided the measurement.
						self.rearm_mux_rate();
					}
				}
			}
		}
		// TS-level scrambling hides a payload from all but a descrambler, so the semantic lane
		// cannot carry it where the raw lane can. Its adaptation field stays clear: the clock
		// above has read it.
		if pkt[1] & 0x80 == 0
			&& pkt[3] & 0xc0 != 0
			&& let Ok(pid) = Pid::new(pid)
			&& (self.sections.contains_key(&pid.as_u16())
				|| self
					.streams
					.get(&pid)
					.is_some_and(|stream| !matches!(stream, Stream::Ignored)))
		{
			return self.scrambled(pid);
		}
		if let Some(section) = self.sections.get_mut(&pid) {
			let unplaced = section.packet(pkt, offset, &self.placement)?;
			let (units, dropped) = section.release(&self.timebase, &self.pts, self.placement.generation)?;
			let unplaced = unplaced + dropped;
			if unplaced > 0 {
				*self.unplaced.entry(pid).or_default() += unplaced;
			}
			self.published |= units > 0;
			self.liveness.delivered(pid, units);
			return Ok(());
		}
		// The standalone SI PIDs carry the service layer, not media, and no PAT or PMT
		// names them. The parser reassembled their sections, each captured whatever its
		// `table_id`.
		if self.supports_mpegts && catalog::SI_PIDS.contains(&pid) {
			for section in packet.sections {
				if !self.si.section(pid, section.clone(), packet.count) {
					*self.unplaced.entry(pid).or_default() += 1;
				}
			}
			return Ok(());
		}
		// An elementary stream's PES is as vulnerable to a break as a section is. A
		// looping publisher wraps with its last PES still open and short of its declared
		// length, so the next loop's leading packets would otherwise complete it and hand
		// the codec one buffer straddling the cut.
		if let Ok(pid) = Pid::new(pid)
			&& self.streams.contains_key(&pid)
		{
			match continuation.expect("media PID continuity was classified") {
				Continuation::Duplicate => return Ok(()),
				// Flagged corrupt, so the packet joins the partial rather than opening a
				// new PES out of bytes the demodulator already disowned.
				Continuation::Corrupt => {
					self.damage(pid, &anyhow::anyhow!("transport error indicator"))?;
					return Ok(());
				}
				Continuation::Broken => {
					self.broken(pid)?;
					// This packet still routes normally: a PUSI opens a fresh PES, while a
					// continuation finds no pending entry and is dropped, so the stream
					// resumes at the next PES start rather than mid-frame.
				}
				Continuation::Contiguous => {}
			}
		}
		let Ok(pid) = Pid::new(pid) else {
			return Ok(());
		};
		if pid.as_u16() == Pid::PAT {
			if let Some(pat) = &packet.pat {
				self.handle_pat(pat)?;
			}
		} else if self.pmt.as_ref().is_some_and(|(pmt, _)| *pmt == pid) {
			self.pmt_packet(pid, packet)?;
		} else {
			// PIDs we don't carry (`Stream::Ignored`, counted when refused) are dropped, as is
			// every PID no table claims (counted by `claims` above) and every PID before the
			// PSI that declares it: a live capture joins mid-stream, so PES arrive before
			// their PMT.
			match self.streams.get(&pid) {
				None | Some(Stream::Ignored) => {}
				Some(_) => self.pes_packet(pid, pkt, offset)?,
			}
		}
		Ok(())
	}

	/// Feed one packet on the PMT PID to its reassembler, applying every good PMT section.
	fn pmt_packet(&mut self, pid: Pid, packet: &Packet) -> anyhow::Result<()> {
		let mut sections = Vec::new();
		if let Some((_, reassembler)) = &mut self.pmt {
			reassembler.push(packet.pkt, &mut sections);
		}
		for section in sections {
			// Other tables may share the PID; only a PMT is this path's to check.
			if section.first() != Some(&psi::PMT_TABLE_ID) {
				continue;
			}
			if !psi::crc_ok(&section) {
				self.crc_error(pid);
				continue;
			}
			if let Some(pmt) = Pmt::parse(&section) {
				self.handle_pmt(pmt)?;
				self.seed_clock(packet)?;
			}
		}
		Ok(())
	}

	/// Start a clock a PMT just named from its latest PCR, if one arrived since the program
	/// last had none, or note from when one may start it.
	fn seed_clock(&mut self, packet: &Packet) -> anyhow::Result<()> {
		match self.pcr_pid {
			Some(pid) => {
				if let Some(since) = self.unclocked_since.take()
					&& let Some(&sample) = packet.pcrs.get(&pid.as_u16())
					&& sample.at - PCR_OFFSET >= since
				{
					self.place_pcr(sample.ticks, sample.at - PCR_OFFSET)?;
				}
			}
			None => {
				self.unclocked_since
					.get_or_insert(packet.offset + TsPacket::SIZE as u64);
			}
		}
		Ok(())
	}

	/// A PMT section failed its CRC: it is dropped whole, and the table already in
	/// force stays.
	fn crc_error(&mut self, pid: Pid) {
		*self.crc_errors.entry(pid.as_u16()).or_default() += 1;
		tracing::warn!(pid = pid.as_u16(), "dropped a PSI section with a bad CRC");
	}

	fn handle_pmt(&mut self, pmt: Pmt) -> anyhow::Result<()> {
		// PMTs of several programs may share one PID; only the chosen one maps streams.
		if self.program.is_some_and(|program| program != pmt.program_number) {
			return Ok(());
		}
		self.claims_changed = true;
		// Which PID speaks for the program clock, so a `discontinuity_indicator` there
		// can be read as a timebase reset rather than a counter jump.
		if self.pcr_pid != pmt.pcr_pid {
			self.pcr_pid = pmt.pcr_pid;
			self.health.pcr_pid(pmt.pcr_pid.map(|pid| pid.as_u16()));
			// A new clock: the intervals straddling the switch measure nothing, and the whole PES
			// the old one began are placed on its timebase, never between its PCRs and the new.
			self.liveness.discontinuity();
			self.si.discontinuity();
			self.reset_mux_rate()?;
			self.settle_placement()?;
		}

		// SCTE-35 is announced by a program-level registration descriptor with
		// format_identifier 'CUEI' (ITU-T J.181). The stream itself uses
		// stream_type 0x86, which is also a DTS audio variant, so detection keys
		// off the CUEI descriptor, not the stream type alone.
		let cuei = pmt
			.program_info
			.iter()
			.any(|d| d.tag == 0x05 && d.data.len() >= 4 && &d.data[0..4] == b"CUEI");

		// Record the program-level descriptors as the PMT revises them; export re-emits them
		// verbatim, including the original CUEI.
		if self.supports_mpegts && self.program_info != pmt.program_info {
			if let Some(mpegts) = self.catalog.modify()?.ext.mpegts_mut() {
				mpegts.program_descriptors = pmt.program_info.clone();
			}
			self.program_info = pmt.program_info.clone();
		}

		// A stream the PMT no longer lists, or lists as something else, ends here, so every
		// track name keeps meaning one elementary stream; a revised one opens under a new name.
		let listed: HashMap<Pid, Listing> = pmt
			.streams
			.iter()
			.map(|es| {
				let listing = Listing {
					stream_type: es.stream_type,
					descriptors: es.descriptors.clone(),
					sections: section_framed(es.stream_type, cuei),
				};
				(es.pid, listing)
			})
			.collect();
		let mut ended: Vec<Pid> = self
			.listed
			.iter()
			.filter(|(pid, listing)| listed.get(pid) != Some(listing))
			.map(|(&pid, _)| pid)
			.collect();
		ended.sort_by_key(Pid::as_u16);
		for pid in ended {
			self.end_route(pid)?;
			if !listed.contains_key(&pid) {
				tracing::info!(pid = pid.as_u16(), "TS elementary stream left the PMT");
			}
		}
		self.listed = listed;

		for es in &pmt.streams {
			let before = self.route(es.pid);
			// Stash ES descriptors so a decoded media track can record them once its
			// (lazily created) track exists; verbatim streams record their own.
			if self.supports_mpegts {
				self.es_descriptors.insert(es.pid.as_u16(), es.descriptors.clone());
			}
			// Section-framed data is reassembled as sections, never PES-parsed. Everything else
			// routes through ensure_stream (a decoded codec, whole PES, or refused).
			if section_framed(es.stream_type, cuei) {
				self.ensure_section(es.pid, es.stream_type, &es.descriptors)?;
			} else {
				self.ensure_stream(es.pid, es.stream_type, &es.descriptors)?;
			}
			// Report a new PID or a changed route once; a repeated PMT stays quiet.
			if let Some((outcome, carriage)) = self.route(es.pid)
				&& before != Some((outcome, carriage))
			{
				tracing::info!(
					pid = es.pid.as_u16(),
					stream_type = es.stream_type,
					outcome,
					carriage,
					"TS elementary stream"
				);
			}
		}
		self.media_clock = pmt.streams.iter().any(|es| {
			self.streams.get(&es.pid).is_some_and(|stream| {
				stream.video() || matches!(stream, Stream::Aac(_) | Stream::Opus(_) | Stream::Legacy(_))
			})
		});
		let pids: Vec<u16> = pmt.streams.iter().map(|es| es.pid.as_u16()).collect();
		self.health.pmt_streams(&pids);
		self.described = true;
		Ok(())
	}

	/// How a PID's elementary stream is carried, as `(outcome, carriage)`, or `None` if unrouted.
	fn route(&self, pid: Pid) -> Option<(&'static str, &'static str)> {
		Some(match self.streams.get(&pid) {
			Some(Stream::H264 { .. } | Stream::H265 { .. } | Stream::Aac(_) | Stream::Opus(_) | Stream::Legacy(_)) => {
				("track", "decoded")
			}
			Some(Stream::PesPacket(_)) => ("verbatim", "pesPacket"),
			Some(Stream::Clock) => ("dropped", "clock only"),
			Some(Stream::Ignored) => ("dropped", "none"),
			None if self.sections.contains_key(&pid.as_u16()) => ("verbatim", "section"),
			None => return None,
		})
	}

	/// Route one packet on an elementary stream PID, at input offset `offset`, into its PES
	/// reassembly.
	fn pes_packet(&mut self, pid: Pid, pkt: &[u8; TsPacket::SIZE], offset: u64) -> anyhow::Result<()> {
		let payload = match payload(pkt) {
			Payload::None => return Ok(()),
			Payload::Bytes(payload) => payload,
			// No payload can be found in it, so like a corrupt packet it breaks the PES.
			Payload::Malformed => {
				return self.damage(pid, &anyhow::anyhow!("malformed TS adaptation field"));
			}
		};
		if pkt[1] & 0x40 != 0 {
			// The new start ends the preceding PES even while its own header is incomplete.
			self.flush(pid)?;
			if self.headers.remove(&pid).is_some() {
				self.damage(pid, &anyhow::anyhow!("PES header truncated at next start"))?;
			}
			// The payload ends the packet, so this is where its start code sits in the input.
			let at = offset + (TsPacket::SIZE - payload.len()) as u64;
			// Taken here, not once the header is whole, so a PCR landing before the header's
			// continuation is the one after it.
			let span = self.placement.span(at);
			return match PesStart::header_len(payload) {
				Ok(len) if payload.len() >= len => match PesStart::parse(payload) {
					Ok(pes) => self.handle_pes_start(pid, pes, span),
					Err(err) => self.damage(pid, &err),
				},
				Ok(_) => self.pes_header(pid, span, payload.to_vec()),
				Err(err) => self.damage(pid, &err),
			};
		}
		if let Some((span, mut header)) = self.headers.remove(&pid) {
			header.extend_from_slice(payload);
			self.pes_header(pid, span, header)
		} else {
			self.handle_pes_continuation(pid, payload)
		}
	}

	/// Continue a PES header split across packets, placed by `span` from its start code.
	fn pes_header(&mut self, pid: Pid, span: Option<Span>, header: Vec<u8>) -> anyhow::Result<()> {
		let len = match PesStart::header_len(&header) {
			Ok(len) => len,
			Err(err) => return self.damage(pid, &err),
		};
		if header.len() < len {
			self.headers.insert(pid, (span, header));
			return Ok(());
		}
		match PesStart::parse(&header) {
			Ok(pes) => self.handle_pes_start(pid, pes, span),
			Err(err) => self.damage(pid, &err),
		}
	}

	fn ensure_stream(&mut self, pid: Pid, stream_type: u8, descriptors: &[catalog::Descriptor]) -> anyhow::Result<()> {
		let temporal_poc = if stream_type == StreamType::H264 as u8 {
			video::temporal_poc(descriptors)?
		} else {
			false
		};
		if let Some(Stream::H264 { assembly, .. }) = self.streams.get_mut(&pid) {
			assembly.clock.temporal_poc = temporal_poc;
		}

		// A later PMT can remap a PID that was section-framed (intercepted in
		// `decode`) to a PES codec/verbatim stream. Drop the stale section route first,
		// or it would keep intercepting the PID and the new stream would never get data.
		// This only fires on a genuine remap: section PIDs otherwise route to
		// `ensure_section`, never here.
		if let Some(mut section) = self.retire_section(pid) {
			section.finish()?;
			self.pending.remove(&pid);
			self.headers.remove(&pid);
			let name = section.entry.name.clone();
			drop(section);
			self.retire_track(Some(name))?;
		}
		// A private PES stream follows the PMT's PCR_PID: refused while there is none to place
		// its PES by, and routed again once there is, or once the PID carries another type.
		match self.streams.get(&pid) {
			Some(Stream::PesPacket(stream)) if self.pcr_pid.is_none() => {
				let carried = stream.stream_type;
				self.refuse(pid, "the program lost the PCR that placed it")?;
				self.unclocked.insert(pid, carried);
				return Ok(());
			}
			Some(Stream::Ignored | Stream::Clock)
				if self
					.unclocked
					.get(&pid)
					.is_some_and(|&refused| self.pcr_pid.is_some() || refused != stream_type) =>
			{
				self.unclocked.remove(&pid);
				self.streams.remove(&pid);
			}
			Some(_) => return Ok(()),
			None => {}
		}

		let stream = match StreamType::from_u8(stream_type).ok() {
			Some(StreamType::H264) => {
				let track = self
					.broadcast
					.unique_track(".avc3", self.catalog.track_info(hang::catalog::PRIORITY.video))?;
				Stream::H264 {
					split: h264::Split::new(),
					assembly: {
						let mut assembly = video::Assembly::new(video::Codec::Avc);
						assembly.clock.temporal_poc = temporal_poc;
						assembly
					},
					import: Box::new(h264::Import::new(track, self.reserve(), self.video_hint())?),
				}
			}
			Some(StreamType::H265) => {
				let track = self
					.broadcast
					.unique_track(".hev1", self.catalog.track_info(hang::catalog::PRIORITY.video))?;
				Stream::H265 {
					split: h265::Split::new(),
					assembly: video::Assembly::new(video::Codec::Hevc),
					import: Box::new(h265::Import::new(track, self.reserve(), self.video_hint())?),
				}
			}
			// Only ADTS-framed AAC (0x0F). 0x11 is LATM/LOAS, which uses a different
			// framing and syncword, so it rides whole like any other undecoded stream.
			Some(StreamType::AdtsAac) => Stream::Aac(Box::new(AacStream {
				import: None,
				asc: bytes::Bytes::new(),
				format: (0, 0, 0),
				broadcast: self.broadcast.clone(),
				reserved: Some(self.reserve()),
				timebase: self.timebase.clone(),
				container: self.container.clone(),
				tail: Vec::new(),
				tail_pts: None,
				resync: Resync::new(pid.as_u16(), ".aac"),
				burst: std::time::Duration::ZERO,
			})),
			// Legacy broadcast audio, carried verbatim. Both MP2 stream types
			// (0x03 MPEG-1, 0x04 MPEG-2 half rate) share one parser; sample rate and
			// channels always come from the frame header, not the PMT.
			Some(StreamType::Mpeg1Audio | StreamType::Mpeg2HalvedSampleRateAudio) => {
				self.legacy_stream(pid, &mp2::DESCRIPTOR)
			}
			Some(StreamType::DolbyDigitalUpToSixChannelAudio) => self.legacy_stream(pid, &ac3::DESCRIPTOR),
			Some(StreamType::DolbyDigitalPlusUpTo16ChannelAudioForAtsc) => self.legacy_stream(pid, &eac3::DESCRIPTOR),
			// Opus rides private-data PES (0x06), distinguished from other private streams
			// by an 'Opus' registration descriptor. Channels and the (always 48 kHz) rate
			// come from the descriptors, so the importer is built up front. A channel code
			// the descriptor cannot justify drops this PID; the rest of the program stays.
			Some(StreamType::Mpeg2PacketizedData) if registration_format(descriptors) == Some(*b"Opus") => {
				match opus_config(descriptors) {
					Ok(config) => {
						let track = self
							.broadcast
							.unique_track(".opus", self.catalog.track_info(hang::catalog::PRIORITY.audio))?;
						let mut config: hang::catalog::AudioConfig = config.into();
						config.container = self.container.clone();
						Stream::Opus(Box::new(OpusStream {
							import: opus::Import::new(track, self.reserve(), config)?,
						}))
					}
					Err(err) => self.uncarried(
						pid,
						stream_type,
						&format!("unsupported Opus channel configuration: {err:#}"),
					),
				}
			}
			// 0x86 is ambiguous: DTS audio, or a non-conformant SCTE-35 mux without CUEI, which is
			// sections a PES parse would abort on. Refuse it rather than guess.
			_ if stream_type == StreamType::Dts8ChannelLosslessAudio as u8 => self.uncarried(
				pid,
				stream_type,
				"0x86 without a CUEI registration may be DTS or SCTE-35",
			),
			// A codec we don't decode, named or not, MPEG-1/2 video included, rides as whole PES
			// packets when the catalog supports the `mpegts` section.
			_ if !self.supports_mpegts => self.uncarried(pid, stream_type, "the catalog has no mpegts section"),
			_ if self.pcr_pid.is_none() => {
				// Nothing could place its PES, so no track is advertised.
				self.unclocked.insert(pid, stream_type);
				self.uncarried(pid, stream_type, "the program has no PCR to place it by")
			}
			_ => Stream::PesPacket(Box::new(PesPacketStream::new(
				self.broadcast.clone(),
				self.catalog.clone(),
				pid.as_u16(),
				stream_type,
				descriptors.to_vec(),
			)?)),
		};

		// Clock is not a decodable track, so it doesn't initialize the importer.
		if !matches!(stream, Stream::Ignored | Stream::Clock) {
			self.initialized = true;
		}
		if !matches!(stream, Stream::Ignored) {
			self.liveness.register(pid.as_u16());
		}
		self.streams.insert(pid, stream);
		self.health.restart(pid.as_u16());
		self.restarts.push(pid.as_u16());
		Ok(())
	}

	/// The route of a listed stream this import does not carry, counted once as refused. MPEG-1/2
	/// video still paces the program by its PTS.
	fn uncarried(&mut self, pid: Pid, stream_type: u8, why: &str) -> Stream<E> {
		*self.refused.entry(pid.as_u16()).or_default() += 1;
		tracing::warn!(pid = pid.as_u16(), stream_type, why, "refused a listed stream");
		match mpeg_video(stream_type) {
			true => Stream::Clock,
			false => Stream::Ignored,
		}
	}

	fn legacy_stream(&self, pid: Pid, descriptor: &'static legacy::Descriptor) -> Stream<E> {
		Stream::Legacy(Box::new(LegacyStream {
			descriptor,
			import: None,
			broadcast: self.broadcast.clone(),
			reserved: Some(self.reserve()),
			timebase: self.timebase.clone(),
			format: (0, 0),
			container: self.container.clone(),
			tail: Vec::new(),
			tail_pts: None,
			resync: Resync::new(pid.as_u16(), descriptor.track_suffix),
		}))
	}

	/// Register a section-framed verbatim PID (SCTE-35 or other private sections):
	/// intercepted (see [`Self::decode`]) with a verbatim track when the catalog
	/// carries the `mpegts` section, dropped as `Ignored` when it can't.
	fn ensure_section(&mut self, pid: Pid, stream_type: u8, descriptors: &[catalog::Descriptor]) -> anyhow::Result<()> {
		if self.supports_mpegts && self.pcr_pid.is_none() {
			if self.unclocked.get(&pid) != Some(&stream_type) {
				self.refuse(pid, "the program has no PCR to place it by")?;
				self.unclocked.insert(pid, stream_type);
			}
			self.liveness.register(pid.as_u16());
			self.initialized = true;
			return Ok(());
		}
		if self.sections.contains_key(&pid.as_u16()) {
			return Ok(());
		}
		// This PID is becoming section-framed; drop any partial PES a prior codec left pending.
		self.pending.remove(&pid);
		self.headers.remove(&pid);
		self.health.restart(pid.as_u16());
		self.restarts.push(pid.as_u16());
		if !self.supports_mpegts {
			// Always route to Ignored, replacing any prior codec on this PID (a later PMT
			// can reassign it), so a private section is never PES-parsed. Counted once.
			let stream = match self.retire_stream(pid) {
				Some(Stream::Ignored) => Stream::Ignored,
				_ => self.uncarried(pid, stream_type, "the catalog has no mpegts section"),
			};
			self.streams.insert(pid, stream);
			return Ok(());
		}
		// A prior PMT may have routed this PID to Ignored; drop it so the PID has one route.
		self.retire_stream(pid);
		let descriptors = descriptors.to_vec();
		let mut stream = SectionStream::new(
			self.broadcast.clone(),
			self.catalog.clone(),
			pid.as_u16(),
			stream_type,
			descriptors,
		)?;
		stream.staging = self.staging.clone();
		self.sections.insert(pid.as_u16(), stream);
		self.liveness.register(pid.as_u16());
		self.initialized = true;
		tracing::debug!(
			pid = pid.as_u16(),
			stream_type,
			"private section stream detected; reassembling its sections"
		);
		Ok(())
	}

	/// Remove a PES route while keeping the counters it accumulated for this importer.
	fn retire_stream(&mut self, pid: Pid) -> Option<Stream<E>> {
		self.unclocked.remove(&pid);
		let stream = self.streams.remove(&pid)?;
		self.keep_stats(pid, stream.stats());
		Some(stream)
	}

	/// Keep a retired route's counters for this importer, naming it the PID's latest route.
	fn keep_stats(&mut self, pid: Pid, current: Option<stats::Stream>) {
		if let Some(current) = current {
			self.retired_stats
				.entry(pid.as_u16())
				.and_modify(|retired| retired.merge(&current))
				.or_insert(current);
		}
	}

	/// Retire the timeline of a track whose route has ended. Every route mints its own name, so
	/// nothing enrolls it again, and a source revising its PMT holds only its live routes.
	fn retire_track(&mut self, name: Option<String>) -> anyhow::Result<()> {
		if let Some(name) = name {
			self.catalog.retire(&name)?;
		}
		Ok(())
	}

	/// End the route on `pid` for a PMT that dropped or revised its stream, as the end of input
	/// ends every route: the PES it began goes out, what waits on the clock is placed from the
	/// PCRs so far or counted unplaced (refusing the import, as end of input does, if no media
	/// timestamp anchored it), and its track finishes and leaves the catalog.
	fn end_route(&mut self, pid: Pid) -> anyhow::Result<()> {
		if self.headers.remove(&pid).is_some() {
			self.damage(pid, &anyhow::anyhow!("PES header truncated by a PMT revision"))?;
		}
		self.flush(pid)?;
		// Released as if its timebase ended here, so a unit no later PCR will bracket for it is
		// unplaced rather than held.
		let generation = self.placement.generation + 1;
		let (mut units, mut unplaced) = (0, 0);
		// Each route stays in its map until it has finished, so a failure on the way leaves it
		// for `abort` to end with the cause.
		if let Some(section) = self.sections.get_mut(&pid.as_u16()) {
			section.settle(&self.placement);
			(units, unplaced) = section.release(&self.timebase, &self.pts, generation)?;
			anyhow::ensure!(
				section.held.is_empty(),
				"TS sections reached a PMT revision without a media timestamp"
			);
			section.finish()?;
		}
		if let Some(section) = self.sections.remove(&pid.as_u16()) {
			self.keep_stats(pid, Some(stats::Stream::new(".ts", stats::Class::Data)));
			let name = section.entry.name.clone();
			drop(section);
			self.retire_track(Some(name))?;
		}
		self.unclocked.remove(&pid);
		if let Some(stream) = self.streams.get_mut(&pid)
			&& let Stream::PesPacket(stream) = &mut *stream
		{
			stream.end();
			stream.settle(&self.placement);
			let (published, dropped) = stream.release(&self.timebase, &self.pts, generation)?;
			units += published;
			unplaced += dropped;
			anyhow::ensure!(
				stream.held.is_empty(),
				"TS private PES reached a PMT revision without a media timestamp"
			);
		}
		units += self.finish_stream(pid)?;
		if let Some(stream) = self.streams.get(&pid) {
			// A verbatim entry leaves with its stream; a decoded track's is recorded here.
			if let Some(name) = self.recorded_media.remove(&pid)
				&& stream.media_track_name().is_some()
				&& let Some(mpegts) = self.catalog.modify()?.ext.mpegts_mut()
			{
				mpegts.tracks.remove(&name);
			}
		}
		if let Some(stream) = self.streams.remove(&pid) {
			// Finishing can publish a held frame, which its counters then include.
			self.keep_stats(pid, stream.stats());
			let name = stream.track_name();
			drop(stream);
			self.retire_track(name)?;
		}
		if unplaced > 0 {
			*self.unplaced.entry(pid.as_u16()).or_default() += unplaced;
		}
		self.published |= units > 0;
		self.liveness.delivered(pid.as_u16(), units);
		self.liveness.retire(pid.as_u16());
		self.es_descriptors.remove(&pid.as_u16());
		Ok(())
	}

	/// Remove the section route on `pid`, keeping the row of the track it published.
	fn retire_section(&mut self, pid: Pid) -> Option<SectionStream<E>> {
		let section = self.sections.remove(&pid.as_u16())?;
		self.retired_stats
			.entry(pid.as_u16())
			.or_insert_with(|| stats::Stream::new(".ts", stats::Class::Data));
		Some(section)
	}

	/// Open the PES whose header `pes` begins where `span` says, if any PCR came before it.
	fn handle_pes_start(&mut self, pid: Pid, pes: PesStart, span: Option<Span>) -> anyhow::Result<()> {
		// Finish the preceding PES before accepting another header.
		if self.pending.contains_key(&pid) {
			self.flush(pid)?;
		}

		let Some(stream) = self.streams.get(&pid) else {
			// PES before its PMT entry; ignore until the layout is known.
			return Ok(());
		};

		let whole = match stream {
			Stream::PesPacket(stream) => Some(stream.stream_type),
			_ => None,
		};
		if let Some(stream_type) = whole
			&& let Some(why) = refusal(&pes, stream_type)
		{
			return self.refuse(pid, &why);
		}
		if pes.scrambled() {
			return self.damage(pid, &anyhow::anyhow!("scrambled PES is not supported"));
		}
		let stream = &self.streams[&pid];

		let is_video = stream.video();
		let is_clock = matches!(stream, Stream::Clock);
		// Verbatim streams with no cadence (SCTE-35, DVB subtitles) are left ungraded
		// rather than watched against an invented one.
		if pes.pts.is_some() && (is_video || !matches!(stream, Stream::PesPacket(_) | Stream::Ignored)) {
			self.health.pts(pid.as_u16(), self.liveness.now());
		}
		// Resolve the wrap at PES start so every elementary stream and private section shares
		// the program timeline, including streams discovered after a wrap.
		let (pts, offset) = match pes.pts {
			Some(raw) => {
				if self.mux_rate_reservation.is_some() {
					self.hold_media(raw);
				}
				// Only a stream still behind a wrap the first PTS crossed reads as negative.
				let Ok(ticks) = u64::try_from(self.pts.unwrap(raw)) else {
					*self.unplaced.entry(pid.as_u16()).or_default() += 1;
					return Ok(());
				};
				let offset = self.timebase.anchor(Timestamp::from_scale(ticks, 90_000)?)?;
				(Some(ticks), offset)
			}
			None => (None, Offset::default()),
		};
		if is_video {
			for stream in self.streams.values_mut() {
				if let Stream::Aac(audio) = stream {
					audio.burst = std::time::Duration::ZERO;
				}
			}
			// Service-information snapshots follow the current video presentation time.
			if pts.is_some() {
				self.last_pts = on_clock(pts, offset)?;
			}
		}
		if pts.is_some() {
			// A first PTS may have anchored the timebase whole PES were waiting on.
			self.release_placed()?;
		}

		if is_clock {
			// A clock-only stream never flushes, so release its reservation here.
			if pts.is_some() {
				self.initial_reservation = None;
			}
			// Nothing is published, but each PES is a picture the source delivered.
			self.liveness.delivered(pid.as_u16(), 1);
			return Ok(());
		}

		// A whole PES keeps its header, and is placed by the PCR before it and the one after.
		let (header, data_len, span) = match whole {
			Some(_) => {
				// A span whose timebase ended while its header assembled was settled with it, or
				// is dropped as unplaced at release.
				let Some(span) = span else {
					// Before the first PCR on this timebase nothing places it. A PTS it carried still
					// anchored the timebase, so it releases the reservation like a flushed PES would.
					if pts.is_some() {
						self.initial_reservation = None;
					}
					*self.unplaced.entry(pid.as_u16()).or_default() += 1;
					return Ok(());
				};
				let len = pes.data_len.map(|len| pes.header.len() + len);
				(pes.header, len, Some(span))
			}
			None => (&[][..], pes.data_len, None),
		};
		let data = &pes.data[..pes.data_len.unwrap_or(pes.data.len()).min(pes.data.len())];
		let mut retained = Retained::new(self.staging.clone());
		// A whole PES is held as a unit until placed, so tiny ones count against the unit bound
		// as sections do, not the byte bound alone.
		retained.grow(header.len() + data.len(), usize::from(span.is_some()))?;
		let mut pending = Pending {
			retained,
			pts,
			offset,
			dts: pes.dts,
			data: Vec::with_capacity(header.len() + data.len()),
			data_len,
			span,
		};
		pending.data.extend_from_slice(header);
		pending.data.extend_from_slice(data);
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
		let len = pending
			.data_len
			.map_or(data.len(), |len| len - pending.data.len())
			.min(data.len());
		pending.retained.grow(len, 0)?;
		pending.data.extend_from_slice(&data[..len]);
		if matches!(pending.data_len, Some(len) if pending.data.len() >= len) {
			self.flush(pid)?;
		}
		Ok(())
	}

	fn video_damage(&mut self, pid: Pid) {
		let damage = match self.streams.get_mut(&pid) {
			Some(Stream::H264 { assembly, .. } | Stream::H265 { assembly, .. }) => assembly.take_damage(),
			_ => None,
		};
		if let Some((count, err)) = damage {
			*self.damaged.entry(pid.as_u16()).or_default() += count;
			self.damage_log.record(pid.as_u16(), &err);
		}
	}

	/// Refuse one damaged unit without changing any other PID or the program clock.
	fn damage(&mut self, pid: Pid, err: &anyhow::Error) -> anyhow::Result<()> {
		// Complete AUs ahead of the damaged packet remain valid, including those
		// in an unbounded PES. Its final NAL/AU is never flushed at a loss boundary.
		if let Some(pending) = self.pending.remove(&pid)
			&& let Some(stream @ (Stream::H264 { .. } | Stream::H265 { .. })) = self.streams.get_mut(&pid)
		{
			let result = stream.write(pending, false);
			self.video_damage(pid);
			let units = match result {
				Ok(units) => units,
				Err(err) if err.is::<Damaged>() => video::published(&err),
				Err(err) => return Err(err),
			};
			self.published |= units > 0;
			self.liveness.delivered(pid.as_u16(), units);
			self.record_media_track(pid)?;
		}
		self.headers.remove(&pid);
		match self.streams.get_mut(&pid) {
			// Nothing is imported from it, so there is nothing to refuse or report.
			Some(Stream::Ignored) => return Ok(()),
			Some(stream) => stream.lost()?,
			None => {}
		}
		*self.damaged.entry(pid.as_u16()).or_default() += 1;
		self.damage_log.record(pid.as_u16(), err);
		Ok(())
	}

	fn flush(&mut self, pid: Pid) -> anyhow::Result<()> {
		let Some(pending) = self.pending.remove(&pid) else {
			return Ok(());
		};

		let batched = self.streams.values().any(Stream::video);
		let Some(stream) = self.streams.get_mut(&pid) else {
			return Ok(());
		};
		let truncated = pending.data_len.is_some_and(|len| pending.data.len() < len);
		if truncated && !stream.salvages_partial_pes() {
			return self.damage(
				pid,
				&anyhow::anyhow!("PES payload truncated before its declared length"),
			);
		}

		// Its PES start anchored the timebase. Release the reservation only now, so the first
		// snapshot carries the final clock: an Opus config comes from the PMT and would otherwise
		// publish first. Every stream in the initial program reserved at its PMT, so any stream's
		// PES releases it.
		if pending.pts.is_some() {
			self.initial_reservation = None;
		}
		let result = stream.write(pending, batched);
		self.video_damage(pid);
		let (units, damage) = match result {
			Ok(units) => (units, None),
			Err(err) if err.is::<Damaged>() => (video::published(&err), Some(err)),
			Err(err) => return Err(err),
		};
		if let Some(err) = damage {
			self.damage(pid, &err)?;
		} else if truncated {
			if matches!(self.streams.get(&pid), Some(Stream::H264 { .. } | Stream::H265 { .. })) {
				self.damage(
					pid,
					&anyhow::anyhow!("PES payload truncated before its declared length"),
				)?;
			} else {
				self.streams.get_mut(&pid).expect("existing route").lost()?;
			}
		}
		self.published |= units > 0;
		self.liveness.delivered(pid.as_u16(), units);
		if matches!(self.streams.get(&pid), Some(Stream::PesPacket(_))) {
			self.release_placed()?;
		}

		// Record the decoded media track's PID + PMT descriptors (language, ...) once
		// its lazily created track exists, so export can preserve them.
		self.record_media_track(pid)
	}

	/// A PCR at input offset `offset`, its packet's, on the program's clock PID: it places every
	/// whole PES and section begun since the last, and releases those complete.
	fn place_pcr(&mut self, raw: u64, offset: u64) -> anyhow::Result<()> {
		if self.placement.rewinds(raw) {
			// The clock stepped back without saying so: what it placed so far is placed by the
			// old timebase, and the new one starts here.
			self.settle_placement()?;
		}
		if !self.media_clock {
			// PCR and PTS share one era. Advance it from PCR while no media can do so,
			// including across a wrap before a later PMT adds a media stream.
			self.pts.unwrap(raw / 300);
			let ticks = u64::try_from(self.pts.pcr(raw))?;
			self.timebase.anchor(Timestamp::from_scale(ticks, PCR_HZ)?)?;
			self.initial_reservation = None;
			if self.mux_rate_reservation.is_some() {
				self.hold_media(raw / 300);
			}
		}
		let sample = self.placement.pcr(raw, offset + PCR_OFFSET);
		let generation = self.placement.generation;
		// A PES still assembling its header is bracketed too, so the first PCR past its start
		// code places it however many arrive before the header is whole.
		let spans = self.pending.values_mut().filter_map(|pending| pending.span.as_mut());
		for span in spans.chain(self.headers.values_mut().filter_map(|(span, _)| span.as_mut())) {
			span.bracket(generation, sample);
		}
		for stream in self.streams.values_mut() {
			if let Stream::PesPacket(stream) = stream {
				stream.bracket(generation, sample);
			}
		}
		for section in self.sections.values_mut() {
			section.bracket(generation, sample);
		}
		self.release_placed()
	}

	/// End the placement timebase: extrapolate every PES or section begun past its last PCR from its
	/// last two, and publish what that places.
	fn settle_placement(&mut self) -> anyhow::Result<()> {
		let spans = self.pending.values_mut().filter_map(|pending| pending.span.as_mut());
		for span in spans.chain(self.headers.values_mut().filter_map(|(span, _)| span.as_mut())) {
			span.settle(&self.placement);
		}
		for stream in self.streams.values_mut() {
			if let Stream::PesPacket(stream) = stream {
				stream.settle(&self.placement);
			}
		}
		for section in self.sections.values_mut() {
			section.settle(&self.placement);
		}
		self.placement.reset();
		self.release_placed()
	}

	/// Publish every held PES or section that is placed, once the timebase has its offset.
	fn release_placed(&mut self) -> anyhow::Result<()> {
		for (pid, stream) in &mut self.streams {
			let Stream::PesPacket(stream) = stream else {
				continue;
			};
			let (units, unplaced) = stream.release(&self.timebase, &self.pts, self.placement.generation)?;
			if unplaced > 0 {
				*self.unplaced.entry(pid.as_u16()).or_default() += unplaced;
			}
			self.published |= units > 0;
			self.liveness.delivered(pid.as_u16(), units);
		}
		for (pid, section) in &mut self.sections {
			let (units, unplaced) = section.release(&self.timebase, &self.pts, self.placement.generation)?;
			if unplaced > 0 {
				*self.unplaced.entry(*pid).or_default() += unplaced;
			}
			self.published |= units > 0;
			self.liveness.delivered(*pid, units);
		}
		Ok(())
	}

	/// Refuse the undecoded stream on `pid` rather than carry it altered: its track ends with an
	/// error and leaves the catalog, and the PID is dropped from here on.
	fn refuse(&mut self, pid: Pid, why: &str) -> anyhow::Result<()> {
		self.pending.remove(&pid);
		self.headers.remove(&pid);
		if let Some(section) = self.retire_section(pid) {
			let name = section.entry.name.clone();
			section.abort(moq_net::Error::Unsupported);
			self.retire_track(Some(name))?;
		}
		// MPEG-1/2 video still paces the program once its PES are refused.
		let clock = match self.retire_stream(pid) {
			Some(stream) => {
				let video = stream.video();
				let name = stream.track_name();
				stream.abort(moq_net::Error::Unsupported);
				self.retire_track(name)?;
				video
			}
			None => false,
		};
		self.streams
			.insert(pid, if clock { Stream::Clock } else { Stream::Ignored });
		*self.refused.entry(pid.as_u16()).or_default() += 1;
		tracing::warn!(pid = pid.as_u16(), why, "refused an undecoded stream");
		Ok(())
	}

	/// A TS-scrambled packet on the carried `pid`: refused, and the unit it interrupts lost with
	/// it. The route stays, so the stream resumes once its packets are clear again.
	fn scrambled(&mut self, pid: Pid) -> anyhow::Result<()> {
		self.pending.remove(&pid);
		self.headers.remove(&pid);
		if let Some(stream) = self.streams.get_mut(&pid) {
			stream.lost()?;
		}
		// A section's continuity counter never sees the packet, so after 16 the next clear one
		// would look contiguous and complete the partial with bytes from another section.
		if let Some(section) = self.sections.get_mut(&pid.as_u16()) {
			section.reassembler = SectionReassembler::default();
		}
		*self.refused.entry(pid.as_u16()).or_default() += 1;
		if self.scrambled.insert(pid.as_u16()) {
			tracing::warn!(
				pid = pid.as_u16(),
				"refusing TS-scrambled packets; carry the multiplex whole to keep them"
			);
		}
		Ok(())
	}

	/// The packet chain on `pid` was cut: salvage the truncated PES, and treat the bytes
	/// after it as [lost](Stream::lost).
	fn broken(&mut self, pid: Pid) -> anyhow::Result<()> {
		self.salvage(pid)?;
		if let Some(stream) = self.streams.get_mut(&pid) {
			stream.lost()?;
		}
		Ok(())
	}

	/// Publish the truncated PES on `pid` only where its bytes stand on their own, and drop
	/// whatever is left mid-unit.
	fn salvage(&mut self, pid: Pid) -> anyhow::Result<()> {
		if self.headers.remove(&pid).is_some() {
			self.damage(pid, &anyhow::anyhow!("PES header truncated by packet loss"))?;
		}
		if self.streams.get(&pid).is_some_and(Stream::salvages_partial_pes) {
			self.flush(pid)
		} else {
			self.pending.remove(&pid);
			self.headers.remove(&pid);
			Ok(())
		}
	}

	/// A system time-base discontinuity: the PCR PID declared that the clock every
	/// timestamp downstream is measured against restarts here.
	///
	/// Publish it as a break on every track, which is what carries it across MoQ (an empty
	/// group per track) and back out of the exporter as a set `discontinuity_indicator`.
	/// Whatever the source does next -- restart at zero, leap half a minute ahead -- is a new
	/// timeline rather than the old one continuing, and a consumer that splices the two gets
	/// the whole jump as a frame duration.
	///
	/// Scoped to the PCR PID on purpose. A counter jump (the same flag on an elementary PID,
	/// or a gap with no flag at all) loses bytes without moving the clock, and the 33-bit
	/// PCR/PTS rollover sets nothing and only moves the next PTS into the next era; neither is
	/// a program break and neither reaches here.
	fn timebase_break(&mut self) -> anyhow::Result<()> {
		// First, so the salvage below cannot publish the first catalog with the old clock's rate.
		self.rearm_mux_rate();
		// The bytes still accumulating belong to the old clock, and their continuation is
		// stamped on the new one, so cut every PES here rather than splicing across. This
		// runs before the marker so a salvaged tail lands in the closing group, which the
		// marker closes without guessing an end.
		for pid in self.streams.keys().copied().collect::<Vec<_>>() {
			self.salvage(pid)?;
		}
		// Whole PES the old clock began go out on its timeline, ahead of the marker, so none waits
		// for a PES on the new one to complete a start code.
		for stream in self.streams.values_mut() {
			stream.end();
		}
		self.settle_placement()?;
		anyhow::ensure!(
			self.sections.values().all(|section| section.held.is_empty()),
			"TS sections reached a clock break without a media timestamp"
		);
		anyhow::ensure!(
			self.streams.values().all(Stream::placed),
			"TS private PES reached a clock break without a media timestamp"
		);
		for stream in self.streams.values_mut() {
			stream.desync();
			stream.discontinuity(self.published)?;
		}
		for section in self.sections.values_mut() {
			section.discontinuity(self.published)?;
		}
		self.last_pts = None;
		self.held_edge = None;
		self.published = false;
		self.liveness.discontinuity();
		self.si.discontinuity();
		if self.mux_rate.discontinuity() {
			self.record_mux_rate()?;
		}
		tracing::debug!("MPEG-TS system time-base discontinuity");
		Ok(())
	}

	/// The program clock changed, so the measured rate no longer holds.
	fn reset_mux_rate(&mut self) -> anyhow::Result<()> {
		self.rearm_mux_rate();
		if self.mux_rate.discontinuity() {
			self.record_mux_rate()?;
		}
		Ok(())
	}

	/// A clock change voids the startup measurement, so until the first catalog publishes
	/// (while the initial reservation still holds it), hold it again for a window on the new
	/// clock.
	fn rearm_mux_rate(&mut self) {
		if self.supports_mpegts && self.mux_rate_reservation.is_none() {
			self.mux_rate_reservation = self.initial_reservation.clone();
		}
	}

	/// Count `pts` toward the startup hold. A source without a usable PCR never completes a
	/// measurement window, so enough media time releases its catalog without a rate.
	fn hold_media(&mut self, pts: u64) {
		const WRAP: u64 = 1 << 33;
		if let Some(edge) = self.held_edge {
			// Modulo the 33-bit wrap, so the hold counts across a rollover. A step past half the
			// range is a frame behind the edge (reordered, or another stream), not a jump forward.
			let step = pts.wrapping_sub(edge) % WRAP;
			if step >= WRAP / 2 {
				return;
			}
			self.held += std::time::Duration::from_nanos(step * 1_000_000_000 / 90_000);
		}
		self.held_edge = Some(pts);
		if self.held >= super::mux_rate::UNMEASURED {
			self.mux_rate_reservation = None;
		}
	}

	/// Copy the measured multiplex rate into the `mpegts` section, publishing the
	/// catalog. Called only when the measurement changed, so a steady source never
	/// republishes.
	fn record_mux_rate(&mut self) -> anyhow::Result<()> {
		let rate = self.mux_rate.published();
		if let Some(mpegts) = self.catalog.modify()?.ext.mpegts_mut() {
			mpegts.mux_rate = rate;
		}
		Ok(())
	}

	/// Record a decoded media stream's PID and ES descriptors into `mpegts.tracks`,
	/// once per track. No-op without the `mpegts` section, before the track exists,
	/// or for verbatim streams (which self-register).
	fn record_media_track(&mut self, pid: Pid) -> anyhow::Result<()> {
		if !self.supports_mpegts {
			return Ok(());
		}
		let Some(name) = self.streams.get(&pid).and_then(|s| s.media_track_name()) else {
			return Ok(());
		};
		if self.recorded_media.get(&pid) == Some(&name) {
			return Ok(());
		}
		let descriptors = self.es_descriptors.get(&pid.as_u16()).cloned().unwrap_or_default();
		if let Some(mpegts) = self.catalog.modify()?.ext.mpegts_mut() {
			// A format change moved the PID to a new track, whose entry replaces the ended one's.
			if let Some(previous) = self.recorded_media.get(&pid) {
				mpegts.tracks.remove(previous);
			}
			let entry = mpegts
				.tracks
				.entry(name.clone())
				.or_insert_with(|| catalog::Track::new(pid.as_u16()));
			entry.pid = pid.as_u16();
			entry.descriptors = descriptors;
		}
		self.recorded_media.insert(pid, name);
		Ok(())
	}

	/// Pick the imported program from a whole PAT and learn its PMT PID, so `decode`
	/// reassembles that PMT and no other. A PAT naming another PID for the program moves the
	/// reassembler there, and the catalog's `program.pmtPid` with it.
	///
	/// Every PAT is checked, so a program added mid-stream ends an unselected import, a
	/// selected program that disappears ends a selected one, and a PAT naming the program under
	/// another identity ends either; media already published stays.
	pub(super) fn handle_pat(&mut self, pat: &psi::Pat) -> anyhow::Result<()> {
		// program_number 0 is the network PID association, not a program.
		let programs: Vec<_> = pat.programs.iter().filter(|entry| entry.program_number != 0).collect();
		let numbers = || programs.iter().map(|entry| entry.program_number).collect::<Vec<_>>();
		let entry = match self.program {
			Some(program) => match programs.iter().find(|entry| entry.program_number == program) {
				Some(entry) => entry,
				None => anyhow::bail!(
					"transport stream has no program {program}; its PAT lists {}",
					list_programs(&numbers())
				),
			},
			None => match programs.as_slice() {
				[] => return Ok(()),
				[entry] => entry,
				_ => return Err(MultipleProgramsError { programs: numbers() }.into()),
			},
		};
		let identity = (pat.transport_stream_id, entry.program_number);
		if let Some((transport_stream_id, program_number)) = self.identity
			&& (transport_stream_id, program_number) != identity
		{
			anyhow::bail!(
				"the PAT changed the program's identity from transport_stream_id {transport_stream_id} program {program_number} to transport_stream_id {} program {}, which the carried service information does not describe",
				identity.0,
				identity.1
			);
		}
		let first = self.identity.replace(identity).is_none();
		self.health.pmt_pids(&[entry.pmt_pid.as_u16()], self.liveness.now());
		let moved = self.pmt.as_ref().is_none_or(|(pid, _)| *pid != entry.pmt_pid);
		if moved {
			if let Some((from, _)) = &self.pmt {
				tracing::info!(
					program = entry.program_number,
					from = from.as_u16(),
					to = entry.pmt_pid.as_u16(),
					"TS PMT moved to another PID"
				);
			}
			// A section begun on the old PID no longer speaks for the program.
			self.pmt = Some((entry.pmt_pid, SectionReassembler::default()));
			self.claims_changed = true;
		}
		// Alongside another program, a PID could be that program's.
		self.alone = programs.len() == 1;
		if first || moved {
			self.record_program_identity(pat.transport_stream_id, entry)?;
		}
		Ok(())
	}

	/// Capture the transport/service identity (TSID, service number, PMT PID) from the
	/// PAT into the catalog service record. No-op without `mpegts` support.
	fn record_program_identity(&mut self, transport_stream_id: u16, entry: &psi::Association) -> anyhow::Result<()> {
		if !self.supports_mpegts {
			return Ok(());
		}
		if let Some(mpegts) = self.catalog.modify()?.ext.mpegts_mut() {
			let program = mpegts.program.get_or_insert_with(Default::default);
			program.transport_stream_id = transport_stream_id;
			program.program_number = entry.program_number;
			program.pmt_pid = entry.pmt_pid.as_u16();
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

	fn finish_stream(&mut self, pid: Pid) -> anyhow::Result<u64> {
		let Some(stream) = self.streams.get_mut(&pid) else {
			return Ok(0);
		};
		let result = stream.finish();
		self.video_damage(pid);
		match result {
			Ok(units) => Ok(units),
			Err(err) if err.is::<Damaged>() => {
				let units = video::published(&err);
				self.damage(pid, &err)?;
				self.streams
					.get_mut(&pid)
					.expect("damage preserves the route")
					.finish()?;
				Ok(units)
			}
			Err(err) => Err(err),
		}
	}

	/// Flush any buffered PES and finish every track.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		self.finished = true;
		self.demux.finish();
		for pid in self.headers.keys().copied().collect::<Vec<_>>() {
			self.damage(pid, &anyhow::anyhow!("PES header truncated at end of input"))?;
		}
		let pids: Vec<Pid> = self.pending.keys().copied().collect();
		for pid in pids {
			self.flush(pid)?;
		}
		for stream in self.streams.values_mut() {
			stream.end();
		}
		self.settle_placement()?;
		anyhow::ensure!(
			self.sections.values().all(|section| section.held.is_empty()),
			"TS sections reached end of input without a media timestamp"
		);
		anyhow::ensure!(
			self.streams.values().all(Stream::placed),
			"TS private PES reached end of input without a media timestamp"
		);
		// EOF releases an unmeasured short source as well as its decoder reservations.
		self.mux_rate_reservation = None;
		self.initial_reservation = None;
		for pid in self.streams.keys().copied().collect::<Vec<_>>() {
			let units = self.finish_stream(pid)?;
			self.published |= units > 0;
			self.liveness.delivered(pid.as_u16(), units);
			self.record_media_track(pid)?;
		}
		for section in self.sections.values_mut() {
			section.finish()?;
		}
		self.si.finish(self.last_pts.unwrap_or(Timestamp::ZERO))?;
		Ok(())
	}

	/// Snapshot what every elementary stream has delivered, the audio frame sync lost on the
	/// way, damaged units refused, and the PAT and PMT sections dropped for a bad CRC.
	///
	/// Cheap: it reads counters the demuxer already keeps, so a caller can poll it per
	/// chunk and report the delta.
	pub fn stats(&self) -> stats::Snapshot {
		let mut streams = self.retired_stats.clone();
		let routes = self
			.streams
			.iter()
			.filter_map(|(pid, stream)| Some((pid.as_u16(), stream.stats()?)))
			.chain(
				self.sections
					.keys()
					.map(|&pid| (pid, stats::Stream::new(".ts", stats::Class::Data))),
			);
		for (pid, current) in routes {
			streams
				.entry(pid)
				.and_modify(|retired| retired.merge(&current))
				.or_insert(current);
		}
		// A dedicated PCR PID routes no stream, so its damage gets a clock-only row.
		for (&pid, &damaged) in &self.damaged {
			streams
				.entry(pid)
				.or_insert_with(|| stats::Stream::new("", stats::Class::Data))
				.damaged = damaged;
		}
		// A stream refused before it had a track has no row of its own either, nor does an
		// SI PID.
		for (&pid, &unplaced) in &self.unplaced {
			let track = if catalog::SI_PIDS.contains(&pid) { ".si" } else { "" };
			streams
				.entry(pid)
				.or_insert_with(|| stats::Stream::new(track, stats::Class::Data))
				.unplaced = unplaced;
		}
		for (&pid, &refused) in &self.refused {
			streams
				.entry(pid)
				.or_insert_with(|| stats::Stream::new("", stats::Class::Data))
				.refused = refused;
		}
		self.unclaimed.report(&mut streams, |pid| self.claims_pid(pid));
		for (pid, stats) in &mut streams {
			(stats.units, stats.quiet) = self.liveness.stream(*pid);
		}
		let mut stats = stats::Snapshot {
			streams,
			crc_error: self.crc_errors.values().sum::<u64>() + self.demux.pat_crc_error(),
			..Default::default()
		};
		let mut errors = self.demux.errors().clone();
		errors.max(self.health.errors());
		errors.report(&mut stats);
		stats
	}

	/// The TR 101 290 counts this program timed, for merging programs that read the same
	/// multiplex.
	pub(super) fn errors(&self) -> &super::health::Errors {
		self.health.errors()
	}

	/// PMT sections dropped for a bad CRC per PID, for merging programs that read the same PMT.
	pub(super) fn crc_errors(&self) -> &BTreeMap<u16, u64> {
		&self.crc_errors
	}

	/// Abort every track with `err` instead of finishing, so subscribers see the
	/// real cause rather than [`moq_net::Error::Dropped`]. Buffered PES is discarded.
	/// Consumes the importer.
	///
	/// Unlike a drop, this leaves the tracks' HLS records to the catalog, whose abort fails them
	/// rather than ending them as a complete recording.
	pub fn abort(mut self, err: moq_net::Error) {
		self.finished = true;
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

impl<E: catalog::Catalog> Drop for Import<E> {
	/// End the input unless it already ended, then retire every route's HLS records: each route
	/// mints its own name, so nothing publishes it again.
	fn drop(&mut self) {
		if !self.finished
			&& let Err(err) = self.finish()
		{
			tracing::warn!(%err, "failed to finish a dropped TS import");
		}
		for stream in std::mem::take(&mut self.streams).into_values() {
			let name = stream.track_name();
			drop(stream);
			if let Some(name) = name {
				self.catalog.retire_dropped(&name);
			}
		}
	}
}

/// A PAT listed more than one program, and the [`Import`] was not told which one to take.
///
/// Importing them all onto one broadcast would put unrelated clocks on one timeline, so the
/// import stops instead. Pick one with [`Import::with_program`], or import each as its own
/// broadcast with [`Programs`](super::Programs).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("transport stream carries {} programs ({})", .programs.len(), list_programs(.programs))]
pub struct MultipleProgramsError {
	/// The program numbers the PAT lists, in PAT order.
	pub programs: Vec<u16>,
}

fn list_programs(programs: &[u16]) -> String {
	programs.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}

/// What a PMT says of one elementary stream. A route lasts as long as its PID's listing does.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Listing {
	stream_type: u8,
	descriptors: Vec<catalog::Descriptor>,
	/// Reassembled as sections: [`section_framed`] under the program descriptors it came with.
	sections: bool,
}

/// Whether a stream of `stream_type` carries sections rather than PES: private sections (0x05),
/// the ISO 13818-6 DSM-CC types, each of which rides DSM-CC sections (0x0A multiprotocol
/// encapsulation, 0x0B U-N messages and carousels, 0x0C stream descriptors, 0x0D any DSM-CC
/// section), and SCTE-35 (0x86) in a program whose descriptors carry the 'CUEI' registration.
/// 0x86 is also a DTS audio type, so without it the stream is not read as cues.
fn section_framed(stream_type: u8, cuei: bool) -> bool {
	matches!(stream_type, 0x05 | 0x0a..=0x0d) || (cuei && stream_type == StreamType::Dts8ChannelLosslessAudio as u8)
}

// Match the raw TS accumulator: half a MoQ group bounds one program's staged input.
const STAGING_BYTES_MAX: usize = (moq_net::group::MAX_CACHE_BYTES / 2) as usize;
const STAGING_UNITS_MAX: usize = moq_net::group::MAX_GROUP_FRAMES / 2;

#[derive(Default)]
struct Staging {
	bytes: AtomicUsize,
	units: AtomicUsize,
}

/// Accounting follows ownership through flush, loss, remapping, and importer drop.
struct Retained {
	staging: Arc<Staging>,
	bytes: usize,
	units: usize,
}

impl Retained {
	fn new(staging: Arc<Staging>) -> Self {
		Self {
			staging,
			bytes: 0,
			units: 0,
		}
	}

	fn grow(&mut self, bytes: usize, units: usize) -> anyhow::Result<()> {
		let reserve = |used: &AtomicUsize, add: usize, max: usize| {
			if add == 0 {
				return true;
			}
			used.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
				used.checked_add(add).filter(|&sum| sum <= max)
			})
			.is_ok()
		};
		anyhow::ensure!(
			reserve(&self.staging.bytes, bytes, STAGING_BYTES_MAX),
			"TS staging exceeds {STAGING_BYTES_MAX} bytes while waiting for a PES boundary or timestamp"
		);
		if !reserve(&self.staging.units, units, STAGING_UNITS_MAX) {
			self.staging.bytes.fetch_sub(bytes, Ordering::Relaxed);
			anyhow::bail!(
				"TS staging exceeds {STAGING_UNITS_MAX} buffered units while waiting for a boundary or timestamp"
			);
		}
		self.bytes += bytes;
		self.units += units;
		Ok(())
	}

	fn resize(&mut self, bytes: usize, units: usize) -> anyhow::Result<()> {
		self.grow(bytes.saturating_sub(self.bytes), units.saturating_sub(self.units))?;
		self.staging.bytes.fetch_sub(self.bytes - bytes, Ordering::Relaxed);
		self.staging.units.fetch_sub(self.units - units, Ordering::Relaxed);
		self.bytes = bytes;
		self.units = units;
		Ok(())
	}

	fn take(&mut self, bytes: usize, units: usize) -> Self {
		self.bytes -= bytes;
		self.units -= units;
		Self {
			staging: self.staging.clone(),
			bytes,
			units,
		}
	}

	fn clear(&mut self) {
		self.staging.bytes.fetch_sub(self.bytes, Ordering::Relaxed);
		self.staging.units.fetch_sub(self.units, Ordering::Relaxed);
		self.bytes = 0;
		self.units = 0;
	}
}

impl Drop for Retained {
	fn drop(&mut self) {
		self.clear();
	}
}

/// A reassembled PES packet awaiting routing to its codec importer.
struct Pending {
	retained: Retained,
	/// The unwrapped 90 kHz PTS,
	/// before the offset.
	pts: Option<u64>,
	/// The timebase's offset, which the PTS shifts by onto the catalog clock.
	offset: Offset,
	/// Raw 90 kHz DTS, still wrapped. Present on reordered (B-frame) video; its distance below
	/// the PTS, modulo the wrap, is the reorder delay published as the catalog jitter.
	dts: Option<u64>,
	/// The payload, or the whole packet, header included, for a [`PesPacketStream`].
	data: Vec<u8>,
	/// Expected length of `data` for bounded PES, else `None` (unbounded video).
	data_len: Option<usize>,
	/// Where a whole PES sits on the program clock; `None` for every other stream.
	span: Option<Span>,
}

impl Pending {
	/// A PES carrying nothing, used to drain a stream's carried tail at end of stream.
	fn empty() -> Self {
		Self {
			retained: Retained::new(Arc::default()),
			pts: None,
			offset: Offset::default(),
			dts: None,
			data: Vec::new(),
			data_len: None,
			span: None,
		}
	}
}

/// Create a track for an undecoded stream and record `entry`, its carriage record, under its
/// name in the `mpegts` catalog section. Shared by the section and whole-PES paths.
fn register_verbatim<E: catalog::Catalog>(
	broadcast: &mut moq_net::broadcast::Producer,
	catalog: &mut crate::catalog::Producer<E>,
	entry: catalog::Track,
) -> anyhow::Result<crate::container::Producer<crate::catalog::hang::Container>> {
	// Verbatim payloads ride the legacy container, which normalizes the per-frame
	// timestamp to microseconds on the wire (see `hang::container::Frame::encode`),
	// so the track declares that timescale to match.
	//
	// Priority follows text rather than the media tiers: an undecoded elementary
	// stream (SCTE-35 cues, teletext, DVB subtitles) is tiny and timing-critical, so
	// it should never queue behind a media backlog, and it's too small to starve
	// anything by sitting above one.
	let track = broadcast.unique_track(".ts", catalog.track_info(hang::catalog::PRIORITY.text))?;
	let name = track.name().to_string();

	// Build the media producer before advertising the track. It is fallible (its
	// timeline track can collide), and the `VerbatimEntry` that removes this catalog
	// entry on drop only exists once this function returns successfully, so an entry
	// published first would be stranded.
	let media = catalog.media_raw(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	)?;

	let mut guard = catalog.modify()?;
	let Some(mpegts) = guard.ext.mpegts_mut() else {
		// supports_mpegts was true when sampled at construction; None here means the
		// catalog dropped the section since.
		anyhow::bail!("catalog extension no longer carries an mpegts section");
	};
	mpegts.tracks.insert(name, entry);
	drop(guard);

	Ok(media)
}

/// Remove a verbatim track's entry from the `mpegts` catalog section on drop.
fn unregister_verbatim<E: catalog::Catalog>(catalog: &mut crate::catalog::Producer<E>, name: &str) {
	// A closed catalog has nothing left to unregister from.
	let Ok(mut catalog) = catalog.modify() else {
		return;
	};
	if let Some(mpegts) = catalog.ext.mpegts_mut() {
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
/// track and catalog entry and places each section on the PCR clock.
struct SectionStream<E: catalog::Catalog> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	/// Clears this track's catalog entry on drop; its name retires the timeline when the route ends.
	entry: VerbatimEntry<E>,
	reassembler: SectionReassembler,
	/// Sections waiting for the next PCR or the timebase's offset, in input order.
	held: VecDeque<Held>,
	staging: Arc<Staging>,
}

impl<E: catalog::Catalog> SectionStream<E> {
	fn new(
		mut broadcast: moq_net::broadcast::Producer,
		mut catalog: crate::catalog::Producer<E>,
		pid: u16,
		stream_type: u8,
		descriptors: Vec<catalog::Descriptor>,
	) -> anyhow::Result<Self> {
		let entry = catalog::Track {
			pid,
			descriptors,
			verbatim: Some(catalog::Verbatim::new(stream_type, catalog::Framing::Section)),
			pes_packet: None,
		};
		let track = register_verbatim(&mut broadcast, &mut catalog, entry)?;
		let entry = VerbatimEntry {
			name: track.name().to_string(),
			catalog,
		};
		Ok(Self {
			track,
			entry,
			reassembler: SectionReassembler::default(),
			held: VecDeque::new(),
			staging: Arc::default(),
		})
	}

	/// Reassemble sections, retaining their first byte's placement across PCRs and fragments.
	/// Returns how many began before the first PCR and cannot be placed.
	fn packet(&mut self, pkt: &[u8], at: u64, placement: &Placement) -> anyhow::Result<u64> {
		let mut completed = Vec::new();
		self.reassembler.push_at(pkt, Some((at, placement)), &mut completed);
		let mut unplaced = 0;
		for (span, section) in completed {
			let Some(span) = span else {
				unplaced += 1;
				continue;
			};
			let mut retained = Retained::new(self.staging.clone());
			retained.grow(section.len(), 1)?;
			self.held.push_back(Held {
				payload: section,
				span,
				_retained: retained,
			});
		}
		Ok(unplaced)
	}

	fn bracket(&mut self, generation: u64, after: Sample) {
		for span in self
			.reassembler
			.span
			.iter_mut()
			.chain(self.held.iter_mut().map(|held| &mut held.span))
		{
			span.bracket(generation, after);
		}
	}

	fn settle(&mut self, placement: &Placement) {
		for span in self
			.reassembler
			.span
			.iter_mut()
			.chain(self.held.iter_mut().map(|held| &mut held.span))
		{
			span.settle(placement);
		}
	}

	/// Publish sections with the same placement and unplaceable handling as whole PES.
	fn release(
		&mut self,
		timebase: &crate::catalog::Timebase<E>,
		pts: &PtsUnwrap,
		generation: u64,
	) -> anyhow::Result<(u64, u64)> {
		let Some(offset) = timebase.offset() else {
			return Ok((0, 0));
		};
		let (mut published, mut unplaced) = (0, 0);
		while let Some(timestamp) = self
			.held
			.front()
			.and_then(|held| held.timestamp(pts, offset, generation))
		{
			let mut held = self.held.pop_front().expect("a front");
			let Some(timestamp) = timestamp else {
				unplaced += 1;
				continue;
			};
			adjust_splice(&mut held.payload, offset);
			self.track.write(crate::container::Frame {
				timestamp,
				duration: None,
				payload: bytes::Bytes::from(held.payload),
				keyframe: true,
			})?;
			self.track.cut(None)?;
			published += 1;
		}
		Ok((published, unplaced))
	}

	fn discontinuity(&mut self, published: bool) -> anyhow::Result<()> {
		// The half-reassembled section is stamped with the clock that just ended.
		self.reassembler = SectionReassembler::default();
		if published {
			self.track.discontinuity()?;
		}
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

/// Shift a SCTE-35 `splice_info_section`'s splice times by `offset`, the shift its media took
/// onto the catalog clock.
///
/// Its `pts_time` fields name the source's PTS base, which nothing downstream can recover once
/// the media moved, so the section's `pts_adjustment` (added to every `pts_time`, modulo 2^33)
/// absorbs the offset and the `CRC_32` is recomputed. The field is outside the encrypted part, so
/// an encrypted section is adjusted alike. Any other section, one too short to be a SCTE-35
/// section, or one failing its CRC is left alone.
fn adjust_splice(section: &mut [u8], offset: Offset) {
	const TABLE_ID: u8 = 0xFC;
	const FIELD: i128 = 1 << 33;
	// table_id through cw_index, plus the CRC_32: the shortest section carrying the field.
	if offset == Offset::default() || section.len() < 14 || section[0] != TABLE_ID {
		return;
	}
	// A corrupt section stays corrupt rather than gaining a CRC that vouches for it.
	if psi::CRC.checksum(section) != 0 {
		return;
	}

	// pts_adjustment: the low bit of byte 4, then bytes 5..9.
	let field = ((section[4] as u64 & 1) << 32) | u32::from_be_bytes(section[5..9].try_into().unwrap()) as u64;
	let adjusted = (field as i128 + offset.ticks(90_000)).rem_euclid(FIELD) as u64;
	section[4] = (section[4] & !1) | (adjusted >> 32) as u8;
	section[5..9].copy_from_slice(&(adjusted as u32).to_be_bytes());

	let body = section.len() - 4;
	let crc = psi::CRC.checksum(&section[..body]);
	section[body..].copy_from_slice(&crc.to_be_bytes());
}

/// Publishes each PES of an elementary stream we don't decode (private data, teletext, DVB
/// subtitles, DVB AC-3, DTS, ...) whole, as one frame on a track described by a
/// [`PesPacket`](catalog::PesPacket) record in the `mpegts` catalog section.
///
/// A frame is the packet from its start code through its payload, its PTS and DTS moved onto the
/// catalog clock and every other byte as received, timestamped with its placement on the program
/// clock. A PES without a PTS keeps none. See [`Import`] for when one is held, dropped, or refuses
/// its stream.
///
/// Each PES is a group of its own, except MPEG-1/2 video's: a PES in which a sequence header
/// begins opens a group, where a decoder can start, and the pictures after it join that group.
/// Those before the first sequence header, or after a loss until the next, are dropped, since
/// they depend on pictures a subscriber never gets. A video PES ending in part of a start code
/// waits for the next PES on the stream, or for the stream to end, to tell whether a sequence
/// header begins in it.
struct PesPacketStream<E: catalog::Catalog> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	/// Clears this track's catalog entry on drop; its name retires the timeline when the route ends.
	entry: VerbatimEntry<E>,
	/// The PMT `stream_type`, which says whether an unbounded PES is video.
	stream_type: u8,
	/// Whole PES waiting for the PCR after them or the timebase's offset, in input order, each
	/// with where it falls in the groups.
	held: VecDeque<(Held, Join)>,
	/// The stream lost a unit since the last PES held, so the next joins no group.
	lost: bool,
	/// The PES at the back of `held` ends in part of a start code, so whether it opens a group
	/// waits for the PES after it; [`Self::release`] stops before it meanwhile.
	straddles: bool,
}

/// Where a whole PES falls in its track's groups.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Join {
	/// It opens a group: every PES of a stream other than MPEG-1/2 video, and one of that video
	/// carrying a sequence header.
	Opens,
	/// It joins the group open before it.
	Continues,
	/// It follows a loss, so the group before it ends and it is dropped with the PES after it
	/// until one opens a group.
	Resyncs,
}

/// A whole PES or section waiting to be placed.
struct Held {
	payload: Vec<u8>,
	span: Span,
	/// Held for its `Drop`, which returns the bytes to the staging budget.
	_retained: Retained,
}

impl Held {
	/// `None` waits for a PCR; `Some(None)` drops a unit its timebase could not place.
	fn timestamp(&self, pts: &PtsUnwrap, offset: Offset, generation: u64) -> Option<Option<Timestamp>> {
		match self.span.ticks {
			Some(ticks) => Some(placement(pts, offset, ticks)),
			None if self.span.generation != generation => Some(None),
			None => None,
		}
	}
}

impl<E: catalog::Catalog> PesPacketStream<E> {
	fn new(
		mut broadcast: moq_net::broadcast::Producer,
		mut catalog: crate::catalog::Producer<E>,
		pid: u16,
		stream_type: u8,
		descriptors: Vec<catalog::Descriptor>,
	) -> anyhow::Result<Self> {
		let entry = catalog::Track {
			pid,
			descriptors,
			verbatim: None,
			pes_packet: Some(catalog::PesPacket::new(stream_type)),
		};
		let track = register_verbatim(&mut broadcast, &mut catalog, entry)?;
		let entry = VerbatimEntry {
			name: track.name().to_string(),
			catalog,
		};
		Ok(Self {
			track,
			entry,
			stream_type,
			held: VecDeque::new(),
			lost: false,
			straddles: false,
		})
	}

	/// Hold one complete PES until it can be placed: [`Self::release`] publishes it.
	fn hold(&mut self, pending: Pending) {
		let span = pending.span.expect("a whole PES is spanned at its start");
		let join = match mpeg_video(self.stream_type) {
			false => Join::Opens,
			true => {
				let payload = es_payload(&pending.data);
				if std::mem::take(&mut self.straddles) {
					let (before, join) = self.held.back_mut().expect("a straddling PES is held");
					if straddled_header(es_payload(&before.payload), payload) {
						*join = Join::Opens;
					}
				}
				let join = match () {
					_ if memchr::memmem::find(payload, &SEQUENCE_HEADER).is_some() => Join::Opens,
					_ if self.lost => Join::Resyncs,
					_ => Join::Continues,
				};
				self.straddles = join != Join::Opens && partial_start_code(payload);
				join
			}
		};
		self.lost = false;
		let held = Held {
			payload: pending.data,
			span,
			_retained: pending.retained,
		};
		self.held.push_back((held, join));
	}

	/// The unit in progress was lost: an MPEG-1/2 picture after it may reference it, and no
	/// sequence header continues from the PES before it.
	fn lost(&mut self) {
		self.lost = true;
		self.straddles = false;
	}

	/// No PES follows the last held on its clock, so a start code its end began completes nothing.
	fn end(&mut self) {
		self.straddles = false;
	}

	/// The PCR `after` arrived on timebase `generation`: every PES held since the last is
	/// bracketed.
	fn bracket(&mut self, generation: u64, after: Sample) {
		for (held, _) in &mut self.held {
			held.span.bracket(generation, after);
		}
	}

	/// The timebase of `placement` ends: extrapolate the PES held past its last PCR.
	fn settle(&mut self, placement: &Placement) {
		for (held, _) in &mut self.held {
			held.span.settle(placement);
		}
	}

	/// Publish the placed PES at the front, grouped as [`Join`] says, once `timebase` has its
	/// offset, and drop those no PCR on their own timebase can place: one ended before them, now
	/// that `generation` is the timebase running. `pts` is the program's PTS history, which
	/// picks each PCR's wrap. Returns how many went each way; a picture no group takes is
	/// neither.
	fn release(
		&mut self,
		timebase: &crate::catalog::Timebase<E>,
		pts: &PtsUnwrap,
		generation: u64,
	) -> anyhow::Result<(u64, u64)> {
		let Some(offset) = timebase.offset() else {
			return Ok((0, 0));
		};
		let (mut published, mut unplaced) = (0, 0);
		while let Some(timestamp) = self
			.held
			.front()
			.filter(|_| !(self.straddles && self.held.len() == 1))
			.and_then(|(held, _)| held.timestamp(pts, offset, generation))
		{
			let (mut held, join) = self.held.pop_front().expect("a front");
			let Some(timestamp) = timestamp else {
				unplaced += 1;
				continue;
			};
			rebase(&mut held.payload, offset);
			if join == Join::Resyncs {
				self.track.cut(None)?;
			}
			let frame = crate::container::Frame {
				timestamp,
				duration: None,
				payload: bytes::Bytes::from(held.payload),
				keyframe: join == Join::Opens,
			};
			match self.track.write(frame) {
				Ok(()) => published += 1,
				// A picture with no group open: before the first sequence header, or after a loss.
				Err(crate::Error::MissingKeyframe(_)) => continue,
				Err(err) => return Err(err.into()),
			}
			if !mpeg_video(self.stream_type) {
				self.track.cut(None)?;
			}
		}
		Ok((published, unplaced))
	}

	fn discontinuity(&mut self, published: bool) -> anyhow::Result<()> {
		if published {
			self.track.discontinuity()?;
		}
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

/// Why a whole-PES track cannot carry `pes` unaltered, if it cannot: a header export refuses
/// ([`PesStart::whole`]), a clock field only the PTS and DTS of are rebased, or a scrambled
/// payload.
fn refusal(pes: &PesStart, stream_type: u8) -> Option<String> {
	match pes.whole(stream_type) {
		Err(err) => Some(format!("{err:#}")),
		Ok(Some(clock)) => Some(clock.to_string()),
		Ok(None) => pes.scrambled().then(|| "PES-level scrambling".to_string()),
	}
}

/// The catalog time of the program clock reading `ticks`, a 27 MHz count, on a timebase shifted
/// by `offset`: its PCR in the wrap nearest the program's PTS history, floored to microseconds.
/// `None` for a reading before the broadcast clock's zero.
fn placement(pts: &PtsUnwrap, offset: Offset, ticks: u64) -> Option<Timestamp> {
	let pcr = pts.pcr(ticks % PCR_WRAP);
	let micros = pcr.div_euclid(i128::from(PCR_HZ / 1_000_000)) + offset.ticks(1_000_000);
	Timestamp::from_micros(u64::try_from(micros).ok()?).ok()
}

/// Move a whole PES's PTS and DTS onto the catalog clock in place, modulo 2^33 as a media PTS
/// moves, leaving their prefix and marker bits and every other byte as they were. The PES CRC
/// covers the previous packet's data bytes, not this header, so it stays valid.
fn rebase(pes: &mut [u8], offset: Offset) {
	const FIELD: i128 = 1 << 33;
	if offset == Offset::default() || !psi::has_optional_header(pes[3]) {
		return;
	}
	let fields = match pes[7] >> 6 {
		0b10 => 1,
		0b11 => 2,
		_ => 0,
	};
	for field in pes[9..9 + 5 * fields].as_chunks_mut::<5>().0 {
		let value = (u64::from(field[0] >> 1) & 0x07) << 30
			| u64::from(field[1]) << 22
			| u64::from(field[2] >> 1) << 15
			| u64::from(field[3]) << 7
			| u64::from(field[4] >> 1);
		let value = (i128::from(value) + offset.ticks(90_000)).rem_euclid(FIELD) as u64;
		field[0] = (field[0] & 0xf1) | ((value >> 29) & 0x0e) as u8;
		field[1] = (value >> 22) as u8;
		field[2] = (field[2] & 0x01) | ((value >> 14) & 0xfe) as u8;
		field[3] = (value >> 7) as u8;
		field[4] = (field[4] & 0x01) | ((value << 1) & 0xfe) as u8;
	}
}

/// The program clock at each input byte, from the PCRs of the current timebase, which places
/// whole PES and sections: a byte is interpolated at the rate between the PCRs either side of it, as
/// [`Passthrough`](super::Passthrough) times its objects.
#[derive(Default)]
struct Placement {
	/// The last two PCRs of the timebase, newest last, unwrapped from its first.
	samples: VecDeque<Sample>,
	/// The last PCR, raw.
	raw: u64,
	/// Counts the timebases ended, so a PES never takes a PCR from one it did not start in.
	generation: u64,
}

impl Placement {
	/// A PCR of `raw` 27 MHz ticks whose reference byte sits at input offset `at`.
	fn pcr(&mut self, raw: u64, at: u64) -> Sample {
		let ticks = match self.samples.back() {
			Some(last) => last.ticks + (raw + PCR_WRAP - self.raw) % PCR_WRAP,
			None => raw,
		};
		let sample = Sample { at, ticks };
		self.samples.push_back(sample);
		if self.samples.len() > 2 {
			self.samples.pop_front();
		}
		self.raw = raw;
		sample
	}

	/// Whether `raw` steps back from the last PCR, modulo the wrap.
	fn rewinds(&self, raw: u64) -> bool {
		!self.samples.is_empty() && (raw + PCR_WRAP - self.raw) % PCR_WRAP >= PCR_WRAP / 2
	}

	/// The timebase's last PCR.
	fn last(&self) -> Option<Sample> {
		self.samples.back().copied()
	}

	/// A PES or section whose first byte sits at input offset `at`, after the last PCR;
	/// `None` before the first.
	fn span(&self, at: u64) -> Option<Span> {
		Some(Span {
			at,
			before: self.last()?,
			generation: self.generation,
			ticks: None,
		})
	}

	/// The program clock at input offset `at`, past the last PCR, at the rate of the interval
	/// before it; `None` without one.
	fn extrapolate(&self, at: u64) -> Option<u64> {
		(self.samples.len() == 2).then(|| locate(at, &self.samples))
	}

	/// End the timebase.
	fn reset(&mut self) {
		self.samples.clear();
		self.generation += 1;
	}
}

/// Where a PES or section starts in the input, and the program clock there once known.
#[derive(Clone, Copy)]
struct Span {
	/// The input offset of its first byte.
	at: u64,
	/// The last PCR before it.
	before: Sample,
	/// The [`Placement::generation`] it started in.
	generation: u64,
	/// The program clock at its first byte, in its timebase's unwrapped 27 MHz ticks.
	ticks: Option<u64>,
}

impl Span {
	/// Interpolate with `after`, the first PCR past it on its timebase.
	fn bracket(&mut self, generation: u64, after: Sample) {
		if self.ticks.is_none() && self.generation == generation {
			self.ticks = Some(locate(self.at, &VecDeque::from([self.before, after])));
		}
	}

	/// Its timebase ended with no PCR after it: extrapolate from the last two.
	fn settle(&mut self, placement: &Placement) {
		if self.ticks.is_none() && self.generation == placement.generation {
			self.ticks = placement.extrapolate(self.at);
		}
	}
}

/// Whether a packet's adaptation field sets `discontinuity_indicator`.
///
/// What that declares depends on the PID: a continuity-counter break on an elementary
/// stream, and additionally a system time-base break on the program's PCR PID. It rides an
/// adaptation-only packet (a clock packet with no payload) as readily as a payload one.
pub(super) fn discontinuity_indicator(pkt: &[u8; 188]) -> bool {
	pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x80 != 0
}

/// The PCR a packet's adaptation field carries, in 27 MHz ticks.
pub(super) fn pcr(pkt: &[u8; 188]) -> Option<u64> {
	if pkt[3] & 0x20 == 0 || pkt[4] < 7 || pkt[5] & 0x10 == 0 {
		return None;
	}
	let base = (u64::from(pkt[6]) << 25)
		| (u64::from(pkt[7]) << 17)
		| (u64::from(pkt[8]) << 9)
		| (u64::from(pkt[9]) << 1)
		| (u64::from(pkt[10]) >> 7);
	let ext = (u64::from(pkt[10] & 0x01) << 8) | u64::from(pkt[11]);
	Some(base * 300 + ext)
}

/// How long each elementary stream has gone without delivering an access unit.
///
/// Measured on the program clock, the PCR summed interval by interval, so it runs straight
/// through the 33-bit wrap, a signalled time-base reset and a corrupt PCR instead of jumping
/// with them. Not the media clock: that follows the video PTS, and stops with the very stream
/// this catches. [`Export`](super::Export) runs the same meter on the PCR it writes.
#[derive(Default)]
pub(super) struct Liveness {
	/// The last PCR in 27 MHz ticks, forgotten at a reset so no interval spans one.
	pcr: Option<u64>,
	/// PCR ticks elapsed since the first PCR, if one has arrived.
	elapsed: Option<u64>,
	/// Per PID: access units delivered, and `elapsed` at the last one, or at registration.
	/// Kept per PID rather than per route, so a PMT remap carries it like `retired_stats`.
	streams: HashMap<u16, (u64, u64)>,
	/// PIDs the PMT stopped listing: their counts stay, but they are not quiet.
	retired: HashSet<u16>,
}

impl Liveness {
	/// A PCR arrived on the program's clock PID.
	///
	/// Stepped one interval at a time, so a corrupt PCR, or the clock stepping back without a
	/// flag, costs one interval rather than inventing hours of silence on every PID. The bound
	/// is the mux-rate meter's.
	pub(super) fn pcr(&mut self, pcr: u64) {
		self.step(pcr, super::mux_rate::MAX_INTERVAL);
	}

	/// A PCR the caller wrote itself. Its clock breaks only where the caller flags a
	/// discontinuity, so every forward step counts, including the jump across a media gap
	/// longer than the backfill covers.
	pub(super) fn written_pcr(&mut self, pcr: u64) {
		self.step(pcr, super::mux_rate::PCR_WRAP);
	}

	fn step(&mut self, pcr: u64, max: u64) {
		let elapsed = self.elapsed.get_or_insert(0);
		if let Some(last) = self.pcr.replace(pcr) {
			let step = (pcr + super::mux_rate::PCR_WRAP - last) % super::mux_rate::PCR_WRAP;
			if step <= max {
				*elapsed += step;
			}
		}
	}

	/// PCR ticks elapsed on the program clock since its first PCR, or `None` before one.
	pub(super) fn now(&self) -> Option<u64> {
		self.elapsed
	}

	/// The clock restarted or changed PID: the next interval measures nothing.
	pub(super) fn discontinuity(&mut self) {
		self.pcr = None;
	}

	/// The PMT declared `pid`. Its silence counts from here until it delivers.
	pub(super) fn register(&mut self, pid: u16) {
		let now = self.elapsed.unwrap_or(0);
		let entry = self.streams.entry(pid).or_insert((0, now));
		if self.retired.remove(&pid) {
			entry.1 = now;
		}
	}

	/// The route on `pid` ended. Until a PMT declares it again it is not silent, only gone.
	pub(super) fn retire(&mut self, pid: u16) {
		if self.streams.contains_key(&pid) {
			self.retired.insert(pid);
		}
	}

	/// `pid` delivered `units` access units just now.
	pub(super) fn delivered(&mut self, pid: u16, units: u64) {
		if units == 0 {
			return;
		}
		let now = self.elapsed.unwrap_or(0);
		if let Some((count, last)) = self.streams.get_mut(&pid) {
			*count += units;
			*last = now;
		}
	}

	/// `pid`'s access units and how long it has been quiet. See [`stats::Stream`].
	pub(super) fn stream(&self, pid: u16) -> (u64, Option<std::time::Duration>) {
		let Some(&(units, last)) = self.streams.get(&pid) else {
			return (0, None);
		};
		if self.retired.contains(&pid) {
			return (units, None);
		}
		let quiet = self
			.elapsed
			.map(|now| std::time::Duration::from_nanos((now - last) * 1_000 / 27));
		(units, quiet)
	}
}

/// Locks onto the 188-byte packet grid of a TS byte stream.
///
/// 0x47 is the packet sync byte but also occurs freely in payload (TS has no byte
/// stuffing), so a lone 0x47 isn't a boundary.
#[derive(Default)]
pub(super) struct Framer {
	/// False until a candidate is confirmed by the next packet's sync byte; once true we
	/// stride 188 at a time and trust the per-packet check. Persists across calls so a
	/// candidate pending confirmation at a buffer tail is re-confirmed, not trusted blindly.
	synced: bool,
}

impl Framer {
	/// Where the next whole packet in `buf` at or after `*off` starts, advancing `*off` past
	/// it. `None` once no confirmed packet remains, with `*off` at the first byte the next
	/// call still needs.
	pub(super) fn next(&mut self, buf: &[u8], off: &mut usize) -> Option<usize> {
		while *off + TsPacket::SIZE <= buf.len() {
			if self.synced && buf[*off] == 0x47 {
				let at = *off;
				*off += TsPacket::SIZE;
				return Some(at);
			}
			self.synced = false;
			// Scan (SIMD via memchr) for a candidate whose next packet also begins with 0x47,
			// confirming the 188 stride before locking onto it. Striding past a false
			// candidate would route one bogus packet; jumping a flat 188 instead would only
			// re-align on exact multiples and could desync forever.
			loop {
				let Some(rel) = memchr::memchr(0x47, &buf[*off..]) else {
					// No sync byte left: the buffer is junk, drop it.
					*off = buf.len();
					break;
				};
				*off += rel;
				match buf.get(*off + TsPacket::SIZE) {
					// Next packet also starts with a sync byte: lock onto this candidate.
					Some(&0x47) => {
						self.synced = true;
						break;
					}
					// The byte 188 ahead isn't a sync byte: this 0x47 was payload, keep scanning.
					Some(_) => *off += 1,
					// Can't confirm yet (candidate is near the buffer tail). Stay unsynced so
					// it's re-confirmed next call (with the trailing bytes) instead of trusted.
					None => break,
				}
			}
			// Unsynced means the buffer had no confirmable sync byte, or the candidate is
			// pending confirmation; either way there's nothing to route until more arrives.
			if !self.synced {
				return None;
			}
		}
		None
	}
}

/// Reads whole PATs off PID 0: reassembles its sections, drops any whose CRC fails, and
/// collects the rest until every section of a version is in.
#[derive(Default)]
pub(super) struct PatReader {
	sections: SectionReassembler,
	table: psi::PatAssembler,
}

impl PatReader {
	/// Consume one packet on PID 0, counting each section dropped for a bad CRC into
	/// `crc_error`. Returns the whole PAT if this packet completed one.
	pub(super) fn push(&mut self, pkt: &[u8; TsPacket::SIZE], crc_error: &mut u64) -> Option<psi::Pat> {
		let mut sections = Vec::new();
		self.sections.push(pkt, &mut sections);
		let mut pat = None;
		for section in sections {
			if section.first() != Some(&psi::PAT_TABLE_ID) {
				continue;
			}
			if !psi::crc_ok(&section) {
				*crc_error += 1;
				tracing::warn!(pid = Pid::PAT, "dropped a PSI section with a bad CRC");
				continue;
			}
			pat = self.table.section(&section).or(pat);
		}
		pat
	}
}

/// Whether every adaptation field fits inside the length its packet declares.
pub(super) fn adaptation_valid(pkt: &[u8; TsPacket::SIZE]) -> bool {
	if pkt[3] & 0x20 == 0 {
		return true;
	}
	// Without payload the adaptation field fills the packet; with payload it leaves a byte.
	let length = usize::from(pkt[4]);
	if (pkt[3] & 0x10 == 0 && length != 183) || (pkt[3] & 0x10 != 0 && length > 182) {
		return false;
	}
	let Some(field) = pkt.get(5..5 + usize::from(pkt[4])) else {
		return false;
	};
	let Some(&flags) = field.first() else {
		return true;
	};
	let mut off =
		1 + usize::from(flags & 0x10 != 0) * 6 + usize::from(flags & 0x08 != 0) * 6 + usize::from(flags & 0x04 != 0);
	for flag in [0x02, 0x01] {
		if flags & flag != 0 {
			let Some(&len) = field.get(off) else {
				return false;
			};
			off += 1 + usize::from(len);
		}
	}
	off <= field.len()
}

/// Where a packet's payload is.
pub(super) enum Payload<'a> {
	/// Adaptation field only, or reserved `adaptation_field_control`.
	None,
	Bytes(&'a [u8]),
	/// The adaptation field claims to run past the packet.
	Malformed,
}

pub(super) fn payload(pkt: &[u8; TsPacket::SIZE]) -> Payload<'_> {
	let afc = (pkt[3] >> 4) & 0x3;
	if afc & 0x1 == 0 {
		return Payload::None;
	}
	let off = if afc & 0x2 != 0 { 5 + pkt[4] as usize } else { 4 };
	match pkt.get(off..) {
		Some([]) => Payload::None,
		Some(payload) => Payload::Bytes(payload),
		None => Payload::Malformed,
	}
}

/// Byte-level reassembler for MPEG-TS private sections on one PID.
///
/// Private sections (SCTE-35 table_id 0xFC and others) are not PES. This handles
/// pointer_field alignment, sections split across packets (including a 3-byte
/// header split, where section_length is not yet known), continuity-counter
/// gaps, and adaptation-field discontinuities. Deliberately minimal: just enough to
/// recover whole sections verbatim.
#[derive(Clone, Default)]
pub(super) struct SectionReassembler {
	/// Bytes of the section currently being reassembled. Its 3-byte header (and
	/// thus section_length) may not all be present yet, so completeness is
	/// re-checked as bytes arrive; empty means no section in progress.
	acc: Vec<u8>,
	/// Placement of the partial section's first byte, bracketed even while it assembles.
	span: Option<Span>,
	/// Shared with the PES path: a broken chain drops the partial either way.
	continuity: Continuity,
	/// Whether the first section the latest packet completed began in an earlier packet.
	resumed: bool,
}

impl SectionReassembler {
	/// Whether the first section the latest packet completed began in an earlier packet.
	pub(super) fn resumed(&self) -> bool {
		self.resumed
	}

	/// Whether a partial section is left assembling.
	pub(super) fn assembling(&self) -> bool {
		!self.acc.is_empty()
	}

	/// Consume one 188-byte TS packet, appending every completed section to `out`.
	pub(super) fn push(&mut self, pkt: &[u8], out: &mut Vec<Vec<u8>>) {
		let mut completed = Vec::new();
		self.push_at(pkt, None, &mut completed);
		out.extend(completed.into_iter().map(|(_, section)| section));
	}

	fn push_at(&mut self, pkt: &[u8], source: Option<(u64, &Placement)>, out: &mut Vec<(Option<Span>, Vec<u8>)>) {
		let pkt: &[u8; 188] = pkt.try_into().expect("section packet must be 188 bytes");
		self.resumed = false;
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
		let payload = match payload(pkt) {
			Payload::None => return,
			Payload::Bytes(payload) => payload,
			// A section can't be found in it, so like a counter gap it costs the partial.
			Payload::Malformed => {
				self.acc.clear();
				return;
			}
		};

		let end = source.map_or(TsPacket::SIZE as u64, |(at, _)| at + TsPacket::SIZE as u64);
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
				let completed = out.len();
				self.drain(out, source, end - (payload.len() - 1 - ptr) as u64);
				self.resumed = out.len() > completed;
			}
			// The pointer_field is a hard section boundary: drop any leftover partial
			// and start the section it points to.
			self.acc.clear();
			self.acc.extend_from_slice(&payload[1 + ptr..]);
			self.span = source.and_then(|(_, placement)| placement.span(end - self.acc.len() as u64));
			self.drain(out, source, end);
		} else if !self.acc.is_empty() {
			// Continuation of the section in progress. A non-PUSI packet with nothing
			// in progress is unaligned (no pointer_field to resync on), so it is
			// ignored until the next PUSI. This keeps us desynced after a gap,
			// discontinuity, or corrupt pointer dropped the partial, rather than
			// resuming on stray bytes that merely look like a section.
			self.acc.extend_from_slice(payload);
			let completed = out.len();
			self.drain(out, source, end);
			self.resumed = out.len() > completed;
		}
	}

	/// Move every complete section out of `acc` into `out`, stopping at the first
	/// partial. The 3-byte header (which holds section_length) can itself be split
	/// across TS packets, so a short buffer waits for more bytes rather than being
	/// dropped. Every complete section is carried verbatim (SCTE-35 and any other
	/// private-section table on the PID); only 0xff stuffing is dropped.
	fn drain(&mut self, out: &mut Vec<(Option<Span>, Vec<u8>)>, source: Option<(u64, &Placement)>, end: u64) {
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
			out.push((self.span.take(), self.acc.drain(..full).collect()));
			self.span = source.and_then(|(_, placement)| placement.span(end - self.acc.len() as u64));
		}
	}
}

/// A parse failure confined to a unit, distinguished from publishing and clock failures.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Damaged(anyhow::Error);

fn unit_error(err: crate::Error) -> anyhow::Error {
	let damaged = matches!(
		&err,
		crate::Error::Annexb(_)
			| crate::Error::H264(
				h264::Error::NalTooShort
					| h264::Error::ForbiddenZeroBit
					| h264::Error::SpsTooShort
					| h264::Error::SpsParse
					| h264::Error::NotInitialized
			) | crate::Error::Aac(
			aac::Error::ProgramConfigMissing | aac::Error::ProgramConfigTruncated | aac::Error::ProgramConfigEmpty
		) | crate::Error::H265(
			h265::Error::NalTooShort
				| h265::Error::ForbiddenZeroBit
				| h265::Error::SpsParse
				| h265::Error::MissingLevelIdc
				| h265::Error::NotInitialized
				| h265::Error::MissingSps
		)
	);
	if damaged {
		Damaged(err.into()).into()
	} else {
		err.into()
	}
}

/// Whether `stream_type` is MPEG-1 or MPEG-2 video, which is carried whole but still paces the
/// program like decoded video.
fn mpeg_video(stream_type: u8) -> bool {
	matches!(stream_type, 0x01 | 0x02)
}

/// An MPEG-1/2 `sequence_header_code` (ISO 13818-2 6.2.2.1), where a decoder can start. Start
/// codes never occur inside coded data, so any match is one.
const SEQUENCE_HEADER: [u8; 4] = [0x00, 0x00, 0x01, 0xb3];

/// The elementary stream bytes of the whole PES `pes`, after its header.
fn es_payload(pes: &[u8]) -> &[u8] {
	let header = match pes.get(3) {
		Some(&stream_id) if psi::has_optional_header(stream_id) => 9 + usize::from(pes.get(8).copied().unwrap_or(0)),
		_ => 6,
	};
	pes.get(header..).unwrap_or_default()
}

/// Whether `payload` ends in the first one to three bytes of a start code (`00`, `00 00`, or
/// `00 00 01`), which the next PES may complete.
fn partial_start_code(payload: &[u8]) -> bool {
	matches!(payload, [.., 0x00] | [.., 0x00, 0x00, 0x01])
}

/// Whether a sequence header begins in `before`'s payload and ends in `after`'s, the next PES
/// on the stream.
fn straddled_header(before: &[u8], after: &[u8]) -> bool {
	let before = &before[before.len().saturating_sub(SEQUENCE_HEADER.len() - 1)..];
	let after = &after[..after.len().min(SEQUENCE_HEADER.len() - 1)];
	memchr::memmem::find(&[before, after].concat(), &SEQUENCE_HEADER).is_some()
}

/// One elementary stream's codec importer.
enum Stream<E: catalog::Catalog = ()> {
	H264 {
		split: h264::Split,
		assembly: video::Assembly,
		import: Box<h264::Import>,
	},
	H265 {
		split: h265::Split,
		assembly: video::Assembly,
		import: Box<h265::Import>,
	},
	Aac(Box<AacStream<E>>),
	Opus(Box<OpusStream>),
	Legacy(Box<LegacyStream<E>>),
	/// A codec we don't decode, carried as whole PES (MPEG-1/2 video, LATM AAC, private PES, ...).
	PesPacket(Box<PesPacketStream<E>>),
	/// MPEG-1/2 video this import refused to carry, kept only to advance the media clock.
	/// [`Self::video`] counts it, so never reuse this variant for audio or data.
	Clock,
	Ignored,
}

impl<E: catalog::Catalog> Stream<E> {
	/// Route one reassembled PES, returning how many access units it published.
	fn write(&mut self, pending: Pending, batched: bool) -> anyhow::Result<u64> {
		match self {
			Stream::H264 {
				split,
				assembly,
				import,
			} => {
				assembly.push(pending)?;
				video::avc(split, import, assembly, false)
			}
			Stream::H265 {
				split,
				assembly,
				import,
			} => {
				assembly.push(pending)?;
				video::hevc(split, import, assembly, false)
			}
			Stream::Aac(stream) => stream.write(pending, batched),
			Stream::Opus(stream) => stream.write(pending),
			Stream::Legacy(stream) => stream.write(pending),
			// Published once placed: [`Import::release_placed`].
			Stream::PesPacket(stream) => {
				stream.hold(pending);
				Ok(0)
			}
			Stream::Clock | Stream::Ignored => Ok(0),
		}
	}

	/// Whether this is the program's video, decoded or not, whose pictures pace the program.
	fn video(&self) -> bool {
		match self {
			Stream::H264 { .. } | Stream::H265 { .. } | Stream::Clock => true,
			Stream::PesPacket(stream) => mpeg_video(stream.stream_type),
			_ => false,
		}
	}

	/// Whether nothing waits to be placed on the program clock.
	fn placed(&self) -> bool {
		match self {
			Stream::PesPacket(stream) => stream.held.is_empty(),
			_ => true,
		}
	}

	/// Whether a PES cut short by a break is still worth publishing.
	///
	/// True where codec framing proves complete units ahead of the cut. A video
	/// assembler publishes only pictures closed by a following codec boundary;
	/// its unconfirmed tail is discarded by `lost`. Verbatim PES are all-or-nothing.
	fn salvages_partial_pes(&self) -> bool {
		match self {
			Stream::Aac(_) | Stream::Legacy(_) | Stream::H264 { .. } | Stream::H265 { .. } => true,
			// Opus carries many packets per PES like the audio above, but its framing is
			// declared rather than self-describing: a trailing packet cut short of the length
			// its control header promises is a parse error, and that error would travel up out
			// of `decode` and end the session. Losing the PES beats losing the broadcast.
			Stream::Opus(_) => false,
			Stream::PesPacket(_) => false,
			Stream::Clock | Stream::Ignored => false,
		}
	}

	/// Sync was lost: drop whatever partial unit is held and stop vouching for the next
	/// boundary. This is [`seek`](Self::seek) without the group-sequence side effect, for a
	/// break the stream recovers from in place.
	fn desync(&mut self) {
		match self {
			Stream::H264 { split, assembly, .. } => {
				split.reset();
				assembly.reset();
			}
			Stream::H265 { split, assembly, .. } => {
				split.reset();
				assembly.reset();
			}
			Stream::Aac(stream) => stream.desync(),
			Stream::Legacy(stream) => stream.desync(),
			Stream::Opus(_) | Stream::PesPacket(_) | Stream::Clock | Stream::Ignored => {}
		}
	}

	/// Bytes were lost mid-stream: [`desync`](Self::desync), and close the open video group
	/// where its content stops. Every picture until the next keyframe may reference what was
	/// lost, so the producer refuses them until a keyframe opens the next group. Left open,
	/// the group would close a GOP later at that keyframe, with an end past its content.
	fn lost(&mut self) -> anyhow::Result<()> {
		self.desync();
		match self {
			Stream::H264 { import, .. } => import.cut(None)?,
			Stream::H265 { import, .. } => import.cut(None)?,
			Stream::PesPacket(stream) => stream.lost(),
			_ => {}
		}
		Ok(())
	}

	/// No PES follows the last on this clock or input: see [`PesPacketStream::end`].
	fn end(&mut self) {
		if let Stream::PesPacket(stream) = self {
			stream.end();
		}
	}

	/// The program clock restarted: mark the break on this track.
	fn discontinuity(&mut self, published: bool) -> anyhow::Result<()> {
		match self {
			Stream::H264 { import, .. } => {
				if published {
					import.discontinuity()?;
				}
				Ok(())
			}
			Stream::H265 { import, .. } => {
				if published {
					import.discontinuity()?;
				}
				Ok(())
			}
			Stream::Aac(stream) => stream.discontinuity(published),
			Stream::Opus(stream) => stream.discontinuity(published),
			Stream::Legacy(stream) => stream.discontinuity(published),
			Stream::PesPacket(stream) => stream.discontinuity(published),
			Stream::Clock | Stream::Ignored => Ok(()),
		}
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		match self {
			Stream::H264 {
				split,
				assembly,
				import,
				..
			} => {
				assembly.reset();
				split.reset();
				Ok(import.seek(sequence)?)
			}
			Stream::H265 {
				split,
				assembly,
				import,
				..
			} => {
				assembly.reset();
				split.reset();
				Ok(import.seek(sequence)?)
			}
			Stream::Aac(stream) => stream.seek(sequence),
			Stream::Opus(stream) => stream.seek(sequence),
			Stream::Legacy(stream) => stream.seek(sequence),
			Stream::PesPacket(stream) => stream.seek(sequence),
			Stream::Clock | Stream::Ignored => Ok(()),
		}
	}

	/// Finish the track, returning the access units a drained tail published.
	fn finish(&mut self) -> anyhow::Result<u64> {
		match self {
			// Only the self-describing audio holds a frame back for a successor to confirm.
			Stream::Aac(stream) => return stream.finish(),
			Stream::Legacy(stream) => return stream.finish(),
			// No input follows: drain the clean tail, then release the splitter and
			// assembler outright, including the parameter caches a loss reset keeps.
			Stream::H264 {
				split,
				assembly,
				import,
			} => {
				let published = video::avc(split, import, assembly, true)?;
				*split = Default::default();
				assembly.finish();
				import.finish()?;
				return Ok(published);
			}
			Stream::H265 {
				split,
				assembly,
				import,
			} => {
				let published = video::hevc(split, import, assembly, true)?;
				*split = Default::default();
				assembly.finish();
				import.finish()?;
				return Ok(published);
			}
			Stream::Opus(stream) => stream.finish()?,
			Stream::PesPacket(stream) => stream.finish()?,
			Stream::Clock | Stream::Ignored => {}
		}
		Ok(0)
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Stream::H264 { import, .. } => import.abort(err),
			Stream::H265 { import, .. } => import.abort(err),
			Stream::Aac(stream) => stream.abort(err),
			Stream::Opus(stream) => stream.abort(err),
			Stream::Legacy(stream) => stream.abort(err),
			Stream::PesPacket(stream) => stream.abort(err),
			Stream::Clock | Stream::Ignored => {}
		}
	}

	/// This route's track and the frame sync it has lost (only the self-describing audio
	/// codecs scan for it), or `None` for a PID that is dropped rather than carried.
	fn stats(&self) -> Option<stats::Stream> {
		Some(match self {
			Stream::Aac(stream) => stream.resync.stats(),
			Stream::Legacy(stream) => stream.resync.stats(),
			Stream::H264 { .. } => stats::Stream::new(".avc3", stats::Class::Video),
			Stream::H265 { .. } => stats::Stream::new(".hev1", stats::Class::Video),
			Stream::Opus(_) => stats::Stream::new(".opus", stats::Class::Audio),
			Stream::PesPacket(stream) => match mpeg_video(stream.stream_type) {
				true => stats::Stream::new(".ts", stats::Class::Video),
				false => stats::Stream::new(".ts", stats::Class::Data),
			},
			Stream::Clock => stats::Stream::new("", stats::Class::Video),
			Stream::Ignored => return None,
		})
	}

	/// The MoQ track name this route publishes, decoded or verbatim, once its track exists.
	fn track_name(&self) -> Option<String> {
		match self {
			Stream::PesPacket(stream) => Some(stream.entry.name.clone()),
			stream => stream.media_track_name(),
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
			Stream::PesPacket(_) | Stream::Clock | Stream::Ignored => None,
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
	/// Elementary stream PID, for the log line and the [`stats::Snapshot`] key.
	pid: u16,
	/// Bytes discarded since the last frame was emitted.
	discarded: usize,
	/// Whether the next frame comes from a scan rather than the previous frame's end, and
	/// so has to be confirmed before it can be published.
	unconfirmed: bool,
	/// End of stream: nothing more can arrive to confirm anything, so publish what parses
	/// rather than drop a frame that is whole.
	draining: bool,
	/// Cumulative counters, published through [`Import::stats`]. Kept beside the scanner
	/// because it is the only thing that knows a scan happened; `discarded` above is the
	/// in-progress scan and is folded in here once it is spent.
	stats: stats::Stream,
}

impl Resync {
	fn new(pid: u16, track: &'static str) -> Self {
		Self {
			pid,
			discarded: 0,
			// A stream starts unconfirmed for the same reason a scan does: nothing has
			// vouched for the boundary yet. A capture joins mid-stream, so the first PES
			// routed to a PID can open in the middle of a frame, and a chance header there
			// would publish payload as audio and take the track's sample rate and channel
			// count from it for the life of the broadcast.
			unconfirmed: true,
			draining: false,
			stats: stats::Stream::new(track, stats::Class::Audio),
		}
	}

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
	///
	/// `unvouched` says nothing confirmed this frame's boundary, which only happens while
	/// draining at end of stream. It is the one case where recovery substitutes audio
	/// instead of leaving a gap, so it is counted apart from a resync.
	fn published(&mut self, unvouched: bool) {
		if self.discarded > 0 {
			self.stats.discarded += self.discarded as u64;
			// Only a confirmed frame proves that the stream regained sync. The EOF drain
			// can publish an unvouched candidate, but that is a substitution rather than a
			// completed resync.
			if !unvouched {
				self.stats.resyncs += 1;
				tracing::warn!(
					pid = self.pid,
					track = self.stats.track,
					discarded = self.discarded,
					resyncs = self.stats.resyncs,
					"audio stream lost frame sync and resynced"
				);
			}
		}
		self.stats.unconfirmed += u64::from(unvouched);
		self.discarded = 0;
		self.unconfirmed = false;
	}

	/// The counters an operator alarms on. See [`stats::Snapshot`].
	fn stats(&self) -> stats::Stream {
		let mut stats = self.stats.clone();
		stats.discarded += self.discarded as u64;
		stats
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
		// A scan in flight is abandoned rather than completed, so the bytes it discarded
		// still count while the resync it belongs to does not.
		self.stats.discarded += self.discarded as u64;
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
	import: Option<aac::Import>,
	/// The AudioSpecificConfig `import` was built with. A program config element in a later frame
	/// must rebuild it exactly.
	asc: bytes::Bytes,
	/// The ADTS object type, sample rate, and channel configuration `import` was built with. A
	/// frame header that changes them starts a new track.
	format: (u8, u32, u8),
	broadcast: moq_net::broadcast::Producer,
	/// Reservation held from the PMT until the first frame builds the importer, so the catalog stays
	/// withheld until this deferred rendition resolves (config comes from the first ADTS header).
	/// Consumed when `import` is built, or released by a frame that cannot build it.
	reserved: Option<crate::catalog::Reserved<E>>,
	/// Reserves the rendition anew on the program's timebase when `import` is built after `reserved`
	/// was released, or for the next track after a config change.
	timebase: crate::catalog::Timebase<E>,
	/// The container this importer publishes decoded renditions with.
	container: hang::catalog::Container,
	/// Partial frame left at the end of the previous PES. ISO 13818-1 doesn't require
	/// audio frames to align with PES boundaries, so a legitimate mux can split one.
	tail: Vec<u8>,
	/// PTS for the frame the tail begins, computed when it was cut. The PES PTS only
	/// covers frames that begin in that PES.
	tail_pts: Option<Timestamp>,
	resync: Resync,
	// Completed audio duration since the last video PES, measured independently for each PID.
	burst: std::time::Duration,
}

impl<E: CatalogExt> AacStream<E> {
	fn write(&mut self, pending: Pending, batched: bool) -> anyhow::Result<u64> {
		let pes_base = on_clock(pending.pts, pending.offset)?;

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
		let mut burst = std::time::Duration::ZERO;
		let mut published = 0;
		let mut offset = 0;
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
			// Nothing vouches for a frame found by a scan or joined onto a carried tail, so
			// publishing one without confirming it (which only end of stream does) substitutes
			// audio rather than leaving a gap. Sampled here, before the publish clears it.
			let unvouched = !confirm && (self.resync.unconfirmed() || in_tail);
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
					// Take the PES PTS only where the tail is actually abandoned: restoring a fallback
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

			let mut block = &data[offset + header.header_len..end];
			let format = (header.object_type, header.sample_rate, header.channel_config);
			// A new object type, sample rate, or channel configuration is a new decoder identity:
			// end this track, and publish the rest under a new name.
			if let Some(import) = &mut self.import
				&& format != self.format
			{
				tracing::info!(
					name = import.name(),
					"AAC format changed mid-stream, starting a new track"
				);
				import.finish()?;
				let name = import.name().to_string();
				// The name is never published again, so its HLS records end once its recorder is gone.
				self.import = None;
				self.timebase.retire(&name)?;
			}
			let import = match &mut self.import {
				Some(import) => {
					// The description already carries the layout, so a repeated program config
					// element (the TS export writes one after each PAT/PMT) leaves the frame too.
					if header.channel_config == 0 {
						let mut rest = block;
						match aac::in_band_config(header.object_type, header.sample_rate, 0, &mut rest) {
							Ok(asc) if asc == self.asc => block = rest,
							Ok(_) => {
								return Err(
									Damaged(anyhow::anyhow!("AAC program config element changed mid-stream")).into(),
								);
							}
							Err(aac::Error::ProgramConfigMissing) => {}
							Err(err) => return Err(unit_error(err.into())),
						}
					}
					import
				}
				None => {
					// Synthesize the AudioSpecificConfig `description` so out-of-band consumers
					// (fMP4/MKV export, WebCodecs) can configure the decoder. A channel_config of 0
					// moves the program config element out of this first frame into it, as
					// ffmpeg's aac_adtstoasc does; the TS export puts it back.
					let asc = aac::in_band_config(
						header.object_type,
						header.sample_rate,
						header.channel_config,
						&mut block,
					)
					.map_err(|err| {
						// ffmpeg writes the element in its first frame only, so a receiver that
						// joined later may never see one. Stop withholding the catalog for this
						// track; it joins the catalog if an element arrives.
						self.reserved.take();
						unit_error(err.into())
					})?;
					let mut config = aac::config(&asc)?;
					config.container = self.container.clone();
					// Consume the reservation held since the PMT: this resolves the gated rendition,
					// and carries the catalog's declared media retention onto the track.
					let reserved = self.reserved.take().unwrap_or_else(|| self.timebase.reserve());
					let track = self
						.broadcast
						.unique_track(".aac", reserved.track_info(hang::catalog::PRIORITY.audio))?;
					let aac = aac::Import::new(track, reserved, config)?;
					self.asc = asc;
					self.format = format;
					self.import.insert(aac)
				}
			};

			import.decode(block, pts)?;
			// Count only completed frames; input gaps and unfinished tails are not a media burst.
			burst += std::time::Duration::from_nanos((1024_u64 * 1_000_000_000).div_ceil(header.sample_rate as u64));
			// The importer accumulates; cut each ADTS frame into its own group (one QUIC stream)
			// so the relay forwards it without waiting for the next.
			import.cut(None)?;
			self.resync.published(unvouched);
			published += 1;
			// Offsets behind the published frame are spent; carrying one would republish it.
			fallback = None;

			// Every ADTS frame is 1024 samples.
			pts = advance_pts(pts, 1024, header.sample_rate)?;
			offset = end;
		}

		if let Some(import) = &mut self.import {
			self.burst = if batched { self.burst + burst } else { burst };
			import.burst(self.burst)?;
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

		Ok(published)
	}

	fn discontinuity(&mut self, published: bool) -> anyhow::Result<()> {
		self.desync();
		if published && let Some(import) = &mut self.import {
			import.discontinuity()?;
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
		self.burst = std::time::Duration::ZERO;
		self.tail.clear();
		self.tail_pts = None;
		self.resync.desynced();
	}

	fn finish(&mut self) -> anyhow::Result<u64> {
		// Drain a frame held only for want of a successor to confirm it: at end of stream
		// that successor is never coming. Only once this stream has published a frame,
		// though. Before that nothing has vouched for any boundary, so accepting one here
		// would hand a capture that joined mid-frame and ended immediately the same false
		// frame that starting unconfirmed exists to reject, and build the track's config out
		// of it.
		let mut drained = 0;
		if !self.tail.is_empty() && self.import.is_some() {
			self.resync.drain();
			// No PTS to translate, so no mapping needed.
			drained = self.write(Pending::empty(), true)?;
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
		Ok(drained)
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
struct OpusStream {
	import: opus::Import,
}

impl OpusStream {
	fn write(&mut self, pending: Pending) -> anyhow::Result<u64> {
		let base = on_clock(pending.pts, pending.offset)?;

		let packets = opus_packets(&pending.data).map_err(Damaged)?;
		let mut published = 0;
		// 48 kHz samples elapsed since this PES's PTS, advancing each packet after the first.
		let mut elapsed: u64 = 0;
		for packet in packets {
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
			published += 1;
		}
		Ok(published)
	}

	fn discontinuity(&mut self, published: bool) -> anyhow::Result<()> {
		if published {
			self.import.discontinuity()?;
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
fn registration_format(descriptors: &[catalog::Descriptor]) -> Option<[u8; 4]> {
	descriptors
		.iter()
		.find(|d| d.tag == 0x05)
		.and_then(|d| d.data.get(..4))
		.and_then(|s| s.try_into().ok())
}

/// The OpusHead implied by the DVB extension descriptor (tag 0x7f, ext tag 0x80).
///
/// Codes 0x00..=0x08 follow the plain Opus-in-TS table (and ffmpeg's demuxer): 0 is dual
/// mono read as one stereo stream, and 1..=8 is that many channels, family 0 up to stereo
/// and the Vorbis family 1 mapping above it. The packet shape of code 0 is one coupled
/// stream either way, so this keeps the family 0 head those streams already imported with.
///
/// 0x80 and 0x82..=0x88 are the other named layouts in Table 4-3 of the Opus-in-TS draft
/// (Xiph's "ETSI TS opus" v0.1.3, never published by ETSI): two
/// independent mono streams, and the uncoupled family 1 tables ffmpeg writes as
/// `0x80 | channels` and then does not read back. 0x81 carries the layout in the
/// descriptor (channel count, mapping family, then the stream counts and table,
/// bit-packed). A reserved code, or an explicit layout that does not parse, refuses this
/// stream. A stream with no extension descriptor stays stereo.
fn opus_config(descriptors: &[catalog::Descriptor]) -> anyhow::Result<opus::Config> {
	let Some(data) = descriptors
		.iter()
		.find(|d| d.tag == 0x7f && d.data.first() == Some(&0x80))
		.map(|d| d.data.as_ref())
	else {
		return Ok(opus::Config::new(48_000, 2));
	};
	let Some(&code) = data.get(1) else {
		anyhow::bail!("Opus extension descriptor has no channel_config_code");
	};

	match code {
		0 => Ok(opus::Config::new(48_000, 2)),
		1..=2 => Ok(opus::Config::new(48_000, u32::from(code))),
		3..=8 => vorbis(code),
		0x80 => {
			anyhow::ensure!(data.len() == 2, "trailing bytes after Opus channel_config_code 0x80");
			mapped(2, 255, 2, 0, &[0, 1])
		}
		0x81 => explicit_layout(&data[2..]),
		0x82..=0x88 => {
			anyhow::ensure!(
				data.len() == 2,
				"trailing bytes after Opus channel_config_code 0x{code:02x}"
			);
			let channels = code - 0x80;
			const IDENTITY: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
			mapped(u32::from(channels), 1, channels, 0, &IDENTITY[..channels as usize])
		}
		_ => anyhow::bail!("reserved Opus channel_config_code 0x{code:02x}"),
	}
}

/// Family 1 Vorbis mapping for a plain channel count of 3..=8.
fn vorbis(channels: u8) -> anyhow::Result<opus::Config> {
	let mut config = opus::Config::new(48_000, u32::from(channels));
	config.mapping = Some(opus::Mapping::vorbis(channels)?);
	Ok(config)
}

/// A mapping table checked by [`opus::Mapping::new`].
fn mapped(channels: u32, family: u8, streams: u8, coupled: u8, table: &[u8]) -> anyhow::Result<opus::Config> {
	let mut config = opus::Config::new(48_000, channels);
	config.mapping = Some(opus::Mapping::new(opus::mapping::Config {
		family,
		streams,
		coupled,
		table,
	})?);
	Ok(config)
}

/// The bit-packed layout after `channel_config_code` 0x81 (Opus-in-TS draft Table 4-2).
///
/// `channel_count` and `mapping_family` are bytes. Family 0 stops there. Otherwise
/// `stream_count - 1`, `coupled_stream_count`, and each `channel_mapping` entry follow
/// at `ceil(log2(...))` bits, MSB first, then zero pad to a byte. The all-ones mapping
/// value is silence, which an OpusHead stores as 255.
///
/// gstreamer sizes each field with `g_bit_storage(n)`, one bit wider than the draft
/// whenever `n` is a power of two. Those descriptors fail the trailing or pad check and
/// are refused. Retrying with gstreamer's widths could misread a descriptor written to
/// the draft, so there is no fallback.
fn explicit_layout(data: &[u8]) -> anyhow::Result<opus::Config> {
	let (&channel_count, rest) = data.split_first().context("truncated Opus channel configuration")?;
	let (&family, rest) = rest.split_first().context("truncated Opus channel configuration")?;
	anyhow::ensure!(channel_count > 0, "Opus channel_count is zero");
	if family == 0 {
		anyhow::ensure!(
			rest.is_empty(),
			"trailing bytes after a family 0 Opus channel configuration"
		);
		anyhow::ensure!(
			(1..=2).contains(&channel_count),
			"channel mapping family 0 does not allow {channel_count} channels"
		);
		return Ok(opus::Config::new(48_000, u32::from(channel_count)));
	}

	let mut bits = Bits::new(rest);
	let stream_count = bits.read(ceil_log2(u32::from(channel_count)))? + 1;
	anyhow::ensure!(
		stream_count <= u32::from(channel_count),
		"Opus stream_count {stream_count} exceeds channel_count {channel_count}"
	);
	let coupled = bits.read(ceil_log2(stream_count + 1))?;
	anyhow::ensure!(
		coupled <= stream_count,
		"Opus coupled_stream_count {coupled} exceeds stream_count {stream_count}"
	);
	let decoded = stream_count + coupled;
	anyhow::ensure!(
		decoded <= 255,
		"Opus channel configuration has {decoded} coded channels"
	);
	let width = ceil_log2(decoded + 1);
	let silence = (1u32 << width) - 1;
	let mut table = Vec::with_capacity(channel_count as usize);
	for _ in 0..channel_count {
		let entry = bits.read(width)?;
		table.push(if entry == silence { 255 } else { entry as u8 });
	}
	let pad = (8 - (bits.taken % 8)) % 8;
	if pad > 0 {
		let reserved = bits.read(pad as u8)?;
		anyhow::ensure!(reserved == 0, "nonzero reserved bits in Opus channel configuration");
	}
	bits.finish()?;

	mapped(
		u32::from(channel_count),
		family,
		stream_count as u8,
		coupled as u8,
		&table,
	)
}

/// `ceil(log2(n))` for `n >= 1`. `ceil(log2(1))` is 0, a zero-width field.
fn ceil_log2(n: u32) -> u8 {
	debug_assert!(n >= 1);
	(u32::BITS - (n - 1).leading_zeros()) as u8
}

/// MSB-first reader over the bit-packed tail of an explicit Opus channel configuration.
struct Bits<'a> {
	data: &'a [u8],
	index: usize,
	current: u8,
	left: u8,
	/// Bits returned so far, padding included, so the caller can byte-align.
	taken: u32,
}

impl Bits<'_> {
	fn new(data: &[u8]) -> Bits<'_> {
		Bits {
			data,
			index: 0,
			current: 0,
			left: 0,
			taken: 0,
		}
	}

	fn read(&mut self, n: u8) -> anyhow::Result<u32> {
		let mut value = 0u32;
		for _ in 0..n {
			if self.left == 0 {
				self.current = *self
					.data
					.get(self.index)
					.context("truncated Opus channel configuration")?;
				self.index += 1;
				self.left = 8;
			}
			self.left -= 1;
			value = (value << 1) | u32::from((self.current >> self.left) & 1);
		}
		self.taken += u32::from(n);
		Ok(value)
	}

	/// The descriptor ended on the byte the layout consumed, with nothing after it.
	fn finish(self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.left == 0 && self.index == self.data.len(),
			"trailing bytes in Opus channel configuration"
		);
		Ok(())
	}
}

/// Validate the complete PES before exposing any of its declared Opus units.
fn opus_packets(mut data: &[u8]) -> anyhow::Result<Vec<&[u8]>> {
	let mut packets = Vec::new();
	while !data.is_empty() {
		let (header_len, size) = parse_opus_control_header(data)?;
		let end = header_len + size;
		let packet = data
			.get(header_len..end)
			.context("Opus access unit exceeds PES payload")?;
		packets.push(packet);
		data = &data[end..];
	}
	Ok(packets)
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
/// (the config isn't in the PMT).
struct LegacyStream<E: CatalogExt = ()> {
	descriptor: &'static legacy::Descriptor,
	import: Option<legacy::Import>,
	broadcast: moq_net::broadcast::Producer,
	/// Reservation held from the PMT until the first frame builds the importer, so the catalog stays
	/// withheld until this deferred rendition resolves (config comes from the first frame header).
	/// Consumed when `import` is built.
	reserved: Option<crate::catalog::Reserved<E>>,
	/// Reserves the next track on the program's timebase after a config change.
	timebase: crate::catalog::Timebase<E>,
	/// The sample rate and channel count `import` was built with, from the first frame header.
	format: (u32, u32),
	/// The container this importer publishes decoded renditions with.
	container: hang::catalog::Container,
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
	fn write(&mut self, pending: Pending) -> anyhow::Result<u64> {
		let mut published = 0;
		let pes_base = on_clock(pending.pts, pending.offset)?;

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
			// Nothing vouches for a frame found by a scan or joined onto a carried tail, so
			// publishing one without confirming it (which only end of stream does) substitutes
			// audio rather than leaving a gap. Sampled here, before the publish clears it.
			let unvouched = !confirm && (self.resync.unconfirmed() || in_tail);
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

			let format = (header.sample_rate, header.channel_count);
			// A new sample rate or channel count is a new decoder identity: end this track, and
			// publish the rest under a new name.
			if let Some(import) = &mut self.import
				&& format != self.format
			{
				tracing::info!(
					name = import.name(),
					"audio format changed mid-stream, starting a new track"
				);
				import.finish()?;
				let name = import.name().to_string();
				// The name is never published again, so its HLS records end once its recorder is gone.
				self.import = None;
				self.timebase.retire(&name)?;
			}
			let import = match &mut self.import {
				Some(import) => import,
				None => {
					let config = legacy::Config {
						sample_rate: header.sample_rate,
						channel_count: header.channel_count,
						container: self.container.clone(),
					};
					// Consume the reservation held since the PMT: this resolves the gated rendition,
					// and carries the catalog's declared media retention onto the track. A track
					// after a format change reserves anew on the same timebase.
					let reserved = self.reserved.take().unwrap_or_else(|| self.timebase.reserve());
					let track = self.broadcast.unique_track(
						self.descriptor.track_suffix,
						reserved.track_info(hang::catalog::PRIORITY.audio),
					)?;
					let legacy = legacy::Import::new(self.descriptor, track, reserved, config)?;
					self.format = format;
					self.import.insert(legacy)
				}
			};

			import.decode(&data[offset..end], pts)?;
			// The importer accumulates; cut each frame into its own group (one QUIC stream)
			// so the relay forwards it without waiting for the next.
			import.cut(None)?;
			self.resync.published(unvouched);
			published += 1;
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

		Ok(published)
	}

	fn discontinuity(&mut self, published: bool) -> anyhow::Result<()> {
		self.desync();
		if published && let Some(import) = &mut self.import {
			import.discontinuity()?;
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

	fn finish(&mut self) -> anyhow::Result<u64> {
		// Drain a frame held only for want of a successor to confirm it: at end of stream
		// that successor is never coming. Only once this stream has published a frame,
		// though. Before that nothing has vouched for any boundary, so accepting one here
		// would hand a capture that joined mid-frame and ended immediately the same false
		// frame that starting unconfirmed exists to reject, and build the track's config out
		// of it.
		let mut drained = 0;
		if !self.tail.is_empty() && self.import.is_some() {
			self.resync.drain();
			// No PTS to translate, so no mapping needed.
			drained = self.write(Pending::empty())?;
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
		Ok(drained)
	}

	fn abort(mut self, err: moq_net::Error) {
		if let Some(import) = self.import.take() {
			import.abort(err);
		}
	}
}

/// Swallow a [`MissingKeyframe`](crate::container::MissingKeyframe) from a video
/// decode: a TS capture can join mid-GOP, so the deltas before the first keyframe
/// have no group to anchor and are simply dropped rather than aborting the demux.
fn skip_missing_keyframe(result: crate::Result<()>) -> anyhow::Result<bool> {
	match result {
		Ok(()) => Ok(true),
		Err(crate::Error::MissingKeyframe(_)) => Ok(false),
		Err(e) => Err(unit_error(e)),
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

/// Tracks the wrap-around of the 33-bit, 90 kHz PTS field so timestamps stay
/// monotonic across the ~26.5 hour wrap period.
///
/// A flagged clock break keeps the history: a forward jump across the wrap lands in the next
/// one, as the passthrough's PCR count does, while a step back of under half a wrap still reads
/// as the rewind it is. That holds before the first wrap too: a stream still behind a wrap the
/// first PTS already crossed (an import started while audio and video straddle it) reads as
/// negative, before the broadcast clock's zero, rather than a whole wrap ahead.
#[derive(Default)]
struct PtsUnwrap {
	last: Option<u64>,
	offset: i64,
}

impl PtsUnwrap {
	fn unwrap(&mut self, raw: u64) -> i64 {
		const WRAP: i64 = 1 << 33;
		if let Some(last) = self.last {
			let diff = raw as i64 - last as i64;
			if diff < -WRAP / 2 {
				self.offset += WRAP;
			} else if diff > WRAP / 2 {
				self.offset -= WRAP;
			}
		}
		self.last = Some(raw);
		self.offset + raw as i64
	}

	/// `raw`, a 27 MHz PCR, in the wrap nearest the last PTS, or in the current one before any:
	/// signed, since a PCR just behind a wrap the PTS already crossed lands in the one before.
	fn pcr(&self, raw: u64) -> i128 {
		let period = i128::from(PCR_WRAP);
		let reference = (i128::from(self.offset) + i128::from(self.last.unwrap_or(raw / 300))) * 300;
		let raw = i128::from(raw);
		raw + (reference - raw + period / 2).div_euclid(period) * period
	}
}

/// A 90 kHz PTS already in its era, as a [`Timestamp`] on the catalog clock: shifted by the
/// timebase's `offset`. Returns `None` when the PES carried no PTS.
fn on_clock(pts: Option<u64>, offset: Offset) -> anyhow::Result<Option<Timestamp>> {
	let Some(pts) = pts else {
		return Ok(None);
	};
	Ok(Some(offset.apply(Timestamp::from_scale(pts, 90_000)?)?))
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

#[cfg(test)]
pub(super) mod test {
	use std::collections::BTreeMap;
	use std::time::Duration;

	use moq_net::Timestamp;

	use mpeg2ts::es::StreamType;

	use super::{Continuation, Continuity, SectionReassembler, Stream};
	use mpeg2ts::ts::{Pid, TsPacket};

	/// A drift budget no test timeline comes close to, so the reader sees every group.
	///
	/// The media track's full retention window, so a reader started after importing can
	/// still read every retained group. These tests import a whole file first, which the default
	/// [`Duration::ZERO`] budget collapses to the live
	/// edge: completeness has to be asked for.
	const RECORDING_MAX_AGE: std::time::Duration = Duration::from_secs(30);

	// libklvanc public-sample cue: table_id 0xFC, section_length 0x1b (27), 30 bytes total.
	pub(in crate::container::ts) const CUE: [u8; 30] = [
		0xfc, 0x30, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf0, 0x0a, 0x05, 0x00, 0x00, 0x2b, 0xb4,
		0x7f, 0xdf, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xad, 0x25, 0xe8, 0x39,
	];

	#[test]
	fn resync_stats_include_an_in_progress_scan() {
		let mut resync = super::Resync::new(0x61, ".mp2");
		let codec = super::SyncWord {
			min_header_len: 4,
			sync_byte: 0xff,
		};

		assert!(matches!(resync.recover(&[0; 10], 0, &codec), super::Recover::Carry(7)));
		let stats = resync.stats();
		assert_eq!(stats.discarded, 7, "the active scan must be visible before recovery");
		assert_eq!(stats.resyncs, 0, "the stream has not regained sync yet");
	}

	#[test]
	fn liveness_steps_over_a_corrupt_pcr_and_the_wrap() {
		const MS: u64 = 27_000;
		let mut liveness = super::Liveness::default();
		liveness.register(0x100);
		let mut pcr = super::super::mux_rate::PCR_WRAP - 100 * MS;
		for _ in 0..10 {
			liveness.pcr(pcr);
			pcr = (pcr + 40 * MS) % super::super::mux_rate::PCR_WRAP;
		}
		assert_eq!(liveness.stream(0x100), (0, Some(Duration::from_millis(360))));

		// A PCR hours ahead, then the clock carrying on where it was: neither step counts.
		liveness.pcr(pcr + 23_861_000 * MS);
		liveness.pcr(pcr);
		assert_eq!(liveness.stream(0x100).1, Some(Duration::from_millis(360)));

		liveness.pcr(pcr + 40 * MS);
		liveness.delivered(0x100, 3);
		liveness.pcr(pcr + 80 * MS);
		assert_eq!(liveness.stream(0x100), (3, Some(Duration::from_millis(40))));
	}

	#[test]
	fn resync_does_not_count_an_unvouched_eof_publication() {
		let mut resync = super::Resync::new(0x61, ".mp2");
		let codec = super::SyncWord {
			min_header_len: 4,
			sync_byte: 0xff,
		};

		resync.recover(&[0; 10], 0, &codec);
		resync.drain();
		resync.published(true);
		let stats = resync.stats();
		assert_eq!(stats.discarded, 7, "the bytes skipped before EOF were discarded");
		assert_eq!(
			stats.resyncs, 0,
			"an unconfirmed frame does not prove sync was regained"
		);
		assert_eq!(stats.unconfirmed, 1, "the unvouched publication is counted separately");
	}

	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn damaged_unit_warnings_are_aggregated_per_pid() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pid = Pid::new(0x61).unwrap();
		let peer = Pid::new(0x62).unwrap();
		let error = anyhow::anyhow!("broken PES");
		for _ in 0..3 {
			import.damage(pid, &error).unwrap();
		}
		import.damage(peer, &error).unwrap();
		logs_assert(|lines| {
			let first = lines
				.iter()
				.filter(|line| line.contains("dropped a damaged TS unit"))
				.count();
			if first == 2 {
				Ok(())
			} else {
				Err(format!("expected one warning per PID, got {first}"))
			}
		});

		tokio::time::advance(super::stats::Log::INTERVAL).await;
		import.decode(&[]).unwrap();
		logs_assert(|lines| {
			let summary: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("TS damaged-unit summary"))
				.collect();
			if summary.len() == 1 && summary[0].contains("pid=97 ") && summary[0].contains("count=2") {
				Ok(())
			} else {
				Err(format!("expected two additional drops on PID 97, got {summary:?}"))
			}
		});
		import.damage(pid, &error).unwrap();
		import.damage(pid, &error).unwrap();
		import.finish().unwrap();
		assert_eq!(import.stats().streams[&0x61].damaged, 5);
		drop(import);
		logs_assert(|lines| {
			let first = lines
				.iter()
				.filter(|line| line.contains("dropped a damaged TS unit"))
				.count();
			let summary = lines
				.iter()
				.filter(|line| line.contains("TS damaged-unit summary"))
				.count();
			if (first, summary) == (3, 2) {
				Ok(())
			} else {
				Err(format!("expected new burst and final summary, got {first}/{summary}"))
			}
		});
	}

	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn abort_summarizes_the_final_damage_burst() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pid = Pid::new(0x61).unwrap();
		let error = anyhow::anyhow!("broken PES");
		for _ in 0..3 {
			import.damage(pid, &error).unwrap();
		}
		import.abort(moq_net::Error::Cancel);
		logs_assert(|lines| {
			let summary: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("TS damaged-unit summary"))
				.collect();
			if summary.len() == 1 && summary[0].contains("count=2") {
				Ok(())
			} else {
				Err(format!("expected the aborted burst's two drops, got {summary:?}"))
			}
		});
	}

	#[test]
	fn remapping_a_pid_keeps_its_retired_resync_stats() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pid = mpeg2ts::ts::Pid::new(0x61).unwrap();
		let mut stream = import.legacy_stream(pid, &super::mp2::DESCRIPTOR);
		let super::Stream::Legacy(legacy) = &mut stream else {
			unreachable!();
		};
		let codec = super::SyncWord::from(&super::mp2::DESCRIPTOR);
		legacy.resync.recover(&[0; 10], 0, &codec);
		legacy.resync.published(false);
		import.streams.insert(pid, stream);
		import.damage(pid, &anyhow::anyhow!("malformed PES")).unwrap();
		let before = import.stats();
		assert_eq!(before.streams[&pid.as_u16()].damaged, 1);

		import
			.ensure_section(pid, StreamType::Dts8ChannelLosslessAudio as u8, &[])
			.unwrap();

		assert!(matches!(import.streams.get(&pid), Some(super::Stream::Ignored)));
		// A base catalog cannot carry the section route, which counts as refused.
		let mut expected = before;
		expected.streams.get_mut(&pid.as_u16()).unwrap().refused = 1;
		assert_eq!(import.stats(), expected, "a PMT remap must not reset lifetime counters");
	}

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
	///
	/// The clock rides the first elementary stream, as a real mux puts it on the video.
	fn synth_pmt(es: &[(StreamType, u16)], cuei: bool) -> Vec<u8> {
		synth_programs(&[(1, 0x0100, es)], cuei)
	}

	/// A program to synthesize: `(program_num, pmt_pid, [(stream_type, pid)])`.
	type SynthProgram<'a> = (u16, u16, &'a [(StreamType, u16)]);

	/// Serialize one PAT listing every program, then each program's PMT, as [`synth_pmt`]
	/// does for one.
	fn synth_programs(programs: &[SynthProgram], cuei: bool) -> Vec<u8> {
		use mpeg2ts::ts::payload::{Pat, Pmt};
		use mpeg2ts::ts::{
			ContinuityCounter, Descriptor, EsInfo, Pid, ProgramAssociation, TransportScramblingControl, TsHeader,
			TsPacket, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
		};

		let pat = Pat {
			transport_stream_id: 1,
			version_number: VersionNumber::default(),
			table: programs
				.iter()
				.map(|&(program_num, pmt_pid, _)| ProgramAssociation {
					program_num,
					program_map_pid: Pid::new(pmt_pid).unwrap(),
				})
				.collect(),
		};
		let pmt = |program_num: u16, es: &[(StreamType, u16)]| Pmt {
			program_num,
			pcr_pid: es.first().map(|&(_, pid)| Pid::new(pid).unwrap()),
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
		for &(program_num, pmt_pid, es) in programs {
			write(&mut out, pmt_pid, TsPayload::Pmt(pmt(program_num, es)));
		}
		out
	}

	// An extended catalog detects the CUEI PID, advertises a cue track, and the
	// section is published (a `Catalog<catalog::Ext>` carries the rendition).
	#[test]
	fn scte35_extension_catalogs_the_cue_track() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, PCR_PID),
				(StreamType::Dts8ChannelLosslessAudio, 0x21),
			],
			true,
		));
		bytes.extend_from_slice(&pcr_packet(PCR_PID, 24_300_000));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&pes_packet(PCR_PID, 90_000));
		bytes.extend_from_slice(&pcr_packet(PCR_PID, 27_000_000));
		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		assert_eq!(
			cue_tracks(&catalog.snapshot().ext.mpegts),
			1,
			"expected one scte35 rendition"
		);
	}

	// An application extension composing the `mpegts` section counts its verbatim tracks as
	// servable without implementing anything beyond `ts::Catalog`, so an input whose only track
	// is a cue announces.
	#[tokio::test(start_paused = true)]
	async fn a_composed_extension_publishes_a_cue_only_catalog() {
		use crate::catalog::hang::{Catalog, CatalogExt};
		use crate::container::ts::catalog::{Catalog as TsCatalog, Mpegts};

		#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
		struct Composed {
			#[serde(default, skip_serializing_if = "Mpegts::is_empty")]
			mpegts: Mpegts,
			#[serde(default, skip_serializing_if = "Option::is_none")]
			app: Option<u32>,
		}
		impl CatalogExt for Composed {}
		impl TsCatalog for Composed {
			fn mpegts_mut(&mut self) -> Option<&mut Mpegts> {
				Some(&mut self.mpegts)
			}
		}

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Composed>::default()),
		)
		.unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, PCR_PID),
				(StreamType::Dts8ChannelLosslessAudio, 0x21),
			],
			true,
		));
		bytes.extend_from_slice(&pcr_packet(PCR_PID, 24_300_000));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&pes_packet(PCR_PID, 90_000));
		bytes.extend_from_slice(&pcr_packet(PCR_PID, 27_000_000));
		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		let first = tokio::time::timeout(Duration::from_secs(10), updates.next())
			.await
			.expect("the cue track publishes the initial catalog")
			.unwrap()
			.unwrap();
		assert!(
			first.video.renditions.is_empty() && first.audio.renditions.is_empty(),
			"only the extension lists a track"
		);
		assert_eq!(cue_tracks(&first.ext.mpegts), 1, "the cue track is listed");
	}

	// The base catalog (`Catalog<()>`) can't carry cues, so a detected CUEI PID routes to
	// Stream::Ignored: dropped before the reader (no abort), with no ScteStream created, so
	// the publishing lock is never taken and the catalog is never republished empty.
	#[tokio::test(start_paused = true)]
	async fn base_catalog_routes_cue_pid_to_ignored() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		// First PMT lacks CUEI: the 0x86 PID is ambiguous and routes to Ignored.
		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, PCR_PID),
				(StreamType::Dts8ChannelLosslessAudio, SECTION_PID),
			],
			false,
		));
		import.decode(&bytes).unwrap();
		assert!(
			matches!(import.streams.get(&pid), Some(super::Stream::Ignored)),
			"pre-CUEI PMT routes the PID to Ignored"
		);

		// Second PMT carries CUEI: upgrade to a cue track, then a section on the same PID.
		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, PCR_PID),
				(StreamType::Dts8ChannelLosslessAudio, SECTION_PID),
			],
			true,
		));
		bytes.extend_from_slice(&pcr_packet(PCR_PID, 24_300_000));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&pes_packet(PCR_PID, 90_000));
		bytes.extend_from_slice(&pcr_packet(PCR_PID, 27_000_000));
		import.decode(&bytes).unwrap();

		assert!(
			!import.streams.contains_key(&pid),
			"upgrade drops the stale Ignored route"
		);
		assert_eq!(
			cue_tracks(&catalog.snapshot().ext.mpegts),
			1,
			"upgrade advertises the cue track"
		);

		// The importer clears its verbatim entries from the catalog when it drops, so read the
		// track name while it is still registered.
		let name = track_on(&catalog.snapshot().ext.mpegts, SECTION_PID);
		import.finish().unwrap();
		let track = consumer
			.track(&name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy(crate::container::Kind::Data));
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read_frame())
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

	/// A CUEI-marked 0x86 section is data, not audio, so a quiet second is not logged.
	/// The video PID beside it, stalled with the same frozen count, is.
	#[test]
	#[tracing_test::traced_test]
	fn sparse_cuei_pid_beside_a_stalled_video_is_not_logged() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		// Not 0x100: `synth_pmt` puts the PMT there, and a later packet on that PID is PSI.
		const VIDEO: u16 = 0x110;
		const CUE_PID: u16 = 0x21;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, VIDEO),
				(StreamType::Dts8ChannelLosslessAudio, CUE_PID),
			],
			true,
		));
		bytes.extend_from_slice(&pcr_packet(VIDEO, 24_300_000));
		// A sequence header, so the picture opens a group rather than waiting for one.
		bytes.extend_from_slice(&video_pes(VIDEO, 0, 90_000, None, &[0, 0, 1, 0xb3, 0, 0, 1, 0]));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&pcr_packet(VIDEO, 27_000_000));
		import.decode(&bytes).unwrap();

		let stats = import.stats();
		assert_eq!(stats.streams[&VIDEO].class, super::stats::Class::Video);
		assert_eq!(stats.streams[&VIDEO].track, ".ts");
		assert!(
			stats.streams[&VIDEO].units >= 1,
			"the video PID delivered before it stalled"
		);
		assert_eq!(stats.streams[&CUE_PID].class, super::stats::Class::Data, "{stats:?}");
		assert_eq!(stats.streams[&CUE_PID].track, ".ts");
		assert!(stats.streams[&CUE_PID].units >= 1, "the cue section was counted");

		let mut log = crate::container::ts::stats::Log::default();
		log.sample(stats);
		log.sample(import.stats());

		logs_assert(|lines: &[&str]| {
			let stopped: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("stopped delivering access units"))
				.collect();
			match stopped.as_slice() {
				[line] if line.contains("pid=272 ") && !line.contains("pid=33 ") => Ok(()),
				_ => Err(format!("expected only the stalled video PID, got {stopped:?}")),
			}
		});
	}

	/// The section tracks the `mpegts` section lists, beside any whole-PES video clocking them.
	fn cue_tracks(mpegts: &super::catalog::Mpegts) -> usize {
		mpegts.tracks.values().filter(|track| track.verbatim.is_some()).count()
	}

	/// The track the `mpegts` section carries `pid` on.
	fn track_on(mpegts: &super::catalog::Mpegts, pid: u16) -> String {
		let found = mpegts.tracks.iter().find(|(_, track)| track.pid == pid);
		found.expect("a track on the PID").0.clone()
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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

	// MPEG-2 video only drives the section clock, so with nothing else to flush it must still
	// anchor the catalog, or cues stamped with its PTS map to the construction-time clock.
	#[test]
	fn a_clock_only_stream_anchors_the_catalog() {
		const VIDEO_PID: u16 = 0x0050;
		const PTS_SECS: u64 = 3600;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		import
			.decode(&synth_pmt(&[(StreamType::Mpeg2Video, VIDEO_PID)], true))
			.unwrap();
		import
			.decode(pes_packet(VIDEO_PID, PTS_SECS * 90_000).as_slice())
			.unwrap();

		let now = catalog.clock().now().as_micros() / 1_000_000;
		assert!(
			(PTS_SECS as u128..PTS_SECS as u128 + 5).contains(&now),
			"catalog clock reads {now}s, not the first video PTS"
		);
	}

	/// A `splice_insert` section splicing at `pts_time`, with no `pts_adjustment` and a valid CRC.
	fn splice_insert(pts_time: u64) -> Vec<u8> {
		// table_id, length (filled below), protocol_version, pts_adjustment, cw_index, tier, and a
		// 15-byte splice_insert command.
		let mut section = vec![0xfc, 0x30, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xf0, 15, 0x05];
		section.extend_from_slice(&1u32.to_be_bytes()); // splice_event_id
		section.push(0x7f); // not cancelled
		section.push(0xcf); // out of network, program splice, no duration, not immediate
		section.push(0xfe | ((pts_time >> 32) as u8 & 1)); // time_specified, then the PTS
		section.extend_from_slice(&(pts_time as u32).to_be_bytes());
		section.extend_from_slice(&[0, 1, 0, 0]); // unique_program_id, avail_num, avails_expected
		section.extend_from_slice(&[0, 0]); // descriptor_loop_length
		section[2] = (section.len() - 3 + 4) as u8;
		let crc = super::psi::CRC.checksum(&section);
		section.extend_from_slice(&crc.to_be_bytes());
		section
	}

	/// A TS import joining a clock already in use shifts its media onto that clock, so its
	/// SCTE-35 sections absorb the same shift in `pts_adjustment`: in the export, a splice time
	/// lands on the exported picture it names, and the section still verifies.
	#[tokio::test(start_paused = true)]
	async fn a_splice_follows_its_media_onto_a_clock_in_use() {
		splice_onto_a_clock_in_use(CueAt::AfterVideo).await;
	}

	/// A cue arriving before any PES waits for the offset the media takes, rather than
	/// publishing unshifted.
	#[tokio::test(start_paused = true)]
	async fn a_startup_splice_follows_its_media_onto_a_clock_in_use() {
		splice_onto_a_clock_in_use(CueAt::BeforeVideo).await;
	}

	/// A non-video PES anchors a cue's placement on the shifted media timeline.
	#[tokio::test(start_paused = true)]
	async fn a_splice_released_before_video_lands_on_the_media_timeline() {
		splice_onto_a_clock_in_use(CueAt::BeforePrivate).await;
	}

	/// A timebase another importer already anchored still holds a cue until this importer has a
	/// timestamp to stamp it with.
	#[tokio::test(start_paused = true)]
	async fn a_splice_on_an_anchored_input_waits_for_its_media() {
		splice_onto_a_clock_in_use(CueAt::BeforeVideoOnAnAnchoredInput).await;
	}

	/// Where the cue arrives relative to the first PES.
	enum CueAt {
		AfterVideo,
		BeforeVideo,
		/// Ahead of a private PES at the first picture's PTS, which anchors the timebase.
		BeforePrivate,
		/// Ahead of the first picture, on a timebase placed before the import starts.
		BeforeVideoOnAnAnchoredInput,
	}

	async fn splice_onto_a_clock_in_use(at: CueAt) {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		const VIDEO_PID: u16 = 0x0050;
		const CUE_PID: u16 = 0x0021;
		const PRIVATE_PID: u16 = 0x0051;
		const MASK: u64 = (1 << 33) - 1;
		// Ten pictures a second from one second in; the cue splices at the sixth.
		let picture = |k: u64| 90_000 + k * 9_000;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		// A capture took the clock, which reads ten seconds: the feed shifts nine seconds later.
		let _capture = catalog.clock();
		let anchored = matches!(at, CueAt::BeforeVideoOnAnAnchoredInput);
		let timebase = catalog.timebase();
		if anchored {
			// The shift then depends on the wall clock, so only its consistency is checked.
			let first = Timestamp::from_scale(picture(0), 90_000).unwrap();
			timebase.place(first, moq_clock::wall()).unwrap();
		}
		let mut import = super::Import::new(broadcast, timebase.reserve());

		let mut bytes = synth_pmt(
			&[
				(StreamType::H264, VIDEO_PID),
				(StreamType::Dts8ChannelLosslessAudio, CUE_PID),
				(StreamType::Mpeg2PacketizedData, PRIVATE_PID),
			],
			true,
		);
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 24_300_000));
		let cue_packet = packet(true, 0, 0, &splice_insert(picture(5)));
		match at {
			CueAt::AfterVideo => {}
			CueAt::BeforeVideo | CueAt::BeforeVideoOnAnAnchoredInput => bytes.extend_from_slice(&cue_packet),
			CueAt::BeforePrivate => {
				bytes.extend_from_slice(&cue_packet);
				bytes.extend_from_slice(&pes_packet(PRIVATE_PID, picture(0)));
			}
		}
		for k in 0..20 {
			bytes.extend_from_slice(&pcr_packet(VIDEO_PID, (81_000 + k * 9_000) * 300));
			bytes.extend_from_slice(&audio_pes_packet(
				VIDEO_PID,
				k as u8,
				picture(k),
				&annexb_au(k % 10 == 0),
			));
			if k == 0 && matches!(at, CueAt::AfterVideo) {
				bytes.extend_from_slice(&cue_packet);
			}
		}
		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		let exporter = crate::container::ts::export_test::open_ts(
			crate::source::announced(&consumer).await,
			crate::catalog::CatalogFormat::Hang,
		)
		.await
		.unwrap();
		let mut exporter = exporter.with_delay(RECORDING_MAX_AGE).with_replay();
		let mut ts = Vec::new();
		while let Ok(Some(frame)) = tokio::time::timeout(RECORDING_MAX_AGE * 3, exporter.next())
			.await
			.map(|r| r.unwrap())
		{
			ts.extend_from_slice(&frame.payload);
		}

		let stamp = |b: &[u8]| {
			u64::from((b[0] >> 1) & 7) << 30
				| u64::from(b[1]) << 22
				| u64::from(b[2] >> 1) << 15
				| u64::from(b[3]) << 7
				| u64::from(b[4] >> 1)
		};
		let mut pictures = Vec::new();
		let mut cue = None;
		for packet in ts.as_chunks::<188>().0 {
			let pid = u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2]);
			if packet[1] & 0x40 == 0 || packet[3] & 0x10 == 0 {
				continue;
			}
			let start = 4 + if packet[3] & 0x20 != 0 {
				usize::from(packet[4]) + 1
			} else {
				0
			};
			let payload = &packet[start..];
			if pid == CUE_PID {
				let section = &payload[1 + usize::from(payload[0])..];
				let len = 3 + (usize::from(section[1] & 0x0f) << 8 | usize::from(section[2]));
				cue = Some(section[..len].to_vec());
			} else if pid != PRIVATE_PID && payload.starts_with(&[0, 0, 1]) && (0xe0..=0xef).contains(&payload[3]) {
				pictures.push(stamp(&payload[9..14]));
			}
		}

		let cue = cue.expect("the cue is exported");
		assert_eq!(super::psi::CRC.checksum(&cue), 0, "the section's CRC verifies");
		let adjustment = u64::from(cue[4] & 1) << 32 | u64::from(u32::from_be_bytes(cue[5..9].try_into().unwrap()));
		let pts_time = u64::from(cue[20] & 1) << 32 | u64::from(u32::from_be_bytes(cue[21..25].try_into().unwrap()));
		assert_eq!(pts_time, picture(5), "the splice time itself is untouched");
		if !anchored {
			assert_eq!(adjustment, 810_000, "the nine seconds the media shifted");
		}
		assert_eq!(pictures.len(), 20, "{pictures:?}");
		// A wall-derived shift isn't a whole number of ticks, so the exported picture may round
		// one tick lower than the section's shift.
		let slack = u64::from(anchored);
		let splice = (pts_time + adjustment) & MASK;
		assert!(
			(pictures[5]..=pictures[5] + slack).contains(&splice),
			"the splice at {splice} lands on the picture it names: {pictures:?}"
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

	#[test]
	fn aac_jitter_accumulates_adjacent_pes_per_pid() {
		const AUDIO: u16 = 0x60;
		const VIDEO: u16 = 0x61;
		const OTHER: u16 = 0x62;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(
				&[
					(StreamType::AdtsAac, AUDIO),
					(StreamType::Mpeg2Video, VIDEO),
					(StreamType::AdtsAac, OTHER),
				],
				false,
			))
			.unwrap();
		let mut frame = super::adts::write_header(2, 44_100, 2, 8).unwrap().to_vec();
		frame.extend_from_slice(&[0; 8]);
		for i in 0..4 {
			import
				.decode(&audio_pes_packet(
					AUDIO,
					i,
					90_000 + u64::from(i) * 4180,
					&frame.repeat(2),
				))
				.unwrap();
		}
		let name = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();
		let jitter = catalog.snapshot().audio.renditions[&name].jitter.unwrap();
		import
			.decode(&audio_pes_packet(OTHER, 0, 900_000, &frame.repeat(2)))
			.unwrap();
		let snapshot = catalog.snapshot();
		let other = snapshot
			.audio
			.renditions
			.iter()
			.find(|(key, _)| *key != &name)
			.unwrap()
			.1;
		assert_eq!(other.jitter.unwrap().as_nanos().div_ceil(1_000_000), 47);
		assert_eq!(jitter.as_nanos().div_ceil(1_000_000), 186);
		import.decode(&pes_packet(VIDEO, 110_000)).unwrap();
		import
			.decode(&audio_pes_packet(AUDIO, 4, 110_000, &frame.repeat(2)))
			.unwrap();
		assert_eq!(catalog.snapshot().audio.renditions[&name].jitter, Some(jitter));
	}

	#[test]
	fn aac_pes_jitter_survives_bitrate_updates() {
		const PID: u16 = 0x60;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(&synth_pmt(&[(StreamType::AdtsAac, PID)], false)).unwrap();
		let mut frame = super::adts::write_header(2, 44_100, 2, 8).unwrap().to_vec();
		frame.extend_from_slice(&[0; 8]);
		import
			.decode(&audio_pes_packet(PID, 0, 90_000, &frame.repeat(7)))
			.unwrap();
		let initial = catalog
			.snapshot()
			.audio
			.renditions
			.values()
			.next()
			.unwrap()
			.jitter
			.unwrap();
		assert_eq!(
			initial.as_nanos().div_ceil(1_000_000),
			163,
			"a seven-frame PES is one burst"
		);
		for i in 0..60 {
			let pts = 90_000 + (7 + i) * 1024 * 90_000 / 44_100;
			import
				.decode(&audio_pes_packet(PID, ((i + 1) % 16) as u8, pts, &frame))
				.unwrap();
		}
		import.finish().unwrap();
		let snapshot = catalog.snapshot();
		let config = snapshot.audio.renditions.values().next().unwrap();
		assert!(config.bitrate.is_some(), "exercise a bitrate refinement");
		assert_eq!(config.jitter, Some(initial));
	}

	#[test]
	fn aac_pes_jitter_counts_completed_split_frames() {
		const PID: u16 = 0x60;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(&synth_pmt(&[(StreamType::AdtsAac, PID)], false)).unwrap();
		let mut frame = super::adts::write_header(2, 44_100, 2, 8).unwrap().to_vec();
		frame.extend_from_slice(&[0; 8]);
		let mut first = frame.repeat(2);
		first.extend_from_slice(&frame[..10]);
		import.decode(&audio_pes_packet(PID, 0, 90_000, &first)).unwrap();
		let mut second = frame[10..].to_vec();
		second.extend_from_slice(&frame.repeat(6));
		// A timestamp jump must not turn time spent waiting for input into publisher jitter.
		import.decode(&audio_pes_packet(PID, 1, 900_000, &second)).unwrap();
		let snapshot = catalog.snapshot();
		let jitter = snapshot.audio.renditions.values().next().unwrap().jitter.unwrap();
		assert_eq!(jitter.as_nanos().div_ceil(1_000_000), 163);
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

	/// ffmpeg packs several ADTS frames into one PES, and the importer cuts every one of them into
	/// its own group in a single synchronous pass. The catalog has to describe that burst, not the
	/// 21 ms frame the estimator sees between writes, and a later bitrate refinement must not walk
	/// it back to one frame.
	#[test]
	fn aac_jitter_is_the_pes_burst_not_one_frame() {
		const AAC_PID: u16 = 0x0060;
		// 1024 samples at 48 kHz, in 90 kHz ticks.
		const FRAME: u64 = 1024 * 90_000 / 48_000;
		const PER_PES: u64 = 7;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false));

		// Ten PES of seven frames: well over a second of media, so the bitrate window closes and
		// republishes the rendition at least once.
		for pes in 0..10u64 {
			let mut payload = Vec::new();
			for frame in 0..PER_PES {
				payload.extend_from_slice(&adts_frame(17, 0xA0 | frame as u8));
			}
			bytes.extend_from_slice(&audio_pes_packet(
				AAC_PID,
				pes as u8 & 0x0f,
				pes * PER_PES * FRAME,
				&payload,
			));
		}

		import.decode(&bytes).unwrap();
		import.finish().unwrap();

		let snap = catalog.snapshot();
		let aac = snap.audio.renditions.values().next().expect("an AAC rendition");
		assert!(aac.bitrate.is_some(), "the bitrate window closed at least once");

		let jitter = aac.jitter.expect("AAC publishes a jitter");
		// One frame is ~21 ms; the burst is six or seven of them (the last frame of a PES waits
		// for the next one to confirm it).
		assert!(
			jitter >= std::time::Duration::from_millis(120),
			"jitter describes one frame, not the burst: {jitter:?}"
		);
		assert!(
			jitter <= std::time::Duration::from_millis(160),
			"jitter grew past one burst: {jitter:?}"
		);
	}

	// MP2/AC-3 flush like any audio PES but don't consume the jitter hint; if one
	// anchored the audio run, an AAC PID in the same TS would publish a jitter
	// inflated by the inter-PID PTS offset.
	#[test]
	fn verbatim_audio_does_not_anchor_aac_jitter() {
		const AAC_PID: u16 = 0x0060;
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
	async fn read_audio_frames<E: crate::catalog::hang::CatalogExt>(
		consumer: &moq_net::broadcast::Consumer,
		catalog: &crate::catalog::Producer<E>,
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = crate::container::Consumer::new(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Audio),
		);
		let mut frames = Vec::new();
		while let Ok(Ok(Some(frame))) = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await {
			frames.push(frame);
		}
		frames
	}

	// A looping publisher plays part of a file and wraps to the top (#2729). The wrap rewinds
	// the audio PTS, which is an encoder restart, so the import ends and the caller publishes
	// a new broadcast. Every frame published before it is still a whole AC-3 frame.
	#[tokio::test(start_paused = true)]
	async fn legacy_refuses_a_looping_file_wrap() {
		let data = include_bytes!("test_data/ac3.ts");

		// Wrap mid-file on a packet boundary, so the TS layer stays aligned.
		let cut = (data.len() / 2) / 188 * 188;
		let mut looped = data[..cut].to_vec();
		looped.extend_from_slice(data);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let err = import
			.decode(&bytes::BytesMut::from(&looped[..]))
			.expect_err("a loop wrap is a restart");
		assert!(is_rewind(&err), "{err:?}");

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert!(!frames.is_empty(), "the pass before the wrap published");
		assert!(
			frames.iter().all(|f| f.payload[0] == 0x0B && f.payload[1] == 0x77),
			"published a frame that doesn't start at an AC-3 sync word"
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
		let mut reader = crate::container::Consumer::new(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
		);
		let mut frames = Vec::new();
		while let Ok(Ok(Some(frame))) = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await {
			frames.push(frame);
		}
		frames
	}

	/// Annex-B bytes for one access unit: SPS + PPS + IDR for a keyframe, else a delta slice.
	pub(in crate::container::ts) fn annexb_au(keyframe: bool) -> Vec<u8> {
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

	// A PES ending mid-picture cannot prove that picture complete. Keep only AUs
	// closed by a codec boundary ahead of a cut, and recover at the next keyframe.
	#[tokio::test(start_paused = true)]
	async fn video_drops_a_partial_access_unit_across_a_continuity_break() {
		const VIDEO_PID: u16 = 0x0050;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		// One resync, charged the damaged frame's 47 bytes (7 header + 40 body).
		assert_eq!(
			import.stats().streams,
			BTreeMap::from([(
				AAC_PID,
				super::stats::Stream {
					track: ".aac".to_string(),
					class: super::stats::Class::Audio,
					units: 4,
					quiet: None,
					resyncs: 1,
					discarded: 47,
					unconfirmed: 0,
					..Default::default()
				}
			)]),
			"the resync left no trace an operator could alarm on"
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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

	// A frame header that changes the channel count is a new decoder identity: the frames after
	// it publish on a new track, and the first track is retired rather than rewritten.
	#[tokio::test(start_paused = true)]
	async fn legacy_channel_change_starts_a_new_track() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false))
			.unwrap();

		// The same frame in stereo: the mode bits clear.
		let stereo = |fill| {
			let mut frame = mp2_frame(fill);
			frame[3] = 0x00;
			frame
		};
		let mono = [mp2_frame(0xAA), mp2_frame(0xBB)].concat();
		import.decode(&audio_pes_packet(MP2_PID, 0, 90_000, &mono)).unwrap();
		let first = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();
		let records = catalog.hls_tracks();
		let both = [stereo(0xCC), stereo(0xDD)].concat();
		import.decode(&audio_pes_packet(MP2_PID, 1, 180_000, &both)).unwrap();
		import.decode(&audio_pes_packet(MP2_PID, 2, 270_000, &both)).unwrap();

		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.audio.renditions.len(), 1, "the mono track is retired");
		let (name, config) = snapshot.audio.renditions.iter().next().unwrap();
		assert_ne!(name, &first, "the stereo frames publish under a new name");
		assert_eq!(config.channel_count, 2);
		assert_eq!(catalog.hls_tracks(), records, "the mono track's HLS records retire");
		import.finish().unwrap();
		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(frames[0].payload, stereo(0xCC), "the new track starts at the change");
	}

	// An ADTS header that changes the sample rate is a new decoder identity, so the frames after
	// it publish on a new track, on the same program timebase.
	#[tokio::test(start_paused = true)]
	async fn aac_sample_rate_change_starts_a_new_track() {
		const AAC_PID: u16 = 0x0060;
		let frame = |rate| {
			let mut f = super::adts::write_header(2, rate, 2, 10).unwrap().to_vec();
			f.resize(f.len() + 10, 0x20);
			f
		};

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false),
			..Default::default()
		};
		let cc = mux.cc(AAC_PID);
		mux.out.extend(audio_pes_packet(
			AAC_PID,
			cc,
			90_000,
			&[frame(48_000), frame(48_000)].concat(),
		));
		import.decode(&mux.out).unwrap();
		let first = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();
		let records = catalog.hls_tracks();

		mux.out.clear();
		for pts in [180_000, 270_000] {
			let cc = mux.cc(AAC_PID);
			mux.out.extend(audio_pes_packet(
				AAC_PID,
				cc,
				pts,
				&[frame(44_100), frame(44_100)].concat(),
			));
		}
		import.decode(&mux.out).unwrap();
		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.audio.renditions.len(), 1, "the 48 kHz track is retired");
		let (name, config) = snapshot.audio.renditions.iter().next().unwrap();
		assert_ne!(name, &first, "the 44.1 kHz frames publish under a new name");
		assert_eq!(config.sample_rate, 44_100);
		assert_eq!(catalog.hls_tracks(), records, "the 48 kHz track's HLS records retire");
		let recorded: Vec<_> = snapshot.ext.mpegts.tracks.keys().cloned().collect();
		assert_eq!(
			recorded,
			vec![name.clone()],
			"the PID's mpegts entry moves to the new track"
		);
		import.finish().unwrap();
		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(
			frames[0].timestamp,
			moq_net::Timestamp::from_micros(2_000_000).unwrap(),
			"the new track keeps the program's timebase"
		);
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		// The recovery leaves evidence: one resync, and the damaged frame's 72 bytes.
		assert_eq!(
			import.stats().streams,
			BTreeMap::from([(
				MP2_PID,
				super::stats::Stream {
					track: ".mp2".to_string(),
					class: super::stats::Class::Audio,
					units: 4,
					quiet: None,
					resyncs: 1,
					discarded: 72,
					unconfirmed: 0,
					..Default::default()
				}
			)]),
			"the resync left no trace an operator could alarm on"
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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

	// #3533: a content join landed a PES PTS a millisecond below the last frame published. Each
	// audio frame is its own group, so that group starts before the previous one: a restart,
	// which ends the import rather than shifting it forward.
	#[tokio::test(start_paused = true)]
	async fn legacy_pes_below_the_live_edge_is_refused() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let pmt = synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();

		// Two frames so the first is confirmed and published: live edge is 90_000 ticks.
		let mut first = mp2_frame(0xAA);
		first.extend_from_slice(&mp2_frame(0xBB));
		import
			.decode(audio_pes_packet(MP2_PID, 0, 90_000, &first).as_slice())
			.unwrap();

		// A join a millisecond behind the live edge.
		let mut join = mp2_frame(0xDD);
		join.extend_from_slice(&mp2_frame(0xEE));
		let err = import
			.decode(audio_pes_packet(MP2_PID, 1, 89_910, &join).as_slice())
			.and_then(|()| import.finish())
			.expect_err("a PES PTS below the live edge is a restart");
		assert!(is_rewind(&err), "{err:?}");
	}

	// Confirming a joined frame means holding it when the buffer ends too soon after it to
	// hold a header. At end of stream that header is never coming, so the drain publishes it
	// anyway rather than dropping a frame that is whole and parses.
	#[tokio::test(start_paused = true)]
	async fn legacy_drains_a_joined_frame_at_end_of_stream() {
		const MP2_PID: u16 = 0x0061;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		// Publishing it is the right trade, but it is a frame nothing vouched for: at a
		// splice those joined bytes are a substitution rather than a gap, so the drain is
		// counted instead of being silent.
		assert_eq!(
			import.stats().streams,
			BTreeMap::from([(
				MP2_PID,
				super::stats::Stream {
					track: ".mp2".to_string(),
					class: super::stats::Class::Audio,
					units: 2,
					quiet: None,
					resyncs: 0,
					discarded: 0,
					unconfirmed: 1,
					..Default::default()
				}
			)]),
			"the drained frame was published without a trace"
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
			.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = crate::container::Consumer::new(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
		);
		let published = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await;
		assert!(
			matches!(published, Ok(Ok(Some(_)))),
			"a PID that never published held the catalog shut: {published:?}"
		);
		drop(import);
	}

	/// A receiver that joins an AAC stream after its program config element (ffmpeg writes it in
	/// the first frame only) cannot build that track, but must not withhold the catalog for the
	/// rest of the program. The track joins once an element arrives, and a repeated element
	/// leaves its frame, since the description carries the layout.
	#[tokio::test(start_paused = true)]
	async fn aac_joined_after_its_program_config_publishes_the_rest() {
		const AAC_PID: u16 = 0x0060;
		// ffmpeg's quad layout, two channel pair elements in a program config element.
		let mut quad = vec![0x11, 0x80, 0x04, 0xC4, 0x04, 0x00, 0x21, 0x10, 0x0C];
		quad.extend_from_slice(b"Lavc63.1.101");
		let pce = crate::codec::aac::in_band(&quad).unwrap().program_config.unwrap();
		// A raw data block: the element, if given, then bytes that lead with a channel pair (ID 1).
		let frame = |pce: &[u8]| {
			let mut f = super::adts::write_header(2, 48_000, 0, pce.len() + 10)
				.unwrap()
				.to_vec();
			f.extend_from_slice(pce);
			f.resize(f.len() + 10, 0x20);
			f
		};

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO), (StreamType::AdtsAac, AAC_PID)], false),
			..Default::default()
		};
		mux.gop(VIDEO, 90_000);
		let cc = mux.cc(AAC_PID);
		let bare = frame(&[]);
		mux.out.extend(audio_pes_packet(
			AAC_PID,
			cc,
			90_000,
			&[bare.as_slice(), &bare, &bare].concat(),
		));
		import.decode(&mux.out).unwrap();
		assert_eq!(import.stats().streams[&AAC_PID].damaged, 1);

		let first = tokio::time::timeout(Duration::from_millis(10), updates.next())
			.await
			.expect("the catalog waits on a track that cannot resolve")
			.unwrap()
			.unwrap();
		assert_eq!(first.video.renditions.len(), 1);
		assert!(first.audio.renditions.is_empty());

		// The element arrives, then frames without it, then a repeat.
		let with = frame(&pce);
		let cc = mux.cc(AAC_PID);
		let late = audio_pes_packet(AAC_PID, cc, 180_000, &[with.as_slice(), &bare, &with, &bare].concat());
		import.decode(&late).unwrap();
		import.finish().unwrap();

		let audio = loop {
			let update = tokio::time::timeout(Duration::from_millis(10), updates.next())
				.await
				.expect("the AAC track never joined")
				.unwrap()
				.unwrap();
			if let Some(audio) = update.audio.renditions.values().next() {
				break audio.clone();
			}
		};
		assert_eq!(audio.channel_count, 4);
		assert_eq!(audio.description.as_deref(), Some(quad.as_slice()));

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(frames.len(), 4, "only the frames from the element on");
		for frame in frames {
			assert_eq!(frame.payload.as_ref(), [0x20; 10], "the element leaves every frame");
		}
	}

	// A program config element that changes the layout mid-stream refuses its unit as
	// damaged, and the track carries on with the frames around it.
	#[tokio::test]
	async fn aac_program_config_changed_mid_stream_refuses_the_unit() {
		const AAC_PID: u16 = 0x0060;
		// ffmpeg's quad layout: one front and one back channel pair.
		let mut quad = vec![0x11, 0x80, 0x04, 0xC4, 0x04, 0x00, 0x21, 0x10, 0x0C];
		quad.extend_from_slice(b"Lavc63.1.101");
		// The same two pairs, with the second moved from the back to the side.
		let mut side = quad.clone();
		side[4] = 0x40;
		let element = |asc: &[u8]| crate::codec::aac::in_band(asc).unwrap().program_config.unwrap();
		let frame = |pce: &[u8]| {
			let mut f = super::adts::write_header(2, 48_000, 0, pce.len() + 10)
				.unwrap()
				.to_vec();
			f.extend_from_slice(pce);
			f.resize(f.len() + 10, 0x20);
			f
		};
		let (with, changed, bare) = (frame(&element(&quad)), frame(&element(&side)), frame(&[]));

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false),
			..Default::default()
		};
		for (pts, unit) in [
			(90_000, [with.as_slice(), &bare]),
			(180_000, [changed.as_slice(), &bare]),
		] {
			let cc = mux.cc(AAC_PID);
			mux.out.extend(audio_pes_packet(AAC_PID, cc, pts, &unit.concat()));
		}
		let cc = mux.cc(AAC_PID);
		mux.out.extend(audio_pes_packet(AAC_PID, cc, 270_000, &bare));
		import.decode(&mux.out).unwrap();
		import.finish().unwrap();
		assert_eq!(import.stats().streams[&AAC_PID].damaged, 1);

		let frames = read_audio_frames(&consumer, &catalog).await;
		assert_eq!(frames.len(), 3, "the changed unit is dropped, the rest publish");
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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

	// A cue is placed at its first section byte, independently of the video PTS.
	#[tokio::test(start_paused = true)]
	async fn scte35_cue_stamped_with_pcr_placement() {
		use crate::catalog::hang::{Catalog, Container};
		use crate::container::Consumer;
		use crate::container::ts::catalog::Ext;
		use moq_net::Timestamp;

		const VIDEO_PID: u16 = 0x0050;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let mut bytes = bytes::BytesMut::new();
		bytes.extend_from_slice(&synth_pmt(
			&[
				(StreamType::Mpeg2Video, VIDEO_PID),
				(StreamType::Dts8ChannelLosslessAudio, 0x21),
			],
			true,
		));
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 27_000_000));
		bytes.extend_from_slice(&pes_packet(VIDEO_PID, 99_000));
		let at = bytes.len() as u64 + 5;
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 29_700_000));
		import.decode(&bytes).unwrap();
		let expected = 1_000_000 + (at - (2 * 188 + 10)) * 100_000 / (3 * 188);
		import.finish().unwrap();

		let name = track_on(&catalog.snapshot().ext.mpegts, 0x21);
		let track = consumer
			.track(&name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
			.await
			.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy(crate::container::Kind::Data));
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read_frame())
			.await
			.expect("cue read timed out")
			.unwrap()
			.expect("a published cue frame");

		assert_eq!(&frame.payload[..], &CUE[..], "verbatim splice_info_section");
		assert_ne!(frame.timestamp, Timestamp::ZERO, "cue must not stamp zero");
		// Placement is normalized to microseconds, independently of the video PTS.
		assert_eq!(
			Duration::from(frame.timestamp),
			Duration::from_micros(expected),
			"cue stamped at its first section byte on the PCR clock"
		);
	}

	/// A section's header can straddle several PCRs; only the first PCR past its first byte places it.
	#[tokio::test(start_paused = true)]
	async fn section_placement_survives_fragmented_headers_and_shared_packets() {
		for split in [1, 2, 17] {
			let mut data = synth_pmt(
				&[
					(StreamType::Mpeg2Video, PCR_PID),
					(StreamType::Dts8ChannelLosslessAudio, 0x21),
				],
				true,
			);
			let before = data.len() as u64 + 10;
			data.extend(pcr_packet(PCR_PID, 27_000_000));
			data.extend(pes_packet(PCR_PID, 108_000));
			let first = [vec![0], CUE[..split].to_vec()].concat();
			let at = data.len() as u64 + 188 - split as u64;
			let first = pes_fragment(0x21, true, 0, &first);
			data.extend(first);
			let after = data.len() as u64 + 10;
			data.extend(pcr_packet(PCR_PID, 28_350_000));
			data.extend(pcr_packet(PCR_PID, 29_700_000));
			// A pointer finishes the partial then starts two complete sections in one packet.
			let tail = &CUE[split..];
			let shared = [tail, &CUE, &CUE].concat();
			let next = data.len() as u64 + 5 + tail.len() as u64;
			let shared = packet(true, 1, tail.len() as u8, &shared);
			assert_eq!(run(&[first.to_vec(), shared.clone()]), vec![CUE.to_vec(); 3]);
			data.extend(shared);
			let last = data.len() as u64 + 10;
			data.extend(pcr_packet(PCR_PID, 31_050_000));
			let (consumer, catalog, _import) = import_all(&data).unwrap();
			let name = track_on(&catalog.snapshot().ext.mpegts, 0x21);
			let frames = read_track(&consumer, &name, crate::container::Kind::Data).await;
			let times: Vec<_> = frames.iter().map(|frame| frame.timestamp.as_micros() as u64).collect();
			let clock = |at: u64, before: u64, ticks: u64, after: u64| {
				(ticks + (at - before) * 1_350_000 / (after - before)) / 27
			};
			assert_eq!(
				times,
				vec![
					clock(at, before, 27_000_000, after),
					clock(next, after + 188, 29_700_000, last),
					clock(next + CUE.len() as u64, after + 188, 29_700_000, last)
				],
				"split {split}"
			);
			assert!(frames.iter().all(|frame| frame.payload.as_ref() == CUE));
		}
	}

	/// Missing placement is counted the same way for whole PES and sections.
	#[tokio::test(start_paused = true)]
	async fn sections_without_a_pcr_interval_are_unplaced() {
		let mut data = synth_pmt(
			&[
				(StreamType::Mpeg2Video, PCR_PID),
				(StreamType::Dts8ChannelLosslessAudio, 0x21),
			],
			true,
		);
		data.extend(packet(true, 0, 0, &CUE));
		data.extend(pcr_packet(PCR_PID, 27_000_000));
		data.extend(pes_packet(PCR_PID, 108_000));
		data.extend(packet(true, 1, 0, &CUE));
		let (consumer, catalog, import) = import_all(&data).unwrap();
		assert_eq!(import.stats().streams[&0x21].unplaced, 2);
		let name = track_on(&catalog.snapshot().ext.mpegts, 0x21);
		assert!(
			read_track(&consumer, &name, crate::container::Kind::Data)
				.await
				.is_empty()
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
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
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
			!catalog
				.snapshot()
				.ext
				.mpegts
				.tracks
				.values()
				.any(|track| track.pid == SECTION_PID),
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

	/// A private-data-only program retains the PES clock while waiting for rate measurement or EOF.
	#[tokio::test]
	async fn private_pes_only_program_retains_its_pes_clock_until_catalog_release() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		const DATA_PID: u16 = 0x0052;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let provisional = catalog.snapshot().clock.expect("a clock");
		let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
		let mut import = super::Import::new(broadcast, catalog.reserve());

		import
			.decode(&synth_pmt(&[(StreamType::Mpeg2PacketizedData, DATA_PID)], false))
			.unwrap();
		assert_eq!(catalog.snapshot().ext.mpegts.tracks.len(), 1, "the PMT was read");
		assert_eq!(clocks.drain(), vec![], "the PMT alone publishes nothing");

		import
			.decode(&audio_pes_packet(DATA_PID, 0, 3_600 * 90_000, &[0xDE, 0xAD]))
			.unwrap();
		let anchored = catalog.snapshot().clock.expect("a clock");
		assert_ne!(anchored, provisional, "the first PES anchors the clock");
		assert!(clocks.drain().is_empty(), "rate measurement still holds the catalog");
		import.finish().unwrap();
		let published = clocks.drain();
		assert!(!published.is_empty(), "EOF releases a program without PCR samples");
		assert!(published.iter().all(|clock| *clock == Some(anchored)), "{published:?}");
	}

	/// A TS packet on `pid` carrying only an adaptation field (no payload) that sets
	/// `discontinuity_indicator`: the clock packet a mux flags a timebase reset on.
	fn clock_break_packet(pid: u16) -> Vec<u8> {
		let mut p = vec![
			0x47,
			(pid >> 8) as u8 & 0x1f,
			(pid & 0xff) as u8,
			0x20,
			183, // adaptation_field_length: the rest of the packet
			0x80,
		];
		p.resize(188, 0xff);
		p
	}

	/// Set `discontinuity_indicator` on a packet that already carries an adaptation field.
	fn flag_discontinuity(mut pkt: Vec<u8>) -> Vec<u8> {
		assert!(
			pkt[3] & 0x20 != 0 && pkt[4] > 0,
			"packet has no adaptation field to flag"
		);
		pkt[5] |= 0x80;
		pkt
	}

	/// One PES on `pid` carrying two whole MP2 frames: enough for the second to confirm the
	/// first, which is what the legacy path publishes on.
	pub(in crate::container::ts) fn mp2_pes(pid: u16, cc: u8, pts: u64, fills: [u8; 2]) -> Vec<u8> {
		let mut payload = mp2_frame(fills[0]);
		payload.extend_from_slice(&mp2_frame(fills[1]));
		audio_pes_packet(pid, cc, pts, &payload)
	}

	/// Read every retained frame of `name`, with the count of timeline breaks the consumer
	/// crossed reading them.
	async fn read_breaks(
		consumer: &moq_net::broadcast::Consumer,
		name: &str,
		kind: crate::container::Kind,
	) -> (Vec<crate::container::Frame>, u64) {
		// A generous max delay: the default of zero would shed every non-latest group, the
		// declared breaks among them, and the subscribe start is resolved from it too, so
		// it has to reach back past a 30 s leap to the first frame.
		let subscription = moq_net::track::Subscription::default().with_max_delay(std::time::Duration::from_secs(3600));
		let track = consumer.track(name).unwrap().subscribe(subscription).await.unwrap();
		let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy(kind));
		let mut frames = Vec::new();
		let mut breaks = 0;
		while let Ok(Ok(Some(read))) = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read()).await {
			match read {
				crate::container::Read::Frame { frame, .. } => frames.push(frame),
				crate::container::Read::Gap { .. } | crate::container::Read::End { .. } => breaks += 1,
			}
		}
		(frames, breaks)
	}

	/// Both [`two_stream_import`] renditions' frames and break counts, in catalog order.
	async fn read_all_breaks(
		consumer: &moq_net::broadcast::Consumer,
		catalog: &crate::catalog::Producer,
	) -> Vec<(Vec<crate::container::Frame>, u64)> {
		let names: Vec<String> = catalog.snapshot().audio.renditions.keys().cloned().collect();
		let mut out = Vec::new();
		for name in names {
			out.push(read_breaks(consumer, &name, crate::container::Kind::Audio).await);
		}
		assert_eq!(out.len(), 2, "both renditions must exist");
		out
	}

	/// A two-program multiplex whose clocks sit an hour apart: program 1's MP2 on
	/// `0x0061` starts at 1 s, program 2's on `0x0071` at 3601 s.
	pub(in crate::container::ts) fn two_programs() -> Vec<u8> {
		let mut data = synth_programs(
			&[
				(1, 0x0100, &[(StreamType::Mpeg1Audio, 0x0061)]),
				(2, 0x0200, &[(StreamType::Mpeg1Audio, 0x0071)]),
			],
			false,
		);
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		data.extend(mp2_pes(0x0071, 0, 3_601 * 90_000, [0xCC, 0xDD]));
		data
	}

	#[test]
	fn a_multi_program_input_is_refused_before_publishing() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());

		let err = import.decode(&two_programs()).unwrap_err();
		assert_eq!(
			err.downcast_ref::<super::MultipleProgramsError>(),
			Some(&super::MultipleProgramsError { programs: vec![1, 2] }),
			"{err:#}"
		);
		assert!(catalog.snapshot().audio.renditions.is_empty(), "nothing was published");
	}

	#[test]
	fn a_program_added_mid_stream_ends_the_import() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::Mpeg1Audio, 0x0061)], false))
			.unwrap();
		import.decode(&mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB])).unwrap();

		let err = import.decode(&two_programs()).unwrap_err();
		assert!(err.downcast_ref::<super::MultipleProgramsError>().is_some(), "{err:#}");
		assert_eq!(
			catalog.snapshot().audio.renditions.len(),
			1,
			"program 1 stays published"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn a_selected_program_publishes_only_its_streams_on_its_clock() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve()).with_program(2);
		import.decode(&two_programs()).unwrap();
		import.finish().unwrap();

		let snapshot = catalog.snapshot();
		let names: Vec<_> = snapshot.audio.renditions.keys().collect();
		assert_eq!(names.len(), 1, "only program 2's stream: {names:?}");
		let (frames, _) = read_breaks(&consumer, names[0], crate::container::Kind::Audio).await;
		assert!(!frames.is_empty(), "program 2 published");
		assert!(
			frames
				.iter()
				.all(|frame| frame.payload[4] == 0xCC || frame.payload[4] == 0xDD),
			"no program 1 frame leaks in"
		);
		assert_eq!(frames[0].timestamp.as_micros(), 3_601_000_000, "program 2's own PTS");
	}

	#[test]
	fn a_selected_program_the_pat_does_not_list_is_refused() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve()).with_program(3);

		let err = import.decode(&two_programs()).unwrap_err().to_string();
		assert!(err.contains("no program 3") && err.contains("1, 2"), "{err}");
	}

	/// `section` on `pid` as the packets a mux would send, counting from `*cc`: the first
	/// opens it behind a pointer_field skipping `tail` (the end of an earlier section), and
	/// the rest carry it on.
	pub(in crate::container::ts) fn psi_packets(pid: u16, cc: &mut u8, tail: &[u8], section: &[u8]) -> Vec<u8> {
		let mut payload = vec![tail.len() as u8];
		payload.extend_from_slice(tail);
		payload.extend_from_slice(section);
		let mut out = Vec::new();
		for (i, chunk) in payload.chunks(184).enumerate() {
			let pusi = if i == 0 { 0x40 } else { 0 };
			out.extend_from_slice(&[0x47, pusi | (pid >> 8) as u8, pid as u8, 0x10 | *cc]);
			out.extend_from_slice(chunk);
			out.resize(out.len().next_multiple_of(188), 0xff);
			*cc = (*cc + 1) & 0x0f;
		}
		out
	}

	/// A PAT section listing `(program_number, pmt_pid)`.
	pub(in crate::container::ts) fn pat_section(programs: &[(u16, u16)]) -> Vec<u8> {
		let body: Vec<u8> = programs
			.iter()
			.flat_map(|&(number, pid)| [(number >> 8) as u8, number as u8, 0xe0 | (pid >> 8) as u8, pid as u8])
			.collect();
		super::psi::section(0x00, 1, 0, 0, 0, &body)
	}

	/// A PMT section listing `(stream_type, pid, es_descriptors)`, the clock on the first.
	pub(in crate::container::ts) fn pmt_section(program: u16, version: u8, streams: &[(u8, u16, &[u8])]) -> Vec<u8> {
		let pcr = streams.first().map_or(0x1fff, |&(_, pid, _)| pid);
		let mut body = vec![0xe0 | (pcr >> 8) as u8, pcr as u8, 0xf0, 0x00];
		for &(stream_type, pid, descriptors) in streams {
			body.extend_from_slice(&[
				stream_type,
				0xe0 | (pid >> 8) as u8,
				pid as u8,
				0xf0,
				descriptors.len() as u8,
			]);
			body.extend_from_slice(descriptors);
		}
		super::psi::section(0x02, program, version, 0, 0, &body)
	}

	/// `section` with one bit of its CRC flipped.
	pub(in crate::container::ts) fn corrupt(mut section: Vec<u8>) -> Vec<u8> {
		*section.last_mut().unwrap() ^= 0x01;
		section
	}

	pub(in crate::container::ts) const MP2: u8 = StreamType::Mpeg1Audio as u8;

	/// A PAT listing fifty programs, which takes two packets, then program 50's PMT (one
	/// MP2 stream on `0x0061`) and one PES on it.
	pub(in crate::container::ts) fn fifty_programs() -> Vec<u8> {
		let programs: Vec<_> = (1..=50).map(|n| (n, 0x0100 + n)).collect();
		let pat = pat_section(&programs);
		let mut data = psi_packets(0, &mut 0, &[], &pat);
		assert_eq!(data.len(), 2 * 188, "the PAT spans two packets");
		data.extend(psi_packets(
			0x0132,
			&mut 0,
			&[],
			&pmt_section(50, 0, &[(MP2, 0x0061, &[])]),
		));
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		data
	}

	/// A PAT split into two sections, each in its own packet, listing program 1 then program
	/// 2, then each program's PMT (one MP2 stream) and one PES on it.
	pub(in crate::container::ts) fn two_section_pat() -> Vec<u8> {
		let mut cc = 0;
		let mut data = Vec::new();
		for (number, program) in [(0, 1u16), (1, 2)] {
			let entry = [0x00, program as u8, 0xe0 | program as u8, 0x00];
			let section = super::psi::section(0x00, 1, 0, number, 1, &entry);
			data.extend(psi_packets(0, &mut cc, &[], &section));
		}
		for (program, pid, fills) in [(1u16, 0x0061, [0xAA, 0xBB]), (2, 0x0071, [0xCC, 0xDD])] {
			let pmt = pmt_section(program, 0, &[(MP2, pid, &[])]);
			data.extend(psi_packets(program << 8, &mut 0, &[], &pmt));
			data.extend(mp2_pes(pid, 0, 90_000, fills));
		}
		data
	}

	fn psi_import() -> (crate::catalog::Producer, super::Import) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let import = super::Import::new(broadcast, catalog.reserve());
		(catalog, import)
	}

	#[test]
	fn a_pmt_spanning_two_packets_is_read_whole() {
		let (catalog, mut import) = psi_import();
		// A private descriptor long enough to push the second stream into the next packet.
		let mut descriptor = vec![0x80, 200];
		descriptor.resize(202, 0x00);
		let pmt = pmt_section(1, 0, &[(MP2, 0x0061, &descriptor), (MP2, 0x0062, &[])]);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100)]));
		data.extend(psi_packets(0x0100, &mut 0, &[], &pmt));
		assert_eq!(data.len(), 3 * 188, "the PMT spans two packets");
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		data.extend(mp2_pes(0x0062, 0, 90_000, [0xCC, 0xDD]));

		import.decode(&data).unwrap();
		import.finish().unwrap();
		assert_eq!(catalog.snapshot().audio.renditions.len(), 2, "both streams publish");
		assert_eq!(import.stats().crc_error, 0);
	}

	#[test]
	fn a_pat_behind_a_nonzero_pointer_field_is_read() {
		let (catalog, mut import) = psi_import();
		let mut data = psi_packets(0, &mut 0, &[0xab; 7], &pat_section(&[(1, 0x0100)]));
		data.extend(psi_packets(
			0x0100,
			&mut 0,
			&[],
			&pmt_section(1, 0, &[(MP2, 0x0061, &[])]),
		));
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));

		import.decode(&data).unwrap();
		import.finish().unwrap();
		assert_eq!(catalog.snapshot().audio.renditions.len(), 1);
	}

	#[test]
	fn a_program_listed_in_a_pats_second_packet_is_selected() {
		let (catalog, import) = psi_import();
		let mut import = import.with_program(50);
		import.decode(&fifty_programs()).unwrap();
		import.finish().unwrap();
		assert_eq!(catalog.snapshot().audio.renditions.len(), 1, "program 50 publishes");

		// Unselected, the whole PAT is what refuses the input.
		let (_, mut import) = psi_import();
		let err = import.decode(&fifty_programs()).unwrap_err();
		let err = err.downcast_ref::<super::MultipleProgramsError>().unwrap();
		assert_eq!(err.programs, (1..=50).collect::<Vec<_>>());
	}

	/// A corrupt PAT and a corrupt PMT between good repetitions are each dropped and counted
	/// once, while the layout they would have changed holds; a later good PMT revision still
	/// applies.
	#[test]
	fn corrupt_psi_between_good_repetitions_is_dropped_and_counted() {
		let (catalog, mut import) = psi_import();
		let (mut pat_cc, mut pmt_cc) = (0, 0);
		let pat = pat_section(&[(1, 0x0100)]);
		let pmt = pmt_section(1, 0, &[(MP2, 0x0061, &[])]);
		let two = [(MP2, 0x0061, &[][..]), (MP2, 0x0062, &[][..])];

		let mut data = psi_packets(0, &mut pat_cc, &[], &pat);
		data.extend(psi_packets(0x0100, &mut pmt_cc, &[], &pmt));
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		import.decode(&data).unwrap();
		let units = import.stats().streams[&0x0061].units;

		// Read, this PAT would end the unselected import with a second program, and this PMT
		// would add a stream.
		let mut data = psi_packets(0, &mut pat_cc, &[], &corrupt(pat_section(&[(1, 0x0100), (2, 0x0200)])));
		data.extend(psi_packets(0x0100, &mut pmt_cc, &[], &corrupt(pmt_section(1, 1, &two))));
		data.extend(psi_packets(0, &mut pat_cc, &[], &pat));
		data.extend(psi_packets(0x0100, &mut pmt_cc, &[], &pmt));
		data.extend(mp2_pes(0x0061, 1, 180_000, [0xAA, 0xBB]));
		import.decode(&data).unwrap();

		assert_eq!(
			catalog.snapshot().audio.renditions.len(),
			1,
			"the corrupt PMT added nothing"
		);
		assert!(
			import.stats().streams[&0x0061].units > units,
			"the stream kept delivering"
		);
		assert_eq!(import.crc_errors(), &BTreeMap::from([(0x0100, 1)]));
		assert_eq!(import.demux.pat_crc_error(), 1);
		assert_eq!(import.stats().crc_error, 2);

		let mut data = psi_packets(0x0100, &mut pmt_cc, &[], &pmt_section(1, 2, &two));
		data.extend(mp2_pes(0x0062, 0, 180_000, [0xCC, 0xDD]));
		import.decode(&data).unwrap();
		import.finish().unwrap();
		assert_eq!(
			catalog.snapshot().audio.renditions.len(),
			2,
			"the good revision applied"
		);
		assert_eq!(import.stats().crc_error, 2, "good sections count nothing");
	}

	/// The positive control: with no good PAT, nothing is learned, so nothing publishes.
	#[test]
	fn a_feed_whose_only_pat_is_corrupt_publishes_nothing() {
		let (catalog, mut import) = psi_import();
		let mut data = psi_packets(0, &mut 0, &[], &corrupt(pat_section(&[(1, 0x0100)])));
		data.extend(psi_packets(
			0x0100,
			&mut 0,
			&[],
			&pmt_section(1, 0, &[(MP2, 0x0061, &[])]),
		));
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));

		import.decode(&data).unwrap();
		import.finish().unwrap();
		assert!(catalog.snapshot().audio.renditions.is_empty());
		assert_eq!(import.stats().crc_error, 1);
		assert!(!import.stats().is_empty(), "a dropped section is something to report");
	}

	#[test]
	fn adaptation_field_past_the_packet_drops_partial() {
		let section = fake_section(0xfc, 300);
		let mut malformed = packet(false, 1, 0, &section[183..]);
		// afc 0b11 with an adaptation_field_length reaching past the packet.
		malformed[3] = 0x31;
		malformed[4] = 200;
		let rest = packet(false, 2, 0, &section[183..]);
		assert!(run(&[packet(true, 0, 0, &section[..183]), malformed, rest]).is_empty());
	}

	/// Two MP2 renditions, the first of which the PMT designates as the PCR PID.
	const PCR_PID: u16 = 0x0061;
	const PEER_PID: u16 = 0x0062;

	fn two_stream_import() -> (moq_net::broadcast::Consumer, crate::catalog::Producer, super::Import) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt = synth_pmt(
			&[(StreamType::Mpeg1Audio, PCR_PID), (StreamType::Mpeg1Audio, PEER_PID)],
			false,
		);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();
		(consumer, catalog, import)
	}

	#[test]
	fn timebase_reset_clears_unpublished_pending() {
		let (_, _, mut import) = two_stream_import();
		let payload = mp2_frame(0xAA);
		import
			.decode(audio_pes_open(PEER_PID, 0, 90_000, payload.len() * 2, &payload[..20]).as_slice())
			.unwrap();
		assert!(!import.published);
		assert!(!import.pending.is_empty());
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		assert!(import.pending.is_empty(), "old PES survived the timebase reset");
	}

	#[test]
	fn timebase_reset_clears_si_clock() {
		let (_, _, mut import) = two_stream_import();
		import.last_pts = Some(Timestamp::from_micros(45_000_000).unwrap());
		import.published = true;
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		assert!(import.last_pts.is_none(), "SI clock belongs to the old timebase");
	}

	#[tokio::test(start_paused = true)]
	async fn timebase_reset_after_anchored_section_publication() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(
				synth_pmt(
					&[
						(StreamType::Mpeg2Video, PCR_PID),
						(StreamType::Dts8ChannelLosslessAudio, 0x21),
					],
					true,
				)
				.as_slice(),
			)
			.unwrap();
		import.decode(pcr_packet(PCR_PID, 24_300_000).as_slice()).unwrap();
		import.decode(packet(true, 0, 0, &CUE).as_slice()).unwrap();
		import.decode(pes_packet(PCR_PID, 90_000).as_slice()).unwrap();
		import.decode(pcr_packet(PCR_PID, 27_000_000).as_slice()).unwrap();
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		import.decode(pcr_packet(PCR_PID, 24_300_000).as_slice()).unwrap();
		import.decode(packet(true, 1, 0, &CUE).as_slice()).unwrap();
		import.decode(pes_packet(PCR_PID, 90_000).as_slice()).unwrap();
		import.decode(pcr_packet(PCR_PID, 27_000_000).as_slice()).unwrap();
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		import.decode(pcr_packet(PCR_PID, 24_300_000).as_slice()).unwrap();
		import.decode(packet(true, 2, 0, &CUE).as_slice()).unwrap();
		import.decode(pes_packet(PCR_PID, 90_000).as_slice()).unwrap();
		import.decode(pcr_packet(PCR_PID, 27_000_000).as_slice()).unwrap();
		import.finish().unwrap();
		let name = track_on(&catalog.snapshot().ext.mpegts, 0x21);
		let (frames, breaks) = read_breaks(&consumer, &name, crate::container::Kind::Data).await;
		assert_eq!(frames.len(), 3);
		assert_eq!(
			breaks, 2,
			"each break on the data track reads once, as its declared end"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn data_only_pcr_era_is_shared_with_media_added_later() {
		use super::super::catalog::Ext;
		use super::super::fixture::{Flags, MS, Mux, WRAP};
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let mut mux = Mux::default();
		mux.pat(&[(1, 0x20)])
			.pmt(0x20, 1, PCR_PID, &[(0x05, 0x21)], 0)
			.packet(PCR_PID, Flags::pcr(WRAP - 40 * MS))
			.section(0x21, &CUE)
			.packet(PCR_PID, Flags::pcr(40 * MS));
		import.decode(&mux.bytes()).unwrap();
		assert!(import.timebase.offset().is_some());
		assert!(!import.media_clock);
		let mut mux = Mux::default();
		mux.pmt(0x20, 1, PCR_PID, &[(0x02, 0x100), (0x05, 0x21)], 0)
			.pes(0x100, Flags::default(), 9_000)
			.fill(0x100, 1);
		import.decode(&mux.bytes()).unwrap();
		assert!(import.media_clock);
		assert_eq!(
			import.last_pts.unwrap(),
			Timestamp::from_scale((1 << 33) + 9_000, 90_000).unwrap()
		);
		import.finish().unwrap();
	}

	#[test]
	fn sections_without_a_media_anchor_follow_whole_pes_handling() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		for clock in ["none", "shared offset", "clock break"] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(
				&mut broadcast,
				crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
			)
			.unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve());
			import
				.decode(&synth_pmt(
					&[
						(StreamType::Mpeg2Video, PCR_PID),
						(StreamType::Dts8ChannelLosslessAudio, 0x21),
					],
					true,
				))
				.unwrap();
			if clock == "shared offset" {
				// A sibling importer can anchor the shared timebase before this source has PTS.
				import
					.timebase
					.anchor(Timestamp::from_micros(1_000_000).unwrap())
					.unwrap();
			} else if clock == "clock break" {
				import.decode(&pes_packet(PCR_PID, 90_000)).unwrap();
				import.decode(&clock_break_packet(PCR_PID)).unwrap();
			}
			import.decode(&pcr_packet(PCR_PID, 27_000_000)).unwrap();
			import.decode(&packet(true, 0, 0, &CUE)).unwrap();
			if clock == "none" {
				let err = import.finish().unwrap_err();
				assert!(err.to_string().contains("without a media timestamp"), "{clock}: {err}");
				assert_eq!(import.sections[&0x21].held.len(), 1);
			} else {
				import.finish().unwrap();
				assert!(import.sections[&0x21].held.is_empty());
				assert_eq!(import.stats().streams[&0x21].unplaced, 1, "{clock}: no PCR interval");
			}
			assert!(!import.published, "{clock}: no publication without an anchor");
		}
	}

	#[test]
	fn unanchored_sections_cannot_cross_a_clock_break() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(
				&[
					(StreamType::Mpeg2Video, PCR_PID),
					(StreamType::Dts8ChannelLosslessAudio, 0x21),
				],
				true,
			))
			.unwrap();
		import.decode(&pcr_packet(PCR_PID, 27_000_000)).unwrap();
		import.decode(&packet(true, 0, 0, &CUE)).unwrap();
		let err = import.decode(&clock_break_packet(PCR_PID)).unwrap_err();
		assert!(err.to_string().contains("clock break without a media timestamp"));
		assert_eq!(import.sections[&0x21].held.len(), 1);
		assert!(!import.published);
	}

	#[tokio::test(start_paused = true)]
	async fn duplicate_payload_clock_packet_declares_one_break() {
		let (consumer, catalog, mut import) = two_stream_import();
		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		let flagged = flag_discontinuity(mp2_pes(PCR_PID, 1, 180_000, [0xCC, 0xDD]));
		import.decode(flagged.as_slice()).unwrap();
		import.decode(flagged.as_slice()).unwrap();
		for pid in [PCR_PID, PEER_PID] {
			let cc = if pid == PCR_PID { 2 } else { 1 };
			import
				.decode(mp2_pes(pid, cc, 270_000, [0xEE, 0xFF]).as_slice())
				.unwrap();
		}
		import.finish().unwrap();
		for (_, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 1, "a retransmitted packet declared another break");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn incomplete_media_does_not_declare_another_break() {
		let (consumer, catalog, mut import) = two_stream_import();
		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		import
			.decode(audio_pes_packet(PEER_PID, 1, 180_000, &mp2_frame(0xCC)[..20]).as_slice())
			.unwrap();
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		for pid in [PCR_PID, PEER_PID] {
			let cc = if pid == PCR_PID { 1 } else { 2 };
			import
				.decode(mp2_pes(pid, cc, 270_000, [0xEE, 0xFF]).as_slice())
				.unwrap();
		}
		import.finish().unwrap();
		for (_, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 1, "an incomplete frame was counted as publication");
		}
	}

	/// The defect from #2833: a source that signals a timebase reset produced nothing on the
	/// exported wire, because the flag never left the demuxer. A `discontinuity_indicator` on
	/// the PCR PID is a *system* time-base break, so every track in the program takes one,
	/// including the peers carrying no flag of their own.
	#[tokio::test(start_paused = true)]
	async fn pcr_discontinuity_breaks_every_track() {
		let (consumer, catalog, mut import) = two_stream_import();

		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		// The encoder restarts: the clock declares the break and the media resumes 20 s
		// ahead. Under the track's 30 s retention window, so a subscribe still reaches
		// back to the first frame; the exported-clock tests cover the 30 s leap itself.
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		for pid in [PCR_PID, PEER_PID] {
			import
				.decode(mp2_pes(pid, 1, 90_000 + 20 * 90_000, [0xCC, 0xDD]).as_slice())
				.unwrap();
		}
		import.finish().unwrap();

		for (frames, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 1, "the timebase break did not reach this track");
			let fills: Vec<u8> = frames.iter().map(|f| f.payload[4]).collect();
			assert_eq!(fills, [0xAA, 0xBB, 0xCC, 0xDD], "media either side of the break");
			assert_eq!(frames[2].timestamp.as_micros(), 21_000_000, "the new timeline");
		}
	}

	/// A backwards restart is the same signal, but a rewind: a new broadcast, so the import
	/// ends. The unwrapper must not read the step back as the 33-bit field wrapping, which
	/// would put the new timeline 26 hours out and publish it.
	#[tokio::test(start_paused = true)]
	async fn a_flagged_backwards_restart_is_refused() {
		let (_consumer, _catalog, mut import) = two_stream_import();

		for pid in [PCR_PID, PEER_PID] {
			import
				.decode(mp2_pes(pid, 0, 45 * 90_000, [0xAA, 0xBB]).as_slice())
				.unwrap();
		}
		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		let err = [PCR_PID, PEER_PID]
			.into_iter()
			.try_for_each(|pid| import.decode(mp2_pes(pid, 1, 90_000, [0xCC, 0xDD]).as_slice()))
			.and_then(|()| import.finish())
			.expect_err("a restart below the live edge is a new broadcast");
		assert!(is_rewind(&err), "{err:?}");
	}

	/// Adaptation stuffing leaves exactly these bytes in a TS payload.
	fn pes_fragment(pid: u16, pusi: bool, cc: u8, data: &[u8]) -> [u8; 188] {
		assert!(!data.is_empty() && data.len() <= 183);
		let mut packet = [0xff; 188];
		packet[..5].copy_from_slice(&[
			0x47,
			((pid >> 8) as u8 & 0x1f) | if pusi { 0x40 } else { 0 },
			pid as u8,
			0x30 | (cc & 15),
			(183 - data.len()) as u8,
		]);
		if packet[4] != 0 {
			packet[5] = 0;
		}
		packet[188 - data.len()..].copy_from_slice(data);
		packet
	}

	#[test]
	fn pes_headers_fragment_at_every_byte_boundary() {
		const PID: u16 = 0x50;
		for (optional, paired) in [(5, false), (10, true), (255, true)] {
			let mut header = vec![0, 0, 1, 0xe0, 0, 0, 0x80, if paired { 0xc0 } else { 0x80 }, optional];
			header.extend_from_slice(&pes_timestamp(if paired { 3 } else { 2 }, 90_000));
			if paired {
				header.extend_from_slice(&pes_timestamp(1, 87_000));
			}
			header.resize(9 + usize::from(optional), 0xff);
			let data = [0xde, 0xad, 0xbe, 0xef];
			for split in 1..header.len() {
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
				let mut import = super::Import::new(broadcast, catalog.reserve());
				let pid = mpeg2ts::ts::Pid::new(PID).unwrap();
				import.streams.insert(pid, super::Stream::Ignored);
				let mut cc = 0;
				for part in [&header[..split], &header[split..], &data] {
					for chunk in part.chunks(183) {
						import
							.pes_packet(pid, &pes_fragment(PID, cc == 0, cc, chunk), 0)
							.unwrap();
						cc += 1;
					}
				}
				let pending = import.pending.get(&pid).expect("a split clear PES is retained");
				assert_eq!(pending.pts, Some(90_000), "optional={optional} split={split}");
				assert_eq!(pending.dts, paired.then_some(87_000));
				assert_eq!(pending.data, data, "optional={optional} split={split}");
			}
		}
	}

	fn ignored_pes_import() -> super::Import {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		for raw in [0x50, 0x51] {
			import.streams.insert(Pid::new(raw).unwrap(), super::Stream::Ignored);
		}
		import
	}

	#[test]
	fn pes_fragmented_header_resets_on_loss_new_start_and_eof() {
		let pid = Pid::new(0x50).unwrap();
		let header = [0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
		for end in ["loss", "start", "eof"] {
			let mut import = ignored_pes_import();
			import
				.pes_packet(pid, &pes_fragment(0x50, true, 0, &header), 0)
				.unwrap();
			assert_eq!(import.headers[&pid].1.len(), 9);
			match end {
				"loss" => import.broken(pid).unwrap(),
				"start" => import
					.pes_packet(
						pid,
						&pes_fragment(0x50, true, 1, &[0, 0, 1, 0xe0, 0, 0, 0x80, 0, 0, 0xaa]),
						0,
					)
					.unwrap(),
				_ => import.finish().unwrap(),
			}
			assert!(!import.headers.contains_key(&pid));
			if end == "start" {
				assert_eq!(import.pending[&pid].data, [0xaa]);
			} else {
				import
					.pes_packet(pid, &pes_fragment(0x50, false, 1, &[0xff; 5]), 0)
					.unwrap();
				assert!(
					!import.pending.contains_key(&pid),
					"orphan bytes cannot complete the lost header"
				);
			}
		}
	}

	/// Reading a file faster than real time must not choose wraps from the host clock.
	#[tokio::test(start_paused = true)]
	async fn a_fast_import_follows_pts_history_past_half_a_wrap() {
		let (_, catalog, mut import) = two_stream_import();
		for i in 0..64 {
			let pts = i * 900 * 90_000;
			import
				.decode(&mp2_pes(PCR_PID, i as u8, pts % (1 << 33), [1, 2]))
				.unwrap();
		}
		import.finish().unwrap();
		assert!(!catalog.snapshot().audio.renditions.is_empty());
	}

	/// A flagged jump that carries the source forward across a wrap continues its timeline: the
	/// PTS before the break, not the raw value after it, picks the wrap.
	#[tokio::test(start_paused = true)]
	async fn a_flagged_jump_across_a_wrap_carries_on() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		// Retained for a day, so a subscribe reaches every frame on both sides of the jump.
		let config = crate::catalog::Config::default().with_max_age(Duration::from_secs(86_400));
		let catalog = crate::catalog::Producer::new(&mut broadcast, config).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt = synth_pmt(
			&[(StreamType::Mpeg1Audio, PCR_PID), (StreamType::Mpeg1Audio, PEER_PID)],
			false,
		);
		import.decode(&bytes::BytesMut::from(&pmt[..])).unwrap();
		// Five minutes short of the wrap, then ten minutes on.
		let first = WRAP - 300 * 90_000;
		let jump = 600 * 90_000;
		for i in 0..20 {
			let mut data = Vec::new();
			if i == 10 {
				data.extend(clock_break_packet(PCR_PID));
			}
			let pts = first + i * STEP + if i >= 10 { jump } else { 0 };
			data.extend(source_pes(i, pts % WRAP));
			import.decode(&data).unwrap();
			tokio::time::advance(Duration::from_millis(144)).await;
		}
		import.finish().unwrap();
		let all = timeline(&consumer, &catalog).await;
		let step = all[&21] - all[&19];
		assert!(
			(600_143_999..=600_144_001).contains(&step),
			"the jump adds its step: {step} us"
		);
	}

	/// An import started just past a wrap, with the peer still before it, steps the peer back into
	/// the wrap before rather than a whole wrap ahead, and its PES before the clock's zero are dropped.
	#[tokio::test(start_paused = true)]
	async fn a_stream_behind_the_first_wrap_steps_back() {
		let (consumer, catalog, mut import) = two_stream_import();
		for i in 0..10 {
			import.decode(&source_pes(i, 4_000 + i * STEP)).unwrap();
			tokio::time::advance(Duration::from_millis(144)).await;
		}
		import.finish().unwrap();
		assert_eq!(import.stats().streams[&PEER_PID].unplaced, 1);
		let all = timeline(&consumer, &catalog).await;
		assert!(!all.contains_key(&0x80), "the peer's PES before the wrap is dropped");
		// The peer keeps its place, 8,000 ticks behind the PCR PID.
		assert_eq!(all[&0x82], (4_000 + STEP - 8_000) as i64 * 1_000_000 / 90_000);
		assert!(
			all.values().all(|&us| us < 2_000_000),
			"the timeline stays at the import's start: {all:?}"
		);
	}

	/// One step of the two-stream source: a PES on each stream, 144 ms of MP2.
	const STEP: u64 = 12_960;
	/// The 33-bit PTS field's period.
	const WRAP: u64 = 1 << 33;

	/// Step `i` of the two-stream source whose PCR PID presents at `pts`. The peer presents 8,000 ticks
	/// earlier, so near a wrap the two streams sit either side of it. Every frame has its own fill.
	fn source_pes(i: u64, pts: u64) -> Vec<u8> {
		let n = i as u8;
		let mut data = mp2_pes(PCR_PID, n, pts, [2 * n + 1, 2 * n + 2]);
		data.extend(mp2_pes(
			PEER_PID,
			n,
			(pts + WRAP - 8_000) % WRAP,
			[0x80 + 2 * n, 0x81 + 2 * n],
		));
		data
	}

	/// Every frame `catalog`'s renditions published, by fill, at its timestamp in microseconds.
	async fn timeline(
		consumer: &moq_net::broadcast::Consumer,
		catalog: &crate::catalog::Producer,
	) -> BTreeMap<u8, i64> {
		let mut frames = BTreeMap::new();
		for name in catalog.snapshot().audio.renditions.keys() {
			for frame in read_track(consumer, name, crate::container::Kind::Audio).await {
				frames.insert(frame.payload[4], frame.timestamp.as_micros() as i64);
			}
		}
		frames
	}

	#[test]
	fn pes_header_duplicate_and_discontinuity_follow_the_packet_chain() {
		let pid = Pid::new(0x50).unwrap();
		let mut header = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
		header.extend_from_slice(&pes_timestamp(2, 90_000));
		header.extend_from_slice(&[0xde, 0xad]);
		for damage in ["duplicate", "loss", "discontinuity"] {
			let mut import = ignored_pes_import();
			import.streams.remove(&pid);
			import.ensure_stream(pid, StreamType::AdtsAac as u8, &[]).unwrap();
			let first = pes_fragment(0x50, true, 0, &header[..7]);
			import.decode(&first).unwrap();
			if damage == "duplicate" {
				import.decode(&first).unwrap();
			}
			let mut next = pes_fragment(0x50, false, if damage == "loss" { 2 } else { 1 }, &header[7..]);
			if damage == "discontinuity" {
				next[5] = 0x80;
			}
			import.decode(&next).unwrap();
			if damage == "duplicate" {
				assert_eq!(import.pending[&pid].pts, Some(90_000));
				assert_eq!(import.pending[&pid].data, [0xde, 0xad]);
			} else {
				assert!(!import.pending.contains_key(&pid));
			}
		}
	}

	#[test]
	fn pes_special_stream_ids_assemble_without_optional_fields() {
		let pid = Pid::new(0x50).unwrap();
		for id in [0xbc, 0xbe, 0xbf, 0xf0, 0xf1, 0xf2, 0xf8, 0xff] {
			for split in 1..6 {
				let mut import = ignored_pes_import();
				let header = [0, 0, 1, id, 0, 3];
				import
					.pes_packet(pid, &pes_fragment(0x50, true, 0, &header[..split]), 0)
					.unwrap();
				import
					.pes_packet(pid, &pes_fragment(0x50, false, 1, &header[split..]), 0)
					.unwrap();
				assert_eq!(import.pending[&pid].data_len, Some(3));
				assert_eq!(import.pending[&pid].pts, None);
			}
		}
		assert_eq!(
			super::PesStart::parse(&[0, 0, 1, 0xbe, 0, 0]).unwrap().data_len,
			Some(0)
		);
	}

	#[test]
	fn malformed_pes_prefix_is_refused_without_waiting_for_more_bytes() {
		let pid = Pid::new(0x50).unwrap();
		for bad in [&[1][..], &[0, 1][..], &[0, 0, 2][..]] {
			let mut import = ignored_pes_import();
			import.pes_packet(pid, &pes_fragment(0x50, true, 0, bad), 0).unwrap();
			assert!(import.headers.is_empty());
			assert!(import.pending.is_empty());
		}
	}

	#[test]
	fn pes_length_overruns_fail_before_waiting_for_the_optional_header() {
		let pid = Pid::new(0x50).unwrap();
		let mut import = ignored_pes_import();
		import
			.pes_packet(
				pid,
				&pes_fragment(0x50, true, 0, &[0, 0, 1, 0xe0, 0, 4, 0x80, 0x80, 255]),
				0,
			)
			.unwrap();
		assert!(import.headers.is_empty());
		assert!(import.pending.is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn a_short_declared_video_pes_is_refused_at_eof_or_the_next_start() {
		let pid = Pid::new(0x50).unwrap();
		for end in ["eof", "start"] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve());
			import.ensure_stream(pid, StreamType::H264 as u8, &[]).unwrap();
			let data = annexb_au(true);
			let len = 8 + data.len() + 1;
			let mut pes = vec![0, 0, 1, 0xe0, (len >> 8) as u8, len as u8, 0x80, 0x80, 5];
			pes.extend_from_slice(&pes_timestamp(2, 90_000));
			pes.extend_from_slice(&data);
			for (cc, chunk) in pes.chunks(183).enumerate() {
				import
					.pes_packet(pid, &pes_fragment(0x50, cc == 0, cc as u8, chunk), 0)
					.unwrap();
			}
			if end == "eof" {
				import.finish().unwrap();
			} else {
				let mut header = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
				header.extend_from_slice(&pes_timestamp(2, 180_000));
				import
					.pes_packet(pid, &pes_fragment(0x50, true, 1, &header), 0)
					.unwrap();
			}
			assert_eq!(
				import.stats().streams[&0x50].units,
				0,
				"a missing byte invalidates the declared access unit at {end}"
			);
			assert_eq!(import.stats().streams[&0x50].damaged, 1);
		}
	}

	#[test]
	fn pes_retention_budget_is_shared_and_released_on_damage() {
		let mut import = ignored_pes_import();
		let first = Pid::new(0x50).unwrap();
		let second = Pid::new(0x51).unwrap();
		for pid in [first, second] {
			import
				.pes_packet(
					pid,
					&pes_fragment(pid.as_u16(), true, 0, &[0, 0, 1, 0xe0, 0, 0, 0x80, 0, 0]),
					0,
				)
				.unwrap();
			import
				.handle_pes_continuation(pid, &vec![0; super::STAGING_BYTES_MAX / 2])
				.unwrap();
		}
		assert_eq!(
			import.staging.bytes.load(super::Ordering::Relaxed),
			super::STAGING_BYTES_MAX
		);
		assert!(
			import
				.handle_pes_continuation(first, &[1])
				.unwrap_err()
				.to_string()
				.contains("staging exceeds")
		);
		assert_eq!(import.pending[&first].data.len(), super::STAGING_BYTES_MAX / 2);
		import.damage(first, &anyhow::anyhow!("loss")).unwrap();
		assert_eq!(
			import.staging.bytes.load(super::Ordering::Relaxed),
			super::STAGING_BYTES_MAX / 2
		);
		import.handle_pes_continuation(second, &[1]).unwrap();
		drop(import.pending.remove(&second));
		assert_eq!(import.staging.bytes.load(super::Ordering::Relaxed), 0);
	}

	#[test]
	fn untimed_sections_stop_at_the_shared_budget_and_release_after_a_clock() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(
				&[
					(StreamType::AdtsAac, 0x50),
					(StreamType::Dts8ChannelLosslessAudio, 0x21),
				],
				true,
			))
			.unwrap();
		import.decode(&pcr_packet(0x50, 27_000_000)).unwrap();
		for i in 0..super::STAGING_UNITS_MAX {
			import.decode(&packet(true, (i % 16) as u8, 0, &CUE)).unwrap();
		}
		let err = import
			.decode(&packet(true, (super::STAGING_UNITS_MAX % 16) as u8, 0, &CUE))
			.unwrap_err();
		assert!(err.to_string().contains(&format!(
			"TS staging exceeds {} buffered units",
			super::STAGING_UNITS_MAX
		)));
		assert_eq!(import.sections[&0x21].held.len(), super::STAGING_UNITS_MAX);
		assert_eq!(
			import.staging.bytes.load(super::Ordering::Relaxed),
			CUE.len() * super::STAGING_UNITS_MAX
		);
		import.decode(&pes_packet(0x50, 90_000)).unwrap();
		import.decode(&pcr_packet(0x50, 29_700_000)).unwrap();
		assert!(import.sections[&0x21].held.is_empty());
		assert_eq!(import.staging.bytes.load(super::Ordering::Relaxed), 0);
		assert_eq!(import.staging.units.load(super::Ordering::Relaxed), 0);
	}

	#[test]
	fn section_retention_budget_counts_units_and_rolls_back_failed_reservations() {
		let staging = std::sync::Arc::<super::Staging>::default();
		let mut held = super::Retained::new(staging.clone());
		held.grow(10, super::STAGING_UNITS_MAX).unwrap();
		assert!(held.grow(100, 1).is_err());
		assert_eq!(staging.bytes.load(super::Ordering::Relaxed), 10);
		assert_eq!(staging.units.load(super::Ordering::Relaxed), super::STAGING_UNITS_MAX);
		held.clear();
		assert_eq!(staging.bytes.load(super::Ordering::Relaxed), 0);
		assert_eq!(staging.units.load(super::Ordering::Relaxed), 0);
	}

	#[test]
	#[ignore = "benchmark"]
	fn benchmark_pes_staging() {
		println!("pids,payload_bytes,fragmented,ns_per_pes");
		for pids in [1u16, 8, 64] {
			for size in [184, 16 * 1024, 256 * 1024] {
				for fragmented in [false, true] {
					let mut import = ignored_pes_import();
					for raw in 0x50..0x50 + pids {
						import.streams.insert(Pid::new(raw).unwrap(), super::Stream::Ignored);
					}
					let data = vec![0xaa; size];
					let header = [0, 0, 1, 0xe0, 0, 0, 0x80, 0, 0];
					let start = std::time::Instant::now();
					for _ in 0..16 {
						for raw in 0x50..0x50 + pids {
							let pid = Pid::new(raw).unwrap();
							if fragmented {
								import
									.pes_packet(pid, &pes_fragment(raw, true, 0, &header[..1]), 0)
									.unwrap();
								import
									.pes_packet(pid, &pes_fragment(raw, false, 1, &header[1..]), 0)
									.unwrap();
							} else {
								import.pes_packet(pid, &pes_fragment(raw, true, 0, &header), 0).unwrap();
							}
							for chunk in data.chunks(184) {
								import
									.handle_pes_continuation(pid, std::hint::black_box(chunk))
									.unwrap();
							}
							import.flush(pid).unwrap();
						}
					}
					println!(
						"{pids},{size},{fragmented},{}",
						start.elapsed().as_nanos() / (16 * u128::from(pids))
					);
					assert_eq!(import.staging.bytes.load(super::Ordering::Relaxed), 0);
				}
			}
		}
	}

	/// A stream a PMT revision adds after the program's clock wrapped takes the clock's era, not
	/// the era its first value would start alone.
	#[tokio::test(start_paused = true)]
	async fn a_stream_added_after_a_wrap_joins_the_wrap_the_clock_is_in() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let (mut pat_cc, mut pmt_cc) = (0, 0);
		let mut data = psi_packets(0, &mut pat_cc, &[], &pat_section(&[(1, 0x0100)]));
		data.extend(psi_packets(
			0x0100,
			&mut pmt_cc,
			&[],
			&pmt_section(1, 0, &[(MP2, PCR_PID, &[])]),
		));
		import.decode(&data).unwrap();

		let first = WRAP - 10 * STEP;
		for i in 0..20 {
			if i == 15 {
				let both = [(MP2, PCR_PID, &[][..]), (MP2, PEER_PID, &[][..])];
				let revision = psi_packets(0x0100, &mut pmt_cc, &[], &pmt_section(1, 1, &both));
				import.decode(&revision).unwrap();
			}
			let mut data = source_pes(i, (first + i * STEP) % WRAP);
			if i < 15 {
				data.truncate(188);
			}
			import.decode(&data).unwrap();
			tokio::time::advance(Duration::from_millis(144)).await;
		}
		import.finish().unwrap();

		let frames = timeline(&consumer, &catalog).await;
		let apart = frames[&31] - frames[&0x9e];
		assert!(
			(88_888..=88_889).contains(&apart),
			"the step's frames sit {apart} µs apart"
		);
	}

	/// A 5-byte PES timestamp field with the 4-bit `prefix` (0b0010 PTS only, 0b0011 PTS of a
	/// pair, 0b0001 DTS).
	fn pes_timestamp(prefix: u8, t: u64) -> [u8; 5] {
		[
			(prefix << 4) | (((t >> 30) & 0x07) << 1) as u8 | 0x01,
			((t >> 22) & 0xff) as u8,
			0x01 | (((t >> 15) & 0x7f) << 1) as u8,
			((t >> 7) & 0xff) as u8,
			0x01 | ((t & 0x7f) << 1) as u8,
		]
	}

	/// A PUSI TS packet on `pid` carrying one bounded video PES (stream_id 0xE0) with `pts`
	/// and, when it differs, `dts`, sized to complete on this packet.
	pub(in crate::container::ts) fn video_pes(pid: u16, cc: u8, pts: u64, dts: Option<u64>, payload: &[u8]) -> Vec<u8> {
		let mut header = Vec::new();
		match dts {
			Some(dts) => {
				header.extend_from_slice(&[0x80, 0xC0, 10]);
				header.extend_from_slice(&pes_timestamp(0b0011, pts));
				header.extend_from_slice(&pes_timestamp(0b0001, dts));
			}
			None => {
				header.extend_from_slice(&[0x80, 0x80, 5]);
				header.extend_from_slice(&pes_timestamp(0b0010, pts));
			}
		}
		let mut pes = vec![0x00, 0x00, 0x01, 0xe0];
		let pes_len = header.len() + payload.len();
		pes.push((pes_len >> 8) as u8);
		pes.push((pes_len & 0xff) as u8);
		pes.extend_from_slice(&header);
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
		assert_eq!(p.len(), 188, "video PES packet must fill exactly one TS packet");
		p
	}

	/// One 30 fps frame in 90 kHz ticks.
	const FRAME: u64 = 3_000;

	/// Appends TS packets to `out`, counting the continuity counter per PID.
	#[derive(Default)]
	struct Mux {
		out: Vec<u8>,
		cc: std::collections::HashMap<u16, u8>,
	}

	impl Mux {
		fn cc(&mut self, pid: u16) -> u8 {
			let cc = self.cc.entry(pid).or_default();
			let current = *cc;
			*cc = (*cc + 1) & 0x0f;
			current
		}

		/// One four-frame closed GOP in decode order, I P B B, presenting `base + FRAME` to
		/// `base + 4 * FRAME`: the reference frames carry a DTS, the B-frames present as they
		/// decode.
		fn gop(&mut self, pid: u16, base: u64) {
			let frames = [
				(true, base + FRAME, Some(base)),
				(false, base + 4 * FRAME, Some(base + FRAME)),
				(false, base + 2 * FRAME, None),
				(false, base + 3 * FRAME, None),
			];
			for (keyframe, pts, dts) in frames {
				let cc = self.cc(pid);
				self.out
					.extend_from_slice(&video_pes(pid, cc, pts, dts, &annexb_au(keyframe)));
			}
		}

		/// `count` GOPs back to back from `base`.
		fn gops(&mut self, pid: u16, base: u64, count: u64) {
			for i in 0..count {
				self.gop(pid, base + i * 4 * FRAME);
			}
		}
	}

	/// Read every retained frame of `name` as `kind`, skipping the empty break markers.
	async fn read_track(
		consumer: &moq_net::broadcast::Consumer,
		name: &str,
		kind: crate::container::Kind,
	) -> Vec<crate::container::Frame> {
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(3600));
		let track = consumer.track(name).unwrap().subscribe(subscription).await.unwrap();
		let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy(kind));
		let mut frames = Vec::new();
		while let Ok(Ok(Some(frame))) = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await {
			if !frame.payload.is_empty() {
				frames.push(frame);
			}
		}
		frames
	}

	/// Import `data` with an `mpegts` catalog, failing on any import error. The importer is
	/// returned to keep its renditions in the catalog.
	fn import_all(
		data: &[u8],
	) -> anyhow::Result<(
		moq_net::broadcast::Consumer,
		crate::catalog::Producer<Ext>,
		super::Import<Ext>,
	)> {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(data)?;
		import.finish()?;
		Ok((consumer, catalog, import))
	}

	use crate::container::ts::catalog::Ext;

	/// Whether `err` is a refused timestamp rewind: the source restarted, which is a new
	/// broadcast rather than a continuation.
	fn is_rewind(err: &anyhow::Error) -> bool {
		err.chain().any(|cause| {
			matches!(
				cause.downcast_ref::<crate::Error>(),
				Some(crate::Error::TimestampRewind(_))
			)
		})
	}

	/// Import `data`, which must end in a refused rewind.
	fn import_refused(data: &[u8]) {
		let Err(err) = import_all(data) else {
			panic!("the import accepted a rewind");
		};
		assert!(is_rewind(&err), "{err:?}");
	}

	/// Group starts never go backwards, and no frame sits below the start of the group before
	/// its own, which is what the producer demands. Frames may still present below the previous
	/// group's content (B-frames, an overlapping keyframe).
	fn assert_forward(frames: &[crate::container::Frame]) {
		let (mut start, mut floor) = (None, None);
		for frame in frames {
			let ts = frame.timestamp.as_micros();
			if frame.keyframe {
				assert!(
					start.is_none_or(|start| ts >= start),
					"a group started at {ts} before {start:?}"
				);
				floor = start;
				start = Some(ts);
			}
			assert!(floor.is_none_or(|floor| ts >= floor), "rewound to {ts} below {floor:?}");
		}
	}

	async fn video_frames(data: &[u8]) -> Vec<crate::container::Frame> {
		let (consumer, catalog, _import) = import_all(data).expect("the import must survive the step back");
		let name = catalog.snapshot().video.renditions.keys().next().unwrap().clone();
		read_track(&consumer, &name, crate::container::Kind::Video).await
	}

	const VIDEO: u16 = 0x0050;

	#[tokio::test(start_paused = true)]
	async fn video_pes_continuation_keeps_the_access_unit_timestamp() {
		let au = annexb_au(true);
		// Split inside the IDR, so neither PES is an independently decodable picture.
		let cut = au.len() - 2;
		let mut data = synth_pmt(&[(StreamType::H264, VIDEO)], false);
		data.extend(video_pes(VIDEO, 0, FRAME, None, &au[..cut]));
		data.extend(video_pes(VIDEO, 1, FRAME * 9, None, &au[cut..]));
		let frames = video_frames(&data).await;
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].payload.as_ref(), au);
		assert_eq!(frames[0].timestamp.as_micros(), u128::from(FRAME) * 1_000_000 / 90_000);
	}

	#[test]
	fn video_pes_does_not_reuse_its_pts_for_an_untimestampable_successor() {
		let mut payload = annexb_au(true);
		payload.extend(annexb_au(false));
		let mut data = synth_pmt(&[(StreamType::H264, VIDEO)], false);
		data.extend(video_pes(VIDEO, 0, FRAME, None, &payload));
		let err = match import_all(&data) {
			Ok(_) => panic!("a second AU cannot reuse the first AU's PES PTS"),
			Err(err) => err,
		};
		assert!(!err.is::<super::Damaged>(), "timing refusal must end the import");
		assert!(err.to_string().contains("timestamp"), "{err:#}");
	}

	/// Repacketize actual encoder-produced POCs, retaining an independent PTS
	/// oracle from its original TS rather than deriving the expected values.
	#[tokio::test(start_paused = true)]
	async fn video_poc_repartition_preserves_frames_timestamps_and_export() {
		use crate::container::ts::fixture;
		use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
		use mpeg2ts::ts::TsPacketReader;
		use std::io::Cursor;
		for (kind, fixture) in [
			(StreamType::H264, &include_bytes!("test_data/poc-avc.ts")[..]),
			(StreamType::H265, &include_bytes!("test_data/poc-hevc.ts")[..]),
		] {
			let mut reader = PesPacketReader::new(TsPacketReader::new(Cursor::new(fixture)));
			let mut pictures = Vec::new();
			while let Some(pes) = reader.read_pes_packet().unwrap() {
				pictures.push((pes.header.pts.unwrap().as_u64(), pes.data));
			}
			assert_eq!(pictures.len(), 12);
			let mut baseline = None;
			for layout in ["single", "combined", "split", "no-clock"] {
				if layout == "no-clock" && kind != StreamType::H264 {
					continue;
				}
				let mut mux = fixture::Mux::default();
				mux.pat(&[(1, 0x1000)]);
				// fixed_frame_rate_flag + temporal_poc_flag. The AVC VUI alone
				// deliberately does not authorize multiplying POC by a clock tick.
				let descriptor = if kind == StreamType::H264 && layout != "no-clock" {
					&[0x2a, 2, 0x7e, 0xdf][..]
				} else {
					&[]
				};
				mux.pmt_with(0x1000, 1, VIDEO, &[(kind as u8, VIDEO, descriptor)], 0);
				let push = |mux: &mut fixture::Mux, pts: Option<u64>, payload: &[u8]| {
					let header_len = if pts.is_some() { 8 } else { 3 };
					let len = header_len + payload.len();
					let mut pes = vec![
						0,
						0,
						1,
						0xe0,
						(len >> 8) as u8,
						len as u8,
						0x80,
						if pts.is_some() { 0x80 } else { 0 },
						if pts.is_some() { 5 } else { 0 },
					];
					if let Some(pts) = pts {
						pes.extend(pes_timestamp(2, pts));
					}
					pes.extend(payload);
					mux.whole(VIDEO, &pes);
				};
				match layout {
					"single" => {
						for (pts, data) in &pictures {
							push(&mut mux, Some(*pts), data);
						}
					}
					"combined" | "no-clock" => {
						for chunk in pictures.chunks(4) {
							let data: Vec<u8> = chunk.iter().flat_map(|(_, data)| data.iter().copied()).collect();
							push(&mut mux, Some(chunk[0].0), &data);
						}
					}
					"split" => {
						for (pts, data) in &pictures {
							// All three required prefix bytes and the final slice body
							// are exercised on opposite sides of a PES boundary.
							for (n, chunk) in data.chunks(2).enumerate() {
								push(&mut mux, if n == 0 { Some(*pts) } else { None }, chunk);
							}
						}
					}
					_ => unreachable!(),
				}
				if layout == "no-clock" {
					// Valid AVC with fixed-rate SPS VUI still needs the TS
					// temporal-POC promise before numeric POC can mean time.
					let err = match import_all(&mux.out) {
						Ok(_) => panic!("SPS fixed frame rate alone cannot timestamp a later AU"),
						Err(err) => err,
					};
					assert!(!err.is::<super::Damaged>());
					assert!(err.to_string().contains("proportional"), "{err:#}");
					continue;
				}
				let (consumer, catalog, _import) =
					import_all(&mux.out).unwrap_or_else(|err| panic!("{kind:?} {layout}: {err:#}"));
				let name = catalog.snapshot().video.renditions.keys().next().unwrap().clone();
				let frames = read_track(&consumer, &name, crate::container::Kind::Video).await;
				let observed: Vec<_> = frames
					.iter()
					.map(|f| (f.timestamp.as_micros(), f.keyframe, f.payload.clone()))
					.collect();
				assert_eq!(frames.len(), pictures.len(), "{kind:?} {layout}");
				assert_eq!(
					frames.iter().map(|f| f.timestamp.as_micros()).collect::<Vec<_>>(),
					pictures
						.iter()
						.map(|(pts, _)| u128::from(*pts) * 1_000_000 / 90_000)
						.collect::<Vec<_>>(),
					"{kind:?} {layout}"
				);
				match &baseline {
					None => baseline = Some(observed.clone()),
					Some(expected) => assert_eq!(&observed, expected, "{kind:?} {layout}"),
				}
				let mut export =
					crate::container::ts::export_test::open_export(crate::source::announced(&consumer).await)
						.await
						.unwrap()
						.with_delay(Duration::from_secs(30))
						.with_replay();
				let mut ts = Vec::new();
				while let Ok(frame) = tokio::time::timeout(Duration::from_secs(100), export.next()).await {
					let Some(frame) = frame.unwrap() else {
						break;
					};
					ts.extend(frame.payload);
				}
				let roundtrip = video_frames(&ts).await;
				assert_eq!(
					roundtrip
						.iter()
						.map(|f| (f.timestamp.as_micros(), f.keyframe, f.payload.clone()))
						.collect::<Vec<_>>(),
					observed,
					"export {kind:?} {layout}"
				);
			}
		}
	}

	/// A PMT listing one PID twice routes it by the later entry, as a newer PMT would.
	#[tokio::test(start_paused = true)]
	async fn a_pmt_listing_a_pid_twice_routes_by_the_last_entry() {
		use crate::container::test_util::{IDR, PPS, SPS};
		let mut payload = Vec::new();
		for nal in [SPS, PPS, IDR] {
			payload.extend([0, 0, 0, 1]);
			payload.extend(nal);
		}
		let mut data = synth_pmt(&[(StreamType::AdtsAac, VIDEO), (StreamType::H264, VIDEO)], false);
		data.extend(video_pes(VIDEO, 0, 90_000, None, &payload));
		let (consumer, catalog, _import) = import_all(&data).unwrap();
		let snapshot = catalog.snapshot();
		assert!(snapshot.audio.renditions.is_empty());
		let name = snapshot.video.renditions.keys().next().unwrap().clone();
		assert_eq!(
			read_track(&consumer, &name, crate::container::Kind::Video).await.len(),
			1
		);
	}

	#[tokio::test(start_paused = true)]
	async fn video_parameter_sets_commit_per_au_instead_of_per_pes() {
		use crate::container::test_util::{IDR, PPS, SPS};
		let extra_pps = [0x68, 0x4e, 0x3c, 0x80];
		let mut payload = Vec::new();
		for nal in [SPS, PPS, &extra_pps, IDR, &[0x67, 0x42], &[0x65, 0x80]] {
			payload.extend([0, 0, 0, 1]);
			payload.extend(nal);
		}
		let mut data = synth_pmt(&[(StreamType::H264, VIDEO)], false);
		data.extend(video_pes(VIDEO, 0, 90_000, None, &payload));
		// A later keyframe proves the damaged AU ended and must recover with
		// the last committed parameter sets, including the extra PPS above.
		let mut bare = vec![0, 0, 0, 1];
		bare.extend(IDR);
		data.extend(video_pes(VIDEO, 1, 180_000, None, &bare));
		let (consumer, catalog, import) = import_all(&data).unwrap();
		let name = catalog.snapshot().video.renditions.keys().next().unwrap().clone();
		let frames = read_track(&consumer, &name, crate::container::Kind::Video).await;
		assert_eq!(frames.len(), 2);
		assert_eq!(import.stats().streams[&VIDEO].damaged, 1);
		assert_eq!(import.stats().streams[&VIDEO].units, 2);
		for frame in &frames {
			assert!(frame.payload.windows(extra_pps.len()).any(|nal| nal == extra_pps));
		}
	}

	#[test]
	fn video_damage_in_the_final_access_unit_is_counted() {
		let mut data = synth_pmt(&[(StreamType::H264, VIDEO)], false);
		data.extend(video_pes(VIDEO, 0, 90_000, None, &annexb_au(true)));
		// Only end of input ends this AU, so finish() refuses it: its truncated
		// SPS assembles but fails to parse.
		let au = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x80];
		data.extend(video_pes(VIDEO, 1, 90_000 + FRAME, None, &au));
		let (_consumer, _catalog, import) = import_all(&data).unwrap();
		assert_eq!(import.stats().streams[&VIDEO].units, 1);
		assert_eq!(import.stats().streams[&VIDEO].damaged, 1);
	}

	/// Damage on one PID refuses that unit and its dependent pictures while another PID
	/// carries on. A later decode call resumes from the next keyframe.
	async fn damaged_video_recovers(damage: &str) {
		let peer = VIDEO + 1;
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO), (StreamType::H264, peer)], false),
			..Default::default()
		};
		mux.gop(VIDEO, 90_000);
		if matches!(damage, "pes" | "adaptation" | "adaptation-clock" | "tei") {
			// These faults hide the next PES entirely. An AUD received ahead of
			// the fault proves the preceding picture complete; the AUD's own
			// picture remains unconfirmed and is discarded at the loss.
			let mut au = annexb_au(false);
			au.extend([0, 0, 1, 9, 0xf0]);
			let last = mux.out.len() - 188;
			mux.out[last..].copy_from_slice(&video_pes(VIDEO, 3, 90_000 + 3 * FRAME, None, &au));
		}
		mux.gop(peer, 90_000);
		let damaged_at = mux.out.len();
		mux.gop(VIDEO, 90_000 + 4 * FRAME);
		mux.gop(peer, 90_000 + 4 * FRAME);
		let packet = &mut mux.out[damaged_at..damaged_at + TsPacket::SIZE];
		let payload_at = 5 + usize::from(packet[4]);
		match damage {
			"pes" => packet[payload_at + 6..payload_at + 19].fill(0),
			"nal" => {
				let nal = packet
					.windows(5)
					.position(|bytes| bytes[..4] == [0, 0, 0, 1] && bytes[4] & 0x1f == 5)
					.unwrap();
				packet[nal + 4] |= 0x80;
			}
			"late-nal" => {
				let mut au = annexb_au(true);
				au.extend_from_slice(&[0, 0, 0, 1, 0xe1, 0x80]);
				packet.copy_from_slice(&video_pes(VIDEO, 4, 90_000 + 5 * FRAME, Some(90_000 + 4 * FRAME), &au));
			}
			"queued" => {
				// The AUD closes a valid IDR before the forbidden NAL in the next
				// AU fails. That already-admitted IDR must remain committed.
				let mut au = annexb_au(true);
				au.extend_from_slice(&[0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0xe1, 0x80, 0, 0, 0, 1, 0x41, 0x9a]);
				packet.copy_from_slice(&video_pes(VIDEO, 4, 90_000 + 5 * FRAME, Some(90_000 + 4 * FRAME), &au));
			}
			"adaptation" => packet[4] = 255,
			"adaptation-clock" => {
				packet[4] = 1;
				packet[5] = 0x90;
			}
			"tei" => packet[1] |= 0x80,
			"clean" => {}
			_ => unreachable!(),
		}
		assert_eq!(
			packet[1] & 0x80 != 0,
			damage == "tei",
			"only flagged corruption sets TEI"
		);
		let resume_at = mux.out.len();
		mux.gop(VIDEO, 90_000 + 8 * FRAME);
		mux.gop(peer, 90_000 + 8 * FRAME);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&mux.out[..resume_at])
			.expect("damage must stay local to the unit");
		assert_eq!(import.demux.held(), 0, "every damaged packet must be consumed");
		assert!(
			!import.pending.contains_key(&Pid::new(VIDEO).unwrap()),
			"no damaged PES remains"
		);
		import
			.decode(&mux.out[resume_at..])
			.expect("next keyframe must recover");
		import.finish().unwrap();
		assert_eq!(import.stats().streams[&VIDEO].damaged, u64::from(damage != "clean"));
		assert_eq!(import.stats().streams[&peer].damaged, 0);
		for (pid, expected) in [
			(
				VIDEO,
				if damage == "clean" {
					12
				} else if damage == "queued" {
					9
				} else {
					8
				},
			),
			(peer, 12),
		] {
			let Stream::H264 { import: video, .. } = &import.streams[&Pid::new(pid).unwrap()] else {
				panic!("video route");
			};
			let frames = read_track(&consumer, video.name(), crate::container::Kind::Video).await;
			assert_eq!(frames.len(), expected, "published pictures on PID {pid}");
			if pid == VIDEO && damage != "clean" {
				let recovery = if damage == "queued" { 5 } else { 4 };
				if damage == "queued" {
					assert!(
						frames[4].keyframe,
						"the valid AU before the damaged tail remains committed"
					);
				}
				assert!(frames[recovery].keyframe, "recovery starts at a keyframe");
				assert_eq!(
					frames[recovery].timestamp.as_micros(),
					u128::from(90_000 + 9 * FRAME) * 1_000_000 / 90_000
				);
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_pes_header_recovers() {
		damaged_video_recovers("pes").await;
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_h264_nal_recovers() {
		damaged_video_recovers("nal").await;
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_trailing_nal_refuses_the_whole_access_unit() {
		damaged_video_recovers("late-nal").await;
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_tail_preserves_an_access_unit_already_admitted() {
		damaged_video_recovers("queued").await;
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_adaptation_recovers() {
		damaged_video_recovers("adaptation").await;
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_adaptation_cannot_reset_the_program_clock() {
		damaged_video_recovers("adaptation-clock").await;
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_tei_recovers() {
		damaged_video_recovers("tei").await;
	}

	#[tokio::test(start_paused = true)]
	async fn clean_video_has_no_damage() {
		damaged_video_recovers("clean").await;
	}

	/// A keyframe refused for a malformed inline SPS must not replace the splitter's last good
	/// parameter sets: a later bare keyframe re-injects those and recovers.
	async fn damaged_sps_keeps_the_last_good_parameter_sets(stream_type: StreamType) {
		let (params, idr, delta): (&[&[u8]], &[u8], &[u8]) = match stream_type {
			StreamType::H264 => {
				use crate::container::test_util::{IDR, PPS, SPS};
				(&[SPS, PPS], IDR, &[0x41, 0x9a, 0x00, 0x01])
			}
			StreamType::H265 => {
				use crate::codec::h265::fixtures::{PPS, SPS, VPS};
				(&[VPS, SPS, PPS], &[0x26, 0x01, 0x80, 0xaa], &[0x02, 0x01, 0x80, 0x33])
			}
			_ => unreachable!(),
		};
		let good_sps = params[params.len() - 2];
		// The SPS keeps its NAL header and loses the rest.
		let truncated = &good_sps[..2];
		let au = |nals: &[&[u8]]| {
			let mut out = Vec::new();
			for nal in nals {
				out.extend_from_slice(&[0, 0, 0, 1]);
				out.extend_from_slice(nal);
			}
			out
		};
		let mut keyframe = params.to_vec();
		keyframe.push(idr);
		let mut damaged = params.to_vec();
		damaged[params.len() - 2] = truncated;
		damaged.push(idr);
		let units = [
			au(&keyframe),
			au(&[delta]),
			au(&damaged),
			au(&[delta]),
			au(&[idr]),
			au(&[delta]),
		];
		let mut data = synth_pmt(&[(stream_type, VIDEO)], false);
		for (cc, unit) in units.iter().enumerate() {
			let pts = 90_000 + cc as u64 * FRAME;
			data.extend(video_pes(VIDEO, cc as u8, pts, None, unit));
		}

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(&data).expect("a damaged SPS stays local to its unit");
		import.finish().unwrap();
		let name = catalog.snapshot().video.renditions.keys().next().unwrap().clone();
		let frames = read_track(&consumer, &name, crate::container::Kind::Video).await;
		assert_eq!(frames.len(), 4, "the bare keyframe after the damage recovers");
		assert!(frames[2].keyframe);
		assert!(
			frames[2].payload.windows(good_sps.len()).any(|bytes| bytes == good_sps),
			"the recovery keyframe carries the last good SPS"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_h264_sps_keeps_the_last_good_parameter_sets() {
		damaged_sps_keeps_the_last_good_parameter_sets(StreamType::H264).await;
	}

	#[tokio::test(start_paused = true)]
	async fn damaged_h265_sps_keeps_the_last_good_parameter_sets() {
		damaged_sps_keeps_the_last_good_parameter_sets(StreamType::H265).await;
	}

	#[test]
	fn adaptation_lengths_obey_the_packet_control() {
		for (control, length, valid) in [
			(0b10, 0, false),
			(0b10, 182, false),
			(0b10, 183, true),
			(0b10, 184, false),
			(0b11, 0, true),
			(0b11, 182, true),
			(0b11, 183, false),
			(0b11, 184, false),
		] {
			let mut packet: [u8; TsPacket::SIZE] = clock_break_packet(VIDEO).try_into().unwrap();
			packet[3] = control << 4;
			packet[4] = length;
			packet[5] = 0;
			assert_eq!(
				super::adaptation_valid(&packet),
				valid,
				"control={control:b} length={length}"
			);
		}
	}

	#[test]
	fn invalid_adaptation_lengths_damage_media_and_dedicated_clock_pids() {
		const PCR: u16 = 0x0200;
		for pid in [VIDEO, PCR] {
			for (control, length) in [(0b10, 0), (0b10, 182), (0b11, 183)] {
				let mut mux = Mux {
					out: synth_pmt(&[(StreamType::H264, VIDEO)], false),
					..Default::default()
				};
				mux.gop(VIDEO, 90_000);
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
				let mut import = super::Import::new(broadcast, catalog.reserve());
				import.decode(&mux.out).unwrap();
				import.pcr_pid = Some(Pid::new(PCR).unwrap());

				let mut packet = clock_break_packet(pid);
				packet[3] = control << 4;
				packet[4] = length;
				import.decode(&packet).unwrap();
				assert!(import.last_pts.is_some(), "invalid adaptation reset the clock");
				assert_eq!(
					import.stats().streams[&pid].damaged,
					1,
					"pid={pid} control={control:b} length={length}"
				);
			}
		}
	}

	/// A dedicated PCR PID's malformed adaptation field cannot declare a timebase break.
	#[test]
	fn damaged_adaptation_on_a_dedicated_pcr_pid_keeps_the_clock() {
		// Clear of the PMT PID and every stream, so only the clock-only row can carry it.
		const PCR: u16 = 0x0200;
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO)], false),
			..Default::default()
		};
		mux.gop(VIDEO, 90_000);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(&mux.out).unwrap();
		import.pcr_pid = Some(Pid::new(PCR).unwrap());
		assert!(import.last_pts.is_some());

		// The flag byte is the field's only byte, so the PCR it announces overruns it.
		let mut packet = clock_break_packet(PCR);
		packet[4] = 1;
		packet[5] = 0x90;
		import.decode(&packet).unwrap();
		assert!(import.last_pts.is_some(), "a malformed field reset the program clock");
		let stats = import.stats();
		assert_eq!(
			(stats.streams[&PCR].track.as_str(), stats.streams[&PCR].damaged),
			("", 1)
		);
	}

	/// A damaged PES start does not revoke the AU a codec boundary already
	/// completed in the preceding unbounded PES.
	#[test]
	fn damaged_pes_start_flushes_the_unit_before_it() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let unbounded = |cc, pts, keyframe| {
			let mut au = annexb_au(keyframe);
			if keyframe {
				au.extend([0, 0, 1, 9, 0xf0]);
			}
			let mut packet = video_pes(VIDEO, cc, pts, None, &au);
			let start = 5 + usize::from(packet[4]);
			packet[start + 4..start + 6].fill(0);
			packet
		};
		let mut data = synth_pmt(&[(StreamType::H264, VIDEO)], false);
		data.extend(unbounded(0, 90_000, true));
		let mut damaged = unbounded(1, 90_000 + FRAME, false);
		let start = 5 + usize::from(damaged[4]);
		damaged[start + 6..start + 19].fill(0);
		data.extend(damaged);
		import.decode(&data).unwrap();
		import.finish().unwrap();
		assert_eq!(import.stats().streams[&VIDEO].damaged, 1);
		assert_eq!(
			import.stats().streams[&VIDEO].units,
			1,
			"the keyframe before the damage"
		);
	}

	/// A flagged clock break that sets PUSI on an adaptation-only packet starts no PES, so it
	/// proves nothing whole: the unbounded picture open across it is cut short and dropped.
	#[test]
	fn flagged_break_without_payload_drops_the_open_picture() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let unbounded = |cc, pts, keyframe| {
			let mut au = annexb_au(keyframe);
			if keyframe {
				au.extend([0, 0, 1, 9, 0xf0]);
			}
			let mut packet = video_pes(VIDEO, cc, pts, None, &au);
			let start = 5 + usize::from(packet[4]);
			packet[start + 4..start + 6].fill(0);
			packet
		};
		let mut data = synth_pmt(&[(StreamType::H264, VIDEO)], false);
		data.extend(unbounded(0, 90_000, true));
		data.extend(unbounded(1, 90_000 + FRAME, false));
		let mut flagged = clock_break_packet(VIDEO);
		flagged[1] |= 0x40;
		flagged[3] |= 1;
		data.extend(flagged);
		import.decode(&data).unwrap();
		import.finish().unwrap();
		assert_eq!(
			import.stats().streams[&VIDEO].units,
			1,
			"only the keyframe the AUD proved whole"
		);
	}

	/// A malformed unbounded PES drained at EOF is still refused and counted.
	#[test]
	fn damaged_final_access_unit_is_refused() {
		let mut au = annexb_au(true);
		let nal = au
			.windows(5)
			.position(|bytes| bytes[..4] == [0, 0, 0, 1] && bytes[4] & 0x1f == 5)
			.unwrap();
		au[nal + 4] |= 0x80;
		for au in [au, vec![0], vec![0, 0], vec![0, 0, 0]] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve());
			let mut packet = video_pes(VIDEO, 0, 90_000, None, &au);
			let start = 5 + usize::from(packet[4]);
			packet[start + 4..start + 6].fill(0);
			let mut data = synth_pmt(&[(StreamType::H264, VIDEO)], false);
			data.extend(packet);
			import.decode(&data).unwrap();
			assert_eq!(import.stats().streams[&VIDEO].damaged, 0, "PES has not ended yet");
			import.finish().unwrap();
			assert_eq!(import.stats().streams[&VIDEO].damaged, 1);
			assert_eq!(import.stats().streams[&VIDEO].units, 0);
			assert!(catalog.snapshot().video.renditions.is_empty());
		}
	}

	/// An invalid trailing Opus control header refuses the complete PES before its first
	/// packet is published. Independently decodable packets in the next PES recover.
	#[tokio::test(start_paused = true)]
	async fn damaged_opus_pes_is_not_half_published() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.ensure_stream(
				Pid::new(VIDEO).unwrap(),
				0x06,
				&[super::catalog::Descriptor {
					tag: 0x05,
					data: bytes::Bytes::from_static(b"Opus"),
				}],
			)
			.unwrap();
		let good = [0x7f, 0xe0, 3, 0xf8, 0xff, 0xfe];
		let mut bad = good.to_vec();
		bad.push(0x7f);
		let mut data = Vec::new();
		for (cc, payload) in [(0, good.as_slice()), (1, bad.as_slice()), (2, good.as_slice())] {
			data.extend(video_pes(VIDEO, cc, 90_000 + u64::from(cc) * FRAME, None, payload));
		}
		import.decode(&data).unwrap();
		import.finish().unwrap();
		assert_eq!(import.stats().streams[&VIDEO].damaged, 1);
		assert_eq!(import.stats().streams[&VIDEO].units, 2);
		let name = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();
		let frames = read_track(&consumer, &name, crate::container::Kind::Audio).await;
		assert_eq!(frames.len(), 2, "no packet from the damaged PES is published");
	}

	/// An aborted track is a feed-wide publishing failure, never a damaged media unit.
	#[test]
	fn damaged_recovery_keeps_publishing_errors_fatal() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let video = crate::codec::h264::Import::new(track.clone(), catalog.reserve(), Default::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.streams.insert(
			Pid::new(VIDEO).unwrap(),
			Stream::H264 {
				split: crate::codec::h264::Split::new(),
				assembly: super::video::Assembly::new(super::video::Codec::Avc),
				import: Box::new(video),
			},
		);
		track.abort(moq_net::Error::Closed).unwrap();
		let mut data = Vec::new();
		for cc in 0..3 {
			data.extend(video_pes(
				VIDEO,
				cc,
				90_000 + u64::from(cc) * FRAME,
				None,
				&annexb_au(true),
			));
		}
		let err = import.decode(&data).expect_err("closed producer must end ingest");
		assert!(
			matches!(err.downcast_ref::<crate::Error>(), Some(crate::Error::Moq(_))),
			"{err:?}"
		);
		assert!(import.damaged.is_empty(), "publishing failure is not local damage");
		assert!(
			!super::unit_error(crate::Error::H264(crate::codec::h264::Error::MissingTimestamp)).is::<super::Damaged>()
		);
	}

	/// A break closes the video group where its content stops, not a GOP later at the next
	/// keyframe. An open GOP's leading pictures present below that keyframe, so an end marker
	/// there would sit past them and the exporter refused them as a rewind.
	#[tokio::test(start_paused = true)]
	async fn damaged_video_closes_its_group_at_the_break() {
		use mpeg2ts::ts::{ReadTsPacket, TsPacketReader, TsPayload};

		const MS: u64 = 90;
		let at = |ms: u64| 90_000 + ms * MS;
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO)], false),
			..Default::default()
		};
		// A closed GOP whose third picture is damaged, then an open GOP whose leading B-frame
		// presents below its IDR.
		let pictures = [
			(true, at(0), None),
			(false, at(40), None),
			(false, at(80), None),
			(true, at(1200), Some(at(1000))),
			(false, at(1040), None),
			(false, at(1280), Some(at(1080))),
		];
		let mut damaged_at = 0;
		for (i, (keyframe, pts, dts)) in pictures.into_iter().enumerate() {
			if i == 2 {
				damaged_at = mux.out.len();
			}
			let cc = mux.cc(VIDEO);
			mux.out.extend_from_slice(&video_pes(VIDEO, cc, pts, dts, &{
				let mut au = annexb_au(keyframe);
				if i == 1 {
					au.extend([0, 0, 1, 9, 0xf0]);
				}
				au
			}));
		}
		mux.out[damaged_at + 1] |= 0x80;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(&mux.out).unwrap();
		import.finish().unwrap();
		assert_eq!(import.stats().streams[&VIDEO].damaged, 1);

		// Each group as published, its empty end marker included.
		let name = catalog.snapshot().video.renditions.keys().next().unwrap().clone();
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(3600));
		let mut track = consumer.track(&name).unwrap().subscribe(subscription).await.unwrap();
		let mut groups = Vec::new();
		while let Some(mut group) = track.recv_group().await.unwrap() {
			let mut frames = Vec::new();
			while let Some(mut frame) = group.next_frame().await.unwrap() {
				let frame = hang::container::Frame::decode(frame.read_all().await.unwrap()).unwrap();
				frames.push((frame.timestamp.as_micros(), frame.payload.is_empty()));
			}
			groups.push(frames);
		}
		let us = |ms: u64| u128::from(at(ms)) * 1_000_000 / 90_000;
		assert_eq!(
			groups,
			[
				vec![(us(0), false), (us(40), false), (us(80), true)],
				vec![(us(1200), false), (us(1040), false), (us(1280), false)],
			],
			"the first group ends at the break, one frame after its last picture"
		);

		let mut exporter = crate::container::ts::export_test::open_export(crate::source::announced(&consumer).await)
			.await
			.unwrap()
			.with_delay(Duration::from_secs(30))
			.with_replay();
		let mut ts = Vec::new();
		while let Ok(res) = tokio::time::timeout(Duration::from_secs(100), exporter.next()).await {
			let Some(frame) = res.expect("the exporter must take the open GOP after the break") else {
				break;
			};
			ts.extend_from_slice(&frame.payload);
		}
		let mut reader = TsPacketReader::new(std::io::Cursor::new(ts));
		let mut pictures = 0;
		while let Some(packet) = reader.read_ts_packet().unwrap() {
			pictures += usize::from(matches!(packet.payload, Some(TsPayload::PesStart(_))));
		}
		assert_eq!(pictures, 5, "every picture either side of the break is exported");
	}

	/// #3798's join: a new IDR presents below the last P-frame on a continuous clock, since
	/// the B-frames before it presented earlier than the P-frame decoded ahead of them. It
	/// still starts after the previous group did, so it overlaps rather than rewinds.
	#[tokio::test(start_paused = true)]
	async fn h264_join_below_a_p_frame_publishes() {
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO)], false),
			..Default::default()
		};
		mux.gops(VIDEO, 90_000, 2);
		// The last P-frame presented at 90_000 + 8 * FRAME; the join lands a frame short.
		mux.gops(VIDEO, 90_000 + 6 * FRAME, 2);
		let frames = video_frames(&mux.out).await;
		assert_eq!(frames.len(), 16, "every picture either side of the join");
		assert_forward(&frames);
	}

	/// An encoder restart that declares the new clock on the PCR PID still restarts lower
	/// than the live edge: a new broadcast, not a continuation.
	#[tokio::test(start_paused = true)]
	async fn h264_flagged_backward_restart_is_refused() {
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO)], false),
			..Default::default()
		};
		mux.gops(VIDEO, 45 * 90_000, 2);
		mux.out.extend_from_slice(&clock_break_packet(VIDEO));
		mux.gops(VIDEO, 90_000, 2);
		import_refused(&mux.out);
	}

	/// The same restart unflagged, as a looping playout wraps to the top of its file.
	#[tokio::test(start_paused = true)]
	async fn h264_unflagged_backward_restart_is_refused() {
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO)], false),
			..Default::default()
		};
		mux.gops(VIDEO, 45 * 90_000, 2);
		mux.gops(VIDEO, 90_000, 2);
		import_refused(&mux.out);
	}

	/// A flagged break that leaps forward is not a rewind: the new timeline publishes.
	#[tokio::test(start_paused = true)]
	async fn h264_flagged_forward_restart_publishes() {
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::H264, VIDEO)], false),
			..Default::default()
		};
		mux.gops(VIDEO, 90_000, 2);
		// A codec boundary, not the following clock marker, vouches for the
		// final picture on the old timeline.
		let mut last = annexb_au(false);
		last.extend([0, 0, 1, 9, 0xf0]);
		let at = mux.out.len() - 188;
		mux.out[at..].copy_from_slice(&video_pes(VIDEO, 7, 90_000 + 7 * FRAME, None, &last));
		mux.out.extend_from_slice(&clock_break_packet(VIDEO));
		// Inside the track's 30 s retention, so the reader still reaches the first pass.
		mux.gops(VIDEO, 20 * 90_000, 2);
		let frames = video_frames(&mux.out).await;
		assert_eq!(frames.len(), 16);
		assert_forward(&frames);
	}

	/// Legacy audio wrapping to the top of a looping file is a restart.
	#[tokio::test(start_paused = true)]
	async fn legacy_loop_wrap_is_refused() {
		const MP2_PID: u16 = 0x0061;
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::Mpeg1Audio, MP2_PID)], false),
			..Default::default()
		};
		// One PES is two 72 ms MP2 frames.
		const PES_TICKS: u64 = 2 * 72 * 90;
		for _ in 0..2 {
			for i in 0..4 {
				let cc = mux.cc(MP2_PID);
				mux.out
					.extend_from_slice(&mp2_pes(MP2_PID, cc, 90_000 + i * PES_TICKS, [0xAA, 0xBB]));
			}
		}
		import_refused(&mux.out);
	}

	/// A looping file whose audio runs past the loop period restarts a frame below the last
	/// frame published: a restart like any other.
	#[tokio::test(start_paused = true)]
	async fn aac_loop_overlapping_the_edge_is_refused() {
		const AAC_PID: u16 = 0x0060;
		// 1024 samples at 48 kHz, in 90 kHz ticks.
		const AAC_FRAME: u64 = 1024 * 90_000 / 48_000;
		let mut mux = Mux {
			out: synth_pmt(&[(StreamType::AdtsAac, AAC_PID)], false),
			..Default::default()
		};
		for pass in 0..2u64 {
			for i in 0..4u64 {
				let mut payload = adts_frame(17, 0xA0 | i as u8);
				payload.extend_from_slice(&adts_frame(17, 0xB0 | i as u8));
				let cc = mux.cc(AAC_PID);
				let pts = 90_000 + pass * 6 * AAC_FRAME + i * 2 * AAC_FRAME;
				mux.out.extend_from_slice(&audio_pes_packet(AAC_PID, cc, pts, &payload));
			}
		}
		import_refused(&mux.out);
	}

	/// PCR placement advances independently of the video's B-frame presentation reorder.
	#[tokio::test(start_paused = true)]
	async fn section_after_a_b_frame_publishes_forward() {
		const CUE_PID: u16 = 0x0021;
		let mut mux = Mux {
			out: synth_pmt(
				&[
					(StreamType::H264, VIDEO),
					(StreamType::Dts8ChannelLosslessAudio, CUE_PID),
				],
				true,
			),
			..Default::default()
		};
		// Two GOPs back to back.
		for gop in 0..2 {
			let base = 90_000 + gop * 4 * FRAME;
			for (index, (keyframe, pts, dts)) in [
				(true, base + FRAME, Some(base)),
				(false, base + 4 * FRAME, Some(base + FRAME)),
				(false, base + 2 * FRAME, None),
			]
			.into_iter()
			.enumerate()
			{
				mux.out
					.extend_from_slice(&pcr_packet(VIDEO, (base + index as u64 * FRAME) * 300));
				let cc = mux.cc(VIDEO);
				mux.out
					.extend_from_slice(&video_pes(VIDEO, cc, pts, dts, &annexb_au(keyframe)));
				// `packet` builds on the cue PID.
				let cc = mux.cc(CUE_PID);
				mux.out.extend_from_slice(&packet(true, cc, 0, &CUE));
			}
		}
		let (consumer, catalog, _import) = import_all(&mux.out).expect("a cue after a B-frame must not end the import");
		let name = catalog.snapshot().ext.mpegts.tracks.keys().next().unwrap().clone();
		let frames = read_track(&consumer, &name, crate::container::Kind::Data).await;
		assert_eq!(frames.len(), 6, "every cue publishes");
		assert_forward(&frames);
		// The next GOP's cues follow the video rather than piling up on the edge.
		assert!(
			frames[3].timestamp.as_micros() > frames[2].timestamp.as_micros(),
			"the cues did not move with the video: {:?}",
			frames.iter().map(|f| f.timestamp.as_micros()).collect::<Vec<_>>()
		);
	}

	/// An adaptation-only clock packet on `pid` carrying `ticks` of the 27 MHz PCR.
	fn pcr_packet(pid: u16, ticks: u64) -> Vec<u8> {
		let (base, ext) = (ticks / 300, ticks % 300);
		let mut p = vec![
			0x47,
			(pid >> 8) as u8 & 0x1f,
			(pid & 0xff) as u8,
			0x20,
			183,
			0x10,
			(base >> 25) as u8,
			(base >> 17) as u8,
			(base >> 9) as u8,
			(base >> 1) as u8,
			((base as u8 & 1) << 7) | 0x7e | (ext >> 8) as u8,
			ext as u8,
		];
		p.resize(188, 0xff);
		p
	}

	#[test]
	fn first_ts_catalog_waits_for_the_mux_rate_window() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		for cbr in [true, false] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(
				&mut broadcast,
				crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
			)
			.unwrap();
			let mut updates = catalog.consume().unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve());
			import
				.decode(&synth_pmt(&[(StreamType::Mpeg2PacketizedData, PCR_PID)], false))
				.unwrap();
			import.decode(&pes_packet(PCR_PID, 90_000)).unwrap();
			let waiter = kio::Waiter::noop();
			assert!(
				updates.poll_next(&waiter).is_pending(),
				"catalog preceded rate measurement"
			);

			for i in 0..=50 {
				let mut bytes = pcr_packet(PCR_PID, i * 27_000_000 / 25);
				let stuffing = if cbr || i < 25 { 99 } else { 19 };
				for _ in 0..stuffing {
					bytes.extend_from_slice(&super::super::export::NULL_PACKET);
				}
				import.decode(&bytes).unwrap();
				if i < 50 {
					assert!(
						updates.poll_next(&waiter).is_pending(),
						"catalog before a full PCR window"
					);
				}
			}
			let std::task::Poll::Ready(Ok(Some(first))) = updates.poll_next(&waiter) else {
				panic!("catalog did not release at the end of the first window");
			};
			assert_eq!(first.ext.mpegts.mux_rate, cbr.then_some(3_760_000));
		}
	}

	/// A live source with no PCR never completes a window, so media time releases its catalog.
	#[test]
	fn a_ts_without_pcr_releases_its_catalog_after_four_seconds_of_media() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::Mpeg2PacketizedData, PCR_PID)], false))
			.unwrap();
		let waiter = kio::Waiter::noop();
		for frame in 0..100 {
			import.decode(&pes_packet(PCR_PID, 90_000 + frame * 3_600)).unwrap();
			assert!(updates.poll_next(&waiter).is_pending(), "catalog after frame {frame}");
		}
		import.decode(&pes_packet(PCR_PID, 90_000 + 100 * 3_600)).unwrap();
		let std::task::Poll::Ready(Ok(Some(first))) = updates.poll_next(&waiter) else {
			panic!("four seconds of media did not release the catalog");
		};
		assert_eq!(first.ext.mpegts.mux_rate, None);
	}

	/// A source whose clock keeps resetting before a window completes still releases its
	/// catalog once enough media time has passed in total.
	#[test]
	fn a_ts_resetting_its_clock_does_not_release_without_a_configured_track() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::H264, PCR_PID)], false))
			.unwrap();
		let waiter = kio::Waiter::noop();
		// Each epoch carries 0.96 s of media, then a flagged PCR restarts the clock.
		for epoch in 0..6 {
			for frame in 0..25 {
				import.decode(&pes_packet(PCR_PID, 90_000 + frame * 3_600)).unwrap();
			}
			import.decode(&flag_discontinuity(pcr_packet(PCR_PID, 0))).unwrap();
			assert!(
				updates.poll_next(&waiter).is_pending(),
				"metadata-only catalog released after epoch {epoch}"
			);
		}
	}

	/// A clock break after a measured window but before the first catalog publishes voids the
	/// measurement, so the catalog waits for a window on the new clock and carries its rate.
	#[test]
	fn a_break_before_the_first_catalog_waits_for_a_new_window() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::Mpeg1Audio, PCR_PID)], false))
			.unwrap();
		let window = |import: &mut super::Import<Ext>, stuffing: usize| {
			for i in 0..=50 {
				let mut bytes = pcr_packet(PCR_PID, i * 27_000_000 / 25);
				for _ in 0..stuffing {
					bytes.extend_from_slice(&super::super::export::NULL_PACKET);
				}
				import.decode(&bytes).unwrap();
			}
		};
		let waiter = kio::Waiter::noop();

		// A full window before any PES, then a break, then the first PES.
		window(&mut import, 99);
		import.decode(&flag_discontinuity(pcr_packet(PCR_PID, 0))).unwrap();
		import.decode(&mp2_pes(PCR_PID, 0, 90_000, [0xaa, 0xbb])).unwrap();
		assert!(
			updates.poll_next(&waiter).is_pending(),
			"the catalog published on a voided measurement"
		);

		window(&mut import, 49);
		let std::task::Poll::Ready(Ok(Some(first))) = updates.poll_next(&waiter) else {
			panic!("the new clock's window did not release the catalog");
		};
		assert_eq!(first.ext.mpegts.mux_rate, Some(1_880_000));
	}

	/// The hold counts media time across a 33-bit PTS rollover.
	#[test]
	fn a_ts_without_pcr_releases_its_catalog_across_a_pts_rollover() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::Mpeg2PacketizedData, PCR_PID)], false))
			.unwrap();
		let waiter = kio::Waiter::noop();
		// Starts two seconds before the rollover.
		let start = (1 << 33) - 180_000;
		for frame in 0..100 {
			import
				.decode(&pes_packet(PCR_PID, (start + frame * 3_600) % (1 << 33)))
				.unwrap();
			assert!(updates.poll_next(&waiter).is_pending(), "catalog after frame {frame}");
		}
		import
			.decode(&pes_packet(PCR_PID, (start + 100 * 3_600) % (1 << 33)))
			.unwrap();
		assert!(
			updates.poll_next(&waiter).is_ready(),
			"four seconds across the rollover did not release the catalog"
		);
	}

	#[test]
	fn finishing_a_short_ts_releases_its_catalog_without_a_rate() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut updates = catalog.consume().unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::Mpeg2PacketizedData, PCR_PID)], false))
			.unwrap();
		import.decode(&pes_packet(PCR_PID, 90_000)).unwrap();
		import.decode(&pcr_packet(PCR_PID, 0)).unwrap();
		let waiter = kio::Waiter::noop();
		assert!(updates.poll_next(&waiter).is_pending());
		import.finish().unwrap();
		let std::task::Poll::Ready(Ok(Some(first))) = updates.poll_next(&waiter) else {
			panic!("short recording finished without a catalog");
		};
		assert_eq!(first.ext.mpegts.mux_rate, None);
	}

	/// The catalog follows the clock: a stable multiplex rate is recorded, a source that
	/// stops holding it clears the record, a fresh stable window records it again, and a
	/// declared time-base break clears it at once.
	#[test]
	fn mux_rate_follows_the_clock() {
		use crate::catalog::hang::Catalog;
		use crate::container::ts::catalog::Ext;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.decode(&synth_pmt(&[(StreamType::Mpeg1Audio, PCR_PID)], false))
			.unwrap();

		// 40 ms PCR intervals; `stuffing(i)` packets of null padding after the i-th clock.
		const INTERVAL: u64 = 27_000_000 / 25;
		let mut ticks = 0;
		let mut feed = |import: &mut super::Import<Ext>, seconds: u64, stuffing: &dyn Fn(u64) -> usize| {
			for i in 0..seconds * 25 {
				let mut bytes = pcr_packet(PCR_PID, ticks);
				for _ in 0..stuffing(i) {
					bytes.extend_from_slice(&super::super::export::NULL_PACKET);
				}
				import.decode(&bytes).unwrap();
				ticks += INTERVAL;
			}
		};
		let rate = || catalog.snapshot().ext.mpegts.mux_rate;

		// 100 packets per 40 ms: 3.76 Mb/s, byte-locked.
		feed(&mut import, 3, &|_| 99);
		assert_eq!(rate(), Some(3_760_000), "a stable window records the rate");

		// The stuffing comes and goes: nothing agrees with the record any more. Windows
		// are 2 s and the feeds are not aligned to them, so allow one straddling window
		// (which still holds intervals at the old rate) before the next full one clears.
		feed(&mut import, 4, &|i| if (i / 5) % 2 == 0 { 10 } else { 90 });
		assert_eq!(rate(), None, "an unstable window clears the record");

		feed(&mut import, 4, &|_| 99);
		assert_eq!(rate(), Some(3_760_000), "a fresh stable window records it again");

		import.decode(&clock_break_packet(PCR_PID)).unwrap();
		assert_eq!(rate(), None, "a time-base break clears the record at once");
	}

	/// The same flag on an elementary PID declares only that the continuity counter jumped.
	/// The partial it interrupts is dropped, as ever, but the program clock is untouched, so
	/// no track takes a break and the peer PID never notices.
	#[tokio::test(start_paused = true)]
	async fn elementary_discontinuity_is_not_a_timebase_break() {
		let (consumer, catalog, mut import) = two_stream_import();

		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		import
			.decode(flag_discontinuity(mp2_pes(PEER_PID, 5, 180_000, [0xCC, 0xDD])).as_slice())
			.unwrap();
		import
			.decode(mp2_pes(PCR_PID, 1, 180_000, [0xCC, 0xDD]).as_slice())
			.unwrap();
		import.finish().unwrap();

		for (_, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 0, "a counter jump is not a program timebase reset");
		}
	}

	/// A counter gap on the PCR PID with no flag anywhere is loss, not a signalled reset: the
	/// partial goes, the clock stays.
	#[tokio::test(start_paused = true)]
	async fn a_counter_gap_is_not_a_timebase_break() {
		let (consumer, catalog, mut import) = two_stream_import();

		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		// cc 1 -> 7 on the clock's own PID.
		import
			.decode(mp2_pes(PCR_PID, 7, 180_000, [0xCC, 0xDD]).as_slice())
			.unwrap();
		import.finish().unwrap();

		for (_, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 0, "lost packets are not a signalled clock reset");
		}
	}

	/// The 33-bit PTS field wraps every 26.5 hours with nothing set anywhere, and that is
	/// correct: it is unwrapped into a continuous timeline rather than declared a break.
	#[tokio::test(start_paused = true)]
	async fn a_timestamp_rollover_is_not_a_timebase_break() {
		let (consumer, catalog, mut import) = two_stream_import();

		const FIELD: u64 = 1 << 33;
		for pid in [PCR_PID, PEER_PID] {
			import
				.decode(mp2_pes(pid, 0, FIELD - 90_000, [0xAA, 0xBB]).as_slice())
				.unwrap();
			import.decode(mp2_pes(pid, 1, 90_000, [0xCC, 0xDD]).as_slice()).unwrap();
		}
		import.finish().unwrap();

		for (frames, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 0, "a rollover is not a break");
			let stamps: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
			assert!(
				stamps.windows(2).all(|pair| pair[0] < pair[1]),
				"the rollover broke the timeline: {stamps:?}"
			);
		}
	}

	/// A mux may flag every packet it emits until the new clock is established, and a clock
	/// packet is retransmitted freely. Consecutive markers with no media between them are one
	/// break, or a downstream re-acquires once per packet.
	#[tokio::test(start_paused = true)]
	async fn repeated_flags_declare_one_break() {
		let (consumer, catalog, mut import) = two_stream_import();

		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		for _ in 0..4 {
			import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		}
		for pid in [PCR_PID, PEER_PID] {
			import
				.decode(mp2_pes(pid, 1, 180_000, [0xCC, 0xDD]).as_slice())
				.unwrap();
		}
		import.finish().unwrap();

		for (_, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 1, "each flagged packet declared its own break");
		}
	}

	/// The demodulator disowned the packet, so nothing in it is evidence, the adaptation
	/// field included. Line noise that happens to set the bit must not break every track in
	/// the program.
	#[tokio::test(start_paused = true)]
	async fn a_corrupt_packet_declares_no_break() {
		let (consumer, catalog, mut import) = two_stream_import();

		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		// transport_error_indicator, on a clock packet that also sets the discontinuity flag.
		let mut corrupt = clock_break_packet(PCR_PID);
		corrupt[1] |= 0x80;
		import.decode(corrupt.as_slice()).unwrap();
		for pid in [PCR_PID, PEER_PID] {
			import
				.decode(mp2_pes(pid, 1, 180_000, [0xCC, 0xDD]).as_slice())
				.unwrap();
		}
		import.finish().unwrap();

		for (_, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 0, "a corrupt packet's adaptation field was trusted");
		}
	}

	/// A flag arriving before anything has been published breaks nothing: a capture joining
	/// mid-stream lands on whatever the mux is flagging at the time, and there is no timeline
	/// behind it to cut.
	#[tokio::test(start_paused = true)]
	async fn a_break_before_any_media_is_ignored() {
		let (consumer, catalog, mut import) = two_stream_import();

		import.decode(clock_break_packet(PCR_PID).as_slice()).unwrap();
		for pid in [PCR_PID, PEER_PID] {
			import.decode(mp2_pes(pid, 0, 90_000, [0xAA, 0xBB]).as_slice()).unwrap();
		}
		import.finish().unwrap();

		for (frames, breaks) in read_all_breaks(&consumer, &catalog).await {
			assert_eq!(breaks, 0, "nothing had been published to break from");
			assert!(!frames.is_empty(), "the media after the flag still publishes");
		}
	}

	/// Each reset stimulus has eight PES packets of two 72 ms MP2 frames.
	const RESET_PES: u64 = 8;
	const RESET_PES_TICKS: u64 = 2 * 72 * 90;
	const RESET_LEAP_TICKS: u64 = 30 * 90_000;

	/// Drain queued media on its existing deadlines, stopping when the live export is idle.
	async fn exported_discontinuities(
		exporter: &mut crate::container::ts::Export<crate::catalog::Consumer>,
	) -> (usize, usize) {
		let waiter = kio::Waiter::noop();
		let mut frames = 0;
		let mut flagged = 0;
		loop {
			match exporter.poll_next(&waiter) {
				std::task::Poll::Ready(Ok(Some(frame))) => {
					frames += 1;
					flagged += frame
						.payload
						.as_chunks::<188>()
						.0
						.iter()
						.filter(|p| p[3] & 0x20 != 0 && p[4] > 0 && p[5] & 0x80 != 0)
						.count();
				}
				std::task::Poll::Ready(Ok(None)) => break,
				std::task::Poll::Ready(Err(err)) => panic!("exporter error: {err}"),
				std::task::Poll::Pending => {
					let Some(deadline) = exporter.next_due() else { break };
					moq_clock::sleep_until(deadline).await;
				}
			}
		}
		(frames, flagged)
	}

	/// Import `before`, re-export the broadcast to MPEG-TS until it has gone out, then import
	/// `after` and count the exported packets whose adaptation field sets
	/// `discontinuity_indicator`.
	async fn export_discontinuities(before: &[u8], after: &[u8]) -> usize {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(&bytes::BytesMut::from(before)).unwrap();

		// Retain the complete stimulus span, the 30 s leap included: less sheds earlier
		// groups and their boundary. Queued media is drained on its deadlines, without
		// generating output through an arbitrary silence before feeding the next phase.
		let delay = std::time::Duration::from_millis((RESET_LEAP_TICKS + RESET_PES * RESET_PES_TICKS) / 90);
		let mut exporter = crate::container::ts::export_test::open_export(crate::source::announced(&consumer).await)
			.await
			.unwrap()
			.with_delay(delay)
			.with_replay();
		// A discontinuity before anything went out moves no clock, so the media before the
		// boundary goes out first, as it would live.
		let (frames, before_flags) = exported_discontinuities(&mut exporter).await;
		assert!(frames > 0, "media before the boundary must reach the output");
		import.decode(&bytes::BytesMut::from(after)).unwrap();
		import.finish().unwrap();
		let (frames, after_flags) = exported_discontinuities(&mut exporter).await;
		assert!(frames > 0, "media after the boundary must reach the output");
		before_flags + after_flags
	}

	/// The end-to-end shape the #2833 stimulus campaign graded: source, MoQ frames, exported
	/// TS. A +30 s leap the source declared must come out declared, where before the flag
	/// died in the demuxer and a downstream saw the leap as an unsignalled timebase change,
	/// i.e. a stream error. The same leap with nothing set at the source stays unflagged:
	/// the exporter reports what the source declared, and does not infer a break from a
	/// timestamp step.
	#[tokio::test(start_paused = true)]
	async fn a_signalled_reset_reaches_the_exported_clock() {
		const PID: u16 = 0x0061;

		let media = |ts: &mut Vec<u8>, cc: &mut u8, base: u64| {
			for i in 0..RESET_PES {
				ts.extend_from_slice(&mp2_pes(PID, *cc, base + i * RESET_PES_TICKS, [0xAA, 0xBB]));
				*cc = (*cc + 1) & 0x0f;
			}
		};

		let mut cc = 0;
		let mut before = synth_pmt(&[(StreamType::Mpeg1Audio, PID)], false);
		media(&mut before, &mut cc, 90_000);
		let mut signalled = clock_break_packet(PID);
		let mut unsignalled = Vec::new();
		let mut peer_cc = cc;
		media(&mut signalled, &mut cc, 90_000 + RESET_LEAP_TICKS);
		media(&mut unsignalled, &mut peer_cc, 90_000 + RESET_LEAP_TICKS);

		assert_eq!(
			export_discontinuities(&before, &signalled).await,
			1,
			"the declared reset never reached the exported clock"
		);
		assert_eq!(
			export_discontinuities(&before, &unsignalled).await,
			0,
			"a leap the source did not declare must not be flagged"
		);
	}

	/// A shared forward boundary is one program break. Every rendition joins the new
	/// generation, so the clock flags it once however many tracks declared the marker.
	#[tokio::test(start_paused = true)]
	async fn a_shared_forward_boundary_resets_once_per_rendition() {
		let mut stimulus = synth_pmt(
			&[(StreamType::Mpeg1Audio, PCR_PID), (StreamType::Mpeg1Audio, PEER_PID)],
			false,
		);
		let media = |ts: &mut Vec<u8>, cc: &mut u8, base: u64| {
			for i in 0..RESET_PES {
				for pid in [PCR_PID, PEER_PID] {
					ts.extend_from_slice(&mp2_pes(pid, *cc, base + i * RESET_PES_TICKS, [0xAA, 0xBB]));
				}
				*cc = (*cc + 1) & 0x0f;
			}
		};

		let mut cc = 0;
		media(&mut stimulus, &mut cc, 90_000);
		let mut after = clock_break_packet(PCR_PID);
		media(&mut after, &mut cc, 90_000 + RESET_LEAP_TICKS);

		assert_eq!(
			export_discontinuities(&stimulus, &after).await,
			1,
			"one flag per program break"
		);
	}

	fn opus_extension(body: &[u8]) -> Vec<super::catalog::Descriptor> {
		vec![super::catalog::Descriptor {
			tag: 0x7f,
			data: bytes::Bytes::copy_from_slice(body),
		}]
	}

	fn mapping(config: &crate::codec::opus::Config) -> (u8, u8, u8, Vec<u8>) {
		let mapping = config.mapping.expect("a channel mapping");
		(
			mapping.family(),
			mapping.streams(),
			mapping.coupled(),
			mapping.table().to_vec(),
		)
	}

	/// Codes at 0x80 and above are the Opus-in-TS draft table, not a stereo guess.
	/// The 6-channel 0x81 body is the descriptor gstreamer writes for family 255
	/// (`[0x81, 6, 255, 160, 20, 229]`); the other bodies are hand-built from that table.
	#[test]
	fn opus_channel_codes_keep_their_layout() {
		let ext = |body: &[u8]| super::opus_config(&opus_extension(body));

		let missing = super::opus_config(&[]).unwrap();
		assert_eq!(missing.channel_count, 2);
		assert!(missing.mapping.is_none(), "no descriptor stays family 0 stereo");

		let plain = ext(&[0x80, 6]).unwrap();
		assert_eq!(plain.channel_count, 6);
		assert_eq!(mapping(&plain), (1, 4, 2, vec![0, 4, 1, 2, 3, 5]));

		// 0x80: two independent mono streams, not one coupled stereo stream.
		let dual = ext(&[0x80, 0x80]).unwrap();
		assert_eq!(dual.channel_count, 2);
		assert_eq!(mapping(&dual), (255, 2, 0, vec![0, 1]));

		// 0x82: ffmpeg's `0x80 | channels` uncoupled family 1 table.
		let uncoupled = ext(&[0x80, 0x82]).unwrap();
		assert_eq!(uncoupled.channel_count, 2);
		assert_eq!(mapping(&uncoupled), (1, 2, 0, vec![0, 1]));

		let wide = ext(&[0x80, 0x88]).unwrap();
		assert_eq!(wide.channel_count, 8);
		assert_eq!(mapping(&wide), (1, 8, 0, vec![0, 1, 2, 3, 4, 5, 6, 7]));

		// gstreamer, 6 uncoupled channels, family 255.
		let gst = ext(&[0x80, 0x81, 6, 255, 160, 20, 229]).unwrap();
		assert_eq!(gst.channel_count, 6);
		assert_eq!(mapping(&gst), (255, 6, 0, vec![0, 1, 2, 3, 4, 5]));

		// Hand-built 5.1 Vorbis table: streams 4, coupled 2, map {0,4,1,2,3,5}.
		let surround = ext(&[0x80, 0x81, 6, 1, 0x68, 0x42, 0x9d]).unwrap();
		assert_eq!(surround.channel_count, 6);
		assert_eq!(mapping(&surround), (1, 4, 2, vec![0, 4, 1, 2, 3, 5]));

		// Silence is the all-ones field (1 bit here), stored as 255 in the OpusHead.
		let silent = ext(&[0x80, 0x81, 2, 255, 0x10]).unwrap();
		assert_eq!(silent.channel_count, 2);
		assert_eq!(mapping(&silent), (255, 1, 0, vec![0, 255]));

		let mono = ext(&[0x80, 0x81, 1, 0]).unwrap();
		assert_eq!(mono.channel_count, 1);
		assert!(mono.mapping.is_none());
	}

	#[test]
	fn a_reserved_or_unparseable_opus_channel_code_is_refused() {
		let err = |body: &[u8]| {
			super::opus_config(&opus_extension(body))
				.expect_err("this code must not import")
				.to_string()
		};

		for code in [0x09_u8, 0x7f, 0x89, 0xff] {
			let message = err(&[0x80, code]);
			assert!(message.contains("reserved"), "{code:#04x}: {message}");
		}
		assert!(err(&[0x80]).contains("no channel_config_code"));
		assert!(err(&[0x80, 0x81]).contains("truncated"));
		assert!(err(&[0x80, 0x81, 0, 1]).contains("channel_count is zero"));
		assert!(err(&[0x80, 0x81, 3, 0]).contains("family 0"));
		assert!(err(&[0x80, 0x81, 2, 0, 0]).contains("trailing"));
		assert!(err(&[0x80, 0x82, 0]).contains("trailing"));
		// Nonzero pad bits on the silence layout above (0x10 with the pad set).
		assert!(err(&[0x80, 0x81, 2, 255, 0x1f]).contains("reserved bits"));
		// Mapping index 2 with only two coded channels, and not the silence value.
		assert!(err(&[0x80, 0x81, 2, 255, 0x84]).contains("mapping"));
		// gstreamer's 4 uncoupled channels: `g_bit_storage` widths at a power of two.
		assert!(err(&[0x80, 0x81, 4, 255, 0x60, 0x14, 0xc0]).contains("trailing"));
	}

	/// PAT/PMT for an MP2 PID plus an Opus PID whose extension body is `extension`.
	fn opus_program(extension: &[u8]) -> Vec<u8> {
		let mut descriptors = vec![0x05, 4, b'O', b'p', b'u', b's', 0x7f, extension.len() as u8];
		descriptors.extend_from_slice(extension);
		let mut data = psi_packets(0, &mut 0, &[], &pat_section(&[(1, 0x0100)]));
		data.extend(psi_packets(
			0x0100,
			&mut 0,
			&[],
			&pmt_section(
				1,
				0,
				&[
					(StreamType::Mpeg1Audio as u8, 0x0061, &[]),
					(StreamType::Mpeg2PacketizedData as u8, 0x0062, &descriptors),
				],
			),
		));
		data.extend(mp2_pes(0x0061, 0, 90_000, [0xAA, 0xBB]));
		data
	}

	fn import_program(data: &[u8]) -> crate::catalog::hang::Catalog {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import.decode(data).unwrap();
		import.finish().unwrap();
		catalog.snapshot()
	}

	/// A 0x81 descriptor reaches the catalog as its real OpusHead, beside the other track.
	#[test]
	fn an_explicit_opus_channel_code_publishes_its_head() {
		// gstreamer's 6-channel family 255 descriptor, in a hand-built program.
		let catalog = import_program(&opus_program(&[0x80, 0x81, 6, 255, 160, 20, 229]));
		assert_eq!(catalog.audio.renditions.len(), 2, "MP2 and Opus both publish");

		let opus = catalog
			.audio
			.renditions
			.values()
			.find(|audio| audio.codec.to_string() == "opus")
			.expect("the Opus track");
		assert_eq!(opus.channel_count, 6);
		let head = crate::codec::opus::Config::parse(&mut opus.description.as_deref().expect("an OpusHead")).unwrap();
		assert_eq!(head.channel_count, 6);
		let map = head.mapping.expect("family 255");
		assert_eq!(map.family(), 255);
		assert_eq!((map.streams(), map.coupled()), (6, 0));
		assert_eq!(map.table(), &[0, 1, 2, 3, 4, 5]);
	}

	/// A reserved code drops that PID. The program's other audio still imports.
	#[test]
	fn a_reserved_opus_channel_code_drops_only_that_stream() {
		let catalog = import_program(&opus_program(&[0x80, 0xff]));
		assert_eq!(catalog.audio.renditions.len(), 1, "only the MP2 track");
		let audio = catalog.audio.renditions.values().next().unwrap();
		assert_eq!(audio.codec.to_string(), "mp2");
	}

	/// Repeated PMTs are quiet, while streams added by a later PMT get their own report.
	#[test]
	#[tracing_test::traced_test]
	fn pmt_reports_each_new_stream_once() {
		use crate::container::ts::psi::{Es, Pmt};
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default()
				.with_catalog(crate::catalog::hang::Catalog::<crate::container::ts::catalog::Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let mut pmt = Pmt {
			program_number: 1,
			pcr_pid: Some(Pid::new(0x100).unwrap()),
			program_info: Vec::new(),
			streams: [0x1b, 0x06, 0x05, 0x86, 0x02]
				.into_iter()
				.enumerate()
				.map(|(index, stream_type)| Es {
					stream_type,
					pid: Pid::new(0x100 + index as u16).unwrap(),
					descriptors: Vec::new(),
				})
				.collect(),
		};
		import.handle_pmt(pmt.clone()).unwrap();
		import.handle_pmt(pmt.clone()).unwrap();
		pmt.streams.push(Es {
			stream_type: 0x0f,
			pid: Pid::new(0x105).unwrap(),
			descriptors: Vec::new(),
		});
		import.handle_pmt(pmt).unwrap();
		import.finish().unwrap();
		logs_assert(|lines| {
			let streams: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("TS elementary stream"))
				.collect();
			if streams.len() != 6 {
				return Err(format!("expected one report per new stream, got {streams:?}"));
			}
			for (pid, stream_type, outcome, carriage) in [
				(256, 0x1b, "track", "decoded"),
				(257, 0x06, "verbatim", "pesPacket"),
				(258, 0x05, "verbatim", "section"),
				(259, 0x86, "dropped", "none"),
				(260, 0x02, "verbatim", "pesPacket"),
				(261, 0x0f, "track", "decoded"),
			] {
				if !streams.iter().any(|line| {
					line.contains(&format!("pid={pid} "))
						&& line.contains(&format!("stream_type={stream_type} "))
						&& line.contains(&format!("outcome=\"{outcome}\""))
						&& line.contains(&format!("carriage=\"{carriage}\""))
				}) {
					return Err(format!("missing {outcome} report for PID {pid}: {streams:?}"));
				}
			}
			Ok(())
		});
	}

	/// A later PMT that changes a PID's route reports it again: a private PES promoted once a
	/// PCR appears, and a section PID refused without one remapped to a decoded codec.
	#[test]
	#[tracing_test::traced_test]
	fn pmt_reports_route_changes() {
		use crate::container::ts::psi::{Es, Pmt};
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default()
				.with_catalog(crate::catalog::hang::Catalog::<crate::container::ts::catalog::Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt = |pcr_pid: Option<u16>, section_type: u8| Pmt {
			program_number: 1,
			pcr_pid: pcr_pid.map(|pid| Pid::new(pid).unwrap()),
			program_info: Vec::new(),
			streams: vec![
				Es {
					stream_type: 0x06,
					pid: Pid::new(0x100).unwrap(),
					descriptors: Vec::new(),
				},
				Es {
					stream_type: section_type,
					pid: Pid::new(0x101).unwrap(),
					descriptors: Vec::new(),
				},
			],
		};
		import.handle_pmt(pmt(None, 0x05)).unwrap();
		import.handle_pmt(pmt(Some(0x100), 0x0f)).unwrap();
		import.handle_pmt(pmt(Some(0x100), 0x0f)).unwrap();
		import.finish().unwrap();
		logs_assert(|lines| {
			let streams: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("TS elementary stream"))
				.collect();
			let expected = [
				(256, 0x06, "dropped", "none"),
				(257, 0x05, "dropped", "none"),
				(256, 0x06, "verbatim", "pesPacket"),
				(257, 0x0f, "track", "decoded"),
			];
			if streams.len() != expected.len() {
				return Err(format!("expected one report per route, got {streams:?}"));
			}
			for (line, (pid, stream_type, outcome, carriage)) in streams.iter().zip(expected) {
				if !(line.contains(&format!("pid={pid} "))
					&& line.contains(&format!("stream_type={stream_type} "))
					&& line.contains(&format!("outcome=\"{outcome}\""))
					&& line.contains(&format!("carriage=\"{carriage}\"")))
				{
					return Err(format!("expected {outcome}/{carriage} for PID {pid}, got {line}"));
				}
			}
			Ok(())
		});
	}

	/// Section routes refuse a missing PCR PID, then return when a later PMT supplies one.
	#[tokio::test(start_paused = true)]
	async fn a_pmt_naming_or_dropping_the_pcr_reroutes_sections() {
		use crate::container::ts::psi::{Es, Pmt};
		for stream_type in [0x05, 0x86] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = crate::catalog::Producer::new(
				&mut broadcast,
				crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
			)
			.unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve());
			let pid = Pid::new(0x21).unwrap();
			let pmt = |pcr_pid| Pmt {
				program_number: 1,
				pcr_pid,
				program_info: vec![super::catalog::Descriptor {
					tag: 5,
					data: bytes::Bytes::from_static(b"CUEI"),
				}],
				streams: vec![
					Es {
						stream_type: 0x02,
						pid: Pid::new(PCR_PID).unwrap(),
						descriptors: Vec::new(),
					},
					Es {
						stream_type,
						pid,
						descriptors: Vec::new(),
					},
				],
			};
			import.handle_pmt(pmt(None)).unwrap();
			import.handle_pmt(pmt(None)).unwrap();
			assert_eq!(import.stats().streams[&0x21].refused, 1, "once per refused route");
			assert!(catalog.snapshot().ext.mpegts.tracks.is_empty());
			assert!(matches!(import.streams[&pid], super::Stream::Ignored));
			import.handle_pmt(pmt(Some(Pid::new(PCR_PID).unwrap()))).unwrap();
			let name = track_on(&catalog.snapshot().ext.mpegts, 0x21);
			let data = [
				pcr_packet(PCR_PID, 27_000_000),
				pes_packet(PCR_PID, 99_000),
				packet(true, 0, 0, &CUE),
				pcr_packet(PCR_PID, 29_700_000),
				pcr_packet(PCR_PID, 32_400_000),
			]
			.concat();
			import.decode(&data).unwrap();
			let track = consumer
				.track(&name)
				.unwrap()
				.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
				.await
				.unwrap();
			let mut reader = crate::container::Consumer::new(
				track,
				crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
			);
			assert_eq!(reader.read_frame().await.unwrap().unwrap().payload.as_ref(), CUE);
			import.handle_pmt(pmt(None)).unwrap();
			assert_eq!(import.stats().streams[&0x21].refused, 2);
			assert!(
				reader.read_frame().await.is_err(),
				"losing the PCR ends the track with an error"
			);
			assert!(catalog.snapshot().ext.mpegts.tracks.is_empty());
			import.handle_pmt(pmt(Some(Pid::new(PCR_PID).unwrap()))).unwrap();
			let next = track_on(&catalog.snapshot().ext.mpegts, 0x21);
			assert_ne!(next, name, "restoring a stream publishes a new track");
			import.finish().unwrap();
		}
	}

	/// A PID refused as private PES for want of a PCR routes again when a later PMT gives it a
	/// type that needs no PCR, though the program still has none.
	#[test]
	fn an_unclocked_pid_reassigned_to_a_decoded_type_routes_again() {
		use crate::container::ts::psi::{Es, Pmt};
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default()
				.with_catalog(crate::catalog::hang::Catalog::<crate::container::ts::catalog::Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pid = Pid::new(0x104).unwrap();
		let pmt = |stream_type| Pmt {
			program_number: 1,
			pcr_pid: None,
			program_info: Vec::new(),
			streams: vec![Es {
				stream_type,
				pid,
				descriptors: Vec::new(),
			}],
		};
		import.handle_pmt(pmt(0x06)).unwrap();
		assert!(matches!(import.streams[&pid], super::Stream::Ignored));
		import.handle_pmt(pmt(0x06)).unwrap();
		assert_eq!(import.stats().streams[&0x104].refused, 1, "refused once, not per PMT");
		import.handle_pmt(pmt(StreamType::H264 as u8)).unwrap();
		assert!(matches!(import.streams[&pid], super::Stream::H264 { .. }));
	}

	/// A PMT that changes a decoded stream's ES descriptors, here an audio language retag,
	/// finishes its track: its rendition and `mpegts` entry leave the catalog, and the next PES
	/// opens a rendition under a new name whose entry records the revised descriptors.
	#[test]
	fn a_relabelled_decoded_stream_reopens_under_a_new_name() {
		use crate::container::ts::psi::{Es, Pmt};
		const AUDIO_PID: u16 = 0x0061;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt = |language: &'static [u8]| Pmt {
			program_number: 1,
			pcr_pid: Some(Pid::new(AUDIO_PID).unwrap()),
			program_info: Vec::new(),
			streams: vec![Es {
				stream_type: MP2,
				pid: Pid::new(AUDIO_PID).unwrap(),
				descriptors: vec![super::catalog::Descriptor {
					tag: 0x0a,
					data: bytes::Bytes::from_static(language),
				}],
			}],
		};
		let language = |catalog: &crate::catalog::hang::Catalog<Ext>, name: &str| {
			catalog.ext.mpegts.tracks[name].descriptors[0].data.clone()
		};

		import.handle_pmt(pmt(b"eng\0")).unwrap();
		// The legacy path publishes once a later PES confirms the frames before it.
		import.decode(&mp2_pes(AUDIO_PID, 0, 90_000, [0xAA, 0xBB])).unwrap();
		import.decode(&mp2_pes(AUDIO_PID, 1, 180_000, [0xAA, 0xBB])).unwrap();
		let before = catalog.snapshot();
		let old = before
			.audio
			.renditions
			.keys()
			.next()
			.expect("the MP2 rendition")
			.clone();
		assert_eq!(language(&before, &old).as_ref(), b"eng\0");

		import.handle_pmt(pmt(b"fra\0")).unwrap();
		let between = catalog.snapshot();
		assert!(
			between.audio.renditions.is_empty(),
			"the old rendition left the catalog"
		);
		assert!(between.ext.mpegts.tracks.is_empty(), "the old entry left the catalog");

		import.decode(&mp2_pes(AUDIO_PID, 2, 270_000, [0xCC, 0xDD])).unwrap();
		import.decode(&mp2_pes(AUDIO_PID, 3, 360_000, [0xCC, 0xDD])).unwrap();
		let after = catalog.snapshot();
		let new = after
			.audio
			.renditions
			.keys()
			.next()
			.expect("the relabelled rendition")
			.clone();
		assert_ne!(new, old);
		assert_eq!(language(&after, &new).as_ref(), b"fra\0");
		import.finish().unwrap();
	}

	/// The program descriptors follow each PMT revision in place, and adding or removing the
	/// 'CUEI' registration reroutes the 0x86 PID: carried as cues under it, dropped without it,
	/// and carried under a new name when it returns.
	#[test]
	fn program_descriptors_follow_the_pmt_and_reroute_the_cue_pid() {
		use crate::container::ts::psi::{Es, Pmt};
		const CUE_PID: u16 = 0x0021;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let descriptor = |tag, data: &'static [u8]| super::catalog::Descriptor {
			tag,
			data: bytes::Bytes::from_static(data),
		};
		let pmt = |program_info: Vec<super::catalog::Descriptor>| Pmt {
			program_number: 1,
			pcr_pid: Some(Pid::new(PCR_PID).unwrap()),
			program_info,
			streams: vec![
				Es {
					stream_type: StreamType::Mpeg2Video as u8,
					pid: Pid::new(PCR_PID).unwrap(),
					descriptors: Vec::new(),
				},
				Es {
					stream_type: StreamType::Dts8ChannelLosslessAudio as u8,
					pid: Pid::new(CUE_PID).unwrap(),
					descriptors: Vec::new(),
				},
			],
		};
		let cues = || {
			let tracks = catalog.snapshot().ext.mpegts.tracks;
			tracks
				.into_iter()
				.find(|(_, track)| track.pid == CUE_PID)
				.map(|(name, _)| name)
		};

		let cuei = vec![descriptor(0x05, b"CUEI")];
		import.handle_pmt(pmt(cuei.clone())).unwrap();
		assert_eq!(catalog.snapshot().ext.mpegts.program_descriptors, cuei);
		let first = cues().expect("cues under CUEI");

		let private = vec![descriptor(0x80, &[0xaa, 0xbb])];
		import.handle_pmt(pmt(private.clone())).unwrap();
		assert_eq!(catalog.snapshot().ext.mpegts.program_descriptors, private);
		assert_eq!(cues(), None, "without CUEI the PID is not cues");
		assert!(matches!(
			import.streams.get(&Pid::new(CUE_PID).unwrap()),
			Some(super::Stream::Ignored)
		));

		import.handle_pmt(pmt(cuei.clone())).unwrap();
		assert_eq!(catalog.snapshot().ext.mpegts.program_descriptors, cuei);
		let second = cues().expect("cues under CUEI again");
		assert_ne!(second, first, "the returning cue stream gets a new name");

		import.handle_pmt(pmt(Vec::new())).unwrap();
		assert!(catalog.snapshot().ext.mpegts.program_descriptors.is_empty());
	}

	/// A PAT that names the program under another `transport_stream_id`, or a single-program
	/// import's PAT naming another program, refuses the import: the carried SI describes the
	/// first. A repeated PAT does not.
	#[test]
	fn a_pat_naming_another_identity_refuses_the_import() {
		use crate::container::ts::psi::{Association, Pat};
		let pat = |transport_stream_id, program_number| Pat {
			transport_stream_id,
			programs: vec![Association {
				program_number,
				pmt_pid: Pid::new(0x20).unwrap(),
			}],
		};
		for (selected, transport_stream_id, program_number) in [(None, 2, 1), (None, 1, 2), (Some(1), 2, 1)] {
			let (_catalog, import) = psi_import();
			let mut import = match selected {
				Some(program) => import.with_program(program),
				None => import,
			};
			import.handle_pat(&pat(1, 1)).unwrap();
			import.handle_pat(&pat(1, 1)).unwrap();
			let err = import
				.handle_pat(&pat(transport_stream_id, program_number))
				.expect_err("another identity is refused");
			assert!(err.to_string().contains("identity"), "{err:#}");
		}
	}

	/// A PAT that moves the PMT to another PID moves the import with it: the catalog records
	/// the new PID, the PMT there revises the program, and the old PID no longer speaks for it.
	#[test]
	fn a_moved_pmt_is_followed() {
		use super::super::fixture::Mux;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default()
				.with_catalog(crate::catalog::hang::Catalog::<crate::container::ts::catalog::Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt_pid = || {
			catalog
				.snapshot()
				.ext
				.mpegts
				.program
				.as_ref()
				.map(|program| program.pmt_pid)
		};

		let mut mux = Mux::default();
		mux.pat(&[(1, 0x100)]).pmt(0x100, 1, 0x61, &[(MP2, 0x61)], 0);
		mux.out.extend(mp2_pes(0x61, 0, 90_000, [0xAA, 0xBB]));
		import.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert_eq!(pmt_pid(), Some(0x100));

		mux.pat(&[(1, 0x200)])
			.pmt(0x200, 1, 0x61, &[(MP2, 0x61), (MP2, 0x62)], 0)
			.pmt(0x100, 1, 0x61, &[], 0);
		mux.out.extend(mp2_pes(0x61, 1, 108_000, [0xAA, 0xBB]));
		mux.out.extend(mp2_pes(0x62, 0, 108_000, [0xCC, 0xDD]));
		import.decode(&mux.out).unwrap();
		import.finish().unwrap();
		assert_eq!(pmt_pid(), Some(0x200));
		assert_eq!(
			catalog.snapshot().audio.renditions.len(),
			2,
			"the PMT on the new PID adds a stream, and the one on the old PID removes none"
		);
	}

	/// A section-framed PID the PMT drops keeps its stats row, so what it delivered stays counted.
	#[test]
	fn a_dropped_section_route_keeps_its_stats_row() {
		const VIDEO_PID: u16 = 0x0050;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let streams = [
			(StreamType::Mpeg2Video, VIDEO_PID),
			(StreamType::Dts8ChannelLosslessAudio, 0x21),
		];
		let mut bytes = synth_pmt(&streams, true);
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 27_000_000));
		bytes.extend_from_slice(&pes_packet(VIDEO_PID, 99_000));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 29_700_000));
		bytes.extend_from_slice(&synth_pmt(&streams[..1], true));
		import.decode(&bytes).unwrap();

		let row = &import.stats().streams[&0x21];
		assert_eq!((row.units, row.quiet), (1, None));
	}

	/// A PMT dropping a section PID before any media timestamp anchors the clock refuses the
	/// import, as end of input does, rather than drop the section it holds.
	#[test]
	fn a_section_route_dropped_before_the_media_clock_refuses_the_import() {
		const VIDEO_PID: u16 = 0x0050;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let streams = [
			(StreamType::Mpeg2Video, VIDEO_PID),
			(StreamType::Dts8ChannelLosslessAudio, 0x21),
		];
		let mut bytes = synth_pmt(&streams, true);
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 27_000_000));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 29_700_000));
		import.decode(&bytes).unwrap();

		let err = import.decode(&synth_pmt(&streams[..1], true)).unwrap_err();
		assert!(err.to_string().contains("without a media timestamp"), "{err:#}");
		assert!(import.sections.contains_key(&0x21), "the route is left for `abort`");
	}

	/// A PMT dropping a section PID past a lone PCR ends it as end of input would: the section
	/// no PCR interval places is counted unplaced.
	#[test]
	fn a_section_route_dropped_past_a_lone_pcr_counts_its_section_unplaced() {
		const VIDEO_PID: u16 = 0x0050;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let streams = [
			(StreamType::Mpeg2Video, VIDEO_PID),
			(StreamType::Dts8ChannelLosslessAudio, 0x21),
		];
		let mut bytes = synth_pmt(&streams, true);
		bytes.extend_from_slice(&pcr_packet(VIDEO_PID, 27_000_000));
		bytes.extend_from_slice(&pes_packet(VIDEO_PID, 99_000));
		bytes.extend_from_slice(&packet(true, 0, 0, &CUE));
		bytes.extend_from_slice(&synth_pmt(&streams[..1], true));
		import.decode(&bytes).unwrap();

		let row = &import.stats().streams[&0x21];
		assert_eq!((row.units, row.unplaced), (0, 1));
		assert!(!import.sections.contains_key(&0x21));
	}

	/// A PID decoded as audio, then revised to private sections, then dropped, reports the
	/// section route it ended on.
	#[test]
	fn a_dropped_route_reports_the_route_it_ended_on() {
		use crate::container::ts::psi::{Es, Pmt};
		const AUDIO_PID: u16 = 0x0062;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt = |streams: &[u8]| Pmt {
			program_number: 1,
			pcr_pid: Some(Pid::new(PCR_PID).unwrap()),
			program_info: Vec::new(),
			streams: streams
				.iter()
				.map(|&stream_type| Es {
					stream_type,
					pid: Pid::new(AUDIO_PID).unwrap(),
					descriptors: Vec::new(),
				})
				.collect(),
		};

		import.handle_pmt(pmt(&[MP2])).unwrap();
		import.decode(&mp2_pes(AUDIO_PID, 0, 90_000, [0xAA, 0xBB])).unwrap();
		import.decode(&mp2_pes(AUDIO_PID, 1, 180_000, [0xAA, 0xBB])).unwrap();
		import.handle_pmt(pmt(&[0x05])).unwrap();
		import.handle_pmt(pmt(&[])).unwrap();

		let row = &import.stats().streams[&AUDIO_PID];
		assert_eq!((row.track.as_str(), row.class), (".ts", super::stats::Class::Data));
	}

	/// Ending a self-describing audio route drains the frame it held for a successor, and its
	/// counters include that frame.
	#[test]
	fn a_dropped_audio_route_counts_the_frame_its_end_drains() {
		use crate::container::ts::psi::{Es, Pmt};
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt = |streams: Vec<Es>| Pmt {
			program_number: 1,
			pcr_pid: Some(Pid::new(PCR_PID).unwrap()),
			program_info: Vec::new(),
			streams,
		};
		let audio = Es {
			stream_type: MP2,
			pid: Pid::new(PCR_PID).unwrap(),
			descriptors: Vec::new(),
		};

		// The second frame spans two PES, so it begins in a carried tail and is held for a
		// successor header that never comes.
		let (mut head, second) = (mp2_frame(0xAA), mp2_frame(0xBB));
		head.extend_from_slice(&second[..36]);
		import.handle_pmt(pmt(vec![audio])).unwrap();
		import.decode(&audio_pes_packet(PCR_PID, 0, 90_000, &head)).unwrap();
		import
			.decode(&audio_pes_packet(PCR_PID, 1, 92_160, &second[36..]))
			.unwrap();
		assert_eq!(import.stats().streams[&PCR_PID].unconfirmed, 0);
		import.handle_pmt(pmt(Vec::new())).unwrap();

		assert_eq!(import.stats().streams[&PCR_PID].unconfirmed, 1);
	}

	/// A source revising its PMT without end holds only its live routes: each revision ends a
	/// decoded or verbatim route, refuses one for a lost PCR, or rewrites the program descriptors,
	/// and none leaves its track's HLS records or catalog entry behind.
	#[test]
	fn pmt_churn_keeps_flat_state() {
		use crate::container::ts::psi::{Es, Pmt};
		// Each round of three revisions ends where it started.
		const ROUNDS: u32 = 333;
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve());
		let pmt = |revision: u32| {
			// A decoded H.264 route, a private PES route, then the same one refused for want of a PCR.
			let (stream_type, pcr) = match revision % 3 {
				0 => (StreamType::H264 as u8, true),
				1 => (0x06, true),
				_ => (0x06, false),
			};
			Pmt {
				program_number: 1,
				pcr_pid: pcr.then(|| Pid::new(PCR_PID).unwrap()),
				program_info: vec![super::catalog::Descriptor {
					tag: 0x80,
					data: bytes::Bytes::copy_from_slice(&revision.to_be_bytes()),
				}],
				streams: vec![Es {
					stream_type,
					pid: Pid::new(0x0062).unwrap(),
					descriptors: Vec::new(),
				}],
			}
		};
		let state = |import: &super::Import<Ext>| {
			let snapshot = catalog.snapshot();
			(
				catalog.hls_tracks(),
				snapshot.ext.mpegts.tracks.len(),
				import.streams.len() + import.sections.len() + import.listed.len() + import.retired_stats.len(),
			)
		};

		for revision in 0..6 {
			import.handle_pmt(pmt(revision)).unwrap();
		}
		let settled = state(&import);
		for revision in 6..3 * ROUNDS {
			import.handle_pmt(pmt(revision)).unwrap();
		}
		assert_eq!(state(&import), settled);
	}

	/// The first PMT records its program descriptors over whatever the catalog held, even when it
	/// carries none.
	#[test]
	fn the_first_pmt_records_its_program_descriptors_even_when_empty() {
		use crate::container::ts::psi::{Es, Pmt};
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		catalog.modify().unwrap().ext.mpegts.program_descriptors = vec![super::catalog::Descriptor {
			tag: 0x80,
			data: bytes::Bytes::from_static(&[0xaa]),
		}];
		let mut import = super::Import::new(broadcast, catalog.reserve());
		import
			.handle_pmt(Pmt {
				program_number: 1,
				pcr_pid: Some(Pid::new(PCR_PID).unwrap()),
				program_info: Vec::new(),
				streams: vec![Es {
					stream_type: StreamType::Mpeg2Video as u8,
					pid: Pid::new(PCR_PID).unwrap(),
					descriptors: Vec::new(),
				}],
			})
			.unwrap();

		assert!(catalog.snapshot().ext.mpegts.program_descriptors.is_empty());
	}
}
