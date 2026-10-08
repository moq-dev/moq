//! Lays a TS export's packets onto its PCR grid.

use std::collections::{HashMap, VecDeque};
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
}

impl Buffer {
	/// The program tables' (ISO 13818-1 2.4.2.3, Rxsys).
	pub const SYSTEM: Self = Self {
		rate: 1_000_000,
		size: None,
	};

	/// The packets one slot may carry on the PID: what its transport buffer passes on within
	/// the slot, less one, since the layout spreads them across it.
	fn per_slot(&self) -> usize {
		let bits = u128::from(self.rate) * PCR_INTERVAL.as_nanos() / 1_000_000_000;
		(bits / (TsPacket::SIZE as u128 * 8)).saturating_sub(1).max(1) as usize
	}
}

/// One access unit's packets, with the tables muxed ahead of it, still to go out.
struct Unit {
	/// The elementary stream it carries, whose buffers bound it.
	pid: u16,
	/// The last slot its packets may ride: the one that ends by its decode time.
	due: u128,
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
}

impl Slot {
	/// The slot's packets after its clock packet: the media spread evenly among the null
	/// packets, and each PID's packets spread evenly among the others, every PID keeping its
	/// own order.
	///
	/// A low-rate stream muxed whole, like an audio frame's few packets or two frames back to
	/// back, would otherwise arrive at the multiplex's full rate and overflow its 512-byte
	/// transport buffer, which drains at 2 Mb/s (ISO 13818-1 2.4.2.3). The program tables (PAT
	/// and the PMT on `pmt_pid`) move ahead of every packet muxed after them, so they still
	/// lead the keyframe they were written for and a reader knows each PID before its first
	/// packet.
	pub fn layout(&self, pmt_pid: u16, null: &[u8]) -> Vec<u8> {
		let packets: Vec<&[u8]> = self
			.packets
			.as_chunks::<{ TsPacket::SIZE }>()
			.0
			.iter()
			.map(|p| p.as_slice())
			.collect();
		let mut counts: HashMap<u16, u64> = HashMap::new();
		for packet in &packets {
			*counts.entry(pid(packet)).or_default() += 1;
		}

		// The k-th of a PID's n packets sits at (2k + 1) / 2n of the way through.
		let mut seen: HashMap<u16, u64> = HashMap::new();
		let mut keys: Vec<(u64, u64)> = packets
			.iter()
			.map(|packet| {
				let pid = pid(packet);
				let k = seen.entry(pid).or_default();
				*k += 1;
				(2 * *k - 1, 2 * counts[&pid])
			})
			.collect();
		let cmp = |(an, ad): (u64, u64), (bn, bd): (u64, u64)| {
			(u128::from(an) * u128::from(bd)).cmp(&(u128::from(bn) * u128::from(ad)))
		};
		let mut after = (1, 1);
		for (packet, key) in packets.iter().zip(keys.iter_mut()).rev() {
			let pid = pid(packet);
			if pid == 0 || pid == pmt_pid {
				*key = after;
			} else if cmp(*key, after).is_lt() {
				after = *key;
			}
		}
		let mut order: Vec<usize> = (0..packets.len()).collect();
		order.sort_by(|&a, &b| cmp(keys[a], keys[b]));

		// Media packet i of k lands at (2i + 1) / 2k of the way through, nulls in between.
		let (k, total) = (order.len(), order.len() + self.nulls);
		let mut out = Vec::with_capacity(total * TsPacket::SIZE);
		let mut next = 0;
		for at in 0..total {
			if next < k && at == (2 * next + 1) * total / (2 * k) {
				out.extend_from_slice(packets[order[next]]);
				next += 1;
			} else {
				out.extend_from_slice(null);
			}
		}
		out
	}
}

