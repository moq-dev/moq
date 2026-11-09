//! Lite-07: an announce stream the peer asked for, answered from the origin's route node.
//!
//! The node sends the peer its routes and, closed by an ANNOUNCE_FLOOR, each route's
//! claims under the requested prefix. The stream filters what it puts on the wire:
//! hidden claims unless the peer asked for them, and anything this handle's scope, the
//! session's grant, the upstream horizon, or the proxy preference leaves out, in which
//! case a floor certifies nothing, since the peer cannot tell what was left out from
//! what is gone.
//!
//! A handle's mounts beneath the prefix are read through a node stream each, at the
//! target, and what they present is named under the mount point on the same routes.

use std::{
	collections::{BTreeMap, HashMap},
	task::{Poll, ready},
};

use crate::{
	Error, Path,
	coding::Stream,
	driver::Transport,
	lite::{self, AnnounceBroadcast as Msg, Version},
	origin,
	route::{self, node},
};

/// A route the peer holds, by the origin node it reaches.
struct Sent {
	/// Its Route ID on the stream.
	wire: u64,
	/// Each source's last reach for it, by index: the route stands while one has one,
	/// and the first one's is the peer's.
	reach: BTreeMap<usize, route::Reach>,
	/// Every claim the node sent for it, named as the handle names it, before filtering.
	claims: BTreeMap<Path, node::Claim>,
	/// The node's floor for those claims.
	floor: Option<route::Rev>,
	/// Whether the peer was sent a floor for it yet.
	floored: bool,
}

impl Sent {
	fn reach(&self) -> Option<route::Reach> {
		self.reach.values().next().copied()
	}
}

/// A node stream the answer reads.
struct Source {
	stream: node::Stream,
	/// The mount it reads through, which names what it presents. `None` for the
	/// requested prefix's own.
	mount: Option<origin::Mounted>,
}

enum Phase {
	/// Waiting for the session's grant before the first message.
	Init,
	Running,
	/// The session is going away: every route is ended, the stream finishes once
	/// that is flushed.
	Withdrawing,
	/// Finished: waiting for the peer to read it.
	Closing,
}

/// Whether a stream on `link` carries `claim` at all: never an upstream-learned claim
/// to an upstream, nor one learned from another node to a peer this node does not
/// proxy for.
fn sends(link: &route::Link, claim: &node::Claim) -> bool {
	!(link.upstream && claim.upstream) && (link.proxy || !claim.learned)
}

/// Answers one lite-07 announce stream.
pub(super) struct Answer {
	origin: origin::Consumer,
	/// The requested prefix, absolute. `None` when it is too long to lie beneath the
	/// handle's root: nothing is there, so the stream announces nothing.
	prefix: Option<Path>,
	hidden: bool,
	link: route::Link,
	/// The prefix's own node stream, then one per mount beneath it. Empty until the
	/// grant is known, and once withdrawing.
	sources: Vec<Source>,
	/// The source read first next, so a busy one does not hold the others.
	turn: usize,
	encoder: lite::AnnounceEncoder,
	routes: HashMap<route::Id, Sent>,
	next_route: u64,
	/// Route IDs ended and not yet echoed, the origin node each reached, and the
	/// source that ended it.
	echoes: HashMap<u64, (route::Id, usize)>,
	/// The claims the peer holds: their Announce ID and attributes.
	live: HashMap<(route::Id, Path), (u64, node::Claim)>,
	gate: crate::auth::Gate,
	epoch: u64,
	permit: crate::auth::Permit,
	phase: Phase,
	/// Passes before the stream yields, so a peer flooding replies cannot hold the task.
	budget: kio::coop::Budget,
	/// The peer's SETUP, which says whether it reads the Lite-06 Chain. `None` sends no
	/// chain.
	peer_setup: Option<lite::PeerSetup>,
	/// Whether the claims sent carry their Lite-06 Chain, known once the peer's SETUP is.
	chains: bool,
}

impl Answer {
	/// Answer the peer's request for `prefix` (relative to the session's root).
	pub(super) fn new(origin: origin::Consumer, prefix: Path, hidden: bool, auth: crate::auth::Link) -> Self {
		let link = route::Link {
			// The peer prices the link on what it receives; nothing here charges it.
			cost: 1,
			priority: 0,
			upstream: origin.upstream(),
			proxy: origin.proxy(),
			scope: None,
		};
		let absolute = origin.root().join(&prefix).ok();
		Self {
			gate: crate::auth::Gate::new(auth, prefix, crate::auth::Direction::Publish),
			prefix: absolute,
			origin,
			hidden,
			link,
			sources: Vec::new(),
			turn: 0,
			encoder: lite::AnnounceEncoder::new(Version::Lite07),
			routes: HashMap::new(),
			next_route: 0,
			echoes: HashMap::new(),
			live: HashMap::new(),
			epoch: 0,
			permit: Default::default(),
			phase: Phase::Init,
			budget: kio::coop::Budget::new(32),
			peer_setup: None,
			chains: false,
		}
	}

