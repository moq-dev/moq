//! Dialing: everything one outgoing connection needs.

use std::net::SocketAddr;

use super::{Connection, Error, Identity};
use crate::udp;

/// How to dial: as whom, and who to trust.
///
/// Where to dial is an argument to [`connect`] and
/// [`Endpoint::connect`](super::Endpoint::connect), so one config serves every peer.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// ALPN protocols to offer, in preference order. Required: QUIC without an
	/// application protocol is refused.
	pub alpn: Vec<String>,
	/// Verify the server's certificate against the roots below (default).
	/// Turn it off only for tests or a pinned local deployment.
	pub verify: bool,
	/// PEM root certificate files to trust, on top of the system store.
	pub roots: Vec<std::path::PathBuf>,
	/// Trust the platform's root store as well, on by default. Turn it off to
	/// trust only [`roots`](Self::roots), which is a real restriction here:
	/// the trust store is built from scratch rather than added to.
	pub system_roots: bool,
	/// A certificate to present when the server asks for one (mTLS).
	pub identity: Option<Identity>,
	/// The per-connection transport settings (timeouts, stream limits,
	/// congestion control).
	pub transport: super::transport::Config,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			alpn: Vec::new(),
			verify: true,
			roots: Vec::new(),
			system_roots: true,
			identity: None,
			transport: super::transport::Config::default(),
		}
	}
}

/// Dial `remote` over `socket`, verifying it as `server_name`, and drive the
/// handshake to completion.
///
/// Shorthand for a dial-only [`Endpoint`](super::Endpoint) and one
/// [`connect`](super::Endpoint::connect) through it. The connection's driver
/// runs as a task on the worker that adopted `socket`. Wrap the returned
/// [`Connection`] with [`crate::transport::Session::new`] and
/// [`moq_net::transport::Address::Setup`], then [`crate::transport::Local`], to
/// hand it to `moq_net::Client::connect`, or use the stream API directly.
pub async fn connect(
	socket: udp::Socket,
	config: &Config,
	remote: SocketAddr,
	server_name: &str,
) -> Result<Connection, Error> {
	let endpoint = super::Endpoint::new(socket, super::endpoint::Config::default())?;
	endpoint.connect(config, remote, server_name).await
}
