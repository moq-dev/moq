//! Dialing: everything one outgoing connection needs.

use std::net::SocketAddr;

use super::{Connection, Error, Identity};
use crate::{Handle, udp};

/// Where to dial, as whom, and who to trust.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The address to dial.
	pub peer: SocketAddr,
	/// The name sent as SNI and verified against the server's certificate.
	pub server_name: String,
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
	pub transport: super::Transport,
}

impl Config {
	/// Dial `peer`, verifying it as `server_name`, with default settings.
	pub fn new(peer: SocketAddr, server_name: impl Into<String>) -> Self {
		Self {
			peer,
			server_name: server_name.into(),
			alpn: Vec::new(),
			verify: true,
			roots: Vec::new(),
			system_roots: true,
			identity: None,
			transport: super::Transport::default(),
		}
	}
}

/// Dial [`Config::peer`] over `socket`, driving the handshake to completion.
///
/// Shorthand for a dial-only [`Endpoint`](super::Endpoint) and one
/// [`connect`](super::Endpoint::connect) through it. The connection's driver
/// runs as a task on the worker behind `handle`, so the returned
/// [`Connection`] just works: hand it to `moq_net::Client::connect_lite` or
/// use the stream API directly.
pub async fn connect(handle: &Handle, socket: udp::Socket, config: &Config) -> Result<Connection, Error> {
	let endpoint = super::Endpoint::new(handle, socket, super::endpoint::Config::default())?;
	endpoint.connect(config).await
}
