//! The per-origin route layer, as one node's state machine.
//!
//! Routing is Babel (RFC 8966) keyed by routing identity: a node selects one
//! feasible route per identity, advertises it to every neighbor whose stream
//! asks, and only the identity's own node advances its seqno. Each origin
//! versions its own claims with a revision, and a node merges every neighbor's
//! certified view claim by claim, so no stale copy revives a withdrawal it
//! already knows. See `quest/m2/cluster-routes/README.md` for the contract.
//!
//! The router does no I/O: the caller feeds it what each neighbor sent and
//! drains [`Out`] messages and reader [`Event`]s. Neighbor keys `N` and claim
//! attributes `A` are opaque to it.

#![cfg_attr(
	not(feature = "async"),
	allow(dead_code, reason = "only the async session drives it")
)]

pub(crate) mod node;
pub(crate) mod path;
mod proof;
mod reader;
mod rtt;
mod view;

#[cfg(test)]
mod tests;

use std::{
	borrow::Cow,
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
	fmt,
	sync::Arc,
	time::Duration,
};

use crate::Path;

pub(crate) use node::Node;
#[cfg(feature = "async")]
pub(crate) use proof::EXPORTER_LABEL;
pub use proof::RouteKey;
pub(crate) use proof::{Proof, Transcript};
use reader::Reader;
#[cfg(feature = "async")]
pub(crate) use rtt::LinkRtt;
pub(crate) use view::{View, within};
use view::{above, below, covering, narrow, within_any};

/// An opaque routing identity, declared by an origin or derived by [`Scope`].
pub(crate) type Id = u64;

/// Set on every identity a [`Scope`] qualifies, so any node recognizes a
/// qualified key and refuses to qualify it again.
pub(crate) const QUALIFIED: Id = 1 << 63;

/// The largest identity a node generates: the 53 bits a generated hop id keeps, so
/// a generated identity travels as itself on a lite-06 hop chain (see `node::hop`).
pub(crate) const GENERATED: Id = (1 << 53) - 1;

/// A route metric: the sum of configured link costs.
pub(crate) type Metric = u64;

/// An origin's route seqno.
pub(crate) type Seqno = u64;

/// An origin's claim revision, advancing on every change to its own claims.
pub(crate) type Rev = u64;

/// The largest lite-07 varint. No origin ever advertises it as a seqno or revision,
/// rotating its identity instead, so every advertised seqno leaves room for the
/// successor a request names, and every revision for ANNOUNCE_FLOOR's offset.
pub(crate) const MAX: u64 = u64::MAX;

/// No floors, for a view this node does not keep.
static EMPTY_FLOORS: BTreeMap<Path, Rev> = BTreeMap::new();

/// A neighbor key, bounded so per-identity state can be read as one range.
pub(crate) trait Key: Copy + Ord + fmt::Debug {
	const MIN: Self;
	const MAX: Self;
}

impl Key for usize {
	const MIN: Self = usize::MIN;
	const MAX: Self = usize::MAX;
}

impl Key for u64 {
	const MIN: Self = u64::MIN;
	const MAX: Self = u64::MAX;
}

/// One side's configuration of a link to a neighbor.
#[derive(Clone)]
pub(crate) struct Link {
	/// Strictly positive: a zero-cost link would leave a route equal to its own
	/// feasibility distance.
	pub cost: Metric,
	/// The application's local priority for this connection; higher wins among
	/// feasible routes, before cost.
	pub priority: i8,
	/// The neighbor is upstream: routes learned here are down-only, and no
	/// down-only route is sent back.
	pub upstream: bool,
	/// Whether this neighbor is sent routes learned from other neighbors.
	pub proxy: bool,
	/// Set when the neighbor publishes under a scoped grant.
	pub scope: Option<Scope>,
}

impl Default for Link {
	fn default() -> Self {
		Self {
			cost: 1,
			priority: 0,
			upstream: false,
			proxy: true,
			scope: None,
		}
	}
}

/// A scoped neighbor's grant: every identity it sends is filed under a
/// qualified key, and its claims count only within the grant's regions.
#[derive(Clone)]
pub(crate) struct Scope {
	/// The prefixes the grant covers.
	pub regions: Vec<Path>,
	/// Derives the qualified key for an identity the neighbor names. It must
	/// set [`QUALIFIED`].
	pub qualify: Arc<dyn Fn(Id) -> Id + Send + Sync>,
}

impl fmt::Debug for Scope {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Scope")
			.field("regions", &self.regions)
			.finish_non_exhaustive()
	}
}

/// Qualifies the identities a scoped neighbor names under `key`: a hash of the key and
/// the identity, collision-resistant at 64 bits, with [`QUALIFIED`] set. Relays proving
/// the same key for a neighbor file its identities alike.
pub(crate) fn qualifier(key: [u8; 32]) -> Arc<dyn Fn(Id) -> Id + Send + Sync> {
	Arc::new(move |id: Id| {
		let mut ctx = rind::digest::Context::new(&rind::digest::SHA256);
		ctx.update(b"moq-lite qualified node id");
		ctx.update(&key);
		ctx.update(&id.to_le_bytes());
		let digest = ctx.finish();
		let mut bytes = [0; 8];
		bytes.copy_from_slice(&digest.as_ref()[..8]);
		u64::from_le_bytes(bytes) | QUALIFIED
	})
}

/// The most nodes a route crosses, its origin included: the longest hop list a
/// message carries.
pub(crate) const MAX_HOPS: usize = 32;

/// What a node advertises for one identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Advert {
	pub seqno: Seqno,
	pub metric: Metric,
	pub down_only: bool,
	/// The round trip to the identity, the sum of each link's min RTT: informational,
	/// never read to rank or select a route. `None` once a node on the path could not
	/// measure its link.
	pub rtt: Option<Duration>,
	/// The nodes the route crossed, by the hop id each names itself with: the origin
	/// node first and the sender last.
	pub hops: Arc<[Id]>,
}

/// The RTT through `peer` along a route it advertised: unknown when either part is.
fn through_rtt(advert: &Advert, peer: &Peer) -> Option<Duration> {
	advert.rtt?.checked_add(peer.rtt?)
}

/// A ROUTE's state on a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
	/// A usable route.
	Finite(Advert),
	/// Unusable, but the sender still holds the identity's announcements while it
	/// recovers. Only follows a finite route.
	Held,
}

/// A message for one neighbor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Out<N, A> {
	/// ROUTE_START or ROUTE_UPDATE, ROUTE_HOLD for a held reach, or ROUTE_END for `None`.
	Route { to: N, id: Id, reach: Option<Reach> },
	/// A batch of announcement changes for `id`, as the neighbor's request presents
	/// them, closed by ANNOUNCE_FLOOR: the revision the stream's claims for the route
	/// are the origin's at, across the neighbor's whole interest, or `None` when this
	/// node does not know them there at one revision.
	Batch {
		to: N,
		id: Id,
		changes: Vec<(Path, Option<A>)>,
		floor: Option<Rev>,
	},
	/// ROUTE_REQUEST, toward the neighbor whose route this node would use.
	Request { to: N, id: Id, seqno: Seqno },
	/// The echo of a ROUTE_END this node applied.
	Echo { to: N, id: Id },
}

/// The route a node selected for an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selected<N> {
	/// The neighbor it goes through, or `None` when this node is the identity.
	pub next: Option<N>,
	pub seqno: Seqno,
	/// This node's metric.
	pub metric: Metric,
	/// Whether an upstream link is on the path.
	pub down_only: bool,
}

/// How a node's reader sees one announced claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown<A> {
	pub item: A,
	/// Whether the node has a usable route to the claim's origin through a
	/// neighbor certifying it.
	pub available: bool,
	/// Whether the claim sits at the root of a region its view was presented at,
	/// where it stands for whichever claim covers that region.
	pub presented: bool,
}

/// A change a node's announcement reader sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
	Start,
	/// The claim's attributes changed.
	Update,
	/// The origin withdrew the claim, as far as this node knows.
	End,
	/// The node let go of the claim: no usable or recovering route certifies it.
	Offline,
	/// The node stopped watching the claim.
	Unwatched,
	Available,
	Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Event<A> {
	pub id: Id,
	pub claim: Path,
	pub kind: Kind,
	pub item: A,
	/// How many times this node forgot this claim, by collecting or retiring
	/// it, before this event: fixtures excuse a stale replay only after one.
	#[cfg(test)]
	pub forgot: u32,
}

/// A route a node holds from one neighbor.
#[derive(Clone, Debug)]
struct Entry {
	/// The last finite advertisement.
	advert: Advert,
	held: bool,
	/// Its hops name this node: a copy of a route this node sent, come back. This
	/// node never uses it, which the distance alone cannot ensure once it is gone.
	crossed: bool,
}

/// A seqno request a node keeps until a route at least that new is selected or
/// no route is left.
#[derive(Clone, Debug)]
struct Pending<N> {
	seqno: Seqno,
	/// Neighbors it forwards the request for; empty when only its own.
	from: BTreeSet<N>,
	/// The neighbor it last sent the request to.
	asked: Option<N>,
}

