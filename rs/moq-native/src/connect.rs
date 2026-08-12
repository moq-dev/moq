//! The dial side of an endpoint: where to connect and how to get there.
//!
//! [`Addrs`] is the peer's address list, [`Config`] is how to reach it, and the
//! accept side lives in [`crate::listen`].

use crate::{Backoff, GoawayConfig, QuicBackend};
use std::net;
use url::Url;

/// Maximum time for one dial, unless overridden by `--connect-timeout`.
pub(crate) const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long to wait before also dialing the next resolved address, unless overridden
/// by `--connect-race`. RFC 8305's recommended Connection Attempt Delay.
///
/// Lives here rather than in [`crate::failover`], which is compiled only for the
/// transports that dial an address themselves: the resolved default is config, so
/// [`Config::resolved_race`] has to answer in every build.
pub(crate) const DEFAULT_RACE: std::time::Duration = std::time::Duration::from_millis(250);

/// One or more addresses for the same peer, tried in order until one connects.
///
/// Most callers have a single URL and never name this type:
/// [`crate::Client::connect`](crate::Client::connect) takes `impl Into<Addrs>` and the
/// [`From<Url>`] impl covers it. Several addresses are for a peer that was
/// discovered rather than configured, where the record lists every interface the
/// peer answered on and nothing says which of them routes from here.
///
/// Non-empty by construction, so a connection always has somewhere to dial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addrs(
	/// Never empty: every constructor takes at least one URL.
	Vec<Url>,
);

impl Addrs {
	/// A peer reachable at one address.
	pub fn new(url: Url) -> Self {
		Self(vec![url])
	}

	/// Add a fallback, tried only when everything before it fails.
	pub fn or(mut self, url: Url) -> Self {
		self.0.push(url);
		self
	}

	/// Collect addresses in dial order, or `None` when there are none.
	///
	/// The `None` is where an empty candidate list has to be dealt with: a peer
	/// that advertised no reachable address is not something to dial and retry,
	/// it's something to skip.
	pub fn collect(urls: impl IntoIterator<Item = Url>) -> Option<Self> {
		let urls: Vec<Url> = urls.into_iter().collect();
		(!urls.is_empty()).then_some(Self(urls))
	}

	/// The addresses, in the order they are dialed. Never empty.
	pub fn as_slice(&self) -> &[Url] {
		&self.0
	}
}

/// The dial socket when `--connect-bind` is unset: an ephemeral dual-stack port.
fn default_bind() -> net::SocketAddr {
	"[::]:0".parse().unwrap()
}

/// The `--client-*` flags from before the dial side was named `connect`.
#[derive(Clone, Debug, Default, clap::Args)]
#[group(id = "connect-legacy")]
pub(crate) struct Legacy {
	#[arg(
		id = "client-connect",
		long = "client-connect",
		env = "MOQ_CLIENT_CONNECT",
		hide = true
	)]
	url: Option<Url>,

	#[arg(id = "client-bind", long = "client-bind", env = "MOQ_CLIENT_BIND", hide = true)]
	bind: Option<net::SocketAddr>,

	#[arg(
		id = "client-backend",
		long = "client-backend",
		env = "MOQ_CLIENT_BACKEND",
		hide = true
	)]
	backend: Option<QuicBackend>,

	#[arg(
		id = "client-connect-timeout",
		long = "client-connect-timeout",
		env = "MOQ_CLIENT_CONNECT_TIMEOUT",
		value_parser = humantime::parse_duration,
		hide = true,
	)]
	timeout: Option<std::time::Duration>,

	#[arg(
		id = "client-failover-delay",
		long = "client-failover-delay",
		env = "MOQ_CLIENT_FAILOVER_DELAY",
		value_parser = humantime::parse_duration,
		hide = true,
	)]
	race: Option<std::time::Duration>,

	#[arg(
		id = "client-reconnect",
		long = "client-reconnect",
		env = "MOQ_CLIENT_RECONNECT",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
		hide = true,
	)]
	reconnect: Option<bool>,

	#[arg(
		id = "client-version",
		long = "client-version",
		env = "MOQ_CLIENT_VERSION",
		value_parser = crate::version_parser(),
		hide = true,
	)]
	version: Vec<moq_net::Version>,
}

