//! Tokio-based connection helpers for native MoQ applications.
//!
//! Establishes MoQ connections over:
//! - WebTransport (HTTP/3)
//! - Raw QUIC (with ALPN negotiation)
//! - WebSocket (qmux over WebSocket, the fallback when QUIC is blocked)
//! - TCP via the `tcp://` (plaintext) and `tls://` schemes (qmux)
//! - Unix domain socket via the `unix://` scheme (qmux, peer-credential aware; unix-only)
//! - A qmux session established elsewhere, such as over a WebRTC data channel
//!
//! Every transport is compiled in, and the URL scheme or config picks one at runtime.
//! [`client::Client`] connects to relays, [`server::Server`] accepts connections, and
//! [`mdns`] finds peers on the local network. [`quic::driver`] is an async QUIC stack
//! for any application, MoQ or not.
//!
//! Cryptography always comes from rind. TLS setup checks CPU support and returns
//! an error naming a missing CPU feature. Older x86_64 CPUs and aarch64 boards without
//! crypto extensions use rind's software paths; aarch64 requires NEON. Armv7 is supported.

#![warn(missing_docs)]

mod abort;
pub mod accept;
pub use moq_sock::bind;
pub mod cli;
pub mod client;
pub mod connect;
pub mod connection;
pub mod crypto;
mod error;
pub mod failover;
#[cfg(feature = "jemalloc")]
pub mod jemalloc;
pub mod listen;
mod log;
pub mod mdns;
pub mod origin;
pub mod panic;
pub mod quic;
mod resolve;
mod runtime;
pub mod secret;
pub mod server;
#[cfg(feature = "sim")]
pub mod sim;
pub mod tcp;
pub mod tls;
pub mod transport;
#[cfg(unix)]
pub mod unix;
pub mod worker;
// Resolving a `host:port` bind string is a QUIC-listener concern; the stream
// listeners take a `SocketAddr`/path straight from their config.
mod util;
pub mod watch;
pub mod websocket;

/// Session settings for every qmux transport moq-tokio establishes (WebSocket, `tcp://`, `tls://`, `unix://`),
/// with `quic`'s windows and stream count.
///
/// A qmux session leases receive credit as QUIC does, so it takes the same per-session
/// numbers rather than qmux's own defaults. moq-lite holds a stream open per subscription,
/// past qmux's default of 100, where a peer's opens would wait on credit forever.
fn qmux_config(quic: &quic::Resolved) -> qmux::Config {
	let mut config = qmux::Config::default();
	config.limits = quic.limits();
	config
}

/// [`qmux_config`] at the `[quic]` defaults, for a server's qmux sessions, which widen once
/// admitted (see [`server::Request::with_quic_limits`]).
fn qmux_default() -> qmux::Config {
	qmux_config(&quic::Resolved::default())
}

/// Run a qmux session's driver on the runtime, handing back the session.
fn spawn_qmux<T: qmux::transport::Transport + Send + 'static>(
	(session, driver): (qmux::Session, qmux::Driver<T>),
) -> qmux::Session {
	runtime::Spawner::current().spawn(driver);
	session
}

// Enumerated rather than globbed, so the root surface is a deliberate list and a
// new `pub` item in these modules doesn't silently join it.
pub use client::Client;
pub use connect::{Addrs, ConnectError};
pub use connection::{Backoff, Connection, Redirect, Restart, Status};
pub use error::{Error, Result};
pub use log::{Log, RedactedUrl};
pub use server::{Listener, Server};
pub use transport::Transport;
