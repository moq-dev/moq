//! Lite-07: an announce stream the peer asked for, answered from the origin's route node.
//!
//! The node sends the peer its routes and, closed by an ANNOUNCE_FLOOR, each route's
//! claims under the requested prefix. The stream filters what it puts on the wire:
//! hidden claims unless the peer asked for them, and anything this handle's scope or
//! the session's grant leaves out, in which case a floor certifies nothing, since the
//! peer cannot tell what was left out from what is gone.

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
	/// Every claim the node sent for it, before filtering.
	claims: BTreeMap<Path, node::Claim>,
	/// The node's floor for those claims.
	floor: Option<route::Rev>,
	/// Whether the peer was sent a floor for it yet.
	floored: bool,
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

/// Answers one lite-07 announce stream.
pub(super) struct Answer {
	origin: origin::Consumer,
	/// The requested prefix, absolute.
	prefix: Path,
	hidden: bool,
	link: route::Link,
	stream: Option<node::Stream>,
	encoder: lite::AnnounceEncoder,
	routes: HashMap<route::Id, Sent>,
	next_route: u64,
	/// Route IDs ended and not yet echoed, and the origin node each reached.
	echoes: HashMap<u64, route::Id>,
	/// The claims the peer holds: their Announce ID and attributes.
	live: HashMap<(route::Id, Path), (u64, node::Claim)>,
	gate: crate::auth::Gate,
	epoch: u64,
	permit: crate::auth::Permit,
	phase: Phase,
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
		let absolute = origin.root().join(&prefix).unwrap_or_else(|_| origin.root().clone());
		Self {
			gate: crate::auth::Gate::new(auth, prefix, crate::auth::Direction::Publish),
			prefix: absolute,
			origin,
			hidden,
			link,
			stream: None,
			encoder: lite::AnnounceEncoder::new(Version::Lite07),
			routes: HashMap::new(),
			next_route: 0,
			echoes: HashMap::new(),
			live: HashMap::new(),
			epoch: 0,
			permit: Default::default(),
			phase: Phase::Init,
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
			let answer = node
				.answer(self.link.clone(), self.prefix.clone())
				.map_err(|_| Error::ProtocolViolation)?;
			while let Some(output) = answer.try_output() {
				self.output(stream, output)?;
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
			self.stream = Some(answer);
			self.phase = Phase::Running;
		}

		loop {
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
				self.stream = None;
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

			match stream.reader.poll_decode_maybe::<lite::AnnounceReply>(waiter) {
				Poll::Ready(Ok(Some(reply))) => {
					self.reply(reply)?;
					continue;
				}
				// The peer closed its side: it wants nothing more.
				Poll::Ready(Ok(None)) => return Poll::Ready(Ok(())),
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => {}
			}

			let answer = self.stream.as_ref().expect("running streams hold the node");
			if let Poll::Ready(output) = answer.poll_output(waiter) {
				self.output(stream, output)?;
				continue;
			}
			return Poll::Pending;
		}
	}

	/// A ROUTE_REQUEST, or the echo of a ROUTE_END.
	fn reply(&mut self, reply: lite::AnnounceReply) -> Result<(), Error> {
		let answer = self.stream.as_ref().expect("running streams hold the node");
		match reply {
			lite::AnnounceReply::RouteRequest { node, seqno } => answer.request(node, seqno),
			lite::AnnounceReply::RouteEnd { id } => {
				// The echo names the Route ID; the node knows the route by its origin node.
				let node = self.echoes.remove(&id).ok_or(Error::ProtocolViolation)?;
				answer.echo(node);
			}
		}
		Ok(())
	}

	fn output(&mut self, stream: &mut Stream<Transport, Version>, output: node::Output) -> Result<(), Error> {
		match output {
			node::Output::Route { id, reach } => self.route(stream, id, reach),
			node::Output::Batch { id, changes, floor } => self.batch(stream, id, changes, floor),
			// Only a stream this node asked for requests, serves, or echoes.
			node::Output::Request { .. }
			| node::Output::Echo { .. }
			| node::Output::Serve { .. }
			| node::Output::Unserve { .. } => Ok(()),
		}
	}

