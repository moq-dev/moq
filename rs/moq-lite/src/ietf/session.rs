use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::{Reader, Stream},
	ietf::{self, RequestId},
};

use super::{Message, Publisher, Subscriber, Version, adapter::ControlStreamAdapter};

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
		Version::Draft17 => run_native(session, setup, publish, subscribe, version).await,
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
	let adapter = ControlStreamAdapter::new(session, tx, client, request_id_max, version);

	let publisher = Publisher::new(adapter.clone(), publish, version);
	let subscriber = Subscriber::new(adapter.clone(), subscribe, version);

	tokio::select! {
		res = adapter.run(setup.reader, setup.writer, rx) => res,
		res = publisher.run() => res,
		res = subscriber.run() => res,
	}
}

/// v17: Use real bidi streams directly. Control stream only for GOAWAY.
async fn run_native<S: web_transport_trait::Session>(
	session: S,
	setup: Stream<S, Version>,
	publish: Option<OriginConsumer>,
	subscribe: Option<OriginProducer>,
	version: Version,
) -> Result<(), Error> {
	let publisher = Publisher::new(session.clone(), publish, version);
	let subscriber = Subscriber::new(session, subscribe, version);

	tokio::select! {
		res = run_goaway(setup.reader) => res,
		res = publisher.run() => res,
		res = subscriber.run() => res,
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
