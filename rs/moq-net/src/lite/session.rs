use tokio::sync::oneshot;

use crate::{
	BandwidthConsumer, BandwidthProducer, Error, OriginConsumer, OriginProducer, StatsHandle, coding::Stream,
	lite::SessionInfo,
};

use super::{Publisher, PublisherConfig, Subscriber, SubscriberConfig, SyncLatch, Version};

/// Start a lite session.
///
/// Returns the receive-bandwidth consumer (if any) and a `synced` receiver that
/// resolves once the initial announce set has been inserted into the subscribe
/// origin, letting `connect()` block past the startup race. The receiver fires
/// immediately when there is nothing to wait on (no subscribe origin, or a version
/// without an initial-set boundary).
pub fn start<S: web_transport_trait::Session>(
	session: S,
	// The stream used to setup the session, after exchanging setup messages.
	// NOTE: No longer used in draft-03.
	setup: Option<Stream<S, Version>>,
	// We will publish any local broadcasts from this origin.
	publish: OriginConsumer,
	// We will consume any remote broadcasts, inserting them into this origin.
	subscribe: OriginProducer,
	// Tier-scoped stats handle. Pass [`StatsHandle::default`] to opt out.
	stats: StatsHandle,
	// The version of the protocol to use.
	version: Version,
) -> Result<(Option<BandwidthConsumer>, oneshot::Receiver<()>), Error> {
	let recv_bw = BandwidthProducer::new();

	let recv_bw_consumer = match version {
		Version::Lite01 | Version::Lite02 => None,
		_ => Some(recv_bw.consume()),
	};

	let recv_bw_for_sub = match version {
		Version::Lite01 | Version::Lite02 => None,
		_ => Some(recv_bw),
	};

	// Connect-sync latch. Only block on the initial set for versions with an
	// initial-set boundary (AnnounceInit for Lite01/02, AnnounceOk for Lite05);
	// otherwise fire now so connect() doesn't block. An empty subscribe origin still
	// resolves immediately because the subscriber arms the latch with a prefix count
	// of zero.
	let (synced_latch, synced_rx) = SyncLatch::new();
	let sub_synced = if matches!(version, Version::Lite01 | Version::Lite02 | Version::Lite05Wip) {
		Some(synced_latch)
	} else {
		synced_latch.fire();
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
		synced: sub_synced,
		version,
	});

	web_async::spawn(async move {
		let res = tokio::select! {
			Err(res) = run_session(setup) => Err(res),
			res = publisher.run() => res,
			res = subscriber.run() => res,
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

	Ok((recv_bw_consumer, synced_rx))
}

// TODO do something useful with this
async fn run_session<S: web_transport_trait::Session>(stream: Option<Stream<S, Version>>) -> Result<(), Error> {
	if let Some(mut stream) = stream {
		while let Some(_info) = stream.reader.decode_maybe::<SessionInfo>().await? {}
		return Err(Error::Cancel);
	}

	Ok(())
}
