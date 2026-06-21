//! Per-track group cache: a bounded RAM window that evicts in batches.
//!
//! A cache is local policy attached to a single track, independent of any retention the original
//! publisher set (it is never carried on the wire). It keeps a `[min, max]` window of recent
//! groups in RAM. When an insert pushes the window past the high watermark (`max`), the oldest
//! groups down to the low watermark (`min`) are drained as one `Batch`, which the caller hands to
//! the next tier (disk or remote object storage). Draining a whole band at once is what keeps a
//! low-latency track (audio makes a group per frame) from producing one tiny object per group; an
//! LRU, which evicts a single item the instant the budget trips, cannot batch.
//!
//! The cache is split into a write half (`Producer`) and a read half (`Consumer`), mirroring the
//! rest of moq-net. `Producer` is intentionally not `Clone` (a single writer fills the cache);
//! `Consumer` is `Clone` and shares the same store, so one cache backs both a track's producer and
//! its consumer.
//!
//! The `segment` submodule is the on-disk byte format used by the disk and remote tiers (a band
//! of groups serialized as one self-describing object) plus the rollup that concatenates several
//! small segments into one larger object. `Group::read` / `Group::produce` bridge a cached group
//! to and from the live group model, and `TrackProducer::with_cache` / `TrackConsumer::with_cache`
//! wire the cache into the track types. The tier I/O (object_store) is the remaining piece; see
//! `rs/moq-net/CACHE.md`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;

use super::{Timescale, Timestamp};

pub mod index;
pub mod segment;

/// Disk and remote tiers backed by object_store. Requires the `cache-tiered` feature.
#[cfg(feature = "cache-tiered")]
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
	fn is_unset(&self) -> bool {
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

/// Local cache policy for a single track. Not carried on the wire.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Bounds on the RAM tier.
	pub ram: Bounds,
	// Disk and remote tiers are forthcoming (object_store-backed, feature-gated).
}

impl Config {
	/// Build a [`Config`] with the given RAM bounds.
	pub fn new(ram: Bounds) -> Self {
		Self { ram }
	}

	/// Start an empty cache with this policy, returning its write half.
	pub fn produce(self) -> Producer {
		Producer::new(self)
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
	/// frame's payload and timestamp. Resolves once the group is finished, so this is how the
	/// producer side snapshots a finished group for caching.
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

/// A band of groups drained from a tier in one flush, oldest first. The caller persists it to the
/// next tier as a single segment.
pub type Batch = Vec<Group>;

/// The shared store behind a [`Producer`] and its [`Consumer`]s.
struct State {
	config: Config,
	/// Groups keyed by sequence, so the first entry is the oldest and the last is the latest.
	ram: BTreeMap<u64, Group>,
	ram_bytes: u64,
}

impl State {
	/// The time span between the oldest group's first frame and the newest group's last frame.
	/// Zero unless both ends carry a timestamp, so a track without media timestamps applies no
	/// duration pressure (byte bounds still apply).
	fn span(&self) -> Duration {
		let first = self.ram.values().next().and_then(|g| g.ts_first());
		let last = self.ram.values().next_back().and_then(|g| g.ts_last());
		match (first, last) {
			(Some(a), Some(b)) => Duration::from(b).saturating_sub(Duration::from(a)),
			_ => Duration::ZERO,
		}
	}

	/// Whether the current contents exceed `limit`. An unset limit is treated as a floor of zero
	/// (any content exceeds it), which is what makes a flush with no `min` drain to just the
	/// latest group.
	fn exceeds(&self, limit: Limit) -> bool {
		if limit.is_unset() {
			return !self.ram.is_empty();
		}
		limit.bytes.is_some_and(|b| self.ram_bytes > b) || limit.duration.is_some_and(|d| self.span() > d)
	}

	/// Whether the high watermark is tripped. An unset high watermark is unbounded (never trips).
	fn over_max(&self) -> bool {
		!self.config.ram.max.is_unset() && self.exceeds(self.config.ram.max)
	}

	fn insert(&mut self, group: Group) -> Option<Batch> {
		let size = group.size();
		if let Some(old) = self.ram.insert(group.sequence, group) {
			self.ram_bytes -= old.size();
		}
		self.ram_bytes += size;
		self.flush()
	}

	/// If over the high watermark, drain the oldest groups down to the low watermark, keeping the
	/// latest group always. Returns the drained band, oldest first, or `None` if nothing flushed.
	fn flush(&mut self) -> Option<Batch> {
		if !self.over_max() {
			return None;
		}

		let mut batch = Batch::new();
		// Drain oldest-first while still above the low watermark, but never the latest group: a
		// new subscriber and the live edge need it, and it is the likeliest next fetch.
		while self.ram.len() > 1 && self.exceeds(self.config.ram.min) {
			let oldest = *self.ram.keys().next().expect("non-empty");
			let latest = *self.ram.keys().next_back().expect("non-empty");
			if oldest == latest {
				break;
			}
			let group = self.ram.remove(&oldest).expect("just observed");
			self.ram_bytes -= group.size();
			batch.push(group);
		}

		(!batch.is_empty()).then_some(batch)
	}
}

/// The write half of a track cache. Insert finished groups; not `Clone` (a single writer fills
/// the cache). Call [`consume`](Self::consume) for a read handle.
pub struct Producer {
	state: Arc<Mutex<State>>,
}

impl Producer {
	fn new(config: Config) -> Self {
		Self {
			state: Arc::new(Mutex::new(State {
				config,
				ram: BTreeMap::new(),
				ram_bytes: 0,
			})),
		}
	}

	/// Insert a finished group.
	///
	/// Returns a [`Batch`] when this insert pushed the RAM tier over its high watermark: the band
	/// drained down to the low watermark, which the caller persists to the next tier. `None` when
	/// nothing was evicted. A RAM-only cache ignores the return (the band is simply dropped).
	pub fn insert(&mut self, group: Group) -> Option<Batch> {
		self.state.lock().expect("cache poisoned").insert(group)
	}

	/// A read handle sharing this cache's store.
	pub fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.clone(),
		}
	}

