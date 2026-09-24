//! Echo a tokio noq peer through the io_uring worker while ablating UDP features.

use criterion::{criterion_group, criterion_main};

#[cfg(target_os = "linux")]
#[path = "../tests/support.rs"]
mod support;

#[cfg(target_os = "linux")]
mod linux {
	use std::net::UdpSocket;
	use std::time::Instant;

	use criterion::{BenchmarkId, Criterion, Throughput};
	use moq_uring::{Config, Error, Worker, quic, udp};
	use web_transport_trait::poll::{RecvStream as _, SendStream as _, Session as _};

	use super::support;

	const PAYLOAD: usize = 1024 * 1024;

	async fn drain(recv: &mut quic::web::RecvStream) -> Vec<u8> {
		let mut out = Vec::new();
		let mut buf = [0u8; 64 * 1024];
		loop {
			match std::future::poll_fn(|cx| recv.poll_read(cx, &mut buf))
				.await
				.expect("read")
			{
				Some(n) => out.extend_from_slice(&buf[..n]),
				None => return out,
			}
		}
	}

	async fn write_finish(send: &mut quic::web::SendStream, mut buf: &[u8]) {
		while !buf.is_empty() {
			let n = std::future::poll_fn(|cx| send.poll_write(cx, buf))
				.await
				.expect("write");
			buf = &buf[n..];
		}
		send.finish().expect("finish");
	}

	fn measure(udp_config: udp::Config, iterations: u64) -> std::time::Duration {
		let mut worker = Worker::new(Config::default()).expect("worker");
		let handle = worker.handle();
		let certs = support::certs().expect("certificates");
		let mut server = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
		server.alpn = vec![web_transport_moq::ALPN.to_string()];
		let socket = handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp_config)
			.expect("socket");
		let endpoint =
			quic::Endpoint::new(socket, quic::endpoint::Config::default().with_server(server)).expect("endpoint");
		let addr = endpoint.local_addr();
		// Queue the first iteration while the worker finishes driving the client
		// handshake. A zero-capacity channel would block this thread before it can
		// poll the endpoint again, leaving the peer to time out mid-CONNECT.
		let (start_tx, start_rx) = std::sync::mpsc::channel::<tokio::sync::oneshot::Sender<()>>();
		let client = std::thread::spawn(move || {
			// Enabling `ring` beside `aws-lc-rs` (as `--all-features` does) leaves rustls no implicit
			// default, and the builder would panic on this thread while the server waits forever.
			let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
			let runtime = tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("runtime");
			runtime.block_on(async move {
				let client = web_transport_moq::ClientBuilder::new()
					.dangerous()
					.with_no_certificate_verification()
					.expect("client");
				let request = web_transport_moq::proto::ConnectRequest::new(
					url::Url::parse(&format!("https://{addr}/echo")).expect("url"),
				);
				let session = client.connect(request).await.expect("connect");
				let payload = vec![0x5a; PAYLOAD];
				while let Ok(done) = start_rx.recv() {
					let (mut send, mut recv) = session.open_bi().await.expect("open stream");
					send.write_all(&payload).await.expect("write");
					send.finish().expect("finish");
					assert_eq!(recv.read_to_end(PAYLOAD + 1).await.expect("read").len(), PAYLOAD);
					done.send(()).expect("notify server");
				}
			});
		});

		let mut session = worker
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

		let start = Instant::now();
		for _ in 0..iterations {
			let (done_tx, done_rx) = tokio::sync::oneshot::channel();
			start_tx.send(done_tx).expect("start");
			worker
				.block_on(async {
					let (mut send, mut recv) = std::future::poll_fn(|cx| session.poll_accept_bi(cx))
						.await
						.expect("accept stream");
					let payload = drain(&mut recv).await;
					write_finish(&mut send, &payload).await;
					done_rx.await.expect("peer received echo");
				})
				.expect("worker");
		}
		let elapsed = start.elapsed();

		drop(start_tx);
		client.join().expect("client thread");
		elapsed
	}

	pub fn benchmark(c: &mut Criterion) {
		match Worker::new(Config::default()) {
			Ok(worker) => drop(worker),
			Err(Error::Unsupported(reason)) => {
				eprintln!("skipping io_uring echo benchmark: {reason}");
				return;
			}
			Err(err) => panic!("worker setup failed: {err}"),
		}

		let all = udp::Config::default();
		let mut no_gso = all.clone();
		no_gso.gso = false;
		let mut no_gro = all.clone();
		no_gro.gro = false;
		let mut oneshot = all.clone();
		oneshot.multishot = false;
		let mut none = all.clone();
		none.gso = false;
		none.gro = false;
		none.multishot = false;
		let ablations = [
			("all-on", all),
			("no-gso", no_gso),
			("no-gro", no_gro),
			("oneshot", oneshot),
			("all-off", none),
		];

		let mut group = c.benchmark_group("echo_noq");
		group.throughput(Throughput::Bytes(PAYLOAD as u64));

		for (name, udp_config) in ablations {
			group.bench_function(BenchmarkId::new("udp", name), |b| {
				b.iter_custom(|iterations| measure(udp_config.clone(), iterations));
			});
		}

		group.finish();
	}
}

#[cfg(target_os = "linux")]
use linux::benchmark;

#[cfg(not(target_os = "linux"))]
fn benchmark(_: &mut criterion::Criterion) {}

criterion_group!(benches, benchmark);
criterion_main!(benches);
