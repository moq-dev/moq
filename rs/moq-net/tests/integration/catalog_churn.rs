#![cfg(feature = "async")]

//! Seeded churn on a catalog: one open group at a time, a snapshot then deltas, read
//! on `R` by subscribers and short fetches that come and go while `R`'s routes to the
//! publisher `P` (through `A`, through `B`, and direct) appear and disappear.
//!
//! A reader may fail loudly when its route goes (its client asks again), but it must
//! never wait forever: every subscriber gets each group it reads from the snapshot
//! on, and every fetch gets the whole group written so far. A failing seed is
//! shrunk to the ops it needs, which the report names.

use crate::support;

use std::time::Duration;

use futures::{StreamExt, channel::mpsc};
use moq_net::{Error, Hop, Timestamp, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const SEEDS: u64 = 32;
const STEPS: usize = 60;

/// How long any one request or read may take, in virtual time.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a reader short of its frames at a quiet point gets to prove it is only late.
const STUCK_AFTER: Duration = Duration::from_secs(120);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// xorshift64, so a seed replays the same ops anywhere.
struct Rng(u64);

impl Rng {
	fn next(&mut self) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0
	}

	fn below(&mut self, n: u64) -> u64 {
		self.next() % n
	}
}

#[derive(Clone, Copy, Debug)]
enum Op {
	/// Append a delta to the open group.
	Write,
	/// Finish the group and open the next with a new snapshot.
	NewGroup,
	/// A subscriber joins.
	Join,
	/// A short reader fetches the latest group, as an HTTP request does.
	Fetch,
	/// A subscriber leaves, picked by this value.
	Leave(u64),
	/// Connect the route, or drop it if another route stays up.
	ToggleA,
	ToggleB,
	ToggleDirect,
	Sleep(Duration),
	/// Settle, then require every subscriber to hold what it should.
	Check,
}

fn generate(seed: u64) -> Vec<Op> {
	let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
	let mut ops = Vec::new();
	for _ in 0..STEPS {
		ops.push(match rng.below(100) {
			0..25 => Op::Write,
			25..30 => Op::NewGroup,
			30..45 => Op::Join,
			45..55 => Op::Fetch,
			55..70 => Op::Leave(rng.next()),
			70..80 => Op::ToggleA,
			80..90 => Op::ToggleB,
			_ => Op::ToggleDirect,
		});
		// Mostly back to back, sometimes past the relays' 30 s idle linger.
		let pause = match rng.below(6) {
			0 | 1 => continue,
			2 => Duration::from_millis(50),
			3 => Duration::from_millis(500),
			4 => Duration::from_secs(5),
			_ => Duration::from_secs(31),
		};
		ops.push(Op::Sleep(pause));
		if pause >= Duration::from_millis(500) {
			ops.push(Op::Check);
		}
	}
	ops.push(Op::Check);
	ops
}

/// How a seed failed: `stuck` (a reader never got what it should have), `late` (only
/// after a quiet point), or `wrong` (it got something it should not have).
struct Failure {
	kind: &'static str,
	detail: String,
}

fn stuck(detail: String) -> Failure {
	Failure { kind: "stuck", detail }
}

fn late(detail: String) -> Failure {
	Failure { kind: "late", detail }
}

fn wrong(detail: String) -> Failure {
	Failure { kind: "wrong", detail }
}

/// A subscriber's frames as `group:payload`, or the error that ended it.
struct Reader {
	id: usize,
	rx: mpsc::UnboundedReceiver<Result<String, Error>>,
	/// Every frame written since it joined, from the snapshot of the group then open.
	expected: Vec<String>,
	seen: Vec<String>,
	/// Owns the subscription, so dropping the reader leaves the track.
	task: moq_clock::sim::JoinHandle<()>,
}

impl Drop for Reader {
	fn drop(&mut self) {
		self.task.abort();
	}
}

impl Reader {
	fn new(id: usize, mut sub: track::Subscriber, expected: Vec<String>) -> Self {
		let (tx, rx) = mpsc::unbounded();
		let task = moq_clock::sim::spawn(async move {
			loop {
				let mut group = match sub.recv_group().await {
					Ok(Some(group)) => group,
					Ok(None) => return,
					Err(err) => {
						let _ = tx.unbounded_send(Err(err));
						return;
					}
				};
				loop {
					let frame = match group.read_frame().await {
						Ok(Some(frame)) => frame,
						Ok(None) => break,
						Err(err) => {
							let _ = tx.unbounded_send(Err(err));
							return;
						}
					};
					let frame = format!("{}:{}", group.sequence, String::from_utf8_lossy(&frame.payload));
					if tx.unbounded_send(Ok(frame)).is_err() {
						return;
					}
				}
			}
		});
		Self {
			id,
			rx,
			expected,
			seen: Vec::new(),
			task,
		}
	}

