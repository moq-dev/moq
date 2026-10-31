//! The unified moq-cli argument surface.
//!
//! Grammar: `moq <MoQ side> <stage> [-- <stage>]...`, where a stage is
//! `<import|export> <endpoint> [endpoint opts]`, plus `moq <MoQ side> play` for
//! native playback, `moq <MoQ side> announced [prefix]` to follow what is announced, and
//! `moq <MoQ side> fetch <track>` to read one group.
//!
//! - The MoQ side (`--connect`, the `--listen*` transport binds, `--cluster-lan`,
//!   and `--cluster-connect` / `--cluster-api-url`; all optional, at least
//!   one) attaches the shared Origin to the MoQ network, and comes before the
//!   first stage. They compose: dial a relay, accept incoming sessions, mesh
//!   with the LAN, and join a cluster all at once.
//! - `import` routes media INTO MoQ from one source; `export` routes it OUT to
//!   one sink. The verb fixes the data direction (and thus, for the
//!   bidirectional gateways, whether `--connect`/`--listen` push or pull).
//! - `devices` and `token` touch no network at all, so they're the verbs that take
//!   no MoQ side. That's why the requirement is enforced per-verb
//!   ([`MoqSide::validate`]) rather than by the parser: an argument group can't be
//!   conditional on the subcommand.
//! - The endpoint is one subcommand: a container format (`ts`, `fmp4`, ... read
//!   from stdin on import, written to stdout on export) or a gateway (`hls`,
//!   `rtmp`, `srt`, `rtc`, `archive`). Exactly one per stage, so "which endpoint" is
//!   unambiguous and there's no silently-ignored flag.
//! - `--` starts another stage on the same Origin and the same MoQ attachment, so
//!   one process can bridge several broadcasts (or both directions at once). Usage
//!   can't express a repeated subcommand, so [`Invocation`] splits argv on `--`
//!   and runs each chunk through a real parser: every stage keeps full validation
//!   and its own `--help`. That claims `--` from Usage, which would otherwise treat
//!   it as the end-of-options marker. The only positional it could have escaped is
//!   an `import hls` playlist path starting with `-`, which `./-name` covers, so
//!   the separator stays unconditional rather than context-sensitive.

use std::ffi::{OsStr, OsString};
use std::time::Duration;

use crate::publish::PublishFormat;
use crate::subscribe::{CatalogFormatArg, SubscribeFormat};

// The globals plus the first stage; later stages are parsed as a [`Stage`]. Keep
// the doc comment to one line: Usage renders the rest as `--help` body text, where
// rustdoc links read as noise.
/// moq-cli: a media router that wires endpoints onto a shared MoQ Origin.
#[derive(usage_rs::Cli, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq", version = env!("CARGO_PKG_VERSION"))]
#[usage(completion, settings)]
#[usage(after_help = "Separate additional import/export stages with `--`; they share one \
                        connection and one Origin. Every `--` starts a stage, so it is not an \
                        end-of-options marker: write a path starting with `-` as `./-name`.")]
pub struct Cli {
	/// Logging configuration.
	#[usage(flatten)]
	pub log: moq_tokio::Log,

	/// The MoQ attachment, shared by both directions.
	#[usage(flatten)]
	pub moq: MoqSide,

	/// The verb and endpoint.
	#[usage(subcommand)]
	pub command: Command,
}

// `no_binary_name` because the chunk after a `--` starts at the verb, and the
// globals are deliberately absent: `--connect` past the first stage would
// read like it scopes that stage, when there is only ever one connection. As with
// [`Cli`], the doc comment stays one line because Usage shows it in `--help`.
/// A stage after the first: the verb and endpoint, without the globals.
#[derive(usage_rs::Cli, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq")]
#[usage(completion, settings)]
pub struct Stage {
	/// The verb and endpoint.
	#[usage(subcommand)]
	pub command: Command,
}

/// The whole command line: the globals plus one or more `--`-separated stages.
pub struct Invocation {
	/// Logging configuration.
	pub log: moq_tokio::Log,

	/// The MoQ attachment, shared by every stage.
	pub moq: MoqSide,

	/// The MoQ-side flags the command line typed, each once, in order.
	///
	/// Only [`Self::reject`] and [`Self::dial_only`] read it. A verb refuses a MoQ
	/// side the user asked for, and an exported `MOQ_CONNECT` is not an ask: it is a
	/// standing setting for the publishing this shell usually does, and it would
	/// otherwise make `moq auth` and `moq completion` fail for everyone who has one.
	/// Read from the parse itself rather than from the built fields, so a flag added
	/// to any flattened config is refused without anyone listing it here.
	given: Vec<&'static usage_rs::Flag<'static>>,

	/// The stages, in the order given. Never empty.
	pub stages: Vec<Command>,
}

/// Broad category for an invocation parse failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseErrorKind {
	/// An argument or flag was not recognized.
	UnknownArgument,
	/// A command or stage was missing.
	MissingSubcommand,
	/// An argument value or deprecated spelling was invalid.
	ValueValidation,
	/// Help was requested.
	DisplayHelp,
	/// Version information was requested.
	DisplayVersion,
	/// Another parser constraint failed.
	Other,
}

/// An owned, rendered invocation parse failure.
#[derive(Debug)]
pub struct ParseError {
	kind: ParseErrorKind,
	message: String,
}

impl ParseError {
	/// The broad failure category.
	#[cfg_attr(not(test), allow(dead_code, reason = "only tests use it"))]
	pub fn kind(&self) -> ParseErrorKind {
		self.kind
	}

	fn new(kind: ParseErrorKind, message: impl Into<String>) -> Self {
		Self {
			kind,
			message: message.into(),
		}
	}

	fn exit(self) -> ! {
		if matches!(self.kind, ParseErrorKind::DisplayHelp | ParseErrorKind::DisplayVersion) {
			print!("{}", self.message);
			std::process::exit(0);
		}
		eprint!("{}", self.message);
		std::process::exit(2)
	}
}

impl std::fmt::Display for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.message)
	}
}

impl std::error::Error for ParseError {}

/// The `MOQ_*` variables read outside the command-line spec.
#[cfg(any(feature = "capture", feature = "transcode", feature = "play"))]
const DIRECT_ENV: &[&str] = moq_video::ENV;
#[cfg(not(any(feature = "capture", feature = "transcode", feature = "play")))]
const DIRECT_ENV: &[&str] = &[];

/// Whether `flag` is one of the flags `T` declares.
fn owns<T: usage_rs::spec::CommandArgs>(flag: &usage_rs::Flag<'_>) -> bool {
	T::COMMAND.flags.iter().any(|own| own.key == flag.key)
}

impl Invocation {
	/// Parse the process arguments, exiting with Usage's rendered message on error.
	///
	/// Async because a completion request is answered first, and the capture
	/// completers enumerate devices asynchronously (see [`crate::complete`]).
	pub async fn parse() -> Self {
		let args: Vec<OsString> = std::env::args_os().collect();
		// `#[usage(completion)]` installs the `__complete_word__` interception in the
		// generated `Cli::parse()`, which the stage grammar cannot use: without this
		// the request reaches the ordinary grammar and is refused. Recognized before
		// the split on `--`, because a completion is not a command this binary runs.
		if let Some(reply) = crate::complete::answer(args.get(1..).unwrap_or_default()).await {
			print!("{reply}");
			std::process::exit(0);
		}
		match Self::try_parse_from(args).and_then(|parsed| {
			// Here rather than in `try_parse_from`, which tests drive under whatever
			// environment the test runner has.
			moq_tokio::cli::check_env(&[Cli::spec(), Stage::spec()], DIRECT_ENV)
				.map_err(|err| ParseError::new(ParseErrorKind::ValueValidation, format!("error: {err}\n")))?;
			Ok(parsed)
		}) {
			Ok(parsed) => parsed,
			Err(err) => err.exit(),
		}
	}

	/// Refuse every MoQ-side flag on a verb that runs locally and takes none, rather
	/// than silently ignoring it.
	///
	/// `--broadcast` counts: a local verb has no content, and next to `auth generate`
	/// it reads like it scopes the key, which `--root` does. Answered from what the
	/// command line said, never from the environment; see [`Self::given`].
	pub fn reject(&self, command: &str) -> anyhow::Result<()> {
		if let Some(flags) = Self::names(self.given.iter()) {
			anyhow::bail!("`{command}` runs locally and takes no MoQ side; drop {flags}");
		}
		Ok(())
	}

	/// Refuse every MoQ-side flag but the dial and those in `allow`, on a verb that
	/// only reads from a relay: a listener, cluster, or auth policy it would never
	/// serve is not silently ignored. The dial is `--connect*` and the `--quic-*` and
	/// `--iroh-*` settings it dials with. Answered from the command line, like
	/// [`Self::reject`].
	pub fn dial_only(&self, command: &str, allow: &[&str]) -> anyhow::Result<()> {
		let dials = |flag: &&&usage_rs::Flag<'_>| {
			#[cfg(feature = "iroh")]
			if owns::<moq_tokio::iroh::Config>(flag) {
				return true;
			}
			owns::<moq_tokio::connect::Config>(flag)
				|| owns::<moq_tokio::quic::Config>(flag)
				|| allow
					.iter()
					.any(|name| name.strip_prefix("--").is_some_and(|name| flag.longs.contains(&name)))
		};

		if let Some(flags) = Self::names(self.given.iter().filter(|flag| !dials(flag))) {
			anyhow::bail!("`{command}` only dials a relay with --connect; drop {flags}");
		}
		Ok(())
	}

	/// Refuse listener, auth, and drain settings when nothing listens, rather
	/// than silently ignoring them. Answered from the command line, like
	/// [`Self::reject`].
	pub fn unserved(&self) -> anyhow::Result<()> {
		if self.moq.serves() {
			return Ok(());
		}
		let listens = |flag: &&&usage_rs::Flag<'_>| {
			owns::<moq_tokio::listen::Config>(flag)
				|| owns::<moq_relay::auth::Config>(flag)
				|| flag.longs.contains(&"drain-timeout")
		};
		if let Some(flags) = Self::names(self.given.iter().filter(listens)) {
			anyhow::bail!(
				"{flags} configure a listener, but none is bound; add --listen, --listen-tcp-bind, --listen-unix-bind, or --cluster-lan"
			);
		}
		Ok(())
	}

	/// `--a, --b` for the flags given, or `None` when there are none.
	fn names<'a>(flags: impl Iterator<Item = &'a &'static usage_rs::Flag<'static>>) -> Option<String> {
		let names: Vec<String> = flags.map(|flag| format!("--{}", flag.longs[0])).collect();
		(!names.is_empty()).then(|| names.join(", "))
	}

	/// Split `argv` on `--` and run each chunk through a real parser.
	pub fn try_parse_from<I, T>(argv: I) -> Result<Self, ParseError>
	where
		I: IntoIterator<Item = T>,
		T: Into<OsString>,
	{
		let argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
		let mut chunks = argv.split(|arg| arg == OsStr::new("--"));

		// `split` always yields at least one chunk, even for an empty argv; Usage then
		// reports the missing subcommand as usual.
		let first = chunks.next().unwrap_or_default();
		let first = first.iter().skip(1).map(OsString::as_os_str).collect::<Vec<_>>();
		let cli = Cli::parse_from(&first).map_err(|err| parse_error(Cli::spec(), Cli::command(), &first, err))?;
		let given = MoqSide::given(&first);

		let mut deprecated = cli.moq.deprecated();
		deprecated.extend(cli.command.deprecated());
		let mut stages = vec![cli.command.current()];
		for chunk in chunks {
			// A trailing or doubled `--` leaves an empty chunk, which Usage would report as a
			// bare missing-subcommand usage dump. Name what's actually wrong instead.
			if chunk.is_empty() {
				return Err(ParseError::new(
					ParseErrorKind::MissingSubcommand,
					"error: `--` starts another stage, so it must be followed by `import` or `export`\n",
				));
			}

			let chunk = chunk.iter().map(OsString::as_os_str).collect::<Vec<_>>();
			let command = Stage::parse_from(&chunk)
				.map_err(|err| parse_error(Stage::spec(), Stage::command(), &chunk, err))?
				.command;
			deprecated.extend(command.deprecated());
			stages.push(command.current());
		}

		// Before anything reads the config: a released spelling parses into a hidden
		// field that nothing honors, so continuing would run on settings the command
		// line never asked for. Every stage is in by now, since a stage can carry a
		// config of its own. A Usage error, since that is what this is.
		if !deprecated.is_empty() {
			return Err(ParseError::new(
				ParseErrorKind::ValueValidation,
				format!("error: {deprecated}\n"),
			));
		}

		Ok(Self {
			log: cli.log,
			moq: cli.moq,
			given,
			stages,
		})
	}

	/// Reject the stage combinations a single process can't run.
	///
	/// Called before anything binds a port or dials out, so a refused invocation has
	/// no side effects to unwind.
	pub fn validate(&self) -> anyhow::Result<()> {
		if !self.stages.iter().all(Command::is_stageable) {
			let listener_settings = self.given.iter().filter(|flag| flag.longs.contains(&"drain-timeout"));
			if let Some(flags) = Self::names(listener_settings) {
				anyhow::bail!("{flags} configure listening import/export stages");
			}
		}

		for command in &self.stages {
			#[cfg(feature = "capture")]
			if let Command::Import(import) = command
				&& let ImportSource::Capture(capture) = &import.source
			{
				capture.validate()?;
			}
			if let Command::Import(import) = command
				&& import.report
				&& let Some(reason) = import.source.report_refusal()
			{
				anyhow::bail!("--report names a track in the catalog the import builds, but {reason}");
			}
			if let Command::Import(import) = command
				&& let Some(path) = &import.echo_path
			{
				if let Some(reason) = import.source.catalog_refusal() {
					anyhow::bail!("--echo-path solicits feedback in the catalog the import builds, but {reason}");
				}
				// A reader refuses the whole catalog over a path it cannot resolve.
				let broadcast = command.broadcast(&self.moq);
				let rel = hang::moq_net::path::Relative::new(path)
					.map_err(|err| anyhow::anyhow!("invalid --echo-path: {err}"))?;
				let broadcast = hang::moq_net::Path::new(&broadcast)
					.map_err(|err| anyhow::anyhow!("invalid --broadcast: {err}"))?;
				let Some(prefix) = broadcast.try_resolve(&rel) else {
					anyhow::bail!("--echo-path `{path}` walks above the connection root from broadcast `{broadcast}`");
				};
				// An origin refuses a broadcast a pattern could not name, so no viewer could
				// publish its `<NAME>.echo` beneath this.
				let feedback = prefix.join(&hang::moq_net::Path::from_static("viewer.echo"));
				let literal = feedback
					.map_err(anyhow::Error::from)
					.and_then(|feedback| Ok(hang::moq_net::Pattern::literal(feedback.as_str())?));
				if let Err(err) = literal {
					anyhow::bail!(
						"--echo-path `{path}` resolves to `{prefix}`, where no viewer can publish its feedback: {err}"
					);
				}
			}
			if let Command::Import(import) = command
				&& import.linger.is_some()
				&& let Some(reason) = import.source.linger_refusal()
			{
				anyhow::bail!("--linger keeps a cleanly ended broadcast up, but {reason}");
			}
			let Command::Export(export) = command else {
				continue;
			};
			export.select.validate()?;
			if let Some(sink) = export.sink.report() {
				anyhow::ensure!(
					export.catalog_format.is_none(),
					"`export {sink}` reads no media, so it takes no --catalog-format"
				);
				anyhow::ensure!(
					export.echo.is_none(),
					"`export {sink}` reads no media to report on; remove --echo"
				);
			}
			if matches!(export.sink, ExportSink::Archive(_)) {
				anyhow::ensure!(
					export.catalog_format.is_none(),
					"`export archive` records every track without reading a catalog, so it takes no --catalog-format"
				);
			}
			if export.echo.is_some() {
				if export.sink.stdout().is_none()
					&& let Some(sink) = export.sink.ignores_selection()
				{
					anyhow::bail!(
						"--echo reports what a stdout container export reads, and `export {sink}` is not one"
					);
				}
				let msf = match export.catalog_format {
					Some(format) => matches!(format, CatalogFormatArg::Msf),
					None => matches!(
						moq_mux::catalog::CatalogFormat::detect(&command.broadcast(&self.moq)),
						Some(moq_mux::catalog::CatalogFormat::Msf)
					),
				};
				anyhow::ensure!(
					!msf,
					"--echo follows the hang catalog's `echo` section, which the MSF catalog does not carry"
				);
			}
			if let Some(stdout) = export.sink.stdout() {
				anyhow::ensure!(
					stdout.follow.is_zero() || matches!(stdout.format, SubscribeFormat::Ts),
					"--linger is gone, and following a broadcast's return needs an output that can mark the restart: only `export ts --follow` can"
				);
				if matches!(stdout.format, SubscribeFormat::H264 | SubscribeFormat::H265) {
					anyhow::ensure!(
						export.select.video != Some(crate::subscribe::Mode::Off),
						"--video off leaves nothing for a video elementary stream; pick a container format"
					);
					if let Some(flag) = export.select.audio_flag() {
						anyhow::bail!("a video elementary stream has no audio; remove {flag}");
					}
				}
			}
			if let Some(sink) = export.sink.ignores_selection()
				&& let Some(flag) = export.select.flag()
			{
				anyhow::bail!("`export {sink}` can't select renditions; remove {flag}");
			}
		}

		// One stage is what the CLI has always run, so nothing below can bite.
		if self.stages.len() == 1 {
			return Ok(());
		}

		// Only `import` and `export` share an Origin. The rest own the process: `play`
		// drives a window on the main thread, `transcode` builds its own Origin, `fetch`
		// and `announced` open their own session, and `auth` / `devices` never touch the network at all.
		if let Some(command) = self.stages.iter().find(|command| !command.is_stageable()) {
			anyhow::bail!(
				"`{}` must be the only verb; it can't share a process with another `--` stage",
				command.name()
			);
		}

		Ok(())
	}
}

