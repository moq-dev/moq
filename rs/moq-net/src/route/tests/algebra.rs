//! Binds the route proof (`rs/moq-net/verify/route-algebra`) to [`relay::Router`], the
//! router that ships.
//!
//! The proof states the route rule, and the claims that ride on it, as increasing path
//! algebras and applies Daggitt and Griffin's convergence theorem to them. A route there
//! is what a node would offer: [`Offer::own`], or its route as [`Offer::onward`] sends it.
//! One step of the algebra is that offer taken in by [`Here::arrive`] at a neighbour, and
//! [`order`] picks between the results. A claim steps by [`Claim::over`] within a routing
//! domain, and by [`Claim::exported`] and then [`Claim::adopted`] across a grant, and
//! [`claim_order`] picks. Those are the functions the router calls. These tests check them
//! against the algebras' axioms on random links, routes, and claims, and write a fixed
//! sample of their results into `Vectors.agda` and `ClaimVectors.agda`, each a `refl` that
//! the nightly Agda check must accept, so the two cannot drift apart unseen.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
	fmt::Write as _,
	path::PathBuf,
};

use crate::route::relay::{
	self, Best, Claim, Here, Hop, NodeId, Offer, Port, Role, claim_order, floor_of, order, precedence,
};

use super::*;

/// The node receiving every route here: relay 0.
const ME: NodeId = NodeId {
	relay: 0,
	marked: false,
};

/// The checked-in samples, which `ROUTE_VECTORS=write` regenerates.
fn vectors_path(file: &str) -> PathBuf {
	PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
		.join("verify/route-algebra")
		.join(file)
}

/// xorshift64, so a seed always draws the same cases.
pub(super) struct Rng(pub u64);

impl Rng {
	pub fn below(&mut self, n: u64) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0 % n
	}

	/// True one time in `n`.
	pub fn one_in(&mut self, n: u64) -> bool {
		self.below(n) == 0
	}

	pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
		items[self.below(items.len() as u64) as usize]
	}
}

const PRIORITIES: [i8; 5] = [i8::MIN, -1, 0, 5, i8::MAX];

/// How the proof names a Node ID (`Named.agda`).
fn named(node: NodeId) -> u64 {
	1_000 * u64::from(node.marked) + node.relay
}

fn marked(node: NodeId) -> NodeId {
	NodeId { marked: true, ..node }
}

/// A node: one of relays 1 to `relays`.
fn node(rng: &mut Rng, relays: u64) -> NodeId {
	NodeId::new(1 + rng.below(relays))
}

/// How far a priority sits below the highest.
fn penalty(priority: i8) -> i16 {
	i16::from(i8::MAX) - i16::from(priority)
}

fn port(rng: &mut Rng, role: Role) -> Port {
	Port {
		cost: 1 + rng.below(4),
		priority: rng.pick(&PRIORITIES),
		upstream: rng.one_in(3),
		role,
	}
}

// --- Routes ---

/// What a neighbour holds for one forwarding node.
#[derive(Clone, Debug)]
enum Holds {
	/// Its route to itself.
	Own,
	/// Its marked identity's route.
	Marked,
	/// A route it learned, as it holds it.
	Learned(Offer),
}

#[derive(Clone, Debug)]
struct Held {
	/// The neighbour.
	node: NodeId,
	/// Its session to its next hop.
	session: u64,
	holds: Holds,
	/// It announces a claim under its own Node ID.
	claimed: bool,
}

impl Held {
	/// What it sends the receiver over a link it marked `upstream`, as the router does.
	fn sent(&self, upstream: bool) -> Option<Offer> {
		match &self.holds {
			Holds::Own if !self.claimed => None,
			Holds::Own => Offer::own(self.node, self.node, upstream),
			Holds::Marked => Offer::own(self.node, marked(self.node), upstream),
			Holds::Learned(best) => best.onward(self.node, self.session, ME, upstream),
		}
	}

	/// Its route as the algebra holds it: before its own send filters, which the link
	/// applies.
	fn route(&self) -> Offer {
		match &self.holds {
			Holds::Own => Offer::own(self.node, self.node, false).expect("own route"),
			Holds::Marked => Offer::own(self.node, marked(self.node), false).expect("marked route"),
			Holds::Learned(best) => {
				let path = best.path.iter().copied().chain([Hop {
					node: self.node,
					session: self.session,
				}]);
				Offer {
					path: path.collect(),
					..best.clone()
				}
			}
		}
	}
}

