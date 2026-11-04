//! Lays a TS export's packets onto its PCR grid.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::time::Duration;

use anyhow::Context;
use mpeg2ts::ts::TsPacket;
use rustc_hash::FxHashMap;

use super::export::PCR_INTERVAL;

/// Grid slots per second, so a multiplex rate in bits per second is also the per-slot
/// allowance in [`PACKET`] units.
const SLOTS_PER_SECOND: u64 = (Duration::from_secs(1).as_nanos() / PCR_INTERVAL.as_nanos()) as u64;
const _: () = assert!(
	Duration::from_secs(1)
		.as_nanos()
		.is_multiple_of(PCR_INTERVAL.as_nanos())
);
/// One packet in bits per second of slots: `rate` bits per second fills slot `index` up to
/// `index * rate / PACKET` packets since the start of the timeline, so no slot rounds on
/// its own.
const PACKET: u64 = TsPacket::SIZE as u64 * 8 * SLOTS_PER_SECOND;
/// The most of a packet a decoder buffer can receive: everything after its 4-byte header.
const PAYLOAD: usize = TsPacket::SIZE - 4;
/// How many empty slots to lay at most before the next packets: one second's worth. Past
/// this the media had an outage rather than a coarse cadence, and a dense clock history for
/// a span that carried no bytes only stalls anything pacing on the asserted values.
const BACKFILL: u128 = 40;
/// The longest a transport buffer may go without emptying (ISO 13818-1 2.4.2.3).
const TB_EMPTY: Duration = Duration::from_secs(1);
/// How many slots after the one its first program tables finish in a timeline that
/// [opens](Schedule::open) holds what a reader places on the program clock. The reader learns
/// the clock PID from the PMT, so it times nothing by the PCR opening their slot; the next
/// slot's PCR is the first it reads, and the one after closes the first interval, which it
/// needs to place a section or whole PES.
const OPENING: u128 = 2;
/// One tick of the system clock, in whole nanoseconds rounded up.
const TICK: i128 = 1_000_000_000_u128.div_ceil(SYSTEM_CLOCK) as i128;
/// The system clock in Hz, the unit of a PCR.
const SYSTEM_CLOCK: u128 = 27_000_000;
/// A PCR wraps at 2^33 ticks of 90 kHz, each 300 ticks of the system clock.
const PCR_WRAP: u128 = (1 << 33) * 300;

/// The grid slot a unit whose last byte has to arrive by `nanos` must be sent by: the
/// slot's bytes are timed up to its boundary.
pub(super) fn slot(nanos: u128) -> u128 {
	nanos / PCR_INTERVAL.as_nanos()
}

/// How many slots ahead of its due slot a unit on a PID with `buffer` may go out, within a
/// `window` of slots: as far as its decoder buffer reaches, or only the slot before its own
/// where no decoder buffer bounds it.
pub(super) fn ahead(buffer: Option<&Buffer>, window: u128) -> u128 {
	match buffer {
		Some(buffer) if buffer.size.is_some() => window.min(slot(buffer.reach.as_nanos())),
		_ => 1,
	}
}

/// One slot in system clock ticks.
const SLOT_TICKS: u128 = PCR_INTERVAL.as_nanos() * SYSTEM_CLOCK / 1_000_000_000;

/// The PCR, in system clock ticks, that opens slot `index`: one slot behind its boundary,
/// so the slot's bytes are timed up to that boundary. It backs off through the 33-bit wrap
/// rather than saturating, since the wire field is a circular clock.
fn grid_pcr(index: u128) -> u128 {
	(index % PCR_WRAP * SLOT_TICKS + PCR_WRAP - SLOT_TICKS) % PCR_WRAP
}

/// How many packets `rate` bits per second carries before slot `index` on the media timeline.
fn packets_before(index: u128, rate: u64) -> u128 {
	index * u128::from(rate) / u128::from(PACKET)
}

/// The PCR that opens slot `index` at `rate`: the time of its first byte, the timeline's
/// packets before it at the rate, one slot behind like [`grid_pcr`]. It is a function of the
/// slot alone, so every exporter of a broadcast stamps a slot alike.
fn rate_pcr(index: u128, rate: u64) -> u128 {
	let at = packets_before(index, rate) * TsPacket::SIZE as u128 * 8 * SYSTEM_CLOCK / u128::from(rate);
	(at % PCR_WRAP + PCR_WRAP - SLOT_TICKS) % PCR_WRAP
}

/// A PID's buffers in a receiver, as the schedule respects them (ISO 13818-1 2.4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Buffer {
	/// How fast the receiver's transport buffer drains whole packets, Rx, in bits per second.
	pub rate: u64,
	/// For video, how fast MB passes payload on into EB, Rbx, in bits per second (ISO
	/// 13818-1 2.14.3.1). `None` where the transport buffer feeds the decoder buffer directly.
	pub leak: Option<u64>,
	/// The decoder buffer (EB for video, B for audio) in bytes, which a unit fills from its
	/// first packet until it decodes. `None` bounds nothing, so the PID's units go out only
	/// in the slot before their own.
	pub size: Option<usize>,
	/// The transport buffer in bytes: 512 (ISO 13818-1 2.4.2.3), 480 for teletext (ETSI EN
	/// 300 472 5).
	pub transport: usize,
	/// How far ahead of its decode time the decoder buffer holds a unit: its size over the
	/// rate it fills at. Sending further ahead only adds latency.
	pub reach: Duration,
}

impl Buffer {
	/// The program tables' (ISO 13818-1 2.4.2.3, Rxsys).
	pub const SYSTEM: Self = Self {
		rate: 1_000_000,
		leak: None,
		size: None,
		transport: 512,
		reach: Duration::ZERO,
	};

	/// How long the receiver takes to pass one packet on into the decoder buffer: through the
	/// transport buffer at Rx and, for video, its payload through MB at Rbx, which overlap.
	pub fn drain(&self) -> Duration {
		let through = |bytes: usize, rate: u64| Duration::from_nanos(bytes as u64 * 8_000_000_000 / rate.max(1));
		let transport = through(TsPacket::SIZE, self.rate);
		self.leak
			.map_or(transport, |leak| transport.max(through(PAYLOAD, leak)))
	}

	/// The media packets slot `index` may carry on the PID unpadded: what its transport buffer
	/// passes on at Rx, less the slot's `clock` packet when it rides the PID, and for video no
	/// more payload than MB passes on at Rbx. Both count from the start of the timeline, so
	/// no slot rounds on its own. At least one, or a PID too slow for both the clock packet and
	/// one of its own would never finish a unit.
	fn per_slot(&self, index: u128, clock: bool) -> usize {
		let through = |rate: u64, bytes: usize| {
			let before =
				|index: u128| index * u128::from(rate) * PCR_INTERVAL.as_nanos() / (bytes as u128 * 8_000_000_000);
			before(index + 1) - before(index)
		};
		let packets = through(self.rate, TsPacket::SIZE).saturating_sub(u128::from(clock));
		let packets = self.leak.map_or(packets, |leak| packets.min(through(leak, PAYLOAD)));
		packets.max(1) as usize
	}
}

/// Bsys in bytes: the main buffer TBsys passes the program's system packets on into (ISO
/// 13818-1 2.4.2.3).
const BSYS: u128 = 1536;

/// Rsys, how fast a receiver removes system data from Bsys at multiplex `rate`, in bits per
/// second (ISO 13818-1 2.4.2.3, equation 2-7).
fn system_rate(rate: u64) -> u128 {
	(u128::from(rate) / 500).max(80_000)
}

/// The last transport arrival and the decoder removal, in nanoseconds.
#[derive(Clone, Copy)]
pub(super) struct Timing {
	/// When the last packet must enter the transport buffer.
	pub by: u128,
	/// When the decoder removes this unit.
	pub decode: u128,
}

/// The program tables as they stand ([`Schedule::set_psi`]), which each unit of them lays when
/// it begins to go out.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Psi {
	/// The PAT and PMT packets.
	pub packets: Vec<u8>,
	/// The same tables flagging a break, for the first unit of them to go out after a switch to
	/// another instance.
	pub flagged: Option<Vec<u8>>,
	/// The PID every slot's clock rides once they have gone out.
	pub clock: u16,
}

/// Program tables to go out ahead of the next unit pushed ([`Schedule::lead`]).
pub(super) struct Tables {
	/// The multiplex rate from the slot after the one they finish in.
	pub rate: Option<u64>,
	/// The PIDs they list that the tables before them did not. Their units wait for them, and
	/// so does the unit they lead.
	pub added: Vec<u16>,
}

/// One access unit's packets, with the tables muxed ahead of it, still to go out.
struct Unit {
	/// Counts up with each unit pushed.
	seq: u64,
	/// The elementary stream it carries, whose buffers bound it.
	pid: u16,
	/// The coarse deadline used by release windows and unpadded admission.
	due: u128,
	timing: Timing,
	/// The slot after the unit pushed before it, which it may go out from even without a
	/// window: nothing pushed later can be due sooner.
	start: u128,
	packets: Vec<u8>,
	/// Packets already laid out.
	sent: usize,
	keyframe: bool,
	/// It was pushed after its due slot went out, so it goes out as soon as it can.
	late: bool,
	/// The first byte's placement in nanoseconds and payload prefix (the section pointer field).
	/// It keeps no deadline of ours: it goes out as soon after as the buffers let it.
	place: Option<(u128, usize)>,
	/// Its bytes in the PID's decoder buffer, until it decodes.
	held: usize,
	/// The decode time and bytes of each frame of a PES that packs several, which leave the
	/// decoder buffer one by one; empty for a PES of one frame, which leaves at once.
	frames: Vec<(u128, usize)>,
	/// The packets it carries into TBsys, one run per PID.
	runs: Vec<Run>,
	/// For a unit of program tables, what they put in force and the PIDs they hold back.
	tables: Option<Program>,
}

/// What a unit of program tables puts in force once it has gone out ([`Tables`]).
struct Program {
	clock: u16,
	rate: Option<u64>,
	holds: Vec<u16>,
	/// The PID of the unit they lead, which they go out as far ahead of as it may.
	leads: u16,
}

impl Unit {
	fn count(&self) -> usize {
		self.packets.len() / TsPacket::SIZE
	}

	/// When its last bytes leave the decoder buffer.
	fn last_decode(&self) -> u128 {
		self.frames.last().map_or(self.timing.decode, |&(decode, _)| decode)
	}

	/// When each of its frames leaves the decoder buffer, with the bytes it takes: all it
	/// holds, for a unit of one frame.
	fn frames(&self) -> impl Iterator<Item = (u128, usize)> + '_ {
		let single = self.frames.is_empty().then_some((self.timing.decode, usize::MAX));
		self.frames.iter().copied().chain(single)
	}

	/// What it leaves in the decoder buffer once sent: each frame's PID, decode time and
	/// bytes, of the bytes it holds, which the frames decoded already have taken first.
	fn leaving(&self) -> Vec<(u16, u128, usize)> {
		let mut held = self.held;
		let mut out: Vec<_> = self.frames().collect();
		for (_, bytes) in out.iter_mut().rev() {
			*bytes = (*bytes).min(held);
			held -= *bytes;
		}
		out.into_iter()
			.filter(|&(_, bytes)| bytes > 0)
			.map(|(decode, bytes)| (self.pid, decode, bytes))
			.collect()
	}

	fn remaining(&self) -> usize {
		self.count() - self.sent
	}

	fn pid_at(&self, packet: usize) -> u16 {
		pid(&self.packets[packet * TsPacket::SIZE..])
	}

	/// How many of the packets in `range` are on `pid`.
	fn packets_on(&self, range: std::ops::Range<usize>, pid: u16) -> usize {
		range.filter(|&k| self.pid_at(k) == pid).count()
	}
}

/// The packets one unit carries into TBsys on one PID: the tables they hold, and where in the
/// unit each packet sits.
struct Run {
	pid: u16,
	table: Vec<u8>,
	at: Vec<usize>,
}

impl Run {
	/// How many of its packets have gone out of a unit that has sent `sent`.
	fn sent(&self, sent: usize) -> usize {
		self.at.partition_point(|&k| k < sent)
	}
}

/// Whether unit `i`'s next table packet must wait. One unit's tables go out at a time, so a
/// section spanning packets reassembles, and none overtakes a different table muxed ahead of
/// it on a PID it also carries, so a changed version follows the one it replaces. A copy of
/// the same tables may go first, as its unit may be due sooner. The unit going out is never
/// held back, nor, while none is, the first holding tables.
fn held_back(units: &VecDeque<Unit>, i: usize) -> bool {
	units.iter().enumerate().any(|(j, unit)| {
		let mut pending = unit
			.runs
			.iter()
			.filter(|run| run.sent(unit.sent) < run.at.len())
			.peekable();
		if j == i || pending.peek().is_none() {
			return false;
		}
		unit.runs.iter().any(|run| run.sent(unit.sent) > 0)
			|| (j < i
				&& pending.any(|run| {
					units[i]
						.runs
						.iter()
						.any(|own| own.pid == run.pid && own.table != run.table)
				}))
	})
}

fn pid(packet: &[u8]) -> u16 {
	u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2])
}

fn payload(packet: &[u8]) -> &[u8] {
	if packet[3] & 0x10 == 0 {
		return &[];
	}
	let start = if packet[3] & 0x20 != 0 {
		5 + usize::from(packet[4])
	} else {
		4
	};
	&packet[start..]
}

/// The bytes of `packet` that video's MB passes on into EB: its payload less any PES header,
/// which MB discards (ISO 13818-1 2.14.3.1).
fn elementary(packet: &[u8]) -> &[u8] {
	let body = payload(packet);
	match packet[1] & 0x40 != 0 && body.starts_with(&[0, 0, 1]) && body.len() >= 9 {
		true => body.get(9 + usize::from(body[8])..).unwrap_or_default(),
		false => body,
	}
}

/// A PCR of `ticks` as an adaptation field carries it: the 33-bit base, six reserved bits,
/// all ones as ISO 13818-1 requires, and the 9-bit extension.
pub(super) fn pcr_field(ticks: u64) -> [u8; 6] {
	let (base, extension) = ((ticks / 300) & ((1 << 33) - 1), ticks % 300);
	[
		(base >> 25) as u8,
		(base >> 17) as u8,
		(base >> 9) as u8,
		(base >> 1) as u8,
		((base as u8) << 7) | 0x7e | (extension >> 8) as u8,
		extension as u8,
	]
}

/// `packets`, the rest of a PES on one PID, rewritten so the first carries the PCR `ticks` in
/// its adaptation field: the payload moves on by the field's bytes, into the stuffing of the
/// last packet or one more packet. `None` unless every packet is the PID's with payload, and
/// its adaptation field, if any, carries nothing but flags and stuffing, as the export
/// writes them.
fn with_pcr(packets: &[u8], ticks: u64) -> Option<Vec<u8>> {
	let packets = packets.as_chunks::<{ TsPacket::SIZE }>().0;
	let first = packets.first()?;
	let mut body = Vec::with_capacity(packets.len() * PAYLOAD);
	let mut flags = Vec::with_capacity(packets.len());
	for packet in packets {
		if pid(packet) != pid(first) || packet[3] & 0x10 == 0 {
			return None;
		}
		// Only the PES's first packet may start it, and only the flags byte's random access
		// and discontinuity indicators may be set.
		if packet != first && packet[1] & 0x40 != 0 {
			return None;
		}
		let field = (packet[3] & 0x20 != 0).then(|| &packet[5..5 + usize::from(packet[4])]);
		let flag = field.and_then(|field| field.first().copied()).unwrap_or(0);
		if flag & !0xc0 != 0 {
			return None;
		}
		flags.push(flag);
		body.extend_from_slice(payload(packet));
	}
	if body.is_empty() {
		return None;
	}
	let mut out = Vec::with_capacity((packets.len() + 1) * TsPacket::SIZE);
	let mut offset = 0;
	for k in 0.. {
		let start = if k == 0 { first[1] & 0x40 } else { 0 };
		let flag = flags.get(k).copied().unwrap_or(0);
		let mut field = Vec::new();
		if k == 0 {
			field.push(flag | 0x10);
			field.extend_from_slice(&pcr_field(ticks));
		} else if flag != 0 {
			field.push(flag);
		}
		let room = PAYLOAD - if field.is_empty() { 0 } else { 1 + field.len() };
		let take = room.min(body.len() - offset);
		if take < room {
			// Stuffing fills the field out to the packet's end; a field of length 0 is the
			// single byte of stuffing.
			if field.is_empty() && take + 1 < PAYLOAD {
				field.push(0);
			}
			field.resize(PAYLOAD - 1 - take, 0xff);
		}
		out.extend_from_slice(&[first[0], first[1] & !0x40 | start, first[2]]);
		match field.is_empty() && take == PAYLOAD {
			true => out.push(0x10),
			false => {
				out.push(0x30);
				out.push(field.len() as u8);
				out.extend_from_slice(&field);
			}
		}
		out.extend_from_slice(&body[offset..offset + take]);
		offset += take;
		if offset == body.len() {
			break;
		}
	}
	Some(out)
}

