//! Measured link costs route around a lossy backbone edge.
//!
//! Three relays on one host, every inter-relay link through a userspace UDP
//! shaper (`support::shaper`): sjc-dal 50 ms, dal-nyc 60 ms, sjc-nyc 110 ms.
//! A broadcast published at sjc and watched at nyc takes the direct link while
//! it is clean. With 1% loss on it the price sjc measures for the link climbs,
//! the cluster moves the route to sjc -> dal -> nyc at a group boundary, and the
//! detour log names the edge.

mod support;

use std::{
	net::SocketAddr,
	sync::{Arc, Mutex, OnceLock},
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use moq_net::Hop;
use moq_relay::{Config, Relay, cluster::Peer};
use support::shaper::{Profile, Shaper};

const TIMEOUT: Duration = Duration::from_secs(60);
const PATH: &str = "testbed/cam";
const TRACK: &str = "video";
/// The publisher's frame cadence and size: about 2.5 Mbit/s, so 1% loss is
/// seen within a couple of sampling intervals rather than minutes.
const FRAME_INTERVAL: Duration = Duration::from_millis(25);
const FRAME_BYTES: usize = 8 * 1024;
const FRAMES_PER_GROUP: u64 = 40;

/// Everything the relays log, so a test can assert the detour line.
static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

struct LogSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		self.0.lock().expect("log sink poisoned").extend_from_slice(buf);
		// `CLUSTER_COST_LOG=<path>` also streams the log to a file, for a run
		// the harness kills before an assertion can dump it.
		if let Ok(path) = std::env::var("CLUSTER_COST_LOG")
			&& let Ok(mut file) = std::fs::OpenOptions::new().append(true).create(true).open(path)
		{
			let _ = file.write_all(buf);
		}
		Ok(buf.len())
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}

fn capture_logs() -> Arc<Mutex<Vec<u8>>> {
	LOGS.get_or_init(|| {
		let buf = Arc::new(Mutex::new(Vec::new()));
		let sink = buf.clone();
		// `RUST_LOG` widens the capture when a run needs the protocol's own trace.
		let filter = tracing_subscriber::EnvFilter::try_from_default_env()
			.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
		let _ = tracing_subscriber::fmt()
			.with_env_filter(filter)
			.with_ansi(false)
			.with_writer(move || LogSink(sink.clone()))
			.try_init();
		buf
	})
	.clone()
}

fn logs() -> String {
	String::from_utf8_lossy(&capture_logs().lock().expect("log sink poisoned")).into_owned()
}

struct RelayHost {
	id: u64,
	addr: SocketAddr,
	task: tokio::task::JoinHandle<()>,
}

impl Drop for RelayHost {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// A relay on QUIC with a generated certificate, dialing `peers` (shaper
/// addresses) and pricing its links every 250 ms so the test converges fast.
async fn spawn_relay(id: u64, peers: &[SocketAddr]) -> RelayHost {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	capture_logs();

	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".to_string());
	config.listen.tls.generate = vec!["localhost".into()];
	config.connect.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.connect.tls.insecure = Some(true);
	config.connect.websocket.enabled = Some(false);
	// Route costs ride moq-lite-06; the default negotiation still settles on
	// lite-05 between relays, which carries hop counts only.
	config.connect.version = vec![lite_06()];
	config.listen.version = vec![lite_06()];
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.cluster.id = Some(id);
	config.cluster.connect = peers.iter().map(|addr| Peer::new(format!("https://{addr}/"))).collect();
	config.cluster.cost.interval = Some(Duration::from_millis(250).into());

	let relay = Relay::load(config).await.expect("relay load");
	let addr = relay.addr().expect("relay did not bind UDP");
	let task = tokio::spawn(async move {
		let _ = relay.run().await;
	});
	RelayHost { id, addr, task }
}

/// The wire that carries route costs; every session in the test speaks it.
fn lite_06() -> moq_net::Version {
	"moq-lite-06-wip".parse().expect("parse version")
}

fn relay_url(addr: SocketAddr) -> url::Url {
	format!("https://{addr}/").parse().expect("parse url")
}

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.tls.insecure = Some(true);
	config.websocket.enabled = Some(false);
	config.version = vec![lite_06()];
	config.init(Default::default()).expect("client init")
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis() as u64
}

struct Publisher {
	_broadcast: moq_net::broadcast::Producer,
	_connection: moq_tokio::Connection,
	streamer: tokio::task::AbortHandle,
}

