use crate::{Auth, AuthParams, AuthToken, Cluster};

use anyhow::Context;
use axum::http;
use moq_native::Request;

/// True when `candidate` is `base` followed by `:<digits>` (a non-empty,
/// all-ASCII-digit port). DNS SANs cannot carry ports, so a node name
/// matching its SAN may only differ by such a suffix.
pub fn is_san_with_port(base: &str, candidate: &str) -> bool {
	candidate
		.strip_prefix(base)
		.and_then(|s| s.strip_prefix(':'))
		.is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

/// Pick the cluster node name for a peer authenticated by a DNS-SAN cert.
///
/// The SAN is required and authoritative: `claimed` may match it directly or
/// extend it with a `:port` suffix (DNS SANs cannot carry ports), but cannot
/// substitute a different name. When `claimed` is `None`, the SAN is used.
///
/// Used both for our own outbound identity (cluster.node vs client.tls SAN)
/// and for inbound mTLS peers (?register= vs peer cert SAN).
pub fn validate_peer(san: Option<&str>, claimed: Option<&str>) -> anyhow::Result<String> {
	let san = san.context("certificate is missing a DNS SAN")?;
	let node = match claimed {
		None => san.to_owned(),
		Some(reg) if reg == san => reg.to_owned(),
		Some(reg) => {
			anyhow::ensure!(
				is_san_with_port(san, reg),
				"node name {reg:?} does not match cert SAN {san:?}"
			);
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
		let token = if let Some(peer) = self.request.peer_identity()? {
			match validate_peer(peer.dns_name.as_deref(), params.register.as_deref()) {
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
					let status: http::StatusCode = (&err).into();
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
