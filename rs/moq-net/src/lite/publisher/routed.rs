//! Lite-07: what this session sends on its Announce Stream as a publisher, answering
//! the prefixes the peer asks for from the origin's route node.
//!
//! The node sends the peer its routes, each by its publisher and the prefix it covers,
//! the DEADs naming hops on them, and, closed by an ANNOUNCE_FLOOR, each route's claims
//! under the requested prefixes. The answer filters what it puts on the wire: hidden
//! claims unless the peer asked for them, and anything this handle's scope, the
//! session's grant, the upstream horizon, or the proxy preference leaves out, in which
//! case a floor certifies nothing, since the peer cannot tell what was left out from
//! what is gone. A route goes on the wire only while the answer shows the peer one of
//! its claims, so a route whose claims it filters out costs the peer nothing.
//!
//! A handle's mounts beneath a requested prefix are read through a node stream each, at
//! the target, and what they present is named under the mount point on the same routes.
//!
//! The peer edits its prefixes in place. Each edit takes effect at the ANNOUNCE_OK
//! answering it, where both ends carry the stream's routes over alike
//! ([`rekey::plan`]), and the answer then sends only what the carried routes lack.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, HashSet},
	sync::Arc,
	task::Poll,
};

use crate::{
	Error, Path,
	lite::{self, AnnounceBroadcast as Msg, Version, rekey, subscriber::route_stream::Writer},
	origin,
	route::{
		self, node,
		path::{Dead, Hop, Offer, Reach},
	},
};

/// A route by its publisher and the prefix it covers, as the handle names it.
type RouteKey = (route::Id, Path);

/// A node stream the answer reads: the session's own, or a mount's.
type Source = u64;

/// The session's own key, which answers the requested prefixes themselves.
const MAIN: Source = 0;

/// A route as one side last knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reached {
	/// The last usable offer: a held route keeps the path it was held on.
	offer: Offer,
	held: bool,
}

impl Reached {
	/// Whether the peer, holding `self`, needs a message to hold `other`: a held route's
	/// offer is no news.
	fn differs(&self, other: &Self) -> bool {
		self.held != other.held || (!other.held && self.offer != other.offer)
	}
}

/// A route the peer holds.
struct Wire {
	id: u64,
	/// What the peer holds for it.
	told: Reached,
	/// The requested prefix it lies in.
	region: Path,
}

/// A route the node sends the session.
#[derive(Default)]
struct Sent {
	/// While the peer holds it: from the first claim the answer shows it until the last
	/// one goes.
	wire: Option<Wire>,
	/// How many of its claims the peer is shown.
	shown: usize,
	/// Each source's reach for it: the route stands while one has one, and the first
	/// one's is the peer's.
	reach: BTreeMap<Source, Reached>,
	/// Every claim the node sent for it, named as the handle names it, before filtering.
	claims: BTreeMap<Path, node::Claim>,
	/// How many of `claims` the session's link never carries (see [`sends`]).
	withheld: usize,
	/// The node's floor for those claims.
	floor: Option<route::Rev>,
	/// Whether the peer was sent a floor for it yet.
	floored: bool,
}

impl Sent {
	fn reach(&self) -> Option<&Reached> {
		self.reach.values().next()
	}

	/// The node's claim at `path` is now `claim`, which `link` may withhold.
	fn set(&mut self, link: &route::Link, path: Path, claim: Option<node::Claim>) {
		let withheld = claim.as_ref().is_some_and(|claim| !sends(link, claim));
		let old = match claim {
			Some(claim) => self.claims.insert(path, claim),
			None => self.claims.remove(&path),
		};
		self.withheld += usize::from(withheld);
		self.withheld -= usize::from(old.is_some_and(|old| !sends(link, &old)));
	}
}

/// A mount beneath a requested prefix, read at its target.
struct Mount {
	stream: node::Stream,
	/// Names what the stream presents.
	mount: origin::Mounted,
	/// The Prefix ID of the requested prefix it lies beneath.
	prefix: u64,
}

/// A prefix the peer asked for.
struct Prefix {
	/// Relative to the session's root, as the peer named it.
	relative: Path,
	/// Absolute. `None` when it is too long to lie beneath the handle's root: nothing is
	/// there, so it is answered with nothing.
	path: Option<Path>,
	hidden: bool,
	/// SYNCED went out for it.
	synced: bool,
}

/// The peer's ANNOUNCE_REQUEST the node has not put in effect yet.
struct Request {
	remove: Vec<u64>,
	add: Vec<(u64, Prefix)>,
}

/// Whether a stream on `link` carries `claim` at all: never an upstream-learned claim
/// to an upstream, nor one learned from another node to a peer this node does not
/// proxy for.
fn sends(link: &route::Link, claim: &node::Claim) -> bool {
	!(link.upstream && claim.upstream) && (link.proxy || !claim.learned)
}

/// The ANNOUNCE_REQUESTs of an edit received so far, applied together at its last.
struct Parts {
	/// The Prefix IDs they remove.
	remove: Vec<u64>,
	/// The prefixes they add, each with its Hidden.
	add: Vec<(Path, bool)>,
}

