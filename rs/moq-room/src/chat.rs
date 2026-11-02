//! Chat over an uncompressed JSON window on `chat`, retaining ten seconds of messages.
//! Sender identity comes from the broadcast, not the payload.

use crate::Error;
use kio::clock::Instant;
use moq_net::{broadcast, track};
use std::{
	collections::VecDeque,
	pin::Pin,
	task::{Poll, ready},
	time::Duration,
};

/// Name of the track carrying the chat window.
pub const TRACK: &str = "chat";
/// Delivery priority, below audio and video.
pub const PRIORITY: i8 = 10;
/// How long a published message stays in the window.
pub const HISTORY: Duration = Duration::from_secs(10);
/// A message entering, leaving, or missed from the window.
pub type Event = moq_json::window::Event<String>;

/// Poll-driven wall-clock deadline, the same shape kio::time used before that
/// module left kio. Built on kio's clock so tokio::time::pause advances it in tests.
struct Deadline {
	at: Option<Instant>,
	sleep: Option<Pin<Box<kio::clock::Sleep>>>,
}

impl Deadline {
	fn new() -> Self {
		Self { at: None, sleep: None }
	}

	fn set(&mut self, at: Option<Instant>) {
		if self.at == at {
			return;
		}
		self.at = at;
		if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
			sleep.as_mut().reset(at);
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.at else { return Poll::Pending };
		let sleep = self.sleep.get_or_insert_with(|| Box::pin(kio::clock::sleep_until(at)));
		if at <= kio::clock::now() {
			return Poll::Ready(());
		}
		waiter.poll_future(sleep.as_mut())
	}
}

/// Track settings for the latest chat window.
pub fn info() -> track::Info {
	track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_priority(PRIORITY)
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
				moq_json::window::ProducerConfig::default().with_ops(false),
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
		self.producer
			.push(moq_net::Timed::from(&text.to_owned()).at(moq_net::Timestamp::now()))?;
		self.expires.push_back(kio::clock::now() + HISTORY);
		Ok(())
	}

	fn prune(&mut self) -> Result<(), Error> {
		let now = kio::clock::now();
		let count = self.expires.iter().take_while(|expires| **expires <= now).count();
		if count == 0 {
			return Ok(());
		}
		self.producer
			.pop(moq_net::Timed::from(count as u64).at(moq_net::Timestamp::now()))?;
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
	pub fn finish(mut self) -> Result<(), Error> {
		Ok(self.producer.finish()?)
	}
}

/// Reads changes to a participant's retained chat window.
pub struct Subscriber {
	state: State,
}

enum State {
	/// Waiting for the newest window, so reading starts there rather than replaying older ones.
	/// Taken only while moving to `Reading`.
	Positioning(Option<Box<track::Subscriber>>),
	Reading(Box<moq_json::window::Consumer<String>>),
}

impl Subscriber {
	/// Subscribe to the newest retained window on a broadcast.
	pub async fn subscribe(broadcast: &broadcast::Consumer) -> Result<Self, Error> {
		Ok(Self::new(broadcast.track(TRACK)?.subscribe(None).await?))
	}

	/// Read window changes from an existing subscription, starting at the newest window.
	pub fn new(track: track::Subscriber) -> Self {
		Self {
			state: State::Positioning(Some(Box::new(track))),
		}
	}

	/// Poll the next window change and when its frame was sent, returning errors separately from
	/// clean completion.
	pub fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::Timed<Event>>, Error>> {
		if let State::Positioning(pending) = &mut self.state {
			let track = pending.as_mut().expect("positioning holds the track");
			// The newest window is known only once the feed is live: a relay copy still waiting
			// on its source reports nothing yet, so wait rather than replay its backlog. The
			// wait ends with the track.
			let largest = ready!(track.poll_largest(waiter));
			let mut track = *pending.take().expect("positioning holds the track");
			if let Some(largest) = largest {
				track = track.with_range(largest.group..);
			}
			self.state = State::Reading(Box::new(moq_json::window::Consumer::new(track, Default::default())));
		}
		let State::Reading(consumer) = &mut self.state else {
			unreachable!("positioned above");
		};
		Poll::Ready(Ok(ready!(consumer.poll_next(waiter))?))
	}

