//! Lite-07: the session's two Announce Streams, one each way.
//!
//! Each endpoint says everything about routing on one unidirectional stream: as a
//! subscriber, the edits of the prefixes it asks for, and as a publisher, the routes
//! and claims answering what its peer asks for. One stream each way keeps a side's
//! messages in order, so a route's claims never overtake the route.
//!
//! The session is one neighbour of the origin's route node for both directions. When
//! the session publishes from one origin and subscribes into another, each origin's
//! node gets a key of its own, and the two halves only share the streams.

use std::{
	sync::Arc,
	task::{Poll, ready},
};

use crate::{
	Error, Path, SessionError, coding,
	driver::Transport,
	lite::{self, AnnounceBroadcast as Msg, Version},
	origin,
	route::{node, relay},
	stats::RouteClose,
	transport::poll::Session as _,
};

use super::{Subscriber, routed::Routed};
use crate::lite::publisher::routed::Answer;

/// The stream this session's messages go out on.
pub(in crate::lite) type Writer = coding::Writer<crate::transport::boxed::SendStream, Version>;

/// The stream the peer's messages arrive on.
pub(in crate::lite) type Reader = coding::Reader<crate::transport::boxed::RecvStream, Version>;

/// What the session answers its peer from.
pub(in crate::lite) struct Publish {
	pub origin: origin::Consumer,
	pub auth: crate::auth::Link,
	pub withdrawal: crate::session::Withdrawal,
	/// Whether this side's SETUP declared a Node ID: it forwards, and trusts the peer
	/// with the whole tree.
	pub routing: bool,
}

/// How long the peer may leave this side's stream unread before the session closes.
const STALL: std::time::Duration = std::time::Duration::from_secs(10);

/// Closes the session whose peer stopped reading the stream.
struct Stall {
	session: Transport,
	runtime: crate::time::Deadlines,
	/// When the stream counts as stalled, and how much it had written when that was set.
	armed: Option<(crate::time::Deadline, u64)>,
}

impl Stall {
	/// Ready once the writer made no progress for [`STALL`].
	fn poll(&mut self, written: u64, waiter: &kio::Waiter) -> Poll<()> {
		if self.armed.as_ref().is_none_or(|(_, at)| *at != written) {
			let deadline = crate::time::Deadline::after(&self.runtime, STALL);
			self.armed = Some((deadline, written));
		}
		let (deadline, _) = self.armed.as_mut().expect("armed above");
		ready!(deadline.poll(waiter));
		self.close("announce stream stalled");
		Poll::Ready(())
	}

	fn close(&self, reason: &str) {
		self.session
			.close(lite::error::to_session_code(&SessionError::Timeout), reason);
	}
}

/// Where this side's stream stands.
enum Sending {
	Open,
	/// The session is leaving: the stream finishes once what it holds is flushed, which
	/// ends everything the peer was sent.
	Ending,
	/// Finished: waiting for the peer to read it.
	Closing,
	Closed,
}

/// The route layer of a running session.
struct Running {
	writer: Writer,
	sending: Sending,
	/// The peer's stream, once it opened one.
	reader: Option<Reader>,
	/// The peer finished its stream.
	finished: bool,
	/// The session's keys on the route nodes: the subscribe origin's, then the publish
	/// origin's when that is another node.
	streams: Vec<Arc<node::Stream>>,
	routed: Routed,
	answer: Answer,
	/// The subscribe permit's epoch last applied.
	epoch: u64,
	/// What the link is charged now, and the price's epoch last applied.
	link_cost: u64,
	price_epoch: u64,
	stall: Stall,
	/// Passes before the machine yields, so a busy node cannot hold the task.
	budget: kio::coop::Budget,
}

enum State {
	/// Opening this side's stream.
	Opening,
	/// Waiting for what the route layer files the session under: the grant, and the
	/// peer's SETUP.
	Setup(Box<Writer>),
	Running(Box<Running>),
}

/// Runs a lite-07 session's route layer on its two Announce Streams.
pub(super) struct RouteStream {
	subscriber: Subscriber,
	publish: Publish,
	/// Held until this side's routes are ended, so a draining session waits for that.
	withdrawing: Option<crate::session::Withdrawing>,
	open: crate::transport::open::Open,
	/// What the origin wanted before the route layer started.
	wants: Vec<(Path, crate::model::Snapshot)>,
	state: State,
}

impl RouteStream {
	pub(super) fn new(subscriber: Subscriber, publish: Publish) -> Self {
		Self {
			withdrawing: Some(publish.withdrawal.register()),
			subscriber,
			publish,
			open: Default::default(),
			wants: Vec::new(),
			state: State::Opening,
		}
	}

