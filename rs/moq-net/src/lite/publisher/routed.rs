//! Lite-07: an announce stream the peer asked for, answered from the origin's route node.
//!
//! The node sends the peer its routes and, closed by an ANNOUNCE_FLOOR, each route's
//! claims under the requested prefix. The stream filters what it puts on the wire:
//! hidden claims unless the peer asked for them, and anything this handle's scope, the
//! session's grant, the upstream horizon, or the proxy preference leaves out, in which
//! case a floor certifies nothing, since the peer cannot tell what was left out from
//! what is gone. A route goes on the wire only while the stream shows the peer one
//! of its claims, so a route whose claims it filters out costs the peer nothing.
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

/// A route the node sends the stream, by the origin node it reaches.
struct Sent {
	/// Its Route ID while the peer holds it: from the first claim the stream shows it
	/// until the last one goes.
	wire: Option<u64>,
	/// The last finite advertisement, which a ROUTE_START carries.
	advert: route::Advert,
	/// How many of its claims the peer is shown.
	shown: usize,
	/// Each source's last reach for it, by index: the route stands while one has one,
	/// and the first one's is the peer's.
	reach: BTreeMap<usize, route::Reach>,
	/// Every claim the node sent for it, named as the handle names it, before filtering.
	claims: BTreeMap<Path, node::Claim>,
	/// How many of `claims` the stream's link never carries (see [`sends`]).
	withheld: usize,
	/// The node's floor for those claims.
	floor: Option<route::Rev>,
	/// Whether the peer was sent a floor for it yet.
	floored: bool,
}

impl Sent {
	fn reach(&self) -> Option<route::Reach> {
		self.reach.values().next().copied()
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
	/// sources whose own end of it waits on that echo: none when the stream ended it
	/// for showing nothing.
	echoes: HashMap<u64, (route::Id, Vec<usize>)>,
	/// The Route ID of each origin node's end in `echoes`: a route starts again only
	/// once its last end is echoed, so a peer that withholds echoes holds at most one
	/// end per node.
	ending: HashMap<route::Id, u64>,
	/// The claims the peer holds: their Announce ID and attributes.
	live: HashMap<(route::Id, Path), (u64, node::Claim)>,
	gate: crate::auth::Gate,
	epoch: u64,
	permit: crate::auth::Permit,
	phase: Phase,
	/// Passes before the stream yields, so a peer flooding replies cannot hold the task.
	budget: kio::coop::Budget,
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
			ending: HashMap::new(),
			live: HashMap::new(),
			epoch: 0,
			permit: Default::default(),
			phase: Phase::Init,
			budget: kio::coop::Budget::new(32),
		}
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
				if let Some(wire) = sent.wire
					&& !sent.floored
				{
					sent.floored = true;
					let floor = Msg::Floor {
						id: wire,
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
					if let Some(wire) = sent.wire {
						stream.writer.buffer(&Msg::RouteEnd { id: wire })?;
					}
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
					self.reply(stream, reply)?;
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
	fn reply(&mut self, stream: &mut Stream<Transport, Version>, reply: lite::AnnounceReply) -> Result<(), Error> {
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
				let (node, waiting) = self.echoes.remove(&id).ok_or(Error::ProtocolViolation)?;
				self.ending.remove(&node);
				for index in waiting {
					if let Some(source) = self.sources.get(index) {
						source.stream.echo(node);
					}
				}
				self.resume(stream, node)?;
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
			// Held only follows a route the node sent, and an end of one it did not is moot.
			// The peer is sent the route with its first claim shown.
			if let Some(reach @ route::Reach::Finite(advert)) = reach {
				let sent = Sent {
					wire: None,
					advert,
					shown: 0,
					reach: BTreeMap::from([(source, reach)]),
					claims: BTreeMap::new(),
					withheld: 0,
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
				let Some(wire) = sent.wire else {
					self.echo(id, source);
					return Ok(());
				};
				stream.writer.buffer(&Msg::RouteEnd { id: wire })?;
				// The ROUTE_END retires every claim under the route.
				for path in sent.claims.keys() {
					if let Some((announce, _)) = self.live.remove(&(id, path.clone())) {
						self.encoder.end(announce);
					}
				}
				self.ending.insert(id, wire);
				self.echoes.insert(wire, (id, vec![source]));
				return Ok(());
			}
			None => {
				// Another source still carries the route, so the peer keeps it: the claims
				// this one presented end, and its end is acked as the peer's copy allows.
				let paths: Vec<Path> = sent.claims.keys().cloned().collect();
				let paths: Vec<Path> = (paths.into_iter()).filter(|path| self.source(path) == source).collect();
				self.echo(id, source);
				let sent = self.routes.get_mut(&id).expect("matched above");
				for path in &paths {
					sent.set(&self.link, path.clone(), None);
				}
				for path in paths {
					self.sync(stream, id, path)?;
				}
				if self.routes[&id].shown == 0 {
					self.floor(stream, id)?;
					self.settle(stream, id)?;
				}
			}
		}
		let sent = self
			.routes
			.get_mut(&id)
			.expect("a route stands while a source carries it");
		let after = sent.reach();
		if let Some(route::Reach::Finite(advert)) = after {
			sent.advert = advert;
		}
		let Some(wire) = sent.wire else {
			return Ok(());
		};
		match after {
			_ if after == before => {}
			Some(route::Reach::Finite(advert)) => stream.writer.buffer(&Msg::RouteUpdate { id: wire, advert })?,
			Some(route::Reach::Held) => stream.writer.buffer(&Msg::RouteHold { id: wire })?,
			None => unreachable!("a route stands while a source carries it"),
		}
		Ok(())
	}

	/// The route's Route ID, sending ROUTE_START if the peer does not hold it, or
	/// `None` while the peer has not echoed the route's last end.
	fn start(&mut self, stream: &mut Stream<Transport, Version>, id: route::Id) -> Result<Option<u64>, Error> {
		let sent = self.routes.get_mut(&id).expect("only a held route starts");
		if let Some(wire) = sent.wire {
			return Ok(Some(wire));
		}
		if self.ending.contains_key(&id) {
			return Ok(None);
		}
		let wire = self.next_route;
		self.next_route += 1;
		stream.writer.buffer(&Msg::RouteStart {
			node: id,
			advert: sent.advert,
		})?;
		if sent.reach() == Some(route::Reach::Held) {
			stream.writer.buffer(&Msg::RouteHold { id: wire })?;
		}
		sent.wire = Some(wire);
		Ok(Some(wire))
	}

	/// End the route on the wire once it shows the peer nothing; the caller sent the
	/// floor certifying that first.
	fn settle(&mut self, stream: &mut Stream<Transport, Version>, id: route::Id) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(&id) else {
			return Ok(());
		};
		let Some(wire) = sent.wire.filter(|_| sent.shown == 0) else {
			return Ok(());
		};
		sent.wire = None;
		sent.floored = false;
		stream.writer.buffer(&Msg::RouteEnd { id: wire })?;
		self.ending.insert(id, wire);
		self.echoes.insert(wire, (id, Vec::new()));
		Ok(())
	}

	/// Echo `source`'s end of the route to `id` once the peer holds no copy of it: at
	/// once, or with the echo of the route's outstanding end, since until then the
	/// peer may still hold and forward the advertisement.
	fn echo(&mut self, id: route::Id, source: usize) {
		match self.ending.get(&id).and_then(|wire| self.echoes.get_mut(wire)) {
			Some((_, waiting)) => waiting.push(source),
			None => self.sources[source].stream.echo(id),
		}
	}

	/// Show the peer the route's claims again once it echoed the route's last end.
	fn resume(&mut self, stream: &mut Stream<Transport, Version>, id: route::Id) -> Result<(), Error> {
		let Some(sent) = self.routes.get(&id) else {
			return Ok(());
		};
		let paths: Vec<Path> = sent.claims.keys().cloned().collect();
		for path in paths {
			self.sync(stream, id, path)?;
		}
		self.floor(stream, id)
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
			sent.set(&self.link, path.clone(), claim);
			paths.push(path);
		}
		sent.floor = floor;
		for path in paths {
			self.sync(stream, id, path)?;
		}
		self.floor(stream, id)?;
		self.settle(stream, id)
	}

