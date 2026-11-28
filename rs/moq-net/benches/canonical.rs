//! Cost of canonical cache sizing and final frame-storage cleanup.
//! Each case retains real frame owners; cleanup includes their weak-record removals,
//! without reporting deferred log retirement as completed physical storage cleanup.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench canonical`.

use std::fmt;

/// Frames stored and views of each, named `<frames>x<views>`.
#[derive(Clone, Copy)]
struct Case {
	frames: usize,
	views: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x{}", self.frames, self.views)
	}
}

fn cases() -> Vec<Case> {
	[1, 64, 1024, 8192]
		.into_iter()
		.flat_map(|frames| [1, 8].map(|views| Case { frames, views }))
		.collect()
}

mod canonical_storage {
	use moq_net::fuzz::CanonicalStorage;
	use swole::bench;

	use super::{Case, cases};

	fn stored(case: Case) -> CanonicalStorage {
		CanonicalStorage::new(case.frames, case.views)
	}

	#[bench(args = cases(), setup = stored)]
	fn size(stored: &CanonicalStorage) -> u64 {
		stored.size()
	}

	/// Drops the storage inside the call, since its cleanup is what this measures.
	#[bench(args = cases(), setup = stored)]
	fn cleanup(stored: CanonicalStorage) {
		drop(stored);
	}
}

fn main() {
	swole::main();
}
