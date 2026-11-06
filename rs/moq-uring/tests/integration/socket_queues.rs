//! Where a UDP datagram is lost between a sender and a relay worker, across
//! kernel queue size, receive pool depth, workers, flows and load shape, on
//! both receive paths a relay runs: moq-sock's `recvmmsg` under Tokio and
//! moq-uring's multishot provided-buffer ring.
//!
//! The receivers are a steered `SO_REUSEPORT` group from
//! `moq_sock::shard::bind`, so each socket starts with the buffer request a
//! relay makes, and each flow's datagrams carry a short-header connection ID
//! naming the worker the steering filter delivers them to. A cell's queue is
//! either that production request or an explicit `SO_RCVBUF` override.
//!
//! Every datagram is accounted for by counters rather than inferred from
//! timing: sent (what `sendmsg` accepted), delivered (what a worker read),
//! socket drops (the kernel's per-socket `sk_drops`, read with `SO_MEMINFO`,
//! the same counter `SO_RXQ_OVFL` reports per receive), and path drops (the
//! shaper's queue overflow, when the cell routes through one). Whatever is left
//! is `unattributed`; the per-socket counter cannot locate every loss, so a
//! nonzero residual is reported rather than folded into either side. One
//! socket drop is one queued skb: a coalesced GSO train counts once however
//! many datagrams it carried, which the `gso` cells exist to show. The
//! namespace-wide `RcvbufErrors` from `/proc/net/snmp` is printed as a cross
//! check; other processes on the host move it too. `enobufs` is moq-uring's
//! `rx_enobufs`: provided-buffer starvation, which leaves the datagram queued
//! in the socket, so it is not a loss unless the socket queue then overflows.
//!
//! Patterns:
//! - `stall`: steady offered load while each worker blocks for `stall_ms`
//!   and then runs for 100 ms, the way a descheduled or busy worker does.
//! - `contend`: steady load while each worker shares its pinned CPU with
//!   `SPINNERS` spinning threads, so the scheduler, not a sleep, decides the
//!   gaps.
//! - `overload`: offered load above what the workers can process (each
//!   datagram costs `work_us` of CPU), so the queue sits full: loss is set by
//!   the rate gap and the queue size sets only the delay.
//! - `path`: a rate limit below the offered load with a deep, finite queue
//!   whose overflow drops (moq-shaper), so the loss belongs to the path.
//!
//! Ignored: a measurement, not a correctness check. Nightly runs it in the
//! Rust benchmark tests group. Run it with:
//!
//! ```text
//! just rs test -p moq-uring --release -E 'test(benchmark_socket_queues)' --run-ignored only --success-output immediate
//! ```
//!
//! `SOCKET_QUEUES_RUNS` sets the rounds (3 by default); each round runs every
//! cell once, so repeated cells interleave rather than run back to back.

#![expect(
	clippy::disallowed_methods,
	reason = "a benchmark paces load and stalls workers on the host clock"
)]

use std::io::{self, IoSliceMut};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use moq_sock::udp::{BATCH_SIZE, RecvMeta, Transmit, UdpSocketState};

/// Bytes per datagram, a full QUIC packet on a 1280-byte path.
const SEGMENT: usize = 1200;
/// Offered load per cell, across every flow: 480 Mbps of 1200-byte datagrams.
const RATE: u64 = 50_000;
/// How long each cell offers load.
const DURATION: Duration = Duration::from_secs(1);
/// How often a stalling worker stalls.
const STALL_PERIOD: Duration = Duration::from_millis(100);
/// Busy threads sharing each worker's CPU in the `contend` cells.
const SPINNERS: usize = 3;
/// Receive buffers moq-tokio's endpoint driver reads into per `recvmmsg`.
const RECV_BUFFER: usize = SEGMENT * 64;

/// QUIC short header: form bit clear, fixed bit set.
const SHORT_HEADER: u8 = 0x40;
/// The relay CID marker the steering filter requires in the fourth CID byte.
const CID_MARKER: u8 = 0x18;
const KIND_DATA: u8 = 0;
const KIND_STOP: u8 = 1;

