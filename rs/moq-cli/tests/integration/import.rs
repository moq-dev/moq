//! `moq import` over a real relay: at stdin EOF the process finishes its tracks and
//! exits, and a subscriber sees the catalog finish rather than the publisher vanish.

use std::process::Stdio;
use std::time::Duration;

use hang::moq_net;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;

const TIMEOUT: Duration = Duration::from_secs(10);
/// How long a finished import stays up: long enough for a subscriber to request its media
/// after reading the final catalog, short enough that the process exits within `TIMEOUT`.
const MAX_AGE: &str = "5s";
const BBB: &[u8] = include_bytes!("../../../moq-mux/src/container/ts/test_data/scte35/bbb5s.ts");

/// Await `step` while `moq` still holds stdin open, failing as soon as the process exits.
///
/// Until stdin ends, an exit is the publisher failing, so report its status rather than
/// waiting out `TIMEOUT` and blaming the step.
async fn running<T>(child: &mut Child, what: &str, step: impl Future<Output = T>) -> T {
	tokio::select! {
		out = step => out,
		status = child.wait() => panic!("moq exited with {} before {what}", status.expect("wait for moq")),
		_ = tokio::time::sleep(TIMEOUT) => panic!("{what} timed out"),
	}
}

/// Start `moq import ts`, publishing `demo` from stdin to the relay at `url`.
fn import(url: &str, fingerprint: &str) -> Child {
	import_as(url, fingerprint, &[], "demo")
}

/// Start `moq <global> import ts`, publishing `name` from stdin to the relay at `url`.
fn import_as(url: &str, fingerprint: &str, global: &[&str], name: &str) -> Child {
	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	// A `MOQ_*` variable in the developer's shell must not reconfigure the child.
	for (var, _) in std::env::vars_os() {
		if var.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(var);
		}
	}
	command
		.args([
			"--connect",
			url,
			"--connect-tls-fingerprint",
			fingerprint,
			"--broadcast",
			name,
		])
		.args(global)
		.args(["import", "--max-age", MAX_AGE, "ts"])
		.stdin(Stdio::piped())
		.kill_on_drop(true)
		.spawn()
		.expect("spawn moq")
}

#[tokio::test]
async fn import_delivers_the_catalog_finish_at_eof() {
	let _ = moq_tokio::crypto::install_default();
	let fixture = moq_relay::test_relay().await.expect("test relay");
	let ready = fixture.relay.ready();
	tokio::spawn(fixture.relay.run());
	ready.wait().await.expect("relay ready");

	let mut child = import(fixture.url.as_str(), &fixture.fingerprint);
	let mut stdin = child.stdin.take().expect("stdin");
	// A live TS catalog waits for its initial two-second PCR rate measurement.
	stdin.write_all(BBB).await.expect("write stdin");

	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let mut announced = consumer.announced();
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	let _connection = config
		.init(Default::default())
		.expect("client")
		.with_subscriber(origin)
		.with_reconnect(false)
		.connect(fixture.url.clone())
		.established()
		.await
		.expect("subscriber connects");

	let event = running(&mut child, "the announce", announced.next())
		.await
		.expect("origin closed");
	let moq_tokio::moq_net::announce::Event::Start(announce) = event else {
		panic!("the first announce event is {event:?}, not a start");
	};
	assert_eq!(announce.prefix.as_str(), "demo");
	let broadcast = running(&mut child, "the broadcast", consumer.request_broadcast("demo", None))
		.await
		.expect("announced broadcast resolves");
	let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&broadcast)
		.await
		.expect("subscribe to the catalog");

	// The relay is serving the catalog before stdin ends, so the finish is queued on a
	// live subscription when the process exits.
	running(&mut child, "the first catalog", catalogs.next())
		.await
		.expect("catalog read")
		.expect("a catalog");

	drop(stdin);
	let status = tokio::time::timeout(TIMEOUT, child.wait())
		.await
		.expect("moq never exited")
		.expect("wait for moq");
	assert!(status.success(), "moq exited with {status}");

	loop {
		match tokio::time::timeout(TIMEOUT, catalogs.next())
			.await
			.expect("the catalog never ended")
		{
			Ok(Some(_)) => {}
			Ok(None) => break,
			Err(err) => panic!("the catalog ends with {err} instead of finishing"),
		}
	}
}

