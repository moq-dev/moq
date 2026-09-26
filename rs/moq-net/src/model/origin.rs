use crate::{broadcast, cache, group, stats, track};
use kio::Task;
use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
	fmt,
	sync::Arc,
	sync::atomic::{AtomicU64, Ordering},
	task::{Poll, ready},
	time::Duration,
};

use rand::RngExt;

use super::{
	Requests, WeakCache, WeakEntry,
	front::{Action, Candidate, Event, Front, Pin, Refusal},
};
use crate::{
	AsPath, Error, InvalidPattern, Path, PathOwned, Pattern, Patterns,
	coding::{BoundsExceeded, Decode, DecodeError, Encode, EncodeError},
	runtime::{Instant, Timers},
	time::Clock,
	util::{Keepalive, TaskSet, Tasks, TasksWeak},
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
	///
	/// Older `@moq/lite` clients decode the exclude hop as a JavaScript number
	/// and reject values above 2^53-1. Keep generated IDs in that range while
	/// [`Self::new`] accepts the full 62-bit wire range for explicit IDs.
	pub fn random() -> Self {
		let mut rng = rand::rng();
		let id = rng.random_range(1..(1u64 << 53));
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
	/// This caps the local retention and delivery budget without changing the
	/// publisher's [`max_age`](track::Info::max_age) metadata. Wall-clock
	/// reclamation of idle content is separate: [`Self::pool`]'s
	/// [`expiry`](cache::Pool::expiry) window. [`Duration::MAX`] (the default)
	/// imposes no ceiling, leaving each track's own window in force.
	pub cache_duration: Duration,
}

impl Default for Config {
	/// A fresh random hop with no byte target and the default idle expiry.
	fn default() -> Self {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY));
		Self {
			hop: Hop::random(),
			pool,
			cache_duration: Duration::MAX,
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
	/// among routes of the same anonymity, with ties broken by a broadcast published
	/// on this origin, then hop length, then a deterministic hash, and finally the
	/// most recently announced route. See [`Cost`].
	pub cost: Cost,

	/// The announcing session's declared or assigned identity.
	///
	/// Local selection state: split-horizon matches this as well as the chain, so a
	/// route is never advertised back to the session it came from even when that
	/// session withheld an identity (hop 0). Never forwarded.
	pub(crate) via: Hop,

	/// Where the route entered this origin; see [`Self::source`]. Never forwarded.
	pub(crate) source: Source,
}

impl Default for Route {
	fn default() -> Self {
		Self {
			hops: Hops::new(),
			cost: Cost::default(),
			via: Hop::UNKNOWN,
			source: Source::Local,
		}
	}
}

/// Where a route entered an origin: here, or from a cluster peer.
///
/// Origin bookkeeping, not a chain fact: the hop chain cannot say it, since a
/// client and a peer relay each append one hop. The origin records it from the
/// handle that announced the route; see [`Producer::peer`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Source {
	/// Announced on this origin: by an in-process producer or a client session.
	#[default]
	Local,
	/// Learned from a cluster peer, named by the announcing session's declared or
	/// assigned identity.
	Peer(Hop),
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

	/// Where the route entered this origin, as delivered by [`Consumer::announced`].
	///
	/// Set by the origin, not the announcer: a route handed to
	/// [`Producer::dynamic`] reports [`Source::Local`] until the origin delivers it.
	pub fn source(&self) -> Source {
		self.source
	}
}

static NEXT_CONSUMER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ConsumerId(u64);

impl ConsumerId {
	fn new() -> Self {
		Self(NEXT_CONSUMER_ID.fetch_add(1, Ordering::Relaxed))
	}
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
/// cost, then a broadcast published on this origin (it serves what is here, not a
/// claim that has to ask), then the shortest hop chain, then a deterministic hash
/// of the prefix and chain so every node converges on the same winner, and finally
/// the newest announcement, so a reconnect under an otherwise identical route wins
/// the moment it lands instead of after the transport retires the old session.
fn route_order(prefix: &Path, entry: &RouteEntry) -> (bool, Cost, bool, usize, u64, Reverse<u64>) {
	(
		entry.is_anonymous(),
		entry.cost,
		!entry.local,
		entry.hops.len(),
		fnv_key(prefix.as_str(), entry.hops.iter().copied()),
		Reverse(entry.id),
	)
}

/// The `(hops, cost, source)` metadata an announce cursor delivers alongside a prefix.
type RouteMeta = (Hops, Cost, Source);

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
	/// Progress toward the one [`AnnounceEvent::Live`] marker.
	live: LiveState,
}

/// Where a cursor stands on delivering its initial set.
enum LiveState {
	/// This many remote sources pending at subscribe time are still replaying.
	Waiting(usize),
	/// Every source has landed: the marker follows once these prefixes, pending
	/// when the last one landed, have been delivered (or cancelled).
	Owed(BTreeSet<PathOwned>),
	/// The marker was delivered.
	Done,
}

impl Default for LiveState {
	fn default() -> Self {
		Self::Waiting(0)
	}
}

impl OriginConsumerState {
	/// One source the cursor was waiting on has landed its initial set.
	fn landed(&mut self) {
		if let LiveState::Waiting(count) = &mut self.live {
			*count = count.saturating_sub(1);
			if *count == 0 {
				self.live = LiveState::Owed(self.pending.keys().cloned().collect());
			}
		}
	}

	/// `prefix` left the pending set (delivered or cancelled), so the marker no
	/// longer waits on it. A prefix still pending (the announce trailing a
	/// retraction) stays owed.
	fn settled(&mut self, prefix: &PathOwned) {
		if let LiveState::Owed(owed) = &mut self.live
			&& !self.pending.contains_key(prefix)
		{
			owed.remove(prefix);
		}
	}

	/// Whether the marker is due now.
	fn live_due(&self) -> bool {
		matches!(&self.live, LiveState::Owed(owed) if owed.is_empty())
	}

	/// Whether [`Self::take`] has something to hand out, or the cursor ended.
	fn ready(&self) -> bool {
		!self.pending.is_empty() || self.ended || self.live_due()
	}

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
			Some(PendingUpdate::Announce(_)) if !self.delivered.contains(&prefix) => self.settled(&prefix),
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

	/// Take one event to deliver to the consumer, if any: the marker as soon as
	/// it is due, otherwise the next update.
	fn take(&mut self) -> Option<AnnounceEvent> {
		if self.live_due() {
			self.live = LiveState::Done;
			return Some(AnnounceEvent::Live);
		}
		let prefix = self.pending.keys().next()?.clone();
		let ((meta, captures), kind): (_, fn(Announce) -> AnnounceEvent) = match self.pending.remove(&prefix).unwrap() {
			PendingUpdate::Announce(meta) => {
				// The consumer has seen this prefix before, so it is a metadata update.
				let kind = match self.delivered.insert(prefix.clone()) {
					true => AnnounceEvent::Announced,
					false => AnnounceEvent::Updated,
				};
				(meta, kind)
			}
			PendingUpdate::Unannounce(meta) => {
				self.delivered.remove(&prefix);
				(meta, AnnounceEvent::Retracted)
			}
			PendingUpdate::UnannounceAnnounce { old, new } => {
				// Deliver the retraction now; leave the trailing announce pending so
				// the next take returns it for the same prefix.
				self.delivered.remove(&prefix);
				self.pending.insert(prefix.clone(), PendingUpdate::Announce(new));
				(old, AnnounceEvent::Retracted)
			}
		};
		self.settled(&prefix);
		Some(kind(Announce {
			prefix,
			captures,
			route: Route {
				hops: meta.0,
				cost: meta.1,
				via: Hop::UNKNOWN,
				source: meta.2,
			},
		}))
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
	/// Whether this is a broadcast published on this origin: a front that
	/// starts from one only fails over to another local publisher.
	local: bool,
	/// Whether a handle marked [`Producer::peer`] inserted the entry, so it
	/// entered from a cluster peer rather than here.
	peer: bool,
	/// The queue requests under this route are served from, when the announcer
	/// serves content on demand (a [`Dynamic`]). `None` for an advertise-only
	/// announcement ([`Producer::announce`]) and for a local broadcast.
	server: Option<kio::Shared<ServeState>>,
	/// The broadcast published on this origin at exactly `prefix`, when the
	/// entry is one: requests resolve to it directly, and the newest one at a
	/// path wins through [`route_order`].
	source: Option<broadcast::Consumer>,
	/// Whether the entry exists for anyone: cursors see it and requests resolve
	/// through it. A broadcast is in the table from creation but serves nobody,
	/// locally or remotely, until it announces.
	advertised: bool,
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

	/// Where the entry entered this origin.
	fn entered(&self) -> Source {
		match self.peer {
			true => Source::Peer(self.via),
			false => Source::Local,
		}
	}

	/// Whether a request for `path` can be served through this entry. A served
	/// route covers everything beneath its prefix; a broadcast published here
	/// is only itself, so it serves its exact path and shadows what is beneath.
	fn serves(&self, path: &Path) -> bool {
		self.server.is_some() || (self.source.is_some() && self.prefix == *path)
	}

	/// Whether `pin` admits this entry for a front's selection.
	fn qualifies(&self, pin: Pin) -> bool {
		match pin {
			Pin::Any => true,
			Pin::Local => self.local,
			Pin::Publisher(first) => self.hops.iter().next() == Some(&first),
			Pin::Route(id) => self.id == id,
		}
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
/// [`Horizon`]. Requesters excluding different peers get separate fronts, so a
/// front's failover never adopts a route flowing back through one of its own
/// readers, nor a local view a peer's route.
type FrontKey = (PathOwned, Horizon);

/// Which routes a reader sees: the split-horizon exclusion and the local-only view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Horizon {
	/// Routes whose hop chain or announcing session (`via`) is this peer are
	/// hidden. `Some(UNKNOWN)` marks an anonymous peer: no hop is excluded, but
	/// local-only broadcasts are not advertised. `None` is a local reader.
	exclude: Option<Hop>,
	/// Hide the routes that entered from a cluster peer ([`Consumer::local`]).
	local: bool,
}

impl Horizon {
	/// Whether `entry` may be observed or served through this horizon.
	fn admits(&self, entry: &RouteEntry) -> bool {
		!(self.local && entry.peer) && entry.visible_to(self.exclude)
	}
}

/// One remotely-served front in [`OriginState::fronts`]: the shared spliced
/// broadcast at a path plus the channel requesters resolve through.
#[derive(Clone)]
struct RemoteFront {
	/// Resolves requesters with the front's consumer (or the error that ended it
	/// unresolved). The producer lives here so the teardown can reject requesters
	/// still parked on a front whose watcher was cancelled.
	request: kio::Producer<PendingBroadcast>,
	/// The front's spliced broadcast, weak: dead once the front ends, so a
	/// later request re-creates the front instead of joining a corpse.
	broadcast: broadcast::WeakConsumer,
	/// Which routes serve the front's content, fixed by its first source. A
	/// request joins the front only while the best route is one of them.
	pin: kio::Lock<Pin>,
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
	/// Where the cursor hangs in the [`RouteTable`]: the literal heads of
	/// `allowed`. A route the cursor can see sits at or under one of them, or on
	/// the walk down to one.
	heads: Vec<PathOwned>,
	/// The routes this cursor may see (control-plane split horizon).
	horizon: Horizon,
	/// Which routes beneath a hidden segment are reported.
	hidden: Hidden,
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

	/// Whether this cursor may observe `entry` at all: advertised, not behind
	/// the excluded peer (split horizon), and within the cursor's patterns.
	fn visible(&self, entry: &RouteEntry) -> bool {
		entry.advertised && self.horizon.admits(entry) && entry.overlaps(&self.allowed) && self.discovers(&entry.prefix)
	}

	/// Whether the hidden rule lets this cursor discover a route at `prefix`.
	fn discovers(&self, prefix: &Path) -> bool {
		(self.hidden.include || !hides(&self.heads, prefix))
			&& self.hidden.beyond.as_ref().is_none_or(|outer| hides(outer, prefix))
	}
}

/// A handle's view of an origin: the absolute patterns it may reach.
#[derive(Clone)]
struct OriginScope {
	// The paths this handle may reach, absolute.
	allowed: Patterns,
}

impl OriginScope {
	/// A view that reaches nothing.
	fn empty() -> Self {
		Self {
			allowed: Patterns::new(),
		}
	}

	/// This view narrowed to the absolute `patterns`: the paths in both.
	fn narrow(&self, patterns: &Patterns) -> Option<Self> {
		let allowed = self.allowed.intersect(patterns).ok()?;
		if allowed.is_empty() {
			None
		} else {
			Some(Self { allowed })
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

/// Which routes beneath a hidden segment an announce cursor reports.
///
/// A route is hidden when a segment below the cursor's requested prefix (its
/// interest head) starts with `.`; a prefix that names the dot segment itself
/// lists what is under it. Only discovery is affected: a request by exact path
/// resolves either way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Hidden {
	/// Report hidden routes too.
	include: bool,
	/// Report only what a feed scoped to these heads hides, for a stream that tops
	/// up a feed already carrying everything visible from them.
	beyond: Option<Vec<PathOwned>>,
}

/// Whether a segment of `prefix` below the head it sits under starts with `.`.
/// A route at or above a head has nothing below it, so it never hides.
fn hides(heads: &[PathOwned], prefix: &Path) -> bool {
	heads
		.iter()
		.any(|head| prefix.strip_prefix(head).is_some_and(|below| below.is_hidden()))
}

/// What an [`AnnounceConsumer`] yields.
#[derive(Clone, Debug)]
pub enum AnnounceEvent {
	/// A route now covers the prefix; the cursor had none there.
	Announced(Announce),
	/// The route covering the prefix changed hops or cost; it is delivered in place.
	Updated(Announce),
	/// No route covers the prefix any more. Carries its last advertised route.
	Retracted(Announce),
	/// Every route live at subscribe time has been delivered; what follows is
	/// live changes. Yielded once, and only after each remote source feeding the
	/// origin at subscribe time has landed its initial set.
	Live,
}

/// A route over a prefix, delivered by [`AnnounceConsumer`] inside an [`AnnounceEvent`].
///
/// An announcement is always a prefix, never a broadcast: it advertises that
/// [`prefix`](Self::prefix) and every path beneath it are servable. A broadcast
/// announces its own path, so the prefix usually names one, but resolve it with
/// [`Consumer::request_broadcast`]; the application decides which paths name
/// broadcasts, and filters with a [`Pattern`] locally when it wants a subset.
#[derive(Clone, Debug)]
pub struct Announce {
	/// The prefix the route covers, relative to the consuming cursor's root.
	pub prefix: PathOwned,
	/// What the scope's wildcards stood for when the announced prefix pins all of
	/// them. `None` for an overlap-only route or a scope without a complete match.
	pub captures: Option<Vec<Pattern>>,
	/// The route serving the prefix. On a retraction this carries its last
	/// advertised metadata.
	pub route: Route,
}

/// Publishes broadcasts and announces routes into an origin.
#[derive(Clone)]
pub struct Producer {
	// Identity for this origin. Appended to route hops when re-announcing so
	// downstream relays can detect loops and prefer the shortest path.
	hop: Hop,

	// The absolute patterns this handle may publish under.
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

	// Ingress stats context. Broadcasts created through this producer are attributed
	// to it (writes counted on the subscriber/ingress side). Empty (no-op) unless a
	// session tagged this handle via [`Self::with_stats`].
	stats: stats::Session,

	// Whether routes announced through this handle entered from a cluster peer
	// (see [`Self::peer`]).
	peer: bool,

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
			stats: stats::Session::default(),
			peer: false,
			tasks,
			timers: timers.clone(),
		};
		let driver = Driver {
			state: DriverState {
				set,
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

	/// Mark this handle (and any handle derived from it) as a cluster peer's:
	/// every route it announces reports [`Source::Peer`], and
	/// [`Consumer::local`] hides it.
	///
	/// Hand it to a session with another relay, so this origin can tell what
	/// entered here from what a peer forwarded. The hop chain cannot: a client
	/// and a peer each append one hop.
	pub fn peer(mut self) -> Self {
		self.peer = true;
		self
	}

	/// This origin's construction config.
	pub fn config(&self) -> Config {
		Config {
			hop: self.hop,
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
		}
	}

	/// This origin's hop identity.
	pub fn hop(&self) -> Hop {
		self.hop
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
			stats: stats::Session::default(),
			peer: false,
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
	/// The broadcast exists for nobody until [`broadcast::Producer::announce`]:
	/// until then no announce cursor lists it and a request for its path fails
	/// with [`Error::Unroutable`], for a consumer of this origin exactly as for a
	/// peer. Announce once the tracks a subscriber needs first exist. To serve
	/// paths on demand without publishing each one, use [`Self::dynamic`].
	///
	/// Announcing is visible to local consumers before it returns; only
	/// lifecycle work (track serving, teardown) waits for the [`Driver`] to be
	/// polled. Register a [`broadcast::Producer::dynamic`] handler before
	/// announcing, so the first consumer finds the tracks it serves.
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
	/// [`Path::MAX_PARTS`], [`Error::InvalidPath`] if it holds a segment no
	/// pattern can spell (`*` or `**`), or [`Error::Closed`] once the origin's
	/// [`Driver`] has been dropped.
	pub fn create_broadcast(&self, path: impl AsPath) -> Result<broadcast::Producer, Error> {
		let path = path.as_path();

		let full = self.root.join(&path).to_owned();
		if !self.scope.permits(&full) {
			return Err(Error::Unauthorized);
		}
		// A decoded prefix and suffix are each within the wire limit, but their
		// join might not be. Enforcing here bounds the table depth and guarantees the
		// path can be re-encoded when forwarded.
		if full.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}
		// A path only a pattern could spell (a `*` segment) advertises nowhere, so
		// refuse it here rather than publish a broadcast no cursor can see.
		let claim = prefix_claim(&full)?;

		// Resolve the ingress counters once, keyed by the absolute broadcast path.
		let ingress = self.stats.ingress(&full);

		// The broadcast is a route table entry at its exact path from the start,
		// hidden from cursors and requests until it announces. The entry lives
		// as long as the broadcast: its announcer drops on finish, abort, or the
		// last handle.
		let announcing = Announcing {
			hop: self.hop,
			shared: self.shared.clone(),
			requested: full.clone(),
			prefixes: vec![(full.clone(), claim)],
			scope: self.scope.allowed.clone(),
			local: true,
			peer: self.peer,
			stats: self.stats.clone(),
		};
		let info = broadcast::Info {
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			path: full,
		};
		let source = info.produce().with_stats(ingress.clone());
		let entry = announcing.announce(
			Route::default(),
			Serving {
				server: None,
				source: Some(source.consume()),
				advertised: false,
			},
		)?;
		Ok(source.with_announcer(Announcer {
			entry,
			ingress,
			_keepalive: self.tasks.keepalive(),
		}))
	}

	/// Create and advertise a broadcast in one call.
	pub fn publish(&self, path: impl AsPath, route: Route) -> Result<broadcast::Producer, Error> {
		let broadcast = self.create_broadcast(path)?;
		broadcast.announce(route)?;
		Ok(broadcast)
	}

	/// Mint a standalone source broadcast for a served-route request: it carries
	/// this origin's cache policy and ingress attribution, but
	/// is *not* entered into the route table. Sessions answer
	/// [`Dynamic`] requests with one of these; the requester already holds
	/// the request's result channel, so the table never needs to resolve it.
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
	/// unless an announced broadcast or a served route ([`Self::dynamic`]) covers
	/// the path too. Tests use it to shape the route table; everything else
	/// advertises through a broadcast ([`broadcast::Producer::announce`]) or a
	/// [`Dynamic`] handler, which serve what they claim.
	#[cfg(test)]
	pub(crate) fn announce(&self, prefix: impl AsPath, route: Route) -> Result<AnnounceProducer, Error> {
		Announcing::new(self, prefix)?.announce(
			route,
			Serving {
				server: None,
				source: None,
				advertised: true,
			},
		)
	}

	/// Register a remote announce source replaying its initial set under
	/// `prefix`, relative to this producer's root.
	///
	/// Every announce cursor overlapping that prefix and registered while the
	/// guard lives withholds its [`AnnounceEvent::Live`] marker until the guard
	/// drops, so a session drops it once the peer's initial routes are in the
	/// table (or the session dies).
	pub(crate) fn replaying(&self, prefix: impl AsPath) -> Replaying {
		// A subtree too complex to intersect waits on the whole scope instead.
		let scope = Pattern::subtree(self.absolute(prefix).as_str())
			.ok()
			.and_then(|subtree| self.scope.allowed.intersect(&subtree.into()).ok())
			.unwrap_or_else(|| self.scope.allowed.clone());
		let mut state = self.shared.lock();
		let id = state.next_replaying;
		state.next_replaying += 1;
		state.replaying.insert(
			id,
			ReplayingSource {
				scope,
				waiting: Vec::new(),
			},
		);
		Replaying {
			shared: self.shared.clone(),
			id,
		}
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
	/// A consumer resolving a path under it through this route is handed to the
	/// handler as a [`Request`] to materialize on demand. This is how a service
	/// answers a whole subtree without publishing each path, and how sessions
	/// land the routes a peer announces to them; a publisher that
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
		let announcement = announcing.announce(
			route,
			Serving {
				server: Some(serve.clone()),
				source: None,
				advertised: true,
			},
		)?;
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
			stats: self.stats.clone(),
			peer: self.peer,
			tasks: self.tasks.clone(),
			timers: self.timers.clone(),
		})
	}

	/// Cheap read handle over this origin's route table.
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
	/// Whether the producer was marked [`Producer::peer`].
	peer: bool,
	stats: stats::Session,
}

impl Announcing {
	/// The requested prefix as-is, refused when its subtree does not overlap the scope.
	fn new(producer: &Producer, prefix: impl AsPath) -> Result<Self, Error> {
		let requested = producer.root.join(prefix.as_path()).to_owned();
		if requested.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}
		let claim = prefix_claim(&requested)?;
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
			peer: producer.peer,
			stats: producer.stats.clone(),
		})
	}

