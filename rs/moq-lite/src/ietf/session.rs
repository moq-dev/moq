use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::{Encode, Reader, Stream, Writer},
	ietf::{self, RequestId},
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
				run_adapted(
					session.clone(),
					setup.expect("setup stream required for v14-16"),
					request_id_max,
					client,
					publish,
					subscribe,
					version,
				)
				.await
			}
			Version::Draft17 => run_native(session.clone(), client, publish, subscribe).await,
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

/// v14-16: Use the ControlStreamAdapter to multiplex control messages into virtual bidi streams.
async fn run_adapted<S: web_transport_trait::Session>(
	session: S,
	setup: Stream<S, Version>,
	request_id_max: Option<RequestId>,
	client: bool,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	version: Version,
) -> Result<(), Error> {
	let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
	let control = Control::new(request_id_max, client);
	let adapter = ControlStreamAdapter::new(session, tx, control.clone(), version);

	let publisher = Publisher::new(adapter.clone(), publish, control.clone(), version);
	let subscriber = Subscriber::new(adapter.clone(), subscribe, control, version);

	let dispatch_session = adapter.clone();
	let mut sub_ns = subscriber.clone();
	let sub_ns_adapter = adapter.clone();

	tokio::select! {
		res = adapter.run(setup.reader, setup.writer, rx) => res,
		res = run_dispatch(dispatch_session, publisher.clone(), subscriber.clone(), version) => res,
		res = publisher.run() => res,
		res = subscriber.run() => res,
		res = async {
			if !sub_ns.has_origin() {
				std::future::pending::<Result<(), Error>>().await
			} else {
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
			}
		} => res,
	}
}

/// v17: Use real bidi streams directly. SETUP is exchanged in the background on uni streams.
async fn run_native<S: web_transport_trait::Session>(
	session: S,
	client: bool,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
) -> Result<(), Error> {
	let version = Version::Draft17;
	let control = Control::new(None, client);
	let publisher = Publisher::new(session.clone(), publish, control.clone(), version);
	let subscriber = Subscriber::new(session.clone(), subscribe, control, version);

	// Spawn the SETUP sender independently — it must outlive the select
	// since the stream doubles as the GOAWAY channel.
	web_async::spawn({
		let session = session.clone();
		async move {
			if let Err(err) = run_setup(session, client).await {
				tracing::warn!(%err, "setup send error");
			}
		}
	});

	let sub_ns_session = session.clone();
	let mut sub_ns = subscriber.clone();

	tokio::select! {
		res = run_recv(session.clone(), subscriber.clone(), client) => res,
		res = run_dispatch(session, publisher.clone(), subscriber.clone(), version) => res,
		res = publisher.run() => res,
		res = async {
			if !sub_ns.has_origin() {
				std::future::pending::<Result<(), Error>>().await
			} else {
				let stream = Stream::open(&sub_ns_session, version).await?;
				sub_ns.run_subscribe_namespace(stream).await
			}
		} => res,
	}
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

/// Accept incoming uni streams for v17.
///
/// Dispatches the SETUP stream (0x2F00) to GOAWAY handling,
/// and all other uni streams to the subscriber for group data.
async fn run_recv<S: web_transport_trait::Session>(
	session: S,
	subscriber: Subscriber<S>,
	client: bool,
) -> Result<(), Error> {
	let version = Version::Draft17;
	let outer_version = crate::Version::Ietf(version);

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader = Reader::new(recv, outer_version);
		let kind: u64 = reader.decode_peek().await?;

		if kind == setup::SETUP_V17 {
			// Read the peer's SETUP message.
			if client {
				let _server: setup::Server = reader.decode().await?;
			} else {
				let _client: setup::Client = reader.decode().await?;
			}

			// Hand off remaining uni stream acceptance to the subscriber.
			let sub = subscriber;
			web_async::spawn(async move {
				if let Err(err) = sub.run().await {
					tracing::warn!(%err, "subscriber uni-stream handler failed");
				}
			});

			// This stream is now the GOAWAY channel — block until GOAWAY or close.
			return run_goaway(reader.with_version(version)).await;
		}

		// Non-SETUP uni stream: dispatch to subscriber for group data.
		let mut sub = subscriber.clone();
		web_async::spawn(async move {
			let mut reader = reader.with_version(version);
			if let Err(err) = sub.recv_group(&mut reader).await {
				reader.abort(&err);
			}
		});
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

/// Read the control/SETUP stream for v17 — only GOAWAY is expected.
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
