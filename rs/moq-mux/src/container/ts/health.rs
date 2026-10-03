//! The ETSI TR 101 290 checks an [`Import`](super::Import) grades its input against.
//!
//! Counters only, at the fixed limits of TR 101 290 V1.4.1: nothing here decides what is
//! published, and nothing is configured. The importer feeds every packet through
//! [`Health::packet`], and the media PIDs and the clock route on its continuity verdict, so
//! the counter and the PES reassembly cannot disagree about which packets broke a chain.
//!
//! Intervals run on the program clock the importer already keeps
//! ([`Liveness`](super::import::Liveness)): the PCR summed step by step, which the 33-bit wrap,
//! a signalled reset or one corrupt value cannot jump. The clock starts at the first PCR after
//! the PMT, and a clock that stops freezes every interval with it. One that jumps forward
//! without a flag, by less than the 1 s bound, stretches every interval by the jump: the PCR
//! check counts the jump, and a table or PTS the stretch makes late counts too.

use std::collections::{BTreeMap, HashMap};

use mpeg2ts::ts::{Pid, TsPacket};

use super::Stats;
use super::import::{Payload, discontinuity_indicator, payload, pcr};
use super::mux_rate::PCR_WRAP;
use super::psi::PMT_TABLE_ID;

const PCR_HZ: u64 = 27_000_000;
/// `PAT_error_2` and `PMT_error_2`: each table recurs at least every 0.5 s.
const TABLE_LIMIT: u64 = PCR_HZ / 2;
/// `PCR_repetition_error` and `PCR_discontinuity_indicator_error`: 100 ms. The 40 ms limit
/// left TS 101 154 in 2005.
const PCR_LIMIT: u64 = PCR_HZ / 10;
/// `PTS_error`: 700 ms.
const PTS_LIMIT: u64 = PCR_HZ * 7 / 10;
/// ISO/IEC 13818-1 G.1: sync is lost after two consecutive corrupt sync bytes, and acquired
/// after five consecutive correct ones.
const SYNC_LOST: u8 = 2;
const SYNC_ACQUIRED: u8 = 5;
const NULL_PID: u16 = 0x1fff;
const SYNC_BYTE: u8 = 0x47;

/// What one importer counted, kept per PID so importers reading the same multiplex can be
/// merged without counting a packet twice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Errors {
	ts_sync_loss: u64,
	sync_byte_error: u64,
	pat_error: u64,
	pids: BTreeMap<u16, PidErrors>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PidErrors {
	continuity_count_error: u64,
	transport_error: u64,
	pmt_error: u64,
	pcr_repetition_error: u64,
	pcr_discontinuity_indicator_error: u64,
	pts_error: u64,
}

impl Errors {
	/// Fold in another importer's count of the same input. Every importer of a multiplex sees
	/// every packet, so each check takes the larger count rather than the sum.
	pub(super) fn max(&mut self, other: &Self) {
		self.ts_sync_loss = self.ts_sync_loss.max(other.ts_sync_loss);
		self.sync_byte_error = self.sync_byte_error.max(other.sync_byte_error);
		self.pat_error = self.pat_error.max(other.pat_error);
		for (pid, theirs) in &other.pids {
			let ours = self.pids.entry(*pid).or_default();
			ours.continuity_count_error = ours.continuity_count_error.max(theirs.continuity_count_error);
			ours.transport_error = ours.transport_error.max(theirs.transport_error);
			ours.pmt_error = ours.pmt_error.max(theirs.pmt_error);
			ours.pcr_repetition_error = ours.pcr_repetition_error.max(theirs.pcr_repetition_error);
			ours.pcr_discontinuity_indicator_error = ours
				.pcr_discontinuity_indicator_error
				.max(theirs.pcr_discontinuity_indicator_error);
			ours.pts_error = ours.pts_error.max(theirs.pts_error);
		}
	}

