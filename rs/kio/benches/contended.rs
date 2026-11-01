//! Cross-core contention on one kio channel.
//!
//! `channel.rs` drives every consumer from one thread, so the channel mutex never
//! changes hands between cores. Here one producer (the bench thread) feeds consumers
//! spread round-robin over 1, 4, or 16 worker threads, swept over the consumer count,
//! so a cost that grows with fan-out or with cores shows up as a slope. Throughput is
//! per consumer: a flat time per element across threads means no contention.
//!
//! - `contended_notify`: the producer writes a `u64`, waking every consumer; each
//!   observes the new value and parks again, as in `channel_notify_cycle`.
//! - `contended_repoll`: consumers re-poll a value that has not changed, as when a
//!   deadline or a second channel wakes their task. Workers run free, with no
//!   producer and no per-pass sync.
//! - `contended_log`: a `GroupState`-shaped log. The producer appends a frame to a
//!   `VecDeque`; each consumer reads its cursor's frame, cloning it out under the
//!   lock, and parks again.
//! - `contended_log_lockfree`: the same log on [`kio::log`], read without the lock;
//!   each consumer takes it only to park.
//!
//! The notify and log cycles include releasing and joining the workers each
//! iteration, a fixed cost that matters only at a few consumers per thread.
//!
//! Run with `cargo bench -p kio --bench contended`.
#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "the bench measures real cross-core time on the host clock"
)]

use std::{
	collections::VecDeque,
	hint::black_box,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
	task::{Poll, Waker},
	thread,
	time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kio::{Consumer, Producer, Waiter};

/// Worker threads the consumers are spread over.
const THREADS: [usize; 3] = [1, 4, 16];

/// Consumers on the one channel, from a lone reader up to a large broadcast.
const CONSUMERS: [usize; 4] = [1, 16, 256, 4096];

/// Frames the log keeps before evicting its oldest, like a group's cache.
const LOG_FRAMES: usize = 64;

/// One pass of a worker over its consumers.
type Work = Box<dyn FnMut() + Send>;

/// Worker threads that each run a number of passes when the driver releases a round.
struct Crew {
	control: Arc<Control>,
	threads: Vec<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Control {
	/// Rounds released so far, or [`STOP`].
	round: AtomicU64,
	/// Passes each worker runs in the current round.
	passes: AtomicU64,
	/// Workers done with the current round.
	done: AtomicUsize,
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
						for _ in 0..control.passes.load(Ordering::Relaxed) {
							work();
						}
						control.done.fetch_add(1, Ordering::AcqRel);
					}
				})
			})
			.collect();
		Self { control, threads }
	}

	/// Run `passes` passes on every worker and wait for all of them.
	fn run(&self, passes: u64) {
		self.control.done.store(0, Ordering::Relaxed);
		self.control.passes.store(passes, Ordering::Relaxed);
		self.control.round.fetch_add(1, Ordering::Release);
		let threads = self.threads.len();
		spin(|| {
			// A panicked worker never reports done, so fail instead of spinning forever.
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
			// Already unwinding from `run`'s check: a second panic would abort.
			if !thread::panicking() {
				result.expect("worker panicked");
			}
		}
	}
}

/// Busy-wait for `f`, yielding once spinning has gone on long enough that the
/// machine may be oversubscribed.
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

/// One consumer, its task's waiter, and the last value (or log index) it read.
struct Reader<T> {
	consumer: Consumer<T>,
	waiter: Waiter,
	seen: u64,
}

/// `consumers` readers parked on `producer`, dealt round-robin into `threads` sets.
fn readers<T>(producer: &Producer<T>, consumers: usize, threads: usize) -> Vec<Vec<Reader<T>>> {
	let mut sets: Vec<Vec<Reader<T>>> = (0..threads).map(|_| Vec::new()).collect();
	for i in 0..consumers {
		let reader = Reader {
			consumer: producer.consume(),
			waiter: Waiter::new(Waker::noop().clone()),
			seen: 0,
		};
		let parked = reader.consumer.poll(&reader.waiter, |_| Poll::<()>::Pending);
		assert!(parked.is_pending());
		drop(parked);
		sets[i % threads].push(reader);
	}
	sets
}

/// Every swept (threads, consumers) pair with at least one consumer per thread.
fn sweep() -> impl Iterator<Item = (usize, usize)> {
	THREADS
		.into_iter()
		.flat_map(|threads| CONSUMERS.into_iter().map(move |consumers| (threads, consumers)))
		.filter(|(threads, consumers)| consumers >= threads)
}

fn bench_notify(c: &mut Criterion) {
	let mut group = c.benchmark_group("contended_notify");
	for (threads, consumers) in sweep() {
		group.throughput(Throughput::Elements(consumers as u64));
		let id = BenchmarkId::new(format!("threads_{threads}"), consumers);
		group.bench_function(id, |b| {
			let producer = Producer::new(0u64);
			let work = readers(&producer, consumers, threads)
				.into_iter()
				.map(|mut set| -> Work {
					Box::new(move || {
						for reader in &mut set {
							let seen = reader.seen;
							let changed = |state: &kio::Ref<'_, u64>| {
								if **state != seen {
									Poll::Ready(**state)
								} else {
									Poll::Pending
								}
							};
							let Poll::Ready(Ok(value)) = reader.consumer.poll(&reader.waiter, changed) else {
								panic!("a published value must be ready");
							};
							reader.seen = black_box(value);
							let changed = |state: &kio::Ref<'_, u64>| {
								if **state != value {
									Poll::Ready(())
								} else {
									Poll::Pending
								}
							};
							assert!(reader.consumer.poll(&reader.waiter, changed).is_pending());
						}
					})
				})
				.collect();
			let crew = Crew::new(work);

			let mut next = 0;
			b.iter_custom(|iters| {
				let start = Instant::now();
				for _ in 0..iters {
					next += 1;
					*producer.write().ok().expect("open") = next;
					crew.run(1);
				}
				start.elapsed()
			});
		});
	}
	group.finish();
}

