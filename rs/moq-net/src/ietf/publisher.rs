use crate::{group, origin, track};
use std::{collections::HashMap, task::Poll};

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_transport_trait::SendStream;

use crate::{
	AsPath, Error, Timescale,
	coding::{Stream, Writer},
	ietf::{self, Control, FetchHeader, FetchType, FilterType, GroupOrder, Location, RequestId},
	track::Subscription,
	util::{MaybeBoxedExt, MaybeSendBox},
};

use super::{Message, Version};

/// A broadcast whose route table is watched for `advertisable` flips while the
/// peer has an assigned identity (`Client::with_peer_origin`).
struct Watched {
	broadcast: crate::broadcast::Consumer,
	/// Set once the broadcast errors, so a dead entry stops being polled.
	dead: bool,
}

impl Watched {
	fn new(broadcast: crate::broadcast::Consumer) -> Self {
		Self { broadcast, dead: false }
	}
}

/// What woke an announce-forwarding loop.
enum NamespaceEvent {
	/// The session or stream ended, with the result to surface.
	Closed(Result<(), Error>),
	/// An origin-level (un)announce, `None` once the announce stream ends.
	Update(Option<crate::announce::Update>),
	/// A watched broadcast's route table changed; re-decide `advertisable`.
	Routes(crate::PathOwned),
}

