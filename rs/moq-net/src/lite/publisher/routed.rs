//! Lite-07: what this session sends on its Announce Stream as a publisher, answering
//! the prefixes the peer asks for from the origin's route node.
//!
//! A peer that forwards and is trusted with the whole tree is sent the node's routes,
//! and under each the claims of the node it leads to, every batch closed by an
//! ANNOUNCE_FLOOR. Nothing is left out of those, since a floor says the claims are
//! complete. Any other peer is sent what the node's reader shows under the prefixes it
//! asked for, less what the answer filters: hidden claims unless the peer asked for
//! them, and anything this handle's scope or the session's grant leaves out.
//!
//! A handle's mounts beneath a requested prefix are read through a node key each, at
//! the target, and what they present is named under the mount point.
//!
//! The peer edits its prefixes in place. Each edit takes effect at the ANNOUNCE_OK
//! answering it, where both ends drop every claim within a prefix it removed.

use std::{
	collections::{BTreeMap, HashMap, HashSet},
	sync::Arc,
	task::Poll,
};

use crate::{
	Error, Instance, Path,
	lite::{self, AnnounceBroadcast as Wire, Version, subscriber::route_stream::Writer},
	origin,
	route::{
		self, NodeId,
		node::{self, Change, Claim, Msg},
		relay,
	},
	stats::Withdrawal,
};

/// A node key the answer reads: the session's own, or a mount's.
type Source = u64;

/// The session's own key, which answers the requested prefixes themselves.
const MAIN: Source = 0;

/// A claim as the handle names it: its absolute path there, and its instance.
type Named = (Path, Instance);

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
	/// How many of the keys answering it have yet to send their initial set.
	waiting: usize,
	/// SYNCED went out for it.
	synced: bool,
}

/// The peer's ANNOUNCE_REQUEST the node has not put in effect yet.
struct Request {
	remove: Vec<u64>,
	add: Vec<(u64, Prefix)>,
}

/// The ANNOUNCE_REQUESTs of an edit received so far, applied together at its last.
struct Parts {
	/// The Prefix IDs they remove.
	remove: Vec<u64>,
	/// The prefixes they add, each with its Hidden.
	add: Vec<(Path, bool)>,
}

/// A route the peer holds: its Route ID, and the Announce ID of each claim under it.
struct Sent {
	id: u64,
	claims: HashMap<node::Broadcast, u64>,
}

/// Answers the prefixes a lite-07 peer asks for.
pub(in crate::lite) struct Answer {
	origin: origin::Consumer,
	auth: crate::auth::Link,
	epoch: u64,
	permit: crate::auth::Permit,
	/// The session's key on the node.
	main: Arc<node::Stream>,
	/// The peer forwards and is trusted with the whole tree: it is sent routes.
	routing: bool,
	mounts: BTreeMap<Source, Mount>,
	next_source: Source,
	/// The mount read first next, so a busy one does not hold the others.
	turn: Source,
	/// The prefixes in effect, by Prefix ID.
	prefixes: BTreeMap<u64, Prefix>,
	next_prefix: u64,
	/// The edit the node has yet to put in effect.
	request: Option<Request>,
	/// The ANNOUNCE_REQUESTs of an edit that continues.
	parts: Option<Parts>,
	encoder: lite::AnnounceEncoder,
	/// The routes a routing peer holds.
	routes: HashMap<NodeId, Sent>,
	next_route: u64,
	/// Every claim the node told for any other peer, as the handle names it, before
	/// filtering, with the key that told it.
	told: BTreeMap<Named, (Source, Claim)>,
	/// The claims such a peer holds: their Announce ID, and what it was sent.
	live: HashMap<Named, (u64, Claim)>,
	/// The session is leaving: nothing more is sent.
	withdrawn: bool,
}

