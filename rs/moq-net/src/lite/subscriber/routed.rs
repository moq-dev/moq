//! Lite-07: an announce stream this session asked for, feeding the origin's route node.
//!
//! The peer's routes, its DEADs, and its claims go to the [`route::Node`] as they arrive,
//! a route's claim changes in one batch closed by its ANNOUNCE_FLOOR. The node decides
//! which of its routes serve each claim it shows, and this stream puts the ones it is
//! handed into the origin, minting a source per request that names the claim's publisher
//! and the route it follows.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	sync::Arc,
	task::Poll,
};

use crate::{
	Error, Path,
	lite::{self},
	route::{
		self, node,
		path::{Dead, Hop, Moved, Offer, Reach},
	},
	transport::{Metrics as _, poll::Session as _},
};

use super::{AnnouncedRoute, Subscriber};

/// One route the peer sent on the stream, by its Route ID.
struct WireRoute {
	node: route::Id,
	/// The prefix it covers, absolute, or `None` when this node refused the suffix the
	/// peer forwarded, which leaves the route refused for good.
	prefix: Option<Path>,
	/// What the peer offers, or the path of an offer this node refuses, which a DEAD
	/// still matches.
	offer: Result<Offer, Arc<[Hop]>>,
	/// The peer holds it, with no path left.
	held: bool,
	/// The Announce ID of each of its live claims, by path.
	claims: BTreeMap<Path, u64>,
	/// The Announce IDs of its live claims whose path this node refused.
	unnamed: BTreeSet<u64>,
	/// The Announce IDs of its live claims at a path whose content this node refuses.
	withheld: BTreeSet<u64>,
	/// How its claims differ from `committed`, by path, so what the stream keeps
	/// between floors is bounded by its claims rather than the messages it read.
	pending: BTreeMap<Path, Option<node::Claim>>,
	/// Its claims and floor as of that ANNOUNCE_FLOOR, replayed when the stream
	/// files again. `None` until the first.
	committed: Option<(BTreeMap<Path, node::Claim>, Option<route::Rev>)>,
}

impl WireRoute {
	/// Whether its floors may certify its claims: not while this node refuses one, since
	/// the origin never withdrew it.
	fn certifies(&self) -> bool {
		self.unnamed.is_empty() && self.withheld.is_empty()
	}

	/// The claim at `path` is now `claim`, pending the next floor.
	fn change(&mut self, path: Path, claim: Option<node::Claim>) {
		let committed = self.committed.as_ref().and_then(|(claims, _)| claims.get(&path));
		match committed == claim.as_ref() {
			true => self.pending.remove(&path),
			false => self.pending.insert(path, claim),
		};
	}

	/// The path the peer last named for it.
	fn path(&self) -> &[Hop] {
		match &self.offer {
			Ok(offer) => &offer.path,
			Err(path) => path,
		}
	}

	/// How the node holds it, or `None` while this node refuses what the peer forwarded:
	/// the node then holds no such route, as if the peer had ended it.
	fn reach(&self) -> Option<Reach> {
		self.prefix.as_ref()?;
		let offer = self.offer.as_ref().ok().filter(|_| self.node != 0)?;
		Some(match self.held {
			true => Reach::Held,
			false => Reach::Finite(offer.clone()),
		})
	}
}

/// One live claim the peer sent, by its Announce ID.
struct WireClaim {
	route: u64,
	/// Absolute: the origin node keys every claim from the origin's root. `None` when this
	/// node refused the path, which the claim then never has.
	path: Option<Path>,
	/// `None` while this node refuses what the peer forwarded, until an ANNOUNCE_RESTART
	/// names all of it again.
	claim: Option<node::Claim>,
}

/// A claim the node serves through this stream, along one of its routes.
struct ServedClaim {
	/// The claim's publisher as the peer names it, carried on every request.
	named: route::Id,
	/// How many leading segments of the request's path make up the route's prefix,
	/// carried on every request.
	depth: u64,
	route: AnnouncedRoute<ServedKey>,
}

/// A served claim's identity, its path, and the prefix of the route it is served along.
type ServedKey = (route::Id, Path, Path);