	fn announce(&self, route: Route, serving: Serving) -> Result<AnnounceProducer, Error> {
		debug_assert!(
			!route.hops.contains(&self.hop),
			"announce called with a looping hop chain",
		);

		let via = route.via;

		let mut shared = self.shared.lock();
		if shared.closed {
			return Err(Error::Closed);
		}

		let mut entries = Vec::with_capacity(self.prefixes.len());
		for (prefix, claim) in &self.prefixes {
			let id = shared.next_route;
			shared.next_route += 1;
			shared.routes.insert(RouteEntry {
				id,
				prefix: prefix.clone(),
				scope: self.scope.clone(),
				hops: route.hops.clone(),
				cost: route.cost,
				via,
				local: self.local,
				peer: self.peer,
				server: serving.server.clone(),
				source: serving.source.clone(),
				advertised: serving.advertised,
				claim: claim.clone(),
			});
			shared.sync_route(prefix, claim);
			entries.push((prefix.clone(), id));
		}
		drop(shared);

		// Ingress announce guard: held while the route is advertised.
		let guard = serving
			.advertised
			.then(|| self.stats.ingress(&self.requested).announce());

		Ok(AnnounceProducer {
			shared: self.shared.clone(),
			entries,
			guard,
		})
	}
}

/// What a route entry serves and whether cursors see it.
struct Serving {
	server: Option<kio::Shared<ServeState>>,
	source: Option<broadcast::Consumer>,
	advertised: bool,
}

/// The table entry a broadcast owns: its exact path, advertised and withdrawn
/// through [`broadcast::Producer::announce`] and
/// [`broadcast::Producer::unannounce`], and removed when the broadcast ends.
///
/// Handed to the broadcast by [`Producer::create_broadcast`], so a standalone
/// broadcast has none and cannot announce.
pub(crate) struct Announcer {
	entry: AnnounceProducer,
	/// The ingress counters an advertised interval's announce guard comes from.
	ingress: stats::Scope,
	/// A published broadcast is lifecycle work: the origin's driver keeps
	/// running for as long as one lives, even once every producer handle is
	/// gone, so a session handed a producer can drop it and keep serving.
	_keepalive: Keepalive,
}

impl Announcer {
	/// Advertise the broadcast's path with `route`, or re-price it in place.
	pub(crate) fn announce(&mut self, route: Route) -> Result<(), Error> {
		self.entry.update(route)?;
		if self.entry.guard.is_none() {
			self.entry.guard = Some(self.ingress.announce());
		}
		Ok(())
	}

	/// Withdraw the advertisement from local and remote consumers alike.
	pub(crate) fn withdraw(&mut self) {
		self.entry.withdraw();
		self.entry.guard = None;
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
	/// The table entries this advertisement created, by prefix and id. A prefix
	/// remains unchanged; pattern scopes only filter its visibility and requests.
	entries: Vec<(PathOwned, u64)>,
	/// Ingress announce stats guard, held only while the entries are advertised.
	guard: Option<stats::Announce>,
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
		for (prefix, id) in &self.entries {
			// Each entry keeps its advertised prefix; only the metadata moves.
			let Some(entry) = shared.routes.entry_mut(prefix, *id) else {
				return Err(Error::Closed);
			};
			entry.hops = route.hops.clone();
			entry.cost = route.cost;
			entry.via = route.via;
			entry.advertised = true;
			let claim = entry.claim.clone();
			shared.sync_route(prefix, &claim);
		}
		Ok(())
	}

	/// Hide the entries from everyone, local and remote alike: cursors see a
	/// retraction and requests stop resolving through them. The entries stay,
	/// so announcing again restores the same route. What
	/// [`broadcast::Producer::unannounce`] does.
	fn withdraw(&self) {
		let mut shared = self.shared.lock();
		for (prefix, id) in &self.entries {
			let Some(entry) = shared.routes.entry_mut(prefix, *id) else {
				continue;
			};
			if !entry.advertised {
				continue;
			}
			entry.advertised = false;
			let claim = entry.claim.clone();
			shared.sync_route(prefix, &claim);
		}
	}

