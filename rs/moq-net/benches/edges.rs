//! A frame write on a session at its live-edge budget, which reclaims on every write.
//!
//! `reclaim` holds `quiet` edges that moved first, each with a frame nobody read, and writes
//! frames to one more edge whose reader takes each as it lands, so the frames to shed are
//! that edge's alone. A reclaim that looks at every edge shows a slope over the quiet edges;
//! the frame size sets how many bytes each write pushes past the budget.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench edges`.

use std::fmt;

use moq_net::fuzz::LiveEdges;
use swole::bench;

/// Edges holding a frame nobody read, ranked to pay before the written one.
const QUIET: [usize; 3] = [1, 32, 1024];

/// Bytes per frame written.
const FRAME: [usize; 2] = [1024, 64 * 1024];

/// One shape, named `<frame>B/<quiet>`.
#[derive(Clone, Copy)]
struct Case {
	quiet: usize,
	frame: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}B/{}", self.frame, self.quiet)
	}
}

fn cases() -> Vec<Case> {
	QUIET
		.into_iter()
		.flat_map(|quiet| FRAME.map(|frame| Case { quiet, frame }))
		.collect()
}

fn edges(case: Case) -> LiveEdges {
	LiveEdges::new(case.quiet, case.frame)
}

#[bench(args = cases(), setup = edges, items = |_: Case| 1)]
fn reclaim(edges: &mut LiveEdges) {
	edges.write()
}

fn main() {
	swole::main();
}
