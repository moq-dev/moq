//! The accept side of an endpoint: what to listen on and how to be trusted.
//!
//! [`Config`] describes the listeners (QUIC, plus optional `tcp`/`unix` qmux)
//! and the served TLS identity. The dial side lives in [`crate::connect`].

/// A QUIC listen address.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Bind {
	/// A resolved socket address.
	Addr(std::net::SocketAddr),
	/// A host and port to resolve when the listener binds.
	Host(String, u16),
}

impl Bind {
	#[cfg(feature = "quic")]
	pub(crate) fn resolve(&self) -> std::io::Result<std::net::SocketAddr> {
		match self {
			Self::Addr(addr) => Ok(*addr),
			Self::Host(host, port) => crate::util::resolve(Some(&format!("{host}:{port}")), ""),
		}
	}
}

impl std::str::FromStr for Bind {
	type Err = std::net::AddrParseError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let socket_error = match value.parse() {
			Ok(addr) => return Ok(Self::Addr(addr)),
			Err(err) => err,
		};

		let Some((host, port)) = value.rsplit_once(':') else {
			return Err(socket_error);
		};
		if host.is_empty() || host.contains(':') {
			return Err(socket_error);
		}
		let Ok(port) = port.parse() else {
			return Err(socket_error);
		};
		Ok(Self::Host(host.to_owned(), port))
	}
}

impl std::fmt::Display for Bind {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Addr(addr) => addr.fmt(f),
			Self::Host(host, port) => write!(f, "{host}:{port}"),
		}
	}
}

impl serde::Serialize for Bind {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		serializer.collect_str(self)
	}
}

impl<'de> serde::Deserialize<'de> for Bind {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		String::deserialize(deserializer)?
			.parse()
			.map_err(serde::de::Error::custom)
	}
}

