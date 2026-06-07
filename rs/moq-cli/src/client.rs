use crate::Publish;

use anyhow::Context;
use hang::moq_net;
use url::Url;

pub async fn run_client(client: moq_native::Client, url: Url, name: String, publish: Publish) -> anyhow::Result<()> {
	// Create an origin producer to publish to the broadcast.
	let origin = moq_net::Origin::random().produce();
	let _publish = origin
		.publish_broadcast(&name, publish.consume())
		.context("failed to publish broadcast")?;

	tracing::info!(%url, %name, "connecting");

	let reconnect = client.with_publisher(origin.clone()).reconnect(url);

	#[cfg(unix)]
	// Notify systemd that we're ready.
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = publish.run() => res,
		res = reconnect.closed() => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}
