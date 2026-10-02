use crate::runtime::Timers as _;
use std::{
	collections::{HashMap, hash_map::Entry},
	task::{Poll, ready},
	time::Duration,
};

use crate::{
	Error, Path, PathOwned, SessionError, Timescale, broadcast,
	coding::{Decode, DecodeError, Reader, Stream},
	frame, group,
	ietf::{self, Control, FetchType, Filter, GroupOrder, RequestId},
	origin, track,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, Tasks},
};

use super::{Message, Version, cluster, error::request, peer, request_update};
use crate::tail::{Reading, Settle, Tail};

use kio::Lock;

const TRACK_ALIAS_TIMEOUT: Duration = Duration::from_secs(1);

/// How many cancelled aliases to remember. Objects keep arriving for about a round trip
/// after we cancel, so a handful covers the window, while the cap keeps a long session with
/// heavy subscription churn from accumulating tombstones for its whole lifetime.
///
/// The bound is a count rather than a deadline, which is what keeps eviction synchronous
/// with retirement instead of needing a timer to sweep expired entries. The trade is that a
/// session cancelling more than this many distinct aliases inside one round trip evicts a
/// tombstone whose objects are still arriving; those groups fall back to the unknown-alias
/// wait, which is the old behavior rather than a new failure.
const RETIRED_ALIAS_CAPACITY: usize = 64;

/// What a track alias currently refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Alias {
	/// An established subscription. Groups carrying this alias belong to it.
	Active(RequestId),

	/// A subscription we cancelled, whose publisher may still be feeding the alias.
	///
	/// The publisher only stops once our cancellation reaches it, so objects keep arriving
	/// for at least a round trip afterwards. Remembering the alias is what lets us discard
	/// them immediately instead of stalling each one on [`TRACK_ALIAS_TIMEOUT`] and calling
	/// it unknown.
	///
	/// It does not make that window safe, only quiet. A publisher that has processed the
	/// cancellation may reassign the alias, and nothing on a group stream distinguishes the
	/// old subscription's objects from the new one's, so a group still in flight when the
	/// new SUBSCRIBE_OK binds the alias is delivered to the new track. The protocol offers
	/// no way to tell them apart: the alias is the only identifier a group carries, and the
	/// draft permits the reuse as long as the two tracks are not live at once. Cancelling
	/// promptly is what bounds the exposure, since it caps the arrival window at a round
	/// trip rather than leaving it open for the life of the session.
	Retired,
}

/// The aliases a remote publisher has bound on this session, plus the cancelled ones we
/// still remember.
#[derive(Default)]
struct AliasTable {
	map: HashMap<u64, Alias>,

	/// Retired aliases in retirement order, so the oldest is forgotten first.
	retired: std::collections::VecDeque<u64>,
}

type TrackAliases = kio::Producer<AliasTable>;

fn insert_track_alias(aliases: &TrackAliases, alias: u64, request_id: RequestId) -> Result<(), Error> {
	let mut aliases = aliases.write().map_err(|_| Error::Dropped)?;
	let table = &mut *aliases;

	match table.map.entry(alias) {
		// Our subscription is gone, so the publisher is free to point the alias somewhere
		// new. Reclaiming it also drops the tombstone early, which reopens the window
		// described on `Alias::Retired`: a group from the old subscription arriving after
		// this lands is indistinguishable from one for the new track.
		Entry::Occupied(mut entry) if *entry.get() == Alias::Retired => {
			entry.insert(Alias::Active(request_id));
			table.retired.retain(|&retired| retired != alias);
			Ok(())
		}
		Entry::Occupied(entry) if *entry.get() == Alias::Active(request_id) => Ok(()),
		Entry::Occupied(_) => Err(Error::Duplicate),
		Entry::Vacant(entry) => {
			entry.insert(Alias::Active(request_id));
			Ok(())
		}
	}
}

/// Whether an error means the peer broke the protocol, as opposed to a stream or
/// transport failing on its own.
///
/// Only the former justifies taking the whole session down. An encode error is ours,
/// not the peer's: we cannot ask it to answer for a message we failed to write.
pub(super) fn is_protocol_violation(err: &Error) -> bool {
	matches!(
		err,
		Error::Decode(_)
			| Error::BoundsExceeded(_)
			| Error::WrongSize
			| Error::TooManyParameters
			| Error::ProtocolViolation
			| Error::UnexpectedMessage
			| Error::UnexpectedStream
	)
}

/// Retire an alias, so groups still in flight for it are dropped promptly rather than
/// reported as unknown (draft-19 section 11.1).
///
/// Only retires an alias that still belongs to this request: a later subscription may
/// already have reclaimed it, and that binding outranks a departing owner.
fn retire_track_alias(aliases: &TrackAliases, alias: u64, request_id: RequestId) {
	let Ok(mut aliases) = aliases.write() else {
		return;
	};
	let table = &mut *aliases;

	if table.map.get(&alias) != Some(&Alias::Active(request_id)) {
		return;
	}

	table.map.insert(alias, Alias::Retired);
	table.retired.push_back(alias);

	while table.retired.len() > RETIRED_ALIAS_CAPACITY {
		let oldest = table.retired.pop_front().expect("non-empty above the capacity");
		// Only forget an entry that is still a tombstone. A reclaimed alias is live again
		// and its own retirement is queued separately.
		if table.map.get(&oldest) == Some(&Alias::Retired) {
			table.map.remove(&oldest);
		}
	}
}

#[derive(Default)]
struct State {
	// Each active subscription
	subscribes: HashMap<RequestId, TrackState>,

	// Joining FETCH request ids, mapped to the SUBSCRIBE they name.
	fetches: HashMap<RequestId, RequestId>,

	// Track aliases chosen by the remote publisher.
	aliases: TrackAliases,

	// Each broadcast created by a PUBLISH_NAMESPACE message.
	broadcasts: HashMap<PathOwned, BroadcastState>,
}

impl State {
	/// End every active subscription with the error that ended the session.
	///
	/// Active receive tasks abort their own groups. Abort any head waiting for its
	/// tail here, along with the track. Ordinary unsubscribe removes its entry.
	fn abort(&mut self, err: &Error) {
		for (_, mut track) in self.subscribes.drain() {
			if let Some(request) = track.pending.take() {
				request.reject(err.clone());
			}
			if let Fill::Ready { producer, .. } = &*track.fill.read() {
				let _ = producer.clone().abort(err.clone());
			}
			if let Some(producer) = track.producer {
				let _ = producer.abort(err.clone());
			}
		}
	}
}

impl Drop for State {
	fn drop(&mut self) {
		// The session dispatcher owns this state and can be dropped at any await. A
		// session that ended with an error already aborted these with it; what
		// remains was cancelled with the dispatcher.
		self.abort(&Error::Cancel);
	}
}

/// The head of a joined group, delivered on the subscription's fill fetch stream.
///
/// Draft-20's current-group join (section 5.1.6) splits one group across two streams: the
/// fill carries the objects already published when we subscribed, and the subscription
/// carries everything after them. The model has one producer per group, so the fill owns it
/// while it writes the head and hands it over here for the live tail to append to.
enum Fill {
	/// Requested, waiting on SUBSCRIBE_OK: it declares the timescale the fill's own object
	/// timestamps are in, and the fetch stream can arrive before it does.
	Requested,

	/// Ready to be served, in these timestamp units. `None` means the publisher opted the
	/// track out of timestamps, so its frames are stamped on arrival.
	Serving(Option<Timescale>),

	/// A fetch stream is writing the head. A second one answers no request of ours.
	Active,

	/// The head is written: `sequence` holds objects up to but excluding `next`, and its
	/// producer is waiting for the live tail to claim it.
	///
	/// The tail is what ends the group, and a publisher serving the subscription's range
	/// opens a stream for it even when the group ended at the join point, since that empty
	/// stream is how the group ends. One that opens none instead leaves this head unfinished
	/// until the subscription ends, which is what publishes it.
	///
	/// Nothing shorter is safe to infer. A later group arriving looks like proof that no
	/// tail is coming, but streams are independent: the tail's own can still be behind it.
	/// Finishing the head on that guess drops the tail when it lands.
	Ready {
		sequence: u64,
		next: u64,
		producer: group::Producer,
	},

	/// No head is coming: none was requested, the fill failed, or the tail already claimed
	/// it. A subgroup stream that starts mid-group is then unstitchable and gets dropped,
	/// which degrades the join to the next group boundary.
	Done,
}

impl Fill {
	/// Whether a head might still arrive or is waiting to be claimed, which is what makes a
	/// subgroup stream worth peeking before its group is created.
	fn outstanding(&self) -> bool {
		!matches!(self, Fill::Done)
	}

	/// Take the head for `sequence`, if this is one and it ends where the tail begins.
	///
	/// `start` is the Object ID the tail stream starts at, or `None` for a tail with no
	/// objects of its own, which takes the head whatever it ends at.
	fn claim(&mut self, sequence: u64, start: Option<u64>) -> Result<Option<group::Producer>, Error> {
		match *self {
			Fill::Ready { sequence: s, next, .. } if s == sequence => {
				if start.is_some_and(|start| start != next) {
					// A head that stops somewhere other than where the tail starts leaves a
					// hole the model cannot express, so neither half of the group is usable.
					tracing::warn!(sequence, next, start, "the fill does not meet the live tail");
					self.release();
					return Err(Error::Unsupported);
				}
			}
			// Nothing of ours: no head at all, or one for another group whose own tail may
			// still claim it.
			_ => return Ok(None),
		}

		match std::mem::replace(self, Fill::Done) {
			Fill::Ready { producer, .. } => Ok(Some(producer)),
			// Unreachable: the match above proved it is Ready.
			_ => Ok(None),
		}
	}

	/// Install the head a finished fetch stream produced.
	///
	/// [`Fill::Done`] is terminal: the subscription ended while the head was being written,
	/// and its teardown could not reach a producer the fetch stream still owned. Publish
	/// what the head carried rather than installing it for a tail that is never coming, or
	/// it outlives the subscription unfinished.
	fn install(&mut self, head: Fill) {
		match self {
			Fill::Done => {
				let mut head = head;
				head.release();
			}
			_ => *self = head,
		}
	}

	/// Release a head nothing claimed, publishing the objects it did carry.
	///
	/// The tail is what normally ends the group, so this is the fallback for when none is
	/// coming: the subscription ended, or the tail that arrived could not be stitched.
	/// Finishing rather than aborting, because the head is a valid prefix of the group: it
	/// starts at the group's first object and has no holes.
	fn release(&mut self) {
		if let Fill::Ready { producer, .. } = std::mem::replace(self, Fill::Done) {
			let _ = producer.finish();
		}
	}
}

/// A pre-draft-20 joining FETCH, sent as its own request after SUBSCRIBE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoiningFetch {
	/// The current group's head: `RelativeJoining` at this offset, which is always 0.
	Relative { group_offset: u64 },
	/// Whole groups from `group_id` through the live edge.
	Absolute { group_id: u64 },
}

/// What a SUBSCRIBE_OK told us about the track it accepted.
struct Accepted {
	/// The Track Alias the publisher bound to this subscription.
	alias: u64,

	/// The units its object timestamps are in, when it declared any.
	timescale: Option<Timescale>,
	/// The publisher's track default in wire order, if declared.
	priority: Option<u8>,

	/// The largest Location in the track, absent when it has no content yet. That absence
	/// is what says a fill we asked for is owed nothing.
	largest: Option<ietf::Location>,
}

struct TrackState {
	producer: Option<track::Producer>,
	/// The origin request, until SUBSCRIBE_OK accepts it. Abort rejects this: the
	/// producer does not exist yet, and dropping the setup task would be `Dropped`.
	pending: Option<track::Request>,
	name: String,
	alias: Option<u64>,

	// The backfill this subscription asked for, and the rendezvous between its fetch
	// stream and the subgroup stream carrying the rest of the group.
	fill: kio::Producer<Fill>,

	// The broadcast this track was subscribed from. With the track name it forms the full
	// track name, which is what decides whether a repeated alias is the fatal collision
	// (one alias, two tracks) or the legal sharing of an alias across subscriptions.
	broadcast: PathOwned,

	// Units for this track's object Timestamps, from the TIMESCALE Track Property in
	// SUBSCRIBE_OK. `None` until it arrives, and for a track that declares none: the
	// publisher opted out of timestamps, so frames are stamped on arrival instead.
	timescale: Option<Timescale>,

	// The SUBSCRIBE_OK Largest Location, which bounds a joining FETCH stitch.
	largest: Option<ietf::Location>,

	// The joining FETCH's own request id, when one was sent.
	fetch_id: Option<RequestId>,

	// A pre-draft-20 joining FETCH, which reuses the fill rendezvous.
	joining: Option<JoiningFetch>,

	// The data streams read so far, which PUBLISH_DONE's Stream Count is checked against.
	tail: kio::Producer<Tail>,
}

impl TrackState {
	#[cfg(test)]
	fn new(
		producer: track::Producer,
		broadcast: PathOwned,
		fill: kio::Producer<Fill>,
		joining: Option<JoiningFetch>,
	) -> Self {
		let mut state = Self::pending(producer.name().to_owned(), broadcast, fill, joining);
		state.producer = Some(producer);
		state
	}

	fn pending(name: String, broadcast: PathOwned, fill: kio::Producer<Fill>, joining: Option<JoiningFetch>) -> Self {
		Self {
			producer: None,
			pending: None,
			name,
			alias: None,
			broadcast,
			timescale: None,
			fill,
			largest: None,
			fetch_id: None,
			joining,
			tail: Default::default(),
		}
	}
}

struct BroadcastState {
	// The route announced into our origin for this namespace, post-charge.
	route: crate::origin::Route,

	// The served route: dropping it (and the serve task's clone) retracts the
	// route and rejects its queued requests. `None` while the session's limit holds
	// it back, though the peer still advertises it.
	dynamic: Option<crate::origin::Dynamic>,

	// Bumped each time the route attaches, so a serve task outlived by a limit that
	// took the route away and gave it back ends rather than serve beside the new one.
	generation: u64,

	// active number of PUBLISH_NAMESPACE messages.
	count: usize,

	// One minted source per requested path under the namespace, each closed
	// when its guard drops.
	sources: HashMap<PathOwned, crate::model::broadcast::SourceGuard>,
}

/// What one advertisement said, once its parameters are resolved against the session.
struct Advertised {
	/// The route it describes, with this link's price already charged. The prefix
	/// is stamped where the advertisement attaches (the namespace).
	route: crate::origin::Route,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: crate::transport::poll::Session> {
	// Arms the track-alias and request-id timeouts.
	runtime: crate::time::Clock,
	session: S,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Producer,
	control: Control,
	// The origin naming this link for split-horizon (`Route.via`) when the peer
	// declares none of its own (see `session_route`). Base moq-transport carries no
	// hop ids, so a peer only has an identity if it negotiated the MoQ Cluster
	// extension or the caller assigned it one (`Client::with_peer_hop`).
	//
	// Otherwise this is `Hop::UNKNOWN` (0), the reserved "no identity" value.
	// The assigned id stays local: it is never written into a hop chain, so a peer
	// that withheld an identity is not named on the wire. A server answers it per
	// accepted session; a client only when it knows the peer.
	session_origin: crate::Hop,
	// A random Hop ID of this connection's own, written as the first hop of any path
	// that arrives naming no publisher, so a publisher that reconnects reads downstream
	// as a new one. Fresh per connection, unlike `session_origin`.
	stamp: crate::Hop,
	// Our own Hop ID, which an advertisement must not already contain: one that does
	// looped back through us.
	self_origin: crate::Hop,
	// What the peer declared in its SETUP.
	peer_setup: peer::PeerSetup,
	// Local policy for what pulling from this peer costs, overriding whatever it
	// declared. See `cluster::link_cost`.
	cost: Option<u64>,
	state: Lock<State>,
	tasks: Tasks,
	version: Version,
	// Set once the peer sends a GOAWAY; new SUBSCRIBEs are then rejected with
	// Error::GoingAway (the peer told us to stop opening streams).
	going_away: crate::goaway::GoingAway,
	// Our grant (MoQ Auth): a subscription it stops covering is cancelled.
	auth: crate::auth::Handle,
	// The AUTHORIZATION TOKEN this side presents on its SUBSCRIBE requests and their
	// REQUEST_UPDATEs (MoQ request-token). A shared handle so a client can replace it while the
	// session runs; the default presents none. A client credential.
	request_token: crate::RequestToken,
	// Whether we declared MoQ Solicit in our SETUP (`solicit::into_setup`). True by default;
	// a side that does not offer it (`Extensions::solicit` off) sets it false. It gates whether an
	// unsolicited PUBLISH_NAMESPACE from a solicit-aware peer is a violation: only a peer that
	// disregarded a requirement we actually stated is at fault.
	declared_solicit: bool,
}

/// Resolve the subscription a data stream belongs to.
///
/// SUBSCRIBE_OK can be reordered behind the stream it describes, so an alias we have not
/// seen is worth waiting on briefly (draft-19 section 11.4.2). Three outcomes:
/// the subscription, [`Error::Cancel`] for an alias we retired, and [`Error::NotFound`]
/// once the wait expires without any binding at all.
async fn resolve_track_alias(
	runtime: &crate::time::Clock,
	aliases: kio::Consumer<AliasTable>,
	alias: u64,
) -> Result<RequestId, Error> {
	let mut timeout = crate::runtime::Deadline::after(runtime, TRACK_ALIAS_TIMEOUT);
	kio::wait(|waiter| {
		let resolved = aliases.poll(waiter, |aliases| match aliases.map.get(&alias) {
			Some(Alias::Active(request_id)) => Poll::Ready(Ok(*request_id)),
			// A subscription we already cancelled, whose publisher has not caught up with
			// our STOP_SENDING. Discard the group now rather than waiting out the timeout
			// for a binding that is never coming.
			Some(Alias::Retired) => Poll::Ready(Err(Error::Cancel)),
			None => Poll::Pending,
		});
		if let Poll::Ready(result) = resolved {
			return Poll::Ready(result.unwrap_or(Err(Error::Dropped)));
		}
		if timeout.poll(waiter).is_ready() {
			return Poll::Ready(Err(Error::NotFound));
		}
		Poll::Pending
	})
	.await
}

impl<S> Subscriber<S>
where
	S: crate::transport::poll::Boxable,
{
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		runtime: crate::time::Clock,
		session: S,
		origin: origin::Producer,
		control: Control,
		peer_hop: Option<crate::Hop>,
		peer_setup: peer::PeerSetup,
		self_origin: crate::Hop,
		cost: Option<u64>,
		version: Version,
		tasks: Tasks,
		going_away: crate::goaway::GoingAway,
	) -> Self {
		Self {
			runtime,
			session,
			origin,
			control,
			session_origin: peer_hop.unwrap_or(crate::Hop::UNKNOWN),
			stamp: crate::Hop::random(),
			self_origin,
			peer_setup,
			cost,
			state: Default::default(),
			tasks,
			version,
			going_away,
			auth: crate::auth::Handle::new(false),
			request_token: crate::RequestToken::default(),
			// We declare MoQ Solicit by default; `with_solicit(false)` opts out.
			declared_solicit: true,
		}
	}

	/// Bound what we subscribe to by the grant this session's tokens earn (MoQ Auth),
	/// and what the peer may publish to us by the session's limit, as either changes.
	pub fn with_auth(mut self, auth: crate::auth::Handle) -> Self {
		self.auth = auth;
		let this = self.clone();
		self.tasks.push(async move { this.run_limit().await });
		self
	}

	/// Follow the session's limit, attaching every withheld namespace a new limit
	/// covers. Each route's own serve task holds itself back when a limit no longer
	/// covers it.
	async fn run_limit(&self) {
		let mut epoch = 0;
		loop {
			let permit = kio::wait(|waiter| {
				self.auth
					.poll_permit(crate::auth::Direction::Subscribe, &mut epoch, waiter)
			})
			.await;
			let mut state = self.state.lock();
			let mut attached = Vec::new();
			for (path, entry) in state.broadcasts.iter_mut() {
				let allowed = permit.within_limit(path.as_str());
				match &entry.dynamic {
					None if allowed => {
						let mut route = entry.route.clone();
						if self.going_away.is_set() {
							route.cost = crate::origin::Cost::DRAIN;
						}
						let Ok(dynamic) = self.origin.dynamic(path, route) else {
							continue;
						};
						tracing::info!(route = %self.origin.absolute(path), "namespace authorized again");
						entry.dynamic = Some(dynamic);
						entry.generation += 1;
						attached.push((path.clone(), entry.generation));
					}
					_ => {}
				}
			}
			drop(state);
			for (path, generation) in attached {
				self.serve_route(path, generation);
			}
		}
	}

	/// Serve the requests beneath one attached namespace on its own task.
	fn serve_route(&self, path: PathOwned, generation: u64) {
		let this = self.clone();
		self.tasks.push(async move {
			// stop_announce is the authoritative remover: it drops the entry
			// (retracting the route) once the announce refcount hits zero,
			// which is what makes run_route exit, as does a limit holding it back.
			this.run_route(path, generation).await;
		});
	}

	/// Whether we declared MoQ Solicit in our SETUP. A side that does not offer it
	/// ([`Extensions::solicit`](crate::setup::Extensions::solicit) off) passes false, so an
	/// unsolicited PUBLISH_NAMESPACE from a solicit-aware peer is expected rather than a
	/// violation.
	pub fn with_solicit(mut self, declared: bool) -> Self {
		self.declared_solicit = declared;
		self
	}

	/// Present this request token (the AUTHORIZATION TOKEN parameter value) on the SUBSCRIBE
	/// requests this side sends, so a client authorizes its subscribes the standard draft-17+
	/// way (MoQ request-token). A shared handle, so a replaced token is re-presented on each
	/// live subscription as a REQUEST_UPDATE.
	pub fn with_request_token(mut self, token: crate::RequestToken) -> Self {
		self.request_token = token;
		self
	}

	/// End every active subscription with the error that ended the session.
	pub fn abort(&self, err: &Error) {
		self.state.lock().abort(err);
	}

	/// Leave `alias` in the state a cancelled subscription leaves behind: bound to a
	/// subscription, then retired.
	///
	/// The alias table is private to this module and the loop that answers a group for a
	/// retired alias lives in `session.rs`, so this is what lets that loop be driven end to
	/// end from there.
	#[cfg(test)]
	pub(super) fn retire_alias(&self, alias: u64) {
		// Which request owned the alias does not matter, only that retirement follows the
		// same binding it does in production.
		const REQUEST_ID: RequestId = RequestId(0);

		let aliases = self.state.lock().aliases.clone();
		insert_track_alias(&aliases, alias, REQUEST_ID).expect("bind the alias");
		retire_track_alias(&aliases, alias, REQUEST_ID);
	}

	/// What the peer declared in its SETUP, or the default (extension off) on a version
	/// that cannot negotiate it. See [`super::Publisher::peer`].
	pub(super) async fn peer(&self) -> cluster::Peer {
		match cluster::supported(self.version) {
			true => self.peer_setup.get().await.cluster,
			false => cluster::Peer::default(),
		}
	}

	/// The announcing session's declared or assigned identity, for split-horizon.
	///
	/// Local selection state: it is stored as `Route.via` and never written into the
	/// hop chain, so an assigned id is not forwarded as a name for a peer that
	/// declined to give one.
	fn via(&self, peer: &cluster::Peer) -> crate::Hop {
		peer.identity().unwrap_or(self.session_origin)
	}

	/// The route for an advertisement that carries no path of its own.
	///
	/// Base moq-transport has no hops on the wire, so the chain is this connection's
	/// stamp, naming the unknown publisher for as long as the connection lasts, then
	/// the anonymous 0 that keeps it ranked below identified routes.
	/// The session's assigned identity stays on `via` for split-horizon; putting it in
	/// the chain would publish a name for a peer that declined to give one.
	///
	/// The link is charged all the same. Such an advertisement carries no ROUTE_COST,
	/// which reads as 0, but the draft charges every advertisement for the direction it
	/// arrived over regardless. Skipping it would forward a paid upstream to
	/// cluster-aware peers as free and pull subscriptions onto the wrong relay.
	///
	/// It is charged only one hop, though the chain it stands for may be arbitrarily
	/// long: a peer that carries no hop ids hides its depth, so this route understates
	/// its true length. Price such a link with [`crate::Client::with_cost`].
	fn session_route(&self, peer: &cluster::Peer) -> crate::origin::Route {
		let mut hops = crate::Hops::new();
		hops.stamp(self.stamp)
			.expect("an empty hop chain has room for the stamp and its 0");
		crate::origin::Route::default()
			.with_hops(hops)
			.with_via(self.via(peer))
			// A peer with no Cluster extension advertises no cost at all, so its cold
			// path is unknown rather than free.
			.with_cost(crate::origin::Cost::UNKNOWN.charged(cluster::link_cost(self.cost, peer)))
	}

	/// The route an advertisement describes, or `None` when it must be discarded.
	///
	/// A negotiated peer supplies the path and cost, so the route is what the mesh
	/// actually knows: the full chain, and the accumulated cost plus this link's price.
	/// A path starting with 0 gets this connection's stamp in front of it; the 0s stay. An advertisement whose path already contains our own Hop ID looped back,
	/// and neither forwarding it nor subscribing through it is safe.
	fn route(&self, advert: Option<&cluster::Advert>, peer: &cluster::Peer) -> Option<Advertised> {
		let Some(advert) = advert else {
			return Some(Advertised {
				route: self.session_route(peer),
			});
		};

		if advert.loops(self.self_origin) {
			return None;
		}

		let mut route = advert
			.route(cluster::link_cost(self.cost, peer))
			.with_via(self.via(peer));
		route.hops.stamp(self.stamp).ok()?;
		Some(Advertised { route })
	}

	/// Bind the alias the publisher chose for this subscription.
	///
	/// Two failures, and only one of them is the session's. A publisher may hand the same
	/// alias to several subscriptions of one track, which draft-19 section 5.1 allows and
	/// expects the subscriber to demux by re-applying each subscription's filter. Ours are
	/// all LargestObject, so they are indistinguishable and we cannot: that costs the one
	/// subscription ([`Error::Unsupported`]). The same alias naming a *different* track is
	/// the collision section 11.1 makes fatal ([`Error::Duplicate`]).
	fn register_alias(&self, request_id: RequestId, alias: u64) -> Result<(), Error> {
		let mut state = self.state.lock();
		if !state.subscribes.contains_key(&request_id) {
			return Err(Error::NotFound);
		}

		if let Err(err) = insert_track_alias(&state.aliases, alias, request_id) {
			return Err(match self.alias_names_same_track(&state, alias, request_id) {
				true => Error::Unsupported,
				false => err,
			});
		}

		state.subscribes.get_mut(&request_id).unwrap().alias = Some(alias);
		Ok(())
	}

	/// Whether the subscription already holding `alias` is for the same full track name as
	/// `request_id`, making the repeat legal sharing rather than a collision.
	fn alias_names_same_track(&self, state: &State, alias: u64, request_id: RequestId) -> bool {
		let aliases = state.aliases.read();
		let Some(Alias::Active(holder)) = aliases.map.get(&alias).copied() else {
			return false;
		};

		let (Some(held), Some(new)) = (state.subscribes.get(&holder), state.subscribes.get(&request_id)) else {
			return false;
		};

		held.broadcast == new.broadcast && held.name == new.name
	}

	/// Take the origin request back out of a subscription that is still setting up.
	fn take_pending(&self, request_id: RequestId) -> Option<track::Request> {
		self.state.lock().subscribes.get_mut(&request_id)?.pending.take()
	}

	fn remove_subscribe(&self, request_id: RequestId) -> Option<TrackState> {
		let mut state = self.state.lock();
		let track = state.subscribes.remove(&request_id)?;
		if let Some(fetch_id) = track.fetch_id {
			state.fetches.remove(&fetch_id);
		}
		if let Some(alias) = track.alias {
			retire_track_alias(&state.aliases, alias, request_id);
		}
		// The subscription is over, so the tail a fill's head was waiting for is never
		// coming. Publish what it did carry rather than dropping the producer unfinished.
		if let Ok(mut fill) = track.fill.write() {
			fill.release();
		}
		Some(track)
	}

	/// The prefixes to issue SUBSCRIBE_NAMESPACE for: this handle's permitted scope,
	/// relative to its root.
	///
	/// The scope is what we may ASK the peer for; the root is where what comes back
	/// MOUNTS locally. Those are independent, and only coincide when the peer shares
	/// our namespace -- a peer outside it has never heard of our root, so a rooted
	/// subscriber asks for its scope and mounts the replies under the root.
	///
	/// Asked unconditionally: a peer with nothing to advertise answers with an empty set,
	/// which costs one stream.
	pub fn subscribe_prefixes(&self) -> Vec<PathOwned> {
		crate::model::interest_prefixes(&self.origin.allowed())
	}