	/// Send each claim's Lite-06 Chain if the peer's SETUP offers to read it.
	pub(super) fn with_peer_setup(mut self, peer_setup: lite::PeerSetup) -> Self {
		self.peer_setup = Some(peer_setup);
		self
	}

	/// Run the stream until the peer closes it, or `withdrawing` closes it here.
	pub(super) fn poll(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		withdrawing: bool,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		if matches!(self.phase, Phase::Init) {
			// Start from the grant the setup token earns, so the initial set never
			// advertises something it would end a moment later.
			ready!(self.gate.poll_setup_answered(waiter));
			if let Some(peer_setup) = &self.peer_setup {
				self.chains = ready!(peer_setup.poll_lite06_chain(waiter));
			}
			if let Poll::Ready(permit) = self.gate.poll_permit(&mut self.epoch, waiter) {
				self.permit = permit;
			}
			stream.writer.buffer(&lite::AnnounceOk {
				origin: crate::Hop::UNKNOWN,
				active: 0,
			})?;
			let node = self.origin.route_node();
			let mounts = (self.prefix.as_ref()).map_or_else(Vec::new, |prefix| self.origin.mounted(prefix));
			let interests = (self.prefix.iter())
				.map(|prefix| (prefix.clone(), None))
				.chain(mounts.into_iter().map(|mount| (mount.interest.clone(), Some(mount))));
			for (interest, mount) in interests {
				let stream = node
					.answer(self.link.clone(), interest)
					.map_err(|_| Error::ProtocolViolation)?;
				self.sources.push(Source { stream, mount });
			}
			for index in 0..self.sources.len() {
				while let Some(output) = self.sources[index].stream.try_output() {
					self.output(stream, index, output)?;
				}
			}
			// Every route the peer holds has a floor before SYNCED; one the node has
			// not certified yet certifies nothing.
			for sent in self.routes.values_mut() {
				if !sent.floored {
					sent.floored = true;
					let floor = Msg::Floor {
						id: sent.wire,
						revision: None,
					};
					stream.writer.buffer(&floor)?;
				}
			}
			stream.writer.buffer(&Msg::Synced)?;
			self.phase = Phase::Running;
		}

		loop {
			ready!(self.budget.poll_yield(waiter));
			// Deliver what is buffered before taking more work.
			ready!(stream.writer.poll_flush(waiter))?;

			match self.phase {
				Phase::Withdrawing => {
					stream.writer.finish()?;
					self.phase = Phase::Closing;
					continue;
				}
				Phase::Closing => return stream.writer.poll_closed(waiter),
				_ => {}
			}

			if withdrawing {
				// Let go of the node first, so nothing more is queued, then end every route.
				self.sources.clear();
				for (_, sent) in std::mem::take(&mut self.routes) {
					stream.writer.buffer(&Msg::RouteEnd { id: sent.wire })?;
				}
				self.live.clear();
				self.phase = Phase::Withdrawing;
				continue;
			}

			// A grant change applies before the next batch, so a claim it no longer
			// covers is ended rather than sent.
			if let Poll::Ready(permit) = self.gate.poll_permit(&mut self.epoch, waiter)
				&& permit != self.permit
			{
				self.permit = permit;
				self.refilter(stream)?;
				continue;
			}

			// One reply and one output per pass, so neither direction starves the other.
			let mut progress = false;
			match stream.reader.poll_decode_maybe::<lite::AnnounceReply>(waiter) {
				Poll::Ready(Ok(Some(reply))) => {
					self.reply(reply)?;
					progress = true;
				}
				// The peer closed its side: it wants nothing more.
				Poll::Ready(Ok(None)) => return Poll::Ready(Ok(())),
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => {}
			}

			let count = self.sources.len();
			let ready = (0..count).map(|offset| (self.turn + offset) % count).find_map(|index| {
				match self.sources[index].stream.poll_output(waiter) {
					Poll::Ready(output) => Some((index, output)),
					Poll::Pending => None,
				}
			});
			if let Some((index, output)) = ready {
				self.turn = index + 1;
				self.output(stream, index, output)?;
				progress = true;
			}
			if !progress {
				return Poll::Pending;
			}
		}
	}

