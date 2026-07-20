//! Publish and consume MoQ traffic stats.
//!
//! `moq-net` collects per-session traffic counters in a
//! [`stats::Registry`](moq_net::stats::Registry); this crate turns that
//! registry into MoQ broadcasts and back:
//!
//! - [`Producer`] drains a registry on an interval and publishes the counters
//!   as JSON tracks on an origin.
//! - [`Consumer`] subscribes to one published stats broadcast and yields typed
//!   frames, for aggregators, dashboards, and billing meters.
//!
//! # Wire format
//!
//! A [`Producer`] publishes one broadcast per node at `<prefix>/node/<node>`
//! (default prefix `.stats`; the node suffix disambiguates relays sharing a
//! cluster origin and may be multi-segment, e.g. `sjc/1`). A grouping `depth`
//! splits that into one broadcast per leading broadcast-path segments at
//! `<prefix>/<group>/node/<node>`, so a consumer can announce-scope to a
//! single group. Parse announce paths back with [`parse_node_path`].
//!
//! Traffic is bucketed by [`Tier`] (an arbitrary label chosen by business
//! logic: billing class, region, ...). The default tier is unprefixed; a named
//! tier prefixes its track names with its label. Each broadcast carries, per
//! tier, a publisher (egress) and a subscriber (ingress) traffic track plus a
//! sessions track, each in a plain and a compressed flavor:
//!
//! * `publisher.json` / `subscriber.json`: each frame is a JSON object mapping
//!   broadcast path to a cumulative [`Traffic`] snapshot ([`TrafficFrame`]),
//!   one full snapshot per frame.
//! * `sessions.json`: each frame maps auth root to a cumulative [`Presence`]
//!   gauge ([`SessionsFrame`]), counting connected sessions regardless of data
//!   flow.
//! * `<name>.json.z`: a compressed sibling of each of the above, encoded with
//!   [`moq_json::snapshot`] (group-scoped DEFLATE plus RFC 7396 merge-patch
//!   deltas). Since successive stats frames are nearly identical, this is a
//!   fraction of the plain track's bytes; read it with [`Consumer`] (or
//!   `moq_json` directly), not as raw JSON frames.
//!
//! Named-tier tracks (`<tier>/publisher.json`, ...) are created the first time
//! traffic records under that label; default-tier tracks always exist and hold
//! `{}` while idle. Compute names with [`traffic_track`] / [`sessions_track`].
//!
//! An entry appears in a frame while it is live (an open counter still exceeds
//! its `*_closed` counterpart, so traffic could resume at any moment) or on
//! the tick its snapshot changed, then is dropped once fully closed. Counters
//! are cumulative and monotonic: a downstream aggregator computes rates from
//! successive snapshots, and a counter going backwards means the relay
//! restarted or the entry was garbage collected and re-created, so consumers
//! should treat a decrease as a fresh segment.

mod consume;
mod produce;

pub use consume::{Consumer, ConsumerConfig, SessionsConsumer, TrafficConsumer};
pub use produce::{Producer, ProducerConfig};

use std::collections::BTreeMap;

/// Counter collection, re-exported from [`moq_net::stats`] so stats consumers
/// can depend on this crate alone.
pub use moq_net::stats::{Handle, Presence, Registry, Role, Tier, Traffic};

use moq_net::{AsPath, Path, PathOwned};

/// One frame off a traffic track: cumulative counters keyed by broadcast path.
pub type TrafficFrame = BTreeMap<String, Traffic>;

/// One frame off a sessions track: connect/disconnect gauges keyed by auth root.
pub type SessionsFrame = BTreeMap<String, Presence>;

/// Suffix appended to a plain track name for its compressed sibling.
pub const COMPRESSED_SUFFIX: &str = ".z";

/// The traffic track name for a tier and role: `<role>.json` at the prefix root
/// on the default tier (`publisher.json` / `subscriber.json`), `<tier>/<role>.json`
/// on a named one, plus [`COMPRESSED_SUFFIX`] when `compressed`.
pub fn traffic_track(tier: &Tier, role: Role, compressed: bool) -> String {
	let mut name = tier.track_name(&format!("{}.json", role.as_str()));
	if compressed {
		name.push_str(COMPRESSED_SUFFIX);
	}
	name
}

