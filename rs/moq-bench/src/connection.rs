use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moq_native::Status;
use moq_native::moq_net::{self, Origin, bytes::Bytes};
use moq_native::moq_net::{broadcast, track};
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
}

/// The subset of [`GroupHeader`] a subscriber reads back to learn the shape of a
/// broadcast it didn't publish. Extra fields on the wire are ignored.
#[derive(Deserialize)]
struct RecvHeader {
	fps: u64,
	frame_size: u64,
	group_size: u64,
}

/// Everything one benchmark connection needs to run: its identity, the rolled
/// parameters, and the shared client/stats handles. Bundled into a struct so
/// `run` and its call site aren't drowning in positional arguments.
pub struct Connection {
	pub index: u64,
	pub run_id: u64,
	pub rolled: Rolled,
	pub config: Arc<crate::Config>,
	pub client: moq_native::Client,
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
		rolled,
		config,
		client,
		stats,
	} = ctx;

	let url = config.client.connect.clone().expect("url required");

	// Publish side: an origin we fill with our broadcasts and hand to the session.
	let publish = Origin::random().produce();
	// Consume side: the session fills this with peer announcements.
	let consume = Origin::random().produce();
	let announced = consume.consume().announced();

	let name = config.name();
	let mut broadcasts = Vec::new();
	let mut own = HashSet::new();
	let mut tasks = JoinSet::new();

	for index in 0..rolled.broadcasts {
		let path = format!("{name}/{run_id:08x}/{connection}/{index}");

		let mut broadcast = match publish.create_broadcast(&path, broadcast::Route::new().with_announce(true)) {
			Ok(broadcast) => broadcast,
			Err(err) => {
				tracing::error!(connection, %err, "failed to create broadcast");
				continue;
			}
		};
		let track = match broadcast.create_track(TRACK, None) {
			Ok(track) => track,
			Err(err) => {
				tracing::error!(connection, %err, "failed to create track");
				continue;
			}
		};
		own.insert(path.clone());
		// Hold the broadcast producer for the connection's lifetime so it stays announced.
		broadcasts.push(broadcast);

		let stats = stats.clone();
		tasks.spawn(produce(connection, path, rolled, track, stats));
	}

	let client = client.with_publisher(&publish).with_subscriber(consume);
	let mut reconnect = client.reconnect(url);

	// Subscriber: drain up to `subscribe` peer broadcasts.
	if rolled.subscribe > 0 {
		tasks.spawn(subscribe(
			announced,
			own,
			rolled.subscribe,
			config.startup(),
			stats.clone(),
		));
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
	mut track: track::Producer,
	stats: Arc<Stats>,
) -> anyhow::Result<()> {
	let _gauge = Gauge::inc(&stats.broadcasts);

	// Zero fps means an idle track: keep it published but never produce.
	if rolled.fps == 0 {
		std::future::pending::<()>().await;
		return Ok(());
	}

	let zeros = Bytes::from(vec![0u8; rolled.frame_size as usize]);
	let period = Duration::from_secs_f64(1.0 / rolled.fps as f64);
	let mut ticker = tokio::time::interval(period);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	let mut sequence = 0u64;
	loop {
		let mut group = track.append_group()?;

		// Keyframe: the JSON header describing this connection's rolled parameters.
		ticker.tick().await;
		let header = GroupHeader {
			connection,
			broadcast: &path,
			group: sequence,
			fps: rolled.fps,
			frame_size: rolled.frame_size,
			group_size: rolled.group_size,
			broadcasts: rolled.broadcasts,
			subscribe: rolled.subscribe,
			timestamp_ms: SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.unwrap_or_default()
				.as_millis(),
		};
		let header = Bytes::from(serde_json::to_vec(&header)?);
		group.write_frame(moq_net::Timestamp::now(), header.clone())?;
		stats.frame_sent(header.len());

		// The remaining frames in the group are zeroed payload.
		for _ in 0..rolled.group_size {
			ticker.tick().await;
			group.write_frame(moq_net::Timestamp::now(), zeros.clone())?;
			stats.frame_sent(zeros.len());
		}

		group.finish()?;
		sequence += 1;
	}
}

