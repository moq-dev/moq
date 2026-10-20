//! Serve an archive's groups through a caller-supplied broadcast.
//!
//! A [`Reader`] replays each track's timeline onto its timeline track and answers FETCH for every
//! frame those timelines commit. A group maps to the records holding it, each one object fetched
//! with a single GET, validated against its record, and kept in a byte-bounded cache so adjacent
//! groups reuse it. A request for one track never downloads another track's object. A group whose
//! records do not reach its end yet, such as an append-only log, grows as later records commit.
//! Timeline groups remain FETCHable from their original objects after broadcast-cache eviction.
//!
//! The reader never infers that a recording ended: it keeps following until the caller, who may
//! know finality out of band, calls [`Reader::finish`].
//!
//! A track whose `.info` declares no timescale replays untimed, along with its timeline: its frames
//! carry no timestamps, and nothing stands in for one.
//!
//! ```no_run
//! # async fn example(store: moq_archive::Store<moq_archive::object_store::memory::InMemory>) -> moq_archive::Result<()> {
//! use moq_archive::reader::{Config, Reader};
//!
//! let broadcast = moq_net::broadcast::Info::new().produce();
//! let timelines = [("video".to_string(), "video.timeline.z".to_string())].into();
//! let mut reader = Reader::open(store, &broadcast, Config::new(timelines)).await?;
//! let serve = reader.serve();
//! // Spawn `serve` on any executor, then call `reader.refresh()` to follow a growing archive.
//! # drop(serve);
//! # Ok(())
//! # }
//! ```

mod index;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use futures::TryStreamExt;
use futures::future::BoxFuture;
use hang::timeline::{Position, Record};
use moq_json::window;
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::ObjectStore;
use tokio::sync::watch;

use self::index::{Index, Span};
use crate::store::list::Query;
use crate::{Error, Key, Object, Result, Store};

/// Configuration for [`Reader::open`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Each served track's timeline track, from the catalog's `archive` entry.
	pub timelines: BTreeMap<String, String>,
	/// Upper bound on cached object bytes shared by every track.
	pub cache: u64,
}

impl Config {
	/// Serve the tracks `timelines` indexes with a 64 MiB object cache.
	pub fn new(timelines: BTreeMap<String, String>) -> Self {
		Self {
			timelines,
			cache: 64 * 1024 * 1024,
		}
	}

	/// Set [`cache`](Self::cache).
	pub fn with_cache(mut self, bytes: u64) -> Self {
		self.cache = bytes;
		self
	}
}

/// Serves an archive through a [`broadcast::Producer`] and follows its timelines.
pub struct Reader<T> {
	shared: Arc<Shared<T>>,
	broadcast: broadcast::Producer,
	dynamic: broadcast::Dynamic,
	timelines: Vec<Timeline>,
}

/// One track's timeline, replayed onto its own timeline track.
struct Timeline {
	/// The track it indexes.
	track: String,
	producer: track::Producer,
	/// `None` when the timeline, like the track it indexes, is untimed.
	timescale: Option<Timescale>,
	decoder: window::Decoder<Record>,
	/// The last timeline segment replayed.
	cursor: Option<u64>,
}

struct TimelineRequests {
	name: String,
	timescale: Option<Timescale>,
	dynamic: track::Dynamic,
}

struct TimelineRequest {
	name: String,
	timescale: Option<Timescale>,
	request: group::Request,
}

struct Shared<T> {
	store: Store<T>,
	index: Mutex<Index>,
	/// Stored object sequence for every validated timeline group.
	timelines: Mutex<HashMap<String, BTreeMap<u64, u64>>>,
	/// New timeline handlers, including tracks enrolled after serve starts.
	requests: kio::Queue<TimelineRequests>,
	cache: quick_cache::sync::Cache<Key, Arc<Object>, Weight>,
	/// Each track's `.info`, which is immutable, so a re-requested track costs no GET.
	infos: Mutex<HashMap<String, track::Info>>,
	/// The tracks a timeline indexes, which are the only ones served.
	served: Mutex<HashSet<String>>,
	/// Notified whenever the index changes; holds whether the caller declared the recording ended.
	changed: watch::Sender<bool>,
}

