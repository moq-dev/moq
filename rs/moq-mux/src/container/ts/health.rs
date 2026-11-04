//! The ETSI TR 101 290 checks an [`Import`](super::Import) grades its input against.
//!
//! Counters only, at the fixed limits of TR 101 290 V1.4.1: nothing here decides what is
//! published, and nothing is configured. The parser grades every packet through
//! [`Multiplex::packet`] once, however many programs read it, and the media PIDs and the clock
//! route on its continuity verdict, so the counter and the PES reassembly cannot disagree about
//! which packets broke a chain. Each program times its own tables, PCR and PTS in [`Health`].
//!
//! Intervals run on each program's health [`Clock`], which grades the encoder's insertion: its
//! PCR summed step by step, which the 33-bit wrap, a signalled reset or one corrupt value
//! cannot jump, carried between PCRs by each packet's place in the multiplex at the rate the
//! PCR measured. A stalled or absent PCR therefore cannot hide missing tables or PTS while the
//! multiplex runs on, however the input is batched. Where no constant rate is measured, the
//! clock falls back to arrival, read once per input chunk; there it grades ingestion. A fully
//! stopped input advances nothing until it resumes, when the PCR's own step counts an encoder
//! outage, and neither EOF nor a timer adds time.

use std::collections::{BTreeMap, HashMap};

use kio::clock::Instant;
use mpeg2ts::ts::{Pid, TsPacket};

use super::import::{Payload, discontinuity_indicator, payload, pcr};
use super::mux_rate::{Meter, PACKET_BITS, PCR_WRAP};
use super::psi::PMT_TABLE_ID;
use super::stats::Snapshot;

const PCR_HZ: u64 = 27_000_000;
/// `PAT_error_2` and `PMT_error_2`: each table recurs at least every 0.5 s.
const TABLE_LIMIT: u64 = PCR_HZ / 2;
/// `PCR_repetition_error` and `PCR_discontinuity_indicator_error`: 100 ms. The 40 ms limit
/// left TS 101 154 in 2005.
const PCR_LIMIT: u64 = PCR_HZ / 10;
/// `PTS_error`: 700 ms.
pub(super) const PTS_LIMIT: u64 = PCR_HZ * 7 / 10;
/// ISO/IEC 13818-1 G.1: sync is lost after two consecutive corrupt sync bytes, and acquired
/// after five consecutive correct ones.
const SYNC_LOST: u8 = 2;
const SYNC_ACQUIRED: u8 = 5;
const NULL_PID: u16 = 0x1fff;
const SYNC_BYTE: u8 = 0x47;

/// What one checker counted, kept per PID so programs reading the same PID can be merged
/// without counting a packet twice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Errors {
	ts_sync_loss: u64,
	sync_byte_error: u64,
	pat_error: u64,
	pids: BTreeMap<u16, PidErrors>,
}

/// The checks a program times on a PID by the way it reads it, which programs reading the PID
/// the same way count alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Checks {
	/// The PTS cadence of an elementary stream it carries, and the PAT's interval on PID 0.
	Stream,
	/// The PCR checks on the PID it takes its clock from.
	Clock,
	/// The PMT interval on the PID it reads its PMT from.
	Table,
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
	/// Fold in another checker's count of the same input. Programs sharing a PAT, PMT or PCR PID
	/// time the same packets, so each check takes the larger count rather than the sum.
	pub(super) fn max(&mut self, other: &Self) {
		self.combine(other, u64::max);
	}

	/// Set each count to `f` of it and `other`'s, check by check and PID by PID.
	pub(super) fn combine(&mut self, other: &Self, f: impl Fn(u64, u64) -> u64) {
		self.ts_sync_loss = f(self.ts_sync_loss, other.ts_sync_loss);
		self.sync_byte_error = f(self.sync_byte_error, other.sync_byte_error);
		self.pat_error = f(self.pat_error, other.pat_error);
		for (pid, theirs) in &other.pids {
			let ours = self.pids.entry(*pid).or_default();
			ours.continuity_count_error = f(ours.continuity_count_error, theirs.continuity_count_error);
			ours.transport_error = f(ours.transport_error, theirs.transport_error);
			ours.pmt_error = f(ours.pmt_error, theirs.pmt_error);
			ours.pcr_repetition_error = f(ours.pcr_repetition_error, theirs.pcr_repetition_error);
			ours.pcr_discontinuity_indicator_error = f(
				ours.pcr_discontinuity_indicator_error,
				theirs.pcr_discontinuity_indicator_error,
			);
			ours.pts_error = f(ours.pts_error, theirs.pts_error);
		}
	}

	/// The `checks` counted on `pid` alone.
	pub(super) fn only(&self, pid: u16, checks: Checks) -> Self {
		let all = self.pids.get(&pid).copied().unwrap_or_default();
		let mut errors = PidErrors::default();
		let mut pat_error = 0;
		match checks {
			Checks::Stream => {
				errors.pts_error = all.pts_error;
				if pid == Pid::PAT {
					pat_error = self.pat_error;
				}
			}
			Checks::Clock => {
				errors.pcr_repetition_error = all.pcr_repetition_error;
				errors.pcr_discontinuity_indicator_error = all.pcr_discontinuity_indicator_error;
			}
			Checks::Table => errors.pmt_error = all.pmt_error,
		}
		Self {
			pat_error,
			pids: BTreeMap::from([(pid, errors)]),
			..Default::default()
		}
	}

	/// Write the stream-wide totals into `stats`, and each elementary stream's own counts into
	/// its row.
	pub(super) fn report(&self, stats: &mut Snapshot) {
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

/// The TR 101 290 checks that read packets alone: sync and per-PID continuity. One per input,
/// however many programs read it.
#[derive(Default)]
pub(super) struct Multiplex {
	errors: Errors,
	sync: Sync,
	/// Per PID, every PID but the null PID.
	continuity: HashMap<u16, Continuity>,
}

impl Multiplex {
	pub(super) fn errors(&self) -> &Errors {
		&self.errors
	}

	/// The framer found a packet at `at` in `buf`. Checks every sync byte position up to it.
	pub(super) fn routed(&mut self, buf: &[u8], at: usize) {
		self.sync.routed(buf, at, &mut self.errors);
	}

	/// The parser is about to drain `buf` up to `off`: check the sync byte positions in the
	/// rest of it, which no packet will be routed from, and carry the grid over.
	pub(super) fn drained(&mut self, buf: &[u8], off: usize) {
		self.sync.drained(buf, off, &mut self.errors);
	}

	/// Grade one packet's continuity, and say what it means for the bytes already accumulated
	/// for its PID.
	///
	/// A packet with `transport_error_indicator` set counts `Transport_error` and nothing else:
	/// TR 101 290 derives no further error from it.
	pub(super) fn packet(&mut self, pkt: &[u8; TsPacket::SIZE]) -> Continuation {
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
			// ISO/IEC 13818-1 2.4.3.3 permits one duplicate; a third copy is an error.
			Continuation::Duplicate if continuity.repeats > 1 => {
				self.errors.pid(pid).continuity_count_error += 1;
			}
			Continuation::Broken if !discontinuity_indicator(pkt) => {
				self.errors.pid(pid).continuity_count_error += 1;
			}
			_ => {}
		}
		continuation
	}

	/// `pid` changed route: forget the chain behind it, so the new route's first packet is not
	/// judged against bytes it never accumulated.
	pub(super) fn restart(&mut self, pid: u16) {
		self.continuity.remove(&pid);
	}
}

