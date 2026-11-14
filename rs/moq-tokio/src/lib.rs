//! Tokio-based connection helpers for native MoQ applications.
//!
//! Establishes MoQ connections over:
//! - WebTransport (HTTP/3)
//! - Raw QUIC (with ALPN negotiation)
//! - WebSocket (fallback via [web-transport-ws](https://crates.io/crates/web-transport-ws))
//! - TCP via the `tcp://` (plaintext) and `tls://` schemes (qmux; requires `tcp` feature)
//! - Unix domain socket via the `unix://` scheme (qmux, peer-credential aware; requires `uds` feature, unix-only)
//! - A qmux session established elsewhere, such as over a WebRTC data channel (requires `qmux` feature)
//!
//! With `net`, `client::Client` connects to relays; transport features enable
//! `server::Server` for accepting connections. The `mdns` feature finds peers on the local network.
//!
//! Cryptography always comes from rind. TLS setup checks CPU support and returns
//! an error naming a missing feature. Older x86_64 CPUs and aarch64 boards without
//! crypto extensions use rind's software paths; aarch64 requires NEON. Armv7 is supported.
//!
//! MoQ sessions need the `net` feature, which every transport turns on. Without
//! it, `driver` builds [`quic::driver`] alone: an async
//! QUIC stack for any application, MoQ or not.

#![warn(missing_docs)]

#[cfg(any(feature = "net", all(feature = "watch", feature = "_certs")))]
mod abort;
pub mod accept;
pub use moq_sock::bind;
pub mod cli;
#[cfg(feature = "net")]
pub mod client;
#[cfg(feature = "net")]
pub mod connect;
#[cfg(feature = "net")]
pub mod connection;
pub mod crypto;
mod error;
#[cfg(any(feature = "quic", feature = "tcp", feature = "websocket"))]
pub mod failover;
#[cfg(feature = "jemalloc")]
pub mod jemalloc;
#[cfg(feature = "net")]
pub mod listen;
mod log;
#[cfg(feature = "net")]
pub mod origin;
pub mod panic;
pub mod quic;
#[cfg(any(feature = "quic", feature = "tcp", feature = "websocket"))]
mod resolve;
#[cfg(any(
	feature = "net",
	feature = "driver",
	feature = "jemalloc",
	feature = "mdns",
	all(feature = "watch", feature = "_certs")
))]
mod runtime;
pub mod secret;
#[cfg(feature = "_transport")]
pub mod server;
pub mod settings;
#[cfg(all(feature = "sim", feature = "net"))]
pub mod sim;
#[cfg(feature = "net")]
pub mod socket;
#[cfg(feature = "tcp")]
pub mod tcp;
pub mod tls;
#[cfg(feature = "net")]
pub mod transport;
#[cfg(all(feature = "uds", unix))]
pub mod unix;
pub mod worker;
// Resolving a `host:port` bind string is a QUIC-listener concern; the stream
// listeners take a `SocketAddr`/path straight from their config.
#[cfg(feature = "quic")]
mod util;
#[cfg(feature = "watch")]
pub mod watch;
#[cfg(feature = "websocket")]
pub mod websocket;

/// Session settings for every qmux transport moq-tokio establishes (WebSocket, `tcp://`, `tls://`, `unix://`),
/// with `quic`'s windows and stream count.
///
/// A qmux session leases receive credit as QUIC does, so it takes the same per-session
/// numbers rather than qmux's own defaults. moq-lite holds a stream open per subscription,
/// past qmux's default of 100, where a peer's opens would wait on credit forever.
#[cfg(any(feature = "websocket", feature = "tcp"))]
fn qmux_config(quic: &quic::Resolved) -> qmux::Config {
	let mut config = qmux::Config::default();
	config.limits = quic.limits();
	config
}

/// [`qmux_config`] at the `[quic]` defaults, for a server's qmux sessions, which widen once
/// admitted (see [`server::Request::with_quic_limits`]).
#[cfg(any(feature = "websocket", feature = "tcp"))]
fn qmux_default() -> qmux::Config {
	qmux_config(&quic::Resolved::default())
}

/// Run a qmux session's driver on the runtime, handing back the session.
#[cfg(any(feature = "websocket", feature = "tcp"))]
fn spawn_qmux<T: qmux::transport::Transport + Send + 'static>(
	(session, driver): (qmux::Session, qmux::Driver<T>),
) -> qmux::Session {
	runtime::spawn(driver);
	session
}

// Enumerated rather than globbed, so the root surface is a deliberate list and a
// new `pub` item in these modules doesn't silently join it.
#[cfg(feature = "net")]
pub use client::Client;
#[cfg(feature = "net")]
pub use connect::{Addrs, ConnectError};
#[cfg(feature = "net")]
pub use connection::{Backoff, Connection, Redirect, Restart, Status};
pub use error::{Error, Result};
pub use log::{Log, RedactedUrl};
#[cfg(feature = "_transport")]
pub use server::{Listener, Server};
#[cfg(feature = "net")]
pub use transport::Transport;

#[cfg(feature = "mdns")]
pub mod mdns;

/// Whether this build can capture qlog traces, which the `qlog` feature gates.
///
/// Setting a qlog directory without it is an error at dial time, so a caller offering
/// the knob should check here rather than surfacing an option that cannot work.
pub fn qlog_supported() -> bool {
	cfg!(feature = "qlog")
}
