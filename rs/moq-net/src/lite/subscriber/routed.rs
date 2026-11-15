//! Lite-07: what the peer sends on its Announce Stream as a publisher, feeding the
//! origin's route node.
//!
//! The session asks the peer for a set of prefixes, edited in place as what the origin
//! wants and what the session's permit allows change. The peer's routes, its DEADs, and
//! its claims go to the [`route::Node`] as they arrive, a route's claim changes in one
//! batch closed by its ANNOUNCE_FLOOR. The node decides which of its routes serve each
//! claim it shows, and the session puts the ones it is handed into the origin, minting a
//! source per request that names the claim's publisher and the route it follows.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	sync::Arc,
	task::Poll,
};

use crate::{
	Error, Path,
	lite::{self, rekey},
	route::{
		self, node,
		path::{Dead, Hop, Moved, Offer, Reach},
	},
	transport::{Metrics as _, poll::Session as _},
};

use super::{AnnouncedRoute, Subscriber};

/// A DEAD whose ROUTE_UPDATEs are still arriving: it applies with its last.
struct Moving {
	hop: Hop,
	far: route::Id,
	gone: bool,
	/// How many ROUTE_UPDATEs are still to come.
	left: u64,
	/// The new offer of each route it moved, by Route ID.
	offers: HashMap<u64, Result<Offer, Arc<[Hop]>>>,
}

/// A prefix this session asked the peer for, by its Prefix ID.
struct Asked {
	/// Absolute.
	path: Path,
	/// The peer answered the ANNOUNCE_REQUEST adding it, so its stream may name it.
	live: bool,
	/// The peer sent its initial set.
	synced: bool,
}

/// The ANNOUNCE_REQUEST the peer has not answered yet.
struct Editing {
	remove: Vec<u64>,
	add: Vec<u64>,
}

