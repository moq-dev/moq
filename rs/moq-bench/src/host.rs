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
//! process restarted. Combine with the load generator's `--output` to compute
//! CPU per connection and CPU per message (see the README).

#[cfg(target_os = "linux")]
mod linux {
	use std::io::Write;
	use std::time::{Duration, SystemTime, UNIX_EPOCH};

	use clap::Parser;
	use procfs::CurrentSI;
	use procfs::process::Process;
	use serde::Serialize;

	/// Sample CPU, memory, and context-switch counters for a running process.
	#[derive(Parser)]
	#[command(version = env!("VERSION"))]
	pub struct Args {
		/// Sample these PIDs. When set, --name is ignored.
		#[arg(long)]
		pub pid: Vec<i32>,

		/// Find target processes by name (/proc/<pid>/comm, 15 bytes max).
		#[arg(long, default_value = "moq-relay")]
		pub name: String,

		/// How often to sample.
		#[arg(long, value_parser = humantime::parse_duration, default_value = "1s")]
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
		/// across all threads. A dip means a thread exited and took its count along.
		ctx_voluntary: u64,
		/// Involuntary context switches (preempted by the scheduler), summed across
		/// all threads.
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

	/// Resolve the target processes from --pid or --name.
	fn find(args: &Args) -> anyhow::Result<Vec<Process>> {
		if !args.pid.is_empty() {
			return args
				.pid
				.iter()
				.map(|&pid| Process::new(pid).map_err(|err| anyhow::anyhow!("pid {pid}: {err}")))
				.collect();
		}

		// A process can exit between the directory listing and the stat read, so
		// per-process errors here mean "gone", not failure.
		let mut found = Vec::new();
		for proc in procfs::process::all_processes()? {
			let Ok(proc) = proc else { continue };
			let Ok(stat) = proc.stat() else { continue };
			if stat.comm == args.name {
				found.push(proc);
			}
		}
		Ok(found)
	}

	/// Cumulative busy seconds across all cores, and the core count.
	fn host_cpu(tps: f64) -> anyhow::Result<(f64, u64)> {
		let kernel = procfs::KernelStats::current()?;
		let t = &kernel.total;
		let busy = t.user
			+ t.nice + t.system
			+ t.irq.unwrap_or(0)
			+ t.softirq.unwrap_or(0)
			+ t.steal.unwrap_or(0)
			+ t.guest.unwrap_or(0)
			+ t.guest_nice.unwrap_or(0);
		Ok((busy as f64 / tps, kernel.cpu_time.len() as u64))
	}

	/// Snapshot one process; `Err` means it exited (or /proc denied us) and should be dropped.
	fn sample(proc: &Process, tps: f64, page: u64, threads: bool) -> anyhow::Result<Sample> {
		let stat = proc.stat()?;
		let (host_cpu_busy, host_cores) = host_cpu(tps)?;

		// /proc/<pid>/status reports the context-switch counters of the thread group
		// leader alone (utime/stime in stat are the aggregated ones), so the
		// process-wide numbers have to be summed over /proc/<pid>/task/*.
		let mut ctx_voluntary = 0;
		let mut ctx_involuntary = 0;
		let mut per_thread = threads.then(Vec::new);
		for task in proc.tasks()? {
			// Threads come and go mid-iteration; skip the ones that vanished.
			let Ok(task) = task else { continue };
			let Ok(tstatus) = task.status() else { continue };
			let voluntary = tstatus.voluntary_ctxt_switches.unwrap_or(0);
			let involuntary = tstatus.nonvoluntary_ctxt_switches.unwrap_or(0);
			ctx_voluntary += voluntary;
			ctx_involuntary += involuntary;

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

		Ok(Sample {
			timestamp_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
			pid: proc.pid,
			comm: stat.comm.clone(),
			cpu_user: stat.utime as f64 / tps,
			cpu_system: stat.stime as f64 / tps,
			rss_bytes: stat.rss * page,
			threads: stat.num_threads.max(0) as u64,
			ctx_voluntary,
			ctx_involuntary,
			host_cpu_busy,
			host_cores,
			per_thread,
		})
	}

	pub fn run() -> anyhow::Result<()> {
		let args = Args::parse();
		let tps = procfs::ticks_per_second() as f64;
		let page = procfs::page_size();

		let mut out: Box<dyn Write> = match &args.output {
			Some(path) => Box::new(std::fs::File::create(path)?),
			None => Box::new(std::io::stdout().lock()),
		};

		let mut procs = find(&args)?;
		anyhow::ensure!(
			!procs.is_empty(),
			"no process matched (name = {:?}); pass --pid or --name",
			args.name
		);
		eprintln!(
			"sampling {} process(es) every {}: {}",
			procs.len(),
			humantime::format_duration(args.interval),
			procs.iter().map(|p| p.pid.to_string()).collect::<Vec<_>>().join(", ")
		);

		let start = std::time::Instant::now();
		loop {
			// Drop targets that exited; sampling the survivors is still useful.
			procs.retain(|proc| match sample(proc, tps, page, args.threads) {
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
					eprintln!("pid {} exited, dropping", proc.pid);
					false
				}
			});
			// Every line lands on disk before the sleep, so a Ctrl-C loses nothing.
			out.flush()?;

			anyhow::ensure!(!procs.is_empty(), "all target processes exited");
			if let Some(duration) = args.duration
				&& start.elapsed() >= duration
			{
				return Ok(());
			}
			std::thread::sleep(args.interval);
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
