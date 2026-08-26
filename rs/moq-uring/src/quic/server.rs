//! Accepting: everything one incoming connection needs.

use super::{Connection, Error, Identity, Trust};
use crate::{Handle, udp};

/// Whether connecting clients are asked for a certificate, and against what.
///
/// `SSL_VERIFY_PEER` on its own asks for a certificate and validates one that
/// arrives, but still admits a client that presents none; only
/// [`Required`](Self::Required) turns a missing certificate into a failed
/// handshake.
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

/// What to present, what to speak, and who may connect.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The certificate chain and key this server presents. Not optional: a
	/// QUIC server without an identity cannot complete a handshake.
	pub identity: Identity,
	/// ALPN protocols to accept, in preference order. Required: QUIC without
	/// an application protocol is refused.
	pub alpn: Vec<String>,
	/// Whether to ask connecting clients for a certificate.
	pub client_auth: ClientAuth,
	/// Close the connection after this long without activity.
	pub idle_timeout: std::time::Duration,
}

impl Config {
	/// Serve `identity`, speaking no ALPN and asking no client for a
	/// certificate until told otherwise.
	pub fn new(identity: Identity) -> Self {
		Self {
			identity,
			alpn: Vec::new(),
			client_auth: ClientAuth::default(),
			idle_timeout: std::time::Duration::from_secs(10),
		}
	}

	pub(crate) fn quiche(&self) -> Result<quiche::Config, Error> {
		use boring::ssl::SslVerifyMode;

		let (roots, verify) = match &self.client_auth {
			ClientAuth::None => (Vec::new(), SslVerifyMode::NONE),
			ClientAuth::Optional(roots) => (roots.clone(), SslVerifyMode::PEER),
			ClientAuth::Required(roots) => (roots.clone(), SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT),
		};
		// An empty store rejects every chain, so asking for a certificate with
		// nothing to check it against refuses exactly the clients that obey.
		if !matches!(self.client_auth, ClientAuth::None) && roots.is_empty() {
			return Err(Error::Tls(
				"client authentication needs at least one root certificate".to_string(),
			));
		}
		let trust = Trust {
			roots,
			// Client certificates chain to the roots configured here, never to
			// the platform store for public sites.
			system: false,
			verify,
		};
		super::tls(&self.alpn, Some(&self.identity), trust, self.idle_timeout)
	}
}

/// Accept the next connection arriving on `socket`, driving the handshake to
/// completion.
///
/// Shorthand for an [`Endpoint`](super::Endpoint) serving this configuration
/// and one [`accept`](super::Endpoint::accept) from it: later arrivals on the
/// socket keep reaching the accepted connection, but nothing else is ever
/// accepted. Keep the endpoint itself for a listener.
pub async fn accept(handle: &Handle, socket: udp::Socket, config: &Config) -> Result<Connection, Error> {
	let endpoint = super::Endpoint::new(
		handle,
		socket,
		super::endpoint::Config::default().with_server(config.clone()),
	)?;
	endpoint.accept().await
}
