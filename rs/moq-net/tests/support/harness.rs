//! Test harness that produces a connected `(client, server)` pair of
//! [`moq_net::Session`]s over the in-memory mock transport.
//!
//! The harness runs the full MoQ handshake (Client::connect + Server::accept)
//! over a [`MockSession`] pair and spawns both protocol drivers, giving tests
//! two live sessions ready for pub/sub without any real QUIC or network I/O.

#![allow(dead_code)]

use moq_net::{Client, Server, Session, Version, origin};

use super::mock::create_mock_session_pair;

/// Options for [`connect_mock`].
pub struct MockConnectOptions {
	/// The MoQ version to negotiate (determines the ALPN protocol string).
	pub version: Version,
	/// Origin whose broadcasts the client publishes to the server.
	pub client_publish: Option<origin::Producer>,
	/// Origin the client inserts remote broadcasts into.
	pub client_subscribe: Option<origin::Producer>,
	/// Origin whose broadcasts the server publishes to the client.
	pub server_publish: Option<origin::Producer>,
	/// Origin the server inserts remote broadcasts into.
	pub server_subscribe: Option<origin::Producer>,
}

impl MockConnectOptions {
	/// Create options for the given version with no origins attached.
	pub fn new(version: Version) -> Self {
		Self {
			version,
			client_publish: None,
			client_subscribe: None,
			server_publish: None,
			server_subscribe: None,
		}
	}
}

/// A connected mock pair. Both protocol drivers run on spawned tasks for the
/// lifetime of their session.
pub struct MockPair {
	pub client: Session,
	pub server: Session,
}

/// Run the MoQ handshake over the mock transport, returning connected sessions.
///
/// Both sides negotiate the version via ALPN (the mock reports the protocol
/// string matching the requested version), mirroring a real QUIC transport
/// where ALPN selects the wire format before the connection starts.
///
/// # Panics
///
/// Panics if the handshake fails on either side (test-only code).
pub async fn connect_mock(opts: MockConnectOptions) -> MockPair {
	let protocol = opts.version.alpn();
	let (client_transport, server_transport) = create_mock_session_pair(Some(protocol));

	let mut client = Client::new().with_versions(opts.version.into());
	if let Some(publish) = &opts.client_publish {
		client = client.with_publisher(publish);
	}
	if let Some(subscribe) = opts.client_subscribe {
		client = client.with_subscriber(subscribe);
	}

	let mut server = Server::new().with_versions(opts.version.into());
	if let Some(publish) = &opts.server_publish {
		server = server.with_publisher(publish);
	}
	if let Some(subscribe) = opts.server_subscribe {
		server = server.with_subscriber(subscribe);
	}

	// Run both handshakes concurrently, spawning each side's driver the moment
	// its handshake resolves: on draft-17+ the server's accept blocks on the
	// client's SETUP, which only reaches the wire once the client's driver is
	// polled (and vice versa for the server's own SETUP).
	let client_fut = async {
		let (session, driver) = client.connect(client_transport).await.expect("client handshake failed");
		tokio::spawn(driver);
		session
	};
	let server_fut = async {
		let (session, driver) = server.accept(server_transport).await.expect("server handshake failed");
		tokio::spawn(driver);
		session
	};
	let (client_session, server_session) = tokio::join!(client_fut, server_fut);

	MockPair {
		client: client_session,
		server: server_session,
	}
}
