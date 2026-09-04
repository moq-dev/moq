use std::{
	collections::{HashMap, hash_map::Entry},
	task::{Poll, ready},
	time::Duration,
};

use crate::{
	Error, Path, PathOwned, SessionError, Timescale, broadcast,
	coding::{Reader, Stream},
	frame, group,
	ietf::{self, Control, Filter, GroupOrder, RequestId},
	origin, track,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, Tasks},
};

use super::{Message, Version, cluster, peer};

use web_async::Lock;
use web_transport_trait::{MaybeSend, MaybeSync};

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

	// Track aliases chosen by the remote publisher.
	aliases: TrackAliases,

	// Each broadcast created by a PUBLISH_NAMESPACE message.
	broadcasts: HashMap<PathOwned, BroadcastState>,
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
		if let Fill::Ready { mut producer, .. } = std::mem::replace(self, Fill::Done) {
			let _ = producer.finish();
		}
	}
}

/// What a SUBSCRIBE_OK told us about the track it accepted.
struct Accepted {
	/// The Track Alias the publisher bound to this subscription.
	alias: u64,

	/// The units its object timestamps are in, when it declared any.
	timescale: Option<Timescale>,

	/// The largest Location in the track, absent when it has no content yet. That absence
	/// is what says a fill we asked for is owed nothing.
	largest: Option<ietf::Location>,
}

struct TrackState {
	producer: track::Producer,
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
}

/// How the last source for a path detaches, which decides whether the origin closes
/// the broadcast now or holds it open for a replacement.
///
/// Only the detach that drops the refcount to zero decides, matching the model's rule
/// for several sources at one path (`detach_source`): an earlier owner that vanished
/// does not outvote the last one still on the path. That keeps two advertisements on
/// one session behaving like the same two on separate sessions, where the model sees
/// two independent sources and the last one out decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Detach {
	/// The peer retracted the path, or we are rolling back an announce we just made.
	/// Nothing is coming back, so close it now.
	Graceful,
	/// The stream carrying the path went away without retracting it. Abort the
	/// source so viewers observe the loss as an error rather than a clean end.
	Abrupt,
}

struct BroadcastState {
	// The route announced into our origin for this namespace, post-charge.
	route: crate::origin::Route,

	// The served route: dropping it (and the serve task's clone) retracts the
	// route and rejects its queued requests.
	dynamic: crate::origin::Dynamic,

	// active number of PUBLISH_NAMESPACE messages.
	count: usize,

	// One minted source per requested path under the namespace: finish() on a
	// deliberate unannounce, dropping (a dying session) aborts them so viewers
	// observe the loss as an error.
	sources: HashMap<PathOwned, crate::model::broadcast::SourceGuard>,
}

/// What one advertisement said, once its parameters are resolved against the session.
struct Advertised {
	/// The route it describes, with this link's price already charged. The prefix
	/// is stamped where the advertisement attaches (the namespace).
	route: crate::origin::Route,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	// Arms the track-alias and request-id timeouts.
	runtime: R,
	session: S,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Producer,
	control: Control,
	// The origin naming this link, appended to the hop chain of every broadcast from
	// this session when the peer declares none of its own (see `session_route`). Base
	// moq-transport carries no hop ids, so a peer only has an identity if it negotiated
	// the MoQ Cluster extension or the caller assigned it one
	// (`Client::with_peer_hop`), which also makes the route recognizable across
	// sessions dialing the same relay.
	//
	// Otherwise this is `Hop::UNKNOWN` (0), the reserved "no identity" value.
	// Minting one here is not this layer's call: the peer never learns the id, so
	// only the side that assigned it can exclude it for loop detection, and whether
	// two sessions should look like one identity or two is the caller's policy. A
	// server answers it per accepted session; a client only when it knows the peer.
	session_origin: crate::Hop,
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
}

