use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::Stream,
	lite::{SessionInfo, Version},
};

use super::{Publisher, Subscriber};

pub(crate) fn start<S: web_transport_trait::Session>(
	session: S,
	// The stream used to setup the session, after exchanging setup messages.
	// NOTE: No longer used in draft-03.
	setup: Option<Stream<S, Version>>,
	// We will publish any local broadcasts from this origin.
	publish: Option<OriginConsumer>,
	// We will consume any remote broadcasts, inserting them into this origin.
	subscribe: Option<OriginProducer>,
	// The version of the protocol to use.
	version: Version,
) -> Result<(), Error> {
	let publisher = Publisher::new(session.clone(), publish, version);
	let subscriber = Subscriber::new(session.clone(), subscribe, version);

	web_async::spawn(async move {
		let res = tokio::select! {
			Err(res) = run_session(setup) => Err(res),
			res = publisher.run() => res,
			res = subscriber.run() => res,
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

// TODO do something useful with this
async fn run_session<S: web_transport_trait::Session>(stream: Option<Stream<S, Version>>) -> Result<(), Error> {
	if let Some(mut stream) = stream {
		while let Some(_info) = stream.reader.decode_maybe::<SessionInfo>().await? {}
		return Err(Error::Cancel);
	}

	Ok(())
}
