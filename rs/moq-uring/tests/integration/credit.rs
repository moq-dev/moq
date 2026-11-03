//! Credit through the io_uring driver: leases keep the connection window spent
//! until consumed, a stuck window evicts the lowest-priority stream, a chunked
//! write takes no more from its buffer than the peer's window admits, and a tiny
//! frame keeps no more memory than its own bytes.
//!
//! Unsupported local kernels skip loudly; CI_STRICT makes CI refuse them.

#![cfg(feature = "quic")]

use crate::support;

use std::net::UdpSocket;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::{Buf, Bytes};

use moq_uring::{Config, Error, Worker, quic, udp};

const ALPN: &str = "moq-uring-credit";
/// The server's connection window.
const WINDOW: usize = 32 * 1024;

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
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

/// Lease at least `want` bytes, returning how many were leased and whether
/// the FIN follows them.
async fn leased(handle: &moq_uring::Handle, recv: &mut quic::recv::Stream, want: usize) -> (usize, bool) {
	within(
		handle,
		"stream data",
		std::future::poll_fn(|cx| {
			recv.poll_lease(cx, want)
				.map_ok(|lease| (lease.remaining(), lease.is_fin()))
		}),
	)
	.await
	.expect("lease")
}

/// Consume the first `n` leased bytes.
async fn consume(handle: &moq_uring::Handle, recv: &mut quic::recv::Stream, n: usize) {
	within(
		handle,
		"stream data",
		std::future::poll_fn(|cx| {
			recv.poll_lease(cx, n).map_ok(|mut lease| {
				lease.advance(n);
				lease.release();
			})
		}),
	)
	.await
	.expect("lease")
}

/// A server endpoint with `window` of connection credit and `stream_window`
/// per stream, and a dial-only client to reach it.
fn endpoints(
	handle: &moq_uring::Handle,
	window: usize,
	stream_window: usize,
) -> (quic::Endpoint, quic::Endpoint, quic::client::Config) {
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
	server_config.transport.limits.receive_window = std::num::NonZeroU64::new(window as u64);
	server_config.transport.limits.stream_receive_window = std::num::NonZeroU64::new(stream_window as u64);
	let server = quic::Endpoint::new(
		socket(handle),
		quic::endpoint::Config::default().with_server(server_config),
	)
	.expect("server endpoint");
	let client = quic::Endpoint::new(socket(handle), quic::endpoint::Config::default()).expect("dial-only endpoint");
	let mut dial = quic::client::Config::default();
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;
	(server, client, dial)
}

/// Write `len` bytes and the FIN, giving up quietly once the receiver stops.
fn write(handle: &moq_uring::Handle, mut send: quic::send::Stream, len: usize) {
	handle.spawn(async move {
		if send.write_all(Bytes::from(vec![7u8; len])).await.is_ok() {
			let _ = send.finish().stopped().await;
		}
	});
}

/// Poll `future` once, without a waker.
fn now<F: Future>(future: F) -> Poll<F::Output> {
	std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop()))
}

#[test]
fn leases_spend_the_connection_window() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	// Far smaller than the window: leases must keep replenishing it.
	let (server, client, dial) = endpoints(&handle, WINDOW, 4096);

	worker
		.block_on(async move {
			let dialed = client
				.connect(&dial, server.local_addr(), "localhost")
				.await
				.expect("dial");
			let accepted = server.accept().await.expect("accept");
			let send = dialed.open_uni(Default::default()).await.expect("open_uni");
			write(&handle, send, WINDOW + 500);
			let mut recv = within(&handle, "the stream", accepted.accept_uni())
				.await
				.expect("accept_uni");

			// A lease returns nothing, so the peer stops at the window.
			assert_eq!(leased(&handle, &mut recv, WINDOW).await, (WINDOW, false));
			let mut cx = Context::from_waker(Waker::noop());
			assert!(matches!(
				recv.poll_lease(&mut cx, WINDOW + 1).map_ok(|lease| lease.remaining()),
				Poll::Ready(Err(quic::recv::Error::TooLarge))
			));

			// Far below the usual update threshold, and still enough to unblock.
			consume(&handle, &mut recv, 100).await;
			assert_eq!(leased(&handle, &mut recv, WINDOW).await, (WINDOW, false));

			consume(&handle, &mut recv, WINDOW).await;
			assert_eq!(leased(&handle, &mut recv, 401).await, (400, true));
		})
		.expect("worker");
}

