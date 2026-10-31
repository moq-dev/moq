//! Connect to peers and manage MoQ sessions.

use std::sync::Arc;
use std::time::Duration;

use url::Url;

use crate::auth::MoqGrantConsumer;
use crate::bandwidth::MoqAllocator;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::{MoqOriginConsumer, MoqOriginProducer};

#[cfg(target_arch = "wasm32")]
mod dial;
#[cfg(target_arch = "wasm32")]
pub(crate) use dial::Dial;

/// Configuration for [`MoqClient::new`], mirroring moq-tokio's client config.
///
/// Every field has a default, so set only what you need; [`moq_client_defaults`] reports
/// them. The browser owns the socket and the trust store, so a wasm build honors only
/// `versions`, `tls.fingerprints`, and the origins, always dials once, and fails `new` with
/// `Unsupported` when anything else is set.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqClientConfig {
	/// Local UDP address to bind, e.g. `0.0.0.0:0`. Null binds an ephemeral dual-stack port.
	#[uniffi(default = None)]
	pub bind: Option<String>,
	/// Protocol versions to offer, most preferred first, spelled like `moq-lite-03`.
	/// Empty offers every supported version; [`moq_versions`] lists them.
	#[uniffi(default = [])]
	pub versions: Vec<String>,
	/// Credentials each session's dials present. The first is the connection credential, unless
	/// the URL carries its own `?jwt=`; each other one widens the session's grant. Replace a
	/// session's through [`MoqSession::tokens`].
	#[uniffi(default = [])]
	pub tokens: Vec<String>,
	/// How long one connect may take, dial and handshake together. 0 waits forever.
	#[uniffi(default = None)]
	pub timeout: Option<Duration>,
	/// Happy Eyeballs: how long before the next resolved address is also dialed. 0 dials
	/// every address at once.
	#[uniffi(default = None)]
	pub race: Option<Duration>,
	/// Happy Eyeballs: how long IPv4 waits for the full DNS answer.
	#[uniffi(default = None)]
	pub resolution_delay: Option<Duration>,
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
	/// Your own `name/version` product tokens, space-separated, advertised in SETUP ahead of
	/// moq-ffi's: `my-app/1.0` advertises `my-app/1.0 moq-ffi/0.4.12 moq-net-rs/0.3.10`.
	#[uniffi(default = None)]
	pub implementation: Option<String>,
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
	/// Server name to verify the certificate against (SNI), instead of the URL's host.
	#[uniffi(default = None)]
	pub host_name: Option<String>,
}

/// QUIC transport tuning, mirroring moq-tokio's QUIC config. Ignored by the WebSocket fallback.
///
/// Null fields use the library default, which [`moq_client_defaults`] reports where there is
/// one; `gso`, `mtu_discovery`, and `congestion_control` leave the choice to the backend.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqQuicConfig {
	/// Cap on the concurrent QUIC streams the peer may open toward this endpoint; 0 is refused.
	///
	/// MoQ opens a stream per group, and for a subscriber those arrive from the peer, so an
	/// endpoint subscribing to many tracks wants this raised.
	#[uniffi(default = None)]
	pub max_streams: Option<u64>,
	/// How long an idle connection survives before it is dropped.
	#[uniffi(default = None)]
	pub idle_timeout: Option<Duration>,
	/// Keep-alive ping interval. 0 disables it; unset derives it from the idle timeout.
	#[uniffi(default = None)]
	pub keep_alive: Option<Duration>,
	/// UDP generic segmentation offload: batches sends into one syscall.
	#[uniffi(default = None)]
	pub gso: Option<bool>,
	/// Path MTU discovery: probes for the largest packet size the path allows.
	#[uniffi(default = None)]
	pub mtu_discovery: Option<bool>,
	/// The congestion control family.
	#[uniffi(default = None)]
	pub congestion_control: Option<MoqCongestionControl>,
	/// Directory, which must exist, to write qlog traces into. Fails `new` unless
	/// [`moq_qlog_supported`].
	#[uniffi(default = None)]
	pub qlog: Option<String>,
	/// Danger: write TLS session secrets to the file `SSLKEYLOGFILE` names, for decrypting a
	/// capture. Null is off; on without `SSLKEYLOGFILE` fails `new`.
	#[uniffi(default = None)]
	pub keylog: Option<bool>,
}

/// The congestion control family for a QUIC connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqCongestionControl {
	/// Loss-based (CUBIC): chases throughput, so the send rate sawtooths.
	Loss,
	/// Delay-based (BBR): keeps queues short and the send rate steady enough for an encoder to track.
	Delay,
	/// No congestion control and no pacing, for tests and controlled links.
	None,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<MoqCongestionControl> for moq_tokio::quic::CongestionControl {
	fn from(family: MoqCongestionControl) -> Self {
		match family {
			MoqCongestionControl::Loss => Self::Loss,
			MoqCongestionControl::Delay => Self::Delay,
			MoqCongestionControl::None => Self::None,
		}
	}
}