	/// The highest sequence currently buffered in RAM, if any.
	pub fn latest(&self) -> Option<u64> {
		self.state
			.lock()
			.expect("cache poisoned")
			.ram
			.keys()
			.next_back()
			.copied()
	}

	/// The number of groups currently buffered in RAM.
	pub fn len(&self) -> usize {
		self.state.lock().expect("cache poisoned").ram.len()
	}

	/// Whether the RAM tier is empty.
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}
}

/// The read half of a track cache. `Clone` shares the same store, so several readers (and a
/// matching [`Producer`]) cache the same groups. Backs a track's `fetch`.
#[derive(Clone)]
pub struct Consumer {
	state: Arc<Mutex<State>>,
}

impl Consumer {
	/// Fetch a cached group by sequence, or `None` if it is not in the RAM tier.
	///
	/// The returned [`Group`] is an owned copy (frame `Bytes` are reference-counted, so this is
	/// cheap), so a later eviction never invalidates a fetch already in flight.
	pub fn get(&self, sequence: u64) -> Option<Group> {
		self.state.lock().expect("cache poisoned").ram.get(&sequence).cloned()
	}

	/// Whether a group with this sequence is currently in the RAM tier.
	pub fn contains(&self, sequence: u64) -> bool {
		self.state.lock().expect("cache poisoned").ram.contains_key(&sequence)
	}

