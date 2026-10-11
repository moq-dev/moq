//! `moq import` over a real relay: at stdin EOF the process finishes its tracks and
//! exits, and a subscriber sees the catalog finish rather than the publisher vanish.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Child;

const TIMEOUT: Duration = Duration::from_secs(10);
const BBB: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb.ts");

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

#[tokio::test]
async fn import_delivers_the_catalog_finish_at_eof() {
	let _ = moq_tokio::crypto::install_default();
	let fixture = moq_relay::test_relay().await.expect("test relay");
	let ready = fixture.relay.ready();
	tokio::spawn(fixture.relay.run());
	ready.wait().await.expect("relay ready");

	let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq"));
	// A `MOQ_*` variable in the developer's shell must not reconfigure the child.
	for (name, _) in std::env::vars_os() {
		if name.to_string_lossy().starts_with("MOQ_") {
			command.env_remove(name);
		}
	}
	let mut child = command
		.args([
			"--connect",
			fixture.url.as_str(),
			"--connect-tls-fingerprint",
			&fixture.fingerprint,
			"--broadcast",
			"demo",
			"import",
			"ts",
		])
		.stdin(Stdio::piped())
		.kill_on_drop(true)
		.spawn()
		.expect("spawn moq");
	let mut stdin = child.stdin.take().expect("stdin");
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