/// How long an accepted connection has to finish its handshake, unless overridden by `--listen-timeout`.
pub(crate) const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The accept side of an endpoint: what to listen on and how to be trusted.
///
/// Derives [`usage_rs::Args`], so flatten it into a binary's own parser with
/// `#[usage(flatten)]`. The dial side is [`crate::connect::Config`].
#[derive(usage_rs::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Listen for QUIC (UDP) on the given address. Defaults to `[::]:443`.
	///
	/// Text configuration accepts socket addresses and `host:port` names. Hostnames
	/// are resolved when the listener binds (first address only). Leave unset while a
	/// `tcp`/`unix` listener is configured to run a stream-only server with no
	/// QUIC.
	#[usage(name = "listen", long = "listen", env = "MOQ_LISTEN", setting = "listen.bind")]
	pub bind: Option<Bind>,

	/// The released `listen` key, kept so [`deprecated`](Self::deprecated) can name [`bind`](Self::bind).
	#[serde(default, skip_serializing)]
	#[usage(skip)]
	pub(crate) listen: Option<String>,

	/// Plaintext qmux TCP listener (`--listen-tcp-bind`, no TLS). Requires the
	/// `tcp` feature.
	#[cfg(feature = "tcp")]
	#[usage(flatten)]
	#[serde(default)]
	pub tcp: crate::tcp::Config,

	/// Plaintext qmux Unix-socket listener (`--listen-unix-bind`) with an optional
	/// peer-credential allowlist. Requires the `uds` feature; unix-only.
	#[cfg(all(feature = "uds", unix))]
	#[usage(flatten)]
	#[serde(default)]
	pub unix: crate::unix::Config,

	/// Restrict the server to specific MoQ protocol version(s).
	///
	/// By default, the server accepts all supported versions.
	/// Use this to restrict to specific versions, e.g. `--listen-version moq-lite-02`.
	/// Can be specified multiple times to accept a subset of versions.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[usage(
		name = "listen-version",
		long = "listen-version",
		env = "MOQ_LISTEN_VERSION",
		setting = "listen.version",
		choices(
			"moq-lite-01",
			"moq-lite-02",
			"moq-lite-03",
			"moq-lite-04",
			"moq-lite-05",
			"moq-lite-06",
			"moq-lite-07-wip",
			"moq-transport-18",
			"moq-transport-20",
			"moq-transport-22"
		)
	)]
	pub version: Vec<moq_net::Version>,

	/// The extensions to offer in SETUP, every moq-transport one by default.
	#[serde(default, skip_serializing_if = "crate::connect::all_extensions")]
	#[usage(skip)]
	pub extensions: moq_net::setup::Extensions,

	/// The implementation to advertise in SETUP, as space-separated `name/version`
	/// product tokens. Defaults to the binary's own; empty advertises nothing.
	#[usage(
		name = "listen-implementation",
		long = "listen-implementation",
		env = "MOQ_LISTEN_IMPLEMENTATION",
		setting = "listen.implementation"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub implementation: Option<String>,

	/// Maximum time for one accepted connection to finish its handshake: the
	/// QUIC, WebTransport, WebSocket, or qmux one, then the MoQ SETUP, whatever the
	/// server runs under (admission, say), until the session is accepted. Defaults to
	/// 10 seconds; set to 0 to wait forever.
	///
	/// A peer that connects and then never speaks would otherwise hold its
	/// connection open indefinitely, since keep-alives count as activity. The
	/// connection is closed with a timeout code once this passes.
	/// `None` uses the default; zero disables this duration.
	#[usage(
		name = "listen-timeout",
		long = "listen-timeout",
		env = "MOQ_LISTEN_TIMEOUT",
		setting = "listen.timeout"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout: Option<crate::cli::Duration>,

	/// The certificates to serve and the roots that authenticate mTLS clients
	/// (`--listen-tls-*`).
	#[usage(flatten)]
	#[serde(default)]
	pub tls: crate::tls::Listen,

	/// IPv4 address advertised as the QUIC preferred_address.
	///
	/// Supporting clients (Chrome M131+, this crate) migrate to this address
	/// shortly after the handshake completes. Typical use: handshake on an
	/// anycast IP, steady-state on this host's unicast IP.
	///
	/// Honored by the QUIC backend. Accept-only, which is why it lives
	/// here rather than in the shared QUIC settings.
	#[usage(
		name = "listen-preferred-v4",
		long = "listen-preferred-v4",
		env = "MOQ_LISTEN_PREFERRED_V4",
		setting = "listen.preferred_v4"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v4: Option<std::net::SocketAddrV4>,

	/// IPv6 address advertised as the QUIC preferred_address. Like `--listen-preferred-v4`, for IPv6.
	#[usage(
		name = "listen-preferred-v6",
		long = "listen-preferred-v6",
		env = "MOQ_LISTEN_PREFERRED_V6",
		setting = "listen.preferred_v6"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v6: Option<std::net::SocketAddrV6>,

	/// QUIC-LB connection-ID encoding (`[listen.lb]`).
	#[usage(flatten)]
	#[serde(default)]
	pub lb: LoadBalancer,

	/// QUIC stateless resets (`--listen-reset-*`).
	#[usage(flatten)]
	#[serde(default)]
	pub reset: Reset,

	/// The released `--server-*` spellings and their env vars, kept parsing but
	/// hidden. Never read as settings: [`Config::deprecated`] names what replaced
	/// each one so a process can say so and stop.
	#[usage(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,

	/// The released `[server.quic]` table, which is now the shared top-level
	/// `[quic]`.
	///
	/// Parsed only so [`deprecated`](Self::deprecated) can name the replacement;
	/// `deny_unknown_fields` would otherwise refuse the file with nothing to
	/// migrate to.
	#[usage(skip)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub(crate) quic: Option<crate::quic::Config>,
}

