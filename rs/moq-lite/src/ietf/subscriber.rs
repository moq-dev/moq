use std::collections::{HashMap, hash_map::Entry};

use crate::{
	Broadcast, BroadcastDynamic, Error, Frame, FrameProducer, Group, GroupProducer, OriginProducer, Path, PathOwned,
	Track, TrackProducer,
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
}

struct BroadcastState {
	producer: BroadcastProducer,

	// active number of PUBLISH or PUBLISH_NAMESPACE messages.
	count: usize,
}

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,
	origin: Option<OriginProducer>,
	control: Control,
	state: Lock<State>,
	version: Version,
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(session: S, origin: Option<OriginProducer>, control: Control, version: Version) -> Self {
		Self {
			session,
			origin,
			control,
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

		// Write SubscribeNamespace
		let msg = ietf::SubscribeNamespace {
			request_id,
			namespace: prefix.clone(),
			subscribe_options: 0x01, // NAMESPACE only
		};

		stream.writer.encode(&ietf::SubscribeNamespace::ID).await?;
		stream.writer.encode(&msg).await?;

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
					let _ = self.stop_announce(path);
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
					let _ = self.stop_announce(path);
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

		self.stop_announce(path)?;

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
			let _ = self.stop_announce(path);
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
			Version::Draft17 => {
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
			Version::Draft17 => {
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
			Version::Draft17 => {
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
			Version::Draft17 => {
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

		let mut state = self.state.lock();
		let broadcast = match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().count += 1;
				return Ok(entry.get().producer.clone());
			}
			Entry::Vacant(entry) => {
				// IETF protocol doesn't have hops; use 1 (remote source).
				let broadcast = Broadcast::new().with_hops(1).produce();
				origin.publish_broadcast(path.clone(), broadcast.consume());
				entry.insert(BroadcastState {
					producer: broadcast.clone(),
					count: 1,
				});
				broadcast
			}
		};

		tracing::debug!(broadcast = %origin.absolute(&path), "announce");

		let this = self.clone();
		let producer = broadcast.clone();

		web_async::spawn(async move {
			if let Err(err) = this.run_broadcast(path.clone(), producer.dynamic()).await {
				tracing::debug!(%err, "error running broadcast");
			}
			this.state.lock().broadcasts.remove(&path);
		});

		Ok(broadcast)
	}

	fn stop_announce(&mut self, path: PathOwned) -> Result<(), Error> {
		let Some(origin) = &self.origin else {
			return Err(Error::InvalidRole);
		};

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

		let mut state = self.state.lock();
		match state.subscribes.entry(request_id) {
			Entry::Vacant(entry) => {
				entry.insert(TrackState {
					producer: track.clone(),
					alias: Some(msg.track_alias),
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
		broadcast.insert_track(&track)?;

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
			web_async::spawn(async move {
				this.run_subscribe(path, track).await;
			});
		}

		Ok(())
	}

	async fn run_subscribe(&mut self, broadcast: Path<'_>, mut track: TrackProducer) {
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
				},
			);
		}

		// Write Subscribe message
		if let Err(err) = self.write_subscribe(&mut stream, request_id, &broadcast, &track).await {
			tracing::debug!(%err, "failed to write subscribe");
			self.state.lock().subscribes.remove(&request_id);
			let _ = track.abort(err);
			return;
		}

		tracing::info!(broadcast = %self.origin.as_ref().expect("origin set by start_announce").absolute(&broadcast), track = %track.info.name, "subscribe started");

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

		// Wait for track unused or PublishDone (stream reader close)
		tokio::select! {
			_ = track.unused() => {
				tracing::info!(broadcast = %self.origin.as_ref().expect("origin set by start_announce").absolute(&broadcast), track = %track.info.name, "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
			}
			res = stream.reader.closed() => {
				match res {
					Ok(()) => {
						tracing::info!(broadcast = %self.origin.as_ref().expect("origin set by start_announce").absolute(&broadcast), track = %track.info.name, "subscribe complete");
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
				track_name: (&track.info.name).into(),
				subscriber_priority: track.info.priority,
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
		tracing::trace!(?group, "received group header");

		if group.sub_group_id != 0 {
			tracing::warn!(sub_group_id = %group.sub_group_id, "subgroup ID is not supported, dropping stream");
			return Err(Error::Unsupported);
		}

		let mut producer = {
			let mut state = self.state.lock();
			let request_id = match state.aliases.get(&group.track_alias) {
				Some(request_id) => *request_id,
				None => {
					tracing::warn!(track_alias = %group.track_alias, "unknown track alias, using request ID");
					RequestId(group.track_alias)
				}
			};
			let track = state.subscribes.get_mut(&request_id).ok_or(Error::NotFound)?;

			let group = Group {
				sequence: group.group_id,
			};
			track.producer.create_group(group)?
		};

		let res = tokio::select! {
			err = producer.closed() => Err(err),
			res = self.run_group(group, stream, producer.clone()) => res,
		};

		match res {
			Err(Error::Cancel) => {
				tracing::trace!(group = %producer.info.sequence, "group cancelled");
				let _ = producer.abort(Error::Cancel);
			}
			Err(err) => {
				tracing::debug!(%err, group = %producer.info.sequence, "group error");
				let _ = producer.abort(err);
			}
			_ => {
				tracing::trace!(group = %producer.info.sequence, "group complete");
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
					frame.finish()?;
				} else if status == 3 && !group.flags.has_end {
					break;
				} else {
					return Err(Error::Unsupported);
				}
			} else {
				let mut frame = producer.create_frame(Frame { size })?;

				if let Err(err) = self.run_frame(stream, frame.clone()).await {
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
	) -> Result<(), Error> {
		let mut remain = frame.info.size;

		tracing::trace!(size = %frame.info.size, "reading frame");

		while remain > 0 {
			let chunk = stream.read(remain as usize).await?.ok_or(Error::WrongSize)?;
			remain = remain.checked_sub(chunk.len() as u64).ok_or(Error::WrongSize)?;
			frame.write(chunk)?;
		}

		tracing::trace!(size = %frame.info.size, "read frame");

		Ok(())
	}
}