	/// Write the stream-wide totals into `stats`, and each elementary stream's own counts into
	/// its row.
	pub(super) fn report(&self, stats: &mut Stats) {
		stats.ts_sync_loss = self.ts_sync_loss;
		stats.sync_byte_error = self.sync_byte_error;
		stats.pat_error = self.pat_error;
		let sum = |field: fn(&PidErrors) -> u64| self.pids.values().map(field).sum();
		stats.continuity_count_error = sum(|pid| pid.continuity_count_error);
		stats.transport_error = sum(|pid| pid.transport_error);
		stats.pmt_error = sum(|pid| pid.pmt_error);
		stats.pcr_repetition_error = sum(|pid| pid.pcr_repetition_error);
		stats.pcr_discontinuity_indicator_error = sum(|pid| pid.pcr_discontinuity_indicator_error);
		stats.pts_error = sum(|pid| pid.pts_error);
		for (pid, row) in &mut stats.streams {
			let errors = self.pids.get(pid).copied().unwrap_or_default();
			row.continuity_count_error = errors.continuity_count_error;
			row.transport_error = errors.transport_error;
			row.pts_error = errors.pts_error;
		}
	}

	fn pid(&mut self, pid: u16) -> &mut PidErrors {
		self.pids.entry(pid).or_default()
	}
}

/// The TR 101 290 checks on one importer's input. See the [module](self).
#[derive(Default)]
pub(super) struct Health {
	errors: Errors,
	sync: Sync,
	/// Per PID, every PID but the null PID.
	continuity: HashMap<u16, Continuity>,
	pcr_pid: Option<u16>,
	/// The last PCR value on the PCR PID, forgotten across a signalled discontinuity.
	pcr: Option<u64>,
	pat: Timer,
	/// The PMT PIDs the PAT names for the imported program.
	pmt: HashMap<u16, Timer>,
	/// The elementary streams with a PTS cadence, from their first PTS.
	pts: HashMap<u16, Timer>,
}

impl Health {
	pub(super) fn errors(&self) -> &Errors {
		&self.errors
	}

	/// The framer found a packet at `at` in `buf`. Checks every sync byte position up to it.
	pub(super) fn routed(&mut self, buf: &[u8], at: usize) {
		self.sync.routed(buf, at, &mut self.errors);
	}

	/// The importer is about to drain `buf` up to `off`: check the sync byte positions in the
	/// rest of it, which no packet will be routed from, and carry the grid over.
	pub(super) fn drained(&mut self, buf: &[u8], off: usize) {
		self.sync.drained(buf, off, &mut self.errors);
	}

	/// Grade one packet, and say what it means for the bytes already accumulated for its PID.
	///
	/// A packet with `transport_error_indicator` set counts `Transport_error` and nothing else:
	/// TR 101 290 derives no further error from it, so it feeds no continuity, table or PCR
	/// check. A duplicate feeds none of them either, after its own continuity check.
	pub(super) fn packet(&mut self, pkt: &[u8; TsPacket::SIZE], now: Option<u64>) -> Continuation {
		let pid = (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]);
		if pkt[1] & 0x80 != 0 {
			self.errors.pid(pid).transport_error += 1;
		}
		// The null PID's counter is undefined (ISO/IEC 13818-1 2.4.3.3).
		if pid == NULL_PID {
			return Continuation::Contiguous;
		}

		let continuity = self.continuity.entry(pid).or_default();
		let continuation = continuity.observe(pkt);
		match continuation {
			Continuation::Corrupt => return continuation,
			// ISO/IEC 13818-1 2.4.3.3 permits one duplicate; a third copy is an error.
			Continuation::Duplicate => {
				if continuity.repeats > 1 {
					self.errors.pid(pid).continuity_count_error += 1;
				}
				return continuation;
			}
			Continuation::Broken if !discontinuity_indicator(pkt) => {
				self.errors.pid(pid).continuity_count_error += 1;
			}
			Continuation::Broken | Continuation::Contiguous => {}
		}

