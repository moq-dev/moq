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
pub struct FileWatcher {
	paths: Vec<PathBuf>,
	mtimes: Vec<Option<SystemTime>>,
	// Holds the OS watcher alive (dropping it stops events). `None` => polling.
	_watcher: Option<notify::RecommendedWatcher>,
	events: Option<mpsc::UnboundedReceiver<notify::Result<notify::Event>>>,
}

impl FileWatcher {
	/// Begin watching `paths`. Records their current mtimes as the baseline so
	/// [`changed`](Self::changed) only resolves on a subsequent change.
	pub fn new(paths: Vec<PathBuf>) -> Self {
		let (tx, rx) = mpsc::unbounded_channel();
		let watcher = notify::recommended_watcher(move |event| {
			let _ = tx.send(event);
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
	pub async fn changed(&mut self) {
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
