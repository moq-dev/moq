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
//! A prefix wider than what the grant shows the peer is read region by region: the
//! session's key reads each granted region inside it, and a claim above a region is
//! presented at the region, as if the peer had asked for each. Nothing outside the grant
//! is read, so what it hides costs no work and never holds back a SYNCED. A grant change
//! narrows those reads again through the same key, so it is answered before the
//! ANNOUNCE_OK of any edit read after it.
//!
//! A handle's mounts beneath what is read are read through a node key each, at the
//! target, and what they present is named under the mount point.
//!
//! The peer edits its prefixes in place. Each edit takes effect at the ANNOUNCE_OK
//! answering it, where both ends drop every claim within a prefix it removed.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
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
	stats::{RouteClose, Withdrawal},
};

/// A node key the answer reads: the session's own, or a mount's.
type Source = u64;

/// The session's own key, which answers the requested prefixes themselves.
const MAIN: Source = 0;

/// A claim as the handle names it: its absolute path there, and its instance.
type Named = (Path, Instance);

/// A mount beneath what the answer reads, read at its target.
struct Mount {
	stream: node::Stream,
	/// Names what the stream presents.
	mount: origin::Mounted,
	/// The Prefix ID of the requested prefix it lies beneath.
	prefix: u64,
	/// What the session's key reads for that prefix, which the mount lies beneath.
	read: Path,
	/// What its initial set holds back until it is in.
	holds: Holds,
}

/// What a mount's initial set holds back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holds {
	/// Its prefix's SYNCED.
	Synced,
	/// Everything else the session's key queued, since a grant change made it: the
	/// ANNOUNCE_OK of the peer's next edit must follow it.
	Main,
	/// Nothing: it is in.
	Nothing,
}

/// What a marker on the session's key answers.
enum Marker {
	/// The peer's edit.
	Request,
	/// A grant change, which stopped reading these.
	Grant { removed: Vec<Path> },
}

