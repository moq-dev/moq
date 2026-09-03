use crate::{broadcast, cache, stats, track};
use kio::Pollable;
use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, HashMap, HashSet},
	fmt,
	sync::Arc,
	sync::atomic::{AtomicU64, Ordering},
	task::{Poll, ready},
	time::Duration,
};

use rand::RngExt;
use web_async::Lock;
use web_transport_trait::{MaybeSend, MaybeSync};

use super::{Requests, WeakCache, WeakEntry};
use crate::{
	AsPath, Error, Path, PathOwned, PathPrefixes,
	coding::{BoundsExceeded, Decode, DecodeError, Encode, EncodeError},
	runtime::{AnyTimers, Instant, Timers, TimersSlot},
	util::{TaskSet, Tasks, TasksWeak},
};

/// One relay's identity in a broadcast's hop chain: a 62-bit varint on the wire.
///
/// Names a *hop*, not an [`origin::Producer`](Producer): a relay's routing table is the
/// origin, and this is the id it stamps into a route's hop chain as an announcement
/// passes through, so a receiver can spot its own id and reject a loop.
///
/// Local hops are built with [`Hop::new`] or [`Hop::random`], both of which guarantee a
/// non-zero id so loop detection can work. Remote peers may still send `0`; it is legal
/// on the wire but cannot be used for loop detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hop {
	/// 62-bit identifier. Encoded as a QUIC varint on the wire.
	id: u64,
}

impl Hop {
	/// Placeholder for hop entries whose actual id is not on the wire (Lite03).
	/// Also used for remote peers that choose the legal but loop-blind id 0.
	pub(crate) const UNKNOWN: Self = Self { id: 0 };

	/// Build a hop from a stable id.
	///
	/// The id must be non-zero and fit in the 62-bit QUIC varint range. Wire
	/// decode accepts remote id 0, but a local hop should not use it because
	/// downstream peers cannot exclude it for loop detection.
	pub fn new(id: u64) -> Result<Self, InvalidHop> {
		if id == 0 || id >= 1u64 << 62 {
			return Err(InvalidHop::Range);
		}
		Ok(Self { id })
	}

	/// Generate a fresh hop with a random non-zero id. Use this for any relay that
	/// does not need a stable identity across restarts.
	///
	/// TEMPORARY: the wire format allows 62 bits, but older `@moq/lite` JS
	/// clients decode `AnnounceInterest.exclude_hop` as a u53 (number) and
	/// throw on anything > 2^53-1. To keep those clients alive against
	/// fresh relays, we cap the random id at 53 bits. Restore to 62 bits
	/// once the JS u62 fix has propagated to deployed bundles.
	pub fn random() -> Self {
		let mut rng = rand::rng();
		let id = rng.random_range(1..(1u64 << 53));
		Self { id }
	}

	/// Return the origin's wire id.
	pub fn id(self) -> u64 {
		self.id
	}
}

/// An origin's identity plus the cache pool its broadcasts inherit.
///
/// Doubles as the construction config for an [origin `Producer`](Producer) and as the
/// parent handle every broadcast carries ([`broadcast::Info::origin`]): the origin owns
/// the [`cache::Pool`] every group in the tree charges into, so a relay configures one
/// bounded pool here and every broadcast, track, and group beneath it reaches that single
/// budget by walking up the ownership chain. Defaults to no byte target and the
/// cache's standard idle expiry. Cheap to clone (a `Copy` id plus an `Arc`-handle
/// bump), so it's stored by value rather than behind another `Arc`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Info {
	/// The origin's wire identity, appended to broadcast hop chains for loop
	/// detection and shortest-path routing.
	pub id: Hop,

	/// The cache pool broadcasts under this origin charge their groups into. It flows
	/// down the ownership chain (origin -> broadcast -> track -> group): a track opens
	/// an account against it, and its groups charge through that. It has no byte target
	/// and uses [`cache::DEFAULT_EXPIRY`] by default; a relay sets a shared configured
	/// pool (via [`Self::with_pool`]) so cached groups across the whole process share
	/// one policy.
	pub pool: cache::Pool,

	/// Ceiling on each track's media-timestamp retention window under this origin.
	/// Each track's own [`max_age`](track::Info::max_age) is clamped down to this
	/// when the track binds, so a subscriber is never promised more history than the
	/// origin allows, regardless of what a publisher advertises. Wall-clock
	/// reclamation of idle content is separate: [`Self::pool`]'s
	/// [`expiry`](cache::Pool::expiry) window. [`Duration::MAX`] (the default)
	/// imposes no ceiling, leaving each track's own window in force.
	pub cache_duration: Duration,

	/// The retention window given to a track whose publisher advertises none.
	///
	/// moq-lite 05+ carries [`max_age`](track::Info::max_age) in TRACK_INFO, so a
	/// track relayed over it keeps the window its publisher chose. Every moq-transport
	/// draft and moq-lite 01-04 have no such wire property, so a track arriving over one
	/// of them lands here instead. Raise it on a relay fronting a segmented egress
	/// (HLS/DASH), which needs a playlist window's worth of history rather than the live
	/// edge. Defaults to [`track::DEFAULT_MAX_AGE`], and [`Self::cache_duration`]
	/// still caps it.
	pub default_max_age: Duration,
}

impl Default for Info {
	/// An unknown origin (id `0`, no loop detection) with no byte target and the
	/// default idle expiry. This is what a standalone broadcast inherits.
	fn default() -> Self {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY));
		Self {
			id: Hop::UNKNOWN,
			pool,
			cache_duration: Duration::MAX,
			default_max_age: track::DEFAULT_MAX_AGE,
		}
	}
}

impl Info {
	/// Config for the given origin id with no byte target and the default idle expiry.
	pub fn new(id: Hop) -> Self {
		Self { id, ..Self::default() }
	}

	/// Set the cache pool this origin's broadcasts inherit, returning `self` for chaining.
	pub fn with_pool(mut self, pool: cache::Pool) -> Self {
		self.pool = pool;
		self
	}

	/// Set the retention ceiling (see [`Self::cache_duration`]) applied to every track
	/// under this origin, returning `self` for chaining.
	pub fn with_cache_duration(mut self, cache_duration: Duration) -> Self {
		self.cache_duration = cache_duration;
		self
	}

	/// Set the retention window (see [`Self::default_max_age`]) used for tracks whose
	/// publisher advertises none, returning `self` for chaining.
	pub fn with_default_max_age(mut self, default_max_age: Duration) -> Self {
		self.default_max_age = default_max_age;
		self
	}
}

impl From<Hop> for Info {
	/// Config for the given origin id with the defaults of [`Info::new`].
	fn from(id: Hop) -> Self {
		Self::new(id)
	}
}

impl TryFrom<u64> for Hop {
	type Error = InvalidHop;

	fn try_from(id: u64) -> Result<Self, Self::Error> {
		Self::new(id)
	}
}

impl fmt::Display for Hop {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.id.fmt(f)
	}
}

impl<V: Copy> Encode<V> for Hop
where
	u64: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.id.encode(w, version)
	}
}

impl<V: Copy> Decode<V> for Hop
where
	u64: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let id = u64::decode(r, version)?;
		if id >= 1u64 << 62 {
			return Err(DecodeError::InvalidValue);
		}
		Ok(Self { id })
	}
}

/// Maximum number of origins (hops) an [`Hops`] can hold.
///
/// Caps pathological or loop-induced announcements at a reasonable cluster
/// diameter; appending past this limit returns [`InvalidHop::TooMany`] rather than
/// silently truncating.
pub(crate) const MAX_HOPS: usize = 32;

/// Bounded, loop-free list of [`Hop`] entries: the hop chain of a broadcast.
///
/// Guarantees `len() <= MAX_HOPS` and that no non-zero [`Hop`] appears twice. Both
/// are wire rules, and both hold wherever a list exists rather than only where one was
/// parsed, so a chain that a conforming receiver would reject cannot be built and sent.
/// Construct via [`Hops::new`] + [`Hops::push`], or fall back to the
/// fallible [`TryFrom<Vec<Hop>>`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hops(Vec<Hop>);

/// Why a [`Hop`] is not usable, on its own or as part of a [`Hops`] chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidHop {
	/// The id is zero or outside the 62-bit wire range, so it cannot identify a local
	/// hop. Only [`Hop::new`] returns this; a chain never holds one.
	Range,

	/// The list is already at its hop-count cap, which a real path never reaches and a
	/// loop does.
	TooMany,

	/// The id is already in the list. A chain that revisits a hop looped, which every
	/// receiver of it must reject, so it must not be built in the first place. The
	/// reserved id 0 identifies nothing and may repeat.
	Duplicate,
}

impl fmt::Display for InvalidHop {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Range => write!(f, "local hop id must be non-zero and below 2^62"),
			Self::TooMany => write!(f, "too many hops (max {MAX_HOPS})"),
			Self::Duplicate => write!(f, "hop already in the chain"),
		}
	}
}

impl std::error::Error for InvalidHop {}

impl From<InvalidHop> for DecodeError {
	fn from(err: InvalidHop) -> Self {
		match err {
			InvalidHop::TooMany => DecodeError::BoundsExceeded,
			InvalidHop::Range | InvalidHop::Duplicate => DecodeError::InvalidValue,
		}
	}
}

impl Hops {
	/// Create an empty list.
	pub fn new() -> Self {
		Self(Vec::new())
	}

	/// Append an [`Hop`], rejecting anything a conforming receiver would.
	///
	/// Fails with [`InvalidHop::TooMany`] once the list is full, and with
	/// [`InvalidHop::Duplicate`] for an id already in the chain, which is a loop. The
	/// reserved id 0 identifies nothing, so it may repeat.
	pub fn push(&mut self, hop: Hop) -> Result<(), InvalidHop> {
		if self.0.len() >= MAX_HOPS {
			return Err(InvalidHop::TooMany);
		}
		if hop != Hop::UNKNOWN && self.0.contains(&hop) {
			return Err(InvalidHop::Duplicate);
		}
		self.0.push(hop);
		Ok(())
	}

	/// Replace the first entry equal to `target` with `replacement`, returning
	/// true if a match was found. The length is unchanged.
	///
	/// Fails with [`InvalidHop::Duplicate`] only when the rewrite would actually name
	/// `replacement` twice, which is the loop [`Self::push`] refuses to build. A `target`
	/// that is not present changes nothing and so cannot duplicate anything, and the slot
	/// being overwritten is not a duplicate of itself.
	pub fn replace_first(&mut self, target: Hop, replacement: Hop) -> Result<bool, InvalidHop> {
		let Some(index) = self.0.iter().position(|entry| *entry == target) else {
			return Ok(false);
		};

		if replacement != Hop::UNKNOWN
			&& self
				.0
				.iter()
				.enumerate()
				.any(|(i, entry)| i != index && *entry == replacement)
		{
			return Err(InvalidHop::Duplicate);
		}

		self.0[index] = replacement;
		Ok(true)
	}

	/// Returns true if any entry matches `hop`.
	pub fn contains(&self, hop: &Hop) -> bool {
		self.0.contains(hop)
	}

	/// Number of entries currently in the list (always `<= MAX_HOPS`).
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether the list contains no entries.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Iterate over the entries in hop order (oldest first).
	pub fn iter(&self) -> std::slice::Iter<'_, Hop> {
		self.0.iter()
	}

	/// Borrow the entries as a slice.
	pub fn as_slice(&self) -> &[Hop] {
		&self.0
	}
}

impl TryFrom<Vec<Hop>> for Hops {
	type Error = InvalidHop;

	fn try_from(v: Vec<Hop>) -> Result<Self, Self::Error> {
		if v.len() > MAX_HOPS {
			return Err(InvalidHop::TooMany);
		}
		// MAX_HOPS is 32, so the quadratic scan is cheaper than allocating a set.
		for (i, hop) in v.iter().enumerate() {
			if *hop != Hop::UNKNOWN && v[i + 1..].contains(hop) {
				return Err(InvalidHop::Duplicate);
			}
		}
		Ok(Self(v))
	}
}

impl<'a> IntoIterator for &'a Hops {
	type Item = &'a Hop;
	type IntoIter = std::slice::Iter<'a, Hop>;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

impl<V: Copy> Encode<V> for Hops
where
	u64: Encode<V>,
	Hop: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		(self.0.len() as u64).encode(w, version)?;
		for origin in &self.0 {
			origin.encode(w, version)?;
		}
		Ok(())
	}
}

impl<V: Copy> Decode<V> for Hops
where
	u64: Decode<V>,
	Hop: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let count = u64::decode(r, version)? as usize;
		if count > MAX_HOPS {
			return Err(DecodeError::BoundsExceeded);
		}
		// Through `push`, so a chain that revisits a hop is rejected here rather than
		// entering the model and being forwarded on to a receiver that must close on it.
		let mut list = Self(Vec::with_capacity(count));
		for _ in 0..count {
			list.push(Hop::decode(r, version)?)?;
		}
		Ok(list)
	}
}

/// The highest value either half of a [`Cost`] can take, and where cost
/// accumulation saturates.
///
/// The ceiling is the wire's, not the model's: lite-06 carries each cost as a QUIC
/// varint, which tops out at 2^62-1, so a larger value could be selected on but
/// never forwarded.
pub const MAX_COST: u64 = (1 << 62) - 1;

/// The cost given to a route whose session is draining, so every other candidate
/// outranks it while it stays selectable as the last path to the content.
///
/// A session sets this on its routes when its peer sends a GOAWAY. Draining is
/// deliberately not a distinct state: cost is the whole mechanism, so a route
/// whose accumulated cost saturates the wire ceiling ranks (and is treated)
/// identically, as a path of last resort.
///
/// It is [`MAX_COST`] rather than a value beyond it for the reason above: a
/// draining route is still announced downstream, so its cost has to fit the wire.
pub const DRAIN_COST: u64 = MAX_COST;

/// What pulling content via a route costs, in two magnitudes that accumulate
/// together and are compared in that order: lower [`warm`](Self::warm) wins, and
/// [`cold`](Self::cold) breaks the tie.
///
/// Both are the same path priced against different cache states. `warm` is what one
/// more subscription would cost the mesh right now; `cold` prices the identical
/// path as if nothing were cached, so it stays meaningful once discounts have
/// flattened `warm`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct Cost {
	/// The cost of pulling content via this route as the mesh stands today,
	/// accumulated per link. Lower wins.
	///
	/// The original publisher seeds it with its production cost (zero for a live
	/// publish, something large for a standby that would have to start working, like
	/// a cold transcoder), and each link adds its own configured price as the
	/// announcement crosses it, so a route over a metered backbone ranks worse than
	/// an equal-length one within a datacenter.
	pub warm: u64,

	/// The same path with every warm discount removed: what pulling the content
	/// would cost if no relay along it were carrying anything.
	///
	/// Accumulates exactly like [`warm`](Self::warm) but never restarts. [`MAX_COST`]
	/// when the peer's wire cannot express it (pre-lite-06, or the MoQ Cluster
	/// extension), which ranks last rather than pretending the path is free.
	pub cold: u64,
}

impl Cost {
	/// Both magnitudes at `cost`: an undiscounted route, which is what a publisher
	/// seeding its production cost means.
	pub const fn new(cost: u64) -> Self {
		Self { warm: cost, cold: cost }
	}

	/// The cost of a draining route: the ceiling in both magnitudes, so every other
	/// candidate outranks it. See [`DRAIN_COST`].
	pub const DRAIN: Self = Self::new(DRAIN_COST);

	/// What a peer advertises when its wire has no room for a cost at all: free to
	/// reach (leaving hop count as the effective metric, exactly as before route
	/// cost existed) with an unknown cold path.
	pub(crate) const UNKNOWN: Self = Self {
		warm: 0,
		cold: MAX_COST,
	};

	/// Add a link's price to both magnitudes, saturating at the largest cost the
	/// wire can carry so a huge cost sorts last instead of wrapping around to best.
	pub(crate) fn charged(self, link_cost: u64) -> Self {
		Self {
			warm: self.warm.saturating_add(link_cost).min(MAX_COST),
			cold: self.cold.saturating_add(link_cost).min(MAX_COST),
		}
	}

	/// Clamp both magnitudes to what a varint can carry, since a locally created
	/// route can name an arbitrary `u64`.
	pub(crate) fn clamped(self) -> Self {
		Self {
			warm: self.warm.min(MAX_COST),
			cold: self.cold.min(MAX_COST),
		}
	}
}

impl From<u64> for Cost {
	fn from(cost: u64) -> Self {
		Self::new(cost)
	}
}

/// What a route covers: a path prefix, matched per segment.
///
/// `room/` covers `room/alice` (and `room/` itself), but a prefix is never half a
/// segment. Opaque so coverage can grow richer matching (wildcard patterns)
/// without a breaking change: construct one from a path and ask it questions.
/// Anything path-like converts into one, so `announce("room/", route)` reads
/// naturally.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Prefix(PathOwned);

impl Prefix {
	/// A prefix covering `path` and every path beneath it.
	pub fn new(prefix: impl AsPath) -> Self {
		Self(prefix.as_path().to_owned())
	}

	/// Whether this prefix covers `path`: it is a segment-wise prefix of it,
	/// including the exact path itself.
	pub fn covers(&self, path: impl AsPath) -> bool {
		path.as_path().has_prefix(&self.0)
	}

	/// The prefix as a path, e.g. to display, compare, or join.
	pub fn as_path(&self) -> Path<'_> {
		self.0.as_path()
	}
}

impl<T: AsPath> From<T> for Prefix {
	fn from(prefix: T) -> Self {
		Self::new(prefix)
	}
}

impl std::fmt::Display for Prefix {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.0.fmt(f)
	}
}

/// The path a route took through the mesh and what using it costs.
///
/// The metadata half of an advertisement: [`Producer::announce`] pairs it with
/// the [`Prefix`] it covers, and [`Consumer::announced`] yields both. A route
/// claims capability, not inventory: it says paths under its prefix are
/// servable, never that any specific broadcast exists. The common convention is
/// that a publisher announces each broadcast's exact path, so subscribers can
/// enumerate broadcasts; a service instead announces one short prefix and
/// answers whatever is requested beneath it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
	/// The chain of origins the route has traversed, oldest first. Each relay
	/// appends its own [`crate::Hop`] when forwarding; used for loop detection
	/// and as the selection tie-break.
	pub hops: Hops,

	/// What pulling content via this route costs, accumulated per link: lower wins,
	/// with ties broken by hop length, then a deterministic hash, and finally the
	/// most recently announced route. See [`Cost`].
	pub cost: Cost,
}

impl Route {
	/// Append a hop to the chain, oldest first.
	///
	/// Fails with [`crate::InvalidHop`] for a hop the wire would reject: one past the
	/// chain's length cap, or one already in it, which is a loop.
	pub fn with_hop(mut self, hop: Hop) -> Result<Self, InvalidHop> {
		self.hops.push(hop)?;
		Ok(self)
	}

	/// Replace the hop chain.
	pub fn with_hops(mut self, hops: Hops) -> Self {
		self.hops = hops;
		self
	}

	/// Set the cost: lower wins among routes covering the same prefix.
	///
	/// A bare `u64` prices the route undiscounted (both halves of [`Cost`] alike),
	/// which is what a publisher seeding its production cost means.
	pub fn with_cost(mut self, cost: impl Into<Cost>) -> Self {
		self.cost = cost.into();
		self
	}
}

static NEXT_CONSUMER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConsumerId(u64);

impl ConsumerId {
	fn new() -> Self {
		Self(NEXT_CONSUMER_ID.fetch_add(1, Ordering::Relaxed))
	}
}