impl Legacy {
	/// The legacy flags in use, each paired with its replacement, for one warning.
	///
	/// Spelled out rather than listing only the new names: `--client-reconnect` maps
	/// to a flag that means the opposite, so a bare list would send someone to a flag
	/// whose value they'd have to invert without being told.
	fn used(&self) -> Vec<&'static str> {
		let mut used = Vec::new();
		if self.url.is_some() {
			used.push("--client-connect -> --connect");
		}
		if self.bind.is_some() {
			used.push("--client-bind -> --connect-bind");
		}
		if self.backend.is_some() {
			used.push("--client-backend -> --connect-backend");
		}
		if self.timeout.is_some() {
			used.push("--client-connect-timeout -> --connect-timeout");
		}
		if self.race.is_some() {
			used.push("--client-failover-delay -> --connect-race");
		}
		if self.reconnect.is_some() {
			used.push("--client-reconnect -> --connect-once (inverted)");
		}
		if !self.version.is_empty() {
			used.push("--client-version -> --connect-version");
		}
		used
	}
}

impl From<Url> for Addrs {
	fn from(url: Url) -> Self {
		Self::new(url)
	}
}

/// A dial URL reduced to the part that names the peer, for logs and errors.
///
/// A dial URL's path and query are exactly where credentials live: `?jwt=` on a
/// relay dial, and a LAN mesh membership proof, which rides as a path segment
/// because raw QUIC has no headers to put it in. Formatting one into a log line
/// puts a replayable credential wherever logs are shipped, so nothing outside
/// this type formats a dial `Url` directly.
///
/// Dropping both rather than redacting known-secret spellings is deliberate: a
/// denylist only covers the credentials that exist today, and the next one to be
/// added would leak until someone remembered to extend it.
pub(crate) struct Endpoint<'a>(pub &'a Url);

impl std::fmt::Display for Endpoint<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}://", self.0.scheme())?;
		let Some(host) = self.0.host_str() else {
			// No host at all, e.g. a `unix:` socket, where the path is the address
			// rather than a credential and is the only thing that identifies it.
			return write!(f, "{}", self.0.path());
		};
		write!(f, "{host}")?;
		match self.0.port() {
			Some(port) => write!(f, ":{port}"),
			None => Ok(()),
		}
	}
}

/// Error returned when connection setup fails for a terminal auth reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
	/// The server rejected the credentials (HTTP 401). Retrying with the same
	/// token will fail again.
	#[error("unauthorized")]
	Unauthorized,

	/// The credentials were understood but don't grant access to this path
	/// (HTTP 403).
	#[error("forbidden")]
	Forbidden,
}

impl ConnectError {
	/// Only the transports that carry an HTTP status (WebTransport, WebSocket) can
	/// classify one; qmux over tcp/unix has no such response.
	#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche", feature = "websocket"))]
	pub(crate) fn from_status_u16(status: u16) -> Option<Self> {
		match status {
			401 => Some(Self::Unauthorized),
			403 => Some(Self::Forbidden),
			_ => None,
		}
	}

	/// Whether this is an authentication failure, meaning a retry is pointless
	/// until the credentials change.
	pub fn is_auth(&self) -> bool {
		matches!(self, Self::Unauthorized | Self::Forbidden)
	}
}

#[cfg(all(
	test,
	any(feature = "noq", feature = "quinn", feature = "quiche", feature = "websocket")
))]
mod tests {
	use super::*;

	#[test]
	fn auth_statuses_are_terminal() {
		assert_eq!(ConnectError::from_status_u16(401), Some(ConnectError::Unauthorized));
		assert_eq!(ConnectError::from_status_u16(403), Some(ConnectError::Forbidden));
	}

