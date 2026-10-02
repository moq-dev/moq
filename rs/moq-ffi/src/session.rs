use std::sync::Arc;

use url::Url;

use crate::bandwidth::MoqBandwidth;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::{MoqOriginConsumer, MoqOriginProducer};

/// Configuration for [`MoqClient::new`], mirroring moq-tokio's client config.
///
/// Every field has a default, so set only what you need. The browser owns the socket and the
/// trust store, so a wasm build honors only `versions`, `tls.fingerprints`, and the origins,
/// always dials once, and fails `new` with `Unsupported` when anything else is set.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqClientConfig {
	/// Local UDP address to bind, e.g. `0.0.0.0:0`. Null binds an ephemeral dual-stack port.
	#[uniffi(default = None)]
	pub bind: Option<String>,
	/// Protocol versions to offer, most preferred first, spelled like `moq-lite-03`.
	/// Empty offers every supported version.
	#[uniffi(default = [])]
	pub versions: Vec<String>,
	/// Certificate trust and the mTLS identity.
	#[uniffi(default)]
	pub tls: MoqClientTls,
	/// QUIC transport tuning.
	#[uniffi(default)]
	pub quic: MoqQuicConfig,
	/// The WebSocket fallback, raced against QUIC for networks that block UDP.
	#[uniffi(default)]
	pub websocket: MoqWebSocketConfig,
	/// Dial once instead of redialing with backoff whenever the transport drops.
	///
	/// With this set, the transport's close ends the session (surfaced via
	/// [`MoqSession::closed`]).
	#[uniffi(default = false)]
	pub once: bool,
	/// Retry pacing for the automatic reconnect.
	#[uniffi(default)]
	pub backoff: MoqBackoff,
	/// The origin whose broadcasts are published to the remote.
	///
	/// With neither `publish` nor `consume` set, each session's two sides share one fresh
	/// origin, so a broadcast announced on it is also discoverable through it. Setting either
	/// opts out of that and gives the other side its own fresh origin.
	#[uniffi(default = None)]
	pub publish: Option<Arc<MoqOriginProducer>>,
	/// The origin that receives broadcasts consumed from the remote. See `publish`.
	#[uniffi(default = None)]
	pub consume: Option<Arc<MoqOriginProducer>>,
}

/// Certificate trust and the mTLS identity for a [`MoqClientConfig`].
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqClientTls {
	/// Skip certificate verification. Local development only.
	#[uniffi(default = false)]
	pub insecure: bool,
	/// PEM root certificate files to trust instead of the platform roots.
	#[uniffi(default = [])]
	pub roots: Vec<String>,
	/// Whether to also trust the platform roots. Null trusts them only when `roots` is empty.
	#[uniffi(default = None)]
	pub system_roots: Option<bool>,
	/// SHA-256 certificate fingerprints, hex-encoded, to pin the peer to.
	///
	/// The native equivalent of WebTransport's `serverCertificateHashes`, accepting what
	/// `MoqServer.cert_fingerprints` reports, so a self-signed certificate is trusted without
	/// disabling verification.
	#[uniffi(default = [])]
	pub fingerprints: Vec<String>,
	/// PEM certificate chain to present when the relay requires mTLS. Pair with `key`.
	#[uniffi(default = None)]
	pub cert: Option<String>,
	/// PEM private key to present when the relay requires mTLS. Pair with `cert`.
	#[uniffi(default = None)]
	pub key: Option<String>,
}

/// QUIC transport tuning, mirroring moq-tokio's QUIC config.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqQuicConfig {
	/// Cap on the concurrent QUIC streams the peer may open toward this endpoint. Null uses 1024.
	///
	/// MoQ opens a stream per group, and for a subscriber those arrive from the peer, so an
	/// endpoint subscribing to many tracks wants this raised. Ignored by the WebSocket fallback.
	#[uniffi(default = None)]
	pub max_streams: Option<u64>,
}

