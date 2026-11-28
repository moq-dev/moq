//! Lite-07: what the peer sends on its Announce Stream as a publisher, feeding the
//! origin's route node.
//!
//! The session asks the peer for a set of prefixes. A peer that forwards and is trusted
//! with the whole tree answers with routes, and under each the claims of the node it
//! leads to, a batch of them closed by its ANNOUNCE_FLOOR. Any other peer answers with
//! claims alone, each taking effect as it arrives. Everything goes to the
//! [`route::Node`], which decides which session serves each claim it shows, and the
//! session puts the ones it is handed into the origin, minting a source per request.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
	sync::Arc,
	task::Poll,
};

use crate::{
	Error, Identity, Path, lite,
	route::{
		self, NodeId,
		node::{self, Change, Claim, Msg},
		relay,
	},
	stats::{RouteClose, RouteRefusal, Withdrawal},
};

use super::{AnnouncedRoute, Subscriber};

/// A prefix this session asked the peer for, by its Prefix ID.
struct Asked {
	/// Absolute.
	path: Path,
	/// Relative to the session, as the wire names it.
	relative: Path,
	/// The peer answered the ANNOUNCE_REQUEST adding it, so its stream may name it.
	live: bool,
	/// The peer sent its initial set.
	synced: bool,
}

/// An ANNOUNCE_REQUEST the peer has not answered yet.
struct Editing {
	remove: Vec<u64>,
	add: Vec<u64>,
}

/// One live claim the peer sent, by its Announce ID.
struct WireClaim {
	/// Absolute: the origin node keys every claim from the origin's root. `None` when
	/// it is no path this node can hold, which the claim then never has.
	path: Option<Path>,
	/// Relative to the session, as the wire names it.
	relative: Option<Path>,
	identity: Identity,
	claim: Claim,
	/// The node whose claim it is, once a floor closed the batch that started it.
	origin: Option<NodeId>,
	/// The absolute paths the node holds it at (see [`Routed::filing`]): none when the
	/// session may carry nothing beneath it.
	filed: Vec<Path>,
}

/// A change to the claim with an Announce ID, waiting for the floor that closes its batch.
struct Pending {
	id: u64,
	broadcast: node::Broadcast,
	change: Change,
}

/// The claim changes since the last floor, at most one per claim, in the order each
/// claim first changed. A peer that withholds the floor then holds no more here than
/// the claims it keeps live and those the last floor left it.
#[derive(Default)]
struct Batch {
	changes: BTreeMap<u64, Pending>,
	/// Where each live claim's change is.
	live: HashMap<u64, u64>,
	next: u64,
}

impl Batch {
	fn is_empty(&self) -> bool {
		self.changes.is_empty()
	}

	fn push(&mut self, pending: Pending) -> u64 {
		let at = self.next;
		self.next += 1;
		self.changes.insert(at, pending);
		at
	}

	/// The claim `id` started or changed: only its latest state is kept.
	fn live(&mut self, id: u64, broadcast: node::Broadcast, change: Change) {
		match self.live.get(&id).and_then(|at| self.changes.get_mut(at)) {
			Some(pending) => pending.change = change,
			None => {
				let at = self.push(Pending { id, broadcast, change });
				self.live.insert(id, at);
			}
		}
	}

	/// The claim `id` ended. One the node was never `told`, started within this batch,
	/// leaves nothing to tell.
	fn end(&mut self, id: u64, broadcast: node::Broadcast, change: Change, told: bool) {
		match (self.live.remove(&id), told) {
			(Some(at), false) => {
				self.changes.remove(&at);
			}
			(Some(at), true) => {
				if let Some(pending) = self.changes.get_mut(&at) {
					pending.change = change;
				}
			}
			(None, _) => {
				self.push(Pending { id, broadcast, change });
			}
		}
	}

	fn take(&mut self) -> impl Iterator<Item = Pending> + use<> {
		self.live.clear();
		std::mem::take(&mut self.changes).into_values()
	}
}

/// A claim the node serves through this session.
struct ServedClaim {
	/// The node every request names, or `None` when the peer selects.
	named: Option<NodeId>,
	route: AnnouncedRoute<ServedKey>,
}

/// A served claim's claimant, its path, and the identity it serves.
type ServedKey = (NodeId, Path, Identity);

/// What a closed session leaves in the origin of a claim it served.
struct Left(crate::origin::Claim);

impl node::Orphan for Left {
	fn release(self: Box<Self>, gone: node::Gone) {
		release(self.0, gone);
	}
}

/// Let go of a copy of a claim the origin holds, for the reason `gone` gives.
fn release(claim: crate::origin::Claim, gone: node::Gone) {
	match gone {
		// Dropped, it goes offline, while another copy serves.
		node::Gone::Moved => drop(claim),
		node::Gone::Lost => claim.retire(false),
		node::Gone::Ended => claim.retire(true),
		node::Gone::Withdrawn => claim.withdrawn(),
	}
}