/// Resolve the subscription a data stream belongs to.
///
/// SUBSCRIBE_OK can be reordered behind the stream it describes, so an alias we have not
/// seen is worth waiting on briefly (draft-19 section 11.4.2). Three outcomes:
/// the subscription, [`Error::Cancel`] for an alias we retired, and [`Error::NotFound`]
/// once the wait expires without any binding at all.
async fn resolve_track_alias<R: crate::runtime::Runtime>(
	runtime: &R,
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

impl<S, R> Subscriber<S, R>
where
	S: crate::transport::poll::Boxable,
	R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
	R::Timer: MaybeSend,
{
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		runtime: R,
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
			self_origin,
			peer_setup,
			cost,
			state: Default::default(),
			tasks,
			version,
			going_away,
		}
	}

	/// What the peer declared in its SETUP, or the default (extension off) on a version
	/// that cannot negotiate it. See [`super::Publisher::peer`].
	pub(super) async fn peer(&self) -> cluster::Peer {
		match cluster::supported(self.version) {
			true => self.peer_setup.get().await.cluster,
			false => cluster::Peer::default(),
		}
	}

	/// The route for an advertisement that carries no path of its own.
	///
	/// Base moq-transport has no hops on the wire, so the chain is a single entry
	/// attributed to this session (`Hop::UNKNOWN` unless the peer or the caller
	/// supplied an identity).
	///
	/// That entry doubles as the content identity, which is what makes an assigned
	/// identity worth having: every session dialing the same relay produces the same
	/// first hop, so a reconnect splices into the front its predecessor was serving
	/// instead of replacing it. The cost is that we cannot tell the peer's own content
	/// apart from our own coming back through it, since neither carries a chain. Loop
	/// detection for that case is `FrontState::excluded`, not the chain.
	///
	/// The link is charged all the same. Such an advertisement carries no ROUTE_COST,
	/// which reads as 0, but the draft charges every advertisement for the direction it
	/// arrived over regardless. Skipping it would forward a paid upstream to
	/// cluster-aware peers as free and pull subscriptions onto the wrong relay.
	///
	/// It is charged only one hop, though the chain it stands for may be arbitrarily
	/// long: a peer that carries no hop ids hides its depth, so this route understates
	/// its true length and can out-rank a longer-looking but genuinely shorter one.
	/// Price such a link with [`crate::Client::with_cost`] rather than trusting the
	/// default.
	fn session_route(&self, peer: &cluster::Peer) -> crate::origin::Route {
		let mut hops = crate::Hops::new();
		hops.push(self.session_origin)
			.expect("an empty hop chain has room for one entry");
		crate::origin::Route::default()
			.with_hops(hops)
			// A peer with no Cluster extension advertises no cost at all, so its cold
			// path is unknown rather than free.
			.with_cost(crate::origin::Cost::UNKNOWN.charged(cluster::link_cost(self.cost, peer)))
	}

	/// The route an advertisement describes, or `None` when it must be discarded.
	///
	/// A negotiated peer supplies the path and cost, so the route is what the mesh
	/// actually knows: the full chain, and the accumulated cost plus this link's price.
	/// An advertisement whose path already contains our own Hop ID looped back, and
	/// neither forwarding it nor subscribing through it is safe.
	fn route(&self, advert: Option<&cluster::Advert>, peer: &cluster::Peer) -> Option<Advertised> {
		let Some(advert) = advert else {
			return Some(Advertised {
				route: self.session_route(peer),
			});
		};

		if advert.loops(self.self_origin) {
			return None;
		}

		Some(Advertised {
			route: advert.route(cluster::link_cost(self.cost, peer)),
		})
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

		held.broadcast == new.broadcast && held.producer.name() == new.producer.name()
	}

	fn remove_subscribe(&self, request_id: RequestId) -> Option<TrackState> {
		let mut state = self.state.lock();
		let track = state.subscribes.remove(&request_id)?;
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
	/// Asked unconditionally, without waiting on the peer's SETUP: a peer with nothing to
	/// advertise answers with an empty set, which costs one stream, while waiting to find
	/// out costs a round trip on every session.
	pub fn subscribe_prefixes(&self) -> Vec<PathOwned> {
		self.origin.allowed().map(|p| p.to_owned()).collect()
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

		let request_id = self.control.next_request_id(&self.runtime).await?;

		// Draft-18+ uses SUBSCRIBE_NAMESPACE (0x50); earlier drafts use the legacy
		// 0x11 message with a Subscribe Options field.
		match self.version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				let msg = ietf::SubscribeNamespaceLegacy {
					request_id,
					namespace: prefix.clone(),
					subscribe_options: 0x01, // NAMESPACE only
				};
				stream.writer.encode(&ietf::SubscribeNamespaceLegacy::ID).await?;
				stream.writer.encode(&msg).await?;
			}
			_ => {
				let msg = ietf::SubscribeNamespace {
					request_id,
					namespace: prefix.clone(),
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
				tracing::warn!(error_code = %msg.error_code, reason = %msg.reason_phrase, "subscribe_namespace error");
				return Err(Error::Cancel);
			}
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(error_code = %msg.error_code, reason = %msg.reason_phrase, "subscribe_namespace error");
				return Err(Error::Cancel);
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
		// Abruptly, including on a clean FIN: closing the stream retracts nothing, since
		// the protocol has NAMESPACE_DONE for that. Whatever is still live here outlived
		// its channel without being withdrawn, so hold the front open for a reconnect.
		// This is what moq-lite already does, where the equivalent map is a local whose
		// guards drop.
		let res = self.run_namespace_entries(&mut stream, &prefix, &peer, &mut live).await;
		for path in live {
			let _ = self.stop_announce(path, Detach::Abrupt);
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
							let _ = self.stop_announce(path, Detach::Graceful);
						}
						continue;
					};

					tracing::debug!(%path, hops = advert.route.hops.len(), cost = ?advert.route.cost, "namespace");
					if live.contains(&path) {
						// A repeat replaces the advertisement atomically; nothing is torn
						// down merely because an update arrived.
						self.update_announce(path, advert)?;
					} else {
						self.start_announce(path.clone(), advert)?;
						live.insert(path);
					}
				}
				ietf::NamespaceDone::ID => {
					let msg = ietf::NamespaceDone::decode_msg(&mut data, self.version)?;
					let path = prefix.join(&msg.suffix);
					tracing::debug!(%path, "namespace_done");
					if live.remove(&path) {
						let _ = self.stop_announce(path, Detach::Graceful);
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
			self.write_error(&mut stream, request_id, 400, "route loops through this relay")
				.await?;
			let _ = stream.writer.close().await;
			return Ok(());
		};

		match self.start_announce(path.clone(), advert) {
			Ok(_) => {
				if let Err(err) = self.write_ok(&mut stream, request_id).await {
					// Local rollback, not a peer unannounce: don't count announce bytes.
					let _ = self.stop_announce(path, Detach::Graceful);
					return Err(err);
				}
			}
			Err(err) => {
				self.write_error(&mut stream, request_id, 400, &err.to_string()).await?;
				let _ = stream.writer.close().await;
				return Ok(());
			}
		}

		// An endpoint updates an advertisement by re-sending it on the stream that
		// already carries it, so keep reading until the stream ends: a close on
		// draft-17+, or v14-16's PublishNamespaceDone (see `terminal_publish_namespace`).
		//
		// `attached` survives the call so a stream that detached mid-flight (a reflected
		// update) is not released twice here.
		let mut attached = true;
		let res = self
			.run_publish_namespace_updates(&mut stream, &path, request_id, peer, &mut attached)
			.await;

		if attached {
			// Ending cleanly IS the retraction here, unlike a NAMESPACE stream: this stream
			// carries exactly one advertisement, and withdrawing it is what ends the stream.
			// Any other ending left the advertisement standing, so the peer never withdrew
			// it and the loss reads as abrupt (an error, not a clean end).
			let detach = match res.is_ok() {
				true => Detach::Graceful,
				false => Detach::Abrupt,
			};
			self.stop_announce(path, detach)?;
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
		match self.version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => type_id == ietf::PublishNamespaceDone::ID,
			_ => false,
		}
	}

	/// Read advertisement updates off a live PUBLISH_NAMESPACE stream until it closes.
	async fn run_publish_namespace_updates(
		&mut self,
		stream: &mut Stream<S, Version>,
		path: &PathOwned,
		request_id: RequestId,
		peer: cluster::Peer,
		attached: &mut bool,
	) -> Result<(), Error> {
		loop {
			let type_id: u64 = match stream.reader.decode_maybe().await? {
				Some(id) => id,
				None => return Ok(()),
			};
			let terminal = self.terminal_publish_namespace(type_id);
			if type_id != ietf::PublishNamespace::ID && !terminal {
				tracing::warn!(type_id, "unexpected message on publish_namespace stream");
				return Err(Error::UnexpectedMessage);
			}

			let size: u16 = stream.reader.decode().await?;
			let mut data = stream.reader.read_exact(size as usize).await?;

			if terminal {
				ietf::PublishNamespaceDone::decode_msg(&mut data, self.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(%path, "publish_namespace_done");
				return Ok(());
			}

			let msg = ietf::PublishNamespace::decode_body(&mut data, self.version, peer.negotiated())?;
			// Junk inside the declared size would otherwise be applied silently, which
			// is the one decode path that skipped the check the others make.
			if !data.is_empty() {
				return Err(Error::WrongSize);
			}

			// The stream is the advertisement, so an update on it must name the same one.
			// Applying a mismatched update would retarget this path's routing with
			// metadata meant for a different request.
			if msg.request_id != request_id || msg.track_namespace.as_str() != path.as_str() {
				tracing::warn!(%path, "publish_namespace update does not match its stream");
				return Err(Error::ProtocolViolation);
			}

			// A path that now runs through us is unusable, so detach rather than keep
			// serving it. Keep reading though: this stream is the only channel the
			// advertisement has, so a later clean path arrives here or nowhere. Ending
			// the stream is also not ours to do, since a peer MAY legitimately send a
			// path carrying our Hop ID when a redundant sibling shares it.
			let Some(advert) = self.route(msg.cluster.as_ref(), &peer) else {
				if std::mem::take(attached) {
					tracing::debug!(%path, "publish_namespace now loops back; detaching");
					let _ = self.stop_announce(path.clone(), Detach::Graceful);
				}
				continue;
			};

			tracing::debug!(%path, hops = advert.route.hops.len(), cost = ?advert.route.cost, "publish_namespace update");
			match *attached {
				true => self.update_announce(path.clone(), advert)?,
				// Re-attach: a clean path replaced the reflected one we detached from.
				false => {
					self.start_announce(path.clone(), advert)?;
					*attached = true;
				}
			}
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

		// NOT_SUPPORTED, from the PUBLISH error codes in draft-19 section 10.10. We decline
		// the method itself rather than this particular track, which is UNINTERESTED (0x4).
		//
		// The alias the message carries is deliberately not recorded. Nothing will ever bind
		// it, and a rejected request has no lifetime of ours to hang the cleanup on, so the
		// entry would have to be swept asynchronously. Any data streams the publisher opened
		// before reading this are dropped by the unknown-alias path instead.
		const NOT_SUPPORTED: u64 = 0x3;

		self.write_publish_error(&mut stream, msg.request_id, NOT_SUPPORTED, "PUBLISH is not supported")
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

	/// Send error on the bidi stream.
	async fn write_error(
		&self,
		stream: &mut Stream<S, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
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

	async fn write_publish_error(
		&self,
		stream: &mut Stream<S, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
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
				entry.dynamic.update(route)?;
				Ok(())
			}
			Entry::Vacant(entry) => {
				// Propagates Error::Unauthorized if the namespace is out of scope.
				let dynamic = self.origin.dynamic(&path, route.clone())?;

				entry.insert(BroadcastState {
					route,
					dynamic,
					count: 1,
					sources: HashMap::new(),
				});

				tracing::debug!(route = %self.origin.absolute(&path), "announce");

				let this = self.clone();
				self.tasks.push(async move {
					// stop_announce is the authoritative remover: it drops the entry
					// (retracting the route) once the announce refcount hits zero,
					// which is what makes run_route exit.
					this.run_route(path).await;
				});

				Ok(())
			}
		}
	}

	fn stop_announce(&mut self, path: PathOwned, detach: Detach) -> Result<(), Error> {
		let mut state = self.state.lock();

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count -= 1;
				if entry.get().count == 0 {
					tracing::debug!(route = %self.origin.absolute(&path), ?detach, "unannounced");
					// Dropping the entry retracts the route (its announcement drops).
					let removed = entry.remove();
					for (_, source) in removed.sources {
						match detach {
							Detach::Graceful => source.finish(),
							// Dropping the guard aborts the source, so the loss reads
							// as an error rather than a clean end.
							Detach::Abrupt => {}
						}
					}
				}
			}
			Entry::Vacant(_) => return Err(Error::NotFound),
		};

		Ok(())
	}

	/// Serve materialization requests for one announced namespace: mint a source
	/// per requested path and serve its track requests until the route is
	/// retracted or the session dies.
	async fn run_route(&self, path: PathOwned) {
		let mut broadcasts = TaskSet::owned();
		let mut closed_session = self.session.clone();
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
					// The route lives in the entry: stop_announce removing it retracts
					// the route, and this loop ends with it.
					let mut state = self.state.lock();
					match state.broadcasts.get_mut(&path) {
						Some(entry) => entry.dynamic.poll_requested_broadcast(waiter).map(Some),
						None => Poll::Ready(None),
					}
				})
				.await;

			let request = match next {
				Some(Ok(request)) => request,
				// Retracted or torn down: no request will ever arrive again.
				Some(Err(_)) | None => break,
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

			// Retain the source so a retraction can finish it. If the route was
			// retracted since, the guard drops here and consumers observe the abort.
			{
				let mut state = self.state.lock();
				let Some(entry) = state.broadcasts.get_mut(&path) else {
					continue;
				};
				entry
					.sources
					.insert(requested.clone(), crate::model::broadcast::SourceGuard::new(source));
			}

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
		let _ = entry.dynamic.update(entry.route.clone());
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
		broadcast: broadcast::Dynamic,
		request: track::Request,
	) {
		// Accept right away: IETF group data can arrive before SubscribeOk, so we
		// need the producer in place to route it. This also unblocks the
		// downstream subscriber's `consume_track`.
		//
		// Set the track timescale to microseconds: IETF object timestamps default to
		// microseconds, and `create_frame` normalizes each frame into the track scale.
		// Accepting at milliseconds (the default) would truncate microsecond precision.
		//
		// moq-transport carries no publisher retention property, so the window comes
		// from the accepting side (see `origin::Info::default_max_age`) rather than
		// from the peer.
		let info = track::Info::default()
			.with_timescale(crate::Timescale::MICRO)
			.with_max_age(self.origin.default_max_age());
		let mut track = request.accept(info);

		// A peer that sent GOAWAY told us to stop opening streams on this session.
		if self.going_away.is_set() {
			let _ = track.abort(Error::GoingAway);
			return;
		}

		let request_id = match self.control.next_request_id(&self.runtime).await {
			Ok(id) => id,
			Err(err) => {
				let _ = track.abort(err);
				return;
			}
		};

		let mut stream = match Stream::open(&mut self.session.clone(), self.version).await {
			Ok(s) => s,
			Err(err) => {
				tracing::debug!(%err, "failed to open subscribe stream");
				let _ = track.abort(err);
				return;
			}
		};

		let subscription = track.subscription();
		let join = subscribe_join(
			subscription.as_ref().and_then(|s| s.start),
			subscription.as_ref().and_then(|s| s.end),
			self.version,
		);

		// Register the request before writing SUBSCRIBE so SUBSCRIBE_OK can bind its alias,
		// and so a fill fetch stream that overtakes it finds the subscription it answers.
		let fill = kio::Producer::new(match join.fill.is_some() {
			true => Fill::Requested,
			false => Fill::Done,
		});
		{
			let mut state = self.state.lock();
			state.subscribes.insert(
				request_id,
				TrackState {
					producer: track.clone(),
					alias: None,
					broadcast: broadcast_path.to_owned(),
					timescale: None,
					fill,
				},
			);
		}

		// Write Subscribe message
		if let Err(err) = self
			.write_subscribe(&mut stream, request_id, &broadcast_path, &track, join)
			.await
		{
			tracing::debug!(%err, "failed to write subscribe");
			self.remove_subscribe(request_id);
			let _ = track.abort(err);
			return;
		}

		tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track.name(), "subscribe started");

		// A publisher can be serving before its SUBSCRIBE_OK reaches us, since the data
		// streams are independent of the request stream. Waiting for the response alone would
		// miss the local side going away in that window and leave the publisher serving a
		// track nobody reads, which is the leak this whole path exists to close.
		enum Setup {
			Response(Result<Option<Accepted>, Error>),
			Unused,
			BroadcastClosed(Error),
		}

		let setup = {
			let mut response = std::pin::pin!(self.read_subscribe_response(&mut stream));
			kio::wait(|waiter| {
				// An answer that has already arrived wins over the local terminals. Both can
				// be ready in one poll, and taking abandonment there would discard a response
				// the publisher has already sent: if it was a rejection, the request is gone
				// and cancelling it names a dead id back at a peer entitled to object.
				if let Poll::Ready(res) = waiter.poll_future(response.as_mut()) {
					return Poll::Ready(Setup::Response(res));
				}
				if track.poll_unused(waiter).is_ready() {
					return Poll::Ready(Setup::Unused);
				}
				if let Poll::Ready(err) = broadcast.poll_closed(waiter) {
					return Poll::Ready(Setup::BroadcastClosed(err));
				}
				Poll::Pending
			})
			.await
		};

		// Abandoned before the publisher answered. It may already be serving, so this still
		// owes it a cancellation rather than a silent walk away.
		let response = match setup {
			Setup::Response(res) => res,
			Setup::Unused | Setup::BroadcastClosed(_) => {
				let err = match setup {
					Setup::BroadcastClosed(err) => err,
					_ => Error::Cancel,
				};

				tracing::info!(
					broadcast = %self.origin.absolute(&broadcast_path),
					track = %track.name(),
					"subscribe abandoned before it was accepted"
				);

				let _ = track.abort(err);
				self.remove_subscribe(request_id);
				self.cancel_subscribe(stream, request_id).await;
				return;
			}
		};

		// Read the response and register the alias mapping
		match response {
			Ok(Some(Accepted {
				alias,
				timescale,
				largest,
			})) => {
				{
					let mut state = self.state.lock();
					if let Some(track) = state.subscribes.get_mut(&request_id) {
						if let Some(timescale) = timescale {
							track.timescale = Some(timescale);
						}
						// The fill fetch stream can be waiting on this: the timescale is
						// what its object timestamps are in.
						if let Ok(mut fill) = track.fill.write()
							&& matches!(*fill, Fill::Requested)
						{
							// An empty track owes no fill: the publisher opens no fetch
							// stream for an empty range, so nothing would settle this and
							// every group would wait on a head that is never coming.
							*fill = match largest {
								Some(_) => Fill::Serving(timescale),
								None => Fill::Done,
							};
						}
					}
				}

				if let Err(err) = self.register_alias(request_id, alias) {
					// Only one alias naming two different tracks is the session's problem. A
					// shared alias we cannot demux, or a request that went away underneath us,
					// costs this subscription and nothing else.
					if matches!(err, Error::Duplicate) {
						tracing::warn!(track_alias = %alias, "publisher reused a live track alias for another track");
						self.session
							.close(SessionError::from(&err).to_code(), err.to_string().as_ref());
					} else {
						tracing::warn!(track_alias = %alias, %err, "could not bind track alias");
						// SUBSCRIBE_OK arrived, so the publisher considers this Established and
						// will keep serving it. Dropping the stream says nothing on the versions
						// that need UNSUBSCRIBE, so cancel it properly.
						self.cancel_subscribe(stream, request_id).await;
					}
					self.remove_subscribe(request_id);
					let _ = track.abort(err);
					return;
				}
			}
			Ok(None) => {}
			Err(err) => {
				tracing::debug!(%err, "subscribe response error");
				self.remove_subscribe(request_id);
				let _ = track.abort(err);
				return;
			}
		};

		// One event ends the subscription: the last consumer leaving, the broadcast
		// dying, or the subscribe stream closing.
		enum End {
			Unused,
			BroadcastClosed(Error),
			StreamClosed(Result<(), Error>),
		}

		let end = kio::wait(|waiter| {
			if track.poll_unused(waiter).is_ready() {
				return Poll::Ready(End::Unused);
			}
			if let Poll::Ready(err) = broadcast.poll_closed(waiter) {
				return Poll::Ready(End::BroadcastClosed(err));
			}
			let mut cx = std::task::Context::from_waker(waiter.waker());
			stream.reader.poll_closed(&mut cx).map(End::StreamClosed)
		})
		.await;

		// Whether we are walking away from a subscription the publisher still considers
		// Established, which is what obliges us to cancel it rather than just close.
		let cancelled = match end {
			End::Unused => {
				tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track.name(), "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
				true
			}
			End::BroadcastClosed(err) => {
				tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track.name(), "broadcast closed");
				let _ = track.abort(err);
				true
			}
			End::StreamClosed(res) => {
				match res {
					Ok(()) => {
						tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track.name(), "subscribe complete");
						let _ = track.finish();
					}
					Err(err) => {
						tracing::debug!(%err, "subscribe stream closed with error");
						let _ = track.abort(err);
					}
				}
				// The publisher already ended the request, so there is nothing to cancel.
				false
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
		reader.stop(super::error::CANCELLED);

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

	async fn write_subscribe(
		&self,
		stream: &mut Stream<S, Version>,
		request_id: RequestId,
		broadcast: &Path<'_>,
		track: &track::Producer,
		join: Join,
	) -> Result<(), Error> {
		stream.writer.encode(&ietf::Subscribe::ID).await?;
		stream
			.writer
			.encode(&ietf::Subscribe {
				request_id,
				track_namespace: broadcast.to_owned(),
				track_name: track.name().into(),
				subscriber_priority: super::priority::to_wire(track.subscription().map(|s| s.priority).unwrap_or(0)),
				group_order: GroupOrder::Descending,
				filter: join.filter,
				fill: join.fill,
				properties_wanted: true,
			})
			.await?;
		Ok(())
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
					largest: msg.largest,
				}))
			}
			ietf::SubscribeError::ID if self.version == Version::Draft14 => {
				let msg = ietf::SubscribeError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "subscribe error");
				Err(Error::Cancel)
			}
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "request error");
				Err(Error::Cancel)
			}
			_ => Err(Error::UnexpectedMessage),
		}
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let group: ietf::GroupHeader = stream.decode().await?;

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

		let (track, timescale, fill) = {
			let state = self.state.lock();
			let track = state.subscribes.get(&request_id).ok_or(Error::NotFound)?;
			(track.producer.clone(), track.timescale, track.fill.clone())
		};

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
		let (mut producer, start) = {
			let mut opening = track.clone();
			let mut open = std::pin::pin!(self.open_group(stream, &mut opening, &fill, group.group_id));
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_closed(waiter) {
					return Poll::Ready(Err(err));
				}
				waiter.poll_future(open.as_mut())
			})
			.await?
		};

		let res = {
			let mut ingest = GroupIngest::new(&group, timescale, self.version, start);
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
			Err(Error::Cancel) => {
				let _ = producer.abort(Error::Cancel);
			}
			Err(err) => {
				tracing::debug!(%err, group = %producer.sequence, "group error");
				let _ = producer.abort(err);
			}
			_ => {
				let _ = producer.finish();
			}
		}

		Ok(())
	}
}

