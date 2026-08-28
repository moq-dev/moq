//! The unified moq-cli argument surface.
//!
//! Grammar: `moq <MoQ side> <stage> [-- <stage>]...`, where a stage is
//! `<import|export> <endpoint> [endpoint opts]`, plus `moq <MoQ side> play` for
//! native playback.
//!
//! - The MoQ side (`--client-connect` or one of the `--server-*-bind` options,
//!   all optional, at least one) attaches the shared Origin to the MoQ network,
//!   and comes before the first stage. They may be combined to dial a relay and
//!   accept incoming sessions.
//! - `import` routes media INTO MoQ from one source; `export` routes it OUT to
//!   one sink. The verb fixes the data direction (and thus, for the
//!   bidirectional gateways, whether `--connect`/`--listen` push or pull).
//! - `devices` and `token` touch no network at all, so they're the verbs that take
//!   no MoQ side. That's why the requirement is enforced per-verb
//!   ([`MoqSide::validate`]) rather than by clap: an `ArgGroup` can't be
//!   conditional on the subcommand.
//! - The endpoint is one subcommand: a container format (`ts`, `fmp4`, ... read
//!   from stdin on import, written to stdout on export) or a gateway (`hls`,
//!   `rtmp`, `srt`, `rtc`). Exactly one per stage, so "which endpoint" is
//!   unambiguous and there's no silently-ignored flag.
//! - `--` starts another stage on the same Origin and the same MoQ attachment, so
//!   one process can bridge several broadcasts (or both directions at once). clap
//!   can't express a repeated subcommand, so [`Invocation`] splits argv on `--`
//!   and runs each chunk through a real parser: every stage keeps full validation
//!   and its own `--help`. That claims `--` from clap, which would otherwise treat
//!   it as the end-of-options marker. The only positional it could have escaped is
//!   an `import hls` playlist path starting with `-`, which `./-name` covers, so
//!   the separator stays unconditional rather than context-sensitive.

use std::ffi::{OsStr, OsString};
use std::time::Duration;

use clap::{ArgGroup, Args, CommandFactory, Parser, Subcommand};
use hang::moq_net;

use crate::publish::PublishFormat;
use crate::subscribe::{CatalogFormatArg, SubscribeFormat};

// The globals plus the first stage; later stages are parsed as a [`Stage`]. Keep
// the doc comment to one line: clap renders the rest as `--help` body text, where
// rustdoc links read as noise.
/// moq-cli: a media router that wires endpoints onto a shared MoQ Origin.
#[derive(Parser, Clone)]
#[command(name = "moq", version = env!("VERSION"))]
#[command(after_help = "Separate additional import/export stages with `--`; they share one \
                        connection and one Origin. Every `--` starts a stage, so it is not an \
                        end-of-options marker: write a path starting with `-` as `./-name`.")]
pub struct Cli {
	/// Logging configuration.
	#[command(flatten)]
	pub log: moq_native::Log,

	/// The MoQ attachment, shared by both directions.
	#[command(flatten)]
	pub moq: MoqSide,

	/// The verb and endpoint.
	#[command(subcommand)]
	pub command: Command,
}

// `no_binary_name` because the chunk after a `--` starts at the verb, and the
// globals are deliberately absent: `--client-connect` past the first stage would
// read like it scopes that stage, when there is only ever one connection. As with
// [`Cli`], the doc comment stays one line because clap shows it in `--help`.
/// A stage after the first: the verb and endpoint, without the globals.
#[derive(Parser, Clone)]
#[command(name = "moq", no_binary_name = true)]
pub struct Stage {
	/// The verb and endpoint.
	#[command(subcommand)]
	pub command: Command,
}

/// The whole command line: the globals plus one or more `--`-separated stages.
pub struct Invocation {
	/// Logging configuration.
	pub log: moq_native::Log,

	/// The MoQ attachment, shared by every stage.
	pub moq: MoqSide,

	/// The stages, in the order given. Never empty.
	pub stages: Vec<Command>,
}

impl Invocation {
	/// Parse the process arguments, exiting with clap's own message on error.
	pub fn parse() -> Self {
		match Self::try_parse_from(std::env::args_os()) {
			Ok(parsed) => parsed,
			Err(err) => err.exit(),
		}
	}

