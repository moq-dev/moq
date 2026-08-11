use crate::{frame, group, origin, track};
use std::{
	collections::HashMap,
	sync::{Arc, atomic},
	task::{Poll, ready},
	time::Duration,
};

use crate::poll_set::{Machine, PollSet};

use crate::{
	AsPath, Error, Path, PathOwned, Timescale, Timestamp, bandwidth,
	coding::{Decode, Reader, Stream},
	lite,
	track::{Position, Subscription},
};

use super::{ConnectingProducer, RouteCost, Version};

use web_async::Lock;

pub(super) struct SubscriberConfig<S: crate::transport::poll::Session> {
	pub session: S,
	/// The origin into which remote broadcasts are inserted. Traffic stats are
	/// attributed through this handle: tag it with [`origin::Producer::with_stats`]
	/// first.
	pub origin: origin::Producer,
	/// Receiver-side bandwidth producer for PROBE feedback. None disables the
	/// feature (used by versions that don't carry probe streams).
	pub recv_bandwidth: Option<bandwidth::Producer>,
	pub version: Version,
	/// Shared slot for the peer's SETUP (lite-05+). Written when the peer's Setup
	/// stream is read; the probe stream waits on it before opening.
	pub peer_setup: super::PeerSetup,
	/// The origin (hop) id assigned to the peer, used whenever the peer doesn't
	/// declare one itself. See `Client::with_peer_origin`.
	pub peer_origin: Option<crate::Origin>,
	/// Local policy for what pulling from this peer costs, overriding whatever it
	/// declared in its SETUP. `None` charges the peer's declared price.
	pub cost: Option<u64>,
	/// Set once the peer sends a GOAWAY; new request streams are then rejected
	/// with [`Error::GoingAway`] (the peer told us to stop asking).
	pub going_away: crate::goaway::GoingAway,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: crate::transport::poll::Session> {
	session: S,

	origin: origin::Producer,
	recv_bandwidth: Option<bandwidth::Producer>,
	// Session-level origin id shared with the Publisher. Used to drop reflected
	// announces: any incoming announce whose hop chain already passed through us
	// has looped, so it is neither used as a route nor forwarded. On lite-04/05
	// we also ask the peer to filter them out (AnnounceRequest.exclude_hop) so
	// they never hit the wire, but this check is what makes it correct.
	self_origin: crate::Origin,
	// The origin stamped into the hop chain of broadcasts from versions that
	// don't carry real hop ids on the wire (Lite01/02/03), and into the
	// placeholder entries Lite03 sends in place of real ids.
	//
	// This is the peer's assigned identity (`peer_origin`) when the caller gave
	// it one, which also makes the route recognizable across sessions. Otherwise
	// it is `Origin::UNKNOWN` (0), the reserved "no identity" value: a random id
	// would look like an identity the peer never agreed to and cannot exclude
	// for loop detection.
	session_origin: crate::Origin,
	// The identity assigned to the peer by `Client::with_peer_origin`, standing
	// in wherever the peer declines to declare one (an AnnounceOk reporting
	// origin id 0).
	peer_origin: Option<crate::Origin>,
	subscribes: Lock<HashMap<u64, TrackEntry>>,
	next_id: Arc<atomic::AtomicU64>,
	version: Version,
	/// The peer's advertised SETUP (lite-05+), set when its Setup stream is read.
	peer_setup: super::PeerSetup,
	/// Local policy overriding the peer's declared egress price. See `poll_link_cost`.
	cost: Option<u64>,
	/// Sources created by the announce half, drained by the driver into
	/// [`SourceServe`] machines.
	sources: kio::Queue<(PathOwned, crate::broadcast::Dynamic)>,
	going_away: crate::goaway::GoingAway,
}

#[derive(Clone)]
struct TrackEntry {
	producer: track::Producer,
	/// Timestamp scale from this track's TRACK_INFO, known before the SUBSCRIBE is
	/// even opened, so group streams decode frames without blocking.
	timescale: Option<Timescale>,
}

impl<S: crate::transport::poll::Session> Subscriber<S> {
	pub fn new(config: SubscriberConfig<S>) -> Self {
		// Identity for incoming-hop loop detection. Derived from the local
		// origin we publish into so it matches the relay identity across
		// every session sharing that origin, required for cross-session
		// loop detection.
		let self_origin = *config.origin;
		Self {
			session: config.session,
			origin: config.origin,
			recv_bandwidth: config.recv_bandwidth,
			self_origin,
			session_origin: config.peer_origin.unwrap_or(crate::Origin::UNKNOWN),
			peer_origin: config.peer_origin,
			subscribes: Default::default(),
			next_id: Default::default(),
			version: config.version,
			peer_setup: config.peer_setup,
			cost: config.cost,
			sources: kio::Queue::new(),
			going_away: config.going_away,
		}
	}

	/// Reject a new request once the peer has sent a GOAWAY: it told us to stop
	/// opening streams on this session (existing subscriptions keep flowing).
	fn check_going_away(&self) -> Result<(), Error> {
		if self.going_away.is_set() {
			return Err(Error::GoingAway);
		}
		Ok(())
	}

	/// What pulling content across this session's link costs, added to the route cost
	/// of every announcement received over it.
	///
	/// A locally configured price wins, since what we charge our own routing is local
	/// policy. Otherwise we charge what the peer declared, which is how a server prices
	/// a link at all: it cannot tell a sibling from a stranger, so the dialer that chose
	/// the peer declares the price for both of them. Falls back to
	/// [`super::DEFAULT_COST`] when neither priced it, and to `0` on a version that
	/// carries no cost at all, whose routes rank on hop count alone.
	///
	/// Our own price short-circuits the peer's, so a session that configured one never
	/// blocks on a SETUP to start routing.
	fn poll_link_cost(&self, waiter: &kio::Waiter) -> Poll<u64> {
		// Older versions carry no cost on the wire, so nothing is charged and their
		// routes rank on hop count alone. Returning early also avoids blocking on a
		// SETUP that versions without a Setup Stream never send.
		if !self.version.has_route_cost() {
			return Poll::Ready(0);
		}
		match self.cost {
			Some(cost) => Poll::Ready(cost),
			None => self
				.peer_setup
				.poll_cost(waiter)
				.map(|cost| cost.unwrap_or(super::DEFAULT_COST)),
		}
	}

	/// Apply one received announce message to the origin and the per-stream
	/// bookkeeping in `run`.
	fn handle_announce(
		&mut self,
		prefix: &PathOwned,
		announce: lite::AnnounceBroadcast<'_>,
		run: &mut PrefixRun,
	) -> Result<(), Error> {
		match announce {
			lite::AnnounceBroadcast::Active { suffix, hops, cost } => {
				let path = prefix.join(&suffix);
				if self.version.has_announce_id() {
					// Every `active` assigns the next ordinal, even ones we drop locally.
					run.announced_by_id.insert(run.next_announce_id, path.clone());
					run.next_announce_id += 1;
				}
				if lite::restart_supported(self.version)
					&& !self.version.has_announce_id()
					&& run.routes.contains_key(&path)
				{
					// lite-05 only: a duplicate ANNOUNCE for an already-announced path is a RESTART;
					// atomically replace the broadcast. Lite06+ restarts by announce id, and older
					// versions never defined restarts, so both fall through to start_announce, which
					// rejects the duplicate (Error::Duplicate).
					self.restart_announce(
						path.clone(),
						hops,
						cost,
						run.link_cost,
						run.responder_origin,
						&mut run.routes,
					)?;
				} else {
					self.start_announce(
						path.clone(),
						hops,
						cost,
						run.link_cost,
						run.responder_origin,
						&mut run.routes,
					)?;
				}
				// The first `initial_count` Active messages are the initial set; once
				// they're all in, the caller drops its producer to mark this prefix
				// connected.
				run.initial_remaining = run.initial_remaining.saturating_sub(1);
			}
			lite::AnnounceBroadcast::Ended { suffix, .. } => {
				let path = prefix.join(&suffix);
				tracing::debug!(broadcast = %self.log_path(&path), "unannounced");

				// The matching Active may have been silently dropped by
				// start_announce as a reflected loop, in which case
				// `routes` has no entry; that's expected, not an error.
				// A deliberate unannounce, so finish() rather than drop; the origin
				// unannounces if this was the broadcast's last route.
				if let Some(entry) = run.routes.remove(&path) {
					entry.finish();
				}
			}
			lite::AnnounceBroadcast::EndedId { id } => {
				// Resolve and retire the id; an unknown or already-retired id is a
				// protocol violation.
				let Some(path) = run.announced_by_id.remove(&id) else {
					return Err(Error::ProtocolViolation);
				};
				tracing::debug!(broadcast = %self.log_path(&path), "unannounced");

				if let Some(entry) = run.routes.remove(&path) {
					entry.finish();
				}
			}
			lite::AnnounceBroadcast::Restart { id, hops, cost } => {
				// Resolve the id; it stays live (the replacement reuses it). An unknown
				// or retired id is a protocol violation.
				let Some(path) = run.announced_by_id.get(&id).cloned() else {
					return Err(Error::ProtocolViolation);
				};
				if run.routes.contains_key(&path) {
					self.restart_announce(path, hops, cost, run.link_cost, run.responder_origin, &mut run.routes)?;
				} else {
					// The original announce was dropped locally (e.g. a reflected loop);
					// the replacement may be routable, so treat it as a fresh start.
					self.start_announce(path, hops, cost, run.link_cost, run.responder_origin, &mut run.routes)?;
				}
			}
		}
		Ok(())
	}

	/// Returns `Ok(true)` if the announce was accepted (and a route was attached to
	/// the origin's broadcast at the path), `Ok(false)` if it was dropped as a
	/// reflected loop.
	fn start_announce(
		&mut self,
		path: PathOwned,
		mut hops: crate::OriginList,
		// The route cost off the wire, i.e. as the peer advertised it. Zero before
		// lite-06, leaving the hop chain as the only routing input as before.
		cost: RouteCost,
		// This link's price, added to the wire cost; the pre-charge value is kept
		// on the route so the origin's handover gate can tell a warm peer apart.
		link_cost: u64,
		// Lite05+: the announce sender's origin id (from AnnounceOk). The sender no
		// longer stamps itself onto the chain, so we append it here to reconstruct
		// the full `[src...sender]` chain Lite04 stored. None for older versions,
		// where the sender already appended itself.
		responder_origin: Option<crate::Origin>,
		routes: &mut HashMap<PathOwned, AnnouncedRoute>,
	) -> Result<bool, Error> {
		if let Some(responder) = responder_origin {
			// If the chain is already full, drop the announce. This is the same decision
			// the Lite04 sender makes at its push site.
			if hops.push(responder).is_err() {
				tracing::warn!(
					broadcast = %self.log_path(&path),
					"dropping announce; hop chain at MAX_HOPS (possible loop)",
				);
				return Ok(false);
			}
		}

		// Drop announces that already passed through us. This connection is
		// a reflection, not a new path. Lite04/05 peers filter these out for us
		// via AnnounceRequest.exclude_hop, but that is only an optimization:
		// this is the authoritative cluster-loop check, and the only one on
		// every other version.
		if hops.contains(&self.self_origin) {
			tracing::debug!(broadcast = %self.log_path(&path), "dropping reflected announce");
			return Ok(false);
		}

		// Lite03 carries its hop count as UNKNOWN placeholders rather than real
		// ids. Rewrite the first placeholder with this connection's origin so
		// the route is attributable to the upstream session, without changing
		// the hop count (shortest-path selection and the MAX_HOPS limit stay
		// accurate). Lite01/02 send no placeholders; they're covered below.
		if self.version_lacks_hops() {
			hops.replace_first(crate::Origin::UNKNOWN, self.session_origin);
		}

		// Guarantee at least one attributable hop for versions that did not provide
		// one. Lite05 peers may legally advertise responder id 0; preserve it above
		// rather than replacing it, even though that route stays loop-blind (the
		// caller can assign an identity via `with_peer_origin`, substituted where
		// the AnnounceOk is read).
		if hops.is_empty() {
			hops.push(self.session_origin)
				.expect("an empty hop chain always has room for one entry");
		}

		// Make sure the peer doesn't double announce.
		if routes.contains_key(&path) {
			return Err(Error::Duplicate);
		}

		tracing::debug!(broadcast = %self.log_path(&path), hops = hops.len(), "announce");

		// The first hop of the reconstructed chain identifies the original
		// publisher; a later restart advertising a different first hop is a new
		// broadcast, not an alternate route to this one.
		let publisher = hops.iter().next().copied().unwrap_or(self.session_origin);

		// Create this session's source feeding the origin-owned broadcast at the
		// path. The first source creates and announces the broadcast; later sources
		// (other sessions announcing the same path) join it silently as standbys.
		// An error means the path is outside our scope, so don't serve it.
		// Reflections are already filtered above.
		let route = self.announced_route(hops, cost, link_cost);
		let Ok(source) = self.origin.create_broadcast(&path, route) else {
			return Ok(false);
		};

		// Serve the origin's track requests for this source in the background; the
		// announce loop keeps the producer so an unannounce can finish it.
		let _ = self.sources.try_push((path.clone(), source.dynamic()));
		routes.insert(path, AnnouncedRoute::new(source, publisher));

		Ok(true)
	}

