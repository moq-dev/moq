//! Delivery-path benchmarks for the group and track models.
//!
//! Small frames expose control and frame-slot allocation costs. The write sweep
//! includes the initial deque capacity and its first growth boundary, as well as
//! groups with thousands of tiny frames.
//!
//! `track_recv_groups` covers the layer above: how much it costs to hand out one
//! cached group, swept over cache depth so a per-delivery scan shows up as a slope.
//! It also covers the steady-state path after a subscriber has reached the edge.
//!
//! `track_stamp_groups` covers stamping a group's first frame, which checks it
//! against its neighbors' starts, swept over cache depth so a scan shows as a slope:
//! `append` among stamped groups, `sparse` among groups created but not yet stamped.
//!
//! `group_contended` reads one live multi-frame group from readers spread over worker
//! threads, swept over threads, readers, and frames appended per round, so contention
//! on the group's shared state shows up as a slope across cores.
//!
//! `group_shed` writes one live group far past what it holds, swept over the frames it
//! holds (a small pool's debt, or the 32 MiB live tail) and the frames it sheds, so
//! shedding the front stays a flat cost per frame.
//!
//! Run with `cargo bench -p moq-net`.

#![expect(
	clippy::disallowed_methods,
	reason = "`iter_custom` timings measure the host clock on purpose"
)]

use std::{
	hint::black_box,
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
	task::Poll,
	thread,
	time::{Duration, Instant},
};

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Timestamp, broadcast, frame, group, track};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// A small, fixed payload shared across every frame. Cloning a `Bytes` is a
/// refcount bump (no allocation), so the benchmark isolates the per-frame control
/// overhead rather than payload allocation.
const PAYLOAD: usize = 64;

/// Frame counts to sweep. The top end is a long group of tiny frames, the most the
/// group cap used to allow. Counts 32 and 33 straddle a full write batch and its next
/// flush.
const COUNTS: [usize; 7] = [4, 5, 32, 33, 512, 2_048, 8_192];

/// Cached group counts to sweep for the track-level delivery benchmarks. A track
/// publishing one group per frame at the default 5s retention sits in the hundreds,
/// so the top end is deliberately past anything realistic: a per-delivery scan over
/// the cache shows up as a slope here, while a seek stays flat.
const DEPTHS: [usize; 3] = [64, 512, 4_096];

/// Groups appended and received in one caught-up benchmark iteration.
const LIVE_BATCH: usize = 128;

/// Keeps the broadcast/track producers alive alongside the group so the group
/// isn't torn down mid-benchmark. Only `group` is written to.
struct Ctx {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	group: group::Producer,
}

/// Build a fresh, empty group via the public producer path.
fn fresh_group() -> Ctx {
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	let track = broadcast.create_track("bench", timed()).unwrap();
	let group = track.append_group().unwrap();
	Ctx {
		_broadcast: broadcast,
		_track: track,
		group,
	}
}

