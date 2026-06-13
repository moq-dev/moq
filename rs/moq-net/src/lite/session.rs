use crate::{
	BandwidthConsumer, BandwidthProducer, Error, OriginConsumer, OriginProducer, StatsHandle, coding::Stream,
	lite::SessionInfo,
};

use super::{Connecting, Publisher, PublisherConfig, Subscriber, SubscriberConfig, Version};

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
	// We will publish any local broadcasts from this origin, when set.
	publish: Option<OriginConsumer>,
	// We will consume any remote broadcasts, inserting them into this origin, when set.
	subscribe: Option<OriginProducer>,
	// Tier-scoped stats handle. Pass [`StatsHandle::default`] to opt out.
	stats: StatsHandle,
	// The version of the protocol to use.
	version: Version,
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
	//
	// A `None` half is left idle: an unset publish origin announces nothing,
	// and an unset subscribe origin never issues ANNOUNCE_PLEASE.
	let publisher = publish.map(|origin| {
		Publisher::new(PublisherConfig {
			session: session.clone(),
			origin,
			stats: stats.clone(),
			version,
		})
	});
	let subscriber = subscribe.map(|origin| {
		Subscriber::new(SubscriberConfig {
			session: session.clone(),
			origin,
			recv_bandwidth: recv_bw_for_sub,
			stats,
			version,
		})
	});

	web_async::spawn(async move {
		let run_publisher = async {
			match publisher {
				Some(p) => p.run().await,
				None => std::future::pending::<Result<(), Error>>().await,
			}
		};
		let run_subscriber = async {
			match subscriber {
				Some(s) => s.run(sub_connecting).await,
				// Drop the connecting producer so `Connecting::ready` unblocks `connect()`.
				None => {
					drop(sub_connecting);
					std::future::pending::<Result<(), Error>>().await
				}
			}
		};

		let res = tokio::select! {
			Err(res) = run_session(setup_stream) => Err(res),
			res = run_publisher => res,
			res = run_subscriber => res,
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

// TODO do something useful with this
async fn run_session<S: web_transport_trait::Session>(stream: Option<Stream<S, Version>>) -> Result<(), Error> {
	if let Some(mut stream) = stream {
		while let Some(_info) = stream.reader.decode_maybe::<SessionInfo>().await? {}
		return Err(Error::Cancel);
	}

	Ok(())
}