/// The route layer a lite-07 session's peer feeds.
pub(in crate::lite) struct Routed {
	stream: Arc<node::Stream>,
	/// How the origin's node names itself.
	me: NodeId,
	/// The peer forwards and is trusted with the whole tree: it sends routes.
	routing: bool,
	/// The permit the session filed under. The node holds the peer's claims only where
	/// its limit admits them (see [`filing`]).
	permit: crate::auth::Permit,
	root: Path,
	/// The session's origin handle, whose exclusions and mounts also gate the claims.
	origin: crate::origin::Producer,
	/// The prefixes the origin wants of the peer, relative to the root, each holding the
	/// origin's readers unsynced until the peer's initial set for it is in.
	wants: BTreeMap<Path, Option<crate::model::Snapshot>>,
	/// The prefixes asked of the peer.
	asked: BTreeMap<u64, Asked>,
	/// What of the grant the peer's answers already cover, `None` for everything: what
	/// a grant change shows beyond it may already be in the peer's answer, unheard here.
	covered: Option<crate::Prefixes>,
	/// What the grant shows beyond `covered` within the wants, held until an answer after
	/// it is in: one per want and prefix of the grant, however often it changes.
	gained: Vec<Gain>,
	/// The grant showed more since the last edit: the next is sent even when it changes
	/// nothing, since its ANNOUNCE_OK follows what the change shows under a wider prefix.
	regranted: bool,
	next_prefix: u64,
	/// The edits the peer has yet to answer, oldest first.
	edits: VecDeque<Editing>,
	/// The peer finished its stream: it answers nothing more.
	finished: bool,
	/// This side finished its stream: it asks for nothing more.
	silent: bool,
	/// The node each live Route ID leads to, and the Announce IDs of its claims.
	routes: HashMap<u64, (NodeId, BTreeSet<u64>)>,
	/// The live Route ID of each node: the stream carries at most one.
	named: HashMap<NodeId, u64>,
	next_route: u64,
	claims: HashMap<u64, WireClaim>,
	/// The claim changes since the last floor.
	pending: Batch,
	decoder: lite::AnnounceDecoder,
	/// The session's announce cap: one slot per live route and claim.
	slots: crate::session::Slots,
	held: Vec<crate::session::Slot>,
	served: HashMap<ServedKey, ServedClaim>,
	/// Served claims whose request queue woke since the last pass.
	ready: kio::Queue<ServedKey>,
	/// Why the last error closes the session, when the route layer said.
	cause: Option<RouteClose>,
}

/// What a grant change newly shows within a want: the readers at `held` wait for an
/// answer after it.
struct Gain {
	want: Path,
	/// The prefix of the grant that showed it.
	prefix: crate::Prefix,
	/// The deeper of the want and the prefix, relative to the root.
	held: Path,
	/// Holds the readers until dropped.
	_snapshot: crate::model::Snapshot,
}

/// The absolute prefixes a session asks a peer that grants `permit` for, to serve
/// `wants`: the wider of any two nested, and few enough to fit one ANNOUNCE_REQUEST.
fn asks(
	root: &Path,
	permit: &crate::auth::Permit,
	wants: &BTreeMap<Path, Option<crate::model::Snapshot>>,
) -> BTreeSet<Path> {
	let granted = permit.granted_regions();
	let mut relative: BTreeSet<Path> = BTreeSet::new();
	for want in wants.keys() {
		match &granted {
			// Whichever of the two lies within the other.
			Some(regions) => relative.extend(regions.iter().filter_map(|region| match () {
				_ if region.has_prefix(want) => Some(region.clone()),
				_ if want.has_prefix(region) => Some(want.clone()),
				_ => None,
			})),
			None => {
				relative.insert(want.clone());
			}
		}
	}
	let regions = granted.unwrap_or_default().into_iter().collect();
	(coarsen(widest(&relative), &regions).iter())
		.filter_map(|prefix| root.join(prefix).ok())
		.collect()
}

/// The prefixes of `set` that lie inside no other.
fn widest(set: &BTreeSet<Path>) -> BTreeSet<Path> {
	set.iter()
		.filter(|prefix| !route::ancestors(prefix).any(|above| above != **prefix && set.contains(&above)))
		.cloned()
		.collect()
}

/// `set`, asked for more coarsely until it fits the Adds of one ANNOUNCE_REQUEST, which
/// the peer refuses a larger set by closing the session: the prefixes inside one of the
/// grant's `regions`, or all of them without a confined grant, give way to the longest
/// prefix they share, the largest such group first, and then a prefix alone in its
/// region gives way to the region. Either lies inside the region, so the session asks
/// for more than it wants and never for more than it is granted. Regions too many on
/// their own give way to the longest prefix any of them share, up to the root: the peer
/// answers a prefix wider than the grant region by region, so none goes unasked.
fn coarsen(mut set: BTreeSet<Path>, regions: &BTreeSet<Path>) -> BTreeSet<Path> {
	while !lite::request_fits(set.iter(), lite::Version::Lite07) {
		// Each prefix files under the widest region holding it.
		let mut groups: BTreeMap<Option<&Path>, Vec<&Path>> = BTreeMap::new();
		for prefix in &set {
			let region = route::heads(prefix).find_map(|head| regions.get(head));
			groups.entry(region).or_default().push(prefix);
		}
		let bytes = |group: &[&Path]| group.iter().map(|prefix| prefix.as_str().len()).sum::<usize>();
		let shared = (groups.values())
			.filter(|group| group.len() > 1)
			.max_by_key(|group| bytes(group))
			.map(|group| {
				(route::ancestors(group[0]).rev())
					.find(|above| group.iter().all(|prefix| route::within(prefix, above)))
					.unwrap_or_default()
			});
		let region = || {
			(groups.iter())
				.filter_map(|(region, group)| Some(((*region)?, group)))
				.filter(|(region, group)| group[0] != *region)
				.max_by_key(|(_, group)| bytes(group))
				.map(|(region, _)| region.clone())
		};
		// Nothing is left to widen within a region: the regions alone pass one request.
		let wider = shared.or_else(region).unwrap_or_else(|| deepest_shared(&set));
		set.insert(wider);
		set = widest(&set);
	}
	set
}