/// The receiver's side of the link to one neighbour.
#[derive(Clone, Debug)]
struct Link {
	port: Port,
	/// The neighbour sent GOAWAY.
	draining: bool,
	/// The neighbour marked the link.
	marked_there: bool,
	/// The receiver's session on the link.
	session: u64,
}

/// What the receiver holds.
#[derive(Clone, Debug, Default)]
struct State {
	closed: BTreeMap<(Id, Id), u64>,
	dead: BTreeMap<Hop, NodeId>,
	draining: BTreeSet<NodeId>,
}

impl State {
	fn here(&self) -> Here<'_> {
		Here {
			closed: &self.closed,
			dead: &self.dead,
			draining: &self.draining,
		}
	}
}

/// A path to a node other than `neighbour`, as `neighbour` would hold it: through each
/// relay once, and now and then through the receiver, or a marked identity first.
fn learned(rng: &mut Rng, neighbour: NodeId) -> Offer {
	// Now and then long enough to reach the hop limit, with nothing on it that anything
	// else refuses.
	let (relays, len, looped) = match rng.one_in(20) {
		true => (40, MAX_HOPS as u64 - 3 + rng.below(4), 0),
		false => (6, 1 + rng.below(4), 10),
	};
	let mut path: Vec<Hop> = Vec::new();
	if looped > 0 && rng.one_in(5) {
		let first = node(rng, relays);
		if first.relay != neighbour.relay {
			path.push(Hop {
				node: marked(first),
				session: 0,
			});
			path.push(Hop {
				node: first,
				session: 0,
			});
		}
	}
	while (path.len() as u64) < len {
		let id = match looped > 0 && rng.one_in(looped) {
			true => ME,
			false => node(rng, relays),
		};
		if id.relay != neighbour.relay && path.iter().all(|hop| hop.node.relay != id.relay) {
			let session = if path.is_empty() { 0 } else { 1 + rng.below(3) };
			path.push(Hop { node: id, session });
		}
	}
	Offer {
		path: path.into(),
		draining: rng.one_in(4),
		priority: rng.pick(&PRIORITIES),
		cost: rng.below(12),
		marked: rng.one_in(3),
	}
}

fn held(rng: &mut Rng, neighbour: NodeId) -> Held {
	let holds = match rng.below(8) {
		0 | 1 => Holds::Own,
		2 => Holds::Marked,
		_ => Holds::Learned(learned(rng, neighbour)),
	};
	Held {
		node: neighbour,
		session: 1 + rng.below(3),
		holds,
		claimed: !rng.one_in(4),
	}
}

fn link(rng: &mut Rng, neighbour: NodeId) -> Link {
	Link {
		port: port(rng, Role::Routing(neighbour)),
		draining: rng.one_in(4),
		marked_there: rng.one_in(3),
		session: 1 + rng.below(3),
	}
}

fn state(rng: &mut Rng) -> State {
	let mut state = State::default();
	for _ in 0..rng.below(4) {
		// The receiver keeps no floor for its own sessions.
		let pair = (1 + rng.below(6), rng.below(7));
		state.closed.insert(pair, rng.below(4));
	}
	for _ in 0..rng.below(3) {
		state.draining.insert(node(rng, 6));
	}
	state
}

/// One step of the algebra as the router takes it: the route the receiver holds, ending
/// at the neighbour, or `None` where it is refused.
fn cross(state: &State, link: &Link, held: &Held) -> Option<Offer> {
	let sent = held.sent(link.marked_there)?;
	state.here().arrive(&link.port, link.draining, &sent)
}

/// The route the receiver holds, as the algebra names it: with the receiver's own hop.
fn at_me(offer: Offer, session: u64) -> Offer {
	let path = offer.path.iter().copied().chain([Hop { node: ME, session }]);
	Offer {
		path: path.collect(),
		..offer
	}
}

/// The candidate the router picks, with no next hop in use.
fn pick(candidates: &[(NodeId, u64, Option<Offer>)]) -> Option<Offer> {
	(candidates.iter())
		.filter_map(|(node, session, offer)| {
			let best = Best {
				via: named(*node) as Node,
				offer: offer.clone()?,
			};
			Some((order(&best, None), at_me(best.offer, *session)))
		})
		.min_by(|(a, _), (b, _)| a.cmp(b))
		.map(|(_, offer)| offer)
}

