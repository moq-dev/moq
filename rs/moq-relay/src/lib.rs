//! Embeddable MoQ relay for connecting publishers to subscribers.
//!
//! The relay is content-agnostic: it forwards live data without
//! interpreting it, so it works equally well for media, sensor telemetry,
//! or any other stream. Clustering, JWT authentication, WebSocket
//! fallback, and an HTTP API are all included.
//!
//! [`Relay::load`] assembles every piece from a [`Config`]. Embedders clone the
//! application handles they need, mount extra routes, and call [`Relay::run`],
//! which keeps the listeners, workers, and shutdown joins. `main.rs` is a thin
//! wrapper over the two.

pub mod auth;
pub mod cache;
pub mod cluster;
mod config;
mod connection;
mod duration;
mod http_client;
pub mod internal;
mod listener;
mod nodes;
mod relay;
pub mod runtime;
pub mod session;
mod settings;
pub mod shutdown;
pub mod stats;
#[cfg(test)]
mod test_env;
#[cfg(feature = "test-support")]
mod test_support;
#[cfg(all(target_os = "linux", feature = "_uring"))]
pub mod uring;
pub mod web;
#[cfg(feature = "websocket")]
mod websocket;

/// The shared default stream limit for MoQ clients and relays.
pub use moq_tokio::quic::DEFAULT_MAX_STREAMS;

/// Default drain window for a shutdown GOAWAY: how long an accepted session may
/// keep running after being told to leave, before being force-closed with
/// [`moq_net::Error::GoawayTimeout`].
pub const DEFAULT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Resolve an optional stats tier label. An absent or empty label selects the
/// default unprefixed tier.
fn configured_tier(label: Option<String>) -> moq_net::stats::Tier {
	label.map(moq_net::stats::Tier::new).unwrap_or_default()
}

pub use config::*;
pub use connection::*;
pub use relay::*;
#[cfg(feature = "test-support")]
pub use test_support::*;

/// The relay settings registry for composing a binary's CLI and TOML merge.
pub fn settings() -> usage::config::Registry {
	settings::Settings::SETTINGS_REGISTRY
}
