//! Whether the source's clock stepped onto another timeline, which ends the import's instance.
//!
//! A contribution switched between two encoders whose clocks were never locked steps every
//! timestamp at once without `discontinuity_indicator`. Splicing the two would put two sources
//! on one timeline, and a step back would break the rule that group starts never go
//! backwards within an instance, so the import ends its instance at the step and hands the
//! input from there to a caller that republishes it under a new epoch.
//!
//! The step is judged the way ffmpeg infers an unflagged one: the program clock and every
//! elementary stream that keeps a timestamp cadence step together, forward by more than
//! [`LEAP`] or back by any amount short of the 33-bit wrap. TR 101 290's 100 ms bound would
//! read SRT loss as a step. One clock stepping alone, such as a corrupt PCR or a stream whose
//! PTS jumps, is not a new source and is left to the import. After a flagged break, the
//! streams stepping back is a rewind; one stepping forward splices as before.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bytes::Bytes;
use mpeg2ts::ts::TsPacket;

use super::demux::Demux;
use super::health::{Continuation, Continuity, Delta, PTS_LIMIT};
use super::import::{Framer, Payload, STAGING_BYTES_MAX, adaptation_valid, discontinuity_indicator, payload, pcr};
use super::mux_rate::PCR_WRAP;
use super::passthrough::PCR_HZ;
use super::psi::PesStart;

/// A forward step beyond this is another timeline: ffmpeg's `dts_delta_threshold`.
const LEAP: Duration = Duration::from_secs(10);
const PTS_HZ: u64 = 90_000;
const PTS_WRAP: u64 = 1 << 33;

/// The source's clock stepped onto another timeline, so the [`Import`](super::Import) ended its
/// instance there.
///
/// What follows is another broadcast. A caller that carries on finishes this import and
/// publishes [`remainder`](Self::remainder) into a new one, as a new instance under a new epoch.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
#[error("the source's clock {step}; what follows is a new broadcast")]
pub struct Restart {
	/// How the clock stepped.
	pub step: Step,
	/// The input from the step on, which this import did not read.
	pub remainder: Bytes,
}

/// How the source's clock stepped, ending an import's instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Step {
	/// Back by `by`, after a `discontinuity_indicator` when `flagged`.
	Rewind {
		/// How far back.
		by: Duration,
		/// Whether the clock PID flagged the break.
		flagged: bool,
	},
	/// Forward by `by`, more than 10 s, without a `discontinuity_indicator`.
	Leap {
		/// How far forward.
		by: Duration,
	},
}

impl std::fmt::Display for Step {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Rewind { by, flagged: true } => write!(f, "went back {by:?} after a flagged break"),
			Self::Rewind { by, flagged: false } => write!(f, "went back {by:?} without a discontinuity_indicator"),
			Self::Leap { by } => write!(f, "jumped forward {by:?} without a discontinuity_indicator"),
		}
	}
}

/// What the import routes: which packets carry the clocks being watched.
pub(super) struct Routes<'a> {
	/// The PID the PMT names for the program clock.
	pub pcr_pid: Option<u16>,
	/// Whether a PID routes to an elementary stream whose PES are timestamped.
	pub timed: &'a dyn Fn(u16) -> bool,
	/// The program clock: PCR ticks summed since the first, as [`Import::now`](super::Import::now).
	pub now: Option<u64>,
}

/// What to do with the next packet.
pub(super) enum Verdict {
	/// Route it.
	Route,
	/// Route nothing yet: a step is being judged on input still to come.
	Hold,
	/// The clock stepped at it: end the instance there.
	Restart(Step),
}

/// Watches the program clock and each timed PID, holding the input while a step is judged.
#[derive(Default)]
pub(super) struct Steps {
	/// The clock PID, and the last PCR routed on it.
	clock: Option<(u16, Option<u64>)>,
	/// Each timed PID's last DTS, or PTS without one, as routed.
	pids: HashMap<u16, Seen>,
	/// Packets before this input offset were judged already, so they route without a lookahead.
	cleared: u64,
	/// The step being judged, from the next packet to route on.
	ahead: Option<Ahead>,
	/// The PES headers begun on the timed PIDs, as routed.
	headers: Headers,
}

