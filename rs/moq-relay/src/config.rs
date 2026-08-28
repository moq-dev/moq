use serde::{Deserialize, Serialize};

use crate::{AuthConfig, CacheConfig, ClusterConfig, InternalConfig, StatsConfig, WebConfig};

/// Top-level relay configuration, loadable from CLI arguments, environment
/// variables, or a TOML file.
/// Top-level relay configuration, as a COMPOSABLE args group.
///
/// `usage::Args` rather than `usage::Cli` on purpose: the program-level parts (a
/// name, a version, the completion command) belong to whichever binary owns the
/// process, and a `Cli` cannot be `#[usage(flatten)]`ed into another one. Keeping
/// them off this type is what lets an embedder put the relay's whole flag surface
/// inside its own CLI -- moq.pro's `edge` does exactly that -- instead of
/// re-declaring it and drifting on every flag added here. This binary wraps it in
/// a private `Cli` that adds those program-level parts; [`spec`] exposes the
/// resulting command line.
#[derive(usage::Args, Clone, Debug, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// The QUIC/TLS configuration for the server.
	#[usage(flatten)]
	#[serde(default)]
	#[serde(alias = "server")]
	pub listen: moq_tokio::listen::Config,

	/// The QUIC/TLS configuration for the client. (clustering only)
	#[usage(flatten)]
	#[serde(default)]
	#[serde(alias = "client")]
	pub connect: moq_tokio::connect::Config,

	/// QUIC transport tuning (`--quic-*`), shared by the dial and accept sides:
	/// these knobs mean the same thing whichever way the connection was opened.
	#[usage(flatten)]
	#[serde(default)]
	pub quic: moq_tokio::quic::Config,

	/// Log configuration.
	#[usage(flatten)]
	#[serde(default)]
	pub log: moq_tokio::Log,

	/// How QUIC work is laid out over threads. One shared runtime unless
	/// `runtime.workers` is set.
	#[usage(flatten)]
	#[serde(default)]
	pub runtime: crate::RuntimeConfig,

	/// Cluster configuration.
	#[usage(flatten)]
	#[serde(default)]
	pub cluster: ClusterConfig,

	/// Authentication configuration.
	#[usage(flatten)]
	#[serde(default)]
	pub auth: AuthConfig,

	/// Optionally run a TCP HTTP/WebSocket server.
	#[usage(flatten)]
	#[serde(default)]
	pub web: WebConfig,

	/// Stats publishing configuration. Disabled unless `stats.enabled = true`.
	#[usage(flatten)]
	#[serde(default)]
	pub stats: StatsConfig,

	/// Group cache sizing. Unbounded unless `cache.capacity` or `cache.headroom`
	/// is set.
	#[usage(flatten)]
	#[serde(default)]
	pub cache: CacheConfig,

	/// Internal (ops) listener for `/metrics`, `/health`, and `/nodes`. Disabled unless
	/// `internal.listen` is set.
	#[usage(flatten)]
	#[serde(default)]
	pub internal: InternalConfig,

	/// How long accepted sessions may keep running after a shutdown signal, e.g.
	/// "10s" or "500ms". The first signal sends every session a GOAWAY and waits
	/// this long for clients to reconnect elsewhere before force-closing them; a
	/// second signal exits immediately. Zero closes them at once, with no GOAWAY
	/// they would have no time to act on. Defaults to 10 seconds.
	#[usage(
		name = "drain-timeout",
		long = "drain-timeout",
		env = "MOQ_DRAIN_TIMEOUT",
		default = "10s"
	)]
	pub drain_timeout: moq_tokio::Duration,

	/// If provided, load the configuration from this file.
	#[serde(default)]
	pub file: Option<String>,

	/// Iroh specific configuration, used for both a client and server.
	#[usage(flatten)]
	#[serde(default)]
	#[cfg(feature = "iroh")]
	pub iroh: moq_tokio::iroh::EndpointConfig,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			listen: Default::default(),
			connect: Default::default(),
			quic: Default::default(),
			log: Default::default(),
			runtime: Default::default(),
			cluster: Default::default(),
			auth: Default::default(),
			web: Default::default(),
			stats: Default::default(),
			cache: Default::default(),
			internal: Default::default(),
			drain_timeout: crate::DEFAULT_DRAIN_TIMEOUT.into(),
			file: None,
			#[cfg(feature = "iroh")]
			iroh: Default::default(),
		}
	}
}