	/// Send the ANNOUNCE_FLOOR closing a batch for the route to `id`, if the peer holds it.
	fn floor(&mut self, stream: &mut Stream<Transport, Version>, id: route::Id) -> Result<(), Error> {
		let certified = self.certified();
		// Like the grant, the upstream horizon and the proxy preference leave claims out
		// that a floor would certify absent.
		let sent = self.routes.get_mut(&id).expect("only a held route has a batch");
		let hides = sent.withheld > 0;
		let Some(wire) = sent.wire else {
			return Ok(());
		};
		sent.floored = true;
		let revision = sent.floor.filter(|_| certified && !hides);
		stream.writer.buffer(&Msg::Floor { id: wire, revision })
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
		let key = (id, path);
		match (self.live.get_mut(&key), wanted) {
			(None, Some(claim)) => {
				let Some(wire) = self.start(stream, id)? else {
					// Shown once the peer echoes the route's last end.
					return Ok(());
				};
				let suffix = self.suffix(&key.1).expect("shown claims are under the prefix");
				tracing::debug!(route = %key.1, node = id, "announce");
				let (announce, suffix) = self.encoder.start(suffix);
				stream.writer.buffer(&Msg::Active {
					suffix,
					route: wire,
					instance: Some(claim.instance.clone()),
					epoch: claim.epoch,
					hops: crate::Hops::new(),
					cost: origin::Cost::new(claim.cost),
				})?;
				let announce = announce.expect("lite-07 assigns announce ids");
				self.live.insert(key, (announce, claim));
				self.routes.get_mut(&id).expect("started above").shown += 1;
			}
			(Some((announce, held)), Some(claim)) => {
				let announce = *announce;
				let restart = held.instance != claim.instance || held.epoch != claim.epoch;
				let reprice = held.cost != claim.cost;
				if restart {
					tracing::debug!(route = %key.1, "restart");
					stream.writer.buffer(&Msg::Restart {
						id: announce,
						instance: Some(claim.instance.clone()),
						epoch: claim.epoch,
						cost: origin::Cost::new(claim.cost),
					})?;
				} else if reprice {
					stream.writer.buffer(&Msg::Update {
						id: announce,
						hops: crate::Hops::new(),
						cost: origin::Cost::new(claim.cost),
					})?;
				}
				self.live.insert(key, (announce, claim));
			}
			(Some(_), None) => {
				let (announce, _) = self.live.remove(&key).expect("matched above");
				stream.writer.buffer(&Msg::EndedId { id: announce })?;
				self.encoder.end(announce);
				self.routes.get_mut(&id).expect("only a held route syncs").shown -= 1;
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
			self.floor(stream, id)?;
			self.settle(stream, id)?;
		}
		Ok(())
	}
}
