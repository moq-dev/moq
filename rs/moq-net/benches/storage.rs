//! Prototype storage shapes for `GroupState`'s frames, benched against each other.
//!
//! Throwaway: the group storage benchmark quest (`quest/m1/group-storage-bench.md`)
//! records the numbers and the choice, and this file never merges. Each shape
//! implements only what `GroupState` does with its frames: append at the tail, read
//! by absolute cursor (up to `Prefetch::CAP` frames per call, as `Prefetch` does),
//! park at the edge, evict from the front, and refill an evicted front frame.
//!
//! - `deque`: today. A `VecDeque` plus an offset inside a `kio::Producer`.
//! - `runs`: Front eviction's sorted runs. The tail run sits inline, so a group that
//!   never evicts costs what `deque` does; refilled runs below it live in a `Vec`.
//!   Everything is under the one kio mutex.
//! - `log`: the append-only log. Write-once slots in refcounted segments, a Release
//!   store of the committed length, and lock-free cursor reads. Front eviction
//!   advances an atomic base and drops whole segments; a refill goes into a side run
//!   below the base, under its own mutex. Parking takes a waiter mutex.
//! - `ring`: a fixed-capacity ring of reused slots, each behind its own `RwLock`
//!   (tokio's broadcast design), since a reused slot can't be read lock-free without
//!   epoch reclamation. Same atomic length, base, side run, and parking as `log`.
//!
//! Workloads, swept over 1, 4 and 16 reader threads and 1, 16 and 1,024 readers:
//!
//! - `storage_write` / `storage_read`: one thread appends, or drains, n frames.
//! - `storage_single`: one-frame groups. Each round makes a group with one frame and
//!   finishes it; every reader opens it, reads the frame, and sees the end.
//! - `storage_edge`: one long-lived (multi-frame) group under front eviction, keeping
//!   64 frames. Each round appends `k` frames; every reader drains them and parks.
//! - `storage_refill`: the edge workload, plus each round refills one frame of an
//!   evicted front run. Half the readers follow the tail, half the refill.
//!
//! Run with `cargo bench -p moq-net --bench storage`.

#![expect(
	clippy::disallowed_methods,
	reason = "`iter_custom` timings measure the host clock on purpose"
)]

use std::{
	collections::VecDeque,
	hint::black_box,
	sync::{
		Arc, Mutex, OnceLock, RwLock, Weak,
		atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence},
	},
	task::{Poll, Waker},
	thread,
	time::{Duration, Instant},
};

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group};
use kio::{Waiter, WaiterList};

/// Worker threads the readers are spread over.
const THREADS: [usize; 3] = [1, 4, 16];

/// Readers of one group.
const READERS: [usize; 3] = [1, 16, 1024];

/// Frames a long-lived group keeps before evicting its front.
const WINDOW: u64 = 64;

/// Frames a read takes per call, like `Prefetch::CAP`.
const BATCH: usize = 8;

/// The refilled front run's length before it is evicted again and refilled from 0.
const REFILL_RUN: u64 = 64;

/// The shape of `moq_net::frame::Frame`: a timestamp, a refcounted payload, an arrival.
#[derive(Clone)]
struct Frame {
	_timestamp: Option<u64>,
	_payload: Bytes,
	_arrived: Instant,
}

/// Distinct payloads cycled through, so frames that are live together never share a
/// refcount, as received frames don't. More than a window plus a refill run.
const PAYLOADS: usize = 512;

/// Payloads cycled through by [`frame`].
struct Payloads {
	all: Vec<Bytes>,
	next: std::cell::Cell<usize>,
}

impl Payloads {
	fn new() -> Self {
		Self {
			all: (0..PAYLOADS).map(|_| Bytes::from(vec![0u8; 64])).collect(),
			next: Default::default(),
		}
	}
}

fn frame(payloads: &Payloads) -> Frame {
	let next = payloads.next.get();
	payloads.next.set((next + 1) % PAYLOADS);
	Frame {
		_timestamp: Some(0),
		_payload: payloads.all[next].clone(),
		_arrived: Instant::now(),
	}
}

/// The outcome of one read.
#[derive(Debug, PartialEq)]
enum Read {
	/// This many frames were appended to the output.
	Frames(usize),
	/// Nothing at the cursor yet; the waiter is registered.
	Pending,
	/// The group finished and the cursor is past its last frame.
	End,
	/// The cursor's frame was evicted and not refilled.
	Evicted,
}