impl<T: ObjectStore> Reader<T> {
	/// Create every timeline track on `broadcast`, then replay each track's retained timeline.
	///
	/// Fails if a timeline's `.info` is missing or invalid, a timeline track already exists, or the
	/// store cannot list a timeline.
	pub async fn open(store: Store<T>, broadcast: &broadcast::Producer, config: Config) -> Result<Self> {
		let items = usize::try_from(config.cache / (1024 * 1024))
			.unwrap_or(usize::MAX)
			.max(16);
		let shared = Arc::new(Shared {
			store,
			index: Mutex::default(),
			timelines: Mutex::default(),
			requests: kio::Queue::new(),
			cache: quick_cache::sync::Cache::with_weighter(items, config.cache, Weight),
			infos: Mutex::default(),
			served: Mutex::default(),
			changed: watch::Sender::new(false),
		});

		let mut reader = Self {
			shared,
			broadcast: broadcast.clone(),
			dynamic: broadcast.dynamic(),
			timelines: Vec::new(),
		};
		for (track, timeline) in config.timelines {
			reader.add(track, timeline).await?;
		}
		reader.refresh().await?;
		Ok(reader)
	}

	/// Serve and follow `track`, whose timeline is the track `timeline`, such as one a recording
	/// enrolled after this reader opened. Its timeline replays on the next [`refresh`](Self::refresh).
	///
	/// Fails if the timeline's `.info` is missing or invalid, or `track` is already served.
	pub async fn track(&mut self, track: &str, timeline: &str) -> Result<()> {
		self.add(track.to_string(), timeline.to_string()).await
	}

	async fn add(&mut self, track: String, timeline: String) -> Result<()> {
		if self.shared.served.lock().unwrap().contains(&track) {
			return Err(moq_net::Error::Duplicate.into());
		}
		let info = track_info(&self.shared.store.get_info(&timeline).await?)?;
		let timescale = info.timescale;
		let producer = self.broadcast.create_track(timeline.clone(), info)?;
		self.shared
			.requests
			.try_push(TimelineRequests {
				name: timeline,
				timescale,
				dynamic: producer.dynamic(),
			})
			.expect("the unbounded handler queue stays open while the reader lives");
		self.shared.served.lock().unwrap().insert(track.clone());
		self.timelines.push(Timeline {
			track,
			producer,
			timescale,
			decoder: window::Decoder::new(window::ConsumerConfig::default().with_compression(true)),
			cursor: None,
		});
		Ok(())
	}

	/// Replay timeline objects committed since the last refresh.
	///
	/// Lists each timeline's keys after its last replayed segment, then GETs them in segment order.
	/// Each stored group opens with a window checkpoint, so a segment that expired or is malformed
	/// is skipped and the next one recovers the retained window, evicting whatever it popped. A
	/// malformed newest object is retried on the next refresh.
	pub async fn refresh(&mut self) -> Result<()> {
		for timeline in &mut self.timelines {
			timeline.refresh(&self.shared).await?;
		}
		Ok(())
	}

	/// Finish every replayed timeline track once the caller knows the recording ended.
	///
	/// Following stops, and a group still growing is served as it stands. Groups already committed
	/// stay servable through [`serve`](Self::serve).
	pub fn finish(self) -> Result<()> {
		self.shared.changed.send_replace(true);
		for timeline in self.timelines {
			timeline.producer.finish()?;
		}
		Ok(())
	}

	/// Answer track and group requests on the broadcast until it closes.
	///
	/// The future is independent of this reader, so it keeps serving after
	/// [`finish`](Self::finish); run it on any executor.
	pub fn serve(&self) -> impl Future<Output = ()> + Send + 'static
	where
		T: 'static,
	{
		serve(self.shared.clone(), self.dynamic.clone())
	}
}

impl Timeline {
	async fn refresh<T: ObjectStore>(&mut self, shared: &Shared<T>) -> Result<()> {
		let name = self.producer.name().to_string();
		let mut query = Query::segments(&name)?;
		if let Some(cursor) = self.cursor {
			query = query.after(&Key::segments(&name, cursor)?)?;
		}

		let entries: Vec<_> = shared.store.list(&query).try_collect().await?;
		let mut segments: Vec<u64> = entries
			.into_iter()
			.filter_map(|entry| match entry.key {
				Key::Segments { segment, .. } => Some(segment),
				_ => None,
			})
			.filter(|segment| self.cursor.is_none_or(|cursor| *segment > cursor))
			.collect();
		segments.sort_unstable();
		segments.dedup();

		for segment in segments {
			match shared
				.store
				.get_segments(&name, segment, self.timescale.is_some())
				.await
			{
				Ok(object) => self.replay(shared, segment, &object)?,
				Err(Error::Store(err)) => return Err(Error::Store(err)),
				Err(err) => {
					tracing::warn!(timeline = name, segment, %err, "skipping unreadable timeline segment");
					continue;
				}
			}
			self.cursor = Some(segment);
		}

		Ok(())
	}