/// The route layer one lite-07 announce stream feeds.
pub(super) struct Routed {
	node: node::Node,
	stream: node::Stream,
	/// The session the stream belongs to.
	peer: node::Peer,
	/// The link the stream filed under, its scope aside.
	link: route::Link,
	/// The permit it filed under. The node holds only the peer's claims its limit
	/// admits; the regions alone can be wider.
	permit: crate::auth::Permit,
	/// The peer's SETUP, whose proven route key files a scoped peer's identities.
	peer_setup: crate::lite::PeerSetup,
	/// Files them instead while the peer proved none: this session's own.
	session_key: [u8; 32],
	/// The requested prefix, absolute.
	prefix: Path,
	root: Path,
	/// The session's origin handle, whose exclusions and mounts also gate the claims.
	origin: crate::origin::Producer,
	routes: HashMap<u64, WireRoute>,
	/// The live Route ID of each publisher and prefix: the stream carries at most one.
	named: HashMap<(route::Id, Path), u64>,
	next_route: u64,
	claims: HashMap<u64, WireClaim>,
	/// The session's announce cap: one slot per live route and claim.
	slots: crate::session::Slots,
	held: Vec<crate::session::Slot>,
	served: HashMap<ServedKey, ServedClaim>,
	/// What the stream served before it filed again, while the new one takes it up.
	previous: HashMap<ServedKey, ServedClaim>,
	/// Served claims whose request queue woke since the last pass.
	ready: kio::Queue<ServedKey>,
	/// The peer sent a GOAWAY: every claim served here ranks last.
	drained: bool,
	/// The link's min RTT, which the peer's routes add.
	rtt: route::LinkRtt,
	/// When to read the link's RTT next, or `None` once it stopped reading.
	read_rtt: Option<crate::time::Deadline>,
	/// Whether the peer has a route here, so the link's RTT is read.
	sampling: bool,
	/// Counts what this node refuses of the peer's routes and claims.
	stats: crate::stats::Session,
}

impl Routed {
	/// File a stream asking the peer for `prefix` (relative to the session's root).
	pub(super) fn new(
		subscriber: &Subscriber,
		prefix: &Path,
		link_cost: u64,
		permit: &crate::auth::Permit,
	) -> Result<Self, Error> {
		let root = subscriber.origin.root().clone();
		let prefix = root.join(prefix)?;
		let link = route::Link {
			// A zero-cost link never reaches here: SETUP refuses it on lite-07.
			cost: link_cost.max(1),
			priority: subscriber.origin.preference().value(),
			upstream: subscriber.origin.link() == crate::origin::Link::Upstream,
			// Only a stream this node answers is sent routes.
			proxy: false,
			scope: None,
		};
		let node = subscriber.origin.route_node();
		let session_key = {
			use rand::RngExt;
			kio::clock::rng().random()
		};
		let peer_setup = subscriber.peer_setup.clone();
		let peer = node::Peer {
			tag: peer_setup.tag(),
			node: peer_setup.node().map_or(0, |node| node.get()),
		};
		let key = peer_setup.route_key().unwrap_or(session_key);
		let stream = node.receive(peer, scoped(&link, &root, permit, key)).map_err(refused)?;
		Ok(Self {
			node,
			stream,
			peer,
			link,
			permit: permit.clone(),
			peer_setup,
			session_key,
			prefix,
			root,
			origin: subscriber.origin.clone(),
			routes: HashMap::new(),
			named: HashMap::new(),
			next_route: 0,
			claims: HashMap::new(),
			slots: subscriber.announces.clone(),
			held: Vec::new(),
			served: HashMap::new(),
			previous: HashMap::new(),
			ready: Default::default(),
			drained: false,
			rtt: Default::default(),
			read_rtt: None,
			sampling: false,
			stats: subscriber.origin.stats().clone(),
		})
	}

