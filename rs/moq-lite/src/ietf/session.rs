use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::{Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, GroupFlags, RequestId},
	setup,
};

use super::{Control, Message, Publisher, Subscriber, Version, adapter::ControlStreamAdapter};

pub fn start<S: web_transport_trait::Session>(
	session: S,
	setup: Option<Stream<S, Version>>,
	request_id_max: Option<RequestId>,
	client: bool,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	version: Version,
) -> Result<(), Error> {
	web_async::spawn(async move {
		let res = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let Some(setup) = setup else {
					return session.close(Error::ProtocolViolation.to_code(), "setup stream required");
				};
				let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
				let control = Control::new(request_id_max, client);
				let adapter = ControlStreamAdapter::new(session.clone(), tx, control.clone(), version);

				let publisher = Publisher::new(adapter.clone(), publish, control.clone(), version);
				let subscriber = Subscriber::new(adapter.clone(), subscribe, control, version);

				let dispatch_session = adapter.clone();
				let mut sub_ns = subscriber.clone();
				let sub_ns_adapter = adapter.clone();

				tokio::select! {
					Err(err) = adapter.run(setup.reader, setup.writer, rx) => Err::<(), Error>(err),
					Err(err) = run_unis(adapter.clone(), subscriber.clone(), version, client) => Err(err),
					Err(err) = run_dispatch(dispatch_session, publisher.clone(), subscriber.clone(), version) => Err(err),
					Err(err) = publisher.run() => Err(err),
					Err(err) = async {
						if !sub_ns.has_origin() {
							return Ok(());
						}
						let stream = match version {
							Version::Draft16 => {
								let (send, recv) = sub_ns_adapter.open_native_bi().await?;
								Stream {
									writer: crate::coding::Writer::new(send, version),
									reader: crate::coding::Reader::new(recv, version),
								}
							}
							_ => Stream::open(&sub_ns_adapter, version).await?,
						};
						sub_ns.run_subscribe_namespace(stream).await
					} => Err(err),
				}
			}
			_ => {
				// Spawn SETUP sender (keeps stream alive for GOAWAY).
				web_async::spawn({
					let session = session.clone();
					async move {
						if let Err(err) = run_setup(session, client).await {
							tracing::warn!(%err, "setup send error");
						}
					}
				});

				let control = Control::new(None, client);
				let publisher = Publisher::new(session.clone(), publish, control.clone(), version);
				let subscriber = Subscriber::new(session.clone(), subscribe, control, version);

				let sub_ns_session = session.clone();
				let mut sub_ns = subscriber.clone();

				tokio::select! {
					Err(err) = run_unis(session.clone(), subscriber.clone(), version, client) => Err(err),
					Err(err) = run_dispatch(session.clone(), publisher.clone(), subscriber.clone(), version) => Err(err),
					Err(err) = publisher.run() => Err(err),
					Err(err) = async {
						if !sub_ns.has_origin() {
							return Ok(());
						}
						let stream = Stream::open(&sub_ns_session, version).await?;
						sub_ns.run_subscribe_namespace(stream).await
					} => Err(err),
				}
			}
		};

		match res {
			Err(Error::Transport) => {
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

/// Send our SETUP on a uni stream and keep it alive for potential GOAWAY.
async fn run_setup<S: web_transport_trait::Session>(session: S, client: bool) -> Result<(), Error> {
	let version = Version::Draft17;
	let outer_version = crate::Version::Ietf(version);

	let send = session.open_uni().await.map_err(Error::from_transport)?;
	let mut writer: Writer<S::SendStream, crate::Version> = Writer::new(send, outer_version);

	let mut parameters = ietf::Parameters::default();
	parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
	let parameters = parameters.encode_bytes(version)?;

	if client {
		writer
			.encode(&setup::Client {
				versions: crate::coding::Versions::from([outer_version.into()]),
				parameters,
			})
			.await?;
	} else {
		writer
			.encode(&setup::Server {
				version: outer_version.into(),
				parameters,
			})
			.await?;
	}

	// Hold the writer alive until the session closes.
	session.closed().await;
	writer.finish().ok();

	Ok(())
}

/// Accept incoming uni streams and dispatch each to a handler.
///
/// For v17, this also handles the SETUP stream (0x2F00) and GOAWAY.
/// For v14-16, all uni streams are group data.
async fn run_unis<S: web_transport_trait::Session>(
	session: S,
	subscriber: Subscriber<S>,
	version: Version,
	client: bool,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> = Reader::new(recv, outer_version);
		let kind: u64 = reader.decode_peek().await?;

		// v17: SETUP arrives on a uni stream, then becomes the GOAWAY channel.
		if kind == setup::SETUP_V17 && version == Version::Draft17 {
			if client {
				let _server: setup::Server = reader.decode().await?;
			} else {
				let _client: setup::Client = reader.decode().await?;
			}

			// Monitor for GOAWAY in the background while we continue accepting unis.
			web_async::spawn(async move {
				if let Err(err) = run_goaway(reader.with_version(version)).await {
					tracing::warn!(%err, "goaway error");
				}
			});

			// Continue the loop to accept group data streams.
			continue;
		}

		// Group data — spawn a handler for each stream.
		let mut sub = subscriber.clone();
		web_async::spawn(async move {
			let mut reader = reader.with_version(version);
			if let Err(err) = run_uni_group(&mut sub, &mut reader).await {
				tracing::debug!(%err, "uni stream error");
				reader.abort(&err);
			}
		});
	}
}

async fn run_uni_group<S: web_transport_trait::Session>(
	subscriber: &mut Subscriber<S>,
	stream: &mut Reader<S::RecvStream, Version>,
) -> Result<(), Error> {
	let kind: u64 = stream.decode_peek().await?;

	match kind {
		GroupFlags::START..=GroupFlags::END | GroupFlags::START_NO_PRIORITY..=GroupFlags::END_NO_PRIORITY => {
			subscriber.recv_group(stream).await
		}
		FetchHeader::TYPE => Err(Error::Unsupported),
		_ => Err(Error::UnexpectedStream),
	}
}

/// Accept incoming bidi streams and dispatch to the correct handler based on message type.
async fn run_dispatch<S: web_transport_trait::Session>(
	session: S,
	publisher: Publisher<S>,
	mut subscriber: Subscriber<S>,
	version: Version,
) -> Result<(), Error> {
	loop {
		let mut stream = Stream::accept(&session, version).await?;

		let id: u64 = stream.reader.decode().await?;
		let size: u16 = stream.reader.decode().await?;
		let data = stream.reader.read_exact(size as usize).await?;

		match id {
			// Publisher handles: Subscribe, Fetch, SubscribeNamespace, TrackStatus
			ietf::Subscribe::ID | ietf::Fetch::ID | ietf::SubscribeNamespace::ID | ietf::TrackStatus::ID => {
				publisher.handle_stream(id, data, stream)?;
			}
			// Subscriber handles: Publish, PublishNamespace
			ietf::Publish::ID | ietf::PublishNamespace::ID => {
				subscriber.handle_stream(id, data, stream)?;
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type");
				return Err(Error::UnexpectedStream);
			}
		}
	}
}

/// Block until GOAWAY or stream close.
async fn run_goaway<R: web_transport_trait::RecvStream>(mut reader: Reader<R, Version>) -> Result<(), Error> {
	let id: u64 = match reader.decode_maybe().await? {
		Some(id) => id,
		None => return Ok(()),
	};

	let size: u16 = reader.decode::<u16>().await?;
	let mut data = reader.read_exact(size as usize).await?;

	if id == ietf::GoAway::ID {
		let msg = ietf::GoAway::decode_msg(&mut data, Version::Draft17)?;
		tracing::debug!(message = ?msg, "received GOAWAY");
		Err(Error::Unsupported)
	} else {
		Err(Error::UnexpectedMessage)
	}
}