/// The last view a neighbor sent for one identity.
#[derive(Clone, Debug)]
struct Heard<A> {
	/// As the neighbor sent it, to apply the next batch to.
	raw: View<A>,
	/// What it certifies here, where that is not `raw`: presented within the
	/// neighbor's scope, or what it certified before its route ended.
	kept: Option<View<A>>,
	/// The route ended: the next batch starts a fresh view.
	ended: bool,
	/// The interest generation it answers.
	interest: u32,
	/// The claims within the view's floors where the merged view holds something
	/// else: the only ones merging the view can change.
	differs: BTreeSet<Path>,
}

impl<A> Heard<A> {
	fn new(interest: u32) -> Self {
		Self {
			raw: View::default(),
			kept: None,
			ended: false,
			interest,
			differs: BTreeSet::new(),
		}
	}

	/// What the neighbor certifies here.
	fn view(&self) -> &View<A> {
		self.kept.as_ref().unwrap_or(&self.raw)
	}
}

impl<A: Clone + Eq> Heard<A> {
	/// Note whether `claim` differs from `merged`'s.
	fn compare(&mut self, merged: Option<&View<A>>, claim: &Path) {
		let view = self.view();
		let mine = view.items.get(claim);
		let differs = view.as_of(claim).is_some() && merged.and_then(|merged| merged.items.get(claim)) != mine;
		match differs {
			true => self.differs.insert(claim.clone()),
			false => self.differs.remove(claim),
		};
	}

	/// Compare every claim against `merged`.
	fn resync(&mut self, merged: Option<&View<A>>) {
		let view = self.view();
		let mut claims: BTreeSet<Path> = view.items.keys().cloned().collect();
		if let Some(merged) = merged {
			for region in view.floors.keys() {
				let exact = merged.items.get_key_value(region);
				claims.extend(
					exact
						.into_iter()
						.chain(below(&merged.items, region))
						.map(|(claim, _)| claim.clone()),
				);
			}
		}
		self.differs.clear();
		for claim in &claims {
			self.compare(merged, claim);
		}
	}
}

struct Peer {
	link: Link,
	/// The link's min RTT, which routes learned here add; `None` while unmeasured.
	rtt: Option<Duration>,
	/// The neighbor sent GOAWAY: rank it below every other eligible route.
	draining: bool,
	/// The prefix the neighbor asked this node for, once it opened a stream.
	watch: Option<Path>,
	/// The prefix this node asked the neighbor for, which its floors certify; the
	/// node's interest when unset.
	asked: Option<Path>,
}

impl Peer {
	/// Whether a request for a claim at `claim` may go through this neighbor: the
	/// claim lies within what this node asked it for and within its scope. A route
	/// reaches an origin node, not each of its claims.
	fn carries(&self, claim: &Path) -> bool {
		self.asked.as_ref().is_none_or(|asked| within(claim, asked))
			&& (self.link.scope.as_ref()).is_none_or(|scope| scope.regions.iter().any(|region| within(claim, region)))
	}
}

/// The claims of an identity changed since it was last recomputed: the ones named,
/// or any when `all` is set.
#[derive(Default)]
struct Dirty {
	all: bool,
	claims: BTreeSet<Path>,
}

/// What an identity's presentation and reader view depend on besides its claims:
/// while it stays the same, only the claims marked [`Dirty`] can change.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ctx<N> {
	own: bool,
	/// Routed, starved, or holding a recoverable route.
	live: bool,
	/// A usable route is selected.
	usable: bool,
	/// The certifiers, the selected next hop first, each with whether its route is
	/// held and whether its stream answers the current interest.
	certifiers: Vec<(N, bool, bool)>,
}

/// An identity this node serves itself.
struct Origin<A> {
	seqno: Seqno,
	/// Whether the node declared it, rather than forging it with no claims.
	declared: bool,
	view: View<A>,
}

/// Retained state, summed over the node, for budgets and leak checks.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Retained {
	pub items: usize,
	pub floors: usize,
	pub heard: usize,
	pub sent: usize,
	pub echoes: usize,
	pub pending: usize,
	pub distances: usize,
	pub certified: usize,
}

pub(crate) struct Router<N, A> {
	/// How this node names itself in the hops of a route it forwards. A rotation
	/// leaves it alone, so a route that crossed this node always says so.
	hop: Id,
	neighbors: BTreeMap<N, Peer>,
	origins: BTreeMap<Id, Origin<A>>,
	rx: BTreeMap<Id, BTreeMap<N, Entry>>,
	// Per-identity state is keyed identity first, so one identity's state is
	// one range and a message costs only the identity it names.
	/// For a key this node qualified, the identity the neighbor named.
	filed: BTreeMap<(Id, N), Id>,
	/// Babel's feasibility distance per identity.
	fd: BTreeMap<Id, (Seqno, Metric)>,
	/// The hops of the route this node last sent under a qualified key, kept as long
	/// as its distance: a route that extends them came through this node's own.
	advertised: BTreeMap<Id, Arc<[Id]>>,
	sent: BTreeMap<(Id, N), Reach>,
	/// ROUTE_ENDs sent to each neighbor and not yet echoed back. Until they all
	/// return, a neighbor may still hold this node's advertisement, so the
	/// feasibility distance stays.
	unacked: BTreeMap<(Id, N), u32>,
	pending: BTreeMap<Id, Pending<N>>,
	/// The prefix this node asks its neighbors for announcements under.
	watch: Path,
	/// Counts this node's interest changes.
	interest: u32,
	/// Each origin's claims as this node knows them.
	views: BTreeMap<Id, View<A>>,
	/// After the interest widened, the regions each view is known current in:
	/// the old interest, plus what each batch answering the new one certified.
	/// The rest is hidden until certified.
	current: BTreeMap<Id, Vec<Path>>,
	heard: BTreeMap<(Id, N), Heard<A>>,
	sent_views: BTreeMap<(Id, N), View<A>>,
	reader: Reader<A>,
	/// The claims changed per identity since it was last recomputed.
	dirty: BTreeMap<Id, Dirty>,
	/// Each identity's context as last recomputed: a change marks every claim dirty.
	ctxs: BTreeMap<Id, Ctx<N>>,
	/// Mints a fresh identity on rotation.
	rng: u64,
	/// Identities recomputed, so fixtures can bound the work per message.
	#[cfg(test)]
	touched: usize,
	out: Vec<Out<N, A>>,
}

/// A refused input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
	#[error("zero-cost link")]
	ZeroCost,
	#[error("unknown neighbor")]
	UnknownNeighbor,
	#[error("no declared identity")]
	NoIdentity,
}

impl<N: Key, A: Clone + Eq + fmt::Debug> Router<N, A> {
	/// A node with no sessions, naming itself `hop` in the routes it forwards and
	/// minting rotated identities from `seed`.
	pub fn new(hop: Id, seed: u64) -> Self {
		Self {
			hop,
			neighbors: BTreeMap::new(),
			origins: BTreeMap::new(),
			rx: BTreeMap::new(),
			filed: BTreeMap::new(),
			fd: BTreeMap::new(),
			advertised: BTreeMap::new(),
			sent: BTreeMap::new(),
			unacked: BTreeMap::new(),
			pending: BTreeMap::new(),
			watch: Path::empty(),
			interest: 0,
			views: BTreeMap::new(),
			current: BTreeMap::new(),
			heard: BTreeMap::new(),
			sent_views: BTreeMap::new(),
			reader: Reader::default(),
			dirty: BTreeMap::new(),
			ctxs: BTreeMap::new(),
			rng: seed | 1,
			#[cfg(test)]
			touched: 0,
			out: Vec::new(),
		}
	}

	/// Take the identities recomputed since the last call, whose selection may have
	/// changed, each with the claims whose shown state did.
	pub fn changed(&mut self) -> BTreeMap<Id, BTreeSet<Path>> {
		self.reader.changed()
	}

	/// Take the messages queued for neighbors, in order.
	pub fn drain(&mut self) -> Vec<Out<N, A>> {
		std::mem::take(&mut self.out)
	}

	/// Take the reader events, in order.
	pub fn events(&mut self) -> Vec<Event<A>> {
		self.reader.events()
	}

	// --- Sessions ---

	/// A session to `neighbor` opened.
	pub fn connect(&mut self, neighbor: N, link: Link) -> Result<(), Error> {
		if link.cost == 0 {
			return Err(Error::ZeroCost);
		}
		self.neighbors.insert(
			neighbor,
			Peer {
				link,
				rtt: None,
				draining: false,
				watch: None,
				asked: None,
			},
		);
		// Nothing is sent or learned until a stream opens.
		Ok(())
	}

