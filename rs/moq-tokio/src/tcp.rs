//! Qmux over TCP: plaintext via the `tcp://` URL scheme, TLS via `tls://`.
//!
//! Both run the QMux wire format directly over TCP with no WebSocket framing.
//! Plaintext `tcp://` has no transport encryption and no authentication, so only
//! use it on a trusted network (loopback, a private VPC interface, etc.).
//! `tls://` encrypts and verifies the server with the same TLS settings as the
//! other transports, for links that need neither QUIC nor a WebSocket.
//!
//! Plaintext TCP has no TLS handshake, so the application protocol (the moq
//! ALPN) is negotiated in-band; TLS negotiates it in its handshake. Either way
//! `qmux::Session::protocol()` is populated before connect/accept returns.

use std::net;
use url::Url;

/// The QMux wire-format version both ends speak over a raw stream. Fixed (not
/// negotiated) since there's no TLS ALPN to carry it.
const WIRE_VERSION: qmux::Version = qmux::Version::QMux01;

/// Qmux TCP listener settings, plaintext or TLS (no UDP).
///
/// Flattened onto [`crate::listen::Config::tcp`]. Plaintext carries no peer
/// identity and no encryption, so bind it to loopback or a private interface; a
/// non-loopback plaintext bind logs a warning but is allowed. With
/// [`tls`](Self::tls) the listener serves the listen TLS certificate instead.
// The derived arg group is named after the struct, so it needs an explicit id to
// stay unique across the flattened sections.
#[derive(usage::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Bind a qmux TCP listener on this address, plaintext unless [`Self::tls`].
	#[usage(
		long = "listen-tcp-bind",
		name = "listen-tcp-bind",
		env = "MOQ_LISTEN_TCP_BIND",
		setting = "listen.tcp.bind"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub bind: Option<net::SocketAddr>,

	/// Serve TLS on the TCP listener with the listen certificate, for `tls://`
	/// dials. It asks for no client certificate, so a peer on it authenticates
	/// with a token rather than mTLS.
	#[usage(
		long = "listen-tcp-tls",
		name = "listen-tcp-tls",
		env = "MOQ_LISTEN_TCP_TLS",
		setting = "listen.tcp.tls",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tls: Option<bool>,

	/// The released `--server-tcp-bind` spelling and its env var, folded in by
	/// [`Config::resolved`].
	#[usage(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,
}

/// The `--server-tcp-*` flag from before the accept side was named `listen`.
///
/// A separate arg rather than a Usage alias, since an alias renames the flag but
/// leaves its env var behind.
#[derive(usage::Args, Clone, Debug, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub(crate) struct Legacy {
	#[usage(
		long = "server-tcp-bind",
		name = "server-tcp-bind",
		env = "MOQ_SERVER_TCP_BIND",
		hide = true
	)]
	bind: Option<net::SocketAddr>,
}

impl Config {
	/// The released spelling, if in use, paired with what replaced it. Reached
	/// through [`crate::listen::Config::deprecated`].
	pub(crate) fn deprecated(&self) -> crate::cli::Deprecated {
		let mut found = crate::cli::Deprecated::default();
		if self.legacy.bind.is_some() {
			found.flag(
				"--server-tcp-bind",
				Some("MOQ_SERVER_TCP_BIND"),
				"--listen-tcp-bind / MOQ_LISTEN_TCP_BIND",
			);
		}
		found
	}
}

/// Errors specific to the qmux TCP transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The TCP socket failed to bind or connect, or the host failed to resolve. Not
	/// accept: a failed `accept(2)` is the listener's own to classify and retry
	/// (see [`crate::accept`]).
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// The `tcp://` or `tls://` URL had no host.
	#[error("missing hostname")]
	MissingHostname,

	/// The `tcp://` or `tls://` URL had no port. Unlike `https`, there is no default.
	#[error("missing port")]
	MissingPort,

	/// The qmux handshake failed while dialing.
	#[error("qmux connect failed: {0}")]
	Connect(String),

	/// The qmux handshake failed while accepting.
	#[error("qmux accept failed: {0}")]
	Accept(String),

	/// DNS resolved the host to no addresses at all.
	#[error("no addresses resolved")]
	NoAddresses,

	/// Two or more addresses were raced and every attempt failed, each paired
	/// with its own error in dial order. All of them are kept: picking one to
	/// report would bury a refused port behind whichever address happened to be
	/// unroutable or to blackhole until its timeout. A host with a single address
	/// reports that error directly instead.
	#[error("all {} connection attempts failed: {}", .0.len(), crate::failover::describe(.0))]
	Failover(Vec<crate::failover::Attempt<Error>>),
}

impl crate::failover::Aggregate for Error {
	fn aggregate(failures: Vec<crate::failover::Attempt<Self>>) -> Self {
		Self::Failover(failures)
	}

