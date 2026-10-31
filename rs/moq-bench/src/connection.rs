use moq_clock::Wall;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use moq_tokio::Status;
use moq_tokio::moq_net::{self, bytes::Bytes};
use moq_tokio::moq_net::{broadcast, group, track};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::Stats;

/// The single track name every broadcast publishes and subscribers look up.
const TRACK: &str = "data";

/// Per-connection parameters, rolled once from the configured ranges.
#[derive(Clone, Copy, Debug)]
pub struct Rolled {
	pub broadcasts: u64,
	pub subscribe: u64,
	pub fps: u64,
	pub frame_size: u64,
	pub group_size: u64,
}

/// The JSON keyframe written at the start of every group, describing the rolled
/// parameters so a subscriber (or a packet capture) can reconstruct the shape.
#[derive(Serialize)]
struct GroupHeader<'a> {
	connection: u64,
	broadcast: &'a str,
	group: u64,
	fps: u64,
	frame_size: u64,
	group_size: u64,
	broadcasts: u64,
	subscribe: u64,
	/// Wall-clock milliseconds, handy for rough one-way latency when clocks agree.
	timestamp_ms: u128,
	/// Zero padding sizing the keyframe up to the rolled frame size, so a
	/// lone-keyframe group (`group_size = 0`, the chat shape) still costs
	/// `frame_size` bytes on the wire.
	pad: String,
}

/// The subset of [`GroupHeader`] a subscriber reads back to learn the shape of a
/// broadcast it didn't publish. Extra fields on the wire are ignored.
#[derive(Deserialize)]
struct RecvHeader {
	fps: u64,
	frame_size: u64,
	group_size: u64,
	timestamp_ms: u128,
}

/// The role one connection plays in the selected benchmark shape.
pub enum Role {
	/// Publish and discover broadcasts within this invocation's namespace.
	Mesh,
	/// Publish the one broadcast every fan-out subscriber targets.
	FanoutPublisher { path: String },
	/// Subscribe to the exact fan-out broadcast, waiting for its announcement.
	FanoutSubscriber { path: String },
}

/// Everything one benchmark connection needs to run: its identity, the rolled
/// parameters, and the shared client/stats handles. Bundled into a struct so
/// `run` and its call site aren't drowning in positional arguments.
pub struct Connection {
	pub index: u64,
	pub run_id: u64,
	pub role: Role,
	pub rolled: Rolled,
	pub config: Arc<crate::Config>,
	pub client: moq_tokio::Client,
	pub stats: Arc<Stats>,
}

/// Publish `broadcasts` tracks and subscribe to `subscribe` peer broadcasts
/// discovered via announcements.
///
/// Returns only when the underlying reconnect loop permanently gives up.
pub async fn run(ctx: Connection) {
	let Connection {
		index: connection,
		run_id,
		role,
		rolled,
		config,
		client,
		stats,
	} = ctx;

	let url = config.client.url.clone().expect("url required");

	// Publish side: an origin we fill with our broadcasts and hand to the session.
	let publish = moq_tokio::origin::spawn();
	// Consume side: the session fills this with peer announcements.
	let consume = moq_tokio::origin::spawn();

	let namespace = format!("{}/{run_id:08x}", config.name());
	let discovery = if config.publishes() {
		namespace.as_str()
	} else {
		config.name()
	};

	let mut broadcasts = Vec::new();
	let mut publishing = Vec::new();
	let mut own = HashSet::new();
	let mut tasks = JoinSet::new();

	let paths: Vec<(String, String)> = match &role {
		Role::Mesh => (0..rolled.broadcasts)
			.map(|index| {
				let relative = format!("{connection}/{index}");
				let path = format!("{namespace}/{relative}");
				(relative, path)
			})
			.collect(),
		Role::FanoutPublisher { path } => vec![(path.clone(), path.clone())],
		Role::FanoutSubscriber { .. } => Vec::new(),
	};

	for (relative, path) in paths {
		let broadcast = match publish.create_broadcast(&path) {
			Ok(broadcast) => broadcast,
			Err(err) => {
				tracing::error!(connection, %err, "failed to create broadcast");
				continue;
			}
		};
		let track = match broadcast.create_track(
			TRACK,
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		) {
			Ok(track) => track,
			Err(err) => {
				tracing::error!(connection, %err, "failed to create track");
				continue;
			}
		};
		if let Err(err) = broadcast.announce(Default::default()) {
			tracing::error!(connection, %err, "failed to announce broadcast");
			continue;
		}
		own.insert(relative);
		// Hold the broadcast producer for the connection's lifetime so it stays
		// published and advertised.
		broadcasts.push(broadcast);

		let published = Published::new(stats.clone());
		publishing.push(Arc::downgrade(&published));
		tasks.spawn(produce(connection, path, rolled, track, published));
	}

	let client = client.with_publisher(&publish).with_subscriber(consume.clone());
	let mut reconnect = client.connect(url);

	match &role {
		Role::Mesh if rolled.subscribe > 0 => {
			tasks.spawn(subscribe(
				discover(&consume, discovery),
				own,
				rolled.subscribe,
				config.startup(),
				config.max_delay(),
				stats.clone(),
			));
		}
		Role::FanoutSubscriber { path } => {
			tasks.spawn(subscribe_named(
				consume.consume(),
				path.clone(),
				config.max_delay(),
				stats.clone(),
			));
		}
		Role::Mesh | Role::FanoutPublisher { .. } => {}
	}

	// The status loop doubles as the keep-alive: it tracks connect/disconnect for
	// the gauge and returns once the reconnect loop gives up.
	let mut connected = false;
	loop {
		tokio::select! {
			status = reconnect.status() => match status {
				// Edge-triggered so repeated same-state events can't drift the gauge.
				Ok(Status::Connected) => {
					if !connected {
						connected = true;
						stats.connections.fetch_add(1, Ordering::Relaxed);
						for published in publishing.iter().filter_map(std::sync::Weak::upgrade) {
							published.establish();
						}
					}
				}
				Ok(Status::Disconnected) => {
					if connected {
						connected = false;
						stats.connections.fetch_sub(1, Ordering::Relaxed);
					}
				}
				Ok(_) => {}
				Err(err) => {
					tracing::warn!(connection, %err, "connection gave up");
					break;
				}
			},
			// Surface a fatal task error, but keep running otherwise.
			Some(res) = tasks.join_next() => {
				if let Ok(Err(err)) = res {
					tracing::debug!(connection, %err, "task ended");
				}
			}
		}
	}

	if connected {
		stats.connections.fetch_sub(1, Ordering::Relaxed);
	}
}

