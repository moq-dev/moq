//! Follow a real JS QMux server's redirect, retaining the reader across the epoch-proven handover.
use std::process::Stdio;

use anyhow::{Context, ensure};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let version = std::env::args()
		.nth(1)
		.context("usage: interop-goaway-client VERSION")?;
	let mut child = tokio::process::Command::new("bun")
		.args([
			concat!(
				env!("CARGO_MANIFEST_DIR"),
				"/../../test/interop/clients/js-native/server.ts"
			),
			"goaway",
			&version,
		])
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.kill_on_drop(true)
		.spawn()?;
	let mut output = tokio::io::BufReader::new(child.stdout.take().context("stdout")?).lines();
	let urls: Vec<String> = serde_json::from_str(&output.next_line().await?.context("JS endpoint URLs")?)?;
	ensure!(urls.len() == 2 && urls[0] != urls[1], "missing replacement endpoint");
	let mut config = moq_tokio::connect::Config::default();
	config.version.push(version.parse().map_err(anyhow::Error::msg)?);
	let client = config.init(Default::default())?;
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let connection = client
		.clone()
		.with_subscriber(origin)
		.connect(url::Url::parse(&urls[0])?)
		.established()
		.await?;
	ensure!(
		connection.version() == Some(version.parse().map_err(anyhow::Error::msg)?),
		"wrong negotiated version"
	);
	let broadcast = consumer.follow("handover", None)?.available().await?;
	let mut track = broadcast.track("video")?.subscribe(None).await?;
	let input = child.stdin.as_mut().context("stdin")?;
	for sequence in 0..2 {
		let mut group = track.recv_group().await?.context("reader ended during GOAWAY")?;
		ensure!(group.sequence == sequence, "wrong group after migration");
		ensure!(
			group.read_frame().await?.context("empty group")?.payload.as_ref() == [sequence as u8; 32],
			"corrupt group"
		);
		ensure!(group.read_frame().await?.is_none(), "extra frame");
		if sequence == 0 {
			input.write_all(b"first group\n").await?;
			// The first endpoint's publisher scope excludes this broadcast, so its bytes
			// prove that the replacement session is serving beyond its SETUP handshake.
			let ready = consumer.follow("replacement", None)?.available().await?;
			let mut marker = ready.track("ready")?.subscribe(None).await?;
			let mut marker = marker.recv_group().await?.context("replacement marker")?;
			ensure!(
				marker.read_frame().await?.context("marker frame")?.payload.as_ref() == [42],
				"wrong marker"
			);
			input.write_all(b"replacement ready\n").await?;
		}
	}
	input.write_all(b"done\n").await?;
	ensure!(child.wait().await?.success(), "JS server failed");
	let _ = connection.close().await;
	client.close().await;
	eprintln!("reverse goaway resumed {version}");
	Ok(())
}