// The origin-owned broadcast at a leaf: the spliced broadcast consumers see and
// the table of local sources feeding it. Local broadcasts are reachable by exact
// path; whether anything is *advertised* is a separate concern, owned by the
// route table (see [`Producer::announce`]).
struct OriginBroadcast {
	/// The shared, spliced broadcast; its `consume()` is what consumers get.
	broadcast: broadcast::Producer,
	/// The source table, shared with every source watcher and the front task.
	/// Also the broadcast's identity for stale-teardown checks.
	state: kio::Producer<FrontState>,
}

/// FNV-1a over a path and a sequence of origin ids.
///
/// FNV-1a, not the std hasher: its output is fixed across Rust versions and
/// builds, which matters when nodes run mismatched binaries during a rolling
/// deploy and still need to agree on the same route. SEED is a custom basis
/// (any nonzero u64 works, the textbook one is just as arbitrary); FNV_PRIME is
/// the standard FNV-64 prime and should stay put. Mixing the path in spreads
/// equal routes across different upstreams rather than funneling onto one.
fn fnv_key(name: &Path, origins: impl IntoIterator<Item = Hop>) -> u64 {
	const SEED: u64 = 0x420C0DECB00B; // 420 C0DEC B00B
	const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

	let mut hash = SEED;
	for &byte in name.as_str().as_bytes() {
		hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
	}
	for origin in origins {
		for &byte in &origin.id().to_le_bytes() {
			hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
		}
	}

	hash
}

/// Ordering key for a route entry covering one prefix. Lower wins: the cheapest
/// cost, then the shortest hop chain, then a deterministic hash of the prefix and
/// chain so every node converges on the same winner, and finally the newest
/// announcement, so a reconnect under an otherwise identical route wins the
/// moment it lands instead of after the transport retires the old session.
fn route_order(prefix: &Path, entry: &RouteEntry) -> (Cost, usize, u64, Reverse<u64>) {
	(
		entry.cost,
		entry.hops.len(),
		fnv_key(prefix, entry.hops.iter().copied()),
		Reverse(entry.id),
	)
}

/// The `(hops, cost)` metadata an announce cursor delivers alongside a prefix.
type RouteMeta = (Hops, Cost);

/// One coalesced update queued for an `AnnounceConsumer`.
///
/// At most one entry exists per prefix, so a slow consumer's pending set is
/// bounded by the number of distinct prefixes. A metadata change on a live route
/// overwrites the pending `Announce` (or is delivered as another active update),
/// while `UnannounceAnnounce` preserves a real retract-then-announce sequence.
enum PendingUpdate {
	Announce(RouteMeta),
	Unannounce(RouteMeta),
	UnannounceAnnounce { old: RouteMeta, new: RouteMeta },
}

/// Pending updates keyed by prefix. `BTreeMap` keeps memory strictly bounded by
/// the number of distinct prefixes with outstanding work (collapsed pairs are
/// fully erased) and gives a deterministic lexicographic delivery order so
/// tests can predict it.
#[derive(Default)]
struct OriginConsumerState {
	pending: BTreeMap<PathOwned, PendingUpdate>,
	/// Prefixes whose most recently delivered update was an announce. A pending
	/// `Announce` is ambiguous on its own: it is an unseen initial announce (a
	/// retraction cancels it entirely) or a metadata update on a route the
	/// consumer already observed (a retraction must still be delivered).
	delivered: BTreeSet<PathOwned>,
	/// Set by the origin's teardown: the cursor drains `pending`, then reports
	/// the end instead of parking forever on a table that can never fire again.
	ended: bool,
}

impl OriginConsumerState {
	fn apply_announce(&mut self, path: PathOwned, meta: RouteMeta) {
		let new = match self.pending.remove(&path) {
			// First announce, a stale announce being replaced, or a metadata update.
			None | Some(PendingUpdate::Announce(_)) => PendingUpdate::Announce(meta),
			// Consumer needs to observe the retraction before this announce.
			Some(PendingUpdate::Unannounce(old) | PendingUpdate::UnannounceAnnounce { old, .. }) => {
				PendingUpdate::UnannounceAnnounce { old, new: meta }
			}
		};
		self.pending.insert(path, new);
	}

	fn apply_unannounce(&mut self, path: PathOwned, last: RouteMeta) {
		match self.pending.remove(&path) {
			// The pending announce was never delivered and neither was any earlier
			// one, so the pair cancels entirely.
			Some(PendingUpdate::Announce(_)) if !self.delivered.contains(&path) => {}
			// Either nothing is pending or the pending announce was a metadata
			// update on a delivered route; the consumer still owes a retraction.
			None | Some(PendingUpdate::Announce(_) | PendingUpdate::Unannounce(_)) => {
				self.pending.insert(path, PendingUpdate::Unannounce(last));
			}
			// The embedded announce cancels with this retraction; the consumer still
			// needs the leading one.
			Some(PendingUpdate::UnannounceAnnounce { old, .. }) => {
				self.pending.insert(path, PendingUpdate::Unannounce(old));
			}
		}
	}

	/// Take one update to deliver to the consumer, if any.
	fn take(&mut self) -> Option<AnnounceUpdate> {
		let path = self.pending.keys().next()?.clone();
		let (meta, active) = match self.pending.remove(&path).unwrap() {
			PendingUpdate::Announce(meta) => {
				self.delivered.insert(path.clone());
				(meta, true)
			}
			PendingUpdate::Unannounce(meta) => {
				self.delivered.remove(&path);
				(meta, false)
			}
			PendingUpdate::UnannounceAnnounce { old, new } => {
				// Deliver the retraction now; leave the trailing announce pending so
				// the next take returns it for the same prefix.
				self.delivered.remove(&path);
				self.pending.insert(path.clone(), PendingUpdate::Announce(new));
				(old, false)
			}
		};
		Some(AnnounceUpdate {
			prefix: Prefix(path),
			route: Route {
				hops: meta.0,
				cost: meta.1,
			},
			active,
		})
	}
}

/// One announced route in the origin's table, absolute prefix.
struct RouteEntry {
	id: u64,
	prefix: PathOwned,
	hops: Hops,
	cost: Cost,
	/// The queue requests under this route are served from, when the announcer
	/// serves content on demand (a session). `None` for an advertise-only
	/// announcement, whose requests fall through to the origin's fallback handler.
	server: Option<kio::Shared<ServeState>>,
}

/// A per-announcer request queue: what materializes a requested path on demand.
///
/// Shared by every requester resolving through the owning route (or, for the
/// origin's fallback, every requester with no covering route) and the handler
/// draining it, so both sides work under one lock.
#[derive(Default)]
struct ServeState {
	// Result channels for pending requests, keyed by absolute path so concurrent
	// `request_broadcast` calls for the same path coalesce onto one channel.
	requests: Requests<PathOwned, kio::Producer<PendingBroadcast>>,

	// Broadcasts the handler has already served, kept weakly so a repeat request for the
	// same path resolves to a shared clone instead of re-invoking the handler (which would
	// open a duplicate upstream subscription). Weak so a served broadcast still closes once
	// its real consumers drop. The cache reclaims closed entries incrementally on insert, so a
	// long-lived origin serving many distinct one-shot paths stays bounded by the live count.
	served: WeakCache<PathOwned, broadcast::WeakConsumer>,

	// Set when the announcement is retracted or the origin tears down: new requests
	// fail immediately and the handler observes the end instead of parking forever.
	closed: bool,
}

/// Key of a remotely-served front: the absolute path and the requester's
/// split-horizon exclusion. Requesters excluding different peers get separate
/// fronts, so a front's failover never adopts a route flowing back through one
/// of its own readers.
type FrontKey = (PathOwned, Option<Hop>);

/// One remotely-served front in [`OriginState::fronts`]: the shared spliced
/// broadcast at a path plus the channel requesters resolve through.
#[derive(Clone)]
struct RemoteFront {
	/// Resolves requesters with the front's consumer (or the error that ended it
	/// unresolved). The producer lives here so the teardown can reject requesters
	/// still parked on a front whose watcher was cancelled.
	request: kio::Producer<PendingBroadcast>,
	/// The front's spliced broadcast, weak: dead once its watcher exits, so a
	/// later request re-creates the front instead of joining a corpse.
	broadcast: broadcast::WeakConsumer,
}

impl WeakEntry for RemoteFront {
	fn is_closed(&self) -> bool {
		self.broadcast.is_closed()
	}

	fn same_channel(&self, other: &Self) -> bool {
		self.broadcast.same_channel(&other.broadcast)
	}
}

/// One registered announce cursor: which prefixes it may see, how they are
/// re-rooted, and the per-cursor delivery buffer.
struct TableCursor {
	/// The prefix stripped from every delivered path.
	root: PathOwned,
	/// The absolute prefixes this cursor is scoped to (its token / scope). A route
	/// is visible where it intersects one of these, clamped to the intersection.
	allowed: Vec<PathOwned>,
	/// Skip routes whose hop chain contains this peer (control-plane split horizon).
	exclude: Option<Hop>,
	/// The delivery buffer, drained by the cursor's `poll_next`.
	state: kio::Producer<OriginConsumerState>,
	/// The last delivered best route per presented (absolute) prefix, for change
	/// detection: `(entry id, hops, cost)`.
	// entry id, metadata, and whether the entry could serve requests: the last
	// is part of the dedupe key (see `sync_cursor`) but never leaves the model.
	current: HashMap<PathOwned, (u64, RouteMeta, bool)>,
}

impl TableCursor {
	/// Where `prefix` presents on this cursor: the intersection of the route's
	/// prefix with each allowed scope, absolute. Empty when disjoint.
	fn presented(&self, prefix: &Path) -> Vec<PathOwned> {
		self.allowed
			.iter()
			.filter_map(|allowed| intersect_prefix(prefix, &allowed.as_path()))
			.collect()
	}

	/// Whether this cursor may observe `entry` at all (split horizon).
	fn visible(&self, entry: &RouteEntry) -> bool {
		match self.exclude {
			Some(peer) if peer != Hop::UNKNOWN => !entry.hops.contains(&peer),
			_ => true,
		}
	}
}

/// The intersection of two path prefixes: the longer one when one contains the
/// other (segment-wise), `None` when they are disjoint.
fn intersect_prefix(a: &Path, b: &Path) -> Option<PathOwned> {
	if a.has_prefix(b) {
		Some(a.to_owned())
	} else if b.has_prefix(a) {
		Some(b.to_owned())
	} else {
		None
	}
}

struct OriginNode {
	// The origin-owned broadcast published at this node, if any (see
	// [`Producer::create_broadcast`]).
	broadcast: Option<OriginBroadcast>,

	// Nested nodes, one level down the tree.
	nested: HashMap<String, Lock<OriginNode>>,
}

impl OriginNode {
	fn new() -> Self {
		Self {
			broadcast: None,
			nested: HashMap::new(),
		}
	}

	fn leaf(&mut self, path: &Path) -> Lock<OriginNode> {
		let (dir, rest) = path.next_part().expect("leaf called with empty path");

		let next = self.entry(dir);
		if rest.is_empty() { next } else { next.lock().leaf(&rest) }
	}

	fn entry(&mut self, dir: &str) -> Lock<OriginNode> {
		match self.nested.get(dir) {
			Some(next) => next.clone(),
			None => {
				let next = Lock::new(OriginNode::new());
				self.nested.insert(dir.to_string(), next.clone());
				next
			}
		}
	}

	fn resolve_broadcast(&self, rest: impl AsPath) -> Option<broadcast::Consumer> {
		let rest = rest.as_path();

		if let Some((dir, rest)) = rest.next_part() {
			let node = self.nested.get(dir)?;
			let node = node.lock();
			return node.resolve_broadcast(&rest);
		}

		Some(self.broadcast.as_ref()?.broadcast.consume())
	}

	/// Remove the broadcast at `relative` if it is `expect`, pruning empty nodes on
	/// the way back up. The identity check keeps a stale teardown from clobbering a
	/// replacement.
	fn remove(&mut self, expect: &kio::Producer<FrontState>, relative: impl AsPath) {
		let relative = relative.as_path();

		if let Some((dir, relative)) = relative.next_part() {
			let Some(nested) = self.nested.get(dir) else { return };
			let nested = nested.clone();
			let mut locked = nested.lock();
			locked.remove(expect, &relative);

			if locked.is_empty() {
				drop(locked);
				self.nested.remove(dir);
			}
		} else if let Some(existing) = &self.broadcast
			&& existing.state.same_channel(expect)
		{
			self.broadcast = None;
		}
	}

	fn is_empty(&self) -> bool {
		self.broadcast.is_none() && self.nested.is_empty()
	}
}

#[derive(Clone)]
struct OriginNodes {
	nodes: Vec<(PathOwned, Lock<OriginNode>)>,
}

impl OriginNodes {
	// Returns nested roots that match the prefixes.
	// PathPrefixes guarantees no duplicates or overlapping prefixes.
	pub fn select(&self, prefixes: &PathPrefixes) -> Option<Self> {
		let mut roots = Vec::new();

		for (root, state) in &self.nodes {
			for prefix in prefixes {
				if root.has_prefix(prefix) {
					// Keep the existing node if we're allowed to access it.
					roots.push((root.to_owned(), state.clone()));
					continue;
				}

				if let Some(suffix) = prefix.strip_prefix(root) {
					// If the requested prefix is larger than the allowed prefix, then we further scope it.
					let nested = state.lock().leaf(&suffix);
					roots.push((prefix.to_owned(), nested));
				}
			}
		}

		if roots.is_empty() {
			None
		} else {
			Some(Self { nodes: roots })
		}
	}

	pub fn root(&self, new_root: impl AsPath) -> Option<Self> {
		let new_root = new_root.as_path();
		let mut roots = Vec::new();

		if new_root.is_empty() {
			return Some(self.clone());
		}

		for (root, state) in &self.nodes {
			if let Some(suffix) = root.strip_prefix(&new_root) {
				// If the old root is longer than the new root, shorten the keys.
				roots.push((suffix.to_owned(), state.clone()));
			} else if let Some(suffix) = new_root.strip_prefix(root) {
				// If the new root is longer than the old root, add a new root.
				// NOTE: suffix can't be empty
				let nested = state.lock().leaf(&suffix);
				roots.push(("".into(), nested));
			}
		}

		if roots.is_empty() {
			None
		} else {
			Some(Self { nodes: roots })
		}
	}

	// Returns the root that has this prefix.
	pub fn get(&self, path: impl AsPath) -> Option<(Lock<OriginNode>, PathOwned)> {
		let path = path.as_path();

		for (root, state) in &self.nodes {
			if let Some(suffix) = path.strip_prefix(root) {
				return Some((state.clone(), suffix.to_owned()));
			}
		}

		None
	}
}

impl Default for OriginNodes {
	fn default() -> Self {
		Self {
			nodes: vec![("".into(), Lock::new(OriginNode::new()))],
		}
	}
}

/// A route announcement or retraction, delivered by [`AnnounceConsumer`].
///
/// An announcement carries no broadcast: it advertises that content under
/// [`prefix`](Self::prefix) is servable. Resolve a specific path with
/// [`Consumer::request_broadcast`]; the application decides which paths name
/// broadcasts.
#[derive(Clone, Debug)]
pub struct AnnounceUpdate {
	/// What the route covers, relative to the consuming cursor's root. A route
	/// announced above the cursor's scope is clamped to that scope, which is the
	/// exact set of covered paths the cursor may see.
	pub prefix: Prefix,
	/// The route serving the prefix. On a retraction this carries its last
	/// advertised metadata.
	pub route: Route,
	/// `false` when the route was retracted. A repeated `true` for the same
	/// prefix is a metadata update (new hops or cost), delivered in place.
	pub active: bool,
}

/// Publishes broadcasts and announces routes into an origin.
#[derive(Clone)]
pub struct Producer {
	// Identity for this origin. Appended to route hops when re-announcing so
	// downstream relays can detect loops and prefer the shortest path.
	info: Hop,

	// The roots of the tree that we are allowed to publish.
	// A path of "" means we can publish anything.
	nodes: OriginNodes,

	// The prefix that is automatically stripped from all paths.
	root: PathOwned,

	// The origin's shared state: the route table, announce cursors, and the
	// fallback request queue. Shared with every derived consumer.
	shared: kio::Shared<OriginState>,

	// The cache pool inherited by broadcasts created under this origin (sessions
	// mint their remote broadcasts with it). Unbounded by default.
	pool: cache::Pool,

	// Retention ceiling inherited by broadcasts created under this origin (see
	// [`Info::cache_duration`]). `Duration::MAX` (no ceiling) by default.
	cache_duration: Duration,

	// Retention window for a track whose publisher advertises none (see
	// [`Info::default_max_age`]).
	default_max_age: Duration,

	// Ingress stats context. Broadcasts created through this producer are attributed
	// to it (writes counted on the subscriber/ingress side). Empty (no-op) unless a
	// session tagged this handle via [`Self::with_stats`].
	stats: stats::Session,

	// Submission handle to the origin's [`Driver`]: source watchers, fronts, and
	// serve tasks queued here run when the driver is polled. Closed once the
	// driver drops, which is what makes later mutations fail with `Closed`.
	tasks: Tasks,

	// The driver's clock and timers, installed by [`Driver::run`].
	timers: TimersSlot,
}

