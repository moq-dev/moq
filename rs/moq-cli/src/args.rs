//! The unified moq-cli argument surface.
//!
//! Grammar: `moq <MoQ side> <stage> [-- <stage>]...`, where a stage is
//! `<import|export> <endpoint> [endpoint opts]`, plus `moq <MoQ side> play` for
//! native playback.
//!
//! - The MoQ side (`--connect`, the `--listen*` transport binds, and
//!   `--cluster-lan`; all optional, at least one) attaches the shared Origin to
//!   the MoQ network, and comes before the first stage. They compose: dial a
//!   relay, accept incoming sessions, and mesh with the LAN all at once.
//! - `import` routes media INTO MoQ from one source; `export` routes it OUT to
//!   one sink. The verb fixes the data direction (and thus, for the
//!   bidirectional gateways, whether `--connect`/`--listen` push or pull).
//! - `devices` and `token` touch no network at all, so they're the verbs that take
//!   no MoQ side. That's why the requirement is enforced per-verb
//!   ([`MoqSide::validate`]) rather than by the parser: an argument group can't be
//!   conditional on the subcommand.
//! - The endpoint is one subcommand: a container format (`ts`, `fmp4`, ... read
//!   from stdin on import, written to stdout on export) or a gateway (`hls`,
//!   `rtmp`, `srt`, `rtc`). Exactly one per stage, so "which endpoint" is
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

use hang::moq_net;

use crate::publish::PublishFormat;
use crate::subscribe::{CatalogFormatArg, SubscribeFormat};

// The globals plus the first stage; later stages are parsed as a [`Stage`]. Keep
// the doc comment to one line: Usage renders the rest as `--help` body text, where
// rustdoc links read as noise.
/// moq-cli: a media router that wires endpoints onto a shared MoQ Origin.
#[derive(usage::Cli, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq", version = env!("VERSION"))]
#[usage(completion)]
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
#[derive(usage::Cli, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "moq")]
#[usage(completion)]
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

	/// The same attachment, built without consulting the environment.
	///
	/// Only [`MoqSide::reject`] reads it. A local verb refuses a MoQ side the user
	/// asked for, and an exported `MOQ_CONNECT` is not an ask: it is a standing
	/// setting for the publishing this shell usually does, and it would otherwise
	/// make `moq token` and `moq completion` fail for everyone who has one.
	pub typed: MoqSide,

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
	#[cfg_attr(not(test), allow(dead_code))]
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

