//! `snapshot::Encoder::update` on a keyed table shaped like a moq-stats traffic frame, swept over
//! total rows and the share of rows that change per tick. Cases that change the same number of rows
//! (100 rows at 100%, 10k at 1%) separate a cost that grows with the table from one that grows with
//! the changed rows.
//!
//! Deltas, explicitly forced snapshots with deltas enabled, and delta-free snapshots are separate
//! cases. Before each delta, restate the baseline outside the measurement to reset the group caps
//! while retaining the warm memo. Every call measures one tick and one update of its kind.
//!
//! Run with `cargo bench -p moq-json --bench table`.
use std::fmt;

use bytes::Bytes;
use moq_json::snapshot::{Config, Encoder};
use moq_net::stats::Traffic;
use serde::Serialize;
use swole::bench;

/// One tick of the table: rows sorted by path, serialized as a JSON object like moq-stats' `Frame`.
struct Table {
	rows: Vec<(String, Traffic)>,
	/// Every `stride`-th row changes each tick.
	stride: usize,
}

impl Serialize for Table {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_map(self.rows.iter().map(|(path, value)| (path.as_str(), value)))
	}
}

impl Table {
	fn new(rows: usize, changed_percent: usize) -> Self {
		let mut rows: Vec<(String, Traffic)> = (0..rows as u64)
			.map(|i| {
				let mut traffic = Traffic::default();
				traffic.announces_started = 1;
				traffic.announced_bytes = 24;
				traffic.broadcasts_started = 3 + i % 7;
				traffic.broadcasts_ended = i % 3;
				traffic.subscriptions_started = 9 + i % 21;
				traffic.subscriptions_ended = 3 * (i % 3);
				traffic.bytes = 62_500 * (1_000 + i * 37);
				traffic.frames = 80 * (1_000 + i * 37);
				traffic.groups = 2 * (1_000 + i * 37);
				(
					format!("anon/live-stream-{:05}-{i}", i.wrapping_mul(7919) % 100_000),
					traffic,
				)
			})
			.collect();
		rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		Self {
			rows,
			stride: 100 / changed_percent,
		}
	}

	/// Advance one tick: the changed rows move their payload counters, the rest stay put.
	fn tick(&mut self) {
		for (_, traffic) in self.rows.iter_mut().step_by(self.stride) {
			traffic.bytes += 62_500;
			traffic.frames += 80;
			traffic.groups += 2;
		}
	}
}

/// A stats track's encoder mode, and whether to force a snapshot before each timed update.
#[derive(Clone, Copy)]
enum Mode {
	Deltas,
	ForcedSnapshots,
	Snapshots,
}

impl Mode {
	fn config(self) -> Config {
		match self {
			Self::Deltas | Self::ForcedSnapshots => Config::default(),
			Self::Snapshots => Config::default().with_deltas(false),
		}
	}
}

/// One table in one mode, named `<mode>/<changed>%/<rows>`.
#[derive(Clone, Copy)]
struct Case {
	mode: Mode,
	rows: usize,
	changed: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let mode = match self.mode {
			Mode::Deltas => "deltas",
			Mode::ForcedSnapshots => "forced-snapshots",
			Mode::Snapshots => "snapshots",
		};
		write!(f, "{mode}/{}%/{}", self.changed, self.rows)
	}
}

fn cases() -> Vec<Case> {
	let mut cases = Vec::new();
	for mode in [Mode::Deltas, Mode::ForcedSnapshots, Mode::Snapshots] {
		for rows in [100, 1_000, 10_000, 20_000] {
			for changed in [1, 10, 100] {
				cases.push(Case { mode, rows, changed });
			}
		}
	}
	cases
}

/// A table and an encoder seeded with it.
struct State {
	mode: Mode,
	table: Table,
	encoder: Encoder<Table>,
}

fn state(case: Case) -> State {
	let table = Table::new(case.rows, case.changed);
	let mut encoder = Encoder::<Table>::new(case.mode.config());
	encoder.update(&table).unwrap().expect("an initial snapshot").commit();
	if let Mode::Deltas = case.mode {
		// Populate the memo so every timed delta has the steady-state fast path.
		assert!(encoder.update(&table).unwrap().is_none());
	}
	State {
		mode: case.mode,
		table,
		encoder,
	}
}

fn prepare(state: &mut State) {
	match state.mode {
		Mode::ForcedSnapshots => state.encoder.reset(),
		// Restating resets both caps without discarding the parsed baseline or memo.
		Mode::Deltas => state.encoder.snapshot().unwrap().expect("a seeded baseline").commit(),
		Mode::Snapshots => {}
	}
}

#[bench(args = cases(), setup = state, before = prepare, items = |case: Case| case.rows)]
fn update(state: &mut State) -> Bytes {
	state.table.tick();
	let frame = state.encoder.update(&state.table).unwrap().expect("a changed tick");
	assert_eq!(frame.keyframe, !matches!(state.mode, Mode::Deltas));
	let payload = frame.payload.clone();
	frame.commit();
	payload
}

fn main() {
	swole::main();
}