/// Produce frames for one track at `fps`, opening a new group every `group_size`
/// frames. Each group starts with a JSON keyframe; the rest are zeroed.
async fn produce(
	connection: u64,
	path: String,
	rolled: Rolled,
	track: track::Producer,
	published: Arc<Published>,
) -> anyhow::Result<()> {
	let stats = &published.stats;
	let _gauge = Gauge::inc(&stats.broadcasts);

	// Zero fps means an idle track: keep it published but never produce.
	if rolled.fps == 0 {
		std::future::pending::<()>().await;
		return Ok(());
	}

	let zeros = Bytes::from(vec![0u8; rolled.frame_size as usize]);
	let period = Duration::from_secs_f64(1.0 / rolled.fps as f64);
	let mut ticker = moq_clock::interval(period);
	ticker.set_missed_tick_behavior(moq_clock::MissedTickBehavior::Delay);

	let mut sequence = 0u64;
	loop {
		let mut group = track.append_group()?;

		// Keyframe: the JSON header describing this connection's rolled parameters.
		ticker.tick().await;
		let mut header = GroupHeader {
			connection,
			broadcast: &path,
			group: sequence,
			fps: rolled.fps,
			frame_size: rolled.frame_size,
			group_size: rolled.group_size,
			broadcasts: rolled.broadcasts,
			subscribe: rolled.subscribe,
			timestamp_ms: moq_clock::wall()
				.saturating_duration_since(Wall::UNIX_EPOCH)
				.as_millis(),
			pad: String::new(),
		};
		let mut payload = serde_json::to_vec(&header)?;
		// Pad the keyframe up to the rolled frame size, so `frame_size` holds even
		// when the keyframe is the only frame (`group_size = 0`, the chat shape).
		// A header already at or past the target is sent as-is.
		if let Some(n) = (rolled.frame_size as usize).checked_sub(payload.len())
			&& n > 0
		{
			header.pad = "0".repeat(n);
			payload = serde_json::to_vec(&header)?;
		}
		let header = Bytes::from(payload);
		group.write_frame(moq_net::Timestamp::now(), header.clone())?;
		stats.frame_sent(header.len());

		// The remaining frames in the group are zeroed payload.
		for _ in 0..rolled.group_size {
			ticker.tick().await;
			group.write_frame(moq_net::Timestamp::now(), zeros.clone())?;
			stats.frame_sent(zeros.len());
		}

		group.finish()?;
		if sequence == 0 {
			published.establish();
		}
		sequence += 1;
	}
}

/// Announce consumer scoped to the bench namespace, emitting paths relative to it.
///
/// The relay announces its own broadcasts too (`.stats/...` when stats publishing
/// is on, which production relays enable), and a subscription slot burned on one
/// of those is never retried, so an unscoped consumer starves the subscribe side.
fn discover(consume: &moq_net::origin::Producer, name: &str) -> moq_net::origin::Consumer {
	consume
		.consume()
		.scope(name, &moq_net::Patterns::from(moq_net::Pattern::all()))
		.expect("origin must permit the bench namespace")
}

/// Wait for one exact broadcast and drain it for the lifetime of the source.
async fn subscribe_named(
	consume: moq_net::origin::Consumer,
	path: String,
	max_delay: Duration,
	stats: Arc<Stats>,
) -> anyhow::Result<()> {
	let broadcast = consume.follow(path.as_str(), None)?.available().await?;
	drain(broadcast, max_delay, &stats).await
}

