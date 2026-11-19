//! Datagram tracks between Rust and JavaScript on lite-07, direct and through a Rust relay.
//!
//! QMux on WebSocket carries the datagrams in order, so every one written after the
//! subscriber's answer must arrive. Broadcast `dg` holds a timed and an untimed datagram
//! track, and a stream track sharing the timed one's name, which must stay independent of
//! it. The JavaScript end is `test/datagram-tracks/client.ts`, which holds the same
//! expectations; it needs Bun, so the interop workflow runs this.
use std::{process::Stdio, time::Duration};

use bytes::Bytes;
use moq_net::{Timescale, Timestamp, datagram};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const TIMEOUT: Duration = Duration::from_secs(15);
const VERSION: &str = "moq-lite-07-wip";

/// The subscriber's priority on `audio`, which the publisher waits to see before writing.
const PRIORITY: i8 = 17;
/// `audio`'s sequence, timestamp in microseconds, and payload. Past 2^53, so a lossy number
/// conversion anywhere on the path shows.
const AUDIO: [(u64, u64, &[u8]); 2] = [(1, 1_234_567, &[1, 2, 3]), ((1 << 53) + 1, 2_000_000, &[4, 5])];
/// `events`' sequence and payload, untimed.
const EVENTS: [(u64, &[u8]); 2] = [(0, b"hello"), (7, b"")];
/// The stream track's one frame: its timestamp in milliseconds and payload.
const FRAME: (u64, &[u8]) = (5, &[9]);

/// One ordered line from a JavaScript peer's stdout.
async fn expect_line(lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>, want: &str) {
	let line = lines
		.next_line()
		.await
		.expect("read the JS peer")
		.expect("JS peer exited");
	assert_eq!(line, want, "JS peer line");
}