/// Write N small frames into a fresh group (producer-side per-frame cost).
///
/// `single` is one `write_frame` per frame: a group lock plus a track-level eviction
/// settle each. `batch32` fills a `frame::Buffer` and hands the whole thing to
/// `write_frames`, paying both once per batch.
fn bench_write(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_write_frames");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::new("batch32", n), &n, |b, &n| {
			b.iter_batched(
				|| (fresh_group(), frame::Buffer::<32>::new()),
				|(mut ctx, mut buf)| {
					for _ in 0..n {
						let frame = frame::Frame::new(Timestamp::ZERO, payload.clone());
						// A full buffer hands the frame back: flush, then take it.
						if let Err(frame) = buf.push(frame) {
							ctx.group.write_frames(&mut buf).unwrap();
							buf.push(frame).unwrap();
						}
					}
					ctx.group.write_frames(&mut buf).unwrap();
					(ctx, buf)
				},
				BatchSize::SmallInput,
			);
		});
		g.bench_with_input(BenchmarkId::new("single", n), &n, |b, &n| {
			b.iter_batched(
				fresh_group,
				|mut ctx| {
					for _ in 0..n {
						ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
					}
					// Return so the drop happens outside the timed region.
					ctx
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

/// A pre-filled, finished group plus a consumer positioned at its first frame.
fn filled_group(n: usize, payload: &Bytes) -> (Ctx, group::Consumer) {
	let mut ctx = fresh_group();
	for _ in 0..n {
		ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	ctx.group.finish().unwrap();
	let consumer = ctx.group.consume();
	(ctx, consumer)
}

/// Drain a whole group through a reused `frame::Buffer<N>`.
fn drain_batched<const N: usize>(consumer: &mut group::Consumer, buf: &mut frame::Buffer<N>) {
	loop {
		let batch = consumer.read_frames(buf).now_or_never().unwrap().unwrap();
		if batch.is_empty() {
			break;
		}
		for frame in batch.iter() {
			black_box(frame);
		}
	}
}

/// Drain N small frames from a pre-filled group (consumer-side per-frame cost).
///
/// `single` is one frame at a time via [`group::Consumer::read_frame`]: a lock and a
/// waker each. `batch{N}` is [`group::Consumer::read_frames`] cloning the ready tail
/// into a reused `N`-frame buffer, amortizing both across the batch.
///
/// Throughput keeps climbing to 32 and stalls after (128 falls out of L1), but the
/// default capacity is 8: the tail of that curve only pays off for a reader draining a
/// backlog, while every buffer pays its stack whether or not the batch fills.
fn bench_read(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_read_frames");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::new("single", n), &n, |b, &n| {
			b.iter_batched(
				|| filled_group(n, &payload),
				|(ctx, mut consumer)| {
					for _ in 0..n {
						// All frames are already present and finished, so a single poll
						// resolves immediately (no runtime needed).
						let frame = consumer.read_frame().now_or_never().unwrap().unwrap();
						black_box(frame);
					}
					(ctx, consumer)
				},
				BatchSize::SmallInput,
			);
		});

		// The buffer is allocated in setup and refilled for the whole group, like a
		// real reader that keeps one per task.
		macro_rules! batched {
			($cap:expr) => {
				g.bench_with_input(BenchmarkId::new(concat!("batch", $cap), n), &n, |b, &n| {
					b.iter_batched(
						|| {
							let (ctx, consumer) = filled_group(n, &payload);
							(ctx, consumer, frame::Buffer::<$cap>::new())
						},
						|(ctx, mut consumer, mut buf)| {
							drain_batched(&mut consumer, &mut buf);
							(ctx, consumer, buf)
						},
						BatchSize::SmallInput,
					);
				});
			};
		}

		batched!(4);
		batched!(8);
		batched!(32);
		batched!(128);
	}
	g.finish();
}

/// Keeps the broadcast alive alongside the track being drained.
struct TrackCtx {
	_broadcast: broadcast::Producer,
	track: track::Producer,
}

/// Build a track holding N cached groups, each with a single small frame.
fn filled_track(n: usize, payload: &Bytes) -> TrackCtx {
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	let track = broadcast.create_track("bench", timed()).unwrap();
	for _ in 0..n {
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		group.finish().unwrap();
	}
	TrackCtx {
		_broadcast: broadcast,
		track,
	}
}

/// Request the full cache window instead of the default live edge.
fn replay() -> track::Subscription {
	track::Subscription::default()
		.with_max_delay(Duration::MAX)
		.with_range(0..)
}

/// Drain N cached groups from a track, in arrival order and in sequence order.
///
/// The two differ in how they find the next group: `recv_group` walks an arrival
/// index, while `next_group` seeks by sequence. Both should be flat in the cache
/// depth; a per-delivery scan over the cache makes the sequence-ordered arm
/// quadratic in N.
fn bench_track_recv(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("track_recv_groups");
	for &n in &DEPTHS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::new("arrival", n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let ctx = filled_track(n, &payload);
					let subscriber = ctx.track.subscribe(replay());
					(ctx, subscriber)
				},
				|(ctx, mut subscriber)| {
					for _ in 0..n {
						let group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
						black_box(group);
					}
					(ctx, subscriber)
				},
				BatchSize::SmallInput,
			);
		});
		g.bench_with_input(BenchmarkId::new("sequence", n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let ctx = filled_track(n, &payload);
					let subscriber = ctx.track.subscribe(replay()).ordered();
					(ctx, subscriber)
				},
				|(ctx, mut subscriber)| {
					for _ in 0..n {
						let group = subscriber.next_group().now_or_never().unwrap().unwrap().unwrap();
						black_box(group);
					}
					(ctx, subscriber)
				},
				BatchSize::SmallInput,
			);
		});
		g.throughput(Throughput::Elements(LIVE_BATCH as u64));
		g.bench_with_input(BenchmarkId::new("caught_up", n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let ctx = filled_track(n, &payload);
					let mut subscriber = ctx.track.subscribe(None);
					let edge = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
					assert_eq!(edge.sequence, n as u64 - 1);
					(ctx, subscriber)
				},
				|(ctx, mut subscriber)| {
					for _ in 0..LIVE_BATCH {
						let mut group = ctx.track.append_group().unwrap();
						group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
						group.finish().unwrap();
						let group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
						black_box(group);
					}
					(ctx, subscriber)
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

/// The full lifecycle: build a group, write N frames, then drain them.
fn bench_roundtrip(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_roundtrip");
	for &n in &COUNTS {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			b.iter_batched(
				fresh_group,
				|mut ctx| {
					for _ in 0..n {
						ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
					}
					ctx.group.finish().unwrap();
					let mut consumer = ctx.group.consume();
					for _ in 0..n {
						let frame = consumer.read_frame().now_or_never().unwrap().unwrap();
						black_box(frame);
					}
					(ctx, consumer)
				},
				BatchSize::SmallInput,
			);
		});
	}
	g.finish();
}

