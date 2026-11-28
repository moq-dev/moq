//! Send scheduling on a congested link: the transport's send groups against the publishers'
//! session-wide priority queue and two scalar orderings.
//!
//! Each workload runs every scheduler from a fresh simulation. The four schedulers:
//!
//! - `groups`: one send group per subscription at its priority, streams ordered by group
//!   key inside it (what native QUIC uses).
//! - `queue`: the publishers' `PriorityQueue`, ranking every in-flight group on the session
//!   into one `u8` send order, re-applied to each stream whose rank moves.
//! - `scalar_seq`: a 32-bit `[priority][group key]` key, which leaves 24 bits for the
//!   key and compares keys across subscriptions.
//! - `scalar`: the priority alone, so equal subscriptions share by stream count.
//!
//! `fanout_*` sweeps subscriptions against groups in flight per subscription, timing one frame,
//! so a scheduling cost that grows with the table rather than the touched path is a slope.
//! `datagram_*` sweeps send groups and queued datagrams per group against one
//! datagram sent and one evicted, for the same reason.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench schedule`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};

use swole::bench;

// Both schedulers compile straight from source, so neither widens its crate's API for this.
#[path = "../src/priority.rs"]
#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
mod priority;

// The queue names `crate::track::Order`, which is public.
mod track {
	pub use moq_net::track::Order;
}

// Keeps moq-quic's own formatting.
#[rustfmt::skip]
#[path = "../../moq-quic/src/connection/streams/schedule.rs"]
#[allow(dead_code, reason = "the bench compiles the private module from source and uses only part of it")]
mod schedule;

use priority::{Priority, PriorityHandle, PriorityQueue};
use schedule::{Next, Schedule, Slot};

/// The most a frame carries, one packet.
const PACKET: usize = 1200;

/// One subscription's media: `rate` bytes a tick into its open group, a new group every
/// `cadence` ticks, numbered from `first` in steps of `step`.
#[derive(Clone, Copy)]
struct Track {
	priority: i8,
	rate: usize,
	cadence: u64,
	first: u64,
	step: u64,
}

impl Track {
	const fn new(priority: i8, rate: usize, cadence: u64) -> Self {
		Self {
			priority,
			rate,
			cadence,
			first: 0,
			step: 1,
		}
	}

	const fn numbered(mut self, first: u64, step: u64) -> Self {
		self.first = first;
		self.step = step;
		self
	}
}

#[derive(Clone)]
struct Workload {
	name: &'static str,
	tracks: Vec<Track>,
	/// Bytes the link takes per tick.
	capacity: usize,
	ticks: u64,
}

impl fmt::Display for Workload {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(self.name)
	}
}

fn workloads() -> Vec<Workload> {
	vec![
		// The issue: audio and video at one priority, under a higher-priority track that
		// takes a quarter of the link. Video has a deep backlog of large groups; audio
		// opens a small group every tick.
		Workload {
			name: "alice-bob",
			tracks: vec![
				Track::new(5, 5_000, 2),
				Track::new(4, 30_000, 30),
				Track::new(4, 15_000, 1),
			],
			capacity: 20_000,
			ticks: 3_000,
		},
		// Group keys crossing 2^24, where the scalar's 24 bits wrap.
		Workload {
			name: "wrap",
			tracks: vec![
				Track::new(4, 15_000, 2).numbered((1 << 24) - 100, 1),
				Track::new(4, 15_000, 2).numbered((1 << 24) - 100, 1),
			],
			capacity: 20_000,
			ticks: 3_000,
		},
		// Groups numbered by a millisecond clock, which wraps 24 bits within hours.
		Workload {
			name: "sparse",
			tracks: vec![
				Track::new(4, 15_000, 2).numbered((1 << 24) - 50_000, 1_000),
				Track::new(4, 15_000, 2).numbered(1 << 30, 1_000),
			],
			capacity: 20_000,
			ticks: 3_000,
		},
		// Many equal subscriptions with different cadences sharing one link.
		Workload {
			name: "crowd",
			tracks: (0..64)
				.map(|i| Track::new(4, 1_000 + 100 * i, 1 + i as u64 % 8))
				.collect(),
			capacity: 50_000,
			ticks: 2_000,
		},
	]
}

