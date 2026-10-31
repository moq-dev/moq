//! Serve one broadcast's archive as an ordinary broadcast.
//!
//! A [`Consumer`] lists a [`Store`] prefix a [`Producer`](crate::Producer) wrote and creates every
//! stored track on a `moq_net` broadcast, the archived catalog included, with its stored
//! timescale and priority and no max age: the store's retention bounds what it holds. Each
//! track's live feed starts at its newest stored sequence, and every older group is served on
//! demand from the store through [`track::Requests`](moq_net::track::Requests) fills, each sequence in its recorded form: a
//! group as a group, a datagram as a datagram.
//!
//! A growing archive is followed: every [`Config::refresh`], one delimiter LIST of the prefix
//! finds added tracks, and one LIST per track after its newest key finds new objects. Once the
//! prefix holds `.end`, each track ends as recorded and refreshing stops. Without it the archive
//! stays live, which is also how one whose producer stopped early reads.
//!
//! ```no_run
//! # async fn example(broadcast: moq_net::broadcast::Producer) -> moq_archive::Result<()> {
//! use moq_archive::object_store::memory::InMemory;
//!
//! let store = moq_archive::Store::new(InMemory::new(), "recordings/demo");
//! let consumer = moq_archive::Consumer::open(store, broadcast, Default::default()).await?;
//! // Every stored track exists now: announce its publication, then serve it.
//! consumer.run().await?;
//! # Ok(())
//! # }
//! ```

mod index;
mod track;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use moq_net::broadcast;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};

use crate::layout::{self, End, Object};
use crate::{Error, Result, Store};
use index::{Index, Start};

/// How a [`Consumer`] follows and caches its archive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// How often a growing archive is listed for new objects and tracks.
	pub refresh: Duration,
	/// The most object bytes kept in memory, so neighbouring groups come from one GET.
	pub cache: u64,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			refresh: Duration::from_secs(1),
			cache: 64 * 1024 * 1024,
		}
	}
}

impl Config {
	/// Set [`refresh`](Self::refresh).
	pub fn with_refresh(mut self, refresh: Duration) -> Self {
		self.refresh = refresh;
		self
	}

	/// Set [`cache`](Self::cache).
	pub fn with_cache(mut self, bytes: u64) -> Self {
		self.cache = bytes;
		self
	}
}

/// Serves one archived broadcast until the broadcast closes.
pub struct Consumer<S> {
	shared: Arc<Shared<S>>,
	broadcast: broadcast::Producer,
	/// Answers requests for names the archive does not hold, and says when the broadcast closed.
	requests: broadcast::Requests,
	refresh: Duration,
	tracks: HashMap<String, Track>,
	/// Whether the prefix held `.end`, so every track ended and nothing is refreshed.
	ended: bool,
}

struct Shared<S> {
	store: Store<S>,
	cache: quick_cache::sync::Cache<Path, Arc<Object>, Weight>,
}

struct Track {
	index: Arc<Mutex<Index>>,
	live: track::Live,
	dynamic: moq_net::track::Requests,
}

/// What one refresh found, applied to the served tracks at once.
#[derive(Default)]
struct Refreshed {
	/// Tracks new since the last refresh, with their index.
	added: Vec<(String, moq_net::track::Info, Arc<Mutex<Index>>)>,
	/// Each track's new objects, fed live from the sequence given.
	pushes: Vec<(String, Vec<Arc<Object>>, u64)>,
	/// Each track's end, once the prefix holds `.end`.
	ends: Option<Vec<(String, End)>>,
}

/// A track as a refresh starts: where its listing resumes and whether its feed started.
struct Snapshot {
	name: String,
	index: Arc<Mutex<Index>>,
	started: bool,
}

