//! Stream fanout through the tokio QUIC driver: one connection, many short streams at once.
//!
//! Each iteration the client opens N unidirectional streams on a long-lived localhost
//! connection, writes a small message on each and finishes it, while the server accepts and
//! reads every stream to the end. Reported per stream, so a cost that grows with the number of
//! streams in flight (a scan over every stream per event, say) shows as a slope across N.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::{StreamExt as _, stream::FuturesUnordered};
use moq_tokio::quic::driver::{
	Limits, client,
	connection::Connection,
	endpoint::{self, Endpoint},
	server, transport,
};
use rustls::{
	RootCertStore,
	pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::runtime::Runtime;

/// Streams in flight per iteration.
const STREAMS: [usize; 4] = [1, 16, 256, 1024];
/// Bytes written on each stream.
const MESSAGE: usize = 1024;

/// A client connected to a server over localhost, each on its own endpoint.
struct Pair {
	runtime: Runtime,
	client: Connection,
	server: Connection,
	// Dropping an endpoint closes its connections.
	_endpoints: [Endpoint; 2],
}

impl Pair {
	fn new() -> Self {
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("runtime");

		// Credit for every stream and byte of the largest batch, so an iteration measures
		// stream handling rather than waiting on the peer to extend flow control.
		let mut limits = Limits::default();
		limits.max_streams = NonZeroU64::new(4 * STREAMS[STREAMS.len() - 1] as u64);
		limits.receive_window = NonZeroU64::new(64 * 1024 * 1024);
		let mut transport = transport::Config::default();
		transport.limits = limits;

		let cert = (|| -> std::result::Result<_, rcgen::Error> {
			let signing_key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256)?;
			let mut params = rcgen::CertificateParams::new(vec!["localhost".into()])?;
			params.serial_number = Some(1u64.into());
			let cert = params.self_signed(&signing_key)?;
			Ok(rcgen::CertifiedKey { cert, signing_key })
		})()
		.expect("certificate");
		let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
			cert.signing_key.to_pkcs8().secret_pkcs8_der().to_vec(),
		));
		let mut server_config =
			server::Config::with_single_cert(vec![cert.cert.der().clone()], key).expect("server config");
		server_config.transport = transport.clone();
		let mut roots = RootCertStore::empty();
		roots.add(cert.cert.der().clone()).expect("root");
		let mut client_config = client::Config::with_root_certificates(Arc::new(roots)).expect("client config");
		client_config.transport = transport;

		let localhost = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
		let (client, server, endpoints) = runtime.block_on(async {
			let server_endpoint = Endpoint::new(
				endpoint::Config::default().with_server(server_config),
				UdpSocket::bind(localhost).expect("bind"),
			)
			.expect("server endpoint");
			let client_endpoint = Endpoint::new(endpoint::Config::default(), UdpSocket::bind(localhost).expect("bind"))
				.expect("client endpoint");
			let connecting = client_endpoint
				.connect(&client_config, server_endpoint.local_addr(), "localhost")
				.expect("connect");
			let accepting = async {
				let incoming = server_endpoint.accept().await.expect("incoming");
				incoming.accept().expect("accept").await.expect("server handshake")
			};
			let (client, server) = tokio::join!(connecting, accepting);
			(
				client.expect("client handshake"),
				server,
				[client_endpoint, server_endpoint],
			)
		});

		Self {
			runtime,
			client,
			server,
			_endpoints: endpoints,
		}
	}

	/// Send `streams` messages, each on its own stream, and wait until the server read them all.
	fn fanout(&self, streams: usize, message: &[u8]) {
		self.runtime.block_on(async {
			let send = async {
				let mut writes: FuturesUnordered<_> = (0..streams)
					.map(|_| async {
						let mut stream = self.client.open_uni(Default::default()).await.expect("open");
						stream.write_all(message).await.expect("write");
						// The server reading to the end is the iteration's completion, so the
						// acknowledgement `Finished::stopped` waits for is left unawaited.
						drop(stream.finish());
					})
					.collect();
				while writes.next().await.is_some() {}
			};

			let recv = async {
				let mut reads = FuturesUnordered::new();
				let (mut accepted, mut read) = (0, 0);
				while read < streams {
					tokio::select! {
						stream = self.server.accept_uni(), if accepted < streams => {
							accepted += 1;
							let mut stream = stream.expect("accept");
							reads.push(async move { stream.read_to_end(usize::MAX).await.expect("read").len() });
						}
						Some(len) = reads.next(), if !reads.is_empty() => {
							assert_eq!(len, message.len());
							read += 1;
						}
					}
				}
			};

			tokio::join!(send, recv);
		});
	}
}