/// Watch announcements and drain up to `want` peer broadcasts (excluding our own),
/// spreading each subscription's start over `startup` to avoid a thundering herd.
async fn subscribe(
	mut announced: moq_net::announce::Consumer,
	own: HashSet<String>,
	want: u64,
	startup: Duration,
	stats: Arc<Stats>,
) -> anyhow::Result<()> {
	let mut tasks = JoinSet::new();
	let mut seen: HashSet<String> = HashSet::new();

	while (seen.len() as u64) < want {
		let Some(moq_net::announce::Update { path, broadcast }) = announced.next().await else {
			break;
		};
		let Some(broadcast) = broadcast else {
			continue;
		};

		let path = path.as_str().to_string();
		if own.contains(&path) || !seen.insert(path.clone()) {
			continue;
		}

		// Stagger the subscription start somewhere within the startup window.
		let delay = {
			let mut rng = rand::rng();
			startup.mul_f64(rng.random_range(0.0..1.0))
		};

		let stats = stats.clone();
		tasks.spawn(async move {
			tokio::time::sleep(delay).await;
			if let Err(err) = drain(broadcast, &stats).await {
				tracing::debug!(%path, %err, "subscription ended");
			}
		});
	}

	// Keep the drain tasks alive; they run until their broadcasts close.
	while tasks.join_next().await.is_some() {}
	Ok(())
}