	/// The origin wants `prefix` of the peer, holding `snapshot` until its initial set is in.
	pub(super) fn want(&mut self, prefix: Path, snapshot: crate::model::Snapshot) {
		match &mut self.state {
			State::Running(running) => running.routed.want(prefix, snapshot),
			_ => self.wants.push((prefix, snapshot)),
		}
	}

	/// The origin no longer wants `prefix` of the peer.
	pub(super) fn unwant(&mut self, prefix: &Path) {
		match &mut self.state {
			State::Running(running) => running.routed.unwant(prefix),
			_ => self.wants.retain(|(want, _)| want != prefix),
		}
	}

	/// Ready only with the error that ends the session.
	pub(super) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		let res = self.poll_run(waiter);
		if self.withdrawing.is_some() && !self.owes(waiter) {
			self.withdrawing = None;
		}
		match res {
			Poll::Ready(Err(err)) => Poll::Ready(lite::error::control(&mut self.subscriber.session.clone(), err)),
			Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
		}
	}

	/// Whether a draining close still waits on this stream: until its FIN is acknowledged,
	/// unless the peer was never sent a route or claim for the FIN to end.
	fn owes(&self, waiter: &kio::Waiter) -> bool {
		match &self.state {
			State::Running(running) => match running.sending {
				Sending::Open => true,
				Sending::Ending | Sending::Closing => running.answer.announced(),
				Sending::Closed => false,
			},
			// Nothing is announced before the route layer runs, and a leaving session never
			// starts it announcing.
			State::Opening | State::Setup(_) => self.publish.withdrawal.poll(waiter).is_pending(),
		}
	}

	pub(super) fn poll_run(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				State::Opening => {
					let session = &self.subscriber.session;
					let stream = ready!(session.poll_open_uni(&mut self.open, waiter, Default::default()))
						.map_err(Error::from_transport)?;
					let mut writer: Writer =
						Writer::new(stream, self.subscriber.version).with_stats(self.subscriber.origin.stats());
					writer.buffer(&lite::DataType::Announce)?;
					self.state = State::Setup(Box::new(writer));
				}
				State::Setup(_) => {
					let running = ready!(self.poll_setup(waiter))?;
					self.state = State::Running(Box::new(running));
				}
				State::Running(running) => {
					return running.poll(&self.subscriber, &self.publish, waiter);
				}
			}
		}
	}

	/// File the session with the route nodes, once the grant and the peer's SETUP say how.
	fn poll_setup(&mut self, waiter: &kio::Waiter) -> Poll<Result<Running, Error>> {
		let subscriber = &self.subscriber;
		// Start from the grant the setup token earns, so the initial set never
		// advertises something it would end a moment later.
		ready!(self.publish.auth.poll_setup_answered(waiter));
		let link_cost = ready!(subscriber.poll_link_cost(waiter));
		// Two endpoints that both declared a Node ID exchange routes.
		let peer = ready!(subscriber.peer_setup.poll_node(waiter));
		let permit = subscriber.auth.permit(crate::auth::Direction::Subscribe);

		let answering = self.publish.origin.route_node();
		let shared = answering.same(&subscriber.origin.route_node());
		let routing = peer.filter(|_| self.publish.routing && shared);
		let mut routed = Routed::new(
			subscriber,
			link_cost,
			&permit,
			shared.then_some(&self.publish.origin),
			routing,
		)?;
		let mut streams = vec![routed.stream()];
		if !shared {
			let port = relay::Port {
				// The peer prices the link on what it receives; nothing here charges it.
				cost: 1,
				priority: 0,
				upstream: self.publish.origin.upstream() || !self.publish.origin.proxy(),
				// This origin asks the peer for nothing.
				role: relay::Role::Scoped(Vec::new()),
			};
			let stats = self.publish.origin.stats();
			let stream = (answering.connect(subscriber.peer_setup.tag(), port))
				.map_err(|err| {
					stats.route_closed(err.close());
					Error::ProtocolViolation
				})?
				.with_clock(subscriber.runtime.clone())
				.with_stats(stats.clone());
			streams.push(Arc::new(stream));
		}
		let main = streams.last().expect("the session's key").clone();
		let answer = Answer::new(
			self.publish.origin.clone(),
			self.publish.auth.clone(),
			main,
			routing.is_some(),
		);
		// A routing peer is sent everything and sends anything, so neither grant may
		// leave part of the tree out.
		if routing.is_some() && !(answer.whole() && super::routed::whole(&permit)) {
			return Poll::Ready(Err(Error::Unauthorized));
		}
		for (prefix, snapshot) in self.wants.drain(..) {
			routed.want(prefix, snapshot);
		}
		let State::Setup(writer) = std::mem::replace(&mut self.state, State::Opening) else {
			unreachable!("polled in setup");
		};
		Poll::Ready(Ok(Running {
			writer: *writer,
			sending: Sending::Open,
			reader: None,
			finished: false,
			streams,
			routed,
			answer,
			epoch: 0,
			link_cost,
			price_epoch: 0,
			stall: Stall {
				session: subscriber.session.clone(),
				runtime: subscriber.runtime.clone(),
				armed: None,
			},
			budget: kio::coop::Budget::new(32),
		}))
	}
}

