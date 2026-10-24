//! Drives a session's SUBSCRIBE_NAMESPACE requests from [`Discovery`].
//!
//! A client asks only about the prefixes its origin's readers watch, as moq-lite does,
//! and ends a request once nothing has watched it for a while.
//!
//! moq-transport refuses a SUBSCRIBE_NAMESPACE that shares a prefix with one still open.
//! A peer that declared MoQ Solicit is ours and holds both, so it gets moq-lite's
//! overlapping requests, which never take each other's routes over. Any other peer gets
//! exclusive discovery: a broader request replaces the narrower ones beneath it, each of
//! which leaves the wire before the broader one goes out. The routes a replaced request
//! carried go with it, and come back once the broader request reports them.

use std::future::Future;

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
	prefix: PathOwned,
	/// Dropped to end it; `None` once the session did.
	live: Option<kio::Producer<()>>,
	/// Closes once it is off the wire, so an overlapping one may go out.
	gone: kio::Consumer<()>,
}

/// A request waiting to go out.
struct Pending<O> {
	/// Close once each request it overlaps is off the wire.
	blockers: Vec<kio::Consumer<()>>,
	/// Opens its stream.
	opening: O,
	/// Closes when the session ends it.
	live: kio::Consumer<()>,
	gone: kio::Producer<()>,
}

impl Subscriber {
	/// Keep one SUBSCRIBE_NAMESPACE open for each prefix `discovery` decides on, each on a
	/// stream from `open`, for the session's life. Only an error ends it: a request the
	/// peer breaks the protocol on.
	pub(in crate::ietf) async fn run_namespaces<F, O>(self, discovery: Discovery, open: F) -> Result<(), Error>
	where
		F: Fn() -> O,
		O: Future<Output = Result<Stream<Transport, Version>, Error>>,
	{
		let mut discovery = match self.peer_setup.get().await.solicit {
			Some(_) => discovery,
			None => discovery.exclusive(),
		};
		let mut requests: HashMap<u64, Request> = HashMap::new();
		// The request discovery holds open for each prefix.
		let mut open_at: HashMap<PathOwned, u64> = HashMap::new();
		let mut running = futures::stream::FuturesUnordered::new();
		let mut next = 0u64;

		loop {
			enum Event<T> {
				Change(Change),
				Done(T),
			}
			let event = kio::wait(|waiter| {
				if let Poll::Ready(change) = discovery.poll(waiter) {
					return Poll::Ready(Event::Change(change));
				}
				let mut cx = waiter.context();
				match futures::Stream::poll_next(std::pin::Pin::new(&mut running), &mut cx) {
					Poll::Ready(Some(done)) => Poll::Ready(Event::Done(done)),
					_ => Poll::Pending,
				}
			})
			.await;

			match event {
				Event::Change(Change::Open(prefix, snapshot)) => {
					// Wait out the overlapping requests on their way out, such as the narrower
					// ones an exclusive session just closed for this one. Those still open
					// overlap it only where the peer holds both.
					let blockers = requests
						.values()
						.filter(|request| request.live.is_none())
						.filter(|request| request.prefix.has_prefix(&prefix) || prefix.has_prefix(&request.prefix))
						.map(|request| request.gone.clone())
						.collect();
					let live = kio::Producer::new(());
					let gone = kio::Producer::new(());
					let key = next;
					next += 1;
					requests.insert(
						key,
						Request {
							prefix: prefix.clone(),
							live: Some(live.clone()),
							gone: gone.consume(),
						},
					);
					open_at.insert(prefix.clone(), key);
					let pending = Pending {
						blockers,
						opening: open(),
						live: live.consume(),
						gone,
					};
					running.push(self.clone().namespace_request(key, prefix, snapshot, pending));
				}
				// Dropping its `live` ends the request.
				Event::Change(Change::Close(prefix)) => {
					if let Some(key) = open_at.remove(&prefix)
						&& let Some(request) = requests.get_mut(&key)
					{
						request.live = None;
					}
				}
				Event::Done((key, res)) => {
					let Some(request) = requests.remove(&key) else {
						continue;
					};
					// Ended on its own, rather than by the session.
					if open_at.get(&request.prefix) == Some(&key) {
						open_at.remove(&request.prefix);
						discovery.ended(&request.prefix);
					}
					if let Err(err) = res {
						// The peer breaking the protocol is fatal, and the driver turns
						// this into the session close the draft wants.
						if is_protocol_violation(&err) {
							return Err(err);
						}
						tracing::warn!(%err, prefix = %request.prefix, "subscribe_namespace failed, continuing without");
					}
				}
			}
		}
	}

	/// Send one request once the ones it overlaps are off the wire, and read it until
	/// either side ends it.
	async fn namespace_request<O>(
		mut self,
		key: u64,
		prefix: PathOwned,
		snapshot: crate::model::Snapshot,
		pending: Pending<O>,
	) -> (u64, Result<Ended, Error>)
	where
		O: Future<Output = Result<Stream<Transport, Version>, Error>>,
	{
		let Pending {
			blockers,
			opening,
			live,
			gone,
		} = pending;
		// The open waits on stream credit the peer grants, so it races `live` too: a request
		// ended before it went out holds nothing, nor the broader one it blocks.
		let mut opening = std::pin::pin!(opening);
		let opened = kio::wait(|waiter| {
			if live.poll_closed(waiter).is_ready() {
				return Poll::Ready(None);
			}
			for blocker in &blockers {
				if blocker.poll_closed(waiter).is_pending() {
					return Poll::Pending;
				}
			}
			waiter.poll_future(opening.as_mut()).map(Some)
		})
		.await;
		let res = match opened {
			Some(Ok(stream)) => self.run_subscribe_namespace(stream, prefix, snapshot, live).await,
			Some(Err(err)) => Err(err),
			None => Ok(Ended::Local),
		};
		// Off the wire: an overlapping request may go out.
		drop(gone);
		(key, res)
	}
}