/// A PID's last timestamp.
#[derive(Clone, Copy)]
struct Seen {
	/// Raw 90 kHz.
	ticks: u64,
	/// The program clock when it was routed.
	at: Option<u64>,
}

/// Which way a clock stepped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
	Back,
	Leap,
}

/// One clock reading against the last.
#[derive(Clone, Copy)]
enum Reading {
	Inline,
	Stepped(Way, Duration),
}

impl Reading {
	fn new(last: u64, value: u64, wrap: u64, hz: u64) -> Self {
		let duration = |ticks: u64| {
			Duration::from_nanos(u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(hz)).unwrap_or(u64::MAX))
		};
		match Delta::between(last, value, wrap) {
			Delta::Back(ticks) => Self::Stepped(Way::Back, duration(ticks)),
			Delta::Forward(ticks) if duration(ticks) > LEAP => Self::Stepped(Way::Leap, duration(ticks)),
			Delta::Forward(_) => Self::Inline,
		}
	}

	fn way(self) -> Option<Way> {
		match self {
			Self::Inline => None,
			Self::Stepped(way, _) => Some(way),
		}
	}
}

/// The clock readings one packet carries.
#[derive(Default)]
struct Marks {
	/// A `discontinuity_indicator` on the clock PID.
	flagged: bool,
	/// A PCR on the clock PID.
	pcr: Option<u64>,
	/// The DTS, or PTS without one, of a PES header this packet completes on a timed PID.
	pes: Option<u64>,
	/// Its timed PID's chain after it, or `None` when it leaves the chain as it was.
	chain: Option<Chain>,
}

impl Marks {
	fn read(pkt: &[u8; TsPacket::SIZE], routes: &Routes, headers: &Headers) -> Self {
		let mut marks = Self::default();
		let pid = (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]);
		// A timed PID's chain follows every packet the import's does, the damaged ones too.
		if (routes.timed)(pid) {
			(marks.chain, marks.pes) = headers.read(pid, pkt);
		}
		// A packet the demodulator flagged corrupt, or whose adaptation field overruns itself,
		// says nothing the import trusts.
		if pkt[1] & 0x80 != 0 || !adaptation_valid(pkt) {
			return marks;
		}
		if routes.pcr_pid == Some(pid) {
			marks.flagged = discontinuity_indicator(pkt);
			marks.pcr = pcr(pkt);
		}
		marks
	}
}

/// One timed PID's packets so far: whether they still chain, and the PES header begun on it.
#[derive(Clone, Default)]
struct Chain {
	continuity: Continuity,
	begun: Option<Vec<u8>>,
}

/// The PES headers begun on each timed PID, so one split across packets still reads.
///
/// Packets join under the import's continuity rules, so a retransmission or a loss cannot
/// splice a timestamp out of bytes the import never joins.
#[derive(Clone, Default)]
struct Headers(HashMap<u16, Chain>);

impl Headers {
	/// What `pkt` does on `pid` without retaining it: the chain after it, `None` when unchanged,
	/// and the DTS, or PTS without one, of a header it completes.
	fn read(&self, pid: u16, pkt: &[u8; TsPacket::SIZE]) -> (Option<Chain>, Option<u64>) {
		let mut chain = self.0.get(&pid).cloned().unwrap_or_default();
		match chain.continuity.observe(pkt) {
			// The import ignores a retransmission entirely.
			Continuation::Duplicate => return (None, None),
			Continuation::Contiguous => {}
			// The bytes joined so far are lost, as the import's partial PES is.
			Continuation::Broken => chain.begun = None,
			Continuation::Corrupt => {
				chain.begun = None;
				return (Some(chain), None);
			}
		}
		// The import refuses a malformed adaptation field and cannot read a scrambled payload.
		if !adaptation_valid(pkt) || pkt[3] & 0xc0 != 0 {
			chain.begun = None;
			return (Some(chain), None);
		}
		let Payload::Bytes(payload) = payload(pkt) else {
			return (Some(chain), None);
		};
		// A PES start abandons whatever header was begun before it, as the import's does.
		let begun = chain.begun.take();
		let header = if pkt[1] & 0x40 != 0 {
			payload.to_vec()
		} else if let Some(mut begun) = begun {
			begun.extend_from_slice(payload);
			begun
		} else {
			return (Some(chain), None);
		};
		// The optional fields are at most 255 bytes, so a held header stays small.
		let ticks = match PesStart::header_len(&header) {
			Ok(len) if header.len() >= len => match PesStart::parse(&header) {
				Ok(PesStart {
					pts: Some(pts), dts, ..
				}) => Some(dts.unwrap_or(pts)),
				_ => None,
			},
			Ok(_) => {
				chain.begun = Some(header);
				None
			}
			Err(_) => None,
		};
		(Some(chain), ticks)
	}