		let scrambled = pkt[3] & 0xc0 != 0;
		if pid == Pid::PAT {
			if scrambled {
				self.errors.pat_error += 1;
			} else {
				match section_start(pkt) {
					Some(0x00) => self.pat.reset(now),
					Some(_) => self.errors.pat_error += 1,
					None => {}
				}
			}
		} else if let Some(timer) = self.pmt.get_mut(&pid) {
			if scrambled {
				self.errors.pid(pid).pmt_error += 1;
			} else if section_start(pkt) == Some(PMT_TABLE_ID) {
				timer.reset(now);
			}
		}
		if self.pcr_pid == Some(pid) {
			self.pcr(pid, pkt);
		}
		continuation
	}

	/// `PCR_repetition_error` and `PCR_discontinuity_indicator_error`, graded on consecutive
	/// PCR values rather than on arrival, so they speak for the encoder's insertion and not for
	/// the network in front of the importer.
	///
	/// On values alone a run of missing PCRs and an unsignalled jump are the same difference,
	/// so one forward difference over 100 ms counts under both. A backward one counts only as
	/// a discontinuity. The interval across a packet that sets `discontinuity_indicator` is
	/// dropped from both (ISO/IEC 13818-1 2.4.3.4), and the wrap is modular.
	fn pcr(&mut self, pid: u16, pkt: &[u8; TsPacket::SIZE]) {
		let value = pcr(pkt);
		if discontinuity_indicator(pkt) {
			self.pcr = value;
			return;
		}
		let Some(value) = value else {
			return;
		};
		let Some(last) = self.pcr.replace(value) else {
			return;
		};
		let step = (value + PCR_WRAP - last) % PCR_WRAP;
		let forward = step <= PCR_WRAP / 2;
		let errors = self.errors.pid(pid);
		if forward && step > PCR_LIMIT {
			errors.pcr_repetition_error += 1;
		}
		if !forward || step > PCR_LIMIT {
			errors.pcr_discontinuity_indicator_error += 1;
		}
	}

	/// The program clock advanced to `now`: count every table and PTS overdue at it.
	pub(super) fn tick(&mut self, now: u64) {
		self.errors.pat_error += self.pat.expired(now, TABLE_LIMIT);
		for (pid, timer) in &mut self.pmt {
			let late = timer.expired(now, TABLE_LIMIT);
			if late > 0 {
				self.errors.pids.entry(*pid).or_default().pmt_error += late;
			}
		}
		for (pid, timer) in &mut self.pts {
			let late = timer.expired(now, PTS_LIMIT);
			if late > 0 {
				self.errors.pids.entry(*pid).or_default().pts_error += late;
			}
		}
	}

	/// The PMT PIDs the latest PAT names for the imported program. A PID it no longer names is
	/// no longer graded; a new one is overdue 0.5 s from now.
	pub(super) fn pmt_pids(&mut self, pids: &[u16], now: Option<u64>) {
		self.pmt.retain(|pid, _| pids.contains(pid));
		for &pid in pids {
			self.pmt.entry(pid).or_insert(Timer { last: now });
		}
	}

	/// The PMT named the PID that carries the program clock.
	pub(super) fn pcr_pid(&mut self, pid: Option<u16>) {
		if self.pcr_pid != pid {
			self.pcr_pid = pid;
			self.pcr = None;
		}
	}

	/// An elementary stream with a PTS cadence started a PES that carries one.
	pub(super) fn pts(&mut self, pid: u16, now: Option<u64>) {
		self.pts.entry(pid).or_default().reset(now);
	}

	/// `pid` changed route: forget the chain behind it, so the new route's first packet is not
	/// judged against bytes it never accumulated, and stop grading its PTS until it has one.
	pub(super) fn restart(&mut self, pid: u16) {
		self.continuity.remove(&pid);
		self.pts.remove(&pid);
	}
}

/// The table_id of the first section a PSI packet starts, if it starts one.
fn section_start(pkt: &[u8; TsPacket::SIZE]) -> Option<u8> {
	if pkt[1] & 0x40 == 0 {
		return None;
	}
	let Payload::Bytes(payload) = payload(pkt) else {
		return None;
	};
	let pointer = usize::from(*payload.first()?);
	payload.get(1 + pointer).copied().filter(|&table_id| table_id != 0xff)
}

/// How long since a table or a PTS last occurred, on the program clock.
#[derive(Default)]
struct Timer {
	/// `None` until the clock starts.
	last: Option<u64>,
}

impl Timer {
	fn reset(&mut self, now: Option<u64>) {
		if now.is_some() {
			self.last = now;
		}
	}

	/// How many times `limit` ran out since the last occurrence, restarting the wait at each,
	/// so an outage counts once per limit it lasts rather than once in all.
	fn expired(&mut self, now: u64, limit: u64) -> u64 {
		let last = self.last.get_or_insert(now);
		let late = now.saturating_sub(*last);
		if late <= limit {
			return 0;
		}
		let count = (late - 1) / limit;
		*last += count * limit;
		count
	}
}

