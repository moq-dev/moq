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
	#[cfg(feature = "noq")]
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
/// Derives [`usage::Args`], so flatten it into a binary's own parser with
/// `#[usage(flatten)]`. The dial side is [`crate::connect::Config`].
#[derive(usage::Args, Clone, Debug, serde::Serialize, serde::Deserialize)]
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
			"moq-transport-14",
			"moq-transport-15",
			"moq-transport-16",
			"moq-transport-17",
			"moq-transport-18",
			"moq-transport-19",
			"moq-transport-20",
			"moq-transport-21",
			"moq-transport-22"
		)
	)]
	pub version: Vec<moq_net::Version>,

	/// Maximum time for one accepted connection to finish its handshake: the
	/// QUIC, WebTransport, WebSocket, or qmux one, then the MoQ SETUP, through
	/// [`crate::server::Request::ok`]. Defaults to 10 seconds; set to 0 to wait forever.
	///
	/// A peer that connects and then never speaks would otherwise hold its
	/// connection open indefinitely, since keep-alives count as activity. The
	/// connection is closed with a timeout code once this passes. See
	/// [`resolved_timeout`](Self::resolved_timeout) for the value in effect.
	#[usage(skip)]
	#[serde(with = "crate::cli::duration::serde_duration")]
	pub timeout: std::time::Duration,

	#[usage(
		name = "listen-timeout",
		long = "listen-timeout",
		env = "MOQ_LISTEN_TIMEOUT",
		default_value_t = crate::cli::Duration::fallback(DEFAULT_TIMEOUT),
		default = "10s",
		setting = "listen.timeout"
	)]
	#[serde(default, rename = "__cli_timeout", skip_serializing_if = "Option::is_none")]
	pub(crate) timeout_arg: Option<crate::cli::Duration>,

	/// The certificates to serve and the roots that authenticate mTLS clients
	/// (`--listen-tls-*`).
	#[usage(flatten)]
	#[serde(default)]
	pub tls: crate::tls::Listen,

	/// IPv4 address advertised as the QUIC preferred_address.
	///
	/// Supporting clients (Chrome M131+, native noq) migrate to this address
	/// shortly after the handshake completes. Typical use: handshake on an
	/// anycast IP, steady-state on this host's unicast IP.
	///
	/// Honored by noq. Accept-only, which is why it lives
	/// here rather than in the shared [`crate::quic::Config`].
	#[usage(
		name = "listen-preferred-v4",
		long = "listen-preferred-v4",
		env = "MOQ_LISTEN_PREFERRED_V4",
		setting = "listen.preferred_v4"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v4: Option<std::net::SocketAddrV4>,

	/// IPv6 address advertised as the QUIC preferred_address. See [`Self::preferred_v4`].
	#[usage(
		name = "listen-preferred-v6",
		long = "listen-preferred-v6",
		env = "MOQ_LISTEN_PREFERRED_V6",
		setting = "listen.preferred_v6"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v6: Option<std::net::SocketAddrV6>,

	/// Server ID to embed in connection IDs for QUIC-LB compatibility.
	/// If set, connection IDs will be derived semi-deterministically.
	#[usage(
		name = "listen-quic-lb-id",
		long = "listen-quic-lb-id",
		env = "MOQ_LISTEN_QUIC_LB_ID",
		setting = "listen.lb_id"
	)]
	#[serde(default, rename = "__cli_lb_id", skip_serializing_if = "Option::is_none")]
	pub(crate) lb_id: Option<crate::quic::ServerId>,

	/// Number of random nonce bytes in QUIC-LB connection IDs.
	/// Must be at least 4, and server_id + nonce + 1 must not exceed 20.
	#[usage(
		name = "listen-quic-lb-nonce",
		long = "listen-quic-lb-nonce",
		env = "MOQ_LISTEN_QUIC_LB_NONCE",
		setting = "listen.lb_nonce",
		requires = "--listen-quic-lb-id"
	)]
	#[serde(default, rename = "__cli_lb_nonce", skip_serializing_if = "Option::is_none")]
	pub(crate) lb_nonce: Option<usize>,

	/// QUIC-LB connection-ID encoding.
	#[usage(skip)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub load_balancer: Option<crate::quic::LoadBalancer>,

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
	pub quic: Option<crate::quic::Config>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			bind: None,
			listen: None,
			#[cfg(feature = "tcp")]
			tcp: Default::default(),
			#[cfg(all(feature = "uds", unix))]
			unix: Default::default(),
			version: Vec::new(),
			timeout: DEFAULT_TIMEOUT,
			timeout_arg: None,
			tls: Default::default(),
			preferred_v4: None,
			preferred_v6: None,
			lb_id: None,
			lb_nonce: None,
			load_balancer: None,
			legacy: Default::default(),
			quic: None,
		}
	}
}

