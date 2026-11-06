use crate::quic::web::{
	Connecting, ServerError, Session, Settings,
	proto::{ConnectRequest, ConnectResponse},
};

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

	/// Accept the request without selecting a subprotocol.
	pub async fn ok(self) -> Result<Session, ServerError> {
		self.respond(ConnectResponse::OK).await
	}

	/// Reply to the session with the given response, usually 200 OK.
	///
	/// [ConnectResponse::with_protocol] can be used to select a subprotocol.
	pub async fn respond(self, response: impl Into<ConnectResponse>) -> Result<Session, ServerError> {
		let response = response.into();
		if response.status != http::StatusCode::OK {
			return Err(super::ConnectError::ErrorStatus(response.status).into());
		}
		let connect = self.connect.respond(response).await?;
		Ok(Session::new(self.conn, self.settings, connect))
	}

	/// Reject the session with a 4xx or 5xx status code.
	pub async fn reject(self, status: http::StatusCode) -> Result<(), ServerError> {
		if !status.is_client_error() && !status.is_server_error() {
			return Err(super::ConnectError::ErrorStatus(status).into());
		}
		self.connect.reject(status).await?;
		Ok(())
	}

	/// Returns the underlying QUIC connection.
	pub fn connection(&self) -> &crate::quic::driver::connection::Connection {
		&self.conn
	}
}

impl core::ops::Deref for Request {
	type Target = ConnectRequest;

	fn deref(&self) -> &Self::Target {
		&self.connect
	}
}