	/// Take what arrived; `false` once the subscription failed or ended.
	fn drain(&mut self) -> bool {
		loop {
			match self.rx.try_recv() {
				Ok(Ok(frame)) => self.seen.push(frame),
				Ok(Err(_)) => return false,
				Err(mpsc::TryRecvError::Empty) => return true,
				Err(mpsc::TryRecvError::Closed) => return false,
			}
		}
	}

	/// Compare what it saw with what it should see. A joiner may first get groups older
	/// than the newest at its join (`R` had not seen the newer one yet), and may skip
	/// whole groups superseded before it resolved, but each group it gets starts at the
	/// snapshot and continues in order.
	fn judge(&self) -> Verdict {
		let group = |frame: &String| frame.split(':').next().unwrap().parse::<u64>().unwrap();
		let joined = self.expected.first().map(group);
		let mut seen = self.seen.as_slice();
		while let (Some(first), Some(joined)) = (seen.first(), joined) {
			let sequence = group(first);
			if sequence >= joined {
				break;
			}
			if !first.ends_with(":snapshot") {
				return Verdict::Wrong;
			}
			let older = seen.iter().take_while(|frame| group(frame) == sequence).count();
			seen = &seen[older..];
		}
		let mut expected = self.expected.as_slice();
		if let Some(first) = seen.first() {
			let skipped = expected.iter().take_while(|frame| group(frame) < group(first)).count();
			expected = &expected[skipped..];
		}
		match (seen == expected, expected.starts_with(seen)) {
			(true, _) => Verdict::Done,
			(false, true) => Verdict::Short,
			(false, false) => Verdict::Wrong,
		}
	}

	fn report(&self) -> String {
		format!("r{} saw {:?}, expected {:?}", self.id, self.seen, self.expected)
	}
}

#[derive(PartialEq)]
enum Verdict {
	Done,
	Short,
	Wrong,
}

struct Topology {
	version: Version,
	publisher: origin::Producer,
	relay_a: origin::Producer,
	relay_b: origin::Producer,
	subscriber: origin::Producer,
	routes: [Option<MockPair>; 3],
	_upstream: Vec<MockPair>,
}

impl Topology {
	/// Connect route `index` (`A`, `B`, direct), or drop it if another stays up.
	async fn toggle(&mut self, index: usize) {
		if let Some(pair) = self.routes[index].take() {
			match self.routes.iter().any(Option::is_some) {
				true => {
					pair.server.abort(Error::Cancel);
					pair.client.abort(Error::Cancel);
				}
				false => self.routes[index] = Some(pair),
			}
			return;
		}
		let from = [&self.relay_a, &self.relay_b, &self.publisher][index];
		self.routes[index] = Some(link(self.version, from, &self.subscriber).await);
	}

	/// A fresh request for the broadcast; `None` when refused, as with no route right now.
	async fn request(&self, step: usize) -> Result<Option<moq_net::broadcast::Consumer>, Failure> {
		let request = self.subscriber.consume().request("live", None);
		match moq_clock::timeout(READ_TIMEOUT, request).await {
			Ok(result) => Ok(result.ok()),
			Err(_) => Err(stuck(format!("{step}: request hung"))),
		}
	}
}

