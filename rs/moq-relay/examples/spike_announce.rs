//! Throwaway spike (never merges): announce propagation delay and announce table
//! memory for W per-thread origins meshed over the in-memory transport, the
//! control plane of the cluster spike (`SPIKE_CLUSTER=1`).
//!
//! Each worker is a thread with a current-thread runtime and its own origin,
//! linked to every other worker by a moq-lite-06 memory session that publishes
//! and subscribes like a dialed cluster peer (`src/spike.rs`).
//!
//! - Delay: one broadcast at a time, published on worker `k % W`; the delay is
//!   each other worker's `Start` minus the publisher's own local `Start`.
//! - Memory: N broadcasts spread over the workers, then the process's live heap
//!   (a counting allocator) once every worker has seen all N and the heap settles.
//!
//! `cargo run --release -p moq-relay --example spike_announce -- [workers...]`

use std::{
	alloc::{GlobalAlloc, Layout, System},
	sync::atomic::{AtomicIsize, AtomicU64, Ordering},
	time::{Duration, Instant},
};

use moq_net::{origin, transport::memory};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		ALLOCS.fetch_add(1, Ordering::Relaxed);
		unsafe { System.alloc(layout) }
	}
	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.dealloc(ptr, layout) }
	}
	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
		LIVE.fetch_add(new as isize - layout.size() as isize, Ordering::Relaxed);
		ALLOCS.fetch_add(1, Ordering::Relaxed);
		unsafe { System.realloc(ptr, layout, new) }
	}
}

#[global_allocator]
static GLOBAL: Counting = Counting;

enum Command {
	Publish(String),
	/// Publish the `live` broadcast with one track here.
	Live,
	/// Subscribe to `live/t` here, reporting each frame's arrival.
	Watch,
	/// Write a one-frame group carrying this sequence number, reporting its write time.
	Write(u64),
	Stop,
}

struct Delivered {
	worker: usize,
	sequence: u64,
	at: Instant,
}

struct Seen {
	worker: usize,
	path: String,
	at: Instant,
}

struct End {
	dial: bool,
	session: memory::Session,
}

fn mesh(workers: usize) -> Vec<Vec<End>> {
	let mut ends: Vec<Vec<End>> = (0..workers).map(|_| Vec::new()).collect();
	for low in 0..workers {
		for high in low + 1..workers {
			let mut config = memory::Config::default();
			config.protocol = Some("moq-lite-06".into());
			let (client, server) = memory::pair(config);
			ends[low].push(End {
				dial: true,
				session: client,
			});
			ends[high].push(End {
				dial: false,
				session: server,
			});
		}
	}
	ends
}

async fn link(origin: origin::Producer, end: End) {
	let origin = origin.with_link(origin::Link::Peer);
	let publish = origin.consume().with_hidden(true);
	let now = moq_net::time::Instant::now();
	let started = match end.dial {
		true => {
			moq_net::Client::new()
				.with_publisher(publish)
				.with_subscriber(origin)
				.connect(now, end.session)
				.await
		}
		false => {
			moq_net::Server::new()
				.with_publisher(publish)
				.with_subscriber(origin)
				.accept(now, end.session)
				.await
		}
	};
	let (session, driver) = started.expect("mesh link handshake");
	moq_net::time::run(driver).await;
	drop(session);
}

