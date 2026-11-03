//! WebTransport is a protocol for client-server communication over QUIC.
//! It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.
//!
//! WebTransport is layered on top of HTTP/3 which is then layered on top of QUIC.
//! This library hides that detail and tries to expose only the QUIC API, delegating as much as
//! possible to the underlying implementation. See [`crate::quic::driver`] for more documentation.
//!
//! QUIC provides two primary APIs:
//!
//! # Streams
//! QUIC streams are ordered, reliable, flow-controlled, and optionally bidirectional.
//! Both endpoints can create and close streams (including an error code) with no overhead.
//! You can think of them as TCP connections, but shared over a single QUIC connection.
//!
//! # Datagrams
//! QUIC datagrams are unordered, unreliable, and not flow-controlled.
//! Both endpoints can send datagrams below the MTU size (~1.2kb minimum) and they might arrive out
//! of order or not at all. They are basically UDP packets, except they are encrypted and congestion
//! controlled.
//!
//! # Limitations
//! WebTransport is able to be pooled with HTTP/3 and multiple WebTransport sessions.
//! This crate avoids that complexity, doing the bare minimum to support a single WebTransport
//! session that owns the entire QUIC connection. If you want to support HTTP/3 on the same
//! host/port, you should use another crate (ex. `h3-webtransport`). If you want to support multiple
//! WebTransport sessions over the same QUIC connection... you should just dial a new QUIC
//! connection instead.

// External
mod error;
mod recv;
mod send;
mod server;
mod session;

pub use error::*;
pub use recv::*;
pub use send::*;
pub use server::*;
pub use session::*;

// Internal
mod connect;
mod fan;
mod settings;

use connect::*;
use settings::*;

// Required to access proto::ConnectError wrapped in ClientError
pub use connect::ConnectError;

/// The HTTP/3 ALPN is required when negotiating a QUIC connection.
pub const ALPN: &str = "h3";

/// Re-export the http crate because it's in the public API.
pub use http;

/// Re-export the WebTransport protocol implementation.
pub use web_transport_proto as proto;
