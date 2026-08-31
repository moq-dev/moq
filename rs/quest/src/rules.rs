//! The rules, and the findings they produce.
//!
//! The contract is quest/AGENTS.md. Every rule here is one that has already
//! been broken by hand, and the expensive failure is a rule that stops firing:
//! a validator that quietly enforces nothing looks exactly like a clean tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::doc::{Doc, Position};

/// The `## ` headings a quest document may use. Readiness greps `## Required`
/// literally, so a typo turns a blocked quest ready and fails nowhere else:
/// the closed vocabulary is what catches it.
const HEADINGS: [&str; 6] = ["Goal", "Plan", "Required", "Closes", "Related", "Quests"];
const SIZES: [&str; 5] = ["XS", "S", "M", "L", "XL"];

/// Sections whose whole content is a list. A heading left standing after its
/// last entry was removed is a bug in both directions: an empty `Required`
/// blocks its quest forever, and an empty `Quests` leaves a questline that
/// should have been deleted with its last quest.
/// `Quests` is deliberately absent: a bullet is not an entry (`- TBD` is a list
/// item with no quest in it), so its emptiness is decided by counting valid
/// entries in `index` instead.
const LIST_SECTIONS: [&str; 3] = ["Required", "Closes", "Related"];

/// The permanent root questline; the one document nothing has to list.
pub const ROOT: &str = "quest/README.md";

/// One violation, addressed like a compiler diagnostic: `path:line: message`.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
	/// Repository-relative document the violation is in.
	pub path: PathBuf,
	/// 1-based line, when the violation has one; `None` for whole-file findings.
	pub line: Option<usize>,
	/// What is wrong, and often why the rule exists.
	pub message: String,
}

impl std::fmt::Display for Finding {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self.line {
			Some(line) => write!(f, "{}:{}: {}", self.path.display(), line, self.message),
			None => write!(f, "{}: {}", self.path.display(), self.message),
		}
	}
}

struct Findings(Vec<Finding>);

impl Findings {
	fn at(&mut self, path: &Path, line: usize, message: impl Into<String>) {
		self.0.push(Finding {
			path: path.to_path_buf(),
			line: Some(line),
			message: message.into(),
		});
	}

	fn on(&mut self, path: &Path, message: impl Into<String>) {
		self.0.push(Finding {
			path: path.to_path_buf(),
			line: None,
			message: message.into(),
		});
	}
}

/// `root` is the repository root; every path in `docs` is relative to it.
pub fn check(root: &Path, docs: &[Doc]) -> Vec<Finding> {
	let mut found = Findings(Vec::new());
	let known: BTreeSet<&Path> = docs.iter().map(|d| d.path.as_path()).collect();

	for doc in docs {
		headings(&mut found, doc);
		links(&mut found, root, &known, doc);
	}

	let index = index(&mut found, &known, docs);
	cycles(&mut found, docs, &index);

	found.0.sort();
	found.0
}

fn headings(found: &mut Findings, doc: &Doc) {
	if !doc.is_questline() {
		let valid = doc.title.as_ref().is_some_and(|title| {
			let Some((size, name)) = title.text.strip_prefix('[').and_then(|s| s.split_once("] ")) else {
				return false;
			};
			title.literal && SIZES.contains(&size) && !name.is_empty()
		});
		if !valid {
			found.on(&doc.path, "quest title must be '# [XS|S|M|L|XL] Title'");
		}
	}

	if !doc.has("Goal") {
		found.on(&doc.path, "missing '## Goal'");
	}

	for heading in &doc.headings {
		// A setext underline and a decorated ``## `Required` `` both render as
		// the same heading, and this validator would treat the quest as blocked.
		// Readiness greps `^## Required$` literally and would call it READY.
		if HEADINGS.contains(&heading.text.as_str()) && !heading.literal {
			found.at(
				&doc.path,
				heading.line,
				format!(
					"'{}' must be written literally as '## {}'; readiness greps that form and would not see this one",
					heading.text, heading.text
				),
			);
		}
		if !HEADINGS.contains(&heading.text.as_str()) {
			found.at(
				&doc.path,
				heading.line,
				format!("unknown '## {}' (allowed: {})", heading.text, HEADINGS.join(", ")),
			);
		}
	}

	// A questline is a README with `## Quests`; a quest is everything else and
	// must not have one. Only quests are executed, so the distinction decides
	// what a reader is allowed to pick up.
	match (doc.is_questline(), doc.has("Quests")) {
		(true, false) => found.on(&doc.path, "a questline needs '## Quests'"),
		(false, true) => found.on(&doc.path, "only a questline README may have '## Quests'"),
		_ => {}
	}
	for heading in &doc.headings {
		if LIST_SECTIONS.contains(&heading.text.as_str()) && !doc.sections_with_entries.contains(&heading.text) {
			let why = match heading.text.as_str() {
				"Required" => "; an empty one blocks the quest forever, so remove the heading with its last entry",
				"Quests" => "; a questline with no quests left should be deleted",
				_ => "; remove the heading with its last entry",
			};
			found.at(&doc.path, heading.line, format!("'## {}' is empty{why}", heading.text));
		}
	}
}

