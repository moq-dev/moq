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

use futures::FutureExt;
use tokio::{sync::watch, time::Instant};

use super::{Group, GroupConsumer, GroupProducer};
use crate::{Error, Produce, Result, TrackMeta, TrackMetaConsumer, TrackMetaProducer};

use std::{
	collections::VecDeque,
	fmt,
	future::Future,
	ops::Deref,
	sync::{
		atomic::{AtomicU64, Ordering},
		Arc,
	},
};

static TRACK_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

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

impl Track {
	pub fn new(name: &str) -> Self {
		Self {
			name: name.to_string(),
			priority: 0,
			max_latency: std::time::Duration::ZERO,
		}
	}

	pub fn produce(self) -> Produce<TrackProducer, TrackConsumer> {
		let producer = TrackProducer::new(self.clone());
		Produce {
			consumer: producer.consume(),
			producer,
		}
	}
}

impl<T: AsRef<str>> From<T> for Track {
	fn from(name: T) -> Self {
		Self::new(name.as_ref())
	}
}

/// Static information about a track
///
/// Only used to make accessing the name easy/fast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
	pub name: Arc<String>,
}

#[derive(Debug, Default)]
struct State {
	// Groups in order of arrival.
	// If None, the group has expired but was not in the front of the queue.
	groups: VecDeque<Option<GroupState>>,

	// +1 every time we remove a group from the front.
	offset: usize,

	// The highest sequence number received, and when.
	max: Option<(u64, Instant)>,

	// Some if the track is closed
	closed: Option<Result<()>>,
}

#[derive(Debug)]
struct GroupState {
	// We need a producer in order to abort on expired/close.
	producer: GroupProducer,

	// If we didn't hold a consumer, `unused()` would be true.
	consumer: GroupConsumer,

	// TODO We should use timestamps on a per-track basis, instead of wall clock time.
	when: Instant,
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	id: u64,
	info: TrackInfo,
	state: watch::Sender<State>,
	meta: Produce<TrackMetaProducer, TrackMetaConsumer>,
}

impl TrackProducer {
	pub fn new<T: Into<Track>>(info: T) -> Self {
		let info = info.into();

		let this = Self {
			id: TRACK_ID_COUNTER.fetch_add(1, Ordering::Relaxed),
			state: Default::default(),
			meta: TrackMeta {
				priority: info.priority,
				max_latency: info.max_latency,
			}
			.produce(),
			info: TrackInfo {
				name: Arc::new(info.name),
			},
		};
		web_async::spawn_named("expires", this.clone().run_expires());
		this
	}

	pub fn info(&self) -> TrackInfo {
		self.info.clone()
	}

	// Information about all of the consumers of this track.
	pub fn meta(&self) -> TrackMetaConsumer {
		self.meta.consumer.clone()
	}

