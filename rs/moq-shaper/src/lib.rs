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
//!
//! A [`Setup`] adds opt-in options to a [`Config`]: a jitter
//! model that keeps the order, one link shared by every client, batches, steps,
//! and named profiles loaded as a [`Preset`]. The README says why each exists.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BinaryHeap, HashMap, hash_map},
	fmt,
	net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
	sync::{
		Arc, Mutex, OnceLock,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

#[cfg(test)]
mod mem;
mod preset;

pub use preset::Preset;

use anyhow::Context;
#[cfg(test)]
use mem::UdpSocket;
use rand::{RngExt, SeedableRng, rngs::Xoshiro256PlusPlus};
use serde::{Deserialize, Serialize};
#[cfg(not(test))]
use tokio::net::UdpSocket;
use tokio::{sync::mpsc, task::JoinSet, time::Instant};

/// How one direction of the path treats each datagram.
///
/// A datagram is first subject to `loss`, then waits for the `rate` limit, then
/// takes `delay` plus or minus `jitter`, drawn as its [`Jitter`] model says, to
/// arrive, unless `reorder` sends it ahead of everything still in flight.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
	/// The base one-way delay.
	pub delay: Duration,
	/// How far a datagram's delay varies from `delay`: the most either way when
	/// [`Jitter::Uniform`], the sigma when [`Jitter::Gaussian`].
	pub jitter: Duration,
	/// The probability that a datagram is dropped.
	pub loss: f64,
	/// The probability that a datagram skips the delay, overtaking those in flight.
	pub reorder: f64,
	/// The bottleneck, if any.
	pub rate: Option<Rate>,
}

impl Profile {
	fn validate(&self, model: Jitter) -> anyhow::Result<()> {
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
		// A gaussian clamps its draw at zero instead, as a queue cannot run early.
		anyhow::ensure!(
			model == Jitter::Gaussian || self.jitter <= self.delay,
			"jitter {:?} exceeds delay {:?}, which would need a negative delay",
			self.jitter,
			self.delay
		);
		// With no delay to vary, `verify` has nothing to hold the jitter to.
		anyhow::ensure!(
			self.jitter.is_zero() || !self.delay.is_zero(),
			"jitter {:?} needs a delay to vary",
			self.jitter
		);
		if let Some(rate) = &self.rate {
			anyhow::ensure!(rate.bits_per_second > 0, "a rate limit of zero passes nothing");
		}
		Ok(())
	}
}

/// How a direction draws each datagram's jitter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Jitter {
	/// Uniform within `jitter` either way of `delay`, drawn for each datagram on
	/// its own, so a later datagram can overtake an earlier one.
	#[default]
	Uniform,
	/// A gaussian with `jitter` as its sigma, clamped at zero, that never leaves
	/// before the datagram in front: it varies the spacing, never the order.
	///
	/// This is queueing delay on a FIFO path. A jitter that overtakes hands QUIC
	/// a gap it can only read as loss, so the run measures its congestion
	/// response rather than the jitter.
	Gaussian,
}

/// Hold datagrams, then release them together, the way a paced hop bunches them.
///
/// A batch closes once `count` datagrams are waiting, or `window` after the
/// first of them arrived, and everything in it leaves when the latest would.
/// That clump is what a receiver's jitter estimate sees on such a path, and no
/// delay or jitter produces it: the datagrams arrive together rather than late.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
	/// How many datagrams close a batch early.
	pub count: usize,
	/// How long a batch waits for that many.
	#[serde(with = "humantime_serde")]
	pub window: Duration,
}

/// A change to one direction's profile once the run reaches `at`.
///
/// A step changes only what it names, so a later step puts one knob back
/// without restating the rest. The rate limit never steps: datagrams already
/// queued behind it keep the old rate's departures, so a new rate would
/// reorder them.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
	/// How far into the run the change happens.
	#[serde(with = "humantime_serde")]
	pub at: Duration,
	/// The delay from then on.
	#[serde(default, with = "humantime_serde")]
	pub delay: Option<Duration>,
	/// The jitter from then on.
	#[serde(default, with = "humantime_serde")]
	pub jitter: Option<Duration>,
	/// The loss from then on.
	#[serde(default)]
	pub loss: Option<f64>,
	/// The reorder from then on.
	#[serde(default)]
	pub reorder: Option<f64>,
}

impl Step {
	/// Change what this step names in `profile`.
	fn apply(&self, profile: &mut Profile) {
		if let Some(delay) = self.delay {
			profile.delay = delay;
		}
		if let Some(jitter) = self.jitter {
			profile.jitter = jitter;
		}
		if let Some(loss) = self.loss {
			profile.loss = loss;
		}
		if let Some(reorder) = self.reorder {
			profile.reorder = reorder;
		}
	}
}

/// One direction's opt-in options beyond its [`Profile`]. The default adds nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
	/// How the profile's `jitter` is drawn.
	pub jitter_model: Jitter,
	/// Hold datagrams and release them together, if at all.
	pub batch: Option<Batch>,
	/// Changes to the profile part-way through the run, earliest first.
	pub steps: Vec<Step>,
}

