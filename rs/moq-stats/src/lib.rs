#![warn(missing_docs)]

//! Publish and consume MoQ traffic stats.
//!
//! `moq-net` collects session and request traffic counters in a
//! [`stats::Registry`](moq_net::stats::Registry); this crate turns that
//! registry into MoQ broadcasts and back:
//!
//! - [`Producer`] drains a registry on an interval and publishes the counters
//!   as JSON tracks on an origin.
//! - [`Consumer`] subscribes to one published stats broadcast and yields typed
//!   frames, for aggregators, dashboards, and billing meters.
//! - [`aggregate::Consumer`] folds every node's broadcast into one merged
//!   view, so a downstream sees a project's total live traffic as if it came
//!   from a single node.
//!
//! # Wire format
//!
//! A [`Producer`] publishes one broadcast per node at `<prefix>/node/<node>`
//! (default prefix `.stats`); parse announce paths back with
//! [`parse_node_path`]. Each announcement carries a fresh [`Epoch`] on its
//! route, so a restarted node is a new broadcast at the same path; a reader
//! tells them apart by the announced route's epoch. The broadcast carries a
//! `totals.json` track of each [`Tier`]'s [`Totals`] for the epoch, and a
//! reader may request any stats label's `<label>/presence.json` track and any
//! broadcast prefix's `<path>/prefix.json` track of [`Prefix`] counters. Each
//! is a compressed [`moq_json::snapshot`] track; compute names with
//! [`totals_track`], [`presence_track`], and [`prefix_track`].
//!
//! Each group of leading broadcast-path segments is a compressed
//! `<group>/group.json` track of [`GroupFrame`]s carrying the group's per-path
//! [`Traffic`] and per-label [`Presence`], listed through the broadcast's
//! track listing while the group is live; compute and parse names with
//! [`group_track`] and [`parse_group_track`]. The full contract (paths,
//! tracks, and counter semantics) is at
//! <https://doc.moq.dev/concept/stats>.

pub mod aggregate;
pub mod consume;
pub mod produce;

pub use consume::Consumer;
pub use produce::Producer;

use std::collections::BTreeMap;

/// Counter collection, re-exported from [`moq_net::stats`] so stats consumers
/// can depend on this crate alone.
pub use moq_net::stats::{Failure, Failures, Handle, Presence, RefusalKind, Refusals, Registry, Role, Tier, Traffic};

use moq_net::{Epoch, Path};

/// One frame off the totals track: each tier's [`Totals`], keyed by tier label
/// (`""` for the default tier).
pub type TotalsFrame = BTreeMap<String, Totals>;

/// One frame off a presence track: one stats label's sessions, keyed by tier label
/// (`""` for the default tier).
pub type PresenceFrame = BTreeMap<String, Presence>;

/// One frame off a prefix track: one broadcast prefix's [`Prefix`] counters,
/// keyed by tier label (`""` for the default tier).
pub type PrefixFrame = BTreeMap<String, Prefix>;

/// One tier's cumulative counters for a whole group: everything its entries
/// sent, received, and connected since the group's epoch began, including
/// entries that have since ended.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Totals {
	/// Egress: what this node sent.
	pub publisher: Traffic,
	/// Ingress: what this node received.
	pub subscriber: Traffic,
	/// Sessions connected under the group's stats labels.
	pub sessions: Presence,
}

impl Totals {
	/// Fold another readout into this one, counter by counter.
	pub fn add(&mut self, other: Totals) {
		self.publisher.add(other.publisher);
		self.subscriber.add(other.subscriber);
		self.sessions.add(other.sessions);
	}
}

/// Egress and ingress traffic for one part of a prefix.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Usage {
	/// Egress: what this node sent.
	pub publisher: Traffic,
	/// Ingress: what this node received.
	pub subscriber: Traffic,
}

impl Usage {
	pub(crate) fn add(&mut self, other: Usage) {
		self.publisher.add(other.publisher);
		self.subscriber.add(other.subscriber);
	}
}

/// One tier's counters on a prefix track: the rollup of every broadcast path
/// at or under the prefix, the paths at exactly the prefix, and the rollup of
/// each direct child. The rollup sits at the top level, so a [`Totals`] reader
/// parses it unchanged.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Prefix {
	/// Egress under the prefix.
	pub publisher: Traffic,
	/// Ingress under the prefix.
	pub subscriber: Traffic,
	/// The broadcast at exactly the prefix, apart from any nested under it.
	#[serde(rename = "self")]
	pub own: Usage,
	/// Each direct child's rollup, keyed by its next path segment.
	pub children: BTreeMap<String, Usage>,
}