	/// Send SUBSCRIBE_NAMESPACE for one prefix on a bidi stream.
	/// The caller is responsible for opening the appropriate stream type
	/// (virtual for v14/v15, real bidi for v16+), one per prefix.
	///
	/// A failure here is per-prefix, so the caller decides what it means for the
	/// session: [`is_protocol_violation`] separates the peer's fault (fatal) from a
	/// stream of ours that simply died (survivable).
	pub async fn run_subscribe_namespace<T: crate::transport::poll::Session>(
		&mut self,
		mut stream: Stream<T, Version>,
		prefix: PathOwned,
	) -> Result<(), Error> {
		// A peer that sent GOAWAY told us to stop opening requests on this session,
		// announce-interest included (draft-19 sect 10.4).
		if self.going_away.is_set() {
			return Err(Error::GoingAway);
		}

		// Hidden namespaces are requested too, as on moq-lite: the session mirrors the
		// peer into the origin and each local reader opts in on its own. The parameter
		// fails decoding at a peer that doesn't know it, so it waits on the peer's SETUP
		// to say whether it does (MoQ Hidden).
		let hidden = self.peer_setup.get().await.hidden;

		let request_id = self.control.next_request_id(&self.runtime).await?;

		// Draft-18+ uses SUBSCRIBE_NAMESPACE (0x50); earlier drafts use the legacy
		// 0x11 message with a Subscribe Options field.
		match self.version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				let msg = ietf::SubscribeNamespaceLegacy {
					request_id,
					namespace: prefix.clone(),
					subscribe_options: 0x01, // NAMESPACE only
					hidden,
				};
				stream.writer.encode(&ietf::SubscribeNamespaceLegacy::ID).await?;
				stream.writer.encode(&msg).await?;
			}
			_ => {
				let msg = ietf::SubscribeNamespace {
					request_id,
					namespace: prefix.clone(),
					hidden,
				};
				stream.writer.encode(&ietf::SubscribeNamespace::ID).await?;
				stream.writer.encode(&msg).await?;
			}
		}

		tracing::debug!(%prefix, "subscribe_namespace sent");

		// Read response
		let type_id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let mut data = stream.reader.read_exact(size as usize).await?;

		match type_id {
			ietf::SubscribeNamespaceOk::ID if self.version == Version::Draft14 => {
				let _msg = ietf::SubscribeNamespaceOk::decode_msg(&mut data, self.version)?;
			}
			ietf::RequestOk::ID => {
				let _msg = ietf::RequestOk::decode_msg(&mut data, self.version)?;
			}
			ietf::SubscribeNamespaceError::ID if self.version == Version::Draft14 => {
				let msg = ietf::SubscribeNamespaceError::decode_msg(&mut data, self.version)?;
				let err = request::from_code(msg.error_code, request::Kind::SubscribeNamespace, self.version);
				tracing::warn!(%err, reason = %msg.reason_phrase, "subscribe_namespace error");
				return Err(err);
			}
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				let err = request::from_code(msg.error_code, request::Kind::SubscribeNamespace, self.version);
				tracing::warn!(%err, reason = %msg.reason_phrase, "subscribe_namespace error");
				return Err(err);
			}
			_ => return Err(Error::UnexpectedMessage),
		}

		tracing::debug!(%prefix, "subscribe_namespace ok");

		// The extension changes the NAMESPACE encoding, so we can't parse one until
		// the peer's SETUP says whether it negotiated.
		let peer = self.peer().await;

		// Suffixes live on this stream, so a repeat is recognized as an update to the
		// advertisement rather than a second one (which would leak the refcount).
		let mut live: std::collections::HashSet<PathOwned> = std::collections::HashSet::new();

		// The stream owns every advertisement it carried, so release them however it
		// ends: a clean close, a decode error, or the peer resetting it. Without this
		// each namespace keeps its refcount and the source never detaches.
		//
		// This is what moq-lite already does, where the equivalent map is a local whose
		// guards drop.
		let res = self.run_namespace_entries(&mut stream, &prefix, &peer, &mut live).await;
		for path in live {
			let _ = self.stop_announce(path);
		}
		res
	}

	/// Read NAMESPACE / NAMESPACE_DONE entries until the stream closes.
	///
	/// `live` tracks the suffixes this stream has advertised, so a repeat is recognized
	/// as an update rather than a second advertisement, and the caller can release
	/// whatever is still held when the stream ends.
	async fn run_namespace_entries<T: crate::transport::poll::Session>(
		&mut self,
		stream: &mut Stream<T, Version>,
		prefix: &PathOwned,
		peer: &cluster::Peer,
		live: &mut std::collections::HashSet<PathOwned>,
	) -> Result<(), Error> {
		loop {
			let type_id: u64 = match stream.reader.decode_maybe().await? {
				Some(id) => id,
				None => break, // Stream closed
			};
			let size: u16 = stream.reader.decode().await?;
			let mut data = stream.reader.read_exact(size as usize).await?;

			match type_id {
				// The suffix is relative to the prefix we subscribed, which is itself
				// relative to our root -- so the join is too, which is what everything
				// below wants (`create_broadcast` joins the root itself).
				ietf::Namespace::ID => {
					let msg = ietf::Namespace::decode_body(&mut data, self.version, peer.negotiated())?;
					if !data.is_empty() {
						return Err(Error::WrongSize);
					}
					let path = prefix.join(&msg.suffix);
					let Some(advert) = self.route(msg.cluster.as_ref(), peer) else {
						// Looped back through us: forwarding it would extend the loop and
						// subscribing through it would route us back to ourselves.
						//
						// An update replaces the advertisement it repeats, so a reflected
						// replacement retracts the route we were holding. Keeping it would
						// leave subscriptions on a path the peer no longer offers.
						tracing::debug!(%path, "dropping reflected namespace");
						if live.remove(&path) {
							let _ = self.stop_announce(path);
						}
						continue;
					};

					tracing::debug!(%path, hops = advert.route.hops.len(), cost = ?advert.route.cost, "namespace");
					if live.contains(&path) {
						// A repeat replaces the advertisement atomically; nothing is torn
						// down merely because an update arrived.
						self.update_announce(path, advert)?;
					} else {
						match self.start_announce(path.clone(), advert) {
							Ok(()) => {
								live.insert(path);
							}
							// The interest names a pattern's literal head, so the peer
							// legitimately advertises namespaces beneath it that the
							// scope excludes; those are filtered here, not fatal.
							Err(Error::Unauthorized) => {
								tracing::debug!(%path, "namespace outside the subscribe scope; ignoring");
							}
							Err(err) => return Err(err),
						}
					}
				}
				ietf::NamespaceDone::ID => {
					let msg = ietf::NamespaceDone::decode_msg(&mut data, self.version)?;
					let path = prefix.join(&msg.suffix);
					tracing::debug!(%path, "namespace_done");
					if live.remove(&path) {
						let _ = self.stop_announce(path);
					}
				}
				_ => {
					tracing::warn!(type_id, "unexpected message on subscribe_namespace stream");
					return Err(Error::UnexpectedMessage);
				}
			}
		}

		Ok(())
	}

	/// Handle an incoming bidi stream dispatched by the session.
	///
	/// `peer` and `declared` are what the peer declared in its SETUP, which the dispatcher
	/// awaited once before accepting streams: PUBLISH_NAMESPACE cannot be parsed without
	/// knowing whether the MoQ Cluster extension is on, and `declared` says whether an
	/// unsolicited one is a bug (MoQ Solicit).
	pub fn handle_stream(
		&mut self,
		id: u64,
		mut data: bytes::Bytes,
		stream: Stream<S, Version>,
		peer: cluster::Peer,
		declared: Option<bool>,
	) -> Result<MaybeSendBox<'static, ()>, Error> {
		let mut this = self.clone();
		let task = match id {
			ietf::Publish::ID => {
				let msg = ietf::Publish::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received publish");
				async move {
					if let Err(err) = this.run_publish_stream(stream, msg).await {
						tracing::debug!(%err, "publish stream error");
					}
				}
				.maybe_boxed()
			}
			ietf::PublishNamespace::ID => {
				// A negotiated session that omits HOP_PATH fails the decode here, which
				// the dispatcher turns into the protocol violation the draft requires.
				let msg = ietf::PublishNamespace::decode_body(&mut data, this.version, peer.negotiated())?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received publish_namespace");
				async move {
					if let Err(err) = this.run_publish_namespace_stream(stream, msg, peer, declared).await {
						// An advertisement update is decoded here rather than in the
						// dispatcher, so nothing else would surface a malformed one. The
						// cluster draft requires closing the session on those; a stream
						// the peer simply reset is not the peer's fault.
						if is_protocol_violation(&err) {
							this.session
								.close(SessionError::from(&err).to_code(), err.to_string().as_ref());
						}
						tracing::debug!(%err, "publish_namespace stream error");
					}
				}
				.maybe_boxed()
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type for subscriber");
				return Err(Error::UnexpectedStream);
			}
		};
		Ok(task)
	}

	/// What the peer declared about being solicited (MoQ Solicit).
	///
	/// Read once by the dispatch loop and handed to each stream rather than awaited per
	/// stream: the slot is settled by the time streams are accepted, and a stream task
	/// that waited on it would park forever if it never were.
	pub(super) async fn solicit(&self) -> Option<bool> {
		self.peer_setup.get().await.solicit
	}

	/// Whether an incoming PUBLISH_NAMESPACE means the peer ignored our SETUP.
	///
	/// We always declare that advertisements to us must be solicited (MoQ Solicit), and a
	/// peer that wrote the option at all proves it implements the extension, whichever
	/// value it chose. It also cannot have advertised before reading our SETUP, since our
	/// SETUP is what says whether advertising unasked is allowed. So this is a bug in the
	/// peer, and a silent one on both sides if we tolerate it.
	///
	/// Draft-14/15 are exempt: they have no inline NAMESPACE, so a PUBLISH_NAMESPACE
	/// request is also how a peer answers our SUBSCRIBE_NAMESPACE there, and the message
	/// alone does not say which it is.
	fn unsolicited_is_a_violation(&self, declared: Option<bool>) -> bool {
		// We only hold a peer to a requirement we actually stated. A side that did not offer MoQ
		// Solicit invited unsolicited advertisements, so one is
		// expected even from a solicit-aware peer.
		if !self.declared_solicit {
			return false;
		}
		match self.version {
			Version::Draft14 | Version::Draft15 => false,
			_ => declared.is_some(),
		}
	}

	/// Handle an incoming PUBLISH_NAMESPACE on its bidi stream.
	async fn run_publish_namespace_stream(
		&mut self,
		mut stream: Stream<S, Version>,
		msg: ietf::PublishNamespace<'_>,
		peer: cluster::Peer,
		declared: Option<bool>,
	) -> Result<(), Error> {
		let request_id = msg.request_id;
		let path = msg.track_namespace.to_owned();

		if self.unsolicited_is_a_violation(declared) {
			tracing::warn!(%path, "unsolicited publish_namespace from a peer that implements MoQ Solicit");
			return Err(Error::ProtocolViolation);
		}

		// A path that already contains our own Hop ID looped back. Reject it rather
		// than attaching a source we would then have to route around.
		let Some(advert) = self.route(msg.cluster.as_ref(), &peer) else {
			tracing::debug!(%path, "dropping reflected publish_namespace");
			self.write_error(
				&mut stream,
				request_id,
				&Error::Unroutable,
				"route loops through this relay",
			)
			.await?;
			let _ = stream.writer.close().await;
			return Ok(());
		};

		// A request token on the PUBLISH_NAMESPACE authorizes the announce when the session
		// grant does not already cover it (MoQ request-token, draft-17 section 9.3.2 /
		// draft-18+ section 10.2.2). Purely additive: with no token, or a grant that covers
		// the path, everything below is unchanged and the origin model decides scope as it
		// did before.
		let mut token_grant = None;
		if let Some(token) = &msg.authorization_token
			&& !self.auth.covers(crate::auth::Direction::Subscribe, path.as_str())
		{
			// An alias reference (DELETE/USE_ALIAS) is a connection-level protocol violation
			// and closes the session exactly as on the SETUP path; a merely-undecodable
			// structure is refused per request without tearing down the connection.
			let structure = match crate::ietf::token::decode_value(token, self.version) {
				Ok(structure) => structure,
				Err(err @ Error::ProtocolViolation) => {
					self.session
						.close(crate::SessionError::ProtocolViolation.to_code(), &err.to_string());
					return Err(err);
				}
				Err(_) => {
					self.write_error(
						&mut stream,
						request_id,
						&Error::Unauthorized,
						"malformed authorization token",
					)
					.await?;
					let _ = stream.writer.close().await;
					return Ok(());
				}
			};
			let verdict = self.auth.verify_request(
				bytes::Bytes::from(structure.value),
				structure.kind,
				path.clone(),
				crate::auth::RequestKind::PublishNamespace,
			);
			match verdict.grant().await {
				// The token's grant must cover this announce; it authorizes nothing else and
				// never joins the session union.
				Ok(grant) if crate::auth::RequestKind::PublishNamespace.covers(&grant, path.as_str()) => {
					token_grant = Some(crate::auth::RequestGrant::new(
						&self.runtime,
						verdict,
						grant,
						path.clone(),
						crate::auth::RequestKind::PublishNamespace,
					));
				}
				Ok(_) => {
					self.write_error(
						&mut stream,
						request_id,
						&Error::Unauthorized,
						"token does not cover this request",
					)
					.await?;
					let _ = stream.writer.close().await;
					return Ok(());
				}
				// UNAUTHORIZED for a refusal, NOT_SUPPORTED when no consumer verifies tokens.
				Err(err) => {
					self.write_error(&mut stream, request_id, &err, &err.to_string())
						.await?;
					let _ = stream.writer.close().await;
					return Ok(());
				}
			}
		}

		match self.start_announce(path.clone(), advert) {
			Ok(_) => {
				if let Err(err) = self.write_ok(&mut stream, request_id).await {
					// Local rollback, not a peer unannounce: don't count announce bytes.
					let _ = self.stop_announce(path);
					return Err(err);
				}
			}
			Err(err) => {
				self.write_error(&mut stream, request_id, &err, &err.to_string())
					.await?;
				let _ = stream.writer.close().await;
				return Ok(());
			}
		}

		// An endpoint updates an advertisement with REQUEST_UPDATE on the stream that
		// already carries it, so keep reading until the stream ends: a close on
		// draft-17+, or v14-16's PublishNamespaceDone (see `terminal_publish_namespace`).
		//
		// `attached` survives the call so a stream that detached mid-flight (a reflected
		// update) is not released twice here.
		let mut attached = true;
		let res = self
			.run_publish_namespace_updates(&mut stream, &path, msg.cluster, peer, &mut attached, token_grant)
			.await;

		if attached {
			self.stop_announce(path)?;
		}

		res
	}

	/// Whether `type_id` retracts a PUBLISH_NAMESPACE rather than updating it.
	///
	/// v14-16 carry the stream over the control stream, and the adapter delivers the
	/// terminal message *before* it FINs (`Route::CloseStream`), so the withdrawal
	/// arrives here as a message and only then as a close. Draft-17+ has a real stream,
	/// where the close alone retracts and a terminal message on it is a violation.
	///
	/// Only PUBLISH_NAMESPACE_DONE: the publisher sends that one. PUBLISH_NAMESPACE_CANCEL
	/// travels the other way, so receiving it on an advertisement *we* were offered is a
	/// violation, not a withdrawal.
	fn terminal_publish_namespace(&self, type_id: u64) -> bool {
		terminal_publish_namespace(self.version, type_id)
	}

	/// Read advertisement updates off a live PUBLISH_NAMESPACE stream until it closes.
	///
	/// `held` is what the peer advertised, kept current because a REQUEST_UPDATE carries
	/// only what changed. Each one is answered with REQUEST_OK, or REQUEST_ERROR and a
	/// closed stream when it cannot be applied, which withdraws the advertisement
	/// (moq-transport Section 9.5.1).
	async fn run_publish_namespace_updates(
		&mut self,
		stream: &mut Stream<S, Version>,
		path: &PathOwned,
		mut held: Option<cluster::Advert>,
		peer: cluster::Peer,
		attached: &mut bool,
		mut token_grant: Option<crate::auth::RequestGrant<crate::time::Clock>>,
	) -> Result<(), Error> {
		let mut pending: Option<(crate::auth::RequestVerdict, RequestId)> = None;
		// Updates that arrived while a renewal was pending, handled in order once each verdict
		// resolves. A FIFO queue, not a single slot: draft-18 section 10.9.1 permits coalescing
		// the cumulative deltas but still requires an answer per update, so an earlier buffered
		// update must not be dropped by a later one. The queue is bounded by MAX_REQUEST_UPDATES,
		// counting the one being verified, so a peer cannot grow it without limit behind a slow
		// verdict.
		let mut stashed: std::collections::VecDeque<(u64, bytes::Bytes)> = std::collections::VecDeque::new();
		// A withdrawal read while a renewal was pending, handled ahead of any queued update so
		// a buffered update never masks it: nothing more is owed once the peer retracts.
		let mut terminal_msg: Option<(u64, bytes::Bytes)> = None;
		loop {
			// A renewal verify in flight is raced against the request grant's deadline (never
			// a bare await), so the old deadline can still fire while a slow acceptor decides,
			// and against the stream, so a peer ending the announce ends it whatever the
			// acceptor does. A message that arrives meanwhile waits for the verdict.
			if let Some((verdict, rid)) = pending.as_mut() {
				let rid = *rid;
				enum Ren {
					Renewal(Result<crate::auth::Grant, Error>),
					Ended(Error),
					Closed(Result<(), Error>),
					// The peer withdrew the announce: the renewal no longer matters.
					Withdrawn,
					// A non-terminal update read while the verdict is pending: buffered to handle
					// once it resolves, so the read keeps watching for a terminal meanwhile.
					Buffered((u64, bytes::Bytes)),
				}
				let version = self.version;
				let ren = {
					let mut read = std::pin::pin!(super::publisher::read_control(&mut stream.reader));
					kio::wait(|waiter| {
						if let Some(rg) = token_grant.as_mut()
							&& let Poll::Ready(err) = rg.poll_ended(waiter)
						{
							return Poll::Ready(Ren::Ended(err));
						}
						if let Poll::Ready(res) = verdict.poll_grant(waiter) {
							return Poll::Ready(Ren::Renewal(res));
						}
						// Keep reading while the verdict is pending so a withdrawal still ends the
						// announce: a buffered update must not mask a later terminal. A terminal is
						// stashed and reported now; a non-terminal update is buffered and the wait
						// broken, so the next read starts fresh and keeps watching.
						match waiter.poll_future(read.as_mut()) {
							Poll::Ready(Ok(Some((id, data)))) if terminal_publish_namespace(version, id) => {
								terminal_msg = Some((id, data));
								Poll::Ready(Ren::Withdrawn)
							}
							Poll::Ready(Ok(Some(message))) => Poll::Ready(Ren::Buffered(message)),
							Poll::Ready(Ok(None)) => Poll::Ready(Ren::Closed(Ok(()))),
							Poll::Ready(Err(err)) => Poll::Ready(Ren::Closed(Err(err))),
							Poll::Pending => Poll::Pending,
						}
					})
					.await
				};
				let res = match ren {
					Ren::Ended(err) => return Err(err),
					Ren::Closed(res) => return res,
					Ren::Withdrawn => {
						pending = None;
						continue;
					}
					Ren::Buffered(message) => {
						// Queue the update to handle once the verdict resolves; the loop keeps
						// reading, so a terminal is never masked by it. Each buffered update is
						// kept, in order, so none loses its cluster delta or its answer. Only a
						// REQUEST_UPDATE counts toward the limit (anything else is caught as an
						// unexpected message when it drains); the one being verified plus the
						// queued updates are the outstanding REQUEST_UPDATEs, and what happens when
						// another would exceed the ceiling is version-appropriate.
						let is_update = message.0 == ietf::PublishNamespaceUpdate::ID;
						let outstanding = stashed
							.iter()
							.filter(|(id, _)| *id == ietf::PublishNamespaceUpdate::ID)
							.count() as u64 + 1;
						if request_update::supported(self.version) && is_update {
							// Draft-19+: we advertised MAX_REQUEST_UPDATES, so a peer with that many
							// already outstanding sending another broke the negotiated limit.
							// Draft-19 section 10.3.1.7 answers that with a session close,
							// TOO_MANY_REQUEST_UPDATES. Returning the error is not enough here: the
							// dispatcher closes only on is_protocol_violation, which excludes
							// Error::Session, so close explicitly as the publisher does. A conforming
							// peer self-limits and never reaches here.
							if outstanding >= request_update::MAX_REQUEST_UPDATES {
								self.session.clone().close(
									crate::SessionError::TooManyRequestUpdates.to_code(),
									"too many request updates",
								);
								return Err(Error::Session(crate::SessionError::TooManyRequestUpdates));
							}
						} else if stashed.len() >= request_update::UNNEGOTIATED_GUARD {
							// Drafts below 19 negotiate no limit, so a peer agreed to no ceiling:
							// this is a local memory guard, not a protocol fault. End this announce,
							// never the session: finish the stream and stop.
							if stream.writer.finish().is_ok() {
								let _ = stream.writer.closed().await;
							}
							return Ok(());
						}
						stashed.push_back(message);
						continue;
					}
					Ren::Renewal(res) => res,
				};
				let (verdict, _) = pending.take().expect("a pending renewal");
				let Some(rg) = token_grant.as_mut() else {
					continue;
				};
				match res {
					// On accept the old grant is dropped and the deadline re-armed (REQUEST_OK).
					Ok(grant) if rg.covers(&grant) => {
						rg.renew(verdict, grant);
						self.write_ok(stream, rid).await?;
					}
					// A refused or uncovered renewal keeps the old grant until it lapses and
					// answers UNAUTHORIZED without tearing down the announce.
					_ => {
						self.write_error(stream, rid, &Error::Unauthorized, "renewal not granted")
							.await?;
					}
				}
				continue;
			}

			// Read one control message, ending the announce (never the session) if the
			// request grant lapses or is revoked meanwhile. The read future borrows only the
			// reader, in an inner block, so that borrow is gone before a renewal answers on
			// the writer.
			enum Ctl {
				Message(u64, bytes::Bytes),
				Closed,
				Ended(Error),
			}
			let ctl = if let Some((id, data)) = terminal_msg.take() {
				// A withdrawal read while a renewal was pending ends the announce now, ahead of
				// any queued update: nothing more is owed once the peer retracts.
				Ctl::Message(id, data)
			} else if let Some((id, data)) = stashed.pop_front() {
				Ctl::Message(id, data)
			} else {
				let mut read = std::pin::pin!(super::publisher::read_control(&mut stream.reader));
				kio::wait(|waiter| -> Poll<Result<Ctl, Error>> {
					if let Some(rg) = token_grant.as_mut()
						&& let Poll::Ready(err) = rg.poll_ended(waiter)
					{
						return Poll::Ready(Ok(Ctl::Ended(err)));
					}
					match waiter.poll_future(read.as_mut()) {
						Poll::Ready(Ok(Some((id, data)))) => Poll::Ready(Ok(Ctl::Message(id, data))),
						Poll::Ready(Ok(None)) => Poll::Ready(Ok(Ctl::Closed)),
						Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
						Poll::Pending => Poll::Pending,
					}
				})
				.await?
			};
			let (type_id, mut data) = match ctl {
				Ctl::Message(type_id, data) => (type_id, data),
				Ctl::Closed => return Ok(()),
				Ctl::Ended(err) => return Err(err),
			};
			let terminal = self.terminal_publish_namespace(type_id);
			if type_id != ietf::PublishNamespaceUpdate::ID && !terminal {
				// A repeated PUBLISH_NAMESPACE lands here too: a second request on the
				// stream is the base draft's duplicate request ID.
				tracing::warn!(type_id, "unexpected message on publish_namespace stream");
				return Err(Error::UnexpectedMessage);
			}

			if terminal {
				ietf::PublishNamespaceDone::decode_msg(&mut data, self.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(%path, "publish_namespace_done");
				return Ok(());
			}

			let msg = ietf::PublishNamespaceUpdate::decode_msg(&mut data, self.version)?;
			// Junk inside the declared size would otherwise be applied silently, which
			// is the one decode path that skipped the check the others make.
			if !data.is_empty() {
				return Err(Error::WrongSize);
			}

			// Cluster parameters and a token renewal can ride the same update, so apply the
			// routing first, for every update that carries it, before dealing with the token.
			// A different original publisher applies in place too: the origin drains what the
			// old one already serves and never splices the two. The parameters exist only on a
			// session that negotiated the extension; anywhere else they are the peer's violation.
			let carries_cluster = msg.hops.is_some() || msg.cost.is_some();
			held = match &held {
				Some(current) => Some(msg.apply(current)),
				None if carries_cluster => {
					tracing::warn!(%path, "cluster parameters on a session that negotiated none");
					return Err(Error::ProtocolViolation);
				}
				None => None,
			};

			// Re-route only when the update actually changes the route (an omitted parameter
			// keeps its value, so a token-only update leaves it untouched). A path that now runs
			// through us is unusable, so detach rather than keep serving it; reading continues,
			// since this stream is the advertisement's only channel and a later clean path
			// arrives here or nowhere. Ending the stream is not ours to do: a peer MAY
			// legitimately send a path carrying our Hop ID when a redundant sibling shares it.
			let applied: Result<(), Error> = if carries_cluster {
				match self.route(held.as_ref(), &peer) {
					None => {
						if std::mem::take(attached) {
							tracing::debug!(%path, "publish_namespace now loops back; detaching");
							let _ = self.stop_announce(path.clone());
						}
						Ok(())
					}
					Some(advert) => {
						tracing::debug!(%path, hops = advert.route.hops.len(), cost = ?advert.route.cost, "publish_namespace update");
						match *attached {
							true => self.update_announce(path.clone(), advert),
							// Re-attach: a clean path replaced the reflected one we detached from.
							false => self.start_announce(path.clone(), advert).map(|()| *attached = true),
						}
					}
				}
			} else {
				Ok(())
			};

			// An unroutable apply withdraws the announce whether or not a token also rides it.
			if let Err(err) = &applied {
				tracing::warn!(%path, %err, "publish_namespace update refused");
				self.write_error(stream, msg.request_id, err, &err.to_string()).await?;
				// The close is the withdrawal; the caller releases what was attached.
				if stream.writer.finish().is_ok() {
					let _ = stream.writer.closed().await;
				}
				return Ok(());
			}

			// A REQUEST_UPDATE carrying a fresh token refreshes the announce's request grant
			// (MoQ request-token), when the announce is token-authorized. The verify is not
			// awaited here: it becomes the pending renewal raced against the deadline above.
			// The routing above is already applied, so the renewal's answer (written when its
			// verdict resolves) is this update's single response, cluster parameters included.
			if let Some(token) = &msg.authorization_token
				&& token_grant.is_some()
			{
				// An alias reference (DELETE/USE_ALIAS) is a connection-level protocol violation
				// and closes the session, as on the SETUP path; a merely-undecodable structure
				// is refused per request, leaving the old grant to stand until it lapses.
				let structure = match crate::ietf::token::decode_value(token, self.version) {
					Ok(structure) => structure,
					Err(err @ Error::ProtocolViolation) => {
						self.session
							.close(crate::SessionError::ProtocolViolation.to_code(), &err.to_string());
						return Err(err);
					}
					Err(_) => {
						self.write_error(
							stream,
							msg.request_id,
							&Error::Unauthorized,
							"malformed authorization token",
						)
						.await?;
						continue;
					}
				};
				let verdict = self.auth.verify_request(
					bytes::Bytes::from(structure.value),
					structure.kind,
					path.clone(),
					crate::auth::RequestKind::PublishNamespace,
				);
				pending = Some((verdict, msg.request_id));
				continue;
			}

			// No token rides this update: acknowledge it now.
			self.write_ok(stream, msg.request_id).await?;
		}
	}

	/// Reject an incoming PUBLISH.
	///
	/// PUBLISH offers a single track, so honoring it means routing per
	/// (namespace, track). Our model routes per namespace: a source attaches at a
	/// path and serves every track under it, resolved on demand via SUBSCRIBE.
	/// Accepting a PUBLISH would mean inventing a namespace-level source out of a
	/// track-level offer, and that fiction then contradicts any real
	/// PUBLISH_NAMESPACE for the same path.
	///
	/// Declining the request rather than failing the session, since a peer using
	/// a feature we don't implement is not a protocol violation.
	async fn run_publish_stream(
		&mut self,
		mut stream: Stream<S, Version>,
		msg: ietf::Publish<'_>,
	) -> Result<(), Error> {
		tracing::debug!(broadcast = %msg.track_namespace, track = %msg.track_name, "rejecting publish");

		// We decline the method itself rather than this particular track, which would be
		// UNINTERESTED.
		//
		// The alias the message carries is deliberately not recorded. Nothing will ever bind
		// it, and a rejected request has no lifetime of ours to hang the cleanup on, so the
		// entry would have to be swept asynchronously. Any data streams the publisher opened
		// before reading this are dropped by the unknown-alias path instead.
		self.write_publish_error(
			&mut stream,
			msg.request_id,
			&Error::Unsupported,
			"PUBLISH is not supported",
		)
		.await?;
		// The rejection is the whole exchange, but it still has to arrive: a finish alone
		// leaves the drop-time reset free to discard it before the peer acknowledges it.
		let _ = stream.writer.close().await;

		Ok(())
	}

	/// Send OK on the bidi stream.
	async fn write_ok(&self, stream: &mut Stream<S, Version>, request_id: RequestId) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				stream.writer.encode(&ietf::PublishNamespaceOk::ID).await?;
				stream.writer.encode(&ietf::PublishNamespaceOk { request_id }).await?;
			}
			Version::Draft15 | Version::Draft16 => {
				stream.writer.encode(&ietf::RequestOk::ID).await?;
				stream
					.writer
					.encode(&ietf::RequestOk {
						request_id: Some(request_id),
					})
					.await?;
			}
			_ => {
				stream.writer.encode(&ietf::RequestOk::ID).await?;
				stream.writer.encode(&ietf::RequestOk { request_id: None }).await?;
			}
		}
		Ok(())
	}

	/// Refuse a PUBLISH_NAMESPACE on the bidi stream that carries it.
	async fn write_error(
		&self,
		stream: &mut Stream<S, Version>,
		request_id: RequestId,
		err: &Error,
		reason: &str,
	) -> Result<(), Error> {
		let error_code = request::to_code(err, request::Kind::PublishNamespace, self.version);

		match self.version {
			Version::Draft14 => {
				stream.writer.encode(&ietf::PublishNamespaceError::ID).await?;
				stream
					.writer
					.encode(&ietf::PublishNamespaceError {
						request_id,
						error_code,
						reason_phrase: reason.into(),
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				stream.writer.encode(&ietf::RequestError::ID).await?;
				stream
					.writer
					.encode(&ietf::RequestError {
						request_id: Some(request_id),
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
			_ => {
				stream.writer.encode(&ietf::RequestError::ID).await?;
				stream
					.writer
					.encode(&ietf::RequestError {
						request_id: None,
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
		}
		Ok(())
	}

	/// Refuse a PUBLISH on the bidi stream that carries it.
	async fn write_publish_error(
		&self,
		stream: &mut Stream<S, Version>,
		request_id: RequestId,
		err: &Error,
		reason: &str,
	) -> Result<(), Error> {
		let error_code = request::to_code(err, request::Kind::Publish, self.version);

		match self.version {
			Version::Draft14 => {
				stream.writer.encode(&ietf::PublishError::ID).await?;
				stream
					.writer
					.encode(&ietf::PublishError {
						request_id,
						error_code,
						reason_phrase: reason.into(),
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				stream.writer.encode(&ietf::RequestError::ID).await?;
				stream
					.writer
					.encode(&ietf::RequestError {
						request_id: Some(request_id),
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
			_ => {
				stream.writer.encode(&ietf::RequestError::ID).await?;
				stream
					.writer
					.encode(&ietf::RequestError {
						request_id: None,
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
		}
		Ok(())
	}

	/// Attach the route for one newly advertised namespace, bumping its refcount.
	///
	/// Pair with [`Self::stop_announce`].
	fn start_announce(&mut self, path: PathOwned, advert: Advertised) -> Result<(), Error> {
		let mut state = self.state.lock();
		let existing = state.broadcasts.contains_key(&path);
		self.attach(&mut state, path.clone(), advert)?;
		if existing && let Some(entry) = state.broadcasts.get_mut(&path) {
			// The path was already attached, so this is one more advertisement for
			// it; only a freshly created entry starts at one and skips this.
			entry.count += 1;
		}
		Ok(())
	}

	/// Apply a changed advertisement to a namespace that is already attached.
	///
	/// An update replaces the advertisement atomically: the refcount does not move,
	/// and no subscription is torn down merely because one arrived.
	fn update_announce(&mut self, path: PathOwned, advert: Advertised) -> Result<(), Error> {
		let mut state = self.state.lock();
		if !state.broadcasts.contains_key(&path) {
			return Err(Error::NotFound);
		}
		self.attach(&mut state, path, advert)?;
		Ok(())
	}

	/// Create or update the announced route for one namespace, leaving the
	/// refcount to the caller.
	///
	/// This is the semantic heart of the mapping: a moq-transport namespace IS a
	/// prefix route, so a PUBLISH_NAMESPACE advertises the whole prefix and paths
	/// beneath it materialize on demand.
	fn attach(&self, state: &mut State, path: PathOwned, advert: Advertised) -> Result<(), Error> {
		let Advertised { mut route } = advert;

		// A namespace published after the peer's GOAWAY starts out draining, so
		// a late arrival on a dying connection can't take over as primary.
		if self.going_away.is_set() {
			route.cost = crate::origin::Cost::DRAIN;
		}

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(entry) => {
				// A repeat is a repricing: update the route in place. In-flight
				// tracks keep flowing.
				let entry = entry.into_mut();
				entry.route = route.clone();
				if let Some(dynamic) = &entry.dynamic {
					dynamic.update(route)?;
				}
				Ok(())
			}
			Entry::Vacant(entry) => {
				// Outside the session's limit: held, not refused, so a wider limit can
				// attach it while the peer still advertises it.
				if !self.auth.within_limit(crate::auth::Direction::Subscribe, path.as_str()) {
					tracing::debug!(route = %self.origin.absolute(&path), "withholding announce outside the limit");
					entry.insert(BroadcastState {
						route,
						dynamic: None,
						generation: 0,
						count: 1,
						sources: HashMap::new(),
					});
					return Ok(());
				}
				// Propagates Error::Unauthorized if the namespace is out of scope.
				let dynamic = self.origin.dynamic(&path, route.clone())?;

				entry.insert(BroadcastState {
					route,
					dynamic: Some(dynamic),
					generation: 0,
					count: 1,
					sources: HashMap::new(),
				});

				tracing::debug!(route = %self.origin.absolute(&path), "announce");
				self.serve_route(path, 0);

				Ok(())
			}
		}
	}

	/// Release one advertisement of `path`, closing its sources when it was the last.
	fn stop_announce(&mut self, path: PathOwned) -> Result<(), Error> {
		let mut state = self.state.lock();

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count -= 1;
				if entry.get().count == 0 {
					tracing::debug!(route = %self.origin.absolute(&path), "unannounced");
					// Dropping the entry retracts the route (its announcement drops) and
					// closes its sources (their guards drop).
					entry.remove();
				}
			}
			Entry::Vacant(_) => return Err(Error::NotFound),
		};

		Ok(())
	}

	/// Run `tasks` to their own end, or until the session dies.
	///
	/// A retraction does not disturb subscriptions already in flight: what ended
	/// takes no new work, but the work it started finishes.
	async fn drain(&self, tasks: &mut TaskSet) {
		let mut session = self.session.clone();
		kio::wait(|waiter| {
			let mut cx = std::task::Context::from_waker(waiter.waker());
			if session.poll_closed(&mut cx).is_ready() {
				return Poll::Ready(());
			}
			tasks.poll(waiter)
		})
		.await
	}

	/// Serve materialization requests for one announced namespace: mint a source
	/// per requested path and serve its track requests until the route is
	/// retracted or the session dies. Tracks in flight at a retraction run to
	/// their own end.
	async fn run_route(&self, path: PathOwned, generation: u64) {
		let mut broadcasts = TaskSet::owned();
		let mut closed_session = self.session.clone();
		let mut epoch = 0;
		loop {
			let next = broadcasts
				.drive(|waiter| {
					let mut cx = std::task::Context::from_waker(waiter.waker());
					if closed_session.poll_closed(&mut cx).is_ready() {
						return Poll::Ready(None);
					}
					// A draining peer usually stops publishing namespaces, so react
					// to the GOAWAY itself; waiting for another message would leave
					// the route primary until the session finally closed.
					// Idempotent, since the signal stays set.
					if self.going_away.poll(waiter).is_ready() {
						self.drain_route(&path);
					}
					// A limit that no longer covers the namespace holds the route back as a
					// retraction would, closing its sources. Tracks in flight end on their
					// own gates, with `Unauthorized`.
					let mut excluded = false;
					while let Poll::Ready(permit) =
						self.auth
							.poll_permit(crate::auth::Direction::Subscribe, &mut epoch, waiter)
					{
						excluded = !permit.within_limit(path.as_str());
					}
					// The route lives in the entry: stop_announce removing it retracts
					// the route, and this loop ends with it.
					let mut state = self.state.lock();
					let Some(entry) = state.broadcasts.get_mut(&path) else {
						return Poll::Ready(None);
					};
					if entry.generation != generation {
						return Poll::Ready(None);
					}
					if excluded && entry.dynamic.is_some() {
						tracing::info!(route = %self.origin.absolute(&path), "namespace no longer authorized");
						entry.dynamic = None;
						entry.sources.clear();
					}
					match &mut entry.dynamic {
						Some(dynamic) => dynamic.poll_requested_broadcast(waiter).map(Some),
						None => Poll::Ready(None),
					}
				})
				.await;

			let request = match next {
				Some(Ok(request)) => request,
				// Retracted or torn down: no request will ever arrive again, but
				// the broadcasts already served keep their tracks in flight.
				Some(Err(_)) | None => {
					self.drain(&mut broadcasts).await;
					break;
				}
			};

			// The request path is absolute; the wire (and our origin handle) speak
			// paths relative to the session's root.
			let requested = match request.path().strip_prefix(self.origin.root()) {
				Some(requested) => requested.to_owned(),
				None => continue,
			};
			let source = self.origin.create_source(&requested);
			let dynamic = source.dynamic();
			request.accept(&source);

			// Retain the source so a retraction can close it. If the route was
			// retracted since the accept, close it here as that retraction would
			// have, and still serve what it took on: tracks subscribed since carry on.
			let guard = crate::model::broadcast::SourceGuard::new(source);
			let retracted = {
				let mut state = self.state.lock();
				match state.broadcasts.get_mut(&path) {
					Some(entry) => {
						entry.sources.insert(requested.clone(), guard);
						None
					}
					None => Some(guard),
				}
			};
			drop(retracted);

			let this = self.clone();
			broadcasts.push(async move {
				if let Err(err) = this.run_broadcast(requested.borrow(), dynamic).await {
					tracing::debug!(%err, "error running broadcast");
				}
			});
		}
	}

	/// Re-price one attached route to a draining cost (the peer sent a GOAWAY):
	/// every other candidate outranks it while it stays selectable as the last
	/// path. Idempotent, since the signal stays set.
	fn drain_route(&self, path: &PathOwned) {
		let mut state = self.state.lock();
		let Some(entry) = state.broadcasts.get_mut(path) else {
			return;
		};
		if entry.route.cost == crate::origin::Cost::DRAIN {
			return;
		}
		entry.route.cost = crate::origin::Cost::DRAIN;
		if let Some(dynamic) = &entry.dynamic {
			let _ = dynamic.update(entry.route.clone());
		}
	}

	async fn run_broadcast(&self, path: Path<'_>, mut broadcast: broadcast::Dynamic) -> Result<(), Error> {
		let mut subscribes = TaskSet::owned();
		let mut closed_session = self.session.clone();
		loop {
			let next = subscribes
				.drive(|waiter| {
					let mut cx = std::task::Context::from_waker(waiter.waker());
					if closed_session.poll_closed(&mut cx).is_ready() {
						return Poll::Ready(None);
					}
					broadcast.poll_requested_track(waiter).map(Some)
				})
				.await;

			let request = match next {
				Some(Ok(request)) => request,
				Some(Err(err)) => {
					tracing::debug!(%err, "broadcast closed");
					// No new tracks, but those in flight run to their own end.
					self.drain(&mut subscribes).await;
					break;
				}
				// Session gone.
				None => break,
			};

			let mut this = self.clone();

			let path = path.to_owned();
			let broadcast = broadcast.clone();
			subscribes.push(async move {
				this.run_subscribe(path, broadcast, request).await;
			});
		}

		Ok(())
	}

	async fn run_subscribe(
		&mut self,
		broadcast_path: Path<'_>,
		// Held for the subscription's lifetime but never watched: the broadcast ending
		// is a retraction, which does not disturb a subscription already in flight.
		_broadcast: broadcast::Dynamic,
		request: track::Request,
	) {
		// Data streams wait on the alias bound by SUBSCRIBE_OK, so leave the model request
		// pending until its immutable track metadata is known.
		if self.going_away.is_set() {
			request.reject(Error::GoingAway);
			return;
		}

		// A SUBSCRIBE always carries the token, so a token-bearing client does not self-censor
		// on its connection grant: the token authorizes what that grant does not cover, and the
		// server's covers-gate is the authority. The token stands in for the grant only, never
		// for the limit this side set on the peer, which still filters and, narrowing, revokes.
		let token_authorized = self.request_token.peek().is_some();
		let allowed = match token_authorized {
			true => self
				.auth
				.within_limit(crate::auth::Direction::Subscribe, broadcast_path.as_str()),
			false => self
				.auth
				.allows(crate::auth::Direction::Subscribe, broadcast_path.as_str()),
		};
		if !allowed {
			request.reject(Error::Unauthorized);
			return;
		}
		let gate_for = match token_authorized {
			true => crate::auth::Gate::limit,
			false => crate::auth::Gate::new,
		};
		let mut gate = gate_for(
			self.auth.clone(),
			broadcast_path.to_owned(),
			crate::auth::Direction::Subscribe,
		);

		let subscription = request.subscription();
		// The wire priority this subscription was opened at, re-sent unchanged on a
		// request-token renewal so the update carries the token without disturbing anything.
		let subscriber_priority = super::priority::to_wire(subscription.as_ref().map(|s| s.priority).unwrap_or(0));
		// A live join delivers nothing below the group SUBSCRIBE_OK names as Largest.
		let live = subscription.as_ref().and_then(|s| s.start).is_none();
		let join = match subscribe_join(
			subscription.as_ref().and_then(|s| s.start),
			subscription.as_ref().and_then(|s| s.end),
			self.version,
		) {
			Ok(join) => join,
			Err(err) => {
				request.reject(err);
				return;
			}
		};

		let request_id = match self.control.next_request_id(&self.runtime).await {
			Ok(id) => id,
			Err(err) => {
				request.reject(err);
				return;
			}
		};

		let mut stream = match Stream::open(&mut self.session.clone(), self.version).await {
			Ok(s) => s,
			Err(err) => {
				tracing::debug!(%err, "failed to open subscribe stream");
				request.reject(err);
				return;
			}
		};

		// Register the request before writing SUBSCRIBE so SUBSCRIBE_OK can bind its alias,
		// and so a fill fetch stream that overtakes it finds the subscription it answers.
		let joining = join.fetch;
		let fill = kio::Producer::new(match join.fill.is_some() || join.fetch.is_some() {
			true => Fill::Requested,
			false => Fill::Done,
		});
		{
			let mut state = self.state.lock();
			state.subscribes.insert(
				request_id,
				TrackState::pending(request.name().to_owned(), broadcast_path.to_owned(), fill, joining),
			);
		}

		// Write Subscribe message
		if let Err(err) = self
			.write_subscribe(&mut stream, request_id, &broadcast_path, &request, join)
			.await
		{
			tracing::debug!(%err, "failed to write subscribe");
			self.remove_subscribe(request_id);
			request.reject(err);
			return;
		}

		tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %request.name(), "subscribe started");

		// Park the origin request where a session abort can reject it. The producer
		// does not exist until SUBSCRIBE_OK, and dropping this task would otherwise
		// end the track as `Dropped`.
		let track_name = request.name().to_owned();
		{
			let mut state = self.state.lock();
			let Some(held) = state.subscribes.get_mut(&request_id) else {
				request.reject(Error::Cancel);
				return;
			};
			held.pending = Some(request);
		}

		// A publisher can be serving before its SUBSCRIBE_OK reaches us, since the data
		// streams are independent of the request stream. Waiting for the response alone would
		// miss the local side going away in that window and leave the publisher serving a
		// track nobody reads, which is the leak this whole path exists to close. The broadcast
		// ending is not a local side going away: a retraction does not disturb subscriptions
		// already in flight, and this one is.
		enum Setup {
			Response(Result<Option<Accepted>, Error>),
			Unused,
			/// `abort` already rejected the parked request.
			Gone,
		}

		let setup = {
			let mut response = std::pin::pin!(self.read_subscribe_response(&mut stream));
			loop {
				let setup = kio::wait(|waiter| {
					// An answer that has already arrived wins over the local terminals. Both can
					// be ready in one poll, and taking abandonment there would discard a response
					// the publisher has already sent: if it was a rejection, the request is gone
					// and cancelling it names a dead id back at a peer entitled to object.
					if let Poll::Ready(res) = waiter.poll_future(response.as_mut()) {
						return Poll::Ready(Setup::Response(res));
					}
					let mut state = self.state.lock();
					let Some(pending) = state
						.subscribes
						.get_mut(&request_id)
						.and_then(|held| held.pending.as_mut())
					else {
						return Poll::Ready(Setup::Gone);
					};
					if pending.poll_unused(waiter).is_ready() {
						return Poll::Ready(Setup::Unused);
					}
					Poll::Pending
				})
				.await;

				match setup {
					Setup::Response(res) => break Some(res),
					Setup::Gone => break None,
					Setup::Unused => {
						let mut state = self.state.lock();
						let Some(pending) = state
							.subscribes
							.get_mut(&request_id)
							.and_then(|held| held.pending.take())
						else {
							break None;
						};
						if pending.reject_unused(Error::Cancel) {
							break None;
						}
						if let Some(held) = state.subscribes.get_mut(&request_id) {
							held.pending = Some(pending);
						}
					}
				}
			}
		};

		let Some(response) = setup else {
			tracing::info!(
				broadcast = %self.origin.absolute(&broadcast_path),
				track = %track_name,
				"subscribe abandoned before it was accepted"
			);
			// The publisher may already be serving before it answers. A session abort
			// already rejected the parked request; dropping what remains is not a second one.
			self.remove_subscribe(request_id);
			self.cancel_subscribe(stream, request_id).await;
			return;
		};

		// SUBSCRIBE_OK commits the model's immutable metadata before the alias releases
		// any data stream that arrived ahead of this control response.
		let accepted = match response {
			Ok(Some(accepted)) => accepted,
			Ok(None) => {
				if let Some(pending) = self.take_pending(request_id) {
					pending.reject(Error::UnexpectedMessage);
				}
				self.remove_subscribe(request_id);
				return;
			}
			Err(err) => {
				tracing::debug!(%err, "subscribe response error");
				if let Some(pending) = self.take_pending(request_id) {
					pending.reject(err);
				}
				self.remove_subscribe(request_id);
				return;
			}
		};
		let Accepted {
			alias,
			timescale,
			priority,
			largest,
		} = accepted;
		// Where this subscription began: the object after the Largest Object SUBSCRIBE_OK
		// named, which every draft before 20 subscribes at. Draft-14's renewal restates it.
		let renewal_start = largest.map_or(ietf::Location { group: 0, object: 0 }, |largest| ietf::Location {
			group: largest.group,
			object: largest.object.saturating_add(1),
		});
		let info = track::Info::default()
			.with_timescale(Timescale::MICRO)
			.with_max_age(self.origin.default_max_age())
			.with_priority(super::priority::from_wire(priority.unwrap_or(128)));
		// Declared before the track is released to readers, so a warm cache waiting on
		// this copy judges itself against where the live feed actually starts.
		let Some(request) = self.take_pending(request_id) else {
			// Aborted while the answer was in hand. The parked request is already rejected.
			self.remove_subscribe(request_id);
			return;
		};
		let request = match live {
			true => request.resolving_start(),
			false => request,
		};
		let mut track = request.accept(info);
		if live {
			let _ = track.start_at(largest.map(|largest| largest.group));
		}
		let mut fetching: Option<MaybeSendBox<'static, ()>> = None;
		{
			let mut state = self.state.lock();
			if let Some(held) = state.subscribes.get_mut(&request_id) {
				held.producer = Some(track.clone());
				held.timescale = timescale;
				held.largest = largest;
				if let Ok(mut fill) = held.fill.write()
					&& matches!(*fill, Fill::Requested)
				{
					*fill = match largest {
						Some(_) => Fill::Serving(timescale),
						None => Fill::Done,
					};
				}
			}
		}
		if let Err(err) = self.register_alias(request_id, alias) {
			if matches!(err, Error::Duplicate) {
				tracing::warn!(track_alias = %alias, "publisher reused a live track alias for another track");
				self.session
					.close(SessionError::from(&err).to_code(), err.to_string().as_ref());
			} else {
				tracing::warn!(track_alias = %alias, %err, "could not bind track alias");
				self.cancel_subscribe(stream, request_id).await;
			}
			self.remove_subscribe(request_id);
			let _ = track.abort(err);
			return;
		}
		if let Some(joining) = joining
			&& largest.is_some()
		{
			fetching = self.start_joining_fetch(request_id, &track, joining).await;
		}

		// One event ends the subscription: the last consumer leaving, or the
		// publisher's PUBLISH_DONE. The broadcast ending does not: a retraction
		// does not disturb subscriptions already in flight.
		enum End {
			Unused,
			Revoked,
			/// The client replaced its request token: re-present it on this live subscription.
			Renew(Option<bytes::Bytes>),
			/// The publisher answered the renewal in flight (REQUEST_OK / REQUEST_ERROR), so
			/// a replacement coalesced behind it may now be sent.
			Answered,
			Done(Result<u64, Error>),
		}

		let mut fetch_done = fetching.is_none();
		// The request token last presented on this subscription (its initial value).
		let mut last_token = self.request_token.peek();
		// At most one renewal (SUBSCRIBE_UPDATE) is left unanswered at a time: a replacement
		// that arrives while one is in flight is coalesced into `last_token` and sent once
		// the outstanding one is answered, so the sender never outruns the receiver's
		// MAX_REQUEST_UPDATES credit (draft-19 section 10.3.1.7) whatever its value, without
		// reading it. `in_flight` is the token value awaiting an answer, `None` when nothing
		// is outstanding. Draft-14 answers no accepted renewal, so it cannot pace on answers
		// and re-presents each change directly; it also advertises no credit to exceed.
		let throttle = !matches!(self.version, Version::Draft14);
		let mut in_flight: Option<bytes::Bytes> = None;
		// Bumped by `read_publish_done` on each renewal answer; the loop releases the
		// coalesced replacement when it advances past `seen_answers`.
		let answers = kio::Shared::new(0u64);
		let mut seen_answers = 0u64;
		let cancelled = {
			let mut done = std::pin::pin!(Self::read_publish_done(&mut stream.reader, self.version, &answers));
			loop {
				let end = kio::wait(|waiter| {
					if !fetch_done
						&& let Some(fut) = fetching.as_mut()
						&& waiter.poll_future(fut.as_mut()).is_ready()
					{
						fetch_done = true;
					}
					if gate.poll_denied(waiter).is_ready() {
						return Poll::Ready(End::Revoked);
					}
					if track.poll_unused(waiter).is_ready() {
						return Poll::Ready(End::Unused);
					}
					if let Poll::Ready(token) = self.request_token.poll_changed(&last_token, waiter) {
						return Poll::Ready(End::Renew(token));
					}
					// Only wait on an answer while a renewal is actually outstanding.
					if throttle
						&& in_flight.is_some()
						&& let Poll::Ready(count) = answers.poll(waiter, |count| match **count != seen_answers {
							true => Poll::Ready(()),
							false => Poll::Pending,
						}) {
						seen_answers = *count;
						return Poll::Ready(End::Answered);
					}
					waiter.poll_future(done.as_mut()).map(End::Done)
				})
				.await;

				match end {
					End::Unused => match track.abort_unused(Error::Cancel) {
						Ok(()) => {
							tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track_name, "subscribe cancelled");
							break true;
						}
						Err(used) => track = used,
					},
					// A replaced token is re-presented as a token-only REQUEST_UPDATE; the read
					// future above consumes the answer. The subscribe stream's writer is a
					// disjoint borrow from its reader, so writing here does not disturb the read.
					End::Renew(token) => {
						last_token = token.clone();
						// Send only when no renewal is outstanding; otherwise coalesce, leaving the
						// newest in `last_token` to go out on End::Answered. A cleared token (`None`)
						// is not a renewal and sends nothing.
						let send_now = !throttle || in_flight.is_none();
						if send_now && let Some(token) = token {
							match self
								.send_request_token_update(
									&mut stream.writer,
									request_id,
									subscriber_priority,
									renewal_start,
									token.clone(),
								)
								.await
							{
								Ok(()) if throttle => {
									in_flight = Some(token);
									// Only an answer that arrives after this send releases the
									// coalesced follow-up, so a stray earlier answer cannot.
									seen_answers = *answers.lock();
								}
								Ok(()) => {}
								// A failed send does not end the subscription: it continues on the old
								// grant until that lapses, and the next change re-presents the token.
								Err(err) => tracing::debug!(%err, "failed to re-present the request token"),
							}
						}
					}
					End::Answered => {
						// The outstanding renewal was answered. If the credential changed while it
						// was in flight, present the newest now (a cleared token sends nothing);
						// otherwise the publisher already holds the latest.
						let was = in_flight.take();
						if last_token != was
							&& let Some(token) = last_token.clone()
						{
							match self
								.send_request_token_update(
									&mut stream.writer,
									request_id,
									subscriber_priority,
									renewal_start,
									token.clone(),
								)
								.await
							{
								Ok(()) => in_flight = Some(token),
								Err(err) => tracing::debug!(%err, "failed to re-present the request token"),
							}
						}
					}
					End::Revoked => {
						tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track_name, "subscription no longer authorized");
						let _ = track.abort(Error::Unauthorized);
						break true;
					}
					End::Done(res) => {
						match res {
							Ok(count) => {
								tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track_name, "subscribe complete");
								// The publisher sends PUBLISH_DONE once every data stream it opened
								// is closed, but QUIC does not order them, so some can still be on
								// their way. Wait until Stream Count of their headers arrived and
								// each is read to its end, or a bounded grace for any reset before
								// its header (the draft says to use a timeout). The count is a hint:
								// a published peer sends 0, which waits out the grace.
								let held = self
									.state
									.lock()
									.subscribes
									.get(&request_id)
									.map(|held| (held.tail.consume(), held.fill.clone()));
								if let Some((tail, fill)) = held {
									let mut settle = Settle::new(&self.runtime, tail);
									kio::wait(|waiter| {
										if !fetch_done
											&& let Some(fut) = fetching.as_mut()
											&& waiter.poll_future(fut.as_mut()).is_ready()
										{
											fetch_done = true;
										}
										poll_settled(&mut settle, waiter, &fill, count)
									})
									.await;
								}
								// A no-op once an END_OF_TRACK declared the end.
								let _ = track.finish();
							}
							Err(err) => {
								tracing::debug!(%err, "subscribe ended with error");
								let _ = track.abort(err);
							}
						}
						// The publisher already ended the request, so there is nothing to cancel.
						break false;
					}
				}
			}
		};

		// Clean up
		self.remove_subscribe(request_id);

		match cancelled {
			true => self.cancel_subscribe(stream, request_id).await,
			// The publisher already ended the request, so a FIN is all we owe it.
			false => {
				stream.writer.finish().ok();
			}
		}
	}

	/// Read the PUBLISH_DONE that ends an Established subscription, as the end it reports
	/// and, for a clean end, its Stream Count.
	///
	/// The publisher must send it before its FIN (draft-19 section 3.3.2), so a FIN
	/// without one is a failed request, not a clean end.
	///
	/// A request-token renewal we sent (SUBSCRIBE_UPDATE) is answered on this same stream
	/// (REQUEST_OK / REQUEST_ERROR on draft-15+, SUBSCRIBE_ERROR on draft-14; draft-14 is
	/// silent on an accepted renewal). Each answer bumps `answers` so the send loop can
	/// release the renewal it coalesced behind the one in flight; the read continues
	/// regardless, since a refused renewal leaves the old grant standing until it lapses,
	/// so the subscription ends then, with its PUBLISH_DONE, not on the answer.
	async fn read_publish_done(
		reader: &mut Reader<S::RecvStream, Version>,
		version: Version,
		answers: &kio::Shared<u64>,
	) -> Result<u64, Error> {
		loop {
			match reader.decode_maybe::<u64>().await? {
				Some(ietf::PublishDone::ID) => {
					let msg: ietf::PublishDone = reader.decode().await?;
					tracing::debug!(message = ?msg, "received publish done");
					msg.end(version)?;
					return Ok(msg.stream_count);
				}
				Some(ietf::RequestOk::ID) => {
					let msg: ietf::RequestOk = reader.decode().await?;
					tracing::debug!(message = ?msg, "request token renewal accepted");
					*answers.lock() += 1;
				}
				Some(ietf::RequestError::ID) => {
					// draft-17+ generalized SUBSCRIBE_ERROR into REQUEST_ERROR at the same id;
					// draft-14 still frames it as SUBSCRIBE_ERROR. Either way it refuses the
					// renewal, and the old grant stands until it lapses.
					match version {
						Version::Draft14 => {
							let msg: ietf::SubscribeError = reader.decode().await?;
							tracing::warn!(message = ?msg, "request token renewal refused");
						}
						_ => {
							let msg: ietf::RequestError = reader.decode().await?;
							tracing::warn!(message = ?msg, "request token renewal refused");
						}
					}
					*answers.lock() += 1;
				}
				Some(_) => return Err(Error::UnexpectedMessage),
				None => return Err(Error::ProtocolViolation),
			}
		}
	}

	/// Tell the publisher to stop serving a subscription we are walking away from.
	///
	/// Every path that abandons an Established subscription goes through here, because
	/// staying silent is what leaves the publisher serving a track nobody is reading and
	/// feeding an alias we already retired.
	///
	/// Two mechanisms, by version. Draft-14 through 16 carry requests over the control
	/// stream adapter, whose virtual streams have no reset or stop of their own, so
	/// UNSUBSCRIBE (draft-16 section 9.12) is the only thing the peer ever sees, and
	/// draft-16 section 5.1.1 makes receiving it what frees the subscription. Draft-17
	/// removed the message, leaving the stream itself: a FIN is explicitly not a
	/// cancellation (draft-19 section 3.3.2), so section 3.3.3's pair applies, an endpoint
	/// that has already FINed its sending direction cancels with STOP_SENDING on the
	/// receiving one.
	async fn cancel_subscribe(&self, stream: Stream<S, Version>, request_id: RequestId) {
		let Stream { mut writer, mut reader } = stream;

		if self.unsubscribes()
			&& let Err(err) = self.write_unsubscribe(&mut writer, request_id).await
		{
			tracing::debug!(%err, "failed to write unsubscribe");
		}

		// STOP_SENDING needs no acknowledgement, so it goes first and the wait below covers
		// only what we still have to deliver.
		reader.abort(&Error::Cancel);

		// Finishing alone would leave the writer's Drop free to RESET_STREAM, and a stream
		// that has sent its FIN is still retransmitting: the reset would discard the
		// UNSUBSCRIBE before the peer ever read it, which is the whole message. Closing
		// consumes the writer, removing that fallback, and waits for the acknowledgement.
		if let Err(err) = writer.close().await {
			tracing::debug!(%err, "failed to close the subscribe stream");
		}
	}

	/// Whether this version cancels a subscription with an UNSUBSCRIBE message.
	///
	/// Draft-17 removed it, leaving the stream reset as the only signal.
	fn unsubscribes(&self) -> bool {
		matches!(self.version, Version::Draft14 | Version::Draft15 | Version::Draft16)
	}

	async fn write_unsubscribe(
		&self,
		writer: &mut crate::coding::Writer<S::SendStream, Version>,
		request_id: RequestId,
	) -> Result<(), Error> {
		writer.encode(&ietf::Unsubscribe::ID).await?;
		writer.encode(&ietf::Unsubscribe { request_id }).await?;
		Ok(())
	}

	/// Re-present the client's request token on a live subscription as a token-only
	/// REQUEST_UPDATE (SUBSCRIBE_UPDATE), so a refreshed credential reaches the publisher
	/// before the old grant lapses (MoQ request-token renewal).
	///
	/// Token-only: draft-15+ omits the range, priority and forward flag, which an update keeps
	/// as they are. Draft-14's fields are fixed, so it restates the subscription's own:
	/// `start` is where it began (the Largest Object after SUBSCRIBE_OK), which the peer
	/// MUST NOT see decrease. The answer, if the version sends one, is read on the
	/// subscription stream by [`read_publish_done`](Self::read_publish_done).
	async fn send_request_token_update(
		&self,
		writer: &mut crate::coding::Writer<S::SendStream, Version>,
		subscription_id: RequestId,
		subscriber_priority: u8,
		start: ietf::Location,
		token: bytes::Bytes,
	) -> Result<(), Error> {
		let request_id = self.control.next_request_id(&self.runtime).await?;
		// Draft-14/15/16 name the subscription being updated; draft-17+ identifies it by stream.
		let subscription_request_id = match self.version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(subscription_id),
			_ => None,
		};
		let draft14 = self.version == Version::Draft14;
		writer.encode(&ietf::SubscribeUpdate::ID).await?;
		writer
			.encode(&ietf::SubscribeUpdate {
				request_id,
				subscription_request_id,
				start_location: start,
				end_group: 0,
				subscriber_priority: draft14.then_some(subscriber_priority),
				forward: draft14.then_some(true),
				filter: None,
				authorization_token: Some(token),
			})
			.await?;
		Ok(())
	}

	async fn write_subscribe(
		&self,
		stream: &mut Stream<S, Version>,
		request_id: RequestId,
		broadcast: &Path<'_>,
		request: &track::Request,
		join: Join,
	) -> Result<(), Error> {
		// Read the aggregate now: a subscriber can join while the request ID and stream
		// were awaited, and nothing updates the priority after SUBSCRIBE.
		let priority = request.subscription().map(|s| s.priority).unwrap_or(0);
		stream.writer.encode(&ietf::Subscribe::ID).await?;
		stream
			.writer
			.encode(&ietf::Subscribe {
				request_id,
				track_namespace: broadcast.to_owned(),
				track_name: request.name().into(),
				subscriber_priority: super::priority::to_wire(priority),
				group_order: GroupOrder::Descending,
				filter: join.filter,
				fill: join.fill,
				properties_wanted: true,
				authorization_token: self.request_token.peek(),
			})
			.await?;
		Ok(())
	}

	/// Send the joining FETCH that follows a pre-draft-20 SUBSCRIBE, and keep reading its
	/// answer until the subscription ends.
	///
	/// The subscribe's request id names the FETCH. A refusal or a failed send settles the
	/// fill so the live subscription continues from the edge; the data, when there is any,
	/// arrives on its own fetch stream and [`Self::recv_fill`] stitches it.
	async fn start_joining_fetch(
		&self,
		subscribe_id: RequestId,
		track: &track::Producer,
		joining: JoiningFetch,
	) -> Option<MaybeSendBox<'static, ()>> {
		let fill = {
			let state = self.state.lock();
			state.subscribes.get(&subscribe_id)?.fill.clone()
		};

		let fetch_id = match self.control.next_request_id(&self.runtime).await {
			Ok(id) => id,
			Err(_) => {
				settle_join_live(&fill);
				return None;
			}
		};

		{
			let mut state = self.state.lock();
			let track = state.subscribes.get_mut(&subscribe_id)?;
			track.fetch_id = Some(fetch_id);
			state.fetches.insert(fetch_id, subscribe_id);
		}

		let mut stream = match Stream::open(&mut self.session.clone(), self.version).await {
			Ok(s) => s,
			Err(err) => {
				tracing::debug!(%err, "failed to open joining FETCH stream");
				settle_join_live(&fill);
				return None;
			}
		};

		let fetch_type = match joining {
			JoiningFetch::Relative { group_offset } => FetchType::RelativeJoining {
				subscriber_request_id: subscribe_id,
				group_offset,
			},
			JoiningFetch::Absolute { group_id } => FetchType::AbsoluteJoining {
				subscriber_request_id: subscribe_id,
				group_id,
			},
		};

		if let Err(err) = async {
			stream.writer.encode(&ietf::Fetch::ID).await?;
			stream
				.writer
				.encode(&ietf::Fetch {
					request_id: fetch_id,
					subscriber_priority: super::priority::to_wire(
						track.subscription().map(|s| s.priority).unwrap_or(0),
					),
					group_order: GroupOrder::Ascending,
					fetch_type,
				})
				.await?;
			Ok::<(), Error>(())
		}
		.await
		{
			tracing::debug!(%err, "failed to write joining FETCH");
			settle_join_live(&fill);
			return None;
		}

		let mut this = self.clone();
		Some(
			async move {
				this.finish_joining_fetch(stream, fill).await;
			}
			.maybe_boxed(),
		)
	}

	async fn finish_joining_fetch(&mut self, mut stream: Stream<S, Version>, fill: kio::Producer<Fill>) {
		if !matches!(self.read_fetch_response(&mut stream).await, Ok(true)) {
			settle_join_live(&fill);
			let _ = stream.writer.close().await;
			return;
		}
		// Hold the request open until this task is dropped with the subscription.
		// Closing our send side first is what a draft-14-16 adapter treats as
		// cancelling the FETCH, and the objects then never leave the publisher.
		let _stream = stream;
		std::future::pending::<()>().await;
	}

	/// `true` when the publisher answered FETCH_OK. A FETCH_ERROR / REQUEST_ERROR is a
	/// refusal, not a session error: the live subscription continues.
	async fn read_fetch_response(&self, stream: &mut Stream<S, Version>) -> Result<bool, Error> {
		let type_id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let mut data = stream.reader.read_exact(size as usize).await?;

		match type_id {
			ietf::FetchOk::ID => {
				let _msg = ietf::FetchOk::decode_msg(&mut data, self.version)?;
				Ok(true)
			}
			ietf::FetchError::ID if self.version == Version::Draft14 => {
				let _msg = ietf::FetchError::decode_msg(&mut data, self.version)?;
				Ok(false)
			}
			ietf::RequestError::ID => {
				let _msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				Ok(false)
			}
			_ => Err(Error::UnexpectedMessage),
		}
	}

	async fn read_subscribe_response(&self, stream: &mut Stream<S, Version>) -> Result<Option<Accepted>, Error> {
		// Read type_id + size + body from the stream
		let type_id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let mut data = stream.reader.read_exact(size as usize).await?;

		match type_id {
			ietf::SubscribeOk::ID => {
				let msg = ietf::SubscribeOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "received subscribe ok");
				Ok(Some(Accepted {
					alias: msg.track_alias,
					timescale: msg.properties.timescale,
					priority: msg.properties.priority,
					largest: msg.largest,
				}))
			}
			// The rejection reaches the track as the reason the publisher gave, so a
			// subscriber can tell a broadcast that is not there from one it may not have.
			ietf::SubscribeError::ID if self.version == Version::Draft14 => {
				let msg = ietf::SubscribeError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "subscribe error");
				Err(request::from_code(
					msg.error_code,
					request::Kind::Subscribe,
					self.version,
				))
			}
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "request error");
				Err(request::from_code(
					msg.error_code,
					request::Kind::Subscribe,
					self.version,
				))
			}
			_ => Err(Error::UnexpectedMessage),
		}
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let mut group: ietf::GroupHeader = stream.decode().await?;

		if group.sub_group_id != 0 {
			tracing::warn!(sub_group_id = %group.sub_group_id, "subgroup ID is not supported, dropping stream");
			return Err(Error::Unsupported);
		}

		// SUBSCRIBE_OK or PUBLISH can be reordered behind this stream. Hold only the
		// subgroup header while waiting so the data stream cannot consume flow control.
		let aliases = self.state.lock().aliases.consume();
		let request_id = match resolve_track_alias(&self.runtime, aliases, group.track_alias).await {
			Ok(request_id) => request_id,
			// Ours: we cancelled the subscription and the publisher has not stopped yet.
			Err(err @ Error::Cancel) => {
				tracing::debug!(track_alias = %group.track_alias, "dropping group for a cancelled subscription");
				return Err(err);
			}
			// Theirs: nothing ever bound this alias. Either the publisher sent data for a
			// track it never acknowledged, or SUBSCRIBE_OK is more than a timeout behind.
			Err(err) => {
				tracing::warn!(
					track_alias = %group.track_alias,
					timeout = ?TRACK_ALIAS_TIMEOUT,
					"unknown track alias: no SUBSCRIBE_OK bound it"
				);
				return Err(err);
			}
		};

		let (mut track, timescale, fill, mut reading) = {
			let state = self.state.lock();
			let track = state.subscribes.get(&request_id).ok_or(Error::NotFound)?;
			(
				track.producer.clone().ok_or(Error::NotFound)?,
				track.timescale,
				track.fill.clone(),
				// Every data stream counts toward PUBLISH_DONE's Stream Count, even one
				// dropped below, and the subscription's end waits until it is read.
				Reading::open(&track.tail, Some(group.group_id), self.runtime.now()),
			)
		};

		// An omitted header priority inherits the track property, then wire 128.
		// The track info carries that fallback after SUBSCRIBE_OK (draft-21 section 10.4).
		if !group.flags.has_priority {
			group.publisher_priority = super::priority::to_wire(track.publisher_priority());
		}

		// FIRST_OBJECT clear says this stream starts partway through the group, which the
		// draft lets a publisher do to answer a filter. Without a head it is unusable: the
		// objects are not decodable without the ones missing in front, and a group is the
		// unit an application resyncs on. Drop it and pick up at the next group, the same
		// degradation as a publisher that no longer holds the head.
		//
		// A fill we asked for is the exception, since its fetch stream is carrying exactly
		// that head for [`Self::open_group`] to stitch this onto.
		//
		// The bit is only the publisher's claim, so what is enforced is the object ids
		// themselves: [`next_object_id`] holds every object to starting where the head
		// stopped and incrementing by 1, whatever the header said and on the drafts that
		// have no such bit to read.
		if !group.flags.first_object && !fill.read().outstanding() {
			tracing::debug!(
				track_alias = %group.track_alias,
				group = %group.group_id,
				"dropping a group with no head"
			);
			return Err(Error::Unsupported);
		}

		// The peek inside blocks until the publisher produces the group's first object, so
		// race it against the subscription going away the same way the group read below is.
		// Otherwise dropping the local subscriber cannot end this handler.
		let opened = {
			let mut opening = track.clone();
			let mut open = std::pin::pin!(self.open_group(stream, &mut opening, &fill, &group, &mut reading));
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_closed(waiter) {
					return Poll::Ready(Err(err));
				}
				waiter.poll_future(open.as_mut())
			})
			.await
		};
		let opened = match opened {
			// The group is at or past the end the publisher declared, which no later stream
			// can repair.
			Err(Error::Closed) => {
				tracing::warn!(group = group.group_id, "group past the declared end of track");
				let _ = track.abort(Error::ProtocolViolation);
				return Err(Error::ProtocolViolation);
			}
			Err(err) => return Err(err),
			Ok(opened) => opened,
		};
		let (producer, start) = match opened {
			Opened::Group(producer, start) => (producer, start),
			// No object at or past object 0 of this group exists, so neither does the group.
			Opened::EndOfTrack => return end_track(&mut track, group.group_id),
		};

		// Guarded: this handler can be dropped at any await below, and a group producer
		// that dies without a terminal leaves its consumer waiting on nothing.
		let producer = crate::recv::Group::new(producer);

		let res = {
			let mut ingest = GroupIngest::new(self.runtime.clone(), &group, timescale, self.version, start);
			let mut writing = producer.clone();
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_closed(waiter) {
					return Poll::Ready(Err(err));
				}
				if let Poll::Ready(err) = producer.poll_closed(waiter) {
					return Poll::Ready(Err(err));
				}
				ingest.poll(stream, &mut writing, waiter)
			})
			.await
		};

		match res {
			Err(err @ (Error::Cancel | Error::Stream(crate::StreamError::Cancel))) => {
				let _ = producer.abort(err);
			}
			Err(err @ Error::Decode(DecodeError::MessageTooLarge { .. })) => {
				let _ = producer.abort(err.clone());
				// Return the refusal to the dispatcher so it sends STOP_SENDING.
				return Err(err);
			}
			Err(err) => {
				tracing::debug!(%err, group = %producer.sequence, "group error");
				let _ = producer.abort(err);
			}
			Ok(Ended::Group) => {
				let _ = producer.finish();
			}
			// No object past this group's last one exists, so the track ends after it.
			Ok(Ended::Track) => {
				let _ = producer.finish();
				return end_track(&mut track, group.group_id.saturating_add(1));
			}
		}

		Ok(())
	}
}

