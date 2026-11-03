use futures::{StreamExt, future::BoxFuture, stream::FuturesUnordered};

use crate::quic::web::{
	Connecting, ServerError, Session, Settings,
	proto::{ConnectRequest, ConnectResponse},
};

/// A WebTransport server that accepts new sessions.
pub struct Server {
	endpoint: crate::quic::driver::endpoint::Endpoint,
	accept: FuturesUnordered<BoxFuture<'static, Result<Request, ServerError>>>,
}

impl core::ops::Deref for Server {
	type Target = crate::quic::driver::endpoint::Endpoint;

	fn deref(&self) -> &Self::Target {
		&self.endpoint
	}
}

impl Server {
	/// Manually create a new server with a manually constructed Endpoint.
	///
	/// NOTE: The ALPN must be set to `crate::quic::web::ALPN` for WebTransport to work.
	pub fn new(endpoint: crate::quic::driver::endpoint::Endpoint) -> Self {
		Self {
			endpoint,
			accept: Default::default(),
		}
	}

	/// Accept a new WebTransport session Request from a client.
	pub async fn accept(&mut self) -> Option<Request> {
		loop {
			tokio::select! {
				res = self.endpoint.accept() => {
					let conn = res?;
					self.accept.push(Box::pin(async move {
						let conn = conn.accept()?.await?;
						Request::accept(conn).await
					}));
				}
				Some(res) = self.accept.next() => {
					if let Ok(session) = res {
						return Some(session)
					}
				}
			}
		}
	}
}

/// A mostly complete WebTransport handshake, just awaiting the server's decision on whether to
/// accept or reject the session based on the URL.
pub struct Request {
	conn: crate::quic::driver::connection::Connection,
	settings: Settings,
	connect: Connecting,
}

impl Request {
	/// Accept a new WebTransport session from a client.
	pub async fn accept(conn: crate::quic::driver::connection::Connection) -> Result<Self, ServerError> {
		// Perform the H3 handshake by sending/reciving SETTINGS frames.
		let settings = Settings::connect(&conn).await?;

		// Accept the CONNECT request but don't send a response yet.
		let connect = Connecting::accept(&conn).await?;

		// Return the resulting request with a reference to the settings/connect streams.
		Ok(Self {
			conn,
			settings,
			connect,
		})
	}

	pub async fn ok(self) -> Result<Session, ServerError> {
		self.respond(ConnectResponse::OK).await
	}

	/// Reply to the session with the given response, usually 200 OK.
	///
	/// [ConnectResponse::with_protocol] can be used to select a subprotocol.
	pub async fn respond(self, response: impl Into<ConnectResponse>) -> Result<Session, ServerError> {
		let response = response.into();
		let connect = self.connect.respond(response).await?;
		Ok(Session::new(self.conn, self.settings, connect))
	}

	/// Reject the session with the given status code.
	pub async fn reject(self, status: http::StatusCode) -> Result<(), ServerError> {
		self.connect.reject(status).await?;
		Ok(())
	}

	/// Returns the underlying QUIC connection.
	pub fn conn(&self) -> &crate::quic::driver::connection::Connection {
		&self.conn
	}

	/// Returns the CONNECT request that was sent by the client.
	///
	/// DEPRECATED: You can access this via the Deref impl.
	pub fn connect(&self) -> &ConnectRequest {
		&self.connect
	}
}

impl core::ops::Deref for Request {
	type Target = ConnectRequest;

	fn deref(&self) -> &Self::Target {
		&self.connect
	}
}
