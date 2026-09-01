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
// Only the capture completers name a future by type; the rest are `async` blocks.
#[cfg(feature = "capture")]
use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{CatalogFormat, Stream};
use tokio::time::{Instant, timeout_at};
use usage::complete::{Candidate, CompleteCtx, CompletionFuture, CompletionOverlay, CompletionRequest, Shell, render};
use usage::spec::{CommandArgs, ValueEnum};

use crate::args::{Cli, Environment, Export, MoqSide, Stage};
use crate::subscribe::CatalogFormatArg;

/// The wall-clock budget one network-backed completer gets, handshake included.
///
/// A ceiling on the whole exchange rather than a timeout per step: whatever has
/// arrived when it expires is the answer. Tab is pressed between keystrokes, so a
/// completer that outlives the user's patience is a shell that appears to hang,
/// and half a list now beats the whole list later.
const BUDGET: Duration = Duration::from_millis(500);

/// The ceiling on a whole completion request, whatever it is answering.
///
/// A backstop, not the working budget: every completer bounds its own lookup by
/// [`BUDGET`], and this is deliberately looser so that theirs always fires first and
/// their partial answer survives. It exists so a completer added later cannot hang a
/// prompt by forgetting to bound itself, and so work that ignores cancellation
/// (a blocking device enumeration) still cannot hold the answer back.
const CEILING: Duration = Duration::from_millis(1_500);

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
/// of `video-name` silently never fires. `every_overlay_matches_only_what_it_answers_for`
/// is what keeps this table honest.
///
/// [`CommandSelector::Any`](usage::spec::CommandSelector::Any) throughout: every one
/// of these names means the same thing wherever it appears, and `--broadcast`
/// deliberately appears both before the verb and on a stage.
static NETWORK: &[CompletionOverlay<'static>] = &[
	CompletionOverlay::async_any("BROADCAST", broadcasts),
	CompletionOverlay::async_any("VIDEO_NAME", video_names),
	CompletionOverlay::async_any("AUDIO_NAME", audio_names),
];

/// The command whose source flags [`CAPTURE`] answers for.
#[cfg(feature = "capture")]
const CAPTURE_PATH: &str = "import capture";

/// The completers for `import capture`'s sources, which only that feature declares.
///
/// Scoped to the one command, unlike [`NETWORK`]: these names are generic enough to
/// collide. `export hls --window <DURATION>` is a playlist window, and an `Any`
/// overlay answered it with this machine's macOS window ids.
#[cfg(feature = "capture")]
static CAPTURE: &[CompletionOverlay<'static>] = &[
	CompletionOverlay::asynchronous(CAPTURE_PATH, "CAMERA", cameras),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "DISPLAY", displays),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "WINDOW", windows),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "APP", apps),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "MICROPHONE", microphones),
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

	// Whatever happens below, the shell gets an answer. `render` of an empty result
	// is a well-formed "no candidates", which is the right thing to say when a
	// lookup has outlived the keystroke that asked for it.
	let answer = timeout_at(Instant::now() + CEILING, complete(&request, staged, &overlays))
		.await
		.unwrap_or_default();

	Some(render(&answer, request.shell))
}

/// Answer one request against the grammar its cursor is in.
async fn complete<'a>(
	request: &CompletionRequest,
	staged: Option<usize>,
	overlays: &'a [CompletionOverlay<'a>],
) -> usage::complete::Completions<'a> {
	let words = request.split.walked();
	match staged {
		None => {
			Cli::app()
				.completion_app()
				.completions(overlays)
				.complete_request(request)
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
				.completions(overlays)
				.complete_request(&request)
				.await
		}
	}
}

/// The process-wide MoQ flags, as far as the words before the cursor go.
///
/// Read from the first chunk, which is the only one that can carry them, and
/// through the real tables rather than by scanning for `--connect`: what a completer
/// dials has to be what an invocation would have dialed, TLS roots included.
///
/// Built twice, because the environment is allowed to configure the dial but not to
/// authorize it. `--connect` has an env var (`MOQ_CONNECT`), so a single pass would
/// let an exported one turn a keystroke into a session with a relay the user never
/// typed, and that URL can carry a `?jwt=` credential. The first pass never reads the
/// environment and its `--connect` is the gate; the second is the one that dials, so
/// everything else an invocation would have picked up still applies.
fn globals(request: &CompletionRequest) -> Option<MoqSide> {
	let chunk = request.split.argv().split(|word| word == "--").next()?;
	let argv: Vec<&OsStr> = chunk.iter().map(OsStr::new).collect();

	let typed = MoqSide::from_argv(&argv, Environment::Ignore)?;
	let mut side = MoqSide::from_argv(&argv, Environment::Read)?;
	if typed.client.url.is_none() {
		side.client.url = None;
	}
	Some(side)
}