impl std::ops::Deref for Producer {
	type Target = Hop;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Producer {
	/// Build a producer from an [`Info`] (identity + cache pool) with no scoped
	/// prefix and no pre-existing broadcasts, paired with the [`Driver`] that runs
	/// the origin's lifecycle work.
	///
	/// Hand the driver a [`crate::Timers`] via [`Driver::run`] and poll the
	/// returned [`Run`] (spawn it, await it, or step [`Run::poll`]) for the
	/// origin to make progress; see the [`Driver`] docs for the exact contract.
	/// `moq_tokio::origin::spawn` wraps this for tokio callers.
	pub fn new(info: Info) -> (Self, Driver) {
		let (tasks, set) = TaskSet::new();
		let nodes = OriginNodes::default();
		let shared = kio::Shared::<OriginState>::default();
		let timers = TimersSlot::default();
		let producer = Self {
			info: info.id,
			nodes: nodes.clone(),
			root: PathOwned::default(),
			shared: shared.clone(),
			pool: info.pool,
			cache_duration: info.cache_duration,
			default_max_age: info.default_max_age,
			stats: stats::Session::default(),
			tasks,
			timers: timers.clone(),
		};
		let driver = Driver {
			state: DriverState {
				set,
				nodes,
				shared,
				done: false,
			},
			timers,
		};
		(producer, driver)
	}

	/// Attach an ingress stats context: broadcasts created through this handle (and
	/// any handle derived from it) are attributed to `session` on the subscriber
	/// (ingress) side. Pass [`stats::Session::default`] to opt out.
	pub fn with_stats(mut self, session: stats::Session) -> Self {
		self.stats = session;
		self
	}

	/// This origin's [`Info`] (identity + cache pool), the parent handle a broadcast
	/// created under this origin carries (see [`broadcast::Info::origin`]).
	pub fn info(&self) -> Info {
		Info {
			id: self.info,
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			default_max_age: self.default_max_age,
		}
	}

	// The retention window for a track whose publisher advertises none (see
	// [`Info::default_max_age`]). Cheaper than `info()`, which clones the pool.
	pub(crate) fn default_max_age(&self) -> Duration {
		self.default_max_age
	}

	/// A producer with *no* allowed prefixes: it can't publish anything and
	/// advertises no subscribe interest (its `allowed()` is empty, so the
	/// subscriber issues no ANNOUNCE_PLEASE). Used to fill an unset session half
	/// so both the publisher and subscriber loops still run.
	pub(crate) fn empty(info: Hop) -> Self {
		// No allowed prefixes means no broadcast is ever created, so nothing will
		// ever be queued on the detached submission handle.
		let (tasks, _) = TaskSet::new();
		Self {
			info,
			nodes: OriginNodes { nodes: Vec::new() },
			root: PathOwned::default(),
			shared: kio::Shared::default(),
			pool: cache::Pool::default(),
			cache_duration: Duration::MAX,
			default_max_age: track::DEFAULT_MAX_AGE,
			stats: stats::Session::default(),
			tasks,
			timers: TimersSlot::default(),
		}
	}

	/// Create a broadcast at `path`, fed through the returned producer.
	///
	/// This is how local content enters an origin. The returned
	/// [`broadcast::Producer`] is a source: the origin owns the broadcast
	/// consumers actually see, and splices its tracks across every source created
	/// at the same path, preferring the newest. When the serving source changes,
	/// tracks resume from the replacement at the first missing group; consumers
	/// never observe the swap.
	///
	/// The broadcast is *not* advertised: it is reachable by exact path for
	/// subscribes and fetches. Advertise it (or a whole prefix of paths) separately
	/// with [`Self::announce`]; the two are independent, so cached or on-demand
	/// content can stay reachable without ever being announced.
	///
	/// The broadcast is visible to exact lookups before this returns; only
	/// lifecycle work (track serving, teardown) waits for the [`Driver`] to be
	/// polled. Register a [`broadcast::Producer::dynamic`] handler right away, so
	/// the first consumer finds the tracks it serves.
	///
	/// End the broadcast with [`broadcast::Producer::finish`]; dropping it
	/// without finishing also works, but logs a warning. Either way the path
	/// closes once it was the last source; an unfinished drop additionally aborts
	/// the spliced tracks with an error, so consumers observe a failure rather
	/// than a clean end.
	///
	/// Fails with [`Error::Unauthorized`] if `path` is outside the prefixes this
	/// producer may publish under (after [`scope`](Self::scope) /
	/// [`with_root`](Self::with_root)), [`Error::BoundsExceeded`] if the full
	/// rooted path exceeds [`Path::MAX_PARTS`], or [`Error::Closed`] once the
	/// origin's [`Driver`] has been dropped.
	pub fn create_broadcast(&self, path: impl AsPath) -> Result<broadcast::Producer, Error> {
		let path = path.as_path();

		// Held across the whole attach: the driver's teardown sets `closed` under
		// this lock, so a create either completes before the teardown (whose walk
		// then cleans the entry up) or observes `closed` here and fails.
		let lifecycle = self.shared.lock();
		if lifecycle.closed {
			return Err(Error::Closed);
		}

		let (node, rest) = self.nodes.get(&path).ok_or(Error::Unauthorized)?;
		let full = self.root.join(&path).to_owned();

		// A decoded prefix and suffix are each within the wire limit, but their
		// join might not be. Enforcing here bounds the tree depth and guarantees the path
		// can be re-encoded when forwarded.
		if full.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}

		// Resolve the ingress counters once, keyed by the absolute broadcast path.
		let ingress = self.stats.ingress(&full);

		let source = broadcast::Info {
			origin: self.info(),
			path: full.clone(),
		}
		.produce()
		.with_stats(ingress.clone());
		let consumer = source.consume();

		// Attach synchronously: the source is visible to exact lookups before this
		// returns; only lifecycle work needs the driver.
		let origin = self.info();
		let ctx = AttachContext {
			origin: &origin,
			node: &node,
			full: &full,
			rest: &rest,
			tasks: &self.tasks,
			timers: &self.timers,
		};
		let leaf = if rest.is_empty() {
			node.clone()
		} else {
			node.lock().leaf(&rest)
		};
		let (state, broadcast, id) = attach_source(&ctx, &leaf, &consumer);

		self.tasks.push(run_source(SourceTask {
			source: consumer,
			timers: self.timers.clone(),
			leaf,
			state,
			broadcast,
			id,
		}));
		drop(lifecycle);

		Ok(source)
	}

	/// Mint a standalone source broadcast for a served-route request: it carries
	/// this origin's identity (cache pool included) and ingress attribution, but
	/// is *not* inserted into the broadcast tree. Sessions answer
	/// [`RouteServer`] requests with one of these; the requester already holds
	/// the request's result channel, so the tree never needs to resolve it.
	pub(crate) fn create_source(&self, path: impl AsPath) -> broadcast::Producer {
		let path = path.as_path();
		let full = self.root.join(&path).to_owned();
		let ingress = self.stats.ingress(&full);
		broadcast::Info {
			origin: self.info(),
			path: full,
		}
		.produce()
		.with_stats(ingress)
	}

	/// Advertise a route: a claim that paths under `prefix` can be served.
	///
	/// The advertisement is visible to [`Consumer::announced`] and forwarded by
	/// sessions until the returned [`AnnounceProducer`] is dropped. Announcing is
	/// independent of [`Self::create_broadcast`], and the right order is
	/// create-populate-announce: announce each broadcast's exact path once its
	/// tracks exist so subscribers can enumerate broadcasts, or announce one short
	/// prefix and serve requests beneath it via [`Self::dynamic`].
	///
	/// The prefix is clamped to the intersection with this producer's allowed
	/// scope, so a broad route announced through a narrow token advertises exactly
	/// what the token may serve. Fails with [`Error::Unauthorized`] when they are
	/// disjoint, and [`Error::Closed`] once the origin's [`Driver`] has been
	/// dropped.
	pub fn announce(&self, prefix: impl Into<Prefix>, route: Route) -> Result<AnnounceProducer, Error> {
		self.announce_inner(prefix.into(), route, None)
	}

	/// [`Self::announce`], plus a request queue: a consumer resolving a path under
	/// this route is handed to the returned server to materialize on demand.
	/// Sessions use this for the routes a peer announces to them.
	pub(crate) fn announce_served(
		&self,
		prefix: impl Into<Prefix>,
		route: Route,
	) -> Result<(AnnounceProducer, RouteServer), Error> {
		let serve = kio::Shared::<ServeState>::default();
		serve.lock().requests.add_handler();
		let server = RouteServer {
			root: self.root.clone(),
			state: serve.clone(),
		};
		let announcement = self.announce_inner(prefix.into(), route, Some(serve))?;
		Ok((announcement, server))
	}

	fn announce_inner(
		&self,
		prefix: Prefix,
		route: Route,
		server: Option<kio::Shared<ServeState>>,
	) -> Result<AnnounceProducer, Error> {
		debug_assert!(
			!route.hops.contains(&self.info),
			"announce called with a looping hop chain",
		);

		let meta: RouteMeta = (route.hops, route.cost);

		// Clamp the prefix against each allowed root: the intersection is exactly
		// the set of covered paths this producer may claim. One entry per
		// intersecting root (a broad route through a multi-prefix token covers each
		// of them).
		let requested = self.root.join(prefix.as_path()).to_owned();
		if requested.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}
		let prefixes: Vec<PathOwned> = self
			.nodes
			.nodes
			.iter()
			.filter_map(|(allowed, _)| {
				let allowed = self.root.join(allowed).to_owned();
				intersect_prefix(&requested.as_path(), &allowed.as_path())
			})
			.collect();
		if prefixes.is_empty() {
			return Err(Error::Unauthorized);
		}

		let mut shared = self.shared.lock();
		if shared.closed {
			return Err(Error::Closed);
		}

		let mut ids = Vec::with_capacity(prefixes.len());
		for prefix in prefixes {
			let id = shared.next_route;
			shared.next_route += 1;
			shared.routes.push(RouteEntry {
				id,
				prefix: prefix.clone(),
				hops: meta.0.clone(),
				cost: meta.1,
				server: server.clone(),
			});
			shared.sync_route(&prefix.as_path());
			ids.push(id);
		}
		drop(shared);

		// Ingress announce guard: held for the announcement's lifetime.
		let guard = self.stats.ingress(&requested).announce();

		Ok(AnnounceProducer {
			shared: self.shared.clone(),
			ids,
			_guard: guard,
		})
	}

	/// Returns a new Producer restricted to publishing under one of `prefixes`.
	///
	/// Returns None if there are no legal prefixes (the requested prefixes are
	/// disjoint from this producer's current scope).
	// TODO accept PathPrefixes instead of &[Path]
	pub fn scope(&self, prefixes: &[Path]) -> Option<Producer> {
		let prefixes = PathPrefixes::new(prefixes);
		Some(Producer {
			info: self.info,
			nodes: self.nodes.select(&prefixes)?,
			root: self.root.clone(),
			shared: self.shared.clone(),
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			default_max_age: self.default_max_age,
			stats: self.stats.clone(),
			tasks: self.tasks.clone(),
			timers: self.timers.clone(),
		})
	}

	/// Create a dynamic handler that picks up [`Consumer::request_broadcast`]
	/// calls no local broadcast or served route resolves.
	///
	/// This is the origin-level analogue of [`broadcast::Producer::dynamic`]: it serves
	/// broadcasts on demand rather than tracks. The served broadcasts are *not*
	/// announced; pair the handler with [`Producer::announce`] to advertise the
	/// prefix it answers under. Drop the handler (and every clone) to reject
	/// pending requests.
	pub fn dynamic(&self) -> Dynamic {
		Dynamic::new(self.info, self.root.clone(), self.shared.clone())
	}

	/// Cheap read handle over this origin's broadcast tree.
	///
	/// Use [`Consumer::announced`] to register interest and start receiving
	/// announcement events; the consumer itself does not allocate any channels.
	pub fn consume(&self) -> Consumer {
		// Untagged: a session tags the egress consumer separately via
		// `origin::Consumer::with_stats` (ingress and egress are distinct sides).
		Consumer::from_producer(self, stats::Session::default())
	}

	/// Returns a new Producer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when [`Self::scope`]
	/// was already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self {
			info: self.info,
			root: self.root.join(&prefix).to_owned(),
			nodes: self.nodes.root(&prefix)?,
			shared: self.shared.clone(),
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			default_max_age: self.default_max_age,
			stats: self.stats.clone(),
			tasks: self.tasks.clone(),
			timers: self.timers.clone(),
		})
	}

	/// Returns the root that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// Iterate over the path prefixes this handle is permitted to publish or subscribe under.
	// TODO return PathPrefixes
	pub fn allowed(&self) -> impl Iterator<Item = &Path<'_>> {
		self.nodes.nodes.iter().map(|(root, _)| root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

/// The write half of an advertisement, from [`Producer::announce`]: a live
/// claim that paths under a [`Prefix`] can be served.
///
/// Hold it for as long as the route should stay advertised and
/// [`update`](Self::update) it to re-price; dropping it retracts the route, which
/// [`AnnounceConsumer`]s observe and sessions withdraw from their peers.
#[must_use = "dropping an announce::Producer retracts the route"]
pub struct AnnounceProducer {
	shared: kio::Shared<OriginState>,
	/// The table entries this advertisement created: one per allowed root the
	/// requested prefix intersected.
	ids: Vec<u64>,
	/// Ingress announce stats guard, held for the advertisement's lifetime.
	_guard: stats::Announce,
}

impl AnnounceProducer {
	/// Re-price the route in place: replace its hops and cost.
	///
	/// Consumers observe another active update for the same prefix; sessions
	/// forward it as a restart, so route churn never looks like new content. The
	/// prefix is fixed at announce time and a [`Route`] cannot name one: to move
	/// an advertisement, drop this and announce again. Fails with
	/// [`Error::Closed`] once the origin's [`Driver`] has been dropped.
	pub fn update(&self, route: Route) -> Result<(), Error> {
		let mut shared = self.shared.lock();
		if shared.closed {
			return Err(Error::Closed);
		}
		for id in &self.ids {
			// Each entry keeps its clamped prefix; only the metadata moves.
			let Some(entry) = shared.routes.iter_mut().find(|entry| entry.id == *id) else {
				continue;
			};
			entry.hops = route.hops.clone();
			entry.cost = route.cost;
			let prefix = entry.prefix.clone();
			shared.sync_route(&prefix.as_path());
		}
		Ok(())
	}
}

impl Drop for AnnounceProducer {
	fn drop(&mut self) {
		let mut shared = self.shared.lock();
		for id in &self.ids {
			let Some(index) = shared.routes.iter().position(|entry| entry.id == *id) else {
				continue;
			};
			let entry = shared.routes.swap_remove(index);
			// Reject anything still waiting on this route's server; a request
			// already handed to the handler resolves through its own `Request`.
			if let Some(server) = &entry.server {
				let mut server = server.lock();
				server.closed = true;
				for producer in server.requests.drain_all() {
					if let Ok(mut request) = producer.write() {
						request.resolved.get_or_insert(Err(Error::Unroutable));
					}
				}
			}
			shared.sync_route(&entry.prefix.as_path());
		}
	}
}

/// The request queue behind a served route, from [`Producer::announce_served`].
///
/// Sessions poll it for the paths consumers resolve under the route and
/// materialize each on demand. Dropping it (without the announcement) leaves the
/// route advertised but unservable; drop both to retract.
pub(crate) struct RouteServer {
	root: PathOwned,
	state: kio::Shared<ServeState>,
}

impl RouteServer {
	/// Poll for the next requested path under this route, without blocking.
	///
	/// Returns [`Error::Closed`] once the announcement is retracted or the origin
	/// torn down: no request will ever arrive again, so server loops should end.
	pub fn poll_requested_broadcast(&mut self, waiter: &kio::Waiter) -> Poll<Result<Request, Error>> {
		let mut state = ready!(self.state.poll(waiter, |state| {
			if state.closed || state.requests.has_queued() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));

		if state.closed {
			return Poll::Ready(Err(Error::Closed));
		}

		let path = state.requests.pop().expect("predicate guaranteed a request");
		let producer = state.requests.get(&path).expect("popped key must be pending").clone();
		Poll::Ready(Ok(Request {
			path,
			producer,
			home: RequestHome::Route(self.state.clone()),
		}))
	}

	/// Returns the prefix that is automatically stripped from requested paths.
	#[allow(dead_code)]
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}
}

impl Drop for RouteServer {
	fn drop(&mut self) {
		let mut state = self.state.lock();
		if state.requests.remove_handler() {
			state.closed = true;
			for producer in state.requests.drain_all() {
				if let Ok(mut request) = producer.write() {
					request.resolved.get_or_insert(Err(Error::Unroutable));
				}
			}
		}
	}
}

/// The origin's lifecycle work, waiting for the [`crate::Timers`] it runs on.
///
/// Returned by [`Producer::new`] alongside the producer. Call
/// [`run`](Self::run) with the timers that arm its deadlines (linger, handover
/// holds) and poll the returned [`Run`] for the life of the origin. Route
/// changes, track serving, linger timers, failover, and teardown all run there:
/// exact lookups and eligible announcements still update synchronously in
/// [`Producer::create_broadcast`], but nothing else makes progress without
/// polling.
///
/// It holds no [`Producer`] clone, so it never keeps the origin alive.
/// Dropping it (before or after `run`) tears the origin down immediately:
/// active fronts abort with [`Error::Dropped`], pending dynamic requests are
/// rejected, announced paths unannounce and announcement cursors end, and later
/// producer mutations fail with [`Error::Closed`].
///
/// `moq_tokio::origin::spawn` wraps construction, `run`, and spawning for
/// tokio callers.
#[must_use = "call Driver::run and poll the result or the origin makes no progress"]
pub struct Driver {
	state: DriverState,
	// The producer's slot, filled by `run` so lifecycle work can mint deadlines.
	timers: TimersSlot,
}

/// Everything the driver polls and tears down, split from the park so the two
/// borrow disjointly.
struct DriverState {
	/// Source watchers, fronts, and serve tasks: producers submit, this polls.
	set: TaskSet,
	/// The whole broadcast tree, for the teardown walk on drop.
	nodes: OriginNodes,
	/// The route table, announce cursors, and the fallback request queue, for
	/// ending everything on drop.
	shared: kio::Shared<OriginState>,
	/// Cached completion so a poll after `Ready` doesn't re-poll the drained set.
	done: bool,
}

impl Driver {
	/// Install the timers and return the runnable driver.
	///
	/// The origin's lifecycle work stamps instants and arms deadlines against
	/// `timers`; nothing runs until the returned [`Run`] is polled.
	pub fn run<T>(self, timers: T) -> Run
	where
		T: crate::runtime::Timers + MaybeSend + MaybeSync + 'static,
		T::Timer: MaybeSend + 'static,
	{
		self.timers.install(AnyTimers::new(timers));
		Run {
			state: self.state,
			park: kio::Park::default(),
		}
	}
}

/// The future running an origin's lifecycle work, from [`Driver::run`].
///
/// Poll it for the life of the origin, either by `.await`ing it (typically
/// spawned on an executor) or by stepping [`poll`](Self::poll) from inside
/// another [`kio`]-style poll function.
///
/// It holds no [`Producer`] clone, so it never keeps the origin alive: it
/// resolves once every producer handle has dropped and the already-submitted
/// lifecycle work has drained, and keeps returning `Ready` if polled again.
/// Dropping it tears the origin down immediately, exactly like dropping the
/// [`Driver`] it came from.
#[must_use = "poll the driver (spawn or await it) or the origin makes no progress"]
pub struct Run {
	state: DriverState,
	// Retains the waiter across `Future` polls so its kio registrations stay live.
	// Kept out of `DriverState` so the borrow `hold` hands back doesn't collide
	// with the `&mut` that polling the state needs.
	park: kio::Park,
}

impl Run {
	/// Drive the origin one step, registering `waiter` for the next wakeup.
	///
	/// The `poll_*` counterpart of `.await`ing, for callers composing the driver
	/// into their own [`kio`]-style poll functions.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll(waiter)
	}
}

impl DriverState {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if !self.done {
			ready!(self.set.poll(waiter));
			self.done = true;
		}
		Poll::Ready(())
	}

	/// Tear the origin down: cancel the lifecycle work, abort and unpublish every
	/// front, retract every route, end announcement cursors, and reject pending
	/// requests.
	fn teardown(&mut self) {
		// Cancel queued and running lifecycle work first, so nothing re-attaches
		// or serves while the walks below empty the tree.
		drop(std::mem::replace(&mut self.set, TaskSet::owned()));

		// Refuse new work and take the pending requests, under the same lock
		// `create_broadcast` holds across its attach: a concurrent create either
		// finishes before this (the walk below cleans its entry up) or observes
		// `closed` and fails with `Closed`.
		let (pending, servers, cursors, fronts) = {
			let mut shared = self.shared.lock();
			shared.closed = true;
			let servers: Vec<_> = shared.routes.iter().filter_map(|entry| entry.server.clone()).collect();
			let cursors: Vec<_> = shared.cursors.values().map(|cursor| cursor.state.clone()).collect();
			let fronts: Vec<_> = shared.fronts.values().map(|front| front.request.clone()).collect();
			(shared.fallback.drain_all(), servers, cursors, fronts)
		};

		// Reject every pending request, including those already handed to a
		// handler: the teardown is terminal, so a handler resolving late must not
		// beat it (resolution is first-write-wins).
		for producer in pending {
			if let Ok(mut request) = producer.write() {
				request.resolved.get_or_insert(Err(Error::Dropped));
			}
		}

		// Reject requesters still parked on a remote front's channel: its watcher
		// was cancelled above and will never resolve them.
		for producer in fronts {
			if let Ok(mut request) = producer.write() {
				request.resolved.get_or_insert(Err(Error::Dropped));
			}
		}
		for server in servers {
			let mut server = server.lock();
			server.closed = true;
			for producer in server.requests.drain_all() {
				if let Ok(mut request) = producer.write() {
					request.resolved.get_or_insert(Err(Error::Dropped));
				}
			}
		}

		// End the announce cursors: each drains its pending updates, then reports
		// the end. Registrations stay (the cursors remove themselves on drop).
		for state in cursors {
			if let Ok(mut state) = state.write() {
				state.ended = true;
			}
		}

		for (_, node) in &self.nodes.nodes {
			teardown_broadcasts(node);
		}
	}
}

