//! Helper library for native applications using MoQ.
//!
//! Makes it easy to establish MoQ connections over:
//! - WebTransport (via HTTP/3)
//! - QUIC (via ALPN)
//! - WebSocket (via [web-transport-ws](https://crates.io/crates/web-transport-ws))
//! - Iroh P2P (requires `iroh` feature)
//!
//! Includes optional logging and configuration.

pub mod client;
mod crypto;
pub mod log;
pub mod server;

pub use client::*;
pub use log::*;
pub use server::*;

// Re-export these crates.
pub use moq_lite;
pub use rustls;
pub use web_transport_quinn;

#[cfg(feature = "iroh")]
pub mod iroh;
