//! A seeded userspace UDP impairment relay.
//!
//! The shaper sits between clients and a target and forwards each datagram,
//! both ways, after applying a [`Profile`]: loss, a token-bucket rate limit,
//! delay, jitter, and reordering. QUIC is indifferent to the extra hop, so this
//! impairs a real transport with no capabilities and nothing touching the
//! host's network, on any OS.
//!
//! Every decision comes from [`Config::seed`], so a failing run's seed
//! reproduces the same treatment. Kernel scheduling still varies delivery
//! timing: the seed makes the decisions reproducible, not the clock.
//!
//! A profile that silently did nothing would turn an impaired run into an
//! unimpaired pass, so [`Shaper::verify`] fails when an impairment the profile
//! configures never acted and the traffic makes that silence implausible.

use std::{
	cmp::Reverse,
	collections::{BinaryHeap, HashMap, hash_map},
	fmt,
	net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
	sync::{
		Arc, OnceLock,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use anyhow::Context;
use rand::{RngExt, SeedableRng, rngs::Xoshiro256PlusPlus};
use tokio::{net::UdpSocket, sync::mpsc, task::JoinSet, time::Instant};

/// How one direction of the path treats each datagram.
///
/// A datagram is first subject to `loss`, then waits for the `rate` limit, then
/// takes `delay` plus or minus `jitter` to arrive, unless `reorder` sends it
/// ahead of everything still in flight.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
	/// The base one-way delay.
	pub delay: Duration,
	/// The most a datagram's delay varies from `delay`, uniformly either way.
	pub jitter: Duration,
	/// The probability that a datagram is dropped.
	pub loss: f64,
	/// The probability that a datagram skips the delay, overtaking those in flight.
	pub reorder: f64,
	/// The bottleneck, if any.
	pub rate: Option<Rate>,
}

impl Profile {
	fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			(0.0..=1.0).contains(&self.loss),
			"loss {} is not a probability",
			self.loss
		);
		anyhow::ensure!(
			(0.0..=1.0).contains(&self.reorder),
			"reorder {} is not a probability",
			self.reorder
		);
		anyhow::ensure!(
			self.reorder == 0.0 || !self.delay.is_zero(),
			"reorder {} needs a delay to overtake",
			self.reorder
		);
		anyhow::ensure!(
			self.jitter <= self.delay,
			"jitter {:?} exceeds delay {:?}, which would need a negative delay",
			self.jitter,
			self.delay
		);
		if let Some(rate) = &self.rate {
			anyhow::ensure!(rate.bits_per_second > 0, "a rate limit of zero passes nothing");
		}
		Ok(())
	}
}

/// Each impairment a profile draws for: its name, how likely it is to act on
/// any one datagram (zero when unconfigured), and the count showing it did.
///
/// A rate limit is not here: it acts only on traffic that exceeds it, which no
/// chance predicts, so its counts are reported but never required.
type Impairment = (&'static str, fn(&Profile) -> f64, fn(&Counters) -> u64);
const IMPAIRMENTS: [Impairment; 3] = [
	("loss", |p| p.loss, |c| c.lost),
	("reorder", |p| p.reorder, |c| c.reordered),
	(
		"delay",
		|p| if p.delay.is_zero() { 0.0 } else { 1.0 - p.reorder },
		|c| c.delayed,
	),
];

/// How unlikely an impairment's silence has to be before [`Shaper::verify`]
/// calls it unapplied. A short run can plausibly see no loss at 2%; a long one
/// cannot, and that is when the silence means the profile is not in the path.
const IMPLAUSIBLE: f64 = 1e-4;

/// The impairments `config` configures that `stats` shows implausibly never acted.
fn unapplied(config: &Config, stats: &Stats) -> Vec<&'static str> {
	IMPAIRMENTS
		.iter()
		.filter(|(_, chance, acted)| {
			let silence = (1.0 - chance(&config.up)).powf(stats.up.packets as f64)
				* (1.0 - chance(&config.down)).powf(stats.down.packets as f64);
			acted(&stats.up) + acted(&stats.down) == 0 && silence < IMPLAUSIBLE
		})
		.map(|(name, ..)| *name)
		.collect()
}

