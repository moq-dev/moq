//! Drives a session's SUBSCRIBE_NAMESPACE requests from [`Discovery`].
//!
//! A client asks only about the prefixes its origin's readers watch, as moq-lite does,
//! and ends a request once nothing has watched it for a while.
//!
//! moq-transport refuses a SUBSCRIBE_NAMESPACE that shares a prefix with one still open,
//! so discovery is exclusive: a broader request replaces the narrower ones beneath it,
//! each of which leaves the wire before the broader one goes out. The routes a replaced
//! request carried go with it, and come back once the broader request reports them.

use std::collections::VecDeque;

use super::*;
use crate::discovery::{Change, Discovery};

/// How a SUBSCRIBE_NAMESPACE ended.
#[derive(Debug)]
pub(super) enum Ended {
	/// The peer ended it, or it never went out.
	Peer,
	/// The session ended it.
	Local,
}

/// A request from when discovery opens it until it is off the wire.
struct Request {
	prefix: Path,
	/// Dropped to end it; `None` once the session did.
	live: Option<kio::Producer<()>>,
	/// Closes once it is off the wire, so an overlapping one may go out.
	gone: kio::Consumer<()>,
}

/// Keeps one SUBSCRIBE_NAMESPACE open for each prefix discovery decides on, each on its
/// own stream, for the session's life. Only an error ends it: a request the peer breaks
/// the protocol on.
pub(in crate::ietf) struct Namespaces {
	subscriber: Subscriber,
	discovery: Discovery,
	requests: HashMap<u64, Request>,
	/// The request discovery holds open for each prefix.
	open_at: HashMap<Path, u64>,
	running: kio::Tasks<Exchange>,
	/// Requests that ended, handled one at a time with discovery's changes between them.
	done: VecDeque<(u64, Result<Ended, Error>)>,
	next: u64,
}

impl Namespaces {
	pub(in crate::ietf) fn new(subscriber: Subscriber, discovery: Discovery) -> Self {
		Self {
			subscriber,
			discovery: discovery.exclusive(),
			requests: HashMap::new(),
			open_at: HashMap::new(),
			running: kio::Tasks::new(),
			done: VecDeque::new(),
			next: 0,
		}
	}

	/// Ready with the error that ends them.
	pub(in crate::ietf) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		let discovery = &mut self.discovery;

		loop {
			if let Poll::Ready(change) = discovery.poll(waiter) {
				match change {
					Change::Open(prefix, snapshot) => {
						// Wait out the overlapping requests on their way out, such as the narrower
						// ones just closed for this one.
						let blockers = self
							.requests
							.values()
							.filter(|request| request.live.is_none())
							.filter(|request| request.prefix.has_prefix(&prefix) || prefix.has_prefix(&request.prefix))
							.map(|request| request.gone.clone())
							.collect();
						let live = kio::Producer::new(());
						let gone = kio::Producer::new(());
						let key = self.next;
						self.next += 1;
						self.requests.insert(
							key,
							Request {
								prefix: prefix.clone(),
								live: Some(live.clone()),
								gone: gone.consume(),
							},
						);
						self.open_at.insert(prefix.clone(), key);
						self.running.push(Exchange {
							key,
							gone: Some(gone),
							state: ExchangeState::Opening {
								subscriber: self.subscriber.clone(),
								prefix,
								snapshot,
								blockers,
								open: crate::transport::open::Open::default(),
								live: live.consume(),
							},
						});
					}
					// Dropping its `live` ends the request.
					Change::Close(prefix) => {
						if let Some(key) = self.open_at.remove(&prefix)
							&& let Some(request) = self.requests.get_mut(&key)
						{
							request.live = None;
						}
					}
				}
				continue;
			}

			let Some((key, res)) = self.done.pop_front() else {
				let done = &mut self.done;
				let _ = self
					.running
					.poll_each(waiter, |exchange, res| done.push_back((exchange.key, res)));
				if self.done.is_empty() {
					return Poll::Pending;
				}
				continue;
			};
			let Some(request) = self.requests.remove(&key) else {
				continue;
			};
			// Ended on its own, rather than by the session.
			if self.open_at.get(&request.prefix) == Some(&key) {
				self.open_at.remove(&request.prefix);
				discovery.ended(&request.prefix);
			}
			if let Err(err) = res {
				// The peer breaking the protocol is fatal, and the driver turns
				// this into the session close the draft wants.
				if is_protocol_violation(&err) {
					return Poll::Ready(err);
				}
				tracing::warn!(%err, prefix = %request.prefix, "subscribe_namespace failed, continuing without");
			}
		}
	}
}

/// One request: sent once the ones it overlaps are off the wire, and read until either
/// side ends it.
struct Exchange {
	key: u64,
	/// Dropped once it is off the wire, so an overlapping request may go out.
	gone: Option<kio::Producer<()>>,
	state: ExchangeState,
}

enum ExchangeState {
	/// Waiting out the requests it overlaps, then for stream credit.
	Opening {
		subscriber: Subscriber,
		prefix: Path,
		snapshot: crate::model::Snapshot,
		/// Close once each request it overlaps is off the wire.
		blockers: Vec<kio::Consumer<()>>,
		open: crate::transport::open::Open,
		/// Closes when the session ends it.
		live: kio::Consumer<()>,
	},
	Running(NamespaceSubscription),
	Done,
}

impl kio::Task for Exchange {
	type Output = Result<Ended, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		let res = loop {
			match &mut self.state {
				ExchangeState::Opening {
					subscriber,
					blockers,
					open,
					live,
					..
				} => {
					// The open races `live` too: a request ended before it went out holds nothing, nor
					// the broader one it blocks. Without stream credit it is refused, not parked.
					if live.poll_closed(waiter).is_ready() {
						break Ok(Ended::Local);
					}
					for blocker in blockers.iter() {
						if blocker.poll_closed(waiter).is_pending() {
							return Poll::Pending;
						}
					}
					let stream = match ready!(Stream::poll_open(&subscriber.session, open, subscriber.version, waiter))
					{
						Ok(stream) => stream,
						Err(err) => break Err(err),
					};
					let ExchangeState::Opening {
						subscriber,
						prefix,
						snapshot,
						live,
						..
					} = std::mem::replace(&mut self.state, ExchangeState::Done)
					else {
						unreachable!("matched above");
					};
					self.state =
						ExchangeState::Running(NamespaceSubscription::new(subscriber, stream, prefix, snapshot, live));
				}
				ExchangeState::Running(request) => break ready!(request.poll(waiter)),
				ExchangeState::Done => panic!("Exchange polled after it finished"),
			}
		};
		self.state = ExchangeState::Done;
		// Off the wire: an overlapping request may go out.
		self.gone = None;
		Poll::Ready(res)
	}
}
