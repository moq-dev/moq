use std::sync::Arc;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

use crate::transport::Stream;
use crate::{Config, Error, Session, Version, alpn};

/// A QMux client over TLS that advertises `(alpn, versions)` entries.
///
/// Each entry's `versions` slice is expanded into the TLS ALPN list as
/// `{v.prefix()}{alpn}` per version (empty = every QMux draft this crate knows
/// about). The bare version ALPNs (`qmux-02`, `qmux-01`, `qmux-00`) are
/// appended as well unless [`require_protocol`](Client::require_protocol) is
/// set, in which case only the prefixed pairs are offered. The peer's chosen
/// ALPN determines the QMux wire-format version; the `alpn_protocols` field on
/// the supplied `rustls::ClientConfig` is ignored and rebuilt from the entries.
#[derive(Clone)]
pub struct Client {
	config: Arc<rustls::ClientConfig>,
	protocols: Vec<(String, Vec<Version>)>,
	require_protocol: bool,
	session: Config,
}

impl Client {
	/// Start building a TLS client with the given `rustls::ClientConfig`.
	pub fn new(config: Arc<rustls::ClientConfig>) -> Self {
		Self {
			config,
			protocols: Vec::new(),
			require_protocol: false,
			session: Config::default(),
		}
	}

	/// Advertise `alpn` under the listed QMux wire-format versions.
	///
	/// Pass `&[v]` to pin to one version, `&[v1, v2]` to enumerate, or `&[]`
	/// to expand to every QMux draft this crate knows about.
	pub fn with_protocol(mut self, alpn: &str, versions: &[Version]) -> Self {
		self.protocols.push((alpn.to_string(), versions.to_vec()));
		self
	}

	/// Advertise multiple `(alpn, versions)` entries in preference order.
	pub fn with_protocols<'a>(mut self, entries: impl IntoIterator<Item = (&'a str, &'a [Version])>) -> Self {
		self.protocols
			.extend(entries.into_iter().map(|(a, vs)| (a.to_string(), vs.to_vec())));
		self
	}

	/// Offer only the prefixed `(alpn, version)` pairs, suppressing the bare
	/// version ALPNs (`qmux-02`, `qmux-01`, `qmux-00`) offered by default.
	pub fn require_protocol(mut self) -> Self {
		self.require_protocol = true;
		self
	}

	/// Use `config` for every session setting except the wire-format version
	/// and application protocol, which the handshake negotiates.
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Connect to `addr` and start a client session. `server_name` is used for
	/// SNI and certificate verification.
	pub async fn connect(&self, addr: impl ToSocketAddrs, server_name: &str) -> Result<Session, Error> {
		let stream = TcpStream::connect(&addr).await?;
		// Adopt the socket before TLS takes ownership; the kernel's view of the TCP
		// connection is the same either side of the encryption.
		let socket_stats = crate::tcp::adopt(&stream)?;

		let server_name = rustls::pki_types::ServerName::try_from(server_name)
			.map_err(|_| Error::InvalidServerName)?
			.to_owned();

		let entries = self.protocols.iter().map(|(a, vs)| (a.as_str(), vs.as_slice()));
		let prefixed = alpn::build(entries, self.require_protocol);

		let mut config = (*self.config).clone();
		config.alpn_protocols = prefixed.iter().map(|s| s.as_bytes().to_vec()).collect();

		tracing::debug!(?prefixed, "TLS connecting");

		let connector = TlsConnector::from(Arc::new(config));
		let tls_stream = connector.connect(server_name, stream).await?;

		let negotiated = tls_stream.get_ref().1.alpn_protocol();
		let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
		tracing::debug!(?negotiated_str, "TLS negotiated ALPN");

		let (version, protocol) = alpn::parse(negotiated_str)?;
		tracing::debug!(?version, ?protocol, "parsed ALPN");

		// In strict mode a bare version ALPN would silently drop the application
		// protocol we promised to enforce.
		if self.require_protocol && protocol.is_none() {
			return Err(Error::InvalidProtocol(negotiated_str.unwrap_or("<none>").to_string()));
		}

		let session_config = self.session.clone().with_negotiated(version, protocol);
		let mut transport = Stream::new(tls_stream);
		if let Some(stats) = socket_stats {
			transport = transport.with_socket_stats(stats);
		}
		// `connect` awaits the peer's transport parameters so `protocol()` is resolved.
		Session::connect(transport, session_config).await
	}
}

/// A QMux server that accepts TLS connections.
///
/// Selection of an offered ALPN happens inside rustls based on the
/// `alpn_protocols` already set on the supplied `rustls::ServerConfig`; the QMux
/// wire-format version is then derived from the negotiated ALPN. Callers
/// building that ALPN list should use `{version.prefix()}{alpn}` for each
/// supported pair (e.g. `"qmux-01.moq-lite-04"`).
#[derive(Clone)]
pub struct Server {
	config: Arc<rustls::ServerConfig>,
	session: Config,
}

impl Server {
	/// Create a TLS server from the given `rustls::ServerConfig`.
	pub fn new(config: Arc<rustls::ServerConfig>) -> Self {
		Self {
			config,
			session: Config::default(),
		}
	}

	/// Use `config` for every session setting except the wire-format version
	/// and application protocol, which the handshake negotiates.
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Accept a TLS connection over an established TCP stream, disabling Nagle on it.
	///
	/// This awaits both the TLS handshake and the QMux handshake (the peer's
	/// transport parameters, bounded by the session's
	/// [`handshake_timeout`](crate::Config::handshake_timeout)). Drive each
	/// connection with `tokio::spawn` so a slow or non-cooperative peer can't
	/// stall your `listener.accept()` loop.
	pub async fn accept(&self, stream: TcpStream) -> Result<Session, Error> {
		let socket_stats = crate::tcp::adopt(&stream)?;
		let acceptor = TlsAcceptor::from(self.config.clone());
		let tls_stream = acceptor.accept(stream).await?;

		let negotiated = tls_stream.get_ref().1.alpn_protocol();
		let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
		tracing::debug!(?negotiated_str, "TLS accepted, negotiated ALPN");

		let (version, protocol) = alpn::parse(negotiated_str)?;
		tracing::debug!(?version, ?protocol, "parsed ALPN");

		let session_config = self.session.clone().with_negotiated(version, protocol);
		let mut transport = Stream::new(tls_stream);
		if let Some(stats) = socket_stats {
			transport = transport.with_socket_stats(stats);
		}
		// `accept` awaits the peer's transport parameters so `protocol()` is resolved.
		Session::accept(transport, session_config).await
	}
}
