//! Lays a TS export's packets onto its PCR grid.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::time::Duration;

use mpeg2ts::ts::TsPacket;

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
	/// How fast the receiver takes the PID's packets on, in bits per second: its transport
	/// buffer's Rx, or for video the slower leak from MB into EB.
	pub rate: u64,
	/// The decoder buffer (EB for video, B for audio) in bytes, which a unit fills from its
	/// first packet until it decodes. `None` bounds nothing, so the PID's units go out only
	/// in the slot before their own.
	pub size: Option<usize>,
	/// The transport buffer in bytes: 512 (ISO 13818-1 2.4.2.3), 480 for teletext (ETSI EN
	/// 300 472 5).
	pub transport: usize,
}

impl Buffer {
	/// The program tables' (ISO 13818-1 2.4.2.3, Rxsys).
	pub const SYSTEM: Self = Self {
		rate: 1_000_000,
		size: None,
		transport: 512,
	};

	/// The packets one slot may carry on the PID: what its transport buffer passes on within
	/// the slot, less one, but at least one. Even laid out evenly, the packets bunch a little
	/// where one slot meets the next, and the slack drains that off.
	fn per_slot(&self) -> usize {
		let bits = u128::from(self.rate) * PCR_INTERVAL.as_nanos() / 1_000_000_000;
		(bits / (TsPacket::SIZE as u128 * 8)).saturating_sub(1).max(1) as usize
	}
}

/// TBsys in a receiver over one slot: the single 512-byte transport buffer that PIDs 0-3 and
/// the program's PMT share, draining at Rxsys (ISO 13818-1 2.4.2.3).
///
/// Its fill is in bytes times the slot's positions, so each position drains a whole slot's
/// worth of Rxsys bytes and nothing rounds. It starts each slot empty: a slot's 25 ms drains
/// six times its size.
struct SystemBuffer {
	pmt_pid: u16,
	positions: u128,
	fill: u128,
}

impl SystemBuffer {
	/// The buffer's size in bytes.
	const SIZE: u128 = 512;
	/// The bytes it drains a slot.
	const DRAIN: u128 = Buffer::SYSTEM.rate as u128 * PCR_INTERVAL.as_nanos() / 8_000_000_000;

	fn new(pmt_pid: u16, positions: usize) -> Self {
		Self {
			pmt_pid,
			positions: positions as u128,
			fill: 0,
		}
	}

	fn carries(&self, pid: u16) -> bool {
		pid <= 3 || pid == self.pmt_pid
	}

	/// Whether a packet it carries fits at the next position.
	fn fits(&self) -> bool {
		self.fill.saturating_sub(Self::DRAIN) + TsPacket::SIZE as u128 * self.positions <= Self::SIZE * self.positions
	}

	/// Take the next position, laid with `packet`.
	fn lay(&mut self, packet: &[u8]) {
		self.fill = self.fill.saturating_sub(Self::DRAIN);
		if self.carries(pid(packet)) {
			self.fill += TsPacket::SIZE as u128 * self.positions;
		}
	}
}

/// The last transport arrival and the decoder removal, in nanoseconds.
#[derive(Clone, Copy)]
pub(super) struct Timing {
	/// When the last packet must enter the transport buffer.
	pub by: u128,
	/// When the decoder removes this unit.
	pub decode: u128,
}

/// One access unit's packets, with the tables muxed ahead of it, still to go out.
struct Unit {
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
	/// Its bytes in the PID's decoder buffer, until it decodes.
	held: usize,
}

