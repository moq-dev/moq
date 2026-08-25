//! Answering a Usage parse result when the generated `parse()` cannot own the process.
//!
//! [`usage::render_failure`] renders [`usage::Error::Help`], `HelpAll` and `Version`
//! as an empty string: they are questions rather than failures, and the generated
//! `parse()` is expected to take them first. A binary that parses more than once
//! never reaches that code, so it has to answer them itself. Both shapes exist here:
//! a TOML merge that parses, overlays a file, then re-parses, and moq-cli's repeated
//! `--` stage grammar.

use std::ffi::OsStr;

/// What a Usage parse result asks the process to do.
#[non_exhaustive]
pub enum Answer {
	/// Someone asked a question. Print it to stdout and exit 0.
	Question(String),
	/// The command line was wrong. Print it to stderr and exit 2, as clap does.
	Failure(String),
}

impl Answer {
	/// The text to print.
	pub fn message(&self) -> &str {
		match self {
			Self::Question(text) | Self::Failure(text) => text,
		}
	}

	/// Whether this is a question rather than a failure.
	pub fn is_question(&self) -> bool {
		matches!(self, Self::Question(_))
	}

	/// Print to the right stream and exit with the matching status.
	pub fn exit(self) -> ! {
		match self {
			Self::Question(text) => {
				print!("{text}");
				std::process::exit(0)
			}
			Self::Failure(text) => {
				eprint!("{text}");
				std::process::exit(2)
			}
		}
	}
}

/// Render a Usage parse error into the output and exit status it asks for.
///
/// `root` is the command the spec is rooted at, which a help request needs in order
/// to render the page for the route the words actually took.
pub fn answer(
	spec: &usage::argv::spec::Spec<'_>,
	root: &usage::Command<'_>,
	argv: &[&OsStr],
	err: usage::Error<'_, '_>,
) -> Answer {
	use usage::help::{Page, Style};

	let page = |cmd, want, style| usage::help::page(spec, root, argv, cmd, want, style).unwrap_or_default();

	match err {
		usage::Error::Help { cmd, long } => {
			let want = if long { Page::Long } else { Page::Short };
			Answer::Question(page(cmd, want, Style::auto()))
		}
		usage::Error::HelpAll { cmd } => Answer::Question(page(cmd, Page::All, Style::auto())),
		// Not a request: `arg_required_else_help` found nothing to do. clap prints the
		// short page to stderr and exits 2, and so does this.
		usage::Error::MissingArgsHelp { cmd } => Answer::Failure(page(cmd, Page::Short, Style::auto_stderr())),
		usage::Error::Version { long } => {
			let bin = spec.bin.unwrap_or(spec.name);
			let version = if long {
				spec.long_version.or(spec.version)
			} else {
				spec.version.or(spec.long_version)
			}
			.unwrap_or_default();
			Answer::Question(format!("{bin} {version}\n"))
		}
		err => Answer::Failure(usage::render_failure(spec, argv, &err)),
	}
}