#[test]
fn a_step_never_improves_a_route() {
	let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
	let mut refused = 0;
	for _ in 0..20_000 {
		let state = state(&mut rng);
		let neighbour = node(&mut rng, 6);
		let (held, link) = (held(&mut rng, neighbour), link(&mut rng, neighbour));
		let Some(out) = cross(&state, &link, &held) else {
			refused += 1;
			continue;
		};
		let sent = held.route();
		// A path naming the receiver's relay is refused, and one taken grows by exactly
		// the receiver's hop.
		assert!(!sent.path.iter().any(|hop| hop.node.relay == ME.relay), "{sent:?}");
		assert_eq!(out.path, sent.path);
		// Drain and priority never improve, and the cost strictly rises.
		assert!(out.draining >= sent.draining && out.priority <= sent.priority);
		assert!(out.cost > sent.cost);
		assert!(out.rank() > sent.rank());
		assert!(out.marked >= sent.marked);
	}
	assert!((2_000..18_000).contains(&refused), "{refused} of 20000 refused");
}

/// The dead hops a node keeps refuse nothing the floor does not, since `closed` files
/// every one of them in the floor too.
#[test]
fn the_floor_refuses_every_dead_hop() {
	let mut rng = Rng(0x2545_f491_4f6c_dd1d);
	for _ in 0..20_000 {
		let mut state = state(&mut rng);
		let neighbour = node(&mut rng, 6);
		let (held, link) = (held(&mut rng, neighbour), link(&mut rng, neighbour));
		let Some(sent) = held.sent(link.marked_there) else {
			continue;
		};
		// Name closed some session on the path, as a DEAD message would, and file it
		// the way `closed` does.
		let mut floored = state.clone();
		for pair in sent.path.windows(2).filter(|_| rng.one_in(2)) {
			let (hop, far) = (pair[1], pair[0].node);
			floored.dead.insert(hop, far);
			let (key, at) = floor_of(hop, far);
			let highest = floored.closed.entry(key).or_default();
			*highest = (*highest).max(at);
		}
		state.closed = floored.closed.clone();
		let here = |state: &State| state.here().arrive(&link.port, link.draining, &sent);
		assert_eq!(here(&floored), here(&state));
	}
}

#[test]
fn candidates_order_totally() {
	let mut rng = Rng(0x1405_7b7e_f767_814f);
	for _ in 0..20_000 {
		let state = state(&mut rng);
		let (a, b) = (node(&mut rng, 6), node(&mut rng, 6));
		if a == b {
			continue;
		}
		let one = cross(&state, &link(&mut rng, a), &held(&mut rng, a));
		let two = cross(&state, &link(&mut rng, b), &held(&mut rng, b));
		let (Some(one), Some(two)) = (one, two) else {
			continue;
		};
		let key = |node: NodeId, offer: &Offer| {
			order(
				&Best {
					via: named(node) as Node,
					offer: offer.clone(),
				},
				None,
			)
		};
		assert_ne!(key(a, &one), key(b, &two));
	}
}

/// The Agda term for `offer` as a node holds it, its own hop last: its path from that
/// node to the one it leads to, each link with the session its nearer end holds it on.
fn weight(offer: Option<&Offer>) -> String {
	let Some(offer) = offer else {
		return "invalid".into();
	};
	let hops = &offer.path;
	let mut path = String::new();
	let mut sessions = String::new();
	for pair in hops.windows(2).rev() {
		write!(path, "({} , {}) ∷ ", named(pair[1].node), named(pair[0].node)).unwrap();
		write!(sessions, "{} ∷ ", pair[1].session).unwrap();
	}
	format!(
		"valid (route tt {} {} {} {} ({path}[]) ({sessions}[]))",
		offer.draining,
		penalty(offer.priority),
		offer.cost,
		offer.marked
	)
}

/// The Agda term for the link from the receiver to a neighbour.
fn agda_link(state: &State, port: &Port, draining: bool, marked_there: bool, claimed: bool, session: u64) -> String {
	let mut floors = String::new();
	for ((near, far), session) in &state.closed {
		write!(floors, "floor {near} {far} {session} ∷ ").unwrap();
	}
	let mut named_draining = String::new();
	for node in &state.draining {
		write!(named_draining, "{} ∷ ", named(*node)).unwrap();
	}
	format!(
		"link true {} {} {draining} ({named_draining}[]) ({floors}[]) {} {marked_there} {claimed} {session}",
		port.cost,
		penalty(port.priority),
		port.upstream,
	)
}