	fn resolve(error: Option<std::io::Error>) -> Self {
		match error {
			Some(error) => Self::Io(error),
			None => Self::NoAddresses,
		}
	}
}

type Result<T> = std::result::Result<T, Error>;

/// Dial a `tcp://host:port` URL, advertising `protocols` for in-band ALPN
/// negotiation. Returns a qmux session over plain TCP.
///
/// The host is resolved alongside an IPv4-only lookup that answers without
/// waiting for its AAAA record, `resolution_delay` apart, and the answers raced
/// Happy Eyeballs style, staggered by `failover_delay` (see [`crate::failover`]).
///
/// The port is required; there is no default for the `tcp` scheme.
pub(crate) async fn connect(
	url: Url,
	protocols: &[&str],
	failover_delay: std::time::Duration,
	resolution_delay: std::time::Duration,
) -> Result<qmux::Session> {
	let host = url.host().ok_or(Error::MissingHostname)?;
	let port = url.port().ok_or(Error::MissingPort)?;

	tracing::debug!(peer = %crate::connect::Endpoint(&url), "connecting via TCP");
	let candidates = crate::resolve::Candidates::resolve(host, port, resolution_delay);
	connect_addrs(candidates, protocols, failover_delay).await
}

/// Dial a `tls://host:port` URL: qmux over TLS over TCP, negotiating one of
/// `protocols` as the TLS ALPN and verifying the server with `tls`.
///
/// Resolves and races addresses exactly like [`connect`], through the TLS and
/// qmux handshakes. The port is required.
pub(crate) async fn connect_tls(
	url: Url,
	protocols: &[&str],
	tls: &crate::tls::Connect,
	failover_delay: std::time::Duration,
	resolution_delay: std::time::Duration,
) -> crate::Result<qmux::Session> {
	let host = url.host().ok_or(Error::MissingHostname)?;
	let port = url.port().ok_or(Error::MissingPort)?;
	let name = tls.host_name.clone().unwrap_or_else(|| match &host {
		url::Host::Domain(name) => name.to_string(),
		url::Host::Ipv4(ip) => ip.to_string(),
		url::Host::Ipv6(ip) => ip.to_string(),
	});
	let client = qmux::tls::Client::new(std::sync::Arc::new(tls.build()?))
		.with_protocols(protocols.iter().map(|&alpn| (alpn, &[WIRE_VERSION][..])))
		.require_protocol();

	tracing::debug!(peer = %crate::connect::Endpoint(&url), "connecting via TLS over TCP");
	let candidates = crate::resolve::Candidates::resolve(host, port, resolution_delay);
	Ok(crate::failover::race(candidates, failover_delay, |addr| {
		let client = client.clone();
		let name = name.clone();
		async move {
			client
				.connect(addr, &name)
				.await
				.map_err(|err| Error::Connect(crate::error::message(err)))
		}
	})
	.await?)
}

/// Dial `candidates` in Happy Eyeballs order, performing the qmux handshake on
/// each attempt; the first session to complete wins.
async fn connect_addrs(
	candidates: crate::resolve::Candidates,
	protocols: &[&str],
	failover_delay: std::time::Duration,
) -> Result<qmux::Session> {
	crate::failover::race(candidates, failover_delay, |addr| {
		let protocols: Vec<String> = protocols.iter().map(|&p| p.to_owned()).collect();
		async move {
			qmux::tcp::Config::new(WIRE_VERSION)
				.protocols(protocols.iter().map(String::as_str))
				.connect(addr)
				.await
				.map_err(|err| Error::Connect(crate::error::message(err)))
		}
	})
	.await
}

/// Listens for incoming qmux connections on a TCP port, plaintext or TLS.
pub struct Listener {
	listener: tokio::net::TcpListener,
	protocols: Vec<String>,
	health: crate::accept::Health,
	/// Present when the listener serves TLS; it negotiates the ALPN itself.
	tls: Option<qmux::tls::Server>,
}

impl Listener {
	/// Bind a TCP listener to the given address.
	pub async fn bind(addr: net::SocketAddr) -> Result<Self> {
		let listener = tokio::net::TcpListener::bind(addr).await?;
		Ok(Self {
			listener,
			protocols: Vec::new(),
			health: crate::accept::Health::new("tcp"),
			tls: None,
		})
	}

	/// Serve TLS with `config`, whose ALPN list already names each
	/// `qmux-01.<protocol>` pair to accept.
	pub(crate) fn with_tls(mut self, config: std::sync::Arc<rustls::ServerConfig>) -> Self {
		self.tls = Some(qmux::tls::Server::new(config));
		self
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

	/// Accept the next connection, performing the TLS (when configured) and qmux
	/// handshakes.
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
		Some(self.accept_pending().await.await.map(|(session, _)| session))
	}