impl Drop for DriverState {
	fn drop(&mut self) {
		self.teardown();
	}
}

impl Future for Run {
	type Output = ();

	fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
		let this = &mut *self;
		// Disjoint field borrows: `hold` borrows the park for as long as the
		// waiter lives, while the state is polled through its own `&mut`.
		let waiter = this.park.hold(cx);
		this.state.poll(waiter)
	}
}

/// Abort and unpublish every broadcast under `node` with [`Error::Dropped`]. The
/// lifecycle tasks are already cancelled, so this finishes the teardown they
/// would have run.
fn teardown_broadcasts(node: &Lock<OriginNode>) {
	let (entry, children) = {
		let mut guard = node.lock();
		let children: Vec<_> = guard.nested.values().cloned().collect();
		(guard.broadcast.take(), children)
	};
	if let Some(mut entry) = entry {
		// Close the front so anything still holding its table observes the end.
		if let Ok(mut state) = entry.state.write() {
			state.closed = true;
		}
		entry.broadcast.abort_spliced(Error::Dropped);
		entry.broadcast.finish();
	}
	for child in children {
		teardown_broadcasts(&child);
	}
}

/// How long a spliced track stays warm after its last reader leaves.
///
/// Within the window a returning viewer, or the next of a run of back-to-back
/// fetches, reuses the source's copy: no new track request, and no second round
/// trip for its `TRACK_INFO`. After it, the copy is released so an idle track
/// costs nothing upstream.
///
/// Sized above the fetch cadence of a segmented consumer: HLS polls every
/// `TARGETDURATION` seconds, commonly 6 or 10, so a shorter window would drop the
/// copy between every segment and re-request the track each time. A warm copy
/// holds no upstream subscription (that is canceled as soon as demand ends), so
/// waiting longer costs cached state, not a viewer.
const TRACK_IDLE_LINGER: Duration = Duration::from_secs(30);

/// One attached source in a [`FrontState`] table.
struct FrontSource {
	id: u64,
	/// The source broadcast tracks are served from.
	source: broadcast::Consumer,
}

/// Shared state behind a front: the attached local sources and which one is
/// active.
struct FrontState {
	/// Attach counter, handed to each [`FrontSource`] so selection can break a
	/// tie toward the newest source.
	next_source: u64,
	sources: Vec<FrontSource>,
	/// The source tracks are dispatched to: the newest attached. Backups park
	/// until promoted.
	active: Option<u64>,
	/// Terminal: no more sources may attach and every poller stops. Set
	/// synchronously by the detach that empties the table.
	closed: bool,
}

impl FrontState {
	/// The newest attached source: the one new work dispatches to. Local sources
	/// carry no route metadata, so recency is the whole order: a publisher
	/// re-creating a path over a fresh handle wins the moment it attaches instead
	/// of waiting for the old handle to be torn down.
	fn best_source(&self) -> Option<u64> {
		self.sources.iter().map(|s| s.id).max()
	}

	/// The source one track should be served from: the front's active source
	/// unless `skip` rules it out, then the newest source that survives.
	///
	/// Whether a source carries a given track is a per-track property (a standby
	/// that has not created it yet, a publisher whose encoder is still starting),
	/// so a source refusing one track is ruled out of that track only, never out
	/// of the front.
	fn serve_route(&self, skip: impl Fn(u64) -> bool) -> Option<u64> {
		if let Some(active) = self.active
			&& !skip(active)
			&& self.sources.iter().any(|s| s.id == active)
		{
			return Some(active);
		}
		self.sources.iter().map(|s| s.id).filter(|id| !skip(*id)).max()
	}

	/// Re-pick the active source after the table changed. Serve tasks watch
	/// `active` and re-splice on their own, so a replacement takes over seamlessly
	/// at a group boundary.
	fn reselect(&mut self) {
		self.active = self.best_source();
	}
}

/// Detach source `id`, promoting the newest remaining source; the tracks it was
/// serving re-splice on their own. Idempotent.
///
/// Detaching the last source closes the broadcast synchronously, however the
/// source ended, which guarantees a following create at the path is a *new*
/// broadcast rather than splicing new content into this one.
fn detach_source(state: &kio::Producer<FrontState>, broadcast: &broadcast::Producer, leaf: &Lock<OriginNode>, id: u64) {
	let close = {
		let Ok(mut s) = state.write() else { return };
		let Some(pos) = s.sources.iter().position(|entry| entry.id == id) else {
			return;
		};
		s.sources.remove(pos);
		s.reselect();
		if s.sources.is_empty() && !s.closed {
			// Last one out: close now. The front task observes `closed` and
			// finishes the teardown (unpublish).
			s.closed = true;
			true
		} else {
			false
		}
	};
	if close {
		broadcast.abort_spliced(Error::Dropped);
	}
	// The tree is pruned by the front task once it observes the close.
	let _ = leaf;
}

/// Everything a queued source watcher continues with after
/// [`Producer::create_broadcast`] performed the synchronous attach.
struct SourceTask {
	/// The source broadcast, watched for its end.
	source: broadcast::Consumer,
	timers: TimersSlot,
	/// The leaf the attach landed on.
	leaf: Lock<OriginNode>,
	/// The front's source table.
	state: kio::Producer<FrontState>,
	/// The spliced broadcast the front serves.
	broadcast: broadcast::Producer,
	/// The source's id in the table.
	id: u64,
}

/// Owns one source's lifecycle after its synchronous attach: waits for the
/// source to end (finish, abort, or drop), then detaches it. Queued on the
/// origin's [`Driver`] by [`Producer::create_broadcast`].
async fn run_source(task: SourceTask) {
	let SourceTask {
		source,
		timers,
		leaf,
		state,
		broadcast,
		id,
	} = task;
	let _ = timers;

	kio::wait(|waiter| source.poll_closed(waiter).map(Ok::<(), Error>))
		.await
		.ok();

	// The source ended, deliberately or not: detach it. If it was the last one
	// the front closes with it.
	detach_source(&state, &broadcast, &leaf, id);
}

/// Everything about a source's attach that does not change between attempts.
struct AttachContext<'a> {
	origin: &'a Info,
	node: &'a Lock<OriginNode>,
	/// Absolute path, for the front's identity and log lines.
	full: &'a PathOwned,
	/// Path relative to `node`, for locating (and later pruning) the leaf.
	rest: &'a PathOwned,
	/// Driver submission handle, for queueing a fresh front's task.
	tasks: &'a Tasks,
	/// The driver's clock, threaded into fronts for the track idle linger.
	timers: &'a TimersSlot,
}

/// Attach a source to the broadcast at `leaf`, creating (and publishing) the
/// broadcast if none is live. One lock acquisition covers the whole
/// join-or-create decision, so concurrent attaches cannot race each other.
///
/// A later source joins the live front and immediately becomes the active one
/// (newest wins), so a publisher re-creating a path over a fresh handle takes
/// over without waiting for the old handle to be torn down; tracks re-splice at
/// the first missing group. A front whose sources have all closed is replaced by
/// a fresh broadcast instead, so new content is never spliced into subscribers
/// of a broadcast that is over.
fn attach_source(
	ctx: &AttachContext,
	leaf: &Lock<OriginNode>,
	source: &broadcast::Consumer,
) -> (kio::Producer<FrontState>, broadcast::Producer, u64) {
	let mut leaf_guard = leaf.lock();

	// Join the live broadcast if the leaf already has one. A closed one (torn
	// down, awaiting teardown, or evicted just below) is replaced instead.
	if let Some(existing) = &leaf_guard.broadcast {
		let mut joined = None;
		if let Ok(mut s) = existing.state.write()
			&& !s.closed
		{
			if !s.sources.is_empty() && s.sources.iter().all(|entry| entry.source.is_closing()) {
				// Every attached source has already closed; only the driver's
				// detach sweep is outstanding. Splicing requires overlapping
				// *live* sources, so joining now would splice new content into
				// subscribers of a broadcast that is over. Close the front and
				// create a fresh one below; its own task finishes the teardown,
				// finding the leaf slot already taken.
				s.closed = true;
			} else {
				let id = s.next_source;
				s.next_source += 1;
				s.sources.push(FrontSource {
					id,
					source: source.clone(),
				});
				s.reselect();
				joined = Some(id);
			}
		}
		if let Some(id) = joined {
			let state = existing.state.clone();
			let broadcast = existing.broadcast.clone();
			return (state, broadcast, id);
		}
	}

	// First source: create the broadcast and publish it into the tree.
	let broadcast = broadcast::Producer::new_spliced(broadcast::Info {
		origin: ctx.origin.clone(),
		path: ctx.full.clone(),
	});
	let state = kio::Producer::new(FrontState {
		next_source: 1,
		sources: vec![FrontSource {
			id: 0,
			source: source.clone(),
		}],
		active: Some(0),
		closed: false,
	});

	// A stale (closed) entry is replaced; its own teardown task then finds the
	// slot already taken and leaves it alone.
	leaf_guard.broadcast = Some(OriginBroadcast {
		broadcast: broadcast.clone(),
		state: state.clone(),
	});
	drop(leaf_guard);

	ctx.tasks.push(run_front(
		state.clone(),
		broadcast.clone(),
		ctx.node.clone(),
		ctx.rest.clone(),
		ctx.tasks.clone(),
		ctx.timers.clone(),
	));

	(state, broadcast, 0)
}

/// Owns a front's lifecycle: dispatches each requested track to a serve task
/// until the last source detaches, then unpublishes the broadcast.
async fn run_front(
	state: kio::Producer<FrontState>,
	mut broadcast: broadcast::Producer,
	node: Lock<OriginNode>,
	rest: PathOwned,
	tasks: Tasks,
	slot: TimersSlot,
) {
	enum Step {
		Serve(Arc<str>, super::resume::Producer),
		Closed,
	}

	loop {
		let step = {
			kio::wait(|waiter| {
				if let Poll::Ready((name, resume)) = broadcast.poll_spliced_assigned(waiter) {
					return Poll::Ready(Step::Serve(name, resume));
				}
				// The close is set synchronously by the detach that empties the
				// table; this task only finishes the teardown. `Err` is the
				// channel itself dying, which also ends the front.
				match state.poll_ref(waiter, |s| match s.closed {
					true => Poll::Ready(()),
					false => Poll::Pending,
				}) {
					Poll::Ready(_) => Poll::Ready(Step::Closed),
					Poll::Pending => Poll::Pending,
				}
			})
			.await
		};

		match step {
			Step::Serve(name, resume) => {
				// Serve tasks self-terminate when the track completes or the
				// front closes.
				tasks.push(serve_track(state.clone(), name, resume, slot.clone()));
			}
			Step::Closed => break,
		}
	}

	// Abort the logical tracks (releasing their subscribers) and unpublish.
	broadcast.abort_spliced(Error::Dropped);

	// Deliberate end; suppresses the dropped-without-finish warning.
	broadcast.finish();

	// Remove the broadcast from the tree (identity-checked, so a replacement is
	// untouched) and prune empty nodes.
	node.lock().remove(&state, &rest);
}

/// Serves one spliced logical track: splices in the best source's copy of the
/// track, re-splicing on handover or failure, until the track completes or the
/// front closes. A refusal (a source rejecting the track, or its copy dying
/// before delivering anything) is authoritative and never retried: the refuser
/// is skipped for this track so a joining standby cannot kill a subscription
/// the incumbent is serving, and once every attached source has refused, the
/// track aborts with the last refusal's error. The verdict belongs to this
/// request; a later consumer request asks afresh (see `track_inner`). Failures
/// after delivered progress (a serving session dying mid-stream) are normal
/// failover and re-splice from the next source at the first missing group; a
/// source that fails while *closing* is a corpse to fail over past (its watcher
/// is about to detach it), not a verdict on the track.
async fn serve_track(
	state: kio::Producer<FrontState>,
	name: Arc<str>,
	mut resume: super::resume::Producer,
	slot: TimersSlot,
) {
	enum Step {
		Closed,
		Splice(u64, broadcast::Consumer),
		Complete,
		Failed(Error),
		/// The route we were serving from left the table with nothing servable to
		/// replace it: drop our handle; the verdict block at the top of the loop
		/// decides between aborting (all refused) and parking (corpses detaching).
		NoRoute,
		/// The linger expired with the track still unread: release the segment.
		Idle,
		/// A reader arrived or the last one left: recompute the demand gate.
		Demand,
	}

	// The source whose copy is currently spliced in, and that copy.
	let mut serving: Option<(u64, track::Consumer)> = None;
	// The delivered edge when that copy spliced in. A copy that dies without
	// advancing it never delivered anything, which is what [`Step::Failed`]
	// uses to tell a refusal from a mid-stream failover. Snapshotted per splice,
	// not per wake: an unrelated wake between the copy's last frame and its
	// death must not launder its delivered progress away.
	let mut spliced_edge: Option<track::Position> = None;
	// Sources that refused this track, and the most recent refusal's error. A
	// standby joining a live front wins dispatch the moment it attaches, which is
	// before a real publisher has created every track, so its refusal must cost
	// the incumbent nothing: we keep serving from a route that has the track.
	let mut refused: HashSet<u64> = HashSet::new();
	let mut refusal: Option<Error> = None;
	// Sources whose splice failed because they had already closed. Their watchers
	// are about to detach them (closing the front if nothing else remains), so
	// wait for the table to move on rather than treating a corpse's error as a
	// refusal (ids are never reused, so this cannot wedge).
	let mut dead: HashSet<u64> = HashSet::new();
	// When the spliced segment stopped being read, starting the release countdown.
	let mut idle_since: Option<Instant> = None;
	// Only the driver polls this body, and `Driver::run` installs the slot
	// before the driver can poll anything (see `run_front`).
	let timers = slot.get();
	let mut deadline = crate::runtime::Deadline::new(&timers);

	loop {
		let serving_id = serving.as_ref().map(|(id, _)| *id);

		// The table's verdict: once every attached source has refused, nothing
		// will ever serve the track (refusals are never retried) and it aborts
		// with the last refusal's error. A detached refuser leaves the set, so a
		// source that reattaches (under a fresh id) is asked anew; a table blocked
		// only by corpses awaiting detach parks instead, since their replacement
		// (a reconnect) deserves the seamless splice.
		{
			let s = state.read();
			refused.retain(|id| s.sources.iter().any(|r| r.id == *id));
			dead.retain(|id| s.sources.iter().any(|r| r.id == *id));
			let exhausted = !s.sources.is_empty()
				&& s.serve_route(|id| refused.contains(&id) || dead.contains(&id))
					.is_none();
			if exhausted && dead.is_empty() {
				drop(s);
				let err = refusal.take().unwrap_or(Error::NotFound);
				tracing::debug!(name = %name, %err, "every source refused track; aborting");
				let _ = resume.abort(err);
				return;
			}
		}

		// Demand gates both directions: an unread track never splices a source in,
		// and a spliced one is released once the idle window expires. Both sides use
		// the same signal, so a release can't immediately re-splice and spin.
		//
		// The countdown keys off the segment, not our handle on the route that
		// produced it: a route that leaves (or a copy that dies) drops the handle
		// while the segment stays spliced, and that segment is exactly what the
		// release exists to reclaim. Keying off the handle strands it until the front
		// closes, pinning the departed source's cached groups and leaving a dead
		// segment's edge behind for the next takeover to splice above.
		let used = resume.is_used();
		idle_since = match (resume.is_spliced(), used) {
			(true, false) => idle_since.or_else(|| Some(timers.now())),
			_ => None,
		};
		deadline.set(idle_since.and_then(|at| at.checked_add(TRACK_IDLE_LINGER)));

		let step = {
			let skip = |id: u64| refused.contains(&id) || dead.contains(&id);
			kio::wait(|waiter| {
				// Watch the source table: the front closing, a better servable
				// source than the one spliced in (skipping any we already know
				// can't serve this track), or the served route leaving the table,
				// which retires the refusals collected against it. Splicing waits
				// for a reader.
				match state.poll(waiter, |s| {
					let gone = serving_id.is_some_and(|id| !s.sources.iter().any(|r| r.id == id));
					if s.closed
						|| (used && (gone || matches!(s.serve_route(skip), Some(next) if Some(next) != serving_id)))
					{
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				}) {
					Poll::Ready(Ok(guard)) => {
						if guard.closed {
							return Poll::Ready(Step::Closed);
						}
						let Some(next) = guard.serve_route(skip) else {
							return Poll::Ready(Step::NoRoute);
						};
						let source = guard
							.sources
							.iter()
							.find(|r| r.id == next)
							.expect("servable source in table")
							.source
							.clone();
						return Poll::Ready(Step::Splice(next, source));
					}
					Poll::Ready(Err(_)) => return Poll::Ready(Step::Closed),
					Poll::Pending => {}
				}

				// Watch the demand edge in whichever direction is unmet. This has to end
				// the wait, not just wake it: `used` and the countdown are computed by
				// the outer loop, so a wake that stayed inside would re-poll with the
				// stale value and never arm (or cancel) the linger.
				let edge = match used {
					true => resume.poll_unused(waiter),
					false => resume.poll_used(waiter),
				};
				if edge.is_ready() {
					return Poll::Ready(Step::Demand);
				}

				// Watch the spliced copy for its end: complete means the logical
				// track is over; anything else means the serving copy died.
				if let Some((_, track)) = &serving
					&& let Poll::Ready(result) = track.poll_complete(waiter)
				{
					return Poll::Ready(match result {
						Ok(()) => Step::Complete,
						Err(err) => Step::Failed(err),
					});
				}

				deadline.poll(waiter).map(|_| Step::Idle)
			})
			.await
		};

		match step {
			// The front's teardown aborts the logical track.
			Step::Closed => return,
			Step::Complete => {
				let _ = resume.finish();
				return;
			}
			Step::Failed(err) => {
				// The spliced copy died mid-serve. With delivered progress since
				// its splice it's a normal failover: re-splice from the (possibly
				// same) active source. A copy that died before producing anything
				// is a refusal (a source whose track keeps dying right after
				// acceptance must not re-splice forever), unless the source
				// itself is closing: that corpse parks for its detach so a
				// reconnect gets the seamless splice.
				if resume.resume_position() == spliced_edge
					&& let Some(id) = serving_id
				{
					let closing = state
						.read()
						.sources
						.iter()
						.find(|r| r.id == id)
						.is_some_and(|r| r.source.is_closing());
					if closing {
						dead.insert(id);
					} else {
						refused.insert(id);
						refusal = Some(err);
					}
				}
				serving = None;
			}
			// The outer loop recomputes `used` and the countdown on the next pass.
			Step::Demand => {}
			// Forget which route we were serving from, or the `gone` edge that woke
			// us keeps firing: the id stays absent from the table, the wait returns
			// Ready at once, and the loop spins on a full core without ever parking.
			// The segment itself stays spliced into `resume` (readers keep whatever
			// it delivered) until a replacement is proven servable.
			Step::NoRoute => serving = None,
			Step::Idle => {
				// Nobody has read the track for the linger: drop the source's copy so
				// its session can release the track (and the cached `track::Info` that
				// came with it). The logical track stays alive and re-splices on the
				// next reader, so a returning viewer or a follow-up fetch resumes.
				if resume.release().is_err() {
					// Finished or aborted meanwhile; the track is over either way.
					return;
				}
				serving = None;
			}
			Step::Splice(id, source) => {
				// Ask the source for its copy and wait for the info to resolve,
				// proving it servable, before splicing it in. Bail out early if
				// the table moves on while waiting.
				let attempt = match source.track(&name) {
					Ok(track) => {
						// `into_inner` sheds the `Pending` future wrapper so only
						// the pollable (which is `Sync`) is held across the await.
						let query = track.info().into_inner();
						let skip = |id: u64| refused.contains(&id) || dead.contains(&id);
						let info = kio::wait(|waiter| {
							if let Poll::Ready(result) = query.poll(waiter) {
								return Poll::Ready(Some(result));
							}
							match state.poll(waiter, |s| {
								if s.closed || s.serve_route(skip) != Some(id) {
									Poll::Ready(())
								} else {
									Poll::Pending
								}
							}) {
								Poll::Ready(_) => Poll::Ready(None),
								Poll::Pending => Poll::Pending,
							}
						})
						.await;
						match info {
							// The table changed under us; re-pick from the top.
							None => continue,
							// A copy that is already aborted can't be spliced;
							// its error is the source's answer for the track.
							Some(Ok(_)) => match track.poll_complete(&kio::Waiter::noop()) {
								Poll::Ready(Err(err)) => Err(err),
								_ => Ok(track),
							},
							Some(Err(err)) => Err(err),
						}
					}
					Err(err) => Err(err),
				};

				match attempt {
					Ok(track) => {
						if let Err(err) = resume.takeover(&track) {
							// Closed means the logical track already ended
							// (finished or aborted). Anything else is a boundary
							// bug; abort rather than strand subscribers on a
							// track no task serves (a no-op after a clean end).
							let _ = resume.abort(err);
							return;
						}
						// `dead` must survive the takeover. A dead source can never
						// serve again (ids are never reused; `is_closing` is
						// terminal), and the retain above reclaims its entry once
						// its watcher detaches it. Re-admitting a still-attached
						// closing route here would let `serve_route`'s active
						// preference re-dispatch it, and because both its instant
						// failure and a cached standby splice resolve without
						// awaiting, the loop would spin inside a single poll,
						// starving the watcher whose detach ends the cycle.
						// The new segment has produced nothing yet, so this is
						// the edge the copy is asked to advance.
						spliced_edge = resume.resume_position();
						serving = Some((id, track));
					}
					// The source itself closed or deliberately ended: not a
					// verdict on the track. Park until its watcher detaches
					// it and the table promotes a replacement.
					Err(_) if source.is_closing() => {
						dead.insert(id);
						serving = None;
					}
					// The dispatched source does not carry the track: a publisher
					// announces a broadcast only once its tracks exist, so the
					// answer is authoritative and never re-asked. Skip the source
					// for this track; the verdict block aborts once every source
					// has refused.
					Err(err) => {
						tracing::debug!(name = %name, source = id, %err, "source refused track");
						refused.insert(id);
						refusal = Some(err);
						serving = None;
					}
				}
			}
		}
	}
}

/// The content identity of a remotely-served front: who originated the route
/// its first source arrived through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Identity {
	/// No source has attached yet: the first request may resolve through any
	/// covering route, and whoever serves it fixes the identity.
	Undetermined,
	/// The serving route's first hop was absent or [`Hop::UNKNOWN`], which
	/// identifies nobody and never matches itself: the front cannot resume, so
	/// its source ending ends it. Two anonymous publishers must never pass for
	/// one reconnecting.
	Anonymous,
	/// The first hop of the serving route: the endpoint that originated it.
	/// Routes sharing it are the same origin reached another way and safe to
	/// resume through; anything else is different content at the same path.
	Publisher(Hop),
}