/// Answers the prefixes a lite-07 peer asks for.
pub(in crate::lite) struct Answer {
	origin: origin::Consumer,
	/// What the session's link never carries (see [`sends`]).
	link: route::Link,
	auth: crate::auth::Link,
	epoch: u64,
	permit: crate::auth::Permit,
	/// The session's key on the node.
	main: Arc<node::Stream>,
	mounts: BTreeMap<Source, Mount>,
	next_source: Source,
	/// The mount read first next, so a busy one does not hold the others.
	turn: Source,
	/// The prefixes in effect, by Prefix ID.
	prefixes: BTreeMap<u64, Prefix>,
	next_prefix: u64,
	request: Option<Request>,
	/// The ANNOUNCE_REQUESTs of an edit that continues.
	parts: Option<Parts>,
	encoder: lite::AnnounceEncoder,
	routes: HashMap<RouteKey, Sent>,
	next_route: u64,
	/// The claims the peer holds: their Announce ID and attributes.
	live: HashMap<(RouteKey, Path), (u64, node::Claim)>,
	/// Whether the claims sent carry their Lite-06 Chain.
	chains: bool,
	/// The session is leaving: every route was ended, and nothing more is sent.
	withdrawn: bool,
}

impl Answer {
	/// Answer the peer from `origin` through the session's key `main` on its node.
	/// `chains` sends each claim's Lite-06 Chain, which the peer's SETUP offers to read.
	pub(in crate::lite) fn new(
		origin: origin::Consumer,
		auth: crate::auth::Link,
		main: Arc<node::Stream>,
		chains: bool,
	) -> Self {
		let link = route::Link {
			upstream: origin.upstream(),
			proxy: origin.proxy(),
			..Default::default()
		};
		// Start from the grant the setup token earns, so the initial set never
		// advertises something it would end a moment later.
		let mut epoch = 0;
		let permit = match auth.poll_permit(crate::auth::Direction::Publish, &mut epoch, &kio::Waiter::noop()) {
			Poll::Ready(permit) => permit,
			Poll::Pending => Default::default(),
		};
		Self {
			origin,
			link,
			auth,
			epoch,
			permit,
			main,
			mounts: BTreeMap::new(),
			next_source: MAIN + 1,
			turn: MAIN,
			prefixes: BTreeMap::new(),
			next_prefix: 0,
			request: None,
			parts: None,
			encoder: lite::AnnounceEncoder::new(Version::Lite07),
			routes: HashMap::new(),
			next_route: 0,
			live: HashMap::new(),
			chains,
			withdrawn: false,
		}
	}

	/// Whether the peer's edit continues in its next message, which must be its next
	/// ANNOUNCE_REQUEST.
	pub(in crate::lite) fn continuing(&self) -> bool {
		self.parts.is_some()
	}

	/// ANNOUNCE_REQUEST: the peer edits the prefixes it asks for, over as many messages as
	/// `more` says. The edit takes effect once the node says where, in order with what
	/// the node already queued for the peer.
	pub(in crate::lite) fn request(
		&mut self,
		more: bool,
		remove: Vec<u64>,
		add: Vec<(Path, bool)>,
	) -> Result<(), Error> {
		if self.withdrawn {
			return Ok(());
		}
		// The peer sends one edit at a time.
		if self.request.is_some() {
			return Err(Error::ProtocolViolation);
		}
		let (mut remove, mut add) = (remove, add);
		if let Some(parts) = self.parts.take() {
			remove.splice(0..0, parts.remove);
			add.splice(0..0, parts.add);
		}
		// Parts that could never make a valid edit fail before more of them pile up: their
		// Adds alone already pass the set's limit, or they remove more than the set holds.
		if remove.len() > self.prefixes.len() || !lite::request_fits(add.iter().map(|(path, _)| path), Version::Lite07)
		{
			return Err(Error::ProtocolViolation);
		}
		if more {
			self.parts = Some(Parts { remove, add });
			return Ok(());
		}
		let mut left: BTreeMap<u64, &Prefix> = self.prefixes.iter().map(|(id, prefix)| (*id, prefix)).collect();
		let mut edit = route::path::Edit {
			remove: Vec::new(),
			add: Vec::new(),
		};
		for id in &remove {
			let prefix = left.remove(id).ok_or(Error::ProtocolViolation)?;
			edit.remove.extend(prefix.path.clone());
		}
		let mut set: HashSet<Path> = left.values().map(|prefix| prefix.relative.clone()).collect();
		let mut added = Vec::new();
		for (relative, hidden) in add {
			if !set.insert(relative.clone()) {
				return Err(Error::ProtocolViolation);
			}
			let path = self.origin.root().join(&relative).ok();
			edit.add.extend(path.clone());
			let prefix = Prefix {
				relative,
				path,
				hidden,
				synced: false,
			};
			added.push((self.next_prefix, prefix));
			self.next_prefix += 1;
		}
		// No prefix in the set lies below another, and the whole set fits one message.
		let nested = |prefix: &Path| {
			route::ancestors(prefix)
				.filter(|above| above != prefix)
				.any(|above| set.contains(&above))
		};
		if set.iter().any(nested) || !lite::request_fits(set.iter(), Version::Lite07) {
			return Err(Error::ProtocolViolation);
		}
		self.main.edit(edit).map_err(|_| Error::ProtocolViolation)?;
		self.request = Some(Request { remove, add: added });
		Ok(())
	}

