//! Bounded warning bursts for damaged transport units.

use std::collections::BTreeMap;

use kio::clock::Instant;

use super::stats;

#[derive(Default)]
pub(super) struct Log {
	bursts: BTreeMap<u16, Burst>,
	next: Option<Instant>,
}

struct Burst {
	last: Instant,
	pending: u64,
}

impl Burst {
	fn flush(&mut self, pid: u16) {
		let count = std::mem::take(&mut self.pending);
		if count != 0 {
			tracing::warn!(pid, count, "TS damaged-unit summary");
		}
	}
}

impl Log {
	pub fn record(&mut self, pid: u16, error: &anyhow::Error) {
		let now = kio::clock::now();
		self.flush(now);
		let first = match self.bursts.entry(pid) {
			std::collections::btree_map::Entry::Occupied(mut entry) => {
				let burst = entry.get_mut();
				// A PID's quiet interval can end before the global summary cadence.
				let first = now.duration_since(burst.last) >= stats::Log::INTERVAL;
				if first {
					burst.flush(pid);
				} else {
					burst.pending += 1;
				}
				burst.last = now;
				first
			}
			std::collections::btree_map::Entry::Vacant(entry) => {
				entry.insert(Burst { last: now, pending: 0 });
				true
			}
		};
		if first {
			tracing::warn!(pid, count = 1, %error, "dropped a damaged TS unit");
		}
		self.next.get_or_insert(now + stats::Log::INTERVAL);
	}

	pub fn flush(&mut self, now: Instant) {
		if self.next.is_none_or(|next| now < next) {
			return;
		}
		self.bursts.retain(|&pid, burst| {
			burst.flush(pid);
			// A quiet interval ends the burst, so its next damage warns immediately.
			now.duration_since(burst.last) < stats::Log::INTERVAL
		});
		self.next = (!self.bursts.is_empty()).then_some(now + stats::Log::INTERVAL);
	}
}

impl Drop for Log {
	// Input can end through finish, abort, or an error, so the final burst summarizes on drop.
	fn drop(&mut self) {
		for (&pid, burst) in &mut self.bursts {
			burst.flush(pid);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test(start_paused = true)]
	#[ignore = "benchmark; runs in nightly"]
	#[expect(
		clippy::disallowed_methods,
		clippy::disallowed_types,
		reason = "the benchmark measures host CPU time"
	)]
	async fn benchmark_damage_pid_fanout() {
		let error = anyhow::anyhow!("broken PES");
		eprintln!("pids,damaged_units,ns_per_unit");
		for pids in [1_u16, 64, 8192] {
			for units in [1, 16, 128] {
				let mut log = Log::default();
				for pid in 0..pids {
					log.record(pid, &error);
				}
				for _ in 0..100 {
					log.record(pids - 1, &error);
				}
				// Media time stays paused, so this measures one touched PID's hot path,
				// independently of periodic summaries that visit every active burst.
				let start = std::time::Instant::now();
				for _ in 0..10_000 {
					for _ in 0..units {
						log.record(std::hint::black_box(pids - 1), &error);
					}
				}
				let nanos = start.elapsed().as_nanos() as f64 / (10_000 * units) as f64;
				eprintln!("{pids},{units},{nanos:.1}");
			}
		}
	}

	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn a_quiet_pid_starts_a_new_burst_between_summary_ticks() {
		let mut log = Log::default();
		let error = anyhow::anyhow!("broken PES");
		log.record(97, &error);
		tokio::time::advance(stats::Log::INTERVAL / 2).await;
		log.record(97, &error);
		tokio::time::advance(stats::Log::INTERVAL / 2).await;
		log.flush(kio::clock::now());
		// This PID is quiet for a full interval before the next global summary tick.
		tokio::time::advance(stats::Log::INTERVAL / 2).await;
		log.record(97, &error);
		logs_assert(|lines| {
			let first = lines
				.iter()
				.filter(|line| line.contains("dropped a damaged TS unit"))
				.count();
			if first == 2 {
				Ok(())
			} else {
				Err(format!("expected an immediate new-burst warning, got {first}"))
			}
		});
	}

	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn a_continuous_burst_logs_one_summary_per_interval() {
		let mut log = Log::default();
		let error = anyhow::anyhow!("broken PES");
		log.record(97, &error);
		for _ in 0..4 {
			tokio::time::advance(stats::Log::INTERVAL / 2).await;
			log.record(97, &error);
		}
		drop(log);
		logs_assert(|lines| {
			let first = lines
				.iter()
				.filter(|line| line.contains("dropped a damaged TS unit"))
				.count();
			let summaries: Vec<_> = lines
				.iter()
				.filter(|line| line.contains("TS damaged-unit summary"))
				.collect();
			if first == 1 && summaries.len() == 3 && summaries[1].contains("count=2") {
				Ok(())
			} else {
				Err(format!(
					"expected one burst and three summaries, got {first}/{summaries:?}"
				))
			}
		});
	}
}