/// QUIC stateless reset settings (`[listen.reset]`, `--listen-reset-*`).
#[derive(usage_rs::Args, Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Reset {
	/// A file holding the secret the stateless reset key is derived from, as 64
	/// hexadecimal characters (`openssl rand -hex 32`).
	///
	/// A server restarted with the same secret resets its predecessor's connections,
	/// so peers reconnect at once instead of waiting out their idle timeout. Every
	/// holder can reset every other holder's connections, so share it between live
	/// servers only behind one address that routes by connection ID (QUIC-LB).
	/// Unset draws a random key per process.
	#[usage(
		name = "listen-reset-secret",
		long = "listen-reset-secret",
		env = "MOQ_LISTEN_RESET_SECRET",
		setting = "listen.reset.secret"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub secret: Option<std::path::PathBuf>,
}

/// The QUIC listener settings with defaults applied, produced by [`Config::resolve`].
///
/// Exhaustive on purpose, unlike [`crate::connect::Resolved`]: a QUIC runtime outside
/// this crate destructures it without `..`, so a setting added here fails to compile
/// until every runtime honors or refuses it.
#[derive(Clone, Debug)]
pub struct Resolved {
	/// The QUIC listen address; unset leaves the choice of default to the runtime.
	pub bind: Option<Bind>,
	/// The versions to accept, every one when none was configured.
	pub versions: moq_net::Versions,
	/// The extensions to offer in SETUP.
	pub extensions: moq_net::setup::Extensions,
	/// The implementation to advertise in SETUP, or the runtime's own when unset.
	pub implementation: Option<String>,
	/// The handshake deadline, from [`Config::resolved_timeout`].
	pub timeout: Option<std::time::Duration>,
	/// The served identity and the roots that authenticate mTLS clients.
	pub tls: crate::tls::Listen,
	/// The IPv4 QUIC preferred_address to advertise.
	pub preferred_v4: Option<std::net::SocketAddrV4>,
	/// The IPv6 QUIC preferred_address to advertise.
	pub preferred_v6: Option<std::net::SocketAddrV6>,
	/// The QUIC-LB connection-ID encoding.
	pub lb: LoadBalancer,
	/// The file the stateless reset key is derived from.
	pub reset_secret: Option<std::path::PathBuf>,
}

/// One server's socket in a complete `SO_REUSEPORT` group.
///
/// Crate-private on purpose: [`crate::worker::Workers`] is the only thing that
/// forms a group here, and callers do not need its raw serving handles.
#[cfg(feature = "_transport")]
pub(crate) use moq_sock::shard::Socket;

/// The `--server-*` flags from before the accept side was named `listen`.
///
/// They carry their original env vars, which is why these are separate args rather
/// than Usage aliases: an alias renames the flag but not the variable, and a relay
/// deployed through the environment is the common case.
#[derive(Clone, Debug, Default, usage_rs::Args)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub(crate) struct Legacy {
	#[usage(name = "server-bind", long = "server-bind", env = "MOQ_SERVER_BIND", hide = true)]
	bind: Option<String>,

	#[usage(
		name = "server-version",
		long = "server-version",
		env = "MOQ_SERVER_VERSION",
		choices(
			"moq-lite-01",
			"moq-lite-02",
			"moq-lite-03",
			"moq-lite-04",
			"moq-lite-05",
			"moq-lite-06",
			"moq-lite-07-wip",
			"moq-transport-18",
			"moq-transport-20",
			"moq-transport-22"
		),
		hide = true
	)]
	version: Vec<moq_net::Version>,

	#[usage(
		name = "server-preferred-v4",
		long = "server-preferred-v4",
		env = "MOQ_SERVER_PREFERRED_V4",
		hide = true
	)]
	preferred_v4: Option<std::net::SocketAddrV4>,

	#[usage(
		name = "server-preferred-v6",
		long = "server-preferred-v6",
		env = "MOQ_SERVER_PREFERRED_V6",
		hide = true
	)]
	preferred_v6: Option<std::net::SocketAddrV6>,

	#[usage(
		name = "server-quic-lb-id",
		long = "server-quic-lb-id",
		env = "MOQ_SERVER_QUIC_LB_ID",
		hide = true
	)]
	lb_id: Option<crate::quic::ServerId>,

	#[usage(
		name = "server-quic-lb-nonce",
		long = "server-quic-lb-nonce",
		env = "MOQ_SERVER_QUIC_LB_NONCE",
		hide = true
	)]
	lb_nonce: Option<usize>,
}

