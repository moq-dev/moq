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

/// The most weight one observation takes, so a lone one holds the quantile for one half-life at most.
const FRESH_MAX: f64 = 0.1;

/// Idle intervals folded at once, purely to bound the work.
const IDLE_MAX: f64 = 1024.0;

/// Arrivals kept in the reference window, a power of two so it is also the ring's
/// largest capacity.
///
/// The window is media time, and a publisher chooses the timestamp rate, so an
/// uncapped history holds every frame of a stream that advances a nanosecond per
/// frame. Lookups are logarithmic either way; this bounds the memory, and stays far
/// above the 2 s window of any real track (a 1000 fps track holds 2000).
const MAX_HISTORY: usize = 8192;

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
#[derive(Clone, Copy, Default)]
struct Entry {
	arrival: f64,
	media: f64,
}

impl Entry {
	/// What the reference minimises: how late this frame arrived for its timestamp.
	fn key(&self) -> f64 {
		self.arrival - self.media
	}
}

/// The arrivals inside the reference window, in media order, answering any
/// prefix's fastest arrival in logarithmic time.
///
/// A reordered frame is measured against the prefix at or before its timestamp,
/// so a peer that sends only reordered frames would otherwise force a scan per
/// frame. Entries live in a ring indexed from `head`, and a segment tree over the
/// ring's slots holds each node's fastest slot, the older on a tie, as the scan
/// would pick. A query never spans the ring's wrap point within one node, so a
/// node fully inside it is in arrival order.
#[derive(Default)]
struct History {
	/// The ring, its length a power of two that grows up to `MAX_HISTORY`.
	ring: Vec<Entry>,
	/// The slot of the oldest entry.
	head: usize,
	/// The number of entries held.
	len: usize,
	/// Leaf `ring.len() + slot` holds `slot`; each parent holds the faster of its children.
	tree: Vec<usize>,
}

impl History {
	/// Append an arrival newer than every entry, after dropping those it pushes out of the window.
	fn push(&mut self, arrival: f64, media: f64) {
		// Pruning by media time rather than by arrival is what makes a timestamp jump
		// self-correcting: everything before it leaves the window at once.
		while self.len > 0 && self.ring[self.head].media < media - WINDOW {
			self.pop();
		}
		if self.len == MAX_HISTORY {
			self.pop();
		}
		if self.len == self.ring.len() {
			self.grow();
		}

		let capacity = self.ring.len();
		let slot = (self.head + self.len) & (capacity - 1);
		self.ring[slot] = Entry { arrival, media };
		self.len += 1;

		let mut node = (capacity + slot) / 2;
		while node > 0 {
			self.tree[node] = self.faster(self.tree[2 * node], self.tree[2 * node + 1]);
			node /= 2;
		}
	}

	fn pop(&mut self) {
		self.head = (self.head + 1) & (self.ring.len() - 1);
		self.len -= 1;
	}

	/// Double the ring, unwrapping it to start at slot zero, and rebuild the tree.
	fn grow(&mut self) {
		let old = self.ring.len();
		let capacity = (old * 2).max(1);
		let mut ring = vec![Entry::default(); capacity];
		for (index, entry) in ring.iter_mut().enumerate().take(self.len) {
			*entry = self.ring[(self.head + index) & (old - 1)];
		}
		self.ring = ring;
		self.head = 0;

		self.tree = vec![0; 2 * capacity];
		for slot in 0..capacity {
			self.tree[capacity + slot] = slot;
		}
		for node in (1..capacity).rev() {
			self.tree[node] = self.faster(self.tree[2 * node], self.tree[2 * node + 1]);
		}
	}

	/// The fastest entry with `entry.media <= media`, the oldest on a tie.
	fn reference(&self, media: f64) -> Option<Entry> {
		// Media timestamps increase along the ring, so the prefix ends at a binary search.
		let mask = self.ring.len().wrapping_sub(1);
		let (mut lo, mut hi) = (0, self.len);
		while lo < hi {
			let mid = lo + (hi - lo) / 2;
			if self.ring[(self.head + mid) & mask].media <= media {
				lo = mid + 1;
			} else {
				hi = mid;
			}
		}
		let last = lo.checked_sub(1)?;

		let end = (self.head + last) & mask;
		let slot = if self.head <= end {
			self.range(self.head, end)
		} else {
			let older = self.range(self.head, mask);
			self.faster(older, self.range(0, end))
		};
		Some(self.ring[slot])
	}

	/// The fastest slot in `first..=last`, which must not wrap.
	fn range(&self, first: usize, last: usize) -> usize {
		let capacity = self.ring.len();
		let (mut lo, mut hi) = (first + capacity, last + capacity + 1);
		let (mut older, mut newer): (Option<usize>, Option<usize>) = (None, None);
		while lo < hi {
			if lo & 1 == 1 {
				older = Some(older.map_or(self.tree[lo], |slot| self.faster(slot, self.tree[lo])));
				lo += 1;
			}
			if hi & 1 == 1 {
				hi -= 1;
				newer = Some(newer.map_or(self.tree[hi], |slot| self.faster(self.tree[hi], slot)));
			}
			lo /= 2;
			hi /= 2;
		}
		match (older, newer) {
			(Some(older), Some(newer)) => self.faster(older, newer),
			(older, newer) => older.or(newer).expect("a non-empty range"),
		}
	}

	/// Of two slots, `older` first in arrival order, the faster one, keeping `older` on a tie.
	fn faster(&self, older: usize, newer: usize) -> usize {
		if self.ring[newer].key() < self.ring[older].key() {
			newer
		} else {
			older
		}
	}
}