fn agda_step(state: &State, link: &Link, held: &Held) -> String {
	format!(
		"extend ({}) ({} , {}) ({})",
		agda_link(
			state,
			&link.port,
			link.draining,
			link.marked_there,
			held.claimed,
			link.session
		),
		named(ME),
		named(held.node),
		weight(Some(&held.route()))
	)
}

/// The route sample: the receiver's marked route, single steps, then picks between two
/// neighbours' offers.
fn route_vectors() -> String {
	let mut out = String::from(
		"-- Generated by `rs/moq-net/src/route/tests/algebra.rs` (`ROUTE_VECTORS=write`); do not edit.\n\
		 -- Each case is a result `route::relay::Router` computed, which the algebra must compute too.\n\n\
		 module Vectors where\n\n\
		 open import Data.Bool.Base using (true; false)\n\
		 open import Data.List.Base using ([]; _∷_)\n\
		 open import Data.Product.Base using (_,_)\n\
		 open import Data.Unit.Base using (tt)\n\
		 open import Relation.Binary.PropositionalEquality using (_≡_; refl)\n\n\
		 open import Named\n",
	);
	let mut rng = Rng(0x0123_4567_89ab_cdef);
	// The receiver's marked identity hangs off it by a free, marked link at session 0.
	for claimed in [true, false] {
		let state = state(&mut rng);
		let free = Port {
			cost: 0,
			priority: i8::MAX,
			upstream: true,
			role: Role::Endpoint,
		};
		let own = claimed.then(|| Offer::own(ME, marked(ME), false)).flatten();
		write!(
			out,
			"\n_ : extend ({}) ({} , {}) (valid (route tt false 0 0 false [] []))\n  ≡ {}\n_ = refl\n",
			agda_link(&state, &free, false, false, claimed, 0),
			named(ME),
			named(marked(ME)),
			weight(own.as_ref())
		)
		.unwrap();
	}
	for _ in 0..200 {
		let state = state(&mut rng);
		let neighbour = node(&mut rng, 6);
		let (held, link) = (held(&mut rng, neighbour), link(&mut rng, neighbour));
		let result = cross(&state, &link, &held).map(|offer| at_me(offer, link.session));
		write!(
			out,
			"\n_ : {}\n  ≡ {}\n_ = refl\n",
			agda_step(&state, &link, &held),
			weight(result.as_ref())
		)
		.unwrap();
	}
	for _ in 0..150 {
		let state = state(&mut rng);
		let (a, b) = (node(&mut rng, 6), node(&mut rng, 6));
		if a == b {
			continue;
		}
		let one = (held(&mut rng, a), link(&mut rng, a));
		// A third of the pairs offer the same route over the same kind of link, so only the
		// neighbour breaks the tie, and a third the same with the other mark or one more
		// hop, so that decides.
		let two = match rng.below(3) {
			0 => (held(&mut rng, b), link(&mut rng, b)),
			tweak => {
				let mut two = (
					Held {
						node: b,
						..one.0.clone()
					},
					one.1.clone(),
				);
				if tweak == 2 {
					match &mut two.0.holds {
						Holds::Learned(best) if rng.one_in(2) && best.path.len() < MAX_HOPS - 3 => {
							let hop = Hop {
								node: NodeId::new(100 + rng.below(100)),
								session: 1,
							};
							best.path = best.path.iter().copied().chain([hop]).collect();
						}
						_ => two.1.port.upstream = !two.1.port.upstream,
					}
				}
				two
			}
		};
		let picked = pick(&[
			(a, one.1.session, cross(&state, &one.1, &one.0)),
			(b, two.1.session, cross(&state, &two.1, &two.0)),
		]);
		write!(
			out,
			"\n_ : {}\n  ⊕ {}\n  ≡ {}\n_ = refl\n",
			agda_step(&state, &one.1, &one.0),
			agda_step(&state, &two.1, &two.0),
			weight(picked.as_ref())
		)
		.unwrap();
	}
	out
}

// --- Claims ---

/// What the proof names the publisher and the broadcast it publishes (`Claim.agda`).
const PUBLISHER: Id = 1;
const BROADCAST: Id = 0;

/// Attributes with an epoch and a distance, as a restarted publisher's claims, or a
/// broader claim presented at a narrower path, carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
	epoch: u64,
	above: u64,
}

impl Item for Run {
	fn lowered(&self, by: u64) -> Self {
		Self {
			above: self.above + by,
			..*self
		}
	}

	fn above(&self) -> u64 {
		self.above
	}

	fn epoch(&self) -> u64 {
		self.epoch
	}
}

