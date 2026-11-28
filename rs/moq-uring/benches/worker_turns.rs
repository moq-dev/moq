//! Worker submission and completion amortization across sockets and burst sizes.

#[cfg(all(target_os = "linux", feature = "quic"))]
#[path = "../tests/integration/support.rs"]
mod support;

#[cfg(target_os = "linux")]
use std::fmt;

#[cfg(target_os = "linux")]
use moq_uring::{Config, Worker, udp};
#[cfg(target_os = "linux")]
use swole::bench;

/// Receiving sockets against datagrams sent to each per call.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Turns {
	sockets: usize,
	burst: usize,
}

#[cfg(target_os = "linux")]
impl fmt::Display for Turns {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "sockets-{}/{}", self.sockets, self.burst)
	}
}

#[cfg(target_os = "linux")]
fn turns() -> Vec<Turns> {
	[1, 4, 16]
		.into_iter()
		.flat_map(|sockets| [1, 8, 64].map(|burst| Turns { sockets, burst }))
		.collect()
}

/// One sender and `sockets` receivers on one worker.
#[cfg(target_os = "linux")]
struct Sockets {
	burst: usize,
	sender: udp::Socket,
	receivers: Vec<udp::Socket>,
	worker: Worker,
}

#[cfg(target_os = "linux")]
fn sockets(case: Turns) -> Sockets {
	let worker = Worker::new(Config::default()).expect("io_uring worker");
	let handle = worker.handle();
	let mut config = udp::Config::default();
	config.gro = false;
	config.gso = false;
	config.rx_buffer_len = 2048;
	config.tx_buffer_len = 2048;
	config.rx_buffers_max = 64;
	let bind = || std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
	let sender = handle.udp(bind(), config.clone()).expect("sender");
	let receivers = (0..case.sockets)
		.map(|_| handle.udp(bind(), config.clone()).expect("receiver"))
		.collect();
	Sockets {
		burst: case.burst,
		sender,
		receivers,
		worker,
	}
}

#[cfg(target_os = "linux")]
#[bench(args = turns(), setup = sockets, items = |case: Turns| case.sockets * case.burst)]
fn worker_turns(sockets: &mut Sockets) {
	let Sockets {
		burst,
		sender,
		receivers,
		worker,
	} = sockets;
	worker
		.block_on(async {
			for receiver in receivers.iter() {
				for _ in 0..*burst {
					let mut tx = sender.acquire().await.expect("send buffer");
					tx.extend_from_slice(b"turn");
					tx.send(udp::Transmit::new(receiver.local_addr().expect("addr"), 4))
						.expect("send");
				}
			}
			for receiver in receivers.iter() {
				for _ in 0..*burst {
					assert_eq!(receiver.recv().await.expect("receive").payload(), b"turn");
				}
			}
		})
		.expect("worker");
}

/// A request and its reply traverse demux, connection and application tasks.
/// Sweep connections and reply bytes so batching cannot hide a fanout cost.
#[cfg(all(target_os = "linux", feature = "quic"))]
mod request_reply {
	use std::{fmt, net::UdpSocket, time::Duration};

	use moq_uring::{Config, Handle, Worker, quic, udp};

	const ALPN: &str = "worker-turns";

	/// Connections against reply bytes.
	#[derive(Clone, Copy)]
	pub struct Case {
		pub connections: usize,
		pub bytes: usize,
	}

