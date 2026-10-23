//! Record every track of one broadcast into a [`Store`].
//!
//! A [`Producer`] lists the broadcast's tracks (TRACKS, [`broadcast::Consumer::tracks`]) and
//! records each through its own ascending SUBSCRIBE, so it holds what the publisher's cache
//! stops serving: a group still missing once the subscription's max delay passes is a gap, never
//! a stall. Datagrams are never cached, so a track's are recorded from when its subscription
//! takes effect. The host picks the key prefix, per broadcast path and epoch say, and runs one
//! producer per broadcast; the producer stores no epoch. See the [crate docs](crate#layout) for
//! what it writes.
//!
//! A producer started under a prefix that already holds objects resumes each track at the frame
//! after its last stored one, and one under a prefix holding `.end` records nothing.
//!
//! ```no_run
//! # async fn example(broadcast: moq_net::broadcast::Consumer) -> moq_archive::Result<()> {
//! use moq_archive::object_store::memory::InMemory;
//!
//! let store = moq_archive::Store::new(InMemory::new(), "recordings/demo");
//! moq_archive::Producer::new(store, broadcast, Default::default()).run().await?;
//! # Ok(())
//! # }
//! ```

mod ordered;
mod resume;
mod track;

#[cfg(test)]
mod backends;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use moq_net::broadcast::TracksEvent;
use moq_net::{StreamError, broadcast};
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};

use crate::layout::{self, End};
use crate::path::push;
use crate::{Error, Result, Store};

/// How a [`Producer`] cuts, flushes, and prunes its objects.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// How much content an object holds before it is cut at the next group boundary: media time
	/// on a timed track, wall-clock time on an untimed one. A group whose own run in an object
	/// reaches it splits between frames.
	pub duration: Duration,
	/// The longest a frame waits in memory before its object is stored, however idle its track.
	/// The object is cut there, mid-group if need be. Keep it above [`Self::duration`], or
	/// objects are cut by it instead.
	pub flush: Duration,
	/// The longest a missing group is waited for, capping the publisher's max age. A group still
	/// missing then is a gap, and the frames behind it wait for that verdict. A track with nothing
	/// stored waits this long from its first arrival for a lower sequence, unless that one is 0.
	pub max_delay: Duration,
	/// Prune old objects (a DVR), or keep everything when `None` (an archive).
	pub dvr: Option<Dvr>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			duration: Duration::from_secs(2),
			flush: Duration::from_secs(4),
			max_delay: Duration::from_secs(10),
			dvr: None,
		}
	}
}

impl Config {
	/// Set [`duration`](Self::duration).
	pub fn with_duration(mut self, duration: Duration) -> Self {
		self.duration = duration;
		self
	}

	/// Set [`flush`](Self::flush).
	pub fn with_flush(mut self, flush: Duration) -> Self {
		self.flush = flush;
		self
	}

	/// Set [`max_delay`](Self::max_delay).
	pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
		self.max_delay = max_delay;
		self
	}

	/// Set [`dvr`](Self::dvr).
	pub fn with_dvr(mut self, dvr: impl Into<Option<Dvr>>) -> Self {
		self.dvr = dvr.into();
		self
	}
}

/// A DVR's pruning, for hosts without bucket lifecycle rules.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Dvr {
	/// How long an object stays after a newer one of its track supersedes it.
	pub window: Duration,
	/// How much longer it stays, so readers that already listed it finish their GETs.
	pub grace: Duration,
}

impl Dvr {
	/// Delete each superseded object `window + grace` after it was superseded.
	pub fn new(window: Duration, grace: Duration) -> Self {
		Self { window, grace }
	}
}

/// Records every track of one broadcast until the broadcast ends.
pub struct Producer<S> {
	shared: Arc<Shared<S>>,
}

struct Shared<S> {
	store: Store<S>,
	broadcast: broadcast::Consumer,
	config: Config,
}

/// What one track's recorder reports once the broadcast's listing ended and it stored everything.
type Recorded = Result<(String, Option<End>)>;

