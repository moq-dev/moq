use crate::{broadcast, cache, stats, track};
use kio::Task;
use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
	fmt,
	num::{NonZeroU64, NonZeroUsize},
	sync::Arc,
	sync::atomic::{AtomicU64, AtomicUsize, Ordering},
	task::{Context, Poll, Wake, Waker, ready},
	time::Duration,
};

use rand::RngExt;

use super::{
	RequestQueue, WeakCache, WeakEntry,
	announced::AnnounceConsumer,
	follow::{Follow, Following},
	front::{Action, Candidate, Event, Front, Refusal},
};
use crate::{
	Error, Instance, InvalidPattern, Path, Pattern, Patterns,
	coding::{BoundsExceeded, Decode, DecodeError, Decoder, Encode, EncodeError, Encoder, Form, varint},
	path::{self, Segment},
	time::{Deadlines, Instant},
	util::{Keepalive, SendCell, TaskSet, Tasks, TasksWeak},
};

/// One relay's identity in a broadcast's hop chain: any 64-bit id.
///
/// Names a *hop*, not an [`origin::Producer`](Producer): a relay's routing table is the
/// origin, and this is the id it stamps into a route's hop chain as an announcement
/// passes through, so a receiver can spot its own id and reject a loop.
///
/// A transit fact the origin and the relays fill in: applications pass plain ids
/// ([`Config::hop`]) and read them back ([`RouteInfo::hops`]).
///
/// Local hops come from a [`NonZeroU64`] or [`Hop::random`], so loop detection can work.
/// Remote peers may still send `0`; it is legal on the wire, names nobody, and marks the
/// chain anonymous for route selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Hop {
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

	/// A local hop from a stable id, or `None` for 0, which cannot be excluded for loop
	/// detection.
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn new(id: u64) -> Option<Self> {
		NonZeroU64::new(id).map(Self::from)
	}

	/// Generate a fresh hop with a random non-zero id; see [`random_id`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn random() -> Self {
		Self::from(random_id())
	}

	/// Return the hop's wire id.
	pub(crate) fn id(self) -> u64 {
		self.id
	}

	/// Build a hop from an id read off the wire, where 0 is legal.
	pub(crate) const fn from_wire(id: u64) -> Self {
		Self { id }
	}

	/// The id this hop carries on a wire whose varints use `form`.
	///
	/// A QUIC varint stops at 2^62 - 1, so a larger id travels as [`Self::UNKNOWN`]:
	/// the route still reaches the peer, losing loop detection through this hop on
	/// that link only.
	pub(crate) fn wire_id(self, form: Form) -> u64 {
		match form {
			Form::Quic if self.id > varint::MAX_QUIC => Self::UNKNOWN.id,
			_ => self.id,
		}
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
	/// The origin's identity on every wire: the hop id appended to broadcast hop chains
	/// for loop detection, and the route identity it declares on moq-lite-07, which also
	/// names it in the hops of each route it forwards there. Random by
	/// default, so each process is a fresh identity; one reused across a restart has its
	/// lite-07 routes refused until a seqno request lifts them past the old process's.
	pub hop: NonZeroU64,

	/// The key this origin proves to every lite-07 peer, which files the routes it sends
	/// under it when its grant is scoped. Origins sharing a key fail over as one. `None`
	/// proves nothing, so such a peer files them under a key of its session alone.
	pub route_key: Option<super::origin::RouteKey>,

	/// Longest hop path a session using this origin accepts or forwards (default 32).
	///
	/// Applies to moq-lite-03+ hop paths and the negotiated MoQ Cluster extension on
	/// moq-transport. A forwarded path counts this origin's own hop too, including
	/// when the receiving peer reconstructs that hop from the announce handshake.
	/// Protocols without a hop path cannot reveal or bound the depth they hide. No wire
	/// carries a path longer than 32, so a larger value acts as 32, and a moq-lite-07
	/// route's own hops stop there whatever this is.
	pub max_hops: NonZeroUsize,

	/// The cache pool broadcasts under this origin charge their groups into. It flows
	/// down the ownership chain (origin -> broadcast -> track -> group): a track opens
	/// an account against it, and its groups charge through that. It has no byte target
	/// and uses [`cache::DEFAULT_EXPIRY`] by default; a relay sets a shared configured
	/// pool (assign [`Self::pool`]) so cached groups across the whole process share
	/// one policy.
	pub pool: cache::Pool,

	/// Local ceiling on each track's max age under this origin.
	/// This caps how long a superseded group stays fresh here, and the delivery budget,
	/// without changing the publisher's [`max_age`](track::Info::max_age) metadata. Wall-clock
	/// reclamation of idle content is separate: [`Self::pool`]'s
	/// [`expiry`](cache::Pool::expiry) window. `None` (the default) imposes no ceiling,
	/// leaving each track's own window in force.
	pub cache_duration: Option<Duration>,

	/// How long an announce cursor holds a changed route before delivering it.
	///
	/// A new route, a removed one, and another instance are delivered at once; only an
	/// update waits, and a newer change during the wait replaces it. When a publisher
	/// withdraws, a relay that loses its best route but still holds routes derived from
	/// the withdrawn one sends nothing while the withdrawal wave removes them, then one
	/// retraction, instead of advertising each stale path in turn. It must outlast
	/// the wave's spread across the mesh. Defaults to 300 ms, longer than a withdrawal
	/// takes to cross a global mesh, with margin; zero delivers updates at once.
	///
	/// A relay further out can still pass a stale path to a cursor that already
	/// retracted the prefix, which would announce it anew. So for two seconds after
	/// a retraction, an announcement from the same publisher (first hop, instance and
	/// epoch) on a strictly worse route waits. A republish ranks the same and a restart
	/// carries a new instance or epoch, so neither waits. Zero disables this too.
	pub update_hold: Duration,
}

/// The default [`Config::update_hold`].
pub(super) const DEFAULT_UPDATE_HOLD: Duration = Duration::from_millis(300);

/// How long a cursor holds back a stale path to a prefix it retracted; see
/// [`Config::update_hold`]. Path hunting after a withdrawal settles within it on
/// random meshes with links of up to 250 ms.
const RETRACT_QUARANTINE: Duration = Duration::from_secs(2);

impl Default for Config {
	/// A fresh random hop with no byte target and the default idle expiry.
	fn default() -> Self {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY));
		Self {
			hop: random_id(),
			route_key: None,
			max_hops: MAX_HOP_LIMIT,
			pool,
			cache_duration: None,
			update_hold: DEFAULT_UPDATE_HOLD,
		}
	}
}

impl Config {
	/// Config for the given origin hop id with no byte target and the default idle expiry.
	pub fn new(hop: NonZeroU64) -> Self {
		Self { hop, ..Self::default() }
	}
}

/// A fresh random hop id, for any origin or peer that needs no stable identity across
/// restarts.
///
/// Older `@moq/lite` clients decode the exclude hop as a JavaScript number and reject
/// values above 2^53-1, so generated ids stay in that range while [`Config::hop`] accepts
/// any non-zero 64-bit id.
pub(crate) fn random_id() -> NonZeroU64 {
	NonZeroU64::MIN.saturating_add(kio::clock::rng().random_range(0..(1u64 << 53) - 1))
}

impl From<NonZeroU64> for Hop {
	/// A local hop: its non-zero id can be excluded for loop detection.
	fn from(id: NonZeroU64) -> Self {
		Self { id: id.get() }
	}
}

impl fmt::Display for Hop {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.id.fmt(f)
	}
}

impl<V> Encode<V> for Hop {
	fn encode(&self, w: &mut Encoder<'_>, _: V) -> Result<(), EncodeError> {
		w.varint(self.wire_id(w.form()))
	}
}

impl<'a, V> Decode<'a, V> for Hop {
	fn decode(r: &mut Decoder<'a>, _: V) -> Result<Self, DecodeError> {
		Ok(Self::from_wire(r.varint()?))
	}
}

/// Maximum number of origins (hops) an [`Hops`] can hold.
///
/// Caps pathological or loop-induced announcements at a reasonable cluster
/// diameter; appending past this limit returns [`InvalidHop::TooMany`] rather than
/// silently truncating.
pub(super) const MAX_HOPS: usize = 32;

/// The default and highest [`Config::max_hops`].
const MAX_HOP_LIMIT: NonZeroUsize = NonZeroUsize::new(MAX_HOPS).unwrap();

/// Bounded, loop-free list of [`Hop`] entries: the hop chain of a broadcast.
///
/// Guarantees `len() <= MAX_HOPS` and that no non-zero [`Hop`] appears twice. Both
/// are wire rules, and both hold wherever a list exists rather than only where one was
/// parsed, so a chain that a conforming receiver would reject cannot be built and sent.
/// Construct via [`Hops::new`] + [`Hops::push`], or fall back to the
/// fallible [`TryFrom<Vec<Hop>>`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Hops(Vec<Hop>);

/// Why a [`Hop`] cannot join a [`Hops`] chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvalidHop {
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
			InvalidHop::Duplicate => DecodeError::InvalidValue,
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
	#[cfg(test)]
	pub fn as_slice(&self) -> &[Hop] {
		&self.0
	}

	/// The entries' ids, in hop order, reusing the list's allocation.
	#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
	pub(crate) fn into_ids(self) -> Vec<u64> {
		self.0.into_iter().map(|hop| hop.id).collect()
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

impl<V: Copy> Encode<V> for Hops {
	fn encode(&self, w: &mut Encoder<'_>, version: V) -> Result<(), EncodeError> {
		w.varint(self.0.len() as u64)?;
		for origin in &self.0 {
			origin.encode(w, version)?;
		}
		Ok(())
	}
}

impl<'a, V: Copy> Decode<'a, V> for Hops {
	fn decode(r: &mut Decoder<'a>, version: V) -> Result<Self, DecodeError> {
		let count = r.varint()? as usize;
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

/// The highest route cost, shared by every wire version.
const MAX_COST: u64 = (1 << 62) - 1;

/// The static cost of pulling content through a route; lower wins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cost(u64);

impl Cost {
	/// Price a route, saturating at the wire ceiling.
	pub const fn new(cost: u64) -> Self {
		Self(if cost > MAX_COST { MAX_COST } else { cost })
	}

	/// The accumulated route price.
	pub const fn value(self) -> u64 {
		self.0
	}

	/// The wire ceiling every price saturates at.
	pub(crate) const MAX: Self = Self(MAX_COST);

	/// A peer without a wire cost: the same as [`Cost::default`], so only the
	/// local link price counts.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) const UNKNOWN: Self = Self(0);

	/// Add a link's static price without overflowing the wire ceiling.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn charged(self, link_cost: u64) -> Self {
		Self::new(self.0.saturating_add(link_cost))
	}
}

impl From<u64> for Cost {
	fn from(cost: u64) -> Self {
		Self::new(cost)
	}
}

/// How much an endpoint prefers a link over its others; higher wins.
///
/// Every link starts at the neutral [`Preference::default`] (0), which is also what
/// a broadcast published locally ranks. A positive value promotes a link and a
/// negative one demotes it. See [`Producer::with_preference`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Preference(i8);

impl Preference {
	/// Rank a link.
	pub const fn new(preference: i8) -> Self {
		Self(preference)
	}

	/// The rank.
	pub const fn value(self) -> i8 {
		self.0
	}
}

impl From<i8> for Preference {
	fn from(preference: i8) -> Self {
		Self::new(preference)
	}
}

/// The stream a received route serves and what using it costs: read-only facts on an
/// announcement.
///
/// Carried by [`Announce::route`]. A route claims capability, not inventory: it says
/// paths under its prefix are servable, never that any specific broadcast exists. The
/// common convention is that a publisher announces each broadcast's exact path with
/// [`Producer::route`], so subscribers can enumerate broadcasts; a service instead
/// announces one short prefix with [`Producer::claim`] and answers whatever is
/// requested beneath it.
///
/// Applications never build one: a [`Route`] takes [`RouteOptions`], and the origin
/// and the relays fill in the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteInfo {
	/// The stream the route serves; see [`Self::instance`]. Always set on a route the
	/// origin delivers. Given to a route a session lands, `None` mints one.
	pub(crate) instance: Option<Instance>,

	/// Orders the incarnations of one instance: among its routes at one prefix only the
	/// highest epoch is eligible. Never compared across instances, and never shown to
	/// readers.
	pub(crate) epoch: u64,

	/// The chain of origins the route has traversed, oldest first. Each relay appends its
	/// own hop when forwarding; used for loop detection and as the selection tie-break. A
	/// 0 entry is the anonymous mark and travels unchanged, ranking the route below every
	/// fully identified one. A route announced on this origin has none.
	pub(crate) hops: Hops,

	/// What pulling content via this route costs; see [`Self::cost`].
	pub(crate) cost: Cost,

	/// The credential the request advertising this route carries, from
	/// `auth::Handle::add`, or `None` for the session's scope.
	///
	/// Only a session built on the handle that issued it carries it, as the token on its
	/// moq-transport PUBLISH_NAMESPACE: any other session, the inline NAMESPACE answering a
	/// solicited SUBSCRIBE_NAMESPACE, and moq-lite (which rides the session's scope)
	/// advertise the route unnamed. Swapping it is an update that renews the request with
	/// REQUEST_UPDATE. A handle passed to
	/// `Client::with_auth` presents it on every session it backs, so the route keeps it
	/// across a reconnect. Local, like [`Self::source`]: a relay never forwards it.
	pub(crate) auth: Option<crate::auth::Credential>,

	/// The announcing session's declared or assigned identity.
	///
	/// Local selection state: split-horizon matches this as well as the chain, so a
	/// route is never advertised back to the session it came from even when that
	/// session withheld an identity (hop 0). Never forwarded.
	pub(crate) via: Hop,

	/// Where the route entered this origin; see [`Self::source`]. Never forwarded.
	pub(crate) source: Source,

	/// The origin node that serves the route, for a route the lite-07 route layer
	/// selected: a request resolved through it names that node to the next hop.
	/// `None` for a route this node serves itself, published here or learned from a
	/// peer that names no node.
	pub(crate) node: Option<u64>,

	/// The session it arrived on is going away (the peer sent a GOAWAY): it ranks below
	/// every other eligible route, whatever its preference or cost, yet still serves when
	/// nothing else does.
	///
	/// A preference state, never a saturated metric. Local, like [`Self::via`]: never
	/// forwarded, so a relay advertises a draining route at its real cost and its
	/// readers never see the flag.
	pub(crate) draining: bool,

	/// Whether a usable path reaches the route's origin; see [`Self::available`].
	pub(crate) available: bool,

	/// The round trip to the route's origin node; see [`Self::rtt`]. Never forwarded:
	/// each node measures its own link.
	pub(crate) rtt: Option<Duration>,
}

impl RouteInfo {
	/// No route yet: no instance, hops, cost or credential, entered here. Crate-private
	/// because a delivered route always names an instance.
	pub(crate) fn new() -> Self {
		Self {
			instance: None,
			epoch: 0,
			hops: Hops::new(),
			cost: Cost::default(),
			auth: None,
			via: Hop::UNKNOWN,
			source: Source::Local,
			node: None,
			draining: false,
			available: true,
			rtt: None,
		}
	}
}

/// Where a route entered an origin: here, or from a cluster peer.
///
/// Origin bookkeeping, not a chain fact: the hop chain cannot say it, since a
/// client and a peer relay each append one hop. The origin records it from the
/// handle that announced the route; see [`Producer::with_link`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Source {
	/// Announced on this origin: by an in-process producer or a client session.
	#[default]
	Local,
	/// Learned from a cluster peer, named by the hop id the announcing session declared
	/// or was assigned. 0 when the announcing handle named none: a peer handle in-process,
	/// or a session whose peer declared no hop and was assigned none.
	Peer(u64),
}

impl RouteInfo {
	/// The stream the route serves: routes with one instance serve one continuous
	/// key space, so a subscription resumes from one to another. Pass it to
	/// [`Consumer::request`] to pin this stream.
	pub fn instance(&self) -> &Instance {
		self.instance.as_ref().expect("a delivered route has an instance")
	}

	/// What pulling content via this route costs, accumulated per link: lower wins
	/// among routes of the same link preference and anonymity, with ties broken by a
	/// broadcast published on this origin, then hop length, then a hash of the instance
	/// and the requested path (except for a route that entered here, at its own path),
	/// and finally the most recently announced route. See [`Cost`].
	pub fn cost(&self) -> Cost {
		self.cost
	}

	/// The round trip to the route's origin node over moq-lite-07, informational: it
	/// never ranks or selects a route.
	///
	/// Each link on the path adds its min RTT, the lowest smoothed RTT it measured over
	/// the last 30 seconds, this session's own link included. `None` on any other wire,
	/// on a route this origin serves itself, and while any link on the path has no
	/// measurement.
	pub fn rtt(&self) -> Option<Duration> {
		self.rtt
	}

	/// Whether a usable path reaches the route's origin. An unavailable route is held:
	/// the network lost every path to it but may recover one, so the announcement stays.
	/// It ranks with a draining route, below every available one, and requests through
	/// it may fail until it recovers: request again once it turns available.
	pub fn available(&self) -> bool {
		self.available
	}

	/// Serve a stable `instance` at `epoch`; see [`RouteOptions::with_instance`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_instance(mut self, instance: Instance, epoch: u64) -> Self {
		self.instance = Some(instance);
		self.epoch = epoch;
		self
	}

	/// Replace the hop chain.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_hops(mut self, hops: Hops) -> Self {
		self.hops = hops;
		self
	}

	/// Set the cost: lower wins among routes covering the same prefix and anonymity.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_cost(mut self, cost: impl Into<Cost>) -> Self {
		self.cost = cost.into();
		self
	}

	/// Set the credential the advertising request carries; see [`Self::auth`].
	#[cfg(all(test, feature = "async"))]
	pub(crate) fn with_auth(mut self, auth: crate::auth::Credential) -> Self {
		self.auth = Some(auth);
		self
	}

	/// The announcing session's declared or assigned identity, for split-horizon.
	///
	/// Not part of the advertised route: an assigned identity is private selection
	/// state and must not be forwarded.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_via(mut self, via: Hop) -> Self {
		self.via = via;
		self
	}

	/// The origin node a route-layer route serves, which a request resolved through it
	/// names to the next hop.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_node(mut self, node: Option<u64>) -> Self {
		self.node = node;
		self
	}

	/// Rank the route last, or not; see [`Self::draining`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_draining(mut self, draining: bool) -> Self {
		self.draining = draining;
		self
	}

	/// Mark whether a usable path reaches the route's origin; see [`Self::available`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_available(mut self, available: bool) -> Self {
		self.available = available;
		self
	}

	/// Set the round trip to the route's origin node; see [`Self::rtt`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_rtt(mut self, rtt: Option<Duration>) -> Self {
		self.rtt = rtt;
		self
	}

	/// Whether this route passed through an anonymous hop.
	///
	/// True when the chain holds a 0 anywhere, including Lite03 hop-count
	/// placeholders. An anonymous route ranks below every fully identified one,
	/// whatever the costs say. An empty chain is a local announcement, not the
	/// anonymous mark; ingress fills a received empty list with 0 before it
	/// enters the table.
	#[cfg(test)]
	pub(crate) fn is_anonymous(&self) -> bool {
		self.hops.iter().any(|hop| *hop == Hop::UNKNOWN)
	}

	/// Where the route entered this origin.
	pub fn source(&self) -> Source {
		self.source
	}
}

/// How an application advertises a [`Route`]: its visibility at creation, its price,
/// its credential, and, rarely, a stable instance.
///
/// The default announces at once, at cost 0, under the session's scope, serving a
/// fresh random instance minted when the route is created.
#[derive(Clone, Debug, Default)]
pub struct RouteOptions {
	hidden: bool,
	cost: Cost,
	auth: Option<crate::auth::Credential>,
	instance: Option<(Instance, u64)>,
}

impl RouteOptions {
	/// Create the route hidden: it exists for nobody until [`Route::announce`].
	///
	/// For a publisher that fills its catalog first, or answers a negotiation, before
	/// readers may find it.
	pub fn hidden() -> Self {
		Self {
			hidden: true,
			..Self::default()
		}
	}

	/// Set the cost: lower wins among routes covering the same prefix and anonymity.
	/// A publisher prices content it already produces at zero, and content it would
	/// have to start producing on demand higher.
	pub fn with_cost(mut self, cost: impl Into<Cost>) -> Self {
		self.cost = cost.into();
		self
	}

	/// Set the credential the advertising request carries, from `auth::Handle::add`.
	///
	/// Only a session built on the handle that issued it presents it, as the token on
	/// its moq-transport PUBLISH_NAMESPACE; every other session advertises the route
	/// under its own scope.
	pub fn with_auth(mut self, auth: crate::auth::Credential) -> Self {
		self.auth = Some(auth);
		self
	}

	/// Serve a stable `instance` at `epoch`, in place of a freshly minted one.
	///
	/// Only for a publisher that continues the instance's key space across restarts,
	/// announcing a higher `epoch` each time, such as a counter persisted on disk. Readers
	/// stitch every route of one instance together, so a publisher that restarts its
	/// group keys under an old instance splices unrelated groups into their tracks.
	/// The highest epoch wins over every older one of the instance, cheaper or not.
	/// Naming an instance grants nothing: publishing still needs the path's grant.
	pub fn with_instance(mut self, instance: Instance, epoch: u64) -> Self {
		self.instance = Some((instance, epoch));
		self
	}

	/// The route metadata these options advertise, under `instance` at `epoch`.
	/// The price and credential to advertise, naming no instance.
	fn info(&self) -> RouteInfo {
		RouteInfo {
			cost: self.cost,
			auth: self.auth.clone(),
			..RouteInfo::new()
		}
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

/// FNV-1a over a requested path and then `bytes`.
///
/// FNV-1a, not the std hasher: its output is fixed across Rust versions and
/// builds, which matters when nodes run mismatched binaries during a rolling
/// deploy and still need to agree on the same route. SEED is a custom basis
/// (any nonzero u64 works, the textbook one is just as arbitrary); FNV_PRIME is
/// the standard FNV-64 prime and should stay put. Mixing the path in spreads
/// equal routes across different paths rather than funneling onto one.
fn fnv_key(path: &str, bytes: impl IntoIterator<Item = u8>) -> u64 {
	const SEED: u64 = 0x420C0DECB00B; // 420 C0DEC B00B
	const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

	let mut hash = SEED;
	for byte in path.as_bytes().iter().copied().chain(bytes) {
		hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
	}
	hash
}

/// The hash that spreads a pool's instances over `path`s: the instance, unlike the hop
/// chain, reads the same on every node, so every relay picks the same one.
fn instance_key(path: &str, instance: &Instance) -> u64 {
	fnv_key(path, instance.as_str().bytes())
}

/// The hash that settles among the routes of one instance: their hop chains, so a node
/// converges on one path rather than following each newer announcement of an equal one.
fn chain_key(path: &str, hops: &Hops) -> u64 {
	fnv_key(path, hops.iter().flat_map(|hop| hop.id().to_le_bytes()))
}

/// The highest epoch each instance reaches among a set of candidates, per prefix: an older
/// incarnation of an instance is never eligible, however cheap, since its publisher was
/// replaced. Built in one pass, so ranking N candidates stays linear. Only epochs above 0
/// are kept, since nothing is below 0: the common case, where every instance announces
/// epoch 0, allocates nothing.
struct Epochs<'a>(HashMap<(&'a Path, &'a Instance), u64>);

impl<'a> Epochs<'a> {
	fn of(candidates: impl Iterator<Item = &'a RouteEntry>) -> Self {
		let mut highest = HashMap::new();
		for entry in candidates.filter(|entry| entry.epoch > 0) {
			let epoch = highest.entry((&entry.prefix, &entry.instance)).or_insert(0);
			*epoch = (*epoch).max(entry.epoch);
		}
		Self(highest)
	}

	/// Whether a candidate carries `entry`'s instance at a higher epoch on the same prefix.
	fn superseded(&self, entry: &RouteEntry) -> bool {
		self.0
			.get(&(&entry.prefix, &entry.instance))
			.is_some_and(|&highest| highest > entry.epoch)
	}
}

/// Ordering key for an eligible route entry resolving `path` (see [`Epochs`]).
/// Lower wins: first a route that is neither draining (see [`RouteInfo::draining`]) nor
/// unavailable (see [`RouteInfo::available`]), so a peer that sent a GOAWAY or a held
/// route ranks below every other eligible route and serves only as a last resort. Then the higher link [`Preference`], so metrics from different operators
/// are never compared. Then an identified chain (no 0) outranks an anonymous one regardless
/// of cost, then the cheapest cost, then a broadcast published on this origin (it serves
/// what is here, not a claim that has to ask), then the shortest hop chain, then a hash
/// of the instance and `path`, so every node converges on the same winner, then, among
/// the routes of that instance, a hash of the chain, and finally the newest announcement,
/// so a reconnect wins the moment it lands instead of after the transport retires the
/// old session.
///
/// `path` is what is being resolved: the requested path for a request, the prefix
/// itself for an advertisement. Keying the hash on the requested path is what
/// spreads a pool of instances at one prefix: each path picks its own winner, rather
/// than every path under the prefix hashing alike.
///
/// A route that entered here for exactly `path` is not hashed: no other node ranks
/// it, so convergence does not need the hash, and a restarted publisher announces
/// under a new instance, so a hash would keep its dead session winning half the time
/// until the transport times out.
fn route_order(path: &Path, entry: &RouteEntry) -> RouteOrder {
	// A lite-07 claim ranks as the one hop to its origin node: the rest of its hops are
	// the moq-lite-06 chain it carries only for loop detection and translation.
	let (anonymous, hops) = match entry.node {
		Some(_) => (false, 1),
		None => (entry.is_anonymous(), entry.hops.len()),
	};
	(
		entry.draining || !entry.available,
		Reverse(entry.preference),
		anonymous,
		entry.cost,
		!entry.local,
		hops,
		match (entry.node, entry.link == Link::Local && entry.prefix == *path) {
			(_, true) => (0, 0),
			// Every node that knows the claim ranks its origin node alike.
			(Some(node), false) => (fnv_key(path.as_str(), node.to_le_bytes()), 0),
			(None, false) => (
				instance_key(path.as_str(), &entry.instance),
				chain_key(path.as_str(), &entry.hops),
			),
		},
		Reverse(entry.id),
	)
}

/// The key [`route_order`] ranks an entry by: lower wins.
type RouteOrder = (
	bool,
	Reverse<Preference>,
	bool,
	Cost,
	bool,
	usize,
	(u64, u64),
	Reverse<u64>,
);

/// The `(hops, cost, source, instance, epoch, auth, node, available, rtt)` metadata
/// an announce cursor delivers alongside a prefix.
type RouteMeta = (
	Hops,
	Cost,
	Source,
	Instance,
	u64,
	Option<crate::auth::Credential>,
	Option<u64>,
	bool,
	Option<Duration>,
);

/// The earlier of two optional deadlines.
fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(a, b) => a.or(b),
	}
}

/// A retraction's quarantine, keyed by prefix in its [`Routes`]: the
/// retracted route's publisher (none for an anonymous route, which holds
/// nothing), instance, epoch, and rank, and when the quarantine ends.
#[derive(Clone)]
struct Quarantine {
	held: Held,
	publisher: Option<Hop>,
	instance: (Instance, u64),
	rank: (Cost, usize),
}

impl Quarantine {
	/// Whether `meta` could be a stale path to the retracted publisher: the same
	/// first hop, instance and epoch, ranked strictly worse. A republish ranks the same
	/// and a restart carries a new instance or epoch, so neither waits.
	fn holds(&self, meta: &RouteMeta) -> bool {
		self.publisher.is_some_and(|hop| meta.0.iter().next() == Some(&hop))
			&& meta.3 == self.instance.0
			&& meta.4 == self.instance.1
			&& (meta.1, meta.0.len()) > self.rank
	}
}

/// A pending update's hold, kept with it in its [`Routes`]'s queue.
///
/// A consumer reads the origin's clock outside the driver, where it holds the
/// driver's last reading, not the time the update arrived. So a hold first waits
/// for the driver's next reading (armed a nanosecond past the stale one, which
/// wakes the driver at once), and counts from there.
#[derive(Clone, Copy, PartialEq)]
enum Held {
	Anchoring(Instant),
	Until(Instant),
}

/// One coalesced update queued for an `Routes`.
///
/// At most one entry exists per prefix, so a slow consumer's pending set is
/// bounded by the number of distinct prefixes. A metadata change on a live route
/// overwrites the pending `Announce` (or is delivered as another active update),
/// a pending `Restart` stays one until delivered whatever metadata follows it,
/// and `UnannounceAnnounce` preserves a real retract-then-announce key.
type AnnounceMeta = (RouteMeta, Option<Vec<Pattern>>);

enum PendingUpdate {
	Announce(AnnounceMeta),
	Restart(AnnounceMeta),
	Unannounce(AnnounceMeta, Gone),
	UnannounceAnnounce {
		old: AnnounceMeta,
		gone: Gone,
		new: AnnounceMeta,
	},
}

/// How a route left a cursor: the origin ended it, or this node lost it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gone {
	/// Its origin withdrew it, as far as this node knows: [`RouteKind::End`].
	End,
	/// This node let go of it, such as the session it arrived on closing:
	/// [`RouteKind::Offline`].
	Offline,
}

impl Gone {
	fn kind(self) -> RouteKind {
		match self {
			Self::End => RouteKind::End,
			Self::Offline => RouteKind::Offline,
		}
	}
}

// An update is taken only while collapsing it under the cursor state lock.
struct Pending {
	update: Option<PendingUpdate>,
	// A later live event must not consume an earlier snapshot entry's debt.
	owed: bool,
	/// The entry's place in delivery order: when it was queued.
	seq: u64,
	/// When a poll looks at the entry again, in [`OriginConsumerState::parked`],
	/// or `None` while it is in [`OriginConsumerState::due`].
	wake: Option<Instant>,
	/// The update hold, from the first poll that saw the entry as one.
	held: Option<Held>,
}

/// Pending updates keep their insertion order when metadata changes. Fully removing
/// cancelled entries bounds memory by prefixes with outstanding work.
///
/// Every entry sits in exactly one of `due` and `parked`, so a poll touches only
/// what changed or came due since the last one, never the entries it holds back.
#[derive(Default)]
struct OriginConsumerState {
	pending: HashMap<Path, Pending>,
	/// The entries a poll looks at next, in delivery order: queued or changed since,
	/// or come due.
	due: BTreeMap<u64, Path>,
	/// The entries held back, by when a poll looks at them again.
	parked: BTreeMap<(Instant, u64), Path>,
	next_seq: u64,
	/// Prefixes whose most recently delivered update was an announce. A pending
	/// `Announce` is ambiguous on its own: it is an unseen initial announce (a
	/// retraction cancels it entirely) or a metadata update on a route the
	/// consumer already observed (a retraction must still be delivered).
	delivered: BTreeSet<Path>,
	/// Set by the origin's teardown: the cursor drains `pending`, then reports
	/// the end instead of parking forever on a table that can never fire again.
	ended: bool,
	/// Snapshots in flight that can present on this consumer's cursors, counted
	/// once per cursor they touch. See [`Routes::poll_synced`].
	///
	/// Changed through a read guard, so a session joining wakes none of the cursors it
	/// holds back (nothing waits on losing sync), except the last landing, which
	/// takes the write guard to owe the queue too. Only the table lock changes it.
	waiting: AtomicUsize,
	/// Pending entries from before the last snapshot landed. Synced at zero
	/// when no source is `waiting`; held metadata changes are not counted.
	owed: usize,
	/// Entries the polls have looked at, which the CPU a poll costs follows.
	#[cfg(test)]
	steps: usize,
}

impl OriginConsumerState {
	/// Whether the consumer has read everything its sources announced before they
	/// synced. A torn-down origin has no sources left to wait on.
	fn synced(&self) -> bool {
		(self.waiting.load(Ordering::Relaxed) == 0 || self.ended) && self.owed == 0
	}

	/// Owe every pending update that changes which prefixes are announced: a start
	/// the consumer has not seen, or an end. A re-price or restart of a delivered
	/// route leaves the set as it is, and may be held, so it is not owed.
	fn owe(&mut self) {
		self.owed = 0;
		for (prefix, pending) in &mut self.pending {
			pending.owed = !self.delivered.contains(prefix)
				|| matches!(
					pending.update,
					Some(PendingUpdate::Unannounce(..) | PendingUpdate::UnannounceAnnounce { .. })
				);
			self.owed += usize::from(pending.owed);
		}
	}

	/// Queue `update` at `prefix`, or replace the one queued there in its place. A
	/// replaced entry is looked at again on the next poll.
	fn put(&mut self, prefix: Path, update: PendingUpdate) {
		let Some(pending) = self.pending.get_mut(&prefix) else {
			self.insert(prefix, update, false);
			return;
		};
		pending.update = Some(update);
		if let Some(wake) = pending.wake.take() {
			self.parked.remove(&(wake, pending.seq));
			self.due.insert(pending.seq, prefix);
		}
	}

	/// Queue `update` at `prefix`, which has nothing queued, last in delivery order.
	fn insert(&mut self, prefix: Path, update: PendingUpdate, owed: bool) {
		let seq = self.next_seq;
		self.next_seq += 1;
		self.due.insert(seq, prefix.clone());
		self.pending.insert(
			prefix,
			Pending {
				update: Some(update),
				owed,
				seq,
				wake: None,
				held: None,
			},
		);
	}

	/// Unqueue the entry at `prefix`.
	fn remove(&mut self, prefix: &Path) -> Option<Pending> {
		let pending = self.pending.remove(prefix)?;
		match pending.wake {
			Some(wake) => self.parked.remove(&(wake, pending.seq)),
			None => self.due.remove(&pending.seq),
		};
		Some(pending)
	}

	/// The origin tore down: drain everything queued, holds and all, then end.
	fn end(&mut self) {
		self.ended = true;
		for ((_, seq), prefix) in std::mem::take(&mut self.parked) {
			self.pending.get_mut(&prefix).unwrap().wake = None;
			self.due.insert(seq, prefix);
		}
	}

	fn apply_announce(&mut self, prefix: Path, meta: RouteMeta, captures: Option<Vec<Pattern>>) {
		let meta = (meta, captures);
		let new = match self.pending.get_mut(&prefix).and_then(|pending| pending.update.take()) {
			// First announce, a stale announce being replaced, or a metadata update.
			None | Some(PendingUpdate::Announce(_)) => PendingUpdate::Announce(meta),
			// The consumer still owes the restart; this only moves its metadata.
			Some(PendingUpdate::Restart(_)) => PendingUpdate::Restart(meta),
			// Consumer needs to observe the retraction before this announce.
			Some(PendingUpdate::Unannounce(old, gone) | PendingUpdate::UnannounceAnnounce { old, gone, .. }) => {
				PendingUpdate::UnannounceAnnounce { old, gone, new: meta }
			}
		};
		self.put(prefix, new);
	}

	/// Another instance replaces the route at `prefix`, or announces it afresh when
	/// the consumer never saw the old one.
	fn apply_restart(&mut self, prefix: Path, meta: RouteMeta, captures: Option<Vec<Pattern>>) {
		let meta = (meta, captures);
		let new = match self.pending.get_mut(&prefix).and_then(|pending| pending.update.take()) {
			Some(PendingUpdate::Unannounce(old, gone) | PendingUpdate::UnannounceAnnounce { old, gone, .. }) => {
				PendingUpdate::UnannounceAnnounce { old, gone, new: meta }
			}
			Some(PendingUpdate::Restart(_)) => PendingUpdate::Restart(meta),
			_ if self.delivered.contains(&prefix) => PendingUpdate::Restart(meta),
			// The consumer never saw the old instance, so this is its first announce.
			_ => PendingUpdate::Announce(meta),
		};
		self.put(prefix, new);
	}

	fn apply_unannounce(&mut self, prefix: Path, last: RouteMeta, captures: Option<Vec<Pattern>>, gone: Gone) {
		let last = (last, captures);
		match self.pending.get_mut(&prefix).and_then(|pending| pending.update.take()) {
			// The pending announce was never delivered and neither was any earlier
			// one, so the pair cancels entirely, and nothing is owed for it.
			Some(PendingUpdate::Announce(_)) if !self.delivered.contains(&prefix) => {
				let pending = self.remove(&prefix).unwrap();
				self.owed -= usize::from(pending.owed);
			}
			// Either nothing is pending or the pending announce was a metadata
			// update on a delivered route; the consumer still owes a retraction.
			None | Some(PendingUpdate::Announce(_) | PendingUpdate::Restart(..) | PendingUpdate::Unannounce(..)) => {
				self.put(prefix, PendingUpdate::Unannounce(last, gone));
			}
			// The embedded announce cancels with this retraction; the consumer still
			// needs the leading one. Its reason is the later one only when both carry
			// one identity (captures and instance); otherwise the later reason is for a
			// route never delivered.
			Some(PendingUpdate::UnannounceAnnounce { old, gone: prior, new }) => {
				let gone = if old.1 == new.1 && old.0.3 == new.0.3 {
					gone
				} else {
					prior
				};
				self.put(prefix, PendingUpdate::Unannounce(old, gone));
			}
		}
	}

	/// Whether a poll at `now` has an entry to look at: one queued, changed, or come
	/// due since the last, or the end of a torn-down origin.
	fn due(&self, now: Instant) -> bool {
		!self.due.is_empty()
			|| self.parked.first_key_value().is_some_and(|((wake, _), _)| *wake <= now)
			|| (self.ended && self.pending.is_empty())
	}

	/// When the next held entry is due, if any is held.
	fn wake(&self) -> Option<Instant> {
		self.parked.first_key_value().map(|((wake, _), _)| *wake)
	}

	/// The first pending prefix ready to deliver at `now`, or when the next held one
	/// is. Each entry looked at is either ready or parked until it could be.
	///
	/// A metadata change to a route the consumer holds waits out the update hold
	/// (see [`Config::update_hold`]), which starts the first time a poll sees it. A
	/// restart onto another instance goes at once. A stale path to a retracted
	/// publisher waits out its quarantine in `retracted`, settled against `now`.
	fn scan(
		&mut self,
		retracted: &HashMap<Path, Quarantine>,
		now: Instant,
		hold: Duration,
	) -> Result<Path, Option<Instant>> {
		while let Some(entry) = self.parked.first_entry()
			&& entry.key().0 <= now
		{
			let ((_, seq), prefix) = entry.remove_entry();
			self.pending.get_mut(&prefix).unwrap().wake = None;
			self.due.insert(seq, prefix);
		}
		while let Some(entry) = self.due.first_entry() {
			#[cfg(test)]
			{
				self.steps += 1;
			}
			let prefix = entry.get();
			if hold.is_zero() || self.ended {
				return Ok(prefix.clone());
			}
			let pending = self.pending.get_mut(prefix).unwrap();
			let wake = match pending.update.as_ref().unwrap() {
				PendingUpdate::Announce(_) if self.delivered.contains(prefix) => {
					// The clock reads the driver's last advance, which may be stale: anchor
					// the hold on the first advance after the update arrived, by waking the
					// driver.
					match pending.held {
						None => {
							pending.held = Some(Held::Anchoring(now));
							Some(now.saturating_add(Duration::from_nanos(1)))
						}
						Some(Held::Anchoring(seen)) if now <= seen => {
							Some(seen.saturating_add(Duration::from_nanos(1)))
						}
						Some(Held::Anchoring(_)) => {
							pending.held = Some(Held::Until(now.saturating_add(hold)));
							Some(now.saturating_add(hold))
						}
						Some(Held::Until(until)) => Some(until).filter(|until| *until > now),
					}
				}
				PendingUpdate::Announce((meta, _)) => match retracted.get(prefix).filter(|q| q.holds(meta)) {
					Some(Quarantine {
						held: Held::Anchoring(seen),
						..
					}) => Some(seen.saturating_add(Duration::from_nanos(1))),
					Some(Quarantine {
						held: Held::Until(until),
						..
					}) => Some(*until).filter(|until| *until > now),
					None => None,
				},
				_ => None,
			};
			let Some(wake) = wake else {
				return Ok(prefix.clone());
			};
			pending.wake = Some(wake);
			let (seq, prefix) = entry.remove_entry();
			self.parked.insert((wake, seq), prefix);
		}
		Err(self.wake())
	}

	/// Take the pending update at `prefix` for delivery.
	fn take_prefix(&mut self, prefix: Path) -> RouteEvent {
		let (event, owed) = self.take_update(prefix);
		// A trailing announce left pending is still owed.
		if owed && !self.pending.contains_key(&event.announce.prefix) {
			self.owed -= 1;
		}
		event
	}

	fn take_update(&mut self, prefix: Path) -> (RouteEvent, bool) {
		let pending = self.remove(&prefix).unwrap();
		let owed = pending.owed;
		let ((meta, captures), kind) = match pending.update.unwrap() {
			PendingUpdate::Announce(meta) => {
				// The consumer has seen this prefix before, so it is a metadata update.
				let kind = match self.delivered.insert(prefix.clone()) {
					true => RouteKind::Start,
					false => RouteKind::Update,
				};
				(meta, kind)
			}
			PendingUpdate::Restart(meta) => {
				self.delivered.insert(prefix.clone());
				(meta, RouteKind::Restart)
			}
			PendingUpdate::Unannounce(meta, gone) => {
				self.delivered.remove(&prefix);
				(meta, gone.kind())
			}
			PendingUpdate::UnannounceAnnounce { old, gone, new } => {
				// Deliver the retraction now; leave the trailing announce pending so
				// the next take returns it for the same prefix.
				self.delivered.remove(&prefix);
				self.insert(prefix.clone(), PendingUpdate::Announce(new), owed);
				(old, gone.kind())
			}
		};
		let announce = Announce {
			prefix,
			captures,
			route: RouteInfo {
				hops: meta.0,
				cost: meta.1,
				via: Hop::UNKNOWN,
				source: meta.2,
				instance: Some(meta.3),
				epoch: meta.4,
				auth: meta.5,
				node: meta.6,
				draining: false,
				available: meta.7,
				rtt: meta.8,
			},
		};
		(RouteEvent { kind, announce }, owed)
	}
}

/// One announced route in the origin's table, absolute prefix.
struct RouteEntry {
	id: u64,
	/// The stream this route serves; see [`RouteInfo::instance`].
	instance: Instance,
	/// Which incarnation of the instance; see [`RouteInfo::epoch`].
	epoch: u64,
	prefix: Path,
	/// The absolute patterns the announcing producer may serve. The prefix is
	/// only the wire-visible covering claim; this scope remains authoritative.
	scope: Patterns,
	/// The absolute subtrees the announcing producer may not serve, even beneath
	/// a covering prefix.
	excluded: Arc<[Path]>,
	hops: Hops,
	cost: Cost,
	/// The auth the request advertising the route carries; see [`RouteInfo::auth`].
	auth: Option<crate::auth::Credential>,
	/// The rank of the link the entry arrived on; see [`Producer::with_preference`].
	preference: Preference,
	/// The announcing session's declared or assigned identity. Split-horizon
	/// matches this as well as [`Self::hops`], so an anonymous hop 0 still
	/// cannot echo back to the session it came from.
	via: Hop,
	/// Whether this is a broadcast published on this origin, which wins a cost tie and
	/// keeps its own cache.
	local: bool,
	/// The link the entry arrived on, from the handle that inserted it.
	link: Link,
	/// The origin node serving the route; see [`RouteInfo::node`].
	node: Option<u64>,
	/// Whether the route ranks last; see [`RouteInfo::draining`]. Local selection state,
	/// so never part of [`Self::meta`].
	draining: bool,
	/// Whether a usable path reaches the route's origin; see [`RouteInfo::available`].
	available: bool,
	/// The round trip to the route's origin node; see [`RouteInfo::rtt`].
	rtt: Option<Duration>,
	/// The queue requests under this route are served from, when the announcer
	/// serves content on demand (a [`Claim`]). `None` for an advertise-only
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
	/// Whether a peer the chain passes through has since withdrawn this prefix.
	/// The route was derived from that peer's advertisement, so it serves nobody
	/// until the peer announces again. See [`Claim::withdrawn`].
	stale: bool,
	/// [`prefix_claim`] of [`Self::prefix`], built once at announce time.
	///
	/// The announce sync evaluates a route's claim once per (cursor, route) pair,
	/// and building one allocates a segment vector and a canonical string. Holding
	/// it makes that visit a comparison.
	claim: Pattern,
	/// Each [`Self::scope`] member intersected with [`Self::claim`], built once at
	/// announce time for the same reason: the paths beneath the prefix the route
	/// may serve. A member whose intersection is too large to spell reaches nothing.
	reach: Vec<Pattern>,
	/// Whether the job routes answering this prefix route take its instance, which it
	/// minted rather than was given: their readers continue on the job's route. It moves
	/// to a fresh one as each job carrying it stops serving; see [`OriginState::advance`].
	lends: bool,
	/// The prefix route, by prefix and id, whose instance this job's route took.
	lent: Option<(Path, u64)>,
}

impl RouteEntry {
	/// Whether the announcing producer may serve the absolute `path`.
	fn authorizes(&self, path: &Path) -> bool {
		self.scope.matches(path.as_str()) && !self.excluded.iter().any(|excluded| path.has_prefix(excluded))
	}

	/// Whether cursors see the entry and requests resolve through it.
	fn live(&self) -> bool {
		self.advertised && !self.stale
	}

	fn is_anonymous(&self) -> bool {
		self.hops.iter().any(|hop| *hop == Hop::UNKNOWN)
	}

	/// Where the entry entered this origin.
	fn entered(&self) -> Source {
		match self.link {
			Link::Local => Source::Local,
			Link::Peer | Link::Upstream => Source::Peer(self.via.id()),
		}
	}

	/// The metadata an announce cursor delivers for this entry.
	fn meta(&self) -> RouteMeta {
		(
			self.hops.clone(),
			self.cost,
			self.entered(),
			self.instance.clone(),
			self.epoch,
			self.auth.clone(),
			self.node,
			self.available,
			self.rtt,
		)
	}

	/// Whether [`Self::meta`] would equal `meta`, without building it.
	fn delivers(&self, meta: &RouteMeta) -> bool {
		let (hops, cost, source, instance, epoch, auth, node, available, rtt) = meta;
		*hops == self.hops
			&& *cost == self.cost
			&& *source == self.entered()
			&& *instance == self.instance
			&& *epoch == self.epoch
			&& *auth == self.auth
			&& *node == self.node
			&& *available == self.available
			&& *rtt == self.rtt
	}

	/// Whether a request for `path` can be served through this entry. A served
	/// route covers everything beneath its prefix; a broadcast published here
	/// is only itself, so it serves its exact path and shadows what is beneath.
	fn serves(&self, path: &Path) -> bool {
		self.server.is_some() || (self.source.is_some() && self.prefix == *path)
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
		self.reach.iter().any(|reach| allowed.overlaps(reach))
	}
}

// Published names are literal coordinates. Navigation belongs to Relative,
// resolved before publication, so a name remains stable when carried by a URL.
fn publish_path(path: &Path) -> Result<(), Error> {
	if path.parts().any(|segment| matches!(segment, "." | "..")) {
		return Err(Error::NavigationSegment);
	}
	Ok(())
}

/// What `scope` lets a route at `claim` serve: see [`RouteEntry::reach`].
fn reach(scope: &Patterns, claim: &Pattern) -> Vec<Pattern> {
	scope
		.iter()
		.filter_map(|scope| scope.intersect(claim).ok())
		.flatten()
		.collect()
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
/// [`Claim`] draining it, so both sides work under one lock.
#[derive(Default)]
pub(super) struct ServeState {
	// Result channels for pending requests, keyed by absolute path so concurrent
	// `request` calls for the same path coalesce onto one channel.
	requests: RequestQueue<Path, kio::Producer<PendingBroadcast>>,

	// Broadcasts the handler has already served, kept weakly so a repeat request for the
	// same path resolves to a shared clone instead of re-invoking the handler (which would
	// open a duplicate upstream subscription). Weak so a served broadcast still closes once
	// its real consumers drop. The cache reclaims closed entries incrementally on insert, so a
	// long-lived origin serving many distinct one-shot paths stays bounded by the live count.
	served: WeakCache<Path, broadcast::WeakConsumer>,

	// How many times the route changed instance: a front compares it to the count when
	// it asked to tell a released request from an answered one without taking this lock.
	renewals: Arc<AtomicU64>,

	// Set when the announcement is retracted or the origin tears down: new requests
	// fail immediately and the handler observes the end instead of parking forever.
	closed: bool,
}

/// Key of a remotely-served front: the absolute path and the requester's
/// [`Horizon::effective`] horizon. Requesters excluding a peer some covering
/// route passes through get a front of their own, so its failover never adopts
/// a route flowing back through one of its readers, nor a local view a peer's
/// route. Every other requester shares the plain front, so fronts scale with
/// the peers in the path's route chains rather than with viewer sessions. A
/// request naming an instance other than the path's winner, such as a relay
/// forwarding toward the pool member its own route names, follows that
/// instance's routes on a front of its own.
type FrontKey = (Path, Horizon, Option<Instance>);

/// Which routes a reader sees: the split-horizon exclusion and the local-only view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Horizon {
	/// Routes whose hop chain or announcing session (`via`) is this peer are
	/// hidden. `Some(UNKNOWN)` marks an anonymous peer: no hop is excluded, but
	/// local-only broadcasts are not advertised. `None` is a local reader.
	exclude: Option<Hop>,
	/// Hide the routes that entered from a cluster peer ([`Consumer::local`]).
	local: bool,
	/// Hide the routes learned on an upstream link, because this reader is one
	/// ([`Link::Upstream`]): a relay never transits between two upstreams.
	upstream: bool,
	/// Hide the routes with no room for our hop under this cap, because this reader
	/// forwards them ([`Consumer::forwardable`]). Selection then falls back to a
	/// shorter route instead of withdrawing a prefix an in-cap path still reaches.
	max_hops: Option<NonZeroUsize>,
	/// Which origin nodes' routes this reader sees ([`Consumer::with_node`]).
	node: NodeFilter,
}

/// Which origin nodes' routes a reader sees: a request naming a node resolves only
/// through that node's routes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum NodeFilter {
	/// Every route.
	#[default]
	Any,
	/// Only the routes this node serves itself.
	Own,
	/// Only the routes the route layer selected toward this origin node.
	#[cfg_attr(
		not(feature = "async"),
		allow(dead_code, reason = "only the async session names a node")
	)]
	Is(u64),
}

impl NodeFilter {
	fn admits(self, node: Option<u64>) -> bool {
		match self {
			Self::Any => true,
			Self::Own => node.is_none(),
			Self::Is(id) => node == Some(id),
		}
	}
}

impl Horizon {
	/// Whether `entry` may be observed or served through this horizon.
	fn admits(&self, entry: &RouteEntry) -> bool {
		self.admits_link(entry.link)
			&& entry.visible_to(self.exclude)
			&& self.max_hops.is_none_or(|max| entry.hops.len() < max.get())
			&& self.node.admits(entry.node)
	}

	/// Whether routes arriving on `link` may be observed through this horizon.
	fn admits_link(&self, link: Link) -> bool {
		!(self.local && link != Link::Local) && !(self.upstream && link == Link::Upstream)
	}

	/// This horizon as it applies to the routes covering `path`: an excluded peer
	/// that none of them passes through hides nothing, so it excludes nothing. Every
	/// session carries a hop of its own, and a viewer's never shows up in a chain, so
	/// this is what lets viewers share a front.
	///
	/// Decided per request: a peer whose hop joins a chain later gets the filtered
	/// front on its next request, while what it already reads stays on the plain
	/// front. That cannot loop, since the peer serves our requests through its own
	/// split horizon, which never routes back through us.
	///
	/// The hop cap bounds what is advertised, not what serves a request, so it never
	/// applies here.
	fn effective(self, routes: &RouteTable, path: &Path) -> Self {
		let horizon = Self { max_hops: None, ..self };
		match horizon.exclude {
			Some(peer) if routes.covering(path).any(|entry| !entry.visible_to(Some(peer))) => horizon,
			_ => Self {
				exclude: None,
				..horizon
			},
		}
	}
}

/// The link a handle's routes arrive on; see [`Producer::with_link`].
///
/// The hop chain cannot say it, since a client and a peer relay each append one hop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Link {
	/// Here: an in-process producer or a client session.
	#[default]
	Local,
	/// A cluster peer: every route it announces reports [`Source::Peer`], and
	/// [`Consumer::local`] hides it.
	Peer,
	/// An upstream peer: a [`Self::Peer`] whose routes are never offered to another
	/// upstream, so this origin never carries traffic between two upstreams.
	Upstream,
}

/// One remotely-served front in [`OriginState::fronts`]: the shared broadcast at a
/// path plus the channel requesters resolve through.
#[derive(Clone)]
struct RemoteFront {
	/// Resolves requesters with the front's verdict. The producer lives here so the
	/// teardown can reject requesters still parked on a front whose watcher was
	/// cancelled.
	request: kio::Producer<PendingFront>,
	/// The front's broadcast, weak: dead once the front ends, so a
	/// later request re-creates the front instead of joining a corpse.
	broadcast: broadcast::WeakConsumer,
}

/// The last route a cursor observed at a presented prefix.
struct CursorRoute {
	/// The stream it serves.
	instance: Instance,
	/// What the cursor delivered with it.
	meta: RouteMeta,
	/// Whether it serves requests: part of the dedupe key, but never delivered.
	served: bool,
	/// The absolute prefix it was announced at, which decides `captures`.
	prefix: Path,
	/// What the cursor's scope captured from `prefix`.
	captures: Option<Vec<Pattern>>,
}

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
	root: Path,
	/// The absolute patterns this cursor is scoped to (its token / scope).
	allowed: Patterns,
	/// Where the cursor hangs in the [`RouteTable`]: the literal heads of
	/// `allowed`. A route the cursor can see sits at or under one of them, or on
	/// the walk down to one.
	heads: Vec<Path>,
	/// The routes this cursor may see (control-plane split horizon).
	horizon: Horizon,
	/// Which routes beneath a hidden segment are reported.
	hidden: Hidden,
	/// Whether a route whose subtree only overlaps the scope is reported too, not just
	/// one whose literal name matches it.
	covering: bool,
	/// The delivery buffer, drained by the cursor's `poll_next`. A mounted
	/// consumer's cursors share one.
	state: kio::Producer<OriginConsumerState>,
	/// Where this cursor's presented prefixes land in the delivery buffer: empty,
	/// or the mount the cursor reads for, relative to the consumer's root.
	under: Path,
	/// The absolute prefixes a mount shadows: routes at or beneath them are not
	/// this cursor's to present.
	holes: Vec<Path>,
	/// For a cursor reading through a mount: the mount and the handle's own
	/// patterns, so a wildcard captures what the handle names rather than the target.
	named: Option<(Mount, Patterns)>,
	/// Whether the cursor counts an [`Interest`] at each of its heads.
	interested: bool,
	/// The last delivered best route per presented (relative) prefix, for change
	/// detection. Servability is part of the dedupe key but never leaves the model.
	current: HashMap<Path, CursorRoute>,
}

impl TableCursor {
	/// Where `prefix` presents on this cursor, named relative to the cursor root.
	/// The prefix stays a prefix; the pattern scope only decides visibility.
	/// `claim` is the prefix's [`prefix_claim`], which the caller already holds:
	/// building one allocates, and the sweeps below ask this per route per
	/// cursor.
	fn presented(&self, prefix: &Path, claim: &Pattern) -> Option<Path> {
		if !self.allowed.overlaps(claim) {
			return None;
		}

		if let Some(relative) = prefix.strip_prefix(&self.root) {
			return Some(relative);
		}
		self.root.has_prefix(prefix).then(Path::default)
	}

	/// What the cursor's most specific matching scope member captures from an
	/// exact announced prefix. An overlap-only route does not pin every wildcard.
	fn captures(&self, prefix: &Path) -> Option<Vec<Pattern>> {
		let (literal, allowed) = match &self.named {
			Some((mount, allowed)) => (Pattern::literal(mount.name(prefix)?.as_str()).ok()?, allowed),
			None => (Pattern::literal(prefix.as_str()).ok()?, &self.allowed),
		};
		allowed
			.iter()
			.filter_map(|allowed| {
				allowed
					.captures(&literal)
					.map(|captures| (allowed.specificity(), captures))
			})
			.max_by_key(|(specificity, _)| *specificity)
			.map(|(_, captures)| captures)
	}

	/// Whether this cursor waits on `snapshot`: what it delivers could present here,
	/// as [`Self::visible`] judges a route at or beneath its prefix.
	fn awaits(&self, snapshot: &SnapshotEntry) -> bool {
		self.horizon.admits_link(snapshot.link)
			&& self.allowed.overlaps(&snapshot.claim)
			&& self.hidden.reaches(&self.heads, &snapshot.prefix)
			&& !self.holes.iter().any(|hole| snapshot.prefix.has_prefix(hole))
			// Too deep for the mount to name means everything beneath is too.
			&& self.named.as_ref().is_none_or(|(mount, _)| mount.names(&snapshot.prefix))
	}

	/// Whether this cursor may observe `entry` at all: advertised, not behind
	/// the excluded peer (split horizon), within the cursor's patterns, and
	/// nameable through its mount.
	fn visible(&self, entry: &RouteEntry) -> bool {
		entry.live()
			&& self.horizon.admits(entry)
			&& entry.overlaps(&self.allowed)
			&& (self.covering
				|| match &self.named {
					Some((mount, allowed)) => mount
						.name(&entry.prefix)
						.is_some_and(|name| allowed.matches(name.as_str())),
					None => self.allowed.matches(entry.prefix.as_str()),
				}) && self.discovers(&entry.prefix)
			&& !self.holes.iter().any(|hole| entry.prefix.has_prefix(hole))
			&& self.named.as_ref().is_none_or(|(mount, _)| mount.names(&entry.prefix))
	}

	/// Deliver a changed winner at this presented prefix, reporting a route left with
	/// none as `gone`.
	fn update(&mut self, presented: &Path, best: Option<&RouteEntry>, gone: Gone) {
		let Some(entry) = best else {
			if let Some(last) = self.current.remove(presented)
				&& let Ok(mut state) = self.state.write()
			{
				state.apply_unannounce(self.under.rejoin(presented), last.meta, last.captures, gone);
			}
			return;
		};
		let served = entry.server.is_some();
		let previous = self.current.get(presented);
		// Compared in place first: most visits find the winner unchanged, and building
		// what it delivers allocates. Captures follow from the prefix.
		if previous.is_some_and(|previous| {
			previous.instance == entry.instance
				&& previous.prefix == entry.prefix
				&& previous.served == served
				&& entry.delivers(&previous.meta)
		}) {
			return;
		}

		let meta = entry.meta();
		let captures = self.captures(&entry.prefix);
		let previous = self.current.insert(
			presented.clone(),
			CursorRoute {
				instance: entry.instance.clone(),
				meta: meta.clone(),
				served,
				prefix: entry.prefix.clone(),
				captures: captures.clone(),
			},
		);
		match previous {
			// Captures are consumer identity, not route metadata. Replace the old
			// identity explicitly so consumers keyed on it remove it.
			Some(previous) if previous.captures != captures => {
				if let Ok(mut state) = self.state.write() {
					state.apply_unannounce(self.under.rejoin(presented), previous.meta, previous.captures, gone);
					state.apply_announce(self.under.rejoin(presented), meta, captures);
				}
			}
			// Another instance: whatever was resolved under the prefix is another stream.
			Some(previous) if previous.instance != entry.instance => {
				if let Ok(mut state) = self.state.write() {
					state.apply_restart(self.under.rejoin(presented), meta, captures);
				}
			}
			// Unchanged metadata and servability: nothing the consumer could
			// act on, even if the winning entry itself changed (a failover
			// between routes of one instance and epoch is invisible, which is the point).
			// A servability flip is delivered: a request that failed Unroutable
			// under an advertise-only route retries on the update, and hiding
			// it would park that waiter forever.
			Some(previous) if previous.meta == meta && previous.served == served => {}
			_ => {
				if let Ok(mut state) = self.state.write() {
					state.apply_announce(self.under.rejoin(presented), meta, captures);
				}
			}
		}
	}

	/// The visible entry among `candidates` that presents here: the longest prefix,
	/// then the first eligible one in [`route_order`].
	fn best<'a>(&self, candidates: impl Iterator<Item = &'a RouteEntry> + Clone) -> Option<&'a RouteEntry> {
		let visible = candidates.filter(|entry| self.visible(entry));
		let epochs = Epochs::of(visible.clone());
		visible
			.clone()
			.filter(|entry| !epochs.superseded(entry))
			.min_by_key(|entry| (Reverse(entry.prefix.len()), route_order(&entry.prefix, entry)))
	}

	/// Whether the hidden rule lets this cursor discover a route at `prefix`.
	fn discovers(&self, prefix: &Path) -> bool {
		self.hidden.reaches(&self.heads, prefix)
	}
}

/// A handle's view of an origin: the absolute patterns it may reach, and the
/// subtrees it reads from elsewhere on the origin.
#[derive(Clone)]
struct OriginScope {
	// The paths this handle may reach, absolute, named as the handle sees them:
	// a path under a mount is authorized here, before it is resolved.
	allowed: Patterns,
	// The subtrees that resolve elsewhere; see [`Producer::mount`]. Disjoint.
	mounts: Arc<[Mount]>,
	// The subtrees this handle never reaches despite `allowed`, absolute and
	// named as the handle sees them; see [`Consumer::exclude`].
	excluded: Arc<[Path]>,
}

/// A subtree a handle reads from elsewhere on the origin: `at/rest` resolves at
/// `target/rest`. Both absolute.
#[derive(Clone, Debug)]
struct Mount {
	at: Path,
	target: Path,
}

/// A mount as a reader of a prefix sees it through a handle: where the origin holds
/// what it presents, and how the handle names that.
#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) struct Mounted {
	mount: Mount,
	/// The absolute, origin-side prefix to read: the target, or beneath it for a
	/// reader's prefix beneath the mount point.
	pub interest: Path,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl Mounted {
	/// How the handle names a claim at the absolute, origin-side `path`, which lies at
	/// or beneath the interest: at or beneath the mount point.
	pub fn name(&self, path: &Path) -> Option<Path> {
		self.mount.name(path)
	}

	/// Whether the absolute, handle-side `path` lies at or beneath the mount point.
	pub fn holds(&self, path: &Path) -> bool {
		path.has_prefix(&self.mount.at)
	}
}

impl Mount {
	/// Where the handle-side absolute `path` resolves, when it is under this mount
	/// and the result fits the wire limits.
	fn resolve(&self, path: &Path) -> Option<Path> {
		let resolved = self.target.join(&path.strip_prefix(&self.at)?).ok()?;
		(resolved.parts().count() <= Path::MAX_PARTS).then_some(resolved)
	}

	/// Where the origin-side absolute `path` shows on the handle, when it is at or
	/// beneath the target. A route covering the target has no handle-side name.
	fn name(&self, path: &Path) -> Option<Path> {
		self.at.join(&path.strip_prefix(&self.target)?).ok()
	}

	/// Whether the origin-side absolute `path` has a handle-side name within the
	/// wire limits, so a reader could ask for it. A route covering the target always does.
	fn names(&self, path: &Path) -> bool {
		path.strip_prefix(&self.target).is_none_or(|rest| {
			self.at.parts().count() + rest.parts().count() <= Path::MAX_PARTS
				&& self.at.len() + usize::from(!self.at.is_empty() && !rest.is_empty()) + rest.len() <= Path::MAX_BYTES
		})
	}

	/// The handle-side absolute `patterns` beneath this mount, as origin-side
	/// patterns beneath its target.
	///
	/// A member too deep to root matches no valid path and drops, except that a
	/// `**` one segment past the limit can only match nothing and is dropped instead,
	/// so a deep target keeps its exact path.
	fn translate(&self, patterns: &Patterns) -> Patterns {
		let target = self.target.as_str();
		patterns
			.rebase(self.at.as_str())
			.iter()
			.filter_map(|member| {
				member
					.rooted(target)
					.or_else(|_| {
						let segments = member
							.segments()
							.iter()
							.filter(|segment| **segment != Segment::Globstar);
						Pattern::new(segments.cloned())?.rooted(target)
					})
					.ok()
			})
			.collect()
	}

	/// A handle-side interest head as an origin-side one: a head beneath the mount
	/// moves under the target, and one above it hangs at the target itself.
	fn translate_head(&self, head: &Path) -> Option<Path> {
		match head.strip_prefix(&self.at) {
			Some(rest) => self.target.join(&rest).ok(),
			None => self.at.has_prefix(head).then(|| self.target.clone()),
		}
	}
}

impl OriginScope {
	/// A view that reaches nothing.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn empty() -> Self {
		Self {
			allowed: Patterns::new(),
			mounts: Arc::from([]),
			excluded: Arc::from([]),
		}
	}

	/// This view narrowed to the absolute `patterns`: the paths in both.
	fn narrow(&self, patterns: &Patterns) -> Option<Self> {
		let allowed = self.allowed.intersect(patterns).ok()?;
		if allowed.is_empty() {
			None
		} else {
			Some(Self {
				allowed,
				..self.clone()
			})
		}
	}

	/// The mount the absolute `path` is under, if any.
	fn mount(&self, path: &Path) -> Option<&Mount> {
		self.mounts.iter().find(|mount| path.has_prefix(&mount.at))
	}

	/// Where the absolute `path` resolves on the origin: itself, or its mount
	/// target. `None` when the mount would resolve it past the wire limits.
	fn resolve(&self, path: &Path) -> Option<Path> {
		match self.mount(path) {
			Some(mount) => mount.resolve(path),
			None => Some(path.clone()),
		}
	}

	/// Whether this view may publish at the absolute `prefix`: a mount is read-only,
	/// so nothing is published at or beneath one, nor in an excluded subtree.
	fn publishes(&self, prefix: &Path) -> bool {
		self.mount(prefix).is_none() && !self.excluded.iter().any(|excluded| prefix.has_prefix(excluded))
	}

	/// Whether this view reaches the absolute `path`.
	fn permits(&self, path: &Path) -> bool {
		self.allowed.matches(path.as_str()) && !self.excluded.iter().any(|excluded| path.has_prefix(excluded))
	}

	/// The excluded subtrees a cursor must not present, in the space it reads: the
	/// origin's own, or `mount`'s target. `None` when an exclusion covers the whole mount.
	fn holes(&self, mount: Option<&Mount>) -> Option<Vec<Path>> {
		let Some(mount) = mount else {
			return Some(self.excluded.to_vec());
		};
		let mut holes = Vec::new();
		for excluded in self.excluded.iter() {
			if mount.at.has_prefix(excluded) {
				return None;
			}
			if excluded.has_prefix(&mount.at) {
				// Too deep for the mount to resolve, nothing beneath is nameable anyway.
				holes.extend(mount.resolve(excluded));
			}
		}
		Some(holes)
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
			mounts: Arc::from([]),
			excluded: Arc::from([]),
		}
	}
}

/// The announce-interest prefixes that cover a pattern scope on a prefix-only
/// wire: each member's literal head, minus heads another already covers.
pub(crate) fn interest_prefixes(allowed: &Patterns) -> Vec<Path> {
	let mut heads: Vec<Path> = allowed.iter().filter_map(|pattern| covering(pattern.head())).collect();
	heads.sort();
	heads.dedup();
	let covered = heads.clone();
	heads.retain(|head| !covered.iter().any(|other| other != head && head.has_prefix(other)));
	heads
}

/// The nearest path at or above a pattern head.
///
/// A head cut at an escaped `/`, such as `a\` from `a\/*`, ends in half an escape, so it
/// falls back to its parent. A head too long for a path covers none, so it asks for nothing.
fn covering(mut head: &str) -> Option<Path> {
	loop {
		match Path::new(head) {
			Ok(path) => return Some(path),
			Err(crate::path::Error::InvalidEscape) => head = head.rsplit_once('/').map_or("", |(parent, _)| parent),
			Err(_) => return None,
		}
	}
}

/// Which routes beneath a hidden segment an announce cursor reports.
///
/// A route is hidden when a segment below the cursor's requested prefix (its
/// interest head) starts with `.`; a prefix that names the dot segment itself
/// lists what is under it. Only discovery is affected: a request by exact path
/// resolves either way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Hidden {
	/// Report hidden routes too.
	include: bool,
	/// Measure visibility from the request prefix instead of authorization heads.
	from: Option<Path>,
}

impl Hidden {
	/// Whether a cursor hanging at `heads` reports a route at or beneath `prefix`. A hidden
	/// segment hides everything beneath it.
	fn reaches(&self, heads: &[Path], prefix: &Path) -> bool {
		self.include || !hides(self.from.as_ref().map(std::slice::from_ref).unwrap_or(heads), prefix)
	}

	/// This rule for a cursor reading through `mount`, whose heads sit on the target.
	fn translate(&self, mount: &Mount) -> Self {
		Self {
			include: self.include,
			from: self.from.as_ref().and_then(|head| mount.translate_head(head)),
		}
	}
}

/// Whether a segment of `prefix` below the head it sits under starts with `.`.
/// A route at or above a head has nothing below it, so it never hides.
fn hides(heads: &[Path], prefix: &Path) -> bool {
	heads
		.iter()
		.any(|head| prefix.strip_prefix(head).is_some_and(|below| below.is_hidden()))
}

/// How many sessions withhold an origin's hidden routes from their peer, such as a
/// moq-lite client before 07, whose peer can't ask for them.
#[derive(Clone, Debug, Default)]
pub(crate) struct HiddenWithheld(kio::Shared<usize>);

impl HiddenWithheld {
	/// Count a session that withholds them, until the returned guard drops.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn hold(&self) -> HiddenWithholding {
		*self.0.lock() += 1;
		HiddenWithholding(self.clone())
	}

	/// Ready while a session withholds them.
	pub fn poll(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.0
			.poll(waiter, |count| match **count {
				0 => Poll::Pending,
				_ => Poll::Ready(()),
			})
			.map(drop)
	}
}

/// A session counted by [`HiddenWithheld::hold`], until dropped.
#[derive(Debug)]
pub(crate) struct HiddenWithholding(HiddenWithheld);

impl Drop for HiddenWithholding {
	fn drop(&mut self) {
		*self.0.0.lock() -= 1;
	}
}

/// What a [`Routes`] yields: how the route covering a prefix changed, and that route.
#[derive(Clone, Debug)]
pub struct RouteEvent {
	/// How the route changed.
	pub kind: RouteKind,
	/// The route; for [`RouteKind::End`], its last advertised route.
	pub announce: Announce,
}

/// How the route covering a prefix changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
	/// A route now covers the prefix; the cursor had none there.
	Start,
	/// The route covering the prefix changed hops, cost, epoch or
	/// [availability](RouteInfo::available), with the same instance; it is delivered in
	/// place.
	Update,
	/// The origin withdrew the route, as far as this origin knows, and no other covers
	/// the prefix.
	End,
	/// This origin let go of the route, and no other covers the prefix: the session it
	/// arrived on closed, or the route layer lost every path to it. Its origin may still
	/// serve it; only an [`End`](Self::End) says it stopped.
	Offline,
	/// Another [`Instance`] now serves the prefix: another stream, such as a publisher
	/// that restarted, a redundant ingest switching, or a pool failing over.
	///
	/// Whatever was resolved under the prefix before is the old stream: drop it and
	/// request afresh to follow the new one. Subscriptions already open stay on the old
	/// one until dropped or its route goes.
	Restart,
}

/// A route over a prefix, as an announcement listing or a [`Routes`] stream reports it.
///
/// An announcement is always a prefix, never a broadcast: it advertises that
/// [`prefix`](Self::prefix) and every path beneath it are servable. A broadcast
/// announces its own path, so the prefix usually names one, but resolve it with
/// [`Consumer::request`]; the application decides which paths name
/// broadcasts, and filters with a [`Pattern`] locally when it wants a subset.
#[derive(Clone, Debug)]
pub struct Announce {
	/// The prefix the route covers, relative to the consuming cursor's root.
	pub prefix: Path,
	/// What the scope's wildcards stood for when the announced prefix pins all of
	/// them. `None` for an overlap-only route or a scope without a complete match.
	pub captures: Option<Vec<Pattern>>,
	/// The route serving the prefix. On a retraction this carries its last
	/// advertised metadata.
	pub route: RouteInfo,
}

/// Publishes broadcasts and announces routes into an origin.
#[derive(Clone)]
pub struct Producer {
	// Identity for this origin. Appended to route hops when re-announcing so
	// downstream relays can detect loops and prefer the shortest path.
	hop: NonZeroU64,
	max_hops: NonZeroUsize,

	// The absolute patterns this handle may publish under.
	scope: OriginScope,

	// The prefix that is automatically stripped from all paths.
	root: Path,

	// The origin's shared state: the route table, announce cursors, and the
	// remotely-served fronts. Shared with every derived consumer.
	shared: SendCell<kio::Shared<OriginState>>,

	// The cache pool inherited by broadcasts created under this origin (sessions
	// mint their remote broadcasts with it). Unbounded by default.
	pool: cache::Pool,

	// Max age ceiling inherited by broadcasts created under this origin (see
	// [`Config::cache_duration`]). No ceiling by default.
	cache_duration: Option<Duration>,

	// Ingress stats context. Broadcasts created through this producer are attributed
	// to it (writes counted on the subscriber/ingress side). Empty (no-op) unless a
	// session tagged this handle via [`Self::with_stats`].
	stats: stats::Session,

	// The link routes announced through this handle arrive on (see
	// [`Self::with_link`]).
	link: Link,

	// The rank of that link; see [`Self::with_preference`].
	preference: Preference,

	// Whether the consumers taken from this handle proxy; see [`Self::with_proxy`].
	proxy: bool,

	// Submission handle to the origin's [`Driver`]: source watchers, fronts, and
	// serve tasks queued here run when the driver is polled. Closed once the
	// driver drops, which is what makes later mutations fail with `Closed`.
	tasks: Tasks,

	// The clock advanced by the origin driver.
	timers: Deadlines,

	// What the frames written through this handle may allocate up front, shared by every
	// broadcast it creates.
	frames: super::frame::Budget,
}

impl Producer {
	/// A handle that queues work on this origin's [`Driver`] without keeping the origin
	/// alive: work queued once the driver is gone is dropped.
	pub(super) fn tasks(&self) -> crate::util::TasksWeak {
		self.tasks.downgrade()
	}

	/// Build a producer from a [`Config`] (identity + cache pool) with no scoped
	/// prefix and no pre-existing broadcasts, paired with the [`Driver`] that runs
	/// the origin's lifecycle work.
	///
	/// Poll the driver with caller-supplied time for the origin to make progress.
	/// `moq_tokio::origin::spawn` wraps this for tokio callers.
	pub fn new(config: Config) -> (Self, Driver) {
		let (tasks, set) = TaskSet::new();
		let scope = OriginScope::default();
		let timers = Deadlines::new(Instant::ZERO);
		let shared = SendCell::new(kio::Shared::new(OriginState::new(config.update_hold, config.route_key)));
		let pool = config.pool.clone();
		let producer = Self {
			hop: config.hop,
			max_hops: config.max_hops.min(MAX_HOP_LIMIT),
			scope: scope.clone(),
			root: Path::default(),
			shared: shared.clone(),
			pool: config.pool,
			cache_duration: config.cache_duration,
			stats: stats::Session::default(),
			link: Link::Local,
			preference: Preference::default(),
			proxy: false,
			tasks,
			timers: timers.clone(),
			frames: Default::default(),
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

	/// Give this handle a frame budget of its own, so what one session allocates up front
	/// never crowds out another's.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_own_frames(mut self) -> Self {
		self.frames = Default::default();
		self
	}

	/// The ingress stats context this handle was tagged with.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn stats(&self) -> &stats::Session {
		&self.stats
	}

	/// Attach an ingress stats context: broadcasts created through this handle (and
	/// any handle derived from it) are attributed to `session` on the subscriber
	/// (ingress) side. Pass [`stats::Session::default`] to opt out.
	pub fn with_stats(mut self, session: stats::Session) -> Self {
		self.stats = session;
		self
	}

	/// Set the link this handle (and any handle derived from it) is given to.
	///
	/// Hand a session with another relay a [`Link::Peer`] handle, so this origin can
	/// tell what entered here from what a peer forwarded. Hand a [`Link::Upstream`]
	/// handle to a session with a peer that should reach everything here without this
	/// relay carrying its traffic to other upstreams, such as an edge's link to a core
	/// or a drone's link to its CDN: a [`Consumer`] taken from it hides every route an
	/// upstream handle announced.
	pub fn with_link(mut self, link: Link) -> Self {
		self.link = link;
		self
	}

	/// Rank the link this handle (and any handle derived from it) is given to:
	/// higher wins, and every handle starts at the neutral [`Preference::default`].
	///
	/// Applies to the routes a session lands from its peer through this handle, not to
	/// a route created here. Only an instance's newest epoch is
	/// eligible, but among eligible routes preference decides before cost, so links to different
	/// operators rank by this instead of comparing their metrics. Hand each CDN's
	/// session its own rank to use the primary while it has a route and fail over to
	/// the next.
	pub fn with_preference(mut self, preference: Preference) -> Self {
		self.preference = preference;
		self
	}

	/// Whether a moq-lite-07 session publishing through a consumer of this handle (and
	/// of any handle derived from it) carries its peer's traffic to other origins: off,
	/// the peer is sent only the routes to this node's own claims; on, the routes it
	/// learned from other peers too.
	///
	/// A relay turns it on for every session. An application publishing its own
	/// broadcasts leaves it off, so a peer never routes through it to reach anyone
	/// else. Older versions advertise everything either way, since their hop chains
	/// cannot tell this node's claims from what it forwards.
	pub fn with_proxy(mut self, proxy: bool) -> Self {
		self.proxy = proxy;
		self
	}

	/// The rank of the link this handle is given to; see [`Self::with_preference`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn preference(&self) -> Preference {
		self.preference
	}

	/// The link this handle's routes arrive on; see [`Self::with_link`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn link(&self) -> Link {
		self.link
	}

	/// The route layer node every lite-07 session on this origin shares; see
	/// [`Consumer::route_node`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn route_node(&self) -> crate::route::Node {
		self.consume().route_node()
	}

	/// See [`Consumer::route_closing`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn route_closing(&self) -> Option<crate::route::node::Closing> {
		let node = self.shared.lock().route_node.clone();
		node.map(|(node, _)| node.closing())
	}

	/// The lite-06 hop this node's route identity travels as, once a moq-lite-07
	/// session created the route node: a relay translating this node's claims for a
	/// lite-06 peer starts their chain with it. It is this origin's own hop until the
	/// identity rotates. `None` while no session has, which leaves the node uncreated.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn route_hop(&self) -> Option<Hop> {
		let (node, _) = self.shared.lock().route_node.clone()?;
		Some(crate::route::node::hop(node.declared()))
	}

	/// This origin's construction config.
	pub fn config(&self) -> Config {
		let shared = self.shared.lock();
		Config {
			hop: self.hop,
			max_hops: self.max_hops,
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			update_hold: shared.update_hold,
			route_key: shared.route_key.clone(),
		}
	}

	/// This origin's hop id; see [`Config::hop`].
	pub fn hop(&self) -> NonZeroU64 {
		self.hop
	}

	/// Whether `id`, as a peer declared it, names this origin: its hop id, or the Node ID
	/// its moq-lite-07 sessions declare.
	pub fn is_own(&self, id: NonZeroU64) -> bool {
		id == self.hop || self.consume().node_id() == Some(id)
	}

	/// The key sessions prove for this origin.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn route_key(&self) -> Option<super::origin::RouteKey> {
		self.shared.lock().route_key.clone()
	}

	// Sessions enforce the policy configured once on the origin.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn max_hops(&self) -> NonZeroUsize {
		self.max_hops
	}

	/// A producer with *no* allowed prefixes: it can't publish anything and
	/// advertises no subscribe interest (its `allowed()` is empty, so the
	/// subscriber issues no ANNOUNCE_PLEASE). Used to fill an unset session half
	/// so both the publisher and subscriber loops still run.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn empty() -> Self {
		// No allowed prefixes means no broadcast is ever created, so nothing will
		// ever be queued on the detached submission handle.
		let (tasks, _) = TaskSet::new();
		Self {
			hop: random_id(),
			scope: OriginScope::empty(),
			max_hops: MAX_HOP_LIMIT,
			root: Path::default(),
			shared: SendCell::new(kio::Shared::new(OriginState::new(DEFAULT_UPDATE_HOLD, None))),
			pool: cache::Pool::default(),
			cache_duration: None,
			stats: stats::Session::default(),
			link: Link::Local,
			preference: Preference::default(),
			proxy: false,
			tasks,
			timers: Deadlines::new(Instant::ZERO),
			frames: Default::default(),
		}
	}

	/// Create a route at exactly `path`, owning a new broadcast to fill.
	///
	/// This is how local content enters an origin. Fill the broadcast through
	/// [`Route::broadcast`]. The route serves a fresh random instance, minted here and
	/// kept for its lifetime, across [`Route::withdraw`] and [`Route::announce`], since
	/// its keys continue: a new route is a new stream that never splices into an
	/// old one. A publisher that continues one key space across restarts may name
	/// a stable instance instead ([`RouteOptions::with_instance`]).
	///
	/// Announced at once unless created [`RouteOptions::hidden`]. A hidden route exists
	/// for nobody until [`Route::announce`]: until then no announce cursor lists it and
	/// a request for its path fails with [`Error::Unroutable`], for a consumer of this
	/// origin exactly as for a peer. Create it hidden when the tracks a subscriber needs
	/// first, such as a catalog, are not there yet. To serve paths on demand without
	/// publishing each one, use [`Self::claim`].
	///
	/// Announcing is visible to local consumers before it returns; only lifecycle work
	/// (track serving, teardown) waits for the [`Driver`] to be polled. Register a
	/// [`broadcast::Producer::requests`] handler before announcing, so the first
	/// consumer finds the tracks it serves.
	///
	/// Dropping the last clone of the route retracts it, as does closing its broadcast.
	///
	/// Fails with [`Error::Unauthorized`] if `path` is outside the prefixes this
	/// producer may publish under (after [`scope`](Self::scope)) or beneath a
	/// [`mount`](Self::mount),
	/// [`Error::BoundsExceeded`] if the full rooted path exceeds
	/// [`Path::MAX_PARTS`] or [`Path::MAX_BYTES`], [`Error::InvalidPath`] if it holds a segment no
	/// pattern can spell (`*` or `**`), [`Error::NavigationSegment`] for a literal `.` or `..`,
	/// or [`Error::Closed`] once the origin's
	/// [`Driver`] has been dropped.
	pub fn route(&self, path: impl TryInto<Path, Error: Into<Error>>, options: RouteOptions) -> Result<Route, Error> {
		let full = self.root.join(&path::convert(path)?)?;
		self.exact(full, options, None)
	}

	/// Whether this handle could publish a route at the absolute `full` prefix: its
	/// scope overlaps the subtree (as a forwarded claim is admitted, a covering prefix
	/// serving the permitted descendants), and no mount or excluded subtree covers it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn publishable(&self, full: &Path) -> bool {
		Pattern::subtree(full.as_str()).is_ok_and(|subtree| self.scope.allowed.overlaps(&subtree))
			&& self.scope.publishes(full)
	}

	/// An exact route at the absolute `full` path, answering `request` on its first
	/// announce when it is a job's.
	fn exact(&self, full: Path, options: RouteOptions, request: Option<Request>) -> Result<Route, Error> {
		publish_path(&full)?;
		if !self.scope.permits(&full) || !self.scope.publishes(&full) {
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
		// as long as the route, or until the broadcast closes.
		let announcing = Announcing {
			hop: Hop::from(self.hop),
			shared: self.shared.clone(),
			requested: full.clone(),
			prefixes: vec![(full.clone(), claim)],
			scope: self.scope.allowed.clone(),
			excluded: self.scope.excluded.clone(),
			local: true,
			link: self.link,
			preference: Preference::default(),
			stats: self.stats.clone(),
		};
		let info = broadcast::Info {
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			path: full,
			instance: None,
			frames: self.frames.clone(),
		};
		let source = info.produce().with_stats(ingress);
		// A job's route naming no instance takes its prefix route's, held in the table
		// until it first announces; see [`Request::route`].
		let lender = request
			.as_ref()
			.filter(|_| options.instance.is_none())
			.map(|request| request.home.clone());
		let instance = match (&options.instance, &lender) {
			(None, None) => Some((Instance::random(), 0)),
			(named, _) => named.clone(),
		};
		let entry = announcing.announce(
			RouteInfo {
				instance: instance.as_ref().map(|(instance, _)| instance.clone()),
				epoch: instance.as_ref().map_or(0, |(_, epoch)| *epoch),
				..options.info()
			},
			Serving {
				server: None,
				source: Some(source.consume()),
				advertised: false,
				lends: false,
			},
			lender.as_ref(),
		)?;
		let hidden = options.hidden;
		let control = RouteControl::new(options, &entry, request);
		let route = Route {
			state: Arc::new(RouteState {
				announcement: entry,
				control: kio::Lock::new(control),
				forwarded: false,
				_keepalive: self.tasks.keepalive(),
			}),
			broadcast: source.clone(),
		};
		// Closing the broadcast retracts the route, and refuses the request a job's route
		// has not answered yet. The broadcast holds only a weak reference, so it never
		// keeps the route alive.
		let weak = Arc::downgrade(&route.state);
		source.on_close(Box::new(move || {
			if let Some(state) = weak.upgrade() {
				state.ended();
			}
		}));
		if !hidden {
			route.announce()?;
		}
		Ok(route)
	}

	/// Mint a standalone source broadcast for a served-route request: it carries
	/// this origin's cache policy and ingress attribution, but
	/// is *not* entered into the route table. Sessions answer
	/// a prefix route's requests with one of these; the requester already holds
	/// the request's result channel, so the table never needs to resolve it.
	/// The source takes new tracks only while `validity` holds.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn create_source(
		&self,
		path: Path,
		validity: crate::model::broadcast::RequestValidity,
	) -> broadcast::Producer {
		// The path came off a served request, whose root join already fit.
		let full = self.root.join(&path).expect("request path fits");
		let ingress = self.stats.ingress(&full);
		broadcast::Info {
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			path: full,
			instance: None,
			frames: self.frames.clone(),
		}
		.produce()
		.with_stats(ingress)
		.with_validity(validity)
	}

	/// Advertise a route without serving it: a claim that paths under `prefix`
	/// can be served, answered by nothing.
	///
	/// A request under an advertise-only route resolves [`Error::Unroutable`]
	/// unless an announced broadcast or a prefix route ([`Self::claim`]) covers
	/// the path too. Tests use it to shape the route table; everything else
	/// advertises through a [`Route`] or [`Claim`], which serves what it claims.
	#[cfg(test)]
	pub(crate) fn announce(
		&self,
		prefix: impl TryInto<Path, Error: Into<Error>>,
		route: RouteInfo,
	) -> Result<AnnounceProducer, Error> {
		Announcing::new(self, prefix)?.announce(
			route,
			Serving {
				server: None,
				source: None,
				advertised: true,
				lends: false,
			},
			None,
		)
	}

	/// Start a [`Snapshot`] under `prefix`: the announce consumers it overlaps are not
	/// synced until the returned guard drops. Taken before the request goes out, so
	/// nothing reads as synced in between.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn snapshot(&self, prefix: impl TryInto<Path, Error: std::fmt::Debug>) -> Snapshot {
		let prefix = self
			.root
			.join(&prefix.try_into().expect("snapshot prefix"))
			.expect("snapshot prefix fits");
		self.shared.lock().snapshot(&self.shared, prefix, self.link)
	}

	/// Hear what this handle's session should ask its peer about: the broadest
	/// prefixes readers of the origin want discovered, mapped onto this handle's
	/// scope, starting with the current ones.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn interests(&self) -> Interests {
		let queue = kio::Queue::new();
		let mut state = self.shared.lock();
		let id = state.next_listener;
		state.next_listener += 1;
		let listener = Listener {
			id,
			root: self.root.clone(),
			heads: interest_prefixes(&self.allowed()),
			allowed: self.scope.allowed.clone(),
			link: self.link,
			queue: queue.clone(),
		};
		match state.closed {
			true => queue.close(),
			false => {
				for prefix in state.routes.interests_from(&Path::empty()) {
					for relative in listener.asks(&prefix) {
						// A head too long to root covers no path, so nothing reads it.
						let Ok(absolute) = self.root.join(&relative) else {
							continue;
						};
						let snapshot = state.snapshot(&self.shared, absolute, self.link);
						let _ = queue.try_push(InterestChange {
							prefix: relative,
							wanted: true,
							snapshot: Some(snapshot),
						});
					}
				}
				state.listeners.push(listener);
			}
		}
		Interests {
			shared: self.shared.clone(),
			id,
			queue,
		}
	}

	/// Whether routes arrive here on a link between relays ([`Link::Peer`] or
	/// [`Link::Upstream`]), whose sessions discover everything they may.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn relays(&self) -> bool {
		self.link != Link::Local
	}

	/// Create a [`Claim`] over `prefix` that serves the requests beneath it.
	///
	/// A route is always a prefix on the wire: a claim covers `prefix` and every path
	/// beneath it (the empty prefix claims every path). A service that only serves some of
	/// them, say `pid/*.hang`, advertises the covering prefix and refuses the
	/// rest as they are requested; consumers narrow with a [`Pattern`] locally.
	/// This is the one shape every wire carries, so a route means the same
	/// thing on every hop.
	///
	/// A consumer resolving a path under it through this route is handed to
	/// [`Claim::poll_requested`] as a [`Request`]: answer it by serving a broadcast already
	/// held ([`Request::serve`]), under this route's instance, or by creating the path's
	/// own exact route ([`Request::route`]). This is how a service answers a whole
	/// subtree without publishing each path ahead of time; a publisher that
	/// knows its broadcasts creates each one's exact route with [`Self::route`]
	/// instead, so subscribers can enumerate them.
	///
	/// Unless `options` names an instance, the route lends the one it mints to the exact
	/// routes answering its requests, so a reader that resolved a path through it
	/// continues on that path's answer rather than restarting. Once an exact route holding
	/// the lent instance stops serving it (withdrawn, closed, or renamed), this route moves
	/// to a fresh instance, as a restarted job's keys are another stream: see
	/// [`Claim::instance`]. A claim naming a stable instance lends nothing, and its
	/// answers each mint their own.
	///
	/// The claim is priced, credentialed and shown like an exact route. It is visible
	/// for as long as the returned [`Claim`] (and every clone) lives, unless withdrawn.
	///
	/// The prefix must overlap this producer's pattern scope. Individual requests
	/// remain authoritative and are refused when they do not match the scope.
	pub fn claim(&self, prefix: impl TryInto<Path, Error: Into<Error>>, options: RouteOptions) -> Result<Claim, Error> {
		// A minted instance lives in the table, which moves it on as its jobs end; see
		// [`OriginState::advance`].
		let info = RouteInfo {
			instance: options.instance.as_ref().map(|(instance, _)| instance.clone()),
			epoch: options.instance.as_ref().map_or(0, |(_, epoch)| *epoch),
			..options.info()
		};
		let advertised = !options.hidden;
		let lends = options.instance.is_none();
		self.serving_route(prefix, info, Some(self.clone()), advertised, lends, options)
	}

	/// Land a peer's route with its wire metadata.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn forward(
		&self,
		prefix: impl TryInto<Path, Error: Into<Error>>,
		mut info: RouteInfo,
	) -> Result<Claim, Error> {
		// A handle that names no instance serves one of its own.
		let instance = info.instance.get_or_insert_with(Instance::random).clone();
		let options = RouteOptions {
			hidden: false,
			cost: info.cost,
			auth: info.auth.clone(),
			instance: Some((instance, info.epoch)),
		};
		self.serving_route(prefix, info, None, true, false, options)
	}

	fn serving_route(
		&self,
		prefix: impl TryInto<Path, Error: Into<Error>>,
		info: RouteInfo,
		publisher: Option<Producer>,
		advertised: bool,
		lends: bool,
		options: RouteOptions,
	) -> Result<Claim, Error> {
		let announcing = Announcing::new(self, prefix)?;
		let serve = kio::Shared::<ServeState>::default();
		serve.lock().requests.add_handler();
		let announcement = announcing.announce(
			info,
			Serving {
				server: Some(serve.clone()),
				source: None,
				advertised,
				lends,
			},
			None,
		)?;
		let control = RouteControl::new(options, &announcement, None);
		// A session's route outlives nothing it was given: dropping it means the session lost it.
		let forwarded = publisher.is_none();
		Ok(Claim {
			state: Arc::new(RouteState {
				announcement,
				control: kio::Lock::new(control),
				forwarded,
				_keepalive: self.tasks.keepalive(),
			}),
			server: serve,
			publisher,
		})
	}

	/// Returns a producer rooted at `root` and restricted to matching `patterns`.
	///
	/// `root` is relative to this producer's root, and `patterns` are relative to
	/// the new root. Returns [`Error::Unauthorized`] when the requested scope has
	/// no overlap with this producer's scope, or [`Error::BoundsExceeded`] when
	/// rooting the patterns would exceed the path limit.
	pub fn scope(&self, root: impl TryInto<Path, Error: Into<Error>>, patterns: &Patterns) -> Result<Producer, Error> {
		let root = self.root.join(&path::convert(root)?)?;
		let rooted = patterns.rooted(root.as_str()).map_err(|_| BoundsExceeded)?;
		let scope = self.scope.narrow(&rooted).ok_or(Error::Unauthorized)?;
		Ok(Producer {
			hop: self.hop,
			max_hops: self.max_hops,
			scope,
			root,
			shared: self.shared.clone(),
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			stats: self.stats.clone(),
			link: self.link,
			preference: self.preference,
			proxy: self.proxy,
			tasks: self.tasks.clone(),
			timers: self.timers.clone(),
			frames: self.frames.clone(),
		})
	}

	/// A clone that never reaches the subtree at `prefix`, relative to this root:
	/// nothing is published or announced at or beneath it, and a consumer derived
	/// from the result does not see it. [`allowed`](Self::allowed) is unchanged.
	pub fn exclude(&self, prefix: impl TryInto<Path, Error: Into<Error>>) -> Result<Self, Error> {
		let prefix = self.root.join(&path::convert(prefix)?)?;
		let mut producer = self.clone();
		producer.scope.excluded = self.scope.excluded.iter().cloned().chain([prefix]).collect();
		Ok(producer)
	}

	/// Returns a producer that reads the subtree at `at` from `target` instead.
	///
	/// Both are relative to this producer's root. A consumer derived from the
	/// result resolves `at/rest` at `target/rest`, through the one front serving
	/// that path, and presents the routes under `target` (or covering it) under
	/// `at`. Permissions stay named from the handle's side: a later
	/// [`scope`](Self::scope) authorizes `at/rest` as written. The mount is
	/// read-only: nothing is published at or beneath `at`, so what a reader finds
	/// there is only ever `target`'s. Whatever the origin holds at `at` itself is
	/// hidden from the mounted handle.
	///
	/// Returns [`Error::Unauthorized`] unless this producer reaches all of
	/// `target`, so a mount never widens a scope, and [`Error::Duplicate`] when
	/// `at` or `target` overlaps a mount point this producer already has, or `at`
	/// overlaps an existing target or its own: mounts never chain, in whatever order they are added,
	/// so a target is always read as the origin holds it. Mounts may share a target.
	/// [`Error::BoundsExceeded`] if either rooted path exceeds [`Path::MAX_PARTS`] or [`Path::MAX_BYTES`],
	/// and [`Error::InvalidPath`] if `at` holds a segment no pattern can spell.
	pub fn mount(
		&self,
		at: impl TryInto<Path, Error: Into<Error>>,
		target: impl TryInto<Path, Error: Into<Error>>,
	) -> Result<Producer, Error> {
		let at = self.root.join(&path::convert(at)?)?;
		let target = self.root.join(&path::convert(target)?)?;
		if at.parts().count() > Path::MAX_PARTS || target.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}
		// A mount point no pattern can spell could never be announced or authorized.
		Pattern::literal(at.as_str())?;
		if !self
			.scope
			.allowed
			.covers(&Patterns::from(Pattern::subtree(target.as_str())?))
		{
			return Err(Error::Unauthorized);
		}
		// Symmetric, so the order mounts are added never changes which sets are accepted:
		// no mount point overlaps any mount's point or target, its own included.
		// Targets may overlap.
		let overlaps = |a: &Path, b: &Path| a.has_prefix(b) || b.has_prefix(a);
		if overlaps(&at, &target)
			|| self
				.scope
				.mounts
				.iter()
				.any(|mount| overlaps(&at, &mount.at) || overlaps(&target, &mount.at) || overlaps(&at, &mount.target))
		{
			return Err(Error::Duplicate);
		}
		let mounts = self
			.scope
			.mounts
			.iter()
			.cloned()
			.chain([Mount { at, target }])
			.collect();
		Ok(Producer {
			scope: OriginScope {
				mounts,
				..self.scope.clone()
			},
			..self.clone()
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
	pub fn root(&self) -> &Path {
		&self.root
	}

	/// The patterns this producer may publish under, relative to its root.
	pub fn allowed(&self) -> Patterns {
		self.scope.relative(&self.root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl TryInto<Path, Error: Into<Error>>) -> Result<Path, Error> {
		Ok(self.root.join(&path::convert(path)?)?)
	}
}

/// What it takes to insert a route: the prefixes it covers and the origin table
/// to insert them into. Built by [`Producer::route`], [`Producer::claim`]
/// and the advertise-only test helper.
struct Announcing {
	hop: Hop,
	shared: SendCell<kio::Shared<OriginState>>,
	/// The absolute advertised prefix, which also keys the ingress announce counters.
	requested: Path,
	/// The prefix inserted into the table, with its [`prefix_claim`]. Pattern
	/// scopes decide visibility and request authorization without changing the
	/// route's prefix shape.
	prefixes: Vec<(Path, Pattern)>,
	/// The absolute paths the producer is authorized to serve.
	scope: Patterns,
	/// The absolute subtrees the producer may not serve.
	excluded: Arc<[Path]>,
	local: bool,
	/// The link the producer's routes arrive on.
	link: Link,
	/// The rank of that link; a broadcast published here is on none, so it ranks at the default.
	preference: Preference,
	stats: stats::Session,
}

impl Announcing {
	/// The requested prefix as-is, refused when its subtree does not overlap the scope.
	fn new(producer: &Producer, prefix: impl TryInto<Path, Error: Into<Error>>) -> Result<Self, Error> {
		let requested = producer.root.join(&path::convert(prefix)?)?;
		publish_path(&requested)?;
		if requested.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}
		let claim = prefix_claim(&requested)?;
		if !producer.scope.allowed.overlaps(&claim) || !producer.scope.publishes(&requested) {
			return Err(Error::Unauthorized);
		}
		Ok(Self {
			hop: Hop::from(producer.hop),
			shared: producer.shared.clone(),
			requested: requested.clone(),
			prefixes: vec![(requested, claim)],
			scope: producer.scope.allowed.clone(),
			excluded: producer.scope.excluded.clone(),
			local: false,
			link: producer.link,
			preference: producer.preference,
			stats: producer.stats.clone(),
		})
	}

	/// Insert the route. A job's route naming no instance reads as its `lender`'s, the
	/// prefix route its request came through, until it binds one as it announces.
	fn announce(
		&self,
		route: RouteInfo,
		serving: Serving,
		lender: Option<&kio::Shared<ServeState>>,
	) -> Result<AnnounceProducer, Error> {
		debug_assert!(
			!route.hops.contains(&self.hop),
			"announce called with a looping hop chain",
		);

		let via = route.via;

		let mut shared = self.shared.lock();
		if shared.closed {
			return Err(Error::Closed);
		}

		// A handle that names no instance serves one of its own.
		let minted = route.instance.clone().unwrap_or_else(Instance::random);
		let mut entries = Vec::with_capacity(self.prefixes.len());
		for (prefix, claim) in &self.prefixes {
			shared.reannounced(prefix, &route.hops);
			let id = shared.next_route;
			shared.next_route += 1;
			let stale = shared.withdrawn_through(prefix, &route.hops);
			let (instance, epoch) = match lender.and_then(|server| shared.lender(prefix, server)) {
				Some(lent) => (lent.instance, lent.epoch),
				None => (minted.clone(), route.epoch),
			};
			shared.routes.insert(RouteEntry {
				id,
				instance,
				epoch,
				prefix: prefix.clone(),
				scope: self.scope.clone(),
				excluded: self.excluded.clone(),
				hops: route.hops.clone(),
				cost: route.cost,
				auth: route.auth.clone(),
				preference: self.preference,
				via,
				local: self.local,
				link: self.link,
				node: route.node,
				draining: route.draining,
				available: route.available,
				rtt: route.rtt,
				server: serving.server.clone(),
				source: serving.source.clone(),
				advertised: serving.advertised,
				stale,
				claim: claim.clone(),
				reach: reach(&self.scope, claim),
				lends: serving.lends,
				lent: None,
			});
			shared.sync_route(prefix, claim, Gone::End);
			entries.push((prefix.clone(), id));
		}
		drop(shared);

		// Ingress announce guard: held while the route is advertised.
		let ingress = self.stats.ingress(&self.requested);
		let guard = serving.advertised.then(|| ingress.announce());

		Ok(AnnounceProducer {
			shared: self.shared.clone(),
			entries,
			ingress,
			guard: kio::Lock::new(guard),
		})
	}
}

/// What a route entry serves and whether cursors see it.
struct Serving {
	server: Option<kio::Shared<ServeState>>,
	source: Option<broadcast::Consumer>,
	advertised: bool,
	/// Whether the job routes answering it take its instance; see [`RouteEntry::lends`].
	lends: bool,
}

/// An exact route on an origin: one path, owning the broadcast published there.
///
/// On the publishing side ownership runs origin → route → broadcast → track → group →
/// frame. Created by [`Producer::route`], or by [`Request::route`] to answer a request
/// with the path's own route. A prefix served on demand is a [`Claim`] instead.
///
/// A route mints its instance when it is created and keeps it for its lifetime, across
/// [`withdraw`](Self::withdraw), [`announce`](Self::announce) and reconnects, since it
/// owns the same broadcast and its keys continue. A new run is a new route.
///
/// ```
/// # fn run(origin: moq_net::origin::Producer) -> Result<(), moq_net::Error> {
/// use moq_net::origin::RouteOptions;
///
/// // Fill the catalog before readers can find it.
/// let route = origin.route("live/alice", RouteOptions::hidden())?;
/// let _catalog = route.broadcast().create_track("catalog.json", None)?;
/// route.announce()?; // visible now, same instance
/// route.set_cost(5)?; // re-prices in place
/// route.withdraw(); // hidden; announce() again continues the same stream
/// # Ok(())
/// # }
/// ```
///
/// Clones share one route. Dropping the last clone retracts it, and so does closing
/// its broadcast. The broadcast lives on for the subscriptions already on it for as
/// long as another of its producer handles does. The route keeps the origin's
/// [`Driver`] running, so a publisher can drop every [`Producer`] and keep serving.
#[derive(Clone)]
#[must_use = "dropping the last clone of a route retracts it"]
pub struct Route {
	// Declared first so the advertisement retracts before the broadcast handle drops.
	state: Arc<RouteState>,
	broadcast: broadcast::Producer,
}

/// A prefix route on an origin: a claim that every path beneath a prefix can be served,
/// answering each request as it arrives.
///
/// Created by [`Producer::claim`]. Requests arrive on [`Self::poll_requested`]; answer
/// each by serving a broadcast already held or by creating the path's own [`Route`].
///
/// Clones share one claim. Dropping the last clone retracts it and refuses the requests
/// still waiting. The claim keeps the origin's [`Driver`] running.
#[derive(Clone)]
#[must_use = "dropping the last clone of a claim retracts it"]
pub struct Claim {
	state: Arc<RouteState>,
	/// The queue requests arrive on.
	server: kio::Shared<ServeState>,
	/// Present for an application's claim, whose requests may create their own exact
	/// routes. A session-forwarded request can only serve its peer's broadcast.
	publisher: Option<Producer>,
}

/// What a [`Route`] and a [`Claim`] share: the advertisement and how it is changed.
struct RouteState {
	/// The advertisement, retracted on drop.
	announcement: AnnounceProducer,
	control: kio::Lock<RouteControl>,
	/// Whether a session landed it for its peer, so dropping it means the session lost
	/// the route ([`RouteKind::Offline`]), not that its origin ended it.
	forwarded: bool,
	/// A route is lifecycle work: the origin's driver keeps running for as long as one
	/// lives, even once every producer handle is gone.
	_keepalive: Keepalive,
}

/// What a route advertises when it announces, changed under one lock so concurrent
/// announces, withdrawals and re-prices land in order.
struct RouteControl {
	options: RouteOptions,
	/// The instance and epoch the route advertises, as last seen when [`Self::held`].
	instance: (Instance, u64),
	/// Whether the table entry holds the instance: a prefix route that lends its own, which
	/// moves on as its jobs end (see [`OriginState::advance`]), or a job's route taking one
	/// from it. An update then names none, leaving the entry's as it is.
	held: bool,
	/// Whether a job's route read its instance before it first announced: it then announces
	/// under that one or not at all, so whatever was built from it, such as a signer, stays
	/// valid.
	bound: bool,
	/// The request a job's route answers when it first announces.
	request: Option<Request>,
}

impl RouteControl {
	fn new(options: RouteOptions, announcement: &AnnounceProducer, request: Option<Request>) -> Self {
		let held = options.instance.is_none();
		let instance = announcement
			.instance(None)
			.expect("a route just inserted has its entries");
		Self {
			options,
			instance,
			held,
			bound: false,
			request,
		}
	}

	/// What to advertise.
	fn info(&self) -> RouteInfo {
		let (instance, epoch) = &self.instance;
		RouteInfo {
			instance: (!self.held).then(|| instance.clone()),
			epoch: *epoch,
			..self.options.info()
		}
	}
}

impl RouteState {
	/// The broadcast closed: retract the route for good and refuse a job's unanswered
	/// request.
	fn ended(&self) {
		// Retracted under the control lock, so an announce racing the close either lands
		// first or finds the entries gone, never advertises after it.
		let mut control = self.control.lock();
		let request = control.request.take();
		self.announcement.retract(Retract::Ended);
		drop(control);
		// Dropped outside every lock: refusing it wakes the requesters.
		drop(request);
	}
}

impl RouteState {
	/// See [`Route::instance`] and [`Claim::instance`].
	fn instance(&self) -> Instance {
		let mut control = self.control.lock();
		if control.held && !control.bound {
			let lender = control.request.as_ref().map(|request| &request.home);
			if let Some(current) = self.announcement.instance(lender) {
				control.instance = current;
			}
			control.bound = control.request.is_some();
		}
		control.instance.0.clone()
	}

	/// See [`Route::withdraw`] and [`Claim::withdraw`].
	fn withdraw(&self) {
		let _control = self.control.lock();
		self.announcement.withdraw();
	}

	/// See [`Route::is_announced`] and [`Claim::is_announced`].
	fn is_announced(&self) -> bool {
		self.announcement.route().is_some()
	}

	/// Apply `update` to the options, re-advertising a visible route in place. A failed
	/// change keeps the options as they were, so a closed route holds nothing it was given.
	fn change(&self, update: impl FnOnce(&mut RouteOptions)) -> Result<(), Error> {
		let mut control = self.control.lock();
		let previous = control.options.clone();
		update(&mut control.options);
		// One rollback for both failures: closed before the check, or between it and the update.
		let result = match self.announcement.is_live() {
			Ok(true) => self.announcement.update_if(control.info(), None, |_| true).map(drop),
			Ok(false) => Ok(()),
			Err(err) => Err(err),
		};
		if result.is_err() {
			control.options = previous;
		}
		result
	}
}

impl Route {
	/// The broadcast this route owns, to fill with tracks.
	pub fn broadcast(&self) -> &broadcast::Producer {
		&self.broadcast
	}

	/// The stream this route serves, minted when it was created unless
	/// [`RouteOptions::with_instance`] named one.
	///
	/// A job's route ([`Request::route`]) naming none takes its claim's current one when it
	/// first announces, or when this reads it first. Read before announcing, it is the one
	/// the job announces under, and [`Self::announce`] fails with [`Error::Unroutable`] once
	/// the claim has moved past it.
	pub fn instance(&self) -> Instance {
		self.state.instance()
	}

	/// Make the route visible, or keep it so. Idempotent.
	///
	/// Visible to local consumers before it returns. A job's route ([`Request::route`])
	/// also answers the requesters waiting on its path.
	///
	/// Fails with [`Error::Closed`] once the route's broadcast closed or the origin's
	/// [`Driver`] was dropped, and with [`Error::Unroutable`], announcing nothing, when a
	/// job's request was released or its claim retracted meanwhile, or when the job read
	/// its [`instance`](Self::instance) and its claim has moved past it since: that route
	/// can never announce. In that last case the request goes back to the claim's handler,
	/// its requesters still waiting.
	pub fn announce(&self) -> Result<(), Error> {
		let mut control = self.state.control.lock();
		let info = control.info();
		let Some(mut request) = control.request.take() else {
			return self.state.announcement.update_if(info, None, |_| true).map(|_| ());
		};
		let bound = std::mem::take(&mut control.bound).then(|| control.instance.0.clone());
		let mut moved = false;
		let answer = self.broadcast.consume();
		let mut resolved = None;
		// Answered under the table lock that every release takes first, so the route
		// lands only for a request still pending and no release can slip in between.
		// A job's route naming no instance takes its claim's current one as it lands, so
		// whatever resolved through the claim for the path continues on it rather than
		// restarting.
		let lender = control.held.then_some(&request.home);
		let admitted = self.state.announcement.update_if(info, lender, |table| {
			// The claim's entries leave the table before its retraction releases the
			// request, so one gone from it no longer answers, even before that release.
			let home = table.routes.covering(&request.path).any(|entry| {
				entry
					.server
					.as_ref()
					.is_some_and(|server| server.same_channel(&request.home))
			});
			if !home {
				return false;
			}
			// A job that read its instance announces under it or not at all. A claim that
			// lends nothing never moves, and one no longer serving released the request.
			if let Some(bound) = &bound
				&& table
					.lender(&request.path, &request.home)
					.is_some_and(|lent| lent.instance != *bound)
			{
				moved = true;
				return false;
			}
			// The exact route owns this answer's availability. Caching it under the
			// claim would keep serving it after this route is withdrawn.
			resolved = request
				.home
				.lock()
				.take(&request.path, &request.producer)
				.map(|()| Ok(answer));
			resolved.is_some()
		});
		let Some(resolved) = resolved.filter(|_| matches!(admitted, Ok(true))) else {
			// A failed answer can never announce. Retracted under the control lock, as
			// `ended` does, so a clone's racing announce finds the entries gone rather than
			// advertising through the ordinary branch now that the request is taken.
			self.state.announcement.retract(Retract::Ended);
			drop(control);
			// The claim moved past the instance the job read: its requesters wait on for
			// the handler to prepare the job again under the new one.
			request.requeue = moved;
			drop(request);
			return Err(admitted.err().unwrap_or(Error::Unroutable));
		};
		drop(control);
		// Woken outside every lock: a front re-entering the table must not find it held.
		if let Ok(mut pending) = request.producer.write() {
			pending.resolved.get_or_insert(resolved);
		}
		Ok(())
	}

	/// Hide the route from everyone, local and remote alike, keeping its broadcast and
	/// instance: [`Self::announce`] brings the same stream back. Requests for the path
	/// fail with [`Error::Unroutable`] meanwhile, while tracks already in flight carry
	/// on to their own end. Idempotent.
	pub fn withdraw(&self) {
		self.state.withdraw()
	}

	/// Whether the route is visible.
	pub fn is_announced(&self) -> bool {
		self.state.is_announced()
	}

	/// Re-price the route: lower wins among routes covering the same prefix and anonymity.
	/// A visible route changes in place, as an update to its readers; a hidden one
	/// keeps the price for when it next announces.
	///
	/// Fails with [`Error::Closed`] once the route's broadcast closed or the origin's
	/// [`Driver`] was dropped.
	pub fn set_cost(&self, cost: impl Into<Cost>) -> Result<(), Error> {
		let cost = cost.into();
		self.state.change(|options| options.cost = cost)
	}

	/// Swap the credential the advertising request carries; see [`RouteOptions::with_auth`].
	/// `None` advertises under the session's scope.
	///
	/// Fails with [`Error::Closed`] once the route's broadcast closed or the origin's
	/// [`Driver`] was dropped.
	pub fn set_auth(&self, auth: Option<crate::auth::Credential>) -> Result<(), Error> {
		self.state.change(|options| options.auth = auth)
	}

	/// The route metadata as advertised, or `None` while hidden.
	#[cfg(test)]
	pub(crate) fn route(&self) -> Option<RouteInfo> {
		self.state.announcement.route()
	}

	/// Announce with `info` as given, instance included when it names one, as a
	/// session's update does: for tests that shape an exact route's metadata.
	#[cfg(all(test, feature = "async"))]
	pub(crate) fn announce_info(&self, mut info: RouteInfo) -> Result<(), Error> {
		let mut control = self.state.control.lock();
		match &info.instance {
			Some(instance) => {
				control.instance = (instance.clone(), info.epoch);
				control.held = false;
			}
			None => {
				info.instance = Some(control.instance.0.clone());
				info.epoch = control.instance.1;
			}
		}
		control.options.cost = info.cost;
		control.options.auth = info.auth.clone();
		self.state.announcement.update_if(info, None, |_| true).map(|_| ())
	}
}

impl Claim {
	/// The stream this claim serves, minted when it was created unless
	/// [`RouteOptions::with_instance`] named one.
	///
	/// Unnamed, it can change: the claim moves to a fresh instance each time an answer
	/// holding its current one ends. See [`Producer::claim`].
	pub fn instance(&self) -> Instance {
		self.state.instance()
	}

	/// Make the claim visible, or keep it so. Idempotent. Visible to local consumers
	/// before it returns.
	///
	/// Fails with [`Error::Closed`] once the origin's [`Driver`] was dropped.
	pub fn announce(&self) -> Result<(), Error> {
		let control = self.state.control.lock();
		self.state
			.announcement
			.update_if(control.info(), None, |_| true)
			.map(|_| ())
	}

	/// Hide the claim from everyone, local and remote alike, keeping its instance:
	/// [`Self::announce`] brings it back. Requests beneath it fail with
	/// [`Error::Unroutable`] meanwhile. Idempotent.
	pub fn withdraw(&self) {
		self.state.withdraw()
	}

	/// Whether the claim is visible.
	pub fn is_announced(&self) -> bool {
		self.state.is_announced()
	}

	/// Re-price the claim: lower wins among routes covering the same prefix and anonymity.
	/// A visible claim changes in place, as an update to its readers; a hidden one
	/// keeps the price for when it next announces.
	///
	/// Fails with [`Error::Closed`] once the origin's [`Driver`] was dropped.
	pub fn set_cost(&self, cost: impl Into<Cost>) -> Result<(), Error> {
		let cost = cost.into();
		self.state.change(|options| options.cost = cost)
	}

	/// Swap the credential the advertising request carries; see [`RouteOptions::with_auth`].
	/// `None` advertises under the session's scope.
	///
	/// Fails with [`Error::Closed`] once the origin's [`Driver`] was dropped.
	pub fn set_auth(&self, auth: Option<crate::auth::Credential>) -> Result<(), Error> {
		self.state.change(|options| options.auth = auth)
	}

	/// The next path a consumer requests beneath this claim, as a [`Request`] to answer.
	///
	/// Takes `&self` so a handler can answer from one task while another re-prices the
	/// claim; concurrent callers each receive distinct requests. Returns `None` once the
	/// claim is retracted or the origin's [`Driver`] was dropped, so handler loops end.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_requested(&self, waiter: &kio::Waiter) -> Poll<Option<Request>> {
		let mut state = ready!(self.server.poll(waiter, |state| {
			if state.closed || state.requests.has_queued() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));

		// The teardown already drained the queue, so there is nothing left to pop.
		if state.closed {
			return Poll::Ready(None);
		}

		let path = state.requests.pop().expect("predicate guaranteed a request");
		// The popped request stays pending, so a repeat request in the window between
		// hand-off and answer coalesces onto it instead of re-invoking the handler. The
		// producer is a shared clone; answering or dropping the `Request` removes the
		// entry. This mirrors how `poll_requested_track` keeps a served track
		// discoverable via the weak cache across the same window.
		let producer = state.requests.get(&path).expect("popped key must be pending").clone();
		Poll::Ready(Some(Request {
			path,
			producer,
			home: self.server.clone(),
			publisher: self.publisher.clone(),
			requeue: false,
		}))
	}

	/// The claim metadata as advertised, or `None` while hidden.
	#[cfg(test)]
	pub(crate) fn route(&self) -> Option<RouteInfo> {
		self.state.announcement.route()
	}

	/// The queue the claim's requests arrive on, for tests that inspect it.
	#[cfg(test)]
	fn server(&self) -> &kio::Shared<ServeState> {
		&self.server
	}

	/// Replace a forwarded claim's metadata in place, taken as given.
	///
	/// A claim naming no [`RouteInfo::instance`], or the same one, re-prices: consumers
	/// observe an update and every handle survives. Another instance is another stream:
	/// consumers see a restart, and a re-request never joins what was resolved under the
	/// old one. Requests still waiting on this handle carry over: the handler is asked
	/// again under the new instance, and its answer to a request asked before the change
	/// is dropped, never served under the new one. A request pinned to the old instance
	/// is refused with [`Error::Unroutable`]. The answers already served are forgotten,
	/// so the next request for one of those paths asks the handler again too. The
	/// broadcasts it served keep running for the subscriptions already on them.
	///
	/// Fails with [`Error::Closed`] once the origin's [`Driver`] has been dropped.
	#[cfg(any(feature = "async", test))]
	pub(crate) fn update(&self, info: RouteInfo) -> Result<(), Error> {
		let mut control = self.state.control.lock();
		if let Some(instance) = &info.instance {
			control.instance = (instance.clone(), info.epoch);
			control.held = false;
		}
		control.options.cost = info.cost;
		control.options.auth = info.auth.clone();
		self.state.announcement.update(info)
	}

	/// Retire an explicit peer withdrawal, hiding derived paths only once its
	/// last live advertisement at the prefix is gone. Readers see it end. A lost
	/// session just drops, and readers see it go offline.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn withdrawn(self) {
		self.state.announcement.retract(Retract::Withdrawn);
	}

	/// Retire a route its announcer ended without a hop chain to stale, such as a
	/// moq-transport PUBLISH_NAMESPACE_DONE: readers see it end, not go offline.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn ended(self) {
		self.state.announcement.retract(Retract::Ended);
	}
}

impl Drop for RouteState {
	fn drop(&mut self) {
		// Retract before a job's unanswered request drops, so its requesters are refused
		// rather than asked again through this route.
		self.announcement.retract(match self.forwarded {
			true => Retract::Lost,
			false => Retract::Ended,
		});
	}
}

/// Why an advertisement leaves the table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retract {
	/// The session it arrived on lost it: readers see it go offline.
	Lost,
	/// Its announcer ended it: readers see it end.
	Ended,
	/// A peer withdrew it, which also stales the routes derived through that peer.
	Withdrawn,
}

/// The write half of an advertisement: a live claim that paths under a
/// [`Pattern`] can be served.
///
/// Held by a [`Route`]; dropping it retracts the route, which [`Routes`]s
/// observe and sessions withdraw from their peers.
#[must_use = "dropping an announcement retracts the route"]
pub(crate) struct AnnounceProducer {
	shared: SendCell<kio::Shared<OriginState>>,
	/// The table entries this advertisement created, by prefix and id. A prefix
	/// remains unchanged; pattern scopes only filter its visibility and requests.
	entries: Vec<(Path, u64)>,
	/// The ingress counters an advertised interval's announce guard comes from.
	ingress: stats::Scope,
	/// Ingress announce stats guard, held only while the entries are advertised.
	guard: kio::Lock<Option<stats::Announce>>,
}

impl AnnounceProducer {
	/// Replace the route in place, instance included when it names one; see
	/// [`Claim::update`].
	///
	/// The prefix is fixed at announce time and a [`RouteInfo`] cannot name one: to
	/// move an advertisement, drop this and announce again. Fails with
	/// [`Error::Closed`] once the origin's [`Driver`] has been dropped.
	#[cfg(any(feature = "async", test))]
	pub fn update(&self, route: RouteInfo) -> Result<(), Error> {
		self.update_if(route, None, |_| true).map(|_| ())
	}

	/// [`Self::update`] once `admit` agrees, returning whether it did. `admit` runs
	/// under the table lock, so nothing ordered after that lock (a release, a
	/// retraction, the teardown) can change its answer before the route lands.
	///
	/// A job's route answering its request through `lender`, a prefix route that lends
	/// its instance, takes that route's instance and epoch as of now.
	fn update_if(
		&self,
		route: RouteInfo,
		lender: Option<&kio::Shared<ServeState>>,
		admit: impl FnOnce(&OriginState) -> bool,
	) -> Result<bool, Error> {
		let mut shared = self.shared.lock();
		if shared.closed {
			return Err(Error::Closed);
		}
		if !self
			.entries
			.iter()
			.all(|(prefix, id)| shared.routes.entry(prefix, *id).is_some())
		{
			return Err(Error::Closed);
		}
		if !admit(&shared) {
			return Ok(false);
		}
		// The requests an instance change released, refused once the table lock is gone: the
		// refusal wakes their fronts, which read the table.
		let mut released = Vec::new();
		for (prefix, id) in &self.entries {
			shared.reannounced(prefix, &route.hops);
			let stale = shared.withdrawn_through(prefix, &route.hops);
			let lent = lender.and_then(|server| shared.lender(prefix, server));
			// Each entry keeps its advertised prefix; only the metadata moves.
			let Some(entry) = shared.routes.entry_mut(prefix, *id) else {
				return Err(Error::Closed);
			};
			let next = match &lent {
				Some(lent) => Some((lent.instance.clone(), lent.epoch)),
				None => route.instance.clone().map(|instance| (instance, route.epoch)),
			};
			let mut spent = None;
			if let Some((instance, epoch)) = next {
				if instance != entry.instance {
					// Another stream: what its server was asked, or answered, for the old one
					// is not its own. Under the table lock, so no front sees the new instance
					// while an old answer can still land. A higher epoch of the same one
					// continues it.
					if let Some(server) = &entry.server {
						released.extend(server.lock().renew());
					}
					// The instance it took stops serving here.
					if entry.advertised {
						spent = entry.lent.take().map(|lender| (lender, entry.instance.clone()));
					}
				}
				entry.instance = instance;
				entry.epoch = epoch;
			}
			if let Some(lent) = lent {
				entry.lent = Some((lent.prefix, lent.id));
			}
			entry.hops = route.hops.clone();
			entry.stale = stale;
			entry.cost = route.cost;
			entry.auth = route.auth.clone();
			entry.via = route.via;
			entry.node = route.node;
			entry.draining = route.draining;
			entry.available = route.available;
			entry.rtt = route.rtt;
			entry.advertised = true;
			let claim = entry.claim.clone();
			shared.sync_route(prefix, &claim, Gone::End);
			if let Some(((lender, lender_id), instance)) = spent {
				shared.advance(&lender, lender_id, &instance);
			}
			shared.prune_withdrawn(prefix);
		}
		// Under the table lock, as withdrawing and retracting clear it, so a racing one
		// never leaves the guard held for entries no longer advertised.
		self.guard.lock().get_or_insert_with(|| self.ingress.announce());
		drop(shared);
		for producer in released {
			if let Ok(mut request) = producer.write() {
				request.resolved.get_or_insert(Err(Error::Unroutable));
			}
		}
		Ok(true)
	}

	/// The instance and epoch the entries carry, or `None` once they are gone. A job's
	/// route not yet announced reads its `lender`'s current one, which it would bind now.
	fn instance(&self, lender: Option<&kio::Shared<ServeState>>) -> Option<(Instance, u64)> {
		let shared = self.shared.lock();
		let (prefix, id) = self.entries.first()?;
		let entry = shared.routes.entry(prefix, *id)?;
		if let Some(lent) = lender.and_then(|server| shared.lender(prefix, server)) {
			return Some((lent.instance, lent.epoch));
		}
		Some((entry.instance.clone(), entry.epoch))
	}

	/// Whether the entries are advertised, or [`Error::Closed`] once they are gone.
	fn is_live(&self) -> Result<bool, Error> {
		let shared = self.shared.lock();
		if shared.closed {
			return Err(Error::Closed);
		}
		let mut advertised = false;
		for (prefix, id) in &self.entries {
			advertised |= shared.routes.entry(prefix, *id).ok_or(Error::Closed)?.advertised;
		}
		Ok(advertised)
	}

	/// The route as last given, while advertised: every entry carries the same one.
	pub(crate) fn route(&self) -> Option<RouteInfo> {
		let shared = self.shared.lock();
		let (prefix, id) = self.entries.first()?;
		let entry = shared.routes.entry(prefix, *id).filter(|entry| entry.advertised)?;
		Some(RouteInfo {
			instance: Some(entry.instance.clone()),
			epoch: entry.epoch,
			hops: entry.hops.clone(),
			cost: entry.cost,
			auth: entry.auth.clone(),
			via: entry.via,
			source: entry.entered(),
			node: entry.node,
			draining: entry.draining,
			available: entry.available,
			rtt: entry.rtt,
		})
	}

	/// Hide the entries from everyone, local and remote alike: cursors see a
	/// retraction and requests stop resolving through them. The entries stay,
	/// so announcing again restores the same route. What [`Route::withdraw`] does.
	///
	/// A prefix route's pending requests, queued or handed to a handler, are released
	/// under the table lock, so an answer prepared for one, such as a hidden job route,
	/// can no longer land once the route is withdrawn.
	fn withdraw(&self) {
		let mut shared = self.shared.lock();
		let guard = self.guard.lock().take();
		let mut released = Vec::new();
		for (prefix, id) in &self.entries {
			let Some(entry) = shared.routes.entry_mut(prefix, *id) else {
				continue;
			};
			if !entry.advertised {
				continue;
			}
			entry.advertised = false;
			if let Some(server) = &entry.server {
				released.extend(server.lock().renew());
			}
			let claim = entry.claim.clone();
			let spent = entry.lent.take().map(|lender| (lender, entry.instance.clone()));
			shared.sync_route(prefix, &claim, Gone::End);
			if let Some(((lender, lender_id), instance)) = spent {
				shared.advance(&lender, lender_id, &instance);
			}
		}
		drop(shared);
		drop(guard);
		// Refused once the table lock is gone: the refusal wakes their fronts, which read
		// the table.
		for producer in released {
			if let Ok(mut request) = producer.write() {
				request.resolved.get_or_insert(Err(Error::Unroutable));
			}
		}
	}

	/// Retract the route now: remove its table entries and reject anything still
	/// waiting on its queue. Idempotent, and what dropping the advertisement does.
	fn retract(&self, how: Retract) {
		let mut shared = self.shared.lock();
		let guard = self.guard.lock().take();
		let mut servers = Vec::new();
		for (prefix, id) in &self.entries {
			let Some(entry) = shared.routes.remove(prefix, *id) else {
				continue;
			};
			servers.extend(entry.server.clone());
			// A peer remains reachable while any of its sessions advertises this
			// prefix. Remove this session's route before testing the remaining
			// claims, under the same lock, so overlapping withdrawals cannot
			// invalidate a newer advertisement or miss the last withdrawal.
			if how == Retract::Withdrawn
				&& let Some(&peer) = entry.hops.iter().last()
				&& peer != Hop::UNKNOWN
				&& !shared
					.routes
					.at(prefix)
					.any(|other| other.live() && other.hops.iter().last() == Some(&peer))
			{
				shared.withdrawn.entry(prefix.clone()).or_default().insert(peer);
				shared.restale(prefix);
			}
			let gone = match how {
				Retract::Lost => Gone::Offline,
				Retract::Ended | Retract::Withdrawn => Gone::End,
			};
			shared.sync_route(&entry.prefix, &entry.claim, gone);
			if entry.advertised
				&& let Some((lender, lender_id)) = &entry.lent
			{
				shared.advance(lender, *lender_id, &entry.instance);
			}
			shared.prune_withdrawn(&entry.prefix);
		}
		drop(shared);
		drop(guard);
		// Reject anything still waiting on this route's server; a request already
		// handed to the handler resolves through its own `Request`. Closing wakes
		// the server's waiters, so it runs after the table unlocks; the entries
		// already left it, so nothing new routes here meanwhile, and a job route
		// answering one of them finds its prefix route gone (see [`Route::announce`]).
		for server in servers {
			let released = {
				let mut server = server.lock();
				server.closed = true;
				server.requests.drain_all()
			};
			for producer in released {
				if let Ok(mut request) = producer.write() {
					request.resolved.get_or_insert(Err(Error::Unroutable));
				}
			}
		}
	}
}

impl Drop for AnnounceProducer {
	fn drop(&mut self) {
		self.retract(Retract::Ended);
	}
}

/// Feeds a route layer node the routes this node serves itself, best per prefix,
/// for as long as the origin lives.
struct OwnClaims {
	routes: Routes,
	/// The same routes, less those learned on an upstream link.
	downstream: Routes,
	/// The claim fed at each prefix.
	best: HashMap<Path, crate::route::node::Claim>,
	/// The instance at each prefix that a route not learned upstream serves.
	local: HashMap<Path, Instance>,
	node: crate::route::Node,
}

/// The driver's task feeding [`OwnClaims`], which [`Consumer::route_node`] also catches up.
/// It ends once nothing owns the origin, after feeding the withdrawals still queued: its
/// cursors end only in the driver's teardown, which waits for this task.
struct FeedOwnClaims {
	claims: Arc<std::sync::Mutex<OwnClaims>>,
	origin: TasksWeak,
}

impl kio::Task for FeedOwnClaims {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Wakes wait for the feeder's lock, which drops before the hold; see
		// `Consumer::route_node`.
		let _hold = kio::hold();
		let mut claims = lock_own(&self.claims);
		if self.origin.poll_orphaned(waiter).is_ready() {
			claims.catch_up();
			return Poll::Ready(());
		}
		claims.poll(waiter)
	}
}

fn lock_own(own: &std::sync::Mutex<OwnClaims>) -> std::sync::MutexGuard<'_, OwnClaims> {
	own.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl OwnClaims {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			if let Poll::Ready(event) = self.downstream.poll_next(waiter) {
				match event {
					Some(event) => self.local(event),
					None => return Poll::Ready(()),
				}
				continue;
			}
			match ready!(self.routes.poll_next(waiter)) {
				Some(event) => self.apply(event),
				None => return Poll::Ready(()),
			}
		}
	}
}

impl OwnClaims {
	/// Feed the node every change the table already holds, so a stream registered next
	/// sees them without waiting for the driver.
	fn catch_up(&mut self) {
		while let Some(event) = self.downstream.try_next() {
			self.local(event);
		}
		while let Some(event) = self.routes.try_next() {
			self.apply(event);
		}
	}

	fn apply(&mut self, event: RouteEvent) {
		let announce = match event {
			RouteEvent {
				kind: RouteKind::Start | RouteKind::Update | RouteKind::Restart,
				announce,
			} => announce,
			RouteEvent {
				kind: RouteKind::End | RouteKind::Offline,
				announce,
			} => {
				self.best.remove(&announce.prefix);
				return self.node.own(announce.prefix, None);
			}
		};
		let route = announce.route;
		let Some(instance) = route.instance else {
			return;
		};
		let claim = crate::route::node::Claim {
			instance,
			epoch: route.epoch,
			cost: route.cost.value(),
			upstream: false,
			// Only a route published here has crossed no node.
			learned: !route.hops.is_empty(),
			// Kept, so the claim that crossed into lite-07 here is still recognized
			// wherever it leaves for lite-06 again.
			chain: route.hops,
		};
		self.best.insert(announce.prefix.clone(), claim);
		self.feed(announce.prefix);
	}

	/// The route not learned upstream changed at a prefix: whether the claim there may
	/// go to an upstream peer follows it.
	fn local(&mut self, event: RouteEvent) {
		match event {
			RouteEvent {
				kind: RouteKind::End | RouteKind::Offline,
				announce,
			} => {
				self.local.remove(&announce.prefix);
				self.feed(announce.prefix);
			}
			RouteEvent {
				kind: RouteKind::Start | RouteKind::Update | RouteKind::Restart,
				announce,
			} => {
				if let Some(instance) = announce.route.instance {
					self.local.insert(announce.prefix.clone(), instance);
				}
				self.feed(announce.prefix);
			}
		}
	}

	/// Feed the node the claim at `prefix`, marked as learned upstream unless a route
	/// not learned upstream serves the same instance there.
	fn feed(&self, prefix: Path) {
		let Some(claim) = self.best.get(&prefix) else {
			return;
		};
		let claim = crate::route::node::Claim {
			upstream: self.local.get(&prefix) != Some(&claim.instance),
			..claim.clone()
		};
		self.node.own(prefix, Some(claim));
	}
}

/// Drives origin lifecycle work and cache expiration with caller-supplied time.
///
/// Returned by [`Producer::new`]. Poll on external activity or at the deadline
/// it returns, supplying nondecreasing instants. RouteInfo changes, track serving, linger,
/// failover, and teardown run here; the route table and announce cursors update
/// synchronously when a route is announced or retracted.
///
/// It holds no [`Producer`] clone, so it never keeps the origin alive. It
/// finishes once every owner is gone: each [`Producer`] clone and [`Route`]. Read handles ([`Consumer`], [`Routes`])
/// are not owners. Dropping it aborts active fronts, rejects pending requests,
/// ends announcements, and makes subsequent producer mutations fail with
/// [`Error::Closed`].
/// `moq_tokio::origin::spawn` handles construction and driving for Tokio callers.
#[must_use = "poll the driver or the origin makes no progress"]
pub struct Driver {
	state: DriverState,
	// Shared by this origin's lifecycle tasks; advanced only when polled.
	timers: Deadlines,
	// The cache pool this origin's groups charge into, swept on a wall-clock
	// cadence so its idle window binds a track whose publisher stopped writing, and
	// collected at its reads' wall-clock deadlines.
	pool: cache::Pool,
}

/// Lifecycle work and the state it tears down.
struct DriverState {
	/// The front drivers: producers submit, this polls.
	set: TaskSet,
	/// The route table, announce cursors, and the remotely-served fronts, for
	/// ending everything on drop.
	shared: SendCell<kio::Shared<OriginState>>,
	/// Cached completion so a poll after `Ready` doesn't re-poll the drained set.
	done: bool,
}

impl Driver {
	/// Process ready origin work using caller-supplied monotonic time.
	///
	/// See [`crate::time::Driver`] for the contract. Finishes with
	/// [`Error::Closed`] once every owner (a [`Producer`] clone or [`Route`]) has dropped and the remaining lifecycle work
	/// has drained.
	pub fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Result<Option<Instant>, Error> {
		waiter.batch(|| {
			self.timers.advance(now);
			let result = self.state.poll(waiter);
			let gc = self.pool.poll(now, waiter);
			if result.is_ready() {
				return Err(Error::Closed);
			}
			Ok(self.timers.park(waiter).into_iter().chain(gc).min())
		})
	}
}

impl crate::time::Driver for Driver {
	fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Result<Option<Instant>, Error> {
		self.poll(now, waiter)
	}
}

/// What an origin holds on behalf of its sessions and readers, for the session fuzz target's
/// leak check.
#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Footprint {
	tasks: usize,
	routes: usize,
	cursors: usize,
	listeners: usize,
	withdrawn: usize,
	withheld: usize,
}

#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
impl Driver {
	/// Count what this origin holds; see [`Footprint`].
	pub(crate) fn footprint(&self) -> Footprint {
		let shared = self.state.shared.lock();
		Footprint {
			tasks: self.state.set.len(),
			routes: shared.routes.entries().count(),
			cursors: shared.cursors.len(),
			listeners: shared.listeners.len(),
			withdrawn: shared.withdrawn.len(),
			withheld: *shared.hidden_withheld.0.read(),
		}
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
		// `route` holds across its attach: a concurrent create either
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
			// Sessions stop hearing interests; their queued snapshots release as they drop.
			for listener in &shared.listeners {
				listener.queue.close();
			}
			(servers, cursors, fronts)
		};

		// Reject requesters still parked on a remote front's channel: its watcher
		// was cancelled above and will never resolve them.
		for producer in fronts {
			if let Ok(mut request) = producer.write() {
				request.resolved.get_or_insert(Err(Error::Closed));
			}
		}
		// Reject every pending route request, including those already handed to a
		// handler: the teardown is terminal, so a handler resolving late must not
		// beat it (resolution is first-write-wins).
		for server in servers {
			let released = {
				let mut server = server.lock();
				server.closed = true;
				server.requests.drain_all()
			};
			for producer in released {
				if let Ok(mut request) = producer.write() {
					request.resolved.get_or_insert(Err(Error::Closed));
				}
			}
		}

		// End the announce cursors: each drains its pending updates, then reports
		// the end. Registrations stay (the cursors remove themselves on drop). A
		// snapshot still in flight is released like a landing, owing what is queued.
		for state in cursors {
			if let Ok(mut state) = state.write() {
				state.end();
				if state.waiting.swap(0, Ordering::Relaxed) > 0 {
					state.owe();
				}
			}
		}
	}
}

impl Drop for DriverState {
	fn drop(&mut self) {
		self.teardown();
	}
}

/// Everything [`FrontServeRun`] owns, queued by [`Consumer::request`].
struct FrontTask {
	/// The route table the front selects from.
	shared: SendCell<kio::Shared<OriginState>>,
	/// The broadcast the front serves.
	broadcast: broadcast::Producer,
	/// Absolute path of the front.
	path: Path,
	/// The requesters' horizon, applied to every (re)selection.
	horizon: Horizon,
	/// The instance the front serves from the start, for requests naming another one than
	/// the path's winner; see [`FrontKey`].
	pin: Option<Instance>,
	/// Wakes the front when a route covering its path changes.
	watch: Watch,
	/// Resolves the requesters parked on the front's channel.
	request: kio::Producer<PendingFront>,
	timers: Deadlines,
}

/// A route asked for its copy of a track: the copy, its info once its query
/// resolved, and a subscription with what the readers want.
struct Asked {
	source: u64,
	/// The track instance the query reached, whatever its source serves under the name later.
	copy: track::Consumer,
	/// The front's own demand on the copy, held for as long as the front holds the copy: a
	/// bare handle is no demand, so this stands in for the readers' queries and fetches.
	query: track::Resolving,
	info: Option<track::Info>,
	/// Some sessions only learn a track's info by subscribing (moq-transport's
	/// SUBSCRIBE_OK), and read the demand only then, so the readers' demand rides the
	/// query. Kept once spliced until the last reader leaves, so the copy is never left
	/// with nobody subscribed while readers are still subscribing to it on their own.
	sub: Option<kio::Pending<track::Subscribing>>,
	/// The copy staged for fetches while its info is in flight, withdrawn if it is never
	/// spliced.
	staged: super::resume::Staged,
}

/// The driver's side of one logical track: the handles behind the names the
/// machine uses.
struct TrackIo {
	/// Queues just this track when a resource it watches changes.
	ready: Arc<crate::util::QueueWake<Arc<str>>>,
	park: kio::Park,
	logical: Logical,
	/// Whether anyone reads the logical track.
	weak: track::TrackWeak,
	/// Which copy serves the track, and how it ends; see [`super::resume`].
	routes: super::resume::Producer,
	/// A copy whose info is still in flight.
	query: Option<Asked>,
	/// The copy whose info resolved, waiting for the machine to splice it.
	staged: Option<Asked>,
	/// The source whose copy serves the track, the copy, and the query holding it; plus the
	/// subscription that query made.
	copy: Option<(u64, track::Consumer, track::Resolving)>,
	held: Option<kio::Pending<track::Subscribing>>,
	/// Whether the track had a reader as of the last demand edge.
	wanted: bool,
}

/// The logical track the front answers its readers with.
enum Logical {
	/// Until the first copy says what it is.
	Requested(track::Request),
	/// Accepted with that copy's info. Nothing writes it: its readers read the serving
	/// route's copy through `routes`.
	Accepted(track::Producer),
	/// Finished, aborted, or refused.
	Ended,
}

impl Logical {
	/// Accept a request still waiting, with the first copy's `info`.
	fn accept(&mut self, info: track::Info) {
		if let Self::Requested(_) = self
			&& let Self::Requested(request) = std::mem::replace(self, Self::Ended)
		{
			*self = Self::Accepted(request.accept(info));
		}
	}

	/// The accepted track, whose readers' demand the copy follows.
	fn accepted(&mut self) -> Option<&mut track::Producer> {
		match self {
			Self::Accepted(accepted) => Some(accepted),
			_ => None,
		}
	}
}

impl TrackIo {
	/// Serve the track from `asked`'s copy, accepting it with that copy's info first.
	fn splice(&mut self, asked: Asked) {
		self.ready.wake_by_ref();
		if let Some(info) = asked.info.clone() {
			self.logical.accept(info);
		}
		self.routes.splice(asked.copy.clone(), asked.staged);
		self.copy = Some((asked.source, asked.copy, asked.query));
		self.held = asked.sub;
	}

	/// End the track: cleanly with `Ok`, or failed for good. Readers drain what the
	/// serving copy still holds. A request still waiting is refused with `retry`, the Retry
	/// Interval an upstream refusal asked for, when there is one.
	fn end(&mut self, result: Result<(), Error>, retry: Option<Duration>) {
		self.ready.wake_by_ref();
		self.query = None;
		self.staged = None;
		self.copy = None;
		self.held = None;
		match std::mem::replace(&mut self.logical, Logical::Ended) {
			// Ended before any route served it: nothing to read, nor to fetch.
			Logical::Requested(request) => {
				self.routes.end(result.clone());
				let err = result.err().unwrap_or(Error::NotFound);
				match retry {
					Some(retry) => request.refuse(err, retry),
					None => request.reject(err),
				}
			}
			Logical::Accepted(accepted) => {
				let _ = match &result {
					Ok(()) => accepted.finish().map(drop),
					// A failed track refuses newcomers too, so they ask afresh.
					Err(err) => accepted.abort(err.clone()),
				};
				self.routes.end(result);
			}
			Logical::Ended => self.routes.end(result),
		}
	}
}

/// The front's answer to listings of its broadcast: the serving source's listing,
/// forwarded through the front's [`broadcast::Lister`].
#[derive(Default)]
struct FrontListing {
	/// The source being read.
	source: Option<u64>,
	reader: Option<broadcast::Tracks>,
	/// Every name the front lists.
	listed: HashSet<Arc<str>>,
	/// Names an earlier source listed that the current one has yet to list. A source of the
	/// same instance lists the same tracks, so its readers see no churn: what it still lacks
	/// at its SYNCED is gone.
	stale: HashSet<Arc<str>>,
}

/// A track the ended front left in flight, held until its last reader leaves. It
/// keeps none of the driver's wake state, which would queue its name for a driver
/// that is gone.
struct InFlight {
	weak: track::TrackWeak,
	routes: super::resume::Producer,
	/// Why the front ended, used if its pending copy cannot answer.
	end: Error,
	// Kept for its readers: the logical track, still owed an answer when the front ended
	// before the copy's info arrived, the query holding its copy, and the copy's
	// subscription, which follows their demand as it did while the front served it.
	logical: Logical,
	query: track::Resolving,
	held: Option<kio::Pending<track::Subscribing>>,
}

/// Keep the query's subscription on a copy following the readers' demand, and let it go
/// with the last reader. Left behind, its first preferences would mask a later, lower
/// aggregate from the copy's source: a shorter budget, or a demoted priority. With `copy`,
/// a reader arriving subscribes it too.
fn follow_demand(
	held: &mut Option<kio::Pending<track::Subscribing>>,
	copy: Option<&track::Consumer>,
	accepted: Option<&mut track::Producer>,
	weak: &track::TrackWeak,
	routes: &super::resume::Producer,
	waiter: &kio::Waiter,
) {
	if held.is_none() && copy.is_none() {
		return;
	}
	if let Some(accepted) = accepted {
		while let Poll::Ready(Ok(_)) = accepted.poll_subscription_changed(waiter) {}
	}
	match (weak.subscription(), held.as_mut()) {
		(Some((subscription, subscribers)), Some(hold)) => {
			let _ = hold.update(routes.subscription(subscription, subscribers));
		}
		(Some((subscription, subscribers)), None) => {
			*held = copy.map(|copy| copy.subscribe(routes.subscription(subscription, subscribers)));
		}
		(None, _) => *held = None,
	}
}

impl InFlight {
	/// Let go once its last reader left. Nobody reads the logical track any more, so it ends
	/// as a cancel rather than as a producer dropped unfinished.
	fn release(self) {
		self.routes.release();
		match self.logical {
			Logical::Requested(request) => request.reject(Error::Cancel),
			Logical::Accepted(accepted) => {
				let _ = accepted.abort(Error::Cancel);
			}
			Logical::Ended => {}
		}
	}
}

/// Drives one front until it ends, then holds the tracks it left in flight until
/// their last reader leaves, or until nothing owns the origin: a reader never keeps
/// the driver from finishing.
struct FrontServeRun {
	state: FrontRunState,
	origin: TasksWeak,
}

#[allow(clippy::large_enum_variant, reason = "a machine's enum is its storage")]
enum FrontRunState {
	/// Not polled yet: the front starts serving on its first poll, so a set that drops
	/// it unpolled never starts it.
	Start(FrontTask),
	Serve(FrontServe),
	/// Each keeps its copy for the readers still on their way, then lets go as an unread
	/// track parks, so the copy never keeps its source subscribed for nobody. Unread once
	/// is unread for good: the front closed its broadcast, which refuses every lookup, so
	/// no reader can arrive once the last one left.
	InFlight(Vec<InFlight>),
}

impl FrontServeRun {
	fn new(task: FrontTask, origin: TasksWeak) -> Self {
		Self {
			state: FrontRunState::Start(task),
			origin,
		}
	}
}

impl kio::Task for FrontServeRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let in_flight = loop {
			match &mut self.state {
				FrontRunState::Start(_) => {
					let FrontRunState::Start(task) =
						std::mem::replace(&mut self.state, FrontRunState::InFlight(Vec::new()))
					else {
						unreachable!("matched above");
					};
					self.state = FrontRunState::Serve(FrontServe::new(task));
				}
				FrontRunState::Serve(serve) => {
					let in_flight = ready!(serve.poll(waiter));
					self.state = FrontRunState::InFlight(in_flight);
				}
				FrontRunState::InFlight(in_flight) => break in_flight,
			}
		};
		// Dropped without a release, so a reader on its way keeps the copy; it stays held
		// only while the logical track's state does, for an origin nobody owns.
		if self.origin.poll_orphaned(waiter).is_ready() {
			for io in in_flight.iter_mut() {
				if let Logical::Requested(_) = io.logical
					&& let Logical::Requested(request) = std::mem::replace(&mut io.logical, Logical::Ended)
				{
					request.reject(io.end.clone());
				}
			}
			return Poll::Ready(());
		}
		for mut io in std::mem::take(in_flight) {
			if let Logical::Requested(request) = &mut io.logical {
				while let Poll::Ready(Some(_)) = request.poll_subscription_changed(waiter) {}
				match io.query.poll_ok(waiter) {
					Poll::Ready(Ok(info)) => io.logical.accept(info),
					Poll::Ready(Err(err)) => {
						// No metadata ever arrived: a lost route cannot answer the request.
						let err = match err.retryable() {
							true => io.end.clone(),
							false => err,
						};
						if let Logical::Requested(request) = std::mem::replace(&mut io.logical, Logical::Ended) {
							request.reject(err.clone());
						}
						io.routes.end(Err(err));
						continue;
					}
					Poll::Pending => {}
				}
			}
			follow_demand(&mut io.held, None, io.logical.accepted(), &io.weak, &io.routes, waiter);
			match io.weak.poll_unwanted(waiter) {
				Poll::Pending => in_flight.push(io),
				Poll::Ready(_) => io.release(),
			}
		}
		match in_flight.is_empty() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}
}

/// The route a front may serve from. `resolved` is the instance the front first
/// resolved, once it has. Its routes serve one continuous key space, so any of them
/// at its newest epoch may take over. The front stays until those routes go, even once
/// another instance wins the path: its subscriptions are sticky, and new requests
/// resolve the winner on a fresh front.
fn pick<'a>(
	table: &'a OriginState,
	path: &Path,
	horizon: Horizon,
	front: &Front,
	resolved: Option<&Instance>,
) -> Option<&'a RouteEntry> {
	let excluded = front.excluded_routes();
	match resolved {
		None => table.best_route(path, horizon, excluded, |_| true),
		Some(instance) => table.best_route(path, horizon, excluded, |entry| entry.instance == *instance),
	}
}

/// What [`FrontServe::poll_step`] returns: one thing that happened.
enum FrontStep {
	Assigned(track::Request),
	Resolved(Result<broadcast::Consumer, Error>),
	SourceClosed(u64),
	Info(Arc<str>, u64, Result<track::Info, Error>),
	Ended(Arc<str>, u64, Result<(), Error>, bool),
	Demand(Arc<str>),
	Holders,
	/// The broadcast closed: nobody can hold it again.
	Closed,
	Deadline,
	Table,
	/// Someone lists the broadcast, or the serving source changed: read its listing.
	Listing,
	/// Nobody lists the broadcast any more.
	Unlisted,
	Listed(Result<Option<broadcast::TracksEvent>, Error>),
}

/// A front's in-flight upstream request.
struct FrontUpstream {
	route: u64,
	/// The route's instance when asked.
	asked: Instance,
	/// [`OriginState::next_route`] when asked.
	asked_at: u64,
	pending: kio::Consumer<PendingBroadcast>,
	/// The queue's [`ServeState::renewals`], and its count when asked: a renewal
	/// since released the request, whatever it resolved to and whenever.
	renewals: Arc<AtomicU64>,
	asked_renewals: u64,
}

/// Feeds the world's events to a [`Front`] and performs the actions it returns,
/// until the front ends; resolves to the tracks it ended in flight. The decisions live
/// in the machine; this only waits and executes, so nothing here decides anything
/// twice.
///
/// Declared in drop order: the state the front builds goes before the task's handles it
/// is built from, the newest first.
struct FrontServe {
	/// The current wait's waiter. Each wait gets a fresh one: a reused waiter keeps the
	/// registrations an earlier wait left, which wake the front spuriously.
	park: kio::Park,
	/// The source the broadcast's datagram lookups go to, as last handed over: datagram
	/// tracks are live only, so a lookup joins the serving source's own track rather than a
	/// copy the front would splice.
	datagram_source: Option<u64>,
	/// Once the front picks a route a session received, it asks sessions to keep
	/// discovering its path for as long as it lives, so the route outlives whatever
	/// reader found it, such as a listing that has since closed.
	discovered: Option<Interest>,
	events: VecDeque<Event>,
	/// Whether a consumer held the broadcast as of the last edge fed to the machine,
	/// starting with the request that minted the front.
	held: bool,
	/// What the front first resolved; see [`pick`].
	resolved: Option<Instance>,
	/// The watch generation the last selection saw.
	seen: u64,
	deadline: crate::time::Deadline,
	ready: kio::Queue<Arc<str>>,
	tracks: HashMap<Arc<str>, TrackIo>,
	upstream: Option<FrontUpstream>,
	next_source: u64,
	/// The instance each source's route served when it was asked, and the table's clock
	/// then: what its content is from.
	instances: HashMap<u64, (Instance, u64)>,
	sources: HashMap<u64, broadcast::Consumer>,
	front: Front,
	listing: Option<FrontListing>,
	lister: broadcast::Lister,
	/// The front serves its broadcast on demand: every track a reader names is
	/// handed here, and its readers read it from whichever source serves the path.
	dynamic: broadcast::Requests,
	timers: Deadlines,
	request: kio::Producer<PendingFront>,
	watch: Watch,
	pin: Option<Instance>,
	horizon: Horizon,
	path: Path,
	broadcast: broadcast::Producer,
	shared: SendCell<kio::Shared<OriginState>>,
}

impl FrontServe {
	fn new(task: FrontTask) -> Self {
		let FrontTask {
			shared,
			broadcast,
			path,
			horizon,
			pin,
			watch,
			request,
			timers,
		} = task;
		let dynamic = broadcast.requests();
		let lister = dynamic
			.lister()
			.expect("only the front answers its broadcast's listings");
		let deadline = crate::time::Deadline::new(&timers);
		broadcast.serve_datagrams_from(None);
		let mut serve = Self {
			park: kio::Park::default(),
			datagram_source: None,
			discovered: None,
			events: VecDeque::new(),
			held: true,
			resolved: None,
			seen: 0,
			deadline,
			ready: kio::Queue::new(),
			tracks: HashMap::new(),
			upstream: None,
			next_source: 0,
			instances: HashMap::new(),
			sources: HashMap::new(),
			front: Front::new(track::IDLE_LINGER),
			listing: None,
			lister,
			dynamic,
			timers,
			request,
			watch,
			pin,
			horizon,
			path,
			broadcast,
			shared,
		};
		let event = serve.select();
		serve.events.push_back(event);
		serve
	}

	/// Read the table for the machine: the best route and whether
	/// the serving source is on its way out. Also what the watch wakes for.
	fn select(&mut self) -> Event {
		let mut table = self.shared.lock();
		if table.closed {
			return Event::Closed;
		}
		// Read alongside the decision, under the lock a poke takes first.
		self.seen = self.watch.seen();
		self.front.retain_routes(|route| table.routes.covers(&self.path, route));
		let best = pick(
			&table,
			&self.path,
			self.horizon,
			&self.front,
			self.resolved.as_ref().or(self.pin.as_ref()),
		);
		// Under the table's lock, which a request reads it under too: a request that
		// sees another winner from here on mints its own front rather than join this one.
		if self.resolved.is_none()
			&& let Some(entry) = best
			&& let Ok(mut pending) = self.request.write()
			&& pending.selected != entry.instance
		{
			pending.selected = entry.instance.clone();
		}
		// Only a received route carries a hop chain: a local one needs no discovery.
		let received = best.is_some_and(|entry| !entry.hops.is_empty());
		let best = best.map(|entry| Candidate {
			route: entry.id,
			local: entry.local,
			carried: !entry.instance.is_minted(),
		});
		// No candidate irreversibly ends this front. Leave the cache under the same lock
		// a lookup takes, before a route can return and a newcomer join the broadcast the
		// end actions are about to close. Existing requesters keep their original verdict.
		if best.is_none() {
			table
				.fronts
				.remove_if(&(self.path.clone(), self.horizon, self.pin.clone()), |front| {
					front.request.same_channel(&self.request)
				});
		}
		let serving_closing = self
			.front
			.serving()
			.and_then(|id| self.sources.get(&id))
			.is_some_and(|source| source.is_closing());
		drop(table);
		if received && self.discovered.is_none() {
			let mut table = self.shared.lock();
			self.discovered = Some(Interest::new(&mut table, &self.shared, self.path.clone()));
		}
		Event::Selected { best, serving_closing }
	}

	/// Whether a route's refusal is the answer: only the current winner speaks for the
	/// path. A retraction resolves like a rejection, and a route beaten while its
	/// request was pending speaks for nobody, so either one re-selects instead.
	fn standing(&self, route: u64) -> bool {
		let table = self.shared.read();
		pick(
			&table,
			&self.path,
			self.horizon,
			&self.front,
			self.resolved.as_ref().or(self.pin.as_ref()),
		)
		.is_some_and(|best| best.id == route)
	}

	/// Whether an answer `route` gave for `asked` would serve another instance than the
	/// front's: the route has since moved off it, or, before the front resolved, another
	/// instance won the path while the request was in flight.
	fn stale(&self, route: u64, asked: &Instance) -> bool {
		let table = self.shared.read();
		let Some(entry) = table.routes.covering(&self.path).find(|entry| entry.id == route) else {
			return false;
		};
		if entry.instance != *asked {
			return true;
		}
		match &self.resolved {
			Some(resolved) => resolved != asked,
			None => pick(&table, &self.path, self.horizon, &self.front, self.pin.as_ref())
				.is_some_and(|best| best.instance != *asked),
		}
	}

	/// Wait for the next thing to happen, the one place the front parks.
	fn poll_step(&mut self, waiter: &kio::Waiter) -> Poll<FrontStep> {
		if let Poll::Ready(Ok(request)) = self.dynamic.poll_requested_track(waiter) {
			return Poll::Ready(FrontStep::Assigned(request));
		}
		if let Some(upstream) = &self.upstream
			&& let Poll::Ready(result) = upstream.pending.poll(waiter, |p| match &p.resolved {
				Some(result) => Poll::Ready(result.clone()),
				None => Poll::Pending,
			}) {
			return Poll::Ready(FrontStep::Resolved(match result {
				Ok(resolved) => resolved,
				// The queue died unresolved (its handler dropped): the route
				// could not serve.
				Err(_closed) => Err(Error::Unroutable),
			}));
		}
		// The listing first: a source ends its listing before it closes, so this forwards
		// the tracks it finished last before its close detaches it.
		match &mut self.listing {
			None => {
				if self.front.serving().is_some() && matches!(self.lister.poll_wanted(waiter), Poll::Ready(Ok(()))) {
					return Poll::Ready(FrontStep::Listing);
				}
			}
			Some(state) => {
				if self.lister.poll_unwanted(waiter).is_ready() {
					return Poll::Ready(FrontStep::Unlisted);
				}
				if state.source != self.front.serving() {
					return Poll::Ready(FrontStep::Listing);
				}
				if let Some(reader) = &mut state.reader
					&& let Poll::Ready(result) = reader.poll_next(waiter)
				{
					return Poll::Ready(FrontStep::Listed(result));
				}
			}
		}
		if let Some(id) = self.front.serving()
			&& let Some(source) = self.sources.get(&id)
			&& source.poll_closed(waiter).is_ready()
		{
			return Poll::Ready(FrontStep::SourceClosed(id));
		}
		while let Poll::Ready(Ok(name)) = self.ready.poll_pop(waiter) {
			let Some(io) = self.tracks.get_mut(&name) else { continue };
			io.ready.popped();
			let waker = Waker::from(io.ready.clone());
			let waiter = io.park.hold(&Context::from_waker(&waker));
			let step = (|| {
				if let Some(asked) = &mut io.query {
					if asked.info.is_none()
						&& let Poll::Ready(result) = asked.query.poll(waiter)
					{
						match result {
							Ok(info) => asked.info = Some(info),
							Err(err) => return Some(FrontStep::Info(name.clone(), asked.source, Err(err))),
						}
					}
					// An awaited copy follows the readers, or it may never send the group
					// they now want.
					if asked.info.is_some() {
						follow_demand(
							&mut asked.sub,
							Some(&asked.copy),
							io.logical.accepted(),
							&io.weak,
							&io.routes,
							waiter,
						);
					}
					// While another copy may still deliver, a matching replacement answers only
					// once it delivers or ends: one failing first refuses, and that copy keeps
					// serving. Different metadata is refused at once.
					if let Some(info) = &asked.info
						&& (io
							.copy
							.as_ref()
							.is_none_or(|(_, copy, _)| copy.poll_complete(waiter).is_ready())
							|| matches!(&io.logical, Logical::Accepted(accepted) if accepted.info() != info)
							|| asked.copy.poll_grouped(waiter).is_ready()
							|| asked.copy.poll_complete(waiter).is_ready())
					{
						return Some(FrontStep::Info(name.clone(), asked.source, Ok(info.clone())));
					}
				}
				follow_demand(&mut io.held, None, io.logical.accepted(), &io.weak, &io.routes, waiter);
				// Settled once the copy closes: a group still open below a declared end is
				// owed until then, and a session closing first means it never came.
				if let Some((source, copy, _)) = &io.copy
					&& copy.poll_closed(waiter).is_ready()
				{
					let result = match copy.poll_complete(&kio::Waiter::noop()) {
						Poll::Ready(result) => result,
						Poll::Pending => {
							// A channel cannot reopen after poll_closed reported its end.
							debug_assert!(false, "closed copy did not report completion");
							Err(Error::Cancel)
						}
					};
					let delivered = copy.latest_key().is_some();
					return Some(FrontStep::Ended(name.clone(), *source, result, delivered));
				}
				// Watch the demand edge in whichever direction is unmet. Only `Pending`
				// parks, so an answer steps and the handler looks again, polling anew when
				// nothing changed. A closed track is unread: it owes one `Unwanted` if
				// recorded read, and is no edge after.
				let edge = match io.wanted {
					true => io.weak.poll_unwanted(waiter).is_ready(),
					false => matches!(io.weak.poll_wanted(waiter), Poll::Ready(Ok(()))),
				};
				if edge {
					return Some(FrontStep::Demand(name.clone()));
				}
				None
			})();
			if let Some(step) = step {
				// A ready answer did not park on its next edge. Revisit this track
				// once the machine has consumed the event and applied its actions.
				io.ready.wake_by_ref();
				return Poll::Ready(step);
			}
		}
		// The same for whoever holds the broadcast itself.
		let edge = match self.held {
			true => self.broadcast.poll_unheld(waiter),
			false => self.broadcast.poll_held(waiter),
		};
		match edge {
			Poll::Ready(Ok(())) => return Poll::Ready(FrontStep::Holders),
			// Answers at once from here on, so it must end the front, not be polled again.
			Poll::Ready(Err(_)) => return Poll::Ready(FrontStep::Closed),
			Poll::Pending => {}
		}
		if self.deadline.poll(waiter).is_ready() {
			return Poll::Ready(FrontStep::Deadline);
		}
		self.watch.poll_changed(waiter, self.seen).map(|()| FrontStep::Table)
	}
}

impl kio::Task for FrontServe {
	type Output = Vec<InFlight>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Vec<InFlight>> {
		loop {
			while let Some(event) = self.events.pop_front() {
				let refused = matches!(
					&event,
					Event::Resolved {
						result: Err(Refusal { standing: true, .. }),
						..
					}
				);
				let answered = match &event {
					Event::TrackInfo { track, source, .. } => Some((track.clone(), *source)),
					_ => None,
				};
				for action in self.front.step(event) {
					match action {
						Action::Reselect => {
							let event = self.select();
							self.events.push_back(event);
						}
						Action::Request { route } => {
							// The entry and what it serves.
							let found = {
								let table = self.shared.read();
								table
									.routes
									.covering(&self.path)
									.find(|entry| entry.id == route && entry.live())
									.map(|entry| {
										let instance = (entry.instance.clone(), table.next_route);
										(entry.source.clone(), entry.server.clone(), instance)
									})
							};
							let Some((source, server, instance)) = found else {
								self.events.push_back(Event::Resolved {
									route,
									result: Err(Refusal {
										err: Error::Unroutable,
										standing: false,
									}),
								});
								continue;
							};
							if let Some(source) = source {
								let id = self.next_source;
								self.next_source += 1;
								self.sources.insert(id, source);
								self.instances.insert(id, instance);
								self.events.push_back(Event::Resolved { route, result: Ok(id) });
								continue;
							}
							let Some(server) = server else {
								self.events.push_back(Event::Resolved {
									route,
									result: Err(Refusal {
										err: Error::Unroutable,
										standing: self.standing(route),
									}),
								});
								continue;
							};
							let mut serve = server.lock();
							if serve.closed {
								// Retracted under us, or its handler dropped while the
								// announcement stands: it cannot serve. A retraction
								// leaves the table before it closes the server, so the
								// table tells the two apart.
								drop(serve);
								self.events.push_back(Event::Resolved {
									route,
									result: Err(Refusal {
										err: Error::Unroutable,
										standing: self.standing(route),
									}),
								});
								continue;
							}
							// A source this route already materialized for the path
							// attaches without another upstream round trip. Its session
							// retires a source nothing holds, atomically with this mint, so
							// one found closed is gone and the route is asked afresh.
							if let Some(source) = serve
								.served
								.get(&self.path)
								.map(|weak| weak.consume())
								.filter(|source| !source.is_closed())
							{
								drop(serve);
								let id = self.next_source;
								self.next_source += 1;
								self.sources.insert(id, source);
								self.instances.insert(id, instance);
								self.events.push_back(Event::Resolved { route, result: Ok(id) });
								continue;
							}
							let pending = match serve.requests.join(&self.path) {
								Some(producer) => producer.consume(),
								None => {
									let producer = kio::Producer::<PendingBroadcast>::default();
									let consumer = producer.consume();
									match serve.requests.insert(self.path.clone(), producer) {
										Ok(()) => consumer,
										// No live handler behind the route: it cannot
										// serve, whatever the table says.
										Err(_) => {
											drop(serve);
											self.events.push_back(Event::Resolved {
												route,
												result: Err(Refusal {
													err: Error::Unroutable,
													standing: self.standing(route),
												}),
											});
											continue;
										}
									}
								}
							};
							self.upstream = Some(FrontUpstream {
								route,
								asked: instance.0,
								asked_at: instance.1,
								pending,
								renewals: serve.renewals.clone(),
								asked_renewals: serve.renewals.load(Ordering::Acquire),
							});
						}
						Action::Detach { source, drain } => {
							self.sources.remove(&source);
							self.instances.remove(&source);
							for io in self.tracks.values_mut() {
								if !drain && io.query.as_ref().is_some_and(|asked| asked.source == source) {
									io.query = None;
								}
								if !drain && io.staged.as_ref().is_some_and(|asked| asked.source == source) {
									io.staged = None;
								}
								// Any source leaving is rare: drop every parked shortcut rather
								// than track which source each came from.
								io.routes.unpark();
							}
						}
						Action::Resolve => {
							// The broadcast is whatever its first source serves, from the instance
							// its route had when asked.
							let Some(source) = self.front.serving() else {
								continue;
							};
							let Some((instance, _)) = self.instances.get(&source).cloned() else {
								continue;
							};
							// Route datagram lookups before a requester can see the broadcast,
							// which would otherwise find no source to ask.
							if self.datagram_source != Some(source) {
								self.datagram_source = Some(source);
								self.broadcast.serve_datagrams_from(self.sources.get(&source).cloned());
							}
							self.resolved = Some(instance.clone());
							if let Ok(mut pending) = self.request.write() {
								pending.resolved.get_or_insert(Ok(instance));
							}
						}
						Action::Query { track: name, source } => {
							let Some(io) = self.tracks.get_mut(&name) else { continue };
							io.ready.wake_by_ref();
							let closing = self.sources.get(&source).is_some_and(|s| s.is_closing());
							match self.sources.get(&source).map(|s| s.track(&name)) {
								Some(Ok(lookup)) => {
									// Subscribe before the query exposes upstream demand, and query
									// that same instance even if the broadcast changes meanwhile.
									let sub = io.weak.subscription().map(|(demand, subscribers)| {
										lookup.subscribe(io.routes.subscription(demand, subscribers))
									});
									let query = match &sub {
										Some(sub) => sub.info(),
										None => lookup.info().into_inner(),
									};
									let copy = query.track();
									io.query = Some(Asked {
										source,
										staged: io.routes.stage(copy.clone()),
										copy,
										query,
										info: None,
										sub,
									});
								}
								Some(Err(err)) => self.events.push_back(Event::TrackInfo {
									track: name,
									source,
									closing,
									result: Err(err),
									retry: None,
								}),
								None => {}
							}
						}
						Action::Splice { track: name, source } => {
							let Some(io) = self.tracks.get_mut(&name) else { continue };
							let Some(asked) = io.staged.take() else {
								continue;
							};
							if asked.source != source {
								continue;
							}
							io.splice(asked);
						}
						Action::Park { track: name } => {
							let Some(io) = self.tracks.get_mut(&name) else { continue };
							io.ready.wake_by_ref();
							// Drop the copy so its source goes idle at once. A returning
							// reader gets a fresh one, so nothing cached can be stale.
							io.routes.park();
							io.query = None;
							io.staged = None;
							io.copy = None;
							io.held = None;
						}
						Action::Forget { track: name } => {
							// A reader that looked the track up since the machine decided keeps
							// it. Feed its `Wanted` edge here: the demand poll only sees the
							// current level, so a reader gone before the next poll would
							// otherwise leave the track unread with no linger armed.
							if let Some(io) = self.tracks.get_mut(&name)
								&& !io.weak.abort_unwanted(Error::Cancel)
							{
								if !io.wanted {
									io.wanted = true;
									self.events.push_back(Event::Wanted { track: name });
								}
								continue;
							}
							// Nobody reads it, so nobody follows its copy: let the route go idle.
							if let Some(io) = self.tracks.remove(&name) {
								io.routes.park();
							}
							self.events.push_back(Event::Forgotten { track: name });
						}
						Action::Finish { track: name } => {
							if let Some(io) = self.tracks.get_mut(&name) {
								io.end(Ok(()), None);
							}
						}
						Action::Abort {
							track: name,
							err,
							retry,
						} => {
							if let Some(io) = self.tracks.get_mut(&name) {
								tracing::debug!(name = %name, %err, "aborting track");
								io.end(Err(err), retry);
							}
						}
						Action::Arm { at } => self.deadline.set(at),
						Action::Retire => {
							// Closing refuses every holder from here on, atomically with minting
							// one: a request that joined first keeps the front, and one after
							// finds it closed and mints a fresh one.
							let event = match self.broadcast.close_unheld() {
								true => Event::Retired,
								false => {
									self.held = true;
									Event::Held
								}
							};
							self.events.push_back(event);
						}
						Action::End { err } => {
							// Leave the table before anything shows the end, whatever ended the
							// front: a requester seeing it re-requests, and must mint a fresh
							// front rather than join this one again. A successor already in the
							// slot stays.
							self.shared
								.lock()
								.fronts
								.remove_if(&(self.path.clone(), self.horizon, self.pin.clone()), |front| {
									front.request.same_channel(&self.request)
								});
							if let Ok(mut pending) = self.request.write() {
								pending.refused = refused;
								pending.resolved.get_or_insert(Err(err.clone()));
							}
							// Ending the broadcast only retracts it: no new requesters or
							// tracks, and a newcomer at the path gets a fresh front. Tracks
							// in flight carry on (moq-lite: retraction does not disturb
							// subscriptions already in flight): their readers follow the copy
							// they read to its end, since no front is left to replace it.
							self.broadcast.close();
							let mut in_flight = Vec::new();
							for (name, mut io) in self.tracks.drain() {
								let wanted = io.weak.is_wanted();
								let err = self.front.lost(&name).unwrap_or_else(|| err.clone());
								// A reader still waiting on its source's answer is in flight
								// too: serve it the copy it asked, so it ends as that copy does.
								let waiting = io.staged.take().or_else(|| io.query.take());
								if io.copy.is_none()
									&& wanted && let Some(asked) = waiting
								{
									io.splice(asked);
								}
								let (_, _, query) = match io.copy.take() {
									Some(copy) if wanted => copy,
									// Nothing in flight: unread, or nothing serving it.
									copy => {
										io.copy = copy;
										// A track that already ended keeps its copy, which subscribes
										// nothing any more, so a fetch still reads what it cached.
										let ended = io.routes.ended();
										io.end(Err(err.clone()), None);
										// An unread track the front has not parked yet still holds
										// its copy, which would keep its source subscribed.
										if !wanted && !ended {
											io.routes.release();
										}
										continue;
									}
								};
								io.routes.conclude();
								let TrackIo {
									weak,
									routes,
									logical,
									held,
									..
								} = io;
								in_flight.push(InFlight {
									weak,
									routes,
									end: err.clone(),
									logical,
									query,
									held,
								});
							}
							return Poll::Ready(in_flight);
						}
					}
				}
				// A staged copy the machine did not splice was refused or is no longer wanted:
				// let it go, or its source stays subscribed for nobody.
				if let Some((name, source)) = answered
					&& let Some(io) = self.tracks.get_mut(&name)
					&& io.staged.as_ref().is_some_and(|asked| asked.source == source)
				{
					io.staged = None;
				}
			}

			if self.front.serving() != self.datagram_source {
				self.datagram_source = self.front.serving();
				self.broadcast
					.serve_datagrams_from(self.datagram_source.and_then(|id| self.sources.get(&id)).cloned());
			}

			let mut park = std::mem::take(&mut self.park);
			let Poll::Ready(step) = self.poll_step(park.hold(&waiter.context())) else {
				self.park = park;
				return Poll::Pending;
			};
			// Before the step runs, so nothing it touches wakes the front for a wait that
			// is over.
			drop(park);

			let event = match step {
				FrontStep::Assigned(request) => {
					let name: Arc<str> = request.name().into();
					let track_ready = crate::util::QueueWake::new(name.clone(), self.ready.clone());
					track_ready.wake_by_ref();
					let routes = super::resume::Producer::new();
					self.tracks.insert(
						name.clone(),
						TrackIo {
							ready: track_ready,
							park: Default::default(),
							weak: request.weak(),
							logical: Logical::Requested(request.routes(routes.consume())),
							routes,
							query: None,
							staged: None,
							copy: None,
							held: None,
							wanted: false,
						},
					);
					Event::TrackAssigned {
						track: name,
						now: self.timers.now(),
					}
				}
				FrontStep::Resolved(result) => {
					let Some(FrontUpstream {
						route,
						asked,
						asked_at,
						renewals,
						asked_renewals,
						..
					}) = self.upstream.take()
					else {
						continue;
					};
					// Counted rather than read off the request, which an answer just before
					// the change already took out of the queue, and rather than compared by
					// instance, which may have come back since.
					let released = renewals.load(Ordering::Acquire) != asked_renewals;
					match result {
						// An instance change released the request, the route moved to another
						// instance, or another won the path, while the request was in flight:
						// ask again rather than resolve requesters onto a broadcast that is
						// already replaced.
						_ if released => Event::Resolved {
							route,
							result: Err(Refusal {
								err: Error::Unroutable,
								standing: false,
							}),
						},
						Ok(_) if self.stale(route, &asked) => Event::Resolved {
							route,
							result: Err(Refusal {
								err: Error::Unroutable,
								standing: false,
							}),
						},
						Ok(source) => {
							let id = self.next_source;
							self.next_source += 1;
							self.sources.insert(id, source);
							self.instances.insert(id, (asked, asked_at));
							Event::Resolved { route, result: Ok(id) }
						}
						Err(err) => Event::Resolved {
							route,
							result: Err(Refusal {
								err,
								standing: self.standing(route),
							}),
						},
					}
				}
				FrontStep::SourceClosed(source) => Event::SourceClosed { source },
				FrontStep::Info(name, source, result) => {
					let closing = self.sources.get(&source).is_some_and(|s| s.is_closing());
					let Some(io) = self.tracks.get_mut(&name) else { continue };
					let Some(asked) = io.query.take() else { continue };
					let retry = asked.query.retry();
					// An aborted copy with history still splices, so its readers drain that
					// history before the outcome, as local readers do. One that delivered
					// nothing is a refusal, which leaves a draining incumbent serving.
					let result = match result {
						Ok(info) => match asked.copy.poll_complete(&kio::Waiter::noop()) {
							Poll::Ready(Err(err)) if asked.copy.latest_key().is_none() => Err(err),
							_ => Ok(info),
						},
						Err(err) => Err(err),
					};
					// Staged only while the track has a reader: without one the machine
					// will not splice, and a held copy would keep the source subscribed.
					if let Ok(info) = &result
						&& io.wanted
					{
						io.staged = Some(Asked {
							info: Some(info.clone()),
							..asked
						});
					}
					Event::TrackInfo {
						track: name,
						source,
						closing,
						retry: result.as_ref().err().and(retry),
						result,
					}
				}
				FrontStep::Ended(name, source, result, delivered) => {
					let closing = self.sources.get(&source).is_some_and(|s| s.is_closing());
					let Some(io) = self.tracks.get_mut(&name) else { continue };
					io.copy = None;
					io.held = None;
					Event::TrackEnded {
						track: name,
						source,
						closing,
						result,
						delivered,
					}
				}
				FrontStep::Demand(name) => {
					let Some(io) = self.tracks.get_mut(&name) else { continue };
					// Read again: the edge may have flipped back since it fired, and the next
					// wait parks on it.
					if io.weak.is_wanted() == io.wanted {
						continue;
					}
					io.wanted = !io.wanted;
					if !io.wanted {
						// Nothing will be spliced now: let go of the copies a query
						// holds, or the source stays subscribed with nobody reading.
						io.query = None;
						io.staged = None;
					}
					match io.wanted {
						true => Event::Wanted { track: name },
						false => Event::Unwanted {
							track: name,
							now: self.timers.now(),
						},
					}
				}
				FrontStep::Holders => {
					// Read again: the edge may have flipped back since it fired.
					if self.broadcast.is_held() == self.held {
						continue;
					}
					self.held = !self.held;
					match self.held {
						true => Event::Held,
						false => Event::Unheld,
					}
				}
				FrontStep::Closed => Event::Retired,
				FrontStep::Deadline => {
					// Cleared here so a fired deadline cannot keep firing; the machine
					// re-arms what is still parked.
					self.deadline.set(None);
					Event::Deadline { now: self.timers.now() }
				}
				FrontStep::Table => self.select(),
				FrontStep::Listing => {
					// A route claiming a prefix names no broadcast, so a path it serves on request
					// has no inventory to list.
					let covering = self.front.serving_route().is_some_and(|route| {
						self.shared
							.read()
							.routes
							.covering(&self.path)
							.find(|entry| entry.id == route)
							.is_some_and(|entry| entry.prefix != self.path)
					});
					if covering {
						self.listing = None;
						self.lister.abort(Error::NotFound);
						continue;
					}
					let state = self.listing.get_or_insert_default();
					state.source = self.front.serving();
					state.stale = state.listed.clone();
					// A closing source lists nothing more; the front moves off it soon.
					state.reader = state
						.source
						.and_then(|id| self.sources.get(&id))
						.and_then(|source| source.tracks().ok());
					continue;
				}
				FrontStep::Unlisted => {
					self.listing = None;
					self.lister.clear();
					continue;
				}
				FrontStep::Listed(result) => {
					let Some(state) = &mut self.listing else { continue };
					// The lister refuses only once the broadcast ended, which ends the front too.
					match result {
						Ok(Some(broadcast::TracksEvent::Added { name, info })) => {
							if !state.stale.remove(&name) && state.listed.insert(name.clone()) {
								let _ = self.lister.add(name, info);
							}
						}
						// Forwarded as the source reported it, finished or not.
						Ok(Some(broadcast::TracksEvent::Removed { name })) => {
							state.stale.remove(&name);
							if state.listed.remove(&name) {
								let _ = self.lister.remove(&name);
							}
						}
						Ok(Some(broadcast::TracksEvent::Finished { name })) => {
							state.stale.remove(&name);
							if state.listed.remove(&name) {
								let _ = self.lister.finish(&name);
							}
						}
						Ok(Some(broadcast::TracksEvent::Synced)) => {
							for name in state.stale.drain() {
								state.listed.remove(&name);
								let _ = self.lister.remove(&name);
							}
							self.lister.synced();
						}
						// The source ended: the front moves to another source, or ends with it.
						Ok(None)
							if state
								.source
								.and_then(|id| self.sources.get(&id))
								.is_none_or(|source| source.is_closing()) =>
						{
							state.reader = None
						}
						// The source's handler ended the listing on a live source, as a peer does
						// once its broadcast ended: so does the front's, and a later reader asks again.
						Ok(None) => {
							self.listing = None;
							self.lister.end();
						}
						Err(err) => {
							// Its readers fail, and a later reader asks again.
							self.listing = None;
							self.lister.abort(err);
						}
					}
					continue;
				}
			};
			self.events.push_back(event);
		}
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
	watches: Vec<(u64, WatchProducer)>,
	/// Watches at this node or below, so a route change walks only the subtrees
	/// holding one.
	watches_below: usize,
	/// Snapshots in flight at exactly this prefix; see [`Snapshot`].
	snapshots: Vec<SnapshotEntry>,
	/// Snapshots at this node or below, so a cursor registering walks only the
	/// subtrees holding one.
	snapshots_below: usize,
	/// Readers asking sessions to discover routes at or beneath exactly this
	/// prefix; see [`Interest`].
	interests: usize,
	/// Interests at this node or below, so finding the narrower ones a change
	/// covers or uncovers walks only the subtrees holding one.
	interests_below: usize,
	children: HashMap<String, RouteNode>,
}

/// What a [`Watch`] observes: bumped by every change to a route covering its
/// path (a broadcast published here is one) and by the origin's teardown.
#[derive(Default)]
struct Watched {
	generation: u64,
}

enum WatchProducer {
	Changes(kio::Producer<Watched>),
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	Selection(kio::Producer<Selected>),
}

struct Selected {
	path: Path,
	via: Hop,
	instance: Instance,
	/// The selection period, closed when the peer stops selecting the instance.
	current: Option<kio::Producer<()>>,
	/// Open until another instance of the peer takes the path, which may answer a
	/// request still unanswered. A withdrawal alone leaves it open, and a period the
	/// instance wins again keeps it.
	answering: Option<kio::Producer<()>>,
}

/// How a [`Selected`] period changes.
enum Change {
	/// A new period: nothing refused in the last one revives.
	Start,
	/// The peer no longer selects the instance.
	End,
	/// Another instance of the peer took the path.
	Replace,
}

impl Selected {
	/// How the period changes now that the peer's best route is `best`, if it does.
	fn change(&self, best: Option<&RouteEntry>) -> Option<Change> {
		let Some(best) = best else {
			self.current.as_ref()?;
			return Some(Change::End);
		};
		if best.instance == self.instance {
			return self.current.is_none().then_some(Change::Start);
		}
		if self.current.is_none() {
			// Withdrawn earlier: the instance taking the path now may answer what is left.
			return self.answering.is_some().then_some(Change::Replace);
		}
		Some(Change::Replace)
	}

	fn apply(&mut self, change: Change) {
		match change {
			Change::Start => {
				if self.answering.is_none() {
					self.answering = Some(kio::Producer::default());
				}
				self.current = Some(kio::Producer::default());
			}
			Change::End => self.current = None,
			Change::Replace => {
				self.current = None;
				self.answering = None;
			}
		}
	}
}

/// The lifetime of path requests while this peer still selects the held instance.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) struct Selection {
	shared: SendCell<kio::Shared<OriginState>>,
	path: Path,
	id: u64,
	signal: kio::Consumer<Selected>,
}

impl Selection {
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn current(&self) -> kio::Consumer<()> {
		self.signal
			.read()
			.current
			.as_ref()
			.map(kio::Producer::consume)
			.unwrap_or_else(|| kio::Producer::new(()).consume())
	}

	/// Closed once another instance takes the path, for a request already sent.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn answering(&self) -> kio::Consumer<()> {
		self.signal
			.read()
			.answering
			.as_ref()
			.map(kio::Producer::consume)
			.unwrap_or_else(|| kio::Producer::new(()).consume())
	}
}

impl Drop for Selection {
	fn drop(&mut self) {
		self.shared.lock().routes.remove_watch(&self.path, self.id);
	}
}

/// A registration in the route table for changes to the routes covering one
/// path. The table pokes it; the holder waits on it, so an announcement wakes
/// only the fronts and requesters it can affect rather than every one of them.
/// Dropping it unregisters, which takes the table lock: never drop one while
/// holding it.
struct Watch {
	shared: SendCell<kio::Shared<OriginState>>,
	path: Path,
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

/// A snapshot in flight, as the route table holds it.
struct SnapshotEntry {
	id: u64,
	/// The link its routes arrive on, so a horizon hiding them never waits on it.
	link: Link,
	/// The prefix's [`prefix_claim`], so a cursor whose patterns miss it never waits on it.
	claim: Pattern,
	/// The absolute prefix, so a cursor that could see nothing beneath it never waits on it.
	prefix: Path,
}

/// A session still delivering its peer's initial announcements under a prefix.
///
/// Every announce consumer whose scope the prefix overlaps is unsynced
/// (see [`Routes::poll_synced`]) until this drops: once the session
/// has inserted the announcements its peer counted, or it gave up on them.
pub(crate) struct Snapshot {
	shared: Option<SendCell<kio::Shared<OriginState>>>,
	prefix: Path,
	id: u64,
}

impl Snapshot {
	/// A snapshot nothing waits on.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn landed() -> Self {
		Self {
			shared: None,
			prefix: Path::default(),
			id: 0,
		}
	}
}

impl Drop for Snapshot {
	fn drop(&mut self) {
		if let Some(shared) = self.shared.take() {
			shared.lock().remove_snapshot(&self.prefix, self.id);
		}
	}
}

/// Counts an absolute prefix as one readers want discovered until dropped: a
/// session fed into the origin asks its peer about the broadest such prefixes.
/// Dropping it takes the table lock: never drop one while holding it.
struct Interest {
	shared: SendCell<kio::Shared<OriginState>>,
	prefix: Path,
}

impl Interest {
	/// Count `prefix` under the held table lock.
	fn new(state: &mut OriginState, shared: &SendCell<kio::Shared<OriginState>>, prefix: Path) -> Self {
		state.add_interest(shared, &prefix);
		Self {
			shared: shared.clone(),
			prefix,
		}
	}
}

impl Drop for Interest {
	fn drop(&mut self) {
		self.shared.lock().remove_interest(&self.shared, &self.prefix);
	}
}

/// A session registered for [`Interests`], as the table holds it.
struct Listener {
	id: u64,
	/// The session's root, absolute: what it asks about is named relative to it.
	root: Path,
	/// The broadest prefixes the session may ask about, relative to `root`.
	heads: Vec<Path>,
	/// The absolute patterns the session may publish under.
	allowed: Patterns,
	/// The link the session's routes arrive on, which its snapshots wait for.
	link: Link,
	queue: kio::Queue<InterestChange>,
}

impl Listener {
	/// What the session asks about, relative to its root, for an interest at the
	/// absolute `prefix`: each head an interest above it covers, since the scope
	/// may ask for no more, or the prefix itself inside a head.
	fn asks(&self, prefix: &Path) -> Vec<Path> {
		let Some(relative) = prefix.strip_prefix(&self.root) else {
			return match self.root.has_prefix(prefix) {
				true => self.heads.clone(),
				false => Vec::new(),
			};
		};
		let mut out = Vec::new();
		for head in &self.heads {
			if head.has_prefix(&relative) {
				out.push(head.clone());
			} else if relative.has_prefix(head) {
				let reaches = Pattern::subtree(prefix.as_str()).is_ok_and(|subtree| self.allowed.overlaps(&subtree));
				return match reaches {
					true => vec![relative],
					false => Vec::new(),
				};
			}
		}
		out
	}
}

/// What a session asks its peer about, as it changes: the broadest prefixes
/// readers of the origin want discovered, mapped onto the session's scope.
pub(crate) struct Interests {
	shared: SendCell<kio::Shared<OriginState>>,
	id: u64,
	queue: kio::Queue<InterestChange>,
}

impl Interests {
	/// The next change, or `None` once the origin closed.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Option<InterestChange>> {
		Poll::Ready(ready!(self.queue.poll_pop(waiter)).ok())
	}
}

impl Drop for Interests {
	fn drop(&mut self) {
		// Only the table's copy of the queue goes here; the changes still queued drop with
		// this handle's, after the lock is released, since each may hold a snapshot.
		self.shared.lock().listeners.retain(|listener| listener.id != self.id);
	}
}

/// One change to what a session asks its peer about; see [`Interests`].
#[cfg_attr(
	not(feature = "async"),
	expect(dead_code, reason = "only async sessions consume discovery changes")
)]
pub(crate) struct InterestChange {
	/// The prefix, relative to the session's root.
	pub prefix: Path,
	/// Whether readers now want the prefix discovered, or no longer do.
	pub wanted: bool,
	/// Set when wanted: holds the readers under the prefix short of synced from
	/// the moment they asked, until the request the session opens for it
	/// delivers the peer's initial set, or the session drops it unasked.
	pub snapshot: Option<Snapshot>,
}

/// What a registration adds to the subtree counts on its walk.
#[derive(Clone, Copy)]
struct Below {
	cursors: usize,
	watches: usize,
	snapshots: usize,
	interests: usize,
}

impl Below {
	const NONE: Self = Self {
		cursors: 0,
		watches: 0,
		snapshots: 0,
		interests: 0,
	};
	const CURSOR: Self = Self {
		cursors: 1,
		..Self::NONE
	};
	const WATCH: Self = Self {
		watches: 1,
		..Self::NONE
	};
	const SNAPSHOT: Self = Self {
		snapshots: 1,
		..Self::NONE
	};
	const INTEREST: Self = Self {
		interests: 1,
		..Self::NONE
	};
}

impl RouteNode {
	/// Nothing here and nothing below: the node can be pruned.
	fn is_empty(&self) -> bool {
		self.entries.is_empty()
			&& self.cursors.is_empty()
			&& self.watches.is_empty()
			&& self.snapshots.is_empty()
			&& self.interests == 0
			&& self.children.is_empty()
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
		self.snapshots_below += below.snapshots;
		self.interests_below += below.interests;
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
		self.snapshots_below -= below.snapshots;
		self.interests_below -= below.interests;
		Some(result)
	}

	/// Wake route watchers, and revoke only leases whose selected instance changed.
	fn poke(&self, routes: Option<&RouteTable>) {
		for (_, watch) in &self.watches {
			match watch {
				WatchProducer::Changes(watch) => {
					if let Ok(mut watched) = watch.write() {
						watched.generation += 1;
					}
				}
				WatchProducer::Selection(watch) => {
					if let Ok(mut selected) = watch.write() {
						let change = match routes {
							Some(routes) => selected.change(routes.best_route(
								&selected.path,
								Horizon::default(),
								&HashSet::new(),
								|entry| entry.via == selected.via,
							)),
							None => selected.current.is_some().then_some(Change::End),
						};
						// Only a change wakes the requests waiting on the selection.
						if let Some(change) = change {
							selected.apply(change);
						}
					}
				}
			}
		}
	}

	/// Wake only the watched subtrees covered by this route.
	fn poke_below(&self, routes: &RouteTable) {
		if self.watches_below == 0 {
			return;
		}
		self.poke(Some(routes));
		for child in self.children.values() {
			child.poke_below(routes);
		}
	}

	/// Visit this node and everything below it.
	fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Self)) {
		visit(self);
		for child in self.children.values() {
			child.walk(visit);
		}
	}

	/// Collect the snapshots at this node and below, skipping subtrees with none.
	fn collect_snapshots<'a>(&'a self, out: &mut Vec<&'a SnapshotEntry>) {
		if self.snapshots_below == 0 {
			return;
		}
		out.extend(&self.snapshots);
		for child in self.children.values() {
			child.collect_snapshots(out);
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

	/// Collect the broadest interests strictly beneath this node, which sits at
	/// `at`: each one no other interest beneath this node covers.
	fn collect_interests(&self, at: &Path, out: &mut Vec<Path>) {
		for (part, child) in &self.children {
			if child.interests_below == 0 {
				continue;
			}
			let path = at.rejoin(&Path::new(part).expect("a segment of a path fits"));
			match child.interests > 0 {
				true => out.push(path),
				false => child.collect_interests(&path, out),
			}
		}
	}
}

impl RouteTable {
	fn best_route(
		&self,
		path: &Path,
		horizon: Horizon,
		excluded: &HashSet<u64>,
		only: impl Fn(&RouteEntry) -> bool,
	) -> Option<&RouteEntry> {
		// Covering prefixes of one path form a chain, so the deepest node with a
		// candidate holds the unique longest prefix; walking down, the last such
		// node decides.
		let (above, at) = self.split(path);
		let mut best = None;
		for node in above.into_iter().chain(at) {
			let mut candidates = node
				.entries
				.iter()
				.filter(|entry| entry.live())
				.filter(|entry| entry.authorizes(path))
				.filter(|entry| horizon.admits(entry))
				.filter(|entry| !excluded.contains(&entry.id))
				.filter(|entry| only(entry))
				.peekable();
			if candidates.peek().is_some() {
				let epochs = Epochs::of(candidates.clone());
				best = candidates
					.clone()
					.filter(|entry| entry.serves(path))
					.filter(|entry| !epochs.superseded(entry))
					.min_by_key(|entry| route_order(path, entry));
			}
		}
		best
	}

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
	fn covering(&self, path: &Path) -> impl Iterator<Item = &RouteEntry> + Clone {
		// Walked lazily rather than through [`Self::split`], which allocates: the
		// announce sync asks this per cursor presenting a prefix as its root.
		let mut parts = path.parts();
		let mut next = Some(&self.root);
		std::iter::from_fn(move || {
			let node = next?;
			next = parts.next().and_then(|part| node.children.get(part));
			Some(node)
		})
		.flat_map(|node| node.entries.iter())
	}

	/// Whether the live route `id` still covers `path`.
	fn covers(&self, path: &Path, id: u64) -> bool {
		self.covering(path).any(|entry| entry.id == id && entry.live())
	}

	/// The routes announced exactly at `prefix`.
	fn at(&self, prefix: &Path) -> impl Iterator<Item = &RouteEntry> + Clone {
		self.root
			.find(prefix.parts())
			.into_iter()
			.flat_map(|node| node.entries.iter())
	}

	/// The routes announced exactly at `prefix`, for a change in place.
	fn at_mut(&mut self, prefix: &Path) -> impl Iterator<Item = &mut RouteEntry> {
		let mut node = Some(&mut self.root);
		for part in prefix.parts() {
			node = node.and_then(|node| node.children.get_mut(part));
		}
		node.into_iter().flat_map(|node| node.entries.iter_mut())
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

	/// The route `id` announced at `prefix`.
	fn entry(&self, prefix: &Path, id: u64) -> Option<&RouteEntry> {
		let mut node = &self.root;
		for part in prefix.parts() {
			node = node.children.get(part)?;
		}
		node.entries.iter().find(|entry| entry.id == id)
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
		self.root
			.reach(path.parts(), Below::WATCH)
			.watches
			.push((id, WatchProducer::Changes(producer)));
		consumer
	}

	/// Take a watch off its path, pruning the nodes it leaves empty. Only ever
	/// called for a path the watch was added at, or the counts drift.
	fn remove_watch(&mut self, path: &Path, id: u64) {
		self.root.edit(path.parts(), Below::WATCH, |node| {
			node.watches.retain(|(watch, _)| *watch != id)
		});
	}

	/// Whether an interest strictly above `path` covers it.
	fn interest_above(&self, path: &Path) -> bool {
		let mut node = &self.root;
		for part in path.parts() {
			if node.interests > 0 {
				return true;
			}
			match node.children.get(part) {
				Some(child) => node = child,
				None => return false,
			}
		}
		false
	}

	/// The interests at `path` or, failing that, the broadest beneath it: what a
	/// session joining now asks about under `path`.
	fn interests_from(&self, path: &Path) -> Vec<Path> {
		let Some(node) = self.root.find(path.parts()) else {
			return Vec::new();
		};
		if node.interests > 0 {
			return vec![path.clone()];
		}
		let mut out = Vec::new();
		node.collect_interests(path, &mut out);
		out
	}

	/// Wake the watches of every path a route at `prefix` covers.
	fn poke_below(&self, prefix: &Path) {
		if let (_, Some(node)) = self.split(prefix) {
			node.poke_below(self);
		}
	}

	/// Wake every watch: the origin is tearing down.
	fn poke_all(&self) {
		self.root.walk(&mut |node| node.poke(None));
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
struct OriginState {
	// See [`Config::update_hold`]. No `Default`: every origin takes it from its
	// config, so no path silently disables the hold.
	update_hold: Duration,
	// See [`Config::route_key`].
	route_key: Option<super::origin::RouteKey>,

	// The announced routes, keyed by prefix. The table holds one entry per live
	// advertisement, not one per broadcast consumer.
	routes: RouteTable,
	// RouteInfo ids.
	next_route: u64,
	next_watch: u64,
	next_snapshot: u64,

	// The registered announce cursors, each with its own coalescing buffer. Each
	// also hangs in the route table at its heads, which is how an announcement
	// finds the cursors it can present on.
	cursors: HashMap<ConsumerId, TableCursor>,

	// The remotely-served fronts; see [`FrontKey`]. Each is a broadcast whose
	// watcher task materializes it from the best covering route and switches it
	// between routes, so a route change is invisible to subscribers. Weak, so a
	// front dies with its watcher and a later request re-creates it.
	fronts: WeakCache<FrontKey, RemoteFront>,

	// Peers that withdrew a prefix while routes there still passed through them,
	// which are stale. See [`Claim::withdrawn`].
	withdrawn: HashMap<Path, HashSet<Hop>>,

	// Set when the origin's driver dropped: new requests fail with `Closed`
	// immediately and handlers observe the end instead of parking forever.
	closed: bool,

	// The sessions asking their peer about only what readers here want
	// discovered; see [`Interests`].
	listeners: Vec<Listener>,
	next_listener: u64,

	// The sessions withholding this origin's hidden routes from their peer.
	hidden_withheld: HiddenWithheld,

	// The route layer node this origin's lite-07 sessions share, and the feeder the
	// driver owns; see [`Consumer::route_node`]. Weak, since the feeder's cursors hold
	// this state.
	route_node: Option<(crate::route::Node, std::sync::Weak<std::sync::Mutex<OwnClaims>>)>,
}

impl OriginState {
	fn new(update_hold: Duration, route_key: Option<super::origin::RouteKey>) -> Self {
		Self {
			update_hold,
			route_key,
			routes: RouteTable::default(),
			next_route: 0,
			next_watch: 0,
			next_snapshot: 0,
			cursors: HashMap::new(),
			fronts: WeakCache::default(),
			withdrawn: HashMap::new(),
			closed: false,
			listeners: Vec::new(),
			next_listener: 0,
			hidden_withheld: HiddenWithheld::default(),
			route_node: None,
		}
	}

	/// Whether a peer in `hops` withdrew `prefix`.
	fn withdrawn_through(&self, prefix: &Path, hops: &Hops) -> bool {
		if self.withdrawn.is_empty() {
			return false;
		}
		self.withdrawn
			.get(prefix)
			.is_some_and(|peers| hops.iter().any(|hop| peers.contains(hop)))
	}

	/// Recompute which entries at `prefix` are stale after its withdrawals changed.
	fn restale(&mut self, prefix: &Path) {
		let peers = self.withdrawn.get(prefix);
		let mut changed = None;
		for entry in self.routes.at_mut(prefix) {
			let stale = peers.is_some_and(|peers| entry.hops.iter().any(|hop| peers.contains(hop)));
			if entry.stale != stale {
				entry.stale = stale;
				changed = Some(entry.claim.clone());
			}
		}
		if let Some(claim) = changed {
			self.sync_route(prefix, &claim, Gone::End);
		}
	}

	/// A route at `prefix` was announced or restarted with `hops`. When its sender,
	/// the chain's last hop, had withdrawn the prefix, routes through it are live
	/// once more.
	fn reannounced(&mut self, prefix: &Path, hops: &Hops) {
		if let Some(sender) = hops.iter().last()
			&& self.withdrawn.get_mut(prefix).is_some_and(|peers| peers.remove(sender))
		{
			self.restale(prefix);
			self.prune_withdrawn(prefix);
		}
	}

	/// Forget the withdrawals at `prefix` that no longer hide a route.
	fn prune_withdrawn(&mut self, prefix: &Path) {
		if self.withdrawn.is_empty() {
			return;
		}
		let Some(peers) = self.withdrawn.get_mut(prefix) else {
			return;
		};
		if peers.len() <= 1 {
			// The common case is already linear and needs no temporary allocation.
			peers.retain(|peer| self.routes.at(prefix).any(|entry| entry.hops.contains(peer)));
		} else {
			let mut unreferenced = peers.clone();
			for entry in self.routes.at(prefix) {
				if unreferenced.is_empty() {
					break;
				}
				for hop in entry.hops.iter() {
					unreferenced.remove(hop);
				}
			}
			peers.retain(|peer| !unreferenced.contains(peer));
		}
		if peers.is_empty() {
			self.withdrawn.remove(prefix);
		}
	}

	/// Re-deliver the best route at every presented prefix `prefix` maps to, on
	/// every cursor it can present on. Called after an entry covering `prefix`
	/// was added, updated, or removed. `claim` is `prefix`'s [`prefix_claim`],
	/// held by the entry that changed.
	fn sync_route(&mut self, prefix: &Path, claim: &Pattern, gone: Gone) {
		// Split borrows: the recompute reads `routes` while mutating a cursor.
		let routes = &self.routes;
		let mut candidates = None;
		for id in routes.cursors_touching(prefix) {
			let Some(cursor) = self.cursors.get_mut(&id) else {
				continue;
			};
			if let Some(presented) = cursor.presented(prefix, claim) {
				if presented.is_empty() {
					// A cursor root can collapse several covering prefixes into one.
					Self::sync_cursor(routes, cursor, &presented, gone);
				} else {
					// Every non-root presentation refers to this exact prefix. Rank
					// its routes once, then take each cursor's first eligible visible entry.
					let candidates = candidates.get_or_insert_with(|| {
						let mut entries: Vec<_> = routes
							.at(prefix)
							.map(|entry| (route_order(prefix, entry), entry))
							.collect();
						entries.sort_unstable_by_key(|(order, _)| *order);
						entries
					});
					let visible = candidates
						.iter()
						.map(|(_, entry)| *entry)
						.filter(|entry| cursor.visible(entry));
					let epochs = Epochs::of(visible.clone());
					let best = visible.clone().find(|entry| !epochs.superseded(entry));
					cursor.update(&presented, best, gone);
				}
			}
		}
		// The fronts and requesters under the prefix re-select from the table.
		routes.poke_below(prefix);
	}

	/// Start a [`Snapshot`] at the absolute `prefix` for the routes arriving on `link`.
	fn snapshot(&mut self, shared: &SendCell<kio::Shared<OriginState>>, prefix: Path, link: Link) -> Snapshot {
		// A prefix past the depth limit can carry no route, so nothing waits on it.
		let Ok(claim) = prefix_claim(&prefix) else {
			return Snapshot::landed();
		};
		if self.closed {
			return Snapshot::landed();
		}
		let id = self.next_snapshot;
		self.next_snapshot += 1;
		self.add_snapshot(
			&prefix,
			SnapshotEntry {
				id,
				link,
				claim,
				prefix: prefix.clone(),
			},
		);
		Snapshot {
			shared: Some(shared.clone()),
			prefix,
			id,
		}
	}

	/// Count the absolute `head` as a prefix readers want discovered, telling the
	/// listeners when that changes the covering set.
	fn add_interest(&mut self, shared: &SendCell<kio::Shared<OriginState>>, head: &Path) {
		let node = self.routes.root.reach(head.parts(), Below::INTEREST);
		node.interests += 1;
		if node.interests > 1 || self.listeners.is_empty() {
			return;
		}
		let mut narrower = Vec::new();
		node.collect_interests(head, &mut narrower);
		if self.routes.interest_above(head) {
			return;
		}
		// Asked for before the narrower ones it covers are let go, so nothing retracts.
		self.emit(shared, head, true);
		for prefix in narrower {
			self.emit(shared, &prefix, false);
		}
	}

	/// Undo one [`Self::add_interest`] of `head`.
	fn remove_interest(&mut self, shared: &SendCell<kio::Shared<OriginState>>, head: &Path) {
		let listening = !self.listeners.is_empty();
		let narrower = self.routes.root.edit(head.parts(), Below::INTEREST, |node| {
			node.interests -= 1;
			(node.interests == 0 && listening).then(|| {
				let mut narrower = Vec::new();
				node.collect_interests(head, &mut narrower);
				narrower
			})
		});
		let Some(Some(narrower)) = narrower else {
			return;
		};
		if self.routes.interest_above(head) {
			return;
		}
		for prefix in narrower {
			self.emit(shared, &prefix, true);
		}
		self.emit(shared, head, false);
	}

	/// Tell every listener the covering set of interests gained or lost the
	/// absolute `prefix`.
	fn emit(&mut self, shared: &SendCell<kio::Shared<OriginState>>, prefix: &Path, wanted: bool) {
		let mut changes = Vec::new();
		for listener in &self.listeners {
			for relative in listener.asks(prefix) {
				// A head too long to root covers no path, so nothing reads it.
				let Ok(absolute) = listener.root.join(&relative) else {
					continue;
				};
				changes.push((listener.queue.clone(), absolute, relative, listener.link));
			}
		}
		for (queue, absolute, prefix, link) in changes {
			let snapshot = wanted.then(|| self.snapshot(shared, absolute, link));
			// A listener still registered has an open queue, except after the teardown,
			// whose snapshots are landed: dropping a refused one never takes this lock.
			let _ = queue.try_push(InterestChange {
				prefix,
				wanted,
				snapshot,
			});
		}
	}

	/// Register a [`Watch`] on the routes covering `path`.
	fn watch(&mut self, shared: &SendCell<kio::Shared<OriginState>>, path: &Path) -> Watch {
		let id = self.next_watch;
		self.next_watch += 1;
		let signal = self.routes.add_watch(path, id);
		Watch {
			shared: shared.clone(),
			path: path.clone(),
			id,
			signal,
		}
	}

	/// Recompute the best visible route presenting at `presented` (relative) for
	/// one cursor and deliver the change, if any.
	fn sync_cursor(routes: &RouteTable, cursor: &mut TableCursor, presented: &Path, gone: Gone) {
		// The entries presenting here are the ones announced at the absolute
		// prefix, or, for the cursor's own root, at the root and every prefix
		// above it (all of which present as the empty path). Among them, the
		// longest prefix wins outright, so the metadata a cursor advertises
		// matches what a request through it actually resolves.
		let best = match presented.is_empty() {
			true => cursor.best(routes.covering(&cursor.root)),
			false => {
				let absolute = cursor.root.rejoin(presented);
				cursor.best(routes.at(&absolute))
			}
		};

		cursor.update(presented, best, gone);
	}

	/// Register a cursor and replay the current best route per presented prefix.
	fn register_cursor(
		&mut self,
		shared: &SendCell<kio::Shared<OriginState>>,
		id: ConsumerId,
		mut cursor: TableCursor,
	) {
		// First, so the snapshots a session takes for these interests hold the cursor
		// unsynced from the start, through the scan below.
		if cursor.interested {
			for head in &cursor.heads {
				self.add_interest(shared, head);
			}
		}

		// The routes a cursor can see sit on the walk down to one of its heads or
		// somewhere beneath it, so only those subtrees are replayed.
		let mut presented: BTreeSet<Path> = BTreeSet::new();
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
			Self::sync_cursor(&self.routes, &mut cursor, p, Gone::End);
		}
		for head in &cursor.heads {
			self.routes.add_cursor(head, id);
		}

		// The snapshots in flight it can see sit where its routes would, each counted once.
		let mut snapshots = Vec::new();
		for head in &cursor.heads {
			let (above, at) = self.routes.split(head);
			snapshots.extend(above.into_iter().flat_map(|node| node.snapshots.iter()));
			if let Some(node) = at {
				node.collect_snapshots(&mut snapshots);
			}
		}
		snapshots.retain(|snapshot| cursor.awaits(snapshot));
		snapshots.sort_unstable_by_key(|snapshot| snapshot.id);
		snapshots.dedup_by_key(|snapshot| snapshot.id);
		if !snapshots.is_empty() {
			cursor
				.state
				.read()
				.waiting
				.fetch_add(snapshots.len(), Ordering::Relaxed);
		}

		self.cursors.insert(id, cursor);
	}

	/// Hold every cursor `snapshot` can present on unsynced until it lands.
	fn add_snapshot(&mut self, prefix: &Path, snapshot: SnapshotEntry) {
		for id in self.routes.cursors_touching(prefix) {
			if let Some(cursor) = self.cursors.get(&id)
				&& cursor.awaits(&snapshot)
			{
				cursor.state.read().waiting.fetch_add(1, Ordering::Relaxed);
			}
		}
		self.routes
			.root
			.reach(prefix.parts(), Below::SNAPSHOT)
			.snapshots
			.push(snapshot);
	}

	/// The snapshot `id` at `prefix` landed: release the cursors it held.
	fn remove_snapshot(&mut self, prefix: &Path, id: u64) {
		let removed = self.routes.root.edit(prefix.parts(), Below::SNAPSHOT, |node| {
			let index = node.snapshots.iter().position(|snapshot| snapshot.id == id)?;
			Some(node.snapshots.swap_remove(index))
		});
		let Some(Some(snapshot)) = removed else {
			return;
		};
		for id in self.routes.cursors_touching(prefix) {
			let Some(cursor) = self.cursors.get(&id).filter(|cursor| cursor.awaits(&snapshot)) else {
				continue;
			};
			// Only the last landing changes what a synced waiter sees, so only it takes the
			// write guard and wakes. It zeroes the count and owes the queue in one critical
			// section, so no poll sees nothing waiting before the owed set is in. The table
			// lock keeps the count from moving between the two guards.
			{
				let state = cursor.state.read();
				if state.waiting.load(Ordering::Relaxed) > 1 {
					state.waiting.fetch_sub(1, Ordering::Relaxed);
					continue;
				}
			}
			match cursor.state.write() {
				Ok(mut state) => {
					state.waiting.store(0, Ordering::Relaxed);
					state.owe();
				}
				Err(state) => state.waiting.store(0, Ordering::Relaxed),
			}
		}
	}

	/// The best served route covering `path` (absolute) for a requester seeing
	/// `horizon`, skipping the `excluded` entry ids.
	///
	/// The most specific covering prefix wins outright, so a narrow advertise-only
	/// announcement shadows a broad served one: requests under it resolve
	/// unroutable instead of being routed around it. Among routes at the winning
	/// prefix, the cheapest served one is picked by [`route_order`].
	///
	/// Only announced routes are candidates: an unannounced broadcast serves
	/// nobody, and does not shadow anything either. Routes `excluded` for the
	/// front's path are skipped. The hash tie-break is keyed on `path`, so
	/// equal-cost advertisers of one prefix share its paths. A broadcast
	/// published on this origin competes on cost like any other route and wins a
	/// tie. Only entries passing `only` are candidates at all, so the rest neither
	/// win nor shadow.
	fn best_route(
		&self,
		path: &Path,
		horizon: Horizon,
		excluded: &HashSet<u64>,
		only: impl Fn(&RouteEntry) -> bool,
	) -> Option<&RouteEntry> {
		self.routes.best_route(path, horizon, excluded, only)
	}

	/// The prefix route `server` answers through for `path`, when it lends its instance to
	/// the job routes answering its requests and its scope authorizes `path`.
	fn lender(&self, path: &Path, server: &kio::Shared<ServeState>) -> Option<Lent> {
		self.routes
			.covering(path)
			.find(|entry| {
				entry.lends && entry.live() && entry.server.as_ref().is_some_and(|other| other.same_channel(server))
			})
			.filter(|entry| entry.authorizes(path))
			.map(|entry| Lent {
				prefix: entry.prefix.clone(),
				id: entry.id,
				instance: entry.instance.clone(),
				epoch: entry.epoch,
			})
	}

	/// A job's route carrying `spent`, the instance it took from the prefix route `id` at
	/// `prefix`, stopped serving it: that prefix route moves to a fresh instance.
	///
	/// One instance is one stream at each path, and the job's keys end with it. Were the
	/// prefix route to keep the instance, its next answer for the path, a restarted job,
	/// would splice a new key space into everything that holds the old one, and nothing
	/// short of remembering every path it ever answered could tell. Under the table's lock,
	/// with the job's retraction, so no request resolves the path through the old instance
	/// in between. A prefix route already past `spent` has nothing to do.
	fn advance(&mut self, prefix: &Path, id: u64, spent: &Instance) {
		let Some(entry) = self.routes.entry_mut(prefix, id) else {
			return;
		};
		if entry.instance != *spent {
			return;
		}
		entry.instance = Instance::random();
		entry.epoch = 0;
		let claim = entry.claim.clone();
		self.sync_route(prefix, &claim, Gone::End);
	}

	/// Whether `instance` still serves `path` as a request would resolve it now: the winning
	/// route carries it, or, for a request `pinned` to it, a route of its own remains. The
	/// rule [`Consumer::lookup`] resolves by.
	fn still_serves(&self, path: &Path, horizon: Horizon, instance: &Instance, pinned: bool) -> bool {
		let Some(best) = self.best_route(path, horizon, &HashSet::new(), |_| true) else {
			return false;
		};
		if best.instance == *instance {
			return true;
		}
		pinned
			&& self
				.best_route(path, horizon, &HashSet::new(), |entry| entry.instance == *instance)
				.is_some()
	}
}

/// A prefix route lending its instance, as [`OriginState::lender`] found it.
struct Lent {
	prefix: Path,
	id: u64,
	instance: Instance,
	epoch: u64,
}

/// One-shot result of a dynamic broadcast request.
///
/// Stays `None` until a handler [`serve`](Request::serve)s (yielding the served
/// broadcast) or [`reject`](Request::reject)s (yielding an error). The producer is
/// dropped right after writing, closing the channel; kio checks the value before the closed
/// flag, so an awaiting requester still observes the final result.
#[derive(Default)]
struct PendingBroadcast {
	resolved: Option<Result<broadcast::Consumer, Error>>,
}

/// A front's verdict, which its requesters wait on: the instance it resolved, or the
/// error that ended it unresolved. A request joins the front only while that instance
/// still wins, or before then, while the one it selected does. Each requester holds the
/// front's broadcast from the moment it joins, so this carries no handle that would keep
/// the front held.
struct PendingFront {
	resolved: Option<Result<Instance, Error>>,
	/// The instance the front resolves on unless the table moves first: the winner it
	/// was minted for, then each one it selects. Written under the table's lock, so a
	/// request never joins a front that picked an instance the request saw beaten.
	selected: Instance,
	/// The selected serving handler refused, rather than the route disappearing.
	refused: bool,
}

impl Claim {
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn request_validity(&self, path: &Path) -> Option<Selection> {
		let shared = &self.state.announcement.shared;
		let server = &self.server;
		let mut table = shared.lock();
		if table.closed {
			return None;
		}
		let entry = table.best_route(path, Horizon::default(), &HashSet::new(), |entry| {
			entry.server.as_ref().is_some_and(|entry| entry.same_channel(server))
		})?;
		let via = entry.via;
		let instance = entry.instance.clone();
		let active = table
			.best_route(path, Horizon::default(), &HashSet::new(), |entry| entry.via == via)
			.is_some_and(|entry| entry.instance == instance);
		let producer = kio::Producer::new(Selected {
			path: path.clone(),
			via,
			instance,
			current: active.then(kio::Producer::default),
			answering: active.then(kio::Producer::default),
		});
		let signal = producer.consume();
		let id = table.next_watch;
		table.next_watch += 1;
		table
			.routes
			.root
			.reach(path.parts(), Below::WATCH)
			.watches
			.push((id, WatchProducer::Selection(producer)));
		Some(Selection {
			shared: shared.clone(),
			path: path.clone(),
			id,
			signal,
		})
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
	/// `request` from the wake must not find the non-reentrant lock
	/// still held.
	fn resolve(
		shared: &kio::Shared<Self>,
		path: &Path,
		producer: &kio::Producer<PendingBroadcast>,
		result: Result<broadcast::Consumer, Error>,
	) -> bool {
		let mut state = shared.lock();
		let Some(resolved) = state.answer(path, producer, result) else {
			return false;
		};
		if let Ok(mut pending) = producer.write() {
			pending.resolved.get_or_insert(resolved);
			drop(state);
		}
		true
	}

	/// Take the request off the queue and cache an accepted broadcast, returning what
	/// its requesters resolve with, or `None` once it is no longer ours to answer.
	fn answer(
		&mut self,
		path: &Path,
		producer: &kio::Producer<PendingBroadcast>,
		result: Result<broadcast::Consumer, Error>,
	) -> Option<Result<broadcast::Consumer, Error>> {
		self.take(path, producer)?;
		let state = self;
		let resolved = match result {
			Ok(broadcast) => {
				// If a live broadcast was already served for this path while we were
				// fetching upstream, dedup onto it and drop ours rather than replace
				// a good entry with a duplicate subscription. One its session retired
				// as this minted is closed, so ours takes the slot after all.
				match state
					.served
					.insert(path.clone(), broadcast.weak())
					.map(|weak| weak.consume())
				{
					Some(existing) if !existing.is_closed() => Ok(existing),
					Some(_) => {
						state.served.insert(path.clone(), broadcast.weak());
						Ok(broadcast)
					}
					None => Ok(broadcast),
				}
			}
			Err(err) => Err(err),
		};
		Some(resolved)
	}

	/// Admit an answer only while its request still belongs to this route instance.
	fn take(&mut self, path: &Path, producer: &kio::Producer<PendingBroadcast>) -> Option<()> {
		if self.closed {
			return None;
		}
		self.requests.remove_if(path, |p| p.same_channel(producer))?;
		Some(())
	}

	/// Release every request, queued or handed to a handler, and forget the answers served,
	/// because the route now names another instance. Returns the released
	/// requests for the caller to refuse once it holds no lock, since the refusal wakes
	/// their fronts. Those ask again under the new instance; the handler's answer to a
	/// released request is dropped.
	#[must_use = "the released requests must be refused"]
	fn renew(&mut self) -> Vec<kio::Producer<PendingBroadcast>> {
		// Counted before any release wakes a front, which reads the count without the lock.
		self.renewals.fetch_add(1, Ordering::Release);
		self.served = WeakCache::default();
		self.requests.drain_all()
	}

	/// Drop the still-pending entry, if it is still ours.
	fn forget(shared: &kio::Shared<Self>, path: &Path, producer: &kio::Producer<PendingBroadcast>) {
		shared.lock().requests.remove_if(path, |p| p.same_channel(producer));
	}

	/// Queue the still-pending entry again for the next handler, its requesters still waiting,
	/// or drop it as [`Self::forget`] does when no handler is left to take it.
	fn requeue(shared: &kio::Shared<Self>, path: &Path, producer: &kio::Producer<PendingBroadcast>) {
		let mut state = shared.lock();
		if !state.requests.get(path).is_some_and(|p| p.same_channel(producer)) {
			return;
		}
		if !state.requests.requeue(path.clone()) {
			state.requests.remove_if(path, |p| p.same_channel(producer));
		}
	}
}

/// A pending request for a broadcast to be served on demand.
///
/// Yielded by [`Claim::poll_requested`]. The requester is awaiting inside
/// [`Consumer::request`]. Answer it one of three ways: [`serve`](Self::serve) a
/// broadcast already held, under the prefix route's instance; create the path's own
/// exact route with [`route`](Self::route), which continues the instance the prefix route
/// lends until it ends; or
/// [`reject`](Self::reject) it with an error. Dropping it rejects it.
///
/// Serve a held broadcast without announcing an exact route:
///
/// ```
/// fn answer(request: moq_net::origin::Request, broadcast: moq_net::broadcast::Consumer) {
///     request.serve(broadcast);
/// }
/// ```
pub struct Request {
	// Absolute path that was requested.
	path: Path,

	// Result channel back to the awaiting requester(s). Writing `resolved` and dropping
	// this wakes them with the outcome.
	producer: kio::Producer<PendingBroadcast>,

	// The queue this request came from, so `serve` can cache the served
	// broadcast for repeat requests.
	home: kio::Shared<ServeState>,

	// Applications can create routes in this origin; a session-forwarded request
	// can only serve its peer's already-routed broadcast.
	publisher: Option<Producer>,

	// Dropped back onto its queue rather than refused: its job read an instance the prefix
	// route has since moved past, so the handler answers it again.
	requeue: bool,
}

impl Request {
	/// The absolute path that was requested.
	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Answer with the path's own exact route, owning a new broadcast.
	///
	/// Unless `options` names an instance, the route takes the one its prefix route lends
	/// when it first announces or [`Route::instance`] first reads it, so whatever resolved the path through the prefix route
	/// continues on this answer; a prefix route that lends nothing has the route mint its
	/// own. Ending the route withdraws only this path, and moves a prefix route that lent
	/// its instance to a fresh one, so a job that ends and restarts beneath the same prefix
	/// is a new stream for its restarted keys. The prefix route keeps answering later
	/// requests. Created visible, the route announces now and answers the requesters
	/// waiting on its path. Created [`RouteOptions::hidden`], they wait until
	/// [`Route::announce`], so the job can fill its catalog first; dropping the route
	/// before then refuses them.
	///
	/// Fails with [`Error::Unroutable`] on a request a session forwarded, which can only
	/// [`serve`](Self::serve) its peer's broadcast, and, announcing nothing, once the
	/// request was released or its prefix route retracted. Otherwise it fails as
	/// [`Producer::route`] does.
	pub fn route(self, options: RouteOptions) -> Result<Route, Error> {
		let publisher = self.publisher.clone().ok_or(Error::Unroutable)?;
		let path = self.path.clone();
		publisher.exact(path, options, Some(self))
	}

	/// Serve a held broadcast under this prefix route without announcing an exact route.
	///
	/// The answer belongs to the prefix route's instance at this path: every answer for
	/// that path must continue the same key space and bytes. Start a new prefix
	/// route to serve different content there. For a job that may restart independently,
	/// use [`Self::route`]: its end moves the prefix route to another instance.
	pub fn serve(self, broadcast: impl Consume<broadcast::Consumer>) {
		let broadcast = broadcast.consume();
		let _ = ServeState::resolve(&self.home, &self.path, &self.producer, Ok(broadcast));
		// `self.producer` drops here, closing the channel; the value is still observable.
	}

	/// Reject the request, resolving every awaiting requester with `err`.
	pub fn reject(self, err: Error) {
		let _ = ServeState::resolve(&self.home, &self.path, &self.producer, Err(err));
	}
}

impl Drop for Request {
	fn drop(&mut self) {
		// Requeued in the same lock as the removal it replaces, so no handler pops it while
		// this one still holds it.
		if self.requeue {
			ServeState::requeue(&self.home, &self.path, &self.producer);
			return;
		}
		// Handed off but neither accepted nor rejected: drop the still-pending entry so its
		// producer clone (plus this one) closes the channel, resolving coalesced requesters to
		// `Unroutable` rather than hanging.
		//
		// The identity guard matters: `serve`/`reject` already removed our entry and released
		// the lock before we run, so a concurrent request for the same path may have registered
		// a *new* one here. Removing unconditionally would clobber it, stranding its requesters.
		ServeState::forget(&self.home, &self.path, &self.producer);
	}
}

/// The immediate table lookup serving a peer without initiating application discovery.
// private: capability gap
pub(crate) struct Lookup {
	inner: RequestState,
	// The path the requester asked for, relative to its cursor's root. Stamped on the
	// resolved broadcast (see [`broadcast::Info::path`]) because a handler is free to
	// serve a broadcast created somewhere else entirely, or at no path at all.
	path: Path,
	// Egress scope applied to the resolved broadcast, so its reads are attributed.
	// Empty (no-op) for an untagged consumer.
	stats: stats::Scope,
	// The instance the requester named, if any: the front may resolve on another one
	// when the route changes while the request is in flight.
	instance: Option<Instance>,
}

enum RequestState {
	// Unroutable at request time: resolves immediately with this error. Baked in so
	// `request` itself stays infallible.
	Failed(Error),
	// Joined a front: resolves with the front's broadcast once its verdict is in, held
	// from the moment of joining so the front cannot retire under the requester.
	Pending {
		front: kio::Consumer<PendingFront>,
		broadcast: broadcast::Consumer,
	},
}

impl Lookup {
	fn failed(error: Error) -> Self {
		Self::new(RequestState::Failed(error))
	}

	fn queued(front: kio::Consumer<PendingFront>, broadcast: broadcast::Consumer) -> Self {
		Self::new(RequestState::Pending { front, broadcast })
	}

	fn new(inner: RequestState) -> Self {
		Self {
			inner,
			path: Path::default(),
			stats: stats::Scope::default(),
			instance: None,
		}
	}

	/// Refuse a resolution on any instance but `instance`.
	fn with_instance(mut self, instance: Option<Instance>) -> Self {
		self.instance = instance;
		self
	}

	fn with_path(mut self, path: Path) -> Self {
		self.path = path;
		self
	}

	/// The egress scope the resolved broadcast's reads are attributed to.
	fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// Stamp a resolved broadcast with the path this cursor asked for, its egress scope,
	/// and the instance of the route it resolved through.
	fn hand_out(&self, broadcast: broadcast::Consumer, instance: Instance) -> broadcast::Consumer {
		broadcast
			.with_path(self.path.clone())
			.with_stats(self.stats.clone())
			.with_instance(Some(instance))
	}

	/// Poll for the requested broadcast without blocking.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<broadcast::Consumer, Error>> {
		match &self.inner {
			RequestState::Failed(error) => Poll::Ready(Err(error.clone())),
			RequestState::Pending { front, broadcast } => Poll::Ready(
				match ready!(front.poll(waiter, |state| match &state.resolved {
					Some(result) => Poll::Ready(result.clone()),
					None => Poll::Pending,
				})) {
					// Another instance than the one named: never hand it out.
					Ok(Ok(instance)) if self.instance.as_ref().is_some_and(|named| *named != instance) => {
						Err(Error::Unroutable)
					}
					Ok(Ok(instance)) => Ok(self.hand_out(broadcast.clone(), instance)),
					Ok(Err(err)) => Err(err),
					// The front's driver went away without a verdict: nobody could route it.
					Err(_closed) => Err(Error::Unroutable),
				},
			),
		}
	}
}

impl kio::Task for Lookup {
	type Output = Result<broadcast::Consumer, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// An owned broadcast resolution, driven by polling or awaiting it.
pub struct Requesting {
	consumer: Consumer,
	path: Path,
	// Asks sessions to discover the path for as long as this waits on it.
	interest: Option<Interest>,
	// The covering routes, watched since the generation the last ask saw.
	watch: Option<(Watch, u64)>,
	// The ask in flight, dropped once it fails `Unroutable` until the routes move.
	request: Option<Lookup>,
	// The instance every ask is pinned to, if any.
	instance: Option<Instance>,
	// A finite request owns discovery and can conclude absence once its initial set lands.
	discovery: Option<Follow>,
	routed: bool,
}

impl Requesting {
	/// Poll for the requested instance, retrying only after its covering routes change.
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<broadcast::Consumer, Error>> {
		loop {
			if let Some(discovery) = &mut self.discovery {
				loop {
					match discovery.poll_next(waiter) {
						Poll::Ready(Some(RouteEvent {
							kind: RouteKind::End | RouteKind::Offline,
							..
						})) => self.routed = false,
						Poll::Ready(Some(RouteEvent {
							kind: RouteKind::Start | RouteKind::Restart | RouteKind::Update,
							announce,
						})) => {
							self.routed = self.instance.is_none() || announce.route.instance == self.instance;
						}
						Poll::Ready(None) => return Poll::Ready(Err(Error::Closed)),
						Poll::Pending => break,
					}
				}
				// Sync is an event, not a deadline: sessions hold it from the moment the
				// interest is created until the initial announcement set is delivered.
				let synced = discovery.poll_synced(waiter).is_ready();
				if self.request.is_none()
					&& synced && (!self.routed || self.watch.as_ref().is_some_and(|(watch, seen)| watch.seen() == *seen))
				{
					return Poll::Ready(Err(Error::Unroutable));
				}
			}

			match (&self.watch, &self.request) {
				(_, Some(request)) => match ready!(request.poll_ok(waiter)) {
					Ok(broadcast) => return Poll::Ready(Ok(broadcast)),
					Err(Error::Unroutable) => {
						// The front already distinguishes a selected handler's refusal
						// from route churn; retain that verdict rather than infer it from
						// unrelated changes a prepared answer makes to the table.
						let refused = match &request.inner {
							RequestState::Pending { front, .. } => front.read().refused,
							RequestState::Failed(_) => false,
						};
						if refused {
							return Poll::Ready(Err(Error::Unroutable));
						}
						self.request = None;
					}
					Err(err) => return Poll::Ready(Err(err)),
				},
				(Some((watch, seen)), None) => {
					ready!(watch.poll_changed(waiter, *seen));
					self.watch = None;
				}
				(None, None) => {
					// `Unroutable` is a verdict of the routes covering the path as they
					// stood when the request was made. Re-asking the same routes would
					// spin, so watch them before asking and wait for them to move (a
					// route arriving or retracting, an identical standby swapping in, a
					// local broadcast announcing at the path), then try again. A change
					// between the ask and the wait bumps the watch first, so that retry
					// is immediate; the teardown pokes every watch, so a closed origin
					// is observed on the next pass.
					let mut table = self.consumer.shared.lock();
					if table.closed {
						return Poll::Ready(Err(Error::Closed));
					}
					let named = match self.consumer.root.join(&self.path) {
						Ok(named) => named,
						Err(err) => return Poll::Ready(Err(err.into())),
					};
					let Some(resolved) = self.consumer.scope.resolve(&named) else {
						return Poll::Ready(Err(BoundsExceeded.into()));
					};
					let watch = table.watch(&self.consumer.shared, &resolved);
					let seen = watch.seen();
					if self.interest.is_none() && self.discovery.is_none() && self.consumer.interested {
						let resolved = resolved.clone();
						self.interest = Some(Interest::new(&mut table, &self.consumer.shared, resolved));
					}
					drop(table);
					self.watch = Some((watch, seen));
					self.request = Some(
						self.consumer
							.lookup(self.path.clone(), self.instance.as_ref())
							.into_inner(),
					);
				}
			}
		}
	}
}

impl kio::Task for Requesting {
	type Output = Result<broadcast::Consumer, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Waits for an instance to stop serving a path, watching the routes covering it. Unlike
/// the broadcast a request resolved, which also closes once its last holder lets it go, this
/// ends only when the routes move to another instance or away. Created by [`Consumer::moved`].
pub(super) struct Moved {
	consumer: Consumer,
	path: Path,
	instance: Instance,
	pinned: bool,
	// The covering routes, watched since the generation the last check saw.
	watch: Option<(Watch, u64)>,
}

impl Moved {
	/// Ready once `instance` no longer serves the path, or the origin closed.
	pub(super) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			if let Some((watch, seen)) = &self.watch {
				ready!(watch.poll_changed(waiter, *seen));
			}
			let named = match self.consumer.root.join(&self.path) {
				Ok(named) if self.consumer.scope.permits(&named) => named,
				_ => return Poll::Ready(()),
			};
			let Some(absolute) = self.consumer.scope.resolve(&named) else {
				return Poll::Ready(());
			};
			let mut state = self.consumer.shared.lock();
			if state.closed {
				return Poll::Ready(());
			}
			// Read the generation under the lock that decides, so a change after it bumps
			// the watch and the next pass checks again. Filling an empty slot drops no watch,
			// whose drop takes this lock.
			match &mut self.watch {
				Some((watch, seen)) => *seen = watch.seen(),
				None => {
					let watch = state.watch(&self.consumer.shared, &absolute);
					let seen = watch.seen();
					self.watch = Some((watch, seen));
				}
			}
			let horizon = self.consumer.horizon.effective(&state.routes, &absolute);
			let serves = state.still_serves(&absolute, horizon, &self.instance, self.pinned);
			drop(state);
			if !serves {
				return Poll::Ready(());
			}
		}
	}
}

/// Derive a read view from a handle.
///
/// Lets APIs accept either a producer or a consumer (e.g.
/// `Client::with_publisher`). The blanket `&T` impl means you can
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
/// resolve a path into a broadcast, call [`Self::request`].
#[derive(Clone)]
pub struct Consumer {
	// Identity of the origin this consumer was derived from.
	hop: NonZeroU64,
	max_hops: NonZeroUsize,
	scope: OriginScope,

	// A prefix that is automatically stripped from all paths.
	root: Path,

	// The origin's shared state: the route table, announce cursors, and the
	// remotely-served fronts.
	shared: SendCell<kio::Shared<OriginState>>,

	// Egress stats context. Broadcasts handed out through this consumer (and any
	// handle derived from them) are attributed to it (reads counted on the
	// publisher/egress side). Empty (no-op) unless a session tagged this handle.
	stats: stats::Session,

	// Split horizon: routes whose hop chain or announcing session (`via`) is the
	// excluded peer are invisible to `announced` and skipped by
	// `request`, so a peer is never served (or advertised) its own
	// content back. A local view (`Self::local`) hides peer routes the same way,
	// and a view taken from an upstream handle hides upstream routes.
	horizon: Horizon,

	// Which routes beneath a hidden (`.`-prefixed) segment `announced` reports.
	hidden: Hidden,

	// Whether `announced` also reports routes whose subtree only overlaps the scope.
	covering: bool,

	// Whether this handle's announcement streams and routed requests count as
	// interests, so a session fed into the origin asks its peer about them. Off
	// for a session serving its peer (see `Self::serving`).
	interested: bool,

	// Whether a lite-07 session publishing through this handle sends its peer the
	// routes this node learned from other peers (see `Self::with_proxy`).
	proxy: bool,

	// The cache policy remote fronts inherit, mirroring what
	// `route` gives a local front.
	pool: cache::Pool,
	cache_duration: Option<Duration>,

	// Non-owning submission handle to the origin's [`Driver`], for the front
	// watcher a routed `request` spawns. Non-owning so a lingering
	// read handle never keeps the driver from finishing.
	tasks: TasksWeak,

	// The driver's clock and timers, threaded into fronts for the track idle
	// linger.
	timers: Deadlines,
}

impl Consumer {
	fn from_producer(producer: &Producer, stats: stats::Session) -> Self {
		Self {
			hop: producer.hop,
			max_hops: producer.max_hops,
			scope: producer.scope.clone(),
			root: producer.root.clone(),
			shared: producer.shared.clone(),
			stats,
			horizon: Horizon {
				upstream: producer.link == Link::Upstream,
				..Horizon::default()
			},
			hidden: Hidden::default(),
			covering: false,
			interested: true,
			proxy: producer.proxy,
			pool: producer.pool.clone(),
			cache_duration: producer.cache_duration,
			tasks: producer.tasks.downgrade(),
			timers: producer.timers.clone(),
		}
	}

	/// This origin's hop id; see [`Config::hop`].
	pub fn hop(&self) -> NonZeroU64 {
		self.hop
	}

	/// The key sessions prove for this origin.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn route_key(&self) -> Option<super::origin::RouteKey> {
		self.shared.lock().route_key.clone()
	}

	/// A clone that never serves the given peer its own data: routes whose hop
	/// chain contains `peer`, or whose announcing session is `peer`, are invisible
	/// and never resolved from, matching what the announce loop advertises to them.
	/// Sessions apply this once they learn the peer's origin id. Hop 0 identifies
	/// nobody, so the announcing session's assigned identity is what keeps an
	/// anonymous route from echoing back. Pass [`Hop::UNKNOWN`] for an anonymous
	/// peer.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn excluding(mut self, peer: Hop) -> Self {
		self.horizon.exclude = Some(peer);
		self
	}

	/// The Node ID this origin's moq-lite-07 sessions declare, without creating the route
	/// node: its identity once created, or the one it will declare. `None` only before a
	/// node whose hop leaves nothing to derive draws a random one.
	pub(crate) fn node_id(&self) -> Option<NonZeroU64> {
		let node = self.shared.lock().route_node.clone();
		let id = match node {
			Some((node, _)) => Some(node.declared()),
			None => crate::route::node::declared_for(self.hop),
		};
		id.and_then(NonZeroU64::new)
	}

	/// Hold back the disconnect of every route stream dropped until the guard is
	/// released, so a session closing its streams costs the route layer one recompute;
	/// `None` while no session created the route node, which then has no streams.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn route_closing(&self) -> Option<crate::route::node::Closing> {
		let node = self.shared.lock().route_node.clone();
		node.map(|(node, _)| node.closing())
	}

	/// The route layer node every lite-07 session on this origin shares, created on
	/// first use along with the task that feeds it the routes this node serves
	/// itself: everything in the table the route layer did not select. It is caught up
	/// with the table on every call, so a stream registered next presents what the
	/// table holds now, not what the driver last fed it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn route_node(&self) -> crate::route::Node {
		let existing = self.shared.lock().route_node.clone();
		let (node, own) = existing.unwrap_or_else(|| self.create_route_node());
		if let Some(own) = own.upgrade() {
			// Catching up wakes the node's streams and the feeder's own cursors. Those wakes
			// wait for the feeder's lock: a waker may poll a session inline, which calls back here.
			let _hold = kio::hold();
			lock_own(&own).catch_up();
		}
		node
	}

	fn create_route_node(&self) -> (crate::route::Node, std::sync::Weak<std::sync::Mutex<OwnClaims>>) {
		// The whole table, counted against no session: only origin-wide state, so the node
		// is the same whichever handle creates it. No `..self` here, so a new per-handle
		// field cannot leak in.
		let own = |upstream| Self {
			hop: self.hop,
			max_hops: self.max_hops,
			scope: OriginScope::default(),
			root: Path::default(),
			shared: self.shared.clone(),
			stats: stats::Session::default(),
			horizon: Horizon {
				node: NodeFilter::Own,
				upstream,
				..Horizon::default()
			},
			hidden: Hidden {
				include: true,
				from: None,
			},
			// Feeding the node is no reason to ask any peer about anything, as when serving.
			covering: true,
			interested: false,
			proxy: false,
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			tasks: self.tasks.clone(),
			timers: self.timers.clone(),
		};
		// Built before taking the lock, since opening a cursor takes it.
		let node = crate::route::Node::origin(self.hop());
		let claims = OwnClaims {
			routes: own(false).routes(),
			downstream: own(true).routes(),
			best: HashMap::new(),
			local: HashMap::new(),
			node: node.clone(),
		};
		let mut shared = self.shared.lock();
		// A concurrent first call won the race: use its node, and close these cursors
		// only once the lock is released, since closing one takes it.
		if let Some(existing) = shared.route_node.clone() {
			drop(shared);
			return existing;
		}
		let claims = Arc::new(std::sync::Mutex::new(claims));
		let created = (node, Arc::downgrade(&claims));
		shared.route_node = Some(created.clone());
		drop(shared);
		self.tasks.push(FeedOwnClaims {
			claims,
			origin: self.tasks.clone(),
		});
		created
	}

	/// Whether this handle carries its peer's traffic to other origins, taken from
	/// the [`Producer::with_proxy`] it was consumed from.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn proxy(&self) -> bool {
		self.proxy
	}

	/// Whether the reader of this handle is an upstream peer, which is never sent a
	/// route learned from another upstream.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn upstream(&self) -> bool {
		self.horizon.upstream
	}

	/// Whether this handle reaches a claim at the absolute `path` as it names it: a
	/// path at or beneath it may be in its scope, as a covering reader sees, and it is
	/// not in an excluded subtree. Beneath a mount, `path` names the target's claim
	/// there, never one the origin holds at `path` itself (see [`Self::mounted`]).
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn reaches(&self, path: &Path) -> bool {
		let Ok(subtree) = Pattern::subtree(path.as_str()) else {
			return false;
		};
		self.scope.allowed.overlaps(&subtree)
			&& !self.scope.excluded.iter().any(|excluded| path.has_prefix(excluded))
			&& self.scope.mount(path).is_none_or(|mount| mount.resolve(path).is_some())
	}

	/// Whether the absolute `path` lies beneath one of this handle's mounts, where the
	/// origin's own claims at that path are hidden from it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn shadows(&self, path: &Path) -> bool {
		self.scope.mount(path).is_some()
	}

	/// The mounts a reader of the absolute `prefix` sees through this handle: each one
	/// at or beneath it, or the one it lies beneath, unless excluded.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn mounted(&self, prefix: &Path) -> Vec<Mounted> {
		(self.scope.mounts.iter())
			.filter(|mount| self.scope.holes(Some(mount)).is_some())
			.filter_map(|mount| {
				Some(Mounted {
					interest: mount.translate_head(prefix)?,
					mount: mount.clone(),
				})
			})
			.collect()
	}

	/// Whether this handle reaches every path at or beneath the absolute `prefix`,
	/// judged pattern by pattern, so a `false` may be spurious but a `true` never is.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn covers(&self, prefix: &Path) -> bool {
		let Ok(subtree) = Pattern::subtree(prefix.as_str()) else {
			return false;
		};
		let overlaps = |other: &Path| other.has_prefix(prefix) || prefix.has_prefix(other);
		self.scope.allowed.contains(&subtree)
			&& !self.scope.excluded.iter().any(overlaps)
			&& !self.scope.mounts.iter().any(|mount| overlaps(&mount.at))
	}

	/// A clone that sees only the routes `node` admits: a request naming an origin
	/// node resolves only through that node's routes, and the route layer's own
	/// claims are the routes this node serves itself.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn with_node(mut self, node: NodeFilter) -> Self {
		self.horizon.node = node;
		self
	}

	/// A clone whose announcements skip the routes this origin cannot forward: a
	/// chain already at [`Config::max_hops`] has no room for our hop. Sessions whose
	/// wire carries the hop path apply this to the routes they advertise.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn forwardable(mut self) -> Self {
		self.horizon.max_hops = Some(self.max_hops);
		self
	}

	/// A view of the routes that entered here: every route a handle marked
	/// [`Link::Peer`] or [`Link::Upstream`] handle announced is hidden from [`Self::announced`] and never
	/// resolved by [`Self::request`].
	///
	/// On a relay, this is what the relay ingests itself, from clients and
	/// in-process producers, as opposed to what its cluster peers forward.
	pub fn local(mut self) -> Self {
		self.horizon.local = true;
		self
	}

	/// A clone for a session serving its peer: its reads are the peer's, not a
	/// local reader's, so they never make a session ask its own peer about them.
	/// It is [`covering`](Self::covering), since the peer routes requests through
	/// what we announce.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn serving(mut self) -> Self {
		self.interested = false;
		self.covering = true;
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

	/// A clone whose [`announced`](Self::announced) also reports every route that could
	/// serve a path in scope, not just those whose literal name matches it.
	///
	/// A route is a prefix that serves every path beneath it, so a route at `room` can
	/// serve `room/lobby.chat` under a `**/*.chat` scope, and one above the root covers
	/// all of it. Code that routes requests or forwards announcements needs those
	/// routes; a session serving its peer always uses this view. Listing broadcasts does
	/// not. Requests resolve through covering routes either way.
	pub fn covering(mut self) -> Self {
		self.covering = true;
		self
	}

	/// A clone that never reaches the subtree at `prefix`, relative to this root:
	/// [`announced`](Self::announced) leaves out the routes at or beneath it and a
	/// request for a path there fails with [`Error::Unauthorized`]. A route
	/// announced above `prefix` is still reported. [`allowed`](Self::allowed) is unchanged.
	pub fn exclude(&self, prefix: impl TryInto<Path, Error: Into<Error>>) -> Result<Self, Error> {
		let prefix = self.root.join(&path::convert(prefix)?)?;
		let mut consumer = self.clone();
		consumer.scope.excluded = self.scope.excluded.iter().cloned().chain([prefix]).collect();
		Ok(consumer)
	}

	/// Set wire discovery visibility relative to this consumer's requested root.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn discovery(mut self, hidden: bool) -> Self {
		self.hidden.include = hidden;
		self.hidden.from = Some(self.root.clone());
		self
	}

	/// Whether [`announced`](Self::announced) reports hidden routes too.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn includes_hidden(&self) -> bool {
		self.hidden.include
	}

	/// The sessions withholding this origin's hidden routes from their peer.
	pub(crate) fn hidden_withheld(&self) -> HiddenWithheld {
		self.shared.read().hidden_withheld.clone()
	}

	/// The egress stats context this handle was tagged with.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn stats(&self) -> &stats::Session {
		&self.stats
	}

	/// Attach an egress stats context: broadcasts handed out through this handle (and
	/// any handle derived from it) are attributed to `session` on the publisher
	/// (egress) side. Pass [`stats::Session::default`] to opt out.
	pub fn with_stats(mut self, session: stats::Session) -> Self {
		self.stats = session;
		self
	}

	/// A clone of this consumer with its stats context cleared, so an internal
	/// lookup stream (e.g. [`Self::routes`]) doesn't drive the egress
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
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn empty(&self) -> Self {
		Self {
			scope: OriginScope::empty(),
			..self.clone()
		}
	}

	/// List the paths announced in this consumer's scope, each present or absent.
	///
	/// A path stays present while any route covers it, whichever instance serves it:
	/// hold a [`follow`](Self::follow) to see the broadcast change. Scope, hiding and
	/// sync behave as for [`routes`](Self::routes). Drop the returned
	/// [`announce::Consumer`](crate::announce::Consumer) to unregister.
	pub fn announced(&self) -> AnnounceConsumer {
		AnnounceConsumer::new(self.routes())
	}

	/// Every change to the routes announced in this consumer's scope, for relays and
	/// stats: route metadata and instance replacements included.
	///
	/// Allocates a per-cursor coalescing buffer and replays the currently
	/// announced routes as initial updates. Routes stay prefixes and are named
	/// relative to this consumer's root; its patterns only filter visibility.
	/// Only routes whose literal name matches those patterns are reported, unless
	/// [`covering`](Self::covering) asks for every route that could serve them.
	/// Routes with a segment starting with `.` below the literal head of those
	/// patterns are hidden unless [`with_hidden`](Self::with_hidden) opted in.
	/// The stream also watches its scope: a client session fed by this origin asks
	/// its peer about the scope while any stream watches it.
	/// Drop the returned [`Routes`] to unregister.
	pub fn routes(&self) -> Routes {
		let state = kio::Producer::<OriginConsumerState>::default();
		let cursor =
			|root: Path, allowed: Patterns, hidden: Hidden, mount: Option<&Mount>, under: Path, holes: Vec<Path>| {
				TableCursor {
					root,
					heads: interest_prefixes(&allowed),
					allowed,
					horizon: self.horizon,
					hidden,
					covering: self.covering,
					state: state.clone(),
					under,
					holes,
					named: mount.map(|mount| (mount.clone(), self.scope.allowed.clone())),
					interested: self.interested,
					current: HashMap::new(),
				}
			};

		// A root at or beneath a mount reads only the mount: one cursor, re-rooted
		// onto the target.
		if let Some(mount) = self.scope.mount(&self.root) {
			// A root the mount resolves past the depth limit, or an excluded one, has
			// nothing to announce.
			let (Some(root), Some(holes)) = (mount.resolve(&self.root), self.scope.holes(Some(mount))) else {
				return Routes::new(
					self.root.clone(),
					Vec::new(),
					state,
					self.stats.clone(),
					&self.shared,
					self.timers.clone(),
				);
			};
			let cursors = vec![cursor(
				root,
				mount.translate(&self.scope.allowed),
				self.hidden.translate(mount),
				Some(mount),
				Path::default(),
				holes,
			)];
			return Routes::new(
				self.root.clone(),
				cursors,
				state,
				self.stats.clone(),
				&self.shared,
				self.timers.clone(),
			);
		}

		// Otherwise the consumer's own cursor, minus the mounted and excluded subtrees,
		// plus one cursor per mount beneath the root it may discover, presenting under it.
		let heads = interest_prefixes(&self.scope.allowed);
		let mut holes = self.scope.holes(None).unwrap_or_default();
		let mut cursors = Vec::new();
		for mount in self.scope.mounts.iter() {
			let Some(under) = mount.at.strip_prefix(&self.root) else {
				continue;
			};
			holes.push(mount.at.clone());
			let Some(mount_holes) = self.scope.holes(Some(mount)) else {
				continue;
			};
			let allowed = mount.translate(&self.scope.allowed);
			// Hidden at the mount point means hidden throughout: the dot segment is above
			// everything the mount holds. A top-up feed's rule moves onto the target.
			if allowed.is_empty()
				|| !(self.hidden.include
					|| !hides(
						self.hidden.from.as_ref().map(std::slice::from_ref).unwrap_or(&heads),
						&mount.at,
					)) {
				continue;
			}
			cursors.push(cursor(
				mount.target.clone(),
				allowed,
				self.hidden.translate(mount),
				Some(mount),
				under.clone(),
				mount_holes,
			));
		}
		cursors.insert(
			0,
			cursor(
				self.root.clone(),
				self.scope.allowed.clone(),
				self.hidden.clone(),
				None,
				Path::default(),
				holes,
			),
		);
		Routes::new(
			self.root.clone(),
			cursors,
			state,
			self.stats.clone(),
			&self.shared,
			self.timers.clone(),
		)
	}

	/// Returns a cheap duplicate of this read handle.
	pub fn consume(&self) -> Self {
		self.clone()
	}

	/// The newest broadcast published on this origin at exactly `path`, if any.
	/// Test-only: a request goes through the table like any other.
	#[cfg(test)]
	pub(crate) fn get_broadcast(&self, path: impl TryInto<Path, Error: Into<Error>>) -> Option<broadcast::Consumer> {
		let full = self.root.join(&path::convert::<Error>(path).ok()?).ok()?;
		if !self.scope.permits(&full) {
			return None;
		}
		let full = self.scope.resolve(&full)?;
		let table = self.shared.lock();
		table
			.routes
			.at(&full)
			.filter(|entry| entry.local)
			.min_by_key(|entry| route_order(&entry.prefix, entry))
			.and_then(|entry| entry.source.clone())
	}

	/// Follow the announcements of the routes covering `path`, reduced to the one serving it.
	///
	/// [`Self::follow`] resolves each instance this reports, and [`Self::request`] concludes
	/// absence from its sync. Fails with [`Error::Unauthorized`] when this consumer's scope can
	/// never cover the path, and with [`Error::InvalidPath`] when no pattern can spell it (a
	/// segment containing `*`).
	pub(crate) fn path_routes(&self, path: impl TryInto<Path, Error: Into<Error>>) -> Result<Follow, Error> {
		let path = path::convert(path)?;

		// Scope down to the path itself, which still sees every route whose claim covers it:
		// the rest of the origin never wakes it, and a route claiming only paths beneath it
		// (a scoped dynamic at the same prefix) never masks the one that serves it, just as a
		// request skips it.
		let consumer = self.scope("", &Patterns::from(Pattern::literal(path.as_str())?))?;

		// `scope` keeps narrower permissions intact: on a consumer limited to
		// `foo/specific`, no route can ever cover `foo`.
		if !consumer.scope.permits(&consumer.root.join(&path)?) {
			return Err(Error::Unauthorized);
		}

		// Untagged: this is a lookup, not egress announce forwarding, so it must not drive
		// the announce guards. Hiding narrows discovery, not lookup, so a hidden path
		// follows like any other.
		let announced = consumer.untagged().with_hidden(true).covering().routes();
		Ok(Follow::new(announced, path))
	}

	/// A resolution that already failed with `err`.
	fn failed(&self, err: Error) -> Requesting {
		let mut request = self.routing(Path::empty(), None);
		request.request = Some(Lookup::failed(err));
		request
	}

	/// Wait for `instance` to stop serving `path`, as a request `pinned` to it or not would
	/// resolve it; see [`Moved`].
	pub(super) fn moved(&self, path: Path, instance: Instance, pinned: bool) -> Moved {
		Moved {
			consumer: self.clone(),
			path,
			instance,
			pinned,
			watch: None,
		}
	}

	/// Resolve a routed instance, returning a selected handler's refusal directly.
	pub(super) fn routing(&self, path: Path, instance: Option<Instance>) -> Requesting {
		// The scope keeps narrower permissions intact: if the whole path is not
		// reachable, no route can ever cover it, so bail rather than loop forever.
		let request = match self.root.join(&path) {
			Ok(named) if self.scope.permits(&named) => None,
			Ok(_) => Some(Lookup::failed(Error::Unauthorized)),
			Err(err) => Some(Lookup::failed(err.into())),
		};
		Requesting {
			consumer: self.clone(),
			path,
			interest: None,
			watch: None,
			request,
			instance,
			discovery: None,
			routed: false,
		}
	}

	/// Follow the broadcast at `path` across the instances that serve it.
	///
	/// The first [`poll_available`](Following::poll_available) resolves the instance serving the path
	/// once one is announced, retrying route churn but returning a selected handler's refusal;
	/// the call after a refusal waits for the serving route to change. Each later call returns
	/// another instance taking over, or `None` once the path goes offline, then whatever serves
	/// it next, even the same instance. Drop what was read from the previous broadcast then:
	/// its tracks end with an error once its routes go. Another route of the same instance,
	/// or another epoch of it, is not a change.
	///
	/// Pass an owned [`Instance`] to follow only that stream: another one taking over ends
	/// it, and the instance is handed out again when it serves the path again. Fails with
	/// [`Error::Unauthorized`] when this consumer's scope can never cover the path, and with
	/// [`Error::InvalidPath`] when no pattern can spell it.
	pub fn follow(
		&self,
		path: impl TryInto<Path, Error: Into<Error>>,
		instance: impl Into<Option<Instance>>,
	) -> Result<Following, Error> {
		let path = path::convert(path)?;
		let routes = self.path_routes(&path)?;
		Ok(Following::new(self.clone(), path, instance.into(), routes))
	}

	/// Returns a consumer rooted at `root` and restricted to matching `patterns`.
	///
	/// `root` is relative to this consumer's root, and `patterns` are relative to
	/// the new root. Returns [`Error::Unauthorized`] when the requested scope has
	/// no overlap with this consumer's scope, or [`Error::BoundsExceeded`] when
	/// rooting the patterns would exceed the path limit.
	pub fn scope(&self, root: impl TryInto<Path, Error: Into<Error>>, patterns: &Patterns) -> Result<Consumer, Error> {
		let root = self.root.join(&path::convert(root)?)?;
		let rooted = patterns.rooted(root.as_str()).map_err(|_| BoundsExceeded)?;
		let scope = self.scope.narrow(&rooted).ok_or(Error::Unauthorized)?;
		Ok(Consumer {
			scope,
			root,
			..self.clone()
		})
	}

	/// Resolve a broadcast by exact path, optionally pinning its [`Instance`].
	///
	/// Returns a [`kio::Pending`] future, mirroring
	/// [`track::Consumer::fetch_group`](track::Consumer::fetch_group). Every
	/// path resolves through a front the origin's [`Driver`] runs: the request
	/// mints one or joins the one already serving the path, and the front picks
	/// one instance's route covering it and materializes it, from the broadcast itself
	/// or from the peer that announced the route. The order is the most specific prefix,
	/// then, within one instance, only its highest epoch is eligible, cheaper or not.
	/// Among instances it is the preferred link ([`Producer::with_preference`]), then
	/// the cheapest, then a broadcast published on this origin, then the shortest hop
	/// chain, then a hash of the instance and the path, which every relay agrees on and
	/// which spreads a pool's paths over its hosts.
	///
	/// When its serving source dies or a better route of the same instance appears,
	/// including a higher epoch, the front switches to it, invisibly to subscribers: the
	/// instance promises one continuous key space. Its routes retracting with no
	/// replacement ends the broadcast, and the next request re-serves the path;
	/// tracks already in flight carry on to their own end. The broadcast also ends
	/// once no handle holds it and none of its tracks has been read for a linger,
	/// and the next request re-serves the path then too.
	///
	/// Another instance winning the path leaves the broadcast to its readers until its
	/// routes go, while the next request resolves the winner on a fresh broadcast.
	/// [`Consumer::routes`] reports that as [`RouteKind::Restart`], and
	/// [`Self::follow`] yields the next broadcast. A job's exact route answering a
	/// claim's request continues the claim's instance, so it is no restart.
	///
	/// Pass an owned [`Instance`] to refuse a different stream, both when requesting
	/// and when an asynchronous answer resolves. `None` leaves lookup unpinned.
	///
	/// The request owns discovery for its path and retries when its covering routes change.
	/// It reports [`Error::Unroutable`] once discovery has synced without a covering route
	/// for the requested instance. A selected handler's explicit refusal is returned as-is.
	/// A broadcast resolved through a discovered route keeps
	/// its path watched until dropped; no separate announcement watcher is needed.
	/// A route claims capability, not inventory: resolving a covered path
	/// succeeds optimistically, and a path that names nothing surfaces as
	/// [`Error::NotFound`] on its tracks instead.
	pub fn request(
		&self,
		path: impl TryInto<Path, Error: Into<Error>>,
		instance: impl Into<Option<Instance>>,
	) -> kio::Pending<Requesting> {
		let path = match path::convert(path) {
			Ok(path) => path,
			Err(err) => return kio::Pending::new(self.failed(err)),
		};
		let mut routing = self.routing(path.clone(), instance.into());
		match self.path_routes(&path) {
			Ok(discovery) => {
				routing.discovery = Some(discovery);
				if let Some(resolved) = self.root.join(&path).ok().and_then(|named| self.scope.resolve(&named)) {
					let mut table = self.shared.lock();
					let watch = table.watch(&self.shared, &resolved);
					let seen = watch.seen();
					drop(table);
					routing.watch = Some((watch, seen));
				}
				// A requests handler hears an already-routed request at creation time.
				routing.request = Some(self.lookup(path, routing.instance.as_ref()).into_inner());
			}
			Err(err) => routing.request = Some(Lookup::failed(err)),
		}
		kio::Pending::new(routing)
	}

	/// The routes as they stand now, without discovery or retry: what a session answers a
	/// peer's request with. Refused with [`Error::Unroutable`] unless the winning route serves
	/// `instance`: a peer asking for one stream must not be handed another.
	pub(crate) fn lookup(&self, path: Path, instance: Option<&Instance>) -> kio::Pending<Lookup> {
		// The path as this handle names it, absolute: what its scope authorizes and
		// what its egress is counted under, so a read through a mount bills where
		// the reader asked.
		let named = match self.root.join(&path) {
			Ok(named) => named,
			Err(err) => return kio::Pending::new(Lookup::failed(err.into())),
		};
		let scope = self.stats.egress(&named);
		// The resolved handle is named by what *this* cursor asked for, not by the absolute
		// path: a rooted cursor cannot name anything above its own root, so that is what a
		// catalog it reads may reference.
		let requested = path;

		// Routes only cover paths within this consumer's scope.
		if !self.scope.permits(&named) {
			return kio::Pending::new(Lookup::failed(Error::Unauthorized));
		}

		// Key requests by the absolute path on the origin, past any mount, so scoped,
		// rooted, and mounted consumers and handlers agree on the same entry and front.
		let Some(absolute) = self.scope.resolve(&named) else {
			return kio::Pending::new(Lookup::failed(BoundsExceeded.into()));
		};

		let mut state = self.shared.lock();

		// The origin's driver dropped: nothing will ever serve this.
		if state.closed {
			return kio::Pending::new(Lookup::failed(Error::Closed));
		}

		let horizon = self.horizon.effective(&state.routes, &absolute);

		// Nothing serves the path: no announced broadcast and no served route.
		// Checked before joining a front, so a front still draining after its
		// route retracted takes no newcomers.
		let Some(winner) = state
			.best_route(&absolute, horizon, &HashSet::new(), |_| true)
			.map(|best| best.instance.clone())
		else {
			return kio::Pending::new(Lookup::failed(Error::Unroutable));
		};
		// A request naming another instance than the winner follows that instance's own
		// routes: every hop resolves the instance the first one chose.
		let pin = instance.filter(|instance| **instance != winner).cloned();
		let best = match &pin {
			None => winner,
			Some(pin)
				if state
					.best_route(&absolute, horizon, &HashSet::new(), |entry| entry.instance == *pin)
					.is_some() =>
			{
				pin.clone()
			}
			Some(_) => return kio::Pending::new(Lookup::failed(Error::Unroutable)),
		};

		// Join the live front for this path and horizon, if any: its watcher
		// resolves (or already resolved) the request channel with its verdict, so
		// repeat requests share one upstream subscription. A front resolved on
		// another instance stays only for the subscriptions already on it, so a new
		// request mints a fresh front for the winner rather than join the old one. So
		// does a front still resolving one the table has since beaten: it may already
		// hold that instance's source, and hand this request an instance it saw lose.
		let key = (absolute.clone(), horizon, pin.clone());
		if let Some(front) = state.fronts.get(&key) {
			let current = {
				let pending = front.request.read();
				match &pending.resolved {
					None => pending.selected == best,
					Some(Ok(resolved)) => *resolved == best,
					Some(Err(_)) => false,
				}
			};
			// Held from here on, so the front cannot retire under this requester. A
			// retiring front closes atomically with this mint: found closed, it is
			// ending, and the request mints a fresh one instead.
			let held = current
				.then(|| front.broadcast.consume())
				.filter(|held| !held.is_closed());
			if let Some(held) = held {
				let pending = Lookup::queued(front.request.consume(), held)
					.with_path(requested)
					.with_stats(scope)
					.with_instance(instance.cloned());
				return kio::Pending::new(pending);
			}
			state.fronts.remove(&key);
		}

		// A route covers the path: mint the front and hand its watcher the
		// request. The watcher materializes the path from the best covering
		// route, resolves the channel, and switches the front between routes for
		// as long as one serves.
		let broadcast = broadcast::Producer::new(broadcast::Info {
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			path: absolute.clone(),
			instance: None,
			// A front reads its routes' copies, so it writes no frames of its own.
			frames: Default::default(),
		});
		let request = kio::Producer::new(PendingFront {
			resolved: None,
			selected: best,
			refused: false,
		});
		let consumer = request.consume();
		// The front starts held by this request; see [`Front::new`].
		let held = broadcast.consume();
		let watch = state.watch(&self.shared, &absolute);
		state.fronts.insert(
			key,
			RemoteFront {
				request: request.clone(),
				broadcast: held.weak(),
			},
		);
		// Released before the push: a set whose handles are gone drops the task,
		// and the `Watch` it carries unregisters under this same lock.
		drop(state);
		self.tasks.push(FrontServeRun::new(
			FrontTask {
				shared: self.shared.clone(),
				broadcast,
				path: absolute,
				horizon,
				pin,
				watch,
				request,
				timers: self.timers.clone(),
			},
			self.tasks.clone(),
		));
		kio::Pending::new(
			Lookup::queued(consumer, held)
				.with_path(requested)
				.with_stats(scope)
				.with_instance(instance.cloned()),
		)
	}

	/// Returns the prefix that is automatically stripped from all paths.
	pub fn root(&self) -> &Path {
		&self.root
	}

	/// The patterns this consumer may reach, relative to its root.
	pub fn allowed(&self) -> Patterns {
		self.scope.relative(&self.root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl TryInto<Path, Error: Into<Error>>) -> Result<Path, Error> {
		Ok(self.root.join(&path::convert(path)?)?)
	}
}

/// Receives every route change for a scope, for relays and stats.
///
/// Created by [`Consumer::routes`].
/// Drop to unregister.
pub struct Routes {
	// One registration per table cursor feeding `state`: more than one when the
	// consumer reads through a mount.
	ids: Vec<ConsumerId>,
	shared: SendCell<kio::Shared<OriginState>>,
	root: Path,

	// Pending updates queued for this cursor. Coalesced so a slow consumer
	// can't accumulate redundant announce/retract pairs.
	state: kio::Producer<OriginConsumerState>,

	// Egress stats context (empty for an untagged stream). Announce events drive the
	// per-prefix announce guards below.
	stats: stats::Session,

	// Live egress announce guards, keyed by absolute prefix. An announce
	// opens one (bumping `announces_started` + `announced_bytes`); the matching retraction
	// drops it (bumping `announces_ended` + `announced_bytes`).
	guards: HashMap<Path, stats::Announce>,

	// Holds the waiter a `Stream` poll registered; disjoint from `state` so the
	// borrow never collides with the body's.
	#[cfg(feature = "async")]
	park: kio::Park,

	// Held updates (see [`Config::update_hold`]): the hold, the clock it counts
	// on, and the timer that wakes the next.
	update_hold: Duration,
	// Quarantines by prefix, and their retractions in clock order: first waiting
	// for a fresh reading (by the stale one), then quarantined (by when each ends).
	retracted: HashMap<Path, Quarantine>,
	anchoring: VecDeque<(Instant, Path)>,
	expiring: VecDeque<(Instant, Path)>,
	timers: Deadlines,
	hold: crate::time::Deadline,
}

impl Routes {
	#[cfg(test)]
	async fn expect_next(&mut self) -> RouteEvent {
		#[cfg(feature = "async")]
		{
			return kio::wait(|waiter| self.poll_next(waiter)).await.expect("no next");
		}
		#[cfg(not(feature = "async"))]
		{
			kio::wait(|waiter| self.poll_next(waiter))
				.now_or_never()
				.expect("next should not block")
				.expect("no next")
		}
	}

	#[cfg(test)]
	pub(crate) async fn expect_next_active(
		&mut self,
		expected: impl TryInto<Path, Error: std::fmt::Debug>,
	) -> RouteInfo {
		let event = self.expect_next().await;
		Self::active(event, &expected.try_into().expect("expected path"))
	}

	#[cfg(test)]
	pub(crate) async fn expect_next_restarted(
		&mut self,
		expected: impl TryInto<Path, Error: std::fmt::Debug>,
	) -> RouteInfo {
		match self.expect_next().await {
			RouteEvent {
				kind: RouteKind::Restart,
				announce,
			} => {
				assert_eq!(
					announce.prefix,
					expected.try_into().expect("expected path"),
					"wrong prefix"
				);
				announce.route
			}
			other => panic!("should be a restart: got {other:?}"),
		}
	}

	#[cfg(test)]
	pub(crate) async fn expect_next_ended(&mut self, expected: impl TryInto<Path, Error: std::fmt::Debug>) {
		match self.expect_next().await {
			RouteEvent {
				kind: RouteKind::End,
				announce,
			} => assert_eq!(
				announce.prefix,
				expected.try_into().expect("expected path"),
				"wrong prefix"
			),
			other => panic!("should be a retraction: got {other:?}"),
		}
	}

	#[cfg(test)]
	pub(crate) async fn expect_next_offline(&mut self, expected: impl TryInto<Path, Error: std::fmt::Debug>) {
		match self.expect_next().await {
			RouteEvent {
				kind: RouteKind::Offline,
				announce,
			} => assert_eq!(
				announce.prefix,
				expected.try_into().expect("expected path"),
				"wrong prefix"
			),
			other => panic!("should go offline: got {other:?}"),
		}
	}

	fn new(
		root: Path,
		cursors: Vec<TableCursor>,
		state: kio::Producer<OriginConsumerState>,
		stats: stats::Session,
		shared: &SendCell<kio::Shared<OriginState>>,
		timers: Deadlines,
	) -> Self {
		let mut ids = Vec::with_capacity(cursors.len());
		let update_hold;
		{
			let mut table = shared.lock();
			update_hold = table.update_hold;
			if table.closed {
				// A cursor on a dead origin is born ended.
				if let Ok(mut state) = state.write() {
					state.ended = true;
				}
			} else {
				for cursor in cursors {
					let id = ConsumerId::new();
					table.register_cursor(shared, id, cursor);
					ids.push(id);
				}
			}
			// Under the table lock, so no snapshot lands between the count and this.
			if let Ok(mut state) = state.write()
				&& state.waiting.load(Ordering::Relaxed) == 0
			{
				state.owe();
			}
		}

		Self {
			ids,
			shared: shared.clone(),
			root,
			state,
			stats,
			guards: HashMap::new(),
			#[cfg(feature = "async")]
			park: kio::Park::default(),
			update_hold,
			retracted: HashMap::new(),
			anchoring: VecDeque::new(),
			expiring: VecDeque::new(),
			hold: crate::time::Deadline::new(&timers),
			timers,
		}
	}

	/// Start a retraction's quarantine. A delivered announcement leaves it to
	/// expire: later ones for the prefix are updates, which it never holds.
	fn quarantine(&mut self, event: &RouteEvent, now: Instant) {
		let RouteEvent {
			kind: RouteKind::End | RouteKind::Offline,
			announce: update,
		} = event
		else {
			return;
		};
		if self.update_hold.is_zero() {
			return;
		}
		// An anonymous route names no publisher, so its quarantine holds nothing.
		let publisher = update
			.route
			.hops
			.iter()
			.next()
			.copied()
			.filter(|hop| *hop != Hop::UNKNOWN);
		// The reading may be stale; [`Self::settle`] starts the quarantine on a fresh one.
		let quarantine = Quarantine {
			held: Held::Anchoring(now),
			publisher,
			instance: (
				update
					.route
					.instance
					.clone()
					.expect("a delivered route has an instance"),
				update.route.epoch,
			),
			rank: (update.route.cost, update.route.hops.len()),
		};
		let replaced = self.retracted.insert(update.prefix.clone(), quarantine);
		// A prefix retracted again at the same reading is already queued, so churn
		// between clock readings can't grow the queue past one entry per prefix.
		if !replaced.is_some_and(|q| q.held == Held::Anchoring(now)) {
			self.anchoring.push_back((now, update.prefix.clone()));
		}
	}

	/// Start the quarantines of retractions the clock has since moved past, and
	/// forget ended ones. Returns when to wake to read the clock again, if any remain.
	fn settle(&mut self, now: Instant) -> Option<Instant> {
		// Both queues are in clock order, so only their fronts can be due. An entry
		// whose prefix was since announced or retracted again no longer matches.
		while let Some((seen, prefix)) = self.anchoring.front() {
			if *seen >= now {
				break;
			}
			if let Some(quarantine) = self.retracted.get_mut(prefix)
				&& quarantine.held == Held::Anchoring(*seen)
			{
				quarantine.held = Held::Until(now.saturating_add(RETRACT_QUARANTINE));
				self.expiring
					.push_back((now.saturating_add(RETRACT_QUARANTINE), prefix.clone()));
			}
			self.anchoring.pop_front();
		}
		while let Some((until, prefix)) = self.expiring.front() {
			if *until > now {
				break;
			}
			if self
				.retracted
				.get(prefix)
				.is_some_and(|q| q.held == Held::Until(*until))
			{
				self.retracted.remove(prefix);
			}
			self.expiring.pop_front();
		}
		let anchoring = self
			.anchoring
			.front()
			.map(|(seen, _)| seen.saturating_add(Duration::from_nanos(1)));
		earliest(anchoring, self.expiring.front().map(|(until, _)| *until))
	}

	/// Drive the egress announce guards for one update.
	fn hand_out(&mut self, event: RouteEvent) -> RouteEvent {
		match &event {
			RouteEvent {
				kind: RouteKind::Start | RouteKind::Update | RouteKind::Restart,
				announce,
			} => {
				let scope = self.stats.egress(self.root.rejoin(&announce.prefix));
				self.guards
					.entry(announce.prefix.clone())
					.or_insert_with(|| scope.announce());
			}
			RouteEvent {
				kind: RouteKind::End | RouteKind::Offline,
				announce,
			} => {
				self.guards.remove(&announce.prefix);
			}
		}
		event
	}

	/// The next route announcement, update, or retraction, its prefix relative to
	/// this cursor's root, or `None` once the cursor is closed.
	///
	/// A retraction is only delivered for a previously announced prefix, and a
	/// repeated announcement for the same prefix is a metadata update. With the
	/// `async` feature, the consumer is also a `futures::Stream` of the same events.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<RouteEvent>> {
		loop {
			let now = self.timers.now();
			let settling = self.settle(now);
			let mut wake = None;
			// Decided without touching the queue: parking what it holds back is a write,
			// which would wake whoever waits on it, this poll included.
			let mut state = match self.state.poll(waiter, |state| match state.due(now) {
				true => Poll::Ready(()),
				false => {
					wake = state.wake();
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(state)) => state,
				// Closed: discard the Ref so its MutexGuard doesn't escape this call.
				Poll::Ready(Err(_)) => return Poll::Ready(None),
				Poll::Pending => {
					self.hold.set(earliest(wake, settling));
					ready!(self.hold.poll(waiter));
					continue;
				}
			};
			if state.pending.is_empty() {
				// Ended by the origin's teardown, pending updates already drained; close
				// the channel so every closure signal agrees.
				state.close();
				return Poll::Ready(None);
			}
			// Whatever was due may all be held, parked until a later poll.
			let Ok(prefix) = state.scan(&self.retracted, now, self.update_hold) else {
				continue;
			};
			let event = state.take_prefix(prefix);
			drop(state);
			self.quarantine(&event, now);
			return Poll::Ready(Some(self.hand_out(event)));
		}
	}

	/// Returns the next event without blocking.
	///
	/// Returns None if there is no event available; NOT because the cursor is closed.
	/// Use [`Self::is_closed`] to check if the cursor is closed.
	pub fn try_next(&mut self) -> Option<RouteEvent> {
		let now = self.timers.now();
		let settling = self.settle(now);
		// Read first, so a call with nothing due writes nothing; see `poll_next`.
		let idle = {
			let state = self.state.read();
			(!state.due(now) || state.pending.is_empty()).then(|| state.wake())
		};
		let scanned = match idle {
			Some(wake) => Err(wake),
			None => {
				let mut state = self.state.write().ok()?;
				state
					.scan(&self.retracted, now, self.update_hold)
					.map(|prefix| state.take_prefix(prefix))
			}
		};
		let event = match scanned {
			Ok(event) => event,
			Err(wake) => {
				// Arm the hold even though nothing polls it: the armed timer wakes the
				// driver, so its clock reaches the deadline for a later call to see.
				self.hold.set(earliest(wake, settling));
				return None;
			}
		};
		self.quarantine(&event, now);
		Some(self.hand_out(event))
	}

	/// Returns true if the cursor is closed (no more updates will arrive).
	pub fn is_closed(&self) -> bool {
		let state = self.state.read();
		state.is_closed() || state.ended
	}

	/// Whether this consumer has synced; see [`Self::poll_synced`].
	#[cfg(test)]
	pub(crate) fn is_synced(&self) -> bool {
		let state = self.state.read();
		state.is_closed() || state.synced()
	}

	/// Wait until this consumer has synced with what its sources announced.
	///
	/// Synced means every session feeding this scope has delivered its peer's
	/// announcements as of when it answered, and this consumer has read every event
	/// queued before then. A prefix not announced by then is not announced, rather
	/// than not heard yet. A session joining the scope later, such as a reconnect,
	/// takes this back until it syncs too; routes published on this origin
	/// itself never wait.
	///
	/// Synced is relative to the session's peer, never global: a relay answers with
	/// what it holds without waiting on its own upstreams. Versions that cannot mark
	/// the boundary (moq-lite-03/04, and moq-transport without MoQ Active Count) catch
	/// up once the peer accepts the request, or once it is sent when nothing answers.
	///
	/// Level-triggered: ready whenever synced, and once the cursor is closed.
	pub fn poll_synced(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state
			.poll(waiter, |state| match state.synced() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
			.map(|_| ())
	}

	// The wire sender uses this only to bound its initial replay.
	#[cfg(feature = "async")]
	pub(crate) fn pending_len(&self) -> usize {
		self.state.read().pending.len()
	}

	// A wider wire grant must reuse pending slots, rather than bypass the initial queue.
	#[cfg(feature = "async")]
	pub(crate) fn replay<'a>(&mut self, advertised: impl IntoIterator<Item = &'a Path>, revoked: &HashSet<Path>) {
		let advertised: HashSet<_> = advertised.into_iter().collect();
		let table = self.shared.lock();
		let Ok(mut state) = self.state.write() else {
			return;
		};
		for id in &self.ids {
			let Some(cursor) = table.cursors.get(id) else {
				continue;
			};
			for (presented, CursorRoute { meta, captures, .. }) in &cursor.current {
				let prefix = cursor.under.rejoin(presented);
				if revoked.contains(&prefix) {
					state.apply_unannounce(prefix, meta.clone(), captures.clone(), Gone::End);
					continue;
				}
				if advertised.contains(&prefix) {
					if state.pending.get(&prefix).is_some_and(|pending| {
						matches!(
							pending.update,
							Some(PendingUpdate::Unannounce(..) | PendingUpdate::UnannounceAnnounce { .. })
						)
					}) {
						state.apply_announce(prefix, meta.clone(), captures.clone());
					}
					continue;
				}
				// The wire never sent this route, so it is a fresh start without a hold.
				state.delivered.remove(&prefix);
				self.retracted.remove(&prefix);
				state.put(prefix, PendingUpdate::Announce((meta.clone(), captures.clone())));
			}
		}
	}

	/// Returns the root that is automatically stripped from emitted prefixes.
	pub fn root(&self) -> &Path {
		&self.root
	}

	/// Converts an emitted prefix back to one rooted at the origin.
	pub fn absolute(&self, prefix: impl TryInto<Path, Error: Into<Error>>) -> Result<Path, Error> {
		Ok(self.root.join(&path::convert(prefix)?)?)
	}
}

#[cfg(feature = "async")]
impl futures::Stream for Routes {
	type Item = RouteEvent;

	fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
		let this = self.get_mut();
		let waiter = this.park.hold(cx).clone();
		this.poll_next(&waiter)
	}
}

impl Drop for Routes {
	fn drop(&mut self) {
		let mut state = self.shared.lock();
		for id in &self.ids {
			if let Some(cursor) = state.cursors.remove(id) {
				for head in &cursor.heads {
					state.routes.remove_cursor(head, *id);
					if cursor.interested {
						state.remove_interest(&self.shared, head);
					}
				}
			}
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[expect(missing_docs, reason = "test-only assertion helpers")]
impl Routes {
	/// The route of an active event at `expected`.
	fn active(event: RouteEvent, expected: &Path) -> RouteInfo {
		match event {
			RouteEvent {
				kind: RouteKind::Start | RouteKind::Update | RouteKind::Restart,
				announce,
			} => {
				assert_eq!(announce.prefix, *expected, "wrong prefix");
				announce.route
			}
			other => panic!("should be an active route: got {other:?}"),
		}
	}

	/// The next update must be an active route at `expected`; returns it.
	pub fn assert_next_active(&mut self, expected: impl TryInto<Path, Error: std::fmt::Debug>) -> RouteInfo {
		let event = kio::wait(|waiter| self.poll_next(waiter))
			.now_or_never()
			.expect("next blocked")
			.expect("no next");
		Self::active(event, &expected.try_into().expect("expected path"))
	}

	/// The `try_next` counterpart of [`Self::assert_next_active`].
	pub fn assert_try_next_active(&mut self, expected: impl TryInto<Path, Error: std::fmt::Debug>) -> RouteInfo {
		let event = self.try_next().expect("no next");
		Self::active(event, &expected.try_into().expect("expected path"))
	}

	/// The next update must be a restart at `expected`; returns its route.
	pub fn assert_next_restarted(&mut self, expected: impl TryInto<Path, Error: std::fmt::Debug>) -> RouteInfo {
		match kio::wait(|waiter| self.poll_next(waiter))
			.now_or_never()
			.expect("next blocked")
			.expect("no next")
		{
			RouteEvent {
				kind: RouteKind::Restart,
				announce,
			} => {
				assert_eq!(
					announce.prefix,
					expected.try_into().expect("expected path"),
					"wrong prefix"
				);
				announce.route
			}
			other => panic!("should be a restart: got {other:?}"),
		}
	}

	/// The next update must be a retraction at `expected`.
	pub fn assert_next_ended(&mut self, expected: impl TryInto<Path, Error: std::fmt::Debug>) {
		match kio::wait(|waiter| self.poll_next(waiter))
			.now_or_never()
			.expect("next blocked")
			.expect("no next")
		{
			RouteEvent {
				kind: RouteKind::End,
				announce,
			} => assert_eq!(
				announce.prefix,
				expected.try_into().expect("expected path"),
				"wrong prefix"
			),
			other => panic!("should be a retraction: got {other:?}"),
		}
	}

	pub fn assert_next_wait(&mut self) {
		if let Some(event) = kio::wait(|waiter| self.poll_next(waiter)).now_or_never() {
			panic!("next should block: got {event:?}");
		}
	}
}

/// Test-only construction shorthand: build the producer and spawn its driver on
/// the test executor, mirroring what `moq_tokio::origin::spawn` does for
/// applications.
#[cfg(test)]
pub(crate) trait ProduceTest {
	fn produce(self) -> Producer;
}

#[cfg(test)]
impl ProduceTest for Config {
	fn produce(self) -> Producer {
		#[cfg(not(feature = "async"))]
		let self_ = Self {
			update_hold: Duration::ZERO,
			..self
		};
		#[cfg(feature = "async")]
		let self_ = self;
		let (producer, driver) = Producer::new(self_);
		#[cfg(feature = "async")]
		if kio::clock::sim::is_running() {
			kio::clock::sim::spawn(crate::time::run(driver));
			return producer;
		}
		{
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
		Config::from(self).produce()
	}
}

/// Test-only: an origin's config for a local hop.
#[cfg(test)]
impl From<Hop> for Config {
	fn from(hop: Hop) -> Self {
		Self::new(NonZeroU64::new(hop.id).expect("a local hop is non-zero"))
	}
}

/// Test-only: one peer's route selected for `room/a`, which a test withdraws or
/// replaces to drive the [`broadcast::RequestValidity`](crate::model::broadcast::RequestValidity)
/// it hands out.
#[cfg(all(test, feature = "async"))]
pub(crate) struct TestSelection {
	origin: Producer,
	route: Option<Claim>,
	replacement: Option<Claim>,
}

#[cfg(all(test, feature = "async"))]
impl TestSelection {
	const PEER: u64 = 2;

	pub(crate) fn new() -> Self {
		let origin = Hop::new(1).unwrap().produce();
		let route = origin
			.forward("room", RouteInfo::new().with_via(Hop::new(Self::PEER).unwrap()))
			.unwrap();
		Self {
			origin,
			route: Some(route),
			replacement: None,
		}
	}

	/// What a request for `room/a` through the selected route must still hold.
	pub(crate) fn validity(&self) -> crate::model::broadcast::RequestValidity {
		crate::model::broadcast::RequestValidity::for_source(self.route.as_ref().unwrap(), &Path::from_static("room/a"))
	}

	/// The peer withdraws the route.
	pub(crate) fn withdraw(&mut self) {
		self.route = None;
	}

	/// Another instance of the peer takes the path.
	pub(crate) fn replace(&mut self) {
		let route = RouteInfo::new().with_via(Hop::new(Self::PEER).unwrap());
		self.replacement = Some(self.origin.forward("room/a", route).unwrap());
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn max_hops_caps_at_the_wire_bound() {
		assert_eq!(Config::default().max_hops.get(), MAX_HOPS);
		for (max, effective) in [(1, 1), (6, 6), (32, 32), (33, 32), (usize::MAX, 32)] {
			let config = Config {
				max_hops: NonZeroUsize::new(max).unwrap(),
				..Default::default()
			};
			let (producer, _driver) = Producer::new(config);
			let scoped = producer.scope("room", &Patterns::from(Pattern::all())).unwrap();
			assert_eq!(scoped.config().max_hops.get(), effective);
			assert_eq!(
				scoped.consume().forwardable().horizon.max_hops.map(NonZeroUsize::get),
				Some(effective)
			);
		}
	}
	#[cfg(feature = "async")]
	use crate::test_tracing::count_drop_warnings;
	use futures::FutureExt;
	use std::sync::atomic::AtomicBool;

	fn origin(id: u64) -> Hop {
		Hop::new(id).unwrap()
	}

	/// One publisher instance, shared by every route in a test that resumes across them.
	fn instance() -> crate::Instance {
		"01900000-0000-7000-8000-000000000001".parse().unwrap()
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

	/// A track whose outcome is already an error still serves its finished history through
	/// a front: learning the info and the outcome in one driver poll must not refuse it.
	#[test]
	fn an_errored_copy_serves_history_before_its_outcome() {
		let now = Instant::ZERO;
		let waiter = kio::Waiter::noop();
		let (producer, mut driver) = Producer::new(Config::from(origin(1)));
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.reserve_track("video").unwrap();

		let mut requesting = producer.consume().request("room", None);
		driver.poll(now, &waiter).unwrap();
		let Poll::Ready(Some(request)) = server.poll_requested(&waiter) else {
			panic!("the route receives the request");
		};
		request.serve(&broadcast);
		driver.poll(now, &waiter).unwrap();
		let Poll::Ready(Ok(remote)) = requesting.poll_ok(&waiter) else {
			panic!("the broadcast resolves");
		};
		let subscribing = remote.track("video").unwrap().subscribe(None);
		driver.poll(now, &waiter).unwrap();
		assert!(subscribing.poll_ok(&waiter).is_pending());

		// The query is already in flight when metadata and the abort arrive together.
		let track = track.accept(track::Info::timed());
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"history".as_ref()).unwrap();
		group.close().unwrap();
		track.abort(Error::App(7)).unwrap();
		driver.poll(now, &waiter).unwrap();
		let Poll::Ready(Ok(mut subscriber)) = subscribing.poll_ok(&waiter) else {
			panic!("the errored copy's info resolves");
		};
		let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("the finished history is still readable");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("the history's frame is still readable");
		};
		assert_eq!(frame.payload.as_ref(), b"history");
		assert!(matches!(
			subscriber.poll_recv_group(&waiter),
			Poll::Ready(Err(Error::App(7)))
		));
	}

	/// Ending a logical track must finish its metadata producer as well as its routes,
	/// without losing the source's final group or warning about an unfinished writer.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_finished_logical_track_does_not_warn() {
		let warns = count_drop_warnings("track::Producer dropped without finish", || {
			let broadcast = Arc::new(broadcast::Info::default());
			let source = track::Producer::new(broadcast.clone(), "catalog.json", crate::track::Info::timed());
			let mut group = source.create_group(7u64.into()).unwrap();
			group.write_frame(group.key_time(), b"snapshot".as_ref()).unwrap();
			group.close().unwrap();
			let source = source.finish().unwrap();

			let routes = crate::model::resume::Producer::new();
			routes.serve(source.consume());
			let request = track::Request::new(broadcast, "catalog.json");
			let consumer = request.consume();
			let weak = request.weak();
			let accepted = request.routes(routes.consume()).accept(None);
			let mut io = TrackIo {
				ready: crate::util::QueueWake::new("catalog.json".into(), kio::Queue::new()),
				park: Default::default(),
				logical: Logical::Accepted(accepted),

				weak,
				routes,
				query: None,
				staged: None,
				copy: None,
				held: None,
				wanted: true,
			};
			let mut subscriber = consumer.subscribe(None).now_or_never().unwrap().unwrap();
			io.end(Ok(()), None);

			let mut group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(group.start, 7);
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.payload.as_ref(), b"snapshot");
			assert!(subscriber.recv_group().now_or_never().unwrap().unwrap().is_none());
		});
		assert_eq!(warns, 0, "a clean logical end must not drop an unfinished producer");
	}

	#[kio::test]
	async fn default_config_mints_a_real_hop() {
		let config = Config::default();
		assert!(config.hop.get() < 1 << 53, "older JS clients decode it as a number");
		let (producer, _driver) = Producer::new(config.clone());
		assert_eq!(producer.hop(), config.hop);
		assert_eq!(producer.consume().hop(), config.hop);
	}

	#[kio::test]
	async fn random_hops_fit_legacy_lite_clients() {
		for _ in 0..32 {
			assert!(Hop::random().id() < 1u64 << 53);
		}
	}

	/// Yield to the driver until `check` passes, bounded so a bug fails instead
	/// of hanging.
	#[cfg(feature = "async")]
	async fn settle(mut check: impl FnMut() -> bool) {
		for _ in 0..100 {
			if check() {
				return;
			}
			kio::clock::sim::yield_now().await;
		}
		panic!("condition never settled");
	}

	/// Yield to the driver until the server's front watcher delivers a request.
	#[cfg(feature = "async")]
	async fn queued(server: &Claim) -> Request {
		let mut request = None;
		settle(|| match server.poll_requested(&kio::Waiter::noop()) {
			Poll::Ready(Some(popped)) => {
				request = Some(popped);
				true
			}
			_ => false,
		})
		.await;
		request.unwrap()
	}

	/// Yield to the driver until the subscription has its next group or its end.
	#[cfg(feature = "async")]
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

	#[kio::test]
	async fn announce_and_retract() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();
		announced.assert_next_wait();

		let announcement = producer.announce("room/alice", RouteInfo::new()).unwrap();
		let route = announced.expect_next_active("room/alice").await;
		assert!(route.hops.is_empty());
		assert_eq!(route.cost, Cost::default());
		announced.assert_next_wait();

		drop(announcement);
		announced.expect_next_ended("room/alice").await;
		announced.assert_next_wait();
	}

	/// A consumer is synced once a snapshot lands and it has read what came before.
	#[test]
	fn synced_after_the_snapshot_and_its_events() {
		let producer = origin(1).produce();
		let snapshot = producer.snapshot("room");
		let mut announced = producer.consume().routes();
		assert!(!announced.is_synced(), "a snapshot in flight holds it");

		let _alice = producer.announce("room/alice", RouteInfo::new()).unwrap();
		drop(snapshot);
		assert!(!announced.is_synced(), "never before an earlier event is read");

		announced.assert_next_active("room/alice");
		assert!(announced.is_synced());

		// A later announcement is news, not part of what it synced with.
		let _bob = producer.announce("room/bob", RouteInfo::new()).unwrap();
		assert!(announced.is_synced());
	}

	/// Without any session feeding it, a consumer syncs on its own replay.
	#[test]
	fn a_local_origin_catches_up_on_its_replay() {
		let producer = origin(1).produce();
		let mut empty = producer.consume().routes();
		assert!(empty.is_synced(), "nothing to wait on");
		empty.assert_next_wait();

		let _alice = producer.announce("room/alice", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		assert!(!announced.is_synced(), "the replay is owed");
		announced.assert_next_active("room/alice");
		assert!(announced.is_synced());
	}

	#[test]
	fn initial_routes_precede_live_prefixes() {
		let producer = origin(1).produce();
		let _initial = producer.announce("z/initial", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		let _live = producer.announce("a/live", RouteInfo::new()).unwrap();
		announced.assert_next_active("z/initial");
		assert!(announced.is_synced());
	}

	#[test]
	fn stalled_initial_replay_collapses_churn_without_a_backlog() {
		let producer = origin(1).produce();
		let initial = producer.announce("z/initial", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		for i in 0..1_000 {
			let transient = producer.announce(format!("a/{i}"), RouteInfo::new()).unwrap();
			initial.update(RouteInfo::new().with_cost(i)).unwrap();
			drop(transient);
			assert_eq!(announced.state.read().pending.len(), 1);
		}
		announced.assert_next_active("z/initial");
		assert!(announced.is_synced());
		announced.assert_next_wait();
	}

	/// A start and end that cancel before being read are owed nothing.
	#[test]
	fn a_cancelled_start_still_catches_up() {
		let producer = origin(1).produce();
		let snapshot = producer.snapshot("");
		let mut announced = producer.consume().routes();

		let alice = producer.announce("room/alice", RouteInfo::new()).unwrap();
		drop(snapshot);
		assert!(!announced.is_synced());
		drop(alice);
		assert!(announced.is_synced(), "the pair cancelled");
		announced.assert_next_wait();
	}

	/// A re-price of a route already read is not owed: the set of prefixes is unchanged.
	#[test]
	fn an_update_is_not_owed() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let alice = producer.announce("room/alice", RouteInfo::new()).unwrap();
		announced.assert_next_active("room/alice");

		let snapshot = producer.snapshot("");
		alice.update(RouteInfo::new().with_cost(5)).unwrap();
		drop(snapshot);
		assert!(announced.is_synced());
	}

	/// A snapshot taken after syncing (a reconnect) holds it again until it lands.
	#[test]
	fn a_new_snapshot_takes_sync_back() {
		let producer = origin(1).produce();
		let announced = producer.consume().routes();
		assert!(announced.is_synced());

		let snapshot = producer.snapshot("room/alice");
		assert!(!announced.is_synced(), "a snapshot beneath the scope holds it");
		let second = producer.snapshot("");
		drop(snapshot);
		assert!(!announced.is_synced(), "every current snapshot must land");
		drop(second);
		assert!(announced.is_synced());
	}

	/// Only snapshots that could present on a consumer hold it.
	#[test]
	fn only_overlapping_snapshots_hold() {
		let producer = origin(1).produce();
		let room = producer
			.consume()
			.scope("room", &Patterns::from(Pattern::all()))
			.unwrap();

		let other = producer.snapshot("other");
		assert!(room.routes().is_synced(), "a disjoint prefix");

		let above = producer.snapshot("");
		let announced = room.routes();
		assert!(!announced.is_synced(), "a snapshot above the root covers it");
		drop(above);
		assert!(announced.is_synced());

		drop(other);

		// A local view never waits on a peer's routes.
		let _peer = producer.clone().with_link(Link::Peer).snapshot("room");
		assert!(producer.consume().local().routes().is_synced());
		assert!(!producer.consume().routes().is_synced());
	}

	/// A source shadowed by a mount can present nothing on the handle, so it never holds it.
	#[test]
	fn a_shadowed_snapshot_never_holds() {
		let producer = origin(1).produce();
		let project = mounted(&producer, "p1", &["**"]).with_hidden(true);
		let announced = project.routes();
		assert!(announced.is_synced());

		let shadowed = producer.snapshot("p1/.svc");
		assert!(announced.is_synced(), "a snapshot in flight under the hole");
		assert!(project.routes().is_synced(), "a reader registering past one");
		drop(shadowed);

		let _target = producer.snapshot(".svc/p1");
		assert!(!announced.is_synced(), "the mount's target does hold it");
	}

	/// A source under a hidden segment holds only the readers that opt into hidden routes.
	#[test]
	fn a_hidden_snapshot_holds_only_hidden_readers() {
		let producer = origin(1).produce();
		let announced = producer.consume().routes();
		let _stats = producer.snapshot(".stats");
		assert!(announced.is_synced(), "a snapshot in flight under a hidden segment");
		assert!(producer.consume().routes().is_synced(), "a reader registering past one");
		assert!(!producer.consume().with_hidden(true).routes().is_synced());
	}

	/// A waiter parked on syncing is woken when it does.
	#[test]
	fn catching_up_wakes_the_waiter() {
		struct Woken(AtomicBool);
		impl Wake for Woken {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let producer = origin(1).produce();
		let snapshot = producer.snapshot("");
		let announced = producer.consume().routes();
		let woken = Arc::new(Woken(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(std::task::Waker::from(woken.clone()));
		assert!(announced.poll_synced(&waiter).is_pending());
		drop(snapshot);
		assert!(woken.0.load(Ordering::SeqCst), "not woken");
		assert!(announced.poll_synced(&waiter).is_ready());
	}

	/// A session joining or landing short of the last one wakes no parked cursor, so a
	/// relay's announce feeds stay idle while peers connect.
	#[test]
	fn holding_back_wakes_nobody() {
		struct Woken(AtomicBool);
		impl Wake for Woken {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let woken = Arc::new(Woken(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(std::task::Waker::from(woken.clone()));
		assert!(announced.poll_next(&waiter).is_pending());
		assert!(announced.poll_synced(&waiter).is_ready());

		let first = producer.snapshot("");
		let second = producer.snapshot("room");
		drop(first);
		assert!(!woken.0.load(Ordering::SeqCst), "woken while still held back");
		drop(second);
		assert!(woken.0.load(Ordering::SeqCst), "the last landing wakes");
	}

	/// A `.`-prefixed segment below the requested prefix hides a route from
	/// discovery unless the reader opts in; one inside the prefix does not.
	#[kio::test]
	async fn hidden_routes_need_an_opt_in() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let _visible = producer.announce("room/alice", RouteInfo::new()).unwrap();
		let _stats = producer.announce(".stats/node", RouteInfo::new()).unwrap();
		let _nested = producer.announce("room/.internal", RouteInfo::new()).unwrap();
		// Only a leading dot hides: a suffix is part of the name.
		let _suffix = producer.announce("room/catalog.pro", RouteInfo::new()).unwrap();

		let mut announced = consumer.routes();
		announced.expect_next_active("room/alice").await;
		announced.expect_next_active("room/catalog.pro").await;
		announced.assert_next_wait();

		let mut announced = consumer.clone().with_hidden(true).routes();
		announced.expect_next_active(".stats/node").await;
		announced.expect_next_active("room/.internal").await;
		announced.expect_next_active("room/alice").await;
		announced.expect_next_active("room/catalog.pro").await;
		announced.assert_next_wait();

		// Naming the dot segment lists what is under it, by root or by pattern.
		let mut announced = consumer
			.scope(".stats", &Patterns::from(Pattern::all()))
			.unwrap()
			.routes();
		announced.expect_next_active("node").await;
		announced.assert_next_wait();
		let mut announced = consumer.scope("", &scopes(&["room/.internal"])).unwrap().routes();
		announced.expect_next_active("room/.internal").await;
		announced.assert_next_wait();
	}

	/// Hiding narrows discovery only: an exact request resolves without an opt-in.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn hidden_broadcast_resolves_by_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let broadcast = producer.route(".stats/node", RouteOptions::hidden()).unwrap();
		broadcast.announce().unwrap();

		consumer.routes().assert_next_wait();
		let resolved = consumer.request(".stats/node", None).await.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), ".stats/node");
	}

	/// An excluded subtree is neither discovered nor resolved, even under a route
	/// covering it, while its hidden siblings and the covering route still show.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn exclude_hides_and_refuses_the_subtree() {
		let producer = origin(1).produce();
		let _claim = producer.announce("p1", RouteInfo::new()).unwrap();
		let _cam = producer.route("p1/cam", RouteOptions::default()).unwrap();
		let _private = producer.route("p1/.private/cam", RouteOptions::default()).unwrap();
		let _overlay = producer.route("p1/.pro/cam", RouteOptions::default()).unwrap();
		let consumer = producer
			.consume()
			.scope("p1", &Patterns::from(Pattern::all()))
			.unwrap()
			.with_hidden(true)
			.exclude(".pro")
			.unwrap();

		let mut announced = consumer.routes();
		announced.expect_next_active("").await;
		announced.expect_next_active(".private/cam").await;
		announced.expect_next_active("cam").await;
		announced.assert_next_wait();

		consumer.request("cam", None).await.expect("resolves");
		for path in [".pro", ".pro/cam", ".pro/cam/.pro/transcode.hang"] {
			let refused = consumer.request(path, None).now_or_never().expect("refused at once");
			assert!(matches!(refused, Err(Error::Unauthorized)), "{path}");
			assert!(
				matches!(consumer.follow(path, None), Err(Error::Unauthorized)),
				"{path}"
			);
		}
		// Only whole segments: a sibling sharing the prefix text is untouched.
		let _sibling = producer.route("p1/.prod", RouteOptions::default()).unwrap();
		announced.expect_next_active(".prod").await;
	}

	/// An exclusion inside a mount carves the target, and one over the mount point
	/// drops the mount.
	#[kio::test]
	async fn exclude_reaches_into_a_mount() {
		let producer = origin(1).produce();
		let _foo = producer.route(".svc/p1/foo", RouteOptions::default()).unwrap();
		let _bar = producer.route(".svc/p1/bar", RouteOptions::default()).unwrap();
		let _cam = producer.route("p1/cam", RouteOptions::default()).unwrap();
		let project = mounted(&producer, "p1", &["**"]).with_hidden(true);

		let mut announced = project.exclude(".svc/foo").unwrap().routes();
		announced.expect_next_active("cam").await;
		announced.expect_next_active(".svc/bar").await;
		announced.assert_next_wait();

		let mut announced = project.exclude(".svc").unwrap().routes();
		announced.expect_next_active("cam").await;
		announced.assert_next_wait();

		// Rooted inside the mount, the exclusion moves onto the target.
		let mut inside = project
			.scope(".svc", &Patterns::from(Pattern::all()))
			.unwrap()
			.exclude("foo")
			.unwrap()
			.routes();
		inside.expect_next_active("bar").await;
		inside.assert_next_wait();
	}

	/// A project-rooted handle reading `.svc` from the fleet-wide `.svc/<pid>`.
	fn mounted(producer: &Producer, pid: &str, patterns: &[&str]) -> Consumer {
		let patterns: Patterns = patterns.iter().map(|pattern| pattern.parse().unwrap()).collect();
		producer
			.mount(format!("{pid}/.svc"), format!(".svc/{pid}"))
			.unwrap()
			.scope(pid, &patterns)
			.unwrap()
			.consume()
	}

	/// A request through a mount is a request for the target path: it joins the
	/// one front there, so the fleet-wide claim is asked once for both readers.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn mount_resolves_on_the_target_front() {
		let producer = origin(1).produce();
		let server = producer.forward(".svc", RouteInfo::new()).unwrap();
		let project = mounted(&producer, "p1", &["**"]);

		let through = project.request(".svc/foo", None);
		let direct = producer.consume().request(".svc/p1/foo", None);

		let request = queued(&server).await;
		assert_eq!(request.path().as_str(), ".svc/p1/foo");
		assert!(server.poll_requested(&kio::Waiter::noop()).is_pending());
		let source = broadcast::Info::new().produce();
		request.serve(&source);

		let through = through.await.expect("resolves");
		let direct = direct.await.expect("resolves");
		assert!(through.is_clone(&direct));
		// Named by what the reader asked for.
		assert_eq!(through.info().path.as_str(), ".svc/foo");
	}

	/// Routes under the target, and the claim covering it, present under the
	/// mount; what the origin holds at the mounted path itself does not.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn mount_presents_target_routes_under_the_mount() {
		let producer = origin(1).produce();
		let _claim = producer.announce(".svc", RouteInfo::new()).unwrap();
		let foo = producer.route(".svc/p1/foo", RouteOptions::default()).unwrap();
		let _other = producer.route(".svc/p2/bar", RouteOptions::default()).unwrap();
		let _cam = producer.route("p1/cam", RouteOptions::default()).unwrap();
		let _shadowed = producer.route("p1/.svc/forged", RouteOptions::default()).unwrap();
		let project = mounted(&producer, "p1", &["**"]);

		// A dot segment hides the mount like any other.
		let mut announced = project.routes();
		announced.expect_next_active("cam").await;
		announced.assert_next_wait();

		let mut announced = project.clone().with_hidden(true).covering().routes();
		announced.expect_next_active("cam").await;
		announced.expect_next_active(".svc").await;
		announced.expect_next_active(".svc/foo").await;
		announced.assert_next_wait();

		// Rooted inside the mount, the claim covers the root itself.
		let mut inside = project
			.scope(".svc", &Patterns::from(Pattern::all()))
			.unwrap()
			.covering()
			.routes();
		inside.expect_next_active("").await;
		inside.expect_next_active("foo").await;
		inside.assert_next_wait();

		drop(foo);
		announced.expect_next_ended(".svc/foo").await;
		inside.expect_next_ended("foo").await;
		announced.assert_next_wait();

		// The shadowed path never resolves: the mount answers for it.
		let err = project.request(".svc/forged", None).await.err().unwrap();
		assert!(matches!(err, Error::NotFound | Error::Unroutable), "{err:?}");
	}

	/// The handle's own patterns authorize a path through the mount, named as the
	/// handle names it.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn mount_authorizes_the_named_path() {
		let producer = origin(1).produce();
		let _foo = producer.route(".svc/p1/foo", RouteOptions::default()).unwrap();
		let _bar = producer.route(".svc/p1/bar", RouteOptions::default()).unwrap();

		let granted = mounted(&producer, "p1", &["foo", ".svc/foo"]);
		granted.request(".svc/foo", None).await.expect("granted");
		let refused = granted
			.request(".svc/bar", None)
			.now_or_never()
			.expect("refused at once");
		assert!(matches!(refused, Err(Error::Unauthorized)));
		let mut announced = granted.clone().with_hidden(true).routes();
		announced.expect_next_active(".svc/foo").await;
		announced.assert_next_wait();

		let narrower = mounted(&producer, "p1", &["foo"]);
		let refused = narrower
			.request(".svc/foo", None)
			.now_or_never()
			.expect("refused at once");
		assert!(matches!(refused, Err(Error::Unauthorized)));
		narrower.clone().with_hidden(true).routes().assert_next_wait();

		// Another project's mount reaches its own target, never this one's.
		let other = mounted(&producer, "p2", &["**"]);
		other.clone().with_hidden(true).routes().assert_next_wait();
		let err = other.request(".svc/foo", None).await.err().unwrap();
		assert!(matches!(err, Error::Unroutable));
	}

	/// A wildcard spanning the mount point captures the path as the handle names
	/// it, so capture-keyed consumers key a mounted route like any other.
	#[kio::test]
	async fn mount_captures_the_named_path() {
		let producer = origin(1).produce();
		let _foo = producer.route(".svc/p1/foo", RouteOptions::default()).unwrap();
		let project = mounted(&producer, "p1", &["**"]).with_hidden(true);

		let Some(RouteEvent {
			kind: RouteKind::Start,
			announce: update,
		}) = project.routes().try_next()
		else {
			panic!("expected foo to start");
		};
		assert_eq!(update.prefix.as_str(), ".svc/foo");
		assert_eq!(update.captures, Some(vec![".svc/foo".parse::<Pattern>().unwrap()]));

		let inside = project.scope(".svc", &Patterns::from(Pattern::all())).unwrap();
		let Some(RouteEvent {
			kind: RouteKind::Start,
			announce: update,
		}) = inside.routes().try_next()
		else {
			panic!("expected foo to start");
		};
		assert_eq!(update.prefix.as_str(), "foo");
		assert_eq!(update.captures, Some(vec!["foo".parse::<Pattern>().unwrap()]));
	}

	/// A target at the maximum depth still presents its exact path: the `**` a
	/// wildcard scope carries past it can only match nothing there.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn mount_keeps_a_max_depth_target() {
		let producer = origin(1).produce();
		let deep = vec!["d"; Path::MAX_PARTS].join("/");
		let _leaf = producer.route(deep.as_str(), RouteOptions::default()).unwrap();
		let project = producer
			.mount("p1/.svc", deep.as_str())
			.unwrap()
			.scope("p1", &Patterns::from(Pattern::all()))
			.unwrap()
			.consume()
			.with_hidden(true);

		let mut announced = project.routes();
		announced.expect_next_active(".svc").await;
		announced.assert_next_wait();

		// Beneath the mount the target has no room: refused, never handed to a route.
		let err = project.request(".svc/x", None).await.err().unwrap();
		assert!(matches!(err, Error::BoundsExceeded(_)), "{err:?}");
		let inside = project.scope(".svc/x", &Patterns::from(Pattern::all())).unwrap();
		inside.routes().assert_next_wait();
	}

	/// A mount point deeper than its target never presents a route whose name
	/// through the mount is past the depth limit, and a mount point past it is refused.
	#[kio::test]
	async fn mount_bounds_the_named_path() {
		let producer = origin(1).produce();
		let _near = producer.route("t/x", RouteOptions::default()).unwrap();
		let deep = format!("t/{}", vec!["d"; Path::MAX_PARTS - 1].join("/"));
		let _deep = producer.route(deep.as_str(), RouteOptions::default()).unwrap();
		let project = producer
			.mount("p1/a/b", "t")
			.unwrap()
			.scope("p1", &Patterns::from(Pattern::all()))
			.unwrap()
			.consume();

		let mut announced = project.routes();
		announced.expect_next_active("a/b/x").await;
		announced.assert_next_wait();

		let over = vec!["d"; Path::MAX_PARTS + 1].join("/");
		assert!(matches!(
			producer.mount(over.as_str(), "t"),
			Err(Error::BoundsExceeded(_))
		));
	}

	/// Nothing is published at or beneath a mount.
	#[kio::test]
	async fn mount_is_read_only() {
		let producer = origin(1).produce();
		let project = producer
			.mount("p1/.svc", ".svc/p1")
			.unwrap()
			.scope("p1", &Patterns::from(Pattern::all()))
			.unwrap();
		assert!(matches!(
			project.route(".svc/foo", RouteOptions::hidden()),
			Err(Error::Unauthorized)
		));
		assert!(matches!(
			project.forward(".svc", RouteInfo::new()),
			Err(Error::Unauthorized)
		));
		assert!(matches!(
			project.forward(".svc/foo", RouteInfo::new()),
			Err(Error::Unauthorized)
		));
		let _cam = project.route("cam", RouteOptions::default()).unwrap();
	}

	/// A mount reaches only what the handle already reaches, and mounts never nest.
	#[kio::test]
	async fn mount_never_widens_a_scope() {
		let (producer, _driver) = Producer::new(Config::from(origin(1)));
		let project = producer.scope("", &scopes(&["p1"])).unwrap();
		assert!(matches!(project.mount("p1/.svc", ".svc/p1"), Err(Error::Unauthorized)));

		let mounted = producer.mount("p1/.svc", ".svc/p1").unwrap();
		assert!(matches!(mounted.mount("p1/.svc/x", ".other"), Err(Error::Duplicate)));
		assert!(matches!(mounted.mount("p1", ".other"), Err(Error::Duplicate)));
		// A target through a mount would read the subtree the mount shadows.
		assert!(matches!(mounted.mount("p2", "p1/.svc/x"), Err(Error::Duplicate)));
		assert!(matches!(mounted.mount("p2", "p1"), Err(Error::Duplicate)));
		// A mount point on a target would chain through it, in either order.
		assert!(matches!(mounted.mount(".svc/p1/x", ".other"), Err(Error::Duplicate)));
		assert!(matches!(mounted.mount(".svc", ".other"), Err(Error::Duplicate)));
		mounted.mount("p1/.other", ".other/p1").unwrap();
		// Mount points may share a target.
		mounted.mount("p2/.svc", ".svc/p1").unwrap();
		// A mount point overlapping its own target is refused like any other overlap.
		for (at, target) in [("a", "a/b"), ("a/b", "a"), ("a", "a")] {
			assert!(
				matches!(producer.mount(at, target), Err(Error::Duplicate)),
				"{at} -> {target}"
			);
		}
	}

	/// A mount point no pattern can spell could never be announced or authorized.
	#[kio::test]
	async fn mount_refuses_a_wildcard_mount_point() {
		let (producer, _driver) = Producer::new(Config::from(origin(1)));
		for at in ["*", "p1/*", "p1/**", "p1/a*"] {
			assert!(
				matches!(producer.mount(at, ".svc/p1"), Err(Error::InvalidPath(_))),
				"{at}"
			);
		}
	}

	/// Egress through a mount counts under the path the reader named, so it
	/// attributes to the reader's root rather than the fleet-wide target.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn mount_egress_counts_under_the_named_path() {
		let registry = stats::Registry::new(stats::Config::new());
		let producer = origin(1).produce();
		let _foo = producer.route(".svc/p1/foo", RouteOptions::default()).unwrap();
		let project = mounted(&producer, "p1", &["**"])
			.with_stats(
				registry
					.tier(stats::Tier::default())
					.session(crate::stats::SessionInfo::new(crate::Path::from_static("p1"))),
			)
			.with_hidden(true);

		let mut announced = project.routes();
		announced.expect_next_active(".svc/foo").await;
		project.request(".svc/foo", None).await.expect("resolves");

		let mut report = stats::Report::default();
		registry.report(&mut report, crate::time::Instant::now());
		let paths: Vec<_> = report
			.traffic
			.iter()
			.map(|entry| entry.path.as_str().to_string())
			.collect();
		assert_eq!(paths, ["p1/.svc/foo"]);
	}

	/// Reads through an unmetered track handle count toward no session, while the metered
	/// handle on the same broadcast still counts the same group.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_unmetered_track_counts_nothing() {
		let registry = stats::Registry::new(stats::Config::new());
		let producer = origin(1).produce();
		let route = producer.route("room/alice", RouteOptions::default()).unwrap();
		let track = route
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let consumer = producer.consume().with_stats(
			registry
				.tier(stats::Tier::default())
				.session(crate::stats::SessionInfo::new(crate::Path::from_static("viewer"))),
		);
		let broadcast = consumer.request("room/alice", None).await.expect("resolves");
		let bytes = || -> u64 {
			registry
				.snapshot()
				.traffic()
				.into_iter()
				.map(|(_, _, traffic)| traffic.bytes)
				.sum()
		};

		let mut unmetered = broadcast
			.track("video")
			.unwrap()
			.unmetered()
			.subscribe(None)
			.await
			.unwrap();
		let mut metered = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"frame".as_ref()).unwrap();
		group.close().unwrap();

		let mut read = unmetered.recv_group().await.unwrap().expect("a group");
		assert_eq!(
			read.read_frame().await.unwrap().expect("a frame").payload.as_ref(),
			b"frame"
		);
		assert_eq!(bytes(), 0, "an unmetered read counts nothing");

		let mut read = metered.recv_group().await.unwrap().expect("a group");
		assert_eq!(
			read.read_frame().await.unwrap().expect("a frame").payload.as_ref(),
			b"frame"
		);
		assert_eq!(bytes(), 5, "the metered handle still counts");
	}

	/// A group a fill answers is ingress like a group the publisher writes live: a relay
	/// writes what a FETCH brings back through its track's requests.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_fetched_group_counts_as_ingress() {
		let registry = stats::Registry::new(stats::Config::new());
		let producer = origin(1).produce().with_stats(
			registry
				.tier(stats::Tier::default())
				.session(crate::stats::SessionInfo::new(crate::Path::from_static("room"))),
		);
		let route = producer.route("room/alice", RouteOptions::default()).unwrap();
		let track = route
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let requests = track.requests();
		let ingress = || -> u64 {
			registry
				.snapshot()
				.traffic()
				.into_iter()
				.filter(|(_, role, _)| *role == stats::Role::Subscriber)
				.map(|(_, _, traffic)| traffic.bytes)
				.sum()
		};

		let mut live = track.create_group(crate::group::Info { start: 1 }).unwrap();
		live.write_frame(crate::Timestamp::from_millis(1).unwrap(), b"live".as_ref())
			.unwrap();
		live.close().unwrap();
		assert_eq!(ingress(), 4);

		let fetch = track.consume().fetch_group(0, None);
		let fill = requests.requested_fill_group().await.unwrap();
		let mut fetched = fill.accept(crate::track::Info::timed()).unwrap();
		fetched
			.write_frame(crate::Timestamp::ZERO, b"fetched".as_ref())
			.unwrap();
		fetched.close().unwrap();
		fetch.await.unwrap();
		assert_eq!(ingress(), 4 + 7, "the fetched group counts as well");

		// A reader outliving the publisher keeps the track's groups, not its session.
		let reader = track.consume();
		drop((track, requests, route, producer));
		let active: u64 = registry
			.snapshot()
			.sessions()
			.into_iter()
			.map(|(_, presence)| presence.active())
			.sum();
		assert_eq!(active, 0, "the publisher's session ended");
		drop(reader);
	}

	/// A route that turns up later is filtered the same way as the replay.
	#[kio::test]
	async fn hidden_route_announced_later_stays_hidden() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();
		let mut opted = consumer.clone().with_hidden(true).routes();

		let hidden = producer.announce(".stats/node", RouteInfo::new()).unwrap();
		announced.assert_next_wait();
		opted.expect_next_active(".stats/node").await;

		drop(hidden);
		announced.assert_next_wait();
		opted.expect_next_ended(".stats/node").await;
	}

	#[kio::test]
	async fn a_replayed_route_retracted_before_delivery_cancels() {
		let producer = origin(1).produce();
		let alice = producer.announce("alice", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		drop(alice);
		announced.assert_next_wait();
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_hidden_route_announces_its_own_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();
		let mut peer = consumer.clone().excluding(Hop::UNKNOWN).routes();

		// Created but not announced: invisible to local and peer cursors alike, even
		// once priced.
		let broadcast = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		broadcast.set_cost(3).unwrap();
		announced.assert_next_wait();
		peer.assert_next_wait();

		broadcast.announce().unwrap();
		assert_eq!(announced.expect_next_active("room/alice").await.cost, Cost::new(3));
		assert_eq!(peer.expect_next_active("room/alice").await.cost, Cost::new(3));

		// Re-pricing a visible route updates it in place.
		broadcast.set_cost(1).unwrap();
		assert_eq!(announced.expect_next_active("room/alice").await.cost, Cost::new(1));
		assert_eq!(peer.expect_next_active("room/alice").await.cost, Cost::new(1));

		// Off the air: the route retracts for everyone and the path is unroutable.
		broadcast.withdraw();
		announced.expect_next_ended("room/alice").await;
		peer.expect_next_ended("room/alice").await;
		broadcast.withdraw();
		announced.assert_next_wait();
		let err = consumer.request("room/alice", None).await.err().unwrap();
		assert!(matches!(err, Error::Unroutable));

		// Back on the air, then the end of the broadcast retracts for good.
		broadcast.announce().unwrap();
		announced.expect_next_active("room/alice").await;
		peer.expect_next_active("room/alice").await;
		broadcast.broadcast().close();
		announced.expect_next_ended("room/alice").await;
		peer.expect_next_ended("room/alice").await;
		assert!(matches!(broadcast.announce(), Err(Error::Closed)));
		announced.assert_next_wait();
	}

	#[kio::test]
	async fn a_route_retracts_with_its_last_clone() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();

		let route = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		let clone = route.clone();
		let source = route.broadcast().clone();
		route.announce().unwrap();
		announced.expect_next_active("room/alice").await;

		// A clone keeps the route alive; a broadcast producer does not.
		drop(route);
		announced.assert_next_wait();
		drop(clone);
		announced.expect_next_ended("room/alice").await;
		assert!(!source.consume().is_closed());
	}

	#[kio::test]
	async fn publish_creates_and_announces_together() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let _broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		announced.expect_next_active("room/alice").await;
	}

	#[kio::test]
	async fn announce_replays_to_late_cursor() {
		let producer = origin(1).produce();
		let _a = producer.announce("room/alice", RouteInfo::new()).unwrap();
		let _b = producer.announce("room/bob", RouteInfo::new()).unwrap();

		let mut announced = producer.consume().routes();
		// Replay follows the table traversal order.
		announced.expect_next_active("room/alice").await;
		announced.expect_next_active("room/bob").await;
		announced.assert_next_wait();
	}

	#[kio::test]
	async fn announce_keeps_its_prefix_under_a_producer_scope() {
		let producer = origin(1).produce();
		let scoped = producer.scope("", &scopes(&["room"])).unwrap();

		// Prefix advertisements stay prefixes. The scope filters requests locally.
		let _a = scoped.announce("", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		announced.expect_next_active("").await;

		// Disjoint prefixes cannot be claimed at all.
		assert!(matches!(
			scoped.announce("other", RouteInfo::new()),
			Err(Error::Unauthorized)
		));
	}

	#[kio::test]
	async fn cursor_keeps_an_overlapping_prefix_above_its_scope() {
		let producer = origin(1).produce();
		let _a = producer.announce("", RouteInfo::new()).unwrap();

		let consumer = producer.consume().scope("", &scopes(&["room"])).unwrap();
		let mut announced = consumer.clone().covering().routes();
		announced.expect_next_active("").await;
		consumer.routes().assert_next_wait();
	}

	#[kio::test]
	async fn cursor_root_strips_prefix() {
		let producer = origin(1).produce();
		let _a = producer.announce("room/alice", RouteInfo::new()).unwrap();

		let consumer = producer
			.consume()
			.scope("room", &Patterns::from(Pattern::all()))
			.unwrap();
		let mut announced = consumer.routes();
		announced.expect_next_active("alice").await;
	}

	#[kio::test]
	async fn best_route_wins_and_fails_over() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let expensive = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[10])).with_cost(5))
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert_eq!(route.cost, Cost::new(5));

		// A cheaper route for the same prefix takes over in place.
		let cheap = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[20])).with_cost(1))
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert_eq!(route.cost, Cost::new(1));

		// Losing the winner falls back to the survivor, still in place.
		drop(cheap);
		let route = announced.expect_next_active("room").await;
		assert_eq!(route.cost, Cost::new(5));

		// Losing the last retracts.
		drop(expensive);
		announced.expect_next_ended("room").await;
	}

	/// Pinned so `spreadHash` in `js/net` picks the same pool member for a path.
	#[kio::test]
	async fn spread_hash_matches_js() {
		assert_eq!(instance_key("pool/job-0", &"a".parse().unwrap()), 0x3df8f75a44f8eefb);
		assert_eq!(instance_key("pool/job-0", &"b".parse().unwrap()), 0x3df8f85a44f8f0ae);
		assert_eq!(chain_key("pool/job-0", &hops(&[10])), 0xefb5e20a66101c32);
		assert_eq!(chain_key("pool/job-0", &hops(&[11])), 0x0eb0a91370ff6653);
	}

	/// Pinned so `js/net` breaks a tie between lite-07 claims as this does: by the
	/// rendezvous hash of each origin node under a claim, and at a claim's own path by the
	/// newest claim, which a re-price leaves in place.
	#[kio::test]
	async fn lite07_ties_match_js() {
		assert_eq!(fnv_key("pool/job", 0x7au64.to_le_bytes()), 0xc376be7bf47aebbf);
		assert_eq!(fnv_key("pool/job", 0x8bu64.to_le_bytes()), 0x5813213b6508aaee);

		let producer = origin(1).produce();
		let claim = |node: u64, name: &str, cost: u64| {
			RouteInfo::new()
				.with_instance(name.parse().unwrap(), 0)
				.with_cost(cost)
				.with_node(Some(node))
		};
		let older = producer.forward("pool", claim(0x7a, "a", 5)).unwrap();
		let _newer = producer.forward("pool", claim(0x8b, "b", 1)).unwrap();
		older.update(older.route().unwrap().with_cost(1)).unwrap();

		let best = |path: &str| {
			let table = producer.shared.read();
			table
				.best_route(&Path::new(path).unwrap(), Horizon::default(), &HashSet::new(), |_| true)
				.and_then(|entry| entry.node)
		};
		assert_eq!(best("pool"), Some(0x8b), "the newer claim keeps a tie a re-price made");
		assert_eq!(
			best("pool/job"),
			Some(0x8b),
			"the lower node hash wins under the claims"
		);

		// A lite-06 route keys on its instance and chain, a lite-07 claim on its node, each on
		// its own: here the claim's is lower.
		assert_eq!(instance_key("pool/job", &"b".parse().unwrap()), 0xd16a3821c67a2d25);
		let producer = origin(1).produce();
		let _claim = producer.forward("pool", claim(0x7a, "a", 1)).unwrap();
		let legacy = RouteInfo::new()
			.with_instance("b".parse().unwrap(), 0)
			.with_hops(hops(&[11]))
			.with_cost(1);
		let _legacy = producer.forward("pool", legacy).unwrap();
		let table = producer.shared.read();
		let winner = table
			.best_route(
				&Path::new("pool/job").unwrap(),
				Horizon::default(),
				&HashSet::new(),
				|_| true,
			)
			.and_then(|entry| entry.node);
		assert_eq!(winner, Some(0x7a), "each route keyed on its own");
	}

	/// Equal-cost advertisers of one prefix share its paths: a set of requested
	/// paths spreads across the pool, and one path always resolves to the same
	/// advertiser, whatever order the routes arrived in. The split is a rendezvous
	/// hash, so losing an advertiser moves only the paths it won.
	#[kio::test]
	async fn equal_cost_pool_spreads_paths() {
		const WORKERS: [u64; 4] = [10, 11, 12, 13];
		const PATHS: usize = 64;

		// The first hop of the route each path resolves to on a node whose pool
		// arrived in `order`.
		fn winners(order: impl Iterator<Item = u64>) -> Vec<Hop> {
			let producer = origin(1).produce();
			let _pool: Vec<Claim> = order
				.map(|id| {
					producer
						.forward(
							"pool",
							RouteInfo::new()
								.with_instance(format!("worker-{id}").parse().unwrap(), 0)
								.with_hops(hops(&[id]))
								.with_cost(3),
						)
						.unwrap()
				})
				.collect();
			let table = producer.shared.read();
			(0..PATHS)
				.map(|i| {
					let path = Path::new(&format!("pool/job-{i}")).unwrap();
					let entry = table
						.best_route(&path, Horizon::default(), &HashSet::new(), |_| true)
						.expect("the pool serves every path");
					entry.hops.iter().next().copied().unwrap()
				})
				.collect()
		}

		let forward = winners(WORKERS.into_iter());
		let reverse = winners(WORKERS.into_iter().rev());
		assert_eq!(forward, reverse, "a path must resolve the same way on every node");

		// Not an assertion about any two paths, which a correct hash may put on
		// one worker: only that the set does not pile onto a few.
		for worker in WORKERS {
			let share = forward.iter().filter(|hop| **hop == origin(worker)).count();
			assert!(share >= PATHS / 16, "worker {worker} took {share} of {PATHS} paths");
		}

		// A mod-N style hash would reshuffle most paths here.
		let lost = origin(WORKERS[1]);
		let survivors = winners(WORKERS.into_iter().filter(|id| origin(*id) != lost));
		for (i, (before, after)) in forward.iter().zip(&survivors).enumerate() {
			if *before != lost {
				assert_eq!(after, before, "pool/job-{i} moved off a surviving worker");
			}
		}
	}

	/// An identical route from a fresh announcement (a reconnect) wins at once. One that
	/// mints its own instance is another stream, so consumers restart; one with the same
	/// instance is the same stream, and nothing is delivered. Retracting the stale twin is
	/// quiet either way.
	#[kio::test]
	async fn identical_reannounce_restarts_under_another_instance() {
		for named in [None, Some(instance())] {
			let producer = origin(1).produce();
			let mut announced = producer.consume().routes();
			let mut route = RouteInfo::new().with_hops(hops(&[10]));
			route.instance = named.clone();

			let old = producer.announce("room", route.clone()).unwrap();
			let first = announced.expect_next_active("room").await;
			assert_eq!(first.hops.as_slice(), hops(&[10]).as_slice());

			let _new = producer.announce("room", route).unwrap();
			match named {
				None => assert_eq!(announced.expect_next_restarted("room").await.hops, hops(&[10])),
				Some(_) => announced.assert_next_wait(),
			}

			drop(old);
			announced.assert_next_wait();
		}
	}

	/// A re-price of the winning route is an update in place, never a restart.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_reprice_is_an_update() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let route = producer.forward("room", RouteInfo::new().with_cost(3)).unwrap();
		announced.expect_next_active("room").await;

		route.update(RouteInfo::new().with_cost(5)).unwrap();
		match announced.next().await.expect("no next") {
			RouteEvent {
				kind: RouteKind::Update,
				announce: update,
			} => assert_eq!(update.route.cost, Cost::new(5)),
			other => panic!("should be an update: got {other:?}"),
		}
	}

	/// Swapping a route's auth is an update in place carrying the new auth, so the session
	/// advertising it can renew its request.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_auth_swap_is_an_update() {
		let handle = crate::auth::Link::ietf();
		let first = handle.present(bytes::Bytes::from_static(b"first"), false).unwrap();
		let second = handle.present(bytes::Bytes::from_static(b"second"), false).unwrap();
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let route = producer
			.forward("room", RouteInfo::new().with_auth(first.clone()))
			.unwrap();
		assert_eq!(announced.expect_next_active("room").await.auth, Some(first));

		route.update(RouteInfo::new().with_auth(second.clone())).unwrap();
		match announced.next().await.expect("no next") {
			RouteEvent {
				kind: RouteKind::Update,
				announce: update,
			} => assert_eq!(update.route.auth, Some(second)),
			other => panic!("should be an update: got {other:?}"),
		}
	}

	/// A change to a closed route fails and keeps nothing, so the route holds no
	/// credential it was refused.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_closed_route_keeps_no_refused_change() {
		let handle = crate::auth::Link::ietf();
		let token = handle.present(bytes::Bytes::from_static(b"token"), false).unwrap();
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let route = producer.route("room", RouteOptions::hidden()).unwrap();
		route.announce().unwrap();
		announced.expect_next_active("room").await;
		route.broadcast().close();
		announced.expect_next_ended("room").await;

		assert!(matches!(route.set_auth(Some(token.clone())), Err(Error::Closed)));
		assert!(matches!(route.set_cost(7), Err(Error::Closed)));
		let control = route.state.control.lock();
		assert!(control.options.auth.is_none());
		assert_eq!(control.options.cost, Cost::default());
	}

	/// A replacement under another instance from a cluster peer at equal cost and chain
	/// length, whose hash loses, competes with the lingering old route: nothing restarts and
	/// requests stay on the old route until it is withdrawn, then consumers restart and a
	/// re-request lands on the replacement. Every node ranks peer routes alike.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_equal_cost_peer_replacement_waits_for_the_old_route() {
		// Pinned in `spread_hash_matches_js`: instance `a` hashes below `b` for this path.
		// Restarts are delivered at once: the hold is not what this checks.
		let producer = Config {
			update_hold: Duration::ZERO,
			..Config::from(origin(1))
		}
		.produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();
		let peer = producer.clone().with_link(Link::Peer);
		let old = peer
			.forward(
				"pool/job-0",
				RouteInfo::new()
					.with_instance("a".parse().unwrap(), 0)
					.with_hops(hops(&[11])),
			)
			.unwrap();
		announced.expect_next_active("pool/job-0").await;

		let new = peer
			.forward(
				"pool/job-0",
				RouteInfo::new()
					.with_instance("b".parse().unwrap(), 0)
					.with_hops(hops(&[10])),
			)
			.unwrap();
		announced.assert_next_wait();

		let pending = consumer.request("pool/job-0", None);
		let request = queued(&old).await;
		let first = broadcast::Info::new().produce();
		request.serve(&first);
		let resolved = pending.await.expect("the old route resolves");
		assert!(new.poll_requested(&kio::Waiter::noop()).is_pending());

		drop(old);
		assert_eq!(announced.expect_next_restarted("pool/job-0").await.hops, hops(&[10]));

		let pending = consumer.request("pool/job-0", None);
		let request = queued(&new).await;
		let second = broadcast::Info::new().produce();
		request.serve(&second);
		let replaced = pending.await.expect("the replacement resolves");
		assert!(!replaced.is_clone(&resolved));
	}

	/// A publisher that restarts while its old session lingers announces its own path under
	/// a new first hop. The restart takes over at once whichever chain hashes lower, so new
	/// requests never wait on the old session.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_equal_cost_local_replacement_takes_over_at_once() {
		for (old_hop, new_hop) in [(11, 10), (10, 11)] {
			let producer = Config {
				update_hold: Duration::ZERO,
				..Config::from(origin(1))
			}
			.produce();
			let consumer = producer.consume();
			let mut announced = consumer.routes();
			let _old = producer
				.forward("pool/job-0", RouteInfo::new().with_hops(hops(&[old_hop])))
				.unwrap();
			announced.expect_next_active("pool/job-0").await;

			let new = producer
				.forward("pool/job-0", RouteInfo::new().with_hops(hops(&[new_hop])))
				.unwrap();
			assert_eq!(
				announced.expect_next_restarted("pool/job-0").await.hops,
				hops(&[new_hop])
			);

			let pending = consumer.request("pool/job-0", None);
			queued(&new).await.serve(broadcast::Info::new().produce());
			pending.await.expect("the replacement resolves");
		}
	}

	/// An origin delivering restarts at once, and a way to claim `prefix` with a
	/// producer scoped to `scope`, from `hop` at `cost`.
	#[cfg(feature = "async")]
	fn scoped_pool() -> (Producer, impl Fn(&str, &str, u64, u64) -> Claim) {
		let producer = Config {
			update_hold: Duration::ZERO,
			..Config::from(origin(1))
		}
		.produce();
		let claims = producer.clone();
		let claim = move |prefix: &str, scope: &str, hop: u64, cost: u64| {
			claims
				.scope("", &scopes(&[scope]))
				.unwrap()
				.forward(prefix, RouteInfo::new().with_hops(hops(&[hop])).with_cost(cost))
				.unwrap()
		};
		(producer, claim)
	}

	/// Request `path` from `consumer` and have `server` answer it: the answer, and
	/// the requester's broadcast.
	#[cfg(feature = "async")]
	async fn hold(consumer: &Consumer, path: &str, server: &Claim) -> (broadcast::Producer, broadcast::Consumer) {
		let pending = consumer.request(path, None);
		let answer = broadcast::Info::new().produce();
		queued(server).await.serve(&answer);
		(answer, pending.await.expect("the request resolves"))
	}

	/// Let the fronts react, then expect nothing more on `announced`.
	#[cfg(feature = "async")]
	async fn quiet(announced: &mut Routes) {
		for _ in 0..20 {
			kio::clock::sim::yield_now().await;
		}
		announced.assert_next_wait();
	}

	/// A route from before the request repricing to win both the prefix and the held
	/// path restarts the prefix once: the cursor's own switch covers the move.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_repriced_route_taking_a_held_path_restarts_once() {
		let (producer, claim) = scoped_pool();
		let consumer = producer.consume();
		let a = claim("room", "room", 10, 5);
		let b = claim("room", "room", 11, 9);
		let mut announced = consumer.routes();
		assert_eq!(announced.expect_next_active("room").await.cost, Cost::new(5));
		let _sticky = hold(&consumer, "room/job", &a).await;

		b.update(RouteInfo::new().with_hops(hops(&[11])).with_cost(3)).unwrap();
		assert_eq!(announced.expect_next_restarted("room").await.cost, Cost::new(3));
		quiet(&mut announced).await;
	}

	#[kio::test]
	async fn exclude_hides_routes_through_the_peer() {
		let producer = origin(1).produce();
		let _a = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[7])))
			.unwrap();

		let mut hidden = producer.consume().excluding(origin(7)).routes();
		hidden.assert_next_wait();

		let mut visible = producer.consume().excluding(origin(8)).routes();
		visible.expect_next_active("room").await;
	}

	#[kio::test]
	async fn exclude_matches_via_when_the_chain_is_anonymous() {
		let producer = origin(1).produce();
		let assigned = origin(777);
		let _echoed = producer
			.announce("echoed", RouteInfo::new().with_hops(hops(&[0])).with_via(assigned))
			.unwrap();
		let _local = producer
			.announce("local", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();

		let mut hidden = producer.consume().excluding(assigned).routes();
		hidden.expect_next_active("local").await;
		hidden.assert_next_wait();
	}

	#[kio::test]
	async fn anonymous_route_loses_to_identified_at_any_cost() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let _anonymous = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[0])).with_cost(1))
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert!(route.is_anonymous());
		assert_eq!(route.cost, Cost::new(1));

		let _identified = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[10])).with_cost(5))
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert!(!route.is_anonymous());
		assert_eq!(route.cost, Cost::new(5));
	}

	#[kio::test]
	async fn a_lite07_claims_carried_chain_does_not_rank_it() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let _native = producer
			.announce(
				"room",
				RouteInfo::new().with_hops(hops(&[10])).with_cost(5).with_node(Some(10)),
			)
			.unwrap();
		announced.expect_next_active("room").await;

		let _carried = producer
			.announce(
				"room",
				RouteInfo::new()
					.with_hops(hops(&[0, 7, 20]))
					.with_cost(1)
					.with_node(Some(20)),
			)
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert_eq!(route.cost, Cost::new(1));
	}

	#[kio::test]
	async fn anonymous_routes_order_by_cost() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let expensive = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[0])).with_cost(5))
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert_eq!(route.cost, Cost::new(5));

		let _cheap = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[0, 7])).with_cost(1))
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert!(route.is_anonymous());
		assert_eq!(route.cost, Cost::new(1));

		drop(expensive);
		announced.assert_next_wait();
	}

	#[kio::test]
	async fn anonymous_chain_from_identified_peer_still_ranks_last() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let _anonymous = producer
			.announce(
				"room",
				RouteInfo::new()
					.with_hops(hops(&[0, 7]))
					.with_cost(1)
					.with_via(origin(7)),
			)
			.unwrap();
		announced.expect_next_active("room").await;

		let _identified = producer
			.announce("room", RouteInfo::new().with_hops(hops(&[10, 20])).with_cost(5))
			.unwrap();
		let route = announced.expect_next_active("room").await;
		assert!(!route.is_anonymous());
		assert_eq!(route.cost, Cost::new(5));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn request_prefers_identified_over_cheaper_anonymous() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let anonymous = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[0])).with_cost(1))
			.unwrap();
		let identified = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])).with_cost(5))
			.unwrap();

		let _pending = consumer.request("room/alice", None);
		let request = queued(&identified).await;
		assert_eq!(request.path().as_str(), "room/alice");
		assert!(
			anonymous.poll_requested(&kio::Waiter::noop()).is_pending(),
			"the cheaper anonymous route must not serve"
		);
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn request_avoids_a_demoted_link_even_when_cheaper() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let demoted = producer
			.clone()
			.with_preference(Preference::new(-1))
			.forward("room", RouteInfo::new().with_hops(hops(&[10])).with_cost(1))
			.unwrap();
		let unranked = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[11])).with_cost(5))
			.unwrap();

		let _pending = consumer.request("room/alice", None);
		let request = queued(&unranked).await;
		assert_eq!(request.path().as_str(), "room/alice");
		assert!(
			demoted.poll_requested(&kio::Waiter::noop()).is_pending(),
			"the cheaper demoted link must not serve"
		);
	}

	/// A draining route ranks below every other eligible one, even on a preferred link
	/// against a demoted one, whatever the costs say.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn request_avoids_a_draining_route_even_on_a_preferred_link() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let draining = producer
			.clone()
			.with_preference(Preference::new(1))
			.forward(
				"room",
				RouteInfo::new().with_hops(hops(&[10])).with_cost(1).with_draining(true),
			)
			.unwrap();
		let demoted = producer
			.clone()
			.with_preference(Preference::new(-1))
			.forward("room", RouteInfo::new().with_hops(hops(&[11])).with_cost(5))
			.unwrap();
		let _bob = consumer.request("room/bob", None);
		let request = queued(&demoted).await;
		assert_eq!(request.path().as_str(), "room/bob");
		assert!(
			draining.poll_requested(&kio::Waiter::noop()).is_pending(),
			"the draining route must not serve"
		);
	}

	/// Draining is local selection state: the winner draining changes nothing a reader
	/// sees, and the route keeps its real cost.
	#[kio::test]
	async fn draining_is_invisible_to_readers() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let dynamic = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])).with_cost(3))
			.unwrap();
		announced.expect_next_active("room").await;
		dynamic.update(dynamic.route().unwrap().with_draining(true)).unwrap();
		// Past the hold a best-route change would wait out.
		kio::clock::sleep(Config::default().update_hold * 2).await;
		announced.assert_next_wait();
		let route = dynamic.route().unwrap();
		assert_eq!((route.cost, route.draining), (Cost::new(3), true));
	}

	#[kio::test]
	async fn update_reprices_in_place() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let announcement = producer.announce("room", RouteInfo::new()).unwrap();
		announced.expect_next_active("room").await;

		announcement.update(RouteInfo::new().with_cost(9)).unwrap();
		let route = announced.expect_next_active("room").await;
		assert_eq!(route.cost, Cost::new(9));
	}

	/// Re-pricing from the current route keeps the instance, so it stays one broadcast.
	#[kio::test]
	async fn reprice_from_the_current_route_keeps_the_instance() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let dynamic = producer
			.forward("room", RouteInfo::new().with_instance(instance(), 0).with_cost(5))
			.unwrap();
		announced.expect_next_active("room").await;
		dynamic.update(dynamic.route().unwrap().with_cost(1)).unwrap();
		let route = announced.expect_next_active("room").await;
		assert_eq!((route.instance, route.cost), (Some(instance()), Cost::new(1)));
		assert_eq!(dynamic.route().unwrap().cost, Cost::new(1));

		let options = RouteOptions::hidden().with_instance(instance(), 0).with_cost(5);
		let broadcast = producer.route("lobby", options).unwrap();
		assert!(broadcast.route().is_none(), "not announced yet");
		broadcast.announce().unwrap();
		announced.expect_next_active("lobby").await;
		broadcast.set_cost(1).unwrap();
		let route = announced.expect_next_active("lobby").await;
		assert_eq!((route.instance, route.cost), (Some(instance()), Cost::new(1)));
		broadcast.withdraw();
		assert!(broadcast.route().is_none(), "withdrawn");
	}

	#[kio::test]
	async fn retract_after_undelivered_reprice_still_delivered() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();

		let announcement = producer.announce("room", RouteInfo::new()).unwrap();
		announced.expect_next_active("room").await;

		// Reprice, then retract before the consumer observes the reprice: the
		// pending metadata update must not cancel the retraction the delivered
		// announce still owes.
		announcement.update(RouteInfo::new().with_cost(9)).unwrap();
		drop(announcement);
		announced.expect_next_ended("room").await;
		announced.assert_next_wait();
	}

	#[kio::test]
	async fn scoped_cursor_advertises_most_specific_covering_route() {
		let producer = origin(1).produce();
		// Broad and cheap; narrow and expensive. Both present relative to a cursor
		// rooted below them, and the narrow one is what a request there resolves.
		let _broad = producer.announce("room", RouteInfo::new().with_cost(1)).unwrap();
		let _narrow = producer.announce("room/alice", RouteInfo::new().with_cost(9)).unwrap();

		let consumer = producer
			.consume()
			.scope("room/alice", &Patterns::from(Pattern::all()))
			.unwrap();
		let mut announced = consumer.routes();
		let route = announced.expect_next_active("").await;
		assert_eq!(route.cost, Cost::new(9));
		announced.assert_next_wait();
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn capture_change_retracts_before_reannouncing_a_presented_prefix() {
		let producer = origin(1).produce();
		let _broad = producer.announce("room", RouteInfo::new()).unwrap();
		let exact = producer.announce("room/alice", RouteInfo::new()).unwrap();
		let consumer = producer
			.consume()
			.scope("", &Patterns::from("room/*".parse::<Pattern>().unwrap()))
			.unwrap()
			.scope("room/alice", &Patterns::from(Pattern::all()))
			.unwrap()
			.covering();
		let mut announced = consumer.routes();

		let Some(Some(RouteEvent {
			kind: RouteKind::Start,
			announce: first,
		})) = announced.next().now_or_never()
		else {
			panic!("expected the announcement");
		};
		assert_eq!(first.prefix.as_str(), "");
		assert_eq!(first.captures, Some(Vec::new()));

		drop(exact);
		let Some(Some(RouteEvent {
			kind: RouteKind::End,
			announce: retracted,
		})) = announced.next().now_or_never()
		else {
			panic!("expected the retraction");
		};
		assert_eq!(retracted.prefix.as_str(), "");
		assert_eq!(retracted.captures, Some(Vec::new()));
		let Some(Some(RouteEvent {
			kind: RouteKind::Start,
			announce: replacement,
		})) = announced.next().now_or_never()
		else {
			panic!("expected the replacement");
		};
		assert_eq!(replacement.prefix.as_str(), "");
		assert_eq!(replacement.captures, None);
	}

	/// A forwarded route lost with its session retracts its identity as offline,
	/// even when a broader route takes over the presented prefix.
	#[kio::test]
	async fn capture_change_reports_a_lost_route_offline() {
		let producer = origin(1).produce();
		let _broad = producer.announce("room", RouteInfo::new()).unwrap();
		let exact = producer.forward("room/alice", RouteInfo::new()).unwrap();
		let mut announced = producer
			.consume()
			.scope("room/alice", &Patterns::from(Pattern::all()))
			.unwrap()
			.covering()
			.routes();
		announced.expect_next_active("").await;

		drop(exact);
		announced.expect_next_offline("").await;
		announced.expect_next_active("").await;
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn capture_change_coalesced_keeps_the_lost_route_offline() {
		let producer = origin(1).produce();
		let broad = producer.announce("room", RouteInfo::new()).unwrap();
		let exact = producer.forward("room/alice", RouteInfo::new()).unwrap();
		let mut announced = producer
			.consume()
			.scope("room/alice", &Patterns::from(Pattern::all()))
			.unwrap()
			.covering()
			.routes();
		announced.expect_next_active("").await;

		// Both leave before the next poll: the one retraction delivered is the
		// exact route's, which was lost rather than withdrawn.
		drop(exact);
		drop(broad);
		announced.expect_next_offline("").await;
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn restart_coalesced_keeps_the_lost_instance_offline() {
		let producer = origin(1).produce();
		let lost = producer.forward("room/alice", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		announced.expect_next_active("room/alice").await;

		// Another instance comes and is withdrawn before the next poll: the one
		// retraction delivered is the lost instance's.
		drop(lost);
		let other = producer.announce("room/alice", RouteInfo::new()).unwrap();
		drop(other);
		announced.expect_next_offline("room/alice").await;
	}

	#[test]
	fn interest_prefixes_fall_back_above_an_escaped_slash() {
		let heads = |pattern: &str| interest_prefixes(&Patterns::from(pattern.parse::<Pattern>().unwrap()));
		assert_eq!(heads(r"room/a\/*"), [Path::from_static("room")]);
		assert_eq!(heads(r"a\/b\/*"), [Path::empty()]);
		assert_eq!(heads(r"a\/b/*"), [Path::from_static(r"a\/b")]);
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn follow_resolves_once_announced() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Asking before anything is announced parks instead of failing Unroutable.
		let mut following = consumer.follow("room/alice", None).unwrap();
		let mut resolving = Box::pin(following.available());
		assert!((&mut resolving).now_or_never().is_none());

		// Creating is not announcing: still parked.
		let broadcast = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		for _ in 0..20 {
			kio::clock::sim::yield_now().await;
		}
		assert!((&mut resolving).now_or_never().is_none());

		broadcast.announce().unwrap();
		let resolved = resolving.await.expect("resolves once announced").unwrap();
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		drop(broadcast);
	}

	/// A local broadcast competes on its announced cost: a cheaper route at the
	/// same path wins, for cursors and requests alike.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn cheaper_remote_route_beats_a_local_broadcast() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();

		let _local = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(instance(), 0).with_cost(5),
			)
			.unwrap();
		assert_eq!(announced.expect_next_active("room/alice").await.cost, Cost::new(5));

		let server = producer
			.forward(
				"room/alice",
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[10]))
					.with_cost(1),
			)
			.unwrap();
		let route = announced.expect_next_active("room/alice").await;
		assert_eq!(route.cost, Cost::new(1));
		assert_eq!(route.hops, hops(&[10]));

		// The request goes upstream rather than to the local broadcast.
		let pending = consumer.request("room/alice", None);
		let request = queued(&server).await;
		let upstream = broadcast::Info::new().produce();
		request.serve(&upstream);
		pending.await.expect("resolves through the cheaper route");
	}

	/// A resolved broadcast names the instance of the route it came through: a stable
	/// one, or the one its publication minted.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_resolved_broadcast_carries_its_route_instance() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let _stable = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let minted = producer.route("room/bob", RouteOptions::default()).unwrap();

		let alice = consumer.request("room/alice", None).await.unwrap();
		assert_eq!(alice.info().instance, Some(instance()));
		let bob = consumer.request("room/bob", None).await.unwrap();
		let instance = minted.route().unwrap().instance;
		assert!(instance.is_some());
		assert_eq!(bob.info().instance, instance);
	}

	/// A cheaper route that appears after a front was minted takes the front over: it
	/// carries the same instance, so a newcomer joins the front.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn cheaper_route_after_a_front_takes_it_over() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let _local = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(instance(), 0).with_cost(5),
			)
			.unwrap();
		let first = consumer.request("room/alice", None).await.expect("resolves locally");

		let server = producer
			.forward(
				"room/alice",
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[10]))
					.with_cost(1),
			)
			.unwrap();
		let request = queued(&server).await;
		let upstream = broadcast::Info::new().produce();
		request.serve(&upstream);

		let second = consumer.request("room/alice", None).await.expect("resolves");
		assert!(first.is_clone(&second), "the newcomer joins the front");
	}

	#[cfg(feature = "async")]
	async fn listed(tracks: &mut broadcast::Tracks) -> String {
		match tracks.next().await {
			Ok(Some(broadcast::TracksEvent::Added { name, .. })) => format!("+{name}"),
			Ok(Some(broadcast::TracksEvent::Removed { name })) => format!("-{name}"),
			Ok(Some(broadcast::TracksEvent::Finished { name })) => format!("~{name}"),
			Ok(Some(broadcast::TracksEvent::Synced)) => "synced".into(),
			Ok(None) => "end".into(),
			Err(err) => format!("error: {err}"),
		}
	}

	/// A front lists what its serving source lists, as it changes.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_front_lists_its_source() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let source = producer.route("room/alice", RouteOptions::default()).unwrap();
		let video = source.broadcast().create_track("video", None).unwrap();

		let front = consumer.request("room/alice", None).await.unwrap();
		let mut tracks = front.tracks().unwrap();
		assert_eq!(listed(&mut tracks).await, "+video");
		assert_eq!(listed(&mut tracks).await, "synced");

		let _audio = source.broadcast().create_track("audio", None).unwrap();
		assert_eq!(listed(&mut tracks).await, "+audio");
		drop(video);
		assert_eq!(listed(&mut tracks).await, "-video");

		drop(source);
		assert_eq!(listed(&mut tracks).await, "end");
	}

	/// A source closing right after its tracks finished, held or not, ends the front's
	/// listing after those finishes, never ahead of them.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_front_forwards_the_finishes_before_its_source_closed() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let source = producer.route("room/alice", RouteOptions::default()).unwrap();
		let dropped = source.broadcast().create_track("dropped", None).unwrap();
		let held = source.broadcast().create_track("held", None).unwrap();
		let _live = source.broadcast().create_track("live", None).unwrap();

		let front = consumer.request("room/alice", None).await.unwrap();
		let mut tracks = front.tracks().unwrap();
		for event in ["+dropped", "+held", "+live", "synced"] {
			assert_eq!(listed(&mut tracks).await, event);
		}

		let dropped = dropped.finish().unwrap();
		drop(dropped);
		let _held = held.finish().unwrap();
		source.broadcast().close();
		for event in ["~dropped", "~held", "end"] {
			assert_eq!(listed(&mut tracks).await, event);
		}
	}

	/// A source's handler ending its listing while the source stays, as a session does on
	/// its peer's FIN before any retraction, ends the front's listing too.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_front_ends_its_listing_with_its_sources() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let source = producer.route("room/alice", RouteOptions::default()).unwrap();
		let lister = source.broadcast().requests().lister().unwrap();
		lister.add("video", crate::track::Info::default()).unwrap();
		lister.synced();

		let front = consumer.request("room/alice", None).await.unwrap();
		let mut tracks = front.tracks().unwrap();
		assert_eq!(listed(&mut tracks).await, "+video");
		assert_eq!(listed(&mut tracks).await, "synced");

		lister.end();
		assert_eq!(listed(&mut tracks).await, "end");
		assert!(!front.is_closed(), "the listing's end is not the broadcast's");
	}

	/// A same-instance route taking the front over lists the same tracks: readers see
	/// only what differs, once the new source's listing synced.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_front_reconciles_a_takeover() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let local = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(instance(), 0).with_cost(5),
			)
			.unwrap();
		let _a = local.broadcast().create_track("a", None).unwrap();
		let _b = local.broadcast().create_track("b", None).unwrap();

		let front = consumer.request("room/alice", None).await.unwrap();
		let mut tracks = front.tracks().unwrap();
		assert_eq!(listed(&mut tracks).await, "+a");
		assert_eq!(listed(&mut tracks).await, "+b");
		assert_eq!(listed(&mut tracks).await, "synced");

		let server = producer
			.forward(
				"room/alice",
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[10]))
					.with_cost(1),
			)
			.unwrap();
		let upstream = broadcast::Info::new().produce();
		let _b = upstream.create_track("b", None).unwrap();
		let _c = upstream.create_track("c", None).unwrap();
		queued(&server).await.serve(&upstream);

		assert_eq!(listed(&mut tracks).await, "+c");
		assert_eq!(listed(&mut tracks).await, "-a");
	}

	/// A path served on request through a covering prefix is not a broadcast anyone
	/// announced, so it has nothing to list.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_front_through_a_covering_prefix_lists_nothing() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.forward("room", RouteInfo::new()).unwrap();
		let pending = consumer.request("room/alice", None);
		let upstream = broadcast::Info::new().produce();
		let _video = upstream.create_track("video", None).unwrap();
		queued(&server).await.serve(&upstream);
		let front = pending.await.unwrap();

		let mut tracks = front.tracks().unwrap();
		assert_eq!(listed(&mut tracks).await, format!("error: {}", Error::NotFound));
	}

	/// At equal cost the local broadcast wins even over a route with no hops of its
	/// own, such as a later claim on this origin: locality is the tie-break after
	/// cost, not the newest entry.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn local_broadcast_wins_a_tie_with_a_hopless_route() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let _local = producer.route("room/alice", RouteOptions::default()).unwrap();
		let server = producer.forward("room/alice", RouteInfo::new()).unwrap();

		// Were the claim to win, the request would park on its handler forever.
		let resolved = kio::clock::timeout(Duration::from_secs(1), consumer.request("room/alice", None))
			.await
			.expect("the newer hopless route won the tie")
			.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		assert!(server.poll_requested(&kio::Waiter::noop()).is_pending());
	}

	/// Ingress announce stats count advertised intervals, not the broadcast's
	/// lifetime: nothing while hidden, one per announce, none for a re-price.
	#[kio::test]
	async fn announce_stats_follow_the_advertisement() {
		let registry = stats::Registry::new(stats::Config::new());
		let producer = origin(1).produce().with_stats(
			registry
				.tier(stats::Tier::default())
				.session(crate::stats::SessionInfo::new(crate::Path::from_static("root"))),
		);
		let announces = || {
			registry
				.snapshot()
				.traffic()
				.into_iter()
				.find(|(_, role, _)| *role == stats::Role::Subscriber)
				.map(|(_, _, traffic)| (traffic.announces_started, traffic.announces_ended))
				.unwrap_or_default()
		};

		let broadcast = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		assert_eq!(announces(), (0, 0), "a hidden broadcast is not announced");
		broadcast.announce().unwrap();
		broadcast.set_cost(3).unwrap();
		assert_eq!(announces(), (1, 0), "a re-price is not another announce");
		broadcast.withdraw();
		assert_eq!(announces(), (1, 1));
		broadcast.announce().unwrap();
		drop(broadcast);
		assert_eq!(announces(), (2, 2));
	}

	/// A route node already fed by its driver still answers a new stream with the table as
	/// it stands, including a claim announced since the driver last ran, whichever handle
	/// created it: one scoped away from the claim, upstream, proxying, and hiding hidden
	/// routes sees the whole table all the same.
	#[test]
	fn the_route_node_answers_with_the_table_as_it_stands() {
		let producer = origin(1).produce();
		(producer.clone().with_link(Link::Upstream).with_proxy(true).consume())
			.scope("elsewhere", &Patterns::from(Pattern::all()))
			.unwrap()
			.with_hidden(false)
			.route_node();
		// Nothing polls this test's driver.
		let _shown = producer.announce("room/alice", RouteInfo::new()).unwrap();
		let link = crate::route::Link {
			proxy: true,
			..Default::default()
		};
		let stream = producer.route_node().answer(link, Path::empty()).unwrap();
		assert!(
			matches!(
				stream.try_output(),
				Some(crate::route::node::Output::Route { reach: Some(_), .. })
			),
			"the new stream is sent the route to the announced claim"
		);
	}

	/// Feeding the route node counts nothing against the session whose handle created it.
	#[test]
	fn the_route_node_counts_nothing_against_its_creator() {
		let registry = stats::Registry::new(stats::Config::new());
		let session = registry
			.tier(stats::Tier::default())
			.session(crate::stats::SessionInfo::new(crate::Path::from_static("root")));
		let producer = origin(1).produce();
		let _shown = producer.announce("room/alice", RouteInfo::new()).unwrap();
		producer.consume().with_stats(session).route_node();
		assert!(
			registry.snapshot().traffic().is_empty(),
			"nothing was sent to the session"
		);
	}

	/// A stream the route node wakes may have its session polled inline, calling back
	/// into the node, so no wake goes out under the feeder's lock or the node's: neither
	/// when `route_node` catches the feeder up nor when the driver feeds it.
	#[test]
	fn the_route_node_wakes_its_streams_unlocked() {
		struct Reenter {
			consumer: Consumer,
			reentered: AtomicUsize,
			locked: AtomicBool,
		}
		impl Wake for Reenter {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				let feeder = self.consumer.shared.lock().route_node.clone().unwrap().1;
				// Re-entering a lock this thread holds would deadlock the test instead of failing it.
				if feeder.upgrade().unwrap().try_lock().is_err() {
					self.locked.store(true, Ordering::SeqCst);
					return;
				}
				self.consumer.route_node().declared();
				self.reentered.fetch_add(1, Ordering::SeqCst);
			}
		}

		let (producer, mut driver) = Producer::new(Config::from(origin(1)));
		let link = crate::route::Link {
			proxy: true,
			..Default::default()
		};
		let stream = producer.route_node().answer(link, Path::empty()).unwrap();
		let reenter = Arc::new(Reenter {
			consumer: producer.consume(),
			reentered: AtomicUsize::new(0),
			locked: AtomicBool::new(false),
		});
		let waiter = kio::Waiter::new(Waker::from(reenter.clone()));
		let park = || {
			while stream.try_output().is_some() {}
			assert!(stream.poll_output(&waiter).is_pending());
		};

		park();
		let _alice = producer.announce("room/alice", RouteInfo::new()).unwrap();
		producer.route_node();
		assert!(
			!reenter.locked.load(Ordering::SeqCst),
			"catching up woke under the feeder's lock"
		);
		assert_eq!(
			reenter.reentered.load(Ordering::SeqCst),
			1,
			"catching up woke no stream"
		);

		driver.poll(Instant::ZERO, &kio::Waiter::noop()).unwrap();
		park();
		let _bob = producer.announce("room/bob", RouteInfo::new()).unwrap();
		driver.poll(Instant::ZERO, &kio::Waiter::noop()).unwrap();
		assert!(
			!reenter.locked.load(Ordering::SeqCst),
			"the driver woke under the feeder's lock"
		);
		assert_eq!(reenter.reentered.load(Ordering::SeqCst), 2, "the driver woke no stream");
	}

	/// At equal cost the local broadcast wins.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn local_broadcast_wins_a_cost_tie() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();

		let server = producer
			.forward(
				"room/alice",
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[10]))
					.with_cost(2),
			)
			.unwrap();
		announced.expect_next_active("room/alice").await;
		let _local = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(instance(), 0).with_cost(2),
			)
			.unwrap();
		assert!(announced.expect_next_active("room/alice").await.hops.is_empty());

		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		for _ in 0..20 {
			kio::clock::sim::yield_now().await;
		}
		assert!(server.poll_requested(&kio::Waiter::noop()).is_pending());
	}

	/// Unannouncing ends the front the origin served from the broadcast and
	/// refuses new requests at once, even before the front acts on it.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn unannounce_ends_the_front() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");

		broadcast.withdraw();
		let err = consumer.request("room/alice", None).await.err().unwrap();
		assert!(matches!(err, Error::Unroutable), "joined a retracted front: {err}");
		settle(|| resolved.is_closed()).await;
		assert!(
			!broadcast.broadcast().consume().is_closed(),
			"the broadcast itself lives on"
		);

		// Announcing again serves a fresh front.
		broadcast.announce().unwrap();
		let again = consumer.request("room/alice", None).await.expect("resolves again");
		assert!(!again.is_clone(&resolved));
	}

	/// Ending a publication leaves a subscriber waiting on its track info in flight:
	/// the source can still answer and finish the copy it already handed out.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn ending_a_publication_keeps_a_track_awaiting_its_info() {
		for close in [false, true] {
			for replacement in [false, true] {
				let producer = origin(1).produce();
				let consumer = producer.consume();

				let broadcast = producer
					.route(
						"room/alice",
						RouteOptions::default().with_instance(crate::Instance::random(), 0),
					)
					.unwrap();
				let mut dynamic = broadcast.broadcast().requests();
				let resolved = consumer.request("room/alice", None).await.expect("resolves");
				let track = resolved.track("video").unwrap();
				let mut subscribing = track.subscribe(crate::track::Subscription::default().with_priority(3));
				assert!((&mut subscribing).now_or_never().is_none());
				let request = kio::clock::timeout(Duration::from_secs(1), dynamic.requested_track())
					.await
					.expect("the front asked the source")
					.expect("request");
				let _replacement = replacement.then(|| {
					producer
						.forward(
							"room/alice",
							RouteInfo::new()
								.with_instance(crate::Instance::random(), 0)
								.with_hops(hops(&[10])),
						)
						.unwrap()
				});

				if close {
					broadcast.broadcast().close();
				} else {
					broadcast.withdraw();
				}
				settle(|| resolved.is_closed()).await;
				subscribing
					.update(crate::track::Subscription::default().with_priority(7))
					.unwrap();
				settle(|| {
					request
						.subscription()
						.is_some_and(|subscription| subscription.priority == 7)
				})
				.await;

				let source = request.accept(crate::track::Info::timed());
				let mut group = source.append_next().unwrap();
				group.write_frame(group.key_time(), b"late".as_ref()).unwrap();
				group.close().unwrap();
				let source = source.finish().unwrap();

				let mut subscription = subscribing.await.expect("subscribe survives the retraction");
				assert_eq!(subscription.info(), source.info(), "the source's actual track info");
				let mut group = subscription.recv_group().await.unwrap().expect("the source's group");
				assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"late");
				assert!(matches!(subscription.recv_group().await, Ok(None)), "ends cleanly");
			}
		}
	}

	/// A front resolved through a served route, as a relay's upstream session serves
	/// one: the upstream broadcast's track requests arrive on the returned handle.
	#[cfg(feature = "async")]
	async fn served_front() -> (Claim, broadcast::Producer, broadcast::Requests, broadcast::Consumer) {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer
			.forward("room/alice", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		let upstream = broadcast::Info::new().produce();
		let dynamic = upstream.requests();
		queued(&server).await.serve(&upstream);
		let resolved = pending.await.expect("resolves");
		(server, upstream, dynamic, resolved)
	}

	/// A finished track stays readable from the front while it is read and for the
	/// linger after, then leaves the broadcast so it stops pinning its cache: the next
	/// reader asks the source afresh.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn finished_track_is_forgotten_after_the_linger() {
		let (_server, _upstream, mut dynamic, resolved) = served_front().await;

		let track = resolved.track("catalog").unwrap();
		let subscribing = kio::clock::sim::spawn(async move { track.subscribe(None).await });
		let request = kio::clock::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source")
			.expect("request");
		let source = request.request_start(None).accept(crate::track::Info::timed());
		let mut group = source.create_group(0u64.into()).unwrap();
		group.write_frame(group.key_time(), b"snapshot".as_ref()).unwrap();
		group.close().unwrap();
		let source = source.finish().unwrap();
		let mut subscription = subscribing.await.unwrap().expect("subscribe");
		assert_eq!(
			next_group(&mut subscription).await.unwrap().expect("the catalog").start,
			0
		);
		assert!(next_group(&mut subscription).await.unwrap().is_none());
		drop(subscription);
		drop(source);

		// Within the linger, a returning reader gets the finished track from the front.
		let mut subscription = resolved.track("catalog").unwrap().subscribe(None).await.unwrap();
		assert_eq!(
			next_group(&mut subscription).await.unwrap().expect("the catalog").start,
			0
		);
		assert!(next_group(&mut subscription).await.unwrap().is_none());
		drop(subscription);
		assert!(
			kio::clock::timeout(Duration::from_secs(1), dynamic.requested_track())
				.await
				.is_err(),
			"a finished track within the linger asked the source again"
		);

		// Paused time runs the front's earlier deadline before this sleep returns.
		kio::clock::sleep(track::IDLE_LINGER).await;

		let track = resolved.track("catalog").unwrap();
		let _subscribing = kio::clock::sim::spawn(async move { track.subscribe(None).await });
		kio::clock::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the finished track outlived the linger")
			.expect("request");
	}

	/// Every viewer session excludes a hop of its own that no route chain names, so
	/// they all share the plain front instead of each minting one that outlives them
	/// for as long as the route stands (#4799). Its tracks still go with the linger.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn viewer_sessions_share_the_plain_front() {
		let producer = origin(1).produce();
		// A prefix route, which resolves any path beneath it optimistically.
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let upstream = broadcast::Info::new().produce();
		let source = upstream.create_track("video", crate::track::Info::timed()).unwrap();
		let mut group = source.append_next().unwrap();
		group.write_frame(group.key_time(), b"live".as_ref()).unwrap();

		let counts = || {
			let table = producer.shared.lock();
			let watches = table
				.routes
				.root
				.find(Path::from_static("room/alice").parts())
				.map_or(0, |node| node.watches.len());
			(table.fronts.len(), watches)
		};

		let mut first = None;
		let mut front = None;
		let mut budget = kio::coop::Budget::new(32);
		for viewer in 0..1000 {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			let session = producer.consume().excluding(Hop::random());
			let pending = session.request("room/alice", None);
			if viewer == 0 {
				queued(&server).await.serve(&upstream);
			}
			let resolved = pending.await.expect("resolves");
			let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
			next_group(&mut subscription).await.unwrap().expect("the live group");
			drop(subscription);
			first.get_or_insert_with(counts);
			front = Some(resolved);
		}
		assert_eq!(first, Some((1, 1)), "one front and one watch for the first viewer");
		assert_eq!(counts(), (1, 1), "viewers minted fronts of their own");

		// The front outlives its viewers while the route stands, but not their track.
		let front = front.unwrap();
		assert_eq!(front.open_tracks(), 1, "the unread track lingers");
		kio::clock::sleep(track::IDLE_LINGER).await;
		settle(|| front.open_tracks() == 0).await;
	}

	/// A requester excluding a peer that a covering route passes through gets a front
	/// of its own. Decided per request: a peer whose hop joins a chain after it joined
	/// the plain front gets the filtered one on its next request.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_hop_in_a_chain_gets_its_own_front() {
		let producer = origin(1).produce();
		let _incumbent = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let fronts = || producer.shared.lock().fronts.len();

		let peer = producer.consume().excluding(origin(7));
		let _local = producer.consume().request("room/alice", None);
		let _peer = peer.request("room/alice", None);
		assert_eq!(fronts(), 1, "a hop no chain names excludes nothing");

		// The same publisher, now also reached through the peer.
		let _echo = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10, 7])))
			.unwrap();
		let _filtered = peer.request("room/alice", None);
		assert_eq!(fronts(), 2, "the peer still shares the plain front");
		let _viewer = producer.consume().excluding(origin(8)).request("room/alice", None);
		assert_eq!(fronts(), 2, "a viewer left the plain front");
	}

	/// A front ends once nothing reads it for the linger and nothing holds its
	/// broadcast, rather than for as long as its route stands, and lets go of its source.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_unread_front_ends_after_the_linger() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let upstream = broadcast::Info::new().produce();
		let source = upstream.create_track("video", crate::track::Info::timed()).unwrap();
		let mut group = source.append_next().unwrap();
		group.write_frame(group.key_time(), b"live".as_ref()).unwrap();
		let fronts = || producer.shared.lock().fronts.len();

		let pending = consumer.request("room/alice", None);
		queued(&server).await.serve(&upstream);
		let resolved = pending.await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		next_group(&mut subscription).await.unwrap().expect("the live group");
		let front = resolved.weak();
		drop(subscription);
		drop(resolved);

		// Within the linger the unread track keeps its front for a returning reader.
		kio::clock::sleep(track::IDLE_LINGER / 2).await;
		assert_eq!(fronts(), 1);
		assert!(upstream.is_held(), "the front let go of its source early");

		kio::clock::sleep(track::IDLE_LINGER).await;
		settle(|| fronts() == 0).await;
		assert!(front.is_closed());
		assert!(!upstream.is_held(), "an ended front still holds its source");
	}

	/// A request racing a front's retirement never joins it as it ends: one that holds
	/// the front before its driver looks keeps it, and one after mints a fresh front.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_racing_a_front_retiring_never_joins_it_ending() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let _broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let fronts = || producer.shared.lock().fronts.len();

		let first = consumer.request("room/alice", None).await.expect("resolves");
		let front = first.weak();
		drop(first);
		let again = consumer.request("room/alice", None).await.expect("joins in time");
		assert!(again.is_clone(&front.consume()), "a request in time keeps the front");

		drop(again);
		settle(|| front.is_closed()).await;
		assert_eq!(fronts(), 0, "a retired front left the table");
		let fresh = consumer.request("room/alice", None).await.expect("a fresh front");
		assert!(!fresh.is_closed());
		assert_eq!(fronts(), 1);
	}

	/// A track that ends while read is unread from then on, even with its reader still
	/// holding it: the front sees it go unread once and lets it linger, rather than
	/// keeping it read forever or spinning on it while it lingers ended and unread.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_track_ending_while_read_lets_its_front_retire() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let upstream = broadcast::Info::new().produce();
		let source = upstream.create_track("video", None).unwrap();
		let fronts = || producer.shared.lock().fronts.len();

		let pending = consumer.request("room/alice", None);
		queued(&server).await.serve(&upstream);
		let resolved = pending.await.expect("resolves");
		let reader = resolved.track("video").unwrap().subscribe(None);
		settle(|| source.demand().is_wanted()).await;

		// The source refuses the track before delivering, which ends it for the reader.
		source.abort(Error::NotFound).unwrap();
		drop(resolved);

		kio::clock::sleep(track::IDLE_LINGER).await;
		settle(|| fronts() == 0).await;
		drop(reader);
	}

	/// A front whose broadcast closes under it ends, rather than polling a holder edge
	/// that a closed broadcast answers at once. Nothing but the front closes it today, so
	/// the test does; a regression spins inside one poll and hangs the test.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_front_whose_broadcast_closes_ends() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let _published = producer.route("room/alice", RouteOptions::default()).unwrap();
		let path = consumer.absolute("room/alice").unwrap();
		let broadcast = broadcast::Info::new().produce();
		// Held, so the front waits on its holder rather than retiring on its own.
		let holder = broadcast.consume();
		let watch = consumer.shared.lock().watch(&consumer.shared, &path);
		let task = FrontTask {
			shared: consumer.shared.clone(),
			broadcast: broadcast.clone(),
			path,
			horizon: Horizon::default(),
			pin: None,
			watch,
			// Nothing joins this front, and its first selection overwrites the instance.
			request: kio::Producer::new(PendingFront {
				resolved: None,
				selected: instance(),
				refused: false,
			}),
			timers: consumer.timers.clone(),
		};
		let mut front = kio::Pending::new(FrontServe::new(task));
		assert!((&mut front).now_or_never().is_none(), "the front ended early");

		broadcast.close();
		let in_flight = kio::clock::timeout(Duration::from_secs(1), front)
			.await
			.expect("the front outlived its broadcast");
		assert!(in_flight.is_empty());
		drop(holder);
	}

	/// The filtered front a peer session gets for a chain through it goes once the peer
	/// stops reading it, leaving the plain front its viewers share.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_peers_filtered_front_ends_once_unread() {
		let producer = origin(1).produce();
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		// The same publisher, also reached through the peer, at a price the plain front skips.
		let _echo = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10, 7])).with_cost(100))
			.unwrap();
		let upstream = broadcast::Info::new().produce();
		let source = upstream.create_track("video", crate::track::Info::timed()).unwrap();
		let mut group = source.append_next().unwrap();
		group.write_frame(group.key_time(), b"live".as_ref()).unwrap();
		let fronts = || producer.shared.lock().fronts.len();

		let pending = producer.consume().request("room/alice", None);
		queued(&server).await.serve(&upstream);
		let _viewer = pending.await.expect("the plain front resolves");

		let peer = producer.consume().excluding(origin(7));
		let peered = peer.request("room/alice", None).await.expect("resolves");
		assert_eq!(fronts(), 2, "the peer has a front of its own");
		let mut subscription = peered.track("video").unwrap().subscribe(None).await.unwrap();
		next_group(&mut subscription).await.unwrap().expect("the live group");

		// The peer session closes.
		drop(subscription);
		drop(peered);
		drop(peer);
		kio::clock::sleep(track::IDLE_LINGER).await;
		settle(|| fronts() == 1).await;
	}

	/// The hairpin the per-request choice accepts ends on its own. A peer still on the
	/// plain front when the only route left runs through it gets that route, but its own
	/// view withdraws the path, so it stops serving the path back to us.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_hairpin_through_the_plain_front_is_withdrawn() {
		let producer = origin(1).produce();
		let peer = producer.consume().excluding(origin(7));
		let mut view = peer.routes();
		let incumbent = producer
			.forward(
				"room",
				RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[10])),
			)
			.unwrap();
		view.expect_next_active("room").await;

		let pending = peer.request("room/alice", None);
		let source = broadcast::Info::new().produce();
		queued(&incumbent).await.serve(&source);
		let resolved = pending.await.expect("resolves through the incumbent");

		// The same publisher, now also reached through the peer, outlives the incumbent.
		let echo = producer
			.forward(
				"room",
				RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[10, 7])),
			)
			.unwrap();
		drop(incumbent);
		drop(source);

		let replacement = broadcast::Info::new().produce();
		queued(&echo).await.serve(&replacement);
		resolved.assert_not_closed();
		view.expect_next_offline("room").await;
	}

	/// A route that ends abruptly releases its open groups, and the next route may
	/// deliver only the frames past the break. The reader mid-group carries on from
	/// there, and a reader arriving afterwards fetches the whole group from that route:
	/// whether the old route dies before the new one takes over (a reconnect) or after
	/// (a cheaper route preempting it). A reader joining mid-outage gets it too, and the
	/// reader already mid-group is not handed it a second time.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_takeover_serves_the_open_group_head_to_later_readers() {
		for dies_first in [true, false] {
			takeover_keeps_the_open_group_head(dies_first).await;
		}
	}

	#[cfg(feature = "async")]
	async fn takeover_keeps_the_open_group_head(dies_first: bool) {
		let producer = origin(1).produce();
		let first_server = producer
			.forward(
				"room/alice",
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[10]))
					.with_cost(5),
			)
			.unwrap();
		let pending = producer.consume().request("room/alice", None);
		let upstream = broadcast::Info::new().produce();
		let mut dynamic = upstream.requests();
		queued(&first_server).await.serve(&upstream);
		let resolved = pending.await.unwrap();

		#[cfg(feature = "async")]
		async fn read(group: &mut crate::group::Consumer) -> Vec<u8> {
			let frame = kio::clock::timeout(Duration::from_secs(1), group.read_frame())
				.await
				.expect("frame")
				.unwrap()
				.expect("group ended");
			frame.payload.to_vec()
		}

		// A reader is mid-way through the open group when the route changes.
		let track = resolved.track("log").unwrap();
		let subscribing = kio::clock::sim::spawn(async move { track.subscribe(None).await });
		let source = dynamic
			.requested_track()
			.await
			.unwrap()
			.accept(crate::track::Info::timed());
		let mut group = source.create_group(0u64.into()).unwrap();
		group.write_frame(group.key_time(), b"a".as_ref()).unwrap();
		group.write_frame(group.key_time(), b"b".as_ref()).unwrap();
		let mut subscription = subscribing.await.unwrap().unwrap();
		let mut reading = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(read(&mut reading).await, b"a");
		assert_eq!(read(&mut reading).await, b"b");

		// Either the first route's session ends and reconnects, announcing the same route
		// afresh while the old one is still held, or a cheaper route preempts the live one.
		let mut first = Some((group, source, upstream, dynamic));
		let mut joining = None;
		if dies_first {
			drop(first.take());
			// The reader sees its route die and stalls. Polled in place: yielding would let
			// the front give up on a path with no route left before the replacement lands.
			assert!(futures::FutureExt::now_or_never(subscription.recv_group()).is_none());
			let track = resolved.track("log").unwrap();
			joining = Some(kio::clock::sim::spawn(async move {
				let mut joined = track.subscribe(None).await.unwrap();
				let group = joined.recv_group().await.unwrap().expect("track ended");
				(joined, group)
			}));
		}
		let cost = if dies_first { 5 } else { 0 };
		let replacement_server = producer
			.forward(
				"room/alice",
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[10]))
					.with_cost(cost),
			)
			.unwrap();
		let replacement = broadcast::Info::new().produce();
		let mut replacement_dynamic = replacement.requests();
		queued(&replacement_server).await.serve(&replacement);
		let request = kio::clock::timeout(Duration::from_secs(1), replacement_dynamic.requested_track())
			.await
			.expect("the front asked the replacement")
			.unwrap();
		// The replacement serves the whole group to a fetch, frames still to come included.
		let fetches = request.requests();
		let fetched = Arc::new(std::sync::Mutex::new(Vec::<crate::group::Producer>::new()));
		let serving = fetched.clone();
		let _fetching = kio::clock::sim::spawn(async move {
			while let Ok(request) = fetches.requested_fill_group().await {
				let mut group = request.accept(crate::track::Info::timed()).unwrap();
				for frame in [b"a", b"b", b"c"] {
					group.write_frame(group.key_time(), frame.as_ref()).unwrap();
				}
				serving.lock().unwrap().push(group);
			}
		});
		let mut next = request.request_start(None).accept(crate::track::Info::timed());
		next.start_at(0).unwrap();
		let mut next_group = next.create_group(0u64.into()).unwrap();
		next_group.start_at(2).unwrap();
		next_group.write_frame(next_group.key_time(), b"c".as_ref()).unwrap();
		assert_eq!(read(&mut reading).await, b"c", "dies_first={dies_first}");

		// A preempted route still owns its open group: its writer and other readers carry
		// on, and its copy of the frame the replacement already wrote is a duplicate.
		if let Some((group, ..)) = &mut first {
			let mut independent = group.consume();
			group.write_frame(group.key_time(), b"c".as_ref()).unwrap();
			for expect in [b"a", b"b", b"c"] {
				assert_eq!(read(&mut independent).await, expect);
			}
		}
		drop(first);

		let track = resolved.track("log").unwrap();
		let mut later = kio::clock::timeout(Duration::from_secs(1), track.subscribe(None))
			.await
			.expect("subscribe")
			.unwrap();
		let mut late = kio::clock::timeout(Duration::from_secs(1), later.recv_group())
			.await
			.unwrap_or_else(|_| panic!("dies_first={dies_first}: the later reader never got the group"))
			.unwrap()
			.expect("track ended");
		assert_eq!(late.start, 0);
		for expect in [b"a", b"b", b"c"] {
			assert_eq!(read(&mut late).await, expect, "dies_first={dies_first}");
		}
		let mut joined = None;
		if let Some(joining) = joining {
			let (subscription, mut group) = kio::clock::timeout(Duration::from_secs(1), joining)
				.await
				.expect("the reader joining mid-outage never got the group")
				.unwrap();
			assert_eq!(group.start, 0);
			for expect in [b"a", b"b", b"c"] {
				assert_eq!(read(&mut group).await, expect);
			}
			joined = Some((subscription, group));
		}

		// The reader that was mid-group already has it.
		assert!(
			kio::clock::timeout(Duration::from_secs(1), subscription.recv_group())
				.await
				.is_err(),
			"dies_first={dies_first}: the group was handed out twice"
		);

		// The continuation still flows to both.
		next_group.write_frame(next_group.key_time(), b"d".as_ref()).unwrap();
		for group in fetched.lock().unwrap().iter_mut() {
			group.write_frame(group.key_time(), b"d".as_ref()).unwrap();
		}
		assert_eq!(read(&mut reading).await, b"d");
		assert_eq!(read(&mut late).await, b"d");
		if let Some((_, group)) = &mut joined {
			assert_eq!(read(group).await, b"d");
		}
		next_group.close().unwrap();
		drop((
			joined,
			reading,
			late,
			subscription,
			later,
			next,
			replacement,
			replacement_server,
			first_server,
		));
	}

	/// The same holds for a reader returning to a parked track: its warm cache
	/// does not stand in for the copy it is waiting on.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn unannounce_keeps_a_returning_reader_awaiting_its_info() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let mut dynamic = broadcast.broadcast().requests();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let track = resolved.track("video").unwrap();
		let subscribing = kio::clock::sim::spawn(async move { track.subscribe(None).await });
		let request = kio::clock::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source")
			.expect("request");
		let source = request.accept(crate::track::Info::timed());
		let mut group = source.append_next().unwrap();
		group.write_frame(group.key_time(), b"cached".as_ref()).unwrap();
		group.close().unwrap();
		let mut subscription = subscribing.await.unwrap().expect("subscribe");
		subscription.recv_group().await.unwrap().expect("the cached group");
		drop(subscription);

		// Parked: the source copy goes, the delivered group stays warm. The source
		// then tears its idle track down, so a returning reader asks it afresh.
		kio::clock::timeout(Duration::from_secs(1), source.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("parked")
			.expect("source open");
		drop(source);
		let track = resolved.track("video").unwrap();
		let subscribing = kio::clock::sim::spawn(async move { track.subscribe(None).await });
		let request = kio::clock::timeout(Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("the front asked the source again")
			.expect("request");

		broadcast.withdraw();
		settle(|| resolved.is_closed()).await;

		// A fresh source copy numbers groups past what the cache already delivered.
		let source = request.accept(crate::track::Info::timed());
		let mut group = source.create_group(1u64.into()).unwrap();
		group.write_frame(group.key_time(), b"late".as_ref()).unwrap();
		group.close().unwrap();
		let _source = source.finish().unwrap();

		let mut subscription = subscribing.await.unwrap().expect("subscribe survives the retraction");
		let mut payloads = Vec::new();
		while let Some(mut group) = subscription.recv_group().await.expect("ends cleanly") {
			payloads.push(group.read_frame().await.unwrap().unwrap().payload);
		}
		assert_eq!(payloads.last().map(|p| &p[..]), Some(&b"late"[..]));
	}

	/// Once a front observes a gap, a request after the route returns cannot join it while
	/// its terminal actions are still being applied on another thread.
	#[test]
	fn a_request_after_a_gap_skips_the_front_already_ending() {
		let (producer, mut driver) = Producer::new(Config::default());
		let consumer = producer.consume();
		let publication = producer.route("room/alice", RouteOptions::default()).unwrap();
		let path = consumer.absolute("room/alice").unwrap();
		let key = (path.clone(), Horizon::default(), None);
		let broadcast = broadcast::Info::new().produce();
		let held = broadcast.consume();
		let (request, watch) = {
			let mut state = consumer.shared.lock();
			let selected = state
				.best_route(&path, Horizon::default(), &HashSet::new(), |_| true)
				.unwrap()
				.instance
				.clone();
			let request = kio::Producer::new(PendingFront {
				resolved: None,
				selected,
				refused: false,
			});
			state.fronts.insert(
				key,
				RemoteFront {
					request: request.clone(),
					broadcast: held.weak(),
				},
			);
			let watch = state.watch(&consumer.shared, &path);
			(request, watch)
		};
		let mut front = FrontServe::new(FrontTask {
			shared: consumer.shared.clone(),
			broadcast,
			path,
			horizon: Horizon::default(),
			pin: None,
			watch,
			request,
			timers: consumer.timers.clone(),
		});
		let waiter = kio::Waiter::noop();
		assert!(kio::Task::poll(&mut front, &waiter).is_pending());
		let Poll::Ready(Ok(first)) = consumer
			.lookup(Path::new("room/alice").unwrap(), None)
			.into_inner()
			.poll_ok(&waiter)
		else {
			panic!("the first front did not resolve")
		};

		publication.withdraw();
		let ending = front.select();
		assert!(matches!(ending, Event::Selected { best: None, .. }));
		publication.announce().unwrap();
		let next = consumer.lookup(Path::new("room/alice").unwrap(), None).into_inner();
		assert!(next.poll_ok(&waiter).is_pending(), "joined the front already ending");

		front.events.push_back(ending);
		assert!(kio::Task::poll(&mut front, &waiter).is_ready());
		driver.poll(Instant::ZERO, &waiter).unwrap();
		let Poll::Ready(Ok(fresh)) = next.poll_ok(&waiter) else {
			panic!("the fresh front did not resolve")
		};
		assert!(!fresh.is_clone(&first));
		assert!(!fresh.is_closed());
		assert!(fresh.track("video").is_ok());
	}

	/// A re-announce that lands before the front acts on the retraction reuses
	/// the same route entry, so the front carries on.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn reannounce_before_the_front_acts_keeps_it() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");

		broadcast.withdraw();
		broadcast.announce().unwrap();
		for _ in 0..20 {
			kio::clock::sim::yield_now().await;
		}
		assert!(!resolved.is_closed(), "the front ended across a reannouncement");
		let again = consumer.request("room/alice", None).await.expect("resolves");
		assert!(again.is_clone(&resolved));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn local_broadcast_resolves_once_announced() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Created but not announced: nobody can reach it, locally included.
		let broadcast = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		let err = consumer
			.request("room/alice", None)
			.now_or_never()
			.expect("unroutable is synchronous")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		broadcast.announce().unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "room/alice");
		drop(broadcast);

		// Nothing covers an unknown path and no handler exists.
		let err = consumer
			.request("room/bob", None)
			.now_or_never()
			.expect("unroutable is synchronous")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
	}

	#[kio::test]
	async fn route_accepts_a_path_at_the_wire_limits() {
		let producer = origin(1).produce();
		let path = vec!["a"; Path::MAX_PARTS].join("/");
		let _broadcast = producer
			.route(path.as_str(), RouteOptions::hidden())
			.expect("max depth is allowed");
		let deeper = vec!["a"; Path::MAX_PARTS + 1].join("/");
		assert!(matches!(
			producer.route(deeper.as_str(), RouteOptions::hidden()),
			Err(Error::BoundsExceeded(_))
		));

		// The byte limit counts the root the producer joins on.
		let rooted = producer.scope("r", &Patterns::from(Pattern::all())).unwrap();
		let longest = "b".repeat(Path::MAX_BYTES - 2);
		let _long = rooted
			.route(longest.as_str(), RouteOptions::hidden())
			.expect("max length is allowed");
		let longer = "c".repeat(Path::MAX_BYTES - 1);
		assert!(matches!(
			rooted.route(longer.as_str(), RouteOptions::hidden()),
			Err(Error::BoundsExceeded(_))
		));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn duplicate_routes_aggregate_until_the_last_leaves() {
		let producer = origin(1).produce();
		let first = producer.forward("live", RouteInfo::new().with_cost(3)).unwrap();
		let second = producer.forward("live", RouteInfo::new().with_cost(1)).unwrap();

		let mut announced = producer.consume().routes();
		let Some(Some(RouteEvent {
			kind: RouteKind::Start,
			announce: update,
		})) = announced.next().now_or_never()
		else {
			panic!("expected the announcement");
		};
		assert_eq!(update.prefix.as_str(), "live");
		assert_eq!(update.route.cost, Cost::new(1));
		announced.assert_next_wait();

		// Under another instance the survivor is another source.
		drop(second);
		assert_eq!(announced.expect_next_restarted("live").await.cost, Cost::new(3));

		drop(first);
		announced.expect_next_offline("live").await;
		announced.assert_next_wait();
	}

	#[kio::test]
	async fn dynamic_may_cover_a_scope_but_disjoint_prefixes_are_refused() {
		let producer = origin(1).produce();
		let scoped = producer.scope("", &scopes(&["room"])).unwrap();
		let _broad = scoped
			.forward("", RouteInfo::new())
			.expect("an overlapping prefix is accepted");

		let _ok = scoped
			.forward("room/alice", RouteInfo::new())
			.expect("a contained prefix is accepted");
		assert!(matches!(
			scoped.forward("other", RouteInfo::new()),
			Err(Error::Unauthorized)
		));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn forward_keeps_its_producer_scope() {
		let producer = origin(1).produce();
		let scope = Patterns::from("*/chat".parse::<Pattern>().unwrap());
		let scoped = producer.scope("", &scope).unwrap();
		let dynamic = scoped.forward("", RouteInfo::new()).unwrap();

		let mut matching = producer
			.consume()
			.scope("", &scopes(&["room/chat"]))
			.unwrap()
			.covering()
			.routes();
		matching.expect_next_active("").await;
		let mut outside = producer
			.consume()
			.scope("", &scopes(&["room/video"]))
			.unwrap()
			.covering()
			.routes();
		outside.assert_next_wait();

		let refused = producer
			.consume()
			.request("room/video", None)
			.now_or_never()
			.expect("an out-of-scope request must be refused synchronously");
		assert!(matches!(refused, Err(Error::Unroutable)));
		assert!(dynamic.requested().now_or_never().is_none());

		let _pending = producer.consume().request("room/chat", None);
		let request = queued(&dynamic).await;
		assert_eq!(request.path().as_str(), "room/chat");
	}

	#[kio::test]
	async fn scoped_cursor_selects_among_the_routes_it_can_see() {
		let producer = origin(1).produce();
		let scoped = |pattern: &str| {
			producer
				.scope("", &Patterns::from(pattern.parse::<Pattern>().unwrap()))
				.unwrap()
		};
		let _chat = scoped("*/chat").forward("", RouteInfo::new().with_cost(1)).unwrap();
		let _video = scoped("*/video").forward("", RouteInfo::new().with_cost(5)).unwrap();

		let mut video = producer
			.consume()
			.scope("", &scopes(&["room/video"]))
			.unwrap()
			.covering()
			.routes();
		assert_eq!(video.expect_next_active("").await.cost, Cost::new(5));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn route_changes_preserve_each_cursors_visible_winner() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut all = consumer.clone().routes();
		let mut excluded = consumer.clone().excluding(origin(7)).routes();
		let mut video = consumer
			.clone()
			.scope("", &scopes(&["room/live/video"]))
			.unwrap()
			.covering()
			.routes();
		let mut rooted = consumer
			.scope("room/live", &Patterns::from(Pattern::all()))
			.unwrap()
			.routes();

		let chat = producer
			.scope("", &scopes(&["room/live/chat"]))
			.unwrap()
			.forward("room/live", RouteInfo::new().with_hops(hops(&[7])).with_cost(1))
			.unwrap();
		assert_eq!(all.expect_next_active("room/live").await.cost, Cost::new(1));
		assert_eq!(rooted.expect_next_active("").await.cost, Cost::new(1));
		excluded.assert_next_wait();
		video.assert_next_wait();

		let video_route = producer
			.scope("", &scopes(&["room/live/video"]))
			.unwrap()
			.forward("room/live", RouteInfo::new().with_hops(hops(&[8])).with_cost(5))
			.unwrap();
		all.assert_next_wait();
		rooted.assert_next_wait();
		assert_eq!(excluded.expect_next_active("room/live").await.cost, Cost::new(5));
		assert_eq!(video.expect_next_active("room/live").await.cost, Cost::new(5));

		chat.update(RouteInfo::new().with_hops(hops(&[7])).with_cost(9))
			.unwrap();
		assert_eq!(all.expect_next_active("room/live").await.cost, Cost::new(5));
		assert_eq!(rooted.expect_next_active("").await.cost, Cost::new(5));
		excluded.assert_next_wait();
		video.assert_next_wait();

		drop(video_route);
		assert_eq!(all.expect_next_active("room/live").await.cost, Cost::new(9));
		assert_eq!(rooted.expect_next_active("").await.cost, Cost::new(9));
		excluded.expect_next_offline("room/live").await;
		video.expect_next_offline("room/live").await;
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn root_cursor_keeps_the_more_specific_covering_route() {
		let producer = origin(1).produce();
		let broad = producer.forward("room", RouteInfo::new().with_cost(1)).unwrap();
		let narrow = producer.forward("room/live", RouteInfo::new().with_cost(9)).unwrap();
		let mut announced = producer
			.consume()
			.scope("room/live/video", &Patterns::from(Pattern::all()))
			.unwrap()
			.covering()
			.routes();
		assert_eq!(announced.expect_next_active("").await.cost, Cost::new(9));

		broad.update(RouteInfo::new().with_cost(0)).unwrap();
		announced.assert_next_wait();
		narrow.update(RouteInfo::new().with_cost(8)).unwrap();
		assert_eq!(announced.expect_next_active("").await.cost, Cost::new(8));
		drop(narrow);
		assert_eq!(announced.expect_next_active("").await.cost, Cost::new(0));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn dynamic_accepts_a_max_depth_prefix() {
		let producer = origin(1).produce();
		let path = (0..Path::MAX_PARTS)
			.map(|i| format!("s{i}"))
			.collect::<Vec<_>>()
			.join("/");
		let mut announced = producer.consume().routes();

		let dynamic = producer.forward(&path, RouteInfo::new()).expect("max depth is allowed");
		announced.expect_next_active(&path).await;

		let _pending = producer.consume().request(&path, None);
		let request = queued(&dynamic).await;
		assert_eq!(request.path().as_str(), path);
	}

	#[kio::test]
	async fn dynamic_exclusion_skips_routes_through_the_subscriber() {
		let producer = origin(1).produce();
		let _server = producer
			.forward("live", RouteInfo::new().with_hops(hops(&[7])))
			.unwrap();

		let mut excluded = producer.consume().excluding(origin(7)).routes();
		excluded.assert_next_wait();

		let mut clean = producer.consume().excluding(origin(8)).routes();
		clean.expect_next_active("live").await;
	}

	/// The consumer is a `Stream` of the same updates as `next`.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn announce_consumer_is_a_stream() {
		use futures::StreamExt;
		let producer = origin(1).produce();
		let server = producer.forward("live", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		let mut next = || StreamExt::next(&mut announced).now_or_never();
		let Some(Some(RouteEvent {
			kind: RouteKind::Start,
			announce: update,
		})) = next()
		else {
			panic!("expected the replayed route");
		};
		assert_eq!(update.prefix.as_str(), "live");
		assert!(next().is_none());
		drop(server);
		assert!(matches!(
			next(),
			Some(Some(RouteEvent {
				kind: RouteKind::Offline,
				..
			}))
		));
	}

	#[kio::test]
	async fn dynamic_retracts() {
		let producer = origin(1).produce();
		let server = producer.forward("live", RouteInfo::new()).unwrap();
		let mut announced = producer.consume().routes();
		announced.expect_next_active("live").await;

		drop(server);
		announced.expect_next_offline("live").await;
	}

	#[kio::test]
	async fn local_broadcast_is_invisible_until_announced() {
		let producer = origin(1).produce();
		let mut local = producer.consume().routes();
		let mut peer = producer.consume().excluding(Hop::UNKNOWN).routes();
		let broadcast = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		local.assert_next_wait();
		peer.assert_next_wait();

		broadcast.announce().unwrap();
		local.expect_next_active("room/alice").await;
		peer.expect_next_active("room/alice").await;

		drop(broadcast);
		local.expect_next_ended("room/alice").await;
		peer.expect_next_ended("room/alice").await;
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn served_route_materializes_on_demand() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let server = producer.claim("room", RouteOptions::default()).unwrap();

		let pending = consumer.request("room/alice", None);
		let request = queued(&server).await;
		assert_eq!(request.path().as_str(), "room/alice");

		let source = broadcast::Info::new().produce();
		request.serve(&source);

		let resolved = pending.await.expect("resolves");
		// The handle is named by what the requester asked for.
		assert_eq!(resolved.info().path.as_str(), "room/alice");

		// A repeat request shares the served broadcast instead of re-asking.
		let again = consumer.request("room/alice", None).await.expect("resolves");
		assert!(again.is_clone(&resolved));
	}

	/// An excluded producer can neither publish into the excluded subtree nor serve
	/// a request there through a covering prefix route.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn excluded_subtree_is_never_published_or_served() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let excluded = producer.exclude(".stats").unwrap();
		assert!(excluded.route(".stats/node", RouteOptions::default()).is_err());
		assert!(excluded.claim(".stats", RouteOptions::default()).is_err());

		let server = excluded.claim("", RouteOptions::default()).unwrap();
		let Err(err) = consumer.request(".stats/evil", None).await else {
			panic!("excluded path served")
		};
		assert!(matches!(err, Error::Unroutable), "got {err:?}");

		let pending = consumer.request("room/alice", None);
		let request = queued(&server).await;
		assert_eq!(request.path().as_str(), "room/alice");
		request.serve(broadcast::Info::new().produce());
		pending.await.expect("resolves outside the excluded subtree");
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn served_requests_coalesce() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.forward("room", RouteInfo::new()).unwrap();

		let first = consumer.request("room/alice", None);
		let second = consumer.request("room/alice", None);

		let request = queued(&server).await;
		// Only one request reaches the server.
		assert!(server.poll_requested(&kio::Waiter::noop()).is_pending());

		let source = broadcast::Info::new().produce();
		request.serve(&source);

		let first = first.await.expect("resolves");
		let second = second.await.expect("resolves");
		assert!(first.is_clone(&second));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn retract_rejects_pending_requests() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.forward("room", RouteInfo::new()).unwrap();

		let pending = consumer.request("room/alice", None);
		drop(server);

		let err = pending.await.err().unwrap();
		assert!(matches!(err, Error::Unroutable));

		// With the route gone, later requests are unroutable immediately.
		let err = consumer
			.request("room/alice", None)
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn follow_survives_serving_route_retraction() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Three identical routes, oldest first: the newest identical route wins
		// requests, and swapping between them emits no announce update.
		let standby_server = producer.forward("room", RouteInfo::new()).unwrap();
		let second_server = producer.forward("room", RouteInfo::new()).unwrap();
		let incumbent_server = producer.forward("room", RouteInfo::new()).unwrap();

		let mut following = consumer.follow("room/alice", None).unwrap();
		let mut resolving = Box::pin(following.available());
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
		request.serve(&source);

		let resolved = resolving.await.expect("resolves via the standby").unwrap();
		assert_eq!(resolved.info().path.as_str(), "room/alice");
	}

	#[kio::test]
	async fn split_horizon_skips_routes_through_the_requester() {
		let producer = origin(1).produce();
		let _server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[7])))
			.unwrap();

		// The requester's own bytes must not be served back to it.
		let excluded = producer.consume().excluding(origin(7));
		let err = excluded
			.request("room/alice", None)
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		// A clean requester resolves through the route (the request queues).
		let clean = producer.consume().excluding(origin(8));
		let pending = clean.request("room/alice", None);
		assert!(pending.now_or_never().is_none());
	}

	#[kio::test]
	async fn routes_report_where_they_entered() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let mut announced = producer.consume().routes();

		let _ingest = producer
			.forward("client", RouteInfo::new().with_hops(hops(&[5])).with_via(origin(5)))
			.unwrap();
		let _gateway = producer.route("gateway", RouteOptions::default()).unwrap();
		let _forwarded = peer
			.forward(
				"forwarded",
				RouteInfo::new().with_hops(hops(&[5, 7])).with_via(origin(7)),
			)
			.unwrap();

		assert_eq!(announced.expect_next_active("client").await.source(), Source::Local);
		assert_eq!(announced.expect_next_active("gateway").await.source(), Source::Local);
		assert_eq!(
			announced.expect_next_active("forwarded").await.source(),
			Source::Peer(7)
		);

		// The mark survives narrowing the handle.
		let scoped = peer.scope("room", &Patterns::from(Pattern::all())).unwrap();
		let _nested = scoped.forward("x", RouteInfo::new().with_via(origin(8))).unwrap();
		assert_eq!(announced.expect_next_active("room/x").await.source(), Source::Peer(8));
	}

	/// A caller that only polls with `try_next` still gets a held update once its
	/// hold passes. Nothing else wakes the idle driver, so the hold's own timer
	/// must, or the origin's clock never reaches the deadline.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn try_next_delivers_a_held_update() {
		// Let paused time pass and every task woken by it run.
		#[cfg(feature = "async")]
		async fn step(by: Duration) {
			kio::clock::sim::advance(by).await;
			for _ in 0..10 {
				kio::clock::sim::yield_now().await;
			}
		}

		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let mut announced = producer.consume().routes();
		step(Duration::from_millis(1)).await;

		// One instance, so the winner moving between routes is an update, not a restart.
		let route = |chain: &[u64], via| {
			RouteInfo::new()
				.with_instance(instance(), 0)
				.with_hops(hops(chain))
				.with_via(origin(via))
		};
		let near = peer.forward("room", route(&[9, 2], 2)).unwrap();
		announced.assert_try_next_active("room");

		let _far = peer.forward("room", route(&[9, 3, 4], 4)).unwrap();
		drop(near);
		assert!(announced.try_next().is_none(), "the update is held");
		step(Duration::from_millis(1)).await;
		assert!(announced.try_next().is_none(), "the update is still held");
		step(DEFAULT_UPDATE_HOLD).await;
		match announced.try_next().expect("the hold passed") {
			RouteEvent {
				kind: RouteKind::Update,
				announce: update,
			} => assert_eq!(update.route.hops, hops(&[9, 3, 4])),
			other => panic!("should be an update: got {other:?}"),
		}
	}

	/// Two peers carry the same `k` broadcasts of one publisher, and both go away a
	/// broadcast at a time, the cursor polled after each change. The first peer's
	/// withdrawals queue `k` held updates; the second's, in the opposite order, make
	/// each ready behind the held rest. A poll touches only what is due, so the wave
	/// costs each cursor O(k log k) steps, not O(k²).
	#[test]
	fn a_withdrawal_wave_costs_k_log_k_steps() {
		for k in [64usize, 256, 1024] {
			let (producer, mut driver) = Producer::new(Config::from(origin(1)));
			driver
				.poll(
					Instant::ZERO.saturating_add(Duration::from_secs(1)),
					&kio::Waiter::noop(),
				)
				.unwrap();
			let peer = producer.clone().with_link(Link::Peer);
			// One instance, so the winner moving between routes is an update, not a restart.
			let route = |chain: &[u64], via| {
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(chain))
					.with_via(origin(via))
			};
			let mut near = Vec::new();
			let mut far = Vec::new();
			for i in 0..k {
				near.push(peer.forward(format!("room/{i}"), route(&[9, 2], 2)).unwrap());
				far.push(peer.forward(format!("room/{i}"), route(&[9, 3, 4], 4)).unwrap());
			}
			let mut announced = producer.consume().routes();
			for _ in 0..k {
				assert!(matches!(
					announced.try_next(),
					Some(RouteEvent {
						kind: RouteKind::Start,
						..
					})
				));
			}
			let before = announced.state.read().steps;

			for route in near {
				drop(route);
				assert!(announced.try_next().is_none(), "the update is held");
			}
			for (i, route) in far.into_iter().enumerate().rev() {
				drop(route);
				match announced.try_next() {
					Some(RouteEvent {
						kind: RouteKind::Offline,
						announce: end,
					}) => assert_eq!(end.prefix, Path::new(&format!("room/{i}")).unwrap()),
					other => panic!("should go offline: got {other:?}"),
				}
			}

			let steps = announced.state.read().steps - before;
			let bound = 4 * k * (k.ilog2() as usize + 1);
			assert!(steps <= bound, "{k} broadcasts took {steps} steps, over {bound}");
		}
	}

	/// A new route and a removal are delivered at once. A change of the best route
	/// within one instance waits out the update hold, and a newer change during the
	/// wait replaces it.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_changed_route_waits_out_the_hold() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let mut announced = producer.consume().routes();
		// Holds count on the driver's clock, which starts at its first poll.
		kio::clock::sleep(Duration::from_millis(1)).await;

		// One instance, so the winner moving between routes is an update, not a restart.
		let route = |chain: &[u64], via| {
			RouteInfo::new()
				.with_instance(instance(), 0)
				.with_hops(hops(chain))
				.with_via(origin(via))
		};
		let near = peer.forward("room", route(&[9, 2], 2)).unwrap();
		announced.expect_next_active("room").await;

		// The best route changes twice; the consumer sees only the last, once.
		let far = peer.forward("room", route(&[9, 3, 4], 4)).unwrap();
		let farther = peer.forward("room", route(&[9, 5, 6, 7], 7)).unwrap();
		let start = kio::clock::now();
		drop(near);
		drop(far);
		match announced.next().await.unwrap() {
			RouteEvent {
				kind: RouteKind::Update,
				announce: update,
			} => assert_eq!(update.route.hops, hops(&[9, 5, 6, 7])),
			other => panic!("should be an update: got {other:?}"),
		}
		assert_eq!(kio::clock::now() - start, DEFAULT_UPDATE_HOLD);
		announced.assert_next_wait();

		// The last route goes: retracted at once.
		drop(farther);
		announced.expect_next_offline("room").await;
	}

	/// After a retraction, a worse path to the same publisher waits out the
	/// quarantine. An anonymous route names no publisher, so one after it is not held.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_retraction_quarantines_a_named_publisher() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let mut announced = producer.consume().routes();
		kio::clock::sleep(Duration::from_millis(1)).await;

		// Every path is to one publisher, so to one instance.
		let route = |chain: &[u64], via| {
			RouteInfo::new()
				.with_instance(instance(), 0)
				.with_hops(hops(chain))
				.with_via(origin(via))
		};

		let near = peer.forward("named", route(&[9, 2], 2)).unwrap();
		announced.expect_next_active("named").await;
		drop(near);
		announced.expect_next_offline("named").await;
		let start = kio::clock::now();
		let _stale = peer.forward("named", route(&[9, 3, 4], 4)).unwrap();
		match announced.next().await.unwrap() {
			RouteEvent {
				kind: RouteKind::Start,
				announce,
			} => assert_eq!(announce.route.hops, hops(&[9, 3, 4])),
			other => panic!("should be a start: got {other:?}"),
		}
		assert!(kio::clock::now() - start >= RETRACT_QUARANTINE);

		let near = peer.forward("anonymous", route(&[0, 2], 2)).unwrap();
		announced.expect_next_active("anonymous").await;
		drop(near);
		announced.expect_next_offline("anonymous").await;
		let _other = peer.forward("anonymous", route(&[0, 3, 4], 4)).unwrap();
		announced.expect_next_active("anonymous").await;

		// Retract and announce cycles at one clock reading queue the prefix once,
		// whether or not the route names its publisher.
		let queued = announced.anchoring.len();
		for chain in [[9, 2], [0, 2], [9, 2], [0, 2]] {
			let churn = peer.forward("churn", route(&chain, 2)).unwrap();
			announced.expect_next_active("churn").await;
			drop(churn);
			announced.expect_next_offline("churn").await;
		}
		assert_eq!(announced.anchoring.len(), queued + 1);

		// A quarantine is forgotten once it ends: the cursor wakes to anchor it,
		// then again to drop it.
		let gone = peer.forward("gone", route(&[9, 2], 2)).unwrap();
		announced.expect_next_active("gone").await;
		drop(gone);
		announced.expect_next_offline("gone").await;
		assert!(!announced.retracted.is_empty());
		for _ in 0..2 {
			announced.assert_next_wait();
			kio::clock::sleep(RETRACT_QUARANTINE).await;
		}
		announced.assert_next_wait();
		assert!(announced.retracted.is_empty(), "the ended quarantine is still held");
	}

	/// A peer withdrawing a prefix hides the routes there through it, so the next
	/// best is never a path derived from the one withdrawn. Announcing again
	/// revives them, and nothing is remembered once no route passes through it.
	#[kio::test]
	async fn withdrawn_peer_hides_routes_through_it() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let mut announced = producer.consume().routes();

		// Publisher 9 ingests at relay 2; relay 3 relays 2's route.
		let direct = || RouteInfo::new().with_hops(hops(&[9, 2])).with_via(origin(2));
		let first = peer.forward("room", direct()).unwrap();
		let relayed = peer
			.forward("room", RouteInfo::new().with_hops(hops(&[9, 2, 3])).with_via(origin(3)))
			.unwrap();
		announced.expect_next_active("room").await;
		announced.assert_next_wait();

		// Relay 2 withdraws: the relayed copy goes with it rather than taking over.
		first.withdrawn();
		announced.expect_next_ended("room").await;
		announced.assert_next_wait();

		// Relay 2 announces again, and the relayed copy is live with it.
		let second = peer.forward("room", direct()).unwrap();
		announced.expect_next_active("room").await;
		assert!(producer.shared.lock().withdrawn.is_empty());

		// Nothing passes through relay 2 once both routes are gone.
		second.withdrawn();
		drop(relayed);
		announced.expect_next_ended("room").await;
		assert!(producer.shared.lock().withdrawn.is_empty());
	}

	/// Overlapping sessions carrying one instance are independent claims, regardless of
	/// announcement order, and falling back from one to the other is the same stream.
	#[kio::test]
	async fn old_session_withdrawal_keeps_newer_route() {
		for old_first in [false, true] {
			let producer = origin(1).produce();
			let peer = producer.clone().with_link(Link::Peer);
			let mut announced = producer.consume().routes();
			let route = RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[9, 2]));
			let first = peer.forward("room", route.clone()).unwrap();
			let second = peer.forward("room", route.clone()).unwrap();
			announced.expect_next_active("room").await;
			announced.assert_next_wait();
			let remaining = if old_first {
				first.withdrawn();
				second
			} else {
				second.withdrawn();
				first
			};
			announced.assert_next_wait();
			assert!(producer.shared.lock().withdrawn.is_empty());
			remaining.withdrawn();
			announced.expect_next_ended("room").await;
			assert!(producer.shared.lock().withdrawn.is_empty());
		}
	}

	#[kio::test]
	async fn withdrawn_route_no_longer_covers_requests() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let direct = peer.forward("room", RouteInfo::new().with_hops(hops(&[9, 2]))).unwrap();
		let _relayed = peer
			.forward("room", RouteInfo::new().with_hops(hops(&[9, 2, 3])))
			.unwrap();
		let path = Path::from_static("room/video");
		let id = producer
			.shared
			.read()
			.routes
			.covering(&path)
			.find(|entry| entry.hops.iter().last() == Some(&origin(3)))
			.unwrap()
			.id;
		assert!(producer.shared.read().routes.covers(&path, id));
		direct.withdrawn();
		assert!(!producer.shared.read().routes.covers(&path, id));
	}

	/// A change of source alone is delivered: the same chain and cost arriving
	/// from a peer instead of a client is a different fact for the consumer. Under one
	/// instance it is an update.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn source_change_is_an_update() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let mut announced = producer.consume().routes();

		let route = RouteInfo::new()
			.with_instance(instance(), 0)
			.with_hops(hops(&[7]))
			.with_via(origin(7));
		let _forwarded = peer.forward("room", route.clone()).unwrap();
		assert_eq!(announced.expect_next_active("room").await.source(), Source::Peer(7));

		// The newest identical route wins, so the local twin takes over.
		let local = producer.forward("room", route).unwrap();
		match announced.next().await.expect("no next") {
			RouteEvent {
				kind: RouteKind::Update,
				announce,
			} => assert_eq!(announce.route.source(), Source::Local),
			other => panic!("should be an update: got {other:?}"),
		}

		drop(local);
		assert_eq!(announced.expect_next_active("room").await.source(), Source::Peer(7));
	}

	#[kio::test]
	async fn local_view_hides_peer_routes() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let mut local = producer.consume().local().routes();

		let _forwarded = peer
			.forward("remote", RouteInfo::new().with_hops(hops(&[7])).with_via(origin(7)))
			.unwrap();
		local.assert_next_wait();

		// A path both ingested here and forwarded by a peer shows the local route,
		// and retracts from the local view when the local route goes, even though
		// the peer's still covers it.
		let _shadow = peer
			.forward("both", RouteInfo::new().with_hops(hops(&[7])).with_via(origin(7)))
			.unwrap();
		let ingest = producer
			.forward(
				"both",
				RouteInfo::new().with_hops(hops(&[5])).with_via(origin(5)).with_cost(9),
			)
			.unwrap();
		assert_eq!(local.expect_next_active("both").await.source(), Source::Local);
		drop(ingest);
		local.expect_next_offline("both").await;

		// Resolution agrees with the cursor: a peer-only path is unroutable here,
		// while the full view queues the request on the peer's route.
		let err = producer
			.consume()
			.local()
			.request("remote/alice", None)
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
		assert!(
			producer
				.consume()
				.request("remote/alice", None)
				.now_or_never()
				.is_none()
		);
	}

	/// An upstream link is offered what entered here and what other peers
	/// forwarded, but never a route learned on another upstream link, and the
	/// best route it sees skips a better upstream one.
	#[kio::test]
	async fn upstream_view_hides_upstream_routes() {
		let producer = origin(1).produce();
		let peer = producer.clone().with_link(Link::Peer);
		let core = producer.clone().with_link(Link::Upstream);
		let mut toward_core = core.consume().routes();
		let mut everyone = producer.consume().routes();

		let _core = core
			.forward("core", RouteInfo::new().with_hops(hops(&[7])).with_via(origin(7)))
			.unwrap();
		assert_eq!(everyone.expect_next_active("core").await.source(), Source::Peer(7));
		toward_core.assert_next_wait();

		let _mesh = peer
			.forward("mesh", RouteInfo::new().with_hops(hops(&[8])).with_via(origin(8)))
			.unwrap();
		assert_eq!(toward_core.expect_next_active("mesh").await.source(), Source::Peer(8));

		// A path both an upstream and a mesh peer reach: the upstream route is
		// cheaper, yet the upstream view selects the mesh peer's.
		let _cheap = core
			.forward("both", RouteInfo::new().with_hops(hops(&[7])).with_via(origin(7)))
			.unwrap();
		let mesh = peer
			.forward(
				"both",
				RouteInfo::new().with_hops(hops(&[8])).with_via(origin(8)).with_cost(9),
			)
			.unwrap();
		assert_eq!(toward_core.expect_next_active("both").await.source(), Source::Peer(8));
		drop(mesh);
		toward_core.expect_next_offline("both").await;

		// Resolution agrees with the cursor: an upstream-only path is unroutable
		// toward an upstream, while the full view queues it on the core's route.
		let err = core
			.consume()
			.request("core/alice", None)
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));
		assert!(producer.consume().request("core/alice", None).now_or_never().is_none());
	}

	/// A handler that rejects a path with `Unroutable` while its route stands
	/// gives the requester that answer; the front must not re-ask the same route
	/// forever, which would spin the origin driver.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn handler_rejection_is_final() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.forward("room", RouteInfo::new()).unwrap();

		let pending = consumer.request("room/alice", None);
		let request = queued(&server).await;
		request.reject(Error::Unroutable);
		let err = kio::clock::timeout(Duration::from_secs(5), pending)
			.await
			.expect("the front must give up, not spin")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		// The route still stands and serves the next path.
		let pending = consumer.request("room/bob", None);
		let request = queued(&server).await;
		assert_eq!(request.path().as_str(), "room/bob");
		let served = broadcast::Info::new().produce();
		request.serve(&served);
		pending.await.expect("resolves");
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn claim_answer_is_hidden_until_published_under_the_claims_instance() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		let mut pending = Box::pin(consumer.request("room/alice", None));
		assert!((&mut pending).now_or_never().is_none());

		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();
		let _track = prepared
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		assert!((&mut pending).now_or_never().is_none());

		prepared.announce().unwrap();
		let resolved = pending.await.unwrap();
		assert_eq!(resolved.info().instance, prepared.route().unwrap().instance);
		assert_eq!(
			resolved.info().instance,
			claim.route().unwrap().instance,
			"the job continues the claim's stream at its path"
		);
		assert!(resolved.track("video").is_ok());
	}

	/// A hidden job's route is listed by no cursor until it announces, under its instance.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_hidden_job_route_is_listed_only_once_announced() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		announced.assert_next_active("room");
		let pending = producer.consume().request("room/alice", None);
		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();
		assert!(prepared.route().is_none());
		announced.assert_next_wait();

		prepared.announce().unwrap();
		let instance = prepared.route().and_then(|route| route.instance);
		assert_eq!(instance, claim.route().unwrap().instance, "the claim's instance");
		match announced.expect_next().await {
			RouteEvent {
				kind: RouteKind::Start,
				announce,
			} => {
				assert_eq!(announce.prefix.as_str(), "room/alice");
				assert_eq!(announce.route.instance, instance);
			}
			other => panic!("should start beneath the claim: got {other:?}"),
		}
		assert_eq!(pending.await.unwrap().info().instance, instance);
	}

	/// Route clones share announcement control, while a fill handle on its broadcast
	/// keeps the broadcast alive but not the route.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn route_clones_share_control_and_fill_handles_do_not_retain_it() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut events = consumer.routes();
		let publication = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let fill = publication.broadcast().clone();
		let control = publication.clone();
		publication.announce().unwrap();
		assert_eq!(events.expect_next_active("room/alice").await.instance, Some(instance()));
		control.withdraw();
		events.expect_next_ended("room/alice").await;
		assert!(publication.route().is_none());
		publication.announce().unwrap();
		assert_eq!(events.expect_next_active("room/alice").await.instance, Some(instance()));
		drop(control);
		events.assert_next_wait();
		let held = consumer.request("room/alice", None).await.unwrap();
		drop(publication);
		events.expect_next_ended("room/alice").await;
		assert!(matches!(
			consumer.request("room/alice", None).await,
			Err(Error::Unroutable)
		));
		assert!(!held.is_closed(), "the fill handle keeps the broadcast alive");
		drop(fill);
	}

	/// A withdrawn stale publication cannot remove a replacement instance's route.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn stale_publication_control_cannot_withdraw_a_replacement() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let old = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let replacement_instance: crate::Instance = "01900000-0000-7000-8000-000000000002".parse().unwrap();
		let replacement = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(replacement_instance.clone(), 0),
			)
			.unwrap();
		old.withdraw();
		old.broadcast().close();
		assert_eq!(
			replacement.route().unwrap().instance,
			Some(replacement_instance.clone())
		);
		let resolved = consumer
			.request("room/alice", replacement_instance.clone())
			.await
			.unwrap();
		assert_eq!(resolved.info().instance.as_ref(), Some(&replacement_instance));
	}

	/// A refused answer keeps its announcer withheld, so a clone cannot announce it later.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_refused_prepared_answer_cannot_announce_later() {
		let producer = origin(1).produce();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		let _pending = producer.consume().request("room/alice", None);
		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();
		let clone = prepared.clone();

		drop(claim);
		assert!(matches!(prepared.announce(), Err(Error::Unroutable)));
		assert!(matches!(clone.announce(), Err(Error::Closed)));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn dropping_a_prepared_claim_answer_refuses_the_request() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		let pending = consumer.request("room/alice", None);
		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();
		drop(prepared);
		assert!(matches!(pending.await, Err(Error::Unroutable)));
	}

	/// An answer prepared for a claim that went away is refused, never announced.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_prepared_answer_for_an_abandoned_claim_is_never_announced() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		announced.assert_next_active("room");
		let _pending = producer.consume().request("room/alice", None);
		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();

		drop(claim);
		announced.assert_next_ended("room");
		assert!(matches!(prepared.announce(), Err(Error::Unroutable)));
		announced.assert_next_wait();
	}

	/// Withdrawing a prefix route releases its pending requests, so a hidden job route
	/// prepared for one is refused rather than announced, and the requester is refused.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_job_route_for_a_withdrawn_prefix_is_never_announced() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		announced.assert_next_active("room");
		let pending = producer.consume().request("room/alice", None);
		let job = queued(&claim).await.route(RouteOptions::hidden()).unwrap();

		claim.withdraw();
		announced.assert_next_ended("room");
		assert!(matches!(job.announce(), Err(Error::Unroutable)));
		assert!(!job.is_announced());
		assert!(matches!(pending.await, Err(Error::Unroutable)));
		announced.assert_next_wait();

		// The prefix route itself comes back and answers new requests.
		claim.announce().unwrap();
		announced.assert_next_active("room");
		let _again = producer.consume().request("room/bob", None);
		assert_eq!(queued(&claim).await.path().as_str(), "room/bob");
	}

	/// An answer prepared before the claim changed instance is refused, never announced.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_prepared_answer_for_a_released_request_is_never_announced() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		announced.assert_next_active("room");
		let _pending = producer.consume().request("room/alice", None);
		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();

		claim
			.update(RouteInfo::new().with_instance(crate::Instance::random(), 0))
			.unwrap();
		announced.assert_next_restarted("room");
		assert!(matches!(prepared.announce(), Err(Error::Unroutable)));
		announced.assert_next_wait();
	}

	/// A published job must stop answering through its claim when its exact route leaves,
	/// even while its fill handle and readers keep the broadcast alive.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_withdrawn_publication_is_not_a_cached_claim_answer() {
		let producer = origin(1).produce();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		let path = Path::from_static("room/alice");
		let pending = producer.consume().request(path.clone(), None);
		let publication = queued(&claim).await.route(RouteOptions::default()).unwrap();
		let held = pending.await.unwrap();
		publication.withdraw();
		assert!(
			claim.server().lock().served.get(&path).is_none(),
			"withdrawn job still cached"
		);
		let next = producer.consume().request(path, None);
		let request = queued(&claim).await;
		request.reject(Error::App(42));
		assert!(next.await.is_err());
		drop(held);
	}

	/// A job withdrawn while it carries the claim's instance ends that stream at its path, so
	/// the claim moves to a fresh instance with it: the next job, at that path or another,
	/// starts a new stream rather than splicing into the old one.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_withdrawn_job_moves_the_claim_to_a_fresh_instance() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		announced.assert_next_active("room");
		let first = claim.instance();

		let answer = async |path: &str| {
			let pending = consumer.request(path, None);
			let published = queued(&claim).await.route(RouteOptions::default()).unwrap();
			pending.await.unwrap();
			published
		};

		let alice = answer("room/alice").await;
		assert_eq!(alice.instance(), first, "the job takes the claim's instance");
		announced.assert_next_active("room/alice");

		let bob = answer("room/bob").await;
		assert_eq!(bob.instance(), first, "jobs at sibling paths share it");
		announced.assert_next_active("room/bob");

		alice.withdraw();
		let second = claim.instance();
		assert_ne!(second, first, "the claim moved on");
		match announced.expect_next().await {
			RouteEvent {
				kind: RouteKind::End,
				announce,
			} => assert_eq!(announce.prefix.as_str(), "room/alice"),
			other => panic!("should end the job: got {other:?}"),
		}
		match announced.expect_next().await {
			RouteEvent {
				kind: RouteKind::Restart,
				announce,
			} => {
				assert_eq!(announce.prefix.as_str(), "room");
				assert_eq!(announce.route.instance, Some(second.clone()));
			}
			other => panic!("should restart the claim: got {other:?}"),
		}
		// The sibling keeps the stream it holds.
		assert_eq!(bob.instance(), first);

		let carol = answer("room/carol").await;
		assert_eq!(carol.instance(), second, "a later job takes the claim's new instance");
		// Bob ending now spends nothing the claim still lends.
		drop(bob);
		assert_eq!(claim.instance(), second);
		drop(carol);
		assert_ne!(claim.instance(), second);
	}

	/// A job's route takes the instance its claim lends when it announces, not when it was
	/// created: a hidden job that never read its instance outlives a sibling's end and joins
	/// the claim's next stream.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_hidden_job_takes_the_claims_instance_when_it_announces() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();

		let alice = consumer.request("room/alice", None);
		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();
		let bob = consumer.request("room/bob", None);
		let sibling = queued(&claim).await.route(RouteOptions::default()).unwrap();
		bob.await.unwrap();
		let first = claim.instance();
		assert_eq!(sibling.instance(), first);

		drop(sibling);
		let second = claim.instance();
		assert_ne!(second, first);

		prepared.announce().unwrap();
		assert_eq!(prepared.instance(), second);
		assert_eq!(alice.await.unwrap().info().instance, Some(second));
	}

	/// A hidden job that read its instance, as a signer does, announces under it or not at
	/// all: once a sibling's end moves the claim past it, the announce is refused and its
	/// request goes back to the worker, which prepares the job under the new instance. Every
	/// requester waits on, including one that joined after the claim moved.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_job_that_read_its_instance_is_refused_once_the_claim_moves() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();

		let alice = consumer.request("room/alice", None);
		let prepared = queued(&claim).await.route(RouteOptions::hidden()).unwrap();
		let bob = consumer.request("room/bob", None);
		let sibling = queued(&claim).await.route(RouteOptions::default()).unwrap();
		bob.await.unwrap();
		let first = prepared.instance();
		assert_eq!(first, claim.instance());

		drop(sibling);
		let second = claim.instance();
		assert_ne!(second, first);
		assert_eq!(prepared.instance(), first, "a read instance moved");
		let late = consumer.request("room/alice", None);
		assert!(matches!(prepared.announce(), Err(Error::Unroutable)));

		let again = queued(&claim).await.route(RouteOptions::hidden()).unwrap();
		assert_eq!(again.instance(), second);
		again.announce().unwrap();
		assert_eq!(alice.await.unwrap().info().instance, Some(second.clone()));
		assert_eq!(late.await.unwrap().info().instance, Some(second));
	}

	/// A request pinned to the claim's instance resolves through its job's route even when a
	/// sibling job ending moved the claim on before the request looked again.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_pinned_request_reaches_its_job_after_a_sibling_moves_the_claim() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		let lent = claim.instance();

		let reader = consumer.request("room/alice", None);
		let alice = queued(&claim).await.route(RouteOptions::default()).unwrap();
		let held = reader.await.unwrap();

		let pinned = consumer.request("room/bob", lent.clone());
		let bob = queued(&claim).await.route(RouteOptions::default()).unwrap();
		drop(alice);
		assert_ne!(claim.instance(), lent);
		assert_eq!(bob.instance(), lent);
		let resolved = pinned.await.expect("the job still serves the pinned instance");
		assert_eq!(resolved.info().instance, Some(lent));
		drop(held);
	}

	/// Once a job ends, a request pinned to its instance is refused without asking the
	/// claim, which serves another stream now, while an unpinned one starts the next job.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_pinned_to_an_ended_job_is_refused() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();

		let pending = consumer.request("room/alice", None);
		let job = queued(&claim).await.route(RouteOptions::default()).unwrap();
		let held = pending.await.unwrap();
		let ended = job.instance();
		job.broadcast().close();
		drop(job);

		let pinned = consumer.request("room/alice", ended.clone()).await;
		assert!(matches!(pinned, Err(Error::Unroutable)));
		assert!(
			claim.poll_requested(&kio::Waiter::noop()).is_pending(),
			"the claim was asked"
		);

		let pending = consumer.request("room/alice", None);
		let restarted = queued(&claim).await.route(RouteOptions::default()).unwrap();
		assert_ne!(restarted.instance(), ended);
		let fresh = pending.await.unwrap();
		assert!(!fresh.is_clone(&held), "joined the ended job");
	}

	/// A claim and its job end independently: a job outliving its claim keeps its stream,
	/// and a claim outliving its job moves on.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_claim_and_its_job_end_independently() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		let pending = consumer.request("room/alice", None);
		let job = queued(&claim).await.route(RouteOptions::default()).unwrap();
		let held = pending.await.unwrap();
		let lent = claim.instance();

		drop(claim);
		assert_eq!(job.instance(), lent);
		let again = consumer.request("room/alice", lent.clone()).await.unwrap();
		assert!(again.is_clone(&held), "the job's stream ended with its claim");
		drop(job);
		assert!(consumer.request("room/alice", lent).await.is_err());
	}

	/// A claim naming a stable instance lends nothing: its answers mint their own, and their
	/// end leaves the claim's instance as it is. A job naming its instance keeps it too.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn named_instances_are_neither_lent_nor_moved() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let stable = instance();
		let claim = producer
			.claim("room", RouteOptions::default().with_instance(stable.clone(), 0))
			.unwrap();

		let pending = consumer.request("room/alice", None);
		let minted = queued(&claim).await.route(RouteOptions::default()).unwrap();
		pending.await.unwrap();
		assert_ne!(minted.instance(), stable, "a stable claim lent its instance");
		drop(minted);
		assert_eq!(claim.instance(), stable);

		let lending = producer.claim("lent", RouteOptions::default()).unwrap();
		let lent = lending.instance();
		let named = instance();
		let pending = consumer.request("lent/bob", None);
		let job = queued(&lending)
			.await
			.route(RouteOptions::default().with_instance(named.clone(), 0))
			.unwrap();
		pending.await.unwrap();
		assert_eq!(job.instance(), named);
		drop(job);
		assert_eq!(lending.instance(), lent, "a named job's end moved the claim");
	}

	/// A request pinned to an instance resolves only through the answer published under
	/// it, never by asking the claim, which serves another instance.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_pinned_request_resolves_only_through_the_published_instance() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let claim = producer.claim("room", RouteOptions::default()).unwrap();
		let unknown = consumer.request("room/alice", crate::Instance::random());
		assert!(matches!(unknown.await, Err(Error::Unroutable)));

		let pending = consumer.request("room/alice", None);
		let published = queued(&claim).await.route(RouteOptions::default()).unwrap();
		let instance = published.route().unwrap().instance.unwrap();
		pending.await.expect("resolves");

		let pinned = consumer.request("room/alice", instance.clone()).await;
		assert_eq!(pinned.expect("resolves").info().instance.as_ref(), Some(&instance));
		let stale = consumer.request("room/alice", crate::Instance::random());
		assert!(matches!(stale.await, Err(Error::Unroutable)));
	}

	/// A refusal from the longest prefix is the answer: neither a costlier advertiser of
	/// that prefix nor a catch-all is asked instead.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn refusal_never_falls_through() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let catch_all = producer.forward("", RouteInfo::new()).unwrap();
		let winner = producer.forward("room", RouteInfo::new()).unwrap();
		let sibling = producer.forward("room", RouteInfo::new().with_cost(2)).unwrap();

		let mut pending = Box::pin(consumer.request("room/alice", None));
		assert!((&mut pending).now_or_never().is_none());
		queued(&winner).await.reject(Error::NotFound);
		let mut result = None;
		settle(|| {
			result = (&mut pending).now_or_never();
			result.is_some()
		})
		.await;
		let err = result.unwrap().err().unwrap();
		assert!(matches!(err, Error::NotFound), "unexpected end: {err}");

		for other in [&sibling, &catch_all] {
			assert!(other.poll_requested(&kio::Waiter::noop()).is_pending());
		}
	}

	/// A refusal from a route a local broadcast superseded while it was pending is moot:
	/// the request resolves to the local broadcast instead of ending.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn superseded_refusal_does_not_end_the_request() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let stale = producer.forward("room", RouteInfo::new()).unwrap();

		let mut pending = Box::pin(consumer.request("room/alice", None));
		assert!((&mut pending).now_or_never().is_none());
		let request = queued(&stale).await;

		// The local broadcast wins before the front sees the rejection.
		let local = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		local.announce().unwrap();
		request.reject(Error::NotFound);

		let mut result = None;
		settle(|| {
			result = (&mut pending).now_or_never();
			result.is_some()
		})
		.await;
		assert!(result.unwrap().is_ok(), "the superseded refusal ended the request");
	}

	/// The same holds for a more specific remote route: the beaten route's refusal is
	/// moot, and the new winner is asked instead.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn refusal_from_a_beaten_route_asks_the_new_winner() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let stale = producer.forward("room", RouteInfo::new()).unwrap();

		let mut pending = Box::pin(consumer.request("room/alice", None));
		assert!((&mut pending).now_or_never().is_none());
		let request = queued(&stale).await;

		let winner = producer.forward("room/alice", RouteInfo::new()).unwrap();
		request.reject(Error::NotFound);
		let source = broadcast::Info::new().produce();
		queued(&winner).await.serve(&source);

		let mut result = None;
		settle(|| {
			result = (&mut pending).now_or_never();
			result.is_some()
		})
		.await;
		assert!(result.unwrap().is_ok(), "the beaten route's refusal ended the request");
	}

	/// Teardown rejects the parked request with the origin's own terminal cause.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn follow_reports_teardown_as_closed() {
		let (producer, driver) = Producer::new(Config::from(origin(1)));
		let consumer = producer.consume();
		let _server = producer.forward("room", RouteInfo::new()).unwrap();

		// Park on the covering route, past the loop's closed check.
		let mut following = consumer.follow("room/alice", None).unwrap();
		let mut resolving = Box::pin(following.available());
		assert!((&mut resolving).now_or_never().is_none());

		drop(driver);

		let err = kio::clock::timeout(Duration::from_secs(5), resolving)
			.await
			.expect("teardown resolves the wait")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Closed), "unexpected end: {err}");
	}

	/// A datagram track a published broadcast creates reaches a reader through the origin:
	/// the front looks it up on the source it serves from.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_front_serves_its_sources_datagram_tracks() {
		let producer = origin(1).produce();
		let publication = producer.route("room", RouteOptions::default()).unwrap();
		let mut datagrams = publication
			.broadcast()
			.create_datagrams("audio", crate::datagram::Info::default())
			.unwrap();

		let front = producer.consume().request("room", None).await.unwrap();
		let mut reader = front.datagrams("audio").unwrap();
		datagrams.append(crate::Timestamp::unscaled(0), "x").unwrap();
		let datagram = reader.recv().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(datagram.sequence, 0);
		assert!(matches!(front.datagrams("video"), Err(Error::NotFound)));
	}

	/// A track first subscribed after the front is already serving another still
	/// replays what its source holds, like the first track did.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn late_track_on_a_served_front_replays() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer.forward("room", RouteInfo::new()).unwrap();

		let source = broadcast::Info::new().produce();
		for name in ["a", "b"] {
			let track = source.create_track(name, crate::track::Info::timed()).unwrap();
			let mut group = track.append_next().unwrap();
			group.write_frame(group.key_time(), name.as_bytes()).unwrap();
			group.close().unwrap();
			// The producer stays alive: the track is open, like a live SI track.
			std::mem::forget(track);
		}

		let pending = consumer.request("room/alice", None);
		queued(&server).await.serve(&source);
		let resolved = pending.await.expect("resolves");

		let budget =
			track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(3600)));
		for name in ["a", "b"] {
			let mut subscription = resolved
				.track(name)
				.unwrap()
				.subscribe(budget.clone())
				.await
				.expect("subscribe");
			let mut group = kio::clock::timeout(Duration::from_secs(5), subscription.recv_group())
				.await
				.expect("the late track must replay, not park")
				.expect("recv group")
				.expect("track ended early");
			let frame = group.read_frame().await.expect("read frame").expect("frame");
			assert_eq!(&frame.payload[..], name.as_bytes());
		}
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn most_specific_prefix_shadows() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broad_server = producer.forward("", RouteInfo::new()).unwrap();
		// A narrow advertise-only claim: requests under it must NOT route to the
		// broad server; they fall through to the (absent) fallback handler.
		let _narrow = producer.announce(".dash", RouteInfo::new()).unwrap();

		let err = consumer
			.request(".dash/pid", None)
			.now_or_never()
			.expect("unroutable")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Unroutable));

		// Everything else still routes to the broad server.
		let _pending = consumer.request("room/alice", None);
		let request = queued(&broad_server).await;
		assert_eq!(request.path().as_str(), "room/alice");
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn root_dynamic_serves_any_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let mut announced = consumer.routes();
		let dynamic = producer.forward("", RouteInfo::new()).unwrap();
		// The root claim is advertised like any other prefix.
		announced.expect_next_active("").await;

		let pending = consumer.request("anything/at/all", None);
		let request = queued(&dynamic).await;
		assert_eq!(request.path().as_str(), "anything/at/all");

		let source = broadcast::Info::new().produce();
		request.serve(&source);
		let resolved = pending.await.expect("resolves");
		assert_eq!(resolved.info().path.as_str(), "anything/at/all");

		// Nothing serves an uncovered path once the handler is gone.
		drop(dynamic);
		announced.expect_next_offline("").await;
		let err = consumer
			.request("something/else", None)
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
	#[kio::test]
	#[cfg(feature = "async")]
	async fn out_of_scope_request_never_reaches_the_dynamic_handler() {
		let producer = origin(1).produce();
		let dynamic = producer.forward("", RouteInfo::new()).unwrap();
		let scoped = producer.consume().scope("", &scopes(&["tenant-a"])).unwrap();

		// `tenant-a-other` shares a character prefix but not a segment, so this
		// also pins that the check is segment-aware rather than textual.
		for path in ["tenant-b/live", "tenant-a-other/live"] {
			let refused = scoped
				.request(path, None)
				.now_or_never()
				.expect("an out-of-scope request must be refused synchronously, not queued");
			assert!(matches!(refused, Err(Error::Unauthorized)));
			assert!(
				dynamic.requested().now_or_never().is_none(),
				"the dynamic handler was asked to create a broadcast the requester may not read"
			);
		}
	}

	/// The route a follower's first event starts on.
	#[cfg(feature = "async")]
	fn started(event: Option<RouteEvent>) -> RouteInfo {
		match event.expect("routed") {
			RouteEvent {
				kind: RouteKind::Start,
				announce,
			} => announce.route,
			other => panic!("should start: got {other:?}"),
		}
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn routes_wait_for_coverage() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let mut routes = consumer.path_routes("room/alice").unwrap();
		let mut fut = routes.next().boxed();
		assert!((&mut fut).now_or_never().is_none());

		// A covering prefix resolves the wait.
		let _a = producer.announce("room", RouteInfo::new().with_cost(3)).unwrap();
		let route = started(fut.now_or_never().expect("covered"));
		assert_eq!(route.cost, Cost::new(3));

		// Already covered: resolves immediately.
		let mut routes = consumer.path_routes("room/alice/cam").unwrap();
		started(routes.next().now_or_never().expect("covered"));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn routes_ignore_deeper_routes() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// A deeper route does not cover the shorter path.
		let _deep = producer.announce("room/alice/cam", RouteInfo::new()).unwrap();
		let mut routes = consumer.path_routes("room/alice").unwrap();
		let mut fut = routes.next().boxed();
		assert!((&mut fut).now_or_never().is_none());

		let _exact = producer.announce("room/alice", RouteInfo::new()).unwrap();
		started(fut.now_or_never().expect("covered"));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn routes_accept_a_max_depth_path() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let path = (0..Path::MAX_PARTS)
			.map(|i| format!("s{i}"))
			.collect::<Vec<_>>()
			.join("/");
		assert_eq!(Path::new(&path).unwrap().parts().count(), Path::MAX_PARTS);

		assert!(consumer.allowed().matches(&path));

		let mut routes = consumer.path_routes(&path).unwrap();
		let mut fut = routes.next().boxed();
		assert!((&mut fut).now_or_never().is_none());

		// A covering root still resolves: the lookup must not require `path/**`.
		let _a = producer.announce("", RouteInfo::new()).unwrap();
		started(fut.now_or_never().expect("covered"));
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn teardown_ends_everything() {
		let (producer, driver) = Producer::new(Config::from(origin(1)));
		let consumer = producer.consume();
		let _announcement = producer.announce("room", RouteInfo::new()).unwrap();
		let mut announced = consumer.routes();
		announced.expect_next_active("room").await;

		let _server = producer.forward("served", RouteInfo::new()).unwrap();
		let pending = consumer.request("served/path", None);

		drop(driver);

		// The cursor observes the end (after draining pending updates).
		announced.expect_next_active("served").await;
		assert!(announced.next().now_or_never().expect("ended").is_none());

		// Pending requests reject; new work refuses.
		assert!(matches!(pending.now_or_never().expect("rejected"), Err(Error::Closed)));
		assert!(matches!(producer.announce("x", RouteInfo::new()), Err(Error::Closed)));
		assert!(matches!(
			producer.route("x", RouteOptions::hidden()),
			Err(Error::Closed)
		));
		let err = consumer
			.request("y", None)
			.now_or_never()
			.expect("closed")
			.err()
			.unwrap();
		assert!(matches!(err, Error::Closed));

		// A cursor born after the teardown is born ended.
		let mut late = consumer.routes();
		assert!(late.next().now_or_never().expect("ended").is_none());
	}

	/// A teardown with a snapshot in flight still owes what the cursor has not read.
	#[test]
	#[cfg(feature = "async")]
	fn teardown_owes_what_a_snapshot_held() {
		let (producer, driver) = Producer::new(Config::from(origin(1)));
		let snapshot = producer.snapshot("room");
		let mut announced = producer.consume().routes();
		let _alice = producer.announce("room/alice", RouteInfo::new()).unwrap();

		drop(driver);
		assert!(!announced.is_synced(), "the queue is still owed");
		announced.assert_next_active("room/alice");
		assert!(announced.is_synced());
		drop(snapshot);
	}

	#[test]
	fn internal_route_requests_are_closed_on_teardown() {
		let (producer, mut driver) = Producer::new(Config::from(origin(1)));
		let server = producer.forward("room", RouteInfo::new()).unwrap();
		let _pending = producer.consume().request("room/alice", None);
		driver.poll(Instant::now(), &kio::Waiter::noop()).unwrap();
		let Poll::Ready(Some(request)) = server.poll_requested(&kio::Waiter::noop()) else {
			panic!("front requested its route");
		};
		drop(driver);
		assert!(matches!(request.producer.read().resolved, Some(Err(Error::Closed))));
	}

	/// An inline native executor can re-enter both tables from the rejection's wake.
	#[test]
	fn refusing_route_requests_wakes_after_unlocking() {
		struct Reenter {
			table: SendCell<kio::Shared<OriginState>>,
			server: kio::Shared<ServeState>,
			woken: AtomicBool,
		}
		impl Wake for Reenter {
			fn wake(self: Arc<Self>) {
				let _table = self.table.lock();
				let _server = self.server.read();
				self.woken.store(true, Ordering::SeqCst);
			}
		}

		for teardown in [false, true] {
			let (producer, mut driver) = Producer::new(Config::from(origin(1)));
			let server = producer.forward("room", RouteInfo::new()).unwrap();
			let _pending = producer.consume().request("room/alice", None);
			driver.poll(Instant::ZERO, &kio::Waiter::noop()).unwrap();
			let channel = server
				.server()
				.read()
				.requests
				.get(&Path::from_static("room/alice"))
				.unwrap()
				.consume();
			// Park on both the request's channel and the server's queue.
			let reenter = || {
				Arc::new(Reenter {
					table: producer.shared.clone(),
					server: server.server().clone(),
					woken: AtomicBool::new(false),
				})
			};
			let (request, queue) = (reenter(), reenter());
			let waiter = kio::Waiter::new(std::task::Waker::from(request.clone()));
			assert!(channel.poll(&waiter, |_| Poll::<()>::Pending).is_pending());
			let waiter = kio::Waiter::new(std::task::Waker::from(queue.clone()));
			assert!(server.server().poll(&waiter, |_| Poll::Pending).is_pending());
			if teardown {
				drop(driver);
			} else {
				drop(server);
			}
			assert!(request.woken.load(Ordering::SeqCst));
			assert!(queue.woken.load(Ordering::SeqCst));
		}
	}

	/// One live subscription reading a track through a remote front, plus the
	/// bookkeeping to kill and replace its serving route.
	#[cfg(feature = "async")]
	struct ResumeRig {
		producer: Producer,
		resolved: broadcast::Consumer,
		subscription: track::Subscriber,
		/// Keeps the incumbent's track producing; dropping it would abort the
		/// track out from under the front mid-test.
		incumbent_track: track::Producer,
	}

	#[cfg(feature = "async")]
	impl ResumeRig {
		/// Announce a served route with `first` as its first hop, materialize
		/// "room/alice" through it with a one-group "before" track, and subscribe.
		#[cfg(feature = "async")]
		async fn new(first: &[u64]) -> (Self, Claim, broadcast::Producer) {
			let producer = origin(1).produce();
			let consumer = producer.consume();

			let server = producer
				.forward(
					"room",
					RouteInfo::new().with_instance(instance(), 0).with_hops(hops(first)),
				)
				.unwrap();

			let pending = consumer.request("room/alice", None);
			let request = queued(&server).await;
			let source = broadcast::Info::new().produce();
			let track = source.create_track("video", crate::track::Info::timed()).unwrap();
			let mut group = track.append_next().unwrap();
			group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
			group.close().unwrap();
			request.serve(&source);

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
		fn standby(&self, first: &[u64]) -> Claim {
			self.producer
				.forward(
					"room",
					RouteInfo::new().with_instance(instance(), 0).with_hops(hops(first)),
				)
				.unwrap()
		}
	}

	/// Accept the front's re-request on `server` with a source carrying the same
	/// content stream (the delivered group plus its successor) and prove the
	/// rig's subscription resumes onto it: the successor group is delivered on
	/// the same subscription, at the group boundary.
	#[cfg(feature = "async")]
	async fn assert_resumes(rig: &mut ResumeRig, server: &Claim) {
		let request = queued(server).await;
		let replacement = broadcast::Info::new().produce();
		let track = replacement.create_track("video", crate::track::Info::timed()).unwrap();
		// The same content: group 0 was already delivered through the old route,
		// so the track resumes at group 1.
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
		group.close().unwrap();
		request.serve(&replacement);

		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"resumed".as_ref()).unwrap();
		group.close().unwrap();

		let mut group = rig
			.subscription
			.recv_group()
			.await
			.expect("subscription survives the failover")
			.expect("track ended early");
		let frame = group.read_frame().await.expect("read frame").expect("frame");
		assert_eq!(&frame.payload[..], b"resumed");
	}

	/// Only an instance's highest epoch is eligible: a costlier route at a higher epoch
	/// takes the front over from the cheaper older one still standing, and the reader
	/// continues across the epoch change without a seam, since the instance's keys
	/// continue. A new request joins the same broadcast.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_higher_epoch_supersedes_a_cheaper_route_and_the_reader_continues() {
		let (mut rig, _incumbent, _source) = ResumeRig::new(&[10]).await;
		let newer = rig
			.producer
			.forward(
				"room",
				RouteInfo::new()
					.with_instance(instance(), 1)
					.with_hops(hops(&[11]))
					.with_cost(9),
			)
			.unwrap();
		assert_resumes(&mut rig, &newer).await;
		let joined = rig.producer.consume().request("room/alice", None).await.unwrap();
		assert!(
			joined.is_clone(&rig.resolved),
			"a newcomer joins the continued broadcast"
		);
	}

	/// A pool host that is full withdraws its route: a path it won moves to another host's
	/// instance, so its broadcast ends and `follow` yields the other host's. A path the
	/// other host won stays where it is.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_full_pool_host_withdraws_its_paths() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let host = |name: &str, hop| {
			producer
				.forward(
					"pool",
					RouteInfo::new()
						.with_instance(name.parse().unwrap(), 0)
						.with_hops(hops(&[hop])),
				)
				.unwrap()
		};
		// Pinned in `spread_hash_matches_js`: `a` wins `pool/job-0`.
		let a = host("a", 10);
		let b = host("b", 11);

		let mut follow = consumer.follow("pool/job-0", None).unwrap();
		let (first, ()) = futures::join!(follow.available(), async {
			queued(&a).await.serve(broadcast::Info::new().produce());
		});
		let first = first.unwrap().unwrap();
		assert_eq!(first.info().instance, Some("a".parse().unwrap()));

		drop(a);
		let (second, ()) = futures::join!(follow.available(), async {
			queued(&b).await.serve(broadcast::Info::new().produce());
		});
		let second = second.unwrap().unwrap();
		assert_eq!(second.info().instance, Some("b".parse().unwrap()));
		assert!(!second.is_clone(&first));
	}

	/// Letting go of the broadcast `follow` handed out retires it, which is no change to
	/// the path: the follower neither asks its host again nor hands the same instance out
	/// again. Its instance withdrawing later still hands out the next one.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_follower_dropping_its_broadcast_waits_for_another_instance() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let host = |name: &str, hop| {
			producer
				.forward(
					"pool",
					RouteInfo::new()
						.with_instance(name.parse().unwrap(), 0)
						.with_hops(hops(&[hop])),
				)
				.unwrap()
		};
		// Pinned in `spread_hash_matches_js`: `a` wins `pool/job-0`.
		let a = host("a", 10);
		let b = host("b", 11);

		let mut follow = consumer.follow("pool/job-0", None).unwrap();
		let (first, ()) = futures::join!(follow.available(), async {
			queued(&a).await.serve(broadcast::Info::new().produce());
		});
		let first = first.unwrap().unwrap();
		let weak = first.weak();
		drop(first);
		settle(|| weak.consume().poll_closed(&kio::Waiter::noop()).is_ready()).await;

		for _ in 0..3 {
			kio::clock::sleep(Duration::from_secs(20)).await;
			assert!(
				follow.poll_available(&kio::Waiter::noop()).is_pending(),
				"the same instance handed out again"
			);
			kio::clock::sim::yield_now().await;
			assert!(
				a.poll_requested(&kio::Waiter::noop()).is_pending(),
				"the host asked again"
			);
		}

		drop(a);
		let (second, ()) = futures::join!(follow.available(), async {
			queued(&b).await.serve(broadcast::Info::new().produce());
		});
		assert_eq!(second.unwrap().unwrap().info().instance, Some("b".parse().unwrap()));
	}

	/// Write one single-frame group and read it back through `subscription`.
	#[cfg(feature = "async")]
	async fn deliver(track: &track::Producer, subscription: &mut track::Subscriber, payload: &'static [u8]) {
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), payload).unwrap();
		group.close().unwrap();
		let mut group = next_group(subscription).await.unwrap().expect("track ended early");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], payload);
	}

	/// Another instance is another stream: a better route under one does not move the
	/// subscription, and when its own route goes it ends rather than resuming elsewhere.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn another_instance_never_resumes_the_subscription() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let first = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])).with_cost(5))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		let source = broadcast::Info::new().produce();
		let track = source.create_track("video", crate::track::Info::timed()).unwrap();
		queued(&first).await.serve(&source);
		let resolved = pending.await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&track, &mut subscription, b"first").await;

		let second = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[11])).with_cost(1))
			.unwrap();
		deliver(&track, &mut subscription, b"pinned").await;

		drop(first);
		drop(track);
		drop(source);
		assert!(
			next_group(&mut subscription).await.is_err(),
			"the track ends with its route"
		);
		settle(|| resolved.is_closed()).await;
		assert!(
			second.poll_requested(&kio::Waiter::noop()).is_pending(),
			"a route of another instance is never asked to resume"
		);
	}

	/// Another instance winning the path takes new requests, while the subscriptions on
	/// the old one stay with it until its route goes.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn another_instance_leaves_the_broadcast_in_flight() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let old = producer
			.route(
				"room/alice",
				RouteOptions::default()
					.with_instance(crate::Instance::random(), 0)
					.with_cost(9),
			)
			.unwrap();
		let old_track = old
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&old_track, &mut subscription, b"old").await;

		let new = producer
			.route(
				"room/alice",
				RouteOptions::default()
					.with_instance(crate::Instance::random(), 0)
					.with_cost(1),
			)
			.unwrap();
		let new_track = new
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		deliver(&old_track, &mut subscription, b"sticky").await;

		let replaced = consumer.request("room/alice", None).await.expect("resolves");
		assert!(!replaced.is_clone(&resolved));
		let mut fresh = replaced.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&new_track, &mut fresh, b"new").await;
		deliver(&old_track, &mut subscription, b"still").await;

		// The old publisher goes, and its subscription with it.
		drop(old_track);
		drop(old);
		assert!(
			!matches!(next_group(&mut subscription).await, Ok(Some(_))),
			"the old subscription outlived its route"
		);
		deliver(&new_track, &mut fresh, b"after").await;
	}

	/// A request in the same tick as another instance's announcement is never handed
	/// the old front: its watcher has not noticed the replacement yet, but the
	/// request mints a front on the new instance instead of joining the replaced one.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_racing_a_new_instance_skips_the_old_front() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let first = crate::Instance::random();
		let old = producer
			.route("room/alice", RouteOptions::default().with_instance(first.clone(), 0))
			.unwrap();
		let _old_track = old
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		assert_eq!(
			resolved.info().instance.as_ref(),
			Some(&first),
			"an unpinned request names what it resolved"
		);

		let instance = crate::Instance::random();
		let new = producer
			.route("room/alice", RouteOptions::default().with_instance(instance.clone(), 0))
			.unwrap();
		let new_track = new
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let named = consumer.request("room/alice", instance.clone());
		let bare = consumer.request("room/alice", None);

		let named = named.await.expect("the new instance resolves");
		let bare = bare.await.expect("the new instance resolves");
		assert!(
			!named.is_clone(&resolved),
			"a request naming the new instance got the old one"
		);
		assert!(!bare.is_clone(&resolved), "a bare request joined the replaced front");
		assert!(named.is_clone(&bare), "both share the new front");
		assert_eq!(
			bare.info().instance.as_ref(),
			Some(&instance),
			"an unpinned request names the new instance"
		);
		let mut subscription = named.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&new_track, &mut subscription, b"new").await;
	}

	/// Another instance winning while the front's first request is in flight beats that
	/// route: its late answer is not what requesters resolve to.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_answer_racing_a_new_instance_resolves_the_winner() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let old = producer
			.forward("room", RouteInfo::new().with_instance(crate::Instance::random(), 0))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		let request = queued(&old).await;

		// The answer and the new instance land in the same tick.
		let new = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(crate::Instance::random(), 0),
			)
			.unwrap();
		let new_track = new
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let stale = broadcast::Info::new().produce();
		let _stale_track = stale.create_track("video", crate::track::Info::timed()).unwrap();
		request.serve(&stale);

		let resolved = pending.await.expect("resolves the winner");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&new_track, &mut subscription, b"new").await;
	}

	/// A request held when its route changes instance carries over: the handler is asked
	/// again under the new instance rather than the requester failing. The old answer is
	/// neither served under the new instance nor cached for the next request, and a request
	/// pinned to the old instance is refused. `answer_first` lands the old answer just
	/// before the change instead of after, before the front takes it.
	#[cfg(feature = "async")]
	async fn held_request_across_an_instance_change(answer_first: bool) {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let old = crate::Instance::random();
		let dynamic = producer
			.forward("room", RouteInfo::new().with_instance(old.clone(), 0))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		let pinned = consumer.request("room/alice", old);
		let request = queued(&dynamic).await;

		let stale = broadcast::Info::new().produce();
		let _stale_track = stale.create_track("video", crate::track::Info::timed()).unwrap();
		let instance = crate::Instance::random();
		if answer_first {
			request.serve(&stale);
			dynamic
				.update(dynamic.route().unwrap().with_instance(instance.clone(), 0))
				.unwrap();
		} else {
			dynamic
				.update(dynamic.route().unwrap().with_instance(instance.clone(), 0))
				.unwrap();
			request.serve(&stale);
		}

		let request = queued(&dynamic).await;
		let fresh = broadcast::Info::new().produce();
		let fresh_track = fresh.create_track("video", crate::track::Info::timed()).unwrap();
		request.serve(&fresh);
		let resolved = pending.await.expect("the request carries over to the new instance");
		assert_eq!(resolved.info().instance.as_ref(), Some(&instance));
		assert!(
			matches!(pinned.await, Err(Error::Unroutable)),
			"a request pinned to the old instance resolved"
		);
		let cached = dynamic.server().lock().served.get(&Path::from_static("room/alice"));
		assert!(
			cached.is_some_and(|cached| cached.consume().is_clone(&fresh.consume())),
			"the old answer was cached"
		);
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&fresh_track, &mut subscription, b"new").await;
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_held_across_an_instance_change_is_asked_again() {
		held_request_across_an_instance_change(false).await;
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_answer_before_an_instance_change_is_asked_again() {
		held_request_across_an_instance_change(true).await;
	}

	/// A refusal that lands just before an instance change, before the front takes it,
	/// was the old instance's answer: the request is asked again, not ended.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_refusal_before_an_instance_change_is_asked_again() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let dynamic = producer
			.forward("room", RouteInfo::new().with_instance(crate::Instance::random(), 0))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		queued(&dynamic).await.reject(Error::NotFound);
		let instance = crate::Instance::random();
		dynamic
			.update(dynamic.route().unwrap().with_instance(instance.clone(), 0))
			.unwrap();

		let fresh = broadcast::Info::new().produce();
		let fresh_track = fresh.create_track("video", crate::track::Info::timed()).unwrap();
		queued(&dynamic).await.serve(&fresh);
		let resolved = pending.await.expect("the request carries over");
		assert_eq!(resolved.info().instance.as_ref(), Some(&instance));
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&fresh_track, &mut subscription, b"new").await;
	}

	/// A release is not undone by the instance coming back before the front looks: a
	/// request held across A to B to A is asked again, not refused.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_held_across_an_instance_round_trip_is_asked_again() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let old = crate::Instance::random();
		let dynamic = producer
			.forward("room", RouteInfo::new().with_instance(old.clone(), 0))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		let request = queued(&dynamic).await;

		dynamic
			.update(dynamic.route().unwrap().with_instance(crate::Instance::random(), 0))
			.unwrap();
		dynamic
			.update(dynamic.route().unwrap().with_instance(old.clone(), 0))
			.unwrap();
		let stale = broadcast::Info::new().produce();
		let _stale_track = stale.create_track("video", crate::track::Info::timed()).unwrap();
		request.serve(&stale);

		let fresh = broadcast::Info::new().produce();
		let fresh_track = fresh.create_track("video", crate::track::Info::timed()).unwrap();
		queued(&dynamic).await.serve(&fresh);
		let resolved = pending.await.expect("the request carries over");
		assert_eq!(resolved.info().instance.as_ref(), Some(&old));
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		deliver(&fresh_track, &mut subscription, b"new").await;
	}

	/// Announce cursors follow an instance's newest epoch over a cheaper older one, and
	/// deliver the change as an update: the same stream, continued.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn the_newest_epoch_is_announced_in_place() {
		let producer = origin(1).produce();
		let mut announced = producer.consume().routes();
		let instance = crate::Instance::random();
		let _old = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(instance.clone(), 1).with_cost(1),
			)
			.unwrap();
		assert_eq!(announced.expect_next_active("room/alice").await.cost, Cost::new(1));

		let new = producer
			.route(
				"room/alice",
				RouteOptions::default().with_instance(instance.clone(), 2).with_cost(9),
			)
			.unwrap();
		let newest = announced.expect_next_active("room/alice").await;
		assert_eq!(
			(newest.instance, newest.epoch, newest.cost),
			(Some(instance.clone()), 2, Cost::new(9))
		);

		// The older one still stands, so it comes back once the newer one goes.
		drop(new);
		let back = announced.expect_next_active("room/alice").await;
		assert_eq!((back.instance, back.epoch), (Some(instance), 1));
	}

	/// A peer naming one publisher instance is never handed another.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_for_another_instance_is_refused() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let _broadcast = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();

		let other: crate::Instance = "01900000-0000-7000-8000-000000000002".parse().unwrap();
		let refused = consumer.request("room/alice", other).await;
		assert!(matches!(refused, Err(Error::Unroutable)));
		let named = consumer
			.request("room/alice", instance())
			.await
			.expect("the named instance resolves");
		assert_eq!(named.info().instance.as_ref(), Some(&instance()));
	}

	/// The driver's completion contract: it resolves once every producer handle
	/// drops, however many read handles remain.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn driver_resolves_with_live_consumers() {
		let (producer, driver) = Producer::new(Config::from(origin(1)));
		let consumer = producer.consume();
		let run = crate::time::run(driver);
		drop(producer);
		kio::clock::timeout(Duration::from_secs(5), run)
			.await
			.expect("driver must finish once the producers are gone");
		drop(consumer);
	}

	/// A source claiming the same content cannot change immutable track metadata:
	/// the successor is refused instead of the subscriber's samples being read on
	/// a different grid, and the verdict outlives the aborted logical track.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn incompatible_successor_is_refused() {
		for replacement in [
			track::Info::default().with_timescale(crate::Timescale::MICRO),
			track::Info::timed().with_priority(7),
			track::Info::timed().with_max_age(Duration::from_secs(7)),
		] {
			let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
			let standby_server = rig.standby(&[10, 20]);
			drop(incumbent);
			drop(source);

			// The front re-requests through the standby, but its copy of the track is
			// on another grid. The incumbent's copy serves until it runs out, then the
			// refusal stands.
			let request = queued(&standby_server).await;
			let successor = broadcast::Info::new().produce();
			let track = successor.create_track("video", replacement).unwrap();
			let mut group = track.append_next().unwrap();
			group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
			group.close().unwrap();
			request.serve(&successor);
			rig.incumbent_track.clone().abort(Error::Dropped).unwrap();

			assert!(
				matches!(rig.subscription.recv_group().await, Err(Error::Unsupported)),
				"the subscription must abort rather than resume onto incompatible metadata"
			);

			// Reopening the aborted logical track must not forget the broadcast's metadata.
			let reopened = rig.resolved.track("video").unwrap();
			assert!(matches!(reopened.info().await, Err(Error::Unsupported)));
			assert!(matches!(reopened.subscribe(None).await, Err(Error::Unsupported)));
		}
	}

	/// Routes with one instance serve one broadcast: when the serving route dies, the
	/// subscription resumes through another with that instance, whatever its first hop,
	/// or with none at all.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn any_route_at_the_path_resumes_the_subscription() {
		let cases = [
			(&[10][..], &[10, 20][..]),
			(&[10][..], &[11][..]),
			(&[][..], &[][..]),
			(&[0][..], &[12][..]),
		];
		for (first, other) in cases {
			let (mut rig, incumbent, source) = ResumeRig::new(first).await;
			let standby = rig.standby(other);

			drop(incumbent);
			drop(source);
			rig.incumbent_track.clone().abort(Error::Dropped).unwrap();

			assert_resumes(&mut rig, &standby).await;
		}
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_replacement_query_carries_the_readers_resume_floor() {
		let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
		let mut group = rig
			.incumbent_track
			.create_group(crate::group::Info { start: 5 })
			.unwrap();
		group.write_frame(group.key_time(), b"five".as_ref()).unwrap();
		group.close().unwrap();
		assert_eq!(rig.subscription.recv_group().await.unwrap().unwrap().start, 5);
		let standby = rig.standby(&[10, 20]);
		drop(incumbent);
		drop(source);
		rig.incumbent_track.clone().abort(Error::Dropped).unwrap();
		let replacement = broadcast::Info::new().produce();
		let mut handler = replacement.requests();
		queued(&standby).await.serve(&replacement);
		let request = handler.requested_track().await.unwrap();
		assert_eq!(request.subscription().unwrap().start(), Some(5));
		let track = request.accept(crate::track::Info::timed());
		let mut preferences = rig.subscription.subscription();
		preferences.priority = 7;
		rig.subscription.update(preferences).unwrap();
		settle(|| track.subscription().is_some_and(|sub| sub.priority == 7)).await;
		assert_eq!(track.subscription().unwrap().start(), Some(5));
	}

	/// A route updated in place to a new first hop still serves the same broadcast:
	/// the subscription carries on through it, and a new request joins the front.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_first_hop_update_keeps_serving() {
		for first in [&[][..], &[0][..], &[10][..]] {
			let (mut rig, server, _source) = ResumeRig::new(first).await;
			let standby = rig.standby(&[10, 20]);
			server.update(RouteInfo::new().with_hops(hops(&[11]))).unwrap();

			let mut group = rig.incumbent_track.append_next().unwrap();
			group.write_frame(group.key_time(), b"after".as_ref()).unwrap();
			group.close().unwrap();
			let mut group = next_group(&mut rig.subscription)
				.await
				.expect("the subscription survives the update")
				.expect("track ended early");
			let frame = group.read_frame().await.expect("read frame").expect("frame");
			assert_eq!(&frame.payload[..], b"after", "first hop {first:?}");

			let resolved = rig.producer.consume().request("room/alice", None).await.unwrap();
			assert!(resolved.is_clone(&rig.resolved), "first hop {first:?} left the front");
			assert!(
				standby.poll_requested(&kio::Waiter::noop()).is_pending(),
				"first hop {first:?} moved off a route that still serves"
			);
		}
	}

	/// A session never subscribes to itself: when the route serving a peer dies, its
	/// front never resumes onto a route through that peer, even one from the same
	/// publisher. A reader that can see the route resumes there.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn resume_never_routes_through_the_requester() {
		for peer in [Some(origin(7)), None] {
			let producer = origin(1).produce();
			let consumer = match peer {
				Some(peer) => producer.consume().excluding(peer),
				None => producer.consume(),
			};
			let incumbent = producer
				.forward(
					"room",
					RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[10])),
				)
				.unwrap();
			// The same publisher, reached through the requesting peer.
			let echo = producer
				.forward(
					"room",
					RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[10, 7])),
				)
				.unwrap();

			let pending = consumer.request("room/alice", None);
			let request = queued(&incumbent).await;
			let source = broadcast::Info::new().produce();
			let track = source.create_track("video", crate::track::Info::timed()).unwrap();
			let mut group = track.append_next().unwrap();
			group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
			group.close().unwrap();
			request.serve(&source);

			let resolved = pending.await.expect("resolves through the incumbent");
			let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
			let mut group = next_group(&mut subscription).await.unwrap().expect("first group");
			assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");

			// The serving route dies, like a session.
			drop(incumbent);
			drop(source);
			track.abort(Error::Dropped).unwrap();

			match peer {
				Some(_) => {
					let err = next_group(&mut subscription).await.err().expect("subscription ends");
					assert!(matches!(err, Error::Dropped), "unexpected end: {err}");
					assert!(
						echo.poll_requested(&kio::Waiter::noop()).is_pending(),
						"the front asked the requester for its own copy"
					);
				}
				None => {
					let request = queued(&echo).await;
					let replacement = broadcast::Info::new().produce();
					let track = replacement.create_track("video", crate::track::Info::timed()).unwrap();
					let mut group = track.append_next().unwrap();
					group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
					group.close().unwrap();
					request.serve(&replacement);
					let mut group = track.append_next().unwrap();
					group.write_frame(group.key_time(), b"resumed".as_ref()).unwrap();
					group.close().unwrap();

					let mut group = next_group(&mut subscription).await.unwrap().expect("resumed group");
					assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"resumed");
				}
			}
		}
	}

	/// An anonymous publisher that dies without unannouncing is replaced by the
	/// next anonymous session at the same path: a subscriber on a third session
	/// gets the newcomer's media immediately, not a lingering dead front.
	///
	/// The front closes with its last source, so the newcomer attaches a fresh
	/// one and the subscriber resolves it without parking.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn anonymous_handoff_serves_the_newcomer_immediately() {
		let producer = origin(1).produce();

		// Session A: an assigned anonymous hop serving the path.
		let server_a = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();

		// Session C: a third anonymous session, excluding the hop the server
		// minted for it, the same split-horizon a live session applies.
		let consumer = producer.consume().excluding(origin(30));
		let pending = consumer.request("room/alice", None);
		let request = queued(&server_a).await;
		let source_a = broadcast::Info::new().produce();
		let track_a = source_a.create_track("video", crate::track::Info::timed()).unwrap();
		let mut group = track_a.append_next().unwrap();
		group.write_frame(group.key_time(), b"from-a".as_ref()).unwrap();
		group.close().unwrap();
		request.serve(&source_a);

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
				consumer.request("room/alice", None).now_or_never(),
				Some(Err(Error::Unroutable))
			)
		})
		.await;

		// Session B attaches at the same path. Its front is served immediately.
		let server_b = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[20])))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		let request = queued(&server_b).await;
		let source_b = broadcast::Info::new().produce();
		let track_b = source_b.create_track("video", crate::track::Info::timed()).unwrap();
		let mut group = track_b.append_next().unwrap();
		group.write_frame(group.key_time(), b"from-b".as_ref()).unwrap();
		group.close().unwrap();
		request.serve(&source_b);

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

	#[kio::test]
	#[cfg(feature = "async")]
	async fn reprice_is_invisible_to_the_subscription() {
		let (rig, incumbent, source) = ResumeRig::new(&[10]).await;

		// A metadata-only reprice of the only route: nothing re-requests and the
		// subscription keeps flowing from the same source.
		incumbent
			.update(RouteInfo::new().with_hops(hops(&[10])).with_cost(9))
			.unwrap();

		let track = source.create_track("audio", crate::track::Info::timed()).unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"steady".as_ref()).unwrap();
		group.close().unwrap();

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

	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_drain_migrates_before_the_session_dies() {
		let (mut rig, incumbent, source) = ResumeRig::new(&[10]).await;
		let standby_server = rig.standby(&[10, 20]);

		// The serving route drains: ranked last, at its real cost, while its session
		// keeps serving. The front migrates to the standby without waiting for
		// the death.
		incumbent
			.update(
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[10]))
					.with_draining(true),
			)
			.unwrap();

		assert_resumes(&mut rig, &standby_server).await;

		// The drained source outlived the migration.
		drop(incumbent);
		drop(source);
	}

	/// A second broadcast at the path under another instance is another instance: new
	/// requests resolve it on a fresh front, rather than join the first one's.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_newer_local_source_gets_a_fresh_front() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let first = producer.route("room/alice", RouteOptions::default()).unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");

		let second = producer.route("room/alice", RouteOptions::default()).unwrap();
		let again = consumer.request("room/alice", None).await.expect("resolves");
		assert!(!again.is_clone(&resolved));

		// Losing one source leaves the path served; losing both frees it.
		second.broadcast().close();
		settle(|| consumer.get_broadcast("room/alice").is_some()).await;
		first.broadcast().close();
		settle(|| consumer.get_broadcast("room/alice").is_none()).await;

		// The path is free again for a fresh broadcast.
		let _third = producer.route("room/alice", RouteOptions::default()).unwrap();
		assert!(consumer.get_broadcast("room/alice").is_some());
	}

	/// A front still resolving an instance that another has since beaten takes no new
	/// requester: it may already hold the beaten instance's source, and would hand it to a
	/// request that saw it lose, which no later announcement corrects.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_skips_a_front_resolving_a_beaten_instance() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();

		// The front selected the covering route and waits on its answer.
		let pending = consumer.request("room/alice", None);
		let _request = queued(&server).await;

		// The exact path beats the covering route before the front resolves.
		let _winner = producer.route("room/alice", RouteOptions::default()).unwrap();
		let again = consumer.request("room/alice", None).await.expect("resolves");
		let first = pending.await.expect("the first front moves to the winner");
		assert!(!again.is_clone(&first), "joined a front resolving the beaten route");
	}

	/// A front that moved to the new winner before resolving takes requesters for it, so
	/// they share it rather than each minting a front on the winner.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_request_joins_a_front_resolving_the_winner() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let covering = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let pending = consumer.request("room/alice", None);
		let _beaten = queued(&covering).await;

		// The exact path wins, and the front asks it in turn.
		let exact = producer
			.forward("room/alice", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let request = queued(&exact).await;
		let again = consumer.request("room/alice", None);

		let upstream = broadcast::Info::new().produce();
		request.serve(&upstream);
		let first = pending.await.expect("resolves");
		let again = again.await.expect("resolves");
		assert!(
			again.is_clone(&first),
			"a request skipped the front resolving its winner"
		);
	}

	/// The publisher finishes a track, then its broadcast. A subscription already in
	/// flight must conclude normally: the track's last group, then the end. moq-lite,
	/// ANNOUNCE_END: "Retraction does not disturb subscriptions already in flight,
	/// which conclude normally with SUBSCRIBE_FINISH."
	///
	/// The runtime is single-threaded and the publisher's whole ending has no await in
	/// it, so the outcome does not depend on timing.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_finished_broadcast_concludes_in_flight_subscriptions() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();

		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let mut subscription = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		// A textbook clean end, innermost first: the group, the track, the broadcast.
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"tail".as_ref()).unwrap();
		group.close().unwrap();
		let track = track.finish().unwrap();
		drop(track);
		broadcast.broadcast().close();

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
	/// SUBSCRIBE_FINISH."
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_retracted_route_concludes_in_flight_subscriptions() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();

		let pending = consumer.request("room/alice", None);
		let request = queued(&server).await;
		let source = broadcast::Info::new().produce();
		let track = source.create_track("video", crate::track::Info::timed()).unwrap();
		request.serve(&source);

		let resolved = pending.await.expect("resolves");
		let mut subscription = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");

		// ANNOUNCE_END overtakes the track's end: the route is retracted, and the front
		// has acted on it, before the track's last group and end arrive.
		source.close();
		drop(server);
		settle(|| resolved.is_closed()).await;
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"tail".as_ref()).unwrap();
		group.close().unwrap();
		let track = track.finish().unwrap();
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

	/// A standing route outlives the source it produced: the front ends instead of
	/// asking that route for the broadcast that just closed.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_closed_source_is_not_requested_again_from_its_standing_route() {
		let producer = origin(1).produce();
		let server = producer
			.forward("room", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let pending = producer.consume().request("room/alice", None);
		let source = broadcast::Info::new().produce();
		queued(&server).await.serve(&source);
		let resolved = pending.await.unwrap();

		source.close();
		settle(|| resolved.is_closed()).await;
		assert!(
			server.poll_requested(&kio::Waiter::noop()).is_pending(),
			"the closed source was requested again"
		);
	}

	/// An origin front drops the source track as soon as its last reader leaves,
	/// so the publisher's demand goes unwanted far below `track::IDLE_LINGER`.
	/// Cached groups stay on the front for the linger; a returning reader
	/// replays them and asks the source again for groups past that edge.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn origin_front_drops_the_source_when_unused() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"cached".as_ref()).unwrap();
		group.close().unwrap();

		let resolved = consumer.request("room/alice", None).await.expect("resolves");
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

		kio::clock::timeout(Duration::from_secs(1), track.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("source unused should resolve far below track::IDLE_LINGER")
			.expect("source closed");

		// Cached groups stay on the front for the linger; a returning reader
		// replays them without waiting out the window.
		let mut again = resolved
			.track("video")
			.unwrap()
			.subscribe(
				track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(3600))),
			)
			.await
			.expect("resubscribe");
		let mut group = kio::clock::timeout(Duration::from_secs(1), again.recv_group())
			.await
			.expect("cached group is still on the front")
			.expect("recv group")
			.expect("track ended early");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"cached");

		kio::clock::timeout(Duration::from_secs(1), track.demand().until(|wanted| wanted.is_some()))
			.await
			.expect("returning reader re-splices the source")
			.expect("source closed");

		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"live".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = kio::clock::timeout(Duration::from_secs(1), again.recv_group())
			.await
			.expect("groups past the cached edge come from the re-splice")
			.expect("recv group")
			.expect("track ended early");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"live");
	}

	/// A reader still holding an open group after its track goes unread gets the rest
	/// of the group: parking the track reads no new group, but the upstream stays
	/// subscribed until the held one ends.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_held_group_continues_after_its_track_goes_unread() {
		let producer = origin(1).produce();
		let server = producer
			.forward("live", RouteInfo::new().with_hops(hops(&[10])))
			.unwrap();
		let pending = producer.consume().request("live", None);
		let source = broadcast::Info::new().produce();
		let track = source.create_track("video", crate::track::Info::timed()).unwrap();
		queued(&server).await.serve(&source);
		let resolved = pending.await.unwrap();

		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut writing = track.append_next().unwrap();
		writing.write_frame(writing.key_time(), b"head".as_ref()).unwrap();
		let mut reading = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&reading.read_frame().await.unwrap().unwrap().payload[..], b"head");

		drop(subscription);
		kio::clock::sleep(Duration::from_millis(10)).await;
		writing.write_frame(writing.key_time(), b"tail".as_ref()).unwrap();
		writing.close().unwrap();

		let tail = kio::clock::timeout(Duration::from_secs(1), reading.read_frame())
			.await
			.expect("the held group lost its source")
			.unwrap()
			.expect("the group ended early");
		assert_eq!(&tail.payload[..], b"tail");
		let end = kio::clock::timeout(Duration::from_secs(1), reading.read_frame()).await;
		assert!(matches!(end, Ok(Ok(None))), "the group should finish: {end:?}");
		kio::clock::timeout(Duration::from_secs(1), track.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("the front lets the source go once the held group ends")
			.expect("source closed");
	}

	/// A source that aborts one group while its track carries on ends that group for
	/// the reader too, rather than leaving it waiting for a route change that never
	/// comes.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_group_the_source_aborts_ends_while_the_track_lives() {
		let producer = origin(1).produce();
		let broadcast = producer.route("live", RouteOptions::default()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = producer.consume().request("live", None).await.unwrap();
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();

		let mut aborted = track.append_next().unwrap();
		aborted.write_frame(aborted.key_time(), b"head".as_ref()).unwrap();
		let mut reading = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&reading.read_frame().await.unwrap().unwrap().payload[..], b"head");
		aborted.abort(Error::Cancel).unwrap();

		let end = kio::clock::timeout(Duration::from_secs(1), reading.read_frame()).await;
		assert!(matches!(end, Ok(Err(_))), "the group should fail: {end:?}");

		// The track carries on.
		let mut next = track.append_next().unwrap();
		next.write_frame(next.key_time(), b"next".as_ref()).unwrap();
		next.close().unwrap();
		let mut group = kio::clock::timeout(Duration::from_secs(1), subscription.recv_group())
			.await
			.expect("the next group")
			.unwrap()
			.unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"next");
	}

	/// A front serving from another front still drops upstream on the unused edge,
	/// so the publisher's demand goes unwanted far below `track::IDLE_LINGER` through
	/// the whole chain, and a returning reader reaches the publisher's cache afresh.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn chained_front_drops_the_source_when_unused() {
		let leaf = origin(1).produce();
		let leaf_consumer = leaf.consume();

		let broadcast = leaf.route("room/alice", RouteOptions::default()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"cached".as_ref()).unwrap();
		group.close().unwrap();

		// The leaf's front view, which the next front serves from.
		let leaf_front = leaf_consumer.request("room/alice", None).await.expect("resolves");

		let mid = origin(2).produce();
		let mid_server = mid.forward("room", RouteInfo::new().with_hops(hops(&[10]))).unwrap();
		let mid_pending = mid.consume().request("room/alice", None);
		queued(&mid_server).await.serve(&leaf_front);
		let mid_resolved = mid_pending.await.expect("mid resolves");

		let edge = origin(3).produce();
		let edge_server = edge.forward("room", RouteInfo::new().with_hops(hops(&[20]))).unwrap();
		let edge_pending = edge.consume().request("room/alice", None);
		queued(&edge_server).await.serve(&mid_resolved);
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

		kio::clock::timeout(Duration::from_secs(5), track.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("chained unused should resolve far below track::IDLE_LINGER")
			.expect("source closed");

		// A budget spanning the cache, so the returning reader replays it.
		let mut subscription = edge_resolved
			.track("video")
			.unwrap()
			.subscribe(
				track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(3600))),
			)
			.await
			.expect("resubscribe");
		kio::clock::timeout(Duration::from_secs(5), track.demand().until(|wanted| wanted.is_some()))
			.await
			.expect("resubscribe should reach the leaf")
			.expect("source open");
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"live".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"cached");
		drop(group);
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"live");
		drop(group);
		drop(subscription);

		kio::clock::timeout(Duration::from_secs(5), track.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("second chained unused should resolve far below track::IDLE_LINGER")
			.expect("source closed");

		// A group the leaf produced while every front was parked: the fetch re-splices
		// each hop to reach the leaf.
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"fetched".as_ref()).unwrap();
		group.close().unwrap();
		let fetch = edge_resolved.track("video").unwrap().fetch_group(2, None);
		let mut fetch = std::pin::pin!(fetch);
		assert!(futures::poll!(fetch.as_mut()).is_pending(), "fetch should re-splice");
		kio::clock::timeout(Duration::from_secs(5), track.demand().until(|wanted| wanted.is_some()))
			.await
			.expect("fetch should reach the leaf")
			.expect("source open");
		let mut group = kio::clock::timeout(Duration::from_secs(5), fetch)
			.await
			.expect("re-spliced source should answer the fetch")
			.expect("fetch succeeds");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"fetched");
	}

	/// A newer local source wins dispatch the moment it attaches, but one whose copy
	/// of the track carries different metadata is refused: the incumbent keeps
	/// serving, and the refusal is never retried once the incumbent leaves.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn incompatible_local_source_keeps_the_incumbent() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		let first = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let track = first
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let mut subscription = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");

		// The newest source is dispatched the track, and refused for its metadata.
		let second = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let _incompatible = second
			.broadcast()
			.create_track("video", track::Info::default().with_timescale(crate::Timescale::MICRO))
			.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		// Still spliced to the incumbent, still delivering.
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"still".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"still");

		// The incumbent leaving exhausts the table: the refusal is never retried.
		drop(track);
		first.broadcast().close();
		assert!(matches!(subscription.recv_group().await, Err(Error::Unsupported)));
	}

	/// A remote front serving "room/alice" with one read track, "video", whose first
	/// group was delivered. Returns the producer, the incumbent's track, and the reader.
	#[cfg(feature = "async")]
	async fn remote_front() -> (Producer, Claim, broadcast::Producer, track::Producer, track::Subscriber) {
		let producer = origin(1).produce();
		let server = producer
			.forward(
				"room",
				RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[10])),
			)
			.unwrap();
		let pending = producer.consume().request("room/alice", None);
		let upstream = broadcast::Info::new().produce();
		let old = upstream.create_track("video", crate::track::Info::timed()).unwrap();
		queued(&server).await.serve(&upstream);
		let resolved = pending.await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = next_group(&mut subscription).await.unwrap().expect("first group");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");
		(producer, server, upstream, old, subscription)
	}

	/// A local newcomer takes over while its answer for "video" is pending, so the track
	/// drains the incumbent. The reader leaves: the incumbent's copy is dropped at once,
	/// like any unread track's, not held until the linger.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_unread_draining_track_releases_the_old_source() {
		let (producer, _server, _upstream, old, subscription) = remote_front().await;

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let _unanswered = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");

		drop(subscription);
		kio::clock::timeout(Duration::from_secs(1), old.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("the draining source stays subscribed with nobody reading")
			.expect("open");
	}

	/// The newcomer answers "video" with matching metadata but aborts before delivering a
	/// group, all before the front polls the answer: that is a refusal, so the incumbent
	/// keeps serving rather than the track ending on the newcomer's error.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_replacement_aborted_without_history_keeps_the_incumbent() {
		let (producer, _server, _upstream, old, mut subscription) = remote_front().await;

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");
		request
			.accept(crate::track::Info::timed())
			.abort(Error::App(7))
			.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"still".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = next_group(&mut subscription)
			.await
			.expect("the incumbent keeps serving")
			.expect("track open");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"still");

		// The incumbent's clean end is the track's, not the newcomer's error.
		let _old = old.finish().unwrap();
		assert!(matches!(next_group(&mut subscription).await, Ok(None)));
	}

	/// The serving route is withdrawn while its copy of the track is still open, and the
	/// next route for the same publisher no longer has the broadcast: its refusal must not
	/// end the track, which takes the incumbent's remaining groups and clean end.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_refused_replacement_keeps_a_withdrawn_incumbent_open() {
		// The incumbent arrives from relay 2, a stale standby from relay 3, both for
		// publisher 9, so withdrawing relay 2 leaves the standby standing.
		let producer = origin(1).produce();
		let server = producer
			.forward(
				"room",
				RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[9, 2])),
			)
			.unwrap();
		let standby = producer
			.forward(
				"room",
				RouteInfo::new()
					.with_instance(instance(), 0)
					.with_hops(hops(&[9, 5, 3])),
			)
			.unwrap();
		let pending = producer.consume().request("room/alice", None);
		let upstream = broadcast::Info::new().produce();
		let old = upstream.create_track("video", crate::track::Info::timed()).unwrap();
		queued(&server).await.serve(&upstream);
		let resolved = pending.await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = next_group(&mut subscription).await.unwrap().expect("first group");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");

		// The peer withdraws the broadcast: the route retracts and its source closes,
		// while the subscription already in flight stays open.
		server.withdrawn();
		upstream.close();

		let successor = broadcast::Info::new().produce();
		let mut handler = successor.requests();
		queued(&standby).await.serve(&successor);
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the standby")
			.expect("request");
		request.reject(Error::Unroutable);
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"last".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = next_group(&mut subscription)
			.await
			.expect("the incumbent keeps serving")
			.expect("track open");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"last");

		let _old = old.finish().unwrap();
		let end = next_group(&mut subscription)
			.await
			.map(|group| group.map(|group| group.start));
		assert!(matches!(end, Ok(None)), "the incumbent's clean end, not {end:?}");
	}

	/// The newcomer answers "video" with matching metadata, the front reads that answer,
	/// and only then does the newcomer abort, before its first group: still a refusal, so
	/// the incumbent keeps serving.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_replacement_aborted_after_its_answer_keeps_the_incumbent() {
		let (producer, _server, _upstream, old, mut subscription) = remote_front().await;

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");
		let replacement = request.accept(crate::track::Info::timed());
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		replacement.abort(Error::App(7)).unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"still".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = next_group(&mut subscription)
			.await
			.expect("the incumbent keeps serving")
			.expect("track open");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"still");

		// The incumbent's clean end is the track's, not the newcomer's error.
		let _old = old.finish().unwrap();
		assert!(matches!(next_group(&mut subscription).await, Ok(None)));
	}

	/// The incumbent delivers its last group and finishes, and only then does the newcomer
	/// abort before its first group: the incumbent's clean end is the track's.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_replacement_aborted_after_the_incumbent_finished_ends_the_track_cleanly() {
		let (producer, _server, _upstream, old, mut subscription) = remote_front().await;

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");
		let replacement = request.accept(crate::track::Info::timed());
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"last".as_ref()).unwrap();
		group.close().unwrap();
		let _old = old.finish().unwrap();
		replacement.abort(Error::App(7)).unwrap();

		let mut group = next_group(&mut subscription)
			.await
			.expect("the incumbent's last group")
			.expect("track open");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"last");
		assert!(matches!(next_group(&mut subscription).await, Ok(None)));
	}

	/// The incumbent finishes and the newcomer aborts before its first group: a reader whose
	/// range starts past the last group, which contains its start, and one joining while the
	/// newcomer is awaited, each read that group and end cleanly.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn readers_owed_nothing_below_the_incumbents_end_end_cleanly() {
		let (producer, _server, _upstream, old, _subscription) = remote_front().await;
		let resolved = producer.consume().request("room/alice", None).await.unwrap();
		let video = resolved.track("video").unwrap();
		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"last".as_ref()).unwrap();
		group.close().unwrap();
		let mut later = video
			.subscribe(crate::track::Subscription {
				ranges: vec![crate::track::Range {
					start: crate::track::Anchor::Time(3),
					end: None,
				}],
				..Default::default()
			})
			.await
			.unwrap();

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");
		let replacement = request.accept(crate::track::Info::timed());
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		let mut joiner = video.subscribe(None).await.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		let _old = old.finish().unwrap();
		replacement.abort(Error::App(7)).unwrap();

		for reader in [&mut later, &mut joiner] {
			let mut group = next_group(reader)
				.await
				.expect("the incumbent's latest group")
				.expect("track open");
			assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"last");
			assert!(matches!(next_group(reader).await, Ok(None)));
		}
	}

	/// While the front waits for the newcomer's first group, a reader changing its
	/// preferences reaches the newcomer's copy, which may otherwise never send that group.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_awaited_replacement_follows_the_readers_demand() {
		let (producer, _server, _upstream, _old, mut subscription) = remote_front().await;

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");
		let replacement = request.accept(crate::track::Info::timed());
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		assert_eq!(replacement.subscription().map(|wanted| wanted.priority), Some(0));

		let preferences = crate::track::Subscription {
			priority: 9,
			..subscription.subscription()
		};
		subscription.update(preferences).unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		assert_eq!(replacement.subscription().map(|wanted| wanted.priority), Some(9));
	}

	/// Before the incumbent delivered anything, the newcomer answers "video" with matching
	/// metadata and then finishes without a group: a deliberate end of the broadcast's
	/// track, so the track ends rather than the front waiting on a group that never comes.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_replacement_finished_without_a_group_ends_the_track() {
		let producer = origin(1).produce();
		let server = producer
			.forward(
				"room",
				RouteInfo::new().with_instance(instance(), 0).with_hops(hops(&[10])),
			)
			.unwrap();
		let pending = producer.consume().request("room/alice", None);
		let upstream = broadcast::Info::new().produce();
		let _old = upstream.create_track("video", crate::track::Info::timed()).unwrap();
		queued(&server).await.serve(&upstream);
		let resolved = pending.await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");
		let replacement = request.accept(crate::track::Info::timed());
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		let replacement = replacement.finish().unwrap();

		assert!(matches!(next_group(&mut subscription).await, Ok(None)));
		drop(replacement);
	}

	/// The newcomer is asked while only an info lookup wants "video", so its copy has no
	/// subscription. A live reader joining while the front waits on its first group
	/// subscribes it, or that group may never come.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_awaited_replacement_asked_without_readers_subscribes_when_one_joins() {
		let (producer, _server, _upstream, _old, subscription) = remote_front().await;
		let resolved = producer.consume().request("room/alice", None).await.unwrap();
		let video = resolved.track("video").unwrap();
		let _info = video.info();
		drop(subscription);
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		let newcomer = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = newcomer.broadcast().requests();
		newcomer.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");
		let replacement = request.accept(crate::track::Info::timed());
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		assert!(replacement.subscription().is_none(), "nobody reads the track yet");

		let _again = video.subscribe(None).await.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		assert!(
			replacement.subscription().is_some(),
			"the awaited copy never got the reader's demand"
		);
	}

	/// The newcomer refuses "video", so the track drains the incumbent. The reader leaves
	/// (the park drops the incumbent's copy), a third source that also lacks the track
	/// takes over, and the reader returns: it gets an outcome rather than waiting on the
	/// dropped copy.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_returning_reader_is_not_stranded_on_a_dropped_copy() {
		let (producer, _server, upstream, old, mut subscription) = remote_front().await;
		let resolved = producer.consume().request("room/alice", None).await.unwrap();

		// No handler and no track: refuses "video" with NotFound.
		let _second = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"draining".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = next_group(&mut subscription).await.unwrap().expect("draining group");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"draining");
		drop(group);

		drop(subscription);
		kio::clock::timeout(Duration::from_secs(1), old.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("parked")
			.expect("open");

		let _third = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		let mut again = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = old.append_next().unwrap();
		group.write_frame(group.key_time(), b"after".as_ref()).unwrap();
		group.close().unwrap();
		let outcome = kio::clock::timeout(Duration::from_secs(5), async {
			loop {
				match again.recv_group().await {
					Ok(Some(mut group)) => {
						if &group.read_frame().await.unwrap().unwrap().payload[..] == b"after" {
							return Ok(());
						}
					}
					Ok(None) => return Ok(()),
					Err(err) => return Err(err),
				}
			}
		})
		.await;
		assert!(outcome.is_ok(), "the returning reader waits on a copy the park dropped");
		drop(upstream);
	}

	/// The reader leaves, parking the track. A fetch of a whole group the parked copy
	/// caches answers at once, without the front serving the copy again; an open group
	/// still waits on the front, and is served once the copy is back.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_parked_track_serves_whole_groups_from_its_copy() {
		let (producer, _server, _upstream, old, subscription) = remote_front().await;
		let resolved = producer.consume().request("room/alice", None).await.unwrap();
		let mut open = old.append_next().unwrap();
		open.write_frame(open.key_time(), b"open".as_ref()).unwrap();
		drop(subscription);
		kio::clock::timeout(Duration::from_secs(1), old.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("parked")
			.expect("open");

		let track = resolved.track("video").unwrap();
		let mut whole = track
			.fetch_group(0, None)
			.now_or_never()
			.expect("a whole group needs no front")
			.unwrap();
		assert_eq!(&whole.read_frame().await.unwrap().unwrap().payload[..], b"before");

		let mut fetching = track.fetch_group(1, None);
		assert!(
			(&mut fetching).now_or_never().is_none(),
			"an open group needs the route"
		);
		open.close().unwrap();
		let mut group = fetching.await.unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"open");
	}

	/// A new source takes over a parked track: a whole group its old copy caches is no
	/// longer served from that copy, but asked of the new route, which lacks it.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_new_source_unparks_the_old_copy() {
		let (producer, _server, _upstream, old, subscription) = remote_front().await;
		let resolved = producer.consume().request("room/alice", None).await.unwrap();
		drop(subscription);
		kio::clock::timeout(Duration::from_secs(1), old.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("parked")
			.expect("open");

		let newcomer = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let _video = newcomer
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		let mut fetching = resolved.track("video").unwrap().fetch_group(0, None);
		assert!(
			(&mut fetching).now_or_never().is_none(),
			"served from the detached copy"
		);
		assert!(matches!(fetching.await, Err(Error::NotFound)));
	}

	/// A newer local source's copy of "video" is refused for its metadata while the
	/// incumbent keeps serving: the refused copy is let go, not kept subscribed.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_refused_copy_is_not_kept_subscribed() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let first = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let track = first
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");

		let second = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let incompatible = second
			.broadcast()
			.create_track("video", track::Info::default().with_timescale(crate::Timescale::MICRO))
			.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}
		assert!(
			!incompatible.demand().is_wanted(),
			"the refused copy is still subscribed"
		);

		// The refusal happened: the incumbent leaving surfaces it.
		drop(track);
		first.broadcast().close();
		assert!(matches!(subscription.recv_group().await, Err(Error::Unsupported)));
	}

	/// As above, the newer source's copy was refused and the incumbent keeps serving.
	/// Then both withdraw and the front ends: the track in flight carries on with the
	/// incumbent, never the refused copy.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn the_end_never_feeds_a_refused_copy() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let first = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let track = first
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");
		drop(group);

		let second = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let incompatible = second
			.broadcast()
			.create_track("video", track::Info::default().with_timescale(crate::Timescale::MICRO))
			.unwrap();
		for _ in 0..10 {
			kio::clock::sim::yield_now().await;
		}

		first.withdraw();
		second.withdraw();
		settle(|| resolved.is_closed()).await;

		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"still".as_ref()).unwrap();
		group.close().unwrap();
		for payload in [b"wrong0".as_ref(), b"wrong1".as_ref()] {
			let mut group = incompatible.append_next().unwrap();
			group.write_frame(group.key_time(), payload).unwrap();
			group.close().unwrap();
		}

		let mut group = next_group(&mut subscription)
			.await
			.expect("the in-flight track carries on")
			.expect("track ended early");
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!(
			&frame.payload[..],
			b"still",
			"the front's end spliced the copy it refused"
		);
	}

	/// The track drains the incumbent while a newer source's answer is pending, then both
	/// withdraw and the front ends: the track carries on with the incumbent's copy rather
	/// than the unanswered one.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn the_end_keeps_the_copy_still_serving() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let first = producer
			.route("room/alice", RouteOptions::default().with_instance(instance(), 0))
			.unwrap();
		let track = first
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"before".as_ref()).unwrap();
		group.close().unwrap();
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"before");
		drop(group);

		let second = producer
			.route("room/alice", RouteOptions::hidden().with_instance(instance(), 0))
			.unwrap();
		let mut handler = second.broadcast().requests();
		second.announce().unwrap();
		let request = kio::clock::timeout(Duration::from_secs(1), handler.requested_track())
			.await
			.expect("the front asked the newcomer")
			.expect("request");

		first.withdraw();
		second.withdraw();
		settle(|| resolved.is_closed()).await;

		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"still".as_ref()).unwrap();
		group.close().unwrap();
		request.reject(Error::NotFound);

		let mut group = next_group(&mut subscription)
			.await
			.expect("the in-flight track carries on")
			.expect("track ended early");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"still");
	}

	#[kio::test]
	async fn multiple_scopes_present_one_broad_prefix() {
		let producer = origin(1).produce();
		let _a = producer.announce("", RouteInfo::new()).unwrap();

		let consumer = producer
			.consume()
			.scope("", &scopes(&["alpha", "beta"]))
			.unwrap()
			.covering();
		let mut announced = consumer.routes();
		announced.expect_next_active("").await;
		announced.assert_next_wait();
	}

	#[kio::test]
	async fn scope_accepts_every_pattern_union() {
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

	#[kio::test]
	async fn route_table_prunes_to_empty() {
		let producer = origin(1).produce();
		let consumer = producer.consume();

		// Routes and cursors hang at their prefixes; the nodes on the way exist
		// only while something is there.
		let cursor = consumer
			.scope("", &scopes(&["room/a", "other/deep/head"]))
			.unwrap()
			.routes();
		let route = producer.announce("room/a/b/c", RouteInfo::new()).unwrap();
		{
			let table = producer.shared.lock();
			assert!(
				table
					.routes
					.root
					.find(Path::from_static("room/a/b/c").parts())
					.is_some()
			);
			assert!(
				table
					.routes
					.root
					.find(Path::from_static("other/deep/head").parts())
					.is_some()
			);
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
	#[kio::test]
	async fn a_published_broadcast_keeps_the_driver_running() {
		let (producer, mut driver) = Producer::new(Config::from(origin(1)));
		let waiter = kio::Waiter::noop();
		let broadcast = producer.route("room/a", RouteOptions::hidden()).unwrap();
		drop(producer);
		assert!(
			driver.poll(Instant::now(), &waiter).is_ok(),
			"the broadcast is lifecycle work"
		);
		drop(broadcast);
		assert!(matches!(driver.poll(Instant::now(), &waiter), Err(Error::Closed)));
	}

	/// The task feeding a route node never keeps the driver running: it finishes once
	/// the last owner drops, whatever read handles remain, and still feeds the node the
	/// withdrawals that last drop queued.
	#[kio::test]
	async fn a_route_node_never_keeps_the_driver_running() {
		let (producer, mut driver) = Producer::new(Config::from(origin(1)));
		let waiter = kio::Waiter::noop();
		let link = crate::route::Link {
			proxy: true,
			..Default::default()
		};
		let consumer = producer.consume();
		let alice = producer.announce("room/alice", RouteInfo::new()).unwrap();
		let stream = consumer.route_node().answer(link, Path::empty()).unwrap();
		assert!(
			driver.poll(Instant::now(), &waiter).is_ok(),
			"the producer owns the origin"
		);
		while stream.try_output().is_some() {}
		drop(alice);
		drop(producer);
		assert!(matches!(driver.poll(Instant::now(), &waiter), Err(Error::Closed)));
		let output = stream.try_output();
		assert!(
			matches!(
				&output,
				Some(crate::route::node::Output::Batch { changes, .. })
					if changes.iter().any(|(path, claim)| path.as_str() == "room/alice" && claim.is_none())
			),
			"the stream is sent the withdrawal: {output:?}"
		);
	}

	/// A handler holding only its `Route` keeps serving once every producer
	/// handle drops, and the driver finishes once the handler is gone too.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_dynamic_keeps_the_driver_running() {
		let (producer, driver) = Producer::new(Config::from(origin(1)));
		let run = kio::clock::sim::spawn(crate::time::run(driver));
		let consumer = producer.consume();
		let server = producer.forward("room", RouteInfo::new()).unwrap();
		drop(producer);

		let pending = consumer.request("room/alice", None);
		let request = queued(&server).await;
		let source = broadcast::Info::new().produce();
		request.serve(&source);
		let resolved = pending.await.expect("the dynamic still serves");

		drop(resolved);
		drop(source);
		drop(server);
		kio::clock::timeout(Duration::from_secs(5), run)
			.await
			.expect("driver must finish once the dynamic is gone")
			.unwrap();
		drop(consumer);
	}

	/// A reader still on a retracted broadcast's track keeps its front draining, but
	/// never keeps the driver from finishing once nothing owns the origin.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_retracted_reader_never_holds_the_driver() {
		let (producer, driver) = Producer::new(Config::from(origin(1)));
		let run = kio::clock::sim::spawn(crate::time::run(driver));
		let consumer = producer.consume();
		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let sub = resolved.track("video").unwrap().subscribe(None).await.unwrap();

		broadcast.withdraw();
		settle(|| resolved.is_closed()).await;
		drop(broadcast);
		drop(producer);
		kio::clock::timeout(Duration::from_secs(5), run)
			.await
			.expect("driver must finish once the producers are gone")
			.unwrap();
		drop(sub);
		drop(track);
	}

	/// A subscriber that asked before the retraction but was not polled yet still finds
	/// the copy after the origin is orphaned and its driver finishes.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_orphaned_front_keeps_the_copy_for_a_pending_subscriber() {
		let (producer, driver) = Producer::new(Config::from(origin(1)));
		let run = kio::clock::sim::spawn(crate::time::run(driver));
		let consumer = producer.consume();
		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let mut track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let sub = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let pending = resolved.track("video").unwrap().subscribe(None);

		broadcast.withdraw();
		settle(|| resolved.is_closed()).await;
		drop(broadcast);
		drop(producer);
		kio::clock::timeout(Duration::from_secs(5), run)
			.await
			.expect("driver must finish once the producers are gone")
			.unwrap();
		track.write_frame(crate::Timestamp::ZERO, b"late".as_ref()).unwrap();
		let mut late = pending.await.expect("subscribes");
		let mut group = late.recv_group().await.expect("recv").expect("the source's group");
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"late");
		drop(sub);
	}

	/// A track whose last reader leaves as its broadcast ends, before the front parks it,
	/// lets go of its copy: the source is not kept subscribed for nobody.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn a_track_unread_as_its_broadcast_ends_releases_its_source() {
		let producer = origin(1).produce();
		let consumer = producer.consume();
		let broadcast = producer.route("room/alice", RouteOptions::default()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let resolved = consumer.request("room/alice", None).await.expect("resolves");
		let sub = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		kio::clock::timeout(Duration::from_secs(1), track.demand().until(|wanted| wanted.is_some()))
			.await
			.expect("the front subscribed the source")
			.unwrap();

		// The front looks at a closed source before demand edges, so it ends with the
		// track unread and its copy not parked yet.
		drop(sub);
		drop(broadcast);
		settle(|| resolved.is_closed()).await;

		kio::clock::timeout(Duration::from_secs(5), track.demand().until(|wanted| wanted.is_none()))
			.await
			.expect("the ended copy must not keep the source subscribed")
			.unwrap();
	}

	#[kio::test]
	async fn source_selection_leases_survive_reprices_but_never_revive_pending_work() {
		let producer = origin(1).produce();
		let broad = producer.forward("room", RouteInfo::new().with_via(origin(2))).unwrap();
		let selected = broad.request_validity(&Path::from_static("room/a")).unwrap();
		let pending = selected.current();
		assert!(!pending.is_closed());
		broad.update(broad.route().unwrap().with_cost(Cost::from(7))).unwrap();
		assert!(!pending.is_closed());
		// Another peer's winning route cannot change what a request on peer 2 names.
		let other = producer
			.forward("room/a", RouteInfo::new().with_via(origin(3)))
			.unwrap();
		assert!(!pending.is_closed());
		let replacement = producer
			.forward("room/a", RouteInfo::new().with_via(origin(2)))
			.unwrap();
		assert!(pending.is_closed());
		assert!(selected.current().is_closed());
		// No waiter has polled between these changes: the old lease must still be dead.
		drop(replacement);
		let fresh = selected.current();
		assert!(!fresh.is_closed());
		assert!(pending.is_closed());
		drop(other);
		assert!(!fresh.is_closed());
		drop(broad);
		assert!(fresh.is_closed());
	}

	/// The answering lease outlives a withdrawal, since the peer still answers for the
	/// instance it retracted, and ends once another instance takes the path.
	#[kio::test]
	async fn source_selection_answering_ends_only_on_a_replacement() {
		let producer = origin(1).produce();
		let path = Path::from_static("room/a");
		let first = producer.forward("room", RouteInfo::new().with_via(origin(2))).unwrap();
		let selected = first.request_validity(&path).unwrap();
		let answering = selected.answering();
		// A route that loses to the selected one replaces nothing.
		let worse = producer
			.forward("room", RouteInfo::new().with_via(origin(2)).with_cost(Cost::from(7)))
			.unwrap();
		assert!(!answering.is_closed());
		drop(worse);

		let replacement = producer
			.forward("room/a", RouteInfo::new().with_via(origin(2)))
			.unwrap();
		assert!(answering.is_closed());
		// Winning again opens a new lease, which a withdrawal leaves open.
		drop(replacement);
		let answering = selected.answering();
		assert!(!answering.is_closed());
		drop(first);
		assert!(selected.current().is_closed());
		assert!(!answering.is_closed());

		// Announced again, the path has a new instance: a replacement.
		let _again = producer.forward("room", RouteInfo::new().with_via(origin(2))).unwrap();
		assert!(answering.is_closed());
	}

	#[kio::test]
	async fn source_selection_unregisters_without_retaining_the_source() {
		let producer = origin(1).produce();
		let route = producer.forward("room", RouteInfo::new().with_via(origin(2))).unwrap();
		let path = Path::from_static("room/a");
		let selected = route.request_validity(&path).unwrap();
		let lease = selected.current();
		drop(selected);
		assert!(lease.is_closed());
		let table = producer.shared.lock();
		assert_eq!(table.routes.root.watches_below, 0);
	}

	#[kio::test]
	async fn watch_wakes_only_for_covering_changes() {
		let producer = origin(1).produce();
		let waiter = kio::Waiter::noop();
		let watch = producer
			.shared
			.lock()
			.watch(&producer.shared, &Path::from_static("room/a"));
		let seen = watch.seen();

		// A route beside the path or beneath it covers nothing at the path.
		let _other = producer.announce("other", RouteInfo::new()).unwrap();
		let _below = producer.announce("room/a/b", RouteInfo::new()).unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_pending());

		// A route above it does, and so does its retraction.
		let above = producer.announce("room", RouteInfo::new()).unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_ready());
		let seen = watch.seen();
		drop(above);
		assert!(watch.poll_changed(&waiter, seen).is_ready());
		let seen = watch.seen();

		// A local broadcast attaching at the exact path does; one beside it does not.
		let _beside = producer.route("room/b", RouteOptions::hidden()).unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_pending());
		let _here = producer.route("room/a", RouteOptions::hidden()).unwrap();
		assert!(watch.poll_changed(&waiter, seen).is_ready());

		// Dropping the watch takes it out of the table.
		drop(watch);
		let table = producer.shared.lock();
		let node = table
			.routes
			.root
			.find(Path::from_static("room/a").parts())
			.expect("route below keeps the node");
		assert!(node.watches.is_empty());
		assert_eq!(table.routes.root.watches_below, 0);
	}

	#[kio::test]
	async fn a_discarded_front_task_unregisters_its_watch() {
		let (producer, _driver) = Producer::new(Config::from(origin(1)));
		let consumer = producer.consume();
		let _served = producer.forward("room", RouteInfo::new()).unwrap();
		// A consumer outlives its producer by design, so the task set can refuse
		// submissions while the origin is still open. The front's task is then
		// dropped on the spot, taking its `Watch` with it: the request must not
		// still be holding the table lock the watch unregisters under.
		drop(producer);
		let _pending = consumer.request("room/a", None);
	}

	#[kio::test]
	async fn published_names_refuse_navigation_segments() {
		let producer = origin(1).produce();
		for path in ["room/./cam", "room/../cam", ".", ".."] {
			assert!(producer.route(path, RouteOptions::hidden()).is_err(), "created {path}");
			assert!(producer.announce(path, RouteInfo::new()).is_err(), "announced {path}");
		}
		let scoped = producer
			.scope("room/..", &Patterns::from_iter([Pattern::all()]))
			.unwrap();
		assert!(scoped.route("cam", RouteOptions::hidden()).is_err());
		for path in ["room/.pro/cam", "room/a..b", "room/cam"] {
			let _route = producer.route(path, RouteOptions::hidden()).unwrap();
		}
	}

	#[kio::test]
	async fn route_refuses_a_path_no_pattern_can_spell() {
		let producer = origin(1).produce();

		// A `*` segment is a valid path but an invalid literal, so its route could
		// never be built: refuse the broadcast instead of publishing one that
		// announces nowhere.
		assert!(matches!(
			producer.route("room/*", RouteOptions::hidden()),
			Err(Error::InvalidPath(_))
		));
		assert!(matches!(
			producer.announce("room/**", RouteInfo::new()),
			Err(Error::InvalidPath(_))
		));
	}

	#[kio::test]
	async fn scope_empty_union_grants_nothing() {
		let producer = origin(1).produce();

		// An empty union grants nothing: scoping is refused, like a disjoint prefix.
		assert!(matches!(producer.scope("", &Patterns::new()), Err(Error::Unauthorized)));
		assert!(matches!(
			producer.consume().scope("", &Patterns::new()),
			Err(Error::Unauthorized)
		));
	}

	#[kio::test]
	async fn scope_nests_and_rebases_roots() {
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
		let broadcast = nested.route("room/chat/live", RouteOptions::hidden()).unwrap();
		assert!(producer.consume().get_broadcast("room/chat/live").is_some());
		broadcast.broadcast().close();
	}

	#[kio::test]
	async fn scope_intersects_and_rebases_arbitrary_grants() {
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

		let broadcast = exact.route("room/alice", RouteOptions::hidden()).unwrap();
		assert!(matches!(
			exact.route("room/alice/cam", RouteOptions::hidden()),
			Err(Error::Unauthorized)
		));
		assert!(producer.consume().get_broadcast("room/alice").is_some());
		drop(broadcast);
	}

	#[kio::test]
	async fn wildcard_scope_filters_announcements_and_reports_captures() {
		let producer = origin(1).produce();
		let consumer = producer
			.consume()
			.scope("", &Patterns::from("room/*/chat".parse::<Pattern>().unwrap()))
			.unwrap();
		let mut listed = consumer.routes();
		let mut announced = consumer.covering().routes();

		let alice = producer.route("room/alice/chat", RouteOptions::hidden()).unwrap();
		alice.announce().unwrap();
		let Some(RouteEvent {
			kind: RouteKind::Start,
			announce: update,
		}) = announced.try_next()
		else {
			panic!("expected alice's chat");
		};
		assert_eq!(update.prefix.as_str(), "room/alice/chat");
		assert_eq!(update.captures, Some(vec!["alice".parse::<Pattern>().unwrap()]));
		listed.expect_next_active("room/alice/chat").await;

		let audio = producer.route("room/alice/audio", RouteOptions::hidden()).unwrap();
		audio.announce().unwrap();
		announced.assert_next_wait();

		let broad = producer.announce("room", RouteInfo::new()).unwrap();
		let Some(RouteEvent {
			kind: RouteKind::Start,
			announce: update,
		}) = announced.try_next()
		else {
			panic!("expected the overlapping broad route");
		};
		assert_eq!(update.prefix.as_str(), "room");
		assert_eq!(update.captures, None, "an overlap does not pin the wildcard");
		listed.assert_next_wait();

		drop(broad);
		drop(audio);
		drop(alice);
	}

	#[kio::test]
	async fn announced_lists_full_names_in_replay_and_live_changes() {
		let producer = origin(1).produce();
		let patterns: Patterns = ["*.hang", "**/*.chat"]
			.into_iter()
			.map(|p| p.parse::<Pattern>().unwrap())
			.collect();
		let consumer = producer.consume().scope("project", &patterns).unwrap();
		let cam = producer.announce("project/cam.hang", RouteInfo::new()).unwrap();
		let overlap = producer.announce("project/room/mic.hang", RouteInfo::new()).unwrap();
		let chat = producer.announce("project/lobby.chat", RouteInfo::new()).unwrap();
		let mut routing = consumer.clone().covering().routes();
		routing.expect_next_active("cam.hang").await;
		routing.expect_next_active("lobby.chat").await;
		routing.expect_next_active("room/mic.hang").await;
		let mut selected = consumer.routes();
		selected.expect_next_active("cam.hang").await;
		selected.expect_next_active("lobby.chat").await;
		selected.assert_next_wait();

		let nested = producer.announce("project/room/lobby.chat", RouteInfo::new()).unwrap();
		selected.expect_next_active("room/lobby.chat").await;
		overlap.update(RouteInfo::new().with_cost(2)).unwrap();
		drop(overlap);
		selected.assert_next_wait();
		drop(nested);
		selected.expect_next_ended("room/lobby.chat").await;
		drop(cam);
		drop(chat);
	}

	#[kio::test]
	async fn announced_keeps_lifecycle_and_filters_before_rerooting() {
		let producer = origin(1).produce();
		let consumer = producer
			.consume()
			.scope("room/alice", &Patterns::from(Pattern::all()))
			.unwrap();
		let broad = producer.announce("room", RouteInfo::new()).unwrap();
		let mut selected = consumer.routes();
		selected.assert_next_wait();

		let first_instance = crate::Instance::random();
		let exact = producer
			.announce("room/alice", RouteInfo::new().with_instance(first_instance.clone(), 0))
			.unwrap();
		assert!(
			matches!(selected.expect_next().await, RouteEvent { kind: RouteKind::Start, announce: a } if a.prefix.is_empty() && a.captures.is_some())
		);
		exact
			.update(RouteInfo::new().with_instance(first_instance, 0).with_cost(2))
			.unwrap();
		assert!(
			matches!(selected.expect_next().await, RouteEvent { kind: RouteKind::Update, announce: a } if a.route.cost == Cost::new(2))
		);
		exact
			.update(RouteInfo::new().with_instance(crate::Instance::random(), 0))
			.unwrap();
		assert!(matches!(
			selected.expect_next().await,
			RouteEvent {
				kind: RouteKind::Restart,
				..
			}
		));
		drop(exact);
		selected.expect_next_ended("").await;
		selected.assert_next_wait();
		let _again = producer.announce("room/alice", RouteInfo::new()).unwrap();
		assert!(matches!(
			selected.expect_next().await,
			RouteEvent {
				kind: RouteKind::Start,
				..
			}
		));
		drop(broad);
		selected.assert_next_wait();
		drop(selected);
		let table = producer.shared.lock();
		assert!(table.cursors.is_empty());
		assert!(table.routes.interests_from(&Path::empty()).is_empty());
	}

	#[kio::test]
	async fn announced_matches_mounted_names_and_keeps_exclusions() {
		let producer = origin(1).produce();
		let _broad = producer.announce(".svc", RouteInfo::new()).unwrap();
		let _overlap = producer.announce(".svc/p1/mic.hang", RouteInfo::new()).unwrap();
		let _hidden = producer
			.announce(".svc/p1/.private/lobby.chat", RouteInfo::new())
			.unwrap();
		let _excluded = producer
			.announce(".svc/p1/private/lobby.chat", RouteInfo::new())
			.unwrap();
		let project = mounted(&producer, "p1", &["**/*.chat"])
			.with_hidden(true)
			.exclude(".svc/private")
			.unwrap();
		let mut selected = project.routes();
		selected.expect_next_active(".svc/.private/lobby.chat").await;
		selected.assert_next_wait();
		let mut inside = project.scope(".svc", &Patterns::from(Pattern::all())).unwrap().routes();
		inside.expect_next_active(".private/lobby.chat").await;
		inside.assert_next_wait();
		let live = producer.announce(".svc/p1/lobby.chat", RouteInfo::new()).unwrap();
		selected.expect_next_active(".svc/lobby.chat").await;
		inside.expect_next_active("lobby.chat").await;
		drop(live);
		selected.expect_next_ended(".svc/lobby.chat").await;
		inside.expect_next_ended("lobby.chat").await;
	}

	#[kio::test]
	#[cfg(feature = "async")]
	async fn announced_requests_covering_routes_and_preserves_shared_fronts_and_stats() {
		let registry = stats::Registry::new(stats::Config::new());
		let producer = origin(1).produce();
		let local = producer.route("project/cam.chat", RouteOptions::default()).unwrap();
		let overlap = producer.announce("project/room", RouteInfo::new()).unwrap();
		let consumer = producer
			.consume()
			.scope("project", &Patterns::from("**/*.chat".parse::<Pattern>().unwrap()))
			.unwrap()
			.local()
			.with_stats(
				registry
					.tier(stats::Tier::default())
					.session(crate::stats::SessionInfo::new(Path::from_static("project"))),
			);
		let mut selected = consumer.routes();
		selected.expect_next_active("cam.chat").await;
		selected.assert_next_wait();
		let resolved = consumer.request("cam.chat", None).await.unwrap();
		let original = producer
			.consume()
			.local()
			.request("project/cam.chat", None)
			.await
			.unwrap();
		assert!(
			resolved.is_clone(&original),
			"the view shares the origin's existing front"
		);
		let track = local
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let mut written = track.append_next().unwrap();
		written.write_frame(written.key_time(), b"frame".as_ref()).unwrap();
		written.close().unwrap();
		let _track = track.finish().unwrap();
		let mut reader = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = reader.recv_group().await.unwrap().unwrap();
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"frame");
		let mut report = stats::Report::default();
		registry.report(&mut report, crate::time::Instant::now());
		assert_eq!(report.traffic.len(), 1);
		assert_eq!(report.traffic[0].path.as_str(), "project/cam.chat");
		assert_eq!(report.traffic[0].publisher.announces_started, 1);
		assert_eq!(report.traffic[0].publisher.bytes, 5);
		drop(selected);
		registry.report(&mut report, crate::time::Instant::now());
		assert_eq!(report.traffic[0].publisher.announces_ended, 1);

		// Discovery of a request still sees a covering route that enumeration omits.
		let mut routes = consumer.path_routes("room/lobby.chat").unwrap();
		assert!(routes.next().await.is_some());
		drop(routes);
		drop(overlap);
		let source = producer.claim("project/room", RouteOptions::default()).unwrap();
		let pending = consumer.request("room/lobby.chat", None);
		let request = source.requested().await.unwrap();
		request.serve(local.broadcast().consume());
		pending.await.unwrap();
	}

	#[kio::test]
	async fn local_broadcast_wins_announcement_ties() {
		let producer = origin(1).produce();
		let remote = producer.announce("room/alice", RouteInfo::new().with_cost(9)).unwrap();
		let local = producer.route("room/alice", RouteOptions::hidden()).unwrap();
		local.announce().unwrap();

		let mut announced = producer.consume().routes();
		let Some(RouteEvent {
			kind: RouteKind::Start,
			announce: update,
		}) = announced.try_next()
		else {
			panic!("expected one winning route");
		};
		assert_eq!(update.prefix.as_str(), "room/alice");
		assert_eq!(update.route.cost, Cost::default());
		announced.assert_next_wait();

		drop(local);
		drop(remote);
	}

	/// Charging a link accumulates onto the static price, saturating rather than wrapping
	/// so a bogus peer sorts last, not first. The ceiling is the largest cost a
	/// varint can carry, so whatever a peer advertises, the sum we forward still
	/// encodes.
	#[kio::test]
	async fn cost_charge_saturates() {
		assert_eq!(Cost::new(4).charged(5), Cost::new(9));
		assert_eq!(Cost::new(u64::MAX).charged(10), Cost::new(MAX_COST));

		assert_eq!(Cost::UNKNOWN.charged(3), Cost::new(3));
	}

	/// Mint an origin whose pool reclaims idle content after `expiry`.
	#[cfg(feature = "async")]
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
	#[kio::test]
	#[cfg(feature = "async")]
	async fn stalled_publisher_open_group_is_reclaimed() {
		let expiry = Duration::from_secs(1);
		let origin = expiring_origin(expiry);
		let broadcast = origin.route("test", RouteOptions::hidden()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();

		let mut stalled = track.append_next().unwrap();
		stalled.write_frame(stalled.key_time(), b"x".as_slice()).unwrap();
		// A successor, so the stalled group is not the protected live edge. Its
		// timestamp is inside the max age, so the stalled group stays fresh and
		// subscription expiry never skips it: only reclamation can bound it.
		let _successor = track.append_next().unwrap();

		let mut reading = stalled.consume();
		assert!(reading.read_frame().await.unwrap().is_some());

		// Production goes quiet: nothing writes to this track again. Bounded so a
		// regression fails rather than parking forever, which is the bug itself. Time
		// is simulated, so the wait costs nothing.
		let reclaimed = kio::clock::timeout(Duration::from_secs(60), reading.read_frame()).await;
		assert!(
			matches!(reclaimed, Ok(Err(Error::Old))),
			"the sweep must reclaim an idle open group and surface the gap, got {reclaimed:?}"
		);
	}

	/// Reclamation is the pool's policy, not the origin's: a pool with no expiry
	/// window keeps idle content until byte pressure takes it, sweep or no sweep.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn sweep_respects_a_disabled_expiry() {
		let origin = Config {
			pool: cache::Pool::unbounded(),
			..Config::default()
		}
		.produce();
		let broadcast = origin.route("test", RouteOptions::hidden()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();

		let mut stalled = track.append_next().unwrap();
		stalled.write_frame(stalled.key_time(), b"x".as_slice()).unwrap();
		let _successor = track.append_next().unwrap();

		let mut reading = stalled.consume();
		assert!(reading.read_frame().await.unwrap().is_some());

		kio::clock::sim::advance(Duration::from_secs(3600)).await;

		assert!(
			reading.read_frame().now_or_never().is_none(),
			"a pool without an expiry window never reclaims"
		);
	}
}
