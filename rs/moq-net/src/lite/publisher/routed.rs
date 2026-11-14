//! Lite-07: an announce stream the peer asked for, answered from the origin's route node.
//!
//! The node sends the peer its routes, each by its publisher and the prefix it covers,
//! the DEADs naming hops on them, and, closed by an ANNOUNCE_FLOOR, each route's claims
//! under the requested prefix. The stream filters what it puts on the wire: hidden
//! claims unless the peer asked for them, and anything this handle's scope, the
//! session's grant, the upstream horizon, or the proxy preference leaves out, in which
//! case a floor certifies nothing, since the peer cannot tell what was left out from
//! what is gone. A route goes on the wire only while the stream shows the peer one of
//! its claims, so a route whose claims it filters out costs the peer nothing.
//!
//! A handle's mounts beneath the prefix are read through a node stream each, at the
//! target, and what they present is named under the mount point on the same routes.
//!
//! The peer sends nothing back on the stream. One that stops reading it for
//! [`route::path::HOLD`] closes its session: the node keeps no more than each route's
//! net change for it meanwhile, and a hold waiting on it ends by then anyway.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	task::{Poll, ready},
};

use crate::{
	Error, Path, SessionError,
	coding::Stream,
	driver::Transport,
	lite::{self, AnnounceBroadcast as Msg, Version},
	origin,
	route::{
		self, node,
		path::{Dead, Hop, Offer, Reach},
	},
	transport::poll::Session as _,
};

/// A route by its publisher and the prefix it covers, as the handle names it.
type RouteKey = (route::Id, Path);

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
}