fn bench_fanout(c: &mut Criterion) {
	let pair = Pair::new();
	let message = vec![0x5a; MESSAGE];
	let mut group = c.benchmark_group("fanout");
	group.sample_size(20);
	group.warm_up_time(Duration::from_secs(1));
	group.measurement_time(Duration::from_secs(5));
	for streams in STREAMS {
		group.throughput(Throughput::Elements(streams as u64));
		group.bench_with_input(BenchmarkId::new("uni_1KiB", streams), &streams, |b, &streams| {
			b.iter(|| pair.fanout(streams, &message));
		});
	}
	group.finish();
}

#[cfg(feature = "quic")]
mod native {
	use super::*;
	use bytes::Bytes;
	use moq_net::transport::poll::{RecvStream as _, SendStream as _};
	use std::alloc::{GlobalAlloc, Layout, System};
	use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

	struct Counter;
	static COUNTING: AtomicBool = AtomicBool::new(false);
	static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
	static ALLOCATED: AtomicU64 = AtomicU64::new(0);
	#[global_allocator]
	static ALLOCATOR: Counter = Counter;

	fn count(size: usize) {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
			ALLOCATED.fetch_add(size as u64, Ordering::Relaxed);
		}
	}
	unsafe impl GlobalAlloc for Counter {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			count(layout.size());
			unsafe { System.alloc(layout) }
		}
		unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
			count(layout.size());
			unsafe { System.alloc_zeroed(layout) }
		}
		unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
			count(size);
			unsafe { System.realloc(ptr, layout, size) }
		}
		unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
			unsafe { System.dealloc(ptr, layout) }
		}
	}

	fn transfer<S: moq_net::transport::poll::Session>(
		pair: &Pair,
		client: &S,
		server: &S,
		streams: usize,
		message: &Bytes,
	) {
		pair.runtime.block_on(async {
			let send = async {
				for _ in 0..streams {
					let mut send = client.open_uni(Default::default()).await.expect("open");
					send.write_chunk(message.clone()).await.expect("write");
					send.finish().expect("finish");
				}
			};
			let recv = async {
				for _ in 0..streams {
					let mut recv = server.accept_uni().await.expect("accept");
					let mut offset = 0;
					while let Some(chunk) = recv.read_chunk(message.len()).await.expect("read") {
						assert_eq!(chunk, message.slice(offset..offset + chunk.len()));
						offset += chunk.len();
					}
					assert_eq!(offset, message.len());
				}
			};
			tokio::join!(send, recv);
		});
	}

	pub fn benchmark(c: &mut Criterion) {
		let pair = Pair::new();
		let client = moq_tokio::quic::web::Session::raw(pair.client.clone());
		let server = moq_tokio::quic::web::Session::raw(pair.server.clone());
		let allocations = std::env::var_os("NATIVE_ALLOCS").is_some();
		let mut group = c.benchmark_group("native_transport");
		for size in [64, 64 * 1024] {
			let message = Bytes::from(vec![0x5a; size]);
			for streams in [1, 16, 256] {
				if allocations {
					for _ in 0..16 {
						transfer(&pair, &client, &server, streams, &message);
					}
					ALLOCATIONS.store(0, Ordering::Relaxed);
					ALLOCATED.store(0, Ordering::Relaxed);
					COUNTING.store(true, Ordering::Relaxed);
					for _ in 0..64 {
						transfer(&pair, &client, &server, streams, &message);
					}
					COUNTING.store(false, Ordering::Relaxed);
					let count = (streams * 64) as f64;
					println!(
						"native_transport bytes={size} streams={streams} allocations_per_stream={} allocated_bytes_per_stream={}",
						ALLOCATIONS.load(Ordering::Relaxed) as f64 / count,
						ALLOCATED.load(Ordering::Relaxed) as f64 / count
					);
				} else {
					group.throughput(Throughput::Elements(streams as u64));
					group.bench_function(format!("bytes-{size}/streams-{streams}"), |b| {
						b.iter(|| transfer(&pair, &client, &server, streams, &message));
					});
				}
			}
		}
		group.finish();
	}
}

#[cfg(not(feature = "quic"))]
mod native {
	pub fn benchmark(_: &mut criterion::Criterion) {}
}

criterion_group!(benches, bench_fanout, native::benchmark);
criterion_main!(benches);
