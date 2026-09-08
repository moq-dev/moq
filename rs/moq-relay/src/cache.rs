//! Relay-side cache pool configuration.
//!
//! The pool itself lives in [`moq_net::cache`]; this module resolves the relay's
//! size knobs (absolute bytes, a percentage of memory, or a dynamic headroom
//! governor) into a [`cache::Pool`] shared by every session.

use std::time::Duration;

use clap::Args;
use moq_net::cache;
use serde::{Deserialize, Serialize};

/// How often the headroom governor re-samples system memory.
const GOVERNOR_INTERVAL: Duration = Duration::from_secs(5);

/// Configuration for the relay's group cache.
///
/// Non-latest groups stay cached until their track's retention window expires,
/// the `duration` ceiling is reached, or the pool runs out of room, whichever
/// comes first (the latest group of every track is always retained). With none
/// of the knobs set the pool is unbounded and only each track's own window
/// bounds memory.
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "cache-config")]
pub struct CacheConfig {
	/// Target bytes of cached group payload, e.g. "8GiB", "512MB", or a
	/// percentage of memory like "75%" (respecting the cgroup limit when set).
	/// Unbounded when unset.
	///
	/// A target that usage converges toward as tracks write, not a hard limit, and it
	/// counts cached group bytes (payload plus a fixed cost per group), not process
	/// RSS; leave some slack below physical memory or combine with `headroom`.
	#[arg(long = "cache-capacity", env = "MOQ_CACHE_CAPACITY")]
	pub capacity: Option<String>,

	/// Keep at least this much system memory available, e.g. "2GiB" or "10%".
	///
	/// Enables a background governor that re-sizes the cache every few seconds
	/// so the cache soaks up idle memory but is the first thing reclaimed when
	/// the rest of the system needs it. Combine with `capacity` to also cap the
	/// absolute size.
	#[arg(long = "cache-headroom", env = "MOQ_CACHE_HEADROOM")]
	pub headroom: Option<String>,

	/// Maximum time a non-latest cached group is retained since it was last
	/// written or served from cache by a FETCH, e.g. "30s" or "500ms".
	///
	/// Caps each track's own retention window: a publisher advertising a longer
	/// window is clamped down to this, bounding how much history a track can
	/// accumulate no matter what upstream asks for. A FETCH cache hit restarts
	/// the clock, so actively-read history stays cached. This bounds memory by
	/// age where `capacity` bounds it by bytes. Unbounded (each track keeps its
	/// own window) when unset.
	///
	/// Two caveats on the ceiling. The latest group of every track is always
	/// retained, since it is the live edge. And expiry is evaluated when a track
	/// writes its next group, so a publisher that stops writing without
	/// disconnecting keeps whatever it had cached until it resumes or the
	/// broadcast closes; under memory pressure the `capacity` budget is repaid by
	/// the tracks that are still writing.
	#[arg(long = "cache-duration", env = "MOQ_CACHE_DURATION", value_parser = humantime::parse_duration)]
	#[serde(default, with = "humantime_serde")]
	pub duration: Option<Duration>,
}

/// The relay's resolved cache settings: the shared byte-budget pool plus the
/// age ceiling applied to every track.
///
/// The headroom governor, when configured, is owned by [`Self::pool`] rather than
/// by this struct: it holds only a [`cache::PoolWeak`] and stops on its next tick
/// once every [`cache::Pool`] clone has dropped. Handing this to
/// [`Cluster::with_cache`](crate::Cluster::with_cache) therefore moves the
/// governor's lifetime onto the cluster, and dropping this struct afterwards keeps
/// it running. Conversely, holding a clone of [`Self::pool`] past the relay keeps
/// the governor running too, deliberately: as long as anything can still cache into
/// the budget, resizing it is still the right thing to do.
pub struct Cache {
	/// The shared byte-budget pool every session's groups register with.
	///
	/// Also what owns the headroom governor; see the type docs.
	pub pool: cache::Pool,

	/// Ceiling on how long a non-latest group is retained (the latest group of every
	/// track is always kept). [`Duration::MAX`] imposes no ceiling, leaving each
	/// track's own window in force.
	pub duration: Duration,
}