	#[test]
	fn non_auth_statuses_are_not_terminal() {
		for status in [400, 404, 500] {
			assert_eq!(ConnectError::from_status_u16(status), None);
		}
	}

	fn url(s: &str) -> Url {
		s.parse().expect("valid url")
	}

	/// Dial order is the order the addresses went in, and a lone URL converts
	/// implicitly so `connect(url)` keeps reading the same.
	#[test]
	fn addrs_preserve_dial_order() {
		let one = Addrs::from(url("moqt://a:4443"));
		assert_eq!(one.as_slice(), [url("moqt://a:4443")]);

		let three = Addrs::new(url("moqt://a:4443"))
			.or(url("moqt://b:4443"))
			.or(url("moqt://c:4443"));
		assert_eq!(
			three.as_slice(),
			[url("moqt://a:4443"), url("moqt://b:4443"), url("moqt://c:4443")]
		);
	}

	/// The whole point of [`Endpoint`]: a dial URL's credentials live in its path
	/// and query, so neither may survive into anything loggable.
	///
	/// The LAN mesh case is the sharp one. Its membership proof is a path segment
	/// (raw QUIC has no headers to put it in), so a dial URL logged verbatim hands
	/// a replayable credential to anyone who can read logs.
	#[test]
	fn endpoint_keeps_credentials_out_of_logs() {
		const SECRET: &str = "a4f1c93e8b7d";

		for dial in [
			// A LAN mesh dial: the proof is a path segment.
			&format!("moqt://192.168.1.5:4443/.cluster/{SECRET}"),
			// A relay dial: the token is in the query.
			&format!("https://relay.example.com/anon?jwt={SECRET}"),
			// Both at once, plus userinfo.
			&format!("https://user:{SECRET}@relay.example.com:8443/room/{SECRET}?jwt={SECRET}"),
		] {
			let rendered = Endpoint(&url(dial)).to_string();
			assert!(!rendered.contains(SECRET), "{dial} leaked through as {rendered}");
		}

		// It still names the peer, which is what a log line is for.
		assert_eq!(
			Endpoint(&url("moqt://192.168.1.5:4443/.cluster/abc")).to_string(),
			"moqt://192.168.1.5:4443"
		);
		// A default port is elided rather than invented.
		assert_eq!(
			Endpoint(&url("https://relay.example.com/anon?jwt=abc")).to_string(),
			"https://relay.example.com"
		);
		// A socket path is the address, not a credential, so it survives.
		assert_eq!(Endpoint(&url("unix:/run/moq.sock")).to_string(), "unix:///run/moq.sock");
	}

	/// A peer that advertised nothing reachable is `None` at construction, not a
	/// connection that retries an empty list forever.
	#[test]
	fn addrs_collect_rejects_an_empty_list() {
		assert_eq!(Addrs::collect([]), None);
		assert_eq!(
			Addrs::collect([url("moqt://a:4443"), url("moqt://b:4443")]),
			Some(Addrs::new(url("moqt://a:4443")).or(url("moqt://b:4443")))
		);
	}
}

/// The dial side of an endpoint: where to connect and how to get there.
///
/// Derives [`clap::Args`], so flatten it into a binary's own parser with
/// `#[command(flatten)]`. The accept side is [`crate::listen::Config`].
#[derive(Clone, Debug, Default, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[group(id = "connect-config")]
#[non_exhaustive]
pub struct Config {
	/// The URL to dial.
	///
	/// Supports WebTransport (`https`/`http`), WebSocket (`ws`/`wss`), raw QUIC
	/// (`moqt`/`moql`), qmux over `tcp`/`unix`, and `iroh`. The URL path is the
	/// request/auth path (e.g. `/anon` for a public relay) and `?jwt=` supplies a
	/// token. `http://` first fetches `/certificate.sha256` for the (insecure)
	/// self-signed fingerprint; `https://` connects directly.
	#[serde(alias = "connect", skip_serializing_if = "Option::is_none")]
	#[arg(id = "connect", long = "connect", env = "MOQ_CONNECT")]
	pub url: Option<Url>,