/// Turn a Usage parse result into a [`ParseError`].
///
/// The rendering lives in [`moq_tokio::cli::answer`], shared with moq-relay and
/// moq-bench, which parse more than once for their own reasons. This adds the
/// failure category, which only this crate's callers ask about.
fn parse_error(
	spec: &usage_rs::argv::spec::Spec<'_>,
	root: &usage_rs::Command<'_>,
	argv: &[&OsStr],
	err: usage_rs::Error<'_, '_>,
) -> ParseError {
	let kind = match &err {
		usage_rs::Error::Help { .. } | usage_rs::Error::HelpAll { .. } => ParseErrorKind::DisplayHelp,
		usage_rs::Error::Version { .. } => ParseErrorKind::DisplayVersion,
		usage_rs::Error::UnknownFlag { .. } | usage_rs::Error::UnexpectedArg { .. } => ParseErrorKind::UnknownArgument,
		usage_rs::Error::MissingSubcommand | usage_rs::Error::MissingArgsHelp { .. } => {
			ParseErrorKind::MissingSubcommand
		}
		usage_rs::Error::InvalidValue(_) | usage_rs::Error::InvalidChoice { .. } => ParseErrorKind::ValueValidation,
		_ => ParseErrorKind::Other,
	};
	ParseError::new(kind, moq_tokio::cli::answer(spec, root, argv, err).message())
}

/// The MoQ attachment: a relay dial, a server listener, a LAN mesh, or any
/// combination.
///
/// The group is not `required`, because the local verbs (`token`, `devices`) run
/// without a MoQ side. Every verb that does need one calls
/// [`validate`](Self::validate).
///
/// The three transport sections are read as plain fields. [`Invocation`] refuses a
/// released spelling while parsing, so a field left unset here means the command
/// line really did leave it unset.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct MoqSide {
	/// The default broadcast name for every stage that doesn't name its own.
	///
	/// Optional for the point endpoints (stdin/stdout, HLS import, and the
	/// `--connect` dials), which default to the root broadcast at the connection
	/// path; required by the `--listen` endpoints and `hls export`, which bridge one
	/// named broadcast.
	#[usage(long, help_heading = "MoQ")]
	pub broadcast: Option<String>,

	/// The released spelling of [`Self::broadcast`].
	#[usage(long = "name", hide = true)]
	name: Option<String>,

	/// MoQ client config (`--connect`, `--connect-bind`, `--connect-tls-*`, ...).
	#[usage(flatten)]
	pub client: moq_tokio::connect::Config,

	/// QUIC transport tuning (`--quic-*`), shared by the dial and accept sides.
	#[usage(flatten)]
	pub quic: moq_tokio::quic::Config,

	/// MoQ server transport config (`--listen`, `--listen-tcp-bind`,
	/// `--listen-unix-bind`, `--listen-tls-*`).
	#[usage(flatten)]
	pub server: moq_tokio::listen::Config,

	/// Iroh transport config (`--iroh-*`), used by both the client and server.
	#[cfg(feature = "iroh")]
	#[usage(flatten)]
	pub iroh: moq_tokio::iroh::Config,

	/// Clustering config (`--cluster-*`, including LAN). The same flags as
	/// `moq-relay`, so a CLI process and a relay on the same network mesh
	/// through one implementation.
	#[usage(flatten)]
	pub cluster: moq_relay::cluster::Config,

	/// Who a `--listen` endpoint admits: `--auth-url` asks an auth server per
	/// session, `--auth-public` grants anonymous patterns. The same flags as
	/// `moq-relay`; a listener needs exactly one.
	#[usage(flatten)]
	pub auth: moq_relay::auth::Config,

	/// Drain accepted sessions for up to this long after SIGINT or SIGTERM; defaults to 10s.
	#[usage(long, env = "MOQ_DRAIN_TIMEOUT", help_heading = "MoQ")]
	pub drain_timeout: Option<moq_tokio::cli::Duration>,
}

impl MoqSide {
	/// Every released spelling this invocation used, across all three sections.
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = self.client.deprecated();
		found.extend(self.quic.deprecated());
		found.extend(self.server.deprecated());
		found.extend(self.cluster.deprecated());
		found.extend(self.auth.deprecated());
		if self.name.is_some() {
			found.flag("--name", None, "--broadcast");
		}
		found
	}

	/// The cluster this process publishes and subscribes on. Built once; the
	/// origin is its origin, whose Hop ID is random per run unless `--cluster-id`
	/// names the node.
	pub fn cluster(&self) -> anyhow::Result<moq_relay::cluster::Cluster> {
		moq_relay::cluster::Cluster::new(moq_relay::cluster::Options::new(self.cluster.clone()))
	}

	/// Whether `--cluster-lan` asked this process to mesh over the LAN.
	pub fn lan(&self) -> bool {
		#[cfg(feature = "cluster-lan")]
		return self.cluster.lan.enabled;
		#[cfg(not(feature = "cluster-lan"))]
		false
	}

	/// The server to bind, which the LAN mesh shares with ordinary clients.
	///
	/// `--cluster-lan` needs a listener for peers to dial, so it fills in the two
	/// things the user would otherwise have to spell out: an ephemeral port and a
	/// generated certificate. An explicit `--listen` or `--listen-tls-*` wins, which
	/// is what puts the mesh on the same port and certificate as everything else.
	pub fn server_config(&self) -> moq_tokio::listen::Config {
		let mut config = self.server.clone();
		if self.lan() {
			config
				.bind
				.get_or_insert_with(|| moq_tokio::listen::Bind::Addr("[::]:0".parse().unwrap()));
			if config.tls.generate.is_empty() && config.tls.cert.is_empty() {
				config.tls.generate = vec!["moq-cluster-lan".to_string()];
			}
		}
		config
	}

	/// Whether a listener has to be bound at all.
	pub fn serves(&self) -> bool {
		self.server.has_explicit_bind() || self.lan()
	}

	/// Whether a WAN cluster dial is configured (`--cluster-connect` or
	/// `--cluster-api-url`).
	fn cluster_dials(&self) -> bool {
		!self.cluster.connect.is_empty() || self.cluster.api.url.is_some()
	}

	/// Reject a verb that needs the MoQ network but was given no way to reach it.
	/// Stands in for the Usage `required` the `moq` group can't carry, since
	/// `devices` is exempt.
	pub fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.client.url.is_some() || self.serves() || self.cluster_dials(),
			"a MoQ side is required: pass --connect <url> to dial a relay, a --listen option to self-host, --cluster-lan to mesh over the LAN, or --cluster-connect to join a cluster"
		);
		// The `--connect` client and the cluster's WAN dials each bind
		// `--connect-bind` for QUIC, so a fixed port would collide halfway through startup.
		#[cfg(feature = "quic")]
		anyhow::ensure!(
			!(self.client.url.is_some()
				&& self.cluster_dials()
				&& self.client.bind.is_some_and(|bind| bind.port() != 0)),
			"--connect-bind with a fixed port cannot serve both --connect and --cluster-connect / --cluster-api-url, which each bind it: use port 0, or drop --connect (unset MOQ_CONNECT if it supplies the URL)"
		);
		#[cfg(feature = "cluster-lan")]
		{
			self.cluster.lan.validate()?;
			if self.lan() {
				moq_relay::cluster::Cluster::validate_lan_versions(&self.client, &self.server_config())?;
			}
		}
		// A listener for ordinary clients admits nobody without a decision; a mesh
		// listener alone admits its peers by their LAN credential.
		anyhow::ensure!(
			!(self.server.has_explicit_bind() && self.auth.is_empty()),
			"--listen needs --auth-url or --auth-public"
		);
		if !self.auth.is_empty() {
			self.auth.validate(self.client_ca())?;
		}
		Ok(())
	}

	/// Whether the listener verifies client certificates (`--listen-tls-root`).
	pub fn client_ca(&self) -> bool {
		!self.server.tls.root.is_empty()
	}

	/// The MoQ-side flags one chunk of a command line typed, each once, in order.
	///
	/// A flattened config's flags sit in this struct's table under the keys that
	/// config minted, so the table is the complete list. The chunk already parsed,
	/// so nothing stops the walk early.
	fn given(argv: &[&OsStr]) -> Vec<&'static usage_rs::Flag<'static>> {
		use usage_rs::spec::CommandArgs;

		let mut given: Vec<&'static usage_rs::Flag<'static>> = Vec::new();
		let mut parser = usage_rs::Parser::new(Cli::command(), argv);
		while let Some(Ok(event)) = parser.next_event() {
			if let usage_rs::Event::Flag { flag, .. } = event
				&& <Self as CommandArgs>::COMMAND
					.flags
					.iter()
					.any(|own| own.key == flag.key)
				&& !given.iter().any(|seen| seen.key == flag.key)
			{
				given.push(flag);
			}
		}
		given
	}
}

/// The verb: for `import`/`export` it is also the data direction, the pivot
/// between the MoQ side and the endpoint.
#[derive(usage_rs::Subcommands, Clone)]
pub enum Command {
	/// Route media INTO MoQ from one source.
	Import(Import),
	/// Route media OUT OF MoQ to one sink.
	Export(Export),
	/// The released spelling of [`Self::Import`].
	#[usage(hide = true)]
	Publish(Import),
	/// The released spelling of [`Self::Export`].
	#[usage(hide = true)]
	Subscribe(Export),
	/// Follow the broadcasts announced on a relay as they start and end.
	Announced(crate::announced::Args),
	/// Write one group of a track to stdout.
	Fetch(crate::fetch::Args),
	/// Play a broadcast in a native window and speaker.
	#[cfg(feature = "play")]
	Play(crate::play::Args),
	/// Re-encode `--broadcast` into a lower ladder, published next to it and
	/// only encoded while watched (just-in-time).
	#[cfg(feature = "transcode")]
	Transcode(crate::transcode::Args),
	/// Generate, sign, and verify the JWT tokens a relay authenticates with.
	Auth(crate::auth::Args),
	/// Write the shell script that completes this command line.
	Completion(crate::complete::Args),
	/// List the capture devices `import capture` can name.
	#[cfg(feature = "capture")]
	Devices,
}

