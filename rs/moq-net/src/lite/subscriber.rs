use crate::driver::Transport;
use crate::transport::poll::Session as _;
use crate::{frame, group, origin, track};
use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	sync::{Arc, atomic},
	task::{Poll, ready},
	time::Duration,
};

use crate::{
	Error, Path, Timescale, Timestamp, bandwidth,
	coding::{Decode as _, Reader, Stream, varint::MAX_QUIC},
	lite,
	track::{Anchor, Position, Subscription},
};

use super::Version;
use crate::tail::{self, Reading, Settle, Tail};

use kio::Lock;

pub(super) struct SubscriberConfig {
	pub runtime: crate::time::Deadlines,
	pub session: Transport,
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
	/// declare one itself. See `Client::with_peer_hop`.
	pub peer_hop: Option<crate::Hop>,
	/// Local policy for what pulling from this peer costs, overriding whatever it
	/// declared in its SETUP. `None` charges the peer's declared price.
	pub cost: Option<u64>,
	/// Set once the peer sends a GOAWAY; this session's routes then cost
	/// [`crate::origin::Cost::DRAIN`], so a replacement session outranks it.
	pub going_away: crate::goaway::GoingAway,
	/// Our tokens' grants: a subscription the union stops covering is cancelled.
	pub auth: crate::auth::Link,
}

#[derive(Clone)]
pub(super) struct Subscriber {
	runtime: crate::time::Deadlines,
	session: Transport,

	origin: origin::Producer,
	recv_bandwidth: Option<bandwidth::Producer>,
	// Session-level origin id shared with the Publisher. Used to drop reflected
	// announces: any incoming announce whose hop chain already passed through us
	// has looped, so it is neither used as a route nor forwarded. On lite-04/05
	// we also ask the peer to filter them out (AnnounceRequest.exclude_hop) so
	// they never hit the wire, but this check is what makes it correct.
	self_origin: crate::Hop,
	// The origin stored as `Route.via` for broadcasts from versions that don't
	// carry real hop ids on the wire (Lite01/02/03), and for a peer that reports
	// 0 in AnnounceOk. Lite03 placeholders stay 0 and count as anonymous.
	//
	// This is the peer's assigned identity (`peer_hop`): a fresh id per dialed or
	// accepted session, so its routes are distinguishable from another session's,
	// unless the caller pinned a stable one with `with_peer_hop`. Without one it is
	// `Hop::UNKNOWN` (0), the reserved "no identity" value. The assigned id stays
	// local and is never written into a hop chain.
	session_origin: crate::Hop,
	subscribes: Lock<HashMap<u64, TrackEntry>>,
	/// Why this session ended, once it has. A track still waiting on TRACK_INFO is
	/// not in [`Self::subscribes`], so dropping its request reads this instead of
	/// becoming [`Error::Dropped`].
	ended: Lock<Option<Error>>,
	next_id: Arc<atomic::AtomicU64>,
	version: Version,
	/// The peer's advertised SETUP (lite-05+), set when its Setup stream is read.
	peer_setup: super::PeerSetup,
	/// GROUP streams parked until the peer's SETUP says how to read their header.
	setup_waits: Arc<atomic::AtomicUsize>,
	/// Local policy overriding the peer's declared egress price. See `poll_link_cost`.
	cost: Option<u64>,
	/// Sources minted by the announce half, drained by the driver into
	/// [`SourceServe`] machines.
	sources: kio::Queue<MintedSource>,
	going_away: crate::goaway::GoingAway,
	auth: crate::auth::Link,
	/// Broadcasts the peer may have announced at once (`session::Limits::announces`).
	pub(super) announces: crate::session::Slots,
}

#[derive(Clone)]
struct TrackEntry {
	producer: track::Producer,
	/// How to read the track, from its TRACK_INFO, known before the SUBSCRIBE is even
	/// opened, so group streams decode frames without blocking.
	framing: lite::Framing,
	/// The groups received so far, so the subscription's end can wait for the ones owed.
	tail: kio::Producer<Tail>,
}

impl Subscriber {
	pub fn new(config: SubscriberConfig) -> Self {
		// Identity for incoming-hop loop detection. Derived from the local
		// origin we publish into so it matches the relay identity across
		// every session sharing that origin, required for cross-session
		// loop detection.
		let self_origin = config.origin.hop();
		Self {
			session: config.session,
			runtime: config.runtime,
			// Its own frame budget, so a peer's declared sizes crowd out no other session.
			origin: config.origin.with_own_frames(),
			recv_bandwidth: config.recv_bandwidth,
			self_origin,
			session_origin: config.peer_hop.unwrap_or(crate::Hop::UNKNOWN),
			subscribes: Default::default(),
			ended: Default::default(),
			next_id: Default::default(),
			version: config.version,
			peer_setup: config.peer_setup,
			setup_waits: Default::default(),
			cost: config.cost,
			sources: kio::Queue::new(),
			going_away: config.going_away,
			auth: config.auth,
			announces: Default::default(),
		}
	}

	/// Record the error that ended the session. The first one wins: a later cancel
	/// from dropping the driver must not replace it.
	fn note_end(&self, err: &Error) {
		let mut slot = self.ended.lock();
		if slot.is_none() {
			*slot = Some(err.clone());
		}
	}

	/// The recorded session end, or cancellation when the dispatcher was dropped.
	fn end_reason(&self) -> Error {
		self.ended.lock().clone().unwrap_or(Error::Cancel)
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
		prefix: &Path,
		announce: lite::AnnounceBroadcast,
		run: &mut PrefixRun,
	) -> Result<(), Error> {
		match announce {
			lite::AnnounceBroadcast::Active {
				suffix,
				epoch,
				hops,
				cost,
			} => {
				let (suffix, hops) = match self.version.has_announce_id() {
					// Every `active` assigns the next ordinal, even ones we drop locally.
					true => run.decoder.start(suffix, hops)?,
					// Nothing references an announcement here, so there are no bases.
					false => (suffix.rest, hops.literal),
				};
				let path = prefix.join(&suffix)?;
				if lite::update_supported(self.version)
					&& !self.version.has_announce_id()
					&& run.announced.contains(&path)
				{
					// lite-05 only: a duplicate ANNOUNCE for an already-announced path is an update;
					// atomically replace its metadata. Lite06+ updates by announce id, and older
					// versions never defined updates, so both fall through to start_announce, which
					// rejects the duplicate (Error::ProtocolViolation).
					self.update_announce(
						path,
						hops,
						cost,
						run.link_cost,
						run.responder_origin,
						&mut run.announced,
					)?;
				} else {
					self.start_announce(
						path,
						epoch,
						hops,
						cost,
						run.link_cost,
						run.responder_origin,
						&mut run.announced,
					)?;
				}
			}
			lite::AnnounceBroadcast::Ended { suffix, .. } => {
				let path = prefix.join(&suffix)?;
				tracing::debug!(broadcast = %self.log_path(&path), "unannounced");
				run.announced.withdraw(&path);
			}
			lite::AnnounceBroadcast::EndedId { id } => {
				// Resolve and retire the id; an unknown or already-retired id is a
				// protocol violation.
				let path = prefix.join(&run.decoder.end(id)?)?;
				tracing::debug!(broadcast = %self.log_path(&path), "unannounced");
				run.announced.withdraw(&path);
			}
			lite::AnnounceBroadcast::Update { id, hops, cost } => {
				// Resolve the id; it stays live (the replacement reuses it). An unknown
				// or retired id is a protocol violation.
				let (suffix, hops) = run.decoder.update(id, hops)?;
				let path = prefix.join(&suffix)?;
				self.update_announce(
					path,
					hops,
					cost,
					run.link_cost,
					run.responder_origin,
					&mut run.announced,
				)?;
			}
			lite::AnnounceBroadcast::Restart { id, epoch, hops, cost } => {
				// Resolved like an update: the id stays live.
				let (suffix, hops) = run.decoder.update(id, hops)?;
				let path = prefix.join(&suffix)?;
				run.announced.restart(&path, epoch);
				self.restart_announce(
					path,
					hops,
					cost,
					run.link_cost,
					run.responder_origin,
					&mut run.announced,
				)?;
			}
			lite::AnnounceBroadcast::Skipped | lite::AnnounceBroadcast::Synced => {}
		}
		Ok(())
	}

	/// Records the advertisement either way. Returns `Ok(true)` if it was accepted
	/// and attached a route, or `Ok(false)` if it was declined locally.
	#[expect(
		clippy::too_many_arguments,
		reason = "private plumbing; a single-use argument struct would only rename the parameters"
	)]
	fn start_announce(
		&mut self,
		path: Path,
		// The publisher instance the peer announced, fixed until it retracts.
		epoch: Option<crate::Epoch>,
		hops: crate::Hops,
		// The route cost off the wire, i.e. as the peer advertised it.
		// [`Cost::UNKNOWN`] before lite-06, leaving the hop chain as the only
		// routing input as before.
		cost: crate::origin::Cost,
		// This link's price, added to the wire cost.
		link_cost: u64,
		// Lite05+: the announce sender's origin id (from AnnounceOk). The sender no
		// longer stamps itself onto the chain, so we append it here to reconstruct
		// the full `[src...sender]` chain Lite04 stored. None for older versions,
		// where the sender already appended itself.
		responder_origin: Option<crate::Hop>,
		announced: &mut Announced,
	) -> Result<bool, Error> {
		// One current advertisement per prefix per stream. Test what the peer
		// advertised, not only what we accepted locally.
		if announced.contains(&path) {
			return Err(Error::ProtocolViolation);
		}

		// The peer holds this prefix now. Everything below either accepts the announcement,
		// replacing this, or declines it and leaves it exactly as reserved. Past the
		// session's cap this refuses the announce stream instead.
		announced.reserve(path.clone(), epoch)?;

		let Some(hops) = self.chain(&path, hops, responder_origin) else {
			return Ok(false);
		};

		tracing::debug!(route = %self.log_path(&path), hops = hops.len(), "announce");

		// Announce this session's route into the origin: paths under the prefix
		// resolve through this session on demand. An error means the prefix is
		// outside our scope, so don't serve it. Reflections are already
		// filtered above.
		let route = self.announced_route(&path, hops, cost, link_cost, responder_origin, announced);
		Ok(announced.offer(self, path, route))
	}

	/// The route to announce for a prefix this peer advertised, charging our
	/// link's price on top of the cost it advertised.
	///
	/// Once the peer has sent a GOAWAY every route it announces starts out draining,
	/// including a restart of one already attached: a connection on its way out must
	/// not win selection, however good the path it advertises looks.
	///
	/// The epoch is the one the peer announced at `path`: an update carries none,
	/// since it never changes the content.
	fn announced_route(
		&self,
		path: &Path,
		hops: crate::Hops,
		cost: crate::origin::Cost,
		link_cost: u64,
		responder: Option<crate::Hop>,
		announced: &Announced,
	) -> crate::origin::Route {
		let mut route = crate::origin::Route {
			epoch: announced.epoch(path),
			..Default::default()
		}
		.with_hops(hops)
		.with_cost(cost.charged(link_cost))
		.with_via(self.via(responder));

		if self.going_away.is_set() {
			route.cost = crate::origin::Cost::DRAIN;
		}

		route
	}

	/// The announcing session's declared or assigned identity, for split-horizon.
	///
	/// A non-zero AnnounceOk origin is the declared id. Otherwise the caller-assigned
	/// identity stands in, locally: it is never written into the hop chain.
	fn via(&self, responder: Option<crate::Hop>) -> crate::Hop {
		responder
			.filter(|hop| *hop != crate::Hop::UNKNOWN)
			.unwrap_or(self.session_origin)
	}

	/// Handle an ANNOUNCE_UPDATE (an explicit update status, or a duplicate ANNOUNCE on
	/// lite-05).
	///
	/// An update carries no content claim, so this session's route re-prices in
	/// place whatever the new chain says: in-flight tracks keep flowing and the
	/// origin only hands over if the winner changed.
	/// The advertisement is already live, so this can attach a route even when the
	/// original advertisement was declined locally.
	///
	/// Returns `Ok(false)` if the new hop chain is a reflected loop (this session's
	/// route is now gone), `Ok(true)` otherwise.
	fn update_announce(
		&mut self,
		path: Path,
		hops: crate::Hops,
		// The route cost off the wire and this link's price. See `start_announce`.
		cost: crate::origin::Cost,
		link_cost: u64,
		// Lite05+: the announce sender's origin id (from AnnounceOk), appended here to
		// rebuild the full chain since the sender no longer stamps itself. None for older
		// versions. See `start_announce`.
		responder_origin: Option<crate::Hop>,
		announced: &mut Announced,
	) -> Result<bool, Error> {
		// Reflected loop (or a full chain): detach its route but keep the advertisement live.
		let Some(hops) = self.chain(&path, hops, responder_origin) else {
			announced.declined(path);
			return Ok(false);
		};

		tracing::debug!(route = %self.log_path(&path), hops = hops.len(), "update");
		let metadata = self.announced_route(&path, hops, cost, link_cost, responder_origin, announced);

		// A re-price keeps the route's prefix (and its
		// served paths) and re-prices in place. In-flight tracks keep flowing.
		if let Some(entry) = announced.attached(&path) {
			entry.update(metadata);
			return Ok(true);
		}

		Ok(announced.offer(self, path, metadata))
	}

	/// Handle an ANNOUNCE_RESTART: another publisher instance replaces a live
	/// advertisement, under the epoch the caller recorded.
	///
	/// The replacement enters the origin as a fresh route before the old one leaves, so
	/// local consumers see one restart rather than an end and a start, and nothing
	/// resolved through the old route is joined again. The sources the old route minted
	/// take no new tracks, while the tracks already in flight run to their own end.
	///
	/// Returns `Ok(false)` if the new hop chain is a reflected loop (this session's
	/// route is now gone), `Ok(true)` otherwise.
	fn restart_announce(
		&mut self,
		path: Path,
		hops: crate::Hops,
		// See `update_announce`.
		cost: crate::origin::Cost,
		link_cost: u64,
		responder_origin: Option<crate::Hop>,
		announced: &mut Announced,
	) -> Result<bool, Error> {
		let Some(hops) = self.chain(&path, hops, responder_origin) else {
			announced.declined(path);
			return Ok(false);
		};

		tracing::debug!(route = %self.log_path(&path), hops = hops.len(), "restart");
		let route = self.announced_route(&path, hops, cost, link_cost, responder_origin, announced);
		// Held to the limit like a start: a restart outside it is withheld, dropping the old instance.
		Ok(announced.offer(self, path, route))
	}

	/// The full chain of a new or replacing advertisement, or `None` when it is
	/// reflected or exceeds the origin's hop limit.
	fn chain(&self, path: &Path, mut hops: crate::Hops, responder_origin: Option<crate::Hop>) -> Option<crate::Hops> {
		let reflected = match responder_origin {
			// A chain already naming the sender came back through it. Zero names
			// nobody and may repeat; every identified sender must be appended once.
			Some(responder) => {
				(responder != crate::Hop::UNKNOWN && hops.contains(&responder))
					|| hops.push(responder).is_err()
					|| hops.contains(&self.self_origin)
			}
			None => hops.contains(&self.self_origin),
		};
		if reflected {
			tracing::debug!(route = %self.log_path(path), "dropping reflected announce");
			return None;
		}

		// Lite03 carries anonymous placeholders, and Lite01/02 carry no path.
		// Keep an anonymous mark so downstream ranks unidentified paths last.
		if hops.is_empty() {
			hops.push(crate::Hop::UNKNOWN)
				.expect("an empty hop chain always has room for one entry, and repeats nothing");
		}
		if !matches!(self.version, Version::Lite01 | Version::Lite02) && hops.len() > self.origin.max_hops().get() {
			tracing::debug!(route = %self.log_path(path), hops = hops.len(), "dropping announce beyond hop limit");
			return None;
		}
		Some(hops)
	}

	/// Remove a subscription, releasing the session's handle on its producer.
	fn remove_subscribe(&self, id: u64) {
		self.subscribes.lock().remove(&id);
	}

	/// Decode one datagram body and hand it to the matching subscription's producer.
	fn route_datagram(&self, payload: bytes::Bytes) -> Result<(), Error> {
		if payload.len() > lite::Datagram::MAX_SIZE {
			return Ok(());
		}
		// Write through the map rather than cloning the entry out: a `TrackEntry` clone
		// is a handful of atomic bumps on every datagram, and a producer held past its
		// removal would keep the track (its cached groups, its stats subscription) alive.
		// The group path already writes to a producer under this lock.
		// Its framing is unknown until the peer's SETUP says whether the `ietf` extension is
		// negotiated, and a best-effort datagram is not worth waiting for.
		let Some(ietf) = self.peer_setup.ietf() else {
			return Ok(());
		};
		let mut subscribes = self.subscribes.lock();
		// An unknown subscription's datagram is dropped below, however it parses.
		let dg = lite::Datagram::decode(payload, self.version, ietf, |id| {
			subscribes.get(&id).map(|entry| entry.framing).unwrap_or_default()
		})?;
		let Some(entry) = subscribes.get_mut(&dg.subscribe) else {
			// Unknown or already-closed subscription: drop the datagram.
			return Ok(());
		};

		let timestamp = dg
			.timestamp
			.zip(entry.framing.timescale)
			.map(|(value, scale)| Timestamp::new(value, scale))
			.transpose()
			.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?
			// Dropped where it may be a send time, like a frame's.
			.filter(|_| self.version.has_media_time());

		// A datagram is never owed a stream, so it never holds the subscription's end open.
		if let Ok(mut tail) = entry.tail.write() {
			tail.account(dg.sequence..dg.sequence.saturating_add(1), self.runtime.now());
		}
		// A datagram says where the live feed is as well as a group does. Inserted first, so
		// the copy goes live with it already showing.
		let live = entry.producer.is_live();
		let datagram = crate::model::datagram::Stored {
			datagram: crate::Datagram {
				sequence: dg.sequence,
				timestamp,
				payload: dg.payload,
			},
			wire: dg.ietf.flatten().map(Arc::new),
			deflated: None,
		};
		match dg.inflated {
			Some(size) => entry.producer.insert_deflated_datagram(datagram, size)?,
			None => entry.producer.insert_wire_datagram(datagram)?,
		}
		// A datagram-only feed may never answer with SUBSCRIBE_START, which waits for a group.
		if !live {
			entry.producer.set_live(Some(Position::group(dg.sequence)), None);
		}
		Ok(())
	}

	fn log_path<'a>(&'a self, path: &'a Path) -> crate::path::Rooted<'a> {
		crate::path::Rooted(self.origin.root(), path)
	}
}

/// The subscriber half's driver: the announce prefixes, the uni-stream accept
/// loop, PROBE feedback, datagrams, and the per-source serve machines. Only an
/// error ends it.
// Owns the active subscriptions for exactly as long as the driver. A dropped
// driver is how a cancelled session unwinds, so cleanup cannot depend on any
// poll returning Ready.
struct SubscriptionCleanup(Lock<HashMap<u64, TrackEntry>>);

impl SubscriptionCleanup {
	/// End every active subscription with the session's error. Group machines own
	/// their cleanup independently; this records the track's terminal state.
	///
	/// A subscription the publisher already ended finishes instead: its track is over,
	/// and what arrived stays servable to readers still behind rather than handed to
	/// another route.
	fn abort(&self, err: &Error) {
		for (_, entry) in self.0.lock().drain() {
			let _ = match entry.tail.read().is_ended() {
				true => entry.producer.finish(),
				false => entry.producer.abort(err.clone()),
			};
		}
	}
}

impl Drop for SubscriptionCleanup {
	fn drop(&mut self) {
		// A session that ended with an error already aborted these with it; what
		// remains was cancelled with the driver.
		self.abort(&Error::Cancel);
	}
}

pub(super) struct SubscriberDriver {
	subscriber: Subscriber,
	/// Aborts whatever is still subscribed when the session ends.
	cleanup: SubscriptionCleanup,
	/// Which prefixes to ask the peer about.
	discovery: crate::discovery::Discovery,
	/// One machine per prefix asked about. Only an error ends the session; a
	/// prefix finishing cleanly (publisher FIN) just retires.
	prefixes: HashMap<Path, AnnouncePrefix>,
	uni: UniAccept,
	/// PROBE feedback; finishes quietly when unsupported or given up on.
	bandwidth: Option<RecvBandwidth>,
	/// Datagram receive; inert on a version or transport without datagrams.
	datagrams: Option<DatagramRecv>,
	/// One machine per minted source, serving the origin's track requests.
	sources: kio::Tasks<SourceServe>,
}

impl SubscriberDriver {
	pub fn new(
		subscriber: Subscriber,
		early: Vec<Reader<crate::transport::boxed::RecvStream, Version>>,
		client: bool,
	) -> Self {
		// Lite03/04 mark no initial set, so a watch would read as synced before the peer
		// answers it. Asking for everything at connect keeps their discovery eager instead.
		let lazy = client && !matches!(subscriber.version, Version::Lite03 | Version::Lite04);
		Self {
			discovery: crate::discovery::Discovery::new(&subscriber.origin, lazy, subscriber.runtime.clone()),
			prefixes: HashMap::new(),
			cleanup: SubscriptionCleanup(subscriber.subscribes.clone()),
			uni: UniAccept::new(subscriber.clone(), early),
			bandwidth: Some(RecvBandwidth::new(subscriber.clone())),
			datagrams: Some(DatagramRecv::new(subscriber.clone())),
			sources: kio::Tasks::new(),
			subscriber,
		}
	}

	/// End every active subscription with the error that ended the session.
	pub fn abort(&self, err: &Error) {
		// Before the subscribe map, so a TRACK_INFO request dropped with this driver
		// rejects with `err` rather than `Dropped`.
		self.subscriber.note_end(err);
		self.cleanup.abort(err);
	}

	/// Read what the closed transport already delivered: the buffered uni streams, then
	/// the subscribe responses. Serve machines wait for the group streams, since ending a
	/// subscription removes the state their tails are delivered into.
	pub fn poll_drain(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let closed = self.uni.poll(waiter).is_ready();
		let _ = self.uni.children.poll(waiter);
		// Once every remaining stream waits on a SETUP that never arrived, none of
		// them can deliver it: the peer can open no more.
		let stuck = self.subscriber.peer_setup.ietf().is_none()
			&& self.uni.children.len() == self.subscriber.setup_waits.load(atomic::Ordering::Relaxed);
		if !closed || !(self.uni.children.is_empty() || stuck) {
			return Poll::Pending;
		}
		// The peer's close can win the race against a SUBSCRIBE FIN it sent first, and on
		// Lite03/04 that FIN is the only end: read it, so the track finishes rather than
		// aborts. Each source reads its streams once, then retires on the closed session.
		let _ = self.sources.poll(waiter);
		Poll::Ready(())
	}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			while let Poll::Ready(change) = self.discovery.poll(waiter) {
				match change {
					crate::discovery::Change::Open(prefix, snapshot) => {
						let machine = AnnouncePrefix::new(self.subscriber.clone(), prefix.clone(), snapshot);
						self.prefixes.insert(prefix, machine);
					}
					// Dropping the machine resets its stream, which the peer reads as the
					// request ending, and retracts every route it reported.
					crate::discovery::Change::Close(prefix) => {
						self.prefixes.remove(&prefix);
					}
				}
			}
			let mut ended = Vec::new();
			for (prefix, machine) in &mut self.prefixes {
				match machine.poll(waiter) {
					Poll::Ready(Ok(())) => ended.push(prefix.clone()),
					Poll::Ready(Err(Error::NoStreamCredit)) => {
						tracing::warn!(%prefix, "announce interest refused without stream credit");
						ended.push(prefix.clone());
					}
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => {}
				}
			}
			if ended.is_empty() {
				break;
			}
			for prefix in ended {
				self.prefixes.remove(&prefix);
				self.discovery.ended(&prefix);
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

		// Sources minted by the announce half; their completion never ends the
		// session (the origin delivers the unannounce itself).
		while let Poll::Ready(Ok(minted)) = self.subscriber.sources.poll_pop(waiter) {
			self.sources.push(SourceServe::new(self.subscriber.clone(), minted));
		}
		let _ = self.sources.poll(waiter);

		Poll::Pending
	}
}

impl Drop for SubscriberDriver {
	fn drop(&mut self) {
		// `abort` already recorded a session error when the driver failed. A driver
		// dropped without that still has to name an end before its setup machines drop.
		self.subscriber.note_end(&Error::Cancel);
	}
}

/// Accepts incoming uni streams (GROUP data plus the peer's SETUP) and drives
/// each as a child machine. Resolves only on a transport error.
struct UniAccept {
	subscriber: Subscriber,
	// A dedicated accept handle: the poll interface takes `&mut self`.
	accept: Transport,
	children: kio::Tasks<UniServe>,
}

impl UniAccept {
	fn new(subscriber: Subscriber, early: Vec<Reader<crate::transport::boxed::RecvStream, Version>>) -> Self {
		let accept = subscriber.session.clone();
		let mut children = kio::Tasks::new();
		for reader in early {
			children.push(UniServe {
				subscriber: subscriber.clone(),
				state: UniState::Start {
					reader: reader.with_stats(subscriber.origin.stats()),
				},
			});
		}
		Self {
			subscriber,
			accept,
			children,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let _ = self.children.poll(waiter);

		loop {
			match self.accept.poll_accept_uni(waiter) {
				Poll::Ready(Ok(stream)) => {
					self.children.push(UniServe {
						subscriber: self.subscriber.clone(),
						state: UniState::Start {
							reader: Reader::new(stream, self.subscriber.version)
								.with_stats(self.subscriber.origin.stats()),
						},
					});
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(crate::lite::error::from_transport(err))),
				Poll::Pending => break,
			}
		}

		// Newly accepted children start now rather than on the next wake.
		let _ = self.children.poll(waiter);
		Poll::Pending
	}
}

/// One accepted uni stream, dispatched on its first varint.
struct UniServe {
	subscriber: Subscriber,
	state: UniState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum UniState {
	/// Reading the stream's type.
	Start {
		reader: Reader<crate::transport::boxed::RecvStream, Version>,
	},
	/// Reading the peer's single SETUP message, recorded so capability-gated
	/// streams (PROBE) can consult it. lite-05+ only.
	Setup {
		reader: Reader<crate::transport::boxed::RecvStream, Version>,
	},
	/// Discarding a lite-07+ padding stream until FIN.
	Padding {
		reader: Reader<crate::transport::boxed::RecvStream, Version>,
	},
	Group(GroupRecv),
	Done,
}

impl kio::Task for UniServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			tracing::debug!(%err, "error running uni stream");
			if matches!(err, Error::ProtocolViolation) {
				self.subscriber
					.session
					.clone()
					.close(crate::SessionError::ProtocolViolation.to_code(), &err.to_string());
			}
		}
		Poll::Ready(())
	}
}