	/// Index one timeline object's groups, then republish each on the timeline track.
	fn replay<T>(&mut self, shared: &Shared<T>, segment: u64, object: &Object) -> Result<()> {
		let timed = self.timescale.is_some();
		for stored in &object.groups {
			let mut group = self.decoder.group();
			let decoded = stored.frames.iter().try_for_each(|frame| group.decode(&frame.payload));

			// Apply whatever decoded, even from a group that failed partway: the next group's
			// checkpoint restates the window regardless.
			let mut index = shared.index.lock().unwrap();
			while let Some(event) = group.next_event() {
				match event {
					window::Event::Push { value, .. }
						if value.pts.is_some() != timed || value.duration.is_some() != timed =>
					{
						tracing::warn!(
							track = self.track,
							sequence = value.sequence,
							"ignoring a record whose timing contradicts its timeline"
						);
					}
					window::Event::Push { index: at, value } if at == value.sequence => index.push(&self.track, &value),
					window::Event::Push { index: at, value } => {
						tracing::warn!(
							track = self.track,
							at,
							sequence = value.sequence,
							"ignoring a misnumbered record"
						);
					}
					window::Event::Pop(range) | window::Event::Skip(range) => {
						for span in index.pop(&self.track, range) {
							if let Ok(key) = Key::segments(self.track.clone(), span.sequence) {
								shared.cache.remove(&key);
							}
						}
					}
					_ => {}
				}
			}
			drop(index);
			shared.changed.send_modify(|_| {});

			if let Err(err) = decoded {
				tracing::warn!(group = stored.sequence, %err, "skipping malformed timeline group");
				continue;
			}

			shared
				.timelines
				.lock()
				.unwrap()
				.entry(self.producer.name().to_string())
				.or_default()
				.insert(stored.sequence, segment);

			let mut producer = match self.producer.create_group(stored.sequence.into()) {
				Ok(producer) => producer,
				Err(moq_net::Error::Duplicate) => continue,
				Err(err) => return Err(err.into()),
			};
			for frame in &stored.frames {
				producer.write_frame(timestamp(frame.timestamp, self.timescale)?, frame.payload.clone())?;
			}
			producer.finish()?;
		}
		Ok(())
	}
}

/// Serve requested tracks until the broadcast closes.
async fn serve<T: ObjectStore>(shared: Arc<Shared<T>>, mut dynamic: broadcast::Dynamic) {
	let mut serving = kio::Tasks::new();
	kio::wait(|waiter| {
		while let Poll::Ready(Ok(requests)) = shared.requests.poll_pop(waiter) {
			serving.push(task(Box::pin(serve_timeline(shared.clone(), requests))));
		}
		loop {
			match dynamic.poll_requested_track(waiter) {
				Poll::Ready(Ok(request)) => {
					let known = shared.served.lock().unwrap().contains(request.name());
					serving.push(task(Box::pin(serve_track(shared.clone(), request, known))))
				}
				Poll::Ready(Err(_)) => return Poll::Ready(()),
				Poll::Pending => break,
			}
		}
		let _ = serving.poll(waiter);
		Poll::Pending
	})
	.await
}

/// Serve a timeline's evicted groups from their original stored objects.
async fn serve_timeline<T: ObjectStore>(shared: Arc<Shared<T>>, requests: TimelineRequests) {
	let mut groups = kio::Tasks::new();
	kio::wait(|waiter| {
		loop {
			match requests.dynamic.poll_requested_group(waiter) {
				Poll::Ready(Ok(request)) => {
					groups.push(task(Box::pin(serve_timeline_group(
						shared.clone(),
						TimelineRequest {
							name: requests.name.clone(),
							timescale: requests.timescale,
							request,
						},
					))));
				}
				Poll::Ready(Err(_)) => return Poll::Ready(()),
				Poll::Pending => break,
			}
		}
		let _ = groups.poll(waiter);
		Poll::Pending
	})
	.await;
}

