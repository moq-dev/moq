//! The TCP listener under [`Web`](crate::web::Web) and [`Internal`](crate::internal::Internal).
//!
//! `axum_server` owns the serve loop (TLS, HTTP version negotiation, WebSocket
//! upgrades, graceful shutdown), and its loop reacts to a failed `accept(2)` by
//! sleeping a flat 50ms and discarding the error, so nothing outside the process
//! ever learns that the relay stopped accepting TCP connections. That is not a side
//! channel here: `:443` carries WebSocket MoQ, WHIP/WHEP signalling, and the whole
//! live HLS origin.
//!
//! `axum_server`'s listener abstraction is the seam. Wrapping the socket in
//! [`Listener`] puts the `accept(2)` call back under our own policy
//! ([`moq_tokio::accept`]): a dead connection retries at once, an exhausted process
//! backs off and warns, and either way it is counted where an embedder can read it
//! back. Because the wrapper never yields an error upward, `axum_server`'s flat
//! sleep is unreachable.

mod idle;
pub(crate) use idle::Admission;

use std::{
	io,
	net::SocketAddr,
	pin::Pin,
	task::{Context, Poll},
};

use axum_server::{AddrListener, Address};
use moq_tokio::accept::Health;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

/// An accepted socket keeps its admission until HTTP classifies or upgrades it.
pub(crate) struct Accepted {
	stream: TcpStream,
	pub(crate) admission: Option<Admission>,
}

impl AsyncRead for Accepted {
	fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.stream).poll_read(cx, buf)
	}
}
impl AsyncWrite for Accepted {
	fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		Pin::new(&mut self.stream).poll_write(cx, buf)
	}
	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.stream).poll_flush(cx)
	}
	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.stream).poll_shutdown(cx)
	}
}
#[cfg(unix)]
impl std::os::fd::AsFd for Accepted {
	fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
		std::os::fd::AsFd::as_fd(&self.stream)
	}
}

/// The peer address of an accepted connection.
///
/// A newtype only because [`Address`] is already implemented for `SocketAddr` with
/// tokio's `TcpListener` nailed on as its listener type; this is what lets
/// [`Listener`] take that slot instead.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Peer(pub(crate) SocketAddr);

impl Address for Peer {
	type Stream = Accepted;
	type Listener = Listener;
}

/// Expose the accepted socket through Axum's standard extractor, including to
/// routes supplied by an embedder.
impl axum::extract::connect_info::Connected<Peer> for SocketAddr {
	fn connect_info(target: Peer) -> Self {
		target.0
	}
}

/// A TCP listener that classifies, counts, paces, and logs its own `accept(2)`
/// failures instead of leaving them to `axum_server`.
pub(crate) struct Listener {
	inner: TcpListener,
	health: Health,
	activity: Option<moq_net::server::Activity>,
}

impl Listener {
	/// Adopt an already-bound listener (from [`moq_tokio::bind::tcp`], which sets
	/// the dual-stack and keepalive options), reporting into `health`.
	pub(crate) fn new(
		listener: std::net::TcpListener,
		health: Health,
		activity: Option<moq_net::server::Activity>,
	) -> io::Result<Self> {
		Ok(Self {
			inner: TcpListener::from_std(listener)?,
			health,
			activity,
		})
	}
}

/// Policy shared by the public and internal HTTP listeners.
pub(crate) struct Config {
	pub health: Health,
	/// Count public web sockets from TCP accept through their first request.
	pub activity: Option<moq_net::server::Activity>,
	pub timeout: Option<std::time::Duration>,
}

/// An `axum_server` bound to [`Listener`] with `acceptor` installed, ready for
/// `.serve(..)`.
///
/// The acceptor is a parameter rather than the builder step `axum_server` offers,
/// so what a listener does at accept time is something each call site states rather
/// than something it can leave out.
///
/// The address type is spelled here rather than at each call site: `A::Listener`
/// doesn't pin `A` down, so inference cannot get there from the listener alone.
pub(crate) fn server<A>(
	listener: std::net::TcpListener,
	config: Config,
	acceptor: A,
) -> io::Result<axum_server::Server<Peer, idle::Acceptor<A>>> {
	let Config {
		health,
		timeout,
		activity,
	} = config;
	let mut server =
		axum_server::Server::from_listener(Listener::new(listener, health, activity)?).acceptor(idle::Acceptor {
			inner: acceptor,
			timeout,
		});
	server
		.http_builder()
		.http1()
		.timer(hyper_util::rt::TokioTimer::new())
		.header_read_timeout(timeout);
	let interval = timeout.unwrap_or(std::time::Duration::from_secs(30));
	server
		.http_builder()
		.http2()
		.timer(hyper_util::rt::TokioTimer::new())
		.keep_alive_interval(timeout)
		.keep_alive_timeout(interval);
	Ok(server)
}

impl AddrListener<Accepted, Peer> for Listener {
	/// Only reachable through `axum_server::Server::bind`, which nothing here uses:
	/// both listeners come pre-bound so their socket options are ours to set.
	async fn bind_to(addr: Peer) -> io::Result<Self> {
		Ok(Self {
			inner: TcpListener::bind(addr.0).await?,
			health: Health::new("tcp"),
			activity: None,
		})
	}

	/// Keep asking until a connection comes back.
	///
	/// Never returns an error: there is nothing above this worth telling, since
	/// `axum_server` would only sleep and drop it. The failure leaves through
	/// [`Health`] instead, which survives the episode that a log line during it may
	/// not.
	async fn accept_stream(&self) -> io::Result<(Accepted, Peer)> {
		loop {
			match self.inner.accept().await {
				Ok((stream, peer)) => {
					// WebSocket qmux already batches records, so Nagle would only hold a
					// batch's tail for the peer's delayed ACK. Only a reset socket refuses it.
					if let Err(err) = stream.set_nodelay(true) {
						tracing::debug!(%peer, %err, "dropped a connection that refused TCP_NODELAY");
						continue;
					}
					// Enter before returning the socket: axum_server spawns the TLS
					// acceptor, and waiting for that task would leave an uncounted gap.
					let admission = self
						.activity
						.as_ref()
						.and_then(|activity| activity.enter())
						.map(Admission::new);
					self.health.accepted();
					return Ok((Accepted { stream, admission }, Peer(peer)));
				}
				Err(err) => {
					if let Some(delay) = self.health.failed(&err) {
						kio::clock::sleep(delay).await;
					}
				}
			}
		}
	}

	fn get_local_addr(&self) -> io::Result<Peer> {
		self.inner.local_addr().map(Peer)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_tokio::accept::Failure;

	/// The wrapper has to survive a real accept, since everything above it is
	/// generic over `Address` and a mismatch only shows up at the serve call.
	#[tokio::test]
	async fn accepts_a_connection_and_reports_health() {
		let bound = moq_tokio::bind::tcp("127.0.0.1:0".parse().unwrap()).unwrap();
		let addr = bound.local_addr().unwrap();
		let health = Health::new("test");
		let listener = Listener::new(bound, health.clone(), None).unwrap();

		assert_eq!(listener.get_local_addr().unwrap().0, addr);

		let connect = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
		let (accepted, peer) = listener.accept_stream().await.unwrap();
		let client = connect.await.unwrap();

		assert_eq!(peer.0, client.local_addr().unwrap());
		assert!(accepted.stream.nodelay().unwrap());
		for &failure in Failure::ALL {
			assert_eq!(health.failures(failure), 0, "{failure} on a healthy accept");
		}
		assert_eq!(health.stalled(), None);
	}
}