/// Estimates the playout target from arrival timing alone.
pub(crate) struct Estimator {
	/// The codec's frame duration, never learned from observed timestamps.
	frame: f64,
	/// The publisher's advertised flush span, a floor on the measured term.
	advertised: f64,

	/// Arrivals still inside the reference window, in media order.
	history: History,
	/// The largest media timestamp observed, so a reordered frame is recognized.
	newest: f64,

	/// The arrival resample intervals are counted from, once one has been seen.
	origin: Option<f64>,
	/// The interval currently accumulating, counted from `origin`.
	interval: f64,
	/// The largest delay seen in that interval.
	peak: f64,

	histogram: [f64; BUCKETS],
	/// How much of the histogram is still the prior's rather than evidence.
	prior: f64,
}

impl Estimator {
	fn new(advertised: f64) -> Self {
		Self {
			frame: 0.0,
			advertised,
			history: History::default(),
			newest: f64::NEG_INFINITY,
			origin: None,
			interval: 0.0,
			peak: 0.0,
			histogram: std::array::from_fn(prior),
			prior: 1.0,
		}
	}

	fn observe(&mut self, arrival: f64, media: f64) {
		// A reordered arrival is measured but never joins the window: it arrived after a
		// newer frame, so it is never the fastest, and nothing later is measured against it.
		if media > self.newest {
			self.newest = media;
			self.history.push(arrival, media);
		}

		// The fastest frame in the window at or before this one, so delay is measured
		// against the best-case path instead of against the previous frame, and an older
		// frame held behind newer ones reads as the wait playing it in order costs. A
		// frame older than the whole window, such as a subscription's backlog behind its
		// live edge, has nothing to be measured against.
		let Some(reference) = self.history.reference(media) else {
			return;
		};

		// Written in this order on purpose: the document requires bit-identical
		// arithmetic, so the difference is not reassociated.
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

	// Evidence decays by `FORGET` and the prior holds the rest, of which an observation
	// takes up to `FRESH_MAX`, so the prior is replaced within seconds while no single
	// interval outweighs the quantile's tail for long. Once the prior is gone this is
	// `h = f*h + (1-f)*x`. An idle interval takes nothing, so its share returns to the
	// prior and the estimate relaxes toward cold start at the same time constant.
	fn fold(&mut self, bucket: Option<usize>) {
		let rest = 1.0 - FORGET * (1.0 - self.prior);
		let fresh = if bucket.is_some() {
			f64::min(FRESH_MAX, rest)
		} else {
			0.0
		};
		let left = rest - fresh;
		let seed = left - FORGET * self.prior;

		for (i, weight) in self.histogram.iter_mut().enumerate() {
			*weight = FORGET * *weight + seed * prior(i);
		}
		if let Some(bucket) = bucket {
			self.histogram[bucket] += fresh;
		}
		self.prior = left;
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
	const CORPUS: [&str; 11] = [
		"advertised",
		"backlog",
		"buildup",
		"fixed",
		"flush",
		"idle",
		"paced",
		"reorder",
		"spike",
		"straggler",
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

	/// The documented scan: the first entry with the smallest key at or before `media`.
	fn scan(history: &History, media: f64) -> Option<(f64, f64)> {
		let mut reference: Option<Entry> = None;
		for index in 0..history.len {
			let entry = history.ring[(history.head + index) & (history.ring.len() - 1)];
			if entry.media > media {
				break;
			}
			if reference.is_none_or(|best| entry.key() < best.key()) {
				reference = Some(entry);
			}
		}
		reference.map(|entry| (entry.arrival, entry.media))
	}

	/// The tree must pick exactly the entry the documented scan would, ties included,
	/// since a tied entry with a different `(arrival, media)` rounds the delay differently.
	#[test]
	fn reference_matches_the_scan() {
		let mut estimator = Estimator::new(0.0);
		let mut seed = 0x9e37_79b9_7f4a_7c15u64;
		for index in 0..400 {
			seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
			// Mostly in order, with an occasional reordered frame and a timestamp jump.
			let media = if index % 37 == 0 {
				(index as f64) * 20.0 - 900.0
			} else if index % 89 == 0 {
				(index as f64) * 20.0 + 3000.0
			} else {
				(index as f64) * 20.0
			};
			// Whole milliseconds of jitter tie often; the occasional fraction does not.
			let jitter = if index % 5 == 0 {
				(seed >> 40) as f64 / 1024.0
			} else {
				(seed >> 61) as f64
			};
			estimator.observe((index as f64) * 20.0 + jitter, media);

			let history = &estimator.history;
			for query in [media - 30.0, media - 10.0, media, media + 10.0, media + 1.0e6] {
				let actual = history.reference(query).map(|entry| (entry.arrival, entry.media));
				assert_eq!(actual, scan(history, query), "frame {index}, query {query}");
			}
		}
	}

	/// A peer choosing a microsecond per frame cannot grow the history without bound,
	/// and the full, wrapped ring still answers every prefix.
	#[test]
	fn the_history_is_capped() {
		let mut estimator = Estimator::new(0.0);
		for index in 0..(MAX_HISTORY + 5000) {
			let media = index as f64 / 1000.0;
			estimator.observe(index as f64 + (index % 7) as f64, media);
			if index % 997 == 0 {
				let history = &estimator.history;
				for query in [media - 3.0, media - 0.5, media] {
					let actual = history.reference(query).map(|entry| (entry.arrival, entry.media));
					assert_eq!(actual, scan(history, query), "frame {index}, query {query}");
				}
			}
		}
		assert_eq!(estimator.history.len, MAX_HISTORY);
		assert_eq!(estimator.history.ring.len(), MAX_HISTORY);
		assert!(estimator.target().is_finite());
	}
}
