//! The sessions a [`Connection`](crate::Connection) serves, whatever transport carries them.

use crate::auth;

/// A session whose SETUP awaits the relay's reply: what a [`Connection`](crate::Connection) serves.
///
/// Sealed: native requests, io_uring workers' requests, plus in-memory accepted sessions
/// with `test-support`.
pub trait Incoming: Sealed {}

impl Incoming for moq_tokio::server::Request {}

#[cfg(all(target_os = "linux", feature = "_uring"))]
impl Incoming for crate::uring::Request {}

#[cfg(feature = "test-support")]
impl Incoming for Accepted {}

/// A session accepted on any moq-net transport, its SETUP awaiting the relay's reply.
///
/// The auth decider sees the path, query, token, and role the SETUP carried, so the
/// client must send its path in-band.
#[cfg(feature = "test-support")]
pub struct Accepted {
	handshake: moq_net::server::Handshake,
	transport: moq_auth::Transport,
}

#[cfg(feature = "test-support")]
impl Accepted {
	/// Wrap `handshake`, reported to the auth decider as arriving over `transport`.
	pub fn new(handshake: moq_net::server::Handshake, transport: moq_auth::Transport) -> Self {
		Self { handshake, transport }
	}
}

/// What a [`Connection`](crate::Connection) needs from its request.
///
/// Public in a private module, so [`Incoming`] can name it while nothing outside
/// this crate can implement it.
#[expect(
	async_fn_in_trait,
	reason = "a sealed trait, so no outside implementor needs a `Send` bound"
)]
pub trait Sealed: Sized {
	/// The running session's protocol driver.
	type Driver: Send + 'static;

	/// The peer's socket address, for the connection's log span.
	fn remote(&self) -> Option<std::net::SocketAddr>;

	/// The transport, for the line logged once the session is negotiated.
	fn transport(&self) -> &'static str;

	/// The tokio runtime that admits and supervises the session, when the connection's own
	/// task cannot: one on an io_uring worker, which has no tokio reactor for the auth
	/// client and stops before the drain ends. `None` does both in place.
	fn shared(&self) -> Option<&tokio::runtime::Handle> {
		None
	}

	/// What the auth decider is asked about this session.
	fn admission(&self, auth: &auth::Auth) -> moq_auth::Request;

	/// Resolve once the handshake deadline passes, having closed the session with the
	/// timeout code; never, without a deadline.
	async fn expired(&mut self);

	/// Refuse the session, closing it with `code`.
	async fn reject(self, code: moq_net::SessionError);

	/// What holds the connection to a capped grant once the session runs, or `None` on
	/// a transport that cannot, which refuses one.
	fn pacer(&self) -> Option<auth::Pacer> {
		None
	}

	/// Apply QUIC limits before answering SETUP, which then opens the receive window the
	/// handshake held back. A transport without moq-quic ignores them.
	fn with_quic_limits(self, _limits: moq_quic::Limits) -> anyhow::Result<Self> {
		Ok(self)
	}

	fn auth(&self) -> moq_net::auth::Handle;
	fn requests(&mut self) -> moq_net::auth::Requests;
	fn role(&self) -> Option<moq_net::setup::Role>;
	/// The identity the peer declared in its SETUP, if any: its Hop ID, or from moq-lite-07 its Node ID.
	fn peer_hop(&self) -> Option<std::num::NonZeroU64>;
	fn with_stats(self, stats: moq_net::stats::Session) -> Self;
	fn with_limits(self, limits: moq_net::session::Limits) -> Self;
	fn with_publisher(self, publish: moq_net::origin::Consumer) -> Self;
	fn with_subscriber(self, subscribe: moq_net::origin::Producer) -> Self;

	/// Answer the SETUP. A MoQ failure comes back as a bare [`moq_net::Error`].
	async fn ok(self) -> anyhow::Result<(moq_net::Session, Self::Driver)>;

	/// Run the session's protocol until it ends, alongside its supervision.
	fn drive(driver: Self::Driver) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}

impl Sealed for moq_tokio::server::Request {
	/// Spawned by [`Request::ok`](moq_tokio::server::Request::ok) on its own task.
	type Driver = ();

	fn remote(&self) -> Option<std::net::SocketAddr> {
		self.remote_addr()
	}