/// A token-bucket rate limit with a bounded queue behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rate {
	/// The sustained rate.
	pub bits_per_second: u64,
	/// How many bytes may go out ahead of the sustained rate after an idle spell.
	pub burst: u64,
	/// The longest a datagram waits for the bucket; one that would wait longer is dropped.
	pub queue: Duration,
}

impl Rate {
	/// How long `bytes` takes at this rate.
	fn cost(&self, bytes: u64) -> Duration {
		Duration::from_secs_f64(bytes as f64 * 8.0 / self.bits_per_second as f64)
	}
}

/// Where the shaper listens, where it forwards, and how it treats each direction.
#[derive(Clone, Debug)]
pub struct Config {
	/// The address clients send to. Port 0 picks one; [`Shaper::addr`] reports it.
	pub bind: SocketAddr,
	/// The address every datagram is forwarded to.
	pub target: SocketAddr,
	/// Seeds every treatment decision.
	pub seed: u64,
	/// The treatment from a client toward the target.
	pub up: Profile,
	/// The treatment from the target back toward a client.
	pub down: Profile,
}

/// What one direction did, summed over every client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
	/// Datagrams received.
	pub packets: u64,
	/// Datagrams dropped by `loss`.
	pub lost: u64,
	/// Datagrams dropped because the rate limit's queue was full.
	pub overflowed: u64,
	/// Datagrams that waited for the rate limit.
	pub throttled: u64,
	/// Datagrams given a nonzero delay.
	pub delayed: u64,
	/// Datagrams sent ahead of the delay, overtaking any still in flight.
	pub reordered: u64,
}

impl fmt::Display for Counters {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"{} packets, {} lost, {} overflowed, {} throttled, {} delayed, {} reordered",
			self.packets, self.lost, self.overflowed, self.throttled, self.delayed, self.reordered
		)
	}
}

/// What both directions did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
	/// From clients toward the target.
	pub up: Counters,
	/// From the target back toward clients.
	pub down: Counters,
}

impl fmt::Display for Stats {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "up: {}; down: {}", self.up, self.down)
	}
}

/// A running shaper. Dropping it stops forwarding.
pub struct Shaper {
	addr: SocketAddr,
	config: Config,
	tally: Arc<[Tally; 2]>,
	/// Why forwarding stopped, if it did.
	failed: Arc<OnceLock<String>>,
	task: tokio::task::JoinHandle<()>,
}

impl Shaper {
	/// Bind the listening socket and start forwarding.
	pub async fn bind(config: Config) -> anyhow::Result<Self> {
		config.up.validate().context("invalid up profile")?;
		config.down.validate().context("invalid down profile")?;

		let listen = UdpSocket::bind(config.bind)
			.await
			.with_context(|| format!("bind {}", config.bind))?;
		let addr = listen.local_addr()?;

		let tally = Arc::new([Tally::default(), Tally::default()]);
		let failed = Arc::new(OnceLock::new());
		let task = tokio::spawn({
			let config = config.clone();
			let tally = tally.clone();
			let failed = failed.clone();
			async move {
				if let Err(err) = run(Arc::new(listen), config, tally).await {
					let _ = failed.set(format!("{err:#}"));
				}
			}
		});

		Ok(Self {
			addr,
			config,
			tally,
			failed,
			task,
		})
	}

	/// The address clients send to.
	pub fn addr(&self) -> SocketAddr {
		self.addr
	}

	/// The configuration this shaper runs, including its seed.
	pub fn config(&self) -> &Config {
		&self.config
	}

	/// What the shaper has done so far.
	pub fn stats(&self) -> Stats {
		Stats {
			up: self.tally[UP].snapshot(),
			down: self.tally[DOWN].snapshot(),
		}
	}

