//! Text chat over a MoQ track, ported from iroh-live's `iroh-rooms`.
//!
//! A broadcast carries chat on one well-known track, [`TRACK`]. Each message is
//! a single group holding one frame of UTF-8 text. The sender's identity comes
//! from the broadcast that carries the track, not from the payload.
//!
//! This is not hang.live's `hang/chat.json` snapshot; that stays an app-defined
//! catalog extension. This track is an ordered append-log.

use std::{
	task::{Poll, ready},
	time::Instant,
};

use moq_net::{Error, Timestamp, broadcast, track};

/// Name of the track that carries chat messages.
pub const TRACK: &str = "chat";

/// Publisher tie-break priority for the chat track.
///
/// Lower than audio and video, which are the tracks a viewer notices first when
/// the link is congested.
pub const PRIORITY: u8 = 10;

/// Returns the track settings for the chat track.
///
/// Groups are ordered, because chat only makes sense read oldest first, whereas
/// the moq-net default favours the newest group.
pub fn info() -> track::Info {
	track::Info::default().with_priority(PRIORITY).with_ordered(true)
}

/// A received chat message, with the time it arrived.
#[derive(Debug, Clone)]
pub struct Message {
	/// The message text.
	pub text: String,
	/// When this message was received locally.
	pub received_at: Instant,
}

/// Writer half of a chat track.
pub struct Publisher {
	track: track::Producer,
}

impl std::fmt::Debug for Publisher {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Publisher").finish_non_exhaustive()
	}
}

impl Publisher {
	/// Creates the chat track on `broadcast` and returns a publisher for it.
	pub fn create(broadcast: &mut broadcast::Producer) -> Result<Self, Error> {
		Ok(Self::new(broadcast.create_track(TRACK, info())?))
	}

	/// Creates a publisher over an existing track producer.
	pub fn new(track: track::Producer) -> Self {
		Self { track }
	}

	/// Sends a text message on the chat track.
	///
	/// Empty messages are dropped rather than written, because a subscriber
	/// cannot tell them apart from a group it failed to read.
	pub fn send(&mut self, text: impl AsRef<str>) -> Result<(), Error> {
		let text = text.as_ref();
		if text.is_empty() {
			return Ok(());
		}
		self.track.write_frame(Timestamp::now(), text)
	}

	/// Ends the chat track so subscribers drain already-sent messages, then see close.
	///
	/// Dropping a [`Publisher`] without this first is an abrupt teardown: the
	/// underlying track discards its cache, and a subscriber that has not yet
	/// read the last message loses it.
	pub fn finish(mut self) -> Result<(), Error> {
		self.track.finish()
	}
}

/// Reader half of a chat track.
pub struct Subscriber {
	track: track::Subscriber,
}

impl std::fmt::Debug for Subscriber {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Subscriber").finish_non_exhaustive()
	}
}

impl Subscriber {
	/// Subscribes to the chat track of `broadcast`.
	///
	/// Fails if the broadcast has no track named [`TRACK`], which is the normal
	/// outcome for a peer that publishes media without chat.
	pub async fn subscribe(broadcast: &broadcast::Consumer) -> Result<Self, Error> {
		let track = broadcast.track(TRACK)?.subscribe(None).await?;
		Ok(Self::new(track))
	}

	/// Creates a subscriber over an existing track subscriber.
	pub fn new(track: track::Subscriber) -> Self {
		Self { track }
	}

	/// Poll for the next chat message.
	///
	/// Returns `None` once the track ends.
	pub fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Option<Message>> {
		loop {
			let frame = match ready!(self.track.poll_read_frame(waiter)) {
				Ok(Some(frame)) => frame,
				Ok(None) => return Poll::Ready(None),
				Err(err) => {
					tracing::warn!("chat track read failed: {err:#}");
					return Poll::Ready(None);
				}
			};
			match std::str::from_utf8(&frame.payload) {
				Ok(text) if !text.is_empty() => {
					return Poll::Ready(Some(Message {
						text: text.to_owned(),
						received_at: Instant::now(),
					}));
				}
				Ok(_) => continue,
				Err(err) => {
					tracing::warn!("chat message is not valid UTF-8: {err}");
					continue;
				}
			}
		}
	}

	/// Wait for the next chat message.
	///
	/// Returns `None` once the track ends.
	pub async fn recv(&mut self) -> Option<Message> {
		kio::wait(|waiter| self.poll_recv(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	async fn pair() -> (broadcast::Producer, Publisher, Subscriber) {
		let mut producer = broadcast::Info::new().produce();
		let publisher = Publisher::create(&mut producer).expect("create chat track");
		let subscriber = Subscriber::subscribe(&producer.consume())
			.await
			.expect("subscribe to chat track");
		(producer, publisher, subscriber)
	}

	#[tokio::test]
	async fn roundtrip() {
		let (_producer, mut publisher, mut subscriber) = pair().await;

		publisher.send("hello").unwrap();
		publisher.send("world").unwrap();

		assert_eq!(subscriber.recv().await.unwrap().text, "hello");
		assert_eq!(subscriber.recv().await.unwrap().text, "world");
	}

	#[tokio::test]
	async fn empty_messages_skipped() {
		let (_producer, mut publisher, mut subscriber) = pair().await;

		publisher.send("").unwrap();
		publisher.send("after empty").unwrap();

		assert_eq!(subscriber.recv().await.unwrap().text, "after empty");
	}

	#[tokio::test]
	async fn closed_track_returns_none() {
		let (_producer, mut publisher, mut subscriber) = pair().await;

		publisher.send("last").unwrap();
		publisher.finish().unwrap();

		assert_eq!(subscriber.recv().await.unwrap().text, "last");
		assert!(subscriber.recv().await.is_none());
	}
}