	/// Apply one message the peer sent on the stream.
	pub(super) fn receive(
		&mut self,
		prefix: &Path,
		announce: lite::AnnounceBroadcast,
		decoder: &mut lite::AnnounceDecoder,
	) -> Result<(), Error> {
		use lite::AnnounceBroadcast as Msg;
		match announce {
			Msg::RouteStart { node, suffix, offer } => self.route_start(node, Some(suffix), Ok(offer))?,
			Msg::RouteUpdate { id, offer } => self.offer(id, Ok(offer))?,
			Msg::RouteHold { id } => {
				let route = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				let before = route.reach();
				route.held = true;
				self.sync(id, before);
			}
			Msg::RouteEnd { id } => {
				let route = self.end_route(id, decoder)?;
				if let (Some(_), Some(prefix)) = (route.reach(), route.prefix) {
					self.stream.route(route.node, prefix, None);
				}
			}
			Msg::Dead(dead) => self.dead(dead, decoder)?,
			Msg::Floor { id, revision } => {
				let route = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				let revision = revision.filter(|_| route.certifies());
				let changes = std::mem::take(&mut route.pending);
				let (committed, floor) = route.committed.get_or_insert_default();
				for (path, claim) in &changes {
					match claim {
						Some(claim) => committed.insert(path.clone(), claim.clone()),
						None => committed.remove(path),
					};
				}
				*floor = revision;
				// A refused route files its claims once the peer forwards it usably again.
				if let Some(prefix) = route.prefix.clone().filter(|_| route.reach().is_some()) {
					let node = route.node;
					self.file(node, prefix, changes, revision);
				}
			}
			Msg::Active {
				suffix,
				route,
				instance,
				epoch,
				hops,
				cost,
			} => {
				let path = decoder.start(Some(suffix))?;
				let path = self.claim_path(prefix, path);
				let claim = claim(instance, epoch, cost, hops)?;
				let id = decoder.last();
				// Fields valid alone can still resolve, or join the prefix, past the path limits.
				if path.is_none() {
					let reason = lite::Refusal::Path;
					tracing::warn!(route, claim = id, %reason, "refusing a forwarded claim");
					self.stats.refuse_route();
				}
				self.start(id, route, path, Some(claim))?;
			}
			Msg::Update { id, hops, cost } => {
				let claim = self.claims.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				// An update names none of what a refused claim lacks.
				if let Some(held) = &mut claim.claim {
					held.cost = cost.value();
					held.chain = hops;
					let change = claim.claim.clone();
					self.pending(id, change)?;
				}
			}
			Msg::Restart {
				id,
				instance,
				epoch,
				cost,
				hops,
			} => {
				let fresh = self::claim(instance, epoch, cost, hops)?;
				let claim = self.claims.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				claim.claim = Some(fresh);
				if let Some(route) = self.routes.get_mut(&claim.route) {
					route.withheld.remove(&id);
				}
				let change = claim.claim.clone();
				self.pending(id, change)?;
			}
			Msg::EndedId { id } => {
				decoder.end(id)?;
				self.end(id)?;
			}
			Msg::Refused { message, reason } => self.refused(prefix, message, reason, decoder)?,
			// Lite-07 ends a claim by its Announce ID only.
			Msg::Ended { .. } => return Err(Error::ProtocolViolation),
			// Only a relay choosing a parent per broadcast uses warm routes, which this
			// endpoint does not do.
			Msg::WarmStart { .. } | Msg::WarmUpdate { .. } | Msg::WarmEnd { .. } => {}
			Msg::Skipped | Msg::Synced => {}
		}
		Ok(())
	}

	/// The peer started a route to `node` with the next Route ID, covering the stream's
	/// prefix joined with `suffix`, along `offer`, or along the path of an offer this
	/// node refuses. A route with no suffix, or to the reserved node, stays refused.
	fn route_start(
		&mut self,
		node: route::Id,
		suffix: Option<Path>,
		offer: Result<Offer, Arc<[Hop]>>,
	) -> Result<(), Error> {
		// Fields valid alone can still join the prefix past the path limits.
		let prefix = suffix.and_then(|suffix| self.prefix.join(&suffix).ok());
		let key = prefix.clone().filter(|_| node != 0).map(|prefix| (node, prefix));
		// The peer keeps one route per publisher and prefix.
		if key.as_ref().is_some_and(|key| self.named.contains_key(key)) {
			return Err(Error::ProtocolViolation);
		}
		self.held.push(self.slots.acquire()?);
		let id = self.next_route;
		self.next_route += 1;
		if let Some(key) = key {
			self.named.insert(key, id);
		}
		let route = WireRoute {
			node,
			prefix,
			offer,
			held: false,
			claims: BTreeMap::new(),
			unnamed: BTreeSet::new(),
			withheld: BTreeSet::new(),
			pending: BTreeMap::new(),
			committed: None,
		};
		self.routes.insert(id, route);
		self.sync(id, None);
		Ok(())
	}

	/// The route with Route ID `id` now offers `offer`, or the path of an offer this node
	/// refuses. Either way the peer no longer holds it.
	fn offer(&mut self, id: u64, offer: Result<Offer, Arc<[Hop]>>) -> Result<(), Error> {
		let route = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
		let before = route.reach();
		route.offer = offer;
		route.held = false;
		self.sync(id, before);
		Ok(())
	}

