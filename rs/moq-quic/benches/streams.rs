//! Stream send and scheduling: one client writes many streams to a server that reads them all.
//!
//! Throughput cases open fresh unidirectional streams on a long-lived connection, write and
//! finish them, then exchange packets until the server has read them and the path is idle.
//! The zero-credit case isolates blocked writes and resets across live-writer counts, with
//! connection setup and dropping the remaining state outside the measurement. The
//! reprioritize case moves streams that stay queued, never sending them. The group churn case
//! moves and cancels send groups, one per subscription, while the rest have data queued.

use std::{fmt, task::Poll};

use bytes::Bytes;
use moq_quic::stream::Dir;
use swole::bench;

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, WriteNow, payload};

#[allow(dead_code, reason = "the scheduler benchmark exercises only its queued send paths")]
#[path = "../src/connection/streams/schedule.rs"]
mod scheduler;

mod schedule {
	use std::fmt;

	use swole::bench;

	use super::scheduler::{Next, Schedule, Slot};

	/// Send groups against streams or datagrams queued in each.
	#[derive(Clone, Copy)]
	struct Queue {
		groups: usize,
		queued: usize,
	}

	impl fmt::Display for Queue {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			write!(f, "groups_{}_queued_{}", self.groups, self.queued)
		}
	}

	fn queues() -> Vec<Queue> {
		[1, 16, 256]
			.into_iter()
			.flat_map(|groups| [1, 16, 256].map(|queued| Queue { groups, queued }))
			.collect()
	}

	struct Streams {
		schedule: Schedule<usize, ()>,
		slots: Vec<Slot>,
	}

	fn streams(case: Queue) -> Streams {
		let mut schedule = Schedule::<usize, ()>::new();
		let ids: Vec<_> = (0..case.groups).map(|_| schedule.open_group(0)).collect();
		let mut slots: Vec<_> = (0..case.groups * case.queued).map(|_| Slot::default()).collect();
		for (id, slot) in slots.iter_mut().enumerate() {
			schedule.join(id, slot, Some(ids[id / case.queued]), 0);
			schedule.push(id, slot, 0);
		}
		Streams { schedule, slots }
	}

	#[bench(args = queues(), setup = streams)]
	fn pop(state: &mut Streams) {
		let Some(Next::Stream(id)) = state.schedule.pop() else {
			panic!("stream remains queued")
		};
		state.schedule.sent(id, &mut state.slots[id], 1200, Some(0), true);
	}

	struct Datagrams {
		schedule: Schedule<usize, ()>,
		ids: Vec<usize>,
		budget: usize,
		next: usize,
	}

	fn datagrams(case: Queue) -> Datagrams {
		let mut schedule = Schedule::<usize, ()>::new();
		let ids: Vec<_> = (0..case.groups).map(|_| schedule.open_group(0)).collect();
		for &id in &ids {
			for _ in 0..case.queued {
				schedule.push_datagram(Some(id), 0, (), 100, usize::MAX);
			}
		}
		Datagrams {
			schedule,
			ids,
			budget: case.groups * case.queued * 100,
			next: 0,
		}
	}

	#[bench(args = queues(), setup = datagrams)]
	fn datagram_push(state: &mut Datagrams) {
		let id = state.ids[state.next % state.ids.len()];
		state.schedule.push_datagram(Some(id), 0, (), 100, state.budget);
		state.next += 1;
	}

	/// Where in the round robin the changed groups sit.
	#[derive(Clone, Copy)]
	enum Order {
		Front,
		Tail,
		Mixed,
	}

	/// Moving or retiring some queued groups, as subscription updates and cancels do, while the
	/// rest stay queued. Groups are picked from the front of the round robin, its tail, or spread
	/// across it, so a cost that grows with the untouched groups shows as a slope.
	#[derive(Clone, Copy)]
	struct Change {
		queued: usize,
		changed: usize,
		order: Order,
	}

	impl fmt::Display for Change {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			let order = match self.order {
				Order::Front => "front",
				Order::Tail => "tail",
				Order::Mixed => "mixed",
			};
			write!(f, "queued_{}_changed_{}_{order}", self.queued, self.changed)
		}
	}

	fn changes() -> Vec<Change> {
		[(16, 16), (256, 16), (4096, 16), (4096, 256)]
			.into_iter()
			.flat_map(|(queued, changed)| {
				[Order::Front, Order::Tail, Order::Mixed].map(|order| Change { queued, changed, order })
			})
			.collect()
	}

	/// One queued stream in each of `queued` groups, and the groups a call changes.
	struct Queued {
		schedule: Schedule<usize, ()>,
		ids: Vec<usize>,
		slots: Vec<Slot>,
		picked: Vec<usize>,
	}

	fn queued(case: Change) -> Queued {
		let Change { queued, changed, order } = case;
		let mut schedule = Schedule::<usize, ()>::new();
		let ids: Vec<_> = (0..queued).map(|_| schedule.open_group(0)).collect();
		let mut slots: Vec<_> = (0..queued).map(|_| Slot::default()).collect();
		for (id, slot) in slots.iter_mut().enumerate() {
			schedule.join(id, slot, Some(ids[id]), 0);
			schedule.push(id, slot, 0);
		}
		let picked = match order {
			Order::Front => (0..changed).collect(),
			Order::Tail => (queued - changed..queued).rev().collect(),
			// An odd stride visits distinct groups of a power-of-two count.
			Order::Mixed => (0..changed).map(|i| i * 2_654_435_761 % queued).collect(),
		};
		Queued {
			schedule,
			ids,
			slots,
			picked,
		}
	}

	/// Returns the schedule, so dropping it stays outside the measurement.
	#[bench(args = changes(), setup = queued, items = |case: Change| case.changed)]
	fn group_priority(mut state: Queued) -> Queued {
		for &index in &state.picked {
			state.schedule.set_group_priority(state.ids[index], 1);
		}
		state
	}

	/// Returns the schedule, so dropping it stays outside the measurement.
	#[bench(args = changes(), setup = queued, items = |case: Change| case.changed)]
	fn group_retire(mut state: Queued) -> Queued {
		for &index in &state.picked {
			state.schedule.close_group(state.ids[index]);
			state.schedule.leave(&mut state.slots[index]);
		}
		state
	}
}