/// A route the node sends the stream.
struct Sent {
	/// While the peer holds it: from the first claim the stream shows it until the last
	/// one goes.
	wire: Option<Wire>,
	/// How many of its claims the peer is shown.
	shown: usize,
	/// Each source's reach for it, by index: the route stands while one has one, and
	/// the first one's is the peer's.
	reach: BTreeMap<usize, Reached>,
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

/// Closes the session whose peer stopped reading the stream.
struct Stall {
	session: Transport,
	runtime: crate::time::Deadlines,
	/// When the stream counts as stalled, and how much it had written when that was set.
	armed: Option<(crate::time::Deadline, u64)>,
}

impl Stall {
	/// Ready once the writer made no progress for [`route::path::HOLD`].
	fn poll(&mut self, written: u64, waiter: &kio::Waiter) -> Poll<()> {
		if self.armed.as_ref().is_none_or(|(_, at)| *at != written) {
			let deadline = crate::time::Deadline::after(&self.runtime, route::path::HOLD);
			self.armed = Some((deadline, written));
		}
		let (deadline, _) = self.armed.as_mut().expect("armed above");
		ready!(deadline.poll(waiter));
		self.session.close(
			lite::error::to_session_code(&SessionError::Timeout),
			"announce stream stalled",
		);
		Poll::Ready(())
	}
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
	routes: HashMap<RouteKey, Sent>,
	next_route: u64,
	/// The claims the peer holds: their Announce ID and attributes.
	live: HashMap<(RouteKey, Path), (u64, node::Claim)>,
	gate: crate::auth::Gate,
	epoch: u64,
	permit: crate::auth::Permit,
	phase: Phase,
	/// Passes before the stream yields, so a busy node cannot hold the task.
	budget: kio::coop::Budget,
	/// The peer's SETUP: its Node ID, its session, and whether it reads the Lite-06
	/// Chain. `None` sends no chain.
	peer_setup: Option<lite::PeerSetup>,
	/// Whether the claims sent carry their Lite-06 Chain, known once the peer's SETUP is.
	chains: bool,
	/// `None` never closes the session.
	stall: Option<Stall>,
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
			live: HashMap::new(),
			epoch: 0,
			permit: Default::default(),
			phase: Phase::Init,
			budget: kio::coop::Budget::new(32),
			peer_setup: None,
			chains: false,
			stall: None,
		}
	}

	/// Send each claim's Lite-06 Chain if the peer's SETUP offers to read it, and name
	/// the peer to the node as its SETUP does.
	pub(super) fn with_peer_setup(mut self, peer_setup: lite::PeerSetup) -> Self {
		self.peer_setup = Some(peer_setup);
		self
	}

	/// Close `session` if the peer stops reading the stream.
	pub(super) fn with_session(mut self, session: Transport, runtime: crate::time::Deadlines) -> Self {
		self.stall = Some(Stall {
			session,
			runtime,
			armed: None,
		});
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
			let peer = match &self.peer_setup {
				Some(peer_setup) => node::Peer {
					tag: peer_setup.tag(),
					node: peer_setup.node().map_or(0, |node| node.get()),
				},
				None => node::Peer {
					tag: lite::session_tag(),
					node: 0,
				},
			};
			let node = self.origin.route_node();
			let mounts = (self.prefix.as_ref()).map_or_else(Vec::new, |prefix| self.origin.mounted(prefix));
			let interests = (self.prefix.iter())
				.map(|prefix| (prefix.clone(), None))
				.chain(mounts.into_iter().map(|mount| (mount.interest.clone(), Some(mount))));
			for (interest, mount) in interests {
				let stream = (node.answer(peer, self.link.clone(), interest)).map_err(|_| Error::ProtocolViolation)?;
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
				if let Some(wire) = &sent.wire
					&& !sent.floored
				{
					sent.floored = true;
					stream.writer.buffer(&Msg::Floor {
						id: wire.id,
						revision: None,
					})?;
				}
			}
			stream.writer.buffer(&Msg::Synced)?;
			self.phase = Phase::Running;
		}

		loop {
			ready!(self.budget.poll_yield(waiter));
			// Deliver what is buffered before taking more work.
			if let Poll::Ready(flushed) = stream.writer.poll_flush(waiter) {
				flushed?;
				if let Some(stall) = &mut self.stall {
					stall.armed = None;
				}
			} else {
				if let Some(stall) = &mut self.stall {
					ready!(stall.poll(stream.writer.written(), waiter));
					return Poll::Ready(Err(Error::Timeout));
				}
				return Poll::Pending;
			}

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
						stream.writer.buffer(&Msg::RouteEnd { id: wire.id })?;
					}
				}
				self.live.clear();
				self.phase = Phase::Withdrawing;
				continue;
			}

			// The peer sends nothing after its request: closing its side means it wants
			// nothing more.
			match stream.reader.poll_has_more(waiter) {
				Poll::Ready(Ok(true)) => return Poll::Ready(Err(Error::ProtocolViolation)),
				Poll::Ready(Ok(false)) => return Poll::Ready(Ok(())),
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => {}
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

			let count = self.sources.len();
			let ready = (0..count).map(|offset| (self.turn + offset) % count).find_map(|index| {
				match self.sources[index].stream.poll_output(waiter) {
					Poll::Ready(output) => Some((index, output)),
					Poll::Pending => None,
				}
			});
			let Some((index, output)) = ready else {
				return Poll::Pending;
			};
			self.turn = index + 1;
			self.output(stream, index, output)?;
		}
	}

	fn output(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		source: usize,
		output: node::Output,
	) -> Result<(), Error> {
		match output {
			node::Output::Route { id, prefix, reach } => self.route(stream, source, id, prefix, reach),
			node::Output::Batch {
				id,
				prefix,
				changes,
				floor,
			} => self.batch(stream, source, id, prefix, changes, floor),
			node::Output::Dead(dead) => self.dead(stream, source, dead),
			// Only a stream this node asked for serves.
			node::Output::Serve { .. } | node::Output::Unserve { .. } => Ok(()),
		}
	}

	/// The route `source` presents at the origin-side `prefix`, named as the handle names
	/// it, within the requested prefix.
	fn name(&self, source: usize, prefix: &Path) -> Option<Path> {
		let named = match self.sources.get(source)?.mount.as_ref() {
			Some(mount) => mount.name(prefix)?,
			None => prefix.clone(),
		};
		route::within(&named, self.prefix.as_ref()?).then_some(named)
	}

	fn route(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		source: usize,
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
				let sent = self.routes.entry(key.clone()).or_insert_with(|| Sent {
					wire: None,
					shown: 0,
					reach: BTreeMap::new(),
					claims: BTreeMap::new(),
					withheld: 0,
					floor: None,
					floored: false,
				});
				sent.reach.insert(source, Reached { offer, held: false });
			}
			// Held only follows a route the node sent.
			Some(Reach::Held) => match self.routes.get_mut(&key).and_then(|sent| sent.reach.get_mut(&source)) {
				Some(reached) => reached.held = true,
				None => return Ok(()),
			},
			None => return self.unreach(stream, source, &key),
		}
		self.reconcile(stream, &key)
	}

	/// `source` no longer carries the route: the claims it presented end, and the route
	/// with them once no source carries it.
	fn unreach(&mut self, stream: &mut Stream<Transport, Version>, source: usize, key: &RouteKey) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(key) else {
			return Ok(());
		};
		if sent.reach.remove(&source).is_none() {
			return Ok(());
		}
		if sent.reach.is_empty() {
			let sent = self.routes.remove(key).expect("matched above");
			if let Some(wire) = sent.wire {
				// The ROUTE_END retires every claim under the route.
				stream.writer.buffer(&Msg::RouteEnd { id: wire.id })?;
			}
			for path in sent.claims.keys() {
				if let Some((announce, _)) = self.live.remove(&(key.clone(), path.clone())) {
					self.encoder.end(announce);
				}
			}
			return Ok(());
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
			self.sync(stream, key, path)?;
		}
		if self.routes[key].shown == 0 {
			self.floor(stream, key)?;
			self.settle(stream, key)?;
		}
		self.reconcile(stream, key)
	}

	/// Tell the peer what became of the route, if it holds it and that changed.
	fn reconcile(&mut self, stream: &mut Stream<Transport, Version>, key: &RouteKey) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(key) else {
			return Ok(());
		};
		let reached = sent.reach.values().next().expect("a route stands while a source carries it");
		let Some(wire) = sent.wire.as_mut().filter(|wire| wire.told.differs(reached)) else {
			return Ok(());
		};
		match reached.held {
			true => {
				stream.writer.buffer(&Msg::RouteHold { id: wire.id })?;
				wire.told.held = true;
			}
			false => {
				stream.writer.buffer(&Msg::RouteUpdate {
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
	fn dead(&mut self, stream: &mut Stream<Transport, Version>, source: usize, dead: Dead) -> Result<(), Error> {
		let (hop, far, moved, gone) = match dead {
			Dead::Draining { node } => {
				let names = (self.routes.values())
					.filter_map(|sent| sent.wire.as_ref())
					.any(|wire| wire.told.offer.path.iter().any(|hop| hop.node == node));
				if names {
					stream.writer.buffer(&Msg::Dead(lite::Dead::Drain { node }))?;
				}
				return Ok(());
			}
			Dead::Closed { hop, far, moved, gone } => (hop, far, moved, gone),
		};
		// What the node now holds sent: a route through the hop continues on the path
		// it moved to, ends if `gone`, and is held otherwise, unless the peer is the
		// node it was learned from, which is told with a ROUTE_HOLD.
		let peer = self.sources[source].stream.peer();
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
		let single = self.sources.len() == 1;
		let mut told: Vec<RouteKey> = (self.routes.iter())
			.filter(|(_, sent)| sent.wire.as_ref().is_some_and(|wire| wire.told.offer.path.contains(&hop)))
			.map(|(key, _)| key.clone())
			.collect();
		told.sort_unstable();
		if !told.is_empty() {
			let wired = |key: &RouteKey| Some((self.routes[key].wire.as_ref()?.id, Ok(moves.get(key)?.clone())));
			let (moved, ends) = match single {
				true => (told.iter().filter_map(wired).collect(), gone),
				false => (Vec::new(), false),
			};
			stream.writer.buffer(&Msg::Dead(lite::Dead::Closed {
				hop,
				far,
				gone: ends,
				moved,
			}))?;
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
					self.unreach(stream, source, &key)?;
					continue;
				}
				None if kept(&reached.offer.path) => {}
				None => reached.held = true,
			}
			touched.insert(key);
		}
		for key in touched {
			self.reconcile(stream, &key)?;
		}
		Ok(())
	}

	/// The route's Route ID, sending ROUTE_START if the peer does not hold it.
	fn start(&mut self, stream: &mut Stream<Transport, Version>, key: &RouteKey) -> Result<u64, Error> {
		let sent = self.routes.get_mut(key).expect("only a held route starts");
		if let Some(wire) = &sent.wire {
			return Ok(wire.id);
		}
		let reached = sent.reach().expect("a route stands while a source carries it").clone();
		let suffix = (self.prefix.as_ref())
			.and_then(|prefix| key.1.strip_prefix(prefix))
			.expect("a route named within the requested prefix");
		let id = self.next_route;
		self.next_route += 1;
		stream.writer.buffer(&Msg::RouteStart {
			node: key.0,
			suffix,
			offer: reached.offer.clone(),
		})?;
		if reached.held {
			stream.writer.buffer(&Msg::RouteHold { id })?;
		}
		sent.wire = Some(Wire { id, told: reached });
		Ok(id)
	}

	/// End the route on the wire once it shows the peer nothing; the caller sent the
	/// floor certifying that first.
	fn settle(&mut self, stream: &mut Stream<Transport, Version>, key: &RouteKey) -> Result<(), Error> {
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
		stream.writer.buffer(&Msg::RouteEnd { id: wire.id })
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
		prefix: Path,
		changes: Vec<(Path, Option<node::Claim>)>,
		floor: Option<route::Rev>,
	) -> Result<(), Error> {
		let Some(named) = self.name(source, &prefix) else {
			return Ok(());
		};
		let key = (id, named);
		let mount = self.sources.get(source).and_then(|source| source.mount.as_ref());
		let Some(sent) = self.routes.get_mut(&key) else {
			return Ok(());
		};
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
			self.sync(stream, &key, path)?;
		}
		self.floor(stream, &key)?;
		self.settle(stream, &key)
	}

	/// Send the ANNOUNCE_FLOOR closing a batch for the route, if the peer holds it.
	fn floor(&mut self, stream: &mut Stream<Transport, Version>, key: &RouteKey) -> Result<(), Error> {
		let certified = self.certified();
		// Like the grant, the upstream horizon and the proxy preference leave claims out
		// that a floor would certify absent.
		let sent = self.routes.get_mut(key).expect("only a held route has a batch");
		let hides = sent.withheld > 0;
		let Some(wire) = &sent.wire else {
			return Ok(());
		};
		sent.floored = true;
		let revision = sent.floor.filter(|_| certified && !hides);
		stream.writer.buffer(&Msg::Floor { id: wire.id, revision })
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

	/// Bring the peer's claim at `path` for the route in line with the node's.
	fn sync(&mut self, stream: &mut Stream<Transport, Version>, key: &RouteKey, path: Path) -> Result<(), Error> {
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
				let wire = self.start(stream, key)?;
				let suffix = self.suffix(&claim_key.1).expect("shown claims are under the prefix");
				tracing::debug!(route = %claim_key.1, node = key.0, "announce");
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
				self.live.insert(claim_key, (announce, claim));
				self.routes.get_mut(key).expect("started above").shown += 1;
			}
			(Some((announce, held)), Some(claim)) => {
				let announce = *announce;
				let restart = held.instance != claim.instance || held.epoch != claim.epoch;
				let update = held.cost != claim.cost || (self.chains && held.chain != claim.chain);
				if restart {
					tracing::debug!(route = %claim_key.1, "restart");
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
				self.live.insert(claim_key, (announce, claim));
			}
			(Some(_), None) => {
				let (announce, _) = self.live.remove(&claim_key).expect("matched above");
				stream.writer.buffer(&Msg::EndedId { id: announce })?;
				self.encoder.end(announce);
				self.routes.get_mut(key).expect("only a held route syncs").shown -= 1;
			}
			(None, None) => {}
		}
		Ok(())
	}

	/// The grant changed: end what it no longer covers, start what it now does, and
	/// floor every route again, which certifies only while the grant covers the prefix.
	fn refilter(&mut self, stream: &mut Stream<Transport, Version>) -> Result<(), Error> {
		let mut keys: Vec<RouteKey> = self.routes.keys().cloned().collect();
		keys.sort_unstable();
		for key in keys {
			let paths: Vec<Path> = self.routes[&key].claims.keys().cloned().collect();
			for path in paths {
				self.sync(stream, &key, path)?;
			}
			self.floor(stream, &key)?;
			self.settle(stream, &key)?;
		}
		Ok(())
	}
}
