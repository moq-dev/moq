//! What dividing a connection's estimate costs as its reservations fan out.
//!
//! - update: one reservation's ceiling moves and the session reads the new target.
//!   Swept over the reservations sharing the allocator, so an update that rescans
//!   every reservation shows up as a slope.
//! - grants/estimate, grants/demand, grants/ceiling: one change (every connection's
//!   estimate, one track's demand on each connection, one ceiling on each connection),
//!   then every observer drains its parked [`Consumer`], as each sender's task does.
//! - grants/peek: every reservation reads its slice once, as a sender does per frame.
//!
//! Each shape is connections x reservations, priority tiers, idle reservations and
//! observers per reservation. Every grant, read and polled, is checked against a
//! reference allocation before timing, so a variant cannot win by doing less.
//!
//! `BANDWIDTH_COUNTS=<iterations>` prints allocations, bytes allocated, wakes delivered
//! and waker clones per operation instead of timing, checking every grant after every
//! operation. A parked task that mints a fresh waiter clones its waker twice.
//! `BANDWIDTH_CASE=<op>/<shape>` keeps one case and skips the checks, so `perf stat`
//! can count instructions for it at two iteration counts and take the difference.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench bandwidth`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group};
use moq_net::bandwidth::{Allocator, Consumer, Producer, Rate, Reservation};
use moq_net::{broadcast, fuzz, kio, track};

/// Reservations sharing the allocator.
const RESERVATIONS: [usize; 4] = [1, 10, 1_000, 10_000];

/// An allocator holding `count` reservations, one track each, plus what keeps them alive.
fn table(count: usize) -> (Allocator, broadcast::Producer, Vec<track::Producer>, Vec<Reservation>) {
	let allocator = Allocator::unlimited();
	let broadcast = broadcast::Info::new().produce();
	let mut tracks = Vec::with_capacity(count);
	let mut reservations = Vec::with_capacity(count);
	for i in 0..count {
		let track = broadcast.create_track(format!("t{i}"), None).unwrap();
		reservations.push(allocator.reserve(&track.demand(), Rate::from_kbps(1_000)));
		tracks.push(track);
	}
	(allocator, broadcast, tracks, reservations)
}

fn bench_update(c: &mut Criterion) {
	let mut group = c.benchmark_group("bandwidth/update");
	for count in RESERVATIONS {
		group.bench_function(BenchmarkId::from_parameter(count), |b| {
			let (allocator, _broadcast, _tracks, reservations) = table(count);
			let mut target = fuzz::ProbeTarget::new(&allocator);
			target.poll().expect("the first poll reports the target");
			let touched = &reservations[count / 2];
			let mut raised = false;
			b.iter(|| {
				raised = !raised;
				touched.update(Rate::from_kbps(if raised { 2_000 } else { 1_000 }));
				std::hint::black_box(target.poll().expect("the target moved"));
			});
		});
	}
	group.finish();
}

// ── counting ───────────────────────────────────────────────────────────────

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static WAKES: AtomicUsize = AtomicUsize::new(0);
static WAITERS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

fn count(size: usize) {
	if COUNTING.load(Ordering::Relaxed) {
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		ALLOCATED.fetch_add(size, Ordering::Relaxed);
	}
}

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		count(layout.size());
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		count(layout.size());
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		count(new_size);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

// A waker that only counts: each clone is a parked task minting a fresh waiter, each
// wake a list delivering one entry, duplicates included.
static VTABLE: RawWakerVTable = RawWakerVTable::new(
	|_| {
		WAITERS.fetch_add(1, Ordering::Relaxed);
		RawWaker::new(std::ptr::null(), &VTABLE)
	},
	|_| {
		WAKES.fetch_add(1, Ordering::Relaxed);
	},
	|_| {
		WAKES.fetch_add(1, Ordering::Relaxed);
	},
	|_| {},
);