impl Options {
	/// Check the options, and `profile` as they draw it, after every step too.
	fn validate(&self, profile: &Profile) -> anyhow::Result<()> {
		profile.validate(self.jitter_model)?;
		if let Some(batch) = &self.batch {
			// The datagram that fills a batch leaves at once, so one of one holds nothing.
			anyhow::ensure!(batch.count > 1, "a batch of {} never holds anything", batch.count);
			anyhow::ensure!(!batch.window.is_zero(), "a batch with no window never holds anything");
		}

		let mut profile = profile.clone();
		let mut previous: Option<Duration> = None;
		for step in &self.steps {
			// The profile is what the run opens with, so a step at zero would
			// replace it before any datagram saw it.
			anyhow::ensure!(
				!step.at.is_zero(),
				"a step at zero replaces the profile; change the profile instead"
			);
			// A link only ever looks at the next step due, so one out of order
			// would be skipped without a word.
			anyhow::ensure!(
				previous.is_none_or(|previous| step.at > previous),
				"the step at {:?} comes after the one at {previous:?}; steps go in order",
				step.at
			);
			// A step that leaves the profile as it was thinks it changes and does not.
			let before = profile.clone();
			step.apply(&mut profile);
			anyhow::ensure!(profile != before, "the step at {:?} changes nothing", step.at);
			profile
				.validate(self.jitter_model)
				.with_context(|| format!("after the step at {:?}", step.at))?;
			previous = Some(step.at);
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

/// The impairments that `stats` shows implausibly never acted, given every
/// phase of both directions as the profile in force and the datagrams it treated.
///
/// Each phase is charged only its own traffic, so the clean traffic after a step
/// that turns an impairment off never counts against the phase before it.
fn unapplied(phases: &[(Profile, u64)], stats: &Stats) -> Vec<&'static str> {
	IMPAIRMENTS
		.iter()
		.filter(|(_, chance, acted)| {
			let silence: f64 = phases
				.iter()
				.map(|(profile, packets)| (1.0 - chance(profile)).powf(*packets as f64))
				.product();
			acted(&stats.up) + acted(&stats.down) == 0 && silence < IMPLAUSIBLE
		})
		.map(|(name, ..)| *name)
		.collect()
}

/// `profile` as the run opens and as each of `steps` leaves it, paired with the
/// datagrams `counts` says that phase treated.
fn phased(profile: &Profile, steps: &[Step], counts: &BTreeMap<usize, u64>) -> Vec<(Profile, u64)> {
	let count = |phase: usize| counts.get(&phase).copied().unwrap_or(0);
	let mut profile = profile.clone();
	let mut phases = vec![(profile.clone(), count(0))];
	for (index, step) in steps.iter().enumerate() {
		step.apply(&mut profile);
		phases.push((profile.clone(), count(index + 1)));
	}
	phases
}

/// Whether the batches `setup` configures implausibly never held a datagram.
///
/// A batch holds every datagram but the one that fills it, so it acts on at
/// least `1 - 1 / count` of them. `batched` counts only what a batch held, not
/// what the profile delayed, so a batch that never changed a departure is caught
/// even behind a delay.
fn unbatched(setup: &Setup, stats: &Stats, batched: u64) -> bool {
	let chance = |options: &Options| options.batch.map_or(0.0, |batch| 1.0 - 1.0 / batch.count as f64);
	let silence = (1.0 - chance(&setup.up)).powf(stats.up.packets as f64)
		* (1.0 - chance(&setup.down)).powf(stats.down.packets as f64);
	batched == 0 && silence < IMPLAUSIBLE
}

/// A token-bucket rate limit with a bounded queue behind it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rate {
	/// The sustained rate.
	pub bits_per_second: u64,
	/// How many bytes may go out ahead of the sustained rate after an idle spell.
	pub burst: u64,
	/// The longest a datagram waits for the bucket; one that would wait longer is dropped.
	#[serde(with = "humantime_serde")]
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

/// A [`Config`] plus the opt-in options it has no field for.
///
/// Every option defaults to off, so a setup made from a config shapes exactly
/// as that config does.
#[derive(Clone, Debug)]
pub struct Setup {
	/// Where to listen and forward, the seed, and each direction's profile.
	pub config: Config,
	/// Every client shares one link each way, the way clients behind one access
	/// link do, rather than each getting its own.
	///
	/// Shared, a jitter that keeps the order keeps it across clients, and one
	/// seeded stream and one rate limit serve them all, so a client's treatment
	/// depends on how its datagrams interleave with the others'.
	pub shared: bool,
	/// The options from a client toward the target.
	pub up: Options,
	/// The options from the target back toward a client.
	pub down: Options,
}

impl From<Config> for Setup {
	fn from(config: Config) -> Self {
		Self {
			config,
			shared: false,
			up: Options::default(),
			down: Options::default(),
		}
	}
}

/// What one direction did, summed over every client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counters {
	/// Datagrams received.
	pub packets: u64,
	/// Datagrams dropped by `loss`.
	pub lost: u64,
	/// Datagrams dropped because the rate limit's queue was full.
	pub overflowed: u64,
	/// Datagrams that waited for the rate limit.
	pub throttled: u64,
	/// Datagrams given a nonzero delay, by the profile or by a batch holding them.
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
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
	setup: Setup,
	tally: Arc<[Tally; 2]>,
	/// Why forwarding stopped, if it did.
	failed: Arc<OnceLock<String>>,
	/// How many [`Outage`]s are holding the path down.
	cuts: Arc<AtomicU64>,
	task: tokio::task::JoinHandle<()>,
}

impl Shaper {
	/// Bind the listening socket and start forwarding.
	pub async fn bind(setup: impl Into<Setup>) -> anyhow::Result<Self> {
		let setup = setup.into();
		let config = &setup.config;
		setup.up.validate(&config.up).context("invalid up profile")?;
		setup.down.validate(&config.down).context("invalid down profile")?;

		let listen = UdpSocket::bind(config.bind)
			.await
			.with_context(|| format!("bind {}", config.bind))?;
		let addr = listen.local_addr()?;

		let tally = Arc::new([Tally::default(), Tally::default()]);
		let failed = Arc::new(OnceLock::new());
		let cuts = Arc::new(AtomicU64::new(0));
		let task = tokio::spawn({
			let setup = setup.clone();
			let tally = tally.clone();
			let failed = failed.clone();
			let cuts = cuts.clone();
			async move {
				if let Err(err) = run(Arc::new(listen), setup, tally, cuts).await {
					let _ = failed.set(format!("{err:#}"));
				}
			}
		});

		Ok(Self {
			addr,
			setup,
			tally,
			failed,
			cuts,
			task,
		})
	}

	/// Take the path down both ways until the returned [`Outage`] drops.
	///
	/// Every datagram is dropped untreated meanwhile, the way a severed link
	/// loses it, so it counts toward no impairment. Datagrams already treated
	/// still leave on time; only what arrives during the outage is lost.
	pub fn cut(&self) -> Outage {
		self.cuts.fetch_add(1, Ordering::Relaxed);
		Outage {
			cuts: self.cuts.clone(),
		}
	}

	/// The address clients send to.
	pub fn addr(&self) -> SocketAddr {
		self.addr
	}

	/// The configuration this shaper runs, including its seed.
	pub fn config(&self) -> &Config {
		&self.setup.config
	}

	/// What the shaper has done so far.
	pub fn stats(&self) -> Stats {
		Stats {
			up: self.tally[UP].snapshot(),
			down: self.tally[DOWN].snapshot(),
		}
	}

