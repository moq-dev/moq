//! Real WebSocket sessions through a Rust relay, with Rust and JavaScript at either end.
//! The JS arm needs Bun and runs in the interop workflow; the Rust arm runs in normal CI.
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

const TIMEOUT: Duration = Duration::from_secs(15);
const AGES: [Option<Duration>; 3] = [None, Some(Duration::ZERO), Some(Duration::from_secs(30))];

async fn relay(version: moq_net::Version, javascript: Option<bool>) -> anyhow::Result<()> {
	let mut cache = moq_net::origin::Config::default();
	cache.cache_duration = Duration::from_secs(1);
	let relay = moq_tokio::origin::spawn_config(cache);
	let ws = moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse()?)
		.await?
		.with_protocols([version.alpn()])?;
	let addr = ws.local_addr()?;
	let mut config = moq_tokio::server::Config::default();
	config.listen.version = vec![version];
	config.websocket = Some(ws);
	let mut server = config.init_streams()?.listen().await?;
	let serve = tokio::spawn(async move {
		let mut sessions = Vec::new();
		for _ in 0..2 {
			let request = server.accept().await.unwrap();
			sessions.push(
				request
					.with_publisher(&relay)
					.with_subscriber(relay.clone())
					.ok()
					.await?,
			);
		}
		for session in sessions {
			let _ = session.closed().await;
		}
		Ok::<_, anyhow::Error>(())
	});
	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("age")?;
	let tracks: Vec<_> = AGES
		.into_iter()
		.enumerate()
		.map(|(i, age)| broadcast.create_track(i.to_string(), moq_net::track::Info::default().with_max_age(age)))
		.collect::<Result<_, _>>()?;
	broadcast.announce(Default::default())?;
	let subscriber = moq_tokio::origin::spawn();
	let consumer = subscriber.consume();
	let mut announced = consumer.announced();
	let mut client_config = moq_tokio::connect::Config::default();
	client_config.version = vec![version];
	let client = client_config.init(Default::default())?;
	let url: url::Url = format!("ws://{addr}").parse()?;
	let mut rust_sessions = Vec::new();
	if javascript != Some(true) {
		rust_sessions.push(
			client
				.clone()
				.with_publisher(&publisher)
				.with_reconnect(false)
				.connect(url.clone())
				.established()
				.await?,
		);
	}
	let mut child = if let Some(publish) = javascript {
		let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/max-age/client.ts");
		let mut child = tokio::process::Command::new("bun")
			.env("NODE_ENV", "production")
			.arg(script)
			.arg(url.as_str())
			.arg(version.alpn())
			.arg(if publish { "publish" } else { "subscribe" })
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit())
			.kill_on_drop(true)
			.spawn()?;
		if publish {
			let mut line = String::new();
			BufReader::new(child.stdout.take().unwrap())
				.read_line(&mut line)
				.await?;
			anyhow::ensure!(line.trim() == "ready", "JS publisher did not become ready: {line}");
		}
		Some(child)
	} else {
		None
	};
	if javascript != Some(false) {
		rust_sessions.push(
			client
				.with_subscriber(subscriber)
				.with_reconnect(false)
				.connect(url)
				.established()
				.await?,
		);
		// The `Live` marker can come first while the publisher is still connecting.
		while !matches!(
			announced
				.next()
				.await
				.ok_or_else(|| anyhow::anyhow!("announcements closed"))?,
			moq_net::announce::Event::Announced(_)
		) {}
		let front = consumer.request_broadcast("age").await?;
		for (i, age) in AGES.into_iter().enumerate() {
			let track = front.track(&i.to_string())?.subscribe(None).await?;
			anyhow::ensure!(
				track.info().max_age == age,
				"{version}: track {i} age {:?}, wanted {age:?}",
				track.info().max_age
			);
		}
	}
	if let Some(child) = child.as_mut() {
		drop(child.stdin.take());
		anyhow::ensure!(child.wait().await?.success(), "JS client failed on {version}");
	}
	drop(rust_sessions);
	drop(tracks);
	drop(broadcast);
	serve.await??;
	Ok(())
}

#[tokio::test]
async fn max_age_relay_rust() {
	for version in ["moq-lite-07-wip", "moq-transport-17", "moq-transport-22"] {
		tokio::time::timeout(TIMEOUT, relay(version.parse().unwrap(), None))
			.await
			.unwrap()
			.unwrap();
	}
}

#[tokio::test]
#[ignore = "requires Bun; run by just test max-age in the interop workflow"]
async fn max_age_relay_javascript() {
	for version in ["moq-lite-07-wip", "moq-transport-17", "moq-transport-22"] {
		for publish in [false, true] {
			tokio::time::timeout(TIMEOUT, relay(version.parse().unwrap(), Some(publish)))
				.await
				.unwrap()
				.unwrap();
		}
	}
}
