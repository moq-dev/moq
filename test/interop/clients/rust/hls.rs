//! HLS record interop: `publish` writes a fixed frame schedule through moq-mux, whose catalog
//! publishes HLS records by default, and `read` prints a broadcast's declared durations and
//! records. The harness reads this publisher and the native JS one (`js-native/hls.ts`), which
//! publishes the same schedule through `@moq/hang`, and requires the two readouts identical.
//!
//!     interop-hls publish URL BROADCAST
//!     interop-hls read URL BROADCAST
//!
//! Keep the schedule in step with `test/interop/clients/js-native/hls.ts`.
use std::time::Duration;

use anyhow::{Context, ensure};
use moq_mux::catalog::Stream;
use moq_mux::catalog::hang::Container;
use moq_mux::container::{Frame, Kind};
use moq_tokio::moq_net;
use tokio::io::AsyncBufReadExt;

/// One frame: its pts in milliseconds and whether it opens a group.
type Schedule = Vec<Vec<(u64, bool)>>;

/// Frames every `step` ms over `start..end`, opening a group every `gop` ms.
fn frames(start: u64, end: u64, step: u64, gop: u64) -> Vec<(u64, bool)> {
	(start..end)
		.step_by(step as usize)
		.map(|ms| (ms, (ms - start) % gop == 0))
		.collect()
}

/// Video: 2s GOPs, a pause, then 3s GOPs off the grid. Audio: 100ms groups of 20ms frames, a
/// pause, then more. Each part ends in a discontinuity but the last, which the finish ends.
fn video() -> Schedule {
	vec![frames(0, 6000, 100, 2000), frames(20000, 30000, 100, 3000)]
}

fn audio() -> Schedule {
	vec![frames(0, 9000, 20, 100), frames(15000, 17000, 20, 100)]
}

fn write<R: Clone + Send + 'static>(
	producer: &mut moq_mux::container::Producer<Container, R>,
	schedule: Schedule,
) -> anyhow::Result<()> {
	for (index, part) in schedule.into_iter().enumerate() {
		if index > 0 {
			producer.discontinuity()?;
		}
		for (ms, keyframe) in part {
			producer.write(Frame {
				timestamp: moq_net::Timestamp::from_millis(ms)?,
				payload: bytes::Bytes::from_static(&[1]),
				keyframe,
				duration: None,
			})?;
		}
	}
	producer.finish()?;
	Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(
		args.len() == 4 && ["publish", "read"].contains(&args[1].as_str()),
		"usage: interop-hls publish|read URL BROADCAST"
	);
	let url: url::Url = args[2].parse()?;
	let path = &args[3];
	let mut config = moq_tokio::connect::Config::default();
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();

	if args[1] == "publish" {
		let publication = origin.create_broadcast(path)?;
		let mut broadcast = publication.broadcast().clone();
		let mut catalog = moq_mux::catalog::Producer::<()>::new(&mut broadcast, Default::default())?;
		let track = broadcast.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))?;
		let config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		// Each producer owns its catalog entry, so both stay alive until the reader is done.
		let mut video_track = catalog.video(track, Container::Legacy(Kind::Video), config)?;
		write(&mut video_track, video())?;
		let track = broadcast.create_track("audio", catalog.track_info(hang::catalog::PRIORITY.audio))?;
		let config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		let mut audio_track = catalog.audio(track, Container::Legacy(Kind::Audio), config)?;
		write(&mut audio_track, audio())?;
		catalog.finish()?;

		let session = client.clone().with_publisher(&origin).connect(url).established().await?;
		publication.announce(Default::default())?;
		println!("hls ready");
		let mut commands = tokio::io::BufReader::new(tokio::io::stdin()).lines();
		ensure!(
			commands.next_line().await?.as_deref() == Some("clean end"),
			"reader did not acknowledge"
		);
		println!("hls acknowledged");
		drop((video_track, audio_track, catalog));
		session.close().await?;
		client.close().await;
		return Ok(());
	}

	let session = client
		.clone()
		.with_subscriber(origin.clone())
		.connect(url)
		.established()
		.await?;
	moq_clock::timeout(Duration::from_secs(30), async {
		let broadcast = origin.consume().follow(path, None)?.available().await?;
		let mut catalogs =
			moq_mux::catalog::Consumer::<()>::new(&broadcast, moq_mux::catalog::CatalogFormat::Hang).await?;
		// Both renditions, each advertising its records.
		let renditions = loop {
			let catalog = catalogs.next().await?.context("catalog ended before advertising records")?;
			let video = catalog.video.renditions.iter().map(|(name, config)| ("video", name, &config.hls));
			let audio = catalog.audio.renditions.iter().map(|(name, config)| ("audio", name, &config.hls));
			let renditions: Vec<_> = video
				.chain(audio)
				.filter_map(|(kind, name, hls)| Some((kind, name.clone(), hls.clone()?)))
				.collect();
			if renditions.len() == 2 {
				break renditions;
			}
		};
		for (kind, name, hls) in renditions {
			let mut records = moq_mux::hls::Consumer::subscribe(&broadcast, &hls).await?;
			let mut out = Vec::new();
			while let Some(event) = records.next().await? {
				if let moq_mux::hls::Event::Push { entry, .. } = event {
					let mut record = format!("{}:{}:{}", entry.sequence, entry.group, entry.pts.as_millis());
					if let Some(duration) = entry.duration {
						record += &format!(":{}", duration.as_millis());
					}
					out.push(record);
				}
			}
			ensure!(!out.is_empty(), "{kind} {name} published no records");
			println!(
				"hls {kind} {name} track={} duration={} records={}",
				hls.track,
				hls.duration.as_millis(),
				out.join(",")
			);
		}
		Ok::<_, anyhow::Error>(())
	})
	.await??;
	session.close().await?;
	client.close().await;
	Ok(())
}