impl Identity {
	/// The first-hop pin for [`OriginState::best_route`], or `None` when any
	/// route qualifies (no source attached yet).
	fn pin(&self) -> Option<Hop> {
		match self {
			Self::Publisher(hop) => Some(*hop),
			_ => None,
		}
	}

	/// Whether route selection applies at all: an anonymous front never adopts
	/// another route.
	fn routable(&self) -> bool {
		!matches!(self, Self::Anonymous)
	}
}

/// Everything [`run_remote_front`] owns, queued by [`Consumer::request_broadcast`].
struct RemoteFrontTask {
	/// The route table the front selects and re-selects from.
	shared: kio::Shared<OriginState>,
	/// The front's source table, shared with its [`serve_track`] tasks.
	state: kio::Producer<FrontState>,
	/// The spliced broadcast the front serves.
	broadcast: broadcast::Producer,
	/// Absolute path of the front.
	path: PathOwned,
	/// The requesters' split-horizon exclusion, applied to every (re)selection.
	exclude: Option<Hop>,
	/// Resolves the requesters parked on the front's channel.
	request: kio::Producer<PendingBroadcast>,
	tasks: TasksWeak,
	timers: TimersSlot,
}

/// Owns a remotely-served front: materializes the path from the best covering
/// route, dispatches its spliced tracks, and re-splices through routes sharing
/// the front's first-hop identity when the serving source dies or a better
/// qualifying route appears, so subscribers never observe a survivable route
/// change. The front ends (aborting its subscribers) when the source dies with
/// no qualifying route left, when a qualifying route refuses the path, or when
/// the origin tears down.
async fn run_remote_front(task: RemoteFrontTask) {
	let RemoteFrontTask {
		shared,
		state,
		broadcast,
		path,
		exclude,
		request,
		tasks,
		timers,
	} = task;

	enum Step {
		/// A spliced track needs a serve task.
		Serve(Arc<str>, super::resume::Producer),
		/// The in-flight upstream request resolved.
		Resolved(Result<broadcast::Consumer, Error>),
		/// The attached source closed.
		SourceDead,
		/// The route table changed in a way the decision below cares about.
		Table,
		/// The origin tore down.
		Closed,
	}

	let mut front = FrontDriver {
		state,
		broadcast,
		initial: Some(request),
		identity: Identity::Undetermined,
		serving: None,
	};
	// The in-flight upstream request: the targeted route entry, its first hop,
	// and the pending channel.
	let mut upstream: Option<(u64, Option<Hop>, kio::Consumer<PendingBroadcast>)> = None;
	// Routes that refused the path (an authoritative reject while another source
	// was serving, or a queue with no live handler). Never retried while the
	// entry stands; a reconnect is a fresh entry.
	let mut refused: HashSet<u64> = HashSet::new();
	// Why the last candidate fell through, reported if the front dies unresolved.
	let mut last_err: Option<Error> = None;

	'run: loop {
		// Decide what the table means for us, then fire at most one upstream
		// request. Locks are sequential, never nested: the table's, then the
		// chosen route's queue.
		// What the wait below compares table changes against: the qualifying
		// route this pass decided on, recomputed identically by the wait's
		// predicate so only a table change that would alter the decision wakes it.
		let decided;
		let fire = {
			let table = shared.read();
			if table.closed {
				break 'run;
			}
			refused.retain(|id| table.routes.iter().any(|entry| entry.id == *id));
			let best = match front.identity.routable() {
				true => table.best_route(&path.as_path(), exclude, front.identity.pin(), &refused),
				false => None,
			};
			decided = best.map(|entry| entry.id);
			let serving_route = front.serving.as_ref().map(|(_, route, _)| *route);
			match best {
				// The serving route is still the best qualifying one.
				Some(entry) if Some(entry.id) == serving_route => {
					upstream = None;
					None
				}
				// A better (or replacement) qualifying route: request through it,
				// unless we already are.
				Some(entry) => match upstream.as_ref().is_some_and(|(route, ..)| *route == entry.id) {
					true => None,
					false => Some((
						entry.id,
						entry.hops.iter().next().copied(),
						entry.server.clone().expect("best_route yields served entries"),
					)),
				},
				// Nothing qualifies. A live source keeps serving (its route may
				// return); without one the front is over.
				None => {
					upstream = None;
					match &front.serving {
						Some(_) => None,
						None => {
							let err = match front.identity {
								Identity::Undetermined => last_err.take().unwrap_or(Error::Unroutable),
								_ => last_err.take().unwrap_or(Error::Dropped),
							};
							front.end(err);
							return;
						}
					}
				}
			}
		};

		if let Some((route, first, server)) = fire {
			let mut serve = server.lock();
			match serve.closed {
				// The server is gone: retracted under us, or its handler dropped
				// while the announcement stands. Either way the route cannot
				// serve, so skip it rather than re-picking it forever.
				true => {
					refused.insert(route);
					last_err = Some(Error::Unroutable);
					continue 'run;
				}
				false => {
					// A source this route already materialized for the path
					// attaches without another upstream round trip.
					if let Some(weak) = serve.served.get(&path) {
						drop(serve);
						front.attach(weak.consume(), route, first);
						continue 'run;
					}
					let pending = match serve.requests.join(&path) {
						Some(producer) => producer.consume(),
						None => {
							let producer = kio::Producer::<PendingBroadcast>::default();
							let consumer = producer.consume();
							match serve.requests.insert(path.clone(), producer) {
								Ok(()) => consumer,
								// No live handler behind the route: it cannot
								// serve, whatever the table says.
								Err(_) => {
									refused.insert(route);
									last_err = Some(Error::Unroutable);
									continue 'run;
								}
							}
						}
					};
					upstream = Some((route, first, pending));
				}
			}
		}

		let pin = front.identity.pin();
		let routable = front.identity.routable();

		let step = kio::wait(|waiter| {
			if let Poll::Ready((name, resume)) = front.broadcast.poll_spliced_assigned(waiter) {
				return Poll::Ready(Step::Serve(name, resume));
			}

			if let Some((.., pending)) = &upstream
				&& let Poll::Ready(result) = pending.poll(waiter, |p| match &p.resolved {
					Some(result) => Poll::Ready(result.clone()),
					None => Poll::Pending,
				}) {
				return Poll::Ready(Step::Resolved(match result {
					Ok(resolved) => resolved,
					// The queue died unresolved (its handler dropped): the route
					// could not serve.
					Err(_closed) => Err(Error::Unroutable),
				}));
			}

			if let Some((.., source)) = &front.serving
				&& source.poll_closed(waiter).is_ready()
			{
				return Poll::Ready(Step::SourceDead);
			}

			match shared.poll(waiter, |table| {
				if table.closed {
					return Poll::Ready(());
				}
				let best = match routable {
					true => table.best_route(&path.as_path(), exclude, pin, &refused).map(|e| e.id),
					false => None,
				};
				match best == decided {
					true => Poll::Pending,
					false => Poll::Ready(()),
				}
			}) {
				Poll::Ready(table) => {
					let closed = table.closed;
					drop(table);
					Poll::Ready(match closed {
						true => Step::Closed,
						false => Step::Table,
					})
				}
				Poll::Pending => Poll::Pending,
			}
		})
		.await;

		match step {
			Step::Serve(name, resume) => {
				tasks.push(serve_track(front.state.clone(), name, resume, timers.clone()));
			}
			Step::Resolved(result) => {
				let (route, first, _) = upstream.take().expect("resolved an in-flight request");
				match result {
					Ok(source) => front.attach(source, route, first),
					// The route retracted before serving: the table already
					// reflects it, so the next pass retries the survivor. Each
					// such retry consumed a real retraction, so this cannot spin.
					Err(Error::Unroutable) => {
						last_err = Some(Error::Unroutable);
					}
					// An authoritative refusal of the path. It ends a front with
					// no other source (a refusal is never retried); a serving
					// front merely skips the refuser.
					Err(err) => match &front.serving {
						Some(_) => {
							refused.insert(route);
							last_err = Some(err);
						}
						None => {
							front.end(err);
							return;
						}
					},
				}
			}
			Step::SourceDead => {
				front.detach_dead();
				last_err = Some(Error::Dropped);
			}
			Step::Table => {}
			Step::Closed => break 'run,
		}
	}

	// The origin tore down; its teardown already rejected parked requesters.
	front.end(Error::Dropped);
}

/// The mutable half of a remote front's watcher: the source table and spliced
/// broadcast it manages, who is serving, and the requesters awaiting the first
/// source.
struct FrontDriver {
	/// The front's source table, shared with its [`serve_track`] tasks.
	state: kio::Producer<FrontState>,
	/// The spliced broadcast the front serves.
	broadcast: broadcast::Producer,
	/// Resolves the requesters parked on the front once the first source
	/// attaches (or the front dies first).
	initial: Option<kio::Producer<PendingBroadcast>>,
	/// The front's content identity, fixed by the first source to attach.
	identity: Identity,
	/// The attached source: its id in the front's table, the route entry that
	/// served it, and the source itself.
	serving: Option<(u64, u64, broadcast::Consumer)>,
}

impl FrontDriver {
	/// Attach a materialized source: replace the previous one (its tracks
	/// re-splice at a group boundary), fix the front's identity on the first
	/// attach, and resolve the requesters parked on the front's channel.
	fn attach(&mut self, source: broadcast::Consumer, route: u64, first: Option<Hop>) {
		let Ok(mut front) = self.state.write() else { return };
		if let Some((id, ..)) = self.serving.take() {
			front.sources.retain(|entry| entry.id != id);
		}
		let id = front.next_source;
		front.next_source += 1;
		front.sources.push(FrontSource {
			id,
			source: source.clone(),
		});
		front.reselect();
		drop(front);

		self.serving = Some((id, route, source));
		if self.identity == Identity::Undetermined {
			self.identity = match first {
				Some(hop) if hop != Hop::UNKNOWN => Identity::Publisher(hop),
				_ => Identity::Anonymous,
			};
		}
		if let Some(request) = self.initial.take()
			&& let Ok(mut pending) = request.write()
		{
			pending.resolved.get_or_insert(Ok(self.broadcast.consume()));
		}
	}

	/// Remove the dead source from the table. Its spliced tracks park on the
	/// empty table until a replacement attaches or the front ends.
	fn detach_dead(&mut self) {
		if let Some((id, ..)) = self.serving.take()
			&& let Ok(mut front) = self.state.write()
		{
			front.sources.retain(|entry| entry.id != id);
			front.reselect();
		}
	}

	/// End the front: reject requesters still parked on its channel, close the
	/// source table so its serve tasks exit, and abort the spliced broadcast so
	/// its subscribers observe the end.
	fn end(&mut self, err: Error) {
		if let Some(request) = self.initial.take()
			&& let Ok(mut pending) = request.write()
		{
			pending.resolved.get_or_insert(Err(err.clone()));
		}
		if let Ok(mut front) = self.state.write() {
			front.closed = true;
		}
		self.broadcast.abort_spliced(err);
		self.broadcast.finish();
	}
}

/// The origin's shared state: the route table, the announce cursors observing
/// it, and the fallback request queue.
///
/// Carried in a [`kio::Shared`], so producers, consumers, and handlers work
/// under one lock. Local broadcasts live in the tree ([`OriginNode`]) instead;
/// this holds everything advertised or served on demand.
#[derive(Default)]
struct OriginState {
	// The announced routes, in announcement order. Scans are linear: the table
	// holds one entry per live advertisement, not one per broadcast consumer.
	routes: Vec<RouteEntry>,
	next_route: u64,

	// The registered announce cursors, each with its own coalescing buffer.
	cursors: HashMap<ConsumerId, TableCursor>,

	// Fallback request queue: `request_broadcast` calls that no local broadcast
	// or served route resolves, drained by `Dynamic` handlers.
	fallback: Requests<PathOwned, kio::Producer<PendingBroadcast>>,

	// Broadcasts a fallback handler has already served, kept weakly so a repeat
	// request for the same path resolves to a shared clone instead of re-invoking
	// the handler. Weak so a served broadcast still closes once its real
	// consumers drop.
	served: WeakCache<PathOwned, broadcast::WeakConsumer>,

	// The remotely-served fronts, keyed by absolute path and the requester's
	// split-horizon exclusion. Each is a spliced broadcast whose watcher task
	// materializes it from the best covering route and re-splices it through
	// routes sharing its first hop, so a route change the identity survives is
	// invisible to subscribers. Keyed per exclusion so a front's failover can
	// never adopt a route flowing back through one of its own readers. Weak, so
	// a front dies with its watcher and a later request re-creates it.
	fronts: WeakCache<FrontKey, RemoteFront>,

	// Set when the origin's driver dropped: new requests fail with `Closed`
	// immediately and handlers observe the end instead of parking forever.
	closed: bool,
}

impl OriginState {
	/// Re-deliver the best route at every presented prefix `prefix` maps to, on
	/// every cursor. Called after an entry covering `prefix` was added, updated,
	/// or removed.
	fn sync_route(&mut self, prefix: &Path) {
		// Split borrows: the recompute reads `routes` while mutating a cursor.
		let routes = &self.routes;
		for cursor in self.cursors.values_mut() {
			for presented in cursor.presented(prefix) {
				Self::sync_cursor(routes, cursor, &presented);
			}
		}
	}

	/// Recompute the best visible route presenting at `presented` (absolute) for
	/// one cursor and deliver the change, if any.
	fn sync_cursor(routes: &[RouteEntry], cursor: &mut TableCursor, presented: &PathOwned) {
		// Mirror `best_server`: among entries presenting here, the most specific
		// prefix wins outright, so the metadata a cursor advertises matches what
		// a request through it actually resolves. Entries at shorter prefixes
		// only present here when clamped to the cursor's scope.
		let candidates: Vec<&RouteEntry> = routes
			.iter()
			.filter(|entry| cursor.visible(entry))
			.filter(|entry| cursor.presented(&entry.prefix.as_path()).contains(presented))
			.collect();
		let most = candidates.iter().map(|entry| entry.prefix.len()).max();
		let best = most.and_then(|most| {
			candidates
				.into_iter()
				.filter(|entry| entry.prefix.len() == most)
				.min_by_key(|entry| route_order(&presented.as_path(), entry))
		});

		let relative = presented
			.strip_prefix(&cursor.root)
			.expect("presented prefix outside the cursor root")
			.to_owned();

		match best {
			Some(entry) => {
				let meta = (entry.hops.clone(), entry.cost);
				let served = entry.server.is_some();
				match cursor
					.current
					.insert(presented.clone(), (entry.id, meta.clone(), served))
				{
					// Unchanged metadata and servability: nothing the consumer could
					// act on, even if the winning entry itself changed (a reconnect
					// under an identical route is invisible, which is the point). A
					// servability flip is delivered: a request that failed Unroutable
					// under an advertise-only route retries on the update, and hiding
					// it would park that waiter forever.
					Some((_, prev, prev_served)) if prev == meta && prev_served == served => {}
					_ => {
						if let Ok(mut state) = cursor.state.write() {
							state.apply_announce(relative, meta);
						}
					}
				}
			}
			None => {
				if let Some((_, last, _)) = cursor.current.remove(presented)
					&& let Ok(mut state) = cursor.state.write()
				{
					state.apply_unannounce(relative, last);
				}
			}
		}
	}

	/// Register a cursor and replay the current best route per presented prefix.
	fn register_cursor(&mut self, id: ConsumerId, mut cursor: TableCursor) {
		let routes = &self.routes;
		let mut presented: Vec<PathOwned> = Vec::new();
		for entry in routes {
			for p in cursor.presented(&entry.prefix.as_path()) {
				if !presented.contains(&p) {
					presented.push(p);
				}
			}
		}
		for p in &presented {
			Self::sync_cursor(routes, &mut cursor, p);
		}
		self.cursors.insert(id, cursor);
	}

