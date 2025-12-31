//! A track is a collection of semi-reliable and semi-ordered streams, split into a [TrackProducer] and [TrackConsumer] handle.
//!
//! A [TrackProducer] creates streams with a sequence number and priority.
//! The sequest number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [Producer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [TrackConsumer] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Tranport is to not block on them.
//! streams will be cached for a potentially limited duration added to the unreliable nature.
//! A cloned [Consumer] will receive a copy of all new stream going forward (fanout).
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use tokio::sync::{oneshot, watch};
use web_async::FuturesExt;

use super::{Group, GroupConsumer, GroupProducer};
use crate::{
	Delivery, DeliveryConsumer, DeliveryProducer, Error, ExpiresConsumer, ExpiresProducer, Produce, Result, Subscriber,
	Subscribers,
};

use std::{
	borrow::Cow,
	collections::VecDeque,
	fmt,
	future::Future,
	ops::{Deref, DerefMut},
	sync::Arc,
};

/// Static information about a track
///
/// Only used to make accessing the name easy/fast.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	pub name: Arc<String>,
}

impl fmt::Display for Track {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.name)
	}
}

impl Track {
	pub fn new<T: ToString>(name: T) -> Self {
		Self {
			name: Arc::new(name.to_string()),
		}
	}

	pub fn as_str(&self) -> &str {
		&self.name
	}
}

impl From<&str> for Track {
	fn from(name: &str) -> Self {
		Self {
			name: Arc::new(name.to_string()),
		}
	}
}

impl From<String> for Track {
	fn from(name: String) -> Self {
		Self { name: Arc::new(name) }
	}
}

impl From<&String> for Track {
	fn from(name: &String) -> Self {
		Self {
			name: Arc::new(name.clone()),
		}
	}
}

impl From<&Track> for Track {
	fn from(track: &Track) -> Self {
		track.clone()
	}
}

impl From<Cow<'_, str>> for Track {
	fn from(name: Cow<'_, str>) -> Self {
		Self {
			name: Arc::new(name.into_owned()),
		}
	}
}

impl From<Arc<String>> for Track {
	fn from(name: Arc<String>) -> Self {
		Self { name }
	}
}

impl AsRef<str> for Track {
	fn as_ref(&self) -> &str {
		&self.name
	}
}

#[derive(Debug, Default)]
struct State {
	// Groups in order of arrival.
	// If None, the group has expired but was not in the front of the queue.
	groups: VecDeque<Option<Produce<GroupProducer, GroupConsumer>>>,

	// +1 every time we remove a group from the front.
	offset: usize,

	// Some if the track is closed
	closed: Option<Result<()>>,

	// The highest sequence number received.
	max: Option<u64>,
}

impl State {
	fn create_group(&mut self, info: Group, expires: ExpiresProducer) -> Result<GroupProducer> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		let group = GroupProducer::new(info.clone(), expires);

		// As a sanity check, make sure this is not a duplicate.
		if self
			.groups
			.iter()
			.filter_map(|g| g.as_ref())
			.any(|g| g.producer.sequence == group.sequence)
		{
			return Err(Error::Duplicate);
		}

		self.max = Some(self.max.unwrap_or_default().max(group.sequence));

		self.groups.push_back(Some(Produce {
			consumer: group.consume(),
			producer: group.clone(),
		}));

		Ok(group)
	}

	fn append_group(&mut self, expires: ExpiresProducer) -> Result<GroupProducer> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		let sequence = match self.max {
			Some(sequence) => sequence + 1,
			None => 0,
		};
		self.max = Some(sequence);

		let group = GroupProducer::new(Group { sequence }, expires);

		self.groups.push_back(Some(Produce {
			consumer: group.consume(),
			producer: group.clone(),
		}));

		Ok(group)
	}

	fn abort(&mut self, err: Error) -> Result<()> {
		if let Some(Err(err)) = self.closed.clone() {
			return Err(err);
		}

		self.closed = Some(Err(err));

		Ok(())
	}

	fn close(&mut self) -> Result<()> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		self.closed = Some(Ok(()));

		Ok(())
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	info: Track,
	state: watch::Sender<State>,
	subscribers: Subscribers,
	delivery: DeliveryProducer,
	expires: ExpiresProducer,
}

impl fmt::Debug for TrackProducer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackProducer")
			.field("info", &self.info)
			.field("state", &self.state.borrow().deref())
			.field("subscribers", &self.subscribers)
			.finish()
	}
}

impl TrackProducer {
	pub fn new<T: Into<Track>>(info: T, delivery: Delivery) -> Self {
		let info = info.into();

		let delivery = DeliveryProducer::new(delivery);

		Self {
			state: watch::Sender::new(State::default()),
			expires: ExpiresProducer::new(delivery.consume()),
			delivery,
			subscribers: Default::default(),
			info,
		}
	}

	pub fn info(&self) -> Track {
		self.info.clone()
	}

	/// A handle to update the delivery information.
	pub fn delivery(&mut self) -> &mut DeliveryProducer {
		&mut self.delivery
	}