/// Where a packet sits in the input, for the health clock.
#[derive(Clone, Copy, Debug)]
pub(super) struct At {
	/// Packets framed through it, every PID and null stuffing included.
	pub count: u64,
	/// When the input chunk that completed it arrived.
	pub arrival: Instant,
}

/// Where a program's health clock next reaches an interval's limit: an input packet count on
/// a measured multiplex, an arrival otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Due {
	pub count: Option<u64>,
	pub arrival: Option<Instant>,
}

impl Due {
	/// Due at once, for a change that may have moved a limit earlier than any recorded.
	pub(super) const NOW: Self = Self {
		count: Some(0),
		arrival: Some(Instant::ZERO),
	};

	/// The earlier of each.
	pub(super) fn min(self, other: Self) -> Self {
		fn min<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
			match (a, b) {
				(Some(a), Some(b)) => Some(a.min(b)),
				(a, b) => a.or(b),
			}
		}
		Self {
			count: min(self.count, other.count),
			arrival: min(self.arrival, other.arrival),
		}
	}
}

/// Table, PCR and PTS intervals on a program's health clock.
#[derive(Clone, Default)]
pub(super) struct Health {
	errors: Errors,
	pcr_pid: Option<u16>,
	/// The last PCR value on the PCR PID, forgotten across a signalled discontinuity.
	pcr: Option<u64>,
	clock: Clock,
	/// Last advancing PCR, or when its PID was declared, on the health clock.
	progress: Option<i64>,
	/// Missing PCR counts once until its value advances or a flagged PCR resets it.
	missing: bool,
	/// The health clock reading at which the next interval can expire.
	due: Option<i64>,
	pat: Timer,
	/// The PMT PIDs the PAT names for the imported program.
	pmt: HashMap<u16, Timer>,
	/// The elementary streams with a PTS cadence, from their first PTS.
	pts: HashMap<u16, Timer>,
}

impl Health {
	/// When the packet just read occurred on the health clock, in 27 MHz ticks since the first
	/// packet read, or `None` before one. Without a measured rate that is its own arrival, which
	/// the clock itself reaches only once its chunk has been read whole.
	pub(super) fn now(&self) -> Option<u64> {
		self.clock.at(self.clock.read?)
	}

	/// Where this clock next reaches a limit, for a caller that advances it without routing
	/// it a packet.
	pub(super) fn due(&self) -> Due {
		self.due.map_or_else(Due::default, |due| self.clock.due(due))
	}

	pub(super) fn errors(&self) -> &Errors {
		&self.errors
	}

	/// The input reached the packet at `at`, whether this program reads it or not. A later
	/// chunk starting means the earlier one was read whole, so it is graded at its arrival.
	pub(super) fn advance(&mut self, at: At) {
		if let Some(read) = self.clock.read
			&& at.arrival > read.arrival
		{
			self.settle(read.arrival);
		}
		self.clock.advance(at);
		self.check();
	}

	/// The input arrived through `arrival` with every packet in it read. Only a multiplex
	/// without a measured rate advances by arrival, and only here, once a chunk has been read
	/// whole: a chunk's own PCR, tables and PTS answer for its delivery delay first.
	pub(super) fn settle(&mut self, arrival: Instant) {
		self.clock.settle(arrival);
		self.check();
	}

	/// Settle at the arrival of the last packet read.
	pub(super) fn flush(&mut self) {
		if let Some(read) = self.clock.read {
			self.settle(read.arrival);
		}
	}

	fn check(&mut self) {
		if let Some(now) = self.clock.now
			&& self.due.is_none_or(|due| ticks(now) >= due)
		{
			self.tick();
		}
	}

