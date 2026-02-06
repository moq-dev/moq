use crate::{Auth, AuthParams, Cluster};

use moq_native::Request;

pub struct Connection {
	pub id: u64,
	pub request: Request,
	pub cluster: Cluster,
	pub auth: Auth,
}

impl Connection {
	#[tracing::instrument("conn", skip_all, fields(id = self.id))]
	pub async fn run(self) -> anyhow::Result<()> {
		let params = match self.request.url() {
			Some(url) => AuthParams::from_url(&url),
			None => AuthParams::default(),
		};

		// Verify the URL before accepting the connection.
		let token = match self.auth.verify(&params) {
			Ok(token) => token,
			Err(err) => {
				let _ = self.request.reject(err.clone().into()).await;
				return Err(err.into());
			}
		};

		let publish = self.cluster.publisher(&token);
		let subscribe = self.cluster.subscriber(&token);

		match (&publish, &subscribe) {
			(Some((publish, _)), Some(subscribe)) => {
				tracing::info!(root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "session accepted");
			}
			(Some((publish, _)), None) => {
				tracing::info!(root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "publisher accepted");
			}
			(None, Some(subscribe)) => {
				tracing::info!(root = %token.root, subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "subscriber accepted")
			}
			_ => anyhow::bail!("invalid session; no allowed paths"),
		}

		// Accept the connection.
		// NOTE: subscribe and publish seem backwards because of how relays work.
		// We publish the tracks the client is allowed to subscribe to.
		// We subscribe to the tracks the client is allowed to publish.
		let session = self
			.request
			.with_publish(subscribe)
			.with_consume(publish.map(|(p, _)| p))
			// TODO: Uncomment when observability feature is merged
			// .with_stats(stats)
			.accept()
			.await?;

		// Wait until the session is closed.
		session.closed().await.map_err(Into::into)
	}
}
