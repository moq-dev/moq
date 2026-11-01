//! Throwaway spike (never merges): a relay per worker thread, clustered over the
//! in-memory transport with nothing shared.
//!
//! `SPIKE_CLUSTER=1` with `runtime.workers` gives each tokio worker its own
//! [`Cluster`](crate::cluster::Cluster): its own origin, cache pool, and origin
//! driver, built on the worker's thread. The workers link in a full mesh of
//! [`moq_net::transport::memory`] sessions, so announcements, routes, and groups
//! all cross workers through moq-lite, exactly as between clustered relays.
//! The lower-indexed worker of each pair dials and the other accepts, and each
//! link publishes and subscribes like a dialed cluster peer
//! (`Cluster::run_remote_session`).

use moq_net::{origin, transport::memory};

/// Whether the spike is on.
pub(crate) fn enabled() -> bool {
	std::env::var_os("SPIKE_CLUSTER").is_some_and(|value| !value.is_empty() && value != "0")
}

/// One worker's end of a mesh link.
pub(crate) struct End {
	dial: bool,
	session: memory::Session,
}

/// Every worker's link ends, indexed by worker.
pub(crate) fn mesh(workers: usize) -> Vec<Vec<End>> {
	let mut ends: Vec<Vec<End>> = (0..workers).map(|_| Vec::new()).collect();
	for low in 0..workers {
		for high in low + 1..workers {
			let mut config = memory::Config::default();
			config.protocol = Some("moq-lite-06".into());
			let (client, server) = memory::pair(config);
			ends[low].push(End {
				dial: true,
				session: client,
			});
			ends[high].push(End {
				dial: false,
				session: server,
			});
		}
	}
	ends
}

/// Build this worker's own cluster and start its mesh links on the current thread.
pub(crate) fn worker(config: &crate::Config, ends: Vec<End>) -> anyhow::Result<crate::cluster::Cluster> {
	let cache = config.cache.init()?;
	let mut options = crate::cluster::Options::new(config.cluster.clone()).with_cache(cache);
	options.session = config.limits.client.clone();
	options.peer = config.limits.peer.clone();
	let limits = options.peer.resolve();
	let cluster = crate::cluster::Cluster::new(options)?;
	for end in ends {
		let origin = cluster.origin.clone();
		tokio::task::spawn_local(async move {
			if let Err(err) = link(origin, limits, end).await {
				tracing::warn!(%err, "spike mesh link ended");
			}
		});
	}
	Ok(cluster)
}

async fn link(origin: origin::Producer, limits: moq_net::session::Limits, end: End) -> anyhow::Result<()> {
	let origin = origin.with_link(origin::Link::Peer);
	let publish = origin.consume().with_hidden(true);
	let now = moq_net::time::Instant::now();
	let (session, driver) = match end.dial {
		true => {
			moq_net::Client::new()
				.with_publisher(publish)
				.with_subscriber(origin)
				.with_limits(limits)
				.connect(now, end.session)
				.await?
		}
		false => {
			moq_net::Server::new()
				.with_publisher(publish)
				.with_subscriber(origin)
				.with_limits(limits)
				.accept(now, end.session)
				.await?
		}
	};
	let err = moq_net::time::run(driver).await;
	drop(session);
	Err(err.into())
}
