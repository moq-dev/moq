//! Over native QUIC, incomplete frames hold the receiver's connection credit, and when
//! they fill its window the receiver stops the least urgent stream and keeps going.
//!
//! The receiver runs on the raw driver so its connection window can be far smaller than
//! the stack allows, which keeps the payloads small. Without recovery the high-priority
//! frame never arrives: the window is spent on a low-priority frame that can never
//! complete. The Rust arm dials a stock server. The JavaScript arms serve WebTransport, and
//! qmux over WebSocket, to a Bun publisher; they need Bun and run in the interop workflow.
//!
//! Some runs go through a bufferbloated path instead of loopback, so a sender held back by
//! a slow link and a deep queue is shown not to look like a stuck window.

#![cfg(feature = "quic")]

use std::{process::Stdio, sync::Arc, time::Duration};

use bytes::Bytes;
use moq_tokio::moq_net::{
	self, Timestamp, frame,
	track::{Delay, Subscription},
};
use moq_tokio::quic::{driver, web};
use tokio::io::AsyncWriteExt;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The receiver's connection receive window.
const WINDOW: usize = 256 * 1024;

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Raw-driver transport settings with a connection receive window of `window`.
fn transport(window: u64) -> driver::transport::Config {
	let mut transport = driver::transport::Config::default();
	transport.limits.receive_window = std::num::NonZeroU64::new(window);
	transport
}

/// A bottleneck below what the sender can push, behind a deep but finite queue that drops
/// what overflows it, both ways. No random loss: this judges throughput and latency.
fn bufferbloat(bits_per_second: u64) -> moq_shaper::Profile {
	moq_shaper::Profile {
		delay: Duration::from_millis(10),
		rate: Some(moq_shaper::Rate {
			bits_per_second,
			// One datagram, so a flight is paced rather than let through whole.
			burst: 1500,
			queue: Duration::from_millis(250),
		}),
		..Default::default()
	}
}

/// A server publishing broadcast `room`, and a client subscribed to its origin over a
/// connection with the given transport settings.
struct Setup {
	broadcast: moq_net::origin::Publication,
	remote: moq_net::broadcast::Consumer,
	/// The client's connection, for its credit stats.
	conn: driver::connection::Connection,
	/// The impairment between them, if any.
	shaper: Option<moq_shaper::Shaper>,
	_client: moq_net::Session,
	_endpoint: driver::endpoint::Endpoint,
}

async fn setup(version: &str, transport: driver::transport::Config) -> Setup {
	setup_through(version, transport, None).await
}

/// [`setup`], with the client reaching the server through a shaper treating both ways to
/// `profile`.
async fn setup_through(
	version: &str,
	transport: driver::transport::Config,
	profile: Option<moq_shaper::Profile>,
) -> Setup {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let version: moq_net::Version = version.parse().expect("version");

	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("room").expect("broadcast");
	broadcast.announce(Default::default()).expect("announce");

	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.tls.generate = vec!["localhost".into()];
	config.version = vec![version];
	let mut server = config
		.init(Default::default())
		.expect("init server")
		.listen()
		.await
		.expect("listen");
	let server_addr: std::net::SocketAddr = ([127, 0, 0, 1], server.local_addr().expect("addr").port()).into();
	let shaper = match profile {
		Some(profile) => Some(
			moq_shaper::Shaper::bind(moq_shaper::Config {
				bind: "127.0.0.1:0".parse().unwrap(),
				target: server_addr,
				seed: rand_seed(),
				up: profile.clone(),
				down: profile,
			})
			.await
			.expect("start the shaper"),
		),
		None => None,
	};
	let addr = shaper.as_ref().map_or(server_addr, |shaper| shaper.addr());
	tokio::spawn(async move {
		let request = server.accept().await.expect("accept");
		let session = request.with_publisher(&publisher).ok().await.expect("session");
		let _ = session.closed().await;
	});

	let (endpoint, conn) = dial(version, addr, transport).await;

	let subscriber = moq_tokio::origin::spawn();
	let session =
		moq_tokio::transport::Session::new(web::Session::raw(conn.clone()), moq_net::transport::Address::Setup);
	let (client, driver) = moq_net::Client::new()
		.with_versions(version.into())
		.with_subscriber(subscriber.clone())
		.connect(moq_net::time::Instant::now(), session)
		.await
		.expect("client handshake");
	tokio::spawn(moq_net::time::run(driver));

	Setup {
		broadcast,
		remote: room(&subscriber).await,
		conn,
		shaper,
		_client: client,
		_endpoint: endpoint,
	}
}

/// Dial native QUIC with the selected version and transport settings.
pub(crate) async fn dial(
	version: moq_net::Version,
	addr: std::net::SocketAddr,
	transport: driver::transport::Config,
) -> (driver::endpoint::Endpoint, driver::connection::Connection) {
	let mut tls = rustls::ClientConfig::builder()
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(AnyCert))
		.with_no_client_auth();
	tls.alpn_protocols = vec![version.alpn().as_bytes().to_vec()];
	let mut client = driver::client::Config::new(Arc::new(tls));
	client.transport = transport;

	let endpoint =
		driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
			.expect("endpoint");
	let conn = endpoint
		.connect(&client, addr, "localhost")
		.expect("connect")
		.await
		.expect("handshake");

	(endpoint, conn)
}

