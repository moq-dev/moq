//! Carrying a transport stream whole, without demultiplexing it.
//!
//! [`Passthrough`] publishes every 188-byte packet of its input, in order, on one track named by
//! the catalog's [`m2ts`](hang::catalog::M2ts) section. It reads the PAT and PMT to find the
//! program clock and the video stream, and reads nothing else past an adaptation field, so a
//! scrambled multiplex groups and paces exactly like its clear twin.
//!
//! # Objects and groups
//!
//! An object runs from one PCR packet on the clock PID to the next, and is also cut wherever a
//! group starts. A group starts at:
//!
//! - a packet with `random_access_indicator` on the video PID, or on the clock PID when the
//!   program has no video;
//! - a PCR with `discontinuity_indicator` that jumps forward, since no group may hold a PCR
//!   discontinuity;
//! - the first PCR at least [`FALLBACK`] after the group's start, for a source that never marks
//!   random access;
//! - the first object that would take the group past [`GROUP_BYTES_MAX`] or
//!   [`GROUP_OBJECTS_MAX`], so a long GOP cannot outgrow what a relay caches. Objects are never
//!   split to meet the bound.
//!
//! Only the first of these makes a group a random access point, so any other start publishes
//! [`random_access`](hang::catalog::M2ts::random_access) false from then on, as does any group
//! carried while the layout allows no random access (more than one program or video stream). A
//! group the PCR discontinuity starts is behind a break even when it is also a random access
//! point.
//!
//! Bytes before the first group are dropped: until the PMT, a PCR, and a group start have all
//! been seen there is no group to put them in. The track is byte-identical to the input from its
//! first group on, and only from there. Bytes off the 188-byte packet grid are dropped too.
//!
//! # Timestamps
//!
//! An object's timestamp is the PCR time of its first byte, interpolated at the rate between the
//! PCRs either side of it, floored to microseconds. A PCR stamps the byte holding the last bit of
//! its base, ten bytes into its packet, so even an object that starts at a PCR packet sits ten
//! bytes of that rate before its own PCR.
//!
//! Every timestamp is then shifted onto the catalog's [`Clock`](crate::Clock) by one offset, fixed
//! through the reservation's [`Timebase`](crate::catalog::Timebase) by the first object: zero when
//! that object places the clock, and otherwise whatever lands it at now on a clock already in use.
//!
//! PCRs are unwrapped into a 64-bit count of 27 MHz ticks that never goes back. A step forward of
//! at most half the 2^33 * 300 tick period adds to the count, across the wrap included. A PCR with
//! `discontinuity_indicator` whose step is forward by that measure adds the step too, after the
//! bytes before it are timed at the rate before it, and starts a group behind a break marker. Any
//! step back ends the import: [`PcrRewindError`] without the flag, [`PcrRestartError`] with it,
//! since a signalled restart is new content and needs a new broadcast. The timeline therefore
//! depends only on the bytes since the importer's first PCR and its shift, and two importers of one
//! feed started at the same packet each placing its own catalog's clock publish identical objects.

use std::collections::{BTreeMap, HashMap, VecDeque};

use bytes::BytesMut;
use mpeg2ts::ts::{Pid, TsPacket};

use super::import::{Framer, PatReader, SectionReassembler, adaptation_valid, discontinuity_indicator, pcr};
use super::mux_rate::{Meter, PCR_WRAP};
use super::psi::{self, Pmt};

/// The PCR clock, in Hz.
const PCR_HZ: u64 = 27_000_000;

/// The longest a group runs without a random access point before a PCR starts the next.
pub const FALLBACK: std::time::Duration = std::time::Duration::from_secs(1);
const FALLBACK_TICKS: u64 = PCR_HZ;

/// The most bytes a group holds before its next object starts another: half of what `moq-net`
/// caches of one group, so the frame headers and an object of slack never reach the cap.
pub const GROUP_BYTES_MAX: u64 = moq_net::group::MAX_CACHE_BYTES / 2;

/// The most objects a group holds before its next object starts another: half of `moq-net`'s
/// frame cap, which a source stamping PCRs far more often than the 40 ms norm could reach.
pub const GROUP_OBJECTS_MAX: usize = moq_net::group::MAX_GROUP_FRAMES / 2;