/// Sweep payload depth and subscribers for both plain and property-bearing frames.
fn bench_relay_frames(c: &mut Criterion) {
	#[cfg(feature = "fuzz")]
	{
		let mut g = c.benchmark_group("group_relay_frames");
		for frames in [4, 512, 8192] {
			for subscribers in [1, 8, 64] {
				for properties in [false, true] {
					let context = moq_net::fuzz::object_group(frames, properties);
					let mode = if properties { "properties" } else { "plain" };
					g.throughput(Throughput::Elements((frames * subscribers) as u64));
					g.bench_function(BenchmarkId::new(mode, format!("{frames}/{subscribers}")), |b| {
						b.iter_batched(
							|| (0..subscribers).map(|_| context.2.consume()).collect::<Vec<_>>(),
							|mut readers| {
								// A capacity `group_read_frames` doesn't sweep: sharing its readers'
								// instantiation moves them out of line and shifts that sweep.
								let mut buffer = frame::Buffer::<16>::new();
								for reader in &mut readers {
									drain_batched(reader, &mut buffer);
								}
								readers
							},
							BatchSize::SmallInput,
						);
					});
				}
			}
		}
		g.finish();
	}
	#[cfg(not(feature = "fuzz"))]
	let _ = c;
}

/// Append and stamp groups on a track already holding N cached groups.
///
/// A group's first frame is checked against the nearest stamped starts on either side,
/// once per group. Each fresh track takes at most N more groups, so the cache stays
/// within 2N for the whole measurement.
fn bench_stamp(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("track_stamp_groups");
	for &n in &DEPTHS {
		for waiting in [false, true] {
			let name = if waiting { "append_waiting" } else { "append" };
			g.bench_with_input(BenchmarkId::new(name, n), &n, |b, &n| {
				b.iter_custom(|iters| {
					let mut total = Duration::ZERO;
					let mut left = iters;
					while left > 0 {
						let ctx = filled_track(n, &payload);
						// A pending fetch keeps the landing index occupied beyond this batch,
						// separating the no-reader fast path from an unrelated parked read.
						let dynamic = waiting.then(|| ctx.track.requests());
						let mut fetching = dynamic
							.as_ref()
							.map(|_| ctx.track.consume().fetch_group((2 * n + 1) as u64, None));
						let waiter = kio::Waiter::noop();
						if let Some(fetching) = &mut fetching {
							assert!(kio::Task::poll(&mut **fetching, &waiter).is_pending());
						}
						let batch = left.min(n as u64);
						let start = Instant::now();
						for _ in 0..batch {
							let mut group = ctx.track.append_group().unwrap();
							group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
							group.finish().unwrap();
						}
						total += start.elapsed();
						left -= batch;
					}
					total
				});
			});
		}
		// N groups created before any is stamped, then stamped in order: each one's upper
		// neighbor is a run of unstamped groups, which a slot scan walks.
		g.bench_with_input(BenchmarkId::new("sparse", n), &n, |b, &n| {
			b.iter_custom(|iters| {
				let mut total = Duration::ZERO;
				let mut left = iters;
				while left > 0 {
					let ctx = filled_track(0, &payload);
					let mut groups: Vec<_> = (0..n).map(|_| ctx.track.append_group().unwrap()).collect();
					let batch = left.min(n as u64);
					let start = Instant::now();
					for group in groups.iter_mut().take(batch as usize) {
						group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
					}
					total += start.elapsed();
					left -= batch;
				}
				total
			});
		});
	}
	g.finish();
}

