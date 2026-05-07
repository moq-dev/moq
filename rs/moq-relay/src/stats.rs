//! Relay-side stats configuration.
//!
//! The actual aggregator lives in [`moq_lite::Stats`]; this module just
//! holds the relay-specific config knobs.

use clap::Args;
use serde::{Deserialize, Serialize};

/// Configuration for the relay's stats publishing.
///
/// Stats are disabled when `name` is unset. When configured, the relay attaches
/// a [`moq_lite::Stats`] aggregator to every session it accepts (and every cluster
/// dial), which publishes `.stats/<level>/<name>` broadcasts on the cluster origin.
/// Each level only advertises while at least one role on that level has an active
/// subscription.
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "stats-config")]
pub struct StatsConfig {
	/// Identifier baked into advertised stats paths (`.stats/<level>/<name>`).
	/// Stats are disabled when unset.
	#[arg(long = "stats-name", env = "MOQ_STATS_NAME")]
	pub name: Option<String>,

	/// How many path-prefix levels to bucket stats by.
	///
	/// `1` produces only the root bucket (`.stats/<name>`). `2` adds a per-first-segment
	/// bucket (e.g. `.stats/demo/<name>` for broadcasts under `demo/*`). Levels deeper
	/// than the broadcast path's segment count are skipped.
	#[arg(long = "stats-levels", env = "MOQ_STATS_LEVELS", default_value_t = 1)]
	pub levels: u32,
}