	/// Time one packet the [`Multiplex`] graded as `continuation`, read at `at`. A corrupt
	/// packet or a duplicate feeds no table or PCR check.
	pub(super) fn packet(&mut self, pkt: &[u8; TsPacket::SIZE], continuation: Continuation, at: At) {
		self.advance(at);
		if matches!(continuation, Continuation::Corrupt | Continuation::Duplicate) {
			return;
		}
		let pid = (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]);
		let scrambled = pkt[3] & 0xc0 != 0;
		let now = self.now();
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
		if self.pcr_pid == Some(pid) && self.pcr(pid, pkt, at.count) {
			self.tick();
		}
	}

	/// `PCR_repetition_error` and `PCR_discontinuity_indicator_error` on consecutive PCR values,
	/// and the PCR progression the health clock follows. Returns whether the clock moved.
	///
	/// On values alone a run of missing PCRs and an unsignalled jump are the same difference,
	/// so one forward difference over 100 ms counts under both. A backward one counts only as
	/// a discontinuity. The interval across a packet that sets `discontinuity_indicator` is
	/// dropped from both (ISO/IEC 13818-1 2.4.3.4), and the wrap is modular. A missing-PCR
	/// outage [`tick`](Self::tick) already counted is not counted again when the PCR returns.
	fn pcr(&mut self, pid: u16, pkt: &[u8; TsPacket::SIZE], count: u64) -> bool {
		let value = pcr(pkt);
		if discontinuity_indicator(pkt) {
			// A new time base voids the measured rate whether or not this packet carries its
			// first PCR. Progress waits for that PCR, so a flag alone cannot hide its absence.
			self.clock.meter.discontinuity();
			self.pcr = value;
			let Some(value) = value else {
				return false;
			};
			self.clock.meter.pcr(value, count);
			self.restart_progress();
			return true;
		}
		let Some(value) = value else {
			return false;
		};
		let Some(last) = self.pcr.replace(value) else {
			self.clock.meter.pcr(value, count);
			self.restart_progress();
			return true;
		};
		if value == last {
			// A frozen value is no progress, and no sample for the rate the clock runs on.
			return false;
		}
		self.clock.meter.pcr(value, count);
		let errors = self.errors.pid(pid);
		let step = match Delta::between(last, value, PCR_WRAP) {
			Delta::Forward(step) => {
				if step > PCR_LIMIT {
					errors.pcr_repetition_error += u64::from(!self.missing);
					errors.pcr_discontinuity_indicator_error += 1;
				}
				Some(step).filter(|&step| step <= super::mux_rate::MAX_INTERVAL)
			}
			Delta::Back(_) => {
				errors.pcr_discontinuity_indicator_error += 1;
				None
			}
		};
		match step {
			Some(step) => {
				self.clock.pcr(step, self.missing);
				self.progress = self.clock.anchor.map(|(now, _)| ticks(now));
				self.missing = false;
			}
			None => self.restart_progress(),
		}
		true
	}

	/// The PCR restarted: reanchor the clock where it reads, with nothing missing.
	fn restart_progress(&mut self) {
		self.clock.pcr(0, true);
		self.progress = self.now().map(ticks);
		self.missing = false;
	}

	/// Count every table and PTS overdue, and a missing PCR once per outage.
	fn tick(&mut self) {
		let Some(now) = self.clock.now else { return };
		let mut next = None;
		let mut due = |last: i64, limit: u64| {
			let at = last + ticks(limit) + 1;
			next = Some(next.map_or(at, |next: i64| next.min(at)));
		};
		if let Some(pid) = self.pcr_pid {
			let progress = self.progress.get_or_insert(ticks(now));
			if ticks(now) - *progress > ticks(PCR_LIMIT) && !self.missing {
				self.errors.pid(pid).pcr_repetition_error += 1;
				self.missing = true;
			}
			if !self.missing {
				due(*progress, PCR_LIMIT);
			}
		}
		self.errors.pat_error += self.pat.expired(now, TABLE_LIMIT);
		due(self.pat.last.unwrap(), TABLE_LIMIT);
		for (pid, timer) in &mut self.pmt {
			let late = timer.expired(now, TABLE_LIMIT);
			if late > 0 {
				self.errors.pids.entry(*pid).or_default().pmt_error += late;
			}
			due(timer.last.unwrap(), TABLE_LIMIT);
		}
		for (pid, timer) in &mut self.pts {
			let late = timer.expired(now, PTS_LIMIT);
			if late > 0 {
				self.errors.pids.entry(*pid).or_default().pts_error += late;
			}
			due(timer.last.unwrap(), PTS_LIMIT);
		}
		self.due = next;
	}

	/// The PMT PIDs the latest PAT names for the imported program. A PID it no longer names is
	/// no longer graded; a new one is overdue 0.5 s from now.
	pub(super) fn pmt_pids(&mut self, pids: &[u16], now: Option<u64>) {
		self.pmt.retain(|pid, _| pids.contains(pid));
		for &pid in pids {
			self.pmt.entry(pid).or_insert(Timer::at(now));
		}
	}

	/// The PMT named the PID that carries the program clock.
	pub(super) fn pcr_pid(&mut self, pid: Option<u16>) {
		if self.pcr_pid != pid {
			self.pcr_pid = pid;
			self.pcr = None;
			// Another PID's PCR paces the multiplex too, but its samples start afresh.
			self.clock.meter.discontinuity();
			self.progress = self.now().map(ticks);
			self.missing = false;
			self.tick();
		}
	}

	/// An elementary stream with a PTS cadence started a PES that carries one.
	pub(super) fn pts(&mut self, pid: u16, now: Option<u64>) {
		self.pts.entry(pid).or_default().reset(now);
	}

	/// The elementary PIDs the latest PMT lists. A PID it no longer lists has no PTS cadence
	/// left to grade.
	pub(super) fn pmt_streams(&mut self, pids: &[u16]) {
		self.pts.retain(|pid, _| pids.contains(pid));
	}

	/// `pid` changed route: stop grading its PTS until it has one.
	pub(super) fn restart(&mut self, pid: u16) {
		self.pts.remove(&pid);
	}

	/// Carry what this checker has timed on `pid` over to `to`, which reads the same packets and
	/// counts `pid` from here on, where `to` has not timed it yet: the PAT's interval on PID 0,
	/// the PMT's on a PMT PID `to` reads, the PTS cadence when `to` grades one there, and a PCR
	/// outage on a clock both take. So an interval running across the handover counts once,
	/// from where it began, even from before `to`'s clock started. Both clocks are read at the
	/// same point of the input, the later packet either has read, since either program may
	/// have last read an unrelated packet. Nothing carries over before both run.
	pub(super) fn hand_over(&self, pid: u16, to: &mut Self, pts: bool) {
		let (Some(read), Some(theirs)) = (self.clock.read, to.clock.read) else {
			return;
		};
		let at = if read.count >= theirs.count { read } else { theirs };
		let (Some(now), Some(then)) = (self.clock.at(at), to.clock.at(at)) else {
			return;
		};
		let carry = |timer: &Timer, theirs: &mut Timer| {
			if theirs.last.is_none() {
				theirs.last = timer.last.map(|last| last + ticks(then) - ticks(now));
			}
		};
		if pid == Pid::PAT {
			carry(&self.pat, &mut to.pat);
		}
		if let (Some(timer), Some(theirs)) = (self.pmt.get(&pid), to.pmt.get_mut(&pid)) {
			carry(timer, theirs);
		}
		if pts && let Some(timer) = self.pts.get(&pid) {
			carry(timer, to.pts.entry(pid).or_default());
		}
		if self.pcr_pid == Some(pid) && to.pcr_pid == Some(pid) {
			to.progress = self.progress.map(|last| last + ticks(then) - ticks(now));
			to.missing = self.missing;
		}
		to.tick();
	}
}

