use std::ffi::c_char;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, State, moq_announce_event, moq_announce_kind, moq_string};

/// Global state managing all active resources.
///
/// Stores all sessions, origins, broadcasts, tracks, and frames in slab allocators,
/// returning opaque IDs to C callers. Also manages async tasks via oneshot channels
/// for cancellation.
// TODO split this up into separate structs/mutexes
#[derive(Default)]
pub struct Origin {
	/// Active origin producers for publishing and consuming broadcasts.
	active: NonZeroSlab<Producer>,

	/// Delivered announce events, freed by the caller.
	announced: NonZeroSlab<AnnouncedRecord>,

	/// Announcement listener tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	announced_task: NonZeroSlab<Task>,

	/// Pending consume-until-announced tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	consume_task: NonZeroSlab<Task>,

	/// Served routes from [Self::dynamic], retracted when the handle is closed.
	dynamic: NonZeroSlab<Option<DynamicEntry>>,

	/// Broadcast requests delivered to a dynamic handler, freed after accept/reject.
	broadcast_request: NonZeroSlab<Option<moq_net::origin::Request>>,
}

#[derive(Clone)]
pub(crate) struct Producer {
	pub producer: moq_net::origin::Producer,
	pub driver: crate::ffi::Background,
}

impl std::ops::Deref for Producer {
	type Target = moq_net::origin::Producer;
	fn deref(&self) -> &Self::Target {
		&self.producer
	}
}

#[derive(Clone)]
pub(crate) struct Consumer {
	pub inner: moq_net::origin::Consumer,
	pub driver: crate::ffi::Background,
}

impl std::ops::Deref for Consumer {
	type Target = moq_net::origin::Consumer;
	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl Producer {
	fn consume_owned(&self) -> Consumer {
		Consumer {
			inner: self.producer.consume(),
			driver: self.driver.clone(),
		}
	}
}

/// One announce event and the C string views borrowed from it.
struct AnnouncedRecord {
	prefix: String,
	captures: Option<Vec<String>>,
	capture_views: Vec<moq_string>,
	kind: moq_announce_kind,
}

// The raw pointers only borrow immutable String allocations owned by this record.
// Moving the record does not move those allocations, and the record is never mutated.
unsafe impl Send for AnnouncedRecord {}

impl AnnouncedRecord {
	fn new(event: moq_net::announce::Event) -> Self {
		use moq_net::announce::Event;
		let (update, kind) = match event {
			Event::Start(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_START),
			Event::Update(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_UPDATE),
			Event::End(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_END),
			Event::Restart(update) => (update, moq_announce_kind::MOQ_ANNOUNCE_KIND_RESTART),
		};
		let captures = update.captures.map(|captures| {
			captures
				.into_iter()
				.map(|capture| capture.to_string())
				.collect::<Vec<_>>()
		});
		let capture_views = captures
			.as_deref()
			.unwrap_or_default()
			.iter()
			.map(|capture| moq_string {
				data: capture.as_ptr().cast(),
				len: capture.len(),
			})
			.collect();

		Self {
			prefix: update.prefix.to_string(),
			captures,
			capture_views,
			kind,
		}
	}
}

struct DynamicEntry {
	inner: Option<moq_net::origin::Dynamic>,
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

impl Origin {
	pub fn create(&mut self) -> Result<Id, Error> {
		// Every FFI entry point runs inside `RUNTIME.enter()`, so the driver
		// lands on the dedicated moq-c runtime.
		let (producer, driver) = moq_net::origin::Producer::new(Default::default());
		self.active.insert(Producer {
			producer,
			driver: crate::ffi::Background::driver(driver),
		})
	}

	pub fn get(&self, id: Id) -> Result<&Producer, Error> {
		let origin = self.active.get(id).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		Ok(origin)
	}

	pub fn announced(
		&mut self,
		origin: Id,
		scope: moq_net::Pattern,
		hidden: bool,
		on_announce: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let consumer = origin
			.producer
			.consume()
			.scope("", &moq_net::Patterns::from(scope))?
			.with_hidden(hidden)
			.announced();
		self.announced_task.spawn(
			on_announce,
			|state| &mut state.origin.announced_task,
			|close| async move { driver.watch(Self::run_announced(on_announce, consumer, close)).await },
		)
	}

