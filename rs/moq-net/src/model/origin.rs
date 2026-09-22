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

use kio::Lock;
use rand::RngExt;

use super::{Requests, WeakCache, WeakEntry};
use crate::{
	AsPath, Error, InvalidPattern, Path, PathOwned, Pattern, Patterns,
	coding::{BoundsExceeded, Decode, DecodeError, Encode, EncodeError},
	runtime::{Instant, Timers},
	time::Clock,
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
/// on the wire, names nobody, and marks the chain anonymous for route selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hop {
	/// 62-bit identifier. Encoded as a QUIC varint on the wire.
	id: u64,
}

impl Hop {
	/// The reserved id 0: no identity.
	///
	/// It stands in for an endpoint that never declared one, and for Lite03 hop-count
	/// placeholders. Any number of endpoints can be 0, so it identifies nothing: it is
	/// never a loop, never a publisher two chains have in common, and a chain that
	/// holds one anywhere is anonymous for route selection.
	pub const UNKNOWN: Self = Self { id: 0 };

	/// Build a hop from a stable id.
	///
	/// The id must be non-zero and fit in the 62-bit QUIC varint range. Wire
	/// decode accepts remote id 0 ([`Self::UNKNOWN`]), but a local hop should
	/// not use it because it cannot be excluded for loop detection.
	pub fn new(id: u64) -> Result<Self, InvalidHop> {
		if id == 0 || id >= 1u64 << 62 {
			return Err(InvalidHop::Range);
		}
		Ok(Self { id })
	}

	/// Generate a fresh hop with a random non-zero id. Use this for any relay that
	/// does not need a stable identity across restarts.
	pub fn random() -> Self {
		let mut rng = rand::rng();
		let id = rng.random_range(1..(1u64 << 62));
		Self { id }
	}

	/// Return the origin's wire id.
	pub fn id(self) -> u64 {
		self.id
	}

	/// Build a hop from an id read off the wire, where 0 is legal.
	pub(crate) fn from_wire(id: u64) -> Result<Self, DecodeError> {
		if id >= 1u64 << 62 {
			return Err(DecodeError::InvalidValue);
		}
		Ok(Self { id })
	}
}

/// An origin's identity plus the cache pool its broadcasts inherit.
///
/// Construction config for an [origin `Producer`](Producer). The origin passes its
/// [`cache::Pool`] to every broadcast it creates, so every track and group beneath it
/// shares one budget. Defaults to no byte target and the cache's standard idle expiry.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The origin's wire identity, appended to broadcast hop chains for loop
	/// detection and shortest-path routing.
	pub hop: Hop,

	/// The cache pool broadcasts under this origin charge their groups into. It flows
	/// down the ownership chain (origin -> broadcast -> track -> group): a track opens
	/// an account against it, and its groups charge through that. It has no byte target
	/// and uses [`cache::DEFAULT_EXPIRY`] by default; a relay sets a shared configured
	/// pool (assign [`Self::pool`]) so cached groups across the whole process share
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

impl Default for Config {
	/// A fresh random hop with no byte target and the default idle expiry.
	fn default() -> Self {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY));
		Self {
			hop: Hop::random(),
			pool,
			cache_duration: Duration::MAX,
			default_max_age: track::DEFAULT_MAX_AGE,
		}
	}
}

impl Config {
	/// Config for the given origin id with no byte target and the default idle expiry.
	pub fn new(hop: Hop) -> Self {
		Self { hop, ..Self::default() }
	}
}

impl From<Hop> for Config {
	/// Config for the given origin id with the defaults of [`Config::new`].
	fn from(hop: Hop) -> Self {
		Self::new(hop)
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
		Self::from_wire(u64::decode(r, version)?)
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
const MAX_COST: u64 = (1 << 62) - 1;

/// What pulling content via a route costs, in two magnitudes that accumulate
/// together and are compared in that order: lower [`warm`](Self::warm) wins, and
/// [`cold`](Self::cold) breaks the tie.
///
/// Both are the same path priced against different cache states. `warm` is what one
/// more subscription would cost the mesh right now; `cold` prices the identical
/// path as if nothing were cached, so it stays meaningful once discounts have
/// flattened `warm`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
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
	/// Accumulates exactly like [`warm`](Self::warm) but never restarts. [`MAX`](Self::MAX)
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

	/// The highest cost either half can take, and where accumulation saturates.
	///
	/// A draining session stamps this on its routes so every other candidate outranks
	/// them while they stay selectable as the last path to the content. Draining is
	/// not a distinct state: cost is the whole mechanism, and a route whose accumulated
	/// cost saturates the wire ceiling ranks (and is treated) the same way.
	pub const MAX: Self = Self::new(MAX_COST);

	/// A draining route: [`MAX`](Self::MAX) in both magnitudes, so every other
	/// candidate outranks it.
	pub const DRAIN: Self = Self::MAX;

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

/// The path a route took through the mesh and what using it costs.
///
/// The metadata half of an advertisement: [`Producer::dynamic`] pairs it with
/// the prefix it covers, [`broadcast::Producer::announce`] with the
/// broadcast's exact path, and [`Consumer::announced`] yields both. A route
/// claims capability, not inventory: it says paths under its prefix are
/// servable, never that any specific broadcast exists. The common convention is
/// that a publisher announces each broadcast's exact path, so subscribers can
/// enumerate broadcasts; a service instead announces one short prefix and
/// answers whatever is requested beneath it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
	/// The chain of origins the route has traversed, oldest first. Each relay
	/// appends its own [`crate::Hop`] when forwarding; used for loop detection
	/// and as the selection tie-break. A 0 entry is the anonymous mark and
	/// travels unchanged; see [`Self::is_anonymous`].
	pub hops: Hops,

	/// What pulling content via this route costs, accumulated per link: lower wins
	/// among routes of the same anonymity, with ties broken by hop length, then a
	/// deterministic hash, and finally the most recently announced route. See [`Cost`].
	pub cost: Cost,

	/// The announcing session's declared or assigned identity.
	///
	/// Local selection state: split-horizon matches this as well as the chain, so a
	/// route is never advertised back to the session it came from even when that
	/// session withheld an identity (hop 0). Never forwarded.
	pub(crate) via: Hop,
}

impl Default for Route {
	fn default() -> Self {
		Self {
			hops: Hops::new(),
			cost: Cost::default(),
			via: Hop::UNKNOWN,
		}
	}
}

impl Route {
	/// Replace the hop chain.
	pub fn with_hops(mut self, hops: Hops) -> Self {
		self.hops = hops;
		self
	}

	/// Set the cost: lower wins among routes covering the same prefix and anonymity.
	///
	/// A bare `u64` prices the route undiscounted (both halves of [`Cost`] alike),
	/// which is what a publisher seeding its production cost means.
	pub fn with_cost(mut self, cost: impl Into<Cost>) -> Self {
		self.cost = cost.into();
		self
	}

	/// The announcing session's declared or assigned identity, for split-horizon.
	///
	/// Not part of the advertised route: an assigned identity is private selection
	/// state and must not be forwarded.
	pub(crate) fn with_via(mut self, via: Hop) -> Self {
		self.via = via;
		self
	}

