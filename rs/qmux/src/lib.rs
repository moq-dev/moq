//! QMux (draft-ietf-quic-qmux) over reliable transports.
//!
//! Runs QUIC streams, datagrams, and flow control over TCP, TLS, WebSocket, and Unix
//! sockets. Speaks draft-02 by default, negotiating down to draft-01 or draft-00. The
//! streams are QUIC's: [`moq_quic`]'s state machine drives them, with each record handed to
//! the transport counting as acknowledged.
//!
//! # Layout
//!
//! The crate root holds the transport-independent session API: [`Session`] and its
//! [`SendStream`] / [`RecvStream`] halves, which implement
//! [`moq_net::transport::poll`], the [`Driver`] that runs a session over the transport it
//! owns, plus the [`Config`], [`Version`], and [`Error`] types they share.
//!
//! Every constructor returns a `(Session, Driver)` pair. The driver is one future that reads
//! and writes the transport and runs the session's timers; spawn it on whatever executor
//! owns the socket. qmux spawns nothing itself and needs no tokio runtime.
//!
//! Everything tied to a specific transport lives in that transport's module:
//!
//! | Module | Entry point |
//! | --- | --- |
//! | [`tcp`] | [`tcp::connect`], [`tcp::accept`] |
//! | `uds` (Unix only) | `uds::connect`, `uds::accept` |
//! | [`tls`] | [`tls::Client`], [`tls::Server`] |
//! | [`ws`] | [`ws::Client`], [`ws::Server`], [`ws::Upgraded`] |
//! | [`transport`] | [`transport::Transport`] |
//!
//! For anything the built-in modules don't cover, wrap an
//! [`AsyncRead`](tokio::io::AsyncRead) + [`AsyncWrite`](tokio::io::AsyncWrite) byte stream
//! in `transport::Stream`, or implement [`transport::Transport`] yourself, and pass it to
//! [`Session::connect`] / [`Session::accept`].

// ALPN/subprotocol negotiation is only used by the TLS and WebSocket transports.
mod alpn;
mod config;
mod driver;
mod error;
mod protocol;
mod session;
mod socket;

/// Test support for the `fuzz/` harness: the target body and its seeds.
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

/// The transport abstraction, and the byte-stream implementation.
pub mod transport;

/// Plain TCP transport.
pub mod tcp;

/// Unix domain socket transport.
#[cfg(unix)]
pub mod uds;

/// TLS over TCP transport.
pub mod tls;

/// WebSocket transport.
pub mod ws;

pub use config::{Config, Protocol};
pub use driver::Driver;
pub use error::Error;
pub use error::WebSocketError;
pub use moq_quic::connection::mux::Version;
pub use moq_quic::{Limits, LimitsError};
pub use session::{RecvStream, SendGroup, SendStream, Session};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use socket::TcpStats;
pub use socket::{SharedSocketStats, SocketStats};

/// The largest frame draft-00 accepts, header included
const MAX_FRAME_SIZE: usize = 16384;
