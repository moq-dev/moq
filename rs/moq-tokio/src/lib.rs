//! Tokio-based connection helpers for native MoQ applications.
//!
//! Establishes MoQ connections over:
//! - WebTransport (HTTP/3)
//! - Raw QUIC (with ALPN negotiation)
//! - WebSocket (fallback via [web-transport-ws](https://crates.io/crates/web-transport-ws))
//! - TCP via the `tcp://` (plaintext) and `tls://` schemes (qmux; requires `tcp` feature)
//! - Unix domain socket via the `unix://` scheme (qmux, peer-credential aware; requires `uds` feature, unix-only)
//! - Iroh P2P (requires `iroh` feature)
//! - A WebRTC data channel with a browser peer, signaled by the caller (qmux; requires `webrtc` feature)
//!
//! See [`Client`] for connecting to relays and [`Server`] for accepting
//! connections. The `mdns` feature finds peers to connect to on the local network.
//!
//! With `default-features = false`, the `quic` backend must be paired
//! with the `aws-lc-rs` or `ring` crypto-provider feature. Every other subset
//! compiles, including no transport at all: such a build cannot connect to
//! anything, though a crypto provider on its own is still enough to configure TLS
//! and to build a plain-TLS listener's `rustls::ServerConfig` from
//! [`tls::Listen::server_config`].
//!
//! MoQ sessions need the `net` feature, which every transport turns on. Without
//! it, `driver` and a crypto provider build [`quic::driver`] alone: an async
//! QUIC stack for any application, MoQ or not.

#![warn(missing_docs)]

// The protocol crates need a compiled provider for reset and retry-token keys. A rustls provider
// installed at runtime cannot supply constructors removed by their compile-time feature gates.
#[cfg(all(feature = "driver", not(any(feature = "aws-lc-rs", feature = "ring"))))]
compile_error!("a rustls QUIC backend requires a crypto provider: enable either the `aws-lc-rs` or `ring` feature");

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
pub mod secret;
#[cfg(feature = "_transport")]
pub mod server;
#[doc(hidden)]
pub mod settings;
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
#[cfg(feature = "webrtc")]
#[doc(hidden)]
pub mod webrtc;
#[cfg(feature = "websocket")]
pub mod websocket;

/// Session settings for every qmux transport (WebSocket, `tcp://`, `tls://`, `unix://`, WebRTC).
///
/// moq-lite holds a stream open per subscription, so a qmux session grants the same
/// stream count as QUIC rather than qmux's default of 100, past which a peer's opens
/// wait on credit forever.
#[cfg(any(feature = "websocket", feature = "tcp", feature = "webrtc"))]
fn qmux_config() -> qmux::Config {
	let mut config = qmux::Config::default();
	config.limits.max_streams = std::num::NonZeroU64::new(quic::DEFAULT_MAX_STREAMS);
	config
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

// Re-export these crates.
#[cfg(feature = "net")]
pub use moq_net;
pub use rustls;

/// Re-exported because [`watch::Files`] surfaces `notify::Result`/`notify::Error`
/// in its API; a major `notify` bump is therefore a breaking change for this crate.
#[cfg(feature = "watch")]
pub use notify;

/// Re-exported because [`tls::init_android`] takes a `jni::Env` handle; a major
/// `jni` bump is therefore a breaking change for this crate.
#[cfg(target_os = "android")]
pub use jni;

#[cfg(feature = "iroh")]
pub mod iroh;

#[cfg(feature = "mdns")]
pub mod mdns;

/// Whether this build can capture qlog traces, which the `qlog` feature gates.
///
/// Setting a qlog directory without it is an error at dial time, so a caller offering
/// the knob should check here rather than surfacing an option that cannot work.
pub fn qlog_supported() -> bool {
	cfg!(feature = "qlog")
}
