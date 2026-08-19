//! Released spellings that no longer work, and what replaced them.
//!
//! The renamed flags and environment variables stay in the parser as hidden args
//! so a process can name their replacement and stop, rather than accept a value it
//! would not honor. [`Deprecated`] is what it collects to say so.

/// The deprecated spellings a config was parsed from, each paired with its
/// replacement.
///
/// Collect one per config section ([`crate::connect::Config::deprecated`] and its
/// siblings), and refuse to start when it is non-empty: the [`std::fmt::Display`]
/// is the migration to print. Honoring the old spelling instead is what this
/// replaced, because a warning that scrolls past leaves a process running on
/// settings it silently dropped.
#[derive(Clone, Debug, Default)]
pub struct Deprecated(Vec<String>);

impl Deprecated {
	/// Whether every spelling in use is a current one, which is the only case a
	/// process may continue from.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Fold another section's findings in, so one message covers the whole config.
	pub fn extend(&mut self, other: Deprecated) {
		self.0.extend(other.0);
	}

	/// Record one released flag and the flag that replaced it.
	///
	/// `env` is the released variable, which is the half a rename usually misses:
	/// a deployment configured through the environment never typed the flag, so a
	/// message naming only the flag reads as unrelated to its problem.
	pub(crate) fn flag(&mut self, old: &str, env: Option<&str>, new: &str) {
		self.entry(&Self::spelling(old, env), new, None);
	}

	/// Record a released flag whose replacement does not mean the same thing, with
	/// a note saying how it differs.
	pub(crate) fn changed(&mut self, old: &str, env: Option<&str>, new: &str, note: &str) {
		self.entry(&Self::spelling(old, env), new, Some(note));
	}

	/// Record a released config-file key or table, which has no environment variable.
	pub(crate) fn toml(&mut self, old: &str, new: &str, note: Option<&str>) {
		self.entry(old, new, note);
	}

	fn entry(&mut self, old: &str, new: &str, note: Option<&str>) {
		self.0.push(match note {
			Some(note) => format!("{old} -> {new} ({note})"),
			None => format!("{old} -> {new}"),
		});
	}

	fn spelling(flag: &str, env: Option<&str>) -> String {
		match env {
			Some(env) => format!("{flag} / {env}"),
			None => flag.to_string(),
		}
	}
}

impl std::fmt::Display for Deprecated {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"these settings were renamed and are no longer applied; update them and try again:"
		)?;
		for entry in &self.0 {
			write!(f, "\n  {entry}")?;
		}
		Ok(())
	}
}
