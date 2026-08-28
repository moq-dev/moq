//! Shell completion: which grammar answers the cursor, and the runtime completers
//! that answer values the static tables cannot know.
//!
//! Two halves. The plumbing decides whether a cursor belongs to the root grammar
//! or to a `--`-separated stage, because this binary splits argv itself (see
//! [`crate::args`]) and Usage's own interception only knows the root. The
//! completers answer the values that are only knowable at the prompt: the capture
//! sources this machine has, and the broadcasts and renditions the relay on the
//! line is carrying.
//!
//! A completion is not a command anybody ran, so nothing here reports a failure.
//! An unreachable relay, a refused session, or a budget that runs out all mean
//! "no candidates": a message in the prompt would be worse than a short list.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{CatalogFormat, Stream};
use tokio::time::{Instant, timeout_at};
use usage::complete::{Candidate, CompleteCtx, CompletionFuture, CompletionOverlay, CompletionRequest, Shell, render};
use usage::spec::{CommandArgs, ValueEnum};

use crate::args::{Cli, Export, MoqSide, Stage};
use crate::subscribe::CatalogFormatArg;

/// The wall-clock budget one network-backed completer gets, handshake included.
///
/// A ceiling on the whole exchange rather than a timeout per step: whatever has
/// arrived when it expires is the answer. Tab is pressed between keystrokes, so a
/// completer that outlives the user's patience is a shell that appears to hang,
/// and half a list now beats the whole list later.
const BUDGET: Duration = Duration::from_millis(500);

/// How long the announce sweep waits for a sibling after an announcement lands.
///
/// A relay sends its whole announced set back to back, so the gap between the
/// first and the last is a round trip, not a budget. Without this the sweep would
/// always cost [`BUDGET`], even when the answer arrived in a few milliseconds.
const SETTLE: Duration = Duration::from_millis(30);

/// The completers every build has, keyed by the *value* name they answer for.
///
/// The value name, not the flag: that is what Usage matches an overlay on, and the
/// derive spells it in screaming snake case (`--video-name <VIDEO_NAME>`), so a key
/// of `video-name` silently never fires. `every_overlay_names_a_declared_value` is
/// what keeps this table honest.
///
/// [`CommandSelector::Any`](usage::spec::CommandSelector::Any) throughout: every one
/// of these names means the same thing wherever it appears, and `--broadcast`
/// deliberately appears both before the verb and on a stage.
static NETWORK: &[CompletionOverlay<'static>] = &[
	CompletionOverlay::async_any("BROADCAST", broadcasts),
	CompletionOverlay::async_any("VIDEO_NAME", video_names),
	CompletionOverlay::async_any("AUDIO_NAME", audio_names),
];

/// The completers for `import capture`'s sources, which only that feature declares.
#[cfg(feature = "capture")]
static CAPTURE: &[CompletionOverlay<'static>] = &[
	CompletionOverlay::async_any("CAMERA", cameras),
	CompletionOverlay::async_any("DISPLAY", displays),
	CompletionOverlay::async_any("WINDOW", windows),
	CompletionOverlay::async_any("APP", apps),
	CompletionOverlay::async_any("MICROPHONE", microphones),
];

/// See [`CAPTURE`].
#[cfg(not(feature = "capture"))]
static CAPTURE: &[CompletionOverlay<'static>] = &[];

/// Every completer this build has. One allocation on the completion path only.
fn overlays() -> Vec<CompletionOverlay<'static>> {
	NETWORK.iter().chain(CAPTURE).copied().collect()
}

thread_local! {
	/// The process-wide MoQ flags of the line being completed.
	///
	/// A completer is a bare `fn` in a table Usage owns, so it cannot capture the
	/// request; and a cursor past a `--` is answered against the stage grammar,
	/// whose chunk has no `--connect` in it by construction. Parsing the globals
	/// once, here, is what lets a completer in either grammar reach them.
	///
	/// Thread-local rather than a process global because this is per-request state:
	/// [`answer`]'s future is `!Send` (Usage's completion futures are), so it can
	/// only ever be driven on the thread that started it.
	static GLOBALS: RefCell<Option<MoqSide>> = const { RefCell::new(None) };
}

/// Holds [`GLOBALS`] for one request and clears it on the way out.
struct Globals;

impl Globals {
	fn set(side: Option<MoqSide>) -> Self {
		GLOBALS.with_borrow_mut(|slot| *slot = side);
		Self
	}