	/// Fail unless the shaper is still forwarding, every impairment the profile
	/// configures acted on some datagram, and some datagram saw every phase of
	/// a direction with steps, before the first and after each.
	///
	/// An impairment is only held to that once the traffic makes its silence
	/// implausible: a short run can see no loss, but never no delay. The phases
	/// are held to it once their direction carried anything: traffic that all
	/// came on one side of a step never saw the path change.
	pub fn verify(&self) -> anyhow::Result<Stats> {
		let stats = self.stats();
		if let Some(err) = self.failed.get() {
			anyhow::bail!("the shaper stopped forwarding: {err} ({stats})");
		}

		let counts = [UP, DOWN].map(|dir| self.tally[dir].phases.lock().unwrap().clone());
		let phases: Vec<(Profile, u64)> = [
			(&self.setup.config.up, &self.setup.up, &counts[UP]),
			(&self.setup.config.down, &self.setup.down, &counts[DOWN]),
		]
		.into_iter()
		.flat_map(|(profile, options, counts)| phased(profile, &options.steps, counts))
		.collect();
		let mut missing: Vec<String> = unapplied(&phases, &stats).into_iter().map(String::from).collect();
		let batched = self
			.tally
			.iter()
			.map(|tally| tally.batched.load(Ordering::Relaxed))
			.sum();
		if unbatched(&self.setup, &stats, batched) {
			missing.push("batch".to_string());
		}
		for (name, options, phases, counters) in [
			("up", &self.setup.up, &counts[UP], &stats.up),
			("down", &self.setup.down, &counts[DOWN], &stats.down),
		] {
			let Some(first) = options.steps.first().filter(|_| counters.packets > 0) else {
				continue;
			};
			if !phases.contains_key(&0) {
				missing.push(format!("the {name} profile before its step at {:?}", first.at));
			}
			for (index, step) in options.steps.iter().enumerate() {
				if !phases.contains_key(&(index + 1)) {
					missing.push(format!("the {name} step at {:?}", step.at));
				}
			}
		}
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

/// A path taken down by [`Shaper::cut`]; dropping it restores the path.
#[must_use = "dropping the outage restores the path at once"]
pub struct Outage {
	cuts: Arc<AtomicU64>,
}

impl Drop for Outage {
	fn drop(&mut self) {
		self.cuts.fetch_sub(1, Ordering::Relaxed);
	}
}

/// Whether an [`Outage`] is holding the path down.
fn is_cut(cuts: &AtomicU64) -> bool {
	cuts.load(Ordering::Relaxed) > 0
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
	/// Datagrams treated in each phase: 0 before the first step, n after the nth.
	phases: Mutex<BTreeMap<usize, u64>>,
	/// Datagrams a batch held past when they would otherwise have left.
	batched: AtomicU64,
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
/// address per client just as it would without the shaper in the way. Each
/// flow takes a link of its own each way, unless the path is shared.
async fn run(listen: Arc<UdpSocket>, setup: Setup, tally: Arc<[Tally; 2]>, cuts: Arc<AtomicU64>) -> anyhow::Result<()> {
	let config = &setup.config;
	let mut flows = HashMap::<SocketAddr, Flow>::new();
	let mut tasks = JoinSet::new();
	let mut buf = vec![0u8; u16::MAX as usize];

	// What a step's `at` counts from.
	let start = Instant::now();

	// A shared path is one link each way, drawing the streams the first flow would.
	let shared = setup
		.shared
		.then(|| [UP, DOWN].map(|direction| link(&mut tasks, &setup, direction, 0, start, &tally)));

	loop {
		let (size, from) = tokio::select! {
			res = listen.recv_from(&mut buf) => res.context("receive from a client")?,
			Some(res) = tasks.join_next() => {
				res.context("flow task panicked")??;
				continue;
			}

		};
		if is_cut(&cuts) {
			continue;
		}
		let now = Instant::now();

		let number = flows.len() as u64;
		let flow = match flows.entry(from) {
			hash_map::Entry::Occupied(entry) => entry.into_mut(),
			hash_map::Entry::Vacant(entry) => {
				let upstream = Arc::new(bind_toward(config.target).await?);

				let [up, down] = match &shared {
					Some(links) => links.clone(),
					None => [UP, DOWN].map(|direction| link(&mut tasks, &setup, direction, number, start, &tally)),
				};
				tasks.spawn(reply(
					upstream.clone(),
					config.target,
					down,
					listen.clone(),
					from,
					tally.clone(),
					cuts.clone(),
				));

				entry.insert(Flow { upstream, up })
			}
		};
		let datagram = buf[..size].to_vec();
		let mut up = flow.up.lock().expect("link poisoned");
		up.push(now, datagram, &flow.upstream, config.target, &tally[UP]);
	}
}

/// One client: its socket toward the target, and the link it sends through.
struct Flow {
	upstream: Arc<UdpSocket>,
	up: Arc<Mutex<Link>>,
}

/// One direction of the flow numbered `flow` as a link, and the task delivering
/// what it treats.
fn link(
	tasks: &mut JoinSet<anyhow::Result<()>>,
	setup: &Setup,
	direction: usize,
	flow: u64,
	start: Instant,
	tally: &Arc<[Tally; 2]>,
) -> Arc<Mutex<Link>> {
	let (profile, options) = match direction {
		UP => (&setup.config.up, &setup.up),
		_ => (&setup.config.down, &setup.down),
	};
	let (queue, queued) = mpsc::unbounded_channel();
	tasks.spawn(deliver(queued, options.batch, tally.clone(), direction));
	let stream = 2 * flow + direction as u64;
	Arc::new(Mutex::new(Link::new(
		profile,
		options,
		setup.config.seed,
		stream,
		start,
		queue,
	)))
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

/// Feed what the target sends a flow into the link back toward its client.
async fn reply(
	socket: Arc<UdpSocket>,
	target: SocketAddr,
	link: Arc<Mutex<Link>>,
	listen: Arc<UdpSocket>,
	client: SocketAddr,
	tally: Arc<[Tally; 2]>,
	cuts: Arc<AtomicU64>,
) -> anyhow::Result<()> {
	let mut buf = vec![0u8; u16::MAX as usize];
	loop {
		let (size, from) = socket.recv_from(&mut buf).await.context("receive from the target")?;
		// Anything else reaching this ephemeral port is not part of the path.
		if from != target || is_cut(&cuts) {
			continue;
		}
		let datagram = buf[..size].to_vec();
		let mut down = link.lock().expect("link poisoned");
		down.push(Instant::now(), datagram, &listen, client, &tally[DOWN]);
	}
}

/// Send each treated datagram at its departure time, after its batch if the
/// link has one.
async fn deliver(
	mut queue: mpsc::UnboundedReceiver<Parcel>,
	batch: Option<Batch>,
	tally: Arc<[Tally; 2]>,
	direction: usize,
) -> anyhow::Result<()> {
	// Ordered by departure, then by arrival, so ties keep their order.
	let mut pending = BinaryHeap::<Reverse<(Instant, u64)>>::new();
	let mut parcels = HashMap::<u64, Parcel>::new();
	let mut sequence = 0u64;
	let mut held = Held::default();
	let mut ready = Vec::new();

	loop {
		let next = pending.peek().map(|Reverse((at, _))| *at);
		let wake = next.into_iter().chain(held.closes).min();
		tokio::select! {
			item = queue.recv() => {
				// The link is gone, so the flow is too.
				let Some(parcel) = item else { return Ok(()) };
				match batch {
					Some(batch) => held.hold(batch, parcel, &mut ready, &tally[direction]),
					None => ready.push(parcel),
				}
			}
			_ = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {
				// The window closes a batch that never filled.
				if let Some(closes) = held.closes.filter(|&closes| closes <= Instant::now()) {
					held.release(closes, &mut ready, &tally[direction]);
				}
			}
		}
		for parcel in ready.drain(..) {
			pending.push(Reverse((parcel.at, sequence)));
			parcels.insert(sequence, parcel);
			sequence += 1;
		}

		// Whatever is due leaves now. The timer rounds up to the next
		// millisecond, which would delay even a datagram given no delay.
		let now = Instant::now();
		while let Some(&Reverse((at, id))) = pending.peek() {
			if at > now {
				break;
			}
			pending.pop();
			let parcel = parcels.remove(&id).expect("queued datagram");
			// A send error is the path losing the datagram, the way a network
			// does when the far end is gone (a killed relay, say), so it is not
			// the shaper failing.
			let _ = parcel.socket.send_to(&parcel.datagram, parcel.dest).await;
		}
	}
}

/// The datagrams a batch is holding, and when its window closes on them.
#[derive(Default)]
struct Held {
	parcels: Vec<Parcel>,
	closes: Option<Instant>,
}

impl Held {
	/// Hold `parcel`, releasing the batch into `ready` once it is full.
	fn hold(&mut self, batch: Batch, parcel: Parcel, ready: &mut Vec<Parcel>, tally: &Tally) {
		let arrived = parcel.arrived;
		// The window may have closed before its timer fired; the batch it closed
		// leaves without this datagram, which starts the next one.
		if let Some(closes) = self.closes.filter(|&closes| closes <= arrived) {
			self.release(closes, ready, tally);
		}
		self.closes.get_or_insert(arrived + batch.window);
		self.parcels.push(parcel);
		if self.parcels.len() >= batch.count {
			self.release(arrived, ready, tally);
		}
	}

	/// Release the batch, closed at `closed`, into `ready`: every datagram leaves
	/// when the latest would, and none before the batch closed.
	fn release(&mut self, closed: Instant, ready: &mut Vec<Parcel>, tally: &Tally) {
		self.closes = None;
		let Some(latest) = self.parcels.iter().map(|parcel| parcel.at).max() else {
			return;
		};
		let at = latest.max(closed);
		for mut parcel in self.parcels.drain(..) {
			if at > parcel.at {
				bump(&tally.batched);
				// A hold is a delay, counted once whichever stage gave it.
				if !parcel.delayed {
					bump(&tally.delayed);
				}
			}
			parcel.at = at;
			ready.push(parcel);
		}
	}
}

/// A treated datagram: when it arrived and leaves, and the socket and address
/// it leaves by.
struct Parcel {
	arrived: Instant,
	at: Instant,
	/// Whether the link already counted it as delayed.
	delayed: bool,
	datagram: Vec<u8>,
	socket: Arc<UdpSocket>,
	dest: SocketAddr,
}

/// One direction of one flow, or of every flow on a shared path: the decisions
/// and the state they depend on.
struct Link {
	profile: Profile,
	jitter: Jitter,
	rng: Xoshiro256PlusPlus,
	/// When the token bucket would be full again, as in GCRA.
	full_at: Instant,
	/// When the latest in-order datagram leaves, which the next one never beats.
	floor: Instant,
	/// When the run started, which a step's `at` counts from.
	start: Instant,
	/// Every step, earliest first.
	steps: Vec<Step>,
	/// How many of `steps` have applied.
	stepped: usize,
	queue: mpsc::UnboundedSender<Parcel>,
}

impl Link {
	/// Each link draws from its own stream of the one seed, so a flow's
	/// decisions do not depend on how its datagrams interleave with anyone
	/// else's, unless the path is shared. Flows are numbered in the order they
	/// first send, since a client's ephemeral port is no identity across runs.
	fn new(
		profile: &Profile,
		options: &Options,
		seed: u64,
		stream: u64,
		start: Instant,
		queue: mpsc::UnboundedSender<Parcel>,
	) -> Self {
		Self {
			profile: profile.clone(),
			jitter: options.jitter_model,
			rng: Xoshiro256PlusPlus::seed_from_u64(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
			// Full since the run started, never since the link was built, which
			// is after its first datagram arrived: that would make the datagram
			// queue behind a bucket still refilling.
			full_at: start,
			floor: start,
			start,
			steps: options.steps.clone(),
			stepped: 0,
			queue,
		}
	}

	/// Apply every step the run has reached by `now`, in order, and record the
	/// phase a datagram arriving then sees.
	fn step(&mut self, now: Instant, tally: &Tally) {
		while let Some(step) = self.steps.get(self.stepped) {
			// A step past what the clock can hold is one the run never reaches.
			if self.start.checked_add(step.at).is_none_or(|at| at > now) {
				break;
			}
			step.apply(&mut self.profile);
			self.stepped += 1;
		}
		// Applying a step is no evidence it acted, so count the datagrams each phase treated.
		*tally.phases.lock().unwrap().entry(self.stepped).or_default() += 1;
	}

	/// Treat a datagram and queue it to leave by `socket` for `dest`.
	fn push(&mut self, now: Instant, datagram: Vec<u8>, socket: &Arc<UdpSocket>, dest: SocketAddr, tally: &Tally) {
		let Some((at, delayed)) = self.treat(now, datagram.len(), tally) else {
			return;
		};
		// The delivery task only ends once this link is dropped.
		let _ = self.queue.send(Parcel {
			arrived: now,
			at,
			delayed,
			datagram,
			socket: socket.clone(),
			dest,
		});
	}

	/// When a datagram of `size` bytes arriving `now` leaves, and whether it was
	/// counted as delayed, or `None` when it is dropped.
	fn treat(&mut self, now: Instant, size: usize, tally: &Tally) -> Option<(Instant, bool)> {
		self.step(now, tally);
		bump(&tally.packets);

		// Every draw happens for every datagram, whatever the profile, so one
		// knob's outcome never shifts the stream another knob draws from.
		let lose = self.rng.random::<f64>() < self.profile.loss;
		let skip = self.rng.random::<f64>() < self.profile.reorder;
		let spread = match self.jitter {
			Jitter::Uniform => self.rng.random::<f64>() * 2.0 - 1.0,
			Jitter::Gaussian => gaussian(&mut self.rng),
		};

		if lose {
			bump(&tally.lost);
			return None;
		}

		let mut depart = now;
		if let Some(rate) = &self.profile.rate {
			// GCRA: a datagram may leave once the backlog, itself included, is
			// within the burst allowance.
			let full_at = self.full_at.max(now) + rate.cost(size as u64);
			let start = full_at
				.checked_sub(rate.cost(rate.burst))
				.map_or(now, |start| start.max(now));
			if start - now > rate.queue {
				bump(&tally.overflowed);
				return None;
			}
			if start > now {
				bump(&tally.throttled);
			}
			self.full_at = full_at;
			depart = start;
		}

		let mut delayed = false;
		if skip {
			bump(&tally.reordered);
		} else if self.jitter == Jitter::Gaussian {
			// A queue: nothing leaves before the datagram in front of it.
			let wait = self.profile.delay.as_secs_f64() + self.profile.jitter.as_secs_f64() * spread;
			let leave = (depart + Duration::from_secs_f64(wait.max(0.0))).max(self.floor);
			// Counted like the uniform model whenever a delay is configured, so
			// the chance `verify` holds it to is the same.
			delayed = !self.profile.delay.is_zero() || leave > depart;
			self.floor = leave;
			depart = leave;
		} else if !self.profile.delay.is_zero() {
			let jitter = self.profile.jitter.as_secs_f64() * spread;
			depart += Duration::from_secs_f64(self.profile.delay.as_secs_f64() + jitter);
			delayed = true;
		}
		if delayed {
			bump(&tally.delayed);
		}

		Some((depart, delayed))
	}
}

/// A standard normal sample by Box-Muller, which always takes exactly two draws,
/// so the jitter never shifts the stream the next datagram's knobs draw from.
fn gaussian(rng: &mut Xoshiro256PlusPlus) -> f64 {
	let u1 = rng.random::<f64>().max(f64::MIN_POSITIVE);
	let u2 = rng.random::<f64>();
	(-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
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

	#[tokio::test(start_paused = true)]
	async fn forwards_both_ways_untouched() {
		let (shaper, client) = setup(1, Profile::default(), Profile::default()).await;
		let got = round_trip(&client, 50).await;
		assert_eq!(got, (0..50).collect::<Vec<_>>());

		let stats = shaper.verify().expect("an empty profile has nothing to apply");
		assert_eq!(stats.up.packets, 50);
		assert_eq!(stats.down.packets, 50);
	}

	#[tokio::test(start_paused = true)]
	async fn a_cut_path_drops_both_ways_until_restored() {
		// The delay holds the datagram already treated on its way up, so the
		// echo comes back while the path is cut.
		let delayed = Profile {
			delay: Duration::from_millis(10),
			..Default::default()
		};
		let (shaper, client) = setup(1, delayed, Profile::default()).await;
		client.send(b"held").await.unwrap();
		while shaper.stats().up.packets == 0 {
			tokio::time::sleep(Duration::from_millis(1)).await;
		}

		let outage = shaper.cut();
		let got = round_trip(&client, 10).await;
		assert!(got.is_empty(), "a cut path delivered {got:?}");
		let stats = shaper.stats();
		assert_eq!(stats.up.packets, 1, "a cut path treated what arrived up: {stats}");
		assert_eq!(stats.down.packets, 0, "a cut path treated the echo: {stats}");

		drop(outage);
		let got = round_trip(&client, 10).await;
		assert_eq!(got, (0..10).collect::<Vec<_>>(), "the restored path lost datagrams");
	}

	#[tokio::test(start_paused = true)]
	async fn an_undelayed_datagram_waits_for_no_timer() {
		let (_shaper, client) = setup(1, Profile::default(), Profile::default()).await;

		// Off a millisecond boundary, where a timer would round up to the next one.
		tokio::time::advance(Duration::from_micros(500)).await;
		let start = Instant::now();
		client.send(b"ping").await.unwrap();
		let mut buf = [0u8; 4];
		client.recv(&mut buf).await.unwrap();

		// A paused clock only moves when every task waits on a timer, so a round
		// trip that took any time slept on one.
		assert_eq!(start.elapsed(), Duration::ZERO);
	}

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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
		let lossy = Profile {
			loss: 0.05,
			..Default::default()
		};
		let both = |packets| [(lossy.clone(), packets), (Profile::default(), packets)];
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
		assert!(unapplied(&both(20), &quiet(20)).is_empty());
		// 0.95^1000 is about 5e-23: the loss is not in the path.
		assert_eq!(unapplied(&both(1000), &quiet(1000)), ["loss"]);

		let mut lost = quiet(1000);
		lost.up.lost = 1;
		assert!(unapplied(&both(1000), &lost).is_empty());

		// A delay acts on every datagram, so even one undelayed datagram is a
		// shaper that is not in the path.
		let delayed = Profile {
			delay: Duration::from_millis(10),
			..Default::default()
		};
		assert_eq!(unapplied(&[(lossy, 1), (delayed, 1)], &quiet(1)), ["delay"]);
	}

	#[test]
	fn a_phase_is_charged_only_its_own_traffic() {
		let ms = Duration::from_millis;
		let lossy = Profile {
			loss: 0.02,
			..Default::default()
		};
		let steps = [Step {
			at: ms(100),
			loss: Some(0.0),
			..Default::default()
		}];
		// One datagram survived the 2% loss, then the loss stepped off for a thousand.
		let phases = phased(&lossy, &steps, &BTreeMap::from([(0, 1), (1, 1000)]));
		let stats = Stats {
			up: Counters {
				packets: 1001,
				..Default::default()
			},
			down: Counters::default(),
		};
		assert!(unapplied(&phases, &stats).is_empty());

		// The same thousand under the loss still fail.
		let phases = phased(&lossy, &steps, &BTreeMap::from([(0, 1000), (1, 1)]));
		assert_eq!(unapplied(&phases, &stats), ["loss"]);
	}

	#[tokio::test(start_paused = true)]
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

	/// An echo server, a shaper `setup` builds from a config aimed at it, and a
	/// client dialing the shaper.
	async fn shaped(setup: impl FnOnce(Config) -> Setup) -> (Shaper, UdpSocket) {
		let echo = UdpSocket::bind(LOCALHOST).await.unwrap();
		let target = echo.local_addr().unwrap();
		tokio::spawn(async move {
			let mut buf = vec![0u8; u16::MAX as usize];
			loop {
				let (size, from) = echo.recv_from(&mut buf).await.unwrap();
				echo.send_to(&buf[..size], from).await.unwrap();
			}
		});

		let config = Config {
			bind: LOCALHOST,
			target,
			seed: 3,
			up: Profile::default(),
			down: Profile::default(),
		};
		let shaper = Shaper::bind(setup(config)).await.unwrap();

		let client = UdpSocket::bind(LOCALHOST).await.unwrap();
		client.connect(shaper.addr()).await.unwrap();
		(shaper, client)
	}

	/// Push `count` numbered datagrams through one link, `spacing` apart, and
	/// return each id with its departure, in the order they would be sent.
	fn departures(
		profile: &Profile,
		options: &Options,
		count: u32,
		spacing: Duration,
	) -> (Vec<(u32, Instant)>, Counters) {
		let (queue, _) = mpsc::unbounded_channel();
		let now = Instant::now();
		let mut link = Link::new(profile, options, 7, 0, now, queue);
		let tally = Tally::default();
		let mut sent: Vec<(u32, Instant)> = (0..count)
			.filter_map(|id| Some((id, link.treat(now + spacing * id, 4, &tally)?.0)))
			.collect();
		// Ties leave in arrival order, which is id order here.
		sent.sort_by_key(|&(id, at)| (at, id));
		(sent, tally.snapshot())
	}

	const GAUSSIAN: Options = Options {
		jitter_model: Jitter::Gaussian,
		batch: None,
		steps: Vec::new(),
	};

	#[tokio::test(start_paused = true)]
	async fn only_the_gaussian_model_keeps_the_order() {
		let jittery = Profile {
			delay: Duration::from_millis(20),
			jitter: Duration::from_millis(10),
			..Default::default()
		};

		let (_uniform, client) = shaped(|config| {
			Config {
				up: jittery.clone(),
				..config
			}
			.into()
		})
		.await;
		let got = round_trip(&client, 100).await;
		let mut sorted = got.clone();
		sorted.sort();
		assert_eq!(sorted, (0..100).collect::<Vec<_>>(), "jitter lost datagrams");
		assert_ne!(got, sorted, "uniform jitter never overtook, so this proves nothing");

		let (gaussian, client) = shaped(|config| Setup {
			up: GAUSSIAN,
			..Config { up: jittery, ..config }.into()
		})
		.await;
		let got = round_trip(&client, 100).await;
		assert_eq!(got, (0..100).collect::<Vec<_>>(), "gaussian jitter reordered");

		let stats = gaussian.verify().unwrap();
		assert_eq!(stats.up.reordered, 0, "{stats}");
		assert_eq!(stats.up.delayed, 100, "{stats}");
	}

	#[test]
	fn gaussian_jitter_never_leaves_before_it_arrived() {
		// A sigma far past the delay, so most draws would be negative unclamped.
		let wide = Profile {
			delay: Duration::from_millis(1),
			jitter: Duration::from_millis(50),
			..Default::default()
		};
		let now = Instant::now();
		let (sent, counters) = departures(&wide, &GAUSSIAN, 1000, Duration::ZERO);
		assert_eq!(sent.len(), 1000);
		assert!(sent.iter().all(|&(_, at)| at >= now));
		assert!(counters.delayed > 0, "{counters}");
	}

	#[test]
	fn gaussian_jitter_alone_never_changes_the_order() {
		// Ten times the arrival spacing as sigma, so an independent draw per
		// datagram would shuffle nearly all of them.
		let jittery = Profile {
			delay: Duration::from_millis(5),
			jitter: Duration::from_millis(50),
			..Default::default()
		};
		let (sent, counters) = departures(&jittery, &GAUSSIAN, 2000, Duration::from_millis(5));
		let ids: Vec<u32> = sent.iter().map(|&(id, _)| id).collect();
		assert_eq!(ids, (0..2000).collect::<Vec<_>>(), "gaussian jitter reordered");
		assert_eq!(counters.reordered, 0);
	}

	#[test]
	fn gaussian_jitter_still_varies_the_spacing() {
		let spacing = Duration::from_millis(20);
		let jittery = Profile {
			delay: Duration::from_millis(5),
			jitter: Duration::from_millis(5),
			..Default::default()
		};
		let (sent, _) = departures(&jittery, &GAUSSIAN, 2000, spacing);

		// Keeping the order is not pacing the path: a datagram that drew more
		// than the one in front falls further behind, and one that drew less
		// closes up against it.
		let gaps: Vec<Duration> = sent.windows(2).map(|pair| pair[1].1 - pair[0].1).collect();
		assert!(gaps.iter().any(|&gap| gap < spacing), "nothing closed up");
		assert!(gaps.iter().any(|&gap| gap > spacing), "nothing fell behind");
	}

	#[test]
	fn a_reorder_still_overtakes_gaussian_jitter() {
		let shuffled = Profile {
			delay: Duration::from_millis(5),
			jitter: Duration::from_millis(50),
			reorder: 0.05,
			..Default::default()
		};
		let (sent, counters) = departures(&shuffled, &GAUSSIAN, 2000, Duration::from_millis(5));
		let overtaken = sent.windows(2).filter(|pair| pair[0].0 > pair[1].0).count();
		assert!(overtaken > 0, "a reorder never overtook");
		assert!(counters.reordered > 0, "{counters}");
	}

	#[test]
	fn the_seed_reproduces_the_gaussian_decisions() {
		let everything = Profile {
			delay: Duration::from_millis(5),
			jitter: Duration::from_millis(5),
			loss: 0.05,
			reorder: 0.05,
			..Default::default()
		};
		let (first, first_counters) = departures(&everything, &GAUSSIAN, 1000, Duration::from_millis(1));
		let (second, second_counters) = departures(&everything, &GAUSSIAN, 1000, Duration::from_millis(1));

		// The same ids survive and leave at the same offsets from their start.
		let offsets = |sent: &[(u32, Instant)]| {
			let start = sent.iter().map(|&(_, at)| at).min().unwrap();
			sent.iter().map(|&(id, at)| (id, at - start)).collect::<Vec<_>>()
		};
		assert_eq!(offsets(&first), offsets(&second));
		assert_eq!(first_counters, second_counters);
		assert!(first_counters.lost > 0 && first_counters.reordered > 0);
	}

	#[tokio::test(start_paused = true)]
	async fn a_gaussian_sigma_may_exceed_the_delay() {
		// The uniform model refuses this; a gaussian clamps its draw at zero.
		let wide = Profile {
			delay: Duration::from_millis(5),
			jitter: Duration::from_millis(10),
			..Default::default()
		};
		let (shaper, client) = shaped(|config| Setup {
			up: GAUSSIAN,
			..Config { up: wide, ..config }.into()
		})
		.await;
		assert_eq!(round_trip(&client, 20).await, (0..20).collect::<Vec<_>>());
		shaper.verify().unwrap();

		// With no delay, `verify` could never tell the jitter acted.
		let undelayed = Profile {
			jitter: Duration::from_millis(5),
			..Default::default()
		};
		let err = Shaper::bind(Setup {
			up: GAUSSIAN,
			..Config {
				bind: LOCALHOST,
				target: LOCALHOST,
				seed: 0,
				up: undelayed,
				down: Profile::default(),
			}
			.into()
		})
		.await
		.err()
		.expect("accepted gaussian jitter with no delay");
		assert!(format!("{err:#}").contains("needs a delay to vary"), "{err:#}");
	}

	/// Two clients take turns sending `count` numbered datagrams up the path
	/// `setup` builds, and what reaches the target comes back, in order, with
	/// how long after the first send it did.
	async fn two_clients(setup: impl FnOnce(Config) -> Setup, count: u32) -> Vec<(u32, Duration)> {
		let target = UdpSocket::bind(LOCALHOST).await.unwrap();
		let config = Config {
			bind: LOCALHOST,
			target: target.local_addr().unwrap(),
			seed: 5,
			up: Profile::default(),
			down: Profile::default(),
		};
		let shaper = Shaper::bind(setup(config)).await.unwrap();

		let clients = [
			UdpSocket::bind(LOCALHOST).await.unwrap(),
			UdpSocket::bind(LOCALHOST).await.unwrap(),
		];
		let start = Instant::now();
		for id in 0..count {
			let client = &clients[id as usize % 2];
			client.send_to(&id.to_be_bytes(), shaper.addr()).await.unwrap();
		}

		let mut got = Vec::new();
		let mut buf = [0u8; 4];
		while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(500), target.recv_from(&mut buf)).await {
			got.push((u32::from_be_bytes(buf), start.elapsed()));
		}
		got
	}

	#[tokio::test(start_paused = true)]
	async fn a_shared_path_keeps_the_order_across_clients() {
		let jittery = Profile {
			delay: Duration::from_millis(20),
			jitter: Duration::from_millis(10),
			..Default::default()
		};
		let order = |shared: bool| {
			let up = jittery.clone();
			async move {
				let got = two_clients(
					|config| Setup {
						shared,
						up: GAUSSIAN,
						..Config { up, ..config }.into()
					},
					200,
				)
				.await;
				got.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
			}
		};

		// Separate links keep each client's order, but not the order between them.
		let apart = order(false).await;
		let mut sorted = apart.clone();
		sorted.sort();
		assert_eq!(sorted, (0..200).collect::<Vec<_>>(), "a client lost datagrams");
		assert_ne!(
			apart, sorted,
			"separate links kept the order anyway, so this proves nothing"
		);
		for parity in 0..2 {
			assert!(apart.iter().filter(|&&id| id % 2 == parity).is_sorted());
		}

		assert_eq!(
			order(true).await,
			(0..200).collect::<Vec<_>>(),
			"a shared path reordered"
		);
	}

	/// Send `count` numbered datagrams `spacing` apart, and collect what echoes
	/// back with when it did, alongside when each was sent.
	async fn paced(client: &UdpSocket, count: u32, spacing: Duration) -> (Vec<Instant>, Vec<(u32, Instant)>) {
		let send = async {
			let mut sent = Vec::new();
			for id in 0..count {
				sent.push(Instant::now());
				client.send(&id.to_be_bytes()).await.unwrap();
				tokio::time::sleep(spacing).await;
			}
			sent
		};
		let receive = async {
			let mut got = Vec::new();
			let mut buf = [0u8; 4];
			while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(500), client.recv(&mut buf)).await {
				got.push((u32::from_be_bytes(buf), Instant::now()));
			}
			got
		};
		tokio::join!(send, receive)
	}

	fn batched(count: usize, window: Duration) -> Options {
		Options {
			batch: Some(Batch { count, window }),
			..Default::default()
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_batch_releases_on_count_and_on_the_window() {
		let socket = Arc::new(UdpSocket::bind(LOCALHOST).await.unwrap());
		let ms = |ms| Duration::from_millis(ms);
		let now = Instant::now();
		let parcel = |at: Instant| Parcel {
			arrived: at,
			at,
			delayed: false,
			datagram: Vec::new(),
			socket: socket.clone(),
			dest: LOCALHOST,
		};
		let batch = Batch {
			count: 3,
			window: ms(160),
		};
		let tally = Tally::default();
		let mut held = Held::default();
		let mut ready = Vec::new();

		held.hold(batch, parcel(now), &mut ready, &tally);
		held.hold(batch, parcel(now + ms(10)), &mut ready, &tally);
		assert!(ready.is_empty(), "a partial batch leaked");
		assert_eq!(
			held.closes,
			Some(now + ms(160)),
			"the window counts from the first arrival"
		);

		held.hold(batch, parcel(now + ms(20)), &mut ready, &tally);
		let leaves: Vec<Instant> = ready.drain(..).map(|parcel| parcel.at).collect();
		assert_eq!(
			leaves,
			[now + ms(20); 3],
			"a full batch leaves together, as the latest would"
		);
		assert_eq!(
			tally.snapshot().delayed,
			2,
			"the datagram that filled it waited for nothing"
		);

		// The window closes a batch that never fills, and nothing in it leaves sooner.
		held.hold(batch, parcel(now + ms(30)), &mut ready, &tally);
		let closes = held.closes.unwrap();
		assert_eq!(closes, now + ms(190));
		held.release(closes, &mut ready, &tally);
		assert_eq!(ready.iter().map(|parcel| parcel.at).collect::<Vec<_>>(), [closes]);
		assert_eq!(tally.snapshot().delayed, 3);
		ready.clear();

		// A datagram that arrives after the window closed, before its timer fired,
		// starts the next batch rather than joining the closed one.
		held.hold(batch, parcel(now + ms(200)), &mut ready, &tally);
		let closes = held.closes.unwrap();
		held.hold(batch, parcel(closes + ms(1)), &mut ready, &tally);
		assert_eq!(ready.iter().map(|parcel| parcel.at).collect::<Vec<_>>(), [closes]);
		assert_eq!(held.parcels.len(), 1, "the late datagram joined the closed batch");
		assert_eq!(held.closes, Some(closes + ms(1) + ms(160)));
		assert_eq!(tally.snapshot().delayed, 4);
		assert_eq!(tally.batched.load(Ordering::Relaxed), 4);

		// Behind a delay longer than the window, a lone datagram leaves when the
		// delay says: the batch held nothing, whatever `delayed` counts.
		ready.clear();
		let mut held = Held::default();
		let tally = Tally::default();
		let late = Parcel {
			at: now + ms(1000),
			delayed: true,
			..parcel(now)
		};
		held.hold(batch, late, &mut ready, &tally);
		held.release(held.closes.unwrap(), &mut ready, &tally);
		assert_eq!(ready[0].at, now + ms(1000));
		assert_eq!(tally.batched.load(Ordering::Relaxed), 0);
	}

	#[tokio::test(start_paused = true)]
	async fn a_batch_releases_datagrams_together() {
		let (shaper, client) = shaped(|config| Setup {
			up: batched(7, Duration::from_millis(160)),
			..config.into()
		})
		.await;

		// Three full batches, each filled well inside its window.
		let (_, got) = paced(&client, 21, Duration::from_millis(10)).await;
		assert_eq!(
			got.iter().map(|&(id, _)| id).collect::<Vec<_>>(),
			(0..21).collect::<Vec<_>>()
		);

		let batches: Vec<&[(u32, Instant)]> = got.chunks(7).collect();
		for batch in &batches {
			let spread = batch[6].1 - batch[0].1;
			assert!(
				spread < Duration::from_millis(15),
				"a batch arrived spread over {spread:?}"
			);
		}
		for pair in batches.windows(2) {
			let gap = pair[1][0].1 - pair[0][0].1;
			assert!(gap > Duration::from_millis(40), "two batches arrived {gap:?} apart");
		}

		let stats = shaper.verify().unwrap();
		assert_eq!(
			stats.up.delayed, 18,
			"every datagram but the last of each batch waits: {stats}"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn the_window_releases_a_batch_that_never_fills() {
		let (shaper, client) = shaped(|config| Setup {
			up: batched(100, Duration::from_millis(80)),
			..config.into()
		})
		.await;

		let (sent, got) = paced(&client, 3, Duration::ZERO).await;
		assert_eq!(got.len(), 3);
		let held = got[0].1 - sent[0];
		assert!(held >= Duration::from_millis(80), "the batch left after only {held:?}");
		assert_eq!(shaper.verify().unwrap().up.delayed, 3);
	}

	#[tokio::test(start_paused = true)]
	async fn a_shared_batch_fills_from_every_client() {
		// Three datagrams from each of two clients, into batches of six: only a
		// shared path fills one, and separate links wait out the window.
		let window = Duration::from_millis(300);
		for shared in [true, false] {
			let got = two_clients(
				|config| Setup {
					shared,
					up: batched(6, window),
					..config.into()
				},
				6,
			)
			.await;
			assert_eq!(got.len(), 6);
			let last = got.iter().map(|&(_, after)| after).max().unwrap();
			assert_eq!(
				last < window / 2,
				shared,
				"shared {shared}: the batch left after {last:?}"
			);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_batch_needs_a_count_and_a_window() {
		let refused = |options: Options, why: &'static str| async move {
			let config = Config {
				bind: LOCALHOST,
				target: LOCALHOST,
				seed: 0,
				up: Profile::default(),
				down: Profile::default(),
			};
			let err = Shaper::bind(Setup {
				up: options,
				..config.into()
			})
			.await
			.err()
			.unwrap_or_else(|| panic!("accepted a batch where {why}"));
			assert!(format!("{err:#}").contains(why), "{err:#}");
		};
		refused(batched(0, Duration::from_millis(10)), "a batch of 0 never holds").await;
		refused(batched(1, Duration::from_millis(10)), "a batch of 1 never holds").await;
		refused(batched(7, Duration::ZERO), "no window never holds").await;
	}

	#[test]
	fn a_batch_that_never_held_anything_is_unapplied() {
		let config = Config {
			bind: LOCALHOST,
			target: LOCALHOST,
			seed: 0,
			up: Profile::default(),
			down: Profile::default(),
		};
		let setup = Setup {
			up: batched(7, Duration::from_millis(160)),
			..config.into()
		};
		let quiet = |packets| Stats {
			up: Counters {
				packets,
				..Default::default()
			},
			..Default::default()
		};

		// (1/7)^2 is 2%: two datagrams through batches of seven prove nothing.
		assert!(!unbatched(&setup, &quiet(2), 0));
		// (1/7)^10 is 4e-9: ten datagrams a batch never held means no batch.
		assert!(unbatched(&setup, &quiet(10), 0));
		assert!(!unbatched(&setup, &quiet(10), 6));

		// A delay the profile gave is not a batch holding anything.
		let mut delayed = quiet(10);
		delayed.up.delayed = 10;
		assert!(unbatched(&setup, &delayed, 0));
	}

	/// How long after `at` each of `sizes` leaves one link, fed at the same instant.
	fn owed(link: &mut Link, at: Instant, sizes: &[usize]) -> Vec<Duration> {
		let tally = Tally::default();
		sizes
			.iter()
			.map(|&size| link.treat(at, size, &tally).expect("dropped").0 - at)
			.collect()
	}

	fn stepped(steps: Vec<Step>) -> Options {
		Options {
			steps,
			..Default::default()
		}
	}

	#[test]
	fn each_step_applies_at_its_own_time() {
		let ms = Duration::from_millis;
		let profile = Profile {
			delay: ms(5),
			..Default::default()
		};
		let options = stepped(vec![
			Step {
				at: ms(100),
				delay: Some(ms(60)),
				..Default::default()
			},
			Step {
				at: ms(200),
				delay: Some(ms(5)),
				..Default::default()
			},
		]);
		let (queue, _) = mpsc::unbounded_channel();
		let start = Instant::now();
		let mut link = Link::new(&profile, &options, 7, 0, start, queue);

		assert_eq!(owed(&mut link, start, &[16]), [ms(5)]);
		assert_eq!(owed(&mut link, start + ms(100), &[16]), [ms(60)]);
		// The step back restores what the run opened with.
		assert_eq!(owed(&mut link, start + ms(200), &[16]), [ms(5)]);
	}

	#[test]
	fn a_loss_step_turns_the_loss_on_and_off_again() {
		let secs = Duration::from_secs;
		let options = stepped(vec![
			Step {
				at: secs(30),
				loss: Some(1.0),
				..Default::default()
			},
			Step {
				at: secs(60),
				loss: Some(0.0),
				..Default::default()
			},
		]);
		let (queue, _) = mpsc::unbounded_channel();
		let start = Instant::now();
		let mut link = Link::new(&Profile::default(), &options, 7, 0, start, queue);
		let tally = Tally::default();
		let mut lost = |at: Instant| (0..100).filter(|_| link.treat(at, 16, &tally).is_none()).count();

		assert_eq!(lost(start), 0, "the profile opens clean");
		assert_eq!(lost(start + secs(30)), 100, "the step lost nothing");
		assert_eq!(lost(start + secs(60)), 0, "the step back never cleared");
	}

	#[tokio::test(start_paused = true)]
	async fn a_step_gets_worse_part_way_through() {
		let ms = Duration::from_millis;
		let (shaper, client) = shaped(|config| Setup {
			up: stepped(vec![Step {
				at: ms(150),
				delay: Some(ms(60)),
				..Default::default()
			}]),
			..Config {
				up: Profile {
					delay: ms(5),
					..Default::default()
				},
				..config
			}
			.into()
		})
		.await;

		let (sent, got) = paced(&client, 40, ms(10)).await;
		assert_eq!(got.len(), 40);
		let median = |range: std::ops::Range<usize>| {
			let mut latency: Vec<Duration> = got[range].iter().map(|&(id, at)| at - sent[id as usize]).collect();
			latency.sort();
			latency[latency.len() / 2]
		};

		// The first ten leave well inside the first 150ms, the last fifteen well after.
		let (before, after) = (median(0..10), median(25..40));
		assert!(
			after > before + ms(30),
			"median latency went from {before:?} to {after:?}"
		);
		shaper.verify().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn a_step_the_run_never_reached_is_unapplied() {
		let (shaper, client) = shaped(|config| Setup {
			down: stepped(vec![Step {
				at: Duration::from_secs(60),
				delay: Some(Duration::from_millis(60)),
				..Default::default()
			}]),
			..config.into()
		})
		.await;
		assert_eq!(round_trip(&client, 10).await, (0..10).collect::<Vec<_>>());

		let err = shaper.verify().expect_err("a run that ended before its step passed");
		assert!(format!("{err:#}").contains("the down step at 60s"), "{err:#}");
	}

	#[tokio::test(start_paused = true)]
	async fn a_step_no_datagram_saw_is_unapplied() {
		let ms = Duration::from_millis;
		let (shaper, client) = shaped(|config| Setup {
			up: stepped(vec![
				Step {
					at: ms(100),
					delay: Some(ms(60)),
					..Default::default()
				},
				Step {
					at: ms(200),
					delay: Some(ms(5)),
					..Default::default()
				},
			]),
			..Config {
				up: Profile {
					delay: ms(5),
					..Default::default()
				},
				..config
			}
			.into()
		})
		.await;

		// One datagram before the first step and one after the second, so the
		// 60ms phase passes with nothing in it.
		let mut buf = [0u8; 4];
		client.send(&0u32.to_be_bytes()).await.unwrap();
		client.recv(&mut buf).await.unwrap();
		tokio::time::sleep(ms(250)).await;
		client.send(&1u32.to_be_bytes()).await.unwrap();
		client.recv(&mut buf).await.unwrap();

		let err = shaper.verify().expect_err("a run that skipped a step passed");
		let err = format!("{err:#}");
		assert!(err.contains("the up step at 100ms"), "{err}");
		assert!(!err.contains("the up step at 200ms"), "{err}");
	}

	#[tokio::test(start_paused = true)]
	async fn a_profile_no_datagram_saw_before_its_step_is_unapplied() {
		let ms = Duration::from_millis;
		let (shaper, client) = shaped(|config| Setup {
			up: stepped(vec![Step {
				at: ms(100),
				delay: Some(ms(60)),
				..Default::default()
			}]),
			..Config {
				up: Profile {
					delay: ms(5),
					..Default::default()
				},
				..config
			}
			.into()
		})
		.await;

		// The first datagram comes after the step, so the 5ms phase passes with nothing in it.
		tokio::time::sleep(ms(250)).await;
		let mut buf = [0u8; 4];
		client.send(&0u32.to_be_bytes()).await.unwrap();
		client.recv(&mut buf).await.unwrap();

		let err = shaper.verify().expect_err("a run that started after its step passed");
		let err = format!("{err:#}");
		assert!(err.contains("the up profile before its step at 100ms"), "{err}");
		assert!(!err.contains("the up step at"), "{err}");
	}

	#[test]
	fn a_step_past_the_clock_never_applies() {
		let options = stepped(vec![Step {
			at: Duration::MAX,
			delay: Some(Duration::from_millis(60)),
			..Default::default()
		}]);
		let (queue, _) = mpsc::unbounded_channel();
		let start = Instant::now();
		let mut link = Link::new(&Profile::default(), &options, 7, 0, start, queue);
		assert_eq!(owed(&mut link, start + Duration::from_secs(1), &[16]), [Duration::ZERO]);
	}

	#[tokio::test(start_paused = true)]
	async fn steps_that_would_be_skipped_or_do_nothing_are_refused() {
		let refused = |steps: Vec<Step>, why: &'static str| async move {
			let config = Config {
				bind: LOCALHOST,
				target: LOCALHOST,
				seed: 0,
				up: Profile {
					delay: Duration::from_millis(20),
					..Default::default()
				},
				down: Profile::default(),
			};
			let err = Shaper::bind(Setup {
				down: stepped(steps),
				..config.into()
			})
			.await
			.err()
			.unwrap_or_else(|| panic!("accepted steps where {why}"));
			assert!(format!("{err:#}").contains(why), "{err:#}");
		};
		let at = |secs| Step {
			at: Duration::from_secs(secs),
			loss: Some(0.1),
			..Default::default()
		};

		refused(vec![at(60), at(30)], "steps go in order").await;
		refused(vec![at(0)], "a step at zero").await;
		refused(
			vec![Step {
				at: Duration::from_secs(30),
				..Default::default()
			}],
			"changes nothing",
		)
		.await;
		// Restating the value already in force changes nothing either.
		refused(
			vec![
				at(30),
				Step {
					at: Duration::from_secs(60),
					loss: Some(0.1),
					..Default::default()
				},
			],
			"the step at 60s changes nothing",
		)
		.await;
		// Uniform jitter past the delay, reached by a step rather than at the start.
		refused(
			vec![Step {
				at: Duration::from_secs(30),
				jitter: Some(Duration::from_millis(10)),
				..Default::default()
			}],
			"exceeds delay",
		)
		.await;
	}
}
