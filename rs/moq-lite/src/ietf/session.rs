use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::{Reader, Stream},
	ietf::{self, Control, Message, RequestId, Version},
};

use super::{Publisher, Subscriber};

pub(crate) async fn start<S: web_transport_trait::Session>(
	session: S,
	setup: Stream<S, Version>,
	request_id_max: RequestId,
	client: bool,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	version: Version,
) -> Result<(), Error> {
	web_async::spawn(async move {
		match run(
			session.clone(),
			setup,
			request_id_max,
			client,
			publish,
			subscribe,
			version,
		)
		.await
		{
			Err(Error::Transport(_)) => {
				tracing::info!("session terminated");
				session.close(1, "");
			}
			Err(err) => {
				tracing::warn!(%err, "session error");
				session.close(err.to_code(), err.to_string().as_ref());
			}
			_ => {
				tracing::info!("session closed");
				session.close(0, "");
			}
		}
	});

	Ok(())
}

async fn run<S: web_transport_trait::Session>(
	session: S,
	setup: Stream<S, Version>,
	request_id_max: RequestId,
	client: bool,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	version: Version,
) -> Result<(), Error> {
	let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
	let control = Control::new(tx, request_id_max, client, version);
	let publisher = Publisher::new(session.clone(), publish, control.clone(), version);
	let subscriber = Subscriber::new(session.clone(), subscribe, control.clone(), version);

	tokio::select! {
		res = subscriber.clone().run() => res,
		res = publisher.clone().run() => res,
		res = run_control_read(setup.reader, control, publisher, subscriber, version) => res,
		res = Control::run::<S>(setup.writer, rx) => res,
	}
}