/// Read every frame of fresh compressed groups with N plain readers each.
///
/// The group inflates once into a copy every plain reader shares, so the cost should
/// slope with the groups and stay nearly flat in the readers; a per-reader inflate
/// shows up as a slope over the readers.
fn bench_inflate(c: &mut Criterion) {
	const FRAMES: usize = 32;
	let payloads: Vec<Bytes> = (0..FRAMES)
		.map(|index| Bytes::from(format!("{{\"frame\":{index},\"status\":\"{}\"}}", "ok".repeat(100))))
		.collect();
	let mut g = c.benchmark_group("group_inflate");
	for groups in [1usize, 16] {
		for readers in [1usize, 8, 64] {
			g.throughput(Throughput::Elements((groups * readers * FRAMES) as u64));
			let id = BenchmarkId::new(format!("groups{groups}"), readers);
			g.bench_with_input(id, &readers, |b, &readers| {
				b.iter_batched(
					|| {
						let broadcast = broadcast::Producer::new(broadcast::Info::default());
						let track = broadcast.create_track("bench", timed().with_compress(true)).unwrap();
						let consumers: Vec<group::Consumer> = (0..groups)
							.flat_map(|_| {
								let mut group = track.append_group().unwrap();
								for payload in &payloads {
									group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
								}
								group.finish().unwrap();
								(0..readers).map(move |_| group.consume()).collect::<Vec<_>>()
							})
							.collect();
						(broadcast, track, consumers)
					},
					|(broadcast, track, mut consumers)| {
						for consumer in &mut consumers {
							while let Some(frame) = consumer.read_frame().now_or_never().unwrap().unwrap() {
								black_box(frame);
							}
						}
						(broadcast, track, consumers)
					},
					BatchSize::SmallInput,
				);
			});
		}
	}
	g.finish();
}

/// Worker threads the contended readers are spread over.
const THREADS: [usize; 3] = [1, 4, 16];

/// Readers of the one contended group.
const READERS: [usize; 3] = [16, 256, 1_024];

/// Frames appended per contended round: one at the edge, or a burst that readers drain
/// several at a time.
const BURSTS: [usize; 2] = [1, 8];

/// Frames a contended group takes before the driver rolls to the next one.
const ROLL: usize = 4_096;

/// One pass of a worker over its readers.
type Work = Box<dyn FnMut() + Send>;

