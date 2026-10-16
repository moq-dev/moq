//! Held receive credit through the io_uring driver: held reads keep the
//! connection window spent until their credit drops, and bytes `poll_closed`
//! reads ahead keep theirs until the application reads them.
//!
//! Unsupported local kernels skip loudly; MOQ_STRICT makes CI refuse them.

#![cfg(all(target_os = "linux", feature = "quic"))]

#[path = "support.rs"]
mod support;

use std::net::UdpSocket;
use std::task::Poll;
use std::time::Duration;

use moq_uring::{Config, Error, Worker, quic, udp};
use web_transport_trait::poll::{RecvStream as _, SendStream as _, Session as _};

const ALPN: &str = "moq-uring-credit";
/// The server's connection window; under `poll_closed`'s read-ahead cap so
/// one window fits in its backlog.
const WINDOW: usize = 32 * 1024;

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("MOQ_STRICT").is_none() => {
			eprintln!("skipping io_uring credit test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

/// Await `future`, failing loudly rather than hanging if it never resolves.
async fn within<T>(handle: &moq_uring::Handle, what: &str, future: impl Future<Output = T>) -> T {
	let mut deadline = moq_uring::Timer::after(handle, Duration::from_secs(5));
	let mut future = std::pin::pin!(future);
	kio::wait(|waiter| {
		let mut cx = waiter.context();
		if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
			return Poll::Ready(Some(value));
		}
		deadline.poll(waiter).map(|()| None)
	})
	.await
	.unwrap_or_else(|| panic!("timed out waiting for {what}"))
}

/// Read exactly `len` bytes, keeping their credit.
async fn read_held(handle: &moq_uring::Handle, recv: &mut quic::RecvStream, len: usize) -> quic::Credit {
	let mut buf = vec![0u8; len];
	let mut held: Option<quic::Credit> = None;
	while held.as_ref().map_or(0, quic::Credit::len) < len as u64 {
		let filled = held.as_ref().map_or(0, |held| held.len() as usize);
		let credit = within(
			handle,
			"stream data",
			std::future::poll_fn(|cx| recv.poll_read_held(cx, &mut buf[filled..])),
		)
		.await
		.expect("read")
		.expect("not finished");
		match &mut held {
			Some(held) => held.merge(credit),
			None => held = Some(credit),
		}
	}
	held.expect("len > 0")
}

/// Read exactly `len` bytes.
async fn read(handle: &moq_uring::Handle, recv: &mut quic::RecvStream, len: usize) {
	let mut buf = vec![0u8; len];
	let mut filled = 0;
	while filled < len {
		let n = within(
			handle,
			"stream data",
			std::future::poll_fn(|cx| recv.poll_read(cx, &mut buf[filled..])),
		)
		.await
		.expect("read")
		.expect("not finished");
		filled += n;
	}
}

#[test]
fn held_reads_spend_the_connection_window() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let socket = |handle: &moq_uring::Handle| {
		handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket")
	};
	let mut server_config = quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&[ALPN], quic::server::ClientAuth::None)
			.expect("tls"),
	);
	server_config.transport.receive_window = Some(WINDOW as u64);
	// Far smaller than the window: held reads must keep replenishing it.
	server_config.transport.stream_receive_window = Some(4096);
	let server = quic::Endpoint::new(
		socket(&handle),
		quic::endpoint::Config::default().with_server(server_config),
	)
	.expect("server endpoint");
	let client = quic::Endpoint::new(socket(&handle), quic::endpoint::Config::default()).expect("dial-only endpoint");
	let mut dial = quic::client::Config::new(server.local_addr(), "localhost");
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;

	worker
		.block_on(async move {
			let mut dialed = client.connect(&dial).await.expect("dial");
			let mut accepted = server.accept().await.expect("accept");

			let mut send = std::future::poll_fn(|cx| dialed.poll_open_uni(cx))
				.await
				.expect("open_uni");
			handle.spawn(async move {
				let payload = vec![7u8; 4 * WINDOW];
				let mut buf = &payload[..];
				while !buf.is_empty() {
					match std::future::poll_fn(|cx| send.poll_write(cx, buf)).await {
						Ok(n) => buf = &buf[n..],
						// The receiver stops once it has seen enough.
						Err(_) => return,
					}
				}
				let _ = send.finish();
				let _ = std::future::poll_fn(|cx| send.poll_closed(cx)).await;
			});

			let mut recv = within(
				&handle,
				"the stream",
				std::future::poll_fn(|cx| accepted.poll_accept_uni(cx)),
			)
			.await
			.expect("accept_uni");

			let mut held = read_held(&handle, &mut recv, WINDOW).await;
			let stats = accepted.stats().credit;
			assert_eq!((stats.held, stats.available), (WINDOW as u64, 0));

			// Far below the usual update threshold, and still enough to unblock.
			drop(held.split(100));
			held.merge(read_held(&handle, &mut recv, 100).await);
			assert_eq!(accepted.stats().credit.held, WINDOW as u64);

			// Read-ahead keeps the credit the application has not taken yet.
			drop(held);
			within(
				&handle,
				"a window of read-ahead",
				std::future::poll_fn(|cx| match recv.poll_closed(cx) {
					Poll::Ready(res) => panic!("stream ended early: {res:?}"),
					Poll::Pending if accepted.stats().credit.held == WINDOW as u64 => Poll::Ready(()),
					Poll::Pending => Poll::Pending,
				}),
			)
			.await;

			// A plain read returns it as it takes the bytes; a held read keeps it.
			read(&handle, &mut recv, 1000).await;
			assert_eq!(accepted.stats().credit.held, WINDOW as u64 - 1000);
			let taken = read_held(&handle, &mut recv, 500).await;
			assert_eq!(accepted.stats().credit.held, WINDOW as u64 - 1000);

			// Stopping returns the read-ahead; the application still owns what it took.
			recv.stop(0);
			assert_eq!(accepted.stats().credit.held, 500);
			drop(recv);
			assert_eq!(accepted.stats().credit.held, 500, "credit outlives its stream");
			drop(taken);
			assert_eq!(accepted.stats().credit.held, 0);
		})
		.expect("worker");
}