#[test]
fn stuck_window_evicts_the_lower_priority() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	// Short of the window, so the first stream cannot keep the second from arriving.
	let (server, client, dial) = endpoints(&handle, WINDOW, WINDOW / 4 * 3);

	worker
		.block_on(async move {
			let dialed = client
				.connect(&dial, server.local_addr(), "localhost")
				.await
				.expect("dial");
			let accepted = server.accept().await.expect("accept");
			for _ in 0..2 {
				let send = dialed.open_uni(Default::default()).await.expect("open_uni");
				write(&handle, send, WINDOW);
			}
			let mut streams = Vec::new();
			for _ in 0..2 {
				let accept = accepted.accept_uni();
				streams.push(within(&handle, "the stream", accept).await.expect("accept_uni"));
			}
			let [mut low, mut high] = streams.try_into().expect("two streams");
			low.set_priority(-1);
			high.set_priority(1);

			// Each waits for a whole window, which only one of them can have.
			let mut low_done = None;
			let mut high_done = None;
			within(
				&handle,
				"an eviction",
				std::future::poll_fn(|cx| {
					if low_done.is_none()
						&& let Poll::Ready(res) = low.poll_lease(cx, WINDOW).map_ok(|lease| lease.remaining())
					{
						low_done = Some(res);
					}
					if high_done.is_none()
						&& let Poll::Ready(res) = high.poll_lease(cx, WINDOW).map_ok(|lease| lease.remaining())
					{
						high_done = Some(res);
					}
					match low_done.is_some() && high_done.is_some() {
						true => Poll::Ready(()),
						false => Poll::Pending,
					}
				}),
			)
			.await;
			assert!(
				matches!(low_done, Some(Err(quic::recv::Error::Evicted))),
				"{low_done:?}"
			);
			assert_eq!(high_done.expect("polled").expect("lease"), WINDOW);
		})
		.expect("worker");
}

#[test]
fn chunked_writes_stop_at_the_window() {
	const STREAM_WINDOW: usize = 4096;

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
	server_config.transport.limits.stream_receive_window = std::num::NonZeroU64::new(STREAM_WINDOW as u64);
	let server = quic::Endpoint::new(
		socket(&handle),
		quic::endpoint::Config::default().with_server(server_config),
	)
	.expect("server endpoint");
	let client = quic::Endpoint::new(socket(&handle), quic::endpoint::Config::default()).expect("dial-only endpoint");
	let mut dial = quic::client::Config::default();
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;

	let payload: Bytes = (0..3 * STREAM_WINDOW).map(|i| (i * 31 % 251) as u8).collect();

	worker
		.block_on(async move {
			let dialed = client
				.connect(&dial, server.local_addr(), "localhost")
				.await
				.expect("dial");
			let accepted = server.accept().await.expect("accept");

			let mut send = dialed.open_uni(Default::default()).await.expect("open_uni");

			// The bytes past the window stay in the caller's buffer, untaken,
			// and moq-quic keeps the ones it took by reference, not as a copy.
			let mut buf = Bytes::copy_from_slice(&payload);
			assert!(buf.is_unique());
			assert!(matches!(now(send.write(&mut buf)), Poll::Ready(Ok(STREAM_WINDOW))));
			assert_eq!(buf, payload.slice(STREAM_WINDOW..));
			assert!(!buf.is_unique(), "moq-quic copied the chunk");
			assert!(now(send.write(&mut buf)).is_pending());
			assert_eq!(buf, payload.slice(STREAM_WINDOW..));

			handle.spawn(async move {
				send.write_all(buf).await.expect("write");
				let _ = send.finish().stopped().await;
			});

			let mut recv = within(&handle, "the stream", accepted.accept_uni())
				.await
				.expect("accept_uni");

			// Straight out of moq-quic, capped at `max`.
			let first = within(&handle, "stream data", recv.read_chunk(1000))
				.await
				.expect("read")
				.expect("not finished");
			assert!(!first.is_empty() && first.len() <= 1000);

			let mut received = first.to_vec();
			while let Some(chunk) = within(&handle, "stream data", recv.read_chunk(usize::MAX))
				.await
				.expect("read")
			{
				received.extend_from_slice(&chunk);
			}
			assert_eq!(received, payload);
			let reset = within(&handle, "the FIN", recv.received_reset()).await;
			assert!(matches!(reset, Ok(None)), "{reset:?}");
		})
		.expect("worker");
}