	/// The sessions to `neighbors` closed: everything learned on them goes, and each
	/// counts as the echo of every ROUTE_END sent on it. Closing them together
	/// recomputes each identity once, rather than once per neighbor.
	pub fn disconnect(&mut self, neighbors: impl IntoIterator<Item = N>) {
		let gone: BTreeSet<N> = (neighbors.into_iter())
			.filter(|neighbor| self.neighbors.remove(neighbor).is_some())
			.collect();
		if gone.is_empty() {
			return;
		}
		let mut ids = BTreeSet::new();
		for (id, routes) in self.rx.iter_mut() {
			let before = routes.len();
			routes.retain(|next, _| !gone.contains(next));
			if routes.len() != before {
				ids.insert(*id);
			}
		}
		self.rx.retain(|_, routes| !routes.is_empty());
		for (id, pending) in self.pending.iter_mut() {
			let before = pending.from.len();
			pending.from.retain(|from| !gone.contains(from));
			if pending.from.len() != before {
				ids.insert(*id);
			}
		}
		let mut on = |(id, n): &(Id, N)| {
			let on = gone.contains(n);
			if on {
				ids.insert(*id);
			}
			!on
		};
		self.filed.retain(|key, _| on(key));
		self.heard.retain(|key, _| on(key));
		self.unacked.retain(|key, _| on(key));
		self.sent.retain(|key, _| on(key));
		self.sent_views.retain(|key, _| on(key));
		self.update(ids);
	}

	/// `neighbor` opened a stream asking for announcements under `prefix`: it
	/// gets a complete snapshot.
	pub fn peer_interest(&mut self, neighbor: N, prefix: Path) -> Result<(), Error> {
		let peer = self.neighbors.get_mut(&neighbor).ok_or(Error::UnknownNeighbor)?;
		peer.watch = Some(prefix);
		self.sent_views.retain(|(_, to), _| *to != neighbor);
		self.update_all();
		Ok(())
	}

	/// This node asked `neighbor` for announcements under `prefix`: the region its
	/// floors certify.
	pub fn asks(&mut self, neighbor: N, prefix: Path) -> Result<(), Error> {
		let peer = self.neighbors.get_mut(&neighbor).ok_or(Error::UnknownNeighbor)?;
		peer.asked = Some(prefix);
		Ok(())
	}

	/// `neighbor` sent GOAWAY: it ranks below every other eligible route while
	/// its session stays up.
	pub fn goaway(&mut self, neighbor: N) {
		if let Some(peer) = self.neighbors.get_mut(&neighbor) {
			peer.draining = true;
		}
		self.update(self.through(neighbor));
	}

	/// The link to `neighbor` has `rtt`, or `None` unmeasured: every route learned
	/// there adds it.
	pub fn rtt(&mut self, neighbor: N, rtt: Option<Duration>) {
		let Some(peer) = self.neighbors.get_mut(&neighbor) else {
			return;
		};
		if peer.rtt == rtt {
			return;
		}
		peer.rtt = rtt;
		self.update(self.through(neighbor));
	}

	/// The identities `neighbor` sent a route for.
	fn through(&self, neighbor: N) -> Vec<Id> {
		(self.rx.iter())
			.filter(|(_, routes)| routes.contains_key(&neighbor))
			.map(|(id, _)| *id)
			.collect()
	}

	#[cfg(test)]
	/// Ask every neighbor for announcements under `prefix` instead, returning
	/// the new interest generation. Each neighbor answers on a new stream, and a
	/// batch from an older one is discarded.
	pub fn watch(&mut self, prefix: Path) -> u32 {
		if !within(&prefix, &self.watch) {
			let old = self.watch.clone();
			for id in self.views.keys().filter(|id| !self.origins.contains_key(id)) {
				self.current.entry(*id).or_insert_with(|| vec![old.clone()]);
			}
		}
		self.current
			.retain(|_, regions| !regions.iter().any(|region| within(&prefix, region)));
		self.watch = prefix;
		self.interest += 1;
		let ids = self.ids();
		for id in &ids {
			self.mark_all(*id);
		}
		self.update(ids);
		self.interest
	}

	/// The current interest generation.
	pub fn interest(&self) -> u32 {
		self.interest
	}

	// --- The origin's own identity ---

	/// Start originating `id` at `seqno`, with no claims.
	pub fn originate(&mut self, id: Id, seqno: Seqno) {
		debug_assert_eq!(id & QUALIFIED, 0, "a declared identity is never qualified");
		let mut view = View::default();
		view.floors.insert(Path::empty(), 0);
		self.origins.insert(
			id,
			Origin {
				seqno,
				declared: true,
				view,
			},
		);
		self.mark_all(id);
		self.update([id]);
	}

	/// Advertise `id` as if serving it, with one uncertified claim: what a forwarder
	/// that lies about an identity does.
	#[cfg(test)]
	pub fn forge(&mut self, id: Id, seqno: Seqno, claim: Path, item: A) {
		let mut view = View::default();
		view.items.insert(claim, item);
		self.origins.insert(
			id,
			Origin {
				seqno,
				declared: false,
				view,
			},
		);
		self.mark_all(id);
		self.update([id]);
	}

	/// The identity this node declares.
	pub fn declared(&self) -> Option<Id> {
		self.origins
			.iter()
			.find(|(_, origin)| origin.declared)
			.map(|(id, _)| *id)
	}

	#[cfg(test)]
	/// The declared identity's seqno.
	pub fn seqno(&self) -> Option<Seqno> {
		self.origins.values().find(|origin| origin.declared).map(|o| o.seqno)
	}

	#[cfg(test)]
	/// The declared identity's claim revision.
	pub fn revision(&self) -> Option<Rev> {
		let origin = self.origins.values().find(|origin| origin.declared)?;
		origin.view.floors.get(&Path::empty()).copied()
	}

	/// Advance the declared identity's seqno, rotating it rather than reach [`MAX`].
	pub fn bump(&mut self) -> Result<(), Error> {
		let id = self.declared().ok_or(Error::NoIdentity)?;
		let seqno = self.origins[&id].seqno;
		if seqno + 1 == MAX {
			self.rotate(id);
		} else {
			self.origins.get_mut(&id).expect("declared").seqno = seqno + 1;
			self.update([id]);
		}
		Ok(())
	}

	/// Start or update a claim of the declared identity.
	pub fn announce(&mut self, claim: Path, item: A) -> Result<(), Error> {
		self.change(claim, Some(item))
	}

	/// Withdraw a claim of the declared identity.
	pub fn unannounce(&mut self, claim: Path) -> Result<(), Error> {
		self.change(claim, None)
	}

	fn change(&mut self, claim: Path, item: Option<A>) -> Result<(), Error> {
		let id = self.declared().ok_or(Error::NoIdentity)?;
		let rev = self.origins[&id].view.floors[&Path::empty()] + 1;
		if rev == MAX {
			self.rotate(id);
			return self.change(claim, item);
		}
		let view = &mut self.origins.get_mut(&id).expect("declared").view;
		view.floors.insert(Path::empty(), rev);
		match item {
			Some(item) => view.items.insert(claim.clone(), item),
			None => view.items.remove(&claim),
		};
		self.mark(id, [claim]);
		self.update([id]);
		Ok(())
	}

	/// Jump the declared identity's revision to `rev`, as a long-lived origin's
	/// claim changes reach it.
	#[cfg(test)]
	pub fn age(&mut self, rev: Rev) {
		let id = self.declared().expect("declared");
		let view = &mut self.origins.get_mut(&id).expect("declared").view;
		view.floors.insert(Path::empty(), rev);
	}

	/// Retire the declared identity for a fresh one, announcing its claims again
	/// there. Every neighbor lets the old one go.
	fn rotate(&mut self, old: Id) {
		let origin = self.origins.remove(&old).expect("declared");
		let id = loop {
			// xorshift64: a fresh identity within a generated hop id's range.
			self.rng ^= self.rng << 13;
			self.rng ^= self.rng >> 7;
			self.rng ^= self.rng << 17;
			let id = self.rng & GENERATED;
			if id != 0 && id != old && !self.origins.contains_key(&id) {
				break id;
			}
		};
		let rev = origin.view.items.len() as Rev;
		let mut view = origin.view;
		view.floors = BTreeMap::from([(Path::empty(), rev)]);
		self.origins.insert(
			id,
			Origin {
				seqno: 0,
				declared: true,
				view,
			},
		);
		// The views heard for it merge from here on.
		self.resync(old);
		self.mark_all(old);
		self.mark_all(id);
		self.update([old, id]);
	}

	// --- Received messages ---

