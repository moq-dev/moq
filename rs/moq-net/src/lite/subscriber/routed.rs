//! Lite-07: an announce stream this session asked for, feeding the origin's route node.
//!
//! The peer's routes and claims go to the [`route::Node`] as they arrive, a route's
//! claim changes in one batch closed by its ANNOUNCE_FLOOR. The node decides which
//! stream serves each claim it shows, and this stream puts the ones it is handed into
//! the origin, minting a source per request that names the claim's origin node.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	task::Poll,
};

use crate::{
	Error, Path,
	lite::{self, AnnounceReply},
	route::{self, node},
	transport::{Metrics as _, poll::Session as _},
};

use super::{AnnouncedRoute, Subscriber};

/// One route the peer sent on the stream, by its Route ID.
struct WireRoute {
	node: route::Id,
	/// How the peer reaches the node, or `None` while this node refuses what the peer
	/// forwarded: the node holds no such route, as if the peer had ended it.
	reach: Option<route::Reach>,
	/// The Announce ID of each of its live claims, by path.
	claims: BTreeMap<Path, u64>,
	/// The Announce IDs of its live claims whose path this node refused.
	unnamed: BTreeSet<u64>,
	/// How its claims differ from `committed`, by path, so what the stream keeps
	/// between floors is bounded by its claims rather than the messages it read.
	pending: BTreeMap<Path, Option<node::Claim>>,
	/// Its claims and floor as of that ANNOUNCE_FLOOR, replayed when the stream
	/// files again. `None` until the first.
	committed: Option<(BTreeMap<Path, node::Claim>, Option<route::Rev>)>,
}

