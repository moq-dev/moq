use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Range;

/// moq-bench configuration, loadable from CLI arguments, environment variables,
/// or a TOML file. CLI flags always win over the TOML file.
///
/// Each `[min, max]` range is rolled once per connection, so a single config can
/// describe a heterogeneous swarm (e.g. some connections at 24fps, others at 60).
#[derive(usage::Cli, Clone, Debug, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[usage(name = "moq-bench", version = env!("VERSION"))]
#[usage(completion)]
#[non_exhaustive]
pub struct Config {
	/// The broadcast namespace prefix. Each broadcast is published under
	/// `<name>/<run>/<connection>/<index>` and subscribers discover peers under `<name>`.
	#[usage(long, env = "MOQ_BENCH_NAME", default = "bench")]
	pub name: String,

	/// Run a 1:N benchmark around one named broadcast. The first connection
	/// publishes `<name>/<run>/<fanout>` and every remaining connection subscribes.
	#[usage(long, env = "MOQ_BENCH_FANOUT")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub fanout: Option<String>,

	/// Spread connection and subscription startup over this duration to avoid a thundering herd.
	#[usage(long, env = "MOQ_BENCH_STARTUP", default = "10s")]
	pub startup: moq_tokio::Duration,

	/// Stop the benchmark after this duration. Runs until interrupted if unset.
	#[usage(long, env = "MOQ_BENCH_DURATION")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration: Option<moq_tokio::Duration>,

	/// How often to log throughput stats.
	#[usage(long, env = "MOQ_BENCH_REPORT", default = "1s")]
	pub report: moq_tokio::Duration,

	/// Number of connections (A) to establish. Rolled once for the whole run.
	#[usage(long, env = "MOQ_BENCH_CONNECTIONS", default = "1")]
	pub connections: Range,

	/// Broadcasts published per connection (B), each with a single track.
	///
	/// `Option` because `--fanout` refuses to run alongside an explicit shape, and
	/// a materialized default cannot say whether one was given.
	#[usage(long, env = "MOQ_BENCH_BROADCASTS")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub broadcasts: Option<Range>,

	/// Other broadcasts each connection subscribes to (C), discovered via announcements.
	///
	/// `Option` for the same reason as [`Self::broadcasts`].
	#[usage(long, env = "MOQ_BENCH_SUBSCRIBE")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub subscribe: Option<Range>,

	/// Frames per second per track (D). Zero leaves the track idle.
	#[usage(long, env = "MOQ_BENCH_FPS", default = "30")]
	pub fps: Range,

	/// Bytes per frame (E).
	#[usage(long, env = "MOQ_BENCH_FRAME_SIZE", default = "1200")]
	pub frame_size: Range,

	/// Zeroed frames per group (F) following the JSON keyframe. May be zero.
	#[usage(long, env = "MOQ_BENCH_GROUP_SIZE", default = "60")]
	pub group_size: Range,

	/// Write machine-readable stats to this file: one JSON line of cumulative
	/// counters per report interval. Truncates on start.
	#[usage(long, env = "MOQ_BENCH_OUTPUT")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub output: Option<std::path::PathBuf>,

	/// The MoQ client (QUIC/TLS) configuration.
	#[usage(flatten)]
	#[serde(default)]
	pub client: moq_tokio::connect::Config,

	/// QUIC transport tuning (`--quic-*`).
	#[usage(flatten)]
	#[serde(default)]
	pub quic: moq_tokio::quic::Config,

	/// Log configuration.
	#[usage(flatten)]
	#[serde(default)]
	pub log: moq_tokio::Log,

	/// Load configuration from this TOML file. CLI flags still take precedence.
	#[usage(long)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub file: Option<String>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			name: "bench".into(),
			fanout: None,
			startup: Duration::from_secs(10).into(),
			duration: None,
			report: Duration::from_secs(1).into(),
			connections: Range::new(1, 1),
			broadcasts: None,
			subscribe: None,
			fps: Range::new(30, 30),
			frame_size: Range::new(1200, 1200),
			group_size: Range::new(60, 60),
			output: None,
			client: Default::default(),
			quic: Default::default(),
			log: Default::default(),
			file: None,
		}
	}
}

impl Config {
	/// Parse from CLI args, optionally merging a TOML file, then init the logger.
	pub fn load() -> anyhow::Result<Self> {
		let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
		// `#[usage(completion)]` installs the `__complete_word__` interception in the
		// generated `parse()`, which this loader does not use: without this the request
		// would reach the ordinary grammar and be refused. Recognized before the parse,
		// because a completion is not a command this binary runs.
		if let Some(reply) = Self::completion_request(args.get(1..).unwrap_or_default()) {
			print!("{reply}");
			std::process::exit(0);
		}
		let config = Self::parse_and_merge(args)?;
		config.log.init()?;
		tracing::trace!(?config, "final config");
		Ok(config)
	}