trait Shape: Send + Sync + Sized + 'static {
	type Reader: Send;
	const NAME: &'static str;

	fn new(capacity: usize) -> Self;
	/// A reader with its cursor at `cursor`.
	/// Each reader holds a handle that keeps the group alive, as a `group::Consumer` does.
	fn reader(self: &Arc<Self>, cursor: u64) -> Self::Reader;
	fn cursor(reader: &mut Self::Reader) -> &mut u64;
	fn append(&self, frame: Frame);
	fn finish(&self);
	/// Evict every frame below `below`.
	fn evict(&self, below: u64);
	/// Write the evicted frame at `index` back. Refills come in order, from the run's start.
	fn refill(&self, index: u64, frame: Frame);
	/// Evict every refilled frame again.
	fn evict_refill(&self);
	/// Read from the cursor into `out`, advancing it.
	fn read(&self, reader: &mut Self::Reader, waiter: &Waiter, out: &mut Vec<Frame>) -> Read;
}

// ---------------------------------------------------------------------------------------
// deque: today's `VecDeque` + offset under the kio mutex.

#[derive(Default)]
struct DequeState {
	frames: VecDeque<Frame>,
	offset: u64,
	fin: bool,
}

struct Deque(kio::Producer<DequeState>);

struct KioReader<T> {
	consumer: kio::Consumer<T>,
	cursor: u64,
}

impl Shape for Deque {
	type Reader = KioReader<DequeState>;
	const NAME: &'static str = "deque";

	fn new(_: usize) -> Self {
		Self(kio::Producer::new(DequeState::default()))
	}

	fn reader(self: &Arc<Self>, cursor: u64) -> Self::Reader {
		KioReader {
			consumer: self.0.consume(),
			cursor,
		}
	}

	fn cursor(reader: &mut Self::Reader) -> &mut u64 {
		&mut reader.cursor
	}

	fn append(&self, frame: Frame) {
		self.0.write().ok().expect("open").frames.push_back(frame);
	}

	fn finish(&self) {
		self.0.write().ok().expect("open").fin = true;
	}

	fn evict(&self, below: u64) {
		let mut state = self.0.write().ok().expect("open");
		while state.offset < below && state.frames.pop_front().is_some() {
			state.offset += 1;
		}
	}

	fn refill(&self, _: u64, _: Frame) {
		unimplemented!("today keeps no refilled runs")
	}

	fn evict_refill(&self) {
		unimplemented!("today keeps no refilled runs")
	}

	fn read(&self, reader: &mut Self::Reader, waiter: &Waiter, out: &mut Vec<Frame>) -> Read {
		let cursor = reader.cursor;
		let res = reader.consumer.poll(waiter, |state| {
			if cursor < state.offset {
				return Poll::Ready(Read::Evicted);
			}
			let local = (cursor - state.offset) as usize;
			let count = state.frames.len().saturating_sub(local).min(BATCH);
			if count > 0 {
				out.extend(state.frames.range(local..local + count).cloned());
				Poll::Ready(Read::Frames(count))
			} else if state.fin {
				Poll::Ready(Read::End)
			} else {
				Poll::Pending
			}
		});
		match res {
			Poll::Ready(Ok(read)) => {
				if let Read::Frames(n) = read {
					reader.cursor += n as u64;
				}
				read
			}
			Poll::Ready(Err(_)) => panic!("closed"),
			Poll::Pending => Read::Pending,
		}
	}
}

// ---------------------------------------------------------------------------------------
// runs: sorted runs under the kio mutex, the tail run inline.

#[derive(Default)]
struct Run {
	start: u64,
	frames: VecDeque<Frame>,
}

impl Run {
	fn end(&self) -> u64 {
		self.start + self.frames.len() as u64
	}
}

#[derive(Default)]
struct RunsState {
	/// The run ending at the write position. Only it grows by append.
	tail: Run,
	/// Refilled runs below `tail.start`, sorted and disjoint.
	front: Vec<Run>,
	fin: bool,
}

impl RunsState {
	/// The run holding `index`, if any.
	fn find(&self, index: u64) -> Option<&Run> {
		if index >= self.tail.start {
			return Some(&self.tail);
		}
		let at = self.front.partition_point(|run| run.end() <= index);
		self.front.get(at).filter(|run| run.start <= index)
	}
}

struct Runs(kio::Producer<RunsState>);

impl Shape for Runs {
	type Reader = KioReader<RunsState>;
	const NAME: &'static str = "runs";

	fn new(_: usize) -> Self {
		Self(kio::Producer::new(RunsState::default()))
	}