#[cfg(feature = "noq")]
pub(crate) use moq_sock::shard::Shard;
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
#[derive(Clone, Debug, Default, usage::Args)]
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
			"moq-transport-14",
			"moq-transport-15",
			"moq-transport-16",
			"moq-transport-17",
			"moq-transport-18",
			"moq-transport-19",
			"moq-transport-20",
			"moq-transport-21",
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
			// `tls://` serves the certificate but never asks for the client's.
			(
				self.tls.peers.is_some(),
				"tls.peers (pinned client certificates) needs a QUIC listener (--listen); the TCP and Unix listeners never ask for a client certificate",
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
				self.lb_id.is_some() || self.load_balancer.is_some(),
				"--listen-quic-lb-id (load_balancer) needs a QUIC listener (--listen)",
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
		let timeout = crate::cli::Duration::resolve(self.timeout_arg, self.timeout);
		(!timeout.is_zero() && std::time::Instant::now().checked_add(timeout).is_some()).then_some(timeout)
	}

	#[cfg(feature = "noq")]
	/// Return the effective QUIC-LB connection-ID encoding.
	pub fn load_balancer(&self) -> Option<crate::quic::LoadBalancer> {
		self.lb_id
			.clone()
			.map(|id| crate::quic::LoadBalancer {
				id,
				nonce: self.lb_nonce.unwrap_or(8),
			})
			.or_else(|| self.load_balancer.clone())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	/// A parser wrapping the config, since it derives `Args` (see the note in
	/// [`crate::connect`]).
	#[derive(usage::Cli)]
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
		assert!(config.lb_id.is_none());
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

	#[cfg(feature = "noq")]
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

	/// Programmatic configuration keeps the QUIC-LB id and nonce paired.
	#[cfg(feature = "noq")]
	#[test]
	fn load_balancer_is_a_single_typed_value() {
		assert_eq!(Config::default().load_balancer(), None);

		let config: Config = toml::from_str(
			r#"
load_balancer = { id = "ab", nonce = 8 }
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
		let cases: [(&str, Set); 8] = [
			("--listen-tls-cert", |c| c.tls.cert = vec!["cert.pem".into()]),
			("--listen-tls-key", |c| c.tls.key = vec!["key.pem".into()]),
			("--listen-tls-generate", |c| c.tls.generate = vec!["localhost".into()]),
			("tls.peers", |c| c.tls.peers = Some(crate::tls::Peers::new())),
			("--listen-preferred-v4", |c| {
				c.preferred_v4 = Some("192.0.2.1:443".parse().unwrap())
			}),
			("--listen-preferred-v6", |c| {
				c.preferred_v6 = Some("[2001:db8::1]:443".parse().unwrap())
			}),
			("--listen-quic-lb-id", |c| c.lb_id = Some("ab".parse().unwrap())),
			("--listen-quic-lb-id (load_balancer)", |c| {
				c.load_balancer = Some(crate::quic::LoadBalancer {
					id: "ab".parse().unwrap(),
					nonce: 8,
				})
			}),
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
			timeout: std::time::Duration::MAX,
			..Default::default()
		};
		assert_eq!(config.resolved_timeout(), None);
	}

	/// The canonical spellings, which is what `--help` teaches.
	#[test]
	fn canonical_spellings_parse() {
		let config = config_from(["test", "--listen", "[::]:443", "--listen-version", "moq-lite-03"]);
		assert_eq!(
			config.bind.as_ref().map(ToString::to_string).as_deref(),
			Some("[::]:443")
		);
		assert_eq!(config.version, vec!["moq-lite-03".parse::<moq_net::Version>().unwrap()]);
	}
}