/// Worker threads that each run one pass when the driver releases a round.
struct Crew {
	control: Arc<Control>,
	threads: Vec<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Control {
	/// Rounds released so far, or [`STOP`].
	round: AtomicU64,
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
						work();
						control.done.fetch_add(1, Ordering::AcqRel);
					}
				})
			})
			.collect();
		Self { control, threads }
	}

	/// Run one pass on every worker and wait for all of them.
	fn run(&self) {
		self.control.done.store(0, Ordering::Relaxed);
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

/// Busy-wait for `f`, yielding once spinning has gone on long enough that the machine
/// may be oversubscribed.
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

/// Read one live group from readers on several threads.
///
/// Each round the driver appends a burst of frames; every reader drains them with
/// [`group::Consumer::poll_read_frame`] and parks at the edge again. Every [`ROLL`]
/// frames the driver publishes the next group and finishes the current one, and each
/// reader moves on once it reads the end, so the group stays under its frame cap.
fn bench_contended(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	let mut g = c.benchmark_group("group_contended");
	g.warm_up_time(Duration::from_secs(1));
	g.measurement_time(Duration::from_secs(3));
	for burst in BURSTS {
		for threads in THREADS {
			for readers in READERS {
				g.throughput(Throughput::Elements((readers * burst) as u64));
				let id = BenchmarkId::new(format!("burst_{burst}/threads_{threads}"), readers);
				g.bench_function(id, |b| {
					let broadcast = broadcast::Producer::new(broadcast::Info::default());
					let track = broadcast.create_track("bench", track::Info::default()).unwrap();
					let mut group = track.append_group().unwrap();
					// The group readers move to once theirs ends.
					let next = Arc::new(Mutex::new(group.consume()));
					let mut sets: Vec<Vec<group::Consumer>> = (0..threads).map(|_| Vec::new()).collect();
					for i in 0..readers {
						sets[i % threads].push(group.consume());
					}
					let work = sets
						.into_iter()
						.map(|mut set| -> Work {
							let next = next.clone();
							let waiter = kio::Waiter::noop();
							Box::new(move || {
								for reader in &mut set {
									let mut read = 0;
									loop {
										match reader.poll_read_frame(&waiter) {
											Poll::Ready(Ok(Some(frame))) => {
												black_box(frame);
												read += 1;
											}
											Poll::Ready(Ok(None)) => *reader = next.lock().unwrap().clone(),
											Poll::Ready(Err(err)) => panic!("read failed: {err}"),
											Poll::Pending => break,
										}
									}
									assert_eq!(read, burst, "every appended frame is ready");
								}
							})
						})
						.collect();
					let crew = Crew::new(work);

					let mut written = 0;
					b.iter_custom(|iters| {
						let start = Instant::now();
						for _ in 0..iters {
							if written + burst > ROLL {
								let rolled = track.append_group().unwrap();
								*next.lock().unwrap() = rolled.consume();
								let mut done = std::mem::replace(&mut group, rolled);
								done.finish().unwrap();
								written = 0;
							}
							for _ in 0..burst {
								group.write_frame(None, payload.clone()).unwrap();
							}
							written += burst;
							crew.run();
						}
						start.elapsed()
					});
				});
			}
		}
	}
	g.finish();
}

/// A frame for the shedding benchmark: large enough that a group outgrows a small pool
/// in a few hundred frames.
const SHED_PAYLOAD: usize = 16 * 1024;

/// A live group cached in a pool of `capacity` bytes, or an unbounded one.
fn pooled_group(capacity: Option<u64>) -> Ctx {
	let mut info = broadcast::Info::default();
	if let Some(capacity) = capacity {
		info.pool = moq_net::cache::Pool::new(moq_net::cache::Config::default().with_capacity(capacity));
	}
	let broadcast = broadcast::Producer::new(info);
	let track = broadcast.create_track("bench", timed()).unwrap();
	let group = track.append_group().unwrap();
	Ctx {
		_broadcast: broadcast,
		_track: track,
		group,
	}
}

/// Write a live group far past what it holds, which sheds its front to make room: under
/// a small pool's debt, or past the 32 MiB live tail with no pool capacity.
///
/// The time per frame should stay flat across both axes: a shed costs the frames it
/// drops, never the frames the group still holds.
fn bench_shed(c: &mut Criterion) {
	let payload = Bytes::from(vec![0u8; SHED_PAYLOAD]);
	let mut g = c.benchmark_group("group_shed");
	for (held, pool) in [
		("pool-1MiB", Some(1 << 20)),
		("pool-16MiB", Some(16 << 20)),
		("tail-32MiB", None),
	] {
		// Twice what the tail holds, and sixteen times.
		for frames in [4_096, 32_768] {
			g.throughput(Throughput::Elements(frames as u64));
			g.bench_function(BenchmarkId::new(held, frames), |b| {
				b.iter_batched(
					|| pooled_group(pool),
					|mut ctx| {
						for _ in 0..frames {
							ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
						}
						ctx
					},
					BatchSize::PerIteration,
				);
			});
		}
	}
	g.finish();
}

criterion_group!(
	benches,
	bench_write,
	bench_read,
	bench_roundtrip,
	bench_track_recv,
	bench_stamp,
	bench_relay_frames,
	bench_inflate,
	bench_contended,
	bench_shed
);
criterion_main!(benches);
