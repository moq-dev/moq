//! Serve an archive's groups through a caller-supplied broadcast.
//!
//! A [`Reader`] replays the archive timeline onto its timeline track and answers FETCH for every
//! group that timeline commits. Each request maps to one range-named object, fetched with a single
//! GET, validated against the record that advertised it, and kept in a byte-bounded cache so
//! adjacent groups reuse it. A request for one track never downloads another track's object.
//!
//! The reader never infers that a recording ended: it keeps following until the caller, who may
//! know finality out of band, calls [`Reader::finish`].
//!
//! ```no_run
//! # async fn example(store: moq_archive::Store<moq_archive::object_store::memory::InMemory>) -> moq_archive::Result<()> {
//! use moq_archive::reader::{Config, Reader};
//!
//! let broadcast = moq_net::broadcast::Info::new().produce();
//! let mut reader = Reader::open(store, &broadcast, Config::new("timeline.z")).await?;
//! let serve = reader.serve();
//! // Spawn `serve` on any executor, then call `reader.refresh()` to follow a growing archive.
//! # drop(serve);
//! # Ok(())
//! # }
//! ```

mod index;

use std::future::Future;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use futures::TryStreamExt;
use futures::future::BoxFuture;
use hang::timeline::Record;
use moq_json::window;
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::ObjectStore;

use self::index::{Index, Span};
use crate::store::list::Query;
use crate::{Error, Key, Object, Result, Store};

/// Configuration for [`Reader::open`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// The timeline track name, from the catalog's `archive` entry.
	pub timeline: String,
	/// Upper bound on cached object bytes shared by every track.
	pub cache: u64,
}

impl Config {
	/// Read the timeline named `timeline` with a 64 MiB object cache.
	pub fn new(timeline: impl Into<String>) -> Self {
		Self {
			timeline: timeline.into(),
			cache: 64 * 1024 * 1024,
		}
	}

	/// Set [`cache`](Self::cache).
	pub fn with_cache(mut self, bytes: u64) -> Self {
		self.cache = bytes;
		self
	}
}

/// Serves an archive through a [`broadcast::Producer`] and follows its timeline.
pub struct Reader<T> {
	shared: Arc<Shared<T>>,
	dynamic: broadcast::Dynamic,
	timeline: track::Producer,
	decoder: window::Decoder<Record>,
	/// The last timeline segment replayed.
	cursor: Option<u64>,
}

struct Shared<T> {
	store: Store<T>,
	index: Mutex<Index>,
	cache: quick_cache::sync::Cache<Key, Arc<Object>, Weight>,
}

impl<T: ObjectStore> Reader<T> {
	/// Create the timeline track on `broadcast`, then replay every retained timeline object.
	///
	/// Fails if the timeline's `.info` is missing or invalid, the timeline track already exists,
	/// or the store cannot list the timeline.
	pub async fn open(store: Store<T>, broadcast: &broadcast::Producer, config: Config) -> Result<Self> {
		let info = track_info(&store.get_info(&config.timeline).await?)?;
		let timeline = broadcast.create_track(config.timeline, info)?;

		let items = usize::try_from(config.cache / (1024 * 1024))
			.unwrap_or(usize::MAX)
			.max(16);
		let shared = Arc::new(Shared {
			store,
			index: Mutex::default(),
			cache: quick_cache::sync::Cache::with_weighter(items, config.cache, Weight),
		});

		let mut reader = Self {
			shared,
			dynamic: broadcast.dynamic(),
			timeline,
			decoder: window::Decoder::new(window::ConsumerConfig::default().with_compression(true)),
			cursor: None,
		};
		reader.refresh().await?;
		Ok(reader)
	}

	/// Replay timeline objects committed since the last refresh.
	///
	/// Lists timeline keys after the last replayed segment, then GETs them in segment order. Each
	/// stored group opens with a window checkpoint, so a segment that expired or is malformed is
	/// skipped and the next one recovers the retained window, evicting whatever it popped. A
	/// malformed newest object is retried on the next refresh.
	pub async fn refresh(&mut self) -> Result<()> {
		let timeline = self.timeline.name().to_string();
		let mut query = Query::segments(&timeline)?;
		if let Some(cursor) = self.cursor {
			query = query.after(&Key::segments(&timeline, cursor)?)?;
		}

		let entries: Vec<_> = self.shared.store.list(&query).try_collect().await?;
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
			match self.shared.store.get_segments(&timeline, segment).await {
				Ok(object) => self.replay(&object)?,
				Err(Error::Store(err)) => return Err(Error::Store(err)),
				Err(err) => {
					tracing::warn!(segment, %err, "skipping unreadable timeline segment");
					continue;
				}
			}
			self.cursor = Some(segment);
		}

