//! `moq import rtsp` over a real relay: every RTSP session is its own broadcast under a fresh
//! epoch, so a camera whose RTP time restarts never splices onto the broadcast before it.

use std::process::Stdio;
use std::time::Duration;

use moq_rtsp::fake::{self, Media, Server};
use moq_tokio::moq_net;
use tokio::io::AsyncReadExt;
use tokio::process::Child;

const TIMEOUT: Duration = Duration::from_secs(10);
/// 30 fps at the RTP video clock.
const FRAME: u32 = 3000;

/// Await `step` while `moq` runs, failing as soon as it exits.
async fn running<T>(child: &mut Child, what: &str, step: impl Future<Output = T>) -> T {
	tokio::select! {
		out = step => out,
		status = child.wait() => panic!("moq exited with {} before {what}", status.expect("wait for moq")),
		_ = moq_clock::sleep(TIMEOUT) => panic!("{what} timed out"),
	}
}

/// Start `moq import rtsp`, publishing `camera` from `rtsp` to the relay at `url`.
fn import(url: &str, fingerprint: &str, rtsp: &str) -> Child {
	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	// A `MOQ_*` variable in the developer's shell must not reconfigure the child.
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(name);
		}
	}
	command
		.args([
			"--connect",
			url,
			"--connect-tls-fingerprint",
			fingerprint,
			"--broadcast",
			"camera",
			"import",
			"rtsp",
			"--connect",
			rtsp,
		])
		.env("RUST_LOG", "info")
		.env("NO_COLOR", "1")
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.spawn()
		.expect("spawn moq")
}

/// Wait for `camera` to be announced, then read its first catalog and video group.
async fn next_broadcast(
	child: &mut Child,
	consumer: &moq_net::origin::Consumer,
	announced: &mut moq_net::announce::Consumer,
) -> hang::catalog::Consumer {
	loop {
		match running(child, "the announce", announced.next())
			.await
			.expect("origin closed")
		{
			moq_net::announce::Event::Start(_)
			| moq_net::announce::Event::Restart(_)
			| moq_net::announce::Event::Takeover { .. } => break,
			moq_net::announce::Event::End(_) | moq_net::announce::Event::Update(_) => {}
		}
	}
	let broadcast = running(child, "the broadcast", consumer.request("camera", None))
		.await
		.expect("announced broadcast resolves");
	let mut catalogs = running(
		child,
		"the catalog",
		hang::catalog::Catalog::<()>::subscribe(&broadcast),
	)
	.await
	.expect("subscribe to the catalog");
	let catalog = running(child, "the first catalog", catalogs.next())
		.await
		.expect("catalog read")
		.expect("a catalog");
	assert_eq!(
		catalog.video.renditions.len(),
		1,
		"a fresh catalog with the session's video"
	);
	assert_eq!(
		catalog.audio.renditions.len(),
		1,
		"a fresh catalog with the session's audio"
	);

	let name = catalog.video.renditions.keys().next().unwrap();
	let mut track = running(child, "the video", broadcast.track(name).unwrap().subscribe(None))
		.await
		.expect("subscribe to the video");
	running(child, "the first group", track.recv_group())
		.await
		.expect("video group")
		.expect("the broadcast starts on a keyframe");
	catalogs
}

/// The first broadcast ends cleanly rather than vanishing: its catalog finishes.
async fn finished(child: &mut Child, mut catalogs: hang::catalog::Consumer) {
	loop {
		match running(child, "the catalog to finish", catalogs.next()).await {
			Ok(Some(_)) => {}
			Ok(None) => return,
			Err(err) => panic!("the catalog ends with {err} instead of finishing"),
		}
	}
}

#[tokio::test]
async fn every_rtsp_session_is_a_fresh_broadcast() {
	let _ = moq_tokio::crypto::install_default();
	let fixture = moq_relay::test_relay().await.expect("test relay");
	let ready = fixture.relay.ready();
	tokio::spawn(fixture.relay.run());
	ready.wait().await.expect("relay ready");

	let server = Server::bind(vec![Media::H264 { sprop: true }, Media::Aac]).await;
	let mut child = import(fixture.url.as_str(), &fixture.fingerprint, server.url().as_str());
	let mut stderr = child.stderr.take().expect("stderr");
	let log = tokio::spawn(async move {
		let mut log = String::new();
		stderr.read_to_string(&mut log).await.expect("read stderr");
		log
	});

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

	// The first session, whose RTP clock then resets mid-stream.
	let mut session = running(&mut child, "the first PLAY", server.accept(90_000)).await;
	session.video(0, 90_000, &[fake::H264_IDR]).await;
	session.aac(1, 90_000, b"first").await;
	let catalogs = next_broadcast(&mut child, &consumer, &mut announced).await;
	session.video(0, 90_000 + FRAME, &[fake::H264_SLICE]).await;
	session.video(0, 0, &[fake::H264_IDR]).await;
	finished(&mut child, catalogs).await;

	// The jump ends the run, and the import redials: a new session from RTP time zero.
	let mut session = running(&mut child, "the second PLAY", server.accept(0)).await;
	session.video(0, 0, &[fake::H264_IDR]).await;
	session.aac(1, 0, b"second").await;
	let catalogs = next_broadcast(&mut child, &consumer, &mut announced).await;

	// The camera restarts: the session ends, and the import reconnects after its backoff.
	session.close().await;
	finished(&mut child, catalogs).await;
	let mut session = running(&mut child, "the third PLAY", server.accept(0)).await;
	session.video(0, 0, &[fake::H264_IDR]).await;
	session.aac(1, 0, b"third").await;
	next_broadcast(&mut child, &consumer, &mut announced).await;

	child.kill().await.expect("kill moq");
	let log = log.await.expect("stderr task");
	let mut epochs: Vec<&str> = log
		.lines()
		.filter(|line| line.contains("publishing RTSP broadcast"))
		.filter_map(|line| line.split("epoch=").nth(1))
		.map(|rest| rest.split_whitespace().next().unwrap())
		.collect();
	assert_eq!(epochs.len(), 3, "one broadcast per session:\n{log}");
	epochs.sort_unstable();
	epochs.dedup();
	assert_eq!(epochs.len(), 3, "every session mints its own epoch:\n{log}");
}