impl UniServe {
	/// Abort the stream with the given error, wherever the reader currently lives.
	fn abort(&mut self, err: &Error) {
		match &mut self.state {
			UniState::Start { reader } | UniState::Setup { reader } | UniState::Padding { reader } => reader.abort(err),
			UniState::Group(recv) => recv.reader.abort(err),
			UniState::Done => {}
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				UniState::Start { reader } => {
					// A decode error here is only logged; the peer hung up or spoke garbage
					// before the stream had a type.
					let kind = ready!(reader.poll_decode::<lite::DataType>(waiter))?;
					// Claim before decoding the body, so two incomplete SETUPs are duplicates too.
					if matches!(kind, lite::DataType::Setup) && self.subscriber.version.has_setup_stream() {
						self.subscriber.peer_setup.claim()?;
					}
					let UniState::Start { reader } = std::mem::replace(&mut self.state, UniState::Done) else {
						unreachable!()
					};
					self.state = match kind {
						lite::DataType::Group => UniState::Group(GroupRecv::new(self.subscriber.clone(), reader)),
						lite::DataType::Setup => {
							let mut reader = reader;
							reader.set_priority(crate::priority::RECV_CONTROL);
							UniState::Setup { reader }
						}
						lite::DataType::Padding => UniState::Padding { reader },
					};
				}
				UniState::Setup { reader } => {
					if !self.subscriber.version.has_setup_stream() {
						let err = Error::UnexpectedStream;
						self.abort(&err);
						return Poll::Ready(Ok(()));
					}
					let res = ready!(reader.poll_decode::<lite::setup::ReceivedSetup>(waiter));
					match res {
						Ok(setup) => {
							tracing::debug!(?setup, "received peer setup");
							self.subscriber.peer_setup.receive(setup)?;
							return Poll::Ready(Ok(()));
						}
						// The slot is claimed, so no other SETUP can arrive and the streams
						// waiting on it would hang. The session cannot continue.
						Err(err) => {
							tracing::debug!(%err, "failed to read peer setup");
							self.abort(&err);
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
					}
				}
				UniState::Padding { reader } => {
					if !self.subscriber.version.has_padding() {
						self.abort(&Error::UnexpectedStream);
						return Poll::Ready(Ok(()));
					}
					return reader.poll_discard(waiter);
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
struct GroupRecv {
	subscriber: Subscriber,
	reader: Reader<crate::transport::boxed::RecvStream, Version>,
	state: GroupRecvState,
	/// Held while parked on the peer's SETUP, so a drain can tell it never arrives.
	setup_wait: Option<SetupWait>,
}

/// Counts one GROUP stream parked on the peer's SETUP for as long as it lives.
struct SetupWait(Arc<atomic::AtomicUsize>);

impl SetupWait {
	fn new(waits: &Arc<atomic::AtomicUsize>) -> Self {
		waits.fetch_add(1, atomic::Ordering::Relaxed);
		Self(waits.clone())
	}
}

impl Drop for SetupWait {
	fn drop(&mut self) {
		self.0.fetch_sub(1, atomic::Ordering::Relaxed);
	}
}

#[kio::machine(self.state: GroupRecvState)]
impl GroupRecv {
	fn new(subscriber: Subscriber, reader: Reader<crate::transport::boxed::RecvStream, Version>) -> Self {
		Self {
			subscriber,
			reader,
			state: GroupRecvState::Start,
			setup_wait: None,
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let (ietf, hdr) = wait!(Header => {
			// Before the header, which a wait after it would lose.
			let Poll::Ready(ietf) = self.subscriber.peer_setup.poll_ietf(waiter) else {
				let waits = &self.subscriber.setup_waits;
				self.setup_wait.get_or_insert_with(|| SetupWait::new(waits));
				return Poll::Pending;
			};
			self.setup_wait = None;
			self.reader.poll_decode::<lite::Group>(waiter).map_ok(|hdr| (ietf, hdr))
		})?;

		let (group, track, framing, reading) = {
			let mut subs = self.subscriber.subscribes.lock();
			let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

			let group_info = group::Info { sequence: hdr.sequence };
			// Stats (groups/frames/bytes) are counted in the model as the group
			// is written, through the tagged `track::Producer`. The track counts
			// it as content once its first frame lands.
			let received = entry.producer.create_group_at(group_info, hdr.frame_start);
			// The subscription's end waits until this stream is read. Counted
			// whatever the cache said, since the publisher counts every stream it
			// opens (a resubscribe resends a cached group), and only after the
			// cache holds the group, so a fetch held for the feed that sees it
			// accounted also finds it.
			let reading = Reading::open(&entry.tail, Some(hdr.sequence), self.subscriber.runtime.now());
			// A route's first group says where its live feed is, when its answer does
			// not: even one the cache already holds, as an idle copy asked from its
			// head gets back. The copy goes live once the cache shows that group.
			// Lite-07's answer does, and a group can overtake it: on a descending
			// track the first to land is the newest, not where the feed starts.
			if !entry.producer.is_live() && !self.subscriber.version.has_largest() {
				entry.producer.set_live(
					Some(Position {
						group: hdr.sequence,
						frame: hdr.frame_start,
					}),
					None,
				);
			}
			let group = match received {
				Ok(group) => group,
				// The group is at or past the end the publisher declared, which no
				// later stream can repair. Before lite-05 only a local finish sets
				// an end, and lite-05 specified an inclusive one, so its last
				// group lands on it: drop only that stream there.
				Err(Error::Closed)
					if !matches!(
						self.subscriber.version,
						Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 | Version::Lite05
					) =>
				{
					tracing::warn!(group = hdr.sequence, "group past the declared end of track");
					let _ = entry.producer.clone().abort(Error::ProtocolViolation);
					return Poll::Ready(Err(Error::ProtocolViolation));
				}
				Err(err) => return Poll::Ready(Err(err)),
			};
			(group, entry.producer.clone(), entry.framing, reading)
		};
		let (priority, order) = track
			.subscription()
			.map_or((0, Default::default()), |sub| (sub.priority, sub.order));
		self.reader
			.set_priority(crate::priority::recv_group(priority, hdr.sequence, order));

		// The timescale came from TRACK_INFO (read before this subscription was
		// even registered), so frames decode immediately. No SUBSCRIBE_OK to
		// wait on.
		// Guarded: dropping this machine mid-group is a cancellation, not a clean end.
		let group: crate::recv::Group = crate::recv::Group::new(group);
		let track: track::Producer = track;
		let ingest: FrameIngest = FrameIngest::new(framing, self.subscriber.version, ietf, hdr.frame_start);
		let reading: Reading = reading;

		// Filling the group, bailing if the track or group dies first. The track or group
		// dying cancels the stream; the peer's own close arrives through the ingest's reads.
		let res = wait!(Serve => {
			if let Poll::Ready(err) = track.poll_aborted(waiter) {
				Poll::Ready(Err(err))
			} else if let Poll::Ready(err) = group.handle().poll_aborted(waiter) {
				Poll::Ready(Err(err))
			} else {
				ingest.poll(&mut self.reader, group, waiter)
			}
		});

		let res = match res {
			Ok(()) => {
				let _ = group.finish();
				Ok(())
			}
			Err(err @ (Error::Cancel | Error::Stream(crate::StreamError::Cancel))) => {
				let _ = group.abort(err);
				Ok(())
			}
			Err(err) => {
				tracing::debug!(%err, group = %group.sequence, "group error");
				let _ = group.abort(err.clone());
				Err(err)
			}
		};
		// Held until the group settles above, so a frame the track or group cut short
		// drops into an already-aborted group instead of reporting a loss.
		drop((ingest, reading));
		Poll::Ready(res)
	}
}

/// Pumps bare FRAME messages from a reader into a group producer: the wire
/// format shared by GROUP streams and FETCH responses.
pub(crate) struct FrameIngest {
	/// A timescale decodes the lite-05 zigzag-delta timestamp prefix; none leaves frames
	/// untimed (pre-lite-05).
	framing: lite::Framing,
	/// Previous frame's raw timestamp value (in `timescale` units), for the
	/// zigzag-delta decode. The first frame's delta is absolute (prev = 0).
	prev_ts: u64,
	phase: IngestPhase,
	/// Where the stream's frames start, the first Object ID until a record names it.
	frame_start: u64,
	/// `Some` once the Group State of the `ietf` extension arrived: every frame follows an
	/// object record.
	records: Option<RecordIngest>,
	/// Whether the session's timestamps are media time. Lite-05/06 ones may be send times,
	/// so they are read to stay in step and dropped.
	media_time: bool,
}

/// A group's object records on ingest, under the `ietf` extension.
struct RecordIngest {
	/// The Group State carried relay state. Otherwise every record is plain: its Object ID
	/// is the frame's index, with no properties and no END_OF_GROUP.
	relayed: bool,
	/// The Group State set EXTENSIONS, without which no record has properties.
	extensions: bool,
	/// The previous record's Object ID, `None` before the first.
	prior: Option<u64>,
	/// The record of the frame being read: its Object ID and properties block.
	///
	/// It is checked when the frame is created, which charges it to the cache, so a stalled
	/// stream holds only the bytes it read.
	pending: Option<(u64, bytes::Bytes)>,
}

enum IngestPhase {
	/// Reading the `ietf` extension's Group State.
	State,
	/// Reading the next object record. Stream end here means the group has no more objects.
	Object,
	/// Reading the record's properties block, whose size comes first.
	Properties { id: u64, end: bool, size: Option<usize> },
	/// The END_OF_GROUP record arrived, so only the stream end may follow.
	Ended,
	/// Reading the timestamp delta (skipped without a timescale). Stream end here
	/// means the group has no more frames.
	Timing,
	/// Reading a compressed Track's inflated size (lite-07), or skipped. Stream end here
	/// also ends the group on an untimed Track, where nothing precedes it.
	Inflated { timestamp: Option<Timestamp> },
	/// Reading the frame size. Stream end here also ends the group (pre-lite-05,
	/// where there is no timing prefix to act as the sentinel).
	Size {
		timestamp: Option<Timestamp>,
		inflated: Option<u64>,
	},
	/// Streaming the frame payload.
	Payload { frame: frame::Producer },
}

impl FrameIngest {
	/// `ietf` when the session negotiated the extension, so the stream starts with a Group State.
	pub(crate) fn new(framing: lite::Framing, version: Version, ietf: bool, frame_start: u64) -> Self {
		Self {
			framing,
			prev_ts: 0,
			phase: match ietf {
				true => IngestPhase::State,
				false => IngestPhase::Timing,
			},
			frame_start,
			records: None,
			media_time: version.has_media_time(),
		}
	}

	/// The phase that starts the next frame.
	fn next_frame(&self) -> IngestPhase {
		match self.records {
			Some(_) => IngestPhase::Object,
			None => IngestPhase::Timing,
		}
	}

	/// Read a frame header varint. Only a plain group may end in its place.
	fn poll_header_varint<R: crate::transport::poll::RecvStream>(
		&self,
		reader: &mut Reader<R, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<u64>, Error>> {
		match self.records {
			Some(_) => reader.poll_varint(waiter).map_ok(Some),
			None => reader.poll_varint_maybe(waiter),
		}
	}

	/// `Ready(Ok(()))` once the stream FINs on a frame boundary. The caller
	/// finishes or aborts the group; a frame cut short mid-payload was already
	/// aborted here with the reason.
	pub(crate) fn poll<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut Reader<R, Version>,
		group: &mut crate::recv::Group,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		let mut budget = kio::coop::Budget::new(32);
		loop {
			match &mut self.phase {
				IngestPhase::State => {
					let state = ready!(reader.poll_varint(waiter))?;
					let wire = lite::ietf::decode_state(state, self.frame_start)?;
					let relayed = wire.is_some();
					// A track published over moq-lite has no moq-transport state.
					if relayed && self.framing.ietf.is_none() {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					let extensions = wire.as_ref().is_some_and(|wire| wire.extensions);
					if let Some(wire) = wire {
						group.set_wire(wire)?;
					}
					self.records = Some(RecordIngest {
						relayed,
						extensions,
						prior: None,
						pending: None,
					});
					self.phase = IngestPhase::Object;
				}
				IngestPhase::Object => {
					let Some(code) = ready!(reader.poll_varint_maybe(waiter))? else {
						return Poll::Ready(Ok(()));
					};
					let records = self.records.as_mut().expect("the Group State came first");
					let (id, end) = lite::ietf::decode_object(records.prior, code)?;
					if records.prior.is_none()
						&& let Some(mut wire) = group.wire()
					{
						wire.first_id = id;
						group.set_wire(wire)?;
					}
					records.prior = Some(id);
					self.phase = IngestPhase::Properties { id, end, size: None };
				}
				IngestPhase::Properties { id, end, size } => {
					let len = match size {
						Some(len) => *len,
						None => {
							let len = ready!(reader.poll_varint(waiter))?;
							if len > lite::ietf::MAX_PROPERTIES as u64 {
								return Poll::Ready(Err(Error::BoundsExceeded(crate::coding::BoundsExceeded)));
							}
							*size.insert(len as usize)
						}
					};
					let block = ready!(reader.poll_read_exact(waiter, len))?;
					let (id, end) = (*id, *end);
					let records = self.records.as_mut().expect("the Group State came first");
					if (end && !records.relayed) || (!block.is_empty() && !records.extensions) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					if end {
						let decoded = lite::ietf::decode_end(&block, self.framing)?;
						group.mark_gap(id, decoded.gap)?;
						group.set_end_object(crate::model::object::Fields {
							id,
							properties: decoded.properties,
						})?;
						self.phase = IngestPhase::Ended;
					} else {
						records.pending = Some((id, block));
						self.phase = IngestPhase::Timing;
					}
				}
				IngestPhase::Ended => {
					if ready!(reader.poll_has_more(waiter))? {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					return Poll::Ready(Ok(()));
				}
				IngestPhase::Timing => {
					let Some(scale) = self.framing.timescale else {
						self.phase = IngestPhase::Inflated { timestamp: None };
						continue;
					};
					// The timestamp delta doubles as the per-frame sentinel.
					let Some(zz) = ready!(self.poll_header_varint(reader, waiter))? else {
						return Poll::Ready(Ok(()));
					};
					let next: u64 = (self.prev_ts as i128 + crate::coding::varint::unzigzag(zz) as i128)
						.try_into()
						.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
					self.prev_ts = next;
					let timestamp = Timestamp::new(next, scale)
						.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
					// Read to stay in step, but dropped where it may be a send time.
					self.phase = IngestPhase::Inflated {
						timestamp: self.media_time.then_some(timestamp),
					};
				}
				IngestPhase::Inflated { timestamp } => {
					let timestamp = *timestamp;
					// Only a compressed Track carries it, and only lite-07 can mark one.
					if !group.compressed() {
						self.phase = IngestPhase::Size {
							timestamp,
							inflated: None,
						};
						continue;
					}
					let Some(inflated) = ready!(self.poll_header_varint(reader, waiter))? else {
						return Poll::Ready(Ok(()));
					};
					self.phase = IngestPhase::Size {
						timestamp,
						inflated: Some(inflated),
					};
				}
				IngestPhase::Size { timestamp, inflated } => {
					let (timestamp, inflated) = (*timestamp, *inflated);
					let Some(size) = ready!(self.poll_header_varint(reader, waiter))? else {
						return Poll::Ready(Ok(()));
					};
					// A frame that says nothing a plain frame would not keeps no fields.
					let fields = match &mut self.records {
						Some(records) => {
							let (id, block) = records.pending.take().expect("a record precedes each frame");
							let lite::ietf::Decoded { properties, gap } =
								lite::ietf::decode_properties(&block, self.framing, true)?;
							group.mark_gap(id, gap)?;
							let plain = id == group.frame_count() as u64 && properties.is_empty();
							if !plain && !records.relayed {
								return Poll::Ready(Err(Error::ProtocolViolation));
							}
							(!plain).then(|| Arc::new(crate::model::object::Fields { id, properties }))
						}
						None => None,
					};
					// `create_object` is the allocation chokepoint: it rejects an oversized
					// `size` and allocates up front only within the origin's budget, so no
					// pre-check is needed. No wire timestamp (pre-lite-05) means an untimed
					// frame. A compressed frame stores the slice as it arrives, its inflated
					// size checked against the same limits before anything inflates.
					let frame = group.create_object(frame::Header {
						info: frame::Info { size, timestamp },
						fields,
						inflated,
					})?;
					self.phase = IngestPhase::Payload { frame };
				}
				IngestPhase::Payload { frame } => {
					let failed = ready!(reader.poll_read_frame(waiter, frame)).err();

					let next = self.next_frame();
					let IngestPhase::Payload { frame } = std::mem::replace(&mut self.phase, next) else {
						unreachable!()
					};
					match failed {
						None => group.restore(frame.finish()?),
						Some(err) => {
							// Fail the group with the reason, not the Drop fallback's
							// generic `Dropped`.
							let _ = frame.abort(err.clone());
							return Poll::Ready(Err(err));
						}
					}
					// An empty frame is a few bytes, so a peer keeping them ready must not
					// hold the session's other tasks.
					ready!(budget.poll_yield(waiter));
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
struct DatagramRecv {
	subscriber: Subscriber,
	// A dedicated receive handle: the poll interface takes `&mut self`.
	recv: Transport,
	enabled: bool,
}

impl DatagramRecv {
	fn new(subscriber: Subscriber) -> Self {
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
		// A peer keeping datagrams ready must not hold the session's other tasks.
		let mut budget = kio::coop::Budget::new(32);
		loop {
			ready!(budget.poll_yield(waiter));
			let payload = ready!(self.recv.poll_recv_datagram(waiter)).map_err(crate::lite::error::from_transport)?;
			if let Err(err) = self.subscriber.route_datagram(payload) {
				tracing::debug!(%err, "dropping datagram");
			}
		}
	}
}

/// Opens a PROBE stream on demand while a consumer is interested.
///
/// Loops forever: wait for a consumer, race the probe stream against the
/// consumer leaving, then loop back. Transport errors stay per-stream; malformed
/// control bytes end the session.
struct RecvBandwidth {
	subscriber: Subscriber,
	state: BandwidthState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum BandwidthState {
	/// lite-05+ negotiates probing: only open a PROBE stream if the peer
	/// advertised it (Report or higher) in its SETUP. Older versions have no
	/// SETUP, so probe is always available there.
	Gate,
	/// Wait until at least one consumer is interested in the estimate.
	WaitUsed,
	/// Race the last consumer leaving against the probe stream ending.
	Probing(ProbeStream),
}

impl RecvBandwidth {
	fn new(subscriber: Subscriber) -> Self {
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
						// machine resets its stream, and clearing the estimate keeps that
						// consumer from reading a stale rate before the new stream reports.
						Poll::Ready(Ok(())) => {
							let _ = bandwidth.set(None);
							self.state = BandwidthState::WaitUsed;
							continue;
						}
						// The channel closed: give up for the rest of the session.
						Poll::Ready(Err(_)) => return Poll::Ready(Ok(())),
						Poll::Pending => {}
					}
					match ready!(probe.poll(waiter)) {
						Ok(()) => tracing::debug!("probe stream closed"),
						Err(err) => {
							let err = lite::error::control(&mut self.subscriber.session.clone(), err);
							tracing::warn!(%err, "probe stream error");
						}
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
struct ProbeStream {
	subscriber: Subscriber,
	session: Transport,
	state: ProbeState,
}

#[kio::machine(self.state: ProbeState)]
impl ProbeStream {
	fn new(subscriber: &Subscriber) -> Self {
		Self {
			subscriber: subscriber.clone(),
			session: subscriber.session.clone(),
			state: ProbeState::Start,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut stream: Stream<Transport, Version> = wait!(Open => {
			// Probe is best-effort telemetry; a session that is going away
			// has no use for a new estimate, so skip it rather than erroring.
			if self.subscriber.going_away.is_set() {
				return Poll::Ready(Ok(()));
			}
			Stream::poll_open(&self.session, self.subscriber.version, waiter)
		})?
		.with_stats(self.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Probe)?;
		wait!(Send => stream.writer.poll_flush(waiter))?;
		Poll::Ready(wait!(Read => {
			let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated by RecvBandwidth");
			// Each estimate wakes every reader, so a peer streaming them must not hold
			// the session's other tasks.
			let mut budget = kio::coop::Budget::new(32);
			loop {
				ready!(budget.poll_yield(waiter));
				let Some(probe) = ready!(stream.reader.poll_decode_maybe::<lite::Probe>(waiter))? else {
					break Poll::Ready(Ok(()));
				};
				bandwidth.set(probe.bitrate.map(bandwidth::Rate::from_bps))?;
			}
		}))
	}
}

/// One announce-interest stream: sends the ANNOUNCE_REQUEST for a prefix, then
/// feeds every received announce into the origin. Only its *error* ends the
/// session; a publisher FIN is a clean end for the prefix alone.
struct AnnouncePrefix {
	subscriber: Subscriber,
	prefix: Path,
	state: PrefixState,
	/// Holds the origin's announce consumers under the prefix unsynced
	/// until the peer's initial set is in. Dropped at the boundary the version
	/// marks, or with the stream.
	snapshot: Option<crate::model::Snapshot>,
}

/// The announce-decode loop's state.
struct PrefixRun {
	responder_origin: Option<crate::Hop>,
	/// What we charge every announcement arriving on this stream. Resolved once:
	/// it comes from the connect config or the peer's SETUP, neither of which
	/// changes for the life of the session.
	link_cost: u64,
	announced: Announced,
	// Lite06+: announce ids. Each received `active` implicitly assigns the next
	// per-stream ordinal; `ended`/`restart` reference it instead of repeating the
	// path, and lite-07 bases name it too. Tracked even for announces we drop
	// locally (reflected loops), since the sender doesn't know we dropped them.
	decoder: lite::AnnounceDecoder,
	/// The auth epoch last applied, so a new limit holds back or brings back routes.
	auth_epoch: u64,
	/// Lite05+: the ANNOUNCE_STARTs of the initial set still to read. Anything else
	/// before them, or the stream ending, cuts the counted set short.
	initial: u64,
}

#[kio::machine(self.state: PrefixState)]
impl AnnouncePrefix {
	/// `snapshot` was taken when the prefix was first wanted, so a consumer of the
	/// origin never reads as synced before the request even goes out.
	fn new(subscriber: Subscriber, prefix: Path, snapshot: crate::model::Snapshot) -> Self {
		Self {
			subscriber,
			prefix,
			state: PrefixState::Start,
			snapshot: Some(snapshot),
		}
	}

	/// Fails the session on any error, [`Error::TooManyRequests`] included: a peer
	/// announcing more than the session allows loses the session.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_run(waiter)
			.map_err(|err| lite::error::control(&mut self.subscriber.session.clone(), err))
	}

	fn poll_run(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Opening the control stream (after the GOAWAY gate).
		let mut stream: Stream<Transport, Version> = wait!(Open => Stream::poll_open_request(
			&self.subscriber.session,
			self.subscriber.version,
			waiter
		))?
		.with_stats(self.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Announce)?;
		// Lite04/05: ask the peer to filter out announces that already passed
		// through us, so the reflected ones never hit the wire. Encoding drops
		// this on every other version, where start_announce below is the only
		// filter.
		// Hidden routes are requested too: the session mirrors the peer into
		// the origin, and each local reader opts in on its own
		// (`origin::Consumer::with_hidden`).
		stream.writer.buffer(&lite::AnnounceRequest {
			prefix: self.prefix.clone(),
			exclude_hop: self.subscriber.self_origin,
			hidden: true,
		})?;
		wait!(Send => stream.writer.poll_flush(waiter))?;

		let version: Version = self.subscriber.version;
		// Lite03/04 mark no initial set, so the request going out is the best
		// boundary. Lite01/02 use ANNOUNCE_INIT, lite05/06 a count, lite07 SYNCED.
		if !version.has_announce_ok() && !matches!(version, Version::Lite01 | Version::Lite02) {
			self.snapshot = None;
		}

		// Lite05+: reading the publisher's ANNOUNCE_OK.
		let ok = wait!(ReadOk => match self.subscriber.version.has_announce_ok() {
			true => stream.reader.poll_decode::<lite::AnnounceOk>(waiter).map_ok(Some),
			false => Poll::Ready(Ok(None)),
		})?;
		// The publisher reports its own origin id, which we stamp onto every received
		// Announce's hop chain since it no longer does so itself. A peer may legally
		// report id 0 (no identity). Keep it: the assigned identity stays on `via` and
		// is never forwarded as a hop. Lite05/06 mark the initial set by count, the
		// ANNOUNCE_STARTs the publisher counted into it; lite07 waits for SYNCED.
		let (responder_origin, initial): (Option<crate::Hop>, u64) = match ok {
			Some(ok) => {
				if ok.active == 0 && !version.has_synced() {
					self.snapshot = None;
				}
				(Some(ok.origin), ok.active)
			}
			None => (None, 0),
		};

		// Waiting for the link cost (may block on the peer's SETUP).
		let link_cost = wait!(Cost => self.subscriber.poll_link_cost(waiter));
		let mut run: PrefixRun = PrefixRun {
			responder_origin,
			link_cost,
			announced: Announced::new(self.subscriber.announces.clone()),
			decoder: lite::AnnounceDecoder::default(),
			auth_epoch: 0,
			initial,
		};

		// Lite01/02 send the initial set as one ANNOUNCE_INIT message, so they
		// read that before the update stream. Every other version streams it as
		// ordinary announces. Each suffix takes an announce slot, so a set past
		// what is left of the budget is refused before it is decoded.
		let init = wait!(ReadInit => match self.subscriber.version {
			Version::Lite01 | Version::Lite02 => {
				let budget = run.announced.slots.available();
				stream
					.reader
					.poll_decode_with(waiter, |r| lite::AnnounceInit::decode_within(r, *version, budget))
					.map_ok(Some)
			}
			_ => Poll::Ready(Ok(None)),
		})?;
		if let Some(init) = init {
			for suffix in init.suffixes {
				let path = self.prefix.join(&suffix)?;
				// Lite01/02 don't carry hop information; the broadcast starts with
				// an empty chain and an unpriced link. Stats are attributed in the
				// model when this enters the origin via `create_broadcast`.
				self.subscriber.start_announce(
					path,
					None,
					crate::Hops::new(),
					crate::origin::Cost::UNKNOWN,
					0,
					run.responder_origin,
					&mut run.announced,
				)?;
			}
			self.snapshot = None;
		}

		// Streaming announce updates.
		Poll::Ready(wait!(Run => {
			// A draining peer usually stops announcing, so react to the
			// GOAWAY itself; waiting for another message would leave the
			// route primary until the session finally closed. Idempotent,
			// since the signal stays set.
			if self.subscriber.going_away.poll(waiter).is_ready() {
				run.announced.drain();
			}
			// A new limit holds back what the peer may no longer publish to us,
			// and brings back what it may again. The peer still holds each
			// advertisement either way.
			while let Poll::Ready(permit) =
				self.subscriber
					.auth
					.poll_permit(crate::auth::Direction::Subscribe, &mut run.auth_epoch, waiter)
			{
				run.announced.limit(&permit, &self.subscriber);
			}
			// Each announce routes into the origin, so a peer streaming them must not hold
			// the session's other tasks. A yield has woken the task, and this arm always
			// ends Pending, so the routes below are still served this turn.
			let mut budget = kio::coop::Budget::new(32);
			while budget.poll_yield(waiter).is_ready() {
				match stream.reader.poll_decode_maybe::<lite::AnnounceBroadcast>(waiter) {
					Poll::Ready(Ok(Some(announce))) => {
						if matches!(announce, lite::AnnounceBroadcast::Synced) {
							if self.snapshot.take().is_none() {
								return Poll::Ready(Err(Error::ProtocolViolation));
							}
							continue;
						}
						if run.initial > 0 {
							match announce {
								lite::AnnounceBroadcast::Active { .. } => run.initial -= 1,
								// An unknown type is no change, so it neither counts nor cuts.
								lite::AnnounceBroadcast::Skipped => {}
								_ => return Poll::Ready(Err(Error::ProtocolViolation)),
							}
						}
						self.subscriber.handle_announce(&self.prefix, announce, run)?;
						// Released only once the last counted route is in the origin.
						if run.initial == 0 && !self.subscriber.version.has_synced() {
							self.snapshot = None;
						}
					}
					Poll::Ready(Ok(None)) if run.initial > 0 || self.snapshot.is_some() => {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					Poll::Ready(Ok(None)) => {
						// The publisher FINed: it has nothing (more) to announce for this
						// prefix (e.g. a publish-only peer). That's a clean completion of
						// this announce stream, not a session error, so finish our side
						// and return Ok. Tearing down only the announce stream is correct
						// since no further progress can be made, but we must not
						// propagate an error that would kill the whole connection.
						stream.writer.finish().ok();
						return Poll::Ready(Ok(()));
					}
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => break,
				}
			}
			// Serve the routes whose request queue woke, including any attached
			// above. Each route parks on its own waker, so the rest cost nothing.
			run.announced.poll_serve(&self.subscriber, waiter);
			Poll::Pending
		}))
	}
}

/// A source the announce half minted for one requested path, for the driver to serve.
struct MintedSource {
	path: Path,
	/// The publisher instance the route announced, asked for on every request.
	epoch: Option<crate::Epoch>,
	source: crate::model::broadcast::SourceGuard,
	/// The source's track handler, registered before its requester could ask for a track.
	dynamic: crate::broadcast::Requests,
	/// Closes when the route that minted the source goes.
	route: kio::Consumer<()>,
}

/// Serves the origin's track requests for one minted source until it closes: its
/// route goes, the peer stops selecting its instance, nothing holds it any more, or
/// the session dies. A closed source
/// takes no new tracks but lets those in flight run to their own end (moq-lite:
/// retraction does not disturb subscriptions already in flight); the session
/// dying drops them.
struct SourceServe {
	subscriber: Subscriber,
	path: Path,
	epoch: Option<crate::Epoch>,
	source: crate::model::broadcast::SourceGuard,
	dynamic: crate::broadcast::Requests,
	route: kio::Consumer<()>,
	// A dedicated close-watch handle, since each pending operation needs its own.
	closed: Transport,
	tracks: kio::Tasks<TrackServeRun>,
	// The source ended: no more track requests will arrive.
	ended: bool,
	/// Answers listings of the source with the peer's TRACKS.
	lister: crate::broadcast::Lister,
	/// The peer's listing, while anyone lists the source.
	listing: Option<TracksRecv>,
}

/// Forwards the peer's TRACKS listing of a source into the source's lister.
struct TracksRecv {
	session: Transport,
	stream: Option<Stream<Transport, Version>>,
	/// What the peer lists, each holding an `announces` slot: discovery state the peer
	/// makes us hold.
	listed: HashMap<Arc<str>, crate::session::Slot>,
	synced: bool,
	/// Each event fans out to every reader, so a peer streaming add/remove churn must not
	/// hold the thread for as long as its bytes keep arriving.
	budget: kio::coop::Budget,
	/// A listing is a subscribe: it waits for a token that may cover the broadcast, and
	/// ends once the session stops allowing it.
	gate: crate::auth::Gate,
	/// The gate let the listing through, and this hop synced its readers.
	admitted: bool,
	/// Until the peer answers, what the source must still hold: the selected instance
	/// until the request is out, then no replacement, since a TRACKS request without an
	/// epoch lists whichever instance won the path when it arrived.
	unanswered: Option<crate::model::broadcast::RequestValidity>,
}

/// A TRACKS event, with its name resolved against what the listing holds.
#[derive(Debug)]
enum Listing {
	Added(Arc<str>, lite::TrackInfo),
	/// The listed name, or `None` if nothing by that name is listed.
	Removed(Option<Arc<str>>),
	/// Like [`Self::Removed`].
	Finished(Option<Arc<str>>),
	Synced,
}

/// The key `listed` holds for `name`, shared rather than copied.
fn known(listed: &HashMap<Arc<str>, crate::session::Slot>, name: &str) -> Option<Arc<str>> {
	listed.get_key_value(name).map(|(name, _)| name.clone())
}

impl TracksRecv {
	/// Apply the peer's events until it ends the listing, or the error that ends it.
	fn poll(&mut self, source: &SourceServe, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match (&self.unanswered, &self.stream) {
			(Some(validity), None) => validity.check(waiter)?,
			(Some(validity), Some(_)) => validity.check_unanswered(waiter)?,
			(None, _) => {}
		}
		ready!(self.gate.poll_ready(waiter));
		if self.gate.poll_denied(waiter).is_ready() {
			return Poll::Ready(Err(Error::Unauthorized));
		}
		if !std::mem::replace(&mut self.admitted, true) {
			// Synced with this hop, which holds nothing yet: a relay never waits on its
			// upstream, so the peer's tracks follow as live adds.
			source.lister.synced();
		}
		let version = source.subscriber.version;
		let stream = match &mut self.stream {
			Some(stream) => stream,
			None => {
				let mut stream = ready!(Stream::poll_open_request(&self.session, version, waiter))?;
				stream = stream.with_stats(source.subscriber.origin.stats());
				stream.writer.buffer(&lite::ControlType::Tracks)?;
				stream.writer.buffer(&lite::Tracks {
					broadcast: source.path.clone(),
					epoch: source.epoch.clone(),
				})?;
				self.stream.insert(stream)
			}
		};
		ready!(stream.writer.poll_flush(waiter))?;
		loop {
			ready!(self.budget.poll_yield(waiter));
			// Resolved while the name is still the wire's: only a new listing copies it, and
			// a removal is a lookup.
			let listed = &self.listed;
			let event = ready!(stream.reader.poll_decode_maybe_with(waiter, |r| {
				Ok(match lite::TracksEvent::decode(r, version)? {
					lite::TracksEvent::Added { name, info } => Listing::Added(name.as_ref().into(), info),
					lite::TracksEvent::Removed { name } => Listing::Removed(known(listed, &name)),
					lite::TracksEvent::Finished { name } => Listing::Finished(known(listed, &name)),
					lite::TracksEvent::Synced => Listing::Synced,
				})
			}))?;
			self.unanswered = None;
			let Some(event) = event else {
				return Poll::Ready(Ok(()));
			};
			match event {
				Listing::Added(name, info) => {
					if self.listed.contains_key(&name) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					let slot = source.subscriber.announces.acquire()?;
					source.lister.add(name.clone(), info.into())?;
					self.listed.insert(name, slot);
				}
				Listing::Removed(name) => {
					let name = name.ok_or(Error::ProtocolViolation)?;
					self.listed.remove(&name);
					source.lister.remove(&name)?;
				}
				Listing::Finished(name) => {
					let name = name.ok_or(Error::ProtocolViolation)?;
					self.listed.remove(&name);
					source.lister.finish(&name)?;
				}
				Listing::Synced => {
					// This hop synced its readers when the listing began; only the order matters here.
					if std::mem::replace(&mut self.synced, true) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
				}
			}
		}
	}
}

impl SourceServe {
	fn new(subscriber: Subscriber, minted: MintedSource) -> Self {
		let closed = subscriber.session.clone();
		Self {
			lister: minted
				.dynamic
				.lister()
				.expect("only the source answers its broadcast's listings"),
			listing: None,
			subscriber,
			path: minted.path,
			epoch: minted.epoch,
			source: minted.source,
			dynamic: minted.dynamic,
			route: minted.route,
			closed,
			tracks: kio::Tasks::new(),
			ended: false,
		}
	}

	/// Answer the source's listings while anyone lists it, from the peer's TRACKS.
	fn poll_listing(&mut self, waiter: &kio::Waiter) {
		// Taken out while it runs, since it reads the rest of the source.
		let mut listing = self.listing.take();
		loop {
			match &mut listing {
				None => {
					if !matches!(self.lister.poll_used(waiter), Poll::Ready(Ok(()))) {
						break;
					}
					if !self.subscriber.version.has_tracks() {
						// An empty answer would claim the peer lists nothing.
						self.lister.abort(Error::Unsupported);
						continue;
					}
					listing = Some(TracksRecv {
						session: self.subscriber.session.clone(),
						stream: None,
						listed: HashMap::new(),
						synced: false,
						budget: kio::coop::Budget::new(32),
						gate: crate::auth::Gate::new(
							self.subscriber.auth.clone(),
							self.path.clone(),
							crate::auth::Direction::Subscribe,
						),
						admitted: false,
						unanswered: Some(self.dynamic.validity().start()),
					});
				}
				Some(_) if self.lister.poll_unused(waiter).is_ready() => {
					// Dropping the stream resets it.
					listing = None;
					self.lister.clear();
				}
				Some(recv) => match recv.poll(self, waiter) {
					Poll::Pending => break,
					// The peer's broadcast ended, whether or not its retraction has arrived:
					// end the listing and let go of what it listed.
					Poll::Ready(Ok(())) => {
						listing = None;
						self.lister.end();
					}
					Poll::Ready(Err(err)) => {
						let err = lite::error::control(&mut self.subscriber.session.clone(), err);
						tracing::debug!(broadcast = %self.path, %err, "tracks failed");
						if let Some(stream) = recv.stream.take() {
							stream.writer.abort(&err);
						}
						listing = None;
						self.lister.abort(err);
					}
				},
			}
		}
		self.listing = listing;
	}
}

impl kio::Task for SourceServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let _ = self.tracks.poll(waiter);

		loop {
			if self.closed.poll_closed(waiter).is_ready() {
				// Session gone.
				return Poll::Ready(());
			}
			self.poll_listing(waiter);
			if self.ended {
				// Done once the tracks in flight are.
				return self.tracks.poll(waiter);
			}
			match self.dynamic.poll_requested_track(waiter) {
				Poll::Ready(Ok(request)) => {
					let serve = TrackServe {
						validity: self.dynamic.validity().clone(),
						route: Some(self.route.clone()),
						subscriber: self.subscriber.clone(),
						path: self.path.clone(),
						epoch: self.epoch.clone(),
						name: request.name().to_string(),
					};
					// One machine per track serves its lone subscription and any number
					// of fetches concurrently.
					self.tracks.push(TrackServeRun::new(serve, request));
				}
				// The source was finished (unannounced) or aborted.
				Poll::Ready(Err(err)) => {
					tracing::debug!(%err, "source closed");
					self.ended = true;
				}
				Poll::Pending => {
					// The route that minted the source went: close it now.
					if self.route.poll_closed(waiter).is_ready() {
						self.source.close();
						continue;
					}
					// Nothing holds the source and no track is on its way: retire it, so
					// what the session keeps for the path goes with the fronts that used
					// it, and a later request asks the route afresh. Declined when a holder
					// or a track got there first, so look again either way.
					if self.source.poll_unheld(waiter).is_pending() {
						break;
					}
					self.source.close_unheld();
				}
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
	use crate::Timescale;
	use crate::coding::{Decode, Encode};
	use crate::lite::test_transport::SinkSession;
	use crate::model::ProduceTest;
	use futures::FutureExt;

	const VERSION: Version = Version::Lite05;

	/// A pre-lite-05 relay's track, untimed, with a fetch of `sequence` queued on it.
	fn feed_fetch(sequence: u64) -> (track::Producer, kio::Pending<track::Fetching>, track::Fill) {
		let info = track::Info::default().with_timescale(None);
		let track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", info);
		let dynamic = track.requests();
		let fetching = track.consume().fetch_group(sequence, None);
		let req = dynamic.requested_fill().now_or_never().expect("queued").unwrap();
		(track, fetching, req)
	}

	/// A live feed whose ledger is `tail`.
	async fn feed_sub(tail: &kio::Producer<Tail>) -> Sub {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		Sub::Active(SubStream {
			order: crate::track::Order::Descending,
			stream: Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Lite04)
				.await
				.unwrap(),
			id: 0,
			max_delay: Duration::ZERO,
			requested_max_delay: Duration::ZERO,
			start: None,
			priority: 0,
			requested: None,
			ranges: vec![lite::Range::OLDEST],
			tail: tail.clone(),
			served: None,
			starts: Vec::new(),
			end: None,
			wanted: None,
			unanswered: None,
			bounded: false,
			updated: false,
		})
	}

	/// A group arriving at the relay as its header lands: cached, then accounted for.
	fn arrive(track: &track::Producer, tail: &kio::Producer<Tail>, sequence: u64, now: crate::time::Instant) {
		let mut group = track.create_group(group::Info { sequence }).unwrap();
		group.write_frame(None, &b"x"[..]).unwrap();
		group.finish().unwrap();
		let Ok(mut ledger) = tail.write() else {
			panic!("ledger closed")
		};
		ledger.account(sequence..sequence + 1, now);
	}

	/// A fetch held for a pre-lite-05 feed is released once its caller gives up, rather
	/// than waiting, track handle and all, for a group the feed may never reach.
	#[moq_clock::test]
	async fn an_abandoned_feed_fetch_is_released() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, fetching, req) = feed_fetch(100);

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert_eq!(fetches.held.len(), 1, "held while its caller waits");

		drop(fetching);
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert!(fetches.held.is_empty(), "released once abandoned");
	}

	/// A lite-07 publisher that FINs with no SUBSCRIBE_START served nothing, which covers the
	/// ranges the SUBSCRIBE asked for but not ones an update it may not have read changed.
	#[moq_clock::test]
	async fn a_startless_fin_covers_only_the_subscribed_ranges() {
		let tail = kio::Producer::new(Tail::default());
		let Sub::Active(mut active) = feed_sub(&tail).await else {
			unreachable!("feed_sub is active")
		};
		active.ranges = vec![lite::Range::span(Some(0), 0, Some(1), None)];
		assert_eq!(active.covered(), Some(Vec::new()));

		active.updated = true;
		assert_eq!(active.covered(), None, "the update may be unread");

		active.served = Some(0);
		assert_eq!(active.covered(), Some(std::iter::once(0..2).collect()));
	}

	/// A covered FIN owes a relative range only from its own SUBSCRIBE_START, not from the
	/// lowest start of a disjoint range below it.
	#[moq_clock::test]
	async fn a_covered_fin_owes_each_range_from_its_own_start() {
		let tail = kio::Producer::new(Tail::default());
		let Sub::Active(mut active) = feed_sub(&tail).await else {
			unreachable!("feed_sub is active")
		};
		let end = |group| Some(lite::Anchor::Absolute(crate::track::Position { group, frame: 0 }));
		active.ranges = vec![
			lite::Range::span(Some(0), 0, Some(0), None),
			lite::Range {
				start: Some(lite::Anchor::Relative(0)),
				end: end(11),
			},
			lite::Range {
				start: Some(lite::Anchor::Relative(0)),
				end: end(20),
			},
		];
		active.served = Some(0);
		active.starts = vec![Some(0), Some(10), None];
		assert_eq!(active.covered(), Some(vec![0..1, 10..11, 20..20]));

		active.ranges[0] = lite::Range::span(Some(0), 0, Some(2), None);
		active.starts[0] = Some(1);
		assert_eq!(active.covered(), Some(vec![1..3, 10..11, 20..20]));
		active.starts[0] = None;
		assert_eq!(active.covered(), Some(vec![3..3, 10..11, 20..20]));

		active.updated = true;
		assert_eq!(
			active.covered(),
			Some(vec![0..3, 0..11, 0..20]),
			"an update leaves only the lowest"
		);
	}

	/// QUIC does not order streams, so group 2's header can reach the relay before group 1's.
	/// A fetch held for group 1 keeps waiting through the grace and is served when it lands.
	#[moq_clock::test]
	async fn a_feed_fetch_waits_out_a_reordered_group() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, fetching, req) = feed_fetch(1);
		arrive(&track, &tail, 0, start);

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		arrive(&track, &tail, 2, start);
		let late = start + crate::tail::GRACE / 2;
		fetches.poll(&sub, late, &kio::Waiter::noop());
		assert_eq!(fetches.held.len(), 1, "group 2 alone does not give up on group 1");
		assert_eq!(fetches.fold.deadline(), Some(start + crate::tail::GRACE));

		arrive(&track, &tail, 1, late);
		fetches.poll(&sub, late, &kio::Waiter::noop());
		assert!(fetches.held.is_empty());
		let group = fetching
			.now_or_never()
			.expect("answered")
			.expect("served from the feed");
		assert_eq!(group.sequence, 1);
	}

	/// A group still missing a grace after a later one arrived is a miss, not a hold.
	#[moq_clock::test]
	async fn a_feed_fetch_misses_once_the_ledger_gives_up() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, fetching, req) = feed_fetch(1);
		arrive(&track, &tail, 0, start);
		arrive(&track, &tail, 2, start);

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start + crate::tail::GRACE, &kio::Waiter::noop());
		assert!(fetches.held.is_empty());
		assert!(matches!(fetching.now_or_never(), Some(Err(Error::NotFound))));
	}

	/// A floor lowered by an update, or a fresh SUBSCRIBE starting below earlier arrivals,
	/// asks for groups only now, so a fetch held for one waits out a fresh grace rather than
	/// the age of the gap below those arrivals.
	#[moq_clock::test]
	async fn a_lowered_floor_restarts_a_held_fetch_gap() {
		for update in [true, false] {
			let h = Harness::new(Version::Lite04);
			let (mut track, fetching, req) = feed_fetch(3);
			for sequence in [5, 6] {
				let mut group = track.create_group(group::Info { sequence }).unwrap();
				group.write_frame(None, &b"x"[..]).unwrap();
				group.finish().unwrap();
			}
			moq_clock::sleep(crate::tail::GRACE * 2).await;

			let lowered = Some(Subscription::default().with_range(2..));
			// The SUBSCRIBE in force, already answered.
			let floor = if update { 5 } else { 2 };
			let mut sub = Sub::Active(SubStream {
				order: crate::track::Order::Descending,
				stream: Stream::open(&mut crate::driver::Transport::new(h.session.clone()), Version::Lite04)
					.await
					.unwrap(),
				id: 0,
				max_delay: Duration::ZERO,
				requested_max_delay: Duration::ZERO,
				start: Some(floor),
				priority: 0,
				requested: Some(floor),
				ranges: vec![lite::Range::span(Some(floor), 0, None, None)],
				tail: Default::default(),
				served: None,
				starts: Vec::new(),
				end: None,
				wanted: None,
				unanswered: None,
				bounded: false,
				updated: false,
			});
			if update {
				h.serve
					.handle_subscription(&mut track, &mut sub, lowered, true)
					.await
					.unwrap();
			} else {
				let begin = h.serve.begin_subscription(&mut track, &mut Sub::None, lowered, true);
				assert!(matches!(begin, Ok(Begin::Establish(_))));
			}

			let now = crate::time::Instant::now();
			let clock = crate::time::Deadlines::new(now);
			let mut fetches = FeedFetches::new(&clock);
			fetches.push(req, &track.consume());
			fetches.poll(&sub, now, &kio::Waiter::noop());
			assert_eq!(
				fetches.held.len(),
				1,
				"group 3 was asked for just now (update: {update})"
			);

			let mut group = track.create_group(group::Info { sequence: 3 }).unwrap();
			group.write_frame(None, &b"x"[..]).unwrap();
			group.finish().unwrap();
			fetches.poll(&sub, now, &kio::Waiter::noop());
			let group = fetching
				.now_or_never()
				.expect("answered")
				.expect("served from the feed");
			assert_eq!(group.sequence, 3);
		}
	}

	/// Many group fetches on one track share model state, but never share readiness.
	fn feed_batch(
		sequences: impl IntoIterator<Item = u64>,
	) -> (track::Producer, Vec<kio::Pending<track::Fetching>>, Vec<track::Fill>) {
		let info = track::Info::default().with_timescale(None);
		let track = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info);
		let dynamic = track.requests();
		let mut callers = Vec::new();
		let mut requests = Vec::new();
		for sequence in sequences {
			callers.push(track.consume().fetch_group(sequence, None));
			requests.push(dynamic.requested_fill().now_or_never().expect("queued").unwrap());
		}
		(track, callers, requests)
	}

	#[moq_clock::test]
	async fn an_abandoned_feed_fetch_wakes_its_owner_without_an_arrival() {
		#[derive(Default)]
		struct Wakes(std::sync::atomic::AtomicUsize);
		impl std::task::Wake for Wakes {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, atomic::Ordering::Relaxed);
			}
		}
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, caller, req) = feed_fetch(100);
		let wakes = Arc::new(Wakes::default());
		let waiter = kio::Waiter::new(std::task::Waker::from(wakes.clone()));
		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &waiter);
		wakes.0.store(0, atomic::Ordering::Relaxed);
		drop(caller);
		assert!(
			wakes.0.load(atomic::Ordering::Relaxed) > 0,
			"cancellation wakes the ready queue's owner"
		);
		fetches.poll(&sub, start, &waiter);
		assert!(fetches.held.is_empty());
		arrive(&track, &tail, 200, start);
		fetches.poll(&sub, start + crate::tail::GRACE, &waiter);
		wakes.0.store(0, atomic::Ordering::Relaxed);
		fetches.poll(&sub, start + crate::tail::GRACE, &waiter);
		assert_eq!(
			wakes.0.load(atomic::Ordering::Relaxed),
			0,
			"an empty expired gap does not self-wake"
		);
		fetches.poll(&Sub::None, start, &waiter);
		wakes.0.store(0, atomic::Ordering::Relaxed);
		fetches.poll(&Sub::None, start, &waiter);
		assert_eq!(
			wakes.0.load(atomic::Ordering::Relaxed),
			0,
			"an absent feed does not self-wake"
		);
	}

	#[moq_clock::test]
	async fn a_feed_arrival_polls_only_its_group() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let mut fetches = FeedFetches::new(&clock);
		let (track, _callers, requests) = feed_batch(0..32);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		arrive(&track, &tail, 16, start);
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 1, "an arrival touches its group alone");
		assert_eq!(fetches.held.len(), 31);
		assert_eq!(fetches.fold.deadline(), Some(start + crate::tail::GRACE));
	}

	#[moq_clock::test]
	async fn a_feed_fold_polls_only_its_expired_gap() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, _callers, requests) = feed_batch([1, 3, 5, 1000]);
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		arrive(&track, &tail, 0, start);
		arrive(&track, &tail, 2, start);
		moq_clock::sleep(crate::tail::GRACE / 2).await;
		arrive(&track, &tail, 4, start + crate::tail::GRACE / 2);
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		fetches.poll(&sub, start + crate::tail::GRACE, &waiter);
		assert_eq!(fetches.polled - polled, 1);
		assert!(!fetches.held.contains_key(&1));
		assert_eq!(fetches.held.len(), 3);
		assert_eq!(
			fetches.fold.deadline(),
			Some(start + crate::tail::GRACE + crate::tail::GRACE / 2)
		);
	}

	#[moq_clock::test]
	async fn feed_boundaries_poll_only_the_groups_they_exclude() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (mut track, _callers, requests) = feed_batch([1, 3, 5, 1000]);
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		track.start_at(2).unwrap();
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 1);
		assert!(!fetches.held.contains_key(&1));
		let polled = fetches.polled;
		track.finish_at(Position::group(5)).unwrap();
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 2);
		assert_eq!(fetches.held.len(), 1);
		assert!(fetches.held.contains_key(&3));
		track.abort(Error::Cancel).unwrap();
		fetches.poll(&sub, start, &waiter);
		assert!(fetches.held.is_empty(), "closure releases all remaining held groups");
	}

	#[moq_clock::test]
	async fn a_feed_drop_polls_only_the_groups_it_excludes() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, mut callers, requests) = feed_batch([1, 3, 5]);
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		track.drop_groups(1..2, crate::StreamError::Old);
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 1);
		assert!(!fetches.held.contains_key(&1));
		assert_eq!(fetches.held.len(), 2);
		// The held fetch ends with the drop's own error.
		assert!(matches!(
			callers.remove(0).now_or_never(),
			Some(Err(Error::Stream(crate::StreamError::Old)))
		));
	}

	/// A peer that starts its feed far past a reader's floor makes the reader's miss wide.
	/// Holding it settles what the feed gave up by run, in one poll, rather than holding or
	/// refusing its groups one at a time.
	#[moq_clock::test]
	async fn a_wide_feed_fill_settles_by_run() {
		const WIDE: u64 = 1 << 40;
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let info = track::Info::default().with_timescale(None);
		let mut track = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info);
		let dynamic = track.requests();
		let mut reader = track.subscribe(
			Subscription::default()
				.with_range(0..)
				.with_max_delay(Duration::from_secs(30)),
		);
		track.start_at(WIDE).unwrap();
		arrive(&track, &tail, WIDE, start);
		let waiter = kio::Waiter::noop();
		// Reading judges the reader's misses.
		let _ = reader.poll_recv_group(&waiter);
		let fill = dynamic.requested_fill().now_or_never().expect("queued").unwrap();
		assert_eq!((fill.start().group, fill.end()), (0, WIDE));

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(fill, &track.consume());
		fetches.poll(&sub, start, &waiter);
		assert!(fetches.held.is_empty(), "the feed starts past every group of it");
		assert_eq!(fetches.polled, 1);
		assert!(reader.poll_recv_group(&waiter).is_pending());
		assert!(
			dynamic.requested_fill().now_or_never().is_none(),
			"a refusal is not asked again"
		);
	}

	#[moq_clock::test]
	async fn the_feed_track_backstop_refuses_the_next_group() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, mut callers, requests) = feed_batch([1, 2, 3]);
		let mut fetches = FeedFetches::new(&clock);
		fetches.max = 2;
		for req in requests {
			fetches.push(req, &track.consume());
		}
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert_eq!(fetches.held.len(), 2);
		assert!(matches!(
			callers.pop().unwrap().now_or_never(),
			Some(Err(Error::TooManyRequests))
		));
		assert!(callers.iter_mut().all(|caller| caller.now_or_never().is_none()));
	}

	#[moq_clock::test]
	async fn a_feed_fetch_can_replace_an_abandoned_attempt_before_polling() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, caller, req) = feed_fetch(1);
		let dynamic = track.requests();
		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &kio::Waiter::noop());
		drop(caller);
		let next = track.consume().fetch_group(1, None);
		let req = dynamic
			.requested_fill()
			.now_or_never()
			.expect("replacement queued")
			.unwrap();
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert_eq!(fetches.held.len(), 1);
		arrive(&track, &tail, 1, start);
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert!(next.now_or_never().unwrap().is_ok());
	}

	/// A replacement takes the place of the attempt it replaces under the limit, before any
	/// poll releases that one.
	#[moq_clock::test]
	async fn a_feed_fetch_replaces_an_abandoned_attempt_at_the_limit() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, mut callers, requests) = feed_batch([1, 2]);
		let dynamic = track.requests();
		let mut fetches = FeedFetches::new(&clock);
		fetches.max = 2;
		for req in requests {
			fetches.push(req, &track.consume());
		}
		fetches.poll(&sub, start, &kio::Waiter::noop());
		drop(callers.remove(0));
		let mut next = track.consume().fetch_group(1, None);
		let req = dynamic
			.requested_fill()
			.now_or_never()
			.expect("replacement queued")
			.unwrap();
		fetches.push(req, &track.consume());
		assert!((&mut next).now_or_never().is_none(), "held, not refused");
		assert_eq!(fetches.held.len(), 2);
		arrive(&track, &tail, 1, start);
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert!(next.now_or_never().unwrap().is_ok());
	}

	/// Sweep held fetches, unrelated tracks, and live arrival runs independently.
	#[moq_clock::test]
	#[ignore = "release benchmark, run by nightly"]
	async fn benchmark_feed_fetch_wakes() {
		struct ScanFetches {
			held: Vec<(track::GroupFill, track::Consumer)>,
			/// Armed at the earliest instant the feed's ledger gives up on a held group, so one
			/// that never arrives still fails.
			fold: crate::time::Deadline,
		}

		impl ScanFetches {
			fn new(clock: &crate::time::Deadlines) -> Self {
				Self {
					held: Vec::new(),
					fold: crate::time::Deadline::new(clock),
				}
			}

			/// Settle the held fetches against `sub`, the feed. A group it will not deliver is a
			/// miss, the same answer as an evicted one: with no feed open, once the feed starts past
			/// it or ends before it, or once its ledger settles it without the group (dropped, or
			/// still missing a grace after a later group arrived). A newer group alone settles
			/// nothing, since QUIC does not order streams. A fetch nobody waits on any more is
			/// dropped, as [`FetchServeRun`] does, so a far-future one isn't held forever.
			fn poll(&mut self, sub: &Sub, now: crate::time::Instant, waiter: &kio::Waiter) {
				let Sub::Active(active) = sub else {
					for (req, _) in self.held.drain(..) {
						req.reject(Error::NotFound);
					}
					self.fold.set(None);
					return;
				};
				let tail = active.tail.read();
				let mut fold: Option<crate::time::Instant> = None;
				for (req, feed) in std::mem::take(&mut self.held) {
					if req.poll_unused(waiter).is_ready() {
						continue;
					}
					let sequence = req.sequence();
					// The ledger before the cache: a group it accounts for by arrival is cached first.
					let missing = !tail.covers(sequence..sequence.saturating_add(1));
					let folds = tail.folds_at(sequence);
					// Every held sequence is ahead of the measured arrivals, so the
					// newer-group check is inert. Keep the scanner on the model's global
					// wake channel rather than giving it the new per-group index.
					match feed.poll_group(sequence, waiter) {
						Poll::Ready(Ok(Some(_))) => drop(req),
						Poll::Ready(Ok(None)) => req.reject(Error::NotFound),
						Poll::Ready(Err(err)) => req.reject(err),
						Poll::Pending if !missing || folds.is_some_and(|at| at <= now) => req.reject(Error::NotFound),
						Poll::Pending => {
							fold = fold.into_iter().chain(folds).min();
							self.held.push((req, feed));
						}
					}
				}
				drop(tail);
				self.fold.set(fold);
				let _ = self.fold.poll(waiter);
			}
		}

		const EVENTS: u64 = 128;
		println!("held,tracks,runs,scan_ns,indexed_ns");
		for held in [1, 64, 1024] {
			for (tracks, runs) in [(1, 1), (8, 1), (32, 1), (1, 64), (1, 1024)] {
				let mut medians = Vec::new();
				for indexed in [false, true] {
					let mut samples = Vec::new();
					for _ in 0..5 {
						let start = crate::time::Instant::now();
						let clock = crate::time::Deadlines::new(start);
						let waiter = kio::Waiter::noop();
						let mut fixtures = Vec::new();
						for _ in 0..tracks {
							// The sim counts ready polls per task poll to catch spinning loops, so
							// give each track's setup, and the timed loop below, a poll of its own.
							moq_clock::sim::yield_now().await;
							let (track, callers, requests) = feed_batch(1_000_000..1_000_000 + held);
							let tail = kio::Producer::new(Tail::default());
							let sub = feed_sub(&tail).await;
							for sequence in 0..runs {
								arrive(&track, &tail, sequence * 2, start);
							}
							let mut scan = ScanFetches::new(&clock);
							let mut fetches = FeedFetches::new(&clock);
							for req in requests {
								if indexed {
									fetches.push(req, &track.consume());
								} else {
									scan.held.push((req.group(req.start().group), track.consume()));
								}
							}
							if indexed {
								fetches.poll(&sub, start, &waiter);
							} else {
								scan.poll(&sub, start, &waiter);
							}
							fixtures.push((track, callers, tail, sub, scan, fetches));
						}
						let (track, _callers, tail, sub, scan, fetches) = &mut fixtures[0];
						moq_clock::sim::yield_now().await;
						#[expect(clippy::disallowed_methods, reason = "the scan cost is real elapsed time")]
						let began = std::time::Instant::now();
						for sequence in runs * 2..runs * 2 + EVENTS {
							arrive(track, tail, sequence, start);
							if indexed {
								fetches.poll(sub, start, &waiter);
							} else {
								scan.poll(sub, start, &waiter);
							}
						}
						samples.push(began.elapsed().as_nanos() / u128::from(EVENTS));
					}
					samples.sort_unstable();
					medians.push(samples[2]);
				}
				println!("{held},{tracks},{runs},{},{}", medians[0], medians[1]);
			}
		}
	}

	/// The owed groups start at the floor the demand last asked for, which an update can move
	/// either way after SUBSCRIBE_START answered the SUBSCRIBE.
	#[moq_clock::test]
	async fn the_owed_floor_follows_the_demand() {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let mut sub = SubStream {
			order: crate::track::Order::Descending,
			stream,
			id: 0,
			max_delay: Duration::ZERO,
			requested_max_delay: Duration::ZERO,
			start: Some(2),
			priority: 0,
			requested: Some(2),
			ranges: vec![lite::Range::span(Some(2), 0, None, None)],
			tail: Default::default(),
			served: None,
			starts: Vec::new(),
			end: Some(lite::SubscribeEnd { group: 10 }),
			wanted: None,
			unanswered: None,
			bounded: false,
			updated: false,
		};
		assert_eq!(
			sub.owed(None),
			Some(10..10),
			"without SUBSCRIBE_START nothing was served"
		);

		sub.served = Some(4);
		assert_eq!(
			sub.owed(None),
			Some(2..10),
			"the groups below START are accounted for on arrival"
		);
		sub.start = Some(6);
		assert_eq!(sub.owed(None), Some(6..10), "a raised floor owes nothing below it");
		sub.start = Some(1);
		assert_eq!(
			sub.owed(None),
			Some(1..10),
			"a lowered floor owes what it newly asked for"
		);
		sub.start = None;
		assert_eq!(
			sub.owed(Some(8)),
			Some(4..8),
			"a live-edge floor starts where START resolved it"
		);
	}

	async fn receive_padding(version: Version, bytes: Vec<u8>) {
		let session = crate::lite::test_transport::ScriptedSession::new(bytes);
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let log = session.log.clone();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut uni = UniServe {
			subscriber,
			state: UniState::Start { reader: stream.reader },
		};
		let waiter = kio::Waiter::noop();
		if version == Version::Lite07 {
			assert!(uni.poll_serve(&waiter).is_pending());
			// The same stream can keep sending before FIN, without accumulating a body.
			for _ in 0..8 {
				session.push(&[0; 4096]);
				assert!(uni.poll_serve(&waiter).is_pending());
			}
			// A body that is all ready at once still yields the driver before FIN.
			session.push(&[0; 64 * 8192]);
			session.close(crate::lite::test_transport::Close::Fin);
			assert!(uni.poll_serve(&waiter).is_pending());
			let mut polls = 1;
			let res = loop {
				if let Poll::Ready(res) = uni.poll_serve(&waiter) {
					break res;
				}
				polls += 1;
				assert!(polls < 64, "padding never finished");
			};
			assert!(res.is_ok());
			assert!(log.stops().is_empty());
		} else {
			assert!(uni.poll_serve(&waiter).is_ready());
			// An unsupported stream is stopped without ending the session.
			drop(uni);
			assert!(!log.stops().is_empty());
		}
		assert!(log.closes().is_empty());
	}

	/// A peer keeping datagrams ready gets a budget of them routed per poll, leaving the
	/// rest for later so the session's other tasks run in between.
	#[moq_clock::test]
	async fn a_flood_of_datagrams_yields() {
		use crate::transport::poll::Session as _;

		let (local, peer) = crate::transport::memory::pair(Default::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(local.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut recv = DatagramRecv::new(subscriber);
		assert!(recv.enabled);
		// Each names no subscription, so it is dropped once routed.
		for _ in 0..1024 {
			peer.send_datagram(bytes::Bytes::from_static(b"x"), Default::default())
				.unwrap();
		}

		assert!(recv.poll(&kio::Waiter::noop()).is_pending());
		let other = local.clone();
		assert!(
			other.poll_recv_datagram(&kio::Waiter::noop()).is_ready(),
			"one poll routed every datagram"
		);
	}

	#[moq_clock::test]
	async fn padding_is_discarded_only_on_lite07() {
		for version in [
			Version::Lite01,
			Version::Lite02,
			Version::Lite03,
			Version::Lite04,
			Version::Lite05,
			Version::Lite06,
			Version::Lite07,
		] {
			let mut bytes = Vec::new();
			crate::coding::Encoder::new(&mut bytes, version.into())
				.varint(2)
				.unwrap();
			receive_padding(version, bytes).await;
		}
	}

	/// Both runtimes receive the other's encoded padding and keep the session live.
	#[moq_clock::test]
	#[ignore = "requires Bun; run by just test interop"]
	async fn lite_padding_interop() {
		for (version, alpn) in [
			(Version::Lite05, "moq-lite-05"),
			(Version::Lite06, "moq-lite-06"),
			(Version::Lite07, "moq-lite-07-wip"),
		] {
			let mut bytes = Vec::new();
			let mut writer = crate::coding::Encoder::new(&mut bytes, version.into());
			lite::DataType::Padding.encode(&mut writer, version).unwrap();
			writer.slice(&[0xab; 4096]);
			let echo: Vec<u8> = serde_json::from_slice(&crate::test_interop::bun(
				"lite-padding.ts",
				serde_json::json!({ "alpn": alpn, "bytes": bytes }),
			))
			.unwrap();
			assert_eq!(echo, bytes);
			receive_padding(version, echo).await;
		}
	}

	/// A GROUP needs no negotiated extension, so it proceeds before SETUP, or after a
	/// server's gated accept held it.
	#[moq_clock::test]
	async fn early_group_is_delivered() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for pre_read in [false, true] {
				let mut bytes = Vec::new();
				lite::DataType::Group
					.encode(&mut crate::coding::Encoder::new(&mut bytes, version.into()), version)
					.unwrap();
				lite::Group {
					subscribe: 7,
					sequence: 3,
					frame_start: 0,
				}
				.encode(&mut crate::coding::Encoder::new(&mut bytes, version.into()), version)
				.unwrap();
				let mut w = crate::coding::Encoder::new(&mut bytes, version.into());
				w.varint(0).unwrap();
				w.varint(1).unwrap();
				w.u8(42);
				let mut setup = Vec::new();
				lite::DataType::Setup
					.encode(&mut crate::coding::Encoder::new(&mut setup, version.into()), version)
					.unwrap();
				lite::Setup::default()
					.encode(&mut crate::coding::Encoder::new(&mut setup, version.into()), version)
					.unwrap();
				let session =
					crate::lite::test_transport::ScriptedSession::eof(Vec::new()).with_incoming_unis(if pre_read {
						vec![Vec::new(), vec![42], bytes, setup]
					} else {
						vec![bytes]
					});
				let log = session.log.clone();
				let peer_setup = lite::PeerSetup::new(false, crate::transport::Address::Setup);
				let early = if pre_read {
					let accepted = lite::accept_setup(&mut crate::driver::Transport::new(session.clone()), version)
						.await
						.unwrap();
					assert_eq!(accepted.early.len(), 2);
					assert!(log.stops().is_empty());
					peer_setup.set(accepted.setup);
					accepted.early
				} else {
					Vec::new()
				};
				let subscriber = Subscriber::new(SubscriberConfig {
					runtime: crate::time::sim(),
					session: crate::driver::Transport::new(session.clone()),
					origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
					recv_bandwidth: None,
					version,
					peer_setup: peer_setup.clone(),
					peer_hop: None,
					cost: None,
					going_away: Default::default(),
					auth: crate::auth::Link::new(false),
				});
				// The track is untimed where the subscriber drops received timestamps.
				let track = track::Producer::new(
					std::sync::Arc::new(crate::broadcast::Info::default()),
					"video",
					crate::track::Info::default().with_timescale(version.has_media_time().then_some(Timescale::MILLI)),
				);
				let mut consumer = track.consume().subscribe(None).await.unwrap();
				subscriber.subscribes.lock().insert(
					7,
					TrackEntry {
						producer: track,
						framing: lite::Framing {
							timescale: Some(Timescale::MILLI),
							ietf: None,
							compressed: false,
						},
						tail: Default::default(),
					},
				);
				let mut accept = UniAccept::new(subscriber, early);
				assert!(accept.poll(&kio::Waiter::noop()).is_pending());
				assert!(log.stops().is_empty());
				let mut group = consumer.recv_group().await.unwrap().unwrap();
				assert_eq!(group.sequence, 3);
				let frame = group.read_frame().await.unwrap().unwrap();
				assert_eq!(frame.payload.as_ref(), &[42]);
				assert!(group.read_frame().await.unwrap().is_none());
			}
		}
	}

	/// A group at or past the end the publisher declared contradicts that end, which no later
	/// stream can repair, so the whole track fails rather than ending clean without it. lite-05
	/// specified an inclusive end, so there it costs only that group's stream.
	#[moq_clock::test]
	async fn a_group_past_the_declared_end_aborts_the_track() {
		use crate::transport::poll::Session as _;

		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut script = Vec::new();
			lite::Group {
				subscribe: 7,
				sequence: 3,
				frame_start: 0,
			}
			.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
			.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::eof(script);
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_hop: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});

			let mut track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				crate::track::Info::timed(),
			);
			track.finish_at(Position::group(3)).unwrap();
			subscriber.subscribes.lock().insert(
				7,
				TrackEntry {
					producer: track.clone(),
					framing: lite::Framing {
						timescale: Some(Timescale::MILLI),
						ietf: None,
						compressed: false,
					},
					tail: Default::default(),
				},
			);

			let (_, recv) = session.open_bi(Default::default()).await.unwrap();
			let mut group = GroupRecv::new(
				subscriber,
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
			);
			let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
			match version {
				Version::Lite05 => {
					assert!(matches!(res, Err(Error::Closed)), "{version:?}: {res:?}");
					assert!(
						track.closed().now_or_never().is_none(),
						"{version:?}: the track lives on"
					);
				}
				_ => {
					assert!(matches!(res, Err(Error::ProtocolViolation)), "{version:?}: {res:?}");
					assert!(matches!(
						track.closed().now_or_never(),
						Some(Err(Error::ProtocolViolation))
					));
				}
			}
		}
	}

	/// A track in `timescale` that a draft-18 moq-transport publisher published.
	fn draft18(timescale: Option<Timescale>) -> lite::Framing {
		lite::Framing {
			timescale,
			ietf: Some(crate::ietf::Version::Draft18),
			compressed: false,
		}
	}

	/// Receive group 0 of a track `framing` describes over a lite-07 session that negotiated
	/// the `ietf` extension, its stream holding `body` after the GROUP header. Also returns
	/// the cache pool the track charges.
	async fn receive_ietf(
		framing: lite::Framing,
		body: &[u64],
	) -> (Result<(), Error>, track::Producer, crate::cache::Pool) {
		use crate::transport::poll::Session as _;

		let version = Version::Lite07;
		let mut script = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
		lite::Group {
			subscribe: 7,
			sequence: 0,
			frame_start: 0,
		}
		.encode(&mut encoder, version)
		.unwrap();
		for value in body {
			encoder.varint(*value).unwrap();
		}
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup).offer_ietf(true);
		peer_setup.set(lite::Setup {
			ietf: true,
			..Default::default()
		});
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup,
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let pool = crate::cache::Pool::unbounded();
		let mut info = crate::track::Info::default().with_timescale(framing.timescale);
		info.properties = framing.ietf.map(|version| {
			std::sync::Arc::new(crate::model::object::TrackProperties {
				version,
				values: Vec::new(),
			})
		});
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info {
				pool: pool.clone(),
				..Default::default()
			}),
			"video",
			info,
		);
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry {
				producer: track.clone(),
				framing,
				tail: Default::default(),
			},
		);

		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut group = GroupRecv::new(
			subscriber,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		);
		let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
		(res, track, pool)
	}

	/// Object records: `(Skip << 1) | END`, the properties block's length, then the frame.
	#[moq_clock::test]
	async fn ietf_records_frame_a_group() {
		// A plain group: Group State 0, then frame 0 with an empty record.
		let (res, track, _) = receive_ietf(draft18(None), &[0, 0, 0, 1, u64::from(b'x')]).await;
		res.unwrap();
		let mut group = track.consume().peek_group(0).unwrap();
		assert_eq!(
			group.read_frame().now_or_never().unwrap().unwrap().unwrap().payload,
			b"x"[..]
		);

		// A relayed group starting at Object 4, then END_OF_GROUP at Object 6.
		let (res, track, _) = receive_ietf(draft18(None), &[0x1, 4 << 1, 0, 1, u64::from(b'x'), (1 << 1) | 1, 0]).await;
		res.unwrap();
		let group = track.consume().peek_group(0).unwrap();
		assert_eq!(group.end_object().map(|end| end.id), Some(6));
		assert_eq!(group.wire().map(|wire| wire.first_id), Some(4));
	}

	/// A record the Group State does not allow, or bytes past END_OF_GROUP, are refused.
	#[moq_clock::test]
	async fn ietf_records_refuse_what_their_group_cannot_hold() {
		for (body, reason) in [
			(&[0x20][..], "an unknown Group State bit"),
			(&[0x2][..], "relay state without RELAYED"),
			(&[0, 1 << 1, 0, 1, 0], "a plain group skipping an Object ID"),
			(&[0, 1, 0], "a plain group with END_OF_GROUP"),
			(&[0, 0, 2, 0x10, 0], "a plain group with properties"),
			(&[0x1, 1, 0, 0], "a byte after END_OF_GROUP"),
			(&[0x1, 0, 0], "a stream ending inside a frame"),
		] {
			let (res, ..) = receive_ietf(draft18(None), body).await;
			assert!(res.is_err(), "accepted {reason}");
		}
	}

	/// A track published over moq-lite has no moq-transport state, so a relayed group on
	/// one is refused, though its plain groups cross.
	#[moq_clock::test]
	async fn ietf_records_need_a_moq_transport_track() {
		let plain = [0, 0, 0, 1, u64::from(b'x')];
		let relayed = [0x1, 0, 0, 1, u64::from(b'x')];
		for (framing, ok) in [(draft18(None), true), (lite::Framing::default(), false)] {
			let (res, ..) = receive_ietf(framing, &plain).await;
			res.unwrap();
			let (res, ..) = receive_ietf(framing, &relayed).await;
			assert_eq!(res.is_ok(), ok, "{framing:?}: {res:?}");
		}
	}

	/// A relayed group's records carry properties only when its Group State sets EXTENSIONS,
	/// as the moq-transport subgroup it mirrors could hold none otherwise.
	#[moq_clock::test]
	async fn ietf_records_need_extensions_for_properties() {
		// Object 0 with a draft-18 block holding 0x20 = 1, or an untimed Timestamp.
		for record in [&[2, 0x20, 1][..], &[2, 0x10, 7]] {
			for (state, ok) in [(0x1 | 0x4, true), (0x1, false)] {
				let mut body = vec![state, 0];
				body.extend(record);
				body.push(0);
				let (res, ..) = receive_ietf(draft18(None), &body).await;
				assert_eq!(res.is_ok(), ok, "Group State {state:#x}, record {record:?}");
			}
		}
	}

	/// On a timed track, the frame holds an object's time, so a record cannot carry a second,
	/// mutable Timestamp that could disagree with it. Records that say nothing of time, or
	/// seal a Timestamp moq-transport egress cannot rewrite, cross beside the frame's.
	#[moq_clock::test]
	async fn ietf_records_leave_time_to_their_frame() {
		let frame = crate::coding::varint::zigzag(500);
		// Each record after its Object code: Object 0 unless it says otherwise.
		for (record, ok, reason) in [
			(&[0, 0][..], true, "nothing at all"),
			(&[0, 3, 0x10, 500], false, "a mutable Timestamp"),
			(&[0, 2, 0x20, 1], true, "properties without a Timestamp"),
			(&[0, 5, 0x0b, 3, 0x10, 499], true, "a sealed Timestamp"),
			(&[1 << 1, 0], true, "Object 1 without properties"),
		] {
			let mut body = vec![0x1 | 0x4];
			body.extend(record);
			body.extend([frame, 0]);
			let (res, track, _) = receive_ietf(draft18(Some(Timescale::MILLI)), &body).await;
			assert_eq!(res.is_ok(), ok, "{reason}: {res:?}");
			if ok {
				let mut group = track.consume().peek_group(0).unwrap();
				let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
				assert_eq!(frame.timestamp, Some(Timestamp::from_millis(500).unwrap()), "{reason}");
			}
		}
	}

	/// A record's properties stay the bytes they arrived as, which the cache charges, rather
	/// than a decoded list costing many times as much.
	#[moq_clock::test]
	async fn ietf_records_hold_properties_at_their_wire_size() {
		const COUNT: u64 = 1000;
		// A relayed group's Object 0, whose draft-18 block holds COUNT two-byte properties
		// (0x20 = 1, then zero deltas), then an empty frame.
		let mut body = vec![0x1 | 0x4, 0, 2 * COUNT, 0x20, 1];
		for _ in 1..COUNT {
			body.extend([0, 1]);
		}
		body.push(0);
		let (res, track, pool) = receive_ietf(draft18(None), &body).await;
		res.unwrap();
		let group = track.consume().peek_group(0).unwrap();
		assert_eq!(group.frame_count(), 1);
		let decoded = COUNT * size_of::<crate::model::object::Property>() as u64;
		assert!(
			(2 * COUNT..decoded / 2).contains(&pool.used()),
			"{} cached for a {}-byte block",
			pool.used(),
			2 * COUNT
		);
	}

	/// Receive group 5 over `version`, one frame per millisecond timestamp delta, into the
	/// copy a subscriber makes there, whose group 4 started at 1000ms, returning the stream's
	/// result and the track.
	async fn receive_group(version: Version, deltas: &[i64]) -> (Result<(), Error>, track::Producer) {
		use crate::transport::poll::Session as _;

		let mut script = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
		lite::Group {
			subscribe: 7,
			sequence: 5,
			frame_start: 0,
		}
		.encode(&mut encoder, version)
		.unwrap();
		// The first delta is absolute.
		for delta in deltas {
			encoder.varint(crate::coding::varint::zigzag(*delta)).unwrap();
			encoder.bytes(b"frame").unwrap();
		}
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// The copy is untimed where the wire's timestamps may be send times.
		let timed = version.has_media_time();
		let info = match timed {
			true => crate::track::Info::timed(),
			false => crate::track::Info::default(),
		};
		let track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", info);
		let mut before = track.create_group(4u64.into()).unwrap();
		before
			.write_frame(timed.then(|| Timestamp::from_millis(1000).unwrap()), &b"before"[..])
			.unwrap();
		before.finish().unwrap();
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry {
				producer: track.clone(),
				framing: lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
				},
				tail: Default::default(),
			},
		);

		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut group = GroupRecv::new(
			subscriber,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		);
		let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
		(res, track)
	}

	/// Lite-05/06 timestamps may be send times, so a received frame is read past its
	/// timestamp and lands untimed: nothing rewinds, and the track stays open.
	#[moq_clock::test]
	async fn lite06_timestamps_are_dropped() {
		for version in [Version::Lite05, Version::Lite06] {
			// Frame 0 at 500ms, before group 4's 1000ms.
			let (res, track) = receive_group(version, &[500]).await;
			res.unwrap_or_else(|err| panic!("{version}: {err:?}"));
			assert!(track.closed().now_or_never().is_none(), "{version}");
			let mut consumer = track.subscribe(None);
			let mut group = consumer.recv_group().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(group.sequence, 5, "{version}");
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.payload.as_ref(), b"frame", "{version}");
			assert_eq!(frame.timestamp, None, "{version}");
		}
	}

	/// Lite-07 timestamps are media time, so a received group is checked like a local
	/// write and one that rewinds is refused.
	#[moq_clock::test]
	async fn a_lite07_rewind_is_refused() {
		let (res, _track) = receive_group(Version::Lite07, &[500]).await;
		assert!(matches!(res, Err(Error::TimestampMismatch)), "{res:?}");
	}

	/// A received timestamp past 2^61 - 1 is refused at every version, so a relay never
	/// holds one that lite-05/06 could not carry as a group's first frame. lite-06 reaches
	/// it with a second delta, lite-07 with a first frame its 64-bit varint can encode.
	/// lite-05/06 drop the timestamp, but a stream carrying one past the bound is still malformed.
	#[moq_clock::test]
	async fn a_received_timestamp_past_2_61_is_refused() {
		let max: i64 = (1 << 61) - 1;
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let (res, track) = receive_group(version, &[max]).await;
			res.unwrap_or_else(|err| panic!("{version}: {err:?}"));
			let mut group = track.consume().peek_group(5).unwrap();
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			let kept = version.has_media_time().then_some(max as u64);
			assert_eq!(frame.timestamp.map(Timestamp::value), kept, "{version}");

			let (res, _) = receive_group(version, &[max, 1]).await;
			assert!(matches!(res, Err(Error::BoundsExceeded(_))), "{version}: {res:?}");
		}
		let (res, _) = receive_group(Version::Lite07, &[max + 1]).await;
		assert!(matches!(res, Err(Error::BoundsExceeded(_))), "{res:?}");
	}

	/// An idle copy that resubscribes gets its newest cached group again. That stream only
	/// duplicates the cache, but lite-07's SUBSCRIBE_END still counts it, so the tail must
	/// too, or the subscription waits out the grace before it ends.
	#[moq_clock::test]
	async fn a_duplicate_group_stream_still_counts_toward_the_end() {
		use crate::transport::poll::Session as _;

		let version = Version::Lite07;
		let mut script = Vec::new();
		lite::Group {
			subscribe: 7,
			sequence: 3,
			frame_start: 0,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
		.unwrap();
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let clock = crate::time::Deadlines::new(crate::time::Instant::now());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: clock.clone(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		let mut cached = track.create_group(group::Info { sequence: 3 }).unwrap();
		cached.write_frame(Timestamp::ZERO, &b"x"[..]).unwrap();
		cached.finish().unwrap();
		let tail = kio::Producer::new(Tail::default());
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry {
				producer: track.clone(),
				framing: lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
				},
				tail: tail.clone(),
			},
		);

		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut group = GroupRecv::new(
			subscriber,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		);
		let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
		assert!(matches!(res, Err(Error::Duplicate)), "{res:?}");
		drop(group);

		// SUBSCRIBE_END counted one stream: the tail settles without the clock moving.
		let mut settle = Settle::new(&clock, tail.consume());
		assert!(settle.poll(&kio::Waiter::noop(), |tail| tail.streams() >= 1).is_ready());
	}

	/// A SUBSCRIBE_END below a group already received contradicts that group. lite-05
	/// specified an inclusive end, and `@moq/net` 0.1.3 to 0.1.9 sent one, so there it only
	/// costs the early boundary; later drafts abort the track.
	#[moq_clock::test]
	async fn a_subscribe_end_below_a_received_group() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut responses = Vec::new();
			lite::SubscribeResponse::Start(lite::SubscribeStart {
				largest: None,
				starts: vec![Some(0)],
			})
			.encode(
				&mut crate::coding::Encoder::new(&mut responses, version.into()),
				version,
			)
			.unwrap();
			lite::SubscribeResponse::End(lite::SubscribeEnd { group: 2 })
				.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::eof(responses);
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_hop: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				subscriber,
				path: Path::from_static("room"),
				epoch: None,
				name: "video".to_string(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let request = broadcast.reserve_track("video").unwrap();
			let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
			let mut group = serving.serving.create_group(group::Info { sequence: 2 }).unwrap();
			group.write_frame(crate::Timestamp::ZERO, b"2".as_slice()).unwrap();
			group.finish().unwrap();
			let mut reader = broadcast
				.consume()
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.unwrap();
			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
			};
			kio::wait(|waiter| kio::Task::poll(&mut running, waiter)).await;

			let end = loop {
				match reader.recv_group().await {
					Ok(Some(_)) => continue,
					end => break end.map(|group| group.map(|group| group.sequence)),
				}
			};
			match version {
				Version::Lite05 => assert!(matches!(end, Ok(None)), "{version:?}: {end:?}"),
				_ => assert!(matches!(end, Err(Error::ProtocolViolation)), "{version:?}: {end:?}"),
			}
		}
	}

	/// Answer a SUBSCRIBE without an epoch with a finished, empty track, but only read the
	/// answer after `retire` changed its source's selection, then return what the reader got.
	async fn answer_after(
		version: Version,
		retire: impl FnOnce(&mut crate::model::TestSelection),
	) -> Option<Result<Option<u64>, Error>> {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut selection = crate::model::TestSelection::new();
		let serve = TrackServe {
			validity: selection.validity(),
			route: None,
			subscriber,
			path: Path::from_static("room"),
			epoch: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("video").unwrap();
		let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
		let mut reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
		};
		assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());

		let mut answer = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut answer, version.into());
		lite::SubscribeResponse::Start(lite::SubscribeStart {
			largest: None,
			starts: vec![Some(0)],
		})
		.encode(&mut encoder, version)
		.unwrap();
		lite::SubscribeResponse::End(lite::SubscribeEnd { group: 0 })
			.encode(&mut encoder, version)
			.unwrap();
		session.push(&answer);
		retire(&mut selection);
		let _ = kio::Task::poll(&mut running, &kio::Waiter::noop());

		reader
			.recv_group()
			.now_or_never()
			.map(|res| res.map(|group| group.map(|group| group.sequence)))
	}

	/// An answer read only after another instance took the path may be that instance's,
	/// so the replacement refuses the subscription instead, as does a withdrawal followed
	/// by a new announcement.
	#[moq_clock::test]
	async fn an_answer_read_after_a_replacement_is_refused() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let res = answer_after(version, |selection| selection.replace()).await;
			assert!(matches!(res, Some(Err(Error::Unroutable))), "{version:?}: {res:?}");
			let res = answer_after(version, |selection| {
				selection.withdraw();
				selection.replace();
			})
			.await;
			assert!(matches!(res, Some(Err(Error::Unroutable))), "{version:?}: {res:?}");
		}
	}

	/// A withdrawal alone leaves the SUBSCRIBE to its answer: the publisher concludes a
	/// subscription in flight, so a track that finished as its broadcast ended arrives whole.
	#[moq_clock::test]
	async fn an_answer_read_after_a_withdrawal_is_kept() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let res = answer_after(version, |selection| selection.withdraw()).await;
			assert!(matches!(res, Some(Ok(None))), "{version:?}: {res:?}");
		}
	}

	/// Lite-07's SUBSCRIBE_START says where the track is, so a reader knows its largest
	/// frame before any group arrives. The FIN with no SUBSCRIBE_END then aborts the track,
	/// and the stream ends with it.
	#[moq_clock::test]
	async fn subscribe_start_reports_the_largest() {
		let version = Version::Lite07;
		let largest = track::Position { group: 4, frame: 2 };
		let mut responses = Vec::new();
		lite::SubscribeResponse::Start(lite::SubscribeStart {
			largest: Some(largest),
			starts: vec![Some(4)],
		})
		.encode(
			&mut crate::coding::Encoder::new(&mut responses, version.into()),
			version,
		)
		.unwrap();
		let session = crate::lite::test_transport::ScriptedSession::eof(responses);
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			subscriber,
			path: Path::from_static("room"),
			epoch: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("video").unwrap();
		let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
		let mut reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
		};
		kio::wait(|waiter| kio::Task::poll(&mut running, waiter)).await;

		assert_eq!(reader.largest().await, Some(largest));
		assert_eq!(reader.largest().await, None, "the stream ends with the aborted track");
		assert!(matches!(reader.recv_group().await, Err(Error::ProtocolViolation)));
	}

	/// A SUBSCRIBE_DROP ends a group still arriving now, with its Error Code on lite-07.
	/// Earlier versions carry an application code there, so the same bytes read as
	/// OLD.
	#[moq_clock::test]
	async fn a_subscribe_drop_ends_an_arriving_group_with_its_code() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut responses = Vec::new();
			let code = crate::coding::StreamCodes::encode_stream_code(&Version::Lite07, &crate::StreamError::App(5));
			for resp in [
				lite::SubscribeResponse::Start(lite::SubscribeStart {
					largest: None,
					starts: vec![Some(0)],
				}),
				lite::SubscribeResponse::Drop(lite::SubscribeDrop {
					start: 2,
					end: 2,
					error: code.into(),
				}),
			] {
				resp.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
			}
			// No FIN: the subscription stays open, so only the DROP ends the group.
			let session = crate::lite::test_transport::ScriptedSession::new(responses);
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_hop: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				subscriber,
				path: Path::from_static("room"),
				epoch: None,
				name: "video".to_string(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let request = broadcast.reserve_track("video").unwrap();
			let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
			let producer = serving.serving.clone();
			let mut arriving = serving.serving.create_group(group::Info { sequence: 2 }).unwrap();
			arriving.write_frame(crate::Timestamp::ZERO, b"2".as_slice()).unwrap();
			let mut reader = broadcast
				.consume()
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.unwrap();
			let mut group = reader.recv_group().await.unwrap().unwrap();
			assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"2");

			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
			};
			assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());

			let expected = match version {
				Version::Lite07 => crate::StreamError::App(5),
				_ => crate::StreamError::Old,
			};
			let res = group.read_frame().now_or_never();
			assert!(
				matches!(&res, Some(Err(Error::Stream(err))) if *err == expected),
				"{version:?}: {res:?}"
			);

			// The track goes on: the next group still reaches the reader.
			let mut following = producer.create_group(group::Info { sequence: 3 }).unwrap();
			following.write_frame(crate::Timestamp::ZERO, b"3".as_slice()).unwrap();
			let mut next = reader
				.recv_group()
				.now_or_never()
				.expect("the next group")
				.unwrap()
				.unwrap();
			assert_eq!(next.sequence, 3, "{version:?}");
			assert_eq!(next.read_frame().await.unwrap().unwrap().payload.as_ref(), b"3");
		}
	}

	/// Drive the subscriber with a peer's response bytes followed by FIN.
	async fn check_subscription_fin(version: Version, responses: Vec<u8>, clean: bool) {
		let session = crate::lite::test_transport::ScriptedSession::eof(responses);
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			subscriber,
			path: Path::from_static("room"),
			epoch: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("video").unwrap();
		let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
		let mut reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
		};
		assert!(
			kio::Task::poll(&mut running, &kio::Waiter::noop()).is_ready(),
			"{version:?}: FIN must settle immediately"
		);
		if clean {
			assert!(reader.recv_group().await.unwrap().is_none());
		} else {
			assert!(matches!(reader.recv_group().await, Err(Error::ProtocolViolation)));
		}
	}

	fn fin_responses(version: Version, started: bool, clean: bool) -> Vec<u8> {
		let mut responses = Vec::new();
		if started {
			lite::SubscribeResponse::Start(lite::SubscribeStart {
				largest: None,
				starts: vec![Some(0)],
			})
			.encode(
				&mut crate::coding::Encoder::new(&mut responses, version.into()),
				version,
			)
			.unwrap();
		}
		if clean {
			lite::SubscribeResponse::End(lite::SubscribeEnd { group: 0 })
				.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
		}
		responses
	}

	#[moq_clock::test]
	async fn bare_fin_requires_subscribe_end() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for (started, clean) in [(false, false), (true, false), (false, true)] {
				check_subscription_fin(version, fin_responses(version, started, clean), clean).await;
			}
		}
	}

	#[moq_clock::test]
	#[ignore = "requires Bun; run by just test bare-fin in interop CI"]
	async fn bare_fin_interop() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for (started, clean) in [(false, false), (true, false), (false, true)] {
				let responses = fin_responses(version, started, clean);
				let responses =
					crate::test_interop::fin(crate::Version::from(version).alpn(), started, clean, responses);
				check_subscription_fin(version, responses, clean).await;
			}
		}
	}

	/// Removing a subscription both stops delivery and releases the session's handle
	/// on the producer, so the track (its cached groups, its stats subscription) ends
	/// rather than outliving the subscription it belonged to.
	#[moq_clock::test]
	async fn unsubscribe_drops_the_datagram_and_releases_the_producer() {
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::default()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let broadcast = crate::broadcast::Info::new().produce();
		// The broadcast keeps only a weak handle, so the map below owns the only strong
		// `track::Producer`: dropping it is what ends the track.
		// Untimed, as the subscriber drops lite-05 timestamps.
		let producer = broadcast
			.create_track("datagrams", crate::track::Info::default())
			.unwrap();
		let mut received = producer.subscribe(None);
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry {
				producer,
				framing: lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
				},
				tail: Default::default(),
			},
		);

		let payload = |sequence, size| {
			lite::Datagram {
				subscribe: 7,
				sequence,
				timestamp: Some(sequence),
				payload: bytes::Bytes::from(vec![0; size]),
				inflated: None,
				ietf: None,
			}
			.encode_bytes(VERSION)
			.unwrap()
		};
		subscriber.route_datagram(payload(1, 1)).unwrap();
		assert_eq!(
			received
				.recv_datagram()
				.now_or_never()
				.unwrap()
				.unwrap()
				.unwrap()
				.sequence,
			1
		);

		let oversized = payload(2, 1198);
		assert_eq!(oversized.len(), 1201);
		subscriber.route_datagram(oversized).unwrap();
		assert!(
			received.recv_datagram().now_or_never().is_none(),
			"oversized body was delivered"
		);
		let boundary = payload(3, 1197);
		assert_eq!(boundary.len(), 1200);
		subscriber.route_datagram(boundary).unwrap();
		assert_eq!(received.recv_datagram().await.unwrap().unwrap().sequence, 3);

		subscriber.remove_subscribe(7);
		subscriber.route_datagram(payload(4, 1)).unwrap();
		// Dropping the last producer is an abrupt teardown, so the track resolves with
		// `Dropped` rather than parking. A route that outlived the removal would keep the
		// producer alive and leave this pending forever.
		assert!(
			matches!(received.recv_datagram().now_or_never(), Some(Err(Error::Dropped))),
			"the track outlived its subscription"
		);
	}

	/// A lite-05 subscribe still waiting on TRACK_INFO is not in the subscribe map.
	/// Dropping that machine must reject the origin request with the session's error.
	#[moq_clock::test]
	async fn session_death_rejects_a_track_waiting_for_info() {
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::default()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let consumer = broadcast.consume();
		let mut waiting = std::pin::pin!(consumer.track("video").unwrap().subscribe(None));
		assert!(
			futures::poll!(waiting.as_mut()).is_pending(),
			"waiting on the publisher"
		);
		let request = dynamic.requested_track().now_or_never().unwrap().expect("request");

		let run = TrackServeRun::new(
			TrackServe {
				validity: Default::default(),
				route: None,
				subscriber: subscriber.clone(),
				path: Path::from_static("room"),
				epoch: None,
				name: "video".to_string(),
			},
			request,
		);
		let death = Error::Session(crate::SessionError::App(7));
		subscriber.note_end(&death);
		drop(run);

		let ended = waiting.now_or_never().expect("subscribe must end");
		assert!(
			matches!(ended, Err(Error::Session(crate::SessionError::App(7)))),
			"waiting track did not end with the session error"
		);
	}

	#[moq_clock::test]
	async fn announce_interest_without_credit_keeps_the_session_alive() {
		use std::sync::atomic::Ordering;
		for version in [Version::Lite04, Version::Lite07] {
			let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let interest = origin.consume().announced();
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).without_credit();
			let log = session.log.clone();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session),
				origin,
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				cost: Some(1),
				peer_hop: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let mut driver = SubscriberDriver::new(subscriber, Vec::new(), false);
			assert!(driver.poll(&kio::Waiter::noop()).is_pending());
			assert_eq!(log.request_attempts.load(Ordering::Relaxed), 1, "{version}");
			moq_clock::sleep(std::time::Duration::from_secs(20)).await;
			assert!(driver.poll(&kio::Waiter::noop()).is_pending());
			assert_eq!(
				log.request_attempts.load(Ordering::Relaxed),
				1,
				"{version}: interest was not retried"
			);
			assert!(log.closes().is_empty());
			assert!(interest.is_synced(), "a refused interest releases its snapshot");
		}
	}

	#[moq_clock::test]
	async fn a_claimed_fetch_waits_only_while_wanted() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for end in ["credit", "unused", "retired"] {
				let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).without_credit();
				let subscriber = Subscriber::new(SubscriberConfig {
					runtime: crate::time::sim(),
					session: crate::driver::Transport::new(session.clone()),
					origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
					recv_bandwidth: None,
					version,
					peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
					peer_hop: None,
					cost: None,
					going_away: Default::default(),
					auth: crate::auth::Link::new(false),
				});
				let route = kio::Producer::new(());
				let serve = TrackServe {
					validity: crate::model::broadcast::RequestValidity::token(Some(route.consume())),
					route: None,
					subscriber,
					path: Path::from_static("room"),
					epoch: None,
					name: "video".into(),
				};
				let info = track::Info::default().with_timescale(None);
				let track = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info.clone());
				let dynamic = track.requests();
				let fetch = track.consume().fetch_group(4, None);
				let request = dynamic.requested_fill_group().await.unwrap();
				let mut run = FetchServeRun::new(serve, request, info);
				let waiter = kio::Waiter::noop();
				assert!(kio::Task::poll(&mut run, &waiter).is_pending(), "{version}: {end}");
				assert_eq!(session.log.bi_opens(), 0);
				if end == "credit" {
					session.grant_credit();
					assert!(kio::Task::poll(&mut run, &waiter).is_pending());
					assert_eq!(session.log.bi_opens(), 1, "credit releases the FETCH");
				}
				if end == "retired" {
					drop(route);
					assert!(kio::Task::poll(&mut run, &waiter).is_ready());
					assert!(matches!(fetch.await, Err(Error::Unroutable)));
				} else {
					drop(fetch);
					assert!(kio::Task::poll(&mut run, &waiter).is_ready(), "{version}: {end}");
				}
			}
		}
	}

	/// A track still waiting on TRACK_INFO ends once nobody wants it, whether its TRACK
	/// transport header never finished, its request is stuck in
	/// send, or its peer never answers.
	/// Otherwise every request a relay's front abandons (a failover, a reader leaving)
	/// holds a task, a stream, and the track until the peer answers, which may be never.
	#[moq_clock::test]
	async fn an_unused_track_stops_waiting_for_info() {
		// A closed gate holds the TRACK request in its send; an open one sends it to a
		// peer that never answers.
		let (closed, open) = (kio::Producer::new(false), kio::Producer::new(true));
		for (stage, session) in [
			("open", SinkSession::default()),
			("send", SinkSession::gated_bi(closed.consume())),
			("read", SinkSession::gated_bi(open.consume())),
		] {
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version: VERSION,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_hop: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});

			let broadcast = crate::broadcast::Info::new().produce();
			let mut dynamic = broadcast.requests();
			let consumer = broadcast.consume();
			let mut waiting = Box::pin(consumer.track("video").unwrap().subscribe(None));
			assert!(
				futures::poll!(waiting.as_mut()).is_pending(),
				"{stage}: waiting on the publisher"
			);
			let request = dynamic.requested_track().now_or_never().unwrap().expect("request");

			let mut run = TrackServeRun::new(
				TrackServe {
					validity: Default::default(),
					route: None,
					subscriber,
					path: Path::from_static("room"),
					epoch: None,
					name: "video".to_string(),
				},
				request,
			);
			let waiter = kio::Waiter::noop();
			assert!(
				kio::Task::poll(&mut run, &waiter).is_pending(),
				"{stage}: waits while the track is wanted"
			);

			drop(waiting);
			assert!(
				kio::Task::poll(&mut run, &waiter).is_ready(),
				"{stage}: an unused track kept waiting on TRACK_INFO"
			);
		}
	}

	/// A lite-07 source of `room` over `session`, a consumer to list it through, and the
	/// route that keeps it served.
	fn listed_source(
		session: SinkSession,
		auth: crate::auth::Link,
		validity: crate::model::broadcast::RequestValidity,
	) -> (SourceServe, crate::broadcast::Consumer, kio::Producer<()>) {
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth,
		});
		let broadcast = crate::broadcast::Info::new().produce().with_validity(validity);
		let consumer = broadcast.consume();
		let dynamic = broadcast.requests();
		let route = kio::Producer::new(());
		let serve = SourceServe::new(
			subscriber,
			MintedSource {
				path: Path::from_static("room"),
				epoch: None,
				source: crate::model::broadcast::SourceGuard::new(broadcast),
				dynamic,
				route: route.consume(),
			},
		);
		(serve, consumer, route)
	}

	/// A listing is a subscribe: it waits for a token that may cover the broadcast before
	/// syncing its readers or asking the peer, which would refuse it until then.
	#[moq_clock::test]
	async fn a_listing_waits_for_its_token() {
		let auth = crate::auth::Link::new(true);
		let token = auth.present(bytes::Bytes::from_static(b"new"), false).unwrap();
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth, Default::default());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 0, "asked the peer before the token settled");
		assert!(
			tracks.next().now_or_never().is_none(),
			"synced before the token settled"
		);

		drop(token);
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(matches!(
			tracks.next().now_or_never(),
			Some(Ok(Some(crate::broadcast::TracksEvent::Synced)))
		));
	}

	/// An instance retired while its listing waits for a token lists nothing: a TRACKS
	/// request without an epoch would list its replacement. The source ends with it.
	#[moq_clock::test]
	async fn a_listing_of_a_retired_instance_never_reaches_the_wire() {
		let auth = crate::auth::Link::new(true);
		let token = auth.present(bytes::Bytes::from_static(b"new"), false).unwrap();
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let selected = kio::Producer::new(());
		let validity = crate::model::broadcast::RequestValidity::Route(selected.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth, validity);
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		drop(selected);
		drop(token);
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_ready());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unroutable))));
		assert_eq!(session.log.bi_opens(), 0);
	}

	/// A listing on the wire but unanswered when another instance takes the path lists
	/// nothing: the peer may list that instance.
	#[moq_clock::test]
	async fn an_unanswered_listing_of_a_replaced_instance_is_refused() {
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let mut selection = crate::model::TestSelection::new();
		let (mut serve, consumer, _route) =
			listed_source(session.clone(), crate::auth::Link::new(false), selection.validity());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(matches!(
			tracks.next().now_or_never(),
			Some(Ok(Some(crate::broadcast::TracksEvent::Synced)))
		));

		selection.replace();
		let _ = kio::Task::poll(&mut serve, &kio::Waiter::noop());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unroutable))));
		assert_eq!(session.log.resets(), vec![crate::StreamError::Unroutable.to_code()]);
	}

	/// A listing outside the session's scope is refused before it reaches the wire.
	#[moq_clock::test]
	async fn a_listing_outside_the_scope_is_refused() {
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant::default());
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth, Default::default());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unauthorized))));
		assert_eq!(session.log.bi_opens(), 0);
	}

	/// A narrower scope resets a live listing with UNAUTHORIZED rather than keep listing
	/// what the session no longer allows, and leaves the session up.
	#[moq_clock::test]
	async fn a_narrower_limit_resets_a_listing() {
		let auth = crate::auth::Link::new(false);
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth.clone(), Default::default());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(matches!(
			tracks.next().now_or_never(),
			Some(Ok(Some(crate::broadcast::TracksEvent::Synced)))
		));

		auth.handle().authorize(&crate::auth::Grant::default());
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unauthorized))));
		assert_eq!(session.log.resets(), vec![crate::StreamError::Unauthorized.to_code()]);
		assert!(session.log.closes().is_empty());
	}

	/// A track waiting on an unanswered token ends once nobody wants it, rather than
	/// holding its task and track until a token that may never be answered.
	#[moq_clock::test]
	async fn an_unused_track_stops_waiting_for_auth() {
		let auth = crate::auth::Link::new(true);
		let _token = auth.present(bytes::Bytes::from_static(b"new"), false).unwrap();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::default()),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth,
		});

		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let consumer = broadcast.consume();
		let mut waiting = Box::pin(consumer.track("video").unwrap().subscribe(None));
		assert!(futures::poll!(waiting.as_mut()).is_pending());
		let request = dynamic.requested_track().now_or_never().unwrap().expect("request");

		let mut run = TrackServeRun::new(
			TrackServe {
				validity: Default::default(),
				route: None,
				subscriber,
				path: Path::from_static("room"),
				epoch: None,
				name: "video".to_string(),
			},
			request,
		);
		let waiter = kio::Waiter::noop();
		assert!(
			kio::Task::poll(&mut run, &waiter).is_pending(),
			"waits on the token while the track is wanted"
		);

		drop(waiting);
		assert!(
			kio::Task::poll(&mut run, &waiter).is_ready(),
			"an unused track kept waiting on its token"
		);
	}

	/// `establish` puts exactly one SUBSCRIBE on the wire, and the id is registered
	/// before any of it reaches the transport.
	///
	/// Both halves matter: a second stream re-requests the same id, which the peer is
	/// free to serve twice, and a late insert loses the race with a publisher that
	/// serves its first group the instant it reads the request (`recv_group` drops a
	/// group whose id isn't in the map yet).
	#[moq_clock::test]
	async fn establish_sends_one_registered_subscribe() {
		// Writes park until this opens, so the assertions below run at the exact moment
		// the request would hit the wire.
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_bi(gate.consume());

		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let subscribes = subscriber.subscribes.clone();
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			subscriber,
			path: Path::from_static("room/host"),
			epoch: None,
			name: "catalog.json".to_string(),
		};

		let broadcast = crate::broadcast::Info::new().produce();
		let mut producer = broadcast
			.create_track("catalog.json", crate::track::Info::timed())
			.unwrap();
		let mut sub = Sub::None;
		let mut establish = std::pin::pin!(serve.establish(&mut producer, &mut sub, Subscription::default()));

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
			crate::coding::decode_buf(&mut wire, VERSION, |r, v| lite::ControlType::decode(r, v)).unwrap(),
			lite::ControlType::Subscribe
		);
		let msg = crate::coding::decode_from(&mut wire, VERSION, lite::Subscribe::decode).unwrap();
		assert_eq!(msg.id, 0);
		assert_eq!(msg.track, "catalog.json");
		assert!(wire.is_empty(), "a second SUBSCRIBE trailed the first");
	}

	/// A narrower limit landing while the SUBSCRIBE is still opening resets it with
	/// UNAUTHORIZED, like a live one, and releases its id.
	#[moq_clock::test]
	async fn a_narrower_limit_resets_a_subscribe_still_opening() {
		// Writes park, so the SUBSCRIBE is on an open stream but not yet flushed.
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_bi(gate.consume());
		let auth = crate::auth::Link::new(false);

		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: auth.clone(),
		});
		let subscribes = subscriber.subscribes.clone();
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			subscriber,
			path: Path::from_static("room/host"),
			epoch: None,
			name: "audio".to_string(),
		};

		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("audio").unwrap();
		let mut serving = ServeLoop::new(&serve, request, Default::default());
		let establish =
			serve.prepare_establish(&mut serving.serving, Subscription::default(), vec![lite::Range::OLDEST]);
		serving.mode = ServeMode::Establish(establish);
		let gate_run = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate: gate_run,
		};

		assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1, "the SUBSCRIBE stream is open");

		auth.handle().authorize(&crate::auth::Grant::default());
		assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_ready());

		let unauthorized = crate::StreamError::Unauthorized.to_code();
		assert_eq!(session.log.resets(), vec![unauthorized]);
		assert!(subscribes.lock().is_empty(), "the id outlived its subscription");
		drop(gate);
	}

	/// Everything a `handle_subscription` test needs to stay alive for the call.
	struct Harness {
		serve: TrackServe,
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
			let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin,
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_hop: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let broadcast = crate::broadcast::Info::new().produce();
			let producer = broadcast
				.create_track("catalog.json", crate::track::Info::timed())
				.unwrap();

			Self {
				serve: TrackServe {
					validity: Default::default(),
					route: None,
					subscriber,
					path: Path::from_static("room/host"),
					epoch: None,
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

	#[moq_clock::test]
	async fn internal_cancel_without_a_recorded_session_end_is_cancel() {
		let h = Harness::new(VERSION);
		assert!(matches!(h.serve.subscriber.end_reason(), Error::Cancel));
	}

	#[moq_clock::test]
	async fn internal_serve_preserves_an_abort_from_a_live_clone() {
		for dynamic_first in [true, false] {
			let h = Harness::new(VERSION);
			let request = h._broadcast.reserve_track("pending").unwrap();
			let mut serving = ServeLoop::new(&h.serve, request, Default::default());
			let other = h._broadcast.create_track("other", None).unwrap();
			if !dynamic_first {
				// Isolate the subscription-changed arm, which can observe a concurrent
				// abort after the dynamic-request poll returned Pending.
				serving.dynamic = other.requests();
			}
			serving.serving.clone().abort(Error::ProtocolViolation).unwrap();
			assert!(matches!(
				serving.poll(&h.serve, &kio::Waiter::noop()),
				Poll::Ready(ServeEnd::GiveBack(Error::ProtocolViolation))
			));
		}
	}

	/// A session that dies hands a live track back with the session's error, but finishes one
	/// whose subscription the publisher already ended, as the driver's cleanup does.
	#[moq_clock::test]
	async fn internal_session_end_gives_back_live_and_finishes_ended() {
		use crate::transport::poll::Session as _;
		for ended in [false, true] {
			let (session, peer) = crate::transport::memory::pair(Default::default());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version: VERSION,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_hop: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				subscriber,
				path: Path::from_static("room"),
				epoch: None,
				name: "video".into(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let mut serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), Default::default());
			let _reader = serving.serving.subscribe(None);
			assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());
			// Held for the whole round: a dropped tail settles at once.
			let tail = kio::Producer::new(Tail::default());
			if ended {
				serving.mode = ServeMode::Tail {
					settle: Settle::new(&serve.subscriber.runtime, tail.consume()),
					owed: Some(std::iter::once(0..1).collect()),
					track: true,
				};
				assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());
			}
			peer.close(crate::SessionError::Unauthorized.to_code(), "revoked");
			let end = serving.poll(&serve, &kio::Waiter::noop());
			match ended {
				true => assert!(matches!(end, Poll::Ready(ServeEnd::Finished))),
				false => assert!(matches!(
					end,
					Poll::Ready(ServeEnd::GiveBack(Error::Session(crate::SessionError::Unauthorized)))
				)),
			}
		}
	}

	/// A publisher that finishes the track stops reading its subscribe stream, so a
	/// SUBSCRIBE_UPDATE written then fails. The track still ends with the publisher's
	/// finish; a reset still hands it back with the reset's error.
	#[moq_clock::test]
	async fn an_update_the_publisher_stopped_reading_defers_to_its_end() {
		use crate::transport::poll::{RecvStream as _, SendStream as _, Session as _};
		for reset in [false, true] {
			let version = Version::Lite06;
			let (session, peer) = crate::transport::memory::pair(Default::default());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_hop: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				subscriber,
				path: Path::from_static("room"),
				epoch: None,
				name: "video".into(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let mut serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), Default::default());
			let mut reader = serving.serving.subscribe(None);
			assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());
			assert!(matches!(serving.sub, Sub::Active(_)), "the SUBSCRIBE is on the wire");

			// The publisher ends the subscription and stops reading the stream.
			let (mut send, mut recv) = kio::wait(|waiter| peer.poll_accept_bi(waiter)).await.unwrap();
			recv.stop(crate::StreamError::Cancel.to_code());
			if reset {
				send.reset(crate::StreamError::NotFound.to_code());
			} else {
				let mut end = Vec::new();
				lite::SubscribeResponse::End(lite::SubscribeEnd { group: 0 })
					.encode(&mut crate::coding::Encoder::new(&mut end, version.into()), version)
					.unwrap();
				kio::wait(|waiter| send.poll_write(waiter, &end)).await.unwrap();
				send.finish().unwrap();
			}

			// The update races it, and its write fails.
			reader.update(Subscription::default().with_priority(1)).unwrap();
			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
			};
			assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_ready());
			let end = reader.recv_group().now_or_never().expect("the track ended").map(|_| ());
			match reset {
				false => assert!(matches!(end, Ok(())), "{end:?}"),
				true => assert!(
					matches!(end, Err(Error::Stream(crate::StreamError::NotFound))),
					"{end:?}"
				),
			}
		}
	}

	/// The model's groups map to wire ranges starting and ending at a group's first frame,
	/// with empty ranges dropped. Off by one here would silently drop or duplicate a group
	/// at every relay hop.
	#[test]
	fn wire_ranges_name_whole_groups() {
		let group = |group| Some(lite::Anchor::Absolute(Position::group(group)));
		let ranges = wire_ranges(&Subscription::default().with_range(5..6), None).unwrap();
		assert_eq!(
			ranges,
			Some(vec![lite::Range {
				start: group(5),
				end: group(6),
			}])
		);
		assert_eq!(wire_ranges(&Subscription::default().with_end(0), None).unwrap(), None);

		// A time anchor needs the track's timescale.
		let timed = Subscription {
			ranges: vec![track::Range {
				start: Anchor::Time(Timestamp::from_millis(5).unwrap()),
				end: None,
			}],
			..Default::default()
		};
		assert!(matches!(wire_ranges(&timed, None), Err(Error::TimestampMismatch)));
	}

	/// A wire without ranges asks from group 0 when no single start covers both a count
	/// back from the latest group and an absolute start.
	#[test]
	fn wire_span_covers_mixed_starts_from_group_zero() {
		let mixed = Subscription {
			ranges: vec![
				track::Range {
					start: Anchor::Relative(0),
					end: None,
				},
				track::Range::groups(5..6),
			],
			..Default::default()
		};
		let span = wire_span(mixed);
		assert_eq!((span.start(), span.end()), (Some(0), None));

		let relative = Subscription {
			ranges: vec![track::Range {
				start: Anchor::Relative(1),
				end: None,
			}],
			..Default::default()
		};
		assert_eq!(wire_span(relative).start(), None);
	}

	/// A subscription that asks for nothing opens nothing.
	///
	/// An end of group 0 is the empty range: nothing sorts below it. The wire cannot
	/// say that, and the nearest thing it can say is "through group 0", which would
	/// deliver the single group the caller excluded.
	#[moq_clock::test]
	async fn an_empty_range_opens_no_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		let empty = Subscription::default().with_end(0);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "must not open a subscription");
		assert!(h.wire().is_empty(), "nothing reached the wire");
	}

	/// Demand collapsing to nothing cancels the upstream rather than sending a bound
	/// that means the opposite.
	#[moq_clock::test]
	async fn an_empty_range_cancels_a_live_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(Subscription::default()), true)
			.await
			.unwrap();
		assert!(matches!(sub, Sub::Active(_)), "the first subscriber opens one");
		let established = h.wire().len();

		let empty = Subscription::default().with_end(0);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
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
	#[moq_clock::test]
	async fn a_nonzero_empty_range_opens_no_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		let empty = Subscription::default().with_start(5).with_end(5);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "must not open a subscription");
		assert!(h.wire().is_empty(), "nothing reached the wire");
	}

	/// Demand collapsing to an empty range mid-track cancels the upstream, the same way
	/// it does at the first position.
	#[moq_clock::test]
	async fn a_nonzero_empty_range_cancels_a_live_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(Subscription::default()), true)
			.await
			.unwrap();
		assert!(matches!(sub, Sub::Active(_)), "the first subscriber opens one");
		let established = h.wire().len();

		let empty = Subscription::default().with_start(5).with_end(5);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "the upstream must be canceled");
		assert_eq!(h.wire().len(), established, "no SUBSCRIBE_UPDATE inverting the range");
	}

	/// A lite-07 FIN on unchanged bounded ranges covers them even when a group the publisher
	/// sent, such as a lost datagram, never arrived: subscribing again would replay the rest
	/// and repeat for as long as it stays undeliverable. Ranges an update changed are asked
	/// again, since the FIN may predate the update.
	#[moq_clock::test]
	async fn a_covered_fin_short_of_a_lost_group_resubscribes_only_after_an_update() {
		for updated in [false, true] {
			let h = Harness::new(Version::Lite07);
			let request = h._broadcast.reserve_track("lossy").unwrap();
			let mut serving = ServeLoop::new(&h.serve, request, track::Info::default());
			let track = h._broadcast.consume().track("lossy").unwrap();
			let _reader = track.subscribe(Subscription::default().with_range(0..2)).await.unwrap();
			assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
			let Sub::Active(active) = &mut serving.sub else {
				panic!("the reader opens the upstream")
			};
			assert!(active.bounded);
			active.updated = updated;
			active.served = Some(0);
			// Group 0 arrived; group 1 was sent and lost.
			let Ok(mut tail) = active.tail.write() else {
				panic!("ledger closed")
			};
			tail.account(0..1, h.serve.subscriber.runtime.now());
			drop(tail);
			serving.mode = ServeMode::Tail {
				settle: Settle::new(&h.serve.subscriber.runtime, active.tail.consume()),
				owed: active.covered(),
				track: false,
			};
			assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
			assert!(
				matches!(serving.mode, ServeMode::Tail { .. }),
				"{updated}: waits out the grace"
			);

			moq_clock::sleep(Duration::from_secs(120)).await;
			let _ = serving.poll(&h.serve, &kio::Waiter::noop());
			assert_eq!(serving.covered.is_some(), !updated, "{updated}");
			assert_eq!(
				matches!(serving.sub, Sub::None),
				!updated,
				"{updated}: subscribes again"
			);
		}
	}

	/// Concurrent joins over the refresh ratio send one SUBSCRIBE_UPDATE upstream, a new
	/// group past the largest sends the next, and every update restates the latest.
	#[moq_clock::test]
	async fn refresh_joins_coalesce_upstream() {
		let h = Harness::new(Version::Lite07);
		let request = h._broadcast.reserve_track("refresh").unwrap();
		let mut serving = ServeLoop::new(&h.serve, request, track::Info::default().with_refresh(1));
		let track = h._broadcast.consume().track("refresh").unwrap();
		let _baseline = track.subscribe(None).await.unwrap();
		assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		assert!(matches!(serving.sub, Sub::Active(_)), "the reader opens the upstream");
		let established = h.wire().len();

		let arrive = |serving: &ServeLoop, sequence| {
			let mut group = serving.serving.create_group(group::Info { sequence }).unwrap();
			for frame in [&b"key"[..], b"abc", b"d"] {
				group.write_frame(None, frame).unwrap();
			}
			group.finish().unwrap();
		};
		let wanted = |h: &Harness| {
			let wire = h.wire();
			let mut wire = &wire[established..];
			let mut wanted = Vec::new();
			while !wire.is_empty() {
				let msg =
					crate::coding::decode_buf(&mut wire, Version::Lite07, |r, v| lite::SubscribeUpdate::decode(r, v))
						.unwrap();
				wanted.extend(msg.wanted_group);
			}
			wanted
		};

		// The deltas outweigh the first frame, so every join asks for group 1.
		arrive(&serving, 0);
		let mut joins = Vec::new();
		for _ in 0..8 {
			joins.push(track.subscribe(None).await.unwrap());
			assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		}
		assert_eq!(wanted(&h), [1], "joins coalesce into one request");

		arrive(&serving, 1);
		joins.push(track.subscribe(None).await.unwrap());
		assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(wanted(&h), [1, 2], "the requested group re-arms the next request");

		// A parameter change restates the pending wanted group rather than clearing it.
		let _urgent = track.subscribe(Subscription::default().with_priority(9)).await.unwrap();
		assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(wanted(&h), [1, 2, 2], "every update restates the wanted group");
	}

	/// A buffered SUBSCRIBE_START describes the demand its SUBSCRIBE carried, so
	/// it applies exactly while the current start matches that demand: an update
	/// that moves the start makes it stale (applying it could reopen a range the
	/// publisher no longer serves, or clamp one it still does), and an update
	/// that moves back restores it (the publisher declared that range gone and
	/// sends no replacement START).
	#[moq_clock::test]
	async fn buffered_start_applies_iff_demand_matches() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		let demand = |group: u64| Some(Subscription::default().with_start(group));
		let applies = |sub: &Sub| matches!(sub, Sub::Active(active) if active.start == active.requested);

		// Establish from group 3; the peer's START is considered in flight.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true)
			.await
			.unwrap();
		assert!(applies(&sub), "a fresh subscription accepts its START");

		// An end-only update leaves the start intact: the START stays valid.
		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(Subscription::default().with_start(3).with_end(9)),
				true,
			)
			.await
			.unwrap();
		assert!(applies(&sub), "an unmoved start keeps the START applicable");

		// The start moves: a buffered START is stale while it sits elsewhere.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(8), true)
			.await
			.unwrap();
		assert!(!applies(&sub), "a moved start must invalidate a buffered START");

		// The start returns: the declaration matches the demand again, so the
		// publisher's skip (it sends no replacement START) must land.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true)
			.await
			.unwrap();
		assert!(applies(&sub), "demand returning restores the START");
	}

	/// The permanent-miss floor follows the demand in every direction: an update
	/// that moves the start forward retires the skipped range (the publisher
	/// stops serving it and no fresh START says so), one that moves it backward
	/// reopens it, and dropping to the live edge clears it entirely.
	#[moq_clock::test]
	async fn updates_move_the_declared_floor_both_ways() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		let demand = |group: u64| Some(Subscription::default().with_start(group));

		// Establish from group 5: the floor tracks the request until START lands.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(5), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), Some(5));

		// Forward: a reader waiting in [5, 8) must fail over, not stall.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(8), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), Some(8));

		// Backward: the reopened range must stop being a permanent miss.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), Some(3));

		// Live edge: the floor is unknown until the next declaration, so a
		// group below the stale one must not be a permanent miss.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(Subscription::default()), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_sequence(), None);
	}

	/// A second announce for a live path is a protocol error whatever its hops say. It
	/// must be caught before the reflection drops, because the caller has already bound
	/// an announce id to it: dropping it silently leaves that id pointing at a path
	/// owned by the earlier announce, and the `ANNOUNCE_END` that follows retires the
	/// wrong route.
	#[moq_clock::test]
	async fn a_double_announce_is_an_error_even_when_reflected() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let mut announced = Announced::default();
		assert!(
			subscriber
				.start_announce(
					path.clone(),
					None,
					crate::Hops::new(),
					crate::origin::Cost::default(),
					0,
					Some(assigned),
					&mut announced,
				)
				.unwrap()
		);

		// The same path again, this time with a chain that names the sender.
		let mut reflected = crate::Hops::new();
		reflected.push(assigned).unwrap();
		assert!(
			matches!(
				subscriber.start_announce(
					path.clone(),
					None,
					reflected,
					crate::origin::Cost::default(),
					0,
					Some(assigned),
					&mut announced,
				),
				Err(Error::ProtocolViolation)
			),
			"the double announce must be reported, not silently dropped",
		);
	}

	/// A serve pass polls only the routes whose request queue woke. The driver runs a
	/// pass on every wake, which is once per group the session carries, so polling
	/// every announced route there made each group cost the whole announce set.
	#[moq_clock::test]
	async fn a_request_readies_only_its_route() {
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(crate::Hop::new(777).unwrap()),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let mut announced = Announced::default();
		for i in 0..64 {
			assert!(
				subscriber
					.start_announce(
						Path::new(&format!("room/{i}")).unwrap(),
						None,
						crate::Hops::new(),
						crate::origin::Cost::default(),
						0,
						None,
						&mut announced,
					)
					.unwrap()
			);
		}

		// Attaching queues each route for its first pass, and that pass leaves none queued.
		assert_eq!(announced.ready.len(), 64);
		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		assert!(announced.ready.is_empty());

		let consumer = origin.consume();
		let request = moq_clock::sim::spawn(async move { consumer.request("room/7", None).await });

		let ready = kio::wait(|waiter| announced.ready.poll_pop(waiter)).await.unwrap();
		assert_eq!(ready.as_str(), "room/7");
		assert!(announced.ready.is_empty(), "only the requested route is ready");

		// Hand it back, as its waker did, and serve it.
		announced.ready.try_push(ready).unwrap();
		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		request.await.unwrap().expect("the requested route serves it");
	}

	/// A source minted under a claim goes once nothing holds it: the front that read it
	/// ended, so the session keeps nothing for the path, and a later request asks the
	/// route afresh. Withdrawing the claim still closes a source in use at once.
	#[moq_clock::test]
	async fn an_unheld_source_is_retired() {
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(crate::Hop::new(777).unwrap()),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let path = Path::from_static("pool");
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				path.clone(),
				None,
				crate::Hops::new(),
				crate::origin::Cost::default(),
				0,
				None,
				&mut announced,
			)
			.unwrap();

		// Serve one request for `pool/p`, returning what the requester resolved and the
		// machine serving the source minted for it.
		async fn serve_one(
			origin: &origin::Producer,
			subscriber: &Subscriber,
			announced: &mut Announced,
		) -> (crate::broadcast::Consumer, SourceServe) {
			let consumer = origin.consume();
			let request = moq_clock::sim::spawn(async move { consumer.request("pool/p", None).await });
			let ready = kio::wait(|waiter| announced.ready.poll_pop(waiter)).await.unwrap();
			announced.ready.try_push(ready).unwrap();
			announced.poll_serve(subscriber, &kio::Waiter::noop());
			let minted = subscriber.sources.try_pop().unwrap().expect("a source was minted");
			let resolved = request.await.unwrap().expect("resolves");
			(resolved, SourceServe::new(subscriber.clone(), minted))
		}

		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		let (resolved, mut serve) = serve_one(&origin, &subscriber, &mut announced).await;
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());

		// The requester leaves without reading: its front ends, and the source with it.
		drop(resolved);
		moq_clock::timeout(
			std::time::Duration::from_secs(1),
			kio::wait(|waiter| kio::Task::poll(&mut serve, waiter)),
		)
		.await
		.expect("the unheld source was retired");

		// Nothing cached it for the path: the route is asked again, and withdrawing the
		// claim closes the new source though its requester still holds it.
		let (_resolved, mut serve) = serve_one(&origin, &subscriber, &mut announced).await;
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		announced.withdraw(&path);
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_ready());
	}

	/// A track read before the minted source's serve machine first runs queues for that
	/// machine, rather than the front finding no handler and ending the track `NotFound`.
	#[moq_clock::test]
	async fn a_track_read_before_its_source_is_served_queues() {
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(crate::Hop::new(777).unwrap()),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				Path::from_static("pool"),
				None,
				crate::Hops::new(),
				crate::origin::Cost::default(),
				0,
				None,
				&mut announced,
			)
			.unwrap();
		announced.poll_serve(&subscriber, &kio::Waiter::noop());

		let consumer = origin.consume();
		let request = moq_clock::sim::spawn(async move { consumer.request("pool/p", None).await });
		let ready = kio::wait(|waiter| announced.ready.poll_pop(waiter)).await.unwrap();
		announced.ready.try_push(ready).unwrap();
		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		let resolved = request.await.unwrap().expect("resolves");

		// The front asks the source for the track while its serve machine is still queued.
		let track = resolved.track("video").unwrap();
		let _subscribing = track.subscribe(None);
		moq_clock::sleep(std::time::Duration::from_millis(1)).await;

		let minted = subscriber.sources.try_pop().unwrap().expect("a source was minted");
		let mut serve = SourceServe::new(subscriber.clone(), minted);
		match serve.dynamic.poll_requested_track(&kio::Waiter::noop()) {
			Poll::Ready(Ok(request)) => assert_eq!(request.name(), "video"),
			other => panic!("the track never queued for the source: {:?}", other.map(|r| r.err())),
		}
	}

	/// Every path out of `start_announce` that declines an announce records it first.
	///
	/// The decline paths are a list one edit can fall off the end of, and a miss is silent:
	/// the path reads as free, a later announce takes it, and the declined one's
	/// `ANNOUNCE_END` retires that route instead. This walks a reflection: the chain
	/// already names this session.
	#[moq_clock::test]
	async fn every_declined_announce_is_recorded() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: Version::Lite03,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let mut announced = Announced::default();

		// A chain that already names us is a reflection, declined but still recorded.
		let hops = crate::Hops::try_from(vec![crate::Hop::new(1).unwrap()]).unwrap();
		assert!(
			!subscriber
				.start_announce(
					path.clone(),
					None,
					hops,
					crate::origin::Cost::default(),
					0,
					None,
					&mut announced
				)
				.unwrap(),
			"a chain that already names this session must be declined",
		);

		// Declined, but still the peer's advertisement at that path.
		let mut fresh = crate::Hops::new();
		fresh.push(crate::Hop::new(7).unwrap()).unwrap();
		assert!(
			matches!(
				subscriber.start_announce(
					path.clone(),
					None,
					fresh,
					crate::origin::Cost::default(),
					0,
					None,
					&mut announced
				),
				Err(Error::ProtocolViolation)
			),
			"a declined announce still holds its path, so a second start for it is a violation",
		);
	}

	/// A dropped announce still holds its path until the peer retracts it.
	#[moq_clock::test]
	async fn a_dropped_announce_still_holds_its_path() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let mut announced = Announced::default();
		let mut reflected = crate::Hops::new();
		reflected.push(assigned).unwrap();
		assert!(
			!subscriber
				.start_announce(
					path.clone(),
					None,
					reflected,
					crate::origin::Cost::default(),
					0,
					Some(assigned),
					&mut announced,
				)
				.unwrap(),
			"a chain naming its own sender must be dropped",
		);
		assert!(consumer.get_broadcast("room/host").is_none());

		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::new(7).unwrap()).unwrap();
		let err = subscriber
			.start_announce(
				path.clone(),
				None,
				hops,
				crate::origin::Cost::default(),
				0,
				Some(assigned),
				&mut announced,
			)
			.expect_err("a second start for the peer-owned path must be rejected");
		assert!(matches!(err, Error::ProtocolViolation));
	}

	/// An announce whose chain already names the sender came back through the sender.
	/// Appending it again would name one identity twice, which is tolerated in a lite
	/// chain but is a PROTOCOL_VIOLATION for an IETF peer the route is later forwarded
	/// to, so the route must never enter the model carrying it.
	#[moq_clock::test]
	async fn an_announce_reflected_by_its_sender_is_dropped() {
		let assigned = crate::Hop::new(777).unwrap();

		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// The sender's identity is already in the chain: the route came back through it.
		let mut hops = crate::Hops::new();
		hops.push(assigned).unwrap();

		let mut announced = Announced::default();
		let accepted = subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				hops,
				crate::origin::Cost::default(),
				0,
				Some(assigned),
				&mut announced,
			)
			.unwrap();
		assert!(!accepted, "a chain naming its own sender must not become a route");

		moq_clock::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_none());
	}

	/// A peer that was advertised a local path and announces it back with this
	/// origin's hop in the chain is a reflection, not a new path. The announce
	/// is dropped, the local front keeps serving, and the peer's own
	/// subscription (split-horizon excluded) still reads from it.
	#[moq_clock::test]
	async fn a_reflected_announce_does_not_displace_the_local_front() {
		let relay = crate::Hop::new(1).unwrap();
		let origin = origin::Config::new(relay).produce();
		let assigned = crate::Hop::new(777).unwrap();

		let local = origin.publish("room/host", origin::Route::default()).unwrap();
		let track = local.create_track("video", crate::track::Info::timed()).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"local".as_ref()).unwrap();
		group.finish().unwrap();

		// The peer's own subscription, excluding the hop the server minted for
		// it, is served from the local front before anything is announced back.
		let peer = origin.consume().excluding(assigned);
		let resolved = peer.request("room/host", None).await.expect("resolves");
		let mut sub = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = sub.recv_group().await.expect("recv group").expect("track ended early");
		assert_eq!(
			&group.read_frame().await.expect("read frame").expect("frame").payload[..],
			b"local"
		);

		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// The path as advertised to the peer: our hop is already in the chain.
		let mut hops = crate::Hops::new();
		hops.push(relay).unwrap();
		let mut announced = Announced::default();
		let accepted = subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				hops,
				crate::origin::Cost::default(),
				0,
				Some(assigned),
				&mut announced,
			)
			.unwrap();
		assert!(!accepted, "an announce that already names this origin must be dropped");

		// The local front is still the one at the path, and still serving: the
		// peer's next request joins it rather than minting another.
		let still = peer
			.request("room/host", None)
			.await
			.expect("the local front keeps serving");
		assert!(
			still.is_clone(&resolved),
			"the reflected announce must not replace the local front"
		);

		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"still".as_ref()).unwrap();
		group.finish().unwrap();
		let mut group = sub.recv_group().await.expect("recv group").expect("track ended early");
		assert_eq!(
			&group.read_frame().await.expect("read frame").expect("frame").payload[..],
			b"still"
		);
	}

	/// A peer that declares no identity is marked anonymous (hop 0). The assigned
	/// identity stays on `via` for split-horizon and is never written into the chain.
	#[moq_clock::test]
	async fn assigned_peer_hop_attributes_announces() {
		let session = SinkSession::new(Default::default());
		let assigned = crate::Hop::new(777).unwrap();

		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: Some(assigned),
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// An announce with an empty chain and no responder id: the versions that
		// carry no hop information on the wire.
		let mut announced = Announced::default();
		let accepted = subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				0,
				None,
				&mut announced,
			)
			.unwrap();
		assert!(accepted);

		// The route is announced synchronously: hop 0 on the wire, assigned id local.
		let mut cursor = consumer.announced();
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN]);
		assert!(route.is_anonymous());

		let mut hidden = consumer.excluding(assigned).announced();
		hidden.assert_next_wait();
	}

	/// Lite03 hop-count placeholders stay 0 and count as anonymous; they are not
	/// rewritten with the assigned identity.
	#[moq_clock::test]
	async fn lite03_placeholders_stay_anonymous() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: Version::Lite03,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let hops = crate::Hops::try_from(vec![crate::Hop::UNKNOWN, crate::Hop::UNKNOWN]).unwrap();
		let mut announced = Announced::default();
		assert!(
			subscriber
				.start_announce(
					Path::from_static("room/host"),
					None,
					hops,
					crate::origin::Cost::UNKNOWN,
					0,
					None,
					&mut announced,
				)
				.unwrap()
		);

		let mut cursor = consumer.announced();
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN, crate::Hop::UNKNOWN]);
		assert!(route.is_anonymous());
	}

	/// A publisher that names nobody keeps the anonymous mark, and nothing in front of it:
	/// identity is the epoch's job, so the relay never names the publisher itself. A
	/// reprice on the same connection keeps it and updates in place.
	#[moq_clock::test]
	async fn an_unnamed_publisher_stays_anonymous() {
		let (mut subscriber, consumer) = restart_subscriber(SinkSession::new(Default::default()));

		let mut announced = Announced::default();
		subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				0,
				Some(crate::Hop::UNKNOWN),
				&mut announced,
			)
			.unwrap();

		let mut cursor = consumer.announced();
		moq_clock::sim::yield_now().await;
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN]);

		// A reprice from the same unnamed publisher stays anonymous and in place.
		subscriber
			.update_announce(
				Path::from_static("room/host"),
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				4,
				Some(crate::Hop::UNKNOWN),
				&mut announced,
			)
			.unwrap();
		moq_clock::sim::yield_now().await;
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN]);
	}

	fn restart_subscriber(session: SinkSession) -> (Subscriber, crate::origin::Consumer) {
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		(subscriber, consumer)
	}

	/// A restart re-prices the announced route in place: consumers observe another
	/// active update with the new metadata rather than a retract-and-announce.
	#[moq_clock::test]
	async fn restart_updates_the_route_in_place() {
		let (mut subscriber, consumer) = restart_subscriber(SinkSession::new(Default::default()));

		let mut announced = Announced::default();
		let path = Path::from_static("room/host");
		subscriber
			.start_announce(
				path.clone(),
				None,
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				0,
				Some(crate::Hop::new(7).unwrap()),
				&mut announced,
			)
			.unwrap();

		let mut cursor = consumer.announced();
		moq_clock::sim::yield_now().await;
		cursor.expect_next_active("room/host").await;

		subscriber
			.update_announce(
				path.clone(),
				crate::Hops::new(),
				crate::origin::Cost::new(5),
				0,
				Some(crate::Hop::new(7).unwrap()),
				&mut announced,
			)
			.unwrap();

		moq_clock::sim::yield_now().await;
		let route = cursor.expect_next_active("room/host").await;
		assert_eq!(route.cost, crate::origin::Cost::new(5).charged(0));
	}

	/// A restart is held to the subscribe limit like a start: one the limit no longer
	/// covers is withheld, ending the old instance rather than attaching the new one.
	#[moq_clock::test]
	async fn a_restart_outside_the_limit_is_withheld() {
		let (mut subscriber, consumer) = restart_subscriber(SinkSession::new(Default::default()));
		let auth = subscriber.auth.clone();

		let mut announced = Announced::default();
		let path = Path::from_static("room/host");
		subscriber
			.start_announce(
				path.clone(),
				None,
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				0,
				Some(crate::Hop::new(7).unwrap()),
				&mut announced,
			)
			.unwrap();
		let mut cursor = consumer.announced();
		cursor.expect_next_active("room/host").await;

		auth.handle().authorize(&crate::auth::Grant {
			publish: Default::default(),
			subscribe: [crate::Pattern::subtree("other").unwrap()].into_iter().collect(),
			expires: None,
		});
		announced.restart(&path, None);
		let attached = subscriber
			.restart_announce(
				path.clone(),
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				0,
				Some(crate::Hop::new(7).unwrap()),
				&mut announced,
			)
			.unwrap();
		assert!(!attached, "a restart outside the limit was attached");
		cursor.expect_next_ended("room/host").await;
	}

	/// An announce stream that dies without an explicit `ended` retracts the route
	/// as promptly as an explicit retraction: a route into a dead session must not
	/// stay announced.
	///
	/// This falls out of `Announced` being a local whose announcements drop,
	/// which is exactly what makes it worth pinning: a refactor that hoisted the map
	/// to the session (outliving the stream) would leak the announcement instead.
	#[moq_clock::test]
	async fn a_lost_announce_stream_retracts_the_route() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_hop: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let hops = crate::Hops::try_from(vec![crate::Hop::new(7).unwrap()]).unwrap();
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				path.clone(),
				None,
				hops,
				crate::origin::Cost::default(),
				1,
				None,
				&mut announced,
			)
			.unwrap();
		let mut cursor = consumer.announced();
		cursor.expect_next_active("room/host").await;

		// The stream ends without retracting anything: the map dies with it and the
		// route retracts.
		drop(announced);
		cursor.expect_next_ended("room/host").await;

		// An explicit retraction retracts it the same way.
		let hops = crate::Hops::try_from(vec![crate::Hop::new(7).unwrap()]).unwrap();
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				path.clone(),
				None,
				hops,
				crate::origin::Cost::default(),
				1,
				None,
				&mut announced,
			)
			.unwrap();
		cursor.expect_next_active("room/host").await;
		assert!(announced.contains(&path.clone()), "the announce was not recorded");
		announced.withdraw(&path);
		cursor.expect_next_ended("room/host").await;
	}

	/// An unknown announce type between two starts is skipped without ending the
	/// stream.
	#[moq_clock::test]
	async fn an_unknown_announce_type_keeps_the_stream() {
		const VERSION: Version = Version::Lite06;
		let start = |suffix| lite::AnnounceBroadcast::Active {
			epoch: None,
			suffix: lite::PathRef::literal(Path::new(suffix).unwrap()),
			hops: lite::HopsRef::literal(crate::Hops::new()),
			cost: crate::origin::Cost::default(),
		};
		let mut script = Vec::new();
		lite::AnnounceOk {
			origin: crate::Hop::new(9).unwrap(),
			active: 2,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
		.unwrap();
		start("a")
			.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
			.unwrap();
		// An unknown announce type with an empty body, which decodes as `Skipped`.
		script.extend([0x3f, 0x00]);
		start("b")
			.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
			.unwrap();

		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(crate::lite::test_transport::ScriptedSession::new(script)),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: Some(1),
			peer_hop: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut prefix = AnnouncePrefix::new(
			subscriber.clone(),
			Path::from_static(""),
			subscriber.origin.snapshot(""),
		);
		let mut cursor = consumer.announced();

		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		cursor.expect_next_active("a").await;
		cursor.expect_next_active("b").await;
	}

	/// A peer's announce stream for the root, encoded for `version`.
	fn announce_script(version: Version, messages: &[&dyn Fn(&mut crate::coding::Encoder<'_>)]) -> Vec<u8> {
		let mut script = Vec::new();
		for message in messages {
			message(&mut crate::coding::Encoder::new(&mut script, version.into()));
		}
		script
	}

	/// An ANNOUNCE_OK counting `active` starts into the initial set.
	fn announce_ok(version: Version, active: u64) -> impl Fn(&mut crate::coding::Encoder<'_>) {
		move |w| {
			lite::AnnounceOk {
				origin: crate::Hop::new(9).unwrap(),
				active,
			}
			.encode(w, version)
			.unwrap()
		}
	}

	/// An ANNOUNCE_START for `suffix`.
	fn announce_start(version: Version, suffix: &'static str) -> impl Fn(&mut crate::coding::Encoder<'_>) {
		move |w| {
			lite::AnnounceBroadcast::Active {
				epoch: None,
				suffix: lite::PathRef::literal(Path::new(suffix).unwrap()),
				hops: lite::HopsRef::literal(crate::Hops::new()),
				cost: crate::origin::Cost::default(),
			}
			.encode(w, version)
			.unwrap()
		}
	}

	/// The announce stream for the root of a fresh origin, its peer replaying `session`.
	fn root_prefix(
		version: Version,
		session: crate::lite::test_transport::ScriptedSession,
	) -> (AnnouncePrefix, origin::Consumer) {
		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: Some(1),
			peer_hop: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		(
			AnnouncePrefix::new(
				subscriber.clone(),
				Path::from_static(""),
				subscriber.origin.snapshot(""),
			),
			consumer,
		)
	}

	/// A peer streaming announces gets a budget of them read per poll, so the session's
	/// other tasks run between them, and the announce behind the flood still arrives.
	#[moq_clock::test]
	async fn a_flood_of_announces_yields() {
		const VERSION: Version = Version::Lite06;
		/// `AnnouncePrefix`'s announces per poll.
		const BUDGET: usize = 32;
		const SKIPS: usize = 4096;
		let mut script = announce_script(VERSION, &[&announce_ok(VERSION, 0)]);
		for _ in 0..SKIPS {
			// An unknown announce type with an empty body: no change.
			script.extend([0x3f, 0x00]);
		}
		script.extend(announce_script(VERSION, &[&announce_start(VERSION, "a")]));
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let mut cursor = consumer.announced();

		let mut polls = 0;
		kio::wait(|waiter| {
			assert!(prefix.poll(waiter).is_pending(), "the announce stream ended");
			polls += 1;
			if cursor.try_next().is_some() {
				return Poll::Ready(());
			}
			assert!(polls <= SKIPS, "the announces stalled");
			Poll::Pending
		})
		.await;
		assert!(polls * BUDGET >= SKIPS, "{SKIPS} announces read in {polls} polls");
	}

	/// The origin's consumers sync once the counted starts are in and read, never before.
	#[moq_clock::test]
	async fn a_counted_initial_set_catches_up() {
		const VERSION: Version = Version::Lite06;
		let script = announce_script(
			VERSION,
			&[
				&announce_ok(VERSION, 2),
				&announce_start(VERSION, "a"),
				&announce_start(VERSION, "b"),
			],
		);
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let mut cursor = consumer.announced();
		assert!(!cursor.is_synced(), "held from the request on");

		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		cursor.assert_next_active("a");
		assert!(!cursor.is_synced(), "never before an earlier event is read");
		cursor.assert_next_active("b");
		assert!(cursor.is_synced());
		// A consumer created after the boundary syncs on its replay.
		let mut late = consumer.announced();
		late.assert_next_active("a");
		late.assert_next_active("b");
		assert!(late.is_synced());
	}

	/// Part of a counted set is not synced, however much of it was read.
	#[moq_clock::test]
	async fn a_partial_initial_set_holds() {
		const VERSION: Version = Version::Lite06;
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 2), &announce_start(VERSION, "a")]);
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let mut cursor = consumer.announced();
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		cursor.assert_next_active("a");
		assert!(!cursor.is_synced());
	}

	/// An empty lite-07 set waits for the ordered SYNCED event.
	#[moq_clock::test]
	async fn an_empty_initial_set_catches_up_at_once() {
		const VERSION: Version = Version::Lite07;
		let synced = |w: &mut crate::coding::Encoder<'_>| lite::AnnounceBroadcast::Synced.encode(w, VERSION).unwrap();
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 0), &synced]);
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let cursor = consumer.announced();
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert!(cursor.is_synced());
	}

	/// A complete body cannot be repaired by bytes from the next announcement.
	/// Keep the peer stream open: FIN must not be what discovers the malformed body.
	#[moq_clock::test]
	async fn malformed_complete_announce_closes_before_more_bytes() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut script = announce_script(version, &[&announce_ok(version, 1)]);
			script.extend_from_slice(match version {
				Version::Lite05 => &[1, 1],
				_ => &[0, 1, 5],
			});
			let session = crate::lite::test_transport::ScriptedSession::new(script);
			let log = session.log.clone();
			let (mut prefix, _consumer) = root_prefix(version, session);
			let result = prefix.poll(&kio::Waiter::noop());
			assert!(
				matches!(result, Poll::Ready(Err(Error::ProtocolViolation))),
				"{version:?}: {result:?}"
			);
			assert_eq!(log.closes()[0].0, crate::SessionError::ProtocolViolation.to_code());
		}
	}

	/// Only an incomplete outer frame waits, then resumes on its missing byte.
	#[moq_clock::test]
	async fn an_incomplete_announce_waits_for_its_body() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut script = announce_script(version, &[&announce_ok(version, 1), &announce_start(version, "room")]);
			let last = script.pop().unwrap();
			let session = crate::lite::test_transport::ScriptedSession::new(script);
			let log = session.log.clone();
			let (mut prefix, consumer) = root_prefix(version, session.clone());
			let mut cursor = consumer.announced();
			assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
			assert!(log.closes().is_empty());
			session.push(&[last]);
			assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
			cursor.assert_next_active("room");
			assert!(log.closes().is_empty());
		}
	}

	#[moq_clock::test]
	async fn a_duplicate_synced_is_refused() {
		const VERSION: Version = Version::Lite07;
		let synced = |w: &mut crate::coding::Encoder<'_>| lite::AnnounceBroadcast::Synced.encode(w, VERSION).unwrap();
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 0), &synced, &synced]);
		let (mut prefix, _consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation));
	}

	/// Anything but a start inside the counted set cuts it short: a protocol violation.
	#[moq_clock::test]
	async fn an_end_inside_the_initial_set_is_refused() {
		const VERSION: Version = Version::Lite06;
		let end =
			|w: &mut crate::coding::Encoder<'_>| lite::AnnounceBroadcast::EndedId { id: 0 }.encode(w, VERSION).unwrap();
		let script = announce_script(
			VERSION,
			&[&announce_ok(VERSION, 2), &announce_start(VERSION, "a"), &end],
		);
		let (mut prefix, _consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation), "{err}");
	}

	/// The stream ending inside the counted set cuts it short too.
	#[moq_clock::test]
	async fn a_fin_inside_the_initial_set_is_refused() {
		const VERSION: Version = Version::Lite06;
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 2), &announce_start(VERSION, "a")]);
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		session.close(crate::lite::test_transport::Close::Fin);
		let (mut prefix, _consumer) = root_prefix(VERSION, session);
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation), "{err}");
	}

	/// Lite03/04 mark no initial set: synced, best effort, once the request is sent.
	#[moq_clock::test]
	async fn an_unmarked_version_catches_up_on_the_request() {
		const VERSION: Version = Version::Lite04;
		let (mut prefix, consumer) =
			root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(Vec::new()));
		let cursor = consumer.announced();
		assert!(!cursor.is_synced());
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert!(cursor.is_synced());
	}

	/// Lite01/02 mark the initial set with ANNOUNCE_INIT.
	#[moq_clock::test]
	async fn announce_init_is_the_initial_set() {
		const VERSION: Version = Version::Lite02;
		let init = |w: &mut crate::coding::Encoder<'_>| {
			lite::AnnounceInit {
				suffixes: vec![Path::from_static("a")],
			}
			.encode(w, VERSION)
			.unwrap()
		};
		let (mut prefix, consumer) = root_prefix(
			VERSION,
			crate::lite::test_transport::ScriptedSession::new(announce_script(VERSION, &[&init])),
		);
		let mut cursor = consumer.announced();
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert!(!cursor.is_synced());
		cursor.assert_next_active("a");
		assert!(cursor.is_synced());
	}

	/// An announce past the session's cap fails the session with TOO_MANY_REQUESTS, and
	/// the stream gives its slots back once the session drops it.
	#[moq_clock::test]
	async fn announces_past_the_cap_close_the_session() {
		const VERSION: Version = Version::Lite06;
		let start = |suffix| lite::AnnounceBroadcast::Active {
			epoch: None,
			suffix: lite::PathRef::literal(Path::new(suffix).unwrap()),
			hops: lite::HopsRef::literal(crate::Hops::new()),
			cost: crate::origin::Cost::default(),
		};
		let mut script = Vec::new();
		lite::AnnounceOk {
			origin: crate::Hop::new(9).unwrap(),
			active: 3,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
		.unwrap();
		for suffix in ["a", "b", "c"] {
			start(suffix)
				.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
				.unwrap();
		}

		let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: Some(1),
			peer_hop: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let slots = crate::session::Slots::new(2);
		subscriber.announces = slots.clone();
		let mut prefix = AnnouncePrefix::new(
			subscriber.clone(),
			Path::from_static(""),
			subscriber.origin.snapshot(""),
		);

		let err = kio::wait(|waiter| prefix.poll(waiter))
			.await
			.expect_err("an announce past the cap must fail the session");
		assert_eq!(crate::SessionError::from(&err), crate::SessionError::TooManyRequests);

		// The session's end drops the stream, which gives its slots back.
		drop(prefix);
		let _a = slots.acquire().expect("the failed stream kept a slot");
		let _b = slots.acquire().expect("the failed stream kept a slot");
	}
	/// Distinct live GROUP starts are copies of the same group, including while
	/// readers retain every earlier tail. Exercise the real header/ingest path.
	#[moq_clock::test]
	async fn live_group_tails_share_content_held_by_earlier_readers() {
		use crate::transport::poll::Session as _;
		let version = Version::Lite07;
		let session = crate::lite::test_transport::ScriptedSession::eof(Vec::new());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session),
			origin: origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_hop: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry {
				producer: track.clone(),
				framing: lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
				},
				tail: Default::default(),
			},
		);
		let mut held = Vec::new();
		for start in (1..=8).rev() {
			let mut script = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
			lite::Group {
				subscribe: 7,
				sequence: 0,
				frame_start: start,
			}
			.encode(&mut encoder, version)
			.unwrap();
			for index in start..9 {
				encoder.varint(crate::coding::varint::zigzag(0)).unwrap();
				encoder.bytes(&vec![index as u8; 1024]).unwrap();
			}
			let incoming = crate::lite::test_transport::ScriptedSession::eof(script);
			let (_, recv) = incoming.open_bi(Default::default()).await.unwrap();
			let mut group = GroupRecv::new(
				subscriber.clone(),
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
			);
			kio::wait(|waiter| group.poll_serve(waiter)).await.unwrap();
			held.push((start, track.consume().peek_group(0).unwrap().with_range(start..)));
		}
		let mut pointers = std::collections::BTreeMap::new();
		for (start, reader) in &mut held {
			for index in *start..9 {
				assert_eq!(reader.index(), index);
				let frame = reader.read_frame().await.unwrap().unwrap();
				assert_eq!(frame.payload.as_ref(), vec![index as u8; 1024]);
				if let Some(prior) = pointers.insert(index, frame.payload.as_ptr() as usize) {
					assert_eq!(
						prior,
						frame.payload.as_ptr() as usize,
						"live GROUP frame {index} has one physical payload across held tails"
					);
				}
			}
			assert!(reader.read_frame().await.unwrap().is_none());
		}
		assert!(track.closed().now_or_never().is_none());
	}
}