impl Running {
	fn poll(&mut self, subscriber: &Subscriber, publish: &Publish, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// A draining peer usually stops announcing, so react to the GOAWAY itself.
		// Idempotent, since the signal stays set.
		if subscriber.going_away.poll(waiter).is_ready() {
			self.routed.goaway();
		}
		// A repriced link re-ranks every route and claim the peer gave, in place.
		while subscriber.price.poll_changed(&mut self.price_epoch, waiter).is_ready() {}
		if let Poll::Ready(link_cost) = subscriber.poll_link_cost(waiter)
			&& link_cost != self.link_cost
		{
			self.routed.reprice(link_cost);
			self.link_cost = link_cost;
		}
		let direction = crate::auth::Direction::Subscribe;
		while let Poll::Ready(permit) = subscriber.auth.poll_permit(direction, &mut self.epoch, waiter) {
			if let Err(err) = self.routed.limit(&permit) {
				// A narrowed grant is not the route layer's; only what the node refused is.
				if let Some(kind) = self.routed.cause() {
					subscriber.origin.stats().route_closed(kind);
				}
				return Poll::Ready(Err(err));
			}
		}

		// What the node queued goes out before the peer's next messages are read, and
		// what those queue is served before the machine rests. The peer's stream is
		// read whether or not it reads this side's, so two peers that both fell behind
		// never wait on each other.
		loop {
			if let Poll::Ready(Err(err)) = self.poll_send(subscriber, publish, waiter) {
				return Poll::Ready(Err(err));
			}
			if !self.poll_receive(subscriber, waiter)? {
				break;
			}
		}
		// Serve the claims whose request queue woke. Each parks on its own waker, so
		// the rest cost nothing.
		self.routed.poll(subscriber, waiter);
		Poll::Pending
	}

	/// Send what the node queued for the peer, and serve what it queued for the origin.
	/// Ready only with an error.
	fn poll_send(
		&mut self,
		subscriber: &Subscriber,
		publish: &Publish,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		loop {
			ready!(self.budget.poll_yield(waiter));
			// What the node serves through the session goes into the origin whether or not
			// the peer reads this side's stream.
			for stream in &self.streams {
				while let Poll::Ready(output) = stream.poll_serve(waiter) {
					self.routed.output(subscriber, output);
				}
			}
			match self.sending {
				Sending::Open | Sending::Ending => {
					// Deliver what is buffered before taking more work.
					match self.writer.poll_flush(waiter) {
						Poll::Ready(flushed) => {
							flushed?;
							self.stall.armed = None;
						}
						Poll::Pending => {
							// A reader that keeps reading a trickle while its routes fail faster
							// is cut off too, before the node holds every failure for it.
							if self.answer.poll_overrun(waiter).is_ready() {
								subscriber.origin.stats().route_closed(RouteClose::Overrun);
								self.stall.close("announce stream fell behind");
								return Poll::Ready(Err(Error::Timeout));
							}
							if self.stall.poll(self.writer.written(), waiter).is_ready() {
								subscriber.origin.stats().route_closed(RouteClose::Stalled);
								return Poll::Ready(Err(Error::Timeout));
							}
							break;
						}
					}
					if matches!(self.sending, Sending::Ending) {
						self.writer.finish()?;
						self.sending = Sending::Closing;
						continue;
					}
					if publish.withdrawal.poll(waiter).is_ready() {
						// The session is leaving: finishing the stream ends what the peer was sent.
						self.answer.withdraw();
						self.routed.silence();
						self.sending = Sending::Ending;
						continue;
					}
					// A grant change applies before the next batch, so a claim it no longer
					// covers is ended rather than sent.
					self.answer.poll_permit(waiter)?;
				}
				Sending::Closing => {
					if let Poll::Ready(closed) = self.writer.poll_closed(waiter) {
						closed?;
						self.sending = Sending::Closed;
					}
				}
				Sending::Closed => {}
			}

			// The session's key waits while a grant change's mounts send their initial sets,
			// which the peer's next ANNOUNCE_OK follows.
			let holding = self.answer.holding();
			let main = self.streams.len() - 1;
			let streams = self.streams.iter().enumerate().filter(|(i, _)| !holding || *i != main);
			let ready = streams
				.map(|(_, stream)| stream)
				.find_map(|stream| match stream.poll_wire(waiter) {
					Poll::Ready(msg) => Some(msg),
					Poll::Pending => None,
				});
			if let Some(msg) = ready {
				match msg {
					node::Msg::Ask(edit) => {
						for request in self.routed.ask(edit) {
							self.writer.buffer(&request)?;
						}
					}
					msg => self.answer.output(&mut self.writer, msg)?,
				}
				continue;
			}
			match self.answer.poll_mounts(&mut self.writer, waiter) {
				Poll::Ready(res) => res?,
				Poll::Pending => {
					// A grant change can buffer claims with nothing queued behind them, and the
					// wakeup that brought it is spent: flush before resting.
					if matches!(self.sending, Sending::Open | Sending::Ending) && !self.writer.is_flushed() {
						continue;
					}
					// Everything the node queued is served, so what the peer synced shows.
					self.routed.release();
					break;
				}
			}
		}
		Poll::Pending
	}