	/// Retract the route now: remove its table entries and reject anything still
	/// waiting on its queue. Idempotent, and what dropping the advertisement does.
	fn retract(&self) {
		let mut shared = self.shared.lock();
		for (prefix, id) in &self.entries {
			let Some(entry) = shared.routes.remove(prefix, *id) else {
				continue;
			};
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
/// failover, and teardown run here; the route table and announce cursors update
/// synchronously when a route is announced or retracted.
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
	/// The front drivers: producers submit, this polls.
	set: TaskSet,
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
		// Cancel queued and running lifecycle work first, so no front serves
		// while the table is ended below.
		drop(std::mem::replace(&mut self.set, TaskSet::owned()));

		// Refuse new work and take the pending requests, under the same lock
		// `create_broadcast` holds across its attach: a concurrent create either
		// finishes before this (the walk below cleans its entry up) or observes
		// `closed` and fails with `Closed`.
		let (servers, cursors, fronts) = {
			let mut shared = self.shared.lock();
			shared.closed = true;
			// Fronts and parked requesters observe `closed` on their next pass.
			shared.routes.poke_all();
			let servers: Vec<_> = shared
				.routes
				.entries()
				.filter_map(|entry| entry.server.clone())
				.collect();
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
	}
}

impl Drop for DriverState {
	fn drop(&mut self) {
		self.teardown();
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

/// A local copy of groups the front already delivered, so resume stays spliced
/// after the source track is dropped. Cache misses stay pending while demand
/// re-splices the upstream source. Finished on drop so an idle linger does not
/// warn about an abandoned producer.
struct WarmCopy {
	track: track::Producer,
	_dynamic: track::Dynamic,
	/// The newest group, where the copy spliced after this one picks up (see
	/// [`TrackIo::head`]).
	edge: Option<WarmGroup>,
}

impl Drop for WarmCopy {
	fn drop(&mut self) {
		let _ = self.track.finish();
	}
}

/// A warm copy's newest group, which the copy spliced after it continues.
///
/// Kept past that splice: an open one must stay open for the continuation (dropping
/// an unfinished producer clears its frames), and either kind supplies the head the
/// continuation lacks when the next park rebuilds the group. Nothing will ever finish
/// an open one, so it aborts on drop.
struct WarmGroup(group::Producer);

impl Drop for WarmGroup {
	fn drop(&mut self) {
		if !self.0.is_finished() {
			let _ = self.0.clone().abort(Error::Cancel);
		}
	}
}

/// Cache what `source` delivered on a new local track the origin owns: its complete
/// groups, and its open live edge rebuilt from the frames already delivered.
///
/// `head` is the previous park's edge. A copy spliced after a warm cache continues its
/// edge group from the next frame, so its copy of that group lacks the head, which
/// `head` supplies.
fn warm_copy(source: &track::Consumer, head: Option<&WarmGroup>) -> Option<WarmCopy> {
	let info = source.cached_info()?;
	let mut track = track::Producer::new(Arc::new(source.broadcast().clone()), source.name(), info);
	let head = head.map(|head| &head.0);
	let groups = source.cached_groups();
	// Not `source.latest()`: datagrams share the sequence counter and can run past it.
	let latest = groups
		.iter()
		.filter(|(_, visible)| *visible)
		.map(|(group, _)| group.sequence)
		.max();
	let mut edge = None;

	// A spliced copy hides the group it continued (its halves sit in two segments), so
	// carry the previous edge over when the copy has no version of it at all. First,
	// since it arrived before anything the copy holds.
	if let Some(head) = head
		&& !groups.iter().any(|(group, _)| group.sequence == head.sequence)
	{
		let is_latest = latest.is_none_or(|latest| head.sequence >= latest);
		if head.is_finished() {
			let _ = track.adopt_group(head.clone(), true);
			if is_latest {
				edge = Some(WarmGroup(head.clone()));
			}
		} else if is_latest {
			edge = warm_rebuild(&track, head, None);
		}
	}

	for (group, visible) in groups {
		let finished = group.is_finished();
		let whole = group.live_first_frame() == Some(0);
		let is_latest = visible && Some(group.sequence) == latest;
		let warm = if finished && whole {
			let _ = track.adopt_group(group.clone(), visible);
			Some(WarmGroup(group))
		} else if (finished || is_latest) && (whole || head.is_some_and(|head| head.sequence == group.sequence)) {
			// Dropping the source copy resets an open live edge mid-transfer, so rebuild
			// it from the frames already delivered: the re-splice asks for the next
			// frame, and a group that stays open for good (a JSON log in group 0)
			// continues instead of being re-sent whole on every resume. A continuation
			// is rebuilt whole from the previous edge's head the same way.
			warm_rebuild(&track, &group, head)
		} else {
			// Mid-transfer backlog, or a continuation with no head to complete it.
			None
		};
		if is_latest {
			edge = warm;
		}
	}
	let dynamic = track.dynamic();
	Some(WarmCopy {
		track,
		_dynamic: dynamic,
		edge,
	})
}

/// Rebuild `live` on `track` from its delivered frames, prefixed by `head`'s when `live`
/// only holds a continuation of it. Finished like `live`, or left open.
fn warm_rebuild(track: &track::Producer, live: &group::Producer, head: Option<&group::Producer>) -> Option<WarmGroup> {
	// A continuation holds nothing below its offset.
	let mut start = live.live_first_frame()? as u64;
	let mut tail = live.consume();
	tail.start_at(start);
	let mut frames = Vec::new();
	if start > 0
		&& let Some(head) = head.filter(|head| head.sequence == live.sequence)
		&& let Some(head_start) = head.live_first_frame()
	{
		let mut head = head.consume();
		head.start_at(head_start as u64);
		while head.index() < start {
			match head.poll_read_frame(&kio::Waiter::noop()) {
				Poll::Ready(Ok(Some(frame))) => frames.push(frame),
				_ => break,
			}
		}
		match head.index() == start {
			true => start = head_start as u64,
			// The head doesn't reach the continuation: keep only the continuation.
			false => frames.clear(),
		}
	}
	while let Poll::Ready(Ok(Some(frame))) = tail.poll_read_frame(&kio::Waiter::noop()) {
		frames.push(frame);
	}
	if frames.is_empty() {
		return None;
	}

	// Wrapped first, so a failed write aborts it rather than dropping it unfinished.
	let rebuilt = WarmGroup(
		track
			.create_group(group::Info {
				sequence: live.sequence,
			})
			.ok()?,
	);
	let mut writer = rebuilt.0.clone();
	if start > 0 {
		writer.start_at(start).ok()?;
	}
	for frame in frames {
		writer.write_frame(frame.timestamp, frame.payload).ok()?;
	}
	if live.is_finished() {
		writer.finish().ok()?;
	}
	Some(rebuilt)
}

/// Everything [`run_front`] owns, queued by [`Consumer::request_broadcast`].
struct FrontTask {
	/// The route table the front selects from.
	shared: kio::Shared<OriginState>,
	/// The spliced broadcast the front serves.
	broadcast: broadcast::Producer,
	/// Absolute path of the front.
	path: PathOwned,
	/// The requesters' horizon, applied to every (re)selection.
	horizon: Horizon,
	/// Wakes the front when a route covering its path changes.
	watch: Watch,
	/// Resolves the requesters parked on the front's channel.
	request: kio::Producer<PendingBroadcast>,
	/// Published for requesters once the first source fixes it; see [`RemoteFront::pin`].
	pin: kio::Lock<Pin>,
	timers: Clock,
}

/// The driver's side of one logical track: the handles behind the names the
/// machine uses.
struct TrackIo {
	resume: super::resume::Producer,
	/// The copy whose info resolved, waiting for the machine to splice it.
	staged: Option<(u64, track::Consumer)>,
	/// A query in flight: the source asked, its copy, and the pending info.
	query: Option<(u64, track::Consumer, track::Querying)>,
	/// The spliced copy: its source and the track.
	copy: Option<(u64, track::Consumer)>,
	/// The delivered edge when the copy spliced in: a copy that dies without
	/// advancing it delivered nothing. Snapshotted per splice, not per wake, so an
	/// unrelated wake between the copy's last frame and its death cannot launder
	/// its progress away.
	edge: Option<track::Position>,
	/// Delivered groups kept after the copy was dropped, so resume stays spliced
	/// through the linger without pinning the source as a reader.
	warm: Option<WarmCopy>,
	/// The last warm copy's newest group, outliving it so the copy spliced after it
	/// can continue the group (see [`WarmGroup`]). Released at the next park.
	head: Option<WarmGroup>,
	/// Whether the track had a reader as of the last demand edge.
	used: bool,
}

/// Drives one front: feeds the world's events to a [`Front`] and performs the
/// actions it returns, until the front ends. The decisions live in the machine;
/// this only waits and executes, so nothing here decides anything twice.
async fn run_front(task: FrontTask) {
	let FrontTask {
		shared,
		broadcast,
		path,
		horizon,
		watch,
		request,
		pin,
		timers,
	} = task;

	/// What the wait below returns: one thing that happened.
	enum Step {
		Assigned(Arc<str>, super::resume::Producer),
		Resolved(u64, Result<broadcast::Consumer, Error>),
		SourceClosed(u64),
		Info(Arc<str>, u64, Result<track::Info, Error>),
		Ended(Arc<str>, u64, Result<(), Error>),
		Demand(Arc<str>),
		Deadline,
		Table,
	}

	let mut front = Front::new(TRACK_IDLE_LINGER);
	let mut sources: HashMap<u64, broadcast::Consumer> = HashMap::new();
	let mut next_source = 0u64;
	// The in-flight upstream request: the route and its pending channel.
	let mut upstream: Option<(u64, kio::Consumer<PendingBroadcast>)> = None;
	let mut tracks: HashMap<Arc<str>, TrackIo> = HashMap::new();
	let mut deadline = crate::runtime::Deadline::new(&timers);
	// The watch generation the last selection saw.
	let mut seen = 0;
	let mut events: VecDeque<Event> = VecDeque::new();

	// Read the table for the machine: the best qualifying route and whether
	// the serving source is on its way out. Also what the watch wakes for.
	let select = |front: &mut Front, sources: &HashMap<u64, broadcast::Consumer>, seen: &mut u64| -> Event {
		let table = shared.read();
		if table.closed {
			return Event::Closed;
		}
		// Read alongside the decision, under the lock a poke takes first.
		*seen = watch.seen();
		front.retain_routes(|route| table.routes.covers(&path.as_path(), route));
		let best = table
			.best_route(&path.as_path(), horizon, front.pin(), front.refused_routes())
			.map(|entry| Candidate {
				route: entry.id,
				first: entry.hops.iter().next().copied(),
				local: entry.local,
			});
		let serving_closing = front
			.serving()
			.and_then(|id| sources.get(&id))
			.is_some_and(|source| source.is_closing());
		Event::Selected { best, serving_closing }
	};

	events.push_back(select(&mut front, &sources, &mut seen));

	loop {
		while let Some(event) = events.pop_front() {
			for action in front.step(event) {
				match action {
					Action::Reselect => events.push_back(select(&mut front, &sources, &mut seen)),
					Action::Request { route } => {
						// The entry, its identity for the front, and what it serves.
						let found = {
							let table = shared.read();
							table
								.routes
								.covering(&path.as_path())
								.find(|entry| entry.id == route)
								.map(|entry| {
									(
										Candidate {
											route,
											first: entry.hops.iter().next().copied(),
											local: entry.local,
										},
										entry.source.clone(),
										entry.server.clone(),
									)
								})
						};
						let Some((candidate, source, server)) = found else {
							events.push_back(Event::Resolved {
								route,
								result: Err(Refusal {
									err: Error::Unroutable,
									standing: false,
								}),
							});
							continue;
						};
						front.identify(candidate);
						*pin.lock() = front.pin();
						if let Some(source) = source {
							let id = next_source;
							next_source += 1;
							sources.insert(id, source);
							events.push_back(Event::Resolved { route, result: Ok(id) });
							continue;
						}
						let Some(server) = server else {
							events.push_back(Event::Resolved {
								route,
								result: Err(Refusal {
									err: Error::Unroutable,
									standing: true,
								}),
							});
							continue;
						};
						let mut serve = server.lock();
						if serve.closed {
							// Retracted under us, or its handler dropped while the
							// announcement stands: it cannot serve.
							drop(serve);
							events.push_back(Event::Resolved {
								route,
								result: Err(Refusal {
									err: Error::Unroutable,
									standing: true,
								}),
							});
							continue;
						}
						// A source this route already materialized for the path
						// attaches without another upstream round trip.
						if let Some(weak) = serve.served.get(&path) {
							drop(serve);
							let id = next_source;
							next_source += 1;
							sources.insert(id, weak.consume());
							events.push_back(Event::Resolved { route, result: Ok(id) });
							continue;
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
										drop(serve);
										events.push_back(Event::Resolved {
											route,
											result: Err(Refusal {
												err: Error::Unroutable,
												standing: true,
											}),
										});
										continue;
									}
								}
							}
						};
						upstream = Some((route, pending));
					}
					Action::Detach { source } => {
						sources.remove(&source);
						// Its copies go with it; the segments they delivered stay
						// spliced until a replacement resumes past them.
						for io in tracks.values_mut() {
							if io.copy.as_ref().is_some_and(|(s, _)| *s == source) {
								io.copy = None;
							}
							if io.query.as_ref().is_some_and(|(s, ..)| *s == source) {
								io.query = None;
							}
							if io.staged.as_ref().is_some_and(|(s, _)| *s == source) {
								io.staged = None;
							}
						}
					}
					Action::Resolve => {
						if let Ok(mut pending) = request.write() {
							pending.resolved.get_or_insert(Ok(broadcast.consume()));
						}
					}
					Action::Query { track: name, source } => {
						let Some(io) = tracks.get_mut(&name) else { continue };
						let closing = sources.get(&source).is_some_and(|s| s.is_closing());
						match sources.get(&source).map(|s| s.track(&name)) {
							Some(Ok(copy)) => {
								// `into_inner` sheds the `Pending` future wrapper so only
								// the pollable (which is `Sync`) is held across the wait.
								let query = copy.query().into_inner();
								io.query = Some((source, copy, query));
							}
							Some(Err(err)) => events.push_back(Event::TrackInfo {
								track: name,
								source,
								closing,
								result: Err(err),
							}),
							None => {}
						}
					}
					Action::Splice { track: name, source } => {
						let Some(io) = tracks.get_mut(&name) else { continue };
						let Some((staged, copy)) = io.staged.take() else {
							continue;
						};
						if staged != source {
							continue;
						}
						if let Err(err) = io.resume.takeover(&copy) {
							// Closed means the logical track already ended. Anything
							// else is a boundary bug; abort rather than strand
							// subscribers on a track nobody serves.
							let _ = io.resume.abort(err);
							tracks.remove(&name);
							continue;
						}
						io.head = io.warm.take().and_then(|mut warm| warm.edge.take());
						// The new segment has produced nothing yet: this is the
						// edge the copy is asked to advance.
						io.edge = io.resume.resume_position();
						io.copy = Some((source, copy));
					}
					Action::Park { track: name } => {
						let Some(io) = tracks.get_mut(&name) else { continue };
						let Some((_, copy)) = io.copy.take() else { continue };
						// Drop the source copy so its producer goes idle at once; keep
						// the groups it delivered on a local track so resume stays
						// spliced until the linger expires.
						let warm = warm_copy(&copy, io.head.as_ref());
						drop(copy);
						io.head = None;
						let parked = match &warm {
							Some(warm) => io.resume.park(&warm.track),
							None => io.resume.release(),
						};
						if parked.is_err() {
							tracks.remove(&name);
							continue;
						}
						io.warm = warm;
					}
					Action::Release { track: name } => {
						let Some(io) = tracks.get_mut(&name) else { continue };
						// A local source releases straight from the spliced copy.
						io.copy = None;
						io.warm = None;
						io.head = None;
						if io.resume.release().is_err() {
							tracks.remove(&name);
						}
					}
					Action::Finish { track: name } => {
						if let Some(mut io) = tracks.remove(&name) {
							let _ = io.resume.finish();
						}
					}
					Action::Abort { track: name, err } => {
						if let Some(mut io) = tracks.remove(&name) {
							tracing::debug!(name = %name, %err, "aborting track");
							let _ = io.resume.abort(err);
						}
					}
					Action::Arm { at } => deadline.set(at),
					Action::End { err } => {
						if let Ok(mut pending) = request.write() {
							pending.resolved.get_or_insert(Err(err.clone()));
						}
						// Ending the broadcast only retracts it: no new requesters or
						// tracks, and a newcomer at the path gets a fresh front. Tracks
						// in flight carry on (moq-lite: retraction does not disturb
						// subscriptions already in flight): dropping their producers
						// leaves each reader on the copy it was spliced from, ending
						// when and as that copy ends.
						broadcast.finish();
						broadcast.release_spliced(err.clone());
						for (_, mut io) in tracks.drain() {
							// A reader still waiting on its source's answer is in flight
							// too: splice the copy it asked, past any warm cache, so it
							// ends as that copy does.
							let waiting = io.staged.take().map(|(_, copy)| copy);
							let waiting = waiting.or_else(|| io.query.take().map(|(_, copy, _)| copy));
							if let Some(copy) = waiting
								&& io.resume.is_used()
							{
								if io.resume.takeover(&copy).is_err() {
									continue;
								}
								io.warm = None;
							}
							// Nothing in flight: unread, never spliced, or only a warm cache.
							if !io.resume.is_used() || !io.resume.is_spliced() || io.warm.is_some() {
								let _ = io.resume.abort(err.clone());
							}
						}
						return;
					}
				}
			}
		}

		let step = kio::wait(|waiter| {
			if let Poll::Ready((name, resume)) = broadcast.poll_spliced_assigned(waiter) {
				return Poll::Ready(Step::Assigned(name, resume));
			}
			if let Some((route, pending)) = &upstream
				&& let Poll::Ready(result) = pending.poll(waiter, |p| match &p.resolved {
					Some(result) => Poll::Ready(result.clone()),
					None => Poll::Pending,
				}) {
				return Poll::Ready(Step::Resolved(
					*route,
					match result {
						Ok(resolved) => resolved,
						// The queue died unresolved (its handler dropped): the route
						// could not serve.
						Err(_closed) => Err(Error::Unroutable),
					},
				));
			}
			if let Some(id) = front.serving()
				&& let Some(source) = sources.get(&id)
				&& source.poll_closed(waiter).is_ready()
			{
				return Poll::Ready(Step::SourceClosed(id));
			}
			for (name, io) in &mut tracks {
				if let Some((source, _, query)) = &mut io.query
					&& let Poll::Ready(result) = query.poll(waiter)
				{
					return Poll::Ready(Step::Info(name.clone(), *source, result));
				}
				if let Some((source, copy)) = &io.copy
					&& let Poll::Ready(result) = copy.poll_complete(waiter)
				{
					return Poll::Ready(Step::Ended(name.clone(), *source, result));
				}
				// Watch the demand edge in whichever direction is unmet.
				let edge = match io.used {
					true => io.resume.poll_unused(waiter),
					false => io.resume.poll_used(waiter),
				};
				if edge.is_ready() {
					return Poll::Ready(Step::Demand(name.clone()));
				}
			}
			if deadline.poll(waiter).is_ready() {
				return Poll::Ready(Step::Deadline);
			}
			watch.poll_changed(waiter, seen).map(|()| Step::Table)
		})
		.await;

		let event = match step {
			Step::Assigned(name, resume) => {
				tracks.insert(
					name.clone(),
					TrackIo {
						resume,
						staged: None,
						query: None,
						copy: None,
						edge: None,
						warm: None,
						head: None,
						used: false,
					},
				);
				Event::TrackAssigned { track: name }
			}
			Step::Resolved(route, result) => {
				upstream = None;
				match result {
					Ok(source) => {
						let id = next_source;
						next_source += 1;
						sources.insert(id, source);
						Event::Resolved { route, result: Ok(id) }
					}
					Err(err) => {
						// A retraction and a handler's rejection resolve alike, so
						// the table tells them apart: an `Unroutable` from a route
						// that still stands is the handler's answer.
						let standing =
							!matches!(err, Error::Unroutable) || shared.read().routes.covers(&path.as_path(), route);
						Event::Resolved {
							route,
							result: Err(Refusal { err, standing }),
						}
					}
				}
			}
			Step::SourceClosed(source) => Event::SourceClosed { source },
			Step::Info(name, source, result) => {
				let closing = sources.get(&source).is_some_and(|s| s.is_closing());
				let Some(io) = tracks.get_mut(&name) else { continue };
				let Some((_, copy, _)) = io.query.take() else { continue };
				// A copy that is already aborted cannot be spliced; its error is
				// the source's answer for the track.
				let result = match result {
					Ok(info) => match copy.poll_complete(&kio::Waiter::noop()) {
						Poll::Ready(Err(err)) => Err(err),
						_ => Ok(info),
					},
					Err(err) => Err(err),
				};
				// Staged only while the track has a reader: without one the machine
				// will not splice, and a held copy would keep the source subscribed.
				if result.is_ok() && io.used {
					io.staged = Some((source, copy));
				}
				Event::TrackInfo {
					track: name,
					source,
					closing,
					result,
				}
			}
			Step::Ended(name, source, result) => {
				let closing = sources.get(&source).is_some_and(|s| s.is_closing());
				let Some(io) = tracks.get_mut(&name) else { continue };
				io.copy = None;
				let delivered = io.resume.resume_position() != io.edge;
				Event::TrackEnded {
					track: name,
					source,
					closing,
					result,
					delivered,
				}
			}
			Step::Demand(name) => {
				let Some(io) = tracks.get_mut(&name) else { continue };
				io.used = io.resume.is_used();
				if !io.used {
					// Nothing will be spliced now: let go of the copies a query
					// holds, or the source stays subscribed with nobody reading.
					io.query = None;
					io.staged = None;
				}
				match io.used {
					true => Event::Used { track: name },
					false => Event::Unused {
						track: name,
						now: timers.now(),
					},
				}
			}
			Step::Deadline => {
				// Cleared here so a fired deadline cannot keep firing; the machine
				// re-arms what is still parked.
				deadline.set(None);
				Event::Deadline { now: timers.now() }
			}
			Step::Table => select(&mut front, &sources, &mut seen),
		};
		events.push_back(event);
	}
}

/// The announced routes, keyed by prefix: a trie with one node per path
/// segment. Every question about a path walks its segments, so the cost of an
/// announcement, a cursor registration, or a request is bounded by the tree
/// around that path and never by the size of the table.
#[derive(Default)]
struct RouteTable {
	root: RouteNode,
}

/// One prefix in the [`RouteTable`]: what is announced exactly there, which
/// cursors hang there, and the prefixes one segment below.
#[derive(Default)]
struct RouteNode {
	/// Routes announced exactly at this prefix.
	entries: Vec<RouteEntry>,
	/// Cursors with an interest head at this prefix (see [`interest_prefixes`]).
	cursors: Vec<ConsumerId>,
	/// Cursors at this node or below. An announcement walks only the subtrees
	/// that hold one, so a deep table of routes nobody watches costs nothing.
	cursors_below: usize,
	/// Who is waiting on the routes covering this prefix: the fronts serving it
	/// and the requesters parked on it (see [`Watch`]).
	watches: Vec<(u64, kio::Producer<Watched>)>,
	/// Watches at this node or below, so a route change walks only the subtrees
	/// holding one.
	watches_below: usize,
	children: HashMap<String, RouteNode>,
}

/// What a [`Watch`] observes: bumped by every change to a route covering its
/// path (a broadcast published here is one) and by the origin's teardown.
#[derive(Default)]
struct Watched {
	generation: u64,
}

/// A registration in the route table for changes to the routes covering one
/// path. The table pokes it; the holder waits on it, so an announcement wakes
/// only the fronts and requesters it can affect rather than every one of them.
/// Dropping it unregisters, which takes the table lock: never drop one while
/// holding it.
struct Watch {
	shared: kio::Shared<OriginState>,
	path: PathOwned,
	id: u64,
	signal: kio::Consumer<Watched>,
}

impl Watch {
	/// The generation to wait past with [`Self::poll_changed`]. Read under the
	/// table lock, alongside the decision it guards, so a poke between the two
	/// cannot be missed: a poke takes that same lock first.
	fn seen(&self) -> u64 {
		self.signal.read().generation
	}