/// The ranges a SUBSCRIBE carries for `subscription`, without the empty ones, with time
/// anchors in `timescale`, the track's as the publisher declared it.
///
/// A subscription always starts and ends at a group boundary, so every absolute end names
/// a group's first frame. `None` when nothing is left to ask for, which no wire can say.
/// The caller narrows a subscription for a wire without ranges to the one span it carries
/// first (see [`wire_span`]).
fn wire_ranges(subscription: &Subscription, timescale: Option<Timescale>) -> Result<Option<Vec<lite::Range>>, Error> {
	let time = |pts: crate::Timestamp| -> Result<lite::Anchor, Error> {
		let scale = timescale.ok_or(Error::TimestampMismatch)?;
		let pts = pts
			.convert(scale)
			.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		Ok(lite::Anchor::Time(pts.value()))
	};
	let mut ranges = Vec::with_capacity(subscription.ranges.len());
	for range in &subscription.ranges {
		// Nothing sorts below group 0, whatever the start.
		if let Some(Anchor::Absolute(end)) = range.end
			&& match range.start {
				Anchor::Absolute(start) => end <= start,
				_ => end == 0,
			} {
			continue;
		}
		let start = match range.start {
			// Past the varint's reach, a count back clamps to group 0 just as the oldest
			// start does, which only that start can spell.
			Anchor::Relative(back) if back > MAX_QUIC => None,
			Anchor::Relative(back) => Some(lite::Anchor::Relative(back)),
			Anchor::Absolute(group) => Some(lite::Anchor::Absolute(Position::group(group))),
			Anchor::Time(pts) => Some(time(pts)?),
		};
		// The copy resolves a relative or time end itself, against the groups it receives past
		// it, so it asks through the live edge. SUBSCRIBE_START names a resolved start, but
		// nothing names a resolved end, so one resolved upstream would leave the copy short.
		let end = match range.end {
			Some(Anchor::Absolute(group)) => Some(lite::Anchor::Absolute(Position::group(group))),
			None | Some(Anchor::Relative(_) | Anchor::Time(_)) => None,
		};
		ranges.push(lite::Range { start, end });
	}
	Ok((!ranges.is_empty()).then_some(ranges))
}