/// One frame off a group track: one instance of the track, and its counters.
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct GroupFrame {
	/// Minted each time the group's track is created. Every counter in a frame
	/// with a new epoch starts from zero, so a reader keeps each epoch's counters
	/// apart rather than diffing across them.
	#[serde_as(as = "serde_with::DisplayFromStr")]
	pub epoch: Epoch,
	/// Each tier's [`Group`] counters, keyed by tier label (`""` for the default
	/// tier).
	#[serde(default)]
	pub tiers: BTreeMap<String, Group>,
}

/// One tier's counters on a group track: the group's totals for the track's
/// epoch, then its live per-path traffic and per-label sessions. The totals sit
/// at the top level, so a [`Totals`] reader parses them unchanged.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Group {
	/// Egress: everything the group's paths sent this epoch.
	pub publisher: Traffic,
	/// Ingress: everything the group's paths received this epoch.
	pub subscriber: Traffic,
	/// Sessions connected under the group's stats labels this epoch.
	pub sessions: Presence,
	/// Each live broadcast path's [`Usage`], from its first moved counter until
	/// the relay drops its counters. A side that never moved is left off the
	/// wire and reads as zero.
	pub paths: BTreeMap<String, Usage>,
	/// Each live stats label's [`Presence`], while a session or request holds
	/// it and on the drain its last one ends.
	pub labels: BTreeMap<String, Presence>,
}

/// The totals track's name.
pub(crate) const TOTALS: &str = "totals.json";

/// The last segment of a presence track's name.
const PRESENCE: &str = "presence.json";

/// The last segment of a prefix track's name.
const PREFIX: &str = "prefix.json";

/// The last segment of a group track's name.
const GROUP: &str = "group.json";

/// The totals track name: `totals.json`.
pub fn totals_track() -> String {
	TOTALS.to_string()
}

/// The presence track name for a stats label: `<label>/presence.json`, or
/// `presence.json` for the empty label. Served only while requested.
pub fn presence_track(label: impl Into<Path>) -> String {
	keyed_track(&label.into(), PRESENCE)
}

/// The prefix track name for a broadcast prefix: `<path>/prefix.json`, or
/// `prefix.json` for the empty prefix. Served only while requested.
pub fn prefix_track(path: impl Into<Path>) -> String {
	keyed_track(&path.into(), PREFIX)
}

/// The group track name for a group: `<group>/group.json`, or `group.json` for
/// the empty group.
pub fn group_track(group: impl Into<Path>) -> String {
	keyed_track(&group.into(), GROUP)
}

/// The group a group track name carries; `None` for any other name. The
/// inverse of [`group_track`], for reading a stats broadcast's track listing.
pub fn parse_group_track(name: &str) -> Option<Path> {
	parse_keyed_track(name, GROUP)
}

/// `<key>/<kind>`, or `<kind>` for the empty key.
fn keyed_track(key: &Path, kind: &str) -> String {
	match key.is_empty() {
		true => kind.to_string(),
		false => format!("{}/{kind}", key.as_str()),
	}
}

/// The stats label a presence track name requests; `None` for any other name.
/// The inverse of [`presence_track`].
pub(crate) fn parse_presence_track(name: &str) -> Option<Path> {
	parse_keyed_track(name, PRESENCE)
}

/// The broadcast prefix a prefix track name requests; `None` for any other
/// name. The inverse of [`prefix_track`].
pub(crate) fn parse_prefix_track(name: &str) -> Option<Path> {
	parse_keyed_track(name, PREFIX)
}

/// The key a `<key>/<kind>` name requests: the last segment picks the kind and
/// everything before it is the key, so names stay injective across kinds.
fn parse_keyed_track(name: &str, kind: &str) -> Option<Path> {
	let key = match name.strip_suffix(kind)? {
		"" => "",
		rest => rest.strip_suffix('/').filter(|key| !key.is_empty())?,
	};
	// Only a valid key round-trips, so each key has exactly one name.
	Path::new(key).ok()
}

/// The node a stats broadcast path names: `<prefix>/node/<node>` yields
/// `<node>`, and `<prefix>/node` the empty node.
///
/// Returns `None` when the path is not under `prefix` or has no `node`
/// category segment right after it, which also filters out sibling categories
/// another producer may publish under the same prefix.
pub fn parse_node_path(prefix: impl Into<Path>, path: impl Into<Path>) -> Option<Path> {
	let prefix: Path = prefix.into();
	let path: Path = path.into();
	let rest = if prefix.is_empty() {
		path.as_str()
	} else {
		path.as_str().strip_prefix(prefix.as_str())?.strip_prefix('/')?
	};
	let node = match rest.strip_prefix("node")? {
		"" => "",
		node => node.strip_prefix('/')?,
	};
	Path::new(node).ok()
}

