//! Relay-side stats configuration.
//!
//! The actual aggregator lives in [`moq_net::Stats`]; this module just
//! holds the relay-specific config knobs.

use clap::Args;
use moq_net::{OriginProducer, PathOwned, Stats};
use serde::{Deserialize, Serialize};

/// Configuration for the relay's stats publishing.
///
/// Set `enabled = true` to attach a [`moq_net::Stats`] aggregator to every
/// session the relay accepts (and every cluster dial). The aggregator
/// publishes `<prefix>/prefix/<level-path>/<node>` broadcasts on the cluster
/// origin, with `<node>` omitted when [`Self::node`] is unset. Each level only
/// advertises while at least one role on that level has an active
/// subscription.
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "stats-config")]
pub struct StatsConfig {
	/// Master switch for stats publishing. Defaults to false.
	///
	/// Typed as `Option<bool>` (not bare `bool`) so a TOML file setting
	/// `stats.enabled = true` survives `Config::load`'s `update_from` CLI
	/// re-parse. With a bare `bool`, an absent `--stats-enabled` CLI flag
	/// writes the `Default::default()` value (`false`) over the TOML value.
	/// See `tests::cli_does_not_clobber_toml_stats_enabled` and the
	/// "Config flags + TOML merge" note in `CLAUDE.md`.
	#[arg(
		long = "stats-enabled",
		env = "MOQ_STATS_ENABLED",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub enabled: Option<bool>,

	/// Top-level path under which stats broadcasts are published. Defaults
	/// to `.stats`. Future stats categories (e.g. host-level node stats)
	/// will share the same prefix.
	#[arg(long = "stats-prefix", env = "MOQ_STATS_PREFIX")]
	pub prefix: Option<String>,

	/// Maximum segment depth stats are bucketed by, capping the number of
	/// aggregation buckets the relay produces per broadcast.
	///
	/// `1` produces only the root bucket (`<prefix>/prefix/<node>`). `2`
	/// adds a per-first-segment bucket (e.g. `<prefix>/prefix/demo/<node>`
	/// for broadcasts under `demo/*`). Levels deeper than the broadcast
	/// path's segment count are skipped. `None` defaults to `1`.
	#[arg(long = "stats-levels", env = "MOQ_STATS_LEVELS")]
	pub levels: Option<u32>,

	/// Node identifier appended to advertised stats paths to disambiguate
	/// broadcasts when multiple relays share a cluster origin. Without this,
	/// peer relays would publish to the same `<prefix>/prefix/<level-path>`
	/// path and the origin's single-source delivery would drop all but one.
	///
	/// May be multi-segment (e.g. `sjc/1`, `sjc/2`) when a region has multiple
	/// hosts; the segments nest under a shared region key on the advertised
	/// path. Single-relay deployments can leave this unset.
	#[arg(long = "stats-node", env = "MOQ_STATS_NODE")]
	pub node: Option<String>,
}

impl StatsConfig {
	/// Build a [`Stats`] aggregator from this config, publishing on `origin`.
	///
	/// Returns [`Stats::disabled`] (a no-op aggregator) when [`Self::enabled`]
	/// is false, so the relay can attach the result unconditionally.
	pub fn build(&self, origin: OriginProducer) -> Stats {
		if !self.enabled.unwrap_or(false) {
			return Stats::disabled();
		}
		let levels = self.levels.unwrap_or(1).max(1);
		let prefix = self.prefix.clone().unwrap_or_else(|| ".stats".to_string());
		let node = self.node.clone().map(PathOwned::from);
		tracing::info!(prefix, levels, node = ?node, "stats publishing enabled");
		Stats::new(prefix, levels, node, origin)
	}
}
