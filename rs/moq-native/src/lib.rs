//! Helper library for native MoQ applications.
//!
//! Establishes MoQ connections over:
//! - WebTransport (HTTP/3)
//! - Raw QUIC (with ALPN negotiation)
//! - WebSocket (fallback via [web-transport-ws](https://crates.io/crates/web-transport-ws))
//! - Iroh P2P (requires `iroh` feature)
//!
//! See [`Client`] for connecting to relays and [`Server`] for accepting connections.

mod client;
mod crypto;
mod log;
mod server;

pub use client::*;
pub use log::*;
pub use server::*;

// Re-export these crates.
pub use moq_lite;
pub use rustls;
pub use web_transport_quinn;

#[cfg(feature = "iroh")]
mod iroh;
#[cfg(feature = "iroh")]
pub use iroh::*;
