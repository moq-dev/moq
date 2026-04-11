//! Task categorisation and profiling.
//!
//! Every detached task spawned by moq-lite (and moq-relay) is attributed to a
//! typed [`Category`]. Each category owns a lazily-initialised
//! [`tokio_metrics::TaskMonitor`], so we can ask "which class of task is not
//! dying?" without rebuilding the process.
//!
//! Call sites look like:
//!
//! ```ignore
//! moq_lite::task::LITE_SESSION.spawn(async move { /* ... */ });
//! ```
//!
//! A snapshot of every category's metrics is available via [`snapshot`].

use std::future::Future;
use std::sync::{Mutex, OnceLock};

use tokio::task::JoinHandle;
use tokio_metrics::{TaskMetrics, TaskMonitor};

/// A task category: a name plus a lazily-initialised [`TaskMonitor`].
///
/// Define each category as a `pub static` constant and call
/// [`Category::spawn`] on it. Typos fail at compile time and there's no hash
/// lookup on the spawn hot path after the first call per category.
pub struct Category {
	name: &'static str,
	monitor: OnceLock<TaskMonitor>,
}

impl Category {
	pub const fn new(name: &'static str) -> Self {
		Self {
			name,
			monitor: OnceLock::new(),
		}
	}

	pub fn name(&self) -> &'static str {
		self.name
	}

	/// Spawn a tracked detached task under this category.
	///
	/// Returns a [`JoinHandle`] so call sites that need `abort_handle()` (e.g.
	/// `moq_relay::cluster`) still work.
	#[track_caller]
	pub fn spawn<F>(&'static self, f: F) -> JoinHandle<()>
	where
		F: Future<Output = ()> + Send + 'static,
	{
		let f = tracing::Instrument::in_current_span(f);
		tokio::task::spawn(self.monitor().instrument(f))
	}

	/// Returns the monitor, initialising it on first call and registering
	/// `self` in the global registry so [`snapshot`] can find it.
	///
	/// Hot path (after first init) is a single `OnceLock::get()` — lock-free.
	fn monitor(&'static self) -> &'static TaskMonitor {
		self.monitor.get_or_init(|| {
			REGISTRY.lock().unwrap().push(self);
			TaskMonitor::new()
		})
	}

	fn cumulative(&self) -> TaskMetrics {
		// The registry only contains categories whose monitor has been
		// initialised, but be defensive so snapshot() can't panic.
		self.monitor.get().map(TaskMonitor::cumulative).unwrap_or_default()
	}
}

static REGISTRY: Mutex<Vec<&'static Category>> = Mutex::new(Vec::new());

/// A flat, leak-focused projection of [`TaskMetrics`] for logging.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TaskSnapshot {
	pub name: &'static str,
	/// `instrumented_count - dropped_count` — tasks currently live.
	pub live: u64,
	pub spawned: u64,
	pub dropped: u64,
	pub slow_polls: u64,
	pub mean_poll_us: u64,
}

impl std::fmt::Display for TaskSnapshot {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"{:40} live={:>5} spawned={:>8} dropped={:>8} slow={:>4} poll_us={:>5}",
			self.name, self.live, self.spawned, self.dropped, self.slow_polls, self.mean_poll_us,
		)
	}
}

/// Per-category snapshot, sorted by category name.
///
/// Only includes categories whose [`Category::spawn`] has been called at least
/// once (they register themselves lazily).
pub fn snapshot() -> Vec<TaskSnapshot> {
	let mut registry = REGISTRY.lock().unwrap().clone();
	registry.sort_by_key(|c| c.name);
	registry
		.iter()
		.map(|c| {
			let m = c.cumulative();
			let live = m.instrumented_count.saturating_sub(m.dropped_count);
			let mean_poll_us = if m.total_poll_count > 0 {
				(m.total_poll_duration.as_micros() as u64) / m.total_poll_count
			} else {
				0
			};
			TaskSnapshot {
				name: c.name,
				live,
				spawned: m.instrumented_count,
				dropped: m.dropped_count,
				slow_polls: m.total_slow_poll_count,
				mean_poll_us,
			}
		})
		.collect()
}

/// Raw metrics escape hatch for anyone who wants the full [`TaskMetrics`].
pub fn snapshot_raw() -> Vec<(&'static str, TaskMetrics)> {
	let mut registry = REGISTRY.lock().unwrap().clone();
	registry.sort_by_key(|c| c.name);
	registry.iter().map(|c| (c.name, c.cumulative())).collect()
}

// ─── moq-lite categories ────────────────────────────────────────────────────
//
// Add new categories here rather than inline at spawn sites — keeps the full
// list discoverable in one place.

