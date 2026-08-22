use std::{
	collections::{HashMap, hash_map::Entry},
	task::Poll,
	time::Duration,
};

use crate::{
	Error, Path, PathOwned, Timescale, broadcast,
	coding::{Reader, Stream},
	frame, group,
	ietf::{self, Control, FilterType, GroupOrder, RequestId},
	origin, track,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, Tasks},
};

use super::{Message, Version, cluster, peer};

use web_async::Lock;

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

struct TrackState {
	producer: track::Producer,
	alias: Option<u64>,

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
	/// The stream carrying the path went away without retracting it. Leave the
	/// origin's linger window open so a source reconnecting into it resumes the
	/// broadcast instead of viewers seeing it end here.
	Abrupt,
}

struct BroadcastState {
	// The source feeding this broadcast into our origin: finish() on a
	// deliberate unannounce, dropping (a dying session) aborts it. Either way
	// the origin unannounces once the last source detaches.
	producer: crate::model::broadcast::SourceGuard,

	// active number of PUBLISH_NAMESPACE messages.
	count: usize,

	// The first entry of the advertised path: the original publisher. `None` on an
	// advertisement that carried no path at all, which is every one on a session
	// without the MoQ Cluster extension. See [`Advertised::publisher`].
	publisher: Option<crate::Origin>,
}

/// What one advertisement said, once its parameters are resolved against the session.
struct Advertised {
	/// The route it describes, with this link's price already charged.
	route: broadcast::Route,

	/// The original publisher: the first entry of HOP_PATH. Two advertisements that
	/// share a non-zero one carry interchangeable content, so a new path splices in.
	/// A different one, or `Some(Origin::UNKNOWN)` (which identifies nothing), is a
	/// distinct publisher reusing the namespace and must not splice. That comparison
	/// only applies between separate advertisements; see [`Arrival`].
	///
	/// `None` when the advertisement carried no path, so there is no identity to
	/// compare and nothing ever replaces the source on identity grounds. That covers
	/// base moq-transport entirely: PUBLISH_NAMESPACE and NAMESPACE for one namespace
	/// are then two messages describing a single source, not two publishers.
	publisher: Option<crate::Origin>,
}

/// How an advertisement relates to whatever already holds the path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arrival {
	/// A separate advertisement for a path another one already holds: a
	/// PUBLISH_NAMESPACE alongside a NAMESPACE. Nothing links the two but the publisher
	/// identity, so only a matching, real one proves they carry the same content.
	Separate,

	/// A repeat of the advertisement already holding the path, on the stream that
	/// carries it. That stream is the continuity: the peer never retracted the
	/// advertisement, so only a *changed* publisher is a new publisher. The expected
	/// repeat is a repricing, and a peer that withholds its identity reprices as often
	/// as any other.
	Repeat,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Producer,
	control: Control,
	// The origin naming this link, appended to the hop chain of every broadcast from
	// this session when the peer declares none of its own (see `session_route`). Base
	// moq-transport carries no hop ids, so a peer only has an identity if it negotiated
	// the MoQ Cluster extension or the caller assigned it one
	// (`Client::with_peer_origin`), which also makes the route recognizable across
	// sessions dialing the same relay.
	//
	// Otherwise this is `Origin::UNKNOWN` (0), the reserved "no identity" value.
	// Inventing a random id here would look like a real identity without being
	// one: the peer never learns it, so it cannot exclude it for loop detection,
	// and two unrelated publishers would each get an id that made their content
	// look distinct rather than merely unknown.
	session_origin: crate::Origin,
	// Our own Hop ID, which an advertisement must not already contain: one that does
	// looped back through us.
	self_origin: crate::Origin,
	// What the peer declared in its SETUP.
	peer_setup: peer::PeerSetup,
	// Local policy for what pulling from this peer costs, overriding whatever it
	// declared. See `cluster::link_cost`.
	cost: Option<u64>,
	state: Lock<State>,
	tasks: Tasks,
	version: Version,
}