#[derive(Clone, Copy)]
enum Kind {
	Groups,
	Queue,
	ScalarSeq,
	Scalar,
}

struct Stream {
	track: usize,
	seq: u64,
	slot: Slot,
	priority: i64,
	/// Bytes written but not yet sent.
	queued: usize,
	/// Whether the group is complete, so the stream closes once drained.
	finished: bool,
	opened: u64,
	rank: Option<PriorityHandle>,
}

/// Collects the streams whose rank moved, the way each group serve is woken.
struct Moved {
	id: u64,
	list: Arc<Mutex<Vec<u64>>>,
}

impl Wake for Moved {
	fn wake(self: Arc<Self>) {
		self.list.lock().unwrap().push(self.id);
	}
}

#[derive(Default, Clone)]
struct Share {
	bytes: u64,
	/// The newest group opened and the newest delivered whole.
	opened: Option<u64>,
	delivered: Option<u64>,
	/// Ticks from a group's completion to its last byte, summed over delivered groups.
	delay: u64,
	groups: u64,
}

struct Sim {
	kind: Kind,
	tracks: Vec<Track>,
	capacity: usize,
	schedule: Schedule<u64, ()>,
	groups: Vec<usize>,
	queue: PriorityQueue,
	moved: Arc<Mutex<Vec<u64>>>,
	streams: HashMap<u64, Stream>,
	current: Vec<u64>,
	next: u64,
	tick: u64,
	shares: Vec<Share>,
	/// Stream priority changes the scheduler needed, beyond each stream's first.
	work: u64,
}

impl Sim {
	fn new(kind: Kind, workload: &Workload) -> Self {
		let mut schedule = Schedule::new();
		let groups = workload
			.tracks
			.iter()
			.map(|track| schedule.open_group(track.priority.into()))
			.collect();
		let mut sim = Self {
			kind,
			tracks: workload.tracks.clone(),
			capacity: workload.capacity,
			schedule,
			groups,
			queue: PriorityQueue::default(),
			moved: Arc::default(),
			streams: HashMap::new(),
			current: Vec::new(),
			next: 0,
			tick: 0,
			shares: vec![Share::default(); workload.tracks.len()],
			work: 0,
		};
		for track in 0..sim.tracks.len() {
			let first = sim.tracks[track].first;
			let id = sim.open(track, first);
			sim.current.push(id);
		}
		sim
	}

	fn open(&mut self, track: usize, seq: u64) -> u64 {
		let id = self.next;
		self.next += 1;
		let priority = self.tracks[track].priority;
		let mut stream = Stream {
			track,
			seq,
			slot: Slot::default(),
			priority: 0,
			queued: 0,
			finished: false,
			opened: self.tick,
			rank: None,
		};
		match self.kind {
			Kind::Groups => {
				self.schedule.join(id, &mut stream.slot, Some(self.groups[track]), 0);
				stream.priority = seq as i64;
			}
			Kind::Queue => {
				let mut rank = self.queue.insert(Priority::new(priority, track as u64, seq));
				stream.priority = PriorityHandle::send_order_of(rank.current()).into();
				let waker = Waker::from(Arc::new(Moved {
					id,
					list: self.moved.clone(),
				}));
				let _ = rank.poll_next(&kio::Waiter::new(waker));
				stream.rank = Some(rank);
			}
			Kind::ScalarSeq => stream.priority = (i64::from(priority) << 24) | (seq & 0xff_ffff) as i64,
			Kind::Scalar => stream.priority = priority.into(),
		}
		let share = &mut self.shares[track];
		share.opened = Some(share.opened.map_or(seq, |opened| opened.max(seq)));
		self.streams.insert(id, stream);
		id
	}

	fn close(&mut self, id: u64) {
		let mut stream = self.streams.remove(&id).unwrap();
		self.schedule.leave(&mut stream.slot);
		let share = &mut self.shares[stream.track];
		share.delivered = Some(share.delivered.map_or(stream.seq, |seq| seq.max(stream.seq)));
		share.delay += self.tick - stream.opened;
		share.groups += 1;
	}

