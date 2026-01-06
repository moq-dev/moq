use crate::Publish;

use hang::moq_lite;
use url::Url;

pub async fn client(
	config: moq_native::ClientConfig,
	#[cfg(feature = "iroh")] iroh: Option<moq_native::iroh::EndpointConfig>,
	url: Url,
	name: String,
	publish: Publish,
) -> anyhow::Result<()> {
	tracing::info!(%url, %name, "connecting");
	// Create an origin producer to publish to the broadcast.
	let origin = moq_lite::Origin::produce();
	origin.producer.publish_broadcast(&name, publish.consume());

	#[cfg(not(feature = "iroh"))]
	let client = config.init().await?;

	#[cfg(feature = "iroh")]
	let client = config.init_with_iroh(iroh).await?;

	// Establish the connection, not providing a subscriber.
	let session = client.connect_with_fallback(url, origin.consumer, None).await?;

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