impl Invocation {
	/// Parse the process arguments, exiting with Usage's rendered message on error.
	///
	/// Async because a completion request is answered first, and some completers
	/// dial the relay the line already names (see [`crate::complete`]).
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
		match Self::try_parse_from(args) {
			Ok(parsed) => parsed,
			Err(err) => err.exit(),
		}
	}

	/// Refuse a MoQ side on a verb that runs locally and takes none.
	///
	/// Answered from what the command line said, never from the environment; see
	/// [`Self::typed`] and `MoqSide::reject`.
	pub fn reject(&self, command: &str) -> anyhow::Result<()> {
		self.typed.reject(command)
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
		let typed = MoqSide::from_argv(&first, Environment::Ignore).unwrap_or_else(|| cli.moq.clone());

		let mut stages = vec![cli.command];
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
			stages.push(
				Stage::parse_from(&chunk)
					.map_err(|err| parse_error(Stage::spec(), Stage::command(), &chunk, err))?
					.command,
			);
		}

		// Before anything reads the config: a released spelling parses into a hidden
		// field that nothing honors, so continuing would run on settings the command
		// line never asked for. Every stage is in by now, since a stage can carry a
		// config of its own. A Usage error, since that is what this is.
		let mut deprecated = cli.moq.deprecated();
		for stage in &stages {
			deprecated.extend(stage.deprecated());
		}
		if !deprecated.is_empty() {
			return Err(ParseError::new(
				ParseErrorKind::ValueValidation,
				format!("error: {deprecated}\n"),
			));
		}

		Ok(Self {
			log: cli.log,
			moq: cli.moq,
			typed,
			stages,
		})
	}

	/// Reject the stage combinations a single process can't run.
	///
	/// Called before anything binds a port or dials out, so a refused invocation has
	/// no side effects to unwind.
	pub fn validate(&self) -> anyhow::Result<()> {
		// One stage is what the CLI has always run, so nothing below can bite.
		if self.stages.len() == 1 {
			return Ok(());
		}

		// Only `import` and `export` share an Origin. The rest own the process: `play`
		// drives a window on the main thread, `transcode` builds its own Origin, and
		// `token` / `devices` never touch the network at all.
		if let Some(command) = self.stages.iter().find(|command| !command.is_stageable()) {
			anyhow::bail!(
				"`{}` must be the only verb; it can't share a process with another `--` stage",
				command.name()
			);
		}

		// Rate control assumes it owns the uplink: the encoder targets a fraction of the
		// connection's estimate, leaving room for its own audio and transport overhead but
		// not for a second publisher. Anything else importing over the same connection
		// spends what that encoder already claimed, so refuse rather than congest the link
		// the estimate exists to protect. Exports only receive, so they don't count, and
		// only an outbound client has an estimate at all.
		let imports = self
			.stages
			.iter()
			.filter(|stage| matches!(stage, Command::Import(_)))
			.count();
		let adaptive = self
			.stages
			.iter()
			.any(|stage| matches!(stage, Command::Import(import) if import.source.uses_bandwidth()));
		anyhow::ensure!(
			self.moq.client.url.is_none() || !adaptive || imports == 1,
			"a stage that encodes to fit the connection's bandwidth estimate assumes it's the only \
			 publisher on that connection, but this runs {imports} import stages; run them as separate \
			 processes, or publish over --listen, which has no estimate"
		);

		Ok(())
	}
}

/// Turn a Usage parse result into a [`ParseError`].
///
/// The rendering lives in [`moq_tokio::cli::answer`], shared with moq-relay and
/// moq-bench, which parse more than once for their own reasons. This adds the
/// failure category, which only this crate's callers ask about.
fn parse_error(
	spec: &usage::argv::spec::Spec<'_>,
	root: &usage::Command<'_>,
	argv: &[&OsStr],
	err: usage::Error<'_, '_>,
) -> ParseError {
	let kind = match &err {
		usage::Error::Help { .. } | usage::Error::HelpAll { .. } => ParseErrorKind::DisplayHelp,
		usage::Error::Version { .. } => ParseErrorKind::DisplayVersion,
		usage::Error::UnknownFlag { .. } | usage::Error::UnexpectedArg { .. } => ParseErrorKind::UnknownArgument,
		usage::Error::MissingSubcommand | usage::Error::MissingArgsHelp { .. } => ParseErrorKind::MissingSubcommand,
		usage::Error::InvalidValue(_) | usage::Error::InvalidChoice { .. } => ParseErrorKind::ValueValidation,
		_ => ParseErrorKind::Other,
	};
	ParseError::new(kind, moq_tokio::cli::answer(spec, root, argv, err).message())
}

/// Whether [`MoqSide::from_argv`] lets the environment fill what the words left out.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Environment {
	/// Apply the `MOQ_*` variables, as an ordinary parse does.
	Read,
	/// Read only what the words say, which is what "the user asked for this" means.
	Ignore,
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
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct MoqSide {
	/// The default broadcast name for every stage that doesn't name its own.
	///
	/// Optional for the point endpoints (stdin/stdout, HLS import, and the
	/// `--connect` dials), which default to the root broadcast at the connection
	/// path; required by the `--listen` endpoints and `hls export`, which bridge one
	/// named broadcast.
	#[usage(long, alias = "name", help_heading = "MoQ")]
	pub broadcast: Option<String>,

	/// Fix this process's Hop ID instead of minting a fresh random one.
	///
	/// The Hop ID is the first hop of every announcement this process
	/// publishes, and relays treat it as the broadcast's content identity:
	/// redundant publishers of the same broadcast share an id so relays fail
	/// over between them at a group boundary. Leave unset outside a redundant
	/// (1+1) chain; the default fresh id per run is what makes a restarted
	/// publisher look like new content instead of silently splicing.
	#[usage(long, env = "MOQ_HOP", help_heading = "MoQ")]
	pub hop: Option<u64>,

	/// The released spelling of [`Self::hop`], kept in the parser only so a
	/// process that still passes it is told what to pass instead.
	#[usage(name = "origin", long = "origin", env = "MOQ_ORIGIN", hide = true)]
	pub origin: Option<u64>,

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
	pub iroh: moq_tokio::iroh::EndpointConfig,

	/// LAN clustering config (`--cluster-lan`, `--cluster-lan-secret`).
	#[cfg(feature = "cluster-lan")]
	#[usage(flatten)]
	pub cluster: crate::cluster::Args,
}