	/// The route to attach to a broadcast this peer announced, charging our link's
	/// price on top of the cost it advertised.
	///
	/// Once the peer has sent a GOAWAY every route it announces starts out draining,
	/// including a restart of one already attached: a connection on its way out must
	/// not win selection, however good the path it advertises looks.
	fn announced_route(&self, hops: crate::OriginList, cost: RouteCost, link_cost: u64) -> crate::broadcast::Route {
		let mut route = crate::broadcast::Route::new()
			.with_hops(hops)
			.with_cost(cost.charged(link_cost).0)
			.with_announce(true);
		route.advertised = cost.0;

		if self.going_away.is_set() {
			route.cost = crate::broadcast::DRAIN_COST;
		}

		route
	}

	/// Handle a RESTART (an explicit restart status, or a duplicate ANNOUNCE on lite-05).
	///
	/// The first hop of the chain identifies the original publisher. When it matches
	/// the prior advertisement and is a real identity, the broadcast is the same
	/// content on a new path: this session's route metadata updates in place,
	/// in-flight tracks keep flowing, and the origin only hands over if the winner
	/// changed. Consumers observe nothing. When the first hop differs, or is
	/// [`Origin::UNKNOWN`](crate::Origin::UNKNOWN), the old route detaches gracefully
	/// and a fresh one attaches, so downstream sees a real Ended + Active.
	///
	/// Returns `Ok(false)` if the new hop chain is a reflected loop (this session's
	/// route is now gone), `Ok(true)` otherwise.
	fn restart_announce(
		&mut self,
		path: PathOwned,
		mut hops: crate::OriginList,
		// The route cost off the wire and this link's price. See `start_announce`.
		cost: RouteCost,
		link_cost: u64,
		// Lite05+: the announce sender's origin id (from AnnounceOk), appended here to
		// rebuild the full chain since the sender no longer stamps itself. None for older
		// versions. See `start_announce`.
		responder_origin: Option<crate::Origin>,
		routes: &mut HashMap<PathOwned, AnnouncedRoute>,
	) -> Result<bool, Error> {
		// Reflected loop (or a full chain): the route can't be used here anymore. Retire it.
		let reflected = match responder_origin {
			Some(responder) => hops.push(responder).is_err() || hops.contains(&self.self_origin),
			None => hops.contains(&self.self_origin),
		};
		if reflected {
			tracing::debug!(broadcast = %self.log_path(&path), "dropping reflected restart");
			// The peer retracted the route deliberately; detach gracefully.
			if let Some(entry) = routes.remove(&path) {
				entry.finish();
			}
			return Ok(false);
		}

		tracing::debug!(broadcast = %self.log_path(&path), hops = hops.len(), "restart");
		let publisher = hops.iter().next().copied().unwrap_or(self.session_origin);
		let metadata = self.announced_route(hops, cost, link_cost);

		match routes.get_mut(&path) {
			Some(entry) if entry.publisher != publisher || publisher == crate::Origin::UNKNOWN => {
				// A different original publisher, or no identity at all (UNKNOWN
				// never proves continuity): a brand-new broadcast may have replaced
				// the old one at this path. Detach gracefully (downstream unannounces
				// if this was the last source) and attach fresh below; cached
				// TRACK_INFO and subscriptions must not carry over.
				let entry = routes.remove(&path).expect("matched above");
				entry.finish();
			}
			Some(entry) => {
				// Same publisher, new path: update the source's route in place.
				// In-flight tracks keep flowing; the origin only hands over if the
				// winning source changed.
				entry.set_route(metadata);
				return Ok(true);
			}
			None => {}
		}

		let Ok(source) = self.origin.create_broadcast(&path, metadata) else {
			return Ok(false);
		};
		let _ = self.sources.try_push((path.clone(), source.dynamic()));
		routes.insert(path, AnnouncedRoute::new(source, publisher));

		Ok(true)
	}

	/// Decode one datagram body and hand it to the matching subscription's producer.
	fn route_datagram(&self, payload: bytes::Bytes) -> Result<(), Error> {
		let mut buf = payload;
		let dg = lite::Datagram::decode(&mut buf, self.version)?;

		let mut entry = match self.subscribes.lock().get(&dg.subscribe) {
			Some(entry) => entry.clone(),
			// Unknown or already-closed subscription: drop the datagram.
			None => return Ok(()),
		};

		// Datagrams are lite-05+, which always negotiates a timescale; default defensively.
		let scale = entry.timescale.unwrap_or_default();
		let timestamp =
			Timestamp::new(dg.timestamp, scale).map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;

		entry.producer.write_datagram(crate::Datagram {
			sequence: dg.sequence,
			timestamp,
			payload: dg.payload,
		})?;
		Ok(())
	}

	fn log_path(&self, path: impl AsPath) -> Path<'_> {
		self.origin.root().join(path)
	}

	/// True for versions that don't carry a real hop list on the wire, so the
	/// received chain is empty (Lite01/02) or anonymous placeholders (Lite03).
	fn version_lacks_hops(&self) -> bool {
		matches!(self.version, Version::Lite01 | Version::Lite02 | Version::Lite03)
	}
}

/// The subscriber half's driver: the announce prefixes, the uni-stream accept
/// loop, PROBE feedback, datagrams, and the per-source serve machines. Only an
/// error ends it.
pub(super) struct SubscriberDriver<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	/// Our own clone of the connection-progress producer, dropped on the first
	/// poll. Holding it until then keeps `Connecting` pending so `connect()`
	/// drives the driver at least once, exactly like the old announce task; the
	/// per-prefix clones then own the boundary.
	connecting: Option<ConnectingProducer>,
	/// One machine per permitted prefix. Only an error ends the session; a
	/// prefix finishing cleanly (publisher FIN) just retires.
	prefixes: Vec<AnnouncePrefix<S>>,
	uni: UniAccept<S>,
	/// PROBE feedback; finishes quietly when unsupported or given up on.
	bandwidth: Option<RecvBandwidth<S>>,
	/// Datagram receive; inert on a version or transport without datagrams.
	datagrams: Option<DatagramRecv<S>>,
	/// One machine per announced source, serving the origin's track requests.
	sources: PollSet<SourceServe<S>>,
}

impl<S: crate::transport::poll::Session> SubscriberDriver<S> {
	/// `connecting` is the connection-progress producer for this session (None for
	/// versions with no initial-set boundary). Each prefix holds its own clone and
	/// drops it once its initial set is in; with no prefixes it drops here, so the
	/// session is connected now.
	pub fn new(subscriber: Subscriber<S>, connecting: Option<ConnectingProducer>) -> Self {
		let prefixes = subscriber
			.origin
			.allowed()
			.map(|p| p.to_owned())
			.collect::<Vec<PathOwned>>()
			.into_iter()
			.map(|prefix| AnnouncePrefix::new(subscriber.clone(), prefix, connecting.clone()))
			.collect();

		Self {
			connecting,
			prefixes,
			uni: UniAccept::new(subscriber.clone()),
			bandwidth: Some(RecvBandwidth::new(subscriber.clone())),
			datagrams: Some(DatagramRecv::new(subscriber.clone())),
			sources: PollSet::new(),
			subscriber,
		}
	}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Each prefix holds its own clone; with no prefixes, this drop is what
		// marks the session connected.
		self.connecting.take();

		let mut i = 0;
		while i < self.prefixes.len() {
			match self.prefixes[i].poll(waiter) {
				Poll::Ready(Ok(())) => {
					self.prefixes.swap_remove(i);
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => i += 1,
			}
		}
		if let Poll::Ready(res) = self.uni.poll(waiter) {
			return Poll::Ready(res);
		}
		if let Some(bandwidth) = &mut self.bandwidth {
			match bandwidth.poll(waiter) {
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Ready(Ok(())) => self.bandwidth = None,
				Poll::Pending => {}
			}
		}
		if let Some(datagrams) = &mut self.datagrams {
			match datagrams.poll(waiter) {
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Ready(Ok(())) => self.datagrams = None,
				Poll::Pending => {}
			}
		}

		// Sources created by the announce half; their completion never ends the
		// session (the origin delivers the unannounce itself).
		while let Poll::Ready(Ok((path, dynamic))) = self.subscriber.sources.poll_pop(waiter) {
			self.sources
				.push(SourceServe::new(self.subscriber.clone(), path, dynamic));
		}
		let _ = self.sources.poll(waiter);

		Poll::Pending
	}
}

/// Accepts incoming uni streams (GROUP data plus the peer's SETUP) and drives
/// each as a child machine. Resolves only on a transport error.
struct UniAccept<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	// A dedicated accept handle: the poll interface takes `&mut self`.
	accept: S,
	children: PollSet<UniServe<S>>,
}

impl<S: crate::transport::poll::Session> UniAccept<S> {
	fn new(subscriber: Subscriber<S>) -> Self {
		let accept = subscriber.session.clone();
		Self {
			subscriber,
			accept,
			children: PollSet::new(),
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let _ = self.children.poll(waiter);

		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match self.accept.poll_accept_uni(&mut cx) {
				Poll::Ready(Ok(stream)) => self.children.push(UniServe {
					subscriber: self.subscriber.clone(),
					state: UniState::Start {
						reader: Reader::new(stream, self.subscriber.version),
					},
				}),
				Poll::Ready(Err(err)) => return Poll::Ready(Err(Error::from_transport(err))),
				Poll::Pending => break,
			}
		}

		// Newly accepted children start now rather than on the next wake.
		let _ = self.children.poll(waiter);
		Poll::Pending
	}
}

/// One accepted uni stream, dispatched on its first varint.
struct UniServe<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	state: UniState<S>,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum UniState<S: crate::transport::poll::Session> {
	/// Reading the stream's type.
	Start {
		reader: Reader<S::RecvStream, Version>,
	},
	/// Reading the peer's single SETUP message, recorded so capability-gated
	/// streams (PROBE) can consult it. lite-05+ only.
	Setup {
		reader: Reader<S::RecvStream, Version>,
	},
	Group(GroupRecv<S>),
	Done,
}

impl<S: crate::transport::poll::Session> Machine for UniServe<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			tracing::debug!(%err, "error running uni stream");
		}
		Poll::Ready(())
	}
}