impl Command {
	/// Every released spelling this stage's own args were parsed from.
	///
	/// The globals are only half the command line: a stage can flatten a
	/// `moq-tokio` config of its own, and `export hls` does. Its TLS section is the
	/// sharp case, because the listener decides whether to serve TLS at all from the
	/// canonical `cert`/`generate` fields, so a released `--tls-cert` would leave it
	/// serving plaintext rather than reaching the builder that refuses.
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		match self {
			Self::Import(import) => import.deprecated(),
			Self::Publish(import) => {
				let mut found = import.deprecated();
				found.flag("publish", None, "import");
				found
			}
			Self::Export(export) => export.deprecated(),
			Self::Subscribe(export) => {
				let mut found = export.deprecated();
				found.flag("subscribe", None, "export");
				found
			}
			_ => moq_tokio::cli::Deprecated::default(),
		}
	}

	/// Rewrite a released verb to the current one. The migration is recorded by
	/// [`Self::deprecated`] before this runs.
	fn current(self) -> Self {
		match self {
			Self::Publish(import) => Self::Import(import),
			Self::Subscribe(export) => Self::Export(export),
			other => other,
		}
	}

	/// The verb as typed, for error messages.
	pub fn name(&self) -> &'static str {
		match self {
			Self::Import(_) | Self::Publish(_) => "import",
			Self::Export(_) | Self::Subscribe(_) => "export",
			Self::Announced(_) => "announced",
			Self::Fetch(_) => "fetch",
			#[cfg(feature = "play")]
			Self::Play(_) => "play",
			#[cfg(feature = "transcode")]
			Self::Transcode(_) => "transcode",
			Self::Auth(_) => "auth",
			Self::Completion(_) => "completion",
			#[cfg(feature = "capture")]
			Self::Devices => "devices",
		}
	}

	/// Whether this verb can share a process (and an Origin) with other stages.
	pub fn is_stageable(&self) -> bool {
		matches!(
			self,
			Self::Import(_) | Self::Export(_) | Self::Publish(_) | Self::Subscribe(_)
		)
	}

	/// The broadcast this stage names, falling back to the process-wide `--broadcast`.
	///
	/// Empty means the root broadcast: MoQ names each broadcast by the connection
	/// path plus any explicit `--broadcast`, so an unset name is the connection path
	/// itself.
	pub fn broadcast(&self, moq: &MoqSide) -> String {
		let stage = match self {
			Self::Import(import) | Self::Publish(import) => import.broadcast.as_deref(),
			Self::Export(export) | Self::Subscribe(export) => export.broadcast.as_deref(),
			_ => None,
		};

		stage.or(moq.broadcast.as_deref()).unwrap_or_default().to_string()
	}
}

// ------------------------------------------------------------------ import

/// import = one source -> MoQ.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Import {
	/// The broadcast this stage publishes, overriding the process-wide `--broadcast`.
	///
	/// Required when a process imports more than one broadcast; a single stage can
	/// keep naming it before the verb.
	#[usage(long)]
	pub broadcast: Option<String>,

	/// The released spelling of [`Self::broadcast`].
	#[usage(long = "name", hide = true)]
	name: Option<String>,

	/// How long relays keep a non-latest group of the published media tracks fetchable,
	/// e.g. "30s" or "5s". Defaults to hang's 30s.
	///
	/// A RETENTION budget, not a delivery one: it never makes a subscriber play further behind
	/// live, it caps how far back a FETCH can still reach (and how long a subscriber may ask to
	/// wait for a late group). The default suits a segmented egress (HLS/DASH), which may only
	/// advertise segments that are still fetchable; lower it when nothing reads history and the
	/// memory matters. Media tracks only -- the catalog and HLS records are read at the live
	/// edge, which is retained unconditionally.
	#[usage(long)]
	pub max_age: Option<moq_tokio::cli::Duration>,

	/// How long a cleanly ended broadcast stays up, counted from its end, e.g. "30s"; "0s"
	/// closes it at once. Defaults to 30s.
	///
	/// Subscribers already reading get the tail either way; this serves one that arrives after
	/// the end, such as a player that read the final catalog late. A clean end is stdin EOF, an
	/// RTMP unpublish or completed play, the end of an SRT stream, a WHIP `DELETE`, or an HLS
	/// playlist's `EXT-X-ENDLIST`. A failure, and Ctrl-C, close the broadcast at once.
	#[usage(long)]
	pub linger: Option<moq_tokio::cli::Duration>,

	/// The released spelling of [`Self::max_age`].
	#[usage(long = "latency-max", hide = true)]
	latency_max: Option<moq_tokio::cli::Duration>,

	/// Name a media report track in the catalog's `stats` section and fill it with what each
	/// track has written, about once a second while subscribed. The stdin imports only.
	#[usage(long)]
	pub report: bool,

	/// Solicit viewer feedback in the catalog: `export --echo <NAME>` publishes `<NAME>.echo`
	/// under this prefix, resolved against the broadcast like a relative URL (`viewers` from
	/// `room/event` is `room/viewers`). The stdin imports only.
	#[usage(long, value_name = "PATH")]
	pub echo_path: Option<String>,

	/// The single source feeding the Origin.
	#[usage(subcommand)]
	pub source: ImportSource,
}

impl Import {
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.name.is_some() {
			found.flag("--name", None, "--broadcast");
		}
		if self.latency_max.is_some() {
			found.flag("--latency-max", None, "--max-age");
		}
		#[cfg(feature = "capture")]
		if let ImportSource::Capture(capture) = &self.source {
			found.extend(capture.deprecated());
		}
		found
	}
}

/// The single source feeding the Origin on an import. The container formats read
/// from stdin; the gateways bridge another protocol.
#[derive(usage_rs::Subcommands, Clone)]
pub enum ImportSource {
	/// Raw H.264 Annex-B from stdin.
	Avc3,
	/// Fragmented MP4 / CMAF from stdin.
	Fmp4(Fmp4Import),
	/// MPEG-TS from stdin.
	Ts(TsImport),
	/// FLV / RTMP container from stdin.
	Flv,
	/// Follow a remote live HLS / LL-HLS playlist (http/https URL or local file) into MoQ.
	Hls(crate::hls::ImportArgs),
	/// RTMP: pull a remote play (`--connect`) or accept incoming publishes (`--listen`).
	Rtmp(crate::rtmp::Args),
	/// SRT: pull a remote stream (`--connect`) or accept incoming publishes (`--listen`).
	Srt(crate::srt::ImportArgs),
	/// RTSP: pull an IP camera's stream (`--connect`), reconnecting on failure.
	Rtsp(crate::rtsp::ImportArgs),
	/// WebRTC: WHEP client pulling a remote (`--connect`) or WHIP server accepting publishes (`--listen`).
	Rtc(crate::rtc::Args),
	/// Replay a recording from an object store, serving its groups on demand.
	Archive(crate::archive::ImportArgs),
	/// Capture a local source (camera, display, window, app, microphone) and
	/// encode natively. Run `moq devices` to list them.
	#[cfg(feature = "capture")]
	Capture(crate::publish::CaptureArgs),
}

impl ImportSource {
	/// The stdin container format, when this source is one of the container formats.
	pub fn stdin_format(&self) -> Option<PublishFormat> {
		Some(match self {
			Self::Avc3 => PublishFormat::Avc3,
			Self::Fmp4(args) => PublishFormat::Fmp4 {
				passthrough: args.passthrough,
			},
			Self::Ts(args) if args.passthrough => PublishFormat::TsPassthrough {
				pcr_pid: args.pcr_pid.map(|pid| pid.0),
			},
			Self::Ts(args) => PublishFormat::Ts {
				program: args.program.and_then(TsProgram::number),
			},
			Self::Flv => PublishFormat::Flv,
			_ => return None,
		})
	}

	/// Why the CLI can't add a section to this source's first catalog, or `None` when it can:
	/// a stdin import's one catalog waits for its importer's tracks.
	fn catalog_refusal(&self) -> Option<&'static str> {
		match self {
			Self::Ts(args) if args.program == Some(TsProgram::All) => {
				Some("`import ts --program all` builds a catalog per program")
			}
			Self::Hls(_) | Self::Rtmp(_) | Self::Srt(_) | Self::Rtsp(_) | Self::Rtc(_) | Self::Archive(_) => {
				Some("the gateway and archive imports build their own catalogs")
			}
			#[cfg(feature = "capture")]
			Self::Capture(_) => Some("`import capture` publishes its catalog as its encoders start"),
			Self::Avc3 | Self::Fmp4(_) | Self::Ts(_) | Self::Flv => None,
		}
	}

	/// Why `--report` can't report this source, or `None` when it can: the CLI builds the one
	/// catalog of a stdin import, whose tracks `moq-mux` counts.
	fn report_refusal(&self) -> Option<&'static str> {
		#[cfg(feature = "capture")]
		if matches!(self, Self::Capture(_)) {
			return Some("`import capture` encodes, and its encoders do not count yet");
		}
		self.catalog_refusal()
	}

	/// Why `--linger` can't apply to this source, or `None` when it can: only a source with a
	/// clean end has a finished broadcast to keep up.
	fn linger_refusal(&self) -> Option<&'static str> {
		match self {
			// Every run is followed by a redial publishing the next one, so a run's end is not
			// the end of the content, and its finished catalog would tell readers to stop.
			Self::Rtsp(_) => Some("`import rtsp` redials after every session, so a run's end is not the content's"),
			Self::Rtc(rtc) if rtc.connect.is_some() => Some("a WHEP pull has no clean end"),
			Self::Archive(_) => Some("`import archive` serves its recording until stopped"),
			#[cfg(feature = "capture")]
			Self::Capture(_) => Some("`import capture` has no clean end"),
			Self::Avc3
			| Self::Fmp4(_)
			| Self::Ts(_)
			| Self::Flv
			| Self::Hls(_)
			| Self::Rtmp(_)
			| Self::Srt(_)
			| Self::Rtc(_) => None,
		}
	}
}

/// The fMP4 stdin container: whether to unwrap its fragments.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Fmp4Import {
	/// Publish each fragment whole as CMAF instead of splitting it into one frame per sample.
	/// Keeps the source's exact sample timing, but every player has to parse ISO BMFF.
	#[usage(long = "cmaf")]
	pub passthrough: bool,
}

/// The MPEG-TS stdin container: which programs of a multiplex to publish.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct TsImport {
	/// Import one program of a multi-program stream, by its PAT program number, or `all` to
	/// publish each program as its own broadcast (`event.hang` becomes `event/1.hang`,
	/// `event/2.hang`, ...). Without it, a stream carrying more than one program is refused.
	#[usage(long)]
	pub program: Option<TsProgram>,

	/// Publish the multiplex whole instead of demultiplexing it: every 188-byte packet verbatim,
	/// on one track the catalog's `m2ts` section names. Scrambled streams, private PIDs, and the
	/// PSI and SI ride through as authored. Bytes before the first group are dropped.
	#[usage(long, conflicts = "--program")]
	pub passthrough: bool,

	/// Pace `--passthrough` on this PID's PCR rather than the `PCR_PID` of the PAT's first
	/// program. Decimal, or hex with `0x`.
	#[usage(long, requires = "--passthrough")]
	pub pcr_pid: Option<TsPid>,
}

/// An `import ts --pcr-pid` value: a PID that can carry a PCR, in decimal or `0x` hex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TsPid(pub u16);

impl std::str::FromStr for TsPid {
	type Err = String;

	fn from_str(arg: &str) -> Result<Self, Self::Err> {
		let parsed = match arg.strip_prefix("0x").or_else(|| arg.strip_prefix("0X")) {
			Some(hex) => u16::from_str_radix(hex, 16),
			None => arg.parse(),
		};
		match parsed {
			Ok(pid @ 0x0001..=0x1ffe) => Ok(Self(pid)),
			Ok(pid) => Err(format!("PID {pid:#06x} cannot carry a PCR; expected 0x0001..=0x1ffe")),
			Err(_) => Err(format!("expected a PID, got `{arg}`")),
		}
	}
}

/// An `import ts --program` or `import srt --program` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TsProgram {
	/// The program with this PAT program number.
	One(u16),
	/// Every program, each as its own broadcast.
	All,
}

impl TsProgram {
	/// The single program selected, `None` for `all`.
	fn number(self) -> Option<u16> {
		match self {
			Self::One(program) => Some(program),
			Self::All => None,
		}
	}
}

impl std::str::FromStr for TsProgram {
	type Err = String;

	fn from_str(arg: &str) -> Result<Self, Self::Err> {
		match arg {
			"all" => Ok(Self::All),
			_ => match arg.parse() {
				Ok(0) => Err("program 0 is the network PID, not a program".to_string()),
				Ok(program) => Ok(Self::One(program)),
				Err(_) => Err(format!("expected a program number or `all`, got `{arg}`")),
			},
		}
	}
}

// ------------------------------------------------------------------ export

/// export = MoQ -> one sink.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Export {
	/// The broadcast this stage subscribes to, overriding the process-wide `--broadcast`.
	///
	/// Required when a process exports more than one broadcast; a single stage can
	/// keep naming it before the verb.
	#[usage(long)]
	pub broadcast: Option<String>,

	/// The released spelling of [`Self::broadcast`].
	#[usage(long = "name", hide = true)]
	name: Option<String>,

	/// Catalog format to read for track discovery (default: detect from the broadcast suffix).
	#[usage(long = "catalog-format", value_enum)]
	pub catalog_format: Option<CatalogFormatArg>,

	/// Rendition selection (`--video-name`, `--audio off`, ...), refused by sinks that don't apply it.
	#[usage(flatten)]
	pub select: crate::subscribe::SelectArgs,

	/// Publish feedback where the broadcast's catalog solicits it: a `<NAME>.echo` broadcast
	/// under the catalog's `echo` path, reporting what each rendition this export reads has
	/// received. One path segment, not starting with `.`. The stdout container exports only.
	#[usage(long, value_name = "NAME")]
	pub echo: Option<crate::report::EchoName>,

	/// The single sink draining the Origin.
	#[usage(subcommand)]
	pub sink: ExportSink,
}

impl Export {
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.name.is_some() {
			found.flag("--name", None, "--broadcast");
		}
		match &self.sink {
			ExportSink::Fmp4(args) => found.extend(args.container.deprecated()),
			ExportSink::Mkv(args) => found.extend(args.container.deprecated()),
			ExportSink::Ts(args) => found.extend(args.deprecated()),
			ExportSink::Flv(args) => found.extend(args.deprecated()),
			ExportSink::H264(args) | ExportSink::H265(args) => found.extend(args.deprecated()),
			ExportSink::Rtmp(rtmp) => found.extend(rtmp.deprecated()),
			ExportSink::Hls(hls) => found.extend(hls.tls.deprecated()),
			_ => {}
		}
		found
	}
}