impl Answer {
	/// Answer the peer from `origin` through the session's key `main` on its node. With
	/// `routing`, the peer forwards and is trusted with the whole tree.
	pub(in crate::lite) fn new(
		origin: origin::Consumer,
		auth: crate::auth::Link,
		main: Arc<node::Stream>,
		routing: bool,
	) -> Self {
		// Start from the grant the setup token earns, so the initial set never
		// advertises something it would end a moment later.
		let mut epoch = 0;
		let permit = match auth.poll_permit(crate::auth::Direction::Publish, &mut epoch, &kio::Waiter::noop()) {
			Poll::Ready(permit) => permit,
			Poll::Pending => Default::default(),
		};
		Self {
			origin,
			auth,
			epoch,
			permit,
			main,
			routing,
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
			told: BTreeMap::new(),
			live: HashMap::new(),
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
		// One edit at a time, so a peer that never reads its answers leaves one here.
		if self.request.is_some() {
			return Err(Error::ProtocolViolation);
		}
		let (mut remove, mut add) = (remove, add);
		if let Some(parts) = self.parts.take() {
			remove.splice(0..0, parts.remove);
			add.splice(0..0, parts.add);
		}
		let mut left: BTreeMap<u64, (&Path, Option<&Path>)> = (self.prefixes.iter())
			.map(|(id, prefix)| (*id, (&prefix.relative, prefix.path.as_ref())))
			.collect();
		// Parts that could never make a valid edit fail before more of them pile up: their
		// Adds alone already pass the set's limit, or they remove more than the set holds.
		if remove.len() > left.len() || !lite::request_fits(add.iter().map(|(path, _)| path), Version::Lite07) {
			return Err(Error::ProtocolViolation);
		}
		if more {
			self.parts = Some(Parts { remove, add });
			return Ok(());
		}
		let mut edit = relay::Edit::default();
		for id in &remove {
			let (_, path) = left.remove(id).ok_or(Error::ProtocolViolation)?;
			edit.remove.extend(path.cloned());
		}
		let mut set: HashSet<Path> = left.values().map(|(relative, _)| (*relative).clone()).collect();
		let mut added = Vec::new();
		let mut next = self.next_prefix;
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
				waiting: 0,
				synced: false,
			};
			added.push((next, prefix));
			next += 1;
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
		// The node refuses a routing peer that asks for part of the tree.
		self.main.recv(Msg::Ask(edit)).map_err(|_| Error::ProtocolViolation)?;
		self.next_prefix = next;
		self.request = Some(Request { remove, add: added });
		Ok(())
	}

	/// The grant changed: end what it no longer covers, and start what it now does. A
	/// routing peer's must stay whole, since its floors leave nothing out.
	pub(in crate::lite) fn poll_permit(&mut self, writer: &mut Writer, waiter: &kio::Waiter) -> Result<(), Error> {
		let direction = crate::auth::Direction::Publish;
		while let Poll::Ready(permit) = self.auth.poll_permit(direction, &mut self.epoch, waiter) {
			if permit == self.permit || self.withdrawn {
				continue;
			}
			self.permit = permit;
			if self.routing {
				if !self.whole() {
					return Err(Error::Unauthorized);
				}
				continue;
			}
			let mut keys: Vec<Named> = self.told.keys().cloned().collect();
			keys.extend(self.live.keys().filter(|key| !self.told.contains_key(key)).cloned());
			for key in keys {
				// No longer granted, it is lost to the peer: its publisher never ended it.
				self.sync(writer, key, true)?;
			}
		}
		Ok(())
	}

	/// Whether the grant and this handle show the peer the whole tree, as a routing peer
	/// must be.
	pub(in crate::lite) fn whole(&self) -> bool {
		self.origin.root().is_empty() && self.origin.covers(&Path::empty()) && self.permit.covers_subtree("")
	}