impl<S: crate::transport::poll::Session> UniServe<S> {
	/// Abort the stream with the given error, wherever the reader currently lives.
	fn abort(&mut self, err: &Error) {
		match &mut self.state {
			UniState::Start { reader } | UniState::Setup { reader } => reader.abort(err),
			UniState::Group(recv) => recv.reader.abort(err),
			UniState::Done => {}
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				UniState::Start { reader } => {
					let mut cx = std::task::Context::from_waker(waiter.waker());
					// A decode error here is only logged; the peer hung up or spoke garbage
					// before the stream had a type.
					let kind = ready!(reader.poll_decode::<lite::DataType>(&mut cx))?;
					let UniState::Start { reader } = std::mem::replace(&mut self.state, UniState::Done) else {
						unreachable!()
					};
					self.state = match kind {
						lite::DataType::Group => UniState::Group(GroupRecv::new(self.subscriber.clone(), reader)),
						lite::DataType::Setup => UniState::Setup { reader },
					};
				}
				UniState::Setup { reader } => {
					if !self.subscriber.version.has_setup_stream() {
						let err = Error::UnexpectedStream;
						self.abort(&err);
						return Poll::Ready(Ok(()));
					}
					let mut cx = std::task::Context::from_waker(waiter.waker());
					let res = ready!(reader.poll_decode::<lite::Setup>(&mut cx));
					match res {
						Ok(setup) => {
							tracing::debug!(?setup, "received peer setup");
							self.subscriber.peer_setup.set(setup);
							return Poll::Ready(Ok(()));
						}
						Err(err) => {
							self.abort(&err);
							return Poll::Ready(Ok(()));
						}
					}
				}
				UniState::Group(recv) => {
					let res = ready!(recv.poll_serve(waiter));
					if let Err(err) = res {
						self.abort(&err);
					}
					return Poll::Ready(Ok(()));
				}
				UniState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

/// Receives one GROUP stream into its subscription's track producer.
struct GroupRecv<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	reader: Reader<S::RecvStream, Version>,
	state: GroupRecvState,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum GroupRecvState {
	/// Reading the GROUP header.
	Header,
	/// Filling the group, bailing if the track or group dies first.
	Serve {
		group: group::Producer,
		track: track::Producer,
		ingest: FrameIngest,
	},
	Done,
}

impl<S: crate::transport::poll::Session> GroupRecv<S> {
	fn new(subscriber: Subscriber<S>, reader: Reader<S::RecvStream, Version>) -> Self {
		Self {
			subscriber,
			reader,
			state: GroupRecvState::Header,
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				GroupRecvState::Header => {
					let mut cx = std::task::Context::from_waker(waiter.waker());
					let hdr = ready!(self.reader.poll_decode::<lite::Group>(&mut cx))?;

					let (group, track, timescale) = {
						let mut subs = self.subscriber.subscribes.lock();
						let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

						let group_info = group::Info { sequence: hdr.sequence };
						// Stats (groups/frames/bytes) are counted in the model as the group
						// is written, through the tagged `track::Producer`.
						let mut group = entry.producer.create_group(group_info)?;
						// The stream may carry only the tail of the group; number the frames
						// from where the publisher said they start so a reader splicing
						// across routes lines them up.
						group.start_at(hdr.frame_start)?;
						(group, entry.producer.clone(), entry.timescale)
					};

					// The timescale came from TRACK_INFO (read before this subscription was
					// even registered), so frames decode immediately. No SUBSCRIBE_OK to
					// wait on.
					self.state = GroupRecvState::Serve {
						group,
						track,
						ingest: FrameIngest::new(timescale),
					};
				}
				GroupRecvState::Serve { group, track, ingest } => {
					// The track or group dying cancels the stream; the peer's own close
					// arrives through the ingest's reads.
					let res = 'serve: {
						if let Poll::Ready(err) = track.poll_closed(waiter) {
							break 'serve Err(err);
						}
						if let Poll::Ready(err) = group.poll_closed(waiter) {
							break 'serve Err(err);
						}
						match ingest.poll(&mut self.reader, group, waiter) {
							Poll::Ready(res) => break 'serve res,
							Poll::Pending => return Poll::Pending,
						}
					};

					let GroupRecvState::Serve { group, .. } = std::mem::replace(&mut self.state, GroupRecvState::Done)
					else {
						unreachable!()
					};
					match res {
						Ok(()) => {
							let mut group = group;
							let _ = group.finish();
						}
						Err(Error::Cancel) => {
							let _ = group.abort(Error::Cancel);
						}
						Err(err) => {
							tracing::debug!(%err, group = %group.sequence, "group error");
							let _ = group.abort(err.clone());
							return Poll::Ready(Err(err));
						}
					}
					return Poll::Ready(Ok(()));
				}
				GroupRecvState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

/// Pumps bare FRAME messages from a reader into a group producer: the wire
/// format shared by GROUP streams and FETCH responses.
struct FrameIngest {
	/// `Some` decodes the lite-05 zigzag-delta timestamp prefix; `None` stamps
	/// local receive time (pre-lite-05).
	timescale: Option<Timescale>,
	/// Previous frame's raw timestamp value (in `timescale` units), for the
	/// zigzag-delta decode. The first frame's delta is absolute (prev = 0).
	prev_ts: u64,
	phase: IngestPhase,
}

enum IngestPhase {
	/// Reading the timestamp delta (skipped without a timescale). Stream end here
	/// means the group has no more frames.
	Timing,
	/// Reading the frame size. Stream end here also ends the group (pre-lite-05,
	/// where there is no timing prefix to act as the sentinel).
	Size { timestamp: Option<Timestamp> },
	/// Streaming the frame payload.
	Payload { frame: frame::ProducerOwned },
}

impl FrameIngest {
	fn new(timescale: Option<Timescale>) -> Self {
		Self {
			timescale,
			prev_ts: 0,
			phase: IngestPhase::Timing,
		}
	}

	/// `Ready(Ok(()))` once the stream FINs on a frame boundary. The caller
	/// finishes or aborts the group; a frame cut short mid-payload was already
	/// aborted here with the reason.
	fn poll<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut Reader<R, Version>,
		group: &mut group::Producer,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.phase {
				IngestPhase::Timing => {
					let Some(scale) = self.timescale else {
						self.phase = IngestPhase::Size { timestamp: None };
						continue;
					};
					// The timestamp delta doubles as the per-frame sentinel.
					let Some(zz) = ready!(reader.poll_decode_maybe::<crate::coding::VarInt>(&mut cx))? else {
						return Poll::Ready(Ok(()));
					};
					let next: u64 = (self.prev_ts as i128 + zz.to_zigzag() as i128)
						.try_into()
						.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
					self.prev_ts = next;
					let timestamp = Timestamp::new(next, scale)
						.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
					self.phase = IngestPhase::Size {
						timestamp: Some(timestamp),
					};
				}
				IngestPhase::Size { timestamp } => {
					let Some(size) = ready!(reader.poll_decode_maybe::<u64>(&mut cx))? else {
						return Poll::Ready(Ok(()));
					};
					// `create_frame_owned` is the allocation chokepoint and rejects an
					// oversized `size` before allocating, so no pre-check is needed. No
					// wire timestamp (pre-lite-05) means local receive time.
					let timestamp = timestamp.unwrap_or_else(Timestamp::now);
					let frame = group.create_frame_owned(frame::Info { size, timestamp })?;
					self.phase = IngestPhase::Payload { frame };
				}
				IngestPhase::Payload { frame } => {
					let failed = loop {
						if frame.remaining() == 0 {
							break None;
						}
						match reader.poll_read_chunk(&mut cx, frame.remaining()) {
							Poll::Pending => return Poll::Pending,
							Poll::Ready(Ok(Some(chunk))) if !chunk.is_empty() => {
								if let Err(err) = frame.write(chunk) {
									break Some(err);
								}
							}
							Poll::Ready(Ok(_)) => break Some(Error::WrongSize),
							Poll::Ready(Err(err)) => break Some(err),
						}
					};

					let IngestPhase::Payload { frame } = std::mem::replace(&mut self.phase, IngestPhase::Timing) else {
						unreachable!()
					};
					match failed {
						None => frame.finish()?,
						Some(err) => {
							// Fail the group with the reason, not the Drop fallback's
							// generic `Dropped`.
							let _ = frame.abort(err.clone());
							return Poll::Ready(Err(err));
						}
					}
				}
			}
		}
	}
}

/// Receives QUIC datagrams and routes each to its subscription's track producer
/// (lite-05 §6.4).
///
/// A decode error or an unknown subscribe id drops that datagram without tearing
/// down the session (best-effort); only a transport-level failure ends the loop.
struct DatagramRecv<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	// A dedicated receive handle: the poll interface takes `&mut self`.
	recv: S,
	enabled: bool,
}

impl<S: crate::transport::poll::Session> DatagramRecv<S> {
	fn new(subscriber: Subscriber<S>) -> Self {
		let recv = subscriber.session.clone();
		let enabled = subscriber.version.has_datagrams() && recv.max_datagram_size() > 0;
		Self {
			subscriber,
			recv,
			enabled,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if !self.enabled {
			return Poll::Ready(Ok(()));
		}
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			let payload = ready!(self.recv.poll_recv_datagram(&mut cx)).map_err(Error::from_transport)?;
			if let Err(err) = self.subscriber.route_datagram(payload) {
				tracing::debug!(%err, "dropping datagram");
			}
		}
	}
}

/// Opens a PROBE stream on demand while a consumer is interested.
///
/// Loops forever: wait for a consumer, race the probe stream against the
/// consumer leaving, then loop back. Probe is best-effort, so stream errors are
/// logged but never tear down the session.
struct RecvBandwidth<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	state: BandwidthState<S>,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum BandwidthState<S: crate::transport::poll::Session> {
	/// lite-05+ negotiates probing: only open a PROBE stream if the peer
	/// advertised it (Report or higher) in its SETUP. Older versions have no
	/// SETUP, so probe is always available there.
	Gate,
	/// Wait until at least one consumer is interested in the estimate.
	WaitUsed,
	/// Race the last consumer leaving against the probe stream ending.
	Probing(ProbeStream<S>),
}

impl<S: crate::transport::poll::Session> RecvBandwidth<S> {
	fn new(subscriber: Subscriber<S>) -> Self {
		Self {
			subscriber,
			state: BandwidthState::Gate,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				BandwidthState::Gate => {
					if self.subscriber.recv_bandwidth.is_none() {
						return Poll::Ready(Ok(()));
					}
					if self.subscriber.version.has_setup_stream()
						&& ready!(self.subscriber.peer_setup.poll_probe_level(waiter)) < lite::ProbeLevel::Report
					{
						tracing::debug!("peer does not support probing; skipping probe stream");
						return Poll::Ready(Ok(()));
					}
					self.state = BandwidthState::WaitUsed;
				}
				BandwidthState::WaitUsed => {
					let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated above");
					match ready!(bandwidth.poll_used(waiter)) {
						Ok(()) => self.state = BandwidthState::Probing(ProbeStream::new(&self.subscriber)),
						Err(_) => return Poll::Ready(Ok(())),
					}
				}
				BandwidthState::Probing(probe) => {
					let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated above");
					match bandwidth.poll_unused(waiter) {
						// Loop back: a new consumer may arrive later. Dropping the probe
						// machine resets its stream.
						Poll::Ready(Ok(())) => {
							self.state = BandwidthState::WaitUsed;
							continue;
						}
						// The channel closed: give up for the rest of the session.
						Poll::Ready(Err(_)) => return Poll::Ready(Ok(())),
						Poll::Pending => {}
					}
					match ready!(probe.poll(waiter)) {
						Ok(()) => tracing::debug!("probe stream closed"),
						Err(err) => tracing::warn!(%err, "probe stream error"),
					}
					// The stream ended (peer FIN'd or errored). Don't hammer an
					// uncooperative peer; give up for the rest of the session.
					return Poll::Ready(Ok(()));
				}
			}
		}
	}
}

/// One PROBE stream: send the type, then feed the peer's estimates into the
/// bandwidth producer until it FINs.
struct ProbeStream<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	session: S,
	state: ProbeState<S>,
}

enum ProbeState<S: crate::transport::poll::Session> {
	Open,
	Send { stream: Stream<S, Version> },
	Read { stream: Stream<S, Version> },
}

impl<S: crate::transport::poll::Session> ProbeStream<S> {
	fn new(subscriber: &Subscriber<S>) -> Self {
		Self {
			subscriber: subscriber.clone(),
			session: subscriber.session.clone(),
			state: ProbeState::Open,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				ProbeState::Open => {
					// After a GOAWAY the peer must not see new streams. Probe is
					// best-effort; skip it rather than erroring.
					if self.subscriber.going_away.is_set() {
						return Poll::Ready(Ok(()));
					}
					let mut stream = ready!(Stream::poll_open(&mut self.session, self.subscriber.version, &mut cx))?;
					stream.writer.buffer(&lite::ControlType::Probe)?;
					self.state = ProbeState::Send { stream };
				}
				ProbeState::Send { stream } => {
					ready!(stream.writer.poll_flush(&mut cx))?;
					let ProbeState::Send { stream } = std::mem::replace(&mut self.state, ProbeState::Open) else {
						unreachable!()
					};
					self.state = ProbeState::Read { stream };
				}
				ProbeState::Read { stream } => {
					let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated by RecvBandwidth");
					loop {
						let Some(probe) = ready!(stream.reader.poll_decode_maybe::<lite::Probe>(&mut cx))? else {
							return Poll::Ready(Ok(()));
						};
						bandwidth.set(Some(probe.bitrate))?;
					}
				}
			}
		}
	}
}

/// One announce-interest stream: sends the ANNOUNCE_REQUEST for a prefix, then
/// feeds every received announce into the origin. Only its *error* ends the
/// session; a publisher FIN is a clean end for the prefix alone.
struct AnnouncePrefix<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	prefix: PathOwned,
	// Dropped once this prefix's initial set is in (or on any exit), so a failed
	// prefix can't hang connect().
	connecting: Option<ConnectingProducer>,
	state: PrefixState<S>,
}

enum PrefixState<S: crate::transport::poll::Session> {
	/// Opening the control stream (after the GOAWAY gate).
	Open,
	/// Flushing the buffered request.
	Send { stream: Stream<S, Version> },
	/// Lite05+: reading the publisher's ANNOUNCE_OK.
	ReadOk { stream: Stream<S, Version> },
	/// Waiting for the link cost (may block on the peer's SETUP).
	Cost {
		stream: Stream<S, Version>,
		responder_origin: Option<crate::Origin>,
		initial_count: u64,
	},
	/// Lite01/02: reading the ANNOUNCE_INIT set.
	ReadInit { stream: Stream<S, Version>, run: PrefixRun },
	/// Streaming announce updates.
	Run { stream: Stream<S, Version>, run: PrefixRun },
}