	fn reader(self: &Arc<Self>, cursor: u64) -> Self::Reader {
		KioReader {
			consumer: self.0.consume(),
			cursor,
		}
	}

	fn cursor(reader: &mut Self::Reader) -> &mut u64 {
		&mut reader.cursor
	}

	fn append(&self, frame: Frame) {
		self.0.write().ok().expect("open").tail.frames.push_back(frame);
	}

	fn finish(&self) {
		self.0.write().ok().expect("open").fin = true;
	}

	fn evict(&self, below: u64) {
		// Only the tail run: refilled runs are evicted by `evict_refill`, as in the other
		// shapes, whose side runs this never touches.
		let mut state = self.0.write().ok().expect("open");
		let tail = &mut state.tail;
		while tail.start < below && tail.frames.pop_front().is_some() {
			tail.start += 1;
		}
	}

	fn refill(&self, index: u64, frame: Frame) {
		let mut state = self.0.write().ok().expect("open");
		assert!(index < state.tail.start, "a refill lands below the tail run");
		let at = state.front.partition_point(|run| run.end() < index);
		match state.front.get_mut(at) {
			Some(run) if run.end() == index => run.frames.push_back(frame),
			_ => state.front.insert(
				at,
				Run {
					start: index,
					frames: VecDeque::from([frame]),
				},
			),
		}
	}

	fn evict_refill(&self) {
		self.0.write().ok().expect("open").front.clear();
	}

	fn read(&self, reader: &mut Self::Reader, waiter: &Waiter, out: &mut Vec<Frame>) -> Read {
		let cursor = reader.cursor;
		let res = reader.consumer.poll(waiter, |state| {
			let Some(run) = state.find(cursor) else {
				// In a hole. A real reader would request the refill and wait for it.
				return if cursor < state.tail.start {
					Poll::Pending
				} else {
					Poll::Ready(Read::Evicted)
				};
			};
			let local = (cursor - run.start) as usize;
			let count = run.frames.len().saturating_sub(local).min(BATCH);
			if count > 0 {
				out.extend(run.frames.range(local..local + count).cloned());
				Poll::Ready(Read::Frames(count))
			} else if state.fin {
				Poll::Ready(Read::End)
			} else {
				Poll::Pending
			}
		});
		match res {
			Poll::Ready(Ok(read)) => {
				if let Read::Frames(n) = read {
					reader.cursor += n as u64;
				}
				read
			}
			Poll::Ready(Err(_)) => panic!("closed"),
			Poll::Pending => Read::Pending,
		}
	}
}

// ---------------------------------------------------------------------------------------
// Shared by log and ring: parking, and the side run below the base.

#[derive(Default)]
struct Park {
	waiters: Mutex<WaiterList>,
	/// Set under `waiters` by a parking reader, cleared by the waker. Paired with a
	/// SeqCst fence on both sides so a reader that re-checks after parking and a writer
	/// that checks after publishing can't both miss each other.
	parked: AtomicBool,
}

impl Park {
	fn register(&self, waiter: &Waiter) {
		let mut waiters = self.waiters.lock().unwrap();
		waiter.register(&mut waiters);
		self.parked.store(true, Ordering::Relaxed);
		drop(waiters);
		fence(Ordering::SeqCst);
	}

	fn wake(&self) {
		fence(Ordering::SeqCst);
		if !self.parked.load(Ordering::Relaxed) {
			return;
		}
		let mut list = {
			let mut waiters = self.waiters.lock().unwrap();
			self.parked.store(false, Ordering::Relaxed);
			waiters.take()
		};
		list.wake();
	}
}

/// Refilled frames below the base, sorted, behind their own mutex so a refill never
/// touches the tail's atomics.
#[derive(Default)]
struct Side {
	runs: Mutex<Vec<Run>>,
}

impl Side {
	fn refill(&self, index: u64, frame: Frame) {
		let mut runs = self.runs.lock().unwrap();
		let at = runs.partition_point(|run| run.end() < index);
		match runs.get_mut(at) {
			Some(run) if run.end() == index => run.frames.push_back(frame),
			_ => runs.insert(
				at,
				Run {
					start: index,
					frames: VecDeque::from([frame]),
				},
			),
		}
	}

	/// Read refilled frames at `cursor`, or `None` when it is in an unfilled hole.
	fn read(&self, cursor: u64, base: u64, out: &mut Vec<Frame>) -> Option<usize> {
		let runs = self.runs.lock().unwrap();
		let at = runs.partition_point(|run| run.end() <= cursor);
		let run = runs.get(at).filter(|run| run.start <= cursor)?;
		let local = (cursor - run.start) as usize;
		// Never read past the base: frames at or above it live in the main storage.
		let count = run
			.frames
			.len()
			.saturating_sub(local)
			.min(BATCH)
			.min((base - cursor) as usize);
		out.extend(run.frames.range(local..local + count).cloned());
		Some(count)
	}
}

