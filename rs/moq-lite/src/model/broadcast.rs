use std::{collections::HashMap, future::Future};

use crate::{Delivery, Error, Produce, Track, TrackConsumer, TrackProducer};
use tokio::sync::watch;
use web_async::Lock;

#[derive(Default)]
struct State {
	// Explicitly published tracks.
	// NOTE: These will be `unused` when there's no active subscribers, but won't be removed.
	producers: HashMap<String, TrackProducer>,

	// Tracks requested over the network.
	// NOTE: These will be removed on `unused`.
	requested: HashMap<String, TrackProducer>,
}

pub struct Broadcast {}

impl Broadcast {
	pub fn produce() -> Produce<BroadcastProducer, BroadcastConsumer> {
		let producer = BroadcastProducer::new();
		Produce {
			consumer: producer.consume(),
			producer,
		}
	}
}

/// Receive broadcast/track requests and return if we can fulfill them.
#[derive(Clone)]
pub struct BroadcastProducer {
	state: Lock<State>,

	closed: watch::Sender<Option<Result<(), Error>>>,
	requested: (
		// We need the sender so we can clone new consumers
		async_channel::Sender<TrackProducer>,
		async_channel::Receiver<TrackProducer>,
	),
}

impl Default for BroadcastProducer {
	fn default() -> Self {
		Self::new()
	}
}

impl BroadcastProducer {
	pub fn new() -> Self {
		Self {
			state: Default::default(),
			closed: Default::default(),
			requested: async_channel::unbounded(),
		}
	}

	/// Return the next requested track, or None if there are no Consumers active.
	pub async fn requested_track(&mut self) -> Option<TrackProducer> {
		tokio::select! {
			request = self.requested.1.recv() => request.ok(),
			_ = self.closed.closed() => None,
		}
	}

	/// Produce a new track and insert it into the broadcast.
	pub fn create_track<T: Into<Track>>(&mut self, track: T, delivery: Delivery) -> TrackProducer {
		let track = TrackProducer::new(track.into(), delivery);
		self.publish_track(track.clone());
		track
	}

	/// Insert a track into the broadcast.
	pub fn publish_track(&mut self, track: TrackProducer) {
		let name = track.name.to_string();

		let mut state = self.state.lock();
		state.producers.insert(name, track);
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) {
		let mut state = self.state.lock();
		state.producers.remove(name);
		state.requested.remove(name);
	}

	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			state: self.state.clone(),
			closed: self.closed.subscribe(),
			requested: self.requested.0.clone(),
		}
	}

	pub fn close(&mut self) -> Result<(), Error> {
		let mut result = Ok(());

		self.closed.send_if_modified(|closed| {
			if let Some(closed) = closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			*closed = Some(Ok(()));
			true
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		let mut result = Ok(());

		self.closed.send_if_modified(|closed| {
			if let Some(Err(err)) = closed.clone() {
				result = Err(err);
				return false;
			}

			*closed = Some(Err(err));
			true
		});

		result
	}

	/// Block until there are no more consumers.
	///
	/// A new consumer can be created by calling [Self::consume] and this will block again.
	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let closed = self.closed.clone();
		async move {
			closed.closed().await;
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.closed.same_channel(&other.closed)
	}
}

/// Subscribe to abitrary broadcast/tracks.
#[derive(Clone)]
pub struct BroadcastConsumer {
	state: Lock<State>,
	closed: watch::Receiver<Option<Result<(), Error>>>,
	requested: async_channel::Sender<TrackProducer>,
}

impl BroadcastConsumer {
	/// Starts fetches the Track over the network, using the given settings.
	///
	/// This is synchronous and cannot fail.
	/// However, the returned [TrackConsumer] will be aborted if the track can't be found.
	pub fn subscribe_track(&self, track: impl Into<Track>, delivery: Delivery) -> TrackConsumer {
		let track = track.into();

		let mut state = self.state.lock();

		// If the track is already published, return it.
		if let Some(existing) = state.producers.get(track.name.as_ref()) {
			return existing.subscribe(delivery);
		}

		if let Some(existing) = state.requested.get(track.name.as_ref()) {
			return existing.subscribe(delivery);
		}

		// Create a new track producer using this first request's delivery information.
		// The publisher SHOULD replace them with their own settigns on OK.
		let track = TrackProducer::new(track, delivery);

		let consumer = track.subscribe(delivery);

		state.requested.insert(track.name.to_string(), track.clone());

		let state = self.state.clone();
		let requested = self.requested.clone();

		web_async::spawn(async move {
			if requested.send(track.clone()).await.is_ok() {
				track.unused().await;
			}
			state.lock().requested.remove(track.name.as_ref());
		});

		consumer
	}

	pub async fn closed(&self) -> Result<(), Error> {
		match self.closed.clone().wait_for(|closed| closed.is_some()).await {
			Ok(closed) => closed.clone().unwrap(),
			Err(_) => Err(Error::Cancel),
		}
	}

	/// Check if this is the exact same instance of a broadcast.
	///
	/// Duplicate names are allowed in the case of resumption.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.closed.same_channel(&other.closed)
	}
}
