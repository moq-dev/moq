//! Cumulative per-rendition counters behind a publisher's stats and a viewer's feedback.
//!
//! Each track bumps its own atomics as it writes or reads a frame, so counting takes no lock. A
//! reporter reads the whole set about once a second through the [`Registry`] the tracks enrolled in:
//! the catalog [`Producer`](crate::catalog::Producer) for what a publisher writes, the export
//! [`Source`](crate::Source) for what a viewer reads.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What one published track has written.
#[derive(Default)]
pub(crate) struct Sent {
	frames: AtomicU64,
	bytes: AtomicU64,
	keyframes: AtomicU64,
}

impl Sent {
	/// Count `frames` media frames carrying `bytes` of payload, `keyframes` of them keyframes.
	pub(crate) fn add(&self, frames: u64, bytes: usize, keyframes: u64) {
		self.frames.fetch_add(frames, Relaxed);
		self.bytes.fetch_add(bytes as u64, Relaxed);
		self.keyframes.fetch_add(keyframes, Relaxed);
	}

	fn report(&self) -> hang::stats::Rendition {
		let mut report = hang::stats::Rendition::default();
		report.frames = self.frames.load(Relaxed);
		report.bytes = self.bytes.load(Relaxed);
		report.keyframes = self.keyframes.load(Relaxed);
		report
	}
}

/// What one exported track has read.
#[derive(Default)]
pub(crate) struct Received {
	frames: AtomicU64,
	bytes: AtomicU64,
	/// The latest frame's media timestamp, in microseconds.
	timestamp: AtomicU64,
	/// When the latest frame arrived, in milliseconds since the Unix epoch, or zero before the first.
	wall: AtomicU64,
}

impl Received {
	/// Count one frame of `bytes` payload at media `timestamp`, arriving now.
	pub(crate) fn frame(&self, timestamp: moq_net::Timestamp, bytes: usize) {
		self.frames.fetch_add(1, Relaxed);
		self.bytes.fetch_add(bytes as u64, Relaxed);
		// A clock before the MoQ epoch has no wire encoding, so such an arrival is never recorded.
		let wall = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0));
		if wall < hang::catalog::MOQ_EPOCH_UNIX_MILLIS {
			return;
		}
		let micros = u64::try_from(timestamp.as_micros()).unwrap_or(u64::MAX);
		self.timestamp.store(micros, Relaxed);
		self.wall.store(wall, Relaxed);
	}

	fn report(&self) -> hang::echo::Rendition {
		let mut report = hang::echo::Rendition::default();
		report.frames = self.frames.load(Relaxed);
		report.bytes = self.bytes.load(Relaxed);
		// The pair is read without a lock, so it may straddle two consecutive frames: off by one
		// frame interval at most, which no latency reading resolves.
		let wall = self.wall.load(Relaxed);
		if wall != 0
			&& let Ok(timestamp) = moq_net::Timestamp::from_micros(self.timestamp.load(Relaxed))
		{
			let wall = UNIX_EPOCH + Duration::from_millis(wall);
			report.arrival = Some(hang::echo::Arrival::new(timestamp, wall));
		}
		report
	}
}

/// Counters keyed by rendition ID, shared by the tracks that bump them and the reporter that reads
/// them.
pub(crate) struct Registry<T>(Mutex<BTreeMap<String, Arc<T>>>);

impl<T> Default for Registry<T> {
	fn default() -> Self {
		Self(Mutex::default())
	}
}

impl<T: Default> Registry<T> {
	/// The counters for `name`, continuing those of an earlier track under that name.
	pub(crate) fn track(&self, name: &str) -> Arc<T> {
		let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
		entries.entry(name.to_string()).or_default().clone()
	}

	/// Read every entry. A track that ended keeps its final counts for the registry's life, as the
	/// catalog a finished import leaves up still lists it.
	fn report<R>(&self, read: impl Fn(&T) -> R) -> BTreeMap<String, R> {
		let entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
		entries
			.iter()
			.map(|(name, counters)| (name.clone(), read(counters)))
			.collect()
	}
}

impl Registry<Sent> {
	/// What each enrolled track has written.
	pub(crate) fn sent(&self) -> BTreeMap<String, hang::stats::Rendition> {
		self.report(Sent::report)
	}
}

impl Registry<Received> {
	/// What each enrolled track has read.
	pub(crate) fn received(&self) -> BTreeMap<String, hang::echo::Rendition> {
		self.report(Received::report)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn an_ended_track_keeps_its_final_counts() {
		let registry = Registry::<Sent>::default();
		let track = registry.track("video");
		track.add(3, 300, 1);
		drop(track);

		for _ in 0..2 {
			let report = registry.sent();
			assert_eq!(report["video"].frames, 3);
			assert_eq!(report["video"].bytes, 300);
			assert_eq!(report["video"].keyframes, 1);
		}
	}

	#[test]
	fn a_returning_track_continues_its_counts() {
		let registry = Registry::<Sent>::default();
		registry.track("video").add(2, 20, 0);
		registry.track("video").add(1, 10, 0);
		assert_eq!(registry.sent()["video"].frames, 3);
		assert_eq!(registry.sent()["video"].bytes, 30);
	}

	#[test]
	fn arrival_pairs_the_latest_timestamp_with_its_wall_time() {
		let registry = Registry::<Received>::default();
		let track = registry.track("hd");
		assert_eq!(registry.received()["hd"].arrival, None);

		let before = SystemTime::now();
		track.frame(moq_net::Timestamp::from_millis(40).unwrap(), 100);
		track.frame(moq_net::Timestamp::from_millis(80).unwrap(), 50);
		let report = &registry.received()["hd"];
		assert_eq!((report.frames, report.bytes), (2, 150));
		let arrival = report.arrival.as_ref().unwrap();
		assert_eq!(arrival.timestamp.as_millis(), 80);
		assert!(arrival.wall + Duration::from_millis(1) >= before);
	}
}
