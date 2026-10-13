//! Accepting: everything one incoming connection needs.

use super::{Connection, Error};
use crate::udp;

/// Whether connecting clients are asked for a certificate, and against what.
///
/// Asking for a certificate and validating one that arrives still admits a
/// client that presents none; only [`Required`](Self::Required) turns a
/// missing certificate into a failed handshake.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum ClientAuth {
	/// Don't ask for a client certificate.
	#[default]
	None,
	/// Ask, and verify against these roots if one is presented. A client that
	/// presents none is still accepted.
	Optional(Vec<std::path::PathBuf>),
	/// Require every client to present a certificate chaining to these roots.
	Required(Vec<std::path::PathBuf>),
}

impl ClientAuth {
	/// The roots a presented certificate is checked against, and whether one
	/// is mandatory.
	pub(crate) fn roots(&self) -> Option<(&[std::path::PathBuf], bool)> {
		match self {
			Self::None => None,
			Self::Optional(roots) => Some((roots, false)),
			Self::Required(roots) => Some((roots, true)),
		}
	}
}

/// What to present, what to speak, and who may connect.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The TLS policy every handshake uses: identity, ALPNs, and client
	/// authentication. Build one from an [`Identity`](super::Identity) with
	/// [`Identity::server`](super::Identity::server), or share a resolver with
	/// other listeners. It must allow TLS 1.3 and name at least one ALPN.
	pub tls: std::sync::Arc<rustls::ServerConfig>,
	/// The per-connection transport settings (timeouts, stream limits,
	/// congestion control).
	pub transport: super::Transport,
}

impl Config {
	/// Serve `tls` with the default transport settings.
	pub fn new(tls: std::sync::Arc<rustls::ServerConfig>) -> Self {
		Self {
			tls,
			transport: super::Transport::default(),
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
	let endpoint = super::Endpoint::new(socket, super::endpoint::Config::default().with_server(config.clone()))?;
	endpoint.accept().await
}