	/// Tell the node what became of the route with Route ID `id`, which it held as
	/// `before`. A route this node refused files again once usable, as if the peer had
	/// ended it and started it over, with the claims of its last floor.
	fn sync(&self, id: u64, before: Option<Reach>) {
		let route = &self.routes[&id];
		let (Some(prefix), after) = (&route.prefix, route.reach()) else {
			return;
		};
		match (before, after) {
			(None, None) => {}
			(Some(_), None) => self.stream.route(route.node, prefix.clone(), None),
			(None, Some(_)) => {
				file_route(&self.stream, route);
				if let Some((claims, floor)) = &route.committed {
					let claims = claims.iter().map(|(path, claim)| (path.clone(), Some(claim.clone())));
					self.file(route.node, prefix.clone(), claims.collect(), *floor);
				}
			}
			(Some(_), Some(reach)) => self.stream.route(route.node, prefix.clone(), Some(reach)),
		}
	}

	/// End the route with Route ID `id` and every claim under it, as ROUTE_END does.
	fn end_route(&mut self, id: u64, decoder: &mut lite::AnnounceDecoder) -> Result<WireRoute, Error> {
		let route = self.routes.remove(&id).ok_or(Error::ProtocolViolation)?;
		if let Some(prefix) = &route.prefix
			&& self.named.get(&(route.node, prefix.clone())) == Some(&id)
		{
			self.named.remove(&(route.node, prefix.clone()));
		}
		for claim in route.claims.values().chain(&route.unnamed) {
			decoder.end(*claim)?;
			self.claims.remove(claim);
			self.held.pop();
		}
		self.held.pop();
		Ok(route)
	}

	/// A DEAD: the stream's routes through the hop it names carry on as it says, which the
	/// node is told as the peer's news. A route this node refuses still follows it, so
	/// the two ends agree on which routes live.
	fn dead(&mut self, dead: lite::Dead, decoder: &mut lite::AnnounceDecoder) -> Result<(), Error> {
		let (hop, far, gone, moved) = match dead {
			lite::Dead::Drain { node } => {
				self.stream.dead(Dead::Draining { node });
				return Ok(());
			}
			lite::Dead::Closed { hop, far, gone, moved } => (hop, far, gone, moved),
		};
		let mut offers = HashMap::new();
		for (id, heard) in moved {
			if !self.routes.contains_key(&id) {
				return Err(Error::ProtocolViolation);
			}
			let heard = heard.map_err(|refused| {
				tracing::warn!(route = id, reason = %refused.reason, "refusing a forwarded route");
				self.stats.refuse_route();
				refused.path
			});
			offers.insert(id, heard);
		}
		let mut ids: Vec<u64> = (self.routes.iter())
			.filter(|(_, route)| route.path().contains(&hop))
			.map(|(id, _)| *id)
			.collect();
		ids.sort_unstable();
		let own = self.node.hop();
		let mut filed = Vec::new();
		let mut ended = Vec::new();
		// Routes this node starts or stops refusing, told to the node after the DEAD.
		let mut changed = Vec::new();
		for id in ids {
			let route = self.routes.get_mut(&id).expect("listed above");
			let before = route.reach();
			let path = route.path();
			let child = path.len() >= 2 && path[path.len() - 2].node == own;
			match offers.remove(&id) {
				Some(offer) => {
					route.offer = offer;
					route.held = false;
					match (&before, route.reach(), &route.prefix) {
						(Some(_), Some(Reach::Finite(offer)), Some(prefix)) => filed.push(Moved {
							id: route.node,
							prefix: prefix.clone(),
							offer,
						}),
						_ => changed.push((id, before)),
					}
				}
				None if gone => ended.push(id),
				// The peer learned it from this node, which it holds under: unchanged.
				None if child => {}
				None => route.held = true,
			}
		}
		for id in ended {
			self.end_route(id, decoder)?;
		}
		self.stream.dead(Dead::Closed {
			hop,
			far,
			moved: filed,
			gone,
		});
		for (id, before) in changed {
			self.sync(id, before);
		}
		Ok(())
	}