/// How a node holds a claim.
#[derive(Clone, Debug)]
enum Kind {
	/// The publisher's own.
	Published,
	/// The node's own, taken from a giver it keys so.
	Own { giver: u64 },
	/// Another node's of its routing domain, over its route there.
	Over { origin: NodeId },
}

/// A claim as a node holds it.
#[derive(Clone, Debug)]
struct Holding {
	holder: NodeId,
	/// As it stands at the holder.
	claim: Claim<Run>,
	kind: Kind,
}

impl Holding {
	/// The relays that handed it on, from the holder to the publisher and its broadcast:
	/// the holder, unless the chain ends with it, then the chain backward.
	fn nodes(&self) -> Vec<Id> {
		let mut nodes = match self.kind {
			Kind::Published => Vec::new(),
			Kind::Own { .. } => Vec::new(),
			Kind::Over { .. } => vec![self.holder.relay],
		};
		nodes.extend(self.claim.chain.iter().rev());
		nodes.extend([PUBLISHER, BROADCAST]);
		nodes
	}

	/// Its claimant, and the holder's key for its giver.
	fn extra(&self) -> (NodeId, u64) {
		match self.kind {
			Kind::Published => (NodeId::new(0), 0),
			Kind::Own { giver } => (
				NodeId {
					marked: self.claim.marked,
					..self.holder
				},
				giver,
			),
			Kind::Over { origin } => (origin, 0),
		}
	}
}

fn run(rng: &mut Rng) -> Run {
	Run {
		epoch: rng.below(3),
		above: rng.below(2),
	}
}

/// A chain of distinct relays 2 and up, now and then long enough to reach the longest.
fn chain(rng: &mut Rng, len: usize) -> Vec<Id> {
	let relays = if len > 8 { 60 } else { 8 };
	let mut chain = Vec::new();
	while chain.len() < len {
		let relay = 2 + rng.below(relays);
		if !chain.contains(&relay) {
			chain.push(relay);
		}
	}
	chain
}

fn chain_len(rng: &mut Rng) -> usize {
	match rng.one_in(15) {
		true => MAX_HOPS - 4 + rng.below(4) as usize,
		false => 1 + rng.below(3) as usize,
	}
}

fn claim(rng: &mut Rng, chain: Vec<Id>) -> Claim<Run> {
	Claim {
		item: run(rng),
		cost: rng.below(12),
		priority: rng.pick(&PRIORITIES),
		draining: rng.one_in(4),
		unavailable: false,
		chain,
		marked: rng.one_in(3),
	}
}

/// A relay's own claim, its chain ending with it.
fn own(rng: &mut Rng, holder: NodeId) -> Holding {
	let len = chain_len(rng) - 1;
	let mut chain = chain(rng, len);
	chain.retain(|relay| *relay != holder.relay);
	chain.push(holder.relay);
	Holding {
		holder,
		claim: claim(rng, chain),
		kind: Kind::Own {
			giver: 1 + rng.below(9),
		},
	}
}

/// What the publisher announces.
fn published(rng: &mut Rng) -> Holding {
	Holding {
		holder: NodeId::new(PUBLISHER),
		claim: Claim {
			item: Run {
				epoch: rng.below(3),
				above: 0,
			},
			cost: 0,
			priority: i8::MAX,
			draining: false,
			unavailable: false,
			chain: Vec::new(),
			marked: false,
		},
		kind: Kind::Published,
	}
}

/// A relay of up to `relays`.
fn relay(rng: &mut Rng, relays: u64) -> NodeId {
	NodeId::new(2 + rng.below(relays))
}

/// A route a node holds to another of its routing domain: its rank and mark are all a
/// claim reads of it.
fn domain_route(rng: &mut Rng, to: NodeId) -> Offer {
	Offer {
		path: [Hop { node: to, session: 0 }].into(),
		draining: rng.one_in(4),
		priority: rng.pick(&PRIORITIES),
		cost: 1 + rng.below(12),
		marked: rng.one_in(3),
	}
}

/// A precedence near `at`: itself mostly, now and then a better or a worse one.
fn top(rng: &mut Rng, at: (u64, Reverse<u64>)) -> (u64, Reverse<u64>) {
	let (above, Reverse(epoch)) = at;
	match rng.below(6) {
		0 if above > 0 => (above - 1, Reverse(epoch)),
		0 => (above, Reverse(epoch + 1)),
		1 => (above + 1, Reverse(epoch)),
		_ => at,
	}
}

