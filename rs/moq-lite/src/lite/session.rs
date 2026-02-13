use tokio::sync::oneshot;

use crate::{
	Error, OriginConsumer, OriginProducer,
	coding::{Stream, Writer},
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

	let session2 = session.clone();

	web_async::spawn(async move {
		let res = tokio::select! {
			res = recv_session_info::<S>(setup.reader) => res,
			res = send_session_info::<S>(&session2, setup.writer) => res,
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

async fn recv_session_info<S: web_transport_trait::Session>(
	mut reader: crate::coding::Reader<S::RecvStream, Version>,
) -> Result<(), Error> {
	while let Some(_info) = reader.decode_maybe::<SessionInfo>().await? {}
	Err(Error::Cancel)
}

// Send interval scales linearly with the relative change in bitrate:
//   0% change  → wait MAX_SEND_INTERVAL
//   ≥SEND_CHANGE_THRESHOLD change → wait MIN_SEND_INTERVAL
const MIN_SEND_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
const MAX_SEND_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const SEND_CHANGE_THRESHOLD: f64 = 0.25;
const SEND_CHECK_INTERVAL: std::time::Duration = MIN_SEND_INTERVAL;

async fn send_session_info<S: web_transport_trait::Session>(
	session: &S,
	mut writer: Writer<S::SendStream, Version>,
) -> Result<(), Error> {
	use web_transport_trait::Stats;

	let mut interval = tokio::time::interval(SEND_CHECK_INTERVAL);
	let mut last_sent: Option<u64> = None;
	let mut last_sent_at = tokio::time::Instant::now();

	loop {
		// Poll frequently so we detect sudden bitrate changes quickly.
		interval.tick().await;

		let bitrate = session.stats().estimated_send_rate();

		let Some(bitrate) = bitrate else {
			continue;
		};

		// Only send updates when the bitrate has changed significantly.
		// Small changes wait longer (up to MAX), large changes send sooner (down to MIN).
		let should_send = match last_sent {
			None => true,
			Some(prev) => {
				let change = (bitrate as f64 - prev as f64).abs() / prev.max(1) as f64;
				let t = (change / SEND_CHANGE_THRESHOLD).min(1.0);
				let required = MAX_SEND_INTERVAL.mul_f64(1.0 - t) + MIN_SEND_INTERVAL.mul_f64(t);
				last_sent_at.elapsed() >= required
			}
		};

		if should_send {
			let info = SessionInfo { bitrate: Some(bitrate) };
			writer.encode(&info).await?;
			last_sent = Some(bitrate);
			last_sent_at = tokio::time::Instant::now();
		}
	}
}
