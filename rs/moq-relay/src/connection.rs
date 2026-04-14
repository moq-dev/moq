use crate::{Auth, AuthParams, AuthToken, Cluster};

use anyhow::Context;
use axum::http;
use moq_native::Request;

/// Pick the cluster node name for an mTLS-authenticated peer.
///
/// The SAN is required on the client certificate, and it is authoritative:
/// the query param may only add a `:port` suffix, so a peer can't register
/// under another SAN's identity.
fn resolve_peer_node(san: Option<&str>, register: Option<&str>) -> anyhow::Result<String> {
	let san = san.context("client certificate is missing a DNS SAN")?;
	let node = match register {
		None => san.to_owned(),
		Some(reg) if reg == san => reg.to_owned(),
		Some(reg) => {
			let ok = reg
				.strip_prefix(san)
				.and_then(|s| s.strip_prefix(':'))
				.is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()));
			anyhow::ensure!(ok, "register param {reg:?} does not match cert SAN {san:?}");
			reg.to_owned()
		}
	};
	Ok(node)
}

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
		// entirely and grant full (cluster) access. The node name comes
		// from the cert's first DNS SAN. Since DNS SANs cannot carry a
		// port, a `?register=` query param is accepted only if it extends
		// the SAN with a `:port` suffix (e.g. SAN `leaf0` + `?register=leaf0:4444`).
		let token = if let Some(peer) = self.request.peer_identity() {
			match resolve_peer_node(peer.dns_name.as_deref(), params.register.as_deref()) {
				Ok(node) => {
					tracing::debug!(?node, "mTLS peer authenticated");
					AuthToken::from_peer(node)
				}
				Err(err) => {
					let _ = self.request.close(http::StatusCode::FORBIDDEN.as_u16()).await;
					return Err(err);
				}
			}
		} else {
			// Verify the URL before accepting the connection.
			match self.auth.verify(&params).await {
				Ok(token) => token,
				Err(err) => {
					let status: http::StatusCode = err.clone().into();
					let _ = self.request.close(status.as_u16()).await;
					return Err(err.into());
				}
			}
		};

		let publish = self.cluster.publisher(&token);
		let subscribe = self.cluster.subscriber(&token);
		let registration = self.cluster.register(&token);
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
		// Keep registration alive so the cluster node stays announced.
		session.closed().await?;
		drop(registration);
		Ok(())
	}
}
