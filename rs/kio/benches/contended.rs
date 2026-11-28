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
//! - `contended_repoll_watch`: the same re-poll through a [`kio::Watch`], which finds the
//!   version unchanged and parks without the value lock or the predicate.
//! - `contended_log`: a `GroupState`-shaped log. The producer appends a frame to a
//!   `VecDeque`; each consumer reads its cursor's frame, cloning it out under the
//!   lock, and parks again.
//! - `contended_log_lockfree`: the same log on [`kio::log`], read without the lock;
//!   each consumer takes it only to park.
//!
//! The notify and log cycles include releasing and joining the workers each
//! call, a fixed cost that matters only at a few consumers per thread. The re-poll
//! benchmarks time themselves, releasing every pass of a sample in one round.
//!
//! Run with `cargo bench -p kio --bench contended`.
#![expect(clippy::disallowed_methods, reason = "host time times the re-poll rounds")]

use std::{
	collections::VecDeque,
	fmt,
	hint::black_box,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
	task::{Poll, Waker},
	thread,
	time::{Duration, Instant},
};

use kio::{Consumer, Producer, Waiter};
use swole::{Iterations, bench};

/// Worker threads the consumers are spread over.
const THREADS: [usize; 3] = [1, 4, 16];

/// Consumers on the one channel, from a lone reader up to a large broadcast.
const CONSUMERS: [usize; 4] = [1, 16, 256, 4096];

/// Frames the log keeps before evicting its oldest, like a bounded cache.
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

/// Consumers on one channel spread over worker threads, named `threads_<threads>/<consumers>`.
#[derive(Clone, Copy)]
struct Case {
	threads: usize,
	consumers: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "threads_{}/{}", self.threads, self.consumers)
	}
}

/// Every swept (threads, consumers) pair with at least one consumer per thread.
fn cases() -> Vec<Case> {
	THREADS
		.into_iter()
		.flat_map(|threads| CONSUMERS.into_iter().map(move |consumers| Case { threads, consumers }))
		.filter(|case| case.consumers >= case.threads)
		.collect()
}

/// A channel, the crew reading it, and the next value the producer writes.
struct Contended<T> {
	producer: Producer<T>,
	crew: Crew,
	next: u64,
}

impl<T> Contended<T> {
	/// Deal `case`'s readers over its threads, each set read by one pass of `work`.
	fn new(case: Case, producer: Producer<T>, work: impl FnMut(Vec<Reader<T>>) -> Work) -> Self {
		let work = readers(&producer, case.consumers, case.threads)
			.into_iter()
			.map(work)
			.collect();
		Self {
			producer,
			crew: Crew::new(work),
			next: 0,
		}
	}
}

fn notify(case: Case) -> Contended<u64> {
	Contended::new(case, Producer::new(0u64), |mut set| -> Work {
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
}

#[bench(args = cases(), setup = notify, items = |case: Case| case.consumers)]
fn contended_notify(contended: &mut Contended<u64>) {
	contended.next += 1;
	*contended.producer.write().ok().expect("open") = contended.next;
	contended.crew.run(1);
}

fn repoll(case: Case) -> Contended<u64> {
	Contended::new(case, Producer::new(0u64), |set| -> Work {
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
}

#[bench(args = cases(), setup = repoll, items = |case: Case| case.consumers)]
fn contended_repoll(contended: &Contended<u64>, iterations: Iterations) -> Duration {
	let start = Instant::now();
	contended.crew.run(iterations.get());
	start.elapsed()
}

fn repoll_watch(case: Case) -> Contended<u64> {
	Contended::new(case, Producer::new(0u64), |set| -> Work {
		let mut set: Vec<_> = set
			.into_iter()
			.map(|reader| (kio::Watch::new(reader.consumer), reader.waiter, reader.seen))
			.collect();
		Box::new(move || {
			for (watch, waiter, seen) in &mut set {
				let seen = *seen;
				let changed = |state: &kio::Ref<'_, u64>| {
					if **state != seen {
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				};
				assert!(watch.poll(waiter, changed).is_pending());
			}
		})
	})
}

#[bench(args = cases(), setup = repoll_watch, items = |case: Case| case.consumers)]
fn contended_repoll_watch(contended: &Contended<u64>, iterations: Iterations) -> Duration {
	let start = Instant::now();
	contended.crew.run(iterations.get());
	start.elapsed()
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

fn log(case: Case) -> Contended<Log> {
	Contended::new(case, Producer::new(Log::default()), |mut set| -> Work {
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
}

#[bench(args = cases(), setup = log, items = |case: Case| case.consumers)]
fn contended_log(contended: &mut Contended<Log>) {
	{
		let mut log = contended.producer.write().ok().expect("open");
		log.frames.push_back(Arc::new(contended.next));
		if log.frames.len() > LOG_FRAMES {
			log.frames.pop_front();
			log.offset += 1;
		}
	}
	contended.next += 1;
	contended.crew.run(1);
}

fn log_lockfree(case: Case) -> Contended<kio::log::Writer<Frame>> {
	Contended::new(case, Producer::new(kio::log::Writer::<Frame>::new(0)), |set| -> Work {
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
}

/// `contended_log` on [`kio::log`]: the frames move out of the channel into the log,
/// which readers read without its lock. The channel stays for parking, so a reader
/// takes the lock only to park at the edge, once per round instead of twice.
#[bench(args = cases(), setup = log_lockfree, items = |case: Case| case.consumers)]
fn contended_log_lockfree(contended: &mut Contended<kio::log::Writer<Frame>>) {
	{
		let mut log = contended.producer.write().ok().expect("open");
		log.push(Arc::new(contended.next));
		let end = log.end();
		log.evict(end.saturating_sub(LOG_FRAMES as u64));
	}
	contended.next += 1;
	contended.crew.run(1);
}

fn main() {
	swole::main();
}
