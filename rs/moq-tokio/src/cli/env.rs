//! Refusing `MOQ_*` environment variables a binary does not read.

use std::collections::HashSet;
use std::ffi::OsString;

use usage_rs::argv::spec::{ArgMeta, CommandMeta, FlagMeta, Spec};

/// The prefix every MoQ binary's environment variables share.
const PREFIX: &str = "MOQ_";

/// `MOQ_*` environment variables that nothing in the binary reads.
///
/// Returned by [`check_env`]. The [`std::fmt::Display`] names each one.
#[derive(Clone, Debug)]
pub struct UnknownEnv(Vec<String>);

impl std::fmt::Display for UnknownEnv {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"unknown environment variables, which this binary does not read; remove or rename them: {}",
			self.0.join(", ")
		)
	}
}

impl std::error::Error for UnknownEnv {}

/// Refuse every `MOQ_*` variable in the process environment that none of `specs`
/// declares and `direct` does not name.
///
/// `specs` are the command-line specs the binary parses with, and `direct` the
/// variables it reads outside them. Without this, a deployment that sets a stale or
/// misspelled name runs on defaults without a word.
pub fn check_env<'a>(specs: &[&'a Spec<'a>], direct: &[&'a str]) -> Result<(), UnknownEnv> {
	let mut known = HashSet::new();
	for spec in specs {
		declared(spec.root, &mut known);
	}
	known.extend(direct.iter().copied());

	// `vars_os`, because `vars` panics on any entry that is not UTF-8, even an unrelated one.
	let unknown = unknown(std::env::vars_os().map(|(name, _)| name), &known);
	if unknown.is_empty() {
		Ok(())
	} else {
		Err(UnknownEnv(unknown))
	}
}

/// Every environment variable `meta` and its subcommands declare, including deprecated ones.
fn declared<'a>(meta: &'a CommandMeta<'a>, known: &mut HashSet<&'a str>) {
	let flag = |flag: &FlagMeta<'a>| {
		flag.env()
			.into_iter()
			.chain(flag.env_fallback().iter().copied())
			.chain(flag.deprecated_env().iter().copied())
	};
	let arg = |arg: &ArgMeta<'a>| {
		arg.env()
			.into_iter()
			.chain(arg.env_fallback().iter().copied())
			.chain(arg.deprecated_env().iter().copied())
	};

	known.extend(meta.flags().iter().flat_map(flag));
	known.extend(meta.args().iter().flat_map(arg));
	if let Some(clause) = meta.clause() {
		known.extend(clause.flags.iter().flat_map(flag));
		known.extend(clause.args.iter().flat_map(arg));
	}
	for sub in meta.subcommands() {
		declared(sub, known);
	}
}

/// The `MOQ_*` names in `names` that `known` lacks, sorted and deduplicated.
fn unknown(names: impl IntoIterator<Item = OsString>, known: &HashSet<&str>) -> Vec<String> {
	let mut unknown: Vec<String> = names
		.into_iter()
		.map(|name| {
			let name = name.to_string_lossy().into_owned();
			// Windows matches variable names case-insensitively, so `moq_connect`
			// sets `MOQ_CONNECT` there.
			if cfg!(windows) { name.to_ascii_uppercase() } else { name }
		})
		.filter(|name| name.starts_with(PREFIX) && !known.contains(name.as_str()))
		.collect();
	unknown.sort();
	unknown.dedup();
	unknown
}

#[cfg(test)]
mod tests {
	use super::*;

	#[allow(dead_code, reason = "only the derived spec is read")]
	#[derive(usage_rs::Args)]
	struct Shared {
		/// Declared by a flattened section.
		#[usage(long, env = "MOQ_SHARED")]
		shared: Option<String>,

		/// A released spelling, still declared so it can be reported.
		#[usage(long, env = "MOQ_RELEASED", hide)]
		released: Option<String>,
	}

	#[allow(dead_code, reason = "only the derived spec is read")]
	#[derive(usage_rs::Cli)]
	#[usage(unknown_flags = "error")]
	struct Cli {
		#[usage(flatten)]
		shared: Shared,

		#[usage(subcommand)]
		command: Command,
	}

	#[allow(dead_code, reason = "only the derived spec is read")]
	#[derive(usage_rs::Subcommands)]
	enum Command {
		/// Declares a variable only under a subcommand.
		Run {
			#[usage(long, env = "MOQ_RUN")]
			run: Option<String>,
		},
	}

	fn known() -> HashSet<&'static str> {
		let mut known = HashSet::new();
		declared(Cli::spec().root, &mut known);
		known
	}

	fn names(names: &[&str]) -> Vec<OsString> {
		names.iter().map(OsString::from).collect()
	}

	/// Flattened, hidden, and subcommand variables all count as read.
	#[test]
	fn declared_reaches_every_section() {
		let known = known();
		for name in ["MOQ_SHARED", "MOQ_RELEASED", "MOQ_RUN"] {
			assert!(known.contains(name), "{name} is declared");
		}
	}

	/// Only `MOQ_*` names are checked, and each is reported once, sorted.
	#[test]
	fn unknown_names_only_moq() {
		let found = unknown(
			names(&["MOQ_SHARED", "PATH", "MOQ_TYPO", "MOQ", "MOQ_RUN", "MOQ_A", "MOQ_TYPO"]),
			&known(),
		);
		assert_eq!(found, ["MOQ_A", "MOQ_TYPO"]);
	}

	/// A name that is not UTF-8 is still refused, rather than panicking or slipping past.
	#[cfg(unix)]
	#[test]
	fn unknown_non_utf8() {
		use std::os::unix::ffi::OsStringExt;

		let found = unknown([OsString::from_vec(b"MOQ_\xff".to_vec())], &known());
		assert_eq!(found, ["MOQ_\u{fffd}"]);
		assert!(unknown([OsString::from_vec(b"OTHER_\xff".to_vec())], &known()).is_empty());
	}
}
