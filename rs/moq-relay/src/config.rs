use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::{AuthConfig, CacheConfig, ClusterConfig, InternalConfig, StatsConfig, WebConfig};

/// Top-level relay configuration, loadable from CLI arguments, environment
/// variables, or a TOML file.
#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[command(version = env!("VERSION"))]
#[non_exhaustive]
pub struct Config {
	/// The QUIC/TLS configuration for the server.
	#[command(flatten)]
	#[serde(default)]
	pub server: moq_native::ServerConfig,

	/// The QUIC/TLS configuration for the client. (clustering only)
	#[command(flatten)]
	#[serde(default)]
	pub client: moq_native::ClientConfig,

	/// Log configuration.
	#[command(flatten)]
	#[serde(default)]
	pub log: moq_native::Log,

	/// Cluster configuration.
	#[command(flatten)]
	#[serde(default)]
	pub cluster: ClusterConfig,

	/// Authentication configuration.
	#[command(flatten)]
	#[serde(default)]
	pub auth: AuthConfig,

	/// Optionally run a TCP HTTP/WebSocket server.
	#[command(flatten)]
	#[serde(default)]
	pub web: WebConfig,

	/// Stats publishing configuration. Disabled unless `stats.enabled = true`.
	#[command(flatten)]
	#[serde(default)]
	pub stats: StatsConfig,

	/// Group cache sizing. Unbounded unless `cache.capacity` or `cache.headroom`
	/// is set.
	#[command(flatten)]
	#[serde(default)]
	pub cache: CacheConfig,

	/// Internal (ops) listener for `/metrics`, `/health`, etc. Disabled unless
	/// `internal.listen` is set.
	#[command(flatten)]
	#[serde(default)]
	pub internal: InternalConfig,

	/// If provided, load the configuration from this file.
	#[serde(default)]
	pub file: Option<String>,

	/// Iroh specific configuration, used for both a client and server.
	#[command(flatten)]
	#[serde(default)]
	#[cfg(feature = "iroh")]
	pub iroh: moq_native::iroh::EndpointConfig,
}

impl Config {
	/// Parses configuration from CLI arguments, optionally merging with a
	/// TOML file specified via the positional `file` argument. Also initializes
	/// the logger.
	pub fn load() -> anyhow::Result<Self> {
		let config = Self::parse_and_merge(std::env::args_os())?;
		config.log.init()?;
		tracing::trace!(?config, "final config");
		Ok(config)
	}

	/// Pure version of [`Self::load`] without logger init, so tests can drive
	/// it with synthetic args and inspect the result.
	///
	/// Merge order: CLI args (the positional `file` and any flags) → TOML file
	/// (if `file` is set) → CLI args re-applied so explicit flags / env vars
	/// override TOML.
	///
	/// # Pitfall (see `rs/CLAUDE.md` and `tests` below)
	///
	/// The final `update_from` re-runs the clap parser over `args`. For
	/// fields typed as bare `bool`, an absent CLI flag writes
	/// `Default::default()` (i.e. `false`) over the TOML value, silently
	/// disabling settings that the TOML enabled. Type any new flag that
	/// should be TOML-overridable as `Option<bool>` (or other `Option<T>`)
	/// — those are left untouched when the CLI arg is absent.
	pub(crate) fn parse_and_merge<I, T>(args: I) -> anyhow::Result<Self>
	where
		I: IntoIterator<Item = T>,
		T: Into<std::ffi::OsString> + Clone,
	{
		let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
		let mut config = Config::parse_from(&args);
		if let Some(file) = config.file.clone() {
			config = toml::from_str(&std::fs::read_to_string(file)?)?;
			config.update_from(&args);
		}
		Ok(config)
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Mutex;

	use super::*;

	/// Serializes tests that touch `MOQ_STATS_*`. Cargo runs tests in parallel
	/// within a single binary, and `env::set_var` / `remove_var` are not
	/// thread-safe with concurrent env reads (which is why they're `unsafe` as
	/// of Rust 1.80). Any test that mutates this env must hold this lock.
	static STATS_ENV_LOCK: Mutex<()> = Mutex::new(());

	/// Regression test for the clap+TOML interaction documented on
	/// `Config::parse_and_merge`. A TOML file that enables stats with no
	/// overriding CLI flag must still produce `stats.enabled == Some(true)`.
	///
	/// Before the fix, `stats.enabled` was a bare `bool`. `update_from` would
	/// re-run the clap parser over args containing no `--stats-enabled`, which
	/// wrote the default `false` over the TOML's `true`, silently disabling
	/// stats publishing for any deployment that configured it via TOML.
	#[test]
	fn cli_does_not_clobber_toml_stats_enabled() {
		let _guard = STATS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// clap reads MOQ_STATS_* via `env = ...`. If the host environment has
		// one set, the test would pass for the wrong reason. Clear them for the
		// duration of this test (lock above serializes with sibling tests).
		// SAFETY: STATS_ENV_LOCK ensures no other test in this binary touches
		// these env vars concurrently.
		unsafe { std::env::remove_var("MOQ_STATS_ENABLED") };
		unsafe { std::env::remove_var("MOQ_STATS_DEPTH") };

		let toml = r#"
[stats]
enabled = true
interval = 5
node = "localhost"
depth = 2
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.stats.enabled,
			Some(true),
			"TOML's stats.enabled=true must not be clobbered by the CLI re-parse \
			 (any new bare-bool field on a flatten-derived config will have the same bug; \
			 type it as Option<bool>)"
		);
		// The `interval` flag must survive the CLI re-parse the same way.
		// It's typed as `Option<u64>` rather than a bare numeric type for
		// exactly this reason.
		assert_eq!(config.stats.interval, Some(5));
		assert_eq!(config.stats.node.as_deref(), Some("localhost"));
		assert_eq!(config.stats.depth, Some(2));
	}