/// `T`'s own flags, as far as the words the cursor's command was given go.
///
/// `None` when the cursor is not inside `T`. A stage's own `--broadcast` and
/// `--catalog-format` override the process-wide ones, so a rendition completer has
/// to read the command it was typed in rather than the globals alone.
fn partial<T: CommandArgs>(ctx: &CompleteCtx<'_>) -> Option<T::Partial> {
	let (command, words) = ctx.command_for(T::COMMAND)?;
	let argv: Vec<&OsStr> = words.iter().map(OsStr::new).collect();

	let mut partial = T::start();
	let mut parser = usage::Parser::new(command, &argv);
	while let Some(event) = parser.next_event() {
		match event {
			Ok(event) => {
				T::apply(&mut partial, &event);
			}
			// A line being completed is unfinished by definition, so an error means the
			// grammar ran out here; the partial holds what was understood before that.
			Err(_) => break,
		}
	}
	Some(partial)
}

/// One raw partial value as text, or `None` when it was never given.
fn given(value: &Option<Vec<u8>>) -> Option<&str> {
	std::str::from_utf8(value.as_deref()?).ok()
}

/// One raw partial value read back through its `ValueEnum`.
fn choice<T: ValueEnum>(value: &Option<Vec<u8>>) -> Option<T> {
	T::from_choice(given(value)?)
}

/// The catalog format the command under the cursor named, if it named one.
///
/// `export` and `play` each declare their own `--catalog-format`, and the cursor is
/// inside exactly one of them. Reading only `export`'s would leave a
/// `play --catalog-format msf` completer subscribing to the Hang track that
/// invocation is never going to read.
fn catalog_format(ctx: &CompleteCtx<'_>, export: Option<&<Export as CommandArgs>::Partial>) -> Option<CatalogFormat> {
	let named = export.and_then(|export| choice::<CatalogFormatArg>(&export.catalog_format));

	#[cfg(feature = "play")]
	let named = named.or_else(|| {
		partial::<crate::play::Args>(ctx).and_then(|play| choice::<CatalogFormatArg>(&play.catalog_format))
	});
	#[cfg(not(feature = "play"))]
	let _ = ctx;

	named.map(Into::into)
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

/// Enumerate one kind of capture source, under [`BUDGET`], into candidates.
///
/// Bounded because enumeration is not the quick local lookup it reads as: several
/// backends (V4L2, Media Foundation, CPAL) do blocking work on a pool thread, and
/// ScreenCaptureKit already waits seconds for its own permission callback. Dropping
/// the future cannot cancel a blocking call, but it does let the process answer and
/// exit, which is what keeps a stuck driver from freezing the prompt.
///
/// A platform that cannot list this kind of source, and a lookup that runs out of
/// time, are the same answer: no candidates.
#[cfg(feature = "capture")]
async fn sources<T, E>(
	found: impl Future<Output = Result<Vec<T>, E>>,
	describe: impl Fn(&T) -> Candidate<'static>,
) -> Vec<Candidate<'static>> {
	match timeout_at(Instant::now() + BUDGET, found).await {
		Ok(Ok(items)) => items.iter().map(describe).collect(),
		Ok(Err(_)) | Err(_) => Vec::new(),
	}
}

/// Complete `--camera` from the cameras this machine has.
///
/// Only the attached `--camera=<TAB>` form reaches this: the flag's value is
/// optional (bare `--camera` opens the default), so a detached word after it is
/// as likely to be the next flag, and Usage will not guess.
#[cfg(feature = "capture")]
fn cameras(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::cameras(), |camera| {
			Candidate::described(camera.id.clone(), camera.name.clone())
		})
		.await
	})
}