/// A tiny datagram, and a tiny stream frame taken from a lease, each reach the
/// application in an allocation of their own rather than as a slice of the
/// receive completion they arrived in, which their credit does not pay for.
#[test]
fn tiny_frames_do_not_pin_receive_buffers() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let (server, client, dial) = endpoints(&handle, WINDOW, WINDOW);

	worker
		.block_on(async move {
			let dialed = client
				.connect(&dial, server.local_addr(), "localhost")
				.await
				.expect("dial");
			let accepted = server.accept().await.expect("accept");

			dialed
				.send_datagram(Bytes::from_static(b"x"), Default::default())
				.expect("send_datagram");
			let datagram = within(&handle, "the datagram", accepted.read_datagram())
				.await
				.expect("recv_datagram");
			assert!(owns_exactly(datagram));

			let send = dialed.open_uni(Default::default()).await.expect("open_uni");
			write(&handle, send, 1);
			let mut recv = within(&handle, "the stream", accepted.accept_uni())
				.await
				.expect("accept_uni");
			let taken = within(
				&handle,
				"stream data",
				std::future::poll_fn(|cx| {
					recv.poll_lease(cx, 1).map_ok(|mut lease| {
						let len = lease.chunk().len();
						lease.copy_to_bytes(len)
					})
				}),
			)
			.await
			.expect("lease");
			assert_eq!(taken, &[7][..]);
			assert!(owns_exactly(taken));
		})
		.expect("worker");
}

/// Whether `bytes` is the only handle on an allocation of exactly its own size.
fn owns_exactly(bytes: Bytes) -> bool {
	let len = bytes.len();
	bytes.try_into_mut().is_ok_and(|bytes| bytes.capacity() == len)
}

/// A held lease borrows nothing from the connection, so the driver and the
/// other streams keep moving while it is out.
#[test]
fn a_held_lease_does_not_stall_the_connection() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let (server, client, dial) = endpoints(&handle, WINDOW, WINDOW);

	worker
		.block_on(async move {
			let dialed = client
				.connect(&dial, server.local_addr(), "localhost")
				.await
				.expect("dial");
			let accepted = server.accept().await.expect("accept");

			write(
				&handle,
				dialed.open_uni(Default::default()).await.expect("open_uni"),
				100,
			);
			let mut held = within(&handle, "the first stream", accepted.accept_uni())
				.await
				.expect("accept_uni");
			let lease = within(&handle, "stream data", held.lease(100)).await.expect("lease");

			write(
				&handle,
				dialed.open_uni(Default::default()).await.expect("open_uni"),
				WINDOW / 2,
			);
			let mut other = within(&handle, "the second stream", accepted.accept_uni())
				.await
				.expect("accept_uni");
			let read = within(&handle, "the second stream's data", other.read_to_end(WINDOW))
				.await
				.expect("read");
			assert_eq!(read.len(), WINDOW / 2);

			assert_eq!(lease.remaining(), 100);
			lease.release();
		})
		.expect("worker");
}
