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

use super::{Message, Version, cluster};

use web_async::Lock;

const TRACK_ALIAS_TIMEOUT: Duration = Duration::from_secs(1);

type TrackAliases = kio::Producer<HashMap<u64, RequestId>>;

fn insert_track_alias(aliases: &TrackAliases, alias: u64, request_id: RequestId) -> Result<(), Error> {
	let mut aliases = aliases.write().map_err(|_| Error::Dropped)?;
	match aliases.entry(alias) {
		Entry::Occupied(entry) if *entry.get() == request_id => Ok(()),
		Entry::Occupied(_) => Err(Error::Duplicate),
		Entry::Vacant(entry) => {
			entry.insert(request_id);
			Ok(())
		}
	}
}

/// Whether an error means the peer broke the protocol, as opposed to a stream or
/// transport failing on its own.
///
/// Only the former justifies taking the whole session down.
fn is_protocol_violation(err: &Error) -> bool {
	matches!(
		err,
		Error::Decode(_)
			| Error::Encode(_)
			| Error::BoundsExceeded(_)
			| Error::WrongSize
			| Error::TooManyParameters
			| Error::ProtocolViolation
			| Error::UnexpectedMessage
			| Error::UnexpectedStream
	)
}

fn remove_track_alias(aliases: &TrackAliases, alias: u64, request_id: RequestId) {
	let Ok(mut aliases) = aliases.write() else {
		return;
	};
	if aliases.get(&alias) == Some(&request_id) {
		aliases.remove(&alias);
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

	// Units for this track's object Timestamps, from the TIMESCALE Track Property in
	// SUBSCRIBE_OK. `None` until it arrives, and for a track that declares none: the
	// publisher opted out of timestamps, so frames are stamped on arrival instead.
	timescale: Option<Timescale>,
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
	/// distinct publisher reusing the namespace and must not splice.
	///
	/// `None` when the advertisement carried no path, so there is no identity to
	/// compare and nothing ever replaces the source on identity grounds. That covers
	/// base moq-transport entirely: PUBLISH_NAMESPACE and NAMESPACE for one namespace
	/// are then two messages describing a single source, not two publishers.
	publisher: Option<crate::Origin>,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Producer,
	control: Control,
	// The origin stamped into the hop chain of every broadcast from this session,
	// when the peer declares none of its own. Base moq-transport carries no hop ids,
	// so a peer only has an identity if it negotiated the MoQ Cluster extension or
	// the caller assigned it one (`Client::with_peer_origin`), which also makes the
	// route recognizable across sessions dialing the same relay.
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
	peer_setup: cluster::PeerSetup,
	// Local policy for what pulling from this peer costs, overriding whatever it
	// declared. See `cluster::link_cost`.
	cost: Option<u64>,
	state: Lock<State>,
	tasks: Tasks,
	version: Version,
}

async fn resolve_track_alias(aliases: kio::Consumer<HashMap<u64, RequestId>>, alias: u64) -> Result<RequestId, Error> {
	let mut timeout = kio::time::Deadline::after(TRACK_ALIAS_TIMEOUT);
	kio::wait(|waiter| {
		let resolved = aliases.poll(waiter, |aliases| match aliases.get(&alias) {
			Some(request_id) => Poll::Ready(*request_id),
			None => Poll::Pending,
		});
		if let Poll::Ready(result) = resolved {
			return Poll::Ready(result.map_err(|_| Error::Dropped));
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
		peer_setup: cluster::PeerSetup,
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
			true => self.peer_setup.get().await,
			false => cluster::Peer::default(),
		}
	}

	/// The route for an advertisement that carries no path of its own.
	///
	/// Base moq-transport has no hops on the wire, so the chain is a single entry
	/// attributed to this session (`Origin::UNKNOWN` unless the peer or the caller
	/// supplied an identity).
	fn session_route(&self) -> broadcast::Route {
		let mut hops = crate::OriginList::new();
		hops.push(self.session_origin)
			.expect("an empty hop chain has room for one entry");
		broadcast::Route::new().with_hops(hops).with_announce(true)
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
				route: self.session_route(),
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

	fn register_alias(&self, request_id: RequestId, alias: u64) -> Result<(), Error> {
		let mut state = self.state.lock();
		if !state.subscribes.contains_key(&request_id) {
			return Err(Error::NotFound);
		}

		insert_track_alias(&state.aliases, alias, request_id)?;
		state.subscribes.get_mut(&request_id).unwrap().alias = Some(alias);
		Ok(())
	}

	fn remove_subscribe(&self, request_id: RequestId) -> Option<TrackState> {
		let mut state = self.state.lock();
		let track = state.subscribes.remove(&request_id)?;
		if let Some(alias) = track.alias {
			remove_track_alias(&state.aliases, alias, request_id);
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
	pub fn subscribe_prefixes(&self) -> Vec<PathOwned> {
		self.origin.allowed().map(|p| p.to_owned()).collect()
	}

	/// Send SUBSCRIBE_NAMESPACE for one prefix on a bidi stream.
	/// The caller is responsible for opening the appropriate stream type
	/// (virtual for v14/v15, real bidi for v16+), one per prefix.
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
		let res = self.run_namespace_entries(&mut stream, &prefix, &peer, &mut live).await;
		for path in live {
			let _ = self.stop_announce(path, false);
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
							let _ = self.stop_announce(path, false);
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
						let _ = self.stop_announce(path, true);
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
	/// `peer` is what the peer declared in its SETUP, which the dispatcher awaited once
	/// before accepting streams: PUBLISH_NAMESPACE cannot be parsed without knowing
	/// whether the MoQ Cluster extension is on.
	pub fn handle_stream(
		&mut self,
		id: u64,
		mut data: bytes::Bytes,
		stream: Stream<S, Version>,
		peer: cluster::Peer,
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
					if let Err(err) = this.run_publish_namespace_stream(stream, msg, peer).await {
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

	/// Handle an incoming PUBLISH_NAMESPACE on its bidi stream.
	async fn run_publish_namespace_stream(
		&mut self,
		mut stream: Stream<S, Version>,
		msg: ietf::PublishNamespace<'_>,
		peer: cluster::Peer,
	) -> Result<(), Error> {
		let request_id = msg.request_id;
		let path = msg.track_namespace.to_owned();

		// A path that already contains our own Hop ID looped back. Reject it rather
		// than attaching a source we would then have to route around.
		let Some(advert) = self.route(msg.cluster.as_ref(), &peer) else {
			tracing::debug!(%path, "dropping reflected publish_namespace");
			self.write_error(&mut stream, request_id, 400, "route loops through this relay")
				.await?;
			let _ = stream.writer.finish();
			let _ = stream.writer.closed().await;
			return Ok(());
		};

		match self.start_announce(path.clone(), advert) {
			Ok(_) => {
				if let Err(err) = self.write_ok(&mut stream, request_id).await {
					// Local rollback, not a peer unannounce: don't count announce bytes.
					let _ = self.stop_announce(path, false);
					return Err(err);
				}
			}
			Err(err) => {
				self.write_error(&mut stream, request_id, 400, &err.to_string()).await?;
				let _ = stream.writer.finish();
				let _ = stream.writer.closed().await;
				return Ok(());
			}
		}

		// An endpoint updates an advertisement by re-sending it on the stream that
		// already carries it, so keep reading until the stream closes (which is also how
		// v14-16's PublishNamespaceDone arrives, via the adapter).
		let res = self
			.run_publish_namespace_updates(&mut stream, &path, request_id, peer)
			.await;

		self.stop_announce(path, true)?;

		res
	}

	/// Read advertisement updates off a live PUBLISH_NAMESPACE stream until it closes.
	async fn run_publish_namespace_updates(
		&mut self,
		stream: &mut Stream<S, Version>,
		path: &PathOwned,
		request_id: RequestId,
		peer: cluster::Peer,
	) -> Result<(), Error> {
		loop {
			let type_id: u64 = match stream.reader.decode_maybe().await? {
				Some(id) => id,
				None => return Ok(()),
			};
			if type_id != ietf::PublishNamespace::ID {
				tracing::warn!(type_id, "unexpected message on publish_namespace stream");
				return Err(Error::UnexpectedMessage);
			}

			let size: u16 = stream.reader.decode().await?;
			let mut data = stream.reader.read_exact(size as usize).await?;
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

			// A route that now loops through us is the peer retracting it, so detach
			// rather than keep serving a path we cannot use.
			let Some(advert) = self.route(msg.cluster.as_ref(), &peer) else {
				tracing::debug!(%path, "publish_namespace now loops back; dropping");
				return Ok(());
			};

			tracing::debug!(%path, hops = advert.route.hops.len(), cost = advert.route.cost, "publish_namespace update");
			self.update_announce(path.clone(), advert)?;
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

		self.write_publish_error(&mut stream, msg.request_id, 400, "PUBLISH is not supported")
			.await?;
		// Finish rather than awaiting the peer's close: the rejection is the whole
		// exchange, and dropping `stream` on return tears down the rest.
		let _ = stream.writer.finish();

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
		let producer = self.attach(&mut state, path.clone(), advert)?;
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
		self.attach(&mut state, path, advert)?;
		Ok(())
	}

	/// Create or update the source for one path, leaving the refcount to the caller.
	///
	/// The ingress announce stats (announced / announced_bytes) are driven in the model
	/// by `create_broadcast`'s route transitions below.
	fn attach(&self, state: &mut State, path: PathOwned, advert: Advertised) -> Result<broadcast::Producer, Error> {
		let publisher = advert.publisher;

		// A different original publisher, or one that identifies nothing, means a
		// distinct broadcast may have taken over this namespace. Its content is not
		// interchangeable, so detach the old source and attach fresh instead of splicing
		// the route in place; cached tracks and subscriptions must not carry over. The
		// refcount rides along: the same advertisements still reference this path.
		//
		// Both sides must be known for the comparison to mean anything. A session
		// without the MoQ Cluster extension has no identity on either, so nothing ever
		// replaces there: its PUBLISH_NAMESPACE and NAMESPACE for one namespace are two
		// messages about a single source, and replacing on the second would tear down
		// what the first attached.
		let mut carried = None;
		if let Entry::Occupied(entry) = state.broadcasts.entry(path.clone())
			&& let (Some(old), Some(new)) = (entry.get().publisher, publisher)
			&& (old != new || new == crate::Origin::UNKNOWN)
		{
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

	/// `_count_bytes` is retained for call-site clarity (a real unannounce vs a local
	/// rollback), but the ingress announce stats are now driven in the model by the
	/// source's route transitions and last-route detach, not here.
	fn stop_announce(&mut self, path: PathOwned, _count_bytes: bool) -> Result<(), Error> {
		let mut state = self.state.lock();

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count -= 1;
				if entry.get().count == 0 {
					tracing::debug!(broadcast = %self.origin.absolute(&path), "unannounced");
					// A deliberate unannounce, so finish() rather than drop.
					entry.remove().producer.finish();
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
		let info = track::Info::default().with_timescale(crate::Timescale::MICRO);
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

		// Read the response and register the alias mapping
		match self.read_subscribe_response(&mut stream).await {
			Ok(Some((alias, timescale))) => {
				if let Some(timescale) = timescale {
					let mut state = self.state.lock();
					if let Some(track) = state.subscribes.get_mut(&request_id) {
						track.timescale = Some(timescale);
					}
				}

				if let Err(err) = self.register_alias(request_id, alias) {
					self.session.close(err.to_code(), err.to_string().as_ref());
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

		match end {
			End::Unused => {
				tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track.name(), "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
			}
			End::BroadcastClosed(err) => {
				tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track.name(), "broadcast closed");
				let _ = track.abort(err);
			}
			End::StreamClosed(res) => match res {
				Ok(()) => {
					tracing::info!(broadcast = %self.origin.absolute(&broadcast_path), track = %track.name(), "subscribe complete");
					let _ = track.finish();
				}
				Err(err) => {
					tracing::debug!(%err, "subscribe stream closed with error");
					let _ = track.abort(err);
				}
			},
		}

		// Clean up
		self.remove_subscribe(request_id);

		stream.writer.finish().ok();
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
				subscriber_priority: track.subscription().map(|s| s.priority).unwrap_or(0),
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
				Ok(Some((msg.track_alias, msg.timescale)))
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
		let request_id = resolve_track_alias(aliases, group.track_alias).await.inspect_err(|_| {
			tracing::warn!(track_alias = %group.track_alias, "unknown track alias");
		})?;

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
			cluster::PeerSetup::default(),
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
		let peer_setup = cluster::PeerSetup::default();
		peer_setup.set(cluster::Peer::default());
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
	fn removing_old_track_does_not_remove_reused_alias() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		remove_track_alias(&aliases, 7, RequestId(13));

		assert_eq!(aliases.read().get(&7), Some(&RequestId(11)));
	}

	/// moq-transport carries no hop ids, so a peer's broadcasts are normally
	/// attributed to a random per-connection origin. An identity assigned via
	/// `Client::with_peer_origin` pins it, so sessions dialing the same relay
	/// resolve to one recognizable route.
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
			cluster::PeerSetup::default(),
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

	fn cluster_subscriber(
		self_origin: crate::Origin,
	) -> (
		Subscriber<crate::lite::test_transport::SinkSession>,
		crate::origin::Producer,
	) {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let origin = crate::origin::Info::new(self_origin).produce();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The set only drains announce-serving tasks; the tests here drive the model
		// directly, so leaking it keeps the handles alive without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			session,
			origin.clone(),
			Control::new(None, false),
			None,
			cluster::PeerSetup::default(),
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
		subscriber.stop_announce(path, true).unwrap();
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
		subscriber.stop_announce(path, true).unwrap();
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
		subscriber.stop_announce(path.clone(), true).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_some());
		subscriber.stop_announce(path, true).unwrap();
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
		subscriber.stop_announce(path, false).unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"the superseded route must not stay attached"
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
			subscriber.stop_announce(path, false).unwrap();
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
			cluster::PeerSetup::default(),
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
			group_order: ietf::GroupOrder::Ascending,
			largest_location: None,
			forward: true,
			timescale: None,
		};

		// Errors are surfaced to the peer on the stream, not raised as a session error.
		subscriber.run_publish_stream(stream, msg).await.unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"a rejected PUBLISH must not announce a broadcast"
		);
		assert_eq!(
			occurrences(&session.log, b"PUBLISH is not supported"),
			1,
			"the decline reaches the peer"
		);
	}
}
