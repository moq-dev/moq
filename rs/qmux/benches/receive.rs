//! Delivery cost at the relay's 4 MiB / 1 MiB windows.
//!
//! Each session holds one or eight unread messages, each half a stream window,
//! under native leases. Eight together fill the connection window. Every call reads,
//! verifies, releases, and refills every message. Transport, session count, stream count,
//! and QMux version are independent axes. TCP and WebSocket framing run over bounded
//! in-memory pipes to isolate receive ownership from socket scheduling and TLS.
//!
//! Run with `cargo bench --manifest-path rs/qmux/Cargo.toml --bench receive`.

use std::fmt;

use bytes::{Buf, Bytes};
use moq_net::transport::poll::{Lease as _, RecvStream as _, SendStream as _, Session as _};
use qmux::{Config, RecvStream, SendStream, Session, Version, transport::Stream};
use swole::bench;
use tokio::task::JoinSet;

const CONNECTION: usize = 4 << 20;
const STREAM: usize = 1 << 20;
const MESSAGE: usize = STREAM / 2;

/// One case, named `<pipe|ws>/<alpn>/<sessions>s/<streams>streams`.
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
				use qmux::ws::Upgraded;
				let (client, server) = tokio::join!(
					Upgraded::new(client, version.alpn())
						.with_session_config(config.clone())
						.connect(),
					Upgraded::new(server, version.alpn())
						.with_session_config(config.clone())
						.accept(),
				);
				let (client, driver) = client.unwrap();
				rig.drivers.spawn(driver);
				let (server, driver) = server.unwrap();
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
}

impl Shape {
	fn bytes(self) -> usize {
		self.sessions * self.streams * MESSAGE
	}
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let transport = if self.websocket { "ws" } else { "pipe" };
		write!(
			f,
			"{transport}/{}/{}s/{}streams",
			self.version.alpn(),
			self.sessions,
			self.streams
		)
	}
}

fn shapes() -> Vec<Shape> {
	let mut shapes = Vec::new();
	for websocket in [false, true] {
		for &version in Version::ALL {
			for sessions in [1, 8, 64] {
				for streams in [1, 8] {
					shapes.push(Shape {
						websocket,
						version,
						sessions,
						streams,
					});
				}
			}
		}
	}
	shapes
}

/// Receivers warmed by two rounds, and the runtime driving them.
struct Rig {
	receivers: Receivers,
	// Declared last so it drops after the drivers it runs.
	runtime: tokio::runtime::Runtime,
}

fn rig(shape: Shape) -> Rig {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let payload = Bytes::from((0..MESSAGE).map(|i| (i % 251) as u8).collect::<Vec<_>>());
	let mut receivers = runtime.block_on(Receivers::new(shape, payload));
	runtime.block_on(receivers.round());
	runtime.block_on(receivers.round());
	Rig { receivers, runtime }
}

#[bench(args = shapes(), setup = rig, bytes = Shape::bytes)]
fn qmux_receive(rig: &mut Rig) {
	rig.runtime.block_on(rig.receivers.round());
}

fn main() {
	swole::main();
}