	/// Retain the chain a packet left on `pid`.
	fn apply(&mut self, pid: u16, chain: Option<Chain>) {
		if let Some(chain) = chain {
			self.0.insert(pid, chain);
		}
	}
}

/// A step being judged, from the packet that showed it on.
struct Ahead {
	/// The input offset of that packet, where a new instance starts.
	at: u64,
	/// The way it stepped and by how much, or `None` for the clock PID's flagged break, which
	/// ends the instance only if the streams step back.
	stepped: Option<(Way, Duration)>,
	/// Whether the PCR still has to step with it.
	pcr: bool,
	/// The PIDs that still have to step with it.
	pids: HashSet<u16>,
	/// The PIDs whose first PES since was judged.
	heard: HashSet<u16>,
	/// The PES headers begun on the timed PIDs, as read ahead.
	headers: Headers,
	/// How far back the first PID to step back went, which a flagged rewind reports.
	back: Option<Duration>,
	/// The first PCR read since, which times how long the silent streams are waited for.
	since: Option<u64>,
	/// The next input offset to read, and the framer's state there.
	scan: u64,
	framer: Framer,
	/// The step, once every clock confirmed it.
	confirmed: Option<Step>,
}

impl Steps {
	/// Judge the next packet the import would route on `demux`.
	pub(super) fn judge(&mut self, demux: &Demux, routes: &Routes) -> Verdict {
		if self.clock.map(|(pid, _)| pid) != routes.pcr_pid {
			self.clock = routes.pcr_pid.map(|pid| (pid, None));
		}
		let (mut cursor, mut framer) = demux.cursor();
		let start = (cursor, framer);
		let Some((at, pkt)) = demux.peek(&mut cursor, &mut framer) else {
			return Verdict::Route;
		};
		let pid = (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]);
		let marks = Marks::read(pkt, routes, &self.headers);