/// The WebSocket fallback for a [`MoqClientConfig`], raced against QUIC for `http(s)` URLs.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqWebSocketConfig {
	/// Whether the fallback runs. Null enables it; disable it for a relay that only serves
	/// QUIC, so a failed QUIC dial reports its own error.
	#[uniffi(default = None)]
	pub enabled: Option<bool>,
	/// Head start QUIC gets before the fallback joins the race, in microseconds. Null uses
	/// 200ms, and 0 races both at once.
	#[uniffi(default = None)]
	pub delay_us: Option<u64>,
}

/// Retry pacing for the automatic reconnect.
///
/// The delay starts at `initial_us`, multiplies by `multiplier` after each failed attempt,
/// and caps at `max_us`. After `timeout_us` of consecutive failures the connection gives up
/// for good; the window resets whenever a session stays up past `initial_us`. Each null
/// field uses moq-tokio's default: 1s, x2, 5s, and a 10s window.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqBackoff {
	/// Delay before the first reconnect attempt, in microseconds.
	#[uniffi(default = None)]
	pub initial_us: Option<u64>,
	/// Multiplier applied to the delay after each failure.
	#[uniffi(default = None)]
	pub multiplier: Option<u32>,
	/// Maximum delay between reconnect attempts, in microseconds.
	#[uniffi(default = None)]
	pub max_us: Option<u64>,
	/// Time spent retrying before giving up, in microseconds. 0 retries forever.
	#[uniffi(default = None)]
	pub timeout_us: Option<u64>,
}

/// Parse protocol version names, as `MoqClientConfig::versions` and
/// `MoqServerConfig::versions` spell them.
pub(crate) fn parse_versions(versions: &[String]) -> Result<Vec<moq_net::Version>, MoqError> {
	versions
		.iter()
		.map(|version| version.parse().map_err(MoqError::Config))
		.collect()
}