async fn serve_timeline_group<T: ObjectStore>(shared: Arc<Shared<T>>, request: TimelineRequest) {
	let TimelineRequest {
		name,
		timescale,
		request,
	} = request;
	let sequence = request.sequence();
	let segment = shared
		.timelines
		.lock()
		.unwrap()
		.get(&name)
		.and_then(|groups| groups.get(&sequence))
		.copied();
	let Some(segment) = segment else {
		request.reject(moq_net::Error::NotFound);
		return;
	};
	let key = match Key::segments(&name, segment) {
		Ok(key) => key,
		Err(_) => {
			request.reject(moq_net::Error::NotFound);
			return;
		}
	};
	let object = shared
		.cache
		.get_or_insert_async(&key, async {
			shared
				.store
				.get_segments(&name, segment, timescale.is_some())
				.await
				.map(Arc::new)
		})
		.await;
	let object = match object {
		Ok(object) => object,
		Err(err) => {
			tracing::warn!(track = name, sequence, %err, "archived timeline group is unavailable");
			request.reject(moq_net::Error::NotFound);
			return;
		}
	};
	let start = request.frame_start();
	let Some(stored) = object.groups.iter().find(|group| group.sequence == sequence) else {
		request.reject(moq_net::Error::NotFound);
		return;
	};
	if object.frame_start != 0 {
		request.reject(moq_net::Error::NotFound);
		return;
	}
	let Ok(mut producer) = request.accept(None) else {
		return;
	};
	if producer.start_at(start).is_err() {
		return;
	}
	for frame in stored.frames.iter().skip(start as usize) {
		let Ok(timestamp) = timestamp(frame.timestamp, timescale) else {
			let _ = producer.abort(moq_net::Error::NotFound);
			return;
		};
		if producer.write_frame(timestamp, frame.payload.clone()).is_err() {
			return;
		}
	}
	let _ = producer.finish();
}

/// Accept a track a timeline indexes, then serve its group requests until nobody uses it.
async fn serve_track<T: ObjectStore>(shared: Arc<Shared<T>>, request: track::Request, known: bool) {
	let name = request.name().to_string();
	if !known {
		request.reject(moq_net::Error::NotFound);
		return;
	}

	let cached = shared.infos.lock().unwrap().get(&name).cloned();
	let info = match cached {
		Some(info) => info,
		None => match shared.store.get_info(&name).await.and_then(|info| track_info(&info)) {
			Ok(info) => {
				shared.infos.lock().unwrap().insert(name.clone(), info.clone());
				info
			}
			Err(err) => {
				tracing::warn!(track = name, %err, "archived track has no usable .info");
				request.reject(moq_net::Error::NotFound);
				return;
			}
		},
	};

	let timescale = info.timescale;
	let dynamic = request.dynamic();
	let demand = dynamic.demand();
	let _producer = request.accept(info);
	let mut groups = kio::Tasks::new();

	kio::wait(|waiter| {
		loop {
			match dynamic.poll_requested_group(waiter) {
				Poll::Ready(Ok(request)) => {
					let serve = serve_group(shared.clone(), name.clone(), timescale, request);
					groups.push(task(Box::pin(serve)));
				}
				Poll::Ready(Err(_)) => return Poll::Ready(()),
				Poll::Pending => break,
			}
		}
		match groups.poll(waiter).is_ready() && demand.poll_unused(waiter).is_ready() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	})
	.await
}