	/// The grant changed: end what it no longer covers, start what it now does, and
	/// floor every route again, which certifies only while the grant covers the prefix.
	pub(in crate::lite) fn poll_permit(&mut self, writer: &mut Writer, waiter: &kio::Waiter) -> Result<(), Error> {
		let direction = crate::auth::Direction::Publish;
		while let Poll::Ready(permit) = self.auth.poll_permit(direction, &mut self.epoch, waiter) {
			if permit == self.permit || self.withdrawn {
				continue;
			}
			self.permit = permit;
			let mut keys: Vec<RouteKey> = self.routes.keys().cloned().collect();
			keys.sort_unstable();
			for key in keys {
				let paths: Vec<Path> = self.routes[&key].claims.keys().cloned().collect();
				for path in paths {
					self.sync(writer, &key, path)?;
				}
				self.floor(writer, &key)?;
				self.settle(writer, &key)?;
			}
		}
		Ok(())
	}

	/// Ready once the peer reads its stream too slowly for what the node queued.
	pub(in crate::lite) fn poll_overrun(&self, waiter: &kio::Waiter) -> Poll<()> {
		let mut streams = std::iter::once(&*self.main).chain(self.mounts.values().map(|mount| &mount.stream));
		match streams.any(|stream| stream.poll_overrun(waiter).is_ready()) {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}

	/// Act on one thing a mount queued, if any has.
	pub(in crate::lite) fn poll_mounts(
		&mut self,
		writer: &mut Writer,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		let after = self.mounts.range(self.turn + 1..);
		let ready = after
			.chain(self.mounts.range(..=self.turn))
			.find_map(|(source, mount)| match mount.stream.poll_output(waiter) {
				Poll::Ready(output) => Some((*source, output)),
				Poll::Pending => None,
			});
		let Some((source, output)) = ready else {
			return Poll::Pending;
		};
		self.turn = source;
		Poll::Ready(self.sourced(writer, source, output))
	}

	/// Act on what the node queued for the peer on the session's key.
	pub(in crate::lite) fn output(&mut self, writer: &mut Writer, output: node::Output) -> Result<(), Error> {
		self.sourced(writer, MAIN, output)
	}

	fn sourced(&mut self, writer: &mut Writer, source: Source, output: node::Output) -> Result<(), Error> {
		if self.withdrawn {
			return Ok(());
		}
		match output {
			node::Output::Route { id, prefix, reach } => self.route(writer, source, id, prefix, reach),
			node::Output::Batch {
				id,
				prefix,
				changes,
				floor,
			} => self.batch(writer, source, id, prefix, changes, floor),
			node::Output::Dead(dead) => self.dead(writer, source, dead),
			node::Output::Edited { routes } => self.edited(writer, routes),
			node::Output::Synced(path) => {
				let id = (self.prefixes.iter())
					.find(|(_, prefix)| !prefix.synced && prefix.path.as_ref() == Some(&path))
					.map(|(id, _)| *id);
				match id {
					Some(id) => self.synced(writer, id),
					None => Ok(()),
				}
			}
			// The other half of the session serves, and asks.
			node::Output::Serve { .. } | node::Output::Unserve { .. } | node::Output::Ask(_) => Ok(()),
		}
	}

	/// The session is leaving: let go of the node, so nothing more is queued, and end
	/// every route.
	pub(in crate::lite) fn withdraw(&mut self, writer: &mut Writer) -> Result<(), Error> {
		if std::mem::replace(&mut self.withdrawn, true) {
			return Ok(());
		}
		self.main.withdraw();
		self.mounts.clear();
		let mut ids: Vec<u64> = (self.routes.drain())
			.filter_map(|(_, sent)| sent.wire.map(|wire| wire.id))
			.collect();
		ids.sort_unstable();
		for id in ids {
			writer.buffer(&Msg::RouteEnd { id })?;
		}
		self.live.clear();
		Ok(())
	}

	/// The requested prefix `path` lies in, and its Prefix ID.
	fn region(&self, path: &Path) -> Option<(u64, &Path)> {
		self.prefixes.iter().find_map(|(id, prefix)| {
			let prefix = prefix.path.as_ref()?;
			route::within(path, prefix).then_some((*id, prefix))
		})
	}

	/// The route `source` presents at the origin-side `prefix`, named as the handle names
	/// it, within a requested prefix.
	fn name(&self, source: Source, prefix: &Path) -> Option<Path> {
		let named = match source {
			MAIN => prefix.clone(),
			_ => self.mounts.get(&source)?.mount.name(prefix)?,
		};
		self.region(&named).is_some().then_some(named)
	}

	/// The source presenting the claim at the handle-side `path`: the mount it lies
	/// beneath, or the session's own.
	fn source(&self, path: &Path) -> Source {
		let mut mounts = self.mounts.iter();
		mounts
			.find(|(_, mount)| mount.mount.holds(path))
			.map_or(MAIN, |(source, _)| *source)
	}

	/// The node put the peer's last request in effect, leaving the session's key the
	/// routes `carried`: answer it, carry the wire's routes over, and send what they lack.
	fn edited(&mut self, writer: &mut Writer, carried: Vec<route::path::Carried<node::Claim>>) -> Result<(), Error> {
		let Some(request) = self.request.take() else {
			return Ok(());
		};
		writer.buffer(&Msg::Ok)?;
		let old: BTreeSet<Path> = self
			.prefixes
			.values()
			.filter_map(|prefix| prefix.path.clone())
			.collect();
		let removed: BTreeSet<u64> = request.remove.iter().copied().collect();
		for id in &removed {
			self.prefixes.remove(id);
		}
		let mut added = Vec::new();
		let mut ids = Vec::new();
		for (id, prefix) in request.add {
			// One that stays as it was, or holds nothing, has no initial set to wait for.
			let waits = prefix.path.as_ref().is_some_and(|path| !old.contains(path));
			ids.push((id, waits));
			added.extend(prefix.path.clone().map(|path| (id, path)));
			self.prefixes.insert(id, prefix);
		}
		let new: BTreeSet<Path> = self
			.prefixes
			.values()
			.filter_map(|prefix| prefix.path.clone())
			.collect();

		// What the node now sends the session's key replaces what it sent before.
		self.unsource(MAIN);
		for route in carried {
			let Some(named) = self.name(MAIN, &route.prefix) else {
				continue;
			};
			let claims: Vec<_> = (route.claims.into_iter())
				.filter(|(path, _)| !self.origin.shadows(path))
				.collect();
			let sent = self.routes.entry((route.id, named)).or_default();
			let reached = Reached {
				offer: route.offer,
				held: route.held,
			};
			sent.reach.insert(MAIN, reached);
			for (path, claim) in claims {
				sent.set(&self.link, path, Some(claim));
			}
			sent.floor = route.floor;
		}
		for (id, path) in &added {
			for mount in self.origin.mounted(path) {
				// A removed prefix's mount that an added one reads alike, as when a prefix is
				// asked again with another Hidden or widened past the mount point, keeps its
				// stream under the new Prefix ID: its routes and claims carry over rather than
				// ending until a new stream sends them again.
				let mut mounts = self.mounts.values_mut();
				if let Some(kept) = mounts.find(|kept| removed.contains(&kept.prefix) && kept.mount == mount) {
					kept.prefix = *id;
					continue;
				}
				let interest = mount.interest.clone();
				let stream = self.main.mount(interest).map_err(|_| Error::ProtocolViolation)?;
				let source = self.next_source;
				self.next_source += 1;
				let mount = Mount {
					stream,
					mount,
					prefix: *id,
				};
				self.mounts.insert(source, mount);
			}
		}

		// Every other mount of a removed prefix goes with it.
		let gone: Vec<Source> = (self.mounts.iter())
			.filter(|(_, mount)| removed.contains(&mount.prefix))
			.map(|(source, _)| *source)
			.collect();
		for source in gone {
			self.unsource(source);
			self.mounts.remove(&source);
		}

		let added: Vec<Path> = added.into_iter().map(|(_, path)| path).collect();
		self.rekey(&old, &new, &added);

		// Send what the carried routes lack, and end what the node no longer sends.
		let mut shown: HashMap<RouteKey, Vec<Path>> = HashMap::new();
		for (key, path) in self.live.keys() {
			shown.entry(key.clone()).or_default().push(path.clone());
		}
		let mut keys: Vec<RouteKey> = self.routes.keys().cloned().collect();
		keys.sort_unstable();
		for key in keys {
			let shown = shown.remove(&key).unwrap_or_default();
			if self.routes[&key].reach.is_empty() {
				self.end(writer, &key, shown)?;
				continue;
			}
			let mut paths: BTreeSet<Path> = self.routes[&key].claims.keys().cloned().collect();
			paths.extend(shown);
			let mut changed = false;
			for path in paths {
				changed |= self.sync(writer, &key, path)?;
			}
			let sent = &self.routes[&key];
			if sent.wire.is_some() && (changed || !sent.floored) {
				self.floor(writer, &key)?;
			}
			self.settle(writer, &key)?;
			self.reconcile(writer, &key)?;
		}
		for (id, waits) in ids {
			if !waits {
				self.synced(writer, id)?;
			}
		}
		Ok(())
	}

	/// Carry the routes and claims the peer holds over an edit of the prefixes from
	/// `old` to `new`, as the peer does at the ANNOUNCE_OK.
	fn rekey(&mut self, old: &BTreeSet<Path>, new: &BTreeSet<Path>, added: &[Path]) {
		let peer = self.main.peer();
		let mut claims: HashMap<RouteKey, BTreeMap<Path, u64>> = HashMap::new();
		let mut states: HashMap<u64, node::Claim> = HashMap::new();
		for ((key, path), (id, claim)) in self.live.drain() {
			claims.entry(key).or_default().insert(path, id);
			states.insert(id, claim);
		}
		let mut table = BTreeMap::new();
		let mut wires = HashMap::new();
		for (key, sent) in &mut self.routes {
			let floored = std::mem::take(&mut sent.floored);
			sent.shown = 0;
			let Some(wire) = sent.wire.take() else {
				continue;
			};
			let route = rekey::Route {
				node: key.0,
				prefix: key.1.clone(),
				region: wire.region.clone(),
				order: rekey::order(wire.told.held, Ok(&wire.told.offer), peer),
				claims: claims.remove(key).unwrap_or_default(),
			};
			table.insert(wire.id, route);
			wires.insert(wire.id, (wire.told, floored));
		}
		let plan = rekey::plan(old, new, added, &table, self.next_route, self.encoder.next());
		let mut kept = HashSet::new();
		for carried in plan.routes {
			let (told, floored) = &wires[&carried.from];
			let key = (table[&carried.from].node, carried.prefix);
			for (path, claim) in &carried.claims {
				match claim.id == claim.from {
					true => {
						kept.insert(claim.id);
					}
					false => {
						let relative = path.strip_prefix(self.origin.root()).unwrap_or_default();
						let assigned = self.encoder.insert(relative);
						debug_assert_eq!(assigned, claim.id, "both ends assign the same id");
					}
				}
				let state = states[&claim.from].clone();
				self.live.insert((key.clone(), path.clone()), (claim.id, state));
			}
			let sent = self.routes.entry(key).or_default();
			sent.shown = carried.claims.len();
			// A widened route certifies nothing until its next floor.
			sent.floored = *floored && carried.kind != rekey::Kind::Widens;
			sent.wire = Some(Wire {
				id: carried.id,
				told: told.clone(),
				region: carried.region,
			});
		}
		for id in states.keys() {
			if !kept.contains(id) {
				self.encoder.end(*id);
			}
		}
		self.next_route = plan.next_route;
	}

	/// Send the initial set's end for the prefix with Prefix ID `id`: what its mounts
	/// present, a floor for every route in it, then SYNCED.
	fn synced(&mut self, writer: &mut Writer, id: u64) -> Result<(), Error> {
		let Some(prefix) = self.prefixes.get_mut(&id).filter(|prefix| !prefix.synced) else {
			return Ok(());
		};
		prefix.synced = true;
		let path = prefix.path.clone();
		let mounts = self.mounts.iter().filter(|(_, mount)| mount.prefix == id);
		let sources: Vec<Source> = mounts.map(|(source, _)| *source).collect();
		for source in sources {
			while let Some(output) = self.mounts.get(&source).and_then(|mount| mount.stream.try_output()) {
				self.sourced(writer, source, output)?;
			}
		}
		// Every route the peer holds has a floor before SYNCED; one the node has not
		// certified yet certifies nothing.
		let mut unfloored = Vec::new();
		for sent in self.routes.values_mut() {
			if let Some(wire) = &sent.wire
				&& Some(&wire.region) == path.as_ref()
				&& !sent.floored
			{
				sent.floored = true;
				unfloored.push(wire.id);
			}
		}
		unfloored.sort_unstable();
		for id in unfloored {
			writer.buffer(&Msg::Floor { id, revision: None })?;
		}
		writer.buffer(&Msg::Synced { prefix: id })
	}

	fn route(
		&mut self,
		writer: &mut Writer,
		source: Source,
		id: route::Id,
		prefix: Path,
		reach: Option<Reach>,
	) -> Result<(), Error> {
		let Some(named) = self.name(source, &prefix) else {
			return Ok(());
		};
		let key = (id, named);
		match reach {
			Some(Reach::Finite(offer)) => {
				// The peer is sent the route with its first claim shown.
				let sent = self.routes.entry(key.clone()).or_default();
				sent.reach.insert(source, Reached { offer, held: false });
			}
			// Held only follows a route the node sent.
			Some(Reach::Held) => match self.routes.get_mut(&key).and_then(|sent| sent.reach.get_mut(&source)) {
				Some(reached) => reached.held = true,
				None => return Ok(()),
			},
			None => return self.unreach(writer, source, &key),
		}
		self.reconcile(writer, &key)
	}

	/// Forget what `source` presented, without telling the peer: the caller brings the
	/// wire in line after.
	fn unsource(&mut self, source: Source) {
		let keys: Vec<RouteKey> = self.routes.keys().cloned().collect();
		for key in keys {
			let paths: Vec<Path> = (self.routes[&key].claims.keys())
				.filter(|path| self.source(path) == source)
				.cloned()
				.collect();
			let sent = self.routes.get_mut(&key).expect("listed above");
			sent.reach.remove(&source);
			for path in paths {
				sent.set(&self.link, path, None);
			}
		}
	}

	/// End the route for the peer, with the claims it holds at `shown`, and forget it.
	fn end(&mut self, writer: &mut Writer, key: &RouteKey, shown: impl IntoIterator<Item = Path>) -> Result<(), Error> {
		let Some(sent) = self.routes.remove(key) else {
			return Ok(());
		};
		if let Some(wire) = sent.wire {
			// The ROUTE_END retires every claim under the route.
			writer.buffer(&Msg::RouteEnd { id: wire.id })?;
		}
		for path in shown {
			if let Some((announce, _)) = self.live.remove(&(key.clone(), path)) {
				self.encoder.end(announce);
			}
		}
		Ok(())
	}

	/// `source` no longer carries the route: the claims it presented end, and the route
	/// with them once no source carries it.
	fn unreach(&mut self, writer: &mut Writer, source: Source, key: &RouteKey) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(key) else {
			return Ok(());
		};
		if sent.reach.remove(&source).is_none() {
			return Ok(());
		}
		if sent.reach.is_empty() {
			let shown: Vec<Path> = sent.claims.keys().cloned().collect();
			return self.end(writer, key, shown);
		}
		// Another source still carries the route, so the peer keeps it.
		let paths: Vec<Path> = (self.routes[key].claims.keys())
			.filter(|path| self.source(path) == source)
			.cloned()
			.collect();
		let sent = self.routes.get_mut(key).expect("matched above");
		for path in &paths {
			sent.set(&self.link, path.clone(), None);
		}
		for path in paths {
			self.sync(writer, key, path)?;
		}
		if self.routes[key].shown == 0 {
			self.floor(writer, key)?;
			self.settle(writer, key)?;
		}
		self.reconcile(writer, key)
	}

