//! The publishing half: drain a [`Registry`] on an interval into stats tracks.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use std::task::Poll;

use moq_clock::Instant;
use moq_net::stats::{Presence, Registry, Report, Tier, Traffic, TrafficEntry};
use moq_net::{Epoch, Path, broadcast, kio, origin, track};
use serde::Serialize;
use web_async::spawn;

use crate::{
	Prefix, Totals, Usage, parse_prefix_track, parse_presence_track, prefix_track, presence_track, totals_track,
};

/// Settings for a [`Producer`]. Construct with [`Config::new`] and chain
/// the `with_*` setters (e.g.
/// `Config::new().with_origin(origin).with_prefix(".foo")`), then hand it
/// to [`Producer::new`].
///
/// With no origin set the resulting producer is a no-op: its registry is
/// disabled (bumps are dropped) and no task spawns. Call
/// [`Config::with_origin`] to publish.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Origin the stats broadcast is created on.
	/// When `None`, [`Producer::new`] spawns no task and publishes nothing.
	pub origin: Option<origin::Producer>,
	/// Top-level path stats are published under (default `.stats`). The full
	/// advertised path is `<prefix>/node/<node>` (or `<prefix>/node` when
	/// `node` is unset), announced under a fresh [`Epoch`] each time. Also the
	/// registry's exclude prefix, so serving a stats broadcast doesn't generate
	/// more stats.
	pub prefix: Path,
	/// Node suffix that disambiguates broadcasts from different relays sharing a
	/// cluster origin. Set this on every node in multi-relay deployments. May be
	/// multi-segment (e.g. `sjc/1`, `sjc/2`) so a region with multiple hosts can
	/// nest under a shared region key. An empty path is treated as unset.
	/// Default none.
	pub node: Option<Path>,
	/// How long the publish task waits between drains. Default 1s. Non-zero:
	/// it is the timescale of every track's group numbers, so a track opens at
	/// most one group per interval.
	pub interval: Duration,
	/// How many leading broadcast-path segments to use as a grouping key, each
	/// group a [`group_track`](crate::group_track) on the node's broadcast.
	///
	/// Default `0` publishes one `group.json` track carrying every path, for
	/// the producer's life. `1` publishes one `<group>/group.json` track per
	/// first segment, and larger values include more leading segments. A
	/// group's track is listed while its group has live traffic, plus the
	/// [`Self::linger`], and a group returning after that is a new track
	/// instance with a new [`GroupFrame::epoch`](crate::GroupFrame::epoch),
	/// counted from zero.
	pub depth: usize,
	/// How long a group's track stays listed after its group's last entry
	/// leaves. Default 5 minutes.
	///
	/// A group that returns within the linger keeps its track and counters.
	/// Zero ends the track on the first drain the group is empty. Holding the
	/// track doesn't count as an entry: ending it ends every subscription to
	/// it. Unused at depth `0`.
	///
	/// A return after the linger is a new epoch, which an aggregating reader
	/// keeps apart for its grace, so a short linger on a flapping group
	/// multiplies that reader's state.
	pub linger: Duration,
}

impl Config {
	/// A config with default settings: no origin (no-op), `.stats` prefix, 1s
	/// interval, and no node suffix. Call [`Self::with_origin`] to actually
	/// publish.
	pub fn new() -> Self {
		Self {
			origin: None,
			prefix: Path::from_static(".stats"),
			node: None,
			interval: Duration::from_secs(1),
			depth: 0,
			linger: DEFAULT_LINGER,
		}
	}

	/// Set the origin to publish the stats broadcast on. Without this the
	/// producer is a no-op.
	pub fn with_origin(mut self, origin: impl Into<Option<origin::Producer>>) -> Self {
		self.origin = origin.into();
		self
	}

	/// Override the top-level prefix (default `.stats`).
	pub fn with_prefix(mut self, prefix: impl Into<Path>) -> Self {
		self.prefix = prefix.into();
		self
	}

	/// Override the publish interval (default 1s).
	pub fn with_interval(mut self, interval: Duration) -> Self {
		self.interval = interval;
		self
	}

	/// Set the node suffix (default none). An empty path is treated as unset.
	pub fn with_node(mut self, node: impl Into<Option<Path>>) -> Self {
		self.node = node.into();
		self
	}

	/// Set the grouping depth (default 0, a single group). See [`Self::depth`].
	pub fn with_depth(mut self, depth: usize) -> Self {
		self.depth = depth;
		self
	}

	/// Set how long an empty group's track stays listed (default 5 minutes).
	/// See [`Self::linger`].
	pub fn with_linger(mut self, linger: Duration) -> Self {
		self.linger = linger;
		self
	}
}

/// The default [`Config::linger`]. Sized from a live fleet, where an idle
/// group's viewers returned 46 to 112 seconds after the last one left.
const DEFAULT_LINGER: Duration = Duration::from_secs(300);

impl Default for Config {
	fn default() -> Self {
		Self::new()
	}
}

/// Keeps the publish task alive: the task holds only a `Weak` to this, so it
/// exits once the last [`Producer`] clone drops.
struct Keepalive;

/// Publishes a [`Registry`]'s counters as a stats broadcast. Cheap to clone.
///
/// [`Producer::new`] builds the registry itself (wiring the config's prefix as
/// its exclude prefix) and spawns the publish task; hand sessions tier-scoped
/// handles via [`Registry::tier`] on [`Producer::registry`]. The task drains
/// the registry every interval and writes a frame per changed track, running
/// until the last [`Producer`] clone is dropped.
#[derive(Clone)]
pub struct Producer {
	registry: Registry,
	/// `None` for a no-op producer (config had no origin): no task was spawned
	/// and the registry is disabled.
	_keepalive: Option<Arc<Keepalive>>,
}

impl Producer {
	/// Build a producer from `config`.
	///
	/// When `config` has an origin, this spawns the publish task immediately
	/// and announces the stats broadcast; the task runs until the last
	/// [`Producer`] clone is dropped. With no origin the producer is a no-op
	/// (its registry is disabled, nothing is published) and no task spawns, so
	/// it's safe to build outside an async runtime.
	pub fn new(config: Config) -> Self {
		let Config {
			origin,
			prefix,
			node,
			interval,
			depth,
			linger,
		} = config;
		// An empty node is indistinguishable from "no node set"; collapse it so
		// downstream code only sees a single representation. We do this here
		// (not in `with_node`) so a directly-assigned `config.node` is covered too.
		let node = node.filter(|p| !p.is_empty());

		let Some(origin) = origin else {
			return Self {
				registry: Registry::disabled(),
				_keepalive: None,
			};
		};

		// The prefix is a literal path, so its subtree claim cannot fail.
		let exclude = moq_net::Pattern::subtree(prefix.as_str()).expect("the stats prefix is a literal path");
		let registry = Registry::new(moq_net::stats::Config::new().with_exclude(exclude));
		let keepalive = Arc::new(Keepalive);
		let task = Task {
			registry: registry.clone(),
			origin,
			prefix,
			node,
			depth,
			linger,
			interval,
		};
		spawn(task.run(Arc::downgrade(&keepalive)));

		Self {
			registry,
			_keepalive: Some(keepalive),
		}
	}

	/// The registry this producer drains. Hand sessions tier-scoped handles via
	/// [`Registry::tier`]; read node totals back with [`Registry::snapshot`].
	/// Disabled (all bumps no-op) for a no-op producer.
	pub fn registry(&self) -> &Registry {
		&self.registry
	}
}

/// Everything the publish task owns.
struct Task {
	registry: Registry,
	origin: origin::Producer,
	prefix: Path,
	node: Option<Path>,
	depth: usize,
	linger: Duration,
	interval: Duration,
}

impl Task {
	/// Publishes the stats broadcast and writes a frame per drain. Runs until
	/// every [`Producer`] clone is dropped (`weak.upgrade()` returns `None`).
	async fn run(self, weak: Weak<Keepalive>) {
		let interval = self.interval;
		let Some(mut drain) = Drain::new(self) else {
			return;
		};

		let mut ticker = moq_clock::interval(interval);
		ticker.set_missed_tick_behavior(moq_clock::MissedTickBehavior::Delay);

		loop {
			// Scheduled instants are at least an interval apart, even after a late tick,
			// so each drain gets an interval of its own to open its groups in.
			let scheduled = ticker.tick().await;

			if weak.upgrade().is_none() {
				drain.finish();
				return;
			}

			drain.collect();
			drain.publish(scheduled);
		}
	}
}

/// The publish task's state, kept across drains so a steady-state drain
/// reuses its buffers instead of allocating per entry.
struct Drain {
	task: Task,
	node: NodePublisher,
	/// Refilled by every drain.
	report: Report,
	/// Drain counter, stamped on the change-detection state an entry touches
	/// so state the report no longer carries can be dropped.
	tick: u64,
	/// An instant and its timestamp, which group numbers count intervals from.
	base: (Instant, Duration),
}

/// One drain's instant and the publish interval it falls in, which numbers every
/// group the drain opens. Counted at full resolution, so drains at least an interval
/// apart never share a number.
#[derive(Clone, Copy, Debug)]
struct Stamp {
	/// Stamps the drain's frames.
	at: Instant,
	/// The drain's interval since the timestamp epoch.
	group: u64,
}

impl Drain {
	/// Announce the node's broadcast, which lives for the producer's life;
	/// `None` if the origin refused it.
	fn new(task: Task) -> Option<Self> {
		let node = NodePublisher::create(&task)?;
		let now = moq_clock::now();
		Some(Self {
			task,
			node,
			report: Report::default(),
			tick: 0,
			base: (now, Duration::from(moq_net::Timestamp::from(now))),
		})
	}

	/// Stamp a drain at `at`, numbering it by the interval it falls in.
	fn stamp(&self, at: Instant) -> Stamp {
		let (instant, timestamp) = self.base;
		let elapsed = timestamp + at.saturating_duration_since(instant);
		Stamp {
			at,
			group: (elapsed.as_nanos() / self.task.interval.as_nanos()) as u64,
		}
	}

	/// Drain the registry and sort each entry into its pending frames,
	/// creating group tracks on first sight.
	fn collect(&mut self) {
		self.task.registry.report(&mut self.report);
		self.tick += 1;
		self.node.collect(&self.report, self.tick, self.task.depth);
	}

	/// Write every pending frame, end group tracks that have lingered empty,
	/// and serve consumer track requests.
	fn publish(&mut self, now: Instant) {
		let stamp = self.stamp(now);
		self.node.flush(self.tick, stamp, self.task.depth, self.task.linger);
		// A label's presence track and a prefix's track exist only while
		// requested. See `serve_requests`.
		self.node.serve_requests(&self.report, self.tick, stamp);
	}

	fn finish(self) {
		self.node.finish();
	}

	/// The largest group snapshot still held, and the largest group frame this
	/// tick wrote.
	#[cfg(feature = "bench")]
	fn group_frame_bytes(&self) -> bench::FrameBytes {
		let mut bytes = bench::FrameBytes::default();
		for track in self.node.groups.values() {
			bytes.held = bytes.held.max(track.snapshot.held);
			bytes.wrote = bytes.wrote.max(track.snapshot.wrote);
		}
		bytes
	}
}

/// Drives one producer tick for the benchmark. Not a public API: compiled only
/// with the `bench` feature, which nothing enables by default.
#[cfg(feature = "bench")]
#[doc(hidden)]
#[path = "produce_bench.rs"]
pub mod bench;

/// One track's frame, rebuilt every drain in a buffer kept across drains.
/// Serializes as a JSON object keyed by path or tier label, byte-identical to
/// the matching [`TotalsFrame`](crate::TotalsFrame),
/// [`PresenceFrame`](crate::PresenceFrame), or
/// [`PrefixFrame`](crate::PrefixFrame) once sorted.
struct Frame<V> {
	entries: Vec<(Path, V)>,
}

impl<V> Default for Frame<V> {
	fn default() -> Self {
		Self { entries: Vec::new() }
	}
}

impl<V: Serialize> Serialize for Frame<V> {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_map(self.entries.iter().map(|(path, value)| (path.as_str(), value)))
	}
}

/// Writes encoded snapshots, numbering each group by the publish interval it
/// opens in (see [`Stamp`]). A recreated track (an
/// on-demand track asked for again, or a group returning after its linger)
/// opens in a later interval than its predecessor's last group, so it resumes
/// past any group a subscriber has cached, with nothing kept for names that
/// left. A track opens at most one group per interval: a snapshot due in an
/// interval that already has one waits for the next.
///
/// The numbering is why this wraps the encoder rather than a
/// `moq_json::snapshot::Producer`, which numbers its own groups.
struct Snapshot<T> {
	track: track::Producer,
	encoder: moq_json::snapshot::Encoder<T>,
	group: Option<moq_net::group::Producer>,
	/// The sequence of the newest group written, which a refresh has to pass.
	largest: Option<u64>,
	deltas: bool,
	/// The newest snapshot payload written, and what the last update wrote
	/// (zero when nothing). Bench builds only.
	#[cfg(feature = "bench")]
	held: usize,
	#[cfg(feature = "bench")]
	wrote: usize,
}

impl<T: Serialize> Snapshot<T> {
	fn new(track: track::Producer, config: moq_json::snapshot::Config) -> Self {
		Self {
			track,
			deltas: config.deltas,
			encoder: moq_json::snapshot::Encoder::new(config),
			group: None,
			largest: None,
			#[cfg(feature = "bench")]
			held: 0,
			#[cfg(feature = "bench")]
			wrote: 0,
		}
	}

