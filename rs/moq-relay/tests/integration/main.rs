//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
//! `drills.rs` stays apart so each drill sensitivity mutation rebuilds only
//! the drills.
//! Relay fixtures opt out of process signals with `with_signals(false)`: only
//! `shutdown_signal` owns those signals, which reach every listener in this process.

mod auth_lifetime;
mod cluster_close;
mod cluster_drain;
mod cluster_idle;
mod cluster_unknown;
#[cfg(feature = "test-support")]
mod connection;
#[cfg(feature = "test-support")]
mod drain;
mod embed;
mod goaway_cluster;
mod hidden_cluster;
mod implementation;
mod lan_mesh;
mod logs;
mod memory;
mod peer_limits;
mod public_rules;
mod refusals;
mod released_cli;
mod runtime_uring;
mod runtime_workers;
mod session_revalidate;
mod shutdown_signal;
mod smoke;
mod stack;
mod upstream_links;
mod wire_stats;
