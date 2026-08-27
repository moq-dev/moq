//! End-to-end benchmarks for kio's shared-state channel.
//!
//! The waiter benchmarks isolate registration. These benchmarks cover the full
//! steady-state path: consumers park on a value, a producer mutates it, dropping
//! the write guard wakes every consumer, and each consumer observes the value and
//! parks again.
//!
//! Run with `cargo bench -p kio --bench channel`.

use std::hint::black_box;
use std::task::{Poll, Waker};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kio::{Consumer, Producer, Waiter};

/// Fanout sizes that cover a lone reader, a small room, and a busy broadcast.
const FANOUT: [usize; 3] = [1, 8, 128];

/// One consumer and the cursor it has most recently observed.
struct Reader {
	consumer: Consumer<u64>,
	waiter: Waiter,
	seen: u64,
}

/// A channel with every consumer parked on its current value.
struct Channel {
	producer: Producer<u64>,
	readers: Vec<Reader>,
	next: u64,
}

impl Channel {
	fn new(readers: usize) -> Self {
		let producer = Producer::new(0);
		let mut readers: Vec<_> = (0..readers)
			.map(|_| Reader {
				consumer: producer.consume(),
				waiter: Waiter::new(Waker::noop().clone()),
				seen: 0,
			})
			.collect();

		for reader in &mut readers {
			let poll = reader.consumer.poll(&reader.waiter, |_| Poll::<u64>::Pending);
			assert!(poll.is_pending());
		}

		Self {
			producer,
			readers,
			next: 1,
		}
	}

	/// Publish one value, deliver it to every reader, then park them for the next.
	fn cycle(&mut self) {
		let next = self.next;
		self.next += 1;

		let Ok(mut state) = self.producer.write() else {
			panic!("benchmark channel is open");
		};
		*state = next;
		drop(state);

		for reader in &mut self.readers {
			let value = match reader.consumer.poll(&reader.waiter, |state| {
				if **state != reader.seen {
					Poll::Ready(**state)
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(value)) => value,
				_ => unreachable!("a published value must be ready"),
			};
			reader.seen = value;
			black_box(value);

			let poll = reader.consumer.poll(&reader.waiter, |state| {
				if **state != reader.seen {
					Poll::Ready(**state)
				} else {
					Poll::Pending
				}
			});
			assert!(poll.is_pending());
		}
	}
}

fn bench_notify(c: &mut Criterion) {
	let mut group = c.benchmark_group("channel_notify_cycle");
	for readers in FANOUT {
		group.throughput(Throughput::Elements(readers as u64));
		group.bench_with_input(BenchmarkId::from_parameter(readers), &readers, |b, &readers| {
			let mut channel = Channel::new(readers);
			b.iter(|| channel.cycle());
		});
	}
	group.finish();
}

criterion_group!(benches, bench_notify);
criterion_main!(benches);
