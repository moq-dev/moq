//! Compressed-catalog interop publisher. The harness acknowledges every reader on stdin.
//!
//! Publishes one hang catalog on `catalog.json`, compressed as every hang publisher does, over
//! moq-lite-07 alone so the relay receives and stores the DEFLATE slices. Prints the catalog
//! group's inflated size, which is what a relay serving plain readers must inflate exactly once.
use anyhow::{Context, ensure};
use moq_tokio::moq_net;
use tokio::io::AsyncBufReadExt;

/// Renditions enough for the catalog to compress, and for readers to check what they decoded.
const CATALOG: &str = r#"{
	"video": {"renditions": {
		"hd": {"codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720, "container": {"kind": "legacy"}},
		"sd": {"codec": "avc1.64001e", "codedWidth": 640, "codedHeight": 360, "container": {"kind": "legacy"}}
	}},
	"audio": {"renditions": {
		"en": {"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "container": {"kind": "legacy"}},
		"fr": {"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "container": {"kind": "legacy"}}
	}}
}"#;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(args.len() == 3, "usage: interop-catalog URL BROADCAST");
	let url: url::Url = args[1].parse()?;
	let mut config = moq_tokio::connect::Config::default();
	config
		.version
		.push("moq-lite-07-wip".parse().map_err(anyhow::Error::msg)?);
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();

	let broadcast = origin.create_broadcast(&args[2])?;
	let track = broadcast.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())?;
	ensure!(track.info().compress, "hang catalogs are compressed");
	let mut catalog = moq_json::snapshot::Producer::<hang::Catalog>::new(track, Default::default());
	let value = hang::Catalog::from_slice(CATALOG.as_bytes())?;
	catalog.update(moq_net::Timed::from(&value).at(moq_net::Timestamp::ZERO))?;
	broadcast.announce(Default::default())?;

	// A local reader sees the inflated snapshot, as a plain peer of the relay does. The group
	// stays open for deltas, and this catalog never writes one.
	let mut local = catalog.consume();
	let mut group = local.recv_group().await?.context("no catalog group")?;
	let bytes = group.read_frame().await?.context("no catalog snapshot")?.payload.len();

	let session = client
		.clone()
		.with_publisher(&origin)
		.connect(url)
		.established()
		.await?;
	println!("catalog bytes={bytes}");

	// Keep the broadcast up until every reader has checked the catalog.
	let mut commands = tokio::io::BufReader::new(tokio::io::stdin()).lines();
	ensure!(
		commands.next_line().await?.as_deref() == Some("clean end"),
		"readers did not acknowledge the catalog"
	);
	catalog.finish()?;
	broadcast.close();
	session.close().await?;
	eprintln!("catalog acknowledged");
	Ok(())
}