#[derive(Clone)]
pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Consumer,
	control: Control,
	// The identity assigned to the peer by `Client::with_peer_origin`.
	// moq-transport has no exclude-hop on the wire, so this is the only signal
	// for filtering announces that would echo the peer's own broadcasts back at
	// it; the data plane is covered by the exclusion the client set on `origin`.
	peer_origin: Option<crate::Origin>,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(
		session: S,
		origin: origin::Consumer,
		control: Control,
		peer_origin: Option<crate::Origin>,
		version: Version,
	) -> Self {
		Self {
			session,
			origin,
			control,
			peer_origin,
			version,
		}
	}

	/// Whether an announced broadcast should be advertised to this peer: it needs
	/// at least one advertisable route that doesn't flow through the identity the
	/// peer was assigned. A same-path source can splice in or detach without an
	/// origin-level (un)announce, silently flipping this, so the announce loops
	/// watch every announced broadcast's route table (see [`Watched`]) and
	/// advertise or withdraw the namespace when eligibility changes.
	fn advertisable(&self, broadcast: &crate::broadcast::Consumer) -> bool {
		let Some(peer) = self.peer_origin else {
			return true;
		};
		broadcast
			.routes()
			.iter()
			.any(|route| route.announce && !route.hops.contains(&peer))
	}

	/// Poll every watched broadcast for a route-table change, reporting the first
	/// changed path. Only populated when the peer has an assigned identity;
	/// without one `advertisable` is constant and there is nothing to watch.
	fn poll_watched(watched: &mut HashMap<crate::PathOwned, Watched>, waiter: &kio::Waiter) -> Poll<crate::PathOwned> {
		for (path, watch) in watched.iter_mut() {
			if watch.dead {
				continue;
			}
			match watch.broadcast.poll_routes_changed(waiter) {
				Poll::Ready(Ok(())) => return Poll::Ready(path.clone()),
				// A dying broadcast has no further route changes; the origin's
				// unannounce is what removes the entry.
				Poll::Ready(Err(_)) => watch.dead = true,
				Poll::Pending => {}
			}
		}
		Poll::Pending
	}

	/// Handle an incoming bidi stream dispatched by the session.
	pub fn handle_stream(
		&self,
		id: u64,
		mut data: bytes::Bytes,
		stream: Stream<S, Version>,
	) -> Result<MaybeSendBox<'static, ()>, Error> {
		let this = self.clone();
		let task = match id {
			ietf::Subscribe::ID => {
				let msg = ietf::Subscribe::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received subscribe");
				async move {
					if let Err(err) = this.run_subscribe_stream(stream, msg).await {
						tracing::debug!(%err, "subscribe stream error");
					}
				}
				.maybe_boxed()
			}
			ietf::Fetch::ID => {
				let msg = ietf::Fetch::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received fetch");
				async move {
					if let Err(err) = this.run_fetch_stream(stream, msg).await {
						tracing::debug!(%err, "fetch stream error");
					}
				}
				.maybe_boxed()
			}
			// Draft-18 SUBSCRIBE_NAMESPACE (0x50) and the legacy 0x11 message decode
			// to the same request_id + namespace; the legacy Subscribe Options field
			// is ignored (moq-lite never subscribes to tracks).
			ietf::SubscribeNamespace::ID | ietf::SubscribeNamespaceLegacy::ID => {
				let msg = if id == ietf::SubscribeNamespace::ID {
					ietf::SubscribeNamespace::decode_msg(&mut data, this.version)?
				} else {
					let legacy = ietf::SubscribeNamespaceLegacy::decode_msg(&mut data, this.version)?;
					ietf::SubscribeNamespace {
						request_id: legacy.request_id,
						namespace: legacy.namespace,
					}
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received subscribe_namespace");
				async move {
					if let Err(err) = this.run_subscribe_namespace_stream(stream, msg).await {
						tracing::debug!(%err, "subscribe_namespace stream error");
					}
				}
				.maybe_boxed()
			}
			ietf::TrackStatus::ID => {
				tracing::warn!("TrackStatus not supported");
				async {}.maybe_boxed()
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type for publisher");
				return Err(Error::UnexpectedStream);
			}
		};
		Ok(task)
	}

	/// Handle a SUBSCRIBE on its bidi stream.
	async fn run_subscribe_stream(self, mut stream: Stream<S, Version>, msg: ietf::Subscribe<'_>) -> Result<(), Error> {
		match msg.filter_type {
			FilterType::AbsoluteStart | FilterType::AbsoluteRange => {
				tracing::warn!(?msg, "absolute subscribe not supported, ignoring");
			}
			FilterType::NextGroup => {
				tracing::warn!(?msg, "next group subscribe not supported, ignoring");
			}
			FilterType::LargestObject => {}
		};

		let request_id = msg.request_id;
		let track_name = msg.track_name.clone();
		let absolute = self.origin.absolute(&msg.track_namespace).to_owned();

		tracing::info!(id = %request_id, broadcast = %absolute, track = %track_name, "subscribe started");

		// Stats (subscriptions, viewer refcount, groups/frames/bytes) are counted in
		// the model, through the tagged `origin::Consumer` the broadcast resolves from.

		// We just received a subscribe for this exact namespace, so the peer must have already
		// seen the announcement. `request_broadcast` resolves it immediately, or falls back to
		// an `origin::Dynamic` handler if one is registered.
		let broadcast = match self.origin.request_broadcast(&msg.track_namespace).await {
			Ok(broadcast) => broadcast,
			Err(_) => {
				self.write_subscribe_error(&mut stream.writer, request_id, 404, "Broadcast not found")
					.await?;
				return Ok(());
			}
		};

		let subscription = Subscription {
			priority: msg.subscriber_priority,
			..Default::default()
		};

		let track = match async { broadcast.track(&msg.track_name)?.subscribe(subscription).await }.await {
			Ok(track) => track,
			Err(err) => {
				self.write_subscribe_error(&mut stream.writer, request_id, 404, &err.to_string())
					.await?;
				return Ok(());
			}
		};

		// Send SubscribeOk on the stream
		stream.writer.encode(&ietf::SubscribeOk::ID).await?;
		stream
			.writer
			.encode(&ietf::SubscribeOk {
				request_id: match self.version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(request_id),
					_ => None,
				},
				track_alias: request_id.0,
				// Declaring the timescale is what opts the track into timestamps; every
				// object Timestamp below is in these units.
				timescale: Some(track.info().timescale),
			})
			.await?;

		// Run the track, cancelling on reader close (Unsubscribe or stream close)
		let res = {
			let mut serve = std::pin::pin!(self.run_track(track, request_id));
			let mut reader_closed = std::pin::pin!(stream.reader.closed());
			let mut session_closed = std::pin::pin!(self.session.closed());
			kio::wait(|waiter| {
				if let Poll::Ready(res) = waiter.poll_future(serve.as_mut()) {
					return Poll::Ready(res);
				}
				if waiter.poll_future(reader_closed.as_mut()).is_ready()
					|| waiter.poll_future(session_closed.as_mut()).is_ready()
				{
					return Poll::Ready(Ok(()));
				}
				Poll::Pending
			})
			.await
		};

		// Send PublishDone
		let (status_code, reason) = match &res {
			Ok(()) => (200, "OK"),
			Err(_) => (500, "error"),
		};
		let _ = stream.writer.encode(&ietf::PublishDone::ID).await;
		let _ = stream
			.writer
			.encode(&ietf::PublishDone {
				request_id: match self.version {
					Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(request_id),
					_ => None,
				},
				status_code,
				stream_count: 0,
				reason_phrase: reason.into(),
			})
			.await;

		stream.writer.finish().ok();

		res
	}

	/// Write a subscribe error on the bidi stream writer.
	async fn write_subscribe_error(
		&self,
		writer: &mut Writer<S::SendStream, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				writer.encode(&ietf::SubscribeError::ID).await?;
				writer
					.encode(&ietf::SubscribeError {
						request_id,
						error_code,
						reason_phrase: reason.into(),
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
					.encode(&ietf::RequestError {
						request_id: Some(request_id),
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
			_ => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
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

	/// Serve a track using FuturesUnordered for unlimited concurrent groups.
	async fn run_track(&self, mut track: track::Subscriber, request_id: RequestId) -> Result<(), Error> {
		let mut tasks = FuturesUnordered::new();

		loop {
			// Await the next group while driving the in-flight group futures.
			let group = {
				kio::wait(|waiter| {
					let mut cx = std::task::Context::from_waker(waiter.waker());
					while let std::task::Poll::Ready(Some(())) = tasks.poll_next_unpin(&mut cx) {}
					track.poll_recv_group(waiter)
				})
				.await
			};

			let Some(group) = group? else {
				// Track finished: drain the in-flight group futures, then FIN.
				while tasks.next().await.is_some() {}
				return Ok(());
			};

			let sequence = group.sequence;
			tracing::debug!(subscribe = %request_id, track = %track.name(), sequence, "serving group");

			let msg = ietf::GroupHeader {
				track_alias: request_id.0,
				group_id: sequence,
				sub_group_id: 0,
				publisher_priority: 0,
				// Carry per-object timestamps as extension headers (the Timestamp Object
				// Property) so moq-transport peers get the real PTS. The units are the
				// track's, declared once in SUBSCRIBE_OK.
				flags: ietf::GroupFlags {
					has_extensions: true,
					..Default::default()
				},
			};

			let priority = track.subscription().priority;
			let timescale = track.info().timescale;
			tasks
				.push(Self::run_group(self.session.clone(), msg, priority, group, timescale, self.version).map(|_| ()));
		}
	}

	async fn run_group(
		session: S,
		msg: ietf::GroupHeader,
		priority: u8,
		mut group: group::Consumer,
		timescale: Timescale,
		version: Version,
	) -> Result<(), Error> {
		let mut stream = session.open_uni().await.map_err(Error::from_transport)?;
		stream.set_priority(priority);

		let mut stream = Writer::new(stream, version);

		stream.encode(&msg).await?;

		loop {
			// Wait for the next frame, bailing if the peer closes the stream first.
			let frame = {
				let mut closed = std::pin::pin!(stream.closed());
				kio::wait(|waiter| {
					if waiter.poll_future(closed.as_mut()).is_ready() {
						return Poll::Ready(Err(Error::Cancel));
					}
					group.poll_next_frame(waiter)
				})
				.await
			};

			let mut frame = match frame? {
				Some(frame) => frame,
				None => break,
			};

			// object id delta is always 0.
			stream.encode(&0u64).await?;

			// Per-object extension headers carry the frame's presentation timestamp.
			if msg.flags.has_extensions {
				let mut ext = bytes::BytesMut::new();
				ietf::encode_object_time(&mut ext, frame.timestamp, timescale, version)?;
				stream.encode(&(ext.len() as u64)).await?;
				stream.write_chunk(ext.freeze()).await?;
			}

			// Write the size of the frame.
			stream.encode(&frame.size).await?;

			if frame.size == 0 {
				// Have to write the object status too.
				stream.encode(&0u8).await?;
			} else {
				// Stream each chunk of the frame.
				loop {
					let chunk = {
						let mut closed = std::pin::pin!(stream.closed());
						kio::wait(|waiter| {
							if waiter.poll_future(closed.as_mut()).is_ready() {
								return Poll::Ready(Err(Error::Cancel));
							}
							frame.poll_read_chunk(waiter)
						})
						.await
					};

					match chunk? {
						Some(chunk) => {
							stream.write_chunk(chunk).await?;
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

	/// Handle a FETCH on its bidi stream.
	async fn run_fetch_stream(self, mut stream: Stream<S, Version>, msg: ietf::Fetch<'_>) -> Result<(), Error> {
		let _subscribe_id = match msg.fetch_type {
			FetchType::Standalone { .. } => {
				self.write_fetch_error(&mut stream.writer, msg.request_id, 500, "not supported")
					.await?;
				return Ok(());
			}
			FetchType::RelativeJoining {
				subscriber_request_id,
				group_offset,
			} => {
				if group_offset != 0 {
					self.write_fetch_error(&mut stream.writer, msg.request_id, 500, "not supported")
						.await?;
					return Ok(());
				}
				subscriber_request_id
			}
			FetchType::AbsoluteJoining { .. } => {
				self.write_fetch_error(&mut stream.writer, msg.request_id, 500, "not supported")
					.await?;
				return Ok(());
			}
		};

		// Send FetchOk/RequestOk
		self.write_fetch_ok(&mut stream.writer, msg.request_id).await?;

		// Create a uni stream with just a FetchHeader and FIN it
		let uni = self.session.open_uni().await.map_err(Error::from_transport)?;
		let mut writer = Writer::new(uni, self.version);
		writer.encode(&FetchHeader::TYPE).await?;
		writer
			.encode(&FetchHeader {
				request_id: msg.request_id,
			})
			.await?;
		writer.finish()?;
		writer.closed().await?;

		Ok(())
	}

	async fn write_fetch_ok(
		&self,
		writer: &mut Writer<S::SendStream, Version>,
		request_id: RequestId,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				writer.encode(&ietf::FetchOk::ID).await?;
				writer
					.encode(&ietf::FetchOk {
						request_id: Some(request_id),
						group_order: GroupOrder::Descending,
						end_of_track: false,
						end_location: Location { group: 0, object: 0 },
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				writer.encode(&ietf::RequestOk::ID).await?;
				writer
					.encode(&ietf::RequestOk {
						request_id: Some(request_id),
					})
					.await?;
			}
			_ => {
				writer.encode(&ietf::RequestOk::ID).await?;
				writer.encode(&ietf::RequestOk { request_id: None }).await?;
			}
		}
		Ok(())
	}

	async fn write_fetch_error(
		&self,
		writer: &mut Writer<S::SendStream, Version>,
		request_id: RequestId,
		error_code: u64,
		reason: &str,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 => {
				writer.encode(&ietf::FetchError::ID).await?;
				writer
					.encode(&ietf::FetchError {
						request_id,
						error_code,
						reason_phrase: reason.into(),
					})
					.await?;
			}
			Version::Draft15 | Version::Draft16 => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
					.encode(&ietf::RequestError {
						request_id: Some(request_id),
						error_code,
						reason_phrase: reason.into(),
						retry_interval: 0,
					})
					.await?;
			}
			_ => {
				writer.encode(&ietf::RequestError::ID).await?;
				writer
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

	/// Advertise one namespace to a subscribed peer.
	///
	/// Draft-16+ writes a NAMESPACE entry inline on the SUBSCRIBE_NAMESPACE stream.
	/// Draft-14/15 predate that message, so the namespace goes out as a
	/// PUBLISH_NAMESPACE request of its own over the control stream, recorded in
	/// `requests` so a withdrawal can close it out. Returns whether the peer saw
	/// the advertisement (it declined the PUBLISH_NAMESPACE otherwise).
	async fn advertise_namespace(
		&self,
		stream: &mut Stream<S, Version>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
		path: &crate::PathOwned,
		suffix: crate::PathOwned,
	) -> Result<bool, Error> {
		match self.version {
			Version::Draft14 | Version::Draft15 => {
				let request_id = self.control.next_request_id().await?;
				let mut request = Stream::open(&self.session, self.version).await?;

				request.writer.encode(&ietf::PublishNamespace::ID).await?;
				request
					.writer
					.encode(&ietf::PublishNamespace {
						request_id,
						track_namespace: path.as_path(),
					})
					.await?;

				let type_id: u64 = request.reader.decode().await?;
				let size: u16 = request.reader.decode().await?;
				let mut data = request.reader.read_exact(size as usize).await?;

				match (self.version, type_id) {
					(Version::Draft14, ietf::PublishNamespaceOk::ID) => {
						let msg = ietf::PublishNamespaceOk::decode_msg(&mut data, self.version)?;
						tracing::debug!(message = ?msg, "publish namespace ok");
					}
					(Version::Draft14, ietf::PublishNamespaceError::ID) => {
						let msg = ietf::PublishNamespaceError::decode_msg(&mut data, self.version)?;
						tracing::warn!(message = ?msg, "publish namespace error");
						return Ok(false);
					}
					(_, ietf::RequestOk::ID) => {
						let msg = ietf::RequestOk::decode_msg(&mut data, self.version)?;
						tracing::debug!(message = ?msg, "publish namespace ok");
					}
					(_, ietf::RequestError::ID) => {
						let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
						tracing::warn!(message = ?msg, "publish namespace error");
						return Ok(false);
					}
					_ => return Err(Error::UnexpectedMessage),
				}

				requests.insert(
					suffix,
					NamespaceRequest {
						path: path.clone(),
						request_id,
						stream: request,
					},
				);
			}
			_ => {
				stream.writer.encode(&ietf::Namespace::ID).await?;
				stream.writer.encode(&ietf::Namespace { suffix }).await?;
			}
		}
		Ok(true)
	}

	/// Withdraw an advertised namespace: NAMESPACE_DONE inline on draft-16+, or
	/// PUBLISH_NAMESPACE_DONE closing its own request on draft-14/15.
	async fn withdraw_namespace(
		&self,
		stream: &mut Stream<S, Version>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
		suffix: crate::PathOwned,
	) -> Result<(), Error> {
		match self.version {
			Version::Draft14 | Version::Draft15 => {
				if let Some(mut request) = requests.remove(&suffix) {
					// Best effort: the peer may already be gone.
					let _ = request
						.stream
						.writer
						.encode_message(&ietf::PublishNamespaceDone {
							track_namespace: request.path.as_path(),
							request_id: request.request_id,
						})
						.await;
					request.stream.writer.finish().ok();
				}
			}
			_ => {
				stream.writer.encode(&ietf::NamespaceDone::ID).await?;
				stream.writer.encode(&ietf::NamespaceDone { suffix }).await?;
			}
		}
		Ok(())
	}

	/// Close out every open draft-14/15 PUBLISH_NAMESPACE request. A no-op on
	/// later drafts, whose entries ride the SUBSCRIBE_NAMESPACE stream itself.
	async fn withdraw_requests(
		&self,
		stream: &mut Stream<S, Version>,
		requests: &mut HashMap<crate::PathOwned, NamespaceRequest<S>>,
	) {
		let suffixes: Vec<crate::PathOwned> = requests.keys().cloned().collect();
		for suffix in suffixes {
			let _ = self.withdraw_namespace(stream, requests, suffix).await;
		}
	}

	/// Handle a SUBSCRIBE_NAMESPACE on its bidi stream.
	///
	/// Namespaces are only advertised in response to one of these, and all the
	/// announce state is local to this task (mirroring `lite::Publisher`'s
	/// announce handling): whatever this subscription advertised is withdrawn
	/// when its stream ends.
	async fn run_subscribe_namespace_stream(
		self,
		mut stream: Stream<S, Version>,
		msg: ietf::SubscribeNamespace<'_>,
	) -> Result<(), Error> {
		let prefix = msg.namespace.to_owned();

		tracing::debug!(prefix = %self.origin.absolute(&prefix), "subscribe_namespace stream");

		// A prefix outside our scope (empty origin, or a token that doesn't grant it)
		// just means we have nothing to announce; respond with an empty set rather than
		// erroring, which would look fatal to the peer.
		let origin = self
			.origin
			.scope(&[prefix.as_path()])
			.unwrap_or_else(|| self.origin.empty());

		// Send OK response
		match self.version {
			Version::Draft14 => {
				stream.writer.encode(&ietf::SubscribeNamespaceOk::ID).await?;
				stream
					.writer
					.encode(&ietf::SubscribeNamespaceOk {
						request_id: msg.request_id,
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

		let mut announced = origin.announced();

		// Namespaces actually sent to the peer, so a filtered announce (no
		// route avoiding the peer's assigned identity) never gets a dangling
		// withdrawal. Keyed by suffix like the maps below.
		let mut sent: std::collections::HashSet<crate::PathOwned> = std::collections::HashSet::new();
		let mut watched: HashMap<crate::PathOwned, Watched> = HashMap::new();
		// Draft-14/15: the open PUBLISH_NAMESPACE request carrying each advertised
		// namespace. Empty on later drafts, whose entries ride `stream` inline.
		let mut requests: HashMap<crate::PathOwned, NamespaceRequest<S>> = HashMap::new();

		// Stream updates (origin (un)announces plus watched route changes),
		// bailing if the peer closes its side first.
		let res = loop {
			let event = {
				let mut closed = std::pin::pin!(stream.reader.closed());
				kio::wait(|waiter| {
					if let Poll::Ready(res) = waiter.poll_future(closed.as_mut()) {
						return Poll::Ready(NamespaceEvent::Closed(res));
					}
					if let Poll::Ready(update) = announced.poll_next(waiter) {
						return Poll::Ready(NamespaceEvent::Update(update));
					}
					Self::poll_watched(&mut watched, waiter).map(NamespaceEvent::Routes)
				})
				.await
			};

			match event {
				NamespaceEvent::Closed(res) => break res,
				NamespaceEvent::Update(None) => {
					// The origin is gone: withdraw everything, then finish the
					// stream and wait for delivery.
					self.withdraw_requests(&mut stream, &mut requests).await;
					stream.writer.finish()?;
					return stream.writer.closed().await;
				}
				NamespaceEvent::Update(Some(crate::announce::Update { path, broadcast })) => {
					let suffix = path
						.strip_prefix(&prefix)
						.expect("origin returned invalid path")
						.to_owned();
					let absolute = origin.absolute(&path).to_owned();
					let path = path.to_owned();

					match broadcast {
						Some(broadcast) => {
							let advertisable = self.advertisable(&broadcast);
							if self.peer_origin.is_some() {
								watched.insert(suffix.clone(), Watched::new(broadcast));
							}
							// Filtered now, but the watch re-decides when the
							// route table changes.
							if !advertisable {
								continue;
							}
							tracing::debug!(broadcast = %absolute, "namespace");
							if self
								.advertise_namespace(&mut stream, &mut requests, &path, suffix.clone())
								.await?
							{
								sent.insert(suffix);
							}
						}
						None => {
							watched.remove(&suffix);
							// Only close out namespaces the peer actually saw.
							if sent.remove(&suffix) {
								tracing::debug!(broadcast = %absolute, "namespace_done");
								self.withdraw_namespace(&mut stream, &mut requests, suffix).await?;
							}
						}
					}
				}
				NamespaceEvent::Routes(suffix) => {
					let Some(watch) = watched.get(&suffix) else { continue };
					let advertisable = self.advertisable(&watch.broadcast);
					if advertisable && !sent.contains(&suffix) {
						tracing::debug!(broadcast = %suffix, "namespace");
						let path = prefix.join(&suffix);
						if self
							.advertise_namespace(&mut stream, &mut requests, &path, suffix.clone())
							.await?
						{
							sent.insert(suffix);
						}
					} else if !advertisable && sent.remove(&suffix) {
						tracing::debug!(broadcast = %suffix, "namespace_done");
						self.withdraw_namespace(&mut stream, &mut requests, suffix).await?;
					}
				}
			}
		};

		// This subscription's advertisements die with it.
		self.withdraw_requests(&mut stream, &mut requests).await;

		res
	}
}

/// One draft-14/15 advertisement: the PUBLISH_NAMESPACE request it rode on and
/// what closes it out with PUBLISH_NAMESPACE_DONE.
struct NamespaceRequest<S: web_transport_trait::Session> {
	path: crate::PathOwned,
	request_id: RequestId,
	stream: Stream<S, Version>,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::test_transport::SinkSession;

	async fn settle() {
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// A broadcast whose every route flows through the peer's assigned identity
	/// (`Client::with_peer_origin`) is never advertised to that peer; it would only
	/// echo the peer's own content back at it. A broadcast with an independent
	/// route still is.
	#[tokio::test]
	async fn assigned_peer_origin_filters_echoed_announces() {
		let assigned = crate::Origin::new(777).unwrap();
		let other = crate::Origin::new(778).unwrap();

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let publisher = Publisher::new(
			session,
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			Version::Draft16,
		);

		let mut echoed_hops = crate::OriginList::new();
		echoed_hops.push(assigned).unwrap();
		let _echoed = origin
			.create_broadcast(
				"from/peer",
				crate::broadcast::Route::new()
					.with_hops(echoed_hops)
					.with_announce(true),
			)
			.unwrap();

		let mut local_hops = crate::OriginList::new();
		local_hops.push(other).unwrap();
		let _local = origin
			.create_broadcast(
				"from/us",
				crate::broadcast::Route::new().with_hops(local_hops).with_announce(true),
			)
			.unwrap();

		// Broadcast visibility is deferred until the executor ticks.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		let echoed = consumer.get_broadcast("from/peer").unwrap();
		assert!(!publisher.advertisable(&echoed));

		let local = consumer.get_broadcast("from/us").unwrap();
		assert!(publisher.advertisable(&local));
	}

	/// A same-path source can splice into (or detach from) an existing broadcast
	/// without an origin-level (un)announce, silently flipping `advertisable`.
	/// Namespace forwarding must follow: advertise when a clean route appears,
	/// withdraw when the last one detaches.
	#[tokio::test]
	async fn namespace_follows_route_eligibility_changes() {
		let assigned = crate::Origin::new(777).unwrap();
		let clean_publisher = crate::Origin::new(778).unwrap();
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let publisher = Publisher::new(
			session.clone(),
			origin.consume(),
			Control::new(None, false),
			Some(assigned),
			Version::Draft16,
		);

		// The broadcast starts with only a route through the assigned peer.
		let mut tainted_hops = crate::OriginList::new();
		tainted_hops.push(assigned).unwrap();
		let _tainted = origin
			.create_broadcast(
				"route-flip-cam",
				crate::broadcast::Route::new()
					.with_hops(tainted_hops)
					.with_announce(true),
			)
			.unwrap();
		settle().await;

		let stream = Stream::open(&session, Version::Draft16).await.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::new(""),
		};
		let mut run = std::pin::pin!(publisher.run_subscribe_namespace_stream(stream, msg));

		// Initial set: the tainted-only broadcast is filtered, nothing but the OK
		// response on the wire.
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(occurrences(&log, b"route-flip-cam"), 0);

		// A clean source splices in: no origin announce fires, only the route table
		// changes. The namespace must now be advertised.
		let mut clean_hops = crate::OriginList::new();
		clean_hops.push(clean_publisher).unwrap();
		let clean = origin
			.create_broadcast(
				"route-flip-cam",
				crate::broadcast::Route::new().with_hops(clean_hops).with_announce(true),
			)
			.unwrap();
		settle().await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(
			occurrences(&log, b"route-flip-cam"),
			1,
			"NAMESPACE after a clean route joins"
		);

		// The clean source detaches, leaving only the tainted route: withdrawn.
		drop(clean);
		settle().await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(
			occurrences(&log, b"route-flip-cam"),
			2,
			"NAMESPACE_DONE after the last clean route detaches"
		);
	}

	/// The peer's OK to a PUBLISH_NAMESPACE, framed exactly as the announce path
	/// reads it -- built with the crate's own writer so the framing can't drift
	/// from the encoder under test.
	async fn publish_namespace_ok(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		match version {
			Version::Draft14 => {
				writer.encode(&ietf::PublishNamespaceOk::ID).await.unwrap();
				writer
					.encode(&ietf::PublishNamespaceOk {
						request_id: RequestId(1),
					})
					.await
					.unwrap();
			}
			_ => {
				writer.encode(&ietf::RequestOk::ID).await.unwrap();
				writer
					.encode(&ietf::RequestOk {
						request_id: Some(RequestId(1)),
					})
					.await
					.unwrap();
			}
		}

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Draft-14/15 predate the NAMESPACE message, so a SUBSCRIBE_NAMESPACE is
	/// answered with one PUBLISH_NAMESPACE request per matching namespace over the
	/// control stream, and PUBLISH_NAMESPACE_DONE withdraws it. The state is local
	/// to the subscription's task, mirroring lite's announce handling.
	#[tokio::test]
	async fn v14_subscribe_namespace_is_answered_with_publish_namespace() {
		const VERSION: Version = Version::Draft14;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();

		// Announced before the peer subscribes: it must only hit the wire after.
		let early = origin
			.create_broadcast("early-cam", crate::broadcast::Route::new().with_announce(true))
			.unwrap();
		settle().await;

		// Stream 1 is the peer's SUBSCRIBE_NAMESPACE (the peer stays quiet after);
		// streams 2 and 3 answer our two PUBLISH_NAMESPACE requests.
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new(), ok.clone(), ok]);
		let log = session.log.clone();

		let publisher = Publisher::new(session.clone(), consumer, Control::new(None, false), None, VERSION);

		let stream = Stream::open(&session, VERSION).await.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::new(""),
		};
		let mut run = std::pin::pin!(publisher.run_subscribe_namespace_stream(stream, msg));

		// The subscription solicits the already-announced namespace.
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"early-cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"early-cam"),
			1,
			"PUBLISH_NAMESPACE after subscribing"
		);

		// A later announce reaches the same subscription.
		let _late = origin
			.create_broadcast("late-cam", crate::broadcast::Route::new().with_announce(true))
			.unwrap();
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"late-cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"late-cam"),
			1,
			"PUBLISH_NAMESPACE for a live announce"
		);

		// An unannounce closes out its own request with PUBLISH_NAMESPACE_DONE.
		drop(early);
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"early-cam") >= 2 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"early-cam"),
			2,
			"PUBLISH_NAMESPACE_DONE on unannounce"
		);

		// One stream for the subscription itself, one per PUBLISH_NAMESPACE: the
		// withdrawal rode the announce's own request, not a new stream.
		assert_eq!(log.bi_opens(), 3, "no extra stream for the withdrawal");
	}
}