impl<S, R> Subscriber<S, R>
where
	S: crate::transport::poll::Boxable,
	R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
	R::Timer: MaybeSend,
{
	/// The group producer this subgroup stream writes into, and the Object ID it starts at.
	///
	/// Normally the stream starts the group. While a fill is outstanding it may instead be
	/// the tail of the group the fill fetch stream began, which the first Object ID decides,
	/// so the stream is peeked before any producer exists. A subscription with no fill skips
	/// the peek: creating the group up front is what it has always done, and waiting for the
	/// first object would hold the group back for as long as the publisher takes to produce
	/// it.
	async fn open_group(
		&self,
		stream: &mut Reader<S::RecvStream, Version>,
		track: &mut track::Producer,
		fill: &kio::Producer<Fill>,
		sequence: u64,
	) -> Result<(group::Producer, u64), Error> {
		// Stats (groups/frames/bytes) are counted in the model as the group is written,
		// through the tagged `track::Producer`.
		let create = |track: &mut track::Producer| track.create_group(group::Info { sequence });

		if !fill.read().outstanding() {
			return Ok((create(track)?, 0));
		}

		// The first object's ID delta is its absolute Object ID (see `next_object_id`).
		match stream.decode_peek_maybe::<u64>().await? {
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

				Ok((create(track)?, 0))
			}

			// A group starting partway through is the tail of one the fill began, and
			// without that head it has a hole at the front.
			Some(start) => match self.claim_fill(fill, track, sequence, Some(start)).await? {
				Some(producer) => Ok((producer, start)),
				None => {
					tracing::warn!(sequence, start, "no fill to stitch a mid-group stream onto");
					Err(Error::Unsupported)
				}
			},

			// A stream that ends without an object: the group is over and had nothing
			// outside the fill's range, so the head it delivered is the whole group.
			None => match self.claim_fill(fill, track, sequence, None).await? {
				Some(producer) => Ok((producer, 0)),
				None => Ok((create(track)?, 0)),
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
	/// costs this stream and nothing else.
	async fn claim_fill(
		&self,
		fill: &kio::Producer<Fill>,
		track: &track::Producer,
		sequence: u64,
		start: Option<u64>,
	) -> Result<Option<group::Producer>, Error> {
		kio::wait(|waiter| {
			if let Poll::Ready(err) = track.poll_closed(waiter) {
				return Poll::Ready(Err(err));
			}

			let settled = fill.poll(waiter, |fill| match **fill {
				Fill::Requested | Fill::Serving(_) | Fill::Active => Poll::Pending,
				Fill::Ready { .. } | Fill::Done => Poll::Ready(()),
			});

			match settled {
				Poll::Ready(Ok(mut fill)) => Poll::Ready(fill.claim(sequence, start)),
				// The subscription went away underneath us.
				Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Dropped)),
				Poll::Pending => Poll::Pending,
			}
		})
		.await
	}
}