/// How often a `holes` case loses a client datagram: often enough that most windows hold a hole.
const LOSE_EVERY: u64 = 16;

/// Opens `streams` streams across `priorities` levels, writes `writes` writes of `size` bytes to
/// each, and drives until the server has read them all.
fn send(pair: &mut Pair, streams: usize, priorities: usize, writes: usize, size: usize) {
	let data = payload(size);
	let target = pair.server.received.finished + streams as u64;
	let conn = pair.client();
	for index in 0..streams {
		let id = conn.streams().open(Dir::Uni, Default::default()).expect("stream limit");
		let mut stream = conn.send_stream(id);
		// `as _` follows the priority type, which a scheduler rewrite widens.
		stream.set_priority((index % priorities) as _).unwrap();
		for _ in 0..writes {
			assert_eq!(stream.write_now(&mut data.clone()), size, "write was cut short");
		}
		stream.finish().unwrap();
	}
	pair.drive_until(|pair| pair.server.received.finished >= target);
	pair.settle();
}

/// Bytes each `send_priority` stream carries.
const PRIORITY_BYTES: usize = 4096;

/// Stream count against priority levels.
#[derive(Clone, Copy)]
struct Priority {
	streams: usize,
	priorities: usize,
}

impl fmt::Display for Priority {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "streams_{}/priorities_{}", self.streams, self.priorities)
	}
}

fn priorities() -> Vec<Priority> {
	[1, 16, 256, 1024]
		.into_iter()
		.flat_map(|streams| [1, 8, 64].map(|priorities| Priority { streams, priorities }))
		.filter(|case| case.priorities <= case.streams)
		.collect()
}

fn priority_pair(case: Priority) -> (Priority, Pair) {
	(case, Pair::new(Setup::default()))
}

/// Stream count against priority levels, with a few packets per stream so they interleave.
#[bench(args = priorities(), setup = priority_pair, bytes = |case: Priority| case.streams * PRIORITY_BYTES)]
fn send_priority((case, pair): &mut (Priority, Pair)) {
	send(pair, case.streams, case.priorities, 1, PRIORITY_BYTES);
}

