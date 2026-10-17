//! Over native QUIC, incomplete frames hold the receiver's connection credit, and when
//! they fill its window the receiver stops the least urgent stream and keeps going.
//!
//! The receiver runs on the raw driver so its connection window can be far smaller than
//! the stack allows, which keeps the payloads small. Without recovery the high-priority
//! frame never arrives: the window is spent on a low-priority frame that can never
//! complete. The Rust arm dials a stock server. The JavaScript arm serves WebTransport
//! to a Bun publisher; it needs Bun and runs in the interop workflow.

#![cfg(feature = "quic")]

use std::{process::Stdio, sync::Arc, time::Duration};

use bytes::Bytes;
use moq_tokio::moq_net::{self, Timestamp, frame, track::Subscription};
use moq_tokio::quic::{driver, web};
use tokio::io::AsyncWriteExt;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The receiver's connection receive window.
const WINDOW: usize = 256 * 1024;

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Raw-driver transport settings with a connection receive window of `window`.
fn transport(window: u64) -> driver::TransportConfig {
	let mut transport = driver::TransportConfig::default();
	transport.receive_window(driver::VarInt::from_u64(window).unwrap());
	transport
}

/// A server publishing broadcast `room`, and a client subscribed to its origin over a
/// connection with the given transport settings.
struct Setup {
	broadcast: moq_net::broadcast::Producer,
	remote: moq_net::broadcast::Consumer,
	/// The client's connection, for its credit stats.
	conn: driver::Connection,
	_client: moq_net::Session,
	_endpoint: driver::Endpoint,
}

async fn setup(version: &str, transport: driver::TransportConfig) -> Setup {
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
	let port = server.local_addr().expect("addr").port();
	tokio::spawn(async move {
		let request = server.accept().await.expect("accept");
		let (session, _driver) = request.with_publisher(&publisher).ok().await.expect("session");
		let _ = session.closed().await;
	});

	let mut tls = rustls::ClientConfig::builder()
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(AnyCert))
		.with_no_client_auth();
	tls.alpn_protocols = vec![version.alpn().as_bytes().to_vec()];
	let quic = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");
	let mut client = driver::ClientConfig::new(Arc::new(quic));
	client.transport_config(Arc::new(transport));

	let endpoint = driver::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("endpoint");
	let conn = endpoint
		.connect_with(client, ([127, 0, 0, 1], port).into(), "localhost")
		.expect("connect")
		.await
		.expect("handshake");

	let subscriber = moq_tokio::origin::spawn();
	let session =
		moq_tokio::transport::Session::new(web::Session::raw(conn.clone()), moq_net::transport::Binding::InBand);
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
		_client: client,
		_endpoint: endpoint,
	}
}