	/// ROUTE_START or ROUTE_UPDATE from `from`, or ROUTE_END for `None`.
	pub fn route(&mut self, from: N, named: Id, reach: Option<Reach>) {
		if !self.neighbors.contains_key(&from) {
			return;
		}
		// Echo every ROUTE_END once applied, filed or refused.
		let ended = reach.is_none();
		if ended {
			self.out.push(Out::Echo { to: from, id: named });
		}
		let Some(id) = self.key(from, named) else {
			return;
		};
		if id != named {
			match reach {
				Some(_) => self.filed.insert((id, from), named),
				None => self.filed.remove(&(id, from)),
			};
		}
		let routes = self.rx.entry(id).or_default();
		// Whether only the route's hops or RTT changed, and not whether this node may
		// use it: that moves no route here.
		let mut same = false;
		match reach {
			Some(Reach::Finite(advert)) => {
				let crossed = advert.hops.contains(&self.hop);
				let full = |advert: &Advert| advert.hops.len() >= MAX_HOPS;
				same = routes.get(&from).is_some_and(|was| {
					let (old, new) = (&was.advert, &advert);
					!was.held
						&& was.crossed == crossed
						&& full(old) == full(new)
						&& (old.seqno, old.metric, old.down_only) == (new.seqno, new.metric, new.down_only)
				});
				let entry = Entry {
					advert,
					held: false,
					crossed,
				};
				routes.insert(from, entry);
			}
			Some(Reach::Held) => {
				if let Some(entry) = routes.get_mut(&from) {
					entry.held = true;
				}
			}
			None => {
				routes.remove(&from);
			}
		}
		if routes.is_empty() {
			self.rx.remove(&id);
		}
		// The sender forgets what it sent for a route it ended, so a restart
		// starts from an empty view: apply the next batch to that, keeping the
		// last view only for what it certified. An uncertified claim lives only
		// while the stream carries it, and the restart's batch names only what it
		// still carries, so one kept here would never be withdrawn.
		if ended && let Some(heard) = self.heard.get_mut(&(id, from)) {
			let raw = std::mem::take(&mut heard.raw);
			let kept = heard.kept.get_or_insert(raw);
			let floors = &kept.floors;
			kept.items.retain(|claim, _| above(floors, claim).next().is_some());
			// What it drops lay outside every floor, so nothing it differed in.
			heard.ended = true;
		}
		self.recompute([id], !same);
	}

	/// A batch of announcement changes for `named` from `from`, closed by its
	/// floor, on the stream answering interest generation `interest`. A floor
	/// certifies the whole region this node asked for at that revision; `None`
	/// certifies nothing, so the stream's claims stay only while it carries them.
	pub fn batch(&mut self, from: N, named: Id, changes: Vec<(Path, Option<A>)>, floor: Option<Rev>, interest: u32) {
		let Some(peer) = self.neighbors.get(&from) else {
			return;
		};
		// A batch from a stream the node has since replaced.
		if interest != self.interest {
			return;
		}
		let Some(id) = self.key(from, named) else {
			return;
		};
		let scope = peer.link.scope.as_ref().map(|scope| scope.regions.clone());
		let region = peer.asked.clone().unwrap_or_else(|| self.watch.clone());
		let floors = floor.map(|rev| BTreeMap::from([(region, rev)])).unwrap_or_default();
		let heard = self.heard.entry((id, from)).or_insert_with(|| Heard::new(interest));
		// What the batch can change in this node's view: the claims it names, and
		// the claims a scope presents at its regions. A new stream, or certified
		// regions that moved, can change any claim.
		let mut all = heard.interest != interest;
		// A new stream, or a restarted route, starts from an empty view.
		let fresh = heard.interest != interest || heard.ended;
		if fresh {
			heard.raw = View::default();
			heard.interest = interest;
			heard.ended = false;
		}
		let mut dirty: Vec<Path> = changes.iter().map(|(claim, _)| claim.clone()).collect();
		let regions = heard.view().floors.keys().cloned().collect::<Vec<_>>();
		heard.raw.apply(changes, floors);
		match &scope {
			Some(scope) => {
				dirty.extend(scope.iter().cloned());
				let Heard { raw, kept, .. } = heard;
				let view = match fresh {
					true => kept.insert(View::default()),
					false => kept.get_or_insert_default(),
				};
				match fresh {
					true => view.represent(raw, scope, raw.items.keys()),
					false => view.represent(raw, scope, &dirty),
				}
			}
			None => heard.kept = None,
		}
		#[cfg(test)]
		if let Some(scope) = &scope {
			assert_eq!(*heard.view(), heard.raw.present_all(scope), "incremental scoped view");
		}
		let moved = !heard.view().floors.keys().eq(regions.iter());
		all |= moved;
		// This node's own identities merge nothing, so they track no differences.
		if !self.origins.contains_key(&id) {
			let merged = self.views.get(&id);
			match fresh || moved {
				true => heard.resync(merged),
				false => {
					for claim in &dirty {
						heard.compare(merged, claim);
					}
				}
			}
		}
		let certifies: Vec<Path> = heard.view().floors.keys().cloned().collect();

		// The batch certifies the regions its floors name.
		for region in &certifies {
			let Some(regions) = self.current.get_mut(&id) else {
				break;
			};
			all = true;
			if *region == self.watch {
				self.current.remove(&id);
			} else if !regions.iter().any(|known| within(region, known)) {
				regions.retain(|known| !within(known, region));
				regions.push(region.clone());
			}
		}
		if !self.origins.contains_key(&id) {
			let merged = self.views.entry(id).or_default();
			let heard = &self.heard[&(id, from)];
			let regions = merged.floors.keys().cloned().collect::<Vec<_>>();
			#[cfg(test)]
			let full = {
				let mut full = merged.clone();
				full.merge(heard.view());
				full
			};
			let changed = merged.merge_claims(heard.view(), &heard.differs);
			#[cfg(test)]
			assert_eq!(*merged, full, "incremental merge");
			all |= !merged.floors.keys().eq(regions.iter());
			// The batch certifies the interest, so what lies outside can go. While every
			// floor lies within the interest, nothing does.
			let outside = !merged.floors.keys().all(|region| within(region, &self.watch));
			#[cfg(test)]
			if !outside {
				assert_eq!(merged.retire(&self.watch), *merged, "nothing to retire");
			}
			let retired = outside && {
				let kept = merged.retire(&self.watch);
				let retired = kept != *merged;
				*merged = kept;
				retired
			};
			if retired {
				all = true;
				let watch = BTreeSet::from([self.watch.clone()]);
				self.forgot_outside(id, &watch);
				self.resync(id);
			} else {
				self.compare(id, &changed);
			}
			dirty.extend(changed);
		}
		match all {
			true => self.mark_all(id),
			false => self.mark(id, dirty),
		}
		self.update([id]);
	}

	/// ROUTE_REQUEST from `from` for `id`, as this node advertised it.
	pub fn request(&mut self, from: N, id: Id, seqno: Seqno) {
		if !self.neighbors.contains_key(&from) {
			return;
		}
		if let Some(origin) = self.origins.get_mut(&id) {
			// Only the origin advances its seqno to answer.
			if seqno == MAX {
				if origin.declared {
					self.rotate(id);
				}
			} else {
				origin.seqno = origin.seqno.max(seqno);
				self.update([id]);
			}
			return;
		}
		if self.selected(id).is_some_and(|route| route.seqno >= seqno) {
			// Already advertised to the requester.
			return;
		}
		let pending = self.pending.entry(id).or_insert(Pending {
			seqno,
			from: BTreeSet::new(),
			asked: None,
		});
		if seqno > pending.seqno {
			pending.seqno = seqno;
			pending.asked = None;
		}
		pending.from.insert(from);
		self.update([id]);
	}

	/// The echo of a ROUTE_END this node sent `from` for `id`.
	pub fn echo(&mut self, from: N, id: Id) {
		let Some(left) = self.unacked.get_mut(&(id, from)) else {
			// Unsolicited: nothing of ours to drain.
			return;
		};
		*left -= 1;
		if *left == 0 {
			self.unacked.remove(&(id, from));
		}
		self.update([id]);
	}

	/// Forget `id`'s routes and announcements. The feasibility distance goes on
	/// its own once drained.
	pub fn collect(&mut self, id: Id) {
		self.rx.remove(&id);
		let filed: Vec<_> = Self::of(&self.filed, id).map(|(key, _)| *key).collect();
		for key in filed {
			self.filed.remove(&key);
		}
		self.views.remove(&id);
		self.current.remove(&id);
		let heard: Vec<_> = Self::of(&self.heard, id).map(|(key, _)| *key).collect();
		for key in heard {
			self.heard.remove(&key);
		}
		self.pending.remove(&id);
		self.forgot_outside(id, &BTreeSet::new());
		self.mark_all(id);
		self.update([id]);
	}

	// --- Queries ---

	/// The route this node selected for `id`.
	pub fn selected(&self, id: Id) -> Option<Selected<N>> {
		if let Some(origin) = self.origins.get(&id) {
			return Some(Selected {
				next: None,
				seqno: origin.seqno,
				metric: 0,
				down_only: false,
			});
		}
		type Rank<N> = (bool, Reverse<i8>, Metric, N);
		let mut best: Option<(Rank<N>, Selected<N>)> = None;
		for (&next, entry) in self.rx.get(&id)? {
			if entry.held || !self.eligible(id, entry) {
				continue;
			}
			let peer = &self.neighbors[&next];
			let Some(metric) = self.metric(next, entry) else {
				continue;
			};
			let rank = (peer.draining, Reverse(peer.link.priority), metric, next);
			if best.as_ref().is_some_and(|(best, _)| *best <= rank) {
				continue;
			}
			let selected = Selected {
				next: Some(next),
				seqno: entry.advert.seqno,
				metric,
				down_only: peer.link.upstream || entry.advert.down_only,
			};
			best = Some((rank, selected));
		}
		best.map(|(_, selected)| selected)
	}

