//! Which branch carries a quest, and which branches it merges through.
//!
//! The mapping is the path alone: a quest's branch is its path without `.md`
//! and a questline's is its README's. Milestones have no branch, so a
//! milestone's direct children merge into `main`. Nothing here asks git, so the
//! answer is the same on every machine and a missing line branch is simply
//! created from the next one in the chain.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};

use crate::doc::Doc;

/// The branch for `path`, then every branch it merges through, nearest first
/// and ending at `main`.
pub fn chain(root: &Path, path: &Path) -> Result<Vec<String>> {
	let docs = crate::load(root)?;
	let by_path: BTreeMap<&Path, &Doc> = docs.iter().map(|d| (d.path.as_path(), d)).collect();
	let mut cur = crate::ready::locate(root, path, &by_path)?;
	let mut out = Vec::new();
	while !Doc::permanent(&cur) {
		out.push(cur.with_extension("").display().to_string());
		cur = Doc::owner(&cur).join("README.md");
	}
	if out.is_empty() {
		bail!("{} is the root or a milestone, which has no branch", path.display());
	}
	out.push("main".to_string());
	Ok(out)
}