/// Native QUIC/WebTransport client state: the configured endpoint and the wired origins.
#[cfg(not(target_arch = "wasm32"))]
struct Client {
	client: moq_tokio::Client,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Client {
	fn new(config: MoqClientConfig) -> Result<Self, MoqError> {
		let _guard = crate::ffi::enter();

		let mut connect = moq_tokio::connect::Config::default();
		if let Some(bind) = config.bind {
			let bind = bind
				.parse()
				.map_err(|err| MoqError::Config(format!("invalid bind address {bind}: {err}")))?;
			connect.bind = Some(bind);
		}
		connect.version = parse_versions(&config.versions)?;

		let tls = config.tls;
		connect.tls.insecure = Some(tls.insecure);
		connect.tls.root = tls.roots.into_iter().map(Into::into).collect();
		connect.tls.system_roots = tls.system_roots;
		connect.tls.fingerprint = tls.fingerprints;
		connect.tls.cert = tls.cert.map(Into::into);
		connect.tls.key = tls.key.map(Into::into);

		connect.websocket.enabled = config.websocket.enabled;
		if let Some(delay) = config.websocket.delay_us {
			connect.websocket.delay = std::time::Duration::from_micros(delay);
		}

		connect.once = Some(config.once);

		let backoff = config.backoff;
		if let Some(initial) = backoff.initial_us {
			connect.backoff.initial = std::time::Duration::from_micros(initial);
		}
		if let Some(multiplier) = backoff.multiplier {
			connect.backoff.multiplier = multiplier;
		}
		if let Some(max) = backoff.max_us {
			connect.backoff.max = std::time::Duration::from_micros(max);
		}
		if let Some(timeout) = backoff.timeout_us {
			connect.backoff.timeout = std::time::Duration::from_micros(timeout);
		}

		let mut quic = moq_tokio::quic::Config::default();
		quic.max_streams = config.quic.max_streams;

		// Building the endpoint here, not at connect, is what surfaces an unreadable
		// certificate or a half-configured mTLS identity from `new`.
		let client = connect.init(quic).map_err(|err| MoqError::Config(format!("{err}")))?;

		Ok(Self {
			client,
			publish: config.publish,
			consume: config.consume,
		})
	}

	async fn connect(&self, url: Url) -> Result<Arc<MoqSession>, MoqError> {
		// Materialize both origin sides so the session can publish/subscribe and the FFI can
		// always hand back a publish/consume origin.
		let (publish, subscribe) = crate::origin::resolve_pair(self.publish.as_ref(), self.consume.as_ref());

		let connection = self
			.client
			.clone()
			.with_publisher(&publish)
			.with_subscriber(subscribe.clone())
			.connect(url);

		// Wait for the first session so auth errors surface here; later drops are the
		// connection's to ride out. `MoqClient::cancel` unblocks a dial stuck retrying.
		// `established` hands the connection back, which is what keeps it reconnecting.
		let connection = connection.established().await.map_err(map_connect_error)?;

		Ok(Arc::new(MoqSession::connected(connection, publish, subscribe)))
	}
}

#[cfg(not(target_arch = "wasm32"))]
fn map_connect_error(err: moq_tokio::Error) -> MoqError {
	match err {
		moq_tokio::Error::MoqNet(err) => err.into(),
		err => match err.connect_error() {
			Some(moq_tokio::ConnectError::Unauthorized) => MoqError::Unauthorized,
			Some(moq_tokio::ConnectError::Forbidden) => MoqError::Forbidden,
			_ => MoqError::Connect(format!("{err}")),
		},
	}
}

/// The terminal error of a connection that had already established a session.
///
/// Same auth rejections as [`map_connect_error`], but a session that ended carries
/// a [`moq_net::Error`], and stringifying that into `Connect` would both lose the
/// variant a caller can match on and claim the dial failed when it had succeeded.
/// A server-accepted session reports its close the same way.
///
/// A local stop is not a failure at all, so it maps to `Closed`, which is what the
/// bindings' `is_shutdown` recognizes. `status()` reaches this path whenever
/// another handle shuts the connection down underneath it, and reporting that as a
/// connect error would make every status watcher treat an expected teardown as a
/// broken connection.
#[cfg(not(target_arch = "wasm32"))]
fn map_closed_error(err: moq_tokio::Error) -> MoqError {
	match err {
		moq_tokio::Error::Stopped => MoqError::Closed,
		// A peer's session/stream code stays structured. HTTP 401/403 are a
		// connect-time rejection, not a protocol code, and land below.
		moq_tokio::Error::MoqNet(err) => err.into(),
		err => match err.connect_error() {
			Some(moq_tokio::ConnectError::Unauthorized) => MoqError::Unauthorized,
			Some(moq_tokio::ConnectError::Forbidden) => MoqError::Forbidden,
			_ => MoqError::Connect(format!("{err}")),
		},
	}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
	use super::*;

	const VALID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

	#[test]
	fn decodes_a_sha256_fingerprint() {
		let bytes = decode_hex(VALID).unwrap();
		assert_eq!(bytes.len(), 32);
		assert_eq!(bytes[0], 0x01);

		// Colons are the other shape `MoqServer::cert_fingerprints` and openssl print.
		let colons = VALID
			.as_bytes()
			.chunks(2)
			.map(|c| std::str::from_utf8(c).unwrap())
			.collect::<Vec<_>>()
			.join(":");
		assert_eq!(decode_hex(&colons).unwrap(), bytes);
	}

	/// Byte-index slicing used to land inside a multi-byte character and panic, which an
	/// FFI caller could reach with any non-ASCII string of even byte length.
	#[test]
	fn rejects_non_ascii_instead_of_panicking() {
		for input in ["aéa", "é", "ééééééééééééééééééééééééééééééé"] {
			assert!(matches!(decode_hex(input), Err(MoqError::Config(_))), "{input}");
		}
	}

	#[test]
	fn rejects_a_wrong_length_fingerprint() {
		assert!(decode_hex("").is_err());
		assert!(decode_hex("abcd").is_err());
		assert!(decode_hex(&VALID[..62]).is_err());
		assert!(decode_hex(&format!("{VALID}ab")).is_err());
	}

	#[test]
	fn rejects_non_hex_digits() {
		for bad in [
			format!("zz{}", &VALID[2..]),
			// `u8::from_str_radix` accepts a leading sign, so an unchecked chunk would
			// decode "+a" to 10 and silently yield a fingerprint matching no certificate.
			format!("+0{}", &VALID[2..]),
			format!("{}+0", &VALID[..62]),
			format!(" 0{}", &VALID[2..]),
		] {
			assert!(matches!(decode_hex(&bad), Err(MoqError::Config(_))), "{bad}");
		}
	}

	#[test]
	fn maps_native_auth_connect_errors() {
		assert!(matches!(
			map_connect_error(moq_tokio::ConnectError::Unauthorized.into()),
			MoqError::Unauthorized
		));
		assert!(matches!(
			map_connect_error(moq_tokio::ConnectError::Forbidden.into()),
			MoqError::Forbidden
		));
	}

	/// A session that ended must report the close, not a dial failure. Auth
	/// rejections keep their dedicated variants, which is what `is_auth` reads in
	/// every binding, but everything else keeps the `moq_net::Error` a caller can
	/// match on instead of being flattened into a `Connect` string.
	#[test]
	fn maps_closed_errors_without_flattening_the_reason() {
		match map_closed_error(moq_net::Error::from(moq_net::SessionError::Unknown(7)).into()) {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.scope, crate::error::MoqErrorScope::Session);
				assert_eq!(protocol.code, 7);
				assert_eq!(protocol.kind, crate::error::MoqProtocolKind::Unknown);
			}
			other => panic!("expected Protocol Unknown(7), got {other:?}"),
		}
		assert!(matches!(
			map_closed_error(moq_net::Error::Cancel.into()),
			MoqError::Cancelled
		));