/// Broadcast `room`, once the peer announces it into `origin`.
async fn room(origin: &moq_net::origin::Producer) -> moq_net::broadcast::Consumer {
	let consumer = origin.consume();
	tokio::time::timeout(TIMEOUT, consumer.routed("room"))
		.await
		.expect("announce timeout")
		.expect("routed");
	tokio::time::timeout(TIMEOUT, consumer.request_broadcast("room", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
}

/// Subscribe to track `name` of `remote` at `priority`.
async fn subscribe(remote: &moq_net::broadcast::Consumer, name: &str, priority: u8) -> moq_net::track::Subscriber {
	tokio::time::timeout(
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
	async fn subscribe(&self, name: &str, priority: u8) -> (moq_net::track::Producer, moq_net::track::Subscriber) {
		let track = self.broadcast.create_track(name, timed()).unwrap();
		(track, subscribe(&self.remote, name, priority).await)
	}
}

/// The next group, which must arrive in time.
async fn next_group(sub: &mut moq_net::track::Subscriber) -> moq_net::group::Consumer {
	tokio::time::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("group timed out")
		.expect("group")
		.expect("track ended")
}

/// The next frame's payload, which must arrive in time.
async fn next_frame(sub: &mut moq_net::track::Subscriber) -> Bytes {
	let mut group = next_group(sub).await;
	tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timed out")
		.expect("frame")
		.expect("group ended")
		.payload
}

/// The next group's first frame, still arriving.
async fn partial_frame(sub: &mut moq_net::track::Subscriber) -> moq_net::frame::Consumer {
	let mut group = next_group(sub).await;
	tokio::time::timeout(TIMEOUT, group.next_frame())
		.await
		.expect("frame header timed out")
		.expect("frame")
		.expect("group ended")
}

/// Read `frame` until `bytes` of it arrived.
async fn read_partial(frame: &mut moq_net::frame::Consumer, bytes: usize) {
	let mut seen = 0;
	while seen < bytes {
		let chunk = tokio::time::timeout(TIMEOUT, frame.read_chunk())
			.await
			.expect("partial payload timed out")
			.expect("chunk")
			.expect("frame ended");
		seen += chunk.len();
	}
}

/// Two partial frames that together exceed the window: the lower-priority one is
/// stopped, and the higher-priority one completes on the credit it returns.
async fn the_less_urgent_partial_frame_gives_way(version: &str) {
	let setup = setup(version, transport(WINDOW as u64)).await;
	let (low, mut low_sub) = setup.subscribe("low", 1).await;
	let (high, mut high_sub) = setup.subscribe("high", 200).await;

	// Three quarters of the window, held by a frame that never finishes.
	let mut low_group = low.append_group().unwrap();
	let mut low_frame = low_group
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
	drop(low_group);
	let mut group = low.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, &b"after"[..]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut low_sub).await, &b"after"[..], "{version}");
}

#[tokio::test]
async fn lite_partial_frame_gives_way() {
	the_less_urgent_partial_frame_gives_way("moq-lite-06").await;
}

#[tokio::test]
async fn ietf_partial_frame_gives_way() {
	the_less_urgent_partial_frame_gives_way("moq-transport-16").await;
}

/// A frame larger than the whole window, which can never complete: the stuck window
/// evicts it like any other, so its reader learns the group was evicted and is not
/// fetched again.
#[tokio::test]
async fn lite_frame_past_the_window_gives_way() {
	let setup = setup("moq-lite-06", transport(WINDOW as u64)).await;
	let (low, mut low_sub) = setup.subscribe("low", 1).await;

	let mut low_group = low.append_group().unwrap();
	let mut low_frame = low_group
		.create_frame(frame::Info {
			size: 2 * WINDOW as u64,
			timestamp: Some(Timestamp::ZERO),
		})
		.unwrap();
	low_frame.write(vec![1u8; 2 * WINDOW]).unwrap();

	let mut partial = partial_frame(&mut low_sub).await;
	loop {
		match tokio::time::timeout(TIMEOUT, partial.read_chunk())
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
	drop(low_group);
	let mut group = low.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, &b"after"[..]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut low_sub).await, &b"after"[..]);
}

