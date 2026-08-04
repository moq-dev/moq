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

use super::{Message, Version};

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

	// Each broadcast created by either a PUBLISH or PUBLISH_NAMESPACE message.
	broadcasts: HashMap<PathOwned, BroadcastState>,

	// Each PUBLISH message that is implicitly causing a PUBLISH_NAMESPACE message.
	publishes: HashMap<RequestId, PathOwned>,
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

	// active number of PUBLISH or PUBLISH_NAMESPACE messages.
	count: usize,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Producer,
	control: Control,
	// The origin stamped into the hop chain of every broadcast from this session.
	// moq-transport carries no hop ids on the wire, so a peer only has an identity
	// if the caller assigned it one (`Client::with_peer_origin`), which also makes
	// the route recognizable across sessions dialing the same relay.
	//
	// Otherwise this is `Origin::UNKNOWN` (0), the reserved "no identity" value.
	// Inventing a random id here would look like a real identity without being
	// one: the peer never learns it, so it cannot exclude it for loop detection,
	// and two unrelated publishers would each get an id that made their content
	// look distinct rather than merely unknown.
	session_origin: crate::Origin,
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
	pub fn new(
		session: S,
		origin: origin::Producer,
		control: Control,
		peer_origin: Option<crate::Origin>,
		version: Version,
		tasks: Tasks,
	) -> Self {
		Self {
			session,
			origin,
			control,
			session_origin: peer_origin.unwrap_or(crate::Origin::UNKNOWN),
			state: Default::default(),
			tasks,
			version,
		}
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

		// Loop reading Namespace/NamespaceDone entries
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
					let msg = ietf::Namespace::decode_msg(&mut data, self.version)?;
					let path = prefix.join(&msg.suffix);
					tracing::debug!(%path, "namespace");
					self.start_announce(path)?;
				}
				ietf::NamespaceDone::ID => {
					let msg = ietf::NamespaceDone::decode_msg(&mut data, self.version)?;
					let path = prefix.join(&msg.suffix);
					tracing::debug!(%path, "namespace_done");
					let _ = self.stop_announce(path, true);
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
	pub fn handle_stream(
		&mut self,
		id: u64,
		mut data: bytes::Bytes,
		stream: Stream<S, Version>,
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
				let msg = ietf::PublishNamespace::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received publish_namespace");
				async move {
					if let Err(err) = this.run_publish_namespace_stream(stream, msg).await {
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
	) -> Result<(), Error> {
		let request_id = msg.request_id;
		let path = msg.track_namespace.to_owned();

		match self.start_announce(path.clone()) {
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

		// Wait for stream close (PublishNamespaceDone in v14-16 comes as stream close via adapter,
		// in v17 the stream simply closes).
		let _ = stream.reader.closed().await;

		self.stop_announce(path, true)?;

		Ok(())
	}

	/// Handle an incoming PUBLISH on its bidi stream.
	async fn run_publish_stream(
		&mut self,
		mut stream: Stream<S, Version>,
		msg: ietf::Publish<'_>,
	) -> Result<(), Error> {
		let request_id = msg.request_id;

		if let Err(err) = self.start_publish(&msg) {
			if matches!(err, Error::Duplicate) {
				self.session.close(err.to_code(), err.to_string().as_ref());
				return Err(err);
			}
			self.write_publish_error(&mut stream, request_id, 400, &err.to_string())
				.await?;
			return Ok(());
		}

		let res = self.write_publish_ok(&mut stream, &msg).await;

		if res.is_ok() {
			// PUBLISH is the peer feeding us a broadcast, so count this session as
			// an active upstream feed for the lifetime of the publish. The guard
			// drops (releasing `broadcasts_closed`) when the stream closes below.
			// Wait for PublishDone or stream close
			let _ = stream.reader.closed().await;
		}

		// Clean up (always runs after start_publish succeeds)
		let mut state = self.state.lock();
		if let Some(mut track) = state.subscribes.remove(&request_id) {
			let _ = track.producer.finish();
			if let Some(alias) = track.alias {
				remove_track_alias(&state.aliases, alias, request_id);
			}
		}
		if let Some(path) = state.publishes.remove(&request_id) {
			drop(state);
			// Count the unannounce only when the publish was OK'd and its stream then
			// closed (a real end); a failed write_publish_ok is a local rollback.
			let _ = self.stop_announce(path, res.is_ok());
		}

		res
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

	async fn write_publish_ok(&self, stream: &mut Stream<S, Version>, msg: &ietf::Publish<'_>) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				stream.writer.encode(&ietf::PublishOk::ID).await?;
				stream
					.writer
					.encode(&ietf::PublishOk {
						request_id: Some(msg.request_id),
						forward: true,
						subscriber_priority: 0,
						group_order: GroupOrder::Descending,
						filter_type: FilterType::LargestObject,
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				stream.writer.encode(&ietf::RequestOk::ID).await?;
				stream
					.writer
					.encode(&ietf::RequestOk {
						request_id: Some(msg.request_id),
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

	fn start_announce(&mut self, path: PathOwned) -> Result<broadcast::Producer, Error> {
		// The ingress announce stats (announced / announced_bytes) are driven in the
		// model by `create_broadcast`'s route transitions below.
		let mut state = self.state.lock();
		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count += 1;
				Ok(entry.get().producer.producer())
			}
			Entry::Vacant(entry) => {
				// Stamp this connection's origin as the sole hop so the route is
				// attributable to the upstream session (moq-transport carries no
				// hops on the wire, so the chain is otherwise empty).
				let mut hops = crate::OriginList::new();
				hops.push(self.session_origin)
					.expect("an empty hop chain has room for one entry");
				let route = broadcast::Route::new().with_hops(hops).with_announce(true);

				// Propagates Error::Unauthorized if the path is out of scope.
				let broadcast = self.origin.create_broadcast(&path, route)?;

				// Register the dynamic handler synchronously: the broadcast only
				// becomes visible to consumers after this function returns to the
				// executor, so the origin's first track dispatch finds a handler
				// (mirrors the note in lite::Subscriber).
				let dynamic = broadcast.dynamic();

				entry.insert(BroadcastState {
					producer: crate::model::broadcast::SourceGuard::new(broadcast.clone()),
					count: 1,
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

	fn start_publish(&mut self, msg: &ietf::Publish<'_>) -> Result<(), Error> {
		let request_id = msg.request_id;
		let namespace = msg.track_namespace.to_owned();

		// Announce the broadcast first so the track is born from it (inheriting the
		// broadcast's Arc<broadcast::Info>). Undo the announce on any error path below.
		let mut broadcast = self.start_announce(namespace.clone())?;
		let track = match broadcast.create_track(msg.track_name.to_string(), None) {
			Ok(track) => track,
			Err(err) => {
				let _ = self.stop_announce(namespace, false);
				return Err(err);
			}
		};

		let mut state = self.state.lock();
		match state.subscribes.entry(request_id) {
			Entry::Vacant(entry) => {
				entry.insert(TrackState {
					producer: track.clone(),
					alias: Some(msg.track_alias),
					timescale: msg.timescale,
				});
			}
			Entry::Occupied(_) => {
				drop(state);
				let _ = self.stop_announce(namespace, false);
				return Err(Error::Duplicate);
			}
		};

		if let Err(err) = insert_track_alias(&state.aliases, msg.track_alias, request_id) {
			state.subscribes.remove(&request_id);
			drop(state);
			let _ = self.stop_announce(namespace, false);
			return Err(err);
		}
		state.publishes.insert(request_id, namespace);
		drop(state);

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
		let mut subscriber = Subscriber::new(session.clone(), scoped, Control::new(None, false), None, VERSION, tasks);

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
			Version::Draft14,
			tasks,
		);

		let _producer = subscriber
			.start_announce(crate::Path::new("room/host").to_owned())
			.unwrap();

		// Broadcast visibility is deferred until the executor ticks.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let broadcast = consumer.get_broadcast("room/host").unwrap();
		let hops: Vec<_> = broadcast.routes()[0].hops.iter().copied().collect();
		assert_eq!(hops, vec![assigned]);
	}
}