		// A local stop is an expected teardown, not a failed connection: the bindings'
		// `is_shutdown` reads `Closed`, and `Connect` would read as a broken dial.
		assert!(matches!(map_closed_error(moq_tokio::Error::Stopped), MoqError::Closed));

		// HTTP auth still wins. A protocol Unauthorized is a structured Protocol error
		// (kind Unauthorized), not this HTTP 401 variant.
		assert!(matches!(
			map_closed_error(moq_tokio::ConnectError::Unauthorized.into()),
			MoqError::Unauthorized
		));
		match map_closed_error(moq_net::Error::from(moq_net::SessionError::Unauthorized).into()) {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.kind, crate::error::MoqProtocolKind::Unauthorized);
				assert_eq!(protocol.code, 0x2);
			}
			other => panic!("expected Protocol Unauthorized, got {other:?}"),
		}
		assert!(matches!(
			map_closed_error(moq_tokio::ConnectError::Forbidden.into()),
			MoqError::Forbidden
		));
	}

	/// `new` is where a bad value surfaces, rather than at the first connect.
	#[test]
	fn new_rejects_invalid_config() {
		let bind = MoqClientConfig {
			bind: Some("not-an-address".into()),
			..Default::default()
		};
		assert!(matches!(MoqClient::new(bind), Err(MoqError::Config(_))));

		let version = MoqClientConfig {
			versions: vec!["moq-lite-99".into()],
			..Default::default()
		};
		assert!(matches!(MoqClient::new(version), Err(MoqError::Config(_))));

		let missing = MoqClientConfig {
			tls: MoqClientTls {
				roots: vec!["/nonexistent/root.pem".into()],
				..Default::default()
			},
			..Default::default()
		};
		assert!(matches!(MoqClient::new(missing), Err(MoqError::Config(_))));
	}

	#[test]
	fn new_accepts_the_defaults_and_known_versions() {
		MoqClient::new(MoqClientConfig::default()).unwrap();
		MoqClient::new(MoqClientConfig {
			bind: Some("127.0.0.1:0".into()),
			versions: vec!["moq-lite-03".into()],
			once: true,
			backoff: MoqBackoff {
				timeout_us: Some(0),
				..Default::default()
			},
			..Default::default()
		})
		.unwrap();
	}
}

