//! Fixed lanes on a constant-rate multiplex, so every exporter of a broadcast writes the same
//! packet at the same position ([`Export::with_sync`](super::Export::with_sync)).
//!
//! At a constant rate, slot `index` carries the timeline's packets from
//! `packets_before(index)` up to `packets_before(index + 1)`. Each PID owns a fixed share of
//! them that is a function of the slot alone: the slot's PCR its first position, the PAT and
//! PMT theirs in every slot that repeats them, and each elementary stream a lane of so many
//! packets a second, spread evenly over the rest. The positions no lane owns carry the SI,
//! else stuffing.
//!
//! A unit takes the next positions of its PID's lane from where its MoQ group was anchored:
//! the first slot the group's first unit may go out in. Where it lands depends on its group's
//! earlier units and nothing else, so a leg that lacks a group, or joined after it, lays the
//! next one where every other leg does. A group still unfinished in its lane when the next
//! one is due pushes the next one back, which only a leg holding both does. A lane needs the
//! headroom to carry each group within its own duration, unless the next group's first picture
//! opens an H.264 buffering period: the export then releases it late enough for its HRD to rule
//! that out.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use mpeg2ts::ts::TsPacket;

use super::export::{PCR_INTERVAL, PSI_INTERVAL};
use super::schedule::{PACKET, packets_before};

/// Grid slots per second.
const SLOTS: u64 = (1_000_000_000 / PCR_INTERVAL.as_nanos()) as u64;
/// How many slots apart the PAT and PMT repeat.
pub(super) const PSI_SLOTS: u128 = PSI_INTERVAL.as_nanos() / PCR_INTERVAL.as_nanos();
/// How fast a system PID's transport buffer drains (Rxsys, ISO 13818-1 2.4.2.3).
const RX_SYSTEM: u64 = 1_000_000;
/// How far a restarted grid's first packet may sit behind the last slot laid, or ahead of it,
/// and still be on the same timeline, so the grid runs on rather than jumps: two seconds back,
/// a minute on.
const BEHIND: u128 = 2 * SLOTS as u128;
const AHEAD: u128 = 60 * SLOTS as u128;

/// What a position of a slot carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owner {
	/// The slot's PCR, on its own PID.
	Clock,
	/// The `n`th packet of the PAT and PMT, sent in a slot that repeats them and in the first a
	/// leg lays, stuffing in the rest.
	Table(usize),
	/// A packet of the `n`th lane.
	Lane(usize),
	/// The SI, else stuffing.
	Free,
}

/// The share of the multiplex a PID's lane takes.
#[derive(Clone, Copy, Debug)]
pub(super) enum Need {
	/// So many packets a second.
	Fixed(u64),
	/// What the fixed lanes leave, split evenly with the other remainders, at most so many
	/// packets a second.
	Rest(u64),
}

/// The positions every PID owns, slot by slot, at one multiplex rate.
#[derive(Debug)]
pub(super) struct Layout {
	rate: u64,
	/// Each lane's PID and packets a second.
	lanes: Vec<(u16, u64)>,
	/// Where the PAT and the PMT's packets sit in every slot.
	tables: Vec<usize>,
	/// How many positions apart a system PID's packets go, so they arrive no faster than its
	/// transport buffer drains.
	spacing: usize,
}