#[cfg(not(target_arch = "wasm32"))]
impl From<MoqQuicConfig> for moq_tokio::quic::Config {
	fn from(config: MoqQuicConfig) -> Self {
		let mut quic = Self::default();
		quic.max_streams = config.max_streams;
		quic.idle_timeout = config.idle_timeout.map(Into::into);
		quic.keep_alive = config.keep_alive.map(Into::into);
		quic.gso = config.gso;
		quic.mtu_discovery = config.mtu_discovery;
		quic.congestion_control = config.congestion_control.map(Into::into);
		quic.qlog = config.qlog.map(Into::into);
		quic.keylog = config
			.keylog
			.filter(|enabled| *enabled)
			.map(|_| std::env::var_os("SSLKEYLOGFILE").unwrap_or_default().into());
		quic
	}
}

/// The WebSocket fallback for a [`MoqClientConfig`], raced against QUIC for `http(s)` URLs.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqWebSocketConfig {
	/// Whether the fallback runs. Null enables it; disable it for a relay that only serves
	/// QUIC, so a failed QUIC dial reports its own error.
	#[uniffi(default = None)]
	pub enabled: Option<bool>,
	/// Head start QUIC gets before the fallback joins the race. Null uses 200ms, and 0 races
	/// both at once.
	#[uniffi(default = None)]
	pub delay: Option<Duration>,
}

/// Retry pacing for the automatic reconnect.
///
/// The delay starts at `initial`, multiplies by `multiplier` after each failed attempt,
/// and caps at `max`. After `timeout` of consecutive failures the connection gives up
/// for good; the window resets whenever a session stays up past 1s. Each null field uses
/// moq-tokio's default: 1s, x2, 5s, and a 10s window. Zero means off: no wait, no growth,
/// no cap, or no give-up.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqBackoff {
	/// Delay before the first reconnect attempt. 0 retries at once.
	#[uniffi(default = None)]
	pub initial: Option<Duration>,
	/// Multiplier applied to the delay after each failure.
	#[uniffi(default = None)]
	pub multiplier: Option<u32>,
	/// Maximum delay between reconnect attempts. 0 leaves it uncapped.
	#[uniffi(default = None)]
	pub max: Option<Duration>,
	/// Time spent retrying before giving up. 0 retries forever.
	#[uniffi(default = None)]
	pub timeout: Option<Duration>,
}

/// What sessions opened through this crate advertise in SETUP: the caller's `prefix`, then
/// moq-ffi, then its core.
pub(crate) fn implementation(prefix: Option<&str>) -> Result<String, MoqError> {
	let core = format!("moq-ffi/{} {}", env!("CARGO_PKG_VERSION"), moq_net::IMPLEMENTATION);
	let Some(prefix) = prefix else {
		return Ok(core);
	};
	// An empty token or stray whitespace would corrupt the list every peer splits on spaces.
	if prefix
		.split(' ')
		.any(|token| token.is_empty() || token.contains(|c: char| c.is_whitespace() || c.is_control()))
	{
		return Err(MoqError::Config {
			reason: format!("implementation must be space-separated product tokens: {prefix:?}"),
		});
	}
	Ok(format!("{prefix} {core}"))
}

/// The settings a default [`MoqClientConfig`] dials with, for a settings UI to show.
///
/// Dialing with it behaves the same as the default. Knobs whose default depends on the backend
/// (`tls.system_roots`, `quic.gso`, `quic.mtu_discovery`, `quic.congestion_control`) stay null,
/// as do the ones that are off by default. A wasm build honors none of the tuning, so it
/// reports none.
#[uniffi::export]
pub fn moq_client_defaults() -> MoqClientConfig {
	#[cfg(not(target_arch = "wasm32"))]
	{
		let connect = moq_tokio::connect::Config::default();
		let resolved = connect.resolve();
		let websocket = connect.websocket.resolve();
		let backoff = connect.backoff;
		let quic = moq_tokio::quic::Config::default().resolve();

		MoqClientConfig {
			timeout: Some(resolved.timeout),
			race: Some(resolved.race),
			resolution_delay: Some(resolved.resolution_delay),
			quic: MoqQuicConfig {
				max_streams: Some(quic.max_streams),
				idle_timeout: Some(quic.idle_timeout),
				// Unset: derived from the idle timeout the peers negotiate.
				keep_alive: quic.keep_alive,
				..Default::default()
			},
			websocket: MoqWebSocketConfig {
				enabled: Some(websocket.enabled),
				delay: Some(websocket.delay),
			},
			backoff: MoqBackoff {
				initial: Some(backoff.initial()),
				multiplier: Some(backoff.multiplier),
				max: Some(backoff.max()),
				timeout: Some(backoff.timeout()),
			},
			..Default::default()
		}
	}

	#[cfg(target_arch = "wasm32")]
	MoqClientConfig::default()
}

/// The protocol versions offered by default, most preferred first, spelled the way
/// `MoqClientConfig::versions` takes them.
///
/// Work-in-progress versions are omitted, since they are only offered when pinned by name.
#[uniffi::export]
pub fn moq_versions() -> Vec<String> {
	moq_net::Versions::all().iter().map(ToString::to_string).collect()
}