/// The longest prefix at least two of `set` lie within, the root at the least.
fn deepest_shared(set: &BTreeSet<Path>) -> Path {
	let mut counts: BTreeMap<Path, usize> = BTreeMap::new();
	for prefix in set {
		for above in route::ancestors(prefix).filter(|above| above != prefix) {
			*counts.entry(above).or_default() += 1;
		}
	}
	(counts.into_iter())
		.filter(|(_, count)| *count > 1)
		.map(|(above, _)| above)
		.max_by_key(|above| above.parts().count())
		.unwrap_or_default()
}

impl Routed {
	/// File the session with the origin's node. `answering` is the handle the session
	/// answers the peer from when it is on the same node: the two directions then share
	/// one key. With `routing`, the peer forwards and is trusted with the whole tree.
	pub(in crate::lite) fn new(
		subscriber: &Subscriber,
		link_cost: u64,
		permit: &crate::auth::Permit,
		answering: Option<&crate::origin::Consumer>,
		routing: Option<NodeId>,
	) -> Result<Self, Error> {
		let root = subscriber.origin.root().clone();
		let node = subscriber.origin.route_node();
		let forwards = subscriber.origin.proxy();
		let role = match routing {
			Some(peer) => relay::Role::Routing(peer),
			// Asked as the origin's readers come to want something.
			None => relay::Role::Scoped(Vec::new()),
		};
		let port = relay::Port {
			// A zero-cost link never reaches here: SETUP refuses it on lite-07.
			cost: link_cost.max(1),
			priority: subscriber.origin.priority().value(),
			// A node that never forwards marks every link, so nothing it learns on one
			// reaches another.
			upstream: !forwards
				|| subscriber.origin.link() == crate::origin::Link::Upstream
				|| answering.is_some_and(|origin| origin.upstream()),
			role,
		};
		let stream = (node.connect(subscriber.peer_setup.tag(), port))
			// Said here, since the peer is told only that it broke the protocol.
			.inspect_err(|err| tracing::warn!(%err, peer = ?routing, "the route layer refused the session"))
			.map_err(|err| {
				subscriber.origin.stats().route_closed(err.close());
				Error::ProtocolViolation
			})?
			.with_clock(subscriber.runtime.clone())
			.with_stats(subscriber.origin.stats().clone());
		Ok(Self {
			me: node.me(),
			stream: Arc::new(stream),
			routing: routing.is_some(),
			permit: permit.clone(),
			root,
			origin: subscriber.origin.clone(),
			wants: BTreeMap::new(),
			asked: BTreeMap::new(),
			covered: permit.grant().cloned(),
			gained: Vec::new(),
			regranted: false,
			next_prefix: 0,
			edits: VecDeque::new(),
			finished: false,
			silent: false,
			routes: HashMap::new(),
			named: HashMap::new(),
			next_route: 0,
			claims: HashMap::new(),
			pending: Batch::default(),
			decoder: Default::default(),
			slots: subscriber.announces.clone(),
			held: Vec::new(),
			served: HashMap::new(),
			ready: Default::default(),
			cause: None,
		})
	}

	/// Why the error this session last returned closes it, as its stats count it: what
	/// the route layer said, or else a message the rule never builds.
	pub(in crate::lite) fn closed(&mut self) -> RouteClose {
		self.cause.take().unwrap_or(RouteClose::Malformed)
	}

	/// Why the last error closes the session, only when the route layer said.
	pub(in crate::lite) fn cause(&mut self) -> Option<RouteClose> {
		self.cause.take()
	}

	/// The session's key on the node.
	pub(in crate::lite) fn stream(&self) -> Arc<node::Stream> {
		self.stream.clone()
	}

	/// The origin wants `prefix` of the peer, holding `snapshot` until its initial set is in.
	pub(in crate::lite) fn want(&mut self, prefix: Path, snapshot: crate::model::Snapshot) {
		self.wants.insert(prefix, Some(snapshot));
		self.reask();
	}

	/// The origin no longer wants `prefix` of the peer.
	pub(in crate::lite) fn unwant(&mut self, prefix: &Path) {
		self.wants.remove(prefix);
		self.gained.retain(|gain| gain.want != *prefix);
		self.reask();
	}