/// One route the peer sent on the stream, by its Route ID.
struct WireRoute {
	node: route::Id,
	/// The requested prefix it lies in, absolute.
	region: Path,
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
	/// the node then holds no such route, as if the peer had ended it. That includes a
	/// path with no room left for this node's hop, which the node refuses as ended, so a
	/// later shorter path files the route's claims again.
	fn reach(&self) -> Option<Reach> {
		self.prefix.as_ref()?;
		let offer = (self.offer.as_ref().ok()).filter(|offer| self.node != 0 && offer.extendable())?;
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

/// The route layer a lite-07 session's peer feeds.
pub(in crate::lite) struct Routed {
	node: node::Node,
	stream: Arc<node::Stream>,
	/// The link the session filed under, its scope and interest aside.
	link: route::Link,
	/// The permit it filed under. The node holds only the peer's claims its limit
	/// admits; the regions alone can be wider.
	permit: crate::auth::Permit,
	/// The peer's SETUP, whose proven route key files a scoped peer's identities.
	peer_setup: crate::lite::PeerSetup,
	/// Files them instead while the peer proved none: this session's own.
	session_key: [u8; 32],
	root: Path,
	/// The session's origin handle, whose exclusions and mounts also gate the claims.
	origin: crate::origin::Producer,
	/// The prefixes the origin wants of the peer, relative to the root, each holding the
	/// origin's readers unsynced until the peer's initial set for it is in.
	wants: BTreeMap<Path, Option<crate::model::Snapshot>>,
	/// The prefixes asked of the peer.
	asked: BTreeMap<u64, Asked>,
	next_prefix: u64,
	editing: Option<Editing>,
	/// The peer finished its stream: it answers nothing more.
	finished: bool,
	/// This side finished its stream: it asks for nothing more.
	silent: bool,
	routes: HashMap<u64, WireRoute>,
	/// The live Route ID of each publisher and prefix: the stream carries at most one.
	named: HashMap<(route::Id, Path), u64>,
	next_route: u64,
	claims: HashMap<u64, WireClaim>,
	decoder: lite::AnnounceDecoder,
	/// The session's announce cap: one slot per live route and claim.
	slots: crate::session::Slots,
	held: Vec<crate::session::Slot>,
	served: HashMap<ServedKey, ServedClaim>,
	/// Served claims whose request queue woke since the last pass.
	ready: kio::Queue<ServedKey>,
	/// The peer sent a GOAWAY: every claim served here ranks last.
	drained: bool,
	moving: Option<Moving>,
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
	/// File the session with the origin's node. `answering` is the handle the session
	/// answers the peer from when it is on the same node: the two directions then share
	/// one key, whose link also says what the peer is sent.
	pub(in crate::lite) fn new(
		subscriber: &Subscriber,
		link_cost: u64,
		permit: &crate::auth::Permit,
		answering: Option<&crate::origin::Consumer>,
	) -> Result<Self, Error> {
		let root = subscriber.origin.root().clone();
		let link = route::Link {
			// A zero-cost link never reaches here: SETUP refuses it on lite-07.
			cost: link_cost.max(1),
			priority: subscriber.origin.preference().value(),
			upstream: subscriber.origin.link() == crate::origin::Link::Upstream
				|| answering.is_some_and(|origin| origin.upstream()),
			// Only a session that answers the peer here sends it routes.
			proxy: answering.is_some_and(|origin| origin.proxy()),
			scope: None,
			interest: BTreeSet::new(),
			// The node files the stream under its session.
			..Default::default()
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
		let stream = (node.connect(peer, scoped(&link, &root, permit, key)))
			.map_err(refused)?
			.with_clock(subscriber.runtime.clone());
		Ok(Self {
			node,
			stream: Arc::new(stream),
			link,
			permit: permit.clone(),
			peer_setup,
			session_key,
			root,
			origin: subscriber.origin.clone(),
			wants: BTreeMap::new(),
			asked: BTreeMap::new(),
			next_prefix: 0,
			editing: None,
			finished: false,
			silent: false,
			routes: HashMap::new(),
			named: HashMap::new(),
			next_route: 0,
			claims: HashMap::new(),
			decoder: Default::default(),
			slots: subscriber.announces.clone(),
			held: Vec::new(),
			served: HashMap::new(),
			ready: Default::default(),
			drained: false,
			moving: None,
			rtt: Default::default(),
			read_rtt: None,
			sampling: false,
			stats: subscriber.origin.stats().clone(),
		})
	}

	/// The session's key on the node.
	pub(in crate::lite) fn stream(&self) -> Arc<node::Stream> {
		self.stream.clone()
	}

	/// The origin wants `prefix` of the peer, holding `snapshot` until its initial set is in.
	pub(in crate::lite) fn want(&mut self, prefix: Path, snapshot: crate::model::Snapshot) {
		self.wants.insert(prefix, Some(snapshot));
		self.relink(false);
	}

	/// The origin no longer wants `prefix` of the peer.
	pub(in crate::lite) fn unwant(&mut self, prefix: &Path) {
		self.wants.remove(prefix);
		self.relink(false);
	}

	/// The link the session files under now: what the origin wants, confined to what the
	/// peer granted, and the scope the permit sets on the peer.
	fn linked(&self) -> route::Link {
		let granted = self.permit.granted_regions();
		let mut interest = BTreeSet::new();
		for want in self.wants.keys() {
			let regions = match &granted {
				// Whichever of the two lies within the other.
				Some(regions) => (regions.iter())
					.filter_map(|region| match () {
						_ if region.has_prefix(want) => Some(region.clone()),
						_ if want.has_prefix(region) => Some(want.clone()),
						_ => None,
					})
					.collect(),
				None => vec![want.clone()],
			};
			interest.extend(regions.iter().filter_map(|region| self.root.join(region).ok()));
		}
		let key = self.peer_setup.route_key().unwrap_or(self.session_key);
		let link = route::Link {
			interest,
			..self.link.clone()
		};
		scoped(&link, &self.root, &self.permit, key)
	}

	/// File the session under its link as it stands now. With `refile`, the scope may
	/// file the peer's routes differently, so each goes to the node again.
	fn relink(&mut self, refile: bool) {
		// The node refuses only a zero-cost link, which this one never is.
		let _ = self.stream.relink(self.linked(), refile);
		if !refile {
			return;
		}
		let mut ids: Vec<_> = self.routes.keys().copied().collect();
		ids.sort_unstable();
		for id in &ids {
			file_route(&self.stream, &self.routes[id]);
		}
		for id in &ids {
			self.refile(*id);
		}
		self.stream.relinked();
	}

	/// Hand the node the claims of the route's last floor again: what the limit admits,
	/// and the end of what it does not.
	fn refile(&self, id: u64) {
		let route = &self.routes[&id];
		let prefix = route.prefix.as_ref().filter(|_| route.reach().is_some());
		if let (Some(prefix), Some((claims, floor))) = (prefix, &route.committed) {
			let changes = (claims.iter())
				.map(|(path, claim)| {
					let admitted = admits(&self.origin, &self.root, &self.permit, path);
					(path.clone(), admitted.then(|| claim.clone()))
				})
				.collect();
			self.stream.batch(route.node, prefix.clone(), changes, *floor);
		}
	}

	/// Let the origin's readers see each prefix synced whose answer is in: the peer
	/// answered every request, and sent the initial set of each prefix asked for it.
	/// The caller serves what the node queued first.
	pub(in crate::lite) fn release(&mut self) {
		if self.wants.values().all(|snapshot| snapshot.is_none()) {
			return;
		}
		if !self.finished && !self.silent {
			let asked: BTreeSet<Path> = self.asked.values().map(|asked| asked.path.clone()).collect();
			if self.editing.is_some() || self.stream.asks() != asked {
				return;
			}
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
			if self.finished || self.silent || answers.all(|asked| asked.synced) {
				*snapshot = None;
			}
		}
	}

	/// The peer finished its stream: it is leaving, and answers nothing more.
	pub(in crate::lite) fn finish(&mut self) -> Result<(), Error> {
		if self.moving.is_some() {
			return Err(Error::ProtocolViolation);
		}
		self.finished = true;
		let mut ids: Vec<_> = self.routes.keys().copied().collect();
		ids.sort_unstable();
		for id in ids {
			let route = self.end_route(id)?;
			if let (Some(_), Some(prefix)) = (route.reach(), route.prefix) {
				self.stream.route(route.node, prefix, None);
			}
		}
		Ok(())
	}

	/// This side finished its stream: the prefixes asked of the peer stay as they are.
	pub(in crate::lite) fn silence(&mut self) {
		self.silent = true;
	}

	/// The node asks the peer for other prefixes: the ANNOUNCE_REQUEST to send, if any.
	fn ask(&mut self, edit: route::path::Edit) -> Option<lite::AnnounceBroadcast> {
		// The node sends one edit at a time.
		if self.editing.is_some() || self.finished || self.silent {
			return None;
		}
		let mut editing = Editing {
			remove: Vec::new(),
			add: Vec::new(),
		};
		for path in &edit.remove {
			let id = (self.asked.iter()).find(|(_, asked)| asked.live && asked.path == *path);
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
				live: false,
				synced: false,
			};
			self.asked.insert(id, asked);
			editing.add.push(id);
			// Hidden claims too: the session mirrors the peer into the origin, and each
			// local reader opts in on its own.
			add.push((relative, true));
		}
		let request = lite::AnnounceBroadcast::Request {
			remove: editing.remove.clone(),
			add,
		};
		self.editing = Some(editing);
		Some(request)
	}

	/// ANNOUNCE_OK: the last edit takes effect on the peer's stream. Its live routes
	/// carry over as [`rekey::plan`] says, which the peer applies alike.
	fn ok(&mut self) -> Result<(), Error> {
		let editing = self.editing.take().ok_or(Error::ProtocolViolation)?;
		// The marker lies between batches.
		if self.routes.values().any(|route| !route.pending.is_empty()) {
			return Err(Error::ProtocolViolation);
		}
		let old: BTreeSet<Path> = (self.asked.values())
			.filter(|asked| asked.live)
			.map(|asked| asked.path.clone())
			.collect();
		for id in &editing.remove {
			self.asked.remove(id);
		}
		let mut added = Vec::new();
		for id in &editing.add {
			let asked = self.asked.get_mut(id).expect("an added prefix");
			asked.live = true;
			added.push(asked.path.clone());
		}
		let new: BTreeSet<Path> = self.asked.values().map(|asked| asked.path.clone()).collect();

		let own = self.node.hop();
		let table: BTreeMap<u64, rekey::Route> = (self.routes.iter())
			.map(|(id, route)| {
				let route = rekey::Route {
					node: route.node,
					prefix: route.prefix.clone().unwrap_or_else(|| route.region.clone()),
					region: route.region.clone(),
					order: rekey::order(route.held, route.offer.as_ref().map_err(|path| &**path), own),
					claims: route.claims.clone(),
				};
				(*id, route)
			})
			.collect();
		let plan = rekey::plan(&old, &new, &added, &table, self.next_route, self.decoder.next());
		let routes = std::mem::take(&mut self.routes);
		let claims = std::mem::take(&mut self.claims);
		let mut moved = Vec::new();
		for carried in plan.routes {
			let source = &routes[&carried.from];
			let mut route = WireRoute {
				node: source.node,
				region: carried.region,
				prefix: source.prefix.as_ref().map(|_| carried.prefix.clone()),
				offer: source.offer.clone(),
				held: source.held,
				claims: BTreeMap::new(),
				unnamed: BTreeSet::new(),
				withheld: BTreeSet::new(),
				pending: BTreeMap::new(),
				committed: None,
			};
			let mut committed = BTreeMap::new();
			for (path, claim) in &carried.claims {
				let held = claims[&claim.from].claim.clone();
				if claim.id != claim.from {
					let relative = path.strip_prefix(&self.root).unwrap_or_default();
					let assigned = self.decoder.insert(relative);
					debug_assert_eq!(assigned, claim.id, "both ends assign the same id");
				}
				match &held {
					Some(held) => {
						committed.insert(path.clone(), held.clone());
					}
					None => {
						route.withheld.insert(claim.id);
					}
				}
				route.claims.insert(path.clone(), claim.id);
				let claim_route = carried.id;
				self.claims.insert(
					claim.id,
					WireClaim {
						route: claim_route,
						path: Some(path.clone()),
						claim: held,
					},
				);
			}
			// A claim without a path stays with the route it was sent under.
			if carried.id == carried.from {
				for id in &source.unnamed {
					route.unnamed.insert(*id);
					self.claims.insert(
						*id,
						WireClaim {
							route: carried.id,
							path: None,
							claim: None,
						},
					);
				}
			}
			route.committed = match (carried.kind, &source.committed) {
				(rekey::Kind::Stays, committed) => committed.clone(),
				// The floors certified only the narrower prefix.
				(rekey::Kind::Widens, _) => Some((committed, None)),
				(rekey::Kind::Narrows, source) => Some((committed, source.as_ref().and_then(|(_, floor)| *floor))),
			};
			if carried.kind != rekey::Kind::Stays {
				moved.push(carried.id);
			}
			self.routes.insert(carried.id, route);
		}
		self.next_route = plan.next_route;
		for id in claims.keys() {
			if !self.claims.contains_key(id) {
				self.decoder.end(*id)?;
			}
		}
		self.named = (self.routes.iter())
			.filter(|(_, route)| route.node != 0)
			.filter_map(|(id, route)| Some(((route.node, route.prefix.clone()?), *id)))
			.collect();
		let holding = self.routes.len() + self.claims.len();
		self.held.truncate(holding);
		while self.held.len() < holding {
			self.held.push(self.slots.acquire()?);
		}

		// The node carries what it holds over the same way. A route this node refused
		// is not there to carry, so each route that moved is filed as it now stands.
		self.stream.edited();
		moved.sort_unstable();
		for id in moved {
			file_route(&self.stream, &self.routes[&id]);
			self.refile(id);
		}
		Ok(())
	}

	/// Apply one message the peer sent on its stream as a publisher.
	pub(in crate::lite) fn receive(&mut self, announce: lite::AnnounceBroadcast) -> Result<(), Error> {
		use lite::AnnounceBroadcast as Msg;
		// A DEAD's ROUTE_UPDATEs arrive back to back.
		if let Some(moving) = &mut self.moving {
			let (id, offer) = match announce {
				Msg::RouteUpdate { id, offer } => (id, Ok(offer)),
				Msg::Refused {
					message: lite::Refusable::RouteUpdate { id, path },
					reason,
				} => {
					tracing::warn!(route = id, %reason, "refusing a forwarded route");
					self.stats.refuse_route();
					(id, Err(path))
				}
				_ => return Err(Error::ProtocolViolation),
			};
			// Each names a live route through the hop, once.
			let through = (self.routes.get(&id)).is_some_and(|route| route.path().contains(&moving.hop));
			if !through || moving.offers.insert(id, offer).is_some() {
				return Err(Error::ProtocolViolation);
			}
			moving.left -= 1;
			if moving.left == 0 {
				let moving = self.moving.take().expect("matched above");
				self.apply_dead(moving)?;
			}
			return Ok(());
		}
		match announce {
			Msg::Ok => self.ok()?,
			Msg::Synced { prefix } => {
				let asked = (self.asked.get_mut(&prefix)).filter(|asked| asked.live && !asked.synced);
				asked.ok_or(Error::ProtocolViolation)?.synced = true;
			}
			// The peer's own requests go to the half answering them.
			Msg::Request { .. } => return Err(Error::ProtocolViolation),
			Msg::RouteStart {
				node,
				prefix,
				suffix,
				offer,
			} => self.route_start(node, prefix, Some(suffix), Ok(offer))?,
			Msg::RouteUpdate { id, offer } => self.offer(id, Ok(offer))?,
			Msg::RouteHold { id } => {
				let route = self.routes.get_mut(&id).ok_or(Error::ProtocolViolation)?;
				let before = route.reach();
				route.held = true;
				self.sync(id, before);
			}
			Msg::RouteEnd { id } => {
				let route = self.end_route(id)?;
				if let (Some(_), Some(prefix)) = (route.reach(), route.prefix) {
					self.stream.route(route.node, prefix, None);
				}
			}
			Msg::Dead(dead) => self.dead(dead)?,
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
				let path = self.decoder.start(Some(suffix))?;
				let path = self.claim_path(path);
				let claim = claim(instance, epoch, cost, hops)?;
				let id = self.decoder.last();
				// Fields valid alone can still resolve, or join the root, past the path limits.
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
				self.decoder.end(id)?;
				self.end(id)?;
			}
			Msg::Refused { message, reason } => self.refused(message, reason)?,
			// Lite-07 ends a claim by its Announce ID only.
			Msg::Ended { .. } => return Err(Error::ProtocolViolation),
			// Only a relay choosing a parent per broadcast uses warm routes, which this
			// endpoint does not do.
			Msg::WarmStart { .. } | Msg::WarmUpdate { .. } | Msg::WarmEnd { .. } => {}
			Msg::Skipped => {}
		}
		Ok(())
	}

	/// The peer started a route to `node` with the next Route ID, covering the requested
	/// prefix `asked` joined with `suffix`, along `offer`, or along the path of an offer
	/// this node refuses. A route with no suffix, or to the reserved node, stays refused.
	fn route_start(
		&mut self,
		node: route::Id,
		asked: u64,
		suffix: Option<Path>,
		offer: Result<Offer, Arc<[Hop]>>,
	) -> Result<(), Error> {
		// The peer names a prefix in effect on its stream.
		let region = (self.asked.get(&asked).filter(|asked| asked.live))
			.ok_or(Error::ProtocolViolation)?
			.path
			.clone();
		// Fields valid alone can still join the prefix past the path limits.
		let prefix = suffix.and_then(|suffix| region.join(&suffix).ok());
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
			region,
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
	fn end_route(&mut self, id: u64) -> Result<WireRoute, Error> {
		let route = self.routes.remove(&id).ok_or(Error::ProtocolViolation)?;
		if let Some(prefix) = &route.prefix
			&& self.named.get(&(route.node, prefix.clone())) == Some(&id)
		{
			self.named.remove(&(route.node, prefix.clone()));
		}
		for claim in route.claims.values().chain(&route.unnamed) {
			self.decoder.end(*claim)?;
			self.claims.remove(claim);
			self.held.pop();
		}
		self.held.pop();
		Ok(route)
	}

	/// A DEAD: the stream's routes through the hop it names carry on as it says, which the
	/// node is told as the peer's news. A route this node refuses still follows it, so
	/// the two ends agree on which routes live. One that moves routes applies with the
	/// last of the ROUTE_UPDATEs following it.
	fn dead(&mut self, dead: lite::Dead) -> Result<(), Error> {
		let moving = match dead {
			lite::Dead::Drain { node } => {
				// The node ranks a scoped peer's routes through `node` draining by their
				// offers alone, so the offers keep it for a new limit to file again.
				if regions(&self.root, &self.permit).is_some() && node != self.node.hop() {
					for route in self.routes.values_mut() {
						if let Ok(offer) = &mut route.offer
							&& offer.path.iter().any(|hop| hop.node == node)
						{
							offer.draining = true;
						}
					}
				}
				self.stream.dead(Dead::Draining { node });
				return Ok(());
			}
			lite::Dead::Closed { hop, far, gone, moved } => Moving {
				hop,
				far,
				gone,
				left: moved,
				offers: HashMap::new(),
			},
		};
		// It moves only routes through the hop, so what its updates hold stays within
		// the stream's routes.
		let through = self.routes.values().filter(|route| route.path().contains(&moving.hop));
		if moving.left > through.count() as u64 {
			return Err(Error::ProtocolViolation);
		}
		match moving.left {
			0 => self.apply_dead(moving),
			_ => {
				self.moving = Some(moving);
				Ok(())
			}
		}
	}

	/// Apply a DEAD and the offers its ROUTE_UPDATEs gave as one.
	fn apply_dead(&mut self, moving: Moving) -> Result<(), Error> {
		let Moving {
			hop,
			far,
			gone,
			offers,
			..
		} = moving;
		// The node applies only a DEAD it credits. One it drops still changes the stream's
		// routes, as the peer applies it to what it sent, so the node then hears each
		// change as the peer's own end, hold, or update, and both keep the same routes.
		let credited = self.stream.credits(hop, far);
		let mut offers = offers;
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
		// What the DEAD changed of each route still live, for a node that drops it.
		let mut touched = Vec::new();
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
						_ => changed.push((id, before.clone())),
					}
					touched.push((id, before));
				}
				None if gone => ended.push(id),
				// The peer learned it from this node, which it holds under: unchanged.
				None if child => {}
				None => {
					route.held = true;
					touched.push((id, before));
				}
			}
		}
		for id in ended {
			let route = self.end_route(id)?;
			if let (false, Some(_), Some(prefix)) = (credited, route.reach(), route.prefix) {
				self.stream.route(route.node, prefix, None);
			}
		}
		if !credited {
			tracing::debug!(?hop, far, "the node drops a DEAD no route from the peer credits");
			for (id, before) in touched {
				self.sync(id, before);
			}
			return Ok(());
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
	fn refused(&mut self, message: lite::Refusable, reason: lite::Refusal) -> Result<(), Error> {
		use lite::Refusable;
		match message {
			Refusable::RouteStart {
				node,
				prefix,
				suffix,
				path,
			} => {
				tracing::warn!(node, %reason, "refusing a forwarded route");
				self.route_start(node, prefix, suffix, Err(path))?;
			}
			Refusable::RouteUpdate { id, path } => {
				let node = self.routes.get(&id).ok_or(Error::ProtocolViolation)?.node;
				tracing::warn!(node, route = id, %reason, "refusing a forwarded route");
				self.offer(id, Err(path))?;
			}
			Refusable::Start { route, suffix } => {
				let path = self.decoder.start(suffix)?;
				let path = self.claim_path(path);
				let id = self.decoder.last();
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

	/// The absolute path of a claim the peer named `path`, or `None` when it is no path
	/// this node can hold.
	fn claim_path(&self, path: Option<Path>) -> Option<Path> {
		self.root.join(&path?).ok()
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

	/// The peer sent a GOAWAY: its routes rank below every other. Idempotent.
	pub(in crate::lite) fn goaway(&mut self) {
		if self.drained {
			return;
		}
		self.drained = true;
		self.stream.goaway();
		for served in self.served.values_mut() {
			served.route.drain();
		}
	}

	/// The session's permit changed. A new limit files the peer's routes and the claims
	/// it admits again, under the scope it sets: it may change the scope, the claims
	/// admitted, the claims the node may serve here, and what the floors certify. What
	/// the peer granted changes the prefixes asked of it. The peer still holds each
	/// claim either way.
	pub(in crate::lite) fn limit(&mut self, permit: &crate::auth::Permit) {
		let same = permit.same_limit(&self.permit);
		let asks = permit.granted_regions() == self.permit.granted_regions();
		self.permit = permit.clone();
		if !same || !asks {
			self.relink(!same);
		}
	}

	/// Act on what the node queued for this half of the session: serve the claims it
	/// hands over, and return the ANNOUNCE_REQUEST asking the peer for what it now wants.
	pub(in crate::lite) fn output(
		&mut self,
		subscriber: &Subscriber,
		output: node::Output,
	) -> Option<lite::AnnounceBroadcast> {
		self.sample(subscriber);
		match output {
			node::Output::Ask(edit) => self.ask(edit),
			output => {
				self.served(subscriber, output);
				None
			}
		}
	}

	fn served(&mut self, subscriber: &Subscriber, output: node::Output) {
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
			// What the peer is sent goes to the half answering it.
			_ => {}
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
			if let Some(entry) = self.served.remove(&key) {
				entry.route.dynamic.withdrawn();
			}
			return;
		}
		let lane = crate::origin::Lane {
			depth: prefix.parts().count() as u64,
			primary: served.primary,
			selected: served.selected,
			path: served.path.clone(),
			priority: Some(served.priority),
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
	pub(in crate::lite) fn poll(&mut self, subscriber: &Subscriber, waiter: &kio::Waiter) {
		self.sample(subscriber);
		while let Some(read_rtt) = &mut self.read_rtt
			&& read_rtt.poll(waiter).is_ready()
		{
			self.read_link(subscriber);
		}
		while let Poll::Ready(Ok(key)) = self.ready.poll_pop(waiter) {
			let Some(entry) = self.served.get_mut(&key) else {
				continue;
			};
			super::mint_sources(subscriber, &mut entry.route, (entry.named, entry.depth));
		}
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
impl Routed {
	/// A session that asked the peer for `prefix`, which the peer answered.
	fn asking(
		subscriber: &Subscriber,
		prefix: &Path,
		link_cost: u64,
		permit: &crate::auth::Permit,
	) -> Result<Self, Error> {
		// Each is a session of its own to the node.
		let mut subscriber = subscriber.clone();
		subscriber.peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		let subscriber = &subscriber;
		let mut routed = Self::new(subscriber, link_cost, permit, None)?;
		routed.want(prefix.clone(), subscriber.origin.snapshot(prefix.as_str()));
		routed.answered(subscriber)?;
		Ok(routed)
	}

	/// Act on everything the node queued, returning the requests it sends the peer.
	fn outputs(&mut self, subscriber: &Subscriber) -> Vec<lite::AnnounceBroadcast> {
		let mut requests = Vec::new();
		while let Some(output) = self.stream.try_output() {
			requests.extend(self.output(subscriber, output));
		}
		requests
	}

	/// Send the peer what the node asks for, and have it answer each request at once.
	fn answered(&mut self, subscriber: &Subscriber) -> Result<(), Error> {
		while !self.outputs(subscriber).is_empty() {
			self.receive(lite::AnnounceBroadcast::Ok)?;
		}
		Ok(())
	}

	/// Apply a new permit, the peer answering whatever it changes of the request.
	fn relimit(&mut self, subscriber: &Subscriber, permit: &crate::auth::Permit) -> Result<(), Error> {
		self.limit(permit);
		self.answered(subscriber)
	}
}

#[cfg(test)]
mod tests;
