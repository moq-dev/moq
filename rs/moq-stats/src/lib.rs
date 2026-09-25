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
//! Every per-broadcast entry is a [`Stats<E>`]: the [`Traffic`] counters plus
//! an extension `E` flattened beside them. A relay uses `()`, adding nothing;
//! a media client uses `hang::Stats` to report what it sent, received, and
//! played. A consumer that does not know the extension ignores its fields.
//!
//! # Naming
//!
//! A stats broadcast is telemetry, not content, and its path says so with a
//! segment ending in `.stats`: a relay publishes under the `.stats` prefix
//! (below), and a client publishes one broadcast at a path its token allows,
//! like `room/alice.stats` ([`produce::Config::at`]). Tell telemetry from
//! content with [`is_stats`].
//!
//! # Wire format
//!
//! A relay's [`Producer`] publishes one broadcast per node at `<prefix>/node/<node>`
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
//!   broadcast path to a cumulative [`Stats`] snapshot ([`TrafficFrame`]),
//!   one full snapshot per frame.
//! * `<path>/publisher.json` / `<path>/subscriber.json`: the same track
//!   filtered to one broadcast path, served on request
//!   ([`Consumer::traffic_for`]). On a named tier the name is
//!   `<tier>/<path>/<role>.json`, matched against the tier labels the producer
//!   has seen, longest first; a default-tier path that starts with one of
//!   those labels is ambiguous and refused.
//! * `sessions.json`: each frame maps auth root to a cumulative [`Presence`]
//!   gauge ([`SessionsFrame`]), counting connected sessions regardless of data
//!   flow. It never carries the extension.
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
//! An entry appears in a frame while it is live (a started counter still exceeds
//! its `*_ended` counterpart, so traffic could resume at any moment, or its
//! extension is still attached) or on the tick its snapshot changed, then is
//! dropped once fully closed. Counters are cumulative and monotonic: a
//! downstream aggregator computes rates from successive snapshots, and a
//! counter going backwards means the producer restarted or the entry was
//! garbage collected and re-created, so consumers should treat a decrease as a
//! fresh segment.

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
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};

/// Folds one contributor's cumulative stats into a running total, so the
/// [`aggregate`] reader can sum the same entry across producers.
///
/// Counters add. Gauges (a latency, a target bitrate) mean nothing summed, so
/// `merge` leaves `self`'s untouched.
pub trait Merge {
	/// Fold `other` into `self`.
	fn merge(&mut self, other: &Self);
}

impl Merge for () {
	fn merge(&mut self, _other: &Self) {}
}

impl Merge for Traffic {
	fn merge(&mut self, other: &Self) {
		self.add(*other);
	}
}

impl Merge for Presence {
	fn merge(&mut self, other: &Self) {
		self.add(*other);
	}
}

/// An extension carried in every per-broadcast entry beside [`Traffic`],
/// flattened into the same JSON object. `()` adds nothing.
///
/// Every field should default when absent and unknown fields should be
/// ignored, so producers and consumers with different extensions still read
/// each other's [`Traffic`].
pub trait Ext: Serialize + DeserializeOwned + Default + Clone + Merge + Send + Sync + 'static {
	/// Decode one entry carrying this extension. The default buffers the
	/// entry's fields to hand them to both halves; `()` has no fields and
	/// decodes [`Traffic`] straight through, which a relay's readers rely on.
	fn deserialize_stats<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Stats<Self>, D::Error> {
		Flat::deserialize(deserializer).map(|Flat { traffic, ext }| Stats { traffic, ext })
	}
}

impl Ext for () {
	fn deserialize_stats<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Stats<Self>, D::Error> {
		Traffic::deserialize(deserializer).map(|traffic| Stats { traffic, ext: () })
	}
}

/// One per-broadcast entry: the [`Traffic`] counters with an extension `E`
/// flattened beside them.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
#[serde(bound(serialize = "E: Serialize"))]
pub struct Stats<E = ()> {
	/// The transport counters every producer reports.
	#[serde(flatten)]
	pub traffic: Traffic,
	/// The producer's extension, `()` for a relay.
	#[serde(flatten)]
	pub ext: E,
}

impl<'de, E: Ext> Deserialize<'de> for Stats<E> {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
		E::deserialize_stats(deserializer)
	}
}

/// The flattened decode behind [`Ext::deserialize_stats`].
#[derive(Deserialize)]
#[serde(bound(deserialize = "E: DeserializeOwned"))]
struct Flat<E> {
	#[serde(flatten)]
	traffic: Traffic,
	#[serde(flatten)]
	ext: E,
}