impl Legacy {
	/// The released spellings in use, each paired with what replaced it.
	fn deprecated(&self) -> crate::cli::Deprecated {
		let mut found = crate::cli::Deprecated::default();
		if self.bind.is_some() {
			found.flag("--server-bind", Some("MOQ_SERVER_BIND"), "--listen / MOQ_LISTEN");
		}
		if !self.version.is_empty() {
			found.flag(
				"--server-version",
				Some("MOQ_SERVER_VERSION"),
				"--listen-version / MOQ_LISTEN_VERSION",
			);
		}
		if self.preferred_v4.is_some() {
			found.flag(
				"--server-preferred-v4",
				Some("MOQ_SERVER_PREFERRED_V4"),
				"--listen-preferred-v4 / MOQ_LISTEN_PREFERRED_V4",
			);
		}
		if self.preferred_v6.is_some() {
			found.flag(
				"--server-preferred-v6",
				Some("MOQ_SERVER_PREFERRED_V6"),
				"--listen-preferred-v6 / MOQ_LISTEN_PREFERRED_V6",
			);
		}
		if self.lb_id.is_some() {
			found.flag(
				"--server-quic-lb-id",
				Some("MOQ_SERVER_QUIC_LB_ID"),
				"--listen-quic-lb-id / MOQ_LISTEN_QUIC_LB_ID",
			);
		}
		if self.lb_nonce.is_some() {
			found.flag(
				"--server-quic-lb-nonce",
				Some("MOQ_SERVER_QUIC_LB_NONCE"),
				"--listen-quic-lb-nonce / MOQ_LISTEN_QUIC_LB_NONCE",
			);
		}
		found
	}
}

impl Config {
	/// Every released spelling this config was parsed from, across this section and
	/// the TLS, TCP, and Unix ones it owns, each paired with what replaced it.
	///
	/// A binary checks this before anything else and exits when it isn't empty. The
	/// old spellings are parsed so the process can name their replacement, not so it
	/// can honor them: [`crate::Server::new`] rejects them too, so a config that
	/// skipped the check can't reach a listener that quietly ignored half of it.
	pub fn deprecated(&self) -> crate::cli::Deprecated {
		let mut found = self.legacy.deprecated();
		if self.listen.is_some() {
			found.toml("listen", "bind", None);
		}
		if self.quic.is_some() {
			found.toml("[server.quic]", "[quic]", Some("now applies to both directions"));
		}
		found.extend(self.tls.deprecated());
		#[cfg(feature = "tcp")]
		found.extend(self.tcp.deprecated());
		#[cfg(all(feature = "uds", unix))]
		found.extend(self.unix.deprecated());
		found
	}

	#[cfg(feature = "_transport")]
	pub(crate) fn validate(&self) -> crate::Result<()> {
		#[cfg(feature = "tcp")]
		if self.tcp.tls == Some(true) && self.tcp.bind.is_none() {
			return Err(crate::Error::NoBackend("--listen-tcp-tls requires --listen-tcp-bind"));
		}
		#[cfg(all(feature = "uds", unix))]
		if !self.unix.allow.is_empty() && self.unix.bind.is_none() {
			return Err(crate::Error::NoBackend(
				"--listen-unix-allow-* requires --listen-unix-bind",
			));
		}
		if self.lb.nonce.is_some() && self.lb.id.is_none() {
			return Err(crate::Error::NoBackend(
				"--listen-quic-lb-nonce requires --listen-quic-lb-id",
			));
		}
		Ok(())
	}

