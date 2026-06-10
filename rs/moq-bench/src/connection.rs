use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moq_native::Status;
use moq_native::moq_net::{self, Broadcast, BroadcastConsumer, Origin, Track, TrackProducer, bytes::Bytes};
use rand::RngExt;
use serde::Serialize;
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

	let url = config.url.clone().expect("url required");

	// Publish side: an origin we fill with our broadcasts and hand to the session.
	let publish = Origin::random().produce();
	// Consume side: the session fills this with peer announcements.
	let consume = Origin::random().produce();
	let announced = consume.consume();

	let name = config.name();
	let mut broadcasts = Vec::new();
	let mut own = HashSet::new();
	let mut tasks = JoinSet::new();

	for index in 0..rolled.broadcasts {
		let path = format!("{name}/{run_id:08x}/{connection}/{index}");

		let mut broadcast = Broadcast::new().produce();
		let track = match broadcast.create_track(Track::new(TRACK)) {
			Ok(track) => track,
			Err(err) => {
				tracing::error!(connection, %err, "failed to create track");
				continue;
			}
		};

		publish.publish_broadcast(&path, broadcast.consume());
		own.insert(path.clone());
		// Hold the broadcast producer for the connection's lifetime so it stays announced.
		broadcasts.push(broadcast);

		let stats = stats.clone();
		tasks.spawn(produce(connection, path, rolled, track, stats));
	}

	let client = client.with_publish(publish.consume()).with_consume(consume);
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
	mut track: TrackProducer,
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
		group.write_frame(header.clone())?;
		stats.frame_sent(header.len());

		// The remaining frames in the group are zeroed payload.
		for _ in 0..rolled.group_size {
			ticker.tick().await;
			group.write_frame(zeros.clone())?;
			stats.frame_sent(zeros.len());
		}

		group.finish()?;
		sequence += 1;
	}
}

/// Watch announcements and drain up to `want` peer broadcasts (excluding our own),
/// spreading each subscription's start over `startup` to avoid a thundering herd.
async fn subscribe(
	mut announced: moq_net::OriginConsumer,
	own: HashSet<String>,
	want: u64,
	startup: Duration,
	stats: Arc<Stats>,
) -> anyhow::Result<()> {
	let mut tasks = JoinSet::new();
	let mut seen: HashSet<String> = HashSet::new();

	while (seen.len() as u64) < want {
		let Some((path, broadcast)) = announced.announced().await else {
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

/// Subscribe to the broadcast's track and count every frame received.
async fn drain(broadcast: BroadcastConsumer, stats: &Stats) -> anyhow::Result<()> {
	let _gauge = Gauge::inc(&stats.subscriptions);

	let mut track = broadcast.subscribe_track(&Track::new(TRACK))?;
	while let Some(mut group) = track.next_group().await? {
		while let Some(frame) = group.read_frame().await? {
			stats.frame_recv(frame.len());
		}
	}
	Ok(())
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
		let mut broadcast = Broadcast::new().produce();
		let track = broadcast.create_track(Track::new(TRACK)).unwrap();
		let consumer = broadcast.consume();

		// 10fps (100ms/frame), 8-byte frames, 2 payload frames per group.
		let task = tokio::spawn(produce(7, "bench/test".into(), rolled(10, 8, 2), track, stats.clone()));

		// Advance past one full group (keyframe + 2 payload) into the next.
		tokio::time::advance(Duration::from_millis(350)).await;

		let mut sub = consumer.subscribe_track(&Track::new(TRACK)).unwrap();
		let mut group = sub.next_group().await.unwrap().expect("a group");

		let keyframe = group.read_frame().await.unwrap().expect("keyframe");
		let header: serde_json::Value = serde_json::from_slice(&keyframe).unwrap();
		assert_eq!(header["connection"], 7);
		assert_eq!(header["broadcast"], "bench/test");
		assert_eq!(header["group"], 0);
		assert_eq!(header["fps"], 10);
		assert_eq!(header["frame_size"], 8);
		assert_eq!(header["group_size"], 2);

		for _ in 0..2 {
			let payload = group.read_frame().await.unwrap().expect("payload");
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
		let mut broadcast = Broadcast::new().produce();
		let track = broadcast.create_track(Track::new(TRACK)).unwrap();
		let consumer = broadcast.consume();

		let task = tokio::spawn(produce(0, "bench/test".into(), rolled(10, 4, 0), track, stats.clone()));
		tokio::time::advance(Duration::from_millis(250)).await;

		let mut sub = consumer.subscribe_track(&Track::new(TRACK)).unwrap();
		let mut group = sub.next_group().await.unwrap().expect("a group");

		// Just the keyframe, then the group ends.
		assert!(group.read_frame().await.unwrap().is_some(), "keyframe");
		assert!(group.read_frame().await.unwrap().is_none(), "no payload frames");

		task.abort();
	}
}