impl<S: ObjectStore> Consumer<S> {
	/// List `store`'s prefix and create every stored track on `broadcast`, so a host can announce
	/// it with every track listed.
	///
	/// Fails with [`Error::NotFound`] when the prefix holds no track, and with [`Error::Path`]
	/// when it holds a key the layout does not name.
	pub async fn open(store: Store<S>, broadcast: broadcast::Producer, config: Config) -> Result<Self> {
		let shared = Arc::new(Shared {
			store,
			cache: quick_cache::sync::Cache::with_weighter(
				// Estimated item count; the byte capacity below is the real bound.
				1024,
				config.cache,
				Weight,
			),
		});
		let mut consumer = Self {
			shared,
			requests: broadcast.requests(),
			broadcast,
			refresh: config.refresh,
			tracks: HashMap::new(),
			ended: false,
		};
		let refreshed = refresh(consumer.shared.clone(), Vec::new()).await?;
		if refreshed.added.is_empty() {
			return Err(Error::NotFound(consumer.shared.store.prefix().to_string()));
		}
		if let Err(err) = consumer.apply(refreshed) {
			consumer.abort(moq_net::Error::Stream(moq_net::StreamError::Internal));
			return Err(err);
		}
		Ok(consumer)
	}

	/// Serve the archive until the broadcast closes, following it while it grows.
	///
	/// Fails when the store does, or when it holds a key or object the layout does not name.
	/// Every track still live is then aborted with that failure; one left live when the
	/// broadcast closes, or when the consumer is dropped, is cancelled.
	pub async fn run(mut self) -> Result<()> {
		let result = self.serve().await;
		if result.is_err() {
			self.abort(moq_net::Error::Stream(moq_net::StreamError::Internal));
		}
		result
	}

	async fn serve(&mut self) -> Result<()> {
		let mut fills = kio::Tasks::new();
		let mut refreshing: Option<BoxFuture<'static, Result<Refreshed>>> = None;

		enum Event {
			Refreshed(Result<Refreshed>),
			Closed,
		}

		loop {
			if refreshing.is_none() && !self.ended {
				let snapshots = self.snapshots();
				let (shared, period) = (self.shared.clone(), self.refresh);
				refreshing = Some(
					async move {
						kio::clock::sleep(period).await;
						refresh(shared, snapshots).await
					}
					.boxed(),
				);
			}

			let event = kio::wait(|waiter| {
				for track in self.tracks.values() {
					while let Poll::Ready(Ok(fill)) = track.dynamic.poll_requested_fill(waiter) {
						let serve = track::serve(self.shared.clone(), track.index.clone(), fill);
						fills.push(task(serve.boxed()));
					}
				}
				let _ = fills.poll(waiter);
				if let Some(future) = refreshing.as_mut()
					&& let Poll::Ready(refreshed) = waiter.poll_future(future.as_mut())
				{
					return Poll::Ready(Event::Refreshed(refreshed));
				}
				loop {
					match self.requests.poll_requested_track(waiter) {
						// Every stored track already exists, so the archive holds no such name.
						Poll::Ready(Ok(request)) => request.reject(moq_net::Error::NotFound),
						Poll::Ready(Err(_)) => return Poll::Ready(Event::Closed),
						Poll::Pending => return Poll::Pending,
					}
				}
			})
			.await;

			match event {
				Event::Refreshed(refreshed) => {
					refreshing = None;
					self.apply(refreshed?)?;
				}
				Event::Closed => return Ok(()),
			}
		}
	}

	fn snapshots(&self) -> Vec<Snapshot> {
		self.tracks
			.iter()
			.map(|(name, track)| Snapshot {
				name: name.clone(),
				index: track.index.clone(),
				started: track.live.is_started(),
			})
			.collect()
	}

	fn apply(&mut self, refreshed: Refreshed) -> Result<()> {
		for (name, info, index) in refreshed.added {
			let producer = self.broadcast.create_track(name.as_str(), info)?;
			let dynamic = producer.requests();
			self.tracks.insert(
				name,
				Track {
					index,
					live: track::Live::new(producer),
					dynamic,
				},
			);
		}
		for (name, objects, from) in refreshed.pushes {
			if let Some(track) = self.tracks.get_mut(&name) {
				track.live.push(&objects, from)?;
			}
		}
		if let Some(ends) = refreshed.ends {
			for (name, end) in ends {
				if let Some(track) = self.tracks.get_mut(&name) {
					track.live.end(end)?;
				}
			}
			self.ended = true;
		}
		Ok(())
	}
}