	/// Refuse a config parsed from released spellings, naming what replaced each.
	///
	/// Checked in `parse_and_merge`, before anything reads the config: those
	/// spellings land on hidden fields that nothing honors, so continuing would dial
	/// with settings the command line never asked for.
	fn check_deprecated(&self) -> anyhow::Result<()> {
		let mut deprecated = self.client.deprecated();
		deprecated.extend(self.quic.deprecated());
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
		Ok(())
	}

	/// Merge defaults and environment, then TOML, then explicit CLI flags.
	pub(crate) fn parse_and_merge<I, T>(args: I) -> anyhow::Result<Self>
	where
		I: IntoIterator<Item = T>,
		T: Into<std::ffi::OsString> + Clone,
	{
		let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
		let argv = args
			.iter()
			.skip(1)
			.map(std::ffi::OsString::as_os_str)
			.collect::<Vec<_>>();
		// Help and version are questions rather than failures. Answered and exited
		// here, because wrapping them renders an empty `anyhow` error and exits
		// non-zero having printed nothing. A real failure still comes back as an
		// error, so a caller that parses synthetic args keeps its Result.
		let mut config = match Config::parse_from(&argv) {
			Ok(config) => config,
			Err(err) => {
				let answer = moq_tokio::cli::answer(Config::spec(), Config::command(), &argv, err);
				if answer.is_question() {
					answer.exit();
				}
				anyhow::bail!("{}", answer.message());
			}
		};
		if let Some(file) = config.file.clone() {
			let mut merged = toml::Value::try_from(&config)?;
			let source = std::fs::read_to_string(file)?;
			let mut file = toml::from_str::<toml::Value>(&source)?;
			normalize_client_aliases(&mut file)?;
			merge_toml(&mut merged, file);
			config = merged.try_into()?;
			config.update_from(&argv);
		}
		config.check_deprecated()?;
		// `Stats::report` feeds this into `tokio::time::interval`, which panics on a
		// zero period. Reject it up front with a clear message.
		anyhow::ensure!(!config.report().is_zero(), "--report must be greater than 0s");
		if let Some(fanout) = &config.fanout {
			anyhow::ensure!(!fanout.is_empty(), "--fanout must name a broadcast");
			let connections = config.connections();
			anyhow::ensure!(
				connections.min.min(connections.max) >= 2,
				"--fanout requires at least 2 connections (one publisher and one subscriber)"
			);
			anyhow::ensure!(
				config.broadcasts.is_none() && config.subscribe.is_none(),
				"--fanout owns the publish/subscribe shape; omit --broadcasts and --subscribe"
			);
		}
		Ok(config)
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	/// The named 1:N broadcast, when fan-out mode is enabled.
	pub fn fanout(&self) -> Option<&str> {
		self.fanout.as_deref()
	}

	pub fn startup(&self) -> Duration {
		self.startup.into_std()
	}

	pub fn report(&self) -> Duration {
		self.report.into_std()
	}

	pub fn connections(&self) -> Range {
		self.connections
	}

	pub fn broadcasts(&self) -> Range {
		self.broadcasts.unwrap_or(Range::new(1, 1))
	}

	pub fn subscribe(&self) -> Range {
		self.subscribe.unwrap_or(Range::new(0, 0))
	}

	pub fn fps(&self) -> Range {
		self.fps
	}

	pub fn frame_size(&self) -> Range {
		self.frame_size
	}

	pub fn group_size(&self) -> Range {
		self.group_size
	}

	/// Whether this configuration expects subscribers to receive media.
	pub fn expects_delivery(&self) -> bool {
		self.fanout.is_some() || self.subscribe().min.max(self.subscribe().max) > 0
	}

	/// Whether any connection may publish a generated broadcast.
	pub fn publishes(&self) -> bool {
		self.broadcasts().min.max(self.broadcasts().max) > 0
	}
}

fn normalize_client_aliases(value: &mut toml::Value) -> anyhow::Result<()> {
	let Some(connect) = value
		.as_table_mut()
		.and_then(|root| root.get_mut("client"))
		.and_then(toml::Value::as_table_mut)
	else {
		return Ok(());
	};
	rename_toml_key(connect, "connect", "url")?;
	rename_toml_key(connect, "failover_delay", "race")?;
	if let Some(tls) = connect.get_mut("tls").and_then(toml::Value::as_table_mut) {
		rename_toml_key(tls, "disable_verify", "insecure")?;
	}
	Ok(())
}

fn rename_toml_key(table: &mut toml::Table, alias: &str, canonical: &str) -> anyhow::Result<()> {
	let Some(value) = table.remove(alias) else {
		return Ok(());
	};
	anyhow::ensure!(
		!table.contains_key(canonical),
		"TOML specifies both `{alias}` and `{canonical}`"
	);
	table.insert(canonical.into(), value);
	Ok(())
}

fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
	match (base, overlay) {
		(toml::Value::Table(base), toml::Value::Table(overlay)) => {
			for (key, value) in overlay {
				match base.get_mut(&key) {
					Some(base) => merge_toml(base, value),
					None => {
						base.insert(key, value);
					}
				}
			}
		}
		(base, overlay) => *base = overlay,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cli_overrides_toml() {
		let toml = r#"
connections = 100
fps = "24:60"

[client]
connect = "https://example.com"
tls.insecure = true

[client.websocket]
enabled = false
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
		assert_eq!(config.client.url.as_ref().unwrap().as_str(), "https://example.com/");
		assert_eq!(config.client.tls.insecure, Some(true));
		assert_eq!(config.client.websocket.enabled, Some(false));

		// CLI flag wins over the TOML value.
		let args = vec![
			std::ffi::OsString::from("moq-bench"),
			std::ffi::OsString::from("--file"),
			path.into(),
			std::ffi::OsString::from("--connections"),
			std::ffi::OsString::from("5:10"),
			std::ffi::OsString::from("--connect-websocket-enabled=true"),
		];
		let config = Config::parse_and_merge(args).unwrap();
		assert_eq!(config.connections(), Range::new(5, 10));
		// Untouched TOML field is still intact.
		assert_eq!(config.fps(), Range::new(24, 60));
		assert_eq!(config.client.websocket.enabled, Some(true));
	}

	#[test]
	fn output_survives_toml_merge() {
		// Optional fields participate in the same source order as concrete defaults.
		let toml = r#"
output = "stats.jsonl"

[client]
connect = "https://example.com"
"#;
		let dir = std::env::temp_dir().join("moq-bench-output-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("bench.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![
			std::ffi::OsString::from("moq-bench"),
			std::ffi::OsString::from("--file"),
			path.into(),
		];
		let config = Config::parse_and_merge(args).unwrap();
		assert_eq!(config.output.as_deref(), Some(std::path::Path::new("stats.jsonl")));
	}

	#[test]
	fn zero_report_is_rejected() {
		// A zero report interval would panic `tokio::time::interval`; reject it early.
		let err = Config::parse_and_merge(["moq-bench", "--report", "0s"]).unwrap_err();
		assert!(err.to_string().contains("report"), "unexpected error: {err}");
	}

	/// `main` reads `client.url` as a plain field, which a released
	/// `--client-connect` never reaches. Refuse the run and name `--connect`, rather
	/// than fail the `--connect is required` check the operator thinks they passed.
	#[test]
	fn the_released_connect_spelling_is_refused() {
		let err = Config::parse_and_merge(["moq-bench", "--client-connect", "https://relay.example.com"])
			.expect_err("must refuse")
			.to_string();
		assert!(
			err.contains("--client-connect / MOQ_CLIENT_CONNECT -> --connect / MOQ_CONNECT"),
			"{err}"
		);
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
		assert_eq!(config.fanout(), None);
		assert!(!config.expects_delivery());
		assert!(config.publishes());
	}

	#[test]
	fn fanout_owns_the_connection_roles() {
		let config = Config::parse_and_merge(["moq-bench", "--fanout", "chat", "--connections", "100"]).unwrap();
		assert_eq!(config.fanout(), Some("chat"));
		assert!(config.expects_delivery());

		let err = Config::parse_and_merge([
			"moq-bench",
			"--fanout",
			"chat",
			"--connections",
			"100",
			"--subscribe",
			"1",
		])
		.unwrap_err();
		assert!(err.to_string().contains("owns the publish/subscribe shape"));
	}

	#[test]
	fn fanout_requires_a_publisher_and_subscriber() {
		let err = Config::parse_and_merge(["moq-bench", "--fanout", "chat", "--connections", "1"]).unwrap_err();
		assert!(err.to_string().contains("at least 2 connections"));
	}

	/// Help and version are answered, not wrapped as failures.
	///
	/// Usage renders those variants as an empty string through `render_failure`,
	/// because the generated `parse()` is expected to take them first. This loader
	/// parses twice for the TOML merge and never reaches that code, so wrapping
	/// them exited non-zero having printed nothing.
	#[test]
	fn help_and_version_are_questions() {
		for flag in ["--help", "-h", "--version", "-V"] {
			let argv = [std::ffi::OsStr::new(flag)];
			let err = Config::parse_from(&argv).unwrap_err();
			let answer = moq_tokio::cli::answer(Config::spec(), Config::command(), &argv, err);
			assert!(answer.is_question(), "{flag} was treated as a failure");
			assert!(!answer.message().trim().is_empty(), "{flag} rendered nothing");
		}
	}

	/// The spec is named for the binary, not for the struct that declares it.
	///
	/// Usage takes the program name from the type unless told otherwise, so an
	/// undeclared name renders every usage line and completion as `config`.
	#[test]
	fn the_spec_is_named_for_the_binary() {
		assert_eq!(Config::spec().bin.unwrap_or(Config::spec().name), "moq-bench");
	}
}