/// One step a claim takes into node `to`, with what the receiver knows: the best
/// precedence of any claim it knows of the broadcast, reachable or not.
#[derive(Clone, Debug)]
enum Step {
	/// `from` tells `to` the claim across a grant, or as its endpoint.
	Adopt {
		from: Holding,
		/// The giver marked the link.
		marked_there: bool,
		/// How far below the claim's path `to` is shown it.
		by: u64,
		/// `to`'s port to the giver.
		port: Port,
		/// The giver sent GOAWAY.
		draining: bool,
		/// `to`'s key for the giver.
		giver: u64,
	},
	/// `to` takes the claim of `from`, an announcer of its routing domain, over its route
	/// there.
	Over { from: Holding, route: Offer },
}

#[derive(Clone, Debug)]
struct Taken {
	to: NodeId,
	step: Step,
	top: (u64, Reverse<u64>),
}

impl Taken {
	fn from(&self) -> &Holding {
		match &self.step {
			Step::Adopt { from, .. } | Step::Over { from, .. } => from,
		}
	}

	/// The claim as the router takes it, before the receiver's precedence filter.
	fn unfiltered(&self) -> Option<Holding> {
		match &self.step {
			Step::Adopt {
				from,
				marked_there,
				by,
				port,
				draining,
				giver,
			} => {
				// An endpoint tells its own claim as it is; a relay exports it.
				let told = match from.kind {
					Kind::Published => from.claim.clone(),
					_ => from
						.claim
						.exported(from.holder.relay, true, *marked_there)?
						.lowered(*by),
				};
				let mut claim = told.adopted(self.to.relay, true, port)?;
				// As `restate` takes it from a giver that sent GOAWAY.
				claim.draining |= draining;
				Some(Holding {
					holder: self.to,
					claim,
					kind: Kind::Own { giver: *giver },
				})
			}
			Step::Over { from, route } => {
				let (origin, _) = from.extra();
				Some(Holding {
					holder: self.to,
					claim: from.claim.over(route),
					kind: Kind::Over { origin },
				})
			}
		}
	}

	/// The claim as the router takes it, or `None` where it is refused: as `closest`
	/// passes over one below the best precedence it knows.
	fn taken(&self) -> Option<Holding> {
		self.unfiltered()
			.filter(|held| precedence(&held.claim.item) <= self.top)
	}

	/// How the router orders it among the candidates at `to`, as `restate` and then
	/// `closest` do.
	fn key(&self) -> Option<((u64, Reverse<u64>), relay::ClaimOrder, u64)> {
		let held = self.taken()?;
		let order = match &self.step {
			Step::Adopt { giver, .. } => {
				let (owner, _) = held.extra();
				(claim_order(&held.claim, None, owner), *giver)
			}
			Step::Over { from, route } => {
				let (origin, _) = from.extra();
				(claim_order(&from.claim, Some(route), origin), 0)
			}
		};
		Some((precedence(&held.claim.item), order.0, order.1))
	}
}

/// A step into `to`: across a grant from a relay holding a claim of its own or of its
/// domain, from the publisher, or within `to`'s domain.
fn taken(rng: &mut Rng, to: NodeId) -> Taken {
	let step = match rng.below(5) {
		0 => Step::Adopt {
			from: published(rng),
			marked_there: false,
			by: 0,
			port: port(rng, Role::Endpoint),
			draining: rng.one_in(4),
			giver: 1 + rng.below(9),
		},
		1 | 2 => {
			let giver = relay(rng, 8);
			let mut from = match rng.one_in(2) {
				true => own(rng, giver),
				false => {
					let origin = relay(rng, 8);
					if origin.relay == giver.relay || origin.relay == to.relay {
						return taken(rng, to);
					}
					let mut announced = own(rng, origin);
					// One that left the giver's domain and came back is the case the proof
					// leaves out.
					announced.claim.chain.retain(|relay| *relay != giver.relay);
					let route = domain_route(rng, origin);
					Holding {
						holder: giver,
						claim: announced.claim.over(&route),
						kind: Kind::Over {
							origin: announced.extra().0,
						},
					}
				}
			};
			// Now and then the receiver is on the chain already.
			if !rng.one_in(6) {
				from.claim.chain.retain(|relay| *relay != to.relay);
			}
			if from.claim.chain.is_empty() || from.holder.relay == to.relay {
				return taken(rng, to);
			}
			Step::Adopt {
				from,
				marked_there: rng.one_in(3),
				by: rng.below(2),
				port: port(rng, Role::Scoped(Vec::new())),
				draining: rng.one_in(4),
				giver: 1 + rng.below(9),
			}
		}
		_ => {
			let origin = relay(rng, 8);
			if origin.relay == to.relay {
				return taken(rng, to);
			}
			let mut from = own(rng, origin);
			// The receiver on the chain is the case the proof leaves out: no node within a
			// domain checks the chain.
			from.claim.chain.retain(|relay| *relay != to.relay);
			if from.claim.chain.is_empty() {
				return taken(rng, to);
			}
			let route = domain_route(rng, origin);
			Step::Over { from, route }
		}
	};
	let mut taken = Taken {
		to,
		step,
		top: (0, Reverse(0)),
	};
	taken.top = match taken.unfiltered() {
		Some(held) => top(rng, precedence(&held.claim.item)),
		None => (rng.below(2), Reverse(rng.below(3))),
	};
	taken
}

