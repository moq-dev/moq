//! Watch on-disk TLS certificates, keys, and root CAs for rotation.

use std::path::{Path, PathBuf};

use notify::Watcher;
use tokio::sync::mpsc;

/// Watches a set of files and resolves whenever one of them changes.
///
/// Reacting to the filesystem (rather than a SIGHUP/SIGUSR1) is what lets
/// cert-manager, Kubernetes secret mounts, and `mv`-into-place rotate files with
/// no extra signalling: they rewrite the file and the watcher fires.
///
/// Watches each file's *parent directory*, not the file itself. Editors,
/// cert-manager, and K8s secret mounts replace files by atomic rename or symlink
/// swap, which changes the inode (and, for the K8s `..data` symlink, fires on the
/// directory without ever naming the file), so a watch set directly on the path
/// would be missed. Events for anything else in those directories are ignored.
pub struct Files {
	// Holds the OS watcher alive; dropping it stops events.
	_watcher: notify::RecommendedWatcher,
	events: mpsc::Receiver<()>,
}

impl Files {
	/// Start watching the parent directories of `paths`. Errors if the OS watcher
	/// can't be created or a directory can't be watched (e.g. the inotify
	/// instance/watch limit is hit). `notify` already falls back to a built-in
	/// poll watcher on platforms without a native backend, so there's no manual
	/// polling here.
	pub fn new(paths: &[PathBuf]) -> notify::Result<Self> {
		// A capacity-1 channel of unit wakeups coalesces the burst of raw events
		// notify emits per change: a full buffer already has a pending wakeup, so
		// extra sends are dropped.
		let (tx, rx) = mpsc::channel(1);
		let watcher = callback(paths, move || {
			let _ = tx.try_send(());
		})?;

		Ok(Self {
			_watcher: watcher,
			events: rx,
		})
	}

	/// Resolve once the OS reports a change to a watched file. The caller reloads
	/// on return; reloads are idempotent, so a redundant wakeup costs only a
	/// redundant reload.
	pub async fn changed(&mut self) {
		// The sender lives inside `_watcher`, which we hold for `&mut self`, so the
		// channel can't be closed here.
		self.events
			.recv()
			.await
			.expect("file watcher channel closed unexpectedly");
	}
}