/// Two importers sharing an epoch publish one timeline from wherever each joined the source: the
/// root clock the epoch places, PTS zero one 33-bit wrap before its time, and the same timestamp
/// on every frame both carry.
#[tokio::test]
async fn importers_sharing_an_epoch_publish_identical_timestamps() {
	let _ = moq_tokio::crypto::install_default();
	let fixture = moq_relay::test_relay().await.expect("test relay");
	let origin = fixture.relay.cluster().origin.consume();
	let ready = fixture.relay.ready();
	tokio::spawn(fixture.relay.run());
	ready.wait().await.expect("relay ready");

	let epoch = moq_net::Epoch::mint();
	let global = ["--epoch", epoch.as_str()];
	// The second joins a third of the way in, at the next PAT and PMT.
	let join = BBB.len() / 188 / 3 * 188;
	let mut published = Vec::new();
	for (name, input) in [("first", BBB), ("second", &BBB[join..])] {
		let mut child = import_as(fixture.url.as_str(), &fixture.fingerprint, &global, name);
		let mut stdin = child.stdin.take().expect("stdin");
		stdin.write_all(input).await.expect("write stdin");
		let broadcast = running(&mut child, "the broadcast", origin.routed_broadcast(name))
			.await
			.expect("announced broadcast resolves");
		let mut catalogs = running(
			&mut child,
			"the catalog subscription",
			hang::catalog::Catalog::<()>::subscribe(&broadcast),
		)
		.await
		.expect("subscribe to the catalog");
		let catalog = running(&mut child, "the first catalog", catalogs.next())
			.await
			.expect("catalog read")
			.expect("a catalog");
		let video = catalog
			.video
			.renditions
			.keys()
			.next()
			.expect("a video rendition")
			.clone();
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(60));
		let mut track = running(
			&mut child,
			"the video subscription",
			broadcast.track(&video).expect("track").subscribe(subscription),
		)
		.await
		.expect("subscribe to the video");
		drop(stdin);

		let mut frames = std::collections::HashSet::new();
		while let Some(mut group) = tokio::time::timeout(TIMEOUT, track.recv_group())
			.await
			.expect("the video never ended")
			.expect("video group")
		{
			while let Some(frame) = group.read_frame().await.expect("video frame") {
				let frame = hang::container::Frame::decode(frame.payload).expect("a timestamped frame");
				frames.insert((frame.timestamp.as_micros(), frame.payload));
			}
		}
		let status = tokio::time::timeout(TIMEOUT, child.wait())
			.await
			.expect("moq never exited")
			.expect("wait for moq");
		assert!(status.success(), "{name} exited with {status}");
		published.push((catalog.clock, frames));
	}

	let zero = epoch.time() - Duration::from_micros(95_443_717_689);
	let micros = zero.duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64
		- hang::catalog::MOQ_EPOCH_UNIX_MILLIS * 1_000;
	let clock = hang::catalog::Clock::new(moq_net::Timestamp::from_micros(micros).unwrap()).unwrap();
	let [(first_clock, first), (second_clock, second)] = &published[..] else {
		unreachable!("two importers");
	};
	assert_eq!(*first_clock, Some(clock), "the epoch places the root clock");
	assert_eq!(second_clock, first_clock);
	assert!(
		!second.is_empty() && second.len() < first.len(),
		"{} of {}",
		second.len(),
		first.len()
	);
	let moved: Vec<_> = second.difference(first).map(|(micros, _)| *micros).collect();
	assert!(
		moved.is_empty(),
		"frames the second published at other timestamps: {moved:?}"
	);
}

/// About one second of CBR HEVC and Opus: shorter than the two-second mux-rate window, so
/// the catalog is only released at EOF.
const SHORT: &[u8] = include_bytes!("../../../moq-mux/src/container/ts/test_data/bbb_cbr.ts")
	.split_at(256 * 188)
	.0;

/// A subscriber already waiting when a short import ends still receives its media: the
/// import announces at the finish, once its catalog lists the tracks, and keeps them servable
/// for their max-age after EOF.
#[tokio::test]
async fn a_short_import_drains_its_media_to_a_waiting_subscriber() {
	let _ = moq_tokio::crypto::install_default();
	let fixture = moq_relay::test_relay().await.expect("test relay");
	let ready = fixture.relay.ready();
	tokio::spawn(fixture.relay.run());
	ready.wait().await.expect("relay ready");

	let mut child = import(fixture.url.as_str(), &fixture.fingerprint);
	let mut stdin = child.stdin.take().expect("stdin");

	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let mut announced = consumer.announced();
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	let _connection = config
		.init(Default::default())
		.expect("client")
		.with_subscriber(origin)
		.with_reconnect(false)
		.connect(fixture.url.clone())
		.established()
		.await
		.expect("subscriber connects");

	stdin.write_all(SHORT).await.expect("write stdin");
	drop(stdin);

	running(&mut child, "the announce", announced.next())
		.await
		.expect("origin closed");
	let broadcast = running(&mut child, "the broadcast", consumer.request_broadcast("demo", None))
		.await
		.expect("announced broadcast resolves");
	let mut catalogs = running(
		&mut child,
		"the catalog subscription",
		hang::catalog::Catalog::<()>::subscribe(&broadcast),
	)
	.await
	.expect("subscribe to the catalog");

	let catalog = tokio::time::timeout(TIMEOUT, catalogs.next())
		.await
		.expect("no catalog")
		.expect("catalog read")
		.expect("the import released its catalog");
	let name = catalog
		.video
		.renditions
		.keys()
		.next()
		.expect("a video rendition")
		.clone();
	let mut track = tokio::time::timeout(TIMEOUT, broadcast.track(&name).expect("track").subscribe(None))
		.await
		.expect("the video subscription timed out")
		.expect("subscribe to the video");
	let mut frames = 0;
	while let Some(mut group) = tokio::time::timeout(TIMEOUT, track.recv_group())
		.await
		.expect("the video never ended")
		.expect("video group")
	{
		while group.read_frame().await.expect("video frame").is_some() {
			frames += 1;
		}
	}
	assert!(frames > 0, "the subscriber received no video");

	let status = tokio::time::timeout(TIMEOUT, child.wait())
		.await
		.expect("moq never exited")
		.expect("wait for moq");
	assert!(status.success(), "moq exited with {status}");
}
