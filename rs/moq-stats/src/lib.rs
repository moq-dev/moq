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
//! - [`aggregate::Consumer`] folds a whole group's per-node broadcasts into one
//!   merged view, so a downstream sees a project's total live traffic as if it
//!   came from a single node.
//!
//! # Wire format
//!
//! A [`Producer`] publishes one broadcast per node at `<prefix>/node/<node>`
//! (default prefix `.stats`), or one per group of leading broadcast-path
//! segments at `<prefix>/<group>/node/<node>`; parse announce paths back with
//! [`parse_node_path`]. Each announcement carries a fresh
//! [`Epoch`](moq_net::Epoch) on its route, so a restarted node or a group
//! returning from idle is a new broadcast at the same path; a reader tells
//! them apart by the announced route's epoch. Every broadcast carries a
//! `totals.json` track of each [`Tier`]'s [`Totals`] for the epoch, each tier
//! carries `publisher.json` and `subscriber.json` tracks of per-path
//! [`Traffic`], and a reader may request any auth root's
//! `<root>/presence.json` track of [`Presence`]. Every track has a `.z`
//! sibling encoded with [`moq_json::snapshot`]; compute names with
//! [`totals_track`], [`traffic_track`], and [`presence_track`]. The full
//! contract (paths, tracks, both encodings, and counter semantics) is at
//! <https://doc.moq.dev/concept/stats>.

pub mod aggregate;
pub mod consume;
pub mod produce;

pub use consume::Consumer;
pub use produce::Producer;

use std::collections::BTreeMap;

/// Counter collection, re-exported from [`moq_net::stats`] so stats consumers
/// can depend on this crate alone.
pub use moq_net::stats::{Handle, Presence, Registry, Role, Tier, Traffic};

use moq_net::{AsPath, Path, PathOwned};

/// One frame off a traffic track: cumulative counters keyed by broadcast path.
pub type TrafficFrame = BTreeMap<String, Traffic>;

/// One frame off the totals track: each tier's [`Totals`], keyed by tier label
/// (`""` for the default tier).
pub type TotalsFrame = BTreeMap<String, Totals>;

/// One frame off a presence track: one auth root's sessions, keyed by tier label
/// (`""` for the default tier).
pub type PresenceFrame = BTreeMap<String, Presence>;

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
	/// Sessions connected under the group's auth roots.
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

/// Suffix appended to a plain track name for its compressed sibling.
pub const COMPRESSED_SUFFIX: &str = ".z";

/// The totals track's plain name.
pub(crate) const TOTALS: &str = "totals.json";

/// The last segment of a presence track's plain name.
const PRESENCE: &str = "presence.json";

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

/// The totals track name: `totals.json`, plus [`COMPRESSED_SUFFIX`] when
/// `compressed`.
pub fn totals_track(compressed: bool) -> String {
	let mut name = TOTALS.to_string();
	if compressed {
		name.push_str(COMPRESSED_SUFFIX);
	}
	name
}

/// The presence track name for an auth root: `<root>/presence.json`, or
/// `presence.json` for the empty root, plus [`COMPRESSED_SUFFIX`] when
/// `compressed`. Served only while requested.
pub fn presence_track(root: impl AsPath, compressed: bool) -> String {
	let root = root.as_path();
	let mut name = match root.is_empty() {
		true => PRESENCE.to_string(),
		false => format!("{}/{PRESENCE}", root.as_str()),
	};
	if compressed {
		name.push_str(COMPRESSED_SUFFIX);
	}
	name
}

/// The auth root a presence track name requests, and whether it is the
/// compressed flavor; `None` for any other name. The inverse of
/// [`presence_track`].
pub(crate) fn parse_presence_track(name: &str) -> Option<(PathOwned, bool)> {
	let (plain, compressed) = match name.strip_suffix(COMPRESSED_SUFFIX) {
		Some(plain) => (plain, true),
		None => (name, false),
	};
	let root = match plain.strip_suffix(PRESENCE)? {
		"" => "",
		rest => rest.strip_suffix('/').filter(|root| !root.is_empty())?,
	};
	// Only a normalized root round-trips, so each root has exactly one name.
	let path = Path::new(root);
	if path.as_str() != root {
		return None;
	}
	Some((path.to_owned(), compressed))
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
		assert_eq!(totals_track(false), "totals.json");
		assert_eq!(totals_track(true), "totals.json.z");
		assert_eq!(presence_track("acme/live", false), "acme/live/presence.json");
		assert_eq!(presence_track("", true), "presence.json.z");
	}

	#[test]
	fn presence_track_names_round_trip() {
		for root in ["", "acme", "acme/live", "a.json", "x/presence.json", "publisher", "z"] {
			for compressed in [false, true] {
				let name = presence_track(root, compressed);
				assert_eq!(
					parse_presence_track(&name),
					Some((PathOwned::from(root), compressed)),
					"{name}"
				);
			}
		}

		// Names no root produces, including unnormalized ones, which would give
		// one root two names.
		for name in [
			"publisher.json",
			"acme/publisher.json",
			"totals.json",
			"xpresence.json",
			"/presence.json",
			"acme//presence.json",
			"a//b/presence.json",
			"/a/presence.json",
			"presence.json.z.z",
		] {
			assert_eq!(parse_presence_track(name), None, "{name}");
		}
	}
}