/// One grid slot's worth of output.
pub(super) struct Slot {
	pub index: u128,
	/// The PCR that opens the slot, in system clock ticks.
	pub pcr: u64,
	/// The media packets, in the order they were muxed.
	pub packets: Vec<u8>,
	/// How many null packets pad the slot to the multiplex rate, its clock packet included.
	pub nulls: usize,
	/// Whether a keyframe's first packet rides in the slot.
	pub keyframe: bool,
	/// The PID of each access unit whose first packet rides in the slot.
	pub units: Vec<u16>,
	/// Exact positions of the media packets in a padded slot, counting the opening clock.
	positions: Option<Vec<usize>>,
	/// The padded slot opens with a packet of the clock's PID carrying its PCR, so it needs
	/// no clock packet of its own.
	pub clocked: bool,
	/// The PID its clock rides.
	pub clock: Option<u16>,
	/// How many units of program tables finish in the slot ([`Schedule::lead`]), which go
	/// out in the order they were pushed.
	pub tables: usize,
	/// How many units of program tables begin in the slot, each laying the tables as they
	/// stood ([`Schedule::set_psi`]).
	pub begun: usize,
}

impl Slot {
	/// The whole slot, opening with its `clock` packet unless it is [`Self::clocked`]. Padded
	/// slots use the exact positions planned against receiver buffers. Unpadded slots use
	/// smooth weighted round-robin: each PID (the clock packet counted on its own) and the null
	/// packets take positions in proportion to their count, as evenly as they can, every PID
	/// keeping its own order.
	///
	/// A PID's packets in a run would arrive at the multiplex's full rate and overflow its
	/// 512-byte transport buffer, which drains only at its Rx (ISO 13818-1 2.4.2.3): an audio
	/// frame's few packets muxed whole, or a video PID taking most of the slot. The program
	/// tables (PAT and the PMT on `pmt_pid`) go just ahead of the first packet muxed after
	/// them, so they still lead the keyframe they were written for and a reader knows each PID
	/// before its first packet. Without a multiplex rate nothing paces them against TBsys and
	/// Bsys: unpadded output makes no system-buffer claim.
	pub fn layout(&self, clock: &[u8], pmt_pid: u16, null: &[u8]) -> Vec<u8> {
		let packets = self.packets.as_chunks::<{ TsPacket::SIZE }>().0;
		if let Some(positions) = &self.positions {
			let count = usize::from(!self.clocked) + packets.len() + self.nulls;
			let mut out = Vec::with_capacity(count * TsPacket::SIZE);
			if !self.clocked {
				out.extend_from_slice(clock);
			}
			for (&position, packet) in positions.iter().zip(packets) {
				while out.len() / TsPacket::SIZE < position {
					out.extend_from_slice(null);
				}
				out.extend_from_slice(packet);
			}
			while out.len() / TsPacket::SIZE < count {
				out.extend_from_slice(null);
			}
			return out;
		}
		let mut tables = VecDeque::new();
		// Each PID's packets in mux order: the clock's PID first, then the rest as they appear.
		let mut lanes: Vec<(u16, VecDeque<usize>)> = vec![(pid(clock), VecDeque::new())];
		for (i, packet) in packets.iter().enumerate() {
			let pid = pid(packet);
			if pid == 0 || pid == pmt_pid {
				tables.push_back(i);
			} else if let Some((_, lane)) = lanes.iter_mut().find(|(p, _)| *p == pid) {
				lane.push_back(i);
			} else {
				lanes.push((pid, VecDeque::from([i])));
			}
		}

		// Every position, each lane (the nulls last) gains its weight and the one furthest
		// ahead takes it, paying back the total: over the slot each takes exactly its weight.
		// The clock takes the first.
		let mut weights: Vec<i64> = lanes
			.iter()
			.map(|(_, lane)| lane.len() as i64)
			.chain([self.nulls as i64])
			.collect();
		weights[0] += 1;
		let total: i64 = weights.iter().sum();
		let mut credit = weights.clone();
		credit[0] -= total;
		// Positions the round-robin has still to lay.
		let mut left = total - 1;

		let mut out = Vec::with_capacity((1 + packets.len() + self.nulls) * TsPacket::SIZE);
		out.extend_from_slice(clock);
		loop {
			let pick = (left > 0).then(|| {
				let mut pick = 0;
				for lane in 1..credit.len() {
					if credit[lane] + weights[lane] > credit[pick] + weights[pick] {
						pick = lane;
					}
				}
				pick
			});
			// The packet the round-robin lays next, `None` for a null.
			let next = pick.map(|pick| lanes.get(pick).map(|(_, lane)| lane[0]));
			// The next table, once the next packet was muxed after it.
			let table = tables
				.front()
				.copied()
				.filter(|&table| next.is_none_or(|next| next.is_some_and(|i| table < i)));
			if let Some(table) = table {
				tables.pop_front();
				out.extend_from_slice(&packets[table]);
				continue;
			}
			let Some(pick) = pick else {
				break;
			};
			for (credit, weight) in credit.iter_mut().zip(&weights) {
				*credit += weight;
			}
			credit[pick] -= total;
			left -= 1;
			let packet = match lanes.get_mut(pick) {
				Some((_, lane)) => {
					let i = lane
						.pop_front()
						.expect("a lane takes no more positions than its weight");
					&packets[i]
				}
				None => null,
			};
			out.extend_from_slice(packet);
		}
		out
	}
}

/// Packs muxed access units onto the PCR grid.
///
/// Each slot's packets go earliest deadline first across PIDs, each PID in its own order, and
/// each unit as soon as its PID's buffers in a receiver admit it. Padded output checks
/// transport occupancy and decoder removal at exact packet positions. A unit may go out as far
/// ahead of its due slot as its PID's decoder buffer reaches, within the `window`, which keeps
/// that far ahead of a heavy passage ([`ahead`]). A slot is laid once no unit still to be
/// pushed could go out in it, so a caller that pushes each unit that far ahead of its due slot
/// has the output trail the decode times by no more than the window.
///
/// With a multiplex rate each slot carries exactly what the rate allows, padded with null
/// packets, and a unit that is not complete by its exact deadline fails the export rather than
/// arrive late. Each slot's PCR is the time of its own first byte at the rate, so it matches
/// its byte position exactly, and how many packets a slot carries and the PCR it opens with
/// are functions of the slot alone, so two exporters of a broadcast lay a slot alike. Without a rate the stream is unpadded, its PCRs on the grid,
/// and a unit still incomplete in its due slot goes out whole there. A rate that turns up
/// mid-stream takes over from the next slot.
pub(super) struct Schedule {
	/// In push order, which is each PID's decode order.
	units: VecDeque<Unit>,
	/// Slots before its due slot a unit may go out at most: the export's delay.
	window: u128,
	/// The multiplex rate, in bits per second.
	rate: Option<u64>,
	/// The next slot to lay out.
	next: Option<u128>,
	/// The latest due slot pushed: the clock runs at least that far.
	horizon: Option<u128>,
	/// The due slot of the last unit pushed.
	last: Option<u128>,
	/// Each PID's buffers, from [`Self::set_buffer`].
	buffers: FxHashMap<u16, Buffer>,
	/// The PID every slot's clock packet rides on, from [`Self::set_clock`].
	clock: Option<u16>,
	/// The PMT's PID, which shares TBsys with the PAT, from [`Self::set_tables`].
	pmt: Option<u16>,
	/// Units sent into a decoder buffer: their PID, decode nanoseconds and bytes held.
	decoding: VecDeque<(u16, u128, usize)>,
	/// When each transport buffer's last admitted packet finishes leaving it, and when it
	/// last started filling from empty.
	transport: FxHashMap<u16, (i128, i128)>,
	/// When Bsys has removed every system byte admitted so far, and the Rsys it empties at
	/// whenever it holds one.
	system: Option<(i128, u128)>,
	/// When each video PID's MB finishes passing its last admitted payload on into EB.
	mb: FxHashMap<u16, i128>,
	/// Every source has ended, so a unit that misses its deadline is the tail going out late
	/// rather than an output that cannot keep up.
	ended: bool,
	/// The sequence number of the next unit pushed.
	seq: u64,
	/// The program tables as they stand, from [`Self::set_psi`].
	psi: Option<Psi>,
	/// Tables to go out ahead of the next unit pushed.
	lead: Option<Tables>,
	/// The timeline opens ([`Self::open`]) and its first tables are still to go out.
	opening: bool,
	/// The first slot a placed unit may go out in: [`OPENING`] slots after the one the first
	/// tables finish in, on a timeline that opens.
	clocked: u128,
	/// Packets a reader places on the program clock, held until the opening's PCR interval is
	/// out ([`Self::push_clocked`]).
	unclocked: Vec<u8>,
	/// The unit held packets last went out in, by `seq`: what is muxed after them waits until it
	/// has finished, as it rides a lane of its own.
	deferred: Option<u64>,
}

impl Schedule {
	pub fn new(window: Duration) -> Self {
		Self {
			units: VecDeque::new(),
			window: slot(window.as_nanos()),
			rate: None,
			next: None,
			horizon: None,
			last: None,
			buffers: FxHashMap::default(),
			clock: None,
			pmt: None,
			decoding: VecDeque::new(),
			transport: FxHashMap::default(),
			system: None,
			mb: FxHashMap::default(),
			ended: false,
			seq: 0,
			psi: None,
			lead: None,
			opening: false,
			clocked: 0,
			unclocked: Vec::new(),
			deferred: None,
		}
	}

	/// Open the timeline for a reader that has no program clock yet: no placed unit goes out
	/// until a PCR interval has followed the first tables ([`OPENING`]), so the reader can
	/// place every one. A timeline that continues a clock the reader already has needs none.
	pub fn open(&mut self) {
		self.opening = true;
	}

	/// Whether a unit pushed now could go out before the opening's PCR interval is out, or
	/// ahead of packets still held for it.
	pub fn clocking(&self) -> bool {
		self.opening
			|| !self.unclocked.is_empty()
			|| self.deferring()
			|| self.next.is_some_and(|next| next < self.clocked)
	}

	/// Whether the unit held packets last went out in has some still to send.
	fn deferring(&self) -> bool {
		self.deferred
			.is_some_and(|seq| self.units.iter().any(|unit| unit.seq == seq))
	}

	/// Hold `packets`, which a reader places on the program clock and which ride no unit of their
	/// own, until the opening's PCR interval is out: they then go out first, as soon as they can.
	pub fn push_clocked(&mut self, packets: Vec<u8>) {
		self.unclocked.extend(packets);
	}

	/// Whether every source has ended, so what is queued still goes out, late if it must.
	/// A source that resumes (a same-instance return) holds its deadlines again.
	pub fn set_ended(&mut self, ended: bool) {
		self.ended = ended;
	}

	/// Pad to `rate` bits per second from the next slot on, or stop padding.
	pub fn set_rate(&mut self, rate: Option<u64>) {
		self.rate = rate;
	}

	/// Hold the packets on `pid` to `buffer`.
	pub fn set_buffer(&mut self, pid: u16, buffer: Buffer) {
		let before = self.transport_buffer(pid);
		self.buffers.insert(pid, buffer);
		if self.transport_buffer(pid) != before {
			self.remap();
		}
	}

	/// Hold the program tables, the PAT and the PMT on `pmt_pid`, to the one TBsys they share.
	pub fn set_tables(&mut self, pmt_pid: u16) {
		let held = self.pmt == Some(pmt_pid)
			&& [0, pmt_pid]
				.iter()
				.all(|pid| self.buffers.get(pid) == Some(&Buffer::SYSTEM));
		self.buffers.insert(0, Buffer::SYSTEM);
		self.buffers.insert(pmt_pid, Buffer::SYSTEM);
		self.pmt = Some(pmt_pid);
		if !held {
			self.remap();
		}
	}

	/// The transport buffer `pid`'s packets enter: TBsys for the reserved system PIDs (PAT,
	/// CAT, TSDT, IPMP) and the PMT, as held to [`Buffer::SYSTEM`], else its own.
	fn transport_buffer(&self, pid: u16) -> u16 {
		let system = pid <= 3 || self.pmt == Some(pid);
		match system && self.buffers.get(&pid) == Some(&Buffer::SYSTEM) {
			true => 0,
			false => pid,
		}
	}

	/// The packets of a unit carried into TBsys, one run per PID.
	fn runs(&self, packets: &[u8]) -> Vec<Run> {
		let mut runs: Vec<Run> = Vec::new();
		for (k, packet) in packets.as_chunks::<{ TsPacket::SIZE }>().0.iter().enumerate() {
			let pid = pid(packet);
			if self.transport_buffer(pid) != 0 {
				continue;
			}
			let at = match runs.iter().position(|run| run.pid == pid) {
				Some(at) => at,
				None => {
					runs.push(Run {
						pid,
						table: Vec::new(),
						at: Vec::new(),
					});
					runs.len() - 1
				}
			};
			let run = &mut runs[at];
			// The continuity counter is numbered as the packet goes out, so it tells no copy apart.
			run.table.extend_from_slice(&packet[..3]);
			run.table.push(packet[3] & 0xf0);
			run.table.extend_from_slice(&packet[4..]);
			run.at.push(k);
		}
		runs
	}

	/// Find each queued unit's TBsys runs again once the PIDs held to TBsys change.
	fn remap(&mut self) {
		let mut units = std::mem::take(&mut self.units);
		for unit in &mut units {
			unit.runs = self.runs(&unit.packets);
		}
		self.units = units;
	}

	/// Count every slot's clock packet, which rides on `pid`, against that PID's transport
	/// buffer. Unpadded admission retains at least one media packet per slot.
	pub fn set_clock(&mut self, pid: u16) {
		self.clock = Some(pid);
	}

	/// Forget the buffers of `pid`, which carries no stream any more, so one that takes it
	/// over goes out under its own.
	pub fn forget(&mut self, pid: u16) {
		if self.buffers.remove(&pid).is_some() {
			self.remap();
		}
	}

	/// Whether a unit on `pid` is still to go out.
	pub fn holds(&self, pid: u16) -> bool {
		self.units.iter().any(|unit| unit.pid == pid && unit.tables.is_none())
	}

	/// Set the program tables as they stand. Every unit of them still to begin lays them, and
	/// takes over their clock PID, so a change reaches each copy not yet on its way.
	pub fn set_psi(&mut self, psi: Psi) {
		if self.psi.as_ref() != Some(&psi) {
			self.psi = Some(psi);
			self.retable();
		}
	}

	/// Lay the tables as they stand in each unit of them still to begin, the first flagging
	/// the break when they do ([`Psi::flagged`]).
	fn retable(&mut self) {
		let Some(psi) = &self.psi else {
			return;
		};
		let mut flagged = psi.flagged.as_ref();
		let mut units = std::mem::take(&mut self.units);
		for unit in units.iter_mut().filter(|unit| unit.sent == 0) {
			let Some(program) = &mut unit.tables else {
				continue;
			};
			program.clock = psi.clock;
			let packets = flagged.take().unwrap_or(&psi.packets);
			if unit.packets != *packets {
				unit.packets.clone_from(packets);
				unit.runs = self.runs(&unit.packets);
			}
		}
		self.units = units;
	}

	/// Send the program tables ahead of the next unit pushed, under its deadline. Program
	/// tables ride one lane, so each version goes out after the one before it, and a revision
	/// is one boundary: no unit of a stream they add goes out before them, and their clock PID
	/// and multiplex rate take over only once they have gone out.
	pub fn lead(&mut self, tables: Tables) {
		self.lead = Some(tables);
	}

	/// Queue an access unit of one frame.
	#[cfg(test)]
	pub fn push(&mut self, pid: u16, timing: Timing, packets: Vec<u8>, keyframe: bool) -> anyhow::Result<()> {
		self.push_frames(pid, timing, packets, keyframe, Vec::new())
	}

	/// Queue an access unit on `pid` (whole TS packets, the tables muxed ahead of it first)
	/// with its exact transport deadline and decoder removal time. A PES that packs several
	/// frames lists each one's decode time and elementary bytes, in decode order, which leave
	/// the decoder buffer one by one; `timing` is then the first frame's.
	pub fn push_frames(
		&mut self,
		pid: u16,
		timing: Timing,
		packets: Vec<u8>,
		keyframe: bool,
		frames: Vec<(u128, usize)>,
	) -> anyhow::Result<()> {
		self.queue(pid, timing, None, packets, keyframe, frames)
	}

	/// Queue a PES or section placed at `place` nanoseconds on the media timeline:
	/// its first byte goes out then, never earlier, with its TS header and `prefix` payload
	/// bytes (the section pointer field) just before it. It
	/// decodes at `decode`, or each of `frames` at its own time, as [`Self::push_frames`]
	/// lists them, which only the PID's decoder buffer reads: the unit misses no deadline.
	/// It yields to units due before it decodes, so a large one, such as a whole picture,
	/// shares the multiplex rather than holding it from placement until its last byte.
	pub fn push_placed(
		&mut self,
		pid: u16,
		(place, decode): (u128, u128),
		prefix: usize,
		packets: Vec<u8>,
		keyframe: bool,
		frames: Vec<(u128, usize)>,
	) -> anyhow::Result<()> {
		let timing = Timing {
			by: decode.max(place),
			decode,
		};
		self.queue(pid, timing, Some((place, prefix)), packets, keyframe, frames)
	}