	/// Whether this route passed through an anonymous hop.
	///
	/// True when the chain holds a 0 anywhere, including Lite03 hop-count
	/// placeholders. An anonymous route ranks below every fully identified one,
	/// whatever the costs say. An empty chain is a local announcement, not the
	/// anonymous mark; ingress fills a received empty list with 0 before it
	/// enters the table.
	pub fn is_anonymous(&self) -> bool {
		self.hops.iter().any(|hop| *hop == Hop::UNKNOWN)
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
fn fnv_key(name: &str, origins: impl IntoIterator<Item = Hop>) -> u64 {
	const SEED: u64 = 0x420C0DECB00B; // 420 C0DEC B00B
	const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

	let mut hash = SEED;
	for &byte in name.as_bytes() {
		hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
	}
	for origin in origins {
		for &byte in &origin.id().to_le_bytes() {
			hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
		}
	}

	hash
}

/// Ordering key for a route entry covering one prefix. Lower wins: an identified
/// chain (no 0) outranks an anonymous one regardless of cost, then the cheapest
/// cost, then the shortest hop chain, then a deterministic hash of the prefix and
/// chain so every node converges on the same winner, and finally the newest
/// announcement, so a reconnect under an otherwise identical route wins the
/// moment it lands instead of after the transport retires the old session.
fn route_order(prefix: &Path, entry: &RouteEntry) -> (bool, Cost, usize, u64, Reverse<u64>) {
	(
		entry.is_anonymous(),
		entry.cost,
		entry.hops.len(),
		fnv_key(prefix.as_str(), entry.hops.iter().copied()),
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
type AnnounceMeta = (RouteMeta, Option<Vec<Pattern>>);

enum PendingUpdate {
	Announce(AnnounceMeta),
	Unannounce(AnnounceMeta),
	UnannounceAnnounce { old: AnnounceMeta, new: AnnounceMeta },
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
	fn apply_announce(&mut self, prefix: PathOwned, meta: RouteMeta, captures: Option<Vec<Pattern>>) {
		let meta = (meta, captures);
		let new = match self.pending.remove(&prefix) {
			// First announce, a stale announce being replaced, or a metadata update.
			None | Some(PendingUpdate::Announce(_)) => PendingUpdate::Announce(meta),
			// Consumer needs to observe the retraction before this announce.
			Some(PendingUpdate::Unannounce(old) | PendingUpdate::UnannounceAnnounce { old, .. }) => {
				PendingUpdate::UnannounceAnnounce { old, new: meta }
			}
		};
		self.pending.insert(prefix, new);
	}

	fn apply_unannounce(&mut self, prefix: PathOwned, last: RouteMeta, captures: Option<Vec<Pattern>>) {
		let last = (last, captures);
		match self.pending.remove(&prefix) {
			// The pending announce was never delivered and neither was any earlier
			// one, so the pair cancels entirely.
			Some(PendingUpdate::Announce(_)) if !self.delivered.contains(&prefix) => {}
			// Either nothing is pending or the pending announce was a metadata
			// update on a delivered route; the consumer still owes a retraction.
			None | Some(PendingUpdate::Announce(_) | PendingUpdate::Unannounce(_)) => {
				self.pending.insert(prefix, PendingUpdate::Unannounce(last));
			}
			// The embedded announce cancels with this retraction; the consumer still
			// needs the leading one.
			Some(PendingUpdate::UnannounceAnnounce { old, .. }) => {
				self.pending.insert(prefix, PendingUpdate::Unannounce(old));
			}
		}
	}

	/// Take one update to deliver to the consumer, if any.
	fn take(&mut self) -> Option<AnnounceUpdate> {
		let prefix = self.pending.keys().next()?.clone();
		let ((meta, captures), kind) = match self.pending.remove(&prefix).unwrap() {
			PendingUpdate::Announce(meta) => {
				// The consumer has seen this prefix before, so it is a metadata update.
				let kind = match self.delivered.insert(prefix.clone()) {
					true => AnnounceKind::Announced,
					false => AnnounceKind::Updated,
				};
				(meta, kind)
			}
			PendingUpdate::Unannounce(meta) => {
				self.delivered.remove(&prefix);
				(meta, AnnounceKind::Retracted)
			}
			PendingUpdate::UnannounceAnnounce { old, new } => {
				// Deliver the retraction now; leave the trailing announce pending so
				// the next take returns it for the same prefix.
				self.delivered.remove(&prefix);
				self.pending.insert(prefix.clone(), PendingUpdate::Announce(new));
				(old, AnnounceKind::Retracted)
			}
		};
		Some(AnnounceUpdate {
			prefix,
			captures,
			route: Route {
				hops: meta.0,
				cost: meta.1,
				via: Hop::UNKNOWN,
			},
			kind,
		})
	}
}

/// One announced route in the origin's table, absolute prefix.
struct RouteEntry {
	id: u64,
	prefix: PathOwned,
	/// The absolute patterns the announcing producer may serve. The prefix is
	/// only the wire-visible covering claim; this scope remains authoritative.
	scope: Patterns,
	hops: Hops,
	cost: Cost,
	/// The announcing session's declared or assigned identity. Split-horizon
	/// matches this as well as [`Self::hops`], so an anonymous hop 0 still
	/// cannot echo back to the session it came from.
	via: Hop,
	/// Whether this is an origin-owned broadcast, which wins announcement ties
	/// just as it wins exact request resolution.
	local: bool,
	/// The queue requests under this route are served from, when the announcer
	/// serves content on demand (a [`Dynamic`]). `None` for an advertise-only
	/// announcement (a broadcast's exact path, or [`Producer::announce`]), whose
	/// covered paths resolve only through the tree.
	server: Option<kio::Shared<ServeState>>,
	/// [`prefix_claim`] of [`Self::prefix`], built once at announce time.
	///
	/// The announce sync evaluates a route's claim once per (cursor, route) pair,
	/// and building one allocates a segment vector and a canonical string. Holding
	/// it makes that visit a comparison.
	claim: Pattern,
}

impl RouteEntry {
	fn is_anonymous(&self) -> bool {
		self.hops.iter().any(|hop| *hop == Hop::UNKNOWN)
	}

	/// Whether this entry may be observed or served to a requester excluding `peer`.
	///
	/// A non-zero peer is hidden when it is the announcing session (`via`) or
	/// appears in the chain. Hop 0 identifies nobody, so it is never excluded.
	fn visible_to(&self, exclude: Option<Hop>) -> bool {
		match exclude {
			Some(peer) if peer != Hop::UNKNOWN => self.via != peer && !self.hops.contains(&peer),
			_ => true,
		}
	}

	/// Whether this route and `allowed` share any path beneath the advertised prefix.
	fn overlaps(&self, allowed: &Patterns) -> bool {
		self.scope.iter().any(|scope| {
			scope
				.intersect(&self.claim)
				.is_ok_and(|scoped| scoped.iter().any(|restriction| allowed.overlaps(restriction)))
		})
	}
}

/// The paths a prefix can cover, using an exact pattern at the path depth limit.
fn prefix_claim(prefix: &Path) -> Result<Pattern, InvalidPattern> {
	if prefix.parts().count() == Path::MAX_PARTS {
		Pattern::literal(prefix.as_str())
	} else {
		Pattern::subtree(prefix.as_str())
	}
}

/// A served route's request queue: what materializes a requested path on demand.
///
/// Shared by every requester resolving through the owning route and the
/// [`Dynamic`] draining it, so both sides work under one lock.
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

/// The last route a cursor observed: entry id, metadata, servability, and captures.
type CursorRoute = (u64, RouteMeta, bool, Option<Vec<Pattern>>);

impl WeakEntry for RemoteFront {
	fn is_closed(&self) -> bool {
		self.broadcast.is_closed()
	}

	fn same_channel(&self, other: &Self) -> bool {
		self.broadcast.same_channel(&other.broadcast)
	}
}

/// One registered announce cursor: which patterns it may see, how prefixes are
/// re-rooted, and the per-cursor delivery buffer.
struct TableCursor {
	/// The prefix stripped from every delivered path.
	root: PathOwned,
	/// The absolute patterns this cursor is scoped to (its token / scope).
	allowed: Patterns,
	/// Skip routes whose hop chain or announcing session (`via`) is this peer
	/// (control-plane split horizon).
	exclude: Option<Hop>,
	/// The delivery buffer, drained by the cursor's `poll_next`.
	state: kio::Producer<OriginConsumerState>,
	/// The last delivered best route per presented (relative) prefix, for change
	/// detection: `(entry id, hops, cost)`.
	// entry id, metadata, and whether the entry could serve requests: the last
	// is part of the dedupe key (see `sync_cursor`) but never leaves the model.
	current: HashMap<PathOwned, CursorRoute>,
}

impl TableCursor {
	/// Where `prefix` presents on this cursor, named relative to the cursor root.
	/// The prefix stays a prefix; the pattern scope only decides visibility.
	/// `claim` is the prefix's [`prefix_claim`], which the caller already holds:
	/// building one allocates, and the sweeps below ask this per route per
	/// cursor.
	fn presented(&self, prefix: &Path, claim: &Pattern) -> Option<PathOwned> {
		if !self.allowed.overlaps(claim) {
			return None;
		}

		if let Some(relative) = prefix.strip_prefix(&self.root) {
			return Some(relative.to_owned());
		}
		self.root.has_prefix(prefix).then(PathOwned::default)
	}

	/// What the cursor's most specific matching scope member captures from an
	/// exact announced prefix. An overlap-only route does not pin every wildcard.
	fn captures(&self, prefix: &Path) -> Option<Vec<Pattern>> {
		let literal = Pattern::literal(prefix.as_str()).ok()?;
		self.allowed
			.iter()
			.filter_map(|allowed| {
				allowed
					.captures(&literal)
					.map(|captures| (allowed.specificity(), captures))
			})
			.max_by_key(|(specificity, _)| *specificity)
			.map(|(_, captures)| captures)
	}

	/// Whether this cursor may observe `entry` at all (split horizon).
	fn visible(&self, entry: &RouteEntry) -> bool {
		entry.visible_to(self.exclude) && entry.overlaps(&self.allowed)
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

	/// Nodes in this subtree, counting self. Test-only: pruning is invisible
	/// through the public surface, so the tests assert on the tree's size.
	#[cfg(test)]
	fn count(&self) -> usize {
		1 + self.nested.values().map(|nested| nested.lock().count()).sum::<usize>()
	}
}

/// A handle's view of an origin's path tree: the tree itself plus the absolute
/// patterns the handle may reach in it.
///
/// Paths, because pruning removes empty nodes: a pinned `Lock<OriginNode>` outlives
/// the prune as an orphan that publishes and resolves where no lookup can reach.
/// Only [`Self::tree`] is stable, so every operation resolves against it and node
/// identity lives in exactly one place. Scoping a handle therefore creates nothing
/// in the tree; only a broadcast does.
#[derive(Clone)]
struct OriginScope {
	// The tree root, shared by every handle derived from one origin. Never pruned:
	// it hangs off no parent.
	tree: Lock<OriginNode>,

	// The paths this handle may reach, absolute under `tree`.
	allowed: Patterns,
}

impl OriginScope {
	/// A view over a fresh tree that reaches nothing.
	fn empty() -> Self {
		Self {
			tree: Lock::new(OriginNode::new()),
			allowed: Patterns::new(),
		}
	}

	/// This view narrowed to the absolute `patterns`: the paths in both.
	fn narrow(&self, patterns: &Patterns) -> Option<Self> {
		let allowed = self.allowed.intersect(patterns).ok()?;
		if allowed.is_empty() {
			None
		} else {
			Some(Self {
				tree: self.tree.clone(),
				allowed,
			})
		}
	}

	/// Whether this view reaches the absolute `path`.
	fn permits(&self, path: &Path) -> bool {
		self.allowed.matches(path.as_str())
	}

	/// What this view reaches, named from `root`.
	fn relative(&self, root: &Path) -> Patterns {
		self.allowed.rebase(root.as_str())
	}
}

impl Default for OriginScope {
	fn default() -> Self {
		Self {
			tree: Lock::new(OriginNode::new()),
			allowed: Patterns::from(Pattern::all()),
		}
	}
}

/// The announce-interest prefixes that cover a pattern scope on a prefix-only
/// wire: each member's literal head, minus heads another already covers.
pub(crate) fn interest_prefixes(allowed: &Patterns) -> Vec<PathOwned> {
	let mut heads: Vec<PathOwned> = allowed
		.iter()
		.map(|pattern| Path::new(pattern.head()).to_owned())
		.collect();
	heads.sort();
	heads.dedup();
	let covered = heads.clone();
	heads.retain(|head| !covered.iter().any(|other| other != head && head.has_prefix(other)));
	heads
}

/// What an [`AnnounceUpdate`] reports about its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnounceKind {
	/// A route now covers the path; the cursor had none there.
	Announced,
	/// The route covering the path changed hops or cost; it is delivered in place.
	Updated,
	/// No route covers the path any more.
	Retracted,
}

impl AnnounceKind {
	/// Whether a route covers the path after this update.
	pub fn is_active(self) -> bool {
		!matches!(self, Self::Retracted)
	}
}

/// A route announcement, update, or retraction, delivered by [`AnnounceConsumer`].
///
/// An announcement is always a prefix, never a broadcast: it advertises that
/// [`prefix`](Self::prefix) and every path beneath it are servable. A broadcast
/// announces its own path, so the prefix usually names one, but resolve it with
/// [`Consumer::request_broadcast`]; the application decides which paths name
/// broadcasts, and filters with a [`Pattern`] locally when it wants a subset.
#[derive(Clone, Debug)]
pub struct AnnounceUpdate {
	/// The prefix the route covers, relative to the consuming cursor's root.
	pub prefix: PathOwned,
	/// What the scope's wildcards stood for when the announced prefix pins all of
	/// them. `None` for an overlap-only route or a scope without a complete match.
	pub captures: Option<Vec<Pattern>>,
	/// The route serving the prefix. On a retraction this carries its last
	/// advertised metadata.
	pub route: Route,
	/// Whether the prefix was announced, re-priced, or retracted.
	pub kind: AnnounceKind,
}

/// Publishes broadcasts and announces routes into an origin.
#[derive(Clone)]
pub struct Producer {
	// Identity for this origin. Appended to route hops when re-announcing so
	// downstream relays can detect loops and prefer the shortest path.
	hop: Hop,

	// The tree and the absolute patterns this handle may publish under.
	scope: OriginScope,

	// The prefix that is automatically stripped from all paths.
	root: PathOwned,

	// The origin's shared state: the route table, announce cursors, and the
	// remotely-served fronts. Shared with every derived consumer.
	shared: kio::Shared<OriginState>,

	// The cache pool inherited by broadcasts created under this origin (sessions
	// mint their remote broadcasts with it). Unbounded by default.
	pool: cache::Pool,

	// Retention ceiling inherited by broadcasts created under this origin (see
	// [`Config::cache_duration`]). `Duration::MAX` (no ceiling) by default.
	cache_duration: Duration,

	// Retention window for a track whose publisher advertises none (see
	// [`Config::default_max_age`]).
	default_max_age: Duration,

	// Ingress stats context. Broadcasts created through this producer are attributed
	// to it (writes counted on the subscriber/ingress side). Empty (no-op) unless a
	// session tagged this handle via [`Self::with_stats`].
	stats: stats::Session,

	// Submission handle to the origin's [`Driver`]: source watchers, fronts, and
	// serve tasks queued here run when the driver is polled. Closed once the
	// driver drops, which is what makes later mutations fail with `Closed`.
	tasks: Tasks,

	// The clock advanced by the origin driver.
	timers: Clock,
}

impl Producer {
	/// Build a producer from a [`Config`] (identity + cache pool) with no scoped
	/// prefix and no pre-existing broadcasts, paired with the [`Driver`] that runs
	/// the origin's lifecycle work.
	///
	/// Poll the driver with caller-supplied time for the origin to make progress.
	/// `moq_tokio::origin::spawn` wraps this for tokio callers.
	pub fn new(config: Config) -> (Self, Driver) {
		let (tasks, set) = TaskSet::new();
		let scope = OriginScope::default();
		let shared = kio::Shared::<OriginState>::default();
		let timers = Clock::default();
		let pool = config.pool.clone();
		let producer = Self {
			hop: config.hop,
			scope: scope.clone(),
			root: PathOwned::default(),
			shared: shared.clone(),
			pool: config.pool,
			cache_duration: config.cache_duration,
			default_max_age: config.default_max_age,
			stats: stats::Session::default(),
			tasks,
			timers: timers.clone(),
		};
		let driver = Driver {
			state: DriverState {
				set,
				tree: scope.tree,
				shared,
				done: false,
			},
			timers,
			pool,
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

	/// This origin's construction config.
	pub fn config(&self) -> Config {
		Config {
			hop: self.hop,
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			default_max_age: self.default_max_age,
		}
	}

	/// This origin's hop identity.
	pub fn hop(&self) -> Hop {
		self.hop
	}

	// The retention window for a track whose publisher advertises none (see
	// [`Config::default_max_age`]). Cheaper than `config()`, which clones the pool.
	pub(crate) fn default_max_age(&self) -> Duration {
		self.default_max_age
	}

	/// A producer with *no* allowed prefixes: it can't publish anything and
	/// advertises no subscribe interest (its `allowed()` is empty, so the
	/// subscriber issues no ANNOUNCE_PLEASE). Used to fill an unset session half
	/// so both the publisher and subscriber loops still run.
	pub(crate) fn empty(hop: Hop) -> Self {
		// No allowed prefixes means no broadcast is ever created, so nothing will
		// ever be queued on the detached submission handle.
		let (tasks, _) = TaskSet::new();
		Self {
			hop,
			scope: OriginScope::empty(),
			root: PathOwned::default(),
			shared: kio::Shared::default(),
			pool: cache::Pool::default(),
			cache_duration: Duration::MAX,
			default_max_age: track::DEFAULT_MAX_AGE,
			stats: stats::Session::default(),
			tasks,
			timers: Clock::default(),
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
	/// The broadcast starts *unadvertised*: it is reachable by exact path for
	/// subscribes and fetches. Advertise it once its tracks exist with
	/// [`broadcast::Producer::announce`] (or a whole prefix of paths with
	/// [`Self::dynamic`]); the two are independent, so cached or on-demand
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
	/// producer may publish under (after [`scope`](Self::scope)),
	/// [`Error::BoundsExceeded`] if the full rooted path exceeds
	/// [`Path::MAX_PARTS`] or holds a segment no pattern can spell (`*` or `**`),
	/// or [`Error::Closed`] once the origin's [`Driver`] has been dropped.
	pub fn create_broadcast(&self, path: impl AsPath) -> Result<broadcast::Producer, Error> {
		let path = path.as_path();

		// Held across the whole attach: the driver's teardown sets `closed` under
		// this lock, so a create either completes before the teardown (whose walk
		// then cleans the entry up) or observes `closed` here and fails.
		let mut lifecycle = self.shared.lock();
		if lifecycle.closed {
			return Err(Error::Closed);
		}

		// `get` resolves the path against the tree root, which is the same absolute
		// path the handle's own root produces: an allowed prefix is always stored
		// alongside its absolute position. So one path serves both the front's
		// identity and its position in the tree.
		let full = self.root.join(&path).to_owned();
		if !self.scope.permits(&full) {
			return Err(Error::Unauthorized);
		}
		let tree = self.scope.tree.clone();

		// A decoded prefix and suffix are each within the wire limit, but their
		// join might not be. Enforcing here bounds the tree depth and guarantees the path
		// can be re-encoded when forwarded.
		if full.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}
		// A path only a pattern could spell (a `*` segment) advertises nowhere, so
		// refuse it here rather than publish a broadcast no cursor can see.
		let claim = prefix_claim(&full).map_err(|_| BoundsExceeded)?;

		// Resolve the ingress counters once, keyed by the absolute broadcast path.
		let ingress = self.stats.ingress(&full);

		// The broadcast advertises its own exact path, already checked against the scope.
		let announcer = Announcer {
			announcing: Announcing {
				hop: self.hop,
				shared: self.shared.clone(),
				requested: full.clone(),
				prefixes: vec![(full.clone(), claim)],
				scope: self.scope.allowed.clone(),
				local: true,
				stats: self.stats.clone(),
			},
			current: None,
		};

		let source = broadcast::Info {
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			path: full.clone(),
		}
		.produce()
		.with_stats(ingress.clone())
		.with_announcer(announcer);
		let consumer = source.consume();

		// Attach synchronously: the source is visible to exact lookups before this
		// returns; only lifecycle work needs the driver.
		let origin = self.config();
		let ctx = AttachContext {
			origin: &origin,
			tree: &tree,
			full: &full,
			tasks: &self.tasks,
			timers: &self.timers,
		};
		// The root is its own leaf and is never pruned.
		let leaf = if full.is_empty() {
			tree.clone()
		} else {
			tree.lock().leaf(&full)
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
		// A local broadcast changes what the exact path resolves to, so a requester
		// parked on the route table (see `routed_broadcast`) retries.
		lifecycle.generation += 1;
		drop(lifecycle);

		Ok(source)
	}

	/// Create and advertise a broadcast in one call.
	pub fn publish(&self, path: impl AsPath, route: Route) -> Result<broadcast::Producer, Error> {
		let broadcast = self.create_broadcast(path)?;
		broadcast.announce(route)?;
		Ok(broadcast)
	}

	/// Mint a standalone source broadcast for a served-route request: it carries
	/// this origin's cache policy and ingress attribution, but
	/// is *not* inserted into the broadcast tree. Sessions answer
	/// [`Dynamic`] requests with one of these; the requester already holds
	/// the request's result channel, so the tree never needs to resolve it.
	pub(crate) fn create_source(&self, path: impl AsPath) -> broadcast::Producer {
		let path = path.as_path();
		let full = self.root.join(&path).to_owned();
		let ingress = self.stats.ingress(&full);
		broadcast::Info {
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			path: full,
		}
		.produce()
		.with_stats(ingress)
	}

	/// Advertise a route without serving it: a claim that paths under `prefix`
	/// can be served, answered by nothing.
	///
	/// A request under an advertise-only route resolves [`Error::Unroutable`]
	/// unless a local broadcast or a served route ([`Self::dynamic`]) covers the
	/// path too. Tests use it to shape the route table; everything else
	/// advertises through a broadcast ([`broadcast::Producer::announce`]) or a
	/// [`Dynamic`] handler, which serve what they claim.
	#[cfg(test)]
	pub(crate) fn announce(&self, prefix: impl AsPath, route: Route) -> Result<AnnounceProducer, Error> {
		Announcing::new(self, prefix)?.announce(route, None)
	}

	/// Advertise a route over `prefix` and serve the requests beneath it.
	///
	/// A route is always a prefix: it claims `prefix` and every path beneath it
	/// (the empty prefix claims every path). A service that only serves some of
	/// them, say `pid/*.hang`, advertises the covering prefix and refuses the
	/// rest as they are requested; consumers narrow with a [`Pattern`] locally.
	/// This is the one shape every wire carries, so a route means the same
	/// thing on every hop.
	///
	/// The advertisement is visible to [`Consumer::announced`] and forwarded by
	/// sessions for as long as the returned [`Dynamic`] (and every clone) lives.
	/// A consumer resolving a path under it that no local broadcast covers is
	/// handed to the handler as a [`Request`] to materialize on demand. This is
	/// how a service answers a whole subtree without publishing each path, and
	/// how sessions land the routes a peer announces to them; a publisher that
	/// knows its broadcasts advertises each one's exact path with
	/// [`broadcast::Producer::announce`] instead, so subscribers can enumerate
	/// them.
	///
	/// The prefix must overlap this producer's pattern scope. Individual requests
	/// remain authoritative and are refused when they do not match the scope.
	pub fn dynamic(&self, prefix: impl AsPath, route: Route) -> Result<Dynamic, Error> {
		let announcing = Announcing::new(self, prefix)?;
		let serve = kio::Shared::<ServeState>::default();
		serve.lock().requests.add_handler();
		let announcement = announcing.announce(route, Some(serve.clone()))?;
		Ok(Dynamic {
			announcement,
			state: serve,
		})
	}

	/// Returns a producer rooted at `root` and restricted to matching `patterns`.
	///
	/// `root` is relative to this producer's root, and `patterns` are relative to
	/// the new root. Returns [`Error::Unauthorized`] when the requested scope has
	/// no overlap with this producer's scope, or [`Error::BoundsExceeded`] when
	/// rooting the patterns would exceed the path limit.
	pub fn scope(&self, root: impl AsPath, patterns: &Patterns) -> Result<Producer, Error> {
		let root = self.root.join(root).to_owned();
		let rooted = patterns.rooted(root.as_str()).map_err(|_| BoundsExceeded)?;
		let scope = self.scope.narrow(&rooted).ok_or(Error::Unauthorized)?;
		Ok(Producer {
			hop: self.hop,
			scope,
			root,
			shared: self.shared.clone(),
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			default_max_age: self.default_max_age,
			stats: self.stats.clone(),
			tasks: self.tasks.clone(),
			timers: self.timers.clone(),
		})
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

	/// Returns the root that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// The patterns this producer may publish under, relative to its root.
	pub fn allowed(&self) -> Patterns {
		self.scope.relative(&self.root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}

	/// Nodes in the whole path tree, counting the root. Test-only.
	#[cfg(test)]
	pub(crate) fn node_count(&self) -> usize {
		self.scope.tree.lock().count()
	}
}

/// What it takes to insert a route: the prefixes it covers and the origin table
/// to insert them into. Built by [`Producer::announce`], [`Producer::dynamic`],
/// and [`Announcer`], which is the same advertisement re-issued from a broadcast.
struct Announcing {
	hop: Hop,
	shared: kio::Shared<OriginState>,
	/// The absolute advertised prefix, which also keys the ingress announce counters.
	requested: PathOwned,
	/// The prefix inserted into the table, with its [`prefix_claim`]. Pattern
	/// scopes decide visibility and request authorization without changing the
	/// route's prefix shape.
	prefixes: Vec<(PathOwned, Pattern)>,
	/// The absolute paths the producer is authorized to serve.
	scope: Patterns,
	local: bool,
	stats: stats::Session,
}

impl Announcing {
	/// The requested prefix as-is, refused when its subtree does not overlap the scope.
	fn new(producer: &Producer, prefix: impl AsPath) -> Result<Self, Error> {
		let requested = producer.root.join(prefix.as_path()).to_owned();
		if requested.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}
		let claim = prefix_claim(&requested).map_err(|_| BoundsExceeded)?;
		if !producer.scope.allowed.overlaps(&claim) {
			return Err(Error::Unauthorized);
		}
		Ok(Self {
			hop: producer.hop,
			shared: producer.shared.clone(),
			requested: requested.clone(),
			prefixes: vec![(requested, claim)],
			scope: producer.scope.allowed.clone(),
			local: false,
			stats: producer.stats.clone(),
		})
	}

	fn announce(&self, route: Route, server: Option<kio::Shared<ServeState>>) -> Result<AnnounceProducer, Error> {
		debug_assert!(
			!route.hops.contains(&self.hop),
			"announce called with a looping hop chain",
		);

		let via = route.via;
		let meta: RouteMeta = (route.hops, route.cost);

		let mut shared = self.shared.lock();
		if shared.closed {
			return Err(Error::Closed);
		}

		let mut ids = Vec::with_capacity(self.prefixes.len());
		for (prefix, claim) in &self.prefixes {
			let id = shared.next_route;
			shared.next_route += 1;
			shared.generation += 1;
			shared.routes.push(RouteEntry {
				id,
				prefix: prefix.clone(),
				scope: self.scope.clone(),
				hops: meta.0.clone(),
				cost: meta.1,
				via,
				local: self.local,
				server: server.clone(),
				claim: claim.clone(),
			});
			shared.sync_route(prefix, claim);
			ids.push(id);
		}
		drop(shared);

		// Ingress announce guard: held for the advertisement's lifetime.
		let guard = self.stats.ingress(&self.requested).announce();

		Ok(AnnounceProducer {
			shared: self.shared.clone(),
			ids,
			_guard: guard,
		})
	}
}

/// The advertisement a broadcast owns: its exact path, announced and retracted
/// through [`broadcast::Producer::announce`] and [`broadcast::Producer::unannounce`],
/// and retracted for good when the broadcast ends.
///
/// Handed to the broadcast by [`Producer::create_broadcast`], so a standalone
/// broadcast has none and cannot announce.
pub(crate) struct Announcer {
	announcing: Announcing,
	current: Option<AnnounceProducer>,
}

impl Announcer {
	/// Advertise the broadcast's path with `route`, or re-price the standing
	/// advertisement in place.
	pub(crate) fn announce(&mut self, route: Route) -> Result<(), Error> {
		if let Some(current) = &self.current {
			return current.update(route);
		}
		self.current = Some(self.announcing.announce(route, None)?);
		Ok(())
	}

	/// Take the standing advertisement, if any, so the caller retracts it by
	/// dropping it outside whatever lock guards this announcer.
	pub(crate) fn take(&mut self) -> Option<AnnounceProducer> {
		self.current.take()
	}
}

/// The write half of an advertisement: a live claim that paths under a
/// [`Pattern`] can be served.
///
/// Held by a [`Dynamic`] and by a broadcast's [`Announcer`]; dropping it
/// retracts the route, which [`AnnounceConsumer`]s observe and sessions withdraw
/// from their peers.
#[must_use = "dropping an announcement retracts the route"]
pub(crate) struct AnnounceProducer {
	shared: kio::Shared<OriginState>,
	/// The table entries this advertisement created. A prefix remains unchanged;
	/// pattern scopes only filter its visibility and requests.
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
			// Each entry keeps its advertised prefix; only the metadata moves.
			let Some(entry) = shared.routes.iter_mut().find(|entry| entry.id == *id) else {
				continue;
			};
			entry.hops = route.hops.clone();
			entry.cost = route.cost;
			entry.via = route.via;
			let (prefix, claim) = (entry.prefix.clone(), entry.claim.clone());
			shared.generation += 1;
			shared.sync_route(&prefix, &claim);
		}
		Ok(())
	}

	/// Retract the route now: remove its table entries and reject anything still
	/// waiting on its queue. Idempotent, and what dropping the advertisement does.
	fn retract(&self) {
		let mut shared = self.shared.lock();
		for id in &self.ids {
			let Some(index) = shared.routes.iter().position(|entry| entry.id == *id) else {
				continue;
			};
			let entry = shared.routes.swap_remove(index);
			shared.generation += 1;
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
			shared.sync_route(&entry.prefix, &entry.claim);
		}
	}
}

impl Drop for AnnounceProducer {
	fn drop(&mut self) {
		self.retract();
	}
}

/// Drives origin lifecycle work and cache expiration with caller-supplied time.
///
/// Returned by [`Producer::new`]. Poll on external activity or at the deadline
/// it returns, supplying nondecreasing instants. Route changes, track serving, linger,
/// failover, and teardown run here; exact lookups and eligible announcements
/// update synchronously in [`Producer::create_broadcast`].
///
/// It holds no [`Producer`] clone, so it never keeps the origin alive. Dropping
/// it aborts active fronts, rejects pending requests, ends announcements, and
/// makes subsequent producer mutations fail with [`Error::Closed`].
/// `moq_tokio::origin::spawn` handles construction and driving for Tokio callers.
#[must_use = "poll the driver or the origin makes no progress"]
pub struct Driver {
	state: DriverState,
	// Shared by this origin's lifecycle tasks; advanced only when polled.
	timers: Clock,
	// The cache pool this origin's groups charge into, swept on a wall-clock
	// cadence so its idle window binds a track whose publisher stopped writing.
	pool: cache::Pool,
}

/// Lifecycle work and the state it tears down.
struct DriverState {
	/// Source watchers, fronts, and serve tasks: producers submit, this polls.
	set: TaskSet,
	/// The whole broadcast tree, for the teardown walk on drop.
	tree: Lock<OriginNode>,
	/// The route table, announce cursors, and the remotely-served fronts, for
	/// ending everything on drop.
	shared: kio::Shared<OriginState>,
	/// Cached completion so a poll after `Ready` doesn't re-poll the drained set.
	done: bool,
}

impl Driver {
	/// Process ready origin work using caller-supplied monotonic time.
	///
	/// See [`crate::time::Driver`] for the contract. Finishes with
	/// [`Error::Closed`] once every producer handle has dropped and the
	/// remaining lifecycle work has drained.
	pub fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Result<Option<Instant>, Error> {
		self.timers.advance(now);
		let result = self.state.poll(waiter);
		let gc = self.pool.gc(now);
		if result.is_ready() {
			return Err(Error::Closed);
		}
		Ok(self.timers.timeout().into_iter().chain(gc).min())
	}
}

impl crate::time::Driver for Driver {
	fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Result<Option<Instant>, Error> {
		self.poll(now, waiter)
	}
}

impl DriverState {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Never gates completion: the pool outlives this origin (a relay shares one
		// across every origin), so a sweep that is still due must not keep the driver
		// alive after its lifecycle work has drained.
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
		let (servers, cursors, fronts) = {
			let mut shared = self.shared.lock();
			shared.closed = true;
			let servers: Vec<_> = shared.routes.iter().filter_map(|entry| entry.server.clone()).collect();
			let cursors: Vec<_> = shared.cursors.values().map(|cursor| cursor.state.clone()).collect();
			let fronts: Vec<_> = shared.fronts.values().map(|front| front.request.clone()).collect();
			(servers, cursors, fronts)
		};

		// Reject requesters still parked on a remote front's channel: its watcher
		// was cancelled above and will never resolve them.
		for producer in fronts {
			if let Ok(mut request) = producer.write() {
				request.resolved.get_or_insert(Err(Error::Dropped));
			}
		}
		// Reject every pending route request, including those already handed to a
		// handler: the teardown is terminal, so a handler resolving late must not
		// beat it (resolution is first-write-wins).
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

		teardown_broadcasts(&self.tree);
	}
}

impl Drop for DriverState {
	fn drop(&mut self) {
		self.teardown();
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
	if let Some(entry) = entry {
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
/// fetches, reads the groups the front already cached: no second round trip for
/// `TRACK_INFO`. Groups past that cached edge cost a fresh source splice. After
/// the window, the cached segment is released.
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
	/// Immutable track metadata, retained across idle release and aborted attempts.
	/// Every source of this broadcast must serve the same content.
	track_info: HashMap<Arc<str>, track::Info>,
	/// The source tracks are dispatched to: the newest attached. Backups park
	/// until promoted.
	active: Option<u64>,
	/// Terminal: no more sources may attach and every poller stops. Set
	/// synchronously by the detach that empties the table.
	closed: bool,
}

impl FrontState {
	/// Admit only copies with the broadcast's established track properties.
	fn accept_track_info(&mut self, name: &Arc<str>, info: track::Info) -> Result<(), Error> {
		if self.closed {
			return Err(Error::Closed);
		}
		if let Some(expected) = self.track_info.get(name) {
			let track::Info {
				timescale,
				max_age,
				priority,
			} = info;
			if timescale != expected.timescale || max_age != expected.max_age || priority != expected.priority {
				return Err(Error::Unsupported);
			}
		} else {
			self.track_info.insert(name.clone(), info);
		}
		Ok(())
	}

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
	timers: Clock,
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
	origin: &'a Config,
	/// The origin's tree root: the one node pruning never removes, so the leaf is
	/// resolved from here rather than pinned by a handle that a prune can orphan.
	tree: &'a Lock<OriginNode>,
	/// Absolute path: the front's identity, its log lines, and its position under
	/// `tree` are all the same path.
	full: &'a PathOwned,
	/// Driver submission handle, for queueing a fresh front's task.
	tasks: &'a Tasks,
	/// The driver's clock, threaded into fronts for the track idle linger.
	timers: &'a Clock,
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
		pool: ctx.origin.pool.clone(),
		cache_duration: ctx.origin.cache_duration,
		path: ctx.full.clone(),
	});
	let state = kio::Producer::new(FrontState {
		next_source: 1,
		sources: vec![FrontSource {
			id: 0,
			source: source.clone(),
		}],
		track_info: HashMap::new(),
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
		ctx.tree.clone(),
		ctx.full.clone(),
		ctx.tasks.clone(),
		ctx.timers.clone(),
	));

	(state, broadcast, 0)
}

/// Owns a front's lifecycle: dispatches each requested track to a serve task
/// until the last source detaches, then unpublishes the broadcast.
async fn run_front(
	state: kio::Producer<FrontState>,
	broadcast: broadcast::Producer,
	tree: Lock<OriginNode>,
	full: PathOwned,
	tasks: Tasks,
	slot: Clock,
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
	tree.lock().remove(&state, &full);
}

/// A local copy of groups the front already delivered, so resume stays spliced
/// after the source track is dropped. Cache misses stay pending while demand
/// re-splices the upstream source. Finished on drop so an idle linger does not
/// warn about an abandoned producer.
struct WarmCopy {
	track: track::Producer,
	_dynamic: track::Dynamic,
}

impl Drop for WarmCopy {
	fn drop(&mut self) {
		let _ = self.track.finish();
	}
}

/// Cache `source`'s groups on a new local track the origin owns.
fn warm_copy(source: &track::Consumer) -> Option<WarmCopy> {
	let info = source.cached_info()?;
	let mut track = track::Producer::new(Arc::new(source.broadcast().clone()), source.name(), info);
	for (group, visible) in source.cached_groups() {
		let _ = track.adopt_group(group, visible);
	}
	let dynamic = track.dynamic();
	Some(WarmCopy {
		track,
		_dynamic: dynamic,
	})
}

/// Serves one spliced logical track: splices in the best source's copy of the
/// track, re-splicing on handover or failure, until the track completes or the
/// front closes. A refusal (a source rejecting the track, returning incompatible
/// metadata, or dying before delivering anything) is authoritative and never
/// retried: the refuser is skipped for this track so a joining standby cannot kill a subscription
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
	slot: Clock,
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
	// Local cache of groups the front already delivered, held after the source
	// copy is dropped so resume stays spliced for the linger without pinning
	// the source as a reader. Cache misses wait for demand to re-splice the
	// source, and the copy is finished on drop so an idle linger is quiet.
	let mut warm: Option<WarmCopy> = None;
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
	// Only the driver polls this body, after advancing the shared clock.
	let timers = slot.clone();
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
		// The source copy is dropped on the unused edge, not at the linger: holding
		// it would count the front as a reader for 30s after the real one left.
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
			// Drop the source copy on the unused edge so its producer goes idle
			// immediately; keep the groups it already delivered on a local track
			// so resume stays spliced until the linger expires. A source with no
			// snapshot (a spliced copy from another front) still releases its
			// segment: the next reader re-splices, paying `TRACK_INFO` again.
			Step::Demand => {
				if !resume.is_used()
					&& let Some((_, source)) = serving.take()
				{
					drop(warm.take());
					let local = warm_copy(&source);
					drop(source);
					if resume.release().is_err() {
						return;
					}
					if let Some(local) = local {
						if let Err(err) = resume.takeover(&local.track) {
							let _ = resume.abort(err);
							return;
						}
						warm = Some(local);
					}
				}
			}
			// Forget which route we were serving from, or the `gone` edge that woke
			// us keeps firing: the id stays absent from the table, the wait returns
			// Ready at once, and the loop spins on a full core without ever parking.
			// The segment itself stays spliced into `resume` (readers keep whatever
			// it delivered) until a replacement is proven servable.
			Step::NoRoute => serving = None,
			Step::Idle => {
				// Nobody has read the track for the linger: release the cached
				// segment. The source's copy was already dropped on the unused
				// edge. The logical track stays alive and re-splices on the next
				// reader.
				if resume.release().is_err() {
					// Finished or aborted meanwhile; the track is over either way.
					return;
				}
				serving = None;
				drop(warm.take());
			}
			Step::Splice(id, source) => {
				// Ask the source for its copy and wait for the info to resolve,
				// proving it servable, before splicing it in. Bail out early if
				// the table moves on while waiting.
				let attempt = match source.track(&name) {
					Ok(track) => {
						// `into_inner` sheds the `Pending` future wrapper so only
						// the pollable (which is `Sync`) is held across the await.
						let query = track.query().into_inner();
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
							// One that claims the same content with different
							// metadata is refused rather than reinterpreted.
							Some(Ok(info)) => match track.poll_complete(&kio::Waiter::noop()) {
								Poll::Ready(Err(err)) => Err(err),
								_ => match state.write() {
									Ok(mut state) => state.accept_track_info(&name, info).map(|()| track),
									Err(_) => Err(Error::Dropped),
								},
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
	timers: Clock,
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
					// A retraction and a handler's rejection resolve alike, so the
					// table tells them apart: an `Unroutable` from a route that
					// still stands is the handler's answer, and re-asking it
					// would spin forever.
					Err(Error::Unroutable) if !shared.lock().routes.iter().any(|entry| entry.id == route) => {
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
/// it, and the remotely-served fronts.
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
	// Bumped on every change to what a path resolves to: a route inserted,
	// re-priced, or retracted, and a local broadcast attached. A requester waits
	// on it rather than re-asking a table that has not moved.
	generation: u64,

	// The registered announce cursors, each with its own coalescing buffer.
	cursors: HashMap<ConsumerId, TableCursor>,

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
	/// `claim` is `prefix`'s [`prefix_claim`], held by the entry that changed.
	fn sync_route(&mut self, prefix: &Path, claim: &Pattern) {
		// Split borrows: the recompute reads `routes` while mutating a cursor.
		let routes = &self.routes;
		for cursor in self.cursors.values_mut() {
			if let Some(presented) = cursor.presented(prefix, claim) {
				Self::sync_cursor(routes, cursor, &presented);
			}
		}
	}

	/// Recompute the best visible route presenting at `presented` (relative) for
	/// one cursor and deliver the change, if any.
	fn sync_cursor(routes: &[RouteEntry], cursor: &mut TableCursor, presented: &PathOwned) {
		// Among entries presenting here, the longest prefix wins outright, so the
		// metadata a cursor advertises matches what a request through it actually
		// resolves.
		let candidates: Vec<&RouteEntry> = routes
			.iter()
			.filter(|entry| cursor.visible(entry))
			.filter(|entry| cursor.presented(&entry.prefix, &entry.claim).as_ref() == Some(presented))
			.collect();
		let most = candidates.iter().map(|entry| entry.prefix.len()).max();
		let best = most.and_then(|most| {
			candidates
				.into_iter()
				.filter(|entry| entry.prefix.len() == most)
				.min_by_key(|entry| (!entry.local, route_order(&entry.prefix, entry)))
		});

		match best {
			Some(entry) => {
				let meta = (entry.hops.clone(), entry.cost);
				let served = entry.server.is_some();
				let captures = cursor.captures(&entry.prefix);
				let previous = cursor
					.current
					.insert(presented.clone(), (entry.id, meta.clone(), served, captures.clone()));
				match previous {
					// Unchanged metadata and servability: nothing the consumer could
					// act on, even if the winning entry itself changed (a reconnect
					// under an identical route is invisible, which is the point). A
					// servability flip is delivered: a request that failed Unroutable
					// under an advertise-only route retries on the update, and hiding
					// it would park that waiter forever.
					Some((_, prev, prev_served, prev_captures))
						if prev == meta && prev_served == served && prev_captures == captures => {}
					// Captures are consumer identity, not route metadata. Replace the
					// old identity explicitly so capture-keyed consumers can remove it.
					Some((_, prev, _, prev_captures)) if prev_captures != captures => {
						if let Ok(mut state) = cursor.state.write() {
							state.apply_unannounce(presented.clone(), prev, prev_captures);
							state.apply_announce(presented.clone(), meta, captures);
						}
					}
					_ => {
						if let Ok(mut state) = cursor.state.write() {
							state.apply_announce(presented.clone(), meta, captures);
						}
					}
				}
			}
			None => {
				if let Some((_, last, _, captures)) = cursor.current.remove(presented)
					&& let Ok(mut state) = cursor.state.write()
				{
					state.apply_unannounce(presented.clone(), last, captures);
				}
			}
		}
	}

	/// Register a cursor and replay the current best route per presented prefix.
	fn register_cursor(&mut self, id: ConsumerId, mut cursor: TableCursor) {
		let routes = &self.routes;
		let mut presented: Vec<PathOwned> = Vec::new();
		for entry in routes {
			if let Some(p) = cursor.presented(&entry.prefix, &entry.claim)
				&& !presented.contains(&p)
			{
				presented.push(p);
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
	/// announcement shadows a broad served one: requests under it resolve
	/// unroutable instead of being routed around it. Among routes at the winning
	/// prefix, the cheapest served one is picked by [`route_order`].
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
			.filter(|entry| entry.scope.matches(path.as_str()))
			.filter(|entry| entry.visible_to(exclude))
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
			.min_by_key(|entry| route_order(&entry.prefix, entry))
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

/// A served route, from [`Producer::dynamic`]: advertises a path prefix and
/// answers the [`Consumer::request_broadcast`] calls beneath it.
///
/// The origin-level analogue of [`broadcast::Dynamic`]: where that serves tracks
/// on demand within a broadcast, this serves whole broadcasts on demand within
/// an origin. A relay holds one per route a peer announces to it, materializing
/// a requested path from that peer; an application holds one to answer a
/// subtree it never publishes ahead of time.
///
/// Drop it to retract the route and reject the requests still waiting to be
/// served; [`update`](Self::update) re-prices it in place.
#[must_use = "dropping an origin::Dynamic retracts the route"]
pub struct Dynamic {
	/// The advertisement, retracted on drop.
	announcement: AnnounceProducer,
	state: kio::Shared<ServeState>,
}

impl Dynamic {
	/// Re-price the route in place: replace its hops and cost.
	///
	/// Consumers observe another active update for the same prefix; sessions
	/// forward it as a restart, so route churn never looks like new content. The
	/// prefix is fixed at announce time: to move a route, drop this and call
	/// [`Producer::dynamic`] again. Fails with [`Error::Closed`] once the origin's
	/// [`Driver`] has been dropped.
	pub fn update(&self, route: Route) -> Result<(), Error> {
		self.announcement.update(route)
	}

	/// Poll for the next requested path under this route, without blocking.
	///
	/// Returns [`Error::Closed`] once the origin's [`Driver`] has been dropped:
	/// no request will ever arrive again, so handler loops should end.
	pub fn poll_requested_broadcast(&self, waiter: &kio::Waiter) -> Poll<Result<Request, Error>> {
		let mut state = ready!(self.state.poll(waiter, |state| {
			if state.closed || state.requests.has_queued() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));

		// The teardown already drained the queue, so there is nothing left to pop.
		if state.closed {
			return Poll::Ready(Err(Error::Closed));
		}

		let path = state.requests.pop().expect("predicate guaranteed a request");
		// The popped request stays pending, so a repeat request in the window between
		// hand-off and accept coalesces onto it instead of re-invoking the handler. The
		// producer is a shared clone; `Request::{accept, reject, drop}` removes the
		// entry. This mirrors how `poll_requested_track` keeps a served track
		// discoverable via the weak cache across the same window.
		let producer = state.requests.get(&path).expect("popped key must be pending").clone();
		Poll::Ready(Ok(Request {
			path,
			producer,
			home: self.state.clone(),
		}))
	}

	/// Block until a consumer requests a path under this route, returning a
	/// [`Request`] to serve.
	///
	/// Takes `&self` so a handler can serve from one task while another re-prices
	/// the route; concurrent callers each receive distinct requests.
	pub async fn requested_broadcast(&self) -> Result<Request, Error> {
		kio::wait(|waiter| self.poll_requested_broadcast(waiter)).await
	}
}

impl ServeState {
	/// Resolve a pending request: cache an accepted broadcast for repeat
	/// requests, remove the queue entry, and wake the requesters.
	///
	/// Resolved while the queue's lock is held, so this linearizes with the
	/// teardown: either the teardown ran first (the `closed` check returns, its
	/// rejection stands) or this write lands first and the teardown finds the
	/// entry already gone. The queue lock is released before the channel guard
	/// drops, so the requester wakes outside it: an inline executor re-entering
	/// `request_broadcast` from the wake must not find the non-reentrant lock
	/// still held.
	fn resolve(
		shared: &kio::Shared<Self>,
		path: &PathOwned,
		producer: &kio::Producer<PendingBroadcast>,
		result: Result<broadcast::Consumer, Error>,
	) {
		let mut state = shared.lock();
		if state.closed {
			return;
		}
		let resolved = match result {
			Ok(broadcast) => {
				// If a live broadcast was already served for this path while we were
				// fetching upstream, dedup onto it and drop ours rather than replace
				// a good entry with a duplicate subscription.
				let existing = state.served.insert(path.clone(), broadcast.weak());
				Ok(existing.map(|weak| weak.consume()).unwrap_or(broadcast))
			}
			Err(err) => Err(err),
		};
		state.requests.remove_if(path, |p| p.same_channel(producer));
		if let Ok(mut pending) = producer.write() {
			pending.resolved.get_or_insert(resolved);
			drop(state);
		}
	}

	/// Drop the still-pending entry, if it is still ours.
	fn forget(shared: &kio::Shared<Self>, path: &PathOwned, producer: &kio::Producer<PendingBroadcast>) {
		shared.lock().requests.remove_if(path, |p| p.same_channel(producer));
	}
}

/// A pending request for a broadcast to be served on demand.
///
/// Yielded by [`Dynamic::requested_broadcast`]. The requester is awaiting inside
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
	home: kio::Shared<ServeState>,
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
		ServeState::resolve(&self.home, &self.path, &self.producer, Ok(broadcast));
		// `self.producer` drops here, closing the channel; the value is still observable.
	}

	/// Reject the request, resolving every awaiting requester with `err`.
	pub fn reject(self, err: Error) {
		ServeState::resolve(&self.home, &self.path, &self.producer, Err(err));
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
		ServeState::forget(&self.home, &self.path, &self.producer);
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
	// The route table's generation when the request was made, so a retry can
	// wait for the table to move rather than re-ask the same routes.
	generation: u64,
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

	fn queued(consumer: kio::Consumer<PendingBroadcast>) -> Self {
		Self::new(RequestState::Pending(consumer))
	}

	/// Whether the request was handed to a serving route, rather than decided on
	/// the spot.
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
			generation: 0,
		}
	}

	fn with_path(mut self, path: PathOwned) -> Self {
		self.path = path;
		self
	}

	fn with_generation(mut self, generation: u64) -> Self {
		self.generation = generation;
		self
	}

	/// The route table's generation when the request was made.
	fn generation(&self) -> u64 {
		self.generation
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
	hop: Hop,
	scope: OriginScope,

	// A prefix that is automatically stripped from all paths.
	root: PathOwned,

	// The origin's shared state: the route table, announce cursors, and the
	// remotely-served fronts.
	shared: kio::Shared<OriginState>,

	// Egress stats context. Broadcasts handed out through this consumer (and any
	// handle derived from them) are attributed to it (reads counted on the
	// publisher/egress side). Empty (no-op) unless a session tagged this handle.
	stats: stats::Session,

	// Split horizon: routes whose hop chain or announcing session (`via`) is this
	// peer are invisible to `announced` and skipped by `request_broadcast`, so a
	// peer is never served (or advertised) its own content back. `None` (the
	// default) filters nothing.
	exclude: Option<Hop>,

	// The cache policy remote fronts inherit, mirroring what
	// `create_broadcast` gives a local front.
	pool: cache::Pool,
	cache_duration: Duration,

	// Non-owning submission handle to the origin's [`Driver`], for the front
	// watcher a routed `request_broadcast` spawns. Non-owning so a lingering
	// read handle never keeps the driver from finishing.
	tasks: TasksWeak,

	// The driver's clock and timers, threaded into fronts for the track idle
	// linger.
	timers: Clock,
}

impl Consumer {
	fn from_producer(producer: &Producer, stats: stats::Session) -> Self {
		Self {
			hop: producer.hop,
			scope: producer.scope.clone(),
			root: producer.root.clone(),
			shared: producer.shared.clone(),
			stats,
			exclude: None,
			pool: producer.pool.clone(),
			cache_duration: producer.cache_duration,
			tasks: producer.tasks.downgrade(),
			timers: producer.timers.clone(),
		}
	}

	/// This origin's hop identity.
	pub fn hop(&self) -> Hop {
		self.hop
	}

	/// A clone that never serves the given peer its own data: routes whose hop
	/// chain contains `peer`, or whose announcing session is `peer`, are invisible
	/// and never resolved from, matching what the announce loop advertises to them.
	/// Sessions apply this once they learn the peer's origin id. Hop 0 identifies
	/// nobody, so the announcing session's assigned identity is what keeps an
	/// anonymous route from echoing back.
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
			scope: OriginScope::empty(),
			..self.clone()
		}
	}

	/// Subscribe to route announcements for this consumer's scope.
	///
	/// Allocates a per-cursor coalescing buffer and replays the currently
	/// announced routes as initial updates. Routes stay prefixes and are named
	/// relative to this consumer's root; its patterns only filter visibility.
	/// Drop the returned [`AnnounceConsumer`] to unregister.
	pub fn announced(&self) -> AnnounceConsumer {
		AnnounceConsumer::new(
			self.root.clone(),
			self.scope.allowed.clone(),
			self.stats.clone(),
			self.exclude,
			&self.shared,
		)
	}

	/// Returns a cheap duplicate of this read handle.
	pub fn consume(&self) -> Self {
		self.clone()
	}

	/// Internal synchronous lookup: the local broadcast at `path`, if any.
	fn resolve(&self, path: impl AsPath) -> Option<broadcast::Consumer> {
		let full = self.root.join(path).to_owned();
		if !self.scope.permits(&full) {
			return None;
		}
		self.scope.tree.lock().resolve_broadcast(&full)
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

		// Scope a fresh consumer down to this path's subtree, so we only wake for
		// announcements that overlap the requested path.
		// A max-depth path cannot be spelled as `path/**` (`**` would be a 33rd
		// segment), so watch the existing stream and match covering claims instead.
		let consumer = match Pattern::subtree(path.as_str()) {
			Ok(subtree) => self.scope("", &Patterns::from(subtree)).ok()?,
			Err(InvalidPattern::TooManySegments) => self.clone(),
			Err(_) => return None,
		};

		// `scope` keeps narrower permissions intact: if we ask for `foo` on a
		// consumer limited to `foo/specific`, `foo` itself is unauthorized. Bail
		// rather than loop forever.
		if !consumer.allowed().matches(path.as_str()) {
			return None;
		}

		// Use an untagged stream: this is a lookup, not egress announce
		// forwarding, so it must not drive the announce guards.
		let mut announced = consumer.untagged().announced();
		loop {
			let update = announced.next().await?;
			if update.kind.is_active() && path.has_prefix(&update.prefix) {
				return Some(update.route);
			}
		}
	}

	/// Block until `path` resolves to a broadcast: [`Self::routed`], then
	/// [`Self::request_broadcast`], retried when the two race.
	///
	/// The wait and the resolution are separate steps, so the covering route can
	/// retract between them (failover churn), a route can cover the path while
	/// nothing serves it yet (an advertise-only announce racing its handler), and
	/// a handler can turn the path down. This rides out the churn by retrying
	/// whenever the route table moves, which is what makes it the right call for
	/// resolving a path right after connecting. Returns [`Error::Unauthorized`]
	/// for a path outside this consumer's scope, [`Error::Closed`] once the origin
	/// closes, and any other resolution failure as-is.
	pub async fn routed_broadcast(&self, path: impl AsPath) -> Result<broadcast::Consumer, Error> {
		let path = path.as_path();

		// `allowed` keeps narrower permissions intact: if the whole path is not
		// reachable, no route can ever cover it, so bail rather than loop forever.
		if !self.allowed().matches(path.as_str()) {
			return Err(Error::Unauthorized);
		}
		loop {
			if self.routed(&path).await.is_none() {
				return Err(Error::Closed);
			}
			let request = self.request_broadcast(&path);
			// `Unroutable` is a verdict of the table as it stood when the request
			// was made: nothing covered the path, the serving route retracted
			// under the request, or its handler declined. Re-asking the same
			// table would spin, so wait for it to move (an identical standby
			// swapping in counts, even though no announce update reports it,
			// and so does a local broadcast attaching at the path) and try
			// again. A retraction that already happened bumped the generation
			// before the error was observed, so that retry is immediate.
			let seen = request.generation();
			match request.await {
				Ok(broadcast) => return Ok(broadcast),
				Err(Error::Unroutable) => {
					let closed = kio::wait(|waiter| {
						self.shared
							.poll(waiter, |table| match table.closed || table.generation != seen {
								true => Poll::Ready(()),
								false => Poll::Pending,
							})
							.map(|table| table.closed)
					})
					.await;
					if closed {
						return Err(Error::Closed);
					}
				}
				Err(err) => return Err(err),
			}
		}
	}

	/// Returns a consumer rooted at `root` and restricted to matching `patterns`.
	///
	/// `root` is relative to this consumer's root, and `patterns` are relative to
	/// the new root. Returns [`Error::Unauthorized`] when the requested scope has
	/// no overlap with this consumer's scope, or [`Error::BoundsExceeded`] when
	/// rooting the patterns would exceed the path limit.
	pub fn scope(&self, root: impl AsPath, patterns: &Patterns) -> Result<Consumer, Error> {
		let root = self.root.join(root).to_owned();
		let rooted = patterns.rooted(root.as_str()).map_err(|_| BoundsExceeded)?;
		let scope = self.scope.narrow(&rooted).ok_or(Error::Unauthorized)?;
		Ok(Consumer {
			scope,
			root,
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
	///
	/// The returned future resolves to [`Error::Unroutable`] when neither exists.
	/// A route claims capability, not inventory: resolving a covered path
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

		// Routes only cover paths within this consumer's scope.
		if !self.scope.permits(&absolute) {
			return kio::Pending::new(Requesting::failed(Error::Unauthorized));
		}

		let mut state = self.shared.lock();

		// The origin's driver dropped: nothing will ever serve this.
		if state.closed {
			return kio::Pending::new(Requesting::failed(Error::Closed));
		}

		// Join the live front for this path and exclusion, if any: its watcher
		// resolves (or already resolved) the request channel with the front's
		// spliced broadcast, so repeat requests share one upstream
		// subscription. A front whose route has since retracted still serves
		// for as long as its session does.
		let key = (absolute.clone(), self.exclude);
		if let Some(front) = state.fronts.get(&key) {
			let pending = Requesting::queued(front.request.consume())
				.with_path(requested)
				.with_stats(scope)
				.with_generation(state.generation);
			return kio::Pending::new(pending);
		}

		// Nothing serves the path: no local broadcast and no served route.
		if state
			.best_route(&absolute.as_path(), self.exclude, None, &HashSet::new())
			.is_none()
		{
			return kio::Pending::new(Requesting::failed(Error::Unroutable));
		}

		// A route covers the path: mint the front and hand its watcher the
		// request. The watcher materializes the path from the best covering
		// route, resolves the channel, and re-splices the front through
		// routes sharing its first hop for as long as one serves.
		let broadcast = broadcast::Producer::new_spliced(broadcast::Info {
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			path: absolute.clone(),
		});
		let front_state = kio::Producer::new(FrontState {
			next_source: 0,
			sources: Vec::new(),
			track_info: HashMap::new(),
			active: None,
			closed: false,
		});
		let request = kio::Producer::<PendingBroadcast>::default();
		let consumer = request.consume();
		let generation = state.generation;
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
		kio::Pending::new(
			Requesting::queued(consumer)
				.with_path(requested)
				.with_stats(scope)
				.with_generation(generation),
		)
	}

	/// Returns the prefix that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// The patterns this consumer may reach, relative to its root.
	pub fn allowed(&self) -> Patterns {
		self.scope.relative(&self.root)
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
	// opens one (bumping `announces_started` + `announced_bytes`); the matching retraction
	// drops it (bumping `announces_ended` + `announced_bytes`).
	guards: HashMap<PathOwned, stats::Announce>,

	// Holds the waiter a `Stream` poll registered; disjoint from `state` so the
	// borrow never collides with the body's.
	park: kio::Park,
}

impl AnnounceConsumer {
	fn new(
		root: PathOwned,
		allowed: Patterns,
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
			park: kio::Park::default(),
		}
	}

	/// Drive the egress announce guards for one update.
	fn hand_out(&mut self, update: AnnounceUpdate) -> AnnounceUpdate {
		let absolute = self.root.join(&update.prefix).to_owned();
		if update.kind.is_active() {
			let scope = self.stats.egress(&absolute);
			self.guards
				.entry(update.prefix.clone())
				.or_insert_with(|| scope.announce());
		} else {
			self.guards.remove(&update.prefix);
		}
		update
	}

	/// Returns the next route announcement, update, or retraction, its prefix
	/// relative to this cursor's root.
	///
	/// A retraction is only delivered for a previously announced prefix, and a
	/// repeated announcement for the same prefix is a metadata update. Returns
	/// None if the cursor is closed. The consumer is also a [`futures::Stream`]
	/// of the same updates.
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

	/// Returns the root that is automatically stripped from emitted prefixes.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// Converts an emitted prefix back to one rooted at the origin.
	pub fn absolute(&self, prefix: impl AsPath) -> Path<'_> {
		self.root.join(prefix)
	}
}

impl futures::Stream for AnnounceConsumer {
	type Item = AnnounceUpdate;

	fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
		let this = self.get_mut();
		let waiter = this.park.hold(cx).clone();
		this.poll_next(&waiter)
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
		assert_eq!(update.prefix, expected, "wrong prefix");
		assert!(update.kind.is_active(), "should be an active route");
		update.route
	}

	/// The `try_next` counterpart of [`Self::assert_next_active`].
	pub fn assert_try_next_active(&mut self, expected: impl AsPath) -> Route {
		let expected = expected.as_path();
		let update = self.try_next().expect("no next");
		assert_eq!(update.prefix, expected, "wrong prefix");
		assert!(update.kind.is_active(), "should be an active route");
		update.route
	}

	/// The next update must be a retraction at `expected`.
	pub fn assert_next_ended(&mut self, expected: impl AsPath) {
		let expected = expected.as_path();
		let update = self.next().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(update.prefix, expected, "wrong prefix");
		assert_eq!(update.kind, AnnounceKind::Retracted, "should be a retraction");
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
impl ProduceTest for Config {
	fn produce(self) -> Producer {
		let (producer, driver) = Producer::new(self);
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(crate::time::run(driver));
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
		Config::new(self).produce()
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
		for &id in ids {
			list.push(if id == 0 { Hop::UNKNOWN } else { origin(id) }).unwrap();
		}
		list
	}

	/// The scope granting these prefixes: each spelled as its subtree pattern.
	fn scopes(prefixes: &[&str]) -> Patterns {
		prefixes
			.iter()
			.map(|prefix| Pattern::subtree(prefix).unwrap())
			.collect()
	}

	#[test]
	fn default_config_mints_a_real_hop() {
		let config = Config::default();
		assert_ne!(config.hop, Hop::UNKNOWN);
		let (producer, _driver) = Producer::new(config.clone());
		assert_eq!(producer.hop(), config.hop);
		assert_eq!(producer.consume().hop(), config.hop);
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
	async fn queued(server: &Dynamic) -> Request {
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
	async fn broadcast_announces_its_own_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();

		// Created unannounced: reachable by exact path, invisible to the cursor.
		let broadcast = producer.create_broadcast("room/alice").unwrap();
		announced.assert_next_wait();
		let local = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("local")
			.expect("resolves");
		assert_eq!(local.info().path.as_str(), "room/alice");

		broadcast.announce(Route::default().with_cost(3)).unwrap();
		let route = announced.assert_next_active("room/alice");
		assert_eq!(route.cost, Cost::new(3));

		// Announcing again re-prices in place.
		broadcast.announce(Route::default().with_cost(1)).unwrap();
		let route = announced.assert_next_active("room/alice");
		assert_eq!(route.cost, Cost::new(1));

		// Off the air: the route retracts while the broadcast stays reachable.
		broadcast.unannounce();
		announced.assert_next_ended("room/alice");
		broadcast.unannounce();
		announced.assert_next_wait();
		let local = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("local")
			.expect("resolves");
		assert_eq!(local.info().path.as_str(), "room/alice");

		// Back on the air, then the end of the broadcast retracts for good.
		broadcast.announce(Route::default()).unwrap();
		announced.assert_next_active("room/alice");
		broadcast.finish();
		announced.assert_next_ended("room/alice");
		assert!(matches!(broadcast.announce(Route::default()), Err(Error::Closed)));
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn broadcast_announcement_retracts_with_the_last_producer() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();

		let broadcast = producer.create_broadcast("room/alice").unwrap();
		let clone = broadcast.clone();
		broadcast.announce(Route::default()).unwrap();
		announced.assert_next_active("room/alice");

		// A clone keeps the broadcast, and its advertisement, alive.
		drop(broadcast);
		announced.assert_next_wait();
		drop(clone);
		announced.assert_next_ended("room/alice");
	}

	#[tokio::test]
	async fn publish_creates_and_announces_together() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();
		let _broadcast = producer.publish("room/alice", Route::default()).unwrap();
		announced.assert_next_active("room/alice");
	}

	#[tokio::test]
	async fn standalone_broadcast_cannot_announce() {
		let broadcast = broadcast::Info::new().produce();
		assert!(matches!(broadcast.announce(Route::default()), Err(Error::Closed)));
		// Harmless without an advertisement to retract.
		broadcast.unannounce();
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

	/// A handle names its subtrees by path, so scoping or rooting one creates
	/// nothing in the tree; only a broadcast does, and its teardown prunes it back
	/// out. Otherwise a relay grows by one node per distinct scope for as long as
	/// it runs.
	#[tokio::test]
	async fn scoping_creates_no_nodes() {
		let producer = origin(1).produce();
		let bare = producer.node_count();

		let scoped = producer.consume().scope("", &scopes(&["room/a", "room/b"])).unwrap();
		assert_eq!(producer.node_count(), bare, "scoping should not create nodes");
		assert!(
			producer
				.consume()
				.scope("room/c", &Patterns::from(Pattern::all()))
				.is_ok()
		);
		assert_eq!(producer.node_count(), bare, "rooting should not create nodes");
		drop(scoped);

		let broadcast = producer.create_broadcast("room/a/chat").unwrap();
		assert_eq!(producer.node_count(), bare + 3, "room, a and chat");
		broadcast.finish();
		settle(|| producer.node_count() == bare).await;
	}

	/// A scoped handle resolves by path against the tree rather than through a
	/// node handle, so pruning its subtree between two broadcasts cannot strand it
	/// on an orphan no lookup reaches.
	#[tokio::test]
	async fn scoped_handles_survive_a_prune() {
		let producer = origin(1).produce();
		let scoped = producer.scope("", &scopes(&["channel"])).unwrap();
		let consumer = producer.consume().scope("", &scopes(&["channel"])).unwrap();

		// Create the scoped subtree and prune it straight back out.
		let first = scoped.create_broadcast("channel/chat").unwrap();
		assert!(consumer.get_broadcast("channel/chat").is_some());
		first.finish();
		settle(|| consumer.get_broadcast("channel/chat").is_none()).await;

		let _second = scoped.create_broadcast("channel/chat").unwrap();
		assert!(
			consumer.get_broadcast("channel/chat").is_some(),
			"the source attached into an orphan"
		);
	}

	#[tokio::test]
	async fn announce_keeps_its_prefix_under_a_producer_scope() {
		let producer = origin(1).produce();
		let scoped = producer.scope("", &scopes(&["room"])).unwrap();

		// Prefix advertisements stay prefixes. The scope filters requests locally.
		let _a = scoped.announce("", Route::default()).unwrap();
		let mut announced = producer.consume().announced();
		announced.assert_next_active("");

		// Disjoint prefixes cannot be claimed at all.
		assert!(matches!(
			scoped.announce("other", Route::default()),
			Err(Error::Unauthorized)
		));
	}

	#[tokio::test]
	async fn cursor_keeps_an_overlapping_prefix_above_its_scope() {
		let producer = origin(1).produce();
		let _a = producer.announce("", Route::default()).unwrap();

		let consumer = producer.consume().scope("", &scopes(&["room"])).unwrap();
		let mut announced = consumer.announced();
		announced.assert_next_active("");
	}

	#[tokio::test]
	async fn cursor_root_strips_prefix() {
		let producer = origin(1).produce();
		let _a = producer.announce("room/alice", Route::default()).unwrap();

		let consumer = producer
			.consume()
			.scope("room", &Patterns::from(Pattern::all()))
			.unwrap();
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
	async fn exclude_matches_via_when_the_chain_is_anonymous() {
		let producer = origin(1).produce();
		let assigned = origin(777);
		let _echoed = producer
			.announce("echoed", Route::default().with_hops(hops(&[0])).with_via(assigned))
			.unwrap();
		let _local = producer
			.announce("local", Route::default().with_hops(hops(&[10])))
			.unwrap();

		let mut hidden = producer.consume().excluding(assigned).announced();
		hidden.assert_next_active("local");
		hidden.assert_next_wait();
	}

	#[tokio::test]
	async fn anonymous_route_loses_to_identified_at_any_cost() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();

		let _anonymous = producer
			.announce("room", Route::default().with_hops(hops(&[0])).with_cost(1))
			.unwrap();
		let route = announced.assert_next_active("room");
		assert!(route.is_anonymous());
		assert_eq!(route.cost, Cost::new(1));

		let _identified = producer
			.announce("room", Route::default().with_hops(hops(&[10])).with_cost(5))
			.unwrap();
		let route = announced.assert_next_active("room");
		assert!(!route.is_anonymous());
		assert_eq!(route.cost, Cost::new(5));
	}

	#[tokio::test]
	async fn anonymous_routes_order_by_cost() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();

		let expensive = producer
			.announce("room", Route::default().with_hops(hops(&[0])).with_cost(5))
			.unwrap();
		let route = announced.assert_next_active("room");
		assert_eq!(route.cost, Cost::new(5));

		let _cheap = producer
			.announce("room", Route::default().with_hops(hops(&[0, 7])).with_cost(1))
			.unwrap();
		let route = announced.assert_next_active("room");
		assert!(route.is_anonymous());
		assert_eq!(route.cost, Cost::new(1));

		drop(expensive);
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn anonymous_chain_from_identified_peer_still_ranks_last() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();

		let _anonymous = producer
			.announce(
				"room",
				Route::default()
					.with_hops(hops(&[0, 7]))
					.with_cost(1)
					.with_via(origin(7)),
			)
			.unwrap();
		announced.assert_next_active("room");

		let _identified = producer
			.announce("room", Route::default().with_hops(hops(&[10, 20])).with_cost(5))
			.unwrap();
		let route = announced.assert_next_active("room");
		assert!(!route.is_anonymous());
		assert_eq!(route.cost, Cost::new(5));
	}

	#[tokio::test]
	async fn request_prefers_identified_over_cheaper_anonymous() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let anonymous = producer
			.dynamic("room", Route::default().with_hops(hops(&[0])).with_cost(1))
			.unwrap();
		let identified = producer
			.dynamic("room", Route::default().with_hops(hops(&[10])).with_cost(5))
			.unwrap();

		let _pending = consumer.request_broadcast("room/alice");
		let request = queued(&identified).await;
		assert_eq!(request.path().as_str(), "room/alice");
		assert!(
			anonymous.poll_requested_broadcast(&kio::Waiter::noop()).is_pending(),
			"the cheaper anonymous route must not serve"
		);
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
		// Broad and cheap; narrow and expensive. Both present relative to a cursor
		// rooted below them, and the narrow one is what a request there resolves.
		let _broad = producer.announce("room", Route::default().with_cost(1)).unwrap();
		let _narrow = producer.announce("room/alice", Route::default().with_cost(9)).unwrap();

		let consumer = producer
			.consume()
			.scope("room/alice", &Patterns::from(Pattern::all()))
			.unwrap();
		let mut announced = consumer.announced();
		let route = announced.assert_next_active("");
		assert_eq!(route.cost, Cost::new(9));
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn capture_change_retracts_before_reannouncing_a_presented_prefix() {
		let producer = origin(1).produce();
		let _broad = producer.announce("room", Route::default()).unwrap();
		let exact = producer.announce("room/alice", Route::default()).unwrap();
		let consumer = producer
			.consume()
			.scope("", &Patterns::from("room/*".parse::<Pattern>().unwrap()))
			.unwrap()
			.scope("room/alice", &Patterns::from(Pattern::all()))
			.unwrap();
		let mut announced = consumer.announced();

		let first = announced.next().now_or_never().expect("next").expect("announce");
		assert_eq!(first.prefix.as_str(), "");
		assert_eq!(first.kind, AnnounceKind::Announced);
		assert_eq!(first.captures, Some(Vec::new()));

		drop(exact);
		let retracted = announced.next().now_or_never().expect("next").expect("retract");
		assert_eq!(retracted.prefix.as_str(), "");
		assert_eq!(retracted.kind, AnnounceKind::Retracted);
		assert_eq!(retracted.captures, Some(Vec::new()));
		let replacement = announced.next().now_or_never().expect("next").expect("announce");
		assert_eq!(replacement.prefix.as_str(), "");
		assert_eq!(replacement.kind, AnnounceKind::Announced);
		assert_eq!(replacement.captures, None);
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

	#[test]
	fn create_broadcast_accepts_a_max_depth_path() {
		let producer = origin(1).produce();
		let path = vec!["a"; Path::MAX_PARTS].join("/");
		let _broadcast = producer.create_broadcast(path.as_str()).expect("max depth is allowed");
		let deeper = vec!["a"; Path::MAX_PARTS + 1].join("/");
		assert!(matches!(
			producer.create_broadcast(deeper.as_str()),
			Err(Error::BoundsExceeded(_))
		));
	}

	#[tokio::test]
	async fn duplicate_routes_aggregate_until_the_last_leaves() {
		let producer = origin(1).produce();
		let first = producer.dynamic("live", Route::default().with_cost(3)).unwrap();
		let second = producer.dynamic("live", Route::default().with_cost(1)).unwrap();

		let mut announced = producer.consume().announced();
		let update = announced.next().now_or_never().expect("next").expect("no next");
		assert_eq!(update.prefix.as_str(), "live");
		assert_eq!(update.kind, AnnounceKind::Announced);
		assert_eq!(update.route.cost, Cost::new(1));
		announced.assert_next_wait();

		drop(second);
		let update = announced.next().now_or_never().expect("next").expect("no next");
		assert_eq!(update.prefix.as_str(), "live");
		assert_eq!(update.kind, AnnounceKind::Updated);
		assert_eq!(update.route.cost, Cost::new(3));

		drop(first);
		announced.assert_next_ended("live");
		announced.assert_next_wait();
	}

	#[test]
	fn dynamic_may_cover_a_scope_but_disjoint_prefixes_are_refused() {
		let producer = origin(1).produce();
		let scoped = producer.scope("", &scopes(&["room"])).unwrap();
		let _broad = scoped
			.dynamic("", Route::default())
			.expect("an overlapping prefix is accepted");

		let _ok = scoped
			.dynamic("room/alice", Route::default())
			.expect("a contained prefix is accepted");
		assert!(matches!(
			scoped.dynamic("other", Route::default()),
			Err(Error::Unauthorized)
		));
	}

	#[tokio::test]
	async fn dynamic_route_keeps_its_producer_scope() {
		let producer = origin(1).produce();
		let scope = Patterns::from("*/chat".parse::<Pattern>().unwrap());
		let scoped = producer.scope("", &scope).unwrap();
		let dynamic = scoped.dynamic("", Route::default()).unwrap();

		let mut matching = producer
			.consume()
			.scope("", &scopes(&["room/chat"]))
			.unwrap()
			.announced();
		matching.assert_next_active("");
		let mut outside = producer
			.consume()
			.scope("", &scopes(&["room/video"]))
			.unwrap()
			.announced();
		outside.assert_next_wait();

		let refused = producer
			.consume()
			.request_broadcast("room/video")
			.now_or_never()
			.expect("an out-of-scope request must be refused synchronously");
		assert!(matches!(refused, Err(Error::Unroutable)));
		assert!(dynamic.requested_broadcast().now_or_never().is_none());

		let _pending = producer.consume().request_broadcast("room/chat");
		let request = queued(&dynamic).await;
		assert_eq!(request.path().as_str(), "room/chat");
	}

	#[tokio::test]
	async fn dynamic_accepts_a_max_depth_prefix() {
		let producer = origin(1).produce();
		let path = (0..Path::MAX_PARTS)
			.map(|i| format!("s{i}"))
			.collect::<Vec<_>>()
			.join("/");
		let mut announced = producer.consume().announced();

		let dynamic = producer.dynamic(&path, Route::default()).expect("max depth is allowed");
		announced.assert_next_active(&path);

		let _pending = producer.consume().request_broadcast(&path);
		let request = queued(&dynamic).await;
		assert_eq!(request.path().as_str(), path);
	}

	#[tokio::test]
	async fn dynamic_exclusion_skips_routes_through_the_subscriber() {
		let producer = origin(1).produce();
		let _server = producer
			.dynamic("live", Route::default().with_hops(hops(&[7])))
			.unwrap();

		let mut excluded = producer.consume().excluding(origin(7)).announced();
		excluded.assert_next_wait();

		let mut clean = producer.consume().excluding(origin(8)).announced();
		clean.assert_next_active("live");
	}

	/// The consumer is a `Stream` of the same updates as `next`.
	#[tokio::test]
	async fn announce_consumer_is_a_stream() {
		use futures::StreamExt;
		let producer = origin(1).produce();
		let server = producer.dynamic("live", Route::default()).unwrap();
		let mut announced = producer.consume().announced();
		let update = StreamExt::next(&mut announced)
			.now_or_never()
			.expect("next")
			.expect("no next");
		assert_eq!(update.prefix.as_str(), "live");
		assert_eq!(update.kind, AnnounceKind::Announced);
		assert!(StreamExt::next(&mut announced).now_or_never().is_none());
		drop(server);
		let update = StreamExt::next(&mut announced)
			.now_or_never()
			.expect("next")
			.expect("no next");
		assert_eq!(update.kind, AnnounceKind::Retracted);
	}

	#[tokio::test]
	async fn dynamic_retracts() {
		let producer = origin(1).produce();
		let server = producer.dynamic("live", Route::default()).unwrap();
		let mut announced = producer.consume().announced();
		announced.assert_next_active("live");

		drop(server);
		announced.assert_next_ended("live");
	}

	#[test]
	fn charged_wildcard_cost_accumulates_across_hops() {
		let first = Cost::new(4).charged(1);
		let second = first.charged(2);
		assert_eq!(second, Cost { warm: 7, cold: 7 });
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

		let server = producer.dynamic("room", Route::default()).unwrap();

		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&server).await;
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
		let server = producer.dynamic("room", Route::default()).unwrap();

		let first = consumer.request_broadcast("room/alice");
		let second = consumer.request_broadcast("room/alice");

		let request = queued(&server).await;
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
		let server = producer.dynamic("room", Route::default()).unwrap();

		let pending = consumer.request_broadcast("room/alice");
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
		let standby_server = producer.dynamic("room", Route::default()).unwrap();
		let second_server = producer.dynamic("room", Route::default()).unwrap();
		let incumbent_server = producer.dynamic("room", Route::default()).unwrap();

		let mut resolving = Box::pin(consumer.routed_broadcast("room/alice"));
		assert!((&mut resolving).now_or_never().is_none());

		// Each incumbent dies with the front's request in flight on it: the
		// front's watcher observes the retraction and retries through the next
		// standby instead of parking on an announce update that never comes. Two
		// retractions in a row, so the announce stream's initial coverage replay
		// cannot paper over the missing retry.
		drop(incumbent_server);
		assert!((&mut resolving).now_or_never().is_none());
		drop(second_server);
		assert!((&mut resolving).now_or_never().is_none());

		let request = queued(&standby_server).await;
		let source = broadcast::Info::new().produce();
		request.accept(&source);

		let resolved = resolving.await.expect("resolves via the standby");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
	}

	#[tokio::test]
	async fn split_horizon_skips_routes_through_the_requester() {
		let producer = origin(1).produce();
		let _server = producer
			.dynamic("room", Route::default().with_hops(hops(&[7])))
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

	/// A handler that rejects a path with `Unroutable` while its route stands
	/// gives the requester that answer; the front must not re-ask the same route
	/// forever, which would spin the origin driver.
	#[tokio::test]
	async fn handler_rejection_is_final() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.dynamic("room", Route::default()).unwrap();

		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&server).await;
		request.reject(Error::Unroutable);
		let err = tokio::time::timeout(Duration::from_secs(5), pending)
			.await
			.expect("the front must give up, not spin")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		// The route still stands and serves the next path.
		let pending = consumer.request_broadcast("room/bob");
		let request = queued(&server).await;
		assert_eq!(request.path().as_str(), "room/bob");
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		pending.await.expect("resolves");
	}

	/// `routed_broadcast` treats a handler's rejection as the table's verdict:
	/// it waits for the table to move instead of re-asking the same route.
	#[tokio::test]
	async fn routed_broadcast_waits_out_a_rejection() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.dynamic("room", Route::default()).unwrap();

		let mut resolving = Box::pin(consumer.routed_broadcast("room/alice"));
		assert!((&mut resolving).now_or_never().is_none());
		let request = queued(&server).await;
		request.reject(Error::Unroutable);

		// Parked: the route stands, so nothing changed that a retry could use.
		for _ in 0..20 {
			tokio::task::yield_now().await;
		}
		assert!((&mut resolving).now_or_never().is_none());
		assert!(server.poll_requested_broadcast(&kio::Waiter::noop()).is_pending());

		// A re-price moves the table: the retry reaches the handler, which serves it.
		server.update(Route::default().with_cost(2)).unwrap();
		assert!((&mut resolving).now_or_never().is_none());
		let request = queued(&server).await;
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		resolving.await.expect("resolves");
	}

	/// A local broadcast appearing at the exact path is a table change too: a
	/// requester parked on a handler's rejection resolves to it.
	#[tokio::test]
	async fn routed_broadcast_wakes_for_a_local_broadcast() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.dynamic("room", Route::default()).unwrap();

		let mut resolving = Box::pin(consumer.routed_broadcast("room/alice"));
		assert!((&mut resolving).now_or_never().is_none());
		queued(&server).await.reject(Error::Unroutable);
		for _ in 0..20 {
			tokio::task::yield_now().await;
		}
		assert!((&mut resolving).now_or_never().is_none());

		// Unannounced, so no route changes: the exact path itself is what moved.
		let _local = producer.create_broadcast("room/alice").unwrap();
		let resolved = resolving.await.expect("resolves locally");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		assert!(server.poll_requested_broadcast(&kio::Waiter::noop()).is_pending());
	}

	/// A track first subscribed after the front is already serving another still
	/// replays what its source holds, like the first track did.
	#[tokio::test]
	async fn late_track_on_a_served_front_replays() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.dynamic("room", Route::default()).unwrap();

		let source = broadcast::Info::new().produce();
		for name in ["a", "b"] {
			let track = source.create_track(name, None).unwrap();
			let mut group = track.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, name.as_bytes()).unwrap();
			group.finish().unwrap();
			// The producer stays alive: the track is open, like a live SI track.
			std::mem::forget(track);
		}

		let pending = consumer.request_broadcast("room/alice");
		queued(&server).await.accept(&source);
		let resolved = pending.await.expect("resolves");

		let budget = track::Subscription::default().with_max_age(Duration::from_secs(3600));
		for name in ["a", "b"] {
			let mut subscription = resolved
				.track(name)
				.unwrap()
				.subscribe(budget.clone())
				.await
				.expect("subscribe");
			let mut group = tokio::time::timeout(Duration::from_secs(5), subscription.recv_group())
				.await
				.expect("the late track must replay, not park")
				.expect("recv group")
				.expect("track ended early");
			let frame = group.read_frame().await.expect("read frame").expect("frame");
			assert_eq!(&frame.payload[..], name.as_bytes());
		}
	}

	#[tokio::test]
	async fn most_specific_prefix_shadows() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broad_server = producer.dynamic("", Route::default()).unwrap();
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
		let request = queued(&broad_server).await;
		assert_eq!(request.path().as_str(), "room/alice");
	}

	#[tokio::test]
	async fn root_dynamic_serves_any_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();
		let dynamic = producer.dynamic("", Route::default()).unwrap();
		// The root claim is advertised like any other prefix.
		announced.assert_next_active("");

		let pending = consumer.request_broadcast("anything/at/all");
		let request = queued(&dynamic).await;
		assert_eq!(request.path().as_str(), "anything/at/all");

		let source = broadcast::Info::new().produce();
		request.accept(&source);
		let resolved = pending.await.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "anything/at/all");

		// Nothing serves an uncovered path once the handler is gone.
		drop(dynamic);
		announced.assert_next_ended("");
		let err = consumer
			.request_broadcast("something/else")
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
	}

	/// A path outside the consumer's scope never reaches a live dynamic handler.
	///
	/// `scope` is authoritative, so an out-of-scope path is unauthorized before
	/// routing can send a request to the handler. A `Request` carries only a path,
	/// so the handler cannot tell who asked.
	#[tokio::test]
	async fn out_of_scope_request_never_reaches_the_dynamic_handler() {
		let producer = origin(1).produce();
		let dynamic = producer.dynamic("", Route::default()).unwrap();
		let scoped = producer.consume().scope("", &scopes(&["tenant-a"])).unwrap();

		// `tenant-a-other` shares a character prefix but not a segment, so this
		// also pins that the check is segment-aware rather than textual.
		for path in ["tenant-b/live", "tenant-a-other/live"] {
			let refused = scoped
				.request_broadcast(path)
				.now_or_never()
				.expect("an out-of-scope request must be refused synchronously, not queued");
			assert!(matches!(refused, Err(Error::Unauthorized)));
			assert!(
				dynamic.requested_broadcast().now_or_never().is_none(),
				"the dynamic handler was asked to create a broadcast the requester may not read"
			);
		}
	}

	#[tokio::test]
	async fn routed_waits_for_coverage() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let mut fut = consumer.routed("room/alice").boxed();
		assert!((&mut fut).now_or_never().is_none());

		// A covering prefix resolves the wait.
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
	async fn routed_accepts_a_max_depth_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let path = (0..Path::MAX_PARTS)
			.map(|i| format!("s{i}"))
			.collect::<Vec<_>>()
			.join("/");
		assert_eq!(Path::new(&path).parts().count(), Path::MAX_PARTS);

		assert!(consumer.allowed().matches(&path));

		let mut fut = consumer.routed(&path).boxed();
		assert!((&mut fut).now_or_never().is_none());

		// A covering root still resolves: the lookup must not require `path/**`.
		let _a = producer.announce("", Route::default()).unwrap();
		fut.now_or_never().expect("covered").expect("routed");
	}

	#[tokio::test]
	async fn teardown_ends_everything() {
		let (producer, driver) = Producer::new(Config::new(origin(1)));
		let consumer = producer.consume();
		let _announcement = producer.announce("room", Route::default()).unwrap();
		let mut announced = consumer.announced();
		announced.assert_next_active("room");

		let _server = producer.dynamic("served", Route::default()).unwrap();
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
		async fn new(first: &[u64]) -> (Self, Dynamic, broadcast::Producer) {
			let producer = origin(1).produce();
			let consumer = producer.consume();

			let server = producer
				.dynamic("room", Route::default().with_hops(hops(first)))
				.unwrap();

			let pending = consumer.request_broadcast("room/alice");
			let request = queued(&server).await;
			let source = broadcast::Info::new().produce();
			let track = source.create_track("video", None).unwrap();
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
				server,
				source,
			)
		}

		/// Stand up a second served route with `first` as its first hop and hand
		/// back its handle, ready to answer the front's re-request.
		fn standby(&self, first: &[u64]) -> Dynamic {
			self.producer
				.dynamic("room", Route::default().with_hops(hops(first)))
				.unwrap()
		}
	}

	/// Accept the front's re-request on `server` with a source carrying the same
	/// content stream (the delivered group plus its successor) and prove the
	/// rig's subscription resumes onto it: the successor group is delivered on
	/// the same subscription, at the group boundary.
	async fn assert_resumes(rig: &mut ResumeRig, server: &Dynamic) {
		let request = queued(server).await;
		let replacement = broadcast::Info::new().produce();
		let track = replacement.create_track("video", None).unwrap();
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

	/// The driver's completion contract: it resolves once every producer handle
	/// drops, however many read handles remain.
	#[tokio::test]
	async fn driver_resolves_with_live_consumers() {
		let (producer, driver) = Producer::new(Config::new(origin(1)));
		let consumer = producer.consume();
		let run = crate::time::run(driver);
		drop(producer);
		tokio::time::timeout(Duration::from_secs(5), run)
			.await
			.expect("driver must finish once the producers are gone");
		drop(consumer);
	}

	#[tokio::test]
	async fn remote_source_resumes_through_same_first_hop() {
		let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
		let standby_server = rig.standby(&[10, 20]);

		// The serving route dies: retraction plus source abort, like a session.
		drop(incumbent);
		drop(source);

		// The standby shares the first hop, so the subscription resumes there.
		assert_resumes(&mut rig, &standby_server).await;
	}

	/// A source claiming the same content cannot change immutable track metadata:
	/// the successor is refused instead of the subscriber's samples being read on
	/// a different grid, and the verdict outlives the aborted logical track.
	#[tokio::test]
	async fn incompatible_successor_is_refused() {
		for replacement in [
			track::Info::default().with_timescale(crate::Timescale::MICRO),
			track::Info::default().with_priority(7),
			track::Info::default().with_max_age(Duration::from_secs(7)),
		] {
			let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
			let standby_server = rig.standby(&[10, 20]);
			drop(incumbent);
			drop(source);

			// The standby shares the first hop, so the front re-requests through it,
			// but its copy of the track is on another grid.
			let request = queued(&standby_server).await;
			let successor = broadcast::Info::new().produce();
			let track = successor.create_track("video", replacement).unwrap();
			let mut group = track.append_group().unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"before".as_ref()).unwrap();
			group.finish().unwrap();
			request.accept(&successor);

			assert!(
				matches!(rig.subscription.recv_group().await, Err(Error::Unsupported)),
				"the subscription must abort rather than resume onto incompatible metadata"
			);

			// Reopening the aborted logical track must not forget the broadcast's metadata.
			let reopened = rig.resolved.track("video").unwrap();
			assert!(matches!(reopened.query().await, Err(Error::Unsupported)));
			assert!(matches!(reopened.subscribe(None).await, Err(Error::Unsupported)));
		}
	}

	#[tokio::test]
	async fn different_first_hop_ends_the_subscription() {
		let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
		// Another publisher entirely: same path, different first hop.
		let rival_server = rig.standby(&[11]);

		drop(incumbent);
		drop(source);

		// The subscription ends rather than splicing onto the rival's frames.
		let err = rig.subscription.recv_group().await.err().expect("subscription ends");
		assert!(matches!(err, Error::Dropped), "unexpected end: {err}");

		// A fresh request resolves through the rival.
		let consumer = rig.producer.consume();
		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&rival_server).await;
		let replacement = broadcast::Info::new().produce();
		request.accept(&replacement);
		pending.await.expect("re-request resolves through the rival");
	}

	#[tokio::test]
	async fn anonymous_routes_never_resume() {
		// An empty hop chain identifies nobody, so two of them must not pass for
		// one publisher reconnecting.
		let (mut rig, incumbent, source) = ResumeRig::new(&[]).await;
		let _twin_server = rig.standby(&[]);

		drop(incumbent);
		drop(source);

		let err = rig.subscription.recv_group().await.err().expect("subscription ends");
		assert!(matches!(err, Error::Dropped), "unexpected end: {err}");
	}

	/// An anonymous publisher that dies without unannouncing is replaced by the
	/// next anonymous session at the same path: a subscriber on a third session
	/// gets the newcomer's media immediately, not a lingering dead front.
	///
	/// The front closes with its last source, so the newcomer attaches a fresh
	/// one and the subscriber resolves it without parking.
	#[tokio::test]
	async fn anonymous_handoff_serves_the_newcomer_immediately() {
		let producer = origin(1).produce();

		// Session A: an assigned anonymous hop serving the path.
		let server_a = producer
			.dynamic("room", Route::default().with_hops(hops(&[10])))
			.unwrap();

		// Session C: a third anonymous session, excluding the hop the server
		// minted for it, the same split-horizon a live session applies.
		let consumer = producer.consume().excluding(origin(30));
		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&server_a).await;
		let source_a = broadcast::Info::new().produce();
		let track_a = source_a.create_track("video", None).unwrap();
		let mut group = track_a.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"from-a".as_ref()).unwrap();
		group.finish().unwrap();
		request.accept(&source_a);

		let resolved_a = pending.await.expect("resolves");
		let mut sub_a = resolved_a
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = sub_a
			.recv_group()
			.await
			.expect("recv group")
			.expect("track ended early");
		assert_eq!(
			&group.read_frame().await.expect("read frame").expect("frame").payload[..],
			b"from-a"
		);

		// A dies without an unannounce: the source and its route drop together,
		// the way a lost session retracts rather than sending ANNOUNCE_END.
		drop(track_a);
		drop(source_a);
		drop(server_a);

		// The front closed with A's last source.
		let err = sub_a.recv_group().await.err().expect("front closed");
		assert!(matches!(err, Error::Dropped), "unexpected end: {err}");

		// No stale front at the leaf, and a repeat request does not join the
		// corpse: nothing covers the path, so it is Unroutable rather than
		// parked on a linger or 404 `dropped` from the dead front.
		settle(|| consumer.get_broadcast("room/alice").is_none()).await;
		settle(|| {
			matches!(
				consumer.request_broadcast("room/alice").now_or_never(),
				Some(Err(Error::Unroutable))
			)
		})
		.await;

		// Session B attaches at the same path. Its front is served immediately.
		let server_b = producer
			.dynamic("room", Route::default().with_hops(hops(&[20])))
			.unwrap();
		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&server_b).await;
		let source_b = broadcast::Info::new().produce();
		let track_b = source_b.create_track("video", None).unwrap();
		let mut group = track_b.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"from-b".as_ref()).unwrap();
		group.finish().unwrap();
		request.accept(&source_b);