/// Whether this build can write qlog traces, so a UI can hide `MoqQuicConfig::qlog` otherwise.
#[uniffi::export]
pub fn moq_qlog_supported() -> bool {
	#[cfg(not(target_arch = "wasm32"))]
	return moq_tokio::qlog_supported();

	#[cfg(target_arch = "wasm32")]
	false
}

/// Parse protocol version names, as `MoqClientConfig::versions` and
/// `MoqServerConfig::versions` spell them.
pub(crate) fn parse_versions(versions: &[String]) -> Result<Vec<moq_net::Version>, MoqError> {
	versions
		.iter()
		.map(|version| version.parse().map_err(|message| MoqError::Config { reason: message }))
		.collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_tokens(tokens: Vec<String>) -> Result<Vec<moq_net::auth::Token>, MoqError> {
	tokens
		.into_iter()
		.map(|token| {
			token
				.try_into()
				.map_err(|err: moq_net::auth::EmptyToken| MoqError::Config {
					reason: err.to_string(),
				})
		})
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
		connect.implementation = Some(implementation(config.implementation.as_deref())?);
		if let Some(bind) = config.bind {
			let bind = bind.parse().map_err(|err| MoqError::Config {
				reason: format!("invalid bind address {bind}: {err}"),
			})?;
			connect.bind = Some(bind);
		}
		connect.version = parse_versions(&config.versions)?;
		connect.tokens = parse_tokens(config.tokens)?;
		connect.timeout = config.timeout.map(Into::into);
		connect.race = config.race.map(Into::into);
		connect.resolution_delay = config.resolution_delay.map(Into::into);

		let tls = config.tls;
		connect.tls.insecure = Some(tls.insecure);
		connect.tls.root = tls.roots.into_iter().map(Into::into).collect();
		connect.tls.system_roots = tls.system_roots;
		connect.tls.fingerprint = tls.fingerprints;
		connect.tls.cert = tls.cert.map(Into::into);
		connect.tls.key = tls.key.map(Into::into);
		connect.tls.host_name = tls.host_name;

		connect.websocket.enabled = config.websocket.enabled;
		if let Some(delay) = config.websocket.delay {
			connect.websocket.delay = Some(delay.into());
		}

		let backoff = config.backoff;
		if let Some(initial) = backoff.initial {
			connect.backoff.initial = Some(initial.into());
		}
		if let Some(multiplier) = backoff.multiplier {
			connect.backoff.multiplier = multiplier;
		}
		if let Some(max) = backoff.max {
			connect.backoff.max = Some(max.into());
		}
		if let Some(timeout) = backoff.timeout {
			connect.backoff.timeout = Some(timeout.into());
		}

		let quic = config.quic.into();

		// Building the endpoint here, not at connect, is what surfaces an unreadable
		// certificate or a half-configured mTLS identity from `new`.
		let client = connect
			.init(quic)
			.map_err(|err| MoqError::Config {
				reason: format!("{err}"),
			})?
			.with_reconnect(!config.once);

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
			.with_publisher(&publish.inner)
			.with_subscriber(subscribe.inner.clone())
			.connect(url);

		Ok(Arc::new(MoqSession::connected(connection, publish, subscribe)))
	}
}

