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

use futures::StreamExt;
use tokio::{sync::watch, time::Instant};
use web_async::FuturesExt;

use crate::{Error, Result};

use super::{Group, GroupConsumer, GroupProducer};

use std::{collections::VecDeque, future::Future, ops::Deref};

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	/// The name of the track.
	pub name: String,

	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: std::time::Duration,
}

#[derive(Default)]
struct TrackState {
	// Groups in order of arrival.
	// If None, the group has expired but was not in the front of the queue.
	groups: VecDeque<Option<GroupProducer>>,

	// +1 every time we remove a group from the front.
	offset: usize,

	// The highest sequence number seen.
	max_sequence: Option<u64>,

	// Some if the track is closed
	closed: Option<Result<()>>,
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	info: Track,
	state: watch::Sender<TrackState>,
}

impl TrackProducer {
	pub fn new(info: Track) -> Self {
		let this = Self {
			info,
			state: Default::default(),
		};
		web_async::spawn(this.clone().run_expires());
		this
	}

	pub fn info(&self) -> &Track {
		&self.info
	}

	/// Create a new group with the given info.
	///
	/// Returns an error if the track is closed.
	pub fn create_group(&mut self, info: Group) -> Result<GroupProducer> {
		let group = GroupProducer::new(info);
		let mut result = Ok(group.clone());

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			// As a sanity check, make sure this is not a duplicate.
			if state
				.groups
				.iter()
				.filter_map(|g| g.as_ref())
				.any(|g| g.sequence == group.sequence)
			{
				result = Err(Error::Duplicate);
				return false;
			}

			if group.sequence >= state.max_sequence.unwrap_or(0) {
				state.max_sequence = Some(group.sequence);
			}

			state.groups.push_back(Some(group));
			true
		});

		result
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer> {
		let mut result = Err(Error::Cancel);

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			let sequence = match state.max_sequence {
				Some(sequence) => sequence + 1,
				None => 0,
			};

			let group = GroupProducer::new(Group { sequence });
			state.max_sequence = Some(sequence);
			state.groups.push_back(Some(group.clone()));
			result = Ok(group);

			true
		});

		result
	}

	pub fn close(&mut self) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			state.closed = Some(Ok(()));
			true
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(Err(closed)) = state.closed.clone() {
				result = Err(closed);
				return false;
			}

			state.closed = Some(Err(err));
			true
		});

		result
	}

	/// Create a new consumer for the track.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			index: 0,
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

	async fn run_expires(self) {
		let mut receiver = self.state.subscribe();
		let mut arrived: VecDeque<Instant> = VecDeque::new();

		loop {
			let (next, closed) = {
				let state = receiver.borrow();

				// This monster of an iterator:
				// - combines the group and arrival timestamp
				// - only looks at existing groups older than the max sequence
				// - finds the group with the minimum arrival timestamp
				// - returns the index and arrival timestamp of the group to expire
				//
				// We sleep by that amount, and if we wake up, remove that index.
				let next = state
					.groups
					.iter()
					.zip(arrived.iter())
					.enumerate()
					.filter_map(|(index, (group, when))| group.as_ref().map(|g| (index, g.sequence, when)))
					.filter(|(_index, sequence, _when)| *sequence < state.max_sequence.unwrap_or(0))
					.min_by_key(|(_index, _sequence, when)| *when)
					.map(|(index, _sequence, when)| (index, *when));

				(next, state.closed.is_some())
			};

			if closed && next.is_none() {
				// No more groups to expire, only the last one is left.
				break;
			}

			tokio::select! {
				Some(()) = async {
					let next = next?.1;

					if !self.max_latency.is_zero() {
						// Sleep until the group expires.
						tokio::time::sleep_until(next + self.max_latency).await;
					}

					Some(())
				} => {
					let (index, _) = next.unwrap();

					self.state.send_if_modified(|state| {
						let mut group = state.groups.get_mut(index).unwrap().take().expect("group must have been Some");
						group.abort(Error::Expired).ok();

						while state.groups.front().is_none() {
							state.groups.pop_front();
							arrived.pop_front();

							state.offset += 1;
						}

						false
					});
				},
				_ = receiver.changed() => {
					let now = Instant::now();

					let state = receiver.borrow_and_update();
					for _ in state.groups.iter().skip(arrived.len()) {
						arrived.push_back(now);
					}
				},
			};
		}
	}
}

impl From<Track> for TrackProducer {
	fn from(info: Track) -> Self {
		TrackProducer::new(info)
	}
}

impl Drop for TrackProducer {
	fn drop(&mut self) {
		// +1 because of run_expires
		if self.state.sender_count() > 2 {
			return;
		}

		self.state.send_if_modified(|state| {
			if state.closed.is_some() {
				return false;
			}

			state.closed = Some(Err(Error::Cancel));
			true
		});
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
#[derive(Clone)]
pub struct TrackConsumer {
	info: Track,

	state: watch::Receiver<TrackState>,

	// We last returned this group, factoring in offset
	index: usize,
}

impl TrackConsumer {
	pub fn info(&self) -> &Track {
		&self.info
	}

	/// Return the next group in order.
	///
	/// NOTE: This can have gaps if the reader is too slow or there were network slowdowns.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		loop {
			// Wait until there's a new latest group or the track is closed.
			let state = self
				.state
				.wait_for(|state| state.closed.is_some() || self.index < state.offset + state.groups.len())
				.await
				.map_err(|_| Error::Cancel)?;

			for i in self.index.saturating_sub(state.offset)..state.groups.len() {
				if let Some(group) = &state.groups[i] {
					self.index = state.offset + i + 1;
					return Ok(Some(group.consume()));
				}
			}

			match &state.closed {
				Some(Ok(_)) => return Ok(None),
				Some(Err(err)) => return Err(err.clone()),
				_ => continue, // There must have been a new None group.
			}
		}
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

	/// Proxy all groups and errors to the given producer.
	///
	/// Returns an error on any unexpected close, which can happen if the [TrackProducer] is cloned.
	pub async fn proxy(mut self, mut dst: TrackProducer) -> Result<()> {
		let mut tasks = futures::stream::FuturesUnordered::new();

		loop {
			tokio::select! {
				biased;
				Some(res) = self.next_group().transpose() => {
					match res {
						Ok(group) => {
							let dst = dst.create_group(group.info().clone())?;
							tasks.push(group.proxy(dst));
						}
						Err(err) => return dst.abort(err),
					}
				}
				// Wait until all groups have finished being proxied.
				Some(_) = tasks.next() => (),
				// We're done with the proxy.
				else => return Ok(()),
			}
		}
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
