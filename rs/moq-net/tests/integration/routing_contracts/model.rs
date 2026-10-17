//! A reference model of the proposed per-origin route layer, small enough to read.
//!
//! It is not the implementation. It exists so the routing contract in
//! `quest/m1/cluster-routing/routes.md` has deterministic counterexamples before
//! the lite-07 wire is fixed: every rule the contract depends on is a switch
//! here, and each fixture runs a scenario under the rule that fails and the rule
//! that holds.
//!
//! Routing is Babel (RFC 8966) keyed by routing identity: each node selects one
//! feasible route per identity, advertises it to every neighbor whose session is
//! up, and only the identity's own node advances its seqno. Messages travel in
//! order on each directed session and can be held to replay them late.
//!
//! Claims come in two forms. The forwarding fixtures use global claims (an
//! identity serving a prefix, at a seed cost and optional epoch) known
//! everywhere unless a fixture hides one from a node, which stands in for
//! scoping, interest, or a claim that has not arrived yet. The handoff fixtures
//! use announcements instead: an origin versions its own claims, and they travel
//! with its route as certified views that each node merges and shows.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, VecDeque},
	iter,
};

pub type Node = usize;

/// An opaque routing identity: the hash of a per-process key, or [`qualified`].
pub type Id = u64;

/// Set on every [`qualified`] identity and never on a key-derived one, so any
/// relay recognizes a qualified key.
const QUALIFIED: Id = 1 << 63;

/// A route metric: the sum of the configured link costs.
pub type Metric = u64;

/// An origin's claim revision: it advances on every change to the origin's own
/// claims, starting from zero with each routing identity.
pub type Rev = u64;

/// An origin's route seqno.
pub type Seqno = u64;

/// The largest lite-07 varint, which carries both a seqno and a revision. An
/// origin never advertises it: reaching it rotates the identity instead, so
/// every advertised seqno leaves room for the successor a request asks for.
pub const VARINT_MAX: u64 = u64::MAX;

/// How link costs accumulate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
	/// Today's single cost: any link cost, saturating at the ceiling, so a
	/// zero-cost link or a saturated total does not increase the metric.
	Saturating { ceiling: Metric },
	/// Strictly positive link costs with checked addition: a total beyond the
	/// ceiling is unreachable rather than capped.
	Checked { ceiling: Metric },
}

impl Rule {
	/// The checked rule up to the largest QUIC varint.
	pub const CHECKED: Self = Self::Checked { ceiling: (1 << 62) - 1 };

	fn admits(self, cost: Metric) -> bool {
		match self {
			Self::Saturating { .. } => true,
			Self::Checked { .. } => cost > 0,
		}
	}

	/// The metric after adding `cost`, or `None` when unreachable.
	fn add(self, metric: Metric, cost: Metric) -> Option<Metric> {
		match self {
			Self::Saturating { ceiling } => Some(metric.saturating_add(cost).min(ceiling)),
			Self::Checked { ceiling } => metric.checked_add(cost).filter(|metric| *metric <= ceiling),
		}
	}
}

/// What a node may advertise to an upstream link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
	/// No restriction: every link transits.
	Anything,
	/// One selected route per identity, carrying a down-only bit set when it was
	/// learned on an upstream link or from a down-only advertisement, and never
	/// sent to an upstream link.
	DownOnly,
}

/// The cross-origin tie-break after cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TieBreak {
	/// Hash the path and the serving identity: every node agrees.
	PathIdentity,
	/// Hash the path, identity, next hop, and this node, standing in for
	/// today's hash over a hop chain that differs per node: nodes may disagree.
	PathHere,
}

/// How a node adopts the announcements its neighbors send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
	/// Next-hop authority: show what the selected next hop has sent so far, and
	/// nothing while there is no selected route.
	NextHop,
	/// A snapshot barrier alone: keep the last adopted view until the new next
	/// hop's complete view arrives, then adopt it whole.
	Barrier,
	/// Merge every neighbor's certified view, claim by claim, keeping whichever
	/// knowledge is at the newer origin revision. A view's floors certify only the
	/// claims overlapping their region; `scoped: false` lets any floor of the
	/// origin vouch for any claim.
	Floors { scoped: bool },
}

/// How a starved node gets its origin to advance the seqno.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requests {
	/// No seqno requests: fixtures advance the origin by hand.
	Off,
	/// The earlier sketch: a starved node sends a request when it starves and on
	/// each [`Net::tick`] up to this many times, then gives up until an infeasible
	/// update arrives. Forwarders keep no state.
	Retries(u32),
	/// Every node holding a request, its own or forwarded, keeps it until a route
	/// at least that new is selected or no route is left, sends it to its next
	/// hop even when that is a requester, and sends it again whenever that
	/// neighbor changes. No timers.
	HopByHop,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
	pub rule: Rule,
	pub export: Export,
	pub tie: TieBreak,
	/// A relay that accepted a request from an upstream link ends it once its
	/// route turns down-only. Without it the relay keeps following that route.
	pub recheck: bool,
	/// A scoped connection may forward an already qualified identity, which
	/// the receiver qualifies again by its own subject. Otherwise that route is
	/// refused.
	pub requalify: bool,
	pub freshness: Freshness,
	pub requests: Requests,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			rule: Rule::CHECKED,
			export: Export::DownOnly,
			tie: TieBreak::PathIdentity,
			recheck: true,
			requalify: false,
			freshness: Freshness::Floors { scoped: true },
			requests: Requests::Off,
		}
	}
}

/// One side's configuration of a link to a neighbor.
#[derive(Clone, Copy, Debug)]
pub struct Link {
	pub cost: Metric,
	/// The application's local priority for this connection; higher wins.
	pub priority: i8,
	/// This side marks the neighbor upstream.
	pub upstream: bool,
	/// This side admitted the neighbor under a scoped publish grant: an
	/// identity the neighbor forwards, other than its own, is filed under
	/// [`qualified`] by the grant's subject, and claims count under that key
	/// only inside the grant's scope.
	pub grant: Option<Grant>,
}

impl Default for Link {
	fn default() -> Self {
		Self {
			cost: 1,
			priority: 0,
			upstream: false,
			grant: None,
		}
	}
}

impl Link {
	pub fn cost(cost: Metric) -> Self {
		Self {
			cost,
			..Self::default()
		}
	}

	pub fn priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	pub fn upstream(mut self) -> Self {
		self.upstream = true;
		self
	}

	pub fn grant(mut self, grant: Grant) -> Self {
		self.grant = Some(grant);
		self
	}
}

/// A scoped publish grant: the credential subject that qualifies forwarded
/// identities, and the prefix standing in for the grant's pattern union.
#[derive(Clone, Copy, Debug)]
pub struct Grant {
	pub subject: &'static str,
	pub scope: &'static str,
}

/// What a node advertises for one identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Advert {
	seqno: Seqno,
	metric: Metric,
	down_only: bool,
}

/// A ROUTE's state on a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
	/// A usable route.
	Finite(Advert),
	/// An infinite metric: unusable, but the sender still holds the identity's
	/// announcements while it waits for recovery. Only follows a finite route.
	Held,
}

/// One message on a directed session.
#[derive(Clone, Debug)]
enum Msg {
	/// ROUTE_START or ROUTE_UPDATE, or `None` for ROUTE_END.
	Route { id: Id, reach: Option<Reach> },
	/// A batch of ANNOUNCEs closed by its floor: the sender's complete view of
	/// this identity's claims within the receiver's interest. `interest` names
	/// the interest it answers, as the request a stream belongs to does.
	Claims { id: Id, view: View, interest: u32 },
	/// ROUTE_REQUEST, toward the neighbor whose route the sender would use.
	Request { id: Id, seqno: Seqno },
}

/// A route a node holds from one neighbor.
#[derive(Clone, Copy, Debug)]
struct Entry {
	/// The last finite advertisement.
	advert: Advert,
	held: bool,
}

/// The route a node selected for an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selected {
	/// The neighbor it goes through, or `None` when this node is the identity.
	pub next: Option<Node>,
	pub seqno: Seqno,
	/// This node's metric.
	pub metric: Metric,
	/// The metric the next hop advertised.
	pub advertised: Metric,
	/// Whether an upstream link is on the path (only under [`Export::DownOnly`]).
	pub down_only: bool,
}

/// A seqno request a node keeps.
#[derive(Clone, Debug)]
struct Pending {
	seqno: Seqno,
	/// Neighbors it forwards the request for; empty when only its own.
	from: BTreeSet<Node>,
	/// The neighbor it last sent the request to.
	asked: Option<Node>,
	/// Resends left under [`Requests::Retries`].
	retries: u32,
}

/// One announced claim: the attributes the wire carries, plus the revision its
/// life began at so fixtures can tell a revival from a fresh announcement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
	pub seed: Metric,
	pub epoch: Option<u32>,
	pub since: Rev,
}

/// What a node knows of one origin's claims: each region's floor certifies
/// that the items overlapping it are exactly the origin's claims there as of
/// that revision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
	floors: BTreeMap<&'static str, Rev>,
	items: BTreeMap<&'static str, Item>,
}

/// Whether a claim on `prefix` covers `path`: itself, or below it for a wildcard.
fn covers(prefix: &str, path: &str) -> bool {
	path == prefix || (prefix.ends_with('/') && path.starts_with(prefix))
}