/// Mark where the track ends, as an END_OF_TRACK object said.
///
/// Draft-14 on carries no end location in PUBLISH_DONE, so this is what lets a subscriber
/// learn the end before the live edge reaches it. A boundary at or below a group already
/// received is the publisher breaking its own end, which no later group can repair. A
/// marker that lands after the subscription already ended (its grace expired) changes
/// nothing.
fn end_track(track: &mut track::Producer, end: u64) -> Result<(), Error> {
	if track.final_sequence().is_some() {
		return Ok(());
	}
	if let Err(err) = track.finish_at(end) {
		tracing::warn!(%err, end, "invalid END_OF_TRACK");
		let _ = track.clone().abort(Error::ProtocolViolation);
		return Err(Error::ProtocolViolation);
	}
	Ok(())
}

/// How [`Subscriber::open_group`] resolved a subgroup stream.
enum Opened {
	/// The group producer the stream writes into, and the Object ID it starts at.
	Group(group::Producer, u64),
	/// The stream's first object is an END_OF_TRACK at object 0: the group does not exist.
	EndOfTrack,
}

/// How a subgroup stream ended cleanly.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
	/// The stream finished, or carried an explicit end of group.
	Group,
	/// It carried an END_OF_TRACK after the group's last object.
	Track,
}