/// The announce-decode loop's state, split out so the states above can share it.
struct PrefixRun {
	responder_origin: Option<crate::Origin>,
	/// What we charge every announcement arriving on this stream. Resolved once:
	/// it comes from the connect config or the peer's SETUP, neither of which
	/// changes for the life of the session.
	link_cost: u64,
	/// The first `initial_count` Active messages are the initial set; once
	/// they're all in, the connecting producer drops to mark this prefix
	/// connected.
	initial_remaining: u64,
	routes: HashMap<PathOwned, AnnouncedRoute>,
	// Lite06+: announce ids. Each received `active` implicitly assigns the next
	// per-stream ordinal; `ended`/`restart` reference it instead of repeating the
	// path. Tracked even for announces we drop locally (reflected loops), since
	// the sender doesn't know we dropped them. We never send a restart ourselves,
	// but a peer may.
	next_announce_id: u64,
	announced_by_id: HashMap<u64, PathOwned>,
}

impl<S: crate::transport::poll::Session> AnnouncePrefix<S> {
	fn new(subscriber: Subscriber<S>, prefix: PathOwned, connecting: Option<ConnectingProducer>) -> Self {
		Self {
			subscriber,
			prefix,
			connecting,
			state: PrefixState::Open,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				PrefixState::Open => {
					// A peer that sent GOAWAY told us to stop opening streams on this session.
					self.subscriber.check_going_away()?;
					let mut stream = ready!(Stream::poll_open(
						&mut self.subscriber.session,
						self.subscriber.version,
						&mut cx
					))?;

					stream.writer.buffer(&lite::ControlType::Announce)?;
					// Lite04/05: ask the peer to filter out announces that already passed
					// through us, so the reflected ones never hit the wire. Encoding drops
					// this on every other version, where start_announce below is the only
					// filter.
					stream.writer.buffer(&lite::AnnounceRequest {
						prefix: self.prefix.as_path(),
						exclude_hop: self.subscriber.self_origin.id(),
					})?;
					self.state = PrefixState::Send { stream };
				}
				PrefixState::Send { stream } => {
					ready!(stream.writer.poll_flush(&mut cx))?;
					let PrefixState::Send { stream } = std::mem::replace(&mut self.state, PrefixState::Open) else {
						unreachable!()
					};
					self.state = match self.subscriber.version.has_announce_ok() {
						true => PrefixState::ReadOk { stream },
						false => PrefixState::Cost {
							stream,
							responder_origin: None,
							initial_count: 0,
						},
					};
				}
				PrefixState::ReadOk { stream } => {
					// Lite05+: the publisher reports its own origin id (which we stamp onto
					// every received Announce's hop chain, since it no longer does so
					// itself) plus the count of initial active announces that follow
					// immediately.
					let ok = ready!(stream.reader.poll_decode::<lite::AnnounceOk>(&mut cx))?;
					// A peer may legally report id 0 (no identity). When the caller assigned
					// it one, stand that in so the route isn't loop-blind.
					let origin = match ok.origin.id() {
						0 => self.subscriber.peer_origin.unwrap_or(ok.origin),
						_ => ok.origin,
					};
					let PrefixState::ReadOk { stream } = std::mem::replace(&mut self.state, PrefixState::Open) else {
						unreachable!()
					};
					self.state = PrefixState::Cost {
						stream,
						responder_origin: Some(origin),
						initial_count: ok.active,
					};
				}
				PrefixState::Cost { .. } => {
					let link_cost = ready!(self.subscriber.poll_link_cost(waiter));
					let PrefixState::Cost {
						stream,
						responder_origin,
						initial_count,
					} = std::mem::replace(&mut self.state, PrefixState::Open)
					else {
						unreachable!()
					};

					let mut run = PrefixRun {
						responder_origin,
						link_cost,
						initial_remaining: 0,
						routes: HashMap::new(),
						next_announce_id: 0,
						announced_by_id: HashMap::new(),
					};

					// Release the producer once this prefix's initial set is in. Lite01/02
					// deliver it via ANNOUNCE_INIT (the ReadInit state); Lite05 delivers
					// `initial_count` Announce::Active counted in the run loop; Lite03/04
					// have no boundary.
					match self.subscriber.version {
						Version::Lite01 | Version::Lite02 => {
							self.state = PrefixState::ReadInit { stream, run };
						}
						_ if self.subscriber.version.has_announce_ok() => {
							if initial_count == 0 {
								self.connecting.take();
							}
							run.initial_remaining = initial_count;
							self.state = PrefixState::Run { stream, run };
						}
						_ => {
							self.connecting.take();
							self.state = PrefixState::Run { stream, run };
						}
					}
				}
				PrefixState::ReadInit { stream, run } => {
					let msg = ready!(stream.reader.poll_decode::<lite::AnnounceInit>(&mut cx))?;
					for suffix in msg.suffixes {
						let path = self.prefix.join(&suffix);
						// Lite01/02 don't carry hop information; the broadcast starts with
						// an empty chain and an unpriced link. Stats are attributed in the
						// model when this enters the origin via `create_broadcast`.
						self.subscriber.start_announce(
							path.clone(),
							crate::OriginList::new(),
							RouteCost::default(),
							0,
							run.responder_origin,
							&mut run.routes,
						)?;
					}
					self.connecting.take();
					let PrefixState::ReadInit { stream, run } = std::mem::replace(&mut self.state, PrefixState::Open)
					else {
						unreachable!()
					};
					self.state = PrefixState::Run { stream, run };
				}
				PrefixState::Run { stream, run } => {
					loop {
						let Some(announce) =
							ready!(stream.reader.poll_decode_maybe::<lite::AnnounceBroadcast>(&mut cx))?
						else {
							// The publisher FINed: it has nothing (more) to announce for this
							// prefix (e.g. a publish-only peer). That's a clean completion of
							// this announce stream, not a session error, so finish our side
							// and return Ok. Tearing down only the announce stream is correct
							// since no further progress can be made, but we must not
							// propagate an error that would kill the whole connection.
							stream.writer.finish().ok();
							return Poll::Ready(Ok(()));
						};
						self.subscriber.handle_announce(&self.prefix, announce, run)?;
						if run.initial_remaining == 0 {
							self.connecting.take();
						}
					}
				}
			}
		}
	}
}

/// Serves the origin's track requests for one announced source until the peer
/// unannounces (the source is finished) or the session dies. Dropping it drops
/// the in-flight track machines with it.
struct SourceServe<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	path: PathOwned,
	dynamic: crate::broadcast::Dynamic,
	// A dedicated close-watch handle, since each pending operation needs its own.
	closed: S,
	tracks: PollSet<TrackServeRun<S>>,
}

impl<S: crate::transport::poll::Session> SourceServe<S> {
	fn new(subscriber: Subscriber<S>, path: PathOwned, dynamic: crate::broadcast::Dynamic) -> Self {
		let closed = subscriber.session.clone();
		Self {
			subscriber,
			path,
			dynamic,
			closed,
			tracks: PollSet::new(),
		}
	}
}

