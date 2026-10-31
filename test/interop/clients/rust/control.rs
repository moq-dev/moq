//! Real Rust server for a JS TRACK_STATUS requester.
use anyhow::{Context, ensure};
use moq_tokio::moq_net;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(
		args.len() == 4 && args[1] == "status",
		"usage: interop-control status VERSION node|bun"
	);
	let version: moq_net::Version = args[2].parse().map_err(anyhow::Error::msg)?;
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("control")?;
	let track = broadcast.create_track("video", moq_net::track::Info::default().with_priority(2))?;
	let mut group = track.create_group(moq_net::group::Info { sequence: 4 })?;
	for byte in [1, 2] {
		group.write_frame(None, vec![byte])?;
	}
	group.finish()?;
	broadcast.announce(Default::default())?;

	let socket = moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse()?).await?;
	let url = format!("http://{}/", socket.local_addr()?);
	let mut config = moq_tokio::server::Config::default();
	config.listen.version.push(version);
	config.websocket = Some(socket);
	config.publisher = Some(origin.consume());
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
			&url,
			&args[2],
		])
		.kill_on_drop(true)
		.spawn()?;
	let session = listener.accept().await.context("accept")?.ok().await?;
	ensure!(session.version() == version, "wrong negotiated version");
	ensure!(child.wait().await?.success(), "JS peer failed");
	session.abort(moq_net::Error::Cancel);
	let _ = session.closed().await;
	drop(track);
	eprintln!("status interop passed {version}");
	Ok(())
}
