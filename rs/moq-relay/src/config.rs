use anyhow::Context;
use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::{AuthConfig, ClusterConfig, StatsConfig, WebConfig};

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
	/// # Pitfall (see `CLAUDE.md` and `tests` below)
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
		merge_from_args(args, |config: &Config| config.file.clone())
	}
}

/// Parse a clap config from CLI args, merging a TOML file if one is named.
///
/// This is the generic core of [`Config::load`], exposed so embedders that
/// **flatten** [`Config`] into their own clap parser (to add extra flags
/// alongside every relay flag) can reuse the exact same merge semantics:
///
/// ```no_run
/// #[derive(clap::Parser, serde::Deserialize)]
/// struct MyConfig {
///     #[command(flatten)]
///     #[serde(flatten)]
///     relay: moq_relay::Config,
///     // CLI/env only: `#[serde(skip)]` keeps this out of the TOML so the
///     // clobber pitfall below can't apply to it (an absent flag overwriting a
///     // TOML value on the re-parse). Drop the skip + use `Option<T>` if you
///     // want it TOML-settable.
///     #[arg(long)]
///     #[serde(skip)]
///     my_flag: bool,
/// }
///
/// let config: MyConfig = moq_relay::load_config(|c| c.relay.file.clone())?;
/// config.relay.log.init()?;
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// `file` extracts the optional TOML path from the parsed value (e.g.
/// `|c| c.relay.file.clone()`). Unlike [`Config::load`] this does NOT initialize
/// the logger; call `config.relay.log.init()` yourself once you have the value.
///
/// The clap+TOML clobber pitfall documented on [`Config::load`] applies to the
/// flattening config too: type any extra TOML-overridable flag as `Option<T>`.
pub fn load_config<T, F>(file: F) -> anyhow::Result<T>
where
	T: Parser + serde::de::DeserializeOwned,
	F: FnOnce(&T) -> Option<String>,
{
	merge_from_args(std::env::args_os(), file)
}

/// Shared implementation behind [`Config::parse_and_merge`] and [`load_config`].
/// Merge order: CLI args -> TOML file (if named) -> CLI args re-applied so
/// explicit flags / env vars win over the file.
fn merge_from_args<I, A, T, F>(args: I, file: F) -> anyhow::Result<T>
where
	I: IntoIterator<Item = A>,
	A: Into<std::ffi::OsString> + Clone,
	T: Parser + serde::de::DeserializeOwned,
	F: FnOnce(&T) -> Option<String>,
{
	let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
	let mut config = T::parse_from(&args);
	if let Some(path) = file(&config) {
		let text = std::fs::read_to_string(&path).with_context(|| format!("reading config file {path}"))?;
		config = toml::from_str(&text).with_context(|| format!("parsing config file {path}"))?;
		config.update_from(&args);
	}
	Ok(config)
}

#[cfg(test)]
mod tests {
	use std::sync::Mutex;

	use super::*;

	/// Serializes tests that touch `MOQ_STATS_ENABLED`. Cargo runs tests in
	/// parallel within a single binary, and `env::set_var` / `remove_var` are
	/// not thread-safe with concurrent env reads (which is why they're `unsafe`
	/// as of Rust 1.80). Any test that mutates this env must hold this lock.
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
		// clap reads MOQ_STATS_ENABLED via `env = ...`. If the host environment
		// has it set, the test would pass for the wrong reason. Clear it for
		// the duration of this test (lock above serializes with sibling tests).
		// SAFETY: STATS_ENV_LOCK ensures no other test in this binary touches
		// this env var concurrently.
		unsafe { std::env::remove_var("MOQ_STATS_ENABLED") };

		let toml = r#"
[stats]
enabled = true
interval = 5
node = "localhost"
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
[server]
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
			config.server.preferred_v4,
			Some("192.0.2.1:443".parse().unwrap()),
			"TOML's server.preferred_v4 must not be clobbered by the CLI re-parse"
		);
		assert_eq!(
			config.server.preferred_v6,
			Some("[2001:db8::1]:443".parse().unwrap()),
			"TOML's server.preferred_v6 must not be clobbered by the CLI re-parse"
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
	/// `Option<bool>` so an absent `--tls-system-roots` CLI flag must not wipe a
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

	/// The embed contract: a downstream binary can flatten [`Config`] into its
	/// own clap parser (adding extra flags alongside every relay flag) and reuse
	/// the relay's CLI -> TOML -> CLI merge via the generic [`merge_from_args`]
	/// (the core of the public [`load_config`]). Exercises all four corners at
	/// once: clap flatten + the positional `file`, serde flatten of `Config`
	/// (its `deny_unknown_fields` must not be enforced *through* the flatten, or
	/// an embedder's own top-level TOML key would be rejected), and the CLI
	/// re-apply landing the embedder's extra flag.
	#[test]
	fn embedder_can_flatten_config() {
		#[derive(clap::Parser, serde::Deserialize, Debug)]
		struct Embed {
			#[command(flatten)]
			#[serde(flatten)]
			relay: Config,

			/// An embedder-specific flag the relay knows nothing about.
			#[arg(long = "worker-enabled")]
			#[serde(skip)]
			worker_enabled: bool,
		}

		// `embedder_only` is a top-level key the relay's `Config` has no field
		// for. It must NOT trip `Config`'s `deny_unknown_fields` (serde can't
		// enforce that through a flatten), or embedders couldn't carry their own
		// TOML config alongside the relay's.
		let toml = "embedder_only = \"ignored\"\n\n[stats]\nenabled = true\nnode = \"embed\"\n";
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("embed-flatten.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![
			std::ffi::OsString::from("my-edge"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--worker-enabled"),
		];
		let config: Embed = merge_from_args(args, |c: &Embed| c.relay.file.clone()).expect("embed config load");

		// The relay's TOML section was applied through the flattened field...
		assert_eq!(config.relay.stats.enabled, Some(true));
		assert_eq!(config.relay.stats.node.as_deref(), Some("embed"));
		// ...and the embedder's own flag came through the CLI re-apply.
		assert!(config.worker_enabled);
	}
}
