use crate::Publish;

use hang::moq_lite;
use url::Url;

pub async fn run_client(client: moq_native::Client, url: Url, name: String, publish: Publish) -> anyhow::Result<()> {
	// Create an origin producer to publish to the broadcast.
	let origin = moq_lite::Origin::produce();
	origin.publish_broadcast(&name, publish.consume());

	tracing::info!(%url, %name, "connecting");

	// Establish the connection, not providing a subscriber.
	let session = client.with_publish(origin.consume()).connect(url).await?;

	#[cfg(unix)]
	// Notify systemd that we're ready.
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = publish.run() => res,
		res = session.closed() => res.map_err(Into::into),
		_ = tokio::signal::ctrl_c() => {
			session.close(moq_lite::Error::Cancel);
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
			Ok(())
		},
	}
}
