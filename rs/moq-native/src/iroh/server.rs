use std::net;

use anyhow::Context;
use web_transport_iroh::{http, iroh};

use futures::{
	future::BoxFuture,
	stream::{FuturesUnordered, StreamExt},
	FutureExt,
};

use crate::{iroh::EndpointConfig, MoqServer, Request};

pub struct Server {
	endpoint: iroh::Endpoint,
	accept: FuturesUnordered<BoxFuture<'static, anyhow::Result<Request>>>,
	fingerprints: Vec<String>,
}

impl MoqServer for Server {
	async fn accept(&mut self) -> Option<Request> {
		self.accept().await
	}
}

impl Server {
	pub async fn new(config: EndpointConfig) -> anyhow::Result<Self> {
		let endpoint = config.bind().await?;
		Ok(Self {
			endpoint,
			accept: Default::default(),
			fingerprints: vec![], // TODO: Do we need these for iroh endpoint? Don't think so.
		})
	}

	pub fn endpoint(&self) -> &iroh::Endpoint {
		&self.endpoint
	}

	pub fn fingerprints(&self) -> &[String] {
		&self.fingerprints
	}

	/// Returns the next partially established QUIC or WebTransport session.
	///
	/// This returns a [Request] instead of a [web_transport_quinn::Session]
	/// so the connection can be rejected early on an invalid path or missing auth.
	///
	/// The [Request] is either a WebTransport or a raw QUIC request.
	/// Call [Request::ok] or [Request::close] to complete the handshake in case this is
	/// a WebTransport request.
	pub async fn accept(&mut self) -> Option<Request> {
		loop {
			tokio::select! {
				res = self.endpoint.accept() => {
					let conn = res?;
					self.accept.push(Self::accept_session(conn).boxed());
				}
				Some(res) = self.accept.next() => {
					match res {
						Ok(session) => return Some(session),
						Err(err) => tracing::debug!(%err, "failed to accept session"),
					}
				}
				_ = tokio::signal::ctrl_c() => {
					self.close().await;
					return None;
				}
			}
		}
	}

	async fn accept_session(conn: iroh::endpoint::Incoming) -> anyhow::Result<Request> {
		let conn = conn.accept()?.await?;
		let alpn = String::from_utf8(conn.alpn().to_vec()).context("failed to decode ALPN")?;
		tracing::Span::current().record("id", conn.stable_id());
		tracing::debug!(remote = %conn.remote_id().fmt_short(), %alpn, "accepted");
		match alpn.as_str() {
			web_transport_iroh::ALPN_H3 => {
				let request = web_transport_iroh::H3Request::accept(conn)
					.await
					.context("failed to receive WebTransport request")?;
				Ok(Request::IrohWebTransport(request))
			}
			moq_lite::lite::ALPN | moq_lite::ietf::ALPN => {
				let request = IrohQuicRequest::accept(conn);
				Ok(Request::IrohQuic(request))
			}
			_ => Err(anyhow::anyhow!("unsupported ALPN: {alpn}")),
		}
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		self.endpoint
			.bound_sockets()
			.into_iter()
			.next()
			.context("failed to get local address")
	}

	// Takes `&mut self` even though `&self` would be enough, because otherwise [`Self::accept`] becomes !Sync.
	// Alternative would be wrapping `Self::accept` in [sync_wrapper](https://docs.rs/sync_wrapper/latest/sync_wrapper/)
	pub async fn close(&mut self) {
		self.endpoint.close().await
	}
}

pub struct IrohQuicRequest(iroh::endpoint::Connection);

impl IrohQuicRequest {
	/// Accept a new QUIC-only WebTransport session from a client.
	pub fn accept(conn: iroh::endpoint::Connection) -> Self {
		Self(conn)
	}

	/// Accept the session.
	pub fn ok(self) -> web_transport_iroh::Session {
		web_transport_iroh::Session::raw(self.0)
	}

	/// Reject the session.
	pub fn close(self, status: http::StatusCode) {
		self.0.close(status.as_u16().into(), status.as_str().as_bytes());
	}
}