impl<S> Consumer<S> {
	/// Abort every served track with `err`, its open group included.
	fn abort(&mut self, err: moq_net::Error) {
		for (_, track) in self.tracks.drain() {
			track.live.abort(err.clone());
		}
	}
}

impl<S> Drop for Consumer<S> {
	fn drop(&mut self) {
		// An archive that ended already finished every track as recorded.
		if !self.ended {
			self.abort(moq_net::Error::Cancel);
		}
	}
}

/// List the prefix for added tracks and `.end`, then each track after its newest key, and GET
/// what each track's live feed takes next.
async fn refresh<S: ObjectStore>(shared: Arc<Shared<S>>, mut tracks: Vec<Snapshot>) -> Result<Refreshed> {
	let store = &shared.store;
	let prefix = store.prefix();
	let scope = (!prefix.as_ref().is_empty()).then_some(prefix);
	let listed = store.inner().list_with_delimiter(scope).await?;

	let mut ended = false;
	for object in &listed.objects {
		match object.location.filename() {
			Some(layout::BROADCAST_END) => ended = true,
			_ => return Err(Error::Path(object.location.to_string())),
		}
	}

	let mut refreshed = Refreshed::default();
	for dir in listed.common_prefixes {
		let segment = dir.filename().ok_or_else(|| Error::Path(dir.to_string()))?;
		let name = layout::parse_track(segment)?;
		if tracks.iter().any(|track| track.name == name) {
			continue;
		}
		let stored = index::info(store.inner(), &dir).await?;
		let info = track::info(&stored)?;
		let index = Arc::new(Mutex::new(Index::new(dir, &stored)?));
		refreshed.added.push((name.clone(), info, index.clone()));
		tracks.push(Snapshot {
			name,
			index,
			started: false,
		});
	}

	let mut ends = Vec::new();
	for track in tracks {
		let (dir, cursor, timed) = {
			let index = track.index.lock().unwrap();
			(index.dir.clone(), index.cursor().clone(), index.timed())
		};
		let listed = index::list(store.inner(), &dir, &cursor, timed, ended).await?;
		track.index.lock().unwrap().extend(&listed.objects);
		if let Some(end) = listed.end {
			ends.push((track.name.clone(), end));
		}

		let (objects, from) = match track.started {
			// Everything new, in order.
			true => (listed.objects, 0),
			// Only the newest sequence: fills serve the rest.
			false => {
				let Some((start, path)) = track.index.lock().unwrap().newest() else {
					continue;
				};
				let newest = object(&shared, &path, start, timed).await?;
				let sequence = newest.entries.last().ok_or(Error::Empty)?.sequence();
				(track.index.lock().unwrap().holding(sequence, 0).0, sequence)
			}
		};
		let mut fetched = Vec::with_capacity(objects.len());
		for (start, path) in objects {
			fetched.push(object(&shared, &path, start, timed).await?);
		}
		if !fetched.is_empty() {
			refreshed.pushes.push((track.name, fetched, from));
		}
	}
	refreshed.ends = ended.then_some(ends);
	Ok(refreshed)
}

/// The object at `path`, from the cache or one GET, checked against the key it is listed under.
async fn object<S: ObjectStore>(shared: &Shared<S>, path: &Path, start: Start, timed: bool) -> Result<Arc<Object>> {
	shared
		.cache
		.get_or_insert_async(path, async {
			let bytes = shared.store.inner().get(path).await?.bytes().await?;
			let object = Object::decode(bytes, timed)?;
			let found = object.start()?;
			if (found.group, found.frame) != start {
				return Err(Error::Path(path.to_string()));
			}
			Ok(Arc::new(object))
		})
		.await
}

/// Adapt a boxed future into a [`kio::Tasks`] task.
fn task(mut future: BoxFuture<'static, ()>) -> impl FnMut(&kio::Waiter) -> Poll<()> + Send {
	move |waiter| waiter.poll_future(future.as_mut())
}

/// Weighs a cached object by its payload bytes plus a small per-frame overhead.
#[derive(Clone)]
struct Weight;

impl quick_cache::Weighter<Path, Arc<Object>> for Weight {
	fn weight(&self, _path: &Path, object: &Arc<Object>) -> u64 {
		object.weight()
	}
}
