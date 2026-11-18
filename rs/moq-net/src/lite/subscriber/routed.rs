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
	Error, Instance, Path, lite,
	route::{
		self, NodeId,
		node::{self, Change, Claim, Msg},
		relay,
	},
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
	instance: Instance,
	claim: Claim,
	/// The node whose claim it is, once a floor closed the batch that started it.
	origin: Option<NodeId>,
	/// Whether the node holds it: a claim the session's limit does not admit is left out.
	filed: bool,
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

/// A served claim's claimant, its path, and the instance it serves.
type ServedKey = (NodeId, Path, Instance);

/// What a closed session leaves in the origin of a claim it served.
struct Left(crate::origin::Claim);

impl node::Orphan for Left {
	fn release(self: Box<Self>, ended: bool) {
		// Dropped, a claim the node let go of goes offline rather than ending.
		if ended {
			self.0.withdrawn();
		}
	}
}

/// The route layer a lite-07 session's peer feeds.
pub(in crate::lite) struct Routed {
	stream: Arc<node::Stream>,
	/// How the origin's node names itself.
	me: NodeId,
	/// The peer forwards and is trusted with the whole tree: it sends routes.
	routing: bool,
	/// The permit the session filed under. The node holds only the peer's claims its
	/// limit admits.
	permit: crate::auth::Permit,
	root: Path,
	/// The session's origin handle, whose exclusions and mounts also gate the claims.
	origin: crate::origin::Producer,
	/// The prefixes the origin wants of the peer, relative to the root, each holding the
	/// origin's readers unsynced until the peer's initial set for it is in.
	wants: BTreeMap<Path, Option<crate::model::Snapshot>>,
	/// The prefixes asked of the peer.
	asked: BTreeMap<u64, Asked>,
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
/// for more than it wants and never for more than it is granted. Regions that are too
/// many on their own lose the last of them.
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
		match shared.or_else(region) {
			Some(wider) => {
				set.insert(wider);
				set = widest(&set);
			}
			None => {
				// Nothing is left to widen: the grant's regions alone pass one request.
				let dropped = set.pop_last();
				tracing::warn!(?dropped, "a grant too wide for one ANNOUNCE_REQUEST goes unasked");
			}
		}
	}
	set
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
			.map_err(refused)?
			.with_clock(subscriber.runtime.clone());
		Ok(Self {
			me: node.me(),
			stream: Arc::new(stream),
			routing: routing.is_some(),
			permit: permit.clone(),
			root,
			origin: subscriber.origin.clone(),
			wants: BTreeMap::new(),
			asked: BTreeMap::new(),
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
		})
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
		if edit.remove.is_empty() && edit.add.is_empty() {
			return;
		}
		// Refused only for a set with one prefix inside another, which `asks` never builds.
		let _ = self.stream.ask(edit);
	}

	/// Let the origin's readers see each prefix synced whose answer is in: the peer
	/// answered every request, and sent the initial set of each prefix asked for it.
	/// The caller serves what the node queued first.
	pub(in crate::lite) fn release(&mut self) {
		if self.wants.values().all(|snapshot| snapshot.is_none()) {
			return;
		}
		let done = self.finished || self.silent;
		if !done && !self.stream.answered() {
			return;
		}
		for (want, snapshot) in &mut self.wants {
			if snapshot.is_none() {
				continue;
			}
			let Ok(want) = self.root.join(want) else {
				*snapshot = None;
				continue;
			};
			let mut answers = (self.asked.values())
				.filter(|asked| route::within(&asked.path, &want) || route::within(&want, &asked.path));
			if done || answers.all(|asked| asked.synced) {
				*snapshot = None;
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
		for ((origin, path, instance), mut served) in self.served.drain() {
			let unavailable = served.route.route.clone().with_available(false);
			served.route.update(unavailable);
			let left = Left(served.route.dynamic.clone());
			self.stream.orphan(origin, &(path, instance), Box::new(left));
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
		self.stream.recv(Msg::Edited).map_err(refused)?;
		self.reask();
		Ok(())
	}

	/// Apply one message the peer sent on its stream as a publisher.
	pub(in crate::lite) fn receive(&mut self, announce: lite::AnnounceBroadcast) -> Result<(), Error> {
		use lite::AnnounceBroadcast as Wire;
		// Only its floor closes a batch of claim messages.
		let claim = matches!(
			announce,
			Wire::Start { .. } | Wire::Change { .. } | Wire::End { .. } | Wire::Floor { .. } | Wire::Skipped
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
			return Err(Error::ProtocolViolation);
		}
		match announce {
			Wire::Ok => self.ok()?,
			Wire::Synced { prefix } => {
				let asked = (self.asked.get_mut(&prefix)).filter(|asked| asked.live && !asked.synced);
				let asked = asked.ok_or(Error::ProtocolViolation)?;
				asked.synced = true;
				let path = asked.path.clone();
				self.stream.recv(Msg::Synced(path)).map_err(refused)?;
			}
			Wire::Route { offer } => {
				let origin = offer.path.first().ok_or(Error::ProtocolViolation)?.node;
				if !self.named.contains_key(&origin) {
					self.held.push(self.slots.acquire()?);
					let id = self.next_route;
					self.next_route += 1;
					self.named.insert(origin, id);
					self.routes.insert(id, (origin, BTreeSet::new()));
				}
				let offer = Some(offer);
				self.stream.recv(Msg::Route { origin, offer }).map_err(refused)?;
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
				self.stream.recv(Msg::Route { origin, offer }).map_err(refused)?;
			}
			Wire::Dead { hop, far } => self.stream.recv(Msg::Dead { hop, far }).map_err(refused)?,
			Wire::Drain { node } => self.stream.recv(Msg::Drain { node }).map_err(refused)?,
			Wire::Floor { id, revision } => self.floor(id, revision)?,
			Wire::Start {
				suffix,
				instance,
				claim,
			} => {
				let relative = self.decoder.start(Some(suffix))?;
				let id = self.decoder.last();
				let path = relative.as_ref().and_then(|relative| self.root.join(relative).ok());
				// Fields valid alone can still resolve to a path this node cannot hold. The
				// claim keeps its id, and reaches no reader here.
				if path.is_none() {
					self.origin.stats().refuse_route();
				}
				self.above(relative.as_ref(), &claim)?;
				self.held.push(self.slots.acquire()?);
				let wire = WireClaim {
					path,
					relative,
					instance,
					claim,
					origin: None,
					filed: false,
				};
				self.claims.insert(id, wire);
				self.changed(id)?;
			}
			Wire::Change { id, claim } => {
				let wire = self.claims.get(&id).ok_or(Error::ProtocolViolation)?;
				self.above(wire.relative.as_ref(), &claim)?;
				self.claims.get_mut(&id).expect("checked above").claim = claim;
				self.changed(id)?;
			}
			Wire::End { id, lost } => {
				self.decoder.end(id)?;
				let wire = self.claims.remove(&id).ok_or(Error::ProtocolViolation)?;
				self.held.pop();
				let change = match lost {
					true => Change::Lost,
					false => Change::Ended,
				};
				let broadcast = wire.path.map(|path| (path, wire.instance));
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
				} else if let (true, Some(broadcast)) = (wire.filed, broadcast) {
					let changes = vec![(broadcast, change)];
					self.stream.recv(Msg::Announce { changes }).map_err(refused)?;
				}
			}
			// The peer's own requests go to the half answering them, and lite-07 has its
			// own form of a claim.
			Wire::Request { .. }
			| Wire::Active { .. }
			| Wire::Ended { .. }
			| Wire::EndedId { .. }
			| Wire::Update { .. } => {
				return Err(Error::ProtocolViolation);
			}
			Wire::Skipped => {}
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

	/// The claim with Announce ID `id` started or changed: a routing neighbour's waits
	/// for its floor, and any other goes to the node at once.
	fn changed(&mut self, id: u64) -> Result<(), Error> {
		let wire = self.claims.get_mut(&id).expect("a live claim");
		let broadcast = wire.path.clone().map(|path| (path, wire.instance.clone()));
		let change = Change::Live(wire.claim.clone());
		if self.routing {
			// Every forwarding node holds every path another does.
			let broadcast = broadcast.ok_or(Error::ProtocolViolation)?;
			self.pending.live(id, broadcast, change);
			return Ok(());
		}
		// A claim at no path this node can hold reaches no reader here.
		let Some(broadcast) = broadcast else {
			return Ok(());
		};
		let admitted = admits(&self.origin, &self.root, &self.permit, &broadcast.0);
		let change = match (wire.filed, admitted) {
			(_, true) => change,
			(true, false) => Change::Lost,
			(false, false) => return Ok(()),
		};
		wire.filed = admitted;
		let changes = vec![(broadcast, change)];
		self.stream.recv(Msg::Announce { changes }).map_err(refused)
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
			.map_err(refused)
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
		self.permit = permit.clone();
		self.reask();
		if same {
			return Ok(());
		}
		let mut changes = Vec::new();
		for wire in self.claims.values_mut() {
			let Some(path) = &wire.path else {
				continue;
			};
			let admitted = admits(&self.origin, &self.root, &self.permit, path);
			let change = match (wire.filed, admitted) {
				(false, true) => Change::Live(wire.claim.clone()),
				(true, false) => Change::Lost,
				_ => continue,
			};
			wire.filed = admitted;
			changes.push(((path.clone(), wire.instance.clone()), change));
		}
		for change in changes {
			let changes = vec![change];
			self.stream.recv(Msg::Announce { changes }).map_err(refused)?;
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
				instance,
				served,
			} => {
				let key = (origin, path, instance);
				let ok = self.serve(subscriber, &key, served);
				// Dropped, a claim this session may no longer carry goes offline rather
				// than ending: its publisher never withdrew it.
				if !ok {
					self.served.remove(&key);
				}
				let (origin, path, instance) = key;
				self.stream.serving(origin, &(path, instance), ok);
			}
			node::Output::Unserve {
				origin,
				path,
				instance,
				ended,
			} => {
				// Dropped, a claim the node let go of goes offline rather than ending.
				if let Some(served) = self.served.remove(&(origin, path, instance))
					&& ended
				{
					served.route.dynamic.withdrawn();
				}
			}
		}
	}

	/// Serve a claim through this session, or update how it is served. Returns whether
	/// the session may carry it.
	fn serve(&mut self, subscriber: &Subscriber, key: &ServedKey, served: node::Served) -> bool {
		let (origin, claim, instance) = key;
		let Some(path) = claim.strip_prefix(&self.root) else {
			// Outside the session's root, so nothing a request could name here.
			return false;
		};
		// The node may hand over a claim another session of the same peer carries, which
		// is served here only if this session may carry it too.
		if !self.permit.within_limit(path.as_str()) {
			return false;
		}
		// The chain a claim travels to a lite-06 peer: the relays that announced it again,
		// then the node claiming it, so a relay it reaches back through recognizes it. A
		// chain already naming this node looped back across lite-06, and the hop limit
		// bounds the whole path across both versions.
		// A node that adopted the claim from an endpoint already ends the chain.
		let claimant = (*origin != self.me && !served.chain.contains(&origin.relay)).then_some(origin.relay);
		let relays = (served.chain.iter().copied())
			.chain(claimant)
			.filter(|relay| *relay != self.me.relay);
		let mut hops = crate::Hops::new();
		for relay in relays {
			let pushed = route::node::hop(relay).is_some_and(|hop| hops.push(hop).is_ok());
			if !pushed {
				return false;
			}
		}
		if subscriber.names_self(&hops) || hops.len() > self.origin.max_hops().get() {
			tracing::debug!(route = %subscriber.log_path(&path), node = ?origin, "dropping looped claim");
			return false;
		}
		let lane = crate::origin::Lane {
			primary: served.primary,
			priority: Some(served.priority),
			// A receiver refuses a claim above its path by more than the path's segments.
			above: u8::try_from(served.attr.above).unwrap_or(u8::MAX),
		};
		// A claim no route reaches is unavailable: it stays shown, and serves nothing.
		let route = crate::origin::RouteInfo::new()
			.with_instance(instance.clone(), served.attr.epoch)
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
			return true;
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
				true
			}
			// Outside our origin's scope.
			Err(_) => false,
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

/// The node refuses what no node following the rule sends.
fn refused(_: route::Error) -> Error {
	Error::ProtocolViolation
}

/// Whether the node may hold the peer's claim at `path`: the permit's limit admits
/// it, and the origin would serve it rather than refuse it (an excluded subtree or mount).
fn admits(origin: &crate::origin::Producer, root: &Path, permit: &crate::auth::Permit, path: &Path) -> bool {
	origin.publishable(path)
		&& path
			.strip_prefix(root)
			.is_some_and(|relative| permit.within_limit(relative.as_str()))
}

/// Whether `permit` trusts the peer with the whole tree: neither grant confines it,
/// whether by setting no patterns or by one that covers everything.
pub(in crate::lite) fn whole(permit: &crate::auth::Permit) -> bool {
	permit.covers_subtree("")
}

#[cfg(test)]
mod tests;
