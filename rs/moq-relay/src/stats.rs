//! Relay-side stats configuration.
//!
//! The actual aggregator lives in [`moq_net::Stats`]; this module just
//! holds the relay-specific config knobs.

use std::time::Duration;

use clap::Args;
use moq_net::{OriginProducer, PathOwned, Stats};
use serde::{Deserialize, Serialize};

/// Configuration for the relay's stats publishing.
///
/// Set `enabled = true` to attach a [`moq_net::Stats`] aggregator to every
/// session the relay accepts (and every cluster dial). The aggregator
/// publishes a single `<prefix>/node/<node>` broadcast (or `<prefix>/node`
/// when [`Self::node`] is unset) on the cluster origin. Each frame is a
/// JSON map of broadcast path to a cumulative counter snapshot; an entry
/// surfaces while the broadcast is live (any open counter exceeds its
/// `*_closed` counterpart) and on the tick its snapshot changes, then is
/// dropped once fully closed. See `moq_net::stats` for the per-field
/// semantics.
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
	/// "Config flags + TOML merge" note in `rs/CLAUDE.md`.
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

	/// Interval (in seconds) between snapshot publishes. Defaults to 1.
	#[arg(long = "stats-interval", env = "MOQ_STATS_INTERVAL")]
	pub interval: Option<u64>,

	/// Node identifier appended to the advertised stats path to disambiguate
	/// broadcasts when multiple relays share a cluster origin. Without this,
	/// peer relays would publish to the same `<prefix>/node` path and the
	/// origin's single-source delivery would drop all but one.
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
	/// Returns a no-op aggregator ([`Stats::default`]) when [`Self::enabled`]
	/// is false, so the relay can attach the result unconditionally.
	pub fn build(&self, origin: OriginProducer) -> Stats {
		if !self.enabled.unwrap_or(false) {
			return Stats::default();
		}
		let prefix = self.prefix.clone().unwrap_or_else(|| ".stats".to_string());
		let interval = Duration::from_secs(self.interval.unwrap_or(1).max(1));
		let node = self.node.clone().map(PathOwned::from);
		tracing::info!(prefix, interval_secs = interval.as_secs(), node = ?node, "stats publishing enabled");
		// Fully qualified to disambiguate from this module's clap-derived StatsConfig.
		let config = moq_net::StatsConfig::new()
			.with_origin(origin)
			.with_prefix(prefix)
			.with_interval(interval)
			.with_node(node);
		Stats::new(config)
	}
}