/// `TS_sync_loss` and `Sync_byte_error`, on a grid of its own.
///
/// The importer's framer re-scans at the first byte that is not a sync byte, which is right
/// for routing and wrong for grading: ISO/IEC 13818-1 G.1 keeps striding on the old grid, and
/// only declares sync lost after two corrupt sync bytes in a row. So this follows the grid
/// the framer last locked, checks every position on it whether or not a packet was routed
/// from there, and adopts the framer's next lock once it has lost its own.
#[derive(Default)]
struct Sync {
	/// The next sync byte position on the grid, relative to the importer's buffer. `None`
	/// until the framer locks, and again after sync is lost.
	next: Option<usize>,
	acquired: bool,
	/// Consecutive correct sync bytes while acquiring, or corrupt ones while acquired.
	run: u8,
}

impl Sync {
	fn routed(&mut self, buf: &[u8], at: usize, errors: &mut Errors) {
		self.walk(buf, at, errors);
		match self.next {
			// A packet on the grid.
			Some(next) if next == at => {
				self.byte(true, errors);
				self.next = Some(at + TsPacket::SIZE);
			}
			// The framer locked another phase. The grid runs on into this packet's payload,
			// where it is checked when the bytes are walked.
			Some(_) => {}
			None => {
				self.next = Some(at + TsPacket::SIZE);
				self.byte(true, errors);
			}
		}
	}

	fn drained(&mut self, buf: &[u8], off: usize, errors: &mut Errors) {
		self.walk(buf, buf.len(), errors);
		// Every position before the end of `buf` has been checked, so the next is at or past
		// `off`.
		self.next = self.next.map(|next| next - off);
	}

	/// Check every grid position before `end`.
	fn walk(&mut self, buf: &[u8], end: usize, errors: &mut Errors) {
		while let Some(next) = self.next
			&& next < end
		{
			self.next = Some(next + TsPacket::SIZE);
			self.byte(buf[next] == SYNC_BYTE, errors);
		}
	}

	fn byte(&mut self, good: bool, errors: &mut Errors) {
		if self.acquired {
			if good {
				self.run = 0;
				return;
			}
			errors.sync_byte_error += 1;
			self.run += 1;
			if self.run >= SYNC_LOST {
				errors.ts_sync_loss += 1;
				self.acquired = false;
				self.run = 0;
				self.next = None;
			}
		} else if good {
			self.run += 1;
			if self.run >= SYNC_ACQUIRED {
				self.acquired = true;
				self.run = 0;
			}
		} else {
			self.run = 0;
			self.next = None;
		}
	}
}

/// Whether one PID's TS packets are still an unbroken chain, for whoever is accumulating
/// bytes out of them.
///
/// Both reassemblers on this PID need the same answer: a section and a PES are equally
/// meaningless when spliced onto bytes that didn't follow them. Keeping one implementation
/// keeps a subtle rule (which packets advance the counter, which retransmissions to ignore)
/// from drifting into two, and `Continuity_count_error` reads the same verdict.
#[derive(Default)]
pub(super) struct Continuity {
	/// Last continuity_counter seen on a packet with payload, to spot gaps.
	last_cc: Option<u8>,
	/// Last payload packet, to identify ISO 13818-1 retransmissions.
	last_pkt: Option<[u8; 188]>,
	/// Copies of `last_pkt` since it arrived.
	repeats: u8,
}

/// What one packet says about the bytes already accumulated for its PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Continuation {
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
	pub(super) fn observe(&mut self, pkt: &[u8; 188]) -> Continuation {
		// transport_error_indicator: the demodulator flagged this packet as corrupt, so its
		// payload can't be trusted (and only a PAT or PMT has its CRC-32 checked). Forgetting the counter
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
		let discontinuity = discontinuity_indicator(pkt);

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
			self.repeats = self.repeats.saturating_add(1);
			return Continuation::Duplicate;
		}
		self.last_pkt = Some(*pkt);
		self.repeats = 0;

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

impl Stats {
	/// The stream-wide TR 101 290 counters in the order of the checks, less `crc_error`, which
	/// predates them and is reported on its own.
	pub(super) fn health(&self) -> [u64; 9] {
		[
			self.ts_sync_loss,
			self.sync_byte_error,
			self.pat_error,
			self.continuity_count_error,
			self.pmt_error,
			self.transport_error,
			self.pcr_repetition_error,
			self.pcr_discontinuity_indicator_error,
			self.pts_error,
		]
	}
}

#[cfg(test)]
mod test {
	use std::collections::HashMap;

	use super::super::{Import, Programs, Stats, psi};
	use super::PCR_WRAP;