		Ok(())
	}

	/// Finish the replayed timeline track once the caller knows the recording ended.
	///
	/// Following stops; groups already committed stay servable through [`serve`](Self::serve).
	pub fn finish(self) -> Result<()> {
		Ok(self.timeline.finish()?)
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

	/// Index one timeline object's groups, then republish each on the timeline track.
	fn replay(&mut self, object: &Object) -> Result<()> {
		for stored in &object.groups {
			let mut group = self.decoder.group();
			let decoded = stored.frames.iter().try_for_each(|frame| group.decode(&frame.payload));

			// Apply whatever decoded, even from a group that failed partway: the next group's
			// checkpoint restates the window regardless.
			let mut index = self.shared.index.lock().unwrap();
			while let Some(event) = group.next_event() {
				match event {
					window::Event::Push { index: at, value } => index.push(at, &value),
					window::Event::Pop(range) => {
						for (track, span) in index.pop(range) {
							if let Ok(key) = Key::groups(track, span.bounds) {
								self.shared.cache.remove(&key);
							}
						}
					}
					_ => {}
				}
			}
			drop(index);

			if let Err(err) = decoded {
				tracing::warn!(group = stored.sequence, %err, "skipping malformed timeline group");
				continue;
			}

			let mut producer = match self.timeline.create_group(stored.sequence.into()) {
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
async fn serve<T: ObjectStore>(shared: Arc<Shared<T>>, mut dynamic: broadcast::Dynamic) {
	let mut tracks = kio::Tasks::new();
	kio::wait(|waiter| {
		loop {
			match dynamic.poll_requested_track(waiter) {
				Poll::Ready(Ok(request)) => tracks.push(task(Box::pin(serve_track(shared.clone(), request)))),
				Poll::Ready(Err(_)) => return Poll::Ready(()),
				Poll::Pending => break,
			}
		}
		let _ = tracks.poll(waiter);
		Poll::Pending
	})
	.await
}

/// Accept a track the timeline names, then serve its group requests until nobody uses it.
async fn serve_track<T: ObjectStore>(shared: Arc<Shared<T>>, request: track::Request) {
	let name = request.name().to_string();
	if !shared.index.lock().unwrap().has_track(&name) {
		request.reject(moq_net::Error::NotFound);
		return;
	}

	let info = match shared.store.get_info(&name).await.and_then(|info| track_info(&info)) {
		Ok(info) => info,
		Err(err) => {
			tracing::warn!(track = name, %err, "archived track has no usable .info");
			request.reject(moq_net::Error::NotFound);
			return;
		}
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

/// Answer one group request from its range-named object, or reject it as never delivered.
async fn serve_group<T: ObjectStore>(
	shared: Arc<Shared<T>>,
	track: String,
	timescale: Timescale,
	request: group::Request,
) {
	let sequence = request.sequence();
	let span = shared.index.lock().unwrap().get(&track, sequence);
	let Some(span) = span else {
		request.reject(moq_net::Error::NotFound);
		return;
	};

	let object = match Key::groups(track.clone(), span.bounds.clone()) {
		Ok(key) => {
			shared
				.cache
				.get_or_insert_async(&key, load(&shared.store, &track, &span))
				.await
		}
		Err(err) => Err(err),
	};
	let object = match object {
		Ok(object) => object,
		Err(err) => {
			tracing::warn!(track, sequence, %err, "archived group is unavailable");
			request.reject(moq_net::Error::NotFound);
			return;
		}
	};

	// Load validated the table against the span, so every advertised group is present.
	let Ok(position) = object.groups.binary_search_by_key(&sequence, |group| group.sequence) else {
		request.reject(moq_net::Error::NotFound);
		return;
	};
	let frames = &object.groups[position].frames;
	let Some(frames) = usize::try_from(request.frame_start())
		.ok()
		.and_then(|start| frames.get(start..))
	else {
		request.reject(moq_net::Error::NotFound);
		return;
	};

	let start = request.frame_start();
	let Ok(mut producer) = request.accept(None) else {
		return;
	};
	let written = producer.start_at(start).and_then(|()| {
		frames.iter().try_for_each(|frame| {
			let timestamp = timestamp(frame.timestamp, timescale).map_err(|_| moq_net::Error::TimestampMismatch)?;
			producer.write_frame(timestamp, frame.payload.clone())
		})
	});
	match written {
		Ok(()) => {
			let _ = producer.finish();
		}
		Err(err) => {
			let _ = producer.abort(err);
		}
	}
}

/// GET a span's object and require its table to hold exactly the advertised groups.
async fn load<T: ObjectStore>(store: &Store<T>, track: &str, span: &Span) -> Result<Arc<Object>> {
	let object = store.get_groups(track, span.bounds.clone()).await?;
	check_runs(&object, &span.runs)?;
	Ok(Arc::new(object))
}

/// Refuse a table whose sequences differ from the advertised runs.
fn check_runs(object: &Object, runs: &[RangeInclusive<u64>]) -> Result<()> {
	let mut groups = object.groups.iter();
	for run in runs {
		for sequence in run.clone() {
			if groups.next().map(|group| group.sequence) != Some(sequence) {
				return Err(Error::Table);
			}
		}
	}
	match groups.next() {
		Some(_) => Err(Error::Table),
		None => Ok(()),
	}
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