	/// Tell the peer what became of the route, if it holds it and that changed.
	fn reconcile(&mut self, writer: &mut Writer, key: &RouteKey) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(key) else {
			return Ok(());
		};
		let Some(reached) = sent.reach.values().next() else {
			return Ok(());
		};
		let Some(wire) = sent.wire.as_mut().filter(|wire| wire.told.differs(reached)) else {
			return Ok(());
		};
		match reached.held {
			true => {
				writer.buffer(&Msg::RouteHold { id: wire.id })?;
				wire.told.held = true;
			}
			false => {
				writer.buffer(&Msg::RouteUpdate {
					id: wire.id,
					offer: reached.offer.clone(),
				})?;
				wire.told = reached.clone();
			}
		}
		Ok(())
	}

	/// A DEAD naming a hop or node on the routes `source` sent. The peer is told only
	/// while a route it holds names it, and what it holds changes as the DEAD says.
	fn dead(&mut self, writer: &mut Writer, source: Source, dead: Dead) -> Result<(), Error> {
		let (hop, far, moved, gone) = match dead {
			Dead::Draining { node } => {
				let names = (self.routes.values())
					.filter_map(|sent| sent.wire.as_ref())
					.any(|wire| wire.told.offer.path.iter().any(|hop| hop.node == node));
				if names {
					writer.buffer(&Msg::Dead(lite::Dead::Drain { node }))?;
				}
				return Ok(());
			}
			Dead::Closed { hop, far, moved, gone } => (hop, far, moved, gone),
		};
		// What the node now holds sent: a route through the hop continues on the path
		// it moved to, ends if `gone`, and is held otherwise, unless the peer is the
		// node it was learned from, which is told with a ROUTE_HOLD.
		let peer = self.main.peer();
		let kept = |path: &[Hop]| route::path::child(path, peer);
		let mut moves: HashMap<RouteKey, Offer> = HashMap::new();
		for moved in moved {
			if let Some(named) = self.name(source, &moved.prefix) {
				moves.insert((moved.id, named), moved.offer);
			}
		}

		// The peer applies the DEAD to the routes it holds through the hop. With one
		// source the node's DEAD is the peer's; with mounts, what the peer holds is
		// the first source's, so the DEAD only holds them and each route then follows
		// its first source.
		let single = self.mounts.is_empty();
		let mut told: Vec<RouteKey> = (self.routes.iter())
			.filter(|(_, sent)| {
				sent.wire
					.as_ref()
					.is_some_and(|wire| wire.told.offer.path.contains(&hop))
			})
			.map(|(key, _)| key.clone())
			.collect();
		told.sort_unstable();
		if !told.is_empty() {
			let wired = |key: &RouteKey| Some((self.routes[key].wire.as_ref()?.id, moves.get(key)?.clone()));
			let (updates, ends): (Vec<(u64, Offer)>, bool) = match single {
				true => (told.iter().filter_map(wired).collect(), gone),
				false => (Vec::new(), false),
			};
			// The routes the DEAD moves follow it as ROUTE_UPDATEs, which the peer applies
			// as one with it.
			writer.buffer(&Msg::Dead(lite::Dead::Closed {
				hop,
				far,
				gone: ends,
				moved: updates.len() as u64,
			}))?;
			for (id, offer) in updates {
				writer.buffer(&Msg::RouteUpdate { id, offer })?;
			}
			for key in &told {
				let sent = self.routes.get_mut(key).expect("listed above");
				let moved = moves.get(key).filter(|_| single);
				if moved.is_none() && ends {
					// The peer ended the route and every claim under it.
					sent.wire = None;
					sent.shown = 0;
					sent.floored = false;
					for path in sent.claims.keys() {
						if let Some((announce, _)) = self.live.remove(&(key.clone(), path.clone())) {
							self.encoder.end(announce);
						}
					}
					continue;
				}
				let wire = sent.wire.as_mut().expect("listed above");
				match moved {
					Some(offer) => {
						wire.told = Reached {
							offer: offer.clone(),
							held: false,
						}
					}
					None if kept(&wire.told.offer.path) => {}
					None => wire.told.held = true,
				}
			}
		}

		let mut through: Vec<RouteKey> = (self.routes.iter())
			.filter(|(_, sent)| (sent.reach.get(&source)).is_some_and(|reached| reached.offer.path.contains(&hop)))
			.map(|(key, _)| key.clone())
			.collect();
		through.sort_unstable();
		let mut touched: BTreeSet<RouteKey> = told.into_iter().collect();
		for key in through {
			let reached = (self.routes.get_mut(&key))
				.and_then(|sent| sent.reach.get_mut(&source))
				.expect("listed above");
			match moves.remove(&key) {
				Some(offer) => *reached = Reached { offer, held: false },
				None if gone => {
					self.unreach(writer, source, &key)?;
					continue;
				}
				None if kept(&reached.offer.path) => {}
				None => reached.held = true,
			}
			touched.insert(key);
		}
		for key in touched {
			self.reconcile(writer, &key)?;
		}
		Ok(())
	}

	/// The route's Route ID, sending ROUTE_START if the peer does not hold it.
	fn start(&mut self, writer: &mut Writer, key: &RouteKey) -> Result<u64, Error> {
		if let Some(wire) = &self.routes[key].wire {
			return Ok(wire.id);
		}
		let (prefix, region) = self.region(&key.1).expect("a route named within a requested prefix");
		let region = region.clone();
		let suffix = key.1.strip_prefix(&region).expect("within the region");
		let sent = self.routes.get_mut(key).expect("only a held route starts");
		let reached = sent.reach().expect("a route stands while a source carries it").clone();
		let id = self.next_route;
		self.next_route += 1;
		writer.buffer(&Msg::RouteStart {
			node: key.0,
			prefix,
			suffix,
			offer: reached.offer.clone(),
		})?;
		if reached.held {
			writer.buffer(&Msg::RouteHold { id })?;
		}
		sent.wire = Some(Wire {
			id,
			told: reached,
			region,
		});
		Ok(id)
	}

	/// End the route on the wire once it shows the peer nothing; the caller sent the
	/// floor certifying that first.
	fn settle(&mut self, writer: &mut Writer, key: &RouteKey) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(key) else {
			return Ok(());
		};
		if sent.shown > 0 {
			return Ok(());
		}
		let Some(wire) = sent.wire.take() else {
			return Ok(());
		};
		sent.floored = false;
		writer.buffer(&Msg::RouteEnd { id: wire.id })
	}

	fn batch(
		&mut self,
		writer: &mut Writer,
		source: Source,
		id: route::Id,
		prefix: Path,
		changes: Vec<(Path, Option<node::Claim>)>,
		floor: Option<route::Rev>,
	) -> Result<(), Error> {
		let Some(named) = self.name(source, &prefix) else {
			return Ok(());
		};
		let key = (id, named);
		let mount = self.mounts.get(&source).map(|mount| &mount.mount);
		let Some(sent) = self.routes.get_mut(&key) else {
			return Ok(());
		};
		let mut paths = Vec::with_capacity(changes.len());
		for (path, claim) in changes {
			// Named as the handle names it: beneath the mount point for a mount, and never
			// a claim the origin holds beneath a mount for the session's own.
			let path = match mount {
				Some(mount) => match mount.name(&path) {
					Some(named) => named,
					None => continue,
				},
				None if self.origin.shadows(&path) => continue,
				None => path,
			};
			sent.set(&self.link, path.clone(), claim);
			paths.push(path);
		}
		sent.floor = floor;
		for path in paths {
			self.sync(writer, &key, path)?;
		}
		self.floor(writer, &key)?;
		self.settle(writer, &key)
	}

	/// Send the ANNOUNCE_FLOOR closing a batch for the route, if the peer holds it.
	fn floor(&mut self, writer: &mut Writer, key: &RouteKey) -> Result<(), Error> {
		let certified = self.certified(key);
		// Like the grant, the upstream horizon and the proxy preference leave claims out
		// that a floor would certify absent.
		let sent = self.routes.get_mut(key).expect("only a held route has a batch");
		let hides = sent.withheld > 0;
		let Some(wire) = &sent.wire else {
			return Ok(());
		};
		sent.floored = true;
		let revision = sent.floor.filter(|_| certified && !hides);
		writer.buffer(&Msg::Floor { id: wire.id, revision })
	}

	/// Whether a floor of the route may certify the whole requested prefix it lies in:
	/// nothing this handle's scope or the session's grant leaves out lies beneath it. A
	/// hidden claim left out does not count, since the peer did not ask to see it.
	fn certified(&self, key: &RouteKey) -> bool {
		let Some((_, prefix)) = self.region(&key.1) else {
			return false;
		};
		let relative = prefix.strip_prefix(self.origin.root()).unwrap_or_default();
		self.origin.covers(prefix) && self.permit.covers_subtree(relative.as_str())
	}

	/// Whether the peer is shown the claim at the absolute `path`.
	fn shows(&self, path: &Path) -> bool {
		let Some((id, prefix)) = self.region(path) else {
			return false;
		};
		let (Some(suffix), Some(relative)) = (path.strip_prefix(prefix), path.strip_prefix(self.origin.root())) else {
			return false;
		};
		let hidden = self.prefixes[&id].hidden;
		(hidden || !suffix.is_hidden()) && self.origin.reaches(path) && self.permit.matches(relative.as_str())
	}

	/// Bring the peer's claim at `path` for the route in line with the node's, returning
	/// whether that took a message.
	fn sync(&mut self, writer: &mut Writer, key: &RouteKey, path: Path) -> Result<bool, Error> {
		let sent = self.routes.get(key).expect("only a held route syncs");
		let wanted = (sent.claims.get(&path))
			.filter(|claim| self.shows(&path) && sends(&self.link, claim))
			.cloned();
		// The Lite-06 Chain goes only to a peer that reads it.
		let chain = (wanted.as_ref())
			.filter(|_| self.chains)
			.map(|claim| claim.chain.clone())
			.unwrap_or_default();
		let claim_key = (key.clone(), path);
		match (self.live.get_mut(&claim_key), wanted) {
			(None, Some(claim)) => {
				let wire = self.start(writer, key)?;
				// The peer names every claim from the session's root.
				let relative = (claim_key.1.strip_prefix(self.origin.root())).expect("shown claims are under the root");
				tracing::debug!(route = %claim_key.1, node = key.0, "announce");
				let (announce, suffix) = self.encoder.start(relative);
				writer.buffer(&Msg::Active {
					suffix,
					route: wire,
					instance: Some(claim.instance.clone()),
					epoch: claim.epoch,
					hops: chain,
					cost: origin::Cost::new(claim.cost),
				})?;
				let announce = announce.expect("lite-07 assigns announce ids");
				self.live.insert(claim_key, (announce, claim));
				self.routes.get_mut(key).expect("started above").shown += 1;
				Ok(true)
			}
			(Some((announce, held)), Some(claim)) => {
				let announce = *announce;
				let restart = held.instance != claim.instance || held.epoch != claim.epoch;
				let update = held.cost != claim.cost || (self.chains && held.chain != claim.chain);
				if restart {
					tracing::debug!(route = %claim_key.1, "restart");
					writer.buffer(&Msg::Restart {
						id: announce,
						instance: Some(claim.instance.clone()),
						epoch: claim.epoch,
						cost: origin::Cost::new(claim.cost),
						hops: chain,
					})?;
				} else if update {
					writer.buffer(&Msg::Update {
						id: announce,
						hops: chain,
						cost: origin::Cost::new(claim.cost),
					})?;
				}
				self.live.insert(claim_key, (announce, claim));
				Ok(restart || update)
			}
			(Some(_), None) => {
				let (announce, _) = self.live.remove(&claim_key).expect("matched above");
				writer.buffer(&Msg::EndedId { id: announce })?;
				self.encoder.end(announce);
				self.routes.get_mut(key).expect("only a held route syncs").shown -= 1;
				Ok(true)
			}
			(None, None) => Ok(false),
		}
	}
}

#[cfg(test)]
impl Answer {
	/// An answer on a session of its own to the origin's node, whose peer asked for
	/// nothing yet.
	pub(in crate::lite) fn alone(origin: origin::Consumer) -> Self {
		let peer = node::Peer {
			tag: lite::session_tag(),
			node: 0,
		};
		let link = route::Link {
			cost: 1,
			..Default::default()
		};
		let main = origin.route_node().connect(peer, link).unwrap();
		Self::new(origin, crate::auth::Link::new(false), Arc::new(main), false)
	}

	/// Act on everything the node queued for the peer.
	pub(in crate::lite) fn drain(&mut self, writer: &mut Writer) -> Result<(), Error> {
		let waiter = kio::Waiter::noop();
		loop {
			if let Some(output) = self.main.try_output() {
				self.output(writer, output)?;
				continue;
			}
			match self.poll_mounts(writer, &waiter) {
				Poll::Ready(res) => res?,
				Poll::Pending => return Ok(()),
			}
		}
	}
}