mod set_priority {
	use moq_quic::stream::{Dir, Id};
	use swole::bench;

	use super::common::{Pair, Setup, WriteNow};

	/// A pair whose client has `ids` queued and never drained.
	struct Queued {
		pair: Pair,
		ids: Vec<Id>,
		round: usize,
	}

	fn queue(queued: usize) -> Queued {
		let mut pair = Pair::new(Setup::default());
		let conn = pair.client();
		let ids = (0..queued)
			.map(|_| {
				let id = conn.streams().open(Dir::Uni, Default::default()).expect("stream limit");
				assert_eq!(conn.send_stream(id).write_now(&mut &b"x"[..]), 1);
				id
			})
			.collect();
		Queued { pair, ids, round: 0 }
	}

	/// Reprioritize every stream of a queue that is never drained, so each call moves a queued
	/// stream.
	#[bench(args = [16, 256, 4096], setup = queue, items = |queued: usize| queued)]
	fn queued(state: &mut Queued) {
		state.round += 1;
		let conn = state.pair.client();
		for (index, &id) in state.ids.iter().enumerate() {
			// A priority differing from the last round's, so no call is a no-op.
			let priority = ((index + state.round) % 64) as _;
			conn.send_stream(id).set_priority(priority).unwrap();
		}
	}
}

/// Bytes each `group_churn` stream carries.
const CHURN_BYTES: usize = 256;

/// Subscriptions against how many of them change each round.
#[derive(Clone, Copy)]
struct Churn {
	subs: usize,
	churn: usize,
}

impl fmt::Display for Churn {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "subs_{}/churn_{}", self.subs, self.churn)
	}
}

fn churns() -> Vec<Churn> {
	[(16, 4), (256, 16), (4096, 16), (4096, 256)]
		.map(|(subs, churn)| Churn { subs, churn })
		.into()
}

/// A pair with one send group per subscription.
struct Subscriptions {
	case: Churn,
	pair: Pair,
	groups: Vec<moq_quic::send::Group>,
	data: Bytes,
	round: usize,
}

fn subscriptions(case: Churn) -> Subscriptions {
	// The moved and cancelled groups stay distinct, below.
	assert!(2 * case.churn <= case.subs && case.subs.is_power_of_two());
	let mut pair = Pair::new(Setup::default());
	let groups = (0..case.subs).map(|_| pair.client().send_groups().open(0)).collect();
	Subscriptions {
		case,
		pair,
		groups,
		data: payload(CHURN_BYTES),
		round: 0,
	}
}

/// Subscription churn on a shared connection: each round writes one small stream in each of
/// `subs` send groups, then, with all of it queued, moves `churn` groups spread across the
/// round robin to another priority and back and cancels `churn` others (reset the stream,
/// close the group, open its replacement), and drives until the server has seen every stream.
#[bench(args = churns(), setup = subscriptions, items = |case: Churn| case.subs)]
fn group_churn(state: &mut Subscriptions) {
	use moq_quic::send;
	let Churn { subs, churn } = state.case;
	state.round += 1;
	let round = state.round;
	let pair = &mut state.pair;
	let groups = &mut state.groups;
	let finished = pair.server.received.finished;
	let reset = pair.server.received.reset;
	let conn = pair.client();
	let ids: Vec<_> = groups
		.iter()
		.map(|group| {
			let options = send::Options {
				group: Some(group),
				priority: 0,
			};
			let id = conn.streams().open(Dir::Uni, options).expect("stream limit");
			let mut stream = conn.send_stream(id);
			assert_eq!(stream.write_now(&mut state.data.clone()), CHURN_BYTES);
			stream.finish().unwrap();
			id
		})
		.collect();
	// An odd stride permutes a power-of-two count, spreading the touched groups over the round
	// robin and shifting them each round.
	let spread = |i: usize| (round + i * 2_654_435_761) % subs;
	for index in (0..churn).map(spread) {
		conn.send_groups().set_priority(&groups[index], 1);
		conn.send_groups().set_priority(&groups[index], 0);
	}
	for index in (churn..2 * churn).map(spread) {
		conn.send_stream(ids[index]).reset(0u32.into()).unwrap();
		let fresh = conn.send_groups().open(0);
		conn.send_groups().close(std::mem::replace(&mut groups[index], fresh));
	}
	let target = (finished + (subs - churn) as u64, reset + churn as u64);
	pair.drive_until(|pair| {
		let received = &pair.server.received;
		received.finished >= target.0 && received.reset >= target.1
	});
	pair.settle();
}