/// Watch announcements and drain up to `want` peer broadcasts (excluding our own).
///
/// Candidates are gathered over the `startup` window and picked at random. The
/// relay replays existing announcements in deterministic path order, so a
/// first-come pick would put every subscriber on the same first few broadcasts
/// and collapse the 1:N presets into hotspots. Announcements arriving after the
/// window fill any remaining slots in arrival order.
///
/// The gather window is the only delay added here: connections are already
/// staggered across `startup` by main, so subscriptions land spread over the
/// second startup window of the run, and the whole swarm is subscribed within
/// two of them.
async fn subscribe(
	consume: moq_net::origin::Consumer,
	own: HashSet<String>,
	want: u64,
	startup: Duration,
	max_delay: Duration,
	stats: Arc<Stats>,
) -> anyhow::Result<()> {
	let mut announced = consume.announced();
	let mut tasks = JoinSet::new();
	let mut seen: HashSet<String> = HashSet::new();
	let mut pool = Vec::new();
	let mut eligible = 0;

	// Gather candidates until the startup window closes (or the announce stream ends).
	let deadline = moq_clock::sleep(startup);
	tokio::pin!(deadline);
	loop {
		tokio::select! {
			// Deadline first: with random polling, a stream that always has another
			// announcement ready could keep gathering past the startup window.
			biased;
			_ = &mut deadline => break,
			update = announced.next() => {
				let update = match update {
					Some(
						moq_net::announce::Event::Start(update)
						| moq_net::announce::Event::Update(update)
						| moq_net::announce::Event::Restart(update) | moq_net::announce::Event::Takeover { announce: update, .. },
					) => update,
					Some(moq_net::announce::Event::End(_)) => continue,
					None => break,
				};
				let path = update.prefix.to_string();
				if own.contains(&path) || !seen.insert(path.clone()) {
					continue;
				}
				eligible += 1;
				reservoir_push(&mut pool, want as usize, eligible, path);
			}
		}
	}

	let mut selected = pool.len() as u64;
	for path in pool {
		let Ok(broadcast) = consume.request(path.as_str(), None).await else {
			continue;
		};
		spawn_drain(&mut tasks, path, broadcast, max_delay, stats.clone());
	}

	// Top up from late announcements, first-come: the pool was too small, so
	// there is nothing to spread over.
	while selected < want {
		let update = match announced.next().await {
			Some(
				moq_net::announce::Event::Start(update)
				| moq_net::announce::Event::Update(update)
				| moq_net::announce::Event::Restart(update)
				| moq_net::announce::Event::Takeover { announce: update, .. },
			) => update,
			Some(moq_net::announce::Event::End(_)) => continue,
			None => break,
		};
		let path = update.prefix.to_string();
		if own.contains(&path) || !seen.insert(path.clone()) {
			continue;
		}
		let Ok(broadcast) = consume.request(path.as_str(), None).await else {
			continue;
		};
		selected += 1;
		spawn_drain(&mut tasks, path, broadcast, max_delay, stats.clone());
	}

	// Keep the drain tasks alive; they run until their broadcasts close.
	while tasks.join_next().await.is_some() {}
	Ok(())
}

/// Reservoir-sample (Algorithm R): offer the `eligible`-th stream item (1-based)
/// to a pool holding at most `want` uniform picks. Bounded memory, so a big
/// namespace never piles a copy of itself into every subscriber; dropped
/// candidates release their broadcast handles immediately.
fn reservoir_push<T>(pool: &mut Vec<T>, want: usize, eligible: usize, item: T) {
	if pool.len() < want {
		pool.push(item);
		return;
	}
	let slot = rand::rng().random_range(0..eligible);
	if slot < want {
		pool[slot] = item;
	}
}

/// Queue one broadcast for draining. No extra delay: the caller's gather window
/// and main's connection stagger already spread subscription starts.
fn spawn_drain(
	tasks: &mut JoinSet<()>,
	path: String,
	broadcast: broadcast::Consumer,
	max_delay: Duration,
	stats: Arc<Stats>,
) {
	tasks.spawn(async move {
		if let Err(err) = drain(broadcast, max_delay, &stats).await {
			tracing::debug!(%path, %err, "subscription ended");
		}
	});
}

/// Subscribe to the broadcast's track, counting every frame received and tracking
/// group-sequence gaps to report skipped groups.
///
/// Only a track- or session-level failure ends the subscription. A group that
/// fails mid-read is the relay giving up on that one group, which a real player
/// skips over while it keeps watching.
///
/// `max_delay` is the player's staleness budget: a group still in flight when a
/// newer one starts is waited on that long before it is skipped.
async fn drain(broadcast: broadcast::Consumer, max_delay: Duration, stats: &Stats) -> anyhow::Result<()> {
	let _gauge = Gauge::inc(&stats.subscriptions);

	let subscription = track::Subscription::default().with_max_delay(max_delay);
	let mut track = broadcast.track(TRACK)?.subscribe(subscription).await?;
	let mut gaps = GapTracker::new(stats);
	let mut learned_shape = false;
	let mut established = None;

	// `recv_group` yields groups in arrival order, including out of sequence, so we
	// can spot holes. `next_group` would silently drop late arrivals and hide them.
	while let Some(mut group) = track.recv_group().await? {
		let sequence = group.sequence;
		match read_group(&mut group, &mut learned_shape, stats).await {
			// Only a group read end to end counts as delivered.
			Ok(()) => {
				if established.is_none() {
					established = Some(Gauge::inc(&stats.subscriptions_established));
				}
				gaps.complete(sequence);
			}
			// The relay failed this one group: `Error::Lagged` once we fall behind.
			Err(err) => {
				gaps.fail(sequence);
				tracing::debug!(sequence, %err, "group ended early");
			}
		}
	}
	Ok(())
}

/// Read one group to its end, counting every frame and sampling the keyframe header.
async fn read_group(group: &mut group::Consumer, learned_shape: &mut bool, stats: &Stats) -> moq_net::Result<()> {
	let mut first = true;
	while let Some(frame) = group.read_frame().await? {
		// The first frame of every group is the JSON keyframe. Parse it once to
		// learn the publisher's shape (we may be watching a peer, not ourselves).
		if first && let Ok(header) = serde_json::from_slice::<RecvHeader>(&frame.payload) {
			stats.latency(header.timestamp_ms);
			if !*learned_shape {
				tracing::debug!(
					fps = header.fps,
					frame_size = header.frame_size,
					group_size = header.group_size,
					"subscribed broadcast shape"
				);
				*learned_shape = true;
			}
		}
		first = false;
		stats.frame_recv(frame.payload.len());
	}
	Ok(())
}