	fn queue(
		&mut self,
		pid: u16,
		timing: Timing,
		place: Option<(u128, usize)>,
		packets: Vec<u8>,
		keyframe: bool,
		mut frames: Vec<(u128, usize)>,
	) -> anyhow::Result<()> {
		if let Some(tables) = self.lead.take() {
			let pmt = self.pmt.context("program tables without a PMT PID")?;
			let clock = self.psi.as_ref().context("program tables never set")?.clock;
			// Later tables wait too, so each version follows the one before it.
			let mut holds = tables.added;
			holds.extend([pid, pmt]);
			let program = Program {
				clock,
				rate: tables.rate,
				holds,
				leads: pid,
			};
			// They go out ahead of the first byte of a placed unit, which a decode time well past
			// its placement would otherwise let them hold the lane until, past later tables' due.
			let lead = Timing {
				by: place.map_or(timing.by, |(place, _)| place.min(timing.by)),
				..timing
			};
			// The unit they lead keeps its own start.
			let last = self.last;
			self.enqueue(pmt, lead, Vec::new(), Some(program));
			self.last = last;
			self.retable();
		}
		if packets.is_empty() {
			return Ok(());
		}
		if frames.len() < 2 {
			frames.clear();
		}
		// An ended source relaxes deadlines, never buffer sizes: such a unit never fits.
		if let Some(size) = self.buffers.get(&pid).and_then(|buffer| buffer.size) {
			let on_pid = || {
				packets
					.as_chunks::<{ TsPacket::SIZE }>()
					.0
					.iter()
					.filter(|packet| self::pid(*packet) == pid)
			};
			let bytes: usize = on_pid().map(|packet| payload(packet).len()).sum();
			// The PES header leaves the decoder buffer with the first frame after it.
			if let Some(first) = frames.first_mut() {
				first.1 += bytes - on_pid().map(|packet| elementary(packet).len()).sum::<usize>();
			}
			// A placed PES's frames leave the buffer one by one as it streams in, so only each
			// has to fit; any other unit arrives whole by its first frame's decode time.
			if let Some(largest) = frames.iter().map(|&(_, bytes)| bytes).max().filter(|_| place.is_some()) {
				anyhow::ensure!(
					largest <= size,
					"MPEG-TS PES on PID {pid} holds a {largest}-byte frame, exceeding its {size}-byte decoder buffer"
				);
			} else if bytes > size {
				// Only the error names the elementary size, so the PES headers are parsed here.
				let headers: usize = on_pid()
					.filter(|packet| packet[1] & 0x40 != 0)
					.map(|packet| payload(packet))
					.filter(|body| body.starts_with(&[0, 0, 1]) && body.len() >= 9)
					.map(|body| 9 + usize::from(body[8]))
					.sum();
				let elementary = bytes.saturating_sub(headers);
				anyhow::bail!(
					"MPEG-TS access unit on PID {pid} has {elementary} payload bytes ({bytes} bytes with PES headers), exceeding its {size}-byte decoder buffer"
				);
			}
		}
		// The deadline its last frame would have on its own.
		let last = timing.by
			+ frames
				.last()
				.map_or(0, |&(decode, _)| decode.saturating_sub(timing.decode));
		self.horizon = self.horizon.max(Some(last.div_ceil(PCR_INTERVAL.as_nanos())));
		let unit = self.enqueue(pid, timing, packets, None);
		unit.keyframe = keyframe;
		unit.place = place;
		unit.frames = frames;
		Ok(())
	}

	/// Push a unit of `packets` on `pid` due by `timing`, after every unit pushed before it.
	fn enqueue(&mut self, pid: u16, timing: Timing, packets: Vec<u8>, tables: Option<Program>) -> &mut Unit {
		let due = slot(timing.by);
		let start = self.last.map_or(due, |last| (last + 1).min(due));
		self.last = Some(due);
		let runs = self.runs(&packets);
		let seq = self.seq;
		self.seq += 1;
		self.units.push_back(Unit {
			seq,
			pid,
			due,
			timing,
			start,
			packets,
			sent: 0,
			keyframe: false,
			late: self.next.is_some_and(|next| due < next),
			place: None,
			held: 0,
			frames: Vec::new(),
			runs,
			tables,
		});
		self.units.back_mut().expect("the unit just pushed")
	}

	/// The PIDs the program tables still to go out hold back, each with the first of them that
	/// does: a unit on one, pushed after those tables, waits for them. `left` is how many
	/// packets each unit has still to send.
	fn holding(&self, left: impl Fn(usize) -> usize) -> FxHashMap<u16, u64> {
		let mut held = FxHashMap::default();
		for (i, unit) in self.units.iter().enumerate() {
			let Some(program) = unit.tables.as_ref().filter(|_| left(i) > 0) else {
				continue;
			};
			for &pid in &program.holds {
				held.entry(pid).or_insert(unit.seq);
			}
		}
		held
	}

	/// Put in force what the tables of unit `i` carry, as their last packet goes out in slot
	/// `index`, if it is one.
	fn enforce(&mut self, i: usize, index: u128) -> bool {
		let Some(program) = &self.units[i].tables else {
			return false;
		};
		self.clock = Some(program.clock);
		self.rate = program.rate;
		if std::mem::take(&mut self.opening) {
			self.clocked = self.clocked.max(index + OPENING);
		}
		true
	}

	/// Whether a unit, or packets held for the opening's clock, are still to go out.
	#[cfg(test)]
	pub fn queued(&self) -> bool {
		!self.units.is_empty() || !self.unclocked.is_empty()
	}

	/// Whether nothing is left to lay out: every unit sent, and the clock past its decode time.
	pub fn is_empty(&self) -> bool {
		// Packets held for the clock count once a slot has gone out, so slots run on to it.
		self.units.is_empty()
			&& (self.unclocked.is_empty() || self.next.is_none())
			&& self
				.horizon
				.is_none_or(|horizon| self.next.is_some_and(|next| next > horizon))
	}

	/// Drop everything queued and start the grid afresh.
	pub fn clear(&mut self) {
		self.units.clear();
		self.decoding.clear();
		self.transport.clear();
		self.system = None;
		self.mb.clear();
		self.next = None;
		self.horizon = None;
		self.last = None;
		self.ended = false;
		self.lead = None;
		// An opening still pending stays: its tables never went out, so the reader has no clock
		// for the next timeline to continue.
		self.clocked = 0;
		self.unclocked.clear();
		self.deferred = None;
	}

	/// The first slot `unit` may go out in: [`ahead`] of its due slot, and never before the
	/// slot after the unit pushed ahead of it needs. A packed PES is due by its first frame.
	///
	/// A placed PES goes no earlier than the slot whose bytes reach its placement: at a
	/// multiplex rate the one whose span holds it, where [`Self::padded`] times each packet,
	/// and otherwise the first that opens at or after it, as nothing times the packets
	/// within an unpadded slot. Slot `index` opens one slot behind its boundary ([`grid_pcr`]).
	/// On a timeline that [opens](Self::open), it waits for the opening's PCR interval too.
	fn release(&self, unit: &Unit) -> u128 {
		if let Some((place, _)) = unit.place {
			let slot = PCR_INTERVAL.as_nanos();
			let release = match self.rate {
				Some(_) => place / slot + 1,
				None => place.div_ceil(slot) + 1,
			};
			return release.max(self.clocked);
		}
		if unit.late {
			return 0;
		}
		let pid = unit.tables.as_ref().map_or(unit.pid, |program| program.leads);
		let ahead = ahead(self.buffers.get(&pid), self.window);
		unit.due.saturating_sub(ahead).min(unit.start)
	}

	/// The slot [`Self::next`] lays out next, with `known` as it would be given.
	fn index(&self, known: Option<u128>) -> Option<u128> {
		let first = self.units.iter().map(|unit| self.release(unit)).min();
		match (self.next, first) {
			(None, None) => None,
			(None, Some(first)) => Some(first),
			// Skip the empty slots of a long gap, keeping the second before what comes next.
			(Some(next), first) => {
				let mut target = first.or(known);
				// The opening's PCR interval runs slot by slot to its gate, so what it holds goes
				// out on a clock the reader has measured.
				if self.opening || !self.unclocked.is_empty() {
					target = target.map(|target| target.min(self.clocked));
				}
				Some(next.max(target.map_or(0, |target| target.saturating_sub(BACKFILL))))
			}
		}
	}

	/// The next slot to lay out, and the first slot a unit still to be pushed could go out in
	/// once every unit that may ride it is known: the `known` that settles it.
	pub fn upcoming(&self) -> Option<(u128, u128)> {
		let index = self.index(None)?;
		Some((index, index + 1))
	}

	/// Lay out the next slot, if it is settled.
	///
	/// `known` is the first slot a unit still to be pushed could go out in ([`Self::release`]),
	/// which settles every slot before it; `None` means nothing more is coming, so every
	/// queued unit goes out and the clock runs on to the last decode time.
	pub fn next(&mut self, known: Option<u128>) -> anyhow::Result<Option<Slot>> {
		if known.is_none() && self.is_empty() {
			return Ok(None);
		}
		let Some(index) = self.index(known) else {
			return Ok(None);
		};
		if known.is_some_and(|known| index >= known) {
			return Ok(None);
		}
		// The first tables finish in the first slot unless their buffers hold them back, and a
		// placed unit beside them there would go out ahead of its clock.
		if self.opening && self.next.is_none() {
			self.clocked = index + OPENING;
		}
		// Those held behind earlier ones wait for them to finish.
		if !self.opening && index >= self.clocked && !self.unclocked.is_empty() && !self.deferring() {
			let packets = std::mem::take(&mut self.unclocked);
			let by = (index + 1) * PCR_INTERVAL.as_nanos();
			// They follow no unit, so the next one pushed keeps its own start.
			let last = self.last;
			let unit = self.enqueue(pid(&packets), Timing { by, decode: by }, packets, None);
			unit.late = true;
			self.deferred = Some(unit.seq);
			self.last = last;
		}
		self.next = Some(index + 1);
		if let Some(rate) = self.rate {
			return self.padded(index, rate).map(Some);
		}

		let clock = self.clock;
		let mut take = self.admit(index, usize::MAX);
		// A placed PES is due only once released, which may come after its due slot. A unit
		// forced out takes the tables ahead of it that hold it back along.
		let mut forced: Vec<bool> = self
			.units
			.iter()
			.map(|unit| unit.due <= index && self.release(unit) <= index)
			.collect();
		for j in (0..self.units.len()).rev() {
			if !forced[j] {
				continue;
			}
			let pid = self.units[j].pid;
			for (unit, forced) in self.units.iter().zip(forced.iter_mut()).take(j) {
				if unit.tables.as_ref().is_some_and(|program| program.holds.contains(&pid)) {
					*forced = true;
				}
			}
		}
		for ((unit, take), forced) in self.units.iter_mut().zip(take.iter_mut()).zip(forced) {
			if forced && *take < unit.remaining() {
				if self.buffers.get(&unit.pid).is_some_and(|buffer| buffer.size.is_some()) {
					unit.held += unit.packets_on(unit.sent + *take..unit.count(), unit.pid) * PAYLOAD;
				}
				*take = unit.remaining();
			}
		}

		let mut packets = Vec::new();
		let mut keyframe = false;
		let mut units = Vec::new();
		let mut begun = 0;
		for (unit, take) in self.units.iter_mut().zip(take) {
			if take == 0 {
				continue;
			}
			if unit.sent == 0 {
				keyframe |= unit.keyframe;
				begun += usize::from(unit.tables.is_some());
				// Only the tables muxed ahead of a frame that wrote nothing are no access unit.
				if unit.tables.is_none() && unit.packets_on(0..unit.count(), unit.pid) > 0 {
					units.extend(std::iter::repeat_n(unit.pid, unit.frames.len().max(1)));
				}
			}
			let from = unit.sent * TsPacket::SIZE;
			packets.extend_from_slice(&unit.packets[from..from + take * TsPacket::SIZE]);
			unit.sent += take;
		}
		let tables = (0..self.units.len())
			.filter(|&i| self.units[i].remaining() == 0 && self.enforce(i, index))
			.count();
		for unit in self.units.iter().filter(|unit| unit.remaining() == 0) {
			self.decoding.extend(unit.leaving());
		}
		self.units.retain(|unit| unit.remaining() > 0);
		Ok(Some(Slot {
			index,
			pcr: grid_pcr(index) as u64,
			packets,
			nulls: 0,
			keyframe,
			units,
			positions: None,
			clocked: false,
			clock,
			tables,
			begun,
		}))
	}