	async fn run_announced(
		callback: OnStatus,
		mut consumer: moq_net::announce::Consumer,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready announcement.
			let event = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = consumer.next() => match next {
					Some(event) => event,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the event; release it before the callback.
			let announced_id = State::lock().origin.announced.insert(AnnouncedRecord::new(event))?;
			callback.call(announced_id);
		}
	}

	pub fn announced_info(&self, announced: Id, dst: &mut moq_announce_event) -> Result<(), Error> {
		let announced = self.announced.get(announced).ok_or(Error::AnnouncementNotFound)?;
		*dst = moq_announce_event {
			prefix: announced.prefix.as_ptr().cast::<c_char>(),
			prefix_len: announced.prefix.len(),
			captures: announced.capture_views.as_ptr(),
			captures_len: announced.capture_views.len(),
			has_captures: announced.captures.is_some(),
			kind: announced.kind,
		};
		Ok(())
	}

	/// Free a single announcement record delivered to an `on_announce` callback.
	///
	/// Each announce event allocates a record (read via [`Self::announced_info`]);
	/// the caller releases it here once done. This is per-record, distinct from
	/// [`Self::announced_close`], which stops the whole listener. Records are freed explicitly
	/// rather than on unannounce: an unannounce is its own delivered record, and auto-freeing
	/// the prior one would race a caller still reading it.
	pub fn announced_free(&mut self, announced: Id) -> Result<(), Error> {
		self.announced.remove(announced).ok_or(Error::AnnouncementNotFound)?;
		Ok(())
	}

	pub fn announced_close(&mut self, announced: Id) -> Result<(), Error> {
		self.announced_task.close(announced, Error::AnnouncementNotFound)
	}

	/// Follow the broadcast at `path`, delivering each publisher instance's handle via the callback.
	///
	/// The callback fires a broadcast handle (> 0) for each instance serving the path, until a
	/// single terminal code (`0` on close, negative on error). Returns a task handle for
	/// cancellation via [`Self::consume_announced_close`].
	pub fn consume_announced(&mut self, origin: Id, path: String, on_broadcast: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let consumer = origin.consume_owned();
		self.consume_task.spawn(
			on_broadcast,
			|state| &mut state.origin.consume_task,
			|close| async move {
				driver
					.watch(Self::run_consume_announced(on_broadcast, consumer, path, close))
					.await
			},
		)
	}

	async fn run_consume_announced(
		callback: OnStatus,
		consumer: Consumer,
		path: String,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// An unreachable path and a closed origin both mean no broadcast can ever arrive here.
		let mut follow = match consumer.follow(path.as_str()) {
			Ok(follow) => follow,
			Err(moq_net::Error::Unauthorized) => return Err(Error::BroadcastNotFound),
			Err(err) => return Err(err.into()),
		};
		let mut unseen = false;
		let mut delivered = false;
		// The handle delivered last, until the caller frees it or the path goes offline. An id
		// rather than the broadcast, so it never keeps an instance the caller freed from retiring.
		let mut last: Option<Id> = None;
		// The origin closing ends the wait, cleanly once a handle was delivered.
		let closed = |delivered: bool| match delivered {
			true => Ok(()),
			false => Err(Error::BroadcastNotFound),
		};

		// Deliver each publisher instance serving the path: the first, then each one that takes
		// over or returns. `biased` so a pending close always wins.
		loop {
			// Apply every change on hand before resolving, so a burst of them delivers only the
			// instance serving the path now.
			if settle(&mut follow, &mut unseen, &mut last).is_none() {
				return closed(delivered);
			}
			if !unseen {
				let event = tokio::select! {
					biased;
					_ = &mut close => return Ok(()),
					event = follow.next() => event,
				};
				let Some(event) = event else {
					return closed(delivered);
				};
				apply(&mut unseen, &mut last, event);
				continue;
			}

			// `routed_broadcast` rides out the churn between something covering the path
			// and the path actually resolving (failover, an advertise-only announce
			// racing its handler).
			let broadcast = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				resolved = consumer.routed_broadcast(path.as_str()) => match resolved {
					Ok(broadcast) => broadcast,
					Err(moq_net::Error::Closed) => return closed(delivered),
					Err(moq_net::Error::Unauthorized) => return Err(Error::BroadcastNotFound),
					Err(err) => return Err(err.into()),
				},
			};

			// A change that came through while resolving may predate the answer, such as an end
			// and a start for the instance it resolved. Resolve afresh once caught up.
			match settle(&mut follow, &mut unseen, &mut last) {
				None => return closed(delivered),
				Some(true) => continue,
				Some(false) => unseen = false,
			}

			// Hold the lock only to buffer the broadcast; release it before the callback.
			let broadcast_id = {
				let mut state = State::lock();
				// Resolving can run ahead of the follower: a takeover resolves at once while its
				// restart is held, and that restart then names the instance already delivered.
				if last.is_some_and(|last| state.consume.holds(last, &broadcast)) {
					continue;
				}
				state.consume.start(broadcast, Some(consumer.clone()))?
			};
			last = Some(broadcast_id);
			delivered = true;
			callback.call(broadcast_id);
		}
	}

