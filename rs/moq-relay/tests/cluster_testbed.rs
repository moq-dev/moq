//! A shaped cluster on one machine, as separate relay processes.
//!
//! Every inter-relay link crosses a userspace UDP shaper (`support::shaper`)
//! with the scenario's delay and loss, one broadcast is published at one relay
//! and read at another, and the run reports the route the subscriber's
//! announcements carried, its throughput and missing frames, frame age
//! percentiles per route phase, the detours the relays logged, the control
//! bytes each idle link carried, and each relay's CPU. It is the proof behind
//! `[cluster.cost]`, so it is ignored by default and run on demand:
//!
//! ```text
//! just test cluster triangle_lossy   # or triangle_clean, ten_regions
//! MOQ_TESTBED_SECS=600 just test cluster ten_regions
//! MOQ_TESTBED_KEEP=1 ...             # keep the run directory with every relay log
//! MOQ_TESTBED_LOG=debug ...          # relay log level in those logs
//! MOQ_TESTBED_MEASURE=false ...      # the control: hop counting, no measured prices
//! MOQ_TESTBED_SETTLE=5 ...           # seconds the cluster gets to form before the clients attach
//! MOQ_TESTBED_PROBE=100000 ...       # bits per second of PROBE padding on every idle link
//! ```

mod support;