	/// Ready once the peer reads its stream too slowly for what the node queued.
	pub(in crate::lite) fn poll_overrun(&self, waiter: &kio::Waiter) -> Poll<()> {
		let mut streams = std::iter::once(&*self.main).chain(self.mounts.values().map(|mount| &mount.stream));
		match streams.any(|stream| stream.poll_overrun(waiter).is_ready()) {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}

	/// Send one thing a mount queued, if any has.
	pub(in crate::lite) fn poll_mounts(
		&mut self,
		writer: &mut Writer,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		let after = self.mounts.range(self.turn + 1..);
		let ready = after
			.chain(self.mounts.range(..=self.turn))
			.find_map(|(source, mount)| match mount.stream.poll_wire(waiter) {
				Poll::Ready(msg) => Some((*source, msg)),
				Poll::Pending => None,
			});
		let Some((source, msg)) = ready else {
			return Poll::Pending;
		};
		self.turn = source;
		Poll::Ready(self.sourced(writer, source, msg))
	}

	/// Send what the node queued for the peer on the session's key.
	pub(in crate::lite) fn output(&mut self, writer: &mut Writer, msg: Msg) -> Result<(), Error> {
		self.sourced(writer, MAIN, msg)
	}

	fn sourced(&mut self, writer: &mut Writer, source: Source, msg: Msg) -> Result<(), Error> {
		if self.withdrawn {
			return Ok(());
		}
		match msg {
			Msg::Route { origin, offer } => self.route(writer, origin, offer),
			Msg::Claims { origin, changes, floor } => self.claims(writer, origin, changes, floor),
			Msg::Dead { hop, far } => {
				self.origin.stats().withdrawal(Withdrawal::Dead, false);
				writer.buffer(&Wire::Dead { hop, far })
			}
			Msg::Drain { node } => {
				self.origin.stats().withdrawal(Withdrawal::Drain, false);
				writer.buffer(&Wire::Drain { node })
			}
			Msg::Announce { changes } => {
				for (broadcast, change) in changes {
					self.announce(writer, source, broadcast, change)?;
				}
				Ok(())
			}
			Msg::Edited if source == MAIN => self.edited(writer),
			// A mount's own marker: the session's covers the edit that made it.
			Msg::Edited => Ok(()),
			Msg::Synced(path) => {
				let id = match self.mounts.get(&source) {
					Some(mount) => Some(mount.prefix),
					None => (self.prefixes.iter())
						.find(|(_, prefix)| !prefix.synced && prefix.path.as_ref() == Some(&path))
						.map(|(id, _)| *id),
				};
				match id {
					Some(id) => self.synced(writer, id, 1),
					None => Ok(()),
				}
			}
			// The other half of the session asks.
			Msg::Ask(_) => Ok(()),
		}
	}

	/// The session is leaving: let go of the node, so nothing more is queued. Finishing
	/// the stream ends everything the peer was sent.
	pub(in crate::lite) fn withdraw(&mut self) {
		if std::mem::replace(&mut self.withdrawn, true) {
			return;
		}
		self.main.mute();
		self.mounts.clear();
		self.routes.clear();
		self.told.clear();
		self.live.clear();
	}

	/// The requested prefix the handle-side `path` lies in, and its Prefix ID.
	fn region(&self, path: &Path) -> Option<(u64, &Prefix)> {
		self.prefixes.iter().find_map(|(id, prefix)| {
			let within = route::within(path, prefix.path.as_ref()?);
			within.then_some((*id, prefix))
		})
	}

	/// The node put the peer's request in effect: answer it, drop what the removed
	/// prefixes covered, and read the mounts beneath the added ones.
	fn edited(&mut self, writer: &mut Writer) -> Result<(), Error> {
		let Some(request) = self.request.take() else {
			return Ok(());
		};
		writer.buffer(&Wire::Ok)?;
		let mut removed = Vec::new();
		for id in &request.remove {
			removed.extend(self.prefixes.remove(id).and_then(|prefix| prefix.path));
			// Its mounts go with it.
			self.mounts.retain(|_, mount| mount.prefix != *id);
		}
		if !removed.is_empty() {
			// Both ends drop every claim the removed prefixes covered.
			let gone = |(path, _): &Named| removed.iter().any(|prefix| route::within(path, prefix));
			let dropped: Vec<Named> = self.live.keys().filter(|key| gone(key)).cloned().collect();
			for key in dropped {
				let (announce, _) = self.live.remove(&key).expect("listed above");
				self.encoder.end(announce);
			}
			self.told.retain(|key, _| !gone(key));
			// A routing peer asked for the whole tree, and now for nothing.
			for (_, sent) in self.routes.drain() {
				for announce in sent.claims.into_values() {
					self.encoder.end(announce);
				}
			}
		}
		let mut ready = Vec::new();
		for (id, mut prefix) in request.add {
			if let Some(path) = &prefix.path {
				// The node's own key answers it, and so does each mount beneath it.
				prefix.waiting = 1;
				for mount in self.origin.mounted(path).into_iter().filter(|_| !self.routing) {
					let stream = self.main.mount().map_err(|_| Error::ProtocolViolation)?;
					let edit = relay::Edit {
						remove: Vec::new(),
						add: vec![mount.interest.clone()],
					};
					stream.recv(Msg::Ask(edit)).map_err(|_| Error::ProtocolViolation)?;
					let source = self.next_source;
					self.next_source += 1;
					let mount = Mount {
						stream,
						mount,
						prefix: id,
					};
					self.mounts.insert(source, mount);
					prefix.waiting += 1;
				}
			} else {
				ready.push(id);
			}
			self.prefixes.insert(id, prefix);
		}
		self.main.flush();
		// A prefix that holds nothing has no initial set to wait for.
		for id in ready {
			self.synced(writer, id, 0)?;
		}
		Ok(())
	}

	/// `keys` of those answering the prefix with Prefix ID `id` sent their initial set:
	/// SYNCED goes out once every one has.
	fn synced(&mut self, writer: &mut Writer, id: u64, keys: usize) -> Result<(), Error> {
		let Some(prefix) = self.prefixes.get_mut(&id).filter(|prefix| !prefix.synced) else {
			return Ok(());
		};
		prefix.waiting = prefix.waiting.saturating_sub(keys);
		if prefix.waiting > 0 {
			return Ok(());
		}
		prefix.synced = true;
		writer.buffer(&Wire::Synced { prefix: id })
	}

	/// The node's route to `origin` changed: `None` ends it, and every claim under it.
	fn route(&mut self, writer: &mut Writer, origin: NodeId, offer: Option<relay::Offer>) -> Result<(), Error> {
		match offer {
			Some(offer) => {
				let next = &mut self.next_route;
				self.routes.entry(origin).or_insert_with(|| {
					let id = *next;
					*next += 1;
					Sent {
						id,
						claims: HashMap::new(),
					}
				});
				writer.buffer(&Wire::Route { offer })
			}
			None => {
				let Some(sent) = self.routes.remove(&origin) else {
					return Ok(());
				};
				for announce in sent.claims.into_values() {
					self.encoder.end(announce);
				}
				writer.buffer(&Wire::RouteEnd { id: sent.id })
			}
		}
	}

	/// A batch of changes to `origin`'s claims, closed by its floor.
	fn claims(
		&mut self,
		writer: &mut Writer,
		origin: NodeId,
		changes: Vec<(node::Broadcast, Change)>,
		floor: route::Rev,
	) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(&origin) else {
			return Ok(());
		};
		for (broadcast, change) in changes {
			match (sent.claims.get(&broadcast).copied(), change) {
				(Some(id), Change::Live(claim)) => writer.buffer(&Wire::Change { id, claim })?,
				(None, Change::Live(claim)) => {
					// A routing peer's handle is rooted at the origin's own root.
					let (announce, suffix) = self.encoder.start(broadcast.0.clone());
					let instance = broadcast.1.clone();
					writer.buffer(&Wire::Start {
						suffix,
						instance,
						claim,
					})?;
					let announce = announce.expect("lite-07 assigns announce ids");
					sent.claims.insert(broadcast, announce);
				}
				(Some(id), change) => {
					sent.claims.remove(&broadcast);
					self.encoder.end(id);
					let lost = change == Change::Lost;
					writer.buffer(&Wire::End { id, lost })?;
				}
				// Never told, so nothing to end.
				(None, _) => {}
			}
		}
		writer.buffer(&Wire::Floor {
			id: sent.id,
			revision: floor,
		})
	}

