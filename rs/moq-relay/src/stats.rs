//! Relay-side stats configuration.
//!
//! The counter collection lives in [`moq_net::stats::Registry`] and the
//! publishing task in [`moq_stats::Producer`]; this module just holds the
//! relay-specific config knobs.

use std::time::Duration;

use anyhow::Context;
use moq_net::Path;
use moq_net::origin;
use serde::{Deserialize, Serialize};

/// Configuration for the relay's stats publishing.
///
/// Set `enabled = true` to attach a [`moq_stats::Producer`] to every session
/// the relay accepts (and every cluster dial). The producer publishes a single
/// `<prefix>/node/<node>` broadcast (or `<prefix>/node` when [`Self::node`] is
/// unset) on the cluster origin, under a fresh epoch each run. The broadcast
/// carries a `totals.json` track of per-tier totals, one `<group>/group.json`
/// track per [`Self::depth`] group with its per-path traffic and per-label
/// sessions, and presence and prefix tracks on request, each a compressed JSON
/// track; see `moq_stats` for the wire format and per-field semantics.
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
	/// `<group>/group.json` track per bucket on the node's broadcast. Defaults
	/// to 0: a single `group.json` track for the whole node. Set to 1 to
	/// publish a per-first-segment track (e.g. per tenant), so a consumer
	/// subscribes to just that group rather than slurping every node's full
	/// stats.
	#[usage(
		long = "stats-depth",
		env = "MOQ_STATS_DEPTH",
		default = "0",
		setting = "stats.depth"
	)]
	pub depth: usize,

	/// How long a group's stats track stays listed after the group's last
	/// session and traffic leave, e.g. "5m" or "30s". Defaults to 5 minutes. A
	/// group that returns within it keeps its track and counters; one that
	/// returns after it is a new track instance counted from zero. Only applies
	/// at `depth` 1 or more. See [`moq_stats::produce::Config::linger`].
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
	pub fn linger(&self) -> Option<Duration> {
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
		let prefix = Path::new(&self.prefix).context("invalid stats.prefix")?;
		let interval = self.interval();
		anyhow::ensure!(interval != Some(Duration::ZERO), "stats.interval must be non-zero");
		let node = self
			.node
			.as_deref()
			.map(Path::new)
			.transpose()
			.context("invalid stats.node")?;
		let depth = self.depth;
		let linger = self.linger();
		tracing::info!(%prefix, ?interval, node = ?node, depth, ?linger, "stats publishing enabled");
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
