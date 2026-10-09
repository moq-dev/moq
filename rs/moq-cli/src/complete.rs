//! Shell completion: which grammar answers the cursor, and the runtime completers
//! that answer values the static tables cannot know.
//!
//! Two halves. The plumbing decides whether a cursor belongs to the root grammar
//! or to a `--`-separated stage, because this binary splits argv itself (see
//! [`crate::args`]) and Usage's own interception only knows the root. The
//! completers answer the capture sources this machine has.
//!
//! Completion is local: nothing here dials a relay or reads a catalog, so a
//! keystroke stays fast and works offline.
//!
//! A completion is not a command anybody ran, so nothing here reports a failure.
//! A source that cannot be listed, or a budget that runs out, means "no
//! candidates": a message in the prompt would be worse than a short list.

use std::ffi::OsString;
// Only the capture completers name a future by type.
#[cfg(feature = "capture")]
use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use tokio::time::{Instant, timeout_at};
#[cfg(feature = "capture")]
use usage::complete::{Candidate, CompleteCtx, CompletionFuture};
use usage::complete::{CompletionOverlay, CompletionRequest, Shell, render};

use crate::args::{Cli, Stage};

/// The wall-clock budget one capture completer gets.
///
/// Tab is pressed between keystrokes, so a completer that outlives the user's
/// patience is a shell that appears to hang, and no list now beats a list later.
#[cfg(feature = "capture")]
const BUDGET: Duration = Duration::from_millis(500);

/// The ceiling on a whole completion request, whatever it is answering.
///
/// A backstop, not the working budget: every completer bounds its own lookup by
/// its budget, and this is deliberately looser so that theirs always fires first.
/// It exists so a completer added later cannot hang a prompt by forgetting to bound
/// itself, and so work that ignores cancellation (a blocking device enumeration)
/// still cannot hold the answer back.
const CEILING: Duration = Duration::from_millis(1_500);

/// The command whose source flags [`OVERLAYS`] answers for.
#[cfg(feature = "capture")]
const CAPTURE_PATH: &str = "import capture";

/// Every completer this build has, keyed by the *value* name it answers for.
///
/// The value name, not the flag: that is what Usage matches an overlay on, and the
/// derive spells it in screaming snake case (`--camera <CAMERA>`).
/// `every_overlay_matches_its_command` keeps this table honest.
///
/// Scoped to `import capture`, the one command that declares these sources: the
/// names are generic enough to collide. `export hls --window <DURATION>` is a
/// playlist window, and an unscoped overlay answered it with this machine's macOS
/// window ids.
#[cfg(feature = "capture")]
static OVERLAYS: &[CompletionOverlay<'static>] = &[
	CompletionOverlay::asynchronous(CAPTURE_PATH, "CAMERA", cameras),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "DISPLAY", displays),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "WINDOW", windows),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "APP", apps),
	CompletionOverlay::asynchronous(CAPTURE_PATH, "MICROPHONE", microphones),
];

/// See [`OVERLAYS`].
#[cfg(not(feature = "capture"))]
static OVERLAYS: &[CompletionOverlay<'static>] = &[];

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
	let answer = timeout_at(Instant::now() + CEILING, complete(&request, staged))
		.await
		.unwrap_or_default();

	Some(render(&answer, request.shell))
}

/// Answer one request against the grammar its cursor is in.
async fn complete(request: &CompletionRequest, staged: Option<usize>) -> usage::complete::Completions<'static> {
	let words = request.split.walked();
	match staged {
		None => {
			Cli::app()
				.completion_app()
				.completions(OVERLAYS)
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
				.completions(OVERLAYS)
				.complete_request(&request)
				.await
		}
	}
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_env::EnvGuard;
	use usage::spec::ValueEnum;

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

	/// A cursor in a later stage is completed against the stage grammar.
	///
	/// The root spec is the globals plus the first stage, so answering a later chunk
	/// against it offers process-wide flags that the chunk refuses.
	#[tokio::test(start_paused = true)]
	async fn retargets_to_the_active_stage() {
		let _env = EnvGuard::clear(&["MOQ_CONNECT"]);
		// A stage offers its own flags, and none of the globals it would refuse.
		let staged = complete("moq --connect http://x/y import fmp4 -- export fmp4 --").await;
		assert!(!staged.is_empty(), "a later stage completed nothing");
		for global in ["--connect", "--epoch", "--broadcast"] {
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

	/// Every overlay is scoped to a command that declares the value it answers for.
	///
	/// A renamed field leaves an overlay matching nothing, and a completer that never
	/// runs looks exactly like one that found nothing. An unscoped overlay captures any
	/// flag that happens to reuse the name: `export hls --window <DURATION>` was
	/// answered with this machine's macOS window ids until the capture overlays were
	/// scoped.
	#[test]
	fn every_overlay_matches_its_command() {
		/// Every command path that declares a value by this name.
		fn declaring(command: &usage::spec::CommandMeta<'_>, value: &str, at: &str, found: &mut Vec<String>) {
			if command.hide {
				return;
			}
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

		for overlay in OVERLAYS {
			let usage::spec::CommandSelector::Path(path) = overlay.command else {
				panic!(
					"`{}` is answered wherever it appears; scope it to its command",
					overlay.value
				);
			};
			let mut found = Vec::new();
			declaring(Cli::spec().root, overlay.value, "", &mut found);
			assert!(
				found.iter().any(|at| at == path),
				"`{path}` takes no value named `{}`, so its completer never runs",
				overlay.value
			);
		}
	}

	/// Completion never dials, even with a reachable relay on the line.
	///
	/// A keystroke must stay fast and work offline, and a dial is neither. The relay
	/// announces a broadcast, so a completer that still dialed would name it.
	#[tokio::test]
	async fn completion_never_dials() {
		let _env = EnvGuard::clear(&["MOQ_CONNECT"]);
		let _ = moq_tokio::crypto::install_default();

		let origin = moq_tokio::origin::spawn();
		let _alpha = origin.create_broadcast("alpha").expect("alpha");
		_alpha.announce(Default::default()).expect("alpha");

		let mut config = moq_tokio::listen::Config::default();
		config.bind = Some("127.0.0.1:0".parse().unwrap());
		config.tls.generate = vec!["localhost".to_string()];
		let server = config.init(Default::default()).expect("failed to bind listener");
		let port = server.local_addr().expect("no local addr").port();
		tokio::spawn(server.serve_publish(origin.consume()));
		let connect = format!("--connect moqt://127.0.0.1:{port} --connect-tls-insecure");

		for line in [
			format!("moq {connect} --broadcast "),
			format!("moq {connect} import fmp4 -- export --broadcast "),
			format!("moq {connect} --broadcast alpha export --video-name "),
		] {
			let found = complete(&line).await;
			assert!(
				!found.iter().any(|candidate| candidate == "alpha"),
				"{line:?} read the relay: {found:?}"
			);
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
}