	/// Read the peer's stream: its requests go to the half answering them, everything
	/// else to the half that asked. Returns whether it read anything.
	fn poll_receive(&mut self, subscriber: &Subscriber, waiter: &kio::Waiter) -> Result<bool, Error> {
		while let Poll::Ready(Ok(reader)) = subscriber.announce_streams.poll_pop(waiter) {
			// The peer opens one stream for the session.
			if self.reader.replace(reader).is_some() {
				subscriber.origin.stats().route_closed(RouteClose::Malformed);
				return Err(Error::ProtocolViolation);
			}
		}
		if self.reader.is_none() || self.finished {
			return Ok(false);
		}
		// Each message goes into the node, so a peer streaming them must not hold the
		// session's other tasks.
		let mut budget = kio::coop::Budget::new(32);
		let read = self.read(subscriber, &mut budget, waiter);
		// One pass of messages reaches the node's other sessions as one change.
		for stream in &self.streams {
			stream.flush();
		}
		read
	}

	/// Read what the peer's stream holds, until it runs dry or `budget` does.
	fn read(
		&mut self,
		subscriber: &Subscriber,
		budget: &mut kio::coop::Budget,
		waiter: &kio::Waiter,
	) -> Result<bool, Error> {
		let reader = self.reader.as_mut().expect("checked by the caller");
		let mut read = false;
		loop {
			// Out of budget: the yield wakes the machine, which sends before it reads on.
			if budget.poll_yield(waiter).is_pending() {
				return Ok(false);
			}
			// Whatever the peer sends that closes the session counts as a route-layer close.
			let stats = subscriber.origin.stats();
			match reader.poll_decode_maybe::<Msg>(waiter) {
				Poll::Ready(Ok(Some(Msg::Request { more, remove, add }))) => {
					if let Err(err) = self.answer.request(more, remove, add) {
						stats.route_closed(self.answer.closed());
						return Err(err);
					}
				}
				// Nothing comes between the ANNOUNCE_REQUESTs of one edit.
				Poll::Ready(Ok(Some(_))) if self.answer.continuing() => {
					stats.route_closed(RouteClose::Malformed);
					return Err(Error::ProtocolViolation);
				}
				Poll::Ready(Ok(Some(announce))) => {
					if let Err(err) = self.routed.receive(announce) {
						stats.route_closed(self.routed.closed());
						return Err(err);
					}
				}
				// The peer is leaving: what it sent ended, and it asks for nothing more.
				Poll::Ready(Ok(None)) => {
					self.finished = true;
					self.routed.finish();
					return Ok(true);
				}
				// The peer may only finish the stream: while the session lives, whatever else
				// broke it lost what the peer said. An ending session's own end says why, which a
				// read can report before the session's close is seen: `transport_error` makes an
				// `Error::Session` only of the transport's session close, never of a stream reset.
				Poll::Ready(Err(err)) => {
					if matches!(err, Error::Session(_)) || subscriber.session.poll_ending(waiter).is_ready() {
						return Err(err);
					}
					tracing::debug!(%err, "the peer's Announce Stream broke");
					stats.route_closed(RouteClose::Malformed);
					return Err(Error::ProtocolViolation);
				}
				Poll::Pending => return Ok(read),
			}
			read = true;
		}
	}
}

#[cfg(test)]
impl Publish {
	/// A session with nothing of its own to answer the peer from.
	pub(in crate::lite) fn idle() -> Self {
		use crate::model::ProduceTest as _;
		Self {
			origin: crate::origin::Config::new(std::num::NonZeroU64::new(0x7e57).unwrap())
				.produce()
				.consume(),
			auth: crate::auth::Link::new(false),
			withdrawal: Default::default(),
			routing: false,
		}
	}
}