impl MoqSide {
	/// Every released spelling this invocation used, across all three sections.
	fn deprecated(&self) -> moq_tokio::Deprecated {
		let mut found = self.client.deprecated();
		found.extend(self.quic.deprecated());
		found.extend(self.server.deprecated());
		if self.origin.is_some() {
			found.flag("--origin", Some("MOQ_ORIGIN"), "--hop / MOQ_HOP");
		}
		found
	}

	/// Mint the origin all broadcasts route through, identified by the pinned
	/// `--hop` id when set and a fresh random one otherwise.
	pub fn origin(&self) -> anyhow::Result<moq_net::origin::Producer> {
		use anyhow::Context;
		Ok(moq_tokio::origin::spawn(match self.hop {
			Some(id) => moq_net::Hop::new(id).with_context(|| format!("invalid --hop {id}"))?,
			None => moq_net::Hop::random(),
		}))
	}

	/// Whether `--cluster-lan` asked this process to mesh over the LAN.
	pub fn lan(&self) -> bool {
		#[cfg(feature = "cluster-lan")]
		return self.cluster.enabled();
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
			config.bind.get_or_insert_with(|| "[::]:0".to_string());
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

	/// Reject a verb that needs the MoQ network but was given no way to reach it.
	/// Stands in for the Usage `required` the `moq` group can't carry, since
	/// `devices` is exempt.
	pub fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.client.url.is_some() || self.serves(),
			"a MoQ side is required: pass --connect <url> to dial a relay, a --listen option to self-host, or --cluster-lan to mesh over the LAN"
		);
		#[cfg(feature = "cluster-lan")]
		{
			self.cluster.validate()?;
			if self.lan() {
				crate::cluster::validate_versions(&self.client, &self.server_config())?;
			}
		}
		Ok(())
	}

	/// Build a [`MoqSide`] from one chunk of a command line, leniently.
	///
	/// Stops at the first thing the grammar cannot take, because the two callers are
	/// both looking at an incomplete line: a half-typed one being completed, and (via
	/// [`Environment::Ignore`]) a real one whose typed values are being separated from
	/// its ambient ones. Whatever was understood before that point is the answer.
	pub(crate) fn from_argv(argv: &[&OsStr], environment: Environment) -> Option<Self> {
		use usage::spec::CommandArgs;

		let mut partial = <Self as CommandArgs>::start();
		let mut parser = usage::Parser::new(Cli::command(), argv);
		while let Some(event) = parser.next_event() {
			match event {
				Ok(event) => {
					<Self as CommandArgs>::apply(&mut partial, &event);
				}
				Err(_) => break,
			}
		}

		if environment == Environment::Read {
			<Self as CommandArgs>::apply_env(&mut partial);
		}
		<Self as CommandArgs>::apply_defaults(&mut partial);
		<Self as CommandArgs>::build(partial).ok()
	}

	/// Reject the MoQ flags on a verb that never touches the network, rather than
	/// silently ignoring them. `--broadcast` counts: a local verb has no content, and
	/// next to `token generate` it reads like it scopes the key, which `--root` does.
	///
	/// Private, and reached only through [`Invocation::reject`], so it cannot be asked
	/// of the resolved side: every one of these flags has a `MOQ_*` variable, and a
	/// shell that exports one for the publishing it usually does has not asked this
	/// verb for anything. A call site that picked the wrong view would read correctly
	/// and be wrong, so there is only one view to pick. `--hop` is in the list for
	/// the same reason it used to be out of it -- an ambient `MOQ_HOP` no longer
	/// reaches here, so a typed one can be refused like the rest.
	fn reject(&self, command: &str) -> anyhow::Result<()> {
		#[cfg(feature = "cluster-lan")]
		let cluster_secret = self.cluster.secret.is_some();
		#[cfg(not(feature = "cluster-lan"))]
		let cluster_secret = false;

		// A legacy `--client-connect` must be rejected here too; the fold has already
		// landed it in `url`.
		let ignored = [
			("--connect", self.client.url.is_some()),
			("--listen", self.server.bind.is_some()),
			("--listen-tcp-bind", self.server.tcp.bind.is_some()),
			("--cluster-lan", self.lan()),
			("--cluster-lan-secret", cluster_secret),
			("--broadcast", self.broadcast.is_some()),
			("--hop", self.hop.is_some()),
		];
		let ignored = ignored.into_iter().find(|(_, given)| *given).map(|(flag, _)| flag);
		#[cfg(unix)]
		let ignored = ignored
			.or_else(|| self.server.unix.bind.is_some().then_some("--listen-unix-bind"))
			.or_else(|| {
				let allow = &self.server.unix.allow;
				[
					("--listen-unix-allow-uid", !allow.uid.is_empty()),
					("--listen-unix-allow-gid", !allow.gid.is_empty()),
					("--listen-unix-allow-pid", !allow.pid.is_empty()),
				]
				.into_iter()
				.find_map(|(flag, given)| given.then_some(flag))
			});

		if let Some(flag) = ignored {
			anyhow::bail!("`{command}` runs locally and takes no MoQ side; drop {flag}");
		}

		Ok(())
	}
}