/// Subscribe to the broadcast's track, counting every frame received and tracking
/// group-sequence gaps to report skipped groups.
async fn drain(broadcast: broadcast::Consumer, stats: &Stats) -> anyhow::Result<()> {
	let _gauge = Gauge::inc(&stats.subscriptions);

	let mut track = broadcast.track(TRACK)?.subscribe(None).await?;
	let mut gaps = GapTracker::new(stats);
	let mut learned_shape = false;

	// `recv_group` yields groups in arrival order, including out of sequence, so we
	// can spot holes. `next_group` would silently drop late arrivals and hide them.
	while let Some(mut group) = track.recv_group().await? {
		gaps.observe(group.sequence);

		let mut first = true;
		while let Some(frame) = group.read_frame().await? {
			// The first frame of every group is the JSON keyframe. Parse it once to
			// learn the publisher's shape (we may be watching a peer, not ourselves).
			if first && !learned_shape {
				if let Ok(header) = serde_json::from_slice::<RecvHeader>(&frame.payload) {
					tracing::debug!(
						fps = header.fps,
						frame_size = header.frame_size,
						group_size = header.group_size,
						"subscribed broadcast shape"
					);
					learned_shape = true;
				}
			}
			first = false;
			stats.frame_recv(frame.payload.len());
		}
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
/// Each observation feeds the shared [`Stats`] incrementally so the reporter sees
/// losses live and many subscriptions sum correctly: `groups_expected` is the size
/// of every settled span, `groups_present` is how many of those groups arrived, and
/// `groups_expected - groups_present` is the total skipped.
struct GapTracker<'a> {
	stats: &'a Stats,
	min: u64,
	max: u64,
	/// Second-highest sequence seen: the settled frontier we count up to. `None`
	/// until a second group arrives.
	cap: Option<u64>,
	/// This subscription's current contribution to `groups_expected` (`cap - min + 1`),
	/// remembered so each update pushes only the delta.
	expected: u64,
	started: bool,
}

impl<'a> GapTracker<'a> {
	fn new(stats: &'a Stats) -> Self {
		Self {
			stats,
			min: 0,
			max: 0,
			cap: None,
			expected: 0,
			started: false,
		}
	}

	fn observe(&mut self, sequence: u64) {
		self.stats.groups_recv.fetch_add(1, Ordering::Relaxed);

		// The first group is the lone frontier: nothing settled yet, so it doesn't count.
		if !self.started {
			self.started = true;
			self.min = sequence;
			self.max = sequence;
			return;
		}

		// Every later group sits at or below the settled frontier, so it's a present group.
		self.stats.groups_present.fetch_add(1, Ordering::Relaxed);

		self.min = self.min.min(sequence);
		if sequence > self.max {
			// New frontier: the old `max` is now settled and becomes the cap.
			self.cap = Some(self.max);
			self.max = sequence;
		} else if self.cap.is_none_or(|cap| sequence > cap) {
			self.cap = Some(sequence);
		}

		// `cap` is always set here: either the branch above set it, or a prior group did.
		let cap = self.cap.expect("cap set once a second group arrives");
		let expected = cap - self.min + 1;
		self.stats
			.groups_expected
			.fetch_add(expected - self.expected, Ordering::Relaxed);
		self.expected = expected;
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

	fn rolled(fps: u64, frame_size: u64, group_size: u64) -> Rolled {
		Rolled {
			broadcasts: 1,
			subscribe: 0,
			fps,
			frame_size,
			group_size,
		}
	}

	/// A produced group must start with the JSON keyframe describing the rolled
	/// parameters, followed by `group_size` zeroed payload frames.
	#[tokio::test]
	async fn produce_keyframe_then_zeroed_payload() {
		tokio::time::pause();

		let stats = Arc::new(Stats::default());
		let mut broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, None).unwrap();
		let consumer = broadcast.consume();

		// 10fps (100ms/frame), 8-byte frames, 2 payload frames per group.
		let task = tokio::spawn(produce(7, "bench/test".into(), rolled(10, 8, 2), track, stats.clone()));

		// Advance past one full group (keyframe + 2 payload) into the next.
		tokio::time::advance(Duration::from_millis(350)).await;

		let mut sub = consumer.track(TRACK).unwrap().subscribe(None).await.unwrap();
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
		let mut broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track(TRACK, None).unwrap();
		let consumer = broadcast.consume();

		let task = tokio::spawn(produce(0, "bench/test".into(), rolled(10, 4, 0), track, stats.clone()));
		tokio::time::advance(Duration::from_millis(250)).await;

		let mut sub = consumer.track(TRACK).unwrap().subscribe(None).await.unwrap();
		let mut group = sub.next_group().await.unwrap().expect("a group");

		// Just the keyframe, then the group ends.
		assert!(group.read_frame().await.unwrap().is_some(), "keyframe");
		assert!(group.read_frame().await.unwrap().is_none(), "no payload frames");

		task.abort();
	}

	fn lost(stats: &Stats) -> u64 {
		stats
			.groups_expected
			.load(Ordering::Relaxed)
			.saturating_sub(stats.groups_present.load(Ordering::Relaxed))
	}

	/// A hole below the frontier (group 2 never arrives, but 3 and 4 do) is one skip.
	#[test]
	fn gap_tracker_counts_skips() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);
		for seq in [0, 1, 3, 4] {
			gaps.observe(seq);
		}
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 4);
		assert_eq!(lost(&stats), 1);
	}

	/// Out-of-order arrivals that fill the whole span count as zero loss.
	#[test]
	fn gap_tracker_handles_out_of_order() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);
		for seq in [2, 0, 1, 3] {
			gaps.observe(seq);
		}
		assert_eq!(stats.groups_recv.load(Ordering::Relaxed), 4);
		assert_eq!(lost(&stats), 0);
	}

	/// The newest group is the live frontier: a hole directly behind it isn't blamed
	/// yet (it may still be in flight), but once the frontier moves past, it counts.
	#[test]
	fn gap_tracker_excludes_live_frontier() {
		let stats = Stats::default();
		let mut gaps = GapTracker::new(&stats);

		// 3 is missing, 4 is the live frontier: nothing is settled past 2 yet.
		for seq in [0, 1, 2, 4] {
			gaps.observe(seq);
		}
		assert_eq!(lost(&stats), 0);

		// 5 advances the frontier, settling 4 and confirming 3 was skipped.
		gaps.observe(5);
		assert_eq!(lost(&stats), 1);
	}
}