/// A program's health clock: its PCR progression, carried between PCRs by the multiplex.
///
/// On a multiplex whose rate the program's PCR has measured as constant, each packet past the
/// last PCR adds its time at that rate, which speaks for the encoder's insertion however the
/// input was batched or delayed on its way. Without a measured rate, before the first window
/// completes, on VBR input, or after the PCR breaks, the clock advances by arrival instead,
/// only when a chunk has been read whole: ingestion, upstream delays included. The reading
/// never runs back, and a PCR that catches up adds nothing twice.
#[derive(Clone, Default)]
struct Clock {
	/// The reading, in 27 MHz ticks since the first packet read.
	now: Option<u64>,
	/// The reading at the last PCR, or the first packet or a reset, and where that sat in the
	/// input.
	anchor: Option<(u64, At)>,
	/// The latest packet read.
	read: Option<At>,
	/// The latest arrival read whole.
	settled: Option<Instant>,
	/// The multiplex rate, measured on advancing PCR values.
	meter: Meter,
}

impl Clock {
	/// The anchor extrapolated to the packet at `count`, read whole through `arrival`.
	fn reading(&self, count: u64, arrival: Option<Instant>) -> Option<u64> {
		let (ticks, from) = self.anchor?;
		let elapsed = match self.meter.published() {
			Some(rate) => {
				let packets = count.saturating_sub(from.count);
				(u128::from(packets) * u128::from(PACKET_BITS) * u128::from(PCR_HZ) / u128::from(rate)) as u64
			}
			None => arrival.map_or(0, |arrival| {
				(arrival.duration_since(from.arrival).as_nanos() * u128::from(PCR_HZ) / 1_000_000_000) as u64
			}),
		};
		Some(ticks + elapsed)
	}

	/// When the packet at `at` occurred: the reading there, as if its chunk had been read whole.
	fn at(&self, at: At) -> Option<u64> {
		let reading = self.reading(at.count, Some(at.arrival))?;
		Some(self.now.unwrap_or(0).max(reading))
	}

	fn update(&mut self) {
		if let Some(read) = self.read
			&& let Some(reading) = self.reading(read.count, self.settled)
		{
			self.now = Some(self.now.unwrap_or(0).max(reading));
		}
	}

	fn advance(&mut self, at: At) {
		self.anchor.get_or_insert((0, at));
		self.read = Some(at);
		self.update();
	}

	fn settle(&mut self, arrival: Instant) {
		if self.settled.is_some_and(|settled| arrival <= settled) {
			return;
		}
		self.settled = Some(arrival);
		self.update();
	}

	/// The PCR at the packet just read advanced by `step` ticks, or `reset` the timeline.
	fn pcr(&mut self, step: u64, reset: bool) {
		let Some(at) = self.read else { return };
		let source = self.anchor.map_or(0, |(ticks, _)| ticks) + step;
		let now = self.now.unwrap_or(0).max(source);
		self.now = Some(now);
		// Ordinary jitter must not accumulate into extra PCR time. Only a counted outage or
		// a clock reset restarts the PCR timeline at the reading.
		self.anchor = Some((if reset { now } else { source }, at));
	}

	/// Where the reading reaches `deadline`.
	fn due(&self, deadline: i64) -> Due {
		let Some((ticks, from)) = self.anchor else {
			return Due::default();
		};
		let ahead = u128::try_from(deadline.saturating_sub(self::ticks(ticks))).unwrap_or(0);
		match self.meter.published() {
			Some(rate) => {
				let packets = (ahead * u128::from(rate)).div_ceil(u128::from(PACKET_BITS) * u128::from(PCR_HZ));
				Due {
					count: u64::try_from(packets)
						.ok()
						.and_then(|packets| from.count.checked_add(packets)),
					arrival: None,
				}
			}
			None => {
				let nanos = (ahead * 1_000_000_000).div_ceil(u128::from(PCR_HZ));
				Due {
					count: None,
					arrival: u64::try_from(nanos)
						.ok()
						.and_then(|nanos| from.arrival.checked_add(std::time::Duration::from_nanos(nanos))),
				}
			}
		}
	}
}

/// The step between two readings of a clock that wraps: forward by up to half the range,
/// otherwise back, so a wrap reads as the small forward step it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Delta {
	/// Ahead by this many ticks.
	Forward(u64),
	/// Behind by this many ticks.
	Back(u64),
}

