//! RTSP import: pull an IP camera's session, reconnecting under a fresh epoch per run.

use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_tokio::RedactedUrl;
use url::Url;

use crate::moq::{ImportTarget, notify_ready};

/// The first reconnect delay, and the one a run that published resets to.
const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// The longest reconnect delay.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// RTSP import args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ImportArgs {
	/// Pull `rtsp://[user:pass@]host[:port]/path`, such as an IP camera's stream.
	#[usage(name = "rtsp-connect", long = "connect", value_name = "URL")]
	pub connect: Url,
}

/// Pull `url` into the Origin as `target.name` until a fatal error, reconnecting with backoff.
///
/// Each run is its own broadcast under a fresh epoch, with a fresh catalog starting on a
/// keyframe: a new session's RTP time restarts, so it never splices onto the old one.
pub async fn connect_import(target: ImportTarget, url: Url) -> anyhow::Result<()> {
	let client = moq_rtsp::Client::new(url.clone())?;
	let url = RedactedUrl::new(&url);
	tracing::info!(%url, name = %target.name, "RTSP client pulling");
	notify_ready();

	let mut backoff = BACKOFF_INITIAL;
	loop {
		let mut published = false;
		let result = run(&target, &client, &mut published).await;
		// A run that published resets the backoff; one that did not keeps a broken camera from
		// being redialed in a tight loop.
		if published {
			backoff = BACKOFF_INITIAL;
		}
		match result {
			// A restarted timeline: restart the broadcast at once.
			Ok(moq_rtsp::End::Jumped) if published => continue,
			Ok(moq_rtsp::End::Jumped) => tracing::warn!(%url, "RTSP timeline jumped before publishing"),
			Ok(_) => tracing::info!(%url, "RTSP session ended"),
			Err(err) if is_fatal(&err) => return Err(err),
			Err(err) => tracing::warn!(%url, "RTSP session failed: {err:#}"),
		}

		tracing::info!(%url, ?backoff, "RTSP reconnecting");
		kio::clock::sleep(backoff).await;
		backoff = (backoff * 2).min(BACKOFF_MAX);
	}
}

/// Retrying cannot fix these: refused access, nothing publishable, or a local failure (such as
/// an invalid broadcast name) rather than one from the RTSP session.
fn is_fatal(err: &anyhow::Error) -> bool {
	!matches!(
		err.downcast_ref::<moq_rtsp::Error>(),
		Some(err) if !matches!(err, moq_rtsp::Error::Unauthorized(_) | moq_rtsp::Error::NoSupportedStream(_))
	)
}

/// Publish one session as one broadcast, announced once its catalog names a track, setting
/// `published` then.
async fn run(target: &ImportTarget, client: &moq_rtsp::Client, published: &mut bool) -> anyhow::Result<moq_rtsp::End> {
	let broadcast = target
		.origin
		.create_broadcast(&target.name)
		.context("failed to create broadcast")?;
	let config = moq_mux::catalog::Config::default()
		.with_max_age(target.max_age)
		.with_bandwidth(target.bandwidth.clone());
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config)?;

	// A relay asks for a broadcast's tracks the moment it is announced, so wait for the
	// first keyframe to put them in the catalog.
	let announce = async {
		let mut updates = catalog.consume()?;
		// A catalog that ends unpublished leaves nothing to announce; the pull is ending too.
		if updates.next().await?.is_none() {
			return std::future::pending().await;
		}
		let epoch = moq_net::Epoch::mint();
		broadcast
			.announce(moq_net::origin::Route::default().with_epoch(epoch.clone()))
			.context("failed to announce broadcast")?;
		tracing::info!(name = %target.name, %epoch, "publishing RTSP broadcast");
		*published = true;
		std::future::pending::<anyhow::Result<moq_rtsp::End>>().await
	};

	let end = tokio::select! {
		end = client.pull(&broadcast, &catalog) => end.map_err(anyhow::Error::from),
		err = announce => err,
	};
	// Closed at once even after a clean end: the pull redials and publishes the next run under a
	// newer epoch, so a run's end is not the content's, and a retained run's finished catalog
	// would tell readers to stop if the next run dropped.
	let finished = match &end {
		Ok(_) => catalog.finish(),
		Err(err) => {
			catalog.abort(moq_net::Error::Transport(format!("{err:#}")));
			Ok(())
		}
	};
	broadcast.close();
	finished?;
	end
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Only a failure of the RTSP session itself is worth redialing.
	#[test]
	fn only_session_failures_retry() {
		let retry = |err: anyhow::Error| !is_fatal(&err);
		assert!(retry(moq_rtsp::Error::Rtsp("connection reset".into()).into()));
		assert!(retry(moq_rtsp::Error::Stalled(Duration::from_secs(10)).into()));
		assert!(!retry(moq_rtsp::Error::Unauthorized("401".into()).into()));
		assert!(!retry(moq_rtsp::Error::NoSupportedStream("audio/pcmu".into()).into()));
		assert!(!retry(anyhow::anyhow!("failed to create broadcast")));
	}
}
