//! Stream fanout through the tokio QUIC driver: one connection, many short streams at once.
//!
//! Each iteration the client opens N unidirectional streams on a long-lived localhost
//! connection, writes a small message on each and finishes it, while the server accepts and
//! reads every stream to the end. Reported per stream, so a cost that grows with the number of
//! streams in flight (a scan over every stream per event, say) shows as a slope across N.
//!
//! Run with `cargo bench -p moq-tokio --bench fanout`.

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
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::num::NonZeroU64;
use std::sync::Arc;
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

mod fanout {
	use swole::bench;

	use super::{MESSAGE, Pair, STREAMS};

	/// A connected pair and how many streams each call opens.
	struct Fanout {
		pair: Pair,
		streams: usize,
	}

	fn fanout(streams: usize) -> Fanout {
		Fanout {
			pair: Pair::new(),
			streams,
		}
	}

	#[bench(args = STREAMS, setup = fanout, items = |streams| streams)]
	fn uni_1kib(fanout: &Fanout) {
		fanout.pair.fanout(fanout.streams, &[0x5a; MESSAGE]);
	}
}

/// The same connection through `moq-net`'s poll transport API.
mod native_transport {
	use std::fmt;

	use bytes::Bytes;
	use moq_net::transport::poll::{RecvStream as _, SendStream as _};
	use moq_tokio::quic::web::Session;
	use swole::bench;

	use super::Pair;

	/// Message size and streams per call, named `bytes-<size>/streams-<n>`.
	#[derive(Clone, Copy)]
	struct Case {
		size: usize,
		streams: usize,
	}

	impl fmt::Display for Case {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			write!(f, "bytes-{}/streams-{}", self.size, self.streams)
		}
	}

	fn cases() -> Vec<Case> {
		[64, 64 * 1024]
			.into_iter()
			.flat_map(|size| [1, 16, 256].map(|streams| Case { size, streams }))
			.collect()
	}

	/// Both ends of a connected pair as transport sessions, and what each call sends.
	struct Native {
		client: Session,
		server: Session,
		message: Bytes,
		streams: usize,
		// Declared last so the sessions drop before the runtime and endpoints they use.
		pair: Pair,
	}

	fn native(case: Case) -> Native {
		let pair = Pair::new();
		Native {
			client: Session::raw(pair.client.clone()),
			server: Session::raw(pair.server.clone()),
			message: Bytes::from(vec![0x5a; case.size]),
			streams: case.streams,
			pair,
		}
	}

	/// Generic so it sends through the transport traits rather than the session's own methods.
	fn send<S: moq_net::transport::poll::Session>(
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

	#[bench(args = cases(), setup = native, items = |case: Case| case.streams)]
	fn transfer(native: &Native) {
		send(
			&native.pair,
			&native.client,
			&native.server,
			native.streams,
			&native.message,
		);
	}
}

fn main() {
	swole::main();
}