	/// Apply a message whose content, which the peer only forwarded, this node refuses:
	/// withdraw the route or claim it names, and keep the stream.
	fn refused(
		&mut self,
		prefix: &Path,
		message: lite::Refusable,
		reason: lite::Refusal,
		decoder: &mut lite::AnnounceDecoder,
	) -> Result<(), Error> {
		use lite::Refusable;
		match message {
			Refusable::RouteStart { node, suffix, path } => {
				tracing::warn!(node, %reason, "refusing a forwarded route");
				self.route_start(node, suffix, Err(path))?;
			}
			Refusable::RouteUpdate { id, path } => {
				let node = self.routes.get(&id).ok_or(Error::ProtocolViolation)?.node;
				tracing::warn!(node, route = id, %reason, "refusing a forwarded route");
				self.offer(id, Err(path))?;
			}
			Refusable::Start { route, suffix } => {
				let path = decoder.start(suffix)?;
				let path = self.claim_path(prefix, path);
				let id = decoder.last();
				tracing::warn!(route, claim = id, %reason, "refusing a forwarded claim");
				self.start(id, route, path, None)?;
			}
			Refusable::Change { id } => {
				let claim = self.claims.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				tracing::warn!(route = claim.route, claim = id, %reason, "refusing a forwarded claim");
				claim.claim = None;
				if claim.path.is_some() {
					let route = self.routes.get_mut(&claim.route).ok_or(Error::ProtocolViolation)?;
					route.withheld.insert(id);
				}
				self.pending(id, None)?;
			}
			// Warm routes are ignored here anyway, so refusing one changes nothing.
			Refusable::WarmStart { route } => {
				tracing::warn!(route, %reason, "refusing a forwarded warm route");
			}
			Refusable::WarmUpdate { id } => {
				tracing::warn!(warm = id, %reason, "refusing a forwarded warm route");
			}
		}
		self.stats.refuse_route();
		Ok(())
	}

	/// Hand the node a batch of the route to `node` covering `prefix`, leaving out what
	/// the limit does not admit, which the node never held, so it neither starts nor ends.
	fn file(
		&self,
		node: route::Id,
		prefix: Path,
		changes: BTreeMap<Path, Option<node::Claim>>,
		revision: Option<route::Rev>,
	) {
		let changes = (changes.into_iter())
			.filter(|(path, _)| admits(&self.origin, &self.root, &self.permit, path))
			.collect();
		self.stream.batch(node, prefix, changes, revision);
	}

	/// The absolute path of a claim under the stream's prefix, or `None` when it, or the
	/// suffix it joins, is no path this node can hold.
	fn claim_path(&self, prefix: &Path, suffix: Option<Path>) -> Option<Path> {
		let path = prefix.join(&suffix?).ok()?;
		self.root.join(&path).ok()
	}

	/// A claim the peer started under `route`, assigned Announce ID `id`, at `path` unless
	/// this node refused it, and `None` while this node refuses what the peer forwarded.
	fn start(&mut self, id: u64, route: u64, path: Option<Path>, claim: Option<node::Claim>) -> Result<(), Error> {
		let wire = self.routes.get_mut(&route).ok_or(Error::ProtocolViolation)?;
		// A route covers only the claims at or below its prefix.
		let outside = |path: &Path| (wire.prefix.as_ref()).is_some_and(|prefix| !route::within(path, prefix));
		match &path {
			Some(path) if wire.claims.contains_key(path) || outside(path) => return Err(Error::ProtocolViolation),
			Some(path) => {
				wire.claims.insert(path.clone(), id);
				wire.change(path.clone(), claim.clone());
				if claim.is_none() {
					wire.withheld.insert(id);
				}
			}
			None => {
				wire.unnamed.insert(id);
			}
		}
		self.held.push(self.slots.acquire()?);
		// A claim without a path stays refused, whatever the peer sends for it.
		let claim = claim.filter(|_| path.is_some());
		self.claims.insert(id, WireClaim { route, path, claim });
		Ok(())
	}

	/// The peer ended the claim with Announce ID `id`.
	fn end(&mut self, id: u64) -> Result<(), Error> {
		let claim = self.claims.remove(&id).ok_or(Error::ProtocolViolation)?;
		self.held.pop();
		let route = self.routes.get_mut(&claim.route).ok_or(Error::ProtocolViolation)?;
		match claim.path {
			Some(path) => {
				route.claims.remove(&path);
				route.withheld.remove(&id);
				route.change(path, None);
			}
			None => {
				route.unnamed.remove(&id);
			}
		}
		Ok(())
	}

