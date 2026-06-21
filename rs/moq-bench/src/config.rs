use std::str::FromStr;
use std::time::Duration;

use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::Range;

/// moq-bench configuration, loadable from CLI arguments, environment variables,
/// or a TOML file. CLI flags always win over the TOML file.
///
/// Each `[min, max]` range is rolled once per connection, so a single config can
/// describe a heterogeneous swarm (e.g. some connections at 24fps, others at 60).
#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
#[command(version = env!("VERSION"))]
#[non_exhaustive]
pub struct Config {
	/// The URL of the MoQ server to benchmark (e.g. `https://relay.example.com`).
	#[arg(long, env = "MOQ_BENCH_URL")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub url: Option<url::Url>,

	/// The broadcast namespace prefix. Each broadcast is published under
	/// `<name>/<run>/<connection>/<index>` and subscribers discover peers under `<name>`.
	#[arg(long, env = "MOQ_BENCH_NAME")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,

	/// Spread connection and subscription startup over this duration to avoid a thundering herd.
	#[arg(long, value_parser = humantime::parse_duration, env = "MOQ_BENCH_STARTUP")]
	#[serde(default, with = "humantime_serde::option", skip_serializing_if = "Option::is_none")]
	pub startup: Option<Duration>,

	/// Stop the benchmark after this duration. Runs until interrupted if unset.
	#[arg(long, value_parser = humantime::parse_duration, env = "MOQ_BENCH_DURATION")]
	#[serde(default, with = "humantime_serde::option", skip_serializing_if = "Option::is_none")]
	pub duration: Option<Duration>,

	/// How often to log throughput stats.
	#[arg(long, value_parser = humantime::parse_duration, env = "MOQ_BENCH_REPORT")]
	#[serde(default, with = "humantime_serde::option", skip_serializing_if = "Option::is_none")]
	pub report: Option<Duration>,

	/// Number of connections (A) to establish. Rolled once for the whole run.
	#[arg(long, value_parser = Range::from_str, env = "MOQ_BENCH_CONNECTIONS")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub connections: Option<Range>,

	/// Broadcasts published per connection (B), each with a single track.
	#[arg(long, value_parser = Range::from_str, env = "MOQ_BENCH_BROADCASTS")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub broadcasts: Option<Range>,

	/// Other broadcasts each connection subscribes to (C), discovered via announcements.
	#[arg(long, value_parser = Range::from_str, env = "MOQ_BENCH_SUBSCRIBE")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub subscribe: Option<Range>,

	/// Frames per second per track (D). Zero leaves the track idle.
	#[arg(long, value_parser = Range::from_str, env = "MOQ_BENCH_FPS")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub fps: Option<Range>,

	/// Bytes per frame (E).
	#[arg(long, value_parser = Range::from_str, env = "MOQ_BENCH_FRAME_SIZE")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub frame_size: Option<Range>,

	/// Zeroed frames per group (F) following the JSON keyframe. May be zero.
	#[arg(long, value_parser = Range::from_str, env = "MOQ_BENCH_GROUP_SIZE")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub group_size: Option<Range>,

	/// The MoQ client (QUIC/TLS) configuration.
	#[command(flatten)]
	#[serde(default)]
	pub client: moq_native::ClientConfig,

	/// Log configuration.
	#[command(flatten)]
	#[serde(default)]
	pub log: moq_native::Log,

	/// Load configuration from this TOML file. CLI flags still take precedence.
	#[arg(long)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub file: Option<String>,
}

impl Config {
	/// Parse from CLI args, optionally merging a TOML file, then init the logger.
	pub fn load() -> anyhow::Result<Self> {
		let config = Self::parse_and_merge(std::env::args_os())?;
		config.log.init()?;
		tracing::trace!(?config, "final config");
		Ok(config)
	}

	/// Merge order mirrors moq-relay: CLI args (including `--file`) -> TOML file
	/// (if set) -> CLI args re-applied so explicit flags override the TOML.
	///
	/// Every overridable field is `Option<T>`, so an absent CLI flag leaves the
	/// TOML value untouched during the final `update_from` re-parse. See
	/// `rs/CLAUDE.md` for why bare fields would silently clobber the TOML.
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
		// `Stats::report` feeds this into `tokio::time::interval`, which panics on a
		// zero period. Reject it up front with a clear message.
		anyhow::ensure!(!config.report().is_zero(), "--report must be greater than 0s");
		Ok(config)
	}

	pub fn name(&self) -> &str {
		self.name.as_deref().unwrap_or("bench")
	}

	pub fn startup(&self) -> Duration {
		self.startup.unwrap_or(Duration::from_secs(10))
	}

	pub fn report(&self) -> Duration {
		self.report.unwrap_or(Duration::from_secs(1))
	}

	pub fn connections(&self) -> Range {
		self.connections.unwrap_or(Range::new(1, 1))
	}

	pub fn broadcasts(&self) -> Range {
		self.broadcasts.unwrap_or(Range::new(1, 1))
	}

	pub fn subscribe(&self) -> Range {
		self.subscribe.unwrap_or(Range::new(0, 0))
	}

	pub fn fps(&self) -> Range {
		self.fps.unwrap_or(Range::new(30, 30))
	}

	pub fn frame_size(&self) -> Range {
		self.frame_size.unwrap_or(Range::new(1200, 1200))
	}

	pub fn group_size(&self) -> Range {
		self.group_size.unwrap_or(Range::new(60, 60))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cli_overrides_toml() {
		let toml = r#"
url = "https://example.com"
connections = 100
fps = "24:60"

[client]
tls.disable_verify = true
"#;
		let dir = std::env::temp_dir().join("moq-bench-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("bench.toml");
		std::fs::write(&path, toml).unwrap();

		// No CLI flag: TOML values survive the re-parse.
		let args = vec![
			std::ffi::OsString::from("moq-bench"),
			std::ffi::OsString::from("--file"),
			path.clone().into(),
		];
		let config = Config::parse_and_merge(args).unwrap();
		assert_eq!(config.connections(), Range::new(100, 100));
		assert_eq!(config.fps(), Range::new(24, 60));
		assert_eq!(config.client.tls.disable_verify, Some(true));

		// CLI flag wins over the TOML value.
		let args = vec![
			std::ffi::OsString::from("moq-bench"),
			std::ffi::OsString::from("--file"),
			path.into(),
			std::ffi::OsString::from("--connections"),
			std::ffi::OsString::from("5:10"),
		];
		let config = Config::parse_and_merge(args).unwrap();
		assert_eq!(config.connections(), Range::new(5, 10));
		// Untouched TOML field is still intact.
		assert_eq!(config.fps(), Range::new(24, 60));
	}

	#[test]
	fn zero_report_is_rejected() {
		// A zero report interval would panic `tokio::time::interval`; reject it early.
		let err = Config::parse_and_merge(["moq-bench", "--report", "0s"]).unwrap_err();
		assert!(err.to_string().contains("report"), "unexpected error: {err}");
	}

	#[test]
	fn defaults_apply_without_toml() {
		let config = Config::parse_and_merge(["moq-bench"]).unwrap();
		assert_eq!(config.connections(), Range::new(1, 1));
		assert_eq!(config.broadcasts(), Range::new(1, 1));
		assert_eq!(config.subscribe(), Range::new(0, 0));
		assert_eq!(config.fps(), Range::new(30, 30));
		assert_eq!(config.frame_size(), Range::new(1200, 1200));
		assert_eq!(config.group_size(), Range::new(60, 60));
		assert_eq!(config.name(), "bench");
	}
}
