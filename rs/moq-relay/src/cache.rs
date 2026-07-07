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
/// Non-latest groups stay cached until their track's TTL expires or the pool
/// runs out of room, whichever comes first (the latest group of every track is
/// always retained). With neither knob set the pool is unbounded and only the
/// per-track TTL bounds memory.
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "cache-config")]
pub struct CacheConfig {
	/// Maximum bytes of cached group payload, e.g. "8GiB", "512MB", or a
	/// percentage of memory like "75%" (respecting the cgroup limit when set).
	/// Unbounded when unset.
	///
	/// The budget counts payload bytes, not process RSS; leave some slack below
	/// physical memory or combine with `headroom`.
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
}

impl CacheConfig {
	/// Build the shared [`cache::Pool`], spawning the headroom governor when
	/// configured. Requires a tokio runtime.
	pub fn init(&self) -> anyhow::Result<cache::Pool> {
		let capacity = self.capacity.as_deref().map(parse_limit).transpose()?;
		let pool = match capacity {
			Some(bytes) => cache::Pool::new(bytes),
			None => cache::Pool::unbounded(),
		};

		if let Some(headroom) = self.headroom.as_deref() {
			let headroom = parse_limit(headroom)?;
			anyhow::ensure!(headroom > 0, "cache headroom must be non-zero");
			tracing::info!(?capacity, headroom, "cache governor enabled");
			tokio::spawn(governor(pool.clone(), capacity, headroom));
		} else if let Some(capacity) = capacity {
			tracing::info!(capacity, "cache capacity set");
		}

		Ok(pool)
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
/// stay available: the cache grows into idle memory and shrinks (evicting LRU
/// groups) when the rest of the system needs it.
async fn governor(pool: cache::Pool, capacity: Option<u64>, headroom: u64) {
	let mut sys = sysinfo::System::new();
	let mut interval = tokio::time::interval(GOVERNOR_INTERVAL);
	interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		interval.tick().await;
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
}
