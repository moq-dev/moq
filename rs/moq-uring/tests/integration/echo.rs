//! A tokio QUIC peer echoing through the io_uring WebTransport server.

#![cfg(feature = "quic")]

use crate::support;

use std::net::UdpSocket;

use moq_net::transport::poll::SendStream as _;
use moq_uring::{Config, Error, Worker, quic, udp};

const PAYLOAD: usize = 512 * 1024;

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring echo test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

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

#[test]
fn echo_tokio_peer() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let server = quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&[moq_tokio::quic::web::ALPN], quic::server::ClientAuth::None)
			.expect("tls"),
	);
	let socket = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let endpoint =
		quic::Endpoint::new(socket, quic::endpoint::Config::default().with_server(server)).expect("endpoint");
	let addr = endpoint.local_addr();
	let payload: Vec<u8> = (0..PAYLOAD).map(|i| (i * 31 % 251) as u8).collect();
	let expected = payload.clone();
	let (done_tx, done_rx) = tokio::sync::oneshot::channel();

	let client = std::thread::spawn(move || {
		// Enabling `ring` beside `aws-lc-rs` (as `--all-features` does) leaves rustls no implicit
		// default, and the builder would panic on this thread while the server waits forever.
		let _ = rind_rustls::default_provider().unwrap().install_default();
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("runtime");
		runtime.block_on(async move {
			let request = moq_tokio::quic::web::proto::ConnectRequest::new(
				url::Url::parse(&format!("https://{addr}/echo")).expect("url"),
			);
			let session = crate::support::web_connect(request, Default::default())
				.await
				.expect("connect");
			let (mut send, mut recv) = session.open_bi(Default::default()).await.expect("open stream");
			send.write_all(&payload[..]).await.expect("write");
			drop(send.finish());
			let echoed = recv.read_to_end(PAYLOAD + 1).await.expect("read");
			done_tx.send(()).expect("notify server");
			echoed
		})
	});

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let request = quic::web::Request::accept(conn).await.expect("handshake");
			let session = request.ok().await.expect("respond");
			let (send, mut recv) = kio::wait(|waiter| session.poll_accept_bi(waiter))
				.await
				.expect("accept stream");
			let payload = drain(&mut recv).await;
			write_finish(send, &payload).await;
			done_rx.await.expect("peer received echo");
		})
		.expect("worker");

	assert_eq!(client.join().expect("client thread"), expected);
}
