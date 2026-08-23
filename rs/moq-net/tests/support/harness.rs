//! Test harness that produces a connected `(client, server)` pair of
//! [`moq_net::Session`]s over the in-memory mock transport.
//!
//! The harness runs the full MoQ handshake (Client::connect + Server::accept)
//! over a [`MockSession`] pair and spawns both protocol drivers, giving tests
//! two live sessions ready for pub/sub without any real QUIC or network I/O.

#![allow(dead_code)]

use std::{marker::PhantomData, pin::Pin, task::Poll};

use moq_net::{Client, Server, Session, Version, origin};

use super::mock::{MockSession, create_mock_session_pair};

/// A tokio-backed [`moq_net::runtime::Runtime`] for these tests: machines are
/// spawned onto tokio and timers are tokio sleeps, so `tokio::time::pause` keeps
/// working. A copy of the crate's own `runtime::tokio_test` module, which is
/// `cfg(test)` and therefore invisible to integration tests.
pub struct TokioRuntime<S = MockSession>(PhantomData<fn(S)>);

impl<S> TokioRuntime<S> {
	pub fn new() -> Self {
		Self(PhantomData)
	}
}

impl<S> Clone for TokioRuntime<S> {
	fn clone(&self) -> Self {
		Self(PhantomData)
	}
}

impl<S> Default for TokioRuntime<S> {
	fn default() -> Self {
		Self::new()
	}
}

// Unbounded: timers don't involve the transport, so origin drivers can borrow
// a transportless handle (`TokioRuntime::<()>::new()`).
impl<S> moq_net::runtime::Timers for TokioRuntime<S> {
	type Timer = TokioTimer;

	fn timer(&self) -> Self::Timer {
		TokioTimer { at: None, sleep: None }
	}

	fn now(&self) -> moq_net::runtime::Instant {
		tokio::time::Instant::now().into_std()
	}
}

impl<S: moq_net::transport::poll::Session> moq_net::runtime::Runtime for TokioRuntime<S> {
	type Transport = S;

	fn spawn(&self, machine: moq_net::runtime::Machine<Self>) {
		tokio::spawn(machine);
	}
}

/// The [`moq_net::runtime::Timer`] handed out by [`TokioRuntime`].
pub struct TokioTimer {
	at: Option<moq_net::runtime::Instant>,
	// Allocated on the first poll after arming, then re-armed in place via
	// `Sleep::reset`; construction panics without a live tokio time driver.
	sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl moq_net::runtime::Timer for TokioTimer {
	fn set(&mut self, at: Option<moq_net::runtime::Instant>) {
		self.at = at;
		if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
			sleep.as_mut().reset(tokio::time::Instant::from_std(at));
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.at else { return Poll::Pending };
		let sleep = self
			.sleep
			.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(at))));
		if sleep.is_elapsed() {
			return Poll::Ready(());
		}
		waiter.poll_future(sleep.as_mut())
	}
}

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

	// Run both handshakes concurrently; the runtime spawns each side's machine
	// the moment its handshake resolves: on draft-17+ the server's accept blocks
	// on the client's SETUP, which only reaches the wire once the client's
	// machine is polled (and vice versa for the server's own SETUP).
	let client_fut = async {
		client
			.connect(TokioRuntime::new(), client_transport)
			.await
			.expect("client handshake failed")
	};
	let server_fut = async {
		server
			.accept(TokioRuntime::new(), server_transport)
			.await
			.expect("server handshake failed")
	};
	let (client_session, server_session) = tokio::join!(client_fut, server_fut);

	MockPair {
		client: client_session,
		server: server_session,
	}
}