/// Tracks group-sequence continuity for one subscription so we can report skipped
/// groups. Groups arrive out of order, so rather than diffing consecutive sequences
/// we measure how many sequences the received groups span against how many actually
/// landed: a span wider than the count means groups in between never showed up.
///
/// The highest sequence (`max`) is the live frontier and is left out of the
/// accounting: groups just below it may still be in flight (reordered behind the
/// newest stream), so blaming them the instant `max` jumps would be a false
/// positive. We only count up to `cap`, the second-highest sequence seen, which is
/// settled once a higher group has confirmed it. A truly skipped group is counted
/// once the frontier moves past it.
///
/// The span's floor is pinned at the first group the subscription receives. A
/// completed group older than that, such as cached history a relay fills in after
/// the subscribe, is counted in `groups_history` and never widens the span, so a
/// join that over-fetches history does not read as loss.
///
/// A group that explicitly fails is known lost immediately wherever it falls,
/// even below the floor or at the live frontier. Each observation feeds the shared [`Stats`]
/// incrementally so the reporter sees losses live and many subscriptions sum
/// correctly: `groups_expected - groups_present` is the gaps still outstanding.
///
/// `groups_lost` only grows: one observation either fills a counted gap or adds
/// new ones, never both, so summing the gap's increases counts each skipped or
/// failed group once, and a late arrival cannot cancel a later loss.
struct GapTracker<'a> {
	stats: &'a Stats,
	/// First sequence received: the span's floor. `None` until a group arrives.
	floor: Option<u64>,
	max: u64,
	/// Second-highest sequence seen: the settled frontier we count up to. `None`
	/// until a second group arrives.
	cap: Option<u64>,
	/// Number of groups that completed, including the live frontier when it completed.
	complete: u64,
	/// Whether the live frontier completed rather than failed.
	frontier_complete: bool,
	/// Failed groups below the floor: outside the span, but still lost.
	failed_history: u64,
	/// This subscription's current contributions, remembered so each update pushes
	/// only the delta into the shared counters.
	expected: u64,
	present: u64,
}

impl<'a> GapTracker<'a> {
	fn new(stats: &'a Stats) -> Self {
		Self {
			stats,
			floor: None,
			max: 0,
			cap: None,
			complete: 0,
			frontier_complete: false,
			failed_history: 0,
			expected: 0,
			present: 0,
		}
	}

	fn complete(&mut self, sequence: u64) {
		self.stats.groups_recv.fetch_add(1, Ordering::Relaxed);
		self.record(sequence, true);
	}

	fn fail(&mut self, sequence: u64) {
		self.record(sequence, false);
	}

	fn record(&mut self, sequence: u64, complete: bool) {
		let floor = match self.floor {
			None => {
				self.floor = Some(sequence);
				self.max = sequence;
				self.frontier_complete = complete;
				sequence
			}
			Some(floor) => {
				if sequence < floor {
					// History from before the subscription's start: outside the span.
					if complete {
						self.stats.groups_history.fetch_add(1, Ordering::Relaxed);
						return;
					}
					self.failed_history += 1;
				} else if sequence > self.max {
					// New frontier: the old `max` is now settled and becomes the cap.
					self.cap = Some(self.max);
					self.max = sequence;
					self.frontier_complete = complete;
				} else if self.cap.is_none_or(|cap| sequence > cap) {
					self.cap = Some(sequence);
				}
				floor
			}
		};
		self.complete += u64::from(complete);

		// The frontier stays out of inferred spans, but a failed frontier is already
		// known lost, as is a failed history group. Completed groups below the
		// frontier are present.
		let expected =
			self.cap.map_or(0, |cap| cap - floor + 1) + u64::from(!self.frontier_complete) + self.failed_history;
		let present = self.complete - u64::from(self.frontier_complete);
		let grown = (expected - present).saturating_sub(self.expected - self.present);
		// Grow the denominator first and release the counters bounded by it, so a
		// snapshot that acquires them before reading `groups_expected` never sees
		// more lost or present groups than expected.
		self.stats
			.groups_expected
			.fetch_add(expected - self.expected, Ordering::Relaxed);
		self.stats.groups_lost.fetch_add(grown, Ordering::Release);
		self.stats
			.groups_present
			.fetch_add(present - self.present, Ordering::Release);
		self.expected = expected;
		self.present = present;
	}
}

/// Shared only while the producer lives; the connection keeps a weak handle.
/// Connecting and completing the first group may race, but count this broadcast once.
struct Published {
	stats: Arc<Stats>,
	established: AtomicBool,
}