/// A subscriber served on the raw driver with a connection window of [`WINDOW`], and a
/// publisher dialing it whose connection can also send raw streams.
struct Reversed {
	broadcast: moq_net::broadcast::Producer,
	remote: moq_net::broadcast::Consumer,
	/// The publisher's connection, for streams no session would send.
	conn: driver::Connection,
	_sessions: (moq_net::Session, moq_net::Session),
	_endpoints: (driver::Endpoint, driver::Endpoint),
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
	let quic = driver::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic config");
	let mut config = driver::ServerConfig::with_crypto(Arc::new(quic));
	config.transport_config(Arc::new(transport(WINDOW as u64)));
	let server = driver::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).expect("server endpoint");
	let addr = server.local_addr().expect("addr");

	let mut tls = rustls::ClientConfig::builder()
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(AnyCert))
		.with_no_client_auth();
	tls.alpn_protocols = vec![alpn];
	let quic = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");
	let client = driver::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("client endpoint");
	let (conn, accepted) = tokio::join!(
		async {
			client
				.connect_with(driver::ClientConfig::new(Arc::new(quic)), addr, "localhost")
				.expect("connect")
				.await
				.expect("handshake")
		},
		async { server.accept().await.expect("incoming").await.expect("accept") },
	);

	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("room").expect("broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let subscriber = moq_tokio::origin::spawn();
	let session = |conn: driver::Connection| {
		moq_tokio::transport::Session::new(web::Session::raw(conn), moq_net::transport::Binding::InBand)
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
	async fn subscribe(&self, name: &str, priority: u8) -> (moq_net::track::Producer, moq_net::track::Subscriber) {
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
					true => conn.open_bi().await.map(|(send, recv)| (send, Some(recv))),
					false => conn.open_uni().await.map(|send| (send, None)),
				}
				.expect("open");
				if send.write_all(&bytes).await.is_ok() {
					std::future::pending::<()>().await;
				}
				let _ = tx.send(());
			});
		}
		for _ in 0..stopped {
			tokio::time::timeout(TIMEOUT, rx.recv())
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
	let (high, mut high_sub) = setup.subscribe("high", 200).await;

	// Several groups' worth of frames that never finish, opened before and after.
	let mut partial = Vec::new();
	let mut open_partial = || {
		// Leaked, so the frame can borrow its group for the rest of the test.
		let group = Box::leak(Box::new(low.append_group().unwrap()));
		let mut frame = group
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
	the_largest_unit_completes_under_pressure("moq-transport-16").await;
}

/// A Bun publisher (`test/receive-credit/sender.ts`) dials a WebTransport server whose
/// connection window is [`WINDOW`] and sends a low-priority frame twice that size. The
/// receiver stops it, a high-priority frame completes, and a later low group arrives.
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
	let quic = driver::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic config");
	let mut config = driver::ServerConfig::with_crypto(Arc::new(quic));
	config.transport_config(Arc::new(transport(WINDOW as u64)));
	let endpoint = driver::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).expect("endpoint");
	let port = endpoint.local_addr().expect("addr").port();
	let mut server = web::Server::new(endpoint);

	let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/receive-credit/sender.ts");
	let mut child = tokio::process::Command::new("bun")
		.arg(script)
		.arg(format!("https://127.0.0.1:{port}/"))
		.arg(version.alpn())
		.arg(&pem)
		.arg(WINDOW.to_string())
		.stdin(Stdio::piped())
		.stdout(Stdio::null())
		.stderr(Stdio::inherit())
		.kill_on_drop(true)
		.spawn()
		.expect("spawn bun");
	let mut commands = child.stdin.take().unwrap();

	let request = tokio::time::timeout(TIMEOUT, server.accept())
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
			moq_tokio::transport::Session::new(session, moq_net::transport::Binding::Uri),
		)
		.await
		.expect("server handshake");
	tokio::spawn(moq_net::time::run(driver));
	let remote = room(&origin).await;
	let mut low = subscribe(&remote, "low", 1).await;
	let mut high = subscribe(&remote, "high", 200).await;

	// Wait until three quarters of the window holds a frame that can never complete.
	commands.write_all(b"low\n").await.unwrap();
	let mut partial = partial_frame(&mut low).await;
	read_partial(&mut partial, WINDOW * 3 / 4).await;

	// Half the window, which only fits once the low frame lets go.
	commands.write_all(b"high\n").await.unwrap();
	assert_eq!(next_frame(&mut high).await, vec![2u8; WINDOW / 2], "{version}");
	loop {
		let chunk = tokio::time::timeout(TIMEOUT, partial.read_chunk())
			.await
			.expect("stop timed out");
		match chunk {
			Ok(Some(_)) => {}
			Ok(None) => panic!("{version}: a frame larger than the window completed"),
			Err(_) => break,
		}
	}

	// The session survives, and the low track keeps delivering what fits.
	commands.write_all(b"after\n").await.unwrap();
	assert_eq!(next_frame(&mut low).await, &b"after"[..], "{version}");

	drop(commands);
	let status = tokio::time::timeout(TIMEOUT, child.wait())
		.await
		.expect("sender exit timed out")
		.expect("sender");
	assert!(status.success(), "{version}: JS sender failed");
	drop(session);
}

#[tokio::test]
#[ignore = "requires Bun; run by just test receive-credit in the interop workflow"]
async fn javascript_partial_frame_gives_way_tokio() {
	for version in ["moq-lite-06", "moq-transport-16"] {
		javascript_partial_frame_gives_way(version).await;
	}
}

/// moq-tokio's default connection receive window.
const STOCK_WINDOW: u64 = 128 << 20;
/// Bytes each benchmark run delivers.
const BENCH_BYTES: usize = 256 << 20;

/// What one benchmark run measured.
struct Measured {
	/// The most the receiver held plus received without reading, in bytes.
	retained: u64,
	/// Payload bytes delivered per second.
	throughput: f64,
	/// How long frames took from the write to the receiver completing them, sorted.
	latency: Vec<Duration>,
}