	/// Ready once the routes covering the path moved past `seen`.
	fn poll_changed(&self, waiter: &kio::Waiter, seen: u64) -> Poll<()> {
		self.signal
			.poll(waiter, |watched| match watched.generation != seen {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
			.map(|_| ())
	}
}

impl Drop for Watch {
	fn drop(&mut self) {
		self.shared.lock().routes.remove_watch(&self.path, self.id);
	}
}

/// What a registration adds to the subtree counts on its walk.
#[derive(Clone, Copy)]
struct Below {
	cursors: usize,
	watches: usize,
}

impl Below {
	const NONE: Self = Self { cursors: 0, watches: 0 };
	const CURSOR: Self = Self { cursors: 1, watches: 0 };
	const WATCH: Self = Self { cursors: 0, watches: 1 };
}

impl RouteNode {
	/// Nothing here and nothing below: the node can be pruned.
	fn is_empty(&self) -> bool {
		self.entries.is_empty() && self.cursors.is_empty() && self.watches.is_empty() && self.children.is_empty()
	}

	/// The node `parts` below this one, if the table has it.
	fn find<'a>(&self, mut parts: impl Iterator<Item = &'a str>) -> Option<&Self> {
		match parts.next() {
			None => Some(self),
			Some(part) => self.children.get(part)?.find(parts),
		}
	}

	/// The node `parts` below this one, created along the way when missing.
	/// `below` is added to the subtree counts at every node on the walk.
	fn reach<'a>(&mut self, mut parts: impl Iterator<Item = &'a str>, below: Below) -> &mut Self {
		self.cursors_below += below.cursors;
		self.watches_below += below.watches;
		match parts.next() {
			None => self,
			Some(part) => self.children.entry(part.to_string()).or_default().reach(parts, below),
		}
	}

	/// Run `f` on the node `parts` below this one, then prune every node the
	/// edit emptied. `below` is subtracted from the subtree counts at every node
	/// on the walk. `None` when the node does not exist, leaving the table as is.
	fn edit<'a, R>(
		&mut self,
		mut parts: impl Iterator<Item = &'a str>,
		below: Below,
		f: impl FnOnce(&mut Self) -> R,
	) -> Option<R> {
		let result = match parts.next() {
			None => f(self),
			Some(part) => {
				let child = self.children.get_mut(part)?;
				let result = child.edit(parts, below, f)?;
				if child.is_empty() {
					self.children.remove(part);
				}
				result
			}
		};
		self.cursors_below -= below.cursors;
		self.watches_below -= below.watches;
		Some(result)
	}

	/// Wake the watches at this node.
	fn poke(&self) {
		for (_, watch) in &self.watches {
			if let Ok(mut watched) = watch.write() {
				watched.generation += 1;
			}
		}
	}

	/// Wake the watches at this node and below: a route here covers every one
	/// of their paths. Skips subtrees holding none.
	fn poke_below(&self) {
		if self.watches_below == 0 {
			return;
		}
		self.poke();
		for child in self.children.values() {
			child.poke_below();
		}
	}

	/// Visit this node and everything below it.
	fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Self)) {
		visit(self);
		for child in self.children.values() {
			child.walk(visit);
		}
	}

	/// Collect the cursors at this node and below, skipping subtrees with none.
	fn collect_cursors(&self, out: &mut Vec<ConsumerId>) {
		if self.cursors_below == 0 {
			return;
		}
		out.extend(&self.cursors);
		for child in self.children.values() {
			child.collect_cursors(out);
		}
	}
}

impl RouteTable {
	/// The nodes above `path` and the node at it, as far as the table has them.
	/// The entries of those nodes are exactly the routes covering `path`.
	fn split(&self, path: &Path) -> (Vec<&RouteNode>, Option<&RouteNode>) {
		let mut above = Vec::new();
		let mut node = &self.root;
		for part in path.parts() {
			above.push(node);
			match node.children.get(part) {
				Some(child) => node = child,
				None => return (above, None),
			}
		}
		(above, Some(node))
	}

	/// The routes covering `path`: those announced at it and at every prefix of it.
	fn covering(&self, path: &Path) -> impl Iterator<Item = &RouteEntry> {
		let (above, at) = self.split(path);
		above.into_iter().chain(at).flat_map(|node| node.entries.iter())
	}

	/// Whether the route `id` still covers `path`.
	fn covers(&self, path: &Path, id: u64) -> bool {
		self.covering(path).any(|entry| entry.id == id)
	}

	/// The routes announced exactly at `prefix`.
	fn at(&self, prefix: &Path) -> impl Iterator<Item = &RouteEntry> {
		self.root
			.find(prefix.parts())
			.into_iter()
			.flat_map(|node| node.entries.iter())
	}

	/// Every route in the table, for the teardown.
	fn entries(&self) -> impl Iterator<Item = &RouteEntry> {
		let mut nodes = Vec::new();
		self.root.walk(&mut |node| nodes.push(node));
		nodes.into_iter().flat_map(|node| node.entries.iter())
	}

	/// Add a route at its prefix, creating the nodes down to it.
	fn insert(&mut self, entry: RouteEntry) {
		let node = self.root.reach(entry.prefix.parts(), Below::NONE);
		node.entries.push(entry);
	}

	/// The route `id` announced at `prefix`, for a re-price in place.
	fn entry_mut(&mut self, prefix: &Path, id: u64) -> Option<&mut RouteEntry> {
		let mut node = &mut self.root;
		for part in prefix.parts() {
			node = node.children.get_mut(part)?;
		}
		node.entries.iter_mut().find(|entry| entry.id == id)
	}

	/// Take the route `id` out of `prefix`, pruning the nodes it leaves empty.
	fn remove(&mut self, prefix: &Path, id: u64) -> Option<RouteEntry> {
		self.root
			.edit(prefix.parts(), Below::NONE, |node| {
				let index = node.entries.iter().position(|entry| entry.id == id)?;
				Some(node.entries.swap_remove(index))
			})
			.flatten()
	}

	/// Hang a cursor at one of its heads, counting it down the walk.
	fn add_cursor(&mut self, head: &Path, id: ConsumerId) {
		self.root.reach(head.parts(), Below::CURSOR).cursors.push(id);
	}

	/// Take a cursor off one of its heads, pruning the nodes it leaves empty. Only
	/// ever called for a head the cursor was added at, or the counts drift.
	fn remove_cursor(&mut self, head: &Path, id: ConsumerId) {
		self.root.edit(head.parts(), Below::CURSOR, |node| {
			node.cursors.retain(|cursor| *cursor != id)
		});
	}

	/// Register a watch on the routes covering `path`; see [`Watch`].
	fn add_watch(&mut self, path: &Path, id: u64) -> kio::Consumer<Watched> {
		let producer = kio::Producer::<Watched>::default();
		let consumer = producer.consume();
		self.root.reach(path.parts(), Below::WATCH).watches.push((id, producer));
		consumer
	}

	/// Take a watch off its path, pruning the nodes it leaves empty. Only ever
	/// called for a path the watch was added at, or the counts drift.
	fn remove_watch(&mut self, path: &Path, id: u64) {
		self.root.edit(path.parts(), Below::WATCH, |node| {
			node.watches.retain(|(watch, _)| *watch != id)
		});
	}

	/// Wake the watches of every path a route at `prefix` covers.
	fn poke_below(&self, prefix: &Path) {
		if let (_, Some(node)) = self.split(prefix) {
			node.poke_below();
		}
	}

	/// Wake every watch: the origin is tearing down.
	fn poke_all(&self) {
		self.root.walk(&mut |node| node.poke());
	}

	/// The cursors a route at `prefix` can present on: a cursor sees a route
	/// only when one of its heads is on the walk down to the prefix or somewhere
	/// beneath it, so those are the only cursors visited.
	fn cursors_touching(&self, prefix: &Path) -> Vec<ConsumerId> {
		let (above, at) = self.split(prefix);
		let mut cursors: Vec<ConsumerId> = above.iter().flat_map(|node| node.cursors.iter().copied()).collect();
		if let Some(node) = at {
			node.collect_cursors(&mut cursors);
		}
		// A cursor with several heads can be reached more than once.
		cursors.sort_unstable();
		cursors.dedup();
		cursors
	}
}

/// The origin's shared state: the route table, the announce cursors observing
/// it, and the remotely-served fronts.
///
/// Carried in a [`kio::Shared`], so producers, consumers, and handlers work
/// under one lock. Broadcasts published here are route table entries like the
/// routes announced from elsewhere; this holds everything that serves a path.
#[derive(Default)]
struct OriginState {
	// The announced routes, keyed by prefix. The table holds one entry per live
	// advertisement, not one per broadcast consumer.
	routes: RouteTable,
	next_route: u64,
	next_watch: u64,

	// The registered announce cursors, each with its own coalescing buffer. Each
	// also hangs in the route table at its heads, which is how an announcement
	// finds the cursors it can present on.
	cursors: HashMap<ConsumerId, TableCursor>,

	// The remotely-served fronts, keyed by absolute path and the requester's
	// split-horizon exclusion. Each is a spliced broadcast whose watcher task
	// materializes it from the best covering route and re-splices it through
	// routes sharing its first hop, so a route change the identity survives is
	// invisible to subscribers. Keyed per exclusion so a front's failover can
	// never adopt a route flowing back through one of its own readers. Weak, so
	// a front dies with its watcher and a later request re-creates it.
	fronts: WeakCache<FrontKey, RemoteFront>,

	// Remote announce sources still replaying their initial set, keyed by
	// [`Replaying`] id. Empty outside a session's first round trip.
	replaying: HashMap<u64, ReplayingSource>,
	next_replaying: u64,

	// Set when the origin's driver dropped: new requests fail with `Closed`
	// immediately and handlers observe the end instead of parking forever.
	closed: bool,
}

/// A remote announce source still replaying its initial set.
struct ReplayingSource {
	/// The absolute patterns the source may announce under.
	scope: Patterns,
	/// The cursors registered while it replayed, each owed its landing.
	waiting: Vec<ConsumerId>,
}

impl OriginState {
	/// Re-deliver the best route at every presented prefix `prefix` maps to, on
	/// every cursor it can present on. Called after an entry covering `prefix`
	/// was added, updated, or removed. `claim` is `prefix`'s [`prefix_claim`],
	/// held by the entry that changed.
	fn sync_route(&mut self, prefix: &Path, claim: &Pattern) {
		// Split borrows: the recompute reads `routes` while mutating a cursor.
		let routes = &self.routes;
		for id in routes.cursors_touching(prefix) {
			let Some(cursor) = self.cursors.get_mut(&id) else {
				continue;
			};
			if let Some(presented) = cursor.presented(prefix, claim) {
				Self::sync_cursor(routes, cursor, &presented);
			}
		}
		// The fronts and requesters under the prefix re-select from the table.
		routes.poke_below(prefix);
	}

	/// Register a [`Watch`] on the routes covering `path`.
	fn watch(&mut self, shared: &kio::Shared<OriginState>, path: &Path) -> Watch {
		let id = self.next_watch;
		self.next_watch += 1;
		let signal = self.routes.add_watch(path, id);
		Watch {
			shared: shared.clone(),
			path: path.to_owned(),
			id,
			signal,
		}
	}

