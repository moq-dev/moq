//! A JavaScript publisher and reader, both over WebSocket, through a Rust relay with more
//! live tracks and groups than qmux's default limit of 100 streams each way.
//! It needs Bun, so it runs in the interop workflow.
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

const TIMEOUT: Duration = Duration::from_secs(15);
const TRACKS: usize = 200;

fn client(url: &url::Url, mode: &str) -> std::io::Result<tokio::process::Child> {
	let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/stream-limits/client.ts");
	tokio::process::Command::new("bun")
		.arg(script)
		.arg(url.as_str())
		.arg(mode)
		.arg(TRACKS.to_string())
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::inherit())
		.kill_on_drop(true)
		.spawn()
}

async fn relay() -> anyhow::Result<()> {
	let relay = moq_tokio::origin::spawn();
	let ws = moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse()?).await?;
	let url: url::Url = format!("http://{}/", ws.local_addr()?).parse()?;
	let mut config = moq_tokio::server::Config::default();
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

	// Each of the relay's subscriptions is a bidirectional stream the publisher accepts.
	let mut publisher = client(&url, "publish")?;
	let mut line = String::new();
	BufReader::new(publisher.stdout.take().unwrap())
		.read_line(&mut line)
		.await?;
	anyhow::ensure!(line.trim() == "ready", "JS publisher did not become ready: {line}");

	// Each live group is a unidirectional stream the reader holds open.
	let mut reader = client(&url, "subscribe")?;
	anyhow::ensure!(reader.wait().await?.success(), "JS reader failed");
	drop(publisher.stdin.take());
	anyhow::ensure!(publisher.wait().await?.success(), "JS publisher failed");
	serve.await??;
	Ok(())
}

#[tokio::test]
#[ignore = "requires Bun; run by just test stream-limits in the interop workflow"]
async fn javascript_over_websocket() {
	kio::clock::timeout(TIMEOUT, relay()).await.unwrap().unwrap();
}