impl CacheConfig {
	/// Resolve the size knobs into a shared [`cache::Pool`] and age ceiling,
	/// spawning the headroom governor when configured. Requires a tokio runtime.
	///
	/// The governor stops with the pool it resizes, so a caller that drops the
	/// returned [`Cache`] without attaching it (a setup step failing after this
	/// point, say) leaves nothing sampling memory behind.
	pub fn init(&self) -> anyhow::Result<Cache> {
		let capacity = self.capacity.as_deref().map(parse_limit).transpose()?;
		let pool = match capacity {
			Some(bytes) => cache::Pool::new(bytes),
			None => cache::Pool::unbounded(),
		};

		if let Some(headroom) = self.headroom.as_deref() {
			let headroom = parse_limit(headroom)?;
			anyhow::ensure!(headroom > 0, "cache headroom must be non-zero");
			tracing::info!(?capacity, headroom, "cache governor enabled");
			tokio::spawn(governor(pool.downgrade(), capacity, headroom));
		} else if let Some(capacity) = capacity {
			tracing::info!(capacity, "cache capacity set");
		}

		if let Some(duration) = self.duration {
			tracing::info!(?duration, "cache duration ceiling set");
		}

		Ok(Cache {
			pool,
			duration: self.duration.unwrap_or(Duration::MAX),
		})
	}
}

/// Parse a size limit: either absolute bytes ("8GiB", "512MB", "1073741824") or
/// a percentage of total memory ("75%"), honoring the cgroup limit when set.
fn parse_limit(value: &str) -> anyhow::Result<u64> {
	let value = value.trim();
	if let Some(percent) = value.strip_suffix('%') {
		let percent: f64 = percent.trim().parse()?;
		anyhow::ensure!(
			percent > 0.0 && percent <= 100.0,
			"memory percentage must be within (0, 100], got {percent}"
		);
		return Ok((total_memory() as f64 * percent / 100.0) as u64);
	}
	let size: bytesize::ByteSize = value
		.parse()
		.map_err(|err| anyhow::anyhow!("invalid size {value:?}: {err}"))?;
	Ok(size.as_u64())
}

/// Total memory this process can use: the cgroup limit when confined (e.g. a
/// container), otherwise physical memory.
fn total_memory() -> u64 {
	let mut sys = sysinfo::System::new();
	sys.refresh_memory();
	match sys.cgroup_limits() {
		Some(limits) => limits.total_memory,
		None => sys.total_memory(),
	}
}