	fn demand(&self) -> moq_net::track::Demand {
		self.track.demand()
	}

	/// Write one frame, stamped and numbered by its drain. `Some(len)` is the payload
	/// written; `None` means the value was unchanged, the interval already has a group for a
	/// snapshot, or the track already ended, and nothing was written.
	///
	/// A frame over the cache limit aborts the track with
	/// [`moq_net::Error::FrameTooLarge`]: skipping it would leave readers on the
	/// previous snapshot with no sign it stopped tracking. A compressed track
	/// holds a frame to the same limit by its uncompressed size.
	fn update(&mut self, value: &T, stamp: Stamp) -> moq_json::Result<Option<usize>> {
		#[cfg(feature = "bench")]
		{
			self.wrote = 0;
		}
		if self.track.is_closed() {
			return Ok(None);
		}
		// A joiner wants a group past the newest, so open this tick's with a snapshot, even of an
		// unchanged value. Answered once per tick rather than at once: a joiner waits at most one
		// interval, the same as for any change.
		if let (Some(wanted), Some(largest)) = (self.track.demand().wanted(), self.largest)
			&& wanted > largest
		{
			self.encoder.reset();
		}
		let frame = match self.encoder.update(value) {
			Ok(Some(frame)) if frame.payload.len() as u64 <= moq_net::group::MAX_CACHE_BYTES => frame,
			Ok(None) => return Ok(None),
			Ok(Some(_)) | Err(moq_json::Error::Net(moq_net::Error::FrameTooLarge)) => {
				let err = moq_net::Error::FrameTooLarge;
				if let Some(group) = self.group.take() {
					let _ = group.abort(err.clone());
				}
				let _ = self.track.clone().abort(err.clone());
				return Err(err.into());
			}
			Err(err) => return Err(err),
		};
		let len = frame.payload.len();
		let timestamp = moq_net::Timestamp::from(stamp.at);
		if frame.keyframe {
			let sequence = stamp.group;
			// The interval already has its group: the uncommitted frame is encoded again next tick.
			if self.largest.is_some_and(|largest| sequence <= largest) {
				return Ok(None);
			}
			if let Some(mut group) = self.group.take() {
				group.finish()?;
			}
			self.group = Some(self.track.create_group(moq_net::group::Info { sequence })?);
			self.largest = Some(sequence);
			#[cfg(feature = "bench")]
			{
				self.held = len;
			}
		}
		let group = self.group.as_mut().expect("a delta follows a snapshot");
		if let Err(err) = group.write_frame(timestamp, frame.payload.clone()) {
			let _ = self.group.take().unwrap().finish();
			return Err(err.into());
		}
		if !self.deltas {
			self.group.take().unwrap().finish()?;
		}
		frame.commit();
		#[cfg(feature = "bench")]
		{
			self.wrote = len;
		}
		Ok(Some(len))
	}

	fn finish(&mut self) -> moq_json::Result<()> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		Ok(self.track.finish()?)
	}
}

/// A stats track and the frame it publishes each drain.
struct Output<V> {
	snapshot: Snapshot<Frame<V>>,
	/// This drain's entries, published and cleared by [`Self::publish`].
	frame: Frame<V>,
}

/// Stats frames are stamped with the wall clock, so their tracks are timed.
/// They are compressed and keep merge-patch deltas inside the track's own
/// DEFLATE window, so a joiner asks for a fresh group once the deltas outweigh
/// eight snapshots.
fn info() -> track::Info {
	track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_compress(true)
		.with_refresh(8)
}

impl<V: Serialize> Output<V> {
	fn create(broadcast: &broadcast::Producer, name: &str) -> Result<Self, moq_net::Error> {
		Ok(Self::new(broadcast.create_track(name, info())?))
	}

	/// Publish on the track `request` asked for, or create `name` without one.
	fn adopt(
		broadcast: &broadcast::Producer,
		name: &str,
		request: Option<track::Request>,
	) -> Result<Self, moq_net::Error> {
		match request {
			Some(request) => Ok(Self::new(request.accept(info()))),
			None => Self::create(broadcast, name),
		}
	}

	fn new(track: track::Producer) -> Self {
		Self {
			snapshot: Snapshot::new(track, moq_json::snapshot::Config::default()),
			frame: Frame::default(),
		}
	}

	/// Whether any consumer exists.
	fn is_used(&self) -> bool {
		self.snapshot.demand().wanted().is_some()
	}

	/// Publish this drain's entries (`{}` when there are none) and clear them
	/// for the next drain; moq-json skips unchanged values.
	fn publish(&mut self, name: &str, stamp: Stamp) {
		self.frame.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		if let Err(err) = self.snapshot.update(&self.frame, stamp) {
			tracing::warn!(?err, name, "stats: failed to write frame");
		}
		self.frame.entries.clear();
	}

	/// Finish the track, so dropping it is a deliberate end instead of a
	/// dropped-without-finish warning. An error means the track already ended;
	/// there is nothing left to close.
	fn finish(&mut self) {
		let _ = self.snapshot.finish();
	}
}

/// The node's stats broadcast and its change-detection state.
struct NodePublisher {
	broadcast: origin::Publication,
	/// Holds the broadcast's request queue open, so a subscriber asking for a
	/// presence or prefix track parks until the next drain serves it instead
	/// of being rejected `NotFound` on the spot.
	dynamic: broadcast::Requests,
	local: HashMap<Path, HashMap<Tier, SideSlots>>,
	totals: TotalsTrack,
	/// Held presence tracks, keyed by stats label.
	presence: HashMap<Path, PresenceTrack>,
	/// Held prefix tracks, keyed by broadcast prefix. A `String` key, so a
	/// drain looks each row's ancestors up by `&str` without allocating.
	prefixes: HashMap<String, PrefixTrack>,
	/// Each live group's track, keyed by group.
	groups: HashMap<String, GroupTrack>,
}

/// One cumulative value per tier: the readouts of entries that ended, plus
/// this drain's live ones. Folding each closing readout is what keeps an
/// ended entry's counters in every later frame.
struct Cumulative<V> {
	tiers: HashMap<Tier, Sum<V>>,
}

/// One tier's [`Cumulative`] value.
#[derive(Default)]
struct Sum<V> {
	/// The closing readouts of entries that ended.
	folded: V,
	/// `folded` plus this drain's live readouts.
	current: V,
}

impl<V: Copy> Sum<V> {
	/// Start a drain: the value is its folded part until the drain's rows are
	/// added.
	fn reset(&mut self) {
		self.current = self.folded;
	}

	/// Add one readout, folding it for good when it is `closed`.
	fn add(&mut self, closed: bool, add: impl Fn(&mut V)) {
		add(&mut self.current);
		if closed {
			add(&mut self.folded);
		}
	}
}

impl<V: Copy + Default> Cumulative<V> {
	fn new() -> Self {
		Self { tiers: HashMap::new() }
	}

	/// Start a drain: every tier's value is its folded part until the drain's
	/// rows are added.
	fn reset(&mut self) {
		for sum in self.tiers.values_mut() {
			sum.reset();
		}
	}

	/// Add one readout to `tier`, folding it for good when it is `closed`.
	fn add(&mut self, tier: &Tier, closed: bool, add: impl Fn(&mut V)) {
		tier_entry(&mut self.tiers, tier).add(closed, add);
	}

	/// Fill `frame` with each tier's current value.
	fn fill(&self, frame: &mut Frame<V>) {
		frame
			.entries
			.extend(self.tiers.iter().map(|(tier, sum)| (tier.label().clone(), sum.current)));
	}
}

/// `tier`'s value in `tiers`, inserted on first sight. Looks up before
/// inserting, so a steady drain never clones a tier.
fn tier_entry<'a, V: Default>(tiers: &'a mut HashMap<Tier, V>, tier: &Tier) -> &'a mut V {
	if !tiers.contains_key(tier) {
		tiers.insert(tier.clone(), V::default());
	}
	tiers.get_mut(tier).expect("just ensured")
}

/// The always-present `totals.json` track and its per-tier sums. Never pruned
/// while the broadcast lives, so it is cumulative within the epoch.
struct TotalsTrack {
	output: Output<Totals>,
	sums: Cumulative<Totals>,
}

/// One stats label's requested `presence.json` track and its per-tier sums,
/// cumulative from when it was first held. Reclaimed once nobody reads it.
struct PresenceTrack {
	/// The track name, for logs.
	name: String,
	output: Output<Presence>,
	sums: Cumulative<Presence>,
	/// Wire collection starts at the current reading on every hold. Other presence
	/// fields keep their existing live-session seeding behavior.
	wire_start: HashMap<Path, (u64, u64)>,
}

impl PresenceTrack {
	fn publish(&mut self, stamp: Stamp) {
		self.sums.fill(&mut self.output.frame);
		for (tier, presence) in &mut self.output.frame.entries {
			if let Some(&(tx, rx)) = self.wire_start.get(tier) {
				presence.wire_tx = presence.wire_tx.saturating_sub(tx);
				presence.wire_rx = presence.wire_rx.saturating_sub(rx);
			}
		}
		self.output.publish(&self.name, stamp);
	}
}

/// One broadcast prefix's requested `prefix.json` track and its per-tier sums,
/// cumulative from when it was first held. Reclaimed once nobody reads it.
struct PrefixTrack {
	/// The track name, for logs.
	name: String,
	output: Output<Prefix>,
	tiers: HashMap<Tier, PrefixSums>,
}

/// One tier's sums on a [`PrefixTrack`].
#[derive(Default)]
struct PrefixSums {
	/// Every path at or under the prefix.
	rollup: Sum<Usage>,
	/// The path at exactly the prefix.
	own: Sum<Usage>,
	/// Keyed by the child's next path segment. A child is dropped on the first
	/// drain with no row under it, so its folded tail stays only in `rollup`.
	children: HashMap<String, ChildSum>,
}

#[derive(Default)]
struct ChildSum {
	sum: Sum<Usage>,
	/// The last drain that carried a row under this child.
	seen: u64,
}

impl PrefixTrack {
	fn reset(&mut self) {
		for sums in self.tiers.values_mut() {
			sums.rollup.reset();
			sums.own.reset();
			for child in sums.children.values_mut() {
				child.sum.reset();
			}
		}
	}

	/// Add one row at or under the prefix: at it when `child` is `None`, else
	/// under that direct child.
	fn add(&mut self, entry: &TrafficEntry, child: Option<&str>, tick: u64) {
		let usage = Usage {
			publisher: entry.publisher,
			subscriber: entry.subscriber,
		};
		let add = |sum: &mut Usage| sum.add(usage);
		let sums = tier_entry(&mut self.tiers, &entry.tier);
		sums.rollup.add(entry.closed, add);
		let Some(child) = child else {
			sums.own.add(entry.closed, add);
			return;
		};
		if !sums.children.contains_key(child) {
			sums.children.insert(child.to_string(), ChildSum::default());
		}
		let state = sums.children.get_mut(child).expect("just ensured");
		state.seen = tick;
		state.sum.add(entry.closed, add);
	}

	/// Publish each tier's sums, dropping children this drain carried no row
	/// for.
	fn publish(&mut self, tick: u64, stamp: Stamp) {
		for (tier, sums) in self.tiers.iter_mut() {
			sums.children.retain(|_, child| child.seen == tick);
			self.output.frame.entries.push((
				tier.label().clone(),
				Prefix {
					publisher: sums.rollup.current.publisher,
					subscriber: sums.rollup.current.subscriber,
					own: sums.own.current,
					children: sums
						.children
						.iter()
						.map(|(segment, child)| (segment.clone(), child.sum.current))
						.collect(),
				},
			));
		}
		self.output.publish(&self.name, stamp);
	}
}

/// One group's track: listed from the group's first entry until it lingers
/// empty, then finished. A return after that is a new instance under a new
/// epoch, counted from zero, so it holds no state for groups that left.
struct GroupTrack {
	/// The track name, for logs.
	name: String,
	snapshot: Snapshot<GroupBody>,
	/// This drain's frame, refilled in place every drain.
	body: GroupBody,
	/// Whether this drain carried a row for the group.
	live: bool,
	/// When the group's last entry left; `None` while it has entries.
	empty_since: Option<Instant>,
}

/// A group track's frame, byte-identical to a [`GroupFrame`](crate::GroupFrame)
/// once sorted.
#[serde_with::serde_as]
#[derive(Serialize)]
struct GroupBody {
	#[serde_as(as = "serde_with::DisplayFromStr")]
	epoch: Epoch,
	tiers: Frame<GroupTier>,
}

/// One tier's entry on a group track; serializes as a [`Group`](crate::Group).
#[derive(Default, Serialize)]
struct GroupTier {
	/// The closing readouts of entries that ended this epoch.
	#[serde(skip)]
	folded: Totals,
	/// `folded` plus this drain's live readouts.
	#[serde(flatten)]
	totals: Totals,
	paths: Frame<PathUsage>,
	labels: Frame<Presence>,
}

/// One path's entry on a group track; serializes as a [`Usage`](crate::Usage)
/// without the sides that never moved, which a reader fills with zeros. A
/// relay path usually moves one way, so this halves the snapshot.
#[derive(Serialize)]
struct PathUsage {
	#[serde(skip_serializing_if = "Option::is_none")]
	publisher: Option<Traffic>,
	#[serde(skip_serializing_if = "Option::is_none")]
	subscriber: Option<Traffic>,
}

