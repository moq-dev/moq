//! Whether a quest can be started, read from the same `Required` sections the
//! rules validate.
//!
//! Readiness is a property of the tree alone, which is what makes it cheap and
//! deterministic: a finished quest is deleted, so a blocker that still resolves
//! is still open. Liveness is the other question - a quest can be ready,
//! coherent, and already done by some other PR - and answering it means asking
//! GitHub, so it belongs to the flow that is already talking to it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::doc::Doc;
use crate::rules;

/// One thing standing between a quest and being started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocker {
	/// The quest or questline that has to finish first, when the blocker is one
	/// of ours. `None` is a plain-text condition, which nothing in the tree can
	/// ever clear.
	pub path: Option<PathBuf>,
	/// The `Required` bullet as written, whitespace collapsed.
	pub text: String,
	/// The still-open quests under a required questline, which is what a
	/// questline blocker actually means. Empty for every other blocker: a
	/// required quest's own blockers are its readiness, not this one's.
	pub blockers: Vec<Blocker>,
}

impl Blocker {
	/// What names the blocker: the document, or the condition's own words.
	pub fn label(&self) -> String {
		match &self.path {
			Some(path) => path.display().to_string(),
			None => self.text.clone(),
		}
	}

	fn write(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
		writeln!(f, "{}{}", "  ".repeat(depth), self.label())?;
		for blocker in &self.blockers {
			blocker.write(f, depth + 1)?;
		}
		Ok(())
	}
}

impl fmt::Display for Blocker {
	/// The whole chain, one blocker per line and indented by depth, newline
	/// included: a caller prints these back to back.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.write(f, 0)
	}
}

/// What blocks `path`, a required questline expanded into the quests it still
/// holds. Empty means ready.
///
/// `path` is the quest as the tree writes it (`/quest/m0/one.md`), as the shell
/// completes it (`quest/m0/one.md`), or as an absolute filesystem path.
pub fn blockers(root: &Path, path: &Path) -> Result<Vec<Blocker>> {
	let docs = crate::load(root)?;
	let by_path: BTreeMap<&Path, &Doc> = docs.iter().map(|d| (d.path.as_path(), d)).collect();
	let path = locate(root, path, &by_path)?;
	Ok(expand(&by_path, by_path[path.as_path()], &mut vec![path.clone()]))
}

/// Every quest that can be started now, in tree order.
///
/// Questlines are never executed, so they are not listed; the absence of a
/// `## Required` heading is what quest/AGENTS.md defines as ready.
pub fn quests(root: &Path) -> Result<Vec<PathBuf>> {
	let docs = crate::load(root)?;
	let mut remaining: BTreeMap<PathBuf, &Doc> = docs.iter().map(|doc| (doc.path.clone(), doc)).collect();
	let mut pending = vec![PathBuf::from("quest/README.md")];
	let mut ready = Vec::new();
	while let Some(path) = pending.pop() {
		let Some(doc) = remaining.remove(&path) else { continue };
		if doc.is_questline() {
			let children: Vec<_> = doc
				.entries("Quests")
				.filter_map(|entry| entry.target.as_deref().and_then(rules::rooted))
				.collect();
			pending.extend(children.into_iter().rev());
		} else if !doc.has("Required") {
			ready.push(path);
		}
	}
	Ok(ready)
}

/// The blockers of one document: a quest waits on its `Required` entries, and a
/// questline is complete only when all of its quests are, so it waits on those.
fn expand(by_path: &BTreeMap<&Path, &Doc>, doc: &Doc, stack: &mut Vec<PathBuf>) -> Vec<Blocker> {
	let section = if doc.is_questline() { "Quests" } else { "Required" };

	// A heading left standing after its last blocker still reads as blocked to
	// everything that greps for it, including `quest check`, which reports it.
	// Calling it ready here would make this the one tool that disagrees.
	if !doc.is_questline() && doc.has("Required") && doc.entries("Required").next().is_none() {
		return vec![Blocker {
			path: None,
			text: "an empty '## Required' section, which blocks the quest until the heading is removed".to_string(),
			blockers: Vec::new(),
		}];
	}

	doc.entries(section)
		.map(|entry| blocker(by_path, entry, stack))
		.collect()
}

fn blocker(by_path: &BTreeMap<&Path, &Doc>, entry: &crate::doc::Entry, stack: &mut Vec<PathBuf>) -> Blocker {
	// A bullet that does not open with a link into the tree is a plain-text
	// condition: an issue, a release, a customer. Nothing here can clear it, so
	// it is a blocker with nothing under it.
	let path = entry
		.target
		.as_deref()
		.and_then(rules::rooted)
		.filter(|path| by_path.contains_key(path.as_path()));

	// Only a questline expands. A required QUEST is the blocker itself, and its
	// own chain is the answer to running this on that quest instead; printing
	// it here buries the entries that were asked for under a repeated subtree.
	// The stack guard is for a tree nobody has run `quest check` on yet, where a
	// questline listing an ancestor must print rather than recurse forever.
	let blockers = match &path {
		Some(path) if by_path[path.as_path()].is_questline() && !stack.contains(path) => {
			stack.push(path.clone());
			let blockers = expand(by_path, by_path[path.as_path()], stack);
			stack.pop();
			blockers
		}
		_ => Vec::new(),
	};

	Blocker {
		path,
		text: entry.text.clone(),
		blockers,
	}
}

/// Resolve a quest path the way a caller is likely to have it to the
/// repository-relative one the tree is keyed on.
fn locate(root: &Path, path: &Path, by_path: &BTreeMap<&Path, &Doc>) -> Result<PathBuf> {
	let mut candidates = vec![rules::normalize(path)];
	if let Some(rooted) = path.to_str().and_then(|p| p.strip_prefix('/')) {
		candidates.push(rules::normalize(Path::new(rooted)));
	}
	if let (Ok(absolute), Ok(root)) = (path.canonicalize(), root.canonicalize())
		&& let Ok(relative) = absolute.strip_prefix(root)
	{
		candidates.push(relative.to_path_buf());
	}

	match candidates.into_iter().find(|c| by_path.contains_key(c.as_path())) {
		Some(found) => Ok(found),
		None => bail!(
			"{} is not a quest document under {}",
			path.display(),
			root.join("quest").display()
		),
	}
}
