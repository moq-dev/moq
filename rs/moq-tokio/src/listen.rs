//! The accept side of an endpoint: what to listen on and how to be trusted.
//!
//! [`Config`] describes the listeners (QUIC, plus optional `tcp`/`unix` qmux)
//! and the served TLS identity. The dial side lives in [`crate::connect`].

use crate::QuicBackend;

/// The accept side of an endpoint: what to listen on and how to be trusted.
///
/// Derives [`clap::Args`], so flatten it into a binary's own parser with
/// `#[command(flatten)]`. The dial side is [`crate::connect::Config`].
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[group(id = "listen-config")]
#[non_exhaustive]
pub struct Config {
	/// Listen for QUIC (UDP) on the given address. Defaults to `[::]:443`.
	///
	/// Accepts standard socket address syntax (e.g. `[::]:443`) or a DNS
	/// `host:port` pair (e.g. `fly-global-services:443`), resolved at bind time
	/// (first address only; Quinn cannot bind multiple). Leave unset while a
	/// `tcp`/`unix` listener is configured to run a stream-only server with no
	/// QUIC.
	#[serde(alias = "listen")]
	#[arg(id = "listen", long = "listen", env = "MOQ_LISTEN")]
	pub bind: Option<String>,

	/// Plaintext qmux TCP listener (`--listen-tcp-bind`, no TLS). Requires the
	/// `tcp` feature.
	#[cfg(feature = "tcp")]
	#[command(flatten)]
	#[serde(default)]
	pub tcp: crate::tcp::Config,

	/// Plaintext qmux Unix-socket listener (`--listen-unix-bind`) with an optional
	/// peer-credential allowlist. Requires the `uds` feature; unix-only.
	#[cfg(all(feature = "uds", unix))]
	#[command(flatten)]
	#[serde(default)]
	pub unix: crate::unix::Config,

	/// The QUIC backend to use.
	/// Auto-detected from compiled features if not specified.
	#[arg(id = "listen-backend", long = "listen-backend", env = "MOQ_LISTEN_BACKEND")]
	pub backend: Option<QuicBackend>,

	/// Restrict the server to specific MoQ protocol version(s).
	///
	/// By default, the server accepts all supported versions.
	/// Use this to restrict to specific versions, e.g. `--listen-version moq-lite-02`.
	/// Can be specified multiple times to accept a subset of versions.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[arg(
		id = "listen-version",
		long = "listen-version",
		env = "MOQ_LISTEN_VERSION",
		value_parser = crate::version_parser(),
	)]
	pub version: Vec<moq_net::Version>,

	/// The certificates to serve and the roots that authenticate mTLS clients
	/// (`--listen-tls-*`).
	#[command(flatten)]
	#[serde(default)]
	pub tls: crate::tls::Listen,

	/// IPv4 address advertised as the QUIC preferred_address.
	///
	/// Supporting clients (Chrome M131+, native Quinn) migrate to this address
	/// shortly after the handshake completes. Typical use: handshake on an
	/// anycast IP, steady-state on this host's unicast IP.
	///
	/// Honored by the Quinn and noq backends. Accept-only, which is why it lives
	/// here rather than in the shared [`crate::quic::Config`].
	#[arg(
		id = "listen-preferred-v4",
		long = "listen-preferred-v4",
		env = "MOQ_LISTEN_PREFERRED_V4"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v4: Option<std::net::SocketAddrV4>,

	/// IPv6 address advertised as the QUIC preferred_address. See [`Self::preferred_v4`].
	#[arg(
		id = "listen-preferred-v6",
		long = "listen-preferred-v6",
		env = "MOQ_LISTEN_PREFERRED_V6"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v6: Option<std::net::SocketAddrV6>,

	/// Server ID to embed in connection IDs for QUIC-LB compatibility.
	/// If set, connection IDs will be derived semi-deterministically.
	#[arg(id = "listen-quic-lb-id", long = "listen-quic-lb-id", env = "MOQ_LISTEN_QUIC_LB_ID")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub lb_id: Option<crate::quic::ServerId>,

	/// Number of random nonce bytes in QUIC-LB connection IDs.
	/// Must be at least 4, and server_id + nonce + 1 must not exceed 20.
	#[arg(
		id = "listen-quic-lb-nonce",
		long = "listen-quic-lb-nonce",
		env = "MOQ_LISTEN_QUIC_LB_NONCE"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub lb_nonce: Option<usize>,

	/// The released `--server-*` spellings and their env vars, kept parsing but
	/// hidden. Never read as settings: [`Config::deprecated`] names what replaced
	/// each one so a process can say so and stop.
	#[command(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,

	/// The released `[server.quic]` table, which is now the shared top-level
	/// `[quic]`.
	///
	/// Parsed only so [`deprecated`](Self::deprecated) can name the replacement;
	/// `deny_unknown_fields` would otherwise refuse the file with nothing to
	/// migrate to.
	#[arg(skip)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quic: Option<crate::quic::Config>,
}