/// Object status: no object at or past this location exists (every implemented draft).
const END_OF_TRACK: u64 = 0x4;

// Implementation limit for object extension blocks, independent of the IETF draft.
const MAX_OBJECT_EXTENSIONS: usize = 64 * 1024;

#[derive(Debug)]
struct ObjectExtensionsLength(usize);

impl Decode<Version> for ObjectExtensionsLength {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let size = usize::decode(buf, version)?;
		if size > MAX_OBJECT_EXTENSIONS {
			return Err(DecodeError::MessageTooLarge {
				size,
				max: MAX_OBJECT_EXTENSIONS,
			});
		}
		Ok(Self(size))
	}
}

/// The start of a subgroup stream's first object, peeked before its group is created.
#[derive(Debug, Clone, Copy)]
struct FirstObject {
	/// The Object ID, which the first object's delta is.
	id: u64,
	end_of_track: bool,
}

/// [`FirstObject`] for a stream whose objects carry extensions, or don't.
#[derive(Debug)]
struct PeekFirst<const EXTENSIONS: bool>(FirstObject);

impl<const EXTENSIONS: bool> Decode<Version> for PeekFirst<EXTENSIONS> {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let id = u64::decode(buf, version)?;
		if EXTENSIONS {
			let ObjectExtensionsLength(size) = ObjectExtensionsLength::decode(buf, version)?;
			if buf.remaining() < size {
				return Err(DecodeError::Short);
			}
			buf.advance(size);
		}
		let size = u64::decode(buf, version)?;
		let end_of_track = size == 0 && u64::decode(buf, version)? == END_OF_TRACK;
		Ok(Self(FirstObject { id, end_of_track }))
	}
}

impl<S> Subscriber<S>
where
	S: crate::transport::poll::Boxable,
{
	/// The group producer this subgroup stream writes into, and the Object ID it starts at.
	///
	/// The first object is peeked before any producer exists, since an END_OF_TRACK at
	/// object 0 means the group does not exist at all. Normally the stream then starts the
	/// group. While a fill is outstanding it may instead be the tail of the group the fill
	/// fetch stream began, which the first Object ID decides.
	async fn open_group(
		&self,
		stream: &mut Reader<S::RecvStream, Version>,
		track: &mut track::Producer,
		fill: &kio::Producer<Fill>,
		header: &ietf::GroupHeader,
		reading: &mut Reading,
	) -> Result<Opened, Error> {
		let sequence = header.group_id;
		// Stats (groups/frames/bytes) are counted in the model as the group is written,
		// through the tagged `track::Producer`.
		let create = |track: &mut track::Producer| track.create_group(group::Info { sequence });

		let peeked = match header.flags.has_extensions {
			true => stream
				.decode_peek_maybe::<PeekFirst<true>>()
				.await
				.map(|peek| peek.map(|peek| peek.0)),
			false => stream
				.decode_peek_maybe::<PeekFirst<false>>()
				.await
				.map(|peek| peek.map(|peek| peek.0)),
		};
		let first = match peeked {
			Ok(first) => first,
			// The header arrived, so the stream counts toward the track's end. Abort the
			// group it named rather than let the track end clean without it. A fill that
			// already holds the group reports it through its own producer.
			Err(err) => {
				if let Ok(group) = create(track) {
					let _ = group.abort(err.clone());
				}
				return Err(err);
			}
		};
		if first.is_some_and(|first| first.id == 0 && first.end_of_track) {
			return Ok(Opened::EndOfTrack);
		}

		if !fill.read().outstanding() {
			return Ok(Opened::Group(create(track)?, 0));
		}

		// The first object's ID delta is its absolute Object ID (see `next_object_id`).
		match first.map(|first| first.id) {
			// A group delivered from its start stands alone, unless the fill already
			// headed this very sequence: the publisher then served those objects twice,
			// and the model has one producer per group. Publish the head as the prefix it
			// is and drop the stream rather than deliver them again.
			Some(0) => {
				let headed = matches!(*fill.read(), Fill::Ready { sequence: s, .. } if s == sequence);
				if headed {
					tracing::warn!(sequence, "a whole group arrived for one the fill already headed");
					if let Ok(mut state) = fill.write() {
						state.release();
					}
					return Err(Error::Unsupported);
				}

				Ok(Opened::Group(create(track)?, 0))
			}

			// A group starting partway through is the tail of one the fill began, and
			// without that head it has a hole at the front.
			Some(start) => match self.claim_fill(fill, track, sequence, Some(start), reading).await? {
				Some(producer) => Ok(Opened::Group(producer, start)),
				None => {
					tracing::warn!(sequence, start, "no fill to stitch a mid-group stream onto");
					Err(Error::Unsupported)
				}
			},

			// A stream that ends without an object: the group is over and had nothing
			// outside the fill's range, so the head it delivered is the whole group.
			None => match self.claim_fill(fill, track, sequence, None, reading).await? {
				Some(producer) => Ok(Opened::Group(producer, 0)),
				None => Ok(Opened::Group(create(track)?, 0)),
			},
		}
	}

	/// Take the head the fill fetch stream delivered for `sequence`, once it has finished
	/// writing it.
	///
	/// The model has one producer per group, so this is the handoff: the fill owns the
	/// producer while it writes objects `0..next`, and the subgroup stream carrying the rest
	/// picks it up here. `start` is the Object ID that stream begins at, or `None` when it
	/// carries no objects at all and simply ends the group.
	///
	/// Waiting is what keeps the two streams from interleaving into one producer. It ends
	/// with the subscription, so a publisher that promises a fill and never delivers one
	/// costs this stream and nothing else. Meanwhile this stream stops holding the
	/// subscription's end open: the fill's own stream holds it if its header arrived, and
	/// the grace gives up on it if not.
	async fn claim_fill(
		&self,
		fill: &kio::Producer<Fill>,
		track: &track::Producer,
		sequence: u64,
		start: Option<u64>,
		reading: &mut Reading,
	) -> Result<Option<group::Producer>, Error> {
		reading.park();
		let settled = kio::wait(|waiter| {
			if let Poll::Ready(err) = track.poll_closed(waiter) {
				return Poll::Ready(Err(err));
			}

			let settled = fill.poll(waiter, |fill| match **fill {
				Fill::Requested | Fill::Serving(_) | Fill::Active => Poll::Pending,
				Fill::Ready { .. } | Fill::Done => Poll::Ready(()),
			});

			match settled {
				Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
				// The subscription went away underneath us.
				Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Dropped)),
				Poll::Pending => Poll::Pending,
			}
		})
		.await;
		// Hold the end open again before taking the head: once the fill is claimed, this
		// stream is the only thing saying the group is still being read.
		reading.resume();
		settled?;
		fill.write().map_err(|_| Error::Dropped)?.claim(sequence, start)
	}
}

/// Pumps moq-transport subgroup objects from a reader into a group producer:
/// the id delta, the extension headers (carrying the timestamp), the size, the
/// status for empty objects, and the streamed payload.
struct GroupIngest {
	runtime: crate::time::Clock,
	has_extensions: bool,
	has_end: bool,
	timescale: Option<Timescale>,
	version: Version,
	prior_object: Option<u64>,
	start: u64,
	phase: IngestPhase,
}

enum IngestPhase {
	/// Reading the object id delta. Stream end here ends the group.
	Delta,
	/// Reading the extension block's size.
	ExtSize,
	/// Reading (and decoding or discarding) the extension block.
	ExtBytes { size: usize },
	/// Reading the object size.
	Size { timestamp: Option<crate::Timestamp> },
	/// Reading the status of an empty object.
	Status { timestamp: Option<crate::Timestamp> },
	/// Streaming the object payload.
	Payload { frame: frame::ProducerOwned },
	/// An explicit end-of-group or end-of-track status arrived.
	Finished(Ended),
}

impl GroupIngest {
	fn new(
		runtime: crate::time::Clock,
		group: &ietf::GroupHeader,
		timescale: Option<Timescale>,
		version: Version,
		start: u64,
	) -> Self {
		Self {
			runtime,
			has_extensions: group.flags.has_extensions,
			has_end: group.flags.has_end,
			timescale,
			version,
			prior_object: None,
			start,
			phase: IngestPhase::Delta,
		}
	}
}

impl<S> Subscriber<S>
where
	S: crate::transport::poll::Boxable,
{
	/// Read a fill fetch stream: the head of the group a subscription joins part way through.
	///
	/// A draft-20 fill answers the FILL_PARAMETERS we sent and is named by the SUBSCRIBE's
	/// Request ID. A pre-draft-20 joining FETCH has its own request id, mapped back to that
	/// subscription. Unlike a subgroup stream it needs no track alias. It writes the objects
	/// into a group producer of its own and hands that to the subgroup stream carrying the
	/// rest of the group; see [`Fill`]. A reset stream is the publisher's fill-failure
	/// signal, and arrives here as a read error, which drops the head and the join with it.
	pub async fn recv_fill(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		// The dispatcher peeked the stream type to get here.
		let _: u64 = stream.decode().await?;
		let header: ietf::FetchHeader = stream.decode().await?;

		let (subscribe_id, fill, joining, largest, _counted) = {
			let state = self.state.lock();
			// A draft-20 fill is named by the SUBSCRIBE's request id. A pre-draft-20 joining
			// FETCH has its own id, which `fetches` maps back to that subscription.
			let joined = state.fetches.get(&header.request_id).copied();
			let subscribe_id = joined.unwrap_or(header.request_id);
			let track = state.subscribes.get(&subscribe_id).ok_or(Error::NotFound)?;
			// A fill is one of the subscription's own data streams, so PUBLISH_DONE counts
			// it. A joining FETCH is a request of its own.
			let counted = joined
				.is_none()
				.then(|| Reading::open(&track.tail, None, self.runtime.now()));
			(subscribe_id, track.fill.clone(), track.joining, track.largest, counted)
		};

		// SUBSCRIBE_OK declares the units these object timestamps are in, and this stream can
		// be reordered ahead of it. Taking the fill in the same step is what refuses a second
		// stream for a request that asked for one fill.
		let timescale = kio::wait(|waiter| {
			let accepted = fill.poll(waiter, |fill| match **fill {
				Fill::Requested => Poll::Pending,
				_ => Poll::Ready(()),
			});

			match accepted {
				Poll::Ready(Ok(mut fill)) => Poll::Ready(match *fill {
					Fill::Serving(timescale) => {
						*fill = Fill::Active;
						Ok(timescale)
					}
					// We requested no fill, or this is a second stream answering the one we
					// did. Either way its objects would duplicate a group already in flight.
					_ => Err(Error::Unsupported),
				}),
				// The subscription went away underneath us.
				Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Dropped)),
				Poll::Pending => Poll::Pending,
			}
		})
		.await?;
		// A fill stream can overtake SUBSCRIBE_OK. The fill gate above is released only
		// after that response commits track Info and installs the producer.
		let track = {
			let state = self.state.lock();
			state
				.subscribes
				.get(&subscribe_id)
				.and_then(|track| track.producer.clone())
				.ok_or(Error::NotFound)?
		};

		// Race the peer's stream against the subscription going away, the same way a
		// subgroup stream is served. Otherwise a peer that stalls partway through a payload
		// keeps this handler and its stream alive for as long as it cares to: aborting the
		// track does not close a group producer, since those lifecycles are independent.
		let res = {
			let mut serving = track.clone();
			let mut serve = std::pin::pin!(self.run_fill(stream, &mut serving, timescale, joining, largest));
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_closed(waiter) {
					return Poll::Ready(Err(err));
				}
				waiter.poll_future(serve.as_mut())
			})
			.await
		};

		let head = match res {
			Ok(head) => head,
			Err(err) => {
				if let Ok(mut state) = fill.write() {
					*state = Fill::Done;
				}
				return Err(err);
			}
		};

		// The subscription can end while the head is being written, and its teardown cannot
		// reach a producer this task still owns. So the handoff is where that is settled.
		match fill.write() {
			Ok(mut state) => state.install(head),
			// The subscription is gone entirely, so nothing is left to hand it to.
			Err(_) => {
				let mut head = head;
				head.release();
				return Err(Error::Dropped);
			}
		}

		Ok(())
	}

	/// Read the fill's objects into a group producer of its own.
	///
	/// Returns the head for the live tail to claim, or [`Fill::Done`] when the stream
	/// carried no objects at all. Aborts the producer on the way out of an error, since a
	/// half-written head is a group with no end.
	async fn run_fill(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		track: &mut track::Producer,
		timescale: Option<Timescale>,
		joining: Option<JoiningFetch>,
		largest: Option<ietf::Location>,
	) -> Result<Fill, Error> {
		let mut head: Option<(u64, u64, crate::recv::Group)> = None;

		match self
			.run_fill_objects(stream, track, timescale, joining, largest, &mut head)
			.await
		{
			Ok(()) => Ok(match head {
				Some((sequence, next, producer)) => {
					// An absolute join that never reached the live group delivered complete
					// groups only. Finish the last one rather than leaving it as a head for a
					// tail that is not coming; the hole before the live edge is a discontinuity.
					if matches!(joining, Some(JoiningFetch::Absolute { .. }))
						&& largest.is_some_and(|largest| sequence < largest.group)
					{
						producer.finish()?;
						Fill::Done
					} else {
						Fill::Ready {
							sequence,
							next,
							producer: producer.into_inner(),
						}
					}
				}
				None => Fill::Done,
			}),
			Err(err) => {
				if let Some((_, _, producer)) = head {
					let _ = producer.abort(err.clone());
				}
				Err(err)
			}
		}
	}

	/// Decode fetch objects into `head`, creating each group from the first object's
	/// absolute IDs.
	///
	/// A relative join and a draft-20 fill are one group: objects numbered from the start
	/// with no gaps. An absolute join spans whole groups up to the subscribe's Largest
	/// Location; each complete group below that is finished, and the last one is the head
	/// the live stream continues. Anything else is a head the model cannot represent, and
	/// refusing the stream leaves the subscription itself alone.
	async fn run_fill_objects(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		track: &mut track::Producer,
		timescale: Option<Timescale>,
		joining: Option<JoiningFetch>,
		largest: Option<ietf::Location>,
		head: &mut Option<(u64, u64, crate::recv::Group)>,
	) -> Result<(), Error> {
		let mut prior_group = None;
		while let Some(object) = decode_fetch_object(stream, self.version).await? {
			if !object.subgroup_ok {
				tracing::warn!("subgroup ID is not supported, dropping fill");
				return Err(Error::Unsupported);
			}

			// Object ID still keys off whether the wire Group ID field was present.
			let group = resolve_fetch_group(self.version, prior_group, object.group)?;
			if let Some(sequence) = group {
				prior_group = Some(sequence);
			}

			match head.as_ref().map(|(sequence, next, _)| (*sequence, *next)) {
				None => {
					let (Some(sequence), Some(0)) = (group, object.object) else {
						tracing::warn!(
							group = ?group,
							object = ?object.object,
							"a fill must start at a group's first object"
						);
						return Err(Error::Unsupported);
					};
					open_fill_group(track, head, sequence)?;
				}
				Some((sequence, _)) if group.is_some_and(|group| group != sequence) => {
					let Some(group) = group else {
						unreachable!("the filter above proved group is Some");
					};
					if object.object != Some(0) {
						tracing::warn!(
							group,
							object = ?object.object,
							"a fill must start at a group's first object"
						);
						return Err(Error::Unsupported);
					}
					advance_fill_group(track, head, group, joining, largest)?;
				}
				Some((sequence, next)) => {
					let id = match (object.group.is_some(), object.object) {
						(true, Some(id)) => id,
						(false, None | Some(1)) => next,
						_ => {
							tracing::warn!(
								sequence,
								next,
								object = ?object.object,
								"fill object IDs must increment by 1"
							);
							return Err(Error::Unsupported);
						}
					};
					if id != next {
						tracing::warn!(sequence, next, object = id, "fill object IDs must increment by 1");
						return Err(Error::Unsupported);
					}
				}
			}

			// The properties carry the frame's presentation timestamp (the Timestamp Object
			// Property) in the units the track declared. A track that declared none opted
			// out, so its frames are stamped on arrival instead.
			let timestamp = match (object.properties, timescale) {
				(Some(properties), Some(timescale)) => {
					let mut properties = bytes::Bytes::from(properties);
					ietf::decode_object_time(&mut properties, timescale, self.version)?
				}
				_ => None,
			};
			let timestamp = timestamp.unwrap_or_else(|| crate::Timestamp::from(self.runtime.now()));

			// A fetch object has no status field from draft-16 on; a zero length is simply
			// an empty object. Draft-14 and 15 still encode Normal (0) after a zero length.
			let size: u64 = stream.decode().await?;
			if size == 0 && matches!(self.version, Version::Draft14 | Version::Draft15) {
				let status: u64 = stream.decode().await?;
				if status != 0 {
					return Err(Error::Unsupported);
				}
			}

			let (_, next, producer) = head.as_mut().expect("the head was created above");

			// `create_frame_owned` is the allocation chokepoint and rejects an oversized `size`
			// before allocating, so no pre-check is needed.
			let mut frame = producer.create_frame_owned(frame::Info { size, timestamp })?;
			if let Err(err) = std::future::poll_fn(|cx| stream.poll_read_frame(cx, &mut frame)).await {
				let _ = frame.abort(err.clone());
				return Err(err);
			}
			frame.finish()?;

			*next += 1;
		}

		Ok(())
	}
}

/// One Object header on a fetch stream, after version-specific decoding.
struct FetchedObject {
	group: Option<u64>,
	object: Option<u64>,
	subgroup_ok: bool,
	properties: Option<Vec<u8>>,
}

/// Decode the next fetch object header, or `None` at stream end.
async fn decode_fetch_object<R: crate::transport::poll::RecvStream>(
	stream: &mut Reader<R, Version>,
	version: Version,
) -> Result<Option<FetchedObject>, Error> {
	if version == Version::Draft14 {
		let Some(group) = stream.decode_maybe::<u64>().await? else {
			return Ok(None);
		};
		let subgroup: u64 = stream.decode().await?;
		let object: u64 = stream.decode().await?;
		let _priority: u8 = stream.decode().await?;
		let properties: Vec<u8> = stream.decode().await?;
		return Ok(Some(FetchedObject {
			group: Some(group),
			object: Some(object),
			subgroup_ok: subgroup == 0,
			properties: Some(properties),
		}));
	}

	Ok(match stream.decode_maybe::<ietf::FetchObject>().await? {
		None => None,
		Some(ietf::FetchObject::EndOfRange { .. }) => {
			tracing::warn!("a fill with an End of Range cannot be stitched");
			return Err(Error::Unsupported);
		}
		Some(ietf::FetchObject::Object {
			subgroup,
			group,
			object,
			properties,
			..
		}) => Some(FetchedObject {
			group,
			object,
			subgroup_ok: matches!(
				subgroup,
				ietf::FetchSubgroup::Zero | ietf::FetchSubgroup::Prior | ietf::FetchSubgroup::Explicit(0)
			),
			properties,
		}),
	})
}

/// Absolute Group ID from a fetch object's Group ID field.
///
/// The first object is always absolute. Draft-14 through draft-17 keep sending the
/// absolute ID when the field is present; draft-18 and later send a Group ID Delta,
/// resolved here in the ascending order this FETCH requested.
fn resolve_fetch_group(version: Version, prior: Option<u64>, wire: Option<u64>) -> Result<Option<u64>, Error> {
	let Some(wire) = wire else {
		return Ok(None);
	};
	let Some(prior) = prior else {
		return Ok(Some(wire));
	};
	match version {
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 => Ok(Some(wire)),
		_ => {
			let step = wire.checked_add(1).ok_or(Error::Unsupported)?;
			prior.checked_add(step).map(Some).ok_or(Error::Unsupported)
		}
	}
}

fn open_fill_group(
	track: &mut track::Producer,
	head: &mut Option<(u64, u64, crate::recv::Group)>,
	sequence: u64,
) -> Result<(), Error> {
	let producer = track.create_group(group::Info { sequence })?;
	*head = Some((sequence, 0, crate::recv::Group::new(producer)));
	Ok(())
}

/// Finish the group we were writing and open the next one, which only an absolute joining
/// FETCH is allowed to span.
fn advance_fill_group(
	track: &mut track::Producer,
	head: &mut Option<(u64, u64, crate::recv::Group)>,
	sequence: u64,
	joining: Option<JoiningFetch>,
	largest: Option<ietf::Location>,
) -> Result<(), Error> {
	if !matches!(joining, Some(JoiningFetch::Absolute { .. })) {
		tracing::warn!("a fill spanning several groups cannot be stitched");
		return Err(Error::Unsupported);
	}

	let Some((prev, _, producer)) = head.take() else {
		return open_fill_group(track, head, sequence);
	};

	if largest.is_some_and(|largest| prev >= largest.group) {
		tracing::warn!("a joining FETCH continued past the subscribe's Largest Location");
		let _ = producer.abort(Error::Unsupported);
		return Err(Error::Unsupported);
	}

	producer.finish()?;
	open_fill_group(track, head, sequence)
}

/// Ready once a finished subscription's data streams are accounted for: Stream Count of
/// their headers, and no fill outstanding. A tail parked on its head holds nothing open
/// itself, so the fill does until the head is claimed.
fn poll_settled(settle: &mut Settle, waiter: &kio::Waiter, fill: &kio::Producer<Fill>, count: u64) -> Poll<()> {
	// Read before the tail: Done is terminal, so the answer cannot go stale.
	let filled = !fill.read().outstanding();
	settle.poll(waiter, |tail| filled && count > 0 && tail.streams() >= count)
}

/// A refused or missing joining FETCH continues the subscription live: drop the outstanding
/// fill so a mid-group tail is not left waiting on a head that is never coming.
fn settle_join_live(fill: &kio::Producer<Fill>) {
	let Ok(mut state) = fill.write() else {
		return;
	};
	if matches!(*state, Fill::Requested | Fill::Serving(_)) {
		*state = Fill::Done;
	}
}

