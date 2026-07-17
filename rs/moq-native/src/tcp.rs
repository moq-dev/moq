//! Plain-TCP qmux transport, reachable via the `tcp://` URL scheme.
//!
//! Runs the QMux wire format directly over TCP with no TLS or WebSocket
//! framing. There is no transport encryption and no authentication, so only
//! use this on a trusted network (loopback, a private VPC interface, etc.).
//!
//! TCP has no TLS handshake, so the application protocol (the moq ALPN) is
//! negotiated in-band: pass the offered/supported protocols and the resulting
//! `qmux::Session::protocol()` is populated before connect/accept returns.

use std::net;
use url::Url;

/// The QMux wire-format version both ends speak over a raw stream. Fixed (not
/// negotiated) since there's no TLS ALPN to carry it.
const WIRE_VERSION: qmux::Version = qmux::Version::QMux01;

/// Plaintext-TCP qmux listener settings (no TLS, no UDP).
///
/// Flattened onto [`crate::ServerConfig::tcp`]. TCP carries no peer identity, so
/// the listener must only be reachable from trusted clients. Bind it to loopback
/// or a private interface; a non-loopback bind logs a warning but is allowed.
// The derived arg group is named after the struct, so it needs an explicit id to
// stay unique across the flattened sections.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[group(id = "server-tcp")]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Bind a plaintext qmux TCP listener on this address.
	#[arg(long = "server-tcp-bind", id = "server-tcp-bind", env = "MOQ_SERVER_TCP_BIND")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub bind: Option<net::SocketAddr>,
}

/// Errors specific to the plain-TCP qmux transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The TCP socket failed to bind, accept, or connect.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// The `tcp://` URL had no host.
	#[error("missing hostname")]
	MissingHostname,

	/// The `tcp://` URL had no port. Unlike `https`, there is no default.
	#[error("missing port")]
	MissingPort,

	/// The qmux handshake failed while dialing.
	#[error("qmux connect failed")]
	Connect(#[source] qmux::Error),

	/// The qmux handshake failed while accepting.
	#[error("qmux accept failed")]
	Accept(#[source] qmux::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// Dial a `tcp://host:port` URL, advertising `protocols` for in-band ALPN
/// negotiation. Returns a qmux session over plain TCP.
///
/// The port is required; there is no default for the `tcp` scheme.
pub(crate) async fn connect(url: Url, protocols: &[&str]) -> Result<qmux::Session> {
	let host = url.host_str().ok_or(Error::MissingHostname)?;
	let port = url.port().ok_or(Error::MissingPort)?;

	tracing::debug!(%url, "connecting via TCP");
	qmux::tcp::Config::new(WIRE_VERSION)
		.protocols(protocols.iter().copied())
		.connect((host, port))
		.await
		.map_err(Error::Connect)
}

/// Listens for incoming plain-TCP qmux connections on a TCP port.
pub struct Listener {
	listener: tokio::net::TcpListener,
	protocols: Vec<String>,
}

impl Listener {
	/// Bind a TCP listener to the given address.
	pub async fn bind(addr: net::SocketAddr) -> Result<Self> {
		let listener = tokio::net::TcpListener::bind(addr).await?;
		Ok(Self {
			listener,
			protocols: Vec::new(),
		})
	}

	/// Advertise these application protocols (moq ALPNs) for in-band negotiation,
	/// in preference order. The first server entry the client also offers wins.
	pub fn with_protocols<I, S>(mut self, protocols: I) -> Self
	where
		I: IntoIterator<Item = S>,
		S: Into<String>,
	{
		self.protocols = protocols.into_iter().map(Into::into).collect();
		self
	}

	/// The local address the listener is bound to.
	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	/// Accept the next connection, performing the qmux handshake over plain TCP.
	///
	/// Returns `None` only if the listener itself is gone; a per-connection
	/// failure is yielded as `Some(Err(..))` so the accept loop keeps running.
	pub async fn accept(&self) -> Option<Result<qmux::Session>> {
		match self.listener.accept().await {
			Ok((stream, addr)) => {
				tracing::debug!(%addr, "accepted TCP connection");
				let session = qmux::tcp::Config::new(WIRE_VERSION)
					.protocols(self.protocols.iter().map(String::as_str))
					.accept(stream)
					.await
					.map_err(Error::Accept);
				Some(session)
			}
			Err(e) => Some(Err(e.into())),
		}
	}
}