/// Writes `total` bytes to one stream in `size`-byte writes, exchanging packets whenever the
/// client's send window is full, and drives until the server has read them all.
fn send_windowed(pair: &mut Pair, total: usize, size: usize) {
	let data = payload(size);
	let target = pair.server.received.finished + 1;
	let id = pair
		.client()
		.streams()
		.open(Dir::Uni, Default::default())
		.expect("stream limit");
	let mut written = 0;
	while written < total {
		// A write the window cuts short resumes mid-chunk, so every chunk boundary stays put.
		let start = written % size;
		let noop = kio::Waiter::noop();
		match pair
			.client()
			.send_stream(id)
			.poll_write(&noop, &mut data.slice(start..))
		{
			Poll::Ready(Ok(n)) => written += n,
			Poll::Pending => pair.drive_until(|pair| {
				matches!(
					pair.client().send_stream(id).poll_write(&noop, &mut Bytes::new()),
					Poll::Ready(Ok(_))
				)
			}),
			Poll::Ready(Err(err)) => panic!("{err}"),
		}
	}
	pair.client().send_stream(id).finish().unwrap();
	pair.drive_until(|pair| pair.server.received.finished >= target);
	pair.settle();
}

/// Bytes `send_write_size` writes to each stream.
const WRITE_BYTES: usize = 64 << 10;
/// Bytes a `send_write_size` window case writes to its one stream.
const WINDOW_TOTAL: usize = 256 << 10;

/// What a `send_write_size` case writes through.
#[derive(Clone, Copy)]
enum Writer {
	/// This many streams, each written 64 KiB.
	Streams(usize),
	/// One stream written 256 KiB through a send window of this many bytes, losing a datagram
	/// now and then when `holes`.
	Window { window: usize, holes: bool },
}

/// What a case writes through, against bytes per write.
#[derive(Clone, Copy)]
struct WriteSize {
	writer: Writer,
	size: usize,
}

impl WriteSize {
	fn bytes(self) -> usize {
		match self.writer {
			Writer::Streams(streams) => streams * WRITE_BYTES,
			Writer::Window { .. } => WINDOW_TOTAL,
		}
	}
}

impl fmt::Display for WriteSize {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self.writer {
			Writer::Streams(streams) => write!(f, "streams_{streams}")?,
			Writer::Window { window, holes: false } => write!(f, "window_{}k", window >> 10)?,
			Writer::Window { window, holes: true } => write!(f, "window_{}k_holes", window >> 10)?,
		}
		write!(f, "/bytes_{}", self.size)
	}
}

fn write_sizes() -> Vec<WriteSize> {
	let windows = [
		(16 << 10, false),
		(64 << 10, false),
		(WINDOW_TOTAL, false),
		(WINDOW_TOTAL, true),
	];
	let writers = [Writer::Streams(1), Writer::Streams(64)]
		.into_iter()
		.chain(windows.map(|(window, holes)| Writer::Window { window, holes }));
	writers
		.flat_map(|writer| [64, 1024, 16384].map(|size| WriteSize { writer, size }))
		.collect()
}

fn write_pair(case: WriteSize) -> (WriteSize, Pair) {
	let pair = match case.writer {
		Writer::Streams(_) => Pair::new(Setup::default()),
		Writer::Window { window, holes } => {
			let mut pair = Pair::new(Setup {
				client_send_window: window as u64,
				..Setup::default()
			});
			pair.lose_every = holes.then_some(LOSE_EVERY);
			pair
		}
	};
	(case, pair)
}