impl WireRoute {
	/// The claim at `path` is now `claim`, pending the next floor.
	fn change(&mut self, path: Path, claim: Option<node::Claim>) {
		let committed = self.committed.as_ref().and_then(|(claims, _)| claims.get(&path));
		match committed == claim.as_ref() {
			true => self.pending.remove(&path),
			false => self.pending.insert(path, claim),
		};
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

/// A claim the node serves through this stream.
struct ServedClaim {
	/// The claim's origin node as the peer names it, carried on every request.
	named: route::Id,
	route: AnnouncedRoute<(route::Id, Path)>,
}

/// The route layer one lite-07 announce stream feeds.
pub(super) struct Routed {
	node: node::Node,
	stream: node::Stream,
	/// The link the stream filed under, its scope aside.
	link: route::Link,
	/// The scope's regions it filed under, or `None` unscoped.
	regions: Option<Vec<Path>>,
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
	next_route: u64,
	claims: HashMap<u64, WireClaim>,
	/// The session's announce cap: one slot per live route and claim.
	slots: crate::session::Slots,
	held: Vec<crate::session::Slot>,
	/// The slots of routes the peer ended, kept until their ROUTE_END echo is flushed,
	/// so a peer leaving its replies unread holds its routes' slots.
	echoing: Vec<crate::session::Slot>,
	/// The ROUTE_END echoes not yet written, each holding a slot in `echoing`.
	ends: Vec<u64>,
	served: HashMap<(route::Id, Path), ServedClaim>,
	/// What the stream served before it filed again, while the new one takes it up.
	previous: HashMap<(route::Id, Path), ServedClaim>,
	/// Served claims whose request queue woke since the last pass.
	ready: kio::Queue<(route::Id, Path)>,
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
		let key = peer_setup.route_key().unwrap_or(session_key);
		let regions = regions(&root, permit);
		let stream = node
			.receive(scoped(&link, &regions, key), prefix.clone())
			.map_err(refused)?;
		Ok(Self {
			node,
			stream,
			link,
			regions,
			permit: permit.clone(),
			peer_setup,
			session_key,
			prefix,
			root,
			origin: subscriber.origin.clone(),
			routes: HashMap::new(),
			next_route: 0,
			claims: HashMap::new(),
			slots: subscriber.announces.clone(),
			held: Vec::new(),
			echoing: Vec::new(),
			ends: Vec::new(),
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
			Msg::RouteStart { node, advert } => self.route_start(node, Some(route::Reach::Finite(advert)))?,
			Msg::RouteUpdate { id, advert } => self.reach(id, Some(route::Reach::Finite(advert)))?,
			Msg::RouteHold { id } => {
				let route = self.routes.get(&id).ok_or(Error::ProtocolViolation)?;
				// A refused route has nothing to hold.
				if route.reach.is_some() {
					self.reach(id, Some(route::Reach::Held))?;
				}
			}
			Msg::RouteEnd { id } => {
				let route = self.routes.remove(&id).ok_or(Error::ProtocolViolation)?;
				for claim in route.claims.into_values().chain(route.unnamed) {
					decoder.end(claim)?;
					self.claims.remove(&claim);
					self.held.pop();
				}
				self.echoing.extend(self.held.pop());
				if route.reach.is_some() {
					self.stream.route(route.node, None);
				}
				self.ends.push(id);
			}
			Msg::Floor { id, revision } => {
				let route = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
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
				if route.reach.is_some() {
					let node = route.node;
					self.file(node, changes, revision);
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
			Refusable::RouteStart { node } => {
				tracing::warn!(node, %reason, "refusing a forwarded route");
				self.route_start(node, None)?;
			}
			Refusable::RouteUpdate { id } => {
				let node = self.routes.get(&id).ok_or(Error::ProtocolViolation)?.node;
				tracing::warn!(node, route = id, %reason, "refusing a forwarded route");
				self.reach(id, None)?;
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

	/// The peer started a route to `node` with the next Route ID, reaching it by `reach`,
	/// or `None` when this node refuses what the peer forwarded.
	fn route_start(&mut self, node: route::Id, reach: Option<route::Reach>) -> Result<(), Error> {
		// The peer keeps one route per origin node; a refused reserved node is none.
		if node != 0 && self.routes.values().any(|route| route.node == node) {
			return Err(Error::ProtocolViolation);
		}
		self.held.push(self.slots.acquire()?);
		let id = self.next_route;
		self.next_route += 1;
		self.routes.insert(
			id,
			WireRoute {
				node,
				reach: reach.clone(),
				claims: BTreeMap::new(),
				unnamed: BTreeSet::new(),
				pending: BTreeMap::new(),
				committed: None,
			},
		);
		if reach.is_some() {
			self.stream.route(node, reach);
		}
		Ok(())
	}

	/// Hand the node a batch of the route to `node`, leaving out what the limit does not
	/// admit, which the node never held, so it neither starts nor ends.
	fn file(&self, node: route::Id, changes: BTreeMap<Path, Option<node::Claim>>, revision: Option<route::Rev>) {
		let changes = (changes.into_iter())
			.filter(|(path, _)| admits(&self.origin, &self.root, &self.permit, path))
			.collect();
		self.stream.batch(node, changes, revision.filter(|_| self.certifies()));
	}

	/// The absolute path of a claim under the stream's prefix, or `None` when it, or the
	/// suffix it joins, is no path this node can hold.
	fn claim_path(&self, prefix: &Path, suffix: Option<Path>) -> Option<Path> {
		let path = prefix.join(&suffix?).ok()?;
		self.root.join(&path).ok()
	}

	/// The route with Route ID `id` now reaches its node by `reach`, or `None` while this
	/// node refuses what the peer forwarded. A route refused before files again once
	/// usable, like a route the peer ended and started over, with the claims of its last
	/// floor.
	fn reach(&mut self, id: u64, reach: Option<route::Reach>) -> Result<(), Error> {
		let route = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
		// The reserved node names no route, whatever the peer forwards for it.
		let reach = reach.filter(|_| route.node != 0);
		let refused = route.reach.is_none();
		route.reach = reach.clone();
		let node = route.node;
		match (refused, reach) {
			(true, None) => {}
			(false, None) => self.stream.route(node, None),
			(true, Some(reach)) => {
				let committed = route.committed.as_ref().map(|(claims, floor)| {
					let claims = claims.iter().map(|(path, claim)| (path.clone(), Some(claim.clone())));
					(claims.collect(), *floor)
				});
				self.stream.route(node, Some(reach));
				if let Some((claims, floor)) = committed {
					self.file(node, claims, floor);
				}
			}
			(false, Some(reach)) => self.stream.route(node, Some(reach)),
		}
		Ok(())
	}

	/// A claim the peer started under `route`, assigned Announce ID `id`, at `path` unless
	/// this node refused it, and `None` while this node refuses what the peer forwarded.
	fn start(&mut self, id: u64, route: u64, path: Option<Path>, claim: Option<node::Claim>) -> Result<(), Error> {
		let wire = self.routes.get_mut(&route).ok_or(Error::ProtocolViolation)?;
		match &path {
			Some(path) if wire.claims.contains_key(path) => return Err(Error::ProtocolViolation),
			Some(path) => {
				wire.claims.insert(path.clone(), id);
				wire.change(path.clone(), claim.clone());
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
		for slot in self.held.iter_mut().chain(&mut self.echoing) {
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
		self.regions = regions(&self.root, permit);
		let key = self.peer_setup.route_key().unwrap_or(self.session_key);
		let stream = self
			.node
			.receive(scoped(&self.link, &self.regions, key), self.prefix.clone())
			.map_err(refused)?;
		stream.rtt(self.rtt.reported());
		// What the old stream served stays in the origin until the new one serves it
		// again, or lets go after.
		let previous = std::mem::take(&mut self.served);
		let mut ids: Vec<_> = self.routes.keys().copied().collect();
		ids.sort_unstable();
		for id in &ids {
			let route = &self.routes[id];
			if let Some(reach) = &route.reach {
				stream.route(route.node, Some(reach.clone()));
			}
		}
		let certifies = self.certifies();
		for id in &ids {
			let route = &self.routes[id];
			if let Some((claims, floor)) = route.committed.as_ref().filter(|_| route.reach.is_some()) {
				let changes = (claims.iter())
					.filter(|(path, _)| admits(&self.origin, &self.root, &self.permit, path))
					.map(|(path, claim)| (path.clone(), Some(claim.clone())));
				stream.batch(route.node, changes.collect(), floor.filter(|_| certifies));
			}
		}
		if self.drained {
			stream.goaway();
		}
		self.stream.replaced_by(&stream);
		drop(std::mem::replace(&mut self.stream, stream));
		self.previous = previous;
		self.outputs(subscriber);
		// What the new stream no longer serves is let go here, not ended by its origin.
		self.previous.clear();
		Ok(())
	}

	/// Whether the peer's floors may certify what reaches the node: the limit admits
	/// all of the requested prefix within the scope's regions. A narrower limit leaves
	/// claims out that a floor would certify absent, so the claims it admits stay only
	/// while the peer carries them.
	fn certifies(&self) -> bool {
		let Some(regions) = &self.regions else {
			return true;
		};
		(regions.iter())
			.filter_map(|region| match () {
				_ if self.prefix.has_prefix(region) => Some(&self.prefix),
				_ if region.has_prefix(&self.prefix) => Some(region),
				_ => None,
			})
			.all(|covered| {
				(covered.strip_prefix(&self.root))
					.is_some_and(|relative| self.permit.within_limit_subtree(relative.as_str()))
			})
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
				served,
			} => self.serve(subscriber, id, named, claim, served),
			node::Output::Unserve { id, claim, ended } => {
				// Dropped, a claim the node let go of goes offline rather than ending.
				if let Some(served) = self.served.remove(&(id, claim))
					&& ended
				{
					served.route.dynamic.withdrawn();
				}
			}
			// The ROUTE_END echo goes out as the peer's ROUTE_END is read, and a stream
			// this node asked for is never sent routes.
			node::Output::Echo { .. } | node::Output::Route { .. } | node::Output::Batch { .. } => {}
		}
	}

	/// Serve a claim of `id` through this stream, or update how it is served.
	fn serve(&mut self, subscriber: &Subscriber, id: route::Id, named: route::Id, claim: Path, served: node::Served) {
		let Some(path) = claim.strip_prefix(&self.root) else {
			// Outside the session's root, so nothing a request could name here.
			return;
		};
		// A route reaches the peer's origin node, but the scope's regions can be wider
		// than its limit: a claim another session of the same peer carries is served
		// here only if this session may carry it too. Left unconfirmed, the node keeps
		// it where the limit admits it.
		if !self.permit.within_limit(path.as_str()) {
			return;
		}
		let key = (id, claim);
		// The chain a claim of `id` travels to a lite-06 peer: the one it crossed into
		// lite-07 with, then its origin node, so a relay it reaches back through
		// recognizes it. A chain already naming this node looped back across lite-06,
		// and the hop limit bounds the whole path across both versions.
		let mut hops = served.claim.chain.clone();
		if hops.push(route::node::hop(id)).is_err()
			|| subscriber.names_self(&hops)
			|| hops.len() > self.origin.max_hops().get()
		{
			tracing::debug!(route = %subscriber.log_path(&path), node = id, "dropping looped claim");
			for entry in self.previous.remove(&key).into_iter().chain(self.served.remove(&key)) {
				entry.route.dynamic.withdrawn();
			}
			return;
		}
		// A route held without a path to it is unavailable: it serves only as a last
		// resort, like a draining peer's, at its real cost.
		let route = crate::origin::RouteInfo::new()
			.with_instance(served.claim.instance.clone(), served.claim.epoch)
			.with_hops(hops)
			.with_cost(served.claim.cost.saturating_add(served.metric))
			.with_via(subscriber.via(None))
			.with_node(Some(id))
			.with_draining(self.drained)
			.with_available(served.available)
			.with_rtt(served.rtt);
		if let Some(entry) = self.previous.remove(&key) {
			self.served.insert(key.clone(), entry);
		}
		if let Some(entry) = self.served.get_mut(&key) {
			let same = entry.route.route.instance == route.instance && entry.route.route.epoch == route.epoch;
			if same {
				entry.named = named;
				entry.route.update(route);
				self.stream.serving(id, &key.1, &served.claim);
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
				self.served.insert(key.clone(), ServedClaim { named, route: entry });
				self.stream.serving(id, &key.1, &served.claim);
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

	/// Write the queued replies, and serve requests on every claim whose queue woke.
	pub(super) fn poll(
		&mut self,
		subscriber: &Subscriber,
		writer: &mut crate::coding::Writer<impl crate::transport::poll::SendStream, lite::Version>,
		waiter: &kio::Waiter,
	) -> Result<(), Error> {
		self.sample(subscriber);
		while let Some(read_rtt) = &mut self.read_rtt
			&& read_rtt.poll(waiter).is_ready()
		{
			self.read_link(subscriber);
		}
		while let Poll::Ready(output) = self.stream.poll_output(waiter) {
			self.output(subscriber, output);
		}
		// Replies are buffered only once the last ones flushed, so a peer leaving them
		// unread holds its echoes' slots and one request per identity, nothing more.
		loop {
			match writer.poll_flush(waiter) {
				Poll::Ready(Ok(())) => {}
				Poll::Ready(Err(err)) => return Err(err),
				Poll::Pending => break,
			}
			// The echoes buffered before went out: keep a slot per echo still queued.
			self.echoing.truncate(self.ends.len());
			let requests = match self.stream.poll_requests(waiter) {
				Poll::Ready(requests) => requests,
				Poll::Pending => Default::default(),
			};
			if self.ends.is_empty() && requests.is_empty() {
				break;
			}
			for id in self.ends.drain(..) {
				writer.buffer(&AnnounceReply::RouteEnd { id })?;
			}
			for (node, seqno) in requests {
				writer.buffer(&AnnounceReply::RouteRequest { node, seqno })?;
			}
		}
		while let Poll::Ready(Ok(key)) = self.ready.poll_pop(waiter) {
			let Some(entry) = self.served.get_mut(&key) else {
				continue;
			};
			super::mint_sources(subscriber, &mut entry.route, entry.named);
		}
		Ok(())
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

/// Whether `permit`'s limit admits the peer's claim at the absolute `path`.
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

fn scoped(link: &route::Link, regions: &Option<Vec<Path>>, key: [u8; 32]) -> route::Link {
	route::Link {
		scope: regions.as_ref().map(|regions| route::Scope {
			regions: regions.clone(),
			qualify: route::qualifier(key),
		}),
		..link.clone()
	}
}

#[cfg(test)]
mod tests {
	use crate::lite::{AnnounceBroadcast as Msg, AnnounceDecoder, PathRef, test_transport::SinkSession};
	use crate::{Instance, Path, auth::Permit, origin::Cost, route::Advert};

	fn advert() -> Advert {
		Advert {
			seqno: 0,
			metric: 0,
			down_only: false,
			rtt: None,
			hops: [].into(),
		}
	}

	fn start(path: &'static str) -> Msg {
		Msg::Active {
			suffix: PathRef::literal(Path::from_static(path)),
			route: 0,
			instance: Some(Instance::random()),
			epoch: 0,
			hops: Default::default(),
			cost: Cost::new(0),
		}
	}

	fn limited(pattern: &str) -> Permit {
		Permit::limited([pattern.parse::<crate::Pattern>().unwrap()].into_iter().collect())
	}

	/// The claims the stream serves into the origin, by path.
	fn served(routed: &super::Routed) -> Vec<String> {
		let mut served: Vec<String> = routed.served.keys().map(|(_, path)| path.to_string()).collect();
		served.sort();
		served
	}

	/// A limit narrower than its regions still leaves out the claims it does not
	/// admit, and a change that keeps the regions brings them in and out again.
	#[kio::test]
	async fn the_limit_admits_each_claim() {
		let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		let prefix = Path::empty();
		let public = limited("live/*/public/**");
		let everything = limited("live/**");
		assert_eq!(public.regions(), everything.regions(), "the limits share their regions");

		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &public).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		let route = Msg::RouteStart {
			node: 7,
			advert: advert(),
		};
		for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
			routed.receive(&prefix, msg, &mut decoder).unwrap();
		}
		routed.outputs(&subscriber);
		assert_eq!(served(&routed), ["live/a/public/x"]);

		routed.limit(&subscriber, &everything).unwrap();
		assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);

		routed.limit(&subscriber, &public).unwrap();
		assert_eq!(served(&routed), ["live/a/public/x"]);
	}

	/// A claim beneath a subtree the origin handle excludes never enters the node, on
	/// the floor or on a replay, even though the permit admits it; a covering claim
	/// overlapping the handle's scope still does.
	#[kio::test]
	async fn an_excluded_claim_never_enters_the_node() {
		let (mut subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		subscriber.origin = {
			let patterns: crate::Patterns = ["live/*/public/**", ".stats/**"]
				.into_iter()
				.map(|pattern| pattern.parse::<crate::Pattern>().unwrap())
				.collect();
			subscriber.origin.scope("", &patterns).unwrap()
		}
		.exclude(".stats")
		.unwrap();
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		let route = Msg::RouteStart {
			node: 7,
			advert: advert(),
		};
		for msg in [
			route,
			start("live"),
			start(".stats/node"),
			start(".stats/node/evil"),
			floor,
		] {
			routed.receive(&prefix, msg, &mut decoder).unwrap();
		}
		let shown = |routed: &super::Routed| -> Vec<String> {
			routed.node.shown().keys().map(|(_, path)| path.to_string()).collect()
		};
		routed.outputs(&subscriber);
		assert_eq!(shown(&routed), ["live"]);
		assert_eq!(served(&routed), ["live"]);

		// A narrower limit certifies nothing, so widening it again replays every
		// committed claim into a fresh view; only the replay filter keeps `.stats` out.
		routed.limit(&subscriber, &limited("live/**")).unwrap();
		assert_eq!(shown(&routed), ["live"]);
		assert_eq!(served(&routed), ["live"]);

		routed.limit(&subscriber, &Default::default()).unwrap();
		assert_eq!(shown(&routed), ["live"]);
		assert_eq!(served(&routed), ["live"]);
	}

	/// An unscoped stream narrowed to a limit that leaves a claim out certifies nothing,
	/// so widening it again brings that claim back at the same revision.
	#[kio::test]
	async fn a_new_scope_certifies_against_its_own_regions() {
		let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		let route = Msg::RouteStart {
			node: 7,
			advert: advert(),
		};
		for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
			routed.receive(&prefix, msg, &mut decoder).unwrap();
		}
		routed.outputs(&subscriber);
		assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);

		routed.limit(&subscriber, &limited("live/*/public/**")).unwrap();
		assert_eq!(served(&routed), ["live/a/public/x"]);

		routed.limit(&subscriber, &limited("live/**")).unwrap();
		assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);
	}

	/// Two sessions proving one route key share the peer's routes, but a session serves
	/// only the claims its own limit admits, even where its regions reach the rest.
	#[kio::test]
	async fn a_claim_is_served_only_where_the_limit_admits_it() {
		let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		let prefix = Path::empty();
		let everything = limited("live/**");
		let public = limited("live/*/public/**");
		let mut wide = super::Routed::new(&subscriber, &prefix, 1, &everything).unwrap();
		let mut narrow = super::Routed::new(&subscriber, &prefix, 1, &everything).unwrap();
		narrow.session_key = wide.session_key;
		narrow.limit(&subscriber, &public).unwrap();

		for routed in [&mut wide, &mut narrow] {
			let mut decoder = AnnounceDecoder::default();
			let floor = Msg::Floor {
				id: 0,
				revision: Some(0),
			};
			let route = Msg::RouteStart {
				node: 7,
				advert: advert(),
			};
			for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
				routed.receive(&prefix, msg, &mut decoder).unwrap();
			}
		}
		wide.outputs(&subscriber);
		narrow.outputs(&subscriber);
		assert_eq!(served(&wide), ["live/a/private/y", "live/a/public/x"]);
		assert_eq!(served(&narrow), ["live/a/public/x"]);
	}

	/// A route the peer ended keeps its announce slot until its ROUTE_END echo is
	/// flushed, so a peer leaving its replies unread cannot churn routes past the cap.
	#[kio::test]
	async fn an_ended_route_holds_its_slot_until_its_echo_is_flushed() {
		let (mut subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		subscriber.announces = crate::session::Slots::new(2);
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let budget = kio::Producer::new(0);
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::lite::test_transport::SinkSend::stepped(log, budget.consume()),
			crate::lite::Version::Lite07,
		);
		let waiter = kio::Waiter::noop();

		for (id, node) in [(0, 7), (1, 8)] {
			let route = Msg::RouteStart { node, advert: advert() };
			routed.receive(&prefix, route, &mut decoder).unwrap();
			routed.receive(&prefix, Msg::RouteEnd { id }, &mut decoder).unwrap();
			routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		}
		let route = Msg::RouteStart {
			node: 9,
			advert: advert(),
		};
		assert!(
			routed.receive(&prefix, route, &mut decoder).is_err(),
			"the unflushed echoes hold the cap"
		);

		*budget.write().ok().expect("the budget is open") = usize::MAX;
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		assert_eq!(subscriber.announces.available(), 2);
	}

	/// The route and claims a peer sends one stream, its Route ID 0.
	fn feed(
		routed: &mut super::Routed,
		decoder: &mut AnnounceDecoder,
		paths: &[&'static str],
	) -> Result<(), crate::Error> {
		let prefix = Path::empty();
		let route = Msg::RouteStart {
			node: 7,
			advert: advert(),
		};
		routed.receive(&prefix, route, decoder)?;
		for path in paths {
			routed.receive(&prefix, start(path), decoder)?;
		}
		Ok(())
	}

	/// A request handed over keeps what it holds on a budget of its own, so the claims
	/// arriving on both it and its replacement during the handover count once against the
	/// session's cap: a session that fits its claims stays open.
	#[kio::test]
	async fn a_handover_counts_the_replacement_alone() {
		let (mut subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		// One route and three claims.
		subscriber.announces = crate::session::Slots::new(4);
		let prefix = Path::empty();
		let mut old = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut old_decoder = AnnounceDecoder::default();
		feed(&mut old, &mut old_decoder, &["x", "y"]).unwrap();

		old.hand_over();
		assert_eq!(
			subscriber.announces.available(),
			4,
			"the session's count is the replacement's"
		);
		let mut new = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut new_decoder = AnnounceDecoder::default();
		feed(&mut new, &mut new_decoder, &["x", "y"]).unwrap();

		// A claim the peer starts during the handover reaches both requests.
		old.receive(&prefix, start("z"), &mut old_decoder)
			.expect("the old request fits it on its own budget");
		new.receive(&prefix, start("z"), &mut new_decoder)
			.expect("the session fits it");
		assert_eq!(subscriber.announces.available(), 0);

		drop(old);
		assert_eq!(
			subscriber.announces.available(),
			0,
			"the old request held none of the session's"
		);
		drop(new);
		assert_eq!(subscriber.announces.available(), 4);
	}

	/// A peer flooding a request handed over fails it on its own budget, the session's cap,
	/// so the session holds at most twice the cap during one handover.
	#[kio::test]
	async fn a_handover_caps_the_old_request() {
		let (mut subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		subscriber.announces = crate::session::Slots::new(3);
		let prefix = Path::empty();
		let mut old = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut old_decoder = AnnounceDecoder::default();
		feed(&mut old, &mut old_decoder, &["x"]).unwrap();

		old.hand_over();
		let mut new = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut new_decoder = AnnounceDecoder::default();
		feed(&mut new, &mut new_decoder, &["x", "y"]).unwrap();
		assert_eq!(subscriber.announces.available(), 0);

		old.receive(&prefix, start("y"), &mut old_decoder)
			.expect("the old request's budget has room for one more");
		assert!(
			old.receive(&prefix, start("z"), &mut old_decoder).is_err(),
			"the old request is capped on its own budget"
		);
		assert!(
			new.receive(&prefix, start("z"), &mut new_decoder).is_err(),
			"the replacement is capped on the session's"
		);
	}

	/// A downstream peer raising its ROUTE_REQUEST seqno while this stream's peer
	/// leaves its replies unread holds one request for the identity, sent once the
	/// writer flushes and naming the highest seqno.
	#[kio::test]
	async fn a_stalled_writer_holds_one_request_per_identity() {
		use crate::coding::Decode;

		let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let budget = kio::Producer::new(0);
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::lite::test_transport::SinkSend::stepped(log.clone(), budget.consume()),
			crate::lite::Version::Lite07,
		);
		let waiter = kio::Waiter::noop();
		let route = Msg::RouteStart {
			node: 7,
			advert: advert(),
		};
		routed.receive(&prefix, route, &mut decoder).unwrap();
		let downstream = (subscriber.origin.route_node())
			.answer(
				crate::route::Link {
					proxy: true,
					..Default::default()
				},
				Path::empty(),
			)
			.unwrap();

		for seqno in 1..=1000 {
			downstream.request(7, seqno);
			routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		}

		*budget.write().ok().expect("the budget is open") = usize::MAX;
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		let writes = log.writes.lock().unwrap().clone();
		let mut buf = writes.as_slice();
		let mut requests = Vec::new();
		while !buf.is_empty() {
			let (reply, size) = crate::lite::AnnounceReply::decode_slice(buf, crate::lite::Version::Lite07).unwrap();
			buf = &buf[size..];
			requests.push(reply);
		}
		// The first went out before the writer stalled; the rest merged behind it.
		assert_eq!(
			requests,
			[1, 1000].map(|seqno| crate::lite::AnnounceReply::RouteRequest { node: 7, seqno })
		);
	}

	/// A transport that reports its first RTT only after the stream opened, such as
	/// qmux before its first ping returns, still gets its link measured.
	#[kio::test]
	async fn a_late_rtt_is_still_read() {
		let session = SinkSession::new(Default::default());
		let (subscriber, _consumer) = super::super::tests::restart_subscriber(session.clone());
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		feed(&mut routed, &mut decoder, &[]).unwrap();
		let mut writer = crate::coding::Writer::new(
			crate::lite::test_transport::SinkSend::new(Default::default()),
			crate::lite::Version::Lite07,
		);
		let waiter = kio::Waiter::noop();
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		kio::clock::sim::advance(super::route::LinkRtt::INTERVAL).await;
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		assert_eq!(routed.rtt.reported(), None);

		let rtt = std::time::Duration::from_millis(20);
		session.set_stats(crate::lite::test_transport::SinkStats::default().with_rtt(rtt));
		kio::clock::sim::advance(super::route::LinkRtt::INTERVAL).await;
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		assert_eq!(routed.rtt.reported(), Some(rtt));
	}

	/// Claims served when the peer reports Synced, before the stream is first polled,
	/// already carry the link's RTT.
	#[kio::test]
	async fn the_first_claims_served_carry_the_rtt() {
		use crate::lite::test_transport::SinkStats;
		let ms = std::time::Duration::from_millis;
		let session = SinkSession::new(Default::default()).with_stats(SinkStats::default().with_rtt(ms(10)));
		let (subscriber, _consumer) = super::super::tests::restart_subscriber(session);
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let route = Msg::RouteStart {
			node: 7,
			advert: Advert {
				rtt: Some(ms(5)),
				..advert()
			},
		};
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		for msg in [route, start("live"), floor] {
			routed.receive(&prefix, msg, &mut decoder).unwrap();
		}
		routed.outputs(&subscriber);
		let rtts: Vec<_> = routed.served.values().map(|entry| entry.route.route.rtt()).collect();
		assert_eq!(rtts, [Some(ms(15))]);
	}

	/// A stream whose peer ends every route stops reading, and starts a fresh window
	/// once one returns, so a reading from before the pause never stays reported.
	#[kio::test]
	async fn a_pause_restarts_the_window() {
		use crate::lite::test_transport::SinkStats;
		let ms = std::time::Duration::from_millis;
		let session = SinkSession::new(Default::default()).with_stats(SinkStats::default().with_rtt(ms(10)));
		let (subscriber, _consumer) = super::super::tests::restart_subscriber(session.clone());
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let mut writer = crate::coding::Writer::new(
			crate::lite::test_transport::SinkSend::new(Default::default()),
			crate::lite::Version::Lite07,
		);
		let waiter = kio::Waiter::noop();
		feed(&mut routed, &mut decoder, &[]).unwrap();
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		assert_eq!(routed.rtt.reported(), Some(ms(10)));

		routed.receive(&prefix, Msg::RouteEnd { id: 0 }, &mut decoder).unwrap();
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		assert!(routed.read_rtt.is_none(), "no route, no timer");
		session.set_stats(SinkStats::default().with_rtt(ms(100)));
		kio::clock::sim::advance(std::time::Duration::from_secs(60)).await;

		let route = Msg::RouteStart {
			node: 7,
			advert: advert(),
		};
		routed.receive(&prefix, route, &mut decoder).unwrap();
		routed.poll(&subscriber, &mut writer, &waiter).unwrap();
		assert_eq!(routed.rtt.reported(), Some(ms(100)));
	}

	/// What the stream keeps between floors is the net change to its committed claims,
	/// however many messages the peer sends without one.
	#[kio::test]
	async fn pending_changes_net_out() {
		let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
		let prefix = Path::empty();
		let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let mut decoder = AnnounceDecoder::default();
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		let route = Msg::RouteStart {
			node: 7,
			advert: advert(),
		};
		for msg in [route, start("room/host"), floor] {
			routed.receive(&prefix, msg, &mut decoder).unwrap();
		}
		let pending = |routed: &super::Routed| routed.routes[&0].pending.len();
		assert_eq!(pending(&routed), 0);

		for cost in 1..1000 {
			let update = Msg::Update {
				id: 0,
				hops: Default::default(),
				cost: Cost::new(cost),
			};
			routed.receive(&prefix, update, &mut decoder).unwrap();
		}
		assert_eq!(pending(&routed), 1, "updates to one claim");

		let back = Msg::Update {
			id: 0,
			hops: Default::default(),
			cost: Cost::new(0),
		};
		routed.receive(&prefix, back, &mut decoder).unwrap();
		assert_eq!(pending(&routed), 0, "an update back to the committed claim");

		for _ in 0..100 {
			routed.receive(&prefix, start("room/guest"), &mut decoder).unwrap();
			let id = decoder.last();
			routed.receive(&prefix, Msg::EndedId { id }, &mut decoder).unwrap();
		}
		assert_eq!(pending(&routed), 0, "claims started and ended between floors");
	}
}