	const PMT_PID: u16 = 0x0100;
	/// MPEG-2 video, read for its clock only. It carries the PCR, on adaptation-only packets
	/// that repeat its counter.
	const VIDEO: u16 = 0x0101;
	/// A second MPEG-2 video PID, so a fault on one can be shown to stay there.
	const PEER: u16 = 0x0102;
	/// PCR ticks per millisecond.
	const MS: u64 = 27_000;

	/// One part of the feed, for a test to leave out.
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	enum Part {
		Pat,
		Pmt,
		Pcr,
		Pes(u16),
	}

	/// A single-program feed built packet by packet, so a test can leave a part out without
	/// breaking a continuity counter, or break one on purpose.
	#[derive(Default)]
	struct Feed {
		cc: HashMap<u16, u8>,
		out: Vec<u8>,
	}

	impl Feed {
		fn header(&mut self, pid: u16, pusi: bool, adaptation: bool, payload: bool) -> Vec<u8> {
			let counter = self.cc.entry(pid).or_default();
			// Only a payload packet advances the counter; one without repeats the last.
			let cc = if payload {
				let cc = *counter;
				*counter = (cc + 1) & 0x0f;
				cc
			} else {
				counter.wrapping_sub(1) & 0x0f
			};
			let afc = (u8::from(adaptation) << 1) | u8::from(payload);
			vec![
				0x47,
				(u8::from(pusi) << 6) | (pid >> 8) as u8,
				pid as u8,
				(afc << 4) | cc,
			]
		}

		fn push(&mut self, mut pkt: Vec<u8>) {
			pkt.resize(188, 0xff);
			self.out.extend(pkt);
		}

		fn psi(&mut self, pid: u16, section: &[u8]) {
			let mut pkt = self.header(pid, true, false, true);
			pkt.push(0);
			pkt.extend_from_slice(section);
			self.push(pkt);
		}

		fn pat(&mut self) {
			let entry = [0x00, 0x01, 0xe0 | (PMT_PID >> 8) as u8, PMT_PID as u8];
			self.psi(0, &psi::section(0x00, 1, 0, 0, 0, &entry));
		}

		fn pmt(&mut self) {
			let mut body = vec![0xe0 | (VIDEO >> 8) as u8, VIDEO as u8, 0xf0, 0x00];
			for pid in [VIDEO, PEER] {
				body.extend_from_slice(&[0x02, 0xe0 | (pid >> 8) as u8, pid as u8, 0xf0, 0x00]);
			}
			self.psi(PMT_PID, &psi::section(0x02, 1, 0, 0, 0, &body));
		}

		/// An adaptation-only clock packet; `flag` sets `discontinuity_indicator`.
		fn pcr(&mut self, ticks: u64, flag: bool) {
			let (base, ext) = (ticks / 300, ticks % 300);
			let mut pkt = self.header(VIDEO, false, true, false);
			pkt.extend_from_slice(&[
				183,
				0x10 | if flag { 0x80 } else { 0 },
				(base >> 25) as u8,
				(base >> 17) as u8,
				(base >> 9) as u8,
				(base >> 1) as u8,
				((base as u8 & 1) << 7) | 0x7e | (ext >> 8) as u8,
				ext as u8,
			]);
			self.push(pkt);
		}

		/// A PES start with `pts` (90 kHz); `flag` gives it an adaptation field that sets
		/// `discontinuity_indicator`.
		fn pes(&mut self, pid: u16, pts: u64, flag: bool) {
			let pts = pts & ((1 << 33) - 1);
			let mut pkt = self.header(pid, true, flag, true);
			if flag {
				pkt.extend_from_slice(&[1, 0x80]);
			}
			pkt.extend_from_slice(&[
				0x00,
				0x00,
				0x01,
				0xe0,
				0x00,
				0x00,
				0x80,
				0x80,
				0x05,
				0x21 | ((pts >> 29) as u8 & 0x0e),
				(pts >> 22) as u8,
				((pts >> 14) as u8 & 0xfe) | 1,
				(pts >> 7) as u8,
				((pts << 1) as u8 & 0xfe) | 1,
			]);
			self.push(pkt);
		}

		fn null(&mut self) {
			self.push(vec![0x47, 0x1f, 0xff, 0x10]);
		}

