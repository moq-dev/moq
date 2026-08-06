//! Embeddable MoQ relay for connecting publishers to subscribers.
//!
//! The relay is content-agnostic: it forwards live data without
//! interpreting it, so it works equally well for media, sensor telemetry,
//! or any other stream. Clustering, JWT authentication, WebSocket
//! fallback, and an HTTP API are all included.
//!
//! [`Relay::load`] assembles every piece from a [`Config`]; [`Relay::run`] drives
//! the stock loop, and embedders with their own workers or routes destructure a
//! `Relay` instead of reproducing the sequence. `main.rs` is a thin wrapper over
//! the two.

mod auth;
mod cache;
mod cluster;
mod config;
mod connection;
mod http_client;
mod internal;
mod listener;
mod nodes;
mod relay;
mod stats;
#[cfg(test)]
mod test_env;
mod web;
#[cfg(feature = "websocket")]
mod websocket;

/// The relay needs higher stream limits than the library default
/// to handle many concurrent subscriptions across connections.
pub const DEFAULT_MAX_STREAMS: u64 = 10_000;

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
pub use relay::*;
pub use stats::*;
pub use web::*;
