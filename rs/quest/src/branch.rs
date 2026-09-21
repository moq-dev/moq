//! Which branch carries a quest, and which branches it merges through.
//!
//! The mapping is the path alone: a quest's branch is its path without `.md`,
//! a questline's is its README's, and the top-level questlines are the
//! long-lived branches themselves. Nothing here asks git, so the answer is the
//! same on every machine and a missing line branch is simply created from the
//! next one in the chain.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};

use crate::doc::Doc;

/// The top-level questlines that are long-lived branches, each with the branch
/// it merges into. Any other top-level questline is the roadmap: nothing under
/// it has a branch until it moves.
const BRANCHES: [(&str, Option<&str>); 2] = [("main", None), ("dev", Some("main"))];

/// The branch for `path`, then every branch it merges through, nearest first
/// and ending at `main`.
pub fn chain(root: &Path, path: &Path) -> Result<Vec<String>> {
	let docs = crate::load(root)?;
	let by_path: BTreeMap<&Path, &Doc> = docs.iter().map(|d| (d.path.as_path(), d)).collect();
	let mut cur = crate::ready::locate(root, path, &by_path)?;
	let mut out = Vec::new();
	loop {
		let parts: Vec<&str> = cur.iter().filter_map(|p| p.to_str()).collect();
		match parts.as_slice() {
			[_, "README.md"] => bail!("{} is the root questline, which has no branch", path.display()),
			[_, top, "README.md"] => {
				let Some((name, base)) = BRANCHES.iter().find(|(name, _)| name == top) else {
					let branches: Vec<_> = BRANCHES.iter().map(|(name, _)| *name).collect();
					bail!(
						"{} is under {top}, which has no branch; move it under {} to start it",
						path.display(),
						branches.join(" or ")
					);
				};
				out.push(name.to_string());
				let mut base = *base;
				while let Some(name) = base {
					out.push(name.to_string());
					base = BRANCHES.iter().find(|(n, _)| *n == name).and_then(|(_, b)| *b);
				}
				return Ok(out);
			}
			_ => {
				out.push(cur.with_extension("").display().to_string());
				cur = Doc::owner(&cur).join("README.md");
			}
		}
	}
}
