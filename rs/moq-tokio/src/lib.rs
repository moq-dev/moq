//! Tokio-based connection helpers for native MoQ applications.
//!
//! Establishes MoQ connections over:
//! - WebTransport (HTTP/3)
//! - Raw QUIC (with ALPN negotiation)
//! - WebSocket (fallback via [web-transport-ws](https://crates.io/crates/web-transport-ws))
//! - Plain TCP via the `tcp://` scheme (qmux, no TLS; requires `tcp` feature)
//! - Unix domain socket via the `unix://` scheme (qmux, peer-credential aware; requires `uds` feature, unix-only)
//! - Iroh P2P (requires `iroh` feature)
//!
//! See [`Client`] for connecting to relays and [`Server`] for accepting
//! connections. The `mdns` feature finds peers to connect to on the local network.
//!
//! With `default-features = false`, the `noq` backend must be paired
//! with the `aws-lc-rs` or `ring` crypto-provider feature. Every other subset
//! compiles, including no transport at all: such a build cannot connect to
//! anything, though a crypto provider on its own is still enough to configure TLS
//! and to build a plain-TLS listener's `rustls::ServerConfig` from
//! [`tls::Listen::server_config`].

#![warn(missing_docs)]

// The protocol crates need a compiled provider for reset and retry-token keys. A rustls provider
// installed at runtime cannot supply constructors removed by their compile-time feature gates.
#[cfg(all(feature = "noq", not(any(feature = "aws-lc-rs", feature = "ring"))))]
compile_error!("a rustls QUIC backend requires a crypto provider: enable either the `aws-lc-rs` or `ring` feature");

mod abort;
pub mod accept;
pub use moq_sock::bind;
pub mod cli;
pub mod client;
pub mod connect;
pub mod connection;
pub mod crypto;
mod error;
#[cfg(any(feature = "noq", feature = "tcp", feature = "websocket"))]
pub mod failover;
#[cfg(feature = "jemalloc")]
pub mod jemalloc;
pub mod listen;
mod log;
#[cfg(feature = "noq")]
pub mod noq;
pub mod origin;
pub mod quic;
#[cfg(any(feature = "noq", feature = "tcp", feature = "websocket"))]
mod resolve;
#[cfg(feature = "_transport")]
pub mod server;
#[doc(hidden)]
pub mod settings;
#[cfg(feature = "tcp")]
pub mod tcp;
pub mod tls;
pub mod transport;
#[cfg(all(feature = "uds", unix))]
pub mod unix;
pub mod worker;
// Resolving a `host:port` bind string is a QUIC-listener concern; the stream
// listeners take a `SocketAddr`/path straight from their config.
#[cfg(feature = "noq")]
mod util;
#[cfg(feature = "watch")]
pub mod watch;
#[cfg(feature = "websocket")]
pub mod websocket;

// Enumerated rather than globbed, so the root surface is a deliberate list and a
// new `pub` item in these modules doesn't silently join it.
pub use client::Client;
pub use connect::{Addrs, ConnectError};
pub use connection::{Backoff, Connection, Redirect, Status};
pub use error::{Error, Result};
pub use log::{Log, RedactedUrl};
#[cfg(feature = "_transport")]
pub use server::{Listener, Server};

// Re-export these crates.
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