	/// Recompute the best visible route presenting at `presented` (relative) for
	/// one cursor and deliver the change, if any.
	fn sync_cursor(routes: &RouteTable, cursor: &mut TableCursor, presented: &PathOwned) {
		// The entries presenting here are the ones announced at the absolute
		// prefix, or, for the cursor's own root, at the root and every prefix
		// above it (all of which present as the empty path). Among them, the
		// longest prefix wins outright, so the metadata a cursor advertises
		// matches what a request through it actually resolves.
		let candidates: Vec<&RouteEntry> = match presented.is_empty() {
			true => routes
				.covering(&cursor.root)
				.filter(|entry| cursor.visible(entry))
				.collect(),
			false => {
				let absolute = cursor.root.join(presented);
				routes.at(&absolute).filter(|entry| cursor.visible(entry)).collect()
			}
		};
		let most = candidates.iter().map(|entry| entry.prefix.len()).max();
		let best = most.and_then(|most| {
			candidates
				.into_iter()
				.filter(|entry| entry.prefix.len() == most)
				.min_by_key(|entry| route_order(&entry.prefix, entry))
		});

		match best {
			Some(entry) => {
				let meta = (entry.hops.clone(), entry.cost, entry.entered());
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
		// The routes a cursor can see sit on the walk down to one of its heads or
		// somewhere beneath it, so only those subtrees are replayed.
		let mut presented: BTreeSet<PathOwned> = BTreeSet::new();
		for head in &cursor.heads {
			let (above, at) = self.routes.split(head);
			let mut nodes = above;
			if let Some(node) = at {
				node.walk(&mut |node| nodes.push(node));
			}
			for entry in nodes.into_iter().flat_map(|node| node.entries.iter()) {
				if let Some(p) = cursor.presented(&entry.prefix, &entry.claim) {
					presented.insert(p);
				}
			}
		}
		for p in &presented {
			Self::sync_cursor(&self.routes, &mut cursor, p);
		}
		// The marker also waits on every source still replaying into this
		// cursor's scope; with none, it follows the replay above.
		let mut waiting = 0;
		for source in self.replaying.values_mut() {
			if source.scope.iter().any(|pattern| cursor.allowed.overlaps(pattern)) {
				source.waiting.push(id);
				waiting += 1;
			}
		}
		if let Ok(mut state) = cursor.state.write() {
			state.live = LiveState::Waiting(waiting + 1);
			state.landed();
		}
		for head in &cursor.heads {
			self.routes.add_cursor(head, id);
		}
		self.cursors.insert(id, cursor);
	}

	/// The best served route covering `path` (absolute) for a requester seeing
	/// `horizon`, skipping the `refused` entry ids.
	///
	/// The most specific covering prefix wins outright, so a narrow advertise-only
	/// announcement shadows a broad served one: requests under it resolve
	/// unroutable instead of being routed around it. Among routes at the winning
	/// prefix, the cheapest served one is picked by [`route_order`].
	///
	/// Only announced routes are candidates: an unannounced broadcast serves
	/// nobody, and does not shadow anything either. `pin` is the front's
	/// identity: only routes it admits are candidates, since a route from anyone
	/// else is different content rather than an alternate path (see [`Front`]).
	/// A broadcast published on this origin competes on cost like any other
	/// route and wins a tie.
	fn best_route(&self, path: &Path, horizon: Horizon, pin: Pin, refused: &HashSet<u64>) -> Option<&RouteEntry> {
		// Covering prefixes of one path form a chain, so the deepest node with a
		// candidate holds the unique longest prefix; walking down, the last such
		// node decides.
		let (above, at) = self.routes.split(path);
		let mut best = None;
		for node in above.into_iter().chain(at) {
			let mut candidates = node
				.entries
				.iter()
				.filter(|entry| entry.advertised)
				.filter(|entry| entry.scope.matches(path.as_str()))
				.filter(|entry| horizon.admits(entry))
				.filter(|entry| entry.qualifies(pin))
				.filter(|entry| !refused.contains(&entry.id))
				.peekable();
			if candidates.peek().is_some() {
				best = candidates
					.filter(|entry| entry.serves(path))
					.min_by_key(|entry| route_order(&entry.prefix, entry));
			}
		}
		best
	}
}

/// A remote announce source still replaying its initial set, from
/// [`Producer::replaying`]. Drop it once the set has landed.
pub(crate) struct Replaying {
	shared: kio::Shared<OriginState>,
	id: u64,
}

impl Drop for Replaying {
	fn drop(&mut self) {
		let mut state = self.shared.lock();
		let Some(source) = state.replaying.remove(&self.id) else {
			return;
		};
		for id in source.waiting {
			// A cursor dropped since registering is simply gone.
			if let Some(cursor) = state.cursors.get(&id)
				&& let Ok(mut cursor) = cursor.state.write()
			{
				cursor.landed();
			}
		}
	}
}

/// When a [`Replaying`] source has landed, for wires that never say where the
/// initial set ends (lite-03/04, moq-transport): once its stream goes quiet.
///
/// A peer writes its whole set back to back, so the first announcement gets a
/// round trip's grace and each one after it only has to beat its siblings.
pub(crate) struct Quiet {
	deadline: crate::runtime::Deadline<Clock>,
	clock: Clock,
}

impl Quiet {
	/// How long the stream may stay silent before its first announcement.
	const FIRST: Duration = Duration::from_millis(500);
	/// How long the stream may stay silent between announcements.
	const GAP: Duration = Duration::from_millis(30);

	/// Start counting from now.
	pub(crate) fn new(clock: &Clock) -> Self {
		Self {
			deadline: crate::runtime::Deadline::after(clock, Self::FIRST),
			clock: clock.clone(),
		}
	}

	/// An announcement arrived: the set is still landing.
	pub(crate) fn heard(&mut self) {
		let now = crate::runtime::Timers::now(&self.clock);
		self.deadline.set(now.checked_add(Self::GAP));
	}

	/// Ready once the stream has gone quiet.
	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		self.deadline.poll(waiter)
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
}

enum RequestState {
	// Unroutable at request time: resolves immediately with this error. Baked in so
	// `request_broadcast` itself stays infallible.
	Failed(Error),
	// Awaiting a handler: resolves when the request's result channel is written.
	Pending(kio::Consumer<PendingBroadcast>),
}

impl Requesting {
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
		}
	}

	fn with_path(mut self, path: PathOwned) -> Self {
		self.path = path;
		self
	}

	/// The egress scope the resolved broadcast's reads are attributed to.
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

impl kio::Task for Requesting {
	type Output = Result<broadcast::Consumer, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
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

/// Cheap read handle over an origin's route table.
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

	// Split horizon: routes whose hop chain or announcing session (`via`) is the
	// excluded peer are invisible to `announced` and skipped by
	// `request_broadcast`, so a peer is never served (or advertised) its own
	// content back. A local view (`Self::local`) hides peer routes the same way.
	horizon: Horizon,

	// Which routes beneath a hidden (`.`-prefixed) segment `announced` reports.
	hidden: Hidden,

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
			horizon: Horizon::default(),
			hidden: Hidden::default(),
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
	/// anonymous route from echoing back. Pass [`Hop::UNKNOWN`] for an anonymous
	/// peer.
	pub(crate) fn excluding(mut self, peer: Hop) -> Self {
		self.horizon.exclude = Some(peer);
		self
	}

	/// A view of the routes that entered here: every route a handle marked
	/// [`Producer::peer`] announced is hidden from [`Self::announced`] and never
	/// resolved by [`Self::request_broadcast`].
	///
	/// On a relay, this is what the relay ingests itself, from clients and
	/// in-process producers, as opposed to what its cluster peers forward.
	pub fn local(mut self) -> Self {
		self.horizon.local = true;
		self
	}

	/// A clone whose [`announced`](Self::announced) also reports hidden routes:
	/// those with a segment starting with `.` below the requested prefix.
	/// Hidden routes are left out by default, so a platform can add `.`-named
	/// broadcasts without them turning up in apps that list everything.
	pub fn with_hidden(mut self, hidden: bool) -> Self {
		self.hidden.include = hidden;
		self
	}

	/// A clone whose [`announced`](Self::announced) reports only the routes a feed
	/// from `outer` hides, for a stream topping up that feed.
	pub(crate) fn beyond(mut self, outer: &Consumer) -> Self {
		self.hidden.beyond = Some(interest_prefixes(&outer.scope.allowed));
		self
	}

	/// Whether [`announced`](Self::announced) reports hidden routes too.
	pub(crate) fn includes_hidden(&self) -> bool {
		self.hidden.include
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
	/// Routes with a segment starting with `.` below the literal head of those
	/// patterns are hidden unless [`with_hidden`](Self::with_hidden) opted in.
	/// Drop the returned [`AnnounceConsumer`] to unregister.
	pub fn announced(&self) -> AnnounceConsumer {
		AnnounceConsumer::new(
			self.root.clone(),
			self.scope.allowed.clone(),
			self.stats.clone(),
			self.horizon,
			self.hidden.clone(),
			&self.shared,
		)
	}

	/// Returns a cheap duplicate of this read handle.
	pub fn consume(&self) -> Self {
		self.clone()
	}

	/// The newest broadcast published on this origin at exactly `path`, if any.
	/// Test-only: a request goes through the table like any other.
	#[cfg(test)]
	pub(crate) fn get_broadcast(&self, path: impl AsPath) -> Option<broadcast::Consumer> {
		let full = self.root.join(path).to_owned();
		if !self.scope.permits(&full) {
			return None;
		}
		let table = self.shared.lock();
		table
			.routes
			.at(&full)
			.filter(|entry| entry.local)
			.min_by_key(|entry| route_order(&entry.prefix, entry))
			.and_then(|entry| entry.source.clone())
	}

	/// Block until an announced route covers `path`, and return it.
	///
	/// Covering means the route's prefix is a (segment-wise) prefix of `path`,
	/// including the exact path itself. Returns `None` if the path is outside this
	/// consumer's scope or the consumer is closed first.
	///
	/// To resolve a broadcast rather than inspect the route, use
	/// [`Self::routed_broadcast`]: pairing this with [`Self::request_broadcast`]
	/// leaves a gap where the covering route can retract.
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
		// forwarding, so it must not drive the announce guards. Hiding narrows
		// discovery, not lookup, so a hidden path resolves like any other.
		let mut announced = consumer.untagged().with_hidden(true).announced();
		loop {
			if let AnnounceEvent::Announced(announce) | AnnounceEvent::Updated(announce) = announced.next().await?
				&& path.has_prefix(&announce.prefix)
			{
				return Some(announce.route);
			}
		}
	}