	/// Request the broadcast at `path`, delivering its handle once it can be served.
	///
	/// Unlike [`Self::consume`] (announced-only, fails fast) and [`Self::consume_announced`]
	/// (waits indefinitely for a future announcement), this resolves against any route
	/// announced now: the callback fires the broadcast
	/// handle (> 0) once served, then a terminal `0`; or a single terminal code (`0` on close,
	/// negative on error) if it can't be served. Returns a task handle for cancellation.
	pub fn request(&mut self, origin: Id, path: String, on_broadcast: OnStatus) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let consumer = origin.consume_owned();
		self.consume_task.spawn(
			on_broadcast,
			|state| &mut state.origin.consume_task,
			|close| async move {
				driver
					.watch(Self::run_request(on_broadcast, consumer, path, close))
					.await
			},
		)
	}

	async fn run_request(
		callback: OnStatus,
		consumer: Consumer,
		path: String,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Resolves to an error when no announced route can serve the path.
		let pending = consumer.request_broadcast(path.as_str(), None);

		// `biased` so a pending close always wins over a ready broadcast.
		let broadcast = tokio::select! {
			biased;
			_ = &mut close => return Ok(()),
			res = pending => res?,
		};

		// Hold the lock only to buffer the broadcast; release it before the callback.
		let broadcast_id = State::lock().consume.start(broadcast, Some(consumer))?;
		callback.call(broadcast_id);
		Ok(())
	}

	pub fn consume_announced_close(&mut self, task: Id) -> Result<(), Error> {
		self.consume_task.close(task, Error::NotFound)
	}

	/// Create an unannounced broadcast at `path` on an origin.
	///
	/// Errors with [`Error::Moq`] if the path is outside the origin's scope.
	pub fn create_broadcast<P: moq_net::AsPath>(
		&self,
		origin: Id,
		path: P,
	) -> Result<(moq_net::broadcast::Producer, crate::ffi::Background), Error> {
		let origin = self.get(origin)?;
		let result = origin.producer.create_broadcast(path);
		origin.driver.check()?;
		Ok((result?, origin.driver.clone()))
	}

