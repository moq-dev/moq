use crate::Publish;

use hang::moq_lite;
use moq_native::web_transport_quinn::generic;
use url::Url;

pub async fn client(
	config: moq_native::ClientConfig,
	#[cfg(feature = "iroh")] iroh: moq_native::iroh::EndpointConfig,
	url: Url,
	name: String,
	publish: Publish,
) -> anyhow::Result<()> {
	tracing::info!(%url, %name, "connecting");
	match url.scheme() {
		#[cfg(feature = "iroh")]
		"iroh" => {
			let client = iroh.init_client().await?;
			let session = client.connect(url).await?;
			run_import_session(session, name, publish).await?;
			client.close().await;
			Ok(())
		}
		_ => {
			let client = config.init()?;
			let session = client.connect(url).await?;
			run_import_session(session, name, publish).await
		}
	}
}

async fn run_import_session<S>(session: S, name: String, publish: Publish) -> anyhow::Result<()>
where
	S: generic::Session,
{
	// Create an origin producer to publish to the broadcast.
	let origin = moq_lite::Origin::produce();
	origin.producer.publish_broadcast(&name, publish.consume());

	// Establish the connection, not providing a subscriber.
	let session = moq_lite::Session::connect(session, origin.consumer, None).await?;

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
