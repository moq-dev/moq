use crate::{Publish, Subscribe, SubscribeArgs};

use hang::moq_lite;
use url::Url;

pub async fn run_client(client: moq_native::Client, url: Url, name: String, publish: Publish) -> anyhow::Result<()> {
	let origin = moq_lite::Origin::produce();
	origin.publish_broadcast(&name, publish.consume());

	tracing::info!(%url, %name, "connecting");

	let mut session = client.with_publish(origin.consume()).connect(url).await?;

	#[cfg(unix)]
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

pub async fn run_client_subscribe(
	client: moq_native::Client,
	url: Url,
	name: String,
	args: SubscribeArgs,
) -> anyhow::Result<()> {
	let origin = moq_lite::Origin::produce();
	let mut consumer = origin.consume();

	tracing::info!(%url, %name, "connecting to subscribe");

	let mut session = client.with_consume(origin).connect(url).await?;

	#[cfg(unix)]
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	// Wait for the named broadcast to be announced
	let broadcast = loop {
		let (path, announced) = consumer
			.announced()
			.await
			.ok_or_else(|| anyhow::anyhow!("origin closed"))?;

		if let Some(broadcast) = announced {
			if path.as_ref() == name {
				break broadcast;
			}
		}
	};

	let subscribe = Subscribe::new(broadcast, args);

	tokio::select! {
		res = subscribe.run() => res,
		res = session.closed() => res.map_err(Into::into),
		_ = tokio::signal::ctrl_c() => {
			session.close(moq_lite::Error::Cancel);
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
			Ok(())
		},
	}
}
