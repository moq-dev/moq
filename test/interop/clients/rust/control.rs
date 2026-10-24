//! Real Rust server for a JS TRACK_STATUS requester and small-window publisher.
use std::process::Stdio;

use anyhow::{Context, ensure};
use futures::FutureExt;
use moq_tokio::moq_net;
use tokio::io::AsyncWriteExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(args.len() == 4, "usage: interop-control status|credit VERSION node|bun");
	let version: moq_net::Version = args[2].parse().map_err(anyhow::Error::msg)?;
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let broadcast = origin.create_broadcast("control")?;
	let _published = if args[1] == "status" {
		let track = broadcast.create_track("video", moq_net::track::Info::default().with_priority(2))?;
		let mut group = track.create_group(moq_net::group::Info { sequence: 4 })?;
		for byte in [1, 2] {
			group.write_frame(None, vec![byte])?;
		}
		group.finish()?;
		broadcast.announce(Default::default())?;
		Some(track)
	} else {
		None
	};
	let socket = moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse()?).await?;
	let url = format!("http://{}/", socket.local_addr()?);
	let mut config = moq_tokio::server::Config::default();
	config.listen.version.push(version);
	config.websocket = Some(socket);
	if args[1] == "status" {
		config.publisher = Some(origin.consume());
	} else {
		config.subscriber = Some(origin);
	}
	let mut listener = config.init_streams()?.listen().await?;
	let mut command = tokio::process::Command::new(&args[3]);
	if args[3] == "node" {
		command.args(["--import", "tsx"]);
	}
	let mut child = command
		.args([
			concat!(
				env!("CARGO_MANIFEST_DIR"),
				"/../../test/interop/clients/js-native/control.ts"
			),
			&args[1],
			&url,
			&args[2],
		])
		.stdin(Stdio::piped())
		.kill_on_drop(true)
		.spawn()?;
	let (session, driver) = listener.accept().await.context("accept")?.ok().await?;
	ensure!(session.version() == version, "wrong negotiated version");
	if args[1] == "credit" {
		// The JS peer has one usable track slot beyond any namespace discovery.
		let remote = consumer.routed_broadcast("control").await?;
		let mut first = remote.track("first")?.subscribe(None).await?;
		let second = remote.track("second")?;
		let mut pending = Box::pin(second.subscribe(None));
		ensure!(
			pending.as_mut().now_or_never().is_none(),
			"second track bypassed request credit"
		);
		let input = child.stdin.as_mut().context("child stdin")?;
		input.write_all(b"probe\n").await?;
		// This round trip keeps the first request alive while the second is queued.
		// Sending it without credit would close the JS peer before UNSUBSCRIBE can free a slot.
		let mut group = first.recv_group().await?.context("probe group")?;
		ensure!(
			group.read_frame().await?.context("probe frame")?.payload.as_ref() == [1],
			"wrong probe"
		);
		drop(group);
		drop(first);
		let mut second = pending.await?;
		let mut group = second.recv_group().await?.context("second group")?;
		ensure!(
			group.read_frame().await?.context("second frame")?.payload.as_ref() == [2],
			"wrong resumed track"
		);
		input.write_all(b"done\n").await?;
	}
	ensure!(child.wait().await?.success(), "JS peer failed");
	session.abort(moq_net::Error::Cancel);
	let _ = driver.await?;
	eprintln!("{} interop passed {version}", args[1]);
	Ok(())
}