	/// Produce a tick of media, then send a tick's worth of it.
	fn step(&mut self) {
		self.tick += 1;
		for track in 0..self.tracks.len() {
			let Track {
				rate, cadence, step, ..
			} = self.tracks[track];
			if self.tick.is_multiple_of(cadence) {
				let old = self.current[track];
				let stream = self.streams.get_mut(&old).unwrap();
				stream.finished = true;
				let seq = stream.seq + step;
				if stream.queued == 0 {
					self.close(old);
				}
				self.current[track] = self.open(track, seq);
			}
			let id = self.current[track];
			let stream = self.streams.get_mut(&id).unwrap();
			if stream.queued == 0 {
				self.schedule.push(id, &mut stream.slot, stream.priority);
			}
			stream.queued += rate;
		}

		// Apply every rank the queue moved, as each woken group serve would.
		let moved = std::mem::take(&mut *self.moved.lock().unwrap());
		for id in moved {
			let Some(stream) = self.streams.get_mut(&id) else {
				continue;
			};
			let rank = stream.rank.as_mut().unwrap();
			let waker = Waker::from(Arc::new(Moved {
				id,
				list: self.moved.clone(),
			}));
			let waiter = kio::Waiter::new(waker);
			while let std::task::Poll::Ready(rank) = rank.poll_next(&waiter) {
				stream.priority = PriorityHandle::send_order_of(rank).into();
				self.work += 1;
			}
		}

		let mut budget = self.capacity;
		while budget > 0 {
			let Some(Next::Stream(id)) = self.schedule.pop() else {
				break;
			};
			let stream = self.streams.get_mut(&id).unwrap();
			let bytes = stream.queued.min(PACKET).min(budget);
			stream.queued -= bytes;
			budget -= bytes;
			self.shares[stream.track].bytes += bytes as u64;
			let requeue = (stream.queued > 0).then_some(stream.priority);
			self.schedule.sent(id, &mut stream.slot, bytes, requeue, true);
			if stream.queued == 0 && stream.finished {
				self.close(id);
			}
		}
	}

	fn run(&mut self, ticks: u64) {
		for _ in 0..ticks {
			self.step();
		}
	}
}

/// A fresh simulation of one workload on one scheduler, and the ticks to run it for.
struct Run {
	sim: Sim,
	ticks: u64,
}

impl Run {
	fn new(kind: Kind, workload: &Workload) -> Self {
		Self {
			sim: Sim::new(kind, workload),
			ticks: workload.ticks,
		}
	}

	/// Run every tick, returning the simulation so its drop stays out of the measurement.
	fn run(mut self) -> Sim {
		self.sim.run(self.ticks);
		self.sim
	}
}

/// One benchmark per scheduler, each over every workload.
macro_rules! workload {
	($name:ident, $setup:ident, $kind:expr) => {
		fn $setup(workload: Workload) -> Run {
			Run::new($kind, &workload)
		}

		#[bench(args = workloads(), setup = $setup)]
		fn $name(run: Run) -> Sim {
			run.run()
		}
	};
}

workload!(groups, groups_run, Kind::Groups);
workload!(queue, queue_run, Kind::Queue);
workload!(scalar_seq, scalar_seq_run, Kind::ScalarSeq);
workload!(scalar, scalar_run, Kind::Scalar);

/// A saturated schedule: `subscriptions` groups at one priority, each with `streams` queued.
fn saturated(subscriptions: usize, streams: usize) -> (Schedule<u64, ()>, Vec<Slot>) {
	let mut schedule = Schedule::new();
	let mut slots = Vec::with_capacity(subscriptions * streams);
	for _ in 0..subscriptions {
		let group = schedule.open_group(0);
		for seq in 0..streams {
			let id = slots.len() as u64;
			let mut slot = Slot::default();
			schedule.join(id, &mut slot, Some(group), seq as i64);
			schedule.push(id, &mut slot, seq as i64);
			slots.push(slot);
		}
	}
	(schedule, slots)
}

/// One point of a fanout sweep: send groups at one priority, against what each holds queued.
#[derive(Clone, Copy)]
struct Fanout {
	/// `subs` for subscriptions, `groups` for datagram send groups.
	axis: &'static str,
	groups: usize,
	queued: usize,
}