/// A prefix the peer asked for.
struct Prefix {
	/// Relative to the session's root, as the peer named it.
	relative: Path,
	/// Absolute. `None` when it is too long to lie beneath the handle's root: nothing is
	/// there, so it is answered with nothing.
	path: Option<Path>,
	hidden: bool,
	/// What the session's key reads for it, which the grant narrows: the prefix itself, or
	/// each granted region inside it.
	reads: BTreeSet<Path>,
	/// How many reads answering it have yet to send their initial set.
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
	/// What the session's key asks the node for: each prefix's reads.
	reads: BTreeSet<Path>,
	/// The absolute regions the grant and the handle's limit confine the peer to, or
	/// `None` when they show it everything.
	regions: Option<Vec<Path>>,
	/// What each marker the session's key has yet to send answers, oldest first.
	markers: VecDeque<Marker>,
	/// The latest marker answered the peer's edit, so the SYNCED after it count.
	answering: bool,
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
	/// Why the last error closes the session, when the route layer said.
	cause: Option<RouteClose>,
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
		let regions = Self::confined(&origin, &permit);
		Self {
			origin,
			auth,
			epoch,
			permit,
			reads: BTreeSet::new(),
			regions,
			markers: VecDeque::new(),
			answering: false,
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
			cause: None,
		}
	}

	/// Whether the peer's edit continues in its next message, which must be its next
	/// ANNOUNCE_REQUEST.
	pub(in crate::lite) fn continuing(&self) -> bool {
		self.parts.is_some()
	}

	/// Why the error [`Self::request`] last returned closes the session, as its stats
	/// count it: what the route layer said, or else a request the rule never builds.
	pub(in crate::lite) fn closed(&mut self) -> RouteClose {
		self.cause.take().unwrap_or(RouteClose::Malformed)
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
		// A grant change the peer may have read before this edit is answered before it.
		self.refresh()?;
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
			let reads = path.map(|path| self.reads.iter().filter(|read| route::within(read, path)));
			edit.remove.extend(reads.into_iter().flatten().cloned());
		}
		let mut set: HashSet<Path> = left.values().map(|(relative, _)| (*relative).clone()).collect();
		let mut added = Vec::new();
		let mut next = self.next_prefix;
		for (relative, hidden) in add {
			if !set.insert(relative.clone()) {
				return Err(Error::ProtocolViolation);
			}
			let path = self.origin.root().join(&relative).ok();
			let reads = path.as_ref().map(|path| self.narrow(path)).unwrap_or_default();
			edit.add.extend(reads.iter().cloned());
			let prefix = Prefix {
				relative,
				path,
				hidden,
				waiting: reads.len(),
				reads,
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
		self.ask(edit, Marker::Request)?;
		self.next_prefix = next;
		self.request = Some(Request { remove, add: added });
		Ok(())
	}

	/// Ask the node for `edit` on the session's key, whose marker answers `marker`.
	fn ask(&mut self, edit: relay::Edit, marker: Marker) -> Result<(), Error> {
		let (remove, add) = (edit.remove.clone(), edit.add.clone());
		self.main.recv(Msg::Ask(edit)).map_err(|err| {
			self.cause = Some(err.close());
			Error::ProtocolViolation
		})?;
		for read in &remove {
			self.reads.remove(read);
		}
		self.reads.extend(add);
		self.markers.push_back(marker);
		Ok(())
	}

	/// The absolute regions `permit` and the handle's root confine the peer to, or `None`.
	fn confined(origin: &origin::Consumer, permit: &crate::auth::Permit) -> Option<Vec<Path>> {
		let regions = permit.regions()?;
		Some(
			regions
				.iter()
				.filter_map(|region| origin.root().join(region).ok())
				.collect(),
		)
	}

	/// What the session's key reads to answer the absolute `prefix`: the prefix itself when
	/// a region holds it or nothing confines the peer, and otherwise each region inside it.
	/// A routing peer's grant is whole.
	fn narrow(&self, prefix: &Path) -> BTreeSet<Path> {
		let Some(regions) = self.regions.as_ref().filter(|_| !self.routing) else {
			return BTreeSet::from([prefix.clone()]);
		};
		if regions.iter().any(|region| route::within(prefix, region)) {
			return BTreeSet::from([prefix.clone()]);
		}
		(regions.iter())
			.filter(|region| route::within(region, prefix))
			.cloned()
			.collect()
	}

	/// Take a grant change the peer may already know of, without waiting.
	fn refresh(&mut self) -> Result<(), Error> {
		if self.routing {
			return Ok(());
		}
		let direction = crate::auth::Direction::Publish;
		let mut changed = false;
		while let Poll::Ready(permit) = self.auth.poll_permit(direction, &mut self.epoch, &kio::Waiter::noop()) {
			changed |= permit != self.permit;
			self.permit = permit;
		}
		match changed {
			true => self.renarrow(),
			false => Ok(()),
		}
	}

	/// The grant changed: read what it now shows of every prefix, the peer's pending edit
	/// included. Its marker ends what it no longer shows and starts what it now does.
	fn renarrow(&mut self) -> Result<(), Error> {
		self.regions = Self::confined(&self.origin, &self.permit);
		let pending = self.request.as_ref();
		let removed = |id: &u64| pending.is_some_and(|request| request.remove.contains(id));
		let prefixes = (self.prefixes.iter())
			.filter(|(id, _)| !removed(id))
			.map(|(_, prefix)| prefix)
			.chain(
				pending
					.into_iter()
					.flat_map(|request| request.add.iter().map(|(_, prefix)| prefix)),
			);
		let wanted: BTreeSet<Path> = (prefixes.filter_map(|prefix| prefix.path.as_ref()))
			.flat_map(|path| self.narrow(path))
			.collect();
		let edit = relay::Edit {
			remove: self.reads.difference(&wanted).cloned().collect(),
			add: wanted.difference(&self.reads).cloned().collect(),
		};
		let removed = edit.remove.clone();
		self.ask(edit, Marker::Grant { removed })?;
		// Queued for this session now, so the pass that saw the change sends it.
		self.main.flush();
		Ok(())
	}

	/// The grant changed: read what it now shows, and at the marker of that, end what it
	/// no longer covers and start what it now does. A routing peer's must stay whole, since
	/// its floors leave nothing out.
	pub(in crate::lite) fn poll_permit(&mut self, waiter: &kio::Waiter) -> Result<(), Error> {
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
			self.renarrow()?;
		}
		Ok(())
	}

	/// Whether what the session's key queued waits for the initial set of a mount a grant
	/// change made, so the peer's next ANNOUNCE_OK follows it.
	pub(in crate::lite) fn holding(&self) -> bool {
		self.mounts.values().any(|mount| mount.holds == Holds::Main)
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
			Msg::Edited if source == MAIN => match self.markers.pop_front() {
				Some(Marker::Grant { removed }) => {
					self.answering = false;
					self.regranted(writer, removed)
				}
				_ => {
					self.answering = true;
					self.edited(writer)
				}
			},
			// A mount's own marker: the session's covers the edit that made it.
			Msg::Edited => Ok(()),
			Msg::Synced(path) => {
				let id = match self.mounts.get_mut(&source) {
					Some(mount) => {
						let holds = std::mem::replace(&mut mount.holds, Holds::Nothing);
						(holds == Holds::Synced).then_some(mount.prefix)
					}
					// What a grant change reads starts after its prefix's SYNCED.
					None if !self.answering => None,
					None => (self.prefixes.iter())
						.find(|(_, prefix)| {
							!prefix.synced && prefix.path.as_ref().is_some_and(|prefix| route::within(&path, prefix))
						})
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
			// The node's own key answers each read, as the edit asked for them, and so does
			// each mount beneath what the grant shows now.
			prefix.reads = (prefix.path.as_ref()).map(|path| self.narrow(path)).unwrap_or_default();
			for read in prefix.reads.clone() {
				prefix.waiting += self.mount(id, read, Holds::Synced)?;
			}
			if prefix.waiting == 0 {
				ready.push(id);
			}
			self.prefixes.insert(id, prefix);
		}
		self.main.flush();
		// A prefix that reads nothing has no initial set to wait for.
		for id in ready {
			self.synced(writer, id, 0)?;
		}
		Ok(())
	}

	/// Read each mount beneath the absolute `read` of the prefix with Prefix ID `id`
	/// through a key of its own, its initial set holding back `holds`. Returns how many.
	fn mount(&mut self, id: u64, read: Path, holds: Holds) -> Result<usize, Error> {
		if self.routing {
			return Ok(0);
		}
		let mounted = self.origin.mounted(&read);
		let count = mounted.len();
		for mount in mounted {
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
				read: read.clone(),
				holds,
			};
			self.mounts.insert(source, mount);
		}
		Ok(count)
	}

	/// The marker of a grant change: the session's key no longer reads `removed`, and the
	/// node dropped what they showed. End what the grant no longer shows, start what it
	/// now does, and read the mounts beneath each region it newly shows.
	fn regranted(&mut self, writer: &mut Writer, removed: Vec<Path>) -> Result<(), Error> {
		let mut gone: Vec<Named> = (self.told.iter())
			.filter(|(key, (source, _))| *source == MAIN && removed.iter().any(|read| route::within(&key.0, read)))
			.map(|(key, _)| key.clone())
			.collect();
		let ids: Vec<u64> = self.prefixes.keys().copied().collect();
		for id in ids {
			let prefix = &self.prefixes[&id];
			let reads = (prefix.path.as_ref()).map(|path| self.narrow(path)).unwrap_or_default();
			let dropped: Vec<Path> = prefix.reads.difference(&reads).cloned().collect();
			let added: Vec<Path> = reads.difference(&prefix.reads).cloned().collect();
			self.prefixes.get_mut(&id).expect("listed above").reads = reads;
			// The mounts beneath a read the grant no longer shows go with it.
			let sources: Vec<Source> = (self.mounts.iter())
				.filter(|(_, mount)| mount.prefix == id && dropped.contains(&mount.read))
				.map(|(source, _)| *source)
				.collect();
			for source in sources {
				let mount = self.mounts.remove(&source).expect("listed above");
				// Its initial set no longer holds the prefix back.
				if mount.holds == Holds::Synced {
					self.synced(writer, id, 1)?;
				}
				gone.extend(
					(self.told.iter())
						.filter(|(_, (told, _))| *told == source)
						.map(|(key, _)| key.clone()),
				);
			}
			for read in added {
				self.mount(id, read, Holds::Main)?;
			}
		}
		for key in &gone {
			self.told.remove(key);
		}
		self.main.flush();
		let mut keys: Vec<Named> = self.told.keys().cloned().collect();
		keys.extend(self.live.keys().filter(|key| !self.told.contains_key(key)).cloned());
		for key in keys {
			// No longer granted, it is lost to the peer: its publisher never ended it.
			self.sync(writer, key, true)?;
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
		Self::alone_on(origin, crate::auth::Link::new(false))
	}

	/// [`Self::alone`], on the session `auth`.
	pub(in crate::lite) fn alone_on(origin: origin::Consumer, auth: crate::auth::Link) -> Self {
		let port = relay::Port {
			cost: 1,
			priority: 0,
			upstream: false,
			role: relay::Role::Scoped(Vec::new()),
		};
		let main = origin.route_node().connect(lite::session_tag(), port).unwrap();
		Self::new(origin, auth, Arc::new(main), false)
	}

	/// Send everything the node queued for the peer.
	pub(in crate::lite) fn drain(&mut self, writer: &mut Writer) -> Result<(), Error> {
		let waiter = kio::Waiter::noop();
		self.main.flush();
		loop {
			if !self.holding()
				&& let Some(msg) = self.main.try_wire()
			{
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
