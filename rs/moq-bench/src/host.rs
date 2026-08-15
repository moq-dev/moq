//! Host-side resource sampler, the other half of `moq-bench`.
//!
//! `moq-bench` drives load from one machine; this binary runs next to the process
//! under test (a relay, usually) and records what that load costs. It is safe on
//! production hosts: it only reads `/proc`, needs no privileges beyond visibility
//! of the target process, and never touches the process itself (no ptrace, no
//! perf, no signals).
//!
//! Every interval it writes one JSON line per sampled process. Counters are
//! cumulative and monotonic, mirroring the moq-stats convention: a consumer diffs
//! successive lines to compute rates, and a counter going backwards means the
//! process restarted. The context-switch counters keep that promise across thread
//! churn: an exited thread's contribution is retained rather than vanishing with
//! its `/proc` entry. Combine with the load generator's `--output` to compute CPU
//! per connection and CPU per message (see the README).

#[cfg(target_os = "linux")]
mod linux {
	use std::collections::HashMap;
	use std::io::Write;
	use std::time::{Duration, SystemTime, UNIX_EPOCH};

	use anyhow::Context;
	use clap::Parser;
	use procfs::CurrentSI;
	use procfs::process::Process;
	use serde::Serialize;

	/// Parse a duration and reject zero: a zero interval would busy-loop over
	/// /proc and flood the output, which a sampler meant to sit on production
	/// hosts must never do.
	fn positive_duration(arg: &str) -> anyhow::Result<Duration> {
		let duration = humantime::parse_duration(arg)?;
		anyhow::ensure!(!duration.is_zero(), "must be greater than 0s");
		Ok(duration)
	}

	/// Sample CPU, memory, and context-switch counters for a running process.
	#[derive(Parser, Debug)]
	#[command(version = env!("VERSION"))]
	pub struct Args {
		/// Sample these PIDs. When set, --name is ignored.
		#[arg(long)]
		pub pid: Vec<i32>,

		/// Find target processes by name (/proc/<pid>/comm, 15 bytes max).
		#[arg(long, default_value = "moq-relay")]
		pub name: String,

		/// How often to sample.
		#[arg(long, value_parser = positive_duration, default_value = "1s")]
		pub interval: Duration,

		/// Stop after this duration. Runs until interrupted (or the targets exit) otherwise.
		#[arg(long, value_parser = humantime::parse_duration)]
		pub duration: Option<Duration>,

		/// Write JSON lines to this file instead of stdout. Truncates on start.
		#[arg(long)]
		pub output: Option<std::path::PathBuf>,

		/// Include a per-thread breakdown in every sample. Costs one /proc read per
		/// thread per interval, so leave it off for casual monitoring.
		#[arg(long)]
		pub threads: bool,
	}

	/// One process snapshot. All counters are cumulative since process (or system) start.
	#[derive(Serialize)]
	struct Sample {
		/// Wall-clock milliseconds since the Unix epoch.
		timestamp_ms: u128,
		pid: i32,
		comm: String,
		/// Seconds this process spent in user mode.
		cpu_user: f64,
		/// Seconds this process spent in kernel mode.
		cpu_system: f64,
		rss_bytes: u64,
		threads: u64,
		/// Voluntary context switches (blocked waiting for I/O or a lock), summed
		/// across all threads the sampler has seen, including exited ones.
		ctx_voluntary: u64,
		/// Involuntary context switches (preempted by the scheduler), summed the
		/// same way.
		ctx_involuntary: u64,
		/// Busy (non-idle, non-iowait) seconds summed across every core on the host,
		/// so the process's share of the whole machine is computable.
		host_cpu_busy: f64,
		host_cores: u64,
		#[serde(skip_serializing_if = "Option::is_none")]
		per_thread: Option<Vec<ThreadSample>>,
	}

	/// One thread's slice of a [`Sample`], for spotting scheduler imbalance and
	/// verifying pinning (the `processor` field is the core it last ran on).
	#[derive(Serialize)]
	struct ThreadSample {
		tid: i32,
		name: String,
		cpu_user: f64,
		cpu_system: f64,
		processor: i32,
		ctx_voluntary: u64,
		ctx_involuntary: u64,
	}

	/// Monotonic context-switch totals for one process.
	///
	/// `/proc/<pid>/task/*` counters vanish when a thread exits, so a plain
	/// per-sample sum can go backwards, which would read as a process restart
	/// downstream. Instead, only nonnegative per-thread deltas accumulate, and an
	/// exited thread's contribution stays in the total.
	#[derive(Default)]
	struct CtxTotals {
		/// Last observed cumulative counters per live thread.
		seen: HashMap<i32, (u64, u64)>,
		voluntary: u64,
		involuntary: u64,
	}

