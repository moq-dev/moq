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
		Arc, Mutex, OnceLock,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use anyhow::Context;
use rand::{RngExt, SeedableRng, rngs::Xoshiro256PlusPlus};
use tokio::{
	net::{TcpListener, TcpStream, UdpSocket},
	sync::mpsc,
	task::JoinSet,
	time::Instant,
};

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
		if let Some(rate) = &self.rate {
			anyhow::ensure!(rate.bits_per_second > 0, "a rate limit of zero passes nothing");
		}
		Ok(())
	}
}

/// How a direction draws each datagram's jitter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Batch {
	/// How many datagrams close a batch early.
	pub count: usize,
	/// How long a batch waits for that many.
	pub window: Duration,
}

/// One direction's opt-in options beyond its [`Profile`]. The default adds nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
	/// How the profile's `jitter` is drawn.
	pub jitter_model: Jitter,
	/// Hold datagrams and release them together, if at all.
	pub batch: Option<Batch>,
}

impl Options {
	/// Check the options, and `profile` as they draw it.
	fn validate(&self, profile: &Profile) -> anyhow::Result<()> {
		profile.validate(self.jitter_model)?;
		if let Some(batch) = &self.batch {
			anyhow::ensure!(batch.count > 0, "a batch of zero datagrams never closes");
			anyhow::ensure!(!batch.window.is_zero(), "a batch with no window never holds anything");
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

/// Whether the batches `setup` configures implausibly never held a datagram.
///
/// A batch holds every datagram but the one that fills it, so it acts on at
/// least `1 - 1 / count` of them. What it holds counts as delayed once the
/// batch leaves.
fn unbatched(setup: &Setup, stats: &Stats) -> bool {
	let chance = |options: &Options| options.batch.map_or(0.0, |batch| 1.0 - 1.0 / batch.count as f64);
	let silence = (1.0 - chance(&setup.up)).powf(stats.up.packets as f64)
		* (1.0 - chance(&setup.down)).powf(stats.down.packets as f64);
	stats.up.delayed + stats.down.delayed == 0 && silence < IMPLAUSIBLE
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

/// A [`Config`] plus the opt-in options it has no field for.
///
/// Every option defaults to off, so a setup made from a config shapes exactly
/// as that config does.
#[derive(Clone, Debug)]
pub struct Setup {
	/// Where to listen and forward, the seed, and each direction's profile.
	pub config: Config,
	/// Also accept TCP on the listening port and pipe it to the target untouched.
	///
	/// A relay serves HTTP on the port number it serves QUIC on, and a browser
	/// fetches the certificate hash from it before it dials WebTransport. TCP is
	/// never impaired: a reliable transport cannot shed load, so shaping it
	/// would measure how TCP retransmits.
	pub tcp_passthrough: bool,
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
			tcp_passthrough: false,
			shared: false,
			up: Options::default(),
			down: Options::default(),
		}
	}
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
	setup: Setup,
	tally: Arc<[Tally; 2]>,
	/// Why forwarding stopped, if it did.
	failed: Arc<OnceLock<String>>,
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

		// The port the UDP socket got, since a relay serves HTTP on its QUIC port.
		let tcp = match setup.tcp_passthrough {
			true => Some(
				TcpListener::bind(addr)
					.await
					.with_context(|| format!("bind the TCP passthrough on {addr}"))?,
			),
			false => None,
		};

		let tally = Arc::new([Tally::default(), Tally::default()]);
		let failed = Arc::new(OnceLock::new());
		let task = tokio::spawn({
			let setup = setup.clone();
			let tally = tally.clone();
			let failed = failed.clone();
			async move {
				if let Err(err) = run(Arc::new(listen), tcp, setup, tally).await {
					let _ = failed.set(format!("{err:#}"));
				}
			}
		});

		Ok(Self {
			addr,
			setup,
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
		&self.setup.config
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

		let mut missing = unapplied(&self.setup.config, &stats);
		if unbatched(&self.setup, &stats) {
			missing.push("batch");
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
/// address per client just as it would without the shaper in the way. Each
/// flow takes a link of its own each way, unless the path is shared.
async fn run(
	listen: Arc<UdpSocket>,
	tcp: Option<TcpListener>,
	setup: Setup,
	tally: Arc<[Tally; 2]>,
) -> anyhow::Result<()> {
	let config = &setup.config;
	let mut flows = HashMap::<SocketAddr, Flow>::new();
	let mut tasks = JoinSet::new();
	let mut buf = vec![0u8; u16::MAX as usize];

	// A shared path is one link each way, drawing the streams the first flow would.
	let shared = setup.shared.then(|| {
		let now = Instant::now();
		[UP, DOWN].map(|direction| link(&mut tasks, &setup, direction, 0, now, &tally))
	});

	loop {
		let (size, from) = tokio::select! {
			res = listen.recv_from(&mut buf) => res.context("receive from a client")?,
			Some(res) = tasks.join_next() => {
				res.context("flow task panicked")??;
				continue;
			}
			res = accept(tcp.as_ref()) => {
				let (stream, _) = res.context("accept a TCP connection")?;
				tasks.spawn(pipe(stream, config.target));
				continue;
			}
		};
		let now = Instant::now();

		let number = flows.len() as u64;
		let flow = match flows.entry(from) {
			hash_map::Entry::Occupied(entry) => entry.into_mut(),
			hash_map::Entry::Vacant(entry) => {
				let upstream = Arc::new(bind_toward(config.target).await?);

				let [up, down] = match &shared {
					Some(links) => links.clone(),
					None => [UP, DOWN].map(|direction| link(&mut tasks, &setup, direction, number, now, &tally)),
				};
				tasks.spawn(reply(
					upstream.clone(),
					config.target,
					down,
					listen.clone(),
					from,
					tally.clone(),
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
	now: Instant,
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
		now,
		queue,
	)))
}

/// The next TCP connection, or never without a passthrough.
async fn accept(tcp: Option<&TcpListener>) -> std::io::Result<(TcpStream, SocketAddr)> {
	match tcp {
		Some(listener) => listener.accept().await,
		None => std::future::pending().await,
	}
}

/// Copy one TCP connection to the target and back, untouched.
async fn pipe(mut client: TcpStream, target: SocketAddr) -> anyhow::Result<()> {
	// Either end refusing or hanging up ends that connection, not the shaper.
	if let Ok(mut server) = TcpStream::connect(target).await {
		let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
	}
	Ok(())
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
) -> anyhow::Result<()> {
	let mut buf = vec![0u8; u16::MAX as usize];
	loop {
		let (size, from) = socket.recv_from(&mut buf).await.context("receive from the target")?;
		// Anything else reaching this ephemeral port is not part of the path.
		if from != target {
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
				let now = Instant::now();
				while let Some(&Reverse((at, id))) = pending.peek() {
					if at > now {
						break;
					}
					pending.pop();
					let parcel = parcels.remove(&id).expect("queued datagram");
					// A send error is the path losing the datagram, the way a
					// network does when the far end is gone (a killed relay, say),
					// so it is not the shaper failing.
					let _ = parcel.socket.send_to(&parcel.datagram, parcel.dest).await;
				}
				// The window closes a batch that never filled.
				if let Some(closes) = held.closes.filter(|&closes| closes <= now) {
					held.release(closes, &mut ready, &tally[direction]);
				}
			}
		}
		for parcel in ready.drain(..) {
			pending.push(Reverse((parcel.at, sequence)));
			parcels.insert(sequence, parcel);
			sequence += 1;
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
			// A hold is a delay, counted once whichever stage gave it.
			if at > parcel.at && !parcel.delayed {
				bump(&tally.delayed);
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
		now: Instant,
		queue: mpsc::UnboundedSender<Parcel>,
	) -> Self {
		Self {
			profile: profile.clone(),
			jitter: options.jitter_model,
			rng: Xoshiro256PlusPlus::seed_from_u64(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
			// Not the moment the link is built, which is after `now`: that would
			// make the first datagram queue behind a bucket still refilling.
			full_at: now,
			floor: now,
			queue,
		}
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

	#[tokio::test]
	async fn tcp_passes_through_untouched() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		// A relay answering HTTP on its QUIC port, reduced to an echo.
		let server = TcpListener::bind(LOCALHOST).await.unwrap();
		let target = server.local_addr().unwrap();
		tokio::spawn(async move {
			let (mut stream, _) = server.accept().await.unwrap();
			let mut buf = [0u8; 64];
			let size = stream.read(&mut buf).await.unwrap();
			stream.write_all(&buf[..size]).await.unwrap();
		});

		// A profile that would lose every datagram, to show TCP skips it.
		let blackhole = Profile {
			loss: 1.0,
			..Default::default()
		};
		let config = Config {
			bind: LOCALHOST,
			target,
			seed: 13,
			up: blackhole.clone(),
			down: blackhole,
		};
		let shaper = Shaper::bind(Setup {
			tcp_passthrough: true,
			..config.into()
		})
		.await
		.unwrap();

		let mut client = TcpStream::connect(shaper.addr()).await.unwrap();
		client.write_all(b"/certificate.sha256").await.unwrap();
		let mut buf = [0u8; 64];
		let size = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
			.await
			.unwrap()
			.unwrap();
		assert_eq!(&buf[..size], b"/certificate.sha256");
		assert_eq!(shaper.stats(), Stats::default(), "TCP reached the datagram path");
	}

	#[tokio::test]
	async fn tcp_is_refused_without_the_passthrough() {
		let (shaper, _client) = setup(1, Profile::default(), Profile::default()).await;
		assert!(TcpStream::connect(shaper.addr()).await.is_err());
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
	};

	#[tokio::test]
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

	#[tokio::test]
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

	#[tokio::test]
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

	#[tokio::test]
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
	}

	#[tokio::test]
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

	#[tokio::test]
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

	#[tokio::test]
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

	#[tokio::test]
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
		refused(batched(0, Duration::from_millis(10)), "never closes").await;
		refused(batched(7, Duration::ZERO), "never holds").await;
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
		assert!(!unbatched(&setup, &quiet(2)));
		// (1/7)^10 is 4e-9: ten datagrams a batch never held means no batch.
		assert!(unbatched(&setup, &quiet(10)));

		let mut held = quiet(10);
		held.up.delayed = 6;
		assert!(!unbatched(&setup, &held));
	}
}
