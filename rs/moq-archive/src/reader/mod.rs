//! Serve an archive's groups through a caller-supplied broadcast.
//!
//! A [`Reader`] replays each track's timeline onto its timeline track and answers FETCH for every
//! frame those timelines commit. A group maps to the records holding it, each one object fetched
//! with a single GET, validated against its record, and kept in a byte-bounded cache so adjacent
//! groups reuse it. A request for one track never downloads another track's object. A group whose
//! records do not reach its end yet, such as an append-only log, grows as later records commit.
//!
//! The reader never infers that a recording ended: it keeps following until the caller, who may
//! know finality out of band, calls [`Reader::finish`].
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

use std::collections::{BTreeMap, HashMap};
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
	dynamic: broadcast::Dynamic,
	timelines: Vec<Timeline>,
}

/// One track's timeline, replayed onto its own timeline track.
struct Timeline {
	/// The track it indexes.
	track: String,
	producer: track::Producer,
	decoder: window::Decoder<Record>,
	/// The last timeline segment replayed.
	cursor: Option<u64>,
}

struct Shared<T> {
	store: Store<T>,
	index: Mutex<Index>,
	cache: quick_cache::sync::Cache<Key, Arc<Object>, Weight>,
	/// Each track's `.info`, which is immutable, so a re-requested track costs no GET.
	infos: Mutex<HashMap<String, track::Info>>,
	/// Notified whenever the index changes; holds whether the caller declared the recording ended.
	changed: watch::Sender<bool>,
}

impl<T: ObjectStore> Reader<T> {
	/// Create every timeline track on `broadcast`, then replay each track's retained timeline.
	///
	/// Fails if a timeline's `.info` is missing or invalid, a timeline track already exists, or the
	/// store cannot list a timeline.
	pub async fn open(store: Store<T>, broadcast: &broadcast::Producer, config: Config) -> Result<Self> {
		let mut timelines = Vec::new();
		for (track, timeline) in config.timelines {
			let info = track_info(&store.get_info(&timeline).await?)?;
			timelines.push(Timeline {
				track,
				producer: broadcast.create_track(timeline, info)?,
				decoder: window::Decoder::new(window::ConsumerConfig::default().with_compression(true)),
				cursor: None,
			});
		}

		let items = usize::try_from(config.cache / (1024 * 1024))
			.unwrap_or(usize::MAX)
			.max(16);
		let shared = Arc::new(Shared {
			store,
			index: Mutex::default(),
			cache: quick_cache::sync::Cache::with_weighter(items, config.cache, Weight),
			infos: Mutex::default(),
			changed: watch::Sender::new(false),
		});

		let mut reader = Self {
			shared,
			dynamic: broadcast.dynamic(),
			timelines,
		};
		reader.refresh().await?;
		Ok(reader)
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
		let tracks = self.timelines.iter().map(|timeline| timeline.track.clone()).collect();
		serve(self.shared.clone(), self.dynamic.clone(), tracks)
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
			match shared.store.get_segments(&name, segment).await {
				Ok(object) => self.replay(shared, &object)?,
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
	fn replay<T>(&mut self, shared: &Shared<T>, object: &Object) -> Result<()> {
		for stored in &object.groups {
			let mut group = self.decoder.group();
			let decoded = stored.frames.iter().try_for_each(|frame| group.decode(&frame.payload));

			// Apply whatever decoded, even from a group that failed partway: the next group's
			// checkpoint restates the window regardless.
			let mut index = shared.index.lock().unwrap();
			while let Some(event) = group.next_event() {
				match event {
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

			let mut producer = match self.producer.create_group(stored.sequence.into()) {
				Ok(producer) => producer,
				Err(moq_net::Error::Duplicate) => continue,
				Err(err) => return Err(err.into()),
			};
			let timescale = producer.timescale();
			for frame in &stored.frames {
				producer.write_frame(timestamp(frame.timestamp, timescale)?, frame.payload.clone())?;
			}
			producer.finish()?;
		}
		Ok(())
	}
}

/// Serve requested tracks until the broadcast closes.
async fn serve<T: ObjectStore>(shared: Arc<Shared<T>>, mut dynamic: broadcast::Dynamic, tracks: Vec<String>) {
	let mut serving = kio::Tasks::new();
	kio::wait(|waiter| {
		loop {
			match dynamic.poll_requested_track(waiter) {
				Poll::Ready(Ok(request)) => {
					let known = tracks.iter().any(|track| track == request.name());
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
		match groups.poll(waiter).is_ready() && dynamic.poll_unused(waiter).is_ready() {
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
	timescale: Timescale,
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
	let mut first = match object(&shared, &track, &spans[0]).await {
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
				None => object(&shared, &track, span).await,
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
		if complete || *changed.borrow_and_update() {
			let _ = producer.finish();
			return;
		}
		// The reader dropping ends the recording as surely as finishing it.
		if changed.changed().await.is_err() {
			let _ = producer.finish();
			return;
		}
		spans = shared.index.lock().unwrap().group(&track, sequence);
	}
}

/// A span's object, from the cache or one GET validated against the span.
async fn object<T: ObjectStore>(shared: &Shared<T>, track: &str, span: &Span) -> Result<Arc<Object>> {
	let key = Key::segments(track.to_string(), span.sequence)?;
	shared
		.cache
		.get_or_insert_async(&key, async {
			let object = shared.store.get_segments(track, span.sequence).await?;
			object.check_span(span.start, span.end)?;
			Ok(Arc::new(object))
		})
		.await
}

fn track_info(info: &crate::Info) -> Result<track::Info> {
	let timescale = Timescale::new(info.timescale).map_err(|_| Error::Timescale(info.timescale))?;
	Ok(track::Info::default()
		.with_timescale(timescale)
		.with_priority(info.priority))
}

fn timestamp(value: u64, timescale: Timescale) -> Result<Timestamp> {
	Timestamp::new(value, timescale).map_err(|_| Error::Id(value))
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
mod tests;