		let resolved_b = pending.await.expect("B's front is served immediately");
		assert!(
			!resolved_b.is_clone(&resolved_a),
			"B must not splice into A's closed front"
		);

		let mut sub_b = resolved_b
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = sub_b
			.recv_group()
			.await
			.expect("recv group")
			.expect("track ended early");
		assert_eq!(
			&group.read_frame().await.expect("read frame").expect("frame").payload[..],
			b"from-b"
		);
	}

	#[tokio::test]
	async fn reprice_is_invisible_to_the_subscription() {
		let (rig, incumbent, source) = ResumeRig::new(&[10]).await;

		// A metadata-only reprice of the only route: nothing re-requests and the
		// subscription keeps flowing from the same source.
		incumbent
			.update(Route::default().with_hops(hops(&[10])).with_cost(9))
			.unwrap();

		let track = source.create_track("audio", None).unwrap();
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
		let standby_server = rig.standby(&[10, 20]);

		// The serving route drains: repriced to the ceiling while its session
		// keeps serving. The front migrates to the standby without waiting for
		// the death.
		incumbent
			.update(Route::default().with_hops(hops(&[10])).with_cost(Cost::DRAIN))
			.unwrap();

		assert_resumes(&mut rig, &standby_server).await;

		// The drained source outlived the migration.
		drop(incumbent);
		drop(source);
	}