/// Browser WebTransport client state.
///
/// The browser owns the socket and the trust store, so none of the native TLS knobs
/// (roots, mTLS, bind address) have an equivalent. Certificate hashes are the one
/// thing WebTransport does expose.
#[cfg(target_arch = "wasm32")]
struct Client {
	fingerprints: Vec<Vec<u8>>,
	versions: Vec<moq_net::Version>,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

#[cfg(target_arch = "wasm32")]
impl Client {
	fn new(config: MoqClientConfig) -> Result<Self, MoqError> {
		let MoqClientConfig {
			bind,
			versions,
			tls,
			quic,
			websocket,
			// A browser session never redials, so it dials once either way.
			once: _,
			backoff,
			publish,
			consume,
		} = config;

		let native_tls = MoqClientTls {
			fingerprints: Vec::new(),
			..tls.clone()
		};
		if bind.is_some()
			|| native_tls != MoqClientTls::default()
			|| quic != MoqQuicConfig::default()
			|| websocket != MoqWebSocketConfig::default()
			|| backoff != MoqBackoff::default()
		{
			return Err(MoqError::Unsupported);
		}

		Ok(Self {
			fingerprints: tls
				.fingerprints
				.iter()
				.map(|hex| decode_hex(hex))
				.collect::<Result<_, _>>()?,
			versions: parse_versions(&versions)?,
			publish,
			consume,
		})
	}

	async fn connect(&self, url: Url) -> Result<Arc<MoqSession>, MoqError> {
		let (publish, subscribe) = crate::origin::resolve_pair(self.publish.as_ref(), self.consume.as_ref());

		let transport = match self.fingerprints.is_empty() {
			true => crate::transport::connect(url).await,
			false => crate::transport::connect_with_hashes(url, self.fingerprints.clone()).await,
		}
		.map_err(|err| MoqError::Connect(format!("{err}")))?;

		let mut client = moq_net::Client::new();
		if !self.versions.is_empty() {
			client = client.with_versions(self.versions.clone().into());
		}

		// Run the driver on the microtask queue. The driver
		// holds no session clone, so dropping the last handle still closes the
		// transport and ends that task.
		let (session, driver) = client
			.with_publisher(&publish)
			.with_subscriber(subscribe.clone())
			.connect(web_async::time::Instant::now(), transport)
			.await?;

		crate::ffi::spawn(async move {
			moq_net::time::run(driver).await;
		});

		Ok(Arc::new(MoqSession::accepted(session, publish, subscribe)))
	}
}

/// Decode a hex-encoded SHA-256 certificate fingerprint into raw bytes.
///
/// Not gated on wasm alone so the native test suite covers it: this parses a string an
/// FFI caller controls, and nothing in this repo runs a wasm test.
#[cfg(any(target_arch = "wasm32", test))]
fn decode_hex(hex: &str) -> Result<Vec<u8>, MoqError> {
	/// A sha-256 digest is 32 bytes, so 64 hex characters.
	const LEN: usize = 64;

	let hex = hex.replace(':', "");

	// This string comes straight from the caller, and one check covers two hazards:
	// non-ASCII would let a 2-byte chunk split a character, and `from_str_radix` accepts
	// a leading sign, so an unchecked "+a" would decode to 10 rather than erroring.
	if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
		return Err(MoqError::Config(format!("fingerprint is not hex: {hex}")));
	}

	// WebTransport's `serverCertificateHashes` only accepts a 32-byte sha-256, so a
	// different length can never match a certificate. Rejecting here beats failing
	// opaquely inside the browser.
	if hex.len() != LEN {
		return Err(MoqError::Config(format!(
			"expected a {LEN}-character sha-256 fingerprint, got {}",
			hex.len()
		)));
	}

	hex.as_bytes()
		.chunks(2)
		.map(|pair| {
			let pair = std::str::from_utf8(pair).expect("checked ascii above");
			u8::from_str_radix(pair, 16).map_err(|err| MoqError::Config(format!("{err}")))
		})
		.collect()
}

/// Dials a [`MoqSession`] with the configuration it was built from.
///
/// The configuration differs by what the target honors, because the transport does; see
/// [`MoqClientConfig`]. [`connect`](Self::connect) may be called again for another session
/// until [`cancel`](Self::cancel).
#[derive(uniffi::Object)]
pub struct MoqClient {
	task: Task<Client>,
}

#[uniffi::export]
impl MoqClient {
	/// Create a client from `config`, failing on any value it cannot use.
	#[uniffi::constructor]
	pub fn new(config: MoqClientConfig) -> Result<Arc<Self>, MoqError> {
		Ok(Arc::new(Self {
			task: Task::new(Client::new(config)?),
		}))
	}