/// The verb: for `import`/`export` it is also the data direction, the pivot
/// between the MoQ side and the endpoint.
#[derive(usage::Subcommands, Clone)]
pub enum Command {
	/// Route media INTO MoQ from one source.
	///
	/// `alias_hidden`, not `alias`: Usage advertises an `alias` in help and
	/// completions, and `import` / `export` are the canonical spellings. The old
	/// names keep parsing without rejoining the published surface.
	#[usage(alias_hidden = "publish")]
	Import(Import),
	/// Route media OUT OF MoQ to one sink.
	#[usage(alias_hidden = "subscribe")]
	Export(Export),
	/// Play a broadcast in a native window and speaker.
	#[cfg(feature = "play")]
	Play(crate::play::Args),
	/// Re-encode `--broadcast` into a lower ladder, published next to it and
	/// only encoded while watched (just-in-time).
	#[cfg(feature = "transcode")]
	Transcode(crate::transcode::Args),
	/// Generate, sign, and verify the JWT tokens a relay authenticates with.
	Token(moq_token_cli::Args),
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
	fn deprecated(&self) -> moq_tokio::Deprecated {
		match self {
			Self::Export(export) => match &export.sink {
				ExportSink::Hls(hls) => hls.tls.deprecated(),
				_ => moq_tokio::Deprecated::default(),
			},
			_ => moq_tokio::Deprecated::default(),
		}
	}

	/// The verb as typed, for error messages.
	pub fn name(&self) -> &'static str {
		match self {
			Self::Import(_) => "import",
			Self::Export(_) => "export",
			#[cfg(feature = "play")]
			Self::Play(_) => "play",
			#[cfg(feature = "transcode")]
			Self::Transcode(_) => "transcode",
			Self::Token(_) => "token",
			Self::Completion(_) => "completion",
			#[cfg(feature = "capture")]
			Self::Devices => "devices",
		}
	}

	/// Whether this verb can share a process (and an Origin) with other stages.
	pub fn is_stageable(&self) -> bool {
		matches!(self, Self::Import(_) | Self::Export(_))
	}

	/// The broadcast this stage names, falling back to the process-wide `--broadcast`.
	///
	/// Empty means the root broadcast: MoQ names each broadcast by the connection
	/// path plus any explicit `--broadcast`, so an unset name is the connection path
	/// itself.
	pub fn broadcast(&self, moq: &MoqSide) -> String {
		let stage = match self {
			Self::Import(import) => import.broadcast.as_deref(),
			Self::Export(export) => export.broadcast.as_deref(),
			_ => None,
		};

		stage.or(moq.broadcast.as_deref()).unwrap_or_default().to_string()
	}
}