fn bench_repoll(c: &mut Criterion) {
	let mut group = c.benchmark_group("contended_repoll");
	for (threads, consumers) in sweep() {
		group.throughput(Throughput::Elements(consumers as u64));
		let id = BenchmarkId::new(format!("threads_{threads}"), consumers);
		group.bench_function(id, |b| {
			let producer = Producer::new(0u64);
			let work = readers(&producer, consumers, threads)
				.into_iter()
				.map(|set| -> Work {
					Box::new(move || {
						for reader in &set {
							let changed = |state: &kio::Ref<'_, u64>| {
								if **state != reader.seen {
									Poll::Ready(())
								} else {
									Poll::Pending
								}
							};
							assert!(reader.consumer.poll(&reader.waiter, changed).is_pending());
						}
					})
				})
				.collect();
			let crew = Crew::new(work);

			b.iter_custom(|iters| {
				let start = Instant::now();
				crew.run(iters);
				start.elapsed()
			});
		});
	}
	group.finish();
}

/// A frame payload. Like `Bytes`, cloning it bumps a refcount shared by every reader.
type Frame = Arc<u64>;

/// The shape of `GroupState`'s frame cache: appended at the back, read by absolute index.
#[derive(Default)]
struct Log {
	frames: VecDeque<Frame>,
	/// Absolute index of `frames[0]`.
	offset: u64,
}

fn bench_log(c: &mut Criterion) {
	let mut group = c.benchmark_group("contended_log");
	for (threads, consumers) in sweep() {
		group.throughput(Throughput::Elements(consumers as u64));
		let id = BenchmarkId::new(format!("threads_{threads}"), consumers);
		group.bench_function(id, |b| {
			let producer = Producer::new(Log::default());
			let work = readers(&producer, consumers, threads)
				.into_iter()
				.map(|mut set| -> Work {
					Box::new(move || {
						for reader in &mut set {
							let cursor = reader.seen;
							let next = |log: &kio::Ref<'_, Log>| match log.frames.get((cursor - log.offset) as usize) {
								Some(frame) => Poll::Ready(frame.clone()),
								None => Poll::Pending,
							};
							let Poll::Ready(Ok(frame)) = reader.consumer.poll(&reader.waiter, next) else {
								panic!("an appended frame must be ready");
							};
							black_box(frame);
							reader.seen += 1;

							let cursor = reader.seen;
							let next = |log: &kio::Ref<'_, Log>| match log.frames.get((cursor - log.offset) as usize) {
								Some(_) => Poll::Ready(()),
								None => Poll::Pending,
							};
							assert!(reader.consumer.poll(&reader.waiter, next).is_pending());
						}
					})
				})
				.collect();
			let crew = Crew::new(work);

			let mut index = 0;
			b.iter_custom(|iters| {
				let start = Instant::now();
				for _ in 0..iters {
					{
						let mut log = producer.write().ok().expect("open");
						log.frames.push_back(Arc::new(index));
						if log.frames.len() > LOG_FRAMES {
							log.frames.pop_front();
							log.offset += 1;
						}
					}
					index += 1;
					crew.run(1);
				}
				start.elapsed()
			});
		});
	}
	group.finish();
}

/// `contended_log` on [`kio::log`]: the frames move out of the channel into the log,
/// which readers read without its lock. The channel stays for parking, so a reader
/// takes the lock only to park at the edge, once per round instead of twice.
fn bench_log_lockfree(c: &mut Criterion) {
	let mut group = c.benchmark_group("contended_log_lockfree");
	for (threads, consumers) in sweep() {
		group.throughput(Throughput::Elements(consumers as u64));
		let id = BenchmarkId::new(format!("threads_{threads}"), consumers);
		group.bench_function(id, |b| {
			let producer = Producer::new(kio::log::Writer::<Frame>::new(0));
			let work = readers(&producer, consumers, threads)
				.into_iter()
				.map(|set| -> Work {
					let mut set: Vec<_> = set
						.into_iter()
						.map(|reader| {
							let log = reader.consumer.read().reader();
							(reader, log)
						})
						.collect();
					Box::new(move || {
						for (reader, log) in &mut set {
							{
								let mut read = log.read();
								let Ok(Some(frame)) = read.get(reader.seen) else {
									panic!("an appended frame must be ready");
								};
								black_box(frame.clone());
								reader.seen += 1;
								assert_eq!(read.get(reader.seen), Ok(None));
							}
							let cursor = reader.seen;
							let next = |log: &kio::Ref<'_, kio::log::Writer<Frame>>| match log.end() > cursor {
								true => Poll::Ready(()),
								false => Poll::Pending,
							};
							assert!(reader.consumer.poll(&reader.waiter, next).is_pending());
						}
					})
				})
				.collect();
			let crew = Crew::new(work);

			let mut index = 0;
			b.iter_custom(|iters| {
				let start = Instant::now();
				for _ in 0..iters {
					{
						let mut log = producer.write().ok().expect("open");
						log.push(Arc::new(index));
						let end = log.end();
						log.evict(end.saturating_sub(LOG_FRAMES as u64));
					}
					index += 1;
					crew.run(1);
				}
				start.elapsed()
			});
		});
	}
	group.finish();
}

criterion_group! {
	name = benches;
	config = Criterion::default().warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));
	targets = bench_notify, bench_repoll, bench_log, bench_log_lockfree
}
criterion_main!(benches);