	/// The best served route covering `path` (absolute) for a requester excluding
	/// `exclude`, skipping the `refused` entry ids.
	///
	/// The most specific covering prefix wins outright, so a narrow advertise-only
	/// announcement shadows a broad served one: requests under it fall through to
	/// the fallback handler instead of being routed around it. Among routes at the
	/// winning prefix, the cheapest served one is picked by [`route_order`].
	///
	/// With `publisher` set, only routes originated by that first hop are
	/// candidates: this is the identity a front resumes through, and a route from
	/// anyone else is different content rather than an alternate path (see
	/// [`run_remote_front`]). `Hop::UNKNOWN` identifies nobody, so callers never
	/// pin it.
	fn best_route(
		&self,
		path: &Path,
		exclude: Option<Hop>,
		publisher: Option<Hop>,
		refused: &HashSet<u64>,
	) -> Option<&RouteEntry> {
		let candidates: Vec<&RouteEntry> = self
			.routes
			.iter()
			.filter(|entry| path.has_prefix(&entry.prefix))
			.filter(|entry| match exclude {
				Some(peer) if peer != Hop::UNKNOWN => !entry.hops.contains(&peer),
				_ => true,
			})
			.filter(|entry| match publisher {
				Some(first) => entry.hops.iter().next() == Some(&first),
				None => true,
			})
			.filter(|entry| !refused.contains(&entry.id))
			.collect();

		// Covering prefixes of one path form a chain, so the longest is unique.
		let most = candidates.iter().map(|entry| entry.prefix.len()).max()?;
		candidates
			.into_iter()
			.filter(|entry| entry.prefix.len() == most && entry.server.is_some())
			.min_by_key(|entry| route_order(path, entry))
	}
}

/// One-shot result of a dynamic broadcast request.
///
/// Stays `None` until a handler [`accept`](Request::accept)s (yielding the served
/// broadcast) or [`reject`](Request::reject)s (yielding an error). The producer is
/// dropped right after writing, closing the channel; kio checks the value before the closed
/// flag, so an awaiting requester still observes the final result.
#[derive(Default)]
struct PendingBroadcast {
	resolved: Option<Result<broadcast::Consumer, Error>>,
}

/// Picks up [`Consumer::request_broadcast`] calls for paths that are not announced.
///
/// The origin-level analogue of [`broadcast::Dynamic`]: where that serves tracks on
/// demand within a broadcast, this serves whole broadcasts on demand within an origin. A
/// relay uses it as a fallback router, fetching a broadcast from upstream only when a
/// downstream consumer asks for an exact path that nobody announced.
///
/// Served broadcasts are deliberately *not* announced, so they never appear in
/// [`Consumer::announced`]. Drop this handle (and every clone) to reject the
/// requests still waiting to be served.
pub struct Dynamic {
	hop: Hop,
	root: PathOwned,
	state: kio::Shared<OriginState>,
}

impl Clone for Dynamic {
	fn clone(&self) -> Self {
		// Mirror `new`: count each live handle. Without this, dropping a clone would
		// decrement past `new`'s increment and prematurely flip the handler count to
		// zero, making future `request_broadcast` calls return `Unroutable`.
		self.state.lock().fallback.add_handler();

		Self {
			hop: self.hop,
			root: self.root.clone(),
			state: self.state.clone(),
		}
	}
}

impl Dynamic {
	fn new(hop: Hop, root: PathOwned, state: kio::Shared<OriginState>) -> Self {
		state.lock().fallback.add_handler();

		Self { hop, root, state }
	}

	/// The id of the origin this handler belongs to.
	pub fn hop(&self) -> Hop {
		self.hop
	}

	/// Poll for the next requested broadcast, without blocking.
	///
	/// Returns [`Error::Closed`] once the origin's [`Driver`] has been dropped:
	/// no request will ever arrive again, so handler loops should end.
	pub fn poll_requested_broadcast(&mut self, waiter: &kio::Waiter) -> Poll<Result<Request, Error>> {
		let mut state = ready!(self.state.poll(waiter, |state| {
			if state.closed || state.fallback.has_queued() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));

		// The teardown already drained the queue, so there is nothing left to pop.
		if state.closed {
			return Poll::Ready(Err(Error::Closed));
		}

		let path = state.fallback.pop().expect("predicate guaranteed a request");
		// The popped request stays pending, so a repeat request in the window between
		// hand-off and accept coalesces onto it instead of re-invoking the handler. The
		// producer is a shared clone; `Request::{accept, reject, drop}` removes the
		// entry. This mirrors how `poll_requested_track` keeps a served track
		// discoverable via the weak cache across the same window.
		let producer = state.fallback.get(&path).expect("popped key must be pending").clone();
		Poll::Ready(Ok(Request {
			path,
			producer,
			home: RequestHome::Fallback(self.state.clone()),
		}))
	}

	/// Block until a consumer requests an unannounced broadcast, returning a
	/// [`Request`] to serve.
	pub async fn requested_broadcast(&mut self) -> Result<Request, Error> {
		kio::wait(|waiter| self.poll_requested_broadcast(waiter)).await
	}

	/// Returns the prefix that is automatically stripped from requested paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}
}

impl Drop for Dynamic {
	fn drop(&mut self) {
		// Decrement and reject under one lock, so a `request_broadcast` that saw a
		// live handler through the same lock can't slip a request past the rejection.
		let mut state = self.state.lock();
		if state.fallback.remove_handler() {
			// No handlers left to pop queued requests; drop them, closing their result
			// channels so awaiting requesters resolve to `Unroutable`. A request already
			// handed to a handler stays, resolved by its `Request` instead.
			state.fallback.drain_queued();
		}
	}
}

/// Where a [`Request`] came from: the origin's fallback queue, or one served
/// route's queue. Both hold the same request bookkeeping under their own lock.
enum RequestHome {
	Fallback(kio::Shared<OriginState>),
	Route(kio::Shared<ServeState>),
}

impl RequestHome {
	/// Resolve the pending request: cache an accepted broadcast for repeat
	/// requests, remove the queue entry, and wake the requesters.
	///
	/// Resolved while the home's lock is held, so this linearizes with the
	/// teardown: either the teardown ran first (the `closed` check returns, its
	/// rejection stands) or this write lands first and the teardown finds the
	/// entry already gone. The home lock is released before the channel guard
	/// drops, so the requester wakes outside it: an inline executor re-entering
	/// `request_broadcast` from the wake must not find the non-reentrant lock
	/// still held.
	fn resolve(
		&self,
		path: &PathOwned,
		producer: &kio::Producer<PendingBroadcast>,
		result: Result<broadcast::Consumer, Error>,
	) {
		match self {
			Self::Fallback(shared) => {
				let mut state = shared.lock();
				if state.closed {
					return;
				}
				let OriginState { fallback, served, .. } = &mut *state;
				let resolved = Self::settle(fallback, served, path, producer, result);
				if let Ok(mut pending) = producer.write() {
					pending.resolved.get_or_insert(resolved);
					drop(state);
				}
			}
			Self::Route(shared) => {
				let mut state = shared.lock();
				if state.closed {
					return;
				}
				let ServeState { requests, served, .. } = &mut *state;
				let resolved = Self::settle(requests, served, path, producer, result);
				if let Ok(mut pending) = producer.write() {
					pending.resolved.get_or_insert(resolved);
					drop(state);
				}
			}
		}
	}

	/// Move an accepted broadcast into the weak `served` cache (deduping onto a
	/// live entry served concurrently) and remove the queue entry.
	fn settle(
		requests: &mut Requests<PathOwned, kio::Producer<PendingBroadcast>>,
		served: &mut WeakCache<PathOwned, broadcast::WeakConsumer>,
		path: &PathOwned,
		producer: &kio::Producer<PendingBroadcast>,
		result: Result<broadcast::Consumer, Error>,
	) -> Result<broadcast::Consumer, Error> {
		let resolved = match result {
			Ok(broadcast) => {
				// If a live broadcast was already served for this path while we were
				// fetching upstream, dedup onto it and drop ours rather than replace
				// a good entry with a duplicate subscription.
				let existing = served.insert(path.clone(), broadcast.weak());
				Ok(existing.map(|weak| weak.consume()).unwrap_or(broadcast))
			}
			Err(err) => Err(err),
		};
		requests.remove_if(path, |p| p.same_channel(producer));
		resolved
	}

	/// Drop the still-pending entry, if it is still ours.
	fn forget(&self, path: &PathOwned, producer: &kio::Producer<PendingBroadcast>) {
		match self {
			Self::Fallback(shared) => {
				shared.lock().fallback.remove_if(path, |p| p.same_channel(producer));
			}
			Self::Route(shared) => {
				shared.lock().requests.remove_if(path, |p| p.same_channel(producer));
			}
		}
	}
}

/// A pending request for a broadcast to be served on demand.
///
/// Yielded by [`Dynamic::requested_broadcast`] (the origin's fallback) and by a
/// served route's queue. The requester is awaiting inside
/// [`Consumer::request_broadcast`]; [`accept`](Self::accept) resolves it with a live
/// broadcast (which the handler keeps producing into) and [`reject`](Self::reject) resolves
/// it with an error. Dropping the request without either rejects it.
pub struct Request {
	// Absolute path that was requested.
	path: PathOwned,

	// Result channel back to the awaiting requester(s). Writing `resolved` and dropping
	// this wakes them with the outcome.
	producer: kio::Producer<PendingBroadcast>,

	// The queue this request came from, so `accept` can cache the served
	// broadcast for repeat requests.
	home: RequestHome,
}

impl Request {
	/// The absolute path that was requested.
	pub fn path(&self) -> &Path<'_> {
		&self.path
	}

	/// Accept the request, resolving every awaiting requester with `broadcast`.
	///
	/// The caller keeps producing into `broadcast` (e.g. a relay proxying tracks from
	/// upstream); the requesters receive a consumer for it. Repeat requests for the
	/// path share the served broadcast for as long as it stays live.
	pub fn accept(self, broadcast: impl Consume<broadcast::Consumer>) {
		let broadcast = broadcast.consume();
		self.home.resolve(&self.path, &self.producer, Ok(broadcast));
		// `self.producer` drops here, closing the channel; the value is still observable.
	}

	/// Reject the request, resolving every awaiting requester with `err`.
	pub fn reject(self, err: Error) {
		self.home.resolve(&self.path, &self.producer, Err(err));
	}
}

impl Drop for Request {
	fn drop(&mut self) {
		// Handed off but neither accepted nor rejected: drop the still-pending entry so its
		// producer clone (plus this one) closes the channel, resolving coalesced requesters to
		// `Unroutable` rather than hanging.
		//
		// The identity guard matters: `accept`/`reject` already removed our entry and released
		// the lock before we run, so a concurrent request for the same path may have registered
		// a *new* one here. Removing unconditionally would clobber it, stranding its requesters.
		self.home.forget(&self.path, &self.producer);
	}
}

/// The pollable result of [`Consumer::request_broadcast`].
///
/// Awaited via the [`kio::Pending`] wrapper; resolves to the [`broadcast::Consumer`]
/// immediately when the broadcast was already announced, or once an [`Dynamic`]
/// handler serves the request. Resolves to an error if the request is rejected or every
/// handler drops before serving it.
pub struct Requesting {
	inner: RequestState,
	// The path the requester asked for, relative to its cursor's root. Stamped on the
	// resolved broadcast (see [`broadcast::Info::path`]) because a handler is free to
	// serve a broadcast created somewhere else entirely, or at no path at all.
	path: PathOwned,
	// Egress scope applied to the resolved broadcast, so its reads are attributed.
	// Empty (no-op) for an untagged consumer.
	stats: stats::Scope,
}

enum RequestState {
	// Already announced: resolves immediately with a clone of this broadcast.
	Ready(broadcast::Consumer),
	// Unroutable at request time: resolves immediately with this error. Baked in so
	// `request_broadcast` itself stays infallible.
	Failed(Error),
	// Awaiting a handler: resolves when the request's result channel is written.
	Pending(kio::Consumer<PendingBroadcast>),
}

impl Requesting {
	fn ready(broadcast: broadcast::Consumer) -> Self {
		Self::new(RequestState::Ready(broadcast))
	}

	fn failed(error: Error) -> Self {
		Self::new(RequestState::Failed(error))
	}

	fn pending(consumer: kio::Consumer<PendingBroadcast>) -> Self {
		Self::new(RequestState::Pending(consumer))
	}

	/// Whether the request was handed to a serving route or fallback handler,
	/// rather than decided on the spot.
	///
	/// Fixed at request time, so it distinguishes the two ways
	/// [`Error::Unroutable`] arises: a queued request that fails was killed by
	/// its serving route retracting, and the table may already hold a
	/// replacement worth retrying against ([`Consumer::routed_broadcast`] does),
	/// while an unqueued failure means nothing could serve the path at all.
	pub fn is_queued(&self) -> bool {
		matches!(self.inner, RequestState::Pending(_))
	}

	fn new(inner: RequestState) -> Self {
		Self {
			inner,
			path: PathOwned::default(),
			stats: stats::Scope::default(),
		}
	}

	fn with_path(mut self, path: PathOwned) -> Self {
		self.path = path;
		self
	}

	fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// Stamp a resolved broadcast with the path this cursor asked for and its egress scope.
	fn hand_out(&self, broadcast: broadcast::Consumer) -> broadcast::Consumer {
		broadcast.with_path(self.path.clone()).with_stats(self.stats.clone())
	}

	/// Poll for the requested broadcast without blocking.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<broadcast::Consumer, Error>> {
		match &self.inner {
			RequestState::Ready(broadcast) => Poll::Ready(Ok(self.hand_out(broadcast.clone()))),
			RequestState::Failed(error) => Poll::Ready(Err(error.clone())),
			RequestState::Pending(consumer) => Poll::Ready(
				match ready!(consumer.poll(waiter, |state| match &state.resolved {
					Some(result) => Poll::Ready(result.clone()),
					None => Poll::Pending,
				})) {
					Ok(result) => result.map(|broadcast| self.hand_out(broadcast)),
					// Every handler dropped without resolving: nobody could route it.
					Err(_closed) => Err(Error::Unroutable),
				},
			),
		}
	}
}

impl kio::Pollable for Requesting {
	type Output = Result<broadcast::Consumer, Error>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Derive a read view from a handle.
///
/// Lets APIs accept either a producer or a consumer (e.g.
/// [`Client::with_publisher`](crate::Client::with_publisher),
/// [`Request::accept`]). The blanket `&T` impl means you can
/// pass by value (`foo(x)`) to hand off ownership, or by reference (`foo(&x)`)
/// to keep it, without spelling out `.consume()`.
pub trait Consume<T> {
	/// Derive a read view (a consumer) from this handle.
	fn consume(&self) -> T;
}

impl<T, U: Consume<T>> Consume<T> for &U {
	fn consume(&self) -> T {
		(**self).consume()
	}
}

impl Consume<Consumer> for Producer {
	fn consume(&self) -> Consumer {
		// Mirrors the inherent `Producer::consume`; inlined to avoid the
		// inherent-vs-trait `consume` ambiguity. Untagged: egress is tagged
		// separately from ingress.
		Consumer::from_producer(self, stats::Session::default())
	}
}

impl Consume<Consumer> for Consumer {
	fn consume(&self) -> Consumer {
		self.clone()
	}
}

impl Consume<broadcast::Consumer> for broadcast::Producer {
	fn consume(&self) -> broadcast::Consumer {
		// The inherent `consume` shadows this trait method, so this delegates.
		self.consume()
	}
}

impl Consume<broadcast::Consumer> for broadcast::Consumer {
	fn consume(&self) -> broadcast::Consumer {
		self.clone()
	}
}

impl Consume<track::Consumer> for track::Producer {
	fn consume(&self) -> track::Consumer {
		self.consume()
	}
}

impl Consume<track::Consumer> for track::Consumer {
	fn consume(&self) -> track::Consumer {
		self.clone()
	}
}

/// Cheap read handle over an origin's broadcast tree and route table.
///
/// Clones share the underlying state without allocating any per-cursor
/// resources. To receive route announcements, call [`Self::announced`]; to
/// resolve a path into a broadcast, call [`Self::request_broadcast`].
#[derive(Clone)]
pub struct Consumer {
	// Identity of the origin this consumer was derived from.
	info: Hop,
	nodes: OriginNodes,

	// A prefix that is automatically stripped from all paths.
	root: PathOwned,

	// The origin's shared state: the route table, announce cursors, and the
	// fallback request queue.
	shared: kio::Shared<OriginState>,

	// Egress stats context. Broadcasts handed out through this consumer (and any
	// handle derived from them) are attributed to it (reads counted on the
	// publisher/egress side). Empty (no-op) unless a session tagged this handle.
	stats: stats::Session,

	// Split horizon: routes whose hop chain contains this peer are invisible to
	// `announced` and skipped by `request_broadcast`, so a peer is never served
	// (or advertised) its own content back. `None` (the default) filters nothing.
	exclude: Option<Hop>,

	// The origin config remote fronts inherit (identity, cache pool, retention),
	// mirroring what `create_broadcast` gives a local front.
	origin: Info,

	// Non-owning submission handle to the origin's [`Driver`], for the front
	// watcher a routed `request_broadcast` spawns. Non-owning so a lingering
	// read handle never keeps the driver from finishing.
	tasks: TasksWeak,

