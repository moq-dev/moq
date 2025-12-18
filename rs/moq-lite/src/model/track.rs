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

use tokio::sync::watch;

use crate::{Error, Produce, Result};

use super::{Group, GroupConsumer, GroupProducer};

use std::{cmp::Ordering, future::Future};

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	pub name: String,
	pub priority: u8,
}

impl Track {
	pub fn new<T: Into<String>>(name: T) -> Self {
		Self {
			name: name.into(),
			priority: 0,
		}
	}

	pub fn produce(self) -> Produce<TrackProducer, TrackConsumer> {
		let producer = TrackProducer::new(self);
		let consumer = producer.consume();
		Produce { producer, consumer }
	}
}

#[derive(Default)]
struct TrackState {
	latest: Option<GroupConsumer>,
	closed: Option<Result<()>>,
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	pub info: Track,
	state: watch::Sender<TrackState>,
}

impl TrackProducer {
	fn new(info: Track) -> Self {
		Self {
			info,
			state: Default::default(),
		}
	}

	/// Insert a group into the track, returning true if this is the latest group.
	pub fn insert_group(&mut self, group: GroupConsumer) -> bool {
		self.state.send_if_modified(|state| {
			assert!(state.closed.is_none());

			if let Some(latest) = &state.latest {
				match group.info.cmp(&latest.info) {
					Ordering::Less => return false,
					Ordering::Equal => return false,
					Ordering::Greater => (),
				}
			}

			state.latest = Some(group.clone());
			true
		})
	}

	/// Create a new group with the given sequence number.
	///
	/// If the sequence number is not the latest, this method will return None.
	pub fn create_group(&mut self, info: Group) -> Option<GroupProducer> {
		let group = info.produce();
		self.insert_group(group.consumer).then_some(group.producer)
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> GroupProducer {
		let mut producer = None;

		self.state.send_if_modified(|state| {
			assert!(state.closed.is_none());

			let sequence = state.latest.as_ref().map_or(0, |group| group.info.sequence + 1);
			let group = Group { sequence }.produce();
			state.latest = Some(group.consumer);
			producer = Some(group.producer);

			true
		});

		producer.unwrap()
	}

	/// Create a group with a single frame.
	pub fn write_frame<B: Into<bytes::Bytes>>(&mut self, frame: B) {
		let mut group = self.append_group();
		group.write_frame(frame.into());
		group.close();
	}

	pub fn close(self) {
		self.state.send_modify(|state| state.closed = Some(Ok(())));
	}

	pub fn abort(self, err: Error) {
		self.state.send_modify(|state| state.closed = Some(Err(err)));
	}

	/// Create a new consumer for the track.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			prev: None,
		}
	}

	/// Block until there are no active consumers.
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

impl From<Track> for TrackProducer {
	fn from(info: Track) -> Self {
		TrackProducer::new(info)
	}
}

/// A consumer for a track, used to read groups.
#[derive(Clone)]
pub struct TrackConsumer {
	pub info: Track,
	state: watch::Receiver<TrackState>,
	prev: Option<u64>, // The previous sequence number
}

impl TrackConsumer {
	/// Return the next group in order.
	///
	/// NOTE: This can have gaps if the reader is too slow or there were network slowdowns.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		// Wait until there's a new latest group or the track is closed.
		let state = match self
			.state
			.wait_for(|state| {
				state.latest.as_ref().map(|group| group.info.sequence) > self.prev || state.closed.is_some()
			})
			.await
		{
			Ok(state) => state,
			Err(_) => return Err(Error::Cancel),
		};

		match &state.closed {
			Some(Ok(_)) => return Ok(None),
			Some(Err(err)) => return Err(err.clone()),
			_ => {}
		}

		// If there's a new latest group, return it.
		let group = state.latest.clone().unwrap();
		self.prev = Some(group.info.sequence);

		Ok(Some(group))
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Result<()> {
		match self.state.clone().wait_for(|state| state.closed.is_some()).await {
			Ok(state) => state.closed.clone().unwrap(),
			Err(_) => Err(Error::Cancel),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
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