/// One server's slot in a group of sockets sharing a port via `SO_REUSEPORT`.
///
/// Every member binds the same address and the kernel spreads inbound datagrams
/// across the group, so N servers on N threads can serve one port without a
/// shared socket between them.
///
/// Crate-private on purpose. The kernel identifies a member by its *position*,
/// so a shard is only meaningful as part of a group that was bound once, in
/// index order, and is never resized. [`crate::worker::Workers`] is what holds
/// that invariant; a shard a caller could mint, clone, or bind twice would break
/// steering with no error to show for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shard {
	index: u16,
	count: u16,
}

impl Shard {
	/// Slot `index` of a group of `count` sockets, or `None` if that slot does not
	/// exist (`count` of zero, or an `index` at or past the end).
	pub(crate) fn new(index: u16, count: u16) -> Option<Self> {
		(index < count).then_some(Self { index, count })
	}

	/// This member's position in the group, from zero.
	pub(crate) fn index(self) -> u16 {
		self.index
	}

	/// How many sockets share the port.
	pub(crate) fn count(self) -> u16 {
		self.count
	}
}

/// The `--server-*` flags from before the accept side was named `listen`.
///
/// They carry their original env vars, which is why these are separate args rather
/// than clap aliases: an alias renames the flag but not the variable, and a relay
/// deployed through the environment is the common case.
#[derive(Clone, Debug, Default, clap::Args)]
#[group(id = "listen-legacy")]
pub(crate) struct Legacy {
	#[arg(id = "server-bind", long = "server-bind", env = "MOQ_SERVER_BIND", hide = true)]
	bind: Option<String>,

	#[arg(
		id = "server-backend",
		long = "server-backend",
		env = "MOQ_SERVER_BACKEND",
		hide = true
	)]
	backend: Option<QuicBackend>,

	#[arg(
		id = "server-version",
		long = "server-version",
		env = "MOQ_SERVER_VERSION",
		value_parser = crate::version_parser(),
		hide = true,
	)]
	version: Vec<moq_net::Version>,

	#[arg(
		id = "server-preferred-v4",
		long = "server-preferred-v4",
		env = "MOQ_SERVER_PREFERRED_V4",
		hide = true
	)]
	preferred_v4: Option<std::net::SocketAddrV4>,

	#[arg(
		id = "server-preferred-v6",
		long = "server-preferred-v6",
		env = "MOQ_SERVER_PREFERRED_V6",
		hide = true
	)]
	preferred_v6: Option<std::net::SocketAddrV6>,

	#[arg(
		id = "server-quic-lb-id",
		long = "server-quic-lb-id",
		env = "MOQ_SERVER_QUIC_LB_ID",
		hide = true
	)]
	lb_id: Option<crate::quic::ServerId>,

	#[arg(
		id = "server-quic-lb-nonce",
		long = "server-quic-lb-nonce",
		env = "MOQ_SERVER_QUIC_LB_NONCE",
		hide = true
	)]
	lb_nonce: Option<usize>,
}