	/// Block until `path` resolves to a broadcast: [`Self::request_broadcast`],
	/// retried whenever the routes covering the path change.
	///
	/// A request answers for the routes as they stand, so it can miss an
	/// announcement that has not arrived yet, lose its covering route to
	/// failover churn, find a route that covers the path while nothing serves it
	/// yet (an advertise-only announce racing its handler), or be turned down by
	/// a handler. This rides all of that out by watching the covering routes
	/// and asking again each time they move, which is what makes it the right
	/// call for resolving a path right after connecting. Returns
	/// [`Error::Unauthorized`] for a path outside this consumer's scope,
	/// [`Error::Closed`] once the origin closes, and any other resolution
	/// failure as-is.
	pub async fn routed_broadcast(&self, path: impl AsPath) -> Result<broadcast::Consumer, Error> {
		let path = path.as_path();

		// `allowed` keeps narrower permissions intact: if the whole path is not
		// reachable, no route can ever cover it, so bail rather than loop forever.
		if !self.allowed().matches(path.as_str()) {
			return Err(Error::Unauthorized);
		}
		loop {
			// `Unroutable` is a verdict of the routes covering the path as they
			// stood when the request was made. Re-asking the same routes would
			// spin, so watch them before asking and wait for them to move (a
			// route arriving or retracting, an identical standby swapping in, a
			// local broadcast announcing at the path), then try again. A change
			// between the ask and the wait bumps the watch first, so that retry
			// is immediate; the teardown pokes every watch, so a closed origin
			// is observed on the next pass.
			let (watch, seen) = {
				let mut table = self.shared.lock();
				if table.closed {
					return Err(Error::Closed);
				}
				let watch = table.watch(&self.shared, &self.root.join(&path));
				let seen = watch.seen();
				(watch, seen)
			};
			match self.request_broadcast(&path).await {
				Ok(broadcast) => return Ok(broadcast),
				Err(Error::Unroutable) => {
					kio::wait(|waiter| watch.poll_changed(waiter, seen)).await;
				}
				// Teardown parks a pending request with `Dropped`; the contract is
				// `Closed` once the origin is gone.
				Err(Error::Dropped) if self.shared.lock().closed => return Err(Error::Closed),
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
	/// Returns a [`kio::Pending`] future, mirroring
	/// [`track::Consumer::fetch_group`](track::Consumer::fetch_group). Every
	/// path resolves through a front the origin's [`Driver`] runs: the request
	/// mints one or joins the one already serving the path, and the front picks
	/// the best announced route covering it (the most specific prefix, then the
	/// cheapest, a broadcast published on this origin winning ties) and
	/// materializes it, from the broadcast itself or from the peer that
	/// announced the route. When its serving source dies or a better qualifying
	/// route appears, the front re-splices through the best route sharing its
	/// first hop at a group boundary, invisibly to subscribers. A change that
	/// does not preserve the first hop ends the broadcast instead, as does its
	/// route retracting with no replacement, and the next request re-serves the
	/// path. Tracks already in flight carry on to their own end.
	///
	/// The returned future fails with [`Error::Unroutable`] at once when no
	/// announced route covers the path, including a broadcast created on this
	/// origin but not announced.
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

		// Routes only cover paths within this consumer's scope.
		if !self.scope.permits(&absolute) {
			return kio::Pending::new(Requesting::failed(Error::Unauthorized));
		}

		let mut state = self.shared.lock();

		// The origin's driver dropped: nothing will ever serve this.
		if state.closed {
			return kio::Pending::new(Requesting::failed(Error::Closed));
		}

		// Nothing serves the path: no announced broadcast and no served route.
		// Checked before joining a front, so a front still draining after its
		// route retracted takes no newcomers.
		if state
			.best_route(&absolute.as_path(), self.horizon, Pin::Any, &HashSet::new())
			.is_none()
		{
			return kio::Pending::new(Requesting::failed(Error::Unroutable));
		}

		// Join the live front for this path and exclusion, if any: its watcher
		// resolves (or already resolved) the request channel with the front's
		// spliced broadcast, so repeat requests share one upstream
		// subscription. Only while the best route still serves the front's
		// content, though: once a different publisher wins (a cheaper route), a
		// newcomer gets a fresh front from it, and the old front keeps serving the
		// readers it has, since other content can't be spliced into it.
		let key = (absolute.clone(), self.horizon);
		if let Some(front) = state.fronts.get(&key) {
			let pin = *front.pin.lock();
			let current = state
				.best_route(&absolute.as_path(), self.horizon, Pin::Any, &HashSet::new())
				.is_some_and(|entry| entry.qualifies(pin));
			if current {
				let pending = Requesting::queued(front.request.consume())
					.with_path(requested)
					.with_stats(scope);
				return kio::Pending::new(pending);
			}
			state.fronts.remove(&key);
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
		let request = kio::Producer::<PendingBroadcast>::default();
		let consumer = request.consume();
		let watch = state.watch(&self.shared, &absolute);
		let pin = kio::Lock::new(Pin::Any);
		state.fronts.insert(
			key,
			RemoteFront {
				request: request.clone(),
				broadcast: broadcast.consume().weak(),
				pin: pin.clone(),
			},
		);
		// Released before the push: a set whose handles are gone drops the task,
		// and the `Watch` it carries unregisters under this same lock.
		drop(state);
		self.tasks.push(run_front(FrontTask {
			shared: self.shared.clone(),
			broadcast,
			path: absolute,
			horizon: self.horizon,
			watch,
			request,
			pin,
			timers: self.timers.clone(),
		}));
		kio::Pending::new(Requesting::queued(consumer).with_path(requested).with_stats(scope))
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
		horizon: Horizon,
		hidden: Hidden,
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
						heads: interest_prefixes(&allowed),
						allowed,
						horizon,
						hidden,
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
	fn hand_out(&mut self, event: AnnounceEvent) -> AnnounceEvent {
		match &event {
			AnnounceEvent::Announced(announce) | AnnounceEvent::Updated(announce) => {
				let scope = self.stats.egress(self.root.join(&announce.prefix));
				self.guards
					.entry(announce.prefix.clone())
					.or_insert_with(|| scope.announce());
			}
			AnnounceEvent::Retracted(announce) => {
				self.guards.remove(&announce.prefix);
			}
			AnnounceEvent::Live => {}
		}
		event
	}

	/// Returns the next route announcement, update, or retraction, its prefix
	/// relative to this cursor's root, or the one [`AnnounceEvent::Live`] marker.
	///
	/// The routes live at subscribe time come first, then the marker, then live
	/// changes, so a caller listing what is live stops at the marker. A
	/// retraction is only delivered for a previously announced prefix, and a
	/// repeated announcement for the same prefix is a metadata update. Returns
	/// None if the cursor is closed. The consumer is also a [`futures::Stream`]
	/// of the same events.
	pub async fn next(&mut self) -> Option<AnnounceEvent> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next event, without blocking.
	///
	/// Returns `Poll::Ready(Some(_))` for an event, `Poll::Ready(None)` if the
	/// cursor is closed, or `Poll::Pending` after registering `waiter` to be
	/// notified when the next event arrives.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<AnnounceEvent>> {
		let event = {
			let mut state = match ready!(self.state.poll(waiter, |state| match state.ready() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})) {
				Ok(state) => state,
				// Closed: discard the Ref so its MutexGuard doesn't escape this call.
				Err(_) => return Poll::Ready(None),
			};
			match state.take() {
				Some(event) => event,
				None => {
					// Ended by the origin's teardown, pending updates already
					// drained; close the channel so every closure signal agrees.
					state.close();
					return Poll::Ready(None);
				}
			}
		};
		Poll::Ready(Some(self.hand_out(event)))
	}

	/// Returns the next event without blocking.
	///
	/// Returns None if there is no event available; NOT because the cursor is closed.
	/// Use [`Self::is_closed`] to check if the cursor is closed.
	pub fn try_next(&mut self) -> Option<AnnounceEvent> {
		let event = self.state.write().ok()?.take()?;
		Some(self.hand_out(event))
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
	type Item = AnnounceEvent;

	fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
		let this = self.get_mut();
		let waiter = this.park.hold(cx).clone();
		this.poll_next(&waiter)
	}
}

impl Drop for AnnounceConsumer {
	fn drop(&mut self) {
		let mut shared = self.shared.lock();
		if let Some(cursor) = shared.cursors.remove(&self.id) {
			for head in &cursor.heads {
				shared.routes.remove_cursor(head, self.id);
			}
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl AnnounceConsumer {
	/// The next route event without blocking, skipping the
	/// [`AnnounceEvent::Live`] marker: these helpers pin routes, and the marker
	/// has its own tests.
	fn next_route_now(&mut self) -> Option<Option<AnnounceEvent>> {
		loop {
			match self.next().now_or_never()? {
				Some(AnnounceEvent::Live) => continue,
				event => return Some(event),
			}
		}
	}

	/// The `try_next` counterpart of [`Self::next_route_now`].
	fn try_next_route(&mut self) -> Option<AnnounceEvent> {
		loop {
			match self.try_next()? {
				AnnounceEvent::Live => continue,
				event => return Some(event),
			}
		}
	}

	/// The route of an active event at `expected`.
	fn active(event: AnnounceEvent, expected: &Path) -> Route {
		match event {
			AnnounceEvent::Announced(announce) | AnnounceEvent::Updated(announce) => {
				assert_eq!(announce.prefix, *expected, "wrong prefix");
				announce.route
			}
			other => panic!("should be an active route: got {other:?}"),
		}
	}

	/// The next update must be an active route at `expected`; returns it.
	pub fn assert_next_active(&mut self, expected: impl AsPath) -> Route {
		let event = self.next_route_now().expect("next blocked").expect("no next");
		Self::active(event, &expected.as_path())
	}

	/// The `try_next` counterpart of [`Self::assert_next_active`].
	pub fn assert_try_next_active(&mut self, expected: impl AsPath) -> Route {
		let event = self.try_next_route().expect("no next");
		Self::active(event, &expected.as_path())
	}

	/// The next update must be a retraction at `expected`.
	pub fn assert_next_ended(&mut self, expected: impl AsPath) {
		match self.next_route_now().expect("next blocked").expect("no next") {
			AnnounceEvent::Retracted(announce) => assert_eq!(announce.prefix, expected.as_path(), "wrong prefix"),
			other => panic!("should be a retraction: got {other:?}"),
		}
	}

	pub fn assert_next_wait(&mut self) {
		if let Some(event) = self.next_route_now() {
			panic!("next should block: got {event:?}");
		}
	}

	/// The next event must be the [`AnnounceEvent::Live`] marker.
	pub fn assert_next_live(&mut self) {
		match self.next().now_or_never() {
			Some(Some(AnnounceEvent::Live)) => {}
			other => panic!("expected the live marker: got {other:?}"),
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

	#[test]
	fn random_hops_fit_legacy_lite_clients() {
		for _ in 0..32 {
			assert!(Hop::random().id() < 1u64 << 53);
		}
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

	/// Yield to the driver until the subscription has its next group or its end.
	async fn next_group(subscription: &mut crate::track::Subscriber) -> Result<Option<crate::group::Consumer>, Error> {
		let mut next = None;
		settle(|| match subscription.poll_recv_group(&kio::Waiter::noop()) {
			Poll::Ready(result) => {
				next = Some(result);
				true
			}
			Poll::Pending => false,
		})
		.await;
		next.unwrap()
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

	/// A `.`-prefixed segment below the requested prefix hides a route from
	/// discovery unless the reader opts in; one inside the prefix does not.
	#[tokio::test]
	async fn hidden_routes_need_an_opt_in() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let _visible = producer.announce("room/alice", Route::default()).unwrap();
		let _stats = producer.announce(".stats/node", Route::default()).unwrap();
		let _nested = producer.announce("room/.internal", Route::default()).unwrap();
		// Only a leading dot hides: a suffix is part of the name.
		let _suffix = producer.announce("room/catalog.pro", Route::default()).unwrap();

		let mut announced = consumer.announced();
		announced.assert_next_active("room/alice");
		announced.assert_next_active("room/catalog.pro");
		announced.assert_next_wait();

		let mut announced = consumer.clone().with_hidden(true).announced();
		announced.assert_next_active(".stats/node");
		announced.assert_next_active("room/.internal");
		announced.assert_next_active("room/alice");
		announced.assert_next_active("room/catalog.pro");
		announced.assert_next_wait();

		// Naming the dot segment lists what is under it, by root or by pattern.
		let mut announced = consumer
			.scope(".stats", &Patterns::from(Pattern::all()))
			.unwrap()
			.announced();
		announced.assert_next_active("node");
		announced.assert_next_wait();
		let mut announced = consumer.scope("", &scopes(&["room/.internal"])).unwrap().announced();
		announced.assert_next_active("room/.internal");
		announced.assert_next_wait();

		// A top-up feed reports only what a feed from the root hid, filtered by its own
		// prefix and opt-in.
		let mut announced = consumer.clone().with_hidden(true).beyond(&consumer).announced();
		announced.assert_next_active(".stats/node");
		announced.assert_next_active("room/.internal");
		announced.assert_next_wait();
		let room = consumer.scope("", &scopes(&["room"])).unwrap().beyond(&consumer);
		let mut announced = room.announced();
		announced.assert_next_wait();
		let stats = consumer.scope("", &scopes(&[".stats"])).unwrap().beyond(&consumer);
		let mut announced = stats.announced();
		announced.assert_next_active(".stats/node");
		announced.assert_next_wait();
	}

	/// Hiding narrows discovery only: an exact request resolves without an opt-in.
	#[tokio::test]
	async fn hidden_broadcast_resolves_by_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let broadcast = producer.create_broadcast(".stats/node").unwrap();
		broadcast.announce(Route::default()).unwrap();

		consumer.announced().assert_next_wait();
		let resolved = consumer.request_broadcast(".stats/node").await.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), ".stats/node");
	}

	/// A route that turns up later is filtered the same way as the replay.
	#[tokio::test]
	async fn hidden_route_announced_later_stays_hidden() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();
		let mut opted = consumer.clone().with_hidden(true).announced();

		let hidden = producer.announce(".stats/node", Route::default()).unwrap();
		announced.assert_next_wait();
		opted.assert_next_active(".stats/node");

		drop(hidden);
		announced.assert_next_wait();
		opted.assert_next_ended(".stats/node");
	}

	#[tokio::test]
	async fn an_empty_origin_is_live_at_once() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().announced();
		announced.assert_next_live();
		announced.assert_next_wait();

		// Later routes are live changes; the marker never repeats.
		let _alice = producer.announce("alice", Route::default()).unwrap();
		announced.assert_next_active("alice");
		assert!(announced.next().now_or_never().is_none());
	}

	#[tokio::test]
	async fn live_follows_the_replayed_routes() {
		let producer = origin(1).produce();
		let _b = producer.announce("b", Route::default()).unwrap();
		let _c = producer.announce("c", Route::default()).unwrap();
		let mut announced = producer.consume().announced();

		// A route arriving before the replay drains may come either side of the
		// marker, but never pushes it past the replay.
		let _a = producer.announce("a", Route::default()).unwrap();
		let mut seen = Vec::new();
		loop {
			match announced.next().now_or_never().expect("blocked").expect("closed") {
				AnnounceEvent::Live => break,
				AnnounceEvent::Announced(announce) => seen.push(announce.prefix.to_string()),
				other => panic!("only announcements before the marker: got {other:?}"),
			}
		}
		assert!(
			seen.contains(&"b".to_string()) && seen.contains(&"c".to_string()),
			"{seen:?}"
		);
	}

	#[tokio::test]
	async fn a_replayed_route_retracted_before_delivery_does_not_hold_live() {
		let producer = origin(1).produce();
		let alice = producer.announce("alice", Route::default()).unwrap();
		let mut announced = producer.consume().announced();
		drop(alice);
		announced.assert_next_live();
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn a_replaying_source_withholds_live_until_it_lands() {
		let producer = origin(1).produce();
		let room = producer.scope("", &scopes(&["room"])).unwrap();
		let replaying = room.replaying("room/alice");

		let mut announced = producer.consume().announced();
		// Out of the source's scope: nothing to wait for.
		let mut elsewhere = producer.consume().scope("", &scopes(&["other"])).unwrap().announced();
		elsewhere.assert_next_live();
		// In its scope but beside the prefix it replays: nothing to wait for either.
		let mut bob = producer
			.consume()
			.scope("", &scopes(&["room/bob"]))
			.unwrap()
			.announced();
		bob.assert_next_live();

		// Routes the source lands arrive before the marker.
		let _alice = room.announce("room/alice", Route::default()).unwrap();
		announced.assert_next_active("room/alice");
		assert!(
			announced.next().now_or_never().is_none(),
			"live before the source landed"
		);

		drop(replaying);
		announced.assert_next_live();

		// A cursor registered after the source landed does not wait on it.
		let mut later = producer.consume().announced();
		later.assert_next_active("room/alice");
		later.assert_next_live();
	}

	#[tokio::test]
	async fn broadcast_announces_its_own_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();
		let mut peer = consumer.clone().excluding(Hop::UNKNOWN).announced();

		// Created but not announced: invisible to local and peer cursors alike.
		let broadcast = producer.create_broadcast("room/alice").unwrap();
		announced.assert_next_wait();
		peer.assert_next_wait();

		broadcast.announce(Route::default().with_cost(3)).unwrap();
		assert_eq!(announced.assert_next_active("room/alice").cost, Cost::new(3));
		assert_eq!(peer.assert_next_active("room/alice").cost, Cost::new(3));

		// Announcing again re-prices in place.
		broadcast.announce(Route::default().with_cost(1)).unwrap();
		assert_eq!(announced.assert_next_active("room/alice").cost, Cost::new(1));
		assert_eq!(peer.assert_next_active("room/alice").cost, Cost::new(1));

		// Off the air: the route retracts for everyone and the path is unroutable.
		broadcast.unannounce();
		announced.assert_next_ended("room/alice");
		peer.assert_next_ended("room/alice");
		broadcast.unannounce();
		announced.assert_next_wait();
		let err = consumer.request_broadcast("room/alice").await.err().unwrap();
		assert!(matches!(err, Error::Unroutable));

		// Back on the air, then the end of the broadcast retracts for good.
		broadcast.announce(Route::default()).unwrap();
		announced.assert_next_active("room/alice");
		peer.assert_next_active("room/alice");
		broadcast.finish();
		announced.assert_next_ended("room/alice");
		peer.assert_next_ended("room/alice");
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

		let Some(Some(AnnounceEvent::Announced(first))) = announced.next_route_now() else {
			panic!("expected the announcement");
		};
		assert_eq!(first.prefix.as_str(), "");
		assert_eq!(first.captures, Some(Vec::new()));

		drop(exact);
		let Some(Some(AnnounceEvent::Retracted(retracted))) = announced.next_route_now() else {
			panic!("expected the retraction");
		};
		assert_eq!(retracted.prefix.as_str(), "");
		assert_eq!(retracted.captures, Some(Vec::new()));
		let Some(Some(AnnounceEvent::Announced(replacement))) = announced.next_route_now() else {
			panic!("expected the replacement");
		};
		assert_eq!(replacement.prefix.as_str(), "");
		assert_eq!(replacement.captures, None);
	}

	#[tokio::test]
	async fn routed_broadcast_resolves_once_announced() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Asking before anything is announced parks instead of failing Unroutable.
		let mut resolving = Box::pin(consumer.routed_broadcast("room/alice"));
		assert!((&mut resolving).now_or_never().is_none());

		// Creating is not announcing: still parked.
		let broadcast = producer.create_broadcast("room/alice").unwrap();
		for _ in 0..20 {
			tokio::task::yield_now().await;
		}
		assert!((&mut resolving).now_or_never().is_none());

		broadcast.announce(Route::default()).unwrap();
		let resolved = resolving.await.expect("resolves once announced");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		drop(broadcast);
	}

	/// A local broadcast competes on its announced cost: a cheaper route at the
	/// same path wins, for cursors and requests alike.
	#[tokio::test]
	async fn cheaper_remote_route_beats_a_local_broadcast() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();

		let _local = producer.publish("room/alice", Route::default().with_cost(5)).unwrap();
		assert_eq!(announced.assert_next_active("room/alice").cost, Cost::new(5));

		let server = producer
			.dynamic("room/alice", Route::default().with_hops(hops(&[10])).with_cost(1))
			.unwrap();
		let route = announced.assert_next_active("room/alice");
		assert_eq!(route.cost, Cost::new(1));
		assert_eq!(route.hops, hops(&[10]));

		// The request goes upstream rather than to the local broadcast.
		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&server).await;
		let upstream = broadcast::Info::new().produce();
		request.accept(&upstream);
		pending.await.expect("resolves through the cheaper route");
	}

	/// A cheaper route that appears after a front was minted wins new requests too:
	/// the cached front serves other content, so a newcomer gets a fresh front from
	/// the winner, while the old front keeps serving the readers it already has.
	#[tokio::test]
	async fn cheaper_route_after_a_front_wins_new_requests() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let _local = producer.publish("room/alice", Route::default().with_cost(5)).unwrap();
		let first = consumer
			.request_broadcast("room/alice")
			.await
			.expect("resolves locally");

		let server = producer
			.dynamic("room/alice", Route::default().with_hops(hops(&[10])).with_cost(1))
			.unwrap();

		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&server).await;
		let upstream = broadcast::Info::new().produce();
		request.accept(&upstream);
		let second = pending.await.expect("resolves through the cheaper route");