/// Packs muxed access units onto the PCR grid.
///
/// Each slot's packets go earliest deadline first across PIDs, each PID in its own order, and
/// each unit as soon as its PID's buffers in a receiver admit it: no more of a PID's packets
/// in a slot than its transport buffer passes on, and no more bytes than its decoder buffer
/// has room for until the units in it decode. A unit may go out up to the `window` before
/// its due slot, which keeps that far ahead of a heavy passage, so the output trails the
/// decode times by the window.
///
/// With a multiplex rate each slot carries exactly what the rate allows, padded with null
/// packets, and a unit that is not complete by its due slot fails the export rather than
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
	/// Units sent into a decoder buffer: their PID, due slot and bytes held.
	decoding: VecDeque<(u16, u128, usize)>,
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
			decoding: VecDeque::new(),
			ended: false,
		}
	}

	/// Every source has ended: what is queued still goes out, late if it must.
	pub fn end(&mut self) {
		self.ended = true;
	}

	/// Pad to `rate` bits per second from the next slot on, or stop padding.
	pub fn set_rate(&mut self, rate: Option<u64>) {
		self.rate = rate;
	}

	/// Hold the packets on `pid` to `buffer`.
	pub fn set_buffer(&mut self, pid: u16, buffer: Buffer) {
		self.buffers.insert(pid, buffer);
	}

	/// Queue an access unit on `pid` (whole TS packets, the tables muxed ahead of it first)
	/// whose last byte has to arrive by `by` nanoseconds: its decode time, less what the
	/// receiver needs to pass the last packet on.
	pub fn push(&mut self, pid: u16, by: u128, packets: Vec<u8>, keyframe: bool) {
		let due = slot(by);
		self.horizon = self.horizon.max(Some(due));
		let start = self.last.map_or(due, |last| (last + 1).min(due));
		self.last = Some(due);
		self.units.push_back(Unit {
			pid,
			due,
			start,
			packets,
			sent: 0,
			keyframe,
			late: self.next.is_some_and(|next| due < next),
			held: 0,
		});
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

		let (take, nulls, pcr) = match self.rate {
			Some(rate) => {
				let allowed = (packets_before(index + 1, rate) - packets_before(index, rate)).max(1);
				let take = self.admit(index, allowed as usize - 1);
				if let Some(unit) = self.missed(index, &take)
					&& !self.ended
				{
					anyhow::bail!(
						"MPEG-TS output missed a decode deadline on PID {} at {rate} b/s; raise the delay or the multiplex rate",
						unit.pid,
					);
				}
				let media: usize = take.iter().sum();
				(take, allowed as usize - 1 - media, rate_pcr(index, rate))
			}
			None => {
				let mut take = self.admit(index, usize::MAX);
				for (unit, take) in self.units.iter_mut().zip(take.iter_mut()) {
					if unit.due <= index && *take < unit.remaining() {
						if self.buffers.get(&unit.pid).is_some_and(|buffer| buffer.size.is_some()) {
							unit.held += unit.packets_on(unit.sent + *take..unit.count(), unit.pid) * PAYLOAD;
						}
						*take = unit.remaining();
					}
				}
				(take, 0, grid_pcr(index))
			}
		};

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
			self.decoding.push_back((unit.pid, unit.due, unit.held));
		}
		self.units.retain(|unit| unit.remaining() > 0);
		Ok(Some(Slot {
			index,
			pcr: pcr as u64,
			packets,
			nulls,
			keyframe,
			units,
		}))
	}

	/// How many of each unit's packets slot `index` carries, at most `budget` in all.
	///
	/// Earliest deadline first across PIDs. A PID whose unit stops short (its buffers full,
	/// or not yet released) takes nothing more this slot, so each keeps its own order.
	fn admit(&mut self, index: u128, mut budget: usize) -> Vec<usize> {
		// A unit due in slot k decodes during slot k + 1, so its bytes leave the decoder
		// buffer once that slot is past.
		let mut occupancy: HashMap<u16, usize> = HashMap::new();
		self.decoding.retain(|&(_, due, _)| due + 1 >= index);
		for &(pid, _, held) in &self.decoding {
			*occupancy.entry(pid).or_default() += held;
		}
		for unit in &self.units {
			*occupancy.entry(unit.pid).or_default() += unit.held;
		}

		let mut order: Vec<usize> = (0..self.units.len()).collect();
		order.sort_by_key(|&i| self.units[i].due);
		let mut take = vec![0; self.units.len()];
		let mut carried: HashMap<u16, usize> = HashMap::new();
		let mut stopped: Vec<u16> = Vec::new();
		for i in order {
			let unit = &self.units[i];
			if budget == 0 {
				break;
			}
			if stopped.contains(&unit.pid) {
				continue;
			}
			// Once every source has ended, a unit past its deadline goes out whatever its decoder
			// buffer holds: it is late already, and one bigger than the buffer would otherwise
			// never finish, holding the end of the stream open on clock packets.
			let size = self
				.buffers
				.get(&unit.pid)
				.and_then(|buffer| buffer.size)
				.filter(|_| !(self.ended && unit.due < index));
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

	/// The first unit due by slot `index` that `take` leaves incomplete, unless it was already
	/// late when pushed.
	fn missed(&self, index: u128, take: &[usize]) -> Option<&Unit> {
		self.units
			.iter()
			.zip(take)
			.find(|(unit, take)| !unit.late && unit.due <= index && unit.sent + **take < unit.count())
			.map(|(unit, _)| unit)
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

	fn ms(ms: u128) -> u128 {
		ms * 1_000_000
	}

	/// 40 packets per slot, the clock packet included.
	const RATE: u64 = 40 * PACKET;
	/// A video-like buffer: far more than a slot passes on, and a large decoder buffer.
	const VIDEO: Buffer = Buffer {
		rate: 100_000_000,
		size: Some(1_000_000),
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

	/// A unit goes out a window ahead of its due slot, as soon as it is released.
	#[test]
	fn a_unit_goes_out_as_soon_as_it_is_released() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.push(1, ms(1_000), unit(1, 3), true);
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
		schedule.push(1, ms(1_000), unit(1, 100), true);
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
		schedule.push(1, ms(1_000), unit(1, 200), true);
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
		schedule.push(1, ms(1_000), unit(1, 200), true);
		schedule.end();
		let sent: usize = drain(&mut schedule)
			.into_iter()
			.map(|(_, per_pid, _)| per_pid.get(&1).copied().unwrap_or(0))
			.sum();
		assert_eq!(sent, 200, "the whole unit goes out");
	}

	/// Once every source has ended, a last unit bigger than its decoder buffer still goes out,
	/// late, and the stream ends rather than run on with clock packets.
	#[test]
	fn a_last_unit_bigger_than_its_buffer_ends_the_stream() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		// Room for ten packets.
		let small = Buffer {
			rate: 100_000_000,
			size: Some(10 * PAYLOAD),
		};
		schedule.set_buffer(1, small);
		schedule.push(1, ms(1_000), unit(1, 11), true);
		schedule.end();
		let mut sent = 0;
		for _ in 0..100 {
			let Some(slot) = schedule.next(None).unwrap() else {
				break;
			};
			sent += slot.packets.len() / TsPacket::SIZE;
		}
		assert_eq!(sent, 11, "the whole unit goes out");
		assert!(schedule.is_empty(), "and the stream ends");
	}

	/// The unit due first goes first, whatever order the PIDs were pushed in.
	#[test]
	fn the_earliest_deadline_goes_first() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.set_buffer(1, VIDEO);
		schedule.set_buffer(2, VIDEO);
		schedule.push(1, ms(1_100), unit(1, 39), true);
		schedule.push(2, ms(1_000), unit(2, 39), false);
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
		};
		schedule.set_buffer(1, audio);
		for at in 0..20 {
			schedule.push(1, ms(1_000 + at), unit(1, 4), false);
		}
		for (index, per_pid, _) in drain(&mut schedule) {
			assert!(per_pid.get(&1).copied().unwrap_or(0) <= 32, "slot {index}: {per_pid:?}");
		}
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
		};
		schedule.set_buffer(1, small);
		schedule.push(1, ms(1_000), unit(1, 8), true);
		schedule.push(1, ms(1_100), unit(1, 8), false);
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.filter_map(|(index, per_pid, _)| per_pid.get(&1).map(|&n| (index, n)))
			.collect();
		// The first unit goes at once; the second fills what is left, then the rest once the
		// first decodes in slot 41.
		assert_eq!(sent, [(32, 8), (36, 2), (42, 6)]);
	}

	/// A PID with no decoder buffer to respect goes out only in the slot before its own.
	#[test]
	fn an_unbounded_pid_goes_just_before_it_is_due() {
		let mut schedule = Schedule::new(Duration::from_millis(100));
		schedule.set_rate(Some(RATE));
		schedule.push(7, ms(1_000), unit(7, 2), false);
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
			schedule.push(1, ms(1_000 + 25 * at), unit(1, 1), false);
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
		schedule.push(1, ms(1_000), unit(1, 8), true);
		schedule.push(1, ms(1_100), unit(1, 100), false);
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
			},
		);
		schedule.push(1, ms(1_000), unit(1, 50), true);
		schedule.push(1, ms(1_100), unit(1, 4), false);
		let sent: Vec<_> = drain(&mut schedule)
			.into_iter()
			.map(|(index, per_pid, nulls)| (index, per_pid.get(&1).copied().unwrap_or(0), nulls))
			.collect();
		assert_eq!(sent, [(40, 50, 0), (41, 4, 0), (42, 0, 0), (43, 0, 0), (44, 0, 0)]);
	}
}