	/// Connect to a MoQ server and wait for the session to be established.
	///
	/// A native session automatically reconnects with backoff when the transport drops
	/// (unless [`MoqClientConfig::once`] is set), and broadcasts consumed through it ride out
	/// the gap. Watch [`MoqSession::status`] for the connect/disconnect transitions,
	/// [`MoqSession::epoch`] for the reconnect count, and [`MoqSession::closed`] for the
	/// connection giving up for good.
	///
	/// Both origin sides are always accessible via [`MoqSession::publish`] and
	/// [`MoqSession::consume`], without the caller constructing a [`MoqOriginProducer`].
	///
	/// Can be cancelled by calling `cancel()`, including while the initial dial is retrying.
	pub async fn connect(&self, url: String) -> Result<Arc<MoqSession>, MoqError> {
		let url = Url::parse(&url)?;
		self.task.run(|state| async move { state.connect(url).await }).await
	}

	/// Cancel all current and future `connect()` calls.
	///
	/// Terminal: the client's configuration and wired origins are released here, not when the
	/// handle is, so this client can't dial again.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

/// A snapshot of connection statistics for a [`MoqSession`].
///
/// Each field is `None` when the transport backend doesn't report that metric (native QUIC
/// reports all of them; the browser WebTransport reports few or none), or when it isn't yet
/// available (e.g. `estimated_send_rate_bps` before the congestion controller has a window). A `None` is
/// not the same as a zero value.
#[derive(uniffi::Record)]
pub struct MoqConnectionStats {
	/// Smoothed round-trip time, in microseconds.
	pub rtt_us: Option<u64>,
	/// Estimated send bandwidth from the congestion controller, in bits per second.
	pub estimated_send_rate_bps: Option<u64>,
	/// Estimated receive bandwidth from MoQ PROBE, in bits per second.
	pub estimated_recv_rate_bps: Option<u64>,
	/// Total bytes sent, including retransmissions and overhead.
	pub bytes_sent: Option<u64>,
	/// Total bytes received, including duplicates and overhead.
	pub bytes_received: Option<u64>,
	/// Total bytes lost (detected via retransmission or acknowledgement).
	pub bytes_lost: Option<u64>,
	/// Total datagrams sent.
	pub packets_sent: Option<u64>,
	/// Total datagrams received.
	pub packets_received: Option<u64>,
	/// Total datagrams detected as lost.
	pub packets_lost: Option<u64>,
}

impl From<moq_net::session::Stats> for MoqConnectionStats {
	fn from(stats: moq_net::session::Stats) -> Self {
		Self {
			rtt_us: stats.rtt.map(|d| d.as_micros() as u64),
			estimated_send_rate_bps: stats.estimated_send_rate.map(moq_net::bandwidth::Rate::as_bps),
			estimated_recv_rate_bps: stats.estimated_recv_rate.map(moq_net::bandwidth::Rate::as_bps),
			bytes_sent: stats.bytes_sent,
			bytes_received: stats.bytes_received,
			bytes_lost: stats.bytes_lost,
			packets_sent: stats.packets_sent,
			packets_received: stats.packets_received,
			packets_lost: stats.packets_lost,
		}
	}
}

/// A connection lifecycle transition reported by [`MoqSession::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqConnectionStatus {
	/// A session connected (the first connect, or a reconnect after a drop).
	Connected,
	/// The session dropped; a reconnect attempt follows.
	Disconnected,
	/// The peer sent a GOAWAY; the replacement is being dialed while the old
	/// session keeps serving.
	Migrating,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<moq_tokio::Status> for MoqConnectionStatus {
	fn from(status: moq_tokio::Status) -> Self {
		match status {
			moq_tokio::Status::Connected => Self::Connected,
			moq_tokio::Status::Disconnected => Self::Disconnected,
			// A future unknown status means the loop is between the known states;
			// Migrating is the "still served, in flux" bucket.
			_ => Self::Migrating,
		}
	}
}

/// What backs a [`MoqSession`]: a client connection (a loop that may redial) or
/// a server-accepted session (one transport; the peer redialing yields a fresh
/// `accept()`).
#[derive(Clone)]
enum Inner {
	#[cfg(not(target_arch = "wasm32"))]
	Connection(moq_tokio::Connection),
	Session(moq_net::Session),
}

#[derive(uniffi::Object)]
pub struct MoqSession {
	inner: Inner,
	/// Serializes `closed()` calls onto the FFI runtime; holds its own `Inner` clone
	/// so a parked `closed()` doesn't block the rest of the surface.
	closed: Task<Inner>,
	/// Serializes `status()` calls, which need `&mut` for per-handle change tracking.
	status: Task<Inner>,
	publisher: Arc<MoqOriginProducer>,
	consumer: Arc<MoqOriginConsumer>,
	/// One allocator for the session. Every [`bandwidth`](Self::bandwidth) handle
	/// clones it, so they share one reservation registry.
	bandwidth: Arc<MoqBandwidth>,
}

impl MoqSession {
	/// Wrap a client connection (see [`MoqClient::connect`]).
	#[cfg(not(target_arch = "wasm32"))]
	pub(crate) fn connected(
		connection: moq_tokio::Connection,
		publish: moq_net::origin::Producer,
		subscribe: moq_net::origin::Producer,
	) -> Self {
		Self::build(Inner::Connection(connection), publish, subscribe)
	}

