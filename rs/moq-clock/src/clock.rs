use anyhow::Context;

use chrono::prelude::*;
use moq_lite::*;

pub struct Publisher {
	track: TrackProducer,
}

impl Publisher {
	pub fn new(track: TrackProducer) -> Self {
		Self { track }
	}

	pub async fn run(mut self) -> anyhow::Result<()> {
		let start = Utc::now();
		let mut now = start;

		// Just for fun, don't start at zero.
		let mut sequence = start.minute();

		loop {
			let segment = self.track.create_group(sequence).unwrap();

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

		// The moq-lite layer needs a timestamp, so might as well use the current Unix time.
		// It's kinda silly that we're also encoding a human-readable string but this is a toy example.
		let timestamp = Time::from_micros(now.timestamp_micros() as u64)?;
		segment.write_frame(base, timestamp)?;

		loop {
			let delta = now.format("%S").to_string();
			let timestamp = Time::from_micros(now.timestamp_micros() as u64)?;
			segment.write_frame(delta, timestamp)?;

			let next = now + chrono::Duration::try_seconds(1).unwrap();
			let next = next.with_nanosecond(0).unwrap();

			let delay = (next - now).to_std().unwrap();
			tokio::time::sleep(delay).await;

			// Get the current time again to check if we overslept
			let next = Utc::now();
			if next.minute() != now.minute() {
				break;
			}

			now = next;
		}

		segment.close()?;

		Ok(())
	}
}
pub struct Subscriber {
	track: TrackConsumer,
}

impl Subscriber {
	pub fn new(track: TrackConsumer) -> Self {
		Self { track }
	}

	pub async fn run(mut self) -> anyhow::Result<()> {
		while let Some(mut group) = self.track.next_group().await? {
			let base = group
				.read_frame()
				.await
				.context("failed to get first object")?
				.context("empty group")?;

			let base = String::from_utf8_lossy(&base);

			while let Ok(Some(object)) = group.read_frame().await {
				let str = String::from_utf8_lossy(&object);
				println!("{base}{str}");
			}
		}

		Ok(())
	}
}
