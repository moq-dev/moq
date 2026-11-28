//! Echo a tokio QUIC peer through the io_uring worker while ablating UDP features.

#[cfg(target_os = "linux")]
#[path = "../tests/integration/support.rs"]
mod support;

#[cfg(target_os = "linux")]
mod linux {
	use std::net::UdpSocket;
	use std::thread::JoinHandle;

	use moq_net::transport::poll::SendStream as _;
	use moq_uring::{Config, Worker, quic, udp};

	use super::support;

	pub const PAYLOAD: usize = 1024 * 1024;

	async fn drain(recv: &mut quic::web::recv::Stream) -> Vec<u8> {
		let mut out = Vec::new();
		let mut buf = [0u8; 64 * 1024];
		loop {
			match kio::wait(|waiter| recv.poll_read(waiter, &mut buf))
				.await
				.expect("read")
			{
				Some(n) => out.extend_from_slice(&buf[..n]),
				None => return out,
			}
		}
	}

	async fn write_finish(mut send: quic::web::send::Stream, mut buf: &[u8]) {
		while !buf.is_empty() {
			let n = kio::wait(|waiter| send.poll_write(waiter, buf)).await.expect("write");
			buf = &buf[n..];
		}
		drop(send.finish());
	}

	/// A WebTransport session the worker serves, and a tokio client on its own thread that sends
	/// a payload each time it is started and waits for the echo.
	pub struct Echo {
		start: Option<std::sync::mpsc::Sender<tokio::sync::oneshot::Sender<()>>>,
		client: Option<JoinHandle<()>>,
		session: quic::web::Session,
		_endpoint: quic::Endpoint,
		worker: Worker,
	}

	impl Echo {
		pub fn new(udp_config: udp::Config) -> Self {
			let mut worker = Worker::new(Config::default()).expect("worker");
			let handle = worker.handle();
			let certs = support::certs().expect("certificates");
			let server = quic::server::Config::new(certs.server(&[moq_tokio::quic::web::ALPN]));
			let socket = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp_config)
				.expect("socket");
			let endpoint =
				quic::Endpoint::new(quic::endpoint::Config::default().with_server(server), socket).expect("endpoint");
			let addr = endpoint.local_addr();
			// Queue the first echo while the worker finishes driving the client
			// handshake. A zero-capacity channel would block this thread before it can
			// poll the endpoint again, leaving the peer to time out mid-CONNECT.
			let (start_tx, start_rx) = std::sync::mpsc::channel::<tokio::sync::oneshot::Sender<()>>();
			let client = std::thread::spawn(move || {
				// rustls has no implicit default provider; without one the client builder would panic on
				// this thread while the server waits forever.
				let _ = rind_rustls::default_provider().unwrap().install_default();
				let runtime = tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.expect("runtime");
				runtime.block_on(async move {
					let request = web_transport_proto::ConnectRequest::new(
						url::Url::parse(&format!("https://{addr}/echo")).expect("url"),
					);
					let session = super::support::web_connect(request, Default::default())
						.await
						.expect("connect");
					let payload = vec![0x5a; PAYLOAD];
					while let Ok(done) = start_rx.recv() {
						let (mut send, mut recv) = session.open_bi(Default::default()).await.expect("open stream");
						send.write_all(&payload[..]).await.expect("write");
						drop(send.finish());
						assert_eq!(recv.read_to_end(PAYLOAD + 1).await.expect("read").len(), PAYLOAD);
						done.send(()).expect("notify server");
					}
				});
			});

			let session = worker
				.block_on(async {
					let conn = endpoint.accept().await.expect("accept");
					quic::web::Request::accept(conn)
						.await
						.expect("handshake")
						.ok()
						.await
						.expect("respond")
				})
				.expect("worker");

			Self {
				start: Some(start_tx),
				client: Some(client),
				session,
				_endpoint: endpoint,
				worker,
			}
		}

		/// Echoes one payload the client sends, until the client has read it all back.
		pub fn echo(&mut self) {
			let (done_tx, done_rx) = tokio::sync::oneshot::channel();
			self.start
				.as_ref()
				.expect("client running")
				.send(done_tx)
				.expect("start");
			let session = &self.session;
			self.worker
				.block_on(async {
					let (send, mut recv) = kio::wait(|waiter| session.poll_accept_bi(waiter))
						.await
						.expect("accept stream");
					let payload = drain(&mut recv).await;
					write_finish(send, &payload).await;
					done_rx.await.expect("peer received echo");
				})
				.expect("worker");
		}
	}

	impl Drop for Echo {
		fn drop(&mut self) {
			// Closing the channel ends the client's loop.
			drop(self.start.take());
			if let Some(client) = self.client.take() {
				client.join().expect("client thread");
			}
		}
	}
}

#[cfg(target_os = "linux")]
use std::fmt;

#[cfg(target_os = "linux")]
use linux::{Echo, PAYLOAD};
#[cfg(target_os = "linux")]
use moq_uring::udp;

/// Which UDP features the worker's sockets use.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum Ablation {
	AllOn,
	NoGso,
	NoGro,
	Oneshot,
	AllOff,
}

#[cfg(target_os = "linux")]
impl Ablation {
	fn config(self) -> udp::Config {
		let mut config = udp::Config::default();
		match self {
			Self::AllOn => {}
			Self::NoGso => config.gso = false,
			Self::NoGro => config.gro = false,
			Self::Oneshot => config.multishot = false,
			Self::AllOff => {
				config.gso = false;
				config.gro = false;
				config.multishot = false;
			}
		}
		config
	}
}

#[cfg(target_os = "linux")]
impl fmt::Display for Ablation {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(match self {
			Self::AllOn => "all-on",
			Self::NoGso => "no-gso",
			Self::NoGro => "no-gro",
			Self::Oneshot => "oneshot",
			Self::AllOff => "all-off",
		})
	}
}

#[cfg(target_os = "linux")]
const ABLATIONS: [Ablation; 5] = [
	Ablation::AllOn,
	Ablation::NoGso,
	Ablation::NoGro,
	Ablation::Oneshot,
	Ablation::AllOff,
];

#[cfg(target_os = "linux")]
fn echo(ablation: Ablation) -> Echo {
	Echo::new(ablation.config())
}

#[cfg(target_os = "linux")]
mod echo_quic {
	use swole::bench;

	use super::{ABLATIONS, Echo, PAYLOAD, echo};

	#[bench(args = ABLATIONS, setup = echo, bytes = |_: super::Ablation| PAYLOAD)]
	fn udp(echo: &mut Echo) {
		echo.echo();
	}
}

fn main() {
	swole::main();
}