	fn route(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		id: route::Id,
		reach: Option<route::Reach>,
	) -> Result<(), Error> {
		match (reach, self.routes.get(&id)) {
			(Some(route::Reach::Finite(advert)), Some(sent)) => {
				stream.writer.buffer(&Msg::RouteUpdate { id: sent.wire, advert })?;
			}
			(Some(route::Reach::Finite(advert)), None) => {
				let wire = self.next_route;
				self.next_route += 1;
				stream.writer.buffer(&Msg::RouteStart { node: id, advert })?;
				self.routes.insert(
					id,
					Sent {
						wire,
						claims: BTreeMap::new(),
						floor: None,
						floored: false,
					},
				);
			}
			(Some(route::Reach::Held), Some(sent)) => stream.writer.buffer(&Msg::RouteHold { id: sent.wire })?,
			(None, Some(_)) => {
				let sent = self.routes.remove(&id).expect("matched above");
				stream.writer.buffer(&Msg::RouteEnd { id: sent.wire })?;
				// The ROUTE_END retires every claim under the route.
				for path in sent.claims.keys() {
					if let Some((announce, _)) = self.live.remove(&(id, path.clone())) {
						self.encoder.end(announce);
					}
				}
				self.echoes.insert(sent.wire, id);
			}
			// Held only follows a route the peer holds, and an end of one it does not is moot.
			(Some(route::Reach::Held) | None, None) => {}
		}
		Ok(())
	}

	fn batch(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		id: route::Id,
		changes: Vec<(Path, Option<node::Claim>)>,
		floor: Option<route::Rev>,
	) -> Result<(), Error> {
		let Some(sent) = self.routes.get_mut(&id) else {
			return Ok(());
		};
		let mut paths = Vec::with_capacity(changes.len());
		for (path, claim) in changes {
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
		let sent = self.routes.get_mut(&id).expect("only a held route has a batch");
		sent.floored = true;
		let revision = sent.floor.filter(|_| certified);
		stream.writer.buffer(&Msg::Floor {
			id: sent.wire,
			revision,
		})
	}

	/// Whether a floor may certify the whole requested prefix: nothing this handle's
	/// scope or the session's grant leaves out lies beneath it. A hidden claim left out
	/// does not count, since the peer did not ask to see it.
	fn certified(&self) -> bool {
		let relative = self.prefix.strip_prefix(self.origin.root()).unwrap_or_default();
		self.origin.covers(&self.prefix) && self.permit.covers_subtree(relative.as_str())
	}

	/// Whether the peer is shown the claim at the absolute `path`.
	fn shows(&self, path: &Path) -> bool {
		let Some(suffix) = path.strip_prefix(&self.prefix) else {
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
		let wanted = sent.claims.get(&path).filter(|_| self.shows(&path)).cloned();
		let wire = sent.wire;
		let key = (id, path);
		match (self.live.get_mut(&key), wanted) {
			(None, Some(claim)) => {
				let suffix = key
					.1
					.strip_prefix(&self.prefix)
					.expect("shown claims are under the prefix");
				let (announce, suffix) = self.encoder.start(suffix);
				let announce = announce.expect("lite-07 assigns announce ids");
				stream.writer.buffer(&Msg::Active {
					suffix,
					route: wire,
					instance: Some(claim.instance.clone()),
					epoch: claim.epoch,
					hops: crate::Hops::new(),
					cost: origin::Cost::new(claim.cost),
				})?;
				self.live.insert(key, (announce, claim));
			}
			(Some((announce, held)), Some(claim)) => {
				if held.instance != claim.instance || held.epoch != claim.epoch {
					stream.writer.buffer(&Msg::Restart {
						id: *announce,
						instance: Some(claim.instance.clone()),
						epoch: claim.epoch,
						cost: origin::Cost::new(claim.cost),
					})?;
				} else if held.cost != claim.cost {
					stream.writer.buffer(&Msg::Update {
						id: *announce,
						hops: crate::Hops::new(),
						cost: origin::Cost::new(claim.cost),
					})?;
				}
				*held = claim;
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