/// Complete `--display` from the displays this machine has.
#[cfg(feature = "capture")]
fn displays(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::displays(), |display| {
			Candidate::described(
				display.id.clone(),
				format!("{} ({}x{})", display.name, display.width, display.height),
			)
		})
		.await
	})
}

/// Complete `--window` from the windows this machine has open.
#[cfg(feature = "capture")]
fn windows(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::windows(), |window| {
			let title = if window.title.is_empty() {
				"(untitled)"
			} else {
				&window.title
			};
			Candidate::described(window.id.clone(), format!("{} - {title}", window.app))
		})
		.await
	})
}

/// Complete `--app` from the applications this machine is running.
#[cfg(feature = "capture")]
fn apps(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_video::capture::apps(), |app| {
			Candidate::described(app.id.clone(), app.name.clone())
		})
		.await
	})
}

/// Complete `--microphone` from the audio inputs this machine has.
#[cfg(feature = "capture")]
fn microphones(_ctx: CompleteCtx<'_>) -> CompletionFuture<'static> {
	Box::pin(async move {
		sources(moq_audio::capture::devices(), |device| match device.default {
			true => Candidate::described(device.id.clone(), "the default input"),
			false => Candidate::new(device.id.clone()),
		})
		.await
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
			let path = update.prefix.to_string();
			// The root broadcast is the connection path itself, which an unset
			// `--broadcast` already names; there is no word to insert for it.
			if path.is_empty() {
				continue;
			}
			match update.active {
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
	let export = partial::<Export>(ctx);
	let export = export.as_ref();

	// A stage's own `--broadcast` wins over the process-wide one, exactly as it does
	// for the invocation this line is on its way to becoming. `play` declares no
	// `--broadcast`, so there it is the global or nothing.
	let path = export
		.and_then(|export| given(&export.broadcast))
		.or(side.broadcast.as_deref())
		.unwrap_or_default()
		.to_string();

	let format = catalog_format(ctx, export)
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
	// Wait for a covering route rather than asking on the spot: the announcement is
	// still in flight right after connecting, and an immediate request reports a live
	// broadcast as unroutable.
	let consumer = origin.consume();
	consumer.routed(path).await?;

	let mut stream = moq_mux::Source::new(consumer, path).catalog(format).await.ok()?;
	stream.next().await.ok()?
}

/// Open a throwaway subscribe-only session to the relay `--connect` names.
///
/// The Hop ID is fresh and random rather than the pinned `--hop`: this
/// session is not the publisher the user is about to start, and a shared id is
/// what tells a relay two sessions carry the same content.
async fn dial(side: &MoqSide, deadline: Instant) -> Option<(moq_net::origin::Producer, moq_tokio::Connection)> {
	let url = side.client.url.clone()?;
	let origin = moq_tokio::origin::spawn(moq_net::Hop::random());

	// Building the client reads the TLS material off disk synchronously, so it goes on
	// the blocking pool and under the deadline like everything else: a `--connect-tls-root`
	// on a stalled mount (or a FIFO) would otherwise hang the prompt before the first
	// timeout is even entered. Dropping the timeout cannot cancel the read, but it does
	// let the process answer and exit.
	//
	// No iroh endpoint: binding one is more setup than a keystroke should pay for, so an
	// `iroh://` peer completes nothing rather than completing slowly.
	let (connect, quic) = (side.client.clone(), side.quic.clone());
	let client = timeout_at(deadline, tokio::task::spawn_blocking(move || connect.init(quic)))
		.await
		.ok()?
		.ok()?
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
	use crate::test_env::EnvGuard;

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
		let _env = EnvGuard::clear(&["MOQ_CONNECT"]);
		// A stage offers its own flags, and none of the globals it would refuse.
		let staged = complete("moq --connect http://x/y import fmp4 -- export fmp4 --").await;
		assert!(!staged.is_empty(), "a later stage completed nothing");
		for global in ["--connect", "--hop", "--broadcast"] {
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

	/// Every overlay names a value that only the commands it is meant for declare.
	///
	/// Two ways this goes wrong, and neither is visible at runtime. A renamed field
	/// leaves an overlay matching nothing, and a completer that never runs looks
	/// exactly like one that found nothing. A *new* flag that happens to reuse the
	/// name captures an unrelated value: `export hls --window <DURATION>` was answered
	/// with this machine's macOS window ids until the capture overlays were scoped.
	#[test]
	fn every_overlay_matches_only_what_it_answers_for() {
		// Where each `Any`-scoped value name is allowed to appear. These are the names
		// whose meaning really is the same wherever they are written: a broadcast before
		// the verb and on a stage are one relay path, and a rendition is a rendition.
		// Anything scoped to a command is checked against that command instead.
		//
		// Built rather than declared, because which commands exist depends on the build:
		// `play` is a feature, and the root itself is the empty path.
		let renditions = match cfg!(feature = "play") {
			true => vec!["export", "play"],
			false => vec!["export"],
		};
		let shared = [
			("BROADCAST", vec!["", "import", "export"]),
			("VIDEO_NAME", renditions.clone()),
			("AUDIO_NAME", renditions),
		];

		/// Every command path that declares a value by this name.
		fn declaring(command: &usage::spec::CommandMeta<'_>, value: &str, at: &str, found: &mut Vec<String>) {
			let declares = command
				.flags
				.iter()
				.any(|field| field.value_name.unwrap_or(field.flag.name).eq_ignore_ascii_case(value))
				|| command
					.args
					.iter()
					.any(|field| field.arg.name.eq_ignore_ascii_case(value));
			if declares {
				found.push(at.to_string());
			}
			for sub in command.subcommands {
				let deeper = match at.is_empty() {
					true => sub.cmd.name.to_string(),
					false => format!("{at} {}", sub.cmd.name),
				};
				declaring(sub, value, &deeper, found);
			}
		}

		for overlay in overlays() {
			let mut found = Vec::new();
			declaring(Cli::spec().root, overlay.value, "", &mut found);
			assert!(
				!found.is_empty(),
				"no flag or argument takes a value named `{}`, so its completer never runs",
				overlay.value
			);

			// A scoped overlay only fires on its own command, so a name reused elsewhere
			// is harmless; an `Any` one answers everywhere the name appears.
			let Some((_, allowed)) = shared.iter().find(|(name, _)| *name == overlay.value) else {
				continue;
			};
			found.sort();
			let mut allowed: Vec<String> = allowed.iter().map(|path| path.to_string()).collect();
			allowed.sort();
			assert_eq!(
				found, allowed,
				"`{}` is answered everywhere it appears, and the set of commands declaring it changed",
				overlay.value
			);
		}
	}

	/// The environment may configure a MoQ side, but it may not ask for one.
	///
	/// Two consumers of that distinction, tested together because both need the
	/// variable set and this module is where every test holds the lock for it.
	/// Completion must not turn a keystroke into a session with a relay the user never
	/// typed (that URL can carry a `?jwt=`), and a local verb must not refuse to run
	/// because the shell exports a relay for the publishing it usually does.
	#[tokio::test]
	async fn the_environment_cannot_ask_for_a_moq_side() {
		let origin = moq_tokio::origin::spawn(moq_net::Hop::random());
		let _alpha = origin.create_broadcast("alpha").expect("alpha");
		let _announce_alpha = origin.announce("alpha", Default::default()).expect("alpha");
		let connect = relay(&origin);

		// The same reachable relay, named only by the environment.
		let url = connect.split_whitespace().nth(1).expect("a --connect url").to_string();
		let _env = EnvGuard::set(&[("MOQ_CONNECT", &url)]);

		assert!(
			complete("moq --connect-tls-insecure --broadcast ").await.is_empty(),
			"MOQ_CONNECT authorized a dial the line never asked for"
		);

		// The same relay named on the line still completes, so the gate is the URL's
		// source and not the dial itself.
		assert_eq!(
			complete(&format!("moq {connect} --broadcast ")).await,
			["alpha"],
			"a typed --connect stopped working"
		);

		// The other reader of the typed view: `moq token` / `devices` / `completion`
		// refuse a MoQ side, and an exported variable is not one being asked for.
		let ambient = crate::args::Invocation::try_parse_from(["moq", "token", "generate"]).expect("parse");
		assert!(
			ambient.moq.client.url.is_some(),
			"the resolved side should still pick the variable up"
		);
		assert!(
			ambient.reject("token").is_ok(),
			"an exported MOQ_CONNECT was treated as a request"
		);

		let typed =
			crate::args::Invocation::try_parse_from(["moq", "--connect", &url, "token", "generate"]).expect("parse");
		assert!(
			typed.reject("token").is_err(),
			"a typed --connect stopped being refused"
		);
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
		let _env = EnvGuard::clear(&["MOQ_CONNECT"]);
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
		let _env = EnvGuard::clear(&["MOQ_CONNECT"]);
		let origin = moq_tokio::origin::spawn(moq_net::Hop::random());
		let _alpha = origin.create_broadcast("alpha").expect("alpha");
		let _announce_alpha = origin.announce("alpha", Default::default()).expect("alpha");
		let _nested = origin.create_broadcast("room/beta").expect("beta");
		let _announce_nested = origin.announce("room/beta", Default::default()).expect("beta");

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
		let _env = EnvGuard::clear(&["MOQ_CONNECT"]);
		use hang::catalog::{AudioCodec, AudioConfig, H264, VideoConfig};

		let origin = moq_tokio::origin::spawn(moq_net::Hop::random());

		// Two broadcasts with different renditions, so a completer reading the wrong
		// one fails loudly instead of matching by luck.
		let mut keep = Vec::new();
		for (path, video, audio) in [("wanted", "hd", "stereo"), ("other", "sd", "mono")] {
			let mut broadcast = origin.create_broadcast(path).expect("broadcast");
			let announcement = origin.announce(path, Default::default()).expect("broadcast");
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
			keep.push((broadcast, catalog, announcement));
		}

		// The global names `other`; the stage overrides it, exactly as the invocation
		// this line is on its way to becoming would.
		let connect = relay(&origin);
		let line = format!("moq {connect} --broadcast other export --broadcast wanted");

		assert_eq!(complete(&format!("{line} --video-name ")).await, ["hd"]);
		assert_eq!(complete(&format!("{line} --audio-name ")).await, ["stereo"]);
	}

	/// The `--catalog-format` on the line decides which catalog track is read.
	///
	/// `export` and `play` each declare their own, and reading only `export`'s left a
	/// `play --catalog-format msf` completer subscribing to a Hang track that
	/// invocation is never going to read. The broadcast here publishes MSF and nothing
	/// else, which is the one shape that tells the two apart: `moq-mux`'s catalog
	/// producer emits hang and MSF from the same source, so an ordinary broadcast
	/// answers either way and hides the bug.
	#[tokio::test]
	async fn the_catalog_format_on_the_line_is_honored() {
		let _env = EnvGuard::clear(&["MOQ_CONNECT"]);
		let origin = moq_tokio::origin::spawn(moq_net::Hop::random());
		let mut broadcast = origin.create_broadcast("room").expect("broadcast");
		let _announce_broadcast = origin.announce("room", Default::default()).expect("broadcast");

		let mut track = broadcast
			.create_track(moq_msf::DEFAULT_NAME, moq_net::track::Info::default())
			.expect("msf track");
		let mut msf = moq_msf::Track::new("hd", moq_msf::Packaging::Loc);
		msf.role = Some(moq_msf::Role::Video);
		// A video track without one is a hard error in the MSF reader, not a skip.
		msf.codec = Some("avc1.42001e".to_string());
		let catalog = moq_msf::Catalog::new(vec![msf]).to_json().expect("msf json");
		let mut group = track.append_group().expect("group");
		group.write_frame(moq_net::Timestamp::now(), catalog).expect("frame");

		let connect = relay(&origin);
		let line = format!("moq {connect} --broadcast room");

		// Nothing publishes a Hang catalog here, so the default finds no renditions.
		assert!(complete(&format!("{line} export --video-name ")).await.is_empty());
		assert_eq!(
			complete(&format!("{line} export --catalog-format msf --video-name ")).await,
			["hd"],
			"export ignored its own --catalog-format"
		);

		// `play` declares a `--catalog-format` of its own, on a different command.
		#[cfg(feature = "play")]
		{
			assert!(complete(&format!("{line} play --video-name ")).await.is_empty());
			assert_eq!(
				complete(&format!("{line} play --catalog-format msf --video-name ")).await,
				["hd"],
				"play ignored its own --catalog-format"
			);
		}
	}
}