/// Whether every range has an end, so a lite-07 publisher ends the subscription once it
/// covers them.
fn bounded(ranges: &[lite::Range]) -> bool {
	ranges.iter().all(|range| range.end.is_some())
}

/// `subscription` narrowed to the one span a wire without ranges carries, which the
/// subscribers' own reads narrow again.
///
/// It starts at the lowest absolute start. A subscription counting only back from the
/// latest group asks with no start, which a pre-06 publisher reads as the latest group and
/// a lite-06 one as no floor. One mixing the two asks from group 0, since no single start
/// covers both: the latest group may sit below the absolute start.
fn wire_span(subscription: Subscription) -> Subscription {
	let start = match subscription.start() {
		Some(start) => Some(start),
		None if subscription
			.ranges
			.iter()
			.any(|range| matches!(range.start, Anchor::Absolute(_))) =>
		{
			Some(0)
		}
		None => None,
	};
	subscription.with_start(start)
}

/// The at-most-one live upstream subscription: its control stream plus the params
/// echoed in every SUBSCRIBE_UPDATE.
struct SubStream {
	stream: Stream<Transport, Version>,
	id: u64,
	/// Original SUBSCRIBE params, echoed in every SUBSCRIBE_UPDATE; refreshed as the
	/// downstream aggregate changes.
	max_delay: Duration,
	/// The lowest absolute start among the ranges, or `None` when one counts back from
	/// the latest group: the floor below which nothing is asked for.
	start: Option<u64>,
	/// The ranges on the wire, never empty.
	ranges: Vec<lite::Range>,
	priority: i8,
	order: crate::track::Order,
	/// The start the SUBSCRIBE itself carried, fixed for the stream's life. A
	/// SUBSCRIBE_START describes this demand and no fresh one follows an update,
	/// so a buffered START only applies while `start` still equals it: while the
	/// start sits elsewhere the request-tracked floor stands instead, and demand
	/// returning here makes the declaration valid again.
	requested: Option<u64>,
	/// The max delay the SUBSCRIBE itself carried, fixed for the stream's life: a lite-06+
	/// SUBSCRIBE_START resolves the start from it, skipping older groups as stale.
	requested_max_delay: Duration,
	/// The groups received for this subscription, shared with its [`TrackEntry`].
	tail: kio::Producer<Tail>,
	/// The first group the publisher serves (SUBSCRIBE_START), once declared.
	served: Option<u64>,
	/// Where SUBSCRIBE_START says each range's delivery starts, `None` for one with nothing
	/// to deliver. It matches `ranges` until an update changes them.
	starts: Vec<Option<u64>>,
	/// The track's exclusive end (SUBSCRIBE_END), once declared.
	end: Option<lite::SubscribeEnd>,
	/// The refresh join's wanted group, restated in every SUBSCRIBE_UPDATE.
	wanted: Option<u64>,
	/// Until the publisher's first response, what the source must still hold: a SUBSCRIBE
	/// without an epoch resolves whichever instance wins the path when it arrives, so one
	/// still unanswered once another instance takes the path may be answered by it.
	unanswered: Option<crate::model::broadcast::RequestValidity>,
	/// Whether this stream ever asked for bounded ranges alone, which a lite-07 publisher
	/// FINs without SUBSCRIBE_END once it covers them.
	bounded: bool,
	/// Whether an update changed the ranges since the SUBSCRIBE, which a publisher may
	/// FIN before reading.
	updated: bool,
}