	fn transport(&self) -> &'static str {
		moq_tokio::server::Request::transport(self).as_str()
	}

	fn admission(&self, auth: &auth::Auth) -> moq_auth::Request {
		auth::request_for(auth, self)
	}

	async fn expired(&mut self) {
		moq_tokio::server::Request::expired(self).await
	}

	async fn reject(self, code: moq_net::SessionError) {
		let _ = moq_tokio::server::Request::reject(self, code).await;
	}

	#[cfg(feature = "quic")]
	fn pacer(&self) -> Option<auth::Pacer> {
		let quic = moq_tokio::server::Request::quic_limits(self)?;
		Some(auth::Pacer::new(move |caps| {
			if let Err(err) = quic.set(caps.limits()) {
				tracing::warn!(%err, "failed to apply bitrate caps");
			}
		}))
	}

	#[cfg(feature = "quic")]
	fn with_quic_limits(self, limits: moq_quic::Limits) -> anyhow::Result<Self> {
		Ok(moq_tokio::server::Request::with_quic_limits(self, limits)?)
	}

	fn auth(&self) -> moq_net::auth::Handle {
		moq_tokio::server::Request::auth(self)
	}

	fn requests(&mut self) -> moq_net::auth::Requests {
		moq_tokio::server::Request::requests(self)
	}

	fn role(&self) -> Option<moq_net::setup::Role> {
		moq_tokio::server::Request::role(self)
	}

	fn peer_hop(&self) -> Option<std::num::NonZeroU64> {
		moq_tokio::server::Request::peer_hop(self)
	}

	fn with_stats(self, stats: moq_net::stats::Session) -> Self {
		moq_tokio::server::Request::with_stats(self, stats)
	}

	fn with_limits(self, limits: moq_net::session::Limits) -> Self {
		moq_tokio::server::Request::with_limits(self, limits)
	}

	fn with_publisher(self, publish: moq_net::origin::Consumer) -> Self {
		moq_tokio::server::Request::with_publisher(self, publish)
	}

	fn with_subscriber(self, subscribe: moq_net::origin::Producer) -> Self {
		moq_tokio::server::Request::with_subscriber(self, subscribe)
	}

	async fn ok(self) -> anyhow::Result<(moq_net::Session, Self::Driver)> {
		match moq_tokio::server::Request::ok(self).await {
			Ok(session) => Ok((session, ())),
			Err(moq_tokio::Error::MoqNet(err)) => Err(err.into()),
			Err(err) => Err(err.into()),
		}
	}

	async fn drive((): Self::Driver) -> anyhow::Result<()> {
		Ok(())
	}
}

#[cfg(feature = "test-support")]
impl Sealed for Accepted {
	/// Polled on the connection's own task, so it needs no runtime to spawn on.
	type Driver = moq_net::Driver;

	fn remote(&self) -> Option<std::net::SocketAddr> {
		None
	}

	fn transport(&self) -> &'static str {
		self.transport.as_str()
	}

	fn admission(&self, auth: &auth::Auth) -> moq_auth::Request {
		auth::request_for_setup(auth, &self.handshake, self.transport)
	}

	async fn expired(&mut self) {
		self.handshake.expired().await
	}

	async fn reject(self, code: moq_net::SessionError) {
		self.handshake.close(code.into());
	}

	fn auth(&self) -> moq_net::auth::Handle {
		self.handshake.auth()
	}

	fn requests(&mut self) -> moq_net::auth::Requests {
		self.handshake.requests()
	}

	fn role(&self) -> Option<moq_net::setup::Role> {
		self.handshake.role()
	}

	fn peer_hop(&self) -> Option<std::num::NonZeroU64> {
		self.handshake.peer_hop()
	}

	fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		self.handshake = self.handshake.with_stats(stats);
		self
	}

	fn with_limits(mut self, limits: moq_net::session::Limits) -> Self {
		self.handshake = self.handshake.with_limits(limits);
		self
	}

	fn with_publisher(mut self, publish: moq_net::origin::Consumer) -> Self {
		self.handshake = self.handshake.with_publisher(publish);
		self
	}

	fn with_subscriber(mut self, subscribe: moq_net::origin::Producer) -> Self {
		self.handshake = self.handshake.with_subscriber(subscribe);
		self
	}

	async fn ok(self) -> anyhow::Result<(moq_net::Session, Self::Driver)> {
		Ok(self.handshake.ok().await?)
	}

	async fn drive(driver: Self::Driver) -> anyhow::Result<()> {
		// The session's close reaches its supervisor, which reports it; the driver
		// ending is only the protocol stopping.
		moq_net::time::run(driver).await;
		Ok(())
	}
}
