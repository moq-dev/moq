//! Disk and remote cache tiers backed by [`object_store`].
//!
//! A `Store` persists flush bands from the RAM tier as segments (one object each), serves a group
//! by ranged-reading its blob, and compacts: once the disk tier is over its bounds, the oldest
//! segments roll up into one remote object (or are evicted if there is no remote tier). All the
//! decisions live in the `index` module; this module is the object_store glue.

use std::ops::Range;
use std::sync::Arc;

use object_store::{ObjectStore, PutPayload, path::Path};

use super::index::{Index, SegmentId, Tier};
use super::segment::{self, Segment};
use super::{Batch, Bounds, Group};

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

/// A tiered durable store: a disk object store, an optional remote one, and the index mapping
/// group sequences to their location. Bands flushed from the RAM tier land here; old disk segments
/// roll up into the remote tier, or are evicted when there is none.
pub struct Store {
	disk: Arc<dyn ObjectStore>,
	remote: Option<Arc<dyn ObjectStore>>,
	bounds: Bounds,
	prefix: Path,
	index: Index,
}

impl Store {
	/// Create a store over `disk` with an optional `remote` tier, keyed under `prefix`. `bounds`
	/// caps the disk tier; exceeding the high watermark promotes (or evicts) the oldest segments.
	pub fn new(disk: Arc<dyn ObjectStore>, remote: Option<Arc<dyn ObjectStore>>, prefix: Path, bounds: Bounds) -> Self {
		Self {
			disk,
			remote,
			bounds,
			prefix,
			index: Index::new(),
		}
	}

	fn store_of(&self, tier: Tier) -> &Arc<dyn ObjectStore> {
		match tier {
			// A remote location is only ever recorded when a remote tier is configured (see
			// `compact`), so this never falls back.
			Tier::Remote => self
				.remote
				.as_ref()
				.expect("a remote location implies a configured remote tier"),
			Tier::Disk => &self.disk,
		}
	}

	fn key(&self, tier: Tier, id: SegmentId) -> Path {
		let dir = match tier {
			Tier::Disk => "disk",
			Tier::Remote => "remote",
		};
		self.prefix.child(dir).child(id.to_string())
	}

	/// Persist a flushed band as one disk segment, then compact if the disk tier is over budget.
	pub async fn flush(&mut self, batch: Batch) -> Result<(), Error> {
		if batch.is_empty() {
			return Ok(());
		}
		let bytes = segment::encode(&batch)?;
		let segment = Segment::open(bytes.clone())?;
		// Write the object before recording it, so a failed put leaves the index unchanged.
		let id = self.index.next_id();
		self.disk
			.put(&self.key(Tier::Disk, id), PutPayload::from_bytes(bytes))
			.await?;
		let added = self.index.add(Tier::Disk, &segment);
		debug_assert_eq!(added, id, "index id drifted from the written key");
		self.compact().await
	}

	/// Fetch a group by sequence: locate it, ranged-read its blob, decode it. `None` if not stored.
	pub async fn get(&self, sequence: u64) -> Result<Option<Group>, Error> {
		let Some(loc) = self.index.locate(sequence) else {
			return Ok(None);
		};
		let end = loc.offset.checked_add(loc.length).ok_or(segment::Error::Truncated)?;
		let range: Range<u64> = loc.offset..end;
		let bytes = self
			.store_of(loc.tier)
			.get_range(&self.key(loc.tier, loc.segment), range)
			.await?;
		Ok(Some(segment::group_from_blob(sequence, bytes)?))
	}

	/// Bring the disk tier within bounds: roll the oldest segments up into one remote object, or
	/// evict them when there is no remote tier. A no-op when the disk tier is within bounds.
	pub async fn compact(&mut self) -> Result<(), Error> {
		let promoted = self.index.promotion(self.bounds);
		if promoted.is_empty() {
			return Ok(());
		}

		match self.remote.clone() {
			Some(remote) => {
				// Read the promoted disk segments whole and roll them into one.
				let mut segments = Vec::with_capacity(promoted.len());
				for id in &promoted {
					let bytes = self.disk.get(&self.key(Tier::Disk, *id)).await?.bytes().await?;
					segments.push(bytes);
				}
				let rolled = segment::rollup(&segments)?;
				let rolled_segment = Segment::open(rolled.clone())?;
				// Upload the remote object before repointing the index, so a failed put leaves the
				// index (still pointing at the disk segments) intact.
				let new_id = self.index.next_id();
				remote
					.put(&self.key(Tier::Remote, new_id), PutPayload::from_bytes(rolled))
					.await?;
				let applied = self.index.apply_promotion(&promoted, &rolled_segment);
				debug_assert_eq!(applied, new_id, "index id drifted from the uploaded key");
				// Best-effort cleanup; an index now pointing at remote makes any leftover disk
				// objects orphans, not inconsistency.
				for id in &promoted {
					self.disk.delete(&self.key(Tier::Disk, *id)).await?;
				}
			}
			None => {
				// No remote tier: drop the oldest disk segments outright.
				for id in &promoted {
					self.disk.delete(&self.key(Tier::Disk, *id)).await?;
				}
				self.index.evict(&promoted);
			}
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

	#[tokio::test]
	async fn flush_and_get_from_disk() {
		let mut store = Store::new(memory(), None, Path::from("cache"), Bounds::default());
		store.flush(vec![group(0, 10), group(1, 20)]).await.unwrap();

		assert_eq!(store.get(0).await.unwrap().unwrap(), group(0, 10));
		assert_eq!(store.get(1).await.unwrap().unwrap(), group(1, 20));
		assert!(store.get(99).await.unwrap().is_none());
	}

	#[tokio::test]
	async fn promotes_to_remote_over_budget() {
		// Segments are ~1 KB; keep ~1 in disk (min 1100), promote at 2 (max 2000).
		let bounds = Bounds::new(Limit::bytes(1100), Limit::bytes(2000));
		let mut store = Store::new(memory(), Some(memory()), Path::from("cache"), bounds);

		for seq in 0..5 {
			store.flush(vec![group(seq, 1000)]).await.unwrap();
		}

		// Every group is still readable, whether it stayed on disk or rolled up to remote.
		for seq in 0..5 {
			assert_eq!(store.get(seq).await.unwrap().unwrap(), group(seq, 1000));
		}
		// Some bytes ended up in the remote tier.
		assert!(store.index.bytes(Tier::Remote) > 0);
	}

	#[tokio::test]
	async fn evicts_oldest_without_remote() {
		let bounds = Bounds::new(Limit::bytes(1100), Limit::bytes(2000));
		let mut store = Store::new(memory(), None, Path::from("cache"), bounds);

		for seq in 0..5 {
			store.flush(vec![group(seq, 1000)]).await.unwrap();
		}

		// The newest group is retained; the oldest was evicted (no remote to promote into).
		assert_eq!(store.get(4).await.unwrap().unwrap(), group(4, 1000));
		assert!(store.get(0).await.unwrap().is_none());
	}
}