impl Layout {
	/// Lay `lanes` out at `rate` bits per second beside `tables` packets of PAT and PMT, keeping
	/// `reserve` packets a second free for the SI.
	pub fn new(rate: u64, tables: usize, reserve: u64, lanes: &[(u16, Need)]) -> anyhow::Result<Self> {
		let spacing = rate.div_ceil(RX_SYSTEM).max(1) as usize;
		// The fewest packets any slot carries at the rate.
		let least = rate / PACKET;
		anyhow::ensure!(
			(1 + tables as u64) * spacing as u64 <= least,
			"MPEG-TS --sync at {rate} b/s has too few packets a slot to pace its {tables} table packets; raise --mux-rate"
		);
		let per_slot = |packets: u64| packets.div_ceil(SLOTS);
		let fixed: u64 = lanes
			.iter()
			.map(|(_, need)| match need {
				Need::Fixed(packets) => per_slot(*packets),
				Need::Rest(_) => 0,
			})
			.sum();
		let taken = 1 + tables as u64 + fixed + per_slot(reserve);
		let rests = lanes.iter().filter(|(_, need)| matches!(need, Need::Rest(_))).count() as u64;
		let left = least.checked_sub(taken).filter(|left| rests == 0 || *left >= rests);
		let Some(left) = left else {
			anyhow::bail!(
				"MPEG-TS --sync at {rate} b/s cannot carry its lanes: the PCR, the tables and each non-video stream at its codec's \
				 most take {} b/s, leaving nothing for video; raise --mux-rate",
				taken * PACKET
			);
		};
		let lanes = lanes
			.iter()
			.map(|&(pid, need)| {
				let packets = match need {
					Need::Fixed(packets) => packets,
					Need::Rest(most) => (left / rests * SLOTS).min(most),
				};
				(pid, packets)
			})
			.collect();
		let tables = (1..=tables).map(|n| n * spacing).collect();
		Ok(Self {
			rate,
			lanes,
			tables,
			spacing,
		})
	}

	pub fn rate(&self) -> u64 {
		self.rate
	}

	/// The lane `pid` owns, if any.
	pub fn lane(&self, pid: u16) -> Option<usize> {
		self.lanes.iter().position(|&(lane, _)| lane == pid)
	}

	/// How fast lane `lane` carries elementary bytes, in bits per second.
	pub fn payload_rate(&self, lane: usize) -> u64 {
		self.lanes[lane].1 * (TsPacket::SIZE as u64 - 4) * 8
	}

	/// The timeline's first packet in slot `index`.
	pub fn first(&self, index: u128) -> u128 {
		packets_before(index, self.rate)
	}

	/// The timeline's first packet whose bytes begin at or after `time` nanoseconds of media
	/// time, in the slot that time falls in.
	pub fn at(&self, time: u128) -> u128 {
		(time * u128::from(self.rate)).div_ceil(TsPacket::SIZE as u128 * 8 * 1_000_000_000)
	}

	/// How many packets slot `index` carries.
	pub fn count(&self, index: u128) -> usize {
		(self.first(index + 1) - self.first(index)) as usize
	}

	/// The slot that carries the timeline's packet `position`.
	pub fn slot_of(&self, position: u128) -> u128 {
		((position + 1) * u128::from(PACKET)).div_ceil(u128::from(self.rate)) - 1
	}

	/// When the timeline's packet `position` begins to arrive, in nanoseconds of media time: its
	/// byte position at the rate, one slot behind like every PCR.
	pub fn time(&self, position: u128) -> u128 {
		(position * TsPacket::SIZE as u128 * 8 * 1_000_000_000 / u128::from(self.rate))
			.saturating_sub(PCR_INTERVAL.as_nanos())
	}

	/// How many packets lane `lane` takes in slot `index`, counted from the start of the
	/// timeline so no slot rounds on its own.
	fn lane_count(&self, lane: usize, index: u128) -> usize {
		let packets = u128::from(self.lanes[lane].1);
		let before = |index: u128| index * packets / u128::from(SLOTS);
		(before(index + 1) - before(index)) as usize
	}

	/// Whether slot `index` repeats the PAT and PMT, on the absolute grid every exporter shares.
	pub fn repeats_tables(index: u128) -> bool {
		index.is_multiple_of(PSI_SLOTS)
	}

	/// What each position of slot `index` carries. The clock takes the first, the tables
	/// theirs, and the lanes share the rest by smooth weighted round-robin with the free
	/// positions: each takes its count as evenly as it can, in PID order where two tie.
	pub fn owners(&self, index: u128) -> Vec<Owner> {
		let count = self.count(index);
		let mut owners = vec![Owner::Free; count];
		owners[0] = Owner::Clock;
		for (n, &position) in self.tables.iter().enumerate() {
			owners[position] = Owner::Table(n);
		}
		let open: Vec<usize> = (1..count).filter(|&p| owners[p] == Owner::Free).collect();
		let mut weights: Vec<i64> = (0..self.lanes.len())
			.map(|lane| self.lane_count(lane, index) as i64)
			.collect();
		let used: i64 = weights.iter().sum();
		weights.push(open.len() as i64 - used);
		debug_assert!(
			weights.last().is_some_and(|free| *free >= 0),
			"lanes overflow slot {index}"
		);
		let total = open.len() as i64;
		let mut credit = vec![0i64; weights.len()];
		for position in open {
			let mut pick = 0;
			for lane in 0..credit.len() {
				credit[lane] += weights[lane];
				if credit[lane] > credit[pick] {
					pick = lane;
				}
			}
			credit[pick] -= total;
			if pick < self.lanes.len() {
				owners[position] = Owner::Lane(pick);
			}
		}
		owners
	}

