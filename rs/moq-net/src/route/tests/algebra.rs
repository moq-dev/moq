//! Binds the route proof (`rs/moq-net/verify/route-algebra`) to the prototype rule.
//!
//! The proof states the multicast rule as an increasing path algebra and applies
//! Daggitt and Griffin's convergence theorem to it. A route there is what a node
//! would offer: [`Offer::own`], or its best route as [`Offer::onward`] sends it. One
//! step of the algebra is that offer crossing [`Here::arrive`] at a neighbour, and
//! [`order`] picks between the results. Those are the functions the router calls.
//! These tests check them against the algebra's axioms on random links and routes,
//! and write a fixed sample of their results into `Vectors.agda`, each a `refl`
//! that the nightly Agda check must accept, so the two cannot drift apart unseen.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
	fmt::Write as _,
	path::PathBuf,
};

use super::compare::Arm;
use super::multi::{Best, Change, Claim, Here, Hop, Msg, Offer, Port, Role, Router, Tune, floor_of, order, started};
use super::*;

/// The rule the epic picked: floor, epoch, batch, drain, and claimed.
const KEPT: Tune = match Arm::KEPT {
	Arm::Multi(tune) => tune,
	Arm::Path(_) => panic!("the picked rule is the prototype's"),
};

/// The node receiving every route here: relay 0 at its first start.
const ME: Id = 1_000;

/// The checked-in sample, which `ROUTE_VECTORS=write` regenerates.
fn vectors_path() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("verify/route-algebra/Vectors.agda")
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

/// A node: relays 1 to `relays` at their first or second start, as `hop` names them.
fn node(rng: &mut Rng, relays: u64) -> Id {
	1_000 * (1 + rng.below(2)) + 1 + rng.below(relays)
}

/// What a neighbour holds for one forwarding node.
#[derive(Clone, Debug)]
struct Held {
	/// The neighbour.
	node: Id,
	/// Its session to its next hop.
	session: u64,
	/// Its best route, or `None` for its route to itself.
	best: Option<Offer>,
	/// It announces a claim.
	claimed: bool,
}

