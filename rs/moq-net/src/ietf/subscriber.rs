use std::collections::{HashMap, hash_map::Entry};

use std::sync::Arc;

use crate::{
	Broadcast, BroadcastDynamic, Error, Frame, FrameProducer, Group, GroupProducer, MAX_FRAME_SIZE, OriginProducer,
	Path, PathOwned, StatsHandle, SubscriberStats, SubscriberTrack, Track, TrackProducer,
	coding::{Reader, Stream},
	ietf::{self, Control, FilterType, GroupOrder, RequestId},
	model::BroadcastProducer,
};

use super::{Message, Version};

use web_async::Lock;

#[derive(Default)]
struct State {
	// Each active subscription
	subscribes: HashMap<RequestId, TrackState>,

	// A map of track aliases to request IDs.
	aliases: HashMap<u64, RequestId>,

	// Each broadcast created by either a PUBLISH or PUBLISH_NAMESPACE message.
	broadcasts: HashMap<PathOwned, BroadcastState>,

	// Each PUBLISH message that is implicitly causing a PUBLISH_NAMESPACE message.
	publishes: HashMap<RequestId, PathOwned>,
}

struct TrackState {
	producer: TrackProducer,
	alias: Option<u64>,
	/// Subscriber-side track stats; counters bump as frames/bytes/groups arrive.
	/// Dropping on subscription end records `subscriptions_closed`.
	stats: Arc<SubscriberTrack>,
}

struct BroadcastState {
	producer: BroadcastProducer,

	// active number of PUBLISH or PUBLISH_NAMESPACE messages.
	count: usize,

	/// Subscriber-side announce guard (bumps `announced` / `announced_closed`),
	/// held for as long as the broadcast is announced into our origin.
	_stats: SubscriberStats,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,
	origin: Option<OriginProducer>,
	control: Control,
	stats: StatsHandle,
	/// Per-session ingress broadcast-subscription tracker. Each upstream
	/// subscription holds a guard so `broadcasts - broadcasts_closed` counts the
	/// distinct upstream sessions feeding each broadcast.
	broadcasts: crate::SessionBroadcasts,
	// A random per-connection origin stamped into the hop chain of every
	// broadcast. moq-transport never carries hop ids on the wire, so each
	// upstream session needs a stable, unique identity in the hop list for two
	// sessions publishing the same path to resolve as distinct routes instead
	// of colliding on an empty chain.
	session_origin: crate::Origin,
	state: Lock<State>,
	version: Version,
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(
		session: S,
		origin: Option<OriginProducer>,
		control: Control,
		stats: StatsHandle,
		version: Version,
	) -> Self {
		let broadcasts = stats.subscriber_broadcasts();
		Self {
			session,
			origin,
			control,
			stats,
			broadcasts,
			session_origin: crate::Origin::random(),
			state: Default::default(),
			version,
		}
	}

	pub fn has_origin(&self) -> bool {
		self.origin.is_some()
	}