async fn execute(version: Version, ops: &[Op]) -> Result<(), Failure> {
	let publisher = produce_origin(1);
	let relay_a = produce_origin(2);
	let relay_b = produce_origin(3);
	let subscriber = produce_origin(4);

	let broadcast = publisher.create_broadcast("live").unwrap();
	let info = track::Info::default().with_timescale(moq_net::Timescale::MILLI);
	let track = broadcast.create_track("catalog.json", info).unwrap();
	broadcast
		.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
		.unwrap();

	let upstream = vec![
		link(version, &publisher, &relay_a).await,
		link(version, &publisher, &relay_b).await,
	];
	let a_to_r = link(version, &relay_a, &subscriber).await;
	crate::support::announced(&subscriber.consume(), "live").await;
	let mut topology = Topology {
		version,
		publisher,
		relay_a,
		relay_b,
		subscriber,
		routes: [Some(a_to_r), None, None],
		_upstream: upstream,
	};

	let mut group = track.append_group().unwrap();
	// The open group as written so far, as `group:payload`.
	let mut written: Vec<String> = Vec::new();
	let mut readers: Vec<Reader> = Vec::new();
	let mut joined = 0;
	let mut clock = 0;
	let mut write =
		|group: &mut moq_net::group::Producer, written: &mut Vec<String>, readers: &mut [Reader], payload: &str| {
			clock += 10;
			group
				.write_frame(Timestamp::from_millis(clock).unwrap(), payload.as_bytes())
				.unwrap();
			let frame = format!("{}:{payload}", group.sequence);
			for reader in readers {
				reader.expected.push(frame.clone());
			}
			written.push(frame);
		};
	write(&mut group, &mut written, &mut readers, "snapshot");

	for (step, op) in ops.iter().enumerate() {
		match *op {
			Op::Write => write(&mut group, &mut written, &mut readers, &format!("d{step}")),
			Op::NewGroup => {
				group.finish().unwrap();
				group = track.append_group().unwrap();
				written.clear();
				write(&mut group, &mut written, &mut readers, "snapshot");
				// Let it reach `R`, so the newest group at the next join is unambiguous.
				moq_clock::sleep(Duration::from_millis(500)).await;
			}
			Op::Join => {
				let Some(remote) = topology.request(step).await? else {
					continue;
				};
				let subscribe = remote.track("catalog.json").unwrap().subscribe(None);
				match moq_clock::timeout(READ_TIMEOUT, subscribe).await {
					Ok(Ok(sub)) => readers.push(Reader::new(joined, sub, written.clone())),
					Ok(Err(_)) => {}
					Err(_) => return Err(stuck(format!("{step}: subscribe hung"))),
				}
				joined += 1;
			}
			Op::Fetch => {
				let Some(remote) = topology.request(step).await? else {
					continue;
				};
				let fetch = remote.track("catalog.json").unwrap().fetch_group(group.sequence, None);
				let mut fetched = match moq_clock::timeout(READ_TIMEOUT, fetch).await {
					Ok(Ok(fetched)) => fetched,
					Ok(Err(_)) => continue,
					Err(_) => return Err(stuck(format!("{step}: fetch hung"))),
				};
				let mut got = Vec::new();
				while got.len() < written.len() {
					match moq_clock::timeout(READ_TIMEOUT, fetched.read_frame()).await {
						Ok(Ok(Some(frame))) => got.push(format!(
							"{}:{}",
							fetched.sequence,
							String::from_utf8_lossy(&frame.payload)
						)),
						Ok(Ok(None)) => return Err(wrong(format!("{step}: fetch ended at {got:?}"))),
						Ok(Err(_)) => break,
						Err(_) => return Err(stuck(format!("{step}: fetched {got:?}, then hung"))),
					}
				}
				if !written.starts_with(&got) {
					return Err(wrong(format!("{step}: fetched {got:?}, wrote {written:?}")));
				}
			}
			Op::Leave(pick) => {
				if !readers.is_empty() {
					readers.remove((pick % readers.len() as u64) as usize);
				}
			}
			Op::ToggleA => topology.toggle(0).await,
			Op::ToggleB => topology.toggle(1).await,
			Op::ToggleDirect => topology.toggle(2).await,
			Op::Sleep(pause) => moq_clock::sleep(pause).await,
			Op::Check => {
				moq_clock::sleep(Duration::from_millis(500)).await;
				let mut failed = Vec::new();
				for (index, reader) in readers.iter_mut().enumerate() {
					let open = reader.drain();
					match reader.judge() {
						Verdict::Wrong => return Err(wrong(format!("{step}: {}", reader.report()))),
						// Failed loudly: its client asks again.
						_ if !open => failed.push(index),
						Verdict::Done => {}
						Verdict::Short => {
							let _ = moq_clock::timeout(STUCK_AFTER, async {
								while reader.judge() == Verdict::Short {
									match reader.rx.next().await {
										Some(Ok(frame)) => reader.seen.push(frame),
										_ => break,
									}
								}
							})
							.await;
							return Err(match reader.judge() {
								Verdict::Done => late(format!("{step}: r{}", reader.id)),
								_ => stuck(format!("{step}: {}", reader.report())),
							});
						}
					}
				}
				for index in failed.into_iter().rev() {
					readers.remove(index);
				}
			}
		}
	}
	Ok(())
}

/// Drop ops one at a time while the same kind of failure persists.
async fn shrink(version: Version, mut ops: Vec<Op>, kind: &str) -> Vec<Op> {
	let mut index = 0;
	while index < ops.len() {
		let mut candidate = ops.clone();
		candidate.remove(index);
		match execute(version, &candidate).await {
			Err(failure) if failure.kind == kind => ops = candidate,
			_ => index += 1,
		}
	}
	ops
}

async fn churn(version: &str) {
	let version: Version = version.parse().unwrap();
	let mut failures = Vec::new();
	for seed in 0..SEEDS {
		let ops = generate(seed);
		if let Err(failure) = execute(version, &ops).await {
			let small = shrink(version, ops, failure.kind).await;
			failures.push(format!(
				"seed {seed}: {} {}\n  shrunk to {small:?}",
				failure.kind, failure.detail
			));
		}
	}
	assert!(failures.is_empty(), "{version}:\n{}", failures.join("\n"));
}

/// Routes carry the publisher's epoch, so subscriptions resume across them.
#[moq_clock::test]
async fn catalog_churn_lite_07() {
	churn("moq-lite-07-wip").await;
}

/// No epoch on the wire: a route change ends subscriptions, which must fail loudly.
#[moq_clock::test]
async fn catalog_churn_lite_06() {
	churn("moq-lite-06").await;
}