/// The single sink draining the Origin on an export. The container formats write
/// to stdout; the gateways bridge another protocol.
#[derive(usage_rs::Subcommands, Clone)]
pub enum ExportSink {
	/// Fragmented MP4 / CMAF to stdout.
	Fmp4(Fragmented),
	/// Matroska / WebM to stdout.
	Mkv(BufferedFragmented),
	/// MPEG-TS to stdout.
	Ts(Transport),
	/// FLV / RTMP container to stdout.
	Flv(Buffered),
	/// H.264 Annex-B elementary stream to stdout.
	H264(Container),
	/// H.265 Annex-B elementary stream to stdout.
	H265(Container),
	/// Serve HLS / LL-HLS and DASH over HTTP.
	Hls(crate::hls::ExportArgs),
	/// RTMP: push to a remote (`--connect`) or serve plays (`--listen`).
	Rtmp(crate::rtmp::ExportArgs),
	/// SRT: push to a remote (`--connect`) or serve requests (`--listen`).
	Srt(crate::srt::ExportArgs),
	/// WebRTC: WHIP client pushing to a remote (`--connect`) or WHEP server serving plays (`--listen`).
	Rtc(crate::rtc::Args),
	/// Record every broadcast announced under `--broadcast` into an object store.
	Archive(crate::archive::ExportArgs),
	/// Write the broadcast's media reports (`import --report`) to stdout as JSON lines.
	Report,
	/// Write the feedback of every `.echo` broadcast under `--broadcast` to stdout as JSON lines.
	Echo,
}

impl ExportSink {
	/// The sink's name when it doesn't apply selection, so the selection flags would be ignored.
	fn ignores_selection(&self) -> Option<&'static str> {
		Some(match self {
			Self::Fmp4(_) | Self::Mkv(_) | Self::Flv(_) | Self::H264(_) | Self::H265(_) => return None,
			Self::Ts(_) => "ts",
			Self::Hls(_) => "hls",
			Self::Rtmp(_) => "rtmp",
			Self::Srt(_) => "srt",
			Self::Rtc(_) => "rtc",
			Self::Archive(_) => "archive",
			Self::Report => "report",
			Self::Echo => "echo",
		})
	}

	/// The report sink's name: these read no media, so they skip catalog discovery.
	fn report(&self) -> Option<&'static str> {
		match self {
			Self::Report => Some("report"),
			Self::Echo => Some("echo"),
			_ => None,
		}
	}

	/// Whether this sink writes to stdout (the container formats and the report sinks).
	pub fn is_stdout(&self) -> bool {
		self.stdout().is_some() || self.report().is_some()
	}

	/// The stdout container format and its options, when this sink writes to
	/// stdout. The fragment cap is fmp4/mkv-only and the mux rate is TS-only.
	pub fn stdout(&self) -> Option<Stdout> {
		let container = |format, container: &Container| Stdout {
			format,
			max_delay: container.max_delay.into(),
			follow: container.linger.into(),
			stitch: false,
			fragment_duration: None,
			mux_rate: None,
			passthrough: false,
		};
		let buffered = |format, args: &Buffered| Stdout {
			format,
			max_delay: args.delay.into(),
			follow: args.linger.into(),
			stitch: false,
			fragment_duration: None,
			mux_rate: None,
			passthrough: false,
		};
		Some(match self {
			Self::Fmp4(args) => Stdout {
				fragment_duration: args.fragment_duration.map(Into::into),
				..container(SubscribeFormat::Fmp4, &args.container)
			},
			Self::Mkv(args) => Stdout {
				fragment_duration: args.fragment_duration.map(Into::into),
				..buffered(SubscribeFormat::Mkv, &args.container)
			},
			Self::Ts(args) => Stdout {
				format: SubscribeFormat::Ts,
				max_delay: args.delay.into(),
				follow: args.follow.into(),
				stitch: args.stitch,
				fragment_duration: None,
				mux_rate: args.mux_rate,
				passthrough: args.passthrough,
			},
			Self::Flv(args) => buffered(SubscribeFormat::Flv, args),
			Self::H264(args) => container(SubscribeFormat::H264, args),
			Self::H265(args) => container(SubscribeFormat::H265, args),
			_ => return None,
		})
	}
}

/// A stdout sink's format and the options that apply to it.
pub struct Stdout {
	pub format: SubscribeFormat,
	/// The staleness budget, also the FLV/MKV release delay and how far behind the source `ts`
	/// runs (`--delay`).
	pub max_delay: Duration,
	/// How long to wait for the broadcast to come back (`ts` only; the others refuse their
	/// hidden `--linger`).
	pub follow: Duration,
	/// Follow a replacement as a program switch (`ts` only).
	pub stitch: bool,
	pub fragment_duration: Option<Duration>,
	pub mux_rate: Option<u64>,
	/// Write the `m2ts` passthrough track back out (`ts` only).
	pub passthrough: bool,
}

/// Staleness options for stdout sinks without a fixed release delay.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Container {
	/// How far a group may fall behind the live edge before it is skipped (e.g. `500ms`, `1s`).
	#[usage(long, default = "500ms")]
	pub max_delay: moq_tokio::cli::Duration,

	/// The released spelling of `export ts --follow`, accepted only to refuse a nonzero value:
	/// only TS can mark where a returned broadcast restarts.
	#[usage(long, default = "0s", hide = true)]
	pub linger: moq_tokio::cli::Duration,

	/// The released spelling of [`Self::max_delay`].
	#[usage(long = "max-age", hide = true)]
	max_age: Option<moq_tokio::cli::Duration>,

	/// The released spelling of [`Self::max_delay`], before `--max-age`.
	#[usage(long = "latency-max", hide = true)]
	latency_max: Option<moq_tokio::cli::Duration>,
}

impl Container {
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.max_age.is_some() {
			found.flag("--max-age", None, "--max-delay");
		}
		if self.latency_max.is_some() {
			found.flag("--latency-max", None, "--max-delay");
		}
		found
	}
}

/// A stdout container with a fixed release delay.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Buffered {
	/// How long after its decode time each frame is written (e.g. `500ms`); late frames are dropped.
	/// The same delay bounds source staleness. Zero writes each frame as soon as it is read and
	/// drops none as late, but still skips a stalled group once a newer one is ready.
	#[usage(long, default = "500ms")]
	pub delay: moq_tokio::cli::Duration,
	/// The released spelling of `export ts --follow`, accepted only to refuse a nonzero value:
	/// only TS can mark a broadcast restart.
	#[usage(long, default = "0s", hide = true)]
	pub linger: moq_tokio::cli::Duration,
	/// An older spelling of the staleness setting.
	#[usage(long = "max-age", hide = true)]
	max_age: Option<moq_tokio::cli::Duration>,
	/// An older spelling of the staleness setting.
	#[usage(long = "latency-max", hide = true)]
	latency_max: Option<moq_tokio::cli::Duration>,
}

impl Buffered {
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		for (present, flag) in [
			(self.max_age.is_some(), "--max-age"),
			(self.latency_max.is_some(), "--latency-max"),
		] {
			if present {
				found.flag(flag, None, "--delay");
			}
		}
		found
	}
}

/// A buffered Matroska output with an optional cluster cap.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct BufferedFragmented {
	#[usage(flatten)]
	pub container: Buffered,
	/// Cap the output cluster duration (e.g. `2s`); defaults to video GOPs.
	#[usage(long)]
	pub fragment_duration: Option<moq_tokio::cli::Duration>,
}

/// The MPEG-TS stdout container.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Transport {
	/// How far behind the source the output runs (e.g. `2s`), like an SRT receiver's latency,
	/// including how far ahead of its decode time each frame is sent. A frame that arrives
	/// later than this is dropped, and a stalled group is skipped once it falls half this
	/// behind the newest content.
	#[usage(long, default = "1s")]
	pub delay: moq_tokio::cli::Duration,

	/// How long to wait for the same publisher instance, or with `--stitch` a replacement, to
	/// come back once the broadcast ends (e.g. `10s`), then carry on with the same stream.
	/// Zero follows only an instance already serving. A replacement exits 1 unless `--stitch` is
	/// passed. A failure other than the publisher leaving exits at once.
	#[usage(long, default = "0s")]
	pub follow: moq_tokio::cli::Duration,

	/// Follow another publisher instance that replaces the broadcast, as a full program switch:
	/// a new PMT from its catalog, flagged as a break on every PID.
	#[usage(long)]
	pub stitch: bool,

	/// Pad the output with null packets to this constant rate, in bits per second.
	/// Defaults to the multiplex rate the catalog recorded from a constant-rate
	/// source (`mpegts.muxRate`); without either the output is unpadded.
	#[usage(long)]
	pub mux_rate: Option<u64>,

	/// Write the multiplex an `import ts --passthrough` published back out byte-identical,
	/// paced on its own PCR, `--delay` behind it. Packets that arrive later than that are
	/// dropped, leaving a continuity error.
	#[usage(long, conflicts("--follow", "--stitch", "--mux-rate"))]
	pub passthrough: bool,

	/// The released spelling of [`Self::follow`].
	#[usage(long = "linger", hide = true)]
	linger: Option<moq_tokio::cli::Duration>,

	/// The released spelling of [`Self::delay`].
	#[usage(long = "max-age", hide = true)]
	max_age: Option<moq_tokio::cli::Duration>,

	/// The released spelling of [`Self::delay`], before `--max-age`.
	#[usage(long = "latency-max", hide = true)]
	latency_max: Option<moq_tokio::cli::Duration>,
}

impl Transport {
	fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		const BEHIND: &str = "it also sets how far behind the source the output runs";
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.max_age.is_some() {
			found.changed("--max-age", None, "--delay", BEHIND);
		}
		if self.latency_max.is_some() {
			found.changed("--latency-max", None, "--delay", BEHIND);
		}
		if self.linger.is_some() {
			found.flag("--linger", None, "--follow");
		}
		found
	}
}

/// The fMP4 stdout container with a fragment cap.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Fragmented {
	#[usage(flatten)]
	pub container: Container,

	/// Cap the output fragment/cluster duration (e.g. `2s`).
	/// Defaults to publisher groups.
	#[usage(long)]
	pub fragment_duration: Option<moq_tokio::cli::Duration>,
}

#[cfg(test)]
mod tests {
	use super::*;

	// Materializing the spec catches invalid relationships, duplicate selectors,
	// and flattened arguments colliding with an existing one.
	// The token verb flattens a whole command tree from another crate, so this is
	// the only thing standing between a rename there and a broken `moq`.
	#[test]
	fn valid() {
		let _ = Cli::to_kdl();
	}

	/// The `Stage` parser is a second entry point into the same command tree, so it
	/// needs the same conflict check as [`Cli`].
	#[test]
	fn valid_stage() {
		let _ = Stage::to_kdl();
	}

	/// Only TS can mark where a returned broadcast restarts, so only `export ts` follows one.
	/// The released `--linger` points there, as a rename on `ts` and a refusal elsewhere.
	#[test]
	fn follow_is_ts_only() {
		let parse = |format: &str, flag: &str, value: &str| {
			Invocation::try_parse_from(["moq", "--connect", "http://relay", "export", format, flag, value])
		};
		assert!(parse("ts", "--follow", "10s").unwrap().validate().is_ok());
		let Err(err) = parse("ts", "--linger", "10s") else {
			panic!("the released spelling must not start a run");
		};
		assert!(err.to_string().contains("--linger -> --follow"), "{err}");
		for format in ["fmp4", "mkv", "flv", "h264", "h265"] {
			assert!(parse(format, "--follow", "10s").is_err(), "{format} takes no --follow");
			let err = parse(format, "--linger", "10s")
				.unwrap()
				.validate()
				.unwrap_err()
				.to_string();
			assert!(err.contains("export ts --follow"), "{format}: {err}");
			assert!(
				parse(format, "--linger", "0s").unwrap().validate().is_ok(),
				"{format}: a zero --linger is always fine"
			);
		}
	}

	/// `export ts` holds every frame for its staleness budget, so the budget is spelled
	/// `--delay` there, and the released spellings stop a run with a migration.
	#[test]
	fn export_ts_takes_a_delay() {
		let parse = |args: &[&str]| {
			let mut argv = vec!["moq", "export", "--broadcast", "b", "ts"];
			argv.extend_from_slice(args);
			Invocation::try_parse_from(argv)
		};
		let stdout = |args: &[&str]| {
			let cli = parse(args).unwrap();
			let Command::Export(export) = &cli.stages[0] else {
				panic!("not an export");
			};
			export.sink.stdout().unwrap().max_delay
		};
		assert_eq!(stdout(&[]), Duration::from_secs(1));
		assert_eq!(stdout(&["--delay", "2s"]), Duration::from_secs(2));
		for old in ["--max-age", "--latency-max"] {
			let Err(err) = parse(&[old, "1s"]) else {
				panic!("{old} must not start a run");
			};
			assert!(err.to_string().contains(&format!("{old} -> --delay")), "{err}");
		}
	}

	#[test]
	fn hls_tls_environment_is_independent_of_the_moq_listener() {
		if !test_env::Env::default()
			.set("MOQ_LISTEN_TLS_CERT", "moq.pem")
			.set("MOQ_HLS_TLS_CERT", "hls.pem")
			.set("MOQ_HLS_TLS_KEY", "hls.key")
			.set("MOQ_HLS_TLS_ROOT", "root.pem")
			.isolate()
		{
			return;
		}
		let cli = Invocation::try_parse_from(["moq", "export", "hls"]).unwrap();
		assert_eq!(cli.moq.server.tls.cert, [std::path::PathBuf::from("moq.pem")]);
		let Command::Export(export) = &cli.stages[0] else {
			panic!("expected export")
		};
		let ExportSink::Hls(hls) = &export.sink else {
			panic!("expected HLS")
		};
		assert_eq!(hls.tls.cert, [std::path::PathBuf::from("hls.pem")]);
		assert_eq!(hls.tls.key, [std::path::PathBuf::from("hls.key")]);
		assert_eq!(hls.tls.root, [std::path::PathBuf::from("root.pem")]);
	}