	/// A ROUTE_REQUEST, or the echo of a ROUTE_END.
	fn reply(&mut self, reply: lite::AnnounceReply) -> Result<(), Error> {
		match reply {
			lite::AnnounceReply::RouteRequest { node, seqno } => {
				// Through the first source carrying the route, or the prefix's own.
				let index = (self.routes.get(&node))
					.and_then(|sent| sent.reach.keys().next().copied())
					.unwrap_or(0);
				if let Some(source) = self.sources.get(index) {
					source.stream.request(node, seqno);
				}
			}
			lite::AnnounceReply::RouteEnd { id } => {
				// The echo names the Route ID; the node knows the route by its origin node.
				let (node, index) = self.echoes.remove(&id).ok_or(Error::ProtocolViolation)?;
				if let Some(source) = self.sources.get(index) {
					source.stream.echo(node);
				}
			}
		}
		Ok(())
	}

	fn output(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		source: usize,
		output: node::Output,
	) -> Result<(), Error> {
		match output {
			node::Output::Route { id, reach } => self.route(stream, source, id, reach),
			node::Output::Batch { id, changes, floor } => self.batch(stream, source, id, changes, floor),
			// Only a stream this node asked for serves or echoes.
			node::Output::Echo { .. } | node::Output::Serve { .. } | node::Output::Unserve { .. } => Ok(()),
		}
	}

