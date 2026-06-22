//! Per-track durable cache: the disk/remote spill tiers behind a track's live RAM window.
//!
//! The RAM tier is the track's own live group buffer ([`crate::TrackProducer`]'s `groups`); this
//! module is everything below it. When a group ages out of that window (see the two retention gates
//! in `track.rs`), it is serialized through `Group` and handed to the disk tier; a fetch that misses
//! the live window then reads it back from disk (or remote) instead of failing.
//!
//! A cache is local policy attached to a single track, independent of any retention the original
//! publisher set (it is never carried on the wire). Attach one with
//! [`crate::TrackProducer::with_cache`]; the disk tier and an optional remote rollup target are
//! described by `Disk`. Because the cache lives on the shared track state, the same store backs
//! the track's producer and every consumer, so a fetch is served from whichever tier holds the
//! group.
//!
//! The `segment` submodule is the on-disk byte format (a band of groups serialized as one
//! self-describing object) plus the rollup that concatenates several small segments into one larger
//! object. `Group::read` / `Group::produce` bridge a cached group to and from the live group model.
//! The `store` submodule is the object_store glue (native-only).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use super::{Timescale, Timestamp};

#[cfg(not(target_arch = "wasm32"))]
use object_store::{ObjectStore, path::Path};

// Internal orchestration for the disk/remote tiers; not part of the public surface, and only
// needed (and only buildable) where object_store is available.
#[cfg(not(target_arch = "wasm32"))]
mod index;

pub mod segment;

/// Disk and remote tiers backed by object_store. Native-only (object_store doesn't build on wasm).
#[cfg(not(target_arch = "wasm32"))]
pub mod store;

/// A cache bound, as a duration, a byte count, or both (the first to trip wins).
///
/// All-`None` means "no threshold": as a high watermark that is unbounded (never flush), as a low
/// watermark that is a floor of zero (drain everything but the latest group).
#[derive(Clone, Copy, Debug, Default)]
pub struct Limit {
	/// Bound on the span between the oldest and newest buffered group's media timestamps.
	pub duration: Option<Duration>,
	/// Bound on the total bytes of buffered group frames.
	pub bytes: Option<u64>,
}

impl Limit {
	/// A duration-only limit.
	pub fn duration(duration: Duration) -> Self {
		Self {
			duration: Some(duration),
			bytes: None,
		}
	}

	/// A byte-only limit.
	pub fn bytes(bytes: u64) -> Self {
		Self {
			duration: None,
			bytes: Some(bytes),
		}
	}

	/// Whether both thresholds are unset (so the limit imposes no ceiling).
	pub(crate) fn is_unset(&self) -> bool {
		self.duration.is_none() && self.bytes.is_none()
	}
}

/// A low/high watermark pair. The gap between them is the flush batch size.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bounds {
	/// Low watermark: a flush drains down to this.
	pub min: Limit,
	/// High watermark: exceeding it triggers a flush.
	pub max: Limit,
}

impl Bounds {
	/// Build bounds from a low and high watermark.
	pub fn new(min: Limit, max: Limit) -> Self {
		Self { min, max }
	}
}

/// The disk spill tier: an object store, a key prefix, retention bounds, and an optional remote
/// store the disk tier rolls up into. Native-only (`object_store` does not build on wasm). Build
/// with [`Disk::new`], optionally [`with_remote`](Disk::with_remote), then attach via
/// [`crate::TrackProducer::with_cache`].
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Disk {
	/// The object store for the disk tier (e.g. a `LocalFileSystem`).
	pub store: Arc<dyn ObjectStore>,
	/// Key prefix under which segments are written.
	pub prefix: Path,
	/// Retention bounds on the disk tier; exceeding them rolls up to `remote` (or evicts).
	pub bounds: Bounds,
	/// Optional remote store the disk tier rolls up into when over its bounds.
	pub remote: Option<Arc<dyn ObjectStore>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Disk {
	/// A disk tier over `store`, writing under `prefix`, capped by `bounds`. No remote rollup.
	pub fn new(store: Arc<dyn ObjectStore>, prefix: Path, bounds: Bounds) -> Self {
		Self {
			store,
			prefix,
			bounds,
			remote: None,
		}
	}

	/// Set the remote store the disk tier rolls up into.
	pub fn with_remote(mut self, remote: Arc<dyn ObjectStore>) -> Self {
		self.remote = Some(remote);
		self
	}
}

/// One frame within a cached group: its optional media timestamp and its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	/// The frame's media timestamp, if the track carries them.
	pub timestamp: Option<Timestamp>,
	/// The frame's payload bytes.
	pub payload: Bytes,
}