	/// Ask the peer for what the session wants of it now, if that changed. A routing
	/// neighbour is always asked for the whole tree.
	fn reask(&mut self) {
		if self.routing || self.finished || self.silent {
			return;
		}
		// One edit at a time: its answer asks again for what the readers want by then, so
		// a peer slow to answer holds one edit here, however often they come and go.
		if !self.stream.answered() {
			return;
		}
		let wanted = asks(&self.root, &self.permit, &self.wants);
		let asking = self.stream.asking();
		let edit = relay::Edit {
			remove: asking.difference(&wanted).cloned().collect(),
			add: wanted.difference(&asking).cloned().collect(),
		};
		let regranted = std::mem::take(&mut self.regranted);
		if edit.remove.is_empty() && edit.add.is_empty() && !regranted {
			return;
		}
		// Refused only for a set with one prefix inside another, which `asks` never builds.
		let _ = self.stream.ask(edit);
	}

	/// Let the origin's readers see each prefix synced whose answer is in: the peer
	/// answered every request, and sent the initial set of each prefix asked for it.
	/// The caller serves what the node queued first.
	pub(in crate::lite) fn release(&mut self) {
		let done = self.finished || self.silent;
		if !done && !self.stream.answered() {
			return;
		}
		let (root, asked) = (&self.root, &self.asked);
		// Whether every prefix asked that overlaps `prefix` sent its initial set.
		let answered = |prefix: &Path| {
			let Ok(prefix) = root.join(prefix) else {
				return true;
			};
			let mut answers = (asked.values())
				.filter(|asked| route::within(&asked.path, &prefix) || route::within(&prefix, &asked.path));
			done || answers.all(|asked| asked.synced)
		};
		for (want, snapshot) in &mut self.wants {
			if snapshot.is_some() && answered(want) {
				*snapshot = None;
			}
		}
		let (released, held): (Vec<Gain>, Vec<Gain>) = std::mem::take(&mut self.gained)
			.into_iter()
			.partition(|gain| answered(&gain.held));
		self.gained = held;
		// The peer's answers now cover each released prefix, and once nothing is held, the
		// whole grant.
		if self.gained.is_empty() {
			if self.covered.as_ref() != self.permit.grant() {
				self.covered = self.permit.grant().cloned();
			}
		} else if let Some(covered) = self.covered.as_mut() {
			for gain in released {
				if !self.gained.iter().any(|held| held.prefix == gain.prefix) {
					covered.insert(gain.prefix);
				}
			}
		}
	}

	/// The peer finished its stream: it is leaving, and answers nothing more.
	pub(in crate::lite) fn finish(&mut self) {
		self.finished = true;
		self.orphan();
		self.stream.finished();
	}

	/// Leave what the session served in the origin, serving nothing, until another
	/// session serves it or the node's reader lets it go.
	fn orphan(&mut self) {
		for ((origin, path, identity), mut served) in self.served.drain() {
			let unavailable = served.route.route.clone().with_available(false);
			served.route.update(unavailable);
			served.route.dynamic.disown();
			let left = Left(served.route.dynamic.clone());
			self.stream.orphan(origin, &(path, identity), Box::new(left));
		}
	}

	/// This side finished its stream: the prefixes asked of the peer stay as they are.
	pub(in crate::lite) fn silence(&mut self) {
		self.silent = true;
	}

	/// The node asks the peer for other prefixes: the ANNOUNCE_REQUESTs of the edit.
	pub(in crate::lite) fn ask(&mut self, edit: relay::Edit) -> Vec<lite::AnnounceBroadcast> {
		if self.finished || self.silent {
			return Vec::new();
		}
		let mut editing = Editing {
			remove: Vec::new(),
			add: Vec::new(),
		};
		for path in &edit.remove {
			// The latest one asked: an earlier one of the same path was removed since.
			let id = self.asked.iter().rev().find(|(_, asked)| asked.path == *path);
			if let Some((id, _)) = id {
				editing.remove.push(*id);
			}
		}
		let mut add = Vec::new();
		for path in edit.add {
			// Asked for within the root, so named beneath it.
			let Some(relative) = path.strip_prefix(&self.root) else {
				continue;
			};
			let id = self.next_prefix;
			self.next_prefix += 1;
			let asked = Asked {
				path,
				relative: relative.clone(),
				live: false,
				synced: false,
			};
			self.asked.insert(id, asked);
			editing.add.push(id);
			// Hidden claims too: the session mirrors the peer into the origin, and each
			// local reader opts in on its own.
			add.push((relative, true));
		}
		let requests = lite::request_parts(editing.remove.clone(), add, lite::Version::Lite07);
		self.edits.push_back(editing);
		requests
	}