/// The terminal error of a connection that had already established a session.
///
/// A session that ended carries
/// a [`moq_net::Error`], and stringifying that into `Connect` would both lose the
/// variant a caller can match on and claim the dial failed when it had succeeded.
/// A server-accepted session reports its close the same way.
///
/// A local stop is not a failure at all, so it maps to `Closed`, which is what the
/// bindings' `is_shutdown` recognizes. `state_changed()` reaches this path whenever
/// another handle shuts the connection down underneath it, and reporting that as a
/// connect error would make every lifecycle watcher treat an expected teardown as a
/// broken connection.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn map_closed_error(err: moq_tokio::Error) -> MoqError {
	match err {
		moq_tokio::Error::Panic => MoqError::Task {
			reason: "background task panicked".into(),
		},
		moq_tokio::Error::Stopped => MoqError::Closed,
		// A peer's session/stream code stays structured. HTTP 401/403 are a
		// connect-time rejection, not a protocol code, and land below.
		moq_tokio::Error::MoqNet(err) => err.into(),
		err => match err.connect_error() {
			Some(moq_tokio::ConnectError::Unauthorized) => MoqError::Unauthorized,
			Some(moq_tokio::ConnectError::Forbidden) => MoqError::Forbidden,
			_ => MoqError::Connect {
				reason: format!("{err}"),
			},
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
			assert!(matches!(decode_hex(input), Err(MoqError::Config { .. })), "{input}");
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
			assert!(matches!(decode_hex(&bad), Err(MoqError::Config { .. })), "{bad}");
		}
	}

	#[test]
	fn maps_native_auth_connect_errors() {
		assert!(matches!(
			map_closed_error(moq_tokio::ConnectError::Unauthorized.into()),
			MoqError::Unauthorized
		));
		assert!(matches!(
			map_closed_error(moq_tokio::ConnectError::Forbidden.into()),
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

		// A session close keeps its 0x18 code; a refused request's expiry has none.
		match map_closed_error(moq_net::Error::from(moq_net::SessionError::Expired).into()) {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.kind, crate::error::MoqProtocolKind::Expired);
				assert_eq!(protocol.code, 0x18);
			}
			other => panic!("expected Protocol Expired, got {other:?}"),
		}
		assert!(matches!(
			map_closed_error(moq_net::Error::Expired.into()),
			MoqError::Expired
		));
	}

	/// `new` is where a bad value surfaces, rather than at the first connect.
	#[test]
	fn new_rejects_invalid_config() {
		let bind = MoqClientConfig {
			bind: Some("not-an-address".into()),
			..Default::default()
		};
		assert!(matches!(MoqClient::new(bind), Err(MoqError::Config { .. })));

		let version = MoqClientConfig {
			versions: vec!["moq-lite-99".into()],
			..Default::default()
		};
		assert!(matches!(MoqClient::new(version), Err(MoqError::Config { .. })));

		let missing = MoqClientConfig {
			tls: MoqClientTls {
				roots: vec!["/nonexistent/root.pem".into()],
				..Default::default()
			},
			..Default::default()
		};
		assert!(matches!(MoqClient::new(missing), Err(MoqError::Config { .. })));

		let empty = MoqClientConfig {
			tokens: vec![String::new()],
			..Default::default()
		};
		assert!(matches!(MoqClient::new(empty), Err(MoqError::Config { .. })));

		for prefix in ["", " app/1", "app/1 ", "app/1  lib/2", "app/1\tlib/2", "app/1\n"] {
			let implementation = MoqClientConfig {
				implementation: Some(prefix.into()),
				..Default::default()
			};
			assert!(
				matches!(MoqClient::new(implementation), Err(MoqError::Config { .. })),
				"{prefix:?}"
			);
		}
	}

	#[tokio::test]
	async fn setting_tokens_refuses_an_empty_token() {
		let client = MoqClient::new(MoqClientConfig::default()).unwrap();
		let session = client.connect("moqt://localhost:1/".into()).await.unwrap();
		let tokens = session.tokens().expect("a client session has tokens");
		tokens.set(vec!["a".into(), "b".into()]).unwrap();
		assert!(matches!(tokens.set(vec![String::new()]), Err(MoqError::Config { .. })));
		session.cancel(0);
	}

	#[test]
	fn new_accepts_the_defaults_and_known_versions() {
		MoqClient::new(MoqClientConfig::default()).unwrap();
		MoqClient::new(MoqClientConfig {
			bind: Some("127.0.0.1:0".into()),
			versions: vec!["moq-lite-03".into()],
			tokens: vec!["credential".into()],
			once: true,
			backoff: MoqBackoff {
				timeout: Some(Duration::ZERO),
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
	/// Offered as ALPNs to WebTransport and then enforced by moq-net, so the two agree.
	versions: moq_net::Versions,
	/// What SETUP advertises, with the caller's prefix joined.
	implementation: String,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

#[cfg(target_arch = "wasm32")]
impl Client {
	fn new(config: MoqClientConfig) -> Result<Self, MoqError> {
		let MoqClientConfig {
			bind,
			versions,
			// Presenting tokens needs the native client's dial and AUTH handling.
			tokens,
			timeout,
			race,
			resolution_delay,
			tls,
			quic,
			websocket,
			// A browser session never redials, so it dials once either way.
			once: _,
			backoff,
			implementation: prefix,
			publish,
			consume,
		} = config;

		let native_tls = MoqClientTls {
			fingerprints: Vec::new(),
			..tls.clone()
		};
		if bind.is_some()
			|| !tokens.is_empty()
			|| timeout.is_some()
			|| race.is_some()
			|| resolution_delay.is_some()
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
			versions: match versions.is_empty() {
				true => moq_net::Versions::default(),
				false => parse_versions(&versions)?.into(),
			},
			implementation: implementation(prefix.as_deref())?,
			publish,
			consume,
		})
	}

	async fn connect(&self, url: Url) -> Result<Arc<MoqSession>, MoqError> {
		let (publish, subscribe) = crate::origin::resolve_pair(self.publish.as_ref(), self.consume.as_ref());

		let versions = self.versions.clone();
		let fingerprints = self.fingerprints.clone();
		let client = moq_net::Client::new().with_url(url.clone());
		let transport = async move {
			match fingerprints.is_empty() {
				true => crate::transport::connect(url, &versions).await,
				false => crate::transport::connect_with_hashes(url, &versions, fingerprints).await,
			}
		};
		let client = client
			.with_versions(self.versions.clone())
			.with_implementation(self.implementation.clone())
			.with_publisher(&publish.inner)
			.with_subscriber(subscribe.inner.clone());

		Ok(Arc::new(MoqSession::build(
			Inner::Dial(Dial::start(client, transport)),
			publish,
			subscribe,
		)))
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
		return Err(MoqError::Config {
			reason: format!("fingerprint is not hex: {hex}"),
		});
	}

	// WebTransport's `serverCertificateHashes` only accepts a 32-byte sha-256, so a
	// different length can never match a certificate. Rejecting here beats failing
	// opaquely inside the browser.
	if hex.len() != LEN {
		return Err(MoqError::Config {
			reason: format!("expected a {LEN}-character sha-256 fingerprint, got {}", hex.len()),
		});
	}

	hex.as_bytes()
		.chunks(2)
		.map(|pair| {
			let pair = std::str::from_utf8(pair).expect("checked ascii above");
			u8::from_str_radix(pair, 16).map_err(|err| MoqError::Config {
				reason: format!("{err}"),
			})
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
		let client = Client::new(config)?;
		Ok(Arc::new(Self {
			task: Task::new(client),
		}))
	}

	/// Start a connection without waiting for its first dial to succeed.
	///
	/// A native session automatically reconnects with backoff when the transport drops
	/// (unless [`MoqClientConfig::once`] is set), and broadcasts consumed through it ride out
	/// the gap. A browser session dials once and never reconnects. Read [`MoqSession::state`]
	/// and wait with [`MoqSession::state_changed`] for first-dial failures and reconnects;
	/// [`MoqSession::closed`] waits for the terminal result.
	///
	/// Both origin sides are always accessible via [`MoqSession::publish`] and
	/// [`MoqSession::consume`], without the caller constructing a [`MoqOriginProducer`].
	///
	/// Cancel the returned session to stop its dial; cancelling the client stops future calls.
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

/// The credentials a native client session presents, replaceable while it runs.
///
/// Returned by [`MoqSession::tokens`], seeded from [`MoqClientConfig::tokens`].
#[derive(uniffi::Object)]
pub struct MoqTokens {
	#[cfg(not(target_arch = "wasm32"))]
	inner: moq_net::auth::Tokens,
}

#[uniffi::export]
impl MoqTokens {
	/// Replace the credentials every dial presents, such as before the current ones expire.
	///
	/// The first is the connection credential, unless the URL carries its own `?jwt=`; each
	/// other one widens the grant over in-band AUTH. The next dial presents them, and a live
	/// session that supports in-band AUTH presents them at once, withdrawing each replaced
	/// token once its replacements are answered, even when one is refused. After an
	/// authorization refusal the session redials only if they changed.
	pub fn set(&self, tokens: Vec<String>) -> Result<(), MoqError> {
		#[cfg(not(target_arch = "wasm32"))]
		{
			self.inner.set(parse_tokens(tokens)?);
			Ok(())
		}
		#[cfg(target_arch = "wasm32")]
		{
			let _ = tokens;
			Err(MoqError::Unsupported)
		}
	}

	/// Wait for the peer's first verdict on the tokens the last [`set`](Self::set) newly
	/// presented: `Ok` once each is granted, in band or as a later dial's connection credential.
	///
	/// A token the set kept counts only once refused, and nothing is re-checked after the
	/// answer. The peer's refusal is a [`MoqError::Protocol`] carrying its code,
	/// [`MoqError::Unsupported`] when the session cannot carry an in-band token, and the
	/// connection's end, such as a refused dial, is its close reason.
	pub async fn answered(&self) -> Result<(), MoqError> {
		#[cfg(not(target_arch = "wasm32"))]
		{
			let tokens = self.inner.clone();
			crate::ffi::detached(async move { tokens.answered().await.map_err(MoqError::from) }).await
		}
		#[cfg(target_arch = "wasm32")]
		{
			Err(MoqError::Unsupported)
		}
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
	/// Smoothed round-trip time.
	pub rtt: Option<Duration>,
	/// Estimated send bandwidth from the congestion controller, in bits per second.
	pub estimated_send_rate_bps: Option<u64>,
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
	/// The protocol version the connection negotiated, spelled like
	/// [`MoqClientConfig::versions`]. Sampled with the rest, so a reconnect can't pair one
	/// connection's version with another's numbers.
	#[uniffi(default = None)]
	pub version: Option<String>,
}

impl MoqConnectionStats {
	fn new(stats: moq_net::session::Stats, version: Option<moq_net::Version>) -> Self {
		Self {
			rtt: stats.rtt.map(crate::ffi::duration),
			estimated_send_rate_bps: stats.estimated_send_rate.map(moq_net::bandwidth::Rate::as_bps),
			bytes_sent: stats.bytes_sent,
			bytes_received: stats.bytes_received,
			bytes_lost: stats.bytes_lost,
			packets_sent: stats.packets_sent,
			packets_received: stats.packets_received,
			packets_lost: stats.packets_lost,
			version: version.map(|version| version.to_string()),
		}
	}
}

/// The current lifecycle phase, distinct from cumulative statistics presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqConnectionPhase {
	/// No session has connected yet.
	Connecting,
	/// A session is serving.
	Connected,
	/// No session is serving while a native connection redials.
	Disconnected,
	/// The old session serves while its replacement dials.
	Migrating,
	/// The connection has stopped for good.
	Closed,
}

/// One coherent connection lifecycle observation.
#[derive(Debug, uniffi::Record)]
pub struct MoqConnectionState {
	/// An opaque revision to pass back to `state_changed` on this session.
	pub revision: u64,
	/// The current lifecycle phase.
	pub phase: MoqConnectionPhase,
	/// How many sessions have connected, including replacements.
	pub connects: u64,
	/// The latest failed attempt or terminal failure, cleared by a successful connect.
	pub failure: Option<MoqError>,
	/// The live session's negotiated protocol, or none while offline.
	pub version: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<moq_tokio::connection::Observation> for MoqConnectionState {
	fn from(state: moq_tokio::connection::Observation) -> Self {
		use moq_tokio::connection::Phase;
		Self {
			revision: state.revision,
			phase: match state.phase {
				Phase::Connecting => MoqConnectionPhase::Connecting,
				Phase::Connected => MoqConnectionPhase::Connected,
				Phase::Disconnected => MoqConnectionPhase::Disconnected,
				Phase::Migrating => MoqConnectionPhase::Migrating,
				Phase::Closed => MoqConnectionPhase::Closed,
			},
			connects: state.connects,
			failure: state.failure.map(map_closed_error),
			version: state.version.map(|v| v.to_string()),
		}
	}
}

/// What backs a [`MoqSession`]: a native client connection (a loop that may redial),
/// a server-accepted session (one transport; the peer redialing yields a fresh
/// `accept()`), or a browser dial (one attempt, then its session).
#[derive(Clone)]
enum Inner {
	#[cfg(not(target_arch = "wasm32"))]
	Connection(moq_tokio::Connection),
	#[cfg(not(target_arch = "wasm32"))]
	Session(Accepted),
	#[cfg(target_arch = "wasm32")]
	Dial(Dial),
}

#[derive(Clone)]
struct Accepted {
	session: moq_net::Session,
}

impl std::ops::Deref for Accepted {
	type Target = moq_net::Session;
	fn deref(&self) -> &Self::Target {
		&self.session
	}
}

impl Accepted {
	fn state(&self) -> MoqConnectionState {
		let mut closed = std::pin::pin!(self.closed::<()>());
		let result = std::future::Future::poll(
			closed.as_mut(),
			&mut std::task::Context::from_waker(std::task::Waker::noop()),
		);
		let (phase, failure, revision) = match result {
			std::task::Poll::Pending => (MoqConnectionPhase::Connected, None, 0),
			std::task::Poll::Ready(result) => (MoqConnectionPhase::Closed, result.err(), 1),
		};
		MoqConnectionState {
			revision,
			phase,
			connects: 1,
			failure,
			version: (phase == MoqConnectionPhase::Connected).then(|| self.session.version().to_string()),
		}
	}

	async fn closed<T>(&self) -> Result<T, MoqError> {
		Err(self.session.closed().await.into())
	}

	async fn close(self) -> Result<(), MoqError> {
		let result = self.session.close().await;
		result.map_err(Into::into)
	}
}

/// An owned drain; dropping its waiter or the foreign session cannot cancel it.
#[derive(Clone)]
struct Shutdown {
	result: kio::Consumer<Option<Result<(), MoqError>>>,
	task: crate::ffi::Background,
}

impl Shutdown {
	async fn finished(&self) -> Result<(), MoqError> {
		// Publish a background panic before reading the channel it dropped.
		#[cfg(not(target_arch = "wasm32"))]
		self.task.finished().await?;
		#[cfg(target_arch = "wasm32")]
		self.task.check()?;
		self.result
			.wait(|result| match &**result {
				Some(result) => std::task::Poll::Ready(result.clone()),
				None => std::task::Poll::Pending,
			})
			.await
			.map_err(|_| MoqError::Cancelled)?
	}
}

/// A connection that exchanges broadcasts with a peer.
#[derive(uniffi::Object)]
pub struct MoqSession {
	inner: Inner,
	shutdown: std::sync::Mutex<Option<Shutdown>>,
	/// Serializes `closed()` calls onto the FFI runtime; holds its own `Inner` clone
	/// so a parked `closed()` doesn't block the rest of the surface.
	closed: Task<Inner>,
	publisher: Arc<MoqOriginProducer>,
	consumer: Arc<MoqOriginConsumer>,
	/// One allocator for the session. Every [`allocator`](Self::allocator) handle
	/// clones it, so they share one reservation registry.
	allocator: Arc<MoqAllocator>,
}

impl MoqSession {
	/// Wrap a client connection (see [`MoqClient::connect`]).
	#[cfg(not(target_arch = "wasm32"))]
	pub(crate) fn connected(
		connection: moq_tokio::Connection,
		publish: crate::origin::Origin,
		subscribe: crate::origin::Origin,
	) -> Self {
		Self::build(Inner::Connection(connection), publish, subscribe)
	}

	/// Wrap a server-accepted session (see `MoqServer::accept`).
	#[cfg(not(target_arch = "wasm32"))]
	pub(crate) fn accepted(
		session: moq_net::Session,
		publish: crate::origin::Origin,
		subscribe: crate::origin::Origin,
	) -> Self {
		Self::build(Inner::Session(Accepted { session }), publish, subscribe)
	}

	fn mint_allocator(inner: &Inner) -> moq_net::bandwidth::Allocator {
		match inner {
			// Persistent across reconnects: `None` while disconnected, then a grant
			// again on the next connection. Reservations survive the gap.
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => connection.allocator(),
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Session(session) => session.allocator(),
			// Persistent from before the handshake, so a reservation made while dialing counts.
			#[cfg(target_arch = "wasm32")]
			Inner::Dial(dial) => dial.allocator(),
		}
	}

	fn build(inner: Inner, publish: crate::origin::Origin, subscribe: crate::origin::Origin) -> Self {
		// Eagerly wrap the wired origin sides so each publish()/consume()
		// call hands back the same Arc. `publish` is published into; `subscribe`
		// is where the remote's broadcasts land (read via its consumer view).
		let publisher = Arc::new(MoqOriginProducer::from_inner(publish));
		let consumer = Arc::new(MoqOriginConsumer::from_inner(subscribe));
		let allocator = Arc::new(MoqAllocator::new(Self::mint_allocator(&inner)));
		Self {
			inner: inner.clone(),
			shutdown: Default::default(),
			closed: Task::new(inner.clone()),
			publisher,
			consumer,
			allocator,
		}
	}

	async fn watch<T>(&self, future: impl std::future::Future<Output = Result<T, MoqError>>) -> Result<T, MoqError> {
		self.publisher
			.inner()
			.driver
			.watch(self.consumer.driver.watch(future))
			.await
	}

	/// Abort the live transport (if any) with `err` and stop any dial or reconnect loop.
	fn teardown(&self, err: moq_net::Error) {
		match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => connection.abort(err),
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Session(session) => session.abort(err),
			#[cfg(target_arch = "wasm32")]
			Inner::Dial(dial) => dial.stop(err),
		}
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		let _guard = crate::ffi::enter();
		// Close the transport while the runtime is entered. The backend spawns a
		// lingering CLOSE task, which panics if no reactor is in context. We can't leave this to the last `Session` clone's drop: clones
		// live in the `closed` task and the connection state, released after
		// this guard, off-runtime. Close-once dedup then makes those trailing drops no-ops.
		if self.shutdown.get_mut().unwrap().is_none() {
			self.teardown(moq_net::Error::Cancel);
		}
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is over.
	///
	/// A client session resolves when its connection stops for good: `Err` with the
	/// terminal error when it gave up (retries exhausted, or the session's close reason
	/// with reconnecting disabled), `Ok` after a local [`shutdown`](Self::shutdown) /
	/// [`cancel`](Self::cancel), or the drain error if shutdown failed. Transient drops the reconnect loop rides out do not
	/// resolve this; watch [`state_changed`](Self::state_changed) for those. A server-accepted
	/// session reports a requested drain's result, otherwise the session's close reason.
	pub async fn closed(&self) -> Result<(), MoqError> {
		// We have a task to run all of the closed calls juuuuust so they use the same tokio runtime.
		let terminal = self
			.watch(self.closed.run(|inner| async move {
				match &*inner {
					#[cfg(not(target_arch = "wasm32"))]
					Inner::Connection(connection) => connection.closed().await.map_err(map_closed_error),
					#[cfg(not(target_arch = "wasm32"))]
					Inner::Session(session) => session.closed().await,
					#[cfg(target_arch = "wasm32")]
					Inner::Dial(dial) => dial.closed().await,
				}
			}))
			.await;
		// The client loop can stop as soon as the drain starts. Serialize this read
		// with publication so an existing waiter cannot miss the drain's result.
		let shutdown = self.shutdown.lock().unwrap().clone();
		match shutdown {
			Some(shutdown) => shutdown.finished().await,
			None => terminal,
		}
	}

	/// Read the connection lifecycle fields together without advancing any watcher.
	pub fn state(&self) -> MoqConnectionState {
		let _runtime = crate::ffi::enter();
		match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => connection.monitor().state().into(),
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Session(session) => session.state(),
			#[cfg(target_arch = "wasm32")]
			Inner::Dial(dial) => dial.state(),
		}
	}

	/// Wait for a revision different from `after`, without sharing a cursor with other callers.
	///
	/// Pass the revision from this session's last snapshot. Changes between reading and
	/// waiting are returned immediately; repeated failed dials advance it too. The final
	/// snapshot reports `Closed`; waiting again at that revision returns the close reason.
	pub async fn state_changed(&self, after: u64) -> Result<MoqConnectionState, MoqError> {
		self.watch(Task::new(self.inner.clone()).run(|inner| async move {
			match &*inner {
				#[cfg(not(target_arch = "wasm32"))]
				Inner::Connection(connection) => Ok(connection
					.monitor()
					.state_changed(after)
					.await
					.map_err(map_closed_error)?
					.into()),
				#[cfg(not(target_arch = "wasm32"))]
				Inner::Session(session) => {
					let state = session.state();
					if state.revision != after {
						return Ok(state);
					}
					if state.phase == MoqConnectionPhase::Closed {
						return Err(state.failure.unwrap_or(MoqError::Closed));
					}
					let _ = session.closed::<()>().await;
					Ok(session.state())
				}
				#[cfg(target_arch = "wasm32")]
				Inner::Dial(dial) => dial.state_changed(after).await,
			}
		}))
		.await
	}

	/// Close the session with the given error code, stopping any reconnect loop.
	pub fn cancel(&self, code: u32) {
		let _guard = crate::ffi::enter();
		self.teardown(moq_net::SessionError::from_code(code).into());
		// NOTE: we don't abort the closed Task; the teardown above resolves it
		// (with the close reason, or Ok once the connection loop stops).
	}

	/// Request a draining close now; wait for its result with [`closed`](Self::closed).
	///
	/// Finish or abort live tracks first. The request survives disposal of this
	/// handle or a `closed()` waiter; repeated requests share the same drain.
	/// `cancel` stays immediate. The name avoids Kotlin's generated
	/// `AutoCloseable.close()`, which releases the handle.
	/// Explicit `moq-lite-07-wip` waits for the peer to receive the tail. Older Lite
	/// versions, including the default lite-06, and IETF wait only for transport ACKs.
	/// A drain that takes longer than one second ends with a delivery timeout.
	pub fn shutdown(&self) {
		let _guard = crate::ffi::enter();
		let mut shutdown = self.shutdown.lock().unwrap();
		if shutdown.is_some() {
			return;
		}
		let result = kio::Producer::new(None);
		let receiver = result.consume();
		let inner = self.inner.clone();
		// Keep the origins, not just the connection, alive through the drain.
		let publisher = self.publisher.clone();
		let consumer = self.consumer.clone();
		let task = crate::ffi::spawn(async move {
			let completed = match inner {
				#[cfg(not(target_arch = "wasm32"))]
				Inner::Connection(connection) => connection.close().await.map_err(map_closed_error),
				#[cfg(not(target_arch = "wasm32"))]
				Inner::Session(session) => session.close().await,
				#[cfg(target_arch = "wasm32")]
				Inner::Dial(dial) => dial.close().await,
			};
			let completed = publisher
				.inner()
				.driver
				.check()
				.and(consumer.driver.check())
				.and(completed);
			*result
				.write()
				.unwrap_or_else(|_| unreachable!("the drain owns its completion")) = Some(completed);
		});
		*shutdown = Some(Shutdown { result: receiver, task });
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
	pub fn allocator(&self) -> Arc<MoqAllocator> {
		self.allocator.clone()
	}

	/// Follow the grant the peer gave this session.
	///
	/// Every call returns an independent consumer. A client session's grant follows each
	/// reconnect, reading `None` while offline.
	pub fn grants(&self) -> Arc<MoqGrantConsumer> {
		Arc::new(MoqGrantConsumer::new(&match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => connection.auth(),
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Session(session) => session.auth(),
			#[cfg(target_arch = "wasm32")]
			Inner::Dial(dial) => dial.auth(),
		}))
	}

	/// The credentials a native client session presents, to replace while it runs.
	///
	/// `None` for a server-accepted session, which presents none, and in the browser, where
	/// the URL carries the credential.
	pub fn tokens(&self) -> Option<Arc<MoqTokens>> {
		match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => Some(Arc::new(MoqTokens {
				inner: connection.tokens(),
			})),
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Session(_) => None,
			#[cfg(target_arch = "wasm32")]
			Inner::Dial(_) => None,
		}
	}

	/// Hold receive-estimate demand until the returned consumer is released.
	pub fn recv_bandwidth(&self) -> Option<Arc<crate::bandwidth::MoqBandwidthConsumer>> {
		let consumer = match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => Some(connection.recv_bandwidth()),
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Session(session) => session.recv_bandwidth(),
			#[cfg(target_arch = "wasm32")]
			Inner::Dial(dial) => Some(dial.recv_bandwidth()),
		}?;
		Some(Arc::new(crate::bandwidth::MoqBandwidthConsumer::new(consumer)))
	}

	/// Snapshot the current connection statistics (RTT, bandwidth estimates,
	/// byte/packet counters). Cheap to call; intended for periodic polling.
	///
	/// Individual fields are `None` when the transport backend doesn't report
	/// them, or (on a client session) while the connection is between sessions;
	/// see [`MoqConnectionStats`].
	/// Reading statistics does not request receive estimates.
	pub fn stats(&self) -> MoqConnectionStats {
		let _guard = crate::ffi::enter();
		match &self.inner {
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Connection(connection) => match connection.monitor().snapshot() {
				Some(snapshot) => MoqConnectionStats::new(snapshot.stats, Some(snapshot.version)),
				None => MoqConnectionStats::new(Default::default(), None),
			},
			#[cfg(not(target_arch = "wasm32"))]
			Inner::Session(session) => MoqConnectionStats::new(session.stats(), Some(session.version())),
			#[cfg(target_arch = "wasm32")]
			Inner::Dial(dial) => match dial.session() {
				Some(session) => MoqConnectionStats::new(session.stats(), Some(session.version())),
				None => MoqConnectionStats::new(Default::default(), None),
			},
		}
	}
}