	/// Fail unless the shaper is still forwarding and every impairment the
	/// profile configures acted on some datagram, in either direction.
	///
	/// An impairment is only held to that once the traffic makes its silence
	/// implausible: a short run can see no loss, but never no delay.
	pub fn verify(&self) -> anyhow::Result<Stats> {
		let stats = self.stats();
		if let Some(err) = self.failed.get() {
			anyhow::bail!("the shaper stopped forwarding: {err} ({stats})");
		}

		let missing = unapplied(&self.config, &stats);
		anyhow::ensure!(
			missing.is_empty(),
			"the profile never applied {} ({stats}), so this run was not impaired as configured",
			missing.join(", ")
		);
		Ok(stats)
	}
}

impl Drop for Shaper {
	fn drop(&mut self) {
		self.task.abort();
	}
}

const UP: usize = 0;
const DOWN: usize = 1;

#[derive(Default)]
struct Tally {
	packets: AtomicU64,
	lost: AtomicU64,
	overflowed: AtomicU64,
	throttled: AtomicU64,
	delayed: AtomicU64,
	reordered: AtomicU64,
}

impl Tally {
	fn snapshot(&self) -> Counters {
		Counters {
			packets: self.packets.load(Ordering::Relaxed),
			lost: self.lost.load(Ordering::Relaxed),
			overflowed: self.overflowed.load(Ordering::Relaxed),
			throttled: self.throttled.load(Ordering::Relaxed),
			delayed: self.delayed.load(Ordering::Relaxed),
			reordered: self.reordered.load(Ordering::Relaxed),
		}
	}
}

fn bump(counter: &AtomicU64) {
	counter.fetch_add(1, Ordering::Relaxed);
}

/// Accept datagrams from clients, giving each client its own flow.
///
/// A flow is a socket of its own toward the target, so the target sees one
/// address per client just as it would without the shaper in the way.
async fn run(listen: Arc<UdpSocket>, config: Config, tally: Arc<[Tally; 2]>) -> anyhow::Result<()> {
	let mut flows = HashMap::<SocketAddr, Link>::new();
	let mut tasks = JoinSet::new();
	let mut buf = vec![0u8; u16::MAX as usize];

	loop {
		let (size, from) = tokio::select! {
			res = listen.recv_from(&mut buf) => res.context("receive from a client")?,
			Some(res) = tasks.join_next() => {
				res.context("flow task panicked")??;
				continue;
			}
		};
		let now = Instant::now();

		let stream = 2 * flows.len() as u64;
		let link = match flows.entry(from) {
			hash_map::Entry::Occupied(entry) => entry.into_mut(),
			hash_map::Entry::Vacant(entry) => {
				let upstream = Arc::new(bind_toward(config.target).await?);

				let (up_tx, up_rx) = mpsc::unbounded_channel();
				tasks.spawn(deliver(up_rx, upstream.clone(), config.target));

				let (down_tx, down_rx) = mpsc::unbounded_channel();
				tasks.spawn(deliver(down_rx, listen.clone(), from));

				let down = Link::new(&config.down, config.seed, stream + 1, now, down_tx);
				tasks.spawn(reply(upstream, config.target, down, tally.clone()));

				entry.insert(Link::new(&config.up, config.seed, stream, now, up_tx))
			}
		};
		link.push(now, buf[..size].to_vec(), &tally[UP]);
	}
}

/// A socket that can reach `target`: loopback for a loopback target, so the
/// shaper never opens a port beyond the host when it does not need to.
async fn bind_toward(target: SocketAddr) -> anyhow::Result<UdpSocket> {
	let ip = match (target.ip().is_loopback(), target.is_ipv4()) {
		(true, true) => IpAddr::V4(Ipv4Addr::LOCALHOST),
		(true, false) => IpAddr::V6(Ipv6Addr::LOCALHOST),
		(false, true) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
		(false, false) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
	};
	UdpSocket::bind(SocketAddr::new(ip, 0))
		.await
		.with_context(|| format!("bind a socket toward {target}"))
}

