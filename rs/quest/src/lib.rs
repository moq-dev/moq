//! Structural validation of the quest tree, whose contract is quest/AGENTS.md.
//!
//! The whole tree is validated on every run, never just the changed files: the
//! link graph and the questline index are global, so completing one quest
//! breaks files the diff never mentions. That is not hypothetical - it is how
//! the index entry for a completed quest survived a rebase that produced no
//! conflict at all.

pub mod doc;
pub mod rules;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub use doc::Doc;
pub use rules::Finding;

/// AGENTS.md is the contract rather than a quest, and CLAUDE.md is a symlink to
/// it; neither is executable work, so neither is indexed or validated.
const NOT_QUESTS: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Every quest document under `<root>/quest`, sorted, repository-relative.
pub fn collect(root: &Path) -> Result<Vec<PathBuf>> {
	let mut out = Vec::new();
	walk(root, &root.join("quest"), &mut out).with_context(|| format!("scanning {}", root.join("quest").display()))?;
	out.sort();
	Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
	for entry in std::fs::read_dir(dir)? {
		let entry = entry?;
		let path = entry.path();
		// `file_type` does not follow symlinks, so quest/CLAUDE.md is a file
		// here and a directory symlink can never make this recurse forever.
		let kind = entry.file_type()?;
		if kind.is_dir() {
			walk(root, &path, out)?;
		} else if path.extension().is_some_and(|e| e == "md")
			&& !path
				.file_name()
				.is_some_and(|n| NOT_QUESTS.iter().any(|skip| n == *skip))
		{
			out.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
		}
	}
	Ok(())
}

/// Parse and validate the tree. Returns every finding, worst-case empty.
pub fn check(root: &Path) -> Result<Vec<Finding>> {
	let paths = collect(root)?;
	if paths.is_empty() {
		bail!("no quest documents found under {}", root.join("quest").display());
	}
	let docs = paths
		.into_iter()
		.map(|p| Doc::parse(root, p))
		.collect::<Result<Vec<_>>>()?;
	Ok(rules::check(root, &docs))
}