impl Legacy {
	/// The released spellings in use, each paired with what replaced it.
	fn deprecated(&self) -> crate::Deprecated {
		let mut found = crate::Deprecated::default();
		if self.bind.is_some() {
			found.flag("--server-bind", Some("MOQ_SERVER_BIND"), "--listen / MOQ_LISTEN");
		}
		if self.backend.is_some() {
			found.flag(
				"--server-backend",
				Some("MOQ_SERVER_BACKEND"),
				"--listen-backend / MOQ_LISTEN_BACKEND",
			);
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
	pub fn deprecated(&self) -> crate::Deprecated {
		let mut found = self.legacy.deprecated();
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

	/// Reject a QUIC-LB nonce with no server id to pair it with.
	///
	/// Checked here rather than with clap's `requires`, which can only name one arg
	/// id, and the nonce reaches this config from a TOML file as well as the flag.
	pub(crate) fn validate(&self) -> crate::Result<()> {
		match (self.lb_id.is_some(), self.lb_nonce.is_some()) {
			(false, true) => Err(crate::Error::LbNonceWithoutId),
			_ => Ok(()),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	/// A parser wrapping the config, since it derives `Args` (see the note in
	/// [`crate::connect`]).
	#[derive(Parser)]
	struct Cli {
		#[command(flatten)]
		config: Config,
	}

	fn config_from<I, T>(args: I) -> Config
	where
		I: IntoIterator<Item = T>,
		T: Into<std::ffi::OsString> + Clone,
	{
		Cli::parse_from(args).config
	}

	/// Every released `--server-*` spelling keeps parsing, so the process can name
	/// its replacement rather than leave clap reporting an unexpected argument.
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
		assert_eq!(config.bind.as_deref(), Some("[::]:443"));
		assert!(config.deprecated().to_string().contains("--server-bind"));

		let config = config_from(["test", "--listen", "[::]:443"]);
		assert!(config.deprecated().is_empty());
	}

	/// A nonce with no server id is meaningless. Checked here rather than with a
	/// clap `requires`, which can only name one arg id and never sees a TOML file.
	#[test]
	fn lb_nonce_needs_an_id() {
		let config = config_from(["test", "--listen-quic-lb-nonce", "8"]);
		assert!(matches!(config.validate(), Err(crate::Error::LbNonceWithoutId)));

		let config = config_from(["test", "--listen-quic-lb-id", "ab", "--listen-quic-lb-nonce", "8"]);
		assert!(config.validate().is_ok());
	}

	/// A slot has to exist in the group it names.
	#[test]
	fn shard_slots_are_bounded() {
		assert_eq!(Shard::new(0, 1).map(|shard| shard.count()), Some(1));
		assert_eq!(Shard::new(3, 4).map(|shard| shard.index()), Some(3));
		assert!(Shard::new(4, 4).is_none());
		assert!(Shard::new(0, 0).is_none());
	}

	/// A stream-only server opens no QUIC listener even with a bind configured,
	/// which is what lets a worker group hold that address instead.
	#[tokio::test]
	async fn init_streams_leaves_quic_alone() {
		let config = Config {
			bind: Some("127.0.0.1:0".to_string()),
			..Default::default()
		};

		let server = config.init_streams().unwrap();
		assert!(matches!(server.local_addr(), Err(crate::Error::NoBackend(_))));
	}

	/// The default constructor still binds QUIC when nothing else is configured,
	/// which is the behavior `init_streams` had to be a separate call to avoid
	/// changing.
	#[tokio::test]
	async fn init_still_defaults_to_quic() {
		let mut config = Config {
			bind: Some("127.0.0.1:0".to_string()),
			..Default::default()
		};
		config.tls.generate = vec!["localhost".to_string()];

		let server = config.init(Default::default()).unwrap();
		assert!(server.local_addr().is_ok());
	}

	/// The canonical spellings, which is what `--help` teaches.
	#[test]
	fn canonical_spellings_parse() {
		let config = config_from(["test", "--listen", "[::]:443", "--listen-version", "moq-lite-03"]);
		assert_eq!(config.bind.as_deref(), Some("[::]:443"));
		assert_eq!(config.version, vec!["moq-lite-03".parse::<moq_net::Version>().unwrap()]);
	}
}