	/// Every usable feasible route to `id`, with this node's metric and RTT through
	/// each. Any of them forwards without a loop, if it [carries](Self::carries) the
	/// claim.
	pub fn routes(&self, id: Id) -> Vec<(N, Metric, Option<Duration>)> {
		if self.origins.contains_key(&id) {
			return Vec::new();
		}
		(self.rx.get(&id).into_iter().flatten())
			.filter(|(_, entry)| !entry.held && self.eligible(id, entry))
			.filter_map(|(&next, entry)| {
				let rtt = through_rtt(&entry.advert, &self.neighbors[&next]);
				Some((next, self.metric(next, entry)?, rtt))
			})
			.collect()
	}

	/// This node's RTT to `id` along `route`: 0 at the identity's own node.
	fn route_rtt(&self, id: Id, route: &Selected<N>) -> Option<Duration> {
		let Some(next) = route.next else {
			return Some(Duration::ZERO);
		};
		let entry = self.rx.get(&id)?.get(&next)?;
		through_rtt(&entry.advert, &self.neighbors[&next])
	}

	/// Whether a request for a claim at `claim` may go through `next`.
	pub fn carries(&self, next: N, claim: &Path) -> bool {
		self.neighbors.get(&next).is_some_and(|peer| peer.carries(claim))
	}

	/// The prefix this node asked `next` for, outside which it carries no claim: the
	/// root when it asked for none.
	pub fn asked(&self, next: N) -> Path {
		(self.neighbors.get(&next))
			.and_then(|peer| peer.asked.clone())
			.unwrap_or_default()
	}

	#[cfg(test)]
	/// The route this node would offer an upstream link for `id`.
	pub fn export(&self, id: Id) -> Option<Selected<N>> {
		self.selected(id).filter(|route| !route.down_only)
	}

	#[cfg(test)]
	/// Whether this node holds a reachable route to `id` but none is feasible.
	pub fn starved(&self, id: Id) -> bool {
		self.selected(id).is_none() && self.infeasible(id).next().is_some()
	}

	#[cfg(test)]
	/// Whether this node, with no usable route to `id`, holds a held route it
	/// could recover through.
	pub fn grounded(&self, id: Id) -> bool {
		self.selected(id).is_none() && self.recoverable(id)
	}

	/// Whether this node holds any route state for `id`.
	pub fn holds(&self, id: Id) -> bool {
		self.rx.contains_key(&id)
	}

	#[cfg(test)]
	/// Whether this node keeps a feasibility distance for `id`.
	pub fn remembers(&self, id: Id) -> bool {
		self.fd.contains_key(&id)
	}

	#[cfg(test)]
	/// Whether this node keeps no state at all for `id`, beyond the changes
	/// [`Self::changed`] has yet to take.
	pub fn keeps_nothing(&self, id: Id) -> bool {
		fn none<K: Ord + Copy, V, N: Ord>(map: &BTreeMap<(K, N), V>, id: K) -> bool {
			!map.keys().any(|(key, _)| *key == id)
		}
		!self.origins.contains_key(&id)
			&& !self.rx.contains_key(&id)
			&& !self.fd.contains_key(&id)
			&& !self.advertised.contains_key(&id)
			&& !self.pending.contains_key(&id)
			&& !self.views.contains_key(&id)
			&& !self.current.contains_key(&id)
			&& !self.dirty.contains_key(&id)
			&& !self.ctxs.contains_key(&id)
			&& none(&self.filed, id)
			&& none(&self.sent, id)
			&& none(&self.unacked, id)
			&& none(&self.heard, id)
			&& none(&self.sent_views, id)
			&& self.reader.claims(id).next().is_none()
	}

	#[cfg(test)]
	/// How many identities this node holds routes to.
	pub fn identities(&self) -> usize {
		self.rx.len()
	}

	#[cfg(test)]
	/// How many qualified keys this node remembers the forwarded name of.
	pub fn aliases(&self) -> usize {
		self.filed.len()
	}

	#[cfg(test)]
	/// How many region floors this node keeps for `id`.
	pub fn floors(&self, id: Id) -> usize {
		self.views.get(&id).map_or(0, |view| view.floors.len())
	}

	/// The view this node keeps for `id`, if any.
	pub fn view(&self, id: Id) -> Option<&View<A>> {
		self.origins
			.get(&id)
			.map(|origin| &origin.view)
			.or_else(|| self.views.get(&id))
	}

	/// Every other origin's view this node keeps.
	#[cfg(test)]
	pub fn views(&self) -> impl Iterator<Item = (Id, &View<A>)> {
		self.views
			.iter()
			.filter(|(id, _)| !self.origins.contains_key(id))
			.map(|(id, view)| (*id, view))
	}

	/// How many identities this node has recomputed.
	#[cfg(test)]
	pub fn touched(&self) -> usize {
		self.touched
	}

	/// What this node's reader sees.
	pub fn shown(&self) -> &BTreeMap<(Id, Path), Shown<A>> {
		self.reader.shown()
	}

	#[cfg(test)]
	/// Whether this node shows `claim` from `id`, and whether it is available.
	pub fn shows(&self, id: Id, claim: &Path) -> Option<bool> {
		self.reader.get(id, claim).map(|shown| shown.available)
	}

	#[cfg(test)]
	pub fn retained(&self) -> Retained {
		let mut out = Retained::default();
		for (id, view) in &self.views {
			if !self.origins.contains_key(id) {
				out.items += view.items.len();
				out.floors += view.floors.len();
			}
		}
		out.heard = self.heard.values().map(|heard| heard.raw.items.len()).sum();
		out.sent = self.sent_views.values().map(|view| view.items.len()).sum();
		out.echoes = self.unacked.values().map(|&count| count as usize).sum();
		out.pending = self.pending.len();
		out.distances = self.fd.keys().filter(|id| !self.origins.contains_key(id)).count();
		out.certified = self.current.values().map(Vec::len).sum();
		out
	}

	// --- Internals ---

	/// The key this node files `named` under when `from` sends it, or `None`
	/// when refused: a scoped neighbor's identities are all qualified, and one
	/// already qualified is refused rather than qualified again.
	fn key(&self, from: N, named: Id) -> Option<Id> {
		match &self.neighbors.get(&from)?.link.scope {
			Some(scope) => {
				if named & QUALIFIED != 0 {
					return None;
				}
				let key = (scope.qualify)(named);
				debug_assert_ne!(key & QUALIFIED, 0, "a qualified key sets QUALIFIED");
				Some(key | QUALIFIED)
			}
			None => Some(named),
		}
	}

	/// The identity this node names for `id` in a request to `next`: the one
	/// `next` advertised, if this node qualified it.
	pub fn name(&self, next: N, id: Id) -> Id {
		self.filed.get(&(id, next)).copied().unwrap_or(id)
	}

	/// Babel's feasibility condition against this node's feasibility distance.
	fn feasible(&self, id: Id, advert: &Advert) -> bool {
		match self.fd.get(&id) {
			None => true,
			Some(&(seqno, metric)) => advert.seqno > seqno || (advert.seqno == seqno && advert.metric < metric),
		}
	}

	/// Whether this node may select or certify through `entry`: a feasible route that
	/// did not cross it. One that crossed it and is feasible all the same is no route
	/// at all, since no seqno would change where it has been.
	fn eligible(&self, id: Id, entry: &Entry) -> bool {
		!entry.crossed && self.feasible(id, &entry.advert)
	}

	/// This node's metric through `entry`, the route `next` sent, or `None` when it is
	/// unreachable, never saturated: the metric overflows, or the route already
	/// crossed as many nodes as one may.
	fn metric(&self, next: N, entry: &Entry) -> Option<Metric> {
		if entry.advert.hops.len() >= MAX_HOPS {
			return None;
		}
		entry.advert.metric.checked_add(self.neighbors[&next].link.cost)
	}