// ------------------------------------------------------------------ import

/// import = one source -> MoQ.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Import {
	/// The broadcast this stage publishes, overriding the process-wide `--broadcast`.
	///
	/// Required when a process imports more than one broadcast; a single stage can
	/// keep naming it before the verb.
	#[usage(long, alias = "name")]
	pub broadcast: Option<String>,

	/// How long relays keep a non-latest group of the published media tracks fetchable,
	/// e.g. "30s" or "5s". Defaults to hang's 30s.
	///
	/// A RETENTION budget, not a delivery one: it never makes a subscriber play further behind
	/// live, it caps how far back a FETCH can still reach (and how long a subscriber may ask to
	/// wait for a late group). The default suits a segmented egress (HLS/DASH), which may only
	/// advertise segments that are still fetchable; lower it when nothing reads history and the
	/// memory matters. Media tracks only -- the catalog and timeline are read at the live edge,
	/// which is retained unconditionally.
	#[usage(long = "latency-max")]
	pub max_age: Option<moq_tokio::Duration>,

	/// The single source feeding the Origin.
	#[usage(subcommand)]
	pub source: ImportSource,
}

/// The single source feeding the Origin on an import. The container formats read
/// from stdin; the gateways bridge another protocol.
#[derive(usage::Subcommands, Clone)]
pub enum ImportSource {
	/// Raw H.264 Annex-B from stdin.
	Avc3,
	/// Fragmented MP4 / CMAF from stdin.
	Fmp4,
	/// MPEG-TS from stdin.
	Ts,
	/// FLV / RTMP container from stdin.
	Flv,
	/// Pull a remote HLS / LL-HLS playlist (http/https URL or local file) into MoQ.
	Hls(crate::hls::ImportArgs),
	/// RTMP: pull a remote play (`--connect`) or accept incoming publishes (`--listen`).
	Rtmp(crate::rtmp::Args),
	/// SRT: pull a remote stream (`--connect`) or accept incoming publishes (`--listen`).
	Srt(crate::srt::Args),
	/// WebRTC: WHEP client pulling a remote (`--connect`) or WHIP server accepting publishes (`--listen`).
	Rtc(crate::rtc::Args),
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
			Self::Fmp4 => PublishFormat::Fmp4,
			Self::Ts => PublishFormat::Ts,
			Self::Flv => PublishFormat::Flv,
			_ => return None,
		})
	}
	/// Whether this source encodes to fit the connection's bandwidth estimate.
	///
	/// Rate control is per-encoder while the estimate is per-connection, so each such
	/// source assumes it's the only one on the uplink. Only the video encoder reads
	/// the estimate, so an audio-only capture doesn't count.
	pub fn uses_bandwidth(&self) -> bool {
		match self {
			#[cfg(feature = "capture")]
			Self::Capture(capture) => !capture.no_video,
			_ => false,
		}
	}
}

// ------------------------------------------------------------------ export

/// export = MoQ -> one sink.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Export {
	/// The broadcast this stage subscribes to, overriding the process-wide `--broadcast`.
	///
	/// Required when a process exports more than one broadcast; a single stage can
	/// keep naming it before the verb.
	#[usage(long, alias = "name")]
	pub broadcast: Option<String>,

	/// Catalog format to read for track discovery (default: detect from the broadcast suffix).
	#[usage(long = "catalog-format", value_enum)]
	pub catalog_format: Option<CatalogFormatArg>,

	/// Rendition selection (`--video-name`, `--video-codec`, `--audio-name`, `--audio-codec`).
	#[usage(flatten)]
	pub select: crate::subscribe::SelectArgs,

	/// The single sink draining the Origin.
	#[usage(subcommand)]
	pub sink: ExportSink,
}