impl Delta {
	/// From `last` to `value`, both below `wrap`.
	pub(super) fn between(last: u64, value: u64, wrap: u64) -> Self {
		let step = (value % wrap + wrap - last % wrap) % wrap;
		if step <= wrap / 2 {
			Self::Forward(step)
		} else {
			Self::Back(wrap - step)
		}
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
#[derive(Clone, Copy, Default)]
struct Timer {
	/// `None` until the clock starts. Signed, since one [handed over](Health::hand_over) from
	/// an earlier clock may predate this one's start.
	last: Option<i64>,
}

impl Timer {
	fn at(now: Option<u64>) -> Self {
		Self { last: now.map(ticks) }
	}

	fn reset(&mut self, now: Option<u64>) {
		if now.is_some() {
			*self = Self::at(now);
		}
	}

	/// How many times `limit` ran out since the last occurrence, restarting the wait at each,
	/// so an outage counts once per limit it lasts rather than once in all.
	fn expired(&mut self, now: u64, limit: u64) -> u64 {
		let last = self.last.get_or_insert(ticks(now));
		let late = u64::try_from(ticks(now) - *last).unwrap_or(0);
		if late <= limit {
			return 0;
		}
		let count = (late - 1) / limit;
		*last += ticks(count * limit);
		count
	}
}

/// A program clock reading as a signed tick count, which 27 MHz keeps in range for millennia.
fn ticks(now: u64) -> i64 {
	i64::try_from(now).unwrap_or(i64::MAX)
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
#[derive(Clone, Default)]
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
	/// The last payload counter, to continue a program map across an import restart.
	pub(super) fn counter(&self) -> Option<u8> {
		self.last_cc
	}

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

impl Snapshot {
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

	use super::super::{Import, Programs, psi, stats::Snapshot};
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
		/// Packets in each 10 ms slot of [`run`](Self::run), padded with stuffing as a
		/// constant-rate multiplexer pads whatever it leaves out. Zero pads nothing.
		slot: usize,
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

		/// A PMT listing `pids` as MPEG-2 video, with the clock on [`VIDEO`].
		fn pmt(&mut self, version: u8, pids: &[u16]) {
			let mut body = vec![0xe0 | (VIDEO >> 8) as u8, VIDEO as u8, 0xf0, 0x00];
			for &pid in pids {
				body.extend_from_slice(&[0x02, 0xe0 | (pid >> 8) as u8, pid as u8, 0xf0, 0x00]);
			}
			self.psi(PMT_PID, &psi::section(0x02, 1, version, 0, 0, &body));
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
		fn run(&mut self, from: u64, to: u64, clock: impl Fn(u64, Part) -> u64, keep: impl Fn(u64, Part) -> bool) {
			for t in (from..to).step_by(10) {
				let start = self.out.len();
				if t % 100 == 0 {
					if keep(t, Part::Pat) {
						self.pat();
					}
					if keep(t, Part::Pmt) {
						self.pmt(0, &[VIDEO, PEER]);
					}
				}
				if keep(t, Part::Pcr) {
					self.pcr(clock(t, Part::Pcr) % PCR_WRAP, false);
				}
				if t % 40 == 0 {
					for pid in [VIDEO, PEER] {
						if keep(t, Part::Pes(pid)) {
							self.pes(pid, clock(t, Part::Pes(pid)) / 300, false);
						}
					}
				}
				self.null();
				while (self.out.len() - start) / 188 < self.slot {
					self.null();
				}
			}
		}

		fn clean(&mut self, from: u64, to: u64) {
			self.run(from, to, |t, _| t * MS, |_, _| true);
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
		fn import(&self, chunk: usize) -> Snapshot {
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
		fn stats(&self) -> Snapshot {
			let stats = self.import(188 * 7);
			assert_eq!(stats, self.import(100), "the counts depend on how the input was split");
			stats
		}
	}

	fn all(_: u64, _: Part) -> bool {
		true
	}

	/// Advance the paused clock by `ms`.
	async fn wait(ms: u64) {
		tokio::time::advance(std::time::Duration::from_millis(ms)).await;
	}

	fn at(count: u64, arrival: kio::clock::Instant) -> super::At {
		super::At { count, arrival }
	}

	/// Without a measured rate the clock falls back to arrival, read once per chunk. Jitter in
	/// when chunks arrive does not accumulate past the PCR.
	#[tokio::test(start_paused = true)]
	async fn arrival_jitter_does_not_accumulate_on_the_health_clock() {
		let mut health = super::Health::default();
		let start = kio::clock::Instant::now();
		health.advance(at(0, start));
		health.pcr_pid(Some(VIDEO));
		let mut feed = Feed::default();
		for i in 0..=30 {
			let ms = i * 50 + if i % 2 == 0 { 0 } else { 30 };
			feed.pcr(i * 50 * MS, false);
			health.packet(
				(&*feed.last_packet()).try_into().unwrap(),
				super::Continuation::Contiguous,
				at(i + 1, start + std::time::Duration::from_millis(ms)),
			);
			health.flush();
		}
		assert_eq!(health.now(), Some(1_500 * MS));
		assert_eq!(health.errors.pids[&VIDEO].pcr_repetition_error, 0);
	}

	/// Before any rate is measured, a PCR that stops or freezes while chunks keep arriving is
	/// counted once on arrival, with the tables and PTS that stop with it.
	#[tokio::test(start_paused = true)]
	async fn a_stalled_pcr_does_not_freeze_timing_checks() {
		for continuing in [false, true] {
			for repeated in [false, true] {
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
				let mut import = Import::new(broadcast, catalog.reserve());
				let mut feed = Feed::default();
				feed.clean(0, 100);
				import.decode(&std::mem::take(&mut feed.out)).unwrap();
				for t in (100..=1_000).step_by(100) {
					wait(100).await;
					if continuing {
						feed.pat();
						feed.pmt(0, &[VIDEO, PEER]);
						feed.pes(VIDEO, t * 90, false);
						feed.pes(PEER, t * 90, false);
					}
					if repeated {
						feed.pcr(90 * MS, false);
					}
					feed.null();
					import.decode(&std::mem::take(&mut feed.out)).unwrap();
				}
				let stats = import.stats();
				assert_eq!(stats.pcr_repetition_error, 1, "{continuing} {repeated}: {stats:?}");
				assert_eq!(stats.pat_error, 2 * u64::from(!continuing), "{stats:?}");
				assert_eq!(stats.pmt_error, 2 * u64::from(!continuing), "{stats:?}");
				assert_eq!(stats.pts_error, 2 * u64::from(!continuing), "{stats:?}");
			}
		}
	}

	/// Once the PCR has measured a constant rate, a PCR that stops or freezes is timed by the
	/// packets the multiplex carries on with, so the counts speak for the encoder's insertion
	/// whether the input is read offline, in any pieces, without any time passing.
	#[tokio::test(start_paused = true)]
	async fn a_measured_multiplex_times_a_stalled_pcr_by_its_packets() {
		let stalled = |t: u64| (3_000..4_000).contains(&t);
		for continuing in [false, true] {
			for frozen in [false, true] {
				let mut feed = Feed {
					slot: 6,
					..Default::default()
				};
				feed.clean(0, 3_000);
				feed.run(
					3_000,
					5_000,
					|t, part| {
						if part == Part::Pcr && stalled(t) {
							2_990 * MS
						} else {
							t * MS
						}
					},
					|t, part| {
						!stalled(t)
							|| match part {
								Part::Pcr => frozen,
								_ => continuing,
							}
					},
				);
				let stats = feed.stats();
				assert_eq!(stats.pcr_repetition_error, 1, "{continuing} {frozen}: {stats:?}");
				let late = 2 * u64::from(!continuing);
				assert_eq!(
					(stats.pat_error, stats.pmt_error, stats.pts_error),
					(late, late, late),
					"{continuing} {frozen}: {stats:?}"
				);
			}
		}
	}

	/// Batched or delayed delivery of a clean feed is no insertion fault: a measured multiplex
	/// ignores arrival, and an unmeasured one lets each chunk's own PCR answer for its delay.
	#[tokio::test(start_paused = true)]
	async fn batched_delivery_of_a_clean_feed_counts_nothing() {
		for measured in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut import = Import::new(broadcast, catalog.reserve());
			let mut feed = Feed {
				slot: 6,
				..Default::default()
			};
			if measured {
				// Long enough to measure the rate, then a packet at a time, with a long wait in
				// front of packets that carry no PCR.
				feed.clean(0, 3_000);
				import.decode(&std::mem::take(&mut feed.out)).unwrap();
				feed.clean(3_000, 5_000);
				for (i, packet) in feed.out.chunks(188).enumerate() {
					if i % 40 == 1 {
						wait(400).await;
					}
					import.decode(packet).unwrap();
				}
			} else {
				// Too short to measure: chunks of 20 packets, each carrying a PCR, 400 ms apart.
				feed.clean(0, 1_500);
				for chunk in feed.out.chunks(188 * 20) {
					wait(400).await;
					import.decode(chunk).unwrap();
				}
			}
			assert_eq!(import.stats().health(), [0; 9], "measured {measured}");
		}
	}

	/// A real constant-rate capture, its delivery paused partway as a sink joining a report
	/// pauses it, then resumed a packet at a time: the rate its PCR measured times the rest, so
	/// the pause counts nothing.
	#[tokio::test(start_paused = true)]
	async fn a_paused_cbr_capture_counts_nothing() {
		const CLIP: &[u8] = include_bytes!("test_data/bbb5s_cbr.ts");
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		let half = CLIP.len() / 2 / 188 * 188;
		import.decode(&CLIP[..half]).unwrap();
		wait(1_000).await;
		for packet in CLIP[half..].chunks(188) {
			import.decode(packet).unwrap();
		}
		import.finish().unwrap();
		assert_eq!(import.stats().health(), [0; 9]);
	}

	/// A flagged break without a PCR still voids the measured rate: a stall after the
	/// multiplex changed rate is timed at the rate measured since, never the one before.
	#[tokio::test(start_paused = true)]
	async fn a_flagged_break_without_a_pcr_remeasures_the_rate() {
		let stalled = |t: u64| (5_200..6_200).contains(&t);
		let mut feed = Feed {
			slot: 6,
			..Default::default()
		};
		feed.clean(0, 3_000);
		let mut flag = feed.header(VIDEO, false, true, false);
		flag.extend_from_slice(&[183, 0x80]);
		feed.push(flag);
		// Twice the rate from the break on: the window spanning it would keep the old one.
		feed.slot = 12;
		feed.run(3_000, 7_000, |t, _| t * MS, |t, _| !stalled(t));
		let stats = feed.stats();
		assert_eq!(
			(
				stats.pat_error,
				stats.pmt_error,
				stats.pts_error,
				stats.pcr_repetition_error
			),
			(2, 2, 2, 1),
			"{stats:?}"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn discontinuity_without_pcr_does_not_hide_its_absence() {
		let start = kio::clock::Instant::now();
		let mut health = super::Health::default();
		health.advance(at(0, start));
		health.pcr_pid(Some(VIDEO));
		let mut feed = Feed::default();
		let mut flag = feed.header(VIDEO, false, true, false);
		flag.extend_from_slice(&[183, 0x80]);
		feed.push(flag);
		for (count, ms) in [50, 100, 150, 201].into_iter().enumerate() {
			health.packet(
				(&*feed.last_packet()).try_into().unwrap(),
				super::Continuation::Broken,
				at(count as u64 + 1, start + std::time::Duration::from_millis(ms)),
			);
			health.flush();
		}
		assert_eq!(
			health.errors.pids.get(&VIDEO).map_or(0, |pid| pid.pcr_repetition_error),
			1
		);
	}

	#[tokio::test(start_paused = true)]
	async fn a_declared_pcr_that_never_arrives_is_missing() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		let mut feed = Feed::default();
		feed.run(0, 100, |t, _| t * MS, |_, part| part != Part::Pcr);
		import.decode(&std::mem::take(&mut feed.out)).unwrap();
		wait(100).await;
		feed.null();
		import.decode(&std::mem::take(&mut feed.out)).unwrap();
		assert_eq!(import.stats().pcr_repetition_error, 0, "exactly 100 ms is on time");
		wait(1).await;
		feed.null();
		import.decode(&std::mem::take(&mut feed.out)).unwrap();
		assert_eq!(import.stats().pcr_repetition_error, 1);
	}

	/// A stopped input advances nothing until it resumes, and neither EOF nor a timer adds
	/// time: what resumes it says whether the encoder stopped too. A PCR that resumes on time
	/// was only delayed in delivery. One that steps by the outage counts it, with the PTS the
	/// step leaves late. One still missing leaves arrival to time its own outage, while the
	/// tables and PTS that resume with it answer for their delay.
	#[tokio::test(start_paused = true)]
	async fn a_stopped_input_is_judged_by_what_resumes_it() {
		#[derive(Clone, Copy, Debug)]
		enum Resume {
			Eof,
			Delayed,
			Stepped,
			Stalled,
		}
		for resume in [Resume::Eof, Resume::Delayed, Resume::Stepped, Resume::Stalled] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut import = Import::new(broadcast, catalog.reserve());
			let mut feed = Feed::default();
			feed.clean(0, 100);
			import.decode(&std::mem::take(&mut feed.out)).unwrap();
			wait(1_000).await;
			assert_eq!(import.stats().health(), [0; 9], "no timer");
			match resume {
				Resume::Eof => {}
				Resume::Delayed => feed.clean(100, 200),
				Resume::Stepped => feed.clean(1_000, 1_100),
				Resume::Stalled => feed.run(100, 200, |t, _| t * MS, |_, part| part != Part::Pcr),
			}
			import.decode(&std::mem::take(&mut feed.out)).unwrap();
			import.finish().unwrap();
			let stats = import.stats();
			let (pat, pts, pcr) = match resume {
				Resume::Eof | Resume::Delayed => (0, 0, 0),
				Resume::Stepped => (0, 2, 1),
				Resume::Stalled => (0, 0, 1),
			};
			assert_eq!(
				(
					stats.pat_error,
					stats.pmt_error,
					stats.pts_error,
					stats.pcr_repetition_error
				),
				(pat, pat, pts, pcr),
				"{resume:?}: {stats:?}"
			);
		}
	}

	/// Input held behind a suspected clock step keeps the arrival it came with and is graded
	/// at it once routed: neither the later input that releases it nor EOF re-times it.
	#[tokio::test(start_paused = true)]
	async fn held_input_is_graded_at_its_own_arrival() {
		for eof in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut import = Import::new(broadcast, catalog.reserve());
			let mut feed = Feed::default();
			feed.clean(0, 200);
			import.decode(&std::mem::take(&mut feed.out)).unwrap();
			wait(50).await;
			// The PCR alone steps back: held until a stream says whether the source stepped.
			feed.pcr((190 * MS + PCR_WRAP - 5_000 * MS) % PCR_WRAP, false);
			import.decode(&std::mem::take(&mut feed.out)).unwrap();
			assert!(import.held() >= 188, "the step is held");
			wait(200).await;
			if eof {
				import.finish().unwrap();
			} else {
				// The streams carry on 200 ms later without a PCR, clearing the step: the PCR
				// is missing from when the held one arrived.
				feed.pes(VIDEO, 200 * 90, false);
				feed.pes(PEER, 200 * 90, false);
				import.decode(&std::mem::take(&mut feed.out)).unwrap();
				assert!(import.held() < 188, "the step cleared");
			}
			let stats = import.stats();
			assert_eq!(stats.pcr_repetition_error, u64::from(!eof), "eof {eof}: {stats:?}");
			assert_eq!(stats.pat_error, 0, "eof {eof}: {stats:?}");
		}
	}

	/// `part` is missing for `len` ms after 1 s.
	fn gap(part: Part, len: u64) -> impl Fn(u64, Part) -> bool {
		move |t, p| p != part || !(t > 1_000 && t < 1_000 + len)
	}

	#[tokio::test(start_paused = true)]
	async fn a_clean_feed_counts_nothing() {
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
	/// one under neither, the wrap is no gap, and a step back is a discontinuity only. The clock
	/// steps back alone: the media stepping with it would restart the import instead.
	#[tokio::test(start_paused = true)]
	async fn pcr_gaps_are_graded_on_their_values() {
		let stats = |clock: &dyn Fn(u64, Part) -> u64, keep: &dyn Fn(u64, Part) -> bool| {
			let mut feed = Feed::default();
			feed.run(0, 2_000, clock, keep);
			feed.stats()
		};
		let pcr = |clock: &dyn Fn(u64, Part) -> u64, keep: &dyn Fn(u64, Part) -> bool| {
			let stats = stats(clock, keep);
			let health = stats.health();
			assert_eq!(
				(&health[..6], health[8]),
				(&[0; 6][..], 0),
				"only the PCR checks move: {stats:?}"
			);
			(stats.pcr_repetition_error, stats.pcr_discontinuity_indicator_error)
		};
		let steady = |t: u64, _: Part| t * MS;
		assert_eq!(pcr(&steady, &gap(Part::Pcr, 150)), (1, 1), "a 150 ms gap");
		assert_eq!(pcr(&steady, &gap(Part::Pcr, 80)), (0, 0), "an 80 ms gap");
		assert_eq!(pcr(&steady, &gap(Part::Pcr, 100)), (0, 0), "a gap of exactly 100 ms");
		assert_eq!(pcr(&|t, _| PCR_WRAP - 1_000 * MS + t * MS, &all), (0, 0), "the wrap");
		assert_eq!(
			pcr(
				&|t, part| if t < 1_000 || part != Part::Pcr {
					t * MS
				} else {
					(t - 500) * MS
				},
				&all
			),
			(0, 1),
			"an unsignalled step back"
		);

		// An unsignalled 500 ms jump also stretches the program clock by 510 ms, which makes
		// the PAT and the PMT each just late. The 40 ms PES cadence absorbs it.
		let jump = stats(&|t, _| if t < 1_000 { t * MS } else { (t + 500) * MS }, &all);
		assert_eq!(
			(jump.pcr_repetition_error, jump.pcr_discontinuity_indicator_error),
			(1, 1),
			"an unsignalled 500 ms jump"
		);
		assert_eq!((jump.pat_error, jump.pmt_error), (1, 1), "{jump:?}");
		assert_eq!(jump.health().iter().sum::<u64>(), 4, "{jump:?}");
	}

	/// The same 500 ms jump, signalled on the PCR PID, is a new time base and grades nothing.
	#[tokio::test(start_paused = true)]
	async fn a_signalled_jump_is_not_graded() {
		let mut feed = Feed::default();
		feed.clean(0, 1_000);
		feed.pcr(1_500 * MS, true);
		feed.run(1_000, 2_000, |t, _| (t + 500) * MS, all);
		let stats = feed.stats();
		assert_eq!(stats.health(), [0; 9], "{stats:?}");
	}

	/// A missing PAT counts once for each half second of program clock it is missing, and
	/// a PAT exactly 0.5 s late is on time.
	#[tokio::test(start_paused = true)]
	async fn a_late_pat_counts_once_per_half_second() {
		for (len, count) in [(400, 0), (500, 0), (800, 1), (1_200, 2)] {
			let mut feed = Feed::default();
			feed.run(0, 3_000, |t, _| t * MS, gap(Part::Pat, len));
			let stats = feed.stats();
			assert_eq!(stats.pat_error, count, "PAT missing for {len} ms");
			assert_eq!(stats.pmt_error, 0, "PAT missing for {len} ms");
		}
	}

	/// PID 0 carries only PATs, in the clear.
	#[tokio::test(start_paused = true)]
	async fn pid_zero_carries_only_clear_pats() {
		let mut feed = Feed::default();
		feed.clean(0, 500);
		feed.psi(0, &psi::section(0x02, 1, 0, 0, 0, &[0xe1, 0x01, 0xf0, 0x00]));
		feed.pat();
		feed.last_packet()[3] |= 0x80;
		feed.clean(500, 1_000);
		let stats = feed.stats();
		assert_eq!(stats.pat_error, 2, "{stats:?}");
	}

	#[tokio::test(start_paused = true)]
	async fn a_late_pmt_counts_on_its_own_check() {
		let mut feed = Feed::default();
		feed.run(0, 3_000, |t, _| t * MS, gap(Part::Pmt, 800));
		let stats = feed.stats();
		assert_eq!((stats.pmt_error, stats.pat_error), (1, 0), "{stats:?}");
	}

	/// A 1 s gap in one stream's PTS counts once, on that stream.
	#[tokio::test(start_paused = true)]
	async fn a_pts_gap_counts_on_its_own_pid() {
		let mut feed = Feed::default();
		feed.run(0, 3_000, |t, _| t * MS, gap(Part::Pes(PEER), 1_000));
		let stats = feed.stats();
		assert_eq!(stats.pts_error, 1, "{stats:?}");
		assert_eq!(stats.streams[&PEER].pts_error, 1);
		assert_eq!(stats.streams[&VIDEO].pts_error, 0);
	}

	/// A stream a PMT revision drops has no PTS cadence left to grade.
	#[tokio::test(start_paused = true)]
	async fn a_stream_the_pmt_drops_has_no_pts_cadence() {
		let mut feed = Feed::default();
		feed.clean(0, 1_000);
		for t in (1_000..3_000).step_by(100) {
			feed.pmt(1, &[VIDEO]);
			feed.run(
				t,
				t + 100,
				|t, _| t * MS,
				|_, part| part != Part::Pmt && part != Part::Pes(PEER),
			);
		}
		let stats = feed.stats();
		assert_eq!(stats.health(), [0; 9], "{stats:?}");
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

	#[tokio::test(start_paused = true)]
	async fn continuity_errors_follow_iso_13818_1() {
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
	#[tokio::test(start_paused = true)]
	async fn a_corrupt_packet_counts_only_a_transport_error() {
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
	#[tokio::test(start_paused = true)]
	async fn sync_is_lost_after_two_corrupt_sync_bytes() {
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
	#[tokio::test(start_paused = true)]
	async fn a_shifted_grid_loses_sync_once() {
		let mut feed = Feed::default();
		feed.clean(0, 1_000);
		feed.out.extend_from_slice(&[0x00; 10]);
		feed.clean(1_000, 2_000);
		let stats = feed.stats();
		assert_eq!((stats.sync_byte_error, stats.ts_sync_loss), (2, 1), "{stats:?}");
		assert_eq!(stats.health().iter().sum::<u64>(), 3, "{stats:?}");
	}

	/// Every program's importer grades every packet of the multiplex, so a fault counts once.
	#[tokio::test(start_paused = true)]
	async fn a_multiplex_counts_each_fault_once() {
		let origin = crate::source::produce_origin();
		let mut programs = Programs::new(
			origin,
			moq_net::Path::from_static("event.hang"),
			crate::catalog::Config::default(),
		);
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