	#[tokio::test]
	async fn local_sources_splice_newest_first() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let first = producer.create_broadcast("room/alice").unwrap();
		let resolved = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("resolves")
			.expect("resolves");

		// A second source at the same path joins the same front.
		let second = producer.create_broadcast("room/alice").unwrap();
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

	/// An origin front drops the source track as soon as its last reader leaves,
	/// so the publisher's `unused()` resolves far below `TRACK_IDLE_LINGER`.
	/// Cached groups stay on the front for the linger; a returning reader
	/// replays them and re-splices for groups past that edge.
	#[tokio::test]
	async fn origin_front_drops_the_source_when_unused() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.create_broadcast("room/alice").unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"cached".as_ref()).unwrap();
		group.finish().unwrap();

		let resolved = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("resolves")
			.expect("resolves");
		let mut subscription = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"cached");
		drop(group);
		drop(subscription);

		tokio::time::timeout(Duration::from_secs(1), track.unused())
			.await
			.expect("source unused should resolve far below TRACK_IDLE_LINGER")
			.expect("source closed");

		// Cached groups stay on the front for the linger; a returning reader
		// replays them without waiting out the window.
		let mut again = resolved
			.track("video")
			.unwrap()
			.subscribe(track::Subscription::default().with_max_age(Duration::from_secs(3600)))
			.await
			.expect("resubscribe");
		let mut group = tokio::time::timeout(Duration::from_secs(1), again.recv_group())
			.await
			.expect("cached group is still on the front")
			.expect("recv group")
			.expect("track ended early");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"cached");

		tokio::time::timeout(Duration::from_secs(1), track.used())
			.await
			.expect("returning reader re-splices the source")
			.expect("source closed");

		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"live".as_ref()).unwrap();
		group.finish().unwrap();
		let mut group = tokio::time::timeout(Duration::from_secs(1), again.recv_group())
			.await
			.expect("groups past the cached edge come from the re-splice")
			.expect("recv group")
			.expect("track ended early");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"live");
	}

	/// A front serving from another front's spliced copy has no snapshot to keep:
	/// it still drops upstream on the unused edge, so the publisher's `unused()`
	/// resolves far below `TRACK_IDLE_LINGER` through the whole chain. The next
	/// reader re-splices, paying `TRACK_INFO` again.
	#[tokio::test]
	async fn chained_front_drops_the_source_when_unused() {
		let leaf = origin(1).produce();
		let leaf_consumer = leaf.consume();

		let broadcast = leaf.create_broadcast("room/alice").unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"cached".as_ref()).unwrap();
		group.finish().unwrap();

		// The leaf's front view: a spliced broadcast, so any front serving from
		// it holds a spliced source copy with nothing to snapshot.
		let leaf_front = leaf_consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("resolves")
			.expect("resolves");

		let mid = origin(2).produce();
		let mid_server = mid.dynamic("room", Route::default().with_hops(hops(&[10]))).unwrap();
		let mid_pending = mid.consume().request_broadcast("room/alice");
		queued(&mid_server).await.accept(&leaf_front);
		let mid_resolved = mid_pending.await.expect("mid resolves");

		let edge = origin(3).produce();
		let edge_server = edge.dynamic("room", Route::default().with_hops(hops(&[20]))).unwrap();
		let edge_pending = edge.consume().request_broadcast("room/alice");
		queued(&edge_server).await.accept(&mid_resolved);
		let edge_resolved = edge_pending.await.expect("edge resolves");

		let mut subscription = edge_resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"cached");
		drop(group);
		drop(subscription);

		tokio::time::timeout(Duration::from_secs(5), track.unused())
			.await
			.expect("chained unused should resolve far below TRACK_IDLE_LINGER")
			.expect("source closed");

		let cached = edge_resolved.track("video").unwrap().cached_groups();
		assert_eq!(
			cached.iter().map(|(group, _)| group.sequence).collect::<Vec<_>>(),
			vec![0],
			"every front keeps the delivered groups after releasing its source"
		);

		let mut subscription = edge_resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("resubscribe");
		tokio::time::timeout(Duration::from_secs(5), track.used())
			.await
			.expect("resubscribe should reach the leaf")
			.expect("source open");
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"live".as_ref()).unwrap();
		group.finish().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"cached");
		drop(group);
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"live");
		drop(group);
		drop(subscription);

		tokio::time::timeout(Duration::from_secs(5), track.unused())
			.await
			.expect("second chained unused should resolve far below TRACK_IDLE_LINGER")
			.expect("source closed");

		let cached = edge_resolved.track("video").unwrap().cached_groups();
		assert_eq!(
			cached.iter().map(|(group, _)| group.sequence).collect::<Vec<_>>(),
			vec![0, 1],
			"repeated demand keeps every complete group while releasing its source"
		);

		let fetch = edge_resolved.track("video").unwrap().fetch_group(2, None);
		let mut fetch = std::pin::pin!(fetch);
		assert!(futures::poll!(fetch.as_mut()).is_pending(), "fetch should re-splice");
		tokio::time::timeout(Duration::from_secs(5), track.used())
			.await
			.expect("fetch should reach the leaf")
			.expect("source open");
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"fetched".as_ref()).unwrap();
		group.finish().unwrap();
		let mut group = tokio::time::timeout(Duration::from_secs(5), fetch)
			.await
			.expect("re-spliced source should answer the fetch")
			.expect("fetch succeeds");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"fetched");
	}

	/// A newer local source wins dispatch the moment it attaches, but one whose copy
	/// of the track carries different metadata is refused: the incumbent keeps
	/// serving, and the refusal is never retried once the incumbent leaves.
	#[tokio::test]
	async fn incompatible_local_source_keeps_the_incumbent() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let first = producer.create_broadcast("room/alice").unwrap();
		let track = first.create_track("video", None).unwrap();
		let resolved = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("resolves")
			.expect("resolves");
		let mut subscription = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"before".as_ref()).unwrap();
		group.finish().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");

		// The newest source is dispatched the track, and refused for its metadata.
		let second = producer.create_broadcast("room/alice").unwrap();
		let _incompatible = second
			.create_track("video", track::Info::default().with_timescale(crate::Timescale::MICRO))
			.unwrap();
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}

		// Still spliced to the incumbent, still delivering.
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"still".as_ref()).unwrap();
		group.finish().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"still");

		// The incumbent leaving exhausts the table: the refusal is never retried.
		drop(track);
		first.finish();
		assert!(matches!(subscription.recv_group().await, Err(Error::Unsupported)));
	}

	#[tokio::test]
	async fn multiple_scopes_present_one_broad_prefix() {
		let producer = origin(1).produce();
		let _a = producer.announce("", Route::default()).unwrap();

		let consumer = producer.consume().scope("", &scopes(&["alpha", "beta"])).unwrap();
		let mut announced = consumer.announced();
		announced.assert_next_active("");
		announced.assert_next_wait();
	}

	#[test]
	fn scope_accepts_every_pattern_union() {
		let producer = origin(1).produce();

		// The root grant is `**`, the old empty prefix.
		let root = producer.scope("", &Patterns::from(Pattern::all())).unwrap();
		assert_eq!(root.allowed(), Patterns::from(Pattern::all()));

		// `foo/**` keeps the old `foo` prefix meaning.
		let scoped = producer.scope("", &scopes(&["room"])).unwrap();
		assert_eq!(scoped.allowed(), scopes(&["room"]));

		// Multiple prefixes round-trip, with overlap collapsed.
		let multi = producer.scope("", &scopes(&["room", "room/chat", "anon"])).unwrap();
		assert_eq!(multi.allowed(), scopes(&["room", "anon"]));

		// The consumer side reports the same way.
		let consumer = producer.consume().scope("", &scopes(&["room"])).unwrap();
		assert_eq!(consumer.allowed(), scopes(&["room"]));

		for text in ["room", "", "*room", "room/*", "*", "**/room", "room/**/chat", "*.hang"] {
			let union = Patterns::from(text.parse::<Pattern>().unwrap());
			assert_eq!(producer.scope("", &union).expect(text).allowed(), union, "{text}");
			assert_eq!(
				producer.consume().scope("", &union).expect(text).allowed(),
				union,
				"{text}"
			);
		}

		let mixed: Patterns = ["room/**".parse().unwrap(), "other".parse().unwrap()]
			.into_iter()
			.collect();
		assert_eq!(producer.scope("", &mixed).unwrap().allowed(), mixed);
	}

	#[test]
	fn create_broadcast_refuses_a_path_no_pattern_can_spell() {
		let producer = origin(1).produce();

		// A `*` segment is a valid path but an invalid literal, so its route could
		// never be built: refuse the broadcast instead of publishing one that
		// announces nowhere.
		assert!(matches!(
			producer.create_broadcast("room/*"),
			Err(Error::BoundsExceeded(_))
		));
		assert!(matches!(
			producer.announce("room/**", Route::default()),
			Err(Error::BoundsExceeded(_))
		));
	}

	#[test]
	fn scope_empty_union_grants_nothing() {
		let producer = origin(1).produce();

		// An empty union grants nothing: scoping is refused, like a disjoint prefix.
		assert!(matches!(producer.scope("", &Patterns::new()), Err(Error::Unauthorized)));
		assert!(matches!(
			producer.consume().scope("", &Patterns::new()),
			Err(Error::Unauthorized)
		));
	}

	#[test]
	fn scope_nests_and_rebases_roots() {
		let producer = origin(1).produce();

		// Narrowing twice intersects; the grant stays in the new vocabulary.
		let scoped = producer.scope("", &scopes(&["room"])).unwrap();
		let nested = scoped.scope("", &scopes(&["room/chat"])).unwrap();
		assert_eq!(nested.allowed(), scopes(&["room/chat"]));

		// A disjoint nesting is refused, not widened.
		assert!(matches!(
			scoped.scope("", &scopes(&["other"])),
			Err(Error::Unauthorized)
		));

		// A literal root rebases the grant without changing its meaning.
		let rooted = nested.scope("room/chat", &Patterns::from(Pattern::all())).unwrap();
		assert_eq!(rooted.allowed(), scopes(&[""]));

		// Publishing through the nested view lands where the root says.
		let broadcast = nested.create_broadcast("room/chat/live").unwrap();
		assert!(producer.consume().get_broadcast("room/chat/live").is_some());
		broadcast.finish();
	}

	#[test]
	fn scope_intersects_and_rebases_arbitrary_grants() {
		let producer = origin(1).produce();
		let rooms = producer
			.scope("", &Patterns::from("room/*".parse::<Pattern>().unwrap()))
			.unwrap();
		let chats = rooms
			.scope("", &Patterns::from("*/chat".parse::<Pattern>().unwrap()))
			.unwrap();
		assert_eq!(chats.allowed(), Patterns::from("room/chat".parse::<Pattern>().unwrap()));

		let exact = producer
			.scope("", &Patterns::from("room/alice".parse::<Pattern>().unwrap()))
			.unwrap();
		let rooted = exact.scope("room", &Patterns::from(Pattern::all())).unwrap();
		assert_eq!(rooted.allowed(), Patterns::from("alice".parse::<Pattern>().unwrap()));
		assert!(matches!(
			exact.scope("room/bob", &Patterns::from(Pattern::all())),
			Err(Error::Unauthorized)
		));

		let broadcast = exact.create_broadcast("room/alice").unwrap();
		assert!(matches!(
			exact.create_broadcast("room/alice/cam"),
			Err(Error::Unauthorized)
		));
		assert!(producer.consume().get_broadcast("room/alice").is_some());
		drop(broadcast);
	}

	#[tokio::test]
	async fn wildcard_scope_filters_announcements_and_reports_captures() {
		let producer = origin(1).produce();
		let consumer = producer
			.consume()
			.scope("", &Patterns::from("room/*/chat".parse::<Pattern>().unwrap()))
			.unwrap();
		let mut announced = consumer.announced();

		let alice = producer.create_broadcast("room/alice/chat").unwrap();
		alice.announce(Route::default()).unwrap();
		let update = announced.try_next().expect("alice's chat");
		assert_eq!(update.prefix.as_str(), "room/alice/chat");
		assert_eq!(update.captures, Some(vec!["alice".parse::<Pattern>().unwrap()]));

		let audio = producer.create_broadcast("room/alice/audio").unwrap();
		audio.announce(Route::default()).unwrap();
		announced.assert_next_wait();

		let broad = producer.announce("room", Route::default()).unwrap();
		let update = announced.try_next().expect("overlapping broad route");
		assert_eq!(update.prefix.as_str(), "room");
		assert_eq!(update.captures, None, "an overlap does not pin the wildcard");

		drop(broad);
		drop(audio);
		drop(alice);
	}

	#[tokio::test]
	async fn local_broadcast_wins_announcement_ties() {
		let producer = origin(1).produce();
		let remote = producer.announce("room/alice", Route::default().with_cost(9)).unwrap();
		let local = producer.create_broadcast("room/alice").unwrap();
		local.announce(Route::default()).unwrap();

		let mut announced = producer.consume().announced();
		let update = announced.try_next().expect("one winning route");
		assert_eq!(update.prefix.as_str(), "room/alice");
		assert_eq!(update.route.cost, Cost::default());
		announced.assert_next_wait();

		drop(local);
		drop(remote);
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

	/// Mint an origin whose pool reclaims idle content after `expiry`.
	fn expiring_origin(expiry: Duration) -> Producer {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(expiry));
		Config {
			pool,
			..Config::default()
		}
		.produce()
	}

	/// A publisher that stalls with a group still open runs no write path, so the
	/// track's own write-driven expiry never fires and a reader parked in that group
	/// is never told. The driver's wall-clock sweep is the bound.
	#[tokio::test(start_paused = true)]
	async fn stalled_publisher_open_group_is_reclaimed() {
		let expiry = Duration::from_secs(1);
		let origin = expiring_origin(expiry);
		let broadcast = origin.create_broadcast("test").unwrap();
		let track = broadcast.create_track("video", None).unwrap();

		let mut stalled = track.append_group().unwrap();
		stalled.write_frame(crate::Timestamp::ZERO, b"x".as_slice()).unwrap();
		// A successor, so the stalled group is not the protected live edge. Its
		// timestamp is inside the retention budget, so subscription expiry keeps the
		// stalled group: only reclamation can bound it.
		let _successor = track.append_group().unwrap();

		let mut reading = stalled.consume();
		assert!(reading.read_frame().await.unwrap().is_some());

		// Production goes quiet: nothing writes to this track again.
		crate::model::clock::advance(expiry * 2);

		// Bounded so a regression fails rather than parking forever, which is the
		// bug itself. Time is virtual, so the wait costs nothing.
		let reclaimed = tokio::time::timeout(Duration::from_secs(60), reading.read_frame()).await;
		assert!(
			matches!(reclaimed, Ok(Err(Error::Old))),
			"the sweep must reclaim an idle open group and surface the gap, got {reclaimed:?}"
		);
	}

	/// Reclamation is the pool's policy, not the origin's: a pool with no expiry
	/// window keeps idle content until byte pressure takes it, sweep or no sweep.
	#[tokio::test(start_paused = true)]
	async fn sweep_respects_a_disabled_expiry() {
		let origin = Config {
			pool: cache::Pool::unbounded(),
			..Config::default()
		}
		.produce();
		let broadcast = origin.create_broadcast("test").unwrap();
		let track = broadcast.create_track("video", None).unwrap();

		let mut stalled = track.append_group().unwrap();
		stalled.write_frame(crate::Timestamp::ZERO, b"x".as_slice()).unwrap();
		let _successor = track.append_group().unwrap();

		let mut reading = stalled.consume();
		assert!(reading.read_frame().await.unwrap().is_some());

		crate::model::clock::advance(Duration::from_secs(3600));
		tokio::time::advance(Duration::from_secs(3600)).await;

		assert!(
			reading.read_frame().now_or_never().is_none(),
			"a pool without an expiry window never reclaims"
		);
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