impl<S: crate::transport::poll::Session> Machine for SourceServe<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let _ = self.tracks.poll(waiter);

		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			if self.closed.poll_closed(&mut cx).is_ready() {
				// Session gone.
				return Poll::Ready(());
			}
			// A draining peer usually stops announcing, so react to the signal
			// itself; waiting for another message would leave the route primary
			// until the session finally closed. Idempotent, since the signal
			// stays set and this task wakes for other reasons too.
			if self.subscriber.going_away.poll(waiter).is_ready() {
				self.dynamic.drain();
			}
			match self.dynamic.poll_requested_track(waiter) {
				Poll::Ready(Ok(request)) => {
					let serve = TrackServe {
						subscriber: self.subscriber.clone(),
						path: self.path.clone(),
						name: request.name().to_string(),
					};
					// One machine per track serves its lone subscription and any number
					// of fetches concurrently.
					self.tracks.push(TrackServeRun::new(serve, request));
				}
				// The source was finished (unannounced) or aborted.
				Poll::Ready(Err(err)) => {
					tracing::debug!(%err, "source closed");
					return Poll::Ready(());
				}
				Poll::Pending => break,
			}
		}

		// Newly requested tracks start now rather than on the next wake.
		let _ = self.tracks.poll(waiter);
		Poll::Pending
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Decode;
	use crate::lite::test_transport::SinkSession;

	const VERSION: Version = Version::Lite05;

	/// `establish` puts exactly one SUBSCRIBE on the wire, and the id is registered
	/// before any of it reaches the transport.
	///
	/// Both halves matter: a second stream re-requests the same id, which the peer is
	/// free to serve twice, and a late insert loses the race with a publisher that
	/// serves its first group the instant it reads the request (`recv_group` drops a
	/// group whose id isn't in the map yet).
	#[tokio::test]
	async fn establish_sends_one_registered_subscribe() {
		// Writes park until this opens, so the assertions below run at the exact moment
		// the request would hit the wire.
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_bi(gate.consume());

		let origin = origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			session: session.clone(),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: Default::default(),
			peer_origin: None,
			cost: None,
			going_away: Default::default(),
		});
		let subscribes = subscriber.subscribes.clone();
		let serve = TrackServe {
			subscriber,
			path: Path::new("room/host").to_owned(),
			name: "catalog.json".to_string(),
		};

		let mut broadcast = crate::broadcast::Info::new().produce();
		let mut producer = broadcast.create_track("catalog.json", None).unwrap();
		let mut sub = Sub::None;
		let mut establish = std::pin::pin!(serve.establish(
			&mut producer,
			&mut sub,
			Subscription::default(),
			Some(Timescale::default()),
		));

		// Parked on the first write: the stream is open and nothing has been sent yet.
		assert!(futures::poll!(establish.as_mut()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(subscribes.lock().contains_key(&0), "registered before the wire");

		let Ok(mut open) = gate.write() else {
			panic!("gate closed")
		};
		*open = true;
		drop(open);

		establish.await.unwrap();

		// One request, on one stream, and nothing else behind it.
		assert_eq!(session.log.bi_opens(), 1);

		let writes = session.log.writes.lock().unwrap().clone();
		let mut wire = writes.as_slice();
		assert_eq!(
			lite::ControlType::decode(&mut wire, VERSION).unwrap(),
			lite::ControlType::Subscribe
		);
		let msg = lite::Subscribe::decode(&mut wire, VERSION).unwrap();
		assert_eq!(msg.id, 0);
		assert_eq!(msg.track, "catalog.json");
		assert!(wire.is_empty(), "a second SUBSCRIBE trailed the first");
	}

	/// Everything a `handle_subscription` test needs to stay alive for the call.
	struct Harness {
		serve: TrackServe<SinkSession>,
		session: SinkSession,
		producer: track::Producer,
		_broadcast: crate::broadcast::Producer,
		_gate: kio::Producer<bool>,
	}

	impl Harness {
		/// A `TrackServe` writing straight to a sink session at `version`.
		fn new(version: Version) -> Self {
			// Open the gate up front: these tests assert what reached the wire, not
			// what was true at the instant it would.
			let gate = kio::Producer::new(true);
			let session = SinkSession::gated_bi(gate.consume());
			let origin = origin::Info::new(crate::Origin::new(1).unwrap()).produce();
			let subscriber = Subscriber::new(SubscriberConfig {
				session: session.clone(),
				origin,
				recv_bandwidth: None,
				version,
				peer_setup: Default::default(),
				peer_origin: None,
				cost: None,
				going_away: Default::default(),
			});
			let mut broadcast = crate::broadcast::Info::new().produce();
			let producer = broadcast.create_track("catalog.json", None).unwrap();

			Self {
				serve: TrackServe {
					subscriber,
					path: Path::new("room/host").to_owned(),
					name: "catalog.json".to_string(),
				},
				session,
				producer,
				_broadcast: broadcast,
				_gate: gate,
			}
		}

		/// Everything written to the session so far.
		fn wire(&self) -> Vec<u8> {
			self.session.log.writes.lock().unwrap().clone()
		}
	}

	/// The `(group, frame)` bounds `resume::slice` produces after a mid-group takeover: a
	/// subscriber resuming at frame 3 of group 5, capped by a later boundary in the same
	/// group.
	fn mid_group_demand() -> Subscription {
		Subscription::default()
			.with_start(Position { group: 5, frame: 3 })
			.with_end(Position::after(5, 7))
	}

	/// A mid-group resume boundary handed to a peer that predates lite-06 is widened to
	/// the whole group rather than refused.
	///
	/// The codec rejects a frame bound such a peer cannot carry, so passing the demand
	/// through unchanged fails the SUBSCRIBE and hands the track back. The origin then
	/// re-splices the same route indefinitely, since the splice itself keeps succeeding
	/// and the retry budget never trips.
	#[tokio::test]
	async fn frame_bounds_widen_for_an_older_peer() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(mid_group_demand()),
				true,
				Some(Timescale::default()),
			)
			.await
			.expect("an older peer must not fail the subscribe");

		let wire = h.wire();
		let mut wire = wire.as_slice();
		assert_eq!(
			lite::ControlType::decode(&mut wire, Version::Lite05).unwrap(),
			lite::ControlType::Subscribe
		);
		let msg = lite::Subscribe::decode(&mut wire, Version::Lite05).unwrap();
		// The group bounds survive; only the frame offsets are widened away.
		assert_eq!((msg.start_group, msg.end_group), (Some(5), Some(5)));
		assert_eq!((msg.start_frame, msg.end_frame), (0, None));
	}

	/// The same demand on a lite-06 peer keeps its frame offsets, so the widening is
	/// version-gated rather than unconditional.
	#[tokio::test]
	async fn frame_bounds_survive_on_a_lite06_peer() {
		let mut h = Harness::new(Version::Lite06Wip);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(mid_group_demand()),
				true,
				Some(Timescale::default()),
			)
			.await
			.unwrap();

		let wire = h.wire();
		let mut wire = wire.as_slice();
		assert_eq!(
			lite::ControlType::decode(&mut wire, Version::Lite06Wip).unwrap(),
			lite::ControlType::Subscribe
		);
		let msg = lite::Subscribe::decode(&mut wire, Version::Lite06Wip).unwrap();
		assert_eq!((msg.start_frame, msg.end_frame), (3, Some(7)));
	}

	/// The model's exclusive end maps back to the wire's inclusive pair.
	///
	/// The two disagree deliberately (see [`Subscription::end`]), so this pins the seam:
	/// an end at the head of a group means the group below it, served whole, while one
	/// mid-group caps the frame below it. Off by one here would silently drop or
	/// duplicate a frame at every relay hop.
	#[test]
	fn wire_bounds_convert_the_exclusive_end() {
		// The whole of group 5 is the head of group 6.
		let bounds = WireBounds::new(None, Some(Position::group(6)));
		assert_eq!((bounds.end_group, bounds.end_frame), (Some(5), None));

		// Group 5 through frame 2 is the head of frame 3.
		let bounds = WireBounds::new(None, Some(Position { group: 5, frame: 3 }));
		assert_eq!((bounds.end_group, bounds.end_frame), (Some(5), Some(2)));

		// Unbounded stays unbounded.
		let bounds = WireBounds::new(None, None);
		assert_eq!((bounds.end_group, bounds.end_frame), (None, None));

		// Starts are inclusive on both sides, so they pass straight through.
		let bounds = WireBounds::new(Some(Position { group: 5, frame: 3 }), None);
		assert_eq!((bounds.start_group, bounds.start_frame), (Some(5), 3));
	}

	/// The builders produce exactly what the wire conversion expects, so an inclusive
	/// bound survives the trip out to a peer unchanged.
	#[test]
	fn wire_bounds_match_the_builders() {
		let whole = Subscription::default().with_end(Position::after_group(5));
		let bounds = WireBounds::new(whole.start, whole.end);
		assert_eq!((bounds.end_group, bounds.end_frame), (Some(5), None));

		let capped = Subscription::default().with_end(Position::after(5, 2));
		let bounds = WireBounds::new(capped.start, capped.end);
		assert_eq!((bounds.end_group, bounds.end_frame), (Some(5), Some(2)));

		let started = Subscription::default().with_start(Position { group: 5, frame: 3 });
		let bounds = WireBounds::new(started.start, started.end);
		assert_eq!((bounds.start_group, bounds.start_frame), (Some(5), 3));
	}

	/// A subscription that asks for nothing opens nothing.
	///
	/// `Position::group(0)` is the empty range: nothing sorts below it. The wire cannot
	/// say that, and the nearest thing it can say is "through group 0", which would
	/// deliver the single group the caller excluded.
	#[tokio::test]
	async fn an_empty_range_opens_no_subscription() {
		let mut h = Harness::new(Version::Lite06Wip);
		let mut sub = Sub::None;

		let empty = Subscription::default().with_end(Position::group(0));
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true, Some(Timescale::default()))
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "must not open a subscription");
		assert!(h.wire().is_empty(), "nothing reached the wire");
	}

	/// Demand collapsing to nothing cancels the upstream rather than sending a bound
	/// that means the opposite.
	#[tokio::test]
	async fn an_empty_range_cancels_a_live_subscription() {
		let mut h = Harness::new(Version::Lite06Wip);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(Subscription::default()),
				true,
				Some(Timescale::default()),
			)
			.await
			.unwrap();
		assert!(matches!(sub, Sub::Active(_)), "the first subscriber opens one");
		let established = h.wire().len();

		let empty = Subscription::default().with_end(Position::group(0));
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true, Some(Timescale::default()))
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "the upstream must be canceled");
		assert_eq!(h.wire().len(), established, "no SUBSCRIBE_UPDATE claiming group 0");
	}

	/// Bounds that meet anywhere in the track are just as empty as ones that meet at the
	/// first position.
	///
	/// The wire has no encoding for either: an exclusive end at a group head floors to
	/// the group below it, so group 5 through group 5 would go out as `start_group = 5`,
	/// `end_group = 4`, an inverted range the publisher happily parks on.
	#[tokio::test]
	async fn a_nonzero_empty_range_opens_no_subscription() {
		let mut h = Harness::new(Version::Lite06Wip);
		let mut sub = Sub::None;

		let empty = Subscription::default()
			.with_start(Position::group(5))
			.with_end(Position::group(5));
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true, Some(Timescale::default()))
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "must not open a subscription");
		assert!(h.wire().is_empty(), "nothing reached the wire");
	}

	/// Demand collapsing to an empty range mid-track cancels the upstream, the same way
	/// it does at the first position.
	#[tokio::test]
	async fn a_nonzero_empty_range_cancels_a_live_subscription() {
		let mut h = Harness::new(Version::Lite06Wip);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(Subscription::default()),
				true,
				Some(Timescale::default()),
			)
			.await
			.unwrap();
		assert!(matches!(sub, Sub::Active(_)), "the first subscriber opens one");
		let established = h.wire().len();

		let empty = Subscription::default()
			.with_start(Position::group(5))
			.with_end(Position::group(5));
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true, Some(Timescale::default()))
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "the upstream must be canceled");
		assert_eq!(h.wire().len(), established, "no SUBSCRIBE_UPDATE inverting the range");
	}

	/// Widening rounds the end outward at the last group too, keeping the range at least
	/// as wide as the request.
	///
	/// Rounding inward there would empty a range the caller asked for, and the filter in
	/// `handle_subscription` runs before the widening, so nothing downstream would catch
	/// it.
	#[tokio::test]
	async fn frame_bounds_widen_outward_at_the_last_group() {
		let h = Harness::new(Version::Lite05);

		let mut subscription = Subscription::default()
			.with_start(Position {
				group: u64::MAX,
				frame: 1,
			})
			.with_end(Position::after(u64::MAX, 5));
		h.serve.widen_frame_bounds(&mut subscription);

		assert_eq!(subscription.start, Some(Position::group(u64::MAX)));
		// Past the last group there is no position to round up to, and unbounded is the
		// wider request.
		assert_eq!(subscription.end, None);
	}

	/// The widening covers SUBSCRIBE_UPDATE too: a downstream peer asking for a frame
	/// offset must not tear down an older upstream that is already serving.
	#[tokio::test]
	async fn frame_bounds_widen_on_update() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(Subscription::default()),
				true,
				Some(Timescale::default()),
			)
			.await
			.unwrap();
		let established = h.wire().len();

		// A lite-06 subscriber downstream now wants to resume mid-group.
		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(mid_group_demand()),
				true,
				Some(Timescale::default()),
			)
			.await
			.expect("a downstream frame offset must not tear down an older upstream");

		// SUBSCRIBE_UPDATE rides the subscribe stream with no control type ahead of it.
		let wire = h.wire();
		let mut wire = &wire[established..];
		let msg = lite::SubscribeUpdate::decode(&mut wire, Version::Lite05).unwrap();
		assert_eq!((msg.start_group, msg.start_frame), (Some(5), 0));
		assert_eq!((msg.end_group, msg.end_frame), (Some(5), None));
	}

	/// A buffered SUBSCRIBE_START describes the demand its SUBSCRIBE carried, so
	/// it applies exactly while the current start matches that demand: an update
	/// that moves the start makes it stale (applying it could reopen a range the
	/// publisher no longer serves, or clamp one it still does), and an update
	/// that moves back restores it (the publisher declared that range gone and
	/// sends no replacement START).
	#[tokio::test]
	async fn buffered_start_applies_iff_demand_matches() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		let demand = |group: u64| Some(Subscription::default().with_start(Position::group(group)));
		let applies = |sub: &Sub<SinkSession>| matches!(sub, Sub::Active(active) if active.start == active.requested);

		// Establish from group 3; the peer's START is considered in flight.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true, Some(Timescale::default()))
			.await
			.unwrap();
		assert!(applies(&sub), "a fresh subscription accepts its START");

		// An end-only update leaves the start intact: the START stays valid.
		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(
					Subscription::default()
						.with_start(Position::group(3))
						.with_end(Position::group(9)),
				),
				true,
				Some(Timescale::default()),
			)
			.await
			.unwrap();
		assert!(applies(&sub), "an unmoved start keeps the START applicable");

		// The start moves: a buffered START is stale while it sits elsewhere.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(8), true, Some(Timescale::default()))
			.await
			.unwrap();
		assert!(!applies(&sub), "a moved start must invalidate a buffered START");

		// The start returns: the declaration matches the demand again, so the
		// publisher's skip (it sends no replacement START) must land.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true, Some(Timescale::default()))
			.await
			.unwrap();
		assert!(applies(&sub), "demand returning restores the START");
	}

	/// The permanent-miss floor follows the demand in every direction: an update
	/// that moves the start forward retires the skipped range (the publisher
	/// stops serving it and no fresh START says so), one that moves it backward
	/// reopens it, and dropping to the live edge clears it entirely.
	#[tokio::test]
	async fn updates_move_the_declared_floor_both_ways() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		let demand = |group: u64| Some(Subscription::default().with_start(Position::group(group)));

		// Establish from group 5: the floor tracks the request until START lands.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(5), true, Some(Timescale::default()))
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), Some(5));

		// Forward: a reader waiting in [5, 8) must fail over, not stall.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(8), true, Some(Timescale::default()))
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), Some(8));

		// Backward: the reopened range must stop being a permanent miss.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true, Some(Timescale::default()))
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), Some(3));

		// Live edge: the floor is unknown until the next declaration, so a
		// group below the stale one must not be a permanent miss.
		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(Subscription::default()),
				true,
				Some(Timescale::default()),
			)
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), None);
	}

	/// A peer that declares no identity gets attributed the origin the caller
	/// assigned it (`Client::with_peer_origin`), so every session dialing the same
	/// relay yields one recognizable hop instead of a random id per connection.
	#[tokio::test]
	async fn assigned_peer_origin_attributes_announces() {
		let session = SinkSession::new(Default::default());
		let assigned = crate::Origin::new(777).unwrap();

		let origin = origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			session,
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: Default::default(),
			peer_origin: Some(assigned),
			cost: None,
			going_away: Default::default(),
		});

		// An announce with an empty chain and no responder id: the versions that
		// carry no hop information on the wire.
		let mut routes = HashMap::new();
		let accepted = subscriber
			.start_announce(
				Path::new("room/host").to_owned(),
				crate::OriginList::new(),
				RouteCost::default(),
				0,
				None,
				&mut routes,
			)
			.unwrap();
		assert!(accepted);

		// Broadcast visibility is deferred until the executor ticks.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let hops: Vec<_> = broadcast.routes()[0].hops.iter().copied().collect();
		assert_eq!(hops, vec![assigned]);
	}

	/// A peer with no assigned identity is attributed the reserved origin 0
	/// (UNKNOWN), not a random one: a random id would look like a real identity
	/// the peer never agreed to and cannot exclude for loop detection.
	#[tokio::test]
	async fn absent_peer_origin_stamps_unknown() {
		let (mut subscriber, consumer) = restart_subscriber(SinkSession::new(Default::default()));

		let mut routes = HashMap::new();
		subscriber
			.start_announce(
				Path::new("room/host").to_owned(),
				crate::OriginList::new(),
				RouteCost::default(),
				0,
				None,
				&mut routes,
			)
			.unwrap();
		tokio::time::sleep(Duration::from_millis(1)).await;

		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let hops: Vec<_> = broadcast.routes()[0].hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Origin::UNKNOWN]);
	}

	fn restart_subscriber(session: SinkSession) -> (Subscriber<SinkSession>, crate::origin::Consumer) {
		let origin = origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let subscriber = Subscriber::new(SubscriberConfig {
			session,
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: Default::default(),
			cost: None,
			peer_origin: None,
			going_away: Default::default(),
		});
		(subscriber, consumer)
	}

	/// An UNKNOWN (0) first hop identifies nothing, so a restart advertising
	/// UNKNOWN again may be a different publisher entirely: the old broadcast must
	/// be replaced, not updated in place. Regression: plain `==` on the publisher
	/// id treated 0 == 0 as content-continuous and spliced unrelated broadcasts.
	#[tokio::test]
	async fn unknown_publisher_restart_replaces() {
		let (mut subscriber, consumer) = restart_subscriber(SinkSession::new(Default::default()));

		let mut routes = HashMap::new();
		let path = Path::new("room/host").to_owned();
		subscriber
			.start_announce(
				path.clone(),
				crate::OriginList::new(),
				RouteCost::default(),
				0,
				Some(crate::Origin::UNKNOWN),
				&mut routes,
			)
			.unwrap();
		tokio::time::sleep(Duration::from_millis(1)).await;
		let before = consumer.get_broadcast("room/host").unwrap();

		subscriber
			.restart_announce(
				path,
				crate::OriginList::new(),
				RouteCost::default(),
				0,
				Some(crate::Origin::UNKNOWN),
				&mut routes,
			)
			.unwrap();
		tokio::time::sleep(Duration::from_millis(1)).await;

		assert!(before.is_closed(), "an UNKNOWN restart must replace the broadcast");
		assert!(
			consumer.get_broadcast("room/host").is_some(),
			"the fresh source re-attaches"
		);
	}

	/// The counterpart: a real (non-zero) publisher id restarting is a route
	/// change for the same content, so the broadcast survives in place.
	#[tokio::test]
	async fn known_publisher_restart_updates_in_place() {
		let (mut subscriber, consumer) = restart_subscriber(SinkSession::new(Default::default()));
		let publisher = crate::Origin::new(7).unwrap();

		let mut routes = HashMap::new();
		let path = Path::new("room/host").to_owned();
		subscriber
			.start_announce(
				path.clone(),
				crate::OriginList::new(),
				RouteCost::default(),
				0,
				Some(publisher),
				&mut routes,
			)
			.unwrap();
		tokio::time::sleep(Duration::from_millis(1)).await;
		let before = consumer.get_broadcast("room/host").unwrap();

		subscriber
			.restart_announce(
				path,
				crate::OriginList::new(),
				RouteCost(5),
				0,
				Some(publisher),
				&mut routes,
			)
			.unwrap();
		tokio::time::sleep(Duration::from_millis(1)).await;

		assert!(
			!before.is_closed(),
			"a known publisher restart keeps the broadcast live"
		);
	}

	/// An announce stream that dies without an explicit `ended` closes the broadcast
	/// as promptly as an explicit retraction: a route into a dead session must not
	/// stay announced, so viewers observe the loss instead of a stale route.
	///
	/// This falls out of `routes` being a local whose `AnnouncedRoute` guards drop,
	/// which is exactly what makes it worth pinning: a refactor that hoisted the map
	/// to the session (outliving the stream) would leak the announcement instead.
	#[tokio::test(start_paused = true)]
	async fn a_lost_announce_stream_closes_the_broadcast() {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			session: SinkSession::new(Default::default()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: Default::default(),
			cost: None,
			peer_origin: None,
			going_away: Default::default(),
		});

		let path = Path::new("room/host").to_owned();
		let hops = crate::OriginList::try_from(vec![crate::Origin::new(7).unwrap()]).unwrap();
		let mut routes = HashMap::new();
		subscriber
			.start_announce(path.clone(), hops, RouteCost(0), 1, None, &mut routes)
			.unwrap();
		tokio::time::sleep(Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_some());

		// The stream ends without retracting anything: the map dies with it and the
		// broadcast closes.
		drop(routes);
		tokio::time::sleep(Duration::from_millis(1)).await;
		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"an abnormal stream loss must close the broadcast",
		);

		// An explicit retraction closes it the same way.
		let hops = crate::OriginList::try_from(vec![crate::Origin::new(7).unwrap()]).unwrap();
		let mut routes = HashMap::new();
		subscriber
			.start_announce(path.clone(), hops, RouteCost(0), 1, None, &mut routes)
			.unwrap();
		tokio::time::sleep(Duration::from_millis(1)).await;
		routes.remove(&path).expect("announced").finish();
		tokio::time::sleep(Duration::from_millis(1)).await;
		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"an explicit retraction must close the broadcast",
		);
	}
}