	/// Wrap a server-accepted session (see `MoqServer::accept`).
	pub(crate) fn accepted(
		session: moq_net::Session,
		publish: moq_net::origin::Producer,
		subscribe: moq_net::origin::Producer,
	) -> Self {
		Self::build(Inner::Session(session), publish, subscribe)
	}

	fn mint_allocator(inner: &Inner) -> moq_net::bandwidth::Allocator {
		match inner {
			// Persistent across reconnects: `None` while disconnected, then a grant
			// again on the next connection. Reservations survive the gap.
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => moq_net::bandwidth::Allocator::new(connection.send_bandwidth()),
			Inner::Session(session) => session
				.send_bandwidth()
				.map(moq_net::bandwidth::Allocator::new)
				.unwrap_or_else(moq_net::bandwidth::Allocator::unlimited),
		}
	}

	fn build(inner: Inner, publish: moq_net::origin::Producer, subscribe: moq_net::origin::Producer) -> Self {
		// Eagerly wrap the wired origin sides so each publish()/consume()
		// call hands back the same Arc. `publish` is published into; `subscribe`
		// is where the remote's broadcasts land (read via its consumer view).
		let publisher = Arc::new(MoqOriginProducer::from_inner(publish));
		let consumer = Arc::new(MoqOriginConsumer::from_inner(subscribe.consume()));
		let bandwidth = Arc::new(MoqBandwidth::new(Self::mint_allocator(&inner)));
		Self {
			inner: inner.clone(),
			closed: Task::new(inner.clone()),
			status: Task::new(inner),
			publisher,
			consumer,
			bandwidth,
		}
	}

	/// Abort the live transport (if any) with `err` and stop any reconnect loop.
	fn teardown(&self, err: moq_net::Error) {
		match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => connection.abort(err),
			Inner::Session(session) => session.abort(err),
		}
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		let _guard = crate::ffi::enter();
		// Close the transport while the runtime is entered. The backend spawns a
		// lingering CLOSE task, which panics (aborting under panic=abort) if no reactor
		// is in context. We can't leave this to the last `Session` clone's drop: clones
		// live in the `closed`/`status` tasks and the connection state, released after
		// this guard, off-runtime. Close-once dedup then makes those trailing drops no-ops.
		self.teardown(moq_net::Error::Cancel);
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is over.
	///
	/// A client session resolves when its connection stops for good: `Err` with the
	/// terminal error when it gave up (retries exhausted, or the session's close reason
	/// with reconnecting disabled), `Ok` after a local [`shutdown`](Self::shutdown) /
	/// [`cancel`](Self::cancel). Transient drops the reconnect loop rides out do not
	/// resolve this; watch [`status`](Self::status) for those. A server-accepted
	/// session resolves with the session's close reason.
	pub async fn closed(&self) -> Result<(), MoqError> {
		// We have a task to run all of the closed calls juuuuust so they use the same tokio runtime.
		self.closed
			.run(|inner| async move {
				match &*inner {
					#[cfg(not(target_arch = "wasm32"))]
					Inner::Connection(connection) => connection.closed().await.map_err(map_closed_error),
					Inner::Session(session) => Err(session.closed().await.into()),
				}
			})
			.await
	}

