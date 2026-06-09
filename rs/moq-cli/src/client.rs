use crate::{Publish, StatsArgs, run_stats};

use anyhow::Context;
use hang::moq_net;
use url::Url;

pub async fn run_client(
	client: moq_native::Client,
	url: Url,
	name: String,
	publish: Publish,
	stats: StatsArgs,
) -> anyhow::Result<()> {
	// Create an origin producer to publish to the broadcast.
	let origin = moq_net::Origin::random().produce();
	let _publish = origin
		.publish_broadcast(&name, publish.consume())
		.context("failed to publish broadcast")?;

	let stats_agg = stats.build();
	let client = match &stats_agg {
		Some(agg) => client.with_stats(agg.tier(moq_net::Tier::External)),
		None => client,
	};

	tracing::info!(%url, %name, "connecting");

	let reconnect = client.with_publisher(origin.clone()).reconnect(url);

	#[cfg(unix)]
	// Notify systemd that we're ready.
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = publish.run() => res,
		res = reconnect.closed() => res,
		res = run_stats(stats_agg, stats.interval) => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}