	/// Create a new group with the given info.
	///
	/// Returns an error if the track is closed.
	pub fn create_group<T: Into<Group>>(&mut self, info: T) -> Result<GroupProducer> {
		let group = GroupProducer::new(info.into());
		let mut result = Err(Error::Cancel);

		// NOTE: The TrackProducer is unused when this returns None.
		let meta = self.meta.consumer.get().unwrap_or_default();

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
				.any(|g| g.producer.sequence == group.sequence)
			{
				result = Err(Error::Duplicate);
				return false;
			}

			let now = if group.sequence >= state.max.map(|m| m.0).unwrap_or(0) {
				let now = Instant::now();
				state.max = Some((group.sequence, now));
				now
			} else if meta.max_latency.is_zero() {
				// Guaranteed to expire, we don't even need to call Instant::now
				result = Err(Error::Expired);
				return false;
			} else {
				// Optimization: Check if this group should have expired by now.
				// We avoid inserting and creating groups that would be instantly expired.
				let max = state.max.expect("impossible").1;

				let now = Instant::now();
				if now - max > meta.max_latency {
					result = Err(Error::Expired);
					return false;
				}

				now
			};

			result = Ok(group.clone());

			let group = GroupState {
				consumer: group.consume(),
				producer: group,
				when: now,
			};

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

			let sequence = match state.max {
				Some((sequence, _)) => sequence + 1,
				None => 0,
			};

			let group = GroupProducer::new(Group { sequence });
			result = Ok(group.clone());

			let now = Instant::now();

			let group = GroupState {
				consumer: group.consume(),
				producer: group,
				when: now,
			};

			state.max = Some((sequence, now));
			state.groups.push_back(Some(group));

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
			id: self.id,
			info: self.info.clone(),
			state: self.state.subscribe(),
			meta: self.meta.producer.clone(),
			index: 0,
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

	// TODO This never stops?
	async fn run_expires(self) {
		let mut updates = self.state.subscribe();
		let mut meta = self.meta.consumer.clone();

		loop {
			let max_latency = meta.get().unwrap_or_default().max_latency;
			let mut expires = None;
			let mut closed = false;

			self.state.send_if_modified(|state| {
				closed = state.closed.is_some();

				loop {
					// Find the next group to expire, which should be index 0 but not if we receive out of order.
					expires = state
						.groups
						.iter()
						// Get the index as well, so we can remove it.
						.enumerate()
						// Only look at Some entries, so we can ignore None groups.
						.filter_map(|(index, group)| {
							group
								.as_ref()
								.map(|g| (index, g.producer.sequence, g.when + max_latency))
						})
						// Ignore the maximum group, wherever it might be
						.filter(|(_index, sequence, _when)| *sequence < state.max.map(|m| m.0).unwrap_or(0))
						// Return the next group to expire.
						.min_by_key(|(_index, _sequence, when)| *when);

					// We found the next group to expire.
					if let Some((index, _sequence, when)) = expires {
						// Check if the group should be expired now.
						if max_latency.is_zero() || !when.elapsed().is_zero() {
							let mut group = state
								.groups
								.get_mut(index)
								.expect("index out of bounds")
								.take()
								.expect("group must have been Some");
							group.producer.abort(Error::Expired).ok();

							while let Some(None) = state.groups.front() {
								state.groups.pop_front();
								state.offset += 1;
							}

							continue;
						}
					}

					// Otherwise, we sleep until the next group will expire, or something changes.
					break;
				}

				// Never notify anybody; we're just cleaning up.
				false
			});

			if closed && expires.is_none() {
				// No more groups to expire, only the last one is left.
				break;
			}

			tokio::select! {
				// Sleep until the next group expires.
				Some(()) = async { let _: () = tokio::time::sleep_until(expires?.2).await; Some(()) } => {},
				// If the max_latency changes, rerun again.
				_ = meta.changed() => {},
				// If self.state changes, rerun again.
				_ = updates.changed() => {},
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
	type Target = TrackInfo;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// A consumer for a track, used to read groups.
///
/// If the consumer is cloned, it will receive a copy of all unread groups.
#[derive(Clone)]
pub struct TrackConsumer {
	id: u64,
	info: TrackInfo,

	state: watch::Receiver<State>,
	meta: TrackMetaProducer,

	// We last returned this group, factoring in offset
	index: usize,
}

impl fmt::Debug for TrackConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackConsumer")
			.field("name", &self.name)
			.field("state", &self.state.borrow().deref())
			.field("index", &self.index)
			.finish()
	}
}

impl TrackConsumer {
	pub fn info(&self) -> TrackInfo {
		self.info.clone()
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
				// If None, the group has expired out of order.
				if let Some(group) = &state.groups[i] {
					self.index = state.offset + i + 1;
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

	pub fn meta(&mut self) -> TrackMetaProducer {
		self.meta.clone()
	}

	/// Proxy all groups and errors to the given producer.
	///
	/// Returns an error on any unexpected close, which can happen if the [TrackProducer] is cloned.
	pub(super) async fn proxy(mut self, mut dst: TrackProducer) -> Result<()> {
		while let Some(group) = self.next_group().await.transpose() {
			match group {
				Ok(group) => {
					let dst = dst.create_group(group.info().clone())?;
					web_async::spawn_named("proxy-group", group.proxy(dst).map(|_| ()));
				}
				Err(err) => return dst.abort(err),
			}
		}

		dst.close()
	}
}

impl Deref for TrackConsumer {
	type Target = TrackInfo;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

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