/// One cached group: its sequence and frames, enough to re-serve it or serialize it to a tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
	/// The group's sequence number within its track.
	pub sequence: u64,
	/// The group's frames, in order.
	pub frames: Vec<Frame>,
}

impl Group {
	/// Total size of the group's frame payloads in bytes.
	pub fn size(&self) -> u64 {
		self.frames.iter().map(|f| f.payload.len() as u64).sum()
	}

	/// The first frame's media timestamp, if any. Used as the group's lower time bound.
	pub fn ts_first(&self) -> Option<Timestamp> {
		self.frames.first().and_then(|f| f.timestamp)
	}

	/// The last frame's media timestamp, if any. Used as the group's upper time bound.
	pub fn ts_last(&self) -> Option<Timestamp> {
		self.frames.last().and_then(|f| f.timestamp)
	}

	/// Drain a live [`GroupConsumer`](crate::GroupConsumer) into a cached group, reading every
	/// frame's payload and timestamp. Resolves once the group is finished, so this is how an evicted
	/// group is snapshotted before it is written to a tier.
	pub async fn read(mut group: crate::GroupConsumer) -> Result<Self, crate::Error> {
		let sequence = group.sequence;
		let mut frames = Vec::new();
		while let Some(mut frame) = group.next_frame().await? {
			let timestamp = frame.timestamp;
			let payload = frame.read_all().await?;
			frames.push(Frame { timestamp, payload });
		}
		Ok(Self { sequence, frames })
	}

	/// Rebuild a live [`GroupConsumer`](crate::GroupConsumer) from this cached group, for serving a
	/// fetch. `timescale` must match the track's: each frame timestamp is validated against it.
	pub fn produce(&self, timescale: impl Into<Option<Timescale>>) -> Result<crate::GroupConsumer, crate::Error> {
		let mut producer = crate::GroupProducer::new(
			crate::Group {
				sequence: self.sequence,
			},
			timescale.into(),
		);
		for frame in &self.frames {
			let info = crate::Frame {
				size: frame.payload.len() as u64,
				timestamp: frame.timestamp,
			};
			let mut chunk = producer.create_frame(info)?;
			chunk.write(frame.payload.clone())?;
			chunk.finish()?;
		}
		producer.finish()?;
		Ok(producer.consume())
	}
}

/// A band of groups serialized to a tier in one flush, oldest first.
pub type Batch = Vec<Group>;

/// Backlog of eviction passes the flush task may fall behind before evicted groups are dropped
/// rather than queued. A queued pass pins its groups' frame buffers, so an unbounded queue would
/// let a slow disk migrate the RAM the live tier just freed into the channel. The cache is
/// best-effort, so on overflow we drop (creating a hole) instead of growing memory.
#[cfg(not(target_arch = "wasm32"))]
const FLUSH_BACKLOG: usize = 256;