/// Feed what the target sends a flow into that flow's return link.
async fn reply(
	socket: Arc<UdpSocket>,
	target: SocketAddr,
	mut link: Link,
	tally: Arc<[Tally; 2]>,
) -> anyhow::Result<()> {
	let mut buf = vec![0u8; u16::MAX as usize];
	loop {
		let (size, from) = socket.recv_from(&mut buf).await.context("receive from the target")?;
		// Anything else reaching this ephemeral port is not part of the path.
		if from != target {
			continue;
		}
		link.push(Instant::now(), buf[..size].to_vec(), &tally[DOWN]);
	}
}

/// Send each treated datagram at its departure time.
async fn deliver(
	mut queue: mpsc::UnboundedReceiver<(Instant, Vec<u8>)>,
	socket: Arc<UdpSocket>,
	dest: SocketAddr,
) -> anyhow::Result<()> {
	// Ordered by departure, then by arrival, so ties keep their order.
	let mut pending = BinaryHeap::<Reverse<(Instant, u64)>>::new();
	let mut datagrams = HashMap::<u64, Vec<u8>>::new();
	let mut sequence = 0u64;

	loop {
		let next = pending.peek().map(|Reverse((at, _))| *at);
		tokio::select! {
			item = queue.recv() => {
				// The link is gone, so the flow is too.
				let Some((at, datagram)) = item else { return Ok(()) };
				pending.push(Reverse((at, sequence)));
				datagrams.insert(sequence, datagram);
				sequence += 1;
			}
			_ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
				let now = Instant::now();
				while let Some(&Reverse((at, id))) = pending.peek() {
					if at > now {
						break;
					}
					pending.pop();
					let datagram = datagrams.remove(&id).expect("queued datagram");
					// A send error is the path losing the datagram, the way a
					// network does when the far end is gone (a killed relay, say),
					// so it is not the shaper failing.
					let _ = socket.send_to(&datagram, dest).await;
				}
			}
		}
	}
}

/// One direction of one flow: the decisions and the state they depend on.
struct Link {
	profile: Profile,
	rng: Xoshiro256PlusPlus,
	/// When the token bucket would be full again, as in GCRA.
	full_at: Instant,
	queue: mpsc::UnboundedSender<(Instant, Vec<u8>)>,
}

