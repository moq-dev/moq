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
	pub iroh: moq_native::IrohEndpointConfig,
}

impl Config {
	/// Parses configuration from CLI arguments, optionally merging with a
	/// TOML file specified via the positional `file` argument. Also initializes
	/// the logger.
	pub fn load() -> anyhow::Result<Self> {
		let config = Self::parse_and_merge(std::env::args_os())?;
		config.log.init();
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
retention = 20
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
		// New `interval` / `retention` flags must survive the CLI re-parse
		// the same way. They're typed as `Option<u64>` / `Option<u32>`
		// rather than bare numeric types for exactly this reason.
		assert_eq!(config.stats.interval, Some(5));
		assert_eq!(config.stats.retention, Some(20));
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
}