impl GroupIngest {
	/// `Ready(Ok(_))` once the stream FINs on an object boundary, or an explicit
	/// end-of-group or end-of-track status arrives. The caller finishes or aborts the
	/// group; an object cut short mid-payload was already aborted here with the reason.
	fn poll<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut Reader<R, Version>,
		group: &mut group::Producer,
		waiter: &kio::Waiter,
	) -> Poll<Result<Ended, Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.phase {
				IngestPhase::Delta => {
					let Some(id_delta) = ready!(reader.poll_decode_maybe::<u64>(&mut cx))? else {
						return Poll::Ready(Ok(Ended::Group));
					};
					self.prior_object = Some(next_object_id(self.prior_object, id_delta, self.start)?);
					self.phase = match self.has_extensions {
						true => IngestPhase::ExtSize,
						false => IngestPhase::Size { timestamp: None },
					};
				}
				IngestPhase::ExtSize => {
					let ObjectExtensionsLength(size) = ready!(reader.poll_decode(&mut cx))?;
					self.phase = IngestPhase::ExtBytes { size };
				}
				IngestPhase::ExtBytes { size } => {
					// Per-object extension headers may carry the frame's presentation
					// timestamp (the Timestamp Object Property), in the units the track
					// declared. A track that declared no timescale opted out, so its
					// objects are stamped on arrival even if one carries a Timestamp we
					// could not interpret.
					let mut ext = ready!(reader.poll_read_exact(&mut cx, *size))?;
					let timestamp = match self.timescale {
						Some(timescale) => ietf::decode_object_time(&mut ext, timescale, self.version)?,
						None => None,
					};
					self.phase = IngestPhase::Size { timestamp };
				}
				IngestPhase::Size { timestamp } => {
					let size: u64 = ready!(reader.poll_decode(&mut cx))?;
					if size == 0 {
						self.phase = IngestPhase::Status { timestamp: *timestamp };
						continue;
					}
					// `create_frame_owned` is the allocation chokepoint and rejects an
					// oversized `size` before allocating, so no pre-check is needed.
					let timestamp = timestamp.unwrap_or_else(|| crate::Timestamp::from(self.runtime.now()));
					let frame = group.create_frame_owned(frame::Info { size, timestamp })?;
					self.phase = IngestPhase::Payload { frame };
				}
				IngestPhase::Status { timestamp } => {
					let status: u64 = ready!(reader.poll_decode(&mut cx))?;
					if status == 0 {
						let timestamp = timestamp.unwrap_or_else(|| crate::Timestamp::from(self.runtime.now()));
						let frame = group.create_frame_owned(frame::Info { size: 0, timestamp })?;
						frame.finish()?;
						self.phase = IngestPhase::Delta;
					} else if status == 3 && !self.has_end {
						self.phase = IngestPhase::Finished(Ended::Group);
					} else if status == END_OF_TRACK {
						// Defined on every implemented draft, whether or not the header marks
						// the group's end.
						self.phase = IngestPhase::Finished(Ended::Track);
					} else {
						return Poll::Ready(Err(Error::Unsupported));
					}
				}
				IngestPhase::Payload { frame } => {
					let failed = ready!(reader.poll_read_frame(&mut cx, frame)).err();

					let IngestPhase::Payload { frame } = std::mem::replace(&mut self.phase, IngestPhase::Delta) else {
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
				IngestPhase::Finished(ended) => {
					let ended = std::mem::replace(ended, Ended::Group);
					return Poll::Ready(Ok(ended));
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::model::ProduceTest;
	use futures::poll;

	use super::*;

	async fn check_publish_fin(responses: Vec<u8>, clean: bool) {
		use crate::lite::test_transport::ScriptedSession;
		use crate::transport::poll::Session as _;
		let mut session = ScriptedSession::eof(responses);
		let (_, recv) = session.open_bi().await.unwrap();
		let mut reader = Reader::new(recv, Version::Draft19);
		let answers = kio::Shared::new(0u64);
		let result = Subscriber::<ScriptedSession>::read_publish_done(&mut reader, Version::Draft19, &answers).await;
		if clean {
			assert_eq!(result.unwrap(), 0);
		} else {
			assert!(matches!(result, Err(Error::ProtocolViolation)));
		}
	}

	fn fin_responses(clean: bool) -> Vec<u8> {
		use crate::coding::Encode;
		let mut responses = Vec::new();
		if clean {
			ietf::PublishDone::ID.encode(&mut responses, Version::Draft19).unwrap();
			ietf::PublishDone {
				request_id: None,
				status_code: ietf::PublishDoneStatus::TrackEnded.code(Version::Draft19),
				stream_count: 0,
				reason_phrase: "done".into(),
			}
			.encode(&mut responses, Version::Draft19)
			.unwrap();
		}
		responses
	}

	#[tokio::test(start_paused = true)]
	async fn bare_fin_requires_publish_done() {
		for clean in [false, true] {
			check_publish_fin(fin_responses(clean), clean).await;
		}
	}

	#[tokio::test(start_paused = true)]
	#[ignore = "requires Bun; run by just test bare-fin in interop CI"]
	async fn bare_fin_interop() {
		for clean in [false, true] {
			let responses = crate::test_interop::fin("moqt-19", false, clean, fin_responses(clean));
			check_publish_fin(responses, clean).await;
		}
	}

	/// The tokio-backed test runtime. Its transport parameter is phantom, so one
	/// type serves every fake session in this module.

	#[tokio::test(start_paused = true)]
	async fn track_alias_waits_for_control_message() {
		let runtime = crate::time::Clock::tokio();
		let aliases = TrackAliases::default();
		let pending = resolve_track_alias(&runtime, aliases.consume(), 7);
		tokio::pin!(pending);

		assert!(poll!(&mut pending).is_pending());

		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();

		assert_eq!(pending.await.unwrap(), RequestId(11));
	}

	/// SUBSCRIBE_OK has not accepted the track, so the map holds no producer.
	/// Abort still has to reject the parked origin request with the session error.
	#[tokio::test]
	async fn session_death_rejects_a_subscribe_still_setting_up() {
		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.dynamic();
		let consumer = broadcast.consume();
		let mut waiting = std::pin::pin!(consumer.track("video").unwrap().subscribe(None));
		assert!(poll!(&mut waiting).is_pending());

		let mut requested = std::pin::pin!(dynamic.requested_track());
		let std::task::Poll::Ready(Ok(request)) = poll!(&mut requested) else {
			panic!("the subscribe did not request a track");
		};

		let mut state = State::default();
		state.subscribes.insert(
			RequestId(1),
			TrackState {
				pending: Some(request),
				..TrackState::pending(
					"video".to_string(),
					crate::Path::new("bcast").to_owned(),
					kio::Producer::new(Fill::Done),
					None,
				)
			},
		);
		state.abort(&Error::Session(crate::SessionError::App(7)));

		assert!(
			matches!(
				poll!(&mut waiting),
				std::task::Poll::Ready(Err(Error::Session(crate::SessionError::App(7))))
			),
			"setup was not rejected with the session error"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn unknown_track_alias_times_out() {
		let aliases = TrackAliases::default();
		assert!(matches!(
			resolve_track_alias(&crate::time::Clock::tokio(), aliases.consume(), 7).await,
			Err(Error::NotFound)
		));
	}

	async fn settle() {
		tokio::time::sleep(Duration::from_millis(1)).await;
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// What an unsolicited advertisement means to a subscriber on `version` whose peer
	/// declared `solicit`.
	fn unsolicited_is_a_violation(solicit: Option<bool>, version: Version) -> bool {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			solicit,
			..Default::default()
		});
		let (tasks, _task_set) = crate::util::TaskSet::new();

		Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			origin,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.unsolicited_is_a_violation(solicit)
	}

	/// We always declare that advertisements to us must be solicited, so a peer that
	/// implements the extension and announces anyway has a bug. Tolerating it is what
	/// keeps that bug invisible on both sides, so the session goes.
	///
	/// Writing the option is the proof of support, whichever value it carries: an explicit
	/// 0 says "no requirement of my own" and still says "I read yours".
	#[tokio::test]
	async fn an_announce_from_a_peer_that_implements_solicit_is_fatal() {
		assert!(
			unsolicited_is_a_violation(Some(true), Version::Draft17),
			"a peer that requires solicitation itself"
		);
		assert!(
			unsolicited_is_a_violation(Some(false), Version::Draft17),
			"an explicit 0 declares support, so ours binds it too"
		);
	}

	/// A peer that declared nothing has never heard of the extension, so it cannot have
	/// honored ours. Announcing at us is what it is supposed to do, and #2730 is what
	/// happens when nobody does.
	#[tokio::test]
	async fn an_announce_from_a_peer_that_declared_nothing_is_fine() {
		assert!(!unsolicited_is_a_violation(None, Version::Draft17));
	}

	/// Draft-14/15 have no inline NAMESPACE, so a PUBLISH_NAMESPACE request is also how a
	/// peer answers our own SUBSCRIBE_NAMESPACE. The message cannot say which it is, so
	/// nothing there is enforceable: our own publisher advertises exactly this way.
	#[tokio::test]
	async fn a_legacy_announce_is_never_a_violation() {
		for version in [Version::Draft14, Version::Draft15] {
			assert!(
				!unsolicited_is_a_violation(Some(true), version),
				"{version:?} answers a subscription this way"
			);
		}
	}

	/// A rooted subscriber asks the peer for its permitted SCOPE. The root names where
	/// replies mount on our side, which is meaningless to a peer outside our namespace,
	/// so sending it asks for a prefix that matches nothing there.
	#[tokio::test]
	async fn a_rooted_subscriber_asks_for_its_scope_not_its_root() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let scope = crate::Patterns::from(crate::Pattern::subtree("cam").unwrap());
		let scoped = origin.scope("rootns", &scope).expect("scope the origin");

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		// The request waits on the peer's SETUP to learn whether it may opt in to
		// hidden namespaces.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			scoped,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft16,
			tasks,
			Default::default(),
		);

		assert_eq!(
			subscriber.subscribe_prefixes(),
			vec![crate::Path::new("cam").to_owned()],
			"one SUBSCRIBE_NAMESPACE per permitted prefix, relative to the root",
		);

		let stream = Stream::open(&mut session.clone(), Version::Draft16).await.unwrap();
		let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(stream, crate::Path::new("cam").to_owned()));
		// Parks awaiting the peer's response; the request is already on the wire.
		assert!(futures::poll!(run.as_mut()).is_pending());

		assert_eq!(occurrences(&log, b"cam"), 1, "asked the peer for our scope");
		assert_eq!(occurrences(&log, b"rootns"), 0, "asked the peer for our local root");
	}

	/// The peer's REQUEST_OK followed by one NAMESPACE, framed exactly as
	/// `run_subscribe_namespace` reads it -- built with the crate's own writer so the
	/// framing can't drift from the encoder under test.
	async fn namespace_response(version: Version, suffix: &str) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		writer.encode(&ietf::RequestOk::ID).await.unwrap();
		writer.encode(&ietf::RequestOk { request_id: None }).await.unwrap();
		writer.encode(&ietf::Namespace::ID).await.unwrap();
		writer
			.encode(&ietf::Namespace {
				suffix: crate::Path::new(suffix),
				cluster: None,
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// A NAMESPACE suffix is relative to the prefix we subscribed, and mounts under
	/// our root exactly once.
	///
	/// Driven through the real response stream rather than by recomputing the join
	/// here: a test that did its own `prefix.join(suffix)` would still pass if the
	/// NAMESPACE arm went back to joining the root.
	#[tokio::test]
	async fn a_rooted_subscriber_mounts_a_reply_under_its_root_once() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let scope = crate::Patterns::from(crate::Pattern::subtree("cam").unwrap());
		let scoped = origin.scope("rootns", &scope).expect("scope the origin");

		let session = crate::lite::test_transport::ScriptedSession::new(namespace_response(VERSION, "x.hang").await);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		// Draft-18 can negotiate the cluster extension, so the subscriber waits for
		// the peer's SETUP before resolving advertisements; settle it as extension-off.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			scoped,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let prefix = subscriber.subscribe_prefixes().pop().expect("one prefix");
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		// Parks on the read after the scripted NAMESPACE is consumed.
		let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(stream, prefix));
		for _ in 0..100 {
			// The result is deliberately ignored: a regressed mount lands out of scope
			// and errors here, which the assertions below name far better than a poll
			// would.
			let _ = futures::poll!(run.as_mut());
			if routed_now(&consumer, "rootns/cam/x.hang").is_some() {
				break;
			}
			settle().await;
		}

		assert!(
			routed_now(&consumer, "rootns/cam/x.hang").is_some(),
			"the reply mounts under the root once",
		);
		assert!(
			routed_now(&consumer, "rootns/rootns/cam/x.hang").is_none(),
			"the root was applied twice",
		);
	}

	#[test]
	fn retiring_old_track_does_not_retire_reused_alias() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(13));

		assert_eq!(aliases.read().map.get(&7), Some(&Alias::Active(RequestId(11))));
	}

	/// A cancelled subscription leaves its alias behind, so the groups the publisher is
	/// still sending are discarded at once instead of stalling out the timeout and being
	/// reported as unknown (draft-19 section 11.1).
	#[tokio::test(start_paused = true)]
	async fn retired_alias_drops_late_groups_immediately() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(11));

		let runtime = crate::time::Clock::tokio();
		let resolve = resolve_track_alias(&runtime, aliases.consume(), 7);
		tokio::pin!(resolve);

		assert!(
			matches!(poll!(&mut resolve), std::task::Poll::Ready(Err(Error::Cancel))),
			"a retired alias must resolve without waiting on the timeout",
		);
	}

	/// A group arriving for a retired alias is the expected tail of our own cancellation, so
	/// the code it maps to has to say so. moq-lite's cancel encodes to 0, which on this wire
	/// is an internal failure, and reporting one to a publisher for a routine unsubscribe is
	/// what distorts its error handling.
	///
	/// Covers the error this path produces and the code it maps to, not the dispatch loop
	/// that sends it. `session::a_group_for_a_retired_alias_is_stopped_with_cancelled`
	/// drives that loop over a real receive stream.
	#[tokio::test(start_paused = true)]
	async fn a_retired_alias_maps_to_the_cancelled_code() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(11));

		let err = resolve_track_alias(&crate::time::Clock::tokio(), aliases.consume(), 7)
			.await
			.expect_err("a retired alias resolves to a cancellation");

		assert_eq!(
			crate::ietf::error::to_stream_code(&crate::StreamError::from(&err), Version::Draft20),
			crate::ietf::error::CANCELLED,
			"the code the dispatch loop maps this error onto",
		);
	}

	/// The publisher may point a retired alias at a new track, so a later SUBSCRIBE_OK
	/// reclaims it rather than colliding with the tombstone.
	#[test]
	fn subscribe_ok_reclaims_a_retired_alias() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(11));

		insert_track_alias(&aliases, 7, RequestId(13)).unwrap();

		assert_eq!(aliases.read().map.get(&7), Some(&Alias::Active(RequestId(13))));
		assert!(
			aliases.read().retired.is_empty(),
			"reclaiming an alias must drop its tombstone",
		);
	}

	/// An alias still serving a live subscription is not a tombstone, so a publisher
	/// pointing it at a second track is the duplicate the draft makes fatal.
	#[test]
	fn active_alias_rejects_a_second_track() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();

		assert!(matches!(
			insert_track_alias(&aliases, 7, RequestId(13)),
			Err(Error::Duplicate)
		));
	}

	/// Build a subscriber with `subscribes` pre-populated, so alias binding can be
	/// exercised without driving a whole SUBSCRIBE exchange.
	fn subscriber_with_tracks(
		tracks: &[(RequestId, &str, &str)],
	) -> Subscriber<crate::lite::test_transport::SinkSession> {
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The tests drive binding directly, so nothing spawns; leaking keeps the handle alive
		// without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			crate::lite::test_transport::SinkSession::new(Default::default()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft19,
			tasks,
			Default::default(),
		);

		{
			let mut state = subscriber.state.lock();
			for (request_id, broadcast, name) in tracks {
				state.subscribes.insert(
					*request_id,
					TrackState::new(
						track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), *name, None),
						Path::new(broadcast).to_owned(),
						kio::Producer::new(Fill::Done),
						None,
					),
				);
			}
		}

		subscriber
	}

	/// Draft-19 section 5.1 lets a publisher give several subscriptions to one track the
	/// same alias. Our filters are all LargestObject, so we cannot re-apply them to tell the
	/// groups apart, but that is one subscription's problem. Killing the session over a
	/// legal choice would take every other broadcast down with it.
	#[test]
	fn a_shared_alias_for_one_track_costs_only_that_subscription() {
		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "video"), (RequestId(13), "cam", "video")]);

		subscriber.register_alias(RequestId(11), 7).unwrap();

		assert!(
			matches!(subscriber.register_alias(RequestId(13), 7), Err(Error::Unsupported)),
			"a shared alias must not be reported as the fatal collision",
		);
	}

	/// One alias naming two different tracks is the collision section 11.1 makes fatal.
	#[test]
	fn an_alias_reused_for_another_track_is_fatal() {
		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "video"), (RequestId(13), "cam", "audio")]);

		subscriber.register_alias(RequestId(11), 7).unwrap();

		assert!(matches!(
			subscriber.register_alias(RequestId(13), 7),
			Err(Error::Duplicate)
		));
	}

	/// Same track name under a different broadcast is a different full track name, so it is
	/// a collision too.
	#[test]
	fn an_alias_reused_across_broadcasts_is_fatal() {
		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "video"), (RequestId(13), "screen", "video")]);

		subscriber.register_alias(RequestId(11), 7).unwrap();

		assert!(matches!(
			subscriber.register_alias(RequestId(13), 7),
			Err(Error::Duplicate)
		));
	}

	/// A FIN only says we will send nothing further; it is not a cancellation (draft-19
	/// section 3.3.2). A publisher holding an Established subscription keeps serving it
	/// until STOP_SENDING arrives on the direction it writes (sections 3.3.3 and 5.1.1),
	/// so a subscriber that only finishes leaves it feeding an alias forever. That is what
	/// turns a routine unsubscribe into an endless "unknown track alias" stream.
	#[tokio::test(start_paused = true)]
	async fn cancelling_a_subscription_stops_the_publisher() {
		for version in [Version::Draft16, Version::Draft20] {
			let log = cancel_a_subscription(version).await;
			// CANCELLED, not the moq-lite cancel code: 0 on this wire is INTERNAL_ERROR, so a
			// routine unsubscribe would read to the publisher as a fault on our side.
			assert_eq!(
				log.stops(),
				vec![crate::ietf::error::CANCELLED],
				"{version:?}: cancelling must STOP_SENDING the publisher's direction, not just FIN ours",
			);
			assert_ne!(
				crate::ietf::error::CANCELLED,
				crate::SessionError::Cancel.to_code(),
				"the two error spaces disagree; that is why this code is mapped separately",
			);
		}
	}

	/// Draft-14 through 16 have an UNSUBSCRIBE message, and draft-16 section 5.1.1 makes it
	/// the thing that lets the publisher destroy the subscription. Resetting the stream
	/// without it leaves a peer that predates draft-17 serving the track forever.
	#[tokio::test(start_paused = true)]
	async fn a_legacy_cancel_sends_unsubscribe() {
		let log = cancel_a_subscription(Version::Draft16).await;
		assert!(
			occurrences(&log, &[ietf::Unsubscribe::ID as u8]) > 0,
			"draft-16 cancels with UNSUBSCRIBE",
		);

		// Draft-17 removed the message, so sending one would be a protocol violation.
		let log = cancel_a_subscription(Version::Draft19).await;
		assert_eq!(
			occurrences(&log, &[ietf::Unsubscribe::ID as u8]),
			0,
			"draft-17+ has no UNSUBSCRIBE",
		);
	}

	/// A rejection and the last consumer leaving can both be ready when the task is next
	/// polled. The publisher destroyed the request when it sent the error, so treating that
	/// as abandonment would cancel a request that no longer exists and name a dead id back at
	/// a peer entitled to object. The answer wins.
	#[tokio::test(start_paused = true)]
	async fn a_ready_rejection_beats_local_abandonment() {
		const VERSION: Version = Version::Draft16;

		// A peer that rejects the subscribe outright.
		let rejection = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer =
				crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);
			writer.encode(&ietf::RequestError::ID).await.unwrap();
			writer
				.encode(&ietf::RequestError {
					request_id: Some(RequestId(1)),
					// DOES_NOT_EXIST, draft-16 section 13.4.2.
					error_code: 0x10,
					reason_phrase: "not found".into(),
					retry_interval: 0,
				})
				.await
				.unwrap();

			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(rejection);
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		// Drop the demand before the task runs, so the rejection and the unused wake are both
		// ready the first time the setup race is polled.
		drop(subscription);
		drop(track);
		drop(consumer);

		let serving = tokio::spawn(async move {
			subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;
		});

		tokio::time::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("run_subscribe did not finish")
			.unwrap();

		assert!(
			!control_message_types(&log, VERSION).contains(&ietf::Unsubscribe::ID),
			"a rejected request is already gone; cancelling it names a dead id at the peer",
		);
	}

	/// A publisher can be serving before its SUBSCRIBE_OK arrives, since data streams are
	/// independent of the request stream. If the last consumer leaves in that window, the
	/// subscriber still owes it a cancellation: walking away silently is what leaves it
	/// serving a track nobody reads.
	#[tokio::test(start_paused = true)]
	async fn abandoning_before_subscribe_ok_still_cancels() {
		const VERSION: Version = Version::Draft16;

		// A peer that accepts the stream and then says nothing at all.
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		let serving = tokio::spawn(async move {
			subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;
		});

		// Let the SUBSCRIBE go out. No SUBSCRIBE_OK is coming, so the subscription never
		// reaches Established on our side.
		settle().await;

		drop(subscription);
		drop(track);
		drop(consumer);

		tokio::time::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("run_subscribe parked waiting for a response that never came")
			.unwrap();

		assert!(
			occurrences(&log, &[ietf::Unsubscribe::ID as u8]) > 0,
			"a subscribe abandoned before SUBSCRIBE_OK must still be cancelled",
		);
		assert_eq!(
			log.stops(),
			vec![crate::ietf::error::CANCELLED],
			"and must stop the direction the publisher writes",
		);
	}

	/// A retraction does not disturb subscriptions already in flight, and one whose
	/// SUBSCRIBE_OK has not arrived yet is in flight too: the publisher may already be
	/// serving it. The broadcast ending in that window must not abort the track or cancel
	/// the subscription.
	#[tokio::test(start_paused = true)]
	async fn a_retraction_before_subscribe_ok_keeps_the_subscription() {
		const VERSION: Version = Version::Draft16;

		// A peer that accepts the stream and then says nothing at all.
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		let serving = tokio::spawn(async move {
			subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;
		});

		// Let the SUBSCRIBE go out, then retract the broadcast before any response.
		settle().await;
		producer.close();
		settle().await;

		assert!(
			!serving.is_finished(),
			"a retraction ended a subscription still in flight"
		);
		assert_eq!(
			occurrences(&log, &[ietf::Unsubscribe::ID as u8]),
			0,
			"a retraction must not cancel a subscription still in flight",
		);

		// The reader leaving is still what ends it.
		drop(subscription);
		drop(track);
		drop(consumer);
		tokio::time::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("run_subscribe parked after its reader left")
			.unwrap();
	}

	/// The control messages that actually reached the wire, by type id.
	///
	/// Decoding the framing rather than scanning for a byte: a type id is one varint among
	/// many, and a substring match would happily find one inside a length or a payload.
	/// A SUBSCRIBE_OK for the subscribe stream, framed as the peer sends it, with a Largest so
	/// the subscription reaches Established. The scripted session parks after it, keeping the
	/// subscription live so the steady-state loop runs.
	async fn subscribe_ok_bytes(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
		writer.encode(&ietf::SubscribeOk::ID).await.unwrap();
		writer
			.encode(&ietf::SubscribeOk {
				request_id: match version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(RequestId(1)),
					_ => None,
				},
				track_alias: 7,
				largest: Some(ietf::Location { group: 0, object: 0 }),
				properties: Default::default(),
			})
			.await
			.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// Replacing the client's request token re-presents it on a live subscription as a token-only
	/// SUBSCRIBE_UPDATE (MoQ request-token renewal), on a legacy draft (draft-14 trailing block)
	/// and a strict one (draft-18 message parameters). SUBSCRIBE_UPDATE carries the token from
	/// draft-14 on, unlike PUBLISH_NAMESPACE_UPDATE (draft-17+ only).
	#[tokio::test(start_paused = true)]
	async fn setting_a_new_request_token_re_presents_it_on_a_live_subscription() {
		for version in [Version::Draft14, Version::Draft18] {
			let first = bytes::Bytes::from_static(&[0x03, 0x00, b'a', b'a']);
			let second = bytes::Bytes::from_static(&[0x03, 0x00, b'b', b'b']);

			let session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok_bytes(version).await);
			let log = session.log.clone();
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let token = crate::RequestToken::new(Some(first.clone()));
			let mut subscriber = Subscriber::new(
				crate::time::Clock::tokio(),
				session,
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(None, false),
				None,
				peer::PeerSetup::default(),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			)
			.with_request_token(token.clone());

			let producer = crate::broadcast::Info::default().produce();
			let mut dynamic = producer.dynamic();
			let consumer = producer.consume();
			let track = consumer.track("video").unwrap();
			// Held for the test so the track never reads as unused (which would cancel it).
			let subscription = track.subscribe(None);
			let request = dynamic.requested_track().await.expect("no track requested");

			let serving = tokio::spawn(async move {
				subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;
			});

			// The initial token rides the SUBSCRIBE.
			let mut initial = false;
			for _ in 0..200 {
				if occurrences(&log, &first) >= 1 {
					initial = true;
					break;
				}
				settle().await;
			}
			assert!(initial, "{version}: the initial token must ride the SUBSCRIBE");

			// Replacing it re-presents the new token on the live subscription as a SUBSCRIBE_UPDATE.
			token.set(Some(second.clone()));
			let mut renewed = false;
			for _ in 0..200 {
				if occurrences(&log, &second) >= 1 {
					renewed = true;
					break;
				}
				settle().await;
			}
			assert!(
				renewed,
				"{version}: a replaced token must be re-presented on the live subscription"
			);

			// The renewal leaves the subscription's range alone: draft-14 restates where it
			// began (the object after SUBSCRIBE_OK's Largest Object, {0, 0} here), and draft-15+
			// omits the filter, priority and forward flag so they keep their values.
			let draft14 = version == Version::Draft14;
			let mut expected = Vec::new();
			for request_id in 0..16 {
				let log = crate::lite::test_transport::Log::default();
				let mut writer =
					crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
				writer.encode(&ietf::SubscribeUpdate::ID).await.unwrap();
				writer
					.encode(&ietf::SubscribeUpdate {
						request_id: RequestId(request_id),
						subscription_request_id: draft14.then_some(RequestId(1)),
						start_location: ietf::Location { group: 0, object: 1 },
						end_group: 0,
						// The SUBSCRIBE went out at the lowest wire priority; the renewal restates it.
						subscriber_priority: draft14.then_some(0xff),
						forward: draft14.then_some(true),
						filter: None,
						authorization_token: Some(second.clone()),
					})
					.await
					.unwrap();
				expected.push(log.writes.lock().unwrap().clone());
			}
			assert!(
				expected.iter().any(|bytes| occurrences(&log, bytes) == 1),
				"{version}: the renewal must keep the subscription's range"
			);

			drop(subscription);
			drop(track);
			drop(consumer);
			serving.abort();
		}
	}

	/// A token-bearing client does not self-censor on its connection grant (B1b, quest Goal): the
	/// subscriber sends a SUBSCRIBE for a path its connection grant does not cover, carrying the
	/// token; a token-less client with the same grant rejects it locally, as before.
	#[tokio::test(start_paused = true)]
	async fn a_token_bearing_client_subscribes_outside_its_connection_grant() {
		const VERSION: Version = Version::Draft18;
		let token = bytes::Bytes::from_static(&[0x03, 0x00, b'o', b'k']);

		// A connection grant of "other", which does not cover "room/x". Held for the run.
		fn seed_auth() -> (crate::auth::Handle, crate::auth::Token) {
			let auth = crate::auth::Handle::new(true);
			let cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
			auth.granted(
				0,
				crate::auth::Grant {
					publish: crate::Patterns::new(),
					subscribe: crate::Pattern::subtree("other").unwrap().into(),
					expires: None,
				},
			);
			(auth, cred)
		}
		assert!(
			!seed_auth().0.allows(crate::auth::Direction::Subscribe, "room/x"),
			"the connection grant must not cover the subscribed path"
		);

		// With a token, the SUBSCRIBE for room/x reaches the wire carrying the token.
		let session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok_bytes(VERSION).await);
		let log = session.log.clone();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let (auth, _cred) = seed_auth();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		)
		.with_auth(auth)
		.with_request_token(crate::RequestToken::new(Some(token.clone())));
		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");
		let serving = tokio::spawn(async move {
			subscriber.run_subscribe(Path::new("room/x"), dynamic, request).await;
		});
		let mut sent = false;
		for _ in 0..200 {
			if occurrences(&log, &token) >= 1 {
				sent = true;
				break;
			}
			settle().await;
		}
		assert!(
			sent,
			"a token-bearing client must subscribe outside its connection grant"
		);
		drop(subscription);
		drop(track);
		drop(consumer);
		serving.abort();

		// Without a token, the same grant rejects the subscribe locally: no SUBSCRIBE is sent.
		let session2 = crate::lite::test_transport::ScriptedSession::new(subscribe_ok_bytes(VERSION).await);
		let log2 = session2.log.clone();
		let (tasks2, _task_set2) = crate::util::TaskSet::new();
		let (auth2, _cred2) = seed_auth();
		let mut subscriber2 = Subscriber::new(
			crate::time::Clock::tokio(),
			session2,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks2,
			Default::default(),
		)
		.with_auth(auth2);
		let producer2 = crate::broadcast::Info::default().produce();
		let mut dynamic2 = producer2.dynamic();
		let consumer2 = producer2.consume();
		let track2 = consumer2.track("video").unwrap();
		let subscription2 = track2.subscribe(None);
		let request2 = dynamic2.requested_track().await.expect("no track requested");
		let serving2 = tokio::spawn(async move {
			subscriber2.run_subscribe(Path::new("room/x"), dynamic2, request2).await;
		});
		for _ in 0..80 {
			settle().await;
		}
		assert!(
			!control_message_types(&log2, VERSION).contains(&ietf::Subscribe::ID),
			"a token-less client rejects a subscribe outside its grant locally"
		);
		drop(subscription2);
		drop(track2);
		drop(consumer2);
		serving2.abort();
	}

	fn control_message_types(log: &crate::lite::test_transport::Log, version: Version) -> Vec<u64> {
		use crate::coding::Decode;

		let writes = log.writes.lock().unwrap().clone();
		let mut buf = writes.as_slice();
		let mut types = Vec::new();

		while !buf.is_empty() {
			let Ok(type_id) = u64::decode(&mut buf, version) else {
				break;
			};
			let Ok(size) = u16::decode(&mut buf, version) else {
				break;
			};
			if buf.len() < size as usize {
				break;
			}
			buf = &buf[size as usize..];
			types.push(type_id);
		}

		types
	}

	/// Drafts 14-16 carry every request over `ControlStreamAdapter`'s virtual streams, so a
	/// cancellation only counts if it traverses the mux and reaches the real control stream
	/// writer. A test that drives a direct stream proves the subscriber's own logic and
	/// nothing about the path production takes: the virtual writer's reset is a no-op and
	/// its close returns as soon as the bytes are queued, so an adapter that dropped them
	/// would look identical.
	#[tokio::test(start_paused = true)]
	async fn a_legacy_cancel_reaches_the_control_stream() {
		const VERSION: Version = Version::Draft16;

		// A peer that opens the control stream and then says nothing, so the subscribe is
		// abandoned before it is accepted and cancelled from there.
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();

		let control = Control::new(None, false);
		let adapter = super::super::adapter::ControlStreamAdapter::new(session.clone(), control.clone(), VERSION);

		// The one real bidi everything is multiplexed onto.
		let control_stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		let running = adapter.clone();
		let (_goaway_handle, goaway) = crate::goaway::Handle::new(true);
		tokio::spawn(async move {
			let _ = running.run(control_stream.reader, control_stream.writer, goaway).await;
		});

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			adapter,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			control,
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		let serving = tokio::spawn(async move {
			subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;
		});

		settle().await;
		drop(subscription);
		drop(track);
		drop(consumer);

		tokio::time::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("run_subscribe did not finish")
			.unwrap();

		// Let the adapter's writer task drain the queue onto the control stream.
		settle().await;

		let types = control_message_types(&log, VERSION);
		assert!(
			types.contains(&ietf::Subscribe::ID),
			"the SUBSCRIBE reached the control stream: {types:?}"
		);
		assert!(
			types.contains(&ietf::Unsubscribe::ID),
			"the UNSUBSCRIBE must traverse the adapter to the control stream, not stop at the \
			 virtual writer: {types:?}"
		);
	}

	/// Establish a subscription on `version`, then drop its last consumer, and hand back
	/// what the session recorded on the way out.
	async fn cancel_a_subscription(version: Version) -> crate::lite::test_transport::Log {
		cancel_a_subscription_inner(version, false).await
	}

	/// A publisher on draft-14 through 16 that legally hands a second subscription to one
	/// track the alias the first already holds. We cannot demux that, so we walk away from
	/// the new subscription. Those versions carry requests over the control stream adapter,
	/// whose virtual streams drop silently, so UNSUBSCRIBE is the only way the publisher
	/// ever learns to stop serving it.
	#[tokio::test(start_paused = true)]
	async fn a_legacy_shared_alias_is_unsubscribed() {
		let log = cancel_a_subscription_inner(Version::Draft16, true).await;

		assert!(
			occurrences(&log, &[ietf::Unsubscribe::ID as u8]) > 0,
			"abandoning a shared alias must still tell the publisher to stop",
		);
	}

	/// Writing the UNSUBSCRIBE is not the same as delivering it. A stream that has only been
	/// finished is still retransmitting, so the writer's Drop reset would discard the message
	/// before the peer read it. Closing consumes the writer, which is what removes that
	/// fallback, so a reset here means the cancellation never landed.
	#[tokio::test(start_paused = true)]
	async fn cancelling_does_not_reset_away_the_unsubscribe() {
		for version in [Version::Draft16, Version::Draft20] {
			let log = cancel_a_subscription(version).await;
			assert!(
				log.resets().is_empty(),
				"{version:?}: the send side must be closed, not reset out from under the cancellation",
			);
		}
	}

	/// When `conflict` is set, an alias-7 binding for the same full track name is seeded
	/// first, so the subscription under test loses the race to bind it.
	async fn cancel_a_subscription_inner(version: Version, conflict: bool) -> crate::lite::test_transport::Log {
		// A peer that accepts the subscription, binding alias 7, then says nothing more.
		let subscribe_ok = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer =
				crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
			writer.encode(&ietf::SubscribeOk::ID).await.unwrap();
			writer
				.encode(&ietf::SubscribeOk {
					request_id: match version {
						Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(RequestId(0)),
						_ => None,
					},
					track_alias: 7,
					largest: None,
					properties: Default::default(),
				})
				.await
				.unwrap();

			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok);
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);

		if conflict {
			let holder = RequestId(999);
			let mut state = subscriber.state.lock();
			state.subscribes.insert(
				holder,
				TrackState {
					alias: Some(7),
					..TrackState::new(
						track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", None),
						Path::new("broadcast").to_owned(),
						kio::Producer::new(Fill::Done),
						None,
					)
				},
			);
			insert_track_alias(&state.aliases, 7, holder).unwrap();
		}

		// A consumer asking for a track is what dispatches a request to the session.
		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		// A handle on the same state the spawned task mutates, so the test can prove the
		// subscription reached Established rather than assume it.
		let probe = subscriber.clone();

		let serving = tokio::spawn(async move {
			subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;
		});

		// Let the SUBSCRIBE go out and the SUBSCRIBE_OK come back, so the subscription is
		// Established when we walk away from it.
		settle().await;

		// Without this the test would still pass if SUBSCRIBE_OK never landed, and it would
		// then be asserting against a subscription that was never established.
		assert!(
			matches!(probe.state.lock().aliases.read().map.get(&7), Some(Alias::Active(_))),
			"{version:?}: alias 7 must be bound before we cancel",
		);

		// The last consumer leaves: nothing wants this track any more.
		drop(subscription);
		drop(track);
		drop(consumer);

		tokio::time::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("run_subscribe did not finish")
			.unwrap();

		log
	}

	/// Establish a draft-20 subscription against a SUBSCRIBE_OK carrying `largest`, and
	/// report whether a fill is still outstanding once it is accepted.
	async fn fill_after_subscribe_ok(largest: Option<ietf::Location>, priority: Option<u8>) -> bool {
		let version = Version::Draft20;

		let subscribe_ok = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer =
				crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
			writer.encode(&ietf::SubscribeOk::ID).await.unwrap();
			writer
				.encode(&ietf::SubscribeOk {
					request_id: None,
					track_alias: 7,
					largest,
					properties: ietf::Properties {
						priority,
						..Default::default()
					},
				})
				.await
				.unwrap();

			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let _subscription = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");

		let probe = subscriber.clone();
		let serving = tokio::spawn(async move {
			subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;
		});

		settle().await;

		let outstanding = {
			let state = probe.state.lock();
			let track = state
				.subscribes
				.values()
				.next()
				.expect("the subscription is registered");
			assert_eq!(
				track.producer.as_ref().unwrap().publisher_priority(),
				255 - priority.unwrap_or(128),
				"SUBSCRIBE_OK priority must be committed before alias registration"
			);
			let fill = track.fill.read();
			fill.outstanding()
		};

		serving.abort();
		outstanding
	}

	/// The publisher opens no fetch stream for an empty range, so a fill against a track
	/// with no content is owed nothing. Leaving it outstanding would withhold every later
	/// group behind a head that is never coming.
	#[tokio::test(start_paused = true)]
	async fn an_empty_track_settles_the_fill() {
		assert!(
			!fill_after_subscribe_ok(None, Some(37)).await,
			"no LARGEST_OBJECT means no content, so no fill is owed"
		);
	}

	/// A track with content does owe one, so the fill stays outstanding until its fetch
	/// stream arrives.
	#[tokio::test(start_paused = true)]
	async fn a_track_with_content_still_awaits_its_fill() {
		assert!(
			fill_after_subscribe_ok(Some(ietf::Location { group: 3, object: 4 }), Some(37)).await,
			"a fetch stream is still owed"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn an_older_peer_without_priority_property_uses_wire_default() {
		assert!(!fill_after_subscribe_ok(None, None).await);
	}

	/// Tombstones are bounded: a session churning through subscriptions must not
	/// accumulate one entry per alias it ever used.
	#[test]
	fn retired_aliases_are_capped() {
		let aliases = TrackAliases::default();

		for i in 0..(RETIRED_ALIAS_CAPACITY as u64 + 10) {
			insert_track_alias(&aliases, i, RequestId(i)).unwrap();
			retire_track_alias(&aliases, i, RequestId(i));
		}

		let table = aliases.read();
		assert_eq!(table.retired.len(), RETIRED_ALIAS_CAPACITY);
		assert_eq!(table.map.len(), RETIRED_ALIAS_CAPACITY);
		assert!(!table.map.contains_key(&0), "the oldest tombstone is forgotten first");
	}

	/// moq-transport carries no hop ids, so a peer's broadcasts are named by the
	/// connection's own random stamp. An identity assigned via `Client::with_peer_hop`
	/// is stored as `via` for split-horizon and never written into the chain.
	#[tokio::test]
	async fn assigned_peer_hop_attributes_announces() {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let assigned = crate::Hop::new(777).unwrap();

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			origin,
			Control::new(None, false),
			Some(assigned),
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft14,
			tasks,
			Default::default(),
		);

		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		subscriber
			.start_announce(crate::Path::new("room/host").to_owned(), advert)
			.unwrap();

		let mut announced = consumer.announced();
		let route = announced.assert_next_active("room/host");
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![subscriber.stamp, crate::Hop::UNKNOWN]);
		assert_ne!(subscriber.stamp, crate::Hop::UNKNOWN);
		assert!(route.is_anonymous(), "the 0 after the stamp still ranks it as unknown");
		assert_ne!(subscriber.stamp, assigned, "the assigned identity stays off the chain");

		let mut hidden = consumer.excluding(assigned).announced();
		hidden.assert_next_wait();
	}

	/// Both directions of a sync target point at one relay, which has no way to tell
	/// our two connections apart on a wire with no hop ids and so offers our own
	/// broadcast back to us. That reflection must not look like a rival publisher
	/// claiming the path: taking it over would leave only a route we refuse to
	/// advertise back to the peer, and the publish direction would withdraw the
	/// announce it just made.
	#[tokio::test]
	async fn reflected_announce_does_not_evict_the_source_we_publish() {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let peer = crate::Hop::new(777).unwrap();
		let self_origin = crate::Hop::new(1).unwrap();

		let origin = crate::origin::Config::new(self_origin).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// The publish direction: an origin handle scoped to the peer, which is what
		// `Client::with_peer_hop` hands the publisher. Holding its announce stream
		// is what records that the peer has been offered these paths.
		let mut publishing = consumer.clone().excluding(peer).announced();

		// What we are publishing to the peer: a real upstream route.
		let upstream = crate::Hops::try_from(vec![crate::Hop::new(7).unwrap()]).unwrap();
		let _source = origin
			.announce("room/host", crate::origin::Route::default().with_hops(upstream.clone()))
			.unwrap();
		announced.assert_next_active("room/host");
		let _advertised = publishing.assert_next_active("room/host");

		// The peer reflects it back over the subscribe direction, which carries no
		// hop chain of its own.
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			origin,
			Control::new(None, false),
			Some(peer),
			peer::PeerSetup::default(),
			self_origin,
			None,
			Version::Draft14,
			tasks,
			Default::default(),
		);
		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		subscriber
			.start_announce(crate::Path::new("room/host").to_owned(), advert)
			.unwrap();

		// No announce churn: the upstream route stays the best one, on both cursors.
		announced.assert_next_wait();
		publishing.assert_next_wait();
		let route = routed_now(&consumer, "room/host").expect("still routed");
		assert_eq!(route.hops, upstream);
	}

	/// Two sessions assigned the same identity still stamp their own first hop, since
	/// a wire with no hop ids cannot say the content continued: the reconnect reads as a
	/// new source, while split-horizon keeps filtering on the shared identity.
	#[tokio::test]
	async fn reconnecting_peer_is_a_new_first_hop() {
		let peer = crate::Hop::new(777).unwrap();
		let self_origin = crate::Hop::new(1).unwrap();

		let origin = crate::origin::Config::new(self_origin).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let connect = || {
			let (tasks, task_set) = crate::util::TaskSet::new();
			std::mem::forget(task_set);
			let mut subscriber = Subscriber::new(
				crate::time::Clock::tokio(),
				crate::lite::test_transport::SinkSession::new(Default::default()),
				origin.clone(),
				Control::new(None, false),
				Some(peer),
				peer::PeerSetup::default(),
				self_origin,
				None,
				Version::Draft14,
				tasks,
				Default::default(),
			);
			let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
			subscriber
				.start_announce(crate::Path::new("room/host").to_owned(), advert)
				.unwrap();
			subscriber
		};

		let first = connect();
		let first_stamp = first.stamp;
		let route = announced.assert_next_active("room/host");
		assert_eq!(route.hops.iter().next(), Some(&first_stamp));

		// The peer reconnects before the old session is retired, under its own stamp.
		let second = connect();
		assert_ne!(second.stamp, first_stamp);
		assert!(routed_now(&consumer, "room/host").is_some());

		// Neither route is offered back to the peer they both came from.
		consumer.excluding(peer).announced().assert_next_wait();
		drop(first);
	}

	fn cluster_subscriber(
		self_origin: crate::Hop,
	) -> (
		Subscriber<crate::lite::test_transport::SinkSession>,
		crate::origin::Producer,
	) {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let origin = crate::origin::Config::new(self_origin).produce();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The set only drains announce-serving tasks; the tests here drive the model
		// directly, so leaking it keeps the handles alive without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			origin.clone(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			self_origin,
			None,
			Version::Draft19,
			tasks,
			Default::default(),
		);
		(subscriber, origin)
	}

	/// The current best route covering `path`, if any (synchronous peek).
	fn routed_now(consumer: &crate::origin::Consumer, path: &str) -> Option<crate::origin::Route> {
		use futures::FutureExt;
		consumer.routed(path).now_or_never().flatten()
	}

	fn hop_path(ids: &[u64]) -> cluster::HopPath {
		let hops = ids.iter().map(|&id| crate::Hop::new(id).unwrap()).collect::<Vec<_>>();
		cluster::HopPath::new(crate::Hops::try_from(hops).unwrap())
	}

	/// A negotiated advertisement carries the whole path and its accumulated cost, and
	/// the receiving relay charges its own link on top (saturating, so an absurd
	/// upstream value ranks last rather than wrapping to best).
	#[tokio::test]
	async fn cluster_advert_becomes_a_route_with_the_link_charged() {
		let (subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();

		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(3),
		};
		let advert = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		};

		let advertised = subscriber.route(Some(&advert), &peer).expect("route");
		assert_eq!(
			advertised.route.cost.warm, 7,
			"the link's price is added to the advertised cost"
		);
		assert_eq!(advertised.route.hops, hop_path(&[7, 9]).hops().clone());

		let mut subscriber = subscriber;
		subscriber
			.start_announce(crate::Path::new("room/host").to_owned(), advertised)
			.unwrap();

		let route = routed_now(&consumer, "room/host").expect("routed");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 9]);
		assert_eq!(route.cost.warm, 7);
	}

	/// An advertisement whose path already contains our own Hop ID looped back:
	/// forwarding it would extend the loop and subscribing through it would route us
	/// back to ourselves. Hop ID 0 identifies nothing, so it is never a loop.
	#[test]
	fn cluster_advert_loop_is_discarded() {
		let (subscriber, _origin) = cluster_subscriber(crate::Hop::new(5).unwrap());
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};

		let looped = cluster::Advert {
			hops: hop_path(&[7, 5, 9]),
			cost: 0,
		};
		assert!(subscriber.route(Some(&looped), &peer).is_none());

		let clean = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 0,
		};
		assert!(subscriber.route(Some(&clean), &peer).is_some());
	}

	/// An unpriced link costs 1, so an unpriced mesh accumulates a cost equal to the
	/// hop count and degenerates to shortest-path routing.
	#[test]
	fn unpriced_link_costs_one() {
		let (subscriber, _origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};
		let advert = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 2,
		};
		assert_eq!(subscriber.route(Some(&advert), &peer).unwrap().route.cost.warm, 3);

		// Zero is meaningful and distinct from absent: a free link adds nothing.
		let free = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		assert_eq!(subscriber.route(Some(&advert), &free).unwrap().route.cost.warm, 2);
	}

	/// A namespace stream that ends with advertisements still live detaches them, so
	/// the broadcast closes rather than staying announced over a dead stream. True
	/// even of a clean FIN, since closing the stream retracts nothing: the protocol
	/// has NAMESPACE_DONE for that. moq-lite already behaves this way (its route map
	/// is a local whose guards drop), which `lite::subscriber` pins separately.
	///
	/// Driven through the real exit path rather than by calling `stop_announce`: a test
	/// that picked the detach itself would still pass if the stream stopped using it.
	#[tokio::test(start_paused = true)]
	async fn a_lost_namespace_stream_closes_the_broadcast() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		// The peer answers, advertises one namespace, then the stream ends without ever
		// retracting it.
		let session = crate::lite::test_transport::ScriptedSession::eof(namespace_response(VERSION, "x.hang").await);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		// Draft-18 can negotiate the extension, so the read loop waits for the peer's
		// SETUP before parsing a NAMESPACE; settle it as extension-off.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		subscriber
			.run_subscribe_namespace(stream, crate::Path::new("").to_owned())
			.await
			.expect("a clean FIN is not an error");
		settle().await;

		assert!(
			routed_now(&consumer, "x.hang").is_none(),
			"an ended stream must retract the route, not leave a stale one",
		);
	}

	/// A subscriber whose session grant covers only `other`, with an app auth acceptor
	/// wired in, so a PUBLISH_NAMESPACE for `room/alice` falls to its request token. The
	/// subscribe stream's reader is scripted with `first_script` (a REQUEST_UPDATE, for the
	/// renewal test). Returns the presented credential to keep it alive.
	fn auth_announce_harness(
		version: Version,
		first_script: Vec<u8>,
	) -> (
		Subscriber<crate::lite::test_transport::ScriptedSession>,
		crate::auth::Requests,
		crate::auth::Token,
		origin::Consumer,
		crate::lite::test_transport::ScriptedSession,
	) {
		auth_announce_harness_on(
			version,
			crate::lite::test_transport::ScriptedSession::per_stream(vec![first_script]),
		)
	}

	/// [`auth_announce_harness`] over a given session, such as one that finishes the stream
	/// after its script.
	fn auth_announce_harness_on(
		version: Version,
		session: crate::lite::test_transport::ScriptedSession,
	) -> (
		Subscriber<crate::lite::test_transport::ScriptedSession>,
		crate::auth::Requests,
		crate::auth::Token,
		origin::Consumer,
		crate::lite::test_transport::ScriptedSession,
	) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let auth = crate::auth::Handle::new(true);
		let requests = auth.requests().unwrap();
		let cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
		auth.granted(
			0,
			crate::auth::Grant {
				publish: crate::Patterns::new(),
				subscribe: crate::Pattern::subtree("other").unwrap().into(),
				expires: None,
			},
		);
		assert!(
			!auth.allows(crate::auth::Direction::Subscribe, "room/alice"),
			"the session grant must not cover the announced path"
		);

		let subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.with_auth(auth);

		(subscriber, requests, cred, consumer, session)
	}

	/// A grant to publish everything, lapsing in `secs` (or never), on the subscriber's clock:
	/// what an announcing peer's token must carry.
	fn publish_grant_expiring(runtime: &crate::time::Clock, secs: Option<u64>) -> crate::auth::Grant {
		crate::auth::Grant {
			publish: crate::Pattern::all().into(),
			subscribe: crate::Patterns::new(),
			expires: secs.map(|s| {
				crate::runtime::Timers::now(runtime)
					.checked_add(std::time::Duration::from_secs(s))
					.unwrap()
			}),
		}
	}

	/// A Token structure value that decodes via `token::decode_value` (USE_VALUE, kind 300).
	fn announce_token() -> bytes::Bytes {
		bytes::Bytes::from_static(&[0x03, 0x81, 0x2c, 0x00, 0xff])
	}

	fn token_publish_namespace() -> ietf::PublishNamespace<'static> {
		ietf::PublishNamespace {
			request_id: RequestId(1),
			track_namespace: crate::Path::new("room/alice"),
			cluster: None,
			authorization_token: Some(announce_token()),
		}
	}

	/// One REQUEST_UPDATE on the announce stream carrying a fresh token, framed as the peer
	/// sends it.
	async fn publish_namespace_update_with_token(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
		let msg = ietf::PublishNamespaceUpdate {
			request_id: RequestId(3),
			hops: None,
			cost: None,
			authorization_token: Some(announce_token()),
		};
		writer.encode(&ietf::PublishNamespaceUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// One REQUEST_UPDATE on the announce stream carrying both a fresh token and new cluster
	/// parameters (HOP_PATH/ROUTE_COST), framed as the peer sends it.
	async fn publish_namespace_update_with_token_and_cluster(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
		let msg = ietf::PublishNamespaceUpdate {
			request_id: RequestId(3),
			hops: Some(hop_path(&[7, 9])),
			cost: Some(0),
			authorization_token: Some(announce_token()),
		};
		writer.encode(&ietf::PublishNamespaceUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// A request token on a PUBLISH_NAMESPACE the session grant does not cover authorizes
	/// the announce: the subscriber verifies it through the acceptor and attaches the route.
	#[tokio::test]
	async fn a_publish_namespace_token_authorizes_an_uncovered_announce() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, mut requests, _cred, consumer, session) = auth_announce_harness(VERSION, Vec::new());
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let acceptor = async {
			let mut held = Vec::new();
			loop {
				let request = requests.next().await.expect("a request");
				held.push(request.accept(crate::auth::Grant::all()));
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut announced = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some() {
				announced = true;
				break;
			}
			settle().await;
		}
		assert!(announced, "a valid request token must authorize the announce");
	}

	/// A refused request token is answered UNAUTHORIZED and the announce is not attached; the
	/// session is untouched.
	#[tokio::test]
	async fn a_refused_publish_namespace_token_is_not_announced() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, mut requests, _cred, consumer, session) = auth_announce_harness(VERSION, Vec::new());
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let acceptor = async {
			let request = requests.next().await.expect("a request");
			request.reject(crate::SessionError::Unauthorized, "no");
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut ended = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(run.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(ended, "a refused announce ends its own stream");
		assert!(
			routed_now(&consumer, "room/alice").is_none(),
			"a refused token must not attach the route"
		);
	}

	/// A REQUEST_UPDATE the acceptor renews keeps a token-authorized announce alive past the
	/// old grant's expiry: the subscriber re-verifies the token off the announce stream and
	/// re-arms the deadline.
	#[tokio::test(start_paused = true)]
	async fn a_publish_namespace_renewal_extends_past_the_old_expiry() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, mut requests, _cred, consumer, session) =
			auth_announce_harness(VERSION, publish_namespace_update_with_token(VERSION).await);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let answered = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let answered = answered.clone();
			async move {
				let mut held = Vec::new();
				// The first grant lapses in 60s; the renewal never expires.
				let first = requests.next().await.expect("a request");
				held.push(first.accept(publish_grant_expiring(&rt, Some(60))));
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let renewal = requests.next().await.expect("a renewal");
				held.push(renewal.accept(publish_grant_expiring(&rt, None)));
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the announce ended during setup"
			);
			if answered.load(std::sync::atomic::Ordering::Relaxed) >= 2 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			answered.load(std::sync::atomic::Ordering::Relaxed),
			2,
			"acceptor never answered both tokens"
		);
		// Let the loop apply the renewal it read.
		for _ in 0..20 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending());
			settle().await;
		}

		// Past the original 60s expiry: the renewal re-armed the deadline, so the announce
		// stays attached.
		tokio::time::advance(std::time::Duration::from_secs(120)).await;
		for _ in 0..50 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"renewal did not extend the announce"
			);
			settle().await;
		}
		assert!(
			routed_now(&consumer, "room/alice").is_some(),
			"the renewed announce must still be attached"
		);
	}
	/// A REQUEST_UPDATE carrying both a fresh token and cluster parameters applies both: the
	/// renewal re-arms the grant and the HOP_PATH/ROUTE_COST re-route the advertisement,
	/// answered with the renewal's single response. Before the fix the token branch
	/// short-circuited the loop and the cluster parameters on the same update were dropped.
	#[tokio::test(start_paused = true)]
	async fn an_update_applies_both_a_renewal_and_cluster_params() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, mut requests, _cred, consumer, session) =
			auth_announce_harness(VERSION, publish_namespace_update_with_token_and_cluster(VERSION).await);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		// A negotiated peer over a free link, so the advertised cost is the route's warm cost.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		// The announce arrives already routed at cost 4; the update re-routes it to cost 0.
		let mut initial = token_publish_namespace();
		initial.cluster = Some(cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		});

		let acceptor = {
			let rt = rt.clone();
			async move {
				let mut held = Vec::new();
				let first = requests.next().await.expect("a request");
				held.push(first.accept(publish_grant_expiring(&rt, Some(60))));
				let renewal = requests.next().await.expect("a renewal");
				held.push(renewal.accept(publish_grant_expiring(&rt, None)));
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(stream, initial, peer, None));

		let mut rerouted = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some_and(|route| route.cost.warm == 0) {
				rerouted = true;
				break;
			}
			settle().await;
		}
		assert!(
			rerouted,
			"the update's cluster parameters must re-route the announce (cost 4 to 0), not be dropped by the renewal"
		);
	}

	/// One PUBLISH_NAMESPACE REQUEST_UPDATE carrying a fresh token, keyed to its own Request ID,
	/// optionally changing the HOP_PATH and/or ROUTE_COST, framed as the peer sends it.
	async fn publish_namespace_update_token_rid(
		version: Version,
		rid: u64,
		hops: Option<cluster::HopPath>,
		cost: Option<u64>,
	) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);
		let msg = ietf::PublishNamespaceUpdate {
			request_id: RequestId(rid),
			hops,
			cost,
			authorization_token: Some(announce_token()),
		};
		writer.encode(&ietf::PublishNamespaceUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// Two announce renewals that arrive while a first renewal's verdict is still pending are
	/// each verified, and each one's cluster delta is applied, not collapsed. The middle update
	/// (0x04) changes only ROUTE_COST (4 to 1) and the last (0x05) only HOP_PATH, so the final
	/// cost shows the middle delta survived; the single slot this replaced dropped 0x04,
	/// leaving the cost at the initial 4 and never verifying its token.
	#[tokio::test(start_paused = true)]
	async fn two_announce_renewals_buffered_behind_a_pending_verdict_each_apply_and_answer() {
		const VERSION: Version = Version::Draft18;

		let mut script = publish_namespace_update_token_rid(VERSION, 0x03, None, None).await;
		script.extend(publish_namespace_update_token_rid(VERSION, 0x04, None, Some(1)).await);
		script.extend(publish_namespace_update_token_rid(VERSION, 0x05, Some(hop_path(&[7, 11])), None).await);
		let (mut subscriber, mut requests, _cred, consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		// A negotiated peer over a free link, so the advertised cost is the route's warm cost.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		// The announce arrives routed at cost 4; the buffered updates re-route it.
		let mut initial = token_publish_namespace();
		initial.cluster = Some(cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		});

		// Hold the first renewal (0x03) so the next two (0x04, 0x05) are read and buffered while
		// its verdict is pending: the window the single slot used to collapse.
		let release = std::sync::Arc::new(tokio::sync::Notify::new());
		let answered = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let rt = rt.clone();
			let answered = answered.clone();
			let release = release.clone();
			async move {
				let first = requests.next().await.expect("a request");
				let mut held = vec![first.accept(publish_grant_expiring(&rt, None))];
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let a = requests.next().await.expect("a renewal");
				release.notified().await;
				held.push(a.accept(publish_grant_expiring(&rt, None)));
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				loop {
					let renewal = requests.next().await.expect("a renewal");
					held.push(renewal.accept(publish_grant_expiring(&rt, None)));
					answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				}
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(stream, initial, peer, None));

		for _ in 0..200 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			settle().await;
		}
		release.notify_one();
		let mut both = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if answered.load(std::sync::atomic::Ordering::Relaxed) >= 4
				&& routed_now(&consumer, "room/alice").is_some_and(|route| route.cost.warm == 1)
			{
				both = true;
				break;
			}
			settle().await;
		}
		assert!(
			both,
			"both buffered updates must each be verified (4 total) and each delta applied: the \
			 middle update's cost (1), not the single-slot survivor's initial 4"
		);
	}

	/// Draft-19 advertises MAX_REQUEST_UPDATES, so a peer that leaves more outstanding than that on
	/// an announce stream broke the negotiated limit: draft-19 section 10.3.1.7 closes the session
	/// with TOO_MANY_REQUEST_UPDATES. run_publish_namespace_updates must close explicitly (the
	/// dispatcher does not close on an Error::Session), and surface that error too.
	#[tokio::test(start_paused = true)]
	async fn announce_renewals_past_the_advertised_limit_close_the_session() {
		const VERSION: Version = Version::Draft19;

		// Exactly one past the limit: the held renewal plus MAX_REQUEST_UPDATES queued behind it.
		let last = 0x40 + request_update::MAX_REQUEST_UPDATES;
		let mut script = Vec::new();
		for rid in 0x40..=last {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		// Accept the initial token, then never answer a renewal, so the buffer only grows.
		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut result = None;
		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			if let std::task::Poll::Ready(r) = futures::poll!(run.as_mut()) {
				result = Some(r);
				break;
			}
			settle().await;
		}
		let Some(Err(err)) = result else {
			panic!("flooding past the advertised limit must end the announce with an error: {result:?}");
		};
		assert_eq!(
			SessionError::from(&err),
			SessionError::TooManyRequestUpdates,
			"the flood must surface TOO_MANY_REQUEST_UPDATES"
		);
		assert!(
			session
				.log
				.closes()
				.iter()
				.any(|close| close.0 == SessionError::TooManyRequestUpdates.to_code()),
			"the flood must close the session with TOO_MANY_REQUEST_UPDATES: {:?}",
			session.log.closes()
		);
	}

	/// Exactly MAX_REQUEST_UPDATES outstanding (the one being verified plus the queue) is within
	/// the advertised limit, so a draft-19 peer holding that many is not faulted: the announce
	/// keeps running and the session stays up.
	#[tokio::test(start_paused = true)]
	async fn announce_renewals_at_the_advertised_limit_keep_the_announce() {
		const VERSION: Version = Version::Draft19;

		let last = 0x40 + request_update::MAX_REQUEST_UPDATES - 1;
		let mut script = Vec::new();
		for rid in 0x40..=last {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the announce must stay up at exactly the advertised limit"
			);
			settle().await;
		}
		assert!(
			session.log.closes().is_empty(),
			"no session close at the limit: {:?}",
			session.log.closes()
		);
	}

	/// Drafts below 19 carry no MAX_REQUEST_UPDATES option, so a peer there agreed to no ceiling:
	/// the same flood that closes a draft-19 session must neither end the announce nor close the
	/// session on draft-18, because a conforming peer must not be stranded for a limit it never saw.
	#[tokio::test(start_paused = true)]
	async fn older_drafts_do_not_strand_an_announce_flood() {
		const VERSION: Version = Version::Draft18;

		let mut script = Vec::new();
		for rid in 0x40..=0x60u64 {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"draft-18 negotiates no limit, so the flood must not strand the announce"
			);
			settle().await;
		}
		assert!(
			session.log.closes().is_empty(),
			"draft-18 flood must not close the session"
		);
	}

	/// On drafts without the option the guard is a memory backstop, not a protocol limit: a flood
	/// past it ends the one announce by finishing the stream, never the session.
	#[tokio::test(start_paused = true)]
	async fn an_older_draft_announce_flood_past_the_guard_ends_the_announce() {
		const VERSION: Version = Version::Draft18;

		let last = 0x40 + request_update::UNNEGOTIATED_GUARD as u64 + 1;
		let mut script = Vec::new();
		for rid in 0x40..=last {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut result = None;
		for _ in 0..4000 {
			let _ = futures::poll!(acceptor.as_mut());
			if let std::task::Poll::Ready(r) = futures::poll!(run.as_mut()) {
				result = Some(r);
				break;
			}
			settle().await;
		}
		assert!(
			matches!(result, Some(Ok(()))),
			"a flood past the guard ends the announce with Ok: {result:?}"
		);
		assert!(
			session.log.closes().is_empty(),
			"the guard ends the announce, not the session"
		);
	}

	/// An alias reference (DELETE/USE_ALIAS) on a PUBLISH_NAMESPACE token is a connection-level
	/// protocol violation, closing the session as on the SETUP path, not a per-request refusal.
	#[tokio::test]
	async fn an_alias_token_on_a_request_is_a_protocol_violation() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, _requests, _cred, _consumer, session) = auth_announce_harness(VERSION, Vec::new());
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let mut msg = token_publish_namespace();
		// USE_ALIAS (0x02): an alias reference cannot precede SETUP, so it is a PROTOCOL_VIOLATION.
		msg.authorization_token = Some(bytes::Bytes::from_static(&[0x02, 0x07]));

		let err = subscriber
			.run_publish_namespace_stream(stream, msg, cluster::Peer::default(), None)
			.await
			.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation), "{err:?}");
		assert_eq!(
			session.log.closes().first().map(|c| c.0),
			Some(crate::SessionError::ProtocolViolation.to_code()),
			"the session must close with PROTOCOL_VIOLATION"
		);
	}

	/// Like [`auth_announce_harness`] but the session never negotiated the MoQ Auth extension,
	/// so its union is `None` forever: `allows` is permissive, `covers` is not. No credential
	/// is presented; the acceptor answers request tokens regardless.
	fn auth_announce_harness_no_ext(
		version: Version,
	) -> (
		Subscriber<crate::lite::test_transport::ScriptedSession>,
		crate::auth::Requests,
		origin::Consumer,
		crate::lite::test_transport::ScriptedSession,
	) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new()]);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let auth = crate::auth::Handle::new(false);
		let requests = auth.requests().unwrap();
		assert!(
			auth.allows(crate::auth::Direction::Subscribe, "room/alice"),
			"a None union is permissive for allows"
		);
		assert!(
			!auth.covers(crate::auth::Direction::Subscribe, "room/alice"),
			"a None union does not cover"
		);

		let subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.with_auth(auth);

		(subscriber, requests, consumer, session)
	}

	/// On a session without the AUTH extension the union is `None` forever, so `allows` is
	/// permissive; a token-bearing PUBLISH_NAMESPACE must still be verified (`covers`), not
	/// admitted by that default. Admitted on a covering grant; refused UNAUTHORIZED (not
	/// admitted) on refusal. This is the standard moq-transport peer shape the quest targets.
	#[tokio::test]
	async fn a_request_token_on_a_no_auth_session_is_verified() {
		const VERSION: Version = Version::Draft18;

		// Accepted: the acceptor is consulted (proving the token path, not the permissive
		// default, which the origin model would also route) and its grant admits the announce.
		{
			let (mut subscriber, mut requests, consumer, session) = auth_announce_harness_no_ext(VERSION);
			let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
			let consulted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
			let acceptor = {
				let consulted = consulted.clone();
				async move {
					let mut held = Vec::new();
					loop {
						let request = requests.next().await.expect("a request reaches the acceptor");
						consulted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
						held.push(request.accept(crate::auth::Grant::all()));
					}
				}
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));
			let mut routed = false;
			for _ in 0..500 {
				let _ = futures::poll!(acceptor.as_mut());
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"the announce ended during setup"
				);
				if routed_now(&consumer, "room/alice").is_some() {
					routed = true;
					break;
				}
				settle().await;
			}
			assert!(
				routed,
				"a token on a no-auth session must be verified and admitted, not ignored"
			);
			assert!(
				consulted.load(std::sync::atomic::Ordering::Relaxed) >= 1,
				"the token must reach the acceptor, not be admitted by the permissive default"
			);
		}

		// Refused: not admitted by the permissive default.
		{
			let (mut subscriber, mut requests, consumer, session) = auth_announce_harness_no_ext(VERSION);
			let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
			let acceptor = async move {
				let request = requests.next().await.expect("a request reaches the acceptor");
				request.reject(crate::SessionError::Unauthorized, "no");
				std::future::pending::<()>().await
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));
			let mut ended = false;
			for _ in 0..500 {
				let _ = futures::poll!(acceptor.as_mut());
				if futures::poll!(run.as_mut()).is_ready() {
					ended = true;
					break;
				}
				settle().await;
			}
			assert!(ended, "a refused token ends the announce");
			assert!(
				routed_now(&consumer, "room/alice").is_none(),
				"a refused token must not be admitted by the permissive default"
			);
		}
	}

	/// A peer that ends the announce while a renewal is still being verified ends it here too:
	/// with a grant that never expires and an acceptor that never answers the renewal, only
	/// the stream closing can end the request, and it must.
	#[tokio::test]
	async fn a_closed_announce_ends_while_a_renewal_is_pending() {
		const VERSION: Version = Version::Draft18;
		let session = crate::lite::test_transport::ScriptedSession::per_stream_eof(vec![
			publish_namespace_update_with_token(VERSION).await,
		]);
		let (mut subscriber, mut requests, _cred, consumer, session) = auth_announce_harness_on(VERSION, session);
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		let popped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let popped = popped.clone();
			async move {
				let first = requests.next().await.expect("a request");
				let _issued = first.accept(crate::auth::Grant::all());
				popped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let _never_answered = requests.next().await.expect("a renewal");
				popped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));
		let mut ended = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(run.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(
			popped.load(std::sync::atomic::Ordering::Relaxed) >= 1,
			"the announce was token-authorized"
		);
		assert!(
			ended,
			"closing the stream ends the announce despite the pending renewal"
		);
		assert!(
			routed_now(&consumer, "room/alice").is_none(),
			"the announce is withdrawn"
		);
	}

	/// An announcing peer's token grant is checked on its `publish` patterns: a read-only grant
	/// does not admit a PUBLISH_NAMESPACE, a write-only one does.
	#[tokio::test]
	async fn an_announce_token_needs_a_publish_grant() {
		const VERSION: Version = Version::Draft18;
		let read_only = crate::auth::Grant {
			publish: crate::Patterns::new(),
			subscribe: crate::Pattern::all().into(),
			expires: None,
		};
		let write_only = crate::auth::Grant {
			publish: crate::Pattern::all().into(),
			subscribe: crate::Patterns::new(),
			expires: None,
		};
		for (grant, admitted) in [(read_only, false), (write_only, true)] {
			let (mut subscriber, mut requests, consumer, session) = auth_announce_harness_no_ext(VERSION);
			let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
			let acceptor = async move {
				let request = requests.next().await.expect("a request reaches the acceptor");
				let _issued = request.accept(grant);
				std::future::pending::<()>().await
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));
			let mut ended = false;
			for _ in 0..300 {
				let _ = futures::poll!(acceptor.as_mut());
				if futures::poll!(run.as_mut()).is_ready() {
					ended = true;
					break;
				}
				if admitted && routed_now(&consumer, "room/alice").is_some() {
					break;
				}
				settle().await;
			}
			assert_eq!(
				routed_now(&consumer, "room/alice").is_some(),
				admitted,
				"admitted={admitted}"
			);
			assert_eq!(ended, !admitted, "a grant that does not cover the announce refuses it");
		}
	}

	/// The additive constraint: a token-LESS PUBLISH_NAMESPACE on a no-auth session (None union)
	/// is admitted by the origin model exactly as before; the token path is never entered.
	#[tokio::test]
	async fn a_token_less_request_on_a_no_auth_session_is_unchanged() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, _requests, consumer, session) = auth_announce_harness_no_ext(VERSION);
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();

		let mut msg = token_publish_namespace();
		msg.authorization_token = None;

		let mut run =
			std::pin::pin!(subscriber.run_publish_namespace_stream(stream, msg, cluster::Peer::default(), None,));
		let mut routed = false;
		for _ in 0..500 {
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some() {
				routed = true;
				break;
			}
			settle().await;
		}
		assert!(
			routed,
			"a token-less announce is admitted by the origin model as before"
		);
	}

	/// NAMESPACE has no REQUEST_UPDATE, so a peer reprices one by re-sending it on the
	/// SUBSCRIBE_NAMESPACE stream. The repeat is neither a duplicate nor a violation: it
	/// replaces the advertisement in place, and the route is never retracted for it.
	#[tokio::test(start_paused = true)]
	async fn a_re_sent_namespace_reprices_in_place() {
		const VERSION: Version = Version::Draft19;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let script = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer =
				crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);
			writer.encode(&ietf::RequestOk::ID).await.unwrap();
			writer.encode(&ietf::RequestOk { request_id: None }).await.unwrap();
			for cost in [4, 0] {
				writer.encode(&ietf::Namespace::ID).await.unwrap();
				writer
					.encode(&ietf::Namespace {
						suffix: crate::Path::new("x.hang"),
						cluster: Some(cluster::Advert {
							hops: hop_path(&[7, 9]),
							cost,
						}),
					})
					.await
					.unwrap();
			}
			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		// A negotiated peer over a free link, so the route cost is what it advertised.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			cluster: cluster::Peer {
				hop: Some(crate::Hop::new(9).unwrap()),
				cost: Some(0),
			},
			..Default::default()
		});
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(stream, crate::Path::new("").to_owned()));
		for _ in 0..100 {
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the stream stays open through a repeat"
			);
			if routed_now(&consumer, "x.hang").is_some_and(|route| route.cost.warm == 0) {
				break;
			}
			settle().await;
		}

		let route = routed_now(&consumer, "x.hang").expect("still routed");
		assert_eq!(route.cost.warm, 0, "the repeat repriced the route");
	}

	/// The peer explicitly retracting a namespace ends the broadcast immediately: it
	/// said the namespace is gone, so a later create at the path is new content.
	#[tokio::test(start_paused = true)]
	async fn an_explicit_namespace_done_closes_the_broadcast() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();

		let path = crate::Path::new("room/host").to_owned();
		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		subscriber.start_announce(path.clone(), advert).unwrap();
		settle().await;

		subscriber.stop_announce(path).unwrap();
		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"an explicit NAMESPACE_DONE must retract the route",
		);
	}

	/// v14-16 withdraw a PUBLISH_NAMESPACE with PUBLISH_NAMESPACE_DONE, which the adapter
	/// delivers as a message before it FINs the virtual stream. Reading it as a stray
	/// message closes the whole session over a routine unannounce.
	#[tokio::test(start_paused = true)]
	async fn a_publish_namespace_done_retracts_without_faulting_the_session() {
		const VERSION: Version = Version::Draft14;

		let path = crate::Path::new("room/host").to_owned();
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);
		writer
			.encode_message(&ietf::PublishNamespaceDone {
				track_namespace: path.borrow(),
				request_id: RequestId(0),
			})
			.await
			.unwrap();
		let script = log.writes.lock().unwrap().clone();

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		let msg = ietf::PublishNamespace {
			request_id: RequestId(0),
			track_namespace: path.borrow(),
			cluster: None,
			authorization_token: None,
		};
		subscriber
			.run_publish_namespace_stream(stream, msg, cluster::Peer::default(), None)
			.await
			.expect("a withdrawal is not a protocol violation");
		settle().await;

		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"an explicit withdrawal must close the broadcast",
		);
	}

	/// A PUBLISH_NAMESPACE stream that dies mid-advertisement detaches it, closing the
	/// broadcast: the advertisement was never withdrawn, but the stream carrying it is
	/// gone, and a route into a dead stream must not stay announced.
	///
	/// Driven through the real exit path rather than by calling `stop_announce`: a test
	/// that picked the detach itself would still pass if the stream stopped using it.
	#[tokio::test(start_paused = true)]
	async fn a_broken_publish_namespace_stream_closes_the_broadcast() {
		const VERSION: Version = Version::Draft19;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		// The peer sends something that does not belong on this stream, ending it with an
		// error while the advertisement is still live.
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);
		writer.encode(&ietf::NamespaceDone::ID).await.unwrap();
		let script = log.writes.lock().unwrap().clone();

		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let path = crate::Path::new("room/host").to_owned();
		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		let msg = ietf::PublishNamespace {
			request_id: RequestId(0),
			track_namespace: path.borrow(),
			cluster: None,
			authorization_token: None,
		};
		subscriber
			.run_publish_namespace_stream(stream, msg, cluster::Peer::default(), None)
			.await
			.expect_err("an unexpected message ends the stream");
		settle().await;

		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"a broken stream must close the broadcast, not leave a stale route",
		);
	}

	/// Several advertisements share one refcounted source, so the detach that empties it
	/// is the one that counts: the broadcast survives the first stop and closes on the
	/// last.
	///
	/// That is the model's own rule for several sources at one path (the front's source
	/// selection),
	/// which is what these advertisements would be had they arrived on two sessions. The
	/// refcount is a detail of sharing one `SourceGuard` per session; it must not change
	/// what the origin sees.
	#[tokio::test(start_paused = true)]
	async fn the_last_owner_out_decides_the_detach() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer::default();

		for _ in 0..2 {
			let advert = subscriber.route(None, &peer).expect("route");
			subscriber.start_announce(path.clone(), advert).unwrap();
		}
		settle().await;

		// One advertisement's stream dies: the other still holds the source.
		subscriber.stop_announce(path.clone()).unwrap();
		settle().await;
		assert!(
			routed_now(&consumer, "room/host").is_some(),
			"the broadcast must survive while an owner remains",
		);

		// The last owner retracts: the broadcast closes with it.
		subscriber.stop_announce(path).unwrap();
		settle().await;
		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"the last owner out must close the broadcast",
		);
	}

	/// An advertisement with no path of its own (a peer that did not negotiate the
	/// extension) still pays for the link it arrived over. Forwarding it as free would
	/// advertise a paid upstream as the cheapest route in the mesh.
	#[test]
	fn a_pathless_advert_still_pays_for_its_link() {
		let (unpriced, _origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let peer = cluster::Peer::default();

		// Nothing priced this direction, so it ranks by hop count.
		assert_eq!(
			unpriced.route(None, &peer).unwrap().route.cost.warm,
			cluster::DEFAULT_COST
		);

		// A peer that declared its egress price is charged it, extension or not.
		let priced_peer = cluster::Peer {
			hop: None,
			cost: Some(4),
		};
		assert_eq!(unpriced.route(None, &priced_peer).unwrap().route.cost.warm, 4);

		// Local policy still wins over what the peer declared.
		let (mut priced, _origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		priced.cost = Some(6);
		assert_eq!(priced.route(None, &priced_peer).unwrap().route.cost.warm, 6);
	}

	/// An update replaces the advertisement in place: the route moves, the refcount does
	/// not, and the source is not torn down.
	#[tokio::test]
	async fn cluster_update_replaces_in_place() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();

		let first = Advertised {
			route: crate::origin::Route::default()
				.with_hops(hop_path(&[7, 9]).hops().clone())
				.with_cost(4),
		};
		subscriber.start_announce(path.clone(), first).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// A new chain and cost: the route updates in place.
		let rerouted = Advertised {
			route: crate::origin::Route::default()
				.with_hops(hop_path(&[7, 11]).hops().clone())
				.with_cost(2),
		};
		subscriber.update_announce(path.clone(), rerouted).unwrap();

		let route = routed_now(&consumer, "room/host").expect("routed");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 11]);
		assert_eq!(route.cost.warm, 2);

		// One advertisement, so one unannounce detaches it. If the update had bumped the
		// refcount, this would leave the route stranded.
		subscriber.stop_announce(path).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// Regression: a publisher that declares no identity of its own contributes
	/// `Hop::UNKNOWN` as the first hop, which identifies nothing. A repeat NAMESPACE
	/// is still the same advertisement being repriced (the expected update, and how a
	/// relay signals that it started carrying the namespace), so the source and every
	/// live subscription on it must survive. Reading the repeat as a new publisher
	/// detached the source milliseconds after SUBSCRIBE went out.
	#[tokio::test(start_paused = true)]
	async fn anonymous_publisher_survives_a_repricing_update() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		// A free link, so the route cost is exactly what the peer advertised.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		let hops = cluster::HopPath::new(
			crate::Hops::try_from(vec![crate::Hop::UNKNOWN, crate::Hop::new(9).unwrap()]).unwrap(),
		);

		let advertised = subscriber
			.route(
				Some(&cluster::Advert {
					hops: hops.clone(),
					cost: 2,
				}),
				&peer,
			)
			.expect("route");
		subscriber.start_announce(path.clone(), advertised).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// The peer re-advertises the same path cheaper: it started carrying it.
		let repriced = subscriber
			.route(Some(&cluster::Advert { hops, cost: 1 }), &peer)
			.expect("route");
		subscriber.update_announce(path.clone(), repriced).unwrap();

		let route = routed_now(&consumer, "room/host").expect("still routed");
		assert_eq!(
			route.cost,
			crate::origin::Cost {
				warm: 1,
				..crate::origin::Cost::UNKNOWN
			},
			"the repriced warm cost arrives; the Cluster extension has nowhere to carry a cold cost, so it stays unknown rather than reading as the publisher's own zero"
		);

		// One advertisement, so one unannounce detaches it.
		subscriber.stop_announce(path).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// Two *separate* advertisements for one namespace refcount a single route:
	/// it takes both retractions to retract it.
	#[tokio::test(start_paused = true)]
	async fn separate_adverts_refcount_the_route() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};
		let hops = cluster::HopPath::new(
			crate::Hops::try_from(vec![crate::Hop::UNKNOWN, crate::Hop::new(9).unwrap()]).unwrap(),
		);
		let advert = cluster::Advert { hops, cost: 0 };

		let first = subscriber.route(Some(&advert), &peer).expect("route");
		subscriber.start_announce(path.clone(), first).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		let second = subscriber.route(Some(&advert), &peer).expect("route");
		subscriber.start_announce(path.clone(), second).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// Two advertisements, so it takes two unannounces to retract.
		subscriber.stop_announce(path.clone()).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());
		subscriber.stop_announce(path).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// Regression: without the MoQ Cluster extension an advertisement carries no path,
	/// so there is no publisher identity to compare. PUBLISH_NAMESPACE and NAMESPACE for
	/// one namespace are then two messages about a single source, and treating the
	/// second as a different publisher would tear down what the first attached, right
	/// as a subscriber is resolving a track through it.
	#[tokio::test]
	async fn pathless_adverts_never_replace_the_source() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer::default();

		// What a PUBLISH_NAMESPACE with no cluster parameters resolves to.
		let first = subscriber.route(None, &peer).expect("route");
		subscriber.start_announce(path.clone(), first).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// The NAMESPACE for the same namespace arrives second.
		let second = subscriber.route(None, &peer).expect("route");
		subscriber.start_announce(path.clone(), second).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// Two advertisements, so it takes two unannounces to retract.
		subscriber.stop_announce(path.clone()).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());
		subscriber.stop_announce(path).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// An update replaces the advertisement it repeats. When the replacement loops back
	/// through us it is a retraction, so the route we were holding must go: keeping it
	/// would leave subscriptions on a path the peer no longer offers.
	#[tokio::test]
	async fn reflected_replacement_retracts_the_route() {
		let self_origin = crate::Hop::new(5).unwrap();
		let (mut subscriber, origin) = cluster_subscriber(self_origin);
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};

		let clean = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 0,
		};
		let advert = subscriber.route(Some(&clean), &peer).expect("route");
		subscriber.start_announce(path.clone(), advert).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// The peer re-advertises the namespace over a path that now flows through us.
		let looped = cluster::Advert {
			hops: hop_path(&[7, 5, 9]),
			cost: 0,
		};
		assert!(
			subscriber.route(Some(&looped), &peer).is_none(),
			"a path containing our own Hop ID is a loop"
		);

		// That supersedes the advertisement it repeats, so the old route is retired.
		subscriber.stop_announce(path).unwrap();
		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"the superseded route must not stay attached"
		);
	}

	async fn publish_namespace_updates(updates: &[cluster::Advert]) -> Vec<u8> {
		const VERSION: Version = Version::Draft19;
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);

		for (i, advert) in updates.iter().enumerate() {
			writer.encode(&ietf::PublishNamespaceUpdate::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceUpdate {
					// Each update consumes a request id of the peer's parity.
					request_id: RequestId(3 + 2 * i as u64),
					hops: Some(advert.hops.clone()),
					cost: Some(advert.cost),
					authorization_token: None,
				})
				.await
				.unwrap();
		}

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Build a subscriber whose peer replays `script` on one PUBLISH_NAMESPACE stream,
	/// with the advertisement already attached. The origin's driver is returned so a
	/// test can tear the origin down underneath a live advertisement.
	async fn update_harness(
		self_origin: crate::Hop,
		peer: &cluster::Peer,
		attached: &cluster::Advert,
		script: Vec<u8>,
	) -> (
		Subscriber<crate::lite::test_transport::ScriptedSession>,
		crate::origin::Consumer,
		Stream<crate::lite::test_transport::ScriptedSession, Version>,
		crate::origin::Driver,
	) {
		const VERSION: Version = Version::Draft19;
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let (origin, driver) = crate::origin::Producer::new(crate::origin::Config::new(self_origin));
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The tests drive the loop directly, so nothing spawns; leaking keeps the
		// handles alive without a spawner.
		std::mem::forget(task_set);

		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			self_origin,
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let path = crate::Path::new("room/host").to_owned();
		let advert = subscriber.route(Some(attached), peer).expect("route");
		subscriber.start_announce(path, advert).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some(), "attached to start with");

		let stream = Stream::open(&mut session.clone(), VERSION).await.unwrap();
		(subscriber, consumer, stream, driver)
	}

	/// Build a subscriber whose peer replays `updates` on one PUBLISH_NAMESPACE stream,
	/// with the advertisement already attached.
	async fn reflected_harness(
		self_origin: crate::Hop,
		peer: &cluster::Peer,
		attached: &cluster::Advert,
		updates: &[cluster::Advert],
	) -> (
		Subscriber<crate::lite::test_transport::ScriptedSession>,
		crate::origin::Consumer,
		Stream<crate::lite::test_transport::ScriptedSession, Version>,
	) {
		let script = publish_namespace_updates(updates).await;
		let (subscriber, consumer, stream, driver) = update_harness(self_origin, peer, attached, script).await;
		// Dropping the driver tears the origin down, so leak it: these tests only
		// need the synchronous half.
		std::mem::forget(driver);
		(subscriber, consumer, stream)
	}

	/// How many times `type_id` was written to the peer, as a one-byte message type.
	fn replies(log: &crate::lite::test_transport::Log, type_id: u64) -> usize {
		let writes = log.writes.lock().unwrap();
		// A REQUEST_OK is the type, a two-byte length of 1, and an empty parameter
		// block; a REQUEST_ERROR's body is longer. Counting the type at the start of
		// each framed message keeps a body byte from being mistaken for a type.
		let mut count = 0;
		let mut at = 0;
		while at + 3 <= writes.len() {
			if writes[at] as u64 == type_id {
				count += 1;
			}
			let len = u16::from_be_bytes([writes[at + 1], writes[at + 2]]) as usize;
			at += 3 + len;
		}
		count
	}

	fn peer_9() -> cluster::Peer {
		cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		}
	}

	/// A clean path, and one that runs back through us (Hop ID 5).
	fn clean_and_looped() -> (cluster::Advert, cluster::Advert) {
		(
			cluster::Advert {
				hops: hop_path(&[7, 9]),
				cost: 0,
			},
			cluster::Advert {
				hops: hop_path(&[7, 5, 9]),
				cost: 0,
			},
		)
	}

	/// A reflected update detaches the route but MUST NOT end the stream. Updates ride
	/// the stream that already carries the advertisement, so closing it strands the
	/// namespace even when the peer's path goes clean again. It is also not ours to
	/// close: a peer MAY legitimately send a path carrying our Hop ID when a redundant
	/// sibling shares it, which the draft answers with "discard", not PROTOCOL_VIOLATION.
	#[tokio::test]
	async fn a_reflected_update_detaches_but_keeps_the_stream() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, looped) = clean_and_looped();

		let (mut subscriber, consumer, mut stream) = reflected_harness(self_origin, &peer, &clean, &[looped]).await;
		let log = subscriber.session.log.clone();

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Some(clean.clone()),
				peer,
				&mut attached,
				None,
			));

			for _ in 0..100 {
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"the stream must stay open after a reflected update"
				);
				if routed_now(&consumer, "room/host").is_none() {
					break;
				}
				settle().await;
			}
		}

		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"an unusable path must not stay attached"
		);
		assert!(!attached, "the caller must not release it a second time");
		assert_eq!(
			replies(&log, ietf::RequestOk::ID),
			1,
			"the update was applied, so it is acknowledged"
		);
	}

	/// Having kept the stream, a later usable path re-attaches on it. This is the whole
	/// reason the stream stays open.
	#[tokio::test]
	async fn a_clean_update_after_a_reflection_reattaches() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, looped) = clean_and_looped();

		let (mut subscriber, consumer, mut stream) =
			reflected_harness(self_origin, &peer, &clean, &[looped, clean.clone()]).await;

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Some(clean.clone()),
				peer,
				&mut attached,
				None,
			));

			// Both updates apply, then the loop parks on the exhausted script. The
			// intermediate detach is not observable (one poll can drain both messages),
			// so the end state is what this asserts; the detach itself is covered by
			// `a_reflected_update_detaches_but_keeps_the_stream`.
			for _ in 0..20 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				settle().await;
			}
		}

		assert!(attached, "the clean path must re-attach");
		assert!(
			routed_now(&consumer, "room/host").is_some(),
			"the namespace is routable again",
		);
	}

	/// The expected update: a relay that started carrying the namespace reprices it to
	/// 0. REQUEST_UPDATE keeps an omitted parameter, so the 0 arrives explicit and alone,
	/// lands on the path already held, and is answered REQUEST_OK.
	#[tokio::test]
	async fn an_explicit_zero_reprices_the_held_path() {
		const VERSION: Version = Version::Draft19;
		let self_origin = crate::Hop::new(5).unwrap();
		// A free link, so the route cost is exactly what the peer advertised.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		let held = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		};

		let script = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer =
				crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);
			writer.encode(&ietf::PublishNamespaceUpdate::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceUpdate {
					request_id: RequestId(3),
					hops: None,
					cost: Some(0),
					authorization_token: None,
				})
				.await
				.unwrap();
			let writes = log.writes.lock().unwrap();
			writes.clone()
		};

		let (mut subscriber, consumer, mut stream, driver) = update_harness(self_origin, &peer, &held, script).await;
		std::mem::forget(driver);
		let log = subscriber.session.log.clone();
		assert_eq!(routed_now(&consumer, "room/host").expect("routed").cost.warm, 4);

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Some(held.clone()),
				peer,
				&mut attached,
				None,
			));
			for _ in 0..20 {
				assert!(futures::poll!(run.as_mut()).is_pending(), "the stream stays open");
				settle().await;
			}
		}

		let route = routed_now(&consumer, "room/host").expect("still routed");
		assert_eq!(route.cost.warm, 0, "the explicit 0 replaced the held cost");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 9], "the omitted path kept its value");
		assert!(attached, "a repricing is not a retraction");
		assert_eq!(replies(&log, ietf::RequestOk::ID), 1);
		assert_eq!(replies(&log, ietf::RequestError::ID), 0);
	}

	/// An update that cannot be applied is refused with REQUEST_ERROR and the stream
	/// closed, which withdraws the advertisement (moq-transport Section 9.5.1). The
	/// caller releases the route, so the loop must return cleanly rather than fault the
	/// session.
	#[tokio::test]
	async fn a_failed_update_withdraws_the_advertisement() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, _) = clean_and_looped();
		let cheaper = cluster::Advert {
			cost: 0,
			..clean.clone()
		};

		let script = publish_namespace_updates(&[cheaper]).await;
		let (mut subscriber, _consumer, mut stream, driver) = update_harness(self_origin, &peer, &clean, script).await;
		let log = subscriber.session.log.clone();

		// Tear the origin down underneath the advertisement: the route can no longer be
		// repriced, which is the one way an in-place update fails.
		drop(driver);

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		let mut result = None;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Some(clean.clone()),
				peer,
				&mut attached,
				None,
			));
			for _ in 0..20 {
				if let std::task::Poll::Ready(res) = futures::poll!(run.as_mut()) {
					result = Some(res);
					break;
				}
				settle().await;
			}
		}

		assert!(
			matches!(result, Some(Ok(()))),
			"a refused update ends the stream cleanly, got {result:?}"
		);
		assert!(attached, "the caller releases the route it attached");
		assert_eq!(replies(&log, ietf::RequestError::ID), 1, "REQUEST_ERROR went out");
		assert_eq!(replies(&log, ietf::RequestOk::ID), 0);
	}

	/// An update whose first Hop ID differs names a different publisher. It still
	/// replaces the advertisement in place and the stream stays open: the origin, not the
	/// session, keeps the two publishers' content apart.
	#[tokio::test]
	async fn an_update_that_changes_the_publisher_applies_in_place() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, _) = clean_and_looped();
		let other_publisher = cluster::Advert {
			hops: hop_path(&[8, 9]),
			cost: 0,
		};

		let script = publish_namespace_updates(&[other_publisher]).await;
		let (mut subscriber, consumer, mut stream, driver) = update_harness(self_origin, &peer, &clean, script).await;
		std::mem::forget(driver);
		let log = subscriber.session.log.clone();

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Some(clean.clone()),
				peer,
				&mut attached,
				None,
			));
			for _ in 0..20 {
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"a publisher change must not close the stream"
				);
				settle().await;
			}
		}

		assert!(attached, "the advertisement stays attached");
		assert_eq!(replies(&log, ietf::RequestOk::ID), 1, "REQUEST_OK went out");
		assert_eq!(replies(&log, ietf::RequestError::ID), 0);
		let route = routed_now(&consumer, "room/host").expect("still routed");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![8, 9], "the held path was replaced");
	}

	/// A second PUBLISH_NAMESPACE on the stream that already carries one is not an
	/// update any more: it is the base draft's duplicate request, a protocol violation.
	#[tokio::test]
	async fn a_repeated_publish_namespace_is_a_duplicate() {
		const VERSION: Version = Version::Draft19;
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, _) = clean_and_looped();

		let script = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer =
				crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);
			writer.encode(&ietf::PublishNamespace::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespace {
					request_id: RequestId(1),
					track_namespace: crate::Path::new("room/host"),
					cluster: Some(cluster::Advert {
						cost: 0,
						..clean.clone()
					}),
					authorization_token: None,
				})
				.await
				.unwrap();
			let writes = log.writes.lock().unwrap();
			writes.clone()
		};

		let (mut subscriber, _consumer, mut stream, driver) = update_harness(self_origin, &peer, &clean, script).await;
		std::mem::forget(driver);

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
			&mut stream,
			&path,
			Some(clean.clone()),
			peer,
			&mut attached,
			None,
		));
		let mut result = None;
		for _ in 0..20 {
			if let std::task::Poll::Ready(res) = futures::poll!(run.as_mut()) {
				result = Some(res);
				break;
			}
			settle().await;
		}

		let err = result.expect("the loop ends").expect_err("a repeat is refused");
		assert!(is_protocol_violation(&err), "a duplicate request is fatal, got {err}");
	}

	/// The SUBSCRIBE_NAMESPACE stream owns every advertisement it carried. When it ends
	/// without a NAMESPACE_DONE for each, those refcounts must still be released, or the
	/// source stays attached for the rest of the session (the stream can die while the
	/// session keeps running).
	#[tokio::test]
	async fn namespace_stream_close_releases_live_paths() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let peer = cluster::Peer::default();

		let mut live = std::collections::HashSet::new();
		for path in ["room/a", "room/b"] {
			let path = crate::Path::new(path).to_owned();
			let advert = subscriber.route(None, &peer).expect("route");
			subscriber.start_announce(path.clone(), advert).unwrap();
			live.insert(path);
		}
		assert!(routed_now(&consumer, "room/a").is_some());
		assert!(routed_now(&consumer, "room/b").is_some());

		// What the stream's exit path does with whatever it still holds.
		for path in live {
			subscriber.stop_announce(path).unwrap();
		}

		assert!(routed_now(&consumer, "room/a").is_none(), "room/a leaked a refcount");
		assert!(routed_now(&consumer, "room/b").is_none(), "room/b leaked a refcount");
	}

	/// PUBLISH offers one track, but a source attaches per namespace and serves every
	/// track under it. Rather than invent a namespace-level source from a track-level
	/// offer, decline the request and leave the session running.
	///
	/// Draft-14 answers with PUBLISH_ERROR and its own registry; draft-15 folded the message
	/// into REQUEST_ERROR, so both shapes have to carry NOT_SUPPORTED.
	#[tokio::test]
	async fn publish_is_rejected_without_announcing() {
		for version in [Version::Draft14, Version::Draft19] {
			// An open gate, so the rejection actually reaches the wire.
			let gate = kio::Producer::new(true);
			let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let consumer = origin.consume();
			let (tasks, task_set) = crate::util::TaskSet::new();
			std::mem::forget(task_set);

			let mut subscriber = Subscriber::new(
				crate::time::Clock::tokio(),
				session.clone(),
				origin,
				Control::new(None, false),
				None,
				peer::PeerSetup::default(),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			);

			let stream = Stream::open(&mut session.clone(), version).await.unwrap();
			let msg = ietf::Publish {
				request_id: RequestId(1),
				track_namespace: crate::Path::new("room/host"),
				track_name: "video".into(),
				track_alias: 7,
				largest_location: None,
				forward: true,
				properties: ietf::Properties::default(),
			};

			// Errors are surfaced to the peer on the stream, not raised as a session error.
			subscriber.run_publish_stream(stream, msg).await.unwrap();
			tokio::time::sleep(Duration::from_millis(1)).await;

			assert!(
				routed_now(&consumer, "room/host").is_none(),
				"a rejected PUBLISH must not announce a broadcast"
			);
			// Encode the reply we expect rather than matching the reason alone, so an error
			// code regressing to something outside the draft's table cannot slip through.
			// NOT_SUPPORTED is 0x3 in every registry, which is what makes it comparable here.
			let expected = {
				const NOT_SUPPORTED: u64 = 0x3;

				let log = crate::lite::test_transport::Log::default();
				let mut writer =
					crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

				match version {
					Version::Draft14 => {
						writer.encode(&ietf::PublishError::ID).await.unwrap();
						writer
							.encode(&ietf::PublishError {
								request_id: RequestId(1),
								error_code: NOT_SUPPORTED,
								reason_phrase: "PUBLISH is not supported".into(),
							})
							.await
							.unwrap();
					}
					_ => {
						writer.encode(&ietf::RequestError::ID).await.unwrap();
						writer
							.encode(&ietf::RequestError {
								request_id: None,
								error_code: NOT_SUPPORTED,
								reason_phrase: "PUBLISH is not supported".into(),
								retry_interval: 0,
							})
							.await
							.unwrap();
					}
				}

				log.writes.lock().unwrap().clone()
			};

			assert_eq!(
				occurrences(&session.log, &expected),
				1,
				"{version} must decline the PUBLISH as NOT_SUPPORTED"
			);
		}
	}
}