// ---------------------------------------------------------------------------------------
// log: write-once slots in refcounted segments, lock-free cursor reads.

/// Slots in the largest segment. Segments double from `FIRST_SEGMENT` up to this, so a
/// one-frame group allocates a small one.
const MAX_SEGMENT: usize = 64;
const FIRST_SEGMENT: usize = 4;

struct Segment {
	start: u64,
	slots: Box<[OnceLock<Frame>]>,
	/// Weak, so a reader holding a stale segment pins only that one, not every segment
	/// after it.
	next: OnceLock<Weak<Segment>>,
}

impl Segment {
	fn new(start: u64, capacity: usize) -> Arc<Self> {
		Arc::new(Self {
			start,
			slots: (0..capacity).map(|_| OnceLock::new()).collect(),
			next: OnceLock::new(),
		})
	}

	fn end(&self) -> u64 {
		self.start + self.slots.len() as u64
	}
}

struct Log {
	/// Frames committed. Release-stored after the slot is set.
	len: AtomicU64,
	/// Frames below this were evicted.
	base: AtomicU64,
	fin: AtomicBool,
	/// The live segments, oldest first. Only writers and new readers lock it.
	segments: Mutex<VecDeque<Arc<Segment>>>,
	park: Park,
	side: Side,
}

struct LogReader {
	_log: Arc<Log>,
	segment: Arc<Segment>,
	cursor: u64,
}

impl Shape for Log {
	type Reader = LogReader;
	const NAME: &'static str = "log";

	fn new(_: usize) -> Self {
		Self {
			len: AtomicU64::new(0),
			base: AtomicU64::new(0),
			fin: AtomicBool::new(false),
			segments: Mutex::new(VecDeque::from([Segment::new(0, FIRST_SEGMENT)])),
			park: Park::default(),
			side: Side::default(),
		}
	}

	fn reader(self: &Arc<Self>, cursor: u64) -> Self::Reader {
		let segments = self.segments.lock().unwrap();
		let at = segments.partition_point(|segment| segment.end() <= cursor);
		let segment = segments.get(at).or(segments.back()).unwrap().clone();
		LogReader {
			_log: self.clone(),
			segment,
			cursor,
		}
	}

	fn cursor(reader: &mut Self::Reader) -> &mut u64 {
		&mut reader.cursor
	}

	fn append(&self, frame: Frame) {
		let mut segments = self.segments.lock().unwrap();
		let index = self.len.load(Ordering::Relaxed);
		let mut tail = segments.back().unwrap();
		if index == tail.end() {
			let capacity = (tail.slots.len() * 2).min(MAX_SEGMENT);
			let next = Segment::new(index, capacity);
			tail.next.set(Arc::downgrade(&next)).ok().unwrap();
			segments.push_back(next);
			tail = segments.back().unwrap();
		}
		tail.slots[(index - tail.start) as usize].set(frame).ok().unwrap();
		self.len.store(index + 1, Ordering::Release);
		drop(segments);
		self.park.wake();
	}

	fn finish(&self) {
		self.fin.store(true, Ordering::Release);
		self.park.wake();
	}

	fn evict(&self, below: u64) {
		self.base.fetch_max(below, Ordering::Release);
		let mut segments = self.segments.lock().unwrap();
		// Keep the tail segment even when empty: the writer appends into it.
		while segments.len() > 1 && segments[0].end() <= below {
			segments.pop_front();
		}
	}

	fn refill(&self, index: u64, frame: Frame) {
		self.side.refill(index, frame);
		self.park.wake();
	}

	fn evict_refill(&self) {
		self.side.runs.lock().unwrap().clear();
	}

