use moq_clock::Wall;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;
use serde::Serialize;

/// One-millisecond buckets through 60 seconds, with the last bucket also
/// collecting larger values. Chat delivery should stay far below this ceiling.
const LATENCY_BUCKETS: usize = 60_001;

/// Fixed sums over every connection's startup roll.
#[derive(Clone, Copy, Default)]
pub(crate) struct Targets {
	pub connections: u64,
	pub broadcasts: u64,
	pub subscriptions: u64,
}

/// Shared counters bumped by the connection tasks and drained by the reporter.
pub struct Stats {
	targets: Targets,
	/// Latched once the live established gauges all meet their targets together.
	ready: AtomicBool,
	/// Active subscriptions that have read a complete group.
	pub(crate) subscriptions_established: AtomicU64,
	/// Active broadcasts that connected or completed their first group.
	pub(crate) broadcasts_established: AtomicU64,
	pub connections: AtomicU64,
	pub broadcasts: AtomicU64,
	pub subscriptions: AtomicU64,
	pub frames_sent: AtomicU64,
	pub bytes_sent: AtomicU64,
	pub frames_recv: AtomicU64,
	pub bytes_recv: AtomicU64,
	/// Completed group deliveries across all subscriptions (the displayed total).
	/// A group the relay failed part-way is omitted and counted as lost.
	pub groups_recv: AtomicU64,
	/// Completed deliveries older than their subscription's first group, such as
	/// cached history filled in after the subscribe. Included in `groups_recv`,
	/// excluded from `groups_expected`, and never counted as loss.
	pub groups_history: AtomicU64,
	/// Settled sequence spans plus any failed live-frontier groups.
	pub groups_expected: AtomicU64,
	/// Completed deliveries within `groups_expected`. The shortfall is the gaps
	/// still outstanding, which a late arrival shrinks.
	pub groups_present: AtomicU64,
	/// Groups skipped past or failed. A late arrival never decrements it.
	pub groups_lost: AtomicU64,
	latency: Latency,
}

impl Default for Stats {
	fn default() -> Self {
		Self::new(Targets::default())
	}
}

impl Stats {
	pub(crate) fn new(targets: Targets) -> Self {
		Self {
			targets,
			ready: AtomicBool::new(false),
			subscriptions_established: AtomicU64::new(0),
			broadcasts_established: AtomicU64::new(0),
			connections: AtomicU64::new(0),
			broadcasts: AtomicU64::new(0),
			subscriptions: AtomicU64::new(0),
			frames_sent: AtomicU64::new(0),
			bytes_sent: AtomicU64::new(0),
			frames_recv: AtomicU64::new(0),
			bytes_recv: AtomicU64::new(0),
			groups_recv: AtomicU64::new(0),
			groups_history: AtomicU64::new(0),
			groups_expected: AtomicU64::new(0),
			groups_present: AtomicU64::new(0),
			groups_lost: AtomicU64::new(0),
			latency: Latency::default(),
		}
	}

	/// Whether the full rolled load has been live at once; a later loss only lowers the gauges.
	pub(crate) fn ready(&self) -> bool {
		if !self.ready.load(Ordering::Relaxed)
			&& self.connections.load(Ordering::Relaxed) >= self.targets.connections
			&& self.broadcasts_established.load(Ordering::Relaxed) >= self.targets.broadcasts
			&& self.subscriptions_established.load(Ordering::Relaxed) >= self.targets.subscriptions
		{
			self.ready.store(true, Ordering::Relaxed);
		}
		self.ready.load(Ordering::Relaxed)
	}

	pub fn frame_sent(&self, bytes: usize) {
		self.frames_sent.fetch_add(1, Ordering::Relaxed);
		self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
	}

	pub fn frame_recv(&self, bytes: usize) {
		self.frames_recv.fetch_add(1, Ordering::Relaxed);
		self.bytes_recv.fetch_add(bytes as u64, Ordering::Relaxed);
	}

