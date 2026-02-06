use url::Url;

/// A raw QUIC connection request via the quiche backend (not using HTTP/3).
pub struct QuicheQuicRequest {
	connection: web_transport_quiche::ez::Connection,
	url: Url,
}

impl QuicheQuicRequest {
	/// Accept a new raw QUIC session from a client.
	pub fn accept(connection: web_transport_quiche::ez::Connection) -> Self {
		let url: Url = format!("moql://{}", connection.peer_addr())
			.parse()
			.expect("URL is valid");
		Self { connection, url }
	}

	/// Accept the session, wrapping as a raw WebTransport-compatible connection.
	pub fn ok(self) -> web_transport_quiche::Connection {
		web_transport_quiche::Connection::raw(self.connection, self.url)
	}

	/// Returns the URL for this connection.
	#[allow(dead_code)]
	pub fn url(&self) -> &Url {
		&self.url
	}

	/// Reject the session with a status code.
	pub fn close(self, status: web_transport_quiche::http::StatusCode) {
		self.connection.close(status.as_u16().into(), status.as_str());
	}
}
