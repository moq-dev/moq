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
	/// Distinct groups received across all subscriptions (the displayed total).
	pub groups_recv: AtomicU64,
	/// Size of every subscription's settled sequence span, excluding the live frontier.
	pub groups_expected: AtomicU64,
	/// How many groups within those settled spans actually arrived. The shortfall
	/// `groups_expected - groups_present` is the number skipped. See `connection::GapTracker`.
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
	/// cumulative counters, timestamped so it can be joined against the host
	/// sampler's records (see `moq-bench-host`). Returns only on a failed write:
	/// a benchmark whose recorded stats are partial is invalid, so the caller
	/// must fail the run rather than exit green.
	pub async fn report(&self, interval: Duration, mut output: Option<std::fs::File>) -> anyhow::Result<()> {
		let mut ticker = tokio::time::interval(interval);
		// Skip the immediate first tick so the first report covers a full interval.
		ticker.tick().await;

		let mut prev = Snapshot::take(self);
		loop {
			ticker.tick().await;
			let now = Snapshot::take(self);

			if let Some(file) = &mut output {
				let record = Record {
					timestamp_ms: SystemTime::now()
						.duration_since(UNIX_EPOCH)
						.unwrap_or_default()
						.as_millis(),
					snapshot: &now,
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
				"stats"
			);

			prev = now;
		}
	}
}

/// One machine-readable stats line: a timestamp plus the cumulative counters.
/// Cumulative and monotonic like moq-stats frames: consumers diff successive
/// lines to compute rates.
#[derive(Serialize)]
struct Record<'a> {
	/// Wall-clock milliseconds since the Unix epoch.
	timestamp_ms: u128,
	#[serde(flatten)]
	snapshot: &'a Snapshot,
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

	#[test]
	fn expected_delivery_must_not_be_zero() {
		let stats = Stats::default();
		assert!(stats.ensure_delivery(false).is_ok());
		assert!(stats.ensure_delivery(true).is_err());
		stats.groups_recv.store(1, Ordering::Relaxed);
		assert!(stats.ensure_delivery(true).is_ok());
	}
}