	/// Record one group keyframe's wall-clock delivery latency.
	pub fn latency(&self, sent_ms: u128) {
		// Read the clock before locking so contention is not measured as latency.
		let now_ms = moq_clock::wall()
			.saturating_duration_since(Wall::UNIX_EPOCH)
			.as_millis();
		self.latency.observe(sent_ms, now_ms);
	}

	/// Reject an invalid subscriber run that completed without one delivered group.
	pub fn ensure_delivery(&self, expected: bool) -> anyhow::Result<()> {
		anyhow::ensure!(
			!expected || self.groups_recv.load(Ordering::Relaxed) > 0,
			"benchmark expected subscribed media but received zero groups"
		);
		Ok(())
	}

	/// Periodically log totals plus the throughput since the previous report.
	///
	/// With an `output` file, each report also appends one JSON line of the
	/// cumulative counters and this interval's latency distribution, timestamped
	/// so it can be joined against the host sampler's records (see
	/// `moq-bench-host`). Returns only on a failed write: a benchmark whose
	/// recorded stats are partial is invalid, so the caller must fail the run
	/// rather than exit green.
	pub async fn report(&self, interval: Duration, mut output: Option<std::fs::File>) -> anyhow::Result<()> {
		let mut ticker = moq_clock::interval(interval);
		// Skip the immediate first tick so the first report covers a full interval.
		ticker.tick().await;

		let mut prev = Snapshot::take(self);
		loop {
			ticker.tick().await;
			let now = Snapshot::take(self);
			// Same bucket vectors as the cumulative percentiles, so the interval
			// count cannot drift from the delta those percentiles describe.
			let latency_interval = IntervalLatency::from_delta(&prev.latency_buckets, &now.latency_buckets);

			if let Some(file) = &mut output {
				let record = Record::new(&prev, &now, &latency_interval)?;
				// A serialization failure is a bug, not a runtime condition.
				let line = serde_json::to_string(&record).expect("stats must serialize");
				writeln!(file, "{line}")
					.and_then(|_| file.flush())
					.context("failed to write stats output")?;
			}
			let secs = interval.as_secs_f64().max(f64::MIN_POSITIVE);

			let send_mbps = (now.bytes_sent.saturating_sub(prev.bytes_sent) as f64 * 8.0) / secs / 1e6;
			let recv_mbps = (now.bytes_recv.saturating_sub(prev.bytes_recv) as f64 * 8.0) / secs / 1e6;
			let send_fps = now.frames_sent.saturating_sub(prev.frames_sent) as f64 / secs;
			let recv_fps = now.frames_recv.saturating_sub(prev.frames_recv) as f64 / secs;

			// Group loss is cumulative (a correctness signal), not a per-interval rate.
			let loss = if now.groups_expected > 0 {
				now.groups_lost as f64 / now.groups_expected as f64 * 100.0
			} else {
				0.0
			};

			tracing::info!(
				connections = now.connections.established,
				broadcasts = now.broadcasts.active,
				subscriptions = now.subscriptions.active,
				send_mbps = format_args!("{send_mbps:.1}"),
				send_fps = format_args!("{send_fps:.0}"),
				recv_mbps = format_args!("{recv_mbps:.1}"),
				recv_fps = format_args!("{recv_fps:.0}"),
				recv_groups = now.groups_recv,
				history_groups = now.groups_history,
				lost_groups = now.groups_lost,
				loss = format_args!("{loss:.2}%"),
				latency_samples = now.latency_samples,
				latency_p50_ms = ?now.latency_p50_ms,
				latency_p90_ms = ?now.latency_p90_ms,
				latency_p99_ms = ?now.latency_p99_ms,
				latency_max_ms = ?now.latency_max_ms,
				latency_clock_skew = now.latency_clock_skew,
				latency_interval_samples = latency_interval.latency_interval_samples,
				latency_interval_p50_ms = ?latency_interval.latency_interval_p50_ms,
				latency_interval_p90_ms = ?latency_interval.latency_interval_p90_ms,
				latency_interval_p99_ms = ?latency_interval.latency_interval_p99_ms,
				latency_interval_max_ms = ?latency_interval.latency_interval_max_ms,
				"stats"
			);

			prev = now;
		}
	}
}