/// The shaper's seed: `MOQ_SHAPER_SEED` to replay a run, or a fresh one, printed.
pub(crate) fn rand_seed() -> u64 {
	let seed = match std::env::var("MOQ_SHAPER_SEED") {
		Ok(seed) => seed.parse().expect("MOQ_SHAPER_SEED is not a u64"),
		Err(_) => kio::clock::wall()
			.saturating_duration_since(kio::clock::Wall::UNIX_EPOCH)
			.as_nanos() as u64,
	};
	eprintln!("MOQ_SHAPER_SEED={seed}");
	seed
}

/// Broadcast `room`, once the peer announces it into `origin`.
async fn room(origin: &moq_net::origin::Producer) -> moq_net::broadcast::Consumer {
	let consumer = origin.consume();
	kio::clock::timeout(TIMEOUT, consumer.follow("room", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
}

/// Subscribe to track `name` of `remote` at `priority`.
async fn subscribe(remote: &moq_net::broadcast::Consumer, name: &str, priority: i8) -> moq_net::track::Subscriber {
	kio::clock::timeout(
		TIMEOUT,
		remote
			.track(name)
			.unwrap()
			.subscribe(Subscription::default().with_priority(priority)),
	)
	.await
	.expect("subscribe timeout")
	.expect("subscribe")
}

impl Setup {
	/// Subscribe to a new track at `priority`.
	async fn subscribe(&self, name: &str, priority: i8) -> (moq_net::track::Producer, moq_net::track::Subscriber) {
		let track = self.broadcast.create_track(name, timed()).unwrap();
		(track, subscribe(&self.remote, name, priority).await)
	}

	/// What the shaper did, failing if the impairment it was given never acted.
	fn verify(&self) -> Option<moq_shaper::Stats> {
		let shaper = self.shaper.as_ref()?;
		Some(shaper.verify().unwrap_or_else(|err| panic!("shaper: {err:#}")))
	}
}

/// The next group, which must arrive in time.
async fn next_group(sub: &mut moq_net::track::Subscriber) -> moq_net::group::Consumer {
	kio::clock::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("group timed out")
		.expect("group")
		.expect("track ended")
}

/// The next frame's payload, which must arrive in time.
async fn next_frame(sub: &mut moq_net::track::Subscriber) -> Bytes {
	let mut group = next_group(sub).await;
	kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timed out")
		.expect("frame")
		.expect("group ended")
		.payload
}

/// The next group's first frame, still arriving.
async fn partial_frame(sub: &mut moq_net::track::Subscriber) -> moq_net::frame::Consumer {
	let mut group = next_group(sub).await;
	kio::clock::timeout(TIMEOUT, group.next_frame())
		.await
		.expect("frame header timed out")
		.expect("frame")
		.expect("group ended")
}

/// Read `frame` until `bytes` of it arrived.
async fn read_partial(frame: &mut moq_net::frame::Consumer, bytes: usize) {
	let mut seen = 0;
	while seen < bytes {
		let chunk = kio::clock::timeout(TIMEOUT, frame.read_chunk())
			.await
			.expect("partial payload timed out")
			.expect("chunk")
			.expect("frame ended");
		seen += chunk.len();
	}
}

/// Two partial frames that together exceed the window: the lower-priority one is
/// stopped, and the higher-priority one completes on the credit it returns.
async fn the_less_urgent_partial_frame_gives_way(version: &str, profile: Option<moq_shaper::Profile>) {
	let setup = setup_through(version, transport(WINDOW as u64), profile).await;
	let (low, mut low_sub) = setup.subscribe("low", 1).await;
	let (high, mut high_sub) = setup.subscribe("high", 100).await;

	// Three quarters of the window, held by a frame that never finishes.
	let mut low_frame = low
		.append_group()
		.unwrap()
		.create_frame(frame::Info {
			size: WINDOW as u64,
			timestamp: Some(Timestamp::ZERO),
		})
		.unwrap();
	low_frame.write(vec![1u8; WINDOW * 3 / 4]).unwrap();

	// Wait until the receiver holds all of it, so nothing else got there first.
	read_partial(&mut partial_frame(&mut low_sub).await, WINDOW * 3 / 4).await;

	// Half the window, which only fits once the low frame lets go.
	let payload = Bytes::from(vec![2u8; WINDOW / 2]);
	let mut high_group = high.append_group().unwrap();
	high_group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	high_group.finish().unwrap();
	assert_eq!(next_frame(&mut high_sub).await, payload, "{version}");

	// The session survives, and the low track keeps delivering what fits.
	drop(low_frame);
	let mut group = low.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, &b"after"[..]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut low_sub).await, &b"after"[..], "{version}");
	assert_eq!(setup.conn.stats().credit.evicted, 1, "{version}");
	let session = moq_tokio::transport::Session::new(
		web::Session::raw(setup.conn.clone()),
		moq_net::transport::Address::Setup,
	);
	assert_eq!(
		moq_net::transport::Metrics::streams_evicted(&moq_net::transport::poll::Session::metrics(&session)),
		Some(1),
		"moq-net sees the eviction count, {version}"
	);
	setup.verify();
}

#[tokio::test]
async fn lite_partial_frame_gives_way() {
	the_less_urgent_partial_frame_gives_way("moq-lite-06", None).await;
}