	/// `release` moved later by as long as lane `lane` takes to carry `bits` of payload, unless a
	/// unit of `count` packets laid from there would arrive after `by`.
	pub fn later(&self, lane: usize, release: u128, bits: u64, by: u128, count: usize) -> u128 {
		let later = release + u128::from(bits) * 1_000_000_000 / u128::from(self.payload_rate(lane).max(1));
		let positions = self.take(lane, self.at(later), count.max(1));
		let last = *positions.last().expect("at least one position");
		match self.time(last + 1) > by {
			true => release,
			false => later,
		}
	}

	/// The next `count` positions of lane `lane` at or after the timeline's packet `from`.
	pub fn take(&self, lane: usize, from: u128, count: usize) -> Vec<u128> {
		let mut out = Vec::with_capacity(count);
		let mut index = self.slot_of(from);
		while out.len() < count {
			let first = self.first(index);
			for (position, owner) in self.owners(index).into_iter().enumerate() {
				let at = first + position as u128;
				if at >= from && owner == Owner::Lane(lane) {
					out.push(at);
					if out.len() == count {
						break;
					}
				}
			}
			index += 1;
		}
		out
	}
}

/// What [`Lane::plan`] gave a unit.
pub(super) struct Placed {
	/// The timeline position of each of its packets.
	pub positions: Vec<u128>,
	/// The continuity counter of its first packet: how many packets its group laid before it.
	pub counter: u8,
}

/// Why [`Lane::plan`] placed nothing.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unplaced {
	/// This leg did not hold the unit's group from its first unit, so it cannot know where
	/// the unit goes.
	Partial,
	/// The lane cannot carry the unit by its deadline.
	Late,
}

/// A PID's place in its lane.
pub(super) struct Lane {
	lane: usize,
	/// The group being laid.
	group: Option<Group>,
	/// One past the last position any group took.
	end: u128,
	/// Groups that began behind the one before them.
	overruns: u64,
}

struct Group {
	sequence: u64,
	/// The next position it may take.
	next: u128,
	/// How many packets it has laid.
	sent: usize,
	/// This leg holds it from its first unit.
	whole: bool,
}

impl Lane {
	pub fn new(lane: usize) -> Self {
		Self {
			lane,
			group: None,
			end: 0,
			overruns: 0,
		}
	}

	/// Which lane of its [`Layout`] it is.
	pub fn index(&self) -> usize {
		self.lane
	}

	/// Groups that began behind the one before them, on a lane short of headroom.
	pub fn overruns(&self) -> u64 {
		self.overruns
	}

	/// Forget every group, for a timeline that starts again.
	pub fn reset(&mut self) {
		self.group = None;
		self.end = 0;
	}

	/// Whether a unit of group `sequence`, its first when `opens`, would be the first its group
	/// places, so its first packet flags the break in the continuity counter.
	pub fn opening(&self, (sequence, opens): (u64, bool)) -> bool {
		match &self.group {
			Some(group) if group.sequence == sequence => group.whole && group.sent == 0,
			_ => opens,
		}
	}