// `SO_MEMINFO` slots (linux/sock_diag.h).
const SK_MEMINFO_RMEM_ALLOC: usize = 0;
const SK_MEMINFO_RCVBUF: usize = 1;
const SK_MEMINFO_DROPS: usize = 8;
const SK_MEMINFO_VARS: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
	Tokio,
	Uring,
}

impl Path {
	fn name(self) -> &'static str {
		match self {
			Self::Tokio => "tokio",
			Self::Uring => "uring",
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pattern {
	Stall,
	Contend,
	Overload,
	Shaped,
}

impl Pattern {
	fn name(self) -> &'static str {
		match self {
			Self::Stall => "stall",
			Self::Contend => "contend",
			Self::Overload => "overload",
			Self::Shaped => "path",
		}
	}
}

/// One measured configuration.
#[derive(Clone, Copy, Debug)]
struct Cell {
	/// Which knob this cell moves away from the default, for reading the table.
	axis: &'static str,
	pattern: Pattern,
	workers: u16,
	flows: usize,
	/// `SO_RCVBUF` to request after the production bind, or `None` to keep the
	/// relay's own request.
	queue: Option<usize>,
	/// moq-uring's `rx_buffers_max`; the Tokio path always reads `BATCH_SIZE`.
	pool: u16,
	/// Datagrams per GSO train, 1 for a plain send each.
	gso: usize,
	stall: Duration,
	/// CPU each delivered datagram costs its worker.
	work: Duration,
}

const DEFAULT_RCVBUF: usize = 212_992;

fn cells() -> Vec<Cell> {
	let default = Cell {
		axis: "default",
		pattern: Pattern::Stall,
		workers: 1,
		flows: 16,
		queue: None,
		pool: moq_uring::udp::Config::default().rx_buffers_max,
		gso: 1,
		stall: Duration::from_millis(20),
		work: Duration::ZERO,
	};
	let mut cells = vec![default];
	for queue in [DEFAULT_RCVBUF, 1 << 20, 4 << 20] {
		cells.push(Cell {
			axis: "queue",
			queue: Some(queue),
			..default
		});
	}
	for stall in [2, 100] {
		cells.push(Cell {
			axis: "stall",
			stall: Duration::from_millis(stall),
			..default
		});
	}
	for (pool, queue) in [(16, None), (16, Some(DEFAULT_RCVBUF)), (4096, Some(DEFAULT_RCVBUF))] {
		cells.push(Cell {
			axis: "pool",
			pool,
			queue,
			..default
		});
	}
	cells.push(Cell {
		axis: "workers",
		workers: 4,
		..default
	});
	cells.push(Cell {
		axis: "workers",
		workers: 4,
		queue: Some(DEFAULT_RCVBUF),
		..default
	});
	for flows in [1, 256] {
		cells.push(Cell {
			axis: "flows",
			flows,
			..default
		});
	}
	cells.push(Cell {
		axis: "gso",
		gso: 16,
		queue: Some(DEFAULT_RCVBUF),
		..default
	});
	for queue in [Some(DEFAULT_RCVBUF), None] {
		cells.push(Cell {
			axis: "contend",
			pattern: Pattern::Contend,
			stall: Duration::ZERO,
			queue,
			..default
		});
	}
	for queue in [Some(DEFAULT_RCVBUF), Some(1 << 20), None] {
		cells.push(Cell {
			axis: "overload",
			pattern: Pattern::Overload,
			stall: Duration::ZERO,
			// About 40k datagrams/s of capacity against 50k offered.
			work: Duration::from_micros(25),
			queue,
			..default
		});
	}
	for queue in [Some(DEFAULT_RCVBUF), None] {
		cells.push(Cell {
			axis: "path",
			pattern: Pattern::Shaped,
			stall: Duration::ZERO,
			queue,
			..default
		});
	}
	cells
}

#[test]
#[ignore = "benchmark: run explicitly; prints a table"]
fn benchmark_socket_queues() {
	let runs = std::env::var("SOCKET_QUEUES_RUNS")
		.ok()
		.map(|runs| runs.parse::<usize>().expect("SOCKET_QUEUES_RUNS is a count"))
		.unwrap_or(3);
	let paths = match uring_supported() {
		true => vec![Path::Tokio, Path::Uring],
		false => vec![Path::Tokio],
	};

	print_host();
	println!(
		"run,path,axis,pattern,workers,flows,queue_req,rcvbuf,pool,gso,stall_ms,work_us,sent,send_dropped,delivered,\
		 socket_drops,path_drops,unattributed,snmp_rcvbuf_errors,backlog_drops,enobufs,peak_queue_kib,p50_ms,p99_ms,max_ms,\
		 goodput_mbps,rx_cpu_us_per_dgram"
	);
	let cells = cells();
	for run in 0..runs {
		for path in &paths {
			for cell in &cells {
				// Tokio reads a fixed `BATCH_SIZE`; it has no pool to vary.
				if *path == Path::Tokio && cell.axis == "pool" {
					continue;
				}
				let mut result = measure(*path, *cell).unwrap_or_else(|err| panic!("{path:?} {cell:?}: {err:#}"));
				println!("{run},{},{}", path.name(), result.row(cell));
				result.check();
			}
		}
	}
}

/// Whether this kernel runs moq-uring, skipping loudly where it does not.
fn uring_supported() -> bool {
	match moq_uring::Worker::new(moq_uring::Config::default()) {
		Ok(_) => true,
		Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping the io_uring path: {reason}");
			false
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

/// The settings that bound every cell, so a table names the host it came from.
fn print_host() {
	let read = |path: &str| std::fs::read_to_string(path).map_or_else(|_| "n/a".into(), |s| s.trim().to_string());
	println!("# kernel {}", read("/proc/sys/kernel/osrelease"));
	println!(
		"# net.core.rmem_max {} rmem_default {} wmem_max {}",
		read("/proc/sys/net/core/rmem_max"),
		read("/proc/sys/net/core/rmem_default"),
		read("/proc/sys/net/core/wmem_max")
	);
	println!(
		"# cpus {} loadavg {}",
		std::thread::available_parallelism().map_or(0, usize::from),
		read("/proc/loadavg")
	);
	println!(
		"# offered {RATE} datagrams/s of {SEGMENT} bytes for {DURATION:?} per cell; {STALL_PERIOD:?} between stalls"
	);
}

#[derive(Default)]
struct Result {
	rcvbuf: usize,
	sent: u64,
	send_dropped: u64,
	delivered: u64,
	delivered_bytes: u64,
	socket_drops: u64,
	path_drops: u64,
	snmp_rcvbuf_errors: u64,
	backlog_drops: u64,
	enobufs: u64,
	peak_queue: u64,
	misrouted: u64,
	delays_us: Vec<u32>,
	rx_cpu: Duration,
}

impl Result {
	fn unattributed(&self) -> i64 {
		self.sent as i64 - self.delivered as i64 - self.socket_drops as i64 - self.path_drops as i64
	}

	fn row(&mut self, cell: &Cell) -> String {
		self.delays_us.sort_unstable();
		let quantile = |q: f64| -> f64 {
			match self.delays_us.len() {
				0 => f64::NAN,
				len => f64::from(self.delays_us[((len - 1) as f64 * q).round() as usize]) / 1000.0,
			}
		};
		let queue_req = cell.queue.map_or_else(|| "relay".to_string(), |q| q.to_string());
		let cpu_per = match self.delivered {
			0 => f64::NAN,
			n => self.rx_cpu.as_secs_f64() * 1e6 / n as f64,
		};
		format!(
			"{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.1},{:.2}",
			cell.axis,
			cell.pattern.name(),
			cell.workers,
			cell.flows,
			queue_req,
			self.rcvbuf,
			cell.pool,
			cell.gso,
			cell.stall.as_millis(),
			cell.work.as_micros(),
			self.sent,
			self.send_dropped,
			self.delivered,
			self.socket_drops,
			self.path_drops,
			self.unattributed(),
			self.snmp_rcvbuf_errors,
			self.backlog_drops,
			self.enobufs,
			self.peak_queue / 1024,
			quantile(0.5),
			quantile(0.99),
			quantile(1.0),
			self.delivered_bytes as f64 * 8.0 / DURATION.as_secs_f64() / 1e6,
			cpu_per,
		)
	}

	/// The accounting invariants a table is only worth reading if they hold.
	fn check(&self) {
		assert_eq!(self.misrouted, 0, "the steering filter delivered to the wrong worker");
		assert!(self.delivered > 0, "nothing was delivered");
		assert!(
			self.delivered + self.socket_drops <= self.sent,
			"more datagrams accounted for than sent"
		);
	}
}

/// A socket's `SO_MEMINFO`: its queue, its budget and its drops.
fn meminfo(fd: RawFd) -> io::Result<[u32; SK_MEMINFO_VARS]> {
	let mut mem = [0u32; SK_MEMINFO_VARS];
	let mut len = std::mem::size_of_val(&mem) as libc::socklen_t;
	// SAFETY: `mem` is writable for `len` bytes, which the kernel truncates to.
	let res = unsafe {
		libc::getsockopt(
			fd,
			libc::SOL_SOCKET,
			libc::SO_MEMINFO,
			mem.as_mut_ptr().cast(),
			&mut len,
		)
	};
	if res != 0 {
		return Err(io::Error::last_os_error());
	}
	Ok(mem)
}

/// Each socket's cumulative `sk_drops`.
fn drops(sockets: &[UdpSocket]) -> io::Result<Vec<u32>> {
	sockets
		.iter()
		.map(|socket| meminfo(socket.as_raw_fd()).map(|mem| mem[SK_MEMINFO_DROPS]))
		.collect()
}

/// Datagrams the host dropped before any socket saw them, because a CPU's
/// input backlog (`net.core.netdev_max_backlog`) was full: the second column
/// of `/proc/net/softnet_stat`, summed over CPUs. Loopback delivers through
/// that backlog, and a socket's `sk_drops` never sees these.
fn backlog_drops() -> u64 {
	std::fs::read_to_string("/proc/net/softnet_stat")
		.unwrap_or_default()
		.lines()
		.filter_map(|line| line.split_whitespace().nth(1))
		.filter_map(|dropped| u64::from_str_radix(dropped, 16).ok())
		.sum()
}

/// The namespace's cumulative UDP `RcvbufErrors`, IPv4 and IPv6 together.
fn snmp_rcvbuf_errors() -> u64 {
	let v4 = std::fs::read_to_string("/proc/net/snmp").unwrap_or_default();
	let mut lines = v4.lines().filter(|line| line.starts_with("Udp: "));
	let v4 = match (lines.next(), lines.next()) {
		(Some(names), Some(values)) => names
			.split_whitespace()
			.zip(values.split_whitespace())
			.find(|(name, _)| *name == "RcvbufErrors")
			.and_then(|(_, value)| value.parse().ok())
			.unwrap_or(0),
		_ => 0,
	};
	let v6 = std::fs::read_to_string("/proc/net/snmp6")
		.unwrap_or_default()
		.lines()
		.find_map(|line| line.strip_prefix("Udp6RcvbufErrors"))
		.and_then(|value| value.trim().parse().ok())
		.unwrap_or(0);
	v4 + v6
}

fn now_ns() -> u64 {
	let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	// SAFETY: `ts` is a valid out-pointer.
	unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
	ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn thread_cpu() -> Duration {
	let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	// SAFETY: `ts` is a valid out-pointer.
	unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
	Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

fn spin(duration: Duration) {
	if duration.is_zero() {
		return;
	}
	let until = now_ns() + duration.as_nanos() as u64;
	while now_ns() < until {
		std::hint::spin_loop();
	}
}

fn pin(cpu: usize) {
	// SAFETY: a zeroed cpu_set_t is empty, and CPU_SET stays inside it.
	unsafe {
		let mut set: libc::cpu_set_t = std::mem::zeroed();
		libc::CPU_SET(cpu, &mut set);
		libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set);
	}
}

/// The CPUs this process may run on, so contention pins never leave its cpuset.
fn allowed_cpus() -> Vec<usize> {
	// SAFETY: a zeroed cpu_set_t is a valid out-parameter.
	unsafe {
		let mut set: libc::cpu_set_t = std::mem::zeroed();
		libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set);
		(0..libc::CPU_SETSIZE as usize)
			.filter(|cpu| libc::CPU_ISSET(*cpu, &set))
			.collect()
	}
}

/// Fill one datagram's header: a short header whose CID steers to `worker`.
fn header(datagram: &mut [u8], worker: u16, kind: u8, sent_ns: u64) {
	datagram[0] = SHORT_HEADER;
	datagram[1] = worker as u8;
	datagram[2] = 0;
	datagram[3] = 0;
	datagram[4] = CID_MARKER;
	datagram[5] = kind;
	datagram[9..17].copy_from_slice(&sent_ns.to_le_bytes());
}

/// What one worker counts as it reads.
struct Tally {
	worker: u16,
	cell: Cell,
	delivered: Arc<AtomicU64>,
	delivered_bytes: u64,
	misrouted: u64,
	delays_us: Vec<u32>,
	next_stall: u64,
	stopped: bool,
}

impl Tally {
	fn new(worker: u16, cell: Cell, delivered: Arc<AtomicU64>) -> Self {
		// Stagger the workers' stalls so a group does not stall in lockstep.
		let offset = STALL_PERIOD.as_nanos() as u64 * u64::from(worker) / u64::from(cell.workers);
		Self {
			worker,
			cell,
			delivered,
			delivered_bytes: 0,
			misrouted: 0,
			delays_us: Vec::with_capacity((RATE * DURATION.as_secs()) as usize / usize::from(cell.workers) + 1024),
			next_stall: now_ns() + STALL_PERIOD.as_nanos() as u64 + offset,
			stopped: false,
		}
	}

	/// Account for one receive: `payload` holds datagrams `stride` apart.
	fn receive(&mut self, payload: &[u8], stride: usize) {
		let mut count = 0;
		for datagram in payload.chunks(stride.max(1)) {
			if datagram.len() < 17 || datagram[0] != SHORT_HEADER {
				continue;
			}
			if datagram[1] as u16 != self.worker {
				self.misrouted += 1;
			}
			if datagram[5] == KIND_STOP {
				self.stopped = true;
				continue;
			}
			let sent = u64::from_le_bytes(datagram[9..17].try_into().expect("8 bytes"));
			let delay = now_ns().saturating_sub(sent) / 1000;
			self.delays_us.push(delay.min(u64::from(u32::MAX)) as u32);
			self.delivered_bytes += datagram.len() as u64;
			count += 1;
			spin(self.cell.work);
		}
		self.delivered.fetch_add(count, Ordering::Relaxed);
		self.maybe_stall();
	}

	fn maybe_stall(&mut self) {
		if self.cell.stall.is_zero() {
			return;
		}
		if now_ns() >= self.next_stall {
			std::thread::sleep(self.cell.stall);
			// The period runs from the end of a stall, so even a stall as long
			// as the period leaves the worker half its time to drain.
			self.next_stall = now_ns() + STALL_PERIOD.as_nanos() as u64;
		}
	}
}

/// What a worker thread hands back.
struct WorkerResult {
	tally: Tally,
	cpu: Duration,
	enobufs: u64,
}

fn run_tokio(socket: UdpSocket, mut tally: Tally) -> anyhow::Result<WorkerResult> {
	socket.set_nonblocking(true)?;
	let runtime = tokio::runtime::Builder::new_current_thread().enable_io().build()?;
	let start = thread_cpu();
	runtime.block_on(async {
		let socket = tokio::net::UdpSocket::from_std(socket)?;
		let state = UdpSocketState::new((&socket).into())?;
		let mut buffers = vec![0u8; RECV_BUFFER * BATCH_SIZE];
		let mut metas = [RecvMeta::default(); BATCH_SIZE];
		while !tally.stopped {
			socket.readable().await?;
			let count = {
				let mut chunks = buffers.chunks_mut(RECV_BUFFER);
				let mut iovs: [IoSliceMut<'_>; BATCH_SIZE] =
					std::array::from_fn(|_| IoSliceMut::new(chunks.next().expect("BATCH_SIZE buffers")));
				match socket.try_io(tokio::io::Interest::READABLE, || {
					state.recv((&socket).into(), &mut iovs, &mut metas)
				}) {
					Ok(count) => count,
					Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
					Err(err) => return Err(err.into()),
				}
			};
			for (meta, buffer) in metas[..count].iter().zip(buffers.chunks(RECV_BUFFER)) {
				tally.receive(&buffer[..meta.len], meta.stride);
			}
		}
		anyhow::Ok(())
	})?;
	Ok(WorkerResult {
		cpu: thread_cpu() - start,
		tally,
		enobufs: 0,
	})
}

fn run_uring(socket: UdpSocket, mut tally: Tally) -> anyhow::Result<WorkerResult> {
	let mut worker = moq_uring::Worker::new(moq_uring::Config::default())?;
	let handle = worker.handle();
	let mut config = moq_uring::udp::Config::default();
	config.rx_buffers_max = tally.cell.pool;
	let socket = handle.udp(socket, config)?;
	let start = thread_cpu();
	let tally = worker.block_on(async move {
		while !tally.stopped {
			let packet = socket.recv().await?;
			tally.receive(packet.payload(), packet.stride());
		}
		anyhow::Ok(tally)
	})??;
	Ok(WorkerResult {
		cpu: thread_cpu() - start,
		tally,
		enobufs: handle.metrics().snapshot().rx_enobufs,
	})
}

/// moq-shaper on its own runtime thread, forwarding to the group.
struct Shaper {
	addr: SocketAddr,
	stop: tokio::sync::oneshot::Sender<()>,
	thread: std::thread::JoinHandle<anyhow::Result<moq_shaper::Stats>>,
}

/// The shaped path's queue: deep, so it smooths rather than drops a burst,
/// and finite, so a rate gap overflows it.
const PATH_QUEUE: Duration = Duration::from_millis(200);

impl Shaper {
	fn start(target: SocketAddr) -> anyhow::Result<Self> {
		let (ready, addr) = std::sync::mpsc::channel();
		let (stop, stopped) = tokio::sync::oneshot::channel();
		let thread = std::thread::spawn(move || {
			let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
			runtime.block_on(async move {
				let profile = moq_shaper::Profile {
					// 80% of the offered load.
					rate: Some(moq_shaper::Rate {
						bits_per_second: RATE * SEGMENT as u64 * 8 * 4 / 5,
						burst: 64 * 1024,
						queue: PATH_QUEUE,
					}),
					..Default::default()
				};
				let shaper = moq_shaper::Shaper::bind(moq_shaper::Setup {
					config: moq_shaper::Config {
						bind: (Ipv4Addr::LOCALHOST, 0).into(),
						target,
						seed: 1,
						up: profile,
						down: moq_shaper::Profile::default(),
					},
					// One bottleneck for every flow, as behind one access link.
					shared: true,
					up: Default::default(),
					down: Default::default(),
				})
				.await?;
				ready.send(shaper.addr()).ok();
				stopped.await.ok();
				Ok(shaper.stats())
			})
		});
		let addr = addr.recv().map_err(|_| anyhow::anyhow!("shaper failed to start"))?;
		Ok(Self { addr, stop, thread })
	}

	fn finish(self) -> anyhow::Result<moq_shaper::Stats> {
		self.stop.send(()).ok();
		self.thread.join().map_err(|_| anyhow::anyhow!("shaper panicked"))?
	}
}

fn measure(path: Path, cell: Cell) -> anyhow::Result<Result> {
	let mut group = moq_sock::shard::bind((Ipv4Addr::LOCALHOST, 0).into(), cell.workers)?;
	let addr = group.addr();
	let mut sockets = Vec::new();
	while let Some(member) = group.member()? {
		sockets.push(member.into_inner());
	}
	if let Some(queue) = cell.queue {
		for socket in &sockets {
			nix::sys::socket::setsockopt(socket, nix::sys::socket::sockopt::RcvBuf, &queue)?;
		}
	}
	// Duplicates of each member, read for its queue while the worker owns it.
	let monitors = sockets
		.iter()
		.map(UdpSocket::try_clone)
		.collect::<io::Result<Vec<_>>>()?;
	let rcvbuf = meminfo(monitors[0].as_raw_fd())?[SK_MEMINFO_RCVBUF] as usize;
	let drops_before = drops(&monitors)?;
	let backlog_before = backlog_drops();

	let delivered = Arc::new(AtomicU64::new(0));
	let contention = Arc::new(AtomicBool::new(false));
	let cpus = allowed_cpus();
	let mut spinners = Vec::new();
	let mut workers = Vec::new();
	for (index, socket) in sockets.into_iter().enumerate() {
		let tally = Tally::new(index as u16, cell, delivered.clone());
		// Pin from the top of the cpuset down, away from where the sender starts.
		let cpu = cpus[cpus.len() - 1 - index % cpus.len()];
		if cell.pattern == Pattern::Contend {
			for _ in 0..SPINNERS {
				let stop = contention.clone();
				spinners.push(std::thread::spawn(move || {
					pin(cpu);
					while !stop.load(Ordering::Relaxed) {
						std::hint::spin_loop();
					}
				}));
			}
		}
		let pattern = cell.pattern;
		workers.push(std::thread::spawn(move || {
			if pattern == Pattern::Contend {
				pin(cpu);
			}
			match path {
				Path::Tokio => run_tokio(socket, tally),
				Path::Uring => run_uring(socket, tally),
			}
		}));
	}

	let shaper = match cell.pattern {
		Pattern::Shaped => Some(Shaper::start(addr)?),
		_ => None,
	};
	let target = shaper.as_ref().map_or(addr, |shaper| shaper.addr);

	// Sample each member's queue occupancy while load runs.
	let sampling = Arc::new(AtomicBool::new(true));
	let sampler = {
		let sampling = sampling.clone();
		let fds: Vec<RawFd> = monitors.iter().map(AsRawFd::as_raw_fd).collect();
		std::thread::spawn(move || {
			let mut peak = 0u64;
			while sampling.load(Ordering::Relaxed) {
				let total: u64 = fds
					.iter()
					.filter_map(|fd| meminfo(*fd).ok())
					.map(|mem| u64::from(mem[SK_MEMINFO_RMEM_ALLOC]))
					.sum();
				peak = peak.max(total);
				std::thread::sleep(Duration::from_millis(1));
			}
			peak
		})
	};

	let snmp_before = snmp_rcvbuf_errors();
	let (sent, send_dropped) = send(cell, target)?;

	// Drain: wait out the path's queue, then until nothing more arrives.
	if cell.pattern == Pattern::Shaped {
		std::thread::sleep(PATH_QUEUE * 2);
	}
	let mut last = u64::MAX;
	loop {
		std::thread::sleep(Duration::from_millis(100) + cell.stall);
		let now = delivered.load(Ordering::Relaxed);
		if now == last {
			break;
		}
		last = now;
	}
	sampling.store(false, Ordering::Relaxed);
	let peak_queue = sampler.join().expect("sampler");
	let snmp_after = snmp_rcvbuf_errors();
	// `sk_drops` is a wrapping 32-bit counter: diff each socket on its own.
	let socket_drops = drops(&monitors)?
		.iter()
		.zip(&drops_before)
		.map(|(after, before)| u64::from(after.wrapping_sub(*before)))
		.sum();
	let backlog = backlog_drops().saturating_sub(backlog_before);
	let path_drops = match shaper {
		Some(shaper) => {
			let stats = shaper.finish()?;
			stats.up.overflowed + stats.up.lost
		}
		None => 0,
	};

	// Stop each worker with a datagram steered to it, straight to the group.
	let stopper = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
	let mut stop = [0u8; SEGMENT];
	let mut result = Result {
		rcvbuf,
		sent,
		send_dropped,
		delivered: delivered.load(Ordering::Relaxed),
		socket_drops,
		backlog_drops: backlog,
		path_drops,
		snmp_rcvbuf_errors: snmp_after.saturating_sub(snmp_before),
		peak_queue,
		..Default::default()
	};
	for (index, worker) in workers.into_iter().enumerate() {
		header(&mut stop, index as u16, KIND_STOP, 0);
		while !worker.is_finished() {
			stopper.send_to(&stop, addr)?;
			std::thread::sleep(Duration::from_millis(20));
		}
		let worker = worker.join().expect("worker thread")?;
		result.delivered_bytes += worker.tally.delivered_bytes;
		result.misrouted += worker.tally.misrouted;
		result.delays_us.extend(worker.tally.delays_us);
		result.rx_cpu += worker.cpu;
		result.enobufs += worker.enobufs;
	}
	contention.store(true, Ordering::Relaxed);
	for spinner in spinners {
		spinner.join().expect("spinner");
	}
	drop(group);
	Ok(result)
}

/// Offer `RATE` datagrams/s across `cell.flows` sockets for `DURATION`,
/// returning what `sendmsg` accepted and what it refused.
fn send(cell: Cell, target: SocketAddr) -> anyhow::Result<(u64, u64)> {
	let flows = (0..cell.flows)
		.map(|_| {
			let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
			socket.set_nonblocking(true)?;
			let state = UdpSocketState::new((&socket).into())?;
			anyhow::Ok((socket, state))
		})
		.collect::<anyhow::Result<Vec<_>>>()?;
	if cell.gso > 1 {
		anyhow::ensure!(
			flows[0].1.max_gso_segments() >= cell.gso,
			"kernel supports fewer than {} GSO segments",
			cell.gso
		);
	}

	let total = RATE * DURATION.as_secs();
	let mut train = vec![0u8; SEGMENT * cell.gso];
	let (mut sent, mut refused) = (0u64, 0u64);
	let start = now_ns();
	let mut flow = 0;
	while sent + refused < total {
		let due = (now_ns() - start) * RATE / 1_000_000_000;
		if sent + refused >= due {
			std::thread::sleep(Duration::from_micros(200));
			continue;
		}
		let count = (cell.gso as u64).min(total - sent - refused) as usize;
		let worker = (flow % usize::from(cell.workers)) as u16;
		let sent_ns = now_ns();
		for datagram in train[..count * SEGMENT].chunks_mut(SEGMENT) {
			header(datagram, worker, KIND_DATA, sent_ns);
		}
		let (socket, state) = &flows[flow];
		let transmit = Transmit {
			destination: target,
			ecn: None,
			contents: &train[..count * SEGMENT],
			segment_size: (count > 1).then_some(SEGMENT),
			src_ip: None,
		};
		match state.try_send(socket.into(), &transmit) {
			Ok(()) => sent += count as u64,
			Err(err) if err.kind() == io::ErrorKind::WouldBlock => refused += count as u64,
			Err(err) => return Err(err.into()),
		}
		flow = (flow + 1) % flows.len();
	}
	Ok((sent, refused))
}