/// Deliver [`BENCH_BYTES`] over `streams` tracks of one-frame groups of `frame` bytes,
/// each track writing its next group once the last arrived, so `streams` streams are
/// open at once. `follow` gives each stream the whole connection window; otherwise it
/// keeps the driver's finite default.
async fn measure(streams: usize, frame: usize, follow: bool) -> Measured {
	let mut transport = transport(STOCK_WINDOW);
	let max = driver::VarInt::from_u64(moq_tokio::quic::DEFAULT_MAX_STREAMS).unwrap();
	transport.max_concurrent_uni_streams(max);
	transport.max_concurrent_bidi_streams(max);
	if follow {
		transport.stream_receive_window(driver::VarInt::from_u64(STOCK_WINDOW).unwrap());
	}
	let setup = setup("moq-lite-06", transport).await;
	let mut tracks = Vec::with_capacity(streams);
	for index in 0..streams {
		tracks.push(setup.subscribe(&index.to_string(), 0).await);
	}

	let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
	let sampler = tokio::spawn({
		let conn = setup.conn.clone();
		let done = done.clone();
		async move {
			let mut peak = 0;
			while !done.load(std::sync::atomic::Ordering::Relaxed) {
				// Every byte whose credit has not come back: leased or not yet read.
				peak = peak.max(STOCK_WINDOW.saturating_sub(conn.stats().credit.available));
				tokio::time::sleep(Duration::from_millis(1)).await;
			}
			peak
		}
	});

	let rounds = (BENCH_BYTES / frame / streams).max(1);
	let payload = Bytes::from(vec![7u8; frame]);
	let start = std::time::Instant::now();
	let tasks: Vec<_> = tracks
		.into_iter()
		.map(|(track, mut sub)| {
			let payload = payload.clone();
			tokio::spawn(async move {
				let mut latency = Vec::with_capacity(rounds);
				for _ in 0..rounds {
					let sent = std::time::Instant::now();
					let mut group = track.append_group().unwrap();
					group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
					group.finish().unwrap();
					assert_eq!(next_frame(&mut sub).await.len(), payload.len());
					latency.push(sent.elapsed());
				}
				latency
			})
		})
		.collect();
	let mut latency = Vec::with_capacity(rounds * streams);
	for task in tasks {
		latency.extend(task.await.unwrap());
	}
	let elapsed = start.elapsed();
	done.store(true, std::sync::atomic::Ordering::Relaxed);
	latency.sort();
	Measured {
		retained: sampler.await.unwrap(),
		throughput: (rounds * streams * frame) as f64 / elapsed.as_secs_f64(),
		latency,
	}
}

/// What bounded receive memory costs: a finite stream window against one that follows the
/// connection window, swept over concurrent streams and frame sizes on separate axes so
/// a cost that grows with either shows up as a slope. Reports the peak retained bytes
/// (leased plus unread), throughput, and frame completion latency, each the median
/// of the runs. The publisher is a stock server, so its send window bounds what is in
/// flight as it would in production. Run by nightly.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "release benchmark, run by nightly"]
async fn benchmark_receive_windows() {
	const RUNS: usize = 3;
	const FRAME: usize = 256 * 1024;
	const STREAMS: usize = 16;
	let cells = [1, 16, 256]
		.map(|streams| ("streams", streams, FRAME))
		.into_iter()
		.chain([16 * 1024, 256 * 1024, 4 << 20].map(|frame| ("frame", STREAMS, frame)));
	println!("axis,streams,frame,stream_window,retained_mib,mib_per_s,p50_ms,p99_ms");
	for (axis, streams, frame) in cells {
		let mut runs: [Vec<Measured>; 2] = Default::default();
		// Alternate the modes so drift in the host hits both alike.
		for _ in 0..RUNS {
			for (mode, follow) in [false, true].into_iter().enumerate() {
				runs[mode].push(measure(streams, frame, follow).await);
			}
		}
		for (runs, mode) in runs.iter().zip(["finite", "follow"]) {
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
			let retained = median(runs.iter().map(|run| run.retained as f64).collect());
			let throughput = median(runs.iter().map(|run| run.throughput).collect());
			println!(
				"{axis},{streams},{frame},{mode},{:.1},{:.0},{:.2},{:.2}",
				retained / (1 << 20) as f64,
				throughput / (1 << 20) as f64,
				percentile(50),
				percentile(99),
			);
		}
	}
}

/// Accepts any server certificate: the test dials its own self-signed server.
#[derive(Debug)]
struct AnyCert;

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