/// The bytes a PCR sits into its packet: four of header, the adaptation field's length and flags,
/// and the five bytes the 33-bit base ends in.
const PCR_OFFSET: u64 = 10;

/// The packet sizes a sync byte can recur at: 188, BDAV's 192, and 204 with Reed-Solomon parity.
const PACKET_SIZES: [usize; 3] = [TsPacket::SIZE, 192, 204];

/// The sync bytes in a row, at one stride, that settle the packet size: payload rarely fakes that.
const SYNC_RUN: usize = 4;

/// The bytes from a candidate first sync byte to the last one a run of the widest stride needs.
const SYNC_WINDOW: usize = 204 * (SYNC_RUN - 1) + 1;

/// The stream types a PMT declares video with. A video stream carried under a private stream
/// type (0x06 with a registration descriptor) is not recognized, and its program groups on the
/// clock PID's random access points instead.
const VIDEO_STREAM_TYPES: &[u8] = &[
	0x01, // MPEG-1 video
	0x02, // MPEG-2 video
	0x10, // MPEG-4 part 2
	0x1B, // H.264
	0x1F, // H.264 SVC sub-bitstream
	0x20, // H.264 MVC sub-bitstream
	0x24, // H.265
	0x33, // H.266
	0x42, // AVS
	0xD1, // Dirac
	0xEA, // VC-1
];

/// The input is not a stream of 188-byte packets.
///
/// A 192-byte source (BDAV M2TS, a four-byte timestamp before each packet) or a 204-byte one
/// (Reed-Solomon parity after each) is refused rather than reframed, since dropping those bytes
/// would break the byte identity passthrough exists for.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("transport stream uses {size}-byte packets; passthrough carries 188-byte packets only")]
pub struct PacketSizeError {
	/// The packet size the sync bytes recur at.
	pub size: usize,
}

/// The clock PID's PCR went backwards without `discontinuity_indicator`.
///
/// The source broke its own timebase without saying so, which leaves no timestamp that both
/// follows the PCR and stays on one timeline.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("PCR on PID {pid} went back from {from} to {to} without a discontinuity_indicator")]
pub struct PcrRewindError {
	/// The clock PID.
	pub pid: u16,
	/// The previous PCR, raw 27 MHz ticks.
	pub from: u64,
	/// The PCR that stepped back, raw 27 MHz ticks.
	pub to: u64,
}

/// The clock PID's PCR went backwards with `discontinuity_indicator`: the source restarted.
///
/// What follows is new content, which belongs in a new broadcast rather than on a timeline that
/// steps back, so the import ends here.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("PCR on PID {pid} restarted from {from} back to {to}; a restart needs a new broadcast")]
pub struct PcrRestartError {
	/// The clock PID.
	pub pid: u16,
	/// The previous PCR, raw 27 MHz ticks.
	pub from: u64,
	/// The flagged PCR, raw 27 MHz ticks.
	pub to: u64,
}

/// The clock PID carried no PCR across more bytes than a group may hold.
///
/// Those bytes cannot be timed, and holding them for a PCR that may never come is unbounded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("no PCR on PID {pid} for {bytes} bytes")]
pub struct PcrGapError {
	/// The clock PID.
	pub pid: u16,
	/// The bytes waiting on a PCR.
	pub bytes: u64,
}

/// Publishes a transport stream verbatim, as one track of whole 188-byte packets.
///
/// See the [module documentation](self) for where objects and groups start and how they are
/// timestamped.
pub struct Passthrough<E: crate::catalog::hang::CatalogExt = ()> {
	catalog: crate::catalog::Producer<E>,
	/// Held until the first group is written, so the first catalog already names the track with
	/// its final `randomAccess`.
	reservation: Option<crate::catalog::Reserved<E>>,
	/// Shifts every object's PCR time onto the catalog clock, anchored by the first object.
	shift: crate::catalog::Timebase<E>,
	media: crate::container::Producer<crate::catalog::hang::Container>,
	/// The section as last written to the catalog, `None` before the first group.
	section: Option<Section<E>>,

	scratch: Vec<u8>,
	framer: Framer,
	/// Set once the packet size has been confirmed as 188.
	sized: bool,
	/// Bytes of whole packets seen so far: the position every PCR and object is placed at.
	offset: u64,

