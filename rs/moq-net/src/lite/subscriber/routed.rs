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
};

use super::{AnnouncedRoute, Subscriber};

/// One route the peer sent on the stream, by its Route ID.
struct WireRoute {
	node: route::Id,
	reach: route::Reach,
	/// The Announce IDs of its live claims.
	claims: BTreeSet<u64>,
	/// Claim changes since its last ANNOUNCE_FLOOR.
	pending: Vec<(Path, Option<node::Claim>)>,
	/// Its claims and floor as of that ANNOUNCE_FLOOR, replayed when the stream
	/// files again. `None` until the first.
	committed: Option<(BTreeMap<Path, node::Claim>, Option<route::Rev>)>,
}

/// One live claim the peer sent, by its Announce ID.
struct WireClaim {
	route: u64,
	/// Absolute: the origin node keys every claim from the origin's root.
	path: Path,
	claim: node::Claim,
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
	/// The peer's SETUP, whose proven route key files a scoped peer's identities.
	peer_setup: crate::lite::PeerSetup,
	/// Files them instead while the peer proved none: this session's own.
	session_key: [u8; 32],
	/// The requested prefix, absolute.
	prefix: Path,
	root: Path,
	routes: HashMap<u64, WireRoute>,
	next_route: u64,
	claims: HashMap<u64, WireClaim>,
	/// The session's announce cap: one slot per live route and claim.
	slots: crate::session::Slots,
	held: Vec<crate::session::Slot>,
	served: HashMap<(route::Id, Path), ServedClaim>,
	/// Served claims whose request queue woke since the last pass.
	ready: kio::Queue<(route::Id, Path)>,
	/// Messages for the peer, written by the stream's task.
	replies: Vec<AnnounceReply>,
	/// The peer sent a GOAWAY: every claim served here ranks last.
	drained: bool,
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
			peer_setup,
			session_key,
			prefix,
			root,
			routes: HashMap::new(),
			next_route: 0,
			claims: HashMap::new(),
			slots: subscriber.announces.clone(),
			held: Vec::new(),
			served: HashMap::new(),
			ready: Default::default(),
			replies: Vec::new(),
			drained: false,
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
			Msg::RouteStart { node, advert } => {
				if self.routes.values().any(|route| route.node == node) {
					return Err(Error::ProtocolViolation);
				}
				self.held.push(self.slots.acquire()?);
				let id = self.next_route;
				self.next_route += 1;
				let reach = route::Reach::Finite(advert);
				self.routes.insert(
					id,
					WireRoute {
						node,
						reach,
						claims: BTreeSet::new(),
						pending: Vec::new(),
						committed: None,
					},
				);
				self.stream.route(node, Some(reach));
			}
			Msg::RouteUpdate { id, advert } => self.reach(id, route::Reach::Finite(advert))?,
			Msg::RouteHold { id } => self.reach(id, route::Reach::Held)?,
			Msg::RouteEnd { id } => {
				let route = self.routes.remove(&id).ok_or(Error::ProtocolViolation)?;
				for claim in route.claims {
					decoder.end(claim)?;
					self.claims.remove(&claim);
					self.held.pop();
				}
				self.held.pop();
				self.stream.route(route.node, None);
				self.replies.push(AnnounceReply::RouteEnd { id });
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
				self.stream.batch(route.node, changes, revision);
			}
			Msg::Active {
				suffix,
				route,
				instance,
				epoch,
				cost,
				..
			} => {
				let path = self.path(prefix, &decoder.start(suffix)?)?;
				let claim = claim(instance, epoch, cost)?;
				self.start(decoder.last(), route, path, claim)?;
			}
			Msg::Takeover {
				predecessor,
				suffix,
				route,
				instance,
				epoch,
				cost,
			} => {
				let (previous, suffix) = decoder.takeover(predecessor, suffix)?;
				let (previous, path) = (self.path(prefix, &previous)?, self.path(prefix, &suffix)?);
				// Kept compiling only: lite-07's ANNOUNCE_TAKEOVER is on its way out, so a
				// claim taking over its predecessor's own path simply replaces it.
				if previous == path {
					self.end(predecessor)?;
				}
				let claim = claim(instance, epoch, cost)?;
				self.start(decoder.last(), route, path, claim)?;
			}
			Msg::Update { id, cost, .. } => {
				let claim = self.claims.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				claim.claim.cost = cost.value();
				let change = (claim.path.clone(), Some(claim.claim.clone()));
				self.pending(id, change)?;
			}
			Msg::Restart {
				id,
				instance,
				epoch,
				cost,
			} => {
				let fresh = self::claim(instance, epoch, cost)?;
				let claim = self.claims.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				claim.claim = fresh;
				let change = (claim.path.clone(), Some(claim.claim.clone()));
				self.pending(id, change)?;
			}
			Msg::EndedId { id } => {
				decoder.end(id)?;
				self.end(id)?;
			}
			// Lite-07 ends a claim by its Announce ID only.
			Msg::Ended { .. } => return Err(Error::ProtocolViolation),
			Msg::Skipped | Msg::Synced => {}
		}
		Ok(())
	}

	/// The absolute path of a claim under the stream's prefix.
	fn path(&self, prefix: &Path, suffix: &Path) -> Result<Path, Error> {
		Ok(self.root.join(&prefix.join(suffix)?)?)
	}

	fn reach(&mut self, id: u64, reach: route::Reach) -> Result<(), Error> {
		let route = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
		route.reach = reach;
		self.stream.route(route.node, Some(reach));
		Ok(())
	}

	/// A claim the peer started under `route`, assigned Announce ID `id`.
	fn start(&mut self, id: u64, route: u64, path: Path, claim: node::Claim) -> Result<(), Error> {
		let claims = &self.claims;
		let wire = self.routes.get_mut(&route).ok_or(Error::ProtocolViolation)?;
		if wire
			.claims
			.iter()
			.any(|other| claims.get(other).is_some_and(|other| other.path == path))
		{
			return Err(Error::ProtocolViolation);
		}
		self.held.push(self.slots.acquire()?);
		wire.claims.insert(id);
		wire.pending.push((path.clone(), Some(claim.clone())));
		self.claims.insert(id, WireClaim { route, path, claim });
		Ok(())
	}

	/// The peer ended the claim with Announce ID `id`.
	fn end(&mut self, id: u64) -> Result<(), Error> {
		let claim = self.claims.remove(&id).ok_or(Error::ProtocolViolation)?;
		self.held.pop();
		let route = self.routes.get_mut(&claim.route).ok_or(Error::ProtocolViolation)?;
		route.claims.remove(&id);
		route.pending.push((claim.path, None));
		Ok(())
	}

	fn pending(&mut self, id: u64, change: (Path, Option<node::Claim>)) -> Result<(), Error> {
		let route = self.claims.get(&id).ok_or(Error::ProtocolViolation)?.route;
		let route = self.routes.get_mut(&route).ok_or(Error::ProtocolViolation)?;
		route.pending.push(change);
		Ok(())
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

	/// The session's permit changed: a scope the stream no longer matches files it
	/// again, the peer's routes and claims replayed under a new key before the old
	/// one lets go.
	pub(super) fn limit(&mut self, subscriber: &Subscriber, permit: &crate::auth::Permit) -> Result<(), Error> {
		let regions = regions(&self.root, permit);
		if regions == self.regions {
			return Ok(());
		}
		let key = self.peer_setup.route_key().unwrap_or(self.session_key);
		let stream = self
			.node
			.receive(scoped(&self.link, &regions, key), self.prefix.clone())
			.map_err(refused)?;
		let mut ids: Vec<_> = self.routes.keys().copied().collect();
		ids.sort_unstable();
		for id in &ids {
			let route = &self.routes[id];
			stream.route(route.node, Some(route.reach));
		}
		for id in &ids {
			let route = &self.routes[id];
			if let Some((claims, floor)) = &route.committed {
				let changes = claims.iter().map(|(path, claim)| (path.clone(), Some(claim.clone())));
				stream.batch(route.node, changes.collect(), *floor);
			}
		}
		if self.drained {
			stream.goaway();
		}
		self.regions = regions;
		// Dropping the old stream lets go of what it served once the new one serves it.
		drop(std::mem::replace(&mut self.stream, stream));
		self.outputs(subscriber);
		Ok(())
	}

	/// Act on everything the node queued for the stream: serve the claims it hands
	/// over, and queue the replies it asks for.
	pub(super) fn outputs(&mut self, subscriber: &Subscriber) {
		while let Some(output) = self.stream.try_output() {
			self.output(subscriber, output);
		}
	}

	fn output(&mut self, subscriber: &Subscriber, output: node::Output) {
		match output {
			node::Output::Request { id, seqno } => self.replies.push(AnnounceReply::RouteRequest { node: id, seqno }),
			node::Output::Serve {
				id,
				named,
				claim,
				served,
			} => self.serve(subscriber, id, named, claim, served),
			node::Output::Unserve { id, claim } => {
				if let Some(served) = self.served.remove(&(id, claim)) {
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
		let cost = match self.drained || !served.available {
			true => crate::origin::Cost::DRAIN,
			false => crate::origin::Cost::new(served.claim.cost.saturating_add(served.metric)),
		};
		let route = crate::origin::RouteInfo::default()
			.with_instance(served.claim.instance.clone(), served.claim.epoch)
			.with_hops(hops(id))
			.with_cost(cost)
			.with_via(subscriber.via(None))
			.with_node(Some(id));
		let key = (id, claim);
		if let Some(entry) = self.served.get_mut(&key) {
			let same = entry.route.route.instance == route.instance && entry.route.route.epoch == route.epoch;
			if same {
				entry.named = named;
				entry.route.update(route);
				self.stream.serving(id, &key.1);
				return;
			}
		}
		// A new instance or epoch replaces the old route, attached before it goes.
		match subscriber.origin.forward(&path, route.clone(), None) {
			Ok(dynamic) => {
				let wake = crate::util::QueueWake::new(key.clone(), self.ready.clone());
				let entry = AnnouncedRoute::new(route, dynamic, wake);
				entry.waker.wake_by_ref();
				self.served.insert(key.clone(), ServedClaim { named, route: entry });
				self.stream.serving(id, &key.1);
			}
			Err(_) => {
				// Outside our origin's scope.
				self.served.remove(&key);
			}
		}
	}

	/// Write the queued replies, and serve requests on every claim whose queue woke.
	pub(super) fn poll(
		&mut self,
		subscriber: &Subscriber,
		writer: &mut crate::coding::Writer<impl crate::transport::poll::SendStream, lite::Version>,
		waiter: &kio::Waiter,
	) -> Result<(), Error> {
		while let Poll::Ready(output) = self.stream.poll_output(waiter) {
			self.output(subscriber, output);
		}
		for reply in self.replies.drain(..) {
			writer.buffer(&reply)?;
		}
		if let Poll::Ready(Err(err)) = writer.poll_flush(waiter) {
			return Err(err.into());
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
fn claim(instance: Option<crate::Instance>, epoch: u64, cost: crate::origin::Cost) -> Result<node::Claim, Error> {
	Ok(node::Claim {
		instance: instance.ok_or(Error::ProtocolViolation)?,
		epoch,
		cost: cost.value(),
	})
}

/// The hop chain a claim of `id` travels to a lite-06 peer, so a relay it reaches
/// back through recognizes it.
fn hops(id: route::Id) -> crate::Hops {
	let mut hops = crate::Hops::new();
	hops.push(route::node::hop(id)).expect("one hop fits any chain");
	hops
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