fn worker(
	index: usize,
	ends: Vec<End>,
	mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
	seen: std::sync::mpsc::Sender<Seen>,
	delivered: std::sync::mpsc::Sender<Delivered>,
) {
	let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
	let local = tokio::task::LocalSet::new();
	local.block_on(&runtime, async move {
		let (origin, driver) = origin::Producer::new(origin::Config::default());
		tokio::task::spawn_local(moq_net::time::run(driver));
		for end in ends {
			tokio::task::spawn_local(link(origin.clone(), end));
		}
		let mut announced = origin.consume().announced();
		tokio::task::spawn_local(async move {
			while let Some(event) = announced.next().await {
				if let moq_net::announce::Event::Start(announce) = event {
					let _ = seen.send(Seen {
						worker: index,
						path: announce.prefix.to_string(),
						at: Instant::now(),
					});
				}
			}
		});
		let mut held = Vec::new();
		let mut live = None;
		while let Some(command) = commands.recv().await {
			match command {
				Command::Publish(path) => held.push(origin.publish(path.as_str(), Default::default()).unwrap()),
				Command::Live => {
					let broadcast = origin.publish("live", Default::default()).unwrap();
					let track = broadcast.create_track("t", None).unwrap();
					live = Some((broadcast, track));
				}
				Command::Watch => {
					let consumer = origin.consume();
					let delivered = delivered.clone();
					tokio::task::spawn_local(async move {
						let broadcast = consumer.request("live", None).await.expect("live resolves");
						let mut subscriber = broadcast
							.track("t")
							.unwrap()
							.subscribe(None)
							.await
							.expect("subscribe live");
						while let Ok(Some(mut group)) = subscriber.recv_group().await {
							while let Ok(Some(frame)) = group.read_frame().await {
								let at = Instant::now();
								let sequence = u64::from_le_bytes(frame.payload[..8].try_into().unwrap());
								let _ = delivered.send(Delivered {
									worker: index,
									sequence,
									at,
								});
							}
						}
					});
				}
				Command::Write(sequence) => {
					let (_, track) = live.as_mut().expect("live published here");
					let at = Instant::now();
					let mut payload = sequence.to_le_bytes().to_vec();
					payload.resize(300, 0);
					track.write_frame(None, payload).unwrap();
					let _ = delivered.send(Delivered {
						worker: usize::MAX,
						sequence,
						at,
					});
				}
				Command::Stop => break,
			}
		}
		drop(live);
		drop(held);
	});
}

/// Wait until every worker has seen each of `paths`; returns each path's per-worker arrival.
fn await_all(
	seen: &std::sync::mpsc::Receiver<Seen>,
	workers: usize,
	paths: &[String],
) -> std::collections::HashMap<String, Vec<Option<Instant>>> {
	let mut arrivals: std::collections::HashMap<String, Vec<Option<Instant>>> =
		paths.iter().map(|path| (path.clone(), vec![None; workers])).collect();
	let mut missing = paths.len() * workers;
	while missing > 0 {
		let event = seen.recv_timeout(Duration::from_secs(30)).expect("announce never arrived");
		if let Some(slots) = arrivals.get_mut(&event.path)
			&& slots[event.worker].is_none()
		{
			slots[event.worker] = Some(event.at);
			missing -= 1;
		}
	}
	arrivals
}

