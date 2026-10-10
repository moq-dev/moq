//! Bounded warning bursts for damaged transport units.

use std::collections::BTreeMap;

use web_async::time::Instant;

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

impl Log {
	pub fn record(&mut self, pid: u16, error: &anyhow::Error) {
		let now = Instant::now();
		self.flush(now, false);
		match self.bursts.entry(pid) {
			std::collections::btree_map::Entry::Occupied(mut entry) => {
				let burst = entry.get_mut();
				burst.last = now;
				burst.pending += 1;
			}
			std::collections::btree_map::Entry::Vacant(entry) => {
				tracing::warn!(pid, count = 1, %error, "dropped a damaged TS unit");
				entry.insert(Burst { last: now, pending: 0 });
			}
		}
		self.next.get_or_insert(now + stats::Log::INTERVAL);
	}

	pub fn flush(&mut self, now: Instant, finish: bool) {
		if !finish && self.next.is_none_or(|next| now < next) {
			return;
		}
		self.bursts.retain(|&pid, burst| {
			let count = std::mem::take(&mut burst.pending);
			if count != 0 {
				tracing::warn!(pid, count, "TS damaged-unit summary");
			}
			// A quiet interval ends the burst, so its next damage warns immediately.
			!finish && now.duration_since(burst.last) < stats::Log::INTERVAL
		});
		self.next = (!self.bursts.is_empty()).then_some(now + stats::Log::INTERVAL);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
		log.flush(Instant::now(), true);
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