	/// Wait for the next window change and when its frame was sent, or None on clean completion.
	///
	/// A message first seen in a later window's header carries that header's time, not its send time.
	pub async fn recv(&mut self) -> Result<Option<moq_net::Timed<Event>>, Error> {
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
		let first = subscriber.recv().await.unwrap().unwrap();
		assert!(first.at.is_some(), "a sent message carries its send time");
		assert_eq!(
			first.value,
			Event::Push {
				index: 0,
				value: "first".into()
			}
		);
		publisher.expire().await.unwrap();
		assert_eq!(
			subscriber.recv().await.unwrap().map(|e| e.value),
			Some(Event::Pop(0..1))
		);
		publisher.send("second").unwrap();
		let mut late = Subscriber::subscribe(&broadcast.consume()).await.unwrap();
		assert_eq!(
			late.recv().await.unwrap().map(|e| e.value),
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
		let broadcast = broadcast::Info::new().produce();
		let mut track = broadcast.create_track(TRACK, info()).unwrap();
		let mut subscriber = Subscriber::subscribe(&broadcast.consume()).await.unwrap();
		track
			.write_frame(moq_net::Timestamp::now(), r#"{"offset":0,"records":[42]}"#)
			.unwrap();
		assert!(matches!(subscriber.recv().await, Err(Error::Json(_))));
	}

	/// An origin at `hop`, its driver spawned.
	fn origin(hop: u64) -> moq_net::origin::Producer {
		let config = moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap());
		let (producer, driver) = moq_net::origin::Producer::new(config);
		tokio::spawn(moq_net::time::run(driver));
		producer
	}

	/// A lite-07 session over memory carrying `from`'s broadcasts into `to`, open while held.
	async fn link(
		from: &moq_net::origin::Producer,
		to: &moq_net::origin::Producer,
	) -> (moq_net::Session, moq_net::Session) {
		use moq_net::transport::memory;

		let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
		let mut config = memory::Config::default();
		config.protocol = Some(version.alpn().to_string());
		let (client_transport, server_transport) = memory::pair(config);
		let client = moq_net::Client::new()
			.with_versions(version.into())
			.with_publisher(from.consume());
		let server = moq_net::Server::new()
			.with_versions(version.into())
			.with_subscriber(to.clone());
		tokio::join!(
			async {
				let (session, driver) = client
					.connect(moq_net::time::Instant::now(), client_transport)
					.await
					.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			},
			async {
				let (session, driver) = server
					.accept(moq_net::time::Instant::now(), server_transport)
					.await
					.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			}
		)
	}

	/// A relay copy whose upstream subscription went idle keeps the windows it cached. A new
	/// reader waits for the copy to go live and starts at its newest window, rather than
	/// replaying a message that has since left it.
	#[tokio::test(start_paused = true)]
	async fn a_warm_copy_starts_at_the_newest_window() {
		let publisher = origin(1);
		let broadcast = publisher.create_broadcast("room").unwrap();
		let track = broadcast.create_track(TRACK, info()).unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut demand = track.demand();
		let mut chat = moq_json::window::Producer::<String>::new(
			track,
			moq_json::window::ProducerConfig::default().with_ops(false),
		);
		let at = |millis| moq_net::Timestamp::from_millis(millis).unwrap();
		chat.push(moq_net::Timed::from(&"first".to_owned()).at(at(0))).unwrap();

		let relay = origin(2);
		let _link = link(&publisher, &relay).await;
		let consumer = relay.consume();
		let remote = consumer.follow("room", None).unwrap().available().await.unwrap();

		// A first reader warms the relay's copy with the first window.
		{
			let mut warm = Subscriber::subscribe(&remote).await.unwrap();
			assert_eq!(
				warm.recv().await.unwrap().map(|e| (e.value, e.at)),
				Some((
					Event::Push {
						index: 0,
						value: "first".into()
					},
					Some(at(0))
				))
			);
		}
		// The relay cancels the unread upstream subscription; its copy keeps the window.
		while demand.wanted_changed().await.unwrap().is_some() {}
		chat.pop(moq_net::Timed::from(1).at(at(1))).unwrap();
		chat.push(moq_net::Timed::from(&"second".to_owned()).at(at(1))).unwrap();

		// A delay budget long enough that the backlog is still worth delivering.
		let subscription = track::Subscription::default().with_max_delay(Duration::from_secs(10));
		let mut late = Subscriber::new(remote.track(TRACK).unwrap().subscribe(subscription).await.unwrap());
		assert_eq!(
			late.recv().await.unwrap().map(|e| (e.value, e.at)),
			Some((
				Event::Push {
					index: 1,
					value: "second".into()
				},
				Some(at(1))
			)),
			"the warm copy's backlog was replayed"
		);
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
