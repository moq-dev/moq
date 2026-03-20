//! Example MoQ application that publishes or subscribes to a clock track.
//!
//! Demonstrates basic [`moq_lite`] usage by streaming time updates every second.
//! Useful for testing relay connectivity and latency.

use url::Url;

use anyhow::Context;
use clap::Parser;

mod clock;
use moq_lite::*;

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

	let track = Track {
		name: config.track,
		priority: 0,
	};

	let origin = moq_lite::Origin::produce();

	match config.role {
		Command::Publish => {
			let mut broadcast = moq_lite::Broadcast::produce();
			let track = broadcast.create_track(track)?;
			let clock = clock::Publisher::new(track);

			origin.publish_broadcast(&config.broadcast, broadcast.consume());

			let session = client.with_publish(origin.consume()).connect(config.url).await?;

			tokio::select! {
				res = session.closed() => res.context("session closed"),
				_ = clock.run() => Ok(()),
			}
		}
		Command::Subscribe => {
			let session = client.with_consume(origin.clone()).connect(config.url).await?;

			// We use announced() to watch for the broadcast to come online/reconnect,
			// rather than a one-shot consume_broadcast() call.

			tracing::info!(broadcast = %config.broadcast, "waiting for broadcast to be online");

			let path: moq_lite::Path<'_> = config.broadcast.into();
			let mut origin = origin
				.consume()
				.with_filter(&[path])
				.context("not allowed to consume broadcast")?;

			// The current subscriber if any, dropped after each announce.
			let mut clock: Option<clock::Subscriber> = None;

			loop {
				tokio::select! {
					res = origin.announced() => {
						let (path, broadcast) = res.context("origin closed")?;
						tracing::info!(broadcast = %path, "broadcast is online, subscribing to track");
						let subscribed_track = broadcast.subscribe_track(&track)?;
						clock = Some(clock::Subscriber::new(subscribed_track));
					},
					res = session.closed() => return res.context("session closed"),
					// NOTE: This drops clock when a new announce arrives, canceling it.
					Some(res) = async { Some(clock.take()?.run().await) } => res.context("clock error")?,
				}
			}
		}
	}
}
