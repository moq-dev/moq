//! Accepting: everything one incoming connection needs.

use super::{Connection, Error};
use crate::udp;

/// What to present, what to speak, and who may connect.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The TLS policy every handshake uses: identity, ALPNs, and client
	/// authentication, shareable with other listeners. It must allow TLS 1.3
	/// and name at least one ALPN.
	pub tls: std::sync::Arc<rustls::ServerConfig>,
	/// The per-connection transport settings (timeouts, stream limits,
	/// congestion control).
	pub transport: super::transport::Config,
}

impl Config {
	/// Serve `tls` with the default transport settings.
	pub fn new(tls: std::sync::Arc<rustls::ServerConfig>) -> Self {
		Self {
			tls,
			transport: super::transport::Config::default(),
		}
	}
}

/// Accept the next connection arriving on `socket`, driving the handshake to
/// completion.
///
/// Shorthand for an [`Endpoint`](super::Endpoint) serving this configuration
/// and one [`accept`](super::Endpoint::accept) from it: later arrivals on the
/// socket keep reaching the accepted connection, but nothing else is ever
/// accepted. Keep the endpoint itself for a listener.
pub async fn accept(socket: udp::Socket, config: &Config) -> Result<Connection, Error> {
	let endpoint = super::Endpoint::new(super::endpoint::Config::default().with_server(config.clone()), socket)?;
	endpoint.accept().await
}