/// Pumps moq-transport subgroup objects from a reader into a group producer:
/// the id delta, the extension headers (carrying the timestamp), the size, the
/// status for empty objects, and the streamed payload.
struct GroupIngest {
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
	/// An explicit end-of-group status arrived.
	Finished,
}

impl GroupIngest {
	fn new(group: &ietf::GroupHeader, timescale: Option<Timescale>, version: Version, start: u64) -> Self {
		Self {
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

impl<S, R> Subscriber<S, R>
where
	S: crate::transport::poll::Boxable,
	R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
	R::Timer: MaybeSend,
{
	/// Read a fill fetch stream: the head of the group a subscription joins part way through
	/// (draft-20 section 5.1.3).
	///
	/// The stream answers the FILL_PARAMETERS we sent, named by the SUBSCRIBE's Request ID,
	/// so unlike a subgroup stream it needs no track alias. It writes the objects into a
	/// group producer of its own and hands that to the subgroup stream carrying the rest of
	/// the group; see [`Fill`]. A reset stream is the publisher's fill-failure signal, and
	/// arrives here as a read error, which drops the head and the join with it.
	pub async fn recv_fill(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		// The dispatcher peeked the stream type to get here.
		let _: u64 = stream.decode().await?;
		let header: ietf::FetchHeader = stream.decode().await?;

		let (track, fill) = {
			let state = self.state.lock();
			let track = state.subscribes.get(&header.request_id).ok_or(Error::NotFound)?;
			(track.producer.clone(), track.fill.clone())
		};

		// SUBSCRIBE_OK declares the units these object timestamps are in, and this stream can
		// be reordered ahead of it. Taking the fill in the same step is what refuses a second
		// stream for a request that asked for one fill.
		let timescale = kio::wait(|waiter| {
			if let Poll::Ready(err) = track.poll_closed(waiter) {
				return Poll::Ready(Err(err));
			}

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

		// Race the peer's stream against the subscription going away, the same way a
		// subgroup stream is served. Otherwise a peer that stalls partway through a payload
		// keeps this handler and its stream alive for as long as it cares to: aborting the
		// track does not close a group producer, since those lifecycles are independent.
		let res = {
			let mut serving = track.clone();
			let mut serve = std::pin::pin!(self.run_fill(stream, &mut serving, timescale));
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
	) -> Result<Fill, Error> {
		let mut head: Option<(u64, u64, group::Producer)> = None;

		match self.run_fill_objects(stream, track, timescale, &mut head).await {
			Ok(()) => Ok(match head {
				Some((sequence, next, producer)) => Fill::Ready {
					sequence,
					next,
					producer,
				},
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

	/// Decode fetch objects (draft-20 section 11.4.4) into `head`, creating its group from
	/// the first object's absolute IDs.
	///
	/// Only the shape our own fill request can produce is accepted: one group, one subgroup,
	/// and objects numbered from the group's start with no gaps. Anything else is a head the
	/// model cannot represent, and refusing the stream leaves the subscription itself alone.
	async fn run_fill_objects(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		track: &mut track::Producer,
		timescale: Option<Timescale>,
		head: &mut Option<(u64, u64, group::Producer)>,
	) -> Result<(), Error> {
		while let Some(object) = stream.decode_maybe::<ietf::FetchObject>().await? {
			let ietf::FetchObject::Object {
				subgroup,
				group,
				object,
				properties,
				..
			} = object
			else {
				// An End of Range names objects that do not exist, are unknown, or timed
				// out: a hole in the head, which the model cannot express.
				tracing::warn!("a fill with an End of Range cannot be stitched");
				return Err(Error::Unsupported);
			};

			// One subgroup per group, matching what we serve, so the head is a single
			// ordered run of objects.
			if !matches!(
				subgroup,
				ietf::FetchSubgroup::Zero | ietf::FetchSubgroup::Prior | ietf::FetchSubgroup::Explicit(0)
			) {
				tracing::warn!(?subgroup, "subgroup ID is not supported, dropping fill");
				return Err(Error::Unsupported);
			}

			match head {
				// The first object carries the absolute Group and Object IDs. It has to be
				// the group's own first object, or the head is not a decodable prefix.
				None => {
					let (Some(sequence), Some(0)) = (group, object) else {
						tracing::warn!(?group, ?object, "a fill must start at a group's first object");
						return Err(Error::Unsupported);
					};

					let producer = track.create_group(group::Info { sequence })?;
					*head = Some((sequence, 0, producer));
				}
				// A Group ID on a later object names a different group. We ask for the
				// current group only, and a publisher refuses a wider fill rather than
				// serving it.
				Some(_) if group.is_some() => {
					tracing::warn!("a fill spanning several groups cannot be stitched");
					return Err(Error::Unsupported);
				}
				// Without a Group ID the Object ID is the prior one plus the delta, or plus
				// one when the delta is absent. Anything else skips an object.
				Some((sequence, next, _)) => {
					let delta = object.unwrap_or(1);
					if delta != 1 {
						tracing::warn!(
							sequence = *sequence,
							next = *next,
							delta,
							"fill object IDs must increment by 1"
						);
						return Err(Error::Unsupported);
					}
				}
			}

			// The properties carry the frame's presentation timestamp (the Timestamp Object
			// Property) in the units the track declared. A track that declared none opted
			// out, so its frames are stamped on arrival instead.
			let timestamp = match (properties, timescale) {
				(Some(properties), Some(timescale)) => {
					let mut properties = bytes::Bytes::from(properties);
					ietf::decode_object_time(&mut properties, timescale, self.version)?
				}
				_ => None,
			};
			let timestamp = timestamp.unwrap_or_else(crate::Timestamp::now);

			// A fetch object has no status field: a zero length is simply an empty object.
			let size: u64 = stream.decode().await?;

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

impl GroupIngest {
	/// `Ready(Ok(()))` once the stream FINs on an object boundary (or an explicit
	/// end-of-group status arrives). The caller finishes or aborts the group; an
	/// object cut short mid-payload was already aborted here with the reason.
	fn poll<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut Reader<R, Version>,
		group: &mut group::Producer,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		loop {
			match &mut self.phase {
				IngestPhase::Delta => {
					let Some(id_delta) = ready!(reader.poll_decode_maybe::<u64>(&mut cx))? else {
						return Poll::Ready(Ok(()));
					};
					self.prior_object = Some(next_object_id(self.prior_object, id_delta, self.start)?);
					self.phase = match self.has_extensions {
						true => IngestPhase::ExtSize,
						false => IngestPhase::Size { timestamp: None },
					};
				}
				IngestPhase::ExtSize => {
					let size: usize = ready!(reader.poll_decode(&mut cx))?;
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
					let timestamp = timestamp.unwrap_or_else(crate::Timestamp::now);
					let frame = group.create_frame_owned(frame::Info { size, timestamp })?;
					self.phase = IngestPhase::Payload { frame };
				}
				IngestPhase::Status { timestamp } => {
					let status: u64 = ready!(reader.poll_decode(&mut cx))?;
					if status == 0 {
						let timestamp = timestamp.unwrap_or_else(crate::Timestamp::now);
						let frame = group.create_frame_owned(frame::Info { size: 0, timestamp })?;
						frame.finish()?;
						self.phase = IngestPhase::Delta;
					} else if status == 3 && !self.has_end {
						self.phase = IngestPhase::Finished;
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
				IngestPhase::Finished => return Poll::Ready(Ok(())),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::model::ProduceTest;
	use futures::poll;

	use super::*;

	/// The tokio-backed test runtime. Its transport parameter is phantom, so one
	/// type serves every fake session in this module.
	type TestRuntime = crate::runtime::tokio_test::Tokio<crate::lite::test_transport::SinkSession>;

	#[tokio::test(start_paused = true)]
	async fn track_alias_waits_for_control_message() {
		let runtime = TestRuntime::new();
		let aliases = TrackAliases::default();
		let pending = resolve_track_alias(&runtime, aliases.consume(), 7);
		tokio::pin!(pending);

		assert!(poll!(&mut pending).is_pending());

		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();

		assert_eq!(pending.await.unwrap(), RequestId(11));
	}

	#[tokio::test(start_paused = true)]
	async fn unknown_track_alias_times_out() {
		let aliases = TrackAliases::default();
		assert!(matches!(
			resolve_track_alias(&TestRuntime::new(), aliases.consume(), 7).await,
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
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			solicit,
			..Default::default()
		});
		let (tasks, _task_set) = crate::util::TaskSet::new();

		Subscriber::new(
			TestRuntime::new(),
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
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let scoped = origin
			.with_root("rootns")
			.and_then(|rooted| rooted.scope(&[crate::Path::new("cam")]))
			.expect("scope the origin");

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			TestRuntime::new(),
			session.clone(),
			scoped,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let scoped = origin
			.with_root("rootns")
			.and_then(|rooted| rooted.scope(&[crate::Path::new("cam")]))
			.expect("scope the origin");

		let session = crate::lite::test_transport::ScriptedSession::new(namespace_response(VERSION, "x.hang").await);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		// Draft-18 can negotiate the cluster extension, so the subscriber waits for
		// the peer's SETUP before resolving advertisements; settle it as extension-off.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let mut subscriber = Subscriber::new(
			TestRuntime::new(),
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

		let runtime = TestRuntime::new();
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
	/// that sends it: `run_unis` is private to `session`, and retiring an alias reaches into
	/// state private to this module, so nothing here can drive one end to end. See #3002.
	#[tokio::test(start_paused = true)]
	async fn a_retired_alias_maps_to_the_cancelled_code() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(11));

		let err = resolve_track_alias(&TestRuntime::new(), aliases.consume(), 7)
			.await
			.expect_err("a retired alias resolves to a cancellation");

		assert_eq!(
			crate::ietf::error::to_stream_code(&err),
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
	) -> Subscriber<crate::lite::test_transport::SinkSession, TestRuntime> {
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The tests drive binding directly, so nothing spawns; leaking keeps the handle alive
		// without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			TestRuntime::new(),
			crate::lite::test_transport::SinkSession::new(Default::default()),
			crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce(),
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
					TrackState {
						producer: track::Producer::new(
							std::sync::Arc::new(crate::broadcast::Info::default()),
							*name,
							None,
						),
						alias: None,
						broadcast: Path::new(broadcast).to_owned(),
						timescale: None,
						fill: kio::Producer::new(Fill::Done),
					},
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
				Error::Cancel.to_code(),
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
					error_code: 404,
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
			TestRuntime::new(),
			session,
			crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce(),
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
			TestRuntime::new(),
			session,
			crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce(),
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

	/// The control messages that actually reached the wire, by type id.
	///
	/// Decoding the framing rather than scanning for a byte: a type id is one varint among
	/// many, and a substring match would happily find one inside a length or a payload.
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
			TestRuntime::new(),
			adapter,
			crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce(),
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
			TestRuntime::new(),
			session,
			crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce(),
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
					producer: track::Producer::new(
						std::sync::Arc::new(crate::broadcast::Info::default()),
						"video",
						None,
					),
					alias: Some(7),
					broadcast: Path::new("broadcast").to_owned(),
					timescale: None,
					fill: kio::Producer::new(Fill::Done),
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
	async fn fill_after_subscribe_ok(largest: Option<ietf::Location>) -> bool {
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
					properties: Default::default(),
				})
				.await
				.unwrap();

			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			TestRuntime::new(),
			session,
			crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce(),
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
			!fill_after_subscribe_ok(None).await,
			"no LARGEST_OBJECT means no content, so no fill is owed"
		);
	}

	/// A track with content does owe one, so the fill stays outstanding until its fetch
	/// stream arrives.
	#[tokio::test(start_paused = true)]
	async fn a_track_with_content_still_awaits_its_fill() {
		assert!(
			fill_after_subscribe_ok(Some(ietf::Location { group: 3, object: 4 })).await,
			"a fetch stream is still owed"
		);
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

	/// moq-transport carries no hop ids, so a peer's broadcasts are normally
	/// attributed to a random per-connection origin. An identity assigned via
	/// `Client::with_peer_hop` pins it, so sessions dialing the same relay
	/// resolve to one recognizable route, and a reconnect splices rather than
	/// replacing.
	#[tokio::test]
	async fn assigned_peer_hop_attributes_announces() {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let assigned = crate::Hop::new(777).unwrap();

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			TestRuntime::new(),
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
		assert_eq!(hops, vec![assigned]);
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

		let origin = crate::origin::Info::new(self_origin).produce();
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
			TestRuntime::new(),
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

	/// The assigned identity is a content identity too, so a second session dialing
	/// the same relay produces the same first hop and splices into the front its
	/// predecessor is serving. That is what makes a reconnect immediate instead of
	/// waiting for the transport to retire the dead session, and the reflection guard
	/// must not cost us it.
	#[tokio::test]
	async fn reconnecting_peer_joins_the_front_it_replaces() {
		let peer = crate::Hop::new(777).unwrap();
		let self_origin = crate::Hop::new(1).unwrap();

		let origin = crate::origin::Info::new(self_origin).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let connect = || {
			let (tasks, task_set) = crate::util::TaskSet::new();
			std::mem::forget(task_set);
			let mut subscriber = Subscriber::new(
				TestRuntime::new(),
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
		announced.assert_next_active("room/host");

		// The peer reconnects before the old session is retired: an identical route
		// from the fresh session joins without any consumer-visible churn.
		let _second = connect();
		announced.assert_next_wait();

		// The stale session finally retracting leaves the fresh route standing.
		drop(first);
		announced.assert_next_wait();
		assert!(routed_now(&consumer, "room/host").is_some());
	}

	fn cluster_subscriber(
		self_origin: crate::Hop,
	) -> (
		Subscriber<crate::lite::test_transport::SinkSession, TestRuntime>,
		crate::origin::Producer,
	) {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let origin = crate::origin::Info::new(self_origin).produce();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The set only drains announce-serving tasks; the tests here drive the model
		// directly, so leaking it keeps the handles alive without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			TestRuntime::new(),
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
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
			TestRuntime::new(),
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

		subscriber.stop_announce(path, Detach::Graceful).unwrap();
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let mut subscriber = Subscriber::new(
			TestRuntime::new(),
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

		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
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
			TestRuntime::new(),
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
	/// last, whatever kind each detach is.
	///
	/// That is the model's own rule for several sources at one path (`detach_source`),
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
		subscriber.stop_announce(path.clone(), Detach::Abrupt).unwrap();
		settle().await;
		assert!(
			routed_now(&consumer, "room/host").is_some(),
			"the broadcast must survive while an owner remains",
		);

		// The last owner retracts: the broadcast closes with it.
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
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
	/// not, and the source is not torn down. Only a changed original publisher replaces
	/// it, since that content is not interchangeable.
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
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
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
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
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
		subscriber.stop_announce(path.clone(), Detach::Graceful).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
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
		subscriber.stop_announce(path.clone(), Detach::Graceful).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
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
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"the superseded route must not stay attached"
		);
	}

	/// The peer's advertisement updates, framed exactly as the update loop reads them,
	/// built with the crate's own writer so the framing cannot drift from the encoder.
	async fn publish_namespace_updates(
		request_id: RequestId,
		path: &str,
		updates: &[Option<cluster::Advert>],
	) -> Vec<u8> {
		const VERSION: Version = Version::Draft19;
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);

		for cluster in updates {
			writer.encode(&ietf::PublishNamespace::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespace {
					request_id,
					track_namespace: crate::Path::new(path),
					cluster: cluster.clone(),
				})
				.await
				.unwrap();
		}

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Build a subscriber whose peer replays `updates` on one PUBLISH_NAMESPACE stream,
	/// with the advertisement already attached.
	async fn reflected_harness(
		self_origin: crate::Hop,
		request_id: RequestId,
		peer: &cluster::Peer,
		attached: &cluster::Advert,
		updates: &[Option<cluster::Advert>],
	) -> (
		Subscriber<crate::lite::test_transport::ScriptedSession, TestRuntime>,
		crate::origin::Consumer,
		Stream<crate::lite::test_transport::ScriptedSession, Version>,
	) {
		const VERSION: Version = Version::Draft19;
		let script = publish_namespace_updates(request_id, "room/host", updates).await;
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let origin = crate::origin::Info::new(self_origin).produce();
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The tests drive the loop directly, so nothing spawns; leaking keeps the
		// handles alive without a spawner.
		std::mem::forget(task_set);

		let mut subscriber = Subscriber::new(
			TestRuntime::new(),
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
		(subscriber, consumer, stream)
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
		let request_id = RequestId(1);
		let peer = peer_9();
		let (clean, looped) = clean_and_looped();

		let (mut subscriber, consumer, mut stream) =
			reflected_harness(self_origin, request_id, &peer, &clean, &[Some(looped)]).await;

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				request_id,
				peer,
				&mut attached,
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
	}

	/// Having kept the stream, a later usable path re-attaches on it. This is the whole
	/// reason the stream stays open.
	#[tokio::test]
	async fn a_clean_update_after_a_reflection_reattaches() {
		let self_origin = crate::Hop::new(5).unwrap();
		let request_id = RequestId(1);
		let peer = peer_9();
		let (clean, looped) = clean_and_looped();

		let (mut subscriber, consumer, mut stream) = reflected_harness(
			self_origin,
			request_id,
			&peer,
			&clean,
			&[Some(looped), Some(clean.clone())],
		)
		.await;

		let path = crate::Path::new("room/host").to_owned();
		let mut attached = true;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				request_id,
				peer,
				&mut attached,
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
			subscriber.stop_announce(path, Detach::Graceful).unwrap();
		}

		assert!(routed_now(&consumer, "room/a").is_none(), "room/a leaked a refcount");
		assert!(routed_now(&consumer, "room/b").is_none(), "room/b leaked a refcount");
	}

	/// PUBLISH offers one track, but a source attaches per namespace and serves every
	/// track under it. Rather than invent a namespace-level source from a track-level
	/// offer, decline the request and leave the session running.
	#[tokio::test]
	async fn publish_is_rejected_without_announcing() {
		// An open gate, so the rejection actually reaches the wire.
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);

		let mut subscriber = Subscriber::new(
			TestRuntime::new(),
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft19,
			tasks,
			Default::default(),
		);

		let stream = Stream::open(&mut session.clone(), Version::Draft19).await.unwrap();
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
		// Encode the reply we expect rather than matching the reason alone, so the error code
		// regressing to something outside draft-19 section 10.10's table cannot slip through.
		let expected = {
			const NOT_SUPPORTED: u64 = 0x3;

			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::lite::test_transport::SinkSend::new(log.clone()),
				Version::Draft19,
			);
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

			log.writes.lock().unwrap().clone()
		};

		assert_eq!(
			occurrences(&session.log, &expected),
			1,
			"the decline reaches the peer as NOT_SUPPORTED"
		);
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
}

/// What a moq-lite subscription's group range asks for on the wire.
///
/// moq-lite joins a track at the *start* of the current group, which is a decodable point.
/// Draft-20 spells that as the draft's own current-group join (section 5.1.6): a Next
/// Object subscription plus a `StartGroup=1` fill, which is the only form a publisher has
/// to honor. It splits the group across two streams, the fill carrying the head and the
/// subscription the tail, which `claim_fill` stitches back into one group producer.
/// Earlier drafts cannot name a fill at all, so they keep asking for the next Object and
/// joining mid-group, exactly as they always did.
/// A start group we already know is absolute and needs no fill: the subscription's own
/// range covers it, which is what our publisher serves from its cache.
fn subscribe_join(start: Option<track::Position>, end: Option<track::Position>, version: Version) -> Join {
	if !Filter::is_draft20(version) {
		return Join {
			filter: Filter::NextObject,
			fill: None,
		};
	}

	match start {
		// The live join: everything after the live edge, plus the current group's head.
		None => Join {
			filter: Filter::NextObject,
			fill: Some(ietf::Fill {
				// One group back from the next group is the current one.
				filter: Some(Filter::Relative(1)),
				range_filters: false,
			}),
		},
		// An absolute {0, 0} with no end is defined as unfiltered, so it spells itself.
		Some(start) if start == track::Position::group(0) && end.is_none() => Join {
			filter: Filter::Unfiltered,
			fill: None,
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
		},
	}
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
			subscribe_join(None, None, Version::Draft20),
			Join {
				filter: Filter::NextObject,
				fill: Some(ietf::Fill {
					filter: Some(Filter::Relative(1)),
					range_filters: false,
				}),
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
			),
			Join {
				filter: Filter::Absolute {
					start: ietf::Location { group: 7, object: 0 },
					end: Some(ietf::EndLocation { group: 9, object: None }),
				},
				fill: None,
			}
		);
	}

	/// The whole track has a spelling of its own: an absent filter is unrestricted.
	#[test]
	fn the_whole_track_is_unfiltered() {
		assert_eq!(
			subscribe_join(Some(track::Position::group(0)), None, Version::Draft20),
			Join {
				filter: Filter::Unfiltered,
				fill: None,
			}
		);
	}

	/// Earlier drafts have no fill and no way to name a group relative to a live edge they
	/// have not learned, so they keep asking for exactly what they always did.
	#[test]
	fn older_drafts_ask_for_the_next_object() {
		for version in [Version::Draft14, Version::Draft16, Version::Draft19] {
			assert_eq!(
				subscribe_join(
					Some(track::Position::group(7)),
					track::Position::after_group(9),
					version
				),
				Join {
					filter: Filter::NextObject,
					fill: None,
				},
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

	type TestRuntime = crate::runtime::tokio_test::Tokio<ScriptedSession>;

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
		let mut buf = bytes::BytesMut::new();
		ietf::FetchHeader::TYPE.encode(&mut buf, VERSION).unwrap();
		ietf::FetchHeader { request_id: REQUEST }
			.encode(&mut buf, VERSION)
			.unwrap();

		for (index, payload) in payloads.iter().enumerate() {
			let mut properties = bytes::BytesMut::new();
			ietf::encode_object_time(&mut properties, timestamp(index), Timescale::MICRO, VERSION).unwrap();

			// Only the first object carries absolute IDs; the rest inherit and increment.
			let first = index == 0;
			ietf::FetchObject::Object {
				subgroup: ietf::FetchSubgroup::Zero,
				group: first.then_some(sequence),
				object: first.then_some(0),
				priority: first.then_some(0),
				properties: Some(properties.to_vec()),
			}
			.encode(&mut buf, VERSION)
			.unwrap();

			(payload.len() as u64).encode(&mut buf, VERSION).unwrap();
			buf.put_slice(payload);
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
		subscriber: Subscriber<ScriptedSession, TestRuntime>,
		session: ScriptedSession,
		track: track::Producer,
		fill: kio::Producer<Fill>,
		_tasks: (Tasks, TaskSet),
	}

	impl Harness {
		fn new(fill: Fill, scripts: Vec<Vec<u8>>) -> Self {
			let session = ScriptedSession::per_stream_eof(scripts);
			let origin = crate::origin::Info::new(crate::Hop::new(1).unwrap()).produce();
			let tasks = TaskSet::new();

			let subscriber = Subscriber::new(
				TestRuntime::new(),
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
						producer: track.clone(),
						alias: Some(ALIAS),
						broadcast: Path::new("broadcast").to_owned(),
						timescale: Some(Timescale::MICRO),
						fill: fill.clone(),
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
		let mut track = track::Producer::new(
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
}
