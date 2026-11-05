//! Retained receive heap and delivery cost at the relay's 4 MiB / 1 MiB windows.
//!
//! Each session holds one or eight unread messages, each half a stream window,
//! under native leases. Eight together fill the connection window. Memory includes
//! both endpoints, the bounded byte pipes, tasks, stream tables, and payload backings;
//! it excludes the runtime and the shared source payload. It counts live requested
//! allocator bytes, not RSS or a sum of allocations. Every timed round reads, verifies,
//! releases, and refills every message. Transport, session count, stream count, and
//! QMux version are independent axes. TCP and WebSocket framing run over bounded
//! in-memory pipes to isolate receive ownership from socket scheduling and TLS.

use kio::clock::Instant;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use bytes::{Buf, Bytes};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::transport::poll::{Lease as _, RecvStream as _, SendStream as _, Session as _};
use qmux::{Config, RecvStream, SendStream, Session, Version, transport::Stream};
use tokio::task::JoinSet;

const CONNECTION: usize = 4 << 20;
const STREAM: usize = 1 << 20;
const MESSAGE: usize = STREAM / 2;

struct Counter;
static LIVE: AtomicIsize = AtomicIsize::new(0);
#[global_allocator]
static ALLOCATOR: Counter = Counter;

// Counts requested live bytes once per backing, including reallocations in place.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc(layout) };
		if !ptr.is_null() {
			LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		}
		ptr
	}
	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc_zeroed(layout) };
		if !ptr.is_null() {
			LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		}
		ptr
	}
	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
		let ptr = unsafe { System.realloc(ptr, layout, size) };
		if !ptr.is_null() {
			LIVE.fetch_add(size as isize - layout.size() as isize, Ordering::Relaxed);
		}
		ptr
	}
	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[derive(Clone, Copy)]
struct Shape {
	websocket: bool,
	version: Version,
	sessions: usize,
	streams: usize,
}

struct Receivers {
	sessions: Vec<(Session, Session)>,
	streams: Vec<(SendStream, RecvStream)>,
	drivers: JoinSet<()>,
	payload: Bytes,
}

impl Receivers {
	async fn new(shape: Shape, payload: Bytes) -> Self {
		let Shape {
			websocket,
			version,
			sessions,
			streams,
		} = shape;
		let mut config = Config::new(version);
		config.limits.receive_window = std::num::NonZeroU64::new(CONNECTION as u64);
		config.limits.stream_receive_window = std::num::NonZeroU64::new(STREAM as u64);
		config.limits.send_window = std::num::NonZeroU64::new(CONNECTION as u64);
		let mut rig = Self {
			sessions: Vec::with_capacity(sessions),
			streams: Vec::with_capacity(sessions * streams),
			drivers: JoinSet::new(),
			payload,
		};
		for _ in 0..sessions {
			let (client, server) = tokio::io::duplex(64 << 10);
			let (client, server) = if websocket {
				use qmux::ws::{Upgraded, tokio_tungstenite::WebSocketStream, tungstenite::protocol::Role};
				let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
				let server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
				let (client, driver) = Upgraded::new(client, version.alpn())
					.with_session_config(config.clone())
					.connect()
					.unwrap();
				rig.drivers.spawn(driver);
				let (server, driver) = Upgraded::new(server, version.alpn())
					.with_session_config(config.clone())
					.accept()
					.unwrap();
				rig.drivers.spawn(driver);
				(client, server)
			} else {
				let (client, server) = tokio::join!(
					Session::connect(Stream::new(client), config.clone()),
					Session::accept(Stream::new(server), config.clone()),
				);
				let (client, driver) = client.unwrap();
				rig.drivers.spawn(driver);
				let (server, driver) = server.unwrap();
				rig.drivers.spawn(driver);
				(client, server)
			};
			for _ in 0..streams {
				let mut send = client.open_uni(Default::default()).await.unwrap();
				send.write_chunk(rig.payload.clone()).await.unwrap();
				let mut recv = server.accept_uni().await.unwrap();
				// Dropping an unread lease leaves the bytes charged and unread in the stream.
				kio::wait(|waiter| recv.poll_lease(waiter, MESSAGE).map_ok(drop))
					.await
					.unwrap();
				rig.streams.push((send, recv));
			}
			rig.sessions.push((client, server));
		}
		rig
	}

	async fn round(&mut self) {
		for (send, recv) in &mut self.streams {
			kio::wait(|waiter| {
				recv.poll_lease(waiter, MESSAGE).map_ok(|mut lease| {
					let received = lease.copy_to_bytes(MESSAGE);
					assert_eq!(received, self.payload);
					lease.release();
				})
			})
			.await
			.unwrap();
			send.write_chunk(self.payload.clone()).await.unwrap();
			kio::wait(|waiter| recv.poll_lease(waiter, MESSAGE).map_ok(drop))
				.await
				.unwrap();
		}
		assert!(self.drivers.try_join_next().is_none(), "a session driver ended");
	}

	async fn close(mut self) {
		self.streams.clear();
		self.sessions.clear();
		self.drivers.shutdown().await;
	}
}

fn receive(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let payload = Bytes::from((0..MESSAGE).map(|i| (i % 251) as u8).collect::<Vec<_>>());
	let mut group = c.benchmark_group("qmux_receive");
	for (transport, websocket) in [("pipe", false), ("ws", true)] {
		for &version in Version::ALL {
			for sessions in [1, 8, 64] {
				for streams in [1, 8] {
					let id = format!("{transport}/{}/{sessions}s/{streams}streams", version.alpn());
					group.throughput(Throughput::Bytes((sessions * streams * MESSAGE) as u64));
					let mut rig = None;
					group.bench_function(BenchmarkId::from_parameter(&id), |b| {
						let rig = rig.get_or_insert_with(|| {
							let before = LIVE.load(Ordering::Relaxed);
							let mut rig = runtime.block_on(Receivers::new(
								Shape {
									websocket,
									version,
									sessions,
									streams,
								},
								payload.clone(),
							));
							runtime.block_on(rig.round());
							runtime.block_on(rig.round());
							let held = LIVE.load(Ordering::Relaxed) - before;
							eprintln!(
								"{id}: retained={held} B, per-session={} B, unread-per-session={} B, connection-window={CONNECTION} B",
								held / sessions as isize,
								streams * MESSAGE
							);
							rig
						});
						b.iter_custom(|iters| {
							runtime.block_on(async {
								let start = Instant::now();
								for _ in 0..iters {
									rig.round().await;
								}
								start.elapsed()
							})
						});
					});
					if let Some(rig) = rig {
						runtime.block_on(rig.close());
					}
				}
			}
		}
	}
	group.finish();
}

criterion_group! {
	name = benches;
	config = Criterion::default().warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2)).sample_size(10);
	targets = receive
}
criterion_main!(benches);
