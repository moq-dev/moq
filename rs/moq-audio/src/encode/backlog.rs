use std::collections::VecDeque;
use std::time::Duration;

use moq_net::Timestamp;

/// Where one codec frame's packet starts and ends, its codec priming folded in.
#[derive(Clone, Copy, Debug)]
pub(super) struct Stamp {
	pub start: Timestamp,
	pub end: Timestamp,
}

pub(super) struct Raw {
	pub samples: Vec<f32>,
	pub stamp: Stamp,
	/// Where the frame's input ends, which retention ages from.
	pub end: Timestamp,
	pub reset: bool,
	pub discontinuity: bool,
}

/// Codec-sized PCM, bounded by both elapsed time and sample count.
pub(super) struct Backlog {
	frames: VecDeque<Raw>,
	retention: Duration,
	limit: usize,
	evicted: bool,
}

impl Backlog {
	pub fn new(retention: Duration, frame_size: usize, sample_rate: u32) -> Self {
		Self {
			frames: VecDeque::new(),
			evicted: false,
			retention,
			limit: usize::try_from(
				retention.as_nanos() * u128::from(sample_rate) / (frame_size as u128 * 1_000_000_000),
			)
			.unwrap_or(usize::MAX),
		}
	}

	pub fn push(&mut self, mut raw: Raw) {
		if std::mem::take(&mut self.evicted) {
			raw.reset = true;
			raw.discontinuity = true;
		}
		let end = raw.end;
		self.frames.push_back(raw);
		self.expire(end);
		while self.frames.len() > self.limit {
			self.evict();
		}
	}

	pub fn expire(&mut self, now: Timestamp) {
		self.expire_within(now, self.retention);
	}

	/// [`expire`](Self::expire), also dropping what a subscriber `budget` behind the live edge
	/// would find stale.
	pub fn expire_within(&mut self, now: Timestamp, budget: Duration) {
		let age = budget.min(self.retention);
		while self
			.frames
			.front()
			.is_some_and(|first| time(first.end).saturating_add(age) <= time(now))
		{
			self.evict();
		}
	}

	/// Whether audio ending at `end` is past retention at `now`.
	#[cfg(any(feature = "capture", test))]
	pub fn expired(&self, end: Timestamp, now: Timestamp) -> bool {
		time(end).saturating_add(self.retention) <= time(now)
	}

	#[cfg(any(feature = "capture", test))]
	pub fn deadline(&self) -> Option<Duration> {
		Some(time(self.frames.front()?.end).saturating_add(self.retention))
	}

	fn evict(&mut self) {
		self.frames.pop_front();
		if let Some(first) = self.frames.front_mut() {
			// Eviction is a real gap in codec state, unlike a loss of demand.
			first.reset = true;
			first.discontinuity = true;
		} else {
			self.evicted = true;
		}
	}

	pub fn pop(&mut self) -> Option<Raw> {
		self.frames.pop_front()
	}

	#[cfg(any(feature = "capture", test))]
	pub fn take_evicted(&mut self) -> bool {
		std::mem::take(&mut self.evicted)
	}

	pub fn is_empty(&self) -> bool {
		self.frames.is_empty()
	}
}

/// A stamp the producer made itself, always in microseconds, or its broadcast clock's now.
fn time(timestamp: Timestamp) -> Duration {
	Duration::try_from(timestamp).expect("the producer stamps at a scale")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn stalled_timestamps_cannot_grow_the_sample_budget() {
		let mut backlog = Backlog::new(Duration::from_secs(1), 960, 48_000);
		for index in 0..200 {
			backlog.push(Raw {
				samples: vec![index as f32; 1920],
				stamp: Stamp {
					start: Timestamp::ZERO,
					end: Timestamp::ZERO,
				},
				end: Timestamp::from_micros(20_000).unwrap(),
				reset: false,
				discontinuity: false,
			});
		}
		assert_eq!(backlog.frames.len(), 50);
		assert_eq!(backlog.pop().unwrap().samples[0], 150.0);
	}
}