	/// Place a unit of `count` packets from group `sequence`, which it `opens` when it is the
	/// group's first unit, that may go out from `release` and must have arrived by `by`,
	/// both in nanoseconds of media time.
	///
	/// A group opens at its lane's first position from its first unit's release, and each unit
	/// after it takes the next positions from there, never before its own release. A unit that
	/// misses its deadline takes nothing, so the units after it keep theirs.
	pub fn plan(
		&mut self,
		layout: &Layout,
		(sequence, opens): (u64, bool),
		release: u128,
		by: u128,
		count: usize,
	) -> Result<Placed, Unplaced> {
		let earliest = layout.at(release);
		let from = match &mut self.group {
			Some(group) if group.sequence == sequence => match group.whole {
				true => group.next.max(earliest),
				false => return Err(Unplaced::Partial),
			},
			_ if !opens => {
				self.group = Some(Group {
					sequence,
					next: 0,
					sent: 0,
					whole: false,
				});
				return Err(Unplaced::Partial);
			}
			_ => {
				if self.end > earliest {
					self.overruns += 1;
				}
				self.group = Some(Group {
					sequence,
					next: self.end.max(earliest),
					sent: 0,
					whole: true,
				});
				self.end.max(earliest)
			}
		};
		let positions = layout.take(self.lane, from, count.max(1));
		let last = *positions.last().expect("at least one position");
		if layout.time(last + 1) > by {
			return Err(Unplaced::Late);
		}
		let group = self.group.as_mut().expect("a group was just set");
		let counter = (group.sent & 0x0f) as u8;
		group.next = last + 1;
		group.sent += count;
		self.end = self.end.max(last + 1);
		Ok(Placed { positions, counter })
	}
}

/// Number `packets` on from `counter`, every one carrying a payload.
pub(super) fn number(packets: &mut [u8], counter: u8) {
	for (n, packet) in packets.as_chunks_mut::<{ TsPacket::SIZE }>().0.iter_mut().enumerate() {
		packet[3] = (packet[3] & 0xf0) | ((counter as usize + n) & 0x0f) as u8;
	}
}

/// The PID of a TS packet.
fn pid(packet: &[u8]) -> u16 {
	u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2])
}

/// One slot as [`Grid::lay`] fills it: its packets at their positions in it, the clock's left
/// to the caller.
pub(super) struct Laid {
	pub packets: Vec<u8>,
	pub positions: Vec<usize>,
	pub count: usize,
	pub keyframe: bool,
	pub units: Vec<u16>,
	/// It does not follow the slot laid before it.
	pub jump: bool,
}

/// The packets placed on a [`Layout`], laid out slot by slot.
pub(super) struct Grid {
	layout: Arc<Layout>,
	/// Each placed packet, by its timeline position.
	placed: BTreeMap<u128, [u8; TsPacket::SIZE]>,
	/// Where each unit's first packet sits: its PID and whether it opens a keyframe.
	starts: BTreeMap<u128, (u16, bool)>,
	/// The PAT then the PMT, numbered as they go out.
	tables: Vec<[u8; TsPacket::SIZE]>,
	/// SI packets still to go out, in order, each no earlier than its position.
	si: VecDeque<(u128, [u8; TsPacket::SIZE])>,
	/// Where each SI PID's last packet went.
	si_last: HashMap<u16, u128>,
	/// The next slot to lay.
	next: Option<u128>,
	/// The last slot laid, kept through a restart.
	last: Option<u128>,
	/// Packets that reached the grid after their position went out.
	late: u64,
}

impl Grid {
	pub fn new(layout: Arc<Layout>, tables: Vec<[u8; TsPacket::SIZE]>) -> Self {
		Self {
			layout,
			placed: BTreeMap::new(),
			starts: BTreeMap::new(),
			tables,
			si: VecDeque::new(),
			si_last: HashMap::new(),
			next: None,
			last: None,
			late: 0,
		}
	}

	pub fn layout(&self) -> &Arc<Layout> {
		&self.layout
	}

	/// Packets that reached the grid after their position went out.
	pub fn late(&self) -> u64 {
		self.late
	}

	/// Put a unit's `packets` at their `positions`, dropping any whose position went out.
	pub fn place(&mut self, packets: &[u8], positions: &[u128], keyframe: bool) {
		let gone = self.next.map_or(0, |next| self.layout.first(next));
		for (n, (packet, &at)) in packets
			.as_chunks::<{ TsPacket::SIZE }>()
			.0
			.iter()
			.zip(positions)
			.enumerate()
		{
			if at < gone {
				self.late += 1;
				continue;
			}
			if n == 0 {
				self.starts.insert(at, (pid(packet), keyframe));
			}
			self.placed.insert(at, *packet);
		}
	}

	/// Send `packets` of SI from slot `index` on, in the positions no lane owns.
	pub fn push_si(&mut self, index: u128, packets: &[u8]) {
		let from = self.layout.first(index);
		for packet in packets.as_chunks::<{ TsPacket::SIZE }>().0 {
			self.si.push_back((from, *packet));
		}
	}