	/// Send from this local UDP address. Defaults to an ephemeral dual-stack port.
	///
	/// `Option` with the default resolved in code rather than a clap `default_value`,
	/// which the post-file CLI re-parse would materialize over whatever a config file
	/// said. It also makes "explicitly the default" distinguishable from unset, which
	/// the fold below needs.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[arg(id = "connect-bind", long = "connect-bind", env = "MOQ_CONNECT_BIND")]
	pub bind: Option<net::SocketAddr>,

	/// The QUIC backend to use.
	/// Auto-detected from compiled features if not specified.
	#[arg(id = "connect-backend", long = "connect-backend", env = "MOQ_CONNECT_BACKEND")]
	pub backend: Option<QuicBackend>,

	/// Delay before also dialing the next resolved address (Happy Eyeballs).
	///
	/// When DNS returns multiple addresses, attempts alternate between IPv6 and
	/// IPv4, each starting this long after the previous one (or immediately when
	/// it fails), and the first connection to complete wins. `0s` dials every
	/// address at once. Defaults to 250ms. Applies to the QUIC and `tcp://` dials.
	///
	/// This staggers the attempts within one [`crate::Client::connect`]; [`Self::timeout`]
	/// bounds that call as a whole.
	#[serde(
		alias = "failover_delay",
		default,
		skip_serializing_if = "Option::is_none",
		with = "humantime_serde::option"
	)]
	#[arg(
		id = "connect-race",
		long = "connect-race",
		env = "MOQ_CONNECT_RACE",
		value_parser = humantime::parse_duration,
	)]
	pub race: Option<std::time::Duration>,

	/// Maximum time for one [`crate::Client::connect`], covering the dial and the MoQ
	/// handshake. Defaults to 30 seconds; set to 0 to wait forever.
	///
	/// This has to live above the transports rather than inside one: QUIC bounds its
	/// own dial, but the WebSocket fallback and the handshake that follows either
	/// transport have no deadline of their own, so a peer that accepts TCP and then
	/// never speaks would hang the whole connect. [`crate::Connection`] only re-arms
	/// its backoff between attempts, so an attempt that never returns stalls the
	/// retry loop indefinitely.
	#[arg(
		id = "connect-timeout",
		long = "connect-timeout",
		env = "MOQ_CONNECT_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	pub timeout: Option<std::time::Duration>,

	/// Restrict the client to specific MoQ protocol version(s).
	///
	/// By default, the client offers all supported versions and lets the server choose.
	/// Use this to force a specific version, e.g. `--connect-version moq-lite-02`.
	/// Can be specified multiple times to offer a subset of versions.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[arg(
		id = "connect-version",
		long = "connect-version",
		env = "MOQ_CONNECT_VERSION",
		value_parser = crate::version_parser(),
	)]
	pub version: Vec<moq_net::Version>,

	/// TLS trust and client-certificate settings (`--connect-tls-*`).
	#[command(flatten)]
	#[serde(default)]
	pub tls: crate::tls::Connect,

	/// Dial once instead of redialing (with backoff) whenever the session drops.
	///
	/// A [`crate::Connection`] reconnects by default. With this set, the session's
	/// close surfaces through [`crate::Connection::closed`] instead.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "connect-once",
		long = "connect-once",
		env = "MOQ_CONNECT_ONCE",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub once: Option<bool>,

	/// The released `reconnect` spelling, which said the same thing the other way
	/// around. Inverted into [`once`](Self::once) by [`resolved`](Self::resolved).
	///
	/// TOML-only: the `--client-reconnect` flag lives on [`Legacy`] with the rest.
	#[serde(default, skip_serializing)]
	#[arg(skip)]
	pub(crate) reconnect: Option<bool>,

	/// Retry pacing for [`crate::Client::connect`] (`--backoff-*`).
	#[command(flatten)]
	#[serde(default)]
	pub backoff: Backoff,

	/// How [`crate::Client::connect`] reacts to a peer's GOAWAY (`--goaway-*`).
	#[command(flatten)]
	#[serde(default)]
	pub goaway: GoawayConfig,

	/// WebSocket fallback settings (`--websocket-*`), used when QUIC is
	/// blocked.
	#[cfg(feature = "websocket")]
	#[command(flatten)]
	#[serde(default)]
	pub websocket: crate::websocket::Config,

	/// The released `--client-*` spellings and their env vars, kept parsing but
	/// hidden. Folded into the canonical fields by [`Config::resolved`].
	#[command(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,

	/// The released `[client.quic]` table, which is now the shared top-level
	/// `[quic]`. See [`crate::listen::Config::quic`].
	#[arg(skip)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quic: Option<crate::quic::Config>,
}