	impl fmt::Display for Case {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			write!(f, "connections-{}/bytes-{}", self.connections, self.bytes)
		}
	}

	pub fn cases() -> Vec<Case> {
		[1, 16]
			.into_iter()
			.flat_map(|connections| [64, 256 * 1024].map(|bytes| Case { connections, bytes }))
			.collect()
	}

	type Streams = Vec<(quic::send::Stream, quic::recv::Stream)>;

	/// One stream per connection to a server that answers each 64-byte request with a reply.
	pub struct Fixture {
		case: Case,
		streams: Streams,
		reply: Vec<u8>,
		completed: kio::Queue<(Streams, Vec<u8>)>,
		_held: Vec<quic::Connection>,
		_client: quic::Endpoint,
		_server: quic::Endpoint,
		handle: Handle,
		worker: Worker,
	}

	pub fn fixture(case: Case) -> Fixture {
		let _ = rind_rustls::default_provider().unwrap().install_default();
		let certs = super::support::certs().expect("certificates");
		let tls = certs.server(&[ALPN]);
		let mut worker = Worker::new(Config::default()).expect("io_uring worker");
		let handle = worker.handle();
		let socket = || {
			handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
				.expect("socket")
		};
		let server = quic::Endpoint::new(
			quic::endpoint::Config::default().with_server(quic::server::Config::new(tls)),
			socket(),
		)
		.expect("server");
		let client = quic::Endpoint::new(quic::endpoint::Config::default(), socket()).expect("client");
		let dial = quic::client::Config::new(super::support::client_tls(&[ALPN]));
		let bytes = case.bytes;
		for _ in 0..case.connections {
			let server = server.clone();
			handle.spawn(async move {
				let conn = server.accept().await.expect("accept");
				let (mut send, mut recv) = conn.accept_bi().await.expect("stream");
				let reply = bytes::Bytes::from(vec![0x5a; bytes]);
				let mut request = [0; 64];
				while recv.read_exact(&mut request).await.is_ok() {
					send.write_all(reply.clone()).await.expect("reply");
				}
			});
		}
		let (streams, held) = worker
			.block_on(async {
				let mut streams = Vec::new();
				let mut held = Vec::new();
				for _ in 0..case.connections {
					let conn = client
						.connect(&dial, server.local_addr(), "localhost")
						.await
						.expect("dial");
					streams.push(conn.open_bi(Default::default()).await.expect("stream"));
					held.push(conn);
				}
				(streams, held)
			})
			.expect("worker");
		Fixture {
			case,
			streams,
			reply: vec![0; bytes],
			completed: kio::Queue::bounded(1),
			_held: held,
			_client: client,
			_server: server,
			handle,
			worker,
		}
	}

	/// Runs `rounds` request/reply rounds as one task, timing the worker until it completes, and
	/// prints the worker's counter deltas over them.
	#[expect(
		clippy::disallowed_methods,
		clippy::disallowed_types,
		reason = "the benchmark times itself on the host clock"
	)]
	pub fn rounds(fixture: &mut Fixture, rounds: u64) -> Duration {
		let mut running = std::mem::take(&mut fixture.streams);
		let mut answer = std::mem::take(&mut fixture.reply);
		let done = fixture.completed.clone();
		fixture.handle.spawn(async move {
			for _ in 0..rounds {
				for (send, _) in &mut running {
					send.write_all(&[0xa5; 64][..]).await.expect("request");
				}
				for (_, recv) in &mut running {
					recv.read_exact(&mut answer).await.expect("reply");
				}
			}
			done.try_push((running, answer)).expect("complete");
		});
		let before = fixture.handle.metrics().snapshot();
		let start = std::time::Instant::now();
		let completed = &fixture.completed;
		(fixture.streams, fixture.reply) = fixture
			.worker
			.block_on(kio::wait(|waiter| completed.poll_pop(waiter)))
			.expect("worker")
			.expect("measurement");
		let elapsed = start.elapsed();
		let after = fixture.handle.metrics().snapshot();
		assert!(fixture.reply.iter().all(|byte| *byte == 0x5a));
		eprintln!(
			"{} rounds={rounds} turns={} passes={} enters={} rx_datagrams={}",
			fixture.case,
			after.turns - before.turns,
			after.passes - before.passes,
			after.enters - before.enters,
			after.rx_datagrams - before.rx_datagrams,
		);
		elapsed
	}
}

/// Requests and replies, timing each run of rounds as one task.
#[cfg(all(target_os = "linux", feature = "quic"))]
#[bench(
	args = request_reply::cases(),
	setup = request_reply::fixture,
	bytes = |case: request_reply::Case| case.connections * (64 + case.bytes),
)]
fn worker_request_reply(fixture: &mut request_reply::Fixture, iterations: swole::Iterations) -> std::time::Duration {
	request_reply::rounds(fixture, iterations.get())
}

/// Compare the same transport method before and after Local's thread guard.
/// The memory transport returns a constant here, keeping actual I/O out of the check cost.
#[cfg(target_os = "linux")]
mod local_thread_check {
	use std::hint::black_box;

	use moq_net::transport::{memory, poll::Session as _};
	use moq_uring::transport::Local;
	use swole::bench;

	/// A memory session, its peer kept open, and the session behind Local.
	type Sessions = (memory::Session, memory::Session, Local<memory::Session>);

	fn sessions() -> Sessions {
		let (session, peer) = memory::pair(memory::Config::default());
		let local = Local::new(session.clone());
		(session, peer, local)
	}

	#[bench(setup = sessions)]
	fn direct((session, _, _): &Sessions) -> usize {
		black_box(session).max_datagram_size()
	}

	#[bench(setup = sessions)]
	fn checked((_, _, local): &Sessions) -> usize {
		black_box(local).max_datagram_size()
	}
}

fn main() {
	swole::main();
}