	/// Split `argv` on `--` and run each chunk through a real parser.
	pub fn try_parse_from<I, T>(argv: I) -> Result<Self, clap::Error>
	where
		I: IntoIterator<Item = T>,
		T: Into<OsString>,
	{
		let argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
		let mut chunks = argv.split(|arg| arg == OsStr::new("--"));

		// `split` always yields at least one chunk, even for an empty argv; clap then
		// reports the missing subcommand as usual.
		let cli = Cli::try_parse_from(chunks.next().unwrap_or_default())?;

		let mut stages = vec![cli.command];
		for chunk in chunks {
			// A trailing or doubled `--` leaves an empty chunk, which clap would report as a
			// bare missing-subcommand usage dump. Name what's actually wrong instead.
			if chunk.is_empty() {
				return Err(Stage::command().error(
					clap::error::ErrorKind::MissingSubcommand,
					"`--` starts another stage, so it must be followed by `import` or `export`",
				));
			}

			stages.push(Stage::try_parse_from(chunk)?.command);
		}

		Ok(Self {
			log: cli.log,
			moq: cli.moq,
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
			self.moq.client.connect.is_none() || !adaptive || imports == 1,
			"a stage that encodes to fit the connection's bandwidth estimate assumes it's the only \
			 publisher on that connection, but this runs {imports} import stages; run them as separate \
			 processes, or publish over --server-bind, which has no estimate"
		);

		Ok(())
	}
}

/// The MoQ attachment. At least one client or server transport must be configured;
/// both may be given at once.
///
/// The group is not `required`, because the local verbs (`token`, `devices`) run
/// without a MoQ side. Every verb that does need one calls
/// [`validate`](Self::validate).
#[derive(Args, Clone)]
#[command(group = ArgGroup::new("moq").multiple(true).args(["client-connect", "server-bind"]))]
pub struct MoqSide {
	/// The default broadcast name for every stage that doesn't name its own.
	///
	/// Optional for the point endpoints (stdin/stdout, HLS import, and the
	/// `--connect` dials), which default to the root broadcast at the connection
	/// path; required by the `--listen` endpoints and `hls export`, which bridge one
	/// named broadcast.
	#[arg(long, alias = "name", help_heading = "MoQ")]
	pub broadcast: Option<String>,

	/// Fix this process's origin id instead of minting a fresh random one.
	///
	/// The origin id is the first hop of every announcement this process
	/// publishes, and relays treat it as the broadcast's content identity:
	/// redundant publishers of the same broadcast share an id so relays fail
	/// over between them at a group boundary. Leave unset outside a redundant
	/// (1+1) chain; the default fresh id per run is what makes a restarted
	/// publisher look like new content instead of silently splicing.
	#[arg(long, env = "MOQ_ORIGIN", help_heading = "MoQ")]
	pub origin: Option<u64>,

	/// MoQ client config (`--client-connect`, `--client-bind`, `--client-tls-*`, ...).
	#[command(flatten)]
	pub client: moq_native::ClientConfig,

	/// MoQ server transport config (`--server-bind`, `--server-tcp-bind`,
	/// `--server-unix-bind`, `--server-tls-*`, `--tls-*`).
	#[command(flatten)]
	pub server: moq_native::ServerConfig,

	/// Iroh transport config (`--iroh-*`), used by both the client and server.
	#[cfg(feature = "iroh")]
	#[command(flatten)]
	pub iroh: moq_native::iroh::EndpointConfig,
}

impl MoqSide {
	/// Mint the origin all broadcasts route through: the pinned `--origin` id
	/// when set, otherwise fresh and random.
	pub fn origin(&self) -> anyhow::Result<moq_net::origin::Producer> {
		use anyhow::Context;
		Ok(match self.origin {
			Some(id) => moq_net::Origin::new(id).with_context(|| format!("invalid --origin {id}"))?,
			None => moq_net::Origin::random(),
		}
		.produce())
	}

	/// Whether any inbound server transport is explicitly configured.
	pub fn serves(&self) -> bool {
		self.server.has_explicit_bind()
	}