	impl CtxTotals {
		/// Fold one thread's current cumulative counters into the running totals.
		fn observe(&mut self, tid: i32, voluntary: u64, involuntary: u64) {
			let (prev_voluntary, prev_involuntary) = self.seen.get(&tid).copied().unwrap_or((0, 0));
			self.voluntary += voluntary.saturating_sub(prev_voluntary);
			self.involuntary += involuntary.saturating_sub(prev_involuntary);
			self.seen.insert(tid, (voluntary, involuntary));
		}

		/// Forget threads that no longer exist, so a reused tid starts a fresh
		/// baseline instead of diffing against a dead thread's counters.
		fn retain(&mut self, live: impl Fn(i32) -> bool) {
			self.seen.retain(|tid, _| live(*tid));
		}
	}

	/// One process under observation: the /proc handle plus the sampler-side
	/// state that outlives individual threads.
	struct Target {
		proc: Process,
		ctx: CtxTotals,
	}

	/// Resolve the target processes from --pid or --name.
	fn find(args: &Args) -> anyhow::Result<Vec<Target>> {
		if !args.pid.is_empty() {
			return args
				.pid
				.iter()
				.map(|&pid| {
					let proc = Process::new(pid).with_context(|| format!("pid {pid}"))?;
					Ok(Target {
						proc,
						ctx: CtxTotals::default(),
					})
				})
				.collect();
		}

		// A process can exit between the directory listing and the stat read, so
		// per-process errors here mean "gone", not failure.
		let mut found = Vec::new();
		for proc in procfs::process::all_processes()? {
			let Ok(proc) = proc else { continue };
			let Ok(stat) = proc.stat() else { continue };
			if stat.comm == args.name {
				found.push(Target {
					proc,
					ctx: CtxTotals::default(),
				});
			}
		}
		Ok(found)
	}

	/// Cumulative busy seconds across all cores, and the core count.
	///
	/// `guest`/`guest_nice` are excluded: the kernel already accounts guest time
	/// inside `user`/`nice`, so adding them again would double-count it.
	fn host_cpu(tps: f64) -> anyhow::Result<(f64, u64)> {
		let kernel = procfs::KernelStats::current()?;
		let t = &kernel.total;
		let busy = t.user + t.nice + t.system + t.irq.unwrap_or(0) + t.softirq.unwrap_or(0) + t.steal.unwrap_or(0);
		Ok((busy as f64 / tps, kernel.cpu_time.len() as u64))
	}

	/// Snapshot one process; `Err` means it exited (or /proc denied us) and should
	/// be dropped. Everything read here is per-target, so an error only ever
	/// condemns this target; host-wide reads live in the caller and fail the run.
	fn sample(target: &mut Target, tps: f64, page: u64, threads: bool, host: (f64, u64)) -> anyhow::Result<Sample> {
		let stat = target.proc.stat()?;
		let (host_cpu_busy, host_cores) = host;

		// /proc/<pid>/status reports the context-switch counters of the thread group
		// leader alone (utime/stime in stat are the aggregated ones), so the
		// process-wide numbers have to be folded over /proc/<pid>/task/*.
		let mut live = Vec::new();
		let mut per_thread = threads.then(Vec::new);
		for task in target.proc.tasks()? {
			// Threads come and go mid-iteration; skip the ones that vanished.
			let Ok(task) = task else { continue };
			let Ok(tstatus) = task.status() else { continue };
			let voluntary = tstatus.voluntary_ctxt_switches.unwrap_or(0);
			let involuntary = tstatus.nonvoluntary_ctxt_switches.unwrap_or(0);
			target.ctx.observe(task.tid, voluntary, involuntary);
			live.push(task.tid);

			if let Some(out) = &mut per_thread {
				let Ok(tstat) = task.stat() else { continue };
				out.push(ThreadSample {
					tid: task.tid,
					name: tstat.comm,
					cpu_user: tstat.utime as f64 / tps,
					cpu_system: tstat.stime as f64 / tps,
					processor: tstat.processor.unwrap_or(-1),
					ctx_voluntary: voluntary,
					ctx_involuntary: involuntary,
				});
			}
		}
		target.ctx.retain(|tid| live.contains(&tid));

		Ok(Sample {
			timestamp_ms: SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.unwrap_or_default()
				.as_millis(),
			pid: target.proc.pid,
			comm: stat.comm.clone(),
			cpu_user: stat.utime as f64 / tps,
			cpu_system: stat.stime as f64 / tps,
			rss_bytes: stat.rss * page,
			threads: stat.num_threads.max(0) as u64,
			ctx_voluntary: target.ctx.voluntary,
			ctx_involuntary: target.ctx.involuntary,
			host_cpu_busy,
			host_cores,
			per_thread,
		})
	}