impl SubStream {
	/// The groups the publisher still owes once it has ended the subscription.
	///
	/// `None` when nothing says which: drafts before SUBSCRIBE_END only have the FIN.
	/// Without a SUBSCRIBE_START the publisher served no group at all. Otherwise they start
	/// at the floor the demand last asked for, which an update can move either way, or
	/// where SUBSCRIBE_START resolved a live-edge one. The groups the SUBSCRIBE asked for
	/// below its SUBSCRIBE_START are accounted for as unavailable when it arrives.
	fn owed(&self, requested_end: Option<u64>) -> Option<std::ops::Range<u64>> {
		let end = self.end.as_ref()?.group;
		let end = requested_end.map_or(end, |requested| requested.min(end));
		let start = match self.served {
			Some(served) => self.start.unwrap_or(served),
			None => end,
		};
		Some(start..end)
	}

	/// The groups the publisher owed once it FINs a covered bounded lite-07 subscription
	/// without SUBSCRIBE_END: each range from its absolute start, or else from where
	/// SUBSCRIBE_START resolved delivery, up to its end. Groups below SUBSCRIBE_START were
	/// accounted for when it arrived.
	///
	/// Without a SUBSCRIBE_START the publisher served no group, which covers the ranges the
	/// SUBSCRIBE asked for, so it is empty unless an update changed them since. `None` when
	/// a range is open, or the ranges changed with nothing served: the publisher covered
	/// what it asked before an update it may not have read.
	fn covered(&self) -> Option<Vec<std::ops::Range<u64>>> {
		let Some(served) = self.served else {
			return (!self.updated).then(Vec::new);
		};
		self.ranges
			.iter()
			.enumerate()
			.map(|(index, range)| {
				// Our absolute ends name a group's first frame (see `wire_ranges`).
				let Some(lite::Anchor::Absolute(end)) = range.end else {
					return None;
				};
				let start = match self.starts.get(index) {
					// Each range's own start accounts for its skipped prefix, including
					// an absolute one when another range has no absolute requested floor.
					Some(start) if !self.updated => start.unwrap_or(end.group),
					_ => match range.start {
						Some(lite::Anchor::Absolute(start)) => start.group,
						_ => served,
					},
				};
				Some(start..end.group)
			})
			.collect()
	}
}