	/// The key `source` now shows the peer `change` of `broadcast`, as the origin names it.
	fn announce(
		&mut self,
		writer: &mut Writer,
		source: Source,
		(path, instance): node::Broadcast,
		change: Change,
	) -> Result<(), Error> {
		// Named as the handle names it: beneath the mount point for a mount, and never a
		// claim the origin holds beneath a mount for the session's own.
		let (named, change) = match self.mounts.get(&source).map(|mount| &mount.mount) {
			Some(mount) => {
				let Some(named) = mount.name(&path) else {
					return Ok(());
				};
				// A claim above the target covers all of the mount, so as the handle names
				// it, it is no more above the path than the mount point is.
				let beneath = mount.beneath(&named);
				let change = match change {
					Change::Live(mut claim) => {
						claim.item.above = claim.item.above.min(beneath);
						Change::Live(claim)
					}
					change => change,
				};
				(named, change)
			}
			None if self.origin.shadows(&path) => return Ok(()),
			None => (path, change),
		};
		let key = (named, instance);
		let lost = match change {
			Change::Live(claim) => {
				self.told.insert(key.clone(), (source, claim));
				false
			}
			change => {
				// Another key may show it now, as when a mount took its place.
				if self.told.get(&key).is_some_and(|(told, _)| *told == source) {
					self.told.remove(&key);
				}
				change == Change::Lost
			}
		};
		self.sync(writer, key, lost)
	}