	/// The globals this request parsed, or `None` when the line has none yet.
	fn get() -> Option<MoqSide> {
		GLOBALS.with_borrow(Clone::clone)
	}
}

impl Drop for Globals {
	fn drop(&mut self) {
		GLOBALS.with_borrow_mut(|slot| *slot = None);
	}
}

/// Answer a shell's completion request, against the grammar the cursor is in.
///
/// `None` for ordinary argv, which is what tells [`crate::args::Invocation::parse`]
/// that this is a real invocation.
///
/// The root spec is the globals plus the *first* stage. A cursor in a later chunk
/// answered against it offers `--connect` and the other process-wide flags, which a
/// stage refuses, so the request is rewritten to the active chunk and handed to
/// [`Stage`].
pub async fn answer(argv: &[OsString]) -> Option<String> {
	let request = CompletionRequest::parse(argv)?;
	// Dropped at the end of this call, so a second request cannot read the first's.
	let _globals = Globals::set(globals(&request));
	let overlays = overlays();

	// Everything past the cursor says nothing about the word being completed.
	let words = request.split.walked();
	// Strictly before the cursor: a cursor sitting *on* a `--` is typing the
	// separator itself, which is the root's business rather than a stage's.
	let staged = words[..request.split.cword]
		.iter()
		.rposition(|word| word == "--")
		.map(|at| at + 1);

	let answer = match staged {
		None => {
			Cli::app()
				.completion_app()
				.completions(&overlays)
				.complete_request(&request)
				.await
		}
		Some(start) => {
			// `words[0]` is read as the program name, so the chunk gets one of its own.
			let mut chunk = vec![words.first().cloned().unwrap_or_default()];
			chunk.extend_from_slice(&words[start..]);

			let mut request = request.clone();
			request.split.cword = chunk.len() - 1;
			request.split.words = chunk;
			Stage::app()
				.completion_app()
				.completions(&overlays)
				.complete_request(&request)
				.await
		}
	};

	Some(render(&answer, request.shell))
}

/// The process-wide MoQ flags, as far as the words before the cursor go.
///
/// Read from the first chunk, which is the only one that can carry them, and
/// through the real tables rather than by scanning for `--connect`: what a
/// completer dials has to be what an invocation would have dialed, TLS roots and
/// environment variables included.
fn globals(request: &CompletionRequest) -> Option<MoqSide> {
	let chunk = request.split.argv().split(|word| word == "--").next()?;
	let argv: Vec<&OsStr> = chunk.iter().map(OsStr::new).collect();

	let mut partial = <MoqSide as CommandArgs>::start();
	let mut parser = usage::Parser::new(Cli::command(), &argv);
	while let Some(event) = parser.next_event() {
		match event {
			Ok(event) => {
				<MoqSide as CommandArgs>::apply(&mut partial, &event);
			}
			// A line being completed is unfinished by definition, so an error means the
			// grammar ran out here; the partial holds what was understood before that.
			Err(_) => break,
		}
	}

	<MoqSide as CommandArgs>::apply_env(&mut partial);
	<MoqSide as CommandArgs>::apply_defaults(&mut partial);
	<MoqSide as CommandArgs>::build(partial).ok()
}

/// The `export` stage the cursor is in, as far as the line goes.
///
/// A stage's own `--broadcast` overrides the process-wide one, and `--video-name`
/// is only ever declared beside it, so a rendition completer has to read the stage
/// it was typed in rather than the globals alone.
fn export(ctx: &CompleteCtx<'_>) -> Option<<Export as CommandArgs>::Partial> {
	let declaration = <Export as CommandArgs>::COMMAND;
	let (command, words) = ctx.command_for(declaration)?;
	let argv: Vec<&OsStr> = words.iter().map(OsStr::new).collect();

	let mut partial = <Export as CommandArgs>::start();
	let mut parser = usage::Parser::new(command, &argv);
	while let Some(event) = parser.next_event() {
		match event {
			Ok(event) => {
				<Export as CommandArgs>::apply(&mut partial, &event);
			}
			Err(_) => break,
		}
	}
	Some(partial)
}

/// One raw partial value as text, or `None` when it was never given.
fn given(value: &Option<Vec<u8>>) -> Option<&str> {
	std::str::from_utf8(value.as_deref()?).ok()
}

// ------------------------------------------------------------------ script