#[expect(
	clippy::large_enum_variant,
	reason = "one per track copy, which holds a live stream while anyone reads it, so boxing it would add an allocation per upstream subscription without saving memory"
)]
enum Sub {
	None,
	Active(SubStream),
}

/// Every advertisement the peer currently has live on one announce stream.
///
/// A declined advertisement remains present with no route because the peer still
/// owns its path and announce id until it retracts or restarts it.
struct Announced {
	routes: HashMap<Path, Held>,
	/// The session's announce cap, and one slot taken from it per entry in `routes`.
	slots: crate::session::Slots,
	held: Vec<crate::session::Slot>,
	/// The epoch each advertisement named, kept while it stands even when declined,
	/// so a restart that attaches it later serves the same broadcast.
	epochs: HashMap<Path, crate::Epoch>,
	/// Attached routes whose request queue woke since the last serve pass. The
	/// driver wakes for every group the session carries, so a pass must cost what
	/// was requested, not every route the peer announced.
	ready: kio::Queue<Path>,
}

/// What we made of one advertisement the peer holds.
enum Held {
	/// Not in the origin for good: a reflection, or outside our origin's scope.
	Declined,
	/// Outside the session's limit: the route as advertised, attached once a
	/// limit allows it.
	Withheld(crate::origin::Route),
	/// In the origin, serving requests.
	Attached(Box<AnnouncedRoute>),
}

#[cfg(test)]
impl Default for Announced {
	fn default() -> Self {
		Self::new(Default::default())
	}
}

impl Announced {
	fn new(slots: crate::session::Slots) -> Self {
		Self {
			routes: HashMap::new(),
			slots,
			held: Vec::new(),
			epochs: HashMap::new(),
			ready: Default::default(),
		}
	}

	fn contains(&self, path: &Path) -> bool {
		self.routes.contains_key(path)
	}

	/// Attach a route, queued for its first serve pass.
	fn attach(&mut self, path: Path, route: crate::origin::Route, dynamic: crate::origin::Requests) {
		let wake = crate::util::QueueWake::new(path.clone(), self.ready.clone());
		let route = AnnouncedRoute::new(route, dynamic, wake);
		route.waker.wake_by_ref();
		self.routes.insert(path, Held::Attached(Box::new(route)));
	}

	/// Put the peer's route into the origin, so paths under it resolve through this
	/// session on demand, or hold it back when the session's limit does not cover it.
	/// Returns whether it attached.
	fn offer(&mut self, subscriber: &Subscriber, path: Path, route: crate::origin::Route) -> bool {
		if !subscriber
			.auth
			.within_limit(crate::auth::Direction::Subscribe, path.as_str())
		{
			tracing::debug!(route = %subscriber.log_path(&path), "withholding announce outside the limit");
			self.routes.insert(path, Held::Withheld(route));
			return false;
		}
		// An error means the prefix is outside our origin's scope, so don't serve it.
		match subscriber.origin.requests_route(&path, route.clone()) {
			Ok(dynamic) => {
				self.attach(path, route, dynamic);
				true
			}
			Err(_) => {
				self.declined(path);
				false
			}
		}
	}

	fn declined(&mut self, path: Path) {
		// Dropping a replaced route closes its sources.
		self.routes.insert(path, Held::Declined);
	}

	/// Record an advertisement before deciding what to do with it.
	///
	/// The peer owns the prefix from the moment it announces, whatever the receiver makes
	/// of it, so the record is taken up front and every way out of the decision leaves it
	/// standing. Accepting replaces it via [`Self::attach`]. Doing it this way rather than
	/// at each rejection is what stops the next early return from silently freeing a path
	/// the peer still holds.
	/// Only valid on a prefix the peer does not already hold, which the caller establishes
	/// with [`Self::contains`]. Overwriting an attached route is [`Self::declined`]'s job.
	///
	/// Fails with [`Error::TooManyRequests`] once the session holds as many as it allows.
	fn reserve(&mut self, path: Path, epoch: Option<crate::Epoch>) -> Result<(), Error> {
		debug_assert!(!self.routes.contains_key(&path), "reserved a prefix already advertised");
		self.held.push(self.slots.acquire()?);
		if let Some(epoch) = epoch {
			self.epochs.insert(path.clone(), epoch);
		}
		self.routes.insert(path, Held::Declined);
		Ok(())
	}

	/// The epoch the advertisement at `path` named, if any.
	fn epoch(&self, path: &Path) -> Option<crate::Epoch> {
		self.epochs.get(path).cloned()
	}

	/// Record the epoch a restart of the live advertisement at `path` names.
	fn restart(&mut self, path: &Path, epoch: Option<crate::Epoch>) {
		match epoch {
			Some(epoch) => self.epochs.insert(path.clone(), epoch),
			None => self.epochs.remove(path),
		};
	}

	fn attached(&mut self, path: &Path) -> Option<&mut AnnouncedRoute> {
		match self.routes.get_mut(path)? {
			Held::Attached(route) => Some(route.as_mut()),
			_ => None,
		}
	}

	/// Retire this session's advertisement without invalidating another live
	/// session from the same peer. Dropping its sources closes their requests.
	fn withdraw(&mut self, path: &Path) {
		self.epochs.remove(path);
		let Some(held) = self.routes.remove(path) else {
			return;
		};
		self.held.pop();
		if let Held::Attached(entry) = held {
			entry.dynamic.withdrawn();
		}
	}