	pat: PatReader,
	crc_errors: u64,
	/// The programs the PAT lists, in PAT order, without the network PID.
	programs: Vec<psi::Association>,
	pmt_sections: HashMap<Pid, SectionReassembler>,
	/// Each listed program's clock and video PIDs, once its PMT has been read.
	pmts: BTreeMap<u16, Program>,
	/// The PCR PID given by [`with_pcr_pid`](Self::with_pcr_pid), overriding the PMT's.
	pcr_override: Option<u16>,
	/// The PID pacing the multiplex.
	pcr_pid: Option<u16>,
	/// The PID whose `random_access_indicator` starts a group.
	access_pid: Option<u16>,
	/// Whether the layout allows random access at all: one program, at most one video PID, and
	/// that program's own clock.
	single: bool,
	/// Whether the PMT of the clock's program has been read.
	mapped: bool,

	timebase: Option<Timebase>,
	/// A `discontinuity_indicator` on the clock PID, waiting for the PCR that it flags (normally
	/// in the same packet).
	pending_break: bool,
	/// Before the first group: the PCR time the importer started waiting for a group start at.
	waiting_since: Option<u64>,
	/// The PCR time of the last object written, which no later one goes below.
	written: u64,

	/// The object being filled, from its first packet.
	open: Option<Object>,
	/// Closed objects waiting on the next PCR for their timestamps.
	queue: VecDeque<Object>,
	group: Option<Group>,
	/// Whether every group so far started at a random access point, and the layout allowed random
	/// access throughout.
	all_random_access: bool,

	mux_rate: Meter,
}

/// Owns the `m2ts` catalog section once written, removing it however the track ends, as the
/// demultiplexed lane's catalog entries are.
struct Section<E: crate::catalog::hang::CatalogExt> {
	catalog: crate::catalog::Producer<E>,
	written: hang::catalog::M2ts,
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Section<E> {
	fn drop(&mut self) {
		// A closed catalog has nothing left to remove it from.
		let _ = self.catalog.mutate(|catalog| catalog.m2ts = None);
	}
}

#[derive(Default)]
struct Program {
	pcr_pid: Option<u16>,
	video: Vec<u16>,
}

/// The PCRs since the importer's first, or since the last flagged jump.
struct Timebase {
	/// The last PCR, raw.
	raw: u64,
	/// The last few PCRs, newest last: enough to time every object since the one before last.
	samples: VecDeque<Sample>,
}

#[derive(Clone, Copy)]
struct Sample {
	/// Where the PCR's byte sits in the input.
	at: u64,
	/// The unwrapped PCR, in 27 MHz ticks.
	ticks: u64,
}

struct Object {
	/// Where the object's first byte sits in the input.
	at: u64,
	payload: BytesMut,
	/// Why this object starts a group, if it does.
	start: Option<Start>,
	/// Whether a flagged PCR jump comes before this object, which the track marks with a break
	/// whatever else starts its group.
	discontinuity: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Start {
	RandomAccess,
	Discontinuity,
	Fallback,
	/// The group before would have outgrown [`GROUP_BYTES_MAX`] or [`GROUP_OBJECTS_MAX`].
	Full,
}

struct Group {
	/// The PCR time of the group's first byte.
	ticks: u64,
	bytes: u64,
	objects: usize,
}

impl<E: crate::catalog::hang::CatalogExt> Passthrough<E> {
	/// A passthrough importer publishing one track on `broadcast`, listed in the catalog behind
	/// `reserved`.
	pub fn new(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> crate::Result<Self> {
		let catalog = reserved.producer();
		let track = broadcast.unique_track(".m2ts", catalog.track_info(hang::catalog::PRIORITY.video))?;
		let media = catalog.media_raw(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
		)?;
		Ok(Self {
			catalog,
			shift: reserved.timebase(),
			reservation: Some(reserved),
			media,
			section: None,
			scratch: Vec::new(),
			framer: Framer::default(),
			sized: false,
			offset: 0,
			pat: PatReader::default(),
			crc_errors: 0,
			programs: Vec::new(),
			pmt_sections: HashMap::new(),
			pmts: BTreeMap::new(),
			pcr_override: None,
			pcr_pid: None,
			access_pid: None,
			single: false,
			mapped: false,
			timebase: None,
			pending_break: false,
			waiting_since: None,
			written: 0,
			open: None,
			queue: VecDeque::new(),
			group: None,
			all_random_access: true,
			mux_rate: Meter::default(),
		})
	}

	/// Pace the multiplex on `pid` rather than on the `PCR_PID` of the PAT's first program.
	///
	/// Groups then start at random access points of the video stream of the first program whose
	/// PMT names `pid` as its clock. When no program does, they start at `pid`'s own random access
	/// points, which mark no program's decoding boundary, so the track never claims random access.
	pub fn with_pcr_pid(mut self, pid: u16) -> Self {
		self.pcr_override = Some(pid);
		self.pcr_pid = Some(pid);
		self
	}

	/// The name of the track carrying the packets.
	pub fn track(&self) -> &str {
		self.media.name()
	}

	/// Append `data` and publish every whole packet it completes. A trailing partial packet is
	/// kept for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		self.scratch.extend_from_slice(data);
		if !self.sized {
			self.probe_packet_size(false)?;
		}
		self.packets()
	}

