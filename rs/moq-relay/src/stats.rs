//! Relay-side stats configuration.
//!
//! The counter collection lives in [`moq_net::stats::Registry`] and the
//! publishing task in [`moq_stats::Producer`]; this module just holds the
//! relay-specific config knobs.

use std::time::Duration;

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
#[derive(usage::Args, Clone, Debug, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct StatsConfig {
	/// Master switch for stats publishing. Defaults to false.
	///
	/// `Option` rather than a materialized default: Usage reads a standing `false`
	/// as an empty boolean, so `update_from` would refill it from the environment
	/// (or a declared default) over whatever the TOML file said. An `Option` is
	/// empty only when nothing set it. A bare `Vec<T>` has the same hazard, since
	/// an empty list also reads as absent; see moq-dev/moq#3051.
	#[usage(
		long = "stats-enabled",
		env = "MOQ_STATS_ENABLED",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub enabled: Option<bool>,

	/// Top-level path under which stats broadcasts are published. Defaults
	/// to `.stats`. Future stats categories (e.g. host-level node stats)
	/// will share the same prefix.
	#[usage(long = "stats-prefix", env = "MOQ_STATS_PREFIX", default = ".stats")]
	pub prefix: String,

	/// Interval (in seconds) between snapshot publishes. Defaults to 1.
	#[usage(long = "stats-interval", env = "MOQ_STATS_INTERVAL", default = "1")]
	pub interval: u64,

	/// Node identifier appended to the advertised stats path to disambiguate
	/// broadcasts when multiple relays share a cluster origin. Without this,
	/// peer relays would publish to the same `<prefix>/node` path and the
	/// origin's single-source delivery would drop all but one.
	///
	/// May be multi-segment (e.g. `sjc/1`, `sjc/2`) when a region has multiple
	/// hosts; the segments nest under a shared region key on the advertised
	/// path. Single-relay deployments can leave this unset.
	#[usage(long = "stats-node", env = "MOQ_STATS_NODE")]
	pub node: Option<String>,

	/// Number of leading broadcast-path segments to bucket stats by, one
	/// broadcast per bucket at `<prefix>/<group>/node/<node>`. Defaults to 0: a
	/// single `<prefix>/node/<node>` broadcast for the whole node. Set to 1 to
	/// publish a per-first-segment broadcast (e.g. per tenant), so a consumer can
	/// announce-scope to just that group rather than slurping every node's full
	/// stats. See [`moq_stats::ProducerConfig::depth`].
	#[usage(long = "stats-depth", env = "MOQ_STATS_DEPTH", default = "0")]
	pub depth: usize,
}

impl Default for StatsConfig {
	fn default() -> Self {
		Self {
			enabled: None,
			prefix: ".stats".into(),
			interval: 1,
			node: None,
			depth: 0,
		}
	}
}

impl StatsConfig {
	/// Build a [`moq_stats::Producer`] from this config, publishing on `origin`.
	///
	/// Returns a no-op producer when [`Self::enabled`] is false, so the relay can
	/// attach the result unconditionally. Hand it to
	/// [`Cluster::with_stats`](crate::Cluster::with_stats), which takes over both
	/// the registry and keeping the publish task alive (the task stops when the
	/// last clone of the producer drops).
	pub fn build(&self, origin: origin::Producer) -> moq_stats::Producer {
		if !self.enabled.unwrap_or(false) {
			return moq_stats::Producer::new(moq_stats::ProducerConfig::new());
		}
		let prefix = self.prefix.clone();
		let interval = Duration::from_secs(self.interval.max(1));
		let node = self.node.clone().map(PathOwned::from);
		let depth = self.depth;
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
