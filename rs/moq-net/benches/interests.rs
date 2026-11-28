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
//! The session side is crate-private, so this reaches it through the `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench interests`.

use std::fmt;

use moq_net::{Prefixes, fuzz, origin};

/// Unrelated watches held and sessions listening, named `<watches>w_<sessions>s`.
#[derive(Clone, Copy)]
struct Shape {
	watches: usize,
	sessions: usize,
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}w_{}s", self.watches, self.sessions)
	}
}

fn shapes() -> Vec<Shape> {
	[(10, 10), (1_000, 10), (10, 1_000), (1_000, 1_000)]
		.into_iter()
		.map(|(watches, sessions)| Shape { watches, sessions })
		.collect()
}

/// An origin with `watches` announcement streams on prefixes of their own under
/// `other/`, one on `room/`, and `sessions` client sessions listening. The handles
/// are held for the whole run.
struct Table {
	producer: origin::Producer,
	_driver: origin::Driver,
	_watches: Vec<origin::Routes>,
	sessions: Vec<fuzz::Interests>,
}

fn table(Shape { watches, sessions }: Shape) -> Table {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
	let mut held: Vec<_> = (0..watches)
		.map(|i| scoped(&producer, &format!("other/{i}")).routes())
		.collect();
	held.push(scoped(&producer, "room").routes());
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
	producer.consume().scope(prefix, &Prefixes::all()).unwrap()
}

mod interests {
	use moq_net::origin;
	use swole::bench;

	use super::{Shape, Table, scoped, shapes, table};

	/// A table and a consumer of the prefix each call watches.
	struct Watching {
		table: Table,
		incoming: origin::Consumer,
	}

	fn fresh(shape: Shape) -> Watching {
		let table = table(shape);
		let incoming = scoped(&table.producer, "incoming");
		Watching { table, incoming }
	}

	fn beneath(shape: Shape) -> Watching {
		let table = table(shape);
		let incoming = scoped(&table.producer, "room/incoming");
		Watching { table, incoming }
	}

	#[bench(args = shapes(), setup = fresh)]
	fn uncovered(input: &Watching) {
		drop(input.incoming.routes());
		for session in &input.table.sessions {
			assert_eq!(session.drain(), 2);
		}
	}

	/// No session hears a watch beneath one already held.
	fn unheard(input: &Watching) {
		for session in &input.table.sessions {
			assert_eq!(session.drain(), 0);
		}
	}

	#[bench(args = shapes(), setup = beneath, after = unheard)]
	fn covered(input: &Watching) {
		drop(input.incoming.routes());
	}
}

fn main() {
	swole::main();
}
