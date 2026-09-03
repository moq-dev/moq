//! The worker's UDP send and pump path must not allocate in steady state.
//!
//! Kernel-gated like the other io_uring tests: skips loudly where io_uring is
//! unavailable, and runs everywhere else.

#![cfg(target_os = "linux")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use moq_uring::{Config, Error, Worker, udp};

/// Counts heap operations while [`ENABLED`], so a test can bracket one call.
struct CountingAlloc;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		if ENABLED.load(Ordering::Relaxed) {
			ALLOCS.fetch_add(1, Ordering::Relaxed);
		}
		// SAFETY: forwarded unchanged to the system allocator.
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		if ENABLED.load(Ordering::Relaxed) {
			DEALLOCS.fetch_add(1, Ordering::Relaxed);
		}
		// SAFETY: `ptr` and `layout` came from the system allocator.
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// Start counting heap operations.
fn count_from_zero() {
	ALLOCS.store(0, Ordering::Relaxed);
	DEALLOCS.store(0, Ordering::Relaxed);
	ENABLED.store(true, Ordering::Relaxed);
}

/// Stop counting, returning the (allocations, deallocations) since the start.
fn counted() -> (usize, usize) {
	ENABLED.store(false, Ordering::Relaxed);
	(ALLOCS.load(Ordering::Relaxed), DEALLOCS.load(Ordering::Relaxed))
}

const SENDS: usize = 1_000;

#[test]
fn steady_state_sends_do_not_allocate() {
	let mut worker = match Worker::new(Config::default()) {
		Ok(worker) => worker,
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring allocation test: {reason}");
			return;
		}
		Err(err) => panic!("worker setup failed: {err}"),
	};
	let handle = worker.handle();
	// One transmit buffer without GSO, so every send stages four datagrams and
	// the next acquire waits on their completions.
	let mut config = udp::Config::default();
	config.gso = false;
	config.tx_buffers_max = 1;
	let socket = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
		.expect("socket");
	let sink = UdpSocket::bind("127.0.0.1:0").expect("bind sink");
	let to = sink.local_addr().expect("sink address");

	let (acquires, stage_allocs, stage_deallocs) = worker
		.block_on(async {
			// Grow every reusable pool before observing the steady state.
			for _ in 0..100 {
				let mut tx = socket.acquire().await.expect("warmup acquire");
				tx[..4800].fill(0x5a);
				tx.send(4800, to, 1200).expect("warmup send");
			}

			let mut acquires = 0;
			let mut stage_allocs = 0;
			let mut stage_deallocs = 0;
			for _ in 0..SENDS {
				// A pending acquire allocates one kio waiter identity; pumping
				// the previous send's completions must add nothing on top.
				count_from_zero();
				let mut tx = socket.acquire().await.expect("acquire");
				acquires += counted().0;

				count_from_zero();
				tx[..4800].fill(0x5a);
				let sent = tx.send(4800, to, 1200);
				let (allocs, deallocs) = counted();
				sent.expect("send");
				stage_allocs += allocs;
				stage_deallocs += deallocs;
			}
			(acquires, stage_allocs, stage_deallocs)
		})
		.expect("worker");

	assert_eq!(stage_allocs, 0, "staging {SENDS} four-datagram sends allocated");
	assert_eq!(stage_deallocs, 0, "staging {SENDS} four-datagram sends deallocated");
	assert!(
		acquires <= SENDS,
		"acquire plus pump made {acquires} allocations over {SENDS} sends, more than one each"
	);
}