	/// Information about all of the subscribers of this track.
	pub fn subscribers(&mut self) -> &mut Subscribers {
		&mut self.subscribers
	}

	/// Return a handle controlling when groups are expired.
	pub fn expires(&mut self) -> &mut ExpiresProducer {
		&mut self.expires
	}

	/// Create a new [GroupProducer] with the given info.
	///
	/// Returns an error if the track is closed.
	pub fn create_group<T: Into<Group>>(&mut self, info: T) -> Result<GroupProducer> {
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			result = state.create_group(info.into(), self.expires.clone());
			result.is_ok()
		});

		result
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer> {
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			result = state.append_group(self.expires.clone());
			result.is_ok()
		});

		result
	}

	pub fn close(&mut self) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			result = state.close();
			result.is_ok()
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			result = state.abort(err);
			result.is_ok()
		});

		result
	}

	/// Create a new consumer for the track.
	pub fn subscribe(&self, delivery: Delivery) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			subscriber: self.subscribers.subscribe(delivery),
			index: 0,
			expires: self.expires.consume(),
			delivery: self.delivery.consume(),
		}
	}

	/// Block until there are no active consumers.
	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let state = self.state.clone();
		async move {
			state.closed().await;
		}
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Deref for TrackProducer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// A consumer for a track, used to read groups.
///
/// If the consumer is cloned, it will receive a copy of all unread groups.
//#[derive(Clone)]
pub struct TrackConsumer {
	info: Track,

	state: watch::Receiver<State>,

	subscriber: Subscriber,

	expires: ExpiresConsumer,

	// We last returned this group, factoring in offset
	index: usize,

	delivery: DeliveryConsumer,
}

impl fmt::Debug for TrackConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackConsumer")
			.field("name", &self.name)
			.field("state", &self.state.borrow().deref())
			.field("subscriber", &self.subscriber)
			.field("index", &self.index)
			.field("delivery", &self.delivery)
			.finish()
	}
}

impl TrackConsumer {
	pub fn info(&self) -> Track {
		self.info.clone()
	}

	/// Return the next group received over the network, in any order.
	///
	/// See [TrackConsumerOrdered] if you're willing to buffer groups in order.
	///
	/// NOTE: This can have gaps due to congestion.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		loop {
			// Wait until there's a new latest group or the track is closed.
			let state = self
				.state
				.wait_for(|state| state.closed.is_some() || self.index < state.offset + state.groups.len())
				.await
				.map_err(|_| Error::Cancel)?;

			for i in self.index.saturating_sub(state.offset)..state.groups.len() {
				// If None, the group has expired out of order.
				if let Some(group) = &state.groups[i] {
					self.index = state.offset + i + 1;

					let info = self.subscriber.current();
					if self.expires.is_expired(group.consumer.sequence, info.max_latency) {
						// Skip expired groups for this consumer
						continue;
					}

					// TODO skip if expired
					return Ok(Some(group.consumer.clone()));
				}
			}

