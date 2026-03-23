use std::collections::HashMap;

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_async::FuturesExt;
use web_transport_trait::SendStream;

use crate::{
	AsPath, Error, Origin, OriginConsumer, Track, TrackConsumer,
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
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(session: S, origin: Option<OriginConsumer>, control: Control, version: Version) -> Self {
		let origin = origin.unwrap_or_else(|| Origin::produce().consume());
		Self {
			session,
			origin,
			control,
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
			ietf::SubscribeNamespace::ID => {
				let msg = ietf::SubscribeNamespace::decode_msg(&mut data, this.version)?;
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

		let Some(broadcast) = self.origin.consume_broadcast(&msg.track_namespace) else {
			self.write_subscribe_error(&mut stream.writer, request_id, 404, "Broadcast not found")
				.await?;
			return Ok(());
		};

		let track = Track {
			name: msg.track_name.to_string(),
			priority: msg.subscriber_priority,
		};

		let track = match broadcast.subscribe_track(&track) {
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
					Version::Draft17 => None,
					_ => Some(request_id),
				},
				track_alias: request_id.0,
			})
			.await?;

		// Run the track, cancelling on reader close (Unsubscribe or stream close)
		let res = tokio::select! {
			res = self.run_track(track, request_id) => res,
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
					Version::Draft17 => None,
					_ => Some(request_id),
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
			Version::Draft17 => {
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
	async fn run_track(&self, mut track: TrackConsumer, request_id: RequestId) -> Result<(), Error> {
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

			let sequence = group.info.sequence;
			tracing::debug!(subscribe = %request_id, track = %track.info.name, sequence, "serving group");

			let msg = ietf::GroupHeader {
				track_alias: request_id.0,
				group_id: sequence,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			};

			tasks
				.push(Self::run_group(self.session.clone(), msg, track.info.priority, group, self.version).map(|_| ()));
		}
	}

	async fn run_group(
		session: S,
		msg: ietf::GroupHeader,
		priority: u8,
		mut group: GroupConsumer,
		version: Version,
	) -> Result<(), Error> {
		let mut stream = session.open_uni().await.map_err(Error::from_transport)?;
		stream.set_priority(priority);

		let mut stream = Writer::new(stream, version);

		stream.encode(&msg).await?;

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
			Version::Draft17 => {
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
			Version::Draft17 => {
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
	async fn run_announce(mut self) -> Result<(), Error> {
		let mut namespace_streams: HashMap<crate::PathOwned, (RequestId, Stream<S, Version>)> = HashMap::new();

		loop {
			let announced = tokio::select! {
				biased;
				_ = self.session.closed() => return Ok(()),
				announced = self.origin.announced() => announced,
			};

			let Some((path, active)) = announced else {
				break;
			};

			let suffix = path.to_owned();

			if active.is_some() {
				tracing::debug!(broadcast = %self.origin.absolute(&path), "announce");

				let request_id = self.control.next_request_id().await?;
				let mut stream = Stream::open(&self.session, self.version).await?;

				// Write the PublishNamespace message
				stream.writer.encode(&ietf::PublishNamespace::ID).await?;
				stream
					.writer
					.encode(&ietf::PublishNamespace {
						request_id,
						track_namespace: suffix.as_path(),
					})
					.await?;

				// Read response from stream.reader
				let type_id: u64 = stream.reader.decode().await?;
				let size: u16 = stream.reader.decode().await?;
				let mut data = stream.reader.read_exact(size as usize).await?;

				match (self.version, type_id) {
					// Draft14 uses PublishNamespaceOk (0x07) / PublishNamespaceError (0x08)
					(Version::Draft14, ietf::PublishNamespaceOk::ID) => {
						let msg = ietf::PublishNamespaceOk::decode_msg(&mut data, self.version)?;
						tracing::debug!(message = ?msg, "publish namespace ok");
						namespace_streams.insert(suffix, (request_id, stream));
					}
					(Version::Draft14, ietf::PublishNamespaceError::ID) => {
						let msg = ietf::PublishNamespaceError::decode_msg(&mut data, self.version)?;
						tracing::warn!(message = ?msg, "publish namespace error");
					}
					// Draft15+ uses RequestOk (0x07) / RequestError (0x05)
					(_, ietf::RequestOk::ID) => {
						let msg = ietf::RequestOk::decode_msg(&mut data, self.version)?;
						tracing::debug!(message = ?msg, "publish namespace ok");
						namespace_streams.insert(suffix, (request_id, stream));
					}
					(_, ietf::RequestError::ID) => {
						let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
						tracing::warn!(message = ?msg, "publish namespace error");
					}
					_ => return Err(Error::UnexpectedMessage),
				}
			} else {
				tracing::debug!(broadcast = %self.origin.absolute(&path), "unannounce");
				if let Some((request_id, mut stream)) = namespace_streams.remove(&suffix) {
					// For v14-16, send PublishNamespaceDone. For v17, just close the stream.
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
						Version::Draft17 => {}
					}
					stream.writer.finish().ok();
				}
			}
		}

		// Clean up remaining streams
		for (suffix, (request_id, mut stream)) in namespace_streams {
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
				Version::Draft17 => {}
			}
			stream.writer.finish().ok();
		}

		Ok(())
	}

	/// Handle a SUBSCRIBE_NAMESPACE on its bidi stream.
	async fn run_subscribe_namespace_stream(
		self,
		mut stream: Stream<S, Version>,
		msg: ietf::SubscribeNamespace<'_>,
	) -> Result<(), Error> {
		let prefix = msg.namespace.to_owned();

		tracing::debug!(prefix = %self.origin.absolute(&prefix), "subscribe_namespace stream");

		let mut origin = self
			.origin
			.consume_only(&[prefix.as_path()])
			.ok_or(Error::Unauthorized)?;

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
			Version::Draft17 => {
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
	}
}