	/// The highest sequence currently buffered in RAM, if any.
	pub fn latest(&self) -> Option<u64> {
		self.state
			.lock()
			.expect("cache poisoned")
			.ram
			.keys()
			.next_back()
			.copied()
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

	/// A one-frame group with no timestamp at the given sequence.
	fn plain(seq: u64, bytes: usize) -> Group {
		Group {
			sequence: seq,
			frames: vec![frame(bytes, None)],
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

	#[test]
	fn insert_and_get() {
		let mut producer = Config::default().produce();
		let consumer = producer.consume();

		assert!(consumer.get(5).is_none());
		producer.insert(plain(5, 100));
		assert_eq!(consumer.get(5).map(|g| g.size()), Some(100));
		assert!(consumer.get(6).is_none());
	}

	#[test]
	fn consumer_sees_producer_inserts() {
		// A cloned consumer observes inserts on the shared store.
		let mut producer = Config::default().produce();
		let a = producer.consume();
		let b = a.clone();

		producer.insert(plain(1, 10));
		assert!(a.contains(1));
		assert!(b.contains(1));
	}

	#[test]
	fn dedup_by_sequence() {
		// Re-inserting a sequence replaces it and keeps byte accounting correct.
		let mut producer = Config::default().produce();
		let consumer = producer.consume();

		producer.insert(plain(1, 100));
		producer.insert(plain(1, 30));
		assert_eq!(producer.len(), 1);
		assert_eq!(consumer.get(1).map(|g| g.size()), Some(30));
	}

	#[test]
	fn unbounded_when_no_max_never_flushes() {
		let mut producer = Config::default().produce();
		let mut flushed = None;
		for seq in 0..100 {
			flushed = flushed.or(producer.insert(plain(seq, 1000)));
		}
		assert!(flushed.is_none());
		assert_eq!(producer.len(), 100);
	}

	#[test]
	fn byte_high_watermark_flushes_batch_to_low() {
		// Keep 60 bytes, flush once over 100. Groups of 20 bytes: the 6th insert (120 bytes)
		// trips the high watermark and drains the three oldest down to the 60-byte low watermark.
		let bounds = Bounds::new(Limit::bytes(60), Limit::bytes(100));
		let mut producer = Config::new(bounds).produce();

		let mut batches: Vec<Batch> = Vec::new();
		for seq in 0..=5 {
			if let Some(batch) = producer.insert(plain(seq, 20)) {
				batches.push(batch);
			}
		}

		// Exactly one flush, draining the three oldest groups as one oldest-first band.
		assert_eq!(batches.len(), 1);
		let drained: Vec<u64> = batches[0].iter().map(|g| g.sequence).collect();
		assert_eq!(drained, vec![0, 1, 2]);
		// The low watermark (60 bytes = 3 groups) is retained, latest included.
		assert_eq!(producer.len(), 3);
		assert_eq!(producer.latest(), Some(5));
	}

	#[test]
	fn settles_within_the_band() {
		// Steady state stays between the low and high watermarks (hysteresis), never above max.
		let bounds = Bounds::new(Limit::bytes(60), Limit::bytes(100));
		let mut producer = Config::new(bounds).produce();
		for seq in 0..50 {
			producer.insert(plain(seq, 20));
			assert!(producer.len() <= 5, "exceeded high watermark: {}", producer.len());
		}
		assert!(producer.len() >= 3, "below low watermark: {}", producer.len());
		assert_eq!(producer.latest(), Some(49));
	}

	#[test]
	fn flush_keeps_latest_even_when_oversized() {
		// A single group larger than the whole budget is still retained (never evict the latest).
		let bounds = Bounds::new(Limit::bytes(10), Limit::bytes(50));
		let mut producer = Config::new(bounds).produce();

		let batch = producer.insert(plain(0, 1000));
		assert!(batch.is_none());
		assert_eq!(producer.len(), 1);
		assert_eq!(producer.latest(), Some(0));
	}

	#[test]
	fn min_unset_drains_to_just_the_latest() {
		// High watermark set, low watermark unset -> flush keeps only the latest group.
		let bounds = Bounds::new(Limit::default(), Limit::bytes(50));
		let mut producer = Config::new(bounds).produce();

		for seq in 0..5 {
			producer.insert(plain(seq, 20));
		}
		assert_eq!(producer.len(), 1);
		assert_eq!(producer.latest(), Some(4));
	}

	#[test]
	fn duration_high_watermark_evicts_by_timespan() {
		// Keep 2s, flush down to 1s. Each group spans 1s of media time.
		let bounds = Bounds::new(
			Limit::duration(Duration::from_secs(1)),
			Limit::duration(Duration::from_secs(2)),
		);
		let mut producer = Config::new(bounds).produce();
		let consumer = producer.consume();

		// seq 0: [0,1]s, seq 1: [1,2]s, seq 2: [2,3]s, seq 3: [3,4]s
		for seq in 0..4u64 {
			let t0 = seq * 1_000_000;
			producer.insert(timed(seq, 10, t0, t0 + 1_000_000));
		}

		assert!(consumer.contains(3), "latest kept");
		assert!(!consumer.contains(0), "oldest evicted");
		assert!(producer.len() <= 2, "len was {}", producer.len());
	}

	#[test]
	fn no_duration_pressure_without_timestamps() {
		// A duration bound with timestamp-less groups never flushes (byte bounds would still).
		let bounds = Bounds::new(
			Limit::duration(Duration::from_secs(1)),
			Limit::duration(Duration::from_secs(2)),
		);
		let mut producer = Config::new(bounds).produce();
		for seq in 0..20 {
			assert!(producer.insert(plain(seq, 1000)).is_none());
		}
		assert_eq!(producer.len(), 20);
	}

	#[test]
	fn latest_tracks_highest_sequence_out_of_order() {
		let mut producer = Config::default().produce();
		producer.insert(plain(5, 1));
		producer.insert(plain(2, 1));
		producer.insert(plain(9, 1));
		producer.insert(plain(7, 1));
		assert_eq!(producer.latest(), Some(9));
	}

	#[test]
	fn out_of_order_old_insert_can_flush_immediately() {
		// Inserting a stale (low) sequence into a full cache evicts it (or an older one) at once.
		let bounds = Bounds::new(Limit::bytes(40), Limit::bytes(50));
		let mut producer = Config::new(bounds).produce();
		for seq in 10..14 {
			producer.insert(plain(seq, 20));
		}
		let batch = producer.insert(plain(0, 20));
		assert!(batch.is_some());
		assert_eq!(producer.latest(), Some(13));
		assert!(!producer.consume().contains(0), "stale insert flushed first");
	}

	#[test]
	fn is_empty_and_len() {
		let mut producer = Config::default().produce();
		assert!(producer.is_empty());
		producer.insert(plain(0, 1));
		assert!(!producer.is_empty());
		assert_eq!(producer.len(), 1);
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
}