/// Top-level relay configuration, loadable from CLI arguments, environment
/// variables, or a TOML file.
//
// NB: the lines above are the `--help` description, not documentation. Usage
// renders a `Cli`'s doc comment as the program's about text, so anything written
// there is printed to users verbatim, rustdoc link syntax and all. The rationale
// for this type belongs in this ordinary comment instead.
//
// `Cli` is `Config` plus the program-level parts, which exists so `Config` can
// stay flattenable. Private, because it is an implementation detail of THIS
// binary: an embedder declares its own and flattens `Config` into it, so nothing
// outside needs to name this one. What callers do need -- the program's spec,
// for completions, docs, and the released-flag test -- is `spec`.
#[derive(usage::Cli, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq-relay", version = env!("VERSION"))]
#[usage(completion)]
struct Cli {
	#[usage(flatten)]
	config: Config,
}

/// The `moq-relay` binary's own command-line spec: every flag and environment
/// variable it accepts.
///
/// Deliberately a free function rather than a method on [`Config`]. The spec is
/// the PROGRAM's, and `Config` is a composable fragment -- an embedder that
/// flattens it has its own spec, so `Config::spec()` would hand back the wrong
/// one and read as though it were theirs.
pub fn spec() -> &'static usage::spec::Spec<'static> {
	Cli::spec()
}

impl Config {
	/// Parses configuration from CLI arguments, optionally merging with a
	/// TOML file specified via the positional `file` argument. Also initializes
	/// the logger.
	pub fn load() -> anyhow::Result<Self> {
		let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
		// `#[usage(completion)]` installs the `__complete_word__` interception in the
		// generated `parse()`, which this loader does not use: without this the request
		// would reach the ordinary grammar and be refused. Recognized before the parse,
		// because a completion is not a command this binary runs.
		if let Some(reply) = Cli::completion_request(args.get(1..).unwrap_or_default()) {
			print!("{reply}");
			std::process::exit(0);
		}
		let config = Self::parse_and_merge(args)?;
		config.log.init()?;
		tracing::trace!(?config, "final config");
		Ok(config)
	}

	/// Pure version of [`Self::load`] without logger init, so tests can drive
	/// it with synthetic args and inspect the result.
	///
	/// Merge defaults and environment, then TOML, then explicit CLI flags.
	///
	/// # Pitfall (see `rs/CLAUDE.md` and `tests` below)
	///
	/// The final `update_from` re-parses `argv` over the merged config. Usage
	/// fills a declared default only where the standing value is still empty,
	/// and a bare `bool` reading `false` is what it counts as empty: a field
	/// with `default = "true"` is therefore refilled over whatever the TOML
	/// said. Type any new flag that should be TOML-overridable as
	/// `Option<bool>` and resolve the default in code. Every other shape is
	/// safe, because a plain value always reads as present.
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
		let mut cli = match Cli::parse_from(&argv) {
			Ok(cli) => cli,
			Err(err) => {
				let answer = moq_tokio::cli::answer(Cli::spec(), Cli::command(), &argv, err);
				if answer.is_question() {
					answer.exit();
				}
				anyhow::bail!("{}", answer.message());
			}
		};
		if let Some(file) = cli.config.file.clone() {
			let mut merged = toml::Value::try_from(&cli.config)?;
			let source = std::fs::read_to_string(file)?;
			let mut file = toml::from_str::<toml::Value>(&source)?;
			normalize_toml_aliases(&mut file)?;
			merge_toml(&mut merged, file);
			cli.config = merged.try_into()?;
			// Re-applied over the merged config so explicit flags still beat the TOML.
			cli.update_from(&argv);
		}
		Ok(cli.config)
	}
}