/// `Usage` adapter for [`Shell`], which is a foreign type and so can't derive
/// `ValueEnum` itself.
#[derive(usage::ValueEnum, Clone, Copy)]
pub enum ShellArg {
	Bash,
	Elvish,
	Fish,
	Nu,
	#[usage(name = "powershell")]
	PowerShell,
	Zsh,
}

impl From<ShellArg> for Shell {
	fn from(shell: ShellArg) -> Self {
		match shell {
			ShellArg::Bash => Self::Bash,
			ShellArg::Elvish => Self::Elvish,
			ShellArg::Fish => Self::Fish,
			ShellArg::Nu => Self::Nu,
			ShellArg::PowerShell => Self::PowerShell,
			ShellArg::Zsh => Self::Zsh,
		}
	}
}

/// `moq completion`: the script that makes a shell ask this binary what fits at
/// the cursor.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// The shell to write the script for.
	#[usage(arg, value_enum)]
	pub shell: ShellArg,

	/// Write it where this shell looks for completions, instead of to stdout.
	#[usage(long)]
	pub install: bool,

	/// Replace a file already at that path that this binary did not write.
	#[usage(long, requires = "--install")]
	pub force: bool,
}

impl Args {
	/// Write the script to stdout, or install it and report where it went.
	///
	/// The script goes to stdout so it can be redirected; everything about the
	/// install goes to stderr, so `moq completion zsh > _moq` stays a script and not
	/// a script with a report on the end of it.
	pub fn run(self) -> anyhow::Result<()> {
		let shell = self.shell.into();
		if !self.install {
			print!("{}", Cli::completion_script(shell));
			return Ok(());
		}

		let on_foreign = match self.force {
			true => usage::install::OnForeign::Overwrite,
			false => usage::install::OnForeign::Refuse,
		};

		let installed = Cli::install_completion(shell, &usage::install::Env::from_process(), on_foreign)
			.context("failed to install the completion script")?;

		let wrote = match installed.wrote {
			usage::install::Wrote::Created => "wrote",
			usage::install::Wrote::Unchanged => "already current",
			usage::install::Wrote::Updated => "updated",
			usage::install::Wrote::Replaced => "replaced",
			_ => "installed",
		};
		eprintln!("{wrote} {}", installed.plan.path.display());

		// A shell that can't autoload the file needs a line in its own config, which
		// this never edits. Say it, rather than reporting success on an install that
		// does nothing yet. The snippet comes first and the reason after it, because
		// Usage's `why` is a paragraph and what you have to paste is one line.
		if let usage::install::Loading::Manual { line, file, why } = &installed.plan.loading {
			eprintln!("\nadd this to {file}:");
			for line in line.lines() {
				eprintln!("    {line}");
			}
			eprintln!("\n{why}");
		}

		Ok(())
	}
}

// ------------------------------------------------------------------ capture

/// Turn a capture enumeration into candidates, or nothing when the platform
/// cannot list that kind of source.
#[cfg(feature = "capture")]
fn sources<T, E>(found: Result<Vec<T>, E>, describe: impl Fn(&T) -> Candidate<'static>) -> Vec<Candidate<'static>> {
	found
		.map(|items| items.iter().map(describe).collect())
		.unwrap_or_default()
}

/// Complete `--camera` from the cameras this machine has.
///
/// Only the attached `--camera=<TAB>` form reaches this: the flag's value is
/// optional (bare `--camera` opens the default), so a detached word after it is
/// as likely to be the next flag, and Usage will not guess.
#[cfg(feature = "capture")]
fn cameras(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::cameras().await, |camera| {
			Candidate::described(camera.id.clone(), camera.name.clone())
		})
	})
}

/// Complete `--display` from the displays this machine has.
#[cfg(feature = "capture")]
fn displays(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::displays().await, |display| {
			Candidate::described(
				display.id.clone(),
				format!("{} ({}x{})", display.name, display.width, display.height),
			)
		})
	})
}

/// Complete `--window` from the windows this machine has open.
#[cfg(feature = "capture")]
fn windows(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::windows().await, |window| {
			let title = if window.title.is_empty() {
				"(untitled)"
			} else {
				&window.title
			};
			Candidate::described(window.id.clone(), format!("{} - {title}", window.app))
		})
	})
}

/// Complete `--app` from the applications this machine is running.
#[cfg(feature = "capture")]
fn apps(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::apps().await, |app| {
			Candidate::described(app.id.clone(), app.name.clone())
		})
	})
}

