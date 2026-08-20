//! Which rungs are encoding right now.
//!
//! Nothing is encoded until a consumer asks for a rung, so a transcoder that is
//! publishing a catalog and a transcoder that is saturating a GPU look identical
//! from the outside. Broadcast demand ([`moq_net::broadcast::Demand`]) closes
//! half the gap: it says *someone* is watching. [`Active`] closes the other half
//! by naming *which renditions* are being produced, which is what a caller
//! pricing the work, metering it, or advertising a route's cost actually needs.
//!
//! Entries are reference counted, because the live path and any number of group
//! fetches encode the same rung concurrently; a rung leaves the set once the
//! last of them finishes. Watchers are only woken when the set of names changes,
//! so a fetch overlapping a live session is not an edge.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::watch;

use crate::catalog::Resolved;

/// One rung currently being encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Encoding {
	/// The rendition/track name, e.g. `video/360p`.
	pub name: String,
	/// The output resolution, derived from the source aspect ratio.
	pub size: moq_video::Size,
	/// The target bitrate, in bits per second.
	pub bitrate: u64,
	/// The output framerate, inherited from the source.
	pub framerate: u32,
}

impl From<&Resolved> for Encoding {
	fn from(rung: &Resolved) -> Self {
		Self {
			name: rung.name.clone(),
			size: rung.size,
			bitrate: rung.bitrate,
			framerate: rung.framerate,
		}
	}
}

/// One entry in the set: the rung plus how many pipelines are encoding it.
#[derive(Clone, Debug)]
struct Entry {
	encoding: Encoding,
	refs: usize,
}

/// A cloneable, watch-only view of which rungs are encoding right now.
///
/// Construct one, hand it to [`Config::active`](crate::Config::active), and read
/// it with [`get`](Self::get) or [`changed`](Self::changed). It is inert until
/// [`run`](crate::run) is driving it, and empty again once that future is
/// dropped; it neither keeps the transcode alive nor counts as demand itself.
///
/// ```no_run
/// # async fn example(active: &mut moq_transcode::Active) {
/// loop {
///     for (name, rung) in active.changed().await {
///         println!("{name} is {}x{}", rung.size.width, rung.size.height);
///     }
/// }
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Active {
	tx: Arc<watch::Sender<BTreeMap<String, Entry>>>,
	rx: watch::Receiver<BTreeMap<String, Entry>>,
}

impl Default for Active {
	fn default() -> Self {
		Self::new()
	}
}

impl Active {
	/// An empty set, to be filled in by the [`run`](crate::run) it is given to.
	pub fn new() -> Self {
		let (tx, rx) = watch::channel(BTreeMap::new());
		Self { tx: Arc::new(tx), rx }
	}

	/// The rungs encoding right now, by track name.
	///
	/// A point-in-time snapshot with no registration; use [`changed`](Self::changed)
	/// to wait for the next edge.
	pub fn get(&self) -> BTreeMap<String, Encoding> {
		Self::snapshot(&self.rx.borrow())
	}

	/// Wait until the set of encoding rungs differs from the last one this handle
	/// returned, then return the new set.
	///
	/// Each clone tracks its own position, so cloning is how you get a second
	/// independent watcher. Resolves immediately the first time if any rung is
	/// already encoding. Pends forever once every [`run`](crate::run) holding the
	/// other half is gone, since nothing can change the set after that.
	pub async fn changed(&mut self) -> BTreeMap<String, Encoding> {
		// A sender is held inside this handle, so `changed` only errors if the
		// channel is closed, which cannot happen while `self` is alive.
		if self.rx.changed().await.is_err() {
			std::future::pending::<()>().await;
		}
		Self::snapshot(&self.rx.borrow_and_update())
	}

	fn snapshot(entries: &BTreeMap<String, Entry>) -> BTreeMap<String, Encoding> {
		entries
			.iter()
			.map(|(name, entry)| (name.clone(), entry.encoding.clone()))
			.collect()
	}

	/// Mark `rung` as encoding until the returned guard drops.
	pub(crate) fn enter(&self, rung: &Resolved) -> Guard {
		let name = rung.name.clone();
		self.tx.send_if_modified(|entries| match entries.get_mut(&name) {
			// Already encoding on another pipeline: the set is unchanged, so
			// don't wake watchers.
			Some(entry) => {
				entry.refs += 1;
				false
			}
			None => {
				entries.insert(
					name.clone(),
					Entry {
						encoding: rung.into(),
						refs: 1,
					},
				);
				true
			}
		});

		Guard {
			active: self.clone(),
			name,
		}
	}
}

/// Holds a rung in the [`Active`] set for as long as it is alive.
///
/// RAII rather than an explicit release: every encode path is cancelled by being
/// dropped (a rung whose demand goes away, a fetch aborted with its `JoinSet`),
/// so a release call would be skipped exactly when it matters and leak the rung
/// into the set forever.
pub(crate) struct Guard {
	active: Active,
	name: String,
}

impl Drop for Guard {
	fn drop(&mut self) {
		self.active.tx.send_if_modified(|entries| {
			let Some(entry) = entries.get_mut(&self.name) else {
				return false;
			};
			entry.refs -= 1;
			if entry.refs > 0 {
				return false;
			}
			entries.remove(&self.name);
			true
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn resolved(name: &str, height: u32) -> Resolved {
		Resolved {
			name: name.to_string(),
			size: moq_video::Size::new(height * 16 / 9, height),
			bitrate: 100_000,
			framerate: 30,
		}
	}

	#[tokio::test]
	async fn tracks_entries_and_wakes_on_set_changes() {
		let active = Active::new();
		let mut watcher = active.clone();
		assert!(active.get().is_empty());

		let guard = active.enter(&resolved("video/360p", 360));
		let seen = watcher.changed().await;
		assert_eq!(seen.keys().collect::<Vec<_>>(), ["video/360p"]);
		assert_eq!(seen["video/360p"].size.height, 360);

		drop(guard);
		assert!(watcher.changed().await.is_empty());
	}

	/// A fetch overlapping a live session must not look like an edge: the set of
	/// names is what a meter integrates, and flapping it would double-count.
	#[tokio::test]
	async fn concurrent_pipelines_are_one_entry() {
		let active = Active::new();
		let mut watcher = active.clone();

		let live = active.enter(&resolved("video/360p", 360));
		watcher.changed().await;

		let fetch = active.enter(&resolved("video/360p", 360));
		let other = active.enter(&resolved("video/240p", 240));
		// Only the second NAME is an edge; the duplicate is not.
		assert_eq!(watcher.changed().await.len(), 2);

		drop(fetch);
		// Still live, so still one entry for it and no edge from the release.
		assert_eq!(active.get().len(), 2);
		drop(live);
		assert_eq!(watcher.changed().await.keys().collect::<Vec<_>>(), ["video/240p"]);
		drop(other);
		assert!(watcher.changed().await.is_empty());
	}

	/// A clone must never miss what is already encoding: a metering loop that
	/// only ever awaits `changed` still has to see a rung that started before it
	/// existed, or that rung encodes for free.
	#[tokio::test]
	async fn clones_never_miss_the_current_set() {
		let active = Active::new();
		let _guard = active.enter(&resolved("video/480p", 480));

		let mut fresh = active.clone();
		assert_eq!(fresh.changed().await.len(), 1);
		// Caught up: now it waits for a real change rather than spinning.
		assert!(
			tokio::time::timeout(std::time::Duration::from_millis(50), fresh.changed())
				.await
				.is_err()
		);
	}
}
