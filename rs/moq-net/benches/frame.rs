//! Frame receive throughput: what allocating a frame's declared size up front saves
//! over growing its buffer with the bytes received, swept over frame size and the
//! number of streams receiving at once.
//!
//! `upfront` allocates every declared size, `grow` only what has arrived, and
//! `budget` is the session default, which allocates up front until the frames in
//! flight would pass it. Every stream reads a packet-sized chunk per turn.
//!
//! `frame_recv_large` sweeps frames up to the group's byte cap over fewer streams,
//! where a frame spans many storage segments.
//!
//! `frame_read` adds subscribers reading each frame after every chunk that arrives, as
//! a relay forwards a frame still streaming in, swept over publishers, readers, and
//! arrival chunks per frame. Readers keep every chunk until the frame ends, so the
//! chunks read before a `grow` buffer outgrew itself still hold the old one. A read
//! should cost nothing that grows with the readers, so a case's allocations per call
//! should hold steady along the readers axis.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench frame`.

use std::fmt;

use moq_net::fuzz::FrameRecv;

/// Roughly one QUIC packet's payload.
const CHUNK: usize = 1200;

const SIZES: [usize; 3] = [16 * 1024, 256 * 1024, 2 * 1024 * 1024];
const STREAMS: [usize; 3] = [1, 16, 64];

const LARGE_SIZES: [usize; 3] = [4 * 1024 * 1024, 16 * 1024 * 1024, 32 * 1024 * 1024];
const LARGE_STREAMS: [usize; 2] = [1, 4];

/// Allocate every declared size up front.
const UPFRONT: Option<usize> = Some(usize::MAX);
/// Allocate only what has arrived.
const GROW: Option<usize> = Some(0);
/// The session default.
const BUDGET: Option<usize> = None;

/// One buffer, or a chain of segments, for the frames read while they stream in.
const READ_SIZES: [usize; 2] = [16 * 1024, 2 * 1024 * 1024];
const PUBLISHERS: [usize; 2] = [1, 4];
const READERS: [usize; 3] = [1, 16, 128];
/// Chunks each frame arrives in, every one read by every reader.
const ARRIVALS: [usize; 2] = [4, 32];

/// Frame size and streams receiving at once, named `size=<size>/streams=<streams>`.
#[derive(Clone, Copy)]
struct Recv {
	size: usize,
	streams: usize,
}

impl Recv {
	fn recv(self, budget: Option<usize>) -> FrameRecv {
		FrameRecv::new(self.streams, self.size, CHUNK, budget)
	}

	fn bytes(self) -> usize {
		self.size * self.streams
	}
}

impl fmt::Display for Recv {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "size={}/streams={}", self.size, self.streams)
	}
}

fn sweep(sizes: &[usize], streams: &'static [usize]) -> Vec<Recv> {
	sizes
		.iter()
		.flat_map(|&size| streams.iter().map(move |&streams| Recv { size, streams }))
		.collect()
}

/// One receive sweep, a benchmark per allocation budget.
macro_rules! sweep {
	($name:ident, $cases:expr) => {
		mod $name {
			use moq_net::fuzz::FrameRecv;
			use swole::bench;

			use super::{BUDGET, GROW, Recv, UPFRONT, sweep};

			fn upfront_recv(case: Recv) -> FrameRecv {
				case.recv(UPFRONT)
			}

			fn grow_recv(case: Recv) -> FrameRecv {
				case.recv(GROW)
			}

			fn budget_recv(case: Recv) -> FrameRecv {
				case.recv(BUDGET)
			}

			#[bench(args = $cases, setup = upfront_recv, bytes = Recv::bytes)]
			fn upfront(recv: FrameRecv) -> usize {
				recv.run()
			}

			#[bench(args = $cases, setup = grow_recv, bytes = Recv::bytes)]
			fn grow(recv: FrameRecv) -> usize {
				recv.run()
			}

			#[bench(args = $cases, setup = budget_recv, bytes = Recv::bytes)]
			fn budget(recv: FrameRecv) -> usize {
				recv.run()
			}
		}
	};
}

sweep!(frame_recv, sweep(&super::SIZES, &super::STREAMS));
sweep!(frame_recv_large, sweep(&super::LARGE_SIZES, &super::LARGE_STREAMS));

/// One `frame_read` shape, named `size=<size>/publishers=<publishers>/readers=<readers>/arrivals=<arrivals>`.
#[derive(Clone, Copy)]
struct Read {
	size: usize,
	publishers: usize,
	readers: usize,
	arrivals: usize,
}

impl Read {
	fn recv(self, budget: Option<usize>) -> FrameRecv {
		FrameRecv::new(self.publishers, self.size, self.size / self.arrivals, budget).with_readers(self.readers)
	}

	fn items(self) -> usize {
		self.publishers * self.readers
	}
}

impl fmt::Display for Read {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"size={}/publishers={}/readers={}/arrivals={}",
			self.size, self.publishers, self.readers, self.arrivals
		)
	}
}

fn reads() -> Vec<Read> {
	READ_SIZES
		.into_iter()
		.flat_map(|size| {
			PUBLISHERS.into_iter().flat_map(move |publishers| {
				READERS.into_iter().flat_map(move |readers| {
					ARRIVALS.into_iter().map(move |arrivals| Read {
						size,
						publishers,
						readers,
						arrivals,
					})
				})
			})
		})
		.collect()
}

mod frame_read {
	use moq_net::fuzz::FrameRecv;
	use swole::bench;

	use super::{GROW, Read, UPFRONT, reads};

	fn upfront_recv(case: Read) -> FrameRecv {
		case.recv(UPFRONT)
	}

	fn grow_recv(case: Read) -> FrameRecv {
		case.recv(GROW)
	}

	#[bench(args = reads(), setup = upfront_recv, items = Read::items)]
	fn upfront(recv: FrameRecv) -> usize {
		recv.run()
	}

	#[bench(args = reads(), setup = grow_recv, items = Read::items)]
	fn grow(recv: FrameRecv) -> usize {
		recv.run()
	}
}

fn main() {
	swole::main();
}