	/// Publish what is left at the end of the input and close the track.
	///
	/// No later PCR will arrive, so the objects since the last one are timed at the rate of the
	/// interval before it. The catalog keeps the `m2ts` section until the importer drops.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		if !self.sized {
			self.probe_packet_size(true)?;
			self.packets()?;
		}
		self.close_object();
		self.drain_queue()?;
		self.reservation = None;
		self.media.finish()?;
		Ok(())
	}

	/// Abort the track with `err` instead of finishing, so subscribers see the real cause rather
	/// than [`moq_net::Error::Dropped`]. Consumes the importer.
	pub fn abort(self, err: moq_net::Error) {
		let Self { media, section, .. } = self;
		media.abort(err);
		drop(section);
	}

	/// Publish every whole packet in the scratch buffer, once the packet size is settled.
	fn packets(&mut self) -> anyhow::Result<()> {
		if !self.sized {
			return Ok(());
		}
		let mut off = 0;
		while let Some(at) = self.framer.next(&self.scratch, &mut off) {
			let pkt: [u8; TsPacket::SIZE] = self.scratch[at..at + TsPacket::SIZE].try_into().unwrap();
			self.packet(&pkt)?;
		}
		self.scratch.drain(..off);
		Ok(())
	}

	/// Settle the packet size before the 188-byte framer can lock onto a false grid, refusing a
	/// 192- or 204-byte source, and drop the bytes before the first sync byte of the grid.
	///
	/// Each offset is tried, in order, once the bytes a run of every stride needs from it have
	/// arrived, so where the grid is found does not depend on how the input was chunked. At the
	/// end of the input a shorter run settles the offsets left, so a stream too short for a full
	/// run still publishes.
	fn probe_packet_size(&mut self, eof: bool) -> anyhow::Result<()> {
		let scratch = &self.scratch;
		let syncs = |k: usize, size: usize, run: usize| (0..run).all(|i| scratch.get(k + i * size) == Some(&0x47));
		let checkable = (scratch.len() + 1).saturating_sub(SYNC_WINDOW);

		let mut found = (0..checkable).find_map(|k| {
			PACKET_SIZES
				.iter()
				.find(|&&size| syncs(k, size, SYNC_RUN))
				.map(|&size| (k, size))
		});
		if found.is_none() && eof {
			// Every sync byte at the stride that still fits in what is left.
			let fits = |k: usize, size: usize| (scratch.len() - k).div_ceil(size);
			found = (checkable..scratch.len())
				.filter(|&k| scratch.len() - k >= TsPacket::SIZE)
				.find_map(|k| {
					PACKET_SIZES
						.iter()
						.find(|&&size| {
							fits(k, size) >= 2 - usize::from(size == TsPacket::SIZE) && syncs(k, size, fits(k, size))
						})
						.map(|&size| (k, size))
				});
		}

		match found {
			Some((k, TsPacket::SIZE)) => {
				self.scratch.drain(..k);
				self.sized = true;
			}
			Some((_, size)) => return Err(PacketSizeError { size }.into()),
			// No grid starts at any offset tried: leading junk.
			None => {
				self.scratch.drain(..checkable);
			}
		}
		Ok(())
	}

