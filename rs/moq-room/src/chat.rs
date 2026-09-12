//! Chat over an uncompressed JSON window on `chat`, retaining ten seconds of messages.
//! Sender identity comes from the broadcast, not the payload.

use crate::Error;
use kio::time::{Deadline, Instant};
use moq_net::{broadcast, track};
use std::{
	collections::VecDeque,
	task::{Poll, ready},
	time::Duration,
};

/// Name of the track carrying the chat window.
pub const TRACK: &str = "chat";
/// Delivery priority, below audio and video.
pub const PRIORITY: u8 = 10;
/// How long a published message stays in the window.
pub const HISTORY: Duration = Duration::from_secs(10);
/// A message entering, leaving, or missed from the window.
pub type Event = moq_json::window::Event<String>;

/// Track settings for the latest chat window.
pub fn info() -> track::Info {
	track::Info::default().with_priority(PRIORITY).with_ordered(false)
}

/// Publishes chat messages; drive `poll_expire` or `expire` to retire them while idle.
pub struct Publisher {
	producer: moq_json::window::Producer<String>,
	expires: VecDeque<Instant>,
	deadline: Deadline,
}

impl Publisher {
	/// Create the chat track on a broadcast.
	pub fn create(broadcast: &mut broadcast::Producer) -> Result<Self, Error> {
		Ok(Self::new(broadcast.create_track(TRACK, info())?))
	}

	/// Publish a chat window over an existing track.
	pub fn new(track: track::Producer) -> Self {
		Self {
			producer: moq_json::window::Producer::new(
				track,
				moq_json::window::ProducerConfig::default().with_op_ratio(0),
			),
			expires: VecDeque::new(),
			deadline: Deadline::new(),
		}
	}

	/// Append nonempty text, first retiring messages whose history has elapsed.
	pub fn send(&mut self, text: &str) -> Result<(), Error> {
		if text.is_empty() {
			return Ok(());
		}
		self.prune()?;
		self.producer.push(&text.to_owned())?;
		self.expires.push_back(Instant::now() + HISTORY);
		Ok(())
	}

	fn prune(&mut self) -> Result<(), Error> {
		let now = Instant::now();
		let count = self.expires.iter().take_while(|expires| **expires <= now).count();
		if count == 0 {
			return Ok(());
		}
		self.producer.pop(count as u64)?;
		self.expires.drain(..count);
		Ok(())
	}

	/// Poll until the oldest retained messages expire; drive this alongside incoming sends.
	pub fn poll_expire(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.deadline.set(self.expires.front().copied());
		ready!(self.deadline.poll(waiter));
		Poll::Ready(self.prune())
	}

	/// Wait until the oldest retained messages expire; call again while the publisher is open.
	pub async fn expire(&mut self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll_expire(waiter)).await
	}

	/// Finish the track, preserving the final retained window for current readers.
	pub fn finish(self) -> Result<(), Error> {
		Ok(self.producer.finish()?)
	}
}

/// Reads changes to a participant's retained chat window.
pub struct Subscriber {
	consumer: moq_json::window::Consumer<String>,
}

impl Subscriber {
	/// Subscribe to the newest retained window on a broadcast.
	pub async fn subscribe(broadcast: &broadcast::Consumer) -> Result<Self, Error> {
		Ok(Self::new(broadcast.track(TRACK)?.subscribe(None).await?))
	}

	/// Read window changes from an existing subscription.
	pub fn new(mut track: track::Subscriber) -> Self {
		if let Some(latest) = track.latest() {
			track.start_at(latest);
		}
		Self {
			consumer: moq_json::window::Consumer::new(track, Default::default()),
		}
	}

	/// Poll the next window change, returning errors separately from clean completion.
	pub fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>, Error>> {
		Poll::Ready(Ok(ready!(self.consumer.poll_next(waiter))?))
	}

	/// Wait for the next window change, or None on clean completion.
	pub async fn recv(&mut self) -> Result<Option<Event>, Error> {
		kio::wait(|waiter| self.poll_recv(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn expires_idle_history_and_late_readers_only_see_retained_messages() {
		tokio::time::pause();
		let mut broadcast = broadcast::Info::new().produce();
		let mut publisher = Publisher::create(&mut broadcast).unwrap();
		let mut subscriber = Subscriber::subscribe(&broadcast.consume()).await.unwrap();
		publisher.send("first").unwrap();
		assert_eq!(
			subscriber.recv().await.unwrap(),
			Some(Event::Push {
				index: 0,
				value: "first".into()
			})
		);
		publisher.expire().await.unwrap();
		assert_eq!(subscriber.recv().await.unwrap(), Some(Event::Pop(0..1)));
		publisher.send("second").unwrap();
		let mut late = Subscriber::subscribe(&broadcast.consume()).await.unwrap();
		assert_eq!(
			late.recv().await.unwrap(),
			Some(Event::Push {
				index: 1,
				value: "second".into()
			})
		);
		publisher.finish().unwrap();
		assert!(late.recv().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn rejects_malformed_records() {
		let mut broadcast = broadcast::Info::new().produce();
		let mut track = broadcast.create_track(TRACK, info()).unwrap();
		let mut subscriber = Subscriber::subscribe(&broadcast.consume()).await.unwrap();
		track
			.write_frame(moq_net::Timestamp::now(), r#"{"offset":0,"records":[42]}"#)
			.unwrap();
		assert!(matches!(subscriber.recv().await, Err(Error::Json(_))));
	}

	#[tokio::test]
	async fn propagates_track_failure() {
		let mut broadcast = broadcast::Info::new().produce();
		let publisher = Publisher::create(&mut broadcast).unwrap();
		let mut subscriber = Subscriber::subscribe(&broadcast.consume()).await.unwrap();
		drop(publisher);
		assert!(subscriber.recv().await.is_err());
	}
}
