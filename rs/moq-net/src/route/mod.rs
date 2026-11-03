//! The per-origin route layer, as one node's state machine.
//!
//! Routing is Babel (RFC 8966) keyed by routing identity: a node selects one
//! feasible route per identity, advertises it to every neighbor whose stream
//! asks, and only the identity's own node advances its seqno. Each origin
//! versions its own claims with a revision, and a node merges every neighbor's
//! certified view claim by claim, so no stale copy revives a withdrawal it
//! already knows. See `quest/m1/cluster-routing/routes.md` for the contract.
//!
//! The router does no I/O: the caller feeds it what each neighbor sent and
//! drains [`Out`] messages and reader [`Event`]s. Neighbor keys `N` and claim
//! attributes `A` are opaque to it.

#![cfg_attr(not(test), allow(dead_code, reason = "the lite-07 session does not drive it yet"))]

mod view;

#[cfg(test)]
mod tests;

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
	fmt,
	sync::Arc,
};

use crate::Path;

pub(crate) use view::{View, overlaps, within};

/// An opaque routing identity, declared by an origin or derived by [`Scope`].
pub(crate) type Id = u64;

/// Set on every identity a [`Scope`] qualifies, so any node recognizes a
/// qualified key and refuses to qualify it again.
pub(crate) const QUALIFIED: Id = 1 << 63;

/// A route metric: the sum of configured link costs.
pub(crate) type Metric = u64;

/// An origin's route seqno.
pub(crate) type Seqno = u64;

/// An origin's claim revision, advancing on every change to its own claims.
pub(crate) type Rev = u64;

/// The largest lite-07 varint. As a metric it means held; no origin ever
/// advertises it as a seqno or revision, rotating its identity instead, so every
/// advertised seqno leaves room for the successor a request names.
pub(crate) const MAX: u64 = u64::MAX;

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

/// What a node advertises for one identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Advert {
	pub seqno: Seqno,
	pub metric: Metric,
	pub down_only: bool,
}

/// A ROUTE's state on a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
	/// A usable route.
	Finite(Advert),
	/// An infinite metric: unusable, but the sender still holds the identity's
	/// announcements while it recovers. Only follows a finite route.
	Held,
}

