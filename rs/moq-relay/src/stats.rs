//! Relay-side stats configuration.
//!
//! The actual aggregator lives in [`moq_net::Stats`]; this module just
//! holds the relay-specific config knobs.

use clap::Args;
use serde::{Deserialize, Serialize};

/// Configuration for the relay's stats publishing.
///
/// Stats are disabled when `node` is unset. When configured, the relay attaches
/// a [`moq_net::Stats`] aggregator to every session it accepts (and every cluster
/// dial), which publishes `<prefix>/broadcasts/<level>/<node>` broadcasts on the
/// cluster origin. Each level only advertises while at least one role on that level
/// has an active subscription.
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "stats-config")]
pub struct StatsConfig {
	/// Top-level path under which stats broadcasts are published, e.g. `.stats`.
	/// Defaults to `.stats` at the cluster constructor when unset. Future stats
	/// categories (e.g. host-level node stats) will share the same prefix.
	#[arg(long = "stats-prefix", env = "MOQ_STATS_PREFIX")]
	pub prefix: Option<String>,

	/// Maximum segment depth stats are bucketed by.
	///
	/// `1` produces only the root bucket (`<prefix>/broadcasts/<node>`). `2` adds
	/// a per-first-segment bucket (e.g. `<prefix>/broadcasts/demo/<node>` for
	/// broadcasts under `demo/*`). Levels deeper than the broadcast path's
	/// segment count are skipped. `None` is treated as `1` at the cluster
	/// constructor.
	///
	/// `Option<u32>` rather than `u32` with a clap default so that
	/// `Config::load()`'s `update_from(args)` (which clap runs after merging TOML)
	/// doesn't overwrite a TOML-provided value when the flag is absent from the
	/// CLI.
	#[arg(long = "stats-levels", env = "MOQ_STATS_LEVELS")]
	pub levels: Option<u32>,

	/// Node identifier appended to advertised stats paths to disambiguate broadcasts
	/// when multiple relays share a cluster origin. Without this, peer relays would
	/// publish to the same `<prefix>/broadcasts/<level>` path and the origin's
	/// single-source delivery would drop all but one. Required to enable stats
	/// publishing — unset means stats are disabled.
	#[arg(long = "stats-node", env = "MOQ_STATS_NODE")]
	pub node: Option<String>,
}
