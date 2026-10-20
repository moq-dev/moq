//! The sessions a [`Connection`](crate::Connection) serves, whatever transport carries them.

use crate::auth;
use axum::http;
use moq_net::transport::poll;

/// A session whose SETUP awaits the relay's reply: what a [`Connection`](crate::Connection) serves.
///
/// Sealed: implemented by [`moq_tokio::server::Request`] and [`Accepted`].
pub trait Incoming: Sealed {}

impl Incoming for moq_tokio::server::Request {}

impl<S: poll::Boxable> Incoming for Accepted<S> {}

/// A session accepted on any moq-net transport, its SETUP awaiting the relay's reply.
///
/// The auth decider sees the path, query, token, and role the SETUP carried, so the
/// client must send its path in-band.
pub struct Accepted<S: poll::Session> {
	handshake: moq_net::server::Handshake<S>,
	transport: moq_auth::Transport,
}

impl<S: poll::Session> Accepted<S> {
	/// Wrap `handshake`, reported to the auth decider as arriving over `transport`.
	pub fn new(handshake: moq_net::server::Handshake<S>, transport: moq_auth::Transport) -> Self {
		Self { handshake, transport }
	}
}

/// What a [`Connection`](crate::Connection) needs from its request.
///
/// Public in a private module, so [`Incoming`] can name it while nothing outside
/// this crate can implement it.
#[allow(async_fn_in_trait)]
pub trait Sealed: Sized {
	/// The running session's protocol driver.
	type Driver;

	/// The peer's socket address, for the connection's log span.
	fn remote(&self) -> Option<std::net::SocketAddr>;

	/// The transport, for the line logged once the session is negotiated.
	fn transport(&self) -> impl std::fmt::Display + use<Self>;

	/// What the auth decider is asked about this session.
	fn admission(&self, auth: &auth::Auth) -> moq_auth::Request;

	/// Refuse the session with the admission's HTTP-style status.
	async fn reject(self, status: http::StatusCode);

	/// The QUIC connection under the request, to widen a peer's limits before SETUP OK.
	#[cfg(feature = "quic")]
	fn quic(&self) -> Option<&moq_tokio::quic::Connection> {
		None
	}

	fn auth(&self) -> moq_net::auth::Handle;
	fn role(&self) -> Option<moq_net::Role>;
	fn with_stats(self, stats: moq_net::stats::Session) -> Self;
	fn with_limits(self, limits: moq_net::session::Limits) -> Self;
	fn with_publisher(self, publish: moq_net::origin::Consumer) -> Self;
	fn with_subscriber(self, subscribe: moq_net::origin::Producer) -> Self;

	/// Answer the SETUP. A MoQ failure comes back as a bare [`moq_net::Error`], so
	/// its cause classifies the failure.
	async fn ok(self) -> anyhow::Result<(moq_net::Session, Self::Driver)>;

	/// Run the session's protocol until it ends, alongside its supervision.
	async fn drive(driver: Self::Driver) -> anyhow::Result<()>;
}

impl Sealed for moq_tokio::server::Request {
	/// Spawned by [`Request::ok`](moq_tokio::server::Request::ok) on its own task.
	type Driver = tokio::task::JoinHandle<moq_tokio::Result<()>>;

	fn remote(&self) -> Option<std::net::SocketAddr> {
		self.remote_addr()
	}

	fn transport(&self) -> impl std::fmt::Display + use<> {
		moq_tokio::server::Request::transport(self)
	}

	fn admission(&self, auth: &auth::Auth) -> moq_auth::Request {
		auth::request_for(auth, self)
	}

	async fn reject(self, status: http::StatusCode) {
		let reject = match status {
			http::StatusCode::UNAUTHORIZED => moq_tokio::server::Reject::Unauthorized,
			http::StatusCode::FORBIDDEN => moq_tokio::server::Reject::Forbidden,
			status => moq_tokio::server::Reject::App(status.as_u16()),
		};
		let _ = moq_tokio::server::Request::reject(self, reject).await;
	}

	#[cfg(feature = "quic")]
	fn quic(&self) -> Option<&moq_tokio::quic::Connection> {
		moq_tokio::server::Request::quic(self)
	}

	fn auth(&self) -> moq_net::auth::Handle {
		moq_tokio::server::Request::auth(self)
	}

	fn role(&self) -> Option<moq_net::Role> {
		moq_tokio::server::Request::role(self)
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
			Ok(ok) => Ok(ok),
			Err(moq_tokio::Error::MoqNet(err)) => Err(err.into()),
			Err(err) => Err(err.into()),
		}
	}

	async fn drive(driver: Self::Driver) -> anyhow::Result<()> {
		Ok(driver.await??)
	}
}

impl<S: poll::Boxable> Sealed for Accepted<S> {
	/// Polled on the connection's own task, so it needs no runtime to spawn on.
	type Driver = moq_net::Driver<S>;

	fn remote(&self) -> Option<std::net::SocketAddr> {
		None
	}

	fn transport(&self) -> impl std::fmt::Display + use<S> {
		self.transport.as_str()
	}

	fn admission(&self, auth: &auth::Auth) -> moq_auth::Request {
		auth::request_for_setup(auth, &self.handshake, self.transport)
	}

	async fn reject(self, status: http::StatusCode) {
		self.handshake.close(match status {
			http::StatusCode::UNAUTHORIZED | http::StatusCode::FORBIDDEN => moq_net::Error::Unauthorized,
			status => moq_net::Error::App(status.as_u16()),
		});
	}

	fn auth(&self) -> moq_net::auth::Handle {
		self.handshake.auth()
	}

	fn role(&self) -> Option<moq_net::Role> {
		self.handshake.role()
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