#[tokio::test]
async fn ietf_partial_frame_gives_way() {
	the_less_urgent_partial_frame_gives_way("moq-transport-18", None).await;
}

/// The same recovery when a slow link with a deep queue sits between them.
#[tokio::test]
async fn lite_partial_frame_gives_way_behind_bufferbloat() {
	the_less_urgent_partial_frame_gives_way("moq-lite-06", Some(bufferbloat(20_000_000))).await;
}

/// A sender held back by a bottleneck and its queue is pacing, not a stuck window: frames
/// that fit the window together all complete, and nothing is evicted.
#[tokio::test]
async fn bufferbloat_is_not_a_stuck_window() {
	const STREAMS: usize = 8;
	const ROUNDS: usize = 16;
	let setup = setup_through("moq-lite-06", transport(WINDOW as u64), Some(bufferbloat(20_000_000))).await;
	let payload = Bytes::from(vec![3u8; WINDOW / (2 * STREAMS)]);
	let mut tasks = Vec::with_capacity(STREAMS);
	for index in 0..STREAMS {
		let (track, mut sub) = setup.subscribe(&index.to_string(), index as i8).await;
		let payload = payload.clone();
		tasks.push(tokio::spawn(async move {
			for _ in 0..ROUNDS {
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
				group.finish().unwrap();
				assert_eq!(next_frame(&mut sub).await, payload);
			}
		}));
	}
	for task in tasks {
		task.await.unwrap();
	}
	assert_eq!(setup.conn.stats().credit.evicted, 0);
	let stats = setup.verify().expect("shaped");
	assert!(stats.down.throttled > 0, "the bottleneck held the sender back: {stats}");
}

/// A frame many windows long still completes, since its credit returns as each storage
/// segment seals: the window only has to fit one segment, so nothing is evicted.
#[tokio::test]
async fn lite_frame_of_many_windows_completes() {
	// Room for one of moq-net's 1 MiB storage segments and its framing.
	const SEGMENTED: usize = 4 << 20;
	let setup = setup("moq-lite-06", transport(SEGMENTED as u64)).await;
	let (track, mut sub) = setup.subscribe("file", 0).await;

	let payload = Bytes::from((0..4 * SEGMENTED).map(|i| (i % 251) as u8).collect::<Vec<_>>());
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	group.finish().unwrap();

	assert!(next_frame(&mut sub).await == payload, "the frame arrived corrupted");
	assert_eq!(setup.conn.stats().credit.evicted, 0);
}

/// A frame within one storage segment but larger than the whole window, which can never
/// complete: the stuck window evicts it like any other, so its reader learns the group
/// was evicted and is not fetched again.
#[tokio::test]
async fn lite_frame_past_the_window_gives_way() {
	let setup = setup("moq-lite-06", transport(WINDOW as u64)).await;
	let (low, mut low_sub) = setup.subscribe("low", 1).await;

	let mut low_frame = low
		.append_group()
		.unwrap()
		.create_frame(frame::Info {
			size: 2 * WINDOW as u64,
			timestamp: Some(Timestamp::ZERO),
		})
		.unwrap();
	low_frame.write(vec![1u8; 2 * WINDOW]).unwrap();

	let mut partial = partial_frame(&mut low_sub).await;
	loop {
		match kio::clock::timeout(TIMEOUT, partial.read_chunk())
			.await
			.expect("the partial frame neither failed nor grew")
		{
			Ok(Some(_)) => {}
			Ok(None) => panic!("a frame larger than the window completed"),
			Err(err) => {
				assert!(matches!(err, moq_net::Error::Evicted), "{err}");
				break;
			}
		}
	}

	drop(low_frame);
	let mut group = low.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, &b"after"[..]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut low_sub).await, &b"after"[..]);
}

/// A subscriber served on the raw driver with a connection window of [`WINDOW`], and a
/// publisher dialing it whose connection can also send raw streams.
struct Reversed {
	broadcast: moq_net::origin::Publication,
	remote: moq_net::broadcast::Consumer,
	/// The publisher's connection, for streams no session would send.
	conn: driver::connection::Connection,
	_sessions: (moq_net::Session, moq_net::Session),
	_endpoints: (driver::endpoint::Endpoint, driver::endpoint::Endpoint),
}

