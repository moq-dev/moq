//! Example MoQ application that publishes or subscribes to a clock track.
//!
//! Demonstrates basic [`moq_lite`] usage by streaming time updates every second.
//! Useful for testing relay connectivity and latency.

use url::Url;

use anyhow::Context;
use clap::Parser;

mod clock;

#[derive(Parser, Clone)]
pub struct Config {
	/// Connect to the given URL starting with https://
	#[arg(long)]
	pub url: Url,

	/// The name of the broadcast to publish or subscribe to.
	#[arg(long)]
	pub broadcast: String,

	/// The MoQ client configuration.
	#[command(flatten)]
	pub client: moq_native::ClientConfig,

	/// The name of the clock track.
	#[arg(long, default_value = "seconds")]
	pub track: String,

	/// The log configuration.
	#[command(flatten)]
	pub log: moq_native::Log,

	/// Whether to publish the clock or consume it.
	#[command(subcommand)]
	pub role: Command,
}

#[derive(Parser, Clone)]
pub enum Command {
	Publish,
	Subscribe,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let config = Config::parse();
	config.log.init();

	let client = config.client.init()?;

	tracing::info!(url = ?config.url, "connecting to server");

	let origin = moq_lite::Origin::produce();
	let mut broadcast = moq_lite::Broadcast::produce();

	match config.role {
		Command::Publish => {
			let track = broadcast
				.producer
				.create_track(config.track, moq_lite::Delivery::default());
			let clock = clock::Publisher::new(track);

			origin.producer.publish_broadcast(&config.broadcast, broadcast.consumer);

			let session = client
				.connect_with_fallback(config.url, Some(origin.consumer), None)
				.await?;

			tokio::select! {
				res = session.closed() => res.context("session closed"),
				_ = clock.run() => Ok(()),
			}
		}
		Command::Subscribe => {
			let session = client.connect_with_fallback(config.url, None, origin.producer).await?;

			// NOTE: We could just call `session.consume_broadcast(&config.broadcast)` instead,
			// However that won't work with IETF MoQ and the current OriginConsumer API the moment.
			// So instead we do the cooler thing and loop while the broadcast is announced.

			tracing::info!(broadcast = %config.broadcast, "waiting for broadcast to be online");

			let path: moq_lite::Path<'_> = config.broadcast.into();
			let mut origin = origin
				.consumer
				.consume_only(&[path])
				.context("not allowed to consume broadcast")?;

			// The current subscriber if any, dropped after each announce.
			let mut clock: Option<clock::Subscriber> = None;

			loop {
				tokio::select! {
					Some(announce) = origin.announced() => match announce {
						(path, Some(broadcast)) => {
							tracing::info!(broadcast = %path, "broadcast is online, subscribing to track");
							let track = broadcast.subscribe_track(config.track.clone(), moq_lite::Delivery::default());
							clock = Some(clock::Subscriber::new(track));
						}
						(path, None) => {
							tracing::warn!(broadcast = %path, "broadcast is offline, waiting...");
						}
					},
					res = session.closed() => return res.context("session closed"),
					// NOTE: This drops clock when a new announce arrives, canceling it.
					Some(res) = async { Some(clock.take()?.run().await) } => res.context("clock error")?,
				}
			}
		}
	}
}