	/// Whether anything placed is still to go out.
	pub fn is_empty(&self) -> bool {
		self.placed.is_empty() && self.si.is_empty()
	}

	/// The slot [`Self::lay`] lays next: on from the last, else the one before the first holding
	/// a packet, so the tables a timeline starts with come before anything of it, unless a
	/// restarted grid's timeline runs on from the slot laid last.
	pub fn index(&self) -> Option<u128> {
		self.next.or_else(|| {
			let first = self.layout.slot_of(*self.placed.keys().next()?);
			Some(match self.last {
				Some(last) if first + BEHIND > last && first <= last + AHEAD => last + 1,
				_ => first.saturating_sub(1),
			})
		})
	}

	/// Drop everything placed. The grid runs on from where it was: its positions are the
	/// slot's alone.
	pub fn clear(&mut self) {
		self.placed.clear();
		self.starts.clear();
		self.si.clear();
	}

	/// Start the grid afresh at the next slot holding a packet, for a timeline that restarted.
	pub fn restart(&mut self) {
		self.clear();
		self.next = None;
		self.si_last.clear();
	}

	/// Lay slot `index`: every placed packet at its position, the tables at theirs where the
	/// slot repeats them or starts a timeline, the SI in order in the free positions, paced
	/// like the tables.
	///
	/// Tables that start a timeline number as the last repetition before them did, so a
	/// receiver tuning in on them counts on into the next.
	pub fn lay(&mut self, index: u128) -> Laid {
		self.next = Some(index + 1);
		let jump = self.last.is_some_and(|last| index != last + 1);
		let tables = Layout::repeats_tables(index) || jump || self.last.is_none();
		self.last = Some(index);
		let first = self.layout.first(index);
		let owners = self.layout.owners(index);
		let count = owners.len();
		let mut out = Laid {
			packets: Vec::new(),
			positions: Vec::new(),
			count,
			keyframe: false,
			units: Vec::new(),
			jump,
		};
		let emission = index / PSI_SLOTS;
		let spacing = self.layout.spacing as u128;
		for (position, owner) in owners.into_iter().enumerate() {
			let at = first + position as u128;
			let packet = match owner {
				Owner::Clock => None,
				Owner::Table(n) => self.tables.get(n).filter(|_| tables).map(|table| {
					let mut packet = *table;
					// The PAT is one packet; the PMT's packets follow it, numbered on from the
					// last emission's on the absolute grid.
					let counter = match n {
						0 => emission,
						n => emission * (self.tables.len() as u128 - 1) + n as u128 - 1,
					};
					packet[3] = (packet[3] & 0xf0) | (counter & 0x0f) as u8;
					packet
				}),
				Owner::Lane(_) => self.placed.remove(&at),
				Owner::Free => {
					let ready = self.si.front().is_some_and(|(from, packet)| {
						*from <= at && self.si_last.get(&pid(packet)).is_none_or(|last| *last + spacing <= at)
					});
					ready.then(|| {
						let (_, packet) = self.si.pop_front().expect("just peeked");
						self.si_last.insert(pid(&packet), at);
						packet
					})
				}
			};
			if let Some((pid, keyframe)) = self.starts.remove(&at) {
				out.keyframe |= keyframe;
				out.units.push(pid);
			}
			if let Some(packet) = packet {
				out.positions.push(position);
				out.packets.extend_from_slice(&packet);
			}
		}
		// A packet left behind its position (one placed into the slot just laid by a lane that
		// no longer owns it) never goes out.
		let end = first + count as u128;
		self.placed = self.placed.split_off(&end);
		self.starts = self.starts.split_off(&end);
		out
	}
}

/// When a unit due by `by` nanoseconds of media time may first go out, `ahead` slots before.
pub(super) fn release(by: u128, ahead: u128) -> u128 {
	by.saturating_sub(ahead * PCR_INTERVAL.as_nanos())
}

#[cfg(test)]
mod test {
	use super::*;

	const RATE: u64 = 34 * 188 * 8 * 40;

	fn layout() -> Layout {
		Layout::new(
			RATE,
			2,
			0,
			&[
				(0x100, Need::Rest(u64::MAX)),
				(0x101, Need::Fixed(422)),
				(0x102, Need::Fixed(40)),
			],
		)
		.unwrap()
	}

