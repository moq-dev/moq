//! The lite publisher's coverage check before SUBSCRIBE_START.
//!
//! `publisher_covered_before_start` holds a bounded lite-07 subscription to `RANGES` ranges
//! whose source gave up `skipped` of them and has yet to resolve a start, then wakes it with
//! nothing new to serve. Each wake ends in the check of whether every range was given up.
//! The range count stays fixed, so a cost that grows with the ranges already given up shows
//! up as a slope over `skipped`. `adjacent` gives up the groups of one range instead, each
//! with a different error from its neighbor, so the record holds one entry per group.
//!
//! `publisher_update` feeds a lite-07 subscription of `ranges` one-group ranges a stream of
//! SUBSCRIBE_UPDATEs, each retiring the `edited` oldest ranges and adding as many, then serving
//! until parked; `steer` moves only the priority, and `routed_edit` edits on a relay's front,
//! which mirrors each update into its route's copy. A cost that grows with the live ranges
//! rather than the edit shows up as a slope over `ranges`.
//!
//! `publisher_drop` gives up `drops` ranges of a subscription to `ranges` ranges, plus one
//! left open, timing each drop and every wake it causes. Each drop starts the range it gives
//! up, so a drop costing every range shows as a slope over `ranges`, and one costing every
//! drop before it as a slope over `drops`.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench publisher`.

use std::fmt;

use moq_net::fuzz::CoverageBench;
use swole::bench;

/// Ranges the source gave up before the wake.
const SKIPPED: [u64; 4] = [1, 32, 1024, 8192];

/// Ranges subscribed: one more than the most given up, so one stays open.
const RANGES: u64 = 8193;

mod publisher_covered_before_start {
	use moq_net::fuzz::CoverageBench;
	use swole::bench;

	use super::{RANGES, SKIPPED};

	fn spread(skipped: u64) -> CoverageBench {
		CoverageBench::new(RANGES, skipped)
	}

	#[bench(args = SKIPPED, setup = spread, items = |_: u64| 1)]
	fn wake(run: &mut CoverageBench) {
		run.wake()
	}

	/// The same record within one range, each group its own entry.
	#[bench(args = SKIPPED, setup = CoverageBench::adjacent, items = |_: u64| 1)]
	fn adjacent(run: &mut CoverageBench) {
		run.wake()
	}
}

/// Live ranges in the subscription, up to about the most one-group ranges a SUBSCRIBE fits.
const LIVE: [u64; 3] = [16, 512, 8192];

mod publisher_update {
	use moq_net::fuzz::UpdateBench;
	use swole::bench;

	use super::LIVE;

	#[bench(args = LIVE, setup = UpdateBench::new, items = |_: u64| 1)]
	fn edit1(run: &mut UpdateBench) {
		run.edit(1)
	}

	#[bench(args = LIVE, setup = UpdateBench::new, items = |_: u64| 1)]
	fn edit16(run: &mut UpdateBench) {
		run.edit(16)
	}

	#[bench(args = LIVE, setup = UpdateBench::new, items = |_: u64| 1)]
	fn steer(run: &mut UpdateBench) {
		run.steer()
	}

	// The same on a relay's front, which mirrors each update into its route's copy.

	#[bench(args = LIVE, setup = UpdateBench::routed, items = |_: u64| 1)]
	fn routed_edit1(run: &mut UpdateBench) {
		run.edit(1)
	}

	#[bench(args = LIVE, setup = UpdateBench::routed, items = |_: u64| 1)]
	fn routed_edit16(run: &mut UpdateBench) {
		run.edit(16)
	}
}

/// Ranges given up per measurement, and ranges subscribed besides the open one.
const SWEEP: [u64; 4] = [1, 32, 1024, 8192];

/// Ranges subscribed besides the open one and ranges given up, named `ranges=<ranges>/drops=<drops>`.
#[derive(Clone, Copy)]
struct Dropping {
	ranges: u64,
	drops: u64,
}

impl fmt::Display for Dropping {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "ranges={}/drops={}", self.ranges, self.drops)
	}
}

fn drops() -> Vec<Dropping> {
	SWEEP
		.into_iter()
		.flat_map(|ranges| {
			SWEEP
				.into_iter()
				.filter(move |drops| *drops <= ranges)
				.map(move |drops| Dropping { ranges, drops })
		})
		.collect()
}

fn spread(case: Dropping) -> (CoverageBench, u64) {
	(CoverageBench::spread(case.ranges + 1, case.drops), case.drops)
}

/// Returns the fixture, so its teardown stays unmeasured.
#[bench(args = drops(), setup = spread, items = |case: Dropping| case.drops)]
fn publisher_drop((mut run, drops): (CoverageBench, u64)) -> CoverageBench {
	run.give_up(drops);
	run
}

fn main() {
	swole::main();
}
