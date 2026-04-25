use std::time::Duration;

use crate::Publish;

use hang::moq_lite;
use url::Url;

pub async fn run_client(
	client: moq_native::Client,
	url: Url,
	name: String,
	publish: Publish,
	stats_interval: Option<Duration>,
) -> anyhow::Result<()> {
	// Create an origin producer to publish to the broadcast.
	let origin = moq_lite::Origin::random().produce();
	origin.publish_broadcast(&name, publish.consume());

	tracing::info!(%url, %name, "connecting");

	let reconnect = client.with_publish(origin.consume()).reconnect(url);

	#[cfg(unix)]
	// Notify systemd that we're ready.
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = publish.run(stats_interval) => res,
		res = reconnect.closed() => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}
