//! What it costs an origin to tell its client sessions which prefixes to ask about.
//!
//! Every announcement stream on the origin counts its prefix as wanted, and each client
//! session fed into the origin hears the changes to the broadest wanted prefixes. Both
//! shapes are swept over the unrelated watches held elsewhere and the sessions
//! listening, so a cost that grows with the held watches rather than the touched path
//! shows up as a slope. Hearing one change costs each session its own entry, so the
//! sessions axis is expected to grow; the watches axis is not.
//!
//! - uncovered: a watch on a fresh prefix comes and goes, which every session hears.
//! - covered: a watch beneath one already held comes and goes, which no session hears.
//!
//! The session side is crate-private, so this reaches it through the hidden `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench interests`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::{Hop, Pattern, Patterns, announce, fuzz, origin};

/// `(watches, sessions)` shapes.
const SHAPES: [(usize, usize); 4] = [(10, 10), (1_000, 10), (10, 1_000), (1_000, 1_000)];

/// An origin with `watches` announcement streams on prefixes of their own under
/// `other/`, one on `room/`, and `sessions` client sessions listening. The handles
/// are held for the whole run.
struct Table {
	producer: origin::Producer,
	_driver: origin::Driver,
	_watches: Vec<announce::Consumer>,
	sessions: Vec<fuzz::Interests>,
}

fn table(watches: usize, sessions: usize) -> Table {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	let mut held: Vec<_> = (0..watches)
		.map(|i| scoped(&producer, &format!("other/{i}")).announced())
		.collect();
	held.push(scoped(&producer, "room").announced());
	let sessions: Vec<_> = (0..sessions).map(|_| fuzz::interests(&producer)).collect();
	for session in &sessions {
		session.drain();
	}
	Table {
		producer,
		_driver: driver,
		_watches: held,
		sessions,
	}
}

fn scoped(producer: &origin::Producer, prefix: &str) -> origin::Consumer {
	producer
		.consume()
		.scope(prefix, &Patterns::from(Pattern::all()))
		.unwrap()
}

fn bench_uncovered(c: &mut Criterion) {
	let mut group = c.benchmark_group("interests/uncovered");
	for (watches, sessions) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{watches}w_{sessions}s"));
		group.bench_function(id, |b| {
			let table = table(watches, sessions);
			let incoming = scoped(&table.producer, "incoming");
			b.iter(|| {
				drop(incoming.announced());
				for session in &table.sessions {
					assert_eq!(session.drain(), 2);
				}
			});
		});
	}
	group.finish();
}

fn bench_covered(c: &mut Criterion) {
	let mut group = c.benchmark_group("interests/covered");
	for (watches, sessions) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{watches}w_{sessions}s"));
		group.bench_function(id, |b| {
			let table = table(watches, sessions);
			let incoming = scoped(&table.producer, "room/incoming");
			b.iter(|| {
				drop(incoming.announced());
			});
			for session in &table.sessions {
				assert_eq!(session.drain(), 0);
			}
		});
	}
	group.finish();
}

criterion_group!(benches, bench_uncovered, bench_covered);
criterion_main!(benches);