	/// Reject a verb that needs the MoQ network but was given no way to reach it.
	/// Stands in for the clap `required` the `moq` group can't carry, since
	/// `devices` is exempt.
	pub fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.client.connect.is_some() || self.serves(),
			"a MoQ side is required: pass --client-connect <url> to dial a relay, or a --server-*-bind option to self-host"
		);
		Ok(())
	}

	/// Reject the MoQ flags on a verb that never touches the network, rather than
	/// silently ignoring them. `--broadcast` counts: a local verb has no content, and
	/// next to `token generate` it reads like it scopes the key, which `--root` does.
	///
	/// `--origin` is left out on purpose. It reads `MOQ_ORIGIN`, so rejecting it would
	/// fail `moq token` in any shell that exports the variable for a publisher, and an
	/// ambient env value is not the deliberate request this is meant to catch.
	pub fn reject(&self, command: &str) -> anyhow::Result<()> {
		let ignored = [
			("--client-connect", self.client.connect.is_some()),
			("--server-bind", self.server.bind.is_some()),
			("--server-tcp-bind", self.server.tcp.bind.is_some()),
			("--broadcast", self.broadcast.is_some()),
		];
		let ignored = ignored.into_iter().find(|(_, given)| *given).map(|(flag, _)| flag);
		#[cfg(unix)]
		let ignored = ignored
			.or_else(|| self.server.unix.bind.is_some().then_some("--server-unix-bind"))
			.or_else(|| {
				let allow = self.server.unix.allow.as_ref()?;
				[
					("--server-unix-allow-uid", !allow.uid.is_empty()),
					("--server-unix-allow-gid", !allow.gid.is_empty()),
					("--server-unix-allow-pid", !allow.pid.is_empty()),
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
#[derive(Subcommand, Clone)]
pub enum Command {
	/// Route media INTO MoQ from one source.
	#[command(alias = "publish")]
	Import(Import),
	/// Route media OUT OF MoQ to one sink.
	#[command(alias = "subscribe")]
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
	/// List the capture devices `import capture` can name.
	#[cfg(feature = "capture")]
	Devices,
}

impl Command {
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
#[derive(Args, Clone)]
pub struct Import {
	/// The broadcast this stage publishes, overriding the process-wide `--broadcast`.
	///
	/// Required when a process imports more than one broadcast; a single stage can
	/// keep naming it before the verb.
	#[arg(long, alias = "name")]
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
	#[arg(long, value_parser = humantime::parse_duration)]
	pub latency_max: Option<std::time::Duration>,

	/// The single source feeding the Origin.
	#[command(subcommand)]
	pub source: ImportSource,
}

/// The single source feeding the Origin on an import. The container formats read
/// from stdin; the gateways bridge another protocol.
#[derive(Subcommand, Clone)]
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

	/// Whether this source threads [`Import::latency_max`] into the catalog it publishes.
	///
	/// The stdin containers, HLS, and capture build their catalog in this crate, so they honor
	/// it. The remaining gateways build theirs inside moq-rtmp / moq-srt / moq-rtc, which take
	/// no retention yet -- so the flag is REFUSED there rather than silently ignored.
	pub fn honors_latency_max(&self) -> bool {
		if self.stdin_format().is_some() {
			return true;
		}
		match self {
			Self::Hls(_) => true,
			#[cfg(feature = "capture")]
			Self::Capture(_) => true,
			_ => false,
		}
	}
}

// ------------------------------------------------------------------ export

/// export = MoQ -> one sink.
#[derive(Args, Clone)]
pub struct Export {
	/// The broadcast this stage subscribes to, overriding the process-wide `--broadcast`.
	///
	/// Required when a process exports more than one broadcast; a single stage can
	/// keep naming it before the verb.
	#[arg(long, alias = "name")]
	pub broadcast: Option<String>,

	/// Catalog format to read for track discovery (default: detect from the broadcast suffix).
	#[arg(long = "catalog-format")]
	pub catalog_format: Option<CatalogFormatArg>,

	/// Rendition selection (`--video-name`, `--video-codec`, `--audio-name`, `--audio-codec`).
	#[command(flatten)]
	pub select: crate::subscribe::SelectArgs,

	/// The single sink draining the Origin.
	#[command(subcommand)]
	pub sink: ExportSink,
}

/// The single sink draining the Origin on an export. The container formats write
/// to stdout; the gateways bridge another protocol.
#[derive(Subcommand, Clone)]
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
	/// Serve HLS / LL-HLS over HTTP.
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
	pub fn stdout(&self) -> Option<(SubscribeFormat, Duration, Option<Duration>)> {
		Some(match self {
			Self::Fmp4(args) => (
				SubscribeFormat::Fmp4,
				args.container.latency_max,
				args.fragment_duration,
			),
			Self::Mkv(args) => (SubscribeFormat::Mkv, args.container.latency_max, args.fragment_duration),
			Self::Ts(args) => (SubscribeFormat::Ts, args.latency_max, None),
			Self::Flv(args) => (SubscribeFormat::Flv, args.latency_max, None),
			Self::H264(args) => (SubscribeFormat::H264, args.latency_max, None),
			Self::H265(args) => (SubscribeFormat::H265, args.latency_max, None),
			_ => return None,
		})
	}
}

/// Options shared by every stdout container sink.
#[derive(Args, Clone)]
pub struct Container {
	/// Maximum latency before skipping a stalled group (e.g. `500ms`, `1s`).
	#[arg(long = "latency-max", default_value = "500ms", value_parser = humantime::parse_duration)]
	pub latency_max: Duration,
}

/// The fmp4 / mkv stdout containers: [`Container`] plus a fragment cap.
#[derive(Args, Clone)]
pub struct Fragmented {
	#[command(flatten)]
	pub container: Container,

	/// Cap the output fragment/cluster duration (e.g. `2s`). Default: one GOP.
	#[arg(long, value_parser = humantime::parse_duration)]
	pub fragment_duration: Option<Duration>,
}

#[cfg(test)]
mod tests {
	use super::*;

	// Catches the conflicts clap only panics on at runtime: a duplicate long, a
	// dangling `conflicts_with`, a flattened arg colliding with an existing one.
	// The token verb flattens a whole command tree from another crate, so this is
	// the only thing standing between a rename there and a broken `moq`.
	#[test]
	fn valid() {
		Cli::command().debug_assert();
	}

	/// The `Stage` parser is a second entry point into the same command tree, so it
	/// needs the same conflict check as [`Cli`].
	#[test]
	fn valid_stage() {
		Stage::command().debug_assert();
	}

	#[test]
	fn single_stage() {
		let cli = Invocation::try_parse_from(["moq", "--client-connect", "http://relay", "import", "ts"]).unwrap();
		assert_eq!(cli.stages.len(), 1);
		assert_eq!(cli.stages[0].name(), "import");
		assert!(cli.validate().is_ok());
	}

	#[test]
	fn tcp_only_listener_is_a_moq_side() {
		let cli =
			Invocation::try_parse_from(["moq", "--server-tcp-bind", "127.0.0.1:0", "import", "ts"]).expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert!(cli.moq.serves());
		assert_eq!(cli.moq.server.tcp.bind, Some("127.0.0.1:0".parse().unwrap()));
	}

	#[cfg(unix)]
	#[test]
	fn unix_only_listener_is_a_moq_side() {
		let cli = Invocation::try_parse_from(["moq", "--server-unix-bind", "/tmp/moq-cli.sock", "export", "ts"])
			.expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert!(cli.moq.serves());
		assert_eq!(
			cli.moq.server.unix.bind.as_deref(),
			Some(std::path::Path::new("/tmp/moq-cli.sock"))
		);
	}

	/// The grammar clap can't express: one connection, several endpoints.
	#[test]
	fn multiple_stages() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--client-connect",
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
			cli.moq.client.connect.as_ref().map(ToString::to_string).as_deref(),
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
			"--client-connect",
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
		let cli = Invocation::try_parse_from(["moq", "--client-connect", "http://relay", "import", "ts"]).unwrap();
		assert_eq!(cli.stages[0].broadcast(&cli.moq), "");
	}

	/// Only import/export share an Origin; the rest own the process.
	#[test]
	fn rejects_unstageable_verbs() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--client-connect",
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

	/// Each stage is parsed by a real clap parser, so a typo past the first `--` is
	/// still a parse error rather than something swallowed as a positional.
	#[test]
	fn stage_errors_are_parse_errors() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--client-connect",
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

		assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
	}

	/// Splitting on `--` claims it from clap, so it can't also escape a positional
	/// starting with `-`. `./-name` is the documented way to write one.
	#[test]
	fn a_dash_prefixed_path_is_written_relative() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--client-connect",
			"http://relay",
			"import",
			"hls",
			"./-odd.m3u8",
		])
		.unwrap();

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
			vec!["moq", "--client-connect", "http://relay", "import", "ts", "--"],
			vec![
				"moq",
				"--client-connect",
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

			assert_eq!(err.kind(), clap::error::ErrorKind::MissingSubcommand);
			assert!(err.to_string().contains("must be followed by"), "{err}");
		}
	}

	/// The globals belong to the invocation, not a stage: there is only ever one
	/// connection, so accepting `--client-connect` again would be a lie.
	#[test]
	fn stages_reject_globals() {
		let Err(err) = Invocation::try_parse_from([
			"moq",
			"--client-connect",
			"http://relay",
			"import",
			"ts",
			"--",
			"--client-connect",
			"http://other",
			"import",
			"fmp4",
		]) else {
			panic!("expected a parse error")
		};

		assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
	}

	/// Stages that never read the estimate can share a connection freely, so the guard
	/// must not reject them.
	#[test]
	fn imports_without_rate_control_can_share_a_connection() {
		let cli = Invocation::try_parse_from([
			"moq",
			"--client-connect",
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
	/// a `--server-bind` publisher has no estimate to oversubscribe.
	#[cfg(feature = "capture")]
	#[test]
	fn an_adaptive_capture_must_be_the_only_import() {
		let client: &[&str] = &["--client-connect", "http://relay"];
		let server: &[&str] = &["--server-bind", "[::]:4443"];

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
			"--client-connect",
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
			"--client-connect",
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
			let argv = [vec!["moq", "--client-connect", "http://relay", "import"], args].concat();
			let cli = Invocation::try_parse_from(argv).unwrap();
			let Command::Import(import) = &cli.stages[0] else {
				panic!("expected import")
			};
			assert_eq!(import.source.uses_bandwidth(), adaptive);
		}
	}

	#[test]
	fn latency_max_is_unset_unless_asked_for() {
		// Unset rather than defaulted to hang's constant, so a source that cannot apply the
		// retention can tell "the user asked for one" from "nobody asked", and refuse only the
		// former. A `default_value` here would make an explicit `--latency-max 30s` on such a
		// source indistinguishable from the default, which is the silent no-op the guard exists
		// to stop.
		let cli = Cli::try_parse_from(["moq", "import", "ts"]).unwrap();
		let Command::Import(import) = cli.command else {
			panic!("expected import")
		};
		assert_eq!(import.latency_max, None);
		assert!(import.source.honors_latency_max());

		let cli = Cli::try_parse_from(["moq", "import", "--latency-max", "5s", "ts"]).unwrap();
		let Command::Import(import) = cli.command else {
			panic!("expected import")
		};
		assert_eq!(import.latency_max, Some(std::time::Duration::from_secs(5)));

		// The gateways build their catalogs in their own crates, so they cannot apply it.
		let cli = Cli::try_parse_from(["moq", "import", "rtmp", "--listen", "127.0.0.1:1935"]).unwrap();
		let Command::Import(import) = cli.command else {
			panic!("expected import")
		};
		assert!(!import.source.honors_latency_max());
	}

	#[test]
	fn token_verb() {
		let cli = Cli::try_parse_from(["moq", "token", "generate", "--algorithm", "ES256"]).unwrap();
		assert!(matches!(cli.command, Command::Token(_)));
		// Local verb: it needs no MoQ side, so what every other verb demands...
		assert!(cli.moq.validate().is_err());
		assert!(cli.moq.reject("token").is_ok());

		// ...these it refuses, rather than accepting the flag and ignoring it.
		for flag in [
			["--client-connect", "https://relay.example.com"],
			["--server-tcp-bind", "127.0.0.1:0"],
			["--broadcast", "room"],
		] {
			let cli = Cli::try_parse_from(["moq", flag[0], flag[1], "token", "generate"]).unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
			assert!(err.contains(flag[0]), "{err}");
		}

		#[cfg(unix)]
		for flag in [
			["--server-unix-bind", "/tmp/moq-cli.sock"],
			["--server-unix-allow-uid", "1000"],
			["--server-unix-allow-gid", "1000"],
			["--server-unix-allow-pid", "1000"],
		] {
			let cli = Cli::try_parse_from(["moq", flag[0], flag[1], "token", "generate"]).unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
			assert!(err.contains(flag[0]), "{err}");
		}
	}

	#[cfg(feature = "play")]
	#[test]
	fn play_verb() {
		let cli = Cli::try_parse_from([
			"moq",
			"--client-connect",
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
		let Command::Play(play) = cli.command else {
			panic!("expected play")
		};
		assert_eq!(play.latency_max, Duration::from_millis(500));
		assert_eq!(play.select.video_name.as_deref(), Some("hd"));
		assert!(cli.moq.validate().is_ok());
		assert!(play.validate().is_ok());
	}

	/// The selection flags are shared with the exports, which pass every codec
	/// through. Playback has to decode, so it rejects the rest up front instead
	/// of filtering the catalog down to a rendition that can't open.
	#[cfg(feature = "play")]
	#[test]
	fn play_rejects_undecodable_codecs() {
		for codec in ["vp8", "vp9"] {
			let cli = Cli::try_parse_from([
				"moq",
				"--client-connect",
				"https://relay.example.com/anon",
				"play",
				"--video-codec",
				codec,
			])
			.unwrap();
			let Command::Play(play) = cli.command else {
				panic!("expected play")
			};
			let err = play.validate().unwrap_err().to_string();
			assert!(err.contains(codec), "{err}");
		}
	}
}
