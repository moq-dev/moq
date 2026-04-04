use std::time::Duration;

use web_transport_trait::Stats;

use crate::{BandwidthProducer, Error, OriginConsumer, OriginProducer, coding::Stream, lite::SessionInfo};

use super::{Publisher, Subscriber, Version};

/// Returned by `start()`: (send_bandwidth, recv_bandwidth)
pub type Bandwidth = (Option<BandwidthProducer>, Option<BandwidthProducer>);

pub fn start<S: web_transport_trait::Session>(
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
) -> Result<Bandwidth, Error> {
	let send_bw = BandwidthProducer::new();
	let send_bw_task = send_bw.clone();

	let recv_bw = BandwidthProducer::new();
	let (recv_bw_ret, recv_bw_for_sub) = match version {
		Version::Lite01 | Version::Lite02 => (None, None),
		_ => (Some(recv_bw.clone()), Some(recv_bw)),
	};

	let publisher = Publisher::new(session.clone(), publish, version);
	let subscriber = Subscriber::new(session.clone(), subscribe, recv_bw_for_sub, version);

	web_async::spawn(async move {
		let res = tokio::select! {
			Err(res) = run_session(setup) => Err(res),
			res = publisher.run() => res,
			res = subscriber.run() => res,
			_ = run_send_bandwidth(&session, send_bw_task) => Ok(()),
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

	Ok((Some(send_bw), recv_bw_ret))
}

/// Polls the QUIC congestion controller for estimated send rate.
/// Only active when at least one consumer exists.
async fn run_send_bandwidth<S: web_transport_trait::Session>(session: &S, producer: BandwidthProducer) {
	const POLL_INTERVAL: Duration = Duration::from_millis(100);

	loop {
		// Wait until someone cares about the send bandwidth.
		if producer.used().await.is_err() {
			return;
		}

		let mut interval = tokio::time::interval(POLL_INTERVAL);

		loop {
			tokio::select! {
				biased;
				res = producer.unused() => {
					if res.is_err() {
						return;
					}
					// No more consumers, pause polling.
					break;
				}
				_ = interval.tick() => {
					let bitrate = session.stats().estimated_send_rate();
					// Ignore errors — producer dropped means we're done.
					if producer.set(bitrate).is_err() {
						return;
					}
				}
			}
		}
	}
}

// TODO do something useful with this
async fn run_session<S: web_transport_trait::Session>(stream: Option<Stream<S, Version>>) -> Result<(), Error> {
	if let Some(mut stream) = stream {
		while let Some(_info) = stream.reader.decode_maybe::<SessionInfo>().await? {}
		return Err(Error::Cancel);
	}

	Ok(())
}