	/// Serializes tests that touch `MOQ_CACHE_*`. Same rationale as
	/// `STATS_ENV_LOCK`.
	static CACHE_ENV_LOCK: Mutex<()> = Mutex::new(());

	/// Regression test for the clap+TOML clobber bug applied to `[cache]`. Both
	/// fields are `Option<String>` so a TOML-configured cache size survives the
	/// CLI re-parse when no `--cache-*` flag is passed.
	#[test]
	fn cli_does_not_clobber_toml_cache() {
		let _guard = CACHE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: CACHE_ENV_LOCK ensures no other test in this binary touches
		// these env vars concurrently.
		unsafe {
			std::env::remove_var("MOQ_CACHE_CAPACITY");
			std::env::remove_var("MOQ_CACHE_HEADROOM");
		}

		let toml = r#"
[cache]
capacity = "8GiB"
headroom = "10%"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cache-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.cache.capacity.as_deref(),
			Some("8GiB"),
			"TOML's cache.capacity must not be clobbered by the CLI re-parse"
		);
		assert_eq!(config.cache.headroom.as_deref(), Some("10%"));
	}

	/// Serializes tests that touch `MOQ_SERVER_PREFERRED_V4` / `_V6`. Same
	/// rationale as `STATS_ENV_LOCK`.
	static PREFERRED_ENV_LOCK: Mutex<()> = Mutex::new(());

	/// Regression test for the same clap+TOML clobber bug applied to the
	/// `preferred_v4` / `preferred_v6` fields on `moq-native::ServerConfig`.
	/// If either field is ever re-typed as a bare `SocketAddrV4` / `SocketAddrV6`
	/// (without `Option<>`), the CLI re-parse will overwrite the TOML value
	/// with `Default::default()` and silently disable the
	/// preferred_address transport parameter for deployments configured via
	/// TOML. This test asserts the TOML value survives an absent CLI flag.
	#[test]
	fn cli_does_not_clobber_toml_preferred_addresses() {
		let _guard = PREFERRED_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: PREFERRED_ENV_LOCK ensures no other test in this binary
		// touches these env vars concurrently.
		unsafe {
			std::env::remove_var("MOQ_SERVER_PREFERRED_V4");
			std::env::remove_var("MOQ_SERVER_PREFERRED_V6");
		}

		let toml = r#"
[server.quic]
preferred_v4 = "192.0.2.1:443"
preferred_v6 = "[2001:db8::1]:443"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("preferred-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.server.quic.preferred_v4,
			Some("192.0.2.1:443".parse().unwrap()),
			"TOML's server.quic.preferred_v4 must not be clobbered by the CLI re-parse"
		);
		assert_eq!(
			config.server.quic.preferred_v6,
			Some("[2001:db8::1]:443".parse().unwrap()),
			"TOML's server.quic.preferred_v6 must not be clobbered by the CLI re-parse"
		);
	}

	/// Serializes tests that touch `MOQ_*_QUIC_CONGESTION_CONTROL`. Same
	/// rationale as `STATS_ENV_LOCK`.
	static CONGESTION_ENV_LOCK: Mutex<()> = Mutex::new(());

	/// Regression test for the clap+TOML clobber bug applied to the
	/// `congestion_control` fields on the quic sections of
	/// `moq-native::ServerConfig` / `ClientConfig`. The fields must stay
	/// `Option<CongestionControl>` so a TOML-selected family survives the
	/// CLI re-parse when no `--*-quic-congestion-control` flag is passed.
	#[test]
	fn cli_does_not_clobber_toml_congestion_control() {
		let _guard = CONGESTION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: CONGESTION_ENV_LOCK ensures no other test in this binary
		// touches these env vars concurrently.
		unsafe {
			std::env::remove_var("MOQ_SERVER_QUIC_CONGESTION_CONTROL");
			std::env::remove_var("MOQ_CLIENT_QUIC_CONGESTION_CONTROL");
		}

		let toml = r#"
[server.quic]
congestion_control = "delay"

[client.quic]
congestion_control = "loss"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("congestion-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.server.quic.congestion_control,
			Some(moq_native::quic::CongestionControl::Delay),
			"TOML's server.quic.congestion_control must not be clobbered by the CLI re-parse"
		);
		assert_eq!(
			config.client.quic.congestion_control,
			Some(moq_native::quic::CongestionControl::Loss),
			"TOML's client.quic.congestion_control must not be clobbered by the CLI re-parse"
		);
	}

	/// Serializes tests that touch `MOQ_WEB_HTTPS_*`. Same rationale as
	/// `STATS_ENV_LOCK`.
	static WEB_HTTPS_ENV_LOCK: Mutex<()> = Mutex::new(());

	#[test]
	fn cli_does_not_clobber_toml_web_https_cert_arrays() {
		let _guard = WEB_HTTPS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: WEB_HTTPS_ENV_LOCK ensures no other test in this binary
		// touches these env vars concurrently.
		unsafe {
			std::env::remove_var("MOQ_WEB_HTTPS_CERT");
			std::env::remove_var("MOQ_WEB_HTTPS_KEY");
		}

		let toml = r#"
[web.https]
listen = "127.0.0.1:4443"
cert = ["cdn.pem", "moq-pro.pem"]
key = ["cdn.key", "moq-pro.key"]
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("web-https-certs-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.web.https.cert,
			vec![
				std::path::PathBuf::from("cdn.pem"),
				std::path::PathBuf::from("moq-pro.pem")
			]
		);
		assert_eq!(
			config.web.https.key,
			vec![
				std::path::PathBuf::from("cdn.key"),
				std::path::PathBuf::from("moq-pro.key")
			]
		);
	}

	/// Explicit CLI flag must still override TOML. Belt-and-suspenders for the
	/// fix above: making `enabled: Option<bool>` shouldn't break the override
	/// path.
	#[test]
	fn cli_flag_overrides_toml_stats_enabled() {
		let _guard = STATS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: STATS_ENV_LOCK ensures no other test in this binary touches
		// this env var concurrently.
		unsafe { std::env::remove_var("MOQ_STATS_ENABLED") };

		let toml = "[stats]\nenabled = true\n";
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cli-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--stats-enabled=false"),
		];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.stats.enabled, Some(false));
	}

	/// Same clap+TOML clobber guard applied to `auth.auth_api`. It's typed as
	/// `Option<String>` so an absent `--auth-api` CLI flag must not wipe a
	/// TOML-configured value during the `update_from` re-parse.
	static AUTH_API_ENV_LOCK: Mutex<()> = Mutex::new(());

	#[test]
	fn cli_does_not_clobber_toml_auth_api() {
		let _guard = AUTH_API_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: AUTH_API_ENV_LOCK serializes this with any sibling test touching
		// the same env var.
		unsafe { std::env::remove_var("MOQ_AUTH_API") };

		let toml = r#"
[auth]
auth_api = "https://api.moq.dev/cluster/auth"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("auth-api-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.auth.auth_api.as_deref(),
			Some("https://api.moq.dev/cluster/auth"),
			"TOML's auth.auth_api must not be clobbered by the CLI re-parse",
		);
	}

	/// Same clap+TOML clobber guard for `client.system_roots`. It's typed as
	/// `Option<bool>` so an absent `--client-tls-system-roots` CLI flag must not wipe a
	/// TOML-configured value during the `update_from` re-parse. A bare `bool`
	/// would reset it to `false`, silently dropping the system roots for a
	/// cluster client that opted into trusting both system and custom roots.
	static SYSTEM_ROOTS_ENV_LOCK: Mutex<()> = Mutex::new(());

	#[test]
	fn cli_does_not_clobber_toml_system_roots() {
		let _guard = SYSTEM_ROOTS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: SYSTEM_ROOTS_ENV_LOCK serializes this with any sibling test
		// touching the same env var.
		unsafe { std::env::remove_var("MOQ_CLIENT_TLS_SYSTEM_ROOTS") };

		let toml = r#"
[client.tls]
system_roots = true
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("system-roots-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.client.tls.system_roots,
			Some(true),
			"TOML's client.tls.system_roots must not be clobbered by the CLI re-parse"
		);
	}

	/// Same clap+TOML clobber guard for `cluster.id`. It's typed as `Option<u64>`
	/// so an absent `--cluster-id` CLI flag must not wipe a TOML-configured value
	/// during the `update_from` re-parse. A bare `u64` would reset it to `0`,
	/// which the cluster treats as reserved and silently swaps for a random id,
	/// defeating the point of pinning a stable origin via TOML.
	static CLUSTER_ID_ENV_LOCK: Mutex<()> = Mutex::new(());

	#[test]
	fn cli_does_not_clobber_toml_cluster_id() {
		let _guard = CLUSTER_ID_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: CLUSTER_ID_ENV_LOCK serializes this with any sibling test
		// touching the same env var.
		unsafe { std::env::remove_var("MOQ_CLUSTER_ID") };

		let toml = r#"
[cluster]
id = 12345
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-id-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.cluster.id,
			Some(12345),
			"TOML's cluster.id must not be clobbered by the CLI re-parse"
		);
	}

	/// The per-site stats tier flags are `Option<String>`, so an absent CLI flag
	/// must not wipe a TOML value during the `update_from` re-parse.
	static TIER_ENV_LOCK: Mutex<()> = Mutex::new(());

	#[test]
	fn cli_does_not_clobber_toml_tiers() {
		let _guard = TIER_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: TIER_ENV_LOCK serializes this with any sibling test touching
		// the same env vars.
		unsafe {
			std::env::remove_var("MOQ_CLUSTER_TIER");
			std::env::remove_var("MOQ_AUTH_MTLS_TIER");
		}

		let toml = r#"
[cluster]
tier = "region"

[auth]
mtls_tier = "edge"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("tiers-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.cluster.tier.as_deref(),
			Some("region"),
			"TOML cluster.tier must survive"
		);
		assert_eq!(
			config.auth.mtls_tier.as_deref(),
			Some("edge"),
			"TOML auth.mtls_tier must survive"
		);
	}

	/// Same clap+TOML clobber guard for the stream listeners. The `[server.unix]`
	/// bind (`Option<PathBuf>`) and its peer-credential allowlist must survive the
	/// `update_from` re-parse when their CLI flags are absent, or a TOML-configured
	/// Unix listener (and its allowlist) gets silently dropped.
	#[cfg(all(feature = "uds", unix))]
	static SERVER_UNIX_ENV_LOCK: Mutex<()> = Mutex::new(());

	#[cfg(all(feature = "uds", unix))]
	#[test]
	fn cli_does_not_clobber_toml_server_unix() {
		let _guard = SERVER_UNIX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: SERVER_UNIX_ENV_LOCK serializes this with any sibling test
		// touching the same env vars.
		unsafe {
			std::env::remove_var("MOQ_SERVER_UNIX_BIND");
			std::env::remove_var("MOQ_SERVER_UNIX_ALLOW_UID");
		}

		let toml = r#"
[server]
bind = "[::]:443"

[server.unix]
bind = "/run/moq/internal.sock"

[server.unix.allow]
uid = [1001]
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("server-unix-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(config.server.bind.as_deref(), Some("[::]:443"));
		assert_eq!(
			config.server.unix.bind.as_deref(),
			Some(std::path::Path::new("/run/moq/internal.sock")),
			"TOML's server.unix.bind must not be clobbered by the CLI re-parse"
		);
		assert_eq!(
			config.server.unix.allow.expect("allow present").uid,
			vec![1001],
			"TOML's server.unix.allow must not be clobbered by the CLI re-parse"
		);
	}

	#[test]
	fn cli_flag_overrides_toml_cluster_id() {
		let _guard = CLUSTER_ID_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: CLUSTER_ID_ENV_LOCK serializes this with any sibling test
		// touching the same env var.
		unsafe { std::env::remove_var("MOQ_CLUSTER_ID") };

		let toml = "[cluster]\nid = 12345\n";
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-id-cli-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--cluster-id=67890"),
		];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.id, Some(67890));
	}

	/// Serializes tests that touch `MOQ_INTERNAL_LISTEN`. Same rationale as
	/// `STATS_ENV_LOCK`.
	static INTERNAL_ENV_LOCK: Mutex<()> = Mutex::new(());

	/// Regression test for the same clap+TOML clobber bug on `internal.listen`
	/// (the `--internal-listen` / `MOQ_INTERNAL_LISTEN` ops listener). It's an
	/// `Option<SocketAddr>`, so an absent CLI flag must leave the TOML value
	/// intact; if it were ever re-typed to a bare `SocketAddr`, the `update_from`
	/// re-parse would overwrite a TOML-configured listener with the default.
	#[test]
	fn cli_does_not_clobber_toml_internal_listen() {
		let _guard = INTERNAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// SAFETY: INTERNAL_ENV_LOCK serializes this with any sibling test
		// touching the same env var.
		unsafe { std::env::remove_var("MOQ_INTERNAL_LISTEN") };

		let toml = "[internal]\nlisten = \"127.0.0.1:9101\"\n";
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("internal-listen-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.internal.listen,
			Some("127.0.0.1:9101".parse().unwrap()),
			"TOML's internal.listen must not be clobbered by the CLI re-parse"
		);
	}
}
