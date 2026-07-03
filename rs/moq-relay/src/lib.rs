//! Embeddable MoQ relay for connecting publishers to subscribers.
//!
//! The relay is content-agnostic: it forwards live data without
//! interpreting it, so it works equally well for media, sensor telemetry,
//! or any other stream. Clustering, JWT authentication, WebSocket
//! fallback, and an HTTP API are all included.
//!
//! See `main.rs` for a complete example of how these pieces fit together.

mod auth;
mod cluster;
mod config;
mod connection;
mod http_client;
mod stats;
mod web;
#[cfg(feature = "websocket")]
mod websocket;

/// The relay needs higher stream limits than the library default
/// to handle many concurrent subscriptions across connections.
pub const DEFAULT_MAX_STREAMS: u64 = 10_000;

/// Default billing tier for trusted (non-JWT/public) connections when no label
/// is configured, shared by the `--cluster-tier` / `--auth-mtls-tier` defaults.
const DEFAULT_TRUSTED_TIER: &str = "internal";

/// Resolve a configured tier label to a [`moq_net::Tier`], defaulting to
/// [`DEFAULT_TRUSTED_TIER`] when unset. An empty label selects the default
/// (unprefixed) tier.
fn trusted_tier(label: Option<String>) -> moq_net::Tier {
	moq_net::Tier::new(label.unwrap_or_else(|| DEFAULT_TRUSTED_TIER.to_string()))
}

pub use auth::*;
pub use cluster::*;
pub use config::*;
pub use connection::*;
pub use stats::*;
pub use web::*;