		/// The feed from `from` to `to` ms on a clock reading `clock(t)` PCR ticks at `t` ms: the
		/// PAT and PMT every 100 ms, a PCR every 10 ms, a PES on each video PID every 40 ms
		/// stamped on the same clock, and a null packet closing each 10 ms. `keep` drops parts.
		fn run(&mut self, from: u64, to: u64, clock: impl Fn(u64) -> u64, keep: impl Fn(u64, Part) -> bool) {
			for t in (from..to).step_by(10) {
				if t % 100 == 0 {
					if keep(t, Part::Pat) {
						self.pat();
					}
					if keep(t, Part::Pmt) {
						self.pmt();
					}
				}
				if keep(t, Part::Pcr) {
					self.pcr(clock(t) % PCR_WRAP, false);
				}
				if t % 40 == 0 {
					for pid in [VIDEO, PEER] {
						if keep(t, Part::Pes(pid)) {
							self.pes(pid, clock(t) / 300, false);
						}
					}
				}
				self.null();
			}
		}

		fn clean(&mut self, from: u64, to: u64) {
			self.run(from, to, |t| t * MS, |_, _| true);
		}

		/// Where each null packet starts.
		fn nulls(&self) -> Vec<usize> {
			(0..self.out.len())
				.step_by(188)
				.filter(|&at| self.out[at + 1] & 0x1f == 0x1f && self.out[at + 2] == 0xff)
				.collect()
		}

		fn last_packet(&mut self) -> &mut [u8] {
			let len = self.out.len();
			&mut self.out[len - 188..]
		}

		/// Import the feed in `chunk`-byte pieces.
		fn import(&self, chunk: usize) -> Stats {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
			let mut import = Import::new(broadcast, catalog.reserve());
			for piece in self.out.chunks(chunk) {
				import.decode(piece).unwrap();
			}
			import.stats()
		}

		/// The counts, the same whether the feed arrives in whole packets or in pieces that
		/// split them.
		fn stats(&self) -> Stats {
			let stats = self.import(188 * 7);
			assert_eq!(stats, self.import(100), "the counts depend on how the input was split");
			stats
		}
	}

	fn all(_: u64, _: Part) -> bool {
		true
	}

	/// `part` is missing for `len` ms after 1 s.
	fn gap(part: Part, len: u64) -> impl Fn(u64, Part) -> bool {
		move |t, p| p != part || !(t > 1_000 && t < 1_000 + len)
	}

	#[test]
	fn a_clean_feed_counts_nothing() {
		let mut feed = Feed::default();
		feed.clean(0, 3_000);
		let stats = feed.stats();
		assert_eq!(stats.health(), [0; 9], "{stats:?}");
		assert_eq!(stats.crc_error, 0);
		for pid in [VIDEO, PEER] {
			assert_eq!(stats.streams[&pid].units, 75, "every PES on {pid:#x} was read");
		}
	}

	/// The PCR is graded on its values: a gap over 100 ms counts under both checks, a shorter
	/// one under neither, the wrap is no gap, and a step back is a discontinuity only.
	#[test]
	fn pcr_gaps_are_graded_on_their_values() {
		let stats = |clock: &dyn Fn(u64) -> u64, keep: &dyn Fn(u64, Part) -> bool| {
			let mut feed = Feed::default();
			feed.run(0, 2_000, clock, keep);
			feed.stats()
		};
		let pcr = |clock: &dyn Fn(u64) -> u64, keep: &dyn Fn(u64, Part) -> bool| {
			let stats = stats(clock, keep);
			let health = stats.health();
			assert_eq!(
				(&health[..6], health[8]),
				(&[0; 6][..], 0),
				"only the PCR checks move: {stats:?}"
			);
			(stats.pcr_repetition_error, stats.pcr_discontinuity_indicator_error)
		};
		let steady = |t| t * MS;
		assert_eq!(pcr(&steady, &gap(Part::Pcr, 150)), (1, 1), "a 150 ms gap");
		assert_eq!(pcr(&steady, &gap(Part::Pcr, 80)), (0, 0), "an 80 ms gap");
		assert_eq!(pcr(&steady, &gap(Part::Pcr, 100)), (0, 0), "a gap of exactly 100 ms");
		assert_eq!(pcr(&|t| PCR_WRAP - 1_000 * MS + t * MS, &all), (0, 0), "the wrap");
		assert_eq!(
			pcr(&|t| if t < 1_000 { t * MS } else { (t - 500) * MS }, &all),
			(0, 1),
			"an unsignalled step back"
		);

		// An unsignalled 500 ms jump also stretches the program clock by 510 ms, which makes
		// the PAT and the PMT each just late. The 40 ms PES cadence absorbs it.
		let jump = stats(&|t| if t < 1_000 { t * MS } else { (t + 500) * MS }, &all);
		assert_eq!(
			(jump.pcr_repetition_error, jump.pcr_discontinuity_indicator_error),
			(1, 1),
			"an unsignalled 500 ms jump"
		);
		assert_eq!((jump.pat_error, jump.pmt_error), (1, 1), "{jump:?}");
		assert_eq!(jump.health().iter().sum::<u64>(), 4, "{jump:?}");
	}