use std::{
	collections::BTreeMap,
	net::SocketAddr,
	path::PathBuf,
	process::Stdio,
	sync::{Arc, Mutex},
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use moq_net::Hop;
use moq_relay::{Config, cluster::Peer};
use support::shaper::{Profile, Shaper};

const PATH: &str = "testbed/cam";
const TRACK: &str = "video";
const FRAME_INTERVAL: Duration = Duration::from_millis(25);
const FRAME_BYTES: usize = 8 * 1024;
const FRAMES_PER_GROUP: u64 = 40;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the broadcast runs once both clients are connected.
fn run_secs() -> u64 {
	std::env::var("MOQ_TESTBED_SECS")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(60)
}

fn lite_06() -> moq_net::Version {
	"moq-lite-06-wip".parse().expect("parse version")
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis() as u64
}

/// A free loopback UDP port, released for the relay to bind.
fn free_udp_port() -> u16 {
	std::net::UdpSocket::bind("127.0.0.1:0")
		.expect("bind probe")
		.local_addr()
		.expect("local addr")
		.port()
}

/// One region: its relay process, its listen address, and its log.
struct Region {
	name: &'static str,
	id: u64,
	addr: SocketAddr,
	log: PathBuf,
	child: tokio::process::Child,
}

impl Region {
	/// The relay's CPU time so far, from `ps`, which macOS prints as `m:ss.cc`
	/// and Linux as `[[d-]hh:]mm:ss`.
	fn cpu_time(&self) -> Option<Duration> {
		let pid = self.child.id()?;
		let out = std::process::Command::new("ps")
			.args(["-o", "cputime=", "-p", &pid.to_string()])
			.output()
			.ok()?;
		let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
		let (days, rest) = match text.split_once('-') {
			Some((d, rest)) => (d.parse::<f64>().ok()?, rest.to_string()),
			None => (0.0, text),
		};
		let parts: Vec<f64> = rest.split(':').map(|p| p.parse::<f64>().ok()).collect::<Option<_>>()?;
		let secs = parts.iter().fold(0.0, |acc, part| acc * 60.0 + part);
		Some(Duration::from_secs_f64(days * 86_400.0 + secs))
	}

	/// The detour lines this relay logged.
	fn detours(&self) -> Vec<String> {
		let log = std::fs::read(&self.log).unwrap_or_default();
		String::from_utf8_lossy(&log)
			.lines()
			.map(strip_ansi)
			.filter(|line| line.contains("cluster link detour"))
			// Keep the fields, drop the timestamp and target.
			.map(|line| line.split("moq_relay::cluster: ").nth(1).unwrap_or(&line).to_string())
			.collect()
	}
}

/// Drop the SGR escapes a relay writes into its log.
fn strip_ansi(line: &str) -> String {
	let mut out = String::with_capacity(line.len());
	let mut rest = line;
	while let Some(start) = rest.find('\x1b') {
		out.push_str(&rest[..start]);
		let after = &rest[start + 1..];
		match after
			.strip_prefix('[')
			.and_then(|s| s.find('m').map(|end| &s[end + 1..]))
		{
			Some(tail) => rest = tail,
			None => {
				rest = after;
			}
		}
	}
	out.push_str(rest);
	out
}

/// One shaped link between two regions.
struct Link {
	a: &'static str,
	b: &'static str,
	rtt: Duration,
	loss: f64,
	shaper: Shaper,
}

/// The whole cluster: regions in start order and the links between them.
struct Testbed {
	dir: tempfile::TempDir,
	regions: Vec<Region>,
	links: Vec<Link>,
}

/// A link to build: `a` dials `b`.
struct Edge {
	a: &'static str,
	b: &'static str,
	rtt_ms: u64,
	loss: f64,
}

const fn edge(a: &'static str, b: &'static str, rtt_ms: u64) -> Edge {
	Edge {
		a,
		b,
		rtt_ms,
		loss: 0.0,
	}
}

const fn lossy(a: &'static str, b: &'static str, rtt_ms: u64, loss: f64) -> Edge {
	Edge { a, b, rtt_ms, loss }
}

impl Testbed {
	/// Start every region and shape every edge. Regions start in an order where
	/// each dialed peer is already listening: `b` before `a` for every edge.
	async fn start(names: &[&'static str], edges: &[Edge]) -> Self {
		let dir = tempfile::Builder::new()
			.prefix("moq-cluster-testbed-")
			.tempdir()
			.expect("run directory");
		let mut ports: BTreeMap<&str, SocketAddr> = BTreeMap::new();
		for name in names {
			ports.insert(name, SocketAddr::from(([127, 0, 0, 1], free_udp_port())));
		}

		// Shapers front the dialed side; the dialer's config points at them.
		let mut links = Vec::new();
		let mut dials: BTreeMap<&str, Vec<SocketAddr>> = BTreeMap::new();
		for (index, edge) in edges.iter().enumerate() {
			let profile = Profile::rtt(Duration::from_millis(edge.rtt_ms))
				.with_loss(edge.loss)
				.with_seed(index as u64 + 1);
			let shaper = Shaper::start(ports[edge.b], profile).await.expect("shaper");
			dials.entry(edge.a).or_default().push(shaper.addr());
			links.push(Link {
				a: edge.a,
				b: edge.b,
				rtt: Duration::from_millis(edge.rtt_ms),
				loss: edge.loss,
				shaper,
			});
		}

		let mut regions = Vec::new();
		for (index, name) in names.iter().enumerate() {
			let id = index as u64 + 1;
			let addr = ports[name];
			let mut config = Config::default();
			config.listen.bind = Some(addr.to_string());
			config.listen.tls.generate = vec!["localhost".into()];
			config.listen.version = vec![lite_06()];
			config.connect.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
			config.connect.tls.insecure = Some(true);
			config.connect.websocket.enabled = Some(false);
			config.connect.version = vec![lite_06()];
			config.auth.public = vec![moq_auth::Pattern::all()];
			config.cluster.id = Some(id);
			config.cluster.node = Some(format!("https://{name}.testbed/"));
			// `MOQ_TESTBED_MEASURE=false` is the control: hop counting, no pricing.
			config.cluster.cost.measure = std::env::var("MOQ_TESTBED_MEASURE").as_deref() != Ok("false");
			// `MOQ_TESTBED_PROBE=<bps>` keeps that much padding on every idle link.
			config.cluster.cost.probe = std::env::var("MOQ_TESTBED_PROBE").ok().and_then(|s| s.parse().ok());
			config.cluster.connect = dials
				.get(name)
				.into_iter()
				.flatten()
				.map(|addr| Peer::new(format!("https://{addr}/")))
				.collect();

			let toml = toml::to_string_pretty(&config).expect("serialize config");
			let path = dir.path().join(format!("{name}.toml"));
			std::fs::write(&path, toml).expect("write config");
			let log = dir.path().join(format!("{name}.log"));
			let stderr = std::fs::File::create(&log).expect("create log");
			let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq-relay"))
				.arg(&path)
				.env(
					"MOQ_LOG_LEVEL",
					std::env::var("MOQ_TESTBED_LOG").unwrap_or_else(|_| "info".into()),
				)
				.stdin(Stdio::null())
				.stdout(Stdio::null())
				.stderr(stderr)
				.kill_on_drop(true)
				.spawn()
				.expect("spawn moq-relay");
			regions.push(Region {
				name,
				id,
				addr,
				log,
				child,
			});
		}

		Self { dir, regions, links }
	}

	fn region(&self, name: &str) -> &Region {
		self.regions.iter().find(|r| r.name == name).expect("region")
	}

	fn name(&self, hop: u64) -> String {
		self.regions
			.iter()
			.find(|r| r.id == hop)
			.map(|r| r.name.to_string())
			.unwrap_or_else(|| format!("#{hop}"))
	}

	fn route_names(&self, hops: &[u64]) -> String {
		// The first hop is the publishing client; the relays follow.
		hops.iter()
			.skip(1)
			.map(|hop| self.name(*hop))
			.collect::<Vec<_>>()
			.join(" -> ")
	}
}

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.tls.insecure = Some(true);
	config.websocket.enabled = Some(false);
	config.version = vec![lite_06()];
	config.init(Default::default()).expect("client init")
}

fn relay_url(addr: SocketAddr) -> url::Url {
	format!("https://{addr}/").parse().expect("parse url")
}

struct Publisher {
	_broadcast: moq_net::broadcast::Producer,
	_connection: moq_tokio::Connection,
	streamer: tokio::task::AbortHandle,
	/// Frames written so far.
	published: Arc<std::sync::atomic::AtomicU64>,
}

impl Drop for Publisher {
	fn drop(&mut self) {
		self.streamer.abort();
	}
}

/// Publish at `region`: 8 KiB frames every 25 ms (about 2.6 Mbit/s), 40 to a
/// group, each stamped with the wall clock and a running sequence number.
async fn publish(region: &Region) -> Publisher {
	let origin = moq_tokio::origin::spawn(Hop::random());
	let broadcast = origin.create_broadcast(PATH).expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let track = broadcast.create_track(TRACK, None).expect("create track");

	let published = Arc::new(std::sync::atomic::AtomicU64::new(0));
	let streamer = tokio::spawn({
		let published = published.clone();
		async move {
			let mut ticker = tokio::time::interval(FRAME_INTERVAL);
			ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
			let mut payload = vec![0u8; FRAME_BYTES];
			let mut sequence = 0u64;
			loop {
				let Ok(mut group) = track.append_group() else { break };
				for _ in 0..FRAMES_PER_GROUP {
					ticker.tick().await;
					payload[..8].copy_from_slice(&now_ms().to_be_bytes());
					payload[8..16].copy_from_slice(&sequence.to_be_bytes());
					sequence += 1;
					if group.write_frame(moq_net::Timestamp::ZERO, payload.clone()).is_err() {
						return;
					}
					published.store(sequence, std::sync::atomic::Ordering::Relaxed);
				}
				if group.finish().is_err() {
					break;
				}
			}
		}
	})
	.abort_handle();

	let connection = tokio::time::timeout(
		CONNECT_TIMEOUT,
		client()
			.with_publisher(&origin)
			.connect(relay_url(region.addr))
			.established(),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	Publisher {
		_broadcast: broadcast,
		_connection: connection,
		streamer,
		published,
	}
}

/// One received frame.
#[derive(Clone, Copy)]
struct Sample {
	at: Instant,
	age_ms: u32,
	sequence: u64,
}

/// What the subscriber observed.
#[derive(Default)]
struct Observed {
	samples: Vec<Sample>,
	bytes: u64,
	/// Groups that ended in an error rather than a clean finish.
	failed_groups: u64,
	/// How the track subscription ended, if it did before the run was over.
	ended: Option<String>,
	/// `(when, hops)` for every route update the subscriber's origin delivered.
	routes: Vec<(Instant, Vec<u64>)>,
}

struct Subscriber {
	_connection: moq_tokio::Connection,
	observed: Arc<Mutex<Observed>>,
	_tasks: Vec<tokio::task::JoinHandle<()>>,
}

async fn subscribe(region: &Region) -> Subscriber {
	let origin = moq_tokio::origin::spawn(Hop::random());
	let consumer = origin.consume();
	let connection = tokio::time::timeout(
		CONNECT_TIMEOUT,
		client()
			.with_subscriber(origin)
			.connect(relay_url(region.addr))
			.established(),
	)
	.await
	.expect("subscriber connect timeout")
	.expect("subscriber connect failed");

	let observed = Arc::new(Mutex::new(Observed::default()));
	let mut announced = consumer.announced();
	let routes = tokio::spawn({
		let observed = observed.clone();
		async move {
			while let Some(update) = announced.next().await {
				if update.path.as_str() == PATH && update.kind.is_active() {
					let hops = update.route.hops.iter().map(|hop| hop.id()).collect();
					observed.lock().unwrap().routes.push((Instant::now(), hops));
				}
			}
		}
	});
	let drain = tokio::spawn({
		let observed = observed.clone();
		async move {
			let broadcast = consumer.routed_broadcast(PATH).await.expect("broadcast routed");
			let mut track = broadcast
				.track(TRACK)
				.expect("track handle")
				.subscribe(None)
				.await
				.expect("subscribe");
			let ended = loop {
				let mut group = match track.recv_group().await {
					Ok(Some(group)) => group,
					Ok(None) => break "track finished".to_string(),
					Err(err) => break format!("track failed: {err}"),
				};
				loop {
					match group.read_frame().await {
						Ok(Some(frame)) => {
							let payload = &frame.payload;
							let sent = u64::from_be_bytes(payload[..8].try_into().unwrap());
							let sequence = u64::from_be_bytes(payload[8..16].try_into().unwrap());
							let age_ms = now_ms().saturating_sub(sent) as u32;
							let mut observed = observed.lock().unwrap();
							observed.bytes += payload.len() as u64;
							observed.samples.push(Sample {
								at: Instant::now(),
								age_ms,
								sequence,
							});
						}
						Ok(None) => break,
						Err(_) => {
							observed.lock().unwrap().failed_groups += 1;
							break;
						}
					}
				}
			};
			observed.lock().unwrap().ended = Some(ended);
		}
	});

	Subscriber {
		_connection: connection,
		observed,
		_tasks: vec![routes, drain],
	}
}

/// Percentiles of a sorted slice.
fn percentile(sorted: &[u32], p: f64) -> u32 {
	if sorted.is_empty() {
		return 0;
	}
	let index = ((sorted.len() - 1) as f64 * p).round() as usize;
	sorted[index]
}

/// Frame-level numbers for one window of samples.
struct Phase {
	label: String,
	frames: usize,
	missing: u64,
	mbps: f64,
	p50: u32,
	p95: u32,
	p99: u32,
}

fn phase(label: String, samples: &[Sample]) -> Phase {
	let mut ages: Vec<u32> = samples.iter().map(|s| s.age_ms).collect();
	ages.sort_unstable();
	let (first, last) = match (samples.first(), samples.last()) {
		(Some(first), Some(last)) => (first, last),
		_ => {
			return Phase {
				label,
				frames: 0,
				missing: 0,
				mbps: 0.0,
				p50: 0,
				p95: 0,
				p99: 0,
			};
		}
	};
	let lo = samples.iter().map(|s| s.sequence).min().unwrap_or(0);
	let hi = samples.iter().map(|s| s.sequence).max().unwrap_or(0);
	let expected = hi - lo + 1;
	let missing = expected.saturating_sub(samples.len() as u64);
	// Under a second of samples has no rate worth printing.
	let elapsed = last.at.duration_since(first.at).as_secs_f64();
	let mbps = match elapsed >= 1.0 {
		true => samples.len() as f64 * FRAME_BYTES as f64 * 8.0 / elapsed / 1e6,
		false => 0.0,
	};
	Phase {
		label,
		frames: samples.len(),
		missing,
		mbps,
		p50: percentile(&ages, 0.50),
		p95: percentile(&ages, 0.95),
		p99: percentile(&ages, 0.99),
	}
}

/// Run one scenario: publish at `from`, subscribe at `to`, stream for the run
/// length, and print the report.
async fn run(title: &str, names: &[&'static str], edges: &[Edge], from: &str, to: &str) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let testbed = Testbed::start(names, edges).await;
	// Let the cluster form and its first prices land before anything is measured:
	// every dial retries on its own backoff, and the first announcements travel
	// over whichever links came up first.
	let settle = std::env::var("MOQ_TESTBED_SETTLE")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(5);
	tokio::time::sleep(Duration::from_secs(settle)).await;

	let publisher = publish(testbed.region(from)).await;
	let subscriber = subscribe(testbed.region(to)).await;
	let started = Instant::now();
	let cpu_start: Vec<Option<Duration>> = testbed.regions.iter().map(Region::cpu_time).collect();
	let bytes_start: Vec<u64> = testbed.links.iter().map(|l| l.shaper.counters().bytes).collect();

	// Once a second: what the publisher's session has sent and what each link
	// has carried, so the report can show where the bytes went around a switch.
	let monitor = publisher._connection.monitor();
	let mut timeline: Vec<(Duration, u64, Vec<u64>)> = Vec::new();
	let deadline = started + Duration::from_secs(run_secs());
	loop {
		tokio::time::sleep(Duration::from_secs(1)).await;
		let sent = monitor.stats().and_then(|s| s.bytes_sent).unwrap_or(0);
		let links = testbed.links.iter().map(|l| l.shaper.counters().bytes).collect();
		timeline.push((started.elapsed(), sent, links));
		if Instant::now() >= deadline {
			break;
		}
	}

	let elapsed = started.elapsed();
	let cpu_end: Vec<Option<Duration>> = testbed.regions.iter().map(Region::cpu_time).collect();
	let bytes_end: Vec<u64> = testbed.links.iter().map(|l| l.shaper.counters().bytes).collect();
	let published = publisher.published.load(std::sync::atomic::Ordering::Relaxed);
	drop(publisher);
	tokio::time::sleep(Duration::from_millis(500)).await;

	let observed = subscriber.observed.lock().unwrap();
	let mut out = String::new();
	out += &format!("\n## {title}\n\n");
	out += &format!(
		"{} relays, {} links, {} s of a {:.1} Mbit/s broadcast from {from} to {to}.\n\n",
		testbed.regions.len(),
		testbed.links.len(),
		elapsed.as_secs(),
		FRAME_BYTES as f64 * 8.0 / FRAME_INTERVAL.as_secs_f64() / 1e6
	);

	// Route timeline.
	out += "| at | route |\n|---|---|\n";
	let mut boundaries: Vec<(Instant, String)> = Vec::new();
	let mut last: Option<&Vec<u64>> = None;
	for (at, hops) in &observed.routes {
		if last == Some(hops) {
			continue;
		}
		last = Some(hops);
		let label = testbed.route_names(hops);
		let when = at.saturating_duration_since(started);
		out += &format!("| {:.1} s | {label} |\n", when.as_secs_f64());
		boundaries.push((*at, label));
	}
	out += "\n";

	// Frame numbers per route phase, then the whole run.
	out += "| phase | frames | missing | Mbit/s | age p50 | p95 | p99 |\n|---|---|---|---|---|---|---|\n";
	let mut phases = Vec::new();
	for (index, (at, label)) in boundaries.iter().enumerate() {
		let end = boundaries.get(index + 1).map(|(next, _)| *next);
		let samples: Vec<Sample> = observed
			.samples
			.iter()
			.copied()
			.filter(|s| s.at >= *at && end.is_none_or(|end| s.at < end))
			.collect();
		phases.push(phase(format!("via {label}"), &samples));
	}
	phases.push(phase("whole run".into(), &observed.samples));
	for p in &phases {
		out += &format!(
			"| {} | {} | {} | {:.2} | {} ms | {} ms | {} ms |\n",
			p.label, p.frames, p.missing, p.mbps, p.p50, p.p95, p.p99
		);
	}
	out += &format!(
		"\nPublished {published} frames, received {}, groups that ended in an error: {}{}.\n\n",
		observed.samples.len(),
		observed.failed_groups,
		observed
			.ended
			.as_deref()
			.map(|why| format!("; {why}"))
			.unwrap_or_default()
	);

	// Links: what each carried, so an idle link shows the control overhead.
	out += "| link | rtt | loss | bytes/s | lost by shaper |\n|---|---|---|---|---|\n";
	for (index, link) in testbed.links.iter().enumerate() {
		let bytes = bytes_end[index].saturating_sub(bytes_start[index]);
		out += &format!(
			"| {}-{} | {} ms | {:.1}% | {:.0} | {} |\n",
			link.a,
			link.b,
			link.rtt.as_millis(),
			link.loss * 100.0,
			bytes as f64 / elapsed.as_secs_f64(),
			link.shaper.counters().lost
		);
	}
	out += "\n";

	// Per-second bytes: every second of a short run, otherwise the seconds
	// around each route change and the end.
	let pivots: Vec<f64> = boundaries
		.iter()
		.map(|(at, _)| at.saturating_duration_since(started).as_secs_f64())
		.chain(std::iter::once(elapsed.as_secs_f64()))
		.collect();
	let shown = |at: f64| elapsed.as_secs() <= 60 || pivots.iter().any(|pivot| (at - pivot).abs() <= 4.0);
	out += "| second | publisher KB/s |";
	for link in &testbed.links {
		out += &format!(" {}-{} KB/s |", link.a, link.b);
	}
	out += "\n|---|---|";
	for _ in &testbed.links {
		out += "---|";
	}
	out += "\n";
	for pair in timeline.windows(2) {
		let (at, sent, links) = &pair[1];
		let (_, prev_sent, prev_links) = &pair[0];
		if !shown(at.as_secs_f64()) {
			continue;
		}
		out += &format!(
			"| {:.0} | {:.0} |",
			at.as_secs_f64(),
			sent.saturating_sub(*prev_sent) as f64 / 1000.0
		);
		for (now, prev) in links.iter().zip(prev_links) {
			out += &format!(" {:.0} |", now.saturating_sub(*prev) as f64 / 1000.0);
		}
		out += "\n";
	}
	out += "\n";

	// CPU per relay over the run.
	out += "| relay | cpu |\n|---|---|\n";
	for (index, region) in testbed.regions.iter().enumerate() {
		let cpu = match (cpu_start[index], cpu_end[index]) {
			(Some(start), Some(end)) => format!(
				"{:.1}%",
				end.saturating_sub(start).as_secs_f64() / elapsed.as_secs_f64() * 100.0
			),
			_ => "n/a".into(),
		};
		out += &format!("| {} | {cpu} |\n", region.name);
	}
	out += "\n";

	// Detours the relays named.
	out += "Detours logged:\n\n";
	let mut any = false;
	for region in &testbed.regions {
		for line in region.detours() {
			any = true;
			out += &format!("- {}: {line}\n", region.name);
		}
	}
	if !any {
		out += "- none\n";
	}

	println!("{out}");
	if std::env::var_os("MOQ_TESTBED_KEEP").is_some() {
		let kept = testbed.dir.keep();
		println!("run directory kept at {}", kept.display());
	}
	drop(observed);
}

#[test]
fn strip_ansi_keeps_the_text() {
	let raw = "\x1b[2m2026-01-01T00:00:00Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mmoq_relay::cluster\x1b[0m\x1b[2m:\x1b[0m cluster link detour: x \x1b[3mpeer\x1b[0m\x1b[2m=\x1b[0m1";
	assert_eq!(
		strip_ansi(raw),
		"2026-01-01T00:00:00Z  INFO moq_relay::cluster: cluster link detour: x peer=1"
	);
}

/// The lossy triangle: sjc-dal 50 ms, dal-nyc 60 ms, sjc-nyc 110 ms with 1% loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "testbed: run with `just test cluster triangle_lossy`"]
async fn triangle_lossy() {
	run(
		"Triangle, lossy direct edge",
		&["nyc", "dal", "sjc"],
		&[
			edge("dal", "nyc", 60),
			edge("sjc", "dal", 50),
			lossy("sjc", "nyc", 110, 0.01),
		],
		"sjc",
		"nyc",
	)
	.await;
}

/// The same triangle with a clean direct edge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "testbed: run with `just test cluster triangle_clean`"]
async fn triangle_clean() {
	run(
		"Triangle, clean direct edge",
		&["nyc", "dal", "sjc"],
		&[edge("dal", "nyc", 60), edge("sjc", "dal", 50), edge("sjc", "nyc", 110)],
		"sjc",
		"nyc",
	)
	.await;
}

/// Ten regions on a plausible backbone. Two edges break the triangle
/// inequality by more than the hop penalty (sjc-dal at 80 ms against
/// sjc-den-dal at 50, nyc-fra at 120 ms against nyc-lon-fra at 85), and two
/// triples are additive with the middle region on the path (sjc-den at 30 ms
/// against sjc-lax-den at 35, den-nyc at 45 ms against den-chi-nyc at 45), so
/// only the first two are detours and nothing flaps on the ties.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "testbed: run with `just test cluster ten_regions`"]
async fn ten_regions() {
	run(
		"Ten regions",
		&["fra", "lon", "iad", "nyc", "atl", "chi", "dal", "den", "lax", "sjc"],
		&[
			edge("lon", "fra", 15),
			edge("nyc", "iad", 8),
			edge("nyc", "lon", 70),
			edge("nyc", "fra", 120),
			edge("atl", "iad", 15),
			edge("chi", "nyc", 20),
			edge("chi", "iad", 20),
			edge("dal", "atl", 20),
			edge("dal", "chi", 25),
			edge("den", "dal", 20),
			edge("den", "chi", 25),
			edge("den", "nyc", 45),
			edge("lax", "den", 25),
			edge("sjc", "lax", 10),
			edge("sjc", "den", 30),
			edge("sjc", "dal", 80),
		],
		"sjc",
		"fra",
	)
	.await;
}