	/// Wait for the connection status to differ from the one this handle last reported.
	///
	/// A client session reports `Connected` first (the connect it was built from), then
	/// follows the reconnect loop: `Disconnected` while redialing, `Connected` again on
	/// success, `Migrating` during a GOAWAY handover. It returns an error once the
	/// connection stops for good (same terminal result as [`closed`](Self::closed)).
	/// A server-accepted session is a single transport, so its only transition is
	/// terminal: this waits for the close and returns its reason.
	///
	/// This is the current status, not a queue of every edge: a drop that reconnects
	/// before you ask again is coalesced away, so the outages it hides are the ones
	/// that already healed. Don't count outages with it.
	pub async fn status(&self) -> Result<MoqConnectionStatus, MoqError> {
		self.status
			.run(|mut inner| async move {
				match &mut *inner {
					#[cfg(not(target_arch = "wasm32"))]
					Inner::Connection(connection) => Ok(connection.status().await.map_err(map_closed_error)?.into()),
					Inner::Session(session) => Err(session.closed().await.into()),
				}
			})
			.await
	}

	/// The connection epoch: 1 for the connect this session was built from, one more
	/// on each reconnect. A server-accepted session is a single transport, so it stays 1.
	///
	/// The count pairs with [`status`](Self::status): a `Connected` transition whose
	/// epoch grew is a reconnect, so a worker can log each one by number. Migrations
	/// count too, since the replacement is a new session.
	pub fn epoch(&self) -> u64 {
		match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => connection.epoch(),
			Inner::Session(_) => 1,
		}
	}

	/// Close the session with the given error code, stopping any reconnect loop.
	pub fn cancel(&self, code: u32) {
		let _guard = crate::ffi::enter();
		self.teardown(moq_net::SessionError::from_code(code).into());
		// NOTE: we don't abort the closed Task; the teardown above resolves it
		// (with the close reason, or Ok once the connection loop stops).
	}

	/// Graceful shutdown. Equivalent to `cancel(0)`. Documents the
	/// convention that code 0 means "no error" so callers don't have to
	/// pick one. Named `shutdown` (not `close`) because UniFFI's Kotlin
	/// generator already emits an `AutoCloseable.close()` that releases
	/// the FFI handle, and shadowing it would silently mean a different
	/// thing per binding.
	pub fn shutdown(&self) {
		self.cancel(0);
	}

	/// The publish-side origin: where local broadcasts get advertised
	/// to the remote. Either the producer the caller wired via
	/// client config or request accept arguments, or one
	/// auto-created if neither was set.
	pub fn publish(&self) -> Arc<MoqOriginProducer> {
		self.publisher.clone()
	}

	/// The subscribe-side origin: a read handle for receiving
	/// announcements pushed by the remote. Either derived from the
	/// consume origin the caller supplied, or auto-created if
	/// neither was set.
	pub fn consume(&self) -> Arc<MoqOriginConsumer> {
		self.consumer.clone()
	}

	/// The session's bandwidth allocator, used to divide the connection's send
	/// estimate among tracks sharing it.
	///
	/// Every call returns a handle to the same registry, so reservations made
	/// through one are visible to the others. A client handle survives
	/// reconnects: the grant is `None` while disconnected and resumes on the
	/// next connection. An accepted session with no congestion estimate mints
	/// an unlimited allocator, which reports `None` for every reservation.
	pub fn bandwidth(&self) -> Arc<MoqBandwidth> {
		self.bandwidth.clone()
	}

	/// Snapshot the current connection statistics (RTT, bandwidth estimates,
	/// byte/packet counters). Cheap to call; intended for periodic polling.
	///
	/// Individual fields are `None` when the transport backend doesn't report
	/// them, or (on a client session) while the connection is between sessions;
	/// see [`MoqConnectionStats`].
	pub fn stats(&self) -> MoqConnectionStats {
		let _guard = crate::ffi::enter();
		match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => connection.monitor().stats(),
			Inner::Session(session) => Some(session.stats()),
		}
		.unwrap_or_default()
		.into()
	}
}
