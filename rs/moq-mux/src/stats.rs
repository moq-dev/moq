//! Cumulative per-rendition counters behind a publisher's stats and a viewer's feedback.
//!
//! Each track bumps its own atomics as it writes or reads a frame, so counting takes no lock. A
//! reporter reads the whole set about once a second through the [`Registry`] the tracks enrolled in:
//! the catalog [`Producer`](crate::catalog::Producer) for what a publisher writes, the export
//! [`Source`](crate::Source) for what a viewer reads.

use kio::clock::Wall;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

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
		let Some(wall) = crate::clock::host_wall() else {
			return;
		};
		let wall = wall
			.checked_duration_since(Wall::UNIX_EPOCH)
			.map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0));
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
			let wall = Wall::UNIX_EPOCH + Duration::from_millis(wall);
			report.arrival = Some(hang::echo::Arrival::new(timestamp, wall));
		}
		report
	}
}

/// The most rendition IDs a registry keeps, so that a remote catalog that keeps renaming its
/// renditions cannot grow it, and each report, without bound.
const MAX_ENTRIES: usize = 1024;

/// Counters keyed by rendition ID, shared by the tracks that bump them and the reporter that reads
/// them.
pub(crate) struct Registry<T> {
	entries: Mutex<BTreeMap<String, Arc<T>>>,
	/// Whether a name has gone unreported because every entry was live, which is logged once.
	full: AtomicBool,
}

impl<T> Default for Registry<T> {
	fn default() -> Self {
		Self {
			entries: Mutex::default(),
			full: AtomicBool::new(false),
		}
	}
}

impl<T: Default> Registry<T> {
	/// The counters for `name`, continuing those of an earlier track under that name. At
	/// [`MAX_ENTRIES`] names, one that no track still counts into makes way; with every one live, a
	/// new name counts into counters no report reads.
	pub(crate) fn track(&self, name: &str) -> Arc<T> {
		let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some(counters) = entries.get(name) {
			return counters.clone();
		}
		if entries.len() >= MAX_ENTRIES {
			// Each track holds its counters for as long as it runs, so the registry's is the last.
			// Only one makes way, so the other ended renditions keep their final counts.
			let ended = entries
				.iter()
				.find(|(_, counters)| Arc::strong_count(counters) == 1)
				.map(|(ended, _)| ended.clone());
			let Some(ended) = ended else {
				if !self.full.swap(true, Relaxed) {
					tracing::warn!(
						name,
						limit = MAX_ENTRIES,
						"every reported rendition is live; not reporting more"
					);
				}
				return Arc::default();
			};
			entries.remove(&ended);
		}
		entries.entry(name.to_string()).or_default().clone()
	}

	/// Read every entry. A track that ended keeps its final counts until the registry fills, as the
	/// catalog a finished import leaves up still lists it.
	fn report<R>(&self, read: impl Fn(&T) -> R) -> BTreeMap<String, R> {
		let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
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

	/// With every entry live a new name goes unreported and a known one still counts; once the
	/// tracks end, a new name takes their place.
	#[test]
	fn renditions_past_the_cap_replace_ended_ones() {
		let registry = Registry::<Sent>::default();
		let live: Vec<_> = (0..MAX_ENTRIES).map(|n| registry.track(&format!("r{n}"))).collect();
		for counters in &live {
			counters.add(1, 1, 0);
		}
		registry.track("late").add(1, 1, 0);
		let report = registry.sent();
		assert_eq!(report.len(), MAX_ENTRIES);
		assert!(!report.contains_key("late"));

		registry.track("r0").add(1, 1, 0);
		assert_eq!(registry.sent()["r0"].frames, 2);

		drop(live);
		registry.track("late").add(1, 1, 0);
		let report = registry.sent();
		assert_eq!(report.len(), MAX_ENTRIES);
		assert_eq!(report["late"].frames, 1);
	}

	/// A new name past the cap evicts one ended rendition, so the rest keep their final counts and
	/// a returning one continues them.
	#[test]
	fn a_new_rendition_evicts_only_one_ended() {
		let registry = Registry::<Sent>::default();
		let live = registry.track("live");
		for n in 1..MAX_ENTRIES {
			registry.track(&format!("r{n}")).add(1, 1, 0);
		}

		registry.track("late").add(1, 1, 0);
		let report = registry.sent();
		assert_eq!(report.len(), MAX_ENTRIES);
		assert!(report.contains_key("live"));
		assert!(report.contains_key("late"));
		let kept = (1..MAX_ENTRIES)
			.filter(|n| report.contains_key(&format!("r{n}")))
			.count();
		assert_eq!(kept, MAX_ENTRIES - 2);

		let returning = (1..MAX_ENTRIES)
			.map(|n| format!("r{n}"))
			.find(|name| report.contains_key(name))
			.unwrap();
		registry.track(&returning).add(1, 1, 0);
		assert_eq!(registry.sent()[&returning].frames, 2);
		drop(live);
	}

	#[test]
	fn arrival_pairs_the_latest_timestamp_with_its_wall_time() {
		let registry = Registry::<Received>::default();
		let track = registry.track("hd");
		assert_eq!(registry.received()["hd"].arrival, None);

		let wall = Wall::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
		crate::clock::fake_host_wall(wall);
		track.frame(moq_net::Timestamp::from_millis(40).unwrap(), 100);
		track.frame(moq_net::Timestamp::from_millis(80).unwrap(), 50);
		let report = &registry.received()["hd"];
		assert_eq!((report.frames, report.bytes), (2, 150));
		let arrival = report.arrival.as_ref().unwrap();
		assert_eq!(arrival.timestamp.as_millis(), 80);
		assert_eq!(arrival.wall, wall);
	}

	/// A host clock before the MoQ epoch has no wire encoding: the frame counts, its arrival doesn't.
	#[test]
	fn an_unset_host_clock_records_no_arrival() {
		let registry = Registry::<Received>::default();
		let track = registry.track("hd");
		crate::clock::fake_host_wall(Wall::UNIX_EPOCH + Duration::from_secs(86_400));
		track.frame(moq_net::Timestamp::from_millis(40).unwrap(), 100);
		let report = &registry.received()["hd"];
		assert_eq!((report.frames, report.bytes), (1, 100));
		assert_eq!(report.arrival, None);
	}
}