impl Link {
	/// Each direction of each flow draws from its own stream of the one seed,
	/// so a flow's decisions do not depend on how its datagrams interleave with
	/// anyone else's. Flows are numbered in the order they first send, since a
	/// client's ephemeral port is no identity across runs.
	fn new(
		profile: &Profile,
		seed: u64,
		stream: u64,
		now: Instant,
		queue: mpsc::UnboundedSender<(Instant, Vec<u8>)>,
	) -> Self {
		Self {
			profile: profile.clone(),
			rng: Xoshiro256PlusPlus::seed_from_u64(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
			// Not the moment the link is built, which is after `now`: that would
			// make the first datagram queue behind a bucket still refilling.
			full_at: now,
			queue,
		}
	}

	fn push(&mut self, now: Instant, datagram: Vec<u8>, tally: &Tally) {
		bump(&tally.packets);

		// Every draw happens for every datagram, whatever the profile, so one
		// knob's outcome never shifts the stream another knob draws from.
		let lose = self.rng.random::<f64>() < self.profile.loss;
		let skip = self.rng.random::<f64>() < self.profile.reorder;
		let spread = self.rng.random::<f64>() * 2.0 - 1.0;

		if lose {
			bump(&tally.lost);
			return;
		}

		let mut depart = now;
		if let Some(rate) = &self.profile.rate {
			// GCRA: a datagram may leave once the backlog, itself included, is
			// within the burst allowance.
			let full_at = self.full_at.max(now) + rate.cost(datagram.len() as u64);
			let start = full_at
				.checked_sub(rate.cost(rate.burst))
				.map_or(now, |start| start.max(now));
			if start - now > rate.queue {
				bump(&tally.overflowed);
				return;
			}
			if start > now {
				bump(&tally.throttled);
			}
			self.full_at = full_at;
			depart = start;
		}

		if skip {
			bump(&tally.reordered);
		} else if !self.profile.delay.is_zero() {
			let jitter = self.profile.jitter.as_secs_f64() * spread;
			depart += Duration::from_secs_f64(self.profile.delay.as_secs_f64() + jitter);
			bump(&tally.delayed);
		}

		// The delivery task only ends once this link is dropped.
		let _ = self.queue.send((depart, datagram));
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const LOCALHOST: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

	/// An echo server, a shaper in front of it, and a client dialing the shaper.
	async fn setup(seed: u64, up: Profile, down: Profile) -> (Shaper, UdpSocket) {
		let echo = UdpSocket::bind(LOCALHOST).await.unwrap();
		let target = echo.local_addr().unwrap();
		tokio::spawn(async move {
			let mut buf = vec![0u8; u16::MAX as usize];
			loop {
				let (size, from) = echo.recv_from(&mut buf).await.unwrap();
				echo.send_to(&buf[..size], from).await.unwrap();
			}
		});

		let shaper = Shaper::bind(Config {
			bind: LOCALHOST,
			target,
			seed,
			up,
			down,
		})
		.await
		.unwrap();

		let client = UdpSocket::bind(LOCALHOST).await.unwrap();
		client.connect(shaper.addr()).await.unwrap();
		(shaper, client)
	}

	/// Send `count` numbered datagrams, then collect whatever echoes back.
	async fn round_trip(client: &UdpSocket, count: u32) -> Vec<u32> {
		for i in 0..count {
			client.send(&i.to_be_bytes()).await.unwrap();
		}
		let mut got = Vec::new();
		let mut buf = [0u8; 4];
		while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(200), client.recv(&mut buf)).await {
			got.push(u32::from_be_bytes(buf));
		}
		got
	}

	#[tokio::test]
	async fn forwards_both_ways_untouched() {
		let (shaper, client) = setup(1, Profile::default(), Profile::default()).await;
		let got = round_trip(&client, 50).await;
		assert_eq!(got, (0..50).collect::<Vec<_>>());

		let stats = shaper.verify().expect("an empty profile has nothing to apply");
		assert_eq!(stats.up.packets, 50);
		assert_eq!(stats.down.packets, 50);
	}

	#[tokio::test]
	async fn the_seed_reproduces_the_losses() {
		let lossy = Profile {
			loss: 0.3,
			..Default::default()
		};

		let (first, client) = setup(7, lossy.clone(), Profile::default()).await;
		let first_got = round_trip(&client, 200).await;
		let (second, client) = setup(7, lossy.clone(), Profile::default()).await;
		let second_got = round_trip(&client, 200).await;
		let (_other, client) = setup(8, lossy, Profile::default()).await;
		let other_got = round_trip(&client, 200).await;

		assert_eq!(first_got, second_got, "the same seed dropped different datagrams");
		assert_ne!(first_got, other_got, "a different seed dropped the same datagrams");
		assert_eq!(first.verify().unwrap(), second.verify().unwrap());
		assert_eq!(first_got.len() as u64, 200 - first.stats().up.lost);
	}

	#[tokio::test]
	async fn reorder_and_jitter_overtake() {
		let shuffled = Profile {
			delay: Duration::from_millis(20),
			jitter: Duration::from_millis(10),
			reorder: 0.2,
			..Default::default()
		};
		let (shaper, client) = setup(3, shuffled, Profile::default()).await;
		let got = round_trip(&client, 100).await;

		let mut sorted = got.clone();
		sorted.sort();
		assert_eq!(sorted, (0..100).collect::<Vec<_>>(), "reordering lost datagrams");
		assert_ne!(got, sorted, "nothing arrived out of order");

		let stats = shaper.verify().unwrap();
		assert!(stats.up.reordered > 0, "{stats}");
	}

	#[tokio::test]
	async fn the_rate_limit_queues_then_drops() {
		// 100 datagrams of 4 bytes is 3200 bits: at 8 kbit/s they need 400ms,
		// and the queue only holds 100ms of it.
		let narrow = Profile {
			rate: Some(Rate {
				bits_per_second: 8_000,
				burst: 8,
				queue: Duration::from_millis(100),
			}),
			..Default::default()
		};
		let (shaper, client) = setup(5, Profile::default(), narrow).await;
		let got = round_trip(&client, 100).await;

		let stats = shaper.verify().unwrap();
		assert!(stats.down.throttled > 0, "{stats}");
		assert!(stats.down.overflowed > 0, "{stats}");
		assert_eq!(got.len() as u64, 100 - stats.down.overflowed);
		// Queued, never reordered: the bottleneck is first in, first out.
		assert!(got.windows(2).all(|pair| pair[0] < pair[1]), "{got:?}");
	}

	#[tokio::test]
	async fn a_rate_limit_wider_than_the_traffic_passes_everything() {
		let wide = Profile {
			rate: Some(Rate {
				bits_per_second: 1_000_000_000_000,
				burst: 1 << 20,
				queue: Duration::ZERO,
			}),
			..Default::default()
		};
		let (shaper, client) = setup(9, wide.clone(), wide).await;
		let got = round_trip(&client, 10).await;

		assert_eq!(got, (0..10).collect::<Vec<_>>());
		assert_eq!(shaper.verify().unwrap().up.overflowed, 0);
	}

	#[tokio::test]
	async fn the_burst_counts_the_datagram_itself() {
		// A burst of one datagram lets the first out at once and holds the second.
		let one = Profile {
			rate: Some(Rate {
				bits_per_second: 8_000,
				burst: 4,
				queue: Duration::from_secs(1),
			}),
			..Default::default()
		};
		let (shaper, client) = setup(11, one, Profile::default()).await;
		let got = round_trip(&client, 2).await;

		assert_eq!(got, [0, 1]);
		assert_eq!(shaper.stats().up.throttled, 1);
	}

	#[test]
	fn silence_is_only_a_failure_once_it_is_implausible() {
		let config = Config {
			bind: LOCALHOST,
			target: LOCALHOST,
			seed: 0,
			up: Profile {
				loss: 0.05,
				..Default::default()
			},
			down: Profile::default(),
		};
		let quiet = |packets| Stats {
			up: Counters {
				packets,
				..Default::default()
			},
			down: Counters {
				packets,
				..Default::default()
			},
		};

		// 0.95^20 is about a third: a short run seeing no loss proves nothing.
		assert!(unapplied(&config, &quiet(20)).is_empty());
		// 0.95^1000 is about 5e-23: the loss is not in the path.
		assert_eq!(unapplied(&config, &quiet(1000)), ["loss"]);

		let mut lossy = quiet(1000);
		lossy.up.lost = 1;
		assert!(unapplied(&config, &lossy).is_empty());

		// A delay acts on every datagram, so even one undelayed datagram is a
		// shaper that is not in the path.
		let config = Config {
			down: Profile {
				delay: Duration::from_millis(10),
				..Default::default()
			},
			..config
		};
		assert_eq!(unapplied(&config, &quiet(1)), ["delay"]);
	}

	#[tokio::test]
	async fn an_invalid_profile_is_refused() {
		let refused = |bad: Profile, why: &'static str| async move {
			let err = Shaper::bind(Config {
				bind: LOCALHOST,
				target: LOCALHOST,
				seed: 0,
				up: bad,
				down: Profile::default(),
			})
			.await
			.err()
			.unwrap_or_else(|| panic!("accepted a profile where {why}"));
			assert!(format!("{err:#}").contains(why), "{err:#}");
		};

		let jittery = Profile {
			delay: Duration::from_millis(5),
			jitter: Duration::from_millis(10),
			..Default::default()
		};
		refused(jittery, "exceeds delay").await;

		// With nothing in flight to overtake, a reorder would count without acting.
		let undelayed = Profile {
			reorder: 0.1,
			..Default::default()
		};
		refused(undelayed, "needs a delay").await;
	}
}