/// Complete `--microphone` from the audio inputs this machine has.
#[cfg(feature = "capture")]
fn microphones(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_audio::capture::devices().await, |device| match device.default {
			true => Candidate::described(device.id.clone(), "the default input"),
			false => Candidate::new(device.id.clone()),
		})
	})
}

// ------------------------------------------------------------------ network

/// Complete `--broadcast` from what the relay on the line announces.
///
/// Offered on an `import` as well as an `export`, even though an import is naming a
/// broadcast it is about to publish rather than one that exists: a redundant (1+1)
/// publisher deliberately reuses the name, and seeing what is already there is how
/// you avoid colliding with it by accident.
fn broadcasts(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		let Some(side) = Globals::get() else {
			return Vec::new();
		};
		let deadline = Instant::now() + BUDGET;
		let Some((origin, connection)) = dial(&side, deadline).await else {
			return Vec::new();
		};

		// Announce and unannounce arrive as separate updates for the same path, so
		// this tracks a set rather than appending: a broadcast that ends while the
		// sweep is running is one the user cannot name by the time they press enter.
		let mut announced = origin.consume().announced();
		let mut live = BTreeSet::new();
		// The first announcement gets the whole remaining budget; each one after it
		// only has to beat its siblings, which are already on the wire.
		let mut until = deadline;
		while let Ok(Some(update)) = timeout_at(until, announced.next()).await {
			until = deadline.min(Instant::now() + SETTLE);
			let path = update.path.to_string();
			// The root broadcast is the connection path itself, which an unset
			// `--broadcast` already names; there is no word to insert for it.
			if path.is_empty() {
				continue;
			}
			match update.broadcast.is_some() {
				true => live.insert(path),
				false => live.remove(&path),
			};
		}
		connection.close();

		live.into_iter().map(Candidate::new).collect()
	})
}

