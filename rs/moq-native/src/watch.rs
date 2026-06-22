//! Watch on-disk files (TLS certs/keys) and get notified when they're rotated.

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
pub struct FileWatcher {
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
	pub fn new(paths: &[PathBuf]) -> notify::Result<Self> {
		// A capacity-1 channel of unit wakeups coalesces the burst of raw events
		// notify emits per change (and any unrelated churn in the directory): a
		// full buffer already has a pending wakeup, so extra sends are dropped.
		let (tx, rx) = mpsc::channel(1);
		let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
			let send = match res {
				Ok(event) => is_reload_trigger(&event.kind),
				// A watcher error (e.g. inotify queue overflow) may mean we missed a
				// real change, so reload to be safe.
				Err(_) => true,
			};
			if send {
				let _ = tx.try_send(());
			}
		})?;

		// Watch each distinct parent directory once. A bare filename like
		// `cert.pem` has an empty-string parent (`Some("")`, not `None`), which the
		// OS watcher rejects with "No path was found", so map that to the current
		// directory.
		let mut dirs: Vec<&Path> = paths
			.iter()
			.filter_map(|p| p.parent())
			.map(|p| if p.as_os_str().is_empty() { Path::new(".") } else { p })
			.collect();
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
	pub async fn changed(&mut self) {
		// The sender lives inside `_watcher`, which we hold for `&mut self`, so the
		// channel can't be closed here.
		self.events
			.recv()
			.await
			.expect("file watcher channel closed unexpectedly");
	}
}

/// Whether a raw notify event reflects a real change that should trigger a reload.
///
/// The reload path opens and reads the watched files, and notify's inotify backend
/// reports IN_OPEN/IN_ACCESS for those reads. Treating them as changes makes a reload
/// re-trigger itself in a tight loop (a ~400/sec storm that starved TLS handshakes in
/// production), so we react only to events that can mean new cert bytes: a create, a
/// modify/rename, or a finished write (IN_CLOSE_WRITE).
fn is_reload_trigger(kind: &notify::EventKind) -> bool {
	use notify::EventKind;
	use notify::event::{AccessKind, AccessMode};
	match kind {
		// A finished write is the only access event that signals new content. The
		// reload opens and reads these files itself (and other processes may read them
		// too), so open/read/close-without-write must be ignored or it loops forever.
		EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
		EventKind::Access(_) => false,
		// Rotations arrive as a create or a modify/rename: cert-manager and
		// mv-into-place rename over the file, the K8s `..data` symlink swap fires a
		// directory rename, and in-place rewrites modify the data.
		EventKind::Create(_) | EventKind::Modify(_) => true,
		// A bare removal leaves nothing to load (wait for the replacement's create),
		// and Any/Other are unclassified noise we don't act on.
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// A bare filename has a `Some("")` parent; watching "" is rejected by the OS
	// watcher, so `new` must fall back to the current directory rather than error.
	#[test]
	fn bare_filename_watches_current_dir() {
		FileWatcher::new(&[PathBuf::from("cert.pem"), PathBuf::from("key.pem")])
			.expect("bare filenames should watch the current directory");
	}

	// The reload reads its own files; reads and bare removals must not re-trigger it.
	#[test]
	fn ignored_events_do_not_trigger_reload() {
		use notify::EventKind;
		use notify::event::{AccessKind, AccessMode, RemoveKind};
		assert!(!is_reload_trigger(&EventKind::Access(AccessKind::Read)));
		assert!(!is_reload_trigger(&EventKind::Access(AccessKind::Open(
			AccessMode::Read
		))));
		assert!(!is_reload_trigger(&EventKind::Access(AccessKind::Open(
			AccessMode::Any
		))));
		assert!(!is_reload_trigger(&EventKind::Access(AccessKind::Close(
			AccessMode::Read
		))));
		assert!(!is_reload_trigger(&EventKind::Remove(RemoveKind::Any)));
	}

	// A finished write, a create, and a modify/rename are real rotations.
	#[test]
	fn writes_and_rotations_trigger_reload() {
		use notify::EventKind;
		use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind};
		assert!(is_reload_trigger(&EventKind::Access(AccessKind::Close(
			AccessMode::Write
		))));
		assert!(is_reload_trigger(&EventKind::Create(CreateKind::Any)));
		assert!(is_reload_trigger(&EventKind::Modify(ModifyKind::Any)));
	}
}