/// Whether `path` is `prefix` or lies below it, matching whole path segments.
pub fn within(path: &str, prefix: &str) -> bool {
	path.strip_prefix(prefix)
		.is_some_and(|rest| prefix.is_empty() || prefix.ends_with('/') || rest.is_empty() || rest.starts_with('/'))
}

/// Whether `claim` can concern a path under `interest`: anything inside it, or
/// a wildcard above it.
pub fn overlaps(claim: &str, interest: &str) -> bool {
	within(claim, interest) || covers(claim, interest)
}

impl View {
	/// What a node keeps of its view once its interest is `watch`. A floor
	/// outside the interest may certify a wildcard above both at a newer
	/// revision than any floor inside, so it stays, with what it certifies,
	/// until a floor inside catches up; dropping it would leave that wildcard
	/// known only at an older revision than the item it holds. Once no
	/// neighbor certifies it, [`Net::prune`] lets it go.
	fn retire(&self, watch: &'static str) -> View {
		let mut kept = self.restrict(watch);
		let newest = kept.floors.values().max().copied();
		for (&region, &floor) in &self.floors {
			if Some(floor) > newest && !kept.floors.contains_key(region) {
				kept.floors.insert(region, floor);
				for (&claim, &item) in &self.items {
					if overlaps(claim, region) {
						kept.items.insert(claim, item);
					}
				}
			}
		}
		kept
	}

	/// The view as a session to a peer interested in `prefix` carries it.
	fn restrict(&self, prefix: &'static str) -> View {
		let mut floors = BTreeMap::new();
		for (&region, &floor) in &self.floors {
			// Two interest regions meet when either contains the other.
			if within(region, prefix) || within(prefix, region) {
				let region = if region.len() > prefix.len() { region } else { prefix };
				let entry = floors.entry(region).or_insert(floor);
				*entry = (*entry).max(floor);
			}
		}
		// An item stays only while a kept floor still certifies it.
		let items = self
			.items
			.iter()
			.filter(|(claim, _)| floors.keys().any(|region| overlaps(claim, region)))
			.map(|(claim, item)| (*claim, *item))
			.collect();
		View { floors, items }
	}

	/// The newest revision this view knows `claim` at, if any region covers it.
	fn as_of(&self, claim: &str, scoped: bool) -> Option<Rev> {
		self.floors
			.iter()
			.filter(|(region, _)| !scoped || overlaps(claim, region))
			.map(|(_, floor)| *floor)
			.max()
	}

	/// Take `other`'s state for every claim it knows at a newer revision, then
	/// raise the floors and drop any region a wider one at least as new covers.
	fn merge(&mut self, other: &View, scoped: bool) {
		let claims: BTreeSet<&'static str> = self.items.keys().chain(other.items.keys()).copied().collect();
		for claim in claims {
			if other.as_of(claim, scoped) > self.as_of(claim, scoped) {
				match other.items.get(claim) {
					Some(item) => self.items.insert(claim, *item),
					None => self.items.remove(claim),
				};
			}
		}
		for (&region, &floor) in &other.floors {
			let entry = self.floors.entry(region).or_insert(floor);
			*entry = (*entry).max(floor);
		}
		let floors = self.floors.clone();
		self.floors.retain(|region, floor| {
			!floors
				.iter()
				.any(|(wider, newer)| wider != region && within(region, wider) && newer >= floor)
		});
	}

	/// Estimated bytes to move a receiver watching `interest` from `old` to
	/// `self`: every floor and the changed or removed items, each with its
	/// path as a suffix of the interest and a varint or two.
	fn diff_bytes(&self, old: Option<&View>, interest: &str) -> (usize, usize) {
		let empty = View::default();
		let old = old.unwrap_or(&empty);
		// Every path overlaps the interest, so one not under it covers it and
		// goes out as an empty suffix. The separator after the interest is
		// model notation, not an encoded byte.
		let suffix = |path: &str| {
			if within(path, interest) {
				path[interest.len()..].trim_start_matches('/').len()
			} else {
				0
			}
		};
		// One ANNOUNCE_FLOOR lists every region, changed or not: a count, then a
		// suffix and a revision per region.
		let floors = 1 + self.floors.keys().map(|region| suffix(region) + 4).sum::<usize>();
		let changed: Vec<usize> = self
			.items
			.iter()
			.filter(|(claim, item)| old.items.get(*claim) != Some(item))
			.map(|(claim, _)| suffix(claim) + 8)
			.chain(
				old.items
					.keys()
					.filter(|claim| !self.items.contains_key(*claim))
					.map(|claim| suffix(claim)),
			)
			.collect();
		(changed.len(), 9 + floors + changed.iter().sum::<usize>())
	}
}

/// A path costs its suffix bytes however the interest above it is spelled.
#[test]
fn a_suffix_costs_the_same_however_its_interest_is_spelled() {
	let item = Item {
		seed: 0,
		epoch: None,
		since: 1,
	};
	let view = View {
		floors: BTreeMap::from([("live/a/x", 1)]),
		items: BTreeMap::from([("live/a/x", item)]),
	};
	// A batch header, then a region count, `x` and its revision, then `x` and its attributes.
	assert_eq!(view.diff_bytes(None, "live/a/"), (1, 9 + 1 + 5 + 9));
	assert_eq!(view.diff_bytes(None, "live/a"), (1, 9 + 1 + 5 + 9));
}

/// One change an origin made to its own claims.
struct Change {
	rev: Rev,
	claim: &'static str,
	item: Option<Item>,
}

/// How a node's reader sees one announced claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
	pub item: Item,
	/// Whether the node has a usable route to the claim's origin.
	pub available: bool,
}

/// A change a node's announcement reader sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	Start,
	/// The claim's attributes changed.
	Update,
	/// The node, still holding the origin, no longer shows the claim.
	End,
	/// The node let go of the origin: it has no usable or recovering route left.
	Offline,
	/// The node stopped watching the claim.
	Unwatched,
	Available,
	Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
	pub node: Node,
	pub id: Id,
	pub claim: &'static str,
	pub kind: Kind,
	pub since: Rev,
	/// How many times the node had restarted, and then forgotten this claim of
	/// this origin by collecting or retiring it, before this event.
	pub forgot: (u32, u32),
	/// Whether the origin had withdrawn or replaced this life of the claim by
	/// then. An `End` without it is a false END.
	pub withdrawn: bool,
}

/// Control traffic and work, as estimates: these are protocol counts in
/// virtual time, not runtime benchmarks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
	pub route_msgs: usize,
	pub claim_msgs: usize,
	/// Changed or removed items carried by claim batches: the receivers' replay work.
	pub claim_items: usize,
	pub claim_bytes: usize,
	pub requests: usize,
}

/// State kept for announcements, summed over every node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retained {
	/// Items in each node's own merged or adopted views.
	pub items: usize,
	/// Region floors in those views.
	pub floors: usize,
	/// Items in the per-session views a receiver keeps to apply the next batch.
	pub heard: usize,
	/// Region floors in those per-session views.
	pub heard_floors: usize,
	/// Items in the per-session views a sender keeps to diff the next batch.
	pub sent: usize,
	/// Region floors in those per-session views.
	pub sent_floors: usize,
	/// ROUTE_ENDs sent and not yet echoed back.
	pub echoes: usize,
	pub pending: usize,
	/// Feasibility distances for identities a node does not serve.
	pub distances: usize,
	/// Regions certified since an interest widened, still awaiting the rest.
	pub certified: usize,
}