	macro_rules! released_environment {
		($test:ident, $old:literal, $replacement:literal, $value:literal) => {
			#[test]
			fn $test() {
				if !test_env::Env::default().set($old, $value).isolate() {
					return;
				}
				let err = Invocation::try_parse_from(["moq", "import", "ts"])
					.err()
					.expect("released environment must not be ignored");
				assert!(
					err.to_string().contains($old) && err.to_string().contains($replacement),
					"{err}"
				);
			}
		};
	}

	released_environment!(
		released_backoff_initial_env,
		"MOQ_BACKOFF_INITIAL",
		"MOQ_CONNECT_BACKOFF_INITIAL",
		"2s"
	);
	released_environment!(
		released_backoff_multiplier_env,
		"MOQ_BACKOFF_MULTIPLIER",
		"MOQ_CONNECT_BACKOFF_MULTIPLIER",
		"3"
	);
	released_environment!(
		released_backoff_max_env,
		"MOQ_BACKOFF_MAX",
		"MOQ_CONNECT_BACKOFF_MAX",
		"2s"
	);
	released_environment!(
		released_backoff_timeout_env,
		"MOQ_BACKOFF_TIMEOUT",
		"MOQ_CONNECT_BACKOFF_TIMEOUT",
		"2s"
	);
	released_environment!(
		released_goaway_redirect_env,
		"MOQ_GOAWAY_REDIRECT",
		"MOQ_CONNECT_GOAWAY_REDIRECT",
		"ignore"
	);
	released_environment!(
		released_goaway_handover_env,
		"MOQ_GOAWAY_HANDOVER",
		"MOQ_CONNECT_GOAWAY_HANDOVER",
		"2s"
	);

	#[test]
	fn connection_prefixes_refuse_released_spellings() {
		for (family, names) in [
			("backoff", &["initial", "multiplier", "max", "timeout"][..]),
			("goaway", &["redirect", "handover"][..]),
		] {
			for name in names {
				let value = match *name {
					"multiplier" => "3",
					"redirect" => "ignore",
					_ => "2s",
				};
				let current = format!("--connect-{family}-{name}");
				Invocation::try_parse_from(["moq", current.as_str(), value, "import", "ts"]).unwrap();
				let old = format!("--{family}-{name}");
				let err = Invocation::try_parse_from(["moq", old.as_str(), value, "import", "ts"])
					.err()
					.expect("released spelling refused");
				assert!(
					err.to_string().contains(&old) && err.to_string().contains(&current),
					"{err}"
				);
			}
		}
	}

	#[test]
	fn audited_flags_parse_the_consumer_policy() {
		for format in ["fmp4", "hls"] {
			let mut argv = vec!["moq", "import", format];
			if format == "hls" {
				argv.push("input.m3u8");
			}
			argv.push("--cmaf");
			let cli = Invocation::try_parse_from(argv).expect("explicit CMAF publishing");
			let Command::Import(import) = &cli.stages[0] else {
				panic!("expected import")
			};
			match &import.source {
				ImportSource::Fmp4(args) => assert!(args.passthrough),
				ImportSource::Hls(args) => assert!(args.passthrough),
				_ => panic!("expected fragment input"),
			}
		}
		Invocation::try_parse_from([
			"moq",
			"export",
			"rtmp",
			"--connect",
			"rtmp://localhost/live/key",
			"--delay",
			"2s",
		])
		.expect("RTMP paced release");
	}

	#[test]
	fn unpublished_spellings_have_no_migration_shim() {
		for format in ["fmp4", "hls"] {
			let mut argv = vec!["moq", "import", format];
			if format == "hls" {
				argv.push("input.m3u8");
			}
			argv.push("--passthrough");
			let err = Invocation::try_parse_from(argv).err().expect("CMAF is explicit");
			assert_eq!(err.kind(), ParseErrorKind::UnknownArgument);
		}
		for format in ["flv", "mkv", "rtmp"] {
			let mut argv = vec!["moq", "export", format, "--max-delay", "2s"];
			if format == "rtmp" {
				argv.extend(["--connect", "rtmp://localhost/live/key"]);
			}
			let err = Invocation::try_parse_from(argv)
				.err()
				.expect("unreleased spelling is unknown");
			assert_eq!(err.kind(), ParseErrorKind::UnknownArgument);
		}
	}

	/// `export ts --passthrough` writes the source's own packets, so it neither switches
	/// programs, waits for a return, nor pads to a rate.
	#[test]
	fn export_ts_passthrough_writes_the_source_alone() {
		let parse = |args: &[&str]| {
			let mut argv = vec!["moq", "export", "--broadcast", "b", "ts", "--passthrough"];
			argv.extend_from_slice(args);
			Invocation::try_parse_from(argv)
		};
		let cli = parse(&["--delay", "2s"]).unwrap();
		let Command::Export(export) = &cli.stages[0] else {
			panic!("not an export");
		};
		let stdout = export.sink.stdout().unwrap();
		assert!(stdout.passthrough);
		assert_eq!(stdout.max_delay, Duration::from_secs(2));
		for flags in [&["--stitch"][..], &["--follow", "10s"], &["--mux-rate", "20000000"]] {
			assert!(parse(flags).is_err(), "{flags:?} must not start a run");
		}
	}

	fn export(flags: &[&str]) -> Result<Invocation, ParseError> {
		let argv = ["moq", "--connect", "http://relay", "--broadcast", "room", "export"];
		Invocation::try_parse_from(argv.iter().chain(flags).copied())
	}