	fn packet(&mut self, pkt: &[u8; TsPacket::SIZE]) -> anyhow::Result<()> {
		let at = self.offset;
		self.offset += TsPacket::SIZE as u64;
		let pid = u16::from_be_bytes([pkt[1] & 0x1f, pkt[2]]);

		if pid == Pid::PAT {
			self.pat_packet(pkt)?;
		} else if let Ok(pmt_pid) = Pid::new(pid)
			&& self.pmt_sections.contains_key(&pmt_pid)
		{
			self.pmt_packet(pmt_pid, pkt)?;
		}
		self.mux_rate.packet();

		// A packet the demodulator flagged corrupt, or whose adaptation field overruns it, is
		// carried like any other, but nothing is read from it.
		let readable = pkt[1] & 0x80 == 0 && adaptation_valid(pkt);
		let mut start = None;
		let mut clock = false;
		let mut discontinuity = false;
		if readable && self.pcr_pid == Some(pid) {
			if discontinuity_indicator(pkt) {
				self.pending_break = true;
			}
			if let Some(raw) = pcr(pkt) {
				clock = true;
				start = self.clock(pid, raw, at)?;
				discontinuity = start == Some(Start::Discontinuity);
			}
		}
		if readable && self.access_pid == Some(pid) && random_access_indicator(pkt) && self.can_start() {
			start = Some(Start::RandomAccess);
		}

		if start.is_some() || (clock && self.started()) {
			if !clock {
				self.close_object();
			}
			let mut payload = BytesMut::with_capacity(TsPacket::SIZE * 8);
			payload.extend_from_slice(pkt);
			self.open = Some(Object {
				at,
				payload,
				start,
				discontinuity,
			});
		} else if let Some(open) = self.open.as_mut() {
			open.payload.extend_from_slice(pkt);
		}

		let waiting = self.open.as_ref().map_or(0, |open| open.payload.len() as u64)
			+ self.queue.iter().map(|object| object.payload.len() as u64).sum::<u64>();
		if waiting > GROUP_BYTES_MAX {
			return Err(PcrGapError {
				pid: self.pcr_pid.unwrap_or_default(),
				bytes: waiting,
			}
			.into());
		}
		Ok(())
	}

	/// A PCR at `at` on the clock PID: close the object it ends, time everything waiting on it,
	/// and say whether its packet starts a group.
	fn clock(&mut self, pid: u16, raw: u64, at: u64) -> anyhow::Result<Option<Start>> {
		let at = at + PCR_OFFSET;
		self.close_object();
		let flagged = std::mem::take(&mut self.pending_break);

		let Some(timebase) = self.timebase.as_mut() else {
			// The first PCR starts the count at its own value.
			self.timebase = Some(Timebase {
				raw,
				samples: VecDeque::from([Sample { at, ticks: raw }]),
			});
			self.waiting_since = self.mapped.then_some(raw);
			if self.mux_rate.pcr(raw) {
				self.record()?;
			}
			return Ok(None);
		};

		// A retransmitted break (the same flagged PCR again) declares nothing new.
		let flagged = flagged && !(timebase.samples.len() == 1 && timebase.raw == raw);
		let step = (raw + PCR_WRAP - timebase.raw) % PCR_WRAP;
		if step > PCR_WRAP / 2 {
			let from = timebase.raw;
			return Err(match flagged {
				true => PcrRestartError { pid, from, to: raw }.into(),
				false => PcrRewindError { pid, from, to: raw }.into(),
			});
		}
		let last = timebase.samples.back().expect("a timebase has a sample").ticks;

		if flagged {
			// No PCR on the old side is coming: what waits on one is timed by the rate before it,
			// and the new side carries on from there.
			self.drain_queue()?;
			let ticks = (last + step).max(self.written);
			self.timebase = Some(Timebase {
				raw,
				samples: VecDeque::from([Sample { at, ticks }]),
			});
			if self.mux_rate.discontinuity() {
				self.record()?;
			}
			if self.mux_rate.pcr(raw) {
				self.record()?;
			}
			// The jump is not elapsed time, so the wait for the first group starts over.
			if self.waiting_since.is_some() {
				self.waiting_since = Some(ticks);
			}
			return Ok(self.started().then_some(Start::Discontinuity));
		}

		let ticks = last + step;
		timebase.raw = raw;
		timebase.samples.push_back(Sample { at, ticks });
		if timebase.samples.len() > 3 {
			timebase.samples.pop_front();
		}
		self.drain_queue()?;
		if self.mux_rate.pcr(raw) {
			self.record()?;
		}

		let since = match &self.group {
			Some(group) => Some(group.ticks),
			None if self.mapped => Some(*self.waiting_since.get_or_insert(ticks)),
			None => None,
		};
		Ok(since
			.filter(|since| ticks.saturating_sub(*since) >= FALLBACK_TICKS && self.can_start())
			.map(|_| Start::Fallback))
	}