	fn read(&self, reader: &mut Self::Reader, waiter: &Waiter, out: &mut Vec<Frame>) -> Read {
		let mut registered = false;
		loop {
			let cursor = reader.cursor;
			let base = self.base.load(Ordering::Acquire);
			if cursor < base {
				match self.side.read(cursor, base, out) {
					Some(n) if n > 0 => {
						reader.cursor += n as u64;
						return Read::Frames(n);
					}
					// An unfilled hole: a real reader requests the refill and waits for it.
					Some(_) | None => {}
				}
			} else {
				let len = self.len.load(Ordering::Acquire);
				if cursor < len {
					let count = ((len - cursor) as usize).min(BATCH);
					for i in 0..count as u64 {
						let index = cursor + i;
						while index >= reader.segment.end() {
							let Some(next) = reader.segment.next.get().and_then(Weak::upgrade) else {
								return Read::Evicted;
							};
							reader.segment = next;
						}
						let slot = &reader.segment.slots[(index - reader.segment.start) as usize];
						out.push(slot.get().expect("committed").clone());
					}
					reader.cursor += count as u64;
					return Read::Frames(count);
				}
				if self.fin.load(Ordering::Acquire) {
					return Read::End;
				}
			}
			if registered {
				return Read::Pending;
			}
			self.park.register(waiter);
			registered = true;
		}
	}
}

// ---------------------------------------------------------------------------------------
// ring: fixed capacity, reused slots each behind an RwLock.

struct Slot {
	index: u64,
	frame: Option<Frame>,
}

struct RingReader {
	_ring: Arc<Ring>,
	cursor: u64,
}

struct Ring {
	slots: Box<[RwLock<Slot>]>,
	mask: u64,
	len: AtomicU64,
	base: AtomicU64,
	fin: AtomicBool,
	/// Serializes writers; the slot locks only fence readers.
	writer: Mutex<()>,
	park: Park,
	side: Side,
}

impl Shape for Ring {
	type Reader = RingReader;
	const NAME: &'static str = "ring";

	fn new(capacity: usize) -> Self {
		// A ring can't grow without stopping its readers, so it is sized up front. A group
		// with no frame cap has no size to give it.
		let capacity = capacity.max(WINDOW as usize * 2).next_power_of_two();
		Self {
			slots: (0..capacity)
				.map(|_| RwLock::new(Slot { index: u64::MAX, frame: None }))
				.collect(),
			mask: capacity as u64 - 1,
			len: AtomicU64::new(0),
			base: AtomicU64::new(0),
			fin: AtomicBool::new(false),
			writer: Mutex::new(()),
			park: Park::default(),
			side: Side::default(),
		}
	}

	fn reader(self: &Arc<Self>, cursor: u64) -> Self::Reader {
		RingReader {
			_ring: self.clone(),
			cursor,
		}
	}

	fn cursor(reader: &mut Self::Reader) -> &mut u64 {
		&mut reader.cursor
	}

	fn append(&self, frame: Frame) {
		let _writer = self.writer.lock().unwrap();
		let index = self.len.load(Ordering::Relaxed);
		let capacity = self.mask + 1;
		if index >= capacity {
			// Overwriting the oldest slot evicts it.
			self.base.fetch_max(index + 1 - capacity, Ordering::Release);
		}
		let old = {
			let mut slot = self.slots[(index & self.mask) as usize].write().unwrap();
			slot.index = index;
			slot.frame.replace(frame)
		};
		self.len.store(index + 1, Ordering::Release);
		drop(old);
		self.park.wake();
	}

	fn finish(&self) {
		self.fin.store(true, Ordering::Release);
		self.park.wake();
	}

	fn evict(&self, below: u64) {
		// Slots are reclaimed when overwritten; the base only hides them.
		self.base.fetch_max(below, Ordering::Release);
	}

	fn refill(&self, index: u64, frame: Frame) {
		self.side.refill(index, frame);
		self.park.wake();
	}

	fn evict_refill(&self) {
		self.side.runs.lock().unwrap().clear();
	}

	fn read(&self, reader: &mut Self::Reader, waiter: &Waiter, out: &mut Vec<Frame>) -> Read {
		let cursor = &mut reader.cursor;
		let mut registered = false;
		loop {
			let base = self.base.load(Ordering::Acquire);
			if *cursor < base {
				match self.side.read(*cursor, base, out) {
					Some(n) if n > 0 => {
						*cursor += n as u64;
						return Read::Frames(n);
					}
					Some(_) | None => {}
				}
			} else {
				let len = self.len.load(Ordering::Acquire);
				if *cursor < len {
					let count = ((len - *cursor) as usize).min(BATCH);
					for i in 0..count as u64 {
						let index = *cursor + i;
						let slot = self.slots[(index & self.mask) as usize].read().unwrap();
						if slot.index != index {
							return Read::Evicted;
						}
						out.push(slot.frame.clone().expect("committed"));
					}
					*cursor += count as u64;
					return Read::Frames(count);
				}
				if self.fin.load(Ordering::Acquire) {
					return Read::End;
				}
			}
			if registered {
				return Read::Pending;
			}
			self.park.register(waiter);
			registered = true;
		}
	}
}

