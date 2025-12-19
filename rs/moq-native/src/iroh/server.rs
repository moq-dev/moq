use std::net;

use anyhow::Context;
use web_transport_iroh::iroh;

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
		let conn = conn.accept()?;
		let conn = conn.await?;
		let alpn = String::from_utf8(conn.alpn().to_vec()).context("failed to decode ALPN")?;
		let span = tracing::Span::current();
		span.record("id", conn.stable_id()); // TODO can we get this earlier?
		tracing::debug!(remote = %conn.remote_id().fmt_short(), %alpn, "accepted");

		match alpn.as_str() {
			moq_lite::lite::ALPN | moq_lite::ietf::ALPN | web_transport_iroh::ALPN => {
				let request = web_transport_iroh::Request::accept(conn)
					.await
					.context("failed to receive WebTransport request")?;
				Ok(Request::Iroh(request))
			}
			_ => anyhow::bail!("unsupported ALPN: {alpn}"),
		}
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		self.endpoint
			.bound_sockets()
			.into_iter()
			.next()
			.context("failed to get local address")
	}

	pub async fn close(&mut self) {
		self.endpoint.close().await
	}
}