/// The four wire fields a subscription's half-open range encodes to.
///
/// The inverse of the publisher's `Bounds::positions`: the model carries whole
/// positions with an exclusive end, while the wire splits each bound into a group and a
/// frame and states both ends inclusive. The range must be non-empty, since an inclusive
/// end has nothing to say below the first position it excludes.
struct WireBounds {
	start_group: Option<u64>,
	start_frame: u64,
	end_group: Option<u64>,
	end_frame: Option<u64>,
}

impl WireBounds {
	fn new(start: Option<Position>, end: Option<Position>) -> Self {
		// An empty range has no wire encoding: flooring its end below asks for the
		// position it excludes, or inverts the range. `handle_subscription` drops such a
		// subscription instead. An absent start is the live edge, so it only makes the
		// range empty when the end sits at the very first position.
		debug_assert!(
			end.is_none_or(|end| end > start.unwrap_or_default()),
			"an empty range cannot be encoded; it should have been dropped as no demand"
		);

		let (end_group, end_frame) = match end {
			// An exclusive end at the head of a group means the group below it is the
			// last one, and it is served whole.
			Some(end) if end.frame == 0 => (Some(end.group.saturating_sub(1)), None),
			Some(end) => (Some(end.group), Some(end.frame - 1)),
			None => (None, None),
		};

		Self {
			start_group: start.map(|start| start.group),
			start_frame: start.map_or(0, |start| start.frame),
			end_group,
			end_frame,
		}
	}
}

/// The at-most-one live upstream subscription: its control stream plus the params
/// echoed in every SUBSCRIBE_UPDATE.
struct SubStream<S: crate::transport::poll::Session> {
	stream: Stream<S, Version>,
	id: u64,
	/// Original SUBSCRIBE params, echoed in every SUBSCRIBE_UPDATE; refreshed as the
	/// downstream aggregate changes.
	ordered: bool,
	latency_max: Duration,
	start: Option<Position>,
	priority: u8,
	/// The start the SUBSCRIBE itself carried, fixed for the stream's life. A
	/// SUBSCRIBE_START describes this demand and no fresh one follows an update,
	/// so a buffered START only applies while `start` still equals it: while the
	/// start sits elsewhere the request-tracked floor stands instead, and demand
	/// returning here makes the declaration valid again.
	requested: Option<Position>,
}

enum Sub<S: crate::transport::poll::Session> {
	None,
	Active(SubStream<S>),
}

/// The source created for one received announce, remembering the publisher
/// identity (the first hop of the reconstructed chain) so a restart can tell an
/// alternate route to the same broadcast from a brand-new broadcast.
struct AnnouncedRoute {
	source: crate::model::broadcast::SourceGuard,
	publisher: crate::Origin,
}

impl AnnouncedRoute {
	fn new(source: crate::broadcast::Producer, publisher: crate::Origin) -> Self {
		Self {
			source: crate::model::broadcast::SourceGuard::new(source),
			publisher,
		}
	}

	/// The peer deliberately retracted the path: finish the source so the origin
	/// detaches it immediately (unannouncing downstream if it was the last).
	fn finish(self) {
		self.source.finish();
	}

	/// Update the source's advertised route in place (a restart on the same
	/// publisher).
	fn set_route(&mut self, route: crate::broadcast::Route) {
		self.source.set_route(route);
	}
}

/// How a [`TrackServe`] run ends.
enum Teardown {
	/// The upstream FIN'd: the track is over for good.
	Finished,
	/// The route or session failed: abort the track so the origin re-splices it
	/// from another source.
	GiveBack(Error),
	/// The origin released this copy: nobody is reading it, so drop it (and the
	/// `TRACK_INFO` behind it) rather than holding the state for a reader that
	/// may never come back.
	Released,
}

/// Serves one requested track for a relay: owns this session's copy of the
/// track (spliced into the origin's logical track), driving the single upstream
/// subscription (opened lazily on the first downstream subscriber, canceled when
/// the last one leaves) concurrently with any number of one-shot fetches.
#[derive(Clone)]
struct TrackServe<S: crate::transport::poll::Session> {
	subscriber: Subscriber<S>,
	path: PathOwned,
	name: String,
}

impl<S: crate::transport::poll::Session> TrackServe<S> {
	fn widen_frame_bounds(&self, subscription: &mut Subscription) {
		if self.subscriber.version.has_frame_bounds() {
			return;
		}

		// Round both bounds outward to the enclosing group, so the peer sends at least
		// what was asked for and never less. Rounding the end down instead would be able
		// to empty a non-empty range, which has no wire encoding at all.
		let start = subscription.start.map(|start| Position::group(start.group));
		let end = subscription.end.and_then(|end| match end.frame {
			0 => Some(end),
			// Past the last group there is no position to round up to, and unbounded is
			// the wider request.
			_ => Position::after_group(end.group),
		});

		if (start, end) != (subscription.start, subscription.end) {
			tracing::debug!(
				track = %self.name,
				version = ?self.subscriber.version,
				"widening frame bounds to whole groups for an older peer"
			);
		}
		subscription.start = start;
		subscription.end = end;
	}

	/// Apply a subscription-demand change: hand back an [`Establish`] to open the
	/// upstream SUBSCRIBE on the first subscriber, buffer a SUBSCRIBE_UPDATE while
	/// live (the caller flushes), or cancel outright when the last one leaves.
	fn begin_subscription(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub<S>,
		pref: Option<Subscription>,
		supports_update: bool,
		timescale: Option<Timescale>,
	) -> Result<Begin<S>, Error> {
		// An empty half-open range asks for nothing, and the wire cannot say that: its
		// bounds are inclusive, so the nearest encoding either hands back the position
		// the caller excluded or inverts the range outright once the two bounds meet. No
		// demand at all is the faithful translation. `resume::slice` reaches this on its
		// own: a subscriber resuming exactly at a segment's cap owes that segment nothing.
		// An absent start is the live edge, wherever that lands, so the only end that is
		// certainly empty is the very first position, which is what `Position::default()`
		// stands in for.
		let pref = pref.filter(|sub| sub.end.is_none_or(|end| end > sub.start.unwrap_or_default()));

		match pref {
			Some(mut subscription) => {
				self.widen_frame_bounds(&mut subscription);
				match sub {
					Sub::None => {
						// Open an upstream SUBSCRIBE for the first subscriber.
						Ok(Begin::Establish(self.prepare_establish(
							producer,
							subscription,
							timescale,
						)))
					}
					Sub::Active(active) => {
						// Downstream preferences changed: forward them upstream as a
						// SUBSCRIBE_UPDATE (Lite03+ only; older peers can't carry one).
						let start_moved = active.start != subscription.start;
						active.priority = subscription.priority;
						active.ordered = subscription.ordered;
						active.latency_max = subscription.latency.max;
						active.start = subscription.start;
						if supports_update {
							// The floor follows the requested start, in both directions:
							// moving below a declared SUBSCRIBE_START reopens those groups
							// (the peer may serve them now), moving forward retires the
							// skipped range (the peer stops serving it, and no fresh START
							// will say so), and dropping to the live edge clears it until
							// the next declaration. A buffered START re-applies only if the
							// start returns to the demand that produced it (see
							// `SubStream::requested`).
							if start_moved {
								let _ = producer.start_at(active.start.map(|start| start.group));
							}
							buffer_update(active, subscription.end)?;
						}
						Ok(Begin::None)
					}
				}
			}
			None => {
				// Last subscriber left: cancel the upstream subscription outright. An
				// idle subscription still streams every group into a cache nobody
				// reads, and the upstream counts it as a live viewer of the broadcast.
				// A returning subscriber re-establishes from the current demand.
				if let Sub::Active(active) = sub {
					self.subscriber.subscribes.lock().remove(&active.id);
					let _ = active.stream.writer.finish();
					tracing::info!(track = %self.name, "subscribe canceled (idle)");
					*sub = Sub::None;
				}
				Ok(Begin::None)
			}
		}
	}

	/// Allocate the id, set the demand floor, and register the subscription, so the
	/// returned [`Establish`] can put the SUBSCRIBE on the wire.
	///
	/// Registration happens here, before any of it reaches the transport: `id` is
	/// live the moment the peer reads it, and a publisher may serve its first group
	/// immediately, so a late insert races the group stream (a group whose id isn't
	/// in the map yet is dropped, stalling the track forever). The caller
	/// deregisters `id` if the establish fails.
	fn prepare_establish(
		&self,
		producer: &mut track::Producer,
		subscription: Subscription,
		timescale: Option<Timescale>,
	) -> Establish<S> {
		let id = self.subscriber.next_id.fetch_add(1, atomic::Ordering::Relaxed);

		// Both halves of each bound come from the same position, so a frame can never
		// reach the wire without the group it counts from (which the peer would reject).
		// The floor tracks the requested start until this subscription's own
		// SUBSCRIBE_START refines it: the peer never serves below the request, a
		// previous subscription's declaration must not outlive its demand, and
		// live-edge demand (None) starts with no floor at all.
		let _ = producer.start_at(subscription.start.map(|start| start.group));

		tracing::info!(id, broadcast = %self.subscriber.log_path(&self.path), track = %self.name, "subscribe started");

		self.subscriber.subscribes.lock().insert(
			id,
			TrackEntry {
				producer: producer.clone(),
				timescale,
			},
		);

		let session = self.subscriber.session.clone();
		Establish {
			serve: self.clone(),
			closed: session.clone(),
			session,
			id,
			subscription,
			state: EstablishState::Open,
		}
	}

