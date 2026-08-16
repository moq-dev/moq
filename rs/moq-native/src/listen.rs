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
	/// hidden. Folded into the canonical fields by [`Config::resolved`].
	#[command(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,

	/// The released `[server.quic]` table, which is now the shared top-level
	/// `[quic]`. Parse-only: a caller folds it in (moq-relay's `Config::resolve`
	/// does), since this side no longer owns transport tuning.
	///
	/// Kept as a field rather than dropped because `deny_unknown_fields` would
	/// otherwise refuse a released config file outright, at startup, with nothing
	/// running.
	#[arg(skip)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quic: Option<crate::quic::Config>,
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
	/// The legacy flags in use, each paired with its replacement, for one warning.
	fn used(&self) -> Vec<&'static str> {
		let mut used = Vec::new();
		if self.bind.is_some() {
			used.push("--server-bind -> --listen");
		}
		if self.backend.is_some() {
			used.push("--server-backend -> --listen-backend");
		}
		if !self.version.is_empty() {
			used.push("--server-version -> --listen-version");
		}
		if self.preferred_v4.is_some() {
			used.push("--server-preferred-v4 -> --listen-preferred-v4");
		}
		if self.preferred_v6.is_some() {
			used.push("--server-preferred-v6 -> --listen-preferred-v6");
		}
		if self.lb_id.is_some() {
			used.push("--server-quic-lb-id -> --listen-quic-lb-id");
		}
		if self.lb_nonce.is_some() {
			used.push("--server-quic-lb-nonce -> --listen-quic-lb-nonce");
		}
		used
	}
}

impl Config {
	/// Fold every released `--server-*` spelling into the canonical fields.
	///
	/// The canonical spelling wins. Warns once when a legacy spelling contributed.
	/// Idempotent, and applied automatically by [`init`](Self::init), so calling it
	/// yourself is only needed to inspect the folded values.
	pub fn resolved(&self) -> Self {
		let used = self.legacy.used();
		if !used.is_empty() {
			tracing::warn!(
				"deprecated --server-* flags in use; the accept side is now --listen-*: {}",
				used.join(", ")
			);
		}
		let legacy = &self.legacy;

		let mut resolved = self.clone();
		resolved.bind = self.bind.clone().or(legacy.bind.clone());
		resolved.backend = self.backend.clone().or(legacy.backend.clone());
		// Concatenated, not replaced: each spelling is its own list of versions to
		// accept, and dropping one would narrow what the listener takes.
		for version in &legacy.version {
			if !resolved.version.contains(version) {
				resolved.version.push(*version);
			}
		}
		resolved.preferred_v4 = self.preferred_v4.or(legacy.preferred_v4);
		resolved.preferred_v6 = self.preferred_v6.or(legacy.preferred_v6);
		resolved.lb_id = self.lb_id.clone().or(legacy.lb_id.clone());
		resolved.lb_nonce = self.lb_nonce.or(legacy.lb_nonce);
		resolved.tls = self.tls.resolved();
		#[cfg(feature = "tcp")]
		{
			resolved.tcp = self.tcp.resolved();
		}
		#[cfg(all(feature = "uds", unix))]
		{
			resolved.unix = self.unix.resolved();
		}
		resolved.legacy = Legacy::default();
		resolved
	}

	/// Take the released `[server.quic]` table, if a config file carried one.
	pub fn take_quic(&mut self) -> Option<crate::quic::Config> {
		self.quic.take().inspect(|_| {
			tracing::warn!("[server.quic] is deprecated; QUIC tuning is now the shared top-level [quic]");
		})
	}

	/// Reject a QUIC-LB nonce with no server id to pair it with.
	///
	/// Checked here rather than with clap's `requires`, which can only name one arg
	/// id: the two knobs each have a released spelling of their own, so any mix of
	/// the four has to be judged after the fold.
	pub(crate) fn validate(&self) -> crate::Result<()> {
		let resolved = self.resolved();
		match (resolved.lb_id.is_some(), resolved.lb_nonce.is_some()) {
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

	/// Every released `--server-*` spelling keeps parsing, so an existing
	/// deployment's command line still boots.
	#[test]
	fn released_server_spellings_still_parse() {
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
		])
		.resolved();
		assert_eq!(config.bind.as_deref(), Some("[::]:4443"));
		assert_eq!(config.version, vec!["moq-lite-03".parse::<moq_net::Version>().unwrap()]);
		assert_eq!(config.preferred_v4, Some("192.0.2.1:443".parse().unwrap()));
		assert!(config.lb_id.is_some());
		assert_eq!(config.tls.cert, vec![std::path::PathBuf::from("/tmp/cert.pem")]);
		assert_eq!(config.tls.root, vec![std::path::PathBuf::from("/tmp/ca.pem")]);
	}

	/// The canonical spelling wins, and folding twice changes nothing.
	#[test]
	fn canonical_wins_over_legacy() {
		let config = config_from(["test", "--listen", "[::]:443", "--server-bind", "[::]:4443"]);
		let once = config.resolved();
		assert_eq!(once.bind.as_deref(), Some("[::]:443"));
		assert_eq!(once.resolved().bind.as_deref(), Some("[::]:443"));
	}

	/// Both spellings name their own files, so neither list may be dropped.
	#[test]
	fn tls_lists_concatenate_across_spellings() {
		let config = config_from([
			"test",
			"--listen-tls-root",
			"/tmp/new.pem",
			"--server-tls-root",
			"/tmp/old.pem",
		])
		.resolved();
		assert_eq!(
			config.tls.root,
			vec![
				std::path::PathBuf::from("/tmp/new.pem"),
				std::path::PathBuf::from("/tmp/old.pem")
			]
		);
	}

	/// A nonce with no server id is meaningless, whichever spelling each came from.
	#[test]
	fn lb_nonce_needs_an_id() {
		let config = config_from(["test", "--listen-quic-lb-nonce", "8"]);
		assert!(matches!(config.validate(), Err(crate::Error::LbNonceWithoutId)));

		// Mixed spellings still pair up, which is what clap `requires` could not do.
		let config = config_from(["test", "--server-quic-lb-id", "ab", "--listen-quic-lb-nonce", "8"]);
		assert!(config.validate().is_ok());
	}

	/// The canonical spellings, which is what `--help` teaches.
	#[test]
	fn canonical_spellings_parse() {
		let config = config_from(["test", "--listen", "[::]:443", "--listen-version", "moq-lite-03"]);
		assert_eq!(config.bind.as_deref(), Some("[::]:443"));
		assert_eq!(config.version, vec!["moq-lite-03".parse::<moq_net::Version>().unwrap()]);
	}
}
