//! Relay-side stats configuration.
//!
//! The counter collection lives in [`moq_net::stats::Registry`] and the
//! publishing task in [`moq_stats::Producer`]; this module just holds the
//! relay-specific config knobs.

use std::time::Duration;

use clap::Args;
use moq_net::PathOwned;
use moq_net::origin;
use serde::{Deserialize, Serialize};

/// Configuration for the relay's stats publishing.
///
/// Set `enabled = true` to attach a [`moq_stats::Producer`] to every session
/// the relay accepts (and every cluster dial). The producer publishes a single
/// `<prefix>/node/<node>` broadcast (or `<prefix>/node` when [`Self::node`] is
/// unset) on the cluster origin. Each broadcast carries plain `.json` tracks
/// (a JSON map of broadcast path to a cumulative counter snapshot per frame)
/// plus compressed `.json.z` siblings; see `moq_stats` for the wire format and
/// per-field semantics.
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

	/// Number of leading broadcast-path segments to bucket stats by, one
	/// broadcast per bucket at `<prefix>/<group>/node/<node>`. Defaults to 0: a
	/// single `<prefix>/node/<node>` broadcast for the whole node. Set to 1 to
	/// publish a per-first-segment broadcast (e.g. per tenant), so a consumer can
	/// announce-scope to just that group rather than slurping every node's full
	/// stats. See [`moq_stats::ProducerConfig::depth`].
	#[arg(long = "stats-depth", env = "MOQ_STATS_DEPTH")]
	pub depth: Option<usize>,
}

impl StatsConfig {
	/// Build a [`moq_stats::Producer`] from this config, publishing on `origin`.
	///
	/// Returns a no-op producer when [`Self::enabled`] is false, so the relay can
	/// attach the result unconditionally. Hand the producer's
	/// [`registry`](moq_stats::Producer::registry) to the cluster and keep the
	/// producer itself alive for as long as the relay runs (its publish task
	/// stops when the last clone drops).
	pub fn build(&self, origin: origin::Producer) -> moq_stats::Producer {
		if !self.enabled.unwrap_or(false) {
			return moq_stats::Producer::new(moq_stats::ProducerConfig::new());
		}
		let prefix = self.prefix.clone().unwrap_or_else(|| ".stats".to_string());
		let interval = Duration::from_secs(self.interval.unwrap_or(1).max(1));
		let node = self.node.clone().map(PathOwned::from);
		let depth = self.depth.unwrap_or(0);
		tracing::info!(prefix, interval_secs = interval.as_secs(), node = ?node, depth, "stats publishing enabled");
		let config = moq_stats::ProducerConfig::new()
			.with_origin(origin)
			.with_prefix(prefix)
			.with_interval(interval)
			.with_node(node)
			.with_depth(depth);
		moq_stats::Producer::new(config)
	}
}