async fn run_control_read<S: web_transport_trait::Session>(
	mut reader: Reader<S::RecvStream, Version>,
	control: Control,
	mut publisher: Publisher<S>,
	mut subscriber: Subscriber<S>,
	version: Version,
) -> Result<(), Error> {
	loop {
		let id: u64 = match reader.decode_maybe().await? {
			Some(id) => id,
			None => return Ok(()),
		};

		let size: u16 = reader.decode::<u16>().await?;
		tracing::trace!(id, size, "reading control message");

		let mut data = reader.read_exact(size as usize).await?;
		tracing::trace!(hex = %hex::encode(&data), "decoding control message");

		match id {
			ietf::Subscribe::ID => {
				let msg = ietf::Subscribe::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_subscribe(msg)?;
			}
			ietf::SubscribeUpdate::ID => {
				let msg = ietf::SubscribeUpdate::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_subscribe_update(msg)?;
			}
			ietf::SubscribeOk::ID => {
				let msg = ietf::SubscribeOk::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				subscriber.recv_subscribe_ok(msg)?;
			}
			// 0x05: SubscribeError in v14, REQUEST_ERROR in v15
			ietf::SubscribeError::ID => match version {
				Version::Draft14 => {
					let msg = ietf::SubscribeError::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					subscriber.recv_subscribe_error(msg)?;
				}
				Version::Draft15 => {
					let msg = ietf::RequestError::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					subscriber.recv_request_error(&msg)?;
					publisher.recv_request_error(&msg)?;
				}
			},
			ietf::PublishNamespace::ID => {
				let msg = ietf::PublishNamespace::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				subscriber.recv_publish_namespace(msg)?;
			}
			// 0x07: PublishNamespaceOk in v14, REQUEST_OK in v15
			ietf::PublishNamespaceOk::ID => match version {
				Version::Draft14 => {
					let msg = ietf::PublishNamespaceOk::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					publisher.recv_publish_namespace_ok(msg)?;
				}
				Version::Draft15 => {
					let msg = ietf::RequestOk::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					subscriber.recv_request_ok(&msg)?;
					publisher.recv_request_ok(&msg)?;
				}
			},
			// 0x08: PublishNamespaceError in v14, removed in v15 (replaced by RequestError 0x05)
			ietf::PublishNamespaceError::ID => match version {
				Version::Draft14 => {
					let msg = ietf::PublishNamespaceError::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					publisher.recv_publish_namespace_error(msg)?;
				}
				Version::Draft15 => return Err(Error::UnexpectedMessage),
			},
			ietf::PublishNamespaceDone::ID => {
				let msg = ietf::PublishNamespaceDone::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				subscriber.recv_publish_namespace_done(msg)?;
			}
			ietf::Unsubscribe::ID => {
				let msg = ietf::Unsubscribe::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_unsubscribe(msg)?;
			}
			ietf::PublishDone::ID => {
				let msg = ietf::PublishDone::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				subscriber.recv_publish_done(msg)?;
			}
			ietf::PublishNamespaceCancel::ID => {
				let msg = ietf::PublishNamespaceCancel::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_publish_namespace_cancel(msg)?;
			}
			ietf::TrackStatus::ID => {
				let msg = ietf::TrackStatus::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_track_status(msg)?;
			}
			ietf::GoAway::ID => {
				let msg = ietf::GoAway::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				return Err(Error::Unsupported);
			}
			ietf::SubscribeNamespace::ID => {
				let msg = ietf::SubscribeNamespace::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_subscribe_namespace(msg)?;
			}
			// 0x12: SubscribeNamespaceOk in v14, removed in v15 (replaced by RequestOk 0x07)
			ietf::SubscribeNamespaceOk::ID => match version {
				Version::Draft14 => {
					let msg = ietf::SubscribeNamespaceOk::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					subscriber.recv_subscribe_namespace_ok(msg)?;
				}
				Version::Draft15 => return Err(Error::UnexpectedMessage),
			},
			// 0x13: SubscribeNamespaceError in v14, removed in v15 (replaced by RequestError 0x05)
			ietf::SubscribeNamespaceError::ID => match version {
				Version::Draft14 => {
					let msg = ietf::SubscribeNamespaceError::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					subscriber.recv_subscribe_namespace_error(msg)?;
				}
				Version::Draft15 => return Err(Error::UnexpectedMessage),
			},
			ietf::UnsubscribeNamespace::ID => {
				let msg = ietf::UnsubscribeNamespace::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_unsubscribe_namespace(msg)?;
			}
			ietf::MaxRequestId::ID => {
				let msg = ietf::MaxRequestId::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				control.max_request_id(msg.request_id);
			}
			ietf::RequestsBlocked::ID => {
				let msg = ietf::RequestsBlocked::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				tracing::warn!(?msg, "ignoring requests blocked");
			}
			ietf::Fetch::ID => {
				let msg = ietf::Fetch::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_fetch(msg)?;
			}
			ietf::FetchCancel::ID => {
				let msg = ietf::FetchCancel::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				publisher.recv_fetch_cancel(msg)?;
			}
			ietf::FetchOk::ID => {
				let msg = ietf::FetchOk::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				subscriber.recv_fetch_ok(msg)?;
			}
			// 0x19: FetchError in v14, removed in v15 (replaced by RequestError 0x05)
			ietf::FetchError::ID => match version {
				Version::Draft14 => {
					let msg = ietf::FetchError::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "received control message");
					subscriber.recv_fetch_error(msg)?;
				}
				Version::Draft15 => return Err(Error::UnexpectedMessage),
			},
			ietf::Publish::ID => {
				let msg = ietf::Publish::decode_msg(&mut data, version)?;
				tracing::debug!(message = ?msg, "received control message");
				subscriber.recv_publish(msg)?;
			}
			// 0x1E: PublishOk — v14: unsupported, v15: removed (replaced by RequestOk 0x07)
			ietf::PublishOk::ID => match version {
				Version::Draft14 | Version::Draft15 => return Err(Error::UnexpectedMessage),
			},
			// 0x1F: PublishError — v14: unsupported, v15: removed (replaced by RequestError 0x05)
			ietf::PublishError::ID => match version {
				Version::Draft14 | Version::Draft15 => return Err(Error::UnexpectedMessage),
			},
			_ => return Err(Error::UnexpectedMessage),
		}

		if !data.is_empty() {
			return Err(Error::WrongSize);
		}
	}
}
