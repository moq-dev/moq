use tokio::sync::oneshot;

use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::Stream,
	lite::{SessionInfo, Version},
};

use super::{Publisher, Subscriber};

pub(crate) async fn start<S: web_transport_trait::Session>(
	session: S,
	// The stream used to setup the session, after exchanging setup messages.
	setup: Stream<S, Version>,
	// We will publish any local broadcasts from this origin.
	publish: Option<OriginConsumer>,
	// We will consume any remote broadcasts, inserting them into this origin.
	subscribe: Option<OriginProducer>,
	// The version of the protocol to use.
	version: Version,
) -> Result<(), Error> {
	let publisher = Publisher::new(session.clone(), publish, version);
	let subscriber = Subscriber::new(session.clone(), subscribe, version);

	let init = oneshot::channel();

	web_async::spawn(async move {
		let res = tokio::select! {
			res = run_session(setup) => res,
			res = publisher.run() => res,
			res = subscriber.run(init.0) => res,
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

	// Wait until receiving the initial announcements to prevent some race conditions.
	// Otherwise, `consume()` might return not found if we don't wait long enough, so just wait.
	// If the announce stream fails or is closed, this will return an error instead of hanging.
	// TODO return a better error
	init.1.await.map_err(|_| Error::Cancel)?;

	Ok(())
}

// TODO do something useful with this
async fn run_session<S: web_transport_trait::Session>(mut stream: Stream<S, Version>) -> Result<(), Error> {
	while let Some(_info) = stream.reader.decode_maybe::<SessionInfo>().await? {}
	Err(Error::Cancel)
}