impl<E: Merge> Merge for Stats<E> {
	fn merge(&mut self, other: &Self) {
		self.traffic.merge(&other.traffic);
		self.ext.merge(&other.ext);
	}
}

/// One frame off a traffic track: cumulative entries keyed by broadcast path.
/// A relay's frames carry no extension, the `()` default.
pub type TrafficFrame<E = ()> = BTreeMap<String, Stats<E>>;

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

/// Whether `path` names a stats broadcast rather than content: some segment
/// ends in `.stats`, as in a client's `room/alice.stats` or anything under a
/// relay's `.stats/` prefix.
pub fn is_stats(path: impl AsPath) -> bool {
	path.as_path()
		.as_str()
		.split('/')
		.any(|segment| segment.ends_with(STATS_SUFFIX))
}

/// The suffix marking a stats broadcast path. See [`is_stats`].
const STATS_SUFFIX: &str = ".stats";

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

	/// A stats broadcast path whose last segment does not end in `.stats`.
	#[error("not a stats path, the last segment must end in .stats: {0}")]
	NotStats(PathOwned),
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

	/// A test extension: one counter and one gauge.
	#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
	#[serde(default)]
	struct Media {
		decoded: u64,
		#[serde(skip_serializing_if = "Option::is_none")]
		latency: Option<u64>,
	}

	impl Merge for Media {
		fn merge(&mut self, other: &Self) {
			self.decoded += other.decoded;
		}
	}

	impl Ext for Media {}

	#[test]
	fn relay_frame_parses_with_any_extension() {
		let relay = r#"{"room":{"announces_started":1,"announced":1,"bytes":5}}"#;

		let frame: TrafficFrame = serde_json::from_str(relay).unwrap();
		assert_eq!(frame["room"].traffic.bytes, 5);
		assert_eq!(frame["room"].traffic.announces_started, 1);

		let frame: TrafficFrame<Media> = serde_json::from_str(relay).unwrap();
		assert_eq!(frame["room"].traffic.bytes, 5);
		assert_eq!(frame["room"].ext, Media::default(), "the extension defaults");
	}

	#[test]
	fn extended_frame_parses_as_plain_traffic() {
		let media = r#"{"room":{"bytes":5,"decoded":3,"latency":9}}"#;

		let frame: BTreeMap<String, Traffic> = serde_json::from_str(media).unwrap();
		assert_eq!(frame["room"].bytes, 5, "an older reader ignores the extension");
		let frame: TrafficFrame = serde_json::from_str(media).unwrap();
		assert_eq!(frame["room"].traffic.bytes, 5);

		let frame: TrafficFrame<Media> = serde_json::from_str(media).unwrap();
		assert_eq!(frame["room"].traffic.bytes, 5);
		assert_eq!(frame["room"].ext.decoded, 3);
		assert_eq!(frame["room"].ext.latency, Some(9));

		// And back out as one flat object.
		let value = serde_json::to_value(&frame["room"]).unwrap();
		assert_eq!(value["bytes"], 5);
		assert_eq!(value["decoded"], 3);
		assert_eq!(value["latency"], 9);
	}

	#[test]
	fn unit_extension_writes_plain_traffic() {
		let mut traffic = Traffic::default();
		traffic.bytes = 7;
		let stats = Stats { traffic, ext: () };
		assert_eq!(
			serde_json::to_string(&stats).unwrap(),
			serde_json::to_string(&traffic).unwrap()
		);
	}

	#[test]
	fn merge_sums_counters_and_leaves_gauges() {
		let stats = |bytes, decoded, latency| {
			let mut traffic = Traffic::default();
			traffic.bytes = bytes;
			Stats {
				traffic,
				ext: Media { decoded, latency },
			}
		};
		let mut total = stats(10, 3, Some(100));
		total.merge(&stats(5, 4, Some(900)));
		assert_eq!(total.traffic.bytes, 15);
		assert_eq!(total.ext.decoded, 7);
		assert_eq!(total.ext.latency, Some(100), "a gauge is never summed or replaced");
	}

	#[test]
	fn is_stats_matches_both_conventions() {
		assert!(is_stats("room/alice.stats"));
		assert!(is_stats(".stats/node/sjc"));
		assert!(is_stats(".stats/acme/node/sjc"));
		assert!(!is_stats("room/alice"));
		assert!(!is_stats("room/stats"));
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