impl Drop for Publisher {
	fn drop(&mut self) {
		self.streamer.abort();
	}
}

/// Publish [`PATH`] at `relay`: a group every second of 8 KiB frames stamped
/// with the wall clock, so a subscriber can age them.
async fn publish(relay: &RelayHost) -> Publisher {
	let origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = origin.create_broadcast(PATH).expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let mut track = broadcast.create_track(TRACK, None).expect("create track");

	let streamer = tokio::spawn(async move {
		let mut ticker = tokio::time::interval(FRAME_INTERVAL);
		ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
		let mut payload = vec![0u8; FRAME_BYTES];
		loop {
			let Ok(mut group) = track.append_group() else { break };
			for _ in 0..FRAMES_PER_GROUP {
				ticker.tick().await;
				payload[..8].copy_from_slice(&now_ms().to_be_bytes());
				if group.write_frame(moq_net::Timestamp::ZERO, payload.clone()).is_err() {
					return;
				}
			}
			if group.finish().is_err() {
				break;
			}
		}
	})
	.abort_handle();

	let connection = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_publisher(&origin)
			.connect(relay_url(relay.addr))
			.established(),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	Publisher {
		_broadcast: broadcast,
		_connection: connection,
		streamer,
	}
}

/// A subscriber at `relay`, reading [`PATH`] so data actually crosses the
/// links, and reporting the hop chain of each route update it sees.
struct Subscriber {
	_connection: moq_tokio::Connection,
	announced: moq_net::announce::Consumer,
	/// Frames read so far, so a test can prove the stream survives a re-route.
	frames: Arc<std::sync::atomic::AtomicU64>,
	_drain: tokio::task::JoinHandle<()>,
}

async fn subscribe(relay: &RelayHost) -> Subscriber {
	let origin = moq_tokio::origin::spawn(Hop::random());
	let consumer = origin.consume();
	let connection = tokio::time::timeout(
		TIMEOUT,
		client()
			.with_subscriber(origin)
			.connect(relay_url(relay.addr))
			.established(),
	)
	.await
	.expect("subscriber connect timeout")
	.expect("subscriber connect failed");

	let announced = consumer.announced();
	let frames = Arc::new(std::sync::atomic::AtomicU64::new(0));
	let drain = tokio::spawn({
		let frames = frames.clone();
		async move {
			let broadcast = consumer.routed_broadcast(PATH).await.expect("broadcast routed");
			let mut track = broadcast
				.track(TRACK)
				.expect("track handle")
				.subscribe(None)
				.await
				.expect("subscribe");
			loop {
				let mut group = match track.recv_group().await {
					Ok(Some(group)) => group,
					Ok(None) => {
						tracing::error!("test subscriber: track finished under the test");
						return;
					}
					Err(err) => {
						tracing::error!(%err, "test subscriber: track failed under the test");
						return;
					}
				};
				while let Ok(Some(_frame)) = group.read_frame().await {
					frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				}
			}
		}
	});

	Subscriber {
		_connection: connection,
		announced,
		frames,
		_drain: drain,
	}
}

impl Subscriber {
	/// Frames read so far.
	fn frames(&self) -> u64 {
		self.frames.load(std::sync::atomic::Ordering::Relaxed)
	}

	/// The hop chain of the next route update for [`PATH`].
	async fn next_route(&mut self) -> Vec<u64> {
		loop {
			let update = self.announced.next().await.expect("subscriber origin closed");
			if update.pattern.as_prefix() == Some(PATH) && update.active {
				return update.route.hops.iter().map(|hop| hop.id()).collect();
			}
		}
	}

	/// Route updates until one ends in `tail` (nearest relay first).
	async fn route_ending(&mut self, tail: &[u64]) -> Vec<u64> {
		loop {
			let route = self.next_route().await;
			if route.iter().rev().take(tail.len()).copied().collect::<Vec<_>>() == tail {
				return route;
			}
		}
	}
}

/// The three relays and the shapers between them.
struct Triangle {
	sjc: RelayHost,
	dal: RelayHost,
	nyc: RelayHost,
	_links: Vec<Shaper>,
}

