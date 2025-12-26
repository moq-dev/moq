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

	let session = client.connect(config.url).await?;

	match config.role {
		Command::Publish => {
			let mut broadcast = moq_lite::BroadcastProducer::new();
			let track = broadcast.create_track(config.track);
			let clock = clock::Publisher::new(track);

			let origin = moq_lite::OriginProducer::new();
			origin.publish_broadcast(&config.broadcast, broadcast.consume());

			let session = moq_lite::Session::connect(session, origin.consume(), None).await?;

			tokio::select! {
				res = session.closed() => res.map_err(Into::into),
				_ = clock.run() => Ok(()),
			}
		}
		Command::Subscribe => {
			let origin = moq_lite::OriginProducer::new();

			let path: moq_lite::Path<'_> = config.broadcast.into();
			let mut consumer = origin
				.consume()
				.consume_only(&[path.clone()])
				.context("not allowed to consume broadcast")?;

			let session = moq_lite::Session::connect(session, None, Some(origin)).await?;

			// NOTE: We could just call `session.consume_broadcast(&config.broadcast)` instead,
			// However that won't work with IETF MoQ and the current OriginConsumer API the moment.
			// So instead we do the cooler thing and loop while the broadcast is announced.

			tracing::info!(broadcast = %path, "waiting for broadcast to be online");

			// The current subscriber if any, dropped after each announce.
			let mut clock: Option<clock::Subscriber> = None;

			loop {
				tokio::select! {
					Some(announce) = consumer.announced() => match announce {
						(path, Some(broadcast)) => {
							tracing::info!(broadcast = %path, "broadcast is online, subscribing to track");
							let track = broadcast.subscribe_track(&config.track);
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
