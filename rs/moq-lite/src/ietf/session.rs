use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::{Reader, Stream},
	ietf::{self, RequestId},
};

use super::{Control, Message, Publisher, Subscriber, Version, adapter::ControlStreamAdapter};

pub fn start<S: web_transport_trait::Session>(
	session: S,
	setup: Stream<S, Version>,
	request_id_max: Option<RequestId>,
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

async fn run<S: web_transport_trait::Session>(
	session: S,
	setup: Stream<S, Version>,
	request_id_max: Option<RequestId>,
	client: bool,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	version: Version,
) -> Result<(), Error> {
	match version {
		Version::Draft14 | Version::Draft15 | Version::Draft16 => {
			run_adapted(session, setup, request_id_max, client, publish, subscribe, version).await
		}
		Version::Draft17 => run_native(session, setup, client, publish, subscribe, version).await,
	}
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
				// No origin, nothing to subscribe to — just wait forever.
				std::future::pending::<Result<(), Error>>().await
			} else {
				// v16: SubscribeNamespace on its own real bidi stream
				// v14/v15: SubscribeNamespace on virtual control stream
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

/// v17: Use real bidi streams directly. Control stream only for GOAWAY.
async fn run_native<S: web_transport_trait::Session>(
	session: S,
	setup: Stream<S, Version>,
	client: bool,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	version: Version,
) -> Result<(), Error> {
	let control = Control::new(None, client);
	let publisher = Publisher::new(session.clone(), publish, control.clone(), version);
	let subscriber = Subscriber::new(session.clone(), subscribe, control, version);

	let sub_ns_session = session.clone();
	let mut sub_ns = subscriber.clone();

	tokio::select! {
		res = run_goaway(setup.reader) => res,
		res = run_dispatch(session, publisher.clone(), subscriber.clone(), version) => res,
		res = publisher.run() => res,
		res = subscriber.run() => res,
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