impl Config {
	/// Fold every released `--client-*` spelling into the canonical fields.
	///
	/// The canonical spelling wins. Warns once when a legacy spelling contributed.
	/// Idempotent, and applied automatically by [`init`](Self::init), so calling it
	/// yourself is only needed to inspect the folded values.
	pub fn resolved(&self) -> Self {
		let used = self.legacy.used();
		if !used.is_empty() {
			tracing::warn!(
				"deprecated --client-* flags in use; the dial side is now --connect-*: {}",
				used.join(", ")
			);
		}
		let legacy = &self.legacy;

		let mut resolved = self.clone();
		resolved.url = self.url.clone().or(legacy.url.clone());
		resolved.bind = self.bind.or(legacy.bind);
		resolved.backend = self.backend.clone().or(legacy.backend.clone());
		resolved.timeout = self.timeout.or(legacy.timeout);
		resolved.race = self.race.or(legacy.race);
		// The two legacy spellings say "keep reconnecting", so they invert.
		resolved.once = self
			.once
			.or(self.reconnect.map(|reconnect| !reconnect))
			.or(legacy.reconnect.map(|reconnect| !reconnect));
		resolved.reconnect = None;
		// Concatenated, not replaced: each spelling is its own list of versions to
		// offer, and dropping one would narrow the offer behind the user's back.
		for version in &legacy.version {
			if !resolved.version.contains(version) {
				resolved.version.push(*version);
			}
		}
		resolved.tls = self.tls.resolved();
		#[cfg(feature = "websocket")]
		{
			resolved.websocket = self.websocket.resolved();
		}
		resolved.legacy = Legacy::default();
		resolved
	}

	/// Take the released `[client.quic]` table, if a config file carried one.
	pub fn take_quic(&mut self) -> Option<crate::quic::Config> {
		self.quic.take().inspect(|_| {
			tracing::warn!("[client.quic] is deprecated; QUIC tuning is now the shared top-level [quic]");
		})
	}

	/// Build the [`crate::Client`] this config describes.
	pub fn init(self, quic: crate::quic::Config) -> crate::Result<crate::Client> {
		crate::Client::new(self, quic)
	}

	/// The local address a dial sends from, resolving the default.
	pub fn resolved_bind(&self) -> net::SocketAddr {
		self.bind.unwrap_or_else(default_bind)
	}

	/// Returns the configured versions, defaulting to all if none specified.
	pub fn versions(&self) -> moq_net::Versions {
		if self.version.is_empty() {
			moq_net::Versions::all()
		} else {
			moq_net::Versions::from(self.version.clone())
		}
	}

	/// The Happy Eyeballs stagger a dial will actually use, resolving the default.
	///
	/// Every backend reads it from here so the four dial paths can't drift apart. The
	/// [`race`](Self::race) field is the override; this is the value
	/// it resolves to, so `Config::default().resolved_race()` is the
	/// default itself.
	pub fn resolved_race(&self) -> std::time::Duration {
		self.race.unwrap_or(DEFAULT_RACE)
	}

	/// The deadline one connection attempt will actually get, dial and handshake
	/// together, resolving the default from the [`timeout`](Self::timeout) override.
	pub fn resolved_timeout(&self) -> std::time::Duration {
		self.timeout.unwrap_or(DEFAULT_TIMEOUT)
	}
}
