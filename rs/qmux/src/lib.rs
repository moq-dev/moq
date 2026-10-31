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
//! [`moq_net::transport::poll`], plus the [`Config`], [`Version`], and [`Error`] types they
//! share.
//!
//! Everything tied to a specific transport lives in that transport's module:
//!
//! | Module | Entry point | Feature |
//! | --- | --- | --- |
//! | `tcp` | `tcp::connect`, `tcp::accept` | `tcp` |
//! | `uds` | `uds::connect`, `uds::accept` | `uds` (Unix only) |
//! | `tls` | `tls::Client`, `tls::Server` | `tls` |
//! | `ws` | `ws::Client`, `ws::Server`, `ws::Upgraded` | `ws` |
//! | [`transport`] | [`transport::Transport`] | |
//!
//! For anything the built-in modules don't cover, wrap an
//! [`AsyncRead`](tokio::io::AsyncRead) + [`AsyncWrite`](tokio::io::AsyncWrite) byte stream
//! in `transport::Stream`, or implement [`transport::Transport`] yourself, and pass it to
//! [`Session::connect`] / [`Session::accept`].

// ALPN/subprotocol negotiation is only used by the TLS and WebSocket transports.
#[cfg(any(feature = "tls", feature = "ws"))]
mod alpn;
mod config;
mod error;
mod protocol;
mod session;
mod socket;

/// The transport abstraction, and the byte-stream implementation.
pub mod transport;

/// Plain TCP transport.
#[cfg(feature = "tcp")]
pub mod tcp;

/// Unix domain socket transport.
#[cfg(all(unix, feature = "uds"))]
pub mod uds;

/// TLS over TCP transport.
#[cfg(feature = "tls")]
pub mod tls;

/// WebSocket transport.
#[cfg(feature = "ws")]
pub mod ws;

pub use config::{Config, Protocol};
pub use error::Error;
pub use moq_quic::mux::Version;
pub use moq_quic::{Limits, LimitsError};
pub use session::{RecvStream, SendGroup, SendStream, Session};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use socket::TcpStats;
pub use socket::{SharedSocketStats, SocketStats};

/// The largest frame draft-00 accepts, header included
#[cfg(any(feature = "tcp", all(unix, feature = "uds"), feature = "ws"))]
const MAX_FRAME_SIZE: usize = 16384;
