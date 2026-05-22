//! Relay-side stats configuration.
//!
//! The actual aggregator lives in [`moq_net::Stats`]; this module just
//! holds the relay-specific config knobs.

use clap::Args;
use serde::{Deserialize, Serialize};

/// Configuration for the relay's stats publishing.
///
/// Stats are disabled when `name` is unset. When configured, the relay attaches
/// a [`moq_net::Stats`] aggregator to every session it accepts (and every cluster
/// dial), which publishes `<level>/.stats/<name>[/<pop>]` broadcasts on the cluster
/// origin. Each level only advertises while at least one role on that level has an
/// active subscription.
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "stats-config")]
pub struct StatsConfig {
	/// Identifier baked into advertised stats paths (`<level>/.stats/<name>[/<pop>]`).
	/// Stats are disabled when unset.
	#[arg(long = "stats-name", env = "MOQ_STATS_NAME")]
	pub name: Option<String>,

	/// Maximum segment depth stats are bucketed by.
	///
	/// `0` disables stats entirely (no buckets). `1` produces the root bucket plus
	/// a per-first-segment bucket (e.g. `demo/.stats/<name>` for broadcasts under
	/// `demo/*`). `2` adds a per-second-segment bucket, and so on. Broadcasts deeper
	/// than `levels` are truncated. `None` is treated as `1` at the cluster constructor.
	///
	/// `Option<u32>` rather than `u32` with a clap default so that
	/// `Config::load()`'s `update_from(args)` (which clap runs after merging TOML)
	/// doesn't overwrite a TOML-provided value when the flag is absent from the
	/// CLI.
	#[arg(long = "stats-levels", env = "MOQ_STATS_LEVELS")]
	pub levels: Option<u32>,

	/// POP identifier appended to advertised stats paths to disambiguate broadcasts
	/// when multiple relays share a cluster origin. Without this, peer relays would
	/// publish to the same `<level>/.stats/<name>` path and the origin's
	/// single-source delivery would drop all but one. Required in any multi-relay
	/// deployment.
	#[arg(long = "stats-pop", env = "MOQ_STATS_POP")]
	pub pop: Option<String>,
}