fn normalize_toml_aliases(value: &mut toml::Value) -> anyhow::Result<()> {
	let Some(root) = value.as_table_mut() else {
		return Ok(());
	};
	rename_toml_key(root, "server", "listen")?;
	rename_toml_key(root, "client", "connect")?;

	if let Some(listen) = root.get_mut("listen").and_then(toml::Value::as_table_mut) {
		rename_toml_key(listen, "listen", "bind")?;
	}
	if let Some(connect) = root.get_mut("connect").and_then(toml::Value::as_table_mut) {
		rename_toml_key(connect, "connect", "url")?;
		rename_toml_key(connect, "failover_delay", "race")?;
		if let Some(tls) = connect.get_mut("tls").and_then(toml::Value::as_table_mut) {
			rename_toml_key(tls, "disable_verify", "insecure")?;
		}
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

impl Config {
	/// Refuse a config parsed from released spellings, then apply the relay's own
	/// defaults.
	///
	/// The check comes first because those spellings configure nothing: a relay that
	/// booted anyway would be serving on defaults, with the deployment's own
	/// `--server-bind` and TLS material silently absent.
	pub(crate) fn resolve(&mut self) -> anyhow::Result<()> {
		let mut deprecated = self.quic.deprecated();
		deprecated.extend(self.listen.deprecated());
		deprecated.extend(self.connect.deprecated());
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");

		self.quic.max_streams.get_or_insert(crate::DEFAULT_MAX_STREAMS);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_env::EnvGuard;

	/// The relay's own default still applies once the released spellings are gone.
	#[test]
	fn the_relay_default_applies() {
		let _env = EnvGuard::clear(&["MOQ_QUIC_MAX_STREAMS", "MOQ_SERVER_QUIC_MAX_STREAMS"]);

		let mut config = Cli::parse_from(&[std::ffi::OsStr::new("--quic-max-streams"), std::ffi::OsStr::new("4096")])
			.unwrap()
			.config;
		config.resolve().expect("current spellings");
		assert_eq!(config.quic.max_streams, Some(4096));

		let mut config = Cli::parse_from(&[]).unwrap().config;
		config.resolve().expect("current spellings");
		assert_eq!(config.quic.max_streams, Some(crate::DEFAULT_MAX_STREAMS));
	}

	/// A released `--server-*` spelling stops the relay and names its replacement.
	///
	/// Booting anyway is the failure this replaced: those flags land on hidden
	/// fields nothing reads, so the relay would come up on the default bind with the
	/// deployment's certificate silently absent.
	#[test]
	fn released_listen_spellings_refuse_to_boot() {
		let _env = EnvGuard::clear(&[
			"MOQ_LISTEN",
			"MOQ_SERVER_BIND",
			"MOQ_LISTEN_TLS_CERT",
			"MOQ_SERVER_TLS_CERT",
		]);

		let mut config = Cli::parse_from(&[
			std::ffi::OsStr::new("--server-bind"),
			std::ffi::OsStr::new("[::]:4443"),
			std::ffi::OsStr::new("--server-tls-cert"),
			std::ffi::OsStr::new("/tmp/cert.pem"),
			std::ffi::OsStr::new("--server-tls-key"),
			std::ffi::OsStr::new("/tmp/cert.key"),
		])
		.unwrap()
		.config;
		assert_eq!(config.listen.bind, None, "the released flag configures nothing");

		let err = config.resolve().expect_err("must refuse").to_string();
		for line in [
			"--server-bind / MOQ_SERVER_BIND -> --listen / MOQ_LISTEN",
			"--tls-cert / MOQ_SERVER_TLS_CERT -> --listen-tls-cert / MOQ_LISTEN_TLS_CERT",
			"--tls-key / MOQ_SERVER_TLS_KEY -> --listen-tls-key / MOQ_LISTEN_TLS_KEY",
		] {
			assert!(err.contains(line), "missing {line:?} from {err}");
		}
	}

	/// A released config file is parsed where the tables used to live, so the relay
	/// can name `[quic]` instead of failing with `unknown field quic`, which is what
	/// `deny_unknown_fields` would say on its own.
	#[test]
	fn released_per_role_quic_tables_refuse_to_boot() {
		let toml = r#"
[server]
listen = "[::]:443"

[server.quic]
max_streams = 4096

[client.quic]
max_streams = 64
"#;
		let mut config: Config = toml::from_str(toml).expect("released config must still parse");
		// The section renames are plain serde aliases, so those keep working.
		assert_eq!(config.listen.bind.as_deref(), Some("[::]:443"));

		let err = config.resolve().expect_err("must refuse").to_string();
		assert!(err.contains("[server.quic] -> [quic]"), "{err}");
		assert!(err.contains("[client.quic] -> [quic]"), "{err}");
		assert!(err.contains("both directions"), "{err}");
	}

	/// The canonical top-level table, which is what the demo configs use.
	#[test]
	fn the_shared_quic_table_applies() {
		let mut config: Config = toml::from_str("[quic]\nmax_streams = 128\n").expect("parse");
		config.resolve().expect("current spellings");
		assert_eq!(config.quic.max_streams, Some(128));
	}

	/// Every config under `demo/relay/` still parses.
	///
	/// These are the configs a reader copies, and a rename that lands in the code
	/// but not in them is invisible until someone's relay refuses to boot. A move
	/// across tables (`[server.quic]` to a top-level `[quic]`) is the case serde
	/// aliases cannot cover, which is exactly why this reads the real files.
	#[test]
	fn demo_configs_parse() {
		let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/relay");
		let mut checked = 0;
		for entry in std::fs::read_dir(&dir).expect("demo/relay") {
			let path = entry.expect("dir entry").path();
			if path.extension().is_none_or(|ext| ext != "toml") {
				continue;
			}
			let toml = std::fs::read_to_string(&path).expect("read config");
			toml::from_str::<Config>(&toml).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
			checked += 1;
		}
		assert!(checked > 0, "no demo configs found in {}", dir.display());
	}

	/// Bare defaults loaded from TOML survive when the CLI does not mention them.
	#[test]
	fn cli_does_not_clobber_toml_stats_enabled() {
		let _env = EnvGuard::clear(&["MOQ_STATS_ENABLED", "MOQ_STATS_DEPTH"]);

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
			"TOML's stats.enabled=true must not be clobbered by CLI defaults"
		);
		assert_eq!(config.stats.interval, 5);
		assert_eq!(config.stats.node.as_deref(), Some("localhost"));
		assert_eq!(config.stats.depth, 2);
	}

	/// Bare runtime defaults loaded from TOML survive when the CLI omits them.
	#[test]
	fn cli_does_not_clobber_toml_runtime() {
		let _env = EnvGuard::clear(&[
			"MOQ_RUNTIME_WORKERS",
			"MOQ_RUNTIME_PIN",
			"MOQ_RUNTIME_IO_URING",
			"MOQ_WEB_WS",
			"MOQ_CONNECT_WEBSOCKET_ENABLED",
		]);

		let toml = r#"
[runtime]
workers = 8
pin = false
io_uring = false

[web]
ws = false

[connect.websocket]
enabled = false
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("runtime-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(config.runtime.workers, Some(8));
		assert_eq!(
			config.runtime.pin,
			Some(false),
			"TOML's runtime.pin=false must survive the CLI re-parse \
			 (a bare bool with a `true` default reads as empty and is refilled; \
			 type it as Option<bool>)"
		);
		assert_eq!(
			config.runtime.io_uring,
			Some(false),
			"TOML's runtime.io_uring=false must survive the CLI re-parse"
		);
		assert_eq!(
			config.web.ws,
			Some(false),
			"TOML's web.ws=false must survive the CLI re-parse"
		);
		assert_eq!(
			config.connect.websocket.enabled,
			Some(false),
			"TOML's connect.websocket.enabled=false must survive the CLI re-parse"
		);

		let args = [
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--runtime-pin=true"),
			std::ffi::OsString::from("--web-ws=true"),
			std::ffi::OsString::from("--connect-websocket-enabled=true"),
		];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.runtime.pin, Some(true));
		assert_eq!(config.web.ws, Some(true));
		assert_eq!(config.connect.websocket.enabled, Some(true));
	}

	#[test]
	fn cli_does_not_clobber_toml_cache() {
		let _env = EnvGuard::clear(&["MOQ_CACHE_CAPACITY", "MOQ_CACHE_HEADROOM", "MOQ_CACHE_DURATION"]);

		let toml = r#"
[cache]
capacity = "8GiB"
headroom = "10%"
duration = "30s"
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
		assert_eq!(
			config.cache.duration,
			Some(std::time::Duration::from_secs(30).into()),
			"TOML's cache.duration must not be clobbered by the CLI re-parse"
		);
	}

	/// `cache.duration` is an `Option<Duration>` behind plain `humantime_serde`
	/// (not `humantime_serde::option`), so pin both directions including the
	/// `None` serialize path, which the merge test above never exercises.
	#[test]
	fn cache_duration_serde_round_trip() {
		let set: CacheConfig = toml::from_str(r#"duration = "30s""#).expect("deserialize Some");
		assert_eq!(set.duration, Some(std::time::Duration::from_secs(30).into()));

		let unset: CacheConfig = toml::from_str("").expect("deserialize absent");
		assert_eq!(unset.duration, None);

		let encoded = toml::to_string(&set).expect("serialize Some");
		let decoded: CacheConfig = toml::from_str(&encoded).expect("re-deserialize");
		assert_eq!(decoded.duration, set.duration, "round trip must preserve the duration");

		toml::to_string(&unset).expect("serialize None");
	}

	/// A deprecated TOML value still survives the merge so validation can name it.
	#[test]
	#[allow(deprecated)]
	fn cli_does_not_clobber_toml_linger() {
		let _env = EnvGuard::clear(&["MOQ_CLUSTER_LINGER"]);

		let toml = r#"
[cluster]
linger = "30s"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("linger-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.cluster.linger,
			Some(std::time::Duration::from_secs(30).into()),
			"TOML's cluster.linger must not be clobbered by the CLI re-parse"
		);
	}

	/// Preferred addresses loaded from TOML survive when the CLI omits them.
	#[test]
	fn cli_does_not_clobber_toml_preferred_addresses() {
		let _env = EnvGuard::clear(&["MOQ_LISTEN_PREFERRED_V4", "MOQ_LISTEN_PREFERRED_V6"]);

		// They are accept-only, so they live on `[listen]` rather than in the
		// shared `[listen.quic]` tuning.
		let toml = r#"
[listen]
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
			config.listen.preferred_v4,
			Some("192.0.2.1:443".parse().unwrap()),
			"TOML's listen.preferred_v4 must not be clobbered by the CLI re-parse"
		);
		assert_eq!(
			config.listen.preferred_v6,
			Some("[2001:db8::1]:443".parse().unwrap()),
			"TOML's listen.preferred_v6 must not be clobbered by the CLI re-parse"
		);
	}

	/// Same clobbering hazard as the preferred addresses above, for the qlog
	/// directory: a TOML-configured trace dir must survive the CLI re-parse.
	#[test]
	fn cli_does_not_clobber_toml_qlog() {
		let _env = EnvGuard::clear(&["MOQ_SERVER_QUIC_QLOG"]);

		let toml = r#"
[quic]
qlog = "/tmp/moq-qlog"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("qlog-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(
			config.quic.qlog.as_deref(),
			Some(std::path::Path::new("/tmp/moq-qlog")),
			"TOML's quic.qlog must not be clobbered by the CLI re-parse"
		);
	}

	/// A backend-specific congestion-control choice loaded from TOML survives.
	#[test]
	fn cli_does_not_clobber_toml_congestion_control() {
		let _env = EnvGuard::clear(&[
			"MOQ_SERVER_QUIC_CONGESTION_CONTROL",
			"MOQ_CLIENT_QUIC_CONGESTION_CONTROL",
		]);

		let toml = r#"
[quic]
congestion_control = "delay"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("congestion-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		// One value, shared by the dial and accept sides: the knob means the same
		// thing whichever way the connection was opened.
		assert_eq!(
			config.quic.congestion_control,
			Some(moq_tokio::quic::CongestionControl::Delay),
			"TOML's quic.congestion_control must not be clobbered by the CLI re-parse"
		);
	}

	/// The client connect timeout loaded from TOML replaces the built-in default.
	#[test]
	fn cli_does_not_clobber_toml_client_connect_timeout() {
		let _env = EnvGuard::clear(&["MOQ_CLIENT_CONNECT_TIMEOUT"]);

		let toml = r#"
[client]
timeout = "2m"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("client-connect-timeout-toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(config.connect.timeout, std::time::Duration::from_secs(120));
	}

	#[test]
	fn cli_does_not_clobber_toml_web_https_cert_arrays() {
		let _env = EnvGuard::clear(&["MOQ_WEB_HTTPS_CERT", "MOQ_WEB_HTTPS_KEY"]);

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

	/// Explicit CLI flags still override TOML defaults.
	#[test]
	fn cli_flag_overrides_toml_stats_enabled() {
		let _env = EnvGuard::clear(&["MOQ_STATS_ENABLED"]);

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

	/// An auth API loaded from TOML survives when the CLI omits it.
	#[test]
	fn cli_does_not_clobber_toml_auth_api() {
		let _env = EnvGuard::clear(&["MOQ_AUTH_API"]);

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

	/// The optional system-roots policy loaded from TOML survives when omitted on the CLI.
	#[test]
	fn cli_does_not_clobber_toml_system_roots() {
		let _env = EnvGuard::clear(&["MOQ_CLIENT_TLS_SYSTEM_ROOTS"]);

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
			config.connect.tls.system_roots,
			Some(true),
			"TOML's client.tls.system_roots must not be clobbered by the CLI re-parse"
		);
	}

	/// A stable cluster id loaded from TOML survives when omitted on the CLI.
	#[test]
	fn cli_does_not_clobber_toml_cluster_id() {
		let _env = EnvGuard::clear(&["MOQ_CLUSTER_ID"]);

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
	#[test]
	fn cli_does_not_clobber_toml_tiers() {
		let _env = EnvGuard::clear(&["MOQ_CLUSTER_TIER", "MOQ_AUTH_MTLS_TIER"]);

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

	/// A Unix listener and its allowlist loaded from TOML survive together.
	#[cfg(all(feature = "uds", unix))]
	#[test]
	fn cli_does_not_clobber_toml_server_unix() {
		let _env = EnvGuard::clear(&["MOQ_SERVER_UNIX_BIND", "MOQ_SERVER_UNIX_ALLOW_UID"]);

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

		assert_eq!(config.listen.bind.as_deref(), Some("[::]:443"));
		assert_eq!(
			config.listen.unix.bind.as_deref(),
			Some(std::path::Path::new("/run/moq/internal.sock")),
			"TOML's server.unix.bind must not be clobbered by the CLI re-parse"
		);
		assert_eq!(
			config.listen.unix.allow.uid,
			vec![1001],
			"TOML's server.unix.allow must not be clobbered by the CLI re-parse"
		);
	}

	#[test]
	fn cli_flag_overrides_toml_cluster_id() {
		let _env = EnvGuard::clear(&["MOQ_CLUSTER_ID"]);

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

	/// An internal listener loaded from TOML survives when omitted on the CLI.
	#[test]
	fn cli_does_not_clobber_toml_internal_listen() {
		let _env = EnvGuard::clear(&["MOQ_INTERNAL_LISTEN"]);

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
			let err = Cli::parse_from(&argv).unwrap_err();
			let answer = moq_tokio::cli::answer(Cli::spec(), Cli::command(), &argv, err);
			assert!(answer.is_question(), "{flag} was treated as a failure");
			assert!(!answer.message().trim().is_empty(), "{flag} rendered nothing");
		}
	}

	/// An embedder can flatten the relay's whole flag surface into its own CLI.
	///
	/// This is the reason [`Config`] derives `Args` rather than `Cli`: a `Cli`
	/// cannot be flattened, so declaring the program-level parts on it would force
	/// every embedder to re-declare the relay's flags and drift on each one added
	/// here. moq.pro's `edge` is the embedder this exists for.
	#[test]
	fn the_config_can_be_embedded() {
		/// A stand-in for an embedder's command line: the relay's flags plus one of
		/// its own.
		#[derive(usage::Cli, Clone, Debug)]
		#[usage(unknown_flags = "error", args_override_self = false)]
		#[usage(name = "embedder", version = "0")]
		struct Embedder {
			#[usage(flatten)]
			relay: Config,

			#[usage(long = "embedder-only")]
			own: Option<String>,
		}

		let _env = EnvGuard::clear(&["MOQ_LISTEN", "MOQ_SERVER_BIND"]);

		// One argv carries both surfaces, which is the whole point: the embedder does
		// not have to split the relay's flags out of its own.
		let parsed = Embedder::parse_from(&[
			std::ffi::OsStr::new("--listen"),
			std::ffi::OsStr::new("[::]:4443"),
			std::ffi::OsStr::new("--embedder-only"),
			std::ffi::OsStr::new("mine"),
		])
		.expect("the relay's flags parse inside an embedder's CLI");

		assert_eq!(parsed.own.as_deref(), Some("mine"));
		assert_eq!(
			parsed.relay.listen.bind.map(|bind| bind.to_string()),
			Some("[::]:4443".to_string()),
			"the flattened relay config received its own flag"
		);
	}

	/// `--help` describes the program, not the type that happens to declare it.
	///
	/// Usage renders a `Cli`'s doc comment as the about text, so a doc comment
	/// written for developers is printed to users verbatim -- rustdoc link syntax
	/// and all. Splitting `Config` out put a private wrapper in that position and
	/// leaked its implementation notes into `moq-relay --help`; this pins the
	/// description so the next edit there cannot.
	#[test]
	fn the_help_description_is_written_for_users() {
		let root = Cli::spec().root;
		// What `--help` actually prints: usage renders `long_about.or(about)`, and a
		// doc comment with a second paragraph fills `long_about` with the whole thing.
		// Asserting on `about` alone would pass while the extra paragraphs leaked.
		let shown = root.long_about.or(root.about).expect("the CLI describes itself");

		assert_eq!(
			shown,
			"Top-level relay configuration, loadable from CLI arguments, environment variables, or a TOML file."
		);
		// The tell that a developer-facing doc comment reached this surface.
		for leak in ["[`", "moq.pro", "implementation detail"] {
			assert!(
				!shown.contains(leak),
				"{leak:?} leaked into the help description: {shown}"
			);
		}
	}

	/// The spec is named for the binary, not for the struct that declares it.
	///
	/// Usage takes the program name from the type unless told otherwise, so an
	/// undeclared name renders every usage line and completion as `config`.
	#[test]
	fn the_spec_is_named_for_the_binary() {
		assert_eq!(Cli::spec().bin.unwrap_or(Cli::spec().name), "moq-relay");
	}

	/// A TOML boolean survives an environment variable that says otherwise.
	///
	/// Usage reads a standing `false` as an empty boolean, so a bare `bool` is
	/// refilled from the environment during the CLI re-parse and the file loses.
	/// Every merged boolean is therefore `Option<bool>`. A bare `Vec<T>` is the
	/// other unsafe shape, tracked in moq-dev/moq#3051; scalars are safe.
	#[test]
	fn env_does_not_clobber_toml_booleans() {
		let _env = EnvGuard::clear(&["MOQ_STATS_ENABLED", "MOQ_CLUSTER_LAN"]);
		// SAFETY: EnvGuard serializes env mutation across these tests.
		unsafe {
			std::env::set_var("MOQ_STATS_ENABLED", "true");
			std::env::set_var("MOQ_CLUSTER_LAN", "true");
		}

		// `[cluster.lan]` is only a key when the feature that reads it is on, and
		// `deny_unknown_fields` refuses it otherwise.
		#[cfg(feature = "cluster-lan")]
		let toml = "[stats]\nenabled = false\n\n[cluster.lan]\nenabled = false\n";
		#[cfg(not(feature = "cluster-lan"))]
		let toml = "[stats]\nenabled = false\n";
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("env-vs-toml-bools.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		unsafe {
			std::env::remove_var("MOQ_STATS_ENABLED");
			std::env::remove_var("MOQ_CLUSTER_LAN");
		}

		assert_eq!(
			config.stats.enabled,
			Some(false),
			"TOML stats.enabled=false must beat MOQ_STATS_ENABLED=true"
		);
		#[cfg(feature = "cluster-lan")]
		assert_eq!(
			config.cluster.lan.enabled,
			Some(false),
			"TOML cluster.lan.enabled=false must beat MOQ_CLUSTER_LAN=true"
		);
	}
}