/// The single sink draining the Origin on an export. The container formats write
/// to stdout; the gateways bridge another protocol.
#[derive(usage::Subcommands, Clone)]
pub enum ExportSink {
	/// Fragmented MP4 / CMAF to stdout.
	Fmp4(Fragmented),
	/// Matroska / WebM to stdout.
	Mkv(Fragmented),
	/// MPEG-TS to stdout.
	Ts(Container),
	/// FLV / RTMP container to stdout.
	Flv(Container),
	/// H.264 Annex-B elementary stream to stdout.
	H264(Container),
	/// H.265 Annex-B elementary stream to stdout.
	H265(Container),
	/// Serve HLS / LL-HLS and DASH over HTTP.
	Hls(crate::hls::ExportArgs),
	/// RTMP: push to a remote (`--connect`) or serve plays (`--listen`).
	Rtmp(crate::rtmp::ExportArgs),
	/// SRT: push to a remote (`--connect`) or serve requests (`--listen`).
	Srt(crate::srt::Args),
	/// WebRTC: WHIP client pushing to a remote (`--connect`) or WHEP server serving plays (`--listen`).
	Rtc(crate::rtc::Args),
}

impl ExportSink {
	/// The stdout container format plus its latency and fragment cap, when this
	/// sink writes to stdout (the container formats). The fragment cap is
	/// fmp4/mkv-only.
	pub fn stdout(&self) -> Option<(SubscribeFormat, std::time::Duration, Option<Duration>)> {
		Some(match self {
			Self::Fmp4(args) => (
				SubscribeFormat::Fmp4,
				args.container.max_age.into_std(),
				args.fragment_duration.map(moq_tokio::Duration::into_std),
			),
			Self::Mkv(args) => (
				SubscribeFormat::Mkv,
				args.container.max_age.into_std(),
				args.fragment_duration.map(moq_tokio::Duration::into_std),
			),
			Self::Ts(args) => (SubscribeFormat::Ts, args.max_age.into_std(), None),
			Self::Flv(args) => (SubscribeFormat::Flv, args.max_age.into_std(), None),
			Self::H264(args) => (SubscribeFormat::H264, args.max_age.into_std(), None),
			Self::H265(args) => (SubscribeFormat::H265, args.max_age.into_std(), None),
			_ => return None,
		})
	}
}

/// Options shared by every stdout container sink.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Container {
	/// Maximum latency before skipping a stalled group (e.g. `500ms`, `1s`).
	#[usage(long = "latency-max", default = "500ms")]
	pub max_age: moq_tokio::Duration,
}