	/// Advertise `prefix` and serve requests beneath it, delivering each as a
	/// broadcast-request handle via `on_request`.
	pub fn dynamic(
		&mut self,
		origin: Id,
		prefix: &str,
		route: moq_net::origin::Route,
		on_request: OnStatus,
	) -> Result<Id, Error> {
		let origin = self.active.get(origin).ok_or(Error::OriginNotFound)?;
		origin.driver.check()?;
		let driver = origin.driver.clone();
		let result = origin.producer.dynamic(prefix, route);
		origin.driver.check()?;
		let inner = result?;
		let channel = oneshot::channel();
		let id = self.dynamic.insert(Some(DynamicEntry {
			inner: Some(inner),
			close: Some(channel.0),
			callback: on_request,
		}))?;

		tokio::spawn(async move {
			let res = crate::ffi::run(driver.watch(Self::run_dynamic(id, channel.1))).await;
			let entry = State::lock().origin.dynamic.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_dynamic(id: Id, mut close: oneshot::Receiver<()>) -> Result<(), Error> {
		loop {
			let request = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				res = kio::wait(|waiter| {
					let state = State::lock();
					match state.origin.dynamic.get(id).and_then(|entry| entry.as_ref()).and_then(|entry| entry.inner.as_ref()) {
						Some(dynamic) => dynamic.poll_requested_broadcast(waiter),
						None => std::task::Poll::Ready(Err(moq_net::Error::Closed)),
					}
				}) => match res {
					Ok(request) => request,
					Err(moq_net::Error::Closed) => return Ok(()),
					Err(err) => return Err(err.into()),
				},
			};

			let request_id = State::lock().origin.broadcast_request.insert(Some(request))?;
			let callback = State::lock()
				.origin
				.dynamic
				.get(id)
				.and_then(|entry| entry.as_ref())
				.map(|entry| entry.callback);
			let Some(callback) = callback else {
				return Ok(());
			};
			callback.call(request_id);
		}
	}

	pub fn dynamic_update(&self, dynamic: Id, route: moq_net::origin::Route) -> Result<(), Error> {
		let dynamic = self
			.dynamic
			.get(dynamic)
			.and_then(|entry| entry.as_ref())
			.and_then(|entry| entry.inner.as_ref())
			.ok_or(Error::NotFound)?;
		Ok(dynamic.update(route)?)
	}

	pub fn dynamic_close(&mut self, dynamic: Id) -> Result<(), Error> {
		let entry = self
			.dynamic
			.get_mut(dynamic)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::NotFound)?;
		let inner = entry.inner.take().ok_or(Error::NotFound)?;
		entry.close.take();
		drop(inner);
		Ok(())
	}

	pub fn broadcast_request_path(&self, request: Id, dst: &mut crate::moq_string) -> Result<(), Error> {
		let request = self
			.broadcast_request
			.get(request)
			.and_then(|slot| slot.as_ref())
			.ok_or(Error::NotFound)?;
		let path = request.path();
		*dst = crate::moq_string {
			data: path.as_str().as_ptr().cast::<std::ffi::c_char>(),
			len: path.as_str().len(),
		};
		Ok(())
	}

	pub fn broadcast_request_take(&mut self, request: Id) -> Result<moq_net::origin::Request, Error> {
		self.broadcast_request.remove(request).flatten().ok_or(Error::NotFound)
	}

	pub fn close(&mut self, origin: Id) -> Result<(), Error> {
		self.active.remove(origin).ok_or(Error::OriginNotFound)?;
		Ok(())
	}
}

/// Apply every change on hand, returning whether any came through, or `None` once the origin
/// closed.
fn settle(follow: &mut moq_net::announce::Follow, unseen: &mut bool, last: &mut Option<Id>) -> Option<bool> {
	let mut changed = false;
	while let std::task::Poll::Ready(event) = follow.poll_next(&kio::Waiter::noop()) {
		changed |= apply(unseen, last, event?);
	}
	Some(changed)
}

/// Apply one change to the path, returning whether it is one.
fn apply(unseen: &mut bool, last: &mut Option<Id>, event: moq_net::announce::Event) -> bool {
	match event {
		moq_net::announce::Event::Start(_) | moq_net::announce::Event::Restart(_) => *unseen = true,
		// An end is a gap that ended every request on the old route, so whatever serves the path
		// next is delivered, even the same instance coming back.
		moq_net::announce::Event::End(_) => {
			*unseen = false;
			*last = None;
		}
		// The same instance over another route, which its subscriptions ride out.
		moq_net::announce::Event::Update(_) => return false,
	}
	true
}

#[cfg(test)]
mod supervision_tests {
	use super::*;

	#[tokio::test]
	async fn a_broadcast_keeps_the_origin_failure_after_its_origin_handle_is_released() {
		let (producer, completion, trigger) = crate::ffi::panicking_origin();
		let mut origins = Origin::default();
		let origin = origins
			.active
			.insert(Producer {
				producer,
				driver: completion.clone(),
			})
			.unwrap();
		let mut publish = crate::publish::Publish::default();
		let broadcast = publish
			.create(origins.create_broadcast(origin, "retained").unwrap())
			.unwrap();
		origins.close(origin).unwrap();
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(Error::Panic)));
		drop(completion);
		assert!(matches!(
			publish.announce(broadcast, Default::default()),
			Err(Error::Panic)
		));
		let healthy = origins.create().unwrap();
		let broadcast = publish
			.create(origins.create_broadcast(healthy, "healthy").unwrap())
			.unwrap();
		publish.announce(broadcast, Default::default()).unwrap();
	}
}
