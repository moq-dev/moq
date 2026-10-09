//! The audio playout target, sized from arrival timing.
//!
//! Implements the algorithm in `doc/concept/audio-jitter.md`, which is normative:
//! the browser's `@moq/hang` implements the same page, and the conformance corpus
//! beside it grades both. The design is WebRTC NetEq's underrun estimator
//! (`underrun_optimizer`, `histogram`, and `packet_arrival_history`), implemented
//! from that description rather than from its source.
//!
//! Every value is `f64` milliseconds, evaluated in the order the page writes it,
//! so both languages produce bit-identical targets from the same trace.

use std::collections::VecDeque;
use std::time::Duration;

/// Milliseconds of media time the reference search looks back over.
const WINDOW: f64 = 2000.0;

/// Milliseconds of delay per histogram bucket.
const BUCKET: f64 = 20.0;

/// Histogram buckets, so the measured term saturates at `BUCKET * BUCKETS`.
const BUCKETS: usize = 100;

/// The quantile of the delay distribution the target is read at.
const QUANTILE: f64 = 0.95;

/// Weight retained per folded observation.
const FORGET: f64 = 0.983;

/// Milliseconds of arrival time per resampled observation.
const RESAMPLE: f64 = 500.0;

/// Holds the forget factor below [`FORGET`] for the first observations.
const RAMP: f64 = 2.0;

/// Idle intervals folded at once, purely to bound the work.
const IDLE_MAX: f64 = 1024.0;

/// The playout target, fixed or estimated.
pub(crate) enum Target {
	/// An explicit delay, taken literally: no floor, no frame term, no estimate.
	Fixed(f64),
	/// Measured from arrivals.
	Auto(Box<Estimator>),
}

impl Target {
	/// A fixed target for `Some`, otherwise the estimator floored at `advertised`.
	pub(crate) fn new(delay: Option<Duration>, advertised: Option<Duration>) -> Self {
		match delay {
			Some(delay) => Self::Fixed(millis(delay)),
			None => Self::Auto(Box::new(Estimator::new(advertised.map_or(0.0, millis)))),
		}
	}

	/// Fold one frame's arrival into the estimate. A fixed target ignores it.
	pub(crate) fn observe(&mut self, arrival: f64, media: f64) {
		if let Self::Auto(estimator) = self {
			estimator.observe(arrival, media);
		}
	}

	/// Set the codec's frame duration, the term added on top of the measurement.
	pub(crate) fn set_frame(&mut self, frame: f64) {
		if let Self::Auto(estimator) = self {
			estimator.frame = frame;
		}
	}

	/// The target in milliseconds.
	pub(crate) fn millis(&self) -> f64 {
		match self {
			Self::Fixed(delay) => *delay,
			Self::Auto(estimator) => estimator.target(),
		}
	}
}

/// A duration in `f64` milliseconds, exact for any whole number of nanoseconds
/// below 2^53, which dividing the seconds by a thousandth would not be.
pub(crate) fn millis(duration: Duration) -> f64 {
	duration.as_nanos() as f64 / 1_000_000.0
}

/// The cold-start distribution, which is also what an idle receiver relaxes back to.
fn prior(i: usize) -> f64 {
	0.5f64.powi(i as i32 + 1)
}

/// One arrival still inside the reference window.
struct Entry {
	arrival: f64,
	media: f64,
}

/// Estimates the playout target from arrival timing alone.
pub(crate) struct Estimator {
	/// The codec's frame duration, never learned from observed timestamps.
	frame: f64,
	/// The publisher's advertised flush span, a floor on the measured term.
	advertised: f64,

	/// Arrivals still inside the reference window, in media order.
	history: VecDeque<Entry>,
	/// The largest media timestamp observed, so a reordered frame is recognized.
	newest: f64,

	/// The arrival resample intervals are counted from, once one has been seen.
	origin: Option<f64>,
	/// The interval currently accumulating, counted from `origin`.
	interval: f64,
	/// The largest delay seen in that interval.
	peak: f64,

	histogram: [f64; BUCKETS],
	/// Observations folded in, which drives the cold-start ramp.
	count: u64,
}

impl Estimator {
	fn new(advertised: f64) -> Self {
		Self {
			frame: 0.0,
			advertised,
			history: VecDeque::new(),
			newest: f64::NEG_INFINITY,
			origin: None,
			interval: 0.0,
			peak: 0.0,
			histogram: std::array::from_fn(prior),
			count: 0,
		}
	}

	fn observe(&mut self, arrival: f64, media: f64) {
		// A reordered arrival is excluded rather than measured: the media-time gap back
		// to the frame that overtook it would otherwise be added to the delay.
		if media <= self.newest {
			return;
		}
		self.newest = media;

		// Pruning by media time rather than by arrival is what makes a timestamp jump
		// self-correcting: everything before it leaves the window at once.
		while self.history.front().is_some_and(|entry| entry.media < media - WINDOW) {
			self.history.pop_front();
		}
		self.history.push_back(Entry { arrival, media });

		// The fastest frame still in the window, so delay is measured against the
		// best-case path instead of against the previous frame.
		let mut reference = &self.history[0];
		for entry in &self.history {
			if entry.arrival - entry.media < reference.arrival - reference.media {
				reference = entry;
			}
		}

		let delay = f64::max(0.0, arrival - reference.arrival - (media - reference.media));

		let Some(origin) = self.origin else {
			self.origin = Some(arrival);
			self.peak = delay;
			return;
		};

		let interval = ((arrival - origin) / RESAMPLE).floor();
		if interval <= self.interval {
			self.peak = self.peak.max(delay);
			return;
		}

		self.fold(Some(((self.peak / BUCKET).floor() as usize).min(BUCKETS - 1)));

		let idle = f64::min(interval - self.interval - 1.0, IDLE_MAX);
		for _ in 0..idle as u32 {
			self.fold(None);
		}

		self.interval = interval;
		self.peak = delay;
	}