	/// The claim with Announce ID `id` is now `change`, pending its route's next floor.
	fn pending(&mut self, id: u64, change: Option<node::Claim>) -> Result<(), Error> {
		let claim = self.claims.get_mut(&id).ok_or(Error::ProtocolViolation)?;
		let Some(path) = claim.path.clone() else {
			// Nothing reaches the node for a claim without a path.
			claim.claim = None;
			return Ok(());
		};
		let route = self.routes.get_mut(&claim.route).ok_or(Error::ProtocolViolation)?;
		route.change(path, change);
		Ok(())
	}

	/// Move what the stream holds onto a budget of its own with the session's cap, while a
	/// request replacing it takes the same claims over on the session's. The session's
	/// count is then the replacement's alone, and a peer flooding this stream fails it
	/// on its own budget: at most twice the cap, during one handover.
	pub(super) fn hand_over(&mut self) {
		let slots = self.slots.split();
		for slot in self.held.iter_mut() {
			*slot = slots.hold();
		}
		self.slots = slots;
	}

	/// The peer sent a GOAWAY: its routes rank below every other. Idempotent.
	pub(super) fn goaway(&mut self, subscriber: &Subscriber) {
		if self.drained {
			return;
		}
		self.drained = true;
		self.stream.goaway();
		for served in self.served.values_mut() {
			served.route.drain();
		}
		self.outputs(subscriber);
	}

	/// The session's permit changed. A new limit files the stream again, the peer's
	/// routes and the claims the limit admits replayed before the old stream lets go:
	/// it may change the scope, the claims admitted, the claims the node may serve
	/// here, and what the floors certify. The peer still holds each claim either way.
	pub(super) fn limit(&mut self, subscriber: &Subscriber, permit: &crate::auth::Permit) -> Result<(), Error> {
		let same = permit.same_limit(&self.permit);
		self.permit = permit.clone();
		if same {
			return Ok(());
		}
		let key = self.peer_setup.route_key().unwrap_or(self.session_key);
		let stream = self
			.node
			.receive(self.peer, scoped(&self.link, &self.root, permit, key))
			.map_err(refused)?;
		stream.rtt(self.rtt.reported());
		// What the old stream served stays in the origin until the new one serves it
		// again, or lets go after.
		let previous = std::mem::take(&mut self.served);
		let mut ids: Vec<_> = self.routes.keys().copied().collect();
		ids.sort_unstable();
		for id in &ids {
			file_route(&stream, &self.routes[id]);
		}
		for id in &ids {
			let route = &self.routes[id];
			let prefix = route.prefix.as_ref().filter(|_| route.reach().is_some());
			if let (Some(prefix), Some((claims, floor))) = (prefix, &route.committed) {
				let changes = (claims.iter())
					.filter(|(path, _)| admits(&self.origin, &self.root, &self.permit, path))
					.map(|(path, claim)| (path.clone(), Some(claim.clone())));
				stream.batch(route.node, prefix.clone(), changes.collect(), *floor);
			}
		}
		if self.drained {
			stream.goaway();
		}
		// The old stream retires once the new one carries what it did.
		drop(std::mem::replace(&mut self.stream, stream));
		self.previous = previous;
		self.outputs(subscriber);
		// What the new stream no longer serves is let go here, not ended by its origin.
		self.previous.clear();
		Ok(())
	}

	/// Act on everything the node queued for the stream: serve the claims it hands
	/// over.
	pub(super) fn outputs(&mut self, subscriber: &Subscriber) {
		self.sample(subscriber);
		while let Some(output) = self.stream.try_output() {
			self.output(subscriber, output);
		}
	}

	fn output(&mut self, subscriber: &Subscriber, output: node::Output) {
		match output {
			node::Output::Serve {
				id,
				named,
				claim,
				prefix,
				served,
			} => self.serve(subscriber, (id, claim, prefix), named, served),
			node::Output::Unserve {
				id,
				claim,
				prefix,
				ended,
			} => {
				// Dropped, a claim the node let go of goes offline rather than ending.
				if let Some(served) = self.served.remove(&(id, claim, prefix))
					&& ended
				{
					served.route.dynamic.withdrawn();
				}
			}
			// A stream this node asked for is never sent routes.
			node::Output::Route { .. } | node::Output::Batch { .. } | node::Output::Dead(_) => {}
		}
	}

