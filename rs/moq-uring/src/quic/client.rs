//! Dialing: everything one outgoing connection needs.

use std::net::SocketAddr;
use std::sync::Arc;

use super::{Connection, Error};
use crate::udp;

/// How to dial: the TLS policy and the transport settings.
///
/// Where to dial is an argument to [`connect`] and
/// [`Endpoint::connect`](super::Endpoint::connect), so one config serves every peer.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The TLS policy every dial uses: who to trust, any client certificate,
	/// and the ALPNs to offer. It must allow TLS 1.3 and name at least one ALPN.
	pub tls: Arc<rustls::ClientConfig>,
	/// The per-connection transport settings (timeouts, stream limits,
	/// congestion control).
	pub transport: super::transport::Config,
}

impl Config {
	/// Dial with `tls` and the default transport settings.
	pub fn new(tls: Arc<rustls::ClientConfig>) -> Self {
		Self {
			tls,
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
/// [`Connection`] with [`super::web::Session::raw`], then [`crate::transport::Local`], to
/// hand it to `moq_net::Client::connect`, or use the stream API directly.
pub async fn connect(
	socket: udp::Socket,
	config: &Config,
	remote: SocketAddr,
	server_name: &str,
) -> Result<Connection, Error> {
	let endpoint = super::Endpoint::new(super::endpoint::Config::default(), socket)?;
	endpoint.connect(config, remote, server_name).await
}