/// sjc dials dal and nyc, dal dials nyc; every link crosses a shaper.
async fn triangle(ids: [u64; 3], direct: Profile) -> Triangle {
	let nyc = spawn_relay(ids[2], &[]).await;
	let dal_nyc = Shaper::start(nyc.addr, Profile::rtt(Duration::from_millis(60)).with_seed(2))
		.await
		.expect("shaper");
	let sjc_nyc = Shaper::start(nyc.addr, direct.with_seed(3)).await.expect("shaper");
	let dal = spawn_relay(ids[1], &[dal_nyc.addr()]).await;
	let sjc_dal = Shaper::start(dal.addr, Profile::rtt(Duration::from_millis(50)).with_seed(1))
		.await
		.expect("shaper");
	let sjc = spawn_relay(ids[0], &[sjc_dal.addr(), sjc_nyc.addr()]).await;
	Triangle {
		sjc,
		dal,
		nyc,
		_links: vec![sjc_dal, dal_nyc, sjc_nyc],
	}
}

/// With 1% loss on the direct edge, nyc ends up pulling through dal and sjc's
/// detour log names the edge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lossy_direct_edge_is_routed_around() {
	let cluster = triangle([11, 12, 13], Profile::rtt(Duration::from_millis(110)).with_loss(0.01)).await;
	// The clients attach while the first announcements still travel over
	// whichever links came up first, so the route shuffles a few times before it
	// settles: each shuffle caps the copy it leaves and lifts the cap on the one
	// it returns to, which is the handover that used to wedge the whole chain.
	let _publisher = publish(&cluster.sjc).await;
	let mut subscriber = subscribe(&cluster.nyc).await;

	// The first announcement can arrive over dal (the 50 ms link handshakes
	// before the 110 ms one), so wait for the direct route: idle, the direct
	// link is priced on its RTT alone and wins by the hop penalty.
	let direct = [cluster.nyc.id, cluster.sjc.id];
	tokio::time::timeout(TIMEOUT, subscriber.route_ending(&direct))
		.await
		.expect("the route never took the direct link");

	// Traffic now crosses the lossy edge, sjc measures the loss, and the route
	// moves onto dal at a group boundary.
	let via_dal = [cluster.nyc.id, cluster.dal.id, cluster.sjc.id];
	if tokio::time::timeout(TIMEOUT, subscriber.route_ending(&via_dal))
		.await
		.is_err()
	{
		panic!("the route never moved onto dal; relay logs:\n{}", logs());
	}

	// The stream survives the move: the handover lands at a group boundary and
	// frames keep arriving over the new path. The fresh subscription ramps over
	// a few seconds before it carries the full rate, so give it a while.
	let before = subscriber.frames();
	let flowing = tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			tokio::time::sleep(Duration::from_millis(250)).await;
			if subscriber.frames() >= before + 200 {
				return;
			}
		}
	})
	.await;
	assert!(
		flowing.is_ok(),
		"only {} frames arrived in 20 s after the re-route (had {before}); relay logs:\n{}",
		subscriber.frames() - before,
		logs()
	);

	// The detour is logged by sjc (peer nyc via dal) once dal's table arrives.
	let needle = format!("peer={} ", cluster.nyc.id);
	let via = format!("via={} ", cluster.dal.id);
	tokio::time::timeout(TIMEOUT, async {
		loop {
			let logs = logs();
			if logs
				.lines()
				.any(|line| line.contains("cluster link detour:") && line.contains(&needle) && line.contains(&via))
			{
				return;
			}
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
	})
	.await
	.expect("the detour was never logged");
}

/// A clean direct edge keeps the direct route: the detour would cost a hop
/// penalty more, and nothing is logged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_direct_edge_stays_direct() {
	let cluster = triangle([21, 22, 23], Profile::rtt(Duration::from_millis(110))).await;
	let _publisher = publish(&cluster.sjc).await;
	let mut subscriber = subscribe(&cluster.nyc).await;

	let direct = [cluster.nyc.id, cluster.sjc.id];
	tokio::time::timeout(TIMEOUT, subscriber.route_ending(&direct))
		.await
		.expect("the route never took the direct link");

	// Long enough for every link to be priced many times over under traffic
	// and any re-route to have happened.
	let moved = tokio::time::timeout(Duration::from_secs(12), async {
		loop {
			let route = subscriber.next_route().await;
			if route.contains(&cluster.dal.id) {
				return route;
			}
		}
	})
	.await;
	assert!(moved.is_err(), "the route left the clean direct link: {moved:?}");

	let logs = logs();
	let ours = format!("via={} ", cluster.dal.id);
	assert!(
		!logs
			.lines()
			.any(|line| line.contains("cluster link detour:") && line.contains(&ours)),
		"a clean triangle logged a detour:\n{logs}"
	);
}