		if self.ahead.is_none() && at >= self.cleared {
			self.ahead = self.trigger(at, pid, &marks, routes, start);
		}
		let Some(ahead) = &mut self.ahead else {
			self.observe(pid, marks, routes);
			return Verdict::Route;
		};
		if let Some(step) = ahead.confirmed {
			return Verdict::Restart(step);
		}
		match ahead.scan(demux, routes, &self.clock, &self.pids) {
			Some(Ok(step)) => {
				ahead.confirmed = Some(step);
				Verdict::Restart(step)
			}
			Some(Err(cleared)) => {
				self.cleared = cleared;
				self.ahead = None;
				self.observe(pid, marks, routes);
				Verdict::Route
			}
			None => Verdict::Hold,
		}
	}

	/// No input follows. A step still being judged never completed, so its input routes as
	/// read; returns whether there is any.
	pub(super) fn settle(&mut self) -> bool {
		if self.ahead.as_ref().is_some_and(|ahead| ahead.confirmed.is_none()) {
			self.ahead = None;
			self.cleared = u64::MAX;
			return true;
		}
		false
	}

	/// `pid` changed route: its timestamps no longer continue the last.
	pub(super) fn forget(&mut self, pid: u16) {
		self.pids.remove(&pid);
		self.headers.0.remove(&pid);
	}

	/// Start judging a step at the packet at `at` if it shows one, with the clocks expected to
	/// step with it.
	fn trigger(
		&self,
		at: u64,
		pid: u16,
		marks: &Marks,
		routes: &Routes,
		(scan, framer): (u64, Framer),
	) -> Option<Ahead> {
		let last_pcr = self.clock.and_then(|(_, last)| last);
		let stepped = if marks.flagged {
			None
		} else {
			// Without a clock to step with them, the streams alone say nothing of the source.
			last_pcr?;
			let pcr = marks
				.pcr
				.zip(last_pcr)
				.map(|(value, last)| Reading::new(last, value, PCR_WRAP, PCR_HZ));
			let pes = marks
				.pes
				.zip(self.pids.get(&pid))
				.map(|(value, last)| Reading::new(last.ticks, value, PTS_WRAP, PTS_HZ));
			let stepped = [pcr, pes].into_iter().flatten().find_map(|reading| match reading {
				Reading::Stepped(way, by) => Some((way, by)),
				Reading::Inline => None,
			});
			Some(stepped?)
		};
		// A PID that kept TR 101 290's PTS cadence up to here is expected to step too.
		let mut pids: HashSet<u16> = self
			.pids
			.iter()
			.filter(|(pid, seen)| {
				(routes.timed)(**pid)
					&& match (seen.at, routes.now) {
						(_, None) => true,
						(Some(at), Some(now)) => now.saturating_sub(at) <= PTS_LIMIT,
						(None, Some(_)) => false,
					}
			})
			.map(|(pid, _)| *pid)
			.collect();
		if marks.pes.is_some() && self.pids.contains_key(&pid) {
			pids.insert(pid);
		}
		Some(Ahead {
			at,
			stepped,
			pcr: !marks.flagged && last_pcr.is_some(),
			pids,
			heard: HashSet::new(),
			headers: self.headers.clone(),
			back: None,
			since: None,
			scan,
			framer,
			confirmed: None,
		})
	}

	/// Record the readings of a packet routed.
	fn observe(&mut self, pid: u16, marks: Marks, routes: &Routes) {
		if let (Some(value), Some((_, last))) = (marks.pcr, &mut self.clock) {
			*last = Some(value);
		}
		if let Some(ticks) = marks.pes {
			self.pids.insert(pid, Seen { ticks, at: routes.now });
		}
		self.headers.apply(pid, marks.chain);
	}
}