impl Unit {
	fn count(&self) -> usize {
		self.packets.len() / TsPacket::SIZE
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
	/// The most packets a slot admission lets each bounded PID carry.
	pub caps: HashMap<u16, usize>,
	/// Exact positions of the media packets in a padded slot, counting the opening clock.
	positions: Option<Vec<usize>>,
}

impl Slot {
	/// The whole slot, opening with its `clock` packet. Padded slots use the exact
	/// positions planned against receiver buffers. Unpadded slots use smooth weighted
	/// round-robin: each PID (the clock packet counted on its own) and the null packets take
	/// positions in proportion to their count, as evenly as they can, every PID keeping its
	/// own order.
	///
	/// A PID's packets in a run would arrive at the multiplex's full rate and overflow its
	/// 512-byte transport buffer, which drains only at its Rx (ISO 13818-1 2.4.2.3): an audio
	/// frame's few packets muxed whole, or a video PID taking most of the slot. The program
	/// tables (PAT and the PMT on `pmt_pid`) go just ahead of the first packet muxed after
	/// them, so they still lead the keyframe they were written for and a reader knows each PID
	/// before its first packet.
	///
	/// The tables share one [`SystemBuffer`]. Where a table ahead of the slot's first media
	/// packet would overflow it, a null takes the position instead, so a PMT spanning several
	/// packets goes out at the buffer's rate, as long as the slot has nulls to spare and the
	/// delayed PIDs can still keep within their own transport buffers ([`Self::caps`]).
	pub fn layout(&self, clock: &[u8], pmt_pid: u16, null: &[u8]) -> Vec<u8> {
		let packets = self.packets.as_chunks::<{ TsPacket::SIZE }>().0;
		if let Some(positions) = &self.positions {
			let mut out = Vec::with_capacity((1 + packets.len() + self.nulls) * TsPacket::SIZE);
			out.extend_from_slice(clock);
			for (&position, packet) in positions.iter().zip(packets) {
				while out.len() / TsPacket::SIZE < position {
					out.extend_from_slice(null);
				}
				out.extend_from_slice(packet);
			}
			while out.len() / TsPacket::SIZE < 1 + packets.len() + self.nulls {
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
		let mut nulls = self.nulls;
		let mut weights: Vec<i64> = lanes
			.iter()
			.map(|(_, lane)| lane.len() as i64)
			.chain([nulls as i64])
			.collect();
		weights[0] += 1;
		let mut total: i64 = weights.iter().sum();
		let mut credit = weights.clone();
		credit[0] -= total;
		// Positions the round-robin has still to lay.
		let mut left = total - 1;
		// No media packet has gone out yet, so laying the slot afresh spaces no PID worse.
		let mut fresh = true;

		let positions = 1 + packets.len() + self.nulls;
		let mut system = SystemBuffer::new(pmt_pid, positions);
		let mut out = Vec::with_capacity(positions * TsPacket::SIZE);
		out.extend_from_slice(clock);
		system.lay(clock);
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
				// Ahead of the slot's first media packet, a null spaces a table that would
				// overflow TBsys, and the rest of the slot is laid out afresh around it, so no
				// PID's packets bunch up to make up the time. The stall must leave every PID
				// spread no denser over the rest of the slot than admission holds it to over a
				// whole one. Otherwise the table goes out regardless: past the first media
				// packet, laying afresh would drop the spacing already given the PIDs served.
				let room = || {
					// The round-robin's positions left after the stall; the tables still to go
					// lead the PIDs they delay, so lend them no time.
					let after = left as usize - 1;
					lanes.iter().all(|(pid, lane)| {
						self.caps
							.get(pid)
							.is_none_or(|&cap| lane.len() * positions <= cap * after)
					})
				};
				if fresh && !system.fits() && nulls > 0 && room() {
					nulls -= 1;
					system.lay(null);
					out.extend_from_slice(null);
					weights = lanes
						.iter()
						.map(|(_, lane)| lane.len() as i64)
						.chain([nulls as i64])
						.collect();
					total = weights.iter().sum();
					credit = vec![0; weights.len()];
					left = total;
					continue;
				}
				tables.pop_front();
				system.lay(&packets[table]);
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
					fresh = false;
					let i = lane
						.pop_front()
						.expect("a lane takes no more positions than its weight");
					&packets[i]
				}
				None => {
					nulls -= 1;
					null
				}
			};
			system.lay(packet);
			out.extend_from_slice(packet);
		}
		out
	}
}