/// One machine-readable stats line: a timestamp, the cumulative counters, and
/// this report interval's latency distribution.
///
/// The counters are cumulative and monotonic like moq-stats frames: consumers
/// diff successive lines to compute rates. The `latency_interval_*` fields
/// already cover only this interval; do not diff them, and do not pair them
/// with the cumulative `latency_samples`.
#[derive(Serialize)]
struct Record<'a> {
	/// Wall-clock milliseconds since the Unix epoch.
	timestamp_ms: u128,
	interval_start_ms: u128,
	interval_end_ms: u128,
	#[serde(flatten)]
	snapshot: &'a Snapshot,
	#[serde(flatten)]
	interval: &'a IntervalLatency,
}

impl<'a> Record<'a> {
	fn new(previous: &Snapshot, snapshot: &'a Snapshot, interval: &'a IntervalLatency) -> anyhow::Result<Self> {
		let interval_start_ms = previous
			.captured_at
			.checked_duration_since(Wall::UNIX_EPOCH)
			.expect("Wall is after the Unix epoch")
			.as_millis();
		let interval_end_ms = snapshot
			.captured_at
			.checked_duration_since(Wall::UNIX_EPOCH)
			.expect("Wall is after the Unix epoch")
			.as_millis();
		Ok(Self {
			timestamp_ms: interval_end_ms,
			interval_start_ms,
			interval_end_ms,
			snapshot,
			interval,
		})
	}
}

#[derive(Serialize)]
struct Connections {
	target: u64,
	established: u64,
}

#[derive(Serialize)]
struct Activity {
	target: u64,
	active: u64,
	established: u64,
}

#[derive(Serialize)]
struct Snapshot {
	ready: bool,
	#[serde(skip)]
	captured_at: Wall,
	connections: Connections,
	broadcasts: Activity,
	subscriptions: Activity,
	frames_sent: u64,
	bytes_sent: u64,
	frames_recv: u64,
	bytes_recv: u64,
	groups_recv: u64,
	groups_history: u64,
	groups_expected: u64,
	groups_present: u64,
	groups_lost: u64,
	latency_samples: u64,
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_p50_ms: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_p90_ms: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_p99_ms: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_max_ms: Option<u64>,
	latency_clock_skew: u64,
	/// Cumulative one-millisecond bucket counts. Not part of the JSON line.
	/// The next report subtracts these to get that interval's distribution.
	#[serde(skip)]
	latency_buckets: Vec<u64>,
}

/// Latency percentiles for the samples observed since the previous report.
///
/// Built from the bucket delta, so an earlier interval (the startup ramp)
/// cannot move these numbers. `latency_interval_samples` is the sum of that
/// delta and the only sample count that belongs with these percentiles.
#[derive(Serialize)]
struct Histogram {
	version: u8,
	unit: &'static str,
	/// Sparse pairs of millisecond bucket index and count, sorted by index.
	buckets: Vec<(u32, u64)>,
}

#[derive(Serialize)]
struct IntervalLatency {
	latency_interval_histogram: Histogram,
	latency_interval_samples: u64,
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_interval_p50_ms: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_interval_p90_ms: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_interval_p99_ms: Option<u64>,
	/// Highest occupied bucket in the delta. Exact below 60 seconds; 60_000
	/// means the overflow bucket (at least 60 seconds), unlike cumulative
	/// `latency_max_ms`, which keeps the exact maximum.
	#[serde(skip_serializing_if = "Option::is_none")]
	latency_interval_max_ms: Option<u64>,
}