/// Re-size the pool periodically so at least `headroom` bytes of system memory
/// stay available: the cache grows into idle memory and shrinks (tracks evict
/// their stalest groups as they write) when the rest of the system needs it.
///
/// Weak on purpose. The pool is the one handle that reaches everything charging
/// into the budget, so tying the task to it means the task ends when the last
/// thing that could use the budget does, with no guard to route through the
/// relay's construction shapes. Returns on the first tick after that.
async fn governor(pool: cache::PoolWeak, capacity: Option<u64>, headroom: u64) {
	let mut sys = sysinfo::System::new();
	let mut interval = tokio::time::interval(GOVERNOR_INTERVAL);
	interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		interval.tick().await;

		let Some(pool) = pool.upgrade() else {
			tracing::debug!("cache pool dropped; stopping governor");
			return;
		};

		sys.refresh_memory();

		let available = match sys.cgroup_limits() {
			Some(limits) => limits.free_memory,
			None => sys.available_memory(),
		};

		// Whatever is free beyond the headroom is the cache's to grow into; a
		// deficit shrinks the target below the current usage, evicting until the
		// headroom is restored.
		let mut target = pool.used().saturating_add(available).saturating_sub(headroom);
		if let Some(capacity) = capacity {
			target = target.min(capacity);
		}

		tracing::trace!(used = pool.used(), available, target, "cache governor tick");
		pool.resize(target);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_limit_bytes() {
		assert_eq!(parse_limit("8GiB").unwrap(), 8 * 1024 * 1024 * 1024);
		assert_eq!(parse_limit("512MB").unwrap(), 512 * 1000 * 1000);
		assert_eq!(parse_limit("1073741824").unwrap(), 1 << 30);
	}

	#[test]
	fn parse_limit_percent() {
		let half = parse_limit("50%").unwrap();
		let full = parse_limit("100%").unwrap();
		assert!(half > 0);
		// Integer truncation makes 2*half at most `full`, never more.
		assert!(half <= full && full <= 2 * (half + 1));
	}

	#[test]
	fn parse_limit_rejects_garbage() {
		assert!(parse_limit("lots").is_err());
		assert!(parse_limit("0%").is_err());
		assert!(parse_limit("150%").is_err());
	}

	/// A config whose only knob is the headroom governor.
	fn governed() -> CacheConfig {
		CacheConfig {
			headroom: Some("10%".to_string()),
			..Default::default()
		}
	}

	/// Spawned tasks currently alive in this test's runtime. The governor is the
	/// only thing these tests spawn, so a delta of one is the governor.
	fn spawned() -> usize {
		tokio::runtime::Handle::current().metrics().num_alive_tasks()
	}

	/// Let every parked governor reach its next tick and finish stopping.
	async fn settle() {
		for _ in 0..3 {
			tokio::time::advance(GOVERNOR_INTERVAL).await;
			tokio::task::yield_now().await;
		}
	}

	/// The prefix of [`crate::Relay::load`] that owns the cache: resolve it, then
	/// hand it to a cluster whose construction can still fail.
	fn attach(cache: &CacheConfig, cluster: crate::ClusterConfig) -> anyhow::Result<crate::Cluster> {
		let cache = cache.init()?;
		Ok(crate::Cluster::new(cluster)?.with_cache(cache))
	}

	#[tokio::test(start_paused = true)]
	async fn governor_stops_when_pool_drops() {
		let pool = cache::Pool::unbounded();
		let governor = tokio::spawn(governor(pool.downgrade(), None, 1024));

		settle().await;
		assert!(!governor.is_finished(), "governor should run while the pool lives");

		drop(pool);
		governor.await.expect("governor should stop, not panic");
	}

	#[tokio::test(start_paused = true)]
	async fn governor_runs_while_any_pool_clone_lives() {
		let pool = cache::Pool::unbounded();
		let clone = pool.clone();
		let governor = tokio::spawn(governor(pool.downgrade(), None, 1024));

		drop(pool);
		settle().await;
		assert!(!governor.is_finished(), "a retained pool keeps the governor running");

		drop(clone);
		governor.await.expect("governor should stop, not panic");
	}

	#[tokio::test(start_paused = true)]
	async fn setup_failure_stops_governor() {
		let before = spawned();

		// `--cluster-id 0` is rejected, so the cache is dropped before it is attached.
		let cluster = crate::ClusterConfig {
			id: Some(0),
			..Default::default()
		};
		assert!(attach(&governed(), cluster).is_err(), "cluster id 0 is rejected");

		settle().await;
		assert_eq!(spawned(), before, "a failed setup should not strand the governor");
	}

	#[tokio::test(start_paused = true)]
	async fn last_owner_drop_stops_governor() {
		let before = spawned();
		let cluster = attach(&governed(), crate::ClusterConfig::default()).unwrap();

		settle().await;
		assert_eq!(spawned(), before + 1, "the cluster should keep the governor running");

		drop(cluster);
		settle().await;
		assert_eq!(spawned(), before, "the last cluster handle should stop the governor");
	}

	#[tokio::test(start_paused = true)]
	async fn extra_cluster_handle_keeps_governor() {
		let before = spawned();
		let cluster = attach(&governed(), crate::ClusterConfig::default()).unwrap();
		let session = cluster.clone();

		drop(cluster);
		settle().await;
		assert_eq!(spawned(), before + 1, "a surviving cluster clone keeps the governor");

		drop(session);
		settle().await;
		assert_eq!(spawned(), before, "the last cluster handle should stop the governor");
	}
}
