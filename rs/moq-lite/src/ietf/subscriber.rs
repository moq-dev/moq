use std::{
	collections::{HashMap, hash_map::Entry},
	sync::Arc,
};

use crate::{
	Broadcast, Error, Frame, FrameProducer, Group, GroupProducer, OriginProducer, Path, PathOwned, Track,
	TrackProducer,
	coding::Reader,
	ietf::{self, Control, FetchHeader, FilterType, GroupFlags, GroupOrder, MessageParameters, RequestId, Version},
	model::BroadcastProducer,
};

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

	// Maps PublishNamespace request_id → track_namespace (for v16 PublishNamespaceDone)
	publish_namespace_ids: HashMap<RequestId, PathOwned>,
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
	state: Lock<State>,
	control: Control,

	version: Version,
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(session: S, origin: Option<OriginProducer>, control: Control, version: Version) -> Self {
		Self {
			session,
			origin,
			state: Default::default(),
			control,
			version,
		}
	}

	pub fn recv_publish_namespace(&mut self, msg: ietf::PublishNamespace) -> Result<(), Error> {
		let request_id = msg.request_id;

		// Track the request_id → namespace mapping for v16 PublishNamespaceDone
		{
			let mut state = self.state.lock();
			state
				.publish_namespace_ids
				.insert(request_id, msg.track_namespace.to_owned());
		}

		match self.start_announce(msg.track_namespace.to_owned()) {
			Ok(_) => self.send_ok(request_id),
			Err(err) => self.send_error(request_id, 400, &err.to_string()),
		}
	}

	/// Send a generic OK response, using the version-appropriate message.
	fn send_ok(&self, request_id: RequestId) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => self.control.send(ietf::PublishNamespaceOk { request_id }),
			Version::Draft15 | Version::Draft16 => self.control.send(ietf::RequestOk {
				request_id,
				parameters: MessageParameters::default(),
			}),
		}
	}

	/// Send a generic error response, using the version-appropriate message.
	fn send_error(&self, request_id: RequestId, error_code: u64, reason: &str) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => self.control.send(ietf::PublishNamespaceError {
				request_id,
				error_code,
				reason_phrase: reason.into(),
			}),
			Version::Draft15 | Version::Draft16 => self.control.send(ietf::RequestError {
				request_id,
				error_code,
				reason_phrase: reason.into(),
				retry_interval: 0,
			}),
		}
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
				let broadcast = Broadcast::produce();
				origin.publish_broadcast(path.clone(), broadcast.consume());
				entry.insert(BroadcastState {
					producer: broadcast.clone(),
					count: 1,
				});
				broadcast
			}
		};

		tracing::debug!(broadcast = %origin.absolute(&path), "announce");

		let mut this = self.clone();
		let producer = broadcast.clone();

		web_async::spawn(async move {
			if let Err(err) = this.run_broadcast(path.clone(), producer).await {
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

		// Close the producer if this was the last announce.
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

	pub fn recv_publish_namespace_done(&mut self, msg: ietf::PublishNamespaceDone) -> Result<(), Error> {
		match self.version {
			Version::Draft14 | Version::Draft15 => self.stop_announce(msg.track_namespace.to_owned()),
			Version::Draft16 => {
				// In v16, PublishNamespaceDone uses request_id instead of track_namespace
				let state = self.state.lock();
				let path = state.publish_namespace_ids.get(&msg.request_id).cloned();
				drop(state);

				if let Some(path) = path {
					self.state.lock().publish_namespace_ids.remove(&msg.request_id);
					self.stop_announce(path)
				} else {
					tracing::warn!(request_id = %msg.request_id, "unknown publish_namespace request_id in done");
					Ok(())
				}
			}
		}
	}

	pub fn recv_subscribe_ok(&mut self, msg: ietf::SubscribeOk) -> Result<(), Error> {
		// Save the track alias
		let mut state = self.state.lock();
		if let Some(subscribe) = state.subscribes.get_mut(&msg.request_id) {
			subscribe.alias = Some(msg.track_alias);
			state.aliases.insert(msg.track_alias, msg.request_id);
		}

		Ok(())
	}

	pub fn recv_subscribe_error(&mut self, msg: ietf::SubscribeError) -> Result<(), Error> {
		let mut state = self.state.lock();

		if let Some(track) = state.subscribes.remove(&msg.request_id) {
			track.producer.abort(Error::Cancel);
			if let Some(alias) = track.alias {
				state.aliases.remove(&alias);
			}
		}

		Ok(())
	}

	pub fn recv_request_ok(&mut self, _msg: &ietf::RequestOk) -> Result<(), Error> {
		// v15: generic OK response. SubscribeOk is still separate (0x04).
		// Other request types (publish_namespace, fetch) are no-ops for us.
		Ok(())
	}

	pub fn recv_request_error(&mut self, msg: &ietf::RequestError<'_>) -> Result<(), Error> {
		// v15: generic error response. Check if it's a subscribe error.
		let mut state = self.state.lock();

		if let Some(track) = state.subscribes.remove(&msg.request_id) {
			track.producer.abort(Error::Cancel);
			if let Some(alias) = track.alias {
				state.aliases.remove(&alias);
			}
		}

		Ok(())
	}

	pub fn recv_publish_done(&mut self, msg: ietf::PublishDone<'_>) -> Result<(), Error> {
		let mut state = self.state.lock();

		if let Some(track) = state.subscribes.remove(&msg.request_id) {
			track.producer.close();
			if let Some(alias) = track.alias {
				state.aliases.remove(&alias);
			}
		}

		if let Some(path) = state.publishes.remove(&msg.request_id) {
			drop(state);
			self.stop_announce(path)?;
		}

		Ok(())
	}

	pub async fn run(self) -> Result<(), Error> {
		loop {
			let stream = self
				.session
				.accept_uni()
				.await
				.map_err(|err| Error::Transport(Arc::new(err)))?;

			let stream = Reader::new(stream, self.version);
			let this = self.clone();

			web_async::spawn(async move {
				if let Err(err) = this.run_uni_stream(stream).await {
					tracing::debug!(%err, "error running uni stream");
				}
			});
		}
	}

	async fn run_uni_stream(mut self, mut stream: Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let kind: u64 = stream.decode_peek().await?;

		match kind {
			FetchHeader::TYPE => return Err(Error::Unsupported),
			GroupFlags::START..=GroupFlags::END | GroupFlags::START_NO_PRIORITY..=GroupFlags::END_NO_PRIORITY => {}
			_ => return Err(Error::UnexpectedStream),
		}

		if let Err(err) = self.recv_group(&mut stream).await {
			stream.abort(&err);
		}

		Ok(())
	}

	async fn run_broadcast(&mut self, path: Path<'_>, mut broadcast: BroadcastProducer) -> Result<(), Error> {
		// Actually start serving subscriptions.
		loop {
			// Keep serving requests until there are no more consumers.
			// This way we'll clean up the task when the broadcast is no longer needed.
			let track = tokio::select! {
				_ = broadcast.unused() => break,
				producer = broadcast.requested_track() => match producer {
					Some(producer) => producer,
					None => break,
				},
				_ = self.session.closed() => break,
			};

			let request_id = self.control.next_request_id().await?;
			let mut this = self.clone();

			let mut state = self.state.lock();
			state.subscribes.insert(
				request_id,
				TrackState {
					producer: track.clone(),
					alias: None,
				},
			);

			let path = path.to_owned();
			web_async::spawn(async move {
				if let Err(err) = this.run_subscribe(request_id, path, track).await {
					tracing::debug!(%err, id = %request_id, "error running subscribe");
				}
				this.state.lock().subscribes.remove(&request_id);
			});
		}

		Ok(())
	}

	async fn run_subscribe(
		&mut self,
		request_id: RequestId,
		broadcast: Path<'_>,
		track: TrackProducer,
	) -> Result<(), Error> {
		self.control.send(ietf::Subscribe {
			request_id,
			track_namespace: broadcast.to_owned(),
			track_name: (&track.info.name).into(),
			subscriber_priority: track.info.priority,
			group_order: GroupOrder::Descending,
			// we want largest group
			filter_type: FilterType::LargestObject,
		})?;

		// TODO we should send a joining fetch, but it's annoying to implement.
		// We hope instead that publisher start subscriptions at group boundaries.

		tracing::info!(id = %request_id, broadcast = %self.origin.as_ref().unwrap().absolute(&broadcast), track = %track.info.name, "subscribe started");

		track.unused().await;
		tracing::info!(id = %request_id, broadcast = %self.origin.as_ref().unwrap().absolute(&broadcast), track = %track.info.name, "subscribe cancelled");

		track.abort(Error::Cancel);

		Ok(())
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let group: ietf::GroupHeader = stream.decode().await?;
		tracing::trace!(?group, "received group header");

		if group.sub_group_id != 0 {
			tracing::warn!(sub_group_id = %group.sub_group_id, "subgroup ID is not supported, stripping");
		}

		let producer = {
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
			track.producer.create_group(group).ok_or(Error::Old)?
		};

		let res = tokio::select! {
			_ = producer.unused() => Err(Error::Cancel),
			res = self.run_group(group, stream, producer.clone()) => res,
		};

		match res {
			Err(Error::Cancel) | Err(Error::Transport(_)) => {
				tracing::trace!(group = %producer.info.sequence, "group cancelled");
				producer.abort(Error::Cancel);
			}
			Err(err) => {
				tracing::debug!(%err, group = %producer.info.sequence, "group error");
				producer.abort(err);
			}
			_ => {
				tracing::trace!(group = %producer.info.sequence, "group complete");
				producer.close();
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
				tracing::warn!(id_delta = %id_delta, "object ID gaps not supported, ignoring");
			}

			if group.flags.has_extensions {
				let size: usize = stream.decode().await?;
				stream.skip(size).await?;
			}

			let size: u64 = stream.decode().await?;
			if size == 0 {
				// Have to read the object status.
				let status: u64 = stream.decode().await?;
				if status == 0 {
					// Empty frame
					let frame = producer.create_frame(Frame { size: 0 });
					frame.close();
				} else if status == 3 && !group.flags.has_end {
					// End of group
					break;
				} else {
					return Err(Error::Unsupported);
				}
			} else {
				let frame = producer.create_frame(Frame { size });

				let res = tokio::select! {
					_ = frame.unused() => Err(Error::Cancel),
					res = self.run_frame(stream, frame.clone()) => res,
				};

				if let Err(err) = res {
					frame.abort(err.clone());
					return Err(err);
				}
			}
		}

		producer.close();

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
			frame.write_chunk(chunk);
		}

		tracing::trace!(size = %frame.info.size, "read frame");

		frame.close();

		Ok(())
	}

	pub fn recv_subscribe_namespace_ok(&mut self, _msg: ietf::SubscribeNamespaceOk) -> Result<(), Error> {
		Err(Error::Unsupported)
	}

	pub fn recv_subscribe_namespace_error(&mut self, _msg: ietf::SubscribeNamespaceError<'_>) -> Result<(), Error> {
		Err(Error::Unsupported)
	}

	pub fn recv_fetch_ok(&mut self, _msg: ietf::FetchOk) -> Result<(), Error> {
		Err(Error::Unsupported)
	}

	pub fn recv_fetch_error(&mut self, _msg: ietf::FetchError<'_>) -> Result<(), Error> {
		Err(Error::Unsupported)
	}

	pub fn recv_publish(&mut self, msg: ietf::Publish<'_>) -> Result<(), Error> {
		if let Err(err) = self.start_publish(&msg) {
			match self.version {
				Version::Draft14 => {
					self.control.send(ietf::PublishError {
						request_id: msg.request_id,
						error_code: 400,
						reason_phrase: err.to_string().into(),
					})?;
				}
				Version::Draft15 | Version::Draft16 => {
					self.control.send(ietf::RequestError {
						request_id: msg.request_id,
						error_code: 400,
						reason_phrase: err.to_string().into(),
						retry_interval: 0,
					})?;
				}
			}
		} else {
			match self.version {
				Version::Draft14 => {
					self.control.send(ietf::PublishOk {
						request_id: msg.request_id,
						forward: true,
						subscriber_priority: 0,
						group_order: GroupOrder::Descending,
						filter_type: FilterType::LargestObject,
					})?;
				}
				Version::Draft15 | Version::Draft16 => {
					self.control.send(ietf::RequestOk {
						request_id: msg.request_id,
						parameters: MessageParameters::default(),
					})?;
				}
			}
		}

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

		// Save that we're implicitly announcing this track.
		state.publishes.insert(request_id, msg.track_namespace.to_owned());
		drop(state);

		// Announce our namespace if we haven't already.
		// NOTE: This is debated in the IETF draft, but is significantly easier to implement.
		let mut broadcast = self.start_announce(msg.track_namespace.to_owned())?;

		let exists = broadcast.insert_track(track);
		if exists {
			tracing::warn!(track = %msg.track_name, "track already exists, replacing it");
		}

		Ok(())
	}
}
