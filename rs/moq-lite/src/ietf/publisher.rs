use std::collections::HashMap;

use tokio::sync::oneshot;
use web_async::{FuturesExt, Lock};
use web_transport_trait::SendStream;

use crate::{
	AsPath, Error, Origin, OriginConsumer, Track, TrackConsumer,
	coding::Writer,
	ietf::{
		self, Control, FetchHeader, FetchType, FilterType, GroupOrder, Location, Message, MessageParameters, RequestId,
		Version,
	},
	model::GroupConsumer,
};

#[derive(Clone)]
pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: OriginConsumer,
	control: Control,

	// Drop in order to cancel the subscribe.
	subscribes: Lock<HashMap<RequestId, oneshot::Sender<()>>>,

	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(session: S, origin: Option<OriginConsumer>, control: Control, version: Version) -> Self {
		// Default to a dummy origin that is immediately closed.
		let origin = origin.unwrap_or_else(|| Origin::produce().consume());
		Self {
			session,
			origin,
			control,
			subscribes: Default::default(),
			version,
		}
	}

	pub async fn run(mut self) -> Result<(), Error> {
		// Track request_id → namespace mapping for v16 PublishNamespaceDone
		let mut namespace_requests: HashMap<crate::PathOwned, RequestId> = HashMap::new();

		while let Some((path, active)) = self.origin.announced().await {
			let suffix = path.to_owned();

			if active.is_some() {
				tracing::debug!(broadcast = %self.origin.absolute(&path), "announce");

				let request_id = self.control.next_request_id().await?;
				namespace_requests.insert(suffix.clone(), request_id);

				self.control.send(ietf::PublishNamespace {
					request_id,
					track_namespace: suffix,
				})?;
			} else {
				tracing::debug!(broadcast = %self.origin.absolute(&path), "unannounce");
				if let Some(request_id) = namespace_requests.remove(&suffix) {
					self.control.send(ietf::PublishNamespaceDone {
						track_namespace: suffix,
						request_id,
					})?;
				} else {
					tracing::warn!(broadcast = %self.origin.absolute(&path), "unannounce for unknown namespace");
				}
			}
		}

		Ok(())
	}

	pub fn recv_subscribe(&mut self, msg: ietf::Subscribe<'_>) -> Result<(), Error> {
		match msg.filter_type {
			FilterType::AbsoluteStart | FilterType::AbsoluteRange => {
				tracing::warn!(?msg, "absolute subscribe not supported, ignoring");
			}
			FilterType::NextGroup => {
				tracing::warn!(?msg, "next group subscribe not supported, ignoring");
			}
			// We actually send LargestGroup, which the peer can't enforce anyway.
			FilterType::LargestObject => {}
		};

		let request_id = msg.request_id;

		let track = msg.track_name.clone();
		let absolute = self.origin.absolute(&msg.track_namespace).to_owned();

		tracing::info!(id = %request_id, broadcast = %absolute, %track, "subscribed started");

		let Some(broadcast) = self.origin.consume_broadcast(&msg.track_namespace) else {
			return self.send_subscribe_error(request_id, 404, "Broadcast not found");
		};

		let track = Track {
			name: msg.track_name.to_string(),
			priority: msg.subscriber_priority,
		};

		let track = broadcast.subscribe_track(&track);

		let (tx, rx) = oneshot::channel();
		let mut subscribes = self.subscribes.lock();
		subscribes.insert(request_id, tx);

		self.control.send(ietf::SubscribeOk {
			request_id,
			track_alias: request_id.0, // NOTE: using track alias as request id for now
		})?;

		let session = self.session.clone();
		let control = self.control.clone();
		let request_id = msg.request_id;
		let subscribes = self.subscribes.clone();
		let version = self.version;

		web_async::spawn(async move {
			if let Err(err) = Self::run_track(session, track, request_id, rx, version).await {
				control
					.send(ietf::PublishDone {
						request_id,
						status_code: 500,
						stream_count: 0, // TODO send the correct value if we want the peer to block.
						reason_phrase: err.to_string().into(),
					})
					.ok();
			} else {
				control
					.send(ietf::PublishDone {
						request_id,
						status_code: 200,
						stream_count: 0, // TODO send the correct value if we want the peer to block.
						reason_phrase: "OK".into(),
					})
					.ok();
			}

			subscribes.lock().remove(&request_id);
		});

		Ok(())
	}

	/// Send a subscribe error, using RequestError for v15+.
	fn send_subscribe_error(&self, request_id: RequestId, error_code: u64, reason: &str) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => self.control.send(ietf::SubscribeError {
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

	pub fn recv_subscribe_update(&mut self, msg: ietf::SubscribeUpdate) -> Result<(), Error> {
		self.send_subscribe_error(msg.request_id, 500, "subscribe update not supported")
	}

	async fn run_track(
		session: S,
		mut track: TrackConsumer,
		request_id: RequestId,
		mut cancel: oneshot::Receiver<()>,
		version: Version,
	) -> Result<(), Error> {
		// TODO use a BTreeMap serve the latest N groups by sequence.
		// Until then, we'll implement N=2 manually.
		// Also, this is more complicated because we can't use tokio because of WASM.
		// We need to drop futures in order to cancel them and keep polling them with select!
		let mut old_group = None;
		let mut new_group = None;

		// Annoying that we can't use a tuple here as we need the compiler to infer the type.
		// Otherwise we'd have to pick Send or !Send...
		let mut old_sequence = None;
		let mut new_sequence = None;

		// Keep reading groups from the track, some of which may arrive out of order.
		loop {
			let group = tokio::select! {
				biased;
				_ = &mut cancel => return Ok(()),
				_ = session.closed() => return Err(Error::Transport),
				Some(group) = track.next_group().transpose() => group,
				Some(_) = async { Some(old_group.as_mut()?.await) } => {
					old_group = None;
					old_sequence = None;
					continue;
				},
				Some(_) = async { Some(new_group.as_mut()?.await) } => {
					new_group = old_group;
					new_sequence = old_sequence;
					old_group = None;
					old_sequence = None;
					continue;
				},
				else => return Ok(()),
			}?;

			let sequence = group.info.sequence;
			let latest = new_sequence.as_ref().unwrap_or(&0);

			tracing::debug!(subscribe = %request_id, track = %track.info.name, sequence, latest, "serving group");

			// If this group is older than the oldest group we're serving, skip it.
			// We always serve at most two groups, but maybe we should serve only sequence >= MAX-1.
			if sequence < *old_sequence.as_ref().unwrap_or(&0) {
				tracing::debug!(subscribe = %request_id, track = %track.info.name, old = %sequence, %latest, "skipping group");
				continue;
			}

			let msg = ietf::GroupHeader {
				track_alias: request_id.0, // NOTE: using track alias as request id for now
				group_id: sequence,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			};

			// Spawn a task to serve this group, ignoring any errors because they don't really matter.
			// TODO add some logging at least.
			let handle = Box::pin(Self::run_group(
				session.clone(),
				msg,
				track.info.priority,
				group,
				version,
			));

			// Terminate the old group if it's still running.
			if let Some(old_sequence) = old_sequence.take() {
				tracing::debug!(subscribe = %request_id, track = %track.info.name, old = %old_sequence, %latest, "aborting group");
				old_group.take(); // Drop the future to cancel it.
			}

			assert!(old_group.is_none());

			if sequence >= *latest {
				old_group = new_group;
				old_sequence = new_sequence;

				new_group = Some(handle);
				new_sequence = Some(sequence);
			} else {
				old_group = Some(handle);
				old_sequence = Some(sequence);
			}
		}
	}

	async fn run_group(
		session: S,
		msg: ietf::GroupHeader,
		priority: u8,
		mut group: GroupConsumer,
		version: Version,
	) -> Result<(), Error> {
		// TODO add a way to open in priority order.
		let mut stream = session.open_uni().await.map_err(Error::from_transport)?;
		stream.set_priority(priority);

		let mut stream = Writer::new(stream, version);

		// Encode the GroupHeader
		stream.encode(&msg).await?;

		tracing::trace!(?msg, "sending group header");

		loop {
			let frame = tokio::select! {
				biased;
				_ = stream.closed() => return Err(Error::Cancel),
				frame = group.next_frame() => frame,
			};

			let mut frame = match frame? {
				Some(frame) => frame,
				None => break,
			};

			// object id delta is always 0.
			stream.encode(&0u64).await?;

			// not using extensions.
			if msg.flags.has_extensions {
				stream.encode(&0u64).await?;
			}

			// Write the size of the frame.
			stream.encode(&frame.info.size).await?;

			if frame.info.size == 0 {
				// Have to write the object status too.
				stream.encode(&0u8).await?;
			} else {
				// Stream each chunk of the frame.
				loop {
					let chunk = tokio::select! {
						biased;
						_ = stream.closed() => return Err(Error::Cancel),
						chunk = frame.read_chunk() => chunk,
					};

					match chunk? {
						Some(mut chunk) => {
							stream.write_all(&mut chunk).await?;
						}
						None => break,
					}
				}
			}
		}

		stream.finish()?;

		// Wait until everything is acknowledged by the peer so we can still cancel the stream.
		stream.closed().await?;

		tracing::debug!(sequence = %msg.group_id, "finished group");

		Ok(())
	}

	pub fn recv_unsubscribe(&mut self, msg: ietf::Unsubscribe) -> Result<(), Error> {
		let mut subscribes = self.subscribes.lock();
		if let Some(tx) = subscribes.remove(&msg.request_id) {
			let _ = tx.send(());
		}
		Ok(())
	}

	pub fn recv_publish_namespace_ok(&mut self, _msg: ietf::PublishNamespaceOk) -> Result<(), Error> {
		// We don't care.
		Ok(())
	}

	pub fn recv_request_ok(&mut self, _msg: &ietf::RequestOk) -> Result<(), Error> {
		// v15: generic OK response. For publish_namespace, we don't care.
		Ok(())
	}

	pub fn recv_request_error(&mut self, msg: &ietf::RequestError<'_>) -> Result<(), Error> {
		// v15: generic error response. Log it like publish_namespace_error.
		tracing::warn!(?msg, "request error");
		Ok(())
	}

	pub fn recv_subscribe_namespace(&mut self, _msg: ietf::SubscribeNamespace<'_>) -> Result<(), Error> {
		// We don't care, we're sending all announcements anyway.
		Ok(())
	}

	pub fn recv_publish_namespace_error(&mut self, msg: ietf::PublishNamespaceError<'_>) -> Result<(), Error> {
		tracing::warn!(?msg, "publish namespace error");
		Ok(())
	}

	pub fn recv_unsubscribe_namespace(&mut self, _msg: ietf::UnsubscribeNamespace) -> Result<(), Error> {
		// We don't care, we're sending all announcements anyway.
		Ok(())
	}

	pub fn recv_publish_namespace_cancel(&mut self, msg: ietf::PublishNamespaceCancel<'_>) -> Result<(), Error> {
		tracing::warn!(?msg, "publish namespace cancel");
		Ok(())
	}

	pub fn recv_track_status(&mut self, _msg: ietf::TrackStatus<'_>) -> Result<(), Error> {
		Err(Error::Unsupported)
	}

	pub fn recv_fetch(&mut self, msg: ietf::Fetch<'_>) -> Result<(), Error> {
		let subscribe_id = match msg.fetch_type {
			FetchType::Standalone { .. } => {
				return self.send_fetch_error(msg.request_id, 500, "not supported");
			}
			FetchType::RelativeJoining {
				subscriber_request_id,
				group_offset,
			} => {
				if group_offset != 0 {
					return self.send_fetch_error(msg.request_id, 500, "not supported");
				}

				subscriber_request_id
			}
			FetchType::AbsoluteJoining { .. } => {
				return self.send_fetch_error(msg.request_id, 500, "not supported");
			}
		};

		let subscribes = self.subscribes.lock();
		if !subscribes.contains_key(&subscribe_id) {
			return self.send_fetch_error(msg.request_id, 404, "Subscribe not found");
		}

		self.send_fetch_ok(msg.request_id)?;

		let session = self.session.clone();
		let request_id = msg.request_id;
		let version = self.version;

		web_async::spawn(async move {
			if let Err(err) = Self::run_fetch(session, request_id, version).await {
				tracing::warn!(?err, "error running fetch");
			}
		});

		Ok(())
	}

	/// Send a fetch OK, using RequestOk for v15+.
	fn send_fetch_ok(&self, request_id: RequestId) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => self.control.send(ietf::FetchOk {
				request_id,
				group_order: GroupOrder::Descending,
				end_of_track: false,
				end_location: Location { group: 0, object: 0 },
			}),
			Version::Draft15 | Version::Draft16 => self.control.send(ietf::RequestOk {
				request_id,
				parameters: MessageParameters::default(),
			}),
		}
	}

	/// Send a fetch error, using RequestError for v15+.
	fn send_fetch_error(&self, request_id: RequestId, error_code: u64, reason: &str) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => self.control.send(ietf::FetchError {
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

	// We literally just create a stream and FIN it.
	async fn run_fetch(session: S, request_id: RequestId, version: Version) -> Result<(), Error> {
		let stream = session.open_uni().await.map_err(Error::from_transport)?;

		let mut writer = Writer::new(stream, version);

		// Encode the stream type and FetchHeader
		writer.encode(&FetchHeader::TYPE).await?;
		writer.encode(&FetchHeader { request_id }).await?;

		writer.finish()?;
		writer.closed().await?;

		Ok(())
	}

	pub fn recv_fetch_cancel(&mut self, msg: ietf::FetchCancel) -> Result<(), Error> {
		tracing::warn!(?msg, "fetch cancel");
		Ok(())
	}

	/// Handle a SUBSCRIBE_NAMESPACE message received on a v16 bidirectional stream.
	/// Reads the request, sends REQUEST_OK, then streams NAMESPACE/NAMESPACE_DONE messages.
	pub async fn recv_subscribe_namespace_stream(
		&mut self,
		mut stream: crate::coding::Stream<S, Version>,
	) -> Result<(), Error> {
		let msg: ietf::SubscribeNamespace = stream.reader.decode().await?;
		let prefix = msg.namespace.to_owned();

		tracing::debug!(prefix = %self.origin.absolute(&prefix), "subscribe_namespace stream");

		// Create a filtered consumer for this prefix
		let mut origin = self
			.origin
			.consume_only(&[prefix.as_path()])
			.ok_or(Error::Unauthorized)?;

		// Send REQUEST_OK
		stream.writer.encode(&ietf::RequestOk::ID).await?;
		stream
			.writer
			.encode(&ietf::RequestOk {
				request_id: msg.request_id,
				parameters: ietf::MessageParameters::default(),
			})
			.await?;

		// Send initial NAMESPACE messages for currently active namespaces
		while let Some((path, active)) = origin.try_announced() {
			let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path");
			if active.is_some() {
				tracing::debug!(broadcast = %origin.absolute(&path), "namespace");
				stream.writer.encode(&ietf::Namespace::ID).await?;
				stream
					.writer
					.encode(&ietf::Namespace {
						suffix: suffix.to_owned(),
					})
					.await?;
			}
		}

		// Stream updates
		loop {
			tokio::select! {
				biased;
				res = stream.reader.closed() => return res,
				announced = origin.announced() => {
					match announced {
						Some((path, active)) => {
							let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path").to_owned();
							if active.is_some() {
								tracing::debug!(broadcast = %origin.absolute(&path), "namespace");
								stream.writer.encode(&ietf::Namespace::ID).await?;
								stream.writer.encode(&ietf::Namespace { suffix }).await?;
							} else {
								tracing::debug!(broadcast = %origin.absolute(&path), "namespace_done");
								stream.writer.encode(&ietf::NamespaceDone::ID).await?;
								stream.writer.encode(&ietf::NamespaceDone { suffix }).await?;
							}
						}
						None => {
							stream.writer.finish()?;
							return stream.writer.closed().await;
						}
					}
				}
			}
		}
	}
}