	/// Serve queued requests on every ready route: mint a source per requested
	/// path, answer the requester with its consumer, and hand the source to the
	/// driver's serve machines, which own it from then on.
	fn poll_serve(&mut self, subscriber: &Subscriber, waiter: &kio::Waiter) {
		let root = subscriber.origin.root();
		while let Poll::Ready(Ok(path)) = self.ready.poll_pop(waiter) {
			// A route retired since it woke has nothing left to serve.
			let Some(Held::Attached(entry)) = self.routes.get_mut(&path) else {
				continue;
			};
			// Cleared before polling, so a request landing mid-pass queues the route again.
			entry.wake.popped();
			let cx = std::task::Context::from_waker(&entry.waker);
			let route_waiter = entry.park.hold(&cx);
			while let Poll::Ready(Ok(request)) = entry.dynamic.poll_requested_broadcast(route_waiter) {
				// The request path is absolute; the wire (and our origin handle)
				// speak paths relative to the session's root.
				let Some(path) = request.path().strip_prefix(root) else {
					// Outside our root: nothing we could name on the wire.
					continue;
				};
				let path = path.clone();
				// A request naming the epoch reaches only that instance, so only a source
				// without one must stop once the peer no longer selects what minted it.
				let validity = if entry.route.epoch.is_some() {
					Default::default()
				} else {
					crate::model::broadcast::RequestValidity::for_source(&entry.dynamic, request.path())
				};
				let source = subscriber.origin.create_source(path.clone(), validity);
				// The handler exists before the requester sees the source, so a track it
				// asks for before the serve machine first runs queues instead of failing
				// `NotFound`. Accepted before the push, so the requester holds the source
				// before its serve machine can see it unheld.
				let dynamic = source.requests();
				request.accept(&source);
				let _ = subscriber.sources.try_push(MintedSource {
					path,
					epoch: entry.route.epoch.clone(),
					source: crate::model::broadcast::SourceGuard::new(source),
					dynamic,
					route: entry.live.consume(),
				});
			}
		}
	}

	/// Apply a new limit: hold back every attached route it no longer covers, closing
	/// its sources (tracks in flight end on their own gates, with `Unauthorized`), and
	/// attach every withheld one it now does.
	fn limit(&mut self, permit: &crate::auth::Permit, subscriber: &Subscriber) {
		let changed: Vec<Path> = self
			.routes
			.iter()
			.filter(|(path, held)| match held {
				Held::Attached(_) => !permit.within_limit(path.as_str()),
				Held::Withheld(_) => permit.within_limit(path.as_str()),
				Held::Declined => false,
			})
			.map(|(path, _)| path.clone())
			.collect();
		for path in changed {
			match self.routes.remove(&path) {
				Some(Held::Attached(entry)) => {
					tracing::info!(route = %subscriber.log_path(&path), "announce no longer authorized");
					self.routes.insert(path, Held::Withheld(entry.route.clone()));
				}
				Some(Held::Withheld(mut route)) => {
					tracing::info!(route = %subscriber.log_path(&path), "announce authorized again");
					if subscriber.going_away.is_set() {
						route.cost = crate::origin::Cost::DRAIN;
					}
					self.offer(subscriber, path, route);
				}
				_ => unreachable!("only attached and withheld routes change"),
			}
		}
	}

	/// Re-price every attached route to a draining cost (the peer sent a GOAWAY).
	fn drain(&mut self) {
		for held in self.routes.values_mut() {
			if let Held::Attached(entry) = held {
				entry.drain();
			}
		}
	}
}

/// One received announce: the served route announced into the origin (the
/// advertisement plus its request queue), which mints a source per requested path
/// beneath it.
struct AnnouncedRoute {
	/// The route as last announced (post-charge), so a drain can re-price it
	/// without recomputing the chain.
	route: crate::origin::Route,
	/// Dropping it retracts the route and rejects its queued requests.
	dynamic: crate::origin::Requests,
	/// Closes as the route goes, which closes every source it minted.
	live: kio::Producer<()>,
	/// Whether the GOAWAY drain already re-priced this route.
	drained: bool,
	/// Queues this route on its prefix's ready set when its request queue wakes.
	wake: Arc<crate::util::QueueWake<Path>>,
	waker: std::task::Waker,
	/// Keeps this route's registration on its request queue alive between passes.
	park: kio::Park,
}

impl AnnouncedRoute {
	fn new(
		route: crate::origin::Route,
		dynamic: crate::origin::Requests,
		wake: Arc<crate::util::QueueWake<Path>>,
	) -> Self {
		Self {
			route,
			dynamic,
			live: Default::default(),
			drained: false,
			waker: std::task::Waker::from(wake.clone()),
			wake,
			park: kio::Park::default(),
		}
	}

	/// Update the route metadata without changing its content identity.
	fn update(&mut self, route: crate::origin::Route) {
		self.route = route.clone();
		self.drained = false;
		let _ = self.dynamic.update_route(route);
	}

	/// Re-price the route to [`crate::origin::Cost::DRAIN`] (the peer sent a
	/// GOAWAY): every other candidate outranks it while it stays selectable as
	/// the last path. Idempotent, since the signal stays set.
	fn drain(&mut self) {
		if self.drained {
			return;
		}
		self.drained = true;
		let mut route = self.route.clone();
		route.cost = crate::origin::Cost::DRAIN;
		let _ = self.dynamic.update_route(route);
	}
}

/// How a [`TrackServe`] run ends.
enum ServeEnd {
	/// The upstream FIN'd: the track is over for good.
	Finished,
	/// The route or session failed: abort the track so the origin serves it from
	/// another source.
	GiveBack(Error),
	/// No consumers or in-flight fetches remain; the owner must commit the idle abort.
	Idle,
}

/// Serves one requested track for a relay: owns this session's copy of the
/// track (pumped into the origin's logical track), driving the single upstream
/// subscription (opened lazily on the first downstream subscriber, canceled when
/// the last one leaves) concurrently with any number of one-shot fetches.
#[derive(Clone)]
struct TrackServe {
	validity: crate::model::broadcast::RequestValidity,
	/// The advertisement that minted this source, absent for standalone test sources.
	route: Option<kio::Consumer<()>>,
	subscriber: Subscriber,
	path: Path,
	/// The publisher instance to ask for; the peer refuses a request for another.
	epoch: Option<crate::Epoch>,
	name: String,
}

impl TrackServe {
	/// Watches whether the session still lets us receive this broadcast from the peer:
	/// our grant, and the ceiling on what the peer may publish.
	fn gate(&self) -> crate::auth::Gate {
		crate::auth::Gate::new(
			self.subscriber.auth.clone(),
			self.path.clone(),
			crate::auth::Direction::Subscribe,
		)
	}

	/// Apply a subscription-demand change: hand back an [`Establish`] to open the
	/// upstream SUBSCRIBE on the first subscriber, buffer a SUBSCRIBE_UPDATE while
	/// live (the caller flushes), or cancel outright when the last one leaves.
	fn begin_subscription(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub,
		pref: Option<Subscription>,
		supports_update: bool,
	) -> Result<Begin, Error> {
		// An empty half-open range asks for nothing, and the wire cannot say that: its
		// bounds are inclusive, so the nearest encoding either hands back the position
		// the caller excluded or inverts the range outright once the two bounds meet. No
		// demand at all is the faithful translation. An absent start is the live edge, wherever that lands, so the only end that is
		// certainly empty is the very first position, which is what `Position::default()`
		// stands in for.
		let pref = pref.filter(|sub| sub.end().is_none_or(|end| end > sub.start().unwrap_or_default()));
		let ranged = self.subscriber.version.has_ranges();
		let request = match pref {
			Some(subscription) => {
				// Lite-07 carries every range asked for; an older wire one span covering them.
				let mut subscription = match ranged {
					true => subscription,
					false => wire_span(subscription),
				};
				// An idle copy is asked from the head of the newest group it cached when
				// the subscriber has no start of its own: a quiet track (a catalog) sends
				// that group again, which says the cache is current. A lite-07 list keeps
				// any range of its own, so only one asking just for the live or oldest group
				// counts.
				let idle = subscription.start().is_none()
					&& (!ranged
						|| subscription
							.ranges
							.iter()
							.all(|range| range.end.is_none() && matches!(range.start, Anchor::Relative(0 | u64::MAX))));
				if matches!(sub, Sub::None) && idle {
					subscription = subscription.with_start(producer.idle_newest());
				}
				wire_ranges(&subscription, producer.info().timescale)?.map(|ranges| (subscription, ranges))
			}
			None => None,
		};

		match request {
			Some((subscription, ranges)) => {
				match sub {
					// Open an upstream SUBSCRIBE for the first subscriber.
					Sub::None => Ok(Begin::Establish(self.prepare_establish(producer, subscription, ranges))),
					Sub::Active(active) => {
						// Downstream preferences changed: forward them upstream as a
						// SUBSCRIBE_UPDATE (Lite03+ only; older peers can't carry one).
						let start_moved = active.start != subscription.start();
						active.priority = subscription.priority;
						active.order = subscription.order;
						active.max_delay = wire_max_delay(subscription.max_delay);
						// A lowered floor owes groups nobody asked for until now, in the session's
						// tail as well as in the track's arrivals, which its start declaration
						// restarts.
						let lowered = subscription.start().map(|start| {
							let floor = active.start.or(active.served);
							start..floor.unwrap_or(u64::MAX)
						});
						if let Ok(mut tail) = active.tail.write() {
							tail.set_grace(tail::grace(subscription.budget()));
							if let Some(groups) = lowered {
								tail.demand(groups, self.subscriber.runtime.now());
							}
						}
						active.start = subscription.start();
						active.updated |= active.ranges != ranges;
						active.ranges = ranges;
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
								let _ = producer.start_at(active.start);
							}
							buffer_update(active)?;
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
					self.subscriber.remove_subscribe(active.id);
					let _ = active.stream.writer.finish();
					tracing::debug!(track = %self.name, "subscribe canceled (idle)");
					*sub = Sub::None;
					// The copy is still held: what it cached goes stale from here.
					producer.set_idle();
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
	///
	/// The subscription's bounds come straight from the demand aggregate.
	fn prepare_establish(
		&self,
		producer: &mut track::Producer,
		subscription: Subscription,
		ranges: Vec<lite::Range>,
	) -> Establish {
		let id = self.subscriber.next_id.fetch_add(1, atomic::Ordering::Relaxed);

		// The floor tracks the requested start until this subscription's own
		// SUBSCRIBE_START refines it: the peer never serves below the request, a
		// previous subscription's declaration must not outlive its demand, and
		// live-edge demand (None) starts with no floor at all. Lite-06+ only resolves
		// the start with its SUBSCRIBE_START, so until then the floor is just a request.
		let floor = subscription.start();
		let _ = match self.subscriber.version.resolves_start() {
			true => producer.request_start(floor),
			false => producer.start_at(floor),
		};

		tracing::debug!(id, broadcast = %self.subscriber.log_path(&self.path), track = %self.name, "subscribe started");

		let tail = kio::Producer::new(Tail::new(tail::grace(subscription.budget())));
		self.subscriber.subscribes.lock().insert(
			id,
			TrackEntry {
				producer: producer.clone(),
				framing: lite::Framing::new(producer.info(), self.subscriber.version),
				tail: tail.clone(),
			},
		);

		let session = self.subscriber.session.clone();
		Establish {
			validity: self.validity.start(),
			serve: self.clone(),
			closed: session.clone(),
			session,
			id,
			subscription,
			ranges,
			tail,
			state: EstablishState::Start,
		}
	}

	/// Test shim: drive the upstream SUBSCRIBE open like the old `establish`.
	#[cfg(test)]
	async fn establish(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub,
		subscription: Subscription,
	) -> Result<(), Error> {
		let subscription = match self.subscriber.version.has_ranges() {
			true => subscription,
			false => wire_span(subscription),
		};
		let ranges = wire_ranges(&subscription, producer.info().timescale)?.expect("a non-empty subscription");
		let mut est = Box::new(self.prepare_establish(producer, subscription, ranges));
		let id = est.id;
		match kio::wait(move |waiter| est.poll(waiter)).await {
			Ok(active) => {
				*sub = Sub::Active(active);
				Ok(())
			}
			Err(err) => {
				self.subscriber.remove_subscribe(id);
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
		sub: &mut Sub,
		pref: Option<Subscription>,
		supports_update: bool,
	) -> Result<(), Error> {
		match self.begin_subscription(producer, sub, pref, supports_update)? {
			Begin::Establish(est) => {
				let mut est = Box::new(est);
				let id = est.id;
				match kio::wait(move |waiter| est.poll(waiter)).await {
					Ok(active) => *sub = Sub::Active(active),
					Err(err) => {
						self.subscriber.remove_subscribe(id);
						return Err(err);
					}
				}
			}
			Begin::None => {
				if let Sub::Active(active) = sub {
					kio::wait(|waiter| active.stream.writer.poll_flush(waiter)).await?;
				}
			}
		}
		Ok(())
	}
}

/// What a demand change asks the serve loop to do next.
#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum Begin {
	/// Nothing further: the update (if any) sits in the active stream's write
	/// buffer, flushed by the loop.
	None,
	/// Open an upstream SUBSCRIBE for the first subscriber.
	Establish(Establish),
}

/// The `Subscriber Max Age` a SUBSCRIBE carries for `max_delay`: `None` is the publisher's
/// max age alone, which the largest value every peer can read leaves to its clamp.
fn wire_max_delay(max_delay: Option<std::time::Duration>) -> std::time::Duration {
	max_delay.unwrap_or(std::time::Duration::from_millis(super::MAX_SAFE_DELAY_MS))
}

/// Buffer the current SUBSCRIBE parameters. The caller flushes.
fn buffer_update(active: &mut SubStream) -> Result<(), Error> {
	active.bounded |= bounded(&active.ranges);
	active.stream.writer.buffer(&lite::SubscribeUpdate {
		wanted_group: active.wanted,
		priority: active.priority,
		max_delay: active.max_delay,
		ranges: active.ranges.clone(),
		order: active.order,
	})
}

/// Opens the upstream SUBSCRIBE control stream: send the request, then (pre
/// lite-05) wait for the SUBSCRIBE_OK. Resolves with the live [`SubStream`];
/// the caller deregisters the id on failure.
struct Establish {
	validity: crate::model::broadcast::RequestValidity,
	serve: TrackServe,
	session: Transport,
	// A dedicated close-watch handle for the SUBSCRIBE_OK wait.
	closed: Transport,
	id: u64,
	subscription: Subscription,
	/// The ranges the SUBSCRIBE carries for `subscription`.
	ranges: Vec<lite::Range>,
	tail: kio::Producer<Tail>,
	state: EstablishState,
}

#[kio::machine(self.state: EstablishState)]
impl Establish {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<SubStream, Error>> {
		#[every_poll]
		self.validity.check(waiter)?;

		let mut stream: Stream<Transport, Version> = wait!(Open => Stream::poll_open_request(
			&self.session,
			self.serve.subscriber.version,
			waiter
		))?;
		let msg = lite::Subscribe {
			epoch: self.serve.epoch.clone(),
			id: self.id,
			broadcast: self.serve.path.clone(),
			track: self.serve.name.as_str().into(),
			priority: self.subscription.priority,
			max_delay: wire_max_delay(self.subscription.max_delay),
			ranges: self.ranges.clone(),
			order: self.subscription.order,
		};
		stream = stream.with_stats(self.serve.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Subscribe)?;
		stream.writer.buffer(&msg)?;
		wait!(Send => stream.writer.poll_flush(waiter))?;
		if self.serve.subscriber.version.has_track_stream() {
			return Poll::Ready(Ok(self.activate(stream, false)));
		}

		// Older drafts: the first SUBSCRIBE_OK confirms it. Bail if the session dies
		// meanwhile; a dying route hands the assignment back through the serve loop's
		// teardown instead.
		let resp = wait!(WaitOk => {
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				return Poll::Ready(Err(crate::lite::error::from_transport(err)));
			}
			stream.reader.poll_decode::<lite::SubscribeResponse>(waiter)
		})?;
		if !matches!(resp, lite::SubscribeResponse::Ok(_)) {
			return Poll::Ready(Err(Error::ProtocolViolation));
		}
		Poll::Ready(Ok(self.activate(stream, true)))
	}

	/// Give up on the SUBSCRIBE, resetting it with `err` if it reached the wire.
	fn abort(self, err: &Error) {
		self.serve.subscriber.remove_subscribe(self.id);
		if let EstablishState::Send(EstablishStateSend { stream })
		| EstablishState::WaitOk(EstablishStateWaitOk { stream }) = self.state
		{
			stream.writer.abort(err);
		}
	}

	/// The live subscription, `answered` once the publisher's first response arrived.
	fn activate(&self, stream: Stream<Transport, Version>, answered: bool) -> SubStream {
		SubStream {
			unanswered: (!answered).then(|| self.validity.clone()),
			stream,
			id: self.id,
			max_delay: wire_max_delay(self.subscription.max_delay),
			requested_max_delay: wire_max_delay(self.subscription.max_delay),
			start: self.subscription.start(),
			ranges: self.ranges.clone(),
			priority: self.subscription.priority,
			order: self.subscription.order,
			requested: self.subscription.start(),
			tail: self.tail.clone(),
			served: None,
			starts: Vec::new(),
			end: None,
			wanted: None,
			bounded: bounded(&self.ranges),
			updated: false,
		}
	}
}

/// Drives one [`TrackServe`]: the TRACK_INFO fetch, then the serve loop, then
/// the teardown that decides how the origin sees this copy end.
struct TrackServeRun {
	serve: TrackServe,
	state: TrackRunState,
	/// Cancels the track once the session stops allowing its broadcast.
	gate: crate::auth::Gate,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum TrackRunState {
	/// Lite05+ learns the track's immutable properties once, up front, via a
	/// TRACK stream. The timescale then flows into every SUBSCRIBE and FETCH
	/// without a per-response header.
	Info {
		request: Option<track::Request>,
		info: TrackInfoFetch,
	},
	Serve(ServeLoop),
	/// Preserve the lite07 completion FIN until the publisher acknowledges it.
	Finish(crate::coding::Writer<crate::transport::boxed::SendStream, Version>),
	Done,
}

impl TrackServeRun {
	fn new(serve: TrackServe, request: track::Request) -> Self {
		let state = if serve.subscriber.version.has_track_stream() {
			TrackRunState::Info {
				request: Some(request),
				info: TrackInfoFetch::new(&serve),
			}
		} else {
			// Older wires declare no publisher retention limit, and no timeline: their
			// frames arrive untimed.
			let info = track::Info::default().with_timescale(None);
			TrackRunState::Serve(ServeLoop::new(&serve, request, info))
		};
		let gate = serve.gate();
		Self { serve, state, gate }
	}

	/// The session no longer allows the broadcast: end the track and cancel the
	/// upstream subscription and fetches, leaving the rest of the session alone.
	fn revoke(&mut self) {
		tracing::info!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, "subscription no longer authorized");
		match std::mem::replace(&mut self.state, TrackRunState::Done) {
			TrackRunState::Info { request, .. } => {
				if let Some(request) = request {
					request.reject(Error::Unauthorized);
				}
			}
			TrackRunState::Serve(serve_loop) => {
				let _ = serve_loop.serving.abort(Error::Unauthorized);
				// Reset rather than finish, so the publisher reads a revocation instead of
				// a routine unsubscribe. A SUBSCRIBE still opening is on the wire too.
				if let Sub::Active(active) = serve_loop.sub {
					self.serve.subscriber.remove_subscribe(active.id);
					active.stream.writer.abort(&Error::Unauthorized);
				}
				if let ServeMode::Establish(establish) = serve_loop.mode {
					establish.abort(&Error::Unauthorized);
				}
				// Each fetch in flight resets itself as it drops, seeing the same denial.
			}
			TrackRunState::Finish(_) | TrackRunState::Done => {}
		}
	}
}

impl TrackServeRun {
	/// Nobody wants the track anymore (the origin failed over, the reader left): stop
	/// waiting on a token or a peer that may never answer, which would otherwise hold
	/// this task, its TRACK stream, and the track for good. Dropping the fetch resets the
	/// stream. Ready once the request is released.
	fn poll_unused(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let TrackRunState::Info { request, .. } = &self.state else {
				return Poll::Pending;
			};
			let pending = request.as_ref().expect("request pending");
			ready!(crate::util::poll_unwanted(&mut pending.demand(), waiter));
			if pending.reject_unused(Error::Cancel) {
				self.state = TrackRunState::Done;
				return Poll::Ready(());
			}
			// Demand returned in the gap and won inside `reject_unused`: poll again so the
			// unused wait is armed for when it leaves.
		}
	}
}

impl kio::Task for TrackServeRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Checked before the auth wait too, so an abandoned request never parks on a token.
		if self.poll_unused(waiter).is_ready() {
			return Poll::Ready(());
		}
		ready!(self.gate.poll_ready(waiter));
		// A finished track only waits for its FIN to be acknowledged: nothing is left to revoke.
		// Nor on a closed session, whose grants end with it: the session's end decides how
		// its tracks end.
		if !matches!(self.state, TrackRunState::Finish(_))
			&& self.gate.poll_denied(waiter).is_ready()
			&& self.serve.subscriber.session.clone().poll_closed(waiter).is_pending()
		{
			self.revoke();
			return Poll::Ready(());
		}
		loop {
			match &mut self.state {
				TrackRunState::Info { request, info } => {
					// Abandonment was checked at the top of this poll, arming its wait.
					let res = ready!(info.poll_fetch(&self.serve, waiter));
					let request = request.take().expect("request pending");
					match res {
						Ok((mut info, held)) => {
							// Lite05/06 timestamps may be send times, so the copy is untimed
							// and the ingest reads past them (see `lite::Framing::new`).
							if !self.serve.subscriber.version.has_media_time() {
								info.timescale = None;
							}
							let mut serve_loop = ServeLoop::new(&self.serve, request, info);
							serve_loop.held = Some(held);
							self.state = TrackRunState::Serve(serve_loop);
						}
						Err(err) => {
							let err = lite::error::control(&mut self.serve.subscriber.session.clone(), err);
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
					let finished = matches!(teardown, ServeEnd::Finished);
					let TrackRunState::Serve(mut serve_loop) = std::mem::replace(&mut self.state, TrackRunState::Done)
					else {
						unreachable!()
					};

					match teardown {
						ServeEnd::Idle => match serve_loop.serving.abort_unused(Error::Cancel) {
							Ok(()) => {
								tracing::debug!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, "track released (idle)");
							}
							Err(used) => {
								serve_loop.serving = used;
								self.state = TrackRunState::Serve(serve_loop);
								continue;
							}
						},
						ServeEnd::Finished => {
							let _ = serve_loop.serving.finish();
						}
						ServeEnd::GiveBack(err) => {
							let err = lite::error::control(&mut self.serve.subscriber.session.clone(), err);
							let _ = serve_loop.serving.abort(err);
						}
					}

					if let Sub::Active(mut active) = serve_loop.sub {
						self.serve.subscriber.remove_subscribe(active.id);
						if active.stream.writer.finish().is_ok()
							&& finished && self.serve.subscriber.version.waits_for_subscriber_fin()
						{
							self.state = TrackRunState::Finish(active.stream.writer);
							continue;
						}
					}

					return Poll::Ready(());
				}
				TrackRunState::Finish(writer) => {
					let _ = ready!(writer.poll_close(waiter));
					self.state = TrackRunState::Done;
				}
				TrackRunState::Done => return Poll::Ready(()),
			}
		}
	}
}

impl Drop for TrackServeRun {
	fn drop(&mut self) {
		// Still waiting on TRACK_INFO: the request never reached the subscribe map,
		// so the driver's abort cannot see it. Reject with the session's error.
		let TrackRunState::Info { request, .. } = &mut self.state else {
			return;
		};
		let Some(request) = request.take() else {
			return;
		};
		request.reject(self.serve.subscriber.end_reason());
	}
}

/// Opens a TRACK stream, reads the single TRACK_INFO, and maps it to the
/// model's [`track::Info`], handing back the stream to hold. Lite05+ only. Bails if the
/// session dies meanwhile.
struct TrackInfoFetch {
	validity: crate::model::broadcast::RequestValidity,
	session: Transport,
	// A dedicated close-watch handle for the read.
	closed: Transport,
	state: TrackInfoState,
}

#[kio::machine(self.state: TrackInfoState)]
impl TrackInfoFetch {
	fn new(serve: &TrackServe) -> Self {
		let session = serve.subscriber.session.clone();
		Self {
			validity: serve.validity.start(),
			closed: session.clone(),
			session,
			state: TrackInfoState::Start,
		}
	}

	fn poll_fetch(
		&mut self,
		serve: &TrackServe,
		waiter: &kio::Waiter,
	) -> Poll<Result<(track::Info, HeldTrack), Error>> {
		#[every_poll]
		self.validity.check(waiter)?;
		#[every_poll]
		if serve
			.route
			.as_ref()
			.is_some_and(|route| route.poll_closed(waiter).is_ready())
		{
			return Poll::Ready(Err(Error::Unroutable));
		}

		let mut stream: Stream<Transport, Version> =
			wait!(Open => Stream::poll_open_request(&self.session, serve.subscriber.version, waiter))?
				.with_stats(serve.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Track)?;
		stream.writer.buffer(&lite::Track {
			epoch: serve.epoch.clone(),
			broadcast: serve.path.clone(),
			track: serve.name.as_str().into(),
		})?;
		wait!(Send => stream.writer.poll_flush(waiter))?;

		let (ietf, info) = wait!(Read => {
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				return Poll::Ready(Err(crate::lite::error::from_transport(err)));
			}
			let ietf = ready!(serve.subscriber.peer_setup.poll_ietf(waiter));
			stream.reader.poll_decode::<lite::TrackInfo>(waiter).map_ok(|info| (ietf, info))
		})?;

		// Publisher Max Age rides on the wire, so the local retention
		// window matches what the upstream advertises (relays re-serve with
		// the same bound). `broadcast` is left at its default here;
		// `track::Request::accept` stamps the track's real broadcast.
		let mut info: track::Info = track::Info::from(info);
		if !ietf {
			return Poll::Ready(Ok((info, HeldTrack(stream))));
		}

		// Reading the TRACK_PROPERTIES the `ietf` extension sends after TRACK_INFO.
		let properties = wait!(Properties => {
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				return Poll::Ready(Err(crate::lite::error::from_transport(err)));
			}
			stream.reader.poll_decode::<lite::ietf::TrackProperties>(waiter)
		})?;
		// Refused as moq-transport ingress refuses it.
		if let Some(err) = properties.0.as_ref().and_then(|properties| properties.refused(&info)) {
			return Poll::Ready(Err(err));
		}
		info.properties = properties.0;
		Poll::Ready(Ok((info, HeldTrack(stream))))
	}
}

/// An answered TRACK stream, kept open as interest in the track: the publisher holds the
/// track for us until we FIN it, which dropping this does. Held until the SUBSCRIBE that
/// follows has its first response, or until nobody wants this copy, so demand never
/// lapses between the two streams at any hop.
struct HeldTrack(Stream<Transport, Version>);

impl Drop for HeldTrack {
	fn drop(&mut self) {
		let _ = self.0.writer.finish();
	}
}

/// The serve loop proper: owns this session's copy of the track (pumped into
/// the origin's logical track), driving the single upstream subscription
/// (opened lazily on the first downstream subscriber, canceled when the last
/// one leaves) concurrently with any number of one-shot fetches.
struct ServeLoop {
	/// This session's copy, accepted with the resolved info. The origin pumps
	/// it into the logical track; demand from the logical subscribers arrives
	/// through the producer's aggregate (including the resume floor after a source
	/// change).
	serving: track::Producer,
	/// Follows `serving`'s largest, to announce a wanted group once one lands below it.
	edge: track::Consumer,
	largest: Option<u64>,
	/// Watches `serving`'s subscribers, to release the copy once nobody reads it.
	demand: track::Demand,
	/// Serve on-demand fetches of uncached groups from this session.
	dynamic: track::Requests,
	sub: Sub,
	fetches: kio::Tasks<FillServeRun>,
	/// Fetches a pre-lite-05 upstream has no FETCH for, held for the live feed.
	feed_fetches: FeedFetches,
	// A dedicated close-watch handle for the session-died arm.
	closed: Transport,
	// SUBSCRIBE_UPDATE only exists on Lite03+, so older peers can't carry a
	// preference change to an established subscription.
	supports_update: bool,
	supports_fetch: bool,
	mode: ServeMode,
	/// Armed while nobody holds the copy: it stays, cache and all, for a returning
	/// reader until this fires.
	linger: crate::time::Deadline,
	/// The TRACK stream that answered, until a SUBSCRIBE has a response or nobody holds
	/// the copy.
	held: Option<HeldTrack>,
	/// The ranges of a bounded lite-07 subscription the publisher covered, while the
	/// demand still asks for just them: subscribing again would only resend them.
	covered: Option<Vec<lite::Range>>,
	/// Our side of the covered subscription, finished and held until the publisher reads
	/// the FIN, since dropping it would reset it.
	closing: Option<crate::coding::Writer<crate::transport::boxed::SendStream, Version>>,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum ServeMode {
	/// Selecting the next event.
	Select,
	/// Driving an upstream SUBSCRIBE open. The demand arms wait meanwhile,
	/// exactly like the old inline await.
	Establish(Establish),
	/// The upstream FIN'd, but QUIC does not order streams: keep the subscription
	/// routable until every owed group is accounted for. The grace bounds an unaccounted
	/// sequence, such as a lost datagram.
	Tail {
		settle: Settle,
		/// The groups owed, `None` when nothing says which, so only the grace settles.
		owed: Option<Vec<std::ops::Range<u64>>>,
		/// Whether the FIN ended the track, rather than a covered bounded lite-07
		/// subscription, after which the copy stays for its readers.
		track: bool,
	},
}

/// Fills a pre-lite-05 upstream has no FETCH for, held while the live subscription may
/// still deliver their groups. A group the feed delivers lands in the cache, where the fill
/// answers it.
///
/// A fill waits on one group at a time, the next it still owes. Each verdict on that group
/// also settles, by run, every group of the fill the feed gave up on, so a fill a peer
/// made wide costs what the cache and ledger hold rather than its width.
struct FeedFetches {
	/// Each fill, by the group it waits on.
	held: BTreeMap<u64, HeldFetch>,
	ready: kio::Shared<BTreeSet<u64>>,
	arrivals: Option<track::Arrivals>,
	/// Only gaps containing a held group arm the earliest fold.
	fold: crate::time::Deadline,
	/// An additional track backstop, above one peer's default FETCH allowance.
	max: usize,
	#[cfg(test)]
	polled: usize,
}

struct HeldFetch {
	fill: track::Fill,
	waiter: kio::Waiter,
	/// Not judged yet: what the feed already gave up on is settled at once.
	fresh: bool,
}

struct FetchWake {
	sequence: u64,
	ready: kio::Shared<BTreeSet<u64>>,
}

impl std::task::Wake for FetchWake {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}
	fn wake_by_ref(self: &Arc<Self>) {
		self.ready.lock().insert(self.sequence);
	}
}

impl FeedFetches {
	fn new(clock: &crate::time::Deadlines) -> Self {
		Self {
			held: Default::default(),
			ready: Default::default(),
			arrivals: None,
			fold: crate::time::Deadline::new(clock),
			max: crate::session::Limits::default().announces,
			#[cfg(test)]
			polled: 0,
		}
	}

	/// Hold `fill` for the live feed.
	fn push(&mut self, fill: track::Fill, feed: &track::Consumer) {
		let Some(arrivals) = feed.arrivals() else {
			fill.drop_groups(fill.start().group..fill.end());
			return;
		};
		if let Some(previous) = &self.arrivals {
			assert!(previous.same_channel(&arrivals), "held fetches belong to one track");
		} else {
			self.arrivals = Some(arrivals);
		}
		self.hold(fill, true);
	}