async fn reversed(version: &str) -> Reversed {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let version: moq_net::Version = version.parse().expect("version");
	let alpn = version.alpn().as_bytes().to_vec();

	let rcgen::CertifiedKey { cert, signing_key } =
		rcgen::generate_simple_self_signed(["localhost".to_string()]).expect("certificate");
	let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
	let mut tls = rustls::ServerConfig::builder()
		.with_no_client_auth()
		.with_single_cert(vec![cert.der().clone()], key.into())
		.expect("tls");
	tls.alpn_protocols = vec![alpn.clone()];
	let mut config = driver::server::Config::new(Arc::new(tls));
	config.transport = transport(WINDOW as u64);
	let server = driver::endpoint::Endpoint::new(
		driver::endpoint::Config::default().with_server(config),
		std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
	)
	.expect("server endpoint");
	let addr = server.local_addr();

	let mut tls = rustls::ClientConfig::builder()
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(AnyCert))
		.with_no_client_auth();
	tls.alpn_protocols = vec![alpn];
	let client = driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
		.expect("client endpoint");
	let (conn, accepted) = tokio::join!(
		async {
			client
				.connect(&driver::client::Config::new(Arc::new(tls)), addr, "localhost")
				.expect("connect")
				.await
				.expect("handshake")
		},
		async {
			server
				.accept()
				.await
				.expect("incoming")
				.accept()
				.expect("accept")
				.await
				.expect("accept")
		},
	);

	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("room").expect("broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let subscriber = moq_tokio::origin::spawn();
	let session = |conn: driver::connection::Connection| {
		moq_tokio::transport::Session::new(web::Session::raw(conn), moq_net::transport::Address::Setup)
	};
	// A moq-lite client's driver sends its half of the handshake, so it runs meanwhile.
	let client_session = tokio::spawn({
		let session = session(conn.clone());
		let publisher = publisher.consume();
		async move {
			let (client, driver) = moq_net::Client::new()
				.with_versions(version.into())
				.with_publisher(publisher)
				.connect(moq_net::time::Instant::now(), session)
				.await
				.expect("client handshake");
			tokio::spawn(moq_net::time::run(driver));
			client
		}
	});
	let (server_session, server_driver) = moq_net::Server::new()
		.with_versions(version.into())
		.with_subscriber(subscriber.clone())
		.accept(moq_net::time::Instant::now(), session(accepted))
		.await
		.expect("server handshake");
	tokio::spawn(moq_net::time::run(server_driver));
	let client_session = client_session.await.expect("client task");

	Reversed {
		broadcast,
		remote: room(&subscriber).await,
		conn,
		_sessions: (client_session, server_session),
		_endpoints: (client, server),
	}
}

impl Reversed {
	/// Subscribe to a new track at `priority`.
	async fn subscribe(&self, name: &str, priority: i8) -> (moq_net::track::Producer, moq_net::track::Subscriber) {
		let track = self.broadcast.create_track(name, timed()).unwrap();
		(track, subscribe(&self.remote, name, priority).await)
	}

	/// Send each of `streams` on a new stream to the subscriber, held open so only it can
	/// end the stream, and wait until it has stopped `stopped` of them. `bi` opens
	/// bidirectional ones, where moq-lite carries control requests.
	async fn raw(&self, bi: bool, streams: Vec<Vec<u8>>, stopped: usize) {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		for bytes in streams {
			let (conn, tx) = (self.conn.clone(), tx.clone());
			tokio::spawn(async move {
				let (mut send, _recv) = match bi {
					true => conn
						.open_bi(Default::default())
						.await
						.map(|(send, recv)| (send, Some(recv))),
					false => conn.open_uni(Default::default()).await.map(|send| (send, None)),
				}
				.expect("open");
				// The credit an eviction frees can reach the writer before the stop does, letting
				// the whole write queue, so only the stop itself proves the stream was stopped.
				let _ = send.write_all(&bytes[..]).await;
				if let Ok(Some(_)) = send.stopped().await {
					let _ = tx.send(());
				}
			});
		}
		for _ in 0..stopped {
			kio::clock::timeout(TIMEOUT, rx.recv())
				.await
				.expect("stop timed out")
				.expect("a writer");
		}
	}
}

/// A moq-lite message of type `kind` whose size prefix claims `size` bytes but carries
/// only `body` of them, so it never completes.
fn partial_message(kind: u8, size: u32, body: usize) -> Vec<u8> {
	assert!(body < size as usize);
	let mut bytes = vec![kind];
	// A 4-byte varint.
	bytes.extend_from_slice(&(0x8000_0000 | size).to_be_bytes());
	bytes.resize(bytes.len() + body, 0);
	bytes
}

/// Group headers that never complete fill the window: one is stopped, and a subscription
/// that starts afterwards still delivers.
#[tokio::test]
async fn unidentified_headers_give_way() {
	let setup = reversed("moq-lite-06").await;
	// GROUP, together more than the window and each short of its declared size.
	let partial = partial_message(0x00, (WINDOW / 2) as u32, WINDOW / 2 - 1);
	setup.raw(false, vec![partial; 2], 1).await;
	let (track, mut sub) = setup.subscribe("video", 1).await;
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, vec![3u8; 4096]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut sub).await.len(), 4096);
}

/// A moq-lite-07 varint below 2^28: leading one bits count the extra bytes.
fn varint(bytes: &mut Vec<u8>, value: u64) {
	let [_, b, c, d] = u32::try_from(value).unwrap().to_be_bytes();
	match value {
		0..0x80 => bytes.push(d),
		0x80..0x4000 => bytes.extend_from_slice(&[0x80 | c, d]),
		0x4000..0x20_0000 => bytes.extend_from_slice(&[0xc0 | b, c, d]),
		0x20_0000..0x1000_0000 => bytes.extend_from_slice(&(0xe000_0000 | value as u32).to_be_bytes()),
		_ => panic!("{value} needs a longer varint"),
	}
}