/// A message for one neighbor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Out<N, A> {
	/// ROUTE_START or ROUTE_UPDATE, or ROUTE_END for `None`.
	Route { to: N, id: Id, reach: Option<Reach> },
	/// A batch of announcement changes for `id`, closed by its floor list, which
	/// replaces the last one sent for that route.
	Batch {
		to: N,
		id: Id,
		changes: Vec<(Path, Option<A>)>,
		floors: BTreeMap<Path, Rev>,
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
#[derive(Clone, Copy, Debug)]
struct Entry {
	/// The last finite advertisement.
	advert: Advert,
	held: bool,
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
	/// Within the neighbor's scope: what it certifies here.
	view: View<A>,
	/// The interest generation it answers.
	interest: u32,
}

struct Neighbor {
	link: Link,
	/// The neighbor sent GOAWAY: rank it below every other eligible route.
	draining: bool,
	/// The prefix the neighbor asked this node for, once it opened a stream.
	watch: Option<Path>,
}

/// An identity this node serves itself.
struct Origin<A> {
	seqno: Seqno,
	/// Whether the node declared it, rather than forging it with no claims.
	declared: bool,
	view: View<A>,
}

/// Retained state, summed over the node, for budgets and leak checks.
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
	neighbors: BTreeMap<N, Neighbor>,
	origins: BTreeMap<Id, Origin<A>>,
	rx: BTreeMap<Id, BTreeMap<N, Entry>>,
	/// For a key this node qualified, the identity the neighbor named.
	filed: BTreeMap<(N, Id), Id>,
	/// Babel's feasibility distance per identity.
	fd: BTreeMap<Id, (Seqno, Metric)>,
	sent: BTreeMap<(N, Id), Reach>,
	/// ROUTE_ENDs sent to each neighbor and not yet echoed back. Until they all
	/// return, a neighbor may still hold this node's advertisement, so the
	/// feasibility distance stays.
	unacked: BTreeMap<(N, Id), u32>,
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
	heard: BTreeMap<(N, Id), Heard<A>>,
	sent_views: BTreeMap<(N, Id), View<A>>,
	shown: BTreeMap<(Id, Path), Shown<A>>,
	/// Mints a fresh identity on rotation.
	rng: u64,
	/// Per claim this node has shown, how many times it forgot that claim.
	#[cfg(test)]
	forgot: BTreeMap<(Id, Path), u32>,
	out: Vec<Out<N, A>>,
	events: Vec<Event<A>>,
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

impl<N: Copy + Ord + fmt::Debug, A: Clone + Eq> Router<N, A> {
	/// A node with no sessions, minting rotated identities from `seed`.
	pub fn new(seed: u64) -> Self {
		Self {
			neighbors: BTreeMap::new(),
			origins: BTreeMap::new(),
			rx: BTreeMap::new(),
			filed: BTreeMap::new(),
			fd: BTreeMap::new(),
			sent: BTreeMap::new(),
			unacked: BTreeMap::new(),
			pending: BTreeMap::new(),
			watch: Path::empty(),
			interest: 0,
			views: BTreeMap::new(),
			current: BTreeMap::new(),
			heard: BTreeMap::new(),
			sent_views: BTreeMap::new(),
			shown: BTreeMap::new(),
			rng: seed | 1,
			#[cfg(test)]
			forgot: BTreeMap::new(),
			out: Vec::new(),
			events: Vec::new(),
		}
	}

	/// Take the messages queued for neighbors, in order.
	pub fn drain(&mut self) -> Vec<Out<N, A>> {
		std::mem::take(&mut self.out)
	}

	/// Take the reader events, in order.
	pub fn events(&mut self) -> Vec<Event<A>> {
		std::mem::take(&mut self.events)
	}

	// --- Sessions ---

	/// A session to `neighbor` opened.
	pub fn connect(&mut self, neighbor: N, link: Link) -> Result<(), Error> {
		if link.cost == 0 {
			return Err(Error::ZeroCost);
		}
		self.neighbors.insert(
			neighbor,
			Neighbor {
				link,
				draining: false,
				watch: None,
			},
		);
		self.advertise();
		Ok(())
	}

	/// The session to `neighbor` closed: everything learned on it goes, and it
	/// counts as the echo of every ROUTE_END sent on it.
	pub fn disconnect(&mut self, neighbor: N) {
		if self.neighbors.remove(&neighbor).is_none() {
			return;
		}
		for routes in self.rx.values_mut() {
			routes.remove(&neighbor);
		}
		self.rx.retain(|_, routes| !routes.is_empty());
		self.filed.retain(|(from, _), _| *from != neighbor);
		self.heard.retain(|(from, _), _| *from != neighbor);
		for pending in self.pending.values_mut() {
			pending.from.remove(&neighbor);
		}
		self.unacked.retain(|(to, _), _| *to != neighbor);
		self.sent.retain(|(to, _), _| *to != neighbor);
		self.sent_views.retain(|(to, _), _| *to != neighbor);
		self.advertise();
	}

	/// `neighbor` opened a stream asking for announcements under `prefix`: it
	/// gets a complete snapshot.
	pub fn peer_interest(&mut self, neighbor: N, prefix: Path) -> Result<(), Error> {
		let peer = self.neighbors.get_mut(&neighbor).ok_or(Error::UnknownNeighbor)?;
		peer.watch = Some(prefix);
		self.sent_views.retain(|(to, _), _| *to != neighbor);
		self.advertise();
		Ok(())
	}

	/// `neighbor` sent GOAWAY: it ranks below every other eligible route while
	/// its session stays up.
	pub fn goaway(&mut self, neighbor: N) {
		if let Some(peer) = self.neighbors.get_mut(&neighbor) {
			peer.draining = true;
		}
		self.advertise();
	}

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
		self.advertise();
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
		self.advertise();
	}

	/// Advertise `id` as if serving it, with no claims: what a forwarder that
	/// lies about an identity does.
	#[cfg(test)]
	pub fn forge(&mut self, id: Id, seqno: Seqno) {
		self.origins.insert(
			id,
			Origin {
				seqno,
				declared: false,
				view: View::default(),
			},
		);
		self.advertise();
	}

	/// The identity this node declares.
	pub fn declared(&self) -> Option<Id> {
		self.origins
			.iter()
			.find(|(_, origin)| origin.declared)
			.map(|(id, _)| *id)
	}

	/// The declared identity's seqno.
	pub fn seqno(&self) -> Option<Seqno> {
		self.origins.values().find(|origin| origin.declared).map(|o| o.seqno)
	}

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
			self.advertise();
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
			Some(item) => view.items.insert(claim, item),
			None => view.items.remove(&claim),
		};
		self.advertise();
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
			// xorshift64: a fresh, unqualified identity.
			self.rng ^= self.rng << 13;
			self.rng ^= self.rng >> 7;
			self.rng ^= self.rng << 17;
			let id = self.rng & !QUALIFIED;
			if id != old && !self.origins.contains_key(&id) {
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
		self.advertise();
	}

	// --- Received messages ---

	/// ROUTE_START or ROUTE_UPDATE from `from`, or ROUTE_END for `None`.
	pub fn route(&mut self, from: N, named: Id, reach: Option<Reach>) {
		if !self.neighbors.contains_key(&from) {
			return;
		}
		// Echo every ROUTE_END once applied, filed or refused.
		if reach.is_none() {
			self.out.push(Out::Echo { to: from, id: named });
		}
		let Some(id) = self.key(from, named) else {
			return;
		};
		if id != named {
			match reach {
				Some(_) => self.filed.insert((from, id), named),
				None => self.filed.remove(&(from, id)),
			};
		}
		let routes = self.rx.entry(id).or_default();
		match reach {
			Some(Reach::Finite(advert)) => {
				routes.insert(from, Entry { advert, held: false });
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
		self.advertise();
	}

	/// A batch of announcement changes for `named` from `from`, closed by its
	/// floor list, on the stream answering interest generation `interest`.
	pub fn batch(
		&mut self,
		from: N,
		named: Id,
		changes: Vec<(Path, Option<A>)>,
		floors: BTreeMap<Path, Rev>,
		interest: u32,
	) {
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
		let heard = self.heard.entry((from, id)).or_insert_with(|| Heard {
			raw: View::default(),
			view: View::default(),
			interest,
		});
		// A new stream starts from an empty view.
		if heard.interest != interest {
			heard.raw = View::default();
			heard.interest = interest;
		}
		heard.raw.apply(changes, floors);
		heard.view = match &scope {
			Some(regions) => heard.raw.restrict_all(regions),
			None => heard.raw.clone(),
		};
		let view = heard.view.clone();

		// The batch certifies the regions its floors name.
		for region in view.floors.keys() {
			let Some(regions) = self.current.get_mut(&id) else {
				break;
			};
			if *region == self.watch {
				self.current.remove(&id);
			} else if !regions.iter().any(|known| within(region, known)) {
				regions.retain(|known| !within(known, region));
				regions.push(region.clone());
			}
		}
		if !self.origins.contains_key(&id) {
			let merged = self.views.entry(id).or_default();
			merged.merge(&view);
			// The batch certifies the interest, so what lies outside can go.
			let kept = merged.retire(&self.watch);
			if kept != *merged {
				*merged = kept;
				let watch = [self.watch.clone()];
				self.forgot_outside(id, &watch);
			}
		}
		self.advertise();
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
			}
			self.advertise();
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
		self.advertise();
	}

	/// The echo of a ROUTE_END this node sent `from` for `id`.
	pub fn echo(&mut self, from: N, id: Id) {
		let Some(left) = self.unacked.get_mut(&(from, id)) else {
			// Unsolicited: nothing of ours to drain.
			return;
		};
		*left -= 1;
		if *left == 0 {
			self.unacked.remove(&(from, id));
		}
		self.advertise();
	}

	/// Forget `id`'s routes and announcements. The feasibility distance goes on
	/// its own once drained.
	pub fn collect(&mut self, id: Id) {
		self.rx.remove(&id);
		self.filed.retain(|(_, key), _| *key != id);
		self.views.remove(&id);
		self.current.remove(&id);
		self.heard.retain(|(_, key), _| *key != id);
		self.pending.remove(&id);
		self.forgot_outside(id, &[]);
		self.advertise();
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
			if entry.held || !self.feasible(id, entry.advert) {
				continue;
			}
			let peer = &self.neighbors[&next];
			let Some(metric) = add(entry.advert.metric, peer.link.cost) else {
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

	/// The route this node would offer an upstream link for `id`.
	pub fn export(&self, id: Id) -> Option<Selected<N>> {
		self.selected(id).filter(|route| !route.down_only)
	}

	/// Whether this node holds a reachable route to `id` but none is feasible.
	pub fn starved(&self, id: Id) -> bool {
		self.selected(id).is_none() && self.infeasible(id).next().is_some()
	}

	/// Whether this node, with no usable route to `id`, holds a held route it
	/// could recover through.
	pub fn grounded(&self, id: Id) -> bool {
		self.selected(id).is_none() && self.recoverable(id)
	}

	/// Whether this node holds any route state for `id`.
	pub fn holds(&self, id: Id) -> bool {
		self.rx.contains_key(&id)
	}

	/// Whether this node keeps a feasibility distance for `id`.
	pub fn remembers(&self, id: Id) -> bool {
		self.fd.contains_key(&id)
	}

	/// How many identities this node holds routes to.
	pub fn identities(&self) -> usize {
		self.rx.len()
	}

	/// How many qualified keys this node remembers the forwarded name of.
	pub fn aliases(&self) -> usize {
		self.filed.len()
	}

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

	/// What this node's reader sees.
	pub fn shown(&self) -> &BTreeMap<(Id, Path), Shown<A>> {
		&self.shown
	}

	/// Whether this node shows `claim` from `id`, and whether it is available.
	pub fn shows(&self, id: Id, claim: &Path) -> Option<bool> {
		self.shown.get(&(id, claim.clone())).map(|shown| shown.available)
	}

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
	fn name(&self, next: N, id: Id) -> Id {
		self.filed.get(&(next, id)).copied().unwrap_or(id)
	}

	/// Babel's feasibility condition against this node's feasibility distance.
	fn feasible(&self, id: Id, advert: Advert) -> bool {
		match self.fd.get(&id) {
			None => true,
			Some(&(seqno, metric)) => advert.seqno > seqno || (advert.seqno == seqno && advert.metric < metric),
		}
	}

	/// The usable but infeasible routes for `id`, with their metrics.
	fn infeasible(&self, id: Id) -> impl Iterator<Item = (Metric, N)> + '_ {
		self.rx
			.get(&id)
			.into_iter()
			.flatten()
			.filter(move |(_, entry)| !entry.held && !self.feasible(id, entry.advert))
			.filter_map(move |(&next, entry)| Some((add(entry.advert.metric, self.neighbors[&next].link.cost)?, next)))
	}

	/// Where this node sends a request for `id`: its feasible route's next hop,
	/// else its best infeasible route's, even when that neighbor asked.
	fn target(&self, id: Id) -> Option<N> {
		self.selected(id)
			.and_then(|route| route.next)
			.or_else(|| self.infeasible(id).min().map(|(_, next)| next))
	}

	/// Whether this node holds a held route to `id` it could recover through.
	/// One counts only if its last finite advertisement is feasible here, so
	/// holds follow a strictly decreasing metric toward a starved node and
	/// cannot hold each other up.
	fn recoverable(&self, id: Id) -> bool {
		self.rx.get(&id).is_some_and(|routes| {
			routes
				.values()
				.any(|entry| entry.held && self.feasible(id, entry.advert))
		})
	}

	/// Whether this node, having selected `route` for `id`, keeps showing its
	/// announcements: routed, starved, or holding a recoverable route.
	fn live(&self, id: Id, route: Option<Selected<N>>) -> bool {
		route.is_some() || self.infeasible(id).next().is_some() || self.recoverable(id)
	}

	/// The neighbors whose views can certify `id`: those whose last finite
	/// advertisement is feasible here, held or not. Feasible edges strictly
	/// decrease the metric toward the origin, so certification never circulates.
	fn certifiers(&self, id: Id) -> Vec<N> {
		self.rx.get(&id).map_or_else(Vec::new, |routes| {
			routes
				.iter()
				.filter(|(_, entry)| self.feasible(id, entry.advert))
				.map(|(&next, _)| next)
				.collect()
		})
	}

	/// The regions of `id` that this node's certifiers certify, those whose
	/// route is held only when `held`, or `None` while it holds no route or a
	/// certifier has not yet answered the current interest.
	fn certified(&self, id: Id, held: bool) -> Option<Vec<Path>> {
		let routes = self.rx.get(&id)?;
		let mut regions = Vec::new();
		for next in self.certifiers(id) {
			let heard = self
				.heard
				.get(&(next, id))
				.filter(|heard| heard.interest == self.interest)?;
			if held || !routes[&next].held {
				regions.extend(heard.view.floors.keys().cloned());
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
		let lost = merged
			.floors
			.iter()
			.filter(|(floor, _)| !regions.iter().any(|region| within(floor, region)))
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
				rebuilt.merge(&self.heard[&(next, id)].view);
			}
			(rebuilt, Vec::new())
		};
		self.views.insert(id, rebuilt);
		self.forgot_outside(id, &remembered);
	}

	/// Count a forget against each shown claim of `id` overlapping none of `kept`.
	#[cfg_attr(not(test), allow(unused_variables, reason = "only fixtures count forgets"))]
	fn forgot_outside(&mut self, id: Id, kept: &[Path]) {
		#[cfg(test)]
		for ((key, claim), count) in self.forgot.iter_mut() {
			if *key == id && !kept.iter().any(|region| overlaps(claim, region)) {
				*count += 1;
			}
		}
	}

	/// The view this node shows and forwards for `id`, having selected `route`.
	fn adopted(&self, id: Id, route: Option<Selected<N>>) -> Option<&View<A>> {
		if let Some(origin) = self.origins.get(&id) {
			return Some(&origin.view);
		}
		self.live(id, route).then(|| self.views.get(&id)).flatten()
	}

	/// Whether this node sends `id` to `neighbor` at all: its own identities to
	/// everyone, learned ones only to a proxied neighbor.
	fn serves(&self, neighbor: &Neighbor, id: Id) -> bool {
		neighbor.link.proxy || self.origins.contains_key(&id)
	}

	/// Recompute every selection, record feasibility distances for what this
	/// node advertises, and queue any change for each neighbor.
	fn advertise(&mut self) {
		let ids: Vec<Id> = self.views.keys().copied().collect();
		for id in ids {
			self.prune(id);
		}
		let mut ids: BTreeSet<Id> = self.rx.keys().copied().collect();
		ids.extend(self.origins.keys());
		ids.extend(self.sent.keys().map(|(_, id)| *id));
		let peers: Vec<N> = self
			.neighbors
			.iter()
			.filter(|(_, peer)| peer.watch.is_some())
			.map(|(n, _)| *n)
			.collect();

		for id in ids {
			let selected = self.selected(id);
			// The feasibility distance tracks finite updates actually sent (RFC 8966 s3.7.3).
			if let Some(selected) = selected
				&& peers.iter().any(|peer| {
					let peer = &self.neighbors[peer];
					self.serves(peer, id) && !(peer.link.upstream && selected.down_only)
				}) {
				self.record(id, selected);
			}
			// Recording may have tightened the feasibility distance: select again.
			let route = self.selected(id);
			let live = self.live(id, route);
			let adopted = self.adopted(id, route).cloned();
			// A sender that certifies anything answers every stream, even one
			// whose interest none of its regions meets, so the receiver knows it
			// certifies nothing there.
			let answers = adopted.as_ref().is_some_and(|view| !view.floors.is_empty());
			let mut restricted: Vec<(Path, View<A>)> = Vec::new();

			for &to in &peers {
				let peer = &self.neighbors[&to];
				let serves = self.serves(peer, id);
				let was = self.sent.get(&(to, id)).copied();
				if !serves && was.is_none() {
					continue;
				}
				let reach = match selected {
					Some(route) if serves && !(peer.link.upstream && route.down_only) => Some(Reach::Finite(Advert {
						seqno: route.seqno,
						metric: route.metric,
						down_only: route.down_only,
					})),
					None if serves && live && was.is_some() => Some(Reach::Held),
					_ => None,
				};
				let watch = peer.watch.clone().expect("a watching peer");
				if was != reach {
					match reach {
						Some(reach) => self.sent.insert((to, id), reach),
						None => {
							*self.unacked.entry((to, id)).or_default() += 1;
							self.sent.remove(&(to, id))
						}
					};
					self.out.push(Out::Route { to, id, reach });
				}
				// Announcements ride on the route and end with it.
				if reach.is_none() {
					self.sent_views.remove(&(to, id));
					continue;
				}
				// Peers mostly share an interest, so restrict once per interest.
				let index = match restricted.iter().position(|(interest, _)| *interest == watch) {
					Some(index) => index,
					None => {
						let view = adopted
							.as_ref()
							.map_or_else(View::default, |view| view.restrict(&watch));
						restricted.push((watch, view));
						restricted.len() - 1
					}
				};
				let view = &restricted[index].1;
				let changes = match self.sent_views.get(&(to, id)) {
					Some(sent) if sent == view => continue,
					Some(sent) => view.diff(sent),
					None if !answers => continue,
					None => view.diff(&View::default()),
				};
				self.sent_views.insert((to, id), view.clone());
				self.out.push(Out::Batch {
					to,
					id,
					changes,
					floors: view.floors.clone(),
				});
			}
		}
		// Drop a feasibility distance once nothing here routes or advertises the
		// identity and every neighbor echoed its ROUTE_END: no node then holds an
		// advertisement from this one, so no older one can return as a loop.
		let (rx, origins, sent, unacked) = (&self.rx, &self.origins, &self.sent, &self.unacked);
		self.fd.retain(|id, _| {
			rx.contains_key(id)
				|| origins.contains_key(id)
				|| sent.keys().any(|(_, sent)| sent == id)
				|| unacked.keys().any(|(_, unacked)| unacked == id)
		});
		self.requests();
		self.show();
	}

	/// Start, resend, or clear this node's seqno requests.
	fn requests(&mut self) {
		let mut ids: BTreeSet<Id> = self.pending.keys().copied().collect();
		ids.extend(self.rx.keys());
		for id in ids {
			if self.origins.contains_key(&id) {
				self.pending.remove(&id);
				continue;
			}
			let route = self.selected(id);
			let starved = route.is_none() && self.infeasible(id).next().is_some();
			if starved {
				let want = self.fd[&id].0 + 1;
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
				continue;
			};
			let answered = route.is_some_and(|route| route.seqno >= pending.seqno);
			if answered || (pending.from.is_empty() && !starved) {
				self.pending.remove(&id);
				continue;
			}
			// A request follows the next hop even back through a requester: next
			// hops form no loop, one pending request per hop stops a bounce, and
			// no requester ever cancels, so excluding one can strand a request
			// while every route still runs through it.
			let Some(target) = self.target(id) else {
				self.pending.remove(&id);
				continue;
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
	}

	/// Recompute what this node's reader sees and log each change.
	fn show(&mut self) {
		let mut ids: BTreeSet<Id> = self.views.keys().copied().collect();
		ids.extend(self.origins.keys());
		ids.extend(self.heard.keys().map(|(_, id)| *id));
		let mut shown = BTreeMap::new();
		for id in ids {
			let route = self.selected(id);
			let Some(view) = self.adopted(id, route) else {
				continue;
			};
			let usable = route.is_some();
			let own = self.origins.contains_key(&id);
			// A claim is available only through a certifier that is not held:
			// another scope's usable route cannot serve it.
			let serving = match usable && !view.items.is_empty() && !own {
				true => self.certified(id, false),
				false => None,
			};
			let current = self.current.get(&id);
			for (claim, item) in &view.items {
				let certified = current.is_none_or(|regions| regions.iter().any(|region| overlaps(claim, region)));
				let available = usable
					&& serving
						.as_ref()
						.is_none_or(|regions| regions.iter().any(|region| overlaps(claim, region)));
				if overlaps(claim, &self.watch) && certified {
					shown.insert(
						(id, claim.clone()),
						Shown {
							item: item.clone(),
							available,
						},
					);
				}
			}
		}
		let old = std::mem::take(&mut self.shown);
		for ((id, claim), now) in &shown {
			match old.get(&(*id, claim.clone())) {
				None => self.log(*id, claim, Kind::Start, &now.item),
				Some(was) => {
					if was.item != now.item {
						self.log(*id, claim, Kind::Update, &now.item);
					}
					if was.available != now.available {
						let kind = match now.available {
							true => Kind::Available,
							false => Kind::Unavailable,
						};
						self.log(*id, claim, kind, &now.item);
					}
				}
			}
		}
		for ((id, claim), was) in &old {
			if shown.contains_key(&(*id, claim.clone())) {
				continue;
			}
			// A claim no routed neighbor's region covers was let go, not ended.
			let uncertified = self
				.certified(*id, true)
				.is_some_and(|regions| !regions.iter().any(|region| overlaps(claim, region)));
			let kind = if !overlaps(claim, &self.watch) {
				Kind::Unwatched
			} else if self.live(*id, self.selected(*id)) && !uncertified {
				Kind::End
			} else {
				Kind::Offline
			};
			self.log(*id, claim, kind, &was.item);
		}
		self.shown = shown;
	}

	fn log(&mut self, id: Id, claim: &Path, kind: Kind, item: &A) {
		#[cfg(test)]
		let forgot = *self.forgot.entry((id, claim.clone())).or_insert(0);
		self.events.push(Event {
			id,
			claim: claim.clone(),
			kind,
			item: item.clone(),
			#[cfg(test)]
			forgot,
		});
	}

	/// Babel's feasibility distance: reset by a newer seqno, lowered by a better metric.
	fn record(&mut self, id: Id, selected: Selected<N>) {
		let fd = self.fd.entry(id).or_insert((selected.seqno, selected.metric));
		if selected.seqno > fd.0 || (selected.seqno == fd.0 && selected.metric < fd.1) {
			*fd = (selected.seqno, selected.metric);
		}
	}
}

/// `metric` plus a strictly positive link `cost`, or `None` when it reaches the
/// infinite metric: overflow is unreachable, never saturated.
fn add(metric: Metric, cost: Metric) -> Option<Metric> {
	metric.checked_add(cost).filter(|&metric| metric < MAX)
}