	/// How many segments the peer's path for the absolute `path` has: the most a claim shown
	/// there can be above it, since a claim above the handle's root covers the whole of what
	/// the peer sees.
	fn depth(&self, path: &Path) -> u64 {
		path.parts().count().saturating_sub(self.origin.root().parts().count()) as u64
	}

	/// Whether the peer is shown the claim at the absolute `path`.
	fn shows(&self, path: &Path) -> bool {
		let Some((_, prefix)) = self.region(path) else {
			return false;
		};
		let within = prefix.path.as_ref().and_then(|prefix| path.strip_prefix(prefix));
		let (Some(suffix), Some(relative)) = (within, path.strip_prefix(self.origin.root())) else {
			return false;
		};
		(prefix.hidden || !suffix.is_hidden()) && self.origin.reaches(path) && self.permit.matches(relative.as_str())
	}

	/// Bring the peer's claim `key` in line with what the node told and the answer
	/// shows. A claim that goes is `lost`, or else ended.
	fn sync(&mut self, writer: &mut Writer, key: Named, lost: bool) -> Result<(), Error> {
		let depth = self.depth(&key.0);
		let wanted = (self.told.get(&key)).filter(|_| self.shows(&key.0)).map(|(_, claim)| {
			let mut claim = claim.clone();
			claim.item.above = claim.item.above.min(depth);
			claim
		});
		match (self.live.get(&key), wanted) {
			(None, Some(claim)) => {
				// The peer names every claim from the session's root.
				let relative = (key.0.strip_prefix(self.origin.root())).expect("shown claims are under the root");
				tracing::debug!(route = %key.0, "announce");
				let (announce, suffix) = self.encoder.start(relative);
				writer.buffer(&Wire::Start {
					suffix,
					instance: key.1.clone(),
					claim: claim.clone(),
				})?;
				let announce = announce.expect("lite-07 assigns announce ids");
				self.live.insert(key, (announce, claim));
			}
			(Some((announce, held)), Some(claim)) => {
				let id = *announce;
				if *held != claim {
					writer.buffer(&Wire::Change {
						id,
						claim: claim.clone(),
					})?;
					self.live.insert(key, (id, claim));
				}
			}
			(Some(_), None) => {
				let (id, _) = self.live.remove(&key).expect("matched above");
				writer.buffer(&Wire::End { id, lost })?;
				self.encoder.end(id);
			}
			(None, None) => {}
		}
		Ok(())
	}
}

#[cfg(test)]
impl Answer {
	/// An answer on a session of its own to the origin's node, whose peer asked for
	/// nothing yet.
	pub(in crate::lite) fn alone(origin: origin::Consumer) -> Self {
		let port = relay::Port {
			cost: 1,
			priority: 0,
			upstream: false,
			role: relay::Role::Scoped(Vec::new()),
		};
		let main = origin.route_node().connect(lite::session_tag(), port).unwrap();
		Self::new(origin, crate::auth::Link::new(false), Arc::new(main), false)
	}

	/// Send everything the node queued for the peer.
	pub(in crate::lite) fn drain(&mut self, writer: &mut Writer) -> Result<(), Error> {
		let waiter = kio::Waiter::noop();
		self.main.flush();
		loop {
			if let Some(msg) = self.main.try_wire() {
				self.output(writer, msg)?;
				continue;
			}
			match self.poll_mounts(writer, &waiter) {
				Poll::Ready(res) => res?,
				Poll::Pending => return Ok(()),
			}
		}
	}
}