	fn target(&self) -> f64 {
		let mut cumulative = 0.0;
		let mut measured = BUCKETS as f64 * BUCKET;
		for (i, weight) in self.histogram.iter().enumerate() {
			cumulative += weight;
			if cumulative > QUANTILE {
				measured = (i + 1) as f64 * BUCKET;
				break;
			}
		}

		measured.max(self.advertised) + self.frame
	}

	// One observation is `h = f*h + (1-f)*x`, where x is the bucket that was observed,
	// or the prior when the interval held no arrival at all. An idle interval therefore
	// relaxes the estimate back toward cold start at the same time constant, rather than
	// freezing it or renormalizing the decay away.
	fn fold(&mut self, bucket: Option<usize>) {
		self.count += 1;
		let count = self.count as f64;
		let forget = f64::min(FORGET, count / (count + RAMP));
		let fresh = 1.0 - forget;

		for (i, weight) in self.histogram.iter_mut().enumerate() {
			*weight *= forget;
			if bucket.is_none() {
				*weight += fresh * prior(i);
			}
		}
		if let Some(bucket) = bucket {
			self.histogram[bucket] += fresh;
		}
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	/// One case of the conformance corpus at `doc/concept/audio-jitter/`.
	#[derive(serde::Deserialize)]
	pub(crate) struct Case {
		pub frame: f64,
		pub advertised: Option<f64>,
		pub delay: Option<f64>,
		pub arrival: Vec<f64>,
		pub media: Vec<f64>,
		pub target: Vec<(usize, f64)>,
	}

	impl Case {
		/// The expected target after each frame, expanded from the change list.
		pub fn series(&self) -> Vec<f64> {
			let mut changes = self.target.iter().peekable();
			let mut current = f64::NAN;
			(0..self.arrival.len())
				.map(|index| {
					if let Some(&&(at, value)) = changes.peek()
						&& at == index
					{
						current = value;
						changes.next();
					}
					current
				})
				.collect()
		}
	}

	/// A corpus case by name, read directly from the checked-in file so neither
	/// implementation is graded against the other.
	pub(crate) fn case(name: &str) -> Case {
		let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
			.join("../../doc/concept/audio-jitter")
			.join(format!("{name}.json"));
		let json = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
		serde_json::from_str(&json).unwrap()
	}

	pub(crate) fn duration(millis: f64) -> Duration {
		Duration::from_nanos((millis * 1_000_000.0) as u64)
	}

	/// Every case the document lists. Named rather than globbed, so a case that
	/// goes missing fails here instead of silently shrinking the suite.
	const CORPUS: [&str; 9] = [
		"advertised",
		"buildup",
		"fixed",
		"flush",
		"idle",
		"paced",
		"reorder",
		"spike",
		"tunein",
	];

	#[test]
	fn conforms_to_the_corpus() {
		for name in CORPUS {
			let case = case(name);
			let mut target = Target::new(case.delay.map(duration), case.advertised.map(duration));
			target.set_frame(case.frame);

			let series = case.series();
			for (index, (&arrival, &media)) in case.arrival.iter().zip(&case.media).enumerate() {
				target.observe(arrival, media);
				let actual = target.millis();
				assert!(
					(actual - series[index]).abs() <= 1e-6,
					"{name}: frame {index} targets {actual}, expected {}",
					series[index]
				);
			}
		}
	}

	/// The #3517 regression, without the corpus: a stale frame then paced audio far
	/// ahead of it must not carry the media-time gap into the target.
	#[test]
	fn a_timestamp_jump_is_not_a_delay() {
		let mut target = Target::new(None, None);
		target.set_frame(20.0);
		target.observe(0.0, 0.0);
		for i in 0..500 {
			target.observe(10.0 + i as f64 * 20.0, 14_500.0 + i as f64 * 20.0);
		}
		assert_eq!(target.millis(), 40.0);
	}

	#[test]
	fn an_advertised_span_floors_rather_than_adds() {
		let target = Target::new(None, Some(Duration::from_millis(200)));
		// The prior reads 100 ms, under the advertised 200 ms.
		assert_eq!(target.millis(), 200.0);
	}

	#[test]
	fn the_measured_term_is_bounded_by_the_histogram() {
		let mut target = Target::new(None, None);
		target.set_frame(20.0);
		// A frame every 20 ms of media arriving a whole second apart: every interval
		// sees a delay far past the last bucket.
		for i in 0..200 {
			target.observe(i as f64 * 1000.0, i as f64 * 20.0);
		}
		assert_eq!(target.millis(), BUCKETS as f64 * BUCKET + 20.0);
	}
}
