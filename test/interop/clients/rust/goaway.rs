//! A real Rust server redirects one JS subscriber between two WebSocket endpoints.
use std::process::Stdio;

use anyhow::{Context, ensure};
use moq_tokio::moq_net;
use tokio::io::AsyncBufReadExt;

async fn endpoint(
	origin: &moq_net::origin::Producer,
	version: moq_net::Version,
) -> anyhow::Result<(moq_tokio::server::Listener, String)> {
	let socket = moq_tokio::websocket::Listener::bind("127.0.0.1:0".parse()?).await?;
	let url = format!("http://{}/", socket.local_addr()?);
	let mut config = moq_tokio::server::Config::default();
	config.listen.version.push(version);
	config.websocket = Some(socket);
	config.publisher = Some(origin.consume());
	Ok((config.init_streams()?.listen().await?, url))
}

fn write(track: &mut moq_net::track::Producer, sequence: u64) -> anyhow::Result<()> {
	let mut group = track.append_group()?;
	ensure!(group.sequence == sequence, "wrong local sequence");
	group.write_frame(None, vec![sequence as u8; 32])?;
	group.finish()?;
	Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(args.len() == 3, "usage: interop-goaway VERSION node|bun");
	let version: moq_net::Version = args[1].parse().map_err(anyhow::Error::msg)?;
	// Without an epoch, JS re-subscribes instead of retaining its reader across the move.
	let legacy = ["moq-transport-14", "moq-transport-16"].contains(&args[1].as_str());
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("handover")?;
	let mut track = broadcast.create_track("video", moq_net::track::Info::default())?;
	broadcast.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))?;
	let (mut first, first_url) = endpoint(&origin, version).await?;
	let (mut second, second_url) = endpoint(&origin, version).await?;
	let script = concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/../../test/interop/clients/js-native/goaway.ts"
	);
	let mut command = tokio::process::Command::new(&args[2]);
	if args[2] == "node" {
		command.args(["--import", "tsx"]);
	}
	let mut child = command
		.args([script, &first_url, &second_url, &args[1]])
		.stdout(Stdio::piped())
		.kill_on_drop(true)
		.spawn()?;
	let mut output = tokio::io::BufReader::new(child.stdout.take().context("child stdout")?).lines();
	let (session, driver) = first.accept().await.context("first accept")?.ok().await?;
	ensure!(session.version() == version, "wrong first negotiated version");
	let mut demand = track.demand();
	while demand.wanted_changed().await?.is_none() {}
	write(&mut track, 0)?;
	// A JS acknowledgement, not a timer, determines when the first group was read.
	ensure!(
		output.next_line().await?.as_deref() == Some("first group"),
		"JS did not read first group"
	);
	session.drain().send(moq_net::goaway::Goaway::redirect(&second_url))?;
	let (replacement, replacement_driver) = second.accept().await.context("replacement accept")?.ok().await?;
	ensure!(replacement.version() == version, "wrong replacement negotiated version");
	// First exercise the replacement while the drained route still serves. A JS
	// acknowledgement of its namespace proves more than the SETUP handshake.
	write(&mut track, 1)?;
	ensure!(
		output.next_line().await?.as_deref() == Some("replacement ready"),
		"replacement route did not arrive"
	);
	// Only the replacement can deliver the next group after this transport is cut.
	session.abort(moq_net::Error::Cancel);
	let _ = driver.await?;
	if legacy {
		ensure!(
			output.next_line().await?.as_deref() == Some("resubscribed"),
			"JS did not re-subscribe through the replacement"
		);
	}
	write(&mut track, 2)?;
	ensure!(
		output.next_line().await?.as_deref() == Some("goaway resumed"),
		"JS subscription did not resume"
	);
	ensure!(child.wait().await?.success(), "JS client failed");
	replacement.abort(moq_net::Error::Cancel);
	let _ = replacement_driver.await?;
	eprintln!("goaway redirect resumed {version}");
	Ok(())
}