impl GroupTrack {
	fn create(broadcast: &broadcast::Producer, group: &Path) -> Result<Self, moq_net::Error> {
		let name = crate::group_track(group);
		let track = broadcast.create_track(name.as_str(), info())?;
		Ok(Self {
			name,
			snapshot: Snapshot::new(track, moq_json::snapshot::Config::default()),
			body: GroupBody {
				// A new instance of the name: readers keep its counters apart from
				// an earlier instance's.
				epoch: Epoch::mint(),
				tiers: Frame::default(),
			},
			live: false,
			empty_since: None,
		})
	}

	/// Start a drain: every tier holds its folded totals and no live entries.
	fn reset(&mut self) {
		self.live = false;
		for (_, tier) in &mut self.body.tiers.entries {
			tier.totals = tier.folded;
			tier.paths.entries.clear();
			tier.labels.entries.clear();
		}
	}

	/// `tier`'s entry, inserted on first sight, marking the group live.
	fn tier(&mut self, tier: &Tier) -> &mut GroupTier {
		self.live = true;
		let entries = &mut self.body.tiers.entries;
		let index = match entries.iter().position(|(label, _)| label == tier.label()) {
			Some(index) => index,
			None => {
				entries.push((tier.label().clone(), GroupTier::default()));
				entries.len() - 1
			}
		};
		&mut entries[index].1
	}

	/// Add one traffic row, listing its path with each side that has moved.
	fn add_traffic(&mut self, entry: &TrafficEntry, publisher: bool, subscriber: bool) {
		let tier = self.tier(&entry.tier);
		let add = |totals: &mut Totals| {
			totals.publisher.add(entry.publisher);
			totals.subscriber.add(entry.subscriber);
		};
		add(&mut tier.totals);
		if entry.closed {
			add(&mut tier.folded);
		}
		if publisher || subscriber {
			tier.paths.entries.push((
				entry.path.clone(),
				PathUsage {
					publisher: publisher.then_some(entry.publisher),
					subscriber: subscriber.then_some(entry.subscriber),
				},
			));
		}
	}

	/// Add one sessions row. The registry reports a label only while a session
	/// or request holds its counters, plus its final readout, so every row is
	/// listed: a label held by an idle request must not drop out and return
	/// looking reset.
	fn add_sessions(&mut self, label: &Path, tier: &Tier, presence: Presence, closed: bool) {
		let tier = self.tier(tier);
		tier.totals.sessions.add(presence);
		if closed {
			tier.folded.sessions.add(presence);
		}
		tier.labels.entries.push((label.clone(), presence));
	}

	/// Whether this drain left the group empty for at least `linger`.
	fn lingered(&mut self, now: Instant, linger: Duration) -> bool {
		if self.live {
			self.empty_since = None;
			return false;
		}
		let since = *self.empty_since.get_or_insert(now);
		now.saturating_duration_since(since) >= linger
	}

	/// Write this drain's frame; moq-json skips an unchanged one. A frame too
	/// large to cache ends this group's track alone.
	fn publish(&mut self, stamp: Stamp) {
		let tiers = &mut self.body.tiers.entries;
		tiers.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		for (_, tier) in tiers.iter_mut() {
			tier.paths.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
			tier.labels.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		}
		if let Err(err) = self.snapshot.update(&self.body, stamp) {
			tracing::warn!(?err, name = self.name, "stats: failed to write group frame");
		}
	}

	/// Finish the track, a deliberate end that also unlists it.
	fn finish(mut self) {
		let _ = self.snapshot.finish();
	}
}

/// Each prefix of `path` from the root down to `path` itself, paired with the
/// next segment below it (`None` at `path`).
fn ancestors(path: &str) -> impl Iterator<Item = (&str, Option<&str>)> {
	let mut at = Some(0);
	std::iter::from_fn(move || {
		let end = at?;
		if end == path.len() {
			at = None;
			return Some((path, None));
		}
		let start = if end == 0 { 0 } else { end + 1 };
		let next = path[start..].find('/').map_or(path.len(), |i| start + i);
		at = Some(next);
		Some((&path[..end], Some(&path[start..next])))
	})
}

impl NodePublisher {
	/// Announce the node's broadcast with its totals track and, at depth 0, the
	/// one group's track, which both live for the producer's life.
	fn create(task: &Task) -> Option<Self> {
		let advertised = advertised_path(&task.prefix, task.node.as_ref())
			.inspect_err(|err| tracing::warn!(%err, "stats: node path too long"))
			.ok()?;
		// A fresh epoch per announcement: a restart is a new broadcast, so no relay
		// resumes it from groups cached under the old one.
		let route = origin::Route::default().with_epoch(Epoch::mint());
		let broadcast = match task.origin.publish(&advertised, route) {
			Ok(broadcast) => broadcast,
			Err(err) => {
				tracing::warn!(advertised = %advertised, ?err, "stats: origin rejected stats broadcast");
				return None;
			}
		};
		tracing::debug!(advertised = %advertised, "stats: publishing broadcast");

		let name = totals_track();
		let totals = match Output::create(&broadcast, &name) {
			Ok(output) => TotalsTrack {
				output,
				sums: Cumulative::new(),
			},
			Err(err) => {
				tracing::warn!(?err, name, "stats: failed to create track");
				return None;
			}
		};

		let mut groups = HashMap::new();
		if task.depth == 0 {
			match GroupTrack::create(&broadcast, &Path::empty()) {
				Ok(track) => {
					groups.insert(String::new(), track);
				}
				Err(err) => {
					tracing::warn!(?err, "stats: failed to create group track");
					return None;
				}
			}
		}

		let dynamic = broadcast.requests();

		Some(Self {
			broadcast,
			dynamic,
			local: HashMap::new(),
			totals,
			presence: HashMap::new(),
			prefixes: HashMap::new(),
			groups,
		})
	}

	/// Run this drain's rows through change detection into the pending frames,
	/// and add them to the totals and every held presence and prefix track.
	fn collect(&mut self, report: &Report, tick: u64, depth: usize) {
		let Self {
			broadcast,
			local,
			totals,
			presence,
			prefixes,
			groups,
			..
		} = self;

		// Each row looks its ancestors up in the held set, so the cost is rows
		// times path depth, and nothing when no prefix is held.
		if !prefixes.is_empty() {
			for track in prefixes.values_mut() {
				track.reset();
			}
			for entry in &report.traffic {
				for (prefix, child) in ancestors(entry.path.as_str()) {
					if let Some(track) = prefixes.get_mut(prefix) {
						track.add(entry, child, tick);
					}
				}
			}
		}

		totals.sums.reset();
		for entry in &report.traffic {
			totals.sums.add(&entry.tier, entry.closed, |sum| {
				sum.publisher.add(entry.publisher);
				sum.subscriber.add(entry.subscriber);
			});
		}
		for track in presence.values_mut() {
			track.sums.reset();
		}
		for entry in &report.sessions {
			totals
				.sums
				.add(&entry.tier, entry.closed, |sum| sum.sessions.add(entry.presence));
			if let Some(track) = presence.get_mut(&entry.label) {
				track.sums.add(&entry.tier, entry.closed, |sum| sum.add(entry.presence));
			}
		}

		for track in groups.values_mut() {
			track.reset();
		}
		for entry in &report.traffic {
			let slots = local
				.entry(entry.path.clone())
				.or_default()
				.entry(entry.tier.clone())
				.or_default();
			slots.seen = tick;
			let publisher = slots.publisher.observe(entry.publisher);
			let subscriber = slots.subscriber.observe(entry.subscriber);
			let key = group_key(&entry.path, depth);
			if let Some(track) = Self::group(broadcast, groups, &key) {
				track.add_traffic(entry, publisher, subscriber);
			}
		}
		for entry in &report.sessions {
			let key = group_key(&entry.label, depth);
			if let Some(track) = Self::group(broadcast, groups, &key) {
				track.add_sessions(&entry.label, &entry.tier, entry.presence, entry.closed);
			}
		}
	}

	/// Get or create the track for group `key`, `None` if the broadcast refused it.
	fn group<'a>(
		broadcast: &broadcast::Producer,
		groups: &'a mut HashMap<String, GroupTrack>,
		key: &Path,
	) -> Option<&'a mut GroupTrack> {
		if !groups.contains_key(key.as_str()) {
			match GroupTrack::create(broadcast, key) {
				Ok(track) => {
					groups.insert(key.to_string(), track);
				}
				Err(err) => {
					tracing::warn!(?err, group = %key, "stats: failed to create group track");
					return None;
				}
			}
		}
		groups.get_mut(key.as_str())
	}

	/// End group tracks that lingered empty, publish the pending frames, then
	/// drop change-detection state for entries this drain did not carry (the
	/// registry pruned them).
	fn flush(&mut self, tick: u64, stamp: Stamp, depth: usize, linger: Duration) {
		// At depth 0 the one group's track lives for the producer's life.
		for (_, track) in self
			.groups
			.extract_if(|_, track| depth > 0 && track.lingered(stamp.at, linger))
		{
			track.finish();
		}
		for track in self.groups.values_mut() {
			track.publish(stamp);
		}

		self.totals.sums.fill(&mut self.totals.output.frame);
		self.totals.output.publish(crate::TOTALS, stamp);
		for track in self.presence.values_mut() {
			track.publish(stamp);
		}
		for track in self.prefixes.values_mut() {
			track.publish(tick, stamp);
		}

		self.local.retain(|_, tiers| {
			tiers.retain(|_, slots| slots.seen == tick);
			!tiers.is_empty()
		});
	}

	/// Serve consumer requests for presence and prefix tracks.
	///
	/// A presence track is held only while requested, for any stats label, and
	/// answered at once with the label's current counters from `report`. It is
	/// reclaimed once its last consumer leaves, so the held set follows live
	/// readers. A prefix track follows the same rules for a broadcast prefix.
	/// Any other name is rejected: a group's track is served only while it is
	/// listed.
	fn serve_requests(&mut self, report: &Report, tick: u64, stamp: Stamp) {
		// Reclaim before adopting, so a request for a track just reclaimed is
		// served again by this very drain.
		self.presence.retain(|_, track| {
			if track.output.is_used() {
				return true;
			}
			track.output.finish();
			false
		});
		self.prefixes.retain(|_, track| {
			if track.output.is_used() {
				return true;
			}
			track.output.finish();
			false
		});

		// Pop everything queued, keeping one request per name so each track is
		// built from an actual request.
		let mut labels: HashMap<Path, track::Request> = HashMap::new();
		let mut paths: HashMap<Path, track::Request> = HashMap::new();
		let noop = kio::Waiter::noop();
		while let Poll::Ready(Ok(request)) = self.dynamic.poll_requested_track(&noop) {
			let keyed = match parse_presence_track(request.name()) {
				Some(label) => Some((&mut labels, label)),
				None => parse_prefix_track(request.name())
					// No broadcast path is deeper than `MAX_PARTS`.
					.filter(|path| path.parts().count() <= Path::MAX_PARTS)
					.map(|path| (&mut paths, path)),
			};
			let Some((held, key)) = keyed else {
				request.reject(moq_net::Error::NotFound);
				continue;
			};
			// A duplicate is dropped into a retry, which finds the live track.
			held.entry(key).or_insert(request);
		}

		for (label, request) in labels {
			self.adopt_presence(report, label, Some(request), stamp);
		}
		for (path, request) in paths {
			self.adopt_prefix(report, path, Some(request), tick, stamp);
		}
	}

	/// Hold a requested label's presence track, publishing its current counters
	/// so the subscription resolves at once. Cumulative from here on.
	fn adopt_presence(&mut self, report: &Report, label: Path, request: Option<track::Request>, stamp: Stamp) {
		// A request for a live track is served by the broadcast itself, so a
		// live one here means it ended and cannot update.
		if self.presence.contains_key(&label) {
			if let Some(request) = request {
				request.reject(moq_net::Error::NotFound);
			}
			return;
		}
		let name = presence_track(&label);
		let output = match Output::adopt(&self.broadcast, &name, request) {
			Ok(output) => output,
			Err(err) => {
				tracing::warn!(?err, name, "stats: failed to adopt presence track");
				return;
			}
		};
		let mut track = PresenceTrack {
			name,
			output,
			sums: Cumulative::new(),
			wire_start: HashMap::new(),
		};
		for entry in report.sessions.iter().filter(|entry| entry.label == label) {
			track.sums.add(&entry.tier, entry.closed, |sum| sum.add(entry.presence));
			let start = track.wire_start.entry(entry.tier.label().clone()).or_default();
			start.0 += entry.presence.wire_tx;
			start.1 += entry.presence.wire_rx;
		}
		track.publish(stamp);
		self.presence.insert(label, track);
	}

	/// Hold a requested prefix track, publishing its current counters so the
	/// subscription resolves at once. Cumulative from here on.
	fn adopt_prefix(&mut self, report: &Report, path: Path, request: Option<track::Request>, tick: u64, stamp: Stamp) {
		// Defensive only, as in `adopt_presence`.
		if self.prefixes.contains_key(path.as_str()) {
			if let Some(request) = request {
				request.reject(moq_net::Error::NotFound);
			}
			return;
		}
		let name = prefix_track(&path);
		let output = match Output::adopt(&self.broadcast, &name, request) {
			Ok(output) => output,
			Err(err) => {
				tracing::warn!(?err, name, "stats: failed to adopt prefix track");
				return;
			}
		};
		let mut track = PrefixTrack {
			name,
			output,
			tiers: HashMap::new(),
		};
		for entry in &report.traffic {
			let Some(rest) = entry.path.strip_prefix(&path) else {
				continue;
			};
			let child = rest.as_str().split('/').next().filter(|child| !child.is_empty());
			track.add(entry, child, tick);
		}
		track.publish(tick, stamp);
		self.prefixes.insert(path.as_str().to_string(), track);
	}

	/// Deliberately end the broadcast: finish every track, then close the broadcast.
	fn finish(mut self) {
		self.totals.output.finish();
		for track in self.presence.values_mut() {
			track.output.finish();
		}
		for track in self.prefixes.values_mut() {
			track.output.finish();
		}
		for (_, track) in self.groups.drain() {
			track.finish();
		}
		self.broadcast.close();
	}
}

