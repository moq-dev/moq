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

		let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("certificate");
		let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()));
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

criterion_group!(benches, bench_fanout);
criterion_main!(benches);
