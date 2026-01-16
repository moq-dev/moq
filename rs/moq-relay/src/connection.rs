use crate::{Auth, Cluster};

use moq_native::Request;
use std::sync::Arc;

pub struct Connection {
	pub id: u64,
	pub request: Request,
	pub cluster: Cluster,
	pub auth: Auth,
}

impl Connection {
	#[tracing::instrument("conn", skip_all, fields(id = self.id))]
	pub async fn run(self) -> anyhow::Result<()> {
		// Track WebTransport sessions.
		let metrics = self.cluster.metrics.clone();
		metrics.inc_active_sessions(crate::Transport::WebTransport);
		struct SessionGuard {
			metrics: crate::MetricsTracker,
			transport: crate::Transport,
		}
		impl Drop for SessionGuard {
			fn drop(&mut self) {
				self.metrics.dec_active_sessions(self.transport);
			}
		}
		let _guard = SessionGuard {
			metrics: metrics.clone(),
			transport: crate::Transport::WebTransport,
		};

		let (path, token) = match self.request.url() {
			Some(url) => {
				// Extract the path and token from the URL.
				let path = url.path();
				let token = url.query_pairs().find(|(k, _)| k == "jwt").map(|(_, v)| v.to_string());
				(path, token)
			}
			None => ("", None),
		};
		// Verify the URL before accepting the connection.
		let token = match self.auth.verify(path, token.as_deref()) {
			Ok(token) => token,
			Err(err) => {
				let _ = self.request.reject(err.clone().into()).await;
				return Err(err.into());
			}
		};

		let publish = self.cluster.publisher(&token);
		let subscribe = self.cluster.subscriber(&token);

		match (&publish, &subscribe) {
			(Some(publish), Some(subscribe)) => {
				tracing::info!(root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "session accepted");
			}
			(Some(publish), None) => {
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
		let stats: Arc<dyn moq_lite::Stats> =
			Arc::new(crate::TransportStats::new(metrics, crate::Transport::WebTransport));
		let session = self
			.request
			.accept_with_stats(subscribe, publish, Some(stats))
			.await?;

		// Wait until the session is closed.
		session.closed().await.map_err(Into::into)
	}
}
