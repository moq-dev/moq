//! Disk and remote cache tiers backed by [`object_store`].
//!
//! A `Store` persists flush bands from the RAM tier as segments (one object each), serves a group
//! by ranged-reading its blob, and compacts: once the disk tier is over its bounds, the oldest
//! segments roll up into one remote object (or are evicted if there is no remote tier).
//!
//! Object I/O never holds a lock. The only shared mutable state is the in-RAM `Index`, guarded by
//! a `Mutex` taken solely for synchronous lookups and updates and dropped before any `.await`. (A
//! `std::sync::MutexGuard` isn't `Send`, so holding one across `.await` wouldn't even compile, which
//! makes the "lock held across I/O" mistake structurally impossible.) The index is the source of
//! truth for what lives where; the object stores are consulted only to read, write, or delete a blob.

use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};

use object_store::{ObjectStore, PutPayload, path::Path};

use super::index::{Index, SegmentId, Tier};
use super::segment::{self, Segment};
use super::{Batch, Bounds, Group, Remote};

/// An error from a tiered [`Store`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// A segment failed to encode or decode.
	#[error(transparent)]
	Segment(#[from] segment::Error),
	/// The backing object store failed.
	#[error(transparent)]
	Store(#[from] object_store::Error),
}

/// A tiered durable store: a disk object store, an optional remote tier, and an in-RAM index mapping
/// group sequences to their location. Bands flushed from the RAM tier land here; old disk segments
/// roll up into the remote tier, or are evicted when there is none.
///
/// All methods take `&self`; the only lock is the index `Mutex`, never held across object I/O.
/// [`flush`](Self::flush) and [`compact`](Self::compact) mutate the index and must be driven
/// serially (a single flush driver); [`get`](Self::get) only reads it and may run concurrently.
pub struct Store {
	disk: Arc<dyn ObjectStore>,
	disk_prefix: Path,
	remote: Option<Remote>,
	bounds: Bounds,
	index: Mutex<Index>,
}

impl Store {
	/// Create a store over a disk tier (keyed under `disk_prefix`, capped by `bounds`) and an
	/// optional remote tier. Exceeding the disk high watermark promotes the oldest segments to the
	/// remote tier, or evicts them when there is none.
	pub fn new(disk: Arc<dyn ObjectStore>, disk_prefix: Path, bounds: Bounds, remote: Option<Remote>) -> Self {
		Self {
			disk,
			disk_prefix,
			remote,
			bounds,
			index: Mutex::new(Index::new()),
		}
	}

	fn index(&self) -> MutexGuard<'_, Index> {
		self.index.lock().expect("cache index poisoned")
	}

	fn object_store(&self, tier: Tier) -> &Arc<dyn ObjectStore> {
		match tier {
			// A remote location is only ever recorded when a remote tier is configured (see
			// `compact`), so this never falls back.
			Tier::Remote => {
				&self
					.remote
					.as_ref()
					.expect("a remote location implies a configured remote tier")
					.store
			}
			Tier::Disk => &self.disk,
		}
	}

	fn key(&self, tier: Tier, id: SegmentId) -> Path {
		match tier {
			Tier::Disk => self.disk_prefix.child(id.to_string()),
			Tier::Remote => self
				.remote
				.as_ref()
				.expect("a remote location implies a configured remote tier")
				.prefix
				.child(id.to_string()),
		}
	}

	/// Persist a flushed band as one disk segment. Compaction is separate (the flush driver calls
	/// [`compact`](Self::compact) after).
	pub async fn flush(&self, batch: Batch) -> Result<(), Error> {
		if batch.is_empty() {
			return Ok(());
		}
		let bytes = segment::encode(&batch)?;
		let segment = Segment::open(bytes.clone())?;
		// Reserve the id, write the object (unlocked), then record it; a failed put leaves the index
		// unchanged. The flush driver is serial, so the reserved id is still next when we add.
		let id = self.index().next_id();
		self.disk
			.put(&self.key(Tier::Disk, id), PutPayload::from_bytes(bytes))
			.await?;
		let added = self.index().add(Tier::Disk, &segment);
		debug_assert_eq!(added, id, "index id drifted from the written key");
		Ok(())
	}

	/// Fetch a group by sequence: locate it (index lock), ranged-read its blob (unlocked), decode it.
	/// `None` if not stored.
	pub async fn get(&self, sequence: u64) -> Result<Option<Group>, Error> {
		let Some(loc) = self.index().locate(sequence) else {
			return Ok(None);
		};
		let end = loc.offset.checked_add(loc.length).ok_or(segment::Error::Truncated)?;
		let range: Range<u64> = loc.offset..end;
		let bytes = self
			.object_store(loc.tier)
			.get_range(&self.key(loc.tier, loc.segment), range)
			.await?;
		Ok(Some(segment::group_from_blob(sequence, bytes)?))
	}

	/// Bring the disk tier within bounds: roll the oldest disk segments up into one remote object, or
	/// evict them when there is no remote tier. A no-op when the disk tier is within bounds.
	///
	/// No object I/O holds the index lock: the index is locked only to pick the promotion, reserve
	/// the remote id, repoint, and evict. The disk segments stay in place and indexed until the
	/// repoint, so a concurrent [`get`](Self::get) still reads them; a failed upload leaves the index
	/// pointing at the intact disk segments (safe to retry).
	pub async fn compact(&self) -> Result<(), Error> {
		let promoted = self.index().promotion(self.bounds);
		if promoted.is_empty() {
			return Ok(());
		}

		let Some(remote) = &self.remote else {
			// No remote tier: delete the oldest disk segments (unlocked), then drop them from the index.
			for id in &promoted {
				self.disk.delete(&self.key(Tier::Disk, *id)).await?;
			}
			self.index().evict(&promoted);
			return Ok(());
		};

		// Read the promoted disk segments whole (unlocked) and roll them into one.
		let mut segments = Vec::with_capacity(promoted.len());
		for id in &promoted {
			let bytes = self.disk.get(&self.key(Tier::Disk, *id)).await?.bytes().await?;
			segments.push(bytes);
		}
		let rolled = segment::rollup(&segments)?;
		let rolled_segment = Segment::open(rolled.clone())?;

		// Reserve the remote id/key, upload (unlocked, before repointing so a failed put leaves the
		// index on the disk segments), then repoint the index at the remote object.
		let new_id = self.index().next_id();
		let key = remote.prefix.child(new_id.to_string());
		remote.store.put(&key, PutPayload::from_bytes(rolled)).await?;
		let applied = self.index().apply_promotion(&promoted, &rolled_segment);
		debug_assert_eq!(applied, new_id, "index id drifted from the uploaded key");

		// Best-effort cleanup of the now-orphaned disk objects (unlocked).
		for id in &promoted {
			self.disk.delete(&self.key(Tier::Disk, *id)).await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::super::{Frame, Limit};
	use super::*;
	use bytes::Bytes;
	use object_store::memory::InMemory;

	/// A one-frame untimed group of `bytes` bytes at the given sequence.
	fn group(sequence: u64, bytes: usize) -> Group {
		Group {
			sequence,
			frames: vec![Frame {
				timestamp: None,
				payload: Bytes::from(vec![sequence as u8; bytes]),
			}],
		}
	}

	fn memory() -> Arc<dyn ObjectStore> {
		Arc::new(InMemory::new())
	}

	fn remote() -> Remote {
		Remote::new(memory(), Path::from("remote"))
	}

	#[tokio::test]
	async fn flush_and_get_from_disk() {
		let store = Store::new(memory(), Path::from("disk"), Bounds::default(), None);
		store.flush(vec![group(0, 10), group(1, 20)]).await.unwrap();

		assert_eq!(store.get(0).await.unwrap().unwrap(), group(0, 10));
		assert_eq!(store.get(1).await.unwrap().unwrap(), group(1, 20));
		assert!(store.get(99).await.unwrap().is_none());
	}

	#[tokio::test]
	async fn promotes_to_remote_over_budget() {
		// Segments are ~1 KB; keep ~1 in disk (min 1100), promote at 2 (max 2000).
		let bounds = Bounds::new(Limit::bytes(1100), Limit::bytes(2000));
		let store = Store::new(memory(), Path::from("disk"), bounds, Some(remote()));

		for seq in 0..5 {
			store.flush(vec![group(seq, 1000)]).await.unwrap();
			store.compact().await.unwrap();
		}

		// Every group is still readable, whether it stayed on disk or rolled up to remote.
		for seq in 0..5 {
			assert_eq!(store.get(seq).await.unwrap().unwrap(), group(seq, 1000));
		}
		// Some bytes ended up in the remote tier.
		assert!(store.index().bytes(Tier::Remote) > 0);
	}

	#[tokio::test]
	async fn evicts_oldest_without_remote() {
		let bounds = Bounds::new(Limit::bytes(1100), Limit::bytes(2000));
		let store = Store::new(memory(), Path::from("disk"), bounds, None);

		for seq in 0..5 {
			store.flush(vec![group(seq, 1000)]).await.unwrap();
			store.compact().await.unwrap();
		}

		// The newest group is retained; the oldest was evicted (no remote to promote into).
		assert_eq!(store.get(4).await.unwrap().unwrap(), group(4, 1000));
		assert!(store.get(0).await.unwrap().is_none());
	}
}
