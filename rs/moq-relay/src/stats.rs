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
/// unset) on the cluster origin, under a fresh epoch each run. Each broadcast
/// carries plain `.json` tracks of cumulative counters (per-tier totals,
/// per-path traffic, per-root sessions, and per-root presence on request) plus compressed
/// `.json.z` siblings; see `moq_stats` for the wire format and per-field
/// semantics. [`Self::layout`] `track` keeps the one broadcast and publishes
/// each [`Self::depth`] group as a track on it instead.
#[derive(usage_rs::Args, Clone, Debug, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Master switch for stats publishing. Defaults to false.
	#[usage(
		long = "stats-enabled",
		env = "MOQ_STATS_ENABLED",
		setting = "stats.enabled",
		bool_value
	)]
	pub enabled: bool,

	/// Top-level path under which stats broadcasts are published. Defaults
	/// to `.stats`. Future stats categories (e.g. host-level node stats)
	/// will share the same prefix.
	#[usage(
		long = "stats-prefix",
		env = "MOQ_STATS_PREFIX",
		default = ".stats",
		setting = "stats.prefix"
	)]
	pub prefix: String,

	/// How often a snapshot is published, e.g. "1s" or "500ms". Defaults to 1
	/// second.
	#[usage(long = "stats-interval", env = "MOQ_STATS_INTERVAL", setting = "stats.interval")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub interval: Option<moq_tokio::cli::Duration>,

	/// Node identifier appended to the advertised stats path to disambiguate
	/// broadcasts when multiple relays share a cluster origin. Without this,
	/// peer relays would publish to the same `<prefix>/node` path and the
	/// origin's single-source delivery would drop all but one.
	///
	/// May be multi-segment (e.g. `sjc/1`, `sjc/2`) when a region has multiple
	/// hosts; the segments nest under a shared region key on the advertised
	/// path. Single-relay deployments can leave this unset.
	#[usage(long = "stats-node", env = "MOQ_STATS_NODE", setting = "stats.node")]
	pub node: Option<String>,

	/// Number of leading broadcast-path segments to bucket stats by, one
	/// broadcast per bucket at `<prefix>/<group>/node/<node>`. Defaults to 0: a
	/// single `<prefix>/node/<node>` broadcast for the whole node. Set to 1 to
	/// publish a per-first-segment broadcast (e.g. per tenant), so a consumer can
	/// announce-scope to just that group rather than slurping every node's full
	/// stats.
	#[usage(
		long = "stats-depth",
		env = "MOQ_STATS_DEPTH",
		default = "0",
		setting = "stats.depth"
	)]
	pub depth: usize,

	/// Where each `depth` group is published: `broadcast` (the default)
	/// announces one broadcast per group per node; `track` keeps the single
	/// `<prefix>/node/<node>` broadcast and lists each group as a
	/// `<group>/group.json` track on it, so group churn costs no announce
	/// traffic.
	///
	/// See [`moq_stats::produce::Layout`].
	#[usage(
		long = "stats-layout",
		env = "MOQ_STATS_LAYOUT",
		value_enum,
		default = "broadcast",
		setting = "stats.layout"
	)]
	pub layout: Layout,

	/// How long a group's stats broadcast stays announced after the group's
	/// last session and traffic leave, e.g. "5m" or "30s". Defaults to 5
	/// minutes. A group that returns within it keeps its broadcast, so viewer
	/// churn doesn't unannounce and re-announce it across the mesh; one that
	/// returns after it announces under a new epoch. Only applies at `depth` 1
	/// or more. See [`moq_stats::produce::Config::linger`].
	#[usage(long = "stats-linger", env = "MOQ_STATS_LINGER", setting = "stats.linger")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub linger: Option<moq_tokio::cli::Duration>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			enabled: false,
			prefix: ".stats".into(),
			interval: None,
			node: None,
			depth: 0,
			layout: Layout::Broadcast,
			linger: None,
		}
	}
}

impl Config {
	/// The publish interval; `None` keeps the producer's default.
	pub(crate) fn interval(&self) -> Option<Duration> {
		self.interval.map(Into::into)
	}

	/// The linger after command-line overrides; `None` keeps the producer's default.
	pub(crate) fn linger(&self) -> Option<Duration> {
		self.linger.map(Into::into)
	}

	/// Build a [`moq_stats::Producer`] from this config, publishing on `origin`.
	///
	/// Returns a no-op producer when [`Self::enabled`] is false, so the relay can
	/// attach the result unconditionally. Hand it to
	/// [`Cluster::with_stats`](crate::cluster::Cluster::with_stats), which takes over both
	/// the registry and keeping the publish task alive (the task stops when the
	/// last clone of the producer drops). Fails on a zero interval.
	pub fn build(&self, origin: origin::Producer) -> anyhow::Result<moq_stats::Producer> {
		if !self.enabled {
			return Ok(moq_stats::Producer::new(moq_stats::produce::Config::new()));
		}
		let prefix = self.prefix.clone();
		let interval = self.interval();
		anyhow::ensure!(interval != Some(Duration::ZERO), "stats.interval must be non-zero");
		let node = self.node.clone().map(PathOwned::from);
		let depth = self.depth;
		let layout = self.layout;
		let linger = self.linger();
		tracing::info!(prefix, ?interval, node = ?node, depth, ?layout, ?linger, "stats publishing enabled");
		let mut config = moq_stats::produce::Config::new()
			.with_origin(origin)
			.with_prefix(prefix)
			.with_node(node)
			.with_depth(depth);
		if let Some(interval) = interval {
			config = config.with_interval(interval);
		}
		if let Some(linger) = linger {
			config = config.with_linger(linger);
		}
		Ok(moq_stats::Producer::new(config))
	}
}

/// Where each stats group is published; see [`moq_stats::produce::Layout`].
#[derive(usage_rs::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[usage(ignore_case)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
	/// One broadcast per group per node.
	#[default]
	Broadcast,
	/// One broadcast per node, with each group a track on it.
	Track,
}

impl From<Layout> for moq_stats::produce::Layout {
	fn from(layout: Layout) -> Self {
		match layout {
			Layout::Broadcast => Self::Broadcast,
			Layout::Track => Self::Track,
		}
	}
}