	/// Serve a claim of `id` through this stream along its route covering `prefix`, or
	/// update how it is served.
	fn serve(&mut self, subscriber: &Subscriber, key: ServedKey, named: route::Id, served: node::Served) {
		let (id, claim, prefix) = &key;
		let Some(path) = claim.strip_prefix(&self.root) else {
			// Outside the session's root, so nothing a request could name here.
			return;
		};
		// A route reaches the peer's publisher, but the scope's regions can be wider
		// than its limit: a claim another session of the same peer carries is served
		// here only if this session may carry it too. Left unconfirmed, the node keeps
		// it where the limit admits it.
		if !self.permit.within_limit(path.as_str()) {
			return;
		}
		// Requests name the prefix of the route the peer sent, which a scoped stream may
		// have narrowed to a region, as the session names paths, under its root.
		let holds = |above: &Path| {
			(self.named.get(&(named, above.clone()))).is_some_and(|id| self.routes[id].reach().is_some())
		};
		let sent = (crate::route::ancestors(prefix).rev())
			.find(|above| holds(above))
			.unwrap_or_else(|| prefix.clone());
		let Some(depth) = sent.strip_prefix(&self.root).map(|sent| sent.parts().count() as u64) else {
			return;
		};
		// The chain a claim of `id` travels to a lite-06 peer: the one it crossed into
		// lite-07 with, then its publisher, so a relay it reaches back through
		// recognizes it. A chain already naming this node looped back across lite-06,
		// and the hop limit bounds the whole path across both versions.
		let mut hops = served.claim.chain.clone();
		if hops.push(route::node::hop(*id)).is_err()
			|| subscriber.names_self(&hops)
			|| hops.len() > self.origin.max_hops().get()
		{
			tracing::debug!(route = %subscriber.log_path(&path), node = id, "dropping looped claim");
			for entry in self.previous.remove(&key).into_iter().chain(self.served.remove(&key)) {
				entry.route.dynamic.withdrawn();
			}
			return;
		}
		let lane = crate::origin::Lane {
			depth: prefix.parts().count() as u64,
			primary: served.primary,
			selected: served.selected,
			path: served.path.clone(),
		};
		// A route held without a path to it is unavailable: it serves only as a last
		// resort, like a draining one, at its real cost.
		let route = crate::origin::RouteInfo::new()
			.with_instance(served.claim.instance.clone(), served.claim.epoch)
			.with_hops(hops)
			.with_cost(served.claim.cost.saturating_add(served.metric))
			.with_via(subscriber.via(None))
			.with_node(Some(*id))
			.with_lane(lane)
			.with_draining(self.drained || served.draining)
			.with_available(served.available)
			.with_rtt(served.rtt);
		if let Some(entry) = self.previous.remove(&key) {
			self.served.insert(key.clone(), entry);
		}
		if let Some(entry) = self.served.get_mut(&key) {
			let same = entry.route.route.instance == route.instance && entry.route.route.epoch == route.epoch;
			if same {
				entry.named = named;
				entry.depth = depth;
				entry.route.update(route);
				self.stream.serving(*id, claim, prefix, &served.claim);
				return;
			}
		}
		// A new instance or epoch replaces the old route, attached before it goes.
		tracing::debug!(route = %subscriber.log_path(&path), node = id, "announce");
		match subscriber.origin.forward(&path, route.clone()) {
			Ok(dynamic) => {
				let wake = crate::util::QueueWake::new(key.clone(), self.ready.clone());
				let entry = AnnouncedRoute::new(route, dynamic, wake);
				entry.waker.wake_by_ref();
				self.stream.serving(*id, claim, prefix, &served.claim);
				let served = ServedClaim {
					named,
					depth,
					route: entry,
				};
				self.served.insert(key, served);
			}
			Err(_) => {
				// Outside our origin's scope.
				self.served.remove(&key);
			}
		}
	}

	/// Start or stop reading the link's min RTT as the peer's routes come and go.
	///
	/// Only a route the peer sent adds the link's min RTT, so a peer sending none costs
	/// no timer. Starting reads at once, so the first claims served already carry it,
	/// and in a fresh window, so a pause never keeps a reading older than the window.
	fn sample(&mut self, subscriber: &Subscriber) {
		match (self.routes.is_empty(), self.sampling) {
			(true, true) => {
				self.sampling = false;
				self.rtt = Default::default();
				self.read_rtt = None;
				self.stream.rtt(None);
			}
			(false, false) => {
				self.sampling = true;
				self.read_link(subscriber);
			}
			_ => {}
		}
	}