/// Whether `type_id` ends a PUBLISH_NAMESPACE stream: PUBLISH_NAMESPACE_DONE before
/// draft-17; later drafts end it by closing the stream.
fn terminal_publish_namespace(version: Version, type_id: u64) -> bool {
	match version {
		Version::Draft14 | Version::Draft15 | Version::Draft16 => type_id == ietf::PublishNamespaceDone::ID,
		_ => false,
	}
}

/// What a SUBSCRIBE asks for: the range it delivers, and the backfill covering a head that
/// range excludes.
#[derive(Debug, Default, PartialEq, Eq)]
struct Join {
	/// The Location Filter, bounding what the subscription itself delivers.
	filter: Filter,

	/// The FILL_PARAMETERS backfill, delivered on its own fetch stream.
	fill: Option<ietf::Fill>,

	/// A pre-draft-20 joining FETCH, sent as its own request after SUBSCRIBE.
	fetch: Option<JoiningFetch>,
}

/// What a moq-lite subscription's group range asks for on the wire.
///
/// moq-lite joins a track at the *start* of the current group, which is a decodable point.
/// Draft-20 spells that as the draft's own current-group join (section 5.1.6): a Next
/// Object subscription plus a `StartGroup=1` fill, which is the only form a publisher has
/// to honor. It splits the group across two streams, the fill carrying the head and the
/// subscription the tail, which `claim_fill` stitches back into one group producer.
///
/// Earlier drafts have no fill parameter. Every subscription is Largest Object, followed
/// by a joining FETCH: relative at offset 0 for a live join, absolute at the requested
/// group for an explicit group-aligned and unbounded start. A frame-level start or a
/// bounded end has no joining-FETCH spelling, so those shapes are refused rather than
/// rounded down or left open.
///
/// A start group we already know is absolute on draft-20 and needs no fill: the
/// subscription's own range covers it, which is what our publisher serves from its cache.
fn subscribe_join(
	start: Option<track::Position>,
	end: Option<track::Position>,
	version: Version,
) -> Result<Join, Error> {
	if !Filter::is_draft20(version) {
		if start.is_some_and(|start| start.frame != 0) || end.is_some() {
			return Err(Error::Unsupported);
		}
		return Ok(Join {
			filter: Filter::NextObject,
			fill: None,
			fetch: Some(match start {
				None => JoiningFetch::Relative { group_offset: 0 },
				Some(start) => JoiningFetch::Absolute { group_id: start.group },
			}),
		});
	}

	Ok(match start {
		// The live join: everything after the live edge, plus the current group's head.
		None => Join {
			filter: Filter::NextObject,
			fill: Some(ietf::Fill {
				// One group back from the next group is the current one.
				filter: Some(Filter::Relative(1)),
				range_filters: false,
			}),
			fetch: None,
		},
		// An absolute {0, 0} with no end is defined as unfiltered, so it spells itself.
		Some(start) if start == track::Position::group(0) && end.is_none() => Join {
			filter: Filter::Unfiltered,
			fill: None,
			fetch: None,
		},
		Some(start) => Join {
			filter: Filter::Absolute {
				start: ietf::Location {
					group: start.group,
					object: start.frame,
				},
				end: end.and_then(|end| {
					if end.frame == 0 {
						Some(ietf::EndLocation {
							group: end.group.checked_sub(1)?,
							object: None,
						})
					} else {
						Some(ietf::EndLocation {
							group: end.group,
							object: Some(end.frame - 1),
						})
					}
				}),
			},
			fill: None,
			fetch: None,
		},
	})
}

