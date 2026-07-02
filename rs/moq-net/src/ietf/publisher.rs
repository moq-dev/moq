use std::collections::HashMap;

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_async::FuturesExt;
use web_transport_trait::SendStream;

use crate::{
	AsPath, Error, OriginConsumer, StatsHandle, Subscription, TrackSubscriber,
	coding::{Stream, Writer},
	ietf::{self, Control, FetchHeader, FetchType, FilterType, GroupOrder, Location, RequestId},
	model::GroupConsumer,
};

use super::{Message, Version};

#[derive(Clone)]
pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: OriginConsumer,
	control: Control,
	stats: StatsHandle,
	/// Per-session egress broadcast-subscription tracker. Each downstream
	/// subscription holds a guard so `broadcasts - broadcasts_closed` counts
	/// the distinct sessions (viewers) watching each broadcast.
	broadcasts: crate::SessionBroadcasts,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(session: S, origin: OriginConsumer, control: Control, stats: StatsHandle, version: Version) -> Self {
		let broadcasts = stats.publisher_broadcasts();
		Self {
			session,
			origin,
			control,
			stats,
			broadcasts,
			version,
		}
	}

	pub async fn run(self) -> Result<(), Error> {
		self.run_announce().await
	}

	/// Handle an incoming bidi stream dispatched by the session.
	pub fn handle_stream(&self, id: u64, mut data: bytes::Bytes, stream: Stream<S, Version>) -> Result<(), Error> {
		let this = self.clone();
		match id {
			ietf::Subscribe::ID => {
				let msg = ietf::Subscribe::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received subscribe");
				web_async::spawn(async move {
					if let Err(err) = this.run_subscribe_stream(stream, msg).await {
						tracing::debug!(%err, "subscribe stream error");
					}
				});
			}
			ietf::Fetch::ID => {
				let msg = ietf::Fetch::decode_msg(&mut data, this.version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received fetch");
				web_async::spawn(async move {
					if let Err(err) = this.run_fetch_stream(stream, msg).await {
						tracing::debug!(%err, "fetch stream error");
					}
				});
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
				web_async::spawn(async move {
					if let Err(err) = this.run_subscribe_namespace_stream(stream, msg).await {
						tracing::debug!(%err, "subscribe_namespace stream error");
					}
				});
			}
			ietf::TrackStatus::ID => {
				tracing::warn!("TrackStatus not supported");
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type for publisher");
				return Err(Error::UnexpectedStream);
			}
		}
		Ok(())
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

		// Per-track subscription guard (bumps `subscriptions`). Taken before
		// validation so a stale/invalid SUBSCRIBE still counts as an attempt,
		// matching the lite path. The per-(session, broadcast) `broadcasts`
		// sentinel that counts viewers is taken only once the subscription is
		// validated and active, below.
		let track_stats = std::sync::Arc::new(self.stats.broadcast(&absolute).publisher_track(&track_name));

		// We just received a subscribe for this exact namespace, so the peer must have already
		// seen the announcement. `request_broadcast` resolves it immediately, or falls back to
		// an `OriginDynamic` handler if one is registered.
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

		// Subscription is now active: count this session as a viewer of the
		// broadcast. Dropping this guard (subscription end) releases it.
		let _broadcast_sub = self.broadcasts.subscribe(&absolute);

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
			})
			.await?;

		// Run the track, cancelling on reader close (Unsubscribe or stream close)
		let res = tokio::select! {
			res = self.run_track(track, request_id, track_stats) => res,
			_ = stream.reader.closed() => Ok(()),
			_ = self.session.closed() => Ok(()),
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
	async fn run_track(
		&self,
		mut track: TrackSubscriber,
		request_id: RequestId,
		track_stats: std::sync::Arc<crate::PublisherTrack>,
	) -> Result<(), Error> {
		let mut tasks = FuturesUnordered::new();

		loop {
			let group = tokio::select! {
				// Poll all active group futures; never matches but keeps them running.
				true = async {
					while tasks.next().await.is_some() {}
					false
				} => unreachable!(),
				Some(group) = track.recv_group().transpose() => group,
				else => return Ok(()),
			}?;

			let sequence = group.sequence;
			tracing::debug!(subscribe = %request_id, track = %track.name(), sequence, "serving group");

			let msg = ietf::GroupHeader {
				track_alias: request_id.0,
				group_id: sequence,
				sub_group_id: 0,
				publisher_priority: 0,
				// Carry per-object timestamps as extension headers (Timestamp/Timescale
				// Object Properties) so moq-transport peers get the real PTS.
				flags: ietf::GroupFlags {
					has_extensions: true,
					..Default::default()
				},
			};

			let priority = track.subscription().priority;
			tasks.push(
				Self::run_group(
					self.session.clone(),
					msg,
					priority,
					group,
					track_stats.clone(),
					self.version,
				)
				.map(|_| ()),
			);
		}
	}

	async fn run_group(
		session: S,
		msg: ietf::GroupHeader,
		priority: u8,
		mut group: GroupConsumer,
		track_stats: std::sync::Arc<crate::PublisherTrack>,
		version: Version,
	) -> Result<(), Error> {
		let mut stream = session.open_uni().await.map_err(Error::from_transport)?;
		stream.set_priority(priority);

		let mut stream = Writer::new(stream, version);

		stream.encode(&msg).await?;
		track_stats.group();

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

			// Per-object extension headers carry the frame's presentation timestamp.
			if msg.flags.has_extensions {
				let mut ext = bytes::BytesMut::new();
				ietf::encode_object_time(&mut ext, frame.timestamp, version)?;
				stream.encode(&(ext.len() as u64)).await?;
				let mut ext = ext.freeze();
				stream.write_all(&mut ext).await?;
			}

			// Write the size of the frame.
			stream.encode(&frame.size).await?;
			track_stats.frame();

			if frame.size == 0 {
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
							let n = chunk.len() as u64;
							stream.write_all(&mut chunk).await?;
							track_stats.bytes(n);
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

	/// Outgoing PublishNamespace: announce each namespace via a bidi stream.
	async fn run_announce(self) -> Result<(), Error> {
		// Each accepted namespace holds a `publisher()` announce guard (bumps
		// `announced` / `announced_closed`) alongside its stream, so dropping the
		// tuple on unannounce or cleanup records the close.
		let mut namespace_streams: HashMap<crate::PathOwned, (RequestId, Stream<S, Version>, crate::PublisherStats)> =
			HashMap::new();
		let mut announced = self.origin.announced();

		loop {
			let next = tokio::select! {
				biased;
				_ = self.session.closed() => return Ok(()),
				next = announced.next() => next,
			};

			let Some((path, event)) = next else {
				break;
			};

			let suffix = path.to_owned();

			match event {
				crate::Announced::Active(_) => {
					self.announce_namespace(suffix, &mut namespace_streams).await?;
				}
				crate::Announced::Restart(_) => {
					// moq-transport has no RESTART; fall back to done + re-announce.
					self.unannounce_namespace(&suffix, &mut namespace_streams).await;
					self.announce_namespace(suffix, &mut namespace_streams).await?;
				}
				crate::Announced::Ended => {
					self.unannounce_namespace(&suffix, &mut namespace_streams).await;
				}
			}
		}

		// Clean up remaining streams
		let suffixes: Vec<crate::PathOwned> = namespace_streams.keys().cloned().collect();
		for suffix in suffixes {
			self.unannounce_namespace(&suffix, &mut namespace_streams).await;
		}

		Ok(())
	}

	/// Open a bidi stream and send a PublishNamespace, recording the stream for later teardown.
	async fn announce_namespace(
		&self,
		suffix: crate::PathOwned,
		namespace_streams: &mut HashMap<crate::PathOwned, (RequestId, Stream<S, Version>, crate::PublisherStats)>,
	) -> Result<(), Error> {
		let absolute = self.origin.absolute(&suffix).to_owned();
		tracing::debug!(broadcast = %absolute, "announce");

		let request_id = self.control.next_request_id().await?;
		let mut stream = Stream::open(&self.session, self.version).await?;

		let bs = self.stats.broadcast(&absolute);

		stream.writer.encode(&ietf::PublishNamespace::ID).await?;
		stream
			.writer
			.encode(&ietf::PublishNamespace {
				request_id,
				track_namespace: suffix.as_path(),
			})
			.await?;
		// Count the broadcast name length (not the encoded message size) as soon
		// as the request is on the wire, so a rejected namespace still counts the
		// announce we spent.
		bs.publisher_announced_bytes(absolute.as_str().len() as u64);

		let type_id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let mut data = stream.reader.read_exact(size as usize).await?;

		match (self.version, type_id) {
			(Version::Draft14, ietf::PublishNamespaceOk::ID) => {
				let msg = ietf::PublishNamespaceOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
				// Holds the announce guard (bumps `announced` / `announced_closed`)
				// until the namespace stream is torn down.
				namespace_streams.insert(suffix, (request_id, stream, bs.publisher()));
			}
			(Version::Draft14, ietf::PublishNamespaceError::ID) => {
				let msg = ietf::PublishNamespaceError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
			}
			(_, ietf::RequestOk::ID) => {
				let msg = ietf::RequestOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "publish namespace ok");
				namespace_streams.insert(suffix, (request_id, stream, bs.publisher()));
			}
			(_, ietf::RequestError::ID) => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "publish namespace error");
			}
			_ => return Err(Error::UnexpectedMessage),
		}

		Ok(())
	}

	/// Tear down the namespace stream for a suffix, sending PublishNamespaceDone where required.
	async fn unannounce_namespace(
		&self,
		suffix: &crate::PathOwned,
		namespace_streams: &mut HashMap<crate::PathOwned, (RequestId, Stream<S, Version>, crate::PublisherStats)>,
	) {
		tracing::debug!(broadcast = %self.origin.absolute(suffix), "unannounce");
		// Dropping `_stats` on removal records the announce close.
		if let Some((request_id, mut stream, _stats)) = namespace_streams.remove(suffix) {
			// v14-16 sends PublishNamespaceDone; v17+ just closes the stream.
			match self.version {
				Version::Draft14 | Version::Draft15 | Version::Draft16 => {
					let _ = stream
						.writer
						.encode_message(&ietf::PublishNamespaceDone {
							track_namespace: suffix.as_path(),
							request_id,
						})
						.await;
				}
				_ => {}
			}
			// Count the unannounce name length, mirroring the announce above (we
			// measure the name, not the on-wire framing, so this is draft-agnostic).
			let absolute = self.origin.absolute(suffix).to_owned();
			self.stats
				.broadcast(&absolute)
				.publisher_announced_bytes(absolute.as_str().len() as u64);
			stream.writer.finish().ok();
		}
	}

	/// Handle a SUBSCRIBE_NAMESPACE on its bidi stream.
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

		match self.version {
			// v14/v15: Namespace/NamespaceDone don't exist. After OK, the publisher
			// sends PUBLISH_NAMESPACE/PUBLISH_NAMESPACE_DONE as separate control
			// stream messages (handled by run_announce). Just wait for stream close.
			Version::Draft14 | Version::Draft15 => {
				return stream.reader.closed().await;
			}
			// v16+: Send Namespace/NamespaceDone entries on this bidi stream.
			_ => {
				let mut announced = origin.announced();

				// Send initial NAMESPACE messages for currently active namespaces
				// Send initial NAMESPACE messages for currently active namespaces.
				// A restart is indistinguishable from an active here (the namespace is present).
				while let Some((path, event)) = announced.try_next() {
					if event.broadcast().is_some() {
						let suffix = path
							.strip_prefix(&prefix)
							.expect("origin returned invalid path")
							.to_owned();
						tracing::debug!(broadcast = %origin.absolute(&path), "namespace");
						stream.writer.encode(&ietf::Namespace::ID).await?;
						stream.writer.encode(&ietf::Namespace { suffix }).await?;
					}
				}

				// Stream updates
				loop {
					tokio::select! {
						biased;
						res = stream.reader.closed() => return res,
					next = announced.next() => {
						let Some((path, event)) = next else {
							stream.writer.finish()?;
							return stream.writer.closed().await;
						};

						let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path").to_owned();
						let absolute = origin.absolute(&path).to_owned();

						match event {
							crate::Announced::Active(_) => {
								tracing::debug!(broadcast = %absolute, "namespace");
								stream.writer.encode(&ietf::Namespace::ID).await?;
								stream.writer.encode(&ietf::Namespace { suffix }).await?;
							}
							crate::Announced::Restart(_) => {
								// moq-transport has no RESTART; fall back to namespace_done + namespace.
								tracing::debug!(broadcast = %absolute, "namespace_done");
								stream.writer.encode(&ietf::NamespaceDone::ID).await?;
								stream.writer.encode(&ietf::NamespaceDone { suffix: suffix.clone() }).await?;
								tracing::debug!(broadcast = %absolute, "namespace");
								stream.writer.encode(&ietf::Namespace::ID).await?;
								stream.writer.encode(&ietf::Namespace { suffix }).await?;
							}
							crate::Announced::Ended => {
								tracing::debug!(broadcast = %absolute, "namespace_done");
								stream.writer.encode(&ietf::NamespaceDone::ID).await?;
								stream.writer.encode(&ietf::NamespaceDone { suffix }).await?;
							}
						}
					}
					}
				}
			}
		}
	}
}