/// Watch `paths` and invoke `changed` after a write or atomic replacement.
///
/// The callback runs on notify's worker thread, so it must finish promptly.
pub(crate) fn callback(
	paths: &[PathBuf],
	mut changed: impl FnMut() + Send + 'static,
) -> notify::Result<notify::RecommendedWatcher> {
	let mut filter = Filter::new(paths);
	let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
		let reload = match res {
			Ok(event) => filter.matches(&event),
			// A watcher error may mean we missed a real change, so reload to be safe.
			Err(_) => true,
		};
		if reload {
			changed();
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

	Ok(watcher)
}

/// Picks the events in the watched directories that concern the watched files.
///
/// Anything else sharing a directory must be ignored: a relay logging into its
/// certificate directory otherwise reloads on every line, and each reload logs
/// another line, a self-sustaining storm.
struct Filter {
	paths: Vec<PathBuf>,
	/// Each path with its directory canonicalized, as `locate` spells an event's path.
	located: Vec<PathBuf>,
	/// Where each path currently resolves through its symlinks, `None` while it
	/// doesn't resolve at all.
	resolved: Vec<Option<PathBuf>>,
}

impl Filter {
	fn new(paths: &[PathBuf]) -> Self {
		let paths = paths.to_vec();
		let located = paths.iter().filter_map(|p| locate(p)).collect();
		let resolved = resolve(&paths);
		Self {
			paths,
			located,
			resolved,
		}
	}

	fn matches(&mut self, event: &notify::Event) -> bool {
		// The OS dropped events (e.g. an inotify queue overflow), so a real change
		// may have gone unseen.
		if event.need_rescan() {
			return true;
		}
		if !is_reload_trigger(&event.kind) {
			return false;
		}

		let resolved = resolve(&self.paths);

		// An event names a watched file by its own path or, for a symlink, by the
		// target it resolves to (an in-place write reports the target's name).
		// Compared with the directory canonicalized, since backends spell it
		// differently (FSEvents reports canonical paths). The file name check first
		// skips that syscall for the unrelated churn that dominates these directories.
		let watched = || self.located.iter().chain(resolved.iter().flatten());
		let named = event.paths.iter().any(|p| {
			let name = p.file_name();
			name.is_some()
				&& watched().any(|f| f.file_name() == name)
				&& locate(p).is_some_and(|p| watched().any(|f| *f == p))
		});

		// A Kubernetes secret mount links `cert.pem` to `..data/cert.pem` and rotates
		// by swapping the `..data` symlink, so no event ever names the file. Catch
		// that by where the paths resolve, refreshed on every event so a rotation
		// that was named doesn't fire again on the next unrelated one.
		let moved = resolved != self.resolved;
		self.resolved = resolved;

		named || moved
	}
}

fn locate(path: &Path) -> Option<PathBuf> {
	let name = path.file_name()?;
	let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
	Some(std::fs::canonicalize(dir.unwrap_or(Path::new("."))).ok()?.join(name))
}

fn resolve(paths: &[PathBuf]) -> Vec<Option<PathBuf>> {
	paths.iter().map(|p| std::fs::canonicalize(p).ok()).collect()
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
		match Files::new(&[PathBuf::from("cert.pem"), PathBuf::from("key.pem")]) {
			Ok(_) => {}
			Err(err) if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) => {
				eprintln!("skipping bare_filename_watches_current_dir: {err}");
			}
			Err(err) => panic!("bare filenames should watch the current directory: {err}"),
		}
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

	fn event(kind: notify::EventKind, path: PathBuf) -> notify::Event {
		notify::Event::new(kind).add_path(path)
	}

	// A log written into the certificate directory must not reload: each reload
	// logs a line, so reacting to it never stops.
	#[test]
	fn unrelated_files_do_not_trigger_reload() {
		use notify::EventKind;
		use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RenameMode};

		let dir = tempfile::TempDir::new().unwrap();
		let cert = dir.path().join("cert.pem");
		let key = dir.path().join("key.pem");
		let log = dir.path().join("relay.log");
		let mut filter = Filter::new(&[cert.clone(), key.clone()]);

		let write = EventKind::Modify(ModifyKind::Data(DataChange::Any));
		let closed = EventKind::Access(AccessKind::Close(AccessMode::Write));
		assert!(!filter.matches(&event(EventKind::Create(CreateKind::File), log.clone())));
		assert!(!filter.matches(&event(write, log.clone())));
		assert!(!filter.matches(&event(closed, log)));

		assert!(filter.matches(&event(closed, cert)));
		let renamed = EventKind::Modify(ModifyKind::Name(RenameMode::To));
		assert!(filter.matches(&event(renamed, key)));

		// Dropped events may have hidden a rotation.
		let rescan = notify::Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
		assert!(filter.matches(&rescan));
	}

	// With the certificate and key in different directories, a file in one that
	// shares the other's name is still unrelated.
	#[test]
	fn same_name_in_another_watched_directory_does_not_trigger_reload() {
		use notify::EventKind;
		use notify::event::{AccessKind, AccessMode};

		let certs = tempfile::TempDir::new().unwrap();
		let keys = tempfile::TempDir::new().unwrap();
		let mut filter = Filter::new(&[certs.path().join("cert.pem"), keys.path().join("key.pem")]);

		let closed = EventKind::Access(AccessKind::Close(AccessMode::Write));
		assert!(!filter.matches(&event(closed, keys.path().join("cert.pem"))));
		assert!(filter.matches(&event(closed, certs.path().join("cert.pem"))));
		assert!(filter.matches(&event(closed, keys.path().join("key.pem"))));
	}

	// A configured symlink whose target is rewritten in place reports the target's
	// name, and nothing resolves anywhere new.
	#[cfg(unix)]
	#[test]
	fn write_to_symlink_target_triggers_reload() {
		use notify::EventKind;
		use notify::event::{AccessKind, AccessMode};

		let dir = tempfile::TempDir::new().unwrap();
		std::fs::write(dir.path().join("actual.pem"), "v1").unwrap();
		std::os::unix::fs::symlink("actual.pem", dir.path().join("cert.pem")).unwrap();
		let mut filter = Filter::new(&[dir.path().join("cert.pem")]);

		let closed = EventKind::Access(AccessKind::Close(AccessMode::Write));
		assert!(filter.matches(&event(closed, dir.path().join("actual.pem"))));
		assert!(!filter.matches(&event(closed, dir.path().join("relay.log"))));
	}

	// Drives a real inotify watch through a Kubernetes secret rotation while a log
	// churns in the same directory. Each step blocks until its own events arrive, so
	// the filter judges every event against the disk as it was at that step.
	#[cfg(target_os = "linux")]
	#[test]
	fn kubernetes_rotation_reloads_and_log_writes_do_not() {
		use std::os::unix::fs::symlink;
		use std::sync::mpsc;

		// Lay out the mount like the kubelet: `tls.crt -> ..data/tls.crt` and
		// `..data -> ..<timestamp>`.
		let dir = tempfile::TempDir::new().unwrap();
		let path = |name: &str| dir.path().join(name);
		std::fs::create_dir(path("..v1")).unwrap();
		std::fs::write(path("..v1/tls.crt"), "v1").unwrap();
		symlink("..v1", path("..data")).unwrap();
		symlink("..data/tls.crt", path("tls.crt")).unwrap();

		let (tx, rx) = mpsc::channel();
		let mut watcher = match notify::recommended_watcher(tx) {
			Ok(watcher) => watcher,
			Err(err) if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) => {
				eprintln!("skipping kubernetes_rotation_reloads_and_log_writes_do_not: {err}");
				return;
			}
			Err(err) => panic!("failed to create watcher: {err}"),
		};
		if let Err(err) = watcher.watch(dir.path(), notify::RecursiveMode::NonRecursive) {
			if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) {
				eprintln!("skipping kubernetes_rotation_reloads_and_log_writes_do_not: {err}");
				return;
			}
			panic!("failed to watch: {err}");
		}
		let mut filter = Filter::new(&[path("tls.crt")]);

		// Judge every event up to and including the first that `done` accepts.
		let mut step = |done: &dyn Fn(&notify::Event) -> bool| -> Vec<bool> {
			let mut verdicts = Vec::new();
			loop {
				// The awaited event is already on its way; a lost one hangs until the
				// test runner's timeout.
				let event = rx.recv().expect("watcher dropped").unwrap();
				verdicts.push(filter.matches(&event));
				if done(&event) {
					return verdicts;
				}
			}
		};
		let named = |name: &'static str| {
			move |event: &notify::Event| event.paths.iter().any(|p| p.file_name() == Some(name.as_ref()))
		};
		let closed = |name: &'static str| {
			let named = named(name);
			move |event: &notify::Event| {
				use notify::event::{AccessKind, AccessMode};
				event.kind == notify::EventKind::Access(AccessKind::Close(AccessMode::Write)) && named(event)
			}
		};

		std::fs::write(path("relay.log"), "reloading server certificates\n").unwrap();
		let verdicts = step(&closed("relay.log"));
		assert!(
			verdicts.iter().all(|reload| !reload),
			"log write reloaded: {verdicts:?}"
		);

		// Rotate the way the kubelet does: write the new data, point `..data_tmp` at it,
		// rename that over `..data`, then remove the old data. No event names `tls.crt`.
		std::fs::create_dir(path("..v2")).unwrap();
		std::fs::write(path("..v2/tls.crt"), "v2").unwrap();
		symlink("..v2", path("..data_tmp")).unwrap();
		std::fs::rename(path("..data_tmp"), path("..data")).unwrap();
		std::fs::remove_dir_all(path("..v1")).unwrap();
		let verdicts = step(&named("..v1"));
		let reloads = verdicts.iter().filter(|reload| **reload).count();
		assert_eq!(reloads, 1, "rotation should reload once: {verdicts:?}");

		std::fs::write(path("relay.log"), "reloading server certificates\n").unwrap();
		let verdicts = step(&closed("relay.log"));
		assert!(
			verdicts.iter().all(|reload| !reload),
			"log write reloaded: {verdicts:?}"
		);
	}
}