/// The absolute Object ID for a subgroup object, given the prior one and its delta.
///
/// The first object's delta is its absolute Object ID; every later one is the prior ID plus
/// the delta plus one. moq-lite groups never skip an object, so a gap is refused: it would
/// renumber every frame after it. Checked against the ID rather than the header's
/// FIRST_OBJECT bit, which is only the publisher's claim.
///
/// `start` is where this stream picks the group up, which is 0 for a group delivered whole
/// and the object after the fill's head for the tail of a stitched one. Anything else has a
/// hole at the front.
fn next_object_id(prior: Option<u64>, delta: u64, start: u64) -> Result<u64, Error> {
	let object = match prior {
		None => delta,
		Some(prior) => prior
			.checked_add(delta)
			.and_then(|id| id.checked_add(1))
			.ok_or(Error::Decode(crate::coding::DecodeError::BoundsExceeded))?,
	};

	let expected = prior.map_or(start, |prior| prior.saturating_add(1));
	if object != expected {
		tracing::warn!(
			object,
			expected,
			"object IDs must start at the group's start and increment by 1"
		);
		return Err(Error::Unsupported);
	}

	Ok(object)
}

#[cfg(test)]
mod object_id_tests {
	use super::*;

	/// A zero delta throughout is a group numbered from 0 with no gaps, which is the only
	/// shape moq-lite can represent.
	#[test]
	fn accepts_sequential_ids_from_zero() {
		let mut prior = None;
		for expected in 0..4 {
			let object = next_object_id(prior, 0, 0).expect("sequential");
			assert_eq!(object, expected);
			prior = Some(object);
		}
	}

	/// The first object's delta is its absolute Object ID, so a non-zero one means the
	/// group starts partway through and has a hole at the front.
	#[test]
	fn rejects_a_group_that_does_not_start_at_zero() {
		assert!(matches!(next_object_id(None, 6, 0), Err(Error::Unsupported)));
	}

	/// The tail of a stitched group starts where the fill's head stopped, and nowhere else.
	#[test]
	fn accepts_a_tail_that_starts_where_the_fill_stopped() {
		assert_eq!(next_object_id(None, 6, 6).expect("the fill's next object"), 6);
		assert_eq!(next_object_id(Some(6), 0, 6).expect("then sequential"), 7);
		assert!(matches!(next_object_id(None, 5, 6), Err(Error::Unsupported)));
		assert!(matches!(next_object_id(None, 7, 6), Err(Error::Unsupported)));
	}

	/// A later delta skips objects, which would renumber every frame after it.
	#[test]
	fn rejects_a_gap() {
		assert!(matches!(next_object_id(Some(0), 1, 0), Err(Error::Unsupported)));
		assert!(matches!(next_object_id(Some(3), 9, 0), Err(Error::Unsupported)));
	}

	/// The running ID is bounded, and the draft makes an overflow a protocol violation
	/// rather than something to wrap.
	#[test]
	fn rejects_an_overflow() {
		assert!(next_object_id(Some(u64::MAX), 0, 0).is_err());
	}
}

#[cfg(test)]
mod filter_tests {
	use super::*;

	/// The live join is the draft's own: the subscription starts after the live edge and a
	/// `StartGroup=1` fill covers the current group's head, so every object arrives exactly
	/// once and the group still starts at a decodable point.
	#[test]
	fn live_joins_the_current_group_with_a_fill() {
		assert_eq!(
			subscribe_join(None, None, Version::Draft20).unwrap(),
			Join {
				filter: Filter::NextObject,
				fill: Some(ietf::Fill {
					filter: Some(Filter::Relative(1)),
					range_filters: false,
				}),
				fetch: None,
			}
		);
	}

	/// A start we can name absolutely is inside the subscription's own range, so there is no
	/// head outside it to fill.
	#[test]
	fn a_past_start_is_absolute() {
		assert_eq!(
			subscribe_join(
				Some(track::Position::group(7)),
				track::Position::after_group(9),
				Version::Draft20,
			)
			.unwrap(),
			Join {
				filter: Filter::Absolute {
					start: ietf::Location { group: 7, object: 0 },
					end: Some(ietf::EndLocation { group: 9, object: None }),
				},
				fill: None,
				fetch: None,
			}
		);
	}

	/// The whole track has a spelling of its own: an absent filter is unrestricted.
	#[test]
	fn the_whole_track_is_unfiltered() {
		assert_eq!(
			subscribe_join(Some(track::Position::group(0)), None, Version::Draft20).unwrap(),
			Join {
				filter: Filter::Unfiltered,
				fill: None,
				fetch: None,
			}
		);
	}

	const JOINING_DRAFTS: [Version; 6] = [
		Version::Draft14,
		Version::Draft15,
		Version::Draft16,
		Version::Draft17,
		Version::Draft18,
		Version::Draft19,
	];

	/// Pre-draft-20 live joins are Largest Object plus a relative joining FETCH at offset 0,
	/// so the current group's head arrives on the fetch stream and the live tail on the
	/// subscription.
	#[test]
	fn older_drafts_live_join_with_a_relative_fetch() {
		for version in JOINING_DRAFTS {
			assert_eq!(
				subscribe_join(None, None, version).unwrap(),
				Join {
					filter: Filter::NextObject,
					fill: None,
					fetch: Some(JoiningFetch::Relative { group_offset: 0 }),
				},
				"{version}"
			);
		}
	}

	/// An explicit group-aligned unbounded start is an absolute joining FETCH at that group.
	#[test]
	fn older_drafts_absolute_join_at_the_start_group() {
		for version in JOINING_DRAFTS {
			assert_eq!(
				subscribe_join(Some(track::Position::group(7)), None, version).unwrap(),
				Join {
					filter: Filter::NextObject,
					fill: None,
					fetch: Some(JoiningFetch::Absolute { group_id: 7 }),
				},
				"{version}"
			);
		}
	}

	/// A frame-level start has no joining-FETCH spelling, so it is refused rather than
	/// rounded down to the group.
	#[test]
	fn older_drafts_refuse_a_frame_level_start() {
		for version in JOINING_DRAFTS {
			assert!(
				matches!(
					subscribe_join(Some(track::Position { group: 7, frame: 1 }), None, version),
					Err(Error::Unsupported)
				),
				"{version}"
			);
		}
	}

	/// A bounded end has no joining-FETCH spelling, so it is refused rather than left open.
	#[test]
	fn older_drafts_refuse_a_bounded_end() {
		for version in JOINING_DRAFTS {
			assert!(
				matches!(
					subscribe_join(
						Some(track::Position::group(7)),
						track::Position::after_group(9),
						version
					),
					Err(Error::Unsupported)
				),
				"{version}"
			);
		}
	}
}

/// Draft-20's current-group join, where one group arrives on two streams: the fill fetch
/// stream carries the head and the subscription's own subgroup stream the tail.
#[cfg(test)]
mod stitch_tests {
	use bytes::BufMut as _;