	/// ANNOUNCE_OK: the oldest edit takes effect on the peer's stream. Both ends drop
	/// every claim within a prefix it removed, and every route when it leaves a routing
	/// neighbour asked for nothing.
	fn ok(&mut self) -> Result<(), Error> {
		let editing = self.edits.pop_front().ok_or(Error::ProtocolViolation)?;
		let mut removed = Vec::new();
		for id in &editing.remove {
			if let Some(asked) = self.asked.remove(id) {
				removed.push(asked.relative);
			}
		}
		// Whether a prefix in effect before the edit survives it: the peer's claims within
		// one stay.
		let survives = self.asked.values().any(|asked| asked.live);
		for id in &editing.add {
			if let Some(asked) = self.asked.get_mut(id) {
				asked.live = true;
			}
		}
		if !removed.is_empty() {
			// A claim whose path this node could not resolve lies in a prefix it cannot
			// name. The peer may still hold it, and ends it by its id, so it goes only when
			// no earlier prefix is left for it to lie in.
			let within = |claim: &WireClaim| match &claim.relative {
				Some(path) => removed.iter().any(|prefix| route::within(path, prefix)),
				None => !survives,
			};
			let gone: Vec<u64> = (self.claims.iter())
				.filter(|(_, claim)| within(claim))
				.map(|(id, _)| *id)
				.collect();
			for id in gone {
				self.decoder.end(id)?;
				self.claims.remove(&id);
				self.held.pop();
			}
			if self.routing {
				self.held.truncate(self.held.len().saturating_sub(self.routes.len()));
				self.routes.clear();
				self.named.clear();
			}
		}
		self.stream
			.recv(Msg::Edited)
			.map_err(|err| refused(err, &mut self.cause))?;
		self.reask();
		Ok(())
	}

	/// Apply one message the peer sent on its stream as a publisher.
	pub(in crate::lite) fn receive(&mut self, announce: lite::AnnounceBroadcast) -> Result<(), Error> {
		use lite::AnnounceBroadcast as Wire;
		// Only its floor closes a batch of claim messages.
		let claim = matches!(
			announce,
			Wire::Start { .. } | Wire::Change { .. } | Wire::End { .. } | Wire::Floor { .. }
		);
		if !claim && !self.pending.is_empty() {
			return Err(Error::ProtocolViolation);
		}
		// Only two nodes that both declared a Node ID exchange routes.
		let route = matches!(
			announce,
			Wire::Route { .. } | Wire::RouteEnd { .. } | Wire::Dead { .. } | Wire::Drain { .. } | Wire::Floor { .. }
		);
		if route && !self.routing {
			self.cause = Some(RouteClose::Role);
			return Err(Error::ProtocolViolation);
		}
		match announce {
			Wire::Ok => self.ok()?,
			Wire::Synced { prefix } => {
				let asked = (self.asked.get_mut(&prefix)).filter(|asked| asked.live && !asked.synced);
				let asked = asked.ok_or(Error::ProtocolViolation)?;
				asked.synced = true;
				let path = asked.path.clone();
				self.stream
					.recv(Msg::Synced(path))
					.map_err(|err| refused(err, &mut self.cause))?;
			}
			Wire::Route { offer } => {
				let origin = offer.path.first().ok_or(Error::ProtocolViolation)?.node;
				if !self.named.contains_key(&origin) {
					self.held.push(
						self.slots
							.acquire()
							.inspect_err(|_| self.cause = Some(RouteClose::Other))?,
					);
					let id = self.next_route;
					self.next_route += 1;
					self.named.insert(origin, id);
					self.routes.insert(id, (origin, BTreeSet::new()));
				}
				let offer = Some(offer);
				self.stream
					.recv(Msg::Route { origin, offer })
					.map_err(|err| refused(err, &mut self.cause))?;
			}
			Wire::RouteEnd { id } => {
				let (origin, claims) = self.routes.remove(&id).ok_or(Error::ProtocolViolation)?;
				self.named.remove(&origin);
				self.held.pop();
				// The route's end retires every claim under it.
				for claim in claims {
					self.decoder.end(claim)?;
					self.claims.remove(&claim);
					self.held.pop();
				}
				let offer = None;
				self.stream
					.recv(Msg::Route { origin, offer })
					.map_err(|err| refused(err, &mut self.cause))?;
			}
			Wire::Dead { hop, far } => {
				self.origin.stats().withdrawal(Withdrawal::Dead, true);
				self.stream
					.recv(Msg::Dead { hop, far })
					.map_err(|err| refused(err, &mut self.cause))?
			}
			Wire::Drain { node } => {
				self.origin.stats().withdrawal(Withdrawal::Drain, true);
				self.stream
					.recv(Msg::Drain { node })
					.map_err(|err| refused(err, &mut self.cause))?
			}
			Wire::Floor { id, revision } => self.floor(id, revision)?,
			Wire::Start {
				suffix,
				identity,
				claim,
			} => {
				let relative = self.decoder.start(Some(suffix))?;
				let id = self.decoder.last();
				let path = relative.as_ref().and_then(|relative| self.root.join(relative).ok());
				// Fields valid alone can still resolve to a path this node cannot hold. The
				// claim keeps its id, and reaches no reader here.
				if path.is_none() {
					self.origin.stats().refuse_route(RouteRefusal::Path);
				}
				self.above(relative.as_ref(), &claim)?;
				self.held.push(
					self.slots
						.acquire()
						.inspect_err(|_| self.cause = Some(RouteClose::Other))?,
				);
				let wire = WireClaim {
					path,
					relative,
					identity,
					claim,
					origin: None,
					filed: Vec::new(),
				};
				self.claims.insert(id, wire);
				self.changed(id, true)?;
			}
			Wire::Change { id, claim } => {
				let wire = self.claims.get(&id).ok_or(Error::ProtocolViolation)?;
				self.above(wire.relative.as_ref(), &claim)?;
				self.claims.get_mut(&id).expect("checked above").claim = claim;
				self.changed(id, false)?;
			}
			Wire::End { id, lost } => {
				self.decoder.end(id)?;
				let wire = self.claims.remove(&id).ok_or(Error::ProtocolViolation)?;
				self.held.pop();
				let change = match lost {
					true => Change::Lost,
					false => Change::Ended,
				};
				let broadcast = wire.path.map(|path| (path, wire.identity.clone()));
				if self.routing {
					if let Some((_, claims)) = wire
						.origin
						.and_then(|origin| self.routes.get_mut(self.named.get(&origin)?))
					{
						claims.remove(&id);
					}
					// Every forwarding node holds every path another does.
					let broadcast = broadcast.ok_or(Error::ProtocolViolation)?;
					self.pending.end(id, broadcast, change, wire.origin.is_some());
				} else if !wire.filed.is_empty() {
					let changes = (wire.filed.into_iter())
						.map(|path| ((path, wire.identity.clone()), change.clone()))
						.collect();
					self.stream
						.recv(Msg::Announce { changes })
						.map_err(|err| refused(err, &mut self.cause))?;
				}
			}
			// The peer's own requests go to the half answering them, and lite-07 has its
			// own form of a claim.
			Wire::Request { .. }
			| Wire::Active { .. }
			| Wire::Ended { .. }
			| Wire::EndedId { .. }
			| Wire::Update { .. }
			// Decoding refuses an unknown lite-07 type; only lite-06 skips one.
			| Wire::Skipped => {
				return Err(Error::ProtocolViolation);
			}
		}
		Ok(())
	}