/// Packs muxed access units onto the PCR grid.
///
/// Each slot's packets go earliest deadline first across PIDs, each PID in its own order, and
/// each unit as soon as its PID's buffers in a receiver admit it. Padded output checks
/// transport occupancy and decoder removal at exact packet positions. A unit may go out up to the `window` before
/// its due slot, which keeps that far ahead of a heavy passage, so the output trails the
/// decode times by the window.
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
	/// Slots before its due slot a unit may go out: the export's delay.
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
	buffers: HashMap<u16, Buffer>,
	/// The PID every slot's clock packet rides on, from [`Self::set_clock`].
	clock: Option<u16>,
	/// The PMT's PID, which shares TBsys with the PAT, from [`Self::set_tables`].
	pmt: Option<u16>,
	/// Units sent into a decoder buffer: their PID, decode nanoseconds and bytes held.
	decoding: VecDeque<(u16, u128, usize)>,
	/// When each transport buffer's last admitted packet finishes leaving it, and when it
	/// last started filling from empty.
	transport: HashMap<u16, (i128, i128)>,
	/// Every source has ended, so a unit that misses its deadline is the tail going out late
	/// rather than an output that cannot keep up.
	ended: bool,
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
			buffers: HashMap::new(),
			clock: None,
			pmt: None,
			decoding: VecDeque::new(),
			transport: HashMap::new(),
			ended: false,
		}
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
		self.buffers.insert(pid, buffer);
	}

	/// Hold the program tables, the PAT and the PMT on `pmt_pid`, to the one TBsys they share.
	pub fn set_tables(&mut self, pmt_pid: u16) {
		self.buffers.insert(0, Buffer::SYSTEM);
		self.buffers.insert(pmt_pid, Buffer::SYSTEM);
		self.pmt = Some(pmt_pid);
	}

	/// The transport buffer `pid`'s packets enter: TBsys for the reserved system PIDs (PAT,
	/// CAT, TSDT, IPMP) and the PMT, else its own.
	fn transport_buffer(&self, pid: u16) -> u16 {
		match pid <= 3 || self.pmt == Some(pid) {
			true => 0,
			false => pid,
		}
	}

	/// Count every slot's clock packet, which rides on `pid`, against that PID's transport
	/// buffer. Unpadded admission retains at least one media packet per slot.
	pub fn set_clock(&mut self, pid: u16) {
		self.clock = Some(pid);
	}

	/// Queue an access unit on `pid` (whole TS packets, the tables muxed ahead of it first)
	/// with its exact transport deadline and decoder removal time.
	pub fn push(&mut self, pid: u16, timing: Timing, packets: Vec<u8>, keyframe: bool) -> anyhow::Result<()> {
		if packets.is_empty() {
			return Ok(());
		}
		let due = slot(timing.by);
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
			if bytes > size {
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
		self.horizon = self.horizon.max(Some(timing.by.div_ceil(PCR_INTERVAL.as_nanos())));
		let start = self.last.map_or(due, |last| (last + 1).min(due));
		self.last = Some(due);
		self.units.push_back(Unit {
			pid,
			due,
			timing,
			start,
			packets,
			sent: 0,
			keyframe,
			late: self.next.is_some_and(|next| due < next),
			held: 0,
		});
		Ok(())
	}

	/// Whether a unit is still to go out.
	#[cfg(test)]
	pub fn queued(&self) -> bool {
		!self.units.is_empty()
	}

	/// Whether nothing is left to lay out: every unit sent, and the clock past its decode time.
	pub fn is_empty(&self) -> bool {
		self.units.is_empty()
			&& self
				.horizon
				.is_none_or(|horizon| self.next.is_some_and(|next| next > horizon))
	}

	/// Drop everything queued and start the grid afresh.
	pub fn clear(&mut self) {
		self.units.clear();
		self.decoding.clear();
		self.transport.clear();
		self.next = None;
		self.horizon = None;
		self.last = None;
		self.ended = false;
	}

	/// The first slot `unit` may go out in: a window ahead of its due slot, or only the slot
	/// before it for a PID whose decoder buffer bounds nothing, and never before the slot
	/// after the unit pushed ahead of it needs.
	fn release(&self, unit: &Unit) -> u128 {
		let ahead = match self.buffers.get(&unit.pid).and_then(|buffer| buffer.size) {
			_ if unit.late => return 0,
			Some(_) => self.window,
			None => 1,
		};
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
				let target = first.or(known.map(|known| known.saturating_sub(self.window)));
				Some(next.max(target.map_or(0, |target| target.saturating_sub(BACKFILL))))
			}
		}
	}

	/// The next slot to lay out, and the first slot a unit still to be pushed could be due
	/// in once every unit that may ride it is known: the `known` that settles it.
	pub fn upcoming(&self) -> Option<(u128, u128)> {
		let index = self.index(None)?;
		Some((index, index + self.window + 1))
	}

	/// Lay out the next slot, if it is settled.
	///
	/// `known` is the first slot a unit still to be pushed could be due in, which settles
	/// every slot whose window ends before it; `None` means nothing more is coming, so every
	/// queued unit goes out and the clock runs on to the last decode time.
	pub fn next(&mut self, known: Option<u128>) -> anyhow::Result<Option<Slot>> {
		if known.is_none() && self.is_empty() {
			return Ok(None);
		}
		let Some(index) = self.index(known) else {
			return Ok(None);
		};
		if known.is_some_and(|known| index + self.window >= known) {
			return Ok(None);
		}
		self.next = Some(index + 1);
		if let Some(rate) = self.rate {
			return self.padded(index, rate).map(Some);
		}

		let mut take = self.admit(index, usize::MAX);
		for (unit, take) in self.units.iter_mut().zip(take.iter_mut()) {
			if unit.due <= index && *take < unit.remaining() {
				if self.buffers.get(&unit.pid).is_some_and(|buffer| buffer.size.is_some()) {
					unit.held += unit.packets_on(unit.sent + *take..unit.count(), unit.pid) * PAYLOAD;
				}
				*take = unit.remaining();
			}
		}

		let mut packets = Vec::new();
		let mut keyframe = false;
		let mut units = Vec::new();
		for (unit, take) in self.units.iter_mut().zip(take) {
			if take == 0 {
				continue;
			}
			if unit.sent == 0 {
				keyframe |= unit.keyframe;
				// Only the tables muxed ahead of a frame that wrote nothing are no access unit.
				if unit.packets_on(0..unit.count(), unit.pid) > 0 {
					units.push(unit.pid);
				}
			}
			let from = unit.sent * TsPacket::SIZE;
			packets.extend_from_slice(&unit.packets[from..from + take * TsPacket::SIZE]);
			unit.sent += take;
		}
		for unit in self.units.iter().filter(|unit| unit.remaining() == 0 && unit.held > 0) {
			self.decoding.push_back((unit.pid, unit.timing.decode, unit.held));
		}
		self.units.retain(|unit| unit.remaining() > 0);
		Ok(Some(Slot {
			index,
			pcr: grid_pcr(index) as u64,
			packets,
			nulls: 0,
			keyframe,
			units,
			caps: self
				.buffers
				.iter()
				.map(|(&pid, buffer)| (pid, buffer.per_slot()))
				.collect(),
			positions: None,
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
			caps: HashMap::new(),
			positions: Some(Vec::new()),
		};
		// The transport buffer the clock packet rides and how long it holds it, which every
		// slot's clock packet may add to a busy stretch without the chance to wait.
		let mut clock = None;
		if let Some(pid) = self.clock
			&& let Some(buffer) = self.buffers.get(&pid)
		{
			let duration = (TsPacket::SIZE as u128 * 8 * 1_000_000_000).div_ceil(u128::from(buffer.rate)) as i128;
			let tb = self.transport_buffer(pid);
			let (free, since) = self.transport.entry(tb).or_insert((time(0), time(0)));
			if *free + TICK <= time(0) {
				*since = time(0);
			}
			*free = ((*free).max(time(0)) + duration).max(time(1));
			clock = Some((tb, duration));
		}
		// One head per PID preserves decode order without walking all future units at every
		// transport position. Removal events keep occupancy work proportional to units.
		let mut lanes: Vec<VecDeque<usize>> = Vec::new();
		let mut lane_by_pid = HashMap::new();
		let mut occupancy: HashMap<u16, usize> = HashMap::new();
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
			if unit.timing.decode as i128 > time(0) {
				*occupancy.entry(unit.pid).or_default() += unit.held;
				removals.push((unit.timing.decode, unit.pid, Some(i), 0));
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
		let mut position = 1;
		while position < count {
			let now = time(position);
			while let Some((_, pid, unit, held)) = removals.next_if(|event| event.0 as i128 <= now) {
				let held = unit.map_or(held, |i| std::mem::take(&mut self.units[i].held));
				*occupancy.get_mut(&pid).unwrap() -= held;
			}
			while waiting.peek().is_some_and(|Reverse((at, _))| *at <= now) {
				let Reverse((_, lane)) = waiting.pop().unwrap();
				ready.push(Reverse((self.units[*lanes[lane].front().unwrap()].timing.by, lane)));
			}
			let mut emitted = false;
			while let Some(Reverse((_, lane))) = ready.pop() {
				let i = *lanes[lane].front().unwrap();
				let unit = &self.units[i];
				let packet = &unit.packets[unit.sent * TsPacket::SIZE..(unit.sent + 1) * TsPacket::SIZE];
				let pid = self::pid(packet);
				let buffer = self.buffers.get(&pid);
				let duration = buffer.map_or(0, |buffer| {
					(TsPacket::SIZE as u128 * 8 * 1_000_000_000).div_ceil(u128::from(buffer.rate))
				});
				let tb = self.transport_buffer(pid);
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
					// The next slot's mandatory clock packet must fit even at a full media PID.
					if self.clock == Some(pid) && drained > time(count) + capacity as i128 - duration as i128 {
						continue;
					}
				}
				// No byte leaves the transport buffer before it has arrived: below Rx, the
				// multiplex rate decides when the packet's last byte gets there.
				let done = drained.max(time(position + 1));
				// A transport buffer must empty at least once a second (2.4.2.3), so a PID kept
				// busy that long waits for it to. Every later slot's clock packet that arrives
				// before the buffer empties extends the stretch without the chance to wait.
				let mut busy = done;
				if let Some((_, duration)) = clock.filter(|&(clock, _)| clock == tb) {
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
				let held = if pid == unit.pid && buffer.is_some_and(|buffer| buffer.size.is_some()) {
					payload(packet).len()
				} else {
					0
				};
				if let Some(size) = buffer.and_then(|buffer| buffer.size)
					&& occupancy.get(&pid).copied().unwrap_or(0) + held > size
				{
					if let Some(event) = removals.peek() {
						let ready = event.0 as i128;
						waiting.push(Reverse((ready, lane)));
					}
					continue;
				}
				if !self.ended && !unit.late && pid == unit.pid && done > unit.timing.decode as i128 {
					anyhow::bail!(
						"MPEG-TS output missed a decode deadline on PID {pid} at {rate} b/s; raise the delay or the multiplex rate"
					);
				}
				self.transport.insert(tb, (done, since));
				out.positions.as_mut().unwrap().push(position);
				out.packets.extend_from_slice(packet);
				out.nulls -= 1;
				let unit = &mut self.units[i];
				if unit.sent == 0 {
					out.keyframe |= unit.keyframe;
					if unit.packets_on(0..unit.count(), unit.pid) > 0 {
						out.units.push(unit.pid);
					}
				}
				unit.sent += 1;
				if unit.timing.decode as i128 > now {
					unit.held += held;
					*occupancy.entry(unit.pid).or_default() += held;
				}
				if unit.remaining() == 0 {
					lanes[lane].pop_front();
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
			if emitted {
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
			&& let Some(unit) = self
				.units
				.iter()
				.find(|unit| !unit.late && unit.timing.by as i128 <= end && unit.remaining() > 0)
		{
			anyhow::bail!(
				"MPEG-TS output missed a decode deadline on PID {} at {rate} b/s; raise the delay or the multiplex rate",
				unit.pid
			);
		}
		for unit in self.units.iter().filter(|unit| unit.remaining() == 0 && unit.held > 0) {
			self.decoding.push_back((unit.pid, unit.timing.decode, unit.held));
		}
		self.units.retain(|unit| unit.remaining() > 0);
		Ok(out)
	}

	/// How many of each unit's packets slot `index` carries, at most `budget` in all.
	///
	/// Earliest deadline first across PIDs. A PID whose unit stops short (its buffers full,
	/// or not yet released) takes nothing more this slot, so each keeps its own order.
	fn admit(&mut self, index: u128, mut budget: usize) -> Vec<usize> {
		// Unpadded layout has no exact packet clock until the slot is assembled, so it
		// retains units until their decode instant is behind the opening grid point.
		let mut occupancy: HashMap<u16, usize> = HashMap::new();
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
		// The clock packet counts against its PID unless that would leave the PID's media no
		// packet at all, which would never finish a unit however sparse its frames.
		let mut carried: HashMap<u16, usize> = self
			.clock
			.filter(|pid| self.buffers.get(pid).is_some_and(|buffer| buffer.per_slot() > 1))
			.map(|pid| (pid, 1))
			.into_iter()
			.collect();
		let mut stopped: Vec<u16> = Vec::new();
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
			if self.release(unit) <= index {
				while n < unit.remaining() && budget > 0 {
					let pid = unit.pid_at(unit.sent + n);
					let cap = self.buffers.get(&pid).map_or(usize::MAX, Buffer::per_slot);
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

	/// 40 packets per slot, the clock packet included.
	const RATE: u64 = 40 * PACKET;
	/// A video-like buffer: far more than a slot passes on, and a large decoder buffer.
	const VIDEO: Buffer = Buffer {
		rate: 100_000_000,
		size: Some(1_000_000),
		transport: 512,
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

	/// Check actual wire positions against the receiver's transport buffer, clock included.
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
				.count();
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
		sent
	}

	/// Distinct tune-in tables keep their copies and still fit TBsys, which the PAT and PMT
	/// share.
	#[test]
	fn system_packets_are_paced_with_media_admission() {
		for pids in [3u8, 4, 8] {
			for count in [10, 100] {
				for rate in [25_000_000, 50_000_000, 100_000_000] {
					let mut schedule = Schedule::new(Duration::from_millis(200));
					schedule.set_rate(Some(rate));
					schedule.set_clock(1);
					schedule.set_tables(100);
					for pid in 1..=pids {
						schedule.set_buffer(u16::from(pid), VIDEO);
						let packets = [unit(0, 1), unit(100, 1), unit(pid, count)].concat();
						schedule.push(u16::from(pid), timing(1_000), packets, true).unwrap();
					}
					let (mut fill, mut previous, mut tables) = (0.0f64, None::<f64>, [0; 2]);
					while let Some(slot) = schedule.next(None).unwrap() {
						for (position, packet) in slot.layout(&unit(1, 1), 100, &[0xff; 188]).chunks(188).enumerate() {
							let at = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / rate as f64;
							if let Some(previous) = previous {
								fill = (fill - (at - previous) * 125_000.0).max(0.0);
							}
							previous = Some(at);
							for (i, system_pid) in [0, 100].into_iter().enumerate() {
								if pid(packet) == system_pid {
									fill += 188.0;
									tables[i] += 1;
								}
							}
							assert!(
								fill <= 512.001,
								"{pids} PIDs x {count} packets at {rate}: TBsys holds {fill} bytes"
							);
						}
					}
					assert_eq!(tables, [usize::from(pids); 2]);
				}
			}
		}
	}

	/// Sections on a reserved system PID share TBsys with the PAT and PMT.
	#[test]
	fn reserved_system_pids_share_tbsys_with_the_tables() {
		let rate = 25_000_000;
		let mut schedule = Schedule::new(Duration::from_millis(200));
		schedule.set_rate(Some(rate));
		schedule.set_clock(7);
		schedule.set_buffer(7, VIDEO);
		schedule.set_tables(100);
		schedule.set_buffer(1, Buffer::SYSTEM);
		// A CAT muxed with the tables ahead of a keyframe, as the export writes them.
		let packets = [unit(0, 1), unit(100, 2), unit(1, 3), unit(7, 1)].concat();
		schedule.push(7, timing(1_000), packets, true).unwrap();
		let (mut fill, mut previous) = (0.0f64, None::<f64>);
		while let Some(slot) = schedule.next(None).unwrap() {
			for (position, packet) in slot.layout(&unit(7, 1), 100, &[0xff; 188]).chunks(188).enumerate() {
				let at = slot.pcr as f64 / SYSTEM_CLOCK as f64 + position as f64 * 1504.0 / rate as f64;
				if let Some(previous) = previous {
					fill = (fill - (at - previous) * 125_000.0).max(0.0);
				}
				previous = Some(at);
				if [0, 1, 100].contains(&pid(packet)) {
					fill += 188.0;
				}
				assert!(fill <= 512.001, "TBsys holds {fill} bytes");
			}
		}
	}

	/// A teletext burst at a high multiplex rate keeps within its 480-byte transport buffer,
	/// measured as the receiver sees it: each packet arriving over its time on the wire.
	#[test]
	fn teletext_keeps_within_its_smaller_transport_buffer() {
		let rate = 50_000_000;
		let teletext = Buffer {
			rate: 6_750_000,
			size: None,
			transport: 480,
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
				size: Some(1_000_000),
				transport: 512,
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

	/// A burst the window cannot carry by its due slot fails the export.
	#[test]
	fn a_burst_too_big_for_the_window_fails() {
		let mut schedule = Schedule::new(Duration::from_millis(50));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.push(1, timing(1_000), unit(1, 200), true).unwrap();
		let err = std::iter::from_fn(|| schedule.next(None).transpose()).find_map(Result::err);
		assert!(err.is_some(), "a burst past the window must fail the export");
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
			size: Some(10 * PAYLOAD),
			transport: 512,
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
		// 2 Mb/s: 33 packets a slot, less one.
		let audio = Buffer {
			rate: 2_000_000,
			size: Some(100_000),
			transport: 512,
		};
		schedule.set_buffer(1, audio);
		for at in 0..20 {
			schedule.push(1, timing(1_000 + at), unit(1, 4), false).unwrap();
		}
		assert_eq!(checked_transport(&mut schedule, 9), 80);
	}

	/// The clock packet opening every slot rides its PID's transport buffer too, so that PID
	/// takes one packet fewer of its own.
	#[test]
	fn the_clock_packet_counts_against_its_pid() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		// 2 Mb/s: 33 packets a slot, less one, the clock packet among them.
		let audio = Buffer {
			rate: 2_000_000,
			size: Some(100_000),
			transport: 512,
		};
		schedule.set_buffer(1, audio);
		schedule.set_clock(1);
		for at in 0..20 {
			schedule.push(1, timing(1_000 + at), unit(1, 4), false).unwrap();
		}
		assert_eq!(checked_transport(&mut schedule, 1), 80);
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
			size: Some(100_000),
			transport: 512,
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
				size: Some(100_000),
				transport: 512,
			},
		);
		schedule.push(1, timing(1_000), unit(1, 4), false).unwrap();
		schedule.push(1, timing(1_200), unit(1, 4), false).unwrap();
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
			size: Some(10 * PAYLOAD),
			transport: 512,
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

	/// Packetize a PES with exactly `bytes` elementary bytes and adaptation stuffing.
	fn audio(bytes: usize) -> Vec<u8> {
		let mut pes = vec![0; 14 + bytes];
		pes[..9].copy_from_slice(&[0, 0, 1, 0xbd, 0, 0, 0x80, 0x80, 5]);
		let mut out = Vec::new();
		for (index, body) in pes.chunks(PAYLOAD).enumerate() {
			let mut packet = [0xff; TsPacket::SIZE];
			packet[..4].copy_from_slice(&[0x47, if index == 0 { 0x40 } else { 0 }, 1, 0x10]);
			if body.len() < PAYLOAD {
				packet[3] |= 0x20;
				packet[4] = (PAYLOAD - body.len() - 1) as u8;
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
				size: Some(2_592),
				transport: 512,
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
				size: Some(2 * PAYLOAD),
				transport: 512,
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
					size: Some(2_592),
					transport: 512,
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
		// Unpadded, both go out as soon as they are released, a window ahead.
		let first: Vec<_> = std::iter::from_fn(|| schedule.next(Some(slot(ms(1_100)))).unwrap())
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
		// 2 Mb/s passes 32 packets a slot.
		schedule.set_buffer(
			1,
			Buffer {
				rate: 2_000_000,
				size: Some(100_000),
				transport: 512,
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
			caps: HashMap::new(),
			positions: None,
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
			caps: HashMap::new(),
			positions: None,
		};
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let laid: Vec<u16> = slot.layout(&unit(1, 1), 100, &null).chunks(188).map(pid).collect();
		assert_eq!(laid, [1, 0, 100, 3, 0x1fff, 3, 3, 1, 0x1fff, 3]);
	}

	/// A PAT and a multi-packet PMT go out at TBsys's rate rather than in a burst, spaced by
	/// nulls and still in order ahead of the keyframe muxed after them. The video they delay
	/// always keeps within its own transport buffer: where spacing the tables would leave the
	/// video too little time (a five-packet PMT, or video near its admission cap), the rest of
	/// the tables go out regardless.
	#[test]
	fn the_tables_go_out_at_the_system_buffer_rate() {
		// The peak fill of a 512-byte transport buffer fed by `pids` and drained at `rx` b/s, over
		// a 25 ms slot.
		let peak = |laid: &[&[u8]], pids: &[u16], rx: f64| {
			let drain = rx / 8.0 * 0.025 / laid.len() as f64;
			let (mut tb, mut peak) = (0.0f64, 0.0f64);
			for packet in laid {
				tb = (tb - drain).max(0.0);
				if pids.contains(&pid(packet)) {
					tb += 188.0;
					peak = peak.max(tb);
				}
			}
			peak
		};
		// The video's Rx is 1.2 x 17,999,872 b/s, as in the full-slot test above.
		const RX: u64 = 21_599_846;
		// A 20 Mb/s slot with a small keyframe, and 25 Mb/s slots whose video takes most of it
		// alongside AC-3, MPEG audio and teletext: (positions, video PID and packets, other
		// PIDs, PMT packets, whether TBsys holds).
		let others: &[(u8, usize)] = &[(122, 9), (121, 5), (123, 5), (124, 5), (131, 1)];
		for (positions, video, count, others, parts, paced) in [
			(332, 1, 50, &[][..], 3, true),
			(416, 111, 300, others, 3, true),
			(416, 111, 300, others, 5, false),
			(416, 111, 352, others, 3, false),
		] {
			let mut packets = unit(0, 1);
			for part in 0..parts {
				let mut pmt = unit(100, 1);
				pmt[4] = part;
				packets.extend(pmt);
			}
			packets.extend(unit(video, count));
			for &(pid, count) in others {
				packets.extend(unit(pid, count));
			}
			let slot = Slot {
				index: 0,
				pcr: 0,
				nulls: positions - 1 - packets.len() / TsPacket::SIZE,
				packets,
				keyframe: true,
				units: vec![video.into()],
				caps: HashMap::from([(
					video.into(),
					Buffer {
						rate: RX,
						size: None,
						transport: 512,
					}
					.per_slot(),
				)]),
				positions: None,
			};
			let mut null = unit(0, 1);
			(null[1], null[2]) = (0x1f, 0xff);
			let laid = slot.layout(&unit(video, 1), 100, &null);
			assert_eq!(laid.len(), positions * TsPacket::SIZE, "the slot keeps its size");
			let laid: Vec<&[u8]> = laid.chunks(TsPacket::SIZE).collect();

			let tables: Vec<usize> = (0..laid.len()).filter(|&k| [0, 100].contains(&pid(laid[k]))).collect();
			let order: Vec<u8> = tables.iter().skip(1).map(|&k| laid[k][4]).collect();
			assert_eq!(order, (0..parts).collect::<Vec<_>>(), "the PMT keeps its order");
			let keyframe = (1..laid.len()).find(|&k| pid(laid[k]) == u16::from(video)).unwrap();
			assert!(tables.iter().all(|&k| k < keyframe), "the tables lead the keyframe");

			let system = peak(&laid, &[0, 100], 1_000_000.0);
			assert!(
				!paced || system <= 512.0,
				"TBsys peak {system:.0} B, {parts} PMT packets"
			);
			let video = peak(&laid, &[video.into()], RX as f64);
			assert!(video <= 512.0, "video TB peak {video:.0} B, {parts} PMT packets");
		}
	}

	/// Tables ahead of a second keyframe, after the first's PID has gone out, never stall the
	/// slot: laying it afresh would drop the spacing that PID already has.
	#[test]
	fn tables_after_media_leave_its_spacing_alone() {
		let mut packets = unit(0, 1);
		packets.extend(unit(100, 1));
		packets.extend(unit(1, 125));
		packets.extend(unit(0, 1));
		packets.extend(unit(100, 1));
		packets.extend(unit(2, 114));
		let (a, b) = (
			Buffer {
				rate: 7_833_600,
				size: None,
				transport: 512,
			},
			Buffer {
				rate: 8_160_000,
				size: None,
				transport: 512,
			},
		);
		let slot = Slot {
			index: 0,
			pcr: 0,
			packets,
			nulls: 88,
			keyframe: true,
			units: vec![1, 2],
			caps: HashMap::from([(1, a.per_slot()), (2, b.per_slot())]),
			positions: None,
		};
		let mut null = unit(0, 1);
		(null[1], null[2]) = (0x1f, 0xff);
		let laid = slot.layout(&unit(1, 1), 100, &null);
		let laid: Vec<&[u8]> = laid.chunks(TsPacket::SIZE).collect();
		assert_eq!(laid.len(), 332);
		for (media, buffer) in [(1, a), (2, b)] {
			let drain = buffer.rate as f64 / 8.0 * 0.025 / laid.len() as f64;
			let (mut tb, mut peak) = (0.0f64, 0.0f64);
			for packet in &laid {
				tb = (tb - drain).max(0.0);
				if pid(packet) == media {
					tb += 188.0;
					peak = peak.max(tb);
				}
			}
			assert!(peak <= 512.0, "PID {media} TB peak {peak:.0} B");
		}
	}
}
