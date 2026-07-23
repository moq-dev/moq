//! Embeddable MoQ relay for connecting publishers to subscribers.
//!
//! The relay is content-agnostic: it forwards live data without
//! interpreting it, so it works equally well for media, sensor telemetry,
//! or any other stream. Clustering, JWT authentication, WebSocket
//! fallback, and an HTTP API are all included.
//!
//! See `main.rs` for a complete example of how these pieces fit together.

mod auth;
mod cache;
mod cluster;
mod config;
mod connection;
mod http_client;
mod internal;
mod shutdown;
mod stats;
mod web;
#[cfg(feature = "websocket")]
mod websocket;

/// The relay needs higher stream limits than the library default
/// to handle many concurrent subscriptions across connections.
pub const DEFAULT_MAX_STREAMS: u64 = 10_000;

/// Default drain window in seconds for a GOAWAY without its own deadline: how
/// long an old session keeps serving after its replacement connects, before
/// being force-closed with [`moq_net::Error::GoawayTimeout`].
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 10;

/// Resolve an optional stats tier label. An absent or empty label selects the
/// default unprefixed tier.
fn configured_tier(label: Option<String>) -> moq_net::stats::Tier {
	label.map(moq_net::stats::Tier::new).unwrap_or_default()
}

pub use auth::*;
pub use cache::*;
pub use cluster::*;
pub use config::*;
pub use connection::*;
pub use internal::*;
pub use shutdown::*;
pub use stats::*;
pub use web::*;