fn counting_waker() -> Waker {
	// SAFETY: the vtable ignores the data pointer, so a null one is valid for every call.
	unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

// ── shapes ─────────────────────────────────────────────────────────────────

/// One benchmark configuration.
#[derive(Copy, Clone)]
struct Shape {
	/// Connections, each its own estimate and allocator.
	connections: usize,
	/// Reservations per connection.
	reservations: usize,
	/// Distinct priorities, assigned round-robin.
	tiers: usize,
	/// Every other reservation's track has no subscriber.
	idle: bool,
	/// Parked consumers per reservation.
	observers: usize,
}

impl Shape {
	const fn new(reservations: usize, tiers: usize) -> Self {
		Self {
			connections: 1,
			reservations,
			tiers,
			idle: false,
			observers: 1,
		}
	}

	fn id(&self) -> String {
		format!(
			"{}c_{}r_{}t_{}i_{}o",
			self.connections,
			self.reservations,
			self.tiers,
			if self.idle { self.reservations / 2 } else { 0 },
			self.observers
		)
	}
}

/// Reservations alone, then distinct priorities, then each other axis on its own.
const SHAPES: [Shape; 11] = [
	Shape::new(1, 1),
	Shape::new(16, 1),
	Shape::new(64, 1),
	Shape::new(256, 1),
	Shape::new(16, 16),
	Shape::new(64, 64),
	Shape::new(256, 256),
	Shape::new(64, 4),
	Shape { idle: true, ..Shape::new(64, 1) },
	Shape {
		observers: 4,
		..Shape::new(64, 1)
	},
	Shape {
		connections: 4,
		..Shape::new(64, 1)
	},
];

/// The two estimates an estimate change alternates between, per reservation, chosen
/// so the lower tiers starve and the rest split unevenly.
const ESTIMATES_KBPS: [u64; 2] = [700, 450];

fn priority(shape: &Shape, index: usize) -> i8 {
	// Wraps past 127, which keeps up to 256 tiers distinct.
	(index % shape.tiers) as u8 as i8
}

fn ceiling(index: usize) -> Rate {
	// Five sizes, so tiers hold ties as well as distinct ceilings.
	Rate::from_kbps(500 + 250 * (index % 5) as u64)
}

// ── fixture ────────────────────────────────────────────────────────────────

/// A parked sender task reading one reservation's slice.
struct Observer {
	consumer: Consumer,
	park: kio::Park,
	/// The last slice reported, or `None` before any.
	last: Option<Option<Rate>>,
}

impl Observer {
	/// Poll until parked, as a woken task does.
	fn drain(&mut self, waker: &Waker) {
		let cx = Context::from_waker(waker);
		loop {
			let waiter = self.park.hold(&cx);
			match self.consumer.poll_changed(waiter) {
				Poll::Ready(Ok(rate)) => self.last = Some(rate),
				Poll::Ready(Err(err)) => panic!("estimate closed: {err}"),
				Poll::Pending => return,
			}
		}
	}
}

/// One reservation, its track, and the subscriber keeping it active.
struct Slot {
	track: track::Producer,
	subscriber: Option<track::Subscriber>,
	reservation: Reservation,
	priority: i8,
	max: Rate,
	observers: Vec<Observer>,
}

struct Connection {
	estimate: Producer,
	kbps: u64,
	_allocator: Allocator,
	_broadcast: broadcast::Producer,
	slots: Vec<Slot>,
}

impl Connection {
	fn new(shape: &Shape) -> Self {
		let estimate = Producer::new();
		let allocator = Allocator::new(estimate.consume());
		let broadcast = broadcast::Info::new().produce();
		let slots = (0..shape.reservations)
			.map(|index| {
				let priority = priority(shape, index);
				let info = track::Info::default().with_priority(priority);
				let track = broadcast.create_track(format!("t{index}"), info).unwrap();
				let subscriber = (!shape.idle || index % 2 == 0).then(|| track.subscribe(None));
				let max = ceiling(index);
				let reservation = allocator.reserve(&track.demand(), max);
				let observers = (0..shape.observers)
					.map(|_| Observer {
						consumer: reservation.consume(),
						park: kio::Park::default(),
						last: None,
					})
					.collect();
				Slot {
					track,
					subscriber,
					reservation,
					priority,
					max,
					observers,
				}
			})
			.collect();
		let kbps = ESTIMATES_KBPS[0] * shape.reservations as u64;
		estimate.set(Some(Rate::from_kbps(kbps))).unwrap();
		Self {
			estimate,
			kbps,
			_allocator: allocator,
			_broadcast: broadcast,
			slots,
		}
	}

	/// The slice `index` should read, by the allocator's contract: strict priority, max-min
	/// fair within a tier in ascending (ceiling, registration) order, idle tracks claiming
	/// nothing but their own reader still seeing what it would get back.
	fn expected(&self, index: usize) -> Rate {
		let mut budget = Rate::from_kbps(self.kbps).as_bps();
		let mut claims: Vec<(i8, Rate, usize)> = self
			.slots
			.iter()
			.enumerate()
			.filter(|(other, slot)| *other == index || slot.subscriber.is_some())
			.map(|(other, slot)| (slot.priority, slot.max, other))
			.collect();
		claims.sort_by_key(|(priority, max, other)| (std::cmp::Reverse(*priority), *max, *other));
		let mut start = 0;
		while start < claims.len() {
			let end = start + claims[start..].iter().take_while(|c| c.0 == claims[start].0).count();
			let mut remaining = (end - start) as u64;
			for (_, max, other) in &claims[start..end] {
				let grant = max.as_bps().min(budget / remaining);
				if *other == index {
					return Rate::from_bps(grant);
				}
				budget -= grant;
				remaining -= 1;
			}
			start = end;
		}
		unreachable!("the reservation always counts for itself")
	}

	fn check(&self) {
		for (index, slot) in self.slots.iter().enumerate() {
			let expected = Some(self.expected(index));
			assert_eq!(slot.reservation.rate(), expected, "reservation {index} rate");
			for observer in &slot.observers {
				assert_eq!(observer.last, Some(expected), "reservation {index} observed");
			}
		}
	}

	fn drain(&mut self, waker: &Waker) {
		for slot in &mut self.slots {
			for observer in &mut slot.observers {
				observer.drain(waker);
			}
		}
	}

	fn toggle_estimate(&mut self) {
		let per = self.kbps / self.slots.len() as u64;
		let next = if per == ESTIMATES_KBPS[0] { ESTIMATES_KBPS[1] } else { ESTIMATES_KBPS[0] };
		self.kbps = next * self.slots.len() as u64;
		self.estimate.set(Some(Rate::from_kbps(self.kbps))).unwrap();
	}

	/// The middle track gains or loses its subscriber.
	fn toggle_demand(&mut self) {
		let index = self.slots.len() / 2;
		let slot = &mut self.slots[index];
		slot.subscriber = match slot.subscriber.take() {
			Some(_) => None,
			None => Some(slot.track.subscribe(None)),
		};
	}

	/// The middle reservation doubles or restores its ceiling.
	fn toggle_ceiling(&mut self) {
		let index = self.slots.len() / 2;
		let slot = &mut self.slots[index];
		let base = ceiling(index);
		slot.max = if slot.max == base {
			Rate::from_bps(base.as_bps() * 2)
		} else {
			base
		};
		slot.reservation.update(slot.max);
	}

	fn peek(&self) {
		for slot in &self.slots {
			std::hint::black_box(slot.reservation.rate());
		}
	}
}

struct Fixture {
	connections: Vec<Connection>,
	waker: Waker,
}

impl Fixture {
	fn new(shape: &Shape) -> Self {
		let mut fixture = Self {
			connections: (0..shape.connections).map(|_| Connection::new(shape)).collect(),
			waker: counting_waker(),
		};
		fixture.drain();
		fixture
	}

	fn drain(&mut self) {
		for connection in &mut self.connections {
			connection.drain(&self.waker);
		}
	}

	fn check(&self) {
		for connection in &self.connections {
			connection.check();
		}
	}

	fn run(&mut self, op: Op) {
		match op {
			Op::Peek => {
				for connection in &self.connections {
					connection.peek();
				}
				return;
			}
			Op::Estimate => self.connections.iter_mut().for_each(Connection::toggle_estimate),
			Op::Demand => self.connections.iter_mut().for_each(Connection::toggle_demand),
			Op::Ceiling => self.connections.iter_mut().for_each(Connection::toggle_ceiling),
		}
		self.drain();
	}
}

#[derive(Copy, Clone)]
enum Op {
	Estimate,
	Demand,
	Ceiling,
	Peek,
}

const OPS: [(Op, &str); 4] = [
	(Op::Estimate, "estimate"),
	(Op::Demand, "demand"),
	(Op::Ceiling, "ceiling"),
	(Op::Peek, "peek"),
];

fn bench_grants(c: &mut Criterion) {
	for (op, name) in OPS {
		let mut group = c.benchmark_group(format!("bandwidth/grants/{name}"));
		group.warm_up_time(Duration::from_millis(200));
		group.measurement_time(Duration::from_secs(1));
		for shape in SHAPES {
			group.bench_function(BenchmarkId::from_parameter(shape.id()), |b| {
				let mut fixture = Fixture::new(&shape);
				fixture.check();
				// Both directions of the toggle, so either one doing less work shows here.
				for _ in 0..2 {
					fixture.run(op);
					fixture.check();
				}
				b.iter(|| fixture.run(op));
			});
		}
		group.finish();
	}
}

/// Deterministic counts per operation, checking every grant after each one.
fn counts(iterations: usize, case: Option<&str>) {
	println!("op,shape,allocations,bytes,wakes,waker_clones");
	for (op, name) in OPS {
		for shape in SHAPES {
			let label = format!("{name}/{}", shape.id());
			if case.is_some_and(|case| case != label) {
				continue;
			}
			let mut fixture = Fixture::new(&shape);
			fixture.check();
			let (mut allocations, mut bytes, mut wakes, mut waiters) = (0, 0, 0, 0);
			for _ in 0..iterations {
				for counter in [&ALLOCATIONS, &ALLOCATED, &WAKES, &WAITERS] {
					counter.store(0, Ordering::Relaxed);
				}
				COUNTING.store(true, Ordering::Relaxed);
				fixture.run(op);
				COUNTING.store(false, Ordering::Relaxed);
				allocations += ALLOCATIONS.load(Ordering::Relaxed);
				bytes += ALLOCATED.load(Ordering::Relaxed);
				wakes += WAKES.load(Ordering::Relaxed);
				waiters += WAITERS.load(Ordering::Relaxed);
				// Skipped when counting instructions, which would otherwise count the check.
				if case.is_none() {
					fixture.check();
				}
			}
			let per = |total: usize| total as f64 / iterations as f64;
			println!(
				"{name},{},{:.1},{:.0},{:.1},{:.1}",
				shape.id(),
				per(allocations),
				per(bytes),
				per(wakes),
				per(waiters)
			);
		}
	}
}

criterion_group!(benches, bench_update, bench_grants);

fn main() {
	if let Ok(iterations) = std::env::var("BANDWIDTH_COUNTS") {
		let iterations = iterations.parse().expect("BANDWIDTH_COUNTS is an iteration count");
		counts(iterations, std::env::var("BANDWIDTH_CASE").ok().as_deref());
		return;
	}
	benches();
	Criterion::default().configure_from_args().final_summary();
}