/// The fmp4 / mkv stdout containers: [`Container`] plus a fragment cap.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Fragmented {
	#[usage(flatten)]
	pub container: Container,

	/// Cap the output fragment/cluster duration (e.g. `2s`). Default: one GOP.
	#[usage(long)]
	pub fragment_duration: Option<moq_tokio::Duration>,
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

	#[test]
	fn single_stage() {
		let cli = Invocation::try_parse_from(["moq", "--connect", "http://relay", "import", "ts"]).unwrap();
		assert_eq!(cli.stages.len(), 1);
		assert_eq!(cli.stages[0].name(), "import");
		assert!(cli.validate().is_ok());
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

	/// `moq-cli`'s own rename rides the same refusal as the flags it flattens from
	/// `moq-tokio`, and lands in the same message.
	///
	/// Both halves matter: `--origin` must not silently pin a Hop ID onto the field
	/// `--hop` now owns, and the migration has to name the environment variable too,
	/// since a deployment that sets `MOQ_ORIGIN` never typed the flag.
	#[test]
	fn the_released_origin_spelling_is_refused_with_a_migration() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--origin",
			"42",
			"--connect",
			"http://relay/anon",
			"export",
			"ts",
		]) else {
			panic!("--origin must not start a run");
		};

		let reported = err.to_string();
		assert!(
			reported.contains("--origin / MOQ_ORIGIN -> --hop / MOQ_HOP"),
			"missing the migration from {reported}"
		);
	}

	/// A stage carries config of its own, and the check has to reach it.
	///
	/// `export hls` flattens `tls::Listen`. Its listener decides whether to serve
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
		let cli =
			Invocation::try_parse_from(["moq", "--listen-tcp-bind", "127.0.0.1:0", "import", "ts"]).expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert!(cli.moq.serves());
		assert_eq!(cli.moq.server_config().tcp.bind, Some("127.0.0.1:0".parse().unwrap()));
	}

	#[cfg(unix)]
	#[test]
	fn unix_only_listener_is_a_moq_side() {
		let cli = Invocation::try_parse_from(["moq", "--listen-unix-bind", "/tmp/moq-cli.sock", "export", "ts"])
			.expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert!(cli.moq.serves());
		assert_eq!(
			cli.moq.server_config().unix.bind.as_deref(),
			Some(std::path::Path::new("/tmp/moq-cli.sock"))
		);
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
			"token",
			"generate",
			"--algorithm",
			"ES256",
		])
		.unwrap();

		let err = cli.validate().unwrap_err().to_string();
		assert!(err.contains("token"), "{err}");
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

	/// Stages that never read the estimate can share a connection freely, so the guard
	/// must not reject them.
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

	/// An encoder that follows the estimate targets most of it, so a second publisher
	/// on the same connection spends what it already claimed. Exports only receive, and
	/// a `--listen` publisher has no estimate to oversubscribe.
	#[cfg(feature = "capture")]
	#[test]
	fn an_adaptive_capture_must_be_the_only_import() {
		let client: &[&str] = &["--connect", "http://relay"];
		let server: &[&str] = &["--listen", "[::]:4443"];

		let cases: [(&[&str], &[&str], bool); 3] = [
			// Two video captures follow the same estimate.
			(client, &["import", "capture"], false),
			// A fixed-rate import spends the same uplink from outside the budget.
			(client, &["import", "rtmp", "--listen", "127.0.0.1:1935"], false),
			// No outbound client, so there's no estimate to oversubscribe.
			(server, &["import", "capture"], true),
		];

		for (side, second, ok) in cases {
			let argv = [&["moq"][..], side, &["import", "capture", "--"], second].concat();
			let cli = Invocation::try_parse_from(argv.clone()).unwrap();
			assert_eq!(cli.validate().is_ok(), ok, "{argv:?}");
		}

		// An audio-only capture never reads the estimate, so it may share the connection.
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"import",
			"capture",
			"--no-video",
			"--",
			"import",
			"rtmp",
			"--listen",
			"127.0.0.1:1935",
		])
		.unwrap();
		assert!(cli.validate().is_ok());

		// Exports only receive, so they don't compete for the uplink.
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"import",
			"capture",
			"--",
			"export",
			"--broadcast",
			"other.hang",
			"fmp4",
		])
		.unwrap();
		assert!(cli.validate().is_ok());
	}

	/// Only the video encoder reads the connection's bandwidth estimate, so an
	/// audio-only capture doesn't compete for it and isn't counted against the
	/// one-adaptive-stage limit.
	#[cfg(feature = "capture")]
	#[test]
	fn audio_only_capture_is_not_bandwidth_adaptive() {
		for (args, adaptive) in [(vec!["capture"], true), (vec!["capture", "--no-video"], false)] {
			let argv = [vec!["moq", "--connect", "http://relay", "import"], args].concat();
			let cli = Invocation::try_parse_from(argv).unwrap();
			let Command::Import(import) = &cli.stages[0] else {
				panic!("expected import")
			};
			assert_eq!(import.source.uses_bandwidth(), adaptive);
		}
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
		let cli = Invocation::try_parse_from(["moq", "import", "--latency-max", "5s", "ts"]).unwrap();
		let Command::Import(import) = &cli.stages[0] else {
			panic!("expected import")
		};
		assert_eq!(import.max_age, Some(Duration::from_secs(5).into()));

		let cli = Invocation::try_parse_from([
			"moq",
			"import",
			"--latency-max",
			"5s",
			"rtmp",
			"--listen",
			"127.0.0.1:1935",
		])
		.unwrap();
		let Command::Import(import) = &cli.stages[0] else {
			panic!("expected import")
		};
		assert_eq!(import.max_age, Some(Duration::from_secs(5).into()));
	}

	#[test]
	fn token_verb() {
		let cli = Invocation::try_parse_from(["moq", "token", "generate", "--algorithm", "ES256"]).unwrap();
		assert!(matches!(cli.stages[0], Command::Token(_)));
		// Local verb: it needs no MoQ side, so what every other verb demands...
		assert!(cli.moq.validate().is_err());
		assert!(cli.moq.reject("token").is_ok());

		// ...these it refuses, rather than accepting the flag and ignoring it.
		for (flag, value, reported) in [
			("--connect", "https://relay.example.com", "--connect"),
			("--listen-tcp-bind", "127.0.0.1:0", "--listen-tcp-bind"),
			("--broadcast", "room", "--broadcast"),
		] {
			let cli = Invocation::try_parse_from(["moq", flag, value, "token", "generate"]).unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
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
				let cli = Invocation::try_parse_from(["moq", flag, value, "token", "generate"]).unwrap();
				let err = cli.moq.reject("token").unwrap_err().to_string();
				assert!(err.contains(reported), "{err}");
			}
		}

		#[cfg(feature = "cluster-lan")]
		{
			let cli = Invocation::try_parse_from(["moq", "--cluster-lan", "token", "generate"]).unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
			assert!(err.contains("--cluster-lan"), "{err}");

			// The parser considers the secret's `requires` satisfied when the boolean flag
			// is explicitly present but false. The local verb still has to reject the
			// otherwise silently ignored secret.
			let cli = Invocation::try_parse_from([
				"moq",
				"--cluster-lan=false",
				"--cluster-lan-secret",
				"cluster.key",
				"token",
				"generate",
			])
			.unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
			assert!(err.contains("--cluster-lan-secret"), "{err}");
		}
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
		assert_eq!(server.bind.as_deref(), Some("[::]:0"), "an ephemeral port");
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
		assert_eq!(server.bind.as_deref(), Some("[::]:4443"));
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
		let err = Invocation::try_parse_from(["moq", "--cluster-lan-secret", "cluster.key", "import", "ts"])
			.err()
			.expect("the secret must require --cluster-lan")
			.to_string();
		assert!(err.contains("cluster-lan"), "{err}");

		// `--cluster-lan=false` satisfies Usage's `requires` (the flag is present),
		// so the real check lives in `validate`.
		let cli = Invocation::try_parse_from([
			"moq",
			"--cluster-lan=false",
			"--cluster-lan-secret",
			"cluster.key",
			"--connect",
			"https://relay.example.com",
			"import",
			"ts",
		])
		.expect("parse");
		let err = cli.moq.validate().unwrap_err().to_string();
		assert!(err.contains("--cluster-lan=true"), "{err}");

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
		assert_eq!(cli.moq.cluster.secret.as_deref(), Some("cluster.key"));
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
		assert_eq!(play.max_age, Duration::from_millis(500));
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
			let err = play.validate().unwrap_err().to_string();
			assert!(err.contains(codec), "{err}");
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

	/// Every `*-version` flag offers exactly the versions [`Version::names`] parses.
	///
	/// The lists are `choices(...)` literals because Usage reads them at expansion
	/// time, so they are copies. This is what keeps a new protocol draft from
	/// parsing through `FromStr` while staying unreachable from the command line:
	/// add the draft, and this fails until every list has it.
	#[test]
	fn version_choices_match_the_parser() {
		fn walk<'a>(cmd: &'a usage::argv::spec::CommandMeta<'a>, found: &mut Vec<(&'a str, Vec<&'a str>)>) {
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

		// As a set: `names()` is preference-ordered (newest first) while a choice
		// list reads ascending, and that ordering is a presentation call.
		let mut expected: Vec<&str> = moq_net::Version::names().collect();
		expected.sort_unstable();
		let mut found = Vec::new();
		walk(Cli::spec().root, &mut found);

		assert!(!found.is_empty(), "no version flag carried a choice list");
		for (long, choices) in &mut found {
			choices.sort_unstable();
			assert_eq!(choices, &expected, "--{long} is out of step with Version::names()");
		}
	}
}
