use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{auth, cache, cluster, internal, stats, web};

/// Top-level relay configuration, as a composable args group.
///
/// Loadable from CLI arguments, environment variables, or a TOML file.
/// Precedence is CLI > env > file > defaults.
///
/// `usage_rs::Args` rather than `usage_rs::Cli` on purpose: the program-level parts (a
/// name, a version, the completion command) belong to whichever binary owns the
/// process, and a `Cli` cannot be `#[usage(flatten)]`ed into another one. Keeping
/// them off this type is what lets an embedder put the relay's whole flag surface
/// inside its own CLI instead of re-declaring it and drifting on every flag added
/// here. This binary wraps it in
/// a private `Cli` that adds those program-level parts; [`spec`] exposes the
/// resulting command line.
#[derive(usage_rs::Args, Clone, Debug, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
#[derive(Default)]
pub struct Config {
	/// Request and transport limits grouped by client and cluster-peer role.
	#[usage(flatten)]
	pub limits: crate::limits::Config,

	#[usage(skip)]
	#[serde(rename = "session", skip_serializing)]
	legacy_session: Option<serde::de::IgnoredAny>,
	#[usage(skip)]
	#[serde(rename = "peer", skip_serializing)]
	legacy_peer: Option<serde::de::IgnoredAny>,

	/// The QUIC/TLS configuration for the server.
	#[usage(flatten)]
	#[serde(default)]
	pub listen: moq_tokio::listen::Config,

	/// The released `[server]` table, kept so [`Config::resolve`] can name `[listen]`.
	#[serde(default, skip_serializing)]
	#[usage(skip)]
	pub(crate) server: Option<moq_tokio::listen::Config>,

	/// The QUIC/TLS configuration for the client. (clustering only)
	#[usage(flatten)]
	#[serde(default)]
	pub connect: moq_tokio::connect::Config,

	/// The released `[client]` table, kept so [`Config::resolve`] can name `[connect]`.
	#[serde(default, skip_serializing)]
	#[usage(skip)]
	pub(crate) client: Option<moq_tokio::connect::Config>,

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
	pub runtime: crate::runtime::Config,

	/// Cluster configuration.
	#[usage(flatten)]
	#[serde(default)]
	pub cluster: cluster::Config,

	/// Authentication configuration.
	#[usage(flatten)]
	#[serde(default)]
	pub auth: auth::Config,

	/// Optionally run a TCP HTTP/WebSocket server.
	#[usage(flatten)]
	#[serde(default)]
	pub web: web::Config,

	/// Automatic certificates from an ACME directory. Disabled unless `acme.domains` is set.
	#[usage(flatten)]
	#[serde(default)]
	pub acme: crate::acme::Config,

	/// Optional public STUN Binding listener, available with the `stun` feature.
	#[cfg(feature = "stun")]
	#[usage(flatten)]
	#[serde(default)]
	pub stun: crate::stun::Config,

	/// Stats publishing configuration. Disabled unless `stats.enabled = true`.
	#[usage(flatten)]
	#[serde(default)]
	pub stats: stats::Config,

	/// Group cache sizing. Unbounded unless `cache.capacity` or `cache.headroom`
	/// is set.
	#[usage(flatten)]
	#[serde(default)]
	pub cache: cache::Config,

	/// Internal (ops) listener for `/metrics`, `/health`, and `/nodes`. Disabled unless
	/// `internal.listen` is set.
	#[usage(flatten)]
	#[serde(default)]
	pub internal: internal::Config,

	/// How long accepted sessions may keep running after a shutdown signal, e.g.
	/// "10s" or "500ms". The first signal sends every session a GOAWAY and waits
	/// up to this long for clients to reconnect elsewhere before force-closing
	/// them, exiting as soon as they have all left; a second signal exits
	/// immediately. Zero closes them at once, with no GOAWAY
	/// they would have no time to act on. Defaults to 10 seconds.
	#[usage(
		name = "drain-timeout",
		long = "drain-timeout",
		env = "MOQ_DRAIN_TIMEOUT",
		setting = "drain_timeout"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub drain_timeout: Option<moq_tokio::cli::Duration>,

	/// If provided, load the configuration from this file.
	#[serde(default)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("toml"))]
	pub file: Option<String>,

	/// Provenance from the last load, skipped in TOML.
	#[usage(skip)]
	#[serde(skip)]
	origins: Option<usage_rs::config::Resolved>,

	/// Iroh specific configuration, used for both a client and server.
	#[usage(flatten)]
	#[serde(default)]
	#[cfg(feature = "iroh")]
	pub iroh: moq_tokio::iroh::Config,
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
#[derive(usage_rs::Cli, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq-relay", version = env!("CARGO_PKG_VERSION"))]
#[usage(completion, settings)]
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
pub fn spec() -> &'static usage_rs::spec::Spec<'static> {
	Cli::spec()
}

impl Config {
	/// Resolve the shutdown grace period after command-line overrides.
	pub(crate) fn drain_timeout(&self) -> std::time::Duration {
		self.drain_timeout
			.map(Into::into)
			.unwrap_or(crate::DEFAULT_DRAIN_TIMEOUT)
	}

	/// Parses configuration from CLI arguments, optionally merging with a
	/// TOML file specified via the positional `file` argument, and refuses any
	/// `MOQ_*` variable the relay does not read. Also initializes the logger.
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
		moq_tokio::cli::check_env(&[Cli::spec()], &[])?;
		config.log.init()?;
		tracing::trace!(?config, "final config");
		Ok(config)
	}