/// Live heap once it stops moving for 500 ms.
fn settled() -> isize {
	let mut last = LIVE.load(Ordering::Relaxed);
	loop {
		std::thread::sleep(Duration::from_millis(500));
		let now = LIVE.load(Ordering::Relaxed);
		if (now - last).abs() <= last / 1000 {
			return now;
		}
		last = now;
	}
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
	sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn run(workers: usize, rounds: usize, broadcasts: usize) {
	let (seen_tx, seen) = std::sync::mpsc::channel();
	let (delivered_tx, delivered) = std::sync::mpsc::channel();
	let mut senders = Vec::new();
	let mut threads = Vec::new();
	for (index, ends) in mesh(workers).into_iter().enumerate() {
		let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
		senders.push(tx);
		let seen = seen_tx.clone();
		let delivered = delivered_tx.clone();
		threads.push(std::thread::spawn(move || worker(index, ends, rx, seen, delivered)));
	}

	// Warm up: every link up and passing announcements.
	senders[0].send(Command::Publish("warmup".into())).unwrap();
	await_all(&seen, workers, &["warmup".into()]);

	// Delay, one broadcast at a time.
	let mut delays = Vec::new();
	for round in 0..rounds {
		let path = format!("delay/{round}");
		let publisher = round % workers;
		senders[publisher].send(Command::Publish(path.clone())).unwrap();
		let arrivals = await_all(&seen, workers, std::slice::from_ref(&path));
		let slots = &arrivals[&path];
		let start = slots[publisher].unwrap();
		for (worker, at) in slots.iter().enumerate() {
			if worker != publisher {
				delays.push(at.unwrap() - start);
			}
		}
	}

	// Frame delivery: one-frame groups written on worker 0, read by a subscriber on
	// every worker; worker 0's own subscriber is the shared-origin reference.
	senders[0].send(Command::Live).unwrap();
	await_all(&seen, workers, &["live".into()]);
	for sender in &senders {
		sender.send(Command::Watch).unwrap();
	}
	std::thread::sleep(Duration::from_millis(200));
	let mut local = Vec::new();
	let mut remote = Vec::new();
	for sequence in 0..rounds as u64 {
		senders[0].send(Command::Write(sequence)).unwrap();
		let mut written = None;
		let mut arrivals = vec![None; workers];
		while written.is_none() || arrivals.iter().any(Option::is_none) {
			let event = delivered.recv_timeout(Duration::from_secs(30)).expect("frame never arrived");
			if event.sequence != sequence {
				continue;
			}
			match event.worker {
				usize::MAX => written = Some(event.at),
				worker => arrivals[worker] = Some(event.at),
			}
		}
		let written = written.unwrap();
		for (worker, at) in arrivals.into_iter().enumerate() {
			let delay = at.unwrap().saturating_duration_since(written);
			match worker {
				0 => local.push(delay),
				_ => remote.push(delay),
			}
		}
		std::thread::sleep(Duration::from_millis(2));
	}
	local.sort();
	remote.sort();
	let us = |d: Duration| d.as_secs_f64() * 1e6;
	match remote.is_empty() {
		true => eprintln!(
			"W={workers} frame delay local p50 {:.1} p99 {:.1} us",
			us(percentile(&local, 0.5)),
			us(percentile(&local, 0.99))
		),
		false => eprintln!(
			"W={workers} frame delay local p50 {:.1} p99 {:.1} us; across the hop p50 {:.1} p99 {:.1} max {:.1} us",
			us(percentile(&local, 0.5)),
			us(percentile(&local, 0.99)),
			us(percentile(&remote, 0.5)),
			us(percentile(&remote, 0.99)),
			us(*remote.last().unwrap()),
		),
	}

	// Memory and convergence, N broadcasts at once.
	let before = settled();
	let allocs = ALLOCS.load(Ordering::Relaxed);
	let paths: Vec<String> = (0..broadcasts).map(|i| format!("mem/{i}")).collect();
	let started = Instant::now();
	for (i, path) in paths.iter().enumerate() {
		senders[i % workers].send(Command::Publish(path.clone())).unwrap();
	}
	let arrivals = await_all(&seen, workers, &paths);
	let converged = arrivals
		.values()
		.flat_map(|slots| slots.iter().map(|at| at.unwrap()))
		.max()
		.unwrap()
		- started;
	let after = settled();
	let allocs = ALLOCS.load(Ordering::Relaxed) - allocs;

	delays.sort();
	let per = (after - before) as f64 / broadcasts as f64;
	match delays.is_empty() {
		true => print!("{workers:>3} {:>9} {:>9} {:>9}", "-", "-", "-"),
		false => print!(
			"{workers:>3} {:>9.1} {:>9.1} {:>9.1}",
			percentile(&delays, 0.5).as_secs_f64() * 1e6,
			percentile(&delays, 0.99).as_secs_f64() * 1e6,
			delays.last().unwrap().as_secs_f64() * 1e6,
		),
	}
	println!(
		" {broadcasts:>7} {:>10.1} {:>12.0} {:>12.0} {:>12.1}",
		converged.as_secs_f64() * 1e3,
		per,
		per / workers as f64,
		allocs as f64 / broadcasts as f64,
	);

	for sender in &senders {
		let _ = sender.send(Command::Stop);
	}
	for thread in threads {
		let _ = thread.join();
	}
}

fn main() {
	let mut args = std::env::args().skip(1).map(|arg| arg.parse::<usize>().unwrap());
	let broadcasts = args.next().unwrap_or(10_000);
	let workers: Vec<usize> = args.collect();
	let workers = match workers.is_empty() {
		true => vec![1, 2, 4, 8, 16],
		false => workers,
	};
	println!(
		"  W   p50(us)   p99(us)   max(us)       N  conv(ms)  heap B/bcast  per worker  allocs/bcast"
	);
	for w in workers {
		run(w, 200, broadcasts);
	}
}