/// Errors produced while publishing or consuming stats.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An error from the underlying track or broadcast.
	#[error(transparent)]
	Net(#[from] moq_net::Error),

	/// An error decoding or encoding a stats frame.
	#[error(transparent)]
	Json(#[from] moq_json::Error),
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_node_path_variants() {
		let parse =
			|path: &str| parse_node_path(moq_net::Path::from_static(".stats"), moq_net::Path::new(path).unwrap());

		assert_eq!(parse(".stats/node/sjc"), Some(Path::from_static("sjc")));
		assert_eq!(
			parse(".stats/node/sjc/1"),
			Some(Path::from_static("sjc/1")),
			"multi-segment node"
		);
		assert_eq!(parse(".stats/node"), Some(Path::empty()), "nodeless path");

		// Not ours: wrong prefix, a sibling category, or anything between.
		assert_eq!(parse("other/node/sjc"), None);
		assert_eq!(parse(".stats/cluster/sjc"), None, "sibling category filtered");
		assert_eq!(parse(".stats/nodes/sjc"), None, "a longer segment is not `node`");
		assert_eq!(parse(".stats/acme/node/sjc"), None, "a segment before `node`");
	}

	#[test]
	fn track_names() {
		assert_eq!(totals_track(), "totals.json");
		assert_eq!(
			presence_track(moq_net::Path::from_static("acme/live")),
			"acme/live/presence.json"
		);
		assert_eq!(presence_track(moq_net::Path::from_static("")), "presence.json");
		assert_eq!(
			prefix_track(moq_net::Path::from_static("acme/live")),
			"acme/live/prefix.json"
		);
		assert_eq!(prefix_track(moq_net::Path::from_static("")), "prefix.json");
	}

	#[test]
	fn presence_track_names_round_trip() {
		for label in ["", "acme", "acme/live", "a.json", "x/presence.json", "publisher", "z"] {
			let name = presence_track(moq_net::Path::new(label).unwrap());
			assert_eq!(
				parse_presence_track(&name),
				Some(Path::try_from(label).unwrap()),
				"{name}"
			);
		}

		// Names no label produces, including ones with an empty segment, which would give
		// one label two names, and the retired `.z` sibling.
		for name in [
			"publisher.json",
			"acme/publisher.json",
			"sessions.json",
			"acme/sessions.json",
			"totals.json",
			"xpresence.json",
			"/presence.json",
			"acme//presence.json",
			"a//b/presence.json",
			"/a/presence.json",
			"presence.json.z",
		] {
			assert_eq!(parse_presence_track(name), None, "{name}");
		}
	}

	#[test]
	fn prefix_track_names_round_trip() {
		for path in [
			"",
			"acme",
			"acme/live",
			"a.json",
			"x/prefix.json",
			"x/presence.json",
			"publisher",
		] {
			let name = prefix_track(moq_net::Path::new(path).unwrap());
			assert_eq!(parse_prefix_track(&name), Some(Path::try_from(path).unwrap()), "{name}");
			assert_eq!(parse_presence_track(&name), None, "{name} is not a presence track");
		}

		for name in [
			"publisher.json",
			"acme/publisher.json",
			"presence.json",
			"acme/presence.json",
			"totals.json",
			"xprefix.json",
			"/prefix.json",
			"acme//prefix.json",
			"prefix.json.z",
		] {
			assert_eq!(parse_prefix_track(name), None, "{name}");
		}
	}

	#[test]
	fn prefix_entry_reads_as_totals() {
		let mut prefix = Prefix::default();
		prefix.publisher.bytes = 7;
		prefix.own.publisher.bytes = 3;
		prefix.children.insert("cam".into(), Usage::default());
		let json = serde_json::to_value(&prefix).unwrap();
		assert_eq!(json["self"]["publisher"]["bytes"], 3);
		assert!(json["children"]["cam"]["subscriber"].is_object());
		let totals: Totals = serde_json::from_value(json.clone()).unwrap();
		assert_eq!(totals.publisher.bytes, 7, "a totals reader reads the rollup");
		assert_eq!(serde_json::from_value::<Prefix>(json).unwrap(), prefix);
	}
}
