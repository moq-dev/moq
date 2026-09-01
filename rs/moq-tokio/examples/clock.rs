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
//! cargo run -p moq-tokio --example clock -- --connect https://relay.example.com/anon --broadcast clock publish
//! cargo run -p moq-tokio --example clock -- --connect https://relay.example.com/anon --broadcast clock subscribe
//! ```

use anyhow::Context;
use chrono::prelude::*;
use moq_net::*;

#[derive(usage::Cli, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "clock")]
struct Config {
	/// The name of the broadcast to publish or subscribe to.
	#[usage(long)]
	broadcast: String,

	/// The MoQ client configuration.
	#[usage(flatten)]
	client: moq_tokio::connect::Config,

	/// The name of the clock track.
	#[usage(long, default = "seconds")]
	track: String,

	/// The log configuration.
	#[usage(flatten)]
	log: moq_tokio::Log,

	/// Whether to publish the clock or consume it.
	#[usage(subcommand)]
	role: Command,
}

#[derive(usage::Subcommands, Clone)]
enum Command {
	Publish,
	Subscribe,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let config = Config::parse();
	config.log.init()?;

	let url = config.client.url.clone().context("--connect is required")?;
	let client = config.client.init(Default::default())?;

	tracing::info!(%url, "connecting to server");

	let track = config.track;

	let origin = moq_tokio::origin::spawn(moq_net::Hop::random());

	match config.role {
		Command::Publish => {
			let mut broadcast = origin
				.create_broadcast(&config.broadcast)
				.context("failed to create broadcast")?;
			let track = broadcast.create_track(track, None)?;
			let _announce_broadcast = origin
				.announce(&config.broadcast, Default::default())
				.context("failed to announce broadcast")?;
			let clock = Publisher::new(track);

			let reconnect = client.with_publisher(&origin).connect(url);

			// Keep the result out of the `select!` arm (a `?` there would return
			// before the close below runs), so the broadcast is always closed.
			let result = tokio::select! {
				res = reconnect.closed() => res.map_err(Into::into),
				_ = clock.run() => Ok(()),
			};

			// Cleanly close the broadcast on exit so subscribers see a normal end
			// rather than Error::Dropped.
			broadcast.finish();
			result
		}
		Command::Subscribe => {
			let reconnect = client.with_subscriber(origin.clone()).connect(url);

			// IETF MoQ + the current origin::Consumer API don't let us call
			// `session.consume_broadcast(&path)` directly, so loop on announces
			// instead. This also makes the subscriber reconnect-aware.
			tracing::info!(broadcast = %config.broadcast, "waiting for broadcast to be online");

			let path: moq_net::Path<'_> = config.broadcast.into();
			let consumer = origin
				.scope(&[path])
				.context("not allowed to consume broadcast")?
				.consume();
			let mut announced = consumer.announced();

			let mut clock: Option<Subscriber> = None;

			loop {
				tokio::select! {
					Some(update) = announced.next() => match update.active {
						true => {
							let path = update.prefix.as_path().to_owned();
							tracing::info!(broadcast = %path, "broadcast is online, subscribing to track");
							let broadcast = consumer.request_broadcast(&path).await?;
							let track = broadcast
								.track(&track)?.subscribe(None).await?;
							clock = Some(Subscriber::new(track));
						}
						false => {
							tracing::warn!(broadcast = %update.prefix, "broadcast is offline, waiting...");
						}
					},
					res = reconnect.closed() => return Ok(res?),
					// Drops the previous subscriber on each new announce.
					Some(res) = async { Some(clock.take()?.run().await) } => res.context("clock error")?,
				}
			}
		}
	}
}

struct Publisher {
	track: track::Producer,
}

impl Publisher {
	fn new(track: track::Producer) -> Self {
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

	async fn send_segment(mut segment: group::Producer, mut now: DateTime<Utc>) -> anyhow::Result<()> {
		// Everything but the second.
		let base = now.format("%Y-%m-%d %H:%M:").to_string();

		segment.write_frame(moq_tokio::moq_net::Timestamp::now(), base.clone())?;

		loop {
			let delta = now.format("%S").to_string();
			segment.write_frame(moq_tokio::moq_net::Timestamp::now(), delta.clone())?;

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
	track: track::Subscriber,
}

impl Subscriber {
	fn new(track: track::Subscriber) -> Self {
		Self { track }
	}

	async fn run(mut self) -> anyhow::Result<()> {
		while let Some(mut group) = self.track.recv_group().await? {
			let base = group
				.read_frame()
				.await
				.context("failed to get first object")?
				.context("empty group")?;

			let base = String::from_utf8_lossy(&base.payload);

			while let Some(object) = group.read_frame().await? {
				let str = String::from_utf8_lossy(&object.payload);
				println!("{base}{str}");
			}
		}

		Ok(())
	}
}
