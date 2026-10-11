//! Accepting: everything one incoming connection needs.

use super::{Connection, Error, Identity};
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

/// The TLS material used for new server handshakes.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Tls {
	/// A fixed PEM identity, using the config's ALPN and client-auth settings.
	Pem(Identity),
	/// A resolved rustls policy, including its ALPNs and shared certificate resolver.
	Rustls(std::sync::Arc<rustls::ServerConfig>),
}

impl From<Identity> for Tls {
	fn from(identity: Identity) -> Self {
		Self::Pem(identity)
	}
}

impl From<std::sync::Arc<rustls::ServerConfig>> for Tls {
	fn from(config: std::sync::Arc<rustls::ServerConfig>) -> Self {
		Self::Rustls(config)
	}
}

/// What to present, what to speak, and who may connect.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The fixed identity or resolved TLS policy used by every handshake.
	pub tls: Tls,
	/// ALPN protocols for a PEM identity; leave empty for a resolved TLS policy.
	/// Required for PEM: QUIC without
	/// an application protocol is refused.
	pub alpn: Vec<String>,
	/// Client authentication for a PEM identity; leave unset for a resolved TLS policy.
	pub client_auth: ClientAuth,
	/// The per-connection transport settings (timeouts, stream limits,
	/// congestion control).
	pub transport: super::Transport,
}

impl Config {
	/// Serve fixed PEM material or a resolved TLS policy shared with other listeners.
	pub fn new(tls: impl Into<Tls>) -> Self {
		Self {
			tls: tls.into(),
			alpn: Vec::new(),
			client_auth: ClientAuth::default(),
			transport: super::Transport::default(),
		}
	}

	/// Refuse a configuration no client could satisfy.
	///
	/// An empty store rejects every chain, so asking for a certificate with
	/// nothing to check it against refuses exactly the clients that obey.
	pub(crate) fn check(&self) -> Result<(), Error> {
		if matches!(self.tls, Tls::Rustls(_))
			&& (!self.alpn.is_empty() || !matches!(self.client_auth, ClientAuth::None))
		{
			return Err(Error::Tls(
				"a resolved TLS policy already owns ALPN and client authentication".into(),
			));
		}
		if self.client_auth.roots().is_some_and(|(roots, _)| roots.is_empty()) {
			return Err(Error::Tls(
				"client authentication needs at least one root certificate".to_string(),
			));
		}
		Ok(())
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