fn holds_well(held: &Holding) {
	if let Kind::Own { .. } = held.kind {
		assert_eq!(held.claim.chain.last(), Some(&held.holder.relay), "{held:?}");
	}
}

/// Across a grant, from an endpoint, and within a domain, a claim never improves: its
/// precedence, rank, and mark never get better, and its path grows by exactly the hop it
/// crossed, so Rust's chain maps onto the algebra's path.
#[test]
fn a_claim_step_never_improves_a_claim() {
	let mut rng = Rng(0xd1b5_4a32_d192_ed03);
	let (mut refused, mut over) = (0, 0);
	for _ in 0..20_000 {
		let to = relay(&mut rng, 8);
		let step = taken(&mut rng, to);
		let from = step.from();
		holds_well(from);
		let Some(out) = step.unfiltered() else {
			refused += 1;
			continue;
		};
		holds_well(&out);
		over += usize::from(matches!(step.step, Step::Over { .. }));
		let (a, b) = (&from.claim, &out.claim);
		assert!(precedence(&b.item) >= precedence(&a.item));
		assert!(b.draining >= a.draining && b.priority <= a.priority && b.cost >= a.cost);
		assert!(b.marked >= a.marked);
		let (before, after) = (from.nodes(), out.nodes());
		assert_eq!(after[1..], before[..], "{from:?} to {out:?}");
		assert_eq!(after[0], to.relay);
		assert!(!before.contains(&to.relay), "{from:?} to {out:?}");
	}
	assert!((1_000..10_000).contains(&refused), "{refused} of 20000 refused");
	assert!(over > 5_000, "{over} domain steps");
}

/// The Agda term for a claim as its holder holds it.
fn claim_weight(held: Option<&Holding>) -> String {
	let Some(held) = held else {
		return "invalid".into();
	};
	let nodes = held.nodes();
	let mut path = String::new();
	for pair in nodes.windows(2) {
		write!(path, "({} , {}) ∷ ", pair[0], pair[1]).unwrap();
	}
	let (claimant, giver) = held.extra();
	let claim = &held.claim;
	format!(
		"valid (route ({} , just {}) {} {} {} {} ({path}[]) (({} , {}) , {giver}))",
		claim.item.above,
		claim.item.epoch,
		claim.draining,
		penalty(claim.priority),
		claim.cost,
		claim.marked,
		claimant.relay,
		u64::from(claimant.marked),
	)
}

fn claim_step(taken: &Taken) -> String {
	let (above, Reverse(epoch)) = taken.top;
	let (to, from) = (taken.to, taken.from());
	let link = match &taken.step {
		Step::Adopt {
			marked_there,
			by,
			port,
			draining,
			giver,
			..
		} => format!(
			"step true {} {} {draining} {} {marked_there} true {by} nothing ({above} , just {epoch}) {} {giver}",
			port.cost,
			penalty(port.priority),
			port.upstream,
			to.relay,
		),
		Step::Over { route, .. } => format!(
			"step true {} {} {} {} false false 0 nothing ({above} , just {epoch}) {} 0",
			route.cost,
			penalty(route.priority),
			route.draining,
			route.marked,
			to.relay,
		),
	};
	let edge = (to.relay, from.nodes()[0]);
	format!(
		"extend ({link}) ({} , {}) ({})",
		edge.0,
		edge.1,
		claim_weight(Some(from))
	)
}