	/// Capture policy values cannot leak into rendition selection or unsupported commands.
	#[test]
	fn media_modes_are_command_specific() {
		// Each command and the arguments that follow its flags: export's selection flags
		// come before the sink.
		let mut commands = vec![(vec!["export"], vec!["fmp4"])];
		if cfg!(feature = "play") {
			commands.push((vec!["play"], vec![]));
		}
		if cfg!(feature = "capture") {
			commands.push((vec!["import", "capture"], vec![]));
		}
		for (command, rest) in commands {
			let argv = |flags: &[&'static str]| [vec!["moq"], command.clone(), flags.to_vec(), rest.clone()].concat();
			for flag in ["--video", "--audio"] {
				for mode in ["on", "off", "demand", "buffered", "always", "true"] {
					let argv = argv(&[flag, mode]);
					let supported =
						matches!(mode, "on" | "off") || (command == ["import", "capture"] && mode == "demand");
					assert_eq!(Invocation::try_parse_from(argv.clone()).is_ok(), supported, "{argv:?}");
				}
				let argv = argv(&[flag]);
				assert!(
					Invocation::try_parse_from(argv.clone()).is_err(),
					"{argv:?} needs a value"
				);
			}
			for flag in ["--no-video", "--no-audio", "--on-demand"] {
				let argv = argv(&[flag]);
				assert!(
					Invocation::try_parse_from(argv.clone()).is_err(),
					"{argv:?} was removed"
				);
			}
		}
	}

	/// Capture's released `--no-*` spellings name their replacement instead of an unknown flag.
	#[cfg(feature = "capture")]
	#[test]
	fn capture_names_the_released_spellings() {
		for (old, new) in [("--no-video", "--video off"), ("--no-audio", "--audio off")] {
			let Err(err) = Invocation::try_parse_from(["moq", "import", "capture", old]) else {
				panic!("{old} parsed");
			};
			let err = err.to_string();
			assert!(err.contains(&format!("{old} -> {new}")), "{err}");
		}
	}

	/// Explicit on still permits narrowing a role, and omission still keeps both roles.
	#[test]
	fn enabled_media_keep_their_rendition_filters() {
		for flags in [
			["fmp4"].as_slice(),
			&[
				"--video",
				"on",
				"--video-name",
				"hd",
				"--video-codec",
				"h264",
				"--audio",
				"on",
				"--audio-name",
				"stereo",
				"--audio-codec",
				"opus",
				"fmp4",
			],
		] {
			let cli = export(flags).unwrap();
			cli.validate().unwrap();
			let Command::Export(export) = &cli.stages[0] else {
				panic!("an export stage");
			};
			let selection = export.select.selection(None);
			assert!(selection.has_video());
			assert!(selection.has_audio());
		}
	}

	/// Leaving a role out can't be combined with narrowing it, or with leaving the
	/// other role out too.
	#[test]
	fn leaving_a_role_out_conflicts_with_selecting_it() {
		for flags in [
			["--video", "off", "--video-name", "hd"].as_slice(),
			&["--video-codec", "h264", "--video", "off"],
			&["--audio", "off", "--audio-name", "stereo"],
			&["--audio-codec", "opus", "--audio", "off"],
			&["--video", "off", "--audio", "off"],
			&["--audio", "off", "--video", "off"],
		] {
			let flags = [flags, &["fmp4"]].concat();
			assert!(export(&flags).unwrap().validate().is_err(), "{flags:?} must be refused");
		}
	}

	#[test]
	fn audio_off_selects_no_audio_role() {
		let cli = export(&["--audio", "off", "fmp4"]).unwrap();
		cli.validate().unwrap();
		let Command::Export(export) = &cli.stages[0] else {
			panic!("an export stage");
		};
		let selection = export.select.selection(None);
		assert!(selection.has_video());
		assert!(!selection.has_audio());
	}

	/// A video elementary stream refuses leaving video out or selecting audio, before dialing.
	#[test]
	fn elementary_streams_refuse_video_off() {
		for format in ["h264", "h265"] {
			let err = export(&["--video", "off", format]).unwrap().validate().unwrap_err();
			assert!(err.to_string().contains("--video off"), "{format}: {err}");
			export(&["--audio", "off", format]).unwrap().validate().unwrap();
			for flag in [["--audio-name", "stereo"], ["--audio-codec", "aac"], ["--audio", "on"]] {
				let err = export(&[flag.as_slice(), &[format]].concat())
					.unwrap()
					.validate()
					.unwrap_err();
				assert!(err.to_string().contains(flag[0]), "{format} {flag:?}: {err}");
			}
		}
		for format in ["fmp4", "mkv", "flv"] {
			export(&["--video", "off", format]).unwrap().validate().unwrap();
		}
	}

	/// A sink that doesn't apply selection refuses the selection flags rather than
	/// ignoring them.
	#[test]
	fn sinks_without_selection_refuse_it() {
		let sinks = [
			["ts"].as_slice(),
			&["hls", "--listen", "127.0.0.1:8080"],
			&["rtmp", "--listen", "127.0.0.1:1935"],
			&["srt", "--listen", "127.0.0.1:9000"],
			&["rtc", "--listen", "127.0.0.1:8443"],
			&["archive", "file:///tmp/archive"],
		];
		for sink in sinks {
			export(sink).unwrap().validate().unwrap();
			for flag in [
				["--video-name", "hd"].as_slice(),
				&["--video-codec", "h264"],
				&["--video", "off"],
				&["--video", "on"],
				&["--audio-name", "stereo"],
				&["--audio-codec", "aac"],
				&["--audio", "off"],
				&["--audio", "on"],
			] {
				let err = export(&[flag, sink].concat()).unwrap().validate().unwrap_err();
				assert!(err.to_string().contains(flag[0]), "{sink:?} {flag:?}: {err}");
			}
		}
	}

	/// `--report` and `--echo-path` go where the CLI builds the one catalog they add a section to.
	#[test]
	fn catalog_sections_need_a_catalog_the_cli_builds() {
		let import = |flags: &[&str], source: &[&str]| {
			let argv = ["moq", "--connect", "http://relay", "--broadcast", "room", "import"];
			Invocation::try_parse_from(argv.iter().chain(flags).chain(source).copied()).unwrap()
		};
		for flags in [["--report"].as_slice(), &["--echo-path", "viewers"]] {
			for source in [
				["ts"].as_slice(),
				&["fmp4"],
				&["flv"],
				&["avc3"],
				&["ts", "--program", "1"],
			] {
				import(flags, source).validate().unwrap();
			}
			for source in [
				["ts", "--program", "all"].as_slice(),
				&["hls", "http://origin/master.m3u8"],
				&["rtmp", "--listen", "127.0.0.1:1935"],
				&["rtsp", "--connect", "rtsp://camera/stream"],
			] {
				let err = import(flags, source).validate().unwrap_err().to_string();
				assert!(err.contains(flags[0]), "{flags:?} {source:?}: {err}");
			}
		}
	}

	/// `--echo-path` resolves against the broadcast. A path above the connection root, which
	/// every reader would refuse the catalog over, or one no viewer could publish its `.echo`
	/// beneath, is refused before dialing.
	#[test]
	fn echo_path_names_where_viewers_can_publish() {
		let import = |broadcast: &str, path: &str| {
			let argv = [
				"moq",
				"--connect",
				"http://relay",
				"--broadcast",
				broadcast,
				"import",
				"--echo-path",
				path,
				"ts",
			];
			Invocation::try_parse_from(argv).unwrap().validate()
		};
		// `viewers` from `<n parts>` resolves to n parts, and the feedback sits one beneath.
		let deep = |parts: usize| (0..parts).map(|i| format!("p{i}")).collect::<Vec<_>>().join("/");
		let (fits, too_deep) = (
			deep(hang::moq_net::Path::MAX_PARTS - 1),
			deep(hang::moq_net::Path::MAX_PARTS),
		);
		for (broadcast, path) in [
			("room", "viewers"),
			("room/live", "../viewers"),
			(fits.as_str(), "viewers"),
		] {
			import(broadcast, path).unwrap();
		}
		for (broadcast, path) in [
			("room", "../viewers"),
			("room/live", "../../viewers"),
			("room", "*"),
			("room", "viewers/**"),
			("room", "view*"),
			(too_deep.as_str(), "viewers"),
		] {
			let err = import(broadcast, path).unwrap_err().to_string();
			assert!(err.contains("--echo-path"), "{broadcast} {path}: {err}");
		}
	}

	/// `--echo` reports what a stdout container export reads from a hang catalog, under one
	/// path segment, and the report sinks read no media to report on.
	#[test]
	fn echo_needs_a_container_export_and_a_hang_catalog() {
		for sink in ["ts", "fmp4", "mkv", "flv", "h264", "h265"] {
			export(&["--echo", "alice", sink]).unwrap().validate().unwrap();
		}
		for sink in [["hls", "--listen", "127.0.0.1:8080"].as_slice(), &["report"], &["echo"]] {
			let err = export(&[["--echo", "alice"].as_slice(), sink].concat())
				.unwrap()
				.validate()
				.unwrap_err();
			assert!(err.to_string().contains("--echo"), "{sink:?}: {err}");
		}
		for flags in [
			["--echo", "alice", "--catalog-format", "msf", "fmp4"].as_slice(),
			&["--broadcast", "live.msf", "--echo", "alice", "fmp4"],
		] {
			let err = export(flags).unwrap().validate().unwrap_err();
			assert!(err.to_string().contains("MSF"), "{flags:?}: {err}");
		}
		export(&[
			"--broadcast",
			"live.msf",
			"--catalog-format",
			"hang",
			"--echo",
			"alice",
			"fmp4",
		])
		.unwrap()
		.validate()
		.unwrap();
		for name in ["", "a/b", ".", "..", "*", ".alice"] {
			assert!(export(&["--echo", name, "fmp4"]).is_err(), "`{name}` must be refused");
		}
		for sink in ["report", "echo"] {
			let err = export(&["--catalog-format", "hang", sink])
				.unwrap()
				.validate()
				.unwrap_err();
			assert!(err.to_string().contains("--catalog-format"), "{sink}: {err}");
		}
	}

	#[test]
	fn import_ts_takes_a_program_number_or_all() {
		// `None` when the command line is refused.
		let program = |value: &str| {
			let cli = Invocation::try_parse_from(["moq", "import", "ts", "--program", value]).ok()?;
			let Command::Import(import) = &cli.stages[0] else {
				panic!("an import stage");
			};
			let ImportSource::Ts(args) = &import.source else {
				panic!("an import ts stage");
			};
			Some(args.program)
		};
		assert_eq!(program("2"), Some(Some(TsProgram::One(2))));
		assert_eq!(program("all"), Some(Some(TsProgram::All)));
		assert_eq!(program("0"), None, "0 is the network PID");
		assert_eq!(program("two"), None);
	}

	#[test]
	fn import_ts_passthrough_takes_an_optional_pcr_pid() {
		// `None` when the command line is refused.
		let format = |args: &[&str]| {
			let cli = Invocation::try_parse_from([&["moq", "import", "ts"], args].concat()).ok()?;
			let Command::Import(import) = &cli.stages[0] else {
				panic!("an import stage");
			};
			import.source.stdin_format()
		};
		assert!(matches!(
			format(&["--passthrough"]),
			Some(PublishFormat::TsPassthrough { pcr_pid: None })
		));
		assert!(matches!(
			format(&["--passthrough", "--pcr-pid", "0x21"]),
			Some(PublishFormat::TsPassthrough { pcr_pid: Some(0x21) })
		));
		assert!(matches!(
			format(&["--passthrough", "--pcr-pid", "33"]),
			Some(PublishFormat::TsPassthrough { pcr_pid: Some(33) })
		));
		assert!(format(&["--pcr-pid", "33"]).is_none(), "--pcr-pid needs --passthrough");
		assert!(
			format(&["--passthrough", "--program", "1"]).is_none(),
			"nothing to select"
		);
		assert!(
			format(&["--passthrough", "--pcr-pid", "0x1fff"]).is_none(),
			"null packets"
		);
		assert!(format(&["--passthrough", "--pcr-pid", "0"]).is_none(), "the PAT");
		assert!(format(&["--passthrough", "--pcr-pid", "clock"]).is_none());
		assert!(matches!(format(&[]), Some(PublishFormat::Ts { program: None })));
	}

	/// `import srt` takes the same `--program` as `import ts`; `export srt` has no program to pick.
	#[test]
	fn import_srt_takes_a_program() {
		let cli =
			Invocation::try_parse_from(["moq", "import", "srt", "--listen", "[::]:9000", "--program", "all"]).unwrap();
		let Command::Import(import) = &cli.stages[0] else {
			panic!("an import stage");
		};
		let ImportSource::Srt(args) = &import.source else {
			panic!("an import srt stage");
		};
		assert_eq!(args.program(), Some(moq_srt::Program::All));
		assert!(args.endpoint.listen.is_some());

		assert!(
			Invocation::try_parse_from(["moq", "export", "srt", "--listen", "[::]:9000", "--program", "2"]).is_err()
		);
	}

	/// `export srt` follows its broadcast with the same `--follow` and `--stitch` as `export ts`.
	#[test]
	fn export_srt_takes_follow_and_stitch() {
		let cli = Invocation::try_parse_from([
			"moq",
			"export",
			"srt",
			"--listen",
			"[::]:9000",
			"--follow",
			"10s",
			"--stitch",
		])
		.unwrap();
		let Command::Export(export) = &cli.stages[0] else {
			panic!("an export stage");
		};
		let ExportSink::Srt(args) = &export.sink else {
			panic!("an export srt stage");
		};
		assert_eq!(Duration::from(args.follow), Duration::from_secs(10));
		assert!(args.stitch);
		assert!(
			Invocation::try_parse_from(["moq", "import", "srt", "--listen", "[::]:9000", "--stitch"]).is_err(),
			"an ingest has nothing to follow"
		);
	}

	/// A released spelling is refused, and the error names what to write instead.
	///
	/// The alternative is what this replaced: the flag parsed onto a hidden field,
	/// warned about the rename, and then went unread, so `moq --client-connect ...`
	/// dialed nothing and neither errored nor exited.
	#[test]
	fn released_spellings_are_refused_with_a_migration() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--client-connect",
			"http://relay/anon",
			"--client-connect-timeout",
			"9s",
			"--client-tls-fingerprint",
			"abcd1234",
			"--client-quic-gso=false",
			"--server-bind",
			"[::]:4443",
			"--server-tcp-bind",
			"127.0.0.1:4444",
			"export",
			"ts",
		]) else {
			panic!("a released spelling must not start a run");
		};

		let reported = err.to_string();
		for line in [
			"--client-connect / MOQ_CLIENT_CONNECT -> --connect / MOQ_CONNECT",
			"--client-connect-timeout / MOQ_CLIENT_CONNECT_TIMEOUT -> --connect-timeout / MOQ_CONNECT_TIMEOUT",
			"--client-tls-fingerprint / MOQ_CLIENT_TLS_FINGERPRINT -> --connect-tls-fingerprint / MOQ_CONNECT_TLS_FINGERPRINT",
			"--client-quic-gso / MOQ_CLIENT_QUIC_GSO -> --quic-gso / MOQ_QUIC_GSO",
			"--server-bind / MOQ_SERVER_BIND -> --listen / MOQ_LISTEN",
			"--server-tcp-bind / MOQ_SERVER_TCP_BIND -> --listen-tcp-bind / MOQ_LISTEN_TCP_BIND",
		] {
			assert!(reported.contains(line), "missing {line:?} from {reported}");
		}
	}

	#[test]
	fn independent_importers_cannot_select_a_shared_epoch() {
		let epoch = hang::moq_net::Epoch::mint().to_string();
		let refused = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay/anon",
			"--epoch",
			&epoch,
			"import",
			"ts",
		]);
		assert!(refused.is_err(), "independent importers cannot claim identical content");
	}

	/// A stage carries config of its own, and the check has to reach it.
	///
	/// `export hls` adapts its TLS arguments. Its listener decides whether to serve
	/// TLS at all from the canonical `cert`/`generate` fields, so a released
	/// `--tls-cert` left it serving plaintext HTTP without ever reaching the builder
	/// that refuses: the certificate and the mTLS roots both silently gone.
	#[test]
	fn a_stage_local_released_spelling_is_refused() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay/anon",
			"--broadcast",
			"room",
			"export",
			"hls",
			"--tls-cert",
			"/tmp/cert.pem",
			"--server-tls-root",
			"/tmp/ca.pem",
		]) else {
			panic!("a released spelling on a stage must not start a run");
		};

		let reported = err.to_string();
		assert!(reported.contains("--tls-cert"), "{reported}");
		assert!(reported.contains("--listen-tls-cert"), "{reported}");
		assert!(reported.contains("--listen-tls-root"), "{reported}");
	}

	/// One released spelling stops the run even when the rest of the command line is
	/// current: honoring the half it understands is how a process ends up serving on
	/// settings nobody wrote.
	#[test]
	fn one_released_spelling_is_enough_to_refuse() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay/anon",
			"--client-quic-gso=false",
			"export",
			"ts",
		]) else {
			panic!("a current spelling alongside a released one must not excuse it");
		};
		assert!(err.to_string().contains("--quic-gso"), "{err}");
	}

	#[test]
	fn tcp_only_listener_is_a_moq_side() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--listen-tcp-bind",
			"127.0.0.1:0",
			"--auth-public",
			"**",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert!(cli.moq.serves());
		assert_eq!(cli.moq.server_config().tcp.bind, Some("127.0.0.1:0".parse().unwrap()));
	}

	#[cfg(unix)]
	#[test]
	fn unix_only_listener_is_a_moq_side() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--listen-unix-bind",
			"/tmp/moq-cli.sock",
			"--auth-public",
			"**",
			"export",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert!(cli.moq.serves());
		assert_eq!(
			cli.moq.server_config().unix.bind.as_deref(),
			Some(std::path::Path::new("/tmp/moq-cli.sock"))
		);
	}

	/// Public rules grant a certificate what they grant anyone, so a client CA on
	/// a public listener refuses to start, as it does on the relay.
	#[test]
	fn a_client_ca_needs_an_auth_server() {
		let parse = |auth: [&str; 2]| {
			let mut argv = vec!["moq", "--listen", "127.0.0.1:0", "--listen-tls-root", "ca.pem"];
			argv.extend(auth);
			argv.extend(["import", "ts"]);
			Invocation::try_parse_from(argv).expect("parse")
		};
		let err = parse(["--auth-public", "**"]).moq.validate().unwrap_err().to_string();
		assert!(err.contains("--auth-public ignores"), "{err}");
		assert!(parse(["--auth-url", "http://127.0.0.1:4440/"]).moq.validate().is_ok());
	}

	#[test]
	fn listening_drain_settings_parse() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--listen-tcp-bind",
			"127.0.0.1:0",
			"--auth-public",
			"**",
			"--drain-timeout",
			"500ms",
			"import",
			"ts",
		])
		.unwrap();
		cli.validate().unwrap();
		cli.unserved().unwrap();
		assert_eq!(
			Duration::from(cli.moq.drain_timeout.unwrap()),
			Duration::from_millis(500)
		);
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"https://relay.example",
			"--drain-timeout",
			"500ms",
			"import",
			"ts",
		])
		.unwrap();
		assert!(cli.unserved().unwrap_err().to_string().contains("--drain-timeout"));
	}

	#[cfg(feature = "play")]
	#[test]
	fn listening_lifecycle_settings_do_not_apply_to_playback() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--listen-tcp-bind",
			"127.0.0.1:0",
			"--auth-public",
			"**",
			"--drain-timeout",
			"500ms",
			"play",
		])
		.unwrap();
		assert!(cli.validate().unwrap_err().to_string().contains("--drain-timeout"));
	}

	/// A listener for ordinary clients admits nobody without a decision, so it
	/// refuses to start with neither flag, and with both.
	#[test]
	fn a_listener_needs_exactly_one_auth_source() {
		let cli =
			Invocation::try_parse_from(["moq", "--listen-tcp-bind", "127.0.0.1:0", "import", "ts"]).expect("parse");
		let err = cli.moq.validate().unwrap_err().to_string();
		assert!(err.contains("--auth-url or --auth-public"), "{err}");

		let cli = Invocation::try_parse_from([
			"moq",
			"--listen-tcp-bind",
			"127.0.0.1:0",
			"--auth-url",
			"http://127.0.0.1:4440/",
			"--auth-public",
			"**",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_err());

		let cli = Invocation::try_parse_from([
			"moq",
			"--listen-tcp-bind",
			"127.0.0.1:0",
			"--auth-url",
			"http://127.0.0.1:4440/",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_ok());

		// A local verb refuses the flag like every other MoQ-side flag.
		let cli = Invocation::try_parse_from(["moq", "--auth-public", "**", "auth", "generate"]).expect("parse");
		assert!(cli.reject("auth").unwrap_err().to_string().contains("--auth-public"));
	}

	/// The grammar Usage can't express: one connection, several endpoints.
	#[test]
	fn multiple_stages() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://localhost:4444/event",
			"import",
			"--broadcast",
			"cam1.hang",
			"rtmp",
			"--listen",
			"0.0.0.0:1935",
			"--",
			"import",
			"--broadcast",
			"cam2.hang",
			"rtmp",
			"--listen",
			"0.0.0.0:1936",
			"--",
			"export",
			"--broadcast",
			"cam1.hang",
			"hls",
			"--listen",
			"0.0.0.0:8080",
		])
		.unwrap();

		assert!(cli.validate().is_ok());
		assert_eq!(cli.stages.len(), 3);

		// The globals are read once, from the first chunk, and shared by every stage.
		assert_eq!(
			cli.moq.client.url.as_ref().map(ToString::to_string).as_deref(),
			Some("http://localhost:4444/event")
		);

		let names: Vec<String> = cli.stages.iter().map(|stage| stage.broadcast(&cli.moq)).collect();
		assert_eq!(names, ["cam1.hang", "cam2.hang", "cam1.hang"]);
		assert_eq!(cli.stages[2].name(), "export");
	}

	/// A stage without its own `--broadcast` falls back to the process-wide one, so
	/// every single-stage invocation keeps naming the broadcast before the verb.
	#[test]
	fn broadcast_falls_back_to_the_global() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"--broadcast",
			"room.hang",
			"import",
			"ts",
			"--",
			"export",
			"--broadcast",
			"other.hang",
			"fmp4",
		])
		.unwrap();

		assert_eq!(cli.stages[0].broadcast(&cli.moq), "room.hang");
		assert_eq!(cli.stages[1].broadcast(&cli.moq), "other.hang");
	}

	/// An unnamed broadcast is the root one at the connection path, not an error.
	#[test]
	fn broadcast_defaults_to_root() {
		let cli = Invocation::try_parse_from(["moq", "--connect", "http://relay", "import", "ts"]).unwrap();
		assert_eq!(cli.stages[0].broadcast(&cli.moq), "");
	}

	/// Only import/export share an Origin; the rest own the process.
	#[test]
	fn rejects_unstageable_verbs() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"import",
			"ts",
			"--",
			"auth",
			"generate",
			"--algorithm",
			"ES256",
		])
		.unwrap();

		let err = cli.validate().unwrap_err().to_string();
		assert!(err.contains("auth"), "{err}");
	}

	/// Each stage is parsed by a real Usage parser, so a typo past the first `--` is
	/// still a parse error rather than something swallowed as a positional.
	#[test]
	fn stage_errors_are_parse_errors() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"import",
			"ts",
			"--",
			"import",
			"rtmp",
			"--bogus",
		]) else {
			panic!("expected a parse error")
		};

		assert_eq!(err.kind(), ParseErrorKind::UnknownArgument);
	}

	/// Splitting on `--` claims it from Usage, so it can't also escape a positional
	/// starting with `-`. `./-name` is the documented way to write one.
	#[test]
	fn a_dash_prefixed_path_is_written_relative() {
		let cli =
			Invocation::try_parse_from(["moq", "--connect", "http://relay", "import", "hls", "./-odd.m3u8"]).unwrap();

		let Command::Import(import) = &cli.stages[0] else {
			panic!("expected import")
		};
		let ImportSource::Hls(hls) = &import.source else {
			panic!("expected hls")
		};
		assert_eq!(hls.playlist, "./-odd.m3u8");
	}

	/// A `--` with nothing after it names no verb, so it's an error rather than an
	/// empty stage. Same for a doubled `--`, which leaves an empty chunk between them.
	#[test]
	fn rejects_an_empty_stage() {
		for argv in [
			vec!["moq", "--connect", "http://relay", "import", "ts", "--"],
			vec![
				"moq",
				"--connect",
				"http://relay",
				"import",
				"ts",
				"--",
				"--",
				"export",
				"fmp4",
			],
		] {
			let Err(err) = Invocation::try_parse_from(argv.clone()) else {
				panic!("expected a parse error for {argv:?}")
			};

			assert_eq!(err.kind(), ParseErrorKind::MissingSubcommand);
			assert!(err.to_string().contains("must be followed by"), "{err}");
		}
	}

	/// The globals belong to the invocation, not a stage: there is only ever one
	/// connection, so accepting `--client-connect` again would be a lie.
	#[test]
	fn stages_reject_globals() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"import",
			"ts",
			"--",
			"--connect",
			"http://other",
			"import",
			"fmp4",
		]) else {
			panic!("expected a parse error")
		};

		assert_eq!(err.kind(), ParseErrorKind::UnknownArgument);
	}

	/// Passthrough imports share a connection; they never read the estimate.
	#[test]
	fn imports_without_rate_control_can_share_a_connection() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"import",
			"--broadcast",
			"a.hang",
			"rtmp",
			"--listen",
			"127.0.0.1:1935",
			"--",
			"import",
			"--broadcast",
			"b.hang",
			"srt",
			"--listen",
			"127.0.0.1:9000",
		])
		.unwrap();

		assert!(cli.validate().is_ok());
	}

	/// Two encoding stages share the connection's allocator, so they may run
	/// together rather than each targeting the whole estimate.
	#[cfg(feature = "capture")]
	#[test]
	fn two_encoding_stages_may_share_a_connection() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"import",
			"capture",
			"--",
			"import",
			"capture",
		])
		.unwrap();

		assert!(cli.validate().is_ok());
	}

	#[test]
	fn max_age_is_unset_unless_asked_for() {
		// Unset rather than defaulted to hang's constant, so the publisher's own default is
		// what every source falls back to. A second default here would put the number in the
		// CLI as well, and the two would drift.
		let cli = Invocation::try_parse_from(["moq", "import", "ts"]).unwrap();
		let Command::Import(import) = &cli.stages[0] else {
			panic!("expected import")
		};
		assert_eq!(import.max_age, None);

		// It sits on the parent `import`, so it parses ahead of any source, gateway or not.
		let cli = Invocation::try_parse_from(["moq", "import", "--max-age", "5s", "ts"]).unwrap();
		let Command::Import(import) = &cli.stages[0] else {
			panic!("expected import")
		};
		assert_eq!(import.max_age, Some(Duration::from_secs(5).into()));

		let cli =
			Invocation::try_parse_from(["moq", "import", "--max-age", "5s", "rtmp", "--listen", "127.0.0.1:1935"])
				.unwrap();
		let Command::Import(import) = &cli.stages[0] else {
			panic!("expected import")
		};
		assert_eq!(import.max_age, Some(Duration::from_secs(5).into()));
	}

	/// The released spelling still parses so the process can name `--max-age`, but
	/// it configures nothing and must stop a run.
	#[test]
	fn latency_max_is_refused_with_a_migration() {
		let Err(err) = Invocation::try_parse_from(["moq", "import", "--latency-max", "5s", "ts"]) else {
			panic!("--latency-max must not start a run");
		};
		assert!(err.to_string().contains("--latency-max -> --max-age"), "{}", err);

		let Err(err) = Invocation::try_parse_from(["moq", "export", "--broadcast", "b", "mkv", "--latency-max", "1s"])
		else {
			panic!("--latency-max must not start a run");
		};
		assert!(err.to_string().contains("--latency-max -> --delay"), "{}", err);
	}

	/// A publisher's retention is `--max-age` on `import`; a subscriber's staleness budget is
	/// `--max-delay` for immediate exports and `--delay` for buffered exports.
	#[test]
	fn export_staleness_is_max_delay_and_import_retention_is_max_age() {
		let export = |args: &[&str]| {
			let mut argv = vec!["moq", "export", "--broadcast", "b"];
			argv.extend_from_slice(args);
			Invocation::try_parse_from(argv)
		};
		let stdout = |args: &[&str]| {
			let cli = export(args).unwrap();
			let Command::Export(export) = &cli.stages[0] else {
				panic!("expected export")
			};
			export.sink.stdout().unwrap().max_delay
		};

		for format in ["fmp4", "h264", "h265"] {
			assert_eq!(stdout(&[format]), Duration::from_millis(500), "{format}");
			assert_eq!(
				stdout(&[format, "--max-delay", "2s"]),
				Duration::from_secs(2),
				"{format}"
			);
			let Err(err) = export(&[format, "--max-age", "2s"]) else {
				panic!("{format}: --max-age must not start a run");
			};
			assert!(err.to_string().contains("--max-age -> --max-delay"), "{format}: {err}");
		}

		for format in ["flv", "mkv"] {
			assert_eq!(stdout(&[format]), Duration::from_millis(500));
			assert_eq!(stdout(&[format, "--delay", "2s"]), Duration::from_secs(2));
			for old in ["--max-age", "--latency-max"] {
				let Err(err) = export(&[format, old, "2s"]) else {
					panic!("{old} must be refused")
				};
				assert!(
					err.to_string().contains(&format!("{old} -> --delay")),
					"{format}: {err}"
				);
			}
		}

		assert_eq!(stdout(&["ts", "--delay", "2s"]), Duration::from_secs(2));
		assert!(export(&["ts", "--max-delay", "2s"]).is_err(), "ts spells it --delay");

		let rtmp = |flag: &str| export(&["rtmp", "--connect", "rtmp://example.com/live/key", flag, "2s"]);
		let cli = rtmp("--delay").unwrap();
		let Command::Export(export_rtmp) = &cli.stages[0] else {
			panic!("expected export")
		};
		let ExportSink::Rtmp(args) = &export_rtmp.sink else {
			panic!("expected rtmp")
		};
		assert_eq!(Duration::from(args.delay), Duration::from_secs(2));
		let Err(err) = rtmp("--max-age") else {
			panic!("rtmp: --max-age must not start a run");
		};
		assert!(err.to_string().contains("--max-age -> --delay"), "rtmp: {err}");

		assert!(
			Invocation::try_parse_from(["moq", "import", "--max-delay", "5s", "ts"]).is_err(),
			"import has no subscriber budget"
		);
	}

	#[test]
	fn the_released_name_and_verb_spellings_are_refused() {
		let Err(err) = Invocation::try_parse_from(["moq", "--name", "room", "import", "ts"]) else {
			panic!("--name must not start a run");
		};
		assert!(err.to_string().contains("--name -> --broadcast"), "{err}");

		let Err(err) = Invocation::try_parse_from(["moq", "publish", "ts"]) else {
			panic!("publish must not start a run");
		};
		assert!(err.to_string().contains("publish -> import"), "{err}");

		let Err(err) = Invocation::try_parse_from(["moq", "subscribe", "ts"]) else {
			panic!("subscribe must not start a run");
		};
		assert!(err.to_string().contains("subscribe -> export"), "{err}");
	}

	/// An exported `MOQ_CONNECT` configures a MoQ side but does not ask for one, so a
	/// local verb still runs in a shell that exports a relay for its usual publishing.
	#[test]
	fn the_environment_cannot_ask_for_a_moq_side() {
		let url = "https://relay.example.com";
		if !test_env::Env::default().set("MOQ_CONNECT", url).isolate() {
			return;
		}

		let ambient = Invocation::try_parse_from(["moq", "auth", "generate"]).expect("parse");
		assert!(
			ambient.moq.client.url.is_some(),
			"the resolved side should still pick the variable up"
		);
		assert!(
			ambient.reject("auth").is_ok(),
			"an exported MOQ_CONNECT was treated as a request"
		);

		let typed = Invocation::try_parse_from(["moq", "--connect", url, "auth", "generate"]).expect("parse");
		assert!(typed.reject("auth").is_err(), "a typed --connect stopped being refused");
	}

	#[test]
	fn auth_verb() {
		let cli = Invocation::try_parse_from(["moq", "auth", "generate", "--algorithm", "ES256"]).unwrap();
		assert!(matches!(cli.stages[0], Command::Auth(_)));
		// Local verb: it needs no MoQ side, so what every other verb demands...
		assert!(cli.moq.validate().is_err());
		assert!(cli.reject("auth").is_ok());

		// ...these it refuses, rather than accepting the flag and ignoring it.
		for (flag, value, reported) in [
			("--connect", "https://relay.example.com", "--connect"),
			("--listen-tcp-bind", "127.0.0.1:0", "--listen-tcp-bind"),
			("--broadcast", "room", "--broadcast"),
			("--cluster-connect", "https://relay.example", "--cluster-connect"),
			("--cluster-api-url", "https://api.example/peers", "--cluster-api-url"),
			("--cluster-node", "https://self.example", "--cluster-node"),
			("--cluster-token", "cluster.jwt", "--cluster-token"),
			("--cluster-id", "1", "--cluster-id"),
			("--cluster-tier", "internal", "--cluster-tier"),
		] {
			let cli = Invocation::try_parse_from(["moq", flag, value, "auth", "generate"]).unwrap();
			let err = cli.reject("auth").unwrap_err().to_string();
			assert!(err.contains(reported), "{err}");
		}

		#[cfg(unix)]
		{
			for (flag, value, reported) in [
				("--listen-unix-bind", "/tmp/moq-cli.sock", "--listen-unix-bind"),
				("--listen-unix-allow-uid", "1000", "--listen-unix-allow-uid"),
				("--listen-unix-allow-gid", "1000", "--listen-unix-allow-gid"),
				("--listen-unix-allow-pid", "1000", "--listen-unix-allow-pid"),
			] {
				let cli = Invocation::try_parse_from(["moq", flag, value, "auth", "generate"]).unwrap();
				let err = cli.reject("auth").unwrap_err().to_string();
				assert!(err.contains(reported), "{err}");
			}
		}

		#[cfg(feature = "cluster-lan")]
		{
			let cli = Invocation::try_parse_from(["moq", "--cluster-lan", "auth", "generate"]).unwrap();
			let err = cli.reject("auth").unwrap_err().to_string();
			assert!(err.contains("--cluster-lan"), "{err}");

			// The parser considers the secret's `requires` satisfied when the boolean flag
			// is explicitly present but false. The local verb still has to reject the
			// otherwise silently ignored secret.
			let cli = Invocation::try_parse_from([
				"moq",
				"--cluster-lan=false",
				"--cluster-lan-secret",
				"cluster.key",
				"auth",
				"generate",
			])
			.unwrap();
			let err = cli.reject("auth").unwrap_err().to_string();
			assert!(err.contains("--cluster-lan-secret"), "{err}");

			let cli = Invocation::try_parse_from([
				"moq",
				"--cluster-lan=false",
				"--cluster-lan-app",
				"custom",
				"auth",
				"generate",
			])
			.unwrap();
			let err = cli.reject("auth").unwrap_err().to_string();
			assert!(err.contains("--cluster-lan-app"), "{err}");
		}
	}

	/// Each verb refuses a flag from every MoQ-side family it never reads, naming it.
	/// The listener and dial families had members the old hand-written list missed.
	#[test]
	fn every_unused_family_is_refused() {
		let accept: &[&[&str]] = &[
			&["--listen", "[::]:0"],
			&["--listen-version", "moq-lite-02"],
			&["--listen-tls-generate", "localhost"],
			&["--listen-preferred-v4", "127.0.0.1:443"],
			&["--listen-quic-lb-id", "01"],
			&["--listen-tcp-bind", "127.0.0.1:0"],
			&["--cluster-node", "https://self.example"],
			&["--auth-public", "**"],
			&["--drain-timeout", "500ms"],
		];
		let dial: &[&[&str]] = &[
			&["--connect", "https://relay.example"],
			&["--connect-tls-insecure"],
			&["--connect-backoff-initial", "2s"],
			&["--quic-idle-timeout", "10s"],
			#[cfg(feature = "iroh")]
			&["--iroh-enabled"],
			&["--broadcast", "room"],
		];

		let parse = |flag: &[&str], verb: &[&str]| {
			let argv = ["moq"].iter().chain(flag).chain(verb).copied();
			Invocation::try_parse_from(argv).unwrap_or_else(|err| panic!("{flag:?}: {err}"))
		};

		for verb in [&["auth", "generate"][..], &["completion", "bash"]] {
			for flag in accept.iter().chain(dial) {
				let err = parse(flag, verb).reject(verb[0]).unwrap_err().to_string();
				assert!(err.contains(flag[0]), "{verb:?} {flag:?}: {err}");
			}
		}

		for flag in accept {
			let err = parse(flag, &["fetch", "data"])
				.dial_only("fetch", &["--broadcast"])
				.unwrap_err()
				.to_string();
			assert!(err.contains(flag[0]), "{flag:?}: {err}");
		}
		for flag in dial {
			parse(flag, &["fetch", "data"])
				.dial_only("fetch", &["--broadcast"])
				.unwrap_or_else(|err| panic!("{flag:?}: {err}"));
		}
	}

	/// A listener or auth flag with nothing listening is refused, naming it, and
	/// accepted once a listener is bound.
	#[test]
	fn listener_flags_need_a_listener() {
		let parse = |flags: &[&str]| {
			let argv = ["moq", "--connect", "https://relay.example"]
				.iter()
				.chain(flags)
				.chain(&["import", "ts"])
				.copied();
			Invocation::try_parse_from(argv).unwrap_or_else(|err| panic!("{flags:?}: {err}"))
		};
		for flag in [
			&["--listen-tls-root", "ca.pem"][..],
			&["--listen-tls-cert", "cert.pem"],
			&["--listen-tcp-tls"],
			&["--listen-version", "moq-lite-02"],
			&["--auth-public", "**"],
		] {
			let err = parse(flag).unserved().unwrap_err().to_string();
			assert!(err.contains(flag[0]), "{flag:?}: {err}");
		}
		parse(&["--listen", "[::]:0", "--listen-tls-root", "ca.pem"])
			.unserved()
			.expect("a listener reads it");
		parse(&[]).unserved().expect("nothing to refuse");
	}

	/// `--connect` and a WAN cluster peer each bind `--connect-bind` for QUIC, so a
	/// fixed port is refused before anything binds; port 0 or either side alone is
	/// fine, and a build without QUIC binds nothing to collide.
	#[test]
	fn a_fixed_connect_bind_serves_one_dialer() {
		let parse = |flags: &[&str]| {
			let args = ["moq"].iter().chain(flags).chain(&["import", "ts"]).copied();
			Invocation::try_parse_from(args).expect("parse")
		};
		for peer in [
			["--cluster-connect", "https://relay.example"],
			["--cluster-api-url", "https://api.example/peers"],
		] {
			let both = [
				&["--connect", "https://relay.example", "--connect-bind", "127.0.0.1:4443"][..],
				&peer,
			]
			.concat();
			let result = parse(&both).moq.validate();
			if cfg!(feature = "quic") {
				let err = result.unwrap_err().to_string();
				assert!(err.contains("--connect-bind") && err.contains(peer[0]), "{err}");
			} else {
				result.expect("without QUIC nothing binds the fixed port");
			}

			let ephemeral = [
				&["--connect", "https://relay.example", "--connect-bind", "127.0.0.1:0"][..],
				&peer,
			]
			.concat();
			parse(&ephemeral).moq.validate().expect("an ephemeral bind serves both");

			let cluster_only = [&["--connect-bind", "127.0.0.1:4443"][..], &peer].concat();
			parse(&cluster_only)
				.moq
				.validate()
				.expect("the cluster alone owns a fixed bind");
		}
		parse(&["--connect", "https://relay.example", "--connect-bind", "127.0.0.1:4443"])
			.moq
			.validate()
			.expect("--connect alone owns a fixed bind");
	}

	/// `--cluster-connect` / `--cluster-api-url` attach the process as a
	/// cluster peer, so they are a MoQ side on their own. `--cluster-node` is
	/// identity, not an attachment.
	#[test]
	fn cluster_connect_is_a_moq_side() {
		let cli = Invocation::try_parse_from(["moq", "--cluster-connect", "https://relay.example", "import", "ts"])
			.expect("parse");
		assert!(cli.moq.validate().is_ok(), "a cluster dial is a MoQ side on its own");

		let cli = Invocation::try_parse_from(["moq", "--cluster-api-url", "https://api.example/peers", "import", "ts"])
			.expect("parse");
		assert!(cli.moq.validate().is_ok(), "a cluster API is a MoQ side on its own");

		let cli = Invocation::try_parse_from(["moq", "--cluster-node", "https://self.example", "import", "ts"])
			.expect("parse");
		let err = cli.moq.validate().unwrap_err().to_string();
		assert!(err.contains("MoQ side"), "{err}");
	}

	/// `--cluster-lan` is a MoQ side on its own, and it supplies the listener the
	/// user would otherwise have to spell out.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_is_a_moq_side_and_fills_in_a_listener() {
		let cli = Invocation::try_parse_from(["moq", "--cluster-lan", "import", "ts"]).expect("parse");
		assert!(cli.moq.lan());
		assert!(cli.moq.validate().is_ok(), "the LAN mesh is a MoQ side on its own");

		let server = cli.moq.server_config();
		assert_eq!(
			server.bind.as_ref().map(ToString::to_string).as_deref(),
			Some("[::]:0"),
			"an ephemeral port"
		);
		assert_eq!(server.tls.generate, ["moq-cluster-lan"], "a generated certificate");

		// An explicit listener wins, so the mesh shares one port and certificate
		// with ordinary clients.
		let cli = Invocation::try_parse_from([
			"moq",
			"--cluster-lan",
			"--listen",
			"[::]:4443",
			"--listen-tls-generate",
			"localhost",
			"import",
			"ts",
		])
		.expect("parse");
		let server = cli.moq.server_config();
		assert_eq!(
			server.bind.as_ref().map(ToString::to_string).as_deref(),
			Some("[::]:4443")
		);
		assert_eq!(server.tls.generate, ["localhost"]);

		// Without the mesh, nothing is filled in.
		let cli = Invocation::try_parse_from(["moq", "--connect", "https://relay.example.com", "import", "ts"])
			.expect("parse");
		assert!(!cli.moq.lan());
		assert_eq!(cli.moq.server_config().bind, None);
	}

	/// The secret is only read by the mesh, so configuring one without it is an
	/// error rather than a silently ignored flag.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_secret_requires_the_mesh() {
		for lan in [None, Some("--cluster-lan=false")] {
			let mut argv = vec!["moq"];
			if let Some(lan) = lan {
				argv.push(lan);
			}
			argv.extend([
				"--cluster-lan-secret",
				"cluster.key",
				"--connect",
				"https://relay.example.com",
				"import",
				"ts",
			]);
			let cli = Invocation::try_parse_from(argv).expect("parse");
			let err = cli.moq.validate().unwrap_err().to_string();
			assert!(err.contains("--cluster-lan=true"), "{err}");
		}

		let cli = Invocation::try_parse_from([
			"moq",
			"--cluster-lan",
			"--cluster-lan-secret",
			"cluster.key",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert_eq!(
			cli.moq.cluster.lan.secret.as_deref(),
			Some(std::path::Path::new("cluster.key"))
		);
	}

	/// The app is only read by the mesh, so configuring one without it is an
	/// error rather than a silently ignored flag. The default is `default`.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_app_requires_the_mesh_and_defaults() {
		for lan in [None, Some("--cluster-lan=false")] {
			let mut argv = vec!["moq"];
			if let Some(lan) = lan {
				argv.push(lan);
			}
			argv.extend([
				"--cluster-lan-app",
				"custom",
				"--connect",
				"https://relay.example.com",
				"import",
				"ts",
			]);
			let cli = Invocation::try_parse_from(argv).expect("parse");
			let err = cli.moq.validate().unwrap_err().to_string();
			assert!(err.contains("--cluster-lan=true"), "{err}");
		}

		let cli = Invocation::try_parse_from(["moq", "--cluster-lan", "import", "ts"]).expect("parse");
		assert_eq!(cli.moq.cluster.lan.app.clone().unwrap_or_default().as_str(), "default");

		let cli = Invocation::try_parse_from(["moq", "--cluster-lan", "--cluster-lan-app", "custom", "import", "ts"])
			.expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert_eq!(
			cli.moq.cluster.lan.app.as_ref().map(ToString::to_string).as_deref(),
			Some("custom")
		);

		let err = Invocation::try_parse_from(["moq", "--cluster-lan", "--cluster-lan-app", "Default", "import", "ts"])
			.err()
			.expect("uppercase must not parse")
			.to_string();
		assert!(err.contains("app") || err.contains("Default"), "{err}");
	}

	/// A mesh dial authenticates through its request path, which legacy moq-lite
	/// versions do not carry.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_requires_a_path_capable_version() {
		for (flag, reported) in [
			("--connect-version", "--connect-version"),
			("--listen-version", "--listen-version"),
		] {
			let cli = Invocation::try_parse_from(["moq", "--cluster-lan", flag, "moq-lite-04", "import", "ts"])
				.expect("parse");
			let err = cli.moq.validate().unwrap_err().to_string();
			assert!(err.contains(reported), "{flag}: {err}");
		}

		let cli = Invocation::try_parse_from([
			"moq",
			"--cluster-lan",
			"--connect-version",
			"moq-lite-04",
			"--connect-version",
			"moq-lite-05",
			"--listen-version",
			"moq-lite-05",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_ok());
	}

	#[cfg(feature = "play")]
	#[test]
	fn play_verb() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"https://relay.example.com/anon",
			"--broadcast",
			"room.hang",
			"play",
			"--video-name",
			"hd",
			"--audio-codec",
			"aac",
		])
		.unwrap();
		let Command::Play(play) = &cli.stages[0] else {
			panic!("expected play")
		};
		assert_eq!(play.delay.to_string(), "auto");
		assert_eq!(play.select.video_name.as_deref(), Some("hd"));
		assert!(cli.moq.validate().is_ok());
		assert!(play.validate().is_ok());
	}

	/// The selection flags are shared with exports, which pass every codec
	/// through. Playback validates them against the codecs it can decode.
	#[cfg(feature = "play")]
	#[test]
	fn play_rejects_undecodable_codecs() {
		for codec in ["vp8", "vp9"] {
			let cli = Invocation::try_parse_from([
				"moq",
				"--connect",
				"https://relay.example.com/anon",
				"play",
				"--video-codec",
				codec,
			])
			.unwrap();
			let Command::Play(play) = &cli.stages[0] else {
				panic!("expected play")
			};
			if cfg!(feature = "vpx") {
				play.validate().unwrap();
			} else {
				let err = play.validate().unwrap_err().to_string();
				assert!(err.contains(codec), "{err}");
			}
		}

		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"https://relay.example.com/anon",
			"play",
			"--audio-codec",
			"aac",
		])
		.unwrap();
		let Command::Play(play) = &cli.stages[0] else {
			panic!("expected play")
		};
		assert!(play.validate().is_ok());
	}

	#[test]
	fn flag_help_uses_plain_names() {
		for command in [
			vec![],
			vec!["import", "fmp4"],
			vec!["import", "hls"],
			vec!["export", "rtmp"],
			vec!["export", "hls"],
			vec!["auth", "serve"],
		] {
			let mut argv = vec!["moq"];
			argv.extend(command);
			argv.push("--help");
			let help = Invocation::try_parse_from(argv)
				.err()
				.expect("help request")
				.to_string();
			assert!(!help.contains("[`"), "{help}");
		}
	}

	/// Help and version are answered with their actual page, not an empty string.
	///
	/// Usage renders those variants as nothing through `render_failure`, because the
	/// caller is expected to take them first. The stage grammar parses each chunk
	/// itself rather than through the generated `parse()`, so it has to.
	#[test]
	fn help_and_version_render_their_output() {
		for args in [
			vec!["moq", "--help"],
			vec!["moq", "-h"],
			vec!["moq", "--version"],
			vec!["moq", "-V"],
			vec!["moq", "publish", "--help"],
		] {
			let Err(err) = Invocation::try_parse_from(args.clone()) else {
				panic!("{args:?} parsed instead of asking a question")
			};
			assert!(
				matches!(err.kind(), ParseErrorKind::DisplayHelp | ParseErrorKind::DisplayVersion),
				"{args:?} produced {:?}",
				err.kind()
			);
			assert!(!err.to_string().trim().is_empty(), "{args:?} printed nothing");
		}
	}

	/// A stage after `--` gets its own help page, since each chunk is its own parse.
	///
	/// The root spec models only the first stage, so a later chunk is parsed against
	/// `Stage` and has to render its own answer.
	#[test]
	fn stage_help_renders() {
		for args in [
			vec![
				"moq",
				"--connect",
				"http://localhost:4444/x",
				"import",
				"fmp4",
				"--",
				"export",
				"--help",
			],
			vec![
				"moq",
				"--connect",
				"http://localhost:4444/x",
				"import",
				"fmp4",
				"--",
				"export",
				"fmp4",
				"--help",
			],
		] {
			let Err(err) = Invocation::try_parse_from(args.clone()) else {
				panic!("{args:?} parsed instead of asking a question")
			};
			assert_eq!(err.kind(), ParseErrorKind::DisplayHelp, "{args:?}");
			assert!(
				err.to_string().contains("Usage:"),
				"{args:?} rendered no help page: {err}"
			);
		}
	}

	/// Every `*-version` flag offers exactly the versions moq-net parses.
	///
	/// The lists are `choices(...)` literals because Usage reads them at expansion
	/// time, so they are copies. This is what keeps a new protocol draft from
	/// parsing through `FromStr` while staying unreachable from the command line:
	/// add the draft, and this fails until every list has it.
	#[test]
	fn version_choices_match_the_parser() {
		fn walk<'a>(cmd: &'a usage_rs::argv::spec::CommandMeta<'a>, found: &mut Vec<(&'a str, Vec<&'a str>)>) {
			for flag in cmd.flags {
				let Some(long) = flag.flag.longs.first() else {
					continue;
				};
				if long.ends_with("version") && !flag.choices.is_empty() {
					found.push((long, flag.choices.to_vec()));
				}
			}
			for sub in cmd.subcommands {
				walk(sub, found);
			}
		}

		// As a set: a choice list reads ascending, and that ordering is a presentation call.
		// Every wire code moq-net parses, including the opt-in `-wip` drafts.
		let codes = (0xff0dad00..=0xff0dadff).chain(0xff000000..=0xff0000ff);
		let mut expected: Vec<&str> = codes
			.filter_map(hang::moq_net::Version::from_code)
			.map(|version| version.as_str())
			.collect();
		expected.sort_unstable();
		let mut found = Vec::new();
		walk(Cli::spec().root, &mut found);

		assert!(!found.is_empty(), "no version flag carried a choice list");
		for (long, choices) in &mut found {
			choices.sort_unstable();
			assert_eq!(choices, &expected, "--{long} is out of step with moq-net's versions");
		}
	}
}