	/// Test shim: drive the upstream SUBSCRIBE open like the old `establish`.
	#[cfg(test)]
	async fn establish(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub<S>,
		subscription: Subscription,
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		let mut est = Box::new(self.prepare_establish(producer, subscription, timescale));
		let id = est.id;
		match kio::wait(move |waiter| est.poll(waiter)).await {
			Ok(active) => {
				*sub = Sub::Active(active);
				Ok(())
			}
			Err(err) => {
				self.subscriber.subscribes.lock().remove(&id);
				Err(err)
			}
		}
	}

	/// Test shim: apply one demand change like the old `handle_subscription`,
	/// driving the establish (or the update flush) to completion inline.
	#[cfg(test)]
	async fn handle_subscription(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub<S>,
		pref: Option<Subscription>,
		supports_update: bool,
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		match self.begin_subscription(producer, sub, pref, supports_update, timescale)? {
			Begin::Establish(est) => {
				let mut est = Box::new(est);
				let id = est.id;
				match kio::wait(move |waiter| est.poll(waiter)).await {
					Ok(active) => *sub = Sub::Active(active),
					Err(err) => {
						self.subscriber.subscribes.lock().remove(&id);
						return Err(err);
					}
				}
			}
			Begin::None => {
				if let Sub::Active(active) = sub {
					std::future::poll_fn(|cx| active.stream.writer.poll_flush(cx)).await?;
				}
			}
		}
		Ok(())
	}
}

/// What a demand change asks the serve loop to do next.
// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum Begin<S: crate::transport::poll::Session> {
	/// Nothing further: the update (if any) sits in the active stream's write
	/// buffer, flushed by the loop.
	None,
	/// Open an upstream SUBSCRIBE for the first subscriber.
	Establish(Establish<S>),
}

/// Buffer a SUBSCRIBE_UPDATE echoing the current params, varying only the end
/// bound. The caller flushes.
fn buffer_update<S: crate::transport::poll::Session>(
	active: &mut SubStream<S>,
	end: Option<Position>,
) -> Result<(), Error> {
	let bounds = WireBounds::new(active.start, end);
	active.stream.writer.buffer(&lite::SubscribeUpdate {
		priority: active.priority,
		ordered: active.ordered,
		latency_max: active.latency_max,
		start_group: bounds.start_group,
		end_group: bounds.end_group,
		start_frame: bounds.start_frame,
		end_frame: bounds.end_frame,
	})
}

/// Opens the upstream SUBSCRIBE control stream: send the request, then (pre
/// lite-05) wait for the SUBSCRIBE_OK. Resolves with the live [`SubStream`];
/// the caller deregisters the id on failure.
struct Establish<S: crate::transport::poll::Session> {
	serve: TrackServe<S>,
	session: S,
	// A dedicated close-watch handle for the SUBSCRIBE_OK wait.
	closed: S,
	id: u64,
	subscription: Subscription,
	state: EstablishState<S>,
}

enum EstablishState<S: crate::transport::poll::Session> {
	Open,
	Send {
		stream: Stream<S, Version>,
	},
	/// Older drafts: the first SUBSCRIBE_OK confirms it. Bail if the session
	/// dies meanwhile; a dying route hands the assignment back through the
	/// serve loop's teardown instead.
	WaitOk {
		stream: Stream<S, Version>,
	},
}

impl<S: crate::transport::poll::Session> Establish<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<SubStream<S>, Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				EstablishState::Open => {
					// A peer that sent GOAWAY told us to stop opening streams.
					self.serve.subscriber.check_going_away()?;
					let mut stream = ready!(Stream::poll_open(
						&mut self.session,
						self.serve.subscriber.version,
						&mut cx
					))?;

					let bounds = WireBounds::new(self.subscription.start, self.subscription.end);
					let msg = lite::Subscribe {
						id: self.id,
						broadcast: self.serve.path.as_path(),
						track: self.serve.name.as_str().into(),
						priority: self.subscription.priority,
						ordered: self.subscription.ordered,
						latency_max: self.subscription.latency.max,
						start_group: bounds.start_group,
						end_group: bounds.end_group,
						start_frame: bounds.start_frame,
						end_frame: bounds.end_frame,
					};
					stream.writer.buffer(&lite::ControlType::Subscribe)?;
					stream.writer.buffer(&msg)?;
					self.state = EstablishState::Send { stream };
				}
				EstablishState::Send { stream } => {
					ready!(stream.writer.poll_flush(&mut cx))?;
					let EstablishState::Send { stream } = std::mem::replace(&mut self.state, EstablishState::Open)
					else {
						unreachable!()
					};
					if !self.serve.subscriber.version.has_track_stream() {
						self.state = EstablishState::WaitOk { stream };
						continue;
					}
					return Poll::Ready(Ok(self.activate(stream)));
				}
				EstablishState::WaitOk { stream } => {
					if self.closed.poll_closed(&mut cx).is_ready() {
						return Poll::Ready(Err(Error::Dropped));
					}
					let resp = ready!(stream.reader.poll_decode::<lite::SubscribeResponse>(&mut cx))?;
					if !matches!(resp, lite::SubscribeResponse::Ok(_)) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					let EstablishState::WaitOk { stream } = std::mem::replace(&mut self.state, EstablishState::Open)
					else {
						unreachable!()
					};
					return Poll::Ready(Ok(self.activate(stream)));
				}
			}
		}
	}

	fn activate(&self, stream: Stream<S, Version>) -> SubStream<S> {
		SubStream {
			stream,
			id: self.id,
			ordered: self.subscription.ordered,
			latency_max: self.subscription.latency.max,
			start: self.subscription.start,
			priority: self.subscription.priority,
			requested: self.subscription.start,
		}
	}
}

/// Drives one [`TrackServe`]: the TRACK_INFO fetch, then the serve loop, then
/// the teardown that decides how the origin sees this copy end.
struct TrackServeRun<S: crate::transport::poll::Session> {
	serve: TrackServe<S>,
	state: TrackRunState<S>,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum TrackRunState<S: crate::transport::poll::Session> {
	/// Lite05+ learns the track's immutable properties once, up front, via a
	/// TRACK stream. The timescale then flows into every SUBSCRIBE and FETCH
	/// without a per-response header.
	Info {
		request: Option<track::Request>,
		info: TrackInfoFetch<S>,
	},
	Serve(ServeLoop<S>),
	Done,
}

impl<S: crate::transport::poll::Session> TrackServeRun<S> {
	fn new(serve: TrackServe<S>, request: track::Request) -> Self {
		let state = if serve.subscriber.version.has_track_stream() {
			TrackRunState::Info {
				request: Some(request),
				info: TrackInfoFetch::new(&serve),
			}
		} else {
			// No TRACK stream, so the publisher's retention window never reaches us:
			// the accepting side picks it (see `origin::Info::latency_default`).
			let info = track::Info::default().with_latency_max(serve.subscriber.origin.latency_default());
			TrackRunState::Serve(ServeLoop::new(&serve, request, info, None))
		};
		Self { serve, state }
	}
}

impl<S: crate::transport::poll::Session> Machine for TrackServeRun<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			match &mut self.state {
				TrackRunState::Info { request, info } => {
					let res = ready!(info.poll_fetch(&self.serve, waiter));
					let request = request.take().expect("request pending");
					match res {
						Ok(info) => {
							// Lite05 carries per-frame timestamps on the wire at this scale;
							// `Some` tells the ingest to decode them instead of stamping
							// local receive time.
							let timescale = Some(info.timescale);
							self.state = TrackRunState::Serve(ServeLoop::new(&self.serve, request, info, timescale));
						}
						Err(err) => {
							tracing::warn!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, %err, "track info failed");
							// Rejecting the request lets the origin retry (bounded) on
							// another source; waiting subscribers stall rather than error
							// meanwhile.
							request.reject(err);
							self.state = TrackRunState::Done;
							return Poll::Ready(());
						}
					}
				}
				TrackRunState::Serve(serve_loop) => {
					let teardown = ready!(serve_loop.poll(&self.serve, waiter));
					let TrackRunState::Serve(mut serve_loop) = std::mem::replace(&mut self.state, TrackRunState::Done)
					else {
						unreachable!()
					};

					if let Sub::Active(active) = &mut serve_loop.sub {
						self.serve.subscriber.subscribes.lock().remove(&active.id);
						let _ = active.stream.writer.finish();
					}

					match teardown {
						// The upstream ended the track for good; the origin observes the
						// completed copy and finishes the logical track.
						Teardown::Finished => {
							let _ = serve_loop.serving.finish();
						}
						Teardown::GiveBack(err) => {
							// Mark this copy dead: subscribers stall while the origin
							// re-splices the track from the next source.
							let _ = serve_loop.serving.abort(err);
						}
						Teardown::Released => {
							// A deliberate end with no reader to observe it, which also
							// drops the cached groups. The origin re-requests the track from
							// this session if one comes back.
							let _ = serve_loop.serving.abort(Error::Cancel);
						}
					}
					return Poll::Ready(());
				}
				TrackRunState::Done => return Poll::Ready(()),
			}
		}
	}
}

/// Opens a TRACK stream, reads the single TRACK_INFO, and maps it to the
/// model's [`track::Info`]. Lite05+ only. Bails if the session dies meanwhile.
struct TrackInfoFetch<S: crate::transport::poll::Session> {
	session: S,
	// A dedicated close-watch handle for the read.
	closed: S,
	state: TrackInfoState<S>,
}

enum TrackInfoState<S: crate::transport::poll::Session> {
	Open,
	Send { stream: Stream<S, Version> },
	Read { stream: Stream<S, Version> },
}

impl<S: crate::transport::poll::Session> TrackInfoFetch<S> {
	fn new(serve: &TrackServe<S>) -> Self {
		let session = serve.subscriber.session.clone();
		Self {
			closed: session.clone(),
			session,
			state: TrackInfoState::Open,
		}
	}

	fn poll_fetch(&mut self, serve: &TrackServe<S>, waiter: &kio::Waiter) -> Poll<Result<track::Info, Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				TrackInfoState::Open => {
					serve.subscriber.check_going_away()?;
					let mut stream = ready!(Stream::poll_open(&mut self.session, serve.subscriber.version, &mut cx))?;
					stream.writer.buffer(&lite::ControlType::Track)?;
					stream.writer.buffer(&lite::Track {
						broadcast: serve.path.as_path(),
						track: serve.name.as_str().into(),
					})?;
					self.state = TrackInfoState::Send { stream };
				}
				TrackInfoState::Send { stream } => {
					ready!(stream.writer.poll_flush(&mut cx))?;
					let TrackInfoState::Send { stream } = std::mem::replace(&mut self.state, TrackInfoState::Open)
					else {
						unreachable!()
					};
					self.state = TrackInfoState::Read { stream };
				}
				TrackInfoState::Read { stream } => {
					if self.closed.poll_closed(&mut cx).is_ready() {
						return Poll::Ready(Err(Error::Dropped));
					}
					let info = ready!(stream.reader.poll_decode::<lite::TrackInfo>(&mut cx))?;
					// The publisher FINs after TRACK_INFO; FIN our side too and let the
					// stream drop.
					let _ = stream.writer.finish();

					// Publisher Max Latency rides on the wire, so the local retention
					// window matches what the upstream advertises (relays re-serve with
					// the same bound). `broadcast` is left at its default here;
					// `track::Request::accept` stamps the track's real broadcast.
					let model = track::Info::default()
						.with_timescale(info.timescale)
						.with_latency_max(info.latency_max)
						.with_priority(info.priority)
						.with_ordered(info.ordered);
					return Poll::Ready(Ok(model));
				}
			}
		}
	}
}

/// The serve loop proper: owns this session's copy of the track (spliced into
/// the origin's logical track), driving the single upstream subscription
/// (opened lazily on the first downstream subscriber, canceled when the last
/// one leaves) concurrently with any number of one-shot fetches.
struct ServeLoop<S: crate::transport::poll::Session> {
	/// This session's copy, accepted with the resolved info. The origin splices
	/// it into the logical track; demand from the logical subscribers arrives
	/// through the producer's aggregate, sliced to this segment's bounds
	/// (including the resume floor after a source change).
	serving: track::Producer,
	/// Serve on-demand fetches of uncached groups from this session.
	dynamic: track::Dynamic,
	sub: Sub<S>,
	fetches: PollSet<FetchServeRun<S>>,
	// A dedicated close-watch handle for the session-died arm.
	closed: S,
	// SUBSCRIBE_UPDATE only exists on Lite03+, so older peers can't carry a
	// preference change to an established subscription.
	supports_update: bool,
	supports_fetch: bool,
	timescale: Option<Timescale>,
	mode: ServeMode<S>,
}