/// The same claim as `from` holds it, held by `other` instead, which neither `to` nor
/// any relay on its chain is. False where it cannot be.
fn moved(from: &mut Holding, to: NodeId, other: NodeId) -> bool {
	let chain = &mut from.claim.chain;
	let named = match from.kind {
		Kind::Published => return false,
		Kind::Own { .. } => chain[..chain.len() - 1].contains(&other.relay),
		Kind::Over { .. } => chain.contains(&other.relay),
	};
	if named || other.relay == to.relay || other.relay == from.holder.relay {
		return false;
	}
	if let Kind::Own { .. } = from.kind {
		*chain.last_mut().expect("its own") = other.relay;
	}
	from.holder = other;
	true
}

/// The claim sample: single steps, then picks between two candidates at one node.
fn claim_vectors() -> String {
	let mut out = String::from(
		"-- Generated by `rs/moq-net/src/route/tests/algebra.rs` (`ROUTE_VECTORS=write`); do not edit.\n\
		 -- Each case is a result `route::relay::Router` computed of a claim, which the algebra must compute too.\n\n\
		 module ClaimVectors where\n\n\
		 open import Data.Bool.Base using (true; false)\n\
		 open import Data.List.Base using ([]; _∷_)\n\
		 open import Data.Maybe.Base using (nothing; just)\n\
		 open import Data.Product.Base using (_,_)\n\
		 open import Relation.Binary.PropositionalEquality using (_≡_; refl)\n\n\
		 open import Claim\n",
	);
	let mut rng = Rng(0x0f1e_2d3c_4b5a_6978);
	for _ in 0..200 {
		let to = relay(&mut rng, 8);
		let step = taken(&mut rng, to);
		write!(
			out,
			"\n_ : {}\n  ≡ {}\n_ = refl\n",
			claim_step(&step),
			claim_weight(step.taken().as_ref())
		)
		.unwrap();
	}
	let mut picks = 0;
	while picks < 120 {
		let to = relay(&mut rng, 8);
		let one = taken(&mut rng, to);
		// A third of the pairs take the same claim the same way from another giver or
		// announcer, so only the claimant or the giver breaks the tie, and a third the same
		// with the other mark or one more relay on the chain, so that decides.
		let mut two = match rng.below(3) {
			0 => taken(&mut rng, to),
			tweak => {
				let mut two = one.clone();
				let other = relay(&mut rng, 8);
				let (Step::Adopt { from, .. } | Step::Over { from, .. }) = &mut two.step;
				if !moved(from, to, other) {
					continue;
				}
				if tweak == 2 && rng.one_in(2) {
					// A relay outside every chain here, which no step refuses.
					from.claim.chain.insert(0, 100 + rng.below(100));
				} else if tweak == 2 {
					match &mut two.step {
						Step::Adopt { port, .. } => port.upstream = !port.upstream,
						Step::Over { route, .. } => route.marked = !route.marked,
					}
				}
				if let Step::Adopt { giver, .. } = &mut two.step {
					*giver = 1 + (*giver % 9);
				}
				two
			}
		};
		two.top = one.top;
		// One holder takes a claim from a giver once, and one claimant has one claim.
		let distinct = match (&one.step, &two.step) {
			(Step::Adopt { giver: a, from: x, .. }, Step::Adopt { giver: b, from: y, .. }) => {
				a != b && x.holder.relay != y.holder.relay
			}
			(Step::Over { from: x, .. }, Step::Over { from: y, .. }) => x.extra().0 != y.extra().0,
			_ => true,
		};
		if !distinct {
			continue;
		}
		picks += 1;
		let picked = [&one, &two]
			.into_iter()
			.filter_map(|step| Some((step.key()?, step.taken()?)))
			.min_by(|(a, _), (b, _)| a.cmp(b))
			.map(|(_, held)| held);
		write!(
			out,
			"\n_ : {}\n  ⊕ {}\n  ≡ {}\n_ = refl\n",
			claim_step(&one),
			claim_step(&two),
			claim_weight(picked.as_ref())
		)
		.unwrap();
	}
	out
}

#[test]
fn the_agda_samples_are_current() {
	let write = std::env::var("ROUTE_VECTORS").as_deref() == Ok("write");
	for (file, now) in [
		("Vectors.agda", route_vectors()),
		("ClaimVectors.agda", claim_vectors()),
	] {
		let path = vectors_path(file);
		if write {
			std::fs::write(&path, &now).unwrap();
			continue;
		}
		let checked_in = std::fs::read_to_string(&path).unwrap();
		assert!(
			checked_in == now,
			"{} is stale: rerun this test with ROUTE_VECTORS=write, then `just test route-proof`",
			path.display()
		);
	}
}