	/// Send SUBSCRIBE_NAMESPACE on a bidi stream.
	/// The caller is responsible for opening the appropriate stream type
	/// (virtual for v14/v15, real bidi for v16+).
	pub async fn run_subscribe_namespace<T: web_transport_trait::Session>(
		&mut self,
		mut stream: Stream<T, Version>,
	) -> Result<(), Error> {
		let prefix = self.origin.as_ref().ok_or(Error::InvalidRole)?.root().to_owned();
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
	pub fn handle_stream(&mut self, id: u64, mut data: bytes::Bytes, stream: Stream<S, Version>) -> Result<(), Error> {
		let mut this = self.clone();
		match id {
			ietf::Publish::ID => {
				let msg = ietf::Publish::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received publish");
				web_async::spawn(async move {
					if let Err(err) = this.run_publish_stream(stream, msg).await {
						tracing::debug!(%err, "publish stream error");
					}
				});
			}
			ietf::PublishNamespace::ID => {
				let msg = ietf::PublishNamespace::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received publish_namespace");
				web_async::spawn(async move {
					if let Err(err) = this.run_publish_namespace_stream(stream, msg).await {
						tracing::debug!(%err, "publish_namespace stream error");
					}
				});
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type for subscriber");
				return Err(Error::UnexpectedStream);
			}
		}
		Ok(())
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
			self.write_publish_error(&mut stream, request_id, 400, &err.to_string())
				.await?;
			return Ok(());
		}

		let res = self.write_publish_ok(&mut stream, &msg).await;

		if res.is_ok() {
			// PUBLISH is the peer feeding us a broadcast, so count this session as
			// an active upstream feed for the lifetime of the publish. The guard
			// drops (releasing `broadcasts_closed`) when the stream closes below.
			let abs = match &self.origin {
				Some(origin) => origin.absolute(&msg.track_namespace).to_owned(),
				None => msg.track_namespace.to_owned(),
			};
			let _broadcast_sub = self.broadcasts.subscribe(&abs);

			// Wait for PublishDone or stream close
			let _ = stream.reader.closed().await;
		}

		// Clean up (always runs after start_publish succeeds)
		let mut state = self.state.lock();
		if let Some(mut track) = state.subscribes.remove(&request_id) {
			let _ = track.producer.finish();
			if let Some(alias) = track.alias {
				state.aliases.remove(&alias);
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

	fn start_announce(&mut self, path: PathOwned) -> Result<BroadcastProducer, Error> {
		let Some(origin) = &self.origin else {
			return Err(Error::InvalidRole);
		};

		let abs = origin.absolute(&path).to_owned();
		// Count the broadcast name length per announce (not the encoded message
		// size, so framing overhead isn't charged), keyed by path so it's
		// independent of the lifetime guard below.
		self.stats
			.broadcast(&abs)
			.subscriber_announced_bytes(abs.as_str().len() as u64);

		let mut state = self.state.lock();
		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count += 1;
				Ok(entry.get().producer.clone())
			}
			Entry::Vacant(entry) => {
				// Stamp this connection's origin as the sole hop so the route is
				// attributable to the upstream session (moq-transport carries no
				// hops on the wire, so the chain is otherwise empty).
				let mut hops = crate::OriginList::new();
				hops.push(self.session_origin)
					.expect("an empty hop chain has room for one entry");
				let broadcast = Broadcast { hops }.produce();

				// Create the dynamic handler BEFORE publishing so consumers see
				// dynamic >= 1 the moment they receive the announce. Otherwise a
				// consumer can call subscribe_track() before the spawned
				// run_broadcast bumps the counter and get NotFound (mirrors the
				// note in lite::Subscriber).
				let dynamic = broadcast.dynamic();

				origin.publish_broadcast(path.clone(), broadcast.consume());
				entry.insert(BroadcastState {
					producer: broadcast.clone(),
					count: 1,
					_stats: self.stats.broadcast(&abs).subscriber(),
				});

				tracing::debug!(broadcast = %origin.absolute(&path), "announce");

				let this = self.clone();
				web_async::spawn(async move {
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

	/// `count_bytes` records the unannounce name length (mirroring the announce in
	/// [`Self::start_announce`]). Pass `true` for a real unannounce / stream-close
	/// control event and `false` for a local rollback (e.g. a failed OK write),
	/// which is a teardown rather than an announce the peer ended.
	fn stop_announce(&mut self, path: PathOwned, count_bytes: bool) -> Result<(), Error> {
		let Some(origin) = &self.origin else {
			return Err(Error::InvalidRole);
		};

		if count_bytes {
			let abs = origin.absolute(&path).to_owned();
			self.stats
				.broadcast(&abs)
				.subscriber_announced_bytes(abs.as_str().len() as u64);
		}

		let mut state = self.state.lock();

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count -= 1;
				if entry.get().count == 0 {
					tracing::debug!(broadcast = %origin.absolute(&path), "unannounced");
					entry.remove();
				}
			}
			Entry::Vacant(_) => return Err(Error::NotFound),
		};

		Ok(())
	}

	fn start_publish(&mut self, msg: &ietf::Publish<'_>) -> Result<(), Error> {
		let request_id = msg.request_id;

		let track = Track {
			name: msg.track_name.to_string(),
			priority: 0,
		}
		.produce();

		let abs = match &self.origin {
			Some(origin) => origin.absolute(&msg.track_namespace).to_owned(),
			None => msg.track_namespace.to_owned(),
		};
		let track_stats = Arc::new(self.stats.broadcast(&abs).subscriber_track(&msg.track_name));

		let mut state = self.state.lock();
		match state.subscribes.entry(request_id) {
			Entry::Vacant(entry) => {
				entry.insert(TrackState {
					producer: track.clone(),
					alias: Some(msg.track_alias),
					stats: track_stats,
				});
			}
			Entry::Occupied(_) => return Err(Error::Duplicate),
		};

		match state.aliases.entry(msg.track_alias) {
			Entry::Vacant(entry) => {
				entry.insert(request_id);
			}
			Entry::Occupied(_) => {
				state.subscribes.remove(&request_id);
				return Err(Error::Duplicate);
			}
		}
		state.publishes.insert(request_id, msg.track_namespace.to_owned());
		drop(state);

		let mut broadcast = self.start_announce(msg.track_namespace.to_owned())?;
		broadcast.insert_track(track.consume())?;

		Ok(())
	}

	async fn run_broadcast(&self, path: Path<'_>, mut broadcast: BroadcastDynamic) -> Result<(), Error> {
		loop {
			let track = tokio::select! {
				producer = broadcast.requested_track() => match producer {
					Ok(producer) => producer,
					Err(err) => {
						tracing::debug!(%err, "broadcast closed");
						break;
					}
				},
				_ = self.session.closed() => break,
			};

			let mut this = self.clone();

			let path = path.to_owned();
			let broadcast = broadcast.clone();
			web_async::spawn(async move {
				this.run_subscribe(path, broadcast, track).await;
			});
		}

		Ok(())
	}

	async fn run_subscribe(&mut self, broadcast_path: Path<'_>, broadcast: BroadcastDynamic, mut track: TrackProducer) {
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

		let abs = self
			.origin
			.as_ref()
			.expect("origin set by start_announce")
			.absolute(&broadcast_path)
			.to_owned();
		let track_stats = Arc::new(self.stats.broadcast(&abs).subscriber_track(&track.name));

		// Pre-register the track so group data arriving before SubscribeOk can be routed.
		// The publisher uses request_id.0 as track_alias, and recv_group falls back to
		// RequestId(track_alias) when no alias mapping exists, so this works.
		{
			let mut state = self.state.lock();
			state.subscribes.insert(
				request_id,
				TrackState {
					producer: track.clone(),
					alias: None,
					stats: track_stats,
				},
			);
		}

		// Write Subscribe message
		if let Err(err) = self
			.write_subscribe(&mut stream, request_id, &broadcast_path, &track)
			.await
		{
			tracing::debug!(%err, "failed to write subscribe");
			self.state.lock().subscribes.remove(&request_id);
			let _ = track.abort(err);
			return;
		}

		tracing::info!(broadcast = %self.origin.as_ref().expect("origin set by start_announce").absolute(&broadcast_path), track = %track.name, "subscribe started");

		// Read the response and register the alias mapping
		let track_alias = match self.read_subscribe_response(&mut stream).await {
			Ok(alias) => {
				if let Some(alias) = alias {
					let mut state = self.state.lock();
					state.aliases.insert(alias, request_id);
					if let Some(track_state) = state.subscribes.get_mut(&request_id) {
						track_state.alias = Some(alias);
					}
				}
				alias
			}
			Err(err) => {
				tracing::debug!(%err, "subscribe response error");
				self.state.lock().subscribes.remove(&request_id);
				let _ = track.abort(err);
				return;
			}
		};

		// Upstream confirmed (SubscribeOk), so this session is now actively feeding
		// the broadcast: take the `broadcasts` sentinel for the subscription's
		// lifetime. It drops (releasing `broadcasts_closed`) when this fn returns.
		let _broadcast_sub = self.broadcasts.subscribe(&abs);

		tokio::select! {
			_ = track.unused() => {
				tracing::info!(broadcast = %self.origin.as_ref().expect("origin set by start_announce").absolute(&broadcast_path), track = %track.name, "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
			}
			err = broadcast.closed() => {
				tracing::info!(broadcast = %self.origin.as_ref().expect("origin set by start_announce").absolute(&broadcast_path), track = %track.name, "broadcast closed");
				let _ = track.abort(err);
			}
			res = stream.reader.closed() => {
				match res {
					Ok(()) => {
						tracing::info!(broadcast = %self.origin.as_ref().expect("origin set by start_announce").absolute(&broadcast_path), track = %track.name, "subscribe complete");
						let _ = track.finish();
					}
					Err(err) => {
						tracing::debug!(%err, "subscribe stream closed with error");
						let _ = track.abort(err);
					}
				}
			}
		}

		// Clean up
		self.state.lock().subscribes.remove(&request_id);
		if let Some(alias) = track_alias {
			self.state.lock().aliases.remove(&alias);
		}

		stream.writer.finish().ok();
	}

	async fn write_subscribe(
		&self,
		stream: &mut Stream<S, Version>,
		request_id: RequestId,
		broadcast: &Path<'_>,
		track: &TrackProducer,
	) -> Result<(), Error> {
		stream.writer.encode(&ietf::Subscribe::ID).await?;
		stream
			.writer
			.encode(&ietf::Subscribe {
				request_id,
				track_namespace: broadcast.to_owned(),
				track_name: (&track.name).into(),
				subscriber_priority: track.priority,
				group_order: GroupOrder::Descending,
				filter_type: FilterType::LargestObject,
			})
			.await?;
		Ok(())
	}

	async fn read_subscribe_response(&self, stream: &mut Stream<S, Version>) -> Result<Option<u64>, Error> {
		// Read type_id + size + body from the stream
		let type_id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let mut data = stream.reader.read_exact(size as usize).await?;

		match type_id {
			ietf::SubscribeOk::ID => {
				let msg = ietf::SubscribeOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "received subscribe ok");
				Ok(Some(msg.track_alias))
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

		let (mut producer, track, track_stats) = {
			let mut state = self.state.lock();
			let request_id = match state.aliases.get(&group.track_alias) {
				Some(request_id) => *request_id,
				None => {
					tracing::warn!(track_alias = %group.track_alias, "unknown track alias, using request ID");
					RequestId(group.track_alias)
				}
			};
			let track = state.subscribes.get_mut(&request_id).ok_or(Error::NotFound)?;

			let group_info = Group {
				sequence: group.group_id,
			};
			let producer = track.producer.create_group(group_info)?;
			(producer, track.producer.clone(), track.stats.clone())
		};

		// Bump groups counter for this incoming group on the subscriber side.
		track_stats.group();

		let res = tokio::select! {
			err = track.closed() => Err(err),
			err = producer.closed() => Err(err),
			res = self.run_group(group, stream, producer.clone(), track_stats.clone()) => res,
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
		mut producer: GroupProducer,
		track_stats: Arc<SubscriberTrack>,
	) -> Result<(), Error> {
		while let Some(id_delta) = stream.decode_maybe::<u64>().await? {
			if id_delta != 0 {
				tracing::warn!(id_delta = %id_delta, "object ID delta is not supported, dropping stream");
				return Err(Error::Unsupported);
			}

			if group.flags.has_extensions {
				let size: usize = stream.decode().await?;
				stream.skip(size).await?;
			}

			let size: u64 = stream.decode().await?;
			if size == 0 {
				let status: u64 = stream.decode().await?;
				if status == 0 {
					let mut frame = producer.create_frame(Frame { size: 0 })?;
					track_stats.frame();
					frame.finish()?;
				} else if status == 3 && !group.flags.has_end {
					break;
				} else {
					return Err(Error::Unsupported);
				}
			} else {
				if size > MAX_FRAME_SIZE {
					return Err(Error::FrameTooLarge);
				}
				let mut frame = producer.create_frame(Frame { size })?;
				track_stats.frame();

				if let Err(err) = self.run_frame(stream, frame.clone(), &track_stats).await {
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
		mut frame: FrameProducer,
		track_stats: &SubscriberTrack,
	) -> Result<(), Error> {
		// FrameProducer impls BufMut; read_buf writes stream bytes directly into
		// the per-frame buffer (see lite/subscriber.rs run_frame for rationale).
		while bytes::BufMut::has_remaining_mut(&frame) {
			match stream.read_buf(&mut frame).await? {
				Some(n) if n > 0 => {
					track_stats.bytes(n as u64);
				}
				_ => return Err(Error::WrongSize),
			}
		}
		Ok(())
	}
}
