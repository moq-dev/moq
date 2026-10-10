use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::Serialize;

/// One-millisecond buckets through 60 seconds, with the last bucket also
/// collecting larger values. Chat delivery should stay far below this ceiling.
const LATENCY_BUCKETS: usize = 60_001;

/// Shared counters bumped by the connection tasks and drained by the reporter.
pub struct Stats {
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
	/// Settled sequence spans plus any failed live-frontier groups.
	pub groups_expected: AtomicU64,
	/// Completed deliveries within `groups_expected`. The shortfall is lost groups.
	pub groups_present: AtomicU64,
	latency: Latency,
}

impl Default for Stats {
	fn default() -> Self {
		Self {
			connections: AtomicU64::new(0),
			broadcasts: AtomicU64::new(0),
			subscriptions: AtomicU64::new(0),
			frames_sent: AtomicU64::new(0),
			bytes_sent: AtomicU64::new(0),
			frames_recv: AtomicU64::new(0),
			bytes_recv: AtomicU64::new(0),
			groups_recv: AtomicU64::new(0),
			groups_expected: AtomicU64::new(0),
			groups_present: AtomicU64::new(0),
			latency: Latency::default(),
		}
	}
}

impl Stats {
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
		let now_ms = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.unwrap_or_default()
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
		let mut ticker = tokio::time::interval(interval);
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
				let record = Record {
					timestamp_ms: SystemTime::now()
						.duration_since(UNIX_EPOCH)
						.unwrap_or_default()
						.as_millis(),
					snapshot: &now,
					interval: &latency_interval,
				};
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
			let lost_groups = now.groups_expected.saturating_sub(now.groups_present);
			let loss = if now.groups_expected > 0 {
				lost_groups as f64 / now.groups_expected as f64 * 100.0
			} else {
				0.0
			};

			tracing::info!(
				connections = now.connections,
				broadcasts = now.broadcasts,
				subscriptions = now.subscriptions,
				send_mbps = format_args!("{send_mbps:.1}"),
				send_fps = format_args!("{send_fps:.0}"),
				recv_mbps = format_args!("{recv_mbps:.1}"),
				recv_fps = format_args!("{recv_fps:.0}"),
				recv_groups = now.groups_recv,
				lost_groups,
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
	#[serde(flatten)]
	snapshot: &'a Snapshot,
	#[serde(flatten)]
	interval: &'a IntervalLatency,
}

#[derive(Serialize)]
struct Snapshot {
	connections: u64,
	broadcasts: u64,
	subscriptions: u64,
	frames_sent: u64,
	bytes_sent: u64,
	frames_recv: u64,
	bytes_recv: u64,
	groups_recv: u64,
	groups_expected: u64,
	groups_present: u64,
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
struct IntervalLatency {
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
		Self {
			connections: stats.connections.load(Ordering::Relaxed),
			broadcasts: stats.broadcasts.load(Ordering::Relaxed),
			subscriptions: stats.subscriptions.load(Ordering::Relaxed),
			frames_sent: stats.frames_sent.load(Ordering::Relaxed),
			bytes_sent: stats.bytes_sent.load(Ordering::Relaxed),
			frames_recv: stats.frames_recv.load(Ordering::Relaxed),
			bytes_recv: stats.bytes_recv.load(Ordering::Relaxed),
			groups_recv: stats.groups_recv.load(Ordering::Relaxed),
			groups_expected: stats.groups_expected.load(Ordering::Relaxed),
			groups_present: stats.groups_present.load(Ordering::Relaxed),
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
	buckets: Box<[AtomicU64]>,
	max_ms: AtomicU64,
	clock_skew: AtomicU64,
}

impl Default for Latency {
	fn default() -> Self {
		Self {
			buckets: (0..LATENCY_BUCKETS).map(|_| AtomicU64::new(0)).collect(),
			max_ms: AtomicU64::new(0),
			clock_skew: AtomicU64::new(0),
		}
	}
}

impl Latency {
	fn observe(&self, sent_ms: u128, now_ms: u128) {
		let Some(latency_ms) = now_ms.checked_sub(sent_ms) else {
			self.clock_skew.fetch_add(1, Ordering::Relaxed);
			return;
		};
		let latency_ms = u64::try_from(latency_ms).unwrap_or(u64::MAX);
		let bucket = usize::try_from(latency_ms)
			.unwrap_or(usize::MAX)
			.min(LATENCY_BUCKETS - 1);
		self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
		self.max_ms.fetch_max(latency_ms, Ordering::Relaxed);
	}

	fn snapshot(&self) -> LatencySnapshot {
		let buckets: Vec<u64> = self
			.buckets
			.iter()
			.map(|bucket| bucket.load(Ordering::Relaxed))
			.collect();
		let samples = buckets.iter().sum();
		LatencySnapshot {
			samples,
			p50_ms: percentile(&buckets, samples, 50),
			p90_ms: percentile(&buckets, samples, 90),
			p99_ms: percentile(&buckets, samples, 99),
			max_ms: (samples > 0).then(|| self.max_ms.load(Ordering::Relaxed)),
			clock_skew: self.clock_skew.load(Ordering::Relaxed),
			buckets,
		}
	}
}

struct LatencySnapshot {
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
	fn expected_delivery_must_not_be_zero() {
		let stats = Stats::default();
		assert!(stats.ensure_delivery(false).is_ok());
		assert!(stats.ensure_delivery(true).is_err());
		stats.groups_recv.store(1, Ordering::Relaxed);
		assert!(stats.ensure_delivery(true).is_ok());
	}
}