	/// A claim is no more than the segments of the path the peer names broader than the
	/// path it is shown at.
	fn above(&self, relative: Option<&Path>, claim: &Claim) -> Result<(), Error> {
		match relative.is_none_or(|path| claim.item.above <= path.parts().count() as u64) {
			true => Ok(()),
			false => Err(Error::ProtocolViolation),
		}
	}

	/// The claim with Announce ID `id` started, when `fresh`, or changed: a routing
	/// neighbour's waits for its floor, and any other goes to the node at once.
	fn changed(&mut self, id: u64, fresh: bool) -> Result<(), Error> {
		let wire = self.claims.get_mut(&id).expect("a live claim");
		let broadcast = wire.path.clone().map(|path| (path, wire.identity.clone()));
		let change = Change::Live(wire.claim.clone());
		if self.routing {
			// Every forwarding node holds every path another does.
			let broadcast = broadcast.ok_or(Error::ProtocolViolation)?;
			self.pending.live(id, broadcast, change);
			return Ok(());
		}
		// A claim at no path this node can hold reaches no reader here.
		let Some((path, identity)) = broadcast else {
			return Ok(());
		};
		let filing = filing(&self.origin, &self.root, &self.permit, &path);
		// Counted once per claim: as it starts, or as a change takes it out of scope.
		if filing.is_empty() && (fresh || !wire.filed.is_empty()) {
			self.origin.stats().refuse_route(RouteRefusal::Scope);
		}
		let changes = refile(wire, &identity, filing, true);
		match changes.is_empty() {
			true => Ok(()),
			false => self
				.stream
				.recv(Msg::Announce { changes })
				.map_err(|err| refused(err, &mut self.cause)),
		}
	}

	/// ANNOUNCE_FLOOR: the claim changes since the last one are those of the node the
	/// route with Route ID `id` leads to, complete as of its `revision`.
	fn floor(&mut self, id: u64, revision: route::Rev) -> Result<(), Error> {
		let (origin, claims) = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
		let origin = *origin;
		let mut changes = Vec::new();
		for pending in self.pending.take() {
			let broadcast = pending.broadcast;
			if let Some(wire) = self.claims.get_mut(&pending.id) {
				// A claim stays under the route it started under.
				if wire.origin.is_some_and(|was| was != origin) {
					return Err(Error::ProtocolViolation);
				}
				wire.origin = Some(origin);
				claims.insert(pending.id);
			}
			changes.push((broadcast, pending.change));
		}
		let floor = revision;
		self.stream
			.recv(Msg::Claims { origin, changes, floor })
			.map_err(|err| refused(err, &mut self.cause))
	}

	/// Tell the node's other sessions what the messages received changed.
	#[cfg(test)]
	pub(in crate::lite) fn flush(&self) {
		self.stream.flush();
	}

	/// The peer sent a GOAWAY: what it gave ranks below everything else. Idempotent.
	pub(in crate::lite) fn goaway(&mut self) {
		self.stream.goaway();
	}

	/// The peer went unheard past its keep-alive's `soft` limit, or was heard again: what
	/// it gave ranks below everything else while it is stale.
	pub(in crate::lite) fn stale(&mut self, stale: bool) {
		self.stream.stale(stale);
	}

