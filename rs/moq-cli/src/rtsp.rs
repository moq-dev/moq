//! RTSP import: pull an IP camera's session, reconnecting under a fresh instance per run.

use std::time::Duration;

use anyhow::Context;
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
/// Each run is its own broadcast under a fresh instance, with a fresh catalog starting on a
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
			Err(Failure::Session(err)) if retries(&err) => tracing::warn!(%url, "RTSP session failed: {err}"),
			Err(Failure::Session(err)) => return Err(err.into()),
			Err(Failure::Local(err)) => return Err(err),
		}

		tracing::info!(%url, ?backoff, "RTSP reconnecting");
		kio::clock::sleep(backoff).await;
		backoff = (backoff * 2).min(BACKOFF_MAX);
	}
}

/// Why a run failed: its RTSP session, or something local such as an invalid broadcast name, which
/// a redial cannot fix.
enum Failure {
	Session(moq_rtsp::Error),
	Local(anyhow::Error),
}

impl std::fmt::Display for Failure {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Session(err) => write!(f, "{err}"),
			Self::Local(err) => write!(f, "{err:#}"),
		}
	}
}

/// Whether redialing can fix a session failure: anything but refused access or nothing publishable.
fn retries(err: &moq_rtsp::Error) -> bool {
	!matches!(
		err,
		moq_rtsp::Error::Unauthorized(_) | moq_rtsp::Error::NoSupportedStream(_)
	)
}

/// Publish one session as one broadcast, announced once its catalog names a track, setting
/// `published` then.
async fn run(target: &ImportTarget, client: &moq_rtsp::Client, published: &mut bool) -> Result<moq_rtsp::End, Failure> {
	// Each run is a new route under a fresh instance, so a redial replaces the last run.
	let route = target
		.origin
		.route(&target.name, moq_net::origin::RouteOptions::hidden())
		.context("failed to create broadcast")
		.map_err(Failure::Local)?;
	let mut catalog = moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), target.ingest.catalog())
		.map_err(|err| Failure::Local(err.into()))?;

	// A relay asks for a broadcast's tracks the moment it is announced, so wait for the
	// first keyframe to put them in the catalog.
	let announce = async {
		let mut updates = catalog.consume()?;
		// A catalog that ends unpublished leaves nothing to announce; the pull is ending too.
		if updates.next().await?.is_none() {
			return std::future::pending().await;
		}
		route.announce().context("failed to announce broadcast")?;
		tracing::info!(name = %target.name, instance = %route.instance(), "publishing RTSP broadcast");
		*published = true;
		std::future::pending::<anyhow::Result<moq_rtsp::End>>().await
	};

	let end = tokio::select! {
		end = client.pull(route.broadcast(), &catalog) => end.map_err(Failure::Session),
		err = announce => err.map_err(Failure::Local),
	};
	// Closed at once even after a clean end: the pull redials and publishes the next run under a
	// new instance, so a run's end is not the content's, and a retained run's finished catalog
	// would tell readers to stop if the next run dropped.
	let finished = match &end {
		Ok(_) => catalog.finish(),
		Err(err) => {
			catalog.abort(moq_net::Error::Transport(err.to_string()));
			Ok(())
		}
	};
	route.broadcast().close();
	finished.map_err(|err| Failure::Local(err.into()))?;
	end
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A session failure is worth redialing unless the server refused access or offers nothing
	/// publishable.
	#[test]
	fn session_failures_retry() {
		assert!(retries(&moq_rtsp::Error::Rtsp("connection reset".into())));
		assert!(retries(&moq_rtsp::Error::Stalled(Duration::from_secs(10))));
		assert!(!retries(&moq_rtsp::Error::Unauthorized("401".into())));
		assert!(!retries(&moq_rtsp::Error::NoSupportedStream("audio/pcmu".into())));
	}
}
