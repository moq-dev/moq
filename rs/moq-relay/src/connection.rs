use crate::{Auth, AuthParams, AuthToken, Cluster};

use axum::http;
use moq_native::Request;

/// An incoming connection that has not yet been authenticated.
///
/// Call [`run`](Self::run) to authenticate the request, wire up
/// publish/subscribe origins, and serve the session until it closes.
pub struct Connection {
	/// A numeric identifier for logging.
	pub id: u64,
	/// The raw QUIC/WebTransport request to accept or reject.
	pub request: Request,
	/// The cluster state used to resolve origins.
	pub cluster: Cluster,
	/// The authenticator used to verify credentials.
	pub auth: Auth,
}

impl Connection {
	/// Authenticates and serves this connection until it closes.
	#[tracing::instrument("conn", skip_all, fields(id = self.id))]
	pub async fn run(self) -> anyhow::Result<()> {
		let params = match self.request.url() {
			Some(url) => AuthParams::from_url(url),
			None => AuthParams::default(),
		};

		// If the client presented a valid mTLS client certificate, skip JWT
		// entirely and grant full (cluster) access. The cert's chain to the
		// configured CA is the only credential we require — DNS SANs and the
		// `?register=` name are no longer consulted.
		let token = if self.request.peer_identity()?.is_some() {
			tracing::debug!("mTLS peer authenticated");
			AuthToken::unrestricted()
		} else {
			// Verify the URL before accepting the connection.
			match self.auth.verify(&params).await {
				Ok(token) => token,
				Err(err) => {
					let status: http::StatusCode = (&err).into();
					let _ = self.request.close(status.as_u16()).await;
					return Err(err.into());
				}
			}
		};

		let publish = self.cluster.publisher(&token);
		let subscribe = self.cluster.subscriber(&token);
		let transport = self.request.transport();

		match (&publish, &subscribe) {
			(Some(publish), Some(subscribe)) => {
				tracing::info!(transport, root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "session accepted");
			}
			(Some(publish), None) => {
				tracing::info!(transport, root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "publisher accepted");
			}
			(None, Some(subscribe)) => {
				tracing::info!(transport, root = %token.root, subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "subscriber accepted")
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
			.with_consume(publish)
			// TODO: Uncomment when observability feature is merged
			// .with_stats(stats)
			.ok()
			.await?;

		tracing::info!(version = %session.version(), transport, "negotiated");

		// Wait until the session is closed.
		session.closed().await?;
		Ok(())
	}
}