impl IntervalLatency {
	fn from_delta(prev: &[u64], now: &[u64]) -> Self {
		assert_eq!(prev.len(), now.len(), "latency snapshots must share one bucket layout");
		let buckets: Vec<u64> = now
			.iter()
			.zip(prev)
			.map(|(current, previous)| current.saturating_sub(*previous))
			.collect();
		let samples = buckets.iter().sum();
		Self {
			latency_interval_histogram: Histogram {
				version: 1,
				unit: "ms",
				buckets: buckets
					.iter()
					.enumerate()
					.filter_map(|(index, &count)| (count > 0).then_some((index as u32, count)))
					.collect(),
			},
			latency_interval_samples: samples,
			latency_interval_p50_ms: percentile(&buckets, samples, 50),
			latency_interval_p90_ms: percentile(&buckets, samples, 90),
			latency_interval_p99_ms: percentile(&buckets, samples, 99),
			latency_interval_max_ms: highest_bucket(&buckets),
		}
	}
}

impl Snapshot {
	fn take(stats: &Stats) -> Self {
		let latency = stats.latency.snapshot();
		// Acquire the counters bounded by `groups_expected` before reading it, pairing
		// with the release in `GapTracker::record`, so neither exceeds it.
		let groups_lost = stats.groups_lost.load(Ordering::Acquire);
		let groups_present = stats.groups_present.load(Ordering::Acquire);
		Self {
			ready: stats.ready(),
			captured_at: latency.captured_at,
			connections: Connections {
				target: stats.targets.connections,
				established: stats.connections.load(Ordering::Relaxed),
			},
			broadcasts: Activity {
				target: stats.targets.broadcasts,
				active: stats.broadcasts.load(Ordering::Relaxed),
				established: stats.broadcasts_established.load(Ordering::Relaxed),
			},
			subscriptions: Activity {
				target: stats.targets.subscriptions,
				active: stats.subscriptions.load(Ordering::Relaxed),
				established: stats.subscriptions_established.load(Ordering::Relaxed),
			},
			frames_sent: stats.frames_sent.load(Ordering::Relaxed),
			bytes_sent: stats.bytes_sent.load(Ordering::Relaxed),
			frames_recv: stats.frames_recv.load(Ordering::Relaxed),
			bytes_recv: stats.bytes_recv.load(Ordering::Relaxed),
			groups_recv: stats.groups_recv.load(Ordering::Relaxed),
			groups_history: stats.groups_history.load(Ordering::Relaxed),
			groups_expected: stats.groups_expected.load(Ordering::Relaxed),
			groups_present,
			groups_lost,
			latency_samples: latency.samples,
			latency_p50_ms: latency.p50_ms,
			latency_p90_ms: latency.p90_ms,
			latency_p99_ms: latency.p99_ms,
			latency_max_ms: latency.max_ms,
			latency_clock_skew: latency.clock_skew,
			latency_buckets: latency.buckets,
		}
	}
}

struct Latency {
	state: Mutex<LatencyState>,
}

#[derive(Clone)]
struct LatencyState {
	buckets: Vec<u64>,
	max_ms: u64,
	clock_skew: u64,
}

impl Default for Latency {
	fn default() -> Self {
		Self {
			state: Mutex::new(LatencyState {
				buckets: vec![0; LATENCY_BUCKETS],
				max_ms: 0,
				clock_skew: 0,
			}),
		}
	}
}

impl LatencyState {
	fn observe(&mut self, sent_ms: u128, now_ms: u128) {
		let Some(latency_ms) = now_ms.checked_sub(sent_ms) else {
			self.clock_skew += 1;
			return;
		};
		let latency_ms = u64::try_from(latency_ms).unwrap_or(u64::MAX);
		let bucket = usize::try_from(latency_ms)
			.unwrap_or(usize::MAX)
			.min(LATENCY_BUCKETS - 1);
		self.buckets[bucket] += 1;
		self.max_ms = self.max_ms.max(latency_ms);
	}
}

impl Latency {
	fn observe(&self, sent_ms: u128, now_ms: u128) {
		self.state
			.lock()
			.expect("latency lock poisoned")
			.observe(sent_ms, now_ms);
	}