	/// Refuse what only the QUIC listener reads, for a server that has none.
	#[cfg(feature = "_transport")]
	pub(crate) fn validate_stream_only(&self) -> crate::Result<()> {
		#[cfg(feature = "tcp")]
		let tcp_tls = self.tcp.tls == Some(true);
		#[cfg(not(feature = "tcp"))]
		let tcp_tls = false;
		#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
		let identity = self.tls.identity.is_some();
		#[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
		let identity = false;

		let refused = [
			(
				!tcp_tls && !self.tls.cert.is_empty(),
				"--listen-tls-cert needs a TLS listener (--listen or --listen-tcp-tls)",
			),
			(
				!tcp_tls && !self.tls.key.is_empty(),
				"--listen-tls-key needs a TLS listener (--listen or --listen-tcp-tls)",
			),
			(
				!tcp_tls && !self.tls.generate.is_empty(),
				"--listen-tls-generate needs a TLS listener (--listen or --listen-tcp-tls)",
			),
			(
				!tcp_tls && identity,
				"tls.identity needs a TLS listener (--listen or --listen-tcp-tls)",
			),
			(
				self.preferred_v4.is_some(),
				"--listen-preferred-v4 needs a QUIC listener (--listen)",
			),
			(
				self.preferred_v6.is_some(),
				"--listen-preferred-v6 needs a QUIC listener (--listen)",
			),
			(
				self.lb.id.is_some(),
				"--listen-quic-lb-id needs a QUIC listener (--listen)",
			),
			(
				self.reset.secret.is_some(),
				"--listen-reset-secret needs a QUIC listener (--listen)",
			),
		];
		match refused.into_iter().find(|(set, _)| *set) {
			Some((_, reason)) => Err(crate::Error::NoBackend(reason)),
			None => Ok(()),
		}
	}

	/// How long an accepted connection has to finish its handshake, or `None` to wait
	/// forever: for zero, and for a timeout too long for the clock to reach.
	pub fn resolved_timeout(&self) -> Option<std::time::Duration> {
		let timeout = self.timeout.map(Into::into).unwrap_or(DEFAULT_TIMEOUT);
		(!timeout.is_zero() && kio::clock::now().checked_add(timeout).is_some()).then_some(timeout)
	}

	/// The QUIC listener settings, for a runtime outside this crate to map.
	///
	/// Refuses released spellings and invalid combinations, as [`crate::Server::new`]
	/// does. Destructures every field, so a new one fails to compile here until it is
	/// either carried into [`Resolved`] or named as something a QUIC listener never
	/// reads.
	#[cfg(feature = "_transport")]
	pub fn resolve(&self) -> crate::Result<Resolved> {
		let deprecated = self.deprecated();
		if !deprecated.is_empty() {
			return Err(crate::Error::Deprecated(deprecated));
		}
		self.validate()?;

		let Self {
			bind,
			version: _,
			extensions,
			implementation,
			timeout: _,
			tls,
			preferred_v4,
			preferred_v6,
			lb,
			reset: Reset { secret: reset_secret },
			// Served by `Server` on the caller's runtime, never by a QUIC listener.
			#[cfg(feature = "tcp")]
				tcp: _,
			#[cfg(all(feature = "uds", unix))]
				unix: _,
			// Refused above as deprecated.
			listen: _,
			legacy: _,
			quic: _,
		} = self;

		Ok(Resolved {
			bind: bind.clone(),
			versions: self.versions(),
			extensions: *extensions,
			implementation: implementation.clone(),
			timeout: self.resolved_timeout(),
			tls: tls.clone(),
			preferred_v4: *preferred_v4,
			preferred_v6: *preferred_v6,
			lb: lb.clone(),
			reset_secret: reset_secret.clone(),
		})
	}