/// Periodic send-bandwidth sampling task (`Session::new`).
pub static BANDWIDTH: Category = Category::new("moq-lite/bandwidth");

/// The moq-lite (draft lite-*) session driver task (`lite::start`).
pub static LITE_SESSION: Category = Category::new("moq-lite/lite-session");

/// Per-PROBE-stream task on the publisher side.
pub static LITE_PROBE: Category = Category::new("moq-lite/lite-probe");

/// Per-ANNOUNCE-stream task on the publisher side.
pub static LITE_ANNOUNCE: Category = Category::new("moq-lite/lite-announce");

/// Per-SUBSCRIBE-stream task on the publisher side.
pub static LITE_SUBSCRIBE: Category = Category::new("moq-lite/lite-subscribe");

/// Per-incoming-uni-stream task on the subscriber side (group data).
pub static LITE_UNI_STREAM: Category = Category::new("moq-lite/lite-uni-stream");

/// Per-broadcast driver task on the subscriber side (`run_broadcast`).
pub static LITE_BROADCAST: Category = Category::new("moq-lite/lite-broadcast");

/// Per-track driver task on the subscriber side (`run_subscribe`).
pub static LITE_TRACK: Category = Category::new("moq-lite/lite-track");

/// The IETF (draft-14+) session driver task (`ietf::start`).
pub static IETF_SESSION: Category = Category::new("moq-lite/ietf-session");

/// IETF SETUP sender / GOAWAY watcher task.
pub static IETF_SETUP: Category = Category::new("moq-lite/ietf-setup");

/// Per-incoming-uni-stream task on the IETF session (group data).
pub static IETF_UNI_STREAM: Category = Category::new("moq-lite/ietf-uni-stream");

/// Per-incoming-SUBSCRIBE handler task on the IETF publisher side.
pub static IETF_PUB_SUBSCRIBE: Category = Category::new("moq-lite/ietf-pub-subscribe");

/// Per-incoming-FETCH handler task on the IETF publisher side.
pub static IETF_PUB_FETCH: Category = Category::new("moq-lite/ietf-pub-fetch");

/// Per-incoming-SUBSCRIBE_NAMESPACE handler task on the IETF publisher side.
pub static IETF_PUB_SUB_NAMESPACE: Category = Category::new("moq-lite/ietf-pub-sub-namespace");

/// Per-incoming-PUBLISH handler task on the IETF subscriber side.
pub static IETF_SUB_PUBLISH: Category = Category::new("moq-lite/ietf-sub-publish");

/// Per-incoming-PUBLISH_NAMESPACE handler task on the IETF subscriber side.
pub static IETF_SUB_PUB_NAMESPACE: Category = Category::new("moq-lite/ietf-sub-pub-namespace");

/// Per-broadcast driver task on the IETF subscriber side.
pub static IETF_SUB_BROADCAST: Category = Category::new("moq-lite/ietf-sub-broadcast");

/// Per-track driver task on the IETF subscriber side (`run_subscribe`).
pub static IETF_SUB_TRACK: Category = Category::new("moq-lite/ietf-sub-track");

/// Cleanup task that removes a broadcast from an origin when it closes.
pub static ORIGIN_CLEANUP: Category = Category::new("moq-lite/origin-cleanup");

/// Per-track dedup cleanup task inside `crate::model::broadcast`.
pub static BROADCAST_DEDUP: Category = Category::new("moq-lite/broadcast-dedup");

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	static TEST_BLOCKER: Category = Category::new("test/blocker");
	static TEST_INSTANT: Category = Category::new("test/instant");

	#[tokio::test]
	async fn snapshot_tracks_live_count() {
		let (tx, rx) = tokio::sync::oneshot::channel::<()>();
		TEST_BLOCKER.spawn(async move {
			let _ = rx.await;
		});
		TEST_INSTANT.spawn(async {});

		// Give the runtime time to poll both tasks once so the instant one
		// actually completes and gets counted as dropped.
		tokio::time::sleep(Duration::from_millis(20)).await;

		let snap = snapshot();
		let blocker = snap
			.iter()
			.find(|s| s.name == "test/blocker")
			.expect("blocker category should be registered");
		let instant = snap
			.iter()
			.find(|s| s.name == "test/instant")
			.expect("instant category should be registered");

		assert_eq!(blocker.live, 1, "blocker should still be live");
		assert_eq!(blocker.spawned, 1);
		assert_eq!(blocker.dropped, 0);

		assert_eq!(instant.live, 0, "instant task should already be dropped");
		assert_eq!(instant.spawned, 1);
		assert_eq!(instant.dropped, 1);

		let _ = tx.send(());
	}
}