	/// Whether a group may start here: once the first one has, always; before it, only once the
	/// clock's program is mapped and a PCR has been seen.
	fn can_start(&self) -> bool {
		self.started() || (self.mapped && self.timebase.is_some())
	}

	/// Whether the first group has started, written or still waiting on its timestamp.
	fn started(&self) -> bool {
		self.group.is_some() || self.open.is_some() || !self.queue.is_empty()
	}

	fn close_object(&mut self) {
		if let Some(object) = self.open.take() {
			self.queue.push_back(object);
		}
	}

	/// Time and write every waiting object against the current timebase.
	fn drain_queue(&mut self) -> anyhow::Result<()> {
		while let Some(object) = self.queue.pop_front() {
			let timebase = self.timebase.as_ref().expect("an object follows a PCR");
			let ticks = locate(object.at, &timebase.samples);
			self.write(object, ticks)?;
		}
		Ok(())
	}

	fn write(&mut self, object: Object, ticks: u64) -> anyhow::Result<()> {
		// Extrapolating past the last PCR before a small flagged jump can overshoot the jump.
		let ticks = ticks.max(self.written);
		self.written = ticks;
		let timestamp = moq_net::Timestamp::from_micros(ticks / (PCR_HZ / 1_000_000))?;
		// The first object, which starts a group, anchors the shift and is live on arrival.
		let timestamp = self.shift.shift(timestamp)?;
		let size = object.payload.len() as u64;
		let start = object.start.or_else(|| {
			self.group
				.as_ref()
				.filter(|group| group.bytes + size > GROUP_BYTES_MAX || group.objects >= GROUP_OBJECTS_MAX)
				.map(|_| Start::Full)
		});

		if let Some(start) = start {
			if object.discontinuity && self.group.is_some() {
				self.media.discontinuity()?;
			}
			if start != Start::RandomAccess || !self.single {
				self.all_random_access = false;
			}
			self.group = Some(Group {
				ticks,
				bytes: 0,
				objects: 0,
			});
		}

		// Only an object that starts a group is ever opened before the first one.
		let group = self.group.as_mut().expect("the first object starts a group");
		group.bytes += size;
		group.objects += 1;
		self.media.write(crate::container::Frame {
			timestamp,
			duration: None,
			payload: object.payload.freeze(),
			keyframe: start.is_some(),
		})?;
		if start.is_some() {
			self.record()?;
			self.reservation = None;
		}
		Ok(())
	}

	/// Write the section to the catalog if it changed. Nothing is written before the first group.
	fn record(&mut self) -> anyhow::Result<()> {
		if self.group.is_none() && self.section.is_none() {
			return Ok(());
		}
		let mut section = hang::catalog::M2ts::new(self.media.name());
		section.random_access = self.single && self.all_random_access;
		section.mux_rate = self.mux_rate.published();
		if self.section.as_ref().is_some_and(|written| written.written == section) {
			return Ok(());
		}
		self.catalog.mutate(|catalog| catalog.m2ts = Some(section.clone()))?;
		match &mut self.section {
			Some(written) => written.written = section,
			None => {
				self.section = Some(Section {
					catalog: self.catalog.clone(),
					written: section,
				})
			}
		}
		Ok(())
	}

	fn pat_packet(&mut self, pkt: &[u8; TsPacket::SIZE]) -> anyhow::Result<()> {
		let Some(pat) = self.pat.push(pkt, &mut self.crc_errors) else {
			return Ok(());
		};
		let programs: Vec<_> = pat
			.programs
			.into_iter()
			.filter(|program| program.program_number != 0)
			.collect();
		if programs == self.programs {
			return Ok(());
		}
		self.pmt_sections
			.retain(|pid, _| programs.iter().any(|program| program.pmt_pid == *pid));
		for program in &programs {
			self.pmt_sections.entry(program.pmt_pid).or_default();
		}
		self.pmts
			.retain(|number, _| programs.iter().any(|program| program.program_number == *number));
		self.programs = programs;
		self.remap()
	}