	/// Pure version of [`Self::load`] without logger init, so tests can drive
	/// it with synthetic args and inspect the result.
	///
	/// Merge CLI, environment, then TOML, then declared defaults.
	///
	/// Precedence is CLI > env > file > defaults, declared in
	/// [`moq_tokio::cli::Merge`]. Presence comes from what the parser and the
	/// environment actually supplied, so a file that sets a list to empty or a
	/// bool to false survives.
	pub fn parse_and_merge<I, T>(args: I) -> anyhow::Result<Self>
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
		let (cli, cli_layer) = match Cli::parse_from_with_settings(&argv) {
			Ok(parsed) => parsed,
			Err(err) => {
				let answer = moq_tokio::cli::answer(Cli::spec(), Cli::command(), &argv, err);
				if answer.is_question() {
					answer.exit();
				}
				anyhow::bail!("{}", answer.message());
			}
		};
		let mut config = cli.config;
		config.merge(&cli_layer)?;
		Ok(config)
	}

	/// Load this relay's optional file, merge explicit CLI and environment values, and refuse obsolete settings.
	///
	/// Call once after the embedding CLI's `parse_from_with_settings`; only this relay
	/// fragment is replaced, so the root's CLI-only fields retain their parsed values.
	/// [`Self::source`] reports where each merged setting came from.
	pub fn merge(&mut self, cli: &usage_rs::config::CliLayer) -> anyhow::Result<()> {
		let file_path = self.file.clone();
		let file_body = file_path
			.as_ref()
			.map(|path| std::fs::read_to_string(path).map(|source| (path.clone(), source)))
			.transpose()?;
		let file_value = file_body
			.as_ref()
			.map(|(_, source)| toml::from_str::<toml::Value>(source))
			.transpose()?;
		let file = file_body
			.as_ref()
			.zip(file_value.as_ref())
			.map(|((path, _), value)| moq_tokio::cli::FileSource {
				path: std::path::Path::new(path),
				value,
			});
		self.merge_into(cli, &usage_rs::config::EnvLayer::from_process(), file)
	}

	/// Merge CLI, environment, and optional TOML values into this relay fragment.
	///
	/// An embedding binary can parse its own flattened CLI once, then merge only
	/// its `relay` field. Other CLI-only fields remain untouched.
	fn merge_into(
		&mut self,
		cli: &usage_rs::config::CliLayer,
		env: &usage_rs::config::EnvLayer,
		file: Option<moq_tokio::cli::FileSource<'_>>,
	) -> anyhow::Result<()> {
		#[cfg(not(feature = "stun"))]
		anyhow::ensure!(
			env.get("MOQ_STUN_BIND").is_none(),
			"MOQ_STUN_BIND requires a moq-relay build with the stun feature"
		);

		#[cfg(not(feature = "iroh"))]
		for name in [
			"MOQ_IROH_ENABLED",
			"MOQ_IROH_SECRET",
			"MOQ_IROH_BIND_V4",
			"MOQ_IROH_BIND_V6",
			"MOQ_IROH_DISABLE_RELAY",
		] {
			anyhow::ensure!(
				env.get(name).is_none(),
				"{name} requires a moq-relay build with the iroh feature"
			);
		}
		let mut deprecated = self.deprecated();
		let (merged, resolved) = moq_tokio::cli::Merge {
			registry: crate::settings(),
			cli,
			env,
			file,
		}
		.apply(self.clone())
		.map_err(|err| anyhow::anyhow!("{err}"))?;
		deprecated.extend(merged.deprecated());
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
		*self = merged;
		self.origins = Some(resolved);
		Ok(())
	}

	/// Where a dotted setting key got its value, when this config was loaded
	/// from the command line, environment, and an optional TOML file.
	pub fn source(&self, key: &str) -> Option<&str> {
		self.origins
			.as_ref()
			.and_then(|resolved| resolved.origin_key(key))
			.map(usage_rs::config::Origin::describe)
	}
}