impl Held {
	/// What it sends the receiver over a link it marked `upstream`, as the router does.
	fn sent(&self, upstream: bool) -> Option<Offer> {
		match &self.best {
			None => Offer::own(self.node, KEPT, self.claimed),
			Some(best) => best.onward(self.node, self.session, ME, upstream),
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
	closed: BTreeMap<(Id, Id), (u64, u64)>,
	dead: BTreeMap<Hop, Id>,
	suspects: BTreeMap<Id, BTreeSet<Hop>>,
	draining: BTreeSet<Id>,
}

impl State {
	fn here(&self) -> Here<'_> {
		Here {
			me: ME,
			tune: KEPT,
			closed: &self.closed,
			dead: &self.dead,
			suspects: &self.suspects,
			draining: &self.draining,
		}
	}
}

fn held(rng: &mut Rng, neighbour: Id) -> Held {
	let best = (!rng.one_in(4)).then(|| {
		// Now and then long enough to reach the hop limit, without the receiver on
		// it so that nothing else refuses it.
		let (relays, len, looped) = match rng.one_in(20) {
			true => (40, MAX_HOPS as u64 - 3 + rng.below(4), 0),
			false => (6, 1 + rng.below(4), 10),
		};
		let mut path: Vec<Hop> = Vec::new();
		while (path.len() as u64) < len {
			let id = match looped > 0 && rng.one_in(looped) {
				true => ME,
				false => node(rng, relays),
			};
			if id != neighbour && path.iter().all(|hop| started(hop.node).0 != started(id).0) {
				let session = if path.is_empty() { 0 } else { 1 + rng.below(3) };
				path.push(Hop { node: id, session });
			}
		}
		Offer {
			path,
			draining: rng.one_in(4),
			priority: rng.pick(&PRIORITIES),
			cost: rng.below(12),
			marked: rng.one_in(3),
		}
	});
	Held {
		node: neighbour,
		session: 1 + rng.below(3),
		best,
		claimed: !rng.one_in(4),
	}
}

fn link(rng: &mut Rng) -> Link {
	Link {
		port: Port {
			cost: 1 + rng.below(4),
			priority: rng.pick(&PRIORITIES),
			upstream: rng.one_in(3),
			role: Role::Routing,
		},
		draining: rng.one_in(4),
		marked_there: rng.one_in(3),
		session: 1 + rng.below(3),
	}
}

fn state(rng: &mut Rng) -> State {
	let mut state = State::default();
	for _ in 0..rng.below(4) {
		let pair = (rng.below(7), rng.below(7));
		state.closed.insert(pair, (1 + rng.below(2), rng.below(4)));
	}
	for _ in 0..rng.below(3) {
		state.draining.insert(node(rng, 6));
	}
	state
}

/// One step of the algebra as the router takes it: the offer as the receiver would
/// send it on, or `None` where it is refused.
fn cross(state: &State, link: &Link, held: &Held) -> Option<Offer> {
	let sent = held.sent(link.marked_there)?;
	let (mut offer, suspect) = state.here().arrive(&link.port, link.draining, &sent)?;
	assert!(!suspect, "the picked rule suspects no one");
	offer.path.push(Hop {
		node: ME,
		session: link.session,
	});
	Some(offer)
}

/// The neighbour's key, as the simulation's routers name their peers.
fn via(node: Id) -> Node {
	started(node).0 as Node
}

/// The candidate the router picks, with no next hop in use.
fn pick(candidates: &[(Id, Option<Offer>)]) -> Option<Offer> {
	(candidates.iter())
		.filter_map(|(node, offer)| {
			let best = Best {
				via: via(*node),
				offer: offer.clone()?,
				suspect: false,
			};
			Some((order(&best, None), best.offer))
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
		let (held, link) = (held(&mut rng, neighbour), link(&mut rng));
		let Some(sent) = held.sent(link.marked_there) else {
			refused += 1;
			continue;
		};
		let Some(out) = cross(&state, &link, &held) else {
			refused += 1;
			continue;
		};
		// A path naming its receiver is refused, and one taken grows by exactly the
		// hop it crossed.
		assert!(!sent.path.iter().any(|hop| hop.node == ME), "{sent:?}");
		assert_eq!(out.path[..sent.path.len()], sent.path[..]);
		assert_eq!(out.path.len(), sent.path.len() + 1);
		// Drain and priority never improve, and the cost strictly rises.
		assert!(out.draining >= sent.draining && out.priority <= sent.priority);
		assert!(out.cost > sent.cost);
		assert!((out.draining, Reverse(out.priority), out.cost) > (sent.draining, Reverse(sent.priority), sent.cost));
		assert!(out.marked >= sent.marked);
	}
	assert!((2_000..18_000).contains(&refused), "{refused} of 20000 refused");
}

/// With `floor`, the dead hops a node keeps refuse nothing the floor does not, since
/// `closed` files every one of them in the floor too.
#[test]
fn the_floor_refuses_every_dead_hop() {
	let mut rng = Rng(0x2545_f491_4f6c_dd1d);
	for _ in 0..20_000 {
		let mut state = state(&mut rng);
		let neighbour = node(&mut rng, 6);
		let (held, link) = (held(&mut rng, neighbour), link(&mut rng));
		let Some(sent) = held.sent(link.marked_there) else {
			continue;
		};
		// Name closed some session on the path, as a DEAD message would, and file it
		// the way `closed` does.
		let mut floored = state.clone();
		for pair in sent.path.windows(2).filter(|_| rng.one_in(2)) {
			let (hop, far) = (pair[1], pair[0].node);
			floored.dead.insert(hop, far);
			let (key, at) = floor_of(KEPT, hop, far);
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
		if via(a) == via(b) {
			continue;
		}
		let one = cross(&state, &link(&mut rng), &held(&mut rng, a));
		let two = cross(&state, &link(&mut rng), &held(&mut rng, b));
		let (Some(one), Some(two)) = (one, two) else {
			continue;
		};
		let key = |node: Id, offer: &Offer| {
			order(
				&Best {
					via: via(node),
					offer: offer.clone(),
					suspect: false,
				},
				None,
			)
		};
		assert_ne!(key(a, &one), key(b, &two));
	}
}

/// Claims across a scoped grant take the same kind of step: the receiving node adds
/// the link's cost, takes the lower priority, keeps a drain, appends itself to the
/// chain, and refuses a claim whose chain names it.
#[test]
fn a_grant_never_improves_a_claim() {
	let mut rng = Rng(0xd1b5_4a32_d192_ed03);
	for _ in 0..2_000 {
		let mut router: Router<Node> = Router::new(ME, KEPT);
		let (from, to) = (1, 2);
		let port = Port {
			cost: 1 + rng.below(4),
			priority: rng.pick(&PRIORITIES),
			upstream: false,
			role: Role::Scoped,
		};
		router.connect(from, hop(from, 0), port);
		router.connect(to, hop(to, 0), Port { cost: 1, ..port });
		router.drain();
		let mut chain: Vec<Id> = (0..rng.below(3)).map(|_| node(&mut rng, 6)).collect();
		if rng.one_in(4) {
			chain.insert(rng.below(chain.len() as u64 + 1) as usize, ME);
		}
		let claim = Claim {
			cost: rng.below(12),
			priority: rng.pick(&PRIORITIES),
			draining: rng.one_in(4),
			chain,
		};
		let broadcast: multi::Broadcast = (7, p("live"));
		router.recv(
			from,
			Msg::Foreign {
				changes: vec![(broadcast.clone(), Change::Live(claim.clone()))],
			},
		);
		let sent: Vec<Claim> = (router.drain().into_iter())
			.filter(|(peer, _)| *peer == to)
			.flat_map(|(_, msg)| match msg {
				Msg::Foreign { changes } => changes,
				other => panic!("{other:?} to a scoped neighbour"),
			})
			.filter_map(|(_, change)| match change {
				Change::Live(claim) => Some(claim),
				_ => None,
			})
			.collect();
		if claim.chain.contains(&ME) {
			assert!(sent.is_empty(), "{sent:?}");
			continue;
		}
		let [out] = &sent[..] else {
			panic!("{sent:?}");
		};
		assert!(out.cost >= claim.cost + port.cost);
		assert_eq!(out.priority, claim.priority.min(port.priority));
		assert_eq!(out.draining, claim.draining);
		assert_eq!(out.chain[..claim.chain.len()], claim.chain[..]);
		assert_eq!(out.chain[claim.chain.len()..], [ME]);
	}
}

/// The Agda term for `offer` as a node sends it: its path from that node to the one
/// it leads to, each link with the session its nearer end holds it on.
fn weight(offer: Option<&Offer>) -> String {
	let Some(offer) = offer else {
		return "invalid".into();
	};
	let hops = &offer.path;
	let mut path = String::new();
	let mut sessions = String::new();
	for pair in hops.windows(2).rev() {
		write!(path, "({} , {}) ∷ ", pair[1].node, pair[0].node).unwrap();
		write!(sessions, "{} ∷ ", pair[1].session).unwrap();
	}
	format!(
		"valid (route {} {} {} ({path}[]) ({} , {sessions}[]))",
		offer.draining,
		penalty(offer.priority),
		offer.cost,
		offer.marked
	)
}

/// How far a priority sits below the highest.
fn penalty(priority: i8) -> i16 {
	i16::from(i8::MAX) - i16::from(priority)
}

fn agda_link(state: &State, link: &Link, held: &Held) -> String {
	let mut floors = String::new();
	for ((near, far), (start, session)) in &state.closed {
		write!(floors, "floor {near} {far} {start} {session} ∷ ").unwrap();
	}
	let mut draining = String::new();
	for node in &state.draining {
		write!(draining, "{node} ∷ ").unwrap();
	}
	format!(
		"link true {} {} {} ({draining}[]) ({floors}[]) {} {} {} {}",
		link.port.cost,
		penalty(link.port.priority),
		link.draining,
		link.port.upstream,
		link.marked_there,
		held.claimed,
		link.session,
	)
}

fn agda_step(state: &State, link: &Link, held: &Held) -> String {
	// The neighbour's route as the algebra holds it: before its own mark filter,
	// which the link applies.
	let route = match &held.best {
		None => Some(Offer::own(held.node, KEPT, true).expect("own route")),
		Some(best) => {
			let mut offer = best.clone();
			offer.path.push(Hop {
				node: held.node,
				session: held.session,
			});
			Some(offer)
		}
	};
	format!(
		"extend ({}) ({ME} , {}) ({})",
		agda_link(state, link, held),
		held.node,
		weight(route.as_ref())
	)
}

/// The sample: single steps, then picks between two neighbours' offers.
fn vectors() -> String {
	let mut out = String::from(
		"-- Generated by `rs/moq-net/src/route/tests/algebra.rs` (`ROUTE_VECTORS=write`); do not edit.\n\
		 -- Each case is a result the Rust prototype computed, which the algebra must compute too.\n\n\
		 module Vectors where\n\n\
		 open import Data.Bool.Base using (true; false)\n\
		 open import Data.List.Base using ([]; _∷_)\n\
		 open import Data.Product.Base using (_,_)\n\
		 open import Relation.Binary.PropositionalEquality using (_≡_; refl)\n\n\
		 open import Sim\n",
	);
	let mut rng = Rng(0x0123_4567_89ab_cdef);
	for _ in 0..200 {
		let state = state(&mut rng);
		let neighbour = node(&mut rng, 6);
		let (held, link) = (held(&mut rng, neighbour), link(&mut rng));
		let result = cross(&state, &link, &held);
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
		if via(a) == via(b) {
			continue;
		}
		let one = (held(&mut rng, a), link(&mut rng));
		// Half the pairs offer the same route over the same kind of link, so only the
		// neighbour breaks the tie.
		let two = match rng.one_in(2) {
			true => (Held { node: b, ..one.0.clone() }, one.1.clone()),
			false => (held(&mut rng, b), link(&mut rng)),
		};
		let picked = pick(&[
			(a, cross(&state, &one.1, &one.0)),
			(b, cross(&state, &two.1, &two.0)),
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

#[test]
fn the_agda_sample_is_current() {
	let path = vectors_path();
	let now = vectors();
	if std::env::var("ROUTE_VECTORS").as_deref() == Ok("write") {
		std::fs::write(&path, &now).unwrap();
		return;
	}
	let checked_in = std::fs::read_to_string(&path).unwrap();
	assert!(
		checked_in == now,
		"{} is stale: rerun this test with ROUTE_VECTORS=write, then `just test route-proof`",
		path.display()
	);
}