// A state machine's enum is its storage: one transient instance per stream, so the
// big variant is the working state, not padding held in bulk.
#[allow(clippy::large_enum_variant)]
enum ServeMode<S: crate::transport::poll::Session> {
	/// Selecting the next event.
	Select,
	/// Driving an upstream SUBSCRIBE open. The demand arms wait meanwhile,
	/// exactly like the old inline await.
	Establish(Establish<S>),
}

impl<S: crate::transport::poll::Session> ServeLoop<S> {
	fn new(serve: &TrackServe<S>, request: track::Request, info: track::Info, timescale: Option<Timescale>) -> Self {
		let serving = request.accept(info);
		let dynamic = serving.dynamic();
		Self {
			serving,
			dynamic,
			sub: Sub::None,
			fetches: PollSet::new(),
			closed: serve.subscriber.session.clone(),
			supports_update: !matches!(serve.subscriber.version, Version::Lite01 | Version::Lite02),
			supports_fetch: serve.subscriber.version.has_track_stream(),
			timescale,
			mode: ServeMode::Select,
		}
	}

	fn poll(&mut self, serve: &TrackServe<S>, waiter: &kio::Waiter) -> Poll<Teardown> {
		loop {
			match &mut self.mode {
				ServeMode::Establish(est) => {
					let res = ready!(est.poll(waiter));
					let id = est.id;
					self.mode = ServeMode::Select;
					match res {
						Ok(active) => self.sub = Sub::Active(active),
						Err(err) => {
							// Opening the upstream failed (usually the session dying): hand
							// the track back for another route to resume.
							serve.subscriber.subscribes.lock().remove(&id);
							return Poll::Ready(Teardown::GiveBack(err));
						}
					}
				}
				ServeMode::Select => {
					let mut cx = std::task::Context::from_waker(waiter.waker());

					// Deliver any buffered SUBSCRIBE_UPDATE before selecting, so the
					// demand that produced it is on the wire.
					if let Sub::Active(active) = &mut self.sub {
						match active.stream.writer.poll_flush(&mut cx) {
							Poll::Ready(Ok(())) => {}
							Poll::Ready(Err(err)) => {
								// The stream is broken; drop it (the writer resets) and
								// hand the track back.
								serve.subscriber.subscribes.lock().remove(&active.id);
								self.sub = Sub::None;
								return Poll::Ready(Teardown::GiveBack(err));
							}
							Poll::Pending => return Poll::Pending,
						}
					}

					// Biased: demand first, then completions, then closures.

					// (1) Track demand: a fetch, a subscription change, or the origin
					// handing the track to another route.

					// A fetch is cheap and one-shot, so serve it ahead of subscription churn.
					match self.dynamic.poll_requested_group(waiter) {
						Poll::Ready(Ok(req)) => {
							if self.supports_fetch {
								self.fetches
									.push(FetchServeRun::new(serve.clone(), req, self.timescale));
							} else {
								req.reject(Error::Version);
							}
							continue;
						}
						// Our own producer is alive (we hold it); treat as terminal anyway.
						Poll::Ready(Err(_)) => return Poll::Ready(Teardown::GiveBack(Error::Dropped)),
						Poll::Pending => {}
					}
					match self.serving.poll_subscription_changed(waiter) {
						Poll::Ready(Ok(pref)) => {
							match serve.begin_subscription(
								&mut self.serving,
								&mut self.sub,
								pref,
								self.supports_update,
								self.timescale,
							) {
								Ok(Begin::Establish(est)) => self.mode = ServeMode::Establish(est),
								Ok(Begin::None) => {}
								// Updating the upstream failed: hand the track back for
								// another route to resume.
								Err(err) => return Poll::Ready(Teardown::GiveBack(err)),
							}
							continue;
						}
						Poll::Ready(Err(_)) => return Poll::Ready(Teardown::GiveBack(Error::Dropped)),
						Poll::Pending => {}
					}

					// (2) In-flight fetches; completions just retire.
					let _ = self.fetches.poll(waiter);

					// (3) Nobody reads this copy anymore: the origin released it after its
					// idle linger, so drop it instead of holding the track state (and its
					// TRACK_INFO) for a reader that may never return. In-flight fetches
					// keep it alive: work already accepted still gets finished.
					if self.fetches.is_empty() && self.serving.poll_unused(waiter).is_ready() {
						tracing::debug!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "track released (idle)");
						return Poll::Ready(Teardown::Released);
					}

					// (4) The upstream subscribe stream closed, or carried a START/END/DROP.
					// Partial message bytes persist in the reader's buffer across turns.
					if let Sub::Active(active) = &mut self.sub
						&& let Poll::Ready(res) = active
							.stream
							.reader
							.poll_decode_maybe::<lite::SubscribeResponse>(&mut cx)
					{
						match res {
							Ok(Some(msg)) => {
								match &msg {
									// SUBSCRIBE_END declares the track's exclusive final
									// sequence, which may arrive while trailing groups are
									// still in flight. Record it on this segment's producer so
									// consumers learn the boundary early; the later stream FIN
									// then finds the track already finished.
									lite::SubscribeResponse::End(end) => {
										// finish_at rejects a boundary at or below the live
										// edge, which is what a peer sending an inclusive bound
										// looks like once the final group has already arrived.
										// Don't abort: the stream FIN still finishes the track,
										// so this only costs the early boundary. Warn anyway,
										// since it's our only signal that a peer disagrees
										// about the encoding.
										if let Err(err) = self.serving.finish_at(end.group) {
											tracing::warn!(track = %serve.name, group = end.group, %err, "invalid subscribe end");
										}
									}
									// SUBSCRIBE_START names the first group this feed serves:
									// the publisher skipped everything below it (e.g. it could
									// not serve the requested frame). Record it as a drop
									// signal, so a spliced reader waiting on a skipped group
									// fails over instead of stalling on a live route.
									lite::SubscribeResponse::Start(start) => {
										// A START describes the demand the SUBSCRIBE carried.
										// It applies only while the current start still matches
										// that demand (updates get no fresh START, so an update
										// that moved the start makes it stale, and one that
										// moved back restores it); elsewhere the
										// request-tracked floor stands rather than a guess.
										if active.start == active.requested {
											let _ = self.serving.start_at(start.group);
										}
									}
									// OK/DROP just resolve the range (the producer already
									// orders groups).
									_ => tracing::debug!(track = %serve.name, ?msg, "subscribe response"),
								}
								continue;
							}
							Ok(None) => {
								tracing::info!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "subscribe complete");
								// Upstream FIN'd the subscription: the publisher only FINs
								// once the track's final sequence is known and delivered, so
								// the logical track is over for good (bounded downstream
								// demand alone never FINs; the publisher parks, since a cap
								// can be raised).
								return Poll::Ready(Teardown::Finished);
							}
							Err(err) => {
								tracing::warn!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, %err, "subscribe error");
								return Poll::Ready(Teardown::GiveBack(err));
							}
						}
					}

					// (5) The session died: hand the track back for another route.
					if self.closed.poll_closed(&mut cx).is_ready() {
						return Poll::Ready(Teardown::GiveBack(Error::Dropped));
					}

					return Poll::Pending;
				}
			}
		}
	}
}

/// Serves one downstream fetch end-to-end on its own bidi stream: send FETCH,
/// then fill the group from the bare FRAME messages that follow. The timescale
/// comes from this track's TRACK_INFO (already known), and the group sequence
/// is implicit from the request.
struct FetchServeRun<S: crate::transport::poll::Session> {
	serve: TrackServe<S>,
	session: S,
	timescale: Option<Timescale>,
	group: u64,
	state: FetchRunState<S>,
}

enum FetchRunState<S: crate::transport::poll::Session> {
	Open {
		request: Option<track::GroupRequest>,
	},
	Send {
		request: Option<track::GroupRequest>,
		stream: Stream<S, Version>,
		frame_start: u64,
	},
	Ingest {
		stream: Stream<S, Version>,
		producer: group::Producer,
		ingest: FrameIngest,
	},
	Done,
}

impl<S: crate::transport::poll::Session> FetchServeRun<S> {
	fn new(serve: TrackServe<S>, request: track::GroupRequest, timescale: Option<Timescale>) -> Self {
		let session = serve.subscriber.session.clone();
		let group = request.sequence();
		Self {
			serve,
			session,
			timescale,
			group,
			state: FetchRunState::Open { request: Some(request) },
		}
	}
}

impl<S: crate::transport::poll::Session> Machine for FetchServeRun<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				FetchRunState::Open { request } => {
					tracing::info!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, group = self.group, "fetch started");

					// A peer that sent GOAWAY told us to stop opening streams on this session.
					if self.serve.subscriber.going_away.is_set() {
						request.take().expect("request pending").reject(Error::GoingAway);
						self.state = FetchRunState::Done;
						return Poll::Ready(());
					}

					let mut stream = match ready!(Stream::poll_open(
						&mut self.session,
						self.serve.subscriber.version,
						&mut cx
					)) {
						Ok(stream) => stream,
						Err(err) => {
							tracing::warn!(track = %self.serve.name, %err, "fetch stream open failed");
							request.take().expect("request pending").reject(err);
							self.state = FetchRunState::Done;
							return Poll::Ready(());
						}
					};

					let request = request.take().expect("request pending");

					// A peer that predates lite-06 addresses whole groups only, so ask for
					// the whole group and number the response from 0. The wider group still
					// covers what the caller asked for (`fetch_group` positions their own
					// consumer), and is more reusable in the cache than the tail would have
					// been. Asking for the offset anyway would fail to encode and reject a
					// fetch we can serve.
					let frame_start = match self.serve.subscriber.version.has_frame_bounds() {
						true => request.frame_start(),
						false => 0,
					};

					let msg = lite::Fetch {
						broadcast: self.serve.path.as_path(),
						track: self.serve.name.as_str().into(),
						priority: request.priority(),
						group: self.group,
						start_frame: frame_start,
						// Always through the end of the group: a fetch that stopped short
						// would cache a group indistinguishable from a complete one. A
						// downstream cap is applied when serving, not when fetching.
						end_frame: None,
					};
					let buffered = stream
						.writer
						.buffer(&lite::ControlType::Fetch)
						.and_then(|()| stream.writer.buffer(&msg));
					if let Err(err) = buffered {
						stream.writer.abort(&err);
						request.reject(err);
						self.state = FetchRunState::Done;
						return Poll::Ready(());
					}
					self.state = FetchRunState::Send {
						request: Some(request),
						stream,
						frame_start,
					};
				}
				FetchRunState::Send { stream, .. } => {
					if let Err(err) = ready!(stream.writer.poll_flush(&mut cx)) {
						let FetchRunState::Send { request, stream, .. } =
							std::mem::replace(&mut self.state, FetchRunState::Done)
						else {
							unreachable!()
						};
						stream.writer.abort(&err);
						request.expect("request pending").reject(err);
						return Poll::Ready(());
					}
					let FetchRunState::Send {
						request,
						stream,
						frame_start,
					} = std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					let request = request.expect("request pending");

					// Make the group available (resolving the downstream fetch) and fill
					// it. The track::Info only takes effect if the track isn't accepted yet
					// (a fetch with no live subscription); otherwise the group inherits the
					// accepted timescale. Relay-served FETCH is lite-05+, so `timescale` is
					// `Some`; fall back to the default scale defensively rather than
					// panicking.
					let group_info = track::Info::default()
						.with_timescale(self.timescale.unwrap_or_default())
						.with_latency_max(self.serve.subscriber.origin.latency_default());
					let mut producer = match request.accept(group_info) {
						Ok(producer) => producer,
						Err(err) => {
							// Already served (a concurrent fetch) or the track closed.
							tracing::debug!(track = %self.serve.name, group = self.group, %err, "fetch not served");
							stream.writer.abort(&err);
							return Poll::Ready(());
						}
					};

					// The response starts at the frame we asked for, so number it from
					// there rather than restarting the group at 0.
					if let Err(err) = producer.start_at(frame_start) {
						stream.writer.abort(&err);
						let _ = producer.abort(err);
						return Poll::Ready(());
					}

					self.state = FetchRunState::Ingest {
						stream,
						producer,
						ingest: FrameIngest::new(self.timescale),
					};
				}
				FetchRunState::Ingest {
					stream,
					producer,
					ingest,
				} => {
					let res = ready!(ingest.poll(&mut stream.reader, producer, waiter));
					let FetchRunState::Ingest { producer, .. } =
						std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					match res {
						Ok(()) => {
							let mut producer = producer;
							let _ = producer.finish();
						}
						Err(err) => {
							let _ = producer.abort(err);
						}
					}
					return Poll::Ready(());
				}
				FetchRunState::Done => return Poll::Ready(()),
			}
		}
	}
}