/// The sessions track name for a tier: `sessions.json` on the default tier,
/// `<tier>/sessions.json` on a named one, plus [`COMPRESSED_SUFFIX`] when
/// `compressed`.
pub fn sessions_track(tier: &Tier, compressed: bool) -> String {
	let mut name = tier.track_name("sessions.json");
	if compressed {
		name.push_str(COMPRESSED_SUFFIX);
	}
	name
}

/// A parsed stats broadcast path: `<prefix>[/<group>]/node[/<node>]`.
/// See [`parse_node_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodePath {
	/// The grouping key: the leading broadcast-path segments selected by the
	/// producer's `depth`, empty at depth 0.
	pub group: PathOwned,
	/// The node suffix, empty when the producer has no node configured.
	pub node: PathOwned,
}

/// Parse a stats broadcast announce path published under `prefix` with the
/// given grouping `depth`, splitting it into its group and node parts.
///
/// Returns `None` when the path is not under `prefix` or has no `node`
/// category segment where one is expected (which also filters out sibling
/// categories another producer may publish under the same prefix). A group
/// segment literally named `node` is ambiguous and will mis-parse; don't name
/// groups that.
pub fn parse_node_path(prefix: impl AsPath, depth: usize, path: impl AsPath) -> Option<NodePath> {
	let prefix = prefix.as_path();
	let path = path.as_path();
	let rest = if prefix.is_empty() {
		path.as_str()
	} else {
		path.as_str().strip_prefix(prefix.as_str())?.strip_prefix('/')?
	};

	// The group is at most `depth` segments (fewer when the broadcast path was
	// shorter), so `node` is the first literal "node" segment at or before
	// index `depth`.
	let mut segments = rest.split('/');
	let mut group: Vec<&str> = Vec::new();
	loop {
		let segment = segments.next()?;
		if segment == "node" {
			break;
		}
		if group.len() >= depth {
			return None;
		}
		group.push(segment);
	}

	let node = segments.collect::<Vec<_>>().join("/");
	Some(NodePath {
		group: Path::new(&group.join("/")).to_owned(),
		node: Path::new(&node).to_owned(),
	})
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
		let parse = |depth, path| parse_node_path(".stats", depth, path);

		// Depth 0: no group segment.
		assert_eq!(
			parse(0, ".stats/node/sjc"),
			Some(NodePath {
				group: Path::empty().to_owned(),
				node: Path::new("sjc").to_owned(),
			})
		);
		assert_eq!(
			parse(0, ".stats/node/sjc/1").unwrap().node,
			Path::new("sjc/1").to_owned(),
			"multi-segment node"
		);
		assert_eq!(
			parse(0, ".stats/node"),
			Some(NodePath {
				group: Path::empty().to_owned(),
				node: Path::empty().to_owned(),
			}),
			"nodeless path"
		);

		// Depth 1: one group segment, as published per tenant.
		assert_eq!(
			parse(1, ".stats/acme/node/sjc"),
			Some(NodePath {
				group: Path::new("acme").to_owned(),
				node: Path::new("sjc").to_owned(),
			})
		);
		// A shorter broadcast path yields a shorter group; still parses.
		assert_eq!(parse(1, ".stats/node/sjc").unwrap().group, Path::empty().to_owned());

		// Not ours: wrong prefix, sibling category, group deeper than depth.
		assert_eq!(parse(0, "other/node/sjc"), None);
		assert_eq!(parse(1, ".stats/acme/vod/sjc"), None, "sibling category filtered");
		assert_eq!(parse(0, ".stats/acme/node/sjc"), None, "group deeper than depth");
	}

	#[test]
	fn track_names() {
		let default = Tier::default();
		let regional = Tier::new("region/sjc");
		assert_eq!(traffic_track(&default, Role::Publisher, false), "publisher.json");
		assert_eq!(traffic_track(&default, Role::Subscriber, true), "subscriber.json.z");
		assert_eq!(
			traffic_track(&regional, Role::Publisher, false),
			"region/sjc/publisher.json"
		);
		assert_eq!(sessions_track(&default, false), "sessions.json");
		assert_eq!(sessions_track(&regional, true), "region/sjc/sessions.json.z");
	}
}