/// A moq-lite-07 GROUP stream for subscription `subscribe`, carrying one frame of `size`
/// bytes timed at zero.
fn group_stream(subscribe: u64, sequence: u64, size: usize) -> Vec<u8> {
	let mut header = Vec::new();
	varint(&mut header, subscribe);
	varint(&mut header, sequence);
	// The first frame.
	varint(&mut header, 0);

	// GROUP, then its size-prefixed header.
	let mut bytes = vec![0x00];
	varint(&mut bytes, header.len() as u64);
	bytes.extend_from_slice(&header);
	// The zigzag timestamp delta, then the frame's size and payload.
	varint(&mut bytes, 0);
	varint(&mut bytes, size as u64);
	bytes.resize(bytes.len() + size, 5);
	bytes
}

/// GROUP streams that beat their subscription's TRACK_INFO wait unread at the lowest
/// receive priority. When they fill the window, they give way before a more urgent track's
/// partial frame, the TRACK_INFO still arrives, and the groups that are left are then read.
#[tokio::test]
async fn groups_waiting_for_track_info_give_way_first() {
	let setup = reversed("moq-lite-07-wip").await;

	// Subscription 0: an urgent track's frame, half sent and holding its credit.
	let (urgent, mut urgent_sub) = setup.subscribe("urgent", 100).await;
	let mut frame = urgent
		.append_group()
		.unwrap()
		.create_frame(frame::Info {
			size: (WINDOW / 2) as u64,
			timestamp: Some(Timestamp::ZERO),
		})
		.unwrap();
	frame.write(vec![1u8; WINDOW / 4]).unwrap();
	let mut partial = partial_frame(&mut urgent_sub).await;
	read_partial(&mut partial, WINDOW / 4).await;

	// Subscription 1, sent alongside a TRACK whose TRACK_INFO the publisher holds back.
	let mut requests = setup.broadcast.requests();
	let every = Subscription::default()
		.with_order(moq_net::track::Order::Ascending)
		.with_range(0..)
		.with_delay(Delay::default().with_max(Duration::from_secs(60)));
	let waiting = tokio::spawn({
		let remote = setup.remote.clone();
		async move { remote.track("parked").unwrap().subscribe(every).await }
	});
	let request = kio::clock::timeout(TIMEOUT, requests.requested_track())
		.await
		.expect("TRACK timed out")
		.expect("TRACK");

	// Its groups, which together with the urgent frame overflow the window: one gives way.
	let groups = (0..4).map(|sequence| group_stream(1, sequence, WINDOW / 5)).collect();
	setup.raw(false, groups, 1).await;

	// The urgent frame was kept, and completes as more of the waiting groups give way.
	frame.write(vec![1u8; WINDOW / 4]).unwrap();
	read_partial(&mut partial, WINDOW / 4).await;
	assert!(
		kio::clock::timeout(TIMEOUT, partial.read_chunk())
			.await
			.expect("the urgent frame never ended")
			.expect("the urgent frame failed")
			.is_none(),
		"the urgent frame overran its size"
	);

	// The TRACK_INFO arrives, and the groups still waiting are read.
	let _track = request.accept(timed());
	let mut parked = kio::clock::timeout(TIMEOUT, waiting)
		.await
		.expect("subscribe timed out")
		.expect("subscribe task")
		.expect("subscribe");
	loop {
		let mut group = next_group(&mut parked).await;
		match kio::clock::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("a waiting group never delivered")
		{
			Ok(Some(frame)) => {
				assert_eq!(frame.payload.len(), WINDOW / 5);
				break;
			}
			// Stopped to make room for the urgent frame.
			Err(moq_net::Error::Evicted) => continue,
			other => panic!("group {} ended with {other:?}", group.sequence),
		}
	}
}

/// Control requests that never complete fill the window: one is stopped with its stream,
/// and the session carries on.
#[tokio::test]
async fn partial_control_requests_give_way() {
	let setup = reversed("moq-lite-06").await;
	// ANNOUNCE_PLEASE at its largest, five together more than the window and each short
	// of it: two have to go before the rest fit.
	let size = u16::MAX as usize;
	let partial = partial_message(0x01, size as u32, size - 1);
	setup.raw(true, vec![partial; 5], 2).await;
	let (track, mut sub) = setup.subscribe("video", 1).await;
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, vec![3u8; 4096]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut sub).await.len(), 4096);
}