	/// Plan the packets and null positions together, against the actual transport clock.
	/// Decoder removal and the delivery deadline stay exact even between PCR grid points.
	fn padded(&mut self, index: u128, rate: u64) -> anyhow::Result<Slot> {
		let first = packets_before(index, rate);
		let count = (packets_before(index + 1, rate) - first).max(1) as usize;
		let time = |position: usize| {
			((first + position as u128) * TsPacket::SIZE as u128 * 8 * 1_000_000_000 / u128::from(rate)) as i128
				- PCR_INTERVAL.as_nanos() as i128
		};
		let mut out = Slot {
			index,
			pcr: rate_pcr(index, rate) as u64,
			packets: Vec::new(),
			nulls: count - 1,
			keyframe: false,
			units: Vec::new(),
			positions: Some(Vec::new()),
			clocked: false,
			clock: self.clock,
			tables: 0,
			begun: 0,
		};
		let clock = self.clock.filter(|pid| self.buffers.contains_key(pid));
		let mut holding = self.holding(|i| self.units[i].remaining());
		// A new multiplex rate changes Rsys from this slot on, and Bsys keeps the bytes it holds.
		let rsys = system_rate(rate);
		if let Some((empty, before)) = self.system
			&& before != rsys
		{
			let left = (empty - time(0)).max(0) as u128;
			self.system = Some((time(0) + (left * before).div_ceil(rsys) as i128, rsys));
		}
		// How long Bsys takes at Rsys to remove a packet.
		let hold = (TsPacket::SIZE as u128 * 8 * 1_000_000_000).div_ceil(rsys) as i128;
		// A lower rate's first slot may begin up to a packet of its own before this one ends,
		// so Rsys may fall as far as its floor anywhere in this slot. Every system packet must
		// fit Bsys as it would be had Rsys fallen at the slot's start, which at or below 40
		// Mb/s, where Rsys is its floor, is Bsys itself.
		let floor = system_rate(0);
		let mut floored = self
			.system
			.map(|(empty, rsys)| time(0) + ((empty - time(0)).max(0) as u128 * rsys).div_ceil(floor) as i128);
		let floor_hold = (TsPacket::SIZE as u128 * 8 * 1_000_000_000).div_ceil(floor) as i128;
		let floor_room = (BSYS * 8 * 1_000_000_000 / floor) as i128;
		// The transport buffer every slot's PCR rides and how long it holds the packet carrying
		// it, which each later slot's PCR may add to a busy stretch without the chance to wait.
		let clock_buffer = clock.map(|pid| {
			let rate = u128::from(self.buffers[&pid].rate);
			let duration = (TsPacket::SIZE as u128 * 8 * 1_000_000_000).div_ceil(rate) as i128;
			(self.transport_buffer(pid), duration)
		});
		// One head per PID preserves decode order without walking all future units at every
		// transport position. Removal events keep occupancy work proportional to units.
		let mut lanes: Vec<VecDeque<usize>> = Vec::new();
		let mut lane_by_pid = FxHashMap::default();
		let mut occupancy: FxHashMap<u16, usize> = FxHashMap::default();
		let mut removals = Vec::new();
		self.decoding.retain(|&(_, decode, _)| decode as i128 > time(0));
		for &(pid, decode, held) in &self.decoding {
			*occupancy.entry(pid).or_default() += held;
			removals.push((decode, pid, None, held));
		}
		for (i, unit) in self.units.iter().enumerate() {
			let lane = *lane_by_pid.entry(unit.pid).or_insert_with(|| {
				lanes.push(VecDeque::new());
				lanes.len() - 1
			});
			lanes[lane].push_back(i);
			if unit.last_decode() as i128 > time(0) {
				*occupancy.entry(unit.pid).or_default() += unit.held;
				for (decode, bytes) in unit.frames().filter(|&(decode, _)| decode as i128 > time(0)) {
					removals.push((decode, unit.pid, Some(i), bytes));
				}
			}
		}
		removals.sort_by_key(|event| event.0);
		let mut removals = removals.into_iter().peekable();
		let mut ready: BinaryHeap<Reverse<(u128, usize)>> = lanes
			.iter()
			.enumerate()
			.filter_map(|(lane, units)| {
				let unit = &self.units[*units.front()?];
				(self.release(unit) <= index).then_some(Reverse((unit.timing.by, lane)))
			})
			.collect();
		let mut waiting: BinaryHeap<Reverse<(i128, usize)>> = BinaryHeap::new();
		let mut position = 0;
		while position < count {
			let now = time(position);
			while let Some((_, pid, unit, held)) = removals.next_if(|event| event.0 as i128 <= now) {
				let held = unit.map_or(held, |i| {
					let unit = &mut self.units[i];
					let take = held.min(unit.held);
					unit.held -= take;
					take
				});
				*occupancy.get_mut(&pid).unwrap() -= held;
			}
			while waiting.peek().is_some_and(|Reverse((at, _))| *at <= now) {
				let Reverse((_, lane)) = waiting.pop().unwrap();
				ready.push(Reverse((self.units[*lanes[lane].front().unwrap()].timing.by, lane)));
			}
			let mut emitted = false;
			// The slot's PCR opens it. Its PID's next packet carries it there when it can go
			// out at once, which spares the slot a clock packet; the other PIDs wait a position.
			let mut passed = Vec::new();
			while let Some(Reverse((by, lane))) = ready.pop() {
				let i = *lanes[lane].front().unwrap();
				let unit = &self.units[i];
				// First tables that finish past the first slot hold back what was released beside
				// them.
				if holding.get(&unit.pid).is_some_and(|&seq| seq < unit.seq) || self.release(unit) > index {
					passed.push(Reverse((by, lane)));
					continue;
				}
				let rest = &unit.packets[unit.sent * TsPacket::SIZE..];
				let opening = match position {
					0 => match clock.filter(|&clock| self::pid(rest) == clock) {
						Some(_) => with_pcr(rest, out.pcr),
						None => None,
					},
					_ => None,
				};
				if position == 0 && opening.is_none() {
					passed.push(Reverse((by, lane)));
					continue;
				}
				let packet = opening.as_deref().unwrap_or(rest);
				let packet = &packet[..TsPacket::SIZE];
				let pid = self::pid(packet);
				// The first PES or section byte waits, with its TS header and pointer just before it.
				if let Some((place, prefix)) = unit.place
					&& pid == unit.pid
					&& packet[1] & 0x40 != 0
				{
					let header = (TsPacket::SIZE - payload(packet).len() + prefix) as u128;
					let at = place as i128 - (header * 8 * 1_000_000_000 / u128::from(rate)) as i128;
					if now < at {
						waiting.push(Reverse((at, lane)));
						continue;
					}
				}
				let buffer = self.buffers.get(&pid);
				let duration = buffer.map_or(0, |buffer| {
					(TsPacket::SIZE as u128 * 8 * 1_000_000_000).div_ceil(u128::from(buffer.rate))
				});
				let tb = self.transport_buffer(pid);
				if tb == 0 && held_back(&self.units, i) {
					passed.push(Reverse((by, lane)));
					continue;
				}
				let (last, since) = self.transport.get(&tb).copied().unwrap_or((now, now));
				// A receiver reads time off the PCR, to a system clock tick, so the buffer counts as
				// empty only once its last packet has left a tick before this one arrives.
				let empty = last + TICK <= now;
				let since = if empty { now } else { since };
				let drained = last.max(now) + duration as i128;
				if let Some(buffer) = buffer {
					let capacity = buffer.transport as u128 * 8 * 1_000_000_000 / u128::from(buffer.rate);
					if drained - now > capacity as i128 {
						let ready = drained - capacity as i128;
						waiting.push(Reverse((ready, lane)));
						continue;
					}
					// The next slot's mandatory clock packet must fit even at a full buffer.
					if clock_buffer.is_some_and(|(clock, _)| clock == tb)
						&& drained > time(count) + capacity as i128 - duration as i128
					{
						continue;
					}
				}
				// Video's payload goes on from MB into EB at Rbx as it leaves the transport
				// buffer, its PES header discarded, so only payload counts against the leak. MB
				// runs up to BSmux ahead of the leak, 4 ms of the larger of Rbx and 2 Mb/s: the
				// room 2.14.3.1 gives it for the multiplex's other PIDs, so the leak never idles
				// while they take a few positions, and BSoh is left for the PES headers.
				let leak = buffer.and_then(|buffer| buffer.leak).map(|leak| {
					let free = last.max(now);
					let from = self.mb.get(&pid).copied().unwrap_or(free).max(free);
					let bits = elementary(packet).len() as u128 * 8 * 1_000_000_000;
					let leaked = from + bits.div_ceil(u128::from(leak)) as i128;
					let room = 4_000_000 * u128::from(leak.max(2_000_000)) / u128::from(leak);
					(leaked, room as i128)
				});
				if let Some((leaked, capacity)) = leak
					&& leaked - now > capacity
				{
					waiting.push(Reverse((leaked - capacity, lane)));
					continue;
				}
				// No byte leaves the transport buffer before it has arrived: below Rx, the
				// multiplex rate decides when the packet's last byte gets there.
				let arrived = drained.max(time(position + 1));
				let done = leak.map_or(arrived, |(leaked, _)| arrived.max(leaked));
				// TBsys passes a system packet whole into Bsys, each byte as it leaves (2.4.2.3),
				// and Bsys empties at Rsys whenever it holds one, so it holds the most as the
				// packet's last byte gets there. Past BSsys at the floor the packet waits for Bsys
				// to drain: from the time this gives, it fits whatever TBsys then holds. With the
				// clock on TBsys, the next slot's clock packet must fit after it, or it waits for
				// the next slot, as starting later in this one leaves more at its end.
				let bsys = (tb == 0).then(|| {
					let before = self.system.map_or(i128::MIN, |(empty, _)| empty);
					let empty = before.max(last.max(now)) + hold;
					let floor_before = floored.unwrap_or(i128::MIN);
					let floor_empty = floor_before.max(last.max(now)) + floor_hold;
					let clocked = clock_buffer.is_some_and(|(clock, _)| clock == 0)
						&& (floor_empty - time(count)).max(0) as u128 * floor
							> (BSYS - TsPacket::SIZE as u128) * 8 * 1_000_000_000;
					let ready = match floor_empty - arrived > floor_room {
						true => Some(floor_before + floor_hold - duration as i128 - floor_room),
						false => clocked.then_some(time(count)),
					};
					(empty, floor_empty, ready)
				});
				if let Some((_, _, Some(ready))) = bsys {
					waiting.push(Reverse((ready, lane)));
					continue;
				}
				// A transport buffer must empty at least once a second (2.4.2.3), so a PID kept
				// busy that long waits for it to. Every later slot's clock packet that arrives
				// before the buffer empties extends the stretch without the chance to wait.
				let mut busy = arrived;
				if let Some((_, duration)) = clock_buffer.filter(|&(clock, _)| clock == tb) {
					let mut next = index + 1;
					while busy - since <= TB_EMPTY.as_nanos() as i128 {
						let start = (packets_before(next, rate) - first) as usize;
						if busy + TICK <= time(start) {
							break;
						}
						busy = (busy.max(time(start)) + duration).max(time(start + 1));
						next += 1;
					}
				}
				if busy - since > TB_EMPTY.as_nanos() as i128 {
					waiting.push(Reverse((last + TICK, lane)));
					continue;
				}
				let bytes = if pid == unit.pid && buffer.is_some_and(|buffer| buffer.size.is_some()) {
					payload(packet).len()
				} else {
					0
				};
				if let Some(size) = buffer.and_then(|buffer| buffer.size)
					&& occupancy.get(&pid).copied().unwrap_or(0) + bytes > size
				{
					if let Some(event) = removals.peek() {
						let ready = event.0 as i128;
						waiting.push(Reverse((ready, lane)));
					}
					continue;
				}
				if !self.ended
					&& !unit.late && unit.place.is_none()
					&& pid == unit.pid
					&& done > unit.timing.decode as i128
				{
					return Err(self.missed(pid, rate));
				}
				self.transport.insert(tb, (arrived, since));
				if let Some((empty, floor_empty, _)) = bsys {
					self.system = Some((empty, rsys));
					floored = Some(floor_empty);
				}
				if let Some((leaked, _)) = leak {
					self.mb.insert(pid, leaked);
				}
				out.positions.as_mut().unwrap().push(position);
				out.packets.extend_from_slice(packet);
				// A packet carrying the PCR takes the clock packet's position, any other a null's.
				if let Some(rest) = opening {
					let unit = &mut self.units[i];
					unit.packets.truncate(unit.sent * TsPacket::SIZE);
					unit.packets.extend_from_slice(&rest);
					// The rest is all on this PID, so only a clock on a table PID rewrites a table.
					if tb == 0 {
						self.units[i].runs = self.runs(&self.units[i].packets);
					}
					out.clocked = true;
				} else {
					out.nulls -= 1;
				}
				let unit = &mut self.units[i];
				if unit.sent == 0 {
					out.keyframe |= unit.keyframe;
					out.begun += usize::from(unit.tables.is_some());
					if unit.tables.is_none() && unit.packets_on(0..unit.count(), unit.pid) > 0 {
						out.units
							.extend(std::iter::repeat_n(unit.pid, unit.frames.len().max(1)));
					}
				}
				unit.sent += 1;
				if unit.last_decode() as i128 > now {
					unit.held += bytes;
					*occupancy.entry(unit.pid).or_default() += bytes;
				}
				if unit.remaining() == 0 {
					lanes[lane].pop_front();
					if self.enforce(i, index) {
						out.tables += 1;
						holding = self.holding(|i| self.units[i].remaining());
					}
				}
				if let Some(&i) = lanes[lane].front() {
					let unit = &self.units[i];
					if self.release(unit) <= index {
						ready.push(Reverse((unit.timing.by, lane)));
					}
				}
				emitted = true;
				break;
			}
			ready.extend(passed);
			if position == 0 && !emitted {
				// A clock packet of its own opens the slot, and its PID's transport buffer
				// takes it first.
				if let Some((tb, duration)) = clock_buffer {
					let (free, since) = self.transport.entry(tb).or_insert((time(0), time(0)));
					if *free + TICK <= time(0) {
						*since = time(0);
					}
					let start = (*free).max(time(0));
					*free = (start + duration).max(time(1));
					// From TBsys it goes on into Bsys, as a table would.
					if tb == 0 {
						let before = self.system.map_or(i128::MIN, |(empty, _)| empty);
						self.system = Some((before.max(start) + hold, rsys));
						floored = Some(floored.unwrap_or(i128::MIN).max(start) + floor_hold);
					}
				}
				position = 1;
			} else if emitted {
				position += 1;
			} else if let Some(Reverse((next, _))) = waiting.peek() {
				// Nothing can use intervening null positions. Jump to the first packet at the
				// receiver event instead of scanning the multiplex rate's worth of padding.
				let absolute = (*next + PCR_INTERVAL.as_nanos() as i128).max(0) as u128;
				let packet = (absolute * u128::from(rate)).div_ceil(TsPacket::SIZE as u128 * 8 * 1_000_000_000);
				position = (packet.saturating_sub(first) as usize).max(position + 1);
			} else {
				break;
			}
		}
		let end = time(count);
		if !self.ended
			&& let Some(unit) = self.units.iter().find(|unit| {
				!unit.late && unit.place.is_none() && unit.timing.by as i128 <= end && unit.remaining() > 0
			}) {
			return Err(self.missed(unit.pid, rate));
		}
		for unit in self.units.iter().filter(|unit| unit.remaining() == 0) {
			self.decoding.extend(unit.leaving());
		}
		self.units.retain(|unit| unit.remaining() > 0);
		Ok(out)
	}

	/// The error for a unit on `pid` that misses its decode deadline at `rate`, naming what
	/// would carry it: the delay at which it goes as far ahead as its decoder buffer holds,
	/// where the window cut it short, else a faster multiplex.
	fn missed(&self, pid: u16, rate: u64) -> anyhow::Error {
		let reach = self
			.buffers
			.get(&pid)
			.filter(|buffer| buffer.size.is_some())
			.map(|buffer| slot(buffer.reach.as_nanos()))
			.filter(|&reach| reach > self.window);
		match reach {
			Some(reach) => anyhow::anyhow!(
				"MPEG-TS output missed a decode deadline on PID {pid} at {rate} b/s; raise the delay to {:?}, which sends it as far ahead as its decoder buffer holds, or the multiplex rate",
				PCR_INTERVAL * u32::try_from(reach).unwrap_or(u32::MAX)
			),
			None => anyhow::anyhow!(
				"MPEG-TS output missed a decode deadline on PID {pid} at {rate} b/s, sent as far ahead as its decoder buffer holds; raise the multiplex rate"
			),
		}
	}