	/// The session's permit changed. A routing neighbour's must stay whole. For any
	/// other, a new limit changes which of the peer's claims the node holds, and what
	/// the peer granted changes the prefixes asked of it.
	pub(in crate::lite) fn limit(&mut self, permit: &crate::auth::Permit) -> Result<(), Error> {
		if self.routing {
			return match whole(permit) {
				true => Ok(()),
				false => Err(Error::Unauthorized),
			};
		}
		let same = permit.same_limit(&self.permit);
		// What a grant change newly shows within a prefix asked may already be in the
		// peer's answer, unheard here: the next edit goes, since its answer follows the
		// change, and the readers there and any reader arriving before that answer wait
		// for it. Elsewhere, and on a narrowing, nothing new can arrive, and a reader stays
		// synced.
		if !permit.same_grant(&self.permit) {
			// The peer's answers cover only what the new grant still shows of what they did.
			self.covered = match (self.covered.take(), permit.grant()) {
				(None, granted) => granted.cloned(),
				(Some(covered), None) => Some(covered),
				(Some(covered), Some(granted)) => Some(covered.intersect(granted)),
			};
			let overlaps = |a: &Path, b: &Path| a.has_prefix(b) || b.has_prefix(a);
			let asking = self.stream.asking();
			let mut before = std::mem::take(&mut self.gained);
			for (granted, region) in permit.beyond(self.covered.as_ref()) {
				// Too deep to lie beneath the root, it shows nothing here.
				let Ok(absolute) = self.root.join(&region) else {
					continue;
				};
				if asking.iter().any(|asked| overlaps(asked, &absolute)) {
					self.regranted = true;
				}
				for want in self.wants.keys().filter(|want| overlaps(want, &region)) {
					self.regranted = true;
					// Held already: keep its snapshot, so its readers wait on unbroken.
					let kept = (before.iter()).position(|gain| gain.want == *want && gain.prefix == granted);
					if let Some(kept) = kept {
						self.gained.push(before.swap_remove(kept));
						continue;
					}
					let held = match region.has_prefix(want) {
						true => region.clone(),
						false => want.clone(),
					};
					self.gained.push(Gain {
						want: want.clone(),
						prefix: granted.clone(),
						_snapshot: self.origin.snapshot(held.clone()),
						held,
					});
				}
			}
			// What the grant no longer shows beyond what is covered can bring nothing more.
			drop(before);
		}
		self.permit = permit.clone();
		self.reask();
		if same {
			return Ok(());
		}
		let mut changes = Vec::new();
		for wire in self.claims.values_mut() {
			let Some(path) = wire.path.clone() else {
				continue;
			};
			let filing = filing(&self.origin, &self.root, &self.permit, &path);
			let identity = wire.identity.clone();
			changes.extend(refile(wire, &identity, filing, false));
		}
		for change in changes {
			let changes = vec![change];
			self.stream
				.recv(Msg::Announce { changes })
				.map_err(|err| refused(err, &mut self.cause))?;
		}
		self.stream.flush();
		Ok(())
	}

	/// Act on a change to what the node serves through this session.
	pub(in crate::lite) fn output(&mut self, subscriber: &Subscriber, output: node::Output) {
		match output {
			node::Output::Serve {
				origin,
				path,
				identity,
				served,
			} => {
				let key = (origin, path, identity);
				let served = self.serve(subscriber, &key, served);
				// Dropped, a claim this session may no longer carry goes offline rather
				// than ending: its publisher never withdrew it.
				if served.is_err() {
					self.served.remove(&key);
				}
				let (origin, path, identity) = key;
				let refused = self.stream.serving(origin, &(path, identity), served.is_ok());
				// Counted as the claim's slot turns refused, not on each update behind it.
				if refused && let Err(Some(kind)) = served {
					self.origin.stats().refuse_route(kind);
				}
			}
			node::Output::Unserve {
				origin,
				path,
				identity,
				gone,
			} => {
				if let Some(served) = self.served.remove(&(origin, path, identity)) {
					release(served.route.dynamic, gone);
				}
			}
		}
	}