#[derive(Default)]
struct State {
	/// The identity this node's sessions prove, if it originates one.
	proven: Option<Id>,
	/// Identities this node serves itself, with their seqnos: its proven one,
	/// plus any it forges.
	origins: BTreeMap<Id, Seqno>,
	/// Whether this node advertises identities it does not serve.
	transit: bool,
	rx: BTreeMap<Id, BTreeMap<Node, Entry>>,
	/// For a key this node qualified, the identity the neighbor named.
	filed: BTreeMap<(Node, Id), Id>,
	fd: BTreeMap<Id, (Seqno, Metric)>,
	sent: BTreeMap<(Node, Id), Reach>,
	/// ROUTE_ENDs sent to each neighbor that it has not echoed back yet. Until
	/// they all return, a neighbor may still hold this node's advertisement,
	/// so the feasibility distance stays.
	unacked: BTreeMap<(Node, Id), u32>,
	pending: BTreeMap<Id, Pending>,
	/// Identities whose own request [`Requests::Retries`] gave up on.
	gave_up: BTreeSet<Id>,
	/// The prefix this node asks its neighbors for announcements under.
	watch: &'static str,
	/// Counts this node's interest changes.
	interest: u32,
	/// Each origin's claims as this node knows them; authoritative for its own.
	views: BTreeMap<Id, View>,
	/// After the interest widened, the regions each view is known current in:
	/// the old interest, plus what each batch answering the new one certified.
	/// The rest is hidden until certified.
	current: BTreeMap<Id, Vec<&'static str>>,
	/// The last view each neighbor sent, per identity.
	heard: BTreeMap<(Node, Id), View>,
	/// The interest stream each heard view arrived on.
	heard_on: BTreeMap<(Node, Id), u32>,
	sent_views: BTreeMap<(Node, Id), View>,
	shown: BTreeMap<(Id, &'static str), Shown>,
	/// How many times this node restarted.
	restarts: u32,
	/// Per claim this node has shown, how many times it forgot that claim, so a
	/// stale replay is excused only for a claim it actually forgot. Fixture
	/// bookkeeping, not relay state: it labels reader events.
	forgot: BTreeMap<(Id, &'static str), u32>,
}

/// Counts a forget against each shown claim of `id` overlapping none of `kept`.
fn forget(forgot: &mut BTreeMap<(Id, &'static str), u32>, id: Id, kept: &[&str]) {
	for ((key, claim), count) in forgot.iter_mut() {
		if *key == id && !kept.iter().any(|region| overlaps(claim, region)) {
			*count += 1;
		}
	}
}

/// An identity serving everything under a prefix.
#[derive(Clone, Copy, Debug)]
pub struct Claim {
	pub id: Id,
	/// A trailing `/` claims the subtree; otherwise only the exact path.
	pub prefix: &'static str,
	pub seed: Metric,
	pub epoch: Option<u32>,
}

impl Claim {
	fn covers(&self, path: &str) -> bool {
		covers(self.prefix, path)
	}
}

/// How a transit hop treats a request it forwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forward {
	/// Every hop reselects among all origins; optionally never back to the requester.
	PathOnly { exclude_requester: bool },
	/// No wire change: a transit hop forwards only to a candidate whose
	/// advertised total is strictly below its own best total (RFC 5286's
	/// downstream condition).
	Downhill,
	/// The first hop names the identity; later hops follow their route to it.
	Pinned,
	/// The first hop names a bound; later hops reselect within it.
	Bounded,
}

/// What a forwarded request carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Carried {
	pinned: Option<Id>,
	bound: Option<Metric>,
	epoch: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
	Served { by: Id, path: Vec<Node> },
	Loop { path: Vec<Node> },
	Stalled { path: Vec<Node> },
}

/// Which existing upstream subscription a forwarded request may join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
	/// Any subscription for the path, as today.
	Path,
	/// One serving the same explicit epoch.
	Epoch,
	/// One following a route to the same identity.
	Identity,
	/// One whose total is within the request's bound.
	Bound,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
	id: Id,
	next: Option<Node>,
	seed: Metric,
	total: Metric,
	advertised: Metric,
	draining: bool,
	priority: i8,
	prefix: usize,
	epoch: Option<u32>,
	down_only: bool,
}

#[derive(Clone, Copy, Debug)]
enum Up {
	Serve,
	Stalled,
	Pending(Node, Node, Carried),
	Joined(usize),
}

#[derive(Clone, Copy, Debug)]
struct Front {
	node: Node,
	id: Id,
	epoch: Option<u32>,
	total: Metric,
	/// Whether the route it follows is down-only.
	down_only: bool,
	up: Up,
}

pub struct Net {
	cfg: Config,
	nodes: Vec<State>,
	links: BTreeMap<(Node, Node), Link>,
	up: BTreeSet<(Node, Node)>,
	/// `(node, peer)`: the identity `node` proved on its session to `peer`. A
	/// session proves one key, once.
	proved: BTreeMap<(Node, Node), Id>,
	/// `(at, peer)`: `peer` sent `at` a GOAWAY on their current session.
	draining: BTreeSet<(Node, Node)>,
	queues: BTreeMap<(Node, Node), VecDeque<Msg>>,
	/// ROUTE_ENDs echoed back, on the stream of the route they end: unordered
	/// against the echoing node's own routes in `queues`.
	acks: BTreeMap<(Node, Node), VecDeque<Id>>,
	held: BTreeSet<(Node, Node)>,
	claims: Vec<Claim>,
	hidden: BTreeSet<(Node, usize)>,
	/// Every change each origin made to its own claims, in revision order.
	history: BTreeMap<Id, Vec<Change>>,
	pub stats: Stats,
	pub events: Vec<Event>,
}

impl Net {
	pub fn new(cfg: Config, nodes: usize) -> Self {
		Self {
			cfg,
			nodes: (0..nodes)
				.map(|_| State {
					transit: true,
					..State::default()
				})
				.collect(),
			links: BTreeMap::new(),
			up: BTreeSet::new(),
			proved: BTreeMap::new(),
			draining: BTreeSet::new(),
			queues: BTreeMap::new(),
			acks: BTreeMap::new(),
			held: BTreeSet::new(),
			claims: Vec::new(),
			hidden: BTreeSet::new(),
			history: BTreeMap::new(),
			stats: Stats::default(),
			events: Vec::new(),
		}
	}

	/// A node that advertises only identities it serves, like a browser.
	pub fn leaf(&mut self, node: Node) {
		self.nodes[node].transit = false;
	}

	/// Configure both sides of a link and open its session.
	pub fn link(&mut self, a: Node, b: Node, ab: Link, ba: Link) {
		let rule = self.cfg.rule;
		assert!(
			rule.admits(ab.cost) && rule.admits(ba.cost),
			"zero-cost link {a}-{b} refused"
		);
		self.links.insert((a, b), ab);
		self.links.insert((b, a), ba);
		self.connect(a, b);
	}

	/// A symmetric link of `cost`.
	pub fn join(&mut self, a: Node, b: Node, cost: Metric) {
		self.link(a, b, Link::cost(cost), Link::cost(cost));
	}

	fn pair(a: Node, b: Node) -> (Node, Node) {
		(a.min(b), a.max(b))
	}

	/// Open a session; each side replays what it currently advertises.
	pub fn connect(&mut self, a: Node, b: Node) {
		assert!(self.links.contains_key(&(a, b)), "unknown link {a}-{b}");
		self.up.insert(Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			if let Some(id) = self.nodes[x].proven {
				self.proved.insert((x, y), id);
			}
		}
		self.advertise(a);
		self.advertise(b);
	}

	pub fn connected(&self, a: Node, b: Node) -> bool {
		self.up.contains(&Self::pair(a, b))
	}

	/// Close a session: in-flight messages and everything learned on it go.
	pub fn disconnect(&mut self, a: Node, b: Node) {
		self.up.remove(&Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			self.queues.remove(&(x, y));
			self.acks.remove(&(x, y));
			self.proved.remove(&(x, y));
			self.held.remove(&(x, y));
			self.draining.remove(&(y, x));
			let state = &mut self.nodes[y];
			for routes in state.rx.values_mut() {
				routes.remove(&x);
			}
			state.rx.retain(|_, routes| !routes.is_empty());
			state.filed.retain(|(from, _), _| *from != x);
			state.heard.retain(|(from, _), _| *from != x);
			state.heard_on.retain(|(from, _), _| *from != x);
			for pending in state.pending.values_mut() {
				pending.from.remove(&x);
			}
			let state = &mut self.nodes[x];
			// A closed session is an implicit echo: it held nothing past the close.
			state.unacked.retain(|(n, _), _| *n != y);
			state.sent.retain(|(n, _), _| *n != y);
			state.sent_views.retain(|(n, _), _| *n != y);
		}
		self.advertise(a);
		self.advertise(b);
	}