	#[test]
	fn every_slot_gives_each_lane_its_share() {
		let layout = layout();
		for index in 0..400u128 {
			let owners = layout.owners(index);
			assert_eq!(owners.len(), 34);
			assert_eq!(owners[0], Owner::Clock);
			for lane in 0..3 {
				let got = owners.iter().filter(|owner| **owner == Owner::Lane(lane)).count();
				assert_eq!(got, layout.lane_count(lane, index), "lane {lane} in slot {index}");
			}
		}
		let total: usize = (0..40).map(|index| layout.lane_count(1, index)).sum();
		assert_eq!(total, 422, "a second carries its packets");
	}

	fn tables() -> Vec<[u8; TsPacket::SIZE]> {
		let mut pat = [0xffu8; TsPacket::SIZE];
		pat[..4].copy_from_slice(&[0x47, 0x40, 0x00, 0x10]);
		let mut pmt = pat;
		pmt[1..3].copy_from_slice(&[0x50, 0x00]);
		vec![pat, pmt]
	}

	#[test]
	fn the_tables_repeat_on_the_absolute_grid_and_lead_a_timeline() {
		let layout = layout();
		for index in [0u128, 20, 21, 40, 1_000_000_020] {
			let owners = layout.owners(index);
			assert_eq!(owners[layout.tables[0]], Owner::Table(0));
			assert_eq!(owners[layout.tables[1]], Owner::Table(1));
		}
		let mut grid = Grid::new(Arc::new(layout), tables());
		let mut sent = Vec::new();
		for index in 21..=41 {
			let laid = grid.lay(index);
			for packet in laid.packets.as_chunks::<{ TsPacket::SIZE }>().0 {
				sent.push((index, pid(packet), packet[3] & 0x0f));
			}
		}
		// The first slot laid leads with them, numbered as slot 20's were, so slot 40's count on.
		assert_eq!(
			sent,
			[(21, 0x0000, 1), (21, 0x1000, 1), (40, 0x0000, 2), (40, 0x1000, 2)]
		);
	}

	#[test]
	fn a_rate_too_low_for_the_lanes_is_refused() {
		let err = Layout::new(
			RATE,
			2,
			0,
			&[(0x100, Need::Rest(u64::MAX)), (0x101, Need::Fixed(40 * 40))],
		)
		.unwrap_err();
		assert!(err.to_string().contains("raise --mux-rate"), "{err}");
	}

	#[test]
	fn positions_map_back_to_their_slots() {
		let layout = Layout::new(10_000_000, 2, 0, &[(0x100, Need::Rest(u64::MAX))]).unwrap();
		for index in [0u128, 1, 7, 39, 40, 123_456_789] {
			let first = layout.first(index);
			assert_eq!(layout.slot_of(first), index);
			assert_eq!(layout.slot_of(first + layout.count(index) as u128 - 1), index);
		}
	}

	/// The start of slot `index`, in nanoseconds of media time.
	fn start(index: u128) -> u128 {
		index * PCR_INTERVAL.as_nanos()
	}

	#[test]
	fn a_group_lays_from_its_own_anchor_whatever_came_before() {
		let layout = layout();
		let by = 10_000_000_000;
		let mut whole = Lane::new(0);
		let mut joiner = Lane::new(0);
		// Group 1, which the joiner lacks.
		whole.plan(&layout, (1, true), start(100), by, 50).unwrap();
		whole.plan(&layout, (1, false), start(101), by, 5).unwrap();
		let a = whole.plan(&layout, (2, true), start(110), by, 20).unwrap();
		let b = joiner.plan(&layout, (2, true), start(110), by, 20).unwrap();
		assert_eq!(a.positions, b.positions);
		assert_eq!((a.counter, b.counter), (0, 0));
		let a = whole.plan(&layout, (2, false), start(110), by, 3).unwrap();
		let b = joiner.plan(&layout, (2, false), start(110), by, 3).unwrap();
		assert_eq!(a.positions, b.positions);
		assert_eq!((a.counter, b.counter), (4, 4));
		assert_eq!((whole.overruns(), joiner.overruns()), (0, 0));
	}