impl Published {
	fn new(stats: Arc<Stats>) -> Arc<Self> {
		Arc::new(Self {
			stats,
			established: AtomicBool::new(false),
		})
	}
	fn establish(&self) {
		if !self.established.swap(true, Ordering::Relaxed) {
			self.stats.broadcasts_established.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for Published {
	fn drop(&mut self) {
		if *self.established.get_mut() {
			self.stats.broadcasts_established.fetch_sub(1, Ordering::Relaxed);
		}
	}
}

/// RAII counter: bumps a gauge on creation and restores it on drop, so a gauge
/// reflects live state even when the owning task is aborted.
struct Gauge<'a>(&'a AtomicU64);

impl<'a> Gauge<'a> {
	fn inc(counter: &'a AtomicU64) -> Self {
		counter.fetch_add(1, Ordering::Relaxed);
		Self(counter)
	}
}

impl Drop for Gauge<'_> {
	fn drop(&mut self) {
		self.0.fetch_sub(1, Ordering::Relaxed);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_broadcast_is_established_once_and_its_gauge_drops_with_the_producer() {
		let stats = Arc::new(Stats::new(crate::stats::Targets {
			broadcasts: 1,
			..Default::default()
		}));
		let published = Published::new(stats.clone());
		assert!(!stats.ready());
		assert_eq!(stats.broadcasts_established.load(Ordering::Relaxed), 0);
		published.establish(); // connected
		published.establish(); // first group
		assert_eq!(stats.broadcasts_established.load(Ordering::Relaxed), 1);
		assert!(stats.ready());
		drop(published);
		assert_eq!(stats.broadcasts_established.load(Ordering::Relaxed), 0);
		assert!(stats.ready());
	}

	#[tokio::test]
	async fn a_subscription_is_established_only_after_a_complete_group() {
		tokio::time::pause();
		let stats = Arc::new(Stats::new(crate::stats::Targets {
			subscriptions: 1,
			..Default::default()
		}));
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, timed()).unwrap();
		let consumer = broadcast.consume();
		let task = {
			let stats = stats.clone();
			tokio::spawn(async move { drain(consumer, Duration::ZERO, &stats).await })
		};
		let mut failed = track.append_group().unwrap();
		failed
			.write_frame(moq_net::Timestamp::now(), Bytes::from_static(b"{}"))
			.unwrap();
		wait_for(&stats.frames_recv, 1).await;
		assert!(!stats.ready());
		assert_eq!(stats.subscriptions.load(Ordering::Relaxed), 1);
		assert_eq!(stats.subscriptions_established.load(Ordering::Relaxed), 0);
		failed.abort(moq_net::Error::Lagged).unwrap();
		let mut complete = track.append_group().unwrap();
		complete
			.write_frame(moq_net::Timestamp::now(), Bytes::from_static(b"{}"))
			.unwrap();
		wait_for(&stats.frames_recv, 2).await;
		assert!(!stats.ready());
		complete.finish().unwrap();
		wait_for(&stats.subscriptions_established, 1).await;
		assert!(stats.ready());
		track.finish().unwrap();
		task.await.unwrap().unwrap();
		assert_eq!(stats.subscriptions_established.load(Ordering::Relaxed), 0);
		assert!(stats.ready());
	}

	#[tokio::test]
	async fn an_idle_producer_counts_before_it_establishes() {
		tokio::time::pause();
		let stats = Arc::new(Stats::new(crate::stats::Targets {
			broadcasts: 1,
			..Default::default()
		}));
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, timed()).unwrap();
		let published = Published::new(stats.clone());
		let connection = Arc::downgrade(&published);
		let task = tokio::spawn(produce(0, "bench/test".into(), rolled(0, 0, 0), track, published));
		wait_for(&stats.broadcasts, 1).await;
		assert_eq!(stats.broadcasts_established.load(Ordering::Relaxed), 0);
		assert!(!stats.ready());
		connection.upgrade().unwrap().establish();
		assert_eq!(stats.broadcasts_established.load(Ordering::Relaxed), 1);
		assert!(stats.ready());
		task.abort();
		assert!(task.await.unwrap_err().is_cancelled());
		assert_eq!(stats.broadcasts.load(Ordering::Relaxed), 0);
		assert_eq!(stats.broadcasts_established.load(Ordering::Relaxed), 0);
		assert!(stats.ready());
	}