	/// Resolve the targets, then sample them every interval until the duration
	/// elapses, the process is interrupted, or every target exits (an error).
	pub fn run() -> anyhow::Result<()> {
		let args = Args::parse();
		let tps = procfs::ticks_per_second() as f64;
		let page = procfs::page_size();

		let mut out: Box<dyn Write> = match &args.output {
			Some(path) => Box::new(std::fs::File::create(path)?),
			None => Box::new(std::io::stdout().lock()),
		};

		let mut targets = find(&args)?;
		anyhow::ensure!(
			!targets.is_empty(),
			"no process matched (name = {:?}); pass --pid or --name",
			args.name
		);
		eprintln!(
			"sampling {} process(es) every {}: {}",
			targets.len(),
			humantime::format_duration(args.interval),
			targets
				.iter()
				.map(|t| t.proc.pid.to_string())
				.collect::<Vec<_>>()
				.join(", ")
		);

		let start = std::time::Instant::now();
		loop {
			// Host-wide once per round: unlike the per-target reads inside `sample`,
			// a failure here is not a target exiting, so it fails the run.
			let host = host_cpu(tps).context("failed to read /proc/stat")?;

			// Drop targets that exited; sampling the survivors is still useful.
			targets.retain_mut(|target| match sample(target, tps, page, args.threads, host) {
				Ok(record) => {
					// A serialization failure is a bug, not a runtime condition.
					let line = serde_json::to_string(&record).expect("sample must serialize");
					if let Err(err) = writeln!(out, "{line}") {
						eprintln!("write failed: {err}");
						std::process::exit(1);
					}
					true
				}
				Err(_) => {
					eprintln!("pid {} exited, dropping", target.proc.pid);
					false
				}
			});
			// Every line lands on disk before the sleep, so a Ctrl-C loses nothing.
			out.flush()?;

			anyhow::ensure!(!targets.is_empty(), "all target processes exited");
			let Some(delay) = next_delay(args.interval, args.duration, start.elapsed()) else {
				return Ok(());
			};
			std::thread::sleep(delay);
		}
	}

	/// How long to sleep before the next sample: the interval, capped at the time
	/// left in `duration`. `None` means the duration has elapsed and the run is
	/// done, so a `--duration` shorter than `--interval` cannot overshoot.
	fn next_delay(interval: Duration, duration: Option<Duration>, elapsed: Duration) -> Option<Duration> {
		let Some(duration) = duration else {
			return Some(interval);
		};
		let remaining = duration.checked_sub(elapsed)?;
		if remaining.is_zero() {
			return None;
		}
		Some(interval.min(remaining))
	}

	#[cfg(test)]
	mod tests {
		use super::*;

		/// `--interval 0s` must be rejected at parse time: with no pacing the
		/// sampler busy-loops over /proc on the very hosts it is meant to be
		/// harmless on.
		#[test]
		fn zero_interval_rejected() {
			let err = Args::try_parse_from(["moq-bench-host", "--interval", "0s"]).unwrap_err();
			assert!(err.to_string().contains("greater than 0s"), "unexpected error: {err}");

			// Nonzero still parses, and the default holds.
			let args = Args::try_parse_from(["moq-bench-host"]).unwrap();
			assert_eq!(args.interval, Duration::from_secs(1));
		}

		/// A duration shorter than the interval must cap the sleep, not stretch the
		/// run to the full interval (10ms of sampling used to take a whole second).
		#[test]
		fn short_duration_caps_the_sleep() {
			let second = Duration::from_secs(1);
			let ms = Duration::from_millis(10);

			// No duration: always the full interval.
			assert_eq!(next_delay(second, None, second * 100), Some(second));
			// Time remains: sleep the smaller of interval and what's left.
			assert_eq!(next_delay(second, Some(ms), Duration::ZERO), Some(ms));
			assert_eq!(next_delay(ms, Some(second), Duration::ZERO), Some(ms));
			// Elapsed at or past the duration: done.
			assert_eq!(next_delay(second, Some(ms), ms), None);
			assert_eq!(next_delay(second, Some(ms), second), None);
		}

		/// The process-wide context-switch totals must stay monotonic when a
		/// thread exits between samples. A naive per-sample sum would drop the
		/// dead thread's counters, go backwards, and read as a process restart
		/// downstream.
		#[test]
		fn ctx_totals_survive_thread_exit() {
			let mut ctx = CtxTotals::default();

			// Sample 1: two live threads.
			ctx.observe(1, 10, 1);
			ctx.observe(2, 5, 2);
			ctx.retain(|tid| [1, 2].contains(&tid));
			assert_eq!((ctx.voluntary, ctx.involuntary), (15, 3));

			// Sample 2: thread 2 exited; thread 1 advanced. Thread 2's counts stay.
			ctx.observe(1, 12, 1);
			ctx.retain(|tid| tid == 1);
			assert_eq!((ctx.voluntary, ctx.involuntary), (17, 3));

			// Sample 3: tid 2 is reused by a fresh thread; its counters start over
			// and must add in full rather than diff against the dead thread's.
			ctx.observe(1, 12, 1);
			ctx.observe(2, 3, 1);
			assert_eq!((ctx.voluntary, ctx.involuntary), (20, 4));
		}
	}
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
	linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
	anyhow::bail!("moq-bench-host reads /proc, so it only runs on Linux")
}
