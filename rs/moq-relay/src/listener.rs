//! The TCP listener under [`Web`](crate::Web) and [`Internal`](crate::Internal).
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
//! ([`moq_native::accept`]): a dead connection retries at once, an exhausted process
//! backs off and warns, and either way it is counted where an embedder can read it
//! back. Because the wrapper never yields an error upward, `axum_server`'s flat
//! sleep is unreachable.

use std::io;
use std::net::SocketAddr;

use axum_server::{AddrListener, Address};
use moq_native::accept::Health;
use tokio::net::{TcpListener, TcpStream};

/// The peer address of an accepted connection.
///
/// A newtype only because [`Address`] is already implemented for `SocketAddr` with
/// tokio's `TcpListener` nailed on as its listener type; this is what lets
/// [`Listener`] take that slot instead.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Peer(pub(crate) SocketAddr);

impl Address for Peer {
	type Stream = TcpStream;
	type Listener = Listener;
}

/// A TCP listener that classifies, counts, paces, and logs its own `accept(2)`
/// failures instead of leaving them to `axum_server`.
pub(crate) struct Listener {
	inner: TcpListener,
	health: Health,
}

impl Listener {
	/// Adopt an already-bound listener (from [`moq_native::bind::tcp`], which sets
	/// the dual-stack and keepalive options), reporting into `health`.
	pub(crate) fn new(listener: std::net::TcpListener, health: Health) -> io::Result<Self> {
		Ok(Self {
			inner: TcpListener::from_std(listener)?,
			health,
		})
	}
}

/// An `axum_server` bound to [`Listener`], ready for `.acceptor(..)`/`.serve(..)`.
///
/// The address type is spelled here rather than at each call site: `A::Listener`
/// doesn't pin `A` down, so inference cannot get there from the listener alone.
pub(crate) fn server(listener: std::net::TcpListener, health: Health) -> io::Result<axum_server::Server<Peer>> {
	Ok(axum_server::Server::from_listener(Listener::new(listener, health)?))
}

impl AddrListener<TcpStream, Peer> for Listener {
	/// Only reachable through `axum_server::Server::bind`, which nothing here uses:
	/// both listeners come pre-bound so their socket options are ours to set.
	async fn bind_to(addr: Peer) -> io::Result<Self> {
		Ok(Self {
			inner: TcpListener::bind(addr.0).await?,
			health: Health::new("tcp"),
		})
	}

	/// Keep asking until a connection comes back.
	///
	/// Never returns an error: there is nothing above this worth telling, since
	/// `axum_server` would only sleep and drop it. The failure leaves through
	/// [`Health`] instead, which survives the episode that a log line during it may
	/// not.
	async fn accept_stream(&self) -> io::Result<(TcpStream, Peer)> {
		loop {
			match self.inner.accept().await {
				Ok((stream, peer)) => {
					self.health.accepted();
					return Ok((stream, Peer(peer)));
				}
				Err(err) => {
					if let Some(delay) = self.health.failed(&err) {
						tokio::time::sleep(delay).await;
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
	use moq_native::accept::Failure;

	/// The wrapper has to survive a real accept, since everything above it is
	/// generic over `Address` and a mismatch only shows up at the serve call.
	#[tokio::test]
	async fn accepts_a_connection_and_reports_health() {
		let bound = moq_native::bind::tcp("127.0.0.1:0".parse().unwrap()).unwrap();
		let addr = bound.local_addr().unwrap();
		let health = Health::new("test");
		let listener = Listener::new(bound, health.clone()).unwrap();

		assert_eq!(listener.get_local_addr().unwrap().0, addr);

		let connect = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
		let (_stream, peer) = listener.accept_stream().await.unwrap();
		let client = connect.await.unwrap();

		assert_eq!(peer.0, client.local_addr().unwrap());
		for failure in Failure::ALL {
			assert_eq!(health.failures(failure), 0, "{failure} on a healthy accept");
		}
		assert_eq!(health.stalled(), None);
	}
}