impl<S: ObjectStore> Producer<S> {
	/// Record `broadcast` under `store`'s prefix.
	pub fn new(store: Store<S>, broadcast: broadcast::Consumer, config: Config) -> Self {
		Self {
			shared: Arc::new(Shared {
				store,
				broadcast,
				config,
			}),
		}
	}

	/// Record until the broadcast ends, then write each track's `end` and the broadcast's `.end`.
	///
	/// Returns at once, recording nothing, when the prefix already holds `.end`, or when TRACKS
	/// is refused with not found, since the path names no broadcast. Any other refusal, a store
	/// failure, or a track whose properties differ from the stored ones fails. Dropping the
	/// future stops recording without markers, so the archive reads as still growing.
	pub async fn run(self) -> Result<()> {
		let shared = self.shared;
		let Some(mut stored) = resume::load(&shared.store).await? else {
			return Ok(());
		};

		let mut listing = Some(shared.broadcast.tracks().map_err(source)?);
		let mut queues: HashMap<String, kio::Queue<moq_net::track::Info>> = HashMap::new();
		let mut recorders: FuturesUnordered<BoxFuture<'static, Recorded>> = FuturesUnordered::new();
		let mut ends: HashMap<String, Option<End>> = HashMap::new();

		enum Event {
			Listed(moq_net::Result<Option<TracksEvent>>),
			Recorded(Recorded),
			Done,
		}

		loop {
			let event = kio::wait(|waiter| {
				if let Poll::Ready(Some(recorded)) = recorders.poll_next_unpin(&mut waiter.context()) {
					return Poll::Ready(Event::Recorded(recorded));
				}
				match listing.as_mut() {
					Some(listing) => listing.poll_next(waiter).map(Event::Listed),
					None if recorders.is_empty() => Poll::Ready(Event::Done),
					None => Poll::Pending,
				}
			})
			.await;

			match event {
				Event::Listed(Ok(Some(TracksEvent::Added { name, info }))) => {
					let queue = queues.entry(name.to_string()).or_insert_with(|| {
						let queue = kio::Queue::new();
						let record = track::record(
							shared.clone(),
							name.to_string(),
							queue.clone(),
							stored.remove(name.as_ref()),
						);
						recorders.push(record.boxed());
						queue
					});
					// Only the newest info matters, so one still queued is replaced: a blocked
					// recorder holds at most one, however often its track is re-added.
					let _ = queue.try_pop();
					// Only fails once closed, which happens when the listing ends.
					let _ = queue.try_push(info);
				}
				Event::Listed(Ok(Some(
					TracksEvent::Synced | TracksEvent::Removed { .. } | TracksEvent::Finished { .. },
				))) => {}
				Event::Listed(Ok(None)) => {
					listing = None;
					for queue in queues.values() {
						queue.close();
					}
				}
				Event::Listed(Err(err)) if queues.is_empty() && StreamError::from(&err) == StreamError::NotFound => {
					tracing::debug!(broadcast = %shared.broadcast.info().path, "not a broadcast");
					return Ok(());
				}
				Event::Listed(Err(err)) => return Err(source(err)),
				Event::Recorded(recorded) => {
					let (name, end) = recorded?;
					ends.insert(name, end);
				}
				Event::Done => break,
			}
		}

		let store = &shared.store;
		for (name, end) in &ends {
			if let Some(end) = end {
				let path = push(&layout::track_dir(store.prefix(), name)?, layout::TRACK_END)?;
				store.inner().put(&path, PutPayload::from(end.encode()?)).await?;
			}
		}
		// A stored track that ended unseen, or was never listed again, may still grow.
		let complete = ends.values().all(Option::is_some) && stored.values().all(|track| track.end.is_some());
		if complete {
			let path = push(store.prefix(), layout::BROADCAST_END)?;
			store.inner().put(&path, PutPayload::new()).await?;
		}
		Ok(())
	}
}

fn source(err: moq_net::Error) -> Error {
	Error::Source(err.to_string())
}