	#[cfg(feature = "quic")]
	/// Return the effective QUIC-LB connection-ID encoding.
	pub(crate) fn load_balancer(&self) -> Option<crate::quic::LoadBalancer> {
		self.lb.id.clone().map(|id| crate::quic::LoadBalancer {
			id,
			nonce: self.lb.nonce.unwrap_or(8),
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The listen config offers every extension unless told otherwise, and leaves the
	/// default out when serialized.
	#[test]
	fn extensions_default_on_and_parse_off() {
		let config: Config = toml::from_str("[extensions]\nauth = false\n").expect("parse");
		assert!(!config.extensions.auth);
		assert!(config.extensions.solicit);
		assert!(toml::to_string(&config).expect("serialize").contains("auth = false"));
		assert!(
			!toml::to_string(&Config::default())
				.expect("serialize")
				.contains("extensions")
		);
	}
	/// A parser wrapping the config, since it derives `Args` (see the note in
	/// [`crate::connect`]).
	#[derive(usage_rs::Cli)]
	#[usage(unknown_flags = "error", args_override_self = false)]
	#[usage(settings)]
	struct Cli {
		#[usage(flatten)]
		config: Config,
	}

	fn config_from<I, T>(args: I) -> Config
	where
		I: IntoIterator<Item = T>,
		T: Into<std::ffi::OsString> + Clone,
	{
		let args = args.into_iter().map(Into::into).collect::<Vec<std::ffi::OsString>>();
		let args = args
			.iter()
			.skip(1)
			.map(std::ffi::OsString::as_os_str)
			.collect::<Vec<_>>();
		Cli::parse_from(&args).unwrap().config
	}

	/// Every released `--server-*` spelling keeps parsing, so the process can name
	/// its replacement rather than leave Usage reporting an unexpected argument.
	/// None of them configure anything.
	#[test]
	fn released_server_spellings_are_reported_not_applied() {
		let config = config_from([
			"test",
			"--server-bind",
			"[::]:4443",
			"--server-version",
			"moq-lite-03",
			"--server-preferred-v4",
			"192.0.2.1:443",
			"--server-quic-lb-id",
			"ab",
			"--server-tls-cert",
			"/tmp/cert.pem",
			"--server-tls-root",
			"/tmp/ca.pem",
		]);
		assert_eq!(config.bind, None);
		assert!(config.version.is_empty());
		assert_eq!(config.preferred_v4, None);
		assert!(config.lb.id.is_none());
		assert!(config.tls.cert.is_empty());
		assert!(config.tls.root.is_empty());

		let reported = config.deprecated().to_string();
		for line in [
			"--server-bind / MOQ_SERVER_BIND -> --listen / MOQ_LISTEN",
			"--server-version / MOQ_SERVER_VERSION -> --listen-version / MOQ_LISTEN_VERSION",
			"--server-preferred-v4 / MOQ_SERVER_PREFERRED_V4 -> --listen-preferred-v4 / MOQ_LISTEN_PREFERRED_V4",
			"--server-quic-lb-id / MOQ_SERVER_QUIC_LB_ID -> --listen-quic-lb-id / MOQ_LISTEN_QUIC_LB_ID",
			"--tls-cert / MOQ_SERVER_TLS_CERT -> --listen-tls-cert / MOQ_LISTEN_TLS_CERT",
			"--server-tls-root / MOQ_SERVER_TLS_ROOT -> --listen-tls-root / MOQ_LISTEN_TLS_ROOT",
		] {
			assert!(reported.contains(line), "missing {line:?} from {reported}");
		}
	}

	/// A `--listen` next to the spelling it replaced is still refused: honoring one
	/// half of a command line and refusing the other is how a deployment ends up
	/// running on settings nobody wrote.
	#[test]
	fn a_canonical_spelling_does_not_excuse_a_released_one() {
		let config = config_from(["test", "--listen", "[::]:443", "--server-bind", "[::]:4443"]);
		assert_eq!(
			config.bind.as_ref().map(ToString::to_string).as_deref(),
			Some("[::]:443")
		);
		assert!(config.deprecated().to_string().contains("--server-bind"));

		let config = config_from(["test", "--listen", "[::]:443"]);
		assert!(config.deprecated().is_empty());
	}

	/// The released TOML key still parses so the process can name `bind`, but it
	/// configures nothing.
	#[test]
	fn released_listen_key_is_reported_not_applied() {
		let config: Config = toml::from_str(r#"listen = "[::]:443""#).expect("parse");
		assert_eq!(config.bind, None);
		assert!(
			config.deprecated().to_string().contains("listen -> bind"),
			"{}",
			config.deprecated()
		);
	}

	#[test]
	fn bind_host_round_trips_as_text() {
		#[derive(serde::Serialize, serde::Deserialize)]
		struct Wrapper {
			bind: Bind,
		}

		let expected = Bind::Host("relay.example.com".to_string(), 443);
		let encoded = toml::to_string(&Wrapper { bind: expected.clone() }).expect("serialize");
		let decoded: Wrapper = toml::from_str(&encoded).expect("deserialize");
		assert_eq!(decoded.bind, expected);

		assert!("relay.example.com:443:8443".parse::<Bind>().is_err());
	}

	#[cfg(feature = "quic")]
	#[test]
	fn cli_load_balancer_survives_the_merge_round_trip() {
		let config = config_from(["test", "--listen-quic-lb-id", "ab", "--listen-quic-lb-nonce", "9"]);
		let encoded = toml::Value::try_from(config).expect("serialize");
		let decoded: Config = encoded.try_into().expect("deserialize");
		assert_eq!(
			decoded.load_balancer(),
			Some(crate::quic::LoadBalancer {
				id: "ab".parse().unwrap(),
				nonce: 9,
			})
		);
	}

	#[test]
	fn quic_lb_nonce_without_id_refuses_to_start() {
		let config: Config = toml::from_str("[lb]\nnonce = 8").unwrap();
		assert!(
			matches!(config.validate(), Err(crate::Error::NoBackend(reason)) if reason.contains("requires --listen-quic-lb-id"))
		);
	}

	#[test]
	fn removed_load_balancer_paths_are_refused() {
		for body in [
			"lb_id = \"ab\"",
			"lb_nonce = 8",
			"__cli_lb_id = \"ab\"",
			"load_balancer = { id = \"ab\", nonce = 8 }",
		] {
			assert!(toml::from_str::<Config>(body).is_err(), "{body}");
		}
	}

	/// Programmatic configuration keeps the QUIC-LB id and nonce paired.
	#[cfg(feature = "quic")]
	#[test]
	fn load_balancer_is_a_single_typed_value() {
		assert_eq!(Config::default().load_balancer(), None);

		let config: Config = toml::from_str(
			r#"
[lb]
id = "ab"
nonce = 8
"#,
		)
		.unwrap();
		assert_eq!(
			config.load_balancer(),
			Some(crate::quic::LoadBalancer {
				id: "ab".parse().unwrap(),
				nonce: 8,
			})
		);
	}

	/// A stream-only server opens no QUIC listener even with a bind configured,
	/// which is what lets a worker group hold that address instead.
	#[tokio::test]
	async fn init_streams_leaves_quic_alone() {
		let config = Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			..Default::default()
		};

		let server = config.init_streams().unwrap();
		assert!(matches!(server.local_addr(), Err(crate::Error::NoBackend(_))));
	}

	/// A stream-only server refuses each setting only QUIC reads rather than
	/// ignoring it, and accepts them once QUIC lives elsewhere.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn stream_only_refuses_quic_settings() {
		let stream_only = || Config {
			tcp: crate::tcp::Config {
				bind: Some("127.0.0.1:0".parse().unwrap()),
				..Default::default()
			},
			..Default::default()
		};
		type Set = fn(&mut Config);
		let cases: [(&str, Set); 7] = [
			("--listen-tls-cert", |c| c.tls.cert = vec!["cert.pem".into()]),
			("--listen-tls-key", |c| c.tls.key = vec!["key.pem".into()]),
			("--listen-tls-generate", |c| c.tls.generate = vec!["localhost".into()]),
			("--listen-preferred-v4", |c| {
				c.preferred_v4 = Some("192.0.2.1:443".parse().unwrap())
			}),
			("--listen-preferred-v6", |c| {
				c.preferred_v6 = Some("[2001:db8::1]:443".parse().unwrap())
			}),
			("--listen-quic-lb-id", |c| c.lb.id = Some("ab".parse().unwrap())),
			("--listen-reset-secret", |c| c.reset.secret = Some("reset.key".into())),
		];
		for (name, set) in cases {
			let mut config = stream_only();
			set(&mut config);
			match config.clone().init(Default::default()) {
				Err(crate::Error::NoBackend(reason)) => assert!(reason.starts_with(name), "{name}: {reason}"),
				Err(err) => panic!("{name}: {err}"),
				Ok(_) => panic!("{name} was ignored"),
			}
			config.init_streams().expect("streams beside worker-owned QUIC");
		}

		// `tls://` serves the certificate.
		let mut config = stream_only();
		config.tcp.tls = Some(true);
		config.tls.generate = vec!["localhost".into()];
		config.init(Default::default()).expect("a TLS stream listener");
	}

	/// A Unix allowlist without the Unix listener it gates is refused.
	#[cfg(all(feature = "uds", unix))]
	#[test]
	fn unix_allow_requires_unix_bind() {
		let mut config = Config::default();
		config.unix.allow.uid = vec![0];
		match config.init(Default::default()) {
			Err(crate::Error::NoBackend(reason)) => assert!(reason.contains("--listen-unix-bind"), "{reason}"),
			Err(err) => panic!("{err}"),
			Ok(_) => panic!("the allowlist was ignored"),
		}
	}

	/// The default constructor still binds QUIC when nothing else is configured,
	/// which is the behavior `init_streams` had to be a separate call to avoid
	/// changing.
	#[tokio::test]
	async fn init_still_defaults_to_quic() {
		let mut config = Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			..Default::default()
		};
		config.tls.generate = vec!["localhost".to_string()];

		let server = config.init(Default::default()).unwrap();
		assert!(server.local_addr().is_ok());
	}

	/// The handshake deadline defaults on, from the parser and in code alike, and
	/// `0s` turns it off.
	#[test]
	fn timeout_defaults_and_disables() {
		let secs = std::time::Duration::from_secs;
		assert_eq!(Config::default().resolved_timeout(), Some(DEFAULT_TIMEOUT));
		assert_eq!(config_from(["test"]).resolved_timeout(), Some(DEFAULT_TIMEOUT));
		assert_eq!(
			config_from(["test", "--listen-timeout", "3s"]).resolved_timeout(),
			Some(secs(3))
		);
		assert_eq!(config_from(["test", "--listen-timeout", "0s"]).resolved_timeout(), None);

		let config: Config = toml::from_str(r#"timeout = "4s""#).expect("parse");
		assert_eq!(config.resolved_timeout(), Some(secs(4)));

		// Past the clock's range is forever, not a panic on the first accept.
		let config = Config {
			timeout: Some((std::time::Duration::MAX).into()),
			..Default::default()
		};
		assert_eq!(config.resolved_timeout(), None);
	}
}

#[cfg(test)]
mod duration_tests {
	use super::*;
	crate::cli::duration::test_config_duration!(
		timeout_set_in_code_beats_parser_default,
		Config,
		timeout,
		"--listen-timeout",
		|c: &Config| c.resolved_timeout(),
		|d: std::time::Duration| (!d.is_zero()).then_some(d)
	);
}

/// QUIC-LB settings for a listener, unset until a server ID is supplied.
#[derive(Clone, Debug, Default, usage_rs::Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadBalancer {
	/// Server ID to embed in connection IDs for QUIC-LB compatibility.
	/// If set, connection IDs will be derived semi-deterministically.
	#[usage(
		name = "listen-quic-lb-id",
		long = "listen-quic-lb-id",
		env = "MOQ_LISTEN_QUIC_LB_ID",
		setting = "listen.lb.id"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<crate::quic::ServerId>,

	/// Number of random nonce bytes in QUIC-LB connection IDs.
	/// Must be at least 4, and server_id + nonce + 1 must not exceed 20.
	#[usage(
		name = "listen-quic-lb-nonce",
		long = "listen-quic-lb-nonce",
		env = "MOQ_LISTEN_QUIC_LB_NONCE",
		setting = "listen.lb.nonce",
		requires = "--listen-quic-lb-id"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub nonce: Option<usize>,
}