	fn snapshot(&self) -> LatencySnapshot {
		// Observations and this boundary share one lock, so the histogram cannot
		// include only part of an observation. One that reads the clock before the
		// boundary but locks after it counts in the next interval, unaltered.
		let state = self.state.lock().expect("latency lock poisoned");
		let captured_at = moq_clock::wall();
		let LatencyState {
			buckets,
			max_ms,
			clock_skew,
		} = state.clone();
		drop(state);

		// Percentile scans and JSON preparation never block observations.
		let samples = buckets.iter().sum();
		LatencySnapshot {
			captured_at,
			samples,
			p50_ms: percentile(&buckets, samples, 50),
			p90_ms: percentile(&buckets, samples, 90),
			p99_ms: percentile(&buckets, samples, 99),
			max_ms: (samples > 0).then_some(max_ms),
			clock_skew,
			buckets,
		}
	}
}

struct LatencySnapshot {
	captured_at: Wall,
	samples: u64,
	p50_ms: Option<u64>,
	p90_ms: Option<u64>,
	p99_ms: Option<u64>,
	max_ms: Option<u64>,
	clock_skew: u64,
	buckets: Vec<u64>,
}

fn percentile(buckets: &[u64], samples: u64, percentile: u64) -> Option<u64> {
	if samples == 0 {
		return None;
	}
	let target = samples.saturating_mul(percentile).div_ceil(100);
	let mut cumulative = 0;
	for (value, count) in buckets.iter().enumerate() {
		cumulative += count;
		if cumulative >= target {
			return Some(value as u64);
		}
	}
	Some((buckets.len() - 1) as u64)
}