	fn route(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		source: usize,
		id: route::Id,
		reach: Option<route::Reach>,
	) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(&id) else {
			// Held only follows a route the peer holds, and an end of one it does not is moot.
			if let Some(reach @ route::Reach::Finite(advert)) = reach {
				let wire = self.next_route;
				self.next_route += 1;
				stream.writer.buffer(&Msg::RouteStart { node: id, advert })?;
				let sent = Sent {
					wire,
					reach: BTreeMap::from([(source, reach)]),
					claims: BTreeMap::new(),
					floor: None,
					floored: false,
				};
				self.routes.insert(id, sent);
			}
			return Ok(());
		};
		let before = sent.reach();
		match reach {
			Some(reach) => {
				sent.reach.insert(source, reach);
			}
			None if sent.reach.remove(&source).is_none() => return Ok(()),
			None if sent.reach.is_empty() => {
				let sent = self.routes.remove(&id).expect("matched above");
				stream.writer.buffer(&Msg::RouteEnd { id: sent.wire })?;
				// The ROUTE_END retires every claim under the route.
				for path in sent.claims.keys() {
					if let Some((announce, _)) = self.live.remove(&(id, path.clone())) {
						self.encoder.end(announce);
					}
				}
				self.echoes.insert(sent.wire, (id, source));
				return Ok(());
			}
			None => {
				// Another source still carries the route, so the peer keeps it: the claims
				// this one presented end, and its end is acked at once.
				let paths: Vec<Path> = sent.claims.keys().cloned().collect();
				let paths: Vec<Path> = (paths.into_iter()).filter(|path| self.source(path) == source).collect();
				self.sources[source].stream.echo(id);
				let sent = self.routes.get_mut(&id).expect("matched above");
				for path in &paths {
					sent.claims.remove(path);
				}
				for path in paths {
					self.sync(stream, id, path)?;
				}
			}
		}
		let sent = &self.routes[&id];
		match sent.reach() {
			after if after == before => {}
			Some(route::Reach::Finite(advert)) => stream.writer.buffer(&Msg::RouteUpdate { id: sent.wire, advert })?,
			Some(route::Reach::Held) => stream.writer.buffer(&Msg::RouteHold { id: sent.wire })?,
			None => unreachable!("a route stands while a source carries it"),
		}
		Ok(())
	}

	/// The index of the source presenting the claim at the handle-side `path`: the
	/// mount it lies beneath, or the prefix's own.
	fn source(&self, path: &Path) -> usize {
		(self.sources.iter())
			.position(|source| source.mount.as_ref().is_some_and(|mount| mount.holds(path)))
			.unwrap_or(0)
	}

	fn batch(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		source: usize,
		id: route::Id,
		changes: Vec<(Path, Option<node::Claim>)>,
		floor: Option<route::Rev>,
	) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(&id) else {
			return Ok(());
		};
		let mount = self.sources.get(source).and_then(|source| source.mount.as_ref());
		let mut paths = Vec::with_capacity(changes.len());
		for (path, claim) in changes {
			// Named as the handle names it: beneath the mount point for a mount, and never
			// a claim the origin holds beneath a mount for the prefix's own.
			let path = match mount {
				Some(mount) => match mount.name(&path) {
					Some(named) => named,
					None => continue,
				},
				None if self.origin.shadows(&path) => continue,
				None => path,
			};
			match claim {
				Some(claim) => sent.claims.insert(path.clone(), claim),
				None => sent.claims.remove(&path),
			};
			paths.push(path);
		}
		sent.floor = floor;
		for path in paths {
			self.sync(stream, id, path)?;
		}
		self.floor(stream, id)
	}

	/// Send the ANNOUNCE_FLOOR closing a batch for the route to `id`.
	fn floor(&mut self, stream: &mut Stream<Transport, Version>, id: route::Id) -> Result<(), Error> {
		let certified = self.certified();
		// Like the grant, the upstream horizon and the proxy preference leave claims out
		// that a floor would certify absent.
		let hides = (self.routes.get(&id).into_iter())
			.flat_map(|sent| sent.claims.values())
			.any(|claim| !sends(&self.link, claim));
		let sent = self.routes.get_mut(&id).expect("only a held route has a batch");
		sent.floored = true;
		let revision = sent.floor.filter(|_| certified && !hides);
		stream.writer.buffer(&Msg::Floor {
			id: sent.wire,
			revision,
		})
	}

	/// Whether a floor may certify the whole requested prefix: nothing this handle's
	/// scope or the session's grant leaves out lies beneath it. A hidden claim left out
	/// does not count, since the peer did not ask to see it.
	fn certified(&self) -> bool {
		let Some(prefix) = &self.prefix else {
			return false;
		};
		let relative = prefix.strip_prefix(self.origin.root()).unwrap_or_default();
		self.origin.covers(prefix) && self.permit.covers_subtree(relative.as_str())
	}

	/// The absolute `path` as the peer names it, under its requested prefix.
	fn suffix(&self, path: &Path) -> Option<Path> {
		path.strip_prefix(self.prefix.as_ref()?)
	}

	/// Whether the peer is shown the claim at the absolute `path`.
	fn shows(&self, path: &Path) -> bool {
		let Some(suffix) = self.suffix(path) else {
			return false;
		};
		let Some(relative) = path.strip_prefix(self.origin.root()) else {
			return false;
		};
		(self.hidden || !suffix.is_hidden()) && self.origin.reaches(path) && self.permit.matches(relative.as_str())
	}

	/// Bring the peer's claim at `path` for the route to `id` in line with the node's.
	fn sync(&mut self, stream: &mut Stream<Transport, Version>, id: route::Id, path: Path) -> Result<(), Error> {
		let sent = self.routes.get(&id).expect("only a held route syncs");
		let wanted = (sent.claims.get(&path))
			.filter(|claim| self.shows(&path) && sends(&self.link, claim))
			.cloned();
		let wire = sent.wire;
		// The Lite-06 Chain goes only to a peer that reads it.
		let chain = (wanted.as_ref())
			.filter(|_| self.chains)
			.map(|claim| claim.chain.clone())
			.unwrap_or_default();
		let key = (id, path);
		match (self.live.get_mut(&key), wanted) {
			(None, Some(claim)) => {
				let suffix = self.suffix(&key.1).expect("shown claims are under the prefix");
				tracing::debug!(route = %key.1, node = id, "announce");
				let (announce, suffix) = self.encoder.start(suffix);
				stream.writer.buffer(&Msg::Active {
					suffix,
					route: wire,
					instance: Some(claim.instance.clone()),
					epoch: claim.epoch,
					hops: chain,
					cost: origin::Cost::new(claim.cost),
				})?;
				let announce = announce.expect("lite-07 assigns announce ids");
				self.live.insert(key, (announce, claim));
			}
			(Some((announce, held)), Some(claim)) => {
				let announce = *announce;
				let restart = held.instance != claim.instance || held.epoch != claim.epoch;
				let update = held.cost != claim.cost || (self.chains && held.chain != claim.chain);
				if restart {
					tracing::debug!(route = %key.1, "restart");
					stream.writer.buffer(&Msg::Restart {
						id: announce,
						instance: Some(claim.instance.clone()),
						epoch: claim.epoch,
						cost: origin::Cost::new(claim.cost),
						hops: chain,
					})?;
				} else if update {
					stream.writer.buffer(&Msg::Update {
						id: announce,
						hops: chain,
						cost: origin::Cost::new(claim.cost),
					})?;
				}
				self.live.insert(key, (announce, claim));
			}
			(Some(_), None) => {
				let (announce, _) = self.live.remove(&key).expect("matched above");
				stream.writer.buffer(&Msg::EndedId { id: announce })?;
				self.encoder.end(announce);
			}
			(None, None) => {}
		}
		Ok(())
	}

	/// The grant changed: end what it no longer covers, start what it now does, and
	/// floor every route again, which certifies only while the grant covers the prefix.
	fn refilter(&mut self, stream: &mut Stream<Transport, Version>) -> Result<(), Error> {
		let mut ids: Vec<_> = self.routes.keys().copied().collect();
		ids.sort_unstable();
		for id in ids {
			let paths: Vec<Path> = self.routes[&id].claims.keys().cloned().collect();
			for path in paths {
				self.sync(stream, id, path)?;
			}
			if self.routes[&id].floored {
				self.floor(stream, id)?;
			}
		}
		Ok(())
	}
}
