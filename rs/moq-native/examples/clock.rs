//! Publish or subscribe to a clock track over MoQ.
//!
//! Each minute is its own group; each second is a frame within that group. The
//! first frame of every group is the `"YYYY-MM-DD HH:MM:"` prefix so subsequent
//! `"SS"` frames stay small. Useful as a tiny reference for [`moq_net`] and for
//! sanity-checking relay connectivity and latency.
//!
//! Run with:
//!
//! ```text
//! cargo run -p moq-native --example clock -- --url https://relay.example.com/anon --broadcast clock publish
//! cargo run -p moq-native --example clock -- --url https://relay.example.com/anon --broadcast clock subscribe
//! ```

use anyhow::Context;
use chrono::prelude::*;
use clap::Parser;
use moq_net::*;
use url::Url;

#[derive(Parser, Clone)]
struct Config {
	/// Connect to the given URL starting with https://
	#[arg(long)]
	url: Url,

	/// The name of the broadcast to publish or subscribe to.
	#[arg(long)]
	broadcast: String,

	/// The MoQ client configuration.
	#[command(flatten)]
	client: moq_native::ClientConfig,

	/// The name of the clock track.
	#[arg(long, default_value = "seconds")]
	track: String,

	/// The log configuration.
	#[command(flatten)]
	log: moq_native::Log,

	/// Whether to publish the clock or consume it.
	#[command(subcommand)]
	role: Command,
}

#[derive(Parser, Clone)]
enum Command {
	Publish,
	Subscribe,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let config = Config::parse();
	config.log.init();

	let client = config.client.init()?;

	tracing::info!(url = ?config.url, "connecting to server");

	let track = Track {
		name: config.track,
		..Default::default()
	};

	let origin = moq_net::Origin::random().produce();

	match config.role {
		Command::Publish => {
			let mut broadcast = moq_net::Broadcast::new().produce();
			let track = broadcast.create_track(track)?;
			let clock = Publisher::new(track);

			origin.publish_broadcast(&config.broadcast, broadcast.consume());

			let reconnect = client.with_publish(origin.consume()).reconnect(config.url);

			tokio::select! {
				res = reconnect.closed() => res,
				_ = clock.run() => Ok(()),
			}
		}
		Command::Subscribe => {
			let reconnect = client.with_consume(origin.clone()).reconnect(config.url);

			// IETF MoQ + the current OriginConsumer API don't let us call
			// `session.consume_broadcast(&path)` directly, so loop on announces
			// instead. This also makes the subscriber reconnect-aware.
			tracing::info!(broadcast = %config.broadcast, "waiting for broadcast to be online");

			let path: moq_net::Path<'_> = config.broadcast.into();
			let mut origin = origin
				.scope(&[path])
				.context("not allowed to consume broadcast")?
				.consume()
				.announced();

			let mut clock: Option<Subscriber> = None;

			loop {
				tokio::select! {
					Some(announce) = origin.next() => match announce {
						(path, Some(broadcast)) => {
							tracing::info!(broadcast = %path, "broadcast is online, subscribing to track");
							let track = broadcast.subscribe_track(&track)?;
							clock = Some(Subscriber::new(track));
						}
						(path, None) => {
							tracing::warn!(broadcast = %path, "broadcast is offline, waiting...");
						}
					},
					res = reconnect.closed() => return res,
					// Drops the previous subscriber on each new announce.
					Some(res) = async { Some(clock.take()?.run().await) } => res.context("clock error")?,
				}
			}
		}
	}
}

struct Publisher {
	track: TrackProducer,
}

impl Publisher {
	fn new(track: TrackProducer) -> Self {
		Self { track }
	}

	async fn run(mut self) -> anyhow::Result<()> {
		let start = Utc::now();
		let mut now = start;

		// Just for fun, don't start at zero.
		let mut sequence = start.minute();

		loop {
			let segment = self.track.create_group(sequence.into()).unwrap();

			sequence += 1;

			tokio::spawn(async move {
				if let Err(err) = Self::send_segment(segment, now).await {
					tracing::warn!("failed to send minute: {:?}", err);
				}
			});

			let next = now + chrono::Duration::try_minutes(1).unwrap();
			let next = next.with_second(0).unwrap().with_nanosecond(0).unwrap();

			let delay = (next - now).to_std().unwrap();
			tokio::time::sleep(delay).await;

			now = next; // just assume we didn't undersleep
		}
	}

	async fn send_segment(mut segment: GroupProducer, mut now: DateTime<Utc>) -> anyhow::Result<()> {
		// Everything but the second.
		let base = now.format("%Y-%m-%d %H:%M:").to_string();

		segment.write_frame(base.clone())?;

		loop {
			let delta = now.format("%S").to_string();
			segment.write_frame(delta.clone())?;

			let next = now + chrono::Duration::try_seconds(1).unwrap();
			let next = next.with_nanosecond(0).unwrap();

			let delay = (next - now).to_std().unwrap();
			tokio::time::sleep(delay).await;

			// Get the current time again to check if we overslept
			let actual = Utc::now();
			if actual.minute() != now.minute() {
				break;
			}

			now = actual;
		}

		segment.finish()?;

		Ok(())
	}
}

struct Subscriber {
	track: TrackConsumer,
}

impl Subscriber {
	fn new(track: TrackConsumer) -> Self {
		Self { track }
	}

	async fn run(mut self) -> anyhow::Result<()> {
		while let Some(mut group) = self.track.recv_group().await? {
			let base = group
				.read_frame()
				.await
				.context("failed to get first object")?
				.context("empty group")?;

			let base = String::from_utf8_lossy(&base);

			while let Some(object) = group.read_frame().await? {
				let str = String::from_utf8_lossy(&object);
				println!("{base}{str}");
			}
		}

		Ok(())
	}
}