/// Highest occupied bucket, or `None` when the slice is empty of samples.
/// The index is the latency in milliseconds, except the last bucket, which
/// also holds every larger value.
fn highest_bucket(buckets: &[u64]) -> Option<u64> {
	buckets
		.iter()
		.enumerate()
		.rev()
		.find(|(_, count)| **count > 0)
		.map(|(value, _)| value as u64)
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::*;

	#[test]
	fn readiness_requires_the_targets_live_at_once() {
		let stats = Stats::new(Targets {
			connections: 2,
			..Default::default()
		});
		// One connection ends before the other connects: two established, never together.
		stats.connections.store(1, Ordering::Relaxed);
		assert!(!stats.ready());
		stats.connections.store(0, Ordering::Relaxed);
		stats.connections.store(1, Ordering::Relaxed);
		assert!(!stats.ready());
		stats.connections.store(2, Ordering::Relaxed);
		assert!(stats.ready());
	}

	#[test]
	fn json_readiness_waits_for_each_target_and_survives_later_loss() {
		let stats = Stats::new(Targets {
			connections: 1,
			broadcasts: 1,
			subscriptions: 2,
		});
		let line = |stats: &Stats| {
			let previous = Snapshot::take(stats);
			let now = Snapshot::take(stats);
			let interval = IntervalLatency::from_delta(&previous.latency_buckets, &now.latency_buckets);
			serde_json::to_value(Record::new(&previous, &now, &interval).unwrap()).unwrap()
		};
		assert_eq!(line(&stats)["ready"], false);
		stats.connections.store(1, Ordering::Relaxed);
		stats.broadcasts.store(1, Ordering::Relaxed);
		stats.broadcasts_established.store(1, Ordering::Relaxed);
		stats.subscriptions.store(2, Ordering::Relaxed);
		stats.subscriptions_established.store(1, Ordering::Relaxed);
		assert_eq!(line(&stats)["ready"], false);
		stats.subscriptions_established.store(2, Ordering::Relaxed);
		let ready = line(&stats);
		assert_eq!(ready["ready"], true);
		assert_eq!(ready["connections"], serde_json::json!({"target": 1, "established": 1}));
		assert_eq!(
			ready["broadcasts"],
			serde_json::json!({"target": 1, "active": 1, "established": 1})
		);
		assert_eq!(
			ready["subscriptions"],
			serde_json::json!({"target": 2, "active": 2, "established": 2})
		);
		for removed in ["targets", "established", "subscriptions_established"] {
			assert!(ready.get(removed).is_none(), "{removed}");
		}
		// Gauges can fall all the way to zero after establishment without resetting readiness.
		stats.connections.store(0, Ordering::Relaxed);
		stats.broadcasts.store(0, Ordering::Relaxed);
		stats.broadcasts_established.store(0, Ordering::Relaxed);
		stats.subscriptions.store(0, Ordering::Relaxed);
		stats.subscriptions_established.store(0, Ordering::Relaxed);
		let lost = line(&stats);
		assert_eq!(lost["connections"]["established"], 0);
		assert_eq!(lost["broadcasts"]["established"], 0);
		assert_eq!(lost["subscriptions"]["established"], 0);
		assert_eq!(lost["ready"], true);
	}

	/// A failed stats write must surface as an error so the run dies loudly.
	/// Silently dropping output leaves a partial JSONL file behind a green exit,
	/// which reads as a valid benchmark that quietly lost data.
	#[tokio::test]
	async fn report_fails_on_output_error() {
		tokio::time::pause();

		let dir = std::env::temp_dir().join("moq-bench-stats-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("out.jsonl");
		std::fs::write(&path, b"").unwrap();
		// A read-only handle: the first write fails.
		let file = std::fs::File::open(&path).unwrap();

		let stats = Arc::new(Stats::default());
		let task = tokio::spawn({
			let stats = stats.clone();
			async move { stats.report(Duration::from_secs(1), Some(file)).await }
		});

		tokio::time::advance(Duration::from_secs(3)).await;
		let result = task.await.unwrap();
		assert!(result.is_err(), "report must surface the output failure");
	}

	#[test]
	fn latency_reports_percentiles_and_clock_skew() {
		let latency = Latency::default();
		for value in 1..=100 {
			latency.observe(1_000, 1_000 + value);
		}
		latency.observe(1_001, 1_000);

		let snapshot = latency.snapshot();
		assert_eq!(snapshot.samples, 100);
		assert_eq!(snapshot.p50_ms, Some(50));
		assert_eq!(snapshot.p90_ms, Some(90));
		assert_eq!(snapshot.p99_ms, Some(99));
		assert_eq!(snapshot.max_ms, Some(100));
		assert_eq!(snapshot.clock_skew, 1);
	}

	/// A ramp of slow first groups must stay in the cumulative percentiles and
	/// drop out of the next interval. The interval's own sample count is the
	/// delta, not `latency_samples`.
	#[test]
	fn interval_delta_drops_the_ramp() {
		let stats = Stats::default();
		for _ in 0..10 {
			stats.latency.observe(0, 500);
		}
		let ramp = Snapshot::take(&stats);
		for value in 1..=100 {
			stats.latency.observe(1_000, 1_000 + value);
		}
		let steady = Snapshot::take(&stats);
		let interval = IntervalLatency::from_delta(&ramp.latency_buckets, &steady.latency_buckets);

		assert_eq!(steady.latency_samples, 110);
		assert_eq!(steady.latency_p50_ms, Some(55));
		assert_eq!(steady.latency_p90_ms, Some(99));
		assert_eq!(steady.latency_p99_ms, Some(500));
		assert_eq!(steady.latency_max_ms, Some(500));

		assert_eq!(interval.latency_interval_samples, 100);
		assert_eq!(interval.latency_interval_p50_ms, Some(50));
		assert_eq!(interval.latency_interval_p90_ms, Some(90));
		assert_eq!(interval.latency_interval_p99_ms, Some(99));
		assert_eq!(interval.latency_interval_max_ms, Some(100));

		let record = Record {
			timestamp_ms: 1,
			interval_start_ms: 0,
			interval_end_ms: 1,
			snapshot: &steady,
			interval: &interval,
		};
		let line = serde_json::to_value(&record).unwrap();
		assert_eq!(line["latency_samples"], serde_json::json!(110));
		assert_eq!(line["latency_interval_samples"], serde_json::json!(100));
		assert_eq!(line["latency_p99_ms"], serde_json::json!(500));
		assert_eq!(line["latency_interval_p99_ms"], serde_json::json!(99));
		assert!(line.get("latency_buckets").is_none());
	}

	#[test]
	fn sparse_runs_merge_at_bucket_resolution() {
		let runs = [vec![0, 1, 1, 200, 60_000, 80_000], vec![1, 2, 4, 200, 200, 90_000]];
		let mut merged = vec![0_u64; LATENCY_BUCKETS];
		let mut concatenated = vec![0_u64; LATENCY_BUCKETS];
		let zeros = vec![0; LATENCY_BUCKETS];
		for samples in runs {
			let mut counts = zeros.clone();
			for sample in samples {
				let index = sample.min(LATENCY_BUCKETS - 1);
				counts[index] += 1;
				concatenated[index] += 1;
			}
			let interval = IntervalLatency::from_delta(&zeros, &counts);
			let histogram = interval.latency_interval_histogram;
			assert_eq!(histogram.version, 1);
			assert_eq!(histogram.unit, "ms");
			assert_eq!(
				histogram.buckets.iter().map(|(_, count)| count).sum::<u64>(),
				interval.latency_interval_samples
			);
			for (index, count) in histogram.buckets {
				merged[index as usize] += count;
			}
		}
		assert_eq!(merged, concatenated);
		for percent in [50, 90, 99, 100] {
			assert_eq!(percentile(&merged, 12, percent), percentile(&concatenated, 12, percent));
		}
	}

	#[test]
	fn quiet_interval_omits_percentiles() {
		let stats = Stats::default();
		stats.latency.observe(0, 4);
		let first = Snapshot::take(&stats);
		let again = Snapshot::take(&stats);
		let interval = IntervalLatency::from_delta(&first.latency_buckets, &again.latency_buckets);
		assert_eq!(interval.latency_interval_samples, 0);
		assert_eq!(interval.latency_interval_p50_ms, None);
		assert_eq!(interval.latency_interval_max_ms, None);

		let record = Record {
			timestamp_ms: 1,
			interval_start_ms: 0,
			interval_end_ms: 1,
			snapshot: &again,
			interval: &interval,
		};
		let line = serde_json::to_value(&record).unwrap();
		assert_eq!(line["latency_samples"], serde_json::json!(1));
		assert_eq!(line["latency_interval_samples"], serde_json::json!(0));
		assert!(line.get("latency_interval_p50_ms").is_none());
		assert!(line.get("latency_interval_p99_ms").is_none());
		assert!(line.get("latency_interval_max_ms").is_none());
		assert_eq!(line["latency_p50_ms"], serde_json::json!(4));
	}

	/// The overflow bucket is one value for interval max. Cumulative max still
	/// keeps the exact sample, including one past 60 seconds.
	#[test]
	fn interval_max_saturates_at_the_overflow_bucket() {
		let latency = Latency::default();
		latency.observe(0, 70_000);
		let slow = latency.snapshot();
		let zeros = vec![0; slow.buckets.len()];
		let interval = IntervalLatency::from_delta(&zeros, &slow.buckets);
		assert_eq!(slow.max_ms, Some(70_000));
		assert_eq!(interval.latency_interval_max_ms, Some(60_000));
		assert_eq!(interval.latency_interval_p99_ms, Some(60_000));

		latency.observe(0, 2);
		let next = latency.snapshot();
		let interval = IntervalLatency::from_delta(&slow.buckets, &next.buckets);
		assert_eq!(next.max_ms, Some(70_000));
		assert_eq!(interval.latency_interval_samples, 1);
		assert_eq!(interval.latency_interval_max_ms, Some(2));
		assert_eq!(interval.latency_interval_p50_ms, Some(2));
	}

	#[test]
	fn report_bounds_belong_to_captured_samples() {
		let stats = Stats::default();
		let mut previous = Snapshot::take(&stats);
		previous.captured_at = Wall::UNIX_EPOCH + Duration::from_millis(1_000);
		stats.latency.observe(1_000, 1_050);
		let mut snapshot = Snapshot::take(&stats);
		snapshot.captured_at = Wall::UNIX_EPOCH + Duration::from_millis(2_000);

		// This delivery belongs to the next capture, even if report construction is delayed.
		stats.latency.observe(2_000, 2_050);
		let interval = IntervalLatency::from_delta(&previous.latency_buckets, &snapshot.latency_buckets);
		let record = Record::new(&previous, &snapshot, &interval).unwrap();
		assert_eq!(record.interval_start_ms, 1_000);
		assert_eq!(record.interval_end_ms, 2_000);
		assert_eq!(record.timestamp_ms, 2_000);
		assert_eq!(record.interval.latency_interval_samples, 1);
		let next = Snapshot::take(&stats);
		assert_eq!(
			IntervalLatency::from_delta(&snapshot.latency_buckets, &next.latency_buckets).latency_interval_samples,
			1
		);
	}

	/// An observation whose clock read precedes a capture but whose update follows
	/// it lands in the next interval and keeps its measured latency.
	#[test]
	fn observation_racing_a_capture_keeps_its_latency() {
		let latency = Latency::default();
		let first = latency.snapshot();
		let captured_ms = first
			.captured_at
			.checked_duration_since(Wall::UNIX_EPOCH)
			.unwrap()
			.as_millis();

		latency.observe(captured_ms - 100, captured_ms - 50);
		latency.observe(captured_ms - 50, captured_ms - 100);
		let next = latency.snapshot();
		let interval = IntervalLatency::from_delta(&first.buckets, &next.buckets);
		assert_eq!(first.samples, 0);
		assert_eq!(interval.latency_interval_samples, 1);
		assert_eq!(interval.latency_interval_max_ms, Some(50));
		assert_eq!(next.clock_skew, 1);
	}

	/// Sweep observation concurrency and report density without a network bottleneck.
	#[test]
	#[ignore = "performance measurement; run by the nightly stats benchmark"]
	#[expect(
		clippy::disallowed_methods,
		clippy::disallowed_types,
		reason = "the benchmark measures real observation throughput"
	)]
	fn benchmark_observation_contention() {
		use std::sync::Barrier;
		use std::time::Instant;

		for writers in [1, 4, 16] {
			for snapshots in [0, 10, 100] {
				let stats = Stats::default();
				let start = Barrier::new(writers + 2);
				let done = AtomicU64::new(0);
				let samples = 100_000;
				let elapsed = std::thread::scope(|scope| {
					for _ in 0..writers {
						scope.spawn(|| {
							start.wait();
							for _ in 0..samples {
								stats.latency(0);
								done.fetch_add(1, Ordering::Relaxed);
							}
						});
					}
					scope.spawn(|| {
						start.wait();
						for report in 1..=snapshots {
							let threshold = (writers * samples * report / snapshots) as u64;
							while done.load(Ordering::Relaxed) < threshold {
								std::thread::yield_now();
							}
							std::hint::black_box(Snapshot::take(&stats));
						}
					});
					let measured_at = Instant::now();
					start.wait();
					measured_at
				});
				let elapsed = elapsed.elapsed();
				assert_eq!(Snapshot::take(&stats).latency_samples, (writers * samples) as u64);
				println!(
					"writers={writers} snapshots={snapshots} samples={} elapsed_ns={} samples_per_second={:.0}",
					writers * samples,
					elapsed.as_nanos(),
					(writers * samples) as f64 / elapsed.as_secs_f64()
				);
			}
		}
	}

	#[test]
	fn expected_delivery_must_not_be_zero() {
		let stats = Stats::default();
		assert!(stats.ensure_delivery(false).is_ok());
		assert!(stats.ensure_delivery(true).is_err());
		stats.groups_recv.store(1, Ordering::Relaxed);
		assert!(stats.ensure_delivery(true).is_ok());
	}
}
