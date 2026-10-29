//! Finite media finish case in the existing interop runner.
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, ensure};
use moq_mux::catalog::Stream;
use moq_tokio::moq_net;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

/// How long the publisher keeps its finished broadcast up for the late readers.
const LINGER: Duration = Duration::from_secs(10);

struct Args {
	role: String,
	url: url::Url,
	broadcast: String,
	version: Option<String>,
	timeout: Duration,
	frames: usize,
	finish: bool,
	finish_ack: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let mut input = std::env::args().skip(1);
	let role = input.next().context("missing role")?;
	let mut options = std::collections::HashMap::new();
	let mut finish = false;
	while let Some(flag) = input.next() {
		if flag == "--finish" {
			finish = true;
			continue;
		}
		ensure!(
			[
				"--url",
				"--broadcast",
				"--version",
				"--timeout",
				"--frames",
				"--finish-ack"
			]
			.contains(&flag.as_str()),
			"unknown flag: {flag}"
		);
		options.insert(flag, input.next().context("missing flag value")?);
	}
	let args = Args {
		role,
		finish,
		url: options.remove("--url").context("missing --url")?.parse()?,
		broadcast: options.remove("--broadcast").context("missing --broadcast")?,
		version: options.remove("--version"),
		timeout: Duration::try_from_secs_f64(options.remove("--timeout").unwrap_or_else(|| "60".into()).parse()?)?,
		frames: options.remove("--frames").unwrap_or_else(|| "10".into()).parse()?,
		finish_ack: options.remove("--finish-ack").map(PathBuf::from),
	};
	ensure!(
		!args.timeout.is_zero() && args.frames > 0,
		"timeout and frames must be positive"
	);
	let mut config = moq_tokio::connect::Config::default();
	if let Some(version) = &args.version {
		config.version.push(version.parse().map_err(anyhow::Error::msg)?);
	}
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let session = client
		.clone()
		.with_publisher(&origin)
		.with_subscriber(origin.clone())
		.connect(args.url)
		.established()
		.await?;
	let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
	if args.role == "publish" {
		let broadcast = origin.create_broadcast(&args.broadcast)?;
		let mut catalog =
			moq_mux::catalog::Producer::<()>::new(&mut broadcast.broadcast().clone(), Default::default())?;
		let mut media = moq_mux::import::TrackStream::video(
			broadcast.reserve_track("video")?,
			catalog.reserve(),
			moq_mux::import::VideoInit::new(moq_mux::import::VideoFormat::Avc3, Vec::new()),
		)?;
		let mut data = Vec::new();
		stdin.read_to_end(&mut data).await?;
		media.decode(&data)?;
		media.finish()?;
		catalog.finish()?;
		broadcast.announce(Default::default())?;
		// Keep the finished broadcast up for the late readers, then take it offline.
		let linger = moq_mux::Linger::new([broadcast.broadcast().clone()], (media, catalog), LINGER);
		println!("finish ready linger_us={}", LINGER.as_micros());
		linger.wait().await;
		println!("finish ended");
		let health = origin.create_broadcast(format!("{}.health", args.broadcast))?;
		let mut track = health.create_track("live", moq_net::track::Info::default())?;
		track.write_frame(None, b"session-alive".to_vec())?;
		track.finish()?;
		health.announce(Default::default())?;
		let ack = args.finish_ack.context("publisher needs --finish-ack")?;
		let mut complete = String::new();
		tokio::io::BufReader::new(tokio::fs::File::open(ack).await?)
			.read_line(&mut complete)
			.await?;
		ensure!(complete == "complete\n", "finish acknowledgement was not complete");
		health.close();
		println!("finish acknowledged");
	} else {
		ensure!(args.role == "subscribe" && args.finish, "expected finish subscriber");
		println!("finish reader ready");
		let mut command = String::new();
		stdin.read_line(&mut command).await?;
		ensure!(command == "read\n", "finish read phase was not released");
		kio::clock::timeout(args.timeout, async {
			let mut routes = consumer.announced();
			let broadcast = consumer.follow(&args.broadcast, None)?.available().await?;
			let mut catalogs =
				moq_mux::catalog::Consumer::<()>::new(&broadcast, moq_mux::catalog::CatalogFormat::Hang).await?;
			let catalog = loop {
				let catalog = catalogs.next().await?.context("finished catalog had no video")?;
				if !catalog.video.renditions.is_empty() {
					break catalog;
				}
			};
			let (name, config) = catalog.video.renditions.iter().next().context("no video")?;
			let track = broadcast
				.track(name)?
				.subscribe(Some(
					moq_net::track::Subscription::default()
						.with_max_delay(args.timeout)
						.with_range(0..),
				))
				.await?;
			let format = moq_mux::catalog::hang::Container::try_from(config)?;
			let mut media = moq_mux::container::Consumer::new(track, format);
			let mut count = 0;
			while let Some(item) = media.read().await? {
				match item {
					moq_mux::container::Read::Frame { frame, .. } => {
						if !frame.payload.is_empty() {
							count += 1;
						}
					}
					moq_mux::container::Read::Gap { .. } => anyhow::bail!("gap in finished tail"),
					moq_mux::container::Read::End { .. } => {}
				}
			}
			ensure!(count == args.frames, "wrong finished tail: {count} frames");
			while catalogs.next().await?.is_some() {}
			loop {
				let event = routes
					.next()
					.await
					.context("origin ended before broadcast withdrawal")?;
				if let moq_net::announce::Event::End(announce) = event
					&& announce.prefix.as_str() == args.broadcast
				{
					break;
				}
			}
			let health = consumer
				.follow(format!("{}.health", args.broadcast), None)?
				.available()
				.await?;
			let mut track = health.track("live")?.subscribe(None).await?;
			let mut group = track.recv_group().await?.context("no health group")?;
			let frame = group.read_frame().await?.context("no health frame")?;
			ensure!(
				frame.payload.as_ref() == b"session-alive" && frame.timestamp.is_none(),
				"health payload or timestamp"
			);
			ensure!(
				group.read_frame().await?.is_none() && track.recv_group().await?.is_none(),
				"health EOF"
			);
			println!("finish clean frames={count} catalog=eof announce=end health=ok");
			Ok::<_, anyhow::Error>(())
		})
		.await??;
	}
	session.close().await?;
	client.close().await;
	Ok(())
}