	// The driver's clock and timers, threaded into fronts for the track idle
	// linger.
	timers: TimersSlot,
}

impl std::ops::Deref for Consumer {
	type Target = Hop;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Consumer {
	fn from_producer(producer: &Producer, stats: stats::Session) -> Self {
		Self {
			info: producer.info,
			nodes: producer.nodes.clone(),
			root: producer.root.clone(),
			shared: producer.shared.clone(),
			stats,
			exclude: None,
			origin: producer.info(),
			tasks: producer.tasks.downgrade(),
			timers: producer.timers.clone(),
		}
	}

	/// A clone that never serves the given peer its own data: routes whose hop
	/// chain contains `peer` are invisible and never resolved from, matching what
	/// the announce loop advertises to them. Sessions apply this once they learn
	/// the peer's origin id.
	pub(crate) fn excluding(mut self, peer: Hop) -> Self {
		self.exclude = Some(peer);
		self
	}

	/// Attach an egress stats context: broadcasts handed out through this handle (and
	/// any handle derived from it) are attributed to `session` on the publisher
	/// (egress) side. Pass [`stats::Session::default`] to opt out.
	pub fn with_stats(mut self, session: stats::Session) -> Self {
		self.stats = session;
		self
	}

	/// A clone of this consumer with its stats context cleared, so an internal
	/// lookup stream (e.g. [`Self::routed`]) doesn't drive the egress
	/// announce guards; the caller re-attributes the result itself.
	fn untagged(&self) -> Self {
		Self {
			stats: stats::Session::default(),
			..self.clone()
		}
	}

	/// A view with this consumer's identity and root but no scope:
	/// [`announced`](Self::announced) yields nothing. Used to answer a peer's
	/// announce-interest for a prefix outside our scope by announcing nothing,
	/// rather than tearing the stream down.
	pub(crate) fn empty(&self) -> Self {
		Self {
			nodes: OriginNodes { nodes: Vec::new() },
			..self.clone()
		}
	}

	/// Subscribe to route announcements for this consumer's scope.
	///
	/// Allocates a per-cursor coalescing buffer and replays the currently
	/// announced routes as initial updates. A route announced above the scope is
	/// clamped to it. Drop the returned [`AnnounceConsumer`] to unregister.
	pub fn announced(&self) -> AnnounceConsumer {
		AnnounceConsumer::new(
			self.root.clone(),
			self.allowed_absolute(),
			self.stats.clone(),
			self.exclude,
			&self.shared,
		)
	}

	/// The absolute prefixes this handle is scoped to.
	fn allowed_absolute(&self) -> Vec<PathOwned> {
		self.nodes
			.nodes
			.iter()
			.map(|(allowed, _)| self.root.join(allowed).to_owned())
			.collect()
	}

	/// Returns a cheap duplicate of this read handle.
	pub fn consume(&self) -> Self {
		self.clone()
	}

	/// Internal synchronous lookup: the local broadcast at `path`, if any.
	fn resolve(&self, path: impl AsPath) -> Option<broadcast::Consumer> {
		let path = path.as_path();
		let (root, rest) = self.nodes.get(&path)?;
		let state = root.lock();
		state.resolve_broadcast(&rest)
	}

	/// [`Self::resolve`] as the peek the tests assert on.
	#[cfg(test)]
	pub(crate) fn get_broadcast(&self, path: impl AsPath) -> Option<broadcast::Consumer> {
		self.resolve(path)
	}

	/// Block until an announced route covers `path`, and return it.
	///
	/// Covering means the route's prefix is a (segment-wise) prefix of `path`,
	/// including the exact path itself. Returns `None` if the path is outside this
	/// consumer's scope or the consumer is closed first.
	///
	/// Use this before [`Self::request_broadcast`] whenever the announcement may
	/// not have arrived yet, which includes every path you resolve right after
	/// connecting: `request_broadcast` answers on the spot, so asking it first
	/// races the announcement and reports a covered path as unroutable.
	pub async fn routed(&self, path: impl AsPath) -> Option<Route> {
		let path = path.as_path();

		// Scope a fresh consumer down to this path: any covering route then clamps
		// to exactly the path, so we only wake for relevant announcements.
		let consumer = self.scope(std::slice::from_ref(&path))?;

		// `scope` keeps narrower permissions intact: if we ask for `foo` on a
		// consumer limited to `foo/specific`, no route can ever clamp to exactly
		// `foo`. Bail rather than loop forever.
		if !consumer.allowed().any(|allowed| path.has_prefix(allowed)) {
			return None;
		}

		// Use an untagged stream: this is a lookup, not egress announce
		// forwarding, so it must not drive the announce guards.
		let mut announced = consumer.untagged().announced();
		loop {
			let update = announced.next().await?;
			if update.active && update.prefix.as_path() == path {
				return Some(update.route);
			}
		}
	}

	/// Block until `path` resolves to a broadcast: [`Self::routed`], then
	/// [`Self::request_broadcast`], retried when the two race.
	///
	/// The wait and the resolution are separate steps, so the covering route can
	/// retract between them (failover churn), and a route can cover the path while
	/// nothing serves it yet (an advertise-only announce racing its handler). This
	/// rides out the churn by retrying whenever the path's coverage changes, which
	/// is what makes it the right call for resolving a path right after
	/// connecting. Returns [`Error::Unauthorized`] for a path outside this
	/// consumer's scope, [`Error::Closed`] once the origin closes, and any other
	/// resolution failure as-is.
	pub async fn routed_broadcast(&self, path: impl AsPath) -> Result<broadcast::Consumer, Error> {
		let path = path.as_path();

		// Watch the path's coverage for the retry wake: scoped so it only wakes
		// for covering routes, untagged because this is a lookup, not egress
		// announce forwarding.
		let scoped = self.scope(std::slice::from_ref(&path)).ok_or(Error::Unauthorized)?;
		// `scope` keeps narrower permissions intact: if the whole path is not
		// reachable, no route can ever cover it, so bail rather than loop forever.
		if !scoped.allowed().any(|allowed| path.has_prefix(allowed)) {
			return Err(Error::Unauthorized);
		}
		let mut announced = scoped.untagged().announced();
		loop {
			if self.routed(&path).await.is_none() {
				return Err(Error::Closed);
			}
			let request = self.request_broadcast(&path);
			// A queued request and an instant verdict fail differently. A request
			// that queued and then failed `Unroutable` was killed by its serving
			// route retracting, and an identical standby swaps in without any
			// announce update, so retry through the already-updated table
			// immediately; each such retry consumed a real retraction, so this
			// cannot spin. An instant `Unroutable` means nothing serves the path
			// right now, and only a coverage change fixes that: park on the
			// announce stream (it replays the current coverage first, so at most
			// one extra attempt runs before this genuinely blocks).
			let queued = request.is_queued();
			match request.await {
				Ok(broadcast) => return Ok(broadcast),
				Err(Error::Unroutable) if queued => {}
				Err(Error::Unroutable) => {
					if announced.next().await.is_none() {
						return Err(Error::Closed);
					}
				}
				Err(err) => return Err(err),
			}
		}
	}

	/// Returns a new Consumer restricted to broadcasts under one of `prefixes`.
	///
	/// Returns None if there are no legal prefixes (the requested prefixes are
	/// disjoint from this consumer's current scope, so it would always return None).
	// TODO accept PathPrefixes instead of &[Path]
	pub fn scope(&self, prefixes: &[Path]) -> Option<Consumer> {
		let prefixes = PathPrefixes::new(prefixes);
		Some(Consumer {
			nodes: self.nodes.select(&prefixes)?,
			..self.clone()
		})
	}

	/// Resolve a broadcast by exact path.
	///
	/// Returns a [`kio::Pending`] future (resolved synchronously where possible),
	/// mirroring [`track::Consumer::fetch_group`](track::Consumer::fetch_group).
	/// The lookup order:
	///
	/// 1. A local broadcast at the exact path ([`Producer::create_broadcast`]),
	///    announced or not, resolves immediately.
	/// 2. Otherwise the best announced route covering the path (most specific
	///    prefix first, then cheapest) serves it on demand: sessions materialize
	///    the path from the peer that announced the route. Concurrent requests
	///    for the same path coalesce onto one shared front, which outlives any
	///    single route: when its serving route dies or a better one appears, the
	///    front re-splices through the best route sharing its first hop at a
	///    group boundary, invisibly to subscribers. A route change that does not
	///    preserve the first hop ends the broadcast instead, and the next
	///    request re-serves the path.
	/// 3. Otherwise a live [`Dynamic`] handler (see [`Producer::dynamic`])
	///    receives the request as a fallback.
	///
	/// The returned future resolves to [`Error::Unroutable`] when none of those
	/// exist. A route claims capability, not inventory: resolving a covered path
	/// succeeds optimistically, and a path that names nothing surfaces as
	/// [`Error::NotFound`] on its tracks instead.
	pub fn request_broadcast(&self, path: impl AsPath) -> kio::Pending<Requesting> {
		let path = path.as_path();

		// Key requests by absolute path so scoped/rooted consumers and handlers
		// (which may have a different root) agree on the same entry, and so the egress
		// counters resolve against the same broadcast the ingress side wrote.
		let absolute = self.root.join(&path).to_owned();
		let scope = self.stats.egress(&absolute);
		// The resolved handle is named by what *this* cursor asked for, not by the absolute
		// path: a rooted cursor cannot name anything above its own root, so that is what a
		// catalog it reads may reference.
		let requested = path.to_owned();

		// A local broadcast at the exact path wins.
		if let Some(broadcast) = self.resolve(&path) {
			let resolved = Requesting::ready(broadcast).with_path(requested).with_stats(scope);
			return kio::Pending::new(resolved);
		}

		// Routes only cover paths within this consumer's scope; the fallback
		// handler is deliberately unscoped, matching the dynamic behavior.
		let in_scope = self.nodes.get(&path).is_some();

		let mut state = self.shared.lock();

		// The origin's driver dropped: nothing will ever serve this.
		if state.closed {
			return kio::Pending::new(Requesting::failed(Error::Closed));
		}

		if in_scope {
			// Join the live front for this path and exclusion, if any: its watcher
			// resolves (or already resolved) the request channel with the front's
			// spliced broadcast, so repeat requests share one upstream
			// subscription. A front whose route has since retracted still serves
			// for as long as its session does.
			let key = (absolute.clone(), self.exclude);
			if let Some(front) = state.fronts.get(&key) {
				let pending = Requesting::pending(front.request.consume())
					.with_path(requested)
					.with_stats(scope);
				return kio::Pending::new(pending);
			}

			if state
				.best_route(&absolute.as_path(), self.exclude, None, &HashSet::new())
				.is_some()
			{
				// A route covers the path: mint the front and hand its watcher the
				// request. The watcher materializes the path from the best covering
				// route, resolves the channel, and re-splices the front through
				// routes sharing its first hop for as long as one serves.
				let broadcast = broadcast::Producer::new_spliced(broadcast::Info {
					origin: self.origin.clone(),
					path: absolute.clone(),
				});
				let front_state = kio::Producer::new(FrontState {
					next_source: 0,
					sources: Vec::new(),
					active: None,
					closed: false,
				});
				let request = kio::Producer::<PendingBroadcast>::default();
				let consumer = request.consume();
				state.fronts.insert(
					key,
					RemoteFront {
						request: request.clone(),
						broadcast: broadcast.consume().weak(),
					},
				);
				self.tasks.push(run_remote_front(RemoteFrontTask {
					shared: self.shared.clone(),
					state: front_state,
					broadcast,
					path: absolute,
					exclude: self.exclude,
					request,
					tasks: self.tasks.clone(),
					timers: self.timers.clone(),
				}));
				return kio::Pending::new(Requesting::pending(consumer).with_path(requested).with_stats(scope));
			}
		}

		// Reuse a still-live broadcast a fallback handler already served for this
		// path.
		if let Some(weak) = state.served.get(&absolute) {
			let resolved = Requesting::ready(weak.consume()).with_path(requested).with_stats(scope);
			return kio::Pending::new(resolved);
		}

		// Coalesce onto a pending request for the same path; otherwise register a new
		// one, unless there is no handler alive to serve it.
		let consumer = if let Some(producer) = state.fallback.join(&absolute) {
			producer.consume()
		} else {
			let producer = kio::Producer::<PendingBroadcast>::default();
			let consumer = producer.consume();
			if state.fallback.insert(absolute, producer).is_err() {
				return kio::Pending::new(Requesting::failed(Error::Unroutable));
			}
			consumer
		};

		kio::Pending::new(Requesting::pending(consumer).with_path(requested).with_stats(scope))
	}

	/// Returns a new Consumer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when [`Self::scope`] was
	/// already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self {
			root: self.root.join(&prefix).to_owned(),
			nodes: self.nodes.root(&prefix)?,
			..self.clone()
		})
	}

	/// Returns the prefix that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// Iterate over the path prefixes this handle is permitted to publish or subscribe under.
	// TODO return PathPrefixes
	pub fn allowed(&self) -> impl Iterator<Item = &Path<'_>> {
		self.nodes.nodes.iter().map(|(root, _)| root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

/// Receives route announcements for a scope.
///
/// Created by [`Consumer::announced`].
/// Drop to unregister.
pub struct AnnounceConsumer {
	id: ConsumerId,
	shared: kio::Shared<OriginState>,
	root: PathOwned,

	// Pending updates queued for this cursor. Coalesced so a slow consumer
	// can't accumulate redundant announce/retract pairs.
	state: kio::Producer<OriginConsumerState>,

	// Egress stats context (empty for an untagged stream). Announce events drive the
	// per-prefix announce guards below.
	stats: stats::Session,

	// Live egress announce guards, keyed by absolute prefix. An announce
	// opens one (bumping `announced` + `announced_bytes`); the matching retraction
	// drops it (bumping `announced_closed` + `announced_bytes`).
	guards: HashMap<PathOwned, stats::Announce>,
}

impl AnnounceConsumer {
	fn new(
		root: PathOwned,
		allowed: Vec<PathOwned>,
		stats: stats::Session,
		exclude: Option<Hop>,
		shared: &kio::Shared<OriginState>,
	) -> Self {
		let state = kio::Producer::<OriginConsumerState>::default();
		let id = ConsumerId::new();

		{
			let mut table = shared.lock();
			if table.closed {
				// A cursor on a dead origin is born ended.
				if let Ok(mut state) = state.write() {
					state.ended = true;
				}
			} else {
				table.register_cursor(
					id,
					TableCursor {
						root: root.clone(),
						allowed,
						exclude,
						state: state.clone(),
						current: HashMap::new(),
					},
				);
			}
		}

		Self {
			id,
			shared: shared.clone(),
			root,
			state,
			stats,
			guards: HashMap::new(),
		}
	}

	/// Drive the egress announce guards for one update.
	fn hand_out(&mut self, update: AnnounceUpdate) -> AnnounceUpdate {
		let absolute = self.root.join(update.prefix.as_path()).to_owned();
		if update.active {
			let scope = self.stats.egress(&absolute);
			self.guards.entry(absolute).or_insert_with(|| scope.announce());
		} else {
			self.guards.remove(&absolute);
		}
		update
	}

	/// Returns the next route announcement or retraction, its prefix relative to
	/// this cursor's root.
	///
	/// A retraction is only delivered for a previously announced prefix, and a
	/// repeated announcement for the same prefix is a metadata update. Returns
	/// None if the cursor is closed.
	pub async fn next(&mut self) -> Option<AnnounceUpdate> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next update, without blocking.
	///
	/// Returns `Poll::Ready(Some(_))` for an update, `Poll::Ready(None)` if the
	/// cursor is closed, or `Poll::Pending` after registering `waiter` to be
	/// notified when the next update arrives.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<AnnounceUpdate>> {
		let update = {
			let mut state = match ready!(self.state.poll(waiter, |state| {
				if state.pending.is_empty() && !state.ended {
					Poll::Pending
				} else {
					Poll::Ready(())
				}
			})) {
				Ok(state) => state,
				// Closed: discard the Ref so its MutexGuard doesn't escape this call.
				Err(_) => return Poll::Ready(None),
			};
			match state.take() {
				Some(update) => update,
				None => {
					// Ended by the origin's teardown, pending updates already
					// drained; close the channel so every closure signal agrees.
					state.close();
					return Poll::Ready(None);
				}
			}
		};
		Poll::Ready(Some(self.hand_out(update)))
	}

	/// Returns the next update without blocking.
	///
	/// Returns None if there is no update available; NOT because the cursor is closed.
	/// Use [`Self::is_closed`] to check if the cursor is closed.
	pub fn try_next(&mut self) -> Option<AnnounceUpdate> {
		let update = self.state.write().ok()?.take()?;
		Some(self.hand_out(update))
	}

	/// Returns true if the cursor is closed (no more updates will arrive).
	pub fn is_closed(&self) -> bool {
		let state = self.state.read();
		state.is_closed() || state.ended
	}

	/// Returns the prefix that is automatically stripped from emitted paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

impl Drop for AnnounceConsumer {
	fn drop(&mut self) {
		self.shared.lock().cursors.remove(&self.id);
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl AnnounceConsumer {
	/// The next update must be an active route at `expected`; returns it.
	pub fn assert_next_active(&mut self, expected: impl AsPath) -> Route {
		let expected = expected.as_path();
		let update = self.next().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(update.prefix.as_path(), expected, "wrong prefix");
		assert!(update.active, "should be an active route");
		update.route
	}

	/// The `try_next` counterpart of [`Self::assert_next_active`].
	pub fn assert_try_next_active(&mut self, expected: impl AsPath) -> Route {
		let expected = expected.as_path();
		let update = self.try_next().expect("no next");
		assert_eq!(update.prefix.as_path(), expected, "wrong prefix");
		assert!(update.active, "should be an active route");
		update.route
	}

	/// The next update must be a retraction at `expected`.
	pub fn assert_next_ended(&mut self, expected: impl AsPath) {
		let expected = expected.as_path();
		let update = self.next().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(update.prefix.as_path(), expected, "wrong prefix");
		assert!(!update.active, "should be a retraction");
	}

	pub fn assert_next_wait(&mut self) {
		if let Some(res) = self.next().now_or_never() {
			panic!("next should block: got {:?}", res.map(|u| u.prefix));
		}
	}
}

/// Test-only construction shorthand: build the producer and spawn its driver on
/// the ambient tokio runtime, mirroring what `moq_tokio::origin::spawn` does
/// for applications.
#[cfg(test)]
pub(crate) trait ProduceTest {
	fn produce(self) -> Producer;
}

#[cfg(test)]
impl ProduceTest for Info {
	fn produce(self) -> Producer {
		let (producer, driver) = Producer::new(self);
		if tokio::runtime::Handle::try_current().is_ok() {
			web_async::spawn(driver.run(crate::runtime::tokio_test::Tokio::<()>::new()));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}
}

#[cfg(test)]
impl ProduceTest for Hop {
	fn produce(self) -> Producer {
		Info::new(self).produce()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use futures::FutureExt;

	fn origin(id: u64) -> Hop {
		Hop::new(id).unwrap()
	}

	fn hops(ids: &[u64]) -> Hops {
		let mut list = Hops::new();
		for id in ids {
			list.push(origin(*id)).unwrap();
		}
		list
	}

	/// Yield to the driver until `check` passes, bounded so a bug fails instead
	/// of hanging.
	async fn settle(mut check: impl FnMut() -> bool) {
		for _ in 0..100 {
			if check() {
				return;
			}
			tokio::task::yield_now().await;
		}
		panic!("condition never settled");
	}

	/// Yield to the driver until the server's front watcher delivers a request.
	async fn queued(server: &mut RouteServer) -> Request {
		let mut request = None;
		settle(|| match server.poll_requested_broadcast(&kio::Waiter::noop()) {
			Poll::Ready(Ok(popped)) => {
				request = Some(popped);
				true
			}
			_ => false,
		})
		.await;
		request.unwrap()
	}

	#[tokio::test]
	async fn announce_and_retract() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();
		announced.assert_next_wait();

		let announcement = producer.announce("room/alice", Route::default()).unwrap();
		let route = announced.assert_next_active("room/alice");
		assert!(route.hops.is_empty());
		assert_eq!(route.cost, Cost::default());
		announced.assert_next_wait();

		drop(announcement);
		announced.assert_next_ended("room/alice");
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn announce_replays_to_late_cursor() {
		let producer = origin(1).produce();
		let _a = producer.announce("room/alice", Route::default()).unwrap();
		let _b = producer.announce("room/bob", Route::default()).unwrap();

		let mut announced = producer.consume().announced();
		// BTreeMap order: lexicographic by prefix.
		announced.assert_next_active("room/alice");
		announced.assert_next_active("room/bob");
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn announce_clamps_to_producer_scope() {
		let producer = origin(1).produce();
		let scoped = producer.scope(&[Path::new("room")]).unwrap();

		// A broad claim through a narrow scope advertises only the intersection.
		let _a = scoped.announce("", Route::default()).unwrap();
		let mut announced = producer.consume().announced();
		announced.assert_next_active("room");

		// Disjoint prefixes cannot be claimed at all.
		assert!(matches!(
			scoped.announce("other", Route::default()),
			Err(Error::Unauthorized)
		));
	}

	#[tokio::test]
	async fn cursor_clamps_above_its_scope() {
		let producer = origin(1).produce();
		let _a = producer.announce("", Route::default()).unwrap();

		let consumer = producer.consume().scope(&[Path::new("room")]).unwrap();
		let mut announced = consumer.announced();
		announced.assert_next_active("room");
	}

	#[tokio::test]
	async fn cursor_root_strips_prefix() {
		let producer = origin(1).produce();
		let _a = producer.announce("room/alice", Route::default()).unwrap();

		let consumer = producer.consume().with_root("room").unwrap();
		let mut announced = consumer.announced();
		announced.assert_next_active("alice");
	}

	#[tokio::test]
	async fn best_route_wins_and_fails_over() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();

		let expensive = producer
			.announce("room", Route::default().with_hops(hops(&[10])).with_cost(5))
			.unwrap();
		let route = announced.assert_next_active("room");
		assert_eq!(route.cost, Cost::new(5));

		// A cheaper route for the same prefix takes over in place.
		let cheap = producer
			.announce("room", Route::default().with_hops(hops(&[20])).with_cost(1))
			.unwrap();
		let route = announced.assert_next_active("room");
		assert_eq!(route.cost, Cost::new(1));

		// Losing the winner falls back to the survivor, still in place.
		drop(cheap);
		let route = announced.assert_next_active("room");
		assert_eq!(route.cost, Cost::new(5));

		// Losing the last retracts.
		drop(expensive);
		announced.assert_next_ended("room");
	}

	#[tokio::test]
	async fn identical_reannounce_is_invisible() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();

		let old = producer
			.announce("room", Route::default().with_hops(hops(&[10])))
			.unwrap();
		let first = announced.assert_next_active("room");
		assert_eq!(first.hops.as_slice(), hops(&[10]).as_slice());

		// An identical route from a fresh announcement (a reconnect) changes
		// nothing a consumer could act on, so nothing is delivered; new requests
		// still prefer the newest entry.
		let _new = producer
			.announce("room", Route::default().with_hops(hops(&[10])))
			.unwrap();
		announced.assert_next_wait();

		// Retracting the stale twin leaves the fresh one standing, still quietly.
		drop(old);
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn exclude_hides_routes_through_the_peer() {
		let producer = origin(1).produce();
		let _a = producer
			.announce("room", Route::default().with_hops(hops(&[7])))
			.unwrap();

		let mut hidden = producer.consume().excluding(origin(7)).announced();
		hidden.assert_next_wait();

		let mut visible = producer.consume().excluding(origin(8)).announced();
		visible.assert_next_active("room");
	}

	#[tokio::test]
	async fn update_reprices_in_place() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();

		let announcement = producer.announce("room", Route::default()).unwrap();
		announced.assert_next_active("room");

		announcement.update(Route::default().with_cost(9)).unwrap();
		let route = announced.assert_next_active("room");
		assert_eq!(route.cost, Cost::new(9));
	}

	#[tokio::test]
	async fn retract_after_undelivered_reprice_still_delivered() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();

		let announcement = producer.announce("room", Route::default()).unwrap();
		announced.assert_next_active("room");

		// Reprice, then retract before the consumer observes the reprice: the
		// pending metadata update must not cancel the retraction the delivered
		// announce still owes.
		announcement.update(Route::default().with_cost(9)).unwrap();
		drop(announcement);
		announced.assert_next_ended("room");
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn scoped_cursor_advertises_most_specific_covering_route() {
		let producer = origin(1).produce();
		// Broad and cheap; narrow and expensive. Both clamp to a cursor rooted
		// below them, and the narrow one is what a request there resolves.
		let _broad = producer.announce("room", Route::default().with_cost(1)).unwrap();
		let _narrow = producer.announce("room/alice", Route::default().with_cost(9)).unwrap();

		let consumer = producer.consume().with_root("room/alice").unwrap();
		let mut announced = consumer.announced();
		let route = announced.assert_next_active("");
		assert_eq!(route.cost, Cost::new(9));
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn routed_broadcast_resolves_once_announced() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Asking before anything is announced parks instead of failing Unroutable.
		let mut resolving = Box::pin(consumer.routed_broadcast("room/alice"));
		assert!((&mut resolving).now_or_never().is_none());

		let broadcast = producer.create_broadcast("room/alice").unwrap();
		let _announcement = producer.announce("room/alice", Route::default()).unwrap();
		let resolved = resolving
			.now_or_never()
			.expect("resolves once announced")
			.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		drop(broadcast);
	}

	#[tokio::test]
	async fn local_broadcast_resolves_by_exact_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.create_broadcast("room/alice").unwrap();
		let resolved = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("local lookup is synchronous")
			.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		drop(broadcast);

		// Nothing covers an unknown path and no handler exists.
		let err = consumer
			.request_broadcast("room/bob")
			.now_or_never()
			.expect("unroutable is synchronous")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
	}

	#[tokio::test]
	async fn local_broadcast_is_not_announced() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();
		let _broadcast = producer.create_broadcast("room/alice").unwrap();
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn served_route_materializes_on_demand() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let (_announcement, mut server) = producer.announce_served("room", Route::default()).unwrap();

		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&mut server).await;
		assert_eq!(request.path().as_str(), "room/alice");

		let source = broadcast::Info::new().produce();
		request.accept(&source);

		let resolved = pending.await.expect("resolves");
		// The handle is named by what the requester asked for.
		assert_eq!(resolved.info().path.as_str(), "room/alice");

		// A repeat request shares the served broadcast instead of re-asking.
		let again = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("cached")
			.expect("resolves");
		assert!(again.is_clone(&resolved));
	}

	#[tokio::test]
	async fn served_requests_coalesce() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let (_announcement, mut server) = producer.announce_served("room", Route::default()).unwrap();

		let first = consumer.request_broadcast("room/alice");
		let second = consumer.request_broadcast("room/alice");

		let request = queued(&mut server).await;
		// Only one request reaches the server.
		assert!(server.poll_requested_broadcast(&kio::Waiter::noop()).is_pending());

		let source = broadcast::Info::new().produce();
		request.accept(&source);

		let first = first.await.expect("resolves");
		let second = second.await.expect("resolves");
		assert!(first.is_clone(&second));
	}

	#[tokio::test]
	async fn retract_rejects_pending_requests() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let (announcement, server) = producer.announce_served("room", Route::default()).unwrap();

		let pending = consumer.request_broadcast("room/alice");
		drop(announcement);
		drop(server);

		let err = pending.await.err().unwrap();
		assert!(matches!(err, Error::Unroutable));

		// With the route gone, later requests are unroutable immediately.
		let err = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
	}

	#[tokio::test]
	async fn routed_broadcast_survives_serving_route_retraction() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Three identical routes, oldest first: the newest identical route wins
		// requests, and swapping between them emits no announce update.
		let (_standby, mut standby_server) = producer.announce_served("room", Route::default()).unwrap();
		let (second, second_server) = producer.announce_served("room", Route::default()).unwrap();
		let (incumbent, incumbent_server) = producer.announce_served("room", Route::default()).unwrap();

		let mut resolving = Box::pin(consumer.routed_broadcast("room/alice"));
		assert!((&mut resolving).now_or_never().is_none());

		// Each incumbent dies with the front's request in flight on it: the
		// front's watcher observes the retraction and retries through the next
		// standby instead of parking on an announce update that never comes. Two
		// retractions in a row, so the announce stream's initial coverage replay
		// cannot paper over the missing retry.
		drop(incumbent);
		drop(incumbent_server);
		assert!((&mut resolving).now_or_never().is_none());
		drop(second);
		drop(second_server);
		assert!((&mut resolving).now_or_never().is_none());

		let request = queued(&mut standby_server).await;
		let source = broadcast::Info::new().produce();
		request.accept(&source);

		let resolved = resolving.await.expect("resolves via the standby");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
	}

	#[tokio::test]
	async fn split_horizon_skips_routes_through_the_requester() {
		let producer = origin(1).produce();
		let (_announcement, _server) = producer
			.announce_served("room", Route::default().with_hops(hops(&[7])))
			.unwrap();

		// The requester's own bytes must not be served back to it.
		let excluded = producer.consume().excluding(origin(7));
		let err = excluded
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		// A clean requester resolves through the route (the request queues).
		let clean = producer.consume().excluding(origin(8));
		let pending = clean.request_broadcast("room/alice");
		assert!(pending.now_or_never().is_none());
	}

	#[tokio::test]
	async fn most_specific_prefix_shadows() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let (_broad, mut broad_server) = producer.announce_served("", Route::default()).unwrap();
		// A narrow advertise-only claim: requests under it must NOT route to the
		// broad server; they fall through to the (absent) fallback handler.
		let _narrow = producer.announce(".dash", Route::default()).unwrap();

		let err = consumer
			.request_broadcast(".dash/pid")
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		// Everything else still routes to the broad server.
		let _pending = consumer.request_broadcast("room/alice");
		let request = queued(&mut broad_server).await;
		assert_eq!(request.path().as_str(), "room/alice");
	}

	#[tokio::test]
	async fn dynamic_fallback_serves_uncovered_paths() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut dynamic = producer.dynamic();

		let pending = consumer.request_broadcast("anything/at/all");
		let request = match dynamic.poll_requested_broadcast(&kio::Waiter::noop()) {
			Poll::Ready(Ok(request)) => request,
			_ => panic!("expected a queued request"),
		};
		assert_eq!(request.path().as_str(), "anything/at/all");

		let source = broadcast::Info::new().produce();
		request.accept(&source);
		let resolved = pending.now_or_never().expect("accepted").expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "anything/at/all");
	}

	#[tokio::test]
	async fn routed_waits_for_coverage() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let mut fut = consumer.routed("room/alice").boxed();
		assert!((&mut fut).now_or_never().is_none());

		// A covering prefix resolves the wait, clamped to the requested path.
		let _a = producer.announce("room", Route::default().with_cost(3)).unwrap();
		let route = fut.now_or_never().expect("covered").expect("routed");
		assert_eq!(route.cost, Cost::new(3));

		// Already covered: resolves immediately.
		consumer
			.routed("room/alice/cam")
			.now_or_never()
			.expect("covered")
			.expect("routed");
	}

	#[tokio::test]
	async fn routed_ignores_deeper_routes() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// A deeper route does not cover the shorter path.
		let _deep = producer.announce("room/alice/cam", Route::default()).unwrap();
		let mut fut = consumer.routed("room/alice").boxed();
		assert!((&mut fut).now_or_never().is_none());

		let _exact = producer.announce("room/alice", Route::default()).unwrap();
		fut.now_or_never().expect("covered").expect("routed");
	}

	#[tokio::test]
	async fn teardown_ends_everything() {
		let (producer, driver) = Producer::new(Info::new(origin(1)));
		let consumer = producer.consume();
		let _announcement = producer.announce("room", Route::default()).unwrap();
		let mut announced = consumer.announced();
		announced.assert_next_active("room");

		let (_a2, _server) = producer.announce_served("served", Route::default()).unwrap();
		let pending = consumer.request_broadcast("served/path");

		drop(driver);

		// The cursor observes the end (after draining pending updates).
		announced.assert_next_active("served");
		assert!(announced.next().now_or_never().expect("ended").is_none());

		// Pending requests reject; new work refuses.
		assert!(pending.now_or_never().expect("rejected").is_err());
		assert!(matches!(producer.announce("x", Route::default()), Err(Error::Closed)));
		assert!(matches!(producer.create_broadcast("x"), Err(Error::Closed)));
		let err = consumer
			.request_broadcast("y")
			.now_or_never()
			.expect("closed")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Closed));

		// A cursor born after the teardown is born ended.
		let mut late = consumer.announced();
		assert!(late.next().now_or_never().expect("ended").is_none());
	}

	/// One live subscription reading a track through a remote front, plus the
	/// bookkeeping to kill and replace its serving route.
	struct ResumeRig {
		producer: Producer,
		resolved: broadcast::Consumer,
		subscription: track::Subscriber,
		/// Keeps the incumbent's track producing; dropping it would abort the
		/// track out from under the front mid-test.
		_incumbent_track: track::Producer,
	}

	impl ResumeRig {
		/// Announce a served route with `first` as its first hop, materialize
		/// "room/alice" through it with a one-group "before" track, and subscribe.
		async fn new(first: &[u64]) -> (Self, AnnounceProducer, broadcast::Producer) {
			let producer = origin(1).produce();
			let consumer = producer.consume();

			let (announcement, mut server) = producer
				.announce_served("room", Route::default().with_hops(hops(first)))
				.unwrap();

			let pending = consumer.request_broadcast("room/alice");
			let request = queued(&mut server).await;
			let mut source = broadcast::Info::new().produce();
			let mut track = source.create_track("video", None).unwrap();
			let mut group = track.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"before".as_ref()).unwrap();
			group.finish().unwrap();
			request.accept(&source);

			let resolved = pending.await.expect("resolves");
			let mut subscription = resolved
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.expect("subscribe");
			let mut group = subscription
				.recv_group()
				.await
				.expect("recv group")
				.expect("track ended early");
			let frame = group.read_frame().await.expect("read frame").expect("frame");
			assert_eq!(&frame.payload[..], b"before");

			(
				Self {
					producer,
					resolved,
					subscription,
					_incumbent_track: track,
				},
				announcement,
				source,
			)
		}

		/// Stand up a second served route with `first` as its first hop and hand
		/// back its server, ready to answer the front's re-request.
		fn standby(&self, first: &[u64]) -> (AnnounceProducer, RouteServer) {
			self.producer
				.announce_served("room", Route::default().with_hops(hops(first)))
				.unwrap()
		}
	}

	/// Accept the front's re-request on `server` with a source carrying the same
	/// content stream (the delivered group plus its successor) and prove the
	/// rig's subscription resumes onto it: the successor group is delivered on
	/// the same subscription, at the group boundary.
	async fn assert_resumes(rig: &mut ResumeRig, server: &mut RouteServer) {
		let request = queued(server).await;
		let mut replacement = broadcast::Info::new().produce();
		let mut track = replacement.create_track("video", None).unwrap();
		// The same content: group 0 was already delivered through the old route,
		// so the splice resumes at group 1.
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"before".as_ref()).unwrap();
		group.finish().unwrap();
		request.accept(&replacement);

		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"resumed".as_ref()).unwrap();
		group.finish().unwrap();

		let mut group = rig
			.subscription
			.recv_group()
			.await
			.expect("subscription survives the failover")
			.expect("track ended early");
		let frame = group.read_frame().await.expect("read frame").expect("frame");
		assert_eq!(&frame.payload[..], b"resumed");
	}

	/// A `RouteServer` dropped while its announcement stands leaves the entry in
	/// the table with a closed queue. The front must refuse it rather than
	/// re-pick it forever, which would spin the origin driver on one core.
	#[tokio::test]
	async fn dropped_server_with_live_announcement_is_refused() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let (_announcement, server) = producer.announce_served("room", Route::default()).unwrap();
		drop(server);

		let err = tokio::time::timeout(Duration::from_secs(5), consumer.request_broadcast("room/alice"))
			.await
			.expect("the front must give up, not spin")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
	}

	/// The driver's completion contract: it resolves once every producer handle
	/// drops, however many read handles remain.
	#[tokio::test]
	async fn driver_resolves_with_live_consumers() {
		let (producer, driver) = Producer::new(Info::new(origin(1)));
		let consumer = producer.consume();
		let run = driver.run(crate::runtime::tokio_test::Tokio::<()>::new());
		drop(producer);
		tokio::time::timeout(Duration::from_secs(5), run)
			.await
			.expect("driver must finish once the producers are gone");
		drop(consumer);
	}

	#[tokio::test]
	async fn remote_source_resumes_through_same_first_hop() {
		let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
		let (_standby, mut standby_server) = rig.standby(&[10, 20]);

		// The serving route dies: retraction plus source abort, like a session.
		drop(incumbent);
		drop(source);

		// The standby shares the first hop, so the subscription resumes there.
		assert_resumes(&mut rig, &mut standby_server).await;
	}

	#[tokio::test]
	async fn different_first_hop_ends_the_subscription() {
		let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
		// Another publisher entirely: same path, different first hop.
		let (_rival, mut rival_server) = rig.standby(&[11]);

		drop(incumbent);
		drop(source);

		// The subscription ends rather than splicing onto the rival's frames.
		let err = rig.subscription.recv_group().await.err().expect("subscription ends");
		assert!(matches!(err, Error::Dropped), "unexpected end: {err}");

		// A fresh request resolves through the rival.
		let consumer = rig.producer.consume();
		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&mut rival_server).await;
		let replacement = broadcast::Info::new().produce();
		request.accept(&replacement);
		pending.await.expect("re-request resolves through the rival");
	}

	#[tokio::test]
	async fn anonymous_routes_never_resume() {
		// An empty hop chain identifies nobody, so two of them must not pass for
		// one publisher reconnecting.
		let (mut rig, incumbent, source) = ResumeRig::new(&[]).await;
		let (_twin, _twin_server) = rig.standby(&[]);

		drop(incumbent);
		drop(source);

		let err = rig.subscription.recv_group().await.err().expect("subscription ends");
		assert!(matches!(err, Error::Dropped), "unexpected end: {err}");
	}

	#[tokio::test]
	async fn reprice_is_invisible_to_the_subscription() {
		let (rig, incumbent, mut source) = ResumeRig::new(&[10]).await;

		// A metadata-only reprice of the only route: nothing re-requests and the
		// subscription keeps flowing from the same source.
		incumbent
			.update(Route::default().with_hops(hops(&[10])).with_cost(9))
			.unwrap();

		let mut track = source.create_track("audio", None).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"steady".as_ref()).unwrap();
		group.finish().unwrap();

		let mut audio = rig
			.resolved
			.track("audio")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe survives the reprice");
		let mut group = audio
			.recv_group()
			.await
			.expect("recv group")
			.expect("track ended early");
		let frame = group.read_frame().await.expect("read frame").expect("frame");
		assert_eq!(&frame.payload[..], b"steady");
	}

	#[tokio::test]
	async fn drain_reprice_migrates_before_the_session_dies() {
		let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
		let (_standby, mut standby_server) = rig.standby(&[10, 20]);

		// The serving route drains: repriced to the ceiling while its session
		// keeps serving. The front migrates to the standby without waiting for
		// the death.
		incumbent
			.update(Route::default().with_hops(hops(&[10])).with_cost(DRAIN_COST))
			.unwrap();

		assert_resumes(&mut rig, &mut standby_server).await;

		// The drained source outlived the migration.
		drop(incumbent);
		drop(source);
	}

	#[tokio::test]
	async fn local_sources_splice_newest_first() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let mut first = producer.create_broadcast("room/alice").unwrap();
		let resolved = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("resolves")
			.expect("resolves");

		// A second source at the same path joins the same front.
		let mut second = producer.create_broadcast("room/alice").unwrap();
		let again = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("resolves")
			.expect("resolves");
		assert!(again.is_clone(&resolved));

		// Losing one source keeps the front alive; losing both closes it.
		first.finish();
		settle(|| consumer.get_broadcast("room/alice").is_some()).await;
		second.finish();
		settle(|| consumer.get_broadcast("room/alice").is_none()).await;

		// The path is free again for a fresh broadcast.
		let _third = producer.create_broadcast("room/alice").unwrap();
		assert!(consumer.get_broadcast("room/alice").is_some());
	}

	#[tokio::test]
	async fn multiple_scopes_present_a_broad_route_at_each() {
		let producer = origin(1).produce();
		let _a = producer.announce("", Route::default()).unwrap();

		let consumer = producer
			.consume()
			.scope(&[Path::new("alpha"), Path::new("beta")])
			.unwrap();
		let mut announced = consumer.announced();
		announced.assert_next_active("alpha");
		announced.assert_next_active("beta");
		announced.assert_next_wait();
	}

	/// Charging a link accumulates onto both halves, saturating rather than wrapping
	/// so a bogus peer sorts last, not first. The ceiling is the largest cost a
	/// varint can carry, so whatever a peer advertises, the sum we forward still
	/// encodes.
	#[test]
	fn cost_charge_saturates() {
		assert_eq!(Cost { warm: 4, cold: 6 }.charged(5), Cost { warm: 9, cold: 11 });
		assert_eq!(Cost::new(u64::MAX).charged(10), Cost::new(MAX_COST));

		// An unknown cold path stays unknown however many links it crosses, so it
		// can never accumulate its way into outranking a path we actually know.
		assert_eq!(Cost::UNKNOWN.charged(3).cold, MAX_COST);
	}

	/// A draining cost still has to fit the wire, since the route keeps being
	/// announced downstream while it drains.
	#[test]
	fn drain_cost_is_encodable() {
		use crate::coding::Encode;

		let mut buf = Vec::new();
		Cost::DRAIN
			.encode(&mut buf, crate::lite::Version::Lite06Wip)
			.expect("a draining route is still forwarded, so its cost must encode");
	}
}