	/// Wait on the next group `fill` still owes, if any. A fresh fill counts against the
	/// limit, once what nobody waits on any more is released.
	fn hold(&mut self, fill: track::Fill, fresh: bool) {
		let mut next = Some((fill, fresh));
		while let Some((fill, fresh)) = next.take() {
			let Some(sequence) = fill.next() else {
				continue;
			};
			// A replacement attempt may arrive before the poll that would see the held one's
			// readers leave this group: that one moves on to the next group it owes, if any.
			if let Some(previous) = self.held.remove(&sequence)
				&& previous.fill.is_used()
			{
				next = Some((previous.fill, false));
			}
			if fresh {
				let moving = usize::from(next.is_some());
				if self.held.len() + moving >= self.max {
					self.held.retain(|_, held| held.fill.is_used());
				}
				if self.held.len() + moving >= self.max {
					fill.refuse(Error::TooManyRequests);
					continue;
				}
			}
			let waiter = kio::Waiter::new(std::task::Waker::from(Arc::new(FetchWake {
				sequence,
				ready: self.ready.clone(),
			})));
			self.held.insert(sequence, HeldFetch { fill, waiter, fresh });
			self.ready.lock().insert(sequence);
		}
	}

	fn poll(&mut self, sub: &Sub, now: crate::time::Instant, waiter: &kio::Waiter) {
		let Sub::Active(_) = sub else {
			for (_, held) in std::mem::take(&mut self.held) {
				held.fill.drop_groups(held.fill.start().group..held.fill.end());
			}
			let mut ready = self.ready.lock();
			if !ready.is_empty() {
				ready.clear();
			}
			drop(ready);
			self.arrivals = None;
			self.fold.set(None);
			return;
		};
		let Some(arrivals) = &self.arrivals else {
			return;
		};
		// One shared read arms the batch for new gaps, without copying the ledger or
		// re-polling held groups whose verdict did not change.
		let arrivals = arrivals.poll(waiter);
		if arrivals.is_closed() {
			self.ready.lock().extend(self.held.keys().copied());
		}
		for (gap, at) in arrivals.folds() {
			if at <= now {
				let mut groups = self.held.range(gap).map(|(sequence, _)| *sequence);
				if let Some(first) = groups.next() {
					let mut ready = self.ready.lock();
					ready.insert(first);
					ready.extend(groups);
				}
			}
		}
		let ready = match self.ready.poll(waiter, |ready| {
			if ready.is_empty() {
				Poll::Pending
			} else {
				Poll::Ready(())
			}
		}) {
			Poll::Ready(mut ready) => std::mem::take(&mut *ready),
			Poll::Pending => BTreeSet::new(),
		};
		let _ = self.ready.poll(waiter, |_| Poll::<()>::Pending);
		let mut unused = Vec::new();
		let mut settled = Vec::new();
		for sequence in ready {
			let Some(mut held) = self.held.remove(&sequence) else {
				continue;
			};
			#[cfg(test)]
			{
				self.polled += 1;
			}
			if held.fill.poll_unused(&held.waiter).is_ready() {
				unused.push(held.fill);
				continue;
			}
			let verdict = arrivals.poll_group(sequence, now, &held.waiter);
			if verdict.is_pending() && !held.fresh {
				self.held.insert(sequence, held);
				continue;
			}
			let gone = arrivals.given_up(held.fill.start().group..held.fill.end(), now);
			if verdict.is_pending() && gone.is_empty() {
				held.fresh = false;
				self.held.insert(sequence, held);
				continue;
			}
			// Never coming is a miss; a group given up keeps the error it was given up with.
			let decided = match verdict {
				Poll::Ready(res) => Some((sequence, res.err().unwrap_or(Error::NotFound))),
				Poll::Pending => None,
			};
			settled.push((held.fill, decided, gone));
		}
		let fold = arrivals
			.folds()
			.filter_map(|(gap, at)| self.held.range(gap).next().map(|_| at))
			.min();
		// Fills own track producers: answering or releasing one takes the track lock too.
		drop(arrivals);
		drop(unused);
		for (fill, decided, gone) in settled {
			// The group waited on is settled, so the fill moves past it: one landed without
			// the frames the fill starts from is no answer, and the cache serves what it has.
			// Answered before the bulk give-up, which knows no reason, so a drop's own error
			// reaches the fetch.
			if let Some((sequence, err)) = decided
				&& fill.next() == Some(sequence)
			{
				fill.group(sequence).reject(err);
			}
			for groups in gone.iter() {
				fill.drop_groups(groups.clone());
			}
			// Waits on the next group it owes, judged on the poll its readiness wakes.
			self.hold(fill, false);
		}
		self.fold.set(fold);
		let _ = self.fold.poll(waiter);
	}
}

impl ServeLoop {
	fn new(serve: &TrackServe, request: track::Request, info: track::Info) -> Self {
		// Register the fetch handler before accepting: `accept` releases the
		// request's own fetch gate, and a cache-miss fetch queued while TRACK_INFO
		// was in flight would be drained as NotFound in the gap.
		let dynamic = request.requests();
		// Lite-06+ resolves each subscription's start from its budget, so the floor
		// the SUBSCRIBE asks for is not where delivery begins until SUBSCRIBE_START says.
		let request = match serve.subscriber.version.resolves_start() {
			true => request.request_start(None),
			false => request,
		};
		// Marked received before any reader sees it, so none resolves an anchor this wire dropped.
		let serving = request.stage(info);
		serving.received();
		if serve.subscriber.version.resolves_anchors() {
			serving.anchored();
		}
		if serve.subscriber.version.fills_from_start() {
			serving.fills_from_start();
		}
		let serving = serving.commit();
		Self {
			edge: serving.consume(),
			largest: None,
			demand: serving.demand(),
			serving,
			dynamic,
			sub: Sub::None,
			fetches: kio::Tasks::new(),
			feed_fetches: FeedFetches::new(&serve.subscriber.runtime),
			closed: serve.subscriber.session.clone(),
			supports_update: !matches!(serve.subscriber.version, Version::Lite01 | Version::Lite02),
			supports_fetch: serve.subscriber.version.has_track_stream(),
			mode: ServeMode::Select,
			linger: crate::time::Deadline::new(&serve.subscriber.runtime),
			held: None,
			covered: None,
			closing: None,
		}
	}

	/// Apply a demand change, opening a fresh upstream subscription after a covered one
	/// unless the demand still asks for exactly what it covered.
	fn begin(&mut self, serve: &TrackServe, pref: Option<Subscription>) -> Result<(), Error> {
		if let Some(covered) = &self.covered {
			let ranges = match &pref {
				Some(pref) => wire_ranges(pref, self.serving.info().timescale)?,
				None => None,
			};
			if ranges.as_ref() == Some(covered) {
				return Ok(());
			}
			// What the copy cached may go stale before the next subscription says where
			// the live feed is.
			self.covered = None;
			self.serving.set_idle();
		}
		match serve.begin_subscription(&mut self.serving, &mut self.sub, pref, self.supports_update)? {
			Begin::Establish(est) => self.mode = ServeMode::Establish(est),
			Begin::None => {}
		}
		Ok(())
	}

	fn poll(&mut self, serve: &TrackServe, waiter: &kio::Waiter) -> Poll<ServeEnd> {
		// Immutable TRACK_INFO stays usable locally after withdrawal. Its wire hold
		// is only a bridge to SUBSCRIBE while this source is still advertised.
		if self.held.is_some()
			&& serve
				.route
				.as_ref()
				.is_some_and(|route| route.poll_closed(waiter).is_ready())
		{
			self.held = None;
		}
		let mut budget = kio::coop::Budget::new(32);
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
							serve.subscriber.remove_subscribe(id);
							return Poll::Ready(ServeEnd::GiveBack(err));
						}
					}
				}
				ServeMode::Tail { settle, owed, track } => {
					let _ = self.fetches.poll(waiter);
					self.feed_fetches
						.poll(&self.sub, serve.subscriber.runtime.now(), waiter);
					let mut whole = false;
					if settle
						.poll(waiter, |tail| {
							whole = owed
								.as_ref()
								.is_some_and(|owed| owed.iter().all(|owed| tail.covers(owed.clone())));
							whole
						})
						.is_ready()
					{
						if *track {
							return Poll::Ready(ServeEnd::Finished);
						}
						// Only the subscription ended: close our side as the publisher waits
						// for, and keep the copy and what it cached for the readers.
						self.mode = ServeMode::Select;
						if let Sub::Active(active) = std::mem::replace(&mut self.sub, Sub::None) {
							serve.subscriber.remove_subscribe(active.id);
							let mut writer = active.stream.writer;
							if writer.finish().is_ok() {
								self.closing = Some(writer);
							}
							// Unchanged ranges are what the publisher covered, even if a datagram
							// went missing. Changed ones settled by the grace instead may have
							// been covered before an update it never read.
							self.covered = (whole || !active.updated).then_some(active.ranges);
						}
						// Whatever the covered ranges leave out of the demand is asked again.
						if let Err(err) = self.begin(serve, self.serving.subscription()) {
							return Poll::Ready(ServeEnd::GiveBack(err));
						}
						continue;
					}
					if self.fetches.is_empty() && crate::util::poll_unwanted(&mut self.demand, waiter).is_ready() {
						return Poll::Ready(ServeEnd::Idle);
					}
					// The publisher already ended the track, so a session that dies now
					// finishes it, as `SubscriptionCleanup` does, rather than hand it to
					// another route. A copy whose subscription alone ended is handed back.
					if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
						return Poll::Ready(match track {
							true => ServeEnd::Finished,
							false => ServeEnd::GiveBack(crate::lite::error::from_transport(err)),
						});
					}
					return Poll::Pending;
				}
				ServeMode::Select => {
					if let Some(writer) = &mut self.closing
						&& writer.poll_close(waiter).is_ready()
					{
						self.closing = None;
					}

					// A SUBSCRIBE still unanswered once another instance takes the path may have
					// resolved it: refuse it. A withdrawal alone leaves it to its answer. Checked
					// before reading the stream, so an answer that only arrived alongside the
					// replacement cannot claim it, and before the flush, so an update parked on
					// flow control cannot hold the refusal back.
					if let Sub::Active(active) = &self.sub
						&& let Some(validity) = &active.unanswered
						&& let Err(err) = validity.check_unanswered(waiter)
					{
						tracing::debug!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "unanswered subscribe refused: instance replaced");
						return Poll::Ready(ServeEnd::GiveBack(err));
					}

					// Deliver any buffered SUBSCRIBE_UPDATE before selecting, so the
					// demand that produced it is on the wire.
					if let Sub::Active(active) = &mut self.sub {
						match active.stream.writer.poll_flush(waiter) {
							Poll::Ready(Ok(())) => {}
							// The publisher stopped reading, as it does once it finishes the
							// track. The update is dropped, and the responses decide how the
							// subscription ends: cleanly at the FIN, or with a reset's error.
							Poll::Ready(Err(err)) => {
								tracing::debug!(track = %serve.name, %err, "subscribe update dropped")
							}
							Poll::Pending => return Poll::Pending,
						}
					}

					// Biased: demand first, then completions, then closures.

					// (1) Track demand: a fetch, a subscription change, or the origin
					// handing the track to another route.

					// A fill is bounded and serves readers already waiting, so serve it ahead of
					// subscription churn.
					match self.dynamic.poll_requested_fill(waiter) {
						Poll::Ready(Ok(fill)) => {
							if self.supports_fetch {
								self.fetches
									.push(FillServeRun::new(serve.clone(), fill, self.serving.info().clone()));
							} else {
								// No FETCH on the wire: wait for the live feed, which a
								// subscription arriving alongside may still be opening.
								self.feed_fetches.push(fill, &self.serving.consume());
							}
							continue;
						}
						// A live clone can abort the producer with a specific cause.
						Poll::Ready(Err(err)) => return Poll::Ready(ServeEnd::GiveBack(err)),
						Poll::Pending => {}
					}
					match self.serving.poll_subscription_changed(waiter) {
						Poll::Ready(Ok(pref)) => {
							// Updating the upstream failed: hand the track back for another
							// route to resume.
							if let Err(err) = self.begin(serve, pref) {
								return Poll::Ready(ServeEnd::GiveBack(err));
							}
							continue;
						}
						Poll::Ready(Err(err)) => return Poll::Ready(ServeEnd::GiveBack(err)),
						Poll::Pending => {}
					}

					// The wanted group is subscription state: an update announces each new one
					// past our largest, and every later update restates it. Joins asking for
					// the same group coalesce into that one update.
					if serve.subscriber.version.has_refresh() {
						while let Poll::Ready(Some(largest)) = self.edge.poll_largest(waiter) {
							self.largest = Some(largest.group);
						}
						let _ = self.demand.poll_wanted_changed(waiter);
						if let Sub::Active(active) = &mut self.sub
							&& let Some(largest) = self.largest
							&& let Some(wanted) = self.demand.wanted().filter(|wanted| *wanted > largest)
							&& active.wanted != Some(wanted)
						{
							active.wanted = Some(wanted);
							if let Err(err) = buffer_update(active) {
								return Poll::Ready(ServeEnd::GiveBack(err));
							}
							continue;
						}
					}

					// (2) In-flight fetches; completions just retire. The demand is settled,
					// so a held fetch with no feed open is a miss.
					let _ = self.fetches.poll(waiter);
					self.feed_fetches
						.poll(&self.sub, serve.subscriber.runtime.now(), waiter);

					// (3) Nobody holds this copy anymore. Its upstream subscription already
					// went with the last subscriber, so it lingers, cache and all, for a
					// reader or fetch that asks again soon, then is dropped. In-flight
					// fetches keep it alive: work already accepted still gets finished.
					if self.fetches.is_empty() && crate::util::poll_unwanted(&mut self.demand, waiter).is_ready() {
						self.held = None;
						if self.linger.deadline().is_none() {
							let now = serve.subscriber.runtime.now();
							self.linger.set(now.checked_add(track::IDLE_LINGER));
						}
						// A reader returning restarts the countdown when it next leaves.
						if self.linger.poll(waiter).is_ready() {
							return Poll::Ready(ServeEnd::Idle);
						}
					} else {
						self.linger.set(None);
					}

					// (4) The upstream subscribe stream closed, or carried a START/END/DROP.
					// Partial message bytes persist in the reader's buffer across turns.
					if let Sub::Active(active) = &mut self.sub
						&& let Poll::Ready(res) = active
							.stream
							.reader
							.poll_decode_maybe::<lite::SubscribeResponse>(waiter)
					{
						// The publisher answered the SUBSCRIBE, so its demand stands on the
						// subscription now. A query still held once that subscription ends
						// stays local: nothing reopens the TRACK for it.
						self.held = None;
						active.unanswered = None;
						match res {
							Ok(Some(msg)) => {
								match &msg {
									// SUBSCRIBE_END declares the track's exclusive final
									// sequence, which may arrive while trailing groups are
									// still in flight. Record it on this segment's producer so
									// consumers learn the boundary early; the later stream FIN
									// then finds the track already finished.
									lite::SubscribeResponse::End(end) => {
										// Lower groups may still be on the wire, behind a higher
										// one, so the end holds readers at a hole until it fills or
										// its grace passes. finish_at rejects a boundary at or below a group
										// already received. lite-05 specified an inclusive end,
										// and `@moq/net` 0.1.3 to 0.1.9 sent one, so there it
										// only costs the early boundary: warn, and let the FIN
										// finish the track. Later drafts made it exclusive, so
										// the publisher contradicted its own end.
										if let Err(err) = self.serving.finish_at(Position::group(end.group)) {
											match serve.subscriber.version {
												Version::Lite05 => {
													tracing::warn!(track = %serve.name, group = end.group, %err, "invalid subscribe end")
												}
												_ => {
													tracing::warn!(track = %serve.name, group = end.group, %err, "subscribe end below a received group");
													return Poll::Ready(ServeEnd::GiveBack(Error::ProtocolViolation));
												}
											}
										}
										active.end = Some(end.clone());
									}
									// SUBSCRIBE_START names where each range's delivery starts,
									// the lowest being the first group this feed serves: the
									// publisher skipped everything below it (e.g. it could not
									// serve the requested frame). Record it as a drop signal,
									// so a reader waiting on a skipped group fails over
									// instead of stalling on a live route.
									lite::SubscribeResponse::Start(start) => {
										// One resolved start per range the SUBSCRIBE carried, unless an
										// update the publisher read first replaced them.
										if serve.subscriber.version.has_ranges()
											&& !active.updated && start.starts.len() != active.ranges.len()
										{
											tracing::warn!(track = %serve.name, starts = start.starts.len(), ranges = active.ranges.len(), "subscribe start resolves other ranges");
											return Poll::Ready(ServeEnd::GiveBack(Error::ProtocolViolation));
										}
										// No range starting anywhere leaves nothing owed.
										let served = start.starts.iter().flatten().min().copied();
										// Where the live feed is, on versions whose answer says.
										if serve.subscriber.version.has_largest() {
											self.serving.set_live(start.largest, served);
											if let Some(largest) = start.largest {
												self.serving.report_largest(largest);
											}
										}
										// A START describes the demand the SUBSCRIBE carried.
										// It applies only while the current start still matches
										// that demand (updates get no fresh START, so an update
										// that moved the start makes it stale, and one that
										// moved back restores it); elsewhere the
										// request-tracked floor stands rather than a guess.
										if active.start == active.requested {
											let _ = match (serve.subscriber.version.resolves_start(), served) {
												(true, Some(served)) => {
													self.serving.start_resolved(served, active.requested_max_delay)
												}
												_ => self.serving.start_at(served),
											};
										}
										active.served = served;
										active.starts = start.starts.clone();
										// The groups the SUBSCRIBE asked for below it are not
										// waited for, whatever the demand asks later. One that
										// still arrives is delivered, so they are not dropped.
										if let Some(requested) = active.requested
											&& let Some(served) = served && let Ok(mut tail) = active.tail.write()
										{
											tail.account(requested..served, serve.subscriber.runtime.now());
										}
									}
									// The publisher will never send these groups, so they
									// are accounted for without a stream, and a reader of
									// one still arriving ends with the drop's error now.
									lite::SubscribeResponse::Drop(dropped) => {
										if let Ok(mut tail) = active.tail.write() {
											tail.account(
												dropped.start..dropped.end.saturating_add(1),
												serve.subscriber.runtime.now(),
											);
										}
										self.serving.drop_groups(
											dropped.start..dropped.end.saturating_add(1),
											dropped.error(serve.subscriber.version),
										);
									}
									// OK just resolves the range (the producer already orders
									// groups).
									lite::SubscribeResponse::Ok(_) => {
										tracing::debug!(track = %serve.name, ?msg, "subscribe response")
									}
								}
								// Each pass re-polls the whole loop, so a peer streaming
								// responses must not hold the session's other tasks.
								ready!(budget.poll_yield(waiter));
								continue;
							}
							// Lite-07 FINs a covered bounded subscription without SUBSCRIBE_END,
							// which ends the subscription but not the track. The demand may have
							// widened it since, in an update the publisher never read.
							Ok(None)
								if serve.subscriber.version.has_ranges() && active.end.is_none() && active.bounded =>
							{
								tracing::debug!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "subscribe covered");
								let budget = self.serving.subscription().map(|sub| sub.budget());
								if let Ok(mut tail) = active.tail.write() {
									tail.set_grace(tail::grace(budget.unwrap_or_default()));
									tail.expire(serve.subscriber.runtime.now());
								}
								self.mode = ServeMode::Tail {
									settle: Settle::new(&serve.subscriber.runtime, active.tail.consume()),
									owed: active.covered(),
									track: false,
								};
								continue;
							}
							Ok(None) => {
								if serve.subscriber.version.has_track_stream() && active.end.is_none() {
									return Poll::Ready(ServeEnd::GiveBack(Error::ProtocolViolation));
								}
								tracing::debug!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "subscribe complete");
								// Upstream FIN'd the subscription: the publisher only FINs
								// once the track's final sequence is known and every group
								// stream finished, so the logical track is over for good
								// (before lite-07, bounded demand alone never FINs; the
								// publisher parks, since a cap can be raised). Those streams
								// can still be in flight, so wait for the tail before finishing.
								let subscription = self.serving.subscription();
								let requested_end = subscription.as_ref().and_then(|sub| sub.end());
								// The effective max delay is the stopgap grace: the wrong clock
								// (it bounds presentation-time drift), but it is how long the
								// subscriber was willing to wait for a late group anyway.
								if let Ok(mut tail) = active.tail.write() {
									tail.end();
									tail.set_grace(tail::grace(
										subscription.map(|sub| sub.budget()).unwrap_or_default(),
									));
									tail.expire(serve.subscriber.runtime.now());
								}
								self.mode = ServeMode::Tail {
									settle: Settle::new(&serve.subscriber.runtime, active.tail.consume()),
									owed: active.owed(requested_end).map(|owed| vec![owed]),
									track: true,
								};
								continue;
							}
							Err(err) => {
								tracing::warn!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, %err, "subscribe error");
								return Poll::Ready(ServeEnd::GiveBack(err));
							}
						}
					}

					// (5) The session died: hand the track back for another route, with
					// the session's error in case none takes it.
					if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
						return Poll::Ready(ServeEnd::GiveBack(crate::lite::error::from_transport(err)));
					}

					return Poll::Pending;
				}
			}
		}
	}
}

/// Serves one fill a group at a time, newest first for a descending reader, each with its
/// own [`FetchServeRun`], until it is answered or nobody waits on it.
struct FillServeRun {
	serve: TrackServe,
	fill: track::Fill,
	info: track::Info,
	run: Option<(u64, FetchServeRun)>,
}

impl FillServeRun {
	fn new(serve: TrackServe, fill: track::Fill, info: track::Info) -> Self {
		Self {
			serve,
			fill,
			info,
			run: None,
		}
	}
}

impl kio::Task for FillServeRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let last = match &mut self.run {
				Some((sequence, run)) => {
					ready!(run.poll(waiter));
					let sequence = *sequence;
					self.run = None;
					Some(sequence)
				}
				None => None,
			};
			// Withdrawn: nobody waits on what is left, so it records nothing.
			if self.fill.poll_unused(waiter).is_ready() {
				return Poll::Ready(());
			}
			let Some(sequence) = self.fill.next() else {
				return Poll::Ready(());
			};
			// Every run answers its group, so the same one coming back is a bug; ending the
			// fill names it dropped rather than fetching it forever.
			if last == Some(sequence) {
				debug_assert!(false, "fill group {sequence} left unanswered");
				return Poll::Ready(());
			}
			let run = FetchServeRun::new(self.serve.clone(), self.fill.group(sequence), self.info.clone());
			self.run = Some((sequence, run));
		}
	}
}

/// Serves one downstream fetch end-to-end on its own bidi stream: send FETCH,
/// then fill the group from the bare FRAME messages that follow. The track's
/// info comes from its TRACK_INFO (already known), and the group sequence
/// is implicit from the request.
struct FetchServeRun {
	validity: crate::model::broadcast::RequestValidity,
	gate: crate::auth::Gate,
	serve: TrackServe,
	session: Transport,
	info: track::Info,
	group: u64,
	state: FetchRunState,
}

enum FetchRunState {
	Open {
		request: Option<track::GroupFill>,
	},
	Send {
		request: Option<track::GroupFill>,
		stream: Stream<Transport, Version>,
		frame_start: u64,
		frame_end: Option<u64>,
	},
	/// Flushed; waiting for the publisher to answer before accepting.
	Answer {
		request: Option<track::GroupFill>,
		stream: Stream<Transport, Version>,
		frame_start: u64,
		frame_end: Option<u64>,
	},
	Ingest {
		stream: Stream<Transport, Version>,
		// Boxed, as is the ingest, so the other states stay small.
		producer: Box<crate::recv::Group>,
		// The accepted group's readers that have not yet picked it up from the cache.
		readers: track::FillReaders,
		ingest: Box<FrameIngest>,
	},
	Done,
}

impl FetchRunState {
	/// The downstream request, while it waits on the publisher's answer.
	fn request(&self) -> Option<&track::GroupFill> {
		match self {
			Self::Open { request } | Self::Send { request, .. } | Self::Answer { request, .. } => request.as_ref(),
			Self::Ingest { .. } | Self::Done => None,
		}
	}
}

impl FetchServeRun {
	fn new(serve: TrackServe, request: track::GroupFill, info: track::Info) -> Self {
		let session = serve.subscriber.session.clone();
		let group = request.sequence();
		Self {
			validity: serve.validity.start(),
			gate: serve.gate(),
			serve,
			session,
			info,
			group,
			state: FetchRunState::Open { request: Some(request) },
		}
	}
}

impl Drop for FetchServeRun {
	fn drop(&mut self) {
		// Dropped because the session stopped allowing the broadcast: reset with that
		// reason, so neither the publisher nor the waiting reader reads a routine cancel.
		// Any other drop keeps the default cancel.
		if matches!(self.state, FetchRunState::Done)
			|| self
				.serve
				.subscriber
				.auth
				.allows(crate::auth::Direction::Subscribe, self.serve.path.as_str())
		{
			return;
		}
		let err = Error::Unauthorized;
		match std::mem::replace(&mut self.state, FetchRunState::Done) {
			FetchRunState::Open { request } => {
				if let Some(request) = request {
					request.reject(err);
				}
			}
			FetchRunState::Send { request, stream, .. } | FetchRunState::Answer { request, stream, .. } => {
				stream.writer.abort(&err);
				if let Some(request) = request {
					request.reject(err);
				}
			}
			FetchRunState::Ingest {
				mut stream,
				producer,
				ingest: _ingest,
				..
			} => {
				stream.reader.abort(&err);
				stream.writer.abort(&err);
				let _ = producer.abort(err);
			}
			FetchRunState::Done => {}
		}
	}
}

impl kio::Task for FetchServeRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.state.request().is_some()
			&& let Err(err) = self.validity.check(waiter)
		{
			match std::mem::replace(&mut self.state, FetchRunState::Done) {
				FetchRunState::Open { request } => request.expect("request pending").reject(err),
				FetchRunState::Send { request, stream, .. } | FetchRunState::Answer { request, stream, .. } => {
					stream.writer.abort(&err);
					request.expect("request pending").reject(err);
				}
				_ => unreachable!(),
			}
			return Poll::Ready(());
		}
		loop {
			// A fetch nobody waits on any more is cancelled upstream, so the publisher stops
			// serving it (and a relay there releases its own FETCH). Ingest has its own check.
			if let Some(request) = self.state.request()
				&& request.poll_unused(waiter).is_ready()
			{
				tracing::debug!(track = %self.serve.name, group = self.group, "fetch abandoned");
				if let FetchRunState::Send { stream, .. } | FetchRunState::Answer { stream, .. } =
					std::mem::replace(&mut self.state, FetchRunState::Done)
				{
					stream.writer.abort(&Error::Cancel);
				}
				return Poll::Ready(());
			}

			match &mut self.state {
				FetchRunState::Open { request } => {
					ready!(self.gate.poll_ready(waiter));
					// The fill already has readers holding its demand. Backpressure keeps their
					// owned request alive; abandoning it above releases this wait too.
					let mut stream =
						match ready!(Stream::poll_open(&self.session, self.serve.subscriber.version, waiter)) {
							Ok(stream) => stream,
							Err(err) => {
								tracing::warn!(track = %self.serve.name, %err, "fetch stream open failed");
								request.take().expect("request pending").reject(err);
								self.state = FetchRunState::Done;
								return Poll::Ready(());
							}
						};

					// Only once the stream opens: credit and its header can wait, and
					// re-polls this state, so logging before it repeats per poll.
					tracing::debug!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, group = self.group, "fetch started");

					let request = request.take().expect("request pending");

					// A peer that predates lite-06 addresses whole groups only, so ask for
					// the whole group and number the response from 0. The wider group still
					// covers what the caller asked for (`fetch_group` positions their own
					// consumer), and is more reusable in the cache than the tail would have
					// been. Asking for the offset anyway would fail to encode and reject a
					// fetch we can serve.
					let frame_start = match self.serve.subscriber.version.has_frame_bounds() {
						true if !self.info.compress => request.frame_start(),
						true => 0,
						false => 0,
					};

					let frame_end = if self.serve.subscriber.version.has_frame_bounds() && !self.info.compress {
						request.frame_end()
					} else {
						None
					};

					let msg = lite::Fetch {
						epoch: self.serve.epoch.clone(),
						broadcast: self.serve.path.clone(),
						track: self.serve.name.as_str().into(),
						priority: request.priority(),
						// Older wires cannot carry it; the local cache judges it instead.
						max_delay: request
							.max_delay()
							.filter(|_| self.serve.subscriber.version.has_fetch_max_delay()),
						group: self.group,
						start_frame: frame_start,
						// A bounded plain copy keeps this provenance in the cache; older wires
						// and compressed groups widen to a complete representation.
						end_frame: frame_end.and_then(|end| end.checked_sub(1)),
					};
					stream = stream.with_stats(self.serve.subscriber.origin.stats());
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
						frame_end,
					};
				}
				FetchRunState::Send { stream, .. } => {
					if let Err(err) = ready!(stream.writer.poll_flush(waiter)) {
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
						frame_end,
					} = std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					self.state = FetchRunState::Answer {
						request,
						stream,
						frame_start,
						frame_end,
					};
				}
				FetchRunState::Answer { stream, .. } => {
					let ietf = ready!(self.serve.subscriber.peer_setup.poll_ietf(waiter));
					// Lite has no FETCH_OK: a publisher without the group resets the
					// stream instead. Accepting before the first byte (or a FIN, for an
					// empty group) would resolve every joined `fetch_group` to a group
					// that only fails on its first read, so wait for the answer.
					let answered = ready!(stream.reader.poll_has_more(waiter));
					let FetchRunState::Answer {
						request,
						stream,
						frame_start,
						frame_end,
					} = std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					let request = request.expect("request pending");
					if let Err(err) = answered {
						tracing::debug!(track = %self.serve.name, group = self.group, %err, "fetch refused");
						stream.writer.abort(&err);
						request.reject(err);
						return Poll::Ready(());
					}

					// Make the group available (resolving the downstream fetch) and fill
					// it. The track::Info only takes effect if the track isn't accepted yet
					// (a fetch with no live subscription); otherwise the group inherits the
					// accepted info.
					let group_info = self.info.clone();
					// The group's readers pick it up from the cache only when next polled,
					// so their demand outlives the request until then.
					let readers = request.readers();
					let producer = match request.accept_from(
						group_info,
						group::Bounds {
							start: frame_start,
							end: frame_end,
						},
					) {
						Ok(producer) => producer,
						Err(err) => {
							// Already served (a concurrent fetch) or the track closed.
							tracing::debug!(track = %self.serve.name, group = self.group, %err, "fetch not served");
							stream.writer.abort(&err);
							return Poll::Ready(());
						}
					};

					self.state = FetchRunState::Ingest {
						stream,
						producer: Box::new(crate::recv::Group::new(producer)),
						readers,
						ingest: Box::new(FrameIngest::new(
							lite::Framing::new(&self.info, self.serve.subscriber.version),
							self.serve.subscriber.version,
							ietf,
							frame_start,
						)),
					};
				}
				FetchRunState::Ingest {
					stream,
					producer,
					readers,
					ingest,
				} => {
					let Poll::Ready(res) = ingest.poll(&mut stream.reader, producer, waiter) else {
						// Still short of its end, so completion always wins. Once nobody
						// wants the rest (no reader left to pick it up, none reading it),
						// cancel upstream and abort the truncated group, never caching it
						// as complete. The abort is atomic with a new reader arriving.
						if readers.poll_unused(waiter).is_pending()
							|| producer.handle().poll_unused(waiter).is_pending()
						{
							return Poll::Pending;
						}
						if !producer.handle().abort_unused(Error::Cancel) {
							continue;
						}
						tracing::debug!(track = %self.serve.name, group = self.group, "fetch abandoned");
						let FetchRunState::Ingest { stream, .. } =
							std::mem::replace(&mut self.state, FetchRunState::Done)
						else {
							unreachable!()
						};
						stream.writer.abort(&Error::Cancel);
						return Poll::Ready(());
					};
					let FetchRunState::Ingest { producer, .. } =
						std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					match res {
						Ok(()) => {
							let producer = producer;
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
