use std::path::{Path, PathBuf};

use notify::Watcher;
use tokio::sync::mpsc;

/// Watches a set of files and resolves whenever something changes in their
/// directories.
///
/// Reacting to the filesystem (rather than a SIGHUP/SIGUSR1) is what lets
/// cert-manager, Kubernetes secret mounts, and `mv`-into-place rotate files with
/// no extra signalling: they rewrite the file and the watcher fires.
///
/// Watches each file's *parent directory*, not the file itself. Editors,
/// cert-manager, and K8s secret mounts replace files by atomic rename or symlink
/// swap, which changes the inode (and, for the K8s `..data` symlink, fires on the
/// directory without ever naming the file), so a watch set directly on the path
/// would be missed.
pub(crate) struct FileWatcher {
	// Holds the OS watcher alive; dropping it stops events.
	_watcher: notify::RecommendedWatcher,
	events: mpsc::Receiver<()>,
}

impl FileWatcher {
	/// Start watching the parent directories of `paths`. Errors if the OS watcher
	/// can't be created or a directory can't be watched (e.g. the inotify
	/// instance/watch limit is hit). `notify` already falls back to a built-in
	/// poll watcher on platforms without a native backend, so there's no manual
	/// polling here.
	pub(crate) fn new(paths: &[PathBuf]) -> notify::Result<Self> {
		// A capacity-1 channel of unit wakeups coalesces the burst of raw events
		// notify emits per change (and any unrelated churn in the directory): a
		// full buffer already has a pending wakeup, so extra sends are dropped.
		let (tx, rx) = mpsc::channel(1);
		let mut watcher = notify::recommended_watcher(move |_event| {
			let _ = tx.try_send(());
		})?;

		// Watch each distinct parent directory once.
		let mut dirs: Vec<&Path> = paths.iter().filter_map(|p| p.parent()).collect();
		dirs.sort_unstable();
		dirs.dedup();
		for dir in dirs {
			watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
		}

		Ok(Self {
			_watcher: watcher,
			events: rx,
		})
	}

	/// Resolve once the OS reports activity in a watched directory. The caller
	/// reloads on return; reloads are idempotent, so the coarse "something
	/// changed" granularity at worst costs an occasional redundant reload.
	pub(crate) async fn changed(&mut self) {
		// The sender lives inside `_watcher`, which we hold for `&mut self`, so the
		// channel can't be closed here.
		self.events
			.recv()
			.await
			.expect("file watcher channel closed unexpectedly");
	}
}
