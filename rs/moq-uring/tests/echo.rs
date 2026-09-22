//! A tokio noq peer echoing through the io_uring WebTransport server.

#![cfg(all(target_os = "linux", feature = "noq"))]

#[path = "support.rs"]
mod support;

use std::net::UdpSocket;

use moq_uring::{Config, Error, Worker, quic, udp};
use web_transport_trait::poll::{RecvStream as _, SendStream as _, Session as _};

const PAYLOAD: usize = 512 * 1024;

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring echo test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

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

#[test]
fn echo_noq_peer() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let mut server = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	server.alpn = vec![web_transport_moq::ALPN.to_string()];
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
			let (mut send, mut recv) = session.open_bi().await.expect("open stream");
			send.write_all(&payload).await.expect("write");
			send.finish().expect("finish");
			let echoed = recv.read_to_end(PAYLOAD + 1).await.expect("read");
			done_tx.send(()).expect("notify server");
			echoed
		})
	});

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let request = quic::web::Request::accept(conn).await.expect("handshake");
			let mut session = request.ok().await.expect("respond");
			let (mut send, mut recv) = std::future::poll_fn(|cx| session.poll_accept_bi(cx))
				.await
				.expect("accept stream");
			let payload = drain(&mut recv).await;
			write_finish(&mut send, &payload).await;
			done_rx.await.expect("peer received echo");
		})
		.expect("worker");

	assert_eq!(client.join().expect("client thread"), expected);
}
