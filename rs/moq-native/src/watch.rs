use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use notify::Watcher;
use tokio::sync::mpsc;

/// Poll interval used only when an OS file watcher can't be created.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Watches a set of files and resolves whenever one of them changes on disk.
///
/// Reacting to the filesystem (rather than a SIGHUP/SIGUSR1) is what lets
/// cert-manager, Kubernetes secret mounts, and `mv`-into-place rotate files with
/// no extra signalling: they rewrite the file and the watcher reloads.
///
/// Watches each file's *parent directory*, not the file itself. Editors,
/// cert-manager, and K8s secret mounts replace files by atomic rename or symlink
/// swap, which changes the inode and would silently drop a watch set directly on
/// the path. Falls back to polling when no OS watcher can be created.
///
/// Change is judged by mtime, and an unreadable file (e.g. the brief window
/// mid-rename) is ignored rather than reported, so a half-written set never
/// triggers a reload. A caller that fails to load can leave its state untouched;
/// the next real change fires again.
pub(crate) struct FileWatcher {
	paths: Vec<PathBuf>,
	mtimes: Vec<Option<SystemTime>>,
	// Holds the OS watcher alive (dropping it stops events). `None` => polling.
	_watcher: Option<notify::RecommendedWatcher>,
	events: Option<mpsc::Receiver<()>>,
}

impl FileWatcher {
	/// Begin watching `paths`. Records their current mtimes as the baseline so
	/// [`changed`](Self::changed) only resolves on a subsequent change.
	pub(crate) fn new(paths: Vec<PathBuf>) -> Self {
		// A capacity-1 channel of unit wakeups coalesces the flood of raw events
		// notify emits (duplicates per change, plus unrelated sibling churn in the
		// watched directory): when the buffer is already full there's a pending
		// wakeup, so extra sends are simply dropped. The payload is irrelevant;
		// `changed` rescans mtimes to decide what actually moved.
		let (tx, rx) = mpsc::channel(1);
		let watcher = notify::recommended_watcher(move |_event| {
			let _ = tx.try_send(());
		})
		.ok()
		.and_then(|mut watcher| {
			// Watch each distinct parent directory once.
			let mut dirs: Vec<&Path> = paths.iter().filter_map(|p| p.parent()).collect();
			dirs.sort_unstable();
			dirs.dedup();
			for dir in dirs {
				watcher.watch(dir, notify::RecursiveMode::NonRecursive).ok()?;
			}
			Some(watcher)
		});

		if watcher.is_none() {
			tracing::warn!(
				?paths,
				"no filesystem watcher available; falling back to {POLL_INTERVAL:?} polling"
			);
		}

		// Snapshot mtimes after the watch is live so a change landing between the
		// snapshot and watch registration can't slip through unobserved.
		let mtimes = paths.iter().map(|p| mtime(p)).collect();
		let events = watcher.is_some().then_some(rx);

		Self {
			paths,
			mtimes,
			_watcher: watcher,
			events,
		}
	}

	/// Resolve once at least one watched file's mtime advances.
	///
	/// Coalesces the duplicate and sibling-file events the OS delivers: a wakeup
	/// that didn't actually change any watched file is swallowed and we keep
	/// waiting.
	pub(crate) async fn changed(&mut self) {
		loop {
			match &mut self.events {
				// `recv` only yields `None` once the watcher is dropped, which
				// can't happen while we hold it; park rather than busy-loop.
				Some(rx) => match rx.recv().await {
					Some(_) => {}
					None => std::future::pending().await,
				},
				None => tokio::time::sleep(POLL_INTERVAL).await,
			}

			if self.advanced() {
				return;
			}
		}
	}

	/// Refresh stored mtimes, returning true if any readable file is newer than
	/// its baseline. Unreadable files are skipped so a mid-rename gap doesn't
	/// fire a reload.
	fn advanced(&mut self) -> bool {
		let mut changed = false;
		for (path, slot) in self.paths.iter().zip(self.mtimes.iter_mut()) {
			let current = mtime(path);
			if current.is_some() && *slot != current {
				*slot = current;
				changed = true;
			}
		}
		changed
	}
}

fn mtime(path: &Path) -> Option<SystemTime> {
	std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	// `changed`'s OS-event and polling paths are timing-dependent, so this
	// exercises the deterministic core: `advanced`'s mtime comparison, which is
	// what both paths gate on.
	#[test]
	fn advanced_reports_newer_files_and_skips_missing() {
		let dir = tempfile::tempdir().unwrap();
		let cert = dir.path().join("cert.pem");
		std::fs::write(&cert, "v1").unwrap();

		let mut watcher = FileWatcher::new(vec![cert.clone()]);
		// Baseline captured the current mtime, so nothing has moved yet.
		assert!(!watcher.advanced());

		// Rewind the baseline to stand in for the file being newer than last seen.
		watcher.mtimes[0] = None;
		assert!(
			watcher.advanced(),
			"a readable file newer than its baseline must register"
		);
		// The baseline now matches, so a re-scan reports no further change.
		assert!(!watcher.advanced());

		// An unreadable (missing) file is skipped, never reported as a change.
		let mut missing = FileWatcher::new(vec![dir.path().join("absent.pem")]);
		assert!(!missing.advanced());
	}
}