	/// Accept a socket and return its independently driven qmux handshake.
	pub(crate) async fn accept_pending(
		&self,
	) -> impl Future<Output = Result<(qmux::Session, net::SocketAddr)>> + use<> {
		let (stream, addr) = self.accept_socket().await;
		tracing::debug!(%addr, "accepted TCP connection");
		let config = qmux::tcp::Config::new(WIRE_VERSION).protocols(self.protocols.iter().map(String::as_str));
		let tls = self.tls.clone();
		async move {
			let session = match tls {
				Some(tls) => tls.accept(stream).await,
				None => config.accept(stream).await,
			}
			.map_err(|err| Error::Accept(crate::error::message(err)))?;
			Ok((session, addr))
		}
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

	/// TLS needs a TCP listener to serve on; asking for it alone is refused.
	#[test]
	fn tls_without_a_bind_is_refused() {
		let mut listen = crate::listen::Config::default();
		listen.tcp.tls = Some(true);
		assert!(matches!(
			listen.init(Default::default()),
			Err(crate::Error::NoBackend(_))
		));
	}

	/// A `tls://` dial carries qmux and the request target over TLS, and a client
	/// that does not trust the certificate is refused.
	#[cfg(feature = "aws-lc-rs")]
	#[tokio::test]
	async fn tls_carries_qmux_and_refuses_an_untrusted_certificate() {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let mut listen = crate::listen::Config::default();
		listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		listen.tcp.tls = Some(true);
		listen.tls.generate = vec!["localhost".into()];
		// A client CA meant for QUIC peers does not make the TCP listener ask for one.
		listen.tls.root = vec!["/nonexistent/client-ca.pem".into()];
		let mut server = listen.init(Default::default()).unwrap().listen().await.unwrap();
		let port = server.tcp_local_addr().unwrap().port();
		let url: Url = format!("tls://localhost:{port}/room?jwt=credential").parse().unwrap();
		let accept = tokio::spawn(async move {
			let request = server.accept().await.unwrap();
			assert_eq!(request.transport(), crate::server::Transport::Tcp);
			assert_eq!(request.path(), "/room");
			assert_eq!(request.query(), Some("jwt=credential"));
			let session = request.ok().await.unwrap();
			(server, session)
		});

		let dial = |insecure: bool| {
			let mut config = crate::connect::Config {
				once: Some(true),
				..Default::default()
			};
			config.tls.insecure = Some(insecure);
			config
				.init(Default::default())
				.unwrap()
				.with_reconnect(false)
				.connect(url.clone())
		};
		let untrusted = tokio::time::timeout(Duration::from_secs(5), dial(false).established()).await;
		assert!(matches!(untrusted, Ok(Err(_))), "an untrusted certificate must be refused");
		let _session = tokio::time::timeout(Duration::from_secs(5), dial(true).established())
			.await
			.expect("trusted dial timed out")
			.expect("trusted dial failed");
		let (_server, _accepted) = tokio::time::timeout(Duration::from_secs(5), accept)
			.await
			.expect("accept timed out")
			.expect("accept task panicked");
	}

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
		let candidates = crate::resolve::Candidates::fixed([blackhole, addr]);
		let session = tokio::time::timeout(
			Duration::from_secs(5),
			connect_addrs(candidates, &["moq-test"], Duration::from_millis(50)),
		)
		.await
		.expect("failover timed out")
		.expect("connect failed");

		assert_eq!(session.protocol(), Some("moq-test"));
		accept.await.expect("accept task panicked");
	}

	#[tokio::test]
	async fn connect_addrs_rejects_empty() {
		let candidates = crate::resolve::Candidates::fixed([]);
		let res = connect_addrs(candidates, &["moq-test"], Duration::ZERO).await;
		assert!(matches!(res, Err(Error::NoAddresses)));
	}
}

#[cfg(test)]
mod legacy_tests {
	use super::*;
	#[derive(usage::Cli)]
	#[usage(unknown_flags = "error", args_override_self = false)]
	#[usage(settings)]
	struct Cli {
		#[usage(flatten)]
		tcp: Config,
	}

	/// The released `--server-tcp-bind` is recognized and reported, never bound.
	#[test]
	fn released_spelling_is_reported_not_applied() {
		let config = Cli::parse_from(&[
			std::ffi::OsStr::new("--server-tcp-bind"),
			std::ffi::OsStr::new("127.0.0.1:4443"),
		])
		.unwrap()
		.tcp;
		assert_eq!(config.bind, None);

		let reported = config.deprecated().to_string();
		assert!(
			reported.contains("--server-tcp-bind / MOQ_SERVER_TCP_BIND -> --listen-tcp-bind / MOQ_LISTEN_TCP_BIND"),
			"{reported}"
		);

		let config = Cli::parse_from(&[
			std::ffi::OsStr::new("--listen-tcp-bind"),
			std::ffi::OsStr::new("127.0.0.1:1"),
		])
		.unwrap()
		.tcp;
		assert!(config.deprecated().is_empty());
		assert_eq!(config.bind, Some("127.0.0.1:1".parse().unwrap()));
	}
}