	/// The same 500 ms jump, signalled on the PCR PID, is a new time base and grades nothing.
	#[test]
	fn a_signalled_jump_is_not_graded() {
		let mut feed = Feed::default();
		feed.clean(0, 1_000);
		feed.pcr(1_500 * MS, true);
		feed.run(1_000, 2_000, |t| (t + 500) * MS, all);
		let stats = feed.stats();
		assert_eq!(stats.health(), [0; 9], "{stats:?}");
	}

	/// A missing PAT counts once for each half second of program clock it is missing, and
	/// a PAT exactly 0.5 s late is on time.
	#[test]
	fn a_late_pat_counts_once_per_half_second() {
		for (len, count) in [(400, 0), (500, 0), (800, 1), (1_200, 2)] {
			let mut feed = Feed::default();
			feed.run(0, 3_000, |t| t * MS, gap(Part::Pat, len));
			let stats = feed.stats();
			assert_eq!(stats.pat_error, count, "PAT missing for {len} ms");
			assert_eq!(stats.pmt_error, 0, "PAT missing for {len} ms");
		}
	}

	/// PID 0 carries only PATs, in the clear.
	#[test]
	fn pid_zero_carries_only_clear_pats() {
		let mut feed = Feed::default();
		feed.clean(0, 500);
		feed.psi(0, &psi::section(0x02, 1, 0, 0, 0, &[0xe1, 0x01, 0xf0, 0x00]));
		feed.pat();
		feed.last_packet()[3] |= 0x80;
		feed.clean(500, 1_000);
		let stats = feed.stats();
		assert_eq!(stats.pat_error, 2, "{stats:?}");
	}

	#[test]
	fn a_late_pmt_counts_on_its_own_check() {
		let mut feed = Feed::default();
		feed.run(0, 3_000, |t| t * MS, gap(Part::Pmt, 800));
		let stats = feed.stats();
		assert_eq!((stats.pmt_error, stats.pat_error), (1, 0), "{stats:?}");
	}

	/// A 1 s gap in one stream's PTS counts once, on that stream.
	#[test]
	fn a_pts_gap_counts_on_its_own_pid() {
		let mut feed = Feed::default();
		feed.run(0, 3_000, |t| t * MS, gap(Part::Pes(PEER), 1_000));
		let stats = feed.stats();
		assert_eq!(stats.pts_error, 1, "{stats:?}");
		assert_eq!(stats.streams[&PEER].pts_error, 1);
		assert_eq!(stats.streams[&VIDEO].pts_error, 0);
	}

	/// The continuity counts after a feed with `fault` applied at 500 ms on [`PEER`]: stream
	/// wide, on `PEER`'s row, and the transport errors.
	fn continuity(fault: impl Fn(&mut Feed)) -> (u64, u64, u64) {
		let mut feed = Feed::default();
		feed.clean(0, 500);
		fault(&mut feed);
		feed.clean(500, 1_000);
		let stats = feed.stats();
		(
			stats.continuity_count_error,
			stats.streams[&PEER].continuity_count_error,
			stats.transport_error,
		)
	}

	#[test]
	fn continuity_errors_follow_iso_13818_1() {
		assert_eq!(continuity(|_| {}), (0, 0, 0), "the control");
		let lost = |feed: &mut Feed| {
			feed.pes(PEER, 500 * 90, false);
			feed.out.truncate(feed.out.len() - 188);
		};
		assert_eq!(continuity(lost), (1, 1, 0), "a lost packet");
		let copies = |n: usize| {
			move |feed: &mut Feed| {
				feed.pes(PEER, 500 * 90, false);
				let packet = feed.last_packet().to_vec();
				for _ in 0..n {
					feed.out.extend_from_slice(&packet);
				}
			}
		};
		assert_eq!(
			continuity(copies(1)),
			(0, 0, 0),
			"the one duplicate ISO 13818-1 permits"
		);
		assert_eq!(continuity(copies(2)), (1, 1, 0), "a third copy");
		let jump = |flag: bool| {
			move |feed: &mut Feed| {
				feed.cc.insert(PEER, 9);
				feed.pes(PEER, 500 * 90, flag);
			}
		};
		assert_eq!(continuity(jump(false)), (1, 1, 0), "a counter jump");
		assert_eq!(
			continuity(jump(true)),
			(0, 0, 0),
			"a jump declared by discontinuity_indicator"
		);
	}