/// Bytes per write against stream count, writing a fixed 64 KiB to each stream.
///
/// The `window` cases instead write a fixed 256 KiB to one stream through a send window, so the
/// data retained until acknowledged grows with the window while the total stays put. The
/// `holes` cases, at the largest window only to keep the nightly short, lose a datagram now and
/// then, so a hole keeps everything after it retained and its retransmission reads old offsets.
#[bench(args = write_sizes(), setup = write_pair, bytes = WriteSize::bytes)]
fn send_write_size((case, pair): &mut (WriteSize, Pair)) {
	match case.writer {
		Writer::Streams(streams) => send(pair, streams, 1, WRITE_BYTES / case.size, case.size),
		Writer::Window { .. } => send_windowed(pair, WINDOW_TOTAL, case.size),
	}
}

/// Writers kept blocked against writers that block and reset each call.
#[derive(Clone, Copy)]
struct Blocked {
	live: usize,
	churn: usize,
}

impl fmt::Display for Blocked {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "live_{}/churn_{}", self.live, self.churn)
	}
}

fn blocked_cases() -> Vec<Blocked> {
	[1, 64, 1024]
		.into_iter()
		.flat_map(|live| [16, 256].map(|churn| Blocked { live, churn }))
		.collect()
}

/// A fresh connection with MAX_DATA spent and `live` writers blocked on it.
fn blocked(case: Blocked) -> (usize, Pair) {
	let mut pair = Pair::new(Setup {
		server_receive_window: 1u32.into(),
		..Setup::default()
	});
	// A window can't be zero, so one stream spends its only byte.
	let id = pair.client().streams().open(Dir::Uni, Default::default()).unwrap();
	let noop = kio::Waiter::noop();
	assert_eq!(
		pair.client().send_stream(id).poll_write(&noop, &mut &b"x"[..]),
		Poll::Ready(Ok(1))
	);
	for _ in 0..case.live {
		let id = pair.client().streams().open(Dir::Uni, Default::default()).unwrap();
		assert_eq!(
			pair.client().send_stream(id).poll_write(&noop, &mut &b"x"[..]),
			Poll::Pending
		);
	}
	(case.churn, pair)
}

/// Keep `live` writers blocked while `churn` others block and reset, with MAX_DATA spent and
/// never raised. A fresh connection per call bounds even the unfixed implementation's retained
/// history.
///
/// Returns the pair, so dropping the live table stays outside the measurement.
#[bench(args = blocked_cases(), setup = blocked, items = |case: Blocked| case.churn)]
fn connection_blocked((churn, mut pair): (usize, Pair)) -> Pair {
	for _ in 0..churn {
		let id = pair.client().streams().open(Dir::Uni, Default::default()).unwrap();
		let mut stream = pair.client().send_stream(id);
		assert_eq!(stream.poll_write(&kio::Waiter::noop(), &mut &b"x"[..]), Poll::Pending);
		stream.reset(0u32.into()).unwrap();
	}
	// The peer never grants credit, so no packets are exchanged.
	pair
}

/// Bytes `send_probe` writes to each stream.
const PROBE_BYTES: usize = 64 << 10;

/// Stream count against whether the client probes.
#[derive(Clone, Copy)]
struct Probe {
	streams: usize,
	probe: bool,
}

impl fmt::Display for Probe {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let probe = if self.probe { "on" } else { "off" };
		write!(f, "streams_{}/probe_{probe}", self.streams)
	}
}

fn probes() -> Vec<Probe> {
	[1, 16, 256]
		.into_iter()
		.flat_map(|streams| [false, true].map(|probe| Probe { streams, probe }))
		.collect()
}

fn probe_pair(case: Probe) -> (Probe, Pair) {
	let pair = Pair::new(Setup {
		// 1 Gbit/s.
		client_probe_target: if case.probe { 125_000_000 } else { 0 },
		..Setup::default()
	});
	(case, pair)
}

/// Probing against stream count: a client whose controller always lets it probe fills every gap
/// in its sends with duplicates of the data in flight, up to a 1 Gbit/s target, so the cost per
/// unique byte against `off` is what probing adds to a sender.
#[bench(args = probes(), setup = probe_pair, bytes = |case: Probe| case.streams * PROBE_BYTES)]
fn send_probe((case, pair): &mut (Probe, Pair)) {
	send(pair, case.streams, 1, 4, PROBE_BYTES / 4);
}

fn main() {
	swole::main();
}