			match &state.closed {
				Some(Ok(_)) => return Ok(None),
				Some(Err(err)) => return Err(err.clone()),
				// There must have been a new None group
				// This can happen if an immediately expired group is received, or just a race.
				_ => {}
			}
		}
	}

	/// Block until the track is closed.
	pub fn closed(&self) -> impl Future<Output = Result<()>> {
		let mut state = self.state.clone();

		async move {
			match state.wait_for(|state| state.closed.is_some()).await {
				Ok(state) => state.closed.clone().unwrap(),
				Err(_) => Err(Error::Cancel),
			}
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Return a handle allowing you to update the subscriber's priority/max_latency.
	pub fn subscriber(&mut self) -> &mut Subscriber {
		&mut self.subscriber
	}

	/// Return a handle to detect when groups are expired.
	///
	/// This is used internally, but worth exporting I guess.
	pub fn expires(&mut self) -> &mut ExpiresConsumer {
		&mut self.expires
	}

	/// Return a handle to update the delivery information.
	pub fn delivery(&mut self) -> &mut DeliveryConsumer {
		&mut self.delivery
	}

	/// Convert to a helper that returns groups in order, if possible.
	pub fn ordered(self) -> TrackConsumerOrdered {
		TrackConsumerOrdered::new(self)
	}
}

impl Deref for TrackConsumer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl TrackConsumer {
	pub fn assert_group(&mut self) -> GroupConsumer {
		self.next_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	pub fn assert_no_group(&mut self) {
		assert!(
			self.next_group().now_or_never().is_none(),
			"next group would not have blocked"
		);
	}

	pub fn assert_not_closed(&self) {
		assert!(self.closed().now_or_never().is_none(), "should not be closed");
	}

	pub fn assert_closed(&self) {
		assert!(self.closed().now_or_never().is_some(), "should be closed");
	}

	// TODO assert specific errors after implementing PartialEq
	pub fn assert_error(&self) {
		assert!(
			self.closed().now_or_never().expect("should not block").is_err(),
			"should be error"
		);
	}

	pub fn assert_is_clone(&self, other: &Self) {
		assert!(self.is_clone(other), "should be clone");
	}

	pub fn assert_not_clone(&self, other: &Self) {
		assert!(!self.is_clone(other), "should not be clone");
	}
}

struct TrackRequestState {
	// If we got a response already, save it here.
	ready: Option<Result<TrackProducer>>,

	// Subscribers waiting for a response.
	// We don't use just `ready` because TrackProducer would be unused for a split second.
	subscribers: Vec<(Delivery, oneshot::Sender<Result<TrackConsumer>>)>,
}

#[derive(Clone)]
pub struct TrackRequest {
	info: Track,
	state: watch::Sender<TrackRequestState>,
}

impl TrackRequest {
	pub(super) fn new<T: Into<Track>>(track: T) -> Self {
		Self {
			info: track.into(),
			state: watch::Sender::new(TrackRequestState {
				ready: None,
				subscribers: Vec::new(),
			}),
		}
	}

	pub fn info(&self) -> &Track {
		&self.info
	}

	pub fn respond(&self, response: Result<TrackProducer>) {
		self.state.send_if_modified(|state| {
			for (subscriber, tx) in state.subscribers.drain(..) {
				match &response {
					Ok(track) => tx.send(Ok(track.subscribe(subscriber))),
					Err(err) => tx.send(Err(err.clone())),
				}
				.ok();
			}

			state.ready = Some(response);
			true
		});
	}

	pub fn subscribe(&self, delivery: Delivery) -> impl Future<Output = Result<TrackConsumer>> {
		let (tx, rx) = oneshot::channel();

		self.state.send_if_modified(|state| {
			match &state.ready {
				Some(Ok(track)) => {
					tx.send(Ok(track.subscribe(delivery))).ok();
				}
				Some(Err(err)) => {
					tx.send(Err(err.clone())).ok();
				}
				None => state.subscribers.push((delivery, tx)),
			};

			false
		});

		async move { rx.await.map_err(|_| Error::Cancel)? }
	}

	pub fn unused(&self) -> impl Future<Output = ()> {
		let mut state = self.state.subscribe();
		async move {
			let producer = {
				let state = match state.wait_for(|state| state.ready.is_some()).await {
					Ok(state) => state,
					Err(_) => return,
				};

				match state.ready.as_ref().unwrap() {
					Ok(producer) => producer.clone(),
					Err(_) => return,
				}
			};

			producer.unused().await;
		}
	}
}

impl Deref for TrackRequest {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// A [TrackConsumer] that returns groups in creation order, if possible.
///
/// It's recommended to set [Delivery::ordered] too if you REALLY want head-of-line blocking.
/// The user experience would be to buffer rather than skip any groups, except in severe congestion.
///
/// With [Delivery::ordered] not set, we will try our best to return groups in order up to `max_latency`.
/// This produces a hybrid experience where we'll buffer up until a point then skip ahead to newer groups.
//
// TODO: There's no group dropped message (yet), so we guess based on min/max timestamps.
pub struct TrackConsumerOrdered {
	track: TrackConsumer,
	expected: u64,
	pending: VecDeque<GroupConsumer>,
}

impl TrackConsumerOrdered {
	pub fn new(track: TrackConsumer) -> Self {
		Self {
			track,
			expected: 0,
			pending: VecDeque::new(),
		}
	}

	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		let mut expires = self.track.expires().clone();

		loop {
			tokio::select! {
				// Get the next group from the track.
				Some(group) = self.track.next_group().transpose() => {
					let group = group?;

					// If we're looking for this sequence number, return it.
					if group.sequence == self.expected {
						self.expected += 1;
						return Ok(Some(group));
					}

					// If it's old, skip it.
					if group.sequence < self.expected {
						continue;
					}

					// If it's new, insert it into the buffered queue based on the sequence number ascending.
					let index = self.pending.partition_point(|g| g.sequence < group.sequence);
					self.pending.insert(index, group);
				}
				Some(next) = async {
					loop {
						// Get the oldest group in the buffered queue.
						let first = self.pending.front()?;

						// Wait until it has a timestamp available. (TODO would be nice to make this required)
						let Ok(timestamp) = first.timestamp().await else {
							self.pending.pop_front();
							continue;
						};

						// Wait until the first frame of the group would have been expired.
						// This doesn't mean the entire group is expired, because that uses the max_timestamp.
						// But even if the group has one frame this will still unstuck the consumer.
						expires.wait_expired(first.sequence, timestamp).await;
						return self.pending.pop_front()
					}
				} => {
					// We found the next group in order, so update the expected sequence number.
					self.expected = next.sequence + 1;
					return Ok(Some(next));
				}
			}
		}
	}
}

impl Deref for TrackConsumerOrdered {
	type Target = TrackConsumer;

	fn deref(&self) -> &Self::Target {
		&self.track
	}
}

impl DerefMut for TrackConsumerOrdered {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.track
	}
}