	/// Serve a claim through this session, or update how it is served. Fails when the
	/// session may not carry it, with the reason its stats count the refusal under.
	///
	/// The root and limit checks count nothing: a claim the peer sent outside them was
	/// refused as it arrived, and the node may hand this session another session's claim.
	fn serve(
		&mut self,
		subscriber: &Subscriber,
		key: &ServedKey,
		served: node::Served,
	) -> Result<(), Option<RouteRefusal>> {
		let (origin, claim, identity) = key;
		let Some(path) = claim.strip_prefix(&self.root) else {
			// Outside the session's root, so nothing a request could name here.
			return Err(None);
		};
		// The node may hand over a claim another session of the same peer carries, which
		// is served here only if this session may carry it too.
		if !self.permit.within_limit(path.as_str()) {
			return Err(None);
		}
		// The chain a claim travels to a lite-06 peer: the relays it entered or left a
		// routing domain by, then the node claiming it, so a relay it reaches back through
		// recognizes it. A chain already naming this node looped back across lite-06, and
		// the hop limit bounds the whole path across both versions.
		// A forwarding node that announced the claim already ends the chain.
		let claimant = (*origin != self.me && !served.chain.contains(&origin.relay)).then_some(origin.relay);
		let relays = (served.chain.iter().copied())
			.chain(claimant)
			.filter(|relay| *relay != self.me.relay);
		let mut hops = crate::Hops::new();
		for relay in relays {
			let pushed = route::node::hop(relay).is_some_and(|hop| hops.push(hop).is_ok());
			if !pushed {
				return Err(Some(RouteRefusal::Looped));
			}
		}
		if subscriber.names_self(&hops) || hops.len() > self.origin.max_hops().get() {
			tracing::debug!(route = %subscriber.log_path(&path), node = ?origin, "dropping looped claim");
			return Err(Some(RouteRefusal::Looped));
		}
		let lane = crate::origin::Lane {
			primary: served.primary,
			priority: Some(served.priority),
			// A receiver refuses a claim above its path by more than the path's segments.
			above: u8::try_from(served.attr.above).unwrap_or(u8::MAX),
			lifecycle: served.lifecycle,
		};
		// A claim no route reaches is unavailable: it stays shown, and serves nothing.
		let route = crate::origin::RouteInfo::new()
			.with_identity(identity.clone(), served.attr.epoch)
			.with_hops(hops)
			.with_cost(served.cost)
			.with_via(subscriber.via(None))
			.with_node(Some(*origin))
			.with_lane(lane)
			.with_draining(served.draining)
			.with_available(served.available)
			.with_marked(served.marked);
		if let Some(entry) = self.served.get_mut(key)
			&& entry.route.route.epoch == route.epoch
		{
			entry.named = served.named;
			entry.route.update(route);
			return Ok(());
		}
		// A new epoch replaces the old route, attached before it goes.
		tracing::debug!(route = %subscriber.log_path(&path), node = ?origin, "announce");
		match subscriber.origin.forward(&path, route.clone()) {
			Ok(dynamic) => {
				let wake = crate::util::QueueWake::new(key.clone(), self.ready.clone());
				let entry = AnnouncedRoute::new(route, dynamic, wake);
				entry.waker.wake_by_ref();
				let served = ServedClaim {
					named: served.named,
					route: entry,
				};
				self.served.insert(key.clone(), served);
				Ok(())
			}
			// Outside our origin's scope.
			Err(_) => Err(Some(RouteRefusal::Scope)),
		}
	}

	/// Serve requests on every claim whose queue woke.
	pub(in crate::lite) fn poll(&mut self, subscriber: &Subscriber, waiter: &kio::Waiter) {
		while let Poll::Ready(Ok(key)) = self.ready.poll_pop(waiter) {
			let Some(entry) = self.served.get_mut(&key) else {
				continue;
			};
			super::mint_sources(subscriber, &mut entry.route, entry.named);
		}
	}
}

impl Drop for Routed {
	fn drop(&mut self) {
		// The session closed.
		self.orphan();
	}
}

/// The node refuses what no node following the rule sends, closing the session for the
/// `cause` it notes.
fn refused(err: route::Error, cause: &mut Option<RouteClose>) -> Error {
	*cause = Some(err.close());
	Error::ProtocolViolation
}

/// Where the node files the peer's claim at the absolute `path`, as the origin files a
/// producer's claim: there when the permit's limit and the origin's scope cover it, and
/// otherwise at each of their prefixes beneath it, so a route through the session serves
/// the whole subtree it is filed at. None when the origin would refuse it (an excluded
/// subtree or a mount) or the session may carry nothing beneath it.
fn filing(origin: &crate::origin::Producer, root: &Path, permit: &crate::auth::Permit, path: &Path) -> Vec<Path> {
	let Some(relative) = path.strip_prefix(root) else {
		return Vec::new();
	};
	if !origin.publishable(path) {
		return Vec::new();
	}
	let scope = permit.limiting(origin.allowed());
	let publishable = |prefix: &Path| root.join(prefix).is_ok_and(|prefix| origin.publishable(&prefix));
	(crate::origin::filed(&scope, &relative, publishable).into_iter())
		.filter_map(|(prefix, _)| root.join(&prefix).ok())
		.collect()
}

/// File `wire`, a claim of `identity`, at `filing` instead of where it is: what the node
/// should hear, each place it holds live, as many segments above it as it lies beneath the
/// claim's path, then each it leaves lost. With `changed`, the claim itself changed, so
/// every place it stays is told again.
fn refile(
	wire: &mut WireClaim,
	identity: &Identity,
	filing: Vec<Path>,
	changed: bool,
) -> Vec<(node::Broadcast, Change)> {
	let depth = wire.path.as_ref().map_or(0, |path| path.depth());
	let mut changes = Vec::new();
	for path in &filing {
		if !changed && wire.filed.contains(path) {
			continue;
		}
		let mut claim = wire.claim.clone();
		claim.item.above = claim.item.above.saturating_add((path.depth() - depth) as u64);
		changes.push(((path.clone(), identity.clone()), Change::Live(claim)));
	}
	for path in &wire.filed {
		if !filing.contains(path) {
			changes.push(((path.clone(), identity.clone()), Change::Lost));
		}
	}
	wire.filed = filing;
	changes
}

/// Whether `permit` trusts the peer with the whole tree: neither grant confines it,
/// whether by setting no prefixes or by the empty one, which covers everything.
pub(in crate::lite) fn whole(permit: &crate::auth::Permit) -> bool {
	permit.matches("")
}

#[cfg(test)]
mod tests;