// ---------------------------------------------------------------------------------------
/// Drain everything readable, then park. Returns the frames read.
fn drain<S: Shape>(shape: &S, reader: &mut S::Reader, waiter: &Waiter, out: &mut Vec<Frame>) -> usize {
	let mut total = 0;
	loop {
		match shape.read(reader, waiter, out) {
			Read::Frames(n) => total += n,
			Read::Pending | Read::End => break,
			Read::Evicted => panic!("evicted at {}", *S::cursor(reader)),
		}
		black_box(out.drain(..).count());
	}
	total
}

fn waiter() -> Waiter {
	Waiter::new(Waker::noop().clone())
}

// ---------------------------------------------------------------------------------------
// Benches.

const COUNTS: [usize; 3] = [1, 60, 8192];

fn bench_write<S: Shape>(c: &mut Criterion, payload: &Payloads) {
	let mut g = c.benchmark_group("storage_write");
	for n in COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_function(BenchmarkId::new(S::NAME, n), |b| {
			b.iter_batched(
				|| (),
				|()| {
					let shape = S::new(n);
					for _ in 0..n {
						shape.append(frame(payload));
					}
					shape.finish();
					shape
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

fn bench_read<S: Shape>(c: &mut Criterion, payload: &Payloads) {
	let mut g = c.benchmark_group("storage_read");
	for n in COUNTS {
		let shape = Arc::new(S::new(n));
		for _ in 0..n {
			shape.append(frame(payload));
		}
		shape.finish();
		let waiter = waiter();
		g.throughput(Throughput::Elements(n as u64));
		g.bench_function(BenchmarkId::new(S::NAME, n), |b| {
			let mut out = Vec::with_capacity(BATCH);
			b.iter_batched(
				|| shape.reader(0),
				|mut reader| {
					assert_eq!(drain(&*shape, &mut reader, &waiter, &mut out), n);
					reader
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

// Worker threads, as in kio's `contended.rs`, each timing its own CPU per pass.

type Work = Box<dyn FnMut() + Send>;

struct Crew {
	control: Arc<Control>,
	threads: Vec<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Control {
	round: AtomicU64,
	done: AtomicUsize,
	/// Thread CPU nanoseconds the workers spent in their passes, excluding the spin
	/// between rounds.
	cpu: AtomicU64,
}

const STOP: u64 = u64::MAX;

impl Crew {
	fn new(work: Vec<Work>) -> Self {
		let control = Arc::new(Control::default());
		let threads = work
			.into_iter()
			.map(|mut work| {
				let control = control.clone();
				thread::spawn(move || {
					let mut seen = 0;
					loop {
						seen = spin(|| {
							let round = control.round.load(Ordering::Acquire);
							(round != seen).then_some(round)
						});
						if seen == STOP {
							return;
						}
						let start = cpu();
						work();
						control.cpu.fetch_add(cpu() - start, Ordering::Relaxed);
						control.done.fetch_add(1, Ordering::AcqRel);
					}
				})
			})
			.collect();
		Self { control, threads }
	}

	fn run(&self) {
		self.control.done.store(0, Ordering::Relaxed);
		self.control.round.fetch_add(1, Ordering::Release);
		let threads = self.threads.len();
		spin(|| {
			assert!(!self.threads.iter().any(|t| t.is_finished()), "worker panicked");
			(self.control.done.load(Ordering::Acquire) == threads).then_some(())
		});
	}
}

impl Drop for Crew {
	fn drop(&mut self) {
		self.control.round.store(STOP, Ordering::Release);
		for thread in self.threads.drain(..) {
			let result = thread.join();
			if !thread::panicking() {
				result.expect("worker panicked");
			}
		}
	}
}

fn spin<R>(mut f: impl FnMut() -> Option<R>) -> R {
	let mut spins = 0u32;
	loop {
		if let Some(r) = f() {
			return r;
		}
		if spins < 1_000 {
			spins += 1;
			std::hint::spin_loop();
		} else {
			thread::yield_now();
		}
	}
}

/// This thread's CPU time in nanoseconds.
fn cpu() -> u64 {
	let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	// SAFETY: `time` is a valid output pointer for a supported clock.
	let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
	assert_eq!(result, 0);
	time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

fn sweep() -> impl Iterator<Item = (usize, usize)> {
	THREADS
		.into_iter()
		.flat_map(|threads| READERS.into_iter().map(move |readers| (threads, readers)))
		.filter(|(threads, readers)| readers >= threads)
}

/// Run rounds of `write` then one crew pass, printing per-reader costs: the median and
/// 10th percentile round wall time, and the CPU of the writer plus every worker.
///
/// The host is shared, so a preempted round is an outlier the median and p10 skip,
/// and CPU time excludes descheduled time (but also time blocked on a futex).
fn rounds(label: &str, readers: usize, crew: &Crew, mut write: impl FnMut()) {
	let budget = Duration::from_millis(ROUND_BUDGET_MS);
	let running = quiet();
	for _ in 0..WARM_ROUNDS {
		write();
		crew.run();
	}
	crew.control.cpu.store(0, Ordering::Relaxed);
	let mut walls = Vec::new();
	let mut writer = 0;
	let started = Instant::now();
	while walls.len() < MAX_ROUNDS && (walls.len() < MIN_ROUNDS || started.elapsed() < budget) {
		let wall = Instant::now();
		let cpu_start = cpu();
		write();
		writer += cpu() - cpu_start;
		crew.run();
		walls.push(wall.elapsed().as_nanos() as u64);
	}
	let n = walls.len();
	walls.sort_unstable();
	let per = |ns: u64| ns as f64 / readers as f64;
	let cpu = (writer + crew.control.cpu.load(Ordering::Relaxed)) as f64 / (n * readers) as f64;
	println!(
		"ROUND\t{label}\t{readers}\t{:.1}\t{:.1}\t{:.1}\t{n}\t{running}\t{}",
		per(walls[n / 2]),
		per(walls[n / 10]),
		cpu,
		running_now(),
	);
}

/// Runnable threads host-wide, from `/proc/loadavg`.
fn running_now() -> usize {
	let loadavg = std::fs::read_to_string("/proc/loadavg").unwrap();
	let field = loadavg.split_whitespace().nth(3).unwrap();
	field.split('/').next().unwrap().parse().unwrap()
}

/// Wait up to `QUIET_WAIT` for at most `STORAGE_GATE` (default 8) runnable threads
/// host-wide before a case, returning the count it started at.
fn quiet() -> usize {
	let gate: usize = std::env::var("STORAGE_GATE").map_or(8, |gate| gate.parse().unwrap());
	let start = Instant::now();
	loop {
		let running = running_now();
		if running <= gate || start.elapsed() > QUIET_WAIT {
			return running;
		}
		thread::sleep(Duration::from_millis(20));
	}
}

const QUIET_WAIT: Duration = Duration::from_secs(20);

const WARM_ROUNDS: usize = 200;
const MIN_ROUNDS: usize = 500;
const MAX_ROUNDS: usize = 50_000;
const ROUND_BUDGET_MS: u64 = 1_500;

/// The fixed cost of releasing and joining the workers, with no reads.
fn baseline(filter: &str, threads: usize, readers: usize) {
	{
		let label = format!("baseline/none/threads_{threads}");
		if !label.contains(filter) {
			return;
		}
		let crew = Crew::new((0..threads).map(|_| -> Work { Box::new(|| {}) }).collect());
		rounds(&label, readers, &crew, || {});
	}
}

/// One-frame groups: each round makes a group, and every reader opens and drains it.
fn single<S: Shape>(payload: &Payloads, filter: &str, threads: usize, readers: usize) {
	{
		let label = format!("single/{}/threads_{threads}", S::NAME);
		if !label.contains(filter) {
			return;
		}
		// One mailbox per worker, so handing out the group never contends between them.
		let mailboxes: Vec<Arc<Mutex<Option<Arc<S>>>>> = (0..threads).map(|_| Default::default()).collect();
		let work = mailboxes
			.iter()
			.enumerate()
			.map(|(t, mailbox)| -> Work {
				let mailbox = mailbox.clone();
				let mine = (t..readers).step_by(threads).count();
				let waiters: Vec<Waiter> = (0..mine).map(|_| waiter()).collect();
				let mut out = Vec::with_capacity(BATCH);
				Box::new(move || {
					let shape = mailbox.lock().unwrap().take().expect("a group");
					for waiter in &waiters {
						let mut reader = shape.reader(0);
						assert_eq!(drain(&*shape, &mut reader, waiter, &mut out), 1);
					}
				})
			})
			.collect();
		let crew = Crew::new(work);
		rounds(&label, readers, &crew, || {
			let shape = Arc::new(S::new(1));
			shape.append(frame(payload));
			shape.finish();
			for mailbox in &mailboxes {
				*mailbox.lock().unwrap() = Some(shape.clone());
			}
		});
	}
}

/// A long-lived group under front eviction. Each round appends `k` frames and evicts
/// down to `WINDOW`; with `refill`, it also refills one frame of the evicted front run,
/// which half the readers follow.
fn edge<S: Shape>(payload: &Payloads, filter: &str, case: Case, threads: usize, readers: usize) {
	let Case { name, k, refill } = case;
	{
		if refill && readers < 2 * threads {
			return;
		}
		let label = format!("{name}/{}/threads_{threads}", S::NAME);
		if !label.contains(filter) {
			return;
		}
		let shape = Arc::new(S::new(0));
		// Start with an evicted front well past the refill run.
		let warm = 4 * WINDOW;
		for _ in 0..warm {
			shape.append(frame(payload));
		}
		shape.evict(warm - WINDOW);
		let mut sets: Vec<Vec<(S::Reader, Waiter, bool)>> = (0..threads).map(|_| Vec::new()).collect();
		for i in 0..readers {
			let follows_refill = refill && i % 2 == 1;
			let mut reader = shape.reader(if follows_refill { 0 } else { warm });
			let waiter = waiter();
			assert_eq!(shape.read(&mut reader, &waiter, &mut Vec::new()), Read::Pending);
			sets[i % threads].push((reader, waiter, follows_refill));
		}
		let work = sets
			.into_iter()
			.map(|mut set| -> Work {
				let shape = shape.clone();
				let mut out = Vec::with_capacity(BATCH);
				Box::new(move || {
					for (reader, waiter, follows_refill) in &mut set {
						if *follows_refill {
							let cursor = S::cursor(reader);
							if *cursor == REFILL_RUN {
								*cursor = 0;
							}
							assert_eq!(drain(&*shape, reader, waiter, &mut out), 1);
						} else {
							assert_eq!(drain(&*shape, reader, waiter, &mut out), k as usize);
						}
					}
				})
			})
			.collect();
		let crew = Crew::new(work);

		let mut len = warm;
		let mut refilled = 0;
		rounds(&label, readers, &crew, || {
			for _ in 0..k {
				shape.append(frame(payload));
			}
			len += k;
			shape.evict(len - WINDOW);
			if refill {
				if refilled == REFILL_RUN {
					shape.evict_refill();
					refilled = 0;
				}
				shape.refill(refilled, frame(payload));
				refilled += 1;
			}
		});
	}
}

/// A long-lived group workload.
#[derive(Clone, Copy)]
struct Case {
	name: &'static str,
	k: u64,
	refill: bool,
}

const EDGE: Case = Case {
	name: "edge",
	k: 1,
	refill: false,
};
const BURST: Case = Case {
	name: "burst",
	k: BATCH as u64,
	refill: false,
};
const REFILL: Case = Case {
	name: "refill",
	k: 1,
	refill: true,
};

fn uncontended<S: Shape>(c: &mut Criterion) {
	let payload = Payloads::new();
	bench_write::<S>(c, &payload);
	bench_read::<S>(c, &payload);
}

fn benches(c: &mut Criterion) {
	uncontended::<Deque>(c);
	uncontended::<Runs>(c);
	uncontended::<Log>(c);
	uncontended::<Ring>(c);
}

criterion_group! {
	name = group;
	config = Criterion::default().warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));
	targets = benches
}

/// `STORAGE_ROUNDS=<filter>` runs the contended round workloads whose label contains
/// the filter (empty for all) instead of the Criterion benches. Every shape runs each
/// case back to back, so they share the host's load at the time.
fn main() {
	if let Some(filter) = std::env::var_os("STORAGE_ROUNDS") {
		let filter = filter.to_string_lossy();
		let payload = Payloads::new();
		for (threads, readers) in sweep() {
			baseline(&filter, threads, readers);
			single::<Deque>(&payload, &filter, threads, readers);
			single::<Runs>(&payload, &filter, threads, readers);
			single::<Log>(&payload, &filter, threads, readers);
			single::<Ring>(&payload, &filter, threads, readers);
			for case in [EDGE, BURST] {
				edge::<Deque>(&payload, &filter, case, threads, readers);
				edge::<Runs>(&payload, &filter, case, threads, readers);
				edge::<Log>(&payload, &filter, case, threads, readers);
				edge::<Ring>(&payload, &filter, case, threads, readers);
			}
			edge::<Runs>(&payload, &filter, REFILL, threads, readers);
			edge::<Log>(&payload, &filter, REFILL, threads, readers);
			edge::<Ring>(&payload, &filter, REFILL, threads, readers);
		}
		return;
	}
	group();
	Criterion::default().configure_from_args().final_summary();
}
