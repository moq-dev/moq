//! The cost of ready stream headers against independently stalled headers.

#[cfg(target_os = "linux")]
use std::{fmt, time::Duration};

#[cfg(target_os = "linux")]
use swole::{Iterations, bench};

#[cfg(target_os = "linux")]
#[path = "../tests/integration/web_headers.rs"]
mod headers;
#[cfg(target_os = "linux")]
#[path = "../tests/integration/support.rs"]
mod support;

/// Streams holding partial headers against ready uni and bi streams of each kind.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Case {
	pending: usize,
	ready: usize,
}

#[cfg(target_os = "linux")]
impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "pending-{}/ready-{}", self.pending, self.ready)
	}
}

#[cfg(target_os = "linux")]
fn cases() -> Vec<Case> {
	[0, 64, 256]
		.into_iter()
		.flat_map(|pending| [1, 16].map(|ready| Case { pending, ready }))
		.collect()
}

/// Times only the served rounds: the fixture is built for how many it serves.
#[cfg(target_os = "linux")]
#[bench(args = cases(), items = |case: Case| 2 * case.ready)]
fn web_headers(case: Case, iterations: Iterations) -> Duration {
	headers::run(case.pending, case.ready, iterations.get())
}

fn main() {
	swole::main();
}
