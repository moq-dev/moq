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
	/// The TCP socket failed to bind or connect. Not accept: a failed `accept(2)` is
	/// the listener's own to classify and retry (see [`crate::accept`]).
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

	/// DNS resolved the host to no addresses at all.
	#[error("no addresses resolved")]
	NoAddresses,

	/// Two or more addresses were raced and every attempt failed, each paired
	/// with its own error in dial order. All of them are kept: picking one to
	/// report would bury a refused port behind whichever address happened to be
	/// unroutable or to blackhole until its timeout. A host with a single address
	/// reports that error directly instead.
	#[error("all {} connection attempts failed: {}", .0.len(), crate::failover::describe(.0))]
	Failover(Vec<crate::failover::Failure<Error>>),
}

impl crate::failover::Aggregate for Error {
	fn aggregate(failures: Vec<crate::failover::Failure<Self>>) -> Self {
		Self::Failover(failures)
	}
}

type Result<T> = std::result::Result<T, Error>;

/// Dial a `tcp://host:port` URL, advertising `protocols` for in-band ALPN
/// negotiation. Returns a qmux session over plain TCP.
///
/// When DNS returns multiple addresses they are raced Happy Eyeballs style,
/// staggered by `failover_delay` (see [`crate::failover`]).
///
/// The port is required; there is no default for the `tcp` scheme.
pub(crate) async fn connect(
	url: Url,
	protocols: &[&str],
	failover_delay: std::time::Duration,
) -> Result<qmux::Session> {
	let host = url.host_str().ok_or(Error::MissingHostname)?;
	let port = url.port().ok_or(Error::MissingPort)?;

	tracing::debug!(%url, "connecting via TCP");
	let addrs = tokio::net::lookup_host((host, port)).await?;
	connect_addrs(crate::failover::interleave(addrs), protocols, failover_delay).await
}

/// Dial the already-resolved `candidates` in Happy Eyeballs order, performing the
/// qmux handshake on each attempt; the first session to complete wins.
async fn connect_addrs(
	candidates: Vec<net::SocketAddr>,
	protocols: &[&str],
	failover_delay: std::time::Duration,
) -> Result<qmux::Session> {
	if candidates.is_empty() {
		return Err(Error::NoAddresses);
	}

	crate::failover::race(candidates, failover_delay, |addr| {
		let protocols: Vec<String> = protocols.iter().map(|&p| p.to_owned()).collect();
		async move {
			qmux::tcp::Config::new(WIRE_VERSION)
				.protocols(protocols.iter().map(String::as_str))
				.connect(addr)
				.await
				.map_err(Error::Connect)
		}
	})
	.await
}

/// Listens for incoming plain-TCP qmux connections on a TCP port.
pub struct Listener {
	listener: tokio::net::TcpListener,
	protocols: Vec<String>,
	health: crate::accept::Health,
}

impl Listener {
	/// Bind a TCP listener to the given address.
	pub async fn bind(addr: net::SocketAddr) -> Result<Self> {
		let listener = tokio::net::TcpListener::bind(addr).await?;
		Ok(Self {
			listener,
			protocols: Vec::new(),
			health: crate::accept::Health::new("tcp"),
		})
	}

	/// A live handle to this listener's accept-loop health, for an embedder that
	/// publishes it (see [`crate::accept`]).
	pub fn accept_health(&self) -> crate::accept::Health {
		self.health.clone()
	}

	/// Report into `health` instead of the one this listener made for itself.
	///
	/// For an owner that has to hand the handle out *before* the listener exists:
	/// [`crate::Server`] binds these lazily (they need a runtime), but an embedder
	/// registering them with a metrics endpoint does so at startup.
	pub fn with_accept_health(mut self, health: crate::accept::Health) -> Self {
		self.health = health;
		self
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
	/// A failed `accept(2)` is handled here rather than yielded: it is classified,
	/// counted, logged, and paced by [`accept_health`](Self::accept_health), then
	/// retried, because the caller has no better answer than to ask again. A
	/// per-connection *handshake* failure is still yielded as `Some(Err(..))`.
	///
	/// The `Option` no longer has a `None` case to report: nothing ends the accept
	/// loop, so this always yields. It stays because dropping it is a breaking change
	/// to a published signature.
	pub async fn accept(&self) -> Option<Result<qmux::Session>> {
		let (stream, addr) = self.accept_socket().await;
		tracing::debug!(%addr, "accepted TCP connection");
		let session = qmux::tcp::Config::new(WIRE_VERSION)
			.protocols(self.protocols.iter().map(String::as_str))
			.accept(stream)
			.await
			.map_err(Error::Accept);
		Some(session)
	}

	/// The `accept(2)` half: keep asking until a connection comes back.
	async fn accept_socket(&self) -> (tokio::net::TcpStream, net::SocketAddr) {
		loop {
			match self.listener.accept().await {
				Ok(accepted) => {
					self.health.accepted();
					return accepted;
				}
				Err(err) => {
					if let Some(delay) = self.health.failed(&err) {
						tokio::time::sleep(delay).await;
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;
	use web_transport_trait::Session as _;

	/// End-to-end failover: the preferred candidate blackholes (TEST-NET-1 never
	/// answers, or is unroutable outright in a sandbox), so the race must fall
	/// through to the loopback listener within the stagger delay.
	#[tokio::test]
	async fn failover_recovers_from_blackhole_candidate() {
		let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
			.await
			.expect("bind listener")
			.with_protocols(["moq-test"]);
		let addr = listener.local_addr().expect("local addr");

		let accept = tokio::spawn(async move { listener.accept().await.expect("listener gone").expect("accept") });

		let blackhole: net::SocketAddr = "192.0.2.1:9".parse().unwrap();
		let session = tokio::time::timeout(
			Duration::from_secs(5),
			connect_addrs(vec![blackhole, addr], &["moq-test"], Duration::from_millis(50)),
		)
		.await
		.expect("failover timed out")
		.expect("connect failed");

		assert_eq!(session.protocol(), Some("moq-test"));
		accept.await.expect("accept task panicked");
	}

	#[tokio::test]
	async fn connect_addrs_rejects_empty() {
		let res = connect_addrs(Vec::new(), &["moq-test"], Duration::ZERO).await;
		assert!(matches!(res, Err(Error::NoAddresses)));
	}
}