/// A frame nearly as large as the window completes while partial frames of a less urgent
/// track keep arriving, each stopped as it gets in the way.
async fn the_largest_unit_completes_under_pressure(version: &str) {
	let setup = reversed(version).await;
	let (low, _low_sub) = setup.subscribe("low", 1).await;
	let (high, mut high_sub) = setup.subscribe("high", 100).await;

	// Several groups' worth of frames that never finish, opened before and after.
	let mut partial = Vec::new();
	let mut open_partial = || {
		let mut frame = low
			.append_group()
			.unwrap()
			.create_frame(frame::Info {
				size: WINDOW as u64,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		frame.write(vec![1u8; WINDOW / 4]).unwrap();
		partial.push(frame);
	};
	open_partial();
	open_partial();

	// Room for the stream's headers and nothing else.
	let payload = Bytes::from(vec![2u8; WINDOW - 64]);
	let mut group = high.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	group.finish().unwrap();
	open_partial();

	assert_eq!(next_frame(&mut high_sub).await.len(), payload.len(), "{version}");
}

#[tokio::test]
async fn lite_largest_unit_completes_under_pressure() {
	the_largest_unit_completes_under_pressure("moq-lite-06").await;
}

#[tokio::test]
async fn ietf_largest_unit_completes_under_pressure() {
	the_largest_unit_completes_under_pressure("moq-transport-18").await;
}

/// Drive the Bun sender through its commands against a subscriber of `origin` whose
/// connection window is `window`: an unsealed low-priority prefix gives way to a high-priority
/// frame, and the low track then keeps delivering. Waits for the sender to exit.
async fn javascript_exhausts(
	version: moq_net::Version,
	window: usize,
	origin: &moq_net::origin::Producer,
	mut child: tokio::process::Child,
) {
	let mut commands = child.stdin.take().unwrap();
	let remote = room(origin).await;
	let mut low = subscribe(&remote, "low", 1).await;
	let mut high = subscribe(&remote, "high", 100).await;

	// The sender holds back the rest, so no segment can seal and return this credit.
	commands.write_all(&b"low\n"[..]).await.unwrap();
	let mut partial = partial_frame(&mut low).await;
	read_partial(&mut partial, window * 3 / 4).await;

	// Half the window, which only fits once the low frame lets go.
	commands.write_all(&b"high\n"[..]).await.unwrap();
	assert_eq!(next_frame(&mut high).await, vec![2u8; window / 2], "{version}");
	let err = kio::clock::timeout(TIMEOUT, partial.read_chunk())
		.await
		.expect("stop timed out")
		.expect_err("the partial frame must be evicted");
	assert!(matches!(err, moq_net::Error::Evicted), "{version}: {err}");

	// The session survives, and the low track keeps delivering what fits.
	commands.write_all(&b"after\n"[..]).await.unwrap();
	assert_eq!(next_frame(&mut low).await, &b"after"[..], "{version}");

	drop(commands);
	let status = kio::clock::timeout(TIMEOUT, child.wait())
		.await
		.expect("sender exit timed out")
		.expect("sender");
	assert!(status.success(), "{version}: JS sender failed");
}

/// A Bun publisher (`test/receive-credit/sender.ts`) dials a WebTransport server whose
/// connection window is [`WINDOW`] and holds an unsealed low-priority prefix in a frame
/// declared twice that size. The receiver evicts it, a high-priority frame completes, and a
/// later low group arrives.
async fn javascript_partial_frame_gives_way(version: &str) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let version: moq_net::Version = version.parse().expect("version");

	// The sender pins this certificate, so it needs no name or CA.
	let rcgen::CertifiedKey { cert, signing_key } =
		rcgen::generate_simple_self_signed(["localhost".to_string()]).expect("certificate");
	let dir = tempfile::tempdir().expect("tempdir");
	let pem = dir.path().join("cert.pem");
	std::fs::write(&pem, cert.pem()).expect("write certificate");
	let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
	let mut tls = rustls::ServerConfig::builder()
		.with_no_client_auth()
		.with_single_cert(vec![cert.der().clone()], key.into())
		.expect("tls");
	tls.alpn_protocols = vec![web::ALPN.as_bytes().to_vec()];
	let mut config = driver::server::Config::new(Arc::new(tls));
	config.transport = transport(WINDOW as u64);
	let endpoint = driver::endpoint::Endpoint::new(
		driver::endpoint::Config::default().with_server(config),
		std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
	)
	.expect("endpoint");
	let port = endpoint.local_addr().port();
	let mut server = web::Server::new(endpoint);

	let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/receive-credit/sender.ts");
	let child = tokio::process::Command::new("bun")
		.arg(script)
		.arg(format!("https://127.0.0.1:{port}/"))
		.arg(version.to_string())
		.arg(&pem)
		.arg(WINDOW.to_string())
		.stdin(Stdio::piped())
		.stdout(Stdio::null())
		.stderr(Stdio::inherit())
		.kill_on_drop(true)
		.spawn()
		.expect("spawn bun");

	let request = kio::clock::timeout(TIMEOUT, server.accept())
		.await
		.expect("connect timeout")
		.expect("request");
	let session = request
		.respond(web::proto::ConnectResponse::OK.with_protocol(version.alpn()))
		.await
		.expect("respond");
	let origin = moq_tokio::origin::spawn();
	let (session, driver) = moq_net::Server::new()
		.with_versions(version.into())
		.with_subscriber(origin.clone())
		.accept(
			moq_net::time::Instant::now(),
			moq_tokio::transport::Session::new(session, moq_net::transport::Address::Uri),
		)
		.await
		.expect("server handshake");
	tokio::spawn(moq_net::time::run(driver));
	javascript_exhausts(version, WINDOW, &origin, child).await;
	drop(session);
}

/// The same exhaustion as [`javascript_partial_frame_gives_way`], over qmux on a WebSocket,
/// whose sessions lease receive credit like native QUIC. The window is moq-net's floor, the
/// smallest a qmux session takes.
async fn javascript_partial_frame_gives_way_websocket(version: &str) {
	const WINDOW: usize = moq_net::transport::MIN_RECEIVE_WINDOW as usize;
	let version: moq_net::Version = version.parse().expect("version");

	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
	let port = listener.local_addr().expect("addr").port();
	let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/receive-credit/sender.ts");
	let child = tokio::process::Command::new("bun")
		.arg(script)
		.arg(format!("http://127.0.0.1:{port}/"))
		.arg(version.to_string())
		.arg("websocket")
		.arg(WINDOW.to_string())
		.stdin(Stdio::piped())
		.stdout(Stdio::null())
		.stderr(Stdio::inherit())
		.kill_on_drop(true)
		.spawn()
		.expect("spawn bun");

	let (socket, _) = kio::clock::timeout(TIMEOUT, listener.accept())
		.await
		.expect("connect timeout")
		.expect("accept");
	socket.set_nodelay(true).expect("nodelay");
	let mut session_config = qmux::Config::default();
	session_config.limits.receive_window = std::num::NonZeroU64::new(WINDOW as u64);
	let (session, driver) = qmux::ws::Server::new()
		.with_protocol(version.alpn(), &[])
		.require_protocol()
		.with_session_config(session_config)
		.accept(socket)
		.await
		.expect("websocket handshake");
	tokio::spawn(driver);
	let origin = moq_tokio::origin::spawn();
	let (session, driver) = moq_net::Server::new()
		.with_versions(version.into())
		.with_subscriber(origin.clone())
		.accept(moq_net::time::Instant::now(), session)
		.await
		.expect("server handshake");
	tokio::spawn(moq_net::time::run(driver));
	javascript_exhausts(version, WINDOW, &origin, child).await;
	drop(session);
}

#[tokio::test]
#[ignore = "requires Bun; run by just test receive-credit in the interop workflow"]
async fn javascript_partial_frame_gives_way_qmux() {
	for version in ["moq-lite-06", "moq-transport-18"] {
		javascript_partial_frame_gives_way_websocket(version).await;
	}
}

#[tokio::test]
#[ignore = "requires Bun; run by just test receive-credit in the interop workflow"]
async fn javascript_partial_frame_gives_way_tokio() {
	for version in ["moq-lite-06", "moq-transport-18"] {
		javascript_partial_frame_gives_way(version).await;
	}
}

/// moq-tokio's default connection receive window.
const STOCK_WINDOW: u64 = moq_tokio::quic::DEFAULT_RECEIVE_WINDOW;
/// Bytes each loopback benchmark run delivers.
const BENCH_BYTES: usize = 256 << 20;
/// The benchmark's bottleneck, and the bytes a run delivers through it.
const BENCH_BLOAT: (u64, usize) = (200_000_000, 16 << 20);
/// Bytes each track keeps in flight, at least one group, so a track is not bound to its round
/// trip. Deeper pipelines of large frames evict: newest-first delivery starves a track's older
/// groups while newer ones arrive, until their remainders fill the window.
const INFLIGHT: usize = 1 << 20;

/// One benchmark configuration.
#[derive(Clone, Copy)]
struct Cell {
	/// The setting this cell varies from the default.
	axis: &'static str,
	/// Tracks writing at once, each its own stream per group.
	streams: usize,
	/// Bytes per frame, one frame per group.
	frame: usize,
	/// The receiver's connection receive window.
	window: u64,
	/// Whether each stream gets the whole connection window instead of the driver's finite default.
	follow: bool,
}

/// What one benchmark run measured.
struct Measured {
	/// The most credit the peer had spent at once: leased or not yet read.
	peak: u64,
	/// Payload bytes delivered per second.
	throughput: f64,
	/// How long frames took from the write to the receiver completing them, sorted.
	latency: Vec<Duration>,
	/// Datagrams the bottleneck's full queue dropped.
	overflowed: u64,
}

/// Deliver `bytes` over `cell.streams` tracks of one-frame groups, each track keeping
/// [`INFLIGHT`] bytes outstanding. Every incomplete frame fits the window alongside the
/// others, so nothing may be evicted, through `profile` or not.
async fn measure(cell: Cell, profile: Option<moq_shaper::Profile>, bytes: usize) -> Measured {
	let mut transport = transport(cell.window);
	transport.limits.max_streams = std::num::NonZeroU64::new(moq_tokio::quic::DEFAULT_MAX_STREAMS);
	if cell.follow {
		transport.limits.stream_receive_window = std::num::NonZeroU64::new(cell.window);
	}
	let setup = setup_through("moq-lite-06", transport, profile).await;
	let mut tracks = Vec::with_capacity(cell.streams);
	for index in 0..cell.streams {
		let name = index.to_string();
		let track = setup.broadcast.create_track(name.as_str(), timed()).unwrap();
		// Groups in flight fall behind the live edge, so a zero budget would skip them.
		let subscription = Subscription::default().with_delay(Delay::default().with_max(TIMEOUT));
		let sub = kio::clock::timeout(TIMEOUT, setup.remote.track(&name).unwrap().subscribe(subscription))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		tracks.push((track, sub));
	}

	let rounds = (bytes / cell.frame / cell.streams).max(1);
	let payload = Bytes::from(vec![7u8; cell.frame]);
	let start = kio::clock::now();
	let tasks: Vec<_> = tracks
		.into_iter()
		.map(|(track, mut sub)| {
			let payload = payload.clone();
			tokio::spawn(async move {
				let inflight = Arc::new(tokio::sync::Semaphore::new((INFLIGHT / payload.len()).max(1)));
				let sent = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
				let writer = tokio::spawn({
					let (inflight, sent, payload) = (inflight.clone(), sent.clone(), payload.clone());
					async move {
						for _ in 0..rounds {
							inflight.acquire().await.unwrap().forget();
							let mut group = track.append_group().unwrap();
							sent.lock().unwrap().insert(group.sequence, kio::clock::now());
							group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
							group.finish().unwrap();
						}
						track
					}
				});
				let mut latency = Vec::with_capacity(rounds);
				for _ in 0..rounds {
					let mut group = next_group(&mut sub).await;
					let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
						.await
						.expect("frame timed out")
						.expect("frame")
						.expect("group ended");
					assert_eq!(frame.payload.len(), payload.len());
					let sent = sent.lock().unwrap().remove(&group.sequence).expect("written");
					latency.push(sent.elapsed());
					inflight.add_permits(1);
				}
				// Keep the track open until every group is read.
				drop(writer.await.unwrap());
				latency
			})
		})
		.collect();
	let mut latency = Vec::with_capacity(rounds * cell.streams);
	for task in tasks {
		latency.extend(task.await.unwrap());
	}
	let elapsed = start.elapsed();
	latency.sort();
	let credit = setup.conn.stats().credit;
	assert_eq!(credit.evicted, 0, "pacing evicted a stream");
	Measured {
		peak: credit.peak_spent,
		throughput: (latency.len() * cell.frame) as f64 / elapsed.as_secs_f64(),
		latency,
		overflowed: setup
			.verify()
			.map_or(0, |stats| stats.down.overflowed + stats.up.overflowed),
	}
}

/// What bounded receive memory costs, on loopback and through a bufferbloated bottleneck.
///
/// Each axis varies one setting from the default cell (16 streams of 256 KiB frames, the
/// stock connection window, the driver's finite stream window): concurrent streams and frame
/// size, so a cost that grows with either shows up as a slope; the connection window, which
/// trades memory for throughput once it nears the path's bandwidth-delay product; and a
/// stream window that follows the connection window. Reports the peak credit the peer had
/// spent (leased plus unread), throughput of delivered frames, their completion latency, the
/// bottleneck's queue drops, each the median of the runs, and fails if any run evicted a stream. The
/// publisher is a stock server, so its send window bounds what is in flight as it would in
/// production. Run by nightly.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "release benchmark, run by nightly"]
async fn benchmark_receive_windows() {
	const RUNS: usize = 3;
	let default = Cell {
		axis: "default",
		streams: 16,
		frame: 256 * 1024,
		window: STOCK_WINDOW,
		follow: false,
	};
	let cells = std::iter::once(default)
		.chain([1, 256].map(|streams| Cell {
			axis: "streams",
			streams,
			..default
		}))
		.chain([16 * 1024, 4 << 20].map(|frame| Cell {
			axis: "frame",
			frame,
			..default
		}))
		.chain([8 << 20, 32 << 20].map(|window| Cell {
			axis: "window",
			window,
			..default
		}))
		.chain([4 << 20].map(|frame| Cell {
			axis: "follow",
			frame,
			follow: true,
			..default
		}));
	let paths = [
		("loopback", None, BENCH_BYTES),
		("bufferbloat", Some(bufferbloat(BENCH_BLOAT.0)), BENCH_BLOAT.1),
	];
	println!("path,axis,streams,frame,window_mib,stream_window,peak_bytes,mib_per_s,p50_ms,p99_ms,overflowed_packets");
	for (cell, (path, profile, bytes)) in cells.flat_map(|cell| paths.clone().map(|path| (cell, path))) {
		let mut runs = Vec::with_capacity(RUNS);
		for _ in 0..RUNS {
			runs.push(measure(cell, profile.clone(), bytes).await);
		}
		let median = |mut values: Vec<f64>| {
			values.sort_by(f64::total_cmp);
			values[values.len() / 2]
		};
		let percentile = |p: usize| {
			median(
				runs.iter()
					.map(|run| run.latency[(run.latency.len() - 1) * p / 100].as_secs_f64() * 1e3)
					.collect(),
			)
		};
		let peak = median(runs.iter().map(|run| run.peak as f64).collect());
		let throughput = median(runs.iter().map(|run| run.throughput).collect());
		let overflowed = median(runs.iter().map(|run| run.overflowed as f64).collect());
		println!(
			"{path},{},{},{},{},{},{peak:.0},{:.0},{:.2},{:.2},{overflowed}",
			cell.axis,
			cell.streams,
			cell.frame,
			cell.window >> 20,
			if cell.follow { "follow" } else { "finite" },
			throughput / (1 << 20) as f64,
			percentile(50),
			percentile(99),
		);
	}
}

/// Accepts any server certificate: the test dials its own self-signed server.
#[derive(Debug)]
pub(crate) struct AnyCert;

impl rustls::client::danger::ServerCertVerifier for AnyCert {
	fn verify_server_cert(
		&self,
		_: &rustls::pki_types::CertificateDer<'_>,
		_: &[rustls::pki_types::CertificateDer<'_>],
		_: &rustls::pki_types::ServerName<'_>,
		_: &[u8],
		_: rustls::pki_types::UnixTime,
	) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		Ok(rustls::client::danger::ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		_: &[u8],
		_: &rustls::pki_types::CertificateDer<'_>,
		_: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
	}

	fn verify_tls13_signature(
		&self,
		_: &[u8],
		_: &rustls::pki_types::CertificateDer<'_>,
		_: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		rustls::crypto::aws_lc_rs::default_provider()
			.signature_verification_algorithms
			.supported_schemes()
	}
}
