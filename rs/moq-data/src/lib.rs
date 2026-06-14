//! Helpers for sending metadata over [`moq-net`](https://docs.rs/moq-net) tracks.
//!
//! Each helper maps an application data structure onto a track, handling snapshots and deltas so a
//! late joiner can reconstruct the current state from the newest group alone.
//!
//! - [`set`] syncs a [`HashSet`](std::collections::HashSet)-like collection of arbitrary binary
//!   items, encoding changes as `+`/`-` deltas.
//! - [`json`] re-exports [`moq-json`](https://docs.rs/moq-json) for snapshot/delta JSON publishing.
//!   It lives in its own crate today and will migrate here over time.

/// Snapshot/delta JSON publishing, re-exported from [`moq-json`](https://docs.rs/moq-json).
#[cfg(feature = "json")]
pub use moq_json as json;

#[cfg(feature = "set")]
pub mod set;
