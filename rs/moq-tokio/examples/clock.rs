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

#[derive(usage_rs::Cli, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(name = "clock")]
#[usage(settings)]
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

#[derive(usage_rs::Subcommands, Clone)]
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

	tracing::info!(url = %moq_tokio::RedactedUrl::new(&url), "connecting to server");

	let track = config.track;

	let origin = moq_tokio::origin::spawn();

	match config.role {
		Command::Publish => {
			// Created hidden, so it is announced only once the track exists and a
			// subscriber acting on the announcement finds it.
			let route = origin
				.route(&config.broadcast, moq_net::origin::RouteOptions::hidden())
				.context("failed to create broadcast")?;
			let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
			let track = route.broadcast().create_track(track, info)?;
			route.announce().context("failed to announce broadcast")?;
			let clock = Publisher::new(track);

			let reconnect = client.with_publisher(&origin).connect(url);

			// Keep the result out of the `select!` arm (a `?` there would return
			// before the close below runs), so the broadcast is always closed.
			let result = tokio::select! {
				res = reconnect.closed() => res.map_err(Into::into),
				_ = clock.run() => Ok(()),
			};

			// End the broadcast on exit.
			route.broadcast().close();
			result
		}
		Command::Subscribe => {
			let reconnect = client.with_subscriber(origin.clone()).connect(url);

			// IETF MoQ + the current origin::Consumer API don't let us call
			// `session.consume_broadcast(&path)` directly, so loop on announces
			// instead. This also makes the subscriber reconnect-aware.
			tracing::info!(broadcast = %config.broadcast, "waiting for broadcast to be online");

			let path = moq_net::Path::new(&config.broadcast)?;
			let scope =
				moq_net::Patterns::from(moq_net::Pattern::subtree(path.as_str()).context("invalid broadcast name")?);
			let consumer = origin
				.scope("", &scope)
				.context("not allowed to consume broadcast")?
				.consume();
			let mut announced = consumer.announced();

			let mut clock: Option<Subscriber> = None;

			loop {
				tokio::select! {
					Some(event) = announced.next() => match event {
						// A restart is a new broadcast: subscribe to it afresh.
						announce::Event::Present(update) => {
							tracing::info!(broadcast = %update.prefix, "broadcast is online, subscribing to track");
							let broadcast = consumer.request(&update.prefix, None).await?;
							let track = broadcast
								.track(&track)?.subscribe(None).await?;
							clock = Some(Subscriber::new(track));
						}
						announce::Event::Absent(update) => {
							tracing::warn!(broadcast = %update.prefix, "broadcast ended, waiting...");
						}
						announce::Event::Offline(update) | announce::Event::Unavailable(update) => {
							tracing::warn!(broadcast = %update.prefix, "broadcast is offline, waiting...");
						}
						announce::Event::Available(update) => {
							tracing::info!(broadcast = %update.prefix, "broadcast is reachable again");
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

/// Milliseconds since the Unix epoch: the track's timescale, on this host's clock.
fn millis(time: DateTime<Utc>) -> anyhow::Result<u64> {
	Ok(u64::try_from(time.timestamp_millis())?)
}

impl Publisher {
	fn new(track: track::Producer) -> Self {
		Self { track }
	}

	async fn run(self) -> anyhow::Result<()> {
		let mut now = Utc::now();

		loop {
			// A group starts at its first frame: the minute, in the track's milliseconds.
			let segment = self.track.append_group(millis(now)?)?;

			tokio::spawn(async move {
				if let Err(err) = Self::send_segment(segment, now).await {
					tracing::warn!("failed to send minute: {:?}", err);
				}
			});

			let next = now + chrono::Duration::try_minutes(1).unwrap();
			let next = next.with_second(0).unwrap().with_nanosecond(0).unwrap();

			let delay = (next - now).to_std().unwrap();
			kio::clock::sleep(delay).await;

			now = next; // just assume we didn't undersleep
		}
	}

	async fn send_segment(mut segment: group::Producer, mut now: DateTime<Utc>) -> anyhow::Result<()> {
		// Everything but the second.
		let base = now.format("%Y-%m-%d %H:%M:").to_string();

		segment.write_frame(moq_net::Timestamp::from_millis(millis(now)?)?, base.clone())?;

		loop {
			let delta = now.format("%S").to_string();
			segment.write_frame(moq_net::Timestamp::from_millis(millis(now)?)?, delta.clone())?;

			let next = now + chrono::Duration::try_seconds(1).unwrap();
			let next = next.with_nanosecond(0).unwrap();

			let delay = (next - now).to_std().unwrap();
			kio::clock::sleep(delay).await;

			// Get the current time again to check if we overslept
			let actual = Utc::now();
			if actual.minute() != now.minute() {
				break;
			}

			now = actual;
		}

		segment.close()?;

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