/// Resolve the subscription a data stream belongs to.
///
/// SUBSCRIBE_OK can be reordered behind the stream it describes, so an alias we have not
/// seen is worth waiting on briefly (draft-19 section 11.4.2). Three outcomes:
/// the subscription, [`Error::Cancel`] for an alias we retired, and [`Error::NotFound`]
/// once the wait expires without any binding at all.
async fn resolve_track_alias(aliases: kio::Consumer<AliasTable>, alias: u64) -> Result<RequestId, Error> {
	let mut timeout = kio::time::Deadline::after(TRACK_ALIAS_TIMEOUT);
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

impl<S: web_transport_trait::Session> Subscriber<S> {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		session: S,
		origin: origin::Producer,
		control: Control,
		peer_origin: Option<crate::Origin>,
		peer_setup: peer::PeerSetup,
		self_origin: crate::Origin,
		cost: Option<u64>,
		version: Version,
		tasks: Tasks,
	) -> Self {
		Self {
			session,
			origin,
			control,
			session_origin: peer_origin.unwrap_or(crate::Origin::UNKNOWN),
			self_origin,
			peer_setup,
			cost,
			state: Default::default(),
			tasks,
			version,
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
	/// attributed to this session (`Origin::UNKNOWN` unless the peer or the caller
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
	fn session_route(&self, peer: &cluster::Peer) -> broadcast::Route {
		let mut hops = crate::OriginList::new();
		hops.push(self.session_origin)
			.expect("an empty hop chain has room for one entry");
		broadcast::Route::new()
			.with_hops(hops)
			.with_cost(cluster::link_cost(self.cost, peer))
			.with_announce(true)
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
				publisher: None,
			});
		};

		if advert.loops(self.self_origin) {
			return None;
		}

		Some(Advertised {
			route: advert.route(cluster::link_cost(self.cost, peer)),
			publisher: Some(
				advert
					.hops
					.hops()
					.iter()
					.next()
					.copied()
					.unwrap_or(crate::Origin::UNKNOWN),
			),
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
	pub async fn run_subscribe_namespace<T: web_transport_trait::Session>(
		&mut self,
		mut stream: Stream<T, Version>,
		prefix: PathOwned,
	) -> Result<(), Error> {
		let request_id = self.control.next_request_id().await?;

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
	async fn run_namespace_entries<T: web_transport_trait::Session>(
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

					tracing::debug!(%path, hops = advert.route.hops.len(), cost = advert.route.cost, "namespace");
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
							this.session.close(err.to_code(), err.to_string().as_ref());
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
			// it and the origin's linger window stays open for the reconnect.
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

			tracing::debug!(%path, hops = advert.route.hops.len(), cost = advert.route.cost, "publish_namespace update");
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

	/// Attach a source for one newly advertised path, bumping its refcount.
	///
	/// `route` is what the advertisement described (see [`Self::route`]), or `None` for
	/// a PUBLISH, which carries no path of its own and must not flatten the route an
	/// advertisement already set. Pair with [`Self::stop_announce`].
	fn start_announce(&mut self, path: PathOwned, advert: Advertised) -> Result<broadcast::Producer, Error> {
		let mut state = self.state.lock();
		let existing = state.broadcasts.contains_key(&path);
		let producer = self.attach(&mut state, path.clone(), advert, Arrival::Separate)?;
		if existing && let Some(entry) = state.broadcasts.get_mut(&path) {
			// The path was already attached, so this is one more advertisement for it.
			// A replacement carries the previous count forward, so the increment still
			// applies; only a freshly created entry starts at one and skips this.
			entry.count += 1;
		}
		Ok(producer)
	}

	/// Apply a changed advertisement to a path that is already attached.
	///
	/// An update replaces the advertisement atomically: the refcount does not move, and
	/// no subscription is torn down merely because one arrived. A changed original
	/// publisher is the exception, since that content is not interchangeable.
	fn update_announce(&mut self, path: PathOwned, advert: Advertised) -> Result<(), Error> {
		let mut state = self.state.lock();
		if !state.broadcasts.contains_key(&path) {
			return Err(Error::NotFound);
		}
		self.attach(&mut state, path, advert, Arrival::Repeat)?;
		Ok(())
	}

	/// Create or update the source for one path, leaving the refcount to the caller.
	///
	/// The ingress announce stats (announced / announced_bytes) are driven in the model
	/// by `create_broadcast`'s route transitions below.
	fn attach(
		&self,
		state: &mut State,
		path: PathOwned,
		advert: Advertised,
		arrival: Arrival,
	) -> Result<broadcast::Producer, Error> {
		let publisher = advert.publisher;

		// A different original publisher means a distinct broadcast has taken over this
		// namespace. Its content is not interchangeable, so detach the old source and
		// attach fresh instead of splicing the route in place; cached tracks and
		// subscriptions must not carry over. The refcount rides along: the same
		// advertisements still reference this path.
		//
		// Both sides must be known for the comparison to mean anything. A session
		// without the MoQ Cluster extension has no identity on either, so nothing ever
		// replaces there: its PUBLISH_NAMESPACE and NAMESPACE for one namespace are two
		// messages about a single source, and replacing on the second would tear down
		// what the first attached.
		let mut carried = None;
		if let Entry::Occupied(entry) = state.broadcasts.entry(path.clone())
			&& let (Some(old), Some(new)) = (entry.get().publisher, publisher)
			&& match arrival {
				// Two advertisements, so identity is the only link between them and
				// UNKNOWN is no link at all: unrelated publishers all present the same
				// one. Take the later as replacing the earlier.
				Arrival::Separate => old != new || new == crate::Origin::UNKNOWN,
				// One advertisement, repeated on the stream that carries it. Continuity
				// is the stream, not the identity, so an unchanged UNKNOWN stays put; a
				// peer that never named itself would otherwise lose every subscription
				// each time it repriced.
				Arrival::Repeat => old != new,
			} {
			tracing::debug!(broadcast = %self.origin.absolute(&path), "publisher changed; replacing the source");
			carried = Some(entry.get().count);
			entry.remove().producer.finish();
		}

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(entry) => {
				let mut producer = entry.get().producer.producer();
				producer.set_route(advert.route)?;
				Ok(producer)
			}
			Entry::Vacant(entry) => {
				let route = advert.route;

				// Propagates Error::Unauthorized if the path is out of scope.
				let broadcast = self.origin.create_broadcast(&path, route)?;

				// Register the dynamic handler synchronously: the broadcast only
				// becomes visible to consumers after this function returns to the
				// executor, so the origin's first track dispatch finds a handler
				// (mirrors the note in lite::Subscriber).
				let dynamic = broadcast.dynamic();

				entry.insert(BroadcastState {
					producer: crate::model::broadcast::SourceGuard::new(broadcast.clone()),
					count: carried.unwrap_or(1),
					publisher,
				});

				tracing::debug!(broadcast = %self.origin.absolute(&path), "announce");

				let this = self.clone();
				self.tasks.push(async move {
					// stop_announce is the authoritative remover: it drops the entry (and
					// its producer) once the announce refcount hits zero, which is what
					// makes run_broadcast exit. Removing here too would let a stale task
					// delete a freshly re-announced entry for the same path.
					if let Err(err) = this.run_broadcast(path, dynamic).await {
						tracing::debug!(%err, "error running broadcast");
					}
				});

				Ok(broadcast)
			}
		}
	}

	fn stop_announce(&mut self, path: PathOwned, detach: Detach) -> Result<(), Error> {
		let mut state = self.state.lock();

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count -= 1;
				if entry.get().count == 0 {
					tracing::debug!(broadcast = %self.origin.absolute(&path), ?detach, "unannounced");
					let producer = entry.remove().producer;
					match detach {
						Detach::Graceful => producer.finish(),
						// Dropping the guard aborts the source, which is what leaves the
						// origin's linger window open for a replacement.
						Detach::Abrupt => drop(producer),
					}
				}
			}
			Entry::Vacant(_) => return Err(Error::NotFound),
		};

		Ok(())
	}

	async fn run_broadcast(&self, path: Path<'_>, mut broadcast: broadcast::Dynamic) -> Result<(), Error> {
		let mut subscribes = TaskSet::owned();
		loop {
			let next = subscribes
				.drive(async {
					let mut closed = std::pin::pin!(self.session.closed());
					kio::wait(|waiter| {
						if waiter.poll_future(closed.as_mut()).is_ready() {
							return Poll::Ready(None);
						}
						broadcast.poll_requested_track(waiter).map(Some)
					})
					.await
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
		// from the accepting side (see `origin::Info::latency_default`) rather than
		// from the peer.
		let info = track::Info::default()
			.with_timescale(crate::Timescale::MICRO)
			.with_latency_max(self.origin.latency_default());
		let mut track = request.accept(info);

		let request_id = match self.control.next_request_id().await {
			Ok(id) => id,
			Err(err) => {
				let _ = track.abort(err);
				return;
			}
		};

		let mut stream = match Stream::open(&self.session, self.version).await {
			Ok(s) => s,
			Err(err) => {
				tracing::debug!(%err, "failed to open subscribe stream");
				let _ = track.abort(err);
				return;
			}
		};

		// Register the request before writing SUBSCRIBE so SUBSCRIBE_OK can bind its alias.
		{
			let mut state = self.state.lock();
			state.subscribes.insert(
				request_id,
				TrackState {
					producer: track.clone(),
					alias: None,
					broadcast: broadcast_path.to_owned(),
					timescale: None,
				},
			);
		}

		// Write Subscribe message
		if let Err(err) = self
			.write_subscribe(&mut stream, request_id, &broadcast_path, &track)
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
			Response(Result<Option<(u64, Option<Timescale>)>, Error>),
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
			Ok(Some((alias, timescale))) => {
				if let Some(timescale) = timescale {
					let mut state = self.state.lock();
					if let Some(track) = state.subscribes.get_mut(&request_id) {
						track.timescale = Some(timescale);
					}
				}

				if let Err(err) = self.register_alias(request_id, alias) {
					// Only one alias naming two different tracks is the session's problem. A
					// shared alias we cannot demux, or a request that went away underneath us,
					// costs this subscription and nothing else.
					if matches!(err, Error::Duplicate) {
						tracing::warn!(track_alias = %alias, "publisher reused a live track alias for another track");
						self.session.close(err.to_code(), err.to_string().as_ref());
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

		let end = {
			let mut closed = std::pin::pin!(stream.reader.closed());
			kio::wait(|waiter| {
				if track.poll_unused(waiter).is_ready() {
					return Poll::Ready(End::Unused);
				}
				if let Poll::Ready(err) = broadcast.poll_closed(waiter) {
					return Poll::Ready(End::BroadcastClosed(err));
				}
				waiter.poll_future(closed.as_mut()).map(End::StreamClosed)
			})
			.await
		};

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
				filter_type: FilterType::LargestObject,
			})
			.await?;
		Ok(())
	}

	async fn read_subscribe_response(
		&self,
		stream: &mut Stream<S, Version>,
	) -> Result<Option<(u64, Option<Timescale>)>, Error> {
		// Read type_id + size + body from the stream
		let type_id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let mut data = stream.reader.read_exact(size as usize).await?;

		match type_id {
			ietf::SubscribeOk::ID => {
				let msg = ietf::SubscribeOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "received subscribe ok");
				Ok(Some((msg.track_alias, msg.properties.timescale)))
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
		let request_id = match resolve_track_alias(aliases, group.track_alias).await {
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

		let (mut producer, track, timescale) = {
			let mut state = self.state.lock();
			let track = state.subscribes.get_mut(&request_id).ok_or(Error::NotFound)?;

			let group_info = group::Info {
				sequence: group.group_id,
			};
			// Stats (groups/frames/bytes) are counted in the model as the group is
			// written, through the tagged `track::Producer`.
			let producer = track.producer.create_group(group_info)?;
			(producer, track.producer.clone(), track.timescale)
		};

		let res = {
			let mut serve = std::pin::pin!(self.run_group(group, stream, producer.clone(), timescale));
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_closed(waiter) {
					return Poll::Ready(Err(err));
				}
				if let Poll::Ready(err) = producer.poll_closed(waiter) {
					return Poll::Ready(Err(err));
				}
				waiter.poll_future(serve.as_mut())
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

	async fn run_group(
		&mut self,
		group: ietf::GroupHeader,
		stream: &mut Reader<S::RecvStream, Version>,
		mut producer: group::Producer,
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		while let Some(id_delta) = stream.decode_maybe::<u64>().await? {
			if id_delta != 0 {
				tracing::warn!(id_delta = %id_delta, "object ID delta is not supported, dropping stream");
				return Err(Error::Unsupported);
			}

			// Per-object extension headers may carry the frame's presentation timestamp
			// (the Timestamp Object Property), in the units the track declared. A track
			// that declared no timescale opted out, so its objects are stamped on arrival
			// even if one carries a Timestamp we could not interpret.
			let timestamp = match (group.flags.has_extensions, timescale) {
				(true, Some(timescale)) => {
					let size: usize = stream.decode().await?;
					let mut ext = stream.read_exact(size).await?;
					ietf::decode_object_time(&mut ext, timescale, self.version)?
				}
				(true, None) => {
					let size: usize = stream.decode().await?;
					stream.read_exact(size).await?;
					None
				}
				(false, _) => None,
			};

			let size: u64 = stream.decode().await?;
			if size == 0 {
				let status: u64 = stream.decode().await?;
				if status == 0 {
					let timestamp = timestamp.unwrap_or_else(crate::Timestamp::now);
					let frame = producer.create_frame(frame::Info { size: 0, timestamp })?;
					frame.finish()?;
				} else if status == 3 && !group.flags.has_end {
					break;
				} else {
					return Err(Error::Unsupported);
				}
			} else {
				// `create_frame` is the allocation chokepoint and rejects an oversized
				// `size` before allocating, so no pre-check is needed.
				let timestamp = timestamp.unwrap_or_else(crate::Timestamp::now);
				let mut frame = producer.create_frame(frame::Info { size, timestamp })?;

				if let Err(err) = self.run_frame(stream, &mut frame).await {
					let _ = frame.abort(err.clone());
					return Err(err);
				}

				frame.finish()?;
			}
		}

		Ok(())
	}

	async fn run_frame(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		frame: &mut frame::Producer<'_>,
	) -> Result<(), Error> {
		while frame.remaining() > 0 {
			match stream.read_chunk(frame.remaining()).await? {
				Some(chunk) if !chunk.is_empty() => {
					frame.write(chunk)?;
				}
				_ => return Err(Error::WrongSize),
			}
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use futures::poll;

	use super::*;

	#[tokio::test(start_paused = true)]
	async fn track_alias_waits_for_control_message() {
		let aliases = TrackAliases::default();
		let pending = resolve_track_alias(aliases.consume(), 7);
		tokio::pin!(pending);

		assert!(poll!(&mut pending).is_pending());

		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();

		assert_eq!(pending.await.unwrap(), RequestId(11));
	}

	#[tokio::test(start_paused = true)]
	async fn unknown_track_alias_times_out() {
		let aliases = TrackAliases::default();
		assert!(matches!(
			resolve_track_alias(aliases.consume(), 7).await,
			Err(Error::NotFound)
		));
	}

	async fn settle() {
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// What an unsolicited advertisement means to a subscriber on `version` whose peer
	/// declared `solicit`.
	fn unsolicited_is_a_violation(solicit: Option<bool>, version: Version) -> bool {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			solicit,
			..Default::default()
		});
		let (tasks, _task_set) = crate::util::TaskSet::new();

		Subscriber::new(
			session,
			origin,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Origin::new(1).unwrap(),
			None,
			version,
			tasks,
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
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let scoped = origin
			.with_root("rootns")
			.and_then(|rooted| rooted.scope(&[crate::Path::new("cam")]))
			.expect("scope the origin");

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			session.clone(),
			scoped,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			Version::Draft16,
			tasks,
		);

		assert_eq!(
			subscriber.subscribe_prefixes(),
			vec![crate::Path::new("cam").to_owned()],
			"one SUBSCRIBE_NAMESPACE per permitted prefix, relative to the root",
		);

		let stream = Stream::open(&session, Version::Draft16).await.unwrap();
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

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
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
			session.clone(),
			scoped,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Origin::new(1).unwrap(),
			None,
			VERSION,
			tasks,
		);

		let prefix = subscriber.subscribe_prefixes().pop().expect("one prefix");
		let stream = Stream::open(&session, VERSION).await.unwrap();
		// Parks on the read after the scripted NAMESPACE is consumed.
		let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(stream, prefix));
		for _ in 0..100 {
			// The result is deliberately ignored: a regressed mount lands out of scope
			// and errors here, which the assertions below name far better than a poll
			// would.
			let _ = futures::poll!(run.as_mut());
			if consumer.get_broadcast("rootns/cam/x.hang").is_some() {
				break;
			}
			settle().await;
		}

		assert!(
			consumer.get_broadcast("rootns/cam/x.hang").is_some(),
			"the reply mounts under the root once",
		);
		assert!(
			consumer.get_broadcast("rootns/rootns/cam/x.hang").is_none(),
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

		let resolve = resolve_track_alias(aliases.consume(), 7);
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

		let err = resolve_track_alias(aliases.consume(), 7)
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
	) -> Subscriber<crate::lite::test_transport::SinkSession> {
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The tests drive binding directly, so nothing spawns; leaking keeps the handle alive
		// without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			crate::lite::test_transport::SinkSession::new(Default::default()),
			crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			Version::Draft19,
			tasks,
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
		for version in [Version::Draft16, Version::Draft19] {
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
			session,
			crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			VERSION,
			tasks,
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
			session,
			crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			VERSION,
			tasks,
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
		let control_stream = Stream::open(&session, VERSION).await.unwrap();
		let running = adapter.clone();
		tokio::spawn(async move {
			let _ = running.run(control_stream.reader, control_stream.writer).await;
		});

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			adapter,
			crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce(),
			control,
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			VERSION,
			tasks,
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
		for version in [Version::Draft16, Version::Draft19] {
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
			session,
			crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			version,
			tasks,
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
	/// `Client::with_peer_origin` pins it, so sessions dialing the same relay
	/// resolve to one recognizable route, and a reconnect splices rather than
	/// replacing.
	#[tokio::test]
	async fn assigned_peer_origin_attributes_announces() {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let assigned = crate::Origin::new(777).unwrap();

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			session,
			origin,
			Control::new(None, false),
			Some(assigned),
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			Version::Draft14,
			tasks,
		);

		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		let _producer = subscriber
			.start_announce(crate::Path::new("room/host").to_owned(), advert)
			.unwrap();

		// Broadcast visibility is deferred until the executor ticks.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let hops: Vec<_> = broadcast.routes()[0].hops.iter().copied().collect();
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
		let peer = crate::Origin::new(777).unwrap();
		let self_origin = crate::Origin::new(1).unwrap();

		let origin = crate::origin::Info::new(self_origin).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// The publish direction: an origin handle scoped to the peer, which is what
		// `Client::with_peer_origin` hands the publisher. Holding its announce stream
		// is what records that the peer has been offered these paths.
		let mut publishing = consumer.clone().excluding(peer).announced();

		// What we are publishing to the peer: a real upstream, announced.
		let upstream = crate::OriginList::try_from(vec![crate::Origin::new(7).unwrap()]).unwrap();
		let _source = origin
			.create_broadcast(
				"room/host",
				crate::broadcast::Route::new()
					.with_hops(upstream.clone())
					.with_announce(true),
			)
			.unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		announced.assert_next_some("room/host");
		// Held for as long as the path is advertised, exactly as the publisher holds
		// it in its `Watched` entry. The exclusion lives on this handle.
		let _advertised = publishing.assert_next_some("room/host");

		// The peer reflects it back over the subscribe direction, which carries no
		// hop chain of its own.
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			session,
			origin,
			Control::new(None, false),
			Some(peer),
			peer::PeerSetup::default(),
			self_origin,
			None,
			Version::Draft14,
			tasks,
		);
		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		let _reflected = subscriber
			.start_announce(crate::Path::new("room/host").to_owned(), advert)
			.unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		// No announce churn, and the path still resolves to the upstream route, which
		// is the one the publish direction can advertise back to the peer.
		announced.assert_next_wait();
		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let routes = broadcast.routes();
		assert_eq!(routes.len(), 1, "the reflection must park, not attach");
		assert_eq!(routes[0].hops, upstream);
	}

	/// The assigned identity is a content identity too, so a second session dialing
	/// the same relay produces the same first hop and splices into the front its
	/// predecessor is serving. That is what makes a reconnect immediate instead of
	/// waiting for the transport to retire the dead session, and the reflection guard
	/// must not cost us it.
	#[tokio::test]
	async fn reconnecting_peer_joins_the_front_it_replaces() {
		let peer = crate::Origin::new(777).unwrap();
		let self_origin = crate::Origin::new(1).unwrap();

		let origin = crate::origin::Info::new(self_origin).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let connect = || {
			let (tasks, task_set) = crate::util::TaskSet::new();
			std::mem::forget(task_set);
			let mut subscriber = Subscriber::new(
				crate::lite::test_transport::SinkSession::new(Default::default()),
				origin.clone(),
				Control::new(None, false),
				Some(peer),
				peer::PeerSetup::default(),
				self_origin,
				None,
				Version::Draft14,
				tasks,
			);
			let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
			let producer = subscriber
				.start_announce(crate::Path::new("room/host").to_owned(), advert)
				.unwrap();
			(subscriber, producer)
		};

		let _first = connect();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		announced.assert_next_some("room/host");

		// The peer reconnects before the old session is retired.
		let _second = connect();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		// It joined: no announce churn, and both routes are in the table so the front
		// can fail over the moment the stale one dies.
		announced.assert_next_wait();
		let routes = consumer.get_broadcast("room/host").unwrap().routes();
		assert_eq!(routes.len(), 2, "a reconnect must join, not park or replace");
	}

	fn cluster_subscriber(
		self_origin: crate::Origin,
	) -> (
		Subscriber<crate::lite::test_transport::SinkSession>,
		crate::origin::Producer,
	) {
		cluster_subscriber_with_linger(self_origin, std::time::Duration::ZERO)
	}

	fn cluster_subscriber_with_linger(
		self_origin: crate::Origin,
		linger: std::time::Duration,
	) -> (
		Subscriber<crate::lite::test_transport::SinkSession>,
		crate::origin::Producer,
	) {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let origin = crate::origin::Info::new(self_origin).with_linger(linger).produce();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The set only drains announce-serving tasks; the tests here drive the model
		// directly, so leaking it keeps the handles alive without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			session,
			origin.clone(),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			self_origin,
			None,
			Version::Draft19,
			tasks,
		);
		(subscriber, origin)
	}

	fn hop_path(ids: &[u64]) -> cluster::HopPath {
		let hops = ids
			.iter()
			.map(|&id| crate::Origin::new(id).unwrap())
			.collect::<Vec<_>>();
		cluster::HopPath::new(crate::OriginList::try_from(hops).unwrap())
	}

	/// A negotiated advertisement carries the whole path and its accumulated cost, and
	/// the receiving relay charges its own link on top (saturating, so an absurd
	/// upstream value ranks last rather than wrapping to best).
	#[tokio::test]
	async fn cluster_advert_becomes_a_route_with_the_link_charged() {
		let (subscriber, origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let consumer = origin.consume();

		let peer = cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
			cost: Some(3),
		};
		let advert = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		};

		let advertised = subscriber.route(Some(&advert), &peer).expect("route");
		assert_eq!(
			advertised.route.cost, 7,
			"the link's price is added to the advertised cost"
		);
		assert_eq!(advertised.route.hops, hop_path(&[7, 9]).hops().clone());
		assert!(advertised.route.announce);
		assert_eq!(advertised.publisher, Some(crate::Origin::new(7).unwrap()));

		let mut subscriber = subscriber;
		subscriber
			.start_announce(crate::Path::new("room/host").to_owned(), advertised)
			.unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let hops: Vec<_> = broadcast.routes()[0].hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 9]);
		assert_eq!(broadcast.routes()[0].cost, 7);
	}

	/// An advertisement whose path already contains our own Hop ID looped back:
	/// forwarding it would extend the loop and subscribing through it would route us
	/// back to ourselves. Hop ID 0 identifies nothing, so it is never a loop.
	#[test]
	fn cluster_advert_loop_is_discarded() {
		let (subscriber, _origin) = cluster_subscriber(crate::Origin::new(5).unwrap());
		let peer = cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
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
		let (subscriber, _origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let peer = cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
			cost: None,
		};
		let advert = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 2,
		};
		assert_eq!(subscriber.route(Some(&advert), &peer).unwrap().route.cost, 3);

		// Zero is meaningful and distinct from absent: a free link adds nothing.
		let free = cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
			cost: Some(0),
		};
		assert_eq!(subscriber.route(Some(&advert), &free).unwrap().route.cost, 2);
	}

	/// A namespace stream that ends with advertisements still live must detach them
	/// ABRUPTLY, leaving the origin's linger window open so a source reconnecting into
	/// it resumes the broadcast. Finishing instead closes it synchronously, which
	/// viewers see as an end and which promises that a later create at the path is new
	/// content rather than a resumption: the wrong promise for a transient blip.
	///
	/// True even of a clean FIN, since closing the stream retracts nothing: the protocol
	/// has NAMESPACE_DONE for that. moq-lite already behaves this way (its route map is
	/// a local whose guards drop), which `lite::subscriber` pins separately.
	///
	/// Driven through the real exit path rather than by calling `stop_announce`: a test
	/// that picked the detach itself would still pass if the stream stopped using it.
	/// A zero-linger origin cannot tell the two detaches apart, which is why this sets one.
	#[tokio::test(start_paused = true)]
	async fn a_lost_namespace_stream_leaves_the_linger_window_open() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap())
			.with_linger(std::time::Duration::from_secs(30))
			.produce();
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
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer_setup,
			crate::Origin::new(1).unwrap(),
			None,
			VERSION,
			tasks,
		);

		let stream = Stream::open(&session, VERSION).await.unwrap();
		subscriber
			.run_subscribe_namespace(stream, crate::Path::new("").to_owned())
			.await
			.expect("a clean FIN is not an error");
		settle().await;

		assert!(
			consumer.get_broadcast("x.hang").is_some(),
			"an ended stream closed the broadcast instead of lingering for a reconnect",
		);
	}

	/// The peer explicitly retracting a namespace still ends the broadcast NOW, linger
	/// or not: it said the namespace is gone, so holding the front open would stall a
	/// replacement and promise a resumption that is not coming.
	#[tokio::test(start_paused = true)]
	async fn an_explicit_namespace_done_closes_despite_the_linger_window() {
		let (mut subscriber, origin) =
			cluster_subscriber_with_linger(crate::Origin::new(1).unwrap(), std::time::Duration::from_secs(30));
		let consumer = origin.consume();

		let path = crate::Path::new("room/host").to_owned();
		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		subscriber.start_announce(path.clone(), advert).unwrap();
		settle().await;

		subscriber.stop_announce(path, Detach::Graceful).unwrap();
		settle().await;
		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"an explicit NAMESPACE_DONE lingered instead of closing",
		);
	}

	/// v14-16 withdraw a PUBLISH_NAMESPACE with PUBLISH_NAMESPACE_DONE, which the adapter
	/// delivers as a message before it FINs the virtual stream. Reading it as a stray
	/// message closes the whole session over a routine unannounce, and strands the
	/// broadcast for the linger window on the way out.
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

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap())
			.with_linger(std::time::Duration::from_secs(30))
			.produce();
		let consumer = origin.consume();
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let mut subscriber = Subscriber::new(
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			VERSION,
			tasks,
		);

		let stream = Stream::open(&session, VERSION).await.unwrap();
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
			consumer.get_broadcast("room/host").is_none(),
			"an explicit withdrawal lingered instead of closing",
		);
	}

	/// A PUBLISH_NAMESPACE stream that dies mid-advertisement detaches ABRUPTLY. Only a
	/// clean close retracts here, since that is how PUBLISH_NAMESPACE_DONE reaches us
	/// (the adapter turns it into one); a stream that ended any other way still held the
	/// advertisement, so the origin keeps the linger window open for the reconnect.
	///
	/// Driven through the real exit path rather than by calling `stop_announce`: a test
	/// that picked the detach itself would still pass if the stream stopped using it.
	#[tokio::test(start_paused = true)]
	async fn a_broken_publish_namespace_stream_leaves_the_linger_window_open() {
		const VERSION: Version = Version::Draft19;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap())
			.with_linger(std::time::Duration::from_secs(30))
			.produce();
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
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			VERSION,
			tasks,
		);

		let path = crate::Path::new("room/host").to_owned();
		let stream = Stream::open(&session, VERSION).await.unwrap();
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
			consumer.get_broadcast("room/host").is_some(),
			"a broken stream closed the broadcast instead of lingering for a reconnect",
		);
	}

	/// Several advertisements share one refcounted source, so the detach that empties it
	/// is the one that counts: an earlier owner lost abruptly does not outvote the last
	/// one's explicit retraction, and does not stall a replacement behind a linger.
	///
	/// That is the model's own rule for several sources at one path (`detach_source`),
	/// which is what these advertisements would be had they arrived on two sessions. The
	/// refcount is a detail of sharing one `SourceGuard` per session; it must not change
	/// what the origin sees.
	#[tokio::test(start_paused = true)]
	async fn the_last_owner_out_decides_the_detach() {
		let (mut subscriber, origin) =
			cluster_subscriber_with_linger(crate::Origin::new(1).unwrap(), std::time::Duration::from_secs(30));
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer::default();

		for _ in 0..2 {
			let advert = subscriber.route(None, &peer).expect("route");
			subscriber.start_announce(path.clone(), advert).unwrap();
		}
		settle().await;

		// One advertisement's stream dies, the other is retracted explicitly.
		subscriber.stop_announce(path.clone(), Detach::Abrupt).unwrap();
		subscriber.stop_announce(path.clone(), Detach::Graceful).unwrap();
		settle().await;
		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"an explicit retraction lingered because an earlier owner was lost abruptly",
		);

		// The reverse order still lingers: the path was never withdrawn on the way out.
		for _ in 0..2 {
			let advert = subscriber.route(None, &peer).expect("route");
			subscriber.start_announce(path.clone(), advert).unwrap();
		}
		settle().await;
		subscriber.stop_announce(path.clone(), Detach::Graceful).unwrap();
		subscriber.stop_announce(path, Detach::Abrupt).unwrap();
		settle().await;
		assert!(
			consumer.get_broadcast("room/host").is_some(),
			"an abrupt last detach closed the broadcast instead of lingering for a reconnect",
		);
	}

	/// An advertisement with no path of its own (a peer that did not negotiate the
	/// extension) still pays for the link it arrived over. Forwarding it as free would
	/// advertise a paid upstream as the cheapest route in the mesh.
	#[test]
	fn a_pathless_advert_still_pays_for_its_link() {
		let (unpriced, _origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let peer = cluster::Peer::default();

		// Nothing priced this direction, so it ranks by hop count.
		assert_eq!(unpriced.route(None, &peer).unwrap().route.cost, cluster::DEFAULT_COST);

		// A peer that declared its egress price is charged it, extension or not.
		let priced_peer = cluster::Peer {
			origin: None,
			cost: Some(4),
		};
		assert_eq!(unpriced.route(None, &priced_peer).unwrap().route.cost, 4);

		// Local policy still wins over what the peer declared.
		let (mut priced, _origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		priced.cost = Some(6);
		assert_eq!(priced.route(None, &priced_peer).unwrap().route.cost, 6);
	}

	/// An update replaces the advertisement in place: the route moves, the refcount does
	/// not, and the source is not torn down. Only a changed original publisher replaces
	/// it, since that content is not interchangeable.
	#[tokio::test]
	async fn cluster_update_replaces_in_place() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();

		let first = crate::broadcast::Route::new()
			.with_hops(hop_path(&[7, 9]).hops().clone())
			.with_cost(4)
			.with_announce(true);
		let first = Advertised {
			route: first,
			publisher: Some(crate::Origin::new(7).unwrap()),
		};
		subscriber.start_announce(path.clone(), first).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		let original = consumer.get_broadcast("room/host").unwrap();

		// Same publisher (hop 7), new path and cost: the route updates in place.
		let rerouted = crate::broadcast::Route::new()
			.with_hops(hop_path(&[7, 11]).hops().clone())
			.with_cost(2)
			.with_announce(true);
		let rerouted = Advertised {
			route: rerouted,
			publisher: Some(crate::Origin::new(7).unwrap()),
		};
		subscriber.update_announce(path.clone(), rerouted).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let hops: Vec<_> = broadcast.routes()[0].hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 11]);
		assert_eq!(broadcast.routes()[0].cost, 2);
		assert!(!original.is_closed(), "the source survived the update");

		// One advertisement, so one unannounce detaches it. If the update had bumped the
		// refcount, this would leave the source stranded.
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_none());
	}

	/// A different original publisher is not interchangeable content, so the source is
	/// replaced rather than spliced. The refcount rides along: the same advertisements
	/// still reference the path.
	#[tokio::test]
	async fn cluster_publisher_change_replaces_the_source() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();

		let first = crate::broadcast::Route::new()
			.with_hops(hop_path(&[7, 9]).hops().clone())
			.with_announce(true);
		let first = Advertised {
			route: first,
			publisher: Some(crate::Origin::new(7).unwrap()),
		};
		subscriber.start_announce(path.clone(), first).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		let original = consumer.get_broadcast("room/host").unwrap();

		let taken_over = crate::broadcast::Route::new()
			.with_hops(hop_path(&[8, 9]).hops().clone())
			.with_announce(true);
		let taken_over = Advertised {
			route: taken_over,
			publisher: Some(crate::Origin::new(8).unwrap()),
		};
		subscriber.update_announce(path.clone(), taken_over).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		assert!(original.is_closed(), "the old source was detached");
		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let hops: Vec<_> = broadcast.routes()[0].hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![8, 9]);

		// Still one advertisement, so one unannounce is all it takes.
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_none());
	}

	/// Regression: a publisher that declares no identity of its own contributes
	/// `Origin::UNKNOWN` as the first hop, which identifies nothing. A repeat NAMESPACE
	/// is still the same advertisement being repriced (the expected update, and how a
	/// relay signals that it started carrying the namespace), so the source and every
	/// live subscription on it must survive. Reading the repeat as a new publisher
	/// detached the source milliseconds after SUBSCRIBE went out.
	#[tokio::test(start_paused = true)]
	async fn anonymous_publisher_survives_a_repricing_update() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		// A free link, so the route cost is exactly what the peer advertised.
		let peer = cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
			cost: Some(0),
		};
		let hops = cluster::HopPath::new(
			crate::OriginList::try_from(vec![crate::Origin::UNKNOWN, crate::Origin::new(9).unwrap()]).unwrap(),
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
		assert_eq!(
			advertised.publisher,
			Some(crate::Origin::UNKNOWN),
			"an anonymous publisher has no identity to carry",
		);
		subscriber.start_announce(path.clone(), advertised).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		let original = consumer.get_broadcast("room/host").unwrap();

		// The peer re-advertises the same path cheaper: it started carrying it.
		let repriced = subscriber
			.route(Some(&cluster::Advert { hops, cost: 1 }), &peer)
			.expect("route");
		subscriber.update_announce(path.clone(), repriced).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		assert!(
			!original.is_closed(),
			"a repricing update must not tear down the source"
		);
		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let routes = broadcast.routes();
		assert_eq!(routes.len(), 1, "the update replaced the route, it did not add one");
		assert_eq!(routes[0].cost, 1);

		// One advertisement, so one unannounce detaches it.
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_none());
	}

	/// Two *separate* advertisements have nothing linking them but the publisher, and
	/// `Origin::UNKNOWN` links nothing: any number of unrelated publishers present it.
	/// So the later one replaces the earlier, unlike the repeat above.
	#[tokio::test(start_paused = true)]
	async fn separate_anonymous_adverts_replace_the_source() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
			cost: None,
		};
		let hops = cluster::HopPath::new(
			crate::OriginList::try_from(vec![crate::Origin::UNKNOWN, crate::Origin::new(9).unwrap()]).unwrap(),
		);
		let advert = cluster::Advert { hops, cost: 0 };

		let first = subscriber.route(Some(&advert), &peer).expect("route");
		subscriber.start_announce(path.clone(), first).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		let original = consumer.get_broadcast("room/host").unwrap();

		let second = subscriber.route(Some(&advert), &peer).expect("route");
		subscriber.start_announce(path.clone(), second).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		assert!(original.is_closed(), "the old source was detached");
		assert!(consumer.get_broadcast("room/host").is_some());

		// Two advertisements, so it takes two unannounces to detach.
		subscriber.stop_announce(path.clone(), Detach::Graceful).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_some());
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_none());
	}

	/// Regression: without the MoQ Cluster extension an advertisement carries no path,
	/// so there is no publisher identity to compare. PUBLISH_NAMESPACE and NAMESPACE for
	/// one namespace are then two messages about a single source, and treating the
	/// second as a different publisher would tear down what the first attached, right
	/// as a subscriber is resolving a track through it.
	#[tokio::test]
	async fn pathless_adverts_never_replace_the_source() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer::default();

		// What a PUBLISH_NAMESPACE with no cluster parameters resolves to.
		let first = subscriber.route(None, &peer).expect("route");
		assert_eq!(first.publisher, None, "no path means no identity to compare");
		subscriber.start_announce(path.clone(), first).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		let original = consumer.get_broadcast("room/host").unwrap();

		// The NAMESPACE for the same namespace arrives second.
		let second = subscriber.route(None, &peer).expect("route");
		subscriber.start_announce(path.clone(), second).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		assert!(!original.is_closed(), "the source survived the second advertisement");
		assert!(consumer.get_broadcast("room/host").is_some());

		// Two advertisements, so it takes two unannounces to detach.
		subscriber.stop_announce(path.clone(), Detach::Graceful).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_some());
		subscriber.stop_announce(path, Detach::Graceful).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_none());
	}

	/// An update replaces the advertisement it repeats. When the replacement loops back
	/// through us it is a retraction, so the route we were holding must go: keeping it
	/// would leave subscriptions on a path the peer no longer offers.
	#[tokio::test]
	async fn reflected_replacement_retracts_the_route() {
		let self_origin = crate::Origin::new(5).unwrap();
		let (mut subscriber, origin) = cluster_subscriber(self_origin);
		let consumer = origin.consume();
		let path = crate::Path::new("room/host").to_owned();
		let peer = cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
			cost: None,
		};

		let clean = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 0,
		};
		let advert = subscriber.route(Some(&clean), &peer).expect("route");
		subscriber.start_announce(path.clone(), advert).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_some());

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
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(
			consumer.get_broadcast("room/host").is_none(),
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
		self_origin: crate::Origin,
		request_id: RequestId,
		peer: &cluster::Peer,
		attached: &cluster::Advert,
		updates: &[Option<cluster::Advert>],
	) -> (
		Subscriber<crate::lite::test_transport::ScriptedSession>,
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
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			self_origin,
			None,
			VERSION,
			tasks,
		);

		let path = crate::Path::new("room/host").to_owned();
		let advert = subscriber.route(Some(attached), peer).expect("route");
		subscriber.start_announce(path, advert).unwrap();
		settle().await;
		assert!(consumer.get_broadcast("room/host").is_some(), "attached to start with");

		let stream = Stream::open(&session, VERSION).await.unwrap();
		(subscriber, consumer, stream)
	}

	fn peer_9() -> cluster::Peer {
		cluster::Peer {
			origin: Some(crate::Origin::new(9).unwrap()),
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
		let self_origin = crate::Origin::new(5).unwrap();
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
				if consumer.get_broadcast("room/host").is_none() {
					break;
				}
				settle().await;
			}
		}

		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"an unusable path must not stay attached"
		);
		assert!(!attached, "the caller must not release it a second time");
	}

	/// Having kept the stream, a later usable path re-attaches on it. This is the whole
	/// reason the stream stays open.
	#[tokio::test]
	async fn a_clean_update_after_a_reflection_reattaches() {
		let self_origin = crate::Origin::new(5).unwrap();
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
			consumer.get_broadcast("room/host").is_some(),
			"the namespace is routable again",
		);
	}

	/// The SUBSCRIBE_NAMESPACE stream owns every advertisement it carried. When it ends
	/// without a NAMESPACE_DONE for each, those refcounts must still be released, or the
	/// source stays attached for the rest of the session (the stream can die while the
	/// session keeps running).
	#[tokio::test]
	async fn namespace_stream_close_releases_live_paths() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Origin::new(1).unwrap());
		let consumer = origin.consume();
		let peer = cluster::Peer::default();

		let mut live = std::collections::HashSet::new();
		for path in ["room/a", "room/b"] {
			let path = crate::Path::new(path).to_owned();
			let advert = subscriber.route(None, &peer).expect("route");
			subscriber.start_announce(path.clone(), advert).unwrap();
			live.insert(path);
		}
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/a").is_some());
		assert!(consumer.get_broadcast("room/b").is_some());

		// What the stream's exit path does with whatever it still holds.
		for path in live {
			subscriber.stop_announce(path, Detach::Graceful).unwrap();
		}
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		assert!(consumer.get_broadcast("room/a").is_none(), "room/a leaked a refcount");
		assert!(consumer.get_broadcast("room/b").is_none(), "room/b leaked a refcount");
	}

	/// PUBLISH offers one track, but a source attaches per namespace and serves every
	/// track under it. Rather than invent a namespace-level source from a track-level
	/// offer, decline the request and leave the session running.
	#[tokio::test]
	async fn publish_is_rejected_without_announcing() {
		// An open gate, so the rejection actually reaches the wire.
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);

		let mut subscriber = Subscriber::new(
			session.clone(),
			origin,
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			crate::Origin::new(1).unwrap(),
			None,
			Version::Draft19,
			tasks,
		);

		let stream = Stream::open(&session, Version::Draft19).await.unwrap();
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
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		assert!(
			consumer.get_broadcast("room/host").is_none(),
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