	fn pmt_packet(&mut self, pid: Pid, pkt: &[u8; TsPacket::SIZE]) -> anyhow::Result<()> {
		let mut sections = Vec::new();
		self.pmt_sections.entry(pid).or_default().push(pkt, &mut sections);
		let mut changed = false;
		for section in sections {
			if section.first() != Some(&psi::PMT_TABLE_ID) || !psi::crc_ok(&section) {
				continue;
			}
			let Some(pmt) = Pmt::parse(&section) else {
				continue;
			};
			let listed = self
				.programs
				.iter()
				.any(|program| program.program_number == pmt.program_number && program.pmt_pid == pid);
			if !listed {
				continue;
			}
			let program = Program {
				pcr_pid: pmt.pcr_pid.map(|pid| pid.as_u16()),
				video: pmt
					.streams
					.iter()
					.filter(|es| VIDEO_STREAM_TYPES.contains(&es.stream_type))
					.map(|es| es.pid.as_u16())
					.collect(),
			};
			let known = self.pmts.get(&pmt.program_number);
			if known.is_none_or(|known| known.pcr_pid != program.pcr_pid || known.video != program.video) {
				self.pmts.insert(pmt.program_number, program);
				changed = true;
			}
		}
		if changed {
			self.remap()?;
		}
		Ok(())
	}

	/// The PAT or a PMT changed: find the clock, the PID that marks random access, and whether
	/// the layout allows random access at all.
	fn remap(&mut self) -> anyhow::Result<()> {
		let clock = match self.pcr_override {
			Some(pid) => self
				.programs
				.iter()
				.find(|program| {
					self.pmts
						.get(&program.program_number)
						.is_some_and(|pmt| pmt.pcr_pid == Some(pid))
				})
				.or(self.programs.first()),
			None => self.programs.first(),
		};
		let pmt = clock.and_then(|program| self.pmts.get(&program.program_number));

		let pcr_pid = self.pcr_override.or(pmt.and_then(|pmt| pmt.pcr_pid));
		// Whether the clock is the mapped program's own. An overriding clock no program names
		// marks its own random access points, which are no program's decoding boundary.
		let owned = pmt.is_some_and(|pmt| pmt.pcr_pid == pcr_pid);
		let access_pid = match pmt {
			Some(pmt) if owned => pmt.video.first().copied().or(pcr_pid),
			_ => pcr_pid,
		};
		let mapped = pmt.is_some();
		let single = self.programs.len() == 1 && owned && pmt.is_some_and(|pmt| pmt.video.len() <= 1);

		if pcr_pid != self.pcr_pid {
			// A new clock PID is a new clock: its next PCR is taken as flagged.
			if self.pcr_pid.is_some() {
				self.pending_break = true;
			}
			self.pcr_pid = pcr_pid;
		}
		self.access_pid = access_pid;
		self.mapped = mapped;
		self.single = single;
		// The group in progress now carries what no random access point covers, and it stays cached.
		if !single && self.started() {
			self.all_random_access = false;
		}
		self.record()
	}
}

/// Whether a packet's adaptation field sets `random_access_indicator`.
fn random_access_indicator(pkt: &[u8; TsPacket::SIZE]) -> bool {
	pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x40 != 0
}

/// The PCR time of the byte at `at`, from the PCRs of its timebase.
///
/// Interpolated between the two PCRs either side of it; extrapolated at the rate of the nearest
/// interval when it lies before the timebase's first PCR or after its last. With a single PCR
/// there is no rate, and the byte takes that PCR's time.
fn locate(at: u64, samples: &VecDeque<Sample>) -> u64 {
	let (first, last) = (samples[0], samples[samples.len() - 1]);
	let (a, b) = match samples.iter().zip(samples.iter().skip(1)).find(|(_, b)| at < b.at) {
		Some((a, b)) => (*a, *b),
		None if samples.len() >= 2 => (samples[samples.len() - 2], last),
		None => return last.ticks,
	};
	let span = u128::from(b.at - a.at);
	let ticks = u128::from(b.ticks - a.ticks);
	if at >= a.at {
		a.ticks + (u128::from(at - a.at) * ticks / span) as u64
	} else {
		debug_assert_eq!(a.at, first.at);
		a.ticks
			.saturating_sub((u128::from(a.at - at) * ticks).div_ceil(span) as u64)
	}
}