	/// How many of each unit's packets slot `index` carries, at most `budget` in all.
	///
	/// Earliest deadline first across PIDs. A PID whose unit stops short (its buffers full,
	/// or not yet released) takes nothing more this slot, so each keeps its own order.
	fn admit(&mut self, index: u128, mut budget: usize) -> Vec<usize> {
		// Unpadded layout has no exact packet clock until the slot is assembled, so it
		// retains units until their decode instant is behind the opening grid point.
		let mut occupancy: FxHashMap<u16, usize> = FxHashMap::default();
		self.decoding
			.retain(|&(_, decode, _)| decode >= index.saturating_sub(1) * PCR_INTERVAL.as_nanos());
		for &(pid, _, held) in &self.decoding {
			*occupancy.entry(pid).or_default() += held;
		}
		for unit in &self.units {
			*occupancy.entry(unit.pid).or_default() += unit.held;
		}

		let mut order: Vec<usize> = (0..self.units.len()).collect();
		order.sort_by_key(|&i| self.units[i].due);
		let mut take = vec![0; self.units.len()];
		let mut carried: FxHashMap<u16, usize> = FxHashMap::default();
		let mut stopped: Vec<u16> = Vec::new();
		let mut holding = self.holding(|i| self.units[i].remaining());
		for i in order {
			let unit = &self.units[i];
			if budget == 0 {
				break;
			}
			if stopped.contains(&unit.pid) {
				continue;
			}
			let size = self.buffers.get(&unit.pid).and_then(|buffer| buffer.size);
			let mut n = 0;
			let held = holding.get(&unit.pid).is_some_and(|&seq| seq < unit.seq);
			if !held && self.release(unit) <= index {
				while n < unit.remaining() && budget > 0 {
					let pid = unit.pid_at(unit.sent + n);
					let cap = self
						.buffers
						.get(&pid)
						.map_or(usize::MAX, |buffer| buffer.per_slot(index, self.clock == Some(pid)));
					let count = carried.entry(pid).or_default();
					if *count >= cap {
						break;
					}
					if pid == unit.pid
						&& let Some(size) = size
					{
						let held = occupancy.entry(pid).or_default();
						if *held + PAYLOAD > size {
							break;
						}
						*held += PAYLOAD;
					}
					*count += 1;
					budget -= 1;
					n += 1;
				}
			}
			if n < unit.remaining() {
				stopped.push(unit.pid);
			} else if unit.tables.is_some() {
				take[i] = n;
				holding = self.holding(|j| self.units[j].remaining() - take[j]);
			}
			take[i] = n;
		}

		// What a sized PID took stays in its decoder buffer until the unit decodes.
		for (unit, &n) in self.units.iter_mut().zip(&take) {
			if self.buffers.get(&unit.pid).is_some_and(|buffer| buffer.size.is_some()) {
				unit.held += unit.packets_on(unit.sent..unit.sent + n, unit.pid) * PAYLOAD;
			}
		}
		take
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use super::*;

	/// A unit of `count` packets on PID `pid`.
	fn unit(pid: u8, count: usize) -> Vec<u8> {
		let mut packet = [0u8; TsPacket::SIZE];
		packet[0] = 0x47;
		packet[2] = pid;
		packet[3] = 0x10;
		packet.repeat(count)
	}

	fn timing(ms: u128) -> Timing {
		Timing {
			by: self::ms(ms),
			decode: self::ms(ms),
		}
	}

	fn ms(ms: u128) -> u128 {
		ms * 1_000_000
	}

	/// A PAT and a PMT on PID 100 of a packet each, the clock on PID 1.
	fn set_psi(schedule: &mut Schedule) {
		schedule.set_psi(Psi {
			packets: [unit(0, 1), unit(100, 1)].concat(),
			flagged: None,
			clock: 1,
		});
	}

	/// 40 packets per slot, the clock packet included.
	const RATE: u64 = 40 * PACKET;
	/// A decoder buffer's reach past every window here, so the window bounds how far ahead a
	/// unit goes.
	const REACH: Duration = Duration::from_secs(1);
	/// A video-like buffer: far more than a slot passes on, and a large decoder buffer.
	const VIDEO: Buffer = Buffer {
		rate: 100_000_000,
		leak: None,
		size: Some(1_000_000),
		transport: 512,
		reach: REACH,
	};

	/// Every slot until the schedule is empty: (index, packets per PID, nulls).
	fn drain(schedule: &mut Schedule) -> Vec<(u128, HashMap<u16, usize>, usize)> {
		let mut slots = Vec::new();
		while let Some(slot) = schedule.next(None).unwrap() {
			let mut per_pid = HashMap::new();
			for packet in slot.packets.as_chunks::<{ TsPacket::SIZE }>().0 {
				*per_pid.entry(pid(packet)).or_default() += 1;
			}
			slots.push((slot.index, per_pid, slot.nulls));
		}
		slots
	}

	/// Check actual wire positions against the receiver's transport buffer, clock included,
	/// and count the packets' worth of payload PID 1 sent.
	fn checked_transport(schedule: &mut Schedule, clock: u8) -> usize {
		let rate = schedule.rate.unwrap();
		let rx = schedule.buffers[&1].rate;
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let (mut fill, mut previous, mut sent) = (0.0f64, None::<f64>, 0);
		while let Some(slot) = schedule.next(None).unwrap() {
			sent += slot
				.packets
				.as_chunks::<{ TsPacket::SIZE }>()
				.0
				.iter()
				.filter(|packet| pid(*packet) == 1)
				.map(|packet| payload(packet).len())
				.sum::<usize>();
			for (position, packet) in slot.layout(&unit(clock, 1), 100, &null).chunks(188).enumerate() {
				let at = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / rate as f64;
				if let Some(previous) = previous {
					fill = (fill - (at - previous) * rx as f64 / 8.0).max(0.0);
				}
				previous = Some(at);
				if pid(packet) == 1 {
					fill += 188.0;
				}
				assert!(fill <= 512.001, "PID1 transport buffer holds {fill} bytes");
			}
		}
		sent / PAYLOAD
	}

	/// Every packet laid until the schedule is empty, opening each slot with `clock` unless it
	/// is clocked, with when its first byte goes out.
	fn wire(schedule: &mut Schedule, clock: &[u8]) -> Vec<(f64, Vec<u8>)> {
		let rate = schedule.rate.unwrap() as f64;
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let mut out = Vec::new();
		while let Some(slot) = schedule.next(None).unwrap() {
			for (position, packet) in slot.layout(clock, 100, &null).chunks(TsPacket::SIZE).enumerate() {
				let at = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / rate;
				out.push((at, packet.to_vec()));
			}
		}
		out
	}

	/// A placed picture five slots long, decoding a second out, shares the multiplex with a
	/// unit due while it is still going out, rather than holding the rate until its last byte.
	#[test]
	fn a_placed_pes_yields_to_units_due_before_it_decodes() {
		let mut schedule = Schedule::new(Duration::from_millis(25));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		let mut picture = unit(7, 200);
		picture[1] |= 0x40;
		schedule
			.push_placed(7, (ms(50), ms(1_000)), 0, picture, true, Vec::new())
			.unwrap();
		schedule.push(1, timing(100), unit(1, 30), true).unwrap();
		let slots = drain(&mut schedule);
		let last = slots
			.iter()
			.filter(|(_, pids, _)| pids.contains_key(&1))
			.map(|&(index, ..)| index)
			.max();
		assert!(
			last.is_some_and(|last| last <= slot(ms(100))),
			"the unit due at 100 ms goes out by slot {last:?}"
		);
		let sent: usize = slots.iter().filter_map(|(_, pids, _)| pids.get(&7)).sum();
		assert_eq!(sent, 200);
	}

	/// Tables leading a placed picture that decodes a second out go out by its placement, so
	/// the tables leading a unit due sooner, which ride the same lane after them, are not held
	/// past that unit's deadline.
	#[test]
	fn tables_leading_a_placed_pes_go_out_by_its_placement() {
		let mut schedule = Schedule::new(Duration::from_millis(25));
		schedule.set_rate(Some(RATE));
		schedule.set_tables(100);
		set_psi(&mut schedule);
		schedule.set_clock(1);
		schedule.set_buffer(1, VIDEO);
		let tables = || Tables {
			rate: Some(RATE),
			added: Vec::new(),
		};
		let mut picture = unit(7, 200);
		picture[1] |= 0x40;
		schedule.lead(tables());
		schedule
			.push_placed(7, (ms(50), ms(1_000)), 0, picture, true, Vec::new())
			.unwrap();
		schedule.lead(tables());
		schedule.push(1, timing(100), unit(1, 30), true).unwrap();
		let slots = drain(&mut schedule);
		let last = slots
			.iter()
			.filter(|(_, pids, _)| pids.contains_key(&1))
			.map(|&(index, ..)| index)
			.max();
		assert!(
			last.is_some_and(|last| last <= slot(ms(100))),
			"the unit due at 100 ms goes out by slot {last:?}"
		);
		let tables: usize = slots.iter().filter_map(|(_, pids, _)| pids.get(&100)).sum();
		assert_eq!(tables, 2);
	}

	/// A timeline that opens holds a placed unit until a PCR interval has followed its first
	/// tables, while the unit they lead goes out where it would have without the opening, padded
	/// or not.
	#[test]
	fn an_opening_holds_placed_units_for_a_pcr_interval() {
		for rate in [None, Some(RATE)] {
			let first = |open: bool| {
				let mut schedule = Schedule::new(Duration::ZERO);
				schedule.set_rate(rate);
				schedule.set_tables(100);
				set_psi(&mut schedule);
				schedule.set_clock(1);
				schedule.set_buffer(1, VIDEO);
				if open {
					schedule.open();
				}
				assert_eq!(schedule.clocking(), open);
				let mut section = unit(7, 1);
				section[1] |= 0x40;
				schedule.push_placed(7, (0, 0), 1, section, false, Vec::new()).unwrap();
				schedule.lead(Tables { rate, added: vec![1] });
				schedule.push(1, timing(10), unit(1, 3), true).unwrap();
				let slots = drain(&mut schedule);
				assert!(!schedule.clocking());
				let on = |pid: u16| {
					slots
						.iter()
						.find(|(_, pids, _)| pids.contains_key(&pid))
						.map(|&(index, ..)| index)
				};
				(on(100).unwrap(), on(1).unwrap(), on(7).unwrap())
			};
			let (tables, media, placed) = first(true);
			assert_eq!(placed, tables + OPENING, "{rate:?}");
			let (_, before, early) = first(false);
			assert_eq!(media, before, "{rate:?}: the media keeps its slot");
			assert!(early < placed, "{rate:?}: without the opening it goes in slot {early}");
		}
	}

	/// Packets held for the opening's clock go out once its PCR interval is, without holding
	/// back the unit they were muxed ahead of, padded or not.
	#[test]
	fn clocked_packets_follow_the_opening_without_the_media() {
		for rate in [None, Some(RATE)] {
			let mut schedule = Schedule::new(Duration::ZERO);
			schedule.set_rate(rate);
			schedule.set_tables(100);
			set_psi(&mut schedule);
			schedule.set_clock(1);
			schedule.set_buffer(1, VIDEO);
			schedule.set_buffer(0x11, Buffer::SYSTEM);
			schedule.open();
			schedule.push_clocked(unit(0x11, 1));
			schedule.lead(Tables { rate, added: vec![1] });
			schedule.push(1, timing(10), unit(1, 3), true).unwrap();
			assert!(schedule.clocking());
			let slots = drain(&mut schedule);
			assert!(!schedule.clocking());
			let on = |pid: u16| {
				slots
					.iter()
					.find(|(_, pids, _)| pids.contains_key(&pid))
					.map(|&(index, ..)| index)
			};
			assert_eq!((on(100), on(1)), (Some(0), Some(0)), "{rate:?}");
			assert_eq!(on(0x11), Some(OPENING), "{rate:?}");
		}
	}

	/// A gap after the first frames skips no slot of the opening's PCR interval, so packets held
	/// for it go out after consecutive PCRs rather than after a jump the reader cannot measure,
	/// padded or not. Until they do, SI muxed after them is held behind them.
	#[test]
	fn clocked_packets_follow_the_opening_across_a_gap() {
		for rate in [None, Some(RATE)] {
			let mut schedule = Schedule::new(Duration::ZERO);
			schedule.set_rate(rate);
			schedule.set_tables(100);
			set_psi(&mut schedule);
			schedule.set_clock(1);
			schedule.set_buffer(1, VIDEO);
			schedule.set_buffer(0x11, Buffer::SYSTEM);
			schedule.open();
			schedule.push_clocked(unit(0x11, 1));
			schedule.lead(Tables { rate, added: vec![1] });
			schedule.push(1, timing(10), unit(1, 1), true).unwrap();
			// The next frame is ten seconds out, far past a skip.
			let known = Some(400);
			let mut indexes = Vec::new();
			let mut held = None;
			while let Some(slot) = schedule.next(known).unwrap() {
				indexes.push(slot.index);
				if slot
					.packets
					.as_chunks::<{ TsPacket::SIZE }>()
					.0
					.iter()
					.any(|packet| pid(packet) == 0x11)
				{
					held = Some(slot.index);
					break;
				}
				// SI muxed now goes behind what is held for the same PID, never ahead of it.
				assert!(schedule.clocking(), "{rate:?}: after slot {}", slot.index);
			}
			assert_eq!(held, Some(OPENING), "{rate:?}: slots {indexes:?}");
			assert_eq!(indexes, (0..=OPENING).collect::<Vec<_>>(), "{rate:?}");
			assert!(!schedule.clocking());
		}
	}

	/// A PAT and a PMT on PID 100 of `count` packets, each packet marked `mark`.
	fn marked_tables(mark: u8, count: usize) -> Vec<u8> {
		let mut packets = [unit(0, 1), unit(100, count)].concat();
		for packet in packets.as_chunks_mut::<{ TsPacket::SIZE }>().0 {
			packet[4] = mark;
		}
		packets
	}

	/// The mark of each table packet laid in `slot`.
	fn table_marks(slot: &Slot) -> Vec<u8> {
		slot.packets
			.as_chunks::<{ TsPacket::SIZE }>()
			.0
			.iter()
			.filter(|packet| matches!(pid(*packet), 0 | 100))
			.map(|packet| packet[4])
			.collect()
	}

	/// Each copy of the tables lays them as they stand when it begins: a change reaches every
	/// copy still to begin, and never one already on its way, which goes out whole as it began.
	#[test]
	fn the_tables_lay_as_they_stand_when_they_begin() {
		let mut schedule = Schedule::new(Duration::from_secs(1));
		schedule.set_rate(Some(RATE));
		schedule.set_tables(100);
		schedule.set_clock(1);
		schedule.set_buffer(1, VIDEO);
		let psi = |mark: u8| Psi {
			packets: marked_tables(mark, 12),
			flagged: None,
			clock: 1,
		};
		schedule.set_psi(psi(1));
		for ms in [500, 600] {
			schedule.lead(Tables {
				rate: Some(RATE),
				added: Vec::new(),
			});
			schedule.push(1, timing(ms), unit(1, 1), true).unwrap();
		}
		schedule.set_psi(psi(2));
		let mut marks = Vec::new();
		let mut begun = 0;
		while let Some(slot) = schedule.next(None).unwrap() {
			marks.extend(table_marks(&slot));
			begun += slot.begun;
			// Too large for Bsys to take at once, the first copy spans slots.
			if begun == 1 && slot.tables == 0 {
				schedule.set_psi(psi(3));
			}
		}
		assert_eq!(begun, 2);
		assert_eq!(marks, [[2; 13], [3; 13]].concat());
	}

	/// Only the first copy of the tables to go out after a switch flags the break.
	#[test]
	fn only_the_first_tables_flag_the_break() {
		for rate in [None, Some(RATE)] {
			let mut schedule = Schedule::new(Duration::ZERO);
			schedule.set_rate(rate);
			schedule.set_tables(100);
			schedule.set_clock(1);
			schedule.set_buffer(1, VIDEO);
			schedule.set_psi(Psi {
				packets: marked_tables(1, 1),
				flagged: Some(marked_tables(2, 1)),
				clock: 1,
			});
			for ms in [10, 20] {
				schedule.lead(Tables {
					rate,
					added: Vec::new(),
				});
				schedule.push(1, timing(ms), unit(1, 1), true).unwrap();
			}
			let mut marks = Vec::new();
			while let Some(slot) = schedule.next(None).unwrap() {
				marks.extend(table_marks(&slot));
			}
			assert_eq!(marks, [2, 2, 1, 1], "{rate:?}");
		}
	}

	/// Packets held for the opening that take more than one slot to go out keep SI muxed after
	/// them held behind them until the last has, padded or not.
	#[test]
	fn clocked_packets_stay_ahead_until_they_finish() {
		for rate in [None, Some(RATE)] {
			let mut schedule = Schedule::new(Duration::ZERO);
			schedule.set_rate(rate);
			schedule.set_tables(100);
			set_psi(&mut schedule);
			schedule.set_clock(1);
			schedule.set_buffer(1, VIDEO);
			schedule.set_buffer(0x11, Buffer::SYSTEM);
			schedule.open();
			schedule.push_clocked(unit(0x11, 100));
			schedule.lead(Tables { rate, added: vec![1] });
			schedule.push(1, timing(10), unit(1, 3), true).unwrap();
			let mut sent = 0;
			let mut slots = Vec::new();
			while let Some(slot) = schedule.next(None).unwrap() {
				let packets = slot.packets.as_chunks::<{ TsPacket::SIZE }>().0;
				let held = packets.iter().filter(|&packet| pid(packet) == 0x11).count();
				if held > 0 {
					slots.push(slot.index);
				}
				sent += held;
				if sent < 100 {
					assert!(
						schedule.clocking(),
						"{rate:?}: {sent} of 100 out by slot {}",
						slot.index
					);
				}
			}
			assert_eq!(sent, 100, "{rate:?}");
			assert!(!schedule.clocking(), "{rate:?}");
			if rate.is_some() {
				assert!(slots.len() > 1, "the held packets span slots {slots:?}");
			}
		}
	}

	/// The pointer byte goes just before placement, so it cannot cost a section another packet.
	#[test]
	fn placed_first_byte_accounts_for_the_section_pointer() {
		for prefix in [0, 1] {
			let mut schedule = Schedule::new(Duration::from_millis(500));
			schedule.set_rate(Some(1_600_000));
			let mut packet = unit(7, 1);
			packet[1] |= 0x40;
			// A packet starts at 69 ms on this rate's lattice; each byte takes 5 us.
			let place = 69_000_000 + (4 + prefix as u128) * 5_000;
			schedule
				.push_placed(7, (place, place), prefix, packet, true, Vec::new())
				.unwrap();
			let wire = wire(&mut schedule, &unit(8, 1));
			let (start, _) = wire.iter().find(|(_, packet)| pid(packet) == 7).unwrap();
			let first = start * 1_000_000_000.0 + (4 + prefix) as f64 * 5_000.0;
			assert!(
				(first - place as f64).abs() < 0.5,
				"prefix {prefix}: first byte at {first} ns, expected {place} ns"
			);
		}
	}

	/// The peak fills in bytes of a transport buffer fed by `pids` on the wire at `rate` and
	/// drained at `rx` b/s, and of the main buffer it passes their bytes on into, drained at
	/// `out` b/s whenever it holds one. Each packet arrives over its time on the wire, and a
	/// byte leaves the transport buffer once those ahead of it have, never before it arrives.
	fn peaks(wire: &[(f64, Vec<u8>)], rate: u64, pids: &[u16], rx: f64, out: f64) -> (f64, f64) {
		let duration = 1504.0 / rate as f64;
		let (mut leave, mut empty) = (f64::MIN, f64::MIN);
		let (mut tb, mut main) = (0.0f64, 0.0f64);
		for (start, _) in wire.iter().filter(|(_, packet)| pids.contains(&pid(packet))) {
			let free = leave.max(*start);
			leave = (start + duration).max(free + 1504.0 / rx);
			tb = tb.max((leave - start - duration) * rx / 8.0);
			// The main buffer takes the bytes faster than it drains, so it peaks as the last leaves.
			empty = empty.max(free) + 1504.0 / out;
			main = main.max((empty - leave) * out / 8.0);
		}
		(tb, main)
	}

	/// TBsys and Bsys peaks in bytes: PIDs 0-3 and the PMT on PID 100, at Rxsys and Rsys.
	fn system_peaks(wire: &[(f64, Vec<u8>)], rate: u64) -> (f64, f64) {
		peaks(wire, rate, &[0, 1, 2, 3, 100], 1_000_000.0, system_rate(rate) as f64)
	}

	/// Distinct tune-in tables keep their copies, in the order they were muxed, and still fit
	/// TBsys and Bsys, which the PAT and PMT share.
	#[test]
	fn system_packets_are_paced_with_media_admission() {
		for pids in [3u8, 4, 8] {
			for count in [10, 100] {
				for rate in [25_000_000, 50_000_000, 100_000_000] {
					let mut schedule = Schedule::new(Duration::from_millis(200));
					schedule.set_rate(Some(rate));
					schedule.set_clock(0x11);
					schedule.set_tables(100);
					for pid in 0x11..=0x10 + pids {
						schedule.set_buffer(u16::from(pid), VIDEO);
						let (mut pat, mut pmt) = (unit(0, 1), unit(100, 1));
						(pat[4], pmt[4]) = (pid, pid);
						let packets = [pat, pmt, unit(pid, count)].concat();
						schedule.push(u16::from(pid), timing(1_000), packets, true).unwrap();
					}
					let wire = wire(&mut schedule, &unit(0x11, 1));
					let (tbsys, bsys) = system_peaks(&wire, rate);
					let at = format!("{pids} PIDs x {count} packets at {rate}");
					assert!(tbsys <= 512.001, "{at}: TBsys peak {tbsys:.0} B");
					assert!(bsys <= 1536.001, "{at}: Bsys peak {bsys:.0} B");
					for table in [0, 100] {
						let copies: Vec<u8> = wire
							.iter()
							.filter(|(_, packet)| pid(packet) == table)
							.map(|(_, packet)| packet[4])
							.collect();
						assert_eq!(copies, (0x11..=0x10 + pids).collect::<Vec<_>>(), "{at}: PID {table}");
					}
				}
			}
		}
	}

	/// Sections on a reserved system PID share TBsys and Bsys with the PAT and PMT.
	#[test]
	fn reserved_system_pids_share_tbsys_with_the_tables() {
		let rate = 25_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(rate));
		schedule.set_clock(7);
		schedule.set_buffer(7, VIDEO);
		schedule.set_tables(100);
		schedule.set_buffer(1, Buffer::SYSTEM);
		// A CAT muxed with the tables ahead of a keyframe, as the export writes them, then two
		// PAT and PMT pairs: ten packets, more than Bsys holds.
		let pair = [unit(0, 1), unit(100, 1)].concat();
		let packets = [unit(0, 1), unit(100, 2), unit(1, 3), pair.clone(), pair, unit(7, 1)].concat();
		schedule.push(7, timing(1_000), packets, true).unwrap();
		let wire = wire(&mut schedule, &unit(7, 1));
		let (tbsys, bsys) = system_peaks(&wire, rate);
		assert!(tbsys <= 512.001, "TBsys peak {tbsys:.0} B");
		assert!(bsys <= 1536.001, "Bsys peak {bsys:.0} B");
		let system: Vec<u16> = wire
			.iter()
			.map(|(_, packet)| pid(packet))
			.filter(|&pid| pid != 0x1fff && pid != 7)
			.collect();
		assert_eq!(system, [0, 100, 100, 1, 1, 1, 0, 100, 0, 100]);
	}