/// Strip a `#fragment`, which addresses a place inside a file rather than a
/// different file. Leaving it on made a phantom graph node that no quest could
/// ever match, so a cycle through an anchored link went unreported.
fn without_fragment(target: &str) -> &str {
	target.split('#').next().unwrap_or(target)
}

/// A root-absolute target as a repository-relative path, or `None` if it is not
/// root-absolute. Fragment-stripped AND normalized: the index and the cycle walk
/// both key on this, and a `..` left in one of them is a node nothing matches.
fn rooted(target: &str) -> Option<PathBuf> {
	target
		.strip_prefix('/')
		.map(|r| normalize(Path::new(without_fragment(r))))
}

fn resolve(doc_path: &Path, target: &str) -> PathBuf {
	match target.strip_prefix('/') {
		Some(rooted) => normalize(Path::new(rooted)),
		None => normalize(&doc_path.parent().unwrap_or(Path::new("")).join(target)),
	}
}

/// Collapse `..` textually. `Path::canonicalize` would need the file to exist,
/// which is the very thing being tested.
fn normalize(path: &Path) -> PathBuf {
	let mut out = PathBuf::new();
	for part in path.components() {
		match part {
			std::path::Component::ParentDir => {
				// Popping unconditionally let `../..` cancel itself, so a link
				// with too many `..` climbed above the root and then walked back
				// down to a real file.
				if out.components().next_back() == Some(std::path::Component::ParentDir) || !out.pop() {
					out.push("..");
				}
			}
			std::path::Component::CurDir => {}
			other => out.push(other.as_os_str()),
		}
	}
	out
}

fn links(found: &mut Findings, root: &Path, known: &BTreeSet<&Path>, doc: &Doc) {
	for link in &doc.links {
		if link.target.contains("://") || link.target.starts_with("mailto:") {
			continue;
		}
		let target = without_fragment(&link.target);
		if target.is_empty() {
			continue;
		}

		// A normalized path that still opens with `..` points above the
		// repository root. Joining it to `root` and testing existence would
		// follow it into whatever sits beside the checkout, so the repo's own
		// directory name (or a sibling worktree) could make a broken link pass.
		let path = resolve(&doc.path, target);
		if path.starts_with("..") || !root.join(&path).exists() {
			found.at(&doc.path, link.line, format!("link does not resolve: {}", link.target));
			continue;
		}

		// Quests and questlines reference each other with root-absolute links.
		// A relative one still renders, so nothing else would notice - but it is
		// invisible to the dependency graph below, which only speaks /quest/...
		if known.contains(path.as_path()) && !link.target.starts_with('/') {
			found.at(
				&doc.path,
				link.line,
				format!(
					"link to a quest must be root-absolute: {} (write /{})",
					link.target,
					path.display()
				),
			);
		}

		// A `Required` bullet is either a dependency edge (the link opens it) or
		// a plain-text external condition (no quest link at all).
		// moq-dev/moq.pro#1170 shipped the third shape: a customer-gate sentence
		// mentioning a questline mid-line, which reads as context but IS a
		// blocker, and so silently required all of m2.
		if link.section.as_deref() == Some("Required")
			&& known.contains(path.as_path())
			&& link.position != Position::Entry
		{
			found.at(
				&doc.path,
				link.line,
				format!(
					"Required links {} mid-sentence; that reads as prose but IS a blocker - open the bullet with the link, or drop the link",
					link.target
				),
			);
		}
	}
}