/// The disk/remote spill handle held on a track's shared state.
///
/// Holds a sender to a background task that drains evicted groups to the disk tier, and the store
/// itself for fetch reads. Native-only (`object_store` does not build on wasm). Constructed by
/// [`crate::TrackProducer::with_cache`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Tiers {
	/// Hands each batch of evicted live groups to the background flush task. Bounded (see
	/// [`FLUSH_BACKLOG`]); a full channel drops rather than blocks the eviction path.
	flush: tokio::sync::mpsc::Sender<Vec<crate::GroupConsumer>>,
	/// The disk/remote store, shared with the flush task; used to serve fetch misses.
	store: Arc<tokio::sync::RwLock<store::Store>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Tiers {
	/// Build the store and spawn the background task that serializes evicted groups into it.
	pub(crate) fn spawn(disk: Disk) -> Self {
		let store = store::Store::new(disk.store, disk.remote, disk.prefix, disk.bounds);
		let store = Arc::new(tokio::sync::RwLock::new(store));
		let (flush, mut rx) = tokio::sync::mpsc::channel::<Vec<crate::GroupConsumer>>(FLUSH_BACKLOG);
		let writer = store.clone();
		web_async::spawn(async move {
			while let Some(first) = rx.recv().await {
				// Coalesce every eviction pass already queued into one segment, so a backlog (or a
				// stampede-trim) becomes one disk object rather than one per pass.
				let mut passes = vec![first];
				while let Ok(more) = rx.try_recv() {
					passes.push(more);
				}

				let mut batch = Batch::new();
				for consumer in passes.into_iter().flatten() {
					match Group::read(consumer).await {
						Ok(group) => batch.push(group),
						// A group torn down before we drained it (e.g. abort) is dropped, not cached.
						Err(err) => tracing::debug!(%err, "skipped uncacheable evicted group"),
					}
				}
				if batch.is_empty() {
					continue;
				}
				// Keep groups in ascending sequence so the segment's footer is ordered.
				batch.sort_by_key(|group| group.sequence);
				if let Err(err) = writer.write().await.flush(batch).await {
					tracing::warn!(%err, "cache disk flush failed");
					continue;
				}

				// Compact in phases so the slow remote upload runs without the store lock that
				// fetches need: plan (locked) snapshots the rollup, upload (unlocked) does the remote
				// put, apply (locked) repoints the index. Bind each phase to its own statement so the
				// lock guard drops at the `;` rather than being held (a held guard would also
				// deadlock the re-entrant `write()` in apply).
				let planned = writer.write().await.plan_compaction().await;
				match planned {
					Ok(Some(rollup)) => {
						if let Err(err) = rollup.upload().await {
							// The index still points at the intact disk segments; safe to leave.
							tracing::warn!(%err, "cache remote rollup upload failed");
						} else if let Err(err) = writer.write().await.apply_compaction(rollup).await {
							tracing::warn!(%err, "cache rollup apply failed");
						}
					}
					Ok(None) => {}
					Err(err) => tracing::warn!(%err, "cache compaction planning failed"),
				}
			}
		});
		Self { flush, store }
	}

	/// Hand a batch of evicted live groups to the flush task. Non-blocking (the caller holds the
	/// track state lock): a full backlog or a gone task drops the batch rather than waiting, leaving
	/// a hole in the best-effort cache instead of stalling eviction or growing RAM.
	pub(crate) fn evict(&self, groups: Vec<crate::GroupConsumer>) {
		if groups.is_empty() {
			return;
		}
		if let Err(err) = self.flush.try_send(groups) {
			let dropped = match &err {
				tokio::sync::mpsc::error::TrySendError::Full(g) => g.len(),
				tokio::sync::mpsc::error::TrySendError::Closed(g) => g.len(),
			};
			tracing::warn!(dropped, "cache flush backlog full; dropping evicted groups");
		}
	}

	/// A handle to the shared disk/remote store, for serving a fetch off the track's poll path.
	pub(crate) fn store_handle(&self) -> Arc<tokio::sync::RwLock<store::Store>> {
		self.store.clone()
	}

	/// Fetch a group from the disk/remote tiers, rebuilt at `timescale`. `None` on a miss or any
	/// tier read / rebuild error (a fetch falls through to the live path).
	#[cfg(test)]
	pub(crate) async fn fetch(&self, sequence: u64, timescale: Option<Timescale>) -> Option<crate::GroupConsumer> {
		let group = self.store.read().await.get(sequence).await.ok()??;
		group.produce(timescale).ok()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A frame of `bytes` zero bytes at an optional micros timestamp.
	fn frame(bytes: usize, ts_micros: Option<u64>) -> Frame {
		Frame {
			timestamp: ts_micros.map(|t| Timestamp::from_micros(t).unwrap()),
			payload: Bytes::from(vec![0u8; bytes]),
		}
	}

	/// A two-frame group spanning `[t0, t1]` micros, total `bytes`.
	fn timed(seq: u64, bytes: usize, t0: u64, t1: u64) -> Group {
		Group {
			sequence: seq,
			frames: vec![frame(bytes / 2, Some(t0)), frame(bytes - bytes / 2, Some(t1))],
		}
	}

	#[test]
	fn size_sums_frame_bytes() {
		let g = Group {
			sequence: 0,
			frames: vec![frame(10, None), frame(10, None), frame(10, None)],
		};
		assert_eq!(g.size(), 30);
	}

	#[test]
	fn ts_first_and_last() {
		let g = timed(0, 8, 100, 900);
		assert_eq!(g.ts_first(), Some(Timestamp::from_micros(100).unwrap()));
		assert_eq!(g.ts_last(), Some(Timestamp::from_micros(900).unwrap()));
	}

	#[tokio::test]
	async fn bridge_round_trips_a_live_group() {
		// Build a live timed group, drain it into a cached group, rebuild a live one, drain again,
		// and confirm the two cached snapshots match (payloads and per-frame timestamps survive).
		let scale = Timescale::new(1_000_000).unwrap();
		let mut live = crate::GroupProducer::new(crate::Group { sequence: 4 }, Some(scale));
		for (i, payload) in [b"hello".as_slice(), b"world".as_slice()].into_iter().enumerate() {
			let info = crate::Frame {
				size: payload.len() as u64,
				timestamp: Some(Timestamp::new(i as u64 * 1000, scale).unwrap()),
			};
			let mut frame = live.create_frame(info).unwrap();
			frame.write(Bytes::copy_from_slice(payload)).unwrap();
			frame.finish().unwrap();
		}
		live.finish().unwrap();

		let cached = Group::read(live.consume()).await.unwrap();
		assert_eq!(cached.sequence, 4);
		assert_eq!(cached.frames.len(), 2);
		assert_eq!(cached.frames[0].payload, Bytes::from_static(b"hello"));
		assert_eq!(cached.frames[1].timestamp, Some(Timestamp::new(1000, scale).unwrap()));

		let rebuilt = Group::read(cached.produce(scale).unwrap()).await.unwrap();
		assert_eq!(cached, rebuilt);
	}

	#[tokio::test]
	async fn bridge_untimed_group() {
		// An untimed track (no timescale, no frame timestamps) round-trips too.
		let mut live = crate::GroupProducer::new(crate::Group { sequence: 0 }, None);
		live.write_frame(Bytes::from_static(b"data")).unwrap();
		live.finish().unwrap();

		let cached = Group::read(live.consume()).await.unwrap();
		assert_eq!(cached.frames.len(), 1);
		assert_eq!(cached.frames[0].timestamp, None);
		assert_eq!(Group::read(cached.produce(None).unwrap()).await.unwrap(), cached);
	}

	#[cfg(not(target_arch = "wasm32"))]
	#[tokio::test]
	async fn tiers_evict_then_fetch_back() {
		use object_store::memory::InMemory;
		use object_store::path::Path;

		// Disk is unbounded so it keeps everything handed to it.
		let disk = Disk::new(Arc::new(InMemory::new()), Path::from("cache"), Bounds::default());
		let tiers = Tiers::spawn(disk);

		// Build three finished live groups and hand them to the flush task as one eviction pass.
		let mut consumers = Vec::new();
		for seq in 0..3u64 {
			let mut live = crate::GroupProducer::new(crate::Group { sequence: seq }, None);
			live.write_frame(Bytes::from(vec![seq as u8; 100])).unwrap();
			live.finish().unwrap();
			consumers.push(live.consume());
		}
		tiers.evict(consumers);

		// The background task writes them to disk; fetch reads them back.
		let mut fetched = None;
		for _ in 0..200 {
			if let Some(group) = tiers.fetch(0, None).await {
				fetched = Some(group);
				break;
			}
			tokio::task::yield_now().await;
		}
		let mut group = fetched.expect("group 0 fetched from disk");
		assert_eq!(group.sequence, 0);
		assert_eq!(group.read_frame().await.unwrap().unwrap(), Bytes::from(vec![0u8; 100]));
		assert!(tiers.fetch(2, None).await.is_some());
		assert!(tiers.fetch(99, None).await.is_none());
	}

	#[cfg(not(target_arch = "wasm32"))]
	#[tokio::test]
	async fn tiers_roll_up_to_remote_and_fetch() {
		use object_store::memory::InMemory;
		use object_store::path::Path;

		// Disk keeps ~1 segment (promote over budget); the rolled-up bytes go to the remote tier.
		// This exercises the phased plan -> upload (off-lock) -> apply path in the flush task.
		let bounds = Bounds::new(Limit::bytes(1100), Limit::bytes(2000));
		let disk =
			Disk::new(Arc::new(InMemory::new()), Path::from("cache"), bounds).with_remote(Arc::new(InMemory::new()));
		let tiers = Tiers::spawn(disk);

		// Evict five ~1 KB groups, one pass at a time, so the disk tier exceeds budget and rolls up.
		for seq in 0..5u64 {
			let mut live = crate::GroupProducer::new(crate::Group { sequence: seq }, None);
			live.write_frame(Bytes::from(vec![seq as u8; 1000])).unwrap();
			live.finish().unwrap();
			tiers.evict(vec![live.consume()]);
			// Let the flush task process this pass (flush + compaction) before the next eviction, so
			// each becomes its own segment rather than coalescing into one.
			for _ in 0..50 {
				tokio::task::yield_now().await;
			}
		}

		// Every group is still fetchable, whether it stayed on disk or rolled up to the remote tier.
		for seq in 0..5u64 {
			let mut found = None;
			for _ in 0..200 {
				if let Some(group) = tiers.fetch(seq, None).await {
					found = Some(group);
					break;
				}
				tokio::task::yield_now().await;
			}
			let mut group = found.unwrap_or_else(|| panic!("group {seq} fetchable after rollup"));
			assert_eq!(group.sequence, seq);
			assert_eq!(
				group.read_frame().await.unwrap().unwrap(),
				Bytes::from(vec![seq as u8; 1000])
			);
		}
	}
}