	/// A run of tables that TBsys alone would pass in one slot overflows Bsys, which drains
	/// at only 80 kb/s at 25 Mb/s, so it spreads over several slots, every copy kept in order.
	#[test]
	fn a_burst_that_fits_tbsys_is_spread_for_bsys() {
		let rate = 25_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(500));
		schedule.set_rate(Some(rate));
		schedule.set_clock(7);
		schedule.set_buffer(7, VIDEO);
		schedule.set_tables(100);
		let mut packets = Vec::new();
		for copy in 0..8 {
			let (mut pat, mut pmt) = (unit(0, 1), unit(100, 1));
			(pat[4], pmt[4]) = (copy, copy);
			packets.extend([pat, pmt].concat());
		}
		packets.extend(unit(7, 1));
		schedule.push(7, timing(1_000), packets, true).unwrap();
		let wire = wire(&mut schedule, &unit(7, 1));
		let (tbsys, bsys) = system_peaks(&wire, rate);
		assert!(tbsys <= 512.001, "TBsys peak {tbsys:.0} B");
		assert!(bsys <= 1536.001, "Bsys peak {bsys:.0} B");
		let tables: Vec<(f64, u8)> = wire
			.iter()
			.filter(|(_, packet)| [0, 100].contains(&pid(packet)))
			.map(|(at, packet)| (*at, packet[4]))
			.collect();
		let copies: Vec<u8> = tables.iter().map(|&(_, copy)| copy).collect();
		assert_eq!(copies, (0..8).flat_map(|copy| [copy, copy]).collect::<Vec<_>>());
		// The sixteen packets take 24 ms through TBsys; Bsys needs 1,472 B drained at 10 kB/s.
		let span = tables.last().unwrap().0 - tables[0].0;
		assert!(span > 0.140, "the run spans {span:.3} s");
	}

	/// Bsys keeps the bytes it holds when the multiplex rate falls and Rsys with it, so tables
	/// still to go wait for the slower drain rather than overflow it, and a table still reaching
	/// Bsys as the rate falls already fits at the slower drain. The rate falls after each slot
	/// in turn, between rates whose packet lattices start the next slot up to a packet early.
	#[test]
	fn bsys_keeps_its_bytes_across_a_rate_fall() {
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		for (from, to) in [
			(100_000_000, 25_000_000),
			(95_250_000, 25_000_000),
			(95_250_000, 2_000_000),
			(60_500_000, 50_000_000),
		] {
			bsys_across_falls(&null, from, to);
		}
	}

	fn bsys_across_falls(null: &[u8], from: u64, to: u64) {
		let mut full = false;
		for fall in 0.. {
			let mut schedule = Schedule::new(Duration::from_millis(500));
			schedule.set_rate(Some(from));
			schedule.set_clock(7);
			schedule.set_buffer(7, VIDEO);
			schedule.set_tables(100);
			let mut packets = Vec::new();
			for copy in 0..12 {
				let (mut pat, mut pmt) = (unit(0, 1), unit(100, 1));
				(pat[4], pmt[4]) = (copy, copy);
				packets.extend([pat, pmt].concat());
			}
			packets.extend(unit(7, 1));
			schedule.push(7, timing(1_000), packets, true).unwrap();
			// Bsys in bytes at `at`, as each system packet's last byte reaches it. Each slot's
			// Rsys drains it from the slot's start, where the receiver reads the new rate off
			// the PCR.
			let (mut leave, mut at, mut level, mut drain, mut peak) = (f64::MIN, f64::MIN, 0.0f64, 0.0f64, 0.0f64);
			let mut slots = 0;
			while let Some(slot) = schedule.next(None).unwrap() {
				let rate = schedule.rate.unwrap();
				let rsys = system_rate(rate) as f64;
				let begin = slot.pcr as f64 / SYSTEM_CLOCK as f64;
				if at < begin {
					level = (level - (begin - at) * drain / 8.0).max(0.0);
				} else {
					level += (at - begin) * (drain - rsys) / 8.0;
					peak = peak.max(level);
				}
				(at, drain) = (at.max(begin), rsys);
				for (position, packet) in slot.layout(&unit(7, 1), 100, null).chunks(TsPacket::SIZE).enumerate() {
					if ![0, 100].contains(&pid(packet)) {
						continue;
					}
					let start = begin + position as f64 * 1504.0 / rate as f64;
					let free = leave.max(start);
					leave = (start + 1504.0 / rate as f64).max(free + 1504.0 / 1_000_000.0);
					level = (level - (free - at).max(0.0) * rsys / 8.0).max(0.0);
					level += 188.0 - (leave - free) * rsys / 8.0;
					peak = peak.max(level);
					at = leave;
				}
				if slots == fall {
					full |= level > 1_000.0;
					schedule.set_rate(Some(to));
				}
				slots += 1;
			}
			assert!(
				peak <= 1536.001,
				"{from} to {to} b/s after slot {fall}: Bsys peak {peak:.2} B"
			);
			if fall >= slots {
				break;
			}
		}
		assert!(full, "Bsys never held 1,000 B at a fall from {from} b/s");
	}

	/// A clock on the PMT's PID rides TBsys and goes on into Bsys, so the tables leave room in
	/// both for every slot's clock packet.
	#[test]
	fn a_clock_on_the_pmt_pid_counts_in_tbsys_and_bsys() {
		let rate = 100_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(500));
		schedule.set_rate(Some(rate));
		schedule.set_clock(100);
		schedule.set_buffer(7, VIDEO);
		schedule.set_tables(100);
		let mut packets = Vec::new();
		for copy in 0..12 {
			let (mut pat, mut pmt) = (unit(0, 1), unit(100, 1));
			(pat[4], pmt[4]) = (copy, copy);
			packets.extend([pat, pmt].concat());
		}
		packets.extend(unit(7, 1));
		schedule.push(7, timing(1_000), packets, true).unwrap();
		let wire = wire(&mut schedule, &unit(100, 1));
		let (tbsys, bsys) = system_peaks(&wire, rate);
		assert!(tbsys <= 512.001, "TBsys peak {tbsys:.0} B");
		assert!(bsys <= 1536.001, "Bsys peak {bsys:.0} B");
	}

	/// A changed table never overtakes the one muxed ahead of it on another PID's unit, even
	/// ahead of a unit due first, so a changed PMT version follows the one it replaces.
	#[test]
	fn a_changed_pmt_follows_the_version_it_replaces() {
		let rate = 25_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(rate));
		schedule.set_clock(7);
		schedule.set_tables(100);
		for pid in [7, 8] {
			schedule.set_buffer(pid, VIDEO);
		}
		for (pid, version, by) in [(8, 1, 1_100), (7, 2, 1_050)] {
			let (mut pat, mut pmt) = (unit(0, 1), unit(100, 1));
			(pat[4], pmt[4]) = (version, version);
			let packets = [pat, pmt, unit(pid, 4)].concat();
			schedule.push(pid.into(), timing(by), packets, true).unwrap();
		}
		let wire = wire(&mut schedule, &unit(7, 1));
		let versions: Vec<(u16, u8)> = wire
			.iter()
			.filter(|(_, packet)| [0, 100].contains(&pid(packet)))
			.map(|(_, packet)| (pid(packet), packet[4]))
			.collect();
		assert_eq!(versions, [(0, 1), (100, 1), (0, 2), (100, 2)]);
	}

	/// Units queued before their PIDs are held to TBsys keep their tables in order once they are.
	#[test]
	fn tables_queued_before_they_are_held_keep_their_order() {
		let rate = 25_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(rate));
		schedule.set_clock(7);
		for (pid, version, by) in [(8, 1, 1_100), (7, 2, 1_050)] {
			let (mut pat, mut pmt) = (unit(0, 1), unit(100, 1));
			(pat[4], pmt[4]) = (version, version);
			let packets = [pat, pmt, unit(pid, 4)].concat();
			schedule.push(pid.into(), timing(by), packets, true).unwrap();
		}
		for pid in [7, 8] {
			schedule.set_buffer(pid, VIDEO);
		}
		schedule.set_tables(100);
		let versions: Vec<(u16, u8)> = wire(&mut schedule, &unit(7, 1))
			.iter()
			.filter(|(_, packet)| [0, 100].contains(&pid(packet)))
			.map(|(_, packet)| (pid(packet), packet[4]))
			.collect();
		assert_eq!(versions, [(0, 1), (100, 1), (0, 2), (100, 2)]);
	}

	/// The same tables muxed ahead of a unit released later hold back no unit due sooner: an
	/// audio frame's copy waits for its frame, while a keyframe's goes ahead with the keyframe.
	#[test]
	fn a_copy_of_the_same_tables_holds_back_no_unit_due_sooner() {
		let rate = 25_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(rate));
		schedule.set_tables(100);
		for pid in [7, 8] {
			schedule.set_buffer(pid, VIDEO);
		}
		for (pid, by) in [(8, 1_100), (7, 1_050)] {
			let packets = [unit(0, 1), unit(100, 1), unit(pid, 4)].concat();
			schedule.push(pid.into(), timing(by), packets, true).unwrap();
		}
		let order: Vec<u16> = wire(&mut schedule, &unit(9, 1))
			.iter()
			.map(|(_, packet)| pid(packet))
			.filter(|&pid| pid != 9 && pid != 0x1fff)
			.collect();
		assert_eq!(order, [0, 100, 7, 7, 7, 7, 0, 100, 8, 8, 8, 8]);
	}

	/// Copies of a PMT spanning four packets never interleave, so a receiver reassembles each
	/// section whole: a unit due sooner, queued while another's copies are going out at Bsys's
	/// rate, sends its own once they have gone.
	#[test]
	fn copies_of_a_multi_packet_table_never_interleave() {
		let rate = 25_000_000;
		let mut schedule = Schedule::new(Duration::from_secs(1));
		schedule.set_rate(Some(rate));
		schedule.set_tables(100);
		for pid in [7, 8] {
			schedule.set_buffer(pid, VIDEO);
		}
		let mut pmt = unit(100, 4);
		for (part, packet) in pmt.chunks_mut(TsPacket::SIZE).enumerate() {
			packet[4] = part as u8;
		}
		// Three copies, twelve packets, more than Bsys holds, so they go out over several slots.
		let tables = pmt.repeat(3);
		schedule
			.push(8, timing(2_000), [tables.clone(), unit(8, 4)].concat(), true)
			.unwrap();
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let (mut parts, mut queued) = (Vec::new(), false);
		while let Some(slot) = schedule.next(None).unwrap() {
			let laid = slot.layout(&unit(9, 1), 100, &null);
			parts.extend(
				laid.chunks(TsPacket::SIZE)
					.filter(|packet| pid(packet) == 100)
					.map(|packet| packet[4]),
			);
			if !queued && !parts.is_empty() {
				schedule
					.push(7, timing(1_950), [tables.clone(), unit(7, 4)].concat(), true)
					.unwrap();
				queued = true;
			}
		}
		assert!(queued);
		assert_eq!(parts, [0, 1, 2, 3].repeat(6));
	}

	/// Where Bsys cannot pass a unit's tables before it is due, the export fails rather than
	/// send it late or drop a table.
	#[test]
	fn tables_bsys_cannot_pass_in_time_fail_the_export() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(25_000_000));
		schedule.set_clock(7);
		schedule.set_buffer(7, VIDEO);
		schedule.set_tables(100);
		let packets = [unit(0, 8), unit(100, 8), unit(7, 1)].concat();
		schedule.push(7, timing(1_000), packets, true).unwrap();
		let failed = loop {
			match schedule.next(None) {
				Ok(Some(_)) => continue,
				Ok(None) => break None,
				Err(err) => break Some(err.to_string()),
			}
		};
		assert!(
			failed
				.as_deref()
				.is_some_and(|err| err.contains("missed a decode deadline")),
			"{failed:?}"
		);
	}

	/// A teletext burst at a high multiplex rate keeps within its 480-byte transport buffer,
	/// measured as the receiver sees it: each packet arriving over its time on the wire.
	#[test]
	fn teletext_keeps_within_its_smaller_transport_buffer() {
		let rate = 50_000_000;
		let teletext = Buffer {
			rate: 6_750_000,
			leak: None,
			size: None,
			transport: 480,
			reach: Duration::ZERO,
		};
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(rate));
		schedule.set_clock(7);
		schedule.set_buffer(7, VIDEO);
		schedule.set_buffer(9, teletext);
		schedule.push(9, timing(1_000), unit(9, 40), false).unwrap();
		let wire = 1504.0 / rate as f64;
		let drain = 1504.0 / teletext.rate as f64;
		let (mut leave, mut peak, mut sent) = (f64::MIN, 0.0f64, 0);
		while let Some(slot) = schedule.next(None).unwrap() {
			for (position, packet) in slot.layout(&unit(7, 1), 100, &[0xff; 188]).chunks(188).enumerate() {
				if pid(packet) != 9 {
					continue;
				}
				let start = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * wire;
				leave = (start + wire).max(leave.max(start) + drain);
				peak = peak.max((leave - start - wire) * teletext.rate as f64 / 8.0);
				sent += 1;
			}
		}
		assert_eq!(sent, 40);
		assert!(peak <= 480.5, "teletext transport buffer holds {peak:.1} bytes");
	}

	/// A PID with more than a second's backlog at its Rx still lets its transport buffer
	/// empty once a second, as the receiver sees it, including when the slots' mandatory
	/// clock packets ride the same PID.
	#[test]
	fn a_busy_transport_buffer_empties_every_second() {
		let rate = 10_000_000;
		for (rx, count, clock) in [(1_000_000, 2_000, 7), (150_000, 150, 9), (120_000, 150, 9)] {
			let slow = Buffer {
				rate: rx,
				leak: None,
				size: Some(1_000_000),
				transport: 512,
				reach: Duration::from_secs(4),
			};
			let mut schedule = Schedule::new(Duration::from_secs(4));
			schedule.set_rate(Some(rate));
			schedule.set_clock(clock);
			schedule.set_buffer(7, VIDEO);
			schedule.set_buffer(9, slow);
			// More than a second of packets at the PID's Rx.
			schedule.push(9, timing(5_000), unit(9, count), false).unwrap();
			let wire = 1504.0 / rate as f64;
			let drain = 1504.0 / rx as f64;
			let (mut leave, mut since, mut longest, mut sent) = (f64::MIN, 0.0, 0.0f64, 0);
			while let Some(slot) = schedule.next(None).unwrap() {
				let laid = slot.layout(&unit(clock as u8, 1), 100, &[0xff; 188]);
				for (position, packet) in laid.chunks(188).enumerate() {
					if pid(packet) != 9 {
						continue;
					}
					let start = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * wire;
					if leave <= start {
						since = start;
					}
					leave = (start + wire).max(leave.max(start) + drain);
					longest = longest.max(leave - since);
					sent += 1;
				}
			}
			assert!(sent >= count, "{rx}: sent {sent}");
			assert!(
				longest <= 1.0,
				"{rx}: the transport buffer stayed busy for {longest:.4} s"
			);
		}
	}

	/// A unit goes out a window ahead of its due slot, as soon as it is released.
	#[test]
	fn a_unit_goes_out_as_soon_as_it_is_released() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.push(1, timing(1_000), unit(1, 3), true).unwrap();
		let slots = drain(&mut schedule);
		assert_eq!(slots[0].0, 36, "a window ahead of its due slot");
		assert_eq!(slots[0].1[&1], 3);
		assert_eq!(slots[0].2, 36, "padded to the rate");
		let clock: Vec<u128> = slots.iter().map(|slot| slot.0).collect();
		assert_eq!(clock, [36, 37, 38, 39, 40], "the clock runs on to its decode time");
	}

	/// However long the window, a unit goes out no further ahead of its due slot than its
	/// decoder buffer reaches.
	#[test]
	fn a_unit_goes_out_no_further_ahead_than_its_buffer_reaches() {
		let mut schedule = Schedule::new(Duration::from_millis(500));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(
			1,
			Buffer {
				reach: Duration::from_millis(100),
				..VIDEO
			},
		);
		schedule.push(1, timing(1_000), unit(1, 3), true).unwrap();
		assert_eq!(
			drain(&mut schedule)[0].0,
			36,
			"the buffer's reach ahead of its due slot"
		);
	}

	/// A burst bigger than a slot fills the slots from its release on.
	#[test]
	fn a_burst_fills_the_slots_from_its_release() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.push(1, timing(1_000), unit(1, 100), true).unwrap();
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.map(|(index, per_pid, _)| (index, per_pid.get(&1).copied().unwrap_or(0)))
			.collect();
		assert_eq!(sent, [(36, 39), (37, 39), (38, 22), (39, 0), (40, 0)]);
	}

	/// A burst the window cannot carry by its due slot fails the export, naming the delay that
	/// sends it as far ahead as its decoder buffer holds. Once the buffer rather than the
	/// window stops it, only a faster multiplex helps.
	#[test]
	fn a_burst_too_big_for_the_window_fails() {
		for (window, reach, fix) in [
			(50, 1_000, "raise the delay to 1s"),
			(1_000, 50, "raise the multiplex rate"),
		] {
			let mut schedule = Schedule::new(Duration::from_millis(window));
			schedule.set_rate(Some(RATE));
			schedule.set_buffer(
				1,
				Buffer {
					reach: Duration::from_millis(reach),
					..VIDEO
				},
			);
			schedule.push(1, timing(1_000), unit(1, 200), true).unwrap();
			let err = std::iter::from_fn(|| schedule.next(None).transpose()).find_map(Result::err);
			let err = err.expect("a burst past the window must fail the export").to_string();
			assert!(err.contains(fix), "window {window} ms, reach {reach} ms: {err}");
		}
	}

	/// Once every source has ended, a unit the window cannot carry by its due slot is the
	/// tail going out late, not a failure.
	#[test]
	fn a_burst_too_big_for_the_window_goes_out_late_at_the_end() {
		let mut schedule = Schedule::new(Duration::from_millis(50));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.push(1, timing(1_000), unit(1, 200), true).unwrap();
		schedule.set_ended(true);
		let sent: usize = drain(&mut schedule)
			.into_iter()
			.map(|(_, per_pid, _)| per_pid.get(&1).copied().unwrap_or(0))
			.sum();
		assert_eq!(sent, 200, "the whole unit goes out");
	}

	/// A unit bigger than its decoder buffer fails even once every source has ended: an ended
	/// source relaxes deadlines, not buffer sizes.
	#[test]
	fn an_oversized_last_unit_fails_after_the_end() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		// Room for ten packets.
		let small = Buffer {
			rate: 100_000_000,
			leak: None,
			size: Some(10 * PAYLOAD),
			transport: 512,
			reach: REACH,
		};
		schedule.set_buffer(1, small);
		schedule.set_ended(true);
		let error = schedule.push(1, timing(1_000), unit(1, 11), true).unwrap_err();
		assert!(error.to_string().contains("decoder buffer"), "{error}");
	}

	/// Below the PID's Rx, a packet's last byte arrives only at the end of its position on the
	/// wire, so a unit still arriving at its decode time misses its deadline.
	#[test]
	fn a_packet_still_on_the_wire_at_decode_misses_its_deadline() {
		// Four positions a slot, each 6.25 ms on the wire.
		let mut schedule = Schedule::new(Duration::ZERO);
		schedule.set_rate(Some(4 * PACKET));
		schedule.set_buffer(1, VIDEO);
		// Slot 40 carries three packets; the fourth starts at 1,006.25 ms in slot 41 and
		// arrives whole at 1,012.5 ms, after the unit decodes.
		schedule.push(1, timing(1_010), unit(1, 4), true).unwrap();
		let error = std::iter::from_fn(|| schedule.next(None).transpose()).find_map(Result::err);
		assert!(error.is_some(), "the last byte arrives after the decode time");
	}

	/// The unit due first goes first, whatever order the PIDs were pushed in.
	#[test]
	fn the_earliest_deadline_goes_first() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.set_buffer(2, VIDEO);
		schedule.push(1, timing(1_100), unit(1, 39), true).unwrap();
		schedule.push(2, timing(1_000), unit(2, 39), false).unwrap();
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.filter(|(_, per_pid, _)| !per_pid.is_empty())
			.map(|(index, per_pid, _)| (index, per_pid.get(&1).copied(), per_pid.get(&2).copied()))
			.collect();
		assert_eq!(sent, [(36, None, Some(39)), (40, Some(39), None)]);
	}

	/// A PID takes no more packets in a slot than its transport buffer passes on, so a
	/// backlog of small audio units spreads out rather than going out in bulk.
	#[test]
	fn a_pid_takes_what_its_transport_buffer_passes_on() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		// 2 Mb/s: 33.2 packets a slot.
		let audio = Buffer {
			rate: 2_000_000,
			leak: None,
			size: Some(100_000),
			transport: 512,
			reach: REACH,
		};
		schedule.set_buffer(1, audio);
		for at in 0..20 {
			schedule.push(1, timing(1_000 + at), unit(1, 4), false).unwrap();
		}
		assert_eq!(checked_transport(&mut schedule, 9), 80);
	}

	/// The PCR opening every slot rides its PID's transport buffer too, in a clock packet or
	/// one of the PID's own.
	#[test]
	fn the_clock_packet_counts_against_its_pid() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		// 2 Mb/s: 33.2 packets a slot, the clock packet among them.
		let audio = Buffer {
			rate: 2_000_000,
			leak: None,
			size: Some(100_000),
			transport: 512,
			reach: REACH,
		};
		schedule.set_buffer(1, audio);
		schedule.set_clock(1);
		for at in 0..20 {
			schedule.push(1, timing(1_000 + at), unit(1, 4), false).unwrap();
		}
		assert_eq!(checked_transport(&mut schedule, 1), 80);
	}

	/// Video whose payload fills its level's leak keeps up: its transport buffer drains whole
	/// packets at Rx, the clock packet among them, while MB passes only payload on at Rbx
	/// (moq-dev/moq#5362). Charging whole packets, or the clock packet, to Rbx falls behind.
	#[test]
	fn video_keeps_up_with_its_full_leak() {
		// Level 4.0 without an HRD: Rx 1.2 x 24 Mb/s, Rbx 24 Mb/s, BSmux + BSoh 16,000 bytes.
		const RX: f64 = 28_800_000.0;
		const RBX: f64 = 24_000_000.0;
		let video = Buffer {
			rate: RX as u64,
			leak: Some(RBX as u64),
			size: Some(3_750_000),
			transport: 512,
			reach: REACH,
		};
		let rate = 26_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(rate));
		schedule.set_buffer(1, video);
		schedule.set_clock(1);
		// 20 s of 40 ms frames, each 652 packets of payload: 23.99 Mb/s.
		for frame in 0..500 {
			schedule
				.push(1, timing(1_000 + 40 * frame), unit(1, 652), frame % 25 == 0)
				.unwrap();
		}
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let (mut tb, mut mb, mut previous, mut sent) = (0.0f64, 0.0f64, None::<f64>, 0);
		while let Some(slot) = schedule.next(None).unwrap() {
			let mut clock = unit(1, 1);
			(clock[3], clock[4]) = (0x20, 183);
			for (position, packet) in slot.layout(&clock, 100, &null).chunks(188).enumerate() {
				let at = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / rate as f64;
				if let Some(previous) = previous {
					tb = (tb - (at - previous) * RX / 8.0).max(0.0);
					mb = (mb - (at - previous) * RBX / 8.0).max(0.0);
				}
				previous = Some(at);
				if pid(packet) == 1 {
					tb += 188.0;
					mb += elementary(packet).len() as f64;
					sent += payload(packet).len();
				}
				assert!(tb <= 512.001, "TB holds {tb} bytes at {at}");
				assert!(mb <= 16_000.0, "MB holds {mb} bytes at {at}");
			}
		}
		assert_eq!(sent, 500 * 652 * PAYLOAD, "every frame goes out");
	}

	/// Audio due before the video takes a position or a few between video packets, and MB
	/// holds enough payload ahead of the leak that the leak runs on through them: video with
	/// frames queued passes on its whole Rbx. With only a transport buffer's worth ahead, the
	/// leak idles after the audio's packets and video falls about 1 % below it.
	#[test]
	fn audio_between_video_packets_leaves_the_leak_running() {
		const RBX: u64 = 24_000_000;
		let video = Buffer {
			rate: 28_800_000,
			leak: Some(RBX),
			size: Some(3_750_000),
			transport: 512,
			reach: Duration::from_secs(2),
		};
		let audio = Buffer {
			rate: 2_000_000,
			leak: None,
			size: Some(3_584),
			transport: 512,
			reach: Duration::from_secs(2),
		};
		let rate = 26_000_000;
		let mut schedule = Schedule::new(Duration::from_secs(2));
		schedule.set_rate(Some(rate));
		schedule.set_buffer(7, video);
		schedule.set_buffer(8, audio);
		schedule.set_buffer(9, audio);
		schedule.set_clock(7);
		// Two seconds of video due from 2 s on, so it is queued from the first slot, and audio
		// due within the first second: MP2's 24 ms frames and AC-3's 32 ms, four packets each.
		let mut units: Vec<(u128, u8, usize)> = (0..50).map(|frame| (2_000 + 40 * frame, 7, 652)).collect();
		units.extend((0..40).map(|frame| (24 * frame, 8, 4)));
		units.extend((0..30).map(|frame| (32 * frame, 9, 4)));
		units.sort();
		for (ms, pid, packets) in units {
			schedule
				.push(pid.into(), timing(ms), unit(pid, packets), false)
				.unwrap();
		}
		let mut video = 0;
		for _ in 0..40 {
			let slot = schedule.next(None).unwrap().unwrap();
			video += slot.packets.chunks(188).filter(|packet| pid(packet) == 7).count();
		}
		// A second of the leak is 16,304 packets of payload.
		assert!(video >= 16_290, "{video} video packets in a second");
	}

	/// Unpadded, a video PID's slot carries the lesser of Rx in whole packets and Rbx in
	/// payload, fractions carried from slot to slot, and the clock packet counts against Rx
	/// alone.
	#[test]
	fn unpadded_video_takes_the_lesser_of_rx_and_rbx() {
		let second = |buffer: Buffer| (0..40).map(|index| buffer.per_slot(index, true)).sum::<usize>();
		// Rbx binds: 24 Mb/s of 184-byte payloads, 16,304.3 a second.
		let leak = Buffer {
			rate: 28_800_000,
			leak: Some(24_000_000),
			size: None,
			transport: 512,
			reach: Duration::ZERO,
		};
		assert_eq!(second(leak), 16_304);
		// Rx binds: 20 Mb/s of 188-byte packets, 13,297.9 a second, less 40 clock packets.
		let transport = Buffer {
			rate: 20_000_000,
			..leak
		};
		assert_eq!(second(transport), 13_297 - 40);
	}

	/// A PID whose transport buffer passes on a single packet a slot still carries one of its
	/// own beside the clock packet, so its units finish on time and the stream ends.
	#[test]
	fn a_one_packet_pid_still_progresses_beside_the_clock() {
		let mut schedule = Schedule::new(Duration::from_millis(500));
		schedule.set_rate(Some(RATE));
		// 76.8 kb/s: one packet a slot.
		let slow = Buffer {
			rate: 76_800,
			leak: None,
			size: Some(100_000),
			transport: 512,
			reach: REACH,
		};
		schedule.set_buffer(1, slow);
		schedule.set_clock(1);
		schedule.push(1, timing(1_000), unit(1, 4), true).unwrap();
		schedule.push(1, timing(1_400), unit(1, 4), false).unwrap();
		assert_eq!(checked_transport(&mut schedule, 1), 8, "both units go out");
		assert!(schedule.is_empty(), "and the stream ends");
	}

	#[test]
	fn an_overloaded_clock_pid_fails_instead_of_overflowing_its_transport_buffer() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.set_clock(1);
		schedule.set_buffer(
			1,
			Buffer {
				rate: 76_800,
				leak: None,
				size: Some(100_000),
				transport: 512,
				reach: REACH,
			},
		);
		// 76.8 kb/s passes on five packets in the window, the slots' PCRs among them.
		schedule.push(1, timing(1_000), unit(1, 6), false).unwrap();
		schedule.push(1, timing(1_200), unit(1, 6), false).unwrap();
		let error = std::iter::from_fn(|| schedule.next(None).transpose())
			.find_map(Result::err)
			.unwrap();
		assert!(error.to_string().contains("decode deadline"), "{error}");
	}

	/// A PID never holds more in its decoder buffer than it has: the next unit waits for the
	/// one before it to decode.
	#[test]
	fn a_full_decoder_buffer_holds_the_next_unit() {
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(RATE));
		// Room for ten packets.
		let small = Buffer {
			rate: 100_000_000,
			leak: None,
			size: Some(10 * PAYLOAD),
			transport: 512,
			reach: REACH,
		};
		schedule.set_buffer(1, small);
		schedule.push(1, timing(1_000), unit(1, 8), true).unwrap();
		schedule.push(1, timing(1_100), unit(1, 8), false).unwrap();
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.filter_map(|(index, per_pid, _)| per_pid.get(&1).map(|&n| (index, n)))
			.collect();
		// The first unit goes at once; the second fills what is left, then the rest once the
		// first decodes in slot 41.
		assert_eq!(sent, [(32, 8), (36, 2), (41, 6)]);
	}

	/// Packetize a PES with exactly `bytes` elementary bytes, each its offset's low byte, and
	/// adaptation stuffing.
	fn audio(bytes: usize) -> Vec<u8> {
		let mut pes: Vec<u8> = (0..14 + bytes).map(|i| i as u8).collect();
		pes[..14].copy_from_slice(&[0, 0, 1, 0xbd, 0, 0, 0x80, 0x80, 5, 0, 0, 0, 0, 0]);
		let mut out = Vec::new();
		for (index, body) in pes.chunks(PAYLOAD).enumerate() {
			let mut packet = [0xff; TsPacket::SIZE];
			packet[..4].copy_from_slice(&[0x47, if index == 0 { 0x40 } else { 0 }, 1, 0x10]);
			if body.len() < PAYLOAD {
				packet[3] |= 0x20;
				packet[4] = (PAYLOAD - body.len() - 1) as u8;
				if packet[4] > 0 {
					packet[5] = 0;
				}
			}
			packet[TsPacket::SIZE - body.len()..].copy_from_slice(body);
			out.extend(packet);
		}
		out
	}

	#[test]
	fn an_oversized_ac3_frame_names_both_sizes_at_admission() {
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_buffer(
			1,
			Buffer {
				rate: 2_000_000,
				leak: None,
				size: Some(2_592),
				transport: 512,
				reach: REACH,
			},
		);
		let error = schedule
			.push(1, timing(1_000), audio(2_688), false)
			.unwrap_err()
			.to_string();
		assert!(error.contains("2688"), "{error}");
		assert!(error.contains("2592"), "{error}");
		assert!(!schedule.queued(), "refusal must not partially queue a unit");
		// The 14-byte PES header waits in B with its unit (BSn includes BSoh, 2.4.2.3), so
		// 2,578 elementary bytes fill the buffer exactly and 2,580 overflow it.
		assert!(schedule.push(1, timing(1_000), audio(2_580), false).is_err());
		for bytes in [2_304, 2_436, 2_578] {
			schedule.push(1, timing(1_000), audio(bytes), false).unwrap();
		}
	}

	/// A unit with no packets, such as a dropped malformed section, has nothing to place.
	#[test]
	fn an_empty_unit_is_skipped() {
		for rate in [Some(RATE), None] {
			let mut schedule = Schedule::new(Duration::from_millis(100));
			schedule.set_rate(rate);
			schedule.set_buffer(1, VIDEO);
			schedule.push(1, timing(1_000), Vec::new(), false).unwrap();
			schedule.push(1, timing(1_040), unit(1, 2), true).unwrap();
			let sent: usize = drain(&mut schedule)
				.iter()
				.filter_map(|(_, per_pid, _)| per_pid.get(&1))
				.sum();
			assert_eq!(sent, 2, "{rate:?}");
		}
	}

	#[test]
	fn freed_decoder_room_is_used_after_the_exact_in_slot_instant() {
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(25_000_000));
		schedule.set_buffer(
			1,
			Buffer {
				rate: 2_000_000,
				leak: None,
				size: Some(2 * PAYLOAD),
				transport: 512,
				reach: REACH,
			},
		);
		schedule
			.push(
				1,
				Timing {
					by: ms(1_006),
					decode: ms(1_007),
				},
				unit(1, 2),
				false,
			)
			.unwrap();
		schedule
			.push(
				1,
				Timing {
					by: ms(1_038),
					decode: ms(1_039),
				},
				unit(1, 2),
				false,
			)
			.unwrap();
		let mut sent = Vec::new();
		while let Some(slot) = schedule.next(None).unwrap() {
			for &position in slot.positions.as_ref().unwrap() {
				sent.push(slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / 25_000_000.0);
			}
		}
		assert_eq!(sent.len(), 4);
		assert!(sent[1] < 1.007);
		assert!(sent[2] >= 1.007 && sent[2] < 1.025, "exact release: {sent:?}");
	}

	/// The ATSC buffer holds each legal 48 kHz AC-3 frame until its own decode instant.
	#[test]
	fn atsc_ac3_frames_finish_at_every_high_bitrate() {
		for bitrate in [384_000usize, 448_000, 512_000, 640_000] {
			let mut schedule = Schedule::new(Duration::from_millis(200));
			schedule.set_rate(Some(25_000_000));
			schedule.set_buffer(
				1,
				Buffer {
					rate: 2_000_000,
					leak: None,
					size: Some(2_592),
					transport: 512,
					reach: REACH,
				},
			);
			let bytes = bitrate / 8 * 32 / 1_000;
			let packets = (bytes + 14).div_ceil(PAYLOAD);
			for frame in 0..64 {
				schedule
					.push(
						1,
						Timing {
							by: ms(999 + frame * 32),
							decode: ms(1_000 + frame * 32),
						},
						audio(bytes),
						false,
					)
					.unwrap();
			}
			let mut sent = 0;
			let mut frames = 0;
			let mut held = [0usize; 64];
			let mut free = 0.0f64;
			while let Some(slot) = schedule
				.next(None)
				.unwrap_or_else(|error| panic!("{bitrate} b/s: {error}"))
			{
				for (&position, packet) in slot
					.positions
					.as_ref()
					.unwrap()
					.iter()
					.zip(slot.packets.as_chunks::<{ TsPacket::SIZE }>().0)
				{
					let at = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / 25_000_000.0;
					if packet[1] & 0x40 != 0 {
						frames += 1;
					}
					let frame = frames - 1;
					free = free.max(at) + 1504.0 / 2_000_000.0;
					let decode = 1.0 + frame as f64 * 0.032;
					assert!(free <= decode + 1e-7, "{bitrate} frame{frame}: {free} > {decode}");
					held[frame] += payload(packet).len();
					let occupancy: usize = held
						.iter()
						.enumerate()
						.filter(|(i, _)| 1.0 + *i as f64 * 0.032 > at)
						.map(|(_, held)| held)
						.sum();
					assert!(occupancy <= 2_592, "{bitrate} at{at}: decoder holds{occupancy}");
					sent += 1;
				}
			}
			assert_eq!(sent, 64 * packets);
		}
	}

	/// A PID with no decoder buffer to respect goes out only in the slot before its own.
	#[test]
	fn an_unbounded_pid_goes_just_before_it_is_due() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.push(7, timing(1_000), unit(7, 2), false).unwrap();
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.filter_map(|(index, per_pid, _)| per_pid.get(&7).map(|&n| (index, n)))
			.collect();
		assert_eq!(sent, [(39, 2)]);
	}

	/// Each PCR is the time of its slot's first byte at the rate, to within a tick of the
	/// system clock, so a rate that does not divide into whole packets per slot moves it off
	/// the grid by less than a packet. Both are functions of the slot alone, so a schedule
	/// started later lays the same slot alike.
	#[test]
	fn the_pcr_is_its_byte_position_at_the_rate() {
		let mut schedule = Schedule::new(Duration::ZERO);
		// 10 Mb/s is 166.2 packets a slot.
		let rate = 10_000_000;
		schedule.set_rate(Some(rate));
		schedule.set_buffer(1, VIDEO);
		for at in 0..40 {
			schedule.push(1, timing(1_000 + 25 * at), unit(1, 1), false).unwrap();
		}
		let mut laid = 0u128;
		let mut first = None;
		while let Some(slot) = schedule.next(None).unwrap() {
			let base = *first.get_or_insert(u128::from(slot.pcr));
			let expected = base + laid * TsPacket::SIZE as u128 * 8 * SYSTEM_CLOCK / u128::from(rate);
			assert!(u128::from(slot.pcr).abs_diff(expected) <= 1, "slot {}", slot.index);
			assert_eq!(u128::from(slot.pcr), rate_pcr(slot.index, rate), "slot {}", slot.index);
			let grid = grid_pcr(slot.index);
			let packet = TsPacket::SIZE as u128 * 8 * SYSTEM_CLOCK / u128::from(rate);
			assert!(
				grid - u128::from(slot.pcr) < packet,
				"slot {} strays a packet off the grid",
				slot.index
			);
			laid += 1 + (slot.packets.len() / TsPacket::SIZE + slot.nulls) as u128;
		}
	}

	/// A rate that turns up mid-stream takes over the queue from the next slot.
	#[test]
	fn a_rate_takes_over_from_the_next_slot() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_buffer(1, VIDEO);
		schedule.push(1, timing(1_000), unit(1, 8), true).unwrap();
		schedule.push(1, timing(1_100), unit(1, 100), false).unwrap();
		// Unpadded, both go out as soon as they are released, a window ahead: the slots before
		// the second unit's release are settled.
		let first: Vec<_> = std::iter::from_fn(|| schedule.next(Some(slot(ms(1_000)))).unwrap())
			.map(|slot| (slot.index, slot.packets.len() / TsPacket::SIZE, slot.nulls))
			.collect();
		assert_eq!(first, [(36, 8, 0), (37, 0, 0), (38, 0, 0), (39, 0, 0)]);
		schedule.set_rate(Some(RATE));
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.map(|(index, per_pid, _)| (index, per_pid.get(&1).copied().unwrap_or(0)))
			.collect();
		assert_eq!(sent, [(40, 39), (41, 39), (42, 22), (43, 0), (44, 0)]);
	}

	/// Unpadded, a unit still incomplete in its due slot goes out whole there, a unit goes out
	/// from the slot after the one before it even without a window, and the clock runs on to
	/// the last decode time.
	#[test]
	fn unpadded_units_go_out_by_their_decode_time() {
		let mut schedule = Schedule::new(Duration::ZERO);
		// 2 Mb/s passes 33 packets a slot.
		schedule.set_buffer(
			1,
			Buffer {
				rate: 2_000_000,
				leak: None,
				size: Some(100_000),
				transport: 512,
				reach: REACH,
			},
		);
		schedule.push(1, timing(1_000), unit(1, 50), true).unwrap();
		schedule.push(1, timing(1_100), unit(1, 4), false).unwrap();
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.map(|(index, per_pid, nulls)| (index, per_pid.get(&1).copied().unwrap_or(0), nulls))
			.collect();
		assert_eq!(sent, [(40, 50, 0), (41, 4, 0), (42, 0, 0), (43, 0, 0), (44, 0, 0)]);
	}

	/// A slot of a 25 Mb/s CBR feed whose video takes 86 % of it still keeps the video within
	/// its 512-byte transport buffer, which drains at Rx (moq-dev/moq#5142).
	#[test]
	fn a_full_slot_keeps_video_within_its_transport_buffer() {
		// 416 packets a slot at 25 Mb/s: the clock packet on the video PID, 358 video packets, 9 AC-3,
		// three MPEG audio PIDs of 5, 1 teletext, and nulls.
		let mut packets = unit(111, 358);
		packets.extend(unit(122, 9));
		for pid in [121, 123, 124] {
			packets.extend(unit(pid, 5));
		}
		packets.extend(unit(131, 1));
		let media = packets.len() / TsPacket::SIZE;
		let slot = Slot {
			index: 0,
			pcr: 0,
			packets,
			nulls: 416 - 1 - media,
			keyframe: false,
			units: vec![],
			positions: None,
			clocked: false,
			clock: None,
			tables: 0,
			begun: 0,
		};
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let laid = slot.layout(&unit(111, 1), 100, &null);

		// TB: 188 B in per packet on the PID, drained at Rx = 1.2 x 17,999,872 b/s.
		let drain = 21_599_846.0 / 8.0 * (188.0 * 8.0 / 25_000_000.0);
		let (mut tb, mut peak) = (0.0f64, 0.0f64);
		for _ in 0..4 {
			for packet in laid.chunks(188) {
				tb = (tb - drain).max(0.0);
				if pid(packet) == 111 {
					tb += 188.0;
					peak = peak.max(tb);
				}
			}
		}
		assert!(peak <= 512.0, "TB peak {peak:.0} B");
	}

	/// The program tables go just ahead of the first packet muxed after them, on whichever PID
	/// the round-robin lays out first, so they still lead the keyframe they were written for.
	#[test]
	fn the_tables_lead_every_packet_muxed_after_them() {
		// The PAT and PMT, a keyframe of one packet on PID 1, then four packets on PID 3.
		let mut packets = unit(0, 1);
		packets.extend(unit(100, 1));
		packets.extend(unit(1, 1));
		packets.extend(unit(3, 4));
		let slot = Slot {
			index: 0,
			pcr: 0,
			packets,
			nulls: 2,
			keyframe: true,
			units: vec![1, 3],
			positions: None,
			clocked: false,
			clock: None,
			tables: 0,
			begun: 0,
		};
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let laid: Vec<u16> = slot.layout(&unit(1, 1), 100, &null).chunks(188).map(pid).collect();
		assert_eq!(laid, [1, 0, 100, 3, 0x1fff, 3, 3, 1, 0x1fff, 3]);
	}

	/// A PAT and a multi-packet PMT go out at TBsys's rate rather than in a burst, still in
	/// order ahead of the keyframe muxed after them, and the video they delay keeps within its
	/// own transport buffer, even with a five-packet PMT or the video near its admission cap.
	#[test]
	fn a_multi_packet_pmt_is_paced_ahead_of_its_keyframe() {
		// The video's Rx is 1.2 x 17,999,872 b/s, as in the full-slot test above.
		const RX: u64 = 21_599_846;
		let video = Buffer {
			rate: RX,
			leak: None,
			size: Some(1_000_000),
			transport: 512,
			reach: REACH,
		};
		let audio = Buffer {
			rate: 2_000_000,
			leak: None,
			size: Some(3_584),
			transport: 512,
			reach: REACH,
		};
		// 25 Mb/s slots whose video takes most of each alongside AC-3, MPEG audio and teletext:
		// (video packets, PMT packets).
		for (count, parts) in [(300, 3), (300, 5), (352, 3)] {
			let rate = 25_000_000;
			let mut schedule = Schedule::new(Duration::from_millis(200));
			schedule.set_rate(Some(rate));
			schedule.set_clock(111);
			schedule.set_tables(100);
			schedule.set_buffer(111, video);
			let mut packets = unit(0, 1);
			for part in 0..parts {
				let mut pmt = unit(100, 1);
				pmt[4] = part;
				packets.extend(pmt);
			}
			packets.extend(unit(111, count));
			schedule.push(111, timing(1_000), packets, true).unwrap();
			for (pid, count) in [(122, 9), (121, 5), (123, 5), (124, 5), (131, 1)] {
				schedule.set_buffer(pid.into(), audio);
				schedule
					.push(pid.into(), timing(1_000), unit(pid, count), false)
					.unwrap();
			}
			let mut clock = unit(111, 1);
			(clock[3], clock[4]) = (0x20, 183);
			let wire = wire(&mut schedule, &clock);

			let tables: Vec<usize> = (0..wire.len())
				.filter(|&k| [0, 100].contains(&pid(&wire[k].1)))
				.collect();
			let order: Vec<u8> = tables.iter().skip(1).map(|&k| wire[k].1[4]).collect();
			assert_eq!(order, (0..parts).collect::<Vec<_>>(), "the PMT keeps its order");
			let keyframe = (0..wire.len())
				.find(|&k| pid(&wire[k].1) == 111 && !payload(&wire[k].1).is_empty())
				.unwrap();
			assert!(tables.iter().all(|&k| k < keyframe), "the tables lead the keyframe");

			let (tbsys, bsys) = system_peaks(&wire, rate);
			assert!(tbsys <= 512.001, "TBsys peak {tbsys:.0} B, {parts} PMT packets");
			assert!(bsys <= 1536.001, "Bsys peak {bsys:.0} B, {parts} PMT packets");
			let (tb, _) = peaks(&wire, rate, &[111], RX as f64, f64::INFINITY);
			assert!(tb <= 512.001, "video TB peak {tb:.0} B, {parts} PMT packets");
		}
	}

	/// The PCR `packet`'s adaptation field carries, in system clock ticks.
	fn pcr_of(packet: &[u8]) -> Option<u64> {
		let field = (packet[3] & 0x20 != 0 && packet[4] >= 7 && packet[5] & 0x10 != 0).then(|| &packet[6..12])?;
		let base = u64::from(field[0]) << 25
			| u64::from(field[1]) << 17
			| u64::from(field[2]) << 9
			| u64::from(field[3]) << 1
			| u64::from(field[4]) >> 7;
		Some(base * 300 + (u64::from(field[4] & 1) << 8 | u64::from(field[5])))
	}

	/// A PCR moves the PES it rides on by its field's bytes: into the last packet's stuffing,
	/// or into one more packet where that has too little.
	#[test]
	fn a_pcr_reflows_the_pes_it_rides() {
		let body = |packets: &[u8]| -> Vec<u8> { packets.chunks(TsPacket::SIZE).flat_map(payload).copied().collect() };
		// With its 14-byte header: one packet with room, two with 7 bytes of stuffing, two full.
		for (bytes, count) in [(100, 1), (346, 2), (354, 3)] {
			let pes = audio(bytes);
			let ticks = 27_000_000 * 5 + 299;
			let out = with_pcr(&pes, ticks).expect("an exported PES takes a PCR");
			let packets: Vec<&[u8]> = out.chunks(TsPacket::SIZE).collect();
			assert_eq!(packets.len(), count, "{bytes} bytes");
			assert_eq!(out.len(), count * TsPacket::SIZE, "{bytes} bytes");
			assert_eq!(pcr_of(packets[0]), Some(ticks), "{bytes} bytes");
			assert!(
				packets[0][1] & 0x40 != 0,
				"{bytes} bytes: the first packet still starts the PES"
			);
			assert!(
				packets[1..]
					.iter()
					.all(|packet| packet[1] & 0x40 == 0 && pcr_of(packet).is_none()),
				"{bytes} bytes: the PCR and the PES start stay in the first packet"
			);
			assert_eq!(body(&out), body(&pes), "{bytes} bytes: the PES is unchanged");
			assert!(
				with_pcr(&out, ticks).is_none(),
				"a PES carrying a PCR already is left alone"
			);
		}
	}

	/// A slot's PCR rides the clock PID's packet when that PID can go out at once, sparing the
	/// slot a clock packet; a slot with only another PID's packets opens with one of its own.
	#[test]
	fn the_pcr_rides_the_clock_pid_when_it_can() {
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.set_buffer(2, VIDEO);
		schedule.set_clock(1);
		schedule.push(1, timing(1_000), unit(1, 4), true).unwrap();
		schedule.push(2, timing(2_000), unit(2, 4), false).unwrap();
		let mut clock = unit(1, 1);
		(clock[3], clock[4]) = (0x20, 183);
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let (mut sent, mut clocked) = (HashMap::<u16, usize>::new(), Vec::new());
		while let Some(slot) = schedule.next(None).unwrap() {
			let laid = slot.layout(&clock, 100, &null);
			assert_eq!(laid.len(), 40 * TsPacket::SIZE, "slot {} keeps its size", slot.index);
			let first = &laid[..TsPacket::SIZE];
			match slot.clocked {
				true => {
					assert_eq!(pid(first), 1, "slot {}", slot.index);
					assert_eq!(pcr_of(first), Some(slot.pcr), "slot {}", slot.index);
				}
				false => assert_eq!(first, &clock[..], "slot {} opens with a clock packet", slot.index),
			}
			for packet in laid.chunks(TsPacket::SIZE).skip(usize::from(!slot.clocked)) {
				if [1, 2].contains(&pid(packet)) {
					*sent.entry(pid(packet)).or_default() += payload(packet).len();
					clocked.push((pid(packet), slot.clocked));
				}
			}
		}
		assert_eq!(
			sent,
			HashMap::from([(1, 4 * PAYLOAD), (2, 4 * PAYLOAD)]),
			"no payload is lost"
		);
		assert_eq!(
			clocked.first(),
			Some(&(1, true)),
			"the clock PID's unit carries its slot's PCR"
		);
		assert!(
			clocked
				.iter()
				.filter(|&&(pid, _)| pid == 2)
				.all(|&(_, clocked)| !clocked),
			"another PID's slot has its own clock packet"
		);
	}

	/// At 90 kb/s a slot alternates between one and two packets. In a one-packet slot, the
	/// clock PID's packet carrying the PCR takes the only position, the one its clock packet
	/// would have.
	#[test]
	fn the_pcr_rides_the_only_packet_of_a_one_packet_slot() {
		let mut schedule = Schedule::new(Duration::from_millis(500));
		schedule.set_rate(Some(90_000));
		schedule.set_buffer(1, VIDEO);
		schedule.set_clock(1);
		for at in 0..8 {
			schedule.push(1, timing(1_000 + 125 * at), audio(100), at == 0).unwrap();
		}
		let mut clock = unit(1, 1);
		(clock[3], clock[4]) = (0x20, 183);
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let mut carried = Vec::new();
		while let Some(slot) = schedule.next(None).unwrap() {
			let laid = slot.layout(&clock, 100, &null);
			if slot.clocked {
				assert_eq!(pcr_of(&laid), Some(slot.pcr), "slot {}", slot.index);
				carried.push(laid.len() / TsPacket::SIZE);
			}
		}
		assert_eq!(carried.len(), 8, "every PES carries its slot's PCR");
		assert!(carried.contains(&1), "some ride a one-packet slot: {carried:?}");
	}

	/// Each frame of a PES that packs several leaves B at its own decode time, the PES header
	/// with the first, so the next PES goes in once the first frame is removed rather than the
	/// last.
	#[test]
	fn packed_frames_leave_the_decoder_buffer_one_by_one() {
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(RATE));
		// The packed PES's 1,514 bytes leave too little for the next one's 414 until its first
		// frame's 514 leave.
		schedule.set_buffer(
			1,
			Buffer {
				rate: 2_000_000,
				leak: None,
				size: Some(1_600),
				transport: 512,
				reach: REACH,
			},
		);
		let frames = vec![(ms(1_000), 500), (ms(1_024), 500), (ms(1_048), 500)];
		schedule
			.push_frames(
				1,
				Timing {
					by: ms(999),
					decode: ms(1_000),
				},
				audio(1_500),
				false,
				frames,
			)
			.unwrap();
		schedule
			.push(
				1,
				Timing {
					by: ms(1_099),
					decode: ms(1_100),
				},
				audio(400),
				false,
			)
			.unwrap();
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let mut starts = Vec::new();
		while let Some(slot) = schedule.next(None).unwrap() {
			for (position, packet) in slot.layout(&unit(9, 1), 100, &null).chunks(TsPacket::SIZE).enumerate() {
				if pid(packet) == 1 && packet[1] & 0x40 != 0 {
					starts.push(slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / RATE as f64);
				}
			}
		}
		assert_eq!(starts.len(), 2);
		assert!(
			starts[1] >= 1.000 && starts[1] < 1.024,
			"the next PES starts at {}",
			starts[1]
		);
	}

	/// A packed PES must be whole by its first frame's decode time, so its send-ahead runs
	/// from that frame's due slot, not its last's: here the PES needs over two slots at Rx.
	#[test]
	fn a_packed_pes_goes_out_ahead_of_its_first_frame() {
		let mut schedule = Schedule::new(Duration::from_secs(1));
		schedule.set_rate(Some(10_000_000));
		schedule.set_buffer(
			1,
			Buffer {
				rate: 2_000_000,
				leak: None,
				size: Some(12_896),
				transport: 512,
				reach: Duration::from_nanos(12_896 * 8 * 1_000_000_000 / 2_000_000),
			},
		);
		schedule.set_buffer(2, VIDEO);
		// Video pushed ahead of it starts the PES no earlier than its own due slot.
		schedule.push(2, timing(3_160), unit(2, 2), true).unwrap();
		let frames = (0..5).map(|i| (ms(3_200 + 32 * i), 2_560)).collect();
		schedule
			.push_frames(
				1,
				Timing {
					by: ms(3_199),
					decode: ms(3_200),
				},
				audio(12_800),
				false,
				frames,
			)
			.unwrap();
		while schedule.queued() {
			schedule.next(Some(u128::MAX)).unwrap();
		}
	}
}