	/// A packet flagged corrupt counts `Transport_error` and nothing else, even when its
	/// counter jumps too.
	#[test]
	fn a_corrupt_packet_counts_only_a_transport_error() {
		let tei = |feed: &mut Feed| {
			feed.cc.insert(PEER, 9);
			feed.pes(PEER, 500 * 90, false);
			feed.last_packet()[1] |= 0x80;
		};
		assert_eq!(continuity(tei), (0, 0, 1));

		let mut feed = Feed::default();
		feed.clean(0, 500);
		tei(&mut feed);
		feed.clean(500, 1_000);
		let stats = feed.stats();
		assert_eq!(stats.streams[&PEER].transport_error, 1);
		assert_eq!(stats.health().iter().sum::<u64>(), 1, "{stats:?}");

		// Nor does a corrupt clock packet 300 ms out, or a corrupt packet on PID 0 that starts
		// some other table.
		let mut feed = Feed::default();
		feed.clean(0, 500);
		feed.pcr(800 * MS, false);
		feed.last_packet()[1] |= 0x80;
		feed.psi(0, &psi::section(0x02, 1, 0, 0, 0, &[0xe1, 0x01, 0xf0, 0x00]));
		feed.last_packet()[1] |= 0x80;
		feed.clean(500, 1_000);
		let stats = feed.stats();
		assert_eq!(stats.transport_error, 2, "{stats:?}");
		assert_eq!(stats.health().iter().sum::<u64>(), 2, "{stats:?}");
	}

	/// One corrupt sync byte is an error without losing sync; two in a row lose it, and five
	/// good ones acquire it again, so the next corrupt byte counts once more.
	#[test]
	fn sync_is_lost_after_two_corrupt_sync_bytes() {
		let mut feed = Feed::default();
		feed.clean(0, 2_000);
		let nulls = feed.nulls();
		// A null packet: no counter to break.
		feed.out[nulls[50]] = 0x00;
		let stats = feed.stats();
		assert_eq!(
			(stats.sync_byte_error, stats.ts_sync_loss),
			(1, 0),
			"one corrupt sync byte"
		);
		assert_eq!(stats.health().iter().sum::<u64>(), 1, "{stats:?}");

		// At 1.01 s the null is followed by an adaptation-only PCR packet, whose loss breaks
		// no counter either.
		feed.out[nulls[101]] = 0x00;
		feed.out[nulls[101] + 188] = 0x00;
		feed.out[nulls[150]] = 0x00;
		let stats = feed.stats();
		assert_eq!((stats.sync_byte_error, stats.ts_sync_loss), (4, 1), "{stats:?}");
		assert_eq!(stats.health().iter().sum::<u64>(), 5, "{stats:?}");
	}

	/// Bytes inserted into the stream move the packet grid: the old grid finds no sync byte
	/// twice running, so sync is lost once, and acquired again on the new one.
	#[test]
	fn a_shifted_grid_loses_sync_once() {
		let mut feed = Feed::default();
		feed.clean(0, 1_000);
		feed.out.extend_from_slice(&[0x00; 10]);
		feed.clean(1_000, 2_000);
		let stats = feed.stats();
		assert_eq!((stats.sync_byte_error, stats.ts_sync_loss), (2, 1), "{stats:?}");
		assert_eq!(stats.health().iter().sum::<u64>(), 3, "{stats:?}");
	}

	/// Every program's importer grades every packet of the multiplex, so a fault counts once.
	#[test]
	fn a_multiplex_counts_each_fault_once() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(origin, "event.hang", crate::catalog::Config::default());
		let mut data = super::super::import::test::two_programs();
		let mut corrupt = vec![0x47, 0x9f, 0xff, 0x10];
		corrupt.resize(188, 0xff);
		data.extend_from_slice(&corrupt);
		let mut null = vec![0x47, 0x1f, 0xff, 0x10];
		null.resize(188, 0xff);
		data.extend_from_slice(&null);
		programs.decode(&data).unwrap();
		assert_eq!(programs.stats().transport_error, 1);
	}
}
