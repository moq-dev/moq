//! Counts steady-state heap operations in the worker's UDP send and pump path.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

#[cfg(target_os = "linux")]
fn main() {
	use std::net::UdpSocket;

	use moq_uring::{Config, Error, Worker, udp};

	if std::env::args().any(|arg| arg == "--list") {
		println!("allocation_count: benchmark");
		return;
	}

	let mut worker = match Worker::new(Config::default()) {
		Ok(worker) => worker,
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring allocation benchmark: {reason}");
			return;
		}
		Err(err) => panic!("worker setup failed: {err}"),
	};
	let handle = worker.handle();
	let mut config = udp::Config::default();
	config.gso = false;
	config.tx_buffers_max = 1;
	let socket = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
		.expect("socket");
	let sink = UdpSocket::bind("127.0.0.1:0").expect("bind sink");
	let to = sink.local_addr().expect("sink address");

	let (stage_allocs, stage_deallocs, pump_allocs, pump_deallocs) = worker
		.block_on(async {
			async fn send(socket: &udp::Socket, to: std::net::SocketAddr) {
				let mut tx = socket.acquire().await.expect("acquire");
				tx[..4800].fill(0x5a);
				tx.send(4800, to, 1200).expect("send");
			}

			// Grow every reusable pool before observing the steady state.
			for _ in 0..100 {
				send(&socket, to).await;
			}

			let mut stage_allocs = 0;
			let mut stage_deallocs = 0;
			let mut pump_allocs = 0;
			let mut pump_deallocs = 0;
			for _ in 0..10_000 {
				ALLOCS.store(0, Ordering::Relaxed);
				DEALLOCS.store(0, Ordering::Relaxed);
				ENABLED.store(true, Ordering::Relaxed);
				let mut tx = socket.acquire().await.expect("acquire");
				ENABLED.store(false, Ordering::Relaxed);
				pump_allocs += ALLOCS.load(Ordering::Relaxed);
				pump_deallocs += DEALLOCS.load(Ordering::Relaxed);

				ALLOCS.store(0, Ordering::Relaxed);
				DEALLOCS.store(0, Ordering::Relaxed);
				ENABLED.store(true, Ordering::Relaxed);
				tx[..4800].fill(0x5a);
				tx.send(4800, to, 1200).expect("send");
				ENABLED.store(false, Ordering::Relaxed);
				stage_allocs += ALLOCS.load(Ordering::Relaxed);
				stage_deallocs += DEALLOCS.load(Ordering::Relaxed);
			}
			(stage_allocs, stage_deallocs, pump_allocs, pump_deallocs)
		})
		.expect("worker");

	println!(
		"10,000 four-datagram sends: stage {stage_allocs}/{stage_deallocs}, acquire+pump {pump_allocs}/{pump_deallocs} allocations/deallocations"
	);
	assert_eq!(stage_allocs, 0, "steady-state send staging allocated");
	assert_eq!(stage_deallocs, 0, "steady-state send staging deallocated");
	// A pending acquire allocates one kio waiter identity. Pumping the previous
	// send's CQEs must add nothing on top of it.
	assert_eq!(pump_allocs, 10_000, "completion pumping changed its allocation count");
	assert_eq!(
		pump_deallocs, 10_000,
		"completion pumping changed its deallocation count"
	);
}

#[cfg(not(target_os = "linux"))]
fn main() {}