	use super::*;
	use crate::{
		Timestamp,
		coding::Encode as _,
		lite::test_transport::ScriptedSession,
		model::ProduceTest,
		transport::poll::Session as _,
		util::{TaskSet, Tasks},
	};

	const VERSION: Version = Version::Draft20;
	const ALIAS: u64 = 7;
	const REQUEST: RequestId = RequestId(1);
	const SEQUENCE: u64 = 4;

	/// A distinct timestamp per object, so a stitched group's frames can be told apart.
	fn timestamp(index: usize) -> Timestamp {
		Timestamp::from_micros(1000 + index as u64).expect("in range")
	}

	/// A publisher's fill fetch stream: a FETCH_HEADER, then one object per payload
	/// numbered from the group's first.
	fn fill_stream(sequence: u64, payloads: &[&[u8]]) -> Vec<u8> {
		fill_stream_for(REQUEST, &[(sequence, payloads)], true)
	}

	/// A joining FETCH stream named by its own request id, possibly spanning groups.
	///
	/// `timed` writes Timestamp properties. Multi-group tests leave them off so frames
	/// stamped on arrival share one epoch with the live tail; a 1000µs presentation time
	/// against a wall-clock tail would convict every earlier group as stale.
	fn fill_stream_for<B: AsRef<[u8]>>(request_id: RequestId, groups: &[(u64, &[B])], timed: bool) -> Vec<u8> {
		let mut buf = bytes::BytesMut::new();
		ietf::FetchHeader::TYPE.encode(&mut buf, VERSION).unwrap();
		ietf::FetchHeader { request_id }.encode(&mut buf, VERSION).unwrap();

		let mut object_index = 0usize;
		let mut prev_group = None;
		for &(sequence, payloads) in groups {
			for (index, payload) in payloads.iter().enumerate() {
				let payload = payload.as_ref();
				let properties = timed.then(|| {
					let mut properties = bytes::BytesMut::new();
					ietf::encode_object_time(&mut properties, timestamp(object_index), Timescale::MICRO, VERSION)
						.unwrap();
					properties.to_vec()
				});

				// The first object of the stream carries the absolute Group ID. From
				// draft-18 on, the first object of a later group carries the ascending
				// Group ID Delta (new = prior + delta + 1), so 7 then 8 is delta 0.
				let first = index == 0;
				let group = match (first, prev_group) {
					(false, _) => None,
					(true, None) => Some(sequence),
					(true, Some(prev)) => Some(sequence.checked_sub(prev + 1).expect("ascending groups")),
				};
				ietf::FetchObject::Object {
					subgroup: ietf::FetchSubgroup::Zero,
					group,
					object: first.then_some(0),
					priority: first.then_some(0),
					properties,
				}
				.encode(&mut buf, VERSION)
				.unwrap();

				(payload.len() as u64).encode(&mut buf, VERSION).unwrap();
				buf.put_slice(payload);
				object_index += 1;
			}
			prev_group = Some(sequence);
		}

		buf.to_vec()
	}

	/// The subscription's own subgroup stream, starting at `start` because a strict
	/// publisher delivers nothing before it: that head is the fill's job.
	fn tail_stream(sequence: u64, start: u64, payloads: &[&[u8]]) -> Vec<u8> {
		let mut buf = bytes::BytesMut::new();
		ietf::GroupHeader {
			track_alias: ALIAS,
			group_id: sequence,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: ietf::GroupFlags {
				first_object: start == 0,
				..Default::default()
			},
		}
		.encode(&mut buf, VERSION)
		.unwrap();

		for (index, payload) in payloads.iter().enumerate() {
			// The first object's delta is its absolute Object ID; every later one counts
			// the objects skipped, so zero is the next one.
			let delta = match index {
				0 => start,
				_ => 0,
			};
			delta.encode(&mut buf, VERSION).unwrap();
			(payload.len() as u64).encode(&mut buf, VERSION).unwrap();
			buf.put_slice(payload);
		}

		buf.to_vec()
	}

	/// A subscriber holding one draft-20 subscription, as its SUBSCRIBE_OK left it: the
	/// alias bound, the timescale declared, and `fill` waiting on its fetch stream.
	struct Harness {
		subscriber: Subscriber<ScriptedSession>,
		session: ScriptedSession,
		track: track::Producer,
		fill: kio::Producer<Fill>,
		_tasks: (Tasks, TaskSet),
	}

	impl Harness {
		fn new(fill: Fill, scripts: Vec<Vec<u8>>) -> Self {
			let session = ScriptedSession::per_stream_eof(scripts);
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let tasks = TaskSet::new();

			let subscriber = Subscriber::new(
				crate::time::Clock::tokio(),
				session.clone(),
				origin,
				Control::new(None, false),
				None,
				peer::PeerSetup::default(),
				crate::Hop::new(1).unwrap(),
				None,
				VERSION,
				tasks.0.clone(),
				Default::default(),
			);

			// The subscriber accepts every track at microseconds, matching `run_subscribe`.
			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				track::Info::default().with_timescale(Timescale::MICRO),
			);
			let fill = kio::Producer::new(fill);

			{
				let mut state = subscriber.state.lock();
				state.subscribes.insert(
					REQUEST,
					TrackState {
						alias: Some(ALIAS),
						timescale: Some(Timescale::MICRO),
						..TrackState::new(track.clone(), Path::new("broadcast").to_owned(), fill.clone(), None)
					},
				);
				insert_track_alias(&state.aliases, ALIAS, REQUEST).unwrap();
			}

			Self {
				subscriber,
				session,
				track,
				fill,
				_tasks: tasks,
			}
		}

		/// Bind a pre-draft-20 joining FETCH onto this subscription, so `recv_fill` looks
		/// the stream up by the FETCH's own request id.
		fn with_joining(self, joining: JoiningFetch, fetch_id: RequestId, largest: ietf::Location) -> Self {
			{
				let mut state = self.subscriber.state.lock();
				state.fetches.insert(fetch_id, REQUEST);
				if let Some(track) = state.subscribes.get_mut(&REQUEST) {
					track.fetch_id = Some(fetch_id);
					track.joining = Some(joining);
					track.largest = Some(largest);
				}
			}
			self
		}

		/// A reader over the next scripted stream, standing in for one the peer opened.
		async fn stream(&self) -> Reader<<ScriptedSession as web_transport_trait::poll::Session>::RecvStream, Version> {
			let mut session = self.session.clone();
			let (_, recv) = session.open_bi().await.unwrap();
			Reader::new(recv, VERSION)
		}
	}

	/// Every frame of the next group, once it finishes.
	async fn read_group(subscriber: &mut track::Subscriber) -> (u64, Vec<(Timestamp, Vec<u8>)>) {
		let mut group = subscriber
			.recv_group()
			.await
			.expect("track aborted")
			.expect("track finished");

		let sequence = group.sequence;
		let mut frames = Vec::new();
		while let Some(frame) = group.read_frame().await.expect("group aborted") {
			frames.push((frame.timestamp, frame.payload.to_vec()));
		}

		(sequence, frames)
	}

	/// The canonical join: the fill carries the objects published before we subscribed and
	/// the subscription the ones after, and they land in one group in order.
	///
	/// The tail is read first, so it has to wait for the head rather than start a group of
	/// its own: with newest-first group order the publisher can prioritize the tail's stream
	/// ahead of the fill's.
	#[tokio::test]
	async fn a_fill_and_its_tail_stitch_into_one_group() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		let mut serve_tail = h.subscriber.clone();
		let mut serve_fill = h.subscriber.clone();
		let (tail, head) = futures::join!(serve_tail.recv_group(&mut tail), serve_fill.recv_fill(&mut fill));
		head.expect("fill");
		tail.expect("tail");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(
			frames,
			vec![
				(timestamp(0), b"head-0".to_vec()),
				(timestamp(1), b"head-1".to_vec()),
				// The tail carries no timestamps of its own, so it is stamped on arrival.
				(frames[2].0, b"tail-2".to_vec()),
			]
		);
		assert!(matches!(*h.fill.read(), Fill::Done), "the head was claimed");
	}

	/// A tail parked on its head stops holding the end open, so the finished fill does until
	/// the head is claimed: otherwise the subscription looks settled between the fill
	/// finishing and the tail waking, and ends before the tail is read.
	#[tokio::test]
	async fn an_unclaimed_fill_holds_the_end_open() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
			],
		);
		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		let mut serve_tail = h.subscriber.clone();
		let mut tailing = std::pin::pin!(serve_tail.recv_group(&mut tail));
		assert!(
			futures::poll!(tailing.as_mut()).is_pending(),
			"the tail waits for its head"
		);
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");

		let state = h.subscriber.state.lock().subscribes[&REQUEST].tail.consume();
		let count = state.read().streams();
		let mut settle = Settle::new(&crate::time::Clock::tokio(), state);
		let mut settled = std::pin::pin!(kio::wait(|waiter| poll_settled(&mut settle, waiter, &h.fill, count)));
		assert!(
			futures::poll!(settled.as_mut()).is_pending(),
			"the head is still owed to the tail"
		);

		tailing.await.expect("tail");
		settled.await;
	}

	#[tokio::test]
	async fn object_extension_limit() {
		for size in [65536usize, 65537] {
			for first in [true, false] {
				let mut script = Vec::new();
				ietf::GroupHeader {
					track_alias: ALIAS,
					group_id: SEQUENCE,
					sub_group_id: 0,
					publisher_priority: 0,
					flags: ietf::GroupFlags {
						has_extensions: true,
						..Default::default()
					},
				}
				.encode(&mut script, VERSION)
				.unwrap();
				if !first {
					// A complete first object exercises the ingestion path on the next one.
					script.extend_from_slice(&[0, 0, 1, 42]);
				}
				0u64.encode(&mut script, VERSION).unwrap();
				size.encode(&mut script, VERSION).unwrap();
				if size == 65536 {
					// Unknown even properties with value zero, valid with delta type ids.
					script.resize(script.len() + size, 0);
					script.extend_from_slice(&[1, 42]);
				}
				// Over-limit lengths deliberately carry no extension bytes.
				let h = Harness::new(Fill::Done, vec![script]);
				let mut consumer = h.track.subscribe(None);
				let mut stream = h.stream().await;
				let result = h.subscriber.clone().recv_group(&mut stream).await;
				if size == 65536 {
					result.unwrap();
					let (_, frames) = read_group(&mut consumer).await;
					assert_eq!(frames.len(), if first { 1 } else { 2 });
				} else {
					assert!(matches!(
						result,
						Err(Error::Decode(DecodeError::MessageTooLarge {
							size: 65537,
							max: 65536
						}))
					));
					// The stream dispatcher uses this mapping to stop only this stream.
					assert_eq!(
						crate::StreamError::from(&result.unwrap_err()),
						crate::StreamError::MalformedTrack
					);
				}
				assert!(h.session.log.closes().is_empty());
			}
		}
	}

	/// Append an END_OF_TRACK object: delta 0, an empty payload, then its status.
	fn end_of_track(mut stream: Vec<u8>) -> Vec<u8> {
		for value in [0u64, 0, END_OF_TRACK] {
			value.encode(&mut stream, VERSION).unwrap();
		}
		stream
	}

	/// END_OF_TRACK after a group's last object ends the track right after that group.
	#[tokio::test]
	async fn an_end_of_track_after_a_group_ends_the_track_after_it() {
		let h = Harness::new(Fill::Done, vec![end_of_track(tail_stream(SEQUENCE, 0, &[b"last"]))]);
		let mut consumer = h.track.subscribe(None);
		let mut stream = h.stream().await;

		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		assert_eq!(h.track.final_sequence(), Some(SEQUENCE + 1));

		let mut group = consumer.recv_group().await.unwrap().expect("the group arrives");
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"last");
		assert!(group.read_frame().await.unwrap().is_none(), "the group is finished");
		assert!(consumer.recv_group().await.unwrap().is_none(), "then the track ends");
	}

	/// A group at or past the end an END_OF_TRACK declared contradicts that end, which no
	/// later stream can repair, so the whole track fails rather than ending clean without it.
	#[tokio::test]
	async fn a_group_past_the_declared_end_aborts_the_track() {
		use futures::FutureExt;

		let mut h = Harness::new(Fill::Done, vec![tail_stream(SEQUENCE, 0, &[b"late"])]);
		h.track.finish_at(SEQUENCE).unwrap();
		let mut stream = h.stream().await;

		let res = h.subscriber.clone().recv_group(&mut stream).await;
		assert!(matches!(res, Err(Error::ProtocolViolation)), "{res:?}");
		assert!(matches!(
			h.track.closed().now_or_never(),
			Some(Error::ProtocolViolation)
		));
	}

	/// END_OF_TRACK at object 0 says the group does not exist, so the track ends before it
	/// and no group is created for it.
	#[tokio::test]
	async fn an_end_of_track_at_object_zero_creates_no_group() {
		let h = Harness::new(Fill::Done, vec![end_of_track(tail_stream(SEQUENCE, 0, &[]))]);
		let mut stream = h.stream().await;

		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		assert_eq!(h.track.final_sequence(), Some(SEQUENCE));
		assert_eq!(h.track.latest(), None, "no group was created");
	}

	/// The group ended exactly where we joined it, so the subscription's stream carries no
	/// objects at all. That still ends the group, which is what publishes the head.
	#[tokio::test]
	async fn an_empty_tail_finishes_the_filled_group() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 2, &[]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		h.subscriber.clone().recv_group(&mut tail).await.expect("tail");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 2, "the head is the whole group");
	}

	/// The subscription can end while the fetch stream is still writing, and that teardown
	/// cannot reach a producer the fetch stream still owns. The handoff has to settle it, or
	/// the head outlives the subscription unfinished and a consumer blocks on it.
	#[tokio::test]
	async fn a_head_finishing_after_teardown_is_published_not_installed() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::default().with_timescale(Timescale::MICRO),
		);
		let mut consumer = track.subscribe(None);

		let mut producer = track.create_group(group::Info { sequence: SEQUENCE }).unwrap();
		producer.write_frame(timestamp(0), b"head-0".as_slice()).unwrap();

		// `remove_subscribe` got there first.
		let mut fill = Fill::Done;
		fill.install(Fill::Ready {
			sequence: SEQUENCE,
			next: 1,
			producer,
		});
		assert!(matches!(fill, Fill::Done), "Done is terminal");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 1, "published rather than left unfinished");
	}

	/// A publisher that serves a head and then opens a whole group for the same sequence
	/// has contradicted its own fill. The model holds one producer per group, so the
	/// duplicate stream goes and the head is published as the prefix it is.
	#[tokio::test]
	async fn a_whole_group_for_a_headed_sequence_is_refused() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 0, &[b"again-0"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut again = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		assert!(matches!(
			h.subscriber.clone().recv_group(&mut again).await,
			Err(Error::Unsupported)
		));

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 2, "the head is published once, not twice");
	}

	/// A fill head parked waiting for its tail is still a live group producer. Dropping the
	/// session has to end it, or the consumer waits on a group nobody will ever write again.
	/// The guard is `State`'s own `Drop`, so it runs however the driver was torn down.
	#[tokio::test]
	async fn a_cancelled_session_aborts_a_waiting_fill_head() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![fill_stream(SEQUENCE, &[b"head-0"])],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");

		// The head exists and is waiting for the tail that never comes.
		let mut group = consumer
			.recv_group()
			.await
			.expect("track aborted")
			.expect("track finished");

		drop(h);

		assert!(
			matches!(group.read_frame().await, Err(Error::Cancel)),
			"a waiting fill head must be cancelled, not left parked"
		);
	}

	/// The same contradiction as above, with the streams the other way round: the whole
	/// group lands before the fill has written its head. The model holds one producer per
	/// live sequence, so the fill loses the race to create it and gives up, rather than a
	/// second producer appearing and the objects being delivered twice.
	#[tokio::test]
	async fn a_whole_group_that_precedes_the_head_wins_the_sequence() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				tail_stream(SEQUENCE, 0, &[b"whole-0"]),
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut whole = h.stream().await;
		let mut fill = h.stream().await;

		h.subscriber
			.clone()
			.recv_group(&mut whole)
			.await
			.expect("the whole group");
		assert!(
			h.subscriber.clone().recv_fill(&mut fill).await.is_err(),
			"the fill cannot create a second producer for a live sequence"
		);

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 1, "the group is whatever one producer wrote, not both");
	}

	/// Without a head there is nothing to stitch onto, so a stream that starts part way
	/// through a group is dropped and the join degrades to the next group boundary. This is
	/// what a strict publisher gives a subscriber that asks for no fill.
	#[tokio::test]
	async fn a_tail_without_a_fill_is_dropped() {
		let h = Harness::new(Fill::Done, vec![tail_stream(SEQUENCE, 2, &[b"tail-2"])]);
		let mut consumer = h.track.subscribe(None);
		let mut tail = h.stream().await;

		// The stream goes, not the session.
		assert!(matches!(
			h.subscriber.clone().recv_group(&mut tail).await,
			Err(Error::Unsupported)
		));

		// Nothing usable reaches the model: the group is never offered at all.
		let delivered = tokio::time::timeout(Duration::from_millis(50), async {
			let mut group = consumer.recv_group().await.ok().flatten()?;
			group.read_frame().await.ok().flatten()
		})
		.await;
		assert!(matches!(delivered, Err(_) | Ok(None)), "no frame is delivered");
	}

	/// A head that stops short of where the tail starts would leave a hole in the middle of
	/// the group, which the model cannot express. Both halves go, and the head is published
	/// as the prefix it is.
	#[tokio::test]
	async fn a_head_that_misses_the_tail_is_refused() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 5, &[b"tail-5"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		assert!(matches!(
			h.subscriber.clone().recv_group(&mut tail).await,
			Err(Error::Unsupported)
		));

		let (_, frames) = read_group(&mut consumer).await;
		assert_eq!(frames.len(), 2, "the head is published as the prefix it is");
	}

	/// A fetch stream can arrive before SUBSCRIBE_OK commits the pending track. It waits
	/// for that response instead of looking up a producer that does not exist yet.
	#[tokio::test]
	async fn an_early_fill_waits_for_subscribe_ok() {
		let h = Harness::new(Fill::Requested, vec![fill_stream(SEQUENCE, &[b"head-0"])]);
		{
			let mut state = h.subscriber.state.lock();
			state.subscribes.get_mut(&REQUEST).unwrap().producer = None;
		}
		let mut fill = h.stream().await;
		let mut subscriber = h.subscriber.clone();
		let mut receiving = Box::pin(subscriber.recv_fill(&mut fill));
		assert!(futures::poll!(receiving.as_mut()).is_pending());
		{
			let mut state = h.subscriber.state.lock();
			state.subscribes.get_mut(&REQUEST).unwrap().producer = Some(h.track.clone());
		}
		*h.fill.write().ok().unwrap() = Fill::Serving(Some(Timescale::MICRO));
		receiving.await.expect("early fill");
		assert!(matches!(*h.fill.read(), Fill::Ready { .. }));
	}

	/// A fetch stream answering a subscription that asked for no fill duplicates a group the
	/// subscription itself is delivering, so it is refused rather than written.
	#[tokio::test]
	async fn an_unsolicited_fill_is_refused() {
		let h = Harness::new(Fill::Done, vec![fill_stream(SEQUENCE, &[b"head-0"])]);
		let mut fill = h.stream().await;

		assert!(matches!(
			h.subscriber.clone().recv_fill(&mut fill).await,
			Err(Error::Unsupported)
		));
	}

	const FETCH: RequestId = RequestId(3);
	const LIVE: ietf::Location = ietf::Location {
		group: SEQUENCE,
		object: 1,
	};

	/// A mid-group subscribe stream waits for the joining FETCH's head and stitches onto it,
	/// the same rendezvous a draft-20 fill uses. The FETCH is named by its own request id.
	#[tokio::test]
	async fn a_joining_fetch_stitches_a_mid_group_tail() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream_for(FETCH, &[(SEQUENCE, &[b"head-0", b"head-1"])], true),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
			],
		)
		.with_joining(JoiningFetch::Relative { group_offset: 0 }, FETCH, LIVE);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		let mut serve_tail = h.subscriber.clone();
		let mut serve_fill = h.subscriber.clone();
		let (tail, head) = futures::join!(serve_tail.recv_group(&mut tail), serve_fill.recv_fill(&mut fill));
		head.expect("joining fetch");
		tail.expect("tail");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].1, b"head-0");
		assert_eq!(frames[1].1, b"head-1");
		assert_eq!(frames[2].1, b"tail-2");
	}

	/// A subscribe stream that starts at object 0 stands alone; the joining FETCH's answer
	/// is discarded rather than delivered twice.
	#[tokio::test]
	async fn a_whole_group_stream_discards_the_joining_fetch() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				tail_stream(SEQUENCE, 0, &[b"whole-0"]),
				fill_stream_for(FETCH, &[(SEQUENCE, &[b"head-0", b"head-1"])], true),
			],
		)
		.with_joining(JoiningFetch::Relative { group_offset: 0 }, FETCH, LIVE);
		let mut consumer = h.track.subscribe(None);

		let mut whole = h.stream().await;
		let mut fill = h.stream().await;

		h.subscriber
			.clone()
			.recv_group(&mut whole)
			.await
			.expect("the whole group");
		assert!(
			h.subscriber.clone().recv_fill(&mut fill).await.is_err(),
			"the fetch cannot create a second producer for a live sequence"
		);

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].1, b"whole-0");
	}

	/// From draft-18 on, a later object's Group ID field is a delta: 7 then 8 is 0, not 8.
	/// Draft-17 and earlier still send the absolute ID.
	#[test]
	fn a_later_fetch_group_field_is_a_delta() {
		assert_eq!(
			resolve_fetch_group(Version::Draft18, Some(7), Some(0)).unwrap(),
			Some(8)
		);
		assert_eq!(
			resolve_fetch_group(Version::Draft20, Some(7), Some(0)).unwrap(),
			Some(8)
		);
		assert_eq!(
			resolve_fetch_group(Version::Draft17, Some(7), Some(8)).unwrap(),
			Some(8)
		);
	}

	/// An absolute joining FETCH writes complete groups below Largest Location, then the
	/// live group's head; the subscribe stream continues that last group with no gap.
	/// Consecutive groups encode as ascending delta 0 from draft-18 on.
	#[tokio::test]
	async fn an_absolute_fetch_stitches_into_the_live_tail() {
		const START: u64 = 7;
		const LIVE_GROUP: u64 = 10;
		let largest = ietf::Location {
			group: LIVE_GROUP,
			object: 1,
		};
		let groups: &[(u64, &[&[u8]])] = &[
			(START, &[b"g7-0"]),
			(8, &[b"g8-0"]),
			(9, &[b"g9-0"]),
			(LIVE_GROUP, &[b"g10-0", b"g10-1"]),
		];
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream_for(FETCH, groups, false),
				tail_stream(LIVE_GROUP, 2, &[b"g10-2"]),
			],
		)
		.with_joining(JoiningFetch::Absolute { group_id: START }, FETCH, largest);
		// Keep every fetched group: the default max-age of zero would drop each one as
		// its successor arrives, and the stitch would hang waiting on a group already skipped.
		let mut consumer = h
			.track
			.subscribe(track::Subscription::default().with_max_age(Duration::from_secs(60)));

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("absolute fetch");
		h.subscriber.clone().recv_group(&mut tail).await.expect("tail");

		let mut groups = Vec::new();
		for _ in 0..4 {
			groups.push(read_group(&mut consumer).await);
		}
		assert_eq!(
			groups
				.iter()
				.map(|(seq, frames)| (*seq, frames.iter().map(|(_, p)| p.as_slice()).collect::<Vec<_>>()))
				.collect::<Vec<_>>(),
			vec![
				(7, vec![b"g7-0".as_slice()]),
				(8, vec![b"g8-0".as_slice()]),
				(9, vec![b"g9-0".as_slice()]),
				(10, vec![b"g10-0".as_slice(), b"g10-1".as_slice(), b"g10-2".as_slice()]),
			]
		);
	}

	/// A fetch stream that ends before the live group delivers what arrived. The first
	/// delivered group is the start, and the live tail that cannot stitch is a discontinuity.
	#[tokio::test]
	async fn a_short_fetch_delivers_its_prefix() {
		const START: u64 = 7;
		let largest = ietf::Location { group: 10, object: 1 };
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream_for(FETCH, &[(START, &[b"g7-0", b"g7-1"])], true),
				tail_stream(10, 2, &[b"g10-2"]),
			],
		)
		.with_joining(JoiningFetch::Absolute { group_id: START }, FETCH, largest);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("short fetch");
		assert!(matches!(
			h.subscriber.clone().recv_group(&mut tail).await,
			Err(Error::Unsupported)
		));

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, START);
		assert_eq!(frames.len(), 2, "the prefix that arrived is published");
		assert_eq!(frames[0].1, b"g7-0");
		assert_eq!(frames[1].1, b"g7-1");
	}
}

/// A scripted peer answering SUBSCRIBE then FETCH, so the join is spelled on the wire.
#[cfg(test)]
mod joining_fetch_tests {
	use super::*;
	use crate::{
		coding::Encode as _,
		lite::test_transport::ScriptedSession,
		model::ProduceTest,
		util::{TaskSet, Tasks},
	};

	const JOINING_DRAFTS: [Version; 6] = [
		Version::Draft14,
		Version::Draft15,
		Version::Draft16,
		Version::Draft17,
		Version::Draft18,
		Version::Draft19,
	];

	async fn settle() {
		tokio::time::sleep(Duration::from_millis(1)).await;
	}

	fn message_bytes<M: Message>(id: u64, msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		id.encode(&mut buf, version).unwrap();
		msg.encode(&mut buf, version).unwrap();
		buf
	}

	fn subscribe_ok(version: Version, largest: Option<ietf::Location>) -> Vec<u8> {
		message_bytes(
			ietf::SubscribeOk::ID,
			&ietf::SubscribeOk {
				request_id: match version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(RequestId(1)),
					_ => None,
				},
				track_alias: 7,
				largest,
				properties: Default::default(),
			},
			version,
		)
	}

	fn fetch_ok(version: Version) -> Vec<u8> {
		message_bytes(
			ietf::FetchOk::ID,
			&ietf::FetchOk {
				request_id: match version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(RequestId(3)),
					_ => None,
				},
				group_order: GroupOrder::Ascending,
				end_of_track: false,
				end_location: ietf::Location { group: 4, object: 2 },
			},
			version,
		)
	}

	fn fetch_error(version: Version) -> Vec<u8> {
		match version {
			Version::Draft14 => message_bytes(
				ietf::FetchError::ID,
				&ietf::FetchError {
					request_id: RequestId(3),
					error_code: 1,
					reason_phrase: "refused".into(),
				},
				version,
			),
			Version::Draft15 | Version::Draft16 => message_bytes(
				ietf::RequestError::ID,
				&ietf::RequestError {
					request_id: Some(RequestId(3)),
					error_code: 1,
					reason_phrase: "refused".into(),
					retry_interval: 0,
				},
				version,
			),
			_ => message_bytes(
				ietf::RequestError::ID,
				&ietf::RequestError {
					request_id: None,
					error_code: 1,
					reason_phrase: "refused".into(),
					retry_interval: 0,
				},
				version,
			),
		}
	}

	fn decode_messages(log: &crate::lite::test_transport::Log, version: Version) -> Vec<(u64, bytes::Bytes)> {
		use crate::coding::Decode;

		let writes = log.writes.lock().unwrap().clone();
		let mut buf = writes.as_slice();
		let mut messages = Vec::new();
		while !buf.is_empty() {
			let Ok(type_id) = u64::decode(&mut buf, version) else {
				break;
			};
			let Ok(size) = u16::decode(&mut buf, version) else {
				break;
			};
			if buf.len() < size as usize {
				break;
			}
			let (body, rest) = buf.split_at(size as usize);
			messages.push((type_id, bytes::Bytes::copy_from_slice(body)));
			buf = rest;
		}
		messages
	}

	struct JoinRun {
		subscriber: Subscriber<ScriptedSession>,
		session: ScriptedSession,
		_hold: (
			crate::broadcast::Producer,
			track::Consumer,
			kio::Pending<track::Subscribing>,
		),
		_tasks: (Tasks, TaskSet),
		serving: tokio::task::JoinHandle<()>,
	}

	impl JoinRun {
		async fn start(version: Version, start: Option<track::Position>, ok: Vec<u8>, fetch: Vec<u8>) -> Self {
			let session = ScriptedSession::per_stream(vec![ok, fetch]);
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let subscriber = Subscriber::new(
				crate::time::Clock::tokio(),
				session.clone(),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(None, false),
				None,
				peer::PeerSetup::default(),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks.clone(),
				Default::default(),
			);

			let producer = crate::broadcast::Info::default().produce();
			let mut dynamic = producer.dynamic();
			let consumer = producer.consume();
			let track = consumer.track("video").unwrap();
			let subscription = match start {
				None => track.subscribe(None),
				Some(start) => track.subscribe(track::Subscription::default().with_start(start)),
			};
			let request = dynamic.requested_track().await.expect("no track requested");

			let mut serving_subscriber = subscriber.clone();
			let serving = tokio::spawn(async move {
				serving_subscriber
					.run_subscribe(Path::new("broadcast"), dynamic, request)
					.await;
			});

			settle().await;

			Self {
				subscriber,
				session,
				_hold: (producer, track, subscription),
				_tasks: (tasks, _task_set),
				serving,
			}
		}

		fn fill_outstanding(&self) -> bool {
			let state = self.subscriber.state.lock();
			let track = state
				.subscribes
				.values()
				.next()
				.expect("the subscription is registered");
			track.fill.read().outstanding()
		}
	}

	impl Drop for JoinRun {
		fn drop(&mut self) {
			self.serving.abort();
		}
	}

	/// Every pre-draft-20 live join is Largest Object on the SUBSCRIBE and a relative
	/// joining FETCH at offset 0 that names the subscribe's request id.
	#[tokio::test(start_paused = true)]
	async fn a_live_join_is_spelled_as_largest_object_plus_relative_fetch() {
		let largest = Some(ietf::Location { group: 4, object: 1 });
		for version in JOINING_DRAFTS {
			let run = JoinRun::start(version, None, subscribe_ok(version, largest), fetch_ok(version)).await;

			let messages = decode_messages(&run.session.log, version);
			let subscribe = messages
				.iter()
				.find(|(id, _)| *id == ietf::Subscribe::ID)
				.expect("SUBSCRIBE");
			let mut body = subscribe.1.clone();
			let msg = ietf::Subscribe::decode_msg(&mut body, version).unwrap();
			assert_eq!(msg.filter, Filter::NextObject, "{version}");
			assert!(msg.fill.is_none(), "{version}");

			let fetch = messages.iter().find(|(id, _)| *id == ietf::Fetch::ID).expect("FETCH");
			let mut body = fetch.1.clone();
			let msg = ietf::Fetch::decode_msg(&mut body, version).unwrap();
			assert_eq!(
				msg.fetch_type,
				FetchType::RelativeJoining {
					subscriber_request_id: RequestId(1),
					group_offset: 0,
				},
				"{version}"
			);
		}
	}

	/// An explicit group-aligned unbounded start is the same Largest Object SUBSCRIBE plus
	/// an absolute joining FETCH at that group.
	#[tokio::test(start_paused = true)]
	async fn an_absolute_join_is_spelled_at_the_start_group() {
		let largest = Some(ietf::Location { group: 9, object: 0 });
		for version in JOINING_DRAFTS {
			let run = JoinRun::start(
				version,
				Some(track::Position::group(7)),
				subscribe_ok(version, largest),
				fetch_ok(version),
			)
			.await;

			let messages = decode_messages(&run.session.log, version);
			let fetch = messages.iter().find(|(id, _)| *id == ietf::Fetch::ID).expect("FETCH");
			let mut body = fetch.1.clone();
			let msg = ietf::Fetch::decode_msg(&mut body, version).unwrap();
			assert_eq!(
				msg.fetch_type,
				FetchType::AbsoluteJoining {
					subscriber_request_id: RequestId(1),
					group_id: 7,
				},
				"{version}"
			);
		}
	}

	/// The peer refusing the FETCH continues the subscription live: the fill is settled so
	/// a later whole group is not left waiting on a head that is never coming.
	#[tokio::test(start_paused = true)]
	async fn a_refused_fetch_continues_live() {
		let largest = Some(ietf::Location { group: 4, object: 1 });
		for version in JOINING_DRAFTS {
			let run = JoinRun::start(version, None, subscribe_ok(version, largest), fetch_error(version)).await;
			assert!(
				!run.fill_outstanding(),
				"{version}: a refused FETCH must not leave the fill waiting"
			);
			assert!(
				run.subscriber.state.lock().subscribes.values().next().is_some(),
				"{version}: the subscription continues live"
			);
		}
	}

	/// A frame-level start is refused before SUBSCRIBE is written, rather than rounded down.
	#[tokio::test(start_paused = true)]
	async fn a_frame_level_start_never_reaches_the_wire() {
		let version = Version::Draft19;
		let session = ScriptedSession::new(Vec::new());
		let log = session.log.clone();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::Clock::tokio(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let _subscription =
			track.subscribe(track::Subscription::default().with_start(track::Position { group: 7, frame: 1 }));
		let request = dynamic.requested_track().await.expect("no track requested");

		subscriber.run_subscribe(Path::new("broadcast"), dynamic, request).await;

		let writes = log.writes.lock().unwrap().clone();
		assert!(writes.is_empty(), "a refused join must not write SUBSCRIBE");
	}
}