/// Complete `--video-name` from the broadcast's catalog.
fn video_names(ctx: CompleteCtx<'_>) -> CompletionFuture<'_> {
	Box::pin(async move {
		let Some(catalog) = renditions(&ctx).await else {
			return Vec::new();
		};
		catalog
			.video
			.renditions
			.iter()
			.map(|(name, config)| {
				let size = match (config.coded_width, config.coded_height) {
					(Some(width), Some(height)) => format!(" {width}x{height}"),
					_ => String::new(),
				};
				Candidate::described(name.clone(), format!("{}{size}", config.codec))
			})
			.collect()
	})
}

/// Complete `--audio-name` from the broadcast's catalog.
fn audio_names(ctx: CompleteCtx<'_>) -> CompletionFuture<'_> {
	Box::pin(async move {
		let Some(catalog) = renditions(&ctx).await else {
			return Vec::new();
		};
		catalog
			.audio
			.renditions
			.iter()
			.map(|(name, config)| {
				Candidate::described(
					name.clone(),
					format!("{} {} Hz {}ch", config.codec, config.sample_rate, config.channel_count),
				)
			})
			.collect()
	})
}

/// Dial the relay and read one catalog snapshot for the broadcast on the line.
async fn renditions(ctx: &CompleteCtx<'_>) -> Option<moq_mux::catalog::hang::Catalog> {
	let side = Globals::get()?;
	let stage = export(ctx);
	let stage = stage.as_ref();

	// A stage's own `--broadcast` wins over the process-wide one, exactly as it does
	// for the invocation this line is on its way to becoming.
	let path = stage
		.and_then(|stage| given(&stage.broadcast))
		.or(side.broadcast.as_deref())
		.unwrap_or_default()
		.to_string();

	let format = stage
		.and_then(|stage| given(&stage.catalog_format))
		.and_then(CatalogFormatArg::from_choice)
		.map(CatalogFormat::from)
		.or_else(|| CatalogFormat::detect(&path))
		.unwrap_or_default();

	let deadline = Instant::now() + BUDGET;
	let (origin, connection) = dial(&side, deadline).await?;
	let catalog = timeout_at(deadline, catalog(&origin, &path, format))
		.await
		.ok()
		.flatten();
	connection.close();
	catalog
}

/// Subscribe to a broadcast's catalog and return its first snapshot.
async fn catalog(
	origin: &moq_net::origin::Producer,
	path: &str,
	format: CatalogFormat,
) -> Option<moq_mux::catalog::hang::Catalog> {
	// `announced_broadcast` rather than a plain lookup: the announcement is still in
	// flight right after connecting, and asking on the spot reports a live broadcast
	// as unroutable.
	let consumer = origin.consume();
	consumer.announced_broadcast(path).await?;

	let mut stream = moq_mux::Source::new(consumer, path).catalog(format).await.ok()?;
	stream.next().await.ok()?
}

/// Open a throwaway subscribe-only session to the relay `--connect` names.
///
/// The origin id is fresh and random rather than the pinned `--origin`: this
/// session is not the publisher the user is about to start, and a shared id is
/// what tells a relay two sessions carry the same content.
async fn dial(side: &MoqSide, deadline: Instant) -> Option<(moq_net::origin::Producer, moq_tokio::Connection)> {
	let url = side.client.url.clone()?;
	let origin = moq_tokio::origin::spawn(moq_net::Origin::random());

	// No iroh endpoint: binding one is more setup than a keystroke should pay for,
	// so an `iroh://` peer completes nothing rather than completing slowly.
	let client = side
		.client
		.clone()
		.init(side.quic.clone())
		.ok()?
		.with_subscriber(origin.clone())
		.with_reconnect(false);

	let connection = timeout_at(deadline, client.connect(url).established())
		.await
		.ok()?
		.ok()?;
	Some((origin, connection))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Answer a whole line, with the cursor at its end.
	async fn complete(line: &str) -> Vec<String> {
		let argv: Vec<OsString> = ["__complete_word__", "--shell", "bash", "--line", line, "--cursor"]
			.iter()
			.map(OsString::from)
			.chain(std::iter::once(OsString::from(line.len().to_string())))
			.collect();

		answer(&argv)
			.await
			.unwrap_or_default()
			.lines()
			.map(str::to_string)
			.collect()
	}

	/// A relay serving `origin`, and the `--connect` flags that reach it.
	///
	/// Self-signed, so the line has to say `--connect-tls-insecure`. That is also the
	/// point: the completer builds its client from the same flags the invocation would
	/// have, so a line that can connect completes and one that cannot does not.
	fn relay(origin: &moq_net::origin::Producer) -> String {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let mut config = moq_tokio::listen::Config::default();
		config.bind = Some("127.0.0.1:0".to_string());
		config.tls.generate = vec!["localhost".to_string()];

		let server = config.init(Default::default()).expect("failed to bind listener");
		let port = server.local_addr().expect("no local addr").port();
		tokio::spawn(server.serve_publish(origin.consume()));

		format!("--connect moqt://127.0.0.1:{port} --connect-tls-insecure")
	}

	/// A cursor in a later stage is completed against the stage grammar.
	///
	/// The root spec is the globals plus the first stage, so answering a later chunk
	/// against it offers process-wide flags that the chunk refuses.
	#[tokio::test]
	async fn retargets_to_the_active_stage() {
		// A stage offers its own flags, and none of the globals it would refuse.
		let staged = complete("moq --connect http://x/y import fmp4 -- export fmp4 --").await;
		assert!(!staged.is_empty(), "a later stage completed nothing");
		for global in ["--connect", "--origin", "--broadcast"] {
			assert!(
				!staged.iter().any(|candidate| candidate == global),
				"{global} leaked into a stage that refuses it: {staged:?}"
			);
		}

		// The root still answers for itself.
		let root = complete("moq --conn").await;
		assert!(
			root.iter().any(|candidate| candidate == "--connect"),
			"root lost its globals: {root:?}"
		);

		// A cursor sitting on the separator is typing `--`, not inside a stage.
		assert!(complete("moq import fmp4 --").await.is_empty());
	}

	/// Every overlay names a value some command actually declares.
	///
	/// An overlay is matched by the value name, which the derive spells for itself, so
	/// a renamed field silently stops firing its completer. Nothing else would notice:
	/// a completer that never runs looks exactly like one that found nothing.
	#[test]
	fn every_overlay_names_a_declared_value() {
		fn declares(command: &usage::spec::CommandMeta<'_>, value: &str) -> bool {
			command
				.flags
				.iter()
				.any(|field| field.value_name.unwrap_or(field.flag.name).eq_ignore_ascii_case(value))
				|| command
					.args
					.iter()
					.any(|field| field.arg.name.eq_ignore_ascii_case(value))
				|| command.subcommands.iter().any(|sub| declares(sub, value))
		}

		for overlay in overlays() {
			assert!(
				declares(Cli::spec().root, overlay.value),
				"no flag or argument takes a value named `{}`, so its completer never runs",
				overlay.value
			);
		}
	}

	/// A completer that needs the network answers nothing when the line names no relay.
	///
	/// The gate is the whole reason tab-completion may dial at all: without a
	/// `--connect` on the line there is nothing to ask, and a keystroke must not open a
	/// connection the user did not name. An overlay that runs and finds nothing still
	/// suppresses the shell's path fallback, so an empty answer here is also evidence
	/// that the completer fired at all.
	#[tokio::test]
	async fn no_relay_on_the_line_means_no_dial() {
		for line in [
			"moq --broadcast ",
			"moq export --broadcast ",
			"moq export --video-name ",
			"moq export --audio-name ",
		] {
			assert!(complete(line).await.is_empty(), "{line:?} completed without a relay");
		}
	}

	/// Every shell this verb offers is one Usage can write a script for, spelled the
	/// way Usage spells it.
	///
	/// The adapter exists because [`Shell`] is foreign, so nothing but this ties the
	/// two spellings together: `powershell` is one word here and two variants apart.
	#[test]
	fn every_shell_choice_names_a_real_shell() {
		for choice in <ShellArg as ValueEnum>::CHOICES {
			let arg = ShellArg::from_choice(choice).expect("a declared choice");
			assert_eq!(
				Shell::from(arg).as_str(),
				*choice,
				"`{choice}` is not what Usage calls it"
			);
		}
	}

	/// The generated script asks this binary, under the name it ships as.
	#[test]
	fn the_script_registers_this_binary() {
		let script = Cli::completion_script(Shell::Zsh);
		assert!(
			script.starts_with("#compdef moq"),
			"{}",
			&script[..40.min(script.len())]
		);
		assert!(script.contains("__complete_word__"), "the script asks nothing");
	}

	/// `--broadcast` is answered from what the relay on the line announces.
	#[tokio::test]
	async fn a_relay_on_the_line_answers_broadcast() {
		let origin = moq_tokio::origin::spawn(moq_net::Origin::random());
		let route = moq_net::broadcast::Route::new().with_announce(true);
		let _alpha = origin.create_broadcast("alpha", route.clone()).expect("alpha");
		let _nested = origin.create_broadcast("room/beta", route).expect("beta");

		let connect = relay(&origin);
		let found = complete(&format!("moq {connect} --broadcast ")).await;
		assert!(found.contains(&"alpha".to_string()), "{found:?}");
		assert!(found.contains(&"room/beta".to_string()), "{found:?}");

		// A cursor past a `--` is answered against the stage grammar, whose chunk holds
		// no `--connect`: the completer still has to reach the relay the line named
		// before the separator.
		let staged = complete(&format!("moq {connect} import fmp4 -- export --broadcast ")).await;
		assert_eq!(staged, found, "a later stage lost the relay the globals named");
	}

	/// `--video-name` and `--audio-name` are answered from the catalog of the
	/// broadcast the stage names, which overrides the process-wide one.
	#[tokio::test]
	async fn a_stage_broadcast_picks_the_catalog_to_read() {
		use hang::catalog::{AudioCodec, AudioConfig, H264, VideoConfig};

		let origin = moq_tokio::origin::spawn(moq_net::Origin::random());
		let route = moq_net::broadcast::Route::new().with_announce(true);

		// Two broadcasts with different renditions, so a completer reading the wrong
		// one fails loudly instead of matching by luck.
		let mut keep = Vec::new();
		for (path, video, audio) in [("wanted", "hd", "stereo"), ("other", "sd", "mono")] {
			let mut broadcast = origin.create_broadcast(path, route.clone()).expect("broadcast");
			let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).expect("catalog");
			let mut edit = catalog.lock();
			edit.video.renditions.insert(
				video.to_string(),
				VideoConfig::new(H264 {
					profile: 0x42,
					constraints: 0,
					level: 0x1e,
					inline: false,
				}),
			);
			edit.audio
				.renditions
				.insert(audio.to_string(), AudioConfig::new(AudioCodec::Opus, 48_000, 2));
			edit.commit().expect("publish the catalog");
			keep.push((broadcast, catalog));
		}

		// The global names `other`; the stage overrides it, exactly as the invocation
		// this line is on its way to becoming would.
		let connect = relay(&origin);
		let line = format!("moq {connect} --broadcast other export --broadcast wanted");

		assert_eq!(complete(&format!("{line} --video-name ")).await, ["hd"]);
		assert_eq!(complete(&format!("{line} --audio-name ")).await, ["stereo"]);
	}
}