async fn run(relay: bool, js_publishes: bool) {
	let version: moq_net::Version = VERSION.parse().unwrap();
	let ws = moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse().unwrap())
		.await
		.unwrap()
		.with_protocols([version.alpn()])
		.unwrap();
	let url: url::Url = format!("ws://{}", ws.local_addr().unwrap()).parse().unwrap();
	let mut config = moq_tokio::server::Config::default();
	config.listen.version = vec![version];
	config.websocket = Some(ws);
	let mut server = config.init_streams().unwrap().listen().await.unwrap();

	// The Rust end's origin: the server's own session when direct, or a client of the relay.
	let local = moq_tokio::origin::spawn();
	let serve = {
		let local = local.clone();
		tokio::spawn(async move {
			// A relay carries what it learned on to its other peers (moq-lite-07).
			let relayed = moq_tokio::origin::spawn().with_proxy(true);
			let mut sessions = Vec::new();
			for _ in 0..if relay { 2 } else { 1 } {
				let request = server.accept().await.unwrap();
				let request = match (relay, js_publishes) {
					(true, _) => request.with_publisher(&relayed).with_subscriber(relayed.clone()),
					(false, true) => request.with_subscriber(local.clone()),
					(false, false) => request.with_publisher(&local),
				};
				sessions.push(request.ok().await.unwrap());
			}
			for session in sessions {
				let _ = session.closed().await;
			}
		})
	};
	let mut client = moq_tokio::connect::Config::default();
	client.version = vec![version];
	let client = client.init(Default::default()).unwrap();

	let spawn = |mode: &str| {
		let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/datagram-tracks/client.ts");
		let mut child = tokio::process::Command::new("bun")
			.arg(script)
			.arg(url.as_str())
			.arg(VERSION)
			.arg(mode)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit())
			.kill_on_drop(true)
			.spawn()
			.expect("spawn bun");
		let lines = BufReader::new(child.stdout.take().unwrap()).lines();
		(child, lines)
	};

	let mut sessions = Vec::new();
	if js_publishes {
		let (mut child, mut lines) = spawn("publish");
		expect_line(&mut lines, "ready").await;
		if relay {
			sessions.push(
				client
					.with_subscriber(local.clone())
					.with_reconnect(false)
					.connect(url.clone())
					.established()
					.await
					.unwrap(),
			);
		}
		let consumer = local.consume();
		let mut announced = consumer.announced();
		// The `Live` marker can come first while the relay is still learning the route.
		while !matches!(
			announced.next().await.expect("announcements closed"),
			moq_net::announce::Event::Present(_)
		) {}
		let broadcast = consumer.request("dg", None).await.unwrap();
		let mut audio = broadcast.datagrams("audio").unwrap();
		audio.set_priority(PRIORITY);
		let mut events = broadcast.datagrams("events").unwrap();
		let mut track = broadcast.track("audio").unwrap().subscribe(None).await.unwrap();
		let info = audio.info().await.unwrap();
		assert_eq!(info.timescale, Some(Timescale::MICRO), "audio timescale");
		assert_eq!(info.priority, -7, "audio publisher priority");
		let info = events.info().await.unwrap();
		assert_eq!(info.timescale, None, "events are untimed");
		assert_eq!(info.priority, 0, "events publisher priority");
		assert_eq!(track.info().timescale, Some(Timescale::MILLI), "stream track timescale");

		let stdin = child.stdin.as_mut().unwrap();
		stdin.write_all(b"send\n").await.unwrap();
		// Datagrams are unordered, so each track's arrive in any order.
		let mut got = Vec::new();
		for _ in AUDIO {
			got.push(audio.recv().await.unwrap().expect("audio ended early"));
		}
		got.sort_by_key(|datagram| datagram.sequence);
		for ((sequence, micros, payload), got) in AUDIO.into_iter().zip(got) {
			assert_eq!(got.sequence, sequence, "audio sequence");
			assert_eq!(
				got.timestamp.as_micros().unwrap(),
				u128::from(micros),
				"audio timestamp"
			);
			assert_eq!(got.payload, payload, "audio payload");
		}
		let mut got = Vec::new();
		for _ in EVENTS {
			got.push(events.recv().await.unwrap().expect("events ended early"));
		}
		got.sort_by_key(|datagram| datagram.sequence);
		for ((sequence, payload), got) in EVENTS.into_iter().zip(got) {
			assert_eq!(got.sequence, sequence, "events sequence");
			assert_eq!(
				got.timestamp,
				Timestamp::unscaled(sequence),
				"a scale-less datagram is stamped with its sequence"
			);
			assert_eq!(got.payload, payload, "events payload");
		}
		let mut group = track.recv_group().await.unwrap().expect("stream track ended early");
		let frame = group.read_frame().await.unwrap().expect("group ended early");
		assert_eq!(
			frame.timestamp.as_millis().unwrap(),
			u128::from(FRAME.0),
			"frame timestamp"
		);
		assert_eq!(frame.payload, FRAME.1, "frame payload");

		stdin.write_all(b"finish\n").await.unwrap();
		assert_eq!(audio.recv().await.unwrap(), None, "audio ends");
		assert_eq!(events.recv().await.unwrap(), None, "events end");
		assert!(track.recv_group().await.unwrap().is_none(), "stream track ends");
		drop(child.stdin.take());
		assert!(child.wait().await.unwrap().success(), "JS publisher failed");
	} else {
		let route = local.route("dg", moq_net::origin::RouteOptions::hidden()).unwrap();
		let mut audio = route
			.broadcast()
			.create_datagrams(
				"audio",
				datagram::Info::default()
					.with_timescale(Timescale::MICRO)
					.with_priority(-7),
			)
			.unwrap();
		let mut events = route
			.broadcast()
			.create_datagrams("events", datagram::Info::default())
			.unwrap();
		let mut track = route
			.broadcast()
			.create_track(
				"audio",
				moq_net::track::Info::default().with_timescale(Timescale::MILLI),
			)
			.unwrap();
		route.announce().unwrap();
		if relay {
			sessions.push(
				client
					.with_publisher(&local)
					.with_reconnect(false)
					.connect(url.clone())
					.established()
					.await
					.unwrap(),
			);
		}

		let (mut child, mut lines) = spawn("subscribe");
		expect_line(&mut lines, "subscribed").await;
		while audio.priority_changed().await != Some(PRIORITY) {}
		for (sequence, micros, payload) in AUDIO {
			audio
				.insert(datagram::Datagram {
					sequence,
					timestamp: Timestamp::from_micros(micros).unwrap(),
					payload: Bytes::from_static(payload),
				})
				.unwrap();
		}
		for (sequence, payload) in EVENTS {
			events
				.insert(datagram::Datagram {
					sequence,
					timestamp: Timestamp::unscaled(sequence),
					payload: Bytes::from_static(payload),
				})
				.unwrap();
		}
		track
			.write_frame(Timestamp::from_millis(FRAME.0).unwrap(), FRAME.1)
			.unwrap();

		expect_line(&mut lines, "received").await;
		audio.finish();
		events.finish();
		let _track = track.finish().unwrap();
		drop(child.stdin.take());
		assert!(child.wait().await.unwrap().success(), "JS subscriber failed");
	}
	drop(sessions);
	serve.await.unwrap();
}

#[tokio::test]
#[ignore = "requires Bun; run by just test datagram-tracks in the interop workflow"]
async fn datagram_tracks_javascript() {
	for relay in [false, true] {
		for js_publishes in [false, true] {
			kio::clock::timeout(TIMEOUT, run(relay, js_publishes))
				.await
				.unwrap_or_else(|_| panic!("timed out: relay {relay}, JS publishes {js_publishes}"));
		}
	}
}