/// Which questline lists each document. The index must be exactly the file
/// tree: every document is listed by the questline it sits under, and a
/// questline lists nothing but its own children - together these also give
/// "listed by exactly one questline".
fn index<'a>(found: &mut Findings, known: &BTreeSet<&Path>, docs: &'a [Doc]) -> BTreeMap<PathBuf, &'a Path> {
	let mut listed: BTreeMap<PathBuf, &Path> = BTreeMap::new();
	let mut entries: BTreeMap<&Path, usize> = BTreeMap::new();

	for doc in docs {
		for link in &doc.links {
			if link.section.as_deref() != Some("Quests") {
				continue;
			}
			// The index is a list of entries, not prose that happens to link:
			// `See [One](/quest/m0/one.md)` must not make One look indexed.
			if link.position != Position::Entry {
				found.at(
					&doc.path,
					link.line,
					format!("a Quests entry must open its bullet: {}", link.target),
				);
				continue;
			}
			let Some(child) = rooted(&link.target) else {
				found.at(
					&doc.path,
					link.line,
					format!(
						"a Quests entry must be a root-absolute /quest/... link: {}",
						link.target
					),
				);
				continue;
			};
			// Existence is not enough: the index points readers at work to pick
			// up, and quest/AGENTS.md is a file under quest/ that is not a quest.
			if !known.contains(child.as_path()) {
				found.at(
					&doc.path,
					link.line,
					format!("lists {}, which is not a quest document", link.target),
				);
				continue;
			}
			if Doc::owner(&child) != doc.path.parent().unwrap_or(Path::new("")) {
				found.at(
					&doc.path,
					link.line,
					format!("lists {}, which does not sit under this questline", link.target),
				);
				continue;
			}
			if listed.insert(child, doc.path.as_path()).is_some() {
				found.at(&doc.path, link.line, format!("lists {} twice", link.target));
			}
			*entries.entry(doc.path.as_path()).or_default() += 1;
		}
	}

	for doc in docs {
		// A questline lists at least one quest. Completing its last one is
		// supposed to delete the directory; a heading holding a bullet with no
		// quest in it (`- TBD`) leaves the husk standing just as well as a bare
		// one does.
		if doc.is_questline() && doc.has("Quests") && entries.get(doc.path.as_path()).copied().unwrap_or(0) == 0 {
			found.on(
				&doc.path,
				"'## Quests' lists no quest; a questline with none left should be deleted",
			);
		}

		// The root questline is permanent and has nothing above it to list it.
		if doc.path == Path::new(ROOT) || listed.contains_key(&doc.path) {
			continue;
		}
		let owner = Doc::owner(&doc.path).join("README.md");
		found.on(
			&doc.path,
			format!(
				"not listed in {}'s '## Quests'; an unlisted quest is unreachable",
				owner.display()
			),
		);
	}

	listed
}

/// `Required` must be acyclic. A cycle is a set of quests none of which can
/// ever start, and walking the links to rule one out is exactly the manual step
/// AGENTS.md asks of an author before adding a blocker.
fn cycles(found: &mut Findings, docs: &[Doc], listed: &BTreeMap<PathBuf, &Path>) {
	let mut blockers: BTreeMap<&Path, Vec<PathBuf>> = BTreeMap::new();

	for doc in docs {
		let edges: Vec<PathBuf> = doc
			.links
			.iter()
			.filter(|l| l.section.as_deref() == Some("Required") && l.position == Position::Entry)
			.filter_map(|l| rooted(&l.target))
			.collect();
		blockers.entry(doc.path.as_path()).or_default().extend(edges);
	}

	// A quest may require a whole QUESTLINE, and a questline is complete only
	// when all of its quests are, so a questline waits on its own children.
	// Without these edges the walk stops at the README and misses the deadlock
	// that spans it: a quest requiring the questline that holds the quest
	// requiring it back.
	for (child, questline) in listed {
		blockers.entry(questline).or_default().push(child.clone());
	}

	#[derive(Clone, Copy, PartialEq)]
	enum State {
		Open,
		Done,
	}

	fn walk(
		node: &Path,
		blockers: &BTreeMap<&Path, Vec<PathBuf>>,
		state: &mut BTreeMap<PathBuf, State>,
		stack: &mut Vec<PathBuf>,
		found: &mut Findings,
	) {
		match state.get(node) {
			Some(State::Done) => return,
			Some(State::Open) => {
				let from = stack.iter().position(|p| p == node).unwrap_or(0);
				let mut path: Vec<String> = stack[from..].iter().map(|p| p.display().to_string()).collect();
				path.push(node.display().to_string());
				found.on(node, format!("Required cycle: {}", path.join(" -> ")));
				return;
			}
			None => {}
		}
		state.insert(node.to_path_buf(), State::Open);
		stack.push(node.to_path_buf());
		for next in blockers.get(node).map(Vec::as_slice).unwrap_or_default() {
			walk(next, blockers, state, stack, found);
		}
		stack.pop();
		state.insert(node.to_path_buf(), State::Done);
	}

	let mut state = BTreeMap::new();
	for doc in docs {
		walk(&doc.path, &blockers, &mut state, &mut Vec::new(), found);
	}
}