impl Config {
	/// The released spellings in use across every section, each paired with what
	/// replaced it.
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut deprecated = self.quic.deprecated();
		deprecated.extend(self.limits.legacy.deprecated());
		if self.legacy_session.is_some() {
			deprecated.toml("[session]", "[limits.client]", None);
		}
		if self.legacy_peer.is_some() {
			deprecated.toml("[peer]", "[limits.peer]", None);
		}
		deprecated.extend(self.listen.deprecated());
		deprecated.extend(self.connect.deprecated());
		deprecated.extend(self.cluster.deprecated());
		deprecated.extend(self.auth.deprecated());
		deprecated.extend(self.acme.deprecated());
		if let Some(server) = &self.server {
			deprecated.toml("[server]", "[listen]", None);
			deprecated.extend(server.deprecated());
		}
		if let Some(client) = &self.client {
			deprecated.toml("[client]", "[connect]", None);
			deprecated.extend(client.deprecated());
		}
		deprecated
	}

	/// Refuse a config parsed from released spellings, then apply the relay's own
	/// defaults.
	///
	/// The check comes first because those spellings configure nothing: a relay that
	/// booted anyway would be serving on defaults, with the deployment's own
	/// `--server-bind` and TLS material silently absent.
	pub(crate) fn resolve(&mut self) -> anyhow::Result<()> {
		let deprecated = self.deprecated();
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
		// Here rather than at an endpoint, since io_uring workers and lazily built
		// dial clients would otherwise check `[quic]` late or never. `[limits.peer.quic]`
		// is checked by `Cluster::new`, which embedders call directly.
		self.quic.validate().context("invalid [quic]")?;
		#[cfg(not(feature = "acme"))]
		self.acme.refuse_unsupported()?;

		self.quic
			.max_streams
			.get_or_insert(moq_tokio::quic::DEFAULT_MAX_STREAMS);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Each renamed setting: its old flag, old variable, a value equal to the old default,
	/// and the replacement its refusal names.
	const OBSOLETE_ROLE_SOURCES: [(&str, &str, &str, &str); 6] = [
		(
			"--session-announces",
			"MOQ_SESSION_ANNOUNCES",
			"1000",
			"--limits-client-announces",
		),
		(
			"--peer-announces",
			"MOQ_PEER_ANNOUNCES",
			"100000",
			"--limits-peer-announces",
		),
		("--acme-dir", "MOQ_ACME_DIR", "cache", "--acme-cache"),
		(
			"--cluster-connect-api",
			"MOQ_CLUSTER_CONNECT_API",
			"peers.json",
			"--cluster-api-url",
		),
		(
			"--cluster-connect-api-tls-root",
			"MOQ_CLUSTER_CONNECT_API_TLS_ROOT",
			"api.pem",
			"--cluster-api-tls-root",
		),
		(
			"--auth-verifications",
			"MOQ_AUTH_VERIFICATIONS",
			"128",
			"fixed per-session verification policy",
		),
	];

	/// Renames refuse an explicit variable even when its value equals the old default.
	#[test]
	fn role_config_refuses_obsolete_env() {
		let env = OBSOLETE_ROLE_SOURCES
			.iter()
			.fold(test_env::Env::default(), |env, (_, var, value, _)| env.set(var, value));
		if !env.isolate() {
			return;
		}
		let error = Config::parse_and_merge(["moq-relay"]).unwrap_err().to_string();
		for (_, var, _, replacement) in OBSOLETE_ROLE_SOURCES {
			assert!(error.contains(var) && error.contains(replacement), "{var}: {error}");
		}
	}

	/// Renames refuse an explicit flag or file setting even when its value equals the old default.
	#[test]
	fn role_config_refuses_obsolete_sources() {
		if !test_env::Env::without(&OBSOLETE_ROLE_SOURCES.map(|(_, var, _, _)| var)).isolate() {
			return;
		}
		for (flag, _, value, replacement) in OBSOLETE_ROLE_SOURCES {
			let error = Config::parse_and_merge(["moq-relay", flag, value])
				.unwrap_err()
				.to_string();
			assert!(error.contains(flag) && error.contains(replacement), "{error}");
		}
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("obsolete.toml");
		for (body, replacement) in [
			("[session]\nannounces = 1000", "[limits.client]"),
			("[peer]\nannounces = 100000", "[limits.peer]"),
			("[acme]\ndir = 'cache'", "[acme] cache"),
			("[cluster]\nconnect_api = 'peers.json'", "[cluster.api] url"),
			("[cluster]\nconnect_api_tls_root = []", "[cluster.api.tls] root"),
			("[auth]\nverifications = 128", "fixed per-session verification policy"),
		] {
			std::fs::write(&path, body).unwrap();
			let error =
				Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), path.clone().into()]).unwrap_err();
			assert!(error.to_string().contains(replacement), "{body}: {error}");
		}
	}

	#[cfg(not(feature = "acme"))]
	#[test]
	fn an_acme_free_build_refuses_configured_issuance() {
		let mut config = Config::default();
		config.resolve().unwrap();
		config.acme.domains.push("relay.example.com".into());
		let error = config.resolve().unwrap_err();
		assert!(error.to_string().contains("acme feature"), "{error}");
	}

	/// The moq-transport extension choices are file-only, so the loader must carry them through.
	#[test]
	fn loader_keeps_toml_extension_settings() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("relay.toml");
		std::fs::write(
			&path,
			"[listen.extensions]\nauth = false\n\n[connect.extensions]\nsolicit = false\n",
		)
		.unwrap();
		let config = Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), path.into()]).unwrap();
		assert!(!config.listen.extensions.auth);
		assert!(config.listen.extensions.solicit);
		assert!(!config.connect.extensions.solicit);
		assert!(config.connect.extensions.auth);
	}

	#[cfg(feature = "stun")]
	#[test]
	fn stun_binding_merges_file_and_cli() {
		if !test_env::Env::without(&["MOQ_STUN_BIND"]).isolate() {
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("stun.toml");
		std::fs::write(&path, "[stun]\nbind = \"127.0.0.1:3478\"\n").unwrap();
		let file = Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), path.clone().into()]).unwrap();
		assert_eq!(file.stun.bind, Some("127.0.0.1:3478".parse().unwrap()));
		let cli = Config::parse_and_merge([
			std::ffi::OsString::from("moq-relay"),
			path.into(),
			"--stun-bind=127.0.0.1:3479".into(),
		])
		.unwrap();
		assert_eq!(cli.stun.bind, Some("127.0.0.1:3479".parse().unwrap()));
		assert!(toml::from_str::<Config>("[stun]\nrate = 10").is_err());
	}

	#[cfg(not(feature = "stun"))]
	#[test]
	fn a_stun_listener_requires_the_feature() {
		assert!(toml::from_str::<Config>("[stun]\nbind = \"127.0.0.1:3478\"").is_err());
		assert!(Config::parse_and_merge(["moq-relay", "--stun-bind=127.0.0.1:3478"]).is_err());
		let cli = usage_rs::config::CliLayer::new(std::iter::empty::<(String, String)>());
		let env = usage_rs::config::EnvLayer::new([("MOQ_STUN_BIND".to_string(), "127.0.0.1:3478".to_string())]);
		let error = Config::default().merge_into(&cli, &env, None).unwrap_err();
		assert!(error.to_string().contains("MOQ_STUN_BIND requires"), "{error}");
	}

	#[cfg(feature = "stun")]
	#[test]
	fn stun_binding_merges_the_environment() {
		if !test_env::Env::default().set("MOQ_STUN_BIND", "[::1]:3478").isolate() {
			return;
		}
		let config = Config::parse_and_merge(["moq-relay"]).unwrap();
		assert_eq!(config.stun.bind, Some("[::1]:3478".parse().unwrap()));
	}

	#[test]
	fn session_limits_merge_without_crossing_peer_policy() {
		if !test_env::Env::without(&["MOQ_LIMITS_CLIENT_ANNOUNCES", "MOQ_LIMITS_PEER_ANNOUNCES"]).isolate() {
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("limits.toml");
		std::fs::write(&path, "[limits.client]\nannounces = 7\n[limits.peer]\nannounces = 90\n").unwrap();
		let config = Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), path.clone().into()]).unwrap();
		assert_eq!(config.limits.client.resolve().announces, 7);
		assert_eq!(config.limits.peer.resolve().announces, 90);
		let config = Config::parse_and_merge([
			std::ffi::OsString::from("moq-relay"),
			path.into(),
			"--limits-client-announces=0".into(),
		])
		.unwrap();
		assert_eq!(config.limits.client.resolve().announces, 0);
		assert_eq!(config.limits.peer.resolve().announces, 90);
	}

	/// Sizing the removed request window refuses to start rather than run without the
	/// setting.
	#[test]
	fn removed_request_limits_are_refused() {
		if !test_env::Env::without(&["MOQ_LIMITS_CLIENT_REQUESTS", "MOQ_LIMITS_PEER_REQUESTS"]).isolate() {
			return;
		}
		let error = Config::parse_and_merge(["moq-relay", "--limits-peer-requests=1000"]).unwrap_err();
		assert!(error.to_string().contains("--limits-peer-requests"), "{error}");

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("limits.toml");
		std::fs::write(&path, "[limits.client]\nrequests = 8\n").unwrap();
		assert!(Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), path.into()]).is_err());
	}

	/// `[limits.peer.quic]` mirrors `[quic]`'s keys without touching `[quic]` itself.
	#[test]
	fn peer_quic_table_merges_with_flags() {
		if !test_env::Env::without(&[
			"MOQ_QUIC_MAX_STREAMS",
			"MOQ_LIMITS_PEER_QUIC_MAX_STREAMS",
			"MOQ_LIMITS_PEER_QUIC_RECEIVE_WINDOW",
			"MOQ_LIMITS_PEER_QUIC_SEND_WINDOW",
		])
		.isolate()
		{
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("peer-quic.toml");
		std::fs::write(
			&path,
			"[quic]\nmax_streams = 4\n[limits.peer.quic]\nmax_streams = 64\nsend_window = 1000\n",
		)
		.unwrap();
		let config = Config::parse_and_merge([
			std::ffi::OsString::from("moq-relay"),
			path.into(),
			"--limits-peer-quic-receive-window=2000".into(),
		])
		.unwrap();
		assert_eq!(config.quic.max_streams, Some(4));
		assert_eq!(config.limits.peer.quic.max_streams.get(), 64);
		assert_eq!(config.limits.peer.quic.receive_window.get(), 2000);
		assert_eq!(config.limits.peer.quic.send_window.get(), 1000);

		assert!(toml::from_str::<Config>("[limits.peer]\nmax_streams = 64\n").is_err());
	}

	/// A zero peer stream limit or window would wedge the connection, so it never parses.
	#[test]
	fn zero_peer_quic_limits_are_refused() {
		if !test_env::Env::without(&[
			"MOQ_LIMITS_PEER_QUIC_MAX_STREAMS",
			"MOQ_LIMITS_PEER_QUIC_RECEIVE_WINDOW",
			"MOQ_LIMITS_PEER_QUIC_SEND_WINDOW",
		])
		.isolate()
		{
			return;
		}
		for key in ["max_streams", "receive_window", "send_window"] {
			assert!(
				toml::from_str::<Config>(&format!("[limits.peer.quic]\n{key} = 0\n")).is_err(),
				"{key}"
			);
		}
		for flag in [
			"--limits-peer-quic-max-streams=0",
			"--limits-peer-quic-receive-window=0",
			"--limits-peer-quic-send-window=0",
		] {
			assert!(
				Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), flag.into()]).is_err(),
				"{flag} must be refused"
			);
		}
	}

	#[cfg(not(feature = "iroh"))]
	#[test]
	fn iroh_environment_requires_the_feature() {
		for name in [
			"MOQ_IROH_ENABLED",
			"MOQ_IROH_SECRET",
			"MOQ_IROH_BIND_V4",
			"MOQ_IROH_BIND_V6",
			"MOQ_IROH_DISABLE_RELAY",
		] {
			let mut config = Config::default();
			let cli = usage_rs::config::CliLayer::new(std::iter::empty::<(String, String)>());
			let env = usage_rs::config::EnvLayer::new([(name.to_string(), "false".to_string())]);
			let error = config.merge_into(&cli, &env, None).unwrap_err();
			assert!(error.to_string().contains(name), "{error}");
		}
	}

	#[cfg(not(feature = "iroh"))]
	#[test]
	fn iroh_file_and_flags_require_the_feature() {
		assert!(Cli::parse_from(&[std::ffi::OsStr::new("--iroh-enabled")]).is_err());
		assert!(toml::from_str::<Config>("[iroh]\nenabled = false").is_err());
	}

	#[test]
	fn packaged_service_arguments() {
		let unit = include_str!("../../../packaging/moq-relay/moq-relay.service");
		let command = unit.lines().find_map(|line| line.strip_prefix("ExecStart=")).unwrap();
		let mut args = command.split_whitespace();
		assert_eq!(args.next(), Some("/usr/bin/moq-relay"));
		let args: Vec<_> = args.map(std::ffi::OsStr::new).collect();
		let cli = Cli::parse_from(&args).expect("packaged service arguments must parse");
		assert_eq!(cli.config.file.as_deref(), Some("/etc/moq-relay/relay.toml"));
	}

	/// The relay's own default still applies once the released spellings are gone.
	#[test]
	fn the_relay_default_applies() {
		if !test_env::Env::without(&["MOQ_QUIC_MAX_STREAMS", "MOQ_SERVER_QUIC_MAX_STREAMS"]).isolate() {
			return;
		}

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
		if !test_env::Env::without(&[
			"MOQ_LISTEN",
			"MOQ_SERVER_BIND",
			"MOQ_LISTEN_TLS_CERT",
			"MOQ_SERVER_TLS_CERT",
		])
		.isolate()
		{
			return;
		}

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
		assert_eq!(config.listen.bind, None, "the released table configures nothing");

		let err = config.resolve().expect_err("must refuse").to_string();
		assert!(err.contains("[server] -> [listen]"), "{err}");
		assert!(err.contains("listen -> bind"), "{err}");
		assert!(err.contains("[server.quic] -> [quic]"), "{err}");
		assert!(err.contains("[client.quic] -> [quic]"), "{err}");
		assert!(err.contains("both directions"), "{err}");
	}

	/// A 0.14 `[auth]` config with `key` and `public` would otherwise boot with
	/// every JWT ignored; each removed key refuses with its replacement named.
	#[test]
	fn released_auth_keys_refuse_to_boot() {
		let toml = r#"
[auth]
key = "root.jwk"
key_dir = "keys/"
auth_api = "https://api.example.com/auth"
domains = ["example.com"]
mtls_tier = "internal"
public = "anon/**"

[auth.tls]
root = ["ca.pem"]
"#;
		let mut config: Config = toml::from_str(toml).expect("released config must still parse");
		let err = config.resolve().expect_err("must refuse").to_string();
		for old in [
			"[auth] key -> --auth-url to `moq auth serve --key`",
			"[auth] key_dir -> --auth-url to `moq auth serve --key-dir`",
			"[auth] auth_api -> ",
			"[auth] domains -> ",
			"[auth] mtls_tier -> --auth-url to `moq auth serve --tier`",
			"[auth.tls] -> --connect-tls-cert/--connect-tls-key for identity; --auth-root for server trust",
		] {
			assert!(err.contains(old), "{old}: {err}");
		}
	}

	#[test]
	fn auth_roots_merge_without_changing_mesh_roots() {
		if !test_env::Env::default().set("MOQ_AUTH_ROOT", "env.pem").isolate() {
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("relay.toml");
		std::fs::write(
			&path,
			"[auth]\nroot = ['file.pem']\n[connect.tls]\nroot = ['mesh.pem']\n",
		)
		.unwrap();
		let config = Config::parse_and_merge(["moq-relay", path.to_str().unwrap()]).unwrap();
		assert_eq!(config.auth.root, vec![std::path::PathBuf::from("env.pem")]);
		assert_eq!(config.connect.tls.root, vec![std::path::PathBuf::from("mesh.pem")]);
		let config = Config::parse_and_merge([
			"moq-relay",
			path.to_str().unwrap(),
			"--auth-root",
			"cli.pem",
			"--auth-root",
			"other.pem",
		])
		.unwrap();
		assert_eq!(
			config.auth.root,
			vec![
				std::path::PathBuf::from("cli.pem"),
				std::path::PathBuf::from("other.pem")
			]
		);
		assert_eq!(config.connect.tls.root, vec![std::path::PathBuf::from("mesh.pem")]);
	}

	/// The released auth variables, each with the spelling its refusal names.
	const RELEASED_AUTH_ENV: [(&str, &str); 7] = [
		("MOQ_AUTH_KEY", "--auth-key / MOQ_AUTH_KEY"),
		("MOQ_AUTH_KEY_DIR", "--auth-key-dir / MOQ_AUTH_KEY_DIR"),
		("MOQ_AUTH_API", "--auth-api / MOQ_AUTH_API"),
		("MOQ_AUTH_PUBLIC_API", "--auth-public-api / MOQ_AUTH_PUBLIC_API"),
		("MOQ_AUTH_DOMAIN", "--auth-domain / MOQ_AUTH_DOMAIN"),
		("MOQ_AUTH_MTLS_TIER", "--auth-mtls-tier / MOQ_AUTH_MTLS_TIER"),
		("MOQ_AUTH_TLS_ROOT", "--auth-tls-* / MOQ_AUTH_TLS_*"),
	];

	/// The environment is the half a removed flag silently misses: a relay deployed
	/// through it never typed the flag.
	#[test]
	fn released_auth_env_refuses_to_boot() {
		let env = RELEASED_AUTH_ENV
			.iter()
			.fold(test_env::Env::default(), |env, (var, _)| env.set(var, "x"));
		if !env.isolate() {
			return;
		}
		// One refusal names every variable, so the operator fixes them all in one pass.
		let err = Config::parse_and_merge(["moq-relay", "--auth-public", "**"])
			.expect_err("must refuse")
			.to_string();
		for (var, spelling) in RELEASED_AUTH_ENV {
			assert!(err.contains(spelling), "{var}: {err}");
		}
	}

	/// 0.14 took the bare flag, so it has to parse to be refused by name.
	#[test]
	fn released_auth_flag_refuses_to_boot() {
		if !test_env::Env::without(&RELEASED_AUTH_ENV.map(|(var, _)| var)).isolate() {
			return;
		}
		let err = Config::parse_and_merge(["moq-relay", "--auth-public", "**", "--auth-tls-disable-verify"])
			.expect_err("must refuse")
			.to_string();
		assert!(err.contains("--auth-tls-* / MOQ_AUTH_TLS_*"), "{err}");
	}

	/// A released flag and a released table are refused together, in one message.
	///
	/// The flag lands on a hidden field the merge's TOML round-trip drops, so a
	/// check that only reads the merged config would boot on the default bind. The
	/// table is only visible after the merge. Reporting them separately would make
	/// the operator fix one, rerun, and hit the other.
	#[test]
	fn released_spellings_refuse_across_the_merge() {
		if !test_env::Env::without(&["MOQ_LISTEN", "MOQ_SERVER_BIND"]).isolate() {
			return;
		}

		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("released-quic-table.toml");
		std::fs::write(&path, "[server.quic]\nmax_streams = 4096\n").unwrap();

		let err = Config::parse_and_merge([
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from("--server-bind"),
			std::ffi::OsString::from("[::]:4443"),
			std::ffi::OsString::from(&path),
		])
		.expect_err("must refuse")
		.to_string();
		assert!(
			err.contains("--server-bind / MOQ_SERVER_BIND -> --listen / MOQ_LISTEN"),
			"{err}"
		);
		assert!(err.contains("[server.quic] -> [quic]"), "{err}");
	}

	/// The canonical top-level table, which is what the demo configs use.
	#[test]
	fn the_shared_quic_table_applies() {
		let mut config: Config = toml::from_str("[quic]\nmax_streams = 128\n").expect("parse");
		config.resolve().expect("current spellings");
		assert_eq!(config.quic.max_streams, Some(128));
	}

	/// Real relay examples, packaging and test fixtures use the same loader as an embedding CLI.
	#[test]
	fn repository_configs_load() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
		// Only repository inputs belong here; installed clients and build output below
		// test/ can contain unrelated TOML. Git's tracked list also includes new fixtures.
		let tracked = std::process::Command::new("git")
			.args(["ls-files", "-z", "--", "demo/relay", "packaging", "test"])
			.current_dir(&root)
			.output()
			.expect("list repository fixtures");
		assert!(tracked.status.success());
		let tracked = String::from_utf8(tracked.stdout).unwrap();
		for area in ["demo/relay", "packaging", "test"] {
			let mut checked = 0;
			for name in tracked.split('\0').filter(|name| name.starts_with(area)) {
				let path = root.join(name);
				if path.extension().is_none_or(|ext| ext != "toml")
					|| matches!(
						path.file_name().and_then(|name| name.to_str()),
						Some("Cargo.toml" | "rust-toolchain.toml" | ".rustfmt.toml")
					) {
					continue;
				}
				let result = Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), path.clone().into()]);
				if path.file_name().unwrap() == "smoke-legacy.toml" {
					let error = result.expect_err("the published legacy fixture must be refused");
					assert!(error.to_string().contains("[server]"), "{}: {error}", path.display());
				} else {
					result.unwrap_or_else(|error| panic!("{}: {error}", path.display()));
				}
				checked += 1;
			}
			assert!(checked > 0, "no relay fixtures found in {area}");
		}
	}

	/// Stats enabled from the CLI alone build at the producer's default interval:
	/// the parser fills a field it skips with its type's zero, not `Default`.
	#[tokio::test]
	async fn cli_stats_build_without_an_interval() {
		if !test_env::Env::without(&["MOQ_STATS_ENABLED", "MOQ_STATS_INTERVAL"]).isolate() {
			return;
		}

		let args = [
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from("--stats-enabled"),
		];
		let config = Config::parse_and_merge(args).expect("config load");
		assert!(config.stats.enabled);
		let cluster = cluster::Cluster::new(cluster::Options::default()).expect("cluster");
		config.stats.build(cluster.origin.clone()).expect("stats build");
	}

	/// Bare defaults loaded from TOML survive when the CLI does not mention them.
	#[test]
	fn cli_does_not_clobber_toml_stats_enabled() {
		if !test_env::Env::without(&["MOQ_STATS_ENABLED", "MOQ_STATS_DEPTH", "MOQ_STATS_LINGER"]).isolate() {
			return;
		}

		let toml = r#"
[stats]
enabled = true
interval = "5s"
node = "localhost"
depth = 2
linger = "2m"
"#;
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("toml-wins.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert!(
			config.stats.enabled,
			"TOML's stats.enabled=true must not be clobbered by CLI defaults"
		);
		assert_eq!(config.stats.interval(), Some(std::time::Duration::from_secs(5)));
		assert_eq!(config.stats.node.as_deref(), Some("localhost"));
		assert_eq!(config.stats.depth, 2);
		assert_eq!(config.stats.linger(), Some(std::time::Duration::from_secs(120)));

		let args = vec![
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--stats-linger=30s"),
			std::ffi::OsString::from("--stats-interval=250ms"),
		];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.stats.linger(), Some(std::time::Duration::from_secs(30)));
		assert_eq!(config.stats.interval(), Some(std::time::Duration::from_millis(250)));
	}

	/// Bare runtime defaults loaded from TOML survive when the CLI omits them.
	#[test]
	fn cli_does_not_clobber_toml_runtime() {
		if !test_env::Env::without(&[
			"MOQ_RUNTIME_WORKERS",
			"MOQ_RUNTIME_PIN",
			"MOQ_RUNTIME_IO_URING",
			"MOQ_WEB_WS",
			"MOQ_CONNECT_WEBSOCKET_ENABLED",
		])
		.isolate()
		{
			return;
		}

		let toml = r#"
[runtime]
workers = 8
pin = false
io_uring = false

[web]
ws = false

"#;
		#[cfg(feature = "websocket")]
		let toml = format!("{toml}\n[connect.websocket]\nenabled = false\n");
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("runtime-toml-wins.toml");
		std::fs::write(&path, toml.as_bytes()).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");

		assert_eq!(config.runtime.workers, Some(8));
		assert!(!config.runtime.pin, "TOML's runtime.pin=false must survive the merge");
		assert!(
			!config.runtime.io_uring,
			"TOML's runtime.io_uring=false must survive the merge"
		);
		assert!(!config.web.ws, "TOML's web.ws=false must survive the merge");
		#[cfg(feature = "websocket")]
		assert_eq!(
			config.connect.websocket.enabled,
			Some(false),
			"TOML's connect.websocket.enabled=false must survive the merge"
		);

		let args = [
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--runtime-pin=true"),
			std::ffi::OsString::from("--web-ws=true"),
			#[cfg(feature = "websocket")]
			std::ffi::OsString::from("--connect-websocket-enabled=true"),
		];
		let config = Config::parse_and_merge(args).expect("config load");
		assert!(config.runtime.pin);
		assert!(config.web.ws);
		#[cfg(feature = "websocket")]
		assert_eq!(config.connect.websocket.enabled, Some(true));
	}

	/// `cache.duration` is an `Option<Duration>` behind plain `humantime_serde`
	/// (not `humantime_serde::option`), so pin both directions including the
	/// `None` serialize path, which `provenance_merge_table` never exercises.
	#[test]
	fn cache_duration_serde_round_trip() {
		let set: cache::Config = toml::from_str(r#"duration = "30s""#).expect("deserialize Some");
		assert_eq!(set.duration, Some(std::time::Duration::from_secs(30).into()));

		let unset: cache::Config = toml::from_str("").expect("deserialize absent");
		assert_eq!(unset.duration, None);

		let encoded = toml::to_string(&set).expect("serialize Some");
		let decoded: cache::Config = toml::from_str(&encoded).expect("re-deserialize");
		assert_eq!(decoded.duration, set.duration, "round trip must preserve the duration");

		toml::to_string(&unset).expect("serialize None");
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
	/// here.
	#[test]
	fn the_config_can_be_embedded() {
		/// A stand-in for an embedder's command line: the relay's flags plus one of
		/// its own.
		#[derive(usage_rs::Cli, Clone, Debug)]
		#[usage(unknown_flags = "error", args_override_self = false)]
		#[usage(name = "embedder", version = "0")]
		#[usage(settings)]
		struct Embedder {
			#[usage(flatten)]
			relay: Config,

			#[usage(long = "embedder-only")]
			own: Option<String>,
		}

		if !test_env::Env::without(&["MOQ_LISTEN", "MOQ_SERVER_BIND"]).isolate() {
			return;
		}

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
		for leak in ["[`", "implementation detail"] {
			assert!(
				!shown.contains(leak),
				"{leak:?} leaked into the help description: {shown}"
			);
		}
	}

	/// The two declarations of every merged flag stay in step.
	#[test]
	fn the_settings_registry_matches_the_cli() {
		let drift = crate::settings::Settings::SETTINGS_REGISTRY.drift(Cli::SETTINGS_BINDINGS);
		assert!(drift.is_empty(), "{drift:#?}");
	}

	/// The relay refuses to start on a `MOQ_*` variable it doesn't read, so a stale
	/// name in the NixOS module would stop every NixOS relay; catch it here first.
	#[test]
	fn the_nixos_module_sets_only_known_variables() {
		let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../nix/modules/moq-relay.nix");
		let module = std::fs::read_to_string(path).unwrap();
		let registry = crate::settings();

		let mut unknown = Vec::new();
		for (start, _) in module.match_indices("MOQ_") {
			let rest = &module[start..];
			let end = rest
				.find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
				.unwrap_or(rest.len());
			let name = &rest[..end];
			let known = registry
				.props
				.iter()
				.any(|prop| prop.deprecated.is_none() && prop.envs.contains(&name));
			if !known {
				unknown.push(name);
			}
		}
		assert!(
			unknown.is_empty(),
			"{path} sets variables moq-relay doesn't read: {unknown:?}"
		);
	}

	/// An embedder can merge its relay fragment without parsing the binary CLI again.
	#[test]
	fn merge_into_relay_fragment() {
		if !test_env::Env::without(&["MOQ_CLUSTER_ID"]).isolate() {
			return;
		}
		let (parsed, cli) =
			Cli::parse_from_with_settings(&[std::ffi::OsStr::new("--cluster-id"), std::ffi::OsStr::new("9")]).unwrap();
		let file = toml::from_str::<toml::Value>("[cluster]\nid = 7\n").unwrap();
		let source = moq_tokio::cli::FileSource {
			path: std::path::Path::new("relay.toml"),
			value: &file,
		};
		let mut config = parsed.config;
		config
			.merge_into(&cli, &usage_rs::config::EnvLayer::from_process(), Some(source))
			.unwrap();
		assert_eq!(config.cluster.id, Some(9));
		assert_eq!(config.source("cluster.id"), Some("--cluster-id"));
	}

	/// One source a merged setting is read from, and what it must end up as.
	struct Case {
		name: &'static str,
		toml: &'static str,
		cli: &'static [&'static str],
		key: &'static str,
		check: fn(&Config),
		source: &'static str,
	}

	/// The variables a provenance case's setting could also be read from.
	const PROVENANCE_ENV: &[&str] = &[
		"MOQ_STATS_ENABLED",
		"MOQ_STATS_PREFIX",
		"MOQ_LISTEN_VERSION",
		"MOQ_LISTEN_TLS_GENERATE",
		"MOQ_RUNTIME_PIN",
		"MOQ_CACHE_DURATION",
		"MOQ_CONNECT_TIMEOUT",
		"MOQ_CLUSTER_TIER",
		"MOQ_CLUSTER_ID",
		"MOQ_CLUSTER_MAX_HOPS",
		"MOQ_QUIC_RECEIVE_WINDOW",
		"MOQ_CONNECT_TLS_SYSTEM_ROOTS",
		"MOQ_QUIC_CONGESTION_CONTROL",
		"MOQ_QUIC_QLOG",
		"MOQ_WEB_HTTPS_CERT",
		"MOQ_WEB_HTTPS_KEY",
		"MOQ_AUTH_URL",
		"MOQ_INTERNAL_LISTEN",
		"MOQ_LISTEN_PREFERRED_V4",
		"MOQ_LISTEN_UNIX_BIND",
		"MOQ_LISTEN_UNIX_ALLOW_UID",
		"MOQ_CLUSTER_LAN",
		"MOQ_CLUSTER_LAN_APP",
	];

	/// Merge each case's file and flags, then check the value and the source it names.
	fn check_provenance(cases: &[Case]) {
		let dir = std::env::temp_dir().join("moq-relay-provenance");
		std::fs::create_dir_all(&dir).unwrap();

		for case in cases {
			let path = dir.join(format!("{}.toml", case.name));
			std::fs::write(&path, case.toml).unwrap();
			let mut args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
			args.extend(case.cli.iter().map(std::ffi::OsString::from));
			let config = Config::parse_and_merge(args).unwrap_or_else(|err| panic!("{}: {err}", case.name));
			(case.check)(&config);
			let source = config
				.source(case.key)
				.unwrap_or_else(|| panic!("{}: missing origin", case.name));
			assert!(
				source.contains(case.source),
				"{}: origin {source:?} should name {:?}",
				case.name,
				case.source
			);
		}
	}

	/// Presence comes from the source, never from whether a standing value looks empty.
	#[test]
	fn provenance_merge_table() {
		if !test_env::Env::without(PROVENANCE_ENV).isolate() {
			return;
		}
		check_provenance(&[
			Case {
				name: "empty-list-from-file",
				toml: "[listen]\nversion = []\n",
				cli: &[],
				key: "listen.version",
				check: |config| assert!(config.listen.version.is_empty(), "file empty list must survive"),
				source: "version",
			},
			Case {
				name: "false-bool-from-file",
				toml: "[stats]\nenabled = false\n",
				cli: &[],
				key: "stats.enabled",
				check: |config| assert!(!config.stats.enabled),
				source: "enabled",
			},
			Case {
				name: "declared-default-true-false-from-file",
				toml: "[web]\nws = false\n",
				cli: &[],
				key: "web.ws",
				check: |config| assert!(!config.web.ws, "file false must beat a declared true default"),
				source: "ws",
			},
			Case {
				name: "optional-with-default-from-file",
				toml: "[stats]\nprefix = \".custom\"\n",
				cli: &[],
				key: "stats.prefix",
				check: |config| assert_eq!(config.stats.prefix, ".custom"),
				source: "prefix",
			},
			Case {
				name: "nested-flatten-from-file",
				toml: "[listen.tls]\ngenerate = [\"localhost\"]\n",
				cli: &[],
				key: "listen.tls.generate",
				check: |config| {
					assert_eq!(config.listen.tls.generate, vec!["localhost".to_string()]);
				},
				source: "generate",
			},
			// One row per value shape a file can carry past an absent flag.
			Case {
				name: "duration-from-file",
				toml: "[cache]\nduration = \"30s\"\n",
				cli: &[],
				key: "cache.duration",
				check: |config| assert_eq!(config.cache.duration, Some(std::time::Duration::from_secs(30).into())),
				source: "duration",
			},
			Case {
				name: "flattened-duration-from-file",
				toml: "[connect]\ntimeout = \"2m\"\n",
				cli: &[],
				key: "connect.timeout",
				check: |config| assert_eq!(config.connect.timeout, Some(std::time::Duration::from_secs(120).into())),
				source: "timeout",
			},
			Case {
				name: "optional-string-from-file",
				toml: "[cluster]\ntier = \"region\"\n",
				cli: &[],
				key: "cluster.tier",
				check: |config| assert_eq!(config.cluster.tier.as_deref(), Some("region")),
				source: "tier",
			},
			Case {
				name: "optional-int-from-file",
				toml: "[cluster]\nid = 12345\n",
				cli: &[],
				key: "cluster.id",
				check: |config| assert_eq!(config.cluster.id, Some(12345)),
				source: "id",
			},
			Case {
				name: "hop-limit-from-file",
				toml: "[cluster]\nmax_hops = 8\n",
				cli: &[],
				key: "cluster.max_hops",
				check: |config| assert_eq!(config.cluster.max_hops, Some(8)),
				source: "max_hops",
			},
			Case {
				name: "flattened-optional-int-from-file",
				toml: "[quic]\nreceive_window = 67108864\n",
				cli: &[],
				key: "quic.receive_window",
				check: |config| assert_eq!(config.quic.receive_window, Some(67108864)),
				source: "receive_window",
			},
			Case {
				name: "optional-bool-from-file",
				toml: "[connect.tls]\nsystem_roots = true\n",
				cli: &[],
				key: "connect.tls.system_roots",
				check: |config| assert_eq!(config.connect.tls.system_roots, Some(true)),
				source: "system_roots",
			},
			Case {
				name: "enum-from-file",
				toml: "[quic]\ncongestion_control = \"delay\"\n",
				cli: &[],
				key: "quic.congestion_control",
				check: |config| {
					assert_eq!(
						config.quic.congestion_control,
						Some(moq_tokio::quic::CongestionControl::Delay)
					)
				},
				source: "congestion_control",
			},
			Case {
				name: "path-from-file",
				toml: "[quic]\nqlog = \"/tmp/moq-qlog\"\n",
				cli: &[],
				key: "quic.qlog",
				check: |config| assert_eq!(config.quic.qlog.as_deref(), Some(std::path::Path::new("/tmp/moq-qlog"))),
				source: "qlog",
			},
			Case {
				name: "path-list-from-file",
				toml: "[web.https]\ncert = [\"a.pem\", \"b.pem\"]\nkey = [\"a.key\", \"b.key\"]\n",
				cli: &[],
				key: "web.https.cert",
				check: |config| {
					assert_eq!(
						config.web.https.cert,
						vec![std::path::PathBuf::from("a.pem"), std::path::PathBuf::from("b.pem")]
					)
				},
				source: "cert",
			},
			Case {
				name: "url-from-file",
				toml: "[auth]\nurl = \"https://auth.example.com/\"\n",
				cli: &[],
				key: "auth.url",
				check: |config| {
					assert_eq!(
						config.auth.url.as_ref().map(url::Url::as_str),
						Some("https://auth.example.com/")
					)
				},
				source: "url",
			},
			Case {
				name: "socket-addr-from-file",
				toml: "[internal]\nlisten = \"127.0.0.1:9101\"\n",
				cli: &[],
				key: "internal.listen",
				check: |config| assert_eq!(config.internal.listen, Some("127.0.0.1:9101".parse().unwrap())),
				source: "listen",
			},
			Case {
				name: "flattened-socket-addr-from-file",
				toml: "[listen]\npreferred_v4 = \"192.0.2.1:443\"\n",
				cli: &[],
				key: "listen.preferred_v4",
				check: |config| assert_eq!(config.listen.preferred_v4, Some("192.0.2.1:443".parse().unwrap())),
				source: "preferred_v4",
			},
			#[cfg(all(feature = "uds", unix))]
			Case {
				name: "int-list-from-file",
				toml: "[listen.unix]\nbind = \"/run/moq/internal.sock\"\n\n[listen.unix.allow]\nuid = [1001]\n",
				cli: &[],
				key: "listen.unix.allow.uid",
				check: |config| {
					assert_eq!(
						config.listen.unix.bind.as_deref(),
						Some(std::path::Path::new("/run/moq/internal.sock"))
					);
					assert_eq!(config.listen.unix.allow.uid, vec![1001]);
				},
				source: "uid",
			},
			#[cfg(feature = "cluster-lan")]
			Case {
				name: "newtype-from-file",
				toml: "[cluster.lan]\nenabled = true\napp = \"custom\"\n",
				cli: &[],
				key: "cluster.lan.app",
				check: |config| {
					assert_eq!(
						config.cluster.lan.app.as_ref().map(ToString::to_string).as_deref(),
						Some("custom")
					)
				},
				source: "app",
			},
		]);
	}

	/// The environment beats the file and loses to the CLI.
	#[test]
	fn provenance_merge_from_the_environment() {
		if !test_env::Env::without(PROVENANCE_ENV)
			.set("MOQ_STATS_ENABLED", "true")
			.isolate()
		{
			return;
		}
		check_provenance(&[
			Case {
				name: "env-overrides-file",
				toml: "[stats]\nenabled = false\n",
				cli: &[],
				key: "stats.enabled",
				check: |config| assert!(config.stats.enabled),
				source: "MOQ_STATS_ENABLED",
			},
			Case {
				name: "cli-overrides-env-and-file",
				toml: "[stats]\nenabled = true\n",
				cli: &["--stats-enabled=false"],
				key: "stats.enabled",
				check: |config| assert!(!config.stats.enabled),
				source: "--stats-enabled",
			},
		]);
	}

	/// The hop limit follows the same precedence: environment over file, CLI over both.
	#[test]
	fn hop_limit_merges_from_the_environment() {
		if !test_env::Env::without(PROVENANCE_ENV)
			.set("MOQ_CLUSTER_MAX_HOPS", "6")
			.isolate()
		{
			return;
		}
		check_provenance(&[
			Case {
				name: "hop-limit-env-overrides-file",
				toml: "[cluster]\nmax_hops = 8\n",
				cli: &[],
				key: "cluster.max_hops",
				check: |config| assert_eq!(config.cluster.max_hops, Some(6)),
				source: "MOQ_CLUSTER_MAX_HOPS",
			},
			Case {
				name: "hop-limit-cli-overrides-env-and-file",
				toml: "[cluster]\nmax_hops = 8\n",
				cli: &["--cluster-max-hops", "4"],
				key: "cluster.max_hops",
				check: |config| assert_eq!(config.cluster.max_hops, Some(4)),
				source: "--cluster-max-hops",
			},
		]);
	}

	/// The variables the public patterns could also be read from.
	const PUBLIC_ENV: &[&str] = &[
		"MOQ_AUTH_PUBLIC",
		"MOQ_AUTH_PUBLIC_SUBSCRIBE",
		"MOQ_AUTH_PUBLIC_PUBLISH",
		"MOQ_AUTH_URL",
	];

	/// The public patterns arrive from the CLI and TOML alike, as patterns rather
	/// than prefixes.
	#[test]
	fn public_patterns_merge_from_every_source() {
		if !test_env::Env::without(PUBLIC_ENV).isolate() {
			return;
		}

		let config = Config::parse_and_merge([
			"moq-relay",
			"--auth-public-subscribe",
			"demo/**",
			"--auth-public-publish",
			"uploads/**",
		])
		.expect("config load");

		assert!(
			config.auth.validate(false).is_ok(),
			"CLI public flags must admit anonymous sessions"
		);
		assert_eq!(config.auth.public_subscribe, vec!["demo/**".parse().unwrap()]);
		assert_eq!(config.auth.public_publish, vec!["uploads/**".parse().unwrap()]);
		assert!(
			config
				.source("auth.public_subscribe")
				.is_some_and(|source| source.contains("--auth-public-subscribe")),
			"origin {:?}",
			config.source("auth.public_subscribe")
		);

		let toml = "[auth]\npublic = \"anon/**\"\n";
		let dir = std::env::temp_dir().join("moq-relay-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("auth-public-toml.toml");
		std::fs::write(&path, toml).unwrap();
		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("toml load");
		assert_eq!(config.auth.public, vec!["anon/**".parse().unwrap()]);
	}

	/// The environment carries a list of public patterns, comma separated.
	#[test]
	fn public_patterns_merge_from_the_environment() {
		if !test_env::Env::without(PUBLIC_ENV)
			.set("MOQ_AUTH_PUBLIC_SUBSCRIBE", "from-env/**,other/**")
			.isolate()
		{
			return;
		}
		let config = Config::parse_and_merge(["moq-relay"]).expect("env load");
		assert_eq!(
			config.auth.public_subscribe,
			vec!["from-env/**".parse().unwrap(), "other/**".parse().unwrap()]
		);
	}

	/// The variables a QUIC-LB setting could also be read from.
	const QUIC_LB_ENV: &[&str] = &["MOQ_LISTEN_QUIC_LB_ID", "MOQ_LISTEN_QUIC_LB_NONCE"];

	/// Merge `body` as the config file named `name` with `cli`, and require that the
	/// listener refuses to start with an error containing `needle`.
	fn refuse_quic_lb(name: &str, body: &str, cli: &[&str], needle: &str) {
		let dir = std::env::temp_dir().join("moq-relay-lb-refusals");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join(name);
		std::fs::write(&path, body).unwrap();
		let mut args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		args.extend(cli.iter().copied().map(std::ffi::OsString::from));
		let config = Config::parse_and_merge(args).unwrap_or_else(|err| panic!("{name} parsed: {err}"));
		match config.listen.init(Default::default()) {
			Err(err) => assert!(err.to_string().contains(needle), "{name}: {err}"),
			Ok(_) => panic!("{name} started"),
		}
	}

	/// A QUIC-LB nonce with no server id stops startup from TOML, and CLI
	/// assignments override the same nested fields.
	///
	/// The CLI flag pair already requires `--listen-quic-lb-id`, so this does not
	/// add a second command-line error for that spelling.
	#[test]
	fn quic_lb_nonce_and_conflicts_refuse_to_start() {
		if !test_env::Env::without(QUIC_LB_ENV).isolate() {
			return;
		}

		let err = Config::parse_and_merge(["moq-relay", "--listen-quic-lb-nonce", "8"])
			.expect_err("the flag pair is already required");
		assert!(err.to_string().contains("--listen-quic-lb-id"), "{err}");

		refuse_quic_lb(
			"nonce.toml",
			"[listen.lb]\nnonce = 8\n",
			&[],
			"requires --listen-quic-lb-id",
		);

		#[cfg(feature = "quic")]
		{
			let dir = std::env::temp_dir().join("moq-relay-lb-refusals");
			std::fs::create_dir_all(&dir).unwrap();
			let path = dir.join("typed.toml");
			std::fs::write(&path, "[listen.lb]\nid = \"ab\"\nnonce = 8\n").unwrap();
			let config =
				Config::parse_and_merge([std::ffi::OsString::from("moq-relay"), path.clone().into()]).expect("typed");
			let load_balancer = config.listen.lb;
			assert_eq!(load_balancer.id, Some("ab".parse().unwrap()));
			assert_eq!(load_balancer.nonce, Some(8));
			let config = Config::parse_and_merge([
				std::ffi::OsString::from("moq-relay"),
				path.into(),
				"--listen-quic-lb-id".into(),
				"ee".into(),
			])
			.expect("CLI overrides the same nested config field");
			let load_balancer = config.listen.lb;
			assert_eq!(load_balancer.id, Some("ee".parse().unwrap()));
			assert_eq!(load_balancer.nonce, Some(8));
		}
	}

	/// A QUIC-LB nonce from the environment with no server id stops startup too.
	#[test]
	fn quic_lb_env_nonce_without_an_id_refuses_to_start() {
		if !test_env::Env::without(QUIC_LB_ENV)
			.set("MOQ_LISTEN_QUIC_LB_NONCE", "8")
			.isolate()
		{
			return;
		}
		let err = Config::parse_and_merge(["moq-relay"]).expect_err("env nonce without id");
		assert!(err.to_string().contains("--listen-quic-lb-id"), "{err}");
	}
}