	/// Sweep connection count and the broadcast fan-out touched by connection establishment.
	#[test]
	#[ignore = "benchmark, run nightly"]
	#[expect(
		clippy::disallowed_methods,
		clippy::disallowed_types,
		reason = "the benchmark measures real establishment time"
	)]
	fn benchmark_readiness_establishment() {
		for connections in [1, 16, 128] {
			for broadcasts in [1, 16, 128] {
				let count = connections * broadcasts;
				let stats = Arc::new(Stats::new(crate::stats::Targets {
					connections,
					broadcasts: count,
					subscriptions: 0,
				}));
				let publishers: Vec<_> = (0..connections)
					.map(|_| {
						(0..broadcasts)
							.map(|_| Published::new(stats.clone()))
							.collect::<Vec<_>>()
					})
					.collect();
				let weak: Vec<Vec<_>> = publishers
					.iter()
					.map(|connection| connection.iter().map(Arc::downgrade).collect())
					.collect();
				let start = std::time::Instant::now();
				for connection in &weak {
					stats.connections.fetch_add(1, Ordering::Relaxed);
					for published in connection.iter().filter_map(std::sync::Weak::upgrade) {
						published.establish();
					}
				}
				assert!(std::hint::black_box(stats.ready()));
				println!(
					"readiness connections={connections} broadcasts_per_connection={broadcasts} elapsed_ns={}",
					start.elapsed().as_nanos()
				);
				assert_eq!(stats.broadcasts_established.load(Ordering::Relaxed), count);
			}
		}
	}

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	async fn wait_for(counter: &AtomicU64, value: u64) {
		while counter.load(Ordering::Relaxed) < value {
			tokio::task::yield_now().await;
		}
	}

	fn rolled(fps: u64, frame_size: u64, group_size: u64) -> Rolled {
		Rolled {
			broadcasts: 1,
			subscribe: 0,
			fps,
			frame_size,
			group_size,
		}
	}

	fn replay() -> track::Subscription {
		track::Subscription::default().with_max_delay(Duration::from_secs(30))
	}

	/// A produced group must start with the JSON keyframe describing the rolled
	/// parameters, followed by `group_size` zeroed payload frames.
	#[tokio::test]
	async fn produce_keyframe_then_zeroed_payload() {
		tokio::time::pause();

		let stats = Arc::new(Stats::default());
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, timed()).unwrap();
		let consumer = broadcast.consume();

		// 10fps (100ms/frame), 8-byte frames, 2 payload frames per group.
		let task = tokio::spawn(produce(
			7,
			"bench/test".into(),
			rolled(10, 8, 2),
			track,
			Published::new(stats.clone()),
		));

		// Advance past one full group (keyframe + 2 payload) into the next.
		tokio::time::advance(Duration::from_millis(350)).await;

		let mut sub = consumer
			.track(TRACK)
			.unwrap()
			.subscribe(replay())
			.await
			.unwrap()
			.ordered();
		let mut group = sub.next_group().await.unwrap().expect("a group");

		let keyframe = group.read_frame().await.unwrap().expect("keyframe");
		let header: serde_json::Value = serde_json::from_slice(&keyframe.payload).unwrap();
		assert_eq!(header["connection"], 7);
		assert_eq!(header["broadcast"], "bench/test");
		assert_eq!(header["group"], 0);
		assert_eq!(header["fps"], 10);
		assert_eq!(header["frame_size"], 8);
		assert_eq!(header["group_size"], 2);

		for _ in 0..2 {
			let payload = group.read_frame().await.unwrap().expect("payload").payload;
			assert_eq!(payload.len(), 8);
			assert!(payload.iter().all(|&b| b == 0));
		}

		assert!(stats.frames_sent.load(Ordering::Relaxed) >= 3);
		task.abort();
	}

	/// `group_size = 0` is the documented edge case: each group is a lone keyframe.
	#[tokio::test]
	async fn produce_zero_group_size_is_keyframe_only() {
		tokio::time::pause();

		let stats = Arc::new(Stats::default());
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, timed()).unwrap();
		let consumer = broadcast.consume();

		let task = tokio::spawn(produce(
			0,
			"bench/test".into(),
			rolled(10, 4, 0),
			track,
			Published::new(stats.clone()),
		));
		tokio::time::advance(Duration::from_millis(250)).await;

		let mut sub = consumer
			.track(TRACK)
			.unwrap()
			.subscribe(replay())
			.await
			.unwrap()
			.ordered();
		let mut group = sub.next_group().await.unwrap().expect("a group");

		// Just the keyframe, then the group ends.
		assert!(group.read_frame().await.unwrap().is_some(), "keyframe");
		assert!(group.read_frame().await.unwrap().is_none(), "no payload frames");

		task.abort();
	}

	/// A lone-keyframe group (`group_size = 0`) must still cost `frame_size`
	/// bytes: the keyframe is padded via its `pad` field, and stays valid JSON.
	/// Without this, the chat presets' frame_size setting had no effect at all.
	#[tokio::test]
	async fn keyframe_padded_to_frame_size() {
		tokio::time::pause();

		let stats = Arc::new(Stats::default());
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, timed()).unwrap();
		let consumer = broadcast.consume();

		// 10fps, 300-byte messages, lone-keyframe groups (the chat shape).
		let task = tokio::spawn(produce(
			3,
			"bench/test".into(),
			rolled(10, 300, 0),
			track,
			Published::new(stats.clone()),
		));
		tokio::time::advance(Duration::from_millis(250)).await;

		let mut sub = consumer.track(TRACK).unwrap().subscribe(None).await.unwrap().ordered();
		let mut group = sub.next_group().await.unwrap().expect("a group");
		let keyframe = group.read_frame().await.unwrap().expect("keyframe").payload;

		assert_eq!(keyframe.len(), 300, "keyframe padded to the rolled frame size");
		let header: serde_json::Value = serde_json::from_slice(&keyframe).expect("padded keyframe is valid JSON");
		assert_eq!(header["frame_size"], 300);

		task.abort();
	}

	/// A frame size below the JSON header's own length is a floor, not an error:
	/// the keyframe goes out at its natural size and still parses. The chat
	/// presets stay above the floor so their configured sizes hold exactly.
	#[tokio::test]
	async fn keyframe_below_header_floor_is_unpadded() {
		tokio::time::pause();

		let stats = Arc::new(Stats::default());
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, timed()).unwrap();
		let consumer = broadcast.consume();

		// 50 bytes is well under the serialized header (roughly 170 bytes).
		let task = tokio::spawn(produce(
			3,
			"bench/test".into(),
			rolled(10, 50, 0),
			track,
			Published::new(stats.clone()),
		));
		tokio::time::advance(Duration::from_millis(250)).await;

		let mut sub = consumer.track(TRACK).unwrap().subscribe(None).await.unwrap().ordered();
		let mut group = sub.next_group().await.unwrap().expect("a group");
		let keyframe = group.read_frame().await.unwrap().expect("keyframe").payload;

		assert!(keyframe.len() > 50, "header is the floor; no truncation to fit");
		let header: serde_json::Value = serde_json::from_slice(&keyframe).expect("unpadded keyframe is valid JSON");
		assert_eq!(header["pad"], "");

		task.abort();
	}

	/// Discovery must skip broadcasts outside the bench namespace: a relay with
	/// stats publishing enabled announces `.stats/...` too, and a subscription
	/// slot burned on it is never retried, so a chat-shaped run (`subscribe = 1`)
	/// used to end up with zero working subscriptions.
	#[tokio::test]
	async fn subscribe_ignores_relay_internal_broadcasts() {
		tokio::time::pause();

		let stats = Arc::new(Stats::default());
		let origin = moq_tokio::origin::spawn();

		// The relay-internal broadcast: announced, but with no bench data track.
		let _internal = origin.create_broadcast(".stats/node/host").unwrap();
		_internal.announce(Default::default()).unwrap();

		// Our own broadcast: in the namespace, but excluded via the `own` set
		// (paths relative to the namespace, matching the scoped announce consumer).
		let _previous = origin.create_broadcast("bench/previous/0/0").unwrap();
		_previous.announce(Default::default()).unwrap();

		let _own = origin.create_broadcast("bench/current/9/9").unwrap();
		_own.announce(Default::default()).unwrap();
		let own = HashSet::from(["9/9".to_string()]);

		// One legitimate peer under the bench namespace with a single finished group.
		let peer = origin.create_broadcast("bench/current/0/0").unwrap();
		peer.announce(Default::default()).unwrap();
		let track = peer.create_track(TRACK, timed()).unwrap();
		let mut group = track.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::now(), Bytes::from_static(b"{}"))
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let announced = discover(&origin, "bench/current");
		subscribe(announced, own, 1, Duration::ZERO, Duration::ZERO, stats.clone())
			.await
			.unwrap();

		// The one wanted slot went to the peer, not `.stats` and not our own.
		assert_eq!(stats.frames_recv.load(Ordering::Relaxed), 1);
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 1);
	}

	#[tokio::test]
	async fn named_subscription_waits_for_the_exact_broadcast() {
		let stats = Arc::new(Stats::default());
		let origin = moq_tokio::origin::spawn();
		let consume = origin.consume();
		let task = tokio::spawn(subscribe_named(
			consume,
			"bench/run/chat".into(),
			Duration::ZERO,
			stats.clone(),
		));

		let broadcast = origin.create_broadcast("bench/run/chat").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let track = broadcast.create_track(TRACK, timed()).unwrap();
		tokio::task::yield_now().await;
		let mut group = track.append_group().unwrap();
		let header = serde_json::json!({
			"fps": 1,
			"frame_size": 200,
			"group_size": 0,
			"timestamp_ms": 0,
		});
		group
			.write_frame(moq_net::Timestamp::now(), serde_json::to_vec(&header).unwrap())
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		task.await.unwrap().unwrap();
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 1);
		broadcast.close();
	}

	/// The relay fails a group it gave up on (`Error::Lagged` once a subscriber
	/// falls behind). That ends the group, not the subscription: the drain must
	/// keep consuming later groups, and charge the failed one as a gap. Treating
	/// it as terminal shrank the offered load as a run went on, so the relay was
	/// measured under fewer subscribers than it was asked to serve.
	#[tokio::test]
	async fn drain_survives_a_failed_group() {
		fn write_group(track: &mut track::Producer) {
			let mut group = track.append_group().unwrap();
			group
				.write_frame(moq_net::Timestamp::now(), Bytes::from_static(b"{}"))
				.unwrap();
			group.finish().unwrap();
		}

		let stats = Arc::new(Stats::default());
		let broadcast = broadcast::Info::new().produce();
		let mut track = broadcast.create_track(TRACK, timed()).unwrap();
		let consumer = broadcast.consume();

		// Group 0 lands intact.
		write_group(&mut track);

		let task = {
			let stats = stats.clone();
			tokio::spawn(async move { drain(consumer, Duration::ZERO, &stats).await })
		};
		// A zero max delay starts at the live frontier. Let the drain consume
		// group 0 before opening group 1, or the task may subscribe to group 1
		// and the frame count can never reach two.
		wait_for(&stats.frames_recv, 1).await;

		// Group 1 opens and is picked up by the drain, then the relay gives up on it.
		// Aborting a group nobody is reading drops its cached frames, so wait for the
		// drain to be parked inside it before failing it.
		let mut group = track.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::now(), Bytes::from_static(b"{}"))
			.unwrap();
		wait_for(&stats.frames_recv, 2).await;
		group.abort(moq_net::Error::Lagged).unwrap();

		// Groups 2 and 3 land intact, then the publisher is done.
		write_group(&mut track);
		wait_for(&stats.groups_recv, 2).await;
		write_group(&mut track);
		wait_for(&stats.groups_recv, 3).await;
		track.finish().unwrap();
		broadcast.close();

		task.await
			.unwrap()
			.expect("a failed group must not end the subscription");

		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 3, "the three intact groups");
		assert_eq!(lost(&stats), 1, "the failed group counts as a gap");
	}

	/// Drain a group that a newer one overtook, then report the groups lost.
	///
	/// The drain is parked in group 0 while groups 1 and 2 are written, so when it
	/// asks for the next group, group 1 already trails group 2 by 500ms of media
	/// time. Timestamps are explicit, so no wall clock is involved.
	async fn lost_to_an_overtaken_group(max_delay: Duration) -> u64 {
		fn write_group(track: &mut track::Producer, millis: &[u64]) {
			let mut group = track.append_group().unwrap();
			for &millis in millis {
				let at = moq_net::Timestamp::from_millis(millis).unwrap();
				group.write_frame(at, Bytes::from_static(b"{}")).unwrap();
			}
			group.finish().unwrap();
		}

		let stats = Arc::new(Stats::default());
		let broadcast = broadcast::Info::new().produce();
		let mut track = broadcast.create_track(TRACK, timed()).unwrap();
		let consumer = broadcast.consume();

		let task = {
			let stats = stats.clone();
			tokio::spawn(async move { drain(consumer, max_delay, &stats).await })
		};

		let mut first = track.append_group().unwrap();
		let at = moq_net::Timestamp::from_millis(1_000).unwrap();
		first.write_frame(at, Bytes::from_static(b"{}")).unwrap();
		wait_for(&stats.frames_recv, 1).await;

		write_group(&mut track, &[1_100]);
		write_group(&mut track, &[1_500, 1_600]);
		first.finish().unwrap();
		wait_for(&stats.groups_recv, 2).await;

		// A last group moves the live frontier past group 2, so 1 and 2 are accounted.
		write_group(&mut track, &[2_000]);
		track.finish().unwrap();
		broadcast.close();

		task.await.unwrap().unwrap();
		lost(&stats)
	}

	/// A player waits out a late group within its budget. A zero budget skips group 1
	/// as soon as group 2 is ahead of it, which charged short groups as loss that no
	/// player would see.
	#[tokio::test]
	async fn drain_waits_for_a_late_group_within_max_delay() {
		assert_eq!(lost_to_an_overtaken_group(Duration::from_secs(2)).await, 0);
		assert_eq!(lost_to_an_overtaken_group(Duration::ZERO).await, 1);
	}

	/// Subscription targets must be picked at random from the announced stream.
	/// Announcements replay in deterministic path order, so a first-come pick
	/// put every subscriber on the same first rooms and turned the 1:N presets
	/// into hotspots on one or two broadcasts.
	#[test]
	fn reservoir_spreads_selections() {
		let mut distinct = HashSet::new();
		for _ in 0..64 {
			let mut pool = Vec::new();
			for item in 0..8 {
				reservoir_push(&mut pool, 1, item + 1, item);
			}
			distinct.insert(pool[0]);
		}
		// First-come always yields element 0. Randomness missing this over 64
		// draws from 8 elements has probability (1/8)^63, i.e. never.
		assert!(distinct.len() > 1, "selection must not be deterministic");

		// Fewer eligible items than want: everything survives.
		let mut pool = Vec::new();
		reservoir_push(&mut pool, 5, 1, 1);
		reservoir_push(&mut pool, 5, 2, 2);
		assert_eq!(pool, vec![1, 2]);
	}

	fn lost(stats: &Stats) -> u64 {
		stats.groups_lost.load(Ordering::Relaxed)
	}

	fn outstanding(stats: &Stats) -> u64 {
		stats.groups_expected.load(Ordering::Relaxed) - stats.groups_present.load(Ordering::Relaxed)
	}

	/// A hole below the frontier (group 2 never arrives, but 3 and 4 do) is one skip.
	#[test]
	fn gap_tracker_counts_skips() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);
		for seq in [0, 1, 3, 4] {
			gaps.complete(seq);
		}
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 4);
		assert_eq!(lost(&stats), 1);
	}

	/// A group that lands after the frontier settled past it closes the gap but
	/// stays lost, so recovering an old gap cannot hide a new loss.
	#[test]
	fn gap_tracker_late_arrival_stays_lost() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);

		for seq in [0, 1, 3, 4] {
			gaps.complete(seq);
		}
		assert_eq!((lost(&stats), outstanding(&stats)), (1, 1));

		gaps.complete(2);
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 5);
		assert_eq!((lost(&stats), outstanding(&stats)), (1, 0), "the late group stays lost");

		// 6 is skipped; 8 settles 7, confirming it.
		for seq in [5, 7, 8] {
			gaps.complete(seq);
		}
		assert_eq!((lost(&stats), outstanding(&stats)), (2, 1));
	}

	/// Out-of-order arrivals that fill the whole span count as zero loss.
	#[test]
	fn gap_tracker_handles_out_of_order() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);
		for seq in [0, 3, 1, 2, 4] {
			gaps.complete(seq);
		}
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 5);
		assert_eq!((lost(&stats), outstanding(&stats)), (0, 0));
	}

	/// Groups older than the first one received are history a relay filled in after
	/// the subscribe: counted apart, never as loss. Gaps at or after the start still count.
	#[test]
	fn gap_tracker_counts_history_apart() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);
		let history = |stats: &Stats| stats.groups_history.load(Ordering::Relaxed);

		// Joins live at 10; 11 and 12 settle the span from 10.
		for seq in [10, 11, 12] {
			gaps.complete(seq);
		}
		assert_eq!(stats.groups_expected.load(Ordering::Relaxed), 2);

		// Cached history arrives behind the live frontier, leaving 0..=4 and 6 unsent.
		for seq in [5, 7, 8, 9] {
			gaps.complete(seq);
		}
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 7);
		assert_eq!(history(&stats), 4);
		assert_eq!(
			(lost(&stats), outstanding(&stats)),
			(0, 0),
			"history never widens the span"
		);
		assert_eq!(stats.groups_expected.load(Ordering::Relaxed), 2);

		// A skip after the start is still loss: 13 never arrives, 15 settles 14.
		for seq in [14, 15] {
			gaps.complete(seq);
		}
		assert_eq!((lost(&stats), outstanding(&stats)), (1, 1));

		// A failed history group is lost wherever it falls.
		gaps.fail(3);
		assert_eq!(history(&stats), 4, "a failed group is not history");
		assert_eq!((lost(&stats), outstanding(&stats)), (2, 2));
	}

	/// The newest group is the live frontier: a hole directly behind it isn't blamed
	/// yet (it may still be in flight), but once the frontier moves past, it counts.
	#[test]
	fn gap_tracker_excludes_live_frontier() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);

		// 3 is missing, 4 is the live frontier: nothing is settled past 2 yet.
		for seq in [0, 1, 2, 4] {
			gaps.complete(seq);
		}
		assert_eq!(lost(&stats), 0);

		// 5 advances the frontier, settling 4 and confirming 3 was skipped.
		gaps.complete(5);
		assert_eq!(lost(&stats), 1);
	}

	/// An explicitly failed first group is lost even before a frontier can settle it.
	#[test]
	fn gap_tracker_counts_failed_first_group() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);

		gaps.fail(0);
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 0);
		assert_eq!(lost(&stats), 1);

		gaps.complete(1);
		assert_eq!(
			lost(&stats),
			1,
			"advancing the frontier must not count the failure twice"
		);
	}

	/// An explicitly failed live frontier is lost without waiting for another group.
	#[test]
	fn gap_tracker_counts_failed_final_group() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);

		gaps.complete(0);
		gaps.fail(1);
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 1);
		assert_eq!(lost(&stats), 1);

		gaps.complete(2);
		assert_eq!(
			lost(&stats),
			1,
			"advancing the frontier must not count the failure twice"
		);
	}
}
