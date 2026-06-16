use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Shared counters bumped by the connection tasks and drained by the reporter.
#[derive(Default)]
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

	/// Periodically log totals plus the throughput since the previous report.
	pub async fn report(&self, interval: Duration) {
		let mut ticker = tokio::time::interval(interval);
		// Skip the immediate first tick so the first report covers a full interval.
		ticker.tick().await;

		let mut prev = Snapshot::take(self);
		loop {
			ticker.tick().await;
			let now = Snapshot::take(self);
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
				"stats"
			);

			prev = now;
		}
	}
}

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
}

impl Snapshot {
	fn take(stats: &Stats) -> Self {
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
		}
	}
}