impl Ahead {
	/// Read on until every clock expected to step has, `Some(Ok)`, or one did not, `Some(Err)`
	/// with the offset judged through. `None` waits for more input.
	fn scan(
		&mut self,
		demux: &Demux,
		routes: &Routes,
		clock: &Option<(u16, Option<u64>)>,
		pids: &HashMap<u16, Seen>,
	) -> Option<Result<Step, u64>> {
		let last_pcr = clock.and_then(|(_, last)| last);
		loop {
			// Held no longer than the import stages bytes awaiting a timestamp.
			if self.scan - self.at > STAGING_BYTES_MAX as u64 {
				return Some(Err(self.scan));
			}
			let (at, pkt) = demux.peek(&mut self.scan, &mut self.framer)?;
			let end = at + TsPacket::SIZE as u64;
			let pid = (u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2]);
			let marks = Marks::read(pkt, routes, &self.headers);

			if self.pcr
				&& let (Some(value), Some(last)) = (marks.pcr, last_pcr)
			{
				let reading = Reading::new(last, value, PCR_WRAP, PCR_HZ);
				if reading.way() != self.stepped.map(|(way, _)| way) {
					return Some(Err(end));
				}
				self.pcr = false;
			}
			// A cadenced stream speaks within TR 101 290's PTS interval, so one still silent
			// that long after the step dropped out with the old source: those that spoke decide.
			if let Some(value) = marks.pcr {
				let since = *self.since.get_or_insert(value);
				if let Delta::Forward(elapsed) = Delta::between(since, value, PCR_WRAP)
					&& elapsed >= PTS_LIMIT
					&& !self.pcr
				{
					return Some(self.verdict(end, !self.heard.is_empty()));
				}
			}
			// Every PID with a timestamp to step from has its say, once: a sparse one too, which
			// only the cadenced ones are waited for.
			if let (Some(value), Some(last)) = (marks.pes, pids.get(&pid))
				&& self.heard.insert(pid)
			{
				self.pids.remove(&pid);
				let reading = Reading::new(last.ticks, value, PTS_WRAP, PTS_HZ);
				let wanted = match self.stepped {
					Some((way, _)) => way,
					None => Way::Back,
				};
				let Reading::Stepped(way, by) = reading else {
					return Some(Err(end));
				};
				if way != wanted {
					return Some(Err(end));
				}
				self.back.get_or_insert(by);
			}
			self.headers.apply(pid, marks.chain);

			// A clock alone, such as one corrupt PCR, needs a stream to step with it.
			if !self.pcr && self.pids.is_empty() && (self.stepped.is_none() || !self.heard.is_empty()) {
				return Some(self.verdict(end, true));
			}
		}
	}

	/// The step every clock heard from agreed on, `stepped` when that is enough to end the
	/// instance, or the offset judged through when it is not.
	fn verdict(&self, end: u64, stepped: bool) -> Result<Step, u64> {
		match self.stepped {
			Some((Way::Back, by)) if stepped => Ok(Step::Rewind { by, flagged: false }),
			Some((Way::Leap, by)) if stepped => Ok(Step::Leap { by }),
			Some(_) => Err(end),
			// A flagged break no PID has stepped back from is a splice.
			None => self.back.map(|by| Step::Rewind { by, flagged: true }).ok_or(end),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::super::fixture::{Flags, Mux};
	use super::*;

	const CLOCK: u16 = 0x1ff;
	const VIDEO: u16 = 0x100;
	/// 40 ms of program clock, in 27 MHz ticks.
	const FRAME: u64 = PCR_HZ / 25;
	/// The step the issue's contribution took, in 27 MHz ticks.
	const SWITCH: u64 = 16_844 * PCR_HZ + PCR_HZ / 2;

	/// Judge each packet of `ts` and route it, as an import does, with `timed` the PIDs that
	/// carry timestamped PES. Returns the first restart.
	fn judge(ts: &[u8], timed: &[u16]) -> Option<Step> {
		let mut demux = Demux::default();
		demux.push(ts);
		let mut steps = Steps::default();
		let timed = |pid| timed.contains(&pid);
		let routes = Routes {
			pcr_pid: Some(CLOCK),
			timed: &timed,
			now: None,
		};
		loop {
			match steps.judge(&demux, &routes) {
				Verdict::Restart(step) => return Some(step),
				Verdict::Hold => return None,
				Verdict::Route => demux.next()?,
			};
		}
	}

	/// A video PES at `pts` (90 kHz), with a short body.
	fn pes_bytes(pts: u64) -> Vec<u8> {
		let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
		pes.extend_from_slice(&[
			0x21 | ((pts >> 29) & 0x0e) as u8,
			(pts >> 22) as u8,
			0x01 | ((pts >> 14) & 0xfe) as u8,
			(pts >> 7) as u8,
			0x01 | ((pts << 1) & 0xfe) as u8,
		]);
		pes.extend_from_slice(&[0xaa; 32]);
		pes
	}

	/// One packet on `pid` carrying `payload`, padded with adaptation stuffing as a muxer does.
	fn ts(cc: &mut u8, pid: u16, start: bool, payload: &[u8]) -> [u8; 188] {
		let mut pkt = [0xff; 188];
		pkt[..3].copy_from_slice(&[0x47, u8::from(start) << 6 | (pid >> 8) as u8, pid as u8]);
		pkt[3] = 0x30 | *cc;
		pkt[4] = (183 - payload.len()) as u8;
		pkt[5] = 0x00;
		pkt[188 - payload.len()..].copy_from_slice(payload);
		*cc = (*cc + 1) & 0x0f;
		pkt
	}

	/// One PES start on `pid` with `pts` (90 kHz), its header split at each of `splits` across
	/// packets. The packet holding fragment `repeat` is sent twice, as ISO 13818-1 allows a
	/// retransmission.
	fn pes(cc: &mut u8, pid: u16, pts: u64, splits: &[usize], repeat: Option<usize>) -> Vec<u8> {
		let pes = pes_bytes(pts);
		let bounds: Vec<usize> = std::iter::once(0)
			.chain(splits.iter().copied())
			.chain(std::iter::once(pes.len()))
			.collect();
		let mut out = Vec::new();
		for (i, range) in bounds.windows(2).enumerate() {
			let pkt = ts(cc, pid, i == 0, &pes[range[0]..range[1]]);
			out.extend_from_slice(&pkt);
			if repeat == Some(i) {
				out.extend_from_slice(&pkt);
			}
		}
		out
	}

	/// A second of the clock and the video on it, from `start` (27 MHz).
	fn second(mux: &mut Mux, cc: &mut u8, start: u64, video: bool) {
		for frame in 0..25 {
			let clock = (start + frame * FRAME) % PCR_WRAP;
			mux.packet(CLOCK, Flags::pcr(clock));
			if video {
				mux.out
					.extend(pes(cc, VIDEO, (clock / 300 + 9_000) % PTS_WRAP, &[7], None));
			}
		}
	}

	/// A program clock stepping with no stream to step with it, such as one corrupt PCR in a
	/// program whose streams are idle, is not another source.
	#[test]
	fn a_clock_alone_does_not_restart() {
		let (mut mux, mut cc) = (Mux::default(), 0);
		second(&mut mux, &mut cc, PCR_HZ, false);
		second(&mut mux, &mut cc, PCR_WRAP + 2 * PCR_HZ - SWITCH, false);
		assert_eq!(judge(&mux.out, &[VIDEO]), None);
	}

	/// A retransmitted fragment of a split PES header is ignored, as the import ignores it, so
	/// it cannot read as the stream stepping with a lone PCR glitch.
	#[test]
	fn a_repeated_header_fragment_does_not_step() {
		let (mut mux, mut cc) = (Mux::default(), 0);
		mux.packet(CLOCK, Flags::pcr(PCR_HZ));
		mux.out.extend(pes(&mut cc, VIDEO, 90_000, &[7], None));
		// One corrupt PCR, then the next frame on time, its PTS 93,600 split after its first byte
		// and the next two. Joining the repeat would read `21 00 05 00 05`, PTS 65,538: back.
		mux.packet(CLOCK, Flags::pcr(PCR_WRAP + PCR_HZ + FRAME - SWITCH));
		mux.out.extend(pes(&mut cc, VIDEO, 93_600, &[10, 12], Some(1)));
		second(&mut mux, &mut cc, PCR_HZ + 2 * FRAME, true);
		assert_eq!(judge(&mux.out, &[VIDEO]), None);
	}

	/// A PES start abandons the header begun before it, as the import's does, so the new PES's
	/// body cannot complete the old header and read as a stream stepping with a lone PCR glitch.
	#[test]
	fn a_pes_start_abandons_the_header_begun() {
		let (mut mux, mut cc) = (Mux::default(), 0);
		mux.packet(CLOCK, Flags::pcr(PCR_HZ));
		mux.out.extend(pes(&mut cc, VIDEO, 90_000, &[7], None));
		// A PES start cut short after nine bytes, then a whole one at PTS 93,600.
		mux.out.extend(ts(&mut cc, VIDEO, true, &pes_bytes(90_000)[..9]));
		mux.out.extend(ts(&mut cc, VIDEO, true, &pes_bytes(93_600)));
		// One corrupt PCR, then a body that, joined to the cut-short header, reads PTS 65,538: back.
		mux.packet(CLOCK, Flags::pcr(PCR_WRAP + PCR_HZ + FRAME - SWITCH));
		mux.out
			.extend(ts(&mut cc, VIDEO, false, &[0x21, 0x00, 0x05, 0x00, 0x05]));
		second(&mut mux, &mut cc, PCR_HZ + 2 * FRAME, true);
		assert_eq!(judge(&mux.out, &[VIDEO]), None);
	}

	/// A PES header split across packets still times its stream, so the switch is seen.
	#[test]
	fn a_split_pes_header_steps_with_the_clock() {
		let (mut mux, mut cc) = (Mux::default(), 0);
		second(&mut mux, &mut cc, PCR_HZ, true);
		second(&mut mux, &mut cc, PCR_WRAP + 2 * PCR_HZ - SWITCH, true);
		let Some(Step::Rewind { by, flagged: false }) = judge(&mux.out, &[VIDEO]) else {
			panic!("the switch was missed");
		};
		assert!(
			by.abs_diff(Duration::from_millis(16_844_460)) < Duration::from_millis(100),
			"{by:?}"
		);
	}
}