impl fmt::Display for Fanout {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}_{}/{}", self.axis, self.groups, self.queued)
	}
}

/// Every point under 100,000 queued in all.
fn fanout(axis: &'static str) -> Vec<Fanout> {
	[1, 16, 256, 4_096]
		.into_iter()
		.flat_map(|groups| [1, 16, 256].map(|queued| Fanout { axis, groups, queued }))
		.filter(|case| case.groups * case.queued <= 100_000)
		.collect()
}

struct Frames {
	schedule: Schedule<u64, ()>,
	slots: Vec<Slot>,
	streams: u64,
}

fn frames(case: Fanout) -> Frames {
	let (schedule, slots) = saturated(case.groups, case.queued);
	Frames {
		schedule,
		slots,
		streams: case.queued as u64,
	}
}

/// One frame sent from a saturated schedule, its stream requeued behind its group's others.
#[bench(args = fanout("subs"), setup = frames)]
fn fanout_frame(frames: &mut Frames) {
	let Some(Next::Stream(id)) = frames.schedule.pop() else {
		unreachable!("only streams are queued");
	};
	let slot = &mut frames.slots[id as usize];
	frames
		.schedule
		.sent(id, slot, PACKET, Some((id % frames.streams) as i64), true);
}

fn churn(case: Fanout) -> (Schedule<u64, ()>, Vec<Slot>) {
	saturated(case.groups, case.queued)
}

/// A group opening and closing a stream, as each new MoQ group does. Opening then closing
/// leaves the schedule as it was, so one serves every call.
#[bench(args = fanout("subs"), setup = churn)]
fn fanout_churn((schedule, _slots): &mut (Schedule<u64, ()>, Vec<Slot>)) {
	let mut slot = Slot::default();
	schedule.join(u64::MAX, &mut slot, Some(0), i64::MAX);
	schedule.push(u64::MAX, &mut slot, i64::MAX);
	schedule.leave(&mut slot);
}

/// A full datagram budget: `groups` send groups at one priority, with `datagrams` queued in
/// each, one packet apiece.
fn datagrams(groups: usize, datagrams: usize) -> (Schedule<u64, ()>, Vec<usize>) {
	let mut schedule = Schedule::new();
	let ids: Vec<_> = (0..groups).map(|_| schedule.open_group(0)).collect();
	let budget = groups * datagrams * PACKET;
	for seq in 0..datagrams {
		for &group in &ids {
			assert!(schedule.push_datagram(Some(group), seq as i64, (), PACKET, budget));
		}
	}
	(schedule, ids)
}

struct Evict {
	schedule: Schedule<u64, ()>,
	ids: Vec<usize>,
	budget: usize,
	seq: i64,
	next: usize,
}

fn evict(case: Fanout) -> Evict {
	let (schedule, ids) = datagrams(case.groups, case.queued);
	Evict {
		budget: schedule.datagram_bytes(),
		schedule,
		ids,
		seq: case.queued as i64,
		next: 0,
	}
}

/// A datagram into a full budget, which evicts the oldest of the lowest-priority group.
#[bench(args = fanout("groups"), setup = evict)]
fn datagram_evict_groups(evict: &mut Evict) {
	// Round robin, so every group keeps its share of the budget.
	evict
		.schedule
		.push_datagram(Some(evict.ids[evict.next]), evict.seq, (), PACKET, evict.budget);
	evict.next = (evict.next + 1) % evict.ids.len();
	evict.seq += 1;
}

fn send(case: Fanout) -> (Schedule<u64, ()>, i64) {
	(datagrams(case.groups, case.queued).0, case.queued as i64)
}

/// A datagram sent and another queued in its place, through every round robin.
#[bench(args = fanout("groups"), setup = send)]
fn datagram_send((schedule, seq): &mut (Schedule<u64, ()>, i64)) {
	let Some(Next::Datagram(at)) = schedule.pop() else {
		unreachable!("only datagrams are queued");
	};
	schedule.sent_datagram(at);
	schedule.push_datagram(at, *seq, (), PACKET, usize::MAX);
	*seq += 1;
}

fn main() {
	swole::main();
}