	/// The process at `node` exits: its sessions close and its state is lost.
	pub fn kill(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.disconnect(node, peer);
		}
		let old = std::mem::take(&mut self.nodes[node]);
		self.nodes[node] = State {
			transit: old.transit,
			watch: old.watch,
			interest: old.interest,
			restarts: old.restarts + 1,
			..State::default()
		};
		self.show(node);
	}

	/// Re-open every configured link of `node`.
	pub fn reconnect(&mut self, node: Node) {
		let peers: Vec<Node> = self.links.keys().filter(|(a, _)| *a == node).map(|(_, b)| *b).collect();
		for peer in peers {
			self.connect(node, peer);
		}
	}

	fn peers(&self, node: Node) -> impl Iterator<Item = Node> + '_ {
		self.up.iter().filter_map(move |&(a, b)| match node {
			n if n == a => Some(b),
			n if n == b => Some(a),
			_ => None,
		})
	}

	/// `node` starts originating `id` at seqno `seqno`, proving it on every session.
	pub fn originate(&mut self, node: Node, id: Id, seqno: Seqno) {
		assert_eq!(id & QUALIFIED, 0, "a key-derived identity is never qualified");
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			let proved = *self.proved.entry((node, peer)).or_insert(id);
			assert_eq!(proved, id, "node {node}'s session to {peer} already proved another key");
		}
		let state = &mut self.nodes[node];
		state.proven = Some(id);
		state.origins.insert(id, seqno);
		state.views.insert(
			id,
			View {
				floors: BTreeMap::from([("", 0)]),
				items: BTreeMap::new(),
			},
		);
		self.advertise(node);
	}

	/// `node` advertises `id` as if forwarding it, without proving it.
	pub fn forge(&mut self, node: Node, id: Id, seqno: Seqno) {
		self.nodes[node].origins.insert(id, seqno);
		self.advertise(node);
	}

	/// The origin advances its seqno.
	pub fn bump(&mut self, node: Node) {
		let state = &mut self.nodes[node];
		let id = state.proven.expect("not an origin");
		let seqno = state.origins.get_mut(&id).unwrap();
		if *seqno + 1 == VARINT_MAX {
			return self.rotate(node);
		}
		*seqno += 1;
		self.advertise(node);
	}

	/// The origin at `node` retires its identity for a fresh one and announces
	/// its claims again there: what it does instead of advancing a seqno or a
	/// revision to [`VARINT_MAX`]. A session proves one key once, so every
	/// session closes and reopens to prove the new one, and every node lets the
	/// old identity go.
	fn rotate(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for &peer in &peers {
			self.disconnect(node, peer);
		}
		let state = &mut self.nodes[node];
		let old = state.proven.expect("not an origin");
		state.origins.remove(&old);
		let claims = state.views.remove(&old).map(|view| view.items).unwrap_or_default();
		self.originate(node, fnv("rotate", &[old]) & !QUALIFIED, 0);
		for (claim, item) in claims {
			self.change(node, claim, Some((item.seed, item.epoch)));
		}
		for peer in peers {
			self.connect(node, peer);
		}
	}

	/// Jump the origin's revision to `rev`, as a long-lived origin's claim
	/// changes reach it.
	pub fn age(&mut self, node: Node, rev: Rev) {
		let id = self.proven(node);
		self.nodes[node].views.get_mut(&id).unwrap().floors.insert("", rev);
	}

	/// The identity `node` proves.
	pub fn proven(&self, node: Node) -> Id {
		self.nodes[node].proven.expect("not an origin")
	}

	/// The prefix `node` watches.
	pub fn watching(&self, node: Node) -> &'static str {
		self.nodes[node].watch
	}

	/// The origin's current seqno.
	pub fn seqno(&self, node: Node) -> Seqno {
		let state = &self.nodes[node];
		state.origins[&state.proven.expect("not an origin")]
	}

	/// The origin at `node` starts or updates a claim, advancing its revision.
	pub fn announce(&mut self, node: Node, claim: &'static str, seed: Metric, epoch: Option<u32>) {
		self.change(node, claim, Some((seed, epoch)));
	}

	/// The origin at `node` withdraws a claim, advancing its revision.
	pub fn unannounce(&mut self, node: Node, claim: &'static str) {
		self.change(node, claim, None);
	}

	fn change(&mut self, node: Node, claim: &'static str, attrs: Option<(Metric, Option<u32>)>) {
		let id = self.nodes[node].proven.expect("not an origin");
		let view = self.nodes[node].views.get_mut(&id).unwrap();
		let rev = view.floors[""] + 1;
		if rev == VARINT_MAX {
			self.rotate(node);
			return self.change(node, claim, attrs);
		}
		view.floors.insert("", rev);
		let item = attrs.map(|(seed, epoch)| Item {
			seed,
			epoch,
			since: view.items.get(claim).map_or(rev, |item| item.since),
		});
		match item {
			Some(item) => view.items.insert(claim, item),
			None => view.items.remove(claim),
		};
		self.history.entry(id).or_default().push(Change { rev, claim, item });
		self.advertise(node);
	}

	/// The prefix `node` asks its neighbors for announcements under.
	///
	/// A new interest is a new stream: the old stream's batches still in flight
	/// are discarded, and each neighbor starts the new stream with a complete
	/// snapshot, which retires what the node knew outside the new interest.
	pub fn watch(&mut self, node: Node, prefix: &'static str) {
		let state = &mut self.nodes[node];
		if !within(prefix, state.watch) {
			let old = state.watch;
			for id in state.views.keys().filter(|id| !state.origins.contains_key(id)) {
				state.current.entry(*id).or_insert_with(|| vec![old]);
			}
		}
		state
			.current
			.retain(|_, regions| !regions.iter().any(|region| within(prefix, region)));
		state.watch = prefix;
		state.interest += 1;
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.nodes[peer].sent_views.retain(|(to, _), _| *to != node);
			self.advertise(peer);
		}
		self.advertise(node);
	}

	/// `node` sends GOAWAY on every session: each peer ranks it below every
	/// other eligible route while the session stays up.
	pub fn goaway(&mut self, node: Node) {
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.draining.insert((peer, node));
			self.advertise(peer);
		}
	}

	/// Forget `id`'s routes and announcements at `node`. The feasibility
	/// distance goes on its own once drained (see [`Net::advertise`]).
	pub fn collect(&mut self, node: Node, id: Id) {
		let state = &mut self.nodes[node];
		state.rx.remove(&id);
		state.filed.retain(|(_, key), _| *key != id);
		state.views.remove(&id);
		state.current.remove(&id);
		state.heard.retain(|(_, key), _| *key != id);
		state.heard_on.retain(|(_, key), _| *key != id);
		state.pending.remove(&id);
		state.gave_up.remove(&id);
		forget(&mut state.forgot, id, &[]);
		self.advertise(node);
	}

	/// Whether every neighbor has processed `node`'s retraction of `id`: none
	/// holds an entry from it and no route message from it is in flight. The
	/// echoed ROUTE_ENDs tell `node` this; a restart gets it by
	/// closing every session.
	pub fn retracted(&self, node: Node, id: Id) -> bool {
		self.peers(node).all(|peer| {
			let queued = self.queues.get(&(node, peer)).is_some_and(|queue| {
				queue
					.iter()
					.any(|msg| matches!(msg, Msg::Route { id: sent, .. } if *sent == id))
			});
			let held = self
				.key(peer, node, id)
				.and_then(|key| self.nodes[peer].rx.get(&key))
				.is_some_and(|routes| routes.contains_key(&node));
			!queued && !held
		})
	}

	/// Drop `node`'s feasibility distance for `id`.
	pub fn forget(&mut self, node: Node, id: Id) {
		self.nodes[node].fd.remove(&id);
	}

	pub fn claim(&mut self, id: Id, prefix: &'static str, seed: Metric, epoch: Option<u32>) -> usize {
		self.claims.push(Claim {
			id,
			prefix,
			seed,
			epoch,
		});
		self.claims.len() - 1
	}

	/// `node` does not know claim `index`.
	pub fn hide(&mut self, node: Node, index: usize) {
		self.hidden.insert((node, index));
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.held.remove(&(from, to));
	}

	/// Deliver one in-order message from `from` to `to`, even if held.
	pub fn deliver(&mut self, from: Node, to: Node) -> bool {
		let Some(msg) = self.queues.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};
		self.receive(to, from, msg);
		true
	}

	/// Deliver one ROUTE_END echoed back from `from` to `to`, even if held.
	pub fn deliver_ack(&mut self, from: Node, to: Node) -> bool {
		let Some(id) = self.acks.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};
		let state = &mut self.nodes[to];
		let left = state.unacked.get_mut(&(from, id)).expect("an echo answers a ROUTE_END");
		*left -= 1;
		if *left == 0 {
			state.unacked.remove(&(from, id));
		}
		self.advertise(to);
		true
	}

	/// The ROUTE_END echoes waiting from `from` to `to`.
	pub fn queued_acks(&self, from: Node, to: Node) -> usize {
		self.acks.get(&(from, to)).map_or(0, VecDeque::len)
	}

	/// Every directed session with an echo waiting, held or not.
	pub fn busy_acks(&self) -> Vec<(Node, Node)> {
		self.acks
			.iter()
			.filter(|(_, queue)| !queue.is_empty())
			.map(|(key, _)| *key)
			.collect()
	}

	/// Whether `node` keeps a feasibility distance for `id`.
	pub fn remembers(&self, node: Node, id: Id) -> bool {
		self.nodes[node].fd.contains_key(&id)
	}

	/// The messages waiting from `from` to `to`.
	pub fn queued(&self, from: Node, to: Node) -> usize {
		self.queues.get(&(from, to)).map_or(0, VecDeque::len)
	}

	/// Every directed session with a message waiting, held or not.
	pub fn busy(&self) -> Vec<(Node, Node)> {
		self.queues
			.iter()
			.filter(|(_, queue)| !queue.is_empty())
			.map(|(key, _)| *key)
			.collect()
	}

	/// Deliver round-robin until every unheld queue is empty. False if it never
	/// settles within a generous budget, which is how oscillation shows up.
	pub fn run(&mut self) -> bool {
		self.settle(10_000)
	}

	/// [`Net::run`] for at most `rounds` rounds.
	pub fn settle(&mut self, rounds: usize) -> bool {
		for _ in 0..rounds {
			let ready: Vec<(Node, Node)> = self
				.queues
				.iter()
				.filter(|(key, queue)| !queue.is_empty() && !self.held.contains(key))
				.map(|(key, _)| *key)
				.collect();
			let acks: Vec<(Node, Node)> = self
				.acks
				.iter()
				.filter(|(key, queue)| !queue.is_empty() && !self.held.contains(key))
				.map(|(key, _)| *key)
				.collect();
			if ready.is_empty() && acks.is_empty() {
				return true;
			}
			for (from, to) in ready {
				self.deliver(from, to);
			}
			for (from, to) in acks {
				self.deliver_ack(from, to);
			}
		}
		false
	}

	/// A resend timer fires everywhere: under [`Requests::Retries`], each node
	/// resends its own pending request, or gives up once its retries are spent.
	pub fn tick(&mut self) {
		if !matches!(self.cfg.requests, Requests::Retries(_)) {
			return;
		}
		for node in 0..self.nodes.len() {
			let own: Vec<Id> = self.nodes[node]
				.pending
				.iter()
				.filter(|(_, pending)| pending.from.is_empty())
				.map(|(id, _)| *id)
				.collect();
			for id in own {
				let pending = self.nodes[node].pending.get_mut(&id).unwrap();
				if pending.retries == 0 {
					self.nodes[node].pending.remove(&id);
					self.nodes[node].gave_up.insert(id);
					continue;
				}
				pending.retries -= 1;
				let seqno = pending.seqno;
				if let Some(target) = self.target(node, id, &BTreeSet::new()) {
					self.send_request(node, target, id, seqno);
				}
			}
		}
	}

	/// The key `at` files `id` under when `from` names it, or `None` when refused.
	fn key(&self, at: Node, from: Node, id: Id) -> Option<Id> {
		match self.links[&(at, from)].grant {
			Some(grant) if self.proved.get(&(from, at)) != Some(&id) => {
				if id & QUALIFIED != 0 && !self.cfg.requalify {
					return None;
				}
				Some(qualified(grant.subject, id))
			}
			_ => Some(id),
		}
	}

	fn receive(&mut self, at: Node, from: Node, msg: Msg) {
		match msg {
			Msg::Route { id: named, reach } => {
				// Echo every ROUTE_END once it is applied, filed or refused.
				if reach.is_none() {
					self.stats.route_msgs += 1;
					self.acks.entry((at, from)).or_default().push_back(named);
				}
				let Some(id) = self.key(at, from, named) else {
					return;
				};
				if id != named {
					match reach {
						Some(_) => self.nodes[at].filed.insert((from, id), named),
						None => self.nodes[at].filed.remove(&(from, id)),
					};
				}
				if let (Requests::Retries(_), Some(Reach::Finite(advert))) = (self.cfg.requests, reach)
					&& !self.feasible(at, id, advert)
				{
					self.nodes[at].gave_up.remove(&id);
				}
				let state = &mut self.nodes[at];
				let routes = state.rx.entry(id).or_default();
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
					state.rx.remove(&id);
				}
			}
			Msg::Claims { id, view, interest } => {
				let Some(id) = self.key(at, from, id) else {
					return;
				};
				let scope = match self.links[&(at, from)].grant {
					Some(grant) if id & QUALIFIED != 0 => Some(grant.scope),
					_ => None,
				};
				let view = match scope {
					Some(scope) => view.restrict(scope),
					None => view,
				};
				let state = &mut self.nodes[at];
				if interest != state.interest {
					// A batch from a stream the node has since replaced.
					return;
				}
				// The batch certifies the regions its floors name.
				let watch = state.watch;
				for &region in view.floors.keys() {
					let Some(regions) = state.current.get_mut(&id) else {
						break;
					};
					if region == watch {
						state.current.remove(&id);
					} else if !regions.iter().any(|known| within(region, known)) {
						regions.retain(|known| !within(known, region));
						regions.push(region);
					}
				}
				if let Freshness::Floors { scoped } = self.cfg.freshness
					&& !state.origins.contains_key(&id)
				{
					let merged = state.views.entry(id).or_default();
					merged.merge(&view, scoped);
					// The batch certifies the interest, so what lies outside can go.
					let kept = merged.retire(state.watch);
					if kept != *merged {
						*merged = kept;
						forget(&mut state.forgot, id, &[state.watch]);
					}
				}
				// The batch's floor list replaces the session's regions: one left
				// out is no longer certified, and an empty list certifies nothing.
				state.heard.insert((from, id), view);
				state.heard_on.insert((from, id), interest);
			}
			Msg::Request { id, seqno } => self.request_received(at, from, id, seqno),
		}
		self.advertise(at);
	}

	fn request_received(&mut self, at: Node, from: Node, id: Id, seqno: Seqno) {
		let state = &mut self.nodes[at];
		if let Some(own) = state.origins.get_mut(&id) {
			// Only the origin advances its seqno to answer.
			if seqno == VARINT_MAX {
				assert_eq!(state.proven, Some(id), "a forged identity ran out of seqnos");
				self.rotate(at);
			} else {
				*own = (*own).max(seqno);
			}
			return;
		}
		if self.route(at, id).is_some_and(|route| route.seqno >= seqno) {
			// Already advertised to the requester.
			return;
		}
		match self.cfg.requests {
			Requests::Off => {}
			Requests::Retries(_) => {
				if let Some(target) = self.target(at, id, &BTreeSet::from([from])) {
					self.send_request(at, target, id, seqno);
				}
			}
			Requests::HopByHop => {
				let pending = self.nodes[at].pending.entry(id).or_insert(Pending {
					seqno,
					from: BTreeSet::new(),
					asked: None,
					retries: 0,
				});
				if seqno > pending.seqno {
					pending.seqno = seqno;
					pending.asked = None;
				}
				pending.from.insert(from);
			}
		}
	}

	fn send_request(&mut self, node: Node, to: Node, id: Id, seqno: Seqno) {
		self.stats.requests += 1;
		let id = self.name(node, to, id);
		self.queues
			.entry((node, to))
			.or_default()
			.push_back(Msg::Request { id, seqno });
	}

	/// The identity `node` names for `id` in a request it sends `next`: the one
	/// `next` advertised, if `node` qualified it by `next`'s subject.
	fn name(&self, node: Node, next: Node, id: Id) -> Id {
		self.nodes[node].filed.get(&(next, id)).copied().unwrap_or(id)
	}

	/// How many qualified keys `node` remembers the forwarded name of.
	pub fn aliases(&self, node: Node) -> usize {
		self.nodes[node].filed.len()
	}

	/// How many identities `node` holds routes to.
	pub fn identities(&self, node: Node) -> usize {
		self.nodes[node].rx.len()
	}

	/// How many region floors `node` keeps for `id`.
	pub fn floors(&self, node: Node, id: Id) -> usize {
		self.nodes[node].views.get(&id).map_or(0, |view| view.floors.len())
	}

	/// Whether `node` holds any route state for `id`.
	pub fn holds(&self, node: Node, id: Id) -> bool {
		self.nodes[node].rx.contains_key(&id)
	}

	/// Babel's feasibility condition against `node`'s feasibility distance.
	fn feasible(&self, node: Node, id: Id, advert: Advert) -> bool {
		match self.nodes[node].fd.get(&id) {
			None => true,
			Some(&(seqno, metric)) => advert.seqno > seqno || (advert.seqno == seqno && advert.metric < metric),
		}
	}

	/// The route `node` selected for `id`.
	pub fn route(&self, node: Node, id: Id) -> Option<Selected> {
		self.route_except(node, id, &BTreeSet::new())
	}

	/// The best feasible usable route that avoids every neighbor in `except`.
	fn route_except(&self, node: Node, id: Id, except: &BTreeSet<Node>) -> Option<Selected> {
		let state = &self.nodes[node];
		if let Some(&seqno) = state.origins.get(&id) {
			return Some(Selected {
				next: None,
				seqno,
				metric: 0,
				advertised: 0,
				down_only: false,
			});
		}
		state
			.rx
			.get(&id)?
			.iter()
			.filter(|(next, entry)| !entry.held && !except.contains(next) && self.feasible(node, id, entry.advert))
			.filter_map(|(&next, entry)| {
				let advert = entry.advert;
				let link = self.links[&(node, next)];
				let metric = self.cfg.rule.add(advert.metric, link.cost)?;
				let selected = Selected {
					next: Some(next),
					seqno: advert.seqno,
					metric,
					advertised: advert.metric,
					down_only: self.cfg.export == Export::DownOnly && (link.upstream || advert.down_only),
				};
				let draining = self.draining.contains(&(node, next));
				Some(((draining, Reverse(link.priority), metric, next), selected))
			})
			.min_by_key(|(rank, _)| *rank)
			.map(|(_, selected)| selected)
	}

	/// The usable but infeasible routes `node` holds for `id`, best first.
	fn infeasible(&self, node: Node, id: Id) -> Vec<Node> {
		let Some(routes) = self.nodes[node].rx.get(&id) else {
			return Vec::new();
		};
		let mut out: Vec<(Metric, Node)> = routes
			.iter()
			.filter(|(_, entry)| !entry.held && !self.feasible(node, id, entry.advert))
			.filter_map(|(&next, entry)| {
				let metric = self.cfg.rule.add(entry.advert.metric, self.links[&(node, next)].cost)?;
				Some((metric, next))
			})
			.collect();
		out.sort();
		out.into_iter().map(|(_, next)| next).collect()
	}

	/// Where `node` sends a request for `id` on behalf of `from`: a feasible
	/// route's next hop if one avoids them, else an infeasible route's (RFC 8966
	/// s3.8.1.2).
	fn target(&self, node: Node, id: Id, from: &BTreeSet<Node>) -> Option<Node> {
		self.route_except(node, id, from)
			.and_then(|route| route.next)
			.or_else(|| self.infeasible(node, id).into_iter().find(|next| !from.contains(next)))
	}

	/// The route `node` would offer an upstream link for `id`.
	pub fn export_route(&self, node: Node, id: Id) -> Option<Selected> {
		self.route(node, id).filter(|route| !route.down_only)
	}

	/// The route `node` follows for `id` on behalf of a request from `from`.
	fn route_from(&self, node: Node, id: Id, from: Option<Node>) -> Option<Selected> {
		if self.exports_only(node, from) {
			self.export_route(node, id)
		} else {
			self.route(node, id)
		}
	}

	/// Whether `node` may follow only exportable routes for a request from `from`.
	fn exports_only(&self, node: Node, from: Option<Node>) -> bool {
		self.cfg.recheck && from.is_some_and(|from| self.links[&(node, from)].upstream)
	}

	/// Whether `node` holds a reachable route to `id` but none is feasible: it
	/// needs the origin to advance its seqno.
	pub fn starved(&self, node: Node, id: Id) -> bool {
		self.route(node, id).is_none() && !self.infeasible(node, id).is_empty()
	}

	/// Whether `node`, with no usable route to `id`, holds a held route it could
	/// recover through. A held route counts only if its last finite advertisement
	/// is feasible here, so holds follow a strictly decreasing metric toward a
	/// starved node and cannot hold each other up.
	pub fn grounded(&self, node: Node, id: Id) -> bool {
		self.route(node, id).is_none()
			&& self.nodes[node].rx.get(&id).is_some_and(|routes| {
				routes
					.values()
					.any(|entry| entry.held && self.feasible(node, id, entry.advert))
			})
	}

	/// Whether `node` keeps showing `id`'s announcements.
	pub fn live(&self, node: Node, id: Id) -> bool {
		self.route(node, id).is_some() || self.starved(node, id) || self.grounded(node, id)
	}

	/// The neighbors whose views can certify `id` at `node`: those whose last
	/// finite advertisement is feasible here, held or not. Feasible edges
	/// strictly decrease the metric toward the origin, so certification never
	/// circulates: two nodes cannot keep a region alive through each other.
	fn certifiers(&self, node: Node, id: Id) -> Vec<Node> {
		self.nodes[node].rx.get(&id).map_or_else(Vec::new, |routes| {
			routes
				.iter()
				.filter(|(_, entry)| self.feasible(node, id, entry.advert))
				.map(|(&next, _)| next)
				.collect()
		})
	}

	/// The regions of `id` that `node`'s certifiers certify, or `None` while it
	/// holds no route or a certifier has not yet answered the current interest:
	/// its announcements may still trail its route.
	fn certified(&self, node: Node, id: Id) -> Option<Vec<&'static str>> {
		let state = &self.nodes[node];
		state.rx.get(&id)?;
		let mut regions = Vec::new();
		for next in self.certifiers(node, id) {
			if state.heard_on.get(&(next, id)) != Some(&state.interest) {
				return None;
			}
			regions.extend(state.heard[&(next, id)].floors.keys());
		}
		Some(regions)
	}

	/// Let go of every region of `id`'s merged view that no routed neighbor
	/// certifies any more, once each has answered the current interest: its
	/// last certifier closed, ended its route, or no longer meets the interest.
	/// The reader sees that region go offline.
	fn prune(&mut self, node: Node, id: Id) {
		let Freshness::Floors { scoped } = self.cfg.freshness else {
			return;
		};
		// A starved or held node keeps presence until it recovers or lets go.
		if self.nodes[node].origins.contains_key(&id) || self.route(node, id).is_none() {
			return;
		}
		let Some(regions) = self.certified(node, id) else {
			return;
		};
		let state = &self.nodes[node];
		let Some(merged) = state.views.get(&id) else {
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
		// wildcard above both was last learned through the lost floor, and is
		// unknown at the revision the kept floors certify.
		let mut kept = View::default();
		for &region in &regions {
			kept.merge(&merged.restrict(region), scoped);
		}
		let (rebuilt, remembered) = if kept.floors.values().all(|&floor| floor >= lost) {
			(kept, regions)
		} else {
			let mut rebuilt = View::default();
			for next in self.certifiers(node, id) {
				rebuilt.merge(&self.nodes[node].heard[&(next, id)], scoped);
			}
			(rebuilt, Vec::new())
		};
		let state = &mut self.nodes[node];
		state.views.insert(id, rebuilt);
		forget(&mut state.forgot, id, &remembered);
	}

	/// The view `node` shows and forwards for `id`.
	fn adopted(&self, node: Node, id: Id) -> Option<&View> {
		let state = &self.nodes[node];
		if state.origins.contains_key(&id) {
			return state.views.get(&id);
		}
		match self.cfg.freshness {
			Freshness::NextHop => self
				.route(node, id)
				.and_then(|route| route.next)
				.and_then(|next| state.heard.get(&(next, id))),
			Freshness::Barrier | Freshness::Floors { .. } => {
				self.live(node, id).then(|| state.views.get(&id)).flatten()
			}
		}
	}

	/// What `node`'s announcement reader sees.
	pub fn shown(&self, node: Node) -> &BTreeMap<(Id, &'static str), Shown> {
		&self.nodes[node].shown
	}

	/// Whether `node` shows `claim` from `id`, and whether it is available.
	pub fn shows(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		self.nodes[node]
			.shown
			.iter()
			.find(|((shown, prefix), _)| *shown == id && *prefix == claim)
			.map(|(_, shown)| shown.available)
	}

	/// Recompute every selection at `node`, record feasibility distances for what
	/// it advertises, and queue any change for each neighbor.
	fn advertise(&mut self, node: Node) {
		let ids: Vec<Id> = self.nodes[node].views.keys().copied().collect();
		for id in ids {
			self.prune(node, id);
		}
		let state = &self.nodes[node];
		let mut ids: BTreeSet<Id> = state.rx.keys().copied().collect();
		ids.extend(state.origins.keys());
		ids.extend(state.sent.keys().map(|(_, id)| *id));
		let peers: Vec<Node> = self.peers(node).collect();

		for id in ids {
			let selected = self.route(node, id);
			if self.cfg.freshness == Freshness::Barrier
				&& let Some(next) = selected.and_then(|route| route.next)
				&& let Some(view) = self.nodes[node].heard.get(&(next, id))
			{
				let view = view.clone();
				self.nodes[node].views.insert(id, view);
			}
			// A leaf advertises only what it serves, and ends what it stopped serving.
			let serves = self.nodes[node].transit || self.nodes[node].origins.contains_key(&id);
			if !serves && !self.nodes[node].sent.keys().any(|(_, sent)| *sent == id) {
				continue;
			}
			let selected = selected.filter(|_| serves);
			// The feasibility distance tracks finite updates actually sent (RFC 8966 s3.7.3).
			if let Some(selected) = selected
				&& peers
					.iter()
					.any(|&peer| !(self.links[&(node, peer)].upstream && selected.down_only))
			{
				self.record(node, id, selected);
			}
			let live = serves && self.live(node, id);
			let view = self.adopted(node, id).cloned().unwrap_or_default();
			// A sender that certifies anything answers every stream, even one
			// whose interest none of its regions meets, so the receiver knows it
			// certifies nothing there.
			let answers = !view.floors.is_empty();
			for &peer in &peers {
				let upstream = self.links[&(node, peer)].upstream;
				let state = &self.nodes[node];
				let reach = match selected {
					Some(route) if !(upstream && route.down_only) => Some(Reach::Finite(Advert {
						seqno: route.seqno,
						metric: route.metric,
						down_only: route.down_only,
					})),
					None if live && state.sent.contains_key(&(peer, id)) => Some(Reach::Held),
					_ => None,
				};
				if state.sent.get(&(peer, id)).copied() != reach {
					let state = &mut self.nodes[node];
					match reach {
						Some(reach) => state.sent.insert((peer, id), reach),
						None => {
							*state.unacked.entry((peer, id)).or_default() += 1;
							state.sent.remove(&(peer, id))
						}
					};
					self.stats.route_msgs += 1;
					self.queues
						.entry((node, peer))
						.or_default()
						.push_back(Msg::Route { id, reach });
				}
				// Announcements ride on the route and end with it.
				let state = &mut self.nodes[node];
				if reach.is_none() {
					state.sent_views.remove(&(peer, id));
					continue;
				}
				// Each floor names its region: the stream's interest, or a scope below it.
				let watch = self.nodes[peer].watch;
				let view = view.restrict(watch);
				let state = &mut self.nodes[node];
				let unchanged = match state.sent_views.get(&(peer, id)) {
					Some(sent) => *sent == view,
					None => !answers,
				};
				if unchanged {
					continue;
				}
				let (items, bytes) = view.diff_bytes(state.sent_views.get(&(peer, id)), watch);
				state.sent_views.insert((peer, id), view.clone());
				self.stats.claim_msgs += 1;
				self.stats.claim_items += items;
				self.stats.claim_bytes += bytes;
				self.queues.entry((node, peer)).or_default().push_back(Msg::Claims {
					id,
					view,
					interest: self.nodes[peer].interest,
				});
			}
		}
		// Drop a feasibility distance once nothing here routes or advertises the
		// identity and every neighbor echoed its ROUTE_END: no node then holds an
		// advertisement from this one, so no older one can return as a loop.
		let state = &mut self.nodes[node];
		let busy: BTreeSet<Id> = state
			.rx
			.keys()
			.chain(state.origins.keys())
			.chain(state.sent.keys().map(|(_, id)| id))
			.chain(state.unacked.keys().map(|(_, id)| id))
			.copied()
			.collect();
		state.fd.retain(|id, _| busy.contains(id));
		self.requests(node);
		self.show(node);
	}

	/// Start, resend, or clear `node`'s seqno requests.
	fn requests(&mut self, node: Node) {
		if self.cfg.requests == Requests::Off {
			return;
		}
		let state = &self.nodes[node];
		let mut ids: BTreeSet<Id> = state.pending.keys().copied().collect();
		ids.extend(state.rx.keys().filter(|id| self.starved(node, **id)));
		for id in ids {
			let state = &self.nodes[node];
			if state.origins.contains_key(&id) {
				self.nodes[node].pending.remove(&id);
				continue;
			}
			let starved = self.starved(node, id);
			if starved {
				let want = state.fd[&id].0 + 1;
				match self.cfg.requests {
					Requests::HopByHop => {
						let pending = self.nodes[node].pending.entry(id).or_insert(Pending {
							seqno: want,
							from: BTreeSet::new(),
							asked: None,
							retries: 0,
						});
						if pending.seqno < want {
							pending.seqno = want;
							pending.asked = None;
						}
					}
					Requests::Retries(retries) => {
						if !state.pending.contains_key(&id) && !state.gave_up.contains(&id) {
							self.nodes[node].pending.insert(
								id,
								Pending {
									seqno: want,
									from: BTreeSet::new(),
									asked: None,
									retries,
								},
							);
						}
					}
					Requests::Off => unreachable!(),
				}
			}
			let Some(pending) = self.nodes[node].pending.get(&id).cloned() else {
				continue;
			};
			let answered = self.route(node, id).is_some_and(|route| route.seqno >= pending.seqno);
			// Hop by hop, a request follows the next hop even back through a
			// requester: next hops form no loop, one pending request per hop stops
			// a bounce, and no requester ever cancels, so excluding one can strand
			// a request while every route still runs through it.
			let except = match self.cfg.requests {
				Requests::HopByHop => BTreeSet::new(),
				_ => pending.from.clone(),
			};
			let target = self.target(node, id, &except);
			if answered || target.is_none() || (pending.from.is_empty() && !starved) {
				self.nodes[node].pending.remove(&id);
				continue;
			}
			let resend = match self.cfg.requests {
				Requests::HopByHop => target != pending.asked,
				_ => pending.asked.is_none(),
			};
			if resend {
				let target = target.unwrap();
				self.nodes[node].pending.get_mut(&id).unwrap().asked = Some(target);
				self.send_request(node, target, id, pending.seqno);
			}
		}
	}

	/// Recompute what `node`'s reader sees and log each change.
	fn show(&mut self, node: Node) {
		let state = &self.nodes[node];
		let mut ids: BTreeSet<Id> = state.views.keys().copied().collect();
		ids.extend(state.heard.keys().map(|(_, id)| *id));
		let mut shown = BTreeMap::new();
		for id in ids {
			let Some(view) = self.adopted(node, id) else {
				continue;
			};
			let usable = self.route(node, id).is_some();
			// A claim is available only through a certifier that is not held:
			// another scope's usable route cannot serve it.
			let serving: Option<Vec<&str>> = match self.cfg.freshness {
				Freshness::Floors { .. } if !state.origins.contains_key(&id) => self.certified(node, id).map(|_| {
					self.certifiers(node, id)
						.into_iter()
						.filter(|next| !state.rx[&id][next].held)
						.flat_map(|next| state.heard[&(next, id)].floors.keys().copied())
						.collect()
				}),
				_ => None,
			};
			let current = state.current.get(&id);
			for (&claim, &item) in &view.items {
				let certified = current.is_none_or(|regions| regions.iter().any(|region| overlaps(claim, region)));
				let available = usable
					&& serving
						.as_ref()
						.is_none_or(|regions| regions.iter().any(|region| overlaps(claim, region)));
				if overlaps(claim, state.watch) && certified {
					shown.insert((id, claim), Shown { item, available });
				}
			}
		}
		let old = std::mem::take(&mut self.nodes[node].shown);
		let mut log = Vec::new();
		for (&(id, claim), now) in &shown {
			match old.get(&(id, claim)) {
				None => log.push((id, claim, Kind::Start, now.item.since)),
				Some(was) => {
					if was.item != now.item {
						log.push((id, claim, Kind::Update, now.item.since));
					}
					if was.available != now.available {
						let kind = if now.available {
							Kind::Available
						} else {
							Kind::Unavailable
						};
						log.push((id, claim, kind, now.item.since));
					}
				}
			}
		}
		for (&(id, claim), was) in &old {
			if shown.contains_key(&(id, claim)) {
				continue;
			}
			// A claim no routed neighbor's region covers was let go, not ended.
			let uncertified = matches!(self.cfg.freshness, Freshness::Floors { .. })
				&& self
					.certified(node, id)
					.is_some_and(|regions| !regions.iter().any(|region| overlaps(claim, region)));
			let kind = if !overlaps(claim, self.nodes[node].watch) {
				Kind::Unwatched
			} else if self.live(node, id) && !uncertified {
				Kind::End
			} else {
				Kind::Offline
			};
			log.push((id, claim, kind, was.item.since));
		}
		let events: Vec<Event> = log
			.into_iter()
			.map(|(id, claim, kind, since)| {
				// A qualified identity answers to the history of the origin it names.
				let origin = self.source(id).unwrap_or(id);
				Event {
					node,
					id,
					claim,
					kind,
					since,
					forgot: (
						self.nodes[node].restarts,
						self.nodes[node].forgot.get(&(id, claim)).copied().unwrap_or(0),
					),
					withdrawn: self
						.truth(origin, claim, self.revision(origin))
						.is_none_or(|item| item.since != since),
				}
			})
			.collect();
		for event in &events {
			self.nodes[node].forgot.entry((event.id, event.claim)).or_insert(0);
		}
		self.events.extend(events);
		self.nodes[node].shown = shown;
	}

	/// Babel's feasibility distance: reset by a newer seqno, lowered by a better metric.
	fn record(&mut self, node: Node, id: Id, selected: Selected) {
		let fd = self.nodes[node]
			.fd
			.entry(id)
			.or_insert((selected.seqno, selected.metric));
		if selected.seqno > fd.0 || (selected.seqno == fd.0 && selected.metric < fd.1) {
			*fd = (selected.seqno, selected.metric);
		}
	}

	/// The origin's state of `claim` as of revision `rev`.
	pub fn truth(&self, id: Id, claim: &str, rev: Rev) -> Option<Item> {
		self.history
			.get(&id)?
			.iter()
			.rev()
			.find(|change| change.rev <= rev && change.claim == claim)
			.and_then(|change| change.item)
	}

	/// The origin's latest revision for `id`.
	pub fn revision(&self, id: Id) -> Rev {
		self.history
			.get(&id)
			.and_then(|history| history.last())
			.map_or(0, |change| change.rev)
	}

	/// Every claim each node knows must be the origin's state at the revision it
	/// knows that claim at: no view holds a mix of stale and fresh knowledge.
	/// Qualified views are checked against their origin. Returns the views checked.
	pub fn check(&self) -> Result<BTreeSet<(Node, Id)>, String> {
		let mut checked = BTreeSet::new();
		for (node, state) in self.nodes.iter().enumerate() {
			for (&key, view) in &state.views {
				if state.origins.contains_key(&key) {
					continue;
				}
				let Some(id) = self.source(key) else {
					continue;
				};
				let history = &self.history[&id];
				checked.insert((node, key));
				let claims: BTreeSet<&str> = history.iter().map(|change| change.claim).collect();
				for claim in claims {
					let Some(rev) = view.as_of(claim, true) else {
						if view.items.contains_key(claim) {
							return Err(format!("node {node} holds {id}'s {claim} with no floor"));
						}
						continue;
					};
					let known = view.items.get(claim).copied();
					if known != self.truth(id, claim, rev) {
						return Err(format!(
							"node {node} holds {key}'s {claim} as {known:?} at revision {rev}, where the origin had {:?}",
							self.truth(id, claim, rev)
						));
					}
				}
			}
		}
		Ok(checked)
	}

	/// The origin identity whose claims `key` carries: itself, or what a grant's
	/// subject qualified, unwound through any requalification.
	fn source(&self, key: Id) -> Option<Id> {
		if self.history.contains_key(&key) {
			return Some(key);
		}
		if key & QUALIFIED == 0 {
			return None;
		}
		let subjects: BTreeSet<&str> = self
			.links
			.values()
			.filter_map(|link| link.grant)
			.map(|grant| grant.subject)
			.collect();
		let known: BTreeSet<Id> = self
			.nodes
			.iter()
			.flat_map(|state| state.views.keys().chain(state.rx.keys()))
			.copied()
			.chain(self.history.keys().copied())
			.collect();
		known
			.into_iter()
			.filter(|&inner| inner != key)
			.find(|&inner| subjects.iter().any(|subject| qualified(subject, inner) == key))
			.and_then(|inner| self.source(inner))
	}

	/// Announcement and route state kept across every node.
	pub fn retained(&self) -> Retained {
		let mut out = Retained::default();
		for state in &self.nodes {
			for (id, view) in &state.views {
				if !state.origins.contains_key(id) {
					out.items += view.items.len();
					out.floors += view.floors.len();
				}
			}
			out.heard += state.heard.values().map(|view| view.items.len()).sum::<usize>();
			out.heard_floors += state.heard.values().map(|view| view.floors.len()).sum::<usize>();
			out.sent += state.sent_views.values().map(|view| view.items.len()).sum::<usize>();
			out.sent_floors += state.sent_views.values().map(|view| view.floors.len()).sum::<usize>();
			out.echoes += state.unacked.values().map(|&count| count as usize).sum::<usize>();
			out.pending += state.pending.len();
			out.distances += state.fd.keys().filter(|id| !state.origins.contains_key(id)).count();
			out.certified += state.current.values().map(Vec::len).sum::<usize>();
		}
		out
	}

	fn candidates(&self, node: Node, path: &str, from: Option<Node>) -> Vec<Candidate> {
		let grants: BTreeSet<(&str, &str)> = self
			.links
			.values()
			.filter_map(|link| link.grant)
			.map(|grant| (grant.subject, grant.scope))
			.collect();
		let mut out = Vec::new();
		for (index, claim) in self.claims.iter().enumerate() {
			if !claim.covers(path) || self.hidden.contains(&(node, index)) {
				continue;
			}
			// A claim counts under its bare identity, and under a qualified key only inside that grant's scope.
			let inside = grants.iter().filter(|(_, scope)| within(claim.prefix, scope));
			for id in iter::once(claim.id).chain(inside.map(|(subject, _)| qualified(subject, claim.id))) {
				let Some(route) = self.route_from(node, id, from) else {
					continue;
				};
				let Some(total) = self.cfg.rule.add(route.metric, claim.seed) else {
					continue;
				};
				out.push(Candidate {
					id,
					next: route.next,
					seed: claim.seed,
					total,
					advertised: route.advertised,
					draining: route.next.is_some_and(|next| self.draining.contains(&(node, next))),
					priority: route.next.map_or(0, |next| self.links[&(node, next)].priority),
					prefix: claim.prefix.len(),
					epoch: claim.epoch,
					down_only: route.down_only,
				});
			}
		}
		out
	}

	#[allow(clippy::type_complexity)]
	fn rank(
		&self,
		node: Node,
		path: &str,
		c: &Candidate,
	) -> (Reverse<usize>, Reverse<Option<u32>>, bool, Reverse<i8>, Metric, u64) {
		let tie = match self.cfg.tie {
			TieBreak::PathIdentity => fnv(path, &[c.id]),
			TieBreak::PathHere => fnv(path, &[c.id, c.next.unwrap_or(node) as u64, node as u64]),
		};
		(
			Reverse(c.prefix),
			Reverse(c.epoch),
			c.draining,
			Reverse(c.priority),
			c.total,
			tie,
		)
	}

	/// The candidate `node` uses for a request arriving from `from` carrying `carried`.
	fn choose(&self, node: Node, path: &str, from: Option<Node>, mode: Forward, carried: Carried) -> Option<Candidate> {
		if let Some(pinned) = carried.pinned {
			// A pinned request needs a route to the identity, not knowledge of its claim.
			let route = self.route_from(node, pinned, from)?;
			return Some(Candidate {
				id: pinned,
				next: route.next,
				seed: 0,
				total: route.metric,
				advertised: route.advertised,
				draining: false,
				priority: 0,
				prefix: 0,
				epoch: carried.epoch,
				down_only: route.down_only,
			});
		}
		let mut candidates = self.candidates(node, path, from);
		match mode {
			Forward::PathOnly {
				exclude_requester: true,
			} => candidates.retain(|c| from.is_none() || c.next != from),
			Forward::Downhill if from.is_some() => {
				let best = candidates.iter().map(|c| c.total).min();
				candidates.retain(|c| c.next.is_none() || best.is_some_and(|best| c.advertised + c.seed < best));
			}
			_ => {}
		}
		if let Some(bound) = carried.bound {
			candidates.retain(|c| c.total <= bound);
		}
		candidates.into_iter().min_by_key(|c| self.rank(node, path, c))
	}

	/// What `node` sends `next` for `chosen`: a pin or bound is set by the first
	/// hop and kept or tightened after, and a pin is named as `next` knows it.
	fn carry(
		&self,
		node: Node,
		next: Node,
		mode: Forward,
		first: bool,
		chosen: &Candidate,
		carried: Carried,
	) -> Carried {
		let carried = if first || mode == Forward::Bounded {
			Carried {
				pinned: (mode == Forward::Pinned).then_some(chosen.id),
				bound: (mode == Forward::Bounded).then_some(chosen.advertised + chosen.seed),
				epoch: chosen.epoch,
			}
		} else {
			carried
		};
		Carried {
			pinned: carried.pinned.map(|id| self.name(node, next, id)),
			..carried
		}
	}

	/// The identity a reader at `at` would pick for `path`.
	pub fn resolve(&self, at: Node, path: &str) -> Option<Id> {
		self.choose(at, path, None, Forward::Pinned, Carried::default())
			.map(|chosen| chosen.id)
	}

	/// Follow a request for `path` that a reader at `at` makes.
	pub fn request(&self, at: Node, path: &str, mode: Forward) -> Outcome {
		let mut path_nodes = vec![at];
		let mut node = at;
		let mut from = None;
		let mut carried = Carried::default();
		let mut seen = BTreeSet::new();
		loop {
			if !seen.insert((node, from, carried)) {
				return Outcome::Loop { path: path_nodes };
			}
			let Some(chosen) = self.choose(node, path, from, mode, carried) else {
				return Outcome::Stalled { path: path_nodes };
			};
			let Some(next) = chosen.next else {
				return Outcome::Served {
					by: chosen.id,
					path: path_nodes,
				};
			};
			carried = self.carry(node, next, mode, from.is_none(), &chosen, carried);
			from = Some(node);
			node = next;
			path_nodes.push(node);
		}
	}

	/// Readers at every node in `openers` subscribe to `path` at once; each
	/// relay's forwarded request may join a subscription the relay already has,
	/// if the request may follow that subscription's route. Returns each
	/// opener's outcome; a subscription cycle that no source feeds is a loop.
	pub fn subscribe_all(&self, path: &str, openers: &[Node], mode: Forward, aggregate: Aggregate) -> Vec<Outcome> {
		let mut fronts: Vec<Front> = Vec::new();
		let open = |net: &Self, node: Node, from: Option<Node>, carried: Carried| -> Front {
			match net.choose(node, path, from, mode, carried) {
				None => Front {
					node,
					id: 0,
					epoch: None,
					total: 0,
					down_only: false,
					up: Up::Stalled,
				},
				Some(chosen) => Front {
					node,
					id: chosen.id,
					epoch: chosen.epoch,
					total: chosen.total,
					down_only: chosen.down_only,
					up: match chosen.next {
						None => Up::Serve,
						Some(next) => Up::Pending(
							node,
							next,
							net.carry(node, next, mode, from.is_none(), &chosen, carried),
						),
					},
				},
			}
		};
		for &node in openers {
			fronts.push(open(self, node, None, Carried::default()));
		}

		let mut index = 0;
		while index < fronts.len() {
			if let Up::Pending(from, to, carried) = fronts[index].up {
				let exports_only = self.exports_only(to, Some(from));
				let joins = fronts.iter().position(|front| {
					front.node == to
						&& !matches!(front.up, Up::Stalled)
						&& !(exports_only && front.down_only)
						&& match aggregate {
							Aggregate::Path => true,
							Aggregate::Epoch => carried.epoch.is_some() && front.epoch == carried.epoch,
							Aggregate::Identity => carried.pinned == Some(front.id),
							Aggregate::Bound => carried.bound.is_some_and(|bound| front.total <= bound),
						}
				});
				let target = joins.unwrap_or_else(|| {
					fronts.push(open(self, to, Some(from), carried));
					fronts.len() - 1
				});
				fronts[index].up = Up::Joined(target);
			}
			index += 1;
		}

		// Every front hangs off an opener's chain, so any cycle shows up as a loop here.
		(0..openers.len())
			.map(|mut at| {
				let mut seen = Vec::new();
				loop {
					let again = seen.contains(&at);
					seen.push(at);
					let path = seen.iter().map(|&i| fronts[i].node).collect();
					if again {
						return Outcome::Loop { path };
					}
					match fronts[at].up {
						Up::Joined(next) => at = next,
						Up::Serve => {
							return Outcome::Served {
								by: fronts[at].id,
								path,
							};
						}
						Up::Stalled => return Outcome::Stalled { path },
						Up::Pending(..) => unreachable!("every pending front is resolved"),
					}
				}
			})
			.collect()
	}
}

/// The key a relay files `id` under when a connection admitted under a
/// scoped grant for `subject` forwards it. It depends on the subject, not the
/// connection or the grant's patterns, so every uplink of one subject reaches
/// the same key across token refresh. FNV keeps the model readable; the
/// contract needs a collision-resistant hash, since the forwarder picks `id`.
pub fn qualified(subject: &str, id: Id) -> Id {
	fnv(subject, &[id]) | QUALIFIED
}

/// FNV-1a, as `origin::fnv_key` uses, over a path and some ids.
pub fn fnv(path: &str, ids: &[u64]) -> u64 {
	const PRIME: u64 = 0x0000_0100_0000_01b3;
	let mut hash: u64 = 0x420C0DECB00B;
	for byte in path.bytes().chain(ids.iter().flat_map(|id| id.to_le_bytes())) {
		hash = (hash ^ u64::from(byte)).wrapping_mul(PRIME);
	}
	hash
}