	/// Read the link's smoothed RTT once and schedule the next read.
	fn read_link(&mut self, subscriber: &Subscriber) {
		let last = self.rtt.reported();
		let now = self.rtt.read(subscriber.session.metrics().rtt());
		if now != last {
			self.stream.rtt(now);
		}
		self.read_rtt =
			(self.rtt.reading()).then(|| crate::time::Deadline::after(&subscriber.runtime, route::LinkRtt::INTERVAL));
	}

	/// Serve requests on every claim whose queue woke.
	pub(super) fn poll(&mut self, subscriber: &Subscriber, waiter: &kio::Waiter) -> Result<(), Error> {
		self.sample(subscriber);
		while let Some(read_rtt) = &mut self.read_rtt
			&& read_rtt.poll(waiter).is_ready()
		{
			self.read_link(subscriber);
		}
		while let Poll::Ready(output) = self.stream.poll_output(waiter) {
			self.output(subscriber, output);
		}
		while let Poll::Ready(Ok(key)) = self.ready.poll_pop(waiter) {
			let Some(entry) = self.served.get_mut(&key) else {
				continue;
			};
			super::mint_sources(subscriber, &mut entry.route, (entry.named, entry.depth));
		}
		Ok(())
	}
}

/// File `route` on `stream` as the node holds it, if it does: its offer, then whether
/// the peer holds it.
fn file_route(stream: &node::Stream, route: &WireRoute) {
	let (Some(prefix), Ok(offer), Some(_)) = (&route.prefix, &route.offer, route.reach()) else {
		return;
	};
	stream.route(route.node, prefix.clone(), Some(Reach::Finite(offer.clone())));
	if route.held {
		stream.route(route.node, prefix.clone(), Some(Reach::Held));
	}
}

/// The node refuses a stream only on a zero-cost link, which SETUP already refused.
fn refused(_: route::Error) -> Error {
	Error::ProtocolViolation
}

/// A claim off the wire. Lite-07 names every claim's instance.
fn claim(
	instance: Option<crate::Instance>,
	epoch: u64,
	cost: crate::origin::Cost,
	chain: crate::Hops,
) -> Result<node::Claim, Error> {
	Ok(node::Claim {
		instance: instance.ok_or(Error::ProtocolViolation)?,
		epoch,
		cost: cost.value(),
		upstream: false,
		learned: false,
		chain,
	})
}

/// Whether the node may hold the peer's claim at `path`: the permit's limit admits
/// it, and the origin would serve it rather than refuse it (an excluded subtree or mount).
fn admits(origin: &crate::origin::Producer, root: &Path, permit: &crate::auth::Permit, path: &Path) -> bool {
	origin.publishable(path)
		&& path
			.strip_prefix(root)
			.is_some_and(|relative| permit.within_limit(relative.as_str()))
}

/// The absolute regions a permit confines the peer's claims to.
fn regions(root: &Path, permit: &crate::auth::Permit) -> Option<Vec<Path>> {
	let regions = permit.regions()?;
	Some(regions.iter().filter_map(|region| root.join(region).ok()).collect())
}

/// The link a scoped peer's routes arrive on: its identities qualified under the route
/// key it proved, and under the limit too when that admits less than its regions hold.
/// A floor certifies what reaches the node, which such a limit filters, so sessions file
/// an identity alike only where their floors certify the same view of it.
fn scoped(link: &route::Link, root: &Path, permit: &crate::auth::Permit, key: [u8; 32]) -> route::Link {
	let regions = regions(root, permit);
	let whole = (regions.iter().flatten()).all(|region| {
		(region.strip_prefix(root)).is_some_and(|relative| permit.within_limit_subtree(relative.as_str()))
	});
	let key = match (whole, permit.limit()) {
		(false, Some(limit)) => {
			let mut ctx = rind::digest::Context::new(&rind::digest::SHA256);
			ctx.update(b"moq-lite limited route key");
			ctx.update(&key);
			for pattern in limit.iter() {
				let pattern = pattern.to_string();
				ctx.update(&(pattern.len() as u64).to_le_bytes());
				ctx.update(pattern.as_bytes());
			}
			ctx.finish().as_ref().try_into().expect("a SHA-256 digest is 32 bytes")
		}
		_ => key,
	};
	route::Link {
		scope: regions.map(|regions| route::Scope {
			regions,
			qualify: route::qualifier(key),
		}),
		..link.clone()
	}
}

#[cfg(test)]
mod tests;