	#[test]
	fn groups_released_in_one_slot_keep_their_own_anchors() {
		let layout = layout();
		// 48 kHz AAC: frames 21.3 ms apart, two to a slot now and then.
		let frame = 1_000_000_000 * 1024 / 48_000;
		let mut whole = Lane::new(1);
		let mut joiner = Lane::new(1);
		let last = start(100) + 2_000_000;
		whole.plan(&layout, (1, true), last - frame, u128::MAX, 2).unwrap();
		whole.plan(&layout, (1, false), last, u128::MAX, 2).unwrap();
		assert_eq!(layout.slot_of(layout.at(last)), layout.slot_of(layout.at(last + frame)));
		let a = whole.plan(&layout, (2, true), last + frame, u128::MAX, 2).unwrap();
		let b = joiner.plan(&layout, (2, true), last + frame, u128::MAX, 2).unwrap();
		assert_eq!(a.positions, b.positions);
		assert_eq!(whole.overruns(), 0);
	}

	#[test]
	fn a_group_held_from_its_middle_is_not_placed() {
		let layout = layout();
		let mut lane = Lane::new(0);
		assert_eq!(
			lane.plan(&layout, (1, false), start(10), u128::MAX, 2).err(),
			Some(Unplaced::Partial)
		);
		assert_eq!(
			lane.plan(&layout, (1, false), start(10), u128::MAX, 2).err(),
			Some(Unplaced::Partial)
		);
		assert!(lane.plan(&layout, (2, true), start(30), u128::MAX, 2).is_ok());
	}

	#[test]
	fn a_late_unit_leaves_its_positions_to_the_next() {
		let layout = layout();
		let mut lane = Lane::new(1);
		let at = layout.time(layout.first(11));
		assert_eq!(
			lane.plan(&layout, (1, true), start(10), at, 30).err(),
			Some(Unplaced::Late)
		);
		let placed = lane.plan(&layout, (1, false), start(10), u128::MAX, 2).unwrap();
		assert_eq!(placed.counter, 0);
		assert!(placed.positions[0] >= layout.first(10));
	}

	#[test]
	fn a_release_moves_later_only_while_its_unit_meets_its_deadline() {
		let layout = layout();
		// Half a second of lane 0's payload.
		let bits = layout.payload_rate(0) / 2;
		let later = layout.later(0, start(100), bits, start(200), 2);
		assert!((start(119)..=start(120)).contains(&later), "{later}");
		assert_eq!(layout.later(0, start(100), bits, start(110), 2), start(100));
		assert_eq!(layout.later(0, start(100), 0, start(110), 2), start(100));
	}

	#[test]
	fn the_grid_lays_tables_and_si_beside_the_lanes() {
		let layout = Layout::new(RATE, 2, 80, &[(0x100, Need::Rest(u64::MAX)), (0x101, Need::Fixed(422))]).unwrap();
		let mut grid = Grid::new(Arc::new(layout), tables());
		let mut si = [0xffu8; TsPacket::SIZE];
		si[..4].copy_from_slice(&[0x47, 0x40, 0x11, 0x10]);
		grid.push_si(40, &[si, si].concat());
		let mut seen = Vec::new();
		for index in 40..44 {
			let laid = grid.lay(index);
			assert_eq!(laid.count, 34);
			let first = grid.layout().first(index);
			for (packet, position) in laid
				.packets
				.as_chunks::<{ TsPacket::SIZE }>()
				.0
				.iter()
				.zip(&laid.positions)
			{
				seen.push((pid(packet), first + *position as u128, packet[3] & 0x0f));
			}
		}
		let first = grid.layout().first(40);
		let on = |pid: u16| -> Vec<(u128, u8)> {
			seen.iter()
				.filter(|seen| seen.0 == pid)
				.map(|&(_, at, counter)| (at - first, counter))
				.collect()
		};
		// The PAT and PMT sit at their positions, numbered on from the emissions before them on
		// the grid.
		let tables = &grid.layout().tables;
		assert_eq!(on(0x0000), [(tables[0] as u128, 2)]);
		assert_eq!(on(0x1000), [(tables[1] as u128, 2)]);
		let si = on(0x0011);
		assert_eq!(si.len(), 2);
		assert!(
			si[1].0 >= si[0].0 + grid.layout().spacing as u128,
			"SI paced like the tables"
		);
	}
}