/// Emission state for one `(path, tier, side)` slot, owned by the publish
/// task. The task is single-threaded so this needs no atomics.
#[derive(Default)]
struct SlotState {
	/// Whether any counter on this side has moved since the registry created
	/// the entry.
	moved: bool,
}

impl SlotState {
	/// Note this drain's readout, returning whether the side has ever moved.
	///
	/// A side can go idle while its entry lives on: the last viewer leaves but
	/// the publisher still holds the path, so the egress counters are kept and
	/// resume where they stopped. Omitting it would make its return look like
	/// a fresh entry to a reader diffing frames, and the old total would count
	/// twice. So once moved, a path stays in every frame until `flush` drops
	/// its state with the entry, and a path missing from a frame really
	/// restarted. A live side has a started counter above zero, so it has
	/// always moved.
	fn observe(&mut self, snap: Traffic) -> bool {
		self.moved |= snap != Traffic::default();
		self.moved
	}
}

/// Emission state for one `(path, tier)`: a [`SlotState`] per side.
#[derive(Default)]
struct SideSlots {
	publisher: SlotState,
	subscriber: SlotState,
	/// The last drain that reported this `(path, tier)`.
	seen: u64,
}

/// The leading `depth` segments of `path`, the group it publishes under.
fn group_key(path: &Path, depth: usize) -> Path {
	path.head(depth).unwrap_or_else(|| path.clone())
}

/// `<prefix>/node/<node>`, or `<prefix>/node` without a node. The fixed `node`
/// category leaves room for sibling categories (e.g. `<prefix>/cluster` for
/// relay-mesh stats) under the same prefix.
fn advertised_path(prefix: &Path, node: Option<&Path>) -> Result<Path, moq_net::path::Invalid> {
	let path = prefix.join(&Path::from_static("node"))?;
	match node {
		Some(node) => path.join(node),
		None => Ok(path),
	}
}

#[cfg(test)]
mod tests {
	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	/// A drain at the current instant, numbered `group`.
	fn stamp(group: u64) -> Stamp {
		Stamp {
			at: moq_clock::now(),
			group,
		}
	}

	/// A joiner once the deltas outweigh the track's ratio gets a fresh snapshot group in
	/// the next interval, even of an unchanged value, and deltas resume in it.
	#[test]
	fn a_joiner_over_the_ratio_gets_a_snapshot_next_interval() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		// Uncompressed, so the refresh ratio is judged on the plain frame sizes below.
		let track = broadcast.create_track("t.json", info().with_compress(false)).unwrap();
		let mut snapshot = Snapshot::<Frame<u64>>::new(track.clone(), moq_json::snapshot::Config::default());
		let frame = |n| Frame {
			entries: vec![(Path::from_static("a"), n)],
		};
		let wanted = |track: &track::Producer| track.demand().wanted();

		// A 7 byte snapshot and nine 7 byte deltas: past eight snapshots.
		for n in 0..10 {
			snapshot.update(&frame(n), stamp(5)).unwrap();
		}
		assert_eq!(snapshot.largest, Some(5));
		let _late = track.subscribe(None);
		assert_eq!(wanted(&track), Some(6), "the join asks for a fresh group");

		// The interval already has its group, so the snapshot waits for the next one.
		assert!(snapshot.update(&frame(9), stamp(5)).unwrap().is_none());
		assert_eq!(snapshot.largest, Some(5));