	/// The usable but infeasible routes for `id`, with their metrics: this node holds
	/// the identity while it has one. One that crossed this node counts like any other:
	/// its sender routes through this node, and moves once it hears this node has no
	/// route. Under a qualified key that holds only if the route extends the one this
	/// node last sent under the key. Otherwise a relay past this node derived the key
	/// from this node's route to another identity, which stands whatever happens to
	/// this one, and waiting on it would never end.
	fn infeasible(&self, id: Id) -> impl Iterator<Item = (Metric, N)> + '_ {
		let sent = self.advertised.get(&id);
		let derived = move |entry: &Entry| match id & QUALIFIED {
			0 => true,
			_ => sent.is_some_and(|sent| entry.advert.hops.starts_with(sent)),
		};
		(self.rx.get(&id).into_iter().flatten())
			.filter(move |(_, entry)| !entry.held && !self.feasible(id, &entry.advert))
			.filter(move |(_, entry)| !entry.crossed || derived(entry))
			.filter_map(move |(&next, entry)| Some((self.metric(next, entry)?, next)))
	}

	/// Where this node sends a request for `id`: its feasible route's next hop,
	/// else its best infeasible route's, even when that neighbor asked.
	fn target(&self, id: Id) -> Option<N> {
		self.selected(id)
			.and_then(|route| route.next)
			.or_else(|| self.infeasible(id).min().map(|(_, next)| next))
	}

	/// Whether this node holds a held route to `id` it could recover through.
	/// One counts only if its last finite advertisement is feasible and
	/// reachable here, so
	/// holds follow a strictly decreasing metric toward a starved node and
	/// cannot hold each other up.
	fn recoverable(&self, id: Id) -> bool {
		self.rx.get(&id).is_some_and(|routes| {
			routes
				.iter()
				.any(|(&next, entry)| entry.held && self.reachable(id, next, entry))
		})
	}

	/// Whether this node, having selected `route` for `id`, keeps showing its
	/// announcements: routed, starved, or holding a recoverable route.
	fn live(&self, id: Id, route: Option<Selected<N>>) -> bool {
		route.is_some() || self.infeasible(id).next().is_some() || self.recoverable(id)
	}

	/// The neighbors whose views can certify `id`: those whose last finite
	/// advertisement is feasible and reachable here, held or not. Feasible edges strictly
	/// decrease the metric toward the origin, so certification never circulates.
	fn certifiers(&self, id: Id) -> Vec<N> {
		self.rx.get(&id).map_or_else(Vec::new, |routes| {
			routes
				.iter()
				.filter(|&(&next, entry)| self.reachable(id, next, entry))
				.map(|(&next, _)| next)
				.collect()
		})
	}

	/// Whether `entry`'s last finite advertisement from `next` is feasible here
	/// and reachable across the link, as selection requires of a usable route.
	fn reachable(&self, id: Id, next: N, entry: &Entry) -> bool {
		self.eligible(id, entry) && self.metric(next, entry).is_some()
	}

	/// The regions of `id` that this node's certifiers certify, those whose
	/// route is held only when `held`, or `None` while it holds no route or a
	/// certifier has not yet answered the current interest.
	fn certified(&self, id: Id, held: bool) -> Option<BTreeSet<Path>> {
		let routes = self.rx.get(&id)?;
		let mut regions = BTreeSet::new();
		for next in self.certifiers(id) {
			let heard = self
				.heard
				.get(&(id, next))
				.filter(|heard| heard.interest == self.interest)?;
			if held || !routes[&next].held {
				regions.extend(heard.view().floors.keys().cloned());
			}
		}
		Some(regions)
	}

	/// Let go of every region of `id`'s merged view that no certifier certifies
	/// any more, once each has answered the current interest.
	fn prune(&mut self, id: Id) {
		// A starved or held node keeps presence until it recovers or lets go.
		if self.origins.contains_key(&id) || self.selected(id).is_none() {
			return;
		}
		let Some(regions) = self.certified(id, true) else {
			return;
		};
		let Some(merged) = self.views.get(&id) else {
			return;
		};
		let lost = (merged.floors.iter())
			.filter(|(floor, _)| !within_any(floor, &regions))
			.map(|(_, &rev)| rev)
			.max();
		let Some(lost) = lost else {
			return;
		};
		// Keep what the view knows within the regions still certified. That is
		// sound only if no lost floor is newer than every kept one: otherwise a
		// claim above both was last learned through the lost floor, and is
		// unknown at the revision the kept floors certify.
		let kept = merged.restrict_all(&regions);
		let (rebuilt, remembered) = if kept.floors.values().all(|&floor| floor >= lost) {
			(kept, regions)
		} else {
			let mut rebuilt = View::default();
			for next in self.certifiers(id) {
				rebuilt.merge(self.heard[&(id, next)].view());
			}
			(rebuilt, BTreeSet::new())
		};
		self.views.insert(id, rebuilt);
		self.resync(id);
		self.forgot_outside(id, &remembered);
		self.mark_all(id);
	}

	/// Compare `claims` of every view heard for `id` against the merged view, once
	/// they changed there.
	fn compare(&mut self, id: Id, claims: &[Path]) {
		let merged = self.views.get(&id);
		for (_, heard) in self.heard.range_mut((id, N::MIN)..=(id, N::MAX)) {
			for claim in claims {
				heard.compare(merged, claim);
			}
		}
	}

	/// Compare every view heard for `id` against the merged view, once it was replaced.
	fn resync(&mut self, id: Id) {
		let merged = self.views.get(&id);
		for (_, heard) in self.heard.range_mut((id, N::MIN)..=(id, N::MAX)) {
			heard.resync(merged);
		}
	}

	/// Count a forget against each shown claim of `id` overlapping none of `kept`.
	fn forgot_outside(&mut self, id: Id, kept: &BTreeSet<Path>) {
		self.reader.forget(id, |claim| within_any(claim, kept));
	}

	/// The view this node shows and forwards for `id`, having selected `route`: what
	/// it knows certified, and each claim a certifier carries uncertified where no
	/// floor it knows covers the claim.
	fn adopted(&self, id: Id, route: Option<Selected<N>>) -> Option<Cow<'_, View<A>>> {
		if let Some(origin) = self.origins.get(&id) {
			return Some(Cow::Borrowed(&origin.view));
		}
		if !self.live(id, route) {
			return None;
		}
		let merged = self.views.get(&id);
		let mut loose = self.loose(id, route, merged).peekable();
		if loose.peek().is_none() {
			return merged.map(Cow::Borrowed);
		}
		let mut view = merged.cloned().unwrap_or_default();
		for (claim, item, _) in loose {
			view.items.entry(claim.clone()).or_insert_with(|| item.clone());
		}
		Some(Cow::Owned(view))
	}

	/// The uncertified claims of `id` its certifiers carry on streams answering the
	/// current interest where `merged` certifies nothing, the selected next hop's
	/// first, and whether the carrier is held.
	fn loose<'a>(
		&'a self,
		id: Id,
		route: Option<Selected<N>>,
		merged: Option<&'a View<A>>,
	) -> impl Iterator<Item = (&'a Path, &'a A, bool)> + 'a {
		let mut certifiers = self.certifiers(id);
		if let Some(next) = route.and_then(|route| route.next) {
			certifiers.sort_by_key(|&n| n != next);
		}
		let routes = self.rx.get(&id);
		certifiers.into_iter().flat_map(move |next| {
			let held = routes.is_some_and(|routes| routes[&next].held);
			self.heard
				.get(&(id, next))
				.filter(|heard| heard.interest == self.interest)
				.into_iter()
				.flat_map(|heard| heard.view().uncertified())
				.filter(move |(claim, _)| merged.is_none_or(|merged| merged.as_of(claim).is_none()))
				.map(move |(claim, item)| (claim, item, held))
		})
	}

	/// Whether this node sends `id` to `neighbor` at all: its own identities to
	/// everyone, learned ones only to a proxied neighbor.
	fn serves(&self, neighbor: &Peer, id: Id) -> bool {
		neighbor.link.proxy || self.origins.contains_key(&id)
	}

	/// One identity's entries in a map keyed identity first.
	fn of<V>(map: &BTreeMap<(Id, N), V>, id: Id) -> impl Iterator<Item = (&(Id, N), &V)> {
		map.range((id, N::MIN)..=(id, N::MAX))
	}

	/// Every identity this node keeps any state for.
	fn ids(&self) -> BTreeSet<Id> {
		let mut ids: BTreeSet<Id> = self.rx.keys().copied().collect();
		ids.extend(self.origins.keys());
		ids.extend(self.fd.keys());
		ids.extend(self.views.keys());
		ids.extend(self.pending.keys());
		ids.extend(self.sent.keys().map(|(id, _)| *id));
		ids.extend(self.unacked.keys().map(|(id, _)| *id));
		ids.extend(self.heard.keys().map(|(id, _)| *id));
		ids.extend(self.reader.shown().keys().map(|(id, _)| *id));
		ids
	}

	/// Recompute every identity: for a change that touches them all, such as a
	/// new interest.
	fn update_all(&mut self) {
		let ids = self.ids();
		self.update(ids);
	}

	/// Mark `claims` of `id` changed.
	fn mark(&mut self, id: Id, claims: impl IntoIterator<Item = Path>) {
		let dirty = self.dirty.entry(id).or_default();
		if !dirty.all {
			dirty.claims.extend(claims);
		}
	}

	/// Mark every claim of `id` changed.
	fn mark_all(&mut self, id: Id) {
		*self.dirty.entry(id).or_default() = Dirty {
			all: true,
			claims: BTreeSet::new(),
		};
	}

	/// Recompute each of `ids`: its selection, the feasibility distance for what
	/// this node advertises, what each neighbor is sent, its seqno request, and
	/// what the reader sees.
	fn update(&mut self, ids: impl IntoIterator<Item = Id>) {
		self.recompute(ids, true);
	}

	/// [`Self::update`], letting go of what no neighbor certifies only when `prune`. A
	/// neighbor's change of hops or RTT alone moves no certifier, so it lets go of
	/// nothing: only a message that could have cost a region its certifier does.
	fn recompute(&mut self, ids: impl IntoIterator<Item = Id>, prune: bool) {
		let peers: Vec<N> = self
			.neighbors
			.iter()
			.filter(|(_, peer)| peer.watch.is_some())
			.map(|(n, _)| *n)
			.collect();
		let ids: BTreeSet<Id> = ids.into_iter().collect();
		#[cfg(test)]
		{
			self.touched += ids.len();
		}
		for &id in &ids {
			self.reader.touch(id);
		}
		for id in ids {
			if prune {
				self.prune(id);
			}
			self.record(id, &peers);
			let ctx = self.ctx(id, self.selected(id));
			let mut dirty = self.dirty.remove(&id).unwrap_or_default();
			dirty.all |= self.ctxs.get(&id) != Some(&ctx);
			self.advertise(id, &peers, &ctx, &dirty);
			// Drop a feasibility distance once nothing here routes or advertises
			// the identity and every neighbor echoed its ROUTE_END: no node then
			// holds an advertisement from this one, so no older one can return as
			// a loop.
			if !self.rx.contains_key(&id)
				&& !self.origins.contains_key(&id)
				&& Self::of(&self.sent, id).next().is_none()
				&& Self::of(&self.unacked, id).next().is_none()
			{
				self.fd.remove(&id);
				self.advertised.remove(&id);
			}
			self.requests(id);
			self.show(id, &ctx, &dirty);
			// Nothing to present or show is left to compare against.
			let empty = !ctx.own && !ctx.live && !self.views.contains_key(&id);
			match empty {
				true => self.ctxs.remove(&id),
				false => self.ctxs.insert(id, ctx),
			};
		}
	}

	/// The context `id`'s claims are presented and shown in, having selected `route`.
	fn ctx(&self, id: Id, route: Option<Selected<N>>) -> Ctx<N> {
		let mut certifiers = self.certifiers(id);
		if let Some(next) = route.and_then(|route| route.next) {
			certifiers.sort_by_key(|&n| n != next);
		}
		let routes = self.rx.get(&id);
		let certifiers = (certifiers.into_iter())
			.map(|next| {
				let held = routes.is_some_and(|routes| routes[&next].held);
				let answers = (self.heard.get(&(id, next))).is_some_and(|heard| heard.interest == self.interest);
				(next, held, answers)
			})
			.collect();
		Ctx {
			own: self.origins.contains_key(&id),
			live: self.live(id, route),
			usable: route.is_some(),
			certifiers,
		}
	}

	/// The floors of the view this node shows and forwards for `id`.
	fn adopted_floors(&self, id: Id) -> Option<&BTreeMap<Path, Rev>> {
		self.view(id).map(|view| &view.floors)
	}

	/// The claim at `claim` in the view this node shows and forwards for `id`, as
	/// [`Self::adopted`] has it, without building the view.
	fn adopted_at<'a>(&'a self, id: Id, ctx: &Ctx<N>, claim: &Path) -> Option<&'a A> {
		if let Some(origin) = self.origins.get(&id) {
			return origin.view.items.get(claim);
		}
		if !ctx.live {
			return None;
		}
		let merged = self.views.get(&id);
		if let Some(item) = merged.and_then(|merged| merged.items.get(claim)) {
			return Some(item);
		}
		self.loose_at(id, ctx, merged, claim).map(|(item, _)| item)
	}

	/// The uncertified claim at `claim` its certifiers carry where `merged` certifies
	/// nothing, as [`Self::loose`] finds it first, and whether a carrier is not held.
	fn loose_at<'a>(&'a self, id: Id, ctx: &Ctx<N>, merged: Option<&View<A>>, claim: &Path) -> Option<(&'a A, bool)> {
		if merged.is_some_and(|merged| merged.as_of(claim).is_some()) {
			return None;
		}
		let mut found: Option<(&A, bool)> = None;
		for &(next, held, answers) in &ctx.certifiers {
			if !answers {
				continue;
			}
			let view = self.heard[&(id, next)].view();
			if let Some(item) = view.items.get(claim)
				&& view.as_of(claim).is_none()
			{
				let found = found.get_or_insert((item, false));
				found.1 |= !held;
			}
		}
		found
	}

	/// Whether `id` meets the interest `watch`: a stream answering it is presented a
	/// claim of the view this node forwards, inside the interest or covering it.
	fn meets(&self, id: Id, ctx: &Ctx<N>, watch: &Path) -> bool {
		let Some(floors) = self.adopted_floors(id).or(ctx.live.then_some(&EMPTY_FLOORS)) else {
			return false;
		};
		if covering(floors, watch, |claim| self.adopted_at(id, ctx, claim)).is_some() {
			return true;
		}
		if let Some(origin) = self.origins.get(&id) {
			return below(&origin.view.items, watch).next().is_some();
		}
		if !ctx.live {
			return false;
		}
		let merged = self.views.get(&id);
		if merged.is_some_and(|merged| below(&merged.items, watch).next().is_some()) {
			return true;
		}
		ctx.certifiers
			.iter()
			.filter(|&&(_, _, answers)| answers)
			.any(|&(next, _, _)| {
				let view = self.heard[&(id, next)].view();
				// A floor over the whole interest certifies everything below it.
				!view.floors.keys().any(|region| within(watch, region))
					&& below(&view.items, watch).any(|(claim, _)| {
						view.as_of(claim).is_none() && merged.is_none_or(|merged| merged.as_of(claim).is_none())
					})
			})
	}

	/// Record the feasibility distance for the route this node selected for `id`, if
	/// it sends that route to any neighbor.
	fn record(&mut self, id: Id, peers: &[N]) {
		let Some(selected) = self.selected(id) else {
			return;
		};
		let ctx = self.ctx(id, Some(selected));
		// As `advertise` decides, including that a route ends on a stream until its end is echoed.
		let sends = peers.iter().any(|to| {
			let peer = &self.neighbors[to];
			self.serves(peer, id)
				&& !(peer.link.upstream && selected.down_only)
				&& (self.sent.contains_key(&(id, *to)) || !self.unacked.contains_key(&(id, *to)))
				&& self.meets(id, &ctx, peer.watch.as_ref().expect("a watching peer"))
		});
		if !sends {
			return;
		}
		// Babel's feasibility distance: reset by a newer seqno, lowered by a better
		// metric. It tracks finite updates actually sent (RFC 8966 s3.7.3).
		let fd = self.fd.entry(id).or_insert((selected.seqno, selected.metric));
		if selected.seqno > fd.0 || (selected.seqno == fd.0 && selected.metric < fd.1) {
			*fd = (selected.seqno, selected.metric);
		}
	}

	/// Queue any change to what each watching neighbor is sent for `id`, in `ctx`,
	/// with `dirty` claims changed since the last time.
	///
	/// A route goes to a stream only while it meets the stream's interest: it starts
	/// before its first claim there, and once the last one goes, the batch whose floor
	/// certifies that withdrawal precedes ROUTE_END. Each stream is sent only what
	/// changed among the dirty claims, unless any could have.
	fn advertise(&mut self, id: Id, peers: &[N], ctx: &Ctx<N>, dirty: &Dirty) {
		let selected = self.selected(id);
		// The selected route's hops, then this node: its own route starts with it.
		let through = selected.and_then(|route| route.next);
		let hops: Arc<[Id]> = (through.iter())
			.flat_map(|next| self.rx[&id][next].advert.hops.iter().copied())
			.chain([self.hop])
			.collect();
		let floors = self.adopted_floors(id).cloned().unwrap_or_default();
		// Built only for a stream sent a whole snapshot, once per interest.
		let mut adopted: Option<View<A>> = None;
		let mut presented: Vec<(Path, View<A>)> = Vec::new();
		let mut plans = Vec::new();

		for &to in peers {
			let peer = &self.neighbors[&to];
			let serves = self.serves(peer, id);
			let started = self.sent.contains_key(&(id, to));
			if !serves && !started {
				continue;
			}
			let reach = match selected {
				Some(route) if serves && !(peer.link.upstream && route.down_only) => Some(Reach::Finite(Advert {
					seqno: route.seqno,
					metric: route.metric,
					down_only: route.down_only,
					rtt: self.route_rtt(id, &route),
					hops: hops.clone(),
				})),
				None if serves && ctx.live && started => Some(Reach::Held),
				_ => None,
			};
			// A route starts again only once the neighbor echoed its last end there, so
			// one that withholds echoes holds at most one end per identity.
			let reach = reach.filter(|_| started || !self.unacked.contains_key(&(id, to)));
			let watch = peer.watch.as_ref().expect("a watching peer");
			let sent = self.sent_views.get(&(id, to));
			let (changes, meets) = match (&reach, sent) {
				(None, _) => (Vec::new(), false),
				// A stream holding nothing that the route still does not meet.
				(Some(_), None) if !self.meets(id, ctx, watch) => (Vec::new(), false),
				(Some(_), Some(sent)) if !dirty.all => {
					let mut held = sent.items.len();
					let mut changes = Vec::new();
					let paths = (dirty.claims.iter()).filter(|claim| within(claim, watch) && *claim != watch);
					for claim in paths {
						let now = self.adopted_at(id, ctx, claim);
						changes.push((claim, now));
					}
					changes.push((watch, covering(&floors, watch, |claim| self.adopted_at(id, ctx, claim))));
					changes.retain(|(claim, now)| sent.items.get(*claim) != *now);
					for (claim, now) in &changes {
						match (sent.items.contains_key(*claim), now.is_some()) {
							(false, true) => held += 1,
							(true, false) => held -= 1,
							_ => {}
						}
					}
					// Starts and changes, then ends, as a whole snapshot's diff orders them.
					changes.sort_by_key(|(_, now)| now.is_none());
					let changes = (changes.into_iter())
						.map(|(claim, now)| (claim.clone(), now.cloned()))
						.collect();
					(changes, held > 0)
				}
				(Some(_), sent) => {
					let index = match presented.iter().position(|(interest, _)| interest == watch) {
						Some(index) => index,
						None => {
							let adopted = adopted.get_or_insert_with(|| {
								self.adopted(id, selected).map(Cow::into_owned).unwrap_or_default()
							});
							presented.push((watch.clone(), adopted.present(watch)));
							presented.len() - 1
						}
					};
					let view = &presented[index].1;
					let changes = view.diff(sent.unwrap_or(&View::default()));
					(changes, !view.items.is_empty())
				}
			};
			let floors = narrow(&floors, watch);
			#[cfg(test)]
			if reach.is_some() {
				let adopted = self.adopted(id, selected).map(Cow::into_owned).unwrap_or_default();
				let mut check = sent.cloned().unwrap_or_default();
				check.apply(changes.clone(), floors.clone());
				assert_eq!(check, adopted.present(watch), "incremental presentation to {to:?}");
			}
			plans.push((to, started, reach, meets, changes, floors, watch.clone()));
		}

		for (to, started, reach, meets, changes, floors, watch) in plans {
			let key = (id, to);
			let reaching = reach.is_some();
			let send = reach.filter(|_| meets);
			let sending = send.is_some();
			if let Some(send) = send
				&& self.sent.get(&key) != Some(&send)
			{
				if id & QUALIFIED != 0
					&& let Reach::Finite(advert) = &send
				{
					self.advertised.insert(id, advert.hops.clone());
				}
				self.sent.insert(key, send.clone());
				self.out.push(Out::Route {
					to,
					id,
					reach: Some(send),
				});
			}
			// A batch goes while the route does, the last one just before its ROUTE_END.
			if sending || reaching && started && self.sent_views.contains_key(&key) {
				let sent = self.sent_views.entry(key).or_default();
				let floor = sent.floor(&watch);
				sent.apply(changes.clone(), floors);
				let now = sent.floor(&watch);
				if !changes.is_empty() || now != floor {
					self.out.push(Out::Batch {
						to,
						id,
						changes,
						floor: now,
					});
				}
			}
			// Announcements ride on the route and end with it.
			if !sending {
				if started {
					*self.unacked.entry(key).or_default() += 1;
					self.sent.remove(&key);
					self.out.push(Out::Route { to, id, reach: None });
				}
				self.sent_views.remove(&key);
			}
		}
	}

	/// Start, resend, or clear this node's seqno request for `id`.
	fn requests(&mut self, id: Id) {
		if self.origins.contains_key(&id) {
			self.pending.remove(&id);
			return;
		}
		let route = self.selected(id);
		let starved = route.is_none() && self.infeasible(id).next().is_some();
		// Nothing follows the last seqno: its origin rotates to a new identity instead, so
		// there is nothing to ask for.
		if starved && let Some(want) = self.fd[&id].0.checked_add(1) {
			let pending = self.pending.entry(id).or_insert(Pending {
				seqno: want,
				from: BTreeSet::new(),
				asked: None,
			});
			if pending.seqno < want {
				pending.seqno = want;
				pending.asked = None;
			}
		}
		let Some(pending) = self.pending.get(&id) else {
			return;
		};
		let answered = route.is_some_and(|route| route.seqno >= pending.seqno);
		if answered || (pending.from.is_empty() && !starved) {
			self.pending.remove(&id);
			return;
		}
		// A request follows the next hop even back through a requester: next hops
		// form no loop, one pending request per hop stops a bounce, and no
		// requester ever cancels, so excluding one can strand a request while every
		// route still runs through it.
		let Some(target) = self.target(id) else {
			self.pending.remove(&id);
			return;
		};
		let pending = self.pending.get_mut(&id).expect("pending");
		if pending.asked != Some(target) {
			pending.asked = Some(target);
			let seqno = pending.seqno;
			let named = self.name(target, id);
			self.out.push(Out::Request {
				to: target,
				id: named,
				seqno,
			});
		}
	}

	/// Assert an incremental reader view, `now` over the dirty claims, matches a full recompute.
	#[cfg(test)]
	fn check_shown(&self, id: Id, ctx: &Ctx<N>, dirty: &Dirty, now: &BTreeMap<Path, Option<Shown<A>>>) {
		if dirty.all {
			return;
		}
		let route = self.selected(id);
		let mut all: BTreeSet<Path> = self.reader.claims(id).cloned().collect();
		if let Some(view) = self.adopted(id, route) {
			all.extend(view.items.keys().cloned());
		}
		self.reader.check(id, now, self.shown_now(id, ctx, all.iter()));
	}

	/// Recompute what this node's reader sees of `id` in `ctx` and log each change:
	/// only the `dirty` claims, unless any could have changed.
	fn show(&mut self, id: Id, ctx: &Ctx<N>, dirty: &Dirty) {
		let claims: BTreeSet<Path> = match dirty.all {
			true => {
				let mut claims: BTreeSet<Path> = self.reader.claims(id).cloned().collect();
				if let Some(view) = self.adopted(id, self.selected(id)) {
					claims.extend(view.items.keys().cloned());
				}
				claims
			}
			false => dirty.claims.clone(),
		};
		let now = self.shown_now(id, ctx, claims.iter());
		#[cfg(test)]
		self.check_shown(id, ctx, dirty, &now);

		// A claim no routed neighbor's region covers was let go, not ended, and so
		// was one the view never certified: only its stream left.
		let regions = self.certified(id, true);
		let views = &self.views;
		let uncertified = |claim: &Path| {
			regions.as_ref().is_some_and(|regions| !within_any(claim, regions))
				|| !ctx.own && views.get(&id).is_none_or(|view| view.as_of(claim).is_none())
		};
		self.reader
			.apply(id, &self.watch, now, |claim| ctx.live && !uncertified(claim));
	}

	/// How the reader now sees each of `claims` of `id`, in `ctx`.
	fn shown_now<'a>(
		&self,
		id: Id,
		ctx: &Ctx<N>,
		claims: impl Iterator<Item = &'a Path>,
	) -> BTreeMap<Path, Option<Shown<A>>> {
		let merged = match ctx.own {
			true => None,
			false => self.views.get(&id),
		};
		// A certified claim is available only through a certifier that is not held:
		// another scope's usable route cannot serve it.
		let serving = match ctx.usable && !ctx.own {
			true => self.certified(id, false),
			false => None,
		};
		let current = self.current.get(&id);
		let at = |claim: &Path| {
			if !within(claim, &self.watch) {
				return None;
			}
			let item = self.adopted_at(id, ctx, claim)?;
			// An uncertified claim is available through a carrier that is not held.
			let loose = match !ctx.own && ctx.live {
				true => self.loose_at(id, ctx, merged, claim).map(|(_, unheld)| unheld),
				false => None,
			};
			// A claim a stream answering the current interest carries is current.
			let certified =
				loose.is_some() || current.is_none_or(|regions| regions.iter().any(|region| within(claim, region)));
			if !certified {
				return None;
			}
			let available = ctx.usable
				&& match loose {
					Some(unheld) => unheld || serving.is_none(),
					None => serving.as_ref().is_none_or(|regions| within_any(claim, regions)),
				};
			Some(Shown {
				item: item.clone(),
				available,
				presented: merged.is_some_and(|merged| merged.floors.contains_key(claim)),
			})
		};
		claims.map(|claim| (claim.clone(), at(claim))).collect()
	}
}
