use crate::{
	BandwidthConsumer, BandwidthProducer, Error, OriginConsumer, OriginProducer, StatsHandle,
	coding::Stream,
	lite::SessionInfo,
	session::{GoawaySignal, goaway_triggered},
};

use super::{Connecting, ControlType, Goaway, Publisher, PublisherConfig, Subscriber, SubscriberConfig, Version};

/// Start a lite session.
///
/// Returns the receive-bandwidth consumer (if any) and a [`Connecting`] handle that
/// becomes ready once the initial announce set has been inserted into the subscribe
/// origin, letting `connect()` block past the startup race. It is ready immediately
/// when there is nothing to wait on (a version without an initial-set boundary).
pub fn start<S: web_transport_trait::Session>(
	session: S,
	// The stream used to set up the session, after exchanging setup messages.
	// NOTE: No longer used in draft-03.
	setup_stream: Option<Stream<S, Version>>,
	// We will publish any local broadcasts from this origin.
	publish: OriginConsumer,
	// We will consume any remote broadcasts, inserting them into this origin.
	subscribe: OriginProducer,
	// Tier-scoped stats handle. Pass [`StatsHandle::default`] to opt out.
	stats: StatsHandle,
	// The version of the protocol to use.
	version: Version,
	// Fires when the session should send a GOAWAY and start draining.
	goaway: GoawaySignal,
) -> Result<(Option<BandwidthConsumer>, Connecting), Error> {
	let recv_bw = BandwidthProducer::new();

	let recv_bw_consumer = match version {
		Version::Lite01 | Version::Lite02 => None,
		_ => Some(recv_bw.consume()),
	};

	let recv_bw_for_sub = match version {
		Version::Lite01 | Version::Lite02 => None,
		_ => Some(recv_bw),
	};

	// Connection-progress tracker. Only block on the initial set for versions with an
	// initial-set boundary (AnnounceInit for Lite01/02, AnnounceOk for Lite05). For other
	// versions we drop the producer here, which closes the channel and makes
	// `Connecting::ready` resolve immediately. An empty subscribe origin also resolves
	// immediately because the subscriber arms with a prefix count of zero.
	let (connecting_producer, connecting) = Connecting::new();
	let sub_connecting = if matches!(version, Version::Lite01 | Version::Lite02 | Version::Lite05Wip) {
		Some(connecting_producer)
	} else {
		None
	};

	// Publisher and Subscriber each derive their identity from their own
	// attached origin (publish.info / subscribe.info). This is what gets
	// stamped onto outbound hops and checked against incoming hops, so it
	// must be stable across every session that shares the local origin.
	// Required for cross-session cluster loop detection.
	let publisher = Publisher::new(PublisherConfig {
		session: session.clone(),
		origin: publish,
		stats: stats.clone(),
		version,
	});
	let subscriber = Subscriber::new(SubscriberConfig {
		session: session.clone(),
		origin: subscribe,
		recv_bandwidth: recv_bw_for_sub,
		stats,
		version,
	});

	// GOAWAY is moq-lite-04+. On older drafts we simply never send it; the relay
	// still drains by waiting for the peer to leave (or a forced shutdown).
	if !matches!(version, Version::Lite01 | Version::Lite02 | Version::Lite03) {
		let session = session.clone();
		web_async::spawn(async move {
			tokio::select! {
				// Don't outlive the session: stop waiting once it's gone.
				_ = session.closed() => {}
				uri = goaway_triggered(goaway) => {
					if let Some(uri) = uri
						&& let Err(err) = send_goaway(&session, &uri, version).await
					{
						tracing::debug!(%err, "failed to send goaway");
					}
				}
			}
		});
	}

	web_async::spawn(async move {
		let res = tokio::select! {
			Err(res) = run_session(setup_stream) => Err(res),
			res = publisher.run() => res,
			res = subscriber.run(sub_connecting) => res,
		};

		match res {
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

	Ok((recv_bw_consumer, connecting))
}

/// Open a dedicated control stream and write a single GOAWAY message.
async fn send_goaway<S: web_transport_trait::Session>(session: &S, uri: &str, version: Version) -> Result<(), Error> {
	let mut stream = Stream::open(session, version).await?;
	stream.writer.encode(&ControlType::Goaway).await?;
	stream.writer.encode(&Goaway { uri: uri.into() }).await?;
	stream.writer.finish()?;
	stream.writer.closed().await
}

// TODO do something useful with this
async fn run_session<S: web_transport_trait::Session>(stream: Option<Stream<S, Version>>) -> Result<(), Error> {
	if let Some(mut stream) = stream {
		while let Some(_info) = stream.reader.decode_maybe::<SessionInfo>().await? {}
		return Err(Error::Cancel);
	}

	Ok(())
}