/// Answer one group request from the records holding it, or reject it as never delivered.
///
/// A group whose records stop short of its end keeps its producer open and grows as later records
/// commit, until one reaches the group's end or the caller finishes the reader.
async fn serve_group<T: ObjectStore>(
	shared: Arc<Shared<T>>,
	track: String,
	timescale: Option<Timescale>,
	request: group::Request,
) {
	let sequence = request.sequence();
	let start = request.frame_start();
	let mut changed = shared.changed.subscribe();
	let mut spans = shared.index.lock().unwrap().group(&track, sequence);

	// The earliest retained record must hold the requested frame; an expired head is gone.
	if spans
		.first()
		.is_none_or(|span| span.start > Position::new(sequence, start))
	{
		request.reject(moq_net::Error::NotFound);
		return;
	}
	// Validate the first object before accepting, so an unreadable group is a plain miss. Keep it,
	// so a cache too small to hold it costs no second GET.
	let timed = timescale.is_some();
	let mut first = match object(&shared, &track, timed, &spans[0]).await {
		Ok(object) => Some((spans[0].sequence, object)),
		Err(err) => {
			tracing::warn!(track, sequence, %err, "archived group is unavailable");
			request.reject(moq_net::Error::NotFound);
			return;
		}
	};
	let Ok(mut producer) = request.accept(None) else {
		return;
	};
	if producer.start_at(start).is_err() {
		return;
	}

	let mut next = start;
	loop {
		for span in &spans {
			if span.end <= Position::new(sequence, next) {
				continue;
			}
			let loaded = match first.take_if(|(loaded, _)| *loaded == span.sequence) {
				Some((_, object)) => Ok(object),
				None => object(&shared, &track, timed, span).await,
			};
			let object = match loaded {
				Ok(object) if span.start <= Position::new(sequence, next) => object,
				Ok(_) => {
					let _ = producer.abort(moq_net::Error::NotFound);
					return;
				}
				Err(err) => {
					tracing::warn!(track, sequence, %err, "archived group is unavailable");
					let _ = producer.abort(moq_net::Error::NotFound);
					return;
				}
			};
			// The span was validated against its object, so the group is present.
			let Some(stored) = object.groups.iter().find(|group| group.sequence == sequence) else {
				continue;
			};
			let first = match object.groups[0].sequence == sequence {
				true => object.frame_start,
				false => 0,
			};
			for (index, frame) in (first..).zip(&stored.frames) {
				if index < next {
					continue;
				}
				let Ok(timestamp) = timestamp(frame.timestamp, timescale) else {
					let _ = producer.abort(moq_net::Error::TimestampMismatch);
					return;
				};
				if let Err(err) = producer.write_frame(timestamp, frame.payload.clone()) {
					let _ = producer.abort(err);
					return;
				}
				next = index + 1;
			}
		}

		let complete = spans.last().is_some_and(|span| span.end.group > sequence);
		if complete {
			let _ = producer.finish();
			return;
		}
		// A change that landed while objects loaded is unseen, so the spans above predate it:
		// reload before finishing or waiting, or its records would never be served.
		if !changed.has_changed().unwrap_or(false) {
			// The caller finished the recording, and these spans are its final index.
			if *changed.borrow() {
				let _ = producer.finish();
				return;
			}
			if changed.changed().await.is_err() {
				let _ = producer.finish();
				return;
			}
		}
		changed.mark_unchanged();
		spans = shared.index.lock().unwrap().group(&track, sequence);
	}
}

/// A span's object, from the cache or one GET validated against the span.
async fn object<T: ObjectStore>(shared: &Shared<T>, track: &str, timed: bool, span: &Span) -> Result<Arc<Object>> {
	let key = Key::segments(track.to_string(), span.sequence)?;
	shared
		.cache
		.get_or_insert_async(&key, async {
			let object = shared.store.get_segments(track, span.sequence, timed).await?;
			object.check_span(span.start, span.end)?;
			Ok(Arc::new(object))
		})
		.await
}

fn track_info(info: &crate::Info) -> Result<track::Info> {
	let mut track = track::Info::default().with_priority(info.priority);
	if let Some(timescale) = info.timescale {
		track = track.with_timescale(Timescale::new(timescale).map_err(|_| Error::Timescale(timescale))?);
	}
	Ok(track)
}

/// A stored timestamp on its track, which must be timed exactly when the frame is.
fn timestamp(value: Option<u64>, timescale: Option<Timescale>) -> Result<Option<Timestamp>> {
	match (value, timescale) {
		(Some(value), Some(timescale)) => Ok(Some(Timestamp::new(value, timescale).map_err(|_| Error::Id(value))?)),
		(None, None) => Ok(None),
		_ => Err(Error::Timing),
	}
}

/// Adapt a boxed future into a [`kio::Tasks`] task.
fn task(mut future: BoxFuture<'static, ()>) -> impl FnMut(&kio::Waiter) -> Poll<()> + Send {
	move |waiter| waiter.poll_future(future.as_mut())
}

/// Weighs a cached object by its payload bytes plus a small per-frame overhead.
#[derive(Clone)]
struct Weight;

impl quick_cache::Weighter<Key, Arc<Object>> for Weight {
	fn weight(&self, _key: &Key, object: &Arc<Object>) -> u64 {
		let frames = object.groups.iter().flat_map(|group| &group.frames);
		frames.map(|frame| frame.payload.len() as u64 + 32).sum::<u64>() + 64
	}
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)]
mod tests;
