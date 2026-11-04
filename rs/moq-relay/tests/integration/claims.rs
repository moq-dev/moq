//! JS claim answers cross the actual relay's routing and Rust control decoder.
#![cfg(all(feature = "websocket", feature = "_quic"))]

use std::time::Duration;

#[tokio::test]
#[ignore = "requires Bun; run by just test interop"]
async fn javascript_claim_restart() {
	let _ = moq_tokio::crypto::install_default();
	let mut config = moq_relay::Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.generate = vec!["localhost".into()];
	config.web.http.listen = Some("127.0.0.1:0".parse().unwrap());
	config.listen.version = vec!["moq-lite-07-wip".parse().unwrap()];
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.drain_timeout = Some(Duration::ZERO.into());
	let relay = moq_relay::Relay::load(config).await.unwrap().with_signals(false);
	let addr = relay.web_addrs().http.unwrap();
	let trigger = relay.shutdown_trigger().clone();
	let ready = relay.ready();
	let running = tokio::spawn(relay.run());
	ready.wait().await.unwrap();
	let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/interop/claims.ts");
	let mut child = tokio::process::Command::new("bun")
		.arg(script)
		.arg(format!("ws://{addr}/"))
		.kill_on_drop(true)
		.spawn()
		.unwrap();
	let status = kio::clock::timeout(Duration::from_secs(15), child.wait()).await;
	if status.is_err() {
		child.kill().await.unwrap();
	}
	trigger.start();
	running.await.unwrap().unwrap();
	assert!(status.expect("JS claim restart timed out").unwrap().success());
}