		assert!(
			snapshot.update(&frame(9), stamp(6)).unwrap().is_some(),
			"restated though unchanged"
		);
		assert_eq!(snapshot.largest, Some(6), "numbered by its interval");
		assert!(snapshot.update(&frame(10), stamp(6)).unwrap().is_some());
		assert_eq!(snapshot.largest, Some(6), "the next change is a delta");
	}

	/// A relay origin, and the cache pool its copies are billed to.
	fn relay() -> (origin::Producer, moq_net::cache::Pool) {
		let config = origin::Config::default();
		let pool = config.pool.clone();
		let (origin, driver) = origin::Producer::new(config);
		tokio::spawn(moq_net::time::run(driver));
		(origin, pool)
	}

	/// Drive `drain` until `future` resolves.
	async fn pumping<T>(drain: &mut Drain, future: impl std::future::Future<Output = T>) -> T {
		pumping_at(drain, Instant::now(), future).await
	}

	/// Drive `drain` at `now` until `future` resolves, one interval of frame time
	/// per drain, as the publish task would.
	async fn pumping_at<T>(drain: &mut Drain, now: Instant, future: impl std::future::Future<Output = T>) -> T {
		let mut future = std::pin::pin!(future);
		for _ in 0..1000 {
			drain.collect();
			drain.publish(now);
			if let Some(result) = futures::FutureExt::now_or_never(future.as_mut()) {
				return result;
			}
			tokio::time::advance(drain.task.interval).await;
		}
		panic!("protocol request did not settle while pumping the stats producer");
	}

	/// Serve `upstream` to `downstream` over an in-memory `version` session.
	async fn link(
		upstream: &origin::Producer,
		downstream: &origin::Producer,
		version: moq_net::Version,
	) -> [moq_net::Session; 2] {
		let mut config = moq_net::transport::memory::Config::default();
		config.protocol = Some(version.to_string());
		let (client_transport, server_transport) = moq_net::transport::memory::pair(config);
		let client = moq_net::Client::new()
			.with_versions(version.into())
			.with_subscriber(downstream.clone().with_link(origin::Link::Peer));
		let server = moq_net::Server::new()
			.with_versions(version.into())
			.with_publisher(upstream.consume().with_hidden(true));
		let client = async {
			let (session, driver) = client.connect(moq_clock::now(), client_transport).await.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		};
		let server = async {
			let (session, driver) = server.accept(moq_clock::now(), server_transport).await.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		};
		let (a, b) = tokio::join!(client, server);
		[a, b]
	}

	/// Real protocol relays keep their idle track cache while upstream presence
	/// is reclaimed. A re-hold must refresh that cache before exposing any frame.
	#[tokio::test(start_paused = true)]
	async fn reheld_wire_presence_refreshes_two_relay_caches() {
		for version in ["moq-lite-06", "moq-lite-07-wip"].map(|value| value.parse().unwrap()) {
			let registry = Registry::new(moq_net::stats::Config::new());
			let origin = produce_origin();
			let mut drain = Drain::new(Task {
				registry: registry.clone(),
				origin: origin.clone(),
				prefix: moq_net::Path::from_static(".stats"),
				node: None,
				depth: 0,
				linger: DEFAULT_LINGER,
				// Hundreds of drains, each an interval apart, stay inside the relays' cache.
				interval: Duration::from_millis(10),
			})
			.unwrap();
			let (middle, middle_cache) = relay();
			let (edge, edge_cache) = relay();
			let _links = [
				link(&origin, &middle, version).await,
				link(&middle, &edge, version).await,
			];
			let session = registry
				.tier(Tier::default())
				.session(moq_net::Path::from_static("room"));
			let counters = moq_net::transport::Wire::new();
			let _wire = session.wire(counters.clone());
			let store = |n: u64| {
				counters.set_sent_bytes(n);
				counters.set_received_bytes(n * 2);
			};
			store(40);
			drain.collect();
			drain.publish(Instant::now());
			let consumer = edge.consume().with_hidden(true);
			let mut events = consumer.announced();
			let (route, active) = next_update(&mut events).await.unwrap();
			assert!(active);
			assert_eq!(route.prefix.as_str(), ".stats/node");
			let broadcast = consumer.request(".stats/node", None).await.unwrap();
			let name = presence_track(moq_net::Path::from_static("room"));
			let mut previous = None;
			for (reading, delta) in [(40, 11), (1_000, 7)] {
				store(reading);
				let subscriber = pumping(&mut drain, broadcast.track(&name).unwrap().subscribe(None))
					.await
					.unwrap();
				let mut cursor = pumping(&mut drain, broadcast.track(&name).unwrap().subscribe(None))
					.await
					.unwrap();
				let group = pumping(&mut drain, cursor.recv_group()).await.unwrap().unwrap();
				if let Some(old) = previous {
					assert!(group.sequence > old, "re-hold must have a fresh group identity");
				}
				drop(group);
				drop(cursor);
				let mut reader = moq_json::snapshot::Consumer::<PresenceFrame>::new(subscriber, Default::default());
				let initial = pumping(&mut drain, reader.next()).await.unwrap().unwrap().value;
				assert_eq!(
					(initial[""].wire_tx, initial[""].wire_rx),
					(0, 0),
					"a cache exposed an earlier hold's bytes"
				);
				store(reading + delta);
				drain.collect();
				drain.publish(Instant::now());
				let next = pumping(&mut drain, reader.next()).await.unwrap().unwrap().value;
				assert_eq!((next[""].wire_tx, next[""].wire_rx), (delta, delta * 2));
				// Cross the codec's group budget naturally, leaving a completed
				// group cached when unsubscribe aborts the still-open last group.
				for extra in 1..=300 {
					store(reading + delta + extra);
					let next = pumping(&mut drain, reader.next()).await.unwrap().unwrap().value;
					assert_eq!(
						(next[""].wire_tx, next[""].wire_rx),
						(delta + extra, (delta + extra) * 2)
					);
				}
				let mut cursor = pumping(&mut drain, broadcast.track(&name).unwrap().subscribe(None))
					.await
					.unwrap();
				previous = Some(
					pumping(&mut drain, cursor.recv_group())
						.await
						.unwrap()
						.unwrap()
						.sequence,
				);
				drop(cursor);
				drop(reader);
				for _ in 0..100 {
					tokio::task::yield_now().await;
					drain.collect();
					drain.publish(Instant::now());
					if drain.node.presence.is_empty() {
						break;
					}
				}
				assert!(
					drain.node.presence.is_empty(),
					"relays must release upstream demand while retaining their idle cache"
				);
				assert!(
					middle_cache.used() > 0 && edge_cache.used() > 0,
					"idle cache: middle={} edge={}",
					middle_cache.used(),
					edge_cache.used()
				);
			}
			let total = drain.node.totals.sums.tiers[&Tier::default()].current.sessions;
			assert_eq!(
				(total.wire_tx, total.wire_rx),
				(1_307, 2_614),
				"epoch totals never reset on re-hold"
			);
		}
	}

	/// A frame that outgrows the cache ends the track with `FrameTooLarge`
	/// instead of leaving readers on their last snapshot. DEFLATE would shrink
	/// the frame under the limit, but a reader inflates it whole, so the track
	/// holds it to the same budget by its uncompressed size.
	#[tokio::test]
	async fn oversized_frame_ends_the_track() {
		use futures::FutureExt;
		type Value = BTreeMap<String, u64>;

		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut output = Output::<u64>::create(&broadcast, "t.json").unwrap();
		let mut publish = |keys: Vec<Path>, value: u64| {
			output.frame.entries.extend(keys.into_iter().map(|key| (key, value)));
			output.publish("t.json", stamp(value));
		};
		publish(vec![Path::from_static("small")], 1);

		let track = consumer
			.track("t.json")
			.unwrap()
			.subscribe(None)
			.now_or_never()
			.unwrap()
			.unwrap();
		let mut reader = moq_json::snapshot::Consumer::<Value>::new(track, Default::default());
		let mut next = || {
			reader
				.next()
				.now_or_never()
				.unwrap()
				.map(|value| value.map(|value| value.value))
		};
		assert_eq!(next().unwrap(), Some(Value::from([("small".to_string(), 1)])));

		publish(oversized_paths(""), 2);
		assert!(matches!(
			next(),
			Err(moq_json::Error::Net(moq_net::Error::FrameTooLarge))
		));
	}

	/// Distinct paths under `prefix` whose JSON overflows a group's cache, since one
	/// path holds at most [`Path::MAX_BYTES`].
	fn oversized_paths(prefix: &str) -> Vec<Path> {
		let filler = "a".repeat(Path::MAX_BYTES - 16);
		(0..=moq_net::group::MAX_CACHE_BYTES as usize / filler.len())
			.map(|i| Path::new(&format!("{prefix}{i}/{filler}")).unwrap())
			.collect()
	}

	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route) => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	use std::collections::BTreeMap;

	use moq_net::stats::{Registry, Tier};
	use moq_net::{Timestamp, announce, broadcast, track};

	use super::*;
	use crate::{PrefixFrame, PresenceFrame, TotalsFrame};

	fn test_producer(node: Option<&str>) -> (Producer, origin::Producer) {
		let origin = produce_origin();
		let producer = Producer::new(
			Config::new()
				.with_origin(origin.clone())
				.with_node(node.map(|s| Path::try_from(s.to_string()).unwrap())),
		);
		(producer, origin)
	}

	/// Kept-alive handles from [`feed`]: dropping them closes the subscription and the
	/// announce (bumping the `_closed` counters).
	#[allow(dead_code, reason = "held for their drop, never read")]
	struct Feed {
		announced: announce::Consumer,
		source: origin::Publication,
		consumer: broadcast::Consumer,
		sub: Option<track::Subscriber>,
	}

	/// Drive a tagged egress broadcast so `registry` records publisher-side traffic on
	/// `path` under `tier`. The local publisher (ingress) is left untagged, so only the
	/// egress (publisher) counters move, matching how a relay bills read-out traffic.
	///
	/// Announces the broadcast; if `subscribe`, opens one subscription and reads a group
	/// of `frames` frames of `frame_size` bytes each (so `bytes`/`frames`/`groups` move).
	async fn feed(
		registry: &Registry,
		tier: Tier,
		path: &str,
		subscribe: bool,
		frames: usize,
		frame_size: usize,
	) -> Feed {
		let ctx = registry.tier(tier).session(moq_net::Path::from_static("feed"));
		let origin = produce_origin();
		// Egress (publisher side) is tagged; the local publisher stays untagged.
		let egress = origin.consume().with_stats(ctx);

		let mut announced = egress.announced();
		let source = origin.create_broadcast(path).expect("create_broadcast");
		source.announce(origin::Route::default()).expect("announce");
		let producer = source.create_track("video", timed()).expect("create_track");

		let (_, active) = next_update(&mut announced).await.expect("announce");
		assert!(active);
		let consumer = egress.request(path, None).await.expect("resolve");

		let sub = if subscribe {
			let mut sub = consumer
				.track("video")
				.expect("track")
				.subscribe(None)
				.await
				.expect("subscribe");

			if frames > 0 {
				let mut group = producer.append_group().expect("group");
				for _ in 0..frames {
					group
						.write_frame(Timestamp::ZERO, vec![0u8; frame_size])
						.expect("write");
				}
				group.finish().expect("finish");

				let mut group = sub.recv_group().await.expect("recv").expect("group");
				while group.read_frame().await.expect("read").is_some() {}
			}
			Some(sub)
		} else {
			None
		};

		Feed {
			announced,
			source,
			consumer,
			sub,
		}
	}

	/// Awaits the stats announce and returns its broadcast.
	async fn announced(origin: &origin::Producer) -> (String, moq_net::broadcast::Consumer) {
		let mut consumer = origin.consume().with_hidden(true).announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let (update, active) = next_update(&mut consumer).await.expect("expected announce");
		assert!(active);
		let broadcast = origin.consume().request(&update.prefix, None).await.expect("resolve");
		(update.prefix.as_str().to_string(), broadcast)
	}

	/// Advance past one publish interval so the task drains and writes frames.
	async fn drive_tick() {
		tokio::time::advance(Duration::from_millis(1100)).await;
		// Yield several times to let the task wake, drain the registry, write
		// the frames, and re-await the next tick.
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
	}

	/// The latest buffered frame on group `group`'s track.
	async fn read_group(broadcast: &moq_net::broadcast::Consumer, group: &str) -> crate::GroupFrame {
		let track = broadcast
			.track(&crate::group_track(moq_net::Path::new(group).unwrap()))
			.expect("track")
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut reader = moq_json::snapshot::Consumer::<crate::GroupFrame>::new(track, Default::default());
		reader.latest().await.expect("read").expect("frame").value
	}

	/// The default tier's egress per path on the depth-0 group's latest frame.
	async fn read_egress(broadcast: &moq_net::broadcast::Consumer) -> BTreeMap<String, Traffic> {
		let mut frame = read_group(broadcast, "").await;
		let group = frame.tiers.remove("").unwrap_or_default();
		group
			.paths
			.into_iter()
			.map(|(path, usage)| (path, usage.publisher))
			.collect()
	}

	/// A snapshot reader over a stats track, polled without blocking.
	struct Reader<T>(moq_json::snapshot::Consumer<T>);

	impl<T: serde::de::DeserializeOwned + Unpin> Reader<T> {
		fn new(track: track::Subscriber) -> Self {
			Self(moq_json::snapshot::Consumer::new(track, Default::default()))
		}

		/// The next reconstructed state, `None` once nothing more is buffered.
		fn try_next(&mut self) -> Option<T> {
			match self.0.poll_next(&kio::Waiter::noop()) {
				Poll::Ready(Ok(Some(value))) => Some(value.value),
				Poll::Ready(Err(err)) => panic!("read: {err}"),
				Poll::Ready(Ok(None)) | Poll::Pending => None,
			}
		}

		/// The newest buffered state, `None` when nothing is buffered.
		fn try_last(&mut self) -> Option<T> {
			let mut last = None;
			while let Some(value) = self.try_next() {
				last = Some(value);
			}
			last
		}
	}

	async fn reader<T: serde::de::DeserializeOwned + Unpin>(
		broadcast: &moq_net::broadcast::Consumer,
		name: &str,
	) -> Reader<T> {
		Reader::new(
			broadcast
				.track(name)
				.expect("track")
				.subscribe(None)
				.await
				.expect("subscribe"),
		)
	}

	/// The latest buffered totals frame.
	async fn read_totals(broadcast: &moq_net::broadcast::Consumer) -> TotalsFrame {
		let track = broadcast
			.track(crate::TOTALS)
			.expect("track")
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut reader = moq_json::snapshot::Consumer::<TotalsFrame>::new(track, Default::default());
		reader.latest().await.expect("read").expect("frame").value
	}

	/// The default tier's totals in the latest buffered totals frame.
	async fn default_totals(broadcast: &moq_net::broadcast::Consumer) -> Totals {
		read_totals(broadcast).await.get("").copied().unwrap_or_default()
	}

	/// The next presence state off a track, `None` once nothing more is buffered.
	fn try_presence(track: &mut Reader<PresenceFrame>) -> Option<PresenceFrame> {
		track.try_next()
	}

	async fn subscribe(broadcast: &moq_net::broadcast::Consumer, name: &str) -> track::Ordered {
		broadcast
			.track(name)
			.expect("track")
			.subscribe(None)
			.await
			.expect("subscribe")
			.ordered()
	}

	/// The same, without blocking: `None` once nothing more is buffered.
	fn try_next_frame(track: &mut track::Ordered) -> Option<moq_net::frame::Frame> {
		use futures::FutureExt;
		let mut group = track.next_group().now_or_never()?.expect("ok")?;
		group.read_frame().now_or_never()?.expect("ok")
	}

	/// The advertised path appends a multi-segment node and drops an empty
	/// one. Observed through the announced path, since the task announces at
	/// construction.
	#[tokio::test(start_paused = true)]
	async fn new_drops_empty_node() {
		let (_producer, origin) = test_producer(Some("sjc/1"));
		assert_eq!(announced(&origin).await.0, ".stats/node/sjc/1");

		let (_producer, origin) = test_producer(Some(""));
		assert_eq!(announced(&origin).await.0, ".stats/node");
	}

	/// A drain that runs late is numbered by its schedule, so the next on-time
	/// drain still opens its own group and publishes its readout on the plain
	/// track, which writes every frame as a fresh group.
	#[tokio::test(start_paused = true)]
	async fn a_late_drain_leaves_the_next_its_own_interval() {
		// Schedule the drains a millisecond before an interval boundary.
		let phase = Duration::from(Timestamp::from(moq_clock::now())).as_millis() % 1000;
		tokio::time::advance(Duration::from_millis(((1999 - phase) % 1000) as u64)).await;

		let origin = produce_origin();
		let producer = node_producer(&origin);
		let session = producer
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		let counters = moq_net::transport::Wire::new();
		let _wire = session.wire(counters.clone());
		let step = async |millis: u64| {
			tokio::time::advance(Duration::from_millis(millis)).await;
			for _ in 0..4 {
				tokio::task::yield_now().await;
			}
		};
		// The first drain announces the broadcast.
		step(0).await;
		let broadcast = origin
			.consume()
			.request(".stats/node/sjc", None)
			.await
			.expect("resolve");
		let mut totals = reader::<TotalsFrame>(&broadcast, crate::TOTALS).await;
		let mut sent = |bytes: u64| {
			counters.set_sent_bytes(bytes);
			totals
				.try_last()
				.map(|frame| frame.get("").expect("the default tier").sessions.wire_tx)
		};

		sent(1);
		// Two milliseconds late, past the boundary into the next drain's interval.
		step(1002).await;
		assert_eq!(sent(2), Some(1));
		step(998).await;
		assert_eq!(sent(3), Some(2), "the on-time drain publishes its readout");
	}

	/// Group numbers count intervals at full resolution: drains an interval apart
	/// get consecutive numbers whatever the phase, even for an interval that is not
	/// a whole number of the timestamp's milliseconds.
	#[tokio::test(start_paused = true)]
	async fn drains_an_interval_apart_never_share_a_number() {
		let interval = Duration::from_micros(1500);
		let drain = Drain::new(Task {
			registry: Registry::new(moq_net::stats::Config::new()),
			origin: produce_origin(),
			prefix: moq_net::Path::from_static(".stats"),
			node: None,
			depth: 0,
			linger: DEFAULT_LINGER,
			interval,
		})
		.unwrap();
		let start = moq_clock::now();
		for phase in (0..1500).step_by(100) {
			let at = start + Duration::from_micros(phase);
			assert_eq!(
				drain.stamp(at + interval).group,
				drain.stamp(at).group + 1,
				"phase {phase}us"
			);
		}
	}

	/// A producer on `origin` publishing as node `sjc` at depth 0.
	fn node_producer(origin: &origin::Producer) -> Producer {
		Producer::new(
			Config::new()
				.with_origin(origin.clone())
				.with_node(Path::from_static("sjc")),
		)
	}

	/// A producer restarted on the same origin ends its old epoch and announces
	/// the same path under a new one.
	#[tokio::test(start_paused = true)]
	async fn restarted_producer_announces_a_new_epoch() {
		let origin = produce_origin();
		let mut events = origin.consume().with_hidden(true).announced();

		let first = node_producer(&origin);
		drive_tick().await;
		let [(path, true, before)] = &take_epochs(&mut events)[..] else {
			panic!("expected one announce");
		};
		assert_eq!(path, ".stats/node/sjc");

		drop(first);
		drive_tick().await;
		assert_eq!(take_epochs(&mut events), [(path.clone(), false, before.clone())]);

		let _second = node_producer(&origin);
		drive_tick().await;
		let [(after_path, true, after)] = &take_epochs(&mut events)[..] else {
			panic!("expected one announce");
		};
		assert_eq!(after_path, path);
		assert!(after > before, "a restart mints a newer epoch");
	}

	/// A restart while the old instance is still announced (a route the mesh
	/// has not withdrawn yet) takes the path at once: the newer epoch restarts
	/// it, so a reader re-requests the new broadcast instead of stalling on the
	/// old one, while the old subscription stays on the old instance.
	#[tokio::test(start_paused = true)]
	async fn restart_replaces_a_still_announced_instance() {
		let origin = produce_origin();
		let mut events = origin.consume().with_hidden(true).announced();

		let first = node_producer(&origin);
		let _f1 = feed(first.registry(), Tier::default(), "foo/bar", true, 1, 100).await;
		drive_tick().await;
		let [(path, true, before)] = &take_epochs(&mut events)[..] else {
			panic!("expected one announce");
		};
		let stats = origin.consume().request(path.as_str(), None).await.expect("resolve");
		let mut old = subscribe(&stats, &crate::group_track(moq_net::Path::from_static(""))).await;
		while try_next_frame(&mut old).is_some() {}

		let second = node_producer(&origin);
		let _f2 = feed(second.registry(), Tier::default(), "foo/bar", true, 1, 30).await;
		drive_tick().await;
		let [(started, true, after)] = &take_epochs(&mut events)[..] else {
			panic!("expected the new epoch to restart the path");
		};
		assert_eq!(started, path);
		assert!(after > before);
		drop(old);

		let stats = origin.consume().request(path.as_str(), None).await.expect("resolve");
		assert_eq!(read_egress(&stats).await["foo/bar"].bytes, 30);
	}

	#[tokio::test(start_paused = true)]
	async fn frame_emits_expected_counters() {
		let (producer, origin) = test_producer(Some("sjc"));
		// One announced broadcast, one subscription, one 42-byte frame read out.
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_egress(&broadcast).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(
			snap.announces_started, 1,
			"egress announce stream bumps announces_started"
		);
		assert_eq!(snap.broadcasts_started, 1, "one session subscribed");
		assert_eq!(snap.subscriptions_started, 1);
		assert_eq!(snap.bytes, 42);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_bytes_surfaces_in_frame() {
		let (producer, origin) = test_producer(Some("sjc"));
		// Announce only: the guard records the broadcast-name length once on open.
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", false, 0, 0).await;

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_egress(&broadcast).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announces_started, 1);
		assert_eq!(
			snap.announced_bytes,
			"foo/bar".len() as u64,
			"name length recorded on announce"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_decouples_from_broadcasts() {
		// An announce with no subscription should bump announces_started but NOT broadcasts_started
		// (which only counts sessions with an active sub).
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", false, 0, 0).await;

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_egress(&broadcast).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announces_started, 1);
		assert_eq!(snap.broadcasts_started, 0, "no subscription, no broadcasts sentinel");
		assert_eq!(snap.subscriptions_started, 0);
	}

	#[tokio::test(start_paused = true)]
	async fn short_lived_sub_is_surfaced() {
		// A subscription that opens AND closes within a single drain window
		// must still surface as a complete broadcasts start/end cycle. The
		// cumulative counters retain broadcasts_started=1/broadcasts_ended=1, and the
		// entry surfaces because it moved, even though it's net-idle by drain time.
		let (producer, origin) = test_producer(Some("sjc"));
		{
			// Subscribe, read one 123-byte frame, then drop everything within the
			// first interval so the open and close both land before the drain.
			let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 123).await;
		}

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_egress(&broadcast).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		// One session opened then closed a subscription within the drain.
		assert_eq!(snap.subscriptions_started, 1);
		assert_eq!(snap.subscriptions_ended, 1);
		assert_eq!(snap.broadcasts_started, 1, "one session subscribed");
		assert_eq!(snap.broadcasts_ended, 1);
		assert_eq!(snap.bytes, 123);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn a_path_stays_in_the_frame_while_its_counters_live() {
		// The publisher holds `foo/bar` throughout while viewers come and go, so
		// the registry keeps its egress counters between them. A reader diffing
		// frames must see the path the whole time: if it vanished, its return
		// (carrying the first viewer's bytes) would look like a fresh entry.
		let (producer, origin) = test_producer(Some("sjc"));
		let registry = producer.registry();
		let data = produce_origin();
		let source = data
			.clone()
			.with_stats(
				registry
					.tier(Tier::default())
					.session(moq_net::Path::from_static("publisher")),
			)
			.publish("foo/bar", origin::Route::default())
			.expect("publish");
		let mut video = source.create_track("video", timed()).expect("create_track");
		let egress = data.consume().with_stats(
			registry
				.tier(Tier::default())
				.session(moq_net::Path::from_static("viewer")),
		);

		async fn view(egress: &origin::Consumer, video: &mut track::Producer, size: usize) {
			let broadcast = egress.request("foo/bar", None).await.expect("resolve");
			let mut sub = broadcast
				.track("video")
				.expect("track")
				.subscribe(None)
				.await
				.expect("subscribe");
			let mut group = video.append_group().expect("group");
			group.write_frame(Timestamp::ZERO, vec![0u8; size]).expect("write");
			group.finish().expect("finish");
			let mut group = sub.recv_group().await.expect("recv").expect("group");
			while group.read_frame().await.expect("read").is_some() {}
		}

		view(&egress, &mut video, 1000).await;
		let (_, stats) = announced(&origin).await;
		for _ in 0..3 {
			drive_tick().await;
			let frame = read_egress(&stats).await;
			let snap = frame.get("foo/bar").expect("kept while its counters live");
			assert_eq!(snap.bytes, 1000);
			assert!(snap.is_idle(), "the viewer left");
		}

		view(&egress, &mut video, 500).await;
		drive_tick().await;
		let frame = read_egress(&stats).await;
		assert_eq!(frame["foo/bar"].bytes, 1500, "resumed, not restarted");

		// Once nothing holds the path the registry drops it, and so does the frame.
		drop((video, source));
		drive_tick().await;
		drive_tick().await;
		let frame = read_egress(&stats).await;
		assert!(
			!frame.contains_key("foo/bar"),
			"dropped with its counters, got {frame:?}"
		);

		// The totals keep its tail, so a reader that missed every frame while
		// the path lived still reads all of its traffic.
		let totals = default_totals(&stats).await;
		assert_eq!(totals.publisher.bytes, 1500);
		assert_eq!(totals.publisher.subscriptions_started, 2);
		assert_eq!(totals.publisher.active_subscriptions(), 0);
		assert_eq!(totals.subscriber.bytes, 1500, "the publisher's ingress");
		assert_eq!(
			totals.subscriber.announces_started, totals.subscriber.announces_ended,
			"the publisher left"
		);
		assert_eq!(totals.sessions.sessions_started, 2);
		assert_eq!(totals.sessions.active(), 1, "the publisher's session left with it");
	}

	/// A producer publishing each first path segment as a group track of
	/// `.stats/node/sjc`.
	fn grouped_producer(linger: Duration) -> (Producer, origin::Producer) {
		let origin = produce_origin();
		let producer = Producer::new(
			Config::new()
				.with_origin(origin.clone())
				.with_node(Path::from_static("sjc"))
				.with_depth(1)
				.with_linger(linger),
		);
		(producer, origin)
	}

	/// The announce events buffered so far in delivery order, as
	/// `(path, active, epoch)`. Every stats route carries an epoch.
	fn take_epochs(events: &mut announce::Consumer) -> Vec<(String, bool, Epoch)> {
		use futures::FutureExt;
		let mut out = Vec::new();
		while let Some(Some((update, active))) = next_update(events).now_or_never() {
			let epoch = update.route.epoch.expect("a stats route carries an epoch");
			out.push((update.prefix.as_str().to_string(), active, epoch));
		}
		out
	}

	#[tokio::test(start_paused = true)]
	async fn session_track_surfaces_by_root() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _a = producer
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		let _b = producer
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		let _c = producer
			.registry()
			.tier(Tier::new(moq_net::Path::from_static("region/sjc")))
			.session(moq_net::Path::from_static("peer"));

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_group(&broadcast, "").await;
		let labels = &frame.tiers[""].labels;
		let snap = labels.get("acme").expect("label entry");
		assert_eq!(snap.sessions_started, 2);
		assert_eq!(snap.sessions_ended, 0);
		assert!(
			!labels.contains_key("peer"),
			"regional session must not appear on the default tier"
		);

		let snap = frame.tiers["region/sjc"].labels.get("peer").expect("regional entry");
		assert_eq!(snap.sessions_started, 1);
	}

	/// A label held only by requests has no live session, but it stays in every
	/// frame until its last request ends: dropping out would read as a reset, and
	/// the next admission would then count the earlier ones twice.
	#[tokio::test(start_paused = true)]
	async fn request_root_stays_in_the_frame_while_held() {
		let (producer, origin) = test_producer(Some("sjc"));
		let first = producer
			.registry()
			.tier(Tier::default())
			.request(moq_net::Path::from_static("acme"));
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;
		let track = broadcast
			.track(&crate::group_track(moq_net::Path::from_static("")))
			.expect("track")
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut track = moq_json::snapshot::Consumer::<crate::GroupFrame>::new(track, Default::default());
		let drain = |track: &mut moq_json::snapshot::Consumer<crate::GroupFrame>| {
			use futures::FutureExt;
			let mut frames = Vec::new();
			while let Some(frame) = track.next().now_or_never() {
				let frame = frame.expect("read").expect("frame").value;
				let labels = frame.tiers.get("").map(|group| &group.labels);
				frames.push(
					labels
						.and_then(|labels| labels.get("acme"))
						.map(|presence| presence.requests),
				);
			}
			frames
		};
		assert_eq!(drain(&mut track).last(), Some(&Some(1)));

		drive_tick().await;
		drive_tick().await;
		assert!(!drain(&mut track).contains(&None), "an idle request stays");

		let second = producer
			.registry()
			.tier(Tier::default())
			.request(moq_net::Path::from_static("acme"));
		drive_tick().await;
		assert_eq!(drain(&mut track), [Some(2)]);

		drop(first);
		drop(second);
		drive_tick().await;
		drive_tick().await;
		assert_eq!(drain(&mut track), [None], "gone once the last request ends");
	}

	#[tokio::test(start_paused = true)]
	async fn totals_split_sessions_by_tier() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _a = producer
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		let b = producer
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("beta"));
		let _c = producer
			.registry()
			.tier(Tier::new(moq_net::Path::from_static("region/sjc")))
			.session(moq_net::Path::from_static("peer"));

		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;
		let totals = read_totals(&broadcast).await;
		assert_eq!(totals[""].sessions.sessions_started, 2, "both default-tier labels");
		assert_eq!(totals["region/sjc"].sessions.sessions_started, 1);
		assert_eq!(totals["region/sjc"].publisher, Traffic::default());

		// A label whose last session leaves is pruned from the registry, but the
		// totals keep it.
		drop(b);
		drive_tick().await;
		drive_tick().await;
		let totals = read_totals(&broadcast).await;
		assert_eq!(totals[""].sessions.sessions_started, 2);
		assert_eq!(totals[""].sessions.active(), 1);
	}

	/// Subscribe to `label`'s presence track on `broadcast`, driving a tick so
	/// the producer adopts the request.
	async fn hold_presence(broadcast: &moq_net::broadcast::Consumer, label: &str) -> Reader<PresenceFrame> {
		let subscribing = broadcast
			.track(&presence_track(moq_net::Path::new(label).unwrap()))
			.expect("track")
			.subscribe(None);
		drive_tick().await;
		Reader::new(subscribing.await.expect("held"))
	}

	/// A held presence track is cumulative from when it was first held: it
	/// starts at the label's current counters, keeps a session that ended and
	/// was pruned, and continues across the label's return.
	#[tokio::test(start_paused = true)]
	async fn presence_track_is_cumulative_while_held() {
		let (producer, origin) = test_producer(Some("sjc"));
		let registry = producer.registry();
		let first = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		let _other = registry
			.tier(Tier::new(moq_net::Path::from_static("rtmp")))
			.session(moq_net::Path::from_static("acme/live"));
		let _unrelated = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("beta"));
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		let mut track = hold_presence(&broadcast, "acme/live").await;
		let frame = try_presence(&mut track).expect("answered at once");
		assert_eq!(frame[""].active(), 1);
		assert_eq!(frame["rtmp"].active(), 1, "every tier the label records on");
		assert_eq!(frame.len(), 2, "only this label");

		// The label's default-tier session leaves and its counters are pruned.
		drop(first);
		for _ in 0..3 {
			drive_tick().await;
		}
		let mut last = None;
		while let Some(frame) = try_presence(&mut track) {
			last = Some(frame);
		}
		let frame = last.expect("the closing readout");
		assert_eq!(frame[""].sessions_started, 1, "kept after the prune");
		assert_eq!(frame[""].active(), 0);

		// A new session under the label continues the count.
		let _again = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		drive_tick().await;
		let mut last = None;
		while let Some(frame) = try_presence(&mut track) {
			last = Some(frame);
		}
		let frame = last.expect("the return");
		assert_eq!(frame[""].sessions_started, 2);
		assert_eq!(frame[""].active(), 1);
	}

	/// A label with no sessions is held open at zero, and reclaimed once its
	/// last consumer leaves.
	#[tokio::test(start_paused = true)]
	async fn presence_track_for_an_idle_root_is_held_then_reclaimed() {
		let registry = Registry::new(moq_net::stats::Config::new());
		let origin = produce_origin();
		let mut drain = Drain::new(Task {
			registry: registry.clone(),
			origin: origin.clone(),
			prefix: Path::from_static(".stats"),
			node: None,
			depth: 0,
			linger: DEFAULT_LINGER,
			interval: Duration::from_secs(1),
		})
		.expect("drain");
		let mut tick = || {
			drain.collect();
			drain.publish(moq_clock::now());
			drain.node.presence.len()
		};
		let broadcast = origin.consume().request(".stats/node", None).await.expect("resolve");

		// Let the request reach the stats broadcast's queue before the drain.
		let subscribing = broadcast
			.track(&presence_track(moq_net::Path::from_static("")))
			.expect("track")
			.subscribe(None);
		for _ in 0..8 {
			tokio::task::yield_now().await;
		}
		assert_eq!(tick(), 1, "held");
		let mut track = Reader::new(subscribing.await.expect("held"));
		assert_eq!(try_presence(&mut track), Some(PresenceFrame::new()), "held at zero");

		// The label records while held: the same subscription carries it.
		let _session = registry.tier(Tier::default()).session(moq_net::Path::from_static(""));
		tokio::time::advance(Duration::from_secs(1)).await;
		tick();
		assert_eq!(try_presence(&mut track).expect("recorded")[""].active(), 1);

		// The consumer leaves: once the origin releases its copy (the track
		// linger), the next drain reclaims the hold.
		drop(track);
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
		tokio::time::advance(Duration::from_secs(31)).await;
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
		assert_eq!(tick(), 0, "reclaimed");
	}

	/// Every label is held, whichever group it falls in.
	#[tokio::test(start_paused = true)]
	async fn presence_requests_hold_any_label() {
		let (producer, origin) = grouped_producer(DEFAULT_LINGER);
		let _f = feed(producer.registry(), Tier::default(), "acme/live", true, 1, 42).await;
		drive_tick().await;
		let (_, acme) = announced(&origin).await;

		let pending: Vec<_> = ["beta", "acme/0", "acme/1"]
			.into_iter()
			.map(|label| {
				acme.track(&presence_track(moq_net::Path::new(label).unwrap()))
					.expect("track")
					.subscribe(None)
			})
			.collect();
		drive_tick().await;
		let mut held = Vec::new();
		for subscribing in pending {
			held.push(subscribing.await.expect("held"));
		}
	}

	/// Subscribe to `path`'s prefix track on `broadcast`, driving a tick so the
	/// producer adopts the request.
	async fn hold_prefix(broadcast: &moq_net::broadcast::Consumer, path: &str) -> Reader<PrefixFrame> {
		let subscribing = broadcast
			.track(&prefix_track(moq_net::Path::new(path).unwrap()))
			.expect("track")
			.subscribe(None);
		drive_tick().await;
		Reader::new(subscribing.await.expect("held"))
	}

	/// The latest buffered prefix frame, `None` when nothing is buffered.
	fn last_prefix(track: &mut Reader<PrefixFrame>) -> Option<PrefixFrame> {
		track.try_last()
	}

	#[test]
	fn ancestors_walk_from_the_root() {
		let walk = |path| ancestors(path).collect::<Vec<_>>();
		assert_eq!(walk(""), [("", None)]);
		assert_eq!(walk("a"), [("", Some("a")), ("a", None)]);
		assert_eq!(
			walk("a/bc/d"),
			[
				("", Some("a")),
				("a", Some("bc")),
				("a/bc", Some("d")),
				("a/bc/d", None)
			]
		);
	}

	/// A held prefix carries its rollup, the broadcast at exactly the prefix,
	/// and one level of children, each summing everything beneath it.
	#[tokio::test(start_paused = true)]
	async fn prefix_track_rolls_up_self_and_children() {
		let (producer, origin) = grouped_producer(DEFAULT_LINGER);
		let registry = producer.registry();
		let _own = feed(registry, Tier::default(), "acme", true, 1, 1).await;
		let _live = feed(registry, Tier::default(), "acme/live", true, 1, 10).await;
		let _cam = feed(registry, Tier::default(), "acme/live/cam", true, 1, 100).await;
		let _rtmp = feed(
			registry,
			Tier::new(moq_net::Path::from_static("rtmp")),
			"acme/vod",
			true,
			1,
			1000,
		)
		.await;
		let _other = feed(registry, Tier::default(), "beta/live", true, 1, 5).await;
		drive_tick().await;
		let (_, acme) = announced(&origin).await;

		let mut track = hold_prefix(&acme, "acme").await;
		let frame = last_prefix(&mut track).expect("answered at once");
		let default = &frame[""];
		assert_eq!(default.publisher.bytes, 111);
		assert_eq!(default.own.publisher.bytes, 1);
		assert_eq!(default.children.len(), 1);
		assert_eq!(
			default.children["live"].publisher.bytes, 110,
			"a child rolls up its subtree"
		);
		assert_eq!(frame["rtmp"].publisher.bytes, 1000, "every tier the prefix records on");
		assert_eq!(frame["rtmp"].children["vod"].publisher.bytes, 1000);
		assert_eq!(frame.len(), 2);

		let mut track = hold_prefix(&acme, "acme/live/cam").await;
		let frame = last_prefix(&mut track).expect("answered at once");
		assert_eq!(frame[""].own.publisher.bytes, 100);
		assert!(frame[""].children.is_empty());

		// Any group's prefix is held, but one deeper than any path is refused.
		let mut track = hold_prefix(&acme, "beta").await;
		let frame = last_prefix(&mut track).expect("answered at once");
		assert_eq!(frame[""].publisher.bytes, 5);
		let deep = ["acme"; Path::MAX_PARTS + 1].join("/");
		let subscribing = acme
			.track(&prefix_track(moq_net::Path::new(deep.as_str()).unwrap()))
			.expect("track")
			.subscribe(None);
		drive_tick().await;
		assert!(matches!(subscribing.await, Err(moq_net::Error::NotFound)));
	}

	/// Counters follow presence: a prefix held while 300 subscriptions are
	/// live counts all 300 as active, and every one leaving reads as ended.
	/// The child leaves the map after its closing readout; its tail stays in
	/// the rollup.
	#[tokio::test(start_paused = true)]
	async fn prefix_held_mid_stream_counts_active_exactly() {
		let (producer, origin) = test_producer(Some("sjc"));
		let registry = producer.registry();
		let data = produce_origin();
		let source = data
			.clone()
			.with_stats(
				registry
					.tier(Tier::default())
					.session(moq_net::Path::from_static("publisher")),
			)
			.publish("acme/live", origin::Route::default())
			.expect("publish");
		let video = source.create_track("video", timed()).expect("create_track");
		let egress = data.consume().with_stats(
			registry
				.tier(Tier::default())
				.session(moq_net::Path::from_static("viewer")),
		);
		let broadcast = egress.request("acme/live", None).await.expect("resolve");
		let mut subs = Vec::new();
		for _ in 0..300 {
			let track = broadcast.track("video").expect("track");
			subs.push(track.subscribe(None).await.expect("subscribe"));
		}
		{
			let mut group = video.append_group().expect("group");
			group.write_frame(Timestamp::ZERO, vec![0u8; 10]).expect("write");
			group.finish().expect("finish");
		}
		drive_tick().await;
		let (_, stats) = announced(&origin).await;

		let mut track = hold_prefix(&stats, "acme").await;
		let frame = last_prefix(&mut track).expect("answered at once");
		assert_eq!(frame[""].publisher.active_subscriptions(), 300);
		assert_eq!(frame[""].children["live"].publisher.active_subscriptions(), 300);

		drop(subs);
		drive_tick().await;
		let frame = last_prefix(&mut track).expect("the leave");
		assert_eq!(frame[""].publisher.subscriptions_started, 300);
		assert_eq!(frame[""].publisher.active_subscriptions(), 0);

		// The broadcast ends: once its counters are dropped the child leaves the
		// map, and the rollup keeps everything.
		let before = frame[""].publisher;
		drop((video, source, broadcast));
		for _ in 0..3 {
			drive_tick().await;
		}
		let frame = last_prefix(&mut track).expect("the child leaves");
		assert!(frame[""].children.is_empty(), "{frame:?}");
		assert_eq!(frame[""].publisher.subscriptions_started, before.subscriptions_started);
		assert_eq!(frame[""].publisher.bytes, before.bytes);
		assert_eq!(frame[""].publisher.active_subscriptions(), 0);
	}

	/// A nested broadcast ending leaves the parent's own counters flat, while
	/// its rollup keeps the nested broadcast's tail.
	#[tokio::test(start_paused = true)]
	async fn nested_broadcast_ending_keeps_the_parent_self_flat() {
		let (producer, origin) = test_producer(Some("sjc"));
		let registry = producer.registry();
		let _parent = feed(registry, Tier::default(), "acme/live", true, 1, 10).await;
		let nested = feed(registry, Tier::default(), "acme/live/cam", true, 1, 100).await;
		drive_tick().await;
		let (_, stats) = announced(&origin).await;

		let mut track = hold_prefix(&stats, "acme/live").await;
		let frame = last_prefix(&mut track).expect("answered at once");
		let own = frame[""].own;
		assert_eq!(own.publisher.bytes, 10);
		assert_eq!(frame[""].publisher.bytes, 110);

		drop(nested);
		for _ in 0..3 {
			drive_tick().await;
		}
		let frame = last_prefix(&mut track).expect("the nested broadcast ended");
		assert!(frame[""].children.is_empty(), "{frame:?}");
		assert_eq!(frame[""].own, own, "the parent's own counters stay flat");
		assert_eq!(frame[""].publisher.bytes, 110, "the rollup keeps the nested tail");
		assert_eq!(frame[""].publisher.active_subscriptions(), 1, "the parent's viewer");
	}

	/// A prefix with no members is held open at zero, carries its first
	/// member on the same subscription, and is reclaimed once its last
	/// consumer leaves.
	#[tokio::test(start_paused = true)]
	async fn prefix_track_for_an_idle_prefix_is_held_then_reclaimed() {
		let registry = Registry::new(moq_net::stats::Config::new());
		let origin = produce_origin();
		let mut drain = Drain::new(Task {
			registry: registry.clone(),
			origin: origin.clone(),
			prefix: Path::from_static(".stats"),
			node: None,
			depth: 0,
			linger: DEFAULT_LINGER,
			interval: Duration::from_secs(1),
		})
		.expect("drain");
		let mut tick = || {
			drain.collect();
			drain.publish(moq_clock::now());
			drain.node.prefixes.len()
		};
		let broadcast = origin.consume().request(".stats/node", None).await.expect("resolve");

		let subscribing = broadcast
			.track(&prefix_track(moq_net::Path::from_static("acme")))
			.expect("track")
			.subscribe(None);
		for _ in 0..8 {
			tokio::task::yield_now().await;
		}
		assert_eq!(tick(), 1, "held");
		let mut track = Reader::new(subscribing.await.expect("held"));
		assert_eq!(last_prefix(&mut track), Some(PrefixFrame::new()), "held at zero");

		let _feed = feed(&registry, Tier::default(), "acme/live", true, 1, 7).await;
		tokio::time::advance(Duration::from_secs(1)).await;
		tick();
		let frame = last_prefix(&mut track).expect("recorded");
		assert_eq!(frame[""].children["live"].publisher.bytes, 7);

		drop(track);
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
		tokio::time::advance(Duration::from_secs(31)).await;
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
		assert_eq!(tick(), 0, "reclaimed");
	}

	#[tokio::test(start_paused = true)]
	async fn unused_slots_dont_surface() {
		// A broadcast that only sees default-tier egress carries zero ingress,
		// and a tier with no traffic is absent from the group frame.
		let (producer, origin) = test_producer(Some("sjc"));
		// Only the egress (publisher) side is tagged, so `foo/bar` gets publisher
		// traffic and no subscriber traffic.
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 8).await;

		drive_tick().await;
		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_group(&broadcast, "").await;
		let usage = frame.tiers[""].paths.get("foo/bar").expect("the active foo/bar entry");
		assert_eq!(usage.publisher.bytes, 8);
		assert_eq!(usage.subscriber, Traffic::default(), "no ingress moved");
		assert!(!frame.tiers.contains_key("region/sjc"), "an idle tier never surfaces");
		let totals = broadcast.track("totals.json").unwrap().info().await.unwrap();
		assert!(totals.compress, "the totals are compressed");
	}

	#[test]
	fn advertised_path_with_and_without_node() {
		let prefix = Path::from_static(".stats");
		assert_eq!(
			advertised_path(&prefix, Some(&Path::from_static("sjc")))
				.unwrap()
				.as_str(),
			".stats/node/sjc"
		);
		assert_eq!(
			advertised_path(&prefix, Some(&Path::from_static("sjc/1")))
				.unwrap()
				.as_str(),
			".stats/node/sjc/1"
		);
		assert_eq!(advertised_path(&prefix, None).unwrap().as_str(), ".stats/node");
		assert_eq!(
			advertised_path(&Path::from_static("metrics"), Some(&Path::from_static("lon")))
				.unwrap()
				.as_str(),
			"metrics/node/lon"
		);
	}

	#[test]
	fn group_key_uses_leading_segments() {
		let key = |path, depth| group_key(&Path::from_static(path), depth);
		assert_eq!(key("acme/room/cam", 0), Path::empty());
		assert_eq!(key("acme/room/cam", 1), Path::from_static("acme"));
		assert_eq!(key("acme/room/cam", 2), Path::from_static("acme/room"));
		assert_eq!(key("acme/room", 3), Path::from_static("acme/room"));
	}

	/// A name no tier could produce is still rejected.
	#[tokio::test(start_paused = true)]
	async fn malformed_track_name_rejected() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		let subscribing = broadcast.track("bogus.json").expect("track").subscribe(None);
		drive_tick().await;
		assert!(subscribing.await.is_err(), "a non-stats name is rejected");
	}

	#[test]
	fn frame_serializes_like_a_btreemap() {
		// The producer's reused frame must stay byte-identical to the
		// `BTreeMap` consumers parse, including key order.
		let traffic = |bytes| {
			let mut traffic = Traffic::default();
			traffic.bytes = bytes;
			traffic
		};
		let mut frame = Frame::default();
		let mut map = BTreeMap::new();
		for (path, bytes) in [("room/b", 2), ("room/a", 1), ("other", 3), ("room/a/cam", 4)] {
			frame.entries.push((Path::try_from(path).unwrap(), traffic(bytes)));
			map.insert(path.to_string(), traffic(bytes));
		}
		frame.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		assert_eq!(serde_json::to_vec(&frame).unwrap(), serde_json::to_vec(&map).unwrap());
		assert_eq!(serde_json::to_vec(&Frame::<Traffic>::default()).unwrap(), b"{}");
	}

	#[test]
	fn group_body_serializes_like_a_group_frame() {
		let mut usage = Usage::default();
		usage.subscriber.bytes = 5;
		let mut presence = Presence::default();
		presence.sessions_started = 2;
		let mut tier = GroupTier::default();
		tier.folded.publisher.bytes = 99;
		tier.totals.publisher.bytes = 7;
		tier.totals.sessions.sessions_started = 2;
		let side = PathUsage {
			publisher: None,
			subscriber: Some(usage.subscriber),
		};
		tier.paths.entries.push((Path::from_static("acme/live"), side));
		tier.labels.entries.push((Path::from_static("acme"), presence));
		let body = GroupBody {
			epoch: Epoch::mint(),
			tiers: Frame {
				entries: vec![(Path::from_static(""), tier)],
			},
		};

		let mut group = crate::Group::default();
		group.publisher.bytes = 7;
		group.sessions.sessions_started = 2;
		group.paths.insert("acme/live".into(), usage);
		group.labels.insert("acme".into(), presence);
		let json: serde_json::Value = serde_json::to_value(&body).unwrap();
		let frame: crate::GroupFrame = serde_json::from_value(json.clone()).unwrap();
		assert_eq!(frame.epoch, body.epoch);
		assert_eq!(frame.tiers, BTreeMap::from([(String::new(), group)]));
		let tier = &json["tiers"][""];
		assert!(tier.get("folded").is_none(), "folded totals are left out");
		assert!(
			tier["paths"]["acme/live"].get("publisher").is_none(),
			"a side that never moved is left out"
		);
	}

	/// Counts this thread's allocations, so the test below measures only its
	/// own drain while other tests run in parallel.
	mod counting {
		use std::alloc::{GlobalAlloc, Layout, System};
		use std::cell::Cell;

		thread_local! {
			static ALLOCS: Cell<usize> = const { Cell::new(0) };
		}

		struct Counting;

		unsafe impl GlobalAlloc for Counting {
			unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
				let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
				unsafe { System.alloc(layout) }
			}

			unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
				unsafe { System.dealloc(ptr, layout) }
			}

			unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
				let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
				unsafe { System.realloc(ptr, layout, new_size) }
			}
		}

		#[global_allocator]
		static GLOBAL: Counting = Counting;

		pub fn allocs() -> usize {
			ALLOCS.with(Cell::get)
		}
	}

	#[tokio::test(start_paused = true)]
	async fn steady_drain_collects_without_allocating() {
		// Once every group, track, and buffer exists, draining the registry
		// into the pending frames allocates nothing, however many broadcasts
		// and tiers there are, or whether a prefix is held. Encoding the frames
		// (moq-json) is out of scope.
		for (depth, hold) in [(0, false), (1, false), (0, true), (1, true)] {
			for (broadcasts, tiers) in [(1, 1), (16, 1), (1, 4), (16, 4)] {
				let registry = Registry::new(moq_net::stats::Config::new());
				let mut feeds = Vec::new();
				for t in 0..tiers {
					let tier = match t {
						0 => Tier::default(),
						t => Tier::new(moq_net::Path::new(&format!("tier{t}")).unwrap()),
					};
					for b in 0..broadcasts {
						feeds.push(feed(&registry, tier.clone(), &format!("room{b}/cam"), true, 1, 8).await);
					}
				}

				let mut drain = Drain::new(Task {
					registry,
					origin: produce_origin(),
					prefix: Path::from_static(".stats"),
					node: None,
					depth,
					linger: DEFAULT_LINGER,
					interval: Duration::from_secs(1),
				})
				.expect("drain");

				// Warm up: create the groups and tracks and grow every buffer.
				for _ in 0..3 {
					drain.collect();
					drain.publish(moq_clock::now());
				}
				// Hold a prefix with a child per tier, as a reader would.
				let _held = hold.then(|| {
					let tick = drain.tick;
					let stamp = drain.stamp(moq_clock::now());
					drain
						.node
						.adopt_prefix(&drain.report, Path::from_static("room0"), None, tick, stamp);
					drain.node.prefixes["room0"].output.snapshot.track.subscribe(None)
				});
				drain.collect();
				drain.publish(moq_clock::now());

				let before = counting::allocs();
				drain.collect();
				let allocs = counting::allocs() - before;
				drain.publish(moq_clock::now());

				assert!(!drain.report.traffic.is_empty(), "the drain carried entries");
				assert_eq!(
					allocs, 0,
					"depth {depth}, {broadcasts} broadcasts x {tiers} tiers, held prefix {hold}"
				);
				if hold {
					assert_eq!(drain.node.prefixes.len(), 1, "still held");
				}
			}
		}
	}

	/// Read `reader` until its track ends, returning how it ended and the last
	/// frame before that.
	async fn drain_group(reader: &mut crate::consume::GroupConsumer) -> (crate::Result<()>, Option<crate::GroupFrame>) {
		let mut last = None;
		loop {
			match reader.next().await {
				Ok(Some(frame)) => last = Some(frame),
				Ok(None) => return (Ok(()), last),
				Err(err) => return (Err(err), last),
			}
		}
	}

	/// The next listing change to a group track, as `(group, listed)`, skipping
	/// every other track.
	async fn next_listed(drain: &mut Drain, now: Instant, tracks: &mut broadcast::Tracks) -> (String, bool) {
		loop {
			let event = pumping_at(drain, now, tracks.next())
				.await
				.expect("listing")
				.expect("live");
			let (name, listed) = match event {
				broadcast::TracksEvent::Added { name, .. } => (name, true),
				broadcast::TracksEvent::Removed { name } | broadcast::TracksEvent::Finished { name } => (name, false),
				broadcast::TracksEvent::Synced => continue,
			};
			if let Some(group) = crate::parse_group_track(&name) {
				return (group.as_str().to_string(), listed);
			}
		}
	}

	/// Groups are listed through a relay as they gain entries, stay listed for
	/// the linger once empty, and are unlisted after it, all on one broadcast
	/// whose announcement never changes.
	#[tokio::test(start_paused = true)]
	async fn group_tracks_list_and_unlist_through_a_relay() {
		let registry = Registry::new(moq_net::stats::Config::new());
		let origin = produce_origin();
		let mut drain = Drain::new(Task {
			registry: registry.clone(),
			origin: origin.clone(),
			prefix: moq_net::Path::from_static(".stats"),
			node: None,
			depth: 1,
			linger: Duration::from_secs(10),
			interval: Duration::from_secs(1),
		})
		.unwrap();
		let (edge, _cache) = relay();
		let _link = link(&origin, &edge, "moq-lite-07-wip".parse().unwrap()).await;
		let start = Instant::now();

		let acme = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		drain.collect();
		drain.publish(start);
		let consumer = edge.consume().with_hidden(true);
		let mut announced = consumer.announced();
		let (route, active) = next_update(&mut announced).await.unwrap();
		assert!(active);
		assert_eq!(route.prefix.as_str(), ".stats/node");
		let broadcast = consumer.request(".stats/node", None).await.unwrap();
		let mut tracks = broadcast.tracks().unwrap();
		assert_eq!(next_listed(&mut drain, start, &mut tracks).await, ("acme".into(), true));

		let beta = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("beta"));
		assert_eq!(next_listed(&mut drain, start, &mut tracks).await, ("beta".into(), true));

		// Empty, acme lingers: still listed, its totals keeping the ended session.
		drop(acme);
		let lingering = start + Duration::from_secs(9);
		let stats = crate::Consumer::new(broadcast.clone(), Default::default());
		let mut acme = pumping_at(&mut drain, lingering, stats.group(moq_net::Path::from_static("acme")))
			.await
			.unwrap();
		let mut frame = pumping_at(&mut drain, lingering, acme.next()).await.unwrap().unwrap();
		while !frame.tiers[""].labels.is_empty() {
			frame = pumping_at(&mut drain, lingering, acme.next()).await.unwrap().unwrap();
		}
		assert_eq!(frame.tiers[""].sessions.sessions_ended, 1);

		// Past the linger, acme alone is unlisted and its subscription ends.
		let lingered = start + Duration::from_secs(20);
		assert_eq!(
			next_listed(&mut drain, lingered, &mut tracks).await,
			("acme".into(), false)
		);
		assert!(pumping_at(&mut drain, lingered, acme.next()).await.unwrap().is_none());
		let mut beta_reader = pumping_at(&mut drain, lingered, stats.group(moq_net::Path::from_static("beta")))
			.await
			.unwrap();
		let frame = pumping_at(&mut drain, lingered, beta_reader.next())
			.await
			.unwrap()
			.unwrap();
		assert_eq!(frame.tiers[""].labels.keys().collect::<Vec<_>>(), ["beta"]);

		// A return is listed again on the same broadcast, with no announce.
		let _acme = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		assert_eq!(
			next_listed(&mut drain, lingered, &mut tracks).await,
			("acme".into(), true)
		);
		drop(beta);
		assert!(
			futures::FutureExt::now_or_never(next_update(&mut announced)).is_none(),
			"group churn costs no announce"
		);
	}

	/// A group that returns after its linger is a new track instance: a new
	/// epoch counted from zero, so a reader summing each epoch never counts the
	/// old one twice, and group numbers that continue past the old instance's.
	#[tokio::test(start_paused = true)]
	async fn a_relisted_group_starts_a_new_epoch_from_zero() {
		let (producer, origin) = grouped_producer(Duration::ZERO);
		let registry = producer.registry();
		let (_, broadcast) = announced(&origin).await;
		let stats = crate::Consumer::new(broadcast.clone(), Default::default());
		let sequence = async |broadcast: &moq_net::broadcast::Consumer| {
			let mut track = subscribe(broadcast, "acme/group.json").await;
			track.next_group().await.unwrap().unwrap().sequence
		};

		let session = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		drive_tick().await;
		let mut reader = stats.group(moq_net::Path::from_static("acme")).await.expect("listed");
		let first = reader.next().await.unwrap().unwrap();
		assert_eq!(first.tiers[""].sessions.sessions_started, 1);
		let old_sequence = sequence(&broadcast).await;

		drop(session);
		for _ in 0..3 {
			drive_tick().await;
		}
		let (end, last) = drain_group(&mut reader).await;
		end.expect("the group's track finishes cleanly");
		let last = last.unwrap_or(first.clone());
		assert_eq!(last.epoch, first.epoch);
		assert_eq!(last.tiers[""].sessions.sessions_ended, 1);

		let _session = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		drive_tick().await;
		let second = stats
			.group(moq_net::Path::from_static("acme"))
			.await
			.expect("listed again")
			.next()
			.await
			.unwrap()
			.unwrap();
		assert!(second.epoch > first.epoch, "a returning group mints a new epoch");
		assert_eq!(
			second.tiers[""].sessions.sessions_started, 1,
			"a new epoch counts from zero"
		);
		assert!(
			sequence(&broadcast).await > old_sequence,
			"group numbers continue past the old instance"
		);
		assert_eq!(
			default_totals(&broadcast).await.sessions.sessions_started,
			2,
			"the node totals carry both epochs"
		);
	}

	/// One group's frame outgrowing the cache ends that group's track alone.
	///
	/// Read off the broadcast itself: through an origin, the subscriber's resume
	/// re-requests the ended track and reads the refusal instead of the cause.
	#[tokio::test(start_paused = true)]
	async fn an_oversize_group_leaves_other_groups_readable() {
		let registry = Registry::new(moq_net::stats::Config::new());
		let mut drain = Drain::new(Task {
			registry: registry.clone(),
			origin: produce_origin(),
			prefix: moq_net::Path::from_static(".stats"),
			node: None,
			depth: 1,
			linger: Duration::ZERO,
			interval: Duration::from_secs(1),
		})
		.unwrap();
		let stats = crate::Consumer::new(drain.node.broadcast.consume(), Default::default());

		let _acme = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		let _big = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("big"));
		let mut acme = pumping(&mut drain, stats.group(moq_net::Path::from_static("acme")))
			.await
			.unwrap();
		let mut big = pumping(&mut drain, stats.group(moq_net::Path::from_static("big")))
			.await
			.unwrap();
		assert!(pumping(&mut drain, acme.next()).await.unwrap().is_some());
		assert!(pumping(&mut drain, big.next()).await.unwrap().is_some());

		let _huge: Vec<_> = oversized_paths("big/")
			.into_iter()
			.map(|path| registry.tier(Tier::default()).session(path))
			.collect();
		let (end, _) = pumping(&mut drain, drain_group(&mut big)).await;
		assert!(
			matches!(
				end,
				Err(crate::Error::Json(moq_json::Error::Net(moq_net::Error::FrameTooLarge)))
			),
			"{end:?}"
		);

		let _more = registry
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/more"));
		let mut frame = pumping(&mut drain, acme.next()).await.unwrap().unwrap();
		while frame.tiers[""].labels.len() < 2 {
			frame = pumping(&mut drain, acme.next()).await.unwrap().unwrap();
		}
		assert_eq!(frame.tiers[""].labels.keys().collect::<Vec<_>>(), ["acme", "acme/more"]);
	}

	/// A group's track carries only its own paths and labels, and there are no
	/// per-tier tracks.
	#[tokio::test(start_paused = true)]
	async fn a_group_track_carries_only_its_own_entries() {
		let (producer, origin) = grouped_producer(DEFAULT_LINGER);
		let registry = producer.registry();
		let (_, broadcast) = announced(&origin).await;
		let stats = crate::Consumer::new(broadcast.clone(), Default::default());

		// Each feed holds a session labelled `feed`, its own group.
		let _acme = feed(registry, Tier::default(), "acme/live", true, 1, 100).await;
		let _beta = feed(registry, Tier::default(), "beta/live", true, 1, 50).await;
		drive_tick().await;

		let frame = stats
			.group(moq_net::Path::from_static("acme"))
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.unwrap();
		let acme = &frame.tiers[""];
		assert_eq!(acme.paths.keys().collect::<Vec<_>>(), ["acme/live"]);
		assert_eq!(acme.paths["acme/live"].publisher.bytes, 100);
		assert_eq!(acme.publisher.bytes, 100, "the totals are the group's alone");
		assert!(acme.labels.is_empty());

		let frame = stats
			.group(moq_net::Path::from_static("beta"))
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.unwrap();
		assert_eq!(frame.tiers[""].paths.keys().collect::<Vec<_>>(), ["beta/live"]);
		assert_eq!(frame.tiers[""].publisher.bytes, 50);

		let frame = stats
			.group(moq_net::Path::from_static("feed"))
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.unwrap();
		assert!(frame.tiers[""].paths.is_empty());
		assert_eq!(frame.tiers[""].labels["feed"].sessions_started, 2);

		assert_eq!(
			default_totals(&broadcast).await.publisher.bytes,
			150,
			"the node totals sum every group"
		);
		let refused = broadcast.track("publisher.json").unwrap().subscribe(None);
		let (refused, ()) = tokio::join!(refused, drive_tick());
		assert!(refused.is_err(), "no per-tier tracks");
	}
}
