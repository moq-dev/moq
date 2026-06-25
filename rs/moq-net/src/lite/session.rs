use crate::{
	BandwidthConsumer, BandwidthProducer, Error, Origin, OriginConsumer, OriginProducer, StatsHandle,
	coding::{Reader, Stream, Writer},
	lite::SessionInfo,
};

use super::{
	Connecting, DataType, PeerSetup, Publisher, PublisherConfig, Setup, Subscriber, SubscriberConfig, Version,
};

/// Server: read the peer's single SETUP message off its Setup Stream before starting
/// the session, so the caller can inspect the advertised path (and gate on it) before
/// serving. lite-05+ only.
///
/// Blocks on the peer's Setup Stream, which every lite-05 endpoint opens at startup.
/// Almost always the first unidirectional stream; any other uni stream that races
/// ahead of it is `STOP_SENDING`-ed and skipped (we don't support proactive uni
/// PUBLISH, so nothing legitimate precedes the SETUP today). The eventual home for
/// out-of-order tolerance is the full session loop with deferred origin binding.
///
/// Pass the returned [`Setup`] to [`start`] as its `peer_setup` so PROBE gating still
/// resolves without re-reading the (consumed) stream.
pub async fn accept_setup<S: web_transport_trait::Session>(session: &S, version: Version) -> Result<Setup, Error> {
	loop {
		let stream = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader = Reader::new(stream, version);

		match reader.decode::<DataType>().await? {
			DataType::Setup => return reader.decode::<Setup>().await,
			// A non-SETUP uni stream this early is unexpected (GROUP needs a prior
			// subscribe). Reject it and keep waiting rather than failing the session.
			_ => reader.abort(&Error::UnexpectedStream),
		}
	}
}

/// Start a lite session.
///
/// Returns the receive-bandwidth consumer (if any) and a [`Connecting`] handle that
/// becomes ready once the initial announce set has been inserted into the subscribe
/// origin, letting `connect()` block past the startup race. It is ready immediately
/// when there is nothing to wait on (a version without an initial-set boundary).
// Internal entry point wiring a session together; the knobs are all distinct and
// positional clarity beats a one-off config struct here.
#[allow(clippy::too_many_arguments)]
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
	// The capabilities (and optional request path) we advertise in our SETUP message.
	// Only sent on versions with a Setup Stream (lite-05+); ignored otherwise.
	our_setup: Setup,
	// The peer's SETUP, when it was already read before `start` (e.g. a server that
	// gated on the client's path via [`accept_setup`]). Seeds the peer-setup slot so
	// the Setup Stream isn't expected again. `None` reads it from the wire as usual.
	peer_setup: Option<Setup>,
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

	// Always run both loops so inbound control (Subscribe/Announce/Probe/Goaway)
	// and GROUP streams are accepted regardless of which halves the caller wired.
	// An unset half gets an empty origin: an empty publish origin announces nothing
	// (and answers the peer's announce-interest with an empty set), and an empty
	// subscribe origin issues no ANNOUNCE_PLEASE (zero prefixes, so `run_announce`
	// drops `connecting` at once and `connect()` still unblocks).
	let publish = publish.unwrap_or_else(|| OriginProducer::empty(Origin::random()).consume());
	let subscribe = subscribe.unwrap_or_else(|| OriginProducer::empty(Origin::random()));

	// Publisher and Subscriber each derive their identity from their own
	// attached origin (publish.info / subscribe.info). This is what gets
	// stamped onto outbound hops and checked against incoming hops, so it
	// must be stable across every session that shares the local origin.
	// Required for cross-session cluster loop detection.
	// Shared slot for the peer's SETUP (lite-05+). The subscriber writes it when it
	// reads the peer's Setup stream; capability-gated streams (PROBE) wait on it.
	// When the caller already read it (a gated server accept), seed the slot so the
	// Setup stream isn't expected on the wire again.
	let peer_setup_slot = PeerSetup::default();
	if let Some(setup) = peer_setup {
		peer_setup_slot.set(setup);
	}
	let peer_setup = peer_setup_slot;

	// Advertise our own capabilities on a uni Setup Stream, then FIN. Best-effort:
	// a failure here just means the peer falls back to "no capabilities" for us.
	if version.has_setup_stream() {
		let session = session.clone();
		web_async::spawn(async move {
			if let Err(err) = send_setup(&session, our_setup, version).await {
				tracing::debug!(%err, "failed to send setup");
			}
		});
	}

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
		peer_setup,
	});

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

/// Open a unidirectional Setup Stream, send our single SETUP message, and FIN.
async fn send_setup<S: web_transport_trait::Session>(session: &S, setup: Setup, version: Version) -> Result<(), Error> {
	let stream = session.open_uni().await.map_err(Error::from_transport)?;
	let mut writer = Writer::new(stream, version);
	writer.encode(&super::DataType::Setup).await?;
	writer.encode(&setup).await?;
	writer.finish()?;
	writer.closed().await
}

// TODO do something useful with this
async fn run_session<S: web_transport_trait::Session>(stream: Option<Stream<S, Version>>) -> Result<(), Error> {
	if let Some(mut stream) = stream {
		while let Some(_info) = stream.reader.decode_maybe::<SessionInfo>().await? {}
		return Err(Error::Cancel);
	}

	Ok(())
}