		assert!(!first.is_closed(), "the old front must keep serving its readers");
		assert!(!first.is_clone(&second), "the newcomer must not join the old front");
	}

	/// At equal cost the local broadcast wins even over a route with no hops of its
	/// own, such as a later claim on this origin: locality is the tie-break after
	/// cost, not the newest entry.
	#[tokio::test]
	async fn local_broadcast_wins_a_tie_with_a_hopless_route() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let _local = producer.publish("room/alice", Route::default()).unwrap();
		let server = producer.dynamic("room/alice", Route::default()).unwrap();

		// Were the claim to win, the request would park on its handler forever.
		let resolved = tokio::time::timeout(Duration::from_secs(1), consumer.request_broadcast("room/alice"))
			.await
			.expect("the newer hopless route won the tie")
			.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		assert!(server.poll_requested_broadcast(&kio::Waiter::noop()).is_pending());
	}

	/// Ingress announce stats count advertised intervals, not the broadcast's
	/// lifetime: nothing while hidden, one per announce, none for a re-price.
	#[tokio::test]
	async fn announce_stats_follow_the_advertisement() {
		let registry = stats::Registry::new(stats::Config::new());
		let producer = origin(1)
			.produce()
			.with_stats(registry.tier(stats::Tier::default()).session("root"));
		let announces = || {
			registry
				.snapshot()
				.traffic()
				.into_iter()
				.find(|(_, role, _)| *role == stats::Role::Subscriber)
				.map(|(_, _, traffic)| (traffic.announces_started, traffic.announces_ended))
				.unwrap_or_default()
		};

		let broadcast = producer.create_broadcast("room/alice").unwrap();
		assert_eq!(announces(), (0, 0), "a hidden broadcast is not announced");
		broadcast.announce(Route::default()).unwrap();
		broadcast
			.announce(Route {
				cost: Cost::new(3),
				..Route::default()
			})
			.unwrap();
		assert_eq!(announces(), (1, 0), "a re-price is not another announce");
		broadcast.unannounce();
		assert_eq!(announces(), (1, 1));
		broadcast.announce(Route::default()).unwrap();
		drop(broadcast);
		assert_eq!(announces(), (2, 2));
	}

	/// At equal cost the local broadcast wins.
	#[tokio::test]
	async fn local_broadcast_wins_a_cost_tie() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.announced();

		let server = producer
			.dynamic("room/alice", Route::default().with_hops(hops(&[10])).with_cost(2))
			.unwrap();
		announced.assert_next_active("room/alice");
		let _local = producer.publish("room/alice", Route::default().with_cost(2)).unwrap();
		assert!(announced.assert_next_active("room/alice").hops.is_empty());

		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		for _ in 0..20 {
			tokio::task::yield_now().await;
		}
		assert!(server.poll_requested_broadcast(&kio::Waiter::noop()).is_pending());
	}

	/// Unannouncing ends the front the origin served from the broadcast and
	/// refuses new requests at once, even before the front acts on it.
	#[tokio::test]
	async fn unannounce_ends_the_front() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.publish("room/alice", Route::default()).unwrap();
		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");

		broadcast.unannounce();
		let err = consumer.request_broadcast("room/alice").await.err().unwrap();
		assert!(matches!(err, Error::Unroutable), "joined a retracted front: {err}");
		settle(|| resolved.is_closed()).await;
		assert!(!broadcast.consume().is_closed(), "the broadcast itself lives on");

		// Announcing again serves a fresh front.
		broadcast.announce(Route::default()).unwrap();
		let again = consumer.request_broadcast("room/alice").await.expect("resolves again");
		assert!(!again.is_clone(&resolved));
	}

	/// A subscriber still waiting on the source's track info is in flight too:
	/// unannouncing leaves it on the copy it asked for, which the source can
	/// still answer and finish.
	#[tokio::test]
	async fn unannounce_keeps_a_track_awaiting_its_info() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.publish("room/alice", Route::default()).unwrap();
		let mut dynamic = broadcast.dynamic();
		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");
		let track = resolved.track("video").unwrap();
		let subscribing = tokio::spawn(async move { track.subscribe(None).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source")
			.expect("request");

		broadcast.unannounce();
		settle(|| resolved.is_closed()).await;

		let source = request.accept(None);
		let mut group = source.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"late".as_ref()).unwrap();
		group.finish().unwrap();
		source.finish().unwrap();

		let mut subscription = subscribing.await.unwrap().expect("subscribe survives the retraction");
		let mut group = subscription.recv_group().await.unwrap().expect("the source's group");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"late");
		assert!(matches!(subscription.recv_group().await, Ok(None)), "ends cleanly");
	}

	/// A front resolved through a served route, as a relay's upstream session serves
	/// one: the upstream broadcast's track requests arrive on the returned handle.
	async fn served_front() -> (Dynamic, broadcast::Producer, broadcast::Dynamic, broadcast::Consumer) {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer
			.dynamic("room/alice", Route::default().with_hops(hops(&[10])))
			.unwrap();
		let pending = consumer.request_broadcast("room/alice");
		let upstream = broadcast::Info::new().produce();
		let dynamic = upstream.dynamic();
		queued(&server).await.accept(&upstream);
		let resolved = pending.await.expect("resolves");
		(server, upstream, dynamic, resolved)
	}

	/// A reader returning to a parked track waits for the fresh copy to resolve its
	/// start, and skips the warm cache when the copy resolves past it: the source
	/// judged the groups in between stale, so the older cache is stale too. Without the
	/// hold the reader was handed the whole warm cache first, seconds behind live.
	#[tokio::test]
	async fn returning_reader_skips_a_warm_cache_the_copy_resolved_past() {
		let ms = |v: u64| crate::Timestamp::from_millis(v).unwrap();
		let (_server, _upstream, mut dynamic, resolved) = served_front().await;
		let budget = track::Subscription::default().with_max_age(Duration::from_millis(100));

		let track = resolved.track("audio").unwrap();
		let b = budget.clone();
		let subscribing = tokio::spawn(async move { track.subscribe(b).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source")
			.expect("request");
		let source = request.resolving_start().accept(None);
		for seq in 0..4u64 {
			let mut group = source.create_group(seq.into()).unwrap();
			group.write_frame(ms(seq * 20), b"old".as_ref()).unwrap();
			group.finish().unwrap();
		}
		let mut subscription = subscribing.await.unwrap().expect("subscribe");
		subscription.recv_group().await.unwrap().expect("the live group");
		drop(subscription);
		tokio::time::timeout(Duration::from_secs(1), source.unused())
			.await
			.expect("parked")
			.expect("source open");
		drop(source);

		let track = resolved.track("audio").unwrap();
		let subscribing = tokio::spawn(async move { track.subscribe(budget).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source again")
			.expect("request");
		let mut source = request.resolving_start().accept(None);
		let mut subscription = subscribing.await.unwrap().expect("resubscribe");

		// The copy has not resolved its start: nothing is handed out yet.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), subscription.recv_group())
				.await
				.is_err(),
			"the warm cache was served before the copy resolved its start"
		);

		// The source resolves past the floor (lite-06 skipped 4..20 as stale).
		source.start_at(20).unwrap();
		let mut group = source.create_group(20u64.into()).unwrap();
		group.write_frame(ms(2000), b"new".as_ref()).unwrap();
		group.finish().unwrap();
		let group = subscription.recv_group().await.unwrap().expect("the live group");
		assert_eq!(group.sequence, 20, "a stale warm group was served");
	}

	/// A warm cache whose newest group finished still resumes when the source has
	/// nothing newer: the re-splice asks for that group's tail, which a source that
	/// resolves starts lazily (with its first served group) can answer at once. Asking
	/// past it left a returning catalog reader waiting for the next catalog change.
	#[tokio::test]
	async fn returning_reader_replays_a_current_warm_cache() {
		let (_server, _upstream, mut dynamic, resolved) = served_front().await;

		let track = resolved.track("catalog").unwrap();
		let subscribing = tokio::spawn(async move { track.subscribe(None).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source")
			.expect("request");
		let source = request.resolving_start().accept(None);
		let mut group = source.create_group(0u64.into()).unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"snapshot".as_ref()).unwrap();
		group.finish().unwrap();
		let mut subscription = subscribing.await.unwrap().expect("subscribe");
		subscription.recv_group().await.unwrap().expect("the catalog");
		drop(subscription);
		tokio::time::timeout(Duration::from_secs(1), source.unused())
			.await
			.expect("parked")
			.expect("source open");
		drop(source);

		let track = resolved.track("catalog").unwrap();
		let subscribing = tokio::spawn(async move { track.subscribe(None).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source again")
			.expect("request");
		let mut source = request.resolving_start().accept(None);
		let mut subscription = subscribing.await.unwrap().expect("resubscribe");

		// The source still has group 0 as its newest: it serves the empty tail, and
		// that is when its start resolves.
		let reading = tokio::spawn(async move {
			let mut group = subscription.recv_group().await.unwrap().expect("the catalog");
			assert_eq!(group.sequence, 0);
			group.read_frame().await.unwrap().expect("the snapshot").payload
		});
		tokio::task::yield_now().await;
		assert_eq!(
			source.subscription().and_then(|sub| sub.start),
			Some(track::Position { group: 0, frame: 1 }),
			"the re-splice asked past the cached catalog"
		);
		source.start_at(0).unwrap();
		let mut tail = source.create_group(0u64.into()).unwrap();
		tail.start_at(1).unwrap();
		tail.finish().unwrap();
		let payload = tokio::time::timeout(Duration::from_secs(1), reading)
			.await
			.expect("the returning reader never got the catalog")
			.unwrap();
		assert_eq!(&payload[..], b"snapshot");
	}

	/// A group that stays open for good (a JSON log in group 0) survives a park: the
	/// returning reader gets the frames delivered before it from the warm cache, and the
	/// re-splice asks the source only for the frames after them, across repeated parks.
	/// A datagram sequenced past the group does not hide it as the live edge.
	#[tokio::test]
	async fn returning_reader_continues_an_open_warm_group() {
		let (_server, _upstream, mut dynamic, resolved) = served_front().await;

		async fn read(group: &mut group::Consumer) -> Vec<u8> {
			let frame = tokio::time::timeout(Duration::from_secs(1), group.read_frame())
				.await
				.expect("frame")
				.unwrap()
				.expect("group ended");
			frame.payload.to_vec()
		}

		let mut expect: Vec<&[u8]> = Vec::new();
		let mut floor: Option<track::Position> = None;
		for (round, payload) in [b"a".as_ref(), b"b", b"c"].into_iter().enumerate() {
			let track = resolved.track("log").unwrap();
			let subscribing = tokio::spawn(async move { track.subscribe(None).await });
			let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
				.await
				.expect("the front asked the source")
				.expect("request");
			let mut source = request.resolving_start().accept(None);
			let mut subscription = subscribing.await.unwrap().expect("subscribe");

			// The source resolves at the floor's group, continuing group 0.
			source.start_at(0).unwrap();
			let mut group = source.create_group(0u64.into()).unwrap();
			if let Some(floor) = floor {
				group.start_at(floor.frame).unwrap();
			}
			group.write_frame(crate::Timestamp::ZERO, payload).unwrap();
			expect.push(payload);
			source
				.insert_datagram(10, crate::Timestamp::ZERO, b"datagram".as_ref())
				.unwrap();

			let mut reading = tokio::time::timeout(Duration::from_secs(1), subscription.recv_group())
				.await
				.expect("group 0")
				.unwrap()
				.expect("track ended");
			assert_eq!(reading.sequence, 0);
			for frame in &expect {
				assert_eq!(read(&mut reading).await, *frame, "round {round}");
			}
			assert_eq!(
				source.subscription().and_then(|sub| sub.start),
				floor,
				"round {round} asked for the wrong continuation"
			);

			drop(reading);
			drop(subscription);
			tokio::time::timeout(Duration::from_secs(1), source.unused())
				.await
				.expect("parked")
				.expect("source open");
			drop(group);
			drop(source);
			floor = Some(track::Position {
				group: 0,
				frame: expect.len() as u64,
			});
		}
	}

	/// The same holds for a reader returning to a parked track: its warm cache
	/// does not stand in for the copy it is waiting on.
	#[tokio::test]
	async fn unannounce_keeps_a_returning_reader_awaiting_its_info() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.publish("room/alice", Route::default()).unwrap();
		let mut dynamic = broadcast.dynamic();
		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");
		let track = resolved.track("video").unwrap();
		let subscribing = tokio::spawn(async move { track.subscribe(None).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source")
			.expect("request");
		let source = request.accept(None);
		let mut group = source.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"cached".as_ref()).unwrap();
		group.finish().unwrap();
		let mut subscription = subscribing.await.unwrap().expect("subscribe");
		subscription.recv_group().await.unwrap().expect("the cached group");
		drop(subscription);

		// Parked: the source copy goes, the delivered group stays warm. The source
		// then tears its idle track down, so a returning reader asks it afresh.
		tokio::time::timeout(Duration::from_secs(1), source.unused())
			.await
			.expect("parked")
			.expect("source open");
		drop(source);
		let track = resolved.track("video").unwrap();
		let subscribing = tokio::spawn(async move { track.subscribe(None).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source again")
			.expect("request");

		broadcast.unannounce();
		settle(|| resolved.is_closed()).await;

		// A fresh source copy numbers groups past what the cache already delivered.
		let source = request.accept(None);
		let mut group = source.create_group(1u64.into()).unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"late".as_ref()).unwrap();
		group.finish().unwrap();
		source.finish().unwrap();

		let mut subscription = subscribing.await.unwrap().expect("subscribe survives the retraction");
		let mut payloads = Vec::new();
		while let Some(mut group) = subscription.recv_group().await.expect("ends cleanly") {
			payloads.push(group.read_frame().await.unwrap().unwrap().payload);
		}
		assert_eq!(payloads.last().map(|p| &p[..]), Some(&b"late"[..]));
	}

	/// A re-announce that lands before the front acts on the retraction reuses
	/// the same route entry, so the front carries on.
	#[tokio::test]
	async fn reannounce_before_the_front_acts_keeps_it() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.publish("room/alice", Route::default()).unwrap();
		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");

		broadcast.unannounce();
		broadcast.announce(Route::default()).unwrap();
		for _ in 0..20 {
			tokio::task::yield_now().await;
		}
		assert!(!resolved.is_closed(), "the front ended across a reannouncement");
		let again = consumer.request_broadcast("room/alice").await.expect("resolves");
		assert!(again.is_clone(&resolved));
	}

	#[tokio::test]
	async fn local_broadcast_resolves_once_announced() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Created but not announced: nobody can reach it, locally included.
		let broadcast = producer.create_broadcast("room/alice").unwrap();
		let err = consumer
			.request_broadcast("room/alice")
			.now_or_never()
			.expect("unroutable is synchronous")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		broadcast.announce(Route::default()).unwrap();
		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");
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
		let Some(Some(AnnounceEvent::Announced(update))) = announced.next_route_now() else {
			panic!("expected the announcement");
		};
		assert_eq!(update.prefix.as_str(), "live");
		assert_eq!(update.route.cost, Cost::new(1));
		announced.assert_next_wait();

		drop(second);
		let Some(Some(AnnounceEvent::Updated(update))) = announced.next_route_now() else {
			panic!("expected the reprice");
		};
		assert_eq!(update.prefix.as_str(), "live");
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
		let mut next = || StreamExt::next(&mut announced).now_or_never();
		let Some(Some(AnnounceEvent::Announced(update))) = next() else {
			panic!("expected the replayed route");
		};
		assert_eq!(update.prefix.as_str(), "live");
		assert!(matches!(next(), Some(Some(AnnounceEvent::Live))));
		assert!(next().is_none());
		drop(server);
		assert!(matches!(next(), Some(Some(AnnounceEvent::Retracted(_)))));
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
	async fn local_broadcast_is_invisible_until_announced() {
		let producer = origin(1).produce();
		let mut local = producer.consume().announced();
		let mut peer = producer.consume().excluding(Hop::UNKNOWN).announced();
		let broadcast = producer.create_broadcast("room/alice").unwrap();
		local.assert_next_wait();
		peer.assert_next_wait();

		broadcast.announce(Route::default()).unwrap();
		local.assert_next_active("room/alice");
		peer.assert_next_active("room/alice");

		drop(broadcast);
		local.assert_next_ended("room/alice");
		peer.assert_next_ended("room/alice");
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
		let again = consumer.request_broadcast("room/alice").await.expect("resolves");
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

	#[tokio::test]
	async fn routes_report_where_they_entered() {
		let producer = origin(1).produce();
		let peer = producer.clone().peer();
		let mut announced = producer.consume().announced();

		let _ingest = producer
			.dynamic("client", Route::default().with_hops(hops(&[5])).with_via(origin(5)))
			.unwrap();
		let _gateway = producer.publish("gateway", Route::default()).unwrap();
		let _forwarded = peer
			.dynamic(
				"forwarded",
				Route::default().with_hops(hops(&[5, 7])).with_via(origin(7)),
			)
			.unwrap();

		assert_eq!(announced.assert_next_active("client").source(), Source::Local);
		assert_eq!(
			announced.assert_next_active("forwarded").source(),
			Source::Peer(origin(7))
		);
		assert_eq!(announced.assert_next_active("gateway").source(), Source::Local);

		// The mark survives narrowing the handle.
		let scoped = peer.scope("room", &Patterns::from(Pattern::all())).unwrap();
		let _nested = scoped.dynamic("x", Route::default().with_via(origin(8))).unwrap();
		assert_eq!(announced.assert_next_active("room/x").source(), Source::Peer(origin(8)));
	}

	/// A change of source alone is delivered: the same chain and cost arriving
	/// from a peer instead of a client is a different fact for the consumer.
	#[tokio::test]
	async fn source_change_is_an_update() {
		let producer = origin(1).produce();
		let peer = producer.clone().peer();
		let mut announced = producer.consume().announced();

		let route = Route::default().with_hops(hops(&[7])).with_via(origin(7));
		let _forwarded = peer.dynamic("room", route.clone()).unwrap();
		assert_eq!(announced.assert_next_active("room").source(), Source::Peer(origin(7)));

		// The newest identical route wins, so the local twin takes over.
		let local = producer.dynamic("room", route).unwrap();
		match announced.next().now_or_never().expect("next blocked").expect("no next") {
			AnnounceEvent::Updated(announce) => assert_eq!(announce.route.source(), Source::Local),
			other => panic!("should be an update: got {other:?}"),
		}

		drop(local);
		assert_eq!(announced.assert_next_active("room").source(), Source::Peer(origin(7)));
	}

	#[tokio::test]
	async fn local_view_hides_peer_routes() {
		let producer = origin(1).produce();
		let peer = producer.clone().peer();
		let mut local = producer.consume().local().announced();

		let _forwarded = peer
			.dynamic("remote", Route::default().with_hops(hops(&[7])).with_via(origin(7)))
			.unwrap();
		local.assert_next_wait();

		// A path both ingested here and forwarded by a peer shows the local route,
		// and retracts from the local view when the local route goes, even though
		// the peer's still covers it.
		let _shadow = peer
			.dynamic("both", Route::default().with_hops(hops(&[7])).with_via(origin(7)))
			.unwrap();
		let ingest = producer
			.dynamic(
				"both",
				Route::default().with_hops(hops(&[5])).with_via(origin(5)).with_cost(9),
			)
			.unwrap();
		assert_eq!(local.assert_next_active("both").source(), Source::Local);
		drop(ingest);
		local.assert_next_ended("both");

		// Resolution agrees with the cursor: a peer-only path is unroutable here,
		// while the full view queues the request on the peer's route.
		let err = producer
			.consume()
			.local()
			.request_broadcast("remote/alice")
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
		assert!(
			producer
				.consume()
				.request_broadcast("remote/alice")
				.now_or_never()
				.is_none()
		);
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

	/// Teardown rejects a parked request with `Dropped`, but a destroyed origin
	/// is `Closed` to `routed_broadcast`'s callers.
	#[tokio::test]
	async fn routed_broadcast_reports_teardown_as_closed() {
		let (producer, driver) = Producer::new(Config::new(origin(1)));
		let consumer = producer.consume();
		let _server = producer.dynamic("room", Route::default()).unwrap();

		// Park on the covering route, past the loop's closed check.
		let mut resolving = Box::pin(consumer.routed_broadcast("room/alice"));
		assert!((&mut resolving).now_or_never().is_none());

		drop(driver);

		let err = tokio::time::timeout(Duration::from_secs(5), resolving)
			.await
			.expect("teardown resolves the wait")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Closed), "unexpected end: {err}");
	}

	/// A local broadcast announcing at the exact path is a table change too: a
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

		// The more specific route wins outright over the handler's prefix.
		let _local = producer.publish("room/alice", Route::default()).unwrap();
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
		incumbent_track: track::Producer,
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
					incumbent_track: track,
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

		// The incumbent's session dies, taking its track with it: a live copy
		// would otherwise keep serving after the front ends.
		drop(incumbent);
		drop(source);
		rig.incumbent_track.abort(Error::Dropped).unwrap();

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
		rig.incumbent_track.abort(Error::Dropped).unwrap();

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

		let first = producer.publish("room/alice", Route::default()).unwrap();
		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");

		// A second source at the same path joins the same front.
		let second = producer.publish("room/alice", Route::default()).unwrap();
		let again = consumer.request_broadcast("room/alice").await.expect("resolves");
		assert!(again.is_clone(&resolved));

		// Losing one source keeps the front alive; losing both closes it.
		first.finish();
		settle(|| consumer.get_broadcast("room/alice").is_some()).await;
		second.finish();
		settle(|| consumer.get_broadcast("room/alice").is_none()).await;

		// The path is free again for a fresh broadcast.
		let _third = producer.publish("room/alice", Route::default()).unwrap();
		assert!(consumer.get_broadcast("room/alice").is_some());
	}

	/// The publisher finishes a track, then its broadcast. A subscription already in
	/// flight must conclude normally: the track's last group, then the end. moq-lite,
	/// ANNOUNCE_END: "Retraction does not disturb subscriptions already in flight,
	/// which conclude normally with SUBSCRIBE_END."
	///
	/// The runtime is single-threaded and the publisher's whole ending has no await in
	/// it, so the outcome does not depend on timing.
	#[tokio::test]
	async fn a_finished_broadcast_concludes_in_flight_subscriptions() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.publish("room/alice", Route::default()).unwrap();
		let track = broadcast.create_track("video", None).unwrap();

		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");
		let mut subscription = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		// A textbook clean end, innermost first: the group, the track, the broadcast.
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"tail".as_ref()).unwrap();
		group.finish().unwrap();
		track.finish().unwrap();
		drop(track);
		broadcast.finish();

		let mut group = next_group(&mut subscription)
			.await
			.expect("a cleanly finished track was served as an error")
			.expect("the track ended before its last group");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"tail");
		drop(group);

		let end = next_group(&mut subscription)
			.await
			.expect("a cleanly finished track ended as an error");
		assert!(end.is_none(), "a group followed the final one");
	}

	/// A route served from upstream is retracted (what the lite subscriber does on
	/// ANNOUNCE_END: finish the source it minted, drop the route) while the track's
	/// last group and end are still on their way. The subscription already in flight
	/// must still conclude normally. moq-lite, ANNOUNCE_END: "Retraction does not
	/// disturb subscriptions already in flight, which conclude normally with
	/// SUBSCRIBE_END."
	#[tokio::test]
	async fn a_retracted_route_concludes_in_flight_subscriptions() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer
			.dynamic("room", Route::default().with_hops(hops(&[10])))
			.unwrap();

		let pending = consumer.request_broadcast("room/alice");
		let request = queued(&server).await;
		let source = broadcast::Info::new().produce();
		let track = source.create_track("video", None).unwrap();
		request.accept(&source);

		let resolved = pending.await.expect("resolves");
		let mut subscription = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");

		// ANNOUNCE_END overtakes the track's end: the route is retracted, and the front
		// has acted on it, before the track's last group and end arrive.
		source.finish();
		drop(server);
		settle(|| resolved.is_closed()).await;
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"tail".as_ref()).unwrap();
		group.finish().unwrap();
		track.finish().unwrap();
		drop(track);

		let mut group = next_group(&mut subscription)
			.await
			.expect("a retracted route's track was served as an error")
			.expect("the track ended before its last group");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"tail");
		drop(group);

		let end = next_group(&mut subscription)
			.await
			.expect("a cleanly finished track ended as an error");
		assert!(end.is_none(), "a group followed the final one");
	}

	/// An origin front drops the source track as soon as its last reader leaves,
	/// so the publisher's `unused()` resolves far below `TRACK_IDLE_LINGER`.
	/// Cached groups stay on the front for the linger; a returning reader
	/// replays them and re-splices for groups past that edge.
	#[tokio::test]
	async fn origin_front_drops_the_source_when_unused() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.publish("room/alice", Route::default()).unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"cached".as_ref()).unwrap();
		group.finish().unwrap();

		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");
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

	/// A returning reader judges the warm cache against the logical track's live
	/// edge, not the parked segment's own frozen one: groups the fresh source
	/// has left behind by more than the budget are skipped, exactly as they would
	/// be on one unspliced track.
	///
	/// A local source is released rather than parked (it keeps its own cache), so
	/// this goes through a served front, which is what actually holds the warm copy.
	/// The copy resolves at the cached edge, not past it: resolving past it drops
	/// the cache outright, which is a different case.
	#[tokio::test]
	async fn resumed_reader_skips_warm_groups_behind_the_new_edge() {
		let ms = |v: u64| crate::Timestamp::from_millis(v).unwrap();
		let (_server, _upstream, mut dynamic, resolved) = served_front().await;
		let budget = track::Subscription::default().with_max_age(Duration::from_millis(100));
		let write = |source: &track::Producer, sequence: u64, millis: u64| {
			let mut group = source.create_group(sequence.into()).unwrap();
			group.write_frame(ms(millis), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		};
		let drain = |subscription: &mut track::Subscriber| {
			let mut sequences = Vec::new();
			while let Poll::Ready(group) = subscription.poll_recv_group(&kio::Waiter::noop()) {
				sequences.push(group.unwrap().expect("track ended").sequence);
			}
			sequences
		};

		let track = resolved.track("video").unwrap();
		let first = budget.clone();
		let subscribing = tokio::spawn(async move { track.subscribe(first).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source")
			.expect("request");
		let source = request.resolving_start().accept(None);
		for (sequence, millis) in [(0, 0), (1, 20), (2, 40), (3, 60)] {
			write(&source, sequence, millis);
		}
		let mut subscription = subscribing.await.unwrap().expect("subscribe");
		next_group(&mut subscription).await.unwrap().expect("a cached group");
		drain(&mut subscription);
		drop(subscription);

		tokio::time::timeout(Duration::from_secs(1), source.unused())
			.await
			.expect("parked")
			.expect("source open");
		drop(source);

		let track = resolved.track("video").unwrap();
		let second = budget.clone();
		let subscribing = tokio::spawn(async move { track.subscribe(second).await });
		let request = tokio::time::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source again")
			.expect("request");
		let mut source = request.resolving_start().accept(None);
		for (sequence, millis) in [(20, 400), (21, 420), (22, 440)] {
			write(&source, sequence, millis);
		}
		// At the cached edge, not past it, so the warm copy stays and the budget
		// decides. Past it, the copy has already judged the cache stale.
		source.start_at(3).unwrap();
		let mut subscription = subscribing.await.unwrap().expect("resubscribe");
		settle(|| subscription.latest() == Some(22)).await;

		// Groups 0..=2 reach at most 60ms against an edge at 440ms. Group 3 reaches
		// where group 20 starts, 40ms behind that edge, inside the 100ms budget.
		assert_eq!(drain(&mut subscription), [3, 20, 21, 22]);
	}

	/// A front serving from another front's spliced copy has no snapshot to keep:
	/// it still drops upstream on the unused edge, so the publisher's `unused()`
	/// resolves far below `TRACK_IDLE_LINGER` through the whole chain. The next
	/// reader re-splices, paying `TRACK_INFO` again.
	#[tokio::test]
	async fn chained_front_drops_the_source_when_unused() {
		let leaf = origin(1).produce();
		let leaf_consumer = leaf.consume();

		let broadcast = leaf.publish("room/alice", Route::default()).unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"cached".as_ref()).unwrap();
		group.finish().unwrap();

		// The leaf's front view: a spliced broadcast, so any front serving from
		// it holds a spliced source copy with nothing to snapshot.
		let leaf_front = leaf_consumer.request_broadcast("room/alice").await.expect("resolves");

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

		// A budget spanning the cache, so the returning reader replays it.
		let mut subscription = edge_resolved
			.track("video")
			.unwrap()
			.subscribe(track::Subscription::default().with_max_age(Duration::from_secs(3600)))
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

		let first = producer.publish("room/alice", Route::default()).unwrap();
		let track = first.create_track("video", None).unwrap();
		let resolved = consumer.request_broadcast("room/alice").await.expect("resolves");
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
		let second = producer.publish("room/alice", Route::default()).unwrap();
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
	fn route_table_prunes_to_empty() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Routes and cursors hang at their prefixes; the nodes on the way exist
		// only while something is there.
		let cursor = consumer
			.scope("", &scopes(&["room/a", "other/deep/head"]))
			.unwrap()
			.announced();
		let route = producer.announce("room/a/b/c", Route::default()).unwrap();
		{
			let table = producer.shared.lock();
			assert!(table.routes.root.find(Path::new("room/a/b/c").parts()).is_some());
			assert!(table.routes.root.find(Path::new("other/deep/head").parts()).is_some());
			assert_eq!(table.routes.root.cursors_below, 2);
		}

		drop(route);
		drop(cursor);
		let table = producer.shared.lock();
		assert!(table.routes.root.is_empty());
		assert_eq!(table.routes.root.cursors_below, 0);
	}

	/// A session handed an `origin::Producer` drops it once it has its own
	/// handles, so the driver must keep running while a published broadcast
	/// lives, and finish once the last one is gone.
	#[test]
	fn a_published_broadcast_keeps_the_driver_running() {
		let (producer, mut driver) = Producer::new(Config::new(origin(1)));
		let waiter = kio::Waiter::noop();
		let broadcast = producer.create_broadcast("room/a").unwrap();
		drop(producer);
		assert!(
			driver.poll(Instant::now(), &waiter).is_ok(),
			"the broadcast is lifecycle work"
		);
		drop(broadcast);
		assert!(matches!(driver.poll(Instant::now(), &waiter), Err(Error::Closed)));
	}

	#[test]
	fn watch_wakes_only_for_covering_changes() {
		let producer = origin(1).produce();
		let waiter = kio::Waiter::noop();
		let watch = producer.shared.lock().watch(&producer.shared, &Path::new("room/a"));
		let seen = watch.seen();

		// A route beside the path or beneath it covers nothing at the path.
		let _other = producer.announce("other", Route::default()).unwrap();
		let _below = producer.announce("room/a/b", Route::default()).unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_pending());

		// A route above it does, and so does its retraction.
		let above = producer.announce("room", Route::default()).unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_ready());
		let seen = watch.seen();
		drop(above);
		assert!(watch.poll_changed(&waiter, seen).is_ready());
		let seen = watch.seen();

		// A local broadcast attaching at the exact path does; one beside it does not.
		let _beside = producer.create_broadcast("room/b").unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_pending());
		let _here = producer.create_broadcast("room/a").unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_ready());

		// Dropping the watch takes it out of the table.
		drop(watch);
		let table = producer.shared.lock();
		let node = table
			.routes
			.root
			.find(Path::new("room/a").parts())
			.expect("route below keeps the node");
		assert!(node.watches.is_empty());
		assert_eq!(table.routes.root.watches_below, 0);
	}

	#[test]
	fn a_discarded_front_task_unregisters_its_watch() {
		let (producer, _driver) = Producer::new(Config {
			hop: origin(1),
			..Default::default()
		});
		let consumer = producer.consume();
		let _served = producer.dynamic("room", Route::default()).unwrap();
		// A consumer outlives its producer by design, so the task set can refuse
		// submissions while the origin is still open. The front's task is then
		// dropped on the spot, taking its `Watch` with it: the request must not
		// still be holding the table lock the watch unregisters under.
		drop(producer);
		let _pending = consumer.request_broadcast("room/a");
	}

	#[test]
	fn create_broadcast_refuses_a_path_no_pattern_can_spell() {
		let producer = origin(1).produce();

		// A `*` segment is a valid path but an invalid literal, so its route could
		// never be built: refuse the broadcast instead of publishing one that
		// announces nowhere.
		assert!(matches!(
			producer.create_broadcast("room/*"),
			Err(Error::InvalidPath(_))
		));
		assert!(matches!(
			producer.announce("room/**", Route::default()),
			Err(Error::InvalidPath(_))
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
		let Some(AnnounceEvent::Announced(update)) = announced.try_next_route() else {
			panic!("expected alice's chat");
		};
		assert_eq!(update.prefix.as_str(), "room/alice/chat");
		assert_eq!(update.captures, Some(vec!["alice".parse::<Pattern>().unwrap()]));

		let audio = producer.create_broadcast("room/alice/audio").unwrap();
		audio.announce(Route::default()).unwrap();
		announced.assert_next_wait();

		let broad = producer.announce("room", Route::default()).unwrap();
		let Some(AnnounceEvent::Announced(update)) = announced.try_next_route() else {
			panic!("expected the overlapping broad route");
		};
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
		let Some(AnnounceEvent::Announced(update)) = announced.try_next_route() else {
			panic!("expected one winning route");
		};
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
			.encode(&mut buf, crate::lite::Version::Lite06)
			.expect("a draining route is still forwarded, so its cost must encode");
	}
}
